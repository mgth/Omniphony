//! Audio input controls: bridged-input config and the live device/backend path
//! (node, layout, mapping, clock, LFE handling), plus importing
//! a layout file for the live input.
//!
//! Each command forwards a value to the renderer over OSC.

use crate::layouts;
use crate::osc_listener::OscControlMsg;
use crate::{send_control, SharedState};
use std::fs;
use tauri::State;

#[tauri::command]
pub fn control_input_config(state: State<SharedState>, payload: serde_json::Value) {
    let text = match serde_json::to_string(&payload) {
        Ok(text) => text,
        Err(_) => return,
    };
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/config/input".to_string(),
            value: text,
        },
    );
}

#[tauri::command]
pub fn control_input_config_apply(state: State<SharedState>) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: "/omniphony/control/config/input/apply".to_string(),
        },
    );
}

/// Canonical spelling of an input mode, or `None` if it is not one.
///
/// The protocol carries historical aliases — `bridge` for `pipe_bridge`, and
/// `live` / `pipewire` (the removed PCM-only sink) for `pipewire_bridge`, the
/// sink that replaced it. Both directions go through here: the frontend used
/// to re-implement this table when reading a snapshot, which meant the same
/// aliases were resolved in two places and only one of them was the
/// authority.
pub fn normalize_input_mode(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "bridge" | "pipe_bridge" => Some("pipe_bridge"),
        "live" | "pipewire" | "pipewire_bridge" => Some("pipewire_bridge"),
        _ => None,
    }
}

#[tauri::command]
pub fn control_input_mode(state: State<SharedState>, value: String) {
    let Some(normalized) = normalize_input_mode(&value) else {
        return;
    };
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/input/mode".to_string(),
            value: normalized.to_string(),
        },
    );
}

#[tauri::command]
pub fn control_input_live_backend(state: State<SharedState>, value: String) {
    let trimmed = value.trim().to_ascii_lowercase();
    if !matches!(trimmed.as_str(), "pipewire" | "asio") {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/input/live/backend".to_string(),
            value: trimmed,
        },
    );
}

#[tauri::command]
pub fn control_input_live_node(state: State<SharedState>, value: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/input/live/node".to_string(),
            value: value.trim().to_string(),
        },
    );
}

#[tauri::command]
pub fn control_input_live_description(state: State<SharedState>, value: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/input/live/description".to_string(),
            value: value.trim().to_string(),
        },
    );
}

#[tauri::command]
pub fn control_input_live_layout(state: State<SharedState>, value: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/input/live/layout".to_string(),
            value: value.trim().to_string(),
        },
    );
}

#[tauri::command]
pub fn import_input_layout_from_path(
    state: State<SharedState>,
    path: String,
) -> Result<serde_json::Value, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("empty layout path".to_string());
    }
    layouts::load_layout_file(std::path::Path::new(trimmed))
        .ok_or_else(|| "failed to parse layout file".to_string())?;
    let contents =
        fs::read_to_string(trimmed).map_err(|e| format!("failed to read layout file: {e}"))?;
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/input/live/layout".to_string(),
            value: trimmed.to_string(),
        },
    );
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/input/live/layout_import".to_string(),
            value: contents,
        },
    );
    Ok(serde_json::json!({ "path": trimmed }))
}

#[tauri::command]
pub fn control_input_live_channels(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/input/live/channels".to_string(),
            value: value.max(1),
        },
    );
}

#[tauri::command]
pub fn control_input_live_sample_rate(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/input/live/sample_rate".to_string(),
            value: value.max(1),
        },
    );
}

#[tauri::command]
pub fn control_input_live_clock_mode(state: State<SharedState>, value: String) {
    let trimmed = value.trim().to_ascii_lowercase();
    if !matches!(trimmed.as_str(), "dac" | "pipewire" | "upstream") {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/input/live/clock_mode".to_string(),
            value: trimmed,
        },
    );
}

#[tauri::command]
pub fn control_input_live_map(state: State<SharedState>, value: String) {
    let trimmed = value.trim().to_ascii_lowercase();
    if !matches!(trimmed.as_str(), "7.1-fixed") {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/input/live/map".to_string(),
            value: trimmed,
        },
    );
}

#[tauri::command]
pub fn control_input_live_lfe_mode(state: State<SharedState>, value: String) {
    let trimmed = value.trim().to_ascii_lowercase();
    if !matches!(trimmed.as_str(), "object" | "direct" | "drop") {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/input/live/lfe_mode".to_string(),
            value: trimmed,
        },
    );
}

#[tauri::command]
pub fn control_input_apply(state: State<SharedState>) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: "/omniphony/control/input/apply".to_string(),
        },
    );
}
