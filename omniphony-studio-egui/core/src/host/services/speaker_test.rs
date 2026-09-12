//! The per-speaker test signal and the safety window that ends it.
//!
//! The renderer has no timeout of its own: it plays until it is told to stop.
//! The web used a `setTimeout` (`speaker-test.js`); the port checked the
//! deadline while drawing the Test tab, so a tone survived a change of tab, a
//! folded panel, and a window that stopped drawing. It lives here instead,
//! where nothing has to be on screen for it to fire.

use std::time::{Duration, Instant};

use super::Tick;
use crate::host::commands::{SharedState, gain};

/// A burst stops itself after two seconds; a toggle after a minute. "Hold" has
/// no window — the pointer ends it.
pub const BURST: Duration = Duration::from_secs(2);
pub const TOGGLE_SAFETY: Duration = Duration::from_secs(60);

/// What this side started, as the model holds it: the view reads `running` to
/// show which speaker is playing.
#[derive(Default, Debug, Clone)]
pub struct SpeakerTestRun {
    pub running: Option<usize>,
    /// When the safety window closes. `None` while a hold test plays.
    pub deadline: Option<Instant>,
    /// What the stop message has to carry, remembered from the start.
    pub isolation: String,
}

/// Start the test on one speaker and arm its window. `mode` is the trigger
/// policy the UI offers: `burst`, `toggle`, or anything else for hold.
pub fn start(state: &SharedState, index: usize, level_db: f32, isolation: String, mode: &str) {
    let window = match mode {
        "burst" => Some(BURST),
        "toggle" => Some(TOGGLE_SAFETY),
        _ => None,
    };
    {
        let mut live = state.inner.lock().unwrap();
        live.speaker_test = SpeakerTestRun {
            running: Some(index),
            deadline: window.map(|window| Instant::now() + window),
            isolation: isolation.clone(),
        };
    }
    // Peak dBFS to the peak linear amplitude the renderer clamps to.
    gain::control_speaker_test(state, index as i32, 10f32.powf(level_db / 20.0), isolation);
}

/// Stop whatever is playing. Sent unconditionally when something was: "nothing
/// is running" is this side's belief, and the renderer's state is the one that
/// matters.
pub fn stop(state: &SharedState) {
    let isolation = {
        let mut live = state.inner.lock().unwrap();
        if live.speaker_test.running.is_none() {
            return;
        }
        let isolation = std::mem::take(&mut live.speaker_test.isolation);
        live.speaker_test = SpeakerTestRun::default();
        isolation
    };
    gain::control_speaker_test(state, -1, 0.0, isolation);
}

/// The safety window, on the core's clock.
#[derive(Default)]
pub struct SpeakerTest;

impl SpeakerTest {
    pub fn tick(&mut self, state: &SharedState, now: Instant) -> Tick {
        let deadline = state.inner.lock().unwrap().speaker_test.deadline;
        let Some(deadline) = deadline else {
            return Tick::idle();
        };
        if now < deadline {
            return Tick {
                changed: false,
                next: Some(deadline),
            };
        }
        stop(state);
        Tick {
            changed: true,
            next: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_stops_itself_and_a_hold_does_not() {
        let state = crate::host::commands::tests::state();
        start(&state, 3, -8.0, "test_only".to_owned(), "burst");
        let started = Instant::now();
        assert_eq!(state.inner.lock().unwrap().speaker_test.running, Some(3));

        let mut service = SpeakerTest;
        // Inside the window it only says when to come back.
        let tick = service.tick(&state, started + BURST / 2);
        assert!(!tick.changed && tick.next.is_some());
        assert_eq!(state.inner.lock().unwrap().speaker_test.running, Some(3));
        // Past it, the test stops itself, whatever is or is not on screen.
        let tick = service.tick(&state, started + BURST + Duration::from_millis(1));
        assert!(tick.changed && tick.next.is_none());
        assert_eq!(state.inner.lock().unwrap().speaker_test.running, None);

        // A hold test has no window at all.
        start(&state, 1, -8.0, "test_only".to_owned(), "hold");
        assert!(state.inner.lock().unwrap().speaker_test.deadline.is_none());
        let tick = service.tick(&state, Instant::now() + TOGGLE_SAFETY * 2);
        assert!(!tick.changed && tick.next.is_none());
        assert_eq!(state.inner.lock().unwrap().speaker_test.running, Some(1));
    }
}
