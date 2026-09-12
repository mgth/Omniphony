//! Engine-level controls: renderer config save/reload, log level, ramp mode,
//! dynamic-range-control (DRC) tuning and the layout-export trigger.
//!
//! Each command forwards a value to the renderer over OSC.

use super::OscControlMsg;
use super::{SharedState, send_control};

pub fn control_save_config(state: &SharedState) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: "/omniphony/control/save_config".to_string(),
        },
    );
}

pub fn control_reload_config(state: &SharedState) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: "/omniphony/control/reload_config".to_string(),
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
            address: "/omniphony/control/log_level".to_string(),
            value: trimmed,
        },
    );
}

pub fn control_ramp_mode(state: &SharedState, value: String) {
    let trimmed = value.trim().to_ascii_lowercase();
    if !matches!(trimmed.as_str(), "off" | "frame" | "sample") {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/ramp_mode".to_string(),
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
pub fn control_option(state: &SharedState, key: String, value: serde_json::Value) {
    let k = key.trim().to_ascii_lowercase();
    if k.is_empty() {
        return;
    }
    let arg = match value {
        serde_json::Value::String(s) => rosc::OscType::String(s.trim().to_ascii_lowercase()),
        serde_json::Value::Bool(b) => rosc::OscType::Int(if b { 1 } else { 0 }),
        serde_json::Value::Number(n) => match n.as_f64() {
            Some(f) if f.is_finite() => rosc::OscType::Float(f as f32),
            _ => return,
        },
        _ => return,
    };
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: "/omniphony/control/option".to_string(),
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
            address: "/omniphony/control/object_generator/param".to_string(),
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
            address: "/omniphony/control/phantom_extract/param".to_string(),
            args: vec![rosc::OscType::String(k), rosc::OscType::Float(value)],
        },
    );
}

/// Set the parametrable virtual bed (a YAML `SpeakerLayout`, one entry per
/// channel label). An empty string resets to the built-in canonical poses.
pub fn control_virtual_bed(state: &SharedState, value: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/virtual_bed".to_string(),
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
            address: "/omniphony/control/input/drc_mode".to_string(),
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
            address: "/omniphony/control/input/drc_weight".to_string(),
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
                    address: "/omniphony/control/layout/export".to_string(),
                    value: trimmed.to_string(),
                },
            );
            return;
        }
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: "/omniphony/control/layout/export".to_string(),
        },
    );
}
