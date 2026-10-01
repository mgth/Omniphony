//! How the servo estimates the source's position and rate from the capture
//! point's readings.
//!
//! - **`own`** (orender clocks the source): the readings are exact, so a
//!   [`Dll`] filters the timestamps and learns the rate.
//! - **`follow`** (the source has its own clock and its bytes arrive late by
//!   a one-sided jitter, ±20 ms from mpv into a pipe): a [`FollowEstimator`].

use crate::dll::{Dll, DllConfig};
use crate::envelope::ArrivalEnvelope;

#[derive(Debug, Clone)]
pub(crate) enum SourceEstimator {
    Dll(Dll),
    /// Boxed: its fixed buffers are ~40 KiB; allocated once, here.
    Follow(Box<FollowEstimator>),
}

impl SourceEstimator {
    pub(crate) fn new(dll: DllConfig, follow: bool, nominal_rate: f64) -> Self {
        if follow {
            Self::Follow(Box::new(FollowEstimator::new(nominal_rate)))
        } else {
            Self::Dll(Dll::new(dll, nominal_rate))
        }
    }

    pub(crate) fn reset(&mut self) {
        match self {
            Self::Dll(d) => d.reset(),
            Self::Follow(f) => f.reset(),
        }
    }

    pub(crate) fn is_tracking(&self) -> bool {
        match self {
            Self::Dll(d) => d.is_tracking(),
            Self::Follow(f) => f.offset.is_some(),
        }
    }

    pub(crate) fn observe(&mut self, t: f64, received: f64) {
        match self {
            Self::Dll(d) => d.observe(t, received),
            Self::Follow(f) => f.observe(t, received),
        }
    }

    /// The source came back after a gap: same clock, new phase. The `follow`
    /// estimator keeps its rate and starts its fits over.
    pub(crate) fn restart_phase(&mut self, t: f64, received: f64) {
        match self {
            Self::Dll(d) => d.restart_phase(t, received),
            Self::Follow(f) => {
                f.rephase_before(f64::INFINITY);
                f.observe(t, received);
            }
        }
    }

    /// Phase breaks the `follow` estimator detected in its readings.
    pub(crate) fn detected_breaks(&self) -> u64 {
        match self {
            Self::Dll(_) => 0,
            Self::Follow(f) => f.detected_breaks,
        }
    }

    pub(crate) fn rate(&self) -> Option<f64> {
        match self {
            Self::Dll(d) => d.rate(),
            Self::Follow(f) => f.offset.map(|_| f.rate),
        }
    }

    pub(crate) fn position_at(&self, t: f64) -> Option<f64> {
        match self {
            Self::Dll(d) => d.position_at(t),
            Self::Follow(f) => f.position_at(t),
        }
    }
}

/// Window of the slope fit (s).
pub const FOLLOW_SLOPE_WINDOW_S: f64 = 120.0;
/// Window of the offset (s).
pub const FOLLOW_OFFSET_WINDOW_S: f64 = 30.0;
/// Steady-state time constant of the rate low-pass (s).
pub const FOLLOW_RATE_TAU_S: f64 = 10.0;
/// While the slope fit is young, its rate low-pass time constant is its span
/// divided by this (it is followed closely, then only through the steady
/// constant).
const RATE_TAU_SPAN_DIV: f64 = 4.0;
/// Span below which the slope fit is not used yet (s).
const MIN_SLOPE_SPAN_S: f64 = 5.0;
/// Time constant with which the offset relaxes downwards (s); it rises at
/// once. Slow on purpose: when the window's earliest reading leaves it, the
/// next earliest can sit a few tenths of a ms lower, and following that at
/// once is a phase step the loop would turn into ratio wander.
pub const FOLLOW_OFFSET_TAU_S: f64 = 15.0;
/// A reading this far off the line (s) is not jitter: ahead of it, the
/// source jumped forward; behind it for [`PHASE_BREAK_HOLD_S`], it lost time.
/// Above the untimed mpv writer's steady lateness runs (≤ 84 ms beyond
/// 20 ms, measured) and far above a timed writer's (0.03 ms p99).
pub const PHASE_BREAK_S: f64 = 0.020;
/// How long readings must stay [`PHASE_BREAK_S`] behind the line before the
/// source is taken to have lost time (s).
pub const PHASE_BREAK_HOLD_S: f64 = 0.5;
/// Readings kept for the offset: one per output callback (~47/s at a 1024
/// quantum) over the offset window, with margin.
const OFFSET_READINGS: usize = 2048;

/// Position and rate of a source whose readings arrive late by a one-sided
/// jitter.
///
/// Each reading `(t, received)` lies on or below the source's true line. The
/// two things to estimate are best estimated over different spans:
///
/// - **The rate** is the slope of the readings' upper convex hull over a long
///   window ([`FOLLOW_SLOPE_WINDOW_S`], see [`ArrivalEnvelope`]), low-passed
///   ([`FOLLOW_RATE_TAU_S`]) so the hull's edge switches do not reach the
///   ratio. A rate changes slowly; a long window makes its error small.
/// - **The offset** is the earliest arrival of the recent readings
///   ([`FOLLOW_OFFSET_WINDOW_S`]) against a line at that rate: the largest
///   `received − rate·t`. It needs no extrapolation, and its error is the
///   smallest lateness among the window's readings, whose p99 falls as
///   `jitter·ln(100)/N` (0.25 ms at 40 ms of jitter needs ~30 s of video
///   frames). It rises at once and relaxes down over [`FOLLOW_OFFSET_TAU_S`],
///   so an earliest reading leaving the window is not a step.
///
/// **Phase breaks.** A player that pauses or seeks keeps its clock but not
/// its phase: its readings step off the line. A step ahead (more than
/// [`PHASE_BREAK_S`]) moves the offset at once, as any earlier arrival does,
/// and restarts the slope fit, whose hull would otherwise span the step.
/// Readings that stay that far behind for [`PHASE_BREAK_HOLD_S`] start a new
/// phase from the first of them: the older readings and the slope fit are
/// dropped, the rate is kept. (The servo also restarts the phase itself when
/// it knows better: after an underrun, or when the capture side reports a
/// new stream.)
///
/// Open-loop study on the measured mpv model (±20 ms one-sided, 23.976 fps
/// bursts), after 100 s: position p99 within 0.33–0.47 ms and rate within
/// 7 ppm peak-to-peak, at 0, ±80, +300 and ±1000 ppm. The previous estimator
/// (the hull edge extrapolated, then a DLL) gave 0.5–1.9 ms and 27–86 ppm.
#[derive(Debug, Clone)]
pub(crate) struct FollowEstimator {
    nominal_rate: f64,
    slope: ArrivalEnvelope,
    /// Recent readings `(t − t_ref, received)`, in arrival order.
    readings: [(f64, f64); OFFSET_READINGS],
    head: usize,
    len: usize,
    t_ref: Option<f64>,
    t_last: Option<f64>,
    rate: f64,
    offset: Option<f64>,
    /// Arrival time of the first of the readings that have stayed behind the
    /// line by more than [`PHASE_BREAK_S`].
    behind_since: Option<f64>,
    detected_breaks: u64,
}

impl FollowEstimator {
    fn new(nominal_rate: f64) -> Self {
        Self {
            nominal_rate,
            slope: ArrivalEnvelope::new(nominal_rate, FOLLOW_SLOPE_WINDOW_S),
            readings: [(0.0, 0.0); OFFSET_READINGS],
            head: 0,
            len: 0,
            t_ref: None,
            t_last: None,
            rate: nominal_rate,
            offset: None,
            behind_since: None,
            detected_breaks: 0,
        }
    }

    fn reset(&mut self) {
        self.slope.reset();
        self.head = 0;
        self.len = 0;
        self.t_ref = None;
        self.t_last = None;
        self.rate = self.nominal_rate;
        self.offset = None;
        self.behind_since = None;
    }

    /// Start a new phase from the readings that arrived at or after `t`
    /// (none for infinity): drop the older ones and the slope fit, keep the
    /// rate.
    fn rephase_before(&mut self, t: f64) {
        self.slope.reset();
        if let Some(t_ref) = self.t_ref {
            while self.len > 0 && self.readings[self.head].0 + t_ref < t {
                self.head = (self.head + 1) % OFFSET_READINGS;
                self.len -= 1;
            }
        }
        if self.len == 0 {
            self.t_ref = None;
            self.t_last = None;
        }
        self.offset = None;
        self.behind_since = None;
    }

    /// Detect a phase break against the current line before taking the
    /// reading `(t, received)`.
    fn check_phase(&mut self, t: f64, received: f64) {
        let Some(line) = self.position_at(t) else {
            return;
        };
        let late_s = (line - received) / self.rate;
        if late_s < -PHASE_BREAK_S {
            self.slope.reset();
            self.behind_since = None;
        } else if late_s > PHASE_BREAK_S {
            let since = *self.behind_since.get_or_insert(t);
            if t - since >= PHASE_BREAK_HOLD_S {
                self.rephase_before(since);
                self.detected_breaks += 1;
            }
        } else {
            self.behind_since = None;
        }
    }

    fn observe(&mut self, t: f64, received: f64) {
        if self.t_last.is_some_and(|last| t <= last) {
            return;
        }
        self.check_phase(t, received);
        let dt = self.t_last.map_or(0.0, |last| t - last);
        self.slope.observe(t, received);
        let t_ref = *self.t_ref.get_or_insert(t);
        let x = t - t_ref;
        if self.len == OFFSET_READINGS {
            self.head = (self.head + 1) % OFFSET_READINGS;
            self.len -= 1;
        }
        self.readings[(self.head + self.len) % OFFSET_READINGS] = (x, received);
        self.len += 1;
        while self.len > 1 && x - self.readings[self.head].0 > FOLLOW_OFFSET_WINDOW_S {
            self.head = (self.head + 1) % OFFSET_READINGS;
            self.len -= 1;
        }

        let span = self.slope.span_s();
        if span >= MIN_SLOPE_SPAN_S
            && let Some(fitted) = self.slope.rate()
        {
            let tau = FOLLOW_RATE_TAU_S.min((span / RATE_TAU_SPAN_DIV).max(0.5));
            self.rate += (fitted - self.rate) * (1.0 - (-dt / tau).exp());
        }

        let rate = self.rate;
        let now = (0..self.len)
            .map(|i| {
                let (xi, ri) = self.readings[(self.head + i) % OFFSET_READINGS];
                ri - rate * xi
            })
            .fold(f64::NEG_INFINITY, f64::max);
        self.offset = Some(match self.offset {
            Some(o) if now <= o => o + (now - o) * (1.0 - (-dt / FOLLOW_OFFSET_TAU_S).exp()),
            _ => now,
        });
        self.t_last = Some(t);
    }

    fn position_at(&self, t: f64) -> Option<f64> {
        Some(self.offset? + self.rate * (t - self.t_ref?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Readings from a 23.976 fps source, each arriving late by a
    /// pseudo-random 0–40 ms: after 100 s the estimate is within half a
    /// millisecond of the earliest-arrival line and the rate within a few ppm,
    /// whatever the drift.
    #[test]
    fn follows_a_late_source_at_any_drift() {
        let nominal = 48_000.0;
        for ppm in [0.0, 80.0, 1000.0, -1000.0] {
            let rate = nominal * (1.0 + ppm * 1e-6);
            let mut est = FollowEstimator::new(nominal);
            let per_frame = nominal * 1001.0 / 24_000.0;
            let (mut worst_pos, mut worst_ppm) = (0.0f64, 0.0f64);
            for j in 0..(24 * 200u64) {
                let t_true = j as f64 * per_frame / rate;
                let x = (j.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 11) as f64 / (1u64 << 53) as f64;
                let late = 0.040 * x;
                est.observe(t_true + late, j as f64 * per_frame);
                if t_true > 100.0 {
                    let now = t_true + late;
                    let err = est.position_at(now).unwrap() - now * rate;
                    worst_pos = worst_pos.max(err.abs());
                    let r = (est.rate / rate - 1.0) * 1e6;
                    worst_ppm = worst_ppm.max(r.abs());
                }
            }
            assert!(
                worst_pos < 48.0,
                "{ppm} ppm: position off by {worst_pos} frames"
            );
            assert!(worst_ppm < 10.0, "{ppm} ppm: rate off by {worst_ppm} ppm");
        }
    }

    /// A source read every 10 ms without jitter, at `ppm` off nominal, from
    /// `t0` to `t1`, its count offset by `shift` frames.
    fn feed(est: &mut FollowEstimator, ppm: f64, t0: f64, t1: f64, shift: f64) {
        let rate = 48_000.0 * (1.0 + ppm * 1e-6);
        let mut t = t0;
        while t < t1 {
            est.observe(t, t * rate + shift);
            t += 0.010;
        }
    }

    fn line_error(est: &FollowEstimator, ppm: f64, t: f64, shift: f64) -> f64 {
        est.position_at(t).unwrap() - (t * 48_000.0 * (1.0 + ppm * 1e-6) + shift)
    }

    /// A pause: the source stops for 0.2 s and goes on from where it was.
    /// Its readings stay behind the old line, so after the hold the estimate
    /// starts a new phase on them, with the rate it had.
    #[test]
    fn a_source_that_loses_time_starts_a_new_phase() {
        let mut est = FollowEstimator::new(48_000.0);
        feed(&mut est, 80.0, 0.0, 60.0, 0.0);
        let lost = -0.2 * 48_000.0;
        feed(&mut est, 80.0, 60.2, 60.2 + PHASE_BREAK_HOLD_S + 0.1, lost);
        assert_eq!(est.detected_breaks, 1);
        let t = 60.2 + PHASE_BREAK_HOLD_S + 0.1;
        assert!(line_error(&est, 80.0, t, lost).abs() < 1.0);
        assert!(((est.rate / 48_000.0 - 1.0) * 1e6 - 80.0).abs() < 2.0);
    }

    /// A jump forward: the offset follows at once, and the slope fit, which
    /// would otherwise span the step, starts over without the rate moving.
    #[test]
    fn a_step_ahead_moves_the_offset_and_spares_the_rate() {
        let mut est = FollowEstimator::new(48_000.0);
        feed(&mut est, -300.0, 0.0, 60.0, 0.0);
        let jump = 0.030 * 48_000.0;
        feed(&mut est, -300.0, 60.0, 90.0, jump);
        assert!(line_error(&est, -300.0, 90.0, jump).abs() < 1.0);
        assert!(((est.rate / 48_000.0 - 1.0) * 1e6 + 300.0).abs() < 2.0);
        assert_eq!(est.detected_breaks, 0);
    }

    /// Jitter-sized steps are not breaks.
    #[test]
    fn jitter_is_not_a_phase_break() {
        let mut est = FollowEstimator::new(48_000.0);
        for j in 0..(24 * 120u64) {
            let t_true = j as f64 / 24.0;
            let x = (j.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 11) as f64 / (1u64 << 53) as f64;
            est.observe(t_true + 0.040 * x, t_true * 48_000.0);
        }
        assert_eq!(est.detected_breaks, 0);
    }

    /// A restart takes its reading as the new phase at once, rate kept.
    #[test]
    fn a_restart_starts_the_new_phase_on_its_reading() {
        let mut source = SourceEstimator::new(DllConfig::default(), true, 48_000.0);
        for k in 0..3000 {
            let t = k as f64 * 0.010;
            source.observe(t, t * 48_000.0);
        }
        // Behind the line by 0.1 s, well inside the hold.
        source.restart_phase(30.1, 30.0 * 48_000.0);
        assert_eq!(source.position_at(30.1), Some(30.0 * 48_000.0));
        assert_eq!(source.rate(), Some(48_000.0));
    }
}
