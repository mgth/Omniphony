//! What the PipeWire output needs to know of the graph: the devices an
//! output can be opened on, how a stream is pinned to one, and the names
//! PipeWire gives the channel positions.

use anyhow::{Result, anyhow};
use pipewire as pw;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::pipewire_registry::{
    ClientEntry, client_from_props, connect_main_loop, non_empty, owner_pid, registry_snapshot,
};

/// Convert speaker name to PipeWire channel position name
/// PipeWire expects lowercase positions like "FL", "FR", "FC", "LFE", "RL", "RR", etc.
pub(crate) fn to_pipewire_position(name: &str) -> String {
    match name {
        "C" => "FC".to_string(),    // Center → Front-Center
        "BL" => "RL".to_string(),   // Back-Left → Rear-Left
        "BR" => "RR".to_string(),   // Back-Right → Rear-Right
        "BC" => "RC".to_string(),   // Back-Center → Rear-Center
        other => other.to_string(), // FL, FR, LFE, SL, SR, etc. stay as-is
    }
}

/// Properties that pin the output stream to `target`.
///
/// Four properties, because stating the device once is not enough:
///
/// - `target.object` is what WirePlumber 0.5 resolves first; `node.target` is
///   the deprecated spelling it only falls back to, so both are stated.
/// - `node.dont-move` makes the session manager ignore a `target.node` entry
///   in the default metadata. Any mixer offering "play on the default device"
///   writes `target.node = -1` there, and that entry outranks both properties
///   above — the stream then leaves the requested device without a word.
/// - `node.dont-fallback` forbids the consolation prize: when the requested
///   device cannot be linked, the stream must stay unlinked rather than land
///   on the default sink.
///
/// The last two matter because of what the default sink can be. Omniphony
/// publishes its own bridge input sink, and a machine that plays through
/// Omniphony has it set as the default: falling back there loops the rendered
/// output straight back into the decoder input, and — since that node drives
/// the graph it is triggered by — hands the output stream a clock that only
/// this stream keeps alive. The ring then stops draining altogether, which
/// reads as an endless "Buffer drain timeout" and total silence.
pub(crate) fn output_target_properties(target: &str) -> [(&'static str, &str); 4] {
    [
        ("target.object", target),
        ("node.target", target),
        ("node.dont-move", "true"),
        ("node.dont-fallback", "true"),
    ]
}

/// Bound on the registry round trip behind the device list: a wedged daemon
/// must not hang the caller (the control surface asks for this list).
const DEVICE_LIST_TIMEOUT: Duration = Duration::from_secs(2);

/// An `Audio/Sink` node offered as an output device, with its owning client.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SinkCandidate {
    /// `node.name`, what `target.object` is set to.
    value: String,
    /// Human-readable label: description, nick, or device name.
    label: String,
    client_id: Option<u32>,
}

/// Reads a registry `Node` global: `Some` only for a named `Audio/Sink`.
fn sink_candidate_from_props<'a>(get: impl Fn(&str) -> Option<&'a str>) -> Option<SinkCandidate> {
    if get(*pw::keys::MEDIA_CLASS)? != "Audio/Sink" {
        return None;
    }
    let value = non_empty(get(*pw::keys::NODE_NAME))?;
    let label = non_empty(get(*pw::keys::NODE_DESCRIPTION))
        .or_else(|| non_empty(get(*pw::keys::NODE_NICK)))
        .or_else(|| non_empty(get(*pw::keys::DEVICE_DESCRIPTION)))
        .or_else(|| non_empty(get(*pw::keys::DEVICE_NAME)))
        .unwrap_or(value);
    Some(SinkCandidate {
        value: value.to_string(),
        label: label.to_string(),
        client_id: non_empty(get(*pw::keys::CLIENT_ID)).and_then(|v| v.parse().ok()),
    })
}

/// The `(node.name, label)` list offered for output, sorted by label.
///
/// Sinks this very process publishes are left out: that is Omniphony's own
/// bridge input sink, and rendering into it loops the output straight back
/// into the decoder input, with a clock only this output stream keeps alive
/// (see [`output_target_properties`]). Offering it would only let the user
/// pick the one device that cannot work.
fn output_device_list(
    sinks: &[SinkCandidate],
    clients: &[ClientEntry],
    own_pid: u32,
) -> Vec<(String, String)> {
    let mut devices: Vec<(String, String)> = sinks
        .iter()
        .filter(|sink| owner_pid(sink.client_id, clients) != Some(own_pid))
        .map(|sink| (sink.value.clone(), sink.label.clone()))
        .collect();
    devices.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    devices.dedup_by(|a, b| a.0 == b.0);
    devices
}

pub fn list_pipewire_output_devices() -> Result<Vec<(String, String)>> {
    let conn = connect_main_loop()?;
    let sinks = Rc::new(RefCell::new(Vec::<SinkCandidate>::new()));
    let clients = Rc::new(RefCell::new(Vec::<ClientEntry>::new()));
    let sinks_for_registry = Rc::clone(&sinks);
    let clients_for_registry = Rc::clone(&clients);
    let answered = registry_snapshot(
        &conn.mainloop,
        &conn.core,
        DEVICE_LIST_TIMEOUT,
        move |global| {
            let Some(props) = global.props.as_ref() else {
                return;
            };
            match global.type_ {
                pw::types::ObjectType::Node => {
                    if let Some(sink) = sink_candidate_from_props(|key| props.get(key)) {
                        sinks_for_registry.borrow_mut().push(sink);
                    }
                }
                pw::types::ObjectType::Client => {
                    clients_for_registry
                        .borrow_mut()
                        .push(client_from_props(global.id, |key| props.get(key)));
                }
                _ => {}
            }
        },
    )?;
    if !answered {
        return Err(anyhow!(
            "PipeWire registry did not answer within {DEVICE_LIST_TIMEOUT:?}"
        ));
    }
    Ok(output_device_list(
        &sinks.borrow(),
        &clients.borrow(),
        std::process::id(),
    ))
}

/// The PipeWire channel behind a position name (`FL`, `TSL`, `AUX3`), as a
/// format's `position` array states it; `None` for a name PipeWire does not
/// know: a sink whose map reads `1 2 3 …`, a layout's own `FWL`.
pub(crate) fn channel_position_id(name: &str) -> Option<u32> {
    use pw::spa::sys as spa;
    let id = match name {
        "MONO" => spa::SPA_AUDIO_CHANNEL_MONO,
        "FL" => spa::SPA_AUDIO_CHANNEL_FL,
        "FR" => spa::SPA_AUDIO_CHANNEL_FR,
        "FC" => spa::SPA_AUDIO_CHANNEL_FC,
        "LFE" => spa::SPA_AUDIO_CHANNEL_LFE,
        "SL" => spa::SPA_AUDIO_CHANNEL_SL,
        "SR" => spa::SPA_AUDIO_CHANNEL_SR,
        "FLC" => spa::SPA_AUDIO_CHANNEL_FLC,
        "FRC" => spa::SPA_AUDIO_CHANNEL_FRC,
        "RC" => spa::SPA_AUDIO_CHANNEL_RC,
        "RL" => spa::SPA_AUDIO_CHANNEL_RL,
        "RR" => spa::SPA_AUDIO_CHANNEL_RR,
        "TC" => spa::SPA_AUDIO_CHANNEL_TC,
        "TFL" => spa::SPA_AUDIO_CHANNEL_TFL,
        "TFC" => spa::SPA_AUDIO_CHANNEL_TFC,
        "TFR" => spa::SPA_AUDIO_CHANNEL_TFR,
        "TRL" => spa::SPA_AUDIO_CHANNEL_TRL,
        "TRC" => spa::SPA_AUDIO_CHANNEL_TRC,
        "TRR" => spa::SPA_AUDIO_CHANNEL_TRR,
        "RLC" => spa::SPA_AUDIO_CHANNEL_RLC,
        "RRC" => spa::SPA_AUDIO_CHANNEL_RRC,
        "FLW" => spa::SPA_AUDIO_CHANNEL_FLW,
        "FRW" => spa::SPA_AUDIO_CHANNEL_FRW,
        "LFE2" => spa::SPA_AUDIO_CHANNEL_LFE2,
        "FLH" => spa::SPA_AUDIO_CHANNEL_FLH,
        "FCH" => spa::SPA_AUDIO_CHANNEL_FCH,
        "FRH" => spa::SPA_AUDIO_CHANNEL_FRH,
        "TFLC" => spa::SPA_AUDIO_CHANNEL_TFLC,
        "TFRC" => spa::SPA_AUDIO_CHANNEL_TFRC,
        "TSL" => spa::SPA_AUDIO_CHANNEL_TSL,
        "TSR" => spa::SPA_AUDIO_CHANNEL_TSR,
        "LLFE" => spa::SPA_AUDIO_CHANNEL_LLFE,
        "RLFE" => spa::SPA_AUDIO_CHANNEL_RLFE,
        "BC" => spa::SPA_AUDIO_CHANNEL_BC,
        "BLC" => spa::SPA_AUDIO_CHANNEL_BLC,
        "BRC" => spa::SPA_AUDIO_CHANNEL_BRC,
        aux => {
            // `AUX0`…`AUX63`, the ones PipeWire has names for.
            let n: u32 = aux.strip_prefix("AUX")?.parse().ok()?;
            if n > 63 {
                return None;
            }
            spa::SPA_AUDIO_CHANNEL_START_Aux + n
        }
    };
    Some(id)
}

/// The names in an `audio.position` property, as the registry shows a
/// node's (`[ FL FR ]`) or as a stream states its own (`FL,FR`).
pub(crate) fn parse_audio_position(value: &str) -> Vec<String> {
    value
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | '[' | ']' | '"'))
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The channel map of the `Audio/Sink` named `target`: its `audio.position`,
/// in port order. `None` when the sink is not there, or states no map.
///
/// The registry announces a node with a few of its properties only, the map
/// not among them: the sink is found in a snapshot, then bound and its own
/// info read, a round trip more than the device list.
pub fn sink_channel_map(target: &str) -> Result<Option<Vec<String>>> {
    let conn = connect_main_loop()?;
    let node_id = Rc::new(Cell::new(None::<u32>));
    let answered = {
        let node_id = Rc::clone(&node_id);
        let target = target.to_owned();
        registry_snapshot(
            &conn.mainloop,
            &conn.core,
            DEVICE_LIST_TIMEOUT,
            move |global| {
                if node_id.get().is_some() || global.type_ != pw::types::ObjectType::Node {
                    return;
                }
                let Some(props) = global.props.as_ref() else {
                    return;
                };
                if props.get(*pw::keys::MEDIA_CLASS) == Some("Audio/Sink")
                    && non_empty(props.get(*pw::keys::NODE_NAME)) == Some(target.as_str())
                {
                    node_id.set(Some(global.id));
                }
            },
        )?
    };
    if !answered {
        return Err(anyhow!(
            "PipeWire registry did not answer within {DEVICE_LIST_TIMEOUT:?}"
        ));
    }
    let Some(id) = node_id.get() else {
        return Ok(None);
    };

    let registry = conn
        .core
        .get_registry()
        .map_err(|e| anyhow!("Failed to get PipeWire registry: {e:?}"))?;
    let node: pw::node::Node = registry
        .bind(&pw::registry::GlobalObject {
            id,
            permissions: pw::permissions::PermissionFlags::empty(),
            type_: pw::types::ObjectType::Node,
            version: 0,
            props: None::<&pw::spa::utils::dict::DictRef>,
        })
        .map_err(|e| anyhow!("Failed to bind the sink {target}: {e:?}"))?;
    // Queued after the bind: its `done` lands after the node's info, or
    // without it when the node went away in between.
    let pending = conn
        .core
        .sync(0)
        .map_err(|e| anyhow!("PipeWire sync failed: {e:?}"))?;
    let done = Rc::new(Cell::new(false));
    let map = Rc::new(RefCell::new(None::<Vec<String>>));
    let _core_listener = {
        let done = Rc::clone(&done);
        conn.core
            .add_listener_local()
            .done(move |id, seq| {
                if id == pw::core::PW_ID_CORE && seq == pending {
                    done.set(true);
                }
            })
            .register()
    };
    let _node_listener = {
        let (map, done) = (Rc::clone(&map), Rc::clone(&done));
        node.add_listener_local()
            .info(move |info| {
                *map.borrow_mut() = info
                    .props()
                    .and_then(|props| props.get("audio.position"))
                    .map(parse_audio_position)
                    .filter(|names| !names.is_empty());
                done.set(true);
            })
            .register()
    };
    let deadline = Instant::now() + DEVICE_LIST_TIMEOUT;
    while !done.get() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(anyhow!(
                "PipeWire did not answer for the sink {target} within {DEVICE_LIST_TIMEOUT:?}"
            ));
        }
        let _ = conn
            .mainloop
            .loop_()
            .iterate(remaining.min(Duration::from_millis(50)));
    }
    let map = map.borrow().clone();
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sink(value: &str, label: &str, client_id: Option<u32>) -> SinkCandidate {
        SinkCandidate {
            value: value.into(),
            label: label.into(),
            client_id,
        }
    }

    fn client(id: u32, pid: Option<u32>) -> ClientEntry {
        ClientEntry {
            id,
            pid,
            binary: None,
        }
    }

    /// Omniphony's own bridge sink (published by this process) is not an
    /// output device: rendering into it would feed the output back into
    /// the decoder input. Every other sink stays, including one whose owner
    /// cannot be resolved.
    #[test]
    fn device_list_leaves_out_this_processes_own_sinks() {
        let own_pid = 4242;
        let sinks = [
            sink("omniphony", "Omniphony", Some(7)),
            sink("alsa_output.dac", "USB DAC", Some(8)),
            sink("other_renderer", "Omniphony (other)", Some(9)),
            sink("loaded_by_daemon", "Null sink", None),
        ];
        let clients = [
            client(7, Some(own_pid)),
            client(8, Some(100)),
            client(9, Some(200)),
        ];
        let list = output_device_list(&sinks, &clients, own_pid);
        assert_eq!(
            list,
            vec![
                ("loaded_by_daemon".to_string(), "Null sink".to_string()),
                (
                    "other_renderer".to_string(),
                    "Omniphony (other)".to_string()
                ),
                ("alsa_output.dac".to_string(), "USB DAC".to_string()),
            ]
        );
    }

    /// Talks to the session's PipeWire daemon: run with `--ignored` to see
    /// the list a renderer would offer. Read-only (one registry snapshot).
    #[test]
    #[ignore = "needs a running PipeWire session"]
    fn live_device_list_answers_within_the_timeout() {
        let devices = list_pipewire_output_devices().expect("device list");
        for (value, label) in &devices {
            eprintln!("{value}\t{label}");
        }
    }

    #[test]
    fn device_list_sorts_by_label_and_drops_duplicate_names() {
        let sinks = [
            sink("b", "Beta", None),
            sink("a", "Alpha", None),
            sink("b", "Beta", None),
        ];
        let list = output_device_list(&sinks, &[], 1);
        assert_eq!(
            list,
            vec![
                ("a".to_string(), "Alpha".to_string()),
                ("b".to_string(), "Beta".to_string()),
            ]
        );
    }

    #[test]
    fn sink_candidates_are_named_audio_sinks_with_a_label_fallback() {
        let props = |pairs: &'static [(&'static str, &'static str)]| {
            move |key: &str| pairs.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
        };
        let s = sink_candidate_from_props(props(&[
            ("media.class", "Audio/Sink"),
            ("node.name", " dac "),
            ("node.nick", "DAC nick"),
            ("client.id", "12"),
        ]))
        .expect("a named sink");
        assert_eq!(s, sink("dac", "DAC nick", Some(12)));
        assert!(
            sink_candidate_from_props(props(&[
                ("media.class", "Audio/Source"),
                ("node.name", "mic"),
            ]))
            .is_none()
        );
        assert!(
            sink_candidate_from_props(props(&[("media.class", "Audio/Sink"), ("node.name", " ")]))
                .is_none()
        );
    }

    #[test]
    fn output_target_is_stated_in_both_spellings() {
        let props = output_target_properties("bluez_output.AA_BB_CC.1");
        assert_eq!(
            props[0],
            ("target.object", "bluez_output.AA_BB_CC.1"),
            "WirePlumber 0.5 resolves target.object first"
        );
        assert_eq!(props[1], ("node.target", "bluez_output.AA_BB_CC.1"));
    }

    #[test]
    fn output_target_refuses_moves_and_default_fallback() {
        let props = output_target_properties("alsa_output.pci-0000_00_1f.3.analog-stereo");
        assert!(props.contains(&("node.dont-move", "true")));
        assert!(props.contains(&("node.dont-fallback", "true")));
    }
    /// The names PipeWire knows, and the ones it does not: a map of
    /// `1 2 3 …` tags nothing, and so does a layout's own `FWL`.
    #[test]
    fn channel_positions_are_the_names_pipewire_knows() {
        use pw::spa::sys as spa;
        assert_eq!(channel_position_id("FL"), Some(spa::SPA_AUDIO_CHANNEL_FL));
        assert_eq!(channel_position_id("TSL"), Some(spa::SPA_AUDIO_CHANNEL_TSL));
        assert_eq!(
            channel_position_id("AUX0"),
            Some(spa::SPA_AUDIO_CHANNEL_AUX0)
        );
        assert_eq!(
            channel_position_id("AUX7"),
            Some(spa::SPA_AUDIO_CHANNEL_AUX7)
        );
        assert_eq!(
            channel_position_id("AUX63"),
            Some(spa::SPA_AUDIO_CHANNEL_AUX63)
        );
        for unknown in ["AUX64", "AUX", "AUX-1", "1", "FWL", "UNK", "", "fl"] {
            assert_eq!(channel_position_id(unknown), None, "{unknown}");
        }
    }

    /// Both spellings of a channel map: the registry's and a stream's.
    #[test]
    fn audio_position_is_read_in_both_spellings() {
        assert_eq!(parse_audio_position("[ FL FR FC ]"), ["FL", "FR", "FC"]);
        assert_eq!(parse_audio_position("FL,FR"), ["FL", "FR"]);
        assert_eq!(
            parse_audio_position("[ \"AUX0\", \"AUX1\" ]"),
            ["AUX0", "AUX1"]
        );
        assert_eq!(parse_audio_position("[ 1 2 ]"), ["1", "2"]);
        assert!(parse_audio_position("[ ]").is_empty());
    }

    /// Talks to the session's PipeWire daemon: run with `--ignored` to see
    /// the map a named sink states. Read-only (one registry snapshot).
    #[test]
    #[ignore = "needs a running PipeWire session"]
    fn live_sink_channel_map_answers_within_the_timeout() {
        for (name, _) in list_pipewire_output_devices().expect("device list") {
            eprintln!(
                "{name}\t{:?}",
                sink_channel_map(&name).expect("channel map")
            );
        }
    }
}
