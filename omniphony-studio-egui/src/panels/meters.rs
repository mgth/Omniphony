//! The two meter behaviours the host computed and the OSC stream does not
//! carry: the derived master meter and the decay ballistics
//! (`osc_listener.rs:1955–1986`, `speakers.js:2410–2461`).
//!
//! Both exist because a meter has to keep saying something true between
//! messages. A renderer that never publishes a master level still has speaker
//! levels, and a meter whose source went quiet has to fall rather than freeze
//! on its last value — a frozen meter reads as signal.

use std::time::{Duration, Instant};

use crate::app::StudioSpike;
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

/// One meter, faded by how long it has been since it was last refreshed.
pub fn decayed(meter: &Meter, since: Option<Duration>) -> Meter {
    let Some(since) = since.filter(|d| *d >= DECAY_START) else {
        return meter.clone();
    };
    let db = DECAY_DB_PER_SEC * (since - DECAY_START).as_secs_f64();
    Meter {
        peak_dbfs: (meter.peak_dbfs - db).max(DECAY_FLOOR),
        rms_dbfs: (meter.rms_dbfs - db).max(DECAY_FLOOR),
    }
}

impl StudioSpike {
    /// Fill in the master meter when the renderer never published one, and let
    /// every meter fall when its source goes quiet.
    ///
    /// Both are done on the model rather than in each panel, so the list rows,
    /// the master section and the 3D scene all read the same numbers.
    pub(crate) fn maintain_meters(&mut self) {
        let now = Instant::now();
        let mut live = self.live.lock().unwrap();
        let seen = live.speaker_level_seen.clone();
        for (id, meter) in live.app.speaker_levels.iter_mut() {
            let since = seen.get(id).map(|at| now.duration_since(*at));
            *meter = decayed(meter, since);
        }
        let seen = live.source_level_seen.clone();
        for (id, meter) in live.app.source_levels.iter_mut() {
            let since = seen.get(id).map(|at| now.duration_since(*at));
            *meter = decayed(meter, since);
        }
        // The renderer's own master level wins whenever it has published one;
        // this only fills a silence.
        if live.master_reported {
            return;
        }
        let levels: Vec<Meter> = live.app.speaker_levels.values().cloned().collect();
        if let Some(master) = derived_master(&levels) {
            live.hold("master".to_owned(), master.peak_dbfs);
            live.app.master_level = Some(master);
        }
    }
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
        // Inside the hold window nothing moves: a meter that fell immediately
        // would flicker on every gap between messages.
        assert_eq!(
            decayed(&m, Some(Duration::from_millis(200))).peak_dbfs,
            -6.0
        );
        assert_eq!(decayed(&m, None).peak_dbfs, -6.0);
        // One second past the hold is 45 dB down, on both readings.
        let fallen = decayed(&m, Some(DECAY_START + Duration::from_secs(1)));
        assert!((fallen.peak_dbfs - -51.0).abs() < 1e-9);
        assert!((fallen.rms_dbfs - -57.0).abs() < 1e-9);
        // And it runs off the bottom of the scale rather than stopping on it.
        let gone = decayed(&m, Some(DECAY_START + Duration::from_secs(10)));
        assert_eq!(gone.peak_dbfs, DECAY_FLOOR);
    }
}
