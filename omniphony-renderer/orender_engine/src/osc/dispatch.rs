use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;

use renderer::live_params::RendererControl;
use rosc::{OscMessage, OscType};
use runtime_control::HostControlHandler;
use runtime_control::band_topology_cache::BandTopologyCache;
use runtime_control::command::{RuntimeCommand, parse_process_command};
use runtime_control::context::RuntimeControlContext;
use runtime_control::heatmap_sub::HeatmapSubscriptionState;
use runtime_control::osc::{
    BroadcastValue, ControlEffects, apply_simple_osc_control, republish_heatmap_if_changed,
};

use super::client_registry::OscClientRegistry;
use super::export::{build_live_state_bundle, export_current_layout, save_live_config};
use super::recompute::trigger_layout_recompute;
use super::transport::{
    broadcast_fff, broadcast_float, broadcast_int, broadcast_string, resolve_register_addr,
    send_diag_state, send_metering_state,
};

#[derive(Default)]
pub(crate) struct RealtimeSeqState {
    pub master_gain: Option<i32>,
    pub speaker_gain: HashMap<usize, i32>,
    pub object_gain: HashMap<String, i32>,
    pub heatmap_sub: Arc<HeatmapSubscriptionState>,
    pub band_topology_cache: Arc<BandTopologyCache>,
}

pub(crate) fn handle_control_message(
    msg: &OscMessage,
    src: SocketAddr,
    control: &Arc<RendererControl>,
    host: Option<&Arc<dyn HostControlHandler>>,
    realtime_seq: &mut RealtimeSeqState,
    socket: &Arc<UdpSocket>,
    clients: &Arc<OscClientRegistry>,
) {
    let addr = msg.addr.as_str();

    // Monitoring cadences live on RendererControl (the source of truth): both
    // CLI and embedded engine read them, they persist to config, and they are
    // broadcast in the live-state bundle. Changing them marks the config dirty.
    if addr == "/omniphony/control/metering/rate_hz" {
        if let Some(hz) = first_rate_hz_arg(msg) {
            control.set_meter_rate_hz(hz);
            control.mark_dirty();
            broadcast_int(socket, clients, "/omniphony/state/config/saved", 0);
            log::info!("OSC metering rate set to {:.1} Hz", control.meter_rate_hz());
        }
        return;
    }
    if addr == "/omniphony/control/diag/rate_hz" {
        if let Some(hz) = first_rate_hz_arg(msg) {
            control.set_diag_rate_hz(hz);
            control.mark_dirty();
            broadcast_int(socket, clients, "/omniphony/state/config/saved", 0);
            log::info!("OSC diag rate set to {:.1} Hz", control.diag_rate_hz());
        }
        return;
    }

    // mpv overlay configuration. The overlay itself is generated in-process by
    // the `overlay` module and pulled over FFI; Studio only configures it here
    // (it no longer transports overlay frames). These are transient view
    // preferences — not persisted, so no `mark_dirty`.
    if addr == "/omniphony/control/overlay/enabled" {
        let enabled = match msg.args.first() {
            Some(OscType::Int(i)) => *i != 0,
            Some(OscType::Float(f)) => *f != 0.0,
            Some(OscType::Bool(b)) => *b,
            _ => return,
        };
        crate::overlay::set_enabled(enabled);
        return;
    }
    if addr == "/omniphony/control/overlay/labels" {
        let enabled = match msg.args.first() {
            Some(OscType::Int(i)) => *i != 0,
            Some(OscType::Float(f)) => *f != 0.0,
            Some(OscType::Bool(b)) => *b,
            _ => return,
        };
        crate::overlay::set_labels_enabled(enabled);
        return;
    }
    if addr == "/omniphony/control/overlay/trails" {
        // Args mirror Studio's former wire fields: enabled, ttl_ms, mode, teleport.
        let enabled = match msg.args.first() {
            Some(OscType::Int(i)) => *i != 0,
            Some(OscType::Float(f)) => *f != 0.0,
            Some(OscType::Bool(b)) => *b,
            _ => return,
        };
        let ttl_ms = match msg.args.get(1) {
            Some(OscType::Int(i)) if *i >= 0 => *i as u32,
            Some(OscType::Float(f)) if *f >= 0.0 => *f as u32,
            _ => 7000,
        };
        let diffuse = matches!(
            msg.args.get(2),
            Some(OscType::String(s)) if s.eq_ignore_ascii_case("diffuse")
        );
        let teleport = match msg.args.get(3) {
            Some(OscType::Float(f)) => *f as f64,
            Some(OscType::Int(i)) => *i as f64,
            _ => 0.0,
        };
        crate::overlay::set_trail_config(enabled, ttl_ms, diffuse, teleport);
        return;
    }
    if addr == "/omniphony/control/overlay/tag" {
        // [id, tag]: tag "A"/"B" sets an override colour, anything else clears it.
        let Some(id) = msg.args.first().and_then(|a| match a {
            OscType::Int(v) if *v >= 0 => Some(*v as u32),
            OscType::Float(v) if *v >= 0.0 => Some(*v as u32),
            OscType::String(s) => s.parse::<u32>().ok(),
            _ => None,
        }) else {
            return;
        };
        let tag = match msg.args.get(1) {
            Some(OscType::String(s)) => s.chars().next().filter(|c| matches!(c, 'A' | 'a' | 'B' | 'b')),
            _ => None,
        };
        crate::overlay::set_tag(id, tag);
        return;
    }
    let runtime_ctx = RuntimeControlContext::with_shared_state(
        Arc::clone(control),
        Arc::clone(&realtime_seq.heatmap_sub),
        Arc::clone(&realtime_seq.band_topology_cache),
    );

    if addr == "/omniphony/control/metering" {
        let enabled = match msg.args.first() {
            Some(OscType::Int(i)) => *i != 0,
            Some(OscType::Float(f)) => *f != 0.0,
            _ => return,
        };
        let client = resolve_register_addr(src, &[]);
        if clients.set_metering(client, enabled) {
            send_metering_state(socket, client, enabled);
        }
        return;
    }

    if addr == "/omniphony/control/diag/enabled" {
        let enabled = match msg.args.first() {
            Some(OscType::Int(i)) => *i != 0,
            Some(OscType::Float(f)) => *f != 0.0,
            Some(OscType::Bool(b)) => *b,
            _ => return,
        };
        let client = resolve_register_addr(src, &[]);
        if clients.set_diag(client, enabled) {
            send_diag_state(socket, client, enabled);
        }
        return;
    }

    if addr == "/omniphony/control/input/refresh" {
        let state_bytes = build_live_state_bundle(control, host);
        super::transport::send_raw(socket, clients, &state_bytes);
        log::info!("OSC: input state refresh requested");
        return;
    }

    if addr == "/omniphony/control/realtime/master_gain" {
        let Some(value) = msg.args.first().and_then(|arg| match arg {
            OscType::Float(v) => Some(*v),
            OscType::Int(v) => Some(*v as f32),
            _ => None,
        }) else {
            return;
        };
        let Some(seq) = msg.args.get(1).and_then(|arg| match arg {
            OscType::Int(v) => Some(*v),
            _ => None,
        }) else {
            return;
        };
        if realtime_seq.master_gain.is_some_and(|last| seq < last) {
            return;
        }
        realtime_seq.master_gain = Some(seq);
        control.live.write().unwrap().master_gain = value;
        control.mark_dirty();
        broadcast_int(socket, clients, "/omniphony/state/config/saved", 0);
        if let Ok(bytes) = rosc::encoder::encode(&rosc::OscPacket::Message(rosc::OscMessage {
            addr: "/omniphony/state/realtime/master_gain".to_string(),
            args: vec![OscType::Float(value), OscType::Int(seq)],
        })) {
            super::transport::send_raw(socket, clients, &bytes);
        }
        return;
    }

    if addr == "/omniphony/control/realtime/speaker_gain" {
        let Some(idx) = msg.args.first().and_then(|arg| match arg {
            OscType::Int(v) if *v >= 0 => Some(*v as usize),
            OscType::Float(v) if *v >= 0.0 => Some(*v as usize),
            _ => None,
        }) else {
            return;
        };
        let Some(value) = msg.args.get(1).and_then(|arg| match arg {
            OscType::Float(v) => Some(*v),
            OscType::Int(v) => Some(*v as f32),
            _ => None,
        }) else {
            return;
        };
        let Some(seq) = msg.args.get(2).and_then(|arg| match arg {
            OscType::Int(v) => Some(*v),
            _ => None,
        }) else {
            return;
        };
        if realtime_seq
            .speaker_gain
            .get(&idx)
            .copied()
            .is_some_and(|last| seq < last)
        {
            return;
        }
        realtime_seq.speaker_gain.insert(idx, seq);
        control
            .live
            .write()
            .unwrap()
            .speakers
            .entry(idx)
            .or_default()
            .gain = value;
        control.mark_speaker_params_dirty();
        control.mark_dirty();
        broadcast_int(socket, clients, "/omniphony/state/config/saved", 0);
        if let Ok(bytes) = rosc::encoder::encode(&rosc::OscPacket::Message(rosc::OscMessage {
            addr: "/omniphony/state/realtime/speaker_gain".to_string(),
            args: vec![
                OscType::Int(idx as i32),
                OscType::Float(value),
                OscType::Int(seq),
            ],
        })) {
            super::transport::send_raw(socket, clients, &bytes);
        }
        return;
    }

    if addr == "/omniphony/control/realtime/object_gain" {
        let Some(id) = msg.args.first().and_then(|arg| match arg {
            OscType::String(v) => {
                let trimmed = v.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            }
            OscType::Int(v) if *v >= 0 => Some(v.to_string()),
            OscType::Float(v) if *v >= 0.0 => Some((*v as i32).to_string()),
            _ => None,
        }) else {
            return;
        };
        let Some(value) = msg.args.get(1).and_then(|arg| match arg {
            OscType::Float(v) => Some(*v),
            OscType::Int(v) => Some(*v as f32),
            _ => None,
        }) else {
            return;
        };
        let Some(seq) = msg.args.get(2).and_then(|arg| match arg {
            OscType::Int(v) => Some(*v),
            _ => None,
        }) else {
            return;
        };
        if realtime_seq
            .object_gain
            .get(&id)
            .copied()
            .is_some_and(|last| seq < last)
        {
            return;
        }
        let Ok(idx) = id.parse::<usize>() else {
            return;
        };
        let clamped = value.clamp(0.0, 2.0);
        realtime_seq.object_gain.insert(id.clone(), seq);
        control
            .live
            .write()
            .unwrap()
            .objects
            .entry(idx)
            .or_default()
            .gain = clamped;
        control.mark_object_params_dirty();
        control.mark_dirty();
        broadcast_int(socket, clients, "/omniphony/state/config/saved", 0);
        if let Ok(bytes) = rosc::encoder::encode(&rosc::OscPacket::Message(rosc::OscMessage {
            addr: "/omniphony/state/realtime/object_gain".to_string(),
            args: vec![
                OscType::String(id.clone()),
                OscType::Float(clamped),
                OscType::Int(seq),
            ],
        })) {
            super::transport::send_raw(socket, clients, &bytes);
        }
        broadcast_float(
            socket,
            clients,
            &format!("/omniphony/state/object/{idx}/gain"),
            clamped,
        );
        return;
    }

    if addr == "/omniphony/control/render/bridge_path" {
        let value = match msg.args.first() {
            Some(OscType::String(s)) => s.trim(),
            _ => return,
        };
        let next = if value.is_empty() {
            None
        } else {
            Some(std::path::PathBuf::from(value))
        };
        if control.bridge_path() != next {
            control.set_bridge_path(next.clone());
            control.mark_dirty();
            broadcast_int(socket, clients, "/omniphony/state/config/saved", 0);
            let state_value = next
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default();
            broadcast_string(
                socket,
                clients,
                "/omniphony/state/render/bridge_path",
                &state_value,
            );
            log::info!(
                "OSC: render.bridge_path → {}",
                next.as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "<auto>".to_string())
            );
        }
        return;
    }

    if addr == "/omniphony/control/render/input_pipe" {
        let value = match msg.args.first() {
            Some(OscType::String(s)) => s.trim(),
            _ => return,
        };
        let next = if value.is_empty() {
            None
        } else {
            Some(value.to_string())
        };
        if control.input_path() != next {
            control.set_input_path(next.clone());
            control.mark_dirty();
            broadcast_int(socket, clients, "/omniphony/state/config/saved", 0);
            broadcast_string(
                socket,
                clients,
                "/omniphony/state/input_pipe",
                &next.clone().unwrap_or_default(),
            );
            log::info!(
                "OSC: render.input_pipe → {}",
                next.as_deref().unwrap_or("<default>")
            );
        }
        return;
    }

    if let Some(command) = parse_process_command(msg) {
        match command {
            RuntimeCommand::SaveConfig => {
                save_live_config(control, host, socket, clients)
            }
            RuntimeCommand::ReloadConfig => {
                log::info!("OSC reload_config requested");
                sys::shutdown::request_restart_from_config();
            }
            RuntimeCommand::Quit => {
                log::info!("OSC quit requested");
                sys::shutdown::request_shutdown();
            }
            RuntimeCommand::SetLogLevel(requested) => {
                sys::live_log::set_runtime_level(requested);
                broadcast_string(
                    socket,
                    clients,
                    "/omniphony/state/log_level",
                    sys::live_log::current_runtime_level_name(),
                );
                log::info!(
                    "OSC: log_level → {}",
                    sys::live_log::current_runtime_level_name()
                );
            }
        }
        return;
    }

    if let Some(effects) = apply_simple_osc_control(msg, &runtime_ctx) {
        apply_control_effects(effects, control, host, socket, clients, &runtime_ctx);
        return;
    }

    // Core didn't handle it — delegate to the host (audio output/input).
    if let Some(effects) = host.and_then(|h| h.handle(addr, msg)) {
        apply_control_effects(effects, control, host, socket, clients, &runtime_ctx);
        return;
    }

    if addr == "/omniphony/control/layout/export" {
        let requested_name = match msg.args.first() {
            Some(OscType::String(s)) if !s.trim().is_empty() => Some(s.trim()),
            _ => None,
        };
        export_current_layout(control, requested_name);
        return;
    }
}

fn first_rate_hz_arg(msg: &OscMessage) -> Option<f32> {
    msg.args.first().and_then(|arg| match arg {
        OscType::Float(v) => Some(*v),
        OscType::Int(v) => Some(*v as f32),
        _ => None,
    })
}

fn set_dirty(control: &Arc<RendererControl>, socket: &UdpSocket, clients: &OscClientRegistry) {
    control.mark_dirty();
    broadcast_int(socket, clients, "/omniphony/state/config/saved", 0);
}

fn apply_control_effects(
    effects: ControlEffects,
    control: &Arc<RendererControl>,
    host: Option<&Arc<dyn HostControlHandler>>,
    socket: &Arc<UdpSocket>,
    clients: &Arc<OscClientRegistry>,
    runtime_ctx: &RuntimeControlContext,
) {
    if effects.mark_dirty {
        set_dirty(control, socket, clients);
        let state_bytes = build_live_state_bundle(control, host);
        super::transport::send_raw(socket, clients, &state_bytes);
    }
    for update in effects.broadcasts {
        match update.value {
            BroadcastValue::Int(value) => broadcast_int(socket, clients, &update.addr, value),
            BroadcastValue::Float(value) => broadcast_float(socket, clients, &update.addr, value),
            BroadcastValue::Fff(a, b, c) => broadcast_fff(socket, clients, &update.addr, a, b, c),
            BroadcastValue::String(value) => {
                broadcast_string(socket, clients, &update.addr, &value)
            }
        }
    }
    if let Some(message) = effects.log_message {
        log::info!("{message}");
    }
    if effects.trigger_layout_recompute {
        trigger_layout_recompute(
            control,
            socket,
            clients,
            Arc::clone(&runtime_ctx.heatmap_sub),
            Arc::clone(&runtime_ctx.band_topology_cache),
        );
    }
    // After any state change, republish the heatmap to the active subscription
    // — but only if the recomputed payload differs from the last one cached.
    // This is the keystone of the pub/sub model: edits that don't actually move
    // the speaker (or otherwise change the heatmap) cost zero VBAP regen.
    if effects.mark_dirty || effects.trigger_layout_recompute {
        let pushes = republish_heatmap_if_changed(runtime_ctx);
        for update in pushes {
            if let BroadcastValue::String(value) = &update.value {
                broadcast_string(socket, clients, &update.addr, value);
            }
        }
    }
}
