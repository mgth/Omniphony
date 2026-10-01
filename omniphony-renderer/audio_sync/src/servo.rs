//! The end-to-end latency servo.
//!
//! Once per output callback the servo is told:
//!
//! - when the callback's cycle started and how far the device clock had
//!   advanced (`t`, `device_position`) — this feeds the device-rate DLL;
//! - the latest source observation (`t`, frames received at the capture
//!   point) — this feeds the source-rate DLL;
//! - where the resampler stands (`play_position`, in source frames) and how
//!   far the ring is filled (`available`).
//!
//! From the two DLLs it knows how many source frames the capture point has
//! received *now*, `N_in(t)`, and both rates. The first source frame this
//! callback plays, `P`, is heard after `heard_delay`; it was received
//! `(N_in(t) − P)/rate_S` ago. So
//!
//! ```text
//! L(P) = heard_delay + (N_in(t) + offset − P) / rate_S
//! ```
//!
//! is the end-to-end latency, exact whatever bursts or holds sit between the
//! capture point and the resampler.
//!
//! **Control.** The resampler consumes `ratio` source frames per output frame:
//!
//! ```text
//! ratio = ratio_ff · (1 + u)     ratio_ff = rate_S / rate_D
//! u     = Kp·e + Ki·∫e dt        e = L − L_target (s)
//! ```
//!
//! With the feed-forward exact, the plant is `de/dt = −u`. The closed loop is
//! `s² + Kp·s + Ki`, so the gains come from one bandwidth `B` and a damping
//! `ζ`: `Kp = 2ζω`, `Ki = ω²`, `ω = 2πB`. The integral runs per second of
//! measured time, independent of the callback size.
//!
//! **Start and realign — one rule.** Whenever the latency is not established
//! (start, after an underrun, or when `|e|` exceeds the realign threshold),
//! the next callback sets it exactly. If `L(P)` is above target, it skips
//! `(L − L_target)·rate_S` source frames; if below, it plays
//! `(L_target − L)·rate_D` output frames of silence first. Playback resumes
//! with a short fade-in; a threshold breach fades the current callback out
//! first. The rates and the integrator are kept: they describe the clocks,
//! not the phase.

use crate::dll::{Dll, DllConfig};
use std::f64::consts::TAU;

/// Tunables. Only the first three are meant for users.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ServoConfig {
    /// End-to-end latency to hold (s).
    pub target_latency_s: f64,
    /// Phase-loop bandwidth (Hz).
    pub loop_bandwidth_hz: f64,
    /// Phase-loop bandwidth when playback starts (Hz). Above
    /// `loop_bandwidth_hz`, the loop narrows from it to `loop_bandwidth_hz`,
    /// halving every `loop_narrowing_half_life_s` of running time: fast enough
    /// to absorb the start-up errors, quiet once the estimates have settled.
    /// At or below it, the loop runs at `loop_bandwidth_hz` from the start.
    pub loop_start_bandwidth_hz: f64,
    pub loop_narrowing_half_life_s: f64,
    /// Latency error beyond which the servo realigns instead of steering (s).
    pub realign_threshold_s: f64,
    /// Phase-loop damping ratio.
    pub damping: f64,
    /// Clamp on the phase correction `u` (fraction, ±).
    pub max_correction: f64,
    /// Feed-forward deviation from the nominal ratio beyond which the clocks
    /// are reported as mismatched and the feed-forward is clamped (fraction).
    pub max_feedforward_deviation: f64,
    /// Whether the feed-forward follows the measured rates (true) or stays at
    /// the nominal ratio, leaving all drift to the integrator (false).
    pub feedforward: bool,
    /// Fade length after a start or realign, and before a realign (s).
    pub fade_s: f64,
    /// Nominal source and output rates (Hz).
    pub source_rate_hz: f64,
    pub output_rate_hz: f64,
    /// Source frames the resampler must have beyond its position to produce
    /// output (half its filter length).
    pub resampler_lookahead_frames: f64,
    /// A source silent for longer than this is treated as stopped: playback
    /// waits, and the source DLL restarts its phase when frames come back (s).
    pub source_gap_s: f64,
    /// The source's readings arrive late by a one-sided jitter (a `follow`
    /// source): fit its line to the earliest arrivals
    /// ([`ArrivalEnvelope`](crate::ArrivalEnvelope)) before the source DLL.
    pub source_late_arrivals: bool,
    /// DLL schedules for the source and the device clocks.
    pub source_dll: DllConfig,
    pub device_dll: DllConfig,
}

/// Phase-loop bandwidth for a `follow` source, once settled (Hz).
pub const FOLLOW_LOOP_HZ: f64 = 0.005;
/// ... and when playback starts (Hz), and how fast it narrows (s).
pub const FOLLOW_LOOP_START_HZ: f64 = 0.02;
pub const FOLLOW_LOOP_HALF_LIFE_S: f64 = 15.0;
/// Source-DLL bandwidth for a `follow` source (Hz).
pub const FOLLOW_SOURCE_DLL_HZ: f64 = 0.01;

impl ServoConfig {
    /// Defaults for a `follow` source: bytes that arrive late by a one-sided
    /// jitter (±20 ms measured from mpv into a pipe), a video frame of audio
    /// at a time, possibly far off nominal (mpv's display-resample: up to
    /// ~1000 ppm).
    ///
    /// - The readings are fitted to their earliest-arrival line first (upper
    ///   convex hull over 30 s), which removes the jitter without biasing the
    ///   phase or the rate.
    /// - The source DLL then tracks that line at 0.01 Hz.
    /// - The phase loop runs at 0.01 Hz.
    ///
    /// Closed-loop simulation at the measured jitter, source at 0, 80 and
    /// 1000 ppm, after 60 s: true latency within ±0.5 ms and ratio error
    /// within ~45 ppm (a slow wander, 0.08 cent at most). The first 30 s carry
    /// a transient of up to ~1.5 ms and ~200 ppm. A DLL fed the raw arrivals
    /// gave ±5 ms and 545 ppm.
    pub fn follow() -> Self {
        Self {
            loop_bandwidth_hz: FOLLOW_LOOP_HZ,
            loop_start_bandwidth_hz: FOLLOW_LOOP_START_HZ,
            loop_narrowing_half_life_s: FOLLOW_LOOP_HALF_LIFE_S,
            source_late_arrivals: true,
            // mpv's display-resample moves the source by up to ~1000 ppm
            // (23.976 fps content on a 24 Hz display).
            max_feedforward_deviation: 2000e-6,
            source_dll: DllConfig {
                bandwidth_hz: FOLLOW_SOURCE_DLL_HZ,
                ..DllConfig::default()
            },
            ..Self::default()
        }
    }
}

impl Default for ServoConfig {
    fn default() -> Self {
        Self {
            target_latency_s: 0.100,
            loop_bandwidth_hz: 0.02,
            loop_start_bandwidth_hz: 0.0,
            loop_narrowing_half_life_s: 15.0,
            realign_threshold_s: 0.020,
            damping: 1.0,
            max_correction: 500e-6,
            max_feedforward_deviation: 1000e-6,
            feedforward: true,
            fade_s: 0.005,
            source_rate_hz: 48_000.0,
            output_rate_hz: 48_000.0,
            resampler_lookahead_frames: 32.0,
            source_gap_s: 0.25,
            source_late_arrivals: false,
            source_dll: DllConfig::default(),
            device_dll: DllConfig::default(),
        }
    }
}

/// The latest `(time, frames received)` reading at the capture point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SourceObservation {
    pub t: f64,
    pub received: f64,
}

/// What the output stage knows at the start of a callback.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CallbackInput {
    /// Start of this device cycle on the reference clock (s).
    pub t: f64,
    /// Device position at `t` (output frames).
    pub device_position: f64,
    /// Output frames to produce.
    pub frames: usize,
    /// From `t` until the first frame produced now is heard (s).
    pub heard_delay_s: f64,
    /// Resampler position before this callback (source frames consumed).
    pub play_position: f64,
    /// Source frames written to the ring so far (end of the readable data).
    pub available: f64,
    /// Latest capture-point reading, if any yet.
    pub source: Option<SourceObservation>,
    /// `inserted − dropped` from the [`Accounting`](crate::Accounting).
    pub source_offset: f64,
}

/// What the output stage must do this callback.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CallbackPlan {
    /// Output frames of silence at the start of the callback. Equal to the
    /// callback size when nothing is played.
    pub silence_frames: usize,
    /// Source frames to skip before playing (whole or fractional).
    pub skip_source_frames: f64,
    /// Source frames consumed per output frame for the played part.
    pub ratio: f64,
    /// Output frames of fade-in from the first played frame (0 = none).
    pub fade_in_frames: usize,
    /// Fade the played part of this callback out to silence.
    pub fade_out: bool,
}

impl CallbackPlan {
    fn silent(frames: usize, ratio: f64) -> Self {
        Self {
            silence_frames: frames,
            skip_source_frames: 0.0,
            ratio,
            fade_in_frames: 0,
            fade_out: false,
        }
    }

    /// Output frames this plan plays from the ring.
    pub fn played_frames(&self, frames: usize) -> usize {
        frames.saturating_sub(self.silence_frames)
    }
}

/// Where the servo stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Latency not established yet: waiting for data or about to set it.
    Starting,
    /// Steering the ratio.
    Running,
    /// A realign is due on the next callback.
    Realigning,
}

/// What the servo publishes for display and diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Telemetry {
    /// Measured end-to-end latency at the last callback (s), once known.
    pub latency_s: Option<f64>,
    /// `latency − target` while running (s).
    pub error_s: f64,
    /// Source and device rates against the reference clock (ppm off nominal).
    pub source_ppm: f64,
    pub device_ppm: f64,
    /// Feed-forward ratio and phase correction (ppm off nominal / ppm).
    pub feedforward_ppm: f64,
    pub correction_ppm: f64,
    /// Ratio applied (source frames per output frame).
    pub ratio: f64,
    pub realigns: u64,
    pub underruns: u64,
    /// Times a `follow` source was taken to have lost or gained time (a
    /// pause, a seek) and its estimate started a new phase.
    pub source_rephases: u64,
    /// The feed-forward hit `max_feedforward_deviation`: the nominal rates are
    /// wrong or a clock is broken.
    pub clock_mismatch: bool,
    /// Lowest latency the pipeline can currently sustain (s): what the ring
    /// lags the capture point by, plus one callback's consumption and the
    /// resampler look-ahead, plus the device delay. Peak-held, relaxing
    /// toward the current value with time constant [`FLOOR_HOLD_S`]. A target
    /// below it underruns: the decoder holds or batches more than the target
    /// leaves room for.
    pub latency_floor_s: f64,
}

/// Time constant with which [`Telemetry::latency_floor_s`] forgets a peak (s).
pub const FLOOR_HOLD_S: f64 = 5.0;

/// See the [module docs](self).
#[derive(Debug, Clone)]
pub struct Servo {
    config: ServoConfig,
    nominal_ratio: f64,
    source: crate::source::SourceEstimator,
    device_dll: Dll,
    last_source_t: Option<f64>,
    source_stalled: bool,
    phase: Phase,
    /// The integral term itself (`∫Ki·e dt`), so a gain that narrows does not
    /// rescale what has been integrated.
    integral: f64,
    /// When playback first started in this epoch, for the loop's narrowing.
    running_since: Option<f64>,
    last_t: Option<f64>,
    telemetry: Telemetry,
}

impl Servo {
    pub fn new(config: ServoConfig) -> Self {
        Self {
            nominal_ratio: config.source_rate_hz / config.output_rate_hz,
            source: crate::source::SourceEstimator::new(
                config.source_dll,
                config.source_late_arrivals,
                config.source_rate_hz,
            ),
            device_dll: Dll::new(config.device_dll, config.output_rate_hz),
            last_source_t: None,
            source_stalled: true,
            phase: Phase::Starting,
            integral: 0.0,
            running_since: None,
            last_t: None,
            telemetry: Telemetry {
                ratio: config.source_rate_hz / config.output_rate_hz,
                ..Telemetry::default()
            },
            config,
        }
    }

    pub fn config(&self) -> &ServoConfig {
        &self.config
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn telemetry(&self) -> &Telemetry {
        &self.telemetry
    }

    /// Start a new epoch: forget both clocks and the integrator, and set the
    /// latency again from scratch on the next callback.
    pub fn reset(&mut self) {
        self.source.reset();
        self.device_dll.reset();
        self.last_source_t = None;
        self.source_stalled = true;
        self.phase = Phase::Starting;
        self.integral = 0.0;
        self.running_since = None;
        self.last_t = None;
    }

    /// Decide this callback. See the [module docs](self).
    pub fn plan(&mut self, input: &CallbackInput) -> CallbackPlan {
        let dt = self
            .last_t
            .map(|last| (input.t - last).clamp(0.0, 0.5))
            .unwrap_or(0.0);
        self.last_t = Some(input.t);
        self.device_dll.observe(input.t, input.device_position);
        self.observe_source(input);

        let ratio_ff = self.feedforward_ratio();
        let rate_s = self.source.rate().unwrap_or(self.config.source_rate_hz);
        let rate_d = self.device_dll.rate().unwrap_or(self.config.output_rate_hz);
        self.telemetry.source_ppm = ppm(rate_s / self.config.source_rate_hz);
        self.telemetry.device_ppm = ppm(rate_d / self.config.output_rate_hz);
        self.telemetry.feedforward_ppm = ppm(ratio_ff / self.nominal_ratio);
        self.telemetry.source_rephases = self.source.rephases();

        // Silent until the source has delivered something, or while it is
        // stalled: there is nothing to measure the latency against.
        let n_in = match self.source.position_at(input.t) {
            Some(p) if !self.source_stalled => p + input.source_offset,
            _ => {
                self.telemetry.latency_s = None;
                if self.phase == Phase::Running {
                    self.phase = Phase::Realigning;
                }
                return self.silent(input.frames, ratio_ff);
            }
        };
        let latency = input.heard_delay_s + (n_in - input.play_position) / rate_s;
        self.telemetry.latency_s = Some(latency);
        let floor = input.heard_delay_s
            + (n_in - input.available
                + ratio_ff * input.frames as f64
                + self.config.resampler_lookahead_frames)
                / rate_s;
        let held = self.telemetry.latency_floor_s;
        let relaxed = held - (held - floor) * (dt / FLOOR_HOLD_S).min(1.0);
        self.telemetry.latency_floor_s = floor.max(relaxed);
        let error = latency - self.config.target_latency_s;

        match self.phase {
            Phase::Starting | Phase::Realigning => {
                self.establish(input, error, rate_s, rate_d, ratio_ff)
            }
            Phase::Running => {
                if error.abs() > self.config.realign_threshold_s {
                    // Steering cannot recover this without an audible pitch
                    // excursion: fade out now, set the latency next callback.
                    self.phase = Phase::Realigning;
                    let mut plan = self.steer(input.t, error, ratio_ff, 0.0);
                    plan.fade_out = true;
                    return self.guard_underrun(input, plan);
                }
                let plan = self.steer(input.t, error, ratio_ff, dt);
                self.guard_underrun(input, plan)
            }
        }
    }

    fn observe_source(&mut self, input: &CallbackInput) {
        let Some(obs) = input.source else {
            return;
        };
        let fresh = self.last_source_t.is_none_or(|last| obs.t > last);
        if fresh {
            let resumed = self
                .last_source_t
                .is_some_and(|last| obs.t - last > self.config.source_gap_s);
            if resumed && self.source.is_tracking() {
                // The source stopped and came back: same clock, new phase.
                self.source.restart_phase(obs.t, obs.received);
            } else {
                self.source.observe(obs.t, obs.received);
            }
            self.last_source_t = Some(obs.t);
            self.source_stalled = false;
        } else if self
            .last_source_t
            .is_some_and(|last| input.t - last > self.config.source_gap_s)
        {
            self.source_stalled = true;
        }
    }

    fn feedforward_ratio(&mut self) -> f64 {
        let nominal = self.nominal_ratio;
        if !self.config.feedforward {
            self.telemetry.clock_mismatch = false;
            return nominal;
        }
        let (Some(rate_s), Some(rate_d)) = (self.source.rate(), self.device_dll.rate()) else {
            return nominal;
        };
        let measured = rate_s / rate_d;
        let max = self.config.max_feedforward_deviation;
        let clamped = measured.clamp(nominal * (1.0 - max), nominal * (1.0 + max));
        self.telemetry.clock_mismatch = clamped != measured;
        clamped
    }

    fn silent(&mut self, frames: usize, ratio: f64) -> CallbackPlan {
        self.telemetry.ratio = ratio;
        self.telemetry.correction_ppm = 0.0;
        CallbackPlan::silent(frames, ratio)
    }

    /// Set the latency exactly this callback: skip source frames if it is too
    /// high, lead with silence if it is too low. Waits (silently) while the
    /// ring does not hold enough to start.
    fn establish(
        &mut self,
        input: &CallbackInput,
        error: f64,
        rate_s: f64,
        rate_d: f64,
        ratio_ff: f64,
    ) -> CallbackPlan {
        let ratio = ratio_ff * (1.0 + self.clamped_correction(0.0, 0.0));
        let (silence, skip) = if error >= 0.0 {
            (0usize, error * rate_s)
        } else {
            let silence = (-error * rate_d).round();
            (silence.min(input.frames as f64) as usize, 0.0)
        };
        if silence >= input.frames {
            // The latency is still short by a whole callback or more.
            return self.silent(input.frames, ratio);
        }
        let played = input.frames - silence;
        let needed = input.play_position
            + skip
            + ratio * played as f64
            + self.config.resampler_lookahead_frames;
        if needed > input.available {
            // Not enough decoded audio to start at the target yet.
            return self.silent(input.frames, ratio);
        }
        if self.phase == Phase::Realigning {
            self.telemetry.realigns += 1;
        }
        self.phase = Phase::Running;
        self.telemetry.ratio = ratio;
        CallbackPlan {
            silence_frames: silence,
            skip_source_frames: skip,
            ratio,
            fade_in_frames: (self.config.fade_s * self.config.output_rate_hz).round() as usize,
            fade_out: false,
        }
    }

    /// `(Kp, Ki)` at `age` seconds of running: `Kp = 2ζω`, `Ki = ω²`, with
    /// the bandwidth narrowing as configured.
    fn gains(&self, age: f64) -> (f64, f64) {
        let c = &self.config;
        let bandwidth = if c.loop_start_bandwidth_hz > c.loop_bandwidth_hz {
            let halvings =
                (age.max(0.0) / c.loop_narrowing_half_life_s.max(f64::EPSILON)).min(1000.0);
            (c.loop_start_bandwidth_hz * 0.5f64.powf(halvings)).max(c.loop_bandwidth_hz)
        } else {
            c.loop_bandwidth_hz
        };
        let omega = TAU * bandwidth;
        (2.0 * c.damping * omega, omega * omega)
    }

    fn steer(&mut self, t: f64, error: f64, ratio_ff: f64, dt: f64) -> CallbackPlan {
        self.telemetry.error_s = error;
        let age = t - *self.running_since.get_or_insert(t);
        let (kp, ki) = self.gains(age);
        // Conditional integration: stop integrating while the correction is
        // saturated in the direction the error pushes.
        let unclamped = kp * error + self.integral;
        let saturated =
            unclamped.abs() >= self.config.max_correction && unclamped.signum() == error.signum();
        if !saturated {
            self.integral += ki * error * dt;
        }
        let u = self.clamped_correction(kp, error);
        let ratio = ratio_ff * (1.0 + u);
        self.telemetry.correction_ppm = u * 1e6;
        self.telemetry.ratio = ratio;
        CallbackPlan {
            silence_frames: 0,
            skip_source_frames: 0.0,
            ratio,
            fade_in_frames: 0,
            fade_out: false,
        }
    }

    fn clamped_correction(&self, kp: f64, error: f64) -> f64 {
        let max = self.config.max_correction;
        (kp * error + self.integral).clamp(-max, max)
    }

    /// Turn a plan the ring cannot feed into a silent callback and a realign.
    fn guard_underrun(&mut self, input: &CallbackInput, plan: CallbackPlan) -> CallbackPlan {
        let needed = input.play_position
            + plan.skip_source_frames
            + plan.ratio * plan.played_frames(input.frames) as f64
            + self.config.resampler_lookahead_frames;
        if needed <= input.available {
            return plan;
        }
        self.telemetry.underruns += 1;
        self.source.rephase_on_next_reading();
        self.phase = Phase::Realigning;
        self.silent(input.frames, plan.ratio)
    }
}

fn ppm(ratio: f64) -> f64 {
    (ratio - 1.0) * 1e6
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(t: f64, play_position: f64, received: f64) -> CallbackInput {
        CallbackInput {
            t,
            device_position: t * 48_000.0,
            frames: 1024,
            heard_delay_s: 0.032,
            play_position,
            available: received,
            source: Some(SourceObservation { t, received }),
            source_offset: 0.0,
        }
    }

    #[test]
    fn gains_follow_the_bandwidth() {
        let servo = Servo::new(ServoConfig::default());
        let omega = TAU * 0.02;
        let (kp, ki) = servo.gains(100.0);
        assert!((kp - 2.0 * omega).abs() < 1e-12);
        assert!((ki - omega * omega).abs() < 1e-12);
    }

    #[test]
    fn a_follow_loop_starts_wide_and_narrows() {
        let servo = Servo::new(ServoConfig::follow());
        let kp_at = |age| servo.gains(age).0 / (2.0 * TAU);
        assert!((kp_at(0.0) - FOLLOW_LOOP_START_HZ).abs() < 1e-12);
        assert!((kp_at(FOLLOW_LOOP_HALF_LIFE_S) - FOLLOW_LOOP_START_HZ / 2.0).abs() < 1e-12);
        assert!((kp_at(600.0) - FOLLOW_LOOP_HZ).abs() < 1e-12);
    }

    #[test]
    fn stays_silent_until_the_source_delivers() {
        let mut servo = Servo::new(ServoConfig::default());
        let mut inp = input(0.0, 0.0, 0.0);
        inp.source = None;
        let plan = servo.plan(&inp);
        assert_eq!(plan.silence_frames, 1024);
        assert_eq!(servo.phase(), Phase::Starting);
    }

    /// Latency below target: lead with exactly the missing silence.
    #[test]
    fn starts_with_the_missing_silence() {
        let mut servo = Servo::new(ServoConfig::default());
        // 0.050 s of audio received at t: L(0) = 0.032 + 0.050 = 0.082,
        // 18 ms short of 0.100 → 864 frames of silence.
        let plan = servo.plan(&input(1.0, 0.0, 2_400.0));
        assert_eq!(plan.silence_frames, 864);
        assert_eq!(plan.skip_source_frames, 0.0);
        assert!(plan.fade_in_frames > 0);
        assert_eq!(servo.phase(), Phase::Running);
    }

    /// Latency above target: skip exactly the excess.
    #[test]
    fn starts_by_skipping_the_excess() {
        let mut servo = Servo::new(ServoConfig::default());
        // 0.100 s received: L(0) = 0.132 → skip 32 ms = 1536 frames.
        let plan = servo.plan(&input(1.0, 0.0, 4_800.0));
        assert_eq!(plan.silence_frames, 0);
        assert!((plan.skip_source_frames - 1536.0).abs() < 1e-6);
    }

    #[test]
    fn waits_while_the_latency_is_short_by_a_whole_callback() {
        let mut servo = Servo::new(ServoConfig::default());
        let plan = servo.plan(&input(1.0, 0.0, 480.0));
        assert_eq!(plan.silence_frames, 1024);
        assert_eq!(servo.phase(), Phase::Starting);
    }

    #[test]
    fn an_underrun_goes_silent_and_realigns() {
        let mut servo = Servo::new(ServoConfig::default());
        servo.plan(&input(1.0, 0.0, 4_800.0));
        assert_eq!(servo.phase(), Phase::Running);
        let mut inp = input(1.0 + 1024.0 / 48_000.0, 1_536.0 + 1024.0, 4_800.0 + 1024.0);
        inp.available = 1_536.0 + 1024.0 + 100.0;
        let plan = servo.plan(&inp);
        assert_eq!(plan.silence_frames, 1024);
        assert_eq!(servo.phase(), Phase::Realigning);
        assert_eq!(servo.telemetry().underruns, 1);
    }

    #[test]
    fn a_large_error_fades_out_then_realigns() {
        let mut servo = Servo::new(ServoConfig::default());
        servo.plan(&input(1.0, 0.0, 4_800.0));
        // 50 ms of gap filled upstream (accounted): the latency steps by
        // 50 ms at once, far beyond the 20 ms threshold.
        let t = 1.0 + 1024.0 / 48_000.0;
        let mut inp = input(t, 1_536.0 + 1024.0, 4_800.0 + 1024.0);
        inp.source_offset = 2_400.0;
        inp.available += 2_400.0;
        let plan = servo.plan(&inp);
        assert!(plan.fade_out);
        assert_eq!(servo.phase(), Phase::Realigning);
        let t2 = t + 1024.0 / 48_000.0;
        let mut inp = input(t2, 1_536.0 + 2048.0, 4_800.0 + 2048.0);
        inp.source_offset = 2_400.0;
        inp.available += 2_400.0;
        let plan = servo.plan(&inp);
        assert!(plan.skip_source_frames > 2_000.0);
        assert_eq!(servo.telemetry().realigns, 1);
        assert_eq!(servo.phase(), Phase::Running);
    }

    #[test]
    fn the_correction_is_clamped() {
        let config = ServoConfig {
            loop_bandwidth_hz: 10.0,
            ..ServoConfig::default()
        };
        let mut servo = Servo::new(config);
        servo.plan(&input(1.0, 0.0, 4_800.0));
        // 10 ms over target: well within the realign threshold but huge for
        // a 10 Hz loop.
        let t = 1.0 + 1024.0 / 48_000.0;
        let mut inp = input(t, 1_536.0 + 1024.0, 4_800.0 + 1024.0);
        inp.source_offset = 480.0;
        inp.available += 480.0;
        let plan = servo.plan(&inp);
        assert!(
            (plan.ratio - (1.0 + 500e-6)).abs() < 1e-9,
            "ratio {}",
            plan.ratio
        );
    }

    #[test]
    fn a_stalled_source_silences_and_its_return_restarts_the_phase() {
        let mut servo = Servo::new(ServoConfig::default());
        servo.plan(&input(1.0, 0.0, 4_800.0));
        // No new source observation for 0.5 s.
        let mut inp = input(1.5, 1_536.0 + 1024.0, 4_800.0);
        inp.source = Some(SourceObservation {
            t: 1.0,
            received: 4_800.0,
        });
        let plan = servo.plan(&inp);
        assert_eq!(plan.silence_frames, 1024);
        assert_eq!(servo.phase(), Phase::Realigning);
        // It comes back: the phase restarts at the new observation.
        let plan = servo.plan(&input(2.0, 2_560.0, 4_800.0 + 4_800.0));
        assert_eq!(servo.phase(), Phase::Running);
        assert!(plan.silence_frames < 1024 || plan.skip_source_frames > 0.0);
    }

    /// A `follow` source pauses for 0.2 s, under the gap that marks a stall,
    /// and resumes where it was. The playback runs dry, and the estimate,
    /// still on the old line, would point the realign at frames that never
    /// come: the next reading must start a new phase instead, so playback
    /// resumes at the target.
    #[test]
    fn a_short_pause_of_a_follow_source_realigns_and_resumes() {
        let mut servo = Servo::new(ServoConfig::follow());
        let frames = 1024usize;
        let dt = frames as f64 / 48_000.0;
        let (pause_at, pause_s) = (30.0, 0.2);
        let mut play = 0.0;
        let mut last_source_t = 0.0;
        let mut last_obs = SourceObservation {
            t: 0.0,
            received: 0.0,
        };
        for k in 0..(40.0 / dt) as usize {
            let t = k as f64 * dt;
            let source_t = if t < pause_at {
                t
            } else if t < pause_at + pause_s {
                last_obs.t
            } else {
                t - pause_s
            };
            if source_t > last_source_t || k == 0 {
                last_source_t = source_t;
                last_obs = SourceObservation {
                    t,
                    received: source_t * 48_000.0,
                };
            }
            let mut inp = input(t, play, last_obs.received);
            inp.source = Some(last_obs);
            let plan = servo.plan(&inp);
            let played = frames - plan.silence_frames.min(frames);
            play += plan.skip_source_frames + plan.ratio * played as f64;

            {
                eprintln!(
                    "t={t:.3} ph={:?} lat={:?} err={:.4} play={play:.0} avail={:.0} sil={} skip={:.0} reph={}",
                    servo.phase(),
                    servo.telemetry().latency_s,
                    servo.telemetry().error_s,
                    last_obs.received,
                    plan.silence_frames,
                    plan.skip_source_frames,
                    servo.telemetry().source_rephases
                );
            }
            if t > pause_at + pause_s + 1.0 {
                assert_eq!(servo.phase(), Phase::Running, "stuck at t = {t:.2}");
                let error = servo.telemetry().error_s;
                assert!(error.abs() < 1e-3, "error {error} at t = {t:.2}");
            }
        }
        assert_eq!(servo.telemetry().realigns, 1);
        assert_eq!(servo.telemetry().source_rephases, 1);
    }
}
