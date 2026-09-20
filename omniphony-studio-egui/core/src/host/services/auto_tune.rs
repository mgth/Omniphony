//! The auto-tune run: feeding the machine and applying what it asks for.
//!
//! [`crate::auto_tune`] is the machine — states, detectors, the patches it
//! decides on. This is what drives it: telemetry in at a fixed cadence, patches
//! out to the live controller, and the snapshot that lets Cancel put the
//! controller back exactly as the run found it.
//!
//! It used to run from the wizard's draw code, which made the cadence the frame
//! rate's business and the run the dialog's. A sweep that tunes an audio loop
//! should not slow down because a window is busy, or stop because one is not
//! being drawn; and the values are live on the renderer whether or not anyone is
//! looking at the dialog. The wizard above this reads the run and draws it.
//!
//! Two rules the web set and this keeps: the values are applied live but not
//! persisted — Save is still the user's to press — and the values the run
//! started from are snapshotted so Cancel and Revert put the controller back.

use std::time::{Duration, Instant};

use super::Tick;
use crate::auto_tune::detectors::{Phase, Sample};
use crate::auto_tune::machine::{Ack, AutoTune, Event, Failure, Note, Outcome, Patch, State};
use crate::host::commands::SharedState;
use crate::model::app_state::AppState;

/// The web polls the controller at 50 ms, and there is nothing to gain from
/// feeding the machine faster than the detectors can use.
const POLL: Duration = Duration::from_millis(50);

/// Why a run could not start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    NotEnabled,
    Paused,
}

/// A run, from the moment the wizard is opened to the moment it is closed.
#[derive(Default)]
pub struct Run {
    pub machine: AutoTune,
    /// Set until Start is pressed, and again if the start is refused.
    pub refused: Option<Refused>,
    pub started: bool,
    /// The last note the machine attached to a step, shown under it.
    pub note: Option<Note>,
    pub outcome: Option<Outcome>,
    pub failure: Option<Failure>,
    pub can_abbreviate: bool,
    pub elapsed_ms: f64,
    polled_at: Option<Instant>,
    /// The values the controller had before the run, restored on Cancel.
    snapshot: Option<Patch>,
}

impl Run {
    /// A sweep is in flight: the controller is mid-patch and the machine wants
    /// telemetry.
    pub fn running(&self) -> bool {
        !matches!(
            self.machine.state(),
            State::Idle | State::Completed | State::Cancelled | State::Failed
        )
    }
}

/// Open the wizard. Nothing is patched until `start`.
pub fn open(state: &SharedState) {
    state.inner.lock().unwrap().auto_tune = Some(Run::default());
    (state.waker)();
}

/// Close it, leaving the controller on whatever the run reached: accepting is
/// letting the live values stand and dropping the snapshot that would undo them.
pub fn close(state: &SharedState) {
    state.inner.lock().unwrap().auto_tune = None;
    (state.waker)();
}

/// Begin the sweep, or say why it cannot begin.
pub fn start(state: &SharedState) {
    let events = {
        let mut guard = state.inner.lock().unwrap();
        let live = &mut *guard;
        // The run patches a controller that has to be running: tuning a
        // disabled or paused one would tune nothing.
        let refused = match (
            live.app.adaptive_resampling.unwrap_or(0) != 0,
            live.app.adaptive_resampling_paused.unwrap_or(0) != 0,
        ) {
            (false, _) => Some(Refused::NotEnabled),
            (_, true) => Some(Refused::Paused),
            _ => None,
        };
        let snapshot = controller_snapshot(&live.app);
        let elapsed = live.started.elapsed().as_secs_f64() * 1000.0;
        let Some(run) = live.auto_tune.as_mut() else {
            return;
        };
        if let Some(refused) = refused {
            run.refused = Some(refused);
            return;
        }
        run.snapshot = Some(snapshot);
        run.started = true;
        run.machine.start(elapsed)
    };
    apply_events(state, events);
}

/// A step the machine asked the user for — the disturbance, or skipping it.
pub fn acknowledge(state: &SharedState, ack: Ack) {
    let events = {
        let mut guard = state.inner.lock().unwrap();
        let live = &mut *guard;
        let now = live.started.elapsed().as_secs_f64() * 1000.0;
        let Some(run) = live.auto_tune.as_mut() else {
            return;
        };
        run.machine.user_ack(ack, now)
    };
    apply_events(state, events);
}

/// Cut the long observation step short, when the machine says it has seen
/// enough to allow it.
pub fn abbreviate(state: &SharedState) {
    let mut live = state.inner.lock().unwrap();
    if let Some(run) = live.auto_tune.as_mut() {
        run.machine.abbreviate();
    }
}

/// Put the controller back where the run found it, and close.
pub fn revert(state: &SharedState) {
    let snapshot = {
        let mut live = state.inner.lock().unwrap();
        let Some(run) = live.auto_tune.as_mut() else {
            return;
        };
        run.machine.cancel();
        run.snapshot
    };
    if let Some(snapshot) = snapshot {
        apply_patch(state, snapshot);
    }
    close(state);
}

/// The four values the run touches, as they are now.
fn controller_snapshot(app: &AppState) -> Patch {
    Patch {
        kp_near: app.adaptive_resampling_kp_near,
        ki: app.adaptive_resampling_ki,
        max_adjust: app.adaptive_resampling_max_adjust,
        update_interval_callbacks: app
            .adaptive_resampling_update_interval_callbacks
            .map(|v| v.max(1) as u32),
    }
}

/// Write a patch into the live controller and send it, the way a slider on the
/// adaptive panel would.
fn apply_patch(state: &SharedState, patch: Patch) {
    {
        let mut live = state.inner.lock().unwrap();
        if let Some(kp) = patch.kp_near {
            live.app.adaptive_resampling_kp_near = Some(kp);
        }
        if let Some(ki) = patch.ki {
            live.app.adaptive_resampling_ki = Some(ki);
        }
        if let Some(max_adjust) = patch.max_adjust {
            live.app.adaptive_resampling_max_adjust = Some(max_adjust);
        }
        if let Some(interval) = patch.update_interval_callbacks {
            live.app.adaptive_resampling_update_interval_callbacks = Some(interval.max(1) as i64);
        }
    }
    crate::host::commands::audio::send_audio_document(state);
}

/// Act on what the machine said. The lock is not held across this: a patch
/// sends the whole audio document, which takes it again.
fn apply_events(state: &SharedState, events: Vec<Event>) {
    for event in events {
        match event {
            Event::ApplyParams(patch) => apply_patch(state, patch),
            Event::Progress(progress) => {
                let mut live = state.inner.lock().unwrap();
                if let Some(run) = live.auto_tune.as_mut() {
                    run.note = progress.note;
                    if let Some(elapsed) = progress.elapsed_ms {
                        run.elapsed_ms = elapsed;
                    }
                    run.can_abbreviate |= progress.can_abbreviate;
                }
            }
            Event::Complete(outcome) => {
                let mut live = state.inner.lock().unwrap();
                if let Some(run) = live.auto_tune.as_mut() {
                    run.outcome = Some(outcome);
                }
                live.push_log("info", "auto-tune", "the run finished");
            }
            Event::Failed(failure) => {
                let mut live = state.inner.lock().unwrap();
                if let Some(run) = live.auto_tune.as_mut() {
                    run.failure = Some(failure);
                }
            }
            Event::SourceLost { events } => {
                state.inner.lock().unwrap().push_log(
                    "warn",
                    "auto-tune",
                    format!("the source went away ({events} recoveries); the run is held"),
                );
            }
            Event::SourceRecovered { .. } | Event::AwaitUserAction(_) | Event::Cancelled => {}
        }
    }
}

#[derive(Default)]
pub struct AutoTuneRunner;

impl AutoTuneRunner {
    pub fn tick(&mut self, state: &SharedState, now: Instant) -> Tick {
        let (events, due) = {
            let mut guard = state.inner.lock().unwrap();
            let live = &mut *guard;
            let Some(run) = live.auto_tune.as_ref() else {
                return Tick::idle();
            };
            if !run.running() {
                return Tick::idle();
            }
            if let Some(at) = run.polled_at
                && now < at + POLL
            {
                return Tick {
                    changed: false,
                    next: Some(at + POLL),
                };
            }
            let sample = Sample {
                t: now.saturating_duration_since(live.started).as_secs_f64() * 1000.0,
                latency_smoothed_ms: live.app.latency.latency_smoothed_ms,
                latency_target_ms: live.app.latency.latency_target_ms.map(|v| v as f64),
                resample_ratio: live.app.resample_ratio,
                phase: match live.app.adaptive_resampling_state.as_deref() {
                    Some("low-recover") => Phase::LowRecover,
                    _ => Phase::Other,
                },
            };
            let Some(run) = live.auto_tune.as_mut() else {
                return Tick::idle();
            };
            run.polled_at = Some(now);
            (run.machine.push_sample(sample), now + POLL)
        };
        apply_events(state, events);
        Tick {
            changed: true,
            next: Some(due),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_refuses_a_controller_that_is_not_running() {
        let state = crate::host::commands::tests::state();
        open(&state);
        start(&state);
        {
            let live = state.inner.lock().unwrap();
            let run = live.auto_tune.as_ref().unwrap();
            assert_eq!(run.refused, Some(Refused::NotEnabled));
            assert!(!run.started);
        }

        // Enabled but held: tuning a paused controller would tune nothing.
        state.inner.lock().unwrap().app.adaptive_resampling = Some(1);
        state.inner.lock().unwrap().app.adaptive_resampling_paused = Some(1);
        start(&state);
        assert_eq!(
            state
                .inner
                .lock()
                .unwrap()
                .auto_tune
                .as_ref()
                .unwrap()
                .refused,
            Some(Refused::Paused)
        );
    }

    #[test]
    fn the_machine_is_fed_on_its_own_cadence_and_not_faster() {
        let state = crate::host::commands::tests::state();
        {
            let mut live = state.inner.lock().unwrap();
            live.app.adaptive_resampling = Some(1);
        }
        open(&state);
        start(&state);
        let mut runner = AutoTuneRunner::default();
        let now = Instant::now();

        let tick = runner.tick(&state, now);
        assert_eq!(tick.next, Some(now + POLL));

        // Too soon: the machine is left alone, and the pass says when it is due.
        let early = now + POLL / 2;
        let tick = runner.tick(&state, early);
        assert!(!tick.changed);
        assert_eq!(tick.next, Some(now + POLL));

        // Due: fed again, and the next deadline moves on.
        let late = now + POLL;
        let tick = runner.tick(&state, late);
        assert!(tick.changed);
        assert_eq!(tick.next, Some(late + POLL));

        // A run that is over is not fed at all.
        revert(&state);
        assert!(state.inner.lock().unwrap().auto_tune.is_none());
        assert!(runner.tick(&state, late + POLL).next.is_none());
    }
}
