//! The two meter behaviours the host computed and the OSC stream does not
//! carry: the derived master meter and the decay ballistics
//! (`osc_listener.rs:1955–1986`, `speakers.js:2410–2461`).
//!
//! Both exist because a meter has to keep saying something true between
//! messages. A renderer that never publishes a master level still has speaker
//! levels, and a meter whose source went quiet has to fall rather than freeze
//! on its last value — a frozen meter reads as signal.

use std::time::{Duration, Instant};

use super::Tick;
use crate::host::commands::SharedState;
use crate::host::peak_hold::METER_DB_MIN;
use crate::model::app_state::Meter;

/// `METER_DECAY_START_MS`: how long a meter holds before it starts falling.
const DECAY_START: Duration = Duration::from_millis(250);
/// `METER_DECAY_DB_PER_SEC`.
const DECAY_DB_PER_SEC: f64 = 45.0;
/// The parser's floor, below the meter scale's own: a decaying meter runs off
/// the bottom of the scale rather than stopping at it.
const DECAY_FLOOR: f64 = -100.0;

/// The master level reconstructed from the speakers.
///
/// Peak is the loudest speaker's peak. RMS is a *power* sum, not an amplitude
/// one: N speakers each at the same level sum to that level, which is what a
/// master meter should read when the same signal is in every channel.
pub fn derived_master(levels: &[Meter]) -> Option<Meter> {
    if levels.is_empty() {
        return None;
    }
    let peak_dbfs = levels
        .iter()
        .map(|m| m.peak_dbfs)
        .fold(METER_DB_MIN, f64::max);
    let sum_squares: f64 = levels
        .iter()
        .map(|m| {
            let linear = 10f64.powf(m.rms_dbfs / 20.0);
            linear * linear
        })
        .sum();
    let rms_linear = (sum_squares / levels.len() as f64).sqrt();
    // Only a true underflow reaches the floor: a quiet −80 dB converts
    // normally, and snapping it up would invent signal.
    let rms_dbfs = if rms_linear > 0.0 {
        20.0 * rms_linear.log10()
    } else {
        METER_DB_MIN
    };
    Some(Meter {
        peak_dbfs,
        rms_dbfs,
    })
}

/// One meter after the decay pass that runs at `now`.
///
/// `seen` is when its source last refreshed it and `last_pass` when the
/// previous pass ran. A pass only takes off the part of the time since the
/// previous one that lies past the hold, so the passes add up: however the
/// silence is cut into frames, a meter ends 45 dB per second past its hold
/// below the value it was refreshed with, exactly as one pass covering all of
/// it would put it (`decayMeters` steps by the time between passes too).
/// Taking the whole time since the refresh off on every pass instead
/// re-applies the fall already taken, and the higher the frame rate, the
/// faster the meter drops.
pub fn decayed(
    meter: &Meter,
    seen: Option<Instant>,
    last_pass: Option<Instant>,
    now: Instant,
) -> Meter {
    // A meter nothing ever refreshed has no age to fall by.
    let Some(seen) = seen else {
        return meter.clone();
    };
    let hold_end = seen + DECAY_START;
    // Before the first pass, nothing has been taken off since the refresh.
    let from = last_pass.map_or(hold_end, |at| at.max(hold_end));
    let db = DECAY_DB_PER_SEC * now.saturating_duration_since(from).as_secs_f64();
    if db <= 0.0 {
        return meter.clone();
    }
    Meter {
        peak_dbfs: (meter.peak_dbfs - db).max(DECAY_FLOOR),
        rms_dbfs: (meter.rms_dbfs - db).max(DECAY_FLOOR),
    }
}

/// How often a decaying meter is stepped. The fall does not depend on it —
/// each pass takes off the time since the previous one — so this is only how
/// smooth it looks, and nothing runs while every meter is either fresh or on
/// the floor.
const DECAY_STEP: Duration = Duration::from_millis(33);

/// The two meter behaviours the stream does not carry.
#[derive(Default)]
pub struct Meters;

impl Meters {
    /// Fill in the master meter when the renderer never published one, and let
    /// every meter fall when its source goes quiet.
    ///
    /// Done on the model rather than in each panel, so the list rows, the
    /// master section and the 3D scene all read the same numbers.
    pub fn tick(&mut self, state: &SharedState, now: Instant) -> Tick {
        let mut guard = state.inner.lock().unwrap();
        // One reborrow, so each level map and its timestamps can be borrowed
        // side by side instead of cloning the timestamps every pass.
        let live = &mut *guard;
        let last_pass = live.meter_decay_at.replace(now);
        let mut changed = false;
        let mut falling = false;
        for (id, meter) in live.app.speaker_levels.iter_mut() {
            let seen = live.speaker_level_seen.get(id).copied();
            let next = decayed(meter, seen, last_pass, now);
            changed |= next != *meter;
            falling |= is_falling(&next, seen, now);
            *meter = next;
        }
        for (id, meter) in live.app.source_levels.iter_mut() {
            let seen = live.source_level_seen.get(id).copied();
            let next = decayed(meter, seen, last_pass, now);
            changed |= next != *meter;
            falling |= is_falling(&next, seen, now);
            *meter = next;
        }
        // The renderer's own master level wins whenever it has published one;
        // this only fills a silence.
        if !live.master_reported {
            let levels: Vec<Meter> = live.app.speaker_levels.values().cloned().collect();
            if let Some(master) = derived_master(&levels) {
                changed |= live.app.master_level.as_ref() != Some(&master);
                live.hold("master".to_owned(), master.peak_dbfs);
                live.app.master_level = Some(master);
            }
        }
        Tick {
            changed,
            next: falling.then(|| now + DECAY_STEP),
        }
    }
}

/// Whether this meter still has somewhere to fall: its hold has passed and it
/// has not reached the floor. A meter that is neither is why the clock can go
/// back to sleep.
fn is_falling(meter: &Meter, seen: Option<Instant>, now: Instant) -> bool {
    let Some(seen) = seen else {
        return false;
    };
    now >= seen + DECAY_START && meter.peak_dbfs > DECAY_FLOOR
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meter(peak: f64, rms: f64) -> Meter {
        Meter {
            peak_dbfs: peak,
            rms_dbfs: rms,
        }
    }

    #[test]
    fn n_equal_speakers_sum_to_their_own_level() {
        // Power addition, not amplitude: four speakers at −12 dB are a −12 dB
        // master, not a −6 dB one.
        let levels = vec![meter(-6.0, -12.0); 4];
        let master = derived_master(&levels).expect("a master");
        assert!((master.rms_dbfs - -12.0).abs() < 1e-9);
        // Peak is the loudest speaker's peak, not a sum.
        assert_eq!(master.peak_dbfs, -6.0);
    }

    #[test]
    fn a_quiet_level_is_not_snapped_up_to_the_floor() {
        let master = derived_master(&[meter(-80.0, -80.0)]).expect("a master");
        assert!((master.rms_dbfs - -80.0).abs() < 1e-6);
        // The peak floor is the meter scale's, not the parser's.
        assert_eq!(master.peak_dbfs, METER_DB_MIN);
        // Nothing to derive from nothing.
        assert!(derived_master(&[]).is_none());
    }

    #[test]
    fn a_meter_holds_then_falls_and_stops_at_the_parsers_floor() {
        let m = meter(-6.0, -12.0);
        let seen = Instant::now();
        // Inside the hold window nothing moves: a meter that fell immediately
        // would flicker on every gap between messages.
        let held = decayed(&m, Some(seen), None, seen + Duration::from_millis(200));
        assert_eq!(held.peak_dbfs, -6.0);
        assert_eq!(
            decayed(&m, None, None, seen + Duration::from_secs(5)).peak_dbfs,
            -6.0
        );
        // A refresh restarts the hold, whenever the previous pass ran.
        let refreshed = seen + Duration::from_secs(1);
        let held = decayed(
            &m,
            Some(refreshed),
            Some(seen),
            refreshed + Duration::from_millis(200),
        );
        assert_eq!(held.peak_dbfs, -6.0);
        // One second past the hold is 45 dB down, on both readings.
        let fallen = decayed(
            &m,
            Some(seen),
            None,
            seen + DECAY_START + Duration::from_secs(1),
        );
        assert!((fallen.peak_dbfs - -51.0).abs() < 1e-9);
        assert!((fallen.rms_dbfs - -57.0).abs() < 1e-9);
        // And it runs off the bottom of the scale rather than stopping on it.
        let gone = decayed(
            &m,
            Some(seen),
            None,
            seen + DECAY_START + Duration::from_secs(10),
        );
        assert_eq!(gone.peak_dbfs, DECAY_FLOOR);
    }

    /// Decay a meter refreshed at `seen` with one pass every `step` until `end`,
    /// the way `maintain_meters` does once per frame.
    fn decay_per_frame(received: &Meter, seen: Instant, step: Duration, end: Instant) -> Meter {
        let mut m = received.clone();
        let mut last_pass = None;
        let mut now = seen;
        while now < end {
            now = (now + step).min(end);
            m = decayed(&m, Some(seen), last_pass, now);
            last_pass = Some(now);
        }
        m
    }

    #[test]
    fn the_fall_does_not_depend_on_how_often_it_is_applied() {
        let received = meter(-6.0, -12.0);
        let seen = Instant::now();
        // A fifth of a second past the hold is 9 dB at any frame rate. Taking
        // the whole time since the refresh off on every frame made a 50 fps
        // meter lose ~49.5 dB here.
        let end = seen + DECAY_START + Duration::from_millis(200);
        let at_50fps = decay_per_frame(&received, seen, Duration::from_millis(20), end);
        assert!(
            (at_50fps.peak_dbfs - -15.0).abs() < 1e-9,
            "{}",
            at_50fps.peak_dbfs
        );

        // Many small passes land where one big one does, on both readings.
        let end = seen + Duration::from_millis(1200);
        let once = decayed(&received, Some(seen), None, end);
        assert!((once.peak_dbfs - (-6.0 - 45.0 * 0.95)).abs() < 1e-9);
        for fps in [30u32, 50, 60, 144, 1000] {
            let stepped = decay_per_frame(&received, seen, Duration::from_secs(1) / fps, end);
            assert!(
                (stepped.peak_dbfs - once.peak_dbfs).abs() < 1e-9,
                "{fps} fps: peak {} dB, one pass {} dB",
                stepped.peak_dbfs,
                once.peak_dbfs
            );
            assert!(
                (stepped.rms_dbfs - once.rms_dbfs).abs() < 1e-9,
                "{fps} fps: rms {} dB, one pass {} dB",
                stepped.rms_dbfs,
                once.rms_dbfs
            );
        }
    }
}
