use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
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
    /// (channel_idx, peak_dbfs, rms_dbfs) — one per input channel, same index as /omniphony/object/{idx}/xyz
    pub object_levels: Vec<(u32, f32, f32)>,
    /// (channel_idx, [band0_rms_dbfs, ...]) — per-crossover-band RMS for the
    /// channels the render reported band energy for. Band order matches the
    /// crossover bands (and the band gain table). Post object-gain, unlike the
    /// pre-gain full-band `object_levels`. Empty when no crossover is active.
    pub object_band_levels: Vec<(u32, Vec<f32>)>,
    /// (peak_dbfs, rms_dbfs) — one per output speaker
    pub speaker_levels: Vec<(f32, f32)>,
    /// Master output level (peak_dbfs, rms_dbfs), aggregated from the
    /// post-master-gain speaker accumulators: peak = max over speakers, rms =
    /// combined RMS across all speakers over the send interval.
    pub master_peak: f32,
    pub master_rms: f32,
}

pub struct AudioMeter {
    num_channels: usize,
    obj_peak: Vec<f32>,
    obj_rms_sq: Vec<f64>,
    /// Per-channel per-band Σs² fed by the render's band-split path
    /// ([`Self::process_object_bands`]). Inner vec sized on first report (and
    /// re-sized on a band-count change, which resets that channel's sums).
    obj_band_sq: Vec<Vec<f64>>,
    obj_count: u64,
    spk_peak: Vec<f32>,
    spk_rms_sq: Vec<f64>,
    spk_count: u64,
    num_speakers: usize,
    last_send: Instant,
    send_interval: Duration,
    /// If present, the send_interval is recomputed from this atomic each
    /// `poll()`, letting OSC clients adjust the metering cadence live.
    rate_hz_bits: Option<Arc<AtomicU32>>,
    last_rate_seen: f32,
}

impl AudioMeter {
    pub fn new(num_speakers: usize, rate_hz: f32) -> Self {
        Self {
            num_channels: 0,
            obj_peak: Vec::new(),
            obj_rms_sq: Vec::new(),
            obj_band_sq: Vec::new(),
            obj_count: 0,
            spk_peak: vec![0.0f32; num_speakers],
            spk_rms_sq: vec![0.0f64; num_speakers],
            spk_count: 0,
            num_speakers,
            last_send: Instant::now(),
            send_interval: Duration::from_secs_f32(1.0 / rate_hz.max(1.0)),
            rate_hz_bits: None,
            last_rate_seen: rate_hz,
        }
    }

    /// Like `new`, but the rate is read from a shared atomic on every
    /// `poll()` — letting OSC clients update the metering cadence at runtime
    /// (e.g. via `/omniphony/control/metering/rate_hz`).
    pub fn new_with_rate_atomic(num_speakers: usize, rate_hz_bits: Arc<AtomicU32>) -> Self {
        let initial = f32::from_bits(rate_hz_bits.load(Ordering::Relaxed)).max(1.0);
        Self {
            num_channels: 0,
            obj_peak: Vec::new(),
            obj_rms_sq: Vec::new(),
            obj_band_sq: Vec::new(),
            obj_count: 0,
            spk_peak: vec![0.0f32; num_speakers],
            spk_rms_sq: vec![0.0f64; num_speakers],
            spk_count: 0,
            num_speakers,
            last_send: Instant::now(),
            send_interval: Duration::from_secs_f32(1.0 / initial),
            rate_hz_bits: Some(rate_hz_bits),
            last_rate_seen: initial,
        }
    }

    /// Resize accumulators to match the actual number of input channels.
    /// The ID used in OSC messages is the channel index (same as /omniphony/object/{idx}/xyz).
    pub fn update_channel_count(&mut self, total_input_channels: usize) {
        if self.num_channels == total_input_channels {
            return;
        }
        self.num_channels = total_input_channels;
        self.obj_peak.resize(total_input_channels, 0.0);
        self.obj_rms_sq.resize(total_input_channels, 0.0);
        self.obj_band_sq.resize(total_input_channels, Vec::new());
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

    /// Accumulate the render's per-band Σs² report for this frame
    /// (`RenderedFrame::object_band_sq`). A channel whose band count changed
    /// (topology rebuild) restarts its sums — mixing intervals across two band
    /// layouts would be meaningless.
    pub fn process_object_bands(&mut self, per_object: &[(usize, Vec<f64>)]) {
        for (ch, sums) in per_object {
            let Some(acc) = self.obj_band_sq.get_mut(*ch) else {
                continue;
            };
            if acc.len() != sums.len() {
                acc.clear();
                acc.resize(sums.len(), 0.0);
            }
            for (a, &s) in acc.iter_mut().zip(sums) {
                *a += s;
            }
        }
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
        if let Some(atomic) = &self.rate_hz_bits {
            let hz = f32::from_bits(atomic.load(Ordering::Relaxed)).max(1.0);
            if (hz - self.last_rate_seen).abs() > 1e-3 {
                self.last_rate_seen = hz;
                self.send_interval = Duration::from_secs_f32(1.0 / hz);
            }
        }
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

        // Same interval mean as the full-band RMS: band sums accumulate zeros
        // implicitly for frames where the object was absent, exactly like the
        // input accumulator does for silent channels.
        let object_band_levels = self
            .obj_band_sq
            .iter()
            .enumerate()
            .filter(|(_, sums)| !sums.is_empty())
            .map(|(i, sums)| {
                let bands = sums
                    .iter()
                    .map(|&sq| linear_to_dbfs((sq / obj_count as f64).sqrt() as f32))
                    .collect();
                (i as u32, bands)
            })
            .collect();

        let speaker_levels = (0..self.num_speakers)
            .map(|i| {
                let peak = linear_to_dbfs(self.spk_peak[i]);
                let rms = linear_to_dbfs((self.spk_rms_sq[i] / spk_count as f64).sqrt() as f32);
                (peak, rms)
            })
            .collect();

        // Master = aggregate of the post-master-gain speaker accumulators.
        // Peak is the loudest speaker sample; RMS is the combined energy across
        // all speakers over the interval. Free: no extra per-sample work.
        let master_peak = linear_to_dbfs(
            self.spk_peak[..self.num_speakers]
                .iter()
                .copied()
                .fold(0.0f32, f32::max),
        );
        let master_rms = if self.num_speakers == 0 {
            DBFS_FLOOR
        } else {
            let energy: f64 = self.spk_rms_sq[..self.num_speakers].iter().sum();
            let total = (self.num_speakers as f64) * (spk_count as f64);
            linear_to_dbfs((energy / total).sqrt() as f32)
        };

        // Reset accumulators
        for v in &mut self.obj_peak {
            *v = 0.0;
        }
        for v in &mut self.obj_rms_sq {
            *v = 0.0;
        }
        // Cleared, not zeroed: a channel that stops reporting bands (crossover
        // switched off) must stop being listed, not report phantom silent
        // bands. Capacity is retained, so the next interval reallocates nothing.
        for bands in &mut self.obj_band_sq {
            bands.clear();
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
            object_band_levels,
            speaker_levels,
            master_peak,
            master_rms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_peak_is_loudest_speaker_and_survives_over_0_dbfs() {
        // 1 kHz send rate → ~1 ms interval, so a short sleep lets poll() fire.
        let mut m = AudioMeter::new(2, 1000.0);
        // 2 speakers, 2 frames. spk0 hits 1.3 linear (> 1.0 → over 0 dBFS).
        let interleaved = [1.3f32, 0.5, 0.2, 0.5];
        m.process_speakers(&interleaved, 2);

        std::thread::sleep(Duration::from_millis(3));
        let snap = m.poll().expect("send interval should have elapsed");

        // Master peak == loudest speaker peak, and the over-0 dBFS value is kept.
        let expected = 20.0 * 1.3f32.log10();
        assert!(
            (snap.master_peak - expected).abs() < 1e-3,
            "master_peak = {} (expected ≈ {})",
            snap.master_peak,
            expected
        );
        assert!(snap.master_peak > 0.0, "over-0 dBFS peak flattened");

        let spk_max = snap
            .speaker_levels
            .iter()
            .map(|&(p, _)| p)
            .fold(f32::MIN, f32::max);
        assert!((snap.master_peak - spk_max).abs() < 1e-6);
    }

    #[test]
    fn band_rms_uses_the_same_interval_mean_as_the_full_band_meter() {
        let mut m = AudioMeter::new(1, 1000.0);
        m.update_channel_count(1);
        // 4 samples at 0.5, all of the energy landing in band 0: the band RMS
        // must come out identical to the full-band RMS, and the silent band at
        // the floor.
        for _ in 0..4 {
            m.process_objects(&[0.5], 1);
        }
        m.process_object_bands(&[(0, vec![4.0 * 0.25, 0.0])]);

        std::thread::sleep(Duration::from_millis(3));
        let snap = m.poll().expect("send interval should have elapsed");

        assert_eq!(snap.object_band_levels.len(), 1);
        let (id, bands) = &snap.object_band_levels[0];
        assert_eq!(*id, 0);
        assert_eq!(bands.len(), 2);
        let full_band_rms = snap.object_levels[0].2;
        assert!(
            (bands[0] - full_band_rms).abs() < 1e-4,
            "band0 {} != full-band {}",
            bands[0],
            full_band_rms
        );
        assert_eq!(bands[1], DBFS_FLOOR);
    }

    #[test]
    fn a_channel_that_stops_reporting_bands_is_dropped_from_the_snapshot() {
        let mut m = AudioMeter::new(1, 1000.0);
        m.update_channel_count(1);
        m.process_objects(&[0.5], 1);
        m.process_object_bands(&[(0, vec![1.0, 0.5])]);
        std::thread::sleep(Duration::from_millis(3));
        assert_eq!(m.poll().unwrap().object_band_levels.len(), 1);

        // Next interval: crossover off, no band report → no phantom silent
        // bands for the channel.
        m.process_objects(&[0.5], 1);
        std::thread::sleep(Duration::from_millis(3));
        assert!(m.poll().unwrap().object_band_levels.is_empty());
    }

    #[test]
    fn a_band_count_change_restarts_the_channel_sums() {
        let mut m = AudioMeter::new(1, 1000.0);
        m.update_channel_count(1);
        m.process_object_bands(&[(0, vec![1.0, 1.0])]);
        // Topology rebuild mid-interval: 3 bands now. The 2-band sums must not
        // bleed into the 3-band ones.
        m.process_object_bands(&[(0, vec![0.0, 0.0, 4.0])]);
        m.process_objects(&[0.5], 1);

        std::thread::sleep(Duration::from_millis(3));
        let snap = m.poll().unwrap();
        let (_, bands) = &snap.object_band_levels[0];
        assert_eq!(bands.len(), 3);
        assert_eq!(bands[0], DBFS_FLOOR);
        assert_eq!(bands[1], DBFS_FLOOR);
        assert!(bands[2] > DBFS_FLOOR);
    }
}
