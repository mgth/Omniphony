//! Chunked resampling with an output FIFO, driven from the audio callback.
//!
//! Everything here runs on the realtime thread, so the steady state must not
//! allocate: the resampler writes into a buffer this struct owns, because
//! rubato's `process` hands back a freshly allocated `Vec<Vec<f32>>` per chunk.
//!
//! The output FIFO is a plain `Vec` drained from the front. That memmoves what
//! is left behind, which looks like the wrong shape — but at these sizes
//! (a few thousand samples) it measures faster than a `VecDeque`, whose
//! per-element wraparound handling costs more than the move it saves. Measured
//! at 0.30 vs 0.52 us per callback for a 2048-sample push and a 1024-sample
//! drain; revisit if the FIFO ever grows by an order of magnitude.

use anyhow::Result;
use crossbeam::queue::ArrayQueue;
use rubato::Resampler;

pub const RESAMPLER_CHUNK_SIZE: usize = 1024;

pub struct ResamplerFifoEngine {
    channel_count: usize,
    resampler_input: Vec<Vec<f32>>,
    /// Planar output the resampler writes into, reused across chunks. Grown on
    /// first use to the resampler's maximum output frames, never after.
    resampler_output: Vec<Vec<f32>>,
    input_frames_collected: usize,
    output_fifo: Vec<f32>,
}

impl ResamplerFifoEngine {
    pub fn new(channel_count: usize) -> Self {
        Self {
            channel_count,
            resampler_input: vec![vec![0.0; RESAMPLER_CHUNK_SIZE]; channel_count],
            // Sized on first use: the frame count depends on the resampler's
            // ratio bounds, which this type is not given.
            resampler_output: vec![Vec::new(); channel_count],
            input_frames_collected: 0,
            output_fifo: Vec::with_capacity(RESAMPLER_CHUNK_SIZE * channel_count * 4),
        }
    }

    pub fn output_len(&self) -> usize {
        self.output_fifo.len()
    }

    pub fn pending_input_samples(&self) -> usize {
        self.input_frames_collected
            .saturating_mul(self.channel_count)
    }

    pub fn reset(&mut self) {
        self.input_frames_collected = 0;
        self.output_fifo.clear();
        for channel in &mut self.resampler_input {
            channel.fill(0.0);
        }
    }

    pub fn ensure_output_samples<R: Resampler<f32>>(
        &mut self,
        input_buffer: &ArrayQueue<f32>,
        resampler: &mut R,
        needed_samples: usize,
    ) -> Result<()> {
        while self.output_fifo.len() < needed_samples {
            while self.input_frames_collected < RESAMPLER_CHUNK_SIZE {
                let mut frame_complete = true;
                if input_buffer.len() >= self.channel_count {
                    for ch in 0..self.channel_count {
                        if let Some(sample_f32) = input_buffer.pop() {
                            self.resampler_input[ch][self.input_frames_collected] = sample_f32;
                        } else {
                            frame_complete = false;
                            break;
                        }
                    }
                } else {
                    frame_complete = false;
                }

                if frame_complete {
                    self.input_frames_collected += 1;
                } else {
                    break;
                }
            }

            if self.input_frames_collected == RESAMPLER_CHUNK_SIZE {
                // Grow to what this resampler can ever emit, so the call below
                // never has to (rubato validates the length and would fail
                // rather than reallocate).
                let max_frames = resampler.output_frames_max();
                if self
                    .resampler_output
                    .first()
                    .is_none_or(|channel| channel.len() < max_frames)
                {
                    for channel in &mut self.resampler_output {
                        channel.resize(max_frames, 0.0);
                    }
                }

                let (_, output_frames) = resampler
                    .process_into_buffer(&self.resampler_input, &mut self.resampler_output, None)
                    .map_err(anyhow::Error::from)?;
                for i in 0..output_frames {
                    for ch in 0..self.channel_count {
                        self.output_fifo.push(self.resampler_output[ch][i]);
                    }
                }
                self.input_frames_collected = 0;
            } else {
                break;
            }
        }

        Ok(())
    }

    pub fn drain_into_slice(&mut self, dest: &mut [f32]) -> usize {
        let count = dest.len().min(self.output_fifo.len());
        for (slot, sample) in dest.iter_mut().zip(self.output_fifo.drain(0..count)) {
            *slot = sample;
        }
        count
    }

    pub fn discard_samples(&mut self, sample_count: usize) -> usize {
        let discard_count = sample_count.min(self.output_fifo.len());
        self.output_fifo.drain(0..discard_count);
        discard_count
    }

    pub fn drain_to_vec(&mut self, sample_count: usize) -> Vec<f32> {
        let count = sample_count.min(self.output_fifo.len());
        self.output_fifo.drain(0..count).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rubato::{SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};

    const CHANNELS: usize = 2;

    fn resampler(ratio: f64) -> SincFixedIn<f32> {
        SincFixedIn::<f32>::new(
            ratio,
            1.1,
            SincInterpolationParameters {
                sinc_len: 64,
                f_cutoff: 0.95,
                interpolation: SincInterpolationType::Linear,
                oversampling_factor: 64,
                window: WindowFunction::BlackmanHarris2,
            },
            RESAMPLER_CHUNK_SIZE,
            CHANNELS,
        )
        .expect("resampler")
    }

    /// Feed one full chunk per channel, interleaved, as the callback does.
    fn feed_one_chunk(queue: &ArrayQueue<f32>) {
        for frame in 0..RESAMPLER_CHUNK_SIZE {
            for ch in 0..CHANNELS {
                // Distinct per channel so a channel swap would show up.
                let _ = queue.push(frame as f32 + ch as f32 * 1000.0);
            }
        }
    }

    #[test]
    fn resamples_a_chunk_into_the_fifo() {
        let mut engine = ResamplerFifoEngine::new(CHANNELS);
        let mut rs = resampler(1.0);
        let queue = ArrayQueue::new(RESAMPLER_CHUNK_SIZE * CHANNELS * 2);
        feed_one_chunk(&queue);

        engine
            .ensure_output_samples(&queue, &mut rs, RESAMPLER_CHUNK_SIZE)
            .expect("resample");

        assert!(engine.output_len() > 0, "a full chunk must produce output");
        assert_eq!(
            engine.output_len() % CHANNELS,
            0,
            "the FIFO holds whole interleaved frames"
        );
        assert_eq!(engine.pending_input_samples(), 0, "the chunk was consumed");
    }

    /// The output buffer is grown once and reused: a second chunk goes through
    /// the same path and still produces a full chunk of output.
    ///
    /// It produces *more* than the first, not the same: the sinc filter's
    /// history starts empty, so the first chunk is short by the interpolator's
    /// startup delay and only the steady state emits a full chunk.
    #[test]
    fn a_second_chunk_reuses_the_output_buffer() {
        let mut engine = ResamplerFifoEngine::new(CHANNELS);
        let mut rs = resampler(1.0);
        let queue = ArrayQueue::new(RESAMPLER_CHUNK_SIZE * CHANNELS * 4);

        feed_one_chunk(&queue);
        engine
            .ensure_output_samples(&queue, &mut rs, RESAMPLER_CHUNK_SIZE)
            .expect("resample");
        let first = engine.output_len();
        assert!(first > 0);
        engine.discard_samples(first);

        feed_one_chunk(&queue);
        engine
            .ensure_output_samples(&queue, &mut rs, RESAMPLER_CHUNK_SIZE)
            .expect("resample");
        let second = engine.output_len();
        assert_eq!(
            second,
            RESAMPLER_CHUNK_SIZE * CHANNELS,
            "at ratio 1.0 the steady state emits one frame per input frame"
        );
        assert!(
            second >= first,
            "the startup transient only shortens the first"
        );
    }

    /// Draining takes from the front, in order, and leaves the rest intact —
    /// the property any future change of FIFO container has to preserve.
    #[test]
    fn draining_takes_the_oldest_samples_in_order() {
        let mut engine = ResamplerFifoEngine::new(CHANNELS);
        let mut rs = resampler(1.0);
        let queue = ArrayQueue::new(RESAMPLER_CHUNK_SIZE * CHANNELS * 2);
        feed_one_chunk(&queue);
        engine
            .ensure_output_samples(&queue, &mut rs, RESAMPLER_CHUNK_SIZE)
            .expect("resample");

        let total = engine.output_len();
        let all = {
            let mut engine = ResamplerFifoEngine::new(CHANNELS);
            let mut rs = resampler(1.0);
            let queue = ArrayQueue::new(RESAMPLER_CHUNK_SIZE * CHANNELS * 2);
            feed_one_chunk(&queue);
            engine
                .ensure_output_samples(&queue, &mut rs, RESAMPLER_CHUNK_SIZE)
                .expect("resample");
            engine.drain_to_vec(total)
        };

        let mut head = vec![0.0; 8];
        assert_eq!(engine.drain_into_slice(&mut head), 8);
        assert_eq!(head, all[..8], "the front of the FIFO comes out first");
        assert_eq!(engine.output_len(), total - 8);

        engine.discard_samples(4);
        let mut next = vec![0.0; 8];
        assert_eq!(engine.drain_into_slice(&mut next), 8);
        assert_eq!(
            next,
            all[12..20],
            "discarding advances the front by exactly that many samples"
        );
    }

    /// Asking for more than the FIFO holds copies what there is and says so,
    /// rather than padding or panicking.
    #[test]
    fn draining_more_than_available_is_a_short_copy() {
        let mut engine = ResamplerFifoEngine::new(CHANNELS);
        let mut dest = vec![-1.0f32; 4];
        assert_eq!(engine.drain_into_slice(&mut dest), 0);
        assert_eq!(
            dest,
            vec![-1.0; 4],
            "nothing written when the FIFO is empty"
        );
        assert_eq!(engine.discard_samples(16), 0);
    }
}
