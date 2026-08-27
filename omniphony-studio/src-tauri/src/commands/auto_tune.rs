//! Commands for the backend auto-tuner.
//!
//! Only reachable when `rust_auto_tune` is on in `osc_config.json`; with the
//! flag off the frontend keeps driving its own state machine and never calls
//! these. See `auto_tune::runner`.

use tauri::{AppHandle, State};

use crate::auto_tune::state_machine::{Ack, Options};
use crate::config::load_config;
use crate::SharedState;

/// Whether the backend tuner is in charge. The wizard asks once, at open, and
/// builds the matching runner.
#[tauri::command]
pub fn auto_tune_backend_enabled(state: State<SharedState>) -> bool {
    load_config(&state.config_dir).rust_auto_tune
}

/// Start a run. The error is the refusal reason the wizard already has strings
/// for: `not-enabled`, `paused`, `already-running`.
#[tauri::command]
pub fn auto_tune_start(app: AppHandle, state: State<SharedState>) -> Result<(), String> {
    state
        .auto_tune
        .start(
            app,
            state.inner.clone(),
            state.osc_tx.clone(),
            Options::default(),
        )
        .map_err(|reason| reason.to_string())
}

/// Stop the run and put the renderer back where it was.
#[tauri::command]
pub fn auto_tune_cancel(app: AppHandle, state: State<SharedState>) -> bool {
    state.auto_tune.cancel(&app, &state.osc_tx)
}

/// Stop the run and keep the tuned values. They are live, not persisted — the
/// wizard tells the user to save.
#[tauri::command]
pub fn auto_tune_accept(state: State<SharedState>) -> bool {
    state.auto_tune.accept()
}

#[tauri::command]
pub fn auto_tune_ack(app: AppHandle, state: State<SharedState>, kind: String) -> bool {
    let Some(ack) = (match kind.as_str() {
        "perturbation" => Some(Ack::Perturbation),
        "skipPerturbation" => Some(Ack::SkipPerturbation),
        "resumeAfterSourceLoss" => Some(Ack::ResumeAfterSourceLoss),
        _ => None,
    }) else {
        return false;
    };
    state.auto_tune.user_ack(&app, &state.osc_tx, ack)
}

#[tauri::command]
pub fn auto_tune_abbreviate(state: State<SharedState>) -> bool {
    state.auto_tune.abbreviate()
}

/// The current state name, `"idle"` when nothing is running. Used by the
/// close-confirmation, which has to know whether a run is in progress.
#[tauri::command]
pub fn auto_tune_state(state: State<SharedState>) -> &'static str {
    state.auto_tune.state()
}
