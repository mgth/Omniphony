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

/// What paces playback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pacing {
    /// The source runs on a clock of its own (`own`, `follow`): the servo
    /// measures it and resamples to hold the latency against it.
    #[default]
    Source,
    /// The source has no clock (`none`: a file, a writer faster than real
    /// time). The device is the master: playback runs at the nominal ratio,
    /// takes what the ring holds, and the producer is held back by the ring.
    /// Nothing is measured against the source, so there is no latency to
    /// hold: [`ServoConfig::target_latency_s`] is how much the producer keeps
    /// ahead, which is its business.
    Device,
}

/// Tunables. Only the first three are meant for users.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ServoConfig {
    /// What paces playback.
    pub pacing: Pacing,
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
    /// Headroom kept above the latency floor when the target is raised to it
    /// (see [`Telemetry::target_s`]) (s).
    pub floor_margin_s: f64,
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
            pacing: Pacing::Source,
            target_latency_s: 0.100,
            loop_bandwidth_hz: 0.02,
            loop_start_bandwidth_hz: 0.0,
            loop_narrowing_half_life_s: 15.0,
            realign_threshold_s: 0.020,
            floor_margin_s: 0.005,
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
    /// Discontinuities the capture side has seen in the source (another
    /// stream started: a track change), counted. A change means the source
    /// broke its phase: the servo restarts it and realigns.
    pub source_breaks: u64,
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
    /// Lowest latency the pipeline was seen to need (s): what the ring lags
    /// the capture point by, plus one callback's consumption and the
    /// resampler look-ahead, plus the device delay.
    ///
    /// It is read once per burst cycle, when audio reaches the ring: the
    /// highest value at a callback since the previous delivery is what that
    /// wait needed. A wait that ends in a stall or a break instead (a pause,
    /// the end of the stream, a track change) says nothing about the
    /// pipeline and is not counted. Peak-held, relaxing toward the latest
    /// cycle with time constant [`FLOOR_HOLD_S`]. A target below it
    /// underruns: the decoder holds or batches more than the target leaves
    /// room for.
    pub latency_floor_s: f64,
    /// The latency the servo holds (s): the configured target, or more when
    /// the pipeline cannot sustain it (see [`Servo::track_floor`]). Raised
    /// within an epoch, never lowered.
    pub target_s: f64,
}

/// Time constant with which [`Telemetry::latency_floor_s`] forgets a peak (s).
pub const FLOOR_HOLD_S: f64 = 5.0;
/// How far behind its line the reading after an underrun may be for an
/// exact (`own`) source to count as having lost nothing (s).
pub const SOURCE_LOSS_TOLERANCE_S: f64 = 0.002;
/// How far behind its line the reading after an underrun may be for a
/// `follow` source to count as not having stopped (s): the estimator's own
/// measure of a phase break. Its readings are late by their jitter, so one
/// of them cannot say the source lost nothing; within this, it says the
/// underrun was not a stop, and the pipeline's own.
pub const FOLLOW_LOSS_TOLERANCE_S: f64 = crate::source::PHASE_BREAK_S;
/// Burst cycles above the target that raise it, and the time they must fall
/// within (s). One cycle above it is a hiccup; a target the pipeline cannot
/// sustain is exceeded again and again.
pub const FLOOR_CYCLES: usize = 3;
pub const FLOOR_WINDOW_S: f64 = 10.0;
/// After a raise, how long the cycles that follow may still lift the level
/// it was set from (s): long enough to see the decoder's whole delivery
/// pattern, which the cycles that triggered the raise were only a part of.
pub const FLOOR_SETTLE_S: f64 = 2.0;

/// See the [module docs](self).
#[derive(Debug, Clone)]
pub struct Servo {
    config: ServoConfig,
    nominal_ratio: f64,
    source: crate::source::SourceEstimator,
    device_dll: Dll,
    last_source_t: Option<f64>,
    source_stalled: bool,
    /// [`CallbackInput::source_breaks`] as last seen.
    source_breaks: u64,
    /// The next fresh reading starts a new source phase (after a break the
    /// capture side reported, or a `follow` underrun).
    restart_next: bool,
    /// That restart was asked for by an underrun, not by a reported break:
    /// an exact source whose reading is still on its line lost nothing, and
    /// keeps its phase.
    restart_after_underrun: bool,
    /// Source phase restarts the servo made.
    restarts: u64,
    phase: Phase,
    /// The integral term itself (`∫Ki·e dt`), so a gain that narrows does not
    /// rescale what has been integrated.
    integral: f64,
    /// When playback first started in this epoch, for the loop's narrowing.
    running_since: Option<f64>,
    last_t: Option<f64>,
    /// The target in force ([`Telemetry::target_s`]).
    target_s: f64,
    /// [`CallbackInput::available`] at the last callback: more at the next
    /// one is a delivery, the end of a burst cycle.
    last_available: f64,
    /// The floor this callback read, and the one read by the callback that
    /// underran, until the source's next reading says what the underrun was.
    last_floor: f64,
    underrun_floor: Option<f64>,
    /// An underrun the source's next reading showed was not a stop: a cycle
    /// above the target, at this level, to count.
    pipeline_underrun: Option<f64>,
    /// Highest floor at a callback since the last delivery; `None` while
    /// what is seen cannot be trusted (a stall, a break).
    cycle: Option<FloorCycle>,
    /// When the floor was last read (the end of the last cycle counted).
    floor_read_at: Option<f64>,
    /// The latest cycles whose floor was above the target in force, oldest
    /// first: `(end time, floor)`.
    over_target: [(f64, f64); FLOOR_CYCLES],
    over_target_len: usize,
    /// The level the target was last raised from, and until when the cycles
    /// that follow may lift it ([`FLOOR_SETTLE_S`]).
    raised_level: Option<(f64, f64)>,
    telemetry: Telemetry,
}

/// A burst cycle in progress: from one delivery into the ring to the next.
#[derive(Debug, Clone, Copy)]
struct FloorCycle {
    /// Highest floor seen in it (s).
    peak_s: f64,
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
            source_breaks: 0,
            restart_next: false,
            restart_after_underrun: false,
            restarts: 0,
            phase: Phase::Starting,
            integral: 0.0,
            running_since: None,
            last_t: None,
            target_s: config.target_latency_s,
            last_available: 0.0,
            last_floor: 0.0,
            underrun_floor: None,
            pipeline_underrun: None,
            cycle: None,
            floor_read_at: None,
            over_target: [(0.0, 0.0); FLOOR_CYCLES],
            over_target_len: 0,
            raised_level: None,
            telemetry: Telemetry {
                ratio: config.source_rate_hz / config.output_rate_hz,
                target_s: config.target_latency_s,
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
        self.restart_next = false;
        self.restart_after_underrun = false;
        self.phase = Phase::Starting;
        self.integral = 0.0;
        self.running_since = None;
        self.last_t = None;
        self.target_s = self.config.target_latency_s;
        self.telemetry.target_s = self.target_s;
        self.last_available = 0.0;
        self.underrun_floor = None;
        self.pipeline_underrun = None;
        self.forget_floor_cycles();
        self.floor_read_at = None;
    }

    /// Decide this callback. See the [module docs](self).
    pub fn plan(&mut self, input: &CallbackInput) -> CallbackPlan {
        if self.config.pacing == Pacing::Device {
            return self.plan_device_paced(input);
        }
        let dt = self
            .last_t
            .map(|last| (input.t - last).clamp(0.0, 0.5))
            .unwrap_or(0.0);
        self.last_t = Some(input.t);
        self.device_dll.observe(input.t, input.device_position);
        // A break the capture side saw: the reading that carries the new
        // stream starts a new phase, and the latency is set again on it.
        let source_broke = input.source_breaks != self.source_breaks;
        if source_broke {
            self.source_breaks = input.source_breaks;
            self.restart_next = true;
            self.restart_after_underrun = false;
        }
        let restarted = self.observe_source(input);
        if source_broke {
            // Another stream: what its decoder batches is yet to be seen.
            self.forget_floor_cycles();
        } else if restarted {
            // The source started a new phase: what the ring lagged it by
            // until now was the break's, not the pipeline's.
            self.cycle = None;
        }

        let ratio_ff = self.feedforward_ratio();
        let rate_s = self.source.rate().unwrap_or(self.config.source_rate_hz);
        let rate_d = self.device_dll.rate().unwrap_or(self.config.output_rate_hz);
        self.telemetry.source_ppm = ppm(rate_s / self.config.source_rate_hz);
        self.telemetry.device_ppm = ppm(rate_d / self.config.output_rate_hz);
        self.telemetry.feedforward_ppm = ppm(ratio_ff / self.nominal_ratio);
        self.telemetry.source_rephases = self.restarts + self.source.detected_breaks();

        // Silent until the source has delivered something, or while it is
        // stalled: there is nothing to measure the latency against.
        let n_in = match self.source.position_at(input.t) {
            Some(p) if !self.source_stalled => p + input.source_offset,
            _ => {
                self.telemetry.latency_s = None;
                self.cycle = None;
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
        self.last_floor = floor;
        // While a restart is pending, whether the source lost time is not
        // known yet: a cycle that ends meanwhile is not read.
        let mut raised = self.track_floor(input.t, dt, input.available, floor, self.restart_next);
        // An underrun under a source that had not stopped was a cycle above
        // the target, whatever the callbacks before it read: the peak comes
        // up to one period after the last of them, and with a long device
        // cycle that is where the underruns are.
        if let Some(level) = self.pipeline_underrun.take() {
            raised |= self.count_over_target(input.t, dt, level.max(self.target_s));
        }
        let error = latency - self.target_s;

        match self.phase {
            // The source is between phases: the latency is set once the
            // reading that starts the new one has come.
            Phase::Starting | Phase::Realigning if self.restart_next => {
                self.silent(input.frames, ratio_ff)
            }
            Phase::Starting | Phase::Realigning => {
                self.establish(input, error, rate_s, rate_d, ratio_ff)
            }
            Phase::Running => {
                if source_broke || raised || error.abs() > self.config.realign_threshold_s {
                    // A new stream, a target just raised, or an error
                    // steering cannot recover without an audible pitch
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

    /// [`Pacing::Device`]: play what the ring holds at the nominal ratio, or
    /// nothing when it does not hold a callback's worth. No source is read
    /// and nothing is steered. What is published as the latency is what is
    /// heard after the ring's read front: the audio buffered ahead plus the
    /// device delay.
    fn plan_device_paced(&mut self, input: &CallbackInput) -> CallbackPlan {
        self.last_t = Some(input.t);
        self.device_dll.observe(input.t, input.device_position);
        let rate_d = self.device_dll.rate().unwrap_or(self.config.output_rate_hz);
        let ratio = self.nominal_ratio;
        self.telemetry.device_ppm = ppm(rate_d / self.config.output_rate_hz);
        self.telemetry.source_ppm = 0.0;
        self.telemetry.feedforward_ppm = 0.0;
        self.telemetry.correction_ppm = 0.0;
        self.telemetry.error_s = 0.0;
        self.telemetry.ratio = ratio;
        let buffered = (input.available - input.play_position).max(0.0);
        self.telemetry.latency_s =
            Some(input.heard_delay_s + buffered / self.config.source_rate_hz);

        let needed = input.play_position
            + ratio * input.frames as f64
            + self.config.resampler_lookahead_frames;
        if needed > input.available {
            // The producer is behind (the start, a slow decode) or done.
            if self.phase == Phase::Running {
                self.telemetry.underruns += 1;
                self.phase = Phase::Starting;
            }
            return CallbackPlan::silent(input.frames, ratio);
        }
        let resuming = self.phase != Phase::Running;
        self.phase = Phase::Running;
        CallbackPlan {
            silence_frames: 0,
            skip_source_frames: 0.0,
            ratio,
            // After a gap, not on every callback.
            fade_in_frames: if resuming {
                (self.config.fade_s * self.config.output_rate_hz).round() as usize
            } else {
                0
            },
            fade_out: false,
        }
    }

    /// Follow the latency floor through the burst cycles, and raise the
    /// target when they show it cannot be sustained. Returns whether the
    /// target was raised.
    ///
    /// The ring lags the capture point along a sawtooth: it grows until the
    /// decoder delivers, then drops. The latency needed is set by the
    /// sawtooth's peaks, but the servo only sees it at its callbacks: the
    /// last one before a delivery read `floor`, and the peak came within one
    /// callback period `dt` after it. So a cycle's peak is somewhere in
    /// `floor..floor + dt`, and where depends on how the deliveries fall
    /// against the callbacks, which drifts.
    ///
    /// A cycle whose `floor` is above the target underruns there, or keeps
    /// the servo from starting. [`FLOOR_CYCLES`] of them within
    /// [`FLOOR_WINDOW_S`] raise the target to their level plus `dt` and
    /// [`ServoConfig::floor_margin_s`]: above the peak of every cycle seen,
    /// however the deliveries fall later, at the price of up to one callback
    /// period more than the lowest latency that would do. The level is the
    /// highest of them, or their median when the highest stands more than a
    /// callback period above it: one long cycle (a hiccup that did not
    /// underrun) does not set the latency.
    ///
    /// Those cycles are only a part of the decoder's delivery pattern. For
    /// [`FLOOR_SETTLE_S`] after a raise, a cycle up to one callback period
    /// above the level lifts it, and the target with it, without a realign:
    /// the servo steers to the few ms of difference.
    fn track_floor(
        &mut self,
        t: f64,
        dt: f64,
        available: f64,
        floor: f64,
        uncertain: bool,
    ) -> bool {
        let delivered = available > self.last_available;
        self.last_available = available;
        if !delivered {
            let peak_s = self.cycle.map_or(floor, |c| c.peak_s.max(floor));
            self.cycle = Some(FloorCycle { peak_s });
            return false;
        }
        // Audio reached the ring: the wait before it was the pipeline's own.
        // (Without a callback seen before the delivery there is no reading
        // of that wait.)
        let Some(cycle) = self.cycle.replace(FloorCycle { peak_s: floor }) else {
            return false;
        };
        if uncertain {
            return false;
        }
        let lower = cycle.peak_s;
        let held = self.telemetry.latency_floor_s;
        let since = self.floor_read_at.map_or(0.0, |at| (t - at).max(0.0));
        self.floor_read_at = Some(t);
        let relaxed = held - (held - lower) * (since / FLOOR_HOLD_S).min(1.0);
        self.telemetry.latency_floor_s = lower.max(relaxed);

        let margin = self.config.floor_margin_s;
        if let Some((level, until)) = self.raised_level {
            if t > until {
                self.raised_level = None;
            } else if lower > level && lower - level <= dt {
                // Part of the pattern the raise had not seen yet: the level
                // follows, and the servo steers to the difference.
                self.raised_level = Some((lower, until));
                self.target_s = self.target_s.max(lower + dt + margin);
                self.telemetry.target_s = self.target_s;
            }
        }
        if lower <= self.target_s {
            return false;
        }
        self.count_over_target(t, dt, lower)
    }

    /// One more cycle above the target, at `lower`: [`FLOOR_CYCLES`] of them
    /// within [`FLOOR_WINDOW_S`] raise the target. Returns whether it was.
    fn count_over_target(&mut self, t: f64, dt: f64, lower: f64) -> bool {
        let margin = self.config.floor_margin_s;
        // Keep the latest cycles above the target that are recent enough.
        let mut kept = 0;
        for i in 0..self.over_target_len {
            if t - self.over_target[i].0 <= FLOOR_WINDOW_S {
                self.over_target[kept] = self.over_target[i];
                kept += 1;
            }
        }
        if kept == FLOOR_CYCLES {
            self.over_target.copy_within(1.., 0);
            kept -= 1;
        }
        self.over_target[kept] = (t, lower);
        self.over_target_len = kept + 1;
        if self.over_target_len < FLOOR_CYCLES {
            return false;
        }
        let mut floors = self.over_target.map(|c| c.1);
        floors.sort_by(f64::total_cmp);
        let (median, highest) = (floors[FLOOR_CYCLES / 2], floors[FLOOR_CYCLES - 1]);
        let level = if highest - median <= dt {
            highest
        } else {
            median
        };
        self.target_s = level + dt + margin;
        self.telemetry.target_s = self.target_s;
        self.over_target_len = 0;
        self.raised_level = Some((level, t + FLOOR_SETTLE_S));
        true
    }

    /// Forget what the burst cycles showed: another epoch or another stream.
    fn forget_floor_cycles(&mut self) {
        self.cycle = None;
        self.over_target_len = 0;
        self.raised_level = None;
    }

    /// Take in the capture point's reading. Returns whether it started a new
    /// source phase.
    fn observe_source(&mut self, input: &CallbackInput) -> bool {
        let Some(obs) = input.source else {
            return false;
        };
        let fresh = self.last_source_t.is_none_or(|last| obs.t > last);
        if fresh {
            let resumed = self
                .last_source_t
                .is_some_and(|last| obs.t - last > self.config.source_gap_s);
            let asked = std::mem::take(&mut self.restart_next);
            let after_underrun = std::mem::take(&mut self.restart_after_underrun);
            // An exact source read on its line after an underrun lost
            // nothing: the decoder held more than the target leaves room
            // for, and the phase stands. (A `follow` source's readings are
            // late by their jitter: nothing can be told from one of them.)
            let off_line = self
                .source
                .position_at(obs.t)
                .map(|line| (line - obs.received).abs() / self.config.source_rate_hz);
            let lost_nothing = after_underrun
                && !self.config.source_late_arrivals
                && off_line.is_some_and(|off| off <= SOURCE_LOSS_TOLERANCE_S);
            // A `follow` source still restarts its phase (one late reading
            // proves nothing about the line), but a reading this close to
            // it says the source had not stopped.
            let underrun_floor = self.underrun_floor.take();
            if after_underrun
                && self.config.source_late_arrivals
                && off_line.is_some_and(|off| off <= FOLLOW_LOSS_TOLERANCE_S)
            {
                self.pipeline_underrun = underrun_floor;
            }
            if (asked && !lost_nothing) || (resumed && self.source.is_tracking()) {
                // The source stopped and came back, lost time, or started
                // another stream: same clock, new phase.
                self.source.restart_phase(obs.t, obs.received);
                self.restarts += 1;
                self.last_source_t = Some(obs.t);
                self.source_stalled = false;
                return true;
            }
            self.source.observe(obs.t, obs.received);
            self.last_source_t = Some(obs.t);
            self.source_stalled = false;
        } else if self
            .last_source_t
            .is_some_and(|last| input.t - last > self.config.source_gap_s)
        {
            self.source_stalled = true;
        }
        false
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
        self.underrun_floor = Some(self.last_floor);
        // The source's line may be ahead of what has arrived: it lost time
        // (a pause, a stop shorter than the gap that marks a stall), and
        // nothing else says so. The next reading starts a new phase, unless
        // it shows an exact source still on its line (`observe_source`).
        self.restart_next = true;
        self.restart_after_underrun = true;
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
            source_breaks: 0,
        }
    }

    /// A source with no clock: the device is the master. Playback starts as
    /// soon as the ring holds a callback's worth, runs at the nominal ratio
    /// whatever the readings say, and a dry ring is a silent callback and a
    /// counted underrun, not a realign.
    #[test]
    fn a_device_paced_source_plays_what_the_ring_holds() {
        let mut servo = Servo::new(ServoConfig {
            pacing: Pacing::Device,
            ..ServoConfig::default()
        });
        let frames = 1024.0;
        let cb = |t: f64, play: f64, available: f64| CallbackInput {
            source: None,
            ..input(t, play, available)
        };
        // Not a callback's worth yet: silence, still starting.
        let plan = servo.plan(&cb(0.0, 0.0, 512.0));
        assert_eq!(plan.silence_frames, 1024);
        assert_eq!(servo.phase(), Phase::Starting);
        assert_eq!(servo.telemetry().underruns, 0);

        // Enough: played from the read front, at the nominal ratio, faded in.
        let plan = servo.plan(&cb(0.021, 0.0, 4096.0));
        assert_eq!(plan.silence_frames, 0);
        assert_eq!(plan.skip_source_frames, 0.0);
        assert_eq!(plan.ratio, 1.0);
        assert!(plan.fade_in_frames > 0);
        assert_eq!(servo.phase(), Phase::Running);
        // The latency read is what is buffered ahead, plus the device.
        let latency = servo.telemetry().latency_s.unwrap();
        assert!((latency - (0.032 + 4096.0 / 48_000.0)).abs() < 1e-9);

        // Steady: no fade, the ratio does not move however full the ring is.
        let plan = servo.plan(&cb(0.042, frames, 48_000.0));
        assert_eq!((plan.ratio, plan.fade_in_frames), (1.0, 0));
        assert_eq!(servo.telemetry().correction_ppm, 0.0);

        // The ring ran dry: one silent callback, counted, then it resumes.
        let plan = servo.plan(&cb(0.063, 2.0 * frames, 2.0 * frames + 100.0));
        assert_eq!(plan.silence_frames, 1024);
        assert_eq!(servo.telemetry().underruns, 1);
        assert_eq!(servo.telemetry().realigns, 0);
        let plan = servo.plan(&cb(0.084, 2.0 * frames, 4.0 * frames));
        assert_eq!(plan.silence_frames, 0);
        assert!(plan.fade_in_frames > 0);
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

    /// A track change: the capture side reports a break, and the new stream's
    /// readings sit 7 ms off the old line, under any jitter threshold. The
    /// servo restarts the phase on the new stream's first reading and sets
    /// the latency on it, instead of steering the 7 ms out over tens of
    /// seconds with a fit that spans the step.
    #[test]
    fn a_reported_break_restarts_the_phase_and_realigns() {
        let mut servo = Servo::new(ServoConfig::follow());
        let frames = 1024usize;
        let dt = frames as f64 / 48_000.0;
        let (break_at, step) = (30.0, 0.007 * 48_000.0);
        let mut play = 0.0;
        for k in 0..(40.0 / dt) as usize {
            let t = k as f64 * dt;
            let after = t >= break_at;
            let received = t * 48_000.0 + if after { step } else { 0.0 };
            let mut inp = input(t, play, received);
            inp.source_breaks = after as u64;
            let plan = servo.plan(&inp);
            let played = frames - plan.silence_frames.min(frames);
            play += plan.skip_source_frames + plan.ratio * played as f64;
            if t > break_at + 0.5 {
                assert_eq!(servo.phase(), Phase::Running, "at t = {t:.2}");
                let error = servo.telemetry().error_s;
                assert!(error.abs() < 1e-4, "error {error} at t = {t:.2}");
            }
        }
        assert_eq!(servo.telemetry().realigns, 1);
        assert_eq!(servo.telemetry().source_rephases, 1);
        assert!(servo.telemetry().source_ppm.abs() < 1.0);
    }
}
