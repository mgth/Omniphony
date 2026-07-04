//! Engine-level controls: renderer config save/reload, log level, ramp mode,
//! dynamic-range-control (DRC) tuning and the layout-export trigger.
//!
//! Each command forwards a value to the renderer over OSC.

use crate::osc_listener::OscControlMsg;
use crate::{send_control, SharedState};
use tauri::State;

#[tauri::command]
pub fn control_save_config(state: State<SharedState>) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: "/omniphony/control/save_config".to_string(),
        },
    );
}

#[tauri::command]
pub fn control_reload_config(state: State<SharedState>) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: "/omniphony/control/reload_config".to_string(),
        },
    );
}

#[tauri::command]
pub fn control_log_level(state: State<SharedState>, value: String) {
    let trimmed = value.trim().to_ascii_lowercase();
    if !matches!(
        trimmed.as_str(),
        "off" | "error" | "warn" | "info" | "debug" | "trace"
    ) {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/log_level".to_string(),
            value: trimmed,
        },
    );
}

#[tauri::command]
pub fn control_ramp_mode(state: State<SharedState>, value: String) {
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
#[tauri::command]
pub fn control_option(state: State<SharedState>, key: String, value: serde_json::Value) {
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
#[tauri::command]
pub fn control_object_generator_param(state: State<SharedState>, key: String, value: f32) {
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
#[tauri::command]
pub fn control_phantom_extract_param(state: State<SharedState>, key: String, value: f32) {
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
#[tauri::command]
pub fn control_virtual_bed(state: State<SharedState>, value: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/virtual_bed".to_string(),
            value,
        },
    );
}

#[tauri::command]
pub fn control_drc_mode(state: State<SharedState>, value: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/input/drc_mode".to_string(),
            value,
        },
    );
}

#[tauri::command]
pub fn control_drc_weight(state: State<SharedState>, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/input/drc_weight".to_string(),
            value: value.clamp(0.0, 1.0),
        },
    );
}

#[tauri::command]
pub fn control_export_layout(state: State<SharedState>, name: Option<String>) {
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
