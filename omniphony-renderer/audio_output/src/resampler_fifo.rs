use anyhow::Result;
use crossbeam::queue::ArrayQueue;
use rubato::Resampler;

pub const RESAMPLER_CHUNK_SIZE: usize = 1024;

pub struct ResamplerFifoEngine {
    channel_count: usize,
    resampler_input: Vec<Vec<f32>>,
    input_frames_collected: usize,
    output_fifo: Vec<f32>,
}

impl ResamplerFifoEngine {
    pub fn new(channel_count: usize) -> Self {
        Self {
            channel_count,
            resampler_input: vec![vec![0.0; RESAMPLER_CHUNK_SIZE]; channel_count],
            input_frames_collected: 0,
            output_fifo: Vec::with_capacity(RESAMPLER_CHUNK_SIZE * channel_count * 4),
        }
    }

    pub fn output_len(&self) -> usize {
        self.output_fifo.len()
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
                let output_planar = resampler
                    .process(&self.resampler_input, None)
                    .map_err(anyhow::Error::from)?;
                let output_frames = output_planar[0].len();
                for i in 0..output_frames {
                    for ch in 0..self.channel_count {
                        self.output_fifo.push(output_planar[ch][i]);
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
        for (i, sample) in self.output_fifo.drain(0..count).enumerate() {
            dest[i] = sample;
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
