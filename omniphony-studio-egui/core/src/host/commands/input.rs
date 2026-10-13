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

/// The option that makes the renderer's sink a carrier sink
/// (`docs/iamf-transport-plan.md`): its channels, or unset for the sink that
/// takes audio.
const CARRIER_CHANNELS_OPTION: &str = "live_input_carrier_channels";
/// The channels a carrier sink can have, as the renderer bounds them, and
/// what switching one on asks for: the width of the sender's default.
pub const CARRIER_CHANNELS: std::ops::RangeInclusive<u16> = 4..=64;
pub const DEFAULT_CARRIER_CHANNELS: u16 = 8;

/// Whether the renderer's sink can be made a carrier sink: its engine
/// declares the option. One that does not would take the write for nothing.
pub fn carrier_sink_offered(state: &SharedState) -> bool {
    state.read().declares_option(CARRIER_CHANNELS_OPTION)
}

/// The carrier sink asked for, by its channels; `None` is the sink that
/// takes audio.
pub fn carrier_channels(state: &SharedState) -> Option<u16> {
    state
        .read()
        .host_option_f64(CARRIER_CHANNELS_OPTION)
        .map(|channels| channels.round() as u16)
        .filter(|channels| CARRIER_CHANNELS.contains(channels))
}

/// The channels of the carrier sink that runs, when the input in use is one.
///
/// A carrier sink states its channels in the applied input's `channels`.
/// So does the regular PipeWire sink of an engine from before the carrier
/// sink existed: the field means a carrier only from an engine that
/// declares the option.
pub fn running_carrier_channels(state: &SharedState) -> Option<u32> {
    let live = state.read();
    (live.declares_option(CARRIER_CHANNELS_OPTION)
        && live.app.input_active_mode.as_deref() == Some("pipewire"))
    .then_some(live.app.input_channels)
    .flatten()
}

/// Ask for a carrier sink of `channels` (bounded), or with `None` for the
/// sink that takes audio: shown at once and sent, to take at the next Apply
/// like the rest of the input.
pub fn set_carrier_channels(state: &SharedState, channels: Option<u16>) {
    let channels =
        channels.map(|channels| channels.clamp(*CARRIER_CHANNELS.start(), *CARRIER_CHANNELS.end()));
    state.inner.lock().unwrap().set_host_option(
        CARRIER_CHANNELS_OPTION,
        channels.map_or(serde_json::Value::Null, |channels| {
            serde_json::json!(channels)
        }),
    );
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: osc_contract::CONTROL_OPTION.to_string(),
            args: vec![
                rosc::OscType::String(CARRIER_CHANNELS_OPTION.to_owned()),
                // (No value unsets an optional option.)
                channels.map_or(rosc::OscType::Nil, |channels| {
                    rosc::OscType::Int(i32::from(channels))
                }),
            ],
        },
    );
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

    /// A carrier sink is asked for through the option, bounded as the
    /// renderer bounds it, and unset with no value; the model shows what was
    /// asked at once. It is offered only by an engine that declares it.
    #[test]
    fn a_carrier_sink_is_an_option_write() {
        let (state, rx) = state_with_outbox(std::sync::Arc::new(|| {}));
        assert!(!carrier_sink_offered(&state));
        state.inner.lock().unwrap().options_schema = Some(serde_json::json!([
            {"key": CARRIER_CHANNELS_OPTION, "group": {"key": "live_input", "mode": "staged"}}
        ]));
        assert!(carrier_sink_offered(&state));
        assert_eq!(carrier_channels(&state), None);

        let sent = |rx: &std::sync::mpsc::Receiver<crate::osc::Control>| {
            rx.try_iter()
                .filter_map(|control| match control {
                    crate::osc::Control::Send { address, args }
                        if address == osc_contract::CONTROL_OPTION =>
                    {
                        Some(args)
                    }
                    _ => None,
                })
                .last()
                .expect("an option was written")
        };
        set_carrier_channels(&state, Some(16));
        assert_eq!(carrier_channels(&state), Some(16));
        assert_eq!(
            sent(&rx),
            [
                rosc::OscType::String(CARRIER_CHANNELS_OPTION.into()),
                rosc::OscType::Int(16)
            ]
        );
        set_carrier_channels(&state, Some(500));
        assert_eq!(carrier_channels(&state), Some(64));
        set_carrier_channels(&state, Some(1));
        assert_eq!(carrier_channels(&state), Some(4));
        set_carrier_channels(&state, None);
        assert_eq!(carrier_channels(&state), None);
        assert_eq!(
            sent(&rx),
            [
                rosc::OscType::String(CARRIER_CHANNELS_OPTION.into()),
                rosc::OscType::Nil
            ]
        );
    }

    /// The sink that runs is a carrier sink when an engine that has one says
    /// so by its channels. An older engine states its regular sink's channels
    /// in the same field of the input state: that is not a carrier.
    #[test]
    fn only_an_engine_with_a_carrier_sink_runs_one() {
        let (state, _rx) = state_with_outbox(std::sync::Arc::new(|| {}));
        let legacy = r#"{"mode":"pipewire","activeMode":"pipewire",
            "applied":{"backend":"pipewire","channels":2,"sampleRate":48000}}"#;
        assert!(crate::osc::apply::apply_input_domain_state(
            &mut state.inner.lock().unwrap().app,
            legacy
        ));
        assert_eq!(running_carrier_channels(&state), None);

        state.inner.lock().unwrap().options_schema = Some(serde_json::json!([
            {"key": CARRIER_CHANNELS_OPTION, "group": {"key": "live_input", "mode": "staged"}}
        ]));
        assert_eq!(running_carrier_channels(&state), Some(2));
        // Its audio sink states none, and the pipe is no sink at all.
        for snapshot in [
            r#"{"activeMode":"pipewire","applied":{"channels":null}}"#,
            r#"{"activeMode":"pipe_bridge","applied":{"channels":8}}"#,
        ] {
            crate::osc::apply::apply_input_domain_state(
                &mut state.inner.lock().unwrap().app,
                snapshot,
            );
            assert_eq!(running_carrier_channels(&state), None, "{snapshot}");
        }
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
