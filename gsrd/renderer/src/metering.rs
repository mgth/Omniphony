use std::time::{Duration, Instant};

const DBFS_FLOOR: f32 = -100.0;

fn linear_to_dbfs(v: f32) -> f32 {
    if v <= 0.0 {
        DBFS_FLOOR
    } else {
        (20.0 * v.log10()).max(DBFS_FLOOR)
    }
}

pub struct MeterSnapshot {
    /// (channel_idx, peak_dbfs, rms_dbfs) — one per input channel, same index as /gsrd/object/{idx}/xyz
    pub object_levels: Vec<(u32, f32, f32)>,
    /// (peak_dbfs, rms_dbfs) — one per output speaker
    pub speaker_levels: Vec<(f32, f32)>,
}

pub struct AudioMeter {
    num_channels: usize,
    obj_peak: Vec<f32>,
    obj_rms_sq: Vec<f64>,
    obj_count: u64,
    spk_peak: Vec<f32>,
    spk_rms_sq: Vec<f64>,
    spk_count: u64,
    num_speakers: usize,
    last_send: Instant,
    send_interval: Duration,
}

impl AudioMeter {
    pub fn new(num_speakers: usize, rate_hz: f32) -> Self {
        Self {
            num_channels: 0,
            obj_peak: Vec::new(),
            obj_rms_sq: Vec::new(),
            obj_count: 0,
            spk_peak: vec![0.0f32; num_speakers],
            spk_rms_sq: vec![0.0f64; num_speakers],
            spk_count: 0,
            num_speakers,
            last_send: Instant::now(),
            send_interval: Duration::from_secs_f32(1.0 / rate_hz),
        }
    }

    /// Resize accumulators to match the actual number of input channels.
    /// The ID used in OSC messages is the channel index (same as /gsrd/object/{idx}/xyz).
    pub fn update_channel_count(&mut self, total_input_channels: usize) {
        if self.num_channels == total_input_channels {
            return;
        }
        self.num_channels = total_input_channels;
        self.obj_peak.resize(total_input_channels, 0.0);
        self.obj_rms_sq.resize(total_input_channels, 0.0);
    }

    /// Call once per sample (one frame = one call per sample in the pcm_data_f32 vec).
    pub fn process_objects(&mut self, frame: &[f32], n_channels: usize) {
        let n = n_channels.min(self.obj_peak.len());
        for ch in 0..n {
            let s = frame[ch].abs();
            if s > self.obj_peak[ch] {
                self.obj_peak[ch] = s;
            }
            self.obj_rms_sq[ch] += (s as f64) * (s as f64);
        }
        self.obj_count += 1;
    }

    /// Call with the interleaved output buffer from render_frame().
    pub fn process_speakers(&mut self, interleaved: &[f32], n_speakers: usize) {
        let n = n_speakers.min(self.num_speakers);
        if n == 0 || n_speakers == 0 {
            return;
        }
        let frame_count = interleaved.len() / n_speakers;
        for f in 0..frame_count {
            for spk in 0..n {
                let s = interleaved[f * n_speakers + spk].abs();
                if s > self.spk_peak[spk] {
                    self.spk_peak[spk] = s;
                }
                self.spk_rms_sq[spk] += (s as f64) * (s as f64);
            }
        }
        self.spk_count += frame_count as u64;
    }

    /// Returns Some(snapshot) when the send interval has elapsed, resetting accumulators.
    pub fn poll(&mut self) -> Option<MeterSnapshot> {
        if self.last_send.elapsed() < self.send_interval {
            return None;
        }

        let obj_count = self.obj_count.max(1);
        let spk_count = self.spk_count.max(1);

        let object_levels = (0..self.num_channels)
            .map(|i| {
                let peak = linear_to_dbfs(self.obj_peak[i]);
                let rms = linear_to_dbfs((self.obj_rms_sq[i] / obj_count as f64).sqrt() as f32);
                (i as u32, peak, rms)
            })
            .collect();

        let speaker_levels = (0..self.num_speakers)
            .map(|i| {
                let peak = linear_to_dbfs(self.spk_peak[i]);
                let rms = linear_to_dbfs((self.spk_rms_sq[i] / spk_count as f64).sqrt() as f32);
                (peak, rms)
            })
            .collect();

        // Reset accumulators
        for v in &mut self.obj_peak {
            *v = 0.0;
        }
        for v in &mut self.obj_rms_sq {
            *v = 0.0;
        }
        self.obj_count = 0;
        for v in &mut self.spk_peak {
            *v = 0.0;
        }
        for v in &mut self.spk_rms_sq {
            *v = 0.0;
        }
        self.spk_count = 0;
        self.last_send = Instant::now();

        Some(MeterSnapshot {
            object_levels,
            speaker_levels,
        })
    }
}
