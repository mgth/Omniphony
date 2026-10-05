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

use crate::ring_buffer_io::RingReader;
use rubato::Resampler;

pub const RESAMPLER_CHUNK_SIZE: usize = 1024;

/// Sinc design of the local output resampler, the same for every backend
/// (PipeWire, cpal): 256 taps, 256× oversampled with linear interpolation
/// between the oversampled points, Blackman-Harris² window, cutoff at 95 %
/// of the lower Nyquist.
pub fn output_resampler_params() -> rubato::SincInterpolationParameters {
    rubato::SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        interpolation: rubato::SincInterpolationType::Linear,
        oversampling_factor: 256,
        window: rubato::WindowFunction::BlackmanHarris2,
    }
}

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

    /// Resample from `input_buffer` until the FIFO holds `needed_samples`, or
    /// the input runs out. The error is rubato's own, which is plain data:
    /// wrapping it (an `anyhow::Error` is a heap allocation, and a backtrace
    /// when those are enabled) would cost the realtime thread exactly when the
    /// resampler is already failing.
    pub fn ensure_output_samples<R: Resampler<f32>>(
        &mut self,
        input_buffer: &mut RingReader,
        resampler: &mut R,
        needed_samples: usize,
    ) -> Result<(), rubato::ResampleError> {
        while self.output_fifo.len() < needed_samples {
            // The whole frames the ring holds, up to what the chunk still
            // lacks, deinterleaved in one pass over one block of the ring (two
            // where it wraps around its end, and a frame may straddle them).
            let channels = self.channel_count;
            let frames = (RESAMPLER_CHUNK_SIZE - self.input_frames_collected)
                .min(input_buffer.available().checked_div(channels).unwrap_or(0));
            if frames > 0 {
                let planar = &mut self.resampler_input;
                let mut frame = self.input_frames_collected;
                let mut channel = 0;
                input_buffer.pop_with(frames * channels, |block| {
                    for &sample in block {
                        planar[channel][frame] = sample;
                        channel += 1;
                        if channel == channels {
                            channel = 0;
                            frame += 1;
                        }
                    }
                });
                self.input_frames_collected += frames;
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

                let (_, output_frames) = resampler.process_into_buffer(
                    &self.resampler_input,
                    &mut self.resampler_output,
                    None,
                )?;
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

    /// Move whole frames of `channels` interleaved samples into `dest`, whose
    /// frames are `dest_channels` wide (`>= channels`; the extra device
    /// channels are zeroed). Moves as many frames as both hold and returns
    /// that count. Allocation-free, for the realtime callback.
    pub fn drain_frames_into(
        &mut self,
        dest: &mut [f32],
        channels: usize,
        dest_channels: usize,
    ) -> usize {
        debug_assert!(channels > 0 && dest_channels >= channels);
        let frames = (dest.len() / dest_channels).min(self.output_fifo.len() / channels);
        for (dst, src) in dest
            .chunks_exact_mut(dest_channels)
            .zip(self.output_fifo.chunks_exact(channels))
            .take(frames)
        {
            dst[..channels].copy_from_slice(src);
            dst[channels..].fill(0.0);
        }
        self.output_fifo.drain(0..frames * channels);
        frames
    }

    pub fn drain_to_vec(&mut self, sample_count: usize) -> Vec<f32> {
        let count = sample_count.min(self.output_fifo.len());
        self.output_fifo.drain(0..count).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ring_buffer_io::{RingWriter, sample_ring};
    use rubato::{SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};

    const CHANNELS: usize = 2;

    /// Whole frames land on the device's wider layout, extra channels zeroed,
    /// and a partial frame is never split.
    #[test]
    fn drain_frames_into_maps_onto_a_wider_device_layout() {
        let mut engine = ResamplerFifoEngine::new(2);
        engine
            .output_fifo
            .extend_from_slice(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        let mut dest = [9.0f32; 9];
        let frames = engine.drain_frames_into(&mut dest, 2, 3);
        assert_eq!(frames, 2);
        assert_eq!(dest[..6], [1.0, 2.0, 0.0, 3.0, 4.0, 0.0]);
        assert_eq!(
            dest[6..],
            [9.0, 9.0, 9.0],
            "untouched past the moved frames"
        );
        assert_eq!(engine.output_len(), 1, "the half frame stays queued");
    }

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

    /// Feed one full chunk per channel, interleaved, as the renderer does.
    fn feed_one_chunk(ring: &mut RingWriter) {
        let mut chunk = Vec::with_capacity(RESAMPLER_CHUNK_SIZE * CHANNELS);
        for frame in 0..RESAMPLER_CHUNK_SIZE {
            for ch in 0..CHANNELS {
                // Distinct per channel so a channel swap would show up.
                chunk.push(frame as f32 + ch as f32 * 1000.0);
            }
        }
        assert_eq!(ring.push_slice(&chunk), chunk.len(), "ring has room");
    }

    #[test]
    fn resamples_a_chunk_into_the_fifo() {
        let mut engine = ResamplerFifoEngine::new(CHANNELS);
        let mut rs = resampler(1.0);
        let (mut writer, mut reader) = sample_ring(RESAMPLER_CHUNK_SIZE * CHANNELS * 2);
        feed_one_chunk(&mut writer);

        engine
            .ensure_output_samples(&mut reader, &mut rs, RESAMPLER_CHUNK_SIZE)
            .expect("resample");

        assert!(engine.output_len() > 0, "a full chunk must produce output");
        assert_eq!(
            engine.output_len() % CHANNELS,
            0,
            "the FIFO holds whole interleaved frames"
        );
        assert_eq!(engine.pending_input_samples(), 0, "the chunk was consumed");
    }

    /// The ring hands its samples over in blocks, two where they wrap around
    /// its end, and nothing aligns that end with a frame: the frame cut in
    /// two there still lands whole, each sample on its own channel.
    #[test]
    fn a_frame_cut_by_the_end_of_the_ring_is_deinterleaved_whole() {
        let mut engine = ResamplerFifoEngine::new(CHANNELS);
        let mut rs = resampler(1.0);
        // An odd capacity, and a read position three samples short of the
        // end: one frame, then the first half of the next.
        let capacity = RESAMPLER_CHUNK_SIZE * CHANNELS * 2 + 1;
        let (mut writer, mut reader) = sample_ring(capacity);
        writer.push_silence(capacity - 3);
        reader.discard(capacity - 3);
        feed_one_chunk(&mut writer);

        engine
            .ensure_output_samples(&mut reader, &mut rs, RESAMPLER_CHUNK_SIZE)
            .expect("resample");

        assert_eq!(engine.pending_input_samples(), 0, "the chunk was consumed");
        for frame in 0..RESAMPLER_CHUNK_SIZE {
            for ch in 0..CHANNELS {
                assert_eq!(
                    engine.resampler_input[ch][frame],
                    frame as f32 + ch as f32 * 1000.0,
                    "frame {frame}, channel {ch}"
                );
            }
        }
    }

    /// Less than a chunk in the ring: the whole frames are collected and kept
    /// for the next call, a trailing half frame stays in the ring, and
    /// nothing is resampled yet.
    #[test]
    fn a_partial_chunk_waits_for_the_rest() {
        let mut engine = ResamplerFifoEngine::new(CHANNELS);
        let mut rs = resampler(1.0);
        let (mut writer, mut reader) = sample_ring(RESAMPLER_CHUNK_SIZE * CHANNELS * 2);
        writer.push_slice(&[1.0, 2.0, 3.0, 4.0, 5.0]);

        engine
            .ensure_output_samples(&mut reader, &mut rs, RESAMPLER_CHUNK_SIZE)
            .expect("resample");
        assert_eq!(engine.output_len(), 0);
        assert_eq!(engine.pending_input_samples(), 4, "two whole frames");
        assert_eq!(reader.available(), 1, "the half frame stays in the ring");
        assert_eq!(engine.resampler_input[0][..2], [1.0, 3.0]);
        assert_eq!(engine.resampler_input[1][..2], [2.0, 4.0]);

        // The rest of the chunk arrives: it is completed where it stopped.
        let rest = vec![9.0; (RESAMPLER_CHUNK_SIZE - 2) * CHANNELS - 1];
        writer.push_slice(&rest);
        engine
            .ensure_output_samples(&mut reader, &mut rs, RESAMPLER_CHUNK_SIZE)
            .expect("resample");
        assert!(engine.output_len() > 0, "the completed chunk was resampled");
        assert_eq!(engine.pending_input_samples(), 0);
        assert_eq!(engine.resampler_input[0][..3], [1.0, 3.0, 5.0]);
        assert_eq!(reader.available(), 0);
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
        let (mut writer, mut reader) = sample_ring(RESAMPLER_CHUNK_SIZE * CHANNELS * 4);

        feed_one_chunk(&mut writer);
        engine
            .ensure_output_samples(&mut reader, &mut rs, RESAMPLER_CHUNK_SIZE)
            .expect("resample");
        let first = engine.output_len();
        assert!(first > 0);
        engine.discard_samples(first);

        feed_one_chunk(&mut writer);
        engine
            .ensure_output_samples(&mut reader, &mut rs, RESAMPLER_CHUNK_SIZE)
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
        let (mut writer, mut reader) = sample_ring(RESAMPLER_CHUNK_SIZE * CHANNELS * 2);
        feed_one_chunk(&mut writer);
        engine
            .ensure_output_samples(&mut reader, &mut rs, RESAMPLER_CHUNK_SIZE)
            .expect("resample");

        let total = engine.output_len();
        let all = {
            let mut engine = ResamplerFifoEngine::new(CHANNELS);
            let mut rs = resampler(1.0);
            let (mut writer, mut reader) = sample_ring(RESAMPLER_CHUNK_SIZE * CHANNELS * 2);
            feed_one_chunk(&mut writer);
            engine
                .ensure_output_samples(&mut reader, &mut rs, RESAMPLER_CHUNK_SIZE)
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
