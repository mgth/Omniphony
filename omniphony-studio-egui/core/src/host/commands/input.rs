//! Audio input controls: which input the renderer reads (the bridge's pipe
//! or its own PipeWire sink), the sink's name, and the Apply that makes a
//! change take.
//!
//! Each command forwards a value to the renderer over OSC.

use super::OscControlMsg;
use super::{SharedState, send_control};
use crate::osc_contract;

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
                "node": input.node,
                "description": input.description,
            }
        })
    };
    control_input_config(state, payload);
    if apply {
        control_input_config_apply(state);
    }
}

/// The input mode: shown at once and sent, to take at the next Apply.
pub fn set_input_mode(state: &SharedState, mode: String) {
    state.inner.lock().unwrap().app.input_mode = Some(mode);
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

/// A single bridge library path; empty means "auto-detect". The list of one
/// it is, sent as the list to an engine that has one.
pub fn set_render_bridge_path(state: &SharedState, path: String) {
    super::bridges::set_render_bridge_paths(state, vec![path]);
}

/// The named pipe the renderer reads; empty means "auto-detect".
pub fn set_orender_input_pipe(state: &SharedState, path: String) {
    let value = path.trim().to_owned();
    state.inner.lock().unwrap().app.orender_input_pipe = (!value.is_empty()).then(|| value.clone());
    super::render::control_render_input_pipe(state, value);
}

/// Apply: the renderer reopens its capture in place, whatever changed (the
/// mode, the sink's name, the pipe), so the document and its apply are all it
/// takes — nothing restarts and nothing is saved.
pub fn apply_input(state: &SharedState) {
    state.inner.lock().unwrap().app.input_apply_pending = Some(1);
    send_input_document(state, true);
}

pub fn control_input_apply(state: &SharedState) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: osc_contract::CONTROL_INPUT_APPLY.to_string(),
        },
    );
}

#[cfg(test)]
mod apply_tests {
    use super::*;
    use crate::host::commands::tests::{sent_addresses, state_with_outbox};

    /// The renderer reopens its capture in place: the document and its
    /// apply, with no restart, and nothing Apply does writes the config.
    #[test]
    fn an_apply_sends_the_document_and_restarts_nothing() {
        let (state, rx) = state_with_outbox(std::sync::Arc::new(|| {}));
        apply_input(&state);
        assert_eq!(
            sent_addresses(&rx),
            [
                osc_contract::CONTROL_CONFIG_INPUT,
                osc_contract::CONTROL_CONFIG_INPUT_APPLY
            ]
        );
        assert_eq!(state.read().app.input_apply_pending, Some(1));
    }

    /// The document carries what the renderer's input takes: the mode and
    /// its sink's name, nothing of the shape the old capture needed.
    #[test]
    fn the_document_is_the_mode_and_the_sink() {
        let (state, rx) = state_with_outbox(std::sync::Arc::new(|| {}));
        set_input_mode(&state, "pipewire".into());
        set_live_input_node(&state, " my-sink ".into());
        let last = rx
            .try_iter()
            .filter_map(|control| match control {
                crate::osc::Control::Send { address, args }
                    if address == osc_contract::CONTROL_CONFIG_INPUT =>
                {
                    match args.first() {
                        Some(rosc::OscType::String(text)) => Some(text.clone()),
                        _ => None,
                    }
                }
                _ => None,
            })
            .last()
            .expect("a document was sent");
        let document: serde_json::Value = serde_json::from_str(&last).unwrap();
        assert_eq!(
            document,
            serde_json::json!({
                "mode": "pipewire",
                "liveInput": { "node": "my-sink", "description": null }
            })
        );
    }
}
