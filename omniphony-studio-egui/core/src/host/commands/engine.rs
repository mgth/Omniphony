//! Engine-level controls: renderer config save/reload, log level, ramp mode,
//! dynamic-range-control (DRC) tuning and the layout-export trigger.
//!
//! Each command forwards a value to the renderer over OSC.

use super::OscControlMsg;
use super::{SharedState, send_control};
use crate::osc_contract;

pub fn control_save_config(state: &SharedState) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: osc_contract::CONTROL_SAVE_CONFIG.to_string(),
        },
    );
}

pub fn control_reload_config(state: &SharedState) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: osc_contract::CONTROL_RELOAD_CONFIG.to_string(),
        },
    );
}

/// The Save button: the model remembers that a save was asked for — the
/// footer's indicator reads it — and the renderer is told. The bootstrap path
/// of the input apply wants only the message, and calls
/// [`control_save_config`].
pub fn request_save_config(state: &SharedState) {
    state.inner.lock().unwrap().save_requested = true;
    control_save_config(state);
}

pub fn control_log_level(state: &SharedState, value: String) {
    let trimmed = value.trim().to_ascii_lowercase();
    if !matches!(
        trimmed.as_str(),
        "off" | "error" | "warn" | "info" | "debug" | "trace"
    ) {
        return;
    }
    state.inner.lock().unwrap().app.log_level = Some(trimmed.clone());
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_LOG_LEVEL.to_string(),
            value: trimmed,
        },
    );
}

pub fn control_ramp_mode(state: &SharedState, value: String) {
    let trimmed = value.trim().to_ascii_lowercase();
    if !matches!(trimmed.as_str(), "off" | "frame" | "sample") {
        return;
    }
    state.inner.lock().unwrap().app.audio.ramp_mode = Some(trimmed.clone());
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_RAMP_MODE.to_string(),
            value: trimmed,
        },
    );
}

/// Set any declared live option (the renderer's `options` registry) through
/// the generic `/omniphony/control/option [key, value]` address. The value is
/// a JSON scalar from the `data-option` binder: a string for enum/id options,
/// a bool for toggles (forwarded as int 0/1), a number for future scalar
/// kinds. Validation lives renderer-side against the registry spec — an
/// unknown key or a bad value is dropped there, per the OSC contract.
/// Pick the object generator, or `""` for none.
///
/// Its own command because changing it drops state: the renderer forgets the
/// previous generator's parameter overrides, so the local copy has to go with
/// it or the form would show the old generator's values under the new one's
/// name until the next snapshot.
pub fn set_object_generator(state: &SharedState, id: &str) {
    state
        .inner
        .lock()
        .unwrap()
        .app
        .live_options
        .object_generator_params = None;
    control_option(
        state,
        "object_generator_id".to_owned(),
        serde_json::json!(id),
    );
}

pub fn control_option(state: &SharedState, key: String, value: serde_json::Value) {
    let k = key.trim().to_ascii_lowercase();
    if k.is_empty() {
        return;
    }
    let arg = match &value {
        serde_json::Value::String(s) => rosc::OscType::String(s.trim().to_ascii_lowercase()),
        serde_json::Value::Bool(b) => rosc::OscType::Int(if *b { 1 } else { 0 }),
        serde_json::Value::Number(n) => match n.as_f64() {
            Some(f) if f.is_finite() => rosc::OscType::Float(f as f32),
            _ => return,
        },
        _ => return,
    };
    // Optimistic, and only for an option that is actually going out: the
    // registry showing a value the renderer never heard about is the failure
    // this command exists to avoid.
    state.inner.lock().unwrap().set_option(&k, value);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: osc_contract::CONTROL_OPTION.to_string(),
            args: vec![rosc::OscType::String(k), arg],
        },
    );
}

/// Set a live object-generator parameter (PAD: `strength` / `hpf_hz` /
/// `gain_db`). Sent as `[key, value]`; the renderer clamps and applies it live.
/// Remember a live parameter in the model, so the slider that set it reads
/// its own value back instead of snapping until the renderer's echo arrives.
fn remember_param(params: &mut Option<serde_json::Value>, key: &str, value: f64) {
    let params = params.get_or_insert_with(|| serde_json::Value::Object(Default::default()));
    if let Some(map) = params.as_object_mut() {
        map.insert(key.to_owned(), serde_json::json!(value));
    }
}

/// A generator parameter, remembered and sent.
pub fn set_object_generator_param(state: &SharedState, key: &str, value: f64) {
    remember_param(
        &mut state
            .inner
            .lock()
            .unwrap()
            .app
            .live_options
            .object_generator_params,
        key,
        value,
    );
    control_object_generator_param(state, key.to_owned(), value as f32);
}

/// A phantom-extraction parameter, remembered and sent.
pub fn set_phantom_extract_param(state: &SharedState, key: &str, value: f64) {
    remember_param(
        &mut state.inner.lock().unwrap().app.live_options.phantom_params,
        key,
        value,
    );
    control_phantom_extract_param(state, key.to_owned(), value as f32);
}

pub fn control_object_generator_param(state: &SharedState, key: String, value: f32) {
    let k = key.trim().to_ascii_lowercase();
    // Any non-empty key is accepted; the renderer validates it against the active
    // generator's declared schema and clamps the value.
    if k.is_empty() || !value.is_finite() {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: osc_contract::CONTROL_OBJECT_GENERATOR_PARAM.to_string(),
            args: vec![rosc::OscType::String(k), rosc::OscType::Float(value)],
        },
    );
}

/// Set a live phantom-extraction parameter (`strength` / `passes` / `lift`). Sent
/// as `[key, value]`; the renderer clamps and applies it live.
pub fn control_phantom_extract_param(state: &SharedState, key: String, value: f32) {
    let k = key.trim().to_ascii_lowercase();
    if k.is_empty() || !value.is_finite() {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: osc_contract::CONTROL_PHANTOM_EXTRACT_PARAM.to_string(),
            args: vec![rosc::OscType::String(k), rosc::OscType::Float(value)],
        },
    );
}

/// Set the parametrable virtual bed (a YAML `SpeakerLayout`, one entry per
/// channel label). An empty string resets to the built-in canonical poses.
/// The parametrable virtual bed, applied and sent. The document is what the
/// channel editor built; the model shows it at once so the editor, the 3D view
/// and the audio agree before the renderer echoes.
pub fn set_virtual_bed(state: &SharedState, payload: serde_json::Value) {
    let value = serde_json::to_string(&payload).ok();
    preview_virtual_bed(state, payload);
    if let Some(value) = value {
        control_virtual_bed(state, value);
    }
}

/// Reset every channel to its catalogue corner, in cartesian mode.
///
/// Sending an empty string would hand the renderer its built-in *polar* poses,
/// which the editor would then display as cartesian corners: the polar form
/// would change while the cartesian fields stayed stale even though the mode
/// read "cartesian". Pushing the explicit cartesian bed keeps the editor, the
/// 3D view and the audio in agreement.
pub fn reset_virtual_bed(state: &SharedState) {
    use crate::host::channels::{Base, Channel, build_layout_payload, default_entry};
    let payload = {
        let live = state.inner.lock().unwrap();
        let room = live.app.room_ratio.clone();
        let channels: Vec<Channel> =
            crate::host::channels::effective_channels(&live.channels, &live.app)
                .iter()
                .map(|channel| {
                    let base = live.channels.base(&channel.name).cloned().unwrap_or(Base {
                        name: channel.name.clone(),
                        x: 0.0,
                        y: 0.0,
                        z: 0.0,
                        spatialize: true,
                    });
                    default_entry(&room, &base)
                })
                .collect();
        build_layout_payload(&live.app, &channels)
    };
    set_virtual_bed(state, payload);
}

/// A drag in flight moves the local copy only: the bed is a whole layout, and
/// pushing one per pointer move would be a stream of layouts.
pub fn preview_virtual_bed(state: &SharedState, payload: serde_json::Value) {
    state.inner.lock().unwrap().app.live_options.virtual_bed = Some(payload);
    // The bed's scene markers are published by a core service, so a bed the UI
    // just changed has to reach the clock. Without this the markers would wait
    // for whatever else wakes it — an incoming packet, which is exactly what a
    // Studio editing its bed offline does not have.
    (state.waker)();
}

pub fn control_virtual_bed(state: &SharedState, value: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_VIRTUAL_BED.to_string(),
            value,
        },
    );
}

pub fn control_drc_mode(state: &SharedState, value: String) {
    // Applied here as well as sent: the control that changes the model is the
    // one that tells the renderer, so a view never writes it (ARCHITECTURE.md).
    // The renderer's echo replaces it a moment later.
    state.inner.lock().unwrap().app.drc_mode = Some(value.clone());
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_INPUT_DRC_MODE.to_string(),
            value,
        },
    );
}

pub fn control_drc_weight(state: &SharedState, value: f32) {
    let clamped = value.clamp(0.0, 1.0);
    state.inner.lock().unwrap().app.drc_weight = Some(clamped);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: osc_contract::CONTROL_INPUT_DRC_WEIGHT.to_string(),
            value: clamped,
        },
    );
}

pub fn control_export_layout(state: &SharedState, name: Option<String>) {
    if let Some(raw) = name {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            send_control(
                &state.osc_tx,
                OscControlMsg::SendString {
                    address: osc_contract::CONTROL_LAYOUT_EXPORT.to_string(),
                    value: trimmed.to_string(),
                },
            );
            return;
        }
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: osc_contract::CONTROL_LAYOUT_EXPORT.to_string(),
        },
    );
}
