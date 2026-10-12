//! Audio output controls: sample rate, output device selection and the
//! output-device list refresh, plus the audio config apply.
//!
//! Each command forwards a value to the renderer over OSC.

use super::OscControlMsg;
use super::{SharedState, send_control};
use crate::osc_contract;

pub fn control_audio_sample_rate(state: &SharedState, sample_rate: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: osc_contract::CONTROL_AUDIO_SAMPLE_RATE.to_string(),
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
    // Neither step below may fail quietly: the apply follows this call, so a
    // `None` that nobody explains means the renderer is told to apply a
    // configuration it was never sent, a silent no-op that reads as a UI bug.
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
            address: osc_contract::CONTROL_CONFIG_AUDIO.to_string(),
            value: text,
        },
    );
    serde_json::to_value(&effective).ok()
}

/// `sendAudioConfig`: the whole audio document, built from the model, resolved
/// by the schema and applied. Every audio control writes the model first and
/// then asks for the document, so the device, the rate and the latency target
/// can never disagree about what was sent.
///
/// The apply only follows a document that actually went out: applying one the
/// renderer never received flips a switch that nothing acts on.
pub fn send_audio_document(state: &SharedState) {
    let payload = {
        let live = state.inner.lock().unwrap();
        let a = &live.app;
        serde_json::json!({
            "outputDevice": a.audio.audio_output_device,
            "sampleRate": a.audio.audio_sample_rate,
            "latencyTargetMs": a
                .latency
                .latency_requested_ms
                .or(a.latency.latency_target_ms),
        })
    };
    if control_audio_config(state, payload).is_some() {
        control_audio_config_apply(state);
    }
}

/// The output device; empty is the renderer's default.
pub fn set_output_device(state: &SharedState, device: String) {
    state.inner.lock().unwrap().app.audio.audio_output_device =
        (!device.is_empty()).then_some(device);
    send_audio_document(state);
}

/// The sample rate; 0 is "native".
pub fn set_sample_rate(state: &SharedState, rate: u32) {
    state.inner.lock().unwrap().app.audio.audio_sample_rate = (rate > 0).then_some(rate);
    send_audio_document(state);
}

/// The output file or named pipe. It is remembered even when empty — the
/// switch restores it — but only a path worth having is sent.
pub fn set_output_file(state: &SharedState, path: String) {
    state.inner.lock().unwrap().app.audio.audio_output_file = Some(path.clone());
    if !path.is_empty() {
        control_audio_output_file(state, path);
    }
}

pub fn control_audio_config_apply(state: &SharedState) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: osc_contract::CONTROL_CONFIG_AUDIO_APPLY.to_string(),
        },
    );
}

pub fn control_audio_output_device(state: &SharedState, output_device: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_AUDIO_OUTPUT_DEVICE.to_string(),
            value: output_device.trim().to_string(),
        },
    );
}

pub fn control_audio_output_backend(state: &SharedState, backend: String) {
    state.inner.lock().unwrap().app.audio.audio_output_backend = Some(backend.clone());
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_AUDIO_OUTPUT_BACKEND.to_string(),
            value: backend.trim().to_string(),
        },
    );
}

pub fn control_audio_output_file(state: &SharedState, path: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_AUDIO_OUTPUT_FILE.to_string(),
            value: path.trim().to_string(),
        },
    );
}

pub fn control_audio_output_file_format(state: &SharedState, format: String) {
    state
        .inner
        .lock()
        .unwrap()
        .app
        .audio
        .audio_output_file_format = Some(format.clone());
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_AUDIO_OUTPUT_FILE_FORMAT.to_string(),
            value: format.trim().to_string(),
        },
    );
}

pub fn refresh_output_devices(state: &SharedState) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: osc_contract::CONTROL_AUDIO_OUTPUT_DEVICES_REFRESH.to_string(),
        },
    );
}
