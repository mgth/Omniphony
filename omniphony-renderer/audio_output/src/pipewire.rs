//! What the PipeWire output needs to know of the graph: the devices an
//! output can be opened on, how a stream is pinned to one, and the names
//! PipeWire gives the channel positions.

use anyhow::{Result, anyhow};
use pipewire as pw;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

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
}
