//! Second-order delay-locked loop on a frame counter.
//!
//! A DLL tracks a counter `p(t)` that advances at a nearly constant rate, from
//! observations `(t, p)` that may be jittered, irregular and bursty (a device
//! position read once per callback, a source that delivers a video frame's
//! worth of audio at a time). It keeps an estimate of the position at the last
//! observation and of the rate, and corrects both from the prediction error
//! with the classic critically-damped gains (`√2·ωΔt`, `(ωΔt)²`, see
//! F. Adriaensen, "Using a DLL to filter time", 2005), recomputed from the
//! actual interval so irregular observations are handled.
//!
//! The bandwidth starts wide and narrows geometrically to its target, so the
//! loop locks within seconds and then filters hard: at 0.01 Hz the rate of a
//! real PipeWire device settles to well under 1 ppm rms (spike S2).

use std::f64::consts::{SQRT_2, TAU};

/// Bandwidth schedule of a [`Dll`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DllConfig {
    /// Steady-state loop bandwidth (Hz).
    pub bandwidth_hz: f64,
    /// Bandwidth right after a (re)start (Hz).
    pub start_bandwidth_hz: f64,
    /// Time for the bandwidth to halve while narrowing (s).
    pub narrowing_half_life_s: f64,
}

impl Default for DllConfig {
    fn default() -> Self {
        Self {
            bandwidth_hz: 0.01,
            start_bandwidth_hz: 1.0,
            narrowing_half_life_s: 1.5,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct State {
    /// Time of the (re)start, for the bandwidth schedule.
    started_at: f64,
    /// Time of the last observation.
    t: f64,
    /// Filtered position at `t`.
    position: f64,
    /// Filtered rate (frames per second of the reference clock).
    rate: f64,
}

/// See the [module docs](self).
#[derive(Debug, Clone)]
pub struct Dll {
    config: DllConfig,
    nominal_rate: f64,
    state: Option<State>,
}

/// Largest `ωΔt` the update accepts. The gains assume `ωΔt ≪ 1`; a long gap
/// between observations at a wide bandwidth would otherwise overshoot.
const MAX_OMEGA_DT: f64 = 0.5;

impl Dll {
    /// A loop that has seen nothing yet, assuming `nominal_rate` until it has.
    pub fn new(config: DllConfig, nominal_rate: f64) -> Self {
        Self {
            config,
            nominal_rate,
            state: None,
        }
    }

    /// Forget everything, including the rate.
    pub fn reset(&mut self) {
        self.state = None;
    }

    /// Restart the phase at `(t, position)` but keep the rate learnt so far:
    /// for a counter that jumped (a source that paused and resumed) on a clock
    /// that did not change.
    pub fn restart_phase(&mut self, t: f64, position: f64) {
        let rate = self.rate().unwrap_or(self.nominal_rate);
        self.state = Some(State {
            started_at: t,
            t,
            position,
            rate,
        });
    }

    /// Whether any observation has been taken since the last reset.
    pub fn is_tracking(&self) -> bool {
        self.state.is_some()
    }

    /// Feed one observation. Observations at or before the previous one are
    /// ignored: the loop only moves forward in time.
    pub fn observe(&mut self, t: f64, position: f64) {
        let Some(state) = self.state.as_mut() else {
            self.state = Some(State {
                started_at: t,
                t,
                position,
                rate: self.nominal_rate,
            });
            return;
        };
        let dt = t - state.t;
        if dt <= 0.0 {
            return;
        }
        let bandwidth = scheduled_bandwidth(&self.config, t - state.started_at);
        let omega_dt = (TAU * bandwidth * dt).min(MAX_OMEGA_DT);
        let predicted = state.position + state.rate * dt;
        let error = position - predicted;
        state.position = predicted + SQRT_2 * omega_dt * error;
        state.rate += omega_dt * omega_dt * error / dt;
        state.t = t;
    }

    /// Filtered rate, once tracking.
    pub fn rate(&self) -> Option<f64> {
        self.state.map(|s| s.rate)
    }

    /// Filtered position extrapolated to `t`, once tracking.
    pub fn position_at(&self, t: f64) -> Option<f64> {
        self.state.map(|s| s.position + s.rate * (t - s.t))
    }
}

fn scheduled_bandwidth(config: &DllConfig, age: f64) -> f64 {
    let halvings = (age.max(0.0) / config.narrowing_half_life_s.max(f64::EPSILON)).min(1000.0);
    (config.start_bandwidth_hz * 0.5f64.powf(halvings)).max(config.bandwidth_hz)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 48_000.0;
    const PERIOD: f64 = 1024.0 / RATE;

    /// Feed a counter running `ppm` off nominal, observed once per period with
    /// optional timestamp jitter, and return the loop.
    fn run(ppm: f64, seconds: f64, jitter: impl Fn(usize) -> f64) -> Dll {
        let mut dll = Dll::new(DllConfig::default(), RATE);
        let true_rate = RATE * (1.0 + ppm * 1e-6);
        let steps = (seconds / PERIOD) as usize;
        for k in 0..steps {
            let t = k as f64 * PERIOD;
            dll.observe(t + jitter(k), t * true_rate);
        }
        dll
    }

    #[test]
    fn locks_onto_an_offset_rate() {
        let dll = run(-17.0, 120.0, |_| 0.0);
        let ppm = (dll.rate().unwrap() / RATE - 1.0) * 1e6;
        assert!((ppm + 17.0).abs() < 0.05, "rate settled at {ppm} ppm");
    }

    #[test]
    fn filters_timestamp_jitter() {
        // ±200 µs of deterministic pseudo-random jitter on every timestamp.
        let jitter = |k: usize| {
            let x = (k as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 11;
            ((x as f64 / (1u64 << 53) as f64) - 0.5) * 400e-6
        };
        let dll = run(50.0, 300.0, jitter);
        let ppm = (dll.rate().unwrap() / RATE - 1.0) * 1e6;
        assert!((ppm - 50.0).abs() < 1.0, "rate settled at {ppm} ppm");
    }

    #[test]
    fn extrapolates_position_between_observations() {
        let dll = run(0.0, 60.0, |_| 0.0);
        let t = 60.0 + 0.5 * PERIOD;
        let p = dll.position_at(t).unwrap();
        assert!((p - t * RATE).abs() < 0.5, "position {p} vs {}", t * RATE);
    }

    #[test]
    fn restart_phase_keeps_the_rate() {
        let mut dll = run(30.0, 120.0, |_| 0.0);
        let rate = dll.rate().unwrap();
        dll.restart_phase(500.0, 1.0);
        assert_eq!(dll.rate(), Some(rate));
        assert_eq!(dll.position_at(500.0), Some(1.0));
        dll.reset();
        assert!(!dll.is_tracking());
    }

    #[test]
    fn ignores_observations_that_do_not_move_forward() {
        let mut dll = Dll::new(DllConfig::default(), RATE);
        dll.observe(1.0, 100.0);
        dll.observe(1.0, 5_000.0);
        dll.observe(0.5, 5_000.0);
        assert_eq!(dll.position_at(1.0), Some(100.0));
    }

    #[test]
    fn bandwidth_narrows_to_its_floor() {
        let config = DllConfig::default();
        assert_eq!(scheduled_bandwidth(&config, 0.0), 1.0);
        assert!((scheduled_bandwidth(&config, 1.5) - 0.5).abs() < 1e-12);
        assert_eq!(scheduled_bandwidth(&config, 60.0), 0.01);
    }
}
