//! Bounded measurement playback. Preparation and reclamation run on the
//! control thread; the audio path only reads a buffer and atomics.
use arc_swap::ArcSwapOption;
use omniphony_osc_contract::measurement::{LevelNoise, SignalState, Sweep};
use parking_lot::Mutex;
use std::sync::{
    Arc,
    atomic::{AtomicU8, AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

const QUEUED: u8 = 0;
const RENDERING: u8 = 1;
const COMPLETED: u8 = 2;
const CANCELLED: u8 = 3;
const EXPIRED: u8 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Configuration {
    pub sample_rate: u32,
    pub width: usize,
    pub speaker_generation: u64,
    pub geometry_generation: u64,
    pub master_gain_bits: u32,
}

pub struct Signal {
    pub output_index: usize,
    pub configuration: Configuration,
    samples: Box<[f32]>,
    routing: Box<[usize]>,
    total_frames: usize,
    fade_frames: usize,
    deadline: Instant,
    rendered: AtomicU64,
    state: AtomicU8,
}

impl Signal {
    /// Logarithmic sweep, with a 5 ms raised-cosine fade at both ends.
    pub fn prepare(
        spec: Sweep,
        output_index: usize,
        configuration: Configuration,
        now: Instant,
    ) -> Result<Arc<Self>, &'static str> {
        Self::prepare_referenced(spec, output_index, None, configuration, now)
    }

    pub fn prepare_referenced(
        spec: Sweep,
        output_index: usize,
        reference_index: Option<usize>,
        configuration: Configuration,
        now: Instant,
    ) -> Result<Arc<Self>, &'static str> {
        let routing = match reference_index {
            Some(reference) => vec![reference, output_index, reference],
            None => vec![output_index],
        };
        Self::prepare_routing(spec, output_index, routing, configuration, now)
    }

    pub fn prepare_sequence(
        spec: Sweep,
        outputs: &[usize],
        reference: usize,
        configuration: Configuration,
        now: Instant,
    ) -> Result<Arc<Self>, &'static str> {
        omniphony_osc_contract::measurement::sequence_frames(
            spec,
            configuration.sample_rate,
            outputs.len(),
        )?;
        let mut routing = Vec::with_capacity(outputs.len() * 2 + 1);
        routing.push(reference);
        for &output in outputs {
            routing.push(output);
            routing.push(reference);
        }
        Self::prepare_routing(spec, outputs[0], routing, configuration, now)
    }

    fn prepare_routing(
        spec: Sweep,
        output_index: usize,
        routing: Vec<usize>,
        configuration: Configuration,
        now: Instant,
    ) -> Result<Arc<Self>, &'static str> {
        let Configuration {
            sample_rate, width, ..
        } = configuration;
        spec.validate(sample_rate)?;
        if routing.iter().any(|&i| i >= width) {
            return Err("output index is outside the layout");
        }
        let samples = spec.samples(sample_rate)?.into_boxed_slice();
        let total_frames = samples
            .len()
            .checked_mul(routing.len())
            .ok_or("sequence is too long")?;
        Ok(Arc::new(Self {
            output_index,
            configuration,
            samples,
            routing: routing.into_boxed_slice(),
            total_frames,
            fade_frames: 0,
            deadline: now + Duration::from_secs_f64(total_frames as f64 / sample_rate as f64 + 2.0),
            rendered: AtomicU64::new(0),
            state: AtomicU8::new(QUEUED),
        }))
    }

    /// Prepare one periodic second with 1/f power in the common band. All
    /// transforms, allocations and peak normalization happen off the audio thread.
    pub fn prepare_noise(
        spec: LevelNoise,
        output_index: usize,
        configuration: Configuration,
        now: Instant,
    ) -> Result<Arc<Self>, &'static str> {
        spec.validate(configuration.sample_rate)?;
        if output_index >= configuration.width {
            return Err("output index is outside the layout");
        }
        let rate = configuration.sample_rate as usize;
        let mut planner = realfft::RealFftPlanner::<f32>::new();
        let transform = planner.plan_fft_inverse(rate);
        let mut spectrum = transform.make_input_vec();
        let mut random = 0x6a09e667_u32;
        for (hz, value) in spectrum.iter_mut().enumerate() {
            if (LevelNoise::LOW_HZ as usize..=LevelNoise::HIGH_HZ as usize).contains(&hz) {
                random ^= random << 13;
                random ^= random >> 17;
                random ^= random << 5;
                let phase = std::f32::consts::TAU * (random as f64 / u32::MAX as f64) as f32;
                *value =
                    realfft::num_complex::Complex32::from_polar(1.0 / (hz as f32).sqrt(), phase);
            }
        }
        let mut samples = transform.make_output_vec();
        transform
            .process(&mut spectrum, &mut samples)
            .map_err(|_| "noise transform failed")?;
        let peak = samples.iter().fold(0.0_f32, |a, x| a.max(x.abs()));
        if !peak.is_finite() || peak <= 0.0 {
            return Err("invalid prepared noise");
        }
        let scale = 10.0_f32.powf(spec.peak_dbfs / 20.0) / peak;
        samples.iter_mut().for_each(|x| *x *= scale);
        Ok(Arc::new(Self {
            output_index,
            configuration,
            samples: samples.into_boxed_slice(),
            routing: vec![output_index; spec.duration_seconds as usize].into_boxed_slice(),
            total_frames: spec.frames(configuration.sample_rate) as usize,
            fade_frames: rate / 200,
            deadline: now + Duration::from_secs(u64::from(spec.duration_seconds) + 2),
            rendered: AtomicU64::new(0),
            state: AtomicU8::new(QUEUED),
        }))
    }

    pub fn rendered_frames(&self) -> u64 {
        self.rendered.load(Ordering::Acquire)
    }
    pub fn total_frames(&self) -> u64 {
        self.total_frames as u64
    }
    pub fn state(&self) -> SignalState {
        match self.state.load(Ordering::Acquire) {
            QUEUED => SignalState::Queued,
            RENDERING => SignalState::Rendering,
            COMPLETED => SignalState::Completed,
            EXPIRED => SignalState::Expired,
            _ => SignalState::Cancelled,
        }
    }
    pub fn cancel(&self) {
        self.terminate(CANCELLED);
    }
    pub fn expire(&self) {
        self.terminate(EXPIRED);
    }
    pub fn poll(&self, now: Instant) {
        if now >= self.deadline {
            self.expire();
        }
    }
    fn terminate(&self, state: u8) {
        let mut old = self.state.load(Ordering::Acquire);
        while old < COMPLETED {
            match self
                .state
                .compare_exchange_weak(old, state, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => break,
                Err(current) => old = current,
            }
        }
    }
}

/// One session lease, independent of the OSC thread's scheduling and UI.
pub struct MeasurementControl {
    origin: Instant,
    lease_until_ms: AtomicU64,
    signal: ArcSwapOption<Signal>,
    /// Keep removed buffers alive until no audio guard can own them.
    retired: Mutex<Vec<Arc<Signal>>>,
}

impl Default for MeasurementControl {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
            lease_until_ms: AtomicU64::new(0),
            signal: ArcSwapOption::empty(),
            retired: Mutex::new(Vec::new()),
        }
    }
}

impl MeasurementControl {
    /// Cheap normal-playback fast path; no wall clock or ArcSwap load.
    pub fn armed(&self) -> bool {
        self.lease_until_ms.load(Ordering::Acquire) != 0
    }
    pub fn renew(&self, now: Instant, lease: Duration) {
        self.lease_until_ms.store(
            now.saturating_duration_since(self.origin).as_millis() as u64
                + lease.as_millis() as u64,
            Ordering::Release,
        );
    }
    pub fn active(&self, now: Instant) -> bool {
        (now.saturating_duration_since(self.origin).as_millis() as u64)
            < self.lease_until_ms.load(Ordering::Acquire)
    }
    /// Control thread only.
    pub fn install(&self, signal: Arc<Signal>) {
        self.replace(Some(signal));
    }
    /// Control thread only. Never changes saved renderer settings.
    pub fn end(&self) {
        self.lease_until_ms.store(0, Ordering::Release);
        self.replace(None);
    }
    fn replace(&self, next: Option<Arc<Signal>>) {
        let old = self.signal.swap(next);
        let mut retired = self.retired.lock();
        if let Some(old) = old {
            old.cancel();
            retired.push(old);
        }
        retired.retain(|signal| Arc::strong_count(signal) > 1);
    }

    /// Returns whether the measurement lease owns this block. All other outputs
    /// are silent during the lease, including between bursts and after Stop.
    pub fn render(&self, output: &mut [f32], configuration: Configuration, now: Instant) -> bool {
        let width = configuration.width;
        let signal = self.signal.load();
        if !self.active(now) {
            if let Some(signal) = signal.as_ref() {
                signal.expire();
            }
            return false;
        }
        output.fill(0.0);
        let Some(signal) = signal.as_ref() else {
            return true;
        };
        signal.poll(now);
        if configuration != signal.configuration
            || width == 0
            || !output.len().is_multiple_of(width)
        {
            signal.cancel();
        }
        let state = signal.state.load(Ordering::Acquire);
        if state >= COMPLETED {
            return true;
        }
        if signal
            .state
            .compare_exchange(state, RENDERING, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return true;
        }
        let start = signal.rendered_frames() as usize;
        let count = (output.len() / width).min(signal.total_frames.saturating_sub(start));
        // Split at segment boundaries, so routing and division stay outside
        // the per-sample loop. The same prepared mono buffer is reused.
        let mut copied = 0;
        while copied < count {
            let cursor = start + copied;
            let segment = cursor / signal.samples.len();
            let offset = cursor % signal.samples.len();
            let n = (count - copied).min(signal.samples.len() - offset);
            let channel = signal.routing[segment];
            let frames = output[copied * width..(copied + n) * width]
                .chunks_exact_mut(width)
                .zip(&signal.samples[offset..offset + n]);
            if signal.fade_frames > 0
                && (cursor < signal.fade_frames
                    || cursor + n > signal.total_frames - signal.fade_frames)
            {
                for (i, (frame, sample)) in frames.enumerate() {
                    let distance = (cursor + i).min(signal.total_frames - 1 - cursor - i);
                    let envelope = (distance as f32 / signal.fade_frames as f32).min(1.0);
                    frame[channel] = *sample * envelope;
                }
            } else {
                for (frame, sample) in frames {
                    frame[channel] = *sample;
                }
            }
            copied += n;
        }
        signal
            .rendered
            .store((start + count) as u64, Ordering::Release);
        if start + count == signal.total_frames {
            let _ = signal.state.compare_exchange(
                RENDERING,
                COMPLETED,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> Configuration {
        Configuration {
            sample_rate: 48_000,
            width: 2,
            speaker_generation: 1,
            geometry_generation: 1,
            master_gain_bits: 1.0_f32.to_bits(),
        }
    }
    fn spec() -> Sweep {
        Sweep {
            duration_ms: 100,
            peak_dbfs: -20.0,
            start_hz: 100.0,
            end_hz: 10_000.0,
        }
    }

    #[test]
    fn finite_pink_noise_reuses_a_periodic_buffer_and_stops_without_frontend_polling() {
        let now = Instant::now();
        let spec = LevelNoise {
            duration_seconds: 5,
            peak_dbfs: -30.0,
        };
        let signal = Signal::prepare_noise(spec, 1, config(), now).unwrap();
        assert_eq!(signal.samples.len(), 48000);
        assert_eq!(signal.total_frames(), 240000);
        assert!(
            signal
                .samples
                .iter()
                .all(|v| v.is_finite() && v.abs() <= 10.0_f32.powf(-30.0 / 20.0) + 1e-7)
        );
        // The generated periodic second is zero outside the requested FFT bins;
        // octave energies follow the pink-noise 1/f power distribution.
        let mut planner = realfft::RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(48000);
        let mut input = signal.samples.to_vec();
        let mut spectrum = fft.make_output_vec();
        fft.process(&mut input, &mut spectrum).unwrap();
        let energy = |lo, hi| {
            spectrum[lo..hi]
                .iter()
                .map(|c| c.norm_sqr() as f64)
                .sum::<f64>()
        };
        let in_band = energy(500, 2001);
        assert!(energy(0, 500) + energy(2001, 24001) < in_band * 1e-10);
        assert!((10.0 * (energy(500, 1000) / energy(1000, 2000)).log10()).abs() < 0.02);
        let c = MeasurementControl::default();
        c.renew(now, Duration::from_secs(20));
        c.install(signal.clone());
        let mut out = vec![0.8; 240017 * 2];
        c.render(&mut out, config(), now);
        assert_eq!(signal.state(), SignalState::Completed);
        assert!(out.as_chunks::<2>().0.iter().all(|f| f[0] == 0.0));
        assert!(out[480000..].iter().all(|v| *v == 0.0));
        for changed in [false, true] {
            let c = MeasurementControl::default();
            let signal = Signal::prepare_noise(spec, 1, config(), now).unwrap();
            c.install(signal.clone());
            c.renew(now, Duration::from_secs(3));
            let mut configuration = config();
            if changed {
                configuration.master_gain_bits = 2.0_f32.to_bits();
            }
            let mut out = [0.0; 256];
            c.render(
                &mut out,
                configuration,
                now + Duration::from_secs(if changed { 0 } else { 4 }),
            );
            assert!(out.iter().all(|v| *v == 0.0));
            assert!(matches!(
                signal.state(),
                SignalState::Cancelled | SignalState::Expired
            ));
        }
        assert!(Signal::prepare_noise(spec, 2, config(), now).is_err());
        assert!(
            Signal::prepare_noise(
                LevelNoise {
                    duration_seconds: 61,
                    ..spec
                },
                0,
                config(),
                now
            )
            .is_err()
        );
    }

    #[test]
    fn finite_signal_is_isolated_and_never_restarts() {
        let c = MeasurementControl::default();
        let now = Instant::now();
        c.renew(now, Duration::from_secs(3));
        let s = Signal::prepare(spec(), 1, config(), now).unwrap();
        c.install(s.clone());
        let mut out = vec![0.8; 30000 * 2];
        assert!(c.render(&mut out, config(), now));
        assert_eq!(s.state(), SignalState::Completed);
        assert_eq!(s.rendered_frames(), 28800);
        assert!(out.as_chunks::<2>().0.iter().all(|f| f[0] == 0.0));
        assert!(out.iter().all(|v| v.is_finite() && v.abs() <= 0.1));
        assert!(out.iter().any(|v| v.abs() > 0.09));
        assert!(out[..24000].iter().all(|v| *v == 0.0));
        assert!(out[33600..].iter().all(|v| *v == 0.0));
        c.render(&mut out, config(), now);
        assert!(out.iter().all(|v| *v == 0.0));
    }

    #[test]
    fn timeout_and_configuration_change_stop_playback_without_control_polling() {
        for changed in [true, false] {
            let c = MeasurementControl::default();
            let now = Instant::now();
            c.renew(now, Duration::from_secs(3));
            let s = Signal::prepare(spec(), 0, config(), now).unwrap();
            c.install(s.clone());
            let mut out = [0.0; 128];
            let mut configuration = config();
            if changed {
                configuration.master_gain_bits = 2.0_f32.to_bits();
            }
            c.render(
                &mut out,
                configuration,
                now + Duration::from_secs(if changed { 0 } else { 4 }),
            );
            assert!(matches!(
                s.state(),
                SignalState::Cancelled | SignalState::Expired
            ));
            assert!(out.iter().all(|v| *v == 0.0));
        }
    }

    #[test]
    fn explicit_stop_cannot_be_undone_by_rendering_or_keepalive() {
        let c = MeasurementControl::default();
        let now = Instant::now();
        let s = Signal::prepare(spec(), 0, config(), now).unwrap();
        c.install(s.clone());
        c.renew(now, Duration::from_secs(3));
        s.cancel();
        let mut out = [1.0; 128];
        c.render(&mut out, config(), now);
        assert_eq!(s.rendered_frames(), 0);
        assert_eq!(s.state(), SignalState::Cancelled);
        c.end();
        assert!(!c.active(now));
    }
    #[test]
    fn referenced_sequence_keeps_sample_spacing_across_block_boundaries() {
        let c = MeasurementControl::default();
        let now = Instant::now();
        c.renew(now, Duration::from_secs(30));
        let s = Signal::prepare_referenced(spec(), 1, Some(0), config(), now).unwrap();
        let segment = spec().samples(48000).unwrap();
        assert_eq!(s.total_frames(), 3 * segment.len() as u64);
        c.install(s.clone());
        let mut collected = Vec::new();
        let mut block = vec![1.0; 257 * 2];
        while s.state() != SignalState::Completed {
            c.render(&mut block, config(), now);
            collected.extend_from_slice(&block);
        }
        for (i, frame) in collected.as_chunks::<2>().0.iter().enumerate() {
            let part = i / segment.len();
            let expected = if part < 3 {
                segment[i % segment.len()]
            } else {
                0.0
            };
            let channel = usize::from(part == 1);
            assert_eq!(frame[channel], expected, "frame {i}");
            assert_eq!(frame[1 - channel], 0.0, "frame {i}");
        }
    }

    #[test]
    fn shared_reference_sequence_routes_all_outputs_with_one_reference_between_targets() {
        let c = MeasurementControl::default();
        let now = Instant::now();
        let config = Configuration {
            width: 14,
            ..config()
        };
        let s = Signal::prepare_sequence(spec(), &[5, 9, 10], 0, config, now).unwrap();
        let sample = spec().samples(48000).unwrap();
        assert_eq!(s.total_frames(), sample.len() as u64 * 7);
        c.install(s.clone());
        c.renew(now, Duration::from_secs(3));
        let mut block = vec![0.0; 257 * 14];
        let mut cursor = 0;
        while s.state() != SignalState::Completed {
            c.render(&mut block, config, now);
            for frame in block.as_chunks::<14>().0 {
                let segment = cursor / sample.len();
                for (channel, value) in frame.iter().enumerate() {
                    let expected = if segment < 7 && channel == [0, 5, 0, 9, 0, 10, 0][segment] {
                        sample[cursor % sample.len()]
                    } else {
                        0.0
                    };
                    assert_eq!(*value, expected, "frame {cursor}, channel {channel}");
                }
                cursor += 1;
            }
        }
        assert!(Signal::prepare_sequence(spec(), &[], 0, config, now).is_err());
        assert!(Signal::prepare_sequence(spec(), &[14], 0, config, now).is_err());
        let s = Signal::prepare_sequence(spec(), &[5, 9], 0, config, now).unwrap();
        c.install(s.clone());
        c.render(&mut block, config, now + Duration::from_secs(4));
        assert_eq!(s.state(), SignalState::Expired);
        assert_eq!(s.rendered_frames(), 0);
    }
}
