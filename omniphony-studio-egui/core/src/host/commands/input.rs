//! Audio input controls: bridged-input config and the live device/backend path
//! (node, layout, mapping, clock, LFE handling), plus importing
//! a layout file for the live input.
//!
//! Each command forwards a value to the renderer over OSC.

use super::OscControlMsg;
use super::{SharedState, send_control};
use crate::model::layouts;
use crate::osc_contract;
use std::fs;

pub fn control_input_config(state: &SharedState, payload: serde_json::Value) {
    let text = match serde_json::to_string(&payload) {
        Ok(text) => text,
        Err(_) => return,
    };
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_CONFIG_INPUT.to_string(),
            value: text,
        },
    );
}

pub fn control_input_config_apply(state: &SharedState) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: osc_contract::CONTROL_CONFIG_INPUT_APPLY.to_string(),
        },
    );
}

/// Canonical spelling of an input mode, or `None` if it is not one.
///
/// The protocol carries historical aliases — `bridge` for `pipe_bridge`, and
/// `live` / `pipewire_bridge` for `pipewire` (the names the PipeWire input
/// went by while a PCM-only sink still existed beside it). Both directions go
/// through here: the frontend used to re-implement this table when reading a
/// snapshot, which meant the same aliases were resolved in two places and only
/// one of them was the authority.
pub fn normalize_input_mode(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "bridge" | "pipe_bridge" => Some("pipe_bridge"),
        "live" | "pipewire" | "pipewire_bridge" => Some("pipewire"),
        _ => None,
    }
}

pub fn control_input_mode(state: &SharedState, value: String) {
    let Some(normalized) = normalize_input_mode(&value) else {
        return;
    };
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_INPUT_MODE.to_string(),
            value: normalized.to_string(),
        },
    );
}

pub fn control_input_live_backend(state: &SharedState, value: String) {
    let trimmed = value.trim().to_ascii_lowercase();
    if !matches!(trimmed.as_str(), "pipewire" | "asio") {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_INPUT_LIVE_BACKEND.to_string(),
            value: trimmed,
        },
    );
}

pub fn control_input_live_node(state: &SharedState, value: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_INPUT_LIVE_NODE.to_string(),
            value: value.trim().to_string(),
        },
    );
}

pub fn control_input_live_description(state: &SharedState, value: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_INPUT_LIVE_DESCRIPTION.to_string(),
            value: value.trim().to_string(),
        },
    );
}

pub fn control_input_live_layout(state: &SharedState, value: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_INPUT_LIVE_LAYOUT.to_string(),
            value: value.trim().to_string(),
        },
    );
}

pub fn import_input_layout_from_path(
    state: &SharedState,
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
            address: osc_contract::CONTROL_INPUT_LIVE_LAYOUT.to_string(),
            value: trimmed.to_string(),
        },
    );
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_INPUT_LIVE_LAYOUT_IMPORT.to_string(),
            value: contents,
        },
    );
    Ok(serde_json::json!({ "path": trimmed }))
}

pub fn control_input_live_channels(state: &SharedState, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: osc_contract::CONTROL_INPUT_LIVE_CHANNELS.to_string(),
            value: value.max(1),
        },
    );
}

pub fn control_input_live_sample_rate(state: &SharedState, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: osc_contract::CONTROL_INPUT_LIVE_SAMPLE_RATE.to_string(),
            value: value.max(1),
        },
    );
}

pub fn control_input_live_clock_mode(state: &SharedState, value: String) {
    let trimmed = value.trim().to_ascii_lowercase();
    if !matches!(trimmed.as_str(), "dac" | "pipewire" | "upstream") {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_INPUT_LIVE_CLOCK_MODE.to_string(),
            value: trimmed,
        },
    );
}

pub fn control_input_live_map(state: &SharedState, value: String) {
    let trimmed = value.trim().to_ascii_lowercase();
    if !matches!(trimmed.as_str(), "7.1-fixed") {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_INPUT_LIVE_MAP.to_string(),
            value: trimmed,
        },
    );
}

pub fn control_input_live_lfe_mode(state: &SharedState, value: String) {
    let trimmed = value.trim().to_ascii_lowercase();
    if !matches!(trimmed.as_str(), "object" | "direct" | "drop") {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_INPUT_LIVE_LFE_MODE.to_string(),
            value: trimmed,
        },
    );
}

/// `sendInputConfig`: the whole input document, as the renderer takes it,
/// built from the model. Every input control writes the model first and then
/// asks for the document, so the document has one shape in one place.
pub fn send_input_document(state: &SharedState, apply: bool) {
    let payload = {
        let live = state.inner.lock().unwrap();
        let input = &live.app.live_input;
        serde_json::json!({
            "mode": live.app.input_mode,
            "liveInput": {
                "backend": input.backend,
                "node": input.node,
                "description": input.description,
                "layout": input.layout,
                "clockMode": input.clock_mode.clone().unwrap_or_else(|| "dac".into()),
                "channels": input.channels.unwrap_or(2),
                "sampleRate": input.sample_rate.unwrap_or(192_000),
                "map": input.map.clone().unwrap_or_else(|| "7.1-fixed".into()),
                "lfeMode": input.lfe_mode.clone().unwrap_or_else(|| "object".into()),
            }
        })
    };
    control_input_config(state, payload);
    if apply {
        control_input_config_apply(state);
    }
}

/// The input mode. The PipeWire path comes with its own channel and rate
/// defaults, as the web sets them.
pub fn set_input_mode(state: &SharedState, mode: String) {
    {
        let mut live = state.inner.lock().unwrap();
        live.app.input_mode = Some(mode.clone());
        if mode == "pipewire" {
            live.app.live_input.channels = Some(2);
            live.app.live_input.sample_rate = Some(192_000);
        }
    }
    send_input_document(state, false);
}

/// The PipeWire node name; empty means "let the renderer choose".
pub fn set_live_input_node(state: &SharedState, node: String) {
    let trimmed = node.trim();
    state.inner.lock().unwrap().app.live_input.node =
        (!trimmed.is_empty()).then(|| trimmed.to_owned());
    send_input_document(state, false);
}

/// The PipeWire node description.
pub fn set_live_input_description(state: &SharedState, description: String) {
    let trimmed = description.trim();
    state.inner.lock().unwrap().app.live_input.description =
        (!trimmed.is_empty()).then(|| trimmed.to_owned());
    send_input_document(state, false);
}

/// The clock mode is held until Apply: it cannot change under a running
/// bridge, so this only records the choice.
pub fn set_live_input_clock_mode(state: &SharedState, clock: String) {
    state.inner.lock().unwrap().app.live_input.clock_mode = Some(clock);
}

/// The bridge library path; empty means "auto-detect".
pub fn set_render_bridge_path(state: &SharedState, path: String) {
    let value = path.trim().to_owned();
    state.inner.lock().unwrap().app.render_bridge_path = (!value.is_empty()).then(|| value.clone());
    super::render::control_render_bridge_path(state, value);
}

/// The named pipe the renderer reads; empty means "auto-detect".
pub fn set_orender_input_pipe(state: &SharedState, path: String) {
    let value = path.trim().to_owned();
    state.inner.lock().unwrap().app.orender_input_pipe = (!value.is_empty()).then(|| value.clone());
    super::render::control_render_input_pipe(state, value);
}

/// Apply: a bridge that has to be (re)started needs its path and clock saved
/// and the configuration reloaded; otherwise the input document is enough.
pub fn apply_input(state: &SharedState, mode: &str, active: Option<&str>) {
    let clock = {
        let live = state.inner.lock().unwrap();
        live.app
            .live_input
            .clock_mode
            .clone()
            .unwrap_or_else(|| "dac".to_owned())
    };
    let needs_bootstrap =
        mode == "pipe_bridge" || (mode == "pipewire" && active != Some("pipewire"));
    if needs_bootstrap {
        let bridge = {
            let live = state.inner.lock().unwrap();
            live.app.render_bridge_path.clone().unwrap_or_default()
        };
        super::render::control_render_bridge_path(state, bridge);
        control_input_live_clock_mode(state, clock);
        super::engine::control_save_config(state);
        super::engine::control_reload_config(state);
    } else {
        state.inner.lock().unwrap().app.input_apply_pending = Some(1);
        control_input_live_clock_mode(state, clock);
        send_input_document(state, true);
    }
}

pub fn control_input_apply(state: &SharedState) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: osc_contract::CONTROL_INPUT_APPLY.to_string(),
        },
    );
}
