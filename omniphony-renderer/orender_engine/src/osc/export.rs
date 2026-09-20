use std::net::{SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::Arc;

use renderer::live_params::RendererControl;
use rosc::{OscBundle, OscMessage, OscPacket, OscTime, OscType};
use runtime_control::HostControlHandler;

use super::client_registry::OscClientRegistry;
use super::transport::{broadcast_int, broadcast_string, send_raw};
use runtime_control::osc_contract;

/// Largest payload one UDP datagram carries over IPv4 (65 535 bytes minus the
/// IP and UDP headers), with room to spare for the bundle framing.
pub(crate) const MAX_STATE_DATAGRAM: usize = 65_000;

const STATE_TIMETAG: OscTime = OscTime {
    seconds: 0,
    fractional: 1,
};

/// The live-state snapshot encoded for the wire: one OSC bundle when it fits a
/// datagram, consecutive bundles when it does not (a long device list, a bridge
/// error report…). Each bundle stays under [`MAX_STATE_DATAGRAM`]; a single
/// message larger than that travels alone and fails on its own, instead of
/// taking the whole snapshot down with it — which is what a snapshot that
/// silently never arrives looks like from Studio: "not connected". Clients read
/// the messages in order and act on `snapshot_complete`, the last one, so the
/// split is invisible to them.
pub(crate) struct LiveStateDatagrams(Vec<Vec<u8>>);

impl LiveStateDatagrams {
    /// Send the snapshot to one client (registration, refresh).
    pub(crate) fn send_to(&self, socket: &UdpSocket, client: SocketAddr) {
        for bytes in &self.0 {
            if let Err(e) = socket.send_to(bytes, client) {
                log::warn!(
                    "Failed to send live state ({} bytes) to {}: {}",
                    bytes.len(),
                    client,
                    e
                );
            }
        }
    }

    /// Broadcast the snapshot to every registered client.
    pub(crate) fn broadcast(&self, socket: &UdpSocket, clients: &OscClientRegistry) {
        for bytes in &self.0 {
            send_raw(socket, clients, bytes);
        }
    }
}

fn encode_bundle(content: Vec<OscPacket>) -> Vec<u8> {
    let bundle = OscPacket::Bundle(OscBundle {
        timetag: STATE_TIMETAG,
        content,
    });
    rosc::encoder::encode(&bundle).unwrap_or_default()
}

/// Encode `messages` as one bundle when that fits `max` bytes, else as the
/// fewest consecutive bundles that each do, in order. The common case costs
/// the single encode it always did; only an oversized snapshot measures its
/// messages one by one, and that is a state change or a registration, never
/// the audio path.
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
    // Bundle framing: "#bundle\0" and an 8-byte timetag, then a 4-byte size
    // prefix in front of every element.
    const BUNDLE_HEADER: usize = 16;
    let mut datagrams = Vec::new();
    let mut group: Vec<OscPacket> = Vec::new();
    let mut group_len = BUNDLE_HEADER;
    for message in content {
        let len = 4 + rosc::encoder::encode(&message)
            .map(|bytes| bytes.len())
            .unwrap_or(0);
        if !group.is_empty() && group_len + len > max {
            datagrams.push(encode_bundle(std::mem::take(&mut group)));
            group_len = BUNDLE_HEADER;
        }
        group.push(message);
        group_len += len;
    }
    if !group.is_empty() {
        datagrams.push(encode_bundle(group));
    }
    datagrams
}

/// Compose the live-state snapshot: core messages (renderer/layout/
/// speakers/loudness/DRC/monitoring/objects) + the host handler's extra
/// messages (e.g. /state/audio + /state/input device fields) + the
/// snapshot_complete marker, bundled and encoded for the wire.
pub(crate) fn build_live_state(
    control: &Arc<RendererControl>,
    host: Option<&Arc<dyn HostControlHandler>>,
) -> LiveStateDatagrams {
    let has_audio = host.is_some();
    let has_input = host.is_some();
    let mut messages =
        runtime_control::snapshot::build_live_state_bundle(control, has_audio, has_input);
    if let Some(h) = host {
        messages.extend(h.extend_snapshot());
    }
    // Declared bed→height object-generator schema (id / label / param specs), so
    // Studio builds the fixed-bed height-generator selector + parameter sliders
    // dynamically. The
    // engine publishes the JSON into `RendererControl` from its registry (which
    // lives in this crate), so any host-registered out-of-tree generators are
    // included.
    messages.push(OscPacket::Message(OscMessage {
        addr: osc_contract::STATE_OBJECT_GENERATORS.to_string(),
        args: vec![OscType::String(control.object_generators_schema())],
    }));
    // Declared phantom-extraction param schema, so Studio builds its sliders.
    messages.push(OscPacket::Message(OscMessage {
        addr: osc_contract::STATE_PHANTOM.to_string(),
        args: vec![OscType::String(control.phantom_schema())],
    }));
    messages.push(OscPacket::Message(OscMessage {
        addr: osc_contract::STATE_SNAPSHOT_COMPLETE.to_string(),
        args: vec![OscType::Int(1)],
    }));
    LiveStateDatagrams(encode_state_datagrams(messages, MAX_STATE_DATAGRAM))
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
            build_live_state(control, host).broadcast(socket, clients);
            log::info!("OSC: config saved to {}", result.path.display());
            if result.restart_required {
                log::info!("OSC: render.bridge_path changed, requesting reload_config");
                sys::shutdown::request_restart_from_config();
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
}
