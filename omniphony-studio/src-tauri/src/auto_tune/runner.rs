//! Drives [`AutoTune`] against the live renderer.
//!
//! This is the backend counterpart of `src/auto-tune/runner.js`: it owns the
//! 50 ms tick, feeds the machine from `AppState`, and turns `ApplyParams` into
//! OSC control messages. Everything else it emits is forwarded verbatim to the
//! frontend as `auto_tune:event`, in the `(event, payload)` shape the wizard
//! already consumes — so the UI does not know which implementation is running.
//!
//! Two differences from the JS runner, both deliberate:
//!
//! **The clock is monotonic.** The JS stamps samples with `Date.now()`, so a
//! system clock adjustment mid-run — NTP stepping, a DST change on a machine
//! that keeps local time in the RTC — moves `sample.t` under a machine that
//! compares it against palier start times. A 15-minute run is long enough for
//! that to happen. Here everything is measured from one `Instant`.
//!
//! **The snapshot is taken from the renderer's own reported state**, not from
//! the frontend's config payload, and restoring it goes back out over the same
//! control path. The JS kept its snapshot in backend memory but nothing ever
//! restored it at startup, so quitting mid-run left the resampler on
//! half-tuned values with no way back.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc::UnboundedSender;

use super::detectors::Sample;
use super::state_machine::{Ack, AutoTune, Event, Options};
use super::wire::{event_name, event_payload};
use crate::app_state::AppState;
use crate::osc_listener::OscControlMsg;

/// How often the machine is fed. Matches the JS runner.
const TICK: Duration = Duration::from_millis(50);

/// The four parameters the tuner is allowed to touch.
///
/// `integral_discharge_ratio` is deliberately absent: it is non-operative on
/// the current hardware and the procedure never patches it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Params {
    pub kp_near: Option<f64>,
    pub ki: Option<f64>,
    pub max_adjust: Option<f64>,
    pub update_interval_callbacks: Option<f64>,
}

/// A tuning run in progress.
struct Session {
    fsm: AutoTune,
    /// Origin for every timestamp this run produces.
    started: Instant,
    /// What the renderer was running before the first patch.
    restore: Params,
}

impl Session {
    fn now_ms(&self) -> f64 {
        self.started.elapsed().as_secs_f64() * 1000.0
    }
}

/// Why a run could not start. Mirrors the JS `preflight()` reasons, which the
/// wizard already has strings for.
fn preflight(state: &AppState) -> Option<&'static str> {
    if state.adaptive_resampling != Some(1) {
        return Some("not-enabled");
    }
    if state.adaptive_resampling_paused == Some(1) {
        return Some("paused");
    }
    None
}

fn snapshot(state: &AppState) -> Params {
    Params {
        kp_near: state.adaptive_resampling_kp_near,
        ki: state.adaptive_resampling_ki,
        max_adjust: state.adaptive_resampling_max_adjust,
        update_interval_callbacks: state
            .adaptive_resampling_update_interval_callbacks
            .map(|v| v as f64),
    }
}

/// Send a parameter change to the renderer.
///
/// The clamps match `commands::resampling`, so a value the tuner picks is
/// treated exactly as one the user would have dialled in by hand.
fn apply(tx: &Arc<Mutex<Option<UnboundedSender<OscControlMsg>>>>, params: &Params) {
    let float = |address: &str, value: f64, floor: f64| OscControlMsg::SendFloat {
        address: address.to_string(),
        value: (value as f32).max(floor as f32),
    };
    if let Some(v) = params.kp_near {
        crate::send_control(
            tx,
            float("/omniphony/control/adaptive_resampling/kp_near", v, 1e-8),
        );
    }
    if let Some(v) = params.ki {
        crate::send_control(
            tx,
            float("/omniphony/control/adaptive_resampling/ki", v, 1e-8),
        );
    }
    if let Some(v) = params.max_adjust {
        crate::send_control(
            tx,
            float("/omniphony/control/adaptive_resampling/max_adjust", v, 1e-6),
        );
    }
    if let Some(v) = params.update_interval_callbacks {
        crate::send_control(
            tx,
            OscControlMsg::SendInt {
                address: "/omniphony/control/adaptive_resampling/update_interval_callbacks"
                    .to_string(),
                value: (v.round() as i32).max(1),
            },
        );
    }
}

/// The tuner, as held by `SharedState`.
#[derive(Clone, Default)]
pub struct AutoTuneRunner {
    session: Arc<Mutex<Option<Session>>>,
}

impl AutoTuneRunner {
    /// The wire name of the current state, `"idle"` when nothing is running.
    pub fn state(&self) -> &'static str {
        match self.session.lock().unwrap().as_ref() {
            Some(s) => s.fsm.state().as_str(),
            None => "idle",
        }
    }

    pub fn is_running(&self) -> bool {
        self.session.lock().unwrap().is_some()
    }

    /// Begin a run. Returns the refusal reason if the preflight fails.
    pub fn start(
        &self,
        app: AppHandle,
        state: Arc<Mutex<AppState>>,
        tx: Arc<Mutex<Option<UnboundedSender<OscControlMsg>>>>,
        options: Options,
    ) -> Result<(), &'static str> {
        if self.is_running() {
            return Err("already-running");
        }
        let restore = {
            let s = state.lock().unwrap();
            if let Some(reason) = preflight(&s) {
                return Err(reason);
            }
            snapshot(&s)
        };

        let mut fsm = AutoTune::new(options);
        let events = fsm.start(0.0);
        *self.session.lock().unwrap() = Some(Session {
            fsm,
            started: Instant::now(),
            restore,
        });
        self.dispatch(&app, &tx, events);

        // A plain OS thread rather than an async task: every step of a tick is
        // blocking work (two std mutexes and a channel send), which is exactly
        // what must not sit on the async runtime. Nothing here awaits.
        let this = self.clone();
        std::thread::Builder::new()
            .name("auto-tune".into())
            .spawn(move || {
                let mut next = Instant::now();
                loop {
                    next += TICK;
                    // Deadline-based so a slow tick does not accumulate drift.
                    if let Some(wait) = next.checked_duration_since(Instant::now()) {
                        std::thread::sleep(wait);
                    } else {
                        next = Instant::now();
                    }
                    if !this.tick(&app, &state, &tx) {
                        break;
                    }
                }
            })
            .map_err(|_| "thread-spawn-failed")?;
        Ok(())
    }

    /// One tick. Returns false once the run is over and the task should stop.
    fn tick(
        &self,
        app: &AppHandle,
        state: &Arc<Mutex<AppState>>,
        tx: &Arc<Mutex<Option<UnboundedSender<OscControlMsg>>>>,
    ) -> bool {
        let sample = {
            let s = state.lock().unwrap();
            Sample {
                t: 0.0, // stamped below, from the session clock
                latency_smoothed_ms: s.latency.latency_smoothed_ms,
                latency_target_ms: s.latency.latency_target_ms.map(|v| v as f64),
                resample_ratio: s.resample_ratio,
                phase: s.adaptive_resampling_state.clone(),
            }
        };

        let (events, finished) = {
            let mut guard = self.session.lock().unwrap();
            let Some(session) = guard.as_mut() else {
                return false;
            };
            let sample = Sample {
                t: session.now_ms(),
                ..sample
            };
            let events = session.fsm.push_sample(sample);
            (events, session.fsm.state().is_terminal())
        };

        self.dispatch(app, tx, events);
        if finished {
            // Leave the session in place so the frontend can still read the
            // final state; `accept` or `cancel` clears it.
            return false;
        }
        true
    }

    /// Forward events to the frontend, acting on the one with a side effect.
    fn dispatch(
        &self,
        app: &AppHandle,
        tx: &Arc<Mutex<Option<UnboundedSender<OscControlMsg>>>>,
        events: Vec<Event>,
    ) {
        for event in events {
            if let Event::ApplyParams {
                kp_near,
                ki,
                max_adjust,
                update_interval_callbacks,
            } = &event
            {
                apply(
                    tx,
                    &Params {
                        kp_near: *kp_near,
                        ki: *ki,
                        max_adjust: *max_adjust,
                        update_interval_callbacks: *update_interval_callbacks,
                    },
                );
            }
            let _ = app.emit(
                "auto_tune:event",
                serde_json::json!({
                    "event": event_name(&event),
                    "payload": event_payload(&event),
                }),
            );
        }
    }

    pub fn user_ack(
        &self,
        app: &AppHandle,
        tx: &Arc<Mutex<Option<UnboundedSender<OscControlMsg>>>>,
        kind: Ack,
    ) -> bool {
        let events = {
            let mut guard = self.session.lock().unwrap();
            let Some(session) = guard.as_mut() else {
                return false;
            };
            let now = session.now_ms();
            session.fsm.user_ack(kind, now)
        };
        let acted = !events.is_empty();
        self.dispatch(app, tx, events);
        acted
    }

    pub fn abbreviate(&self) -> bool {
        match self.session.lock().unwrap().as_mut() {
            Some(session) => session.fsm.abbreviate(),
            None => false,
        }
    }

    /// Stop the run and put the renderer back where it was.
    pub fn cancel(
        &self,
        app: &AppHandle,
        tx: &Arc<Mutex<Option<UnboundedSender<OscControlMsg>>>>,
    ) -> bool {
        let Some(mut session) = self.session.lock().unwrap().take() else {
            return false;
        };
        let events = session.fsm.cancel();
        self.dispatch(app, tx, events);
        apply(tx, &session.restore);
        true
    }

    /// Stop the run and keep the tuned values.
    pub fn accept(&self) -> bool {
        self.session.lock().unwrap().take().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::unbounded_channel;

    /// Drain a control channel into `(address, value)` pairs, where the value
    /// is stringified so floats and ints compare the same way.
    fn drain(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<OscControlMsg>,
    ) -> Vec<(String, String)> {
        let mut out = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            match msg {
                OscControlMsg::SendFloat { address, value } => {
                    out.push((address, value.to_string()))
                }
                OscControlMsg::SendInt { address, value } => out.push((address, value.to_string())),
                _ => panic!("the tuner should only ever send floats and ints"),
            }
        }
        out
    }

    fn channel() -> (
        Arc<Mutex<Option<UnboundedSender<OscControlMsg>>>>,
        tokio::sync::mpsc::UnboundedReceiver<OscControlMsg>,
    ) {
        let (tx, rx) = unbounded_channel();
        (Arc::new(Mutex::new(Some(tx))), rx)
    }

    /// The addresses are the contract with the renderer: a typo here sends the
    /// tuning into a void and the run silently does nothing.
    #[test]
    fn a_full_patch_addresses_every_parameter() {
        let (tx, mut rx) = channel();
        apply(
            &tx,
            &Params {
                kp_near: Some(12.5),
                ki: Some(2.5),
                max_adjust: Some(0.04),
                update_interval_callbacks: Some(5.0),
            },
        );
        assert_eq!(
            drain(&mut rx),
            vec![
                (
                    "/omniphony/control/adaptive_resampling/kp_near".to_string(),
                    "12.5".to_string()
                ),
                (
                    "/omniphony/control/adaptive_resampling/ki".to_string(),
                    "2.5".to_string()
                ),
                (
                    "/omniphony/control/adaptive_resampling/max_adjust".to_string(),
                    "0.04".to_string()
                ),
                (
                    "/omniphony/control/adaptive_resampling/update_interval_callbacks".to_string(),
                    "5".to_string()
                ),
            ]
        );
    }

    /// `ApplyParams` carries only what changed, and so must the patch — the kp
    /// sweep must not keep re-sending a ki the machine has not chosen yet.
    #[test]
    fn a_partial_patch_sends_only_what_it_carries() {
        let (tx, mut rx) = channel();
        apply(
            &tx,
            &Params {
                ki: Some(3.0),
                ..Default::default()
            },
        );
        let sent = drain(&mut rx);
        assert_eq!(sent.len(), 1, "sent {sent:?}");
        assert!(sent[0].0.ends_with("/ki"));
    }

    /// Same floors as `commands::resampling`, so a tuner value is treated
    /// exactly like one dialled in by hand. Zero ki in particular: the machine
    /// applies ki = 0 during the whole kp sweep.
    #[test]
    fn the_clamps_match_the_manual_controls() {
        let (tx, mut rx) = channel();
        apply(
            &tx,
            &Params {
                kp_near: Some(0.0),
                ki: Some(0.0),
                max_adjust: Some(0.0),
                update_interval_callbacks: Some(0.4),
            },
        );
        let sent = drain(&mut rx);
        assert_eq!(sent[0].1, "0.00000001", "kp_near floor");
        assert_eq!(sent[1].1, "0.00000001", "ki floor");
        assert_eq!(sent[2].1, "0.000001", "max_adjust floor");
        assert_eq!(sent[3].1, "1", "update interval floor");
    }

    /// The JS rounds the callback count (`Math.round`); truncating would put
    /// the renderer one callback below what the tuner measured with.
    #[test]
    fn the_update_interval_is_rounded_not_truncated() {
        let (tx, mut rx) = channel();
        apply(
            &tx,
            &Params {
                update_interval_callbacks: Some(5.6),
                ..Default::default()
            },
        );
        assert_eq!(drain(&mut rx)[0].1, "6");
    }

    #[test]
    fn the_preflight_refuses_a_disabled_or_paused_controller() {
        let mut state = AppState::default();
        state.adaptive_resampling = Some(0);
        assert_eq!(preflight(&state), Some("not-enabled"));

        state.adaptive_resampling = Some(1);
        state.adaptive_resampling_paused = Some(1);
        assert_eq!(preflight(&state), Some("paused"));

        state.adaptive_resampling_paused = Some(0);
        assert_eq!(preflight(&state), None);
    }

    /// The snapshot is what `cancel` and the exit handler put back, so it has
    /// to read the renderer's reported values, not the defaults.
    #[test]
    fn the_snapshot_captures_the_four_tunable_parameters() {
        let mut state = AppState::default();
        state.adaptive_resampling_kp_near = Some(7.0);
        state.adaptive_resampling_ki = Some(1.4);
        state.adaptive_resampling_max_adjust = Some(0.1);
        state.adaptive_resampling_update_interval_callbacks = Some(10);
        let snap = snapshot(&state);
        assert_eq!(snap.kp_near, Some(7.0));
        assert_eq!(snap.ki, Some(1.4));
        assert_eq!(snap.max_adjust, Some(0.1));
        assert_eq!(snap.update_interval_callbacks, Some(10.0));
    }
}
