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
                let rate = f.rate;
                f.reset();
                f.rate = rate;
                f.observe(t, received);
            }
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
    }

    fn observe(&mut self, t: f64, received: f64) {
        let dt = match self.t_last {
            Some(last) if t <= last => return,
            Some(last) => t - last,
            None => 0.0,
        };
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
}
