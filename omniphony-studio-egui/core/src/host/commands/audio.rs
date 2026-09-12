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

/// `sendAudioConfig`: the whole audio document, built from the model, resolved
/// by the schema and applied. Every audio control writes the model first and
/// then asks for the document, so the device, the rate and the adaptive
/// controller can never disagree about what was sent.
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
            // The adaptive controller rides the same document, so a
            // parameter change and a device change cannot disagree.
            "adaptiveResampling": {
                "enabled": a.adaptive_resampling.unwrap_or(0) != 0,
                "enableFarMode": a.adaptive_resampling_enable_far_mode.unwrap_or(0) != 0,
                "forceSilenceInFarMode": a
                    .adaptive_resampling_force_silence_in_far_mode
                    .unwrap_or(1)
                    != 0,
                "hardRecoverHighInFarMode": a
                    .adaptive_resampling_hard_recover_high_in_far_mode
                    .unwrap_or(1)
                    != 0,
                "hardRecoverLowInFarMode": a
                    .adaptive_resampling_hard_recover_low_in_far_mode
                    .unwrap_or(0)
                    != 0,
                "farModeReturnFadeInMs": a.adaptive_resampling_far_mode_return_fade_in_ms,
                "kpNear": a.adaptive_resampling_kp_near,
                "ki": a.adaptive_resampling_ki,
                "integralDischargeRatio": a.adaptive_resampling_integral_discharge_ratio,
                "maxAdjust": a.adaptive_resampling_max_adjust,
                "highRecoverEntryMarginMs": a
                    .adaptive_resampling_high_recover_entry_margin_ms,
                "updateIntervalCallbacks": a.adaptive_resampling_update_interval_callbacks,
                "lowRecoverSettleStableMs": a
                    .adaptive_resampling_low_recover_settle_stable_ms,
                "lowRecoverEntryMarginMs": a
                    .adaptive_resampling_low_recover_entry_margin_ms,
                "lowRecoverExitMarginMs": a.adaptive_resampling_low_recover_exit_margin_ms,
                "lowRecoverSettleMarginMs": a
                    .adaptive_resampling_low_recover_settle_margin_ms,
                "lowRecoverRefillDeltaAlpha": a
                    .adaptive_resampling_low_recover_refill_delta_alpha,
                "controlSmoothingCutoffHz": a
                    .adaptive_resampling_control_smoothing_cutoff_hz,
                "controlSmoothingOrder": a.adaptive_resampling_control_smoothing_order,
                "paused": a.adaptive_resampling_paused.unwrap_or(0) != 0,
                "usePreBridgeClock": a.adaptive_resampling_use_pre_bridge_clock.unwrap_or(0) != 0,
                "useOutputPacing": a.adaptive_resampling_use_output_pacing.unwrap_or(0) != 0,
                "disableBackpressure": a
                    .adaptive_resampling_disable_backpressure
                    .unwrap_or(0)
                    != 0,
            }
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
    state.inner.lock().unwrap().app.audio.audio_output_backend = Some(backend.clone());
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
