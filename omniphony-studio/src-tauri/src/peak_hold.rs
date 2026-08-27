//! Peak-hold for the level meters.
//!
//! The bar follows the instantaneous peak; a cursor holds the loudest recent
//! value so a transient stays readable after it has passed. The frontend had
//! two copies of this — one for object and speaker meters, one inlined for the
//! master meter — each keeping its own hold state per meter.
//!
//! # Decay is per-second here, not per-frame
//!
//! The JS decayed by a fixed 2 dB **each time it repainted**, so the fall rate
//! was whatever the UI render loop happened to be running at, and a browser
//! that throttled the tab slowed the meters down with it. This decays by
//! elapsed time instead. [`DECAY_DB_PER_SEC`] is set so the two agree at the
//! 60 Hz the loop normally runs at; away from that they deliberately differ,
//! because tying a physical fall rate to repaint cadence was the bug.
//!
//! # The re-arm
//!
//! Decay does not simply run to the floor. Once the held value falls back to
//! the bar, the hold re-arms for another second — otherwise a cursor sitting on
//! a sustained level would keep sliding down through it. The comparison is made
//! in *bar percent*, not dB, because that is where the frontend made it and the
//! scale clamps at both ends: two different dB values can share a percent, and
//! at the rails they do.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Bottom of the meter scale, in dBFS (`METER_DB_MIN` in `src/mute-solo.js`).
pub const METER_DB_MIN: f64 = -60.0;
/// Top of the meter scale, in dBFS (`METER_DB_MAX` in `src/mute-solo.js`).
pub const METER_DB_MAX: f64 = 6.0;

/// How long the cursor sits still before it starts falling.
const HOLD: Duration = Duration::from_millis(1000);

/// Fall rate once the hold expires.
///
/// The frontend used 2 dB per repaint at ~60 Hz; 2 × 60 = 120 dB/s is the same
/// slope expressed in a unit that does not depend on the render loop.
const DECAY_DB_PER_SEC: f64 = 120.0;

/// How close, in bar percent, the held value must come to the bar to re-arm the
/// hold. Mirrors the frontend's `peak.value <= levelPercent + 0.1`.
const REARM_PERCENT_EPSILON: f64 = 0.1;

/// Position of a level on the meter bar, 0-100.
///
/// Mirrors `dbToMeterPercent` in `src/mute-solo.js`: linear in dB between the
/// scale's ends, clamped at both. 0 dBFS lands near 90.9%, leaving the headroom
/// zone above it where clipping stays visible.
pub fn db_to_meter_percent(db: f64) -> f64 {
    let v = if db.is_finite() { db } else { METER_DB_MIN };
    (((v - METER_DB_MIN) / (METER_DB_MAX - METER_DB_MIN)) * 100.0).clamp(0.0, 100.0)
}

#[derive(Clone, Copy)]
struct Hold {
    db: f64,
    /// When the cursor may start falling.
    falls_after: Instant,
    /// Last time this hold was advanced, so decay can be time-based.
    updated_at: Instant,
}

/// Per-meter hold state, keyed by the caller's meter id.
#[derive(Default)]
pub struct PeakHolds {
    holds: HashMap<String, Hold>,
}

impl PeakHolds {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold in a new instantaneous peak and return the value the cursor should
    /// now show, in dBFS.
    ///
    /// `now` is passed rather than read so the behaviour is testable; callers
    /// use [`Instant::now`].
    pub fn update(&mut self, key: &str, peak_db: f64, now: Instant) -> f64 {
        let peak_db = if peak_db.is_finite() {
            peak_db
        } else {
            METER_DB_MIN
        };

        let hold = match self.holds.get(key).copied() {
            // A new peak at or above the held one re-arms from scratch. `>=`
            // rather than `>` matches the frontend, and means a sustained level
            // keeps the cursor pinned instead of letting it creep down.
            Some(h) if peak_db < h.db => h,
            _ => {
                let fresh = Hold {
                    db: peak_db,
                    falls_after: now + HOLD,
                    updated_at: now,
                };
                self.holds.insert(key.to_string(), fresh);
                return fresh.db;
            }
        };

        if now <= hold.falls_after {
            // Still holding: the cursor does not move, but the clock does, so
            // the first decay step after the hold measures from here.
            let held = Hold {
                updated_at: now,
                ..hold
            };
            self.holds.insert(key.to_string(), held);
            return held.db;
        }

        let elapsed = now.saturating_duration_since(hold.updated_at).as_secs_f64();
        let decayed = (hold.db - DECAY_DB_PER_SEC * elapsed).max(peak_db);
        let mut next = Hold {
            db: decayed,
            falls_after: hold.falls_after,
            updated_at: now,
        };
        // Landed back on the bar: hold again rather than sliding through it.
        if db_to_meter_percent(decayed) <= db_to_meter_percent(peak_db) + REARM_PERCENT_EPSILON {
            next.falls_after = now + HOLD;
        }
        self.holds.insert(key.to_string(), next);
        next.db
    }

    /// Forget a meter, so a removed object does not leave state behind.
    pub fn forget(&mut self, key: &str) {
        self.holds.remove(key);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.holds.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn the_first_peak_is_held_as_is() {
        let mut holds = PeakHolds::new();
        let t0 = Instant::now();
        assert_eq!(holds.update("a", -12.0, t0), -12.0);
    }

    #[test]
    fn a_louder_peak_takes_over_immediately() {
        let mut holds = PeakHolds::new();
        let t0 = Instant::now();
        holds.update("a", -30.0, t0);
        assert_eq!(holds.update("a", -6.0, at(t0, 10)), -6.0);
    }

    #[test]
    fn a_quieter_peak_does_not_move_the_cursor_during_the_hold() {
        let mut holds = PeakHolds::new();
        let t0 = Instant::now();
        holds.update("a", -6.0, t0);
        // Well inside the 1 s hold.
        assert_eq!(holds.update("a", -40.0, at(t0, 500)), -6.0);
        assert_eq!(holds.update("a", -40.0, at(t0, 999)), -6.0);
    }

    #[test]
    fn the_cursor_falls_once_the_hold_expires() {
        let mut holds = PeakHolds::new();
        let t0 = Instant::now();
        holds.update("a", -6.0, t0);
        holds.update("a", -40.0, at(t0, 1000));
        // 100 ms past the hold at 120 dB/s would be 12 dB, but the value is
        // measured from the previous update, which was at t=1000.
        let value = holds.update("a", -40.0, at(t0, 1100));
        assert!(
            (value - (-18.0)).abs() < 1e-6,
            "expected -18 after 100 ms of decay, got {value}"
        );
    }

    /// The frontend fell 2 dB per repaint at ~60 Hz. One 16.67 ms step here
    /// must move the cursor by the same 2 dB, or every meter in the app decays
    /// at a visibly different rate than before.
    #[test]
    fn one_frame_of_decay_matches_the_frontend_step() {
        let mut holds = PeakHolds::new();
        let t0 = Instant::now();
        holds.update("a", 0.0, t0);
        holds.update("a", -60.0, at(t0, 1000));
        let after_one_frame = holds.update("a", -60.0, at(t0, 1000 + 17));
        // 17 ms at 120 dB/s = 2.04 dB.
        assert!(
            (after_one_frame - (-2.04)).abs() < 0.01,
            "one frame moved the cursor to {after_one_frame}, expected about -2.04"
        );
    }

    #[test]
    fn the_cursor_never_falls_below_the_current_peak() {
        let mut holds = PeakHolds::new();
        let t0 = Instant::now();
        holds.update("a", 0.0, t0);
        holds.update("a", -20.0, at(t0, 1000));
        // A full second of decay would be 120 dB; the live peak stops it.
        let value = holds.update("a", -20.0, at(t0, 2000));
        assert_eq!(value, -20.0);
    }

    /// Once the cursor lands on the bar the hold re-arms, so a sustained level
    /// keeps its cursor instead of letting it slide away.
    #[test]
    fn landing_on_the_bar_re_arms_the_hold() {
        let mut holds = PeakHolds::new();
        let t0 = Instant::now();
        holds.update("a", 0.0, t0);
        // Decay all the way down to the sustained level.
        holds.update("a", -20.0, at(t0, 1000));
        let landed = holds.update("a", -20.0, at(t0, 2000));
        assert_eq!(landed, -20.0);
        // Re-armed: a further second must not move it, because the hold
        // restarted when it landed.
        let still = holds.update("a", -20.0, at(t0, 2500));
        assert_eq!(still, -20.0);
    }

    #[test]
    fn holds_are_independent_per_meter() {
        let mut holds = PeakHolds::new();
        let t0 = Instant::now();
        holds.update("a", -6.0, t0);
        holds.update("b", -40.0, t0);
        assert_eq!(holds.update("a", -50.0, at(t0, 100)), -6.0);
        assert_eq!(holds.update("b", -50.0, at(t0, 100)), -40.0);
    }

    #[test]
    fn a_non_finite_peak_reads_as_the_floor() {
        let mut holds = PeakHolds::new();
        let t0 = Instant::now();
        assert_eq!(holds.update("a", f64::NAN, t0), METER_DB_MIN);
    }

    #[test]
    fn forgetting_a_meter_drops_its_state() {
        let mut holds = PeakHolds::new();
        let t0 = Instant::now();
        holds.update("a", -6.0, t0);
        assert_eq!(holds.len(), 1);
        holds.forget("a");
        assert_eq!(holds.len(), 0);
        // And it starts clean rather than resuming the old hold.
        assert_eq!(holds.update("a", -40.0, at(t0, 10)), -40.0);
    }

    // ── the percent mapping the re-arm depends on ───────────────────────────

    #[test]
    fn the_meter_scale_matches_the_frontend() {
        assert_eq!(db_to_meter_percent(METER_DB_MIN), 0.0);
        assert_eq!(db_to_meter_percent(METER_DB_MAX), 100.0);
        // 0 dBFS sits near 90.9%, leaving the headroom zone above it.
        assert!((db_to_meter_percent(0.0) - 90.909).abs() < 0.01);
    }

    #[test]
    fn the_meter_scale_clamps_at_both_ends() {
        assert_eq!(db_to_meter_percent(-200.0), 0.0);
        assert_eq!(db_to_meter_percent(60.0), 100.0);
        assert_eq!(db_to_meter_percent(f64::NAN), 0.0);
    }

    /// Both ends of the scale clamp, so two different dB values share a
    /// percent there. That is why the re-arm compares percent and not dB.
    #[test]
    fn the_re_arm_triggers_at_the_rail_where_db_would_still_differ() {
        let mut holds = PeakHolds::new();
        let t0 = Instant::now();
        holds.update("a", -50.0, t0);
        // Decay past the bottom of the scale: -70 and -200 are different dB
        // values but both read as 0%.
        let value = holds.update("a", -200.0, at(t0, 2000));
        assert!(value <= METER_DB_MIN, "cursor was {value}");
        assert_eq!(db_to_meter_percent(value), 0.0);
    }
}
