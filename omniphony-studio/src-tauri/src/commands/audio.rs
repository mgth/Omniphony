//! Audio output controls: sample rate, output device selection and the
//! output-device list refresh, plus the audio config apply.
//!
//! Each command forwards a value to the renderer over OSC.

use crate::osc_listener::OscControlMsg;
use crate::{send_control, SharedState};
use tauri::State;

#[tauri::command]
pub fn control_audio_sample_rate(state: State<SharedState>, sample_rate: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/audio/sample_rate".to_string(),
            value: sample_rate.max(0),
        },
    );
}

#[tauri::command]
pub fn control_audio_config(
    state: State<SharedState>,
    payload: serde_json::Value,
) -> Option<serde_json::Value> {
    // The form sends what the user typed; the schema decides what it means.
    // Returning the effective configuration is what lets the UI show the
    // corrected value rather than the rejected one — the frontend used to
    // apply these bounds itself on the way out, so a field that was pulled
    // into range looked accepted as typed.
    //
    // Neither step below may fail quietly. `sendAudioConfig()` chains the
    // apply call onto this one's promise, so a `None` here means the renderer
    // is told to apply a configuration it was never sent: the switch flips on,
    // nothing reaches the audio path, and the next state broadcast flips it
    // back. That is a silent no-op that reads as a UI bug, and it is what the
    // original `.ok()?` produced.
    let raw: crate::audio_config::AudioConfig = match serde_json::from_value(payload.clone()) {
        Ok(config) => config,
        Err(err) => {
            // `eprintln!`, not `log::error!`: this crate never installs a
            // logger, so every `log::` macro in it writes to nowhere.
            eprintln!(
                "[audio config] rejected — not sent to the renderer: {err}; payload: {payload}"
            );
            return None;
        }
    };
    let effective = raw.resolve();
    let text = match serde_json::to_string(&effective) {
        Ok(text) => text,
        Err(err) => {
            // serde_json refuses non-finite floats, so this is reachable if a
            // resolved field ever comes out NaN or infinite.
            eprintln!("[audio config] not serialisable — not sent: {err}; resolved: {effective:?}");
            return None;
        }
    };
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/config/audio".to_string(),
            value: text,
        },
    );
    serde_json::to_value(&effective).ok()
}

#[tauri::command]
pub fn control_audio_config_apply(state: State<SharedState>) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: "/omniphony/control/config/audio/apply".to_string(),
        },
    );
}

#[tauri::command]
pub fn control_audio_output_device(state: State<SharedState>, output_device: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/audio/output_device".to_string(),
            value: output_device.trim().to_string(),
        },
    );
}

#[tauri::command]
pub fn control_audio_output_backend(state: State<SharedState>, backend: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/audio/output_backend".to_string(),
            value: backend.trim().to_string(),
        },
    );
}

#[tauri::command]
pub fn control_audio_output_file(state: State<SharedState>, path: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/audio/output_file".to_string(),
            value: path.trim().to_string(),
        },
    );
}

#[tauri::command]
pub fn control_audio_output_file_format(state: State<SharedState>, format: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/audio/output_file_format".to_string(),
            value: format.trim().to_string(),
        },
    );
}

#[tauri::command]
pub fn refresh_output_devices(state: State<SharedState>) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: "/omniphony/control/audio/output_devices/refresh".to_string(),
        },
    );
}
