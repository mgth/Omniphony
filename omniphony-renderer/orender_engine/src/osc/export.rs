use std::net::UdpSocket;
use std::path::PathBuf;
use std::sync::Arc;

use renderer::live_params::RendererControl;
use rosc::{OscBundle, OscMessage, OscPacket, OscTime, OscType};
use runtime_control::HostControlHandler;

use super::client_registry::OscClientRegistry;
use super::transport::{broadcast_int, broadcast_string, send_raw};

/// Compose the live-state snapshot bundle: core messages (renderer/layout/
/// speakers/loudness/DRC/monitoring/objects) + the host handler's extra
/// messages (e.g. /state/audio + /state/input device fields) + the
/// snapshot_complete marker, bundled and encoded.
pub(crate) fn build_live_state_bundle(
    control: &Arc<RendererControl>,
    host: Option<&Arc<dyn HostControlHandler>>,
) -> Vec<u8> {
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
        addr: "/omniphony/state/object_generators".to_string(),
        args: vec![OscType::String(control.object_generators_schema())],
    }));
    // Declared phantom-extraction param schema, so Studio builds its sliders.
    messages.push(OscPacket::Message(OscMessage {
        addr: "/omniphony/state/phantom".to_string(),
        args: vec![OscType::String(control.phantom_schema())],
    }));
    messages.push(OscPacket::Message(OscMessage {
        addr: "/omniphony/state/snapshot_complete".to_string(),
        args: vec![OscType::Int(1)],
    }));
    let bundle = OscPacket::Bundle(OscBundle {
        timetag: OscTime {
            seconds: 0,
            fractional: 1,
        },
        content: messages,
    });
    rosc::encoder::encode(&bundle).unwrap_or_default()
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
    broadcast_string(socket, clients, "/omniphony/state/config/save_error", "");
    match runtime_control::persist::save_live_config(control, host_ref) {
        Ok(result) => {
            broadcast_int(socket, clients, "/omniphony/state/config/saved", 1);
            let state_bytes = build_live_state_bundle(control, host);
            send_raw(socket, clients, &state_bytes);
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
                "/omniphony/state/config/save_error",
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
