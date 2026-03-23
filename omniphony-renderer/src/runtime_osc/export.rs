use std::net::UdpSocket;
use std::path::PathBuf;
use std::sync::Arc;

use audio_output::AudioControl;
use renderer::live_params::RendererControl;

use super::client_registry::OscClientRegistry;
use super::transport::{broadcast_int, send_raw};

pub(crate) fn build_live_state_bundle(
    control: &Arc<RendererControl>,
    audio_control: Option<&Arc<AudioControl>>,
) -> Vec<u8> {
    runtime_control::snapshot::build_live_state_bundle(control, audio_control)
}

pub(crate) fn save_live_config(
    control: &Arc<RendererControl>,
    audio_control: Option<&Arc<AudioControl>>,
    socket: &UdpSocket,
    clients: &OscClientRegistry,
) {
    match runtime_control::persist::save_live_config(control, audio_control) {
        Ok(result) => {
            broadcast_int(socket, clients, "/omniphony/state/config/saved", 1);
            send_raw(socket, clients, &result.state_bundle);
            log::info!("OSC: config saved to {}", result.path.display());
        }
        Err(e) => {
            log::error!("OSC: failed to save config: {}", e);
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
        let guard = control.config_path.lock().unwrap();
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
