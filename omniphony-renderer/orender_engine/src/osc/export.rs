use std::net::UdpSocket;

use super::peer::Peer;
use std::path::PathBuf;
use std::sync::Arc;

use renderer::live_params::RendererControl;
use rosc::{OscBundle, OscMessage, OscPacket, OscType};
use runtime_control::HostControlHandler;

use super::client_registry::OscClientRegistry;
use super::transport::{
    STATE_TIMETAG, broadcast_int, broadcast_string, send_raw, state_generation_message,
};
use runtime_control::osc_contract;

/// Largest payload one UDP datagram carries over IPv4 (65 535 bytes minus the
/// IP and UDP headers), with room to spare for the bundle framing.
pub(crate) const MAX_STATE_DATAGRAM: usize = 65_000;

/// Largest snapshot packet sent to a stream client: the whole snapshot in
/// one bundle, short of a pathological one, within what both ends accept
/// (`osc_contract::stream::MAX_PACKET`).
pub(crate) const MAX_STATE_STREAM_PACKET: usize = runtime_control::osc_contract::stream::MAX_PACKET;

/// Broadcast the live-state snapshot to every registered client.
///
/// Captured, numbered and sent under the publication lock (see
/// [`OscClientRegistry::publish`]): a snapshot read before another thread
/// published a newer state cannot go out after it under a higher generation.
pub(crate) fn broadcast_live_state(
    control: &Arc<RendererControl>,
    host: Option<&Arc<dyn HostControlHandler>>,
    socket: &UdpSocket,
    clients: &OscClientRegistry,
) {
    clients.publish(|generation| {
        let generation = generation.advance();
        let messages = live_state_messages(control, host);
        for bytes in encode_snapshot(messages, generation, MAX_STATE_DATAGRAM) {
            send_raw(socket, clients, &bytes);
        }
    });
}

/// Send the live-state snapshot to one client (registration, refresh), with
/// the current generation: it tells that client where it stands and moves
/// nothing for the others. Under the publication lock like a broadcast, so
/// the generation is the one of the state it captured.
pub(crate) fn send_live_state_to(
    control: &Arc<RendererControl>,
    host: Option<&Arc<dyn HostControlHandler>>,
    socket: &UdpSocket,
    clients: &OscClientRegistry,
    client: &Peer,
) {
    // A stream carries the snapshot whole: no datagram to fit it in (#680).
    let max = if client.is_stream() {
        MAX_STATE_STREAM_PACKET
    } else {
        MAX_STATE_DATAGRAM
    };
    clients.publish(|generation| {
        let generation = generation.current();
        let messages = live_state_messages(control, host);
        for bytes in encode_snapshot(messages, generation, max) {
            if let Err(e) = client.send(socket, &bytes) {
                log::warn!(
                    "Failed to send live state ({} bytes) to {}: {}",
                    bytes.len(),
                    client,
                    e
                );
            }
        }
    });
}

/// The live-state snapshot encoded for the wire: one OSC bundle when it fits
/// `max` bytes, consecutive bundles when it does not (a long device list, a
/// bridge error report…). Each bundle stays under `max`; a single message
/// larger than that travels alone and fails on its own, instead of taking the
/// whole snapshot down with it — which is what a snapshot that silently never
/// arrives looks like from Studio: "not connected". Clients read the messages
/// in order and act on `snapshot_complete`, the last one.
///
/// Every bundle opens on `/state/generation [generation, 1, part, parts]`: a
/// client marks itself current only once it holds every part, so losing the
/// first bundle of a split snapshot and getting the last is not taken for a
/// whole one.
fn encode_snapshot(messages: Vec<OscPacket>, generation: u32, max: usize) -> Vec<Vec<u8>> {
    let marker = |part, parts| state_generation_message(generation, true, part, parts);
    // The common case, a single bundle, is encoded once.
    let mut content = Vec::with_capacity(messages.len() + 1);
    content.push(marker(0, 1));
    content.extend(messages);
    let packet = OscPacket::Bundle(OscBundle {
        timetag: STATE_TIMETAG,
        content,
    });
    let whole = rosc::encoder::encode(&packet).unwrap_or_default();
    let mut content = match packet {
        OscPacket::Bundle(bundle) if whole.len() > max => bundle.content,
        _ => return vec![whole],
    };
    let marker_len = rosc::encoder::encode(&content.remove(0))
        .map(|bytes| 4 + bytes.len())
        .unwrap_or(0);
    let groups = split_state_groups(content, max.saturating_sub(marker_len));
    let parts = groups.len();
    groups
        .into_iter()
        .enumerate()
        .map(|(part, mut group)| {
            group.insert(0, marker(part, parts));
            encode_bundle(group)
        })
        .collect()
}

fn encode_bundle(content: Vec<OscPacket>) -> Vec<u8> {
    let bundle = OscPacket::Bundle(OscBundle {
        timetag: STATE_TIMETAG,
        content,
    });
    rosc::encoder::encode(&bundle).unwrap_or_default()
}

/// Encode `messages` as one bundle when that fits `max` bytes, else as the
/// fewest consecutive bundles that each do, in order.
#[cfg(test)]
pub(crate) fn encode_state_datagrams(messages: Vec<OscPacket>, max: usize) -> Vec<Vec<u8>> {
    let packet = OscPacket::Bundle(OscBundle {
        timetag: STATE_TIMETAG,
        content: messages,
    });
    let whole = rosc::encoder::encode(&packet).unwrap_or_default();
    let content = match packet {
        OscPacket::Bundle(bundle) if whole.len() > max => bundle.content,
        _ => return vec![whole],
    };
    split_state_groups(content, max)
        .into_iter()
        .map(encode_bundle)
        .collect()
}

/// The fewest consecutive groups of `messages` whose bundles each fit `max`
/// bytes, in order. Only an oversized snapshot comes here, so measuring the
/// messages one by one is a state change or a registration, never the audio
/// path.
fn split_state_groups(messages: Vec<OscPacket>, max: usize) -> Vec<Vec<OscPacket>> {
    // Bundle framing: "#bundle\0" and an 8-byte timetag, then a 4-byte size
    // prefix in front of every element.
    const BUNDLE_HEADER: usize = 16;
    let mut groups = Vec::new();
    let mut group: Vec<OscPacket> = Vec::new();
    let mut group_len = BUNDLE_HEADER;
    for message in messages {
        let len = 4 + rosc::encoder::encode(&message)
            .map(|bytes| bytes.len())
            .unwrap_or(0);
        if !group.is_empty() && group_len + len > max {
            groups.push(std::mem::take(&mut group));
            group_len = BUNDLE_HEADER;
        }
        group.push(message);
        group_len += len;
    }
    if !group.is_empty() {
        groups.push(group);
    }
    groups
}

/// Compose the live-state snapshot: core messages (renderer/layout/
/// speakers/loudness/DRC/monitoring/objects) + the host handler's extra
/// messages (e.g. /state/audio + /state/input device fields) + the
/// snapshot_complete marker. Read under the publication lock, by
/// [`broadcast_live_state`] and [`send_live_state_to`] only.
fn live_state_messages(
    control: &Arc<RendererControl>,
    host: Option<&Arc<dyn HostControlHandler>>,
) -> Vec<OscPacket> {
    let has_audio = host.is_some();
    let has_input = host.is_some();
    let mut messages = runtime_control::snapshot::build_live_state_bundle_with_host(
        control,
        has_audio,
        has_input,
        host.map(|h| h.as_ref()),
    );
    if let Some(h) = host {
        messages.extend(h.extend_snapshot());
    }
    // The bed→height object-generator listings (id / label / param specs, the
    // format of `available_backends`), so Studio builds the fixed-bed
    // height-generator selector + parameter controls dynamically. The engine
    // publishes them into `RendererControl` from its registry (which lives in
    // this crate), so any host-registered out-of-tree generators are included.
    messages.push(OscPacket::Message(OscMessage {
        addr: osc_contract::STATE_OBJECT_GENERATORS.to_string(),
        args: vec![OscType::String(control.object_generators_json())],
    }));
    // The phantom-extraction stage's listing, so Studio builds its controls.
    messages.push(OscPacket::Message(OscMessage {
        addr: osc_contract::STATE_PHANTOM.to_string(),
        args: vec![OscType::String(control.phantom_json())],
    }));
    messages.push(OscPacket::Message(OscMessage {
        addr: osc_contract::STATE_SNAPSHOT_COMPLETE.to_string(),
        args: vec![OscType::Int(1)],
    }));
    messages
}

pub(crate) fn save_live_config(
    control: &Arc<RendererControl>,
    host: Option<&Arc<dyn HostControlHandler>>,
    socket: &UdpSocket,
    clients: &OscClientRegistry,
) {
    let host_ref: Option<&dyn HostControlHandler> = host.map(|h| h.as_ref());
    // Clear any previous save error so the UI returns to a clean state for
    // this attempt (mirrors what we do at the start of a recompute).
    broadcast_string(socket, clients, osc_contract::STATE_CONFIG_SAVE_ERROR, "");
    match runtime_control::persist::save_live_config(control, host_ref) {
        Ok(result) => {
            broadcast_int(socket, clients, osc_contract::STATE_CONFIG_SAVED, 1);
            broadcast_live_state(control, host, socket, clients);
            log::info!("OSC: config saved to {}", result.path.display());
            if result.restart_required {
                if sys::shutdown::is_restartable() {
                    log::info!("OSC: render.bridge_path changed, requesting reload_config");
                    sys::shutdown::request_restart_from_config();
                } else {
                    // An embedded host cannot swap its bridge in place, and a
                    // restart request nobody consumes would stay latched and
                    // suppress the live handoff at teardown.
                    log::info!(
                        "OSC: render.bridge_path changed; takes effect when the host restarts the renderer"
                    );
                }
            }
        }
        Err(e) => {
            let message = format!("config save failed: {e}");
            log::error!("OSC: {message}");
            broadcast_string(
                socket,
                clients,
                osc_contract::STATE_CONFIG_SAVE_ERROR,
                &message,
            );
        }
    }
}

fn default_layout_export_name(layout: &renderer::speaker_layout::SpeakerLayout) -> String {
    let mut a: usize = 0;
    let mut b: usize = 0;
    let mut c: usize = 0;
    for speaker in &layout.speakers {
        if !speaker.spatialize {
            b += 1;
            continue;
        }
        let el = speaker.elevation.to_radians();
        let y = speaker.distance * el.sin();
        if y > 0.5 {
            c += 1;
        } else {
            a += 1;
        }
    }
    format!("{}.{}.{}", a, b, c)
}

fn sanitize_layout_name(name: &str) -> String {
    let sanitized: String = name
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = sanitized.trim_matches('.');
    if trimmed.is_empty() {
        "layout".to_string()
    } else {
        trimmed.to_string()
    }
}

pub(crate) fn export_current_layout(control: &Arc<RendererControl>, requested_name: Option<&str>) {
    let config_path = {
        let guard = control.config_path.lock();
        guard.clone()
    };
    let base_dir = config_path
        .as_ref()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let out_dir = base_dir.join("layouts");
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        log::error!(
            "OSC: failed to create layout export directory {}: {}",
            out_dir.display(),
            e
        );
        return;
    }
    let layout = control.editable_layout();
    let base_name = requested_name
        .map(sanitize_layout_name)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default_layout_export_name(&layout));
    let file_name = if base_name.to_ascii_lowercase().ends_with(".yaml") {
        base_name
    } else {
        format!("{}.yaml", base_name)
    };
    let out_path = out_dir.join(file_name);
    match layout.save_to_file(&out_path) {
        Ok(()) => log::info!("OSC: layout exported to {}", out_path.display()),
        Err(e) => log::error!(
            "OSC: failed to export layout to {}: {}",
            out_path.display(),
            e
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(addr: &str, payload_len: usize) -> OscPacket {
        OscPacket::Message(OscMessage {
            addr: addr.to_string(),
            args: vec![OscType::String("x".repeat(payload_len))],
        })
    }

    fn addrs(datagram: &[u8]) -> Vec<String> {
        match rosc::decoder::decode_udp(datagram).expect("valid OSC").1 {
            OscPacket::Bundle(bundle) => bundle
                .content
                .iter()
                .map(|p| match p {
                    OscPacket::Message(m) => m.addr.clone(),
                    OscPacket::Bundle(_) => panic!("nested bundle"),
                })
                .collect(),
            OscPacket::Message(_) => panic!("a state datagram is a bundle"),
        }
    }

    /// The whole live state of a wide layout reaches a datagram client: the
    /// layout and the speakers' state each travel as one message that cannot
    /// be split, and on 80, 128 and 256 speakers (full names, gains, delays
    /// and band edges set) every datagram of the snapshot is one a socket
    /// carries.
    #[test]
    fn a_wide_layouts_snapshot_fits_its_datagrams() {
        for speakers in [80, 128, 256] {
            let control = renderer::test_support::fixture_control();
            let mut layout = renderer::test_support::dome_layout(speakers);
            for (index, speaker) in layout.speakers.iter_mut().enumerate() {
                speaker.name = format!("Loudspeaker ring position {index}");
                speaker.freq_low = Some(80.0);
                speaker.freq_high = Some(18_000.0);
                speaker.delay_ms = 12.345;
            }
            control.with_editable_layout(|editable| *editable = layout);
            {
                let mut live = control.live.write();
                for index in 0..speakers {
                    let params = live.speakers.entry(index).or_default();
                    params.gain = 0.123_456;
                    params.muted = index % 2 == 0;
                }
            }
            let messages = live_state_messages(&control, None);
            let largest = messages
                .iter()
                .map(|message| rosc::encoder::encode(message).unwrap().len())
                .max()
                .unwrap();
            let datagrams = encode_snapshot(messages, 7, MAX_STATE_DATAGRAM);
            let sizes: Vec<usize> = datagrams.iter().map(Vec::len).collect();
            println!(
                "{speakers} speakers: snapshot in {} datagram(s) {sizes:?}, largest message \
                 {largest} bytes",
                datagrams.len()
            );
            assert!(
                sizes.iter().all(|size| *size <= MAX_STATE_DATAGRAM),
                "{speakers} speakers: {sizes:?} (largest message {largest} bytes)"
            );
        }
    }

    #[test]
    fn a_snapshot_that_fits_is_one_bundle() {
        let datagrams = encode_state_datagrams(vec![msg("/a", 10), msg("/b", 10)], 1000);
        assert_eq!(datagrams.len(), 1);
        assert_eq!(addrs(&datagrams[0]), ["/a", "/b"]);
    }

    #[test]
    fn an_oversized_snapshot_splits_into_bundles_under_the_limit_in_order() {
        let messages: Vec<OscPacket> = (0..20).map(|i| msg(&format!("/m{i}"), 300)).collect();
        let datagrams = encode_state_datagrams(messages, 1000);
        assert!(datagrams.len() > 1);
        assert!(
            datagrams.iter().all(|d| d.len() <= 1000),
            "every bundle fits"
        );
        let seen: Vec<String> = datagrams.iter().flat_map(|d| addrs(d)).collect();
        let expected: Vec<String> = (0..20).map(|i| format!("/m{i}")).collect();
        assert_eq!(seen, expected);
    }

    #[test]
    fn a_message_larger_than_the_limit_travels_alone() {
        let datagrams = encode_state_datagrams(
            vec![msg("/small", 10), msg("/huge", 5000), msg("/tail", 10)],
            1000,
        );
        assert_eq!(datagrams.len(), 3);
        assert_eq!(addrs(&datagrams[1]), ["/huge"]);
        assert!(datagrams[1].len() > 1000);
        assert_eq!(addrs(&datagrams[2]), ["/tail"]);
    }

    /// Every datagram of a split snapshot opens on its generation, its index
    /// and the count, and still fits: losing one is visible from any other.
    #[test]
    fn every_part_of_a_split_snapshot_carries_its_index_and_the_count() {
        let mut messages: Vec<OscPacket> = (0..20).map(|i| msg(&format!("/m{i}"), 300)).collect();
        messages.push(msg(osc_contract::STATE_SNAPSHOT_COMPLETE, 0));
        let datagrams = encode_snapshot(messages, 7, 1000);
        let parts = datagrams.len();
        assert!(parts > 1);
        for (index, datagram) in datagrams.iter().enumerate() {
            assert!(datagram.len() <= 1000, "part {index} fits");
            let OscPacket::Bundle(bundle) = rosc::decoder::decode_udp(datagram).unwrap().1 else {
                panic!("a state datagram is a bundle");
            };
            let Some(OscPacket::Message(marker)) = bundle.content.first() else {
                panic!("part {index} has no marker");
            };
            assert_eq!(marker.addr, osc_contract::STATE_GENERATION);
            assert_eq!(
                marker.args,
                [
                    OscType::Int(7),
                    OscType::Int(1),
                    OscType::Int(index as i32),
                    OscType::Int(parts as i32),
                ]
            );
        }
        let last = addrs(datagrams.last().unwrap());
        assert_eq!(
            last.last().map(String::as_str),
            Some(osc_contract::STATE_SNAPSHOT_COMPLETE)
        );
    }

    #[test]
    fn a_snapshot_that_fits_is_one_part() {
        let datagrams = encode_snapshot(vec![msg("/a", 10)], 3, 1000);
        assert_eq!(datagrams.len(), 1);
        assert_eq!(addrs(&datagrams[0]), [osc_contract::STATE_GENERATION, "/a"]);
    }
}
