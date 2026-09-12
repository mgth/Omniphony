//! Audio output controls: sample rate, output device selection and the
//! output-device list refresh, plus the audio config apply.
//!
//! Each command forwards a value to the renderer over OSC.

use super::OscControlMsg;
use super::{SharedState, send_control};

pub fn control_audio_sample_rate(state: &SharedState, sample_rate: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/audio/sample_rate".to_string(),
            value: sample_rate.max(0),
        },
    );
}

pub fn control_audio_config(
    state: &SharedState,
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
    let raw: crate::host::audio_config::AudioConfig = match serde_json::from_value(payload.clone())
    {
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

pub fn control_audio_config_apply(state: &SharedState) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: "/omniphony/control/config/audio/apply".to_string(),
        },
    );
}

pub fn control_audio_output_device(state: &SharedState, output_device: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/audio/output_device".to_string(),
            value: output_device.trim().to_string(),
        },
    );
}

pub fn control_audio_output_backend(state: &SharedState, backend: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/audio/output_backend".to_string(),
            value: backend.trim().to_string(),
        },
    );
}

pub fn control_audio_output_file(state: &SharedState, path: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/audio/output_file".to_string(),
            value: path.trim().to_string(),
        },
    );
}

pub fn control_audio_output_file_format(state: &SharedState, format: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/audio/output_file_format".to_string(),
            value: format.trim().to_string(),
        },
    );
}

pub fn refresh_output_devices(state: &SharedState) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: "/omniphony/control/audio/output_devices/refresh".to_string(),
        },
    );
}
