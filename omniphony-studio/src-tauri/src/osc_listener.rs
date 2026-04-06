use rosc::{decoder, OscPacket};
use std::net::UdpSocket;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::app_state::OutputDeviceOption;
use crate::app_state::{AppState, Meter};
use crate::layouts::build_live_layout_from_cache;
use crate::osc_parser::{
    is_heartbeat_address, parse_osc_message, CoordinateFormat, HeartbeatResponse, LogEntry,
    OscEvent,
};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const HEARTBEAT_ACK_TIMEOUT: Duration = Duration::from_secs(10);
const SNAPSHOT_REQUEST_INTERVAL: Duration = Duration::from_secs(1);

// ── control messages (frontend → OSC listener) ────────────────────────────

pub enum OscControlMsg {
    SendFloat {
        address: String,
        value: f32,
    },
    SendInt {
        address: String,
        value: i32,
    },
    SendNoArgs {
        address: String,
    },
    SendString {
        address: String,
        value: String,
    },
    SendFloats3 {
        address: String,
        a: f32,
        b: f32,
        c: f32,
    },
    SendSpeakerAdd {
        name: String,
        azimuth: f32,
        elevation: f32,
        distance: f32,
        spatialize: i32,
        delay_ms: f32,
    },
    SendSpeakersMove {
        from: i32,
        to: i32,
    },
    Reconnect {
        host: String,
        rx_port: u16,
        listen_port: u16,
    },
    SetMeteringEnabled {
        enabled: bool,
    },
}

// ── OSC send helpers ─────────────────────────────────────────────────────

fn send_osc_float(socket: &UdpSocket, addr: &str, host: &str, rx_port: u16, value: f32) {
    use rosc::{encoder, OscMessage, OscType};
    let msg = OscPacket::Message(OscMessage {
        addr: addr.to_string(),
        args: vec![OscType::Float(value)],
    });
    if let Ok(data) = encoder::encode(&msg) {
        let _ = socket.send_to(&data, format!("{host}:{rx_port}"));
    }
}

fn send_osc_int(socket: &UdpSocket, addr: &str, host: &str, rx_port: u16, value: i32) {
    use rosc::{encoder, OscMessage, OscType};
    let msg = OscPacket::Message(OscMessage {
        addr: addr.to_string(),
        args: vec![OscType::Int(value)],
    });
    if let Ok(data) = encoder::encode(&msg) {
        let _ = socket.send_to(&data, format!("{host}:{rx_port}"));
    }
}

fn send_osc_no_args(socket: &UdpSocket, addr: &str, host: &str, rx_port: u16) {
    use rosc::{encoder, OscMessage};
    let msg = OscPacket::Message(OscMessage {
        addr: addr.to_string(),
        args: vec![],
    });
    if let Ok(data) = encoder::encode(&msg) {
        let _ = socket.send_to(&data, format!("{host}:{rx_port}"));
    }
}

fn send_osc_string(socket: &UdpSocket, addr: &str, host: &str, rx_port: u16, value: &str) {
    use rosc::{encoder, OscMessage, OscType};
    let msg = OscPacket::Message(OscMessage {
        addr: addr.to_string(),
        args: vec![OscType::String(value.to_string())],
    });
    if let Ok(data) = encoder::encode(&msg) {
        let _ = socket.send_to(&data, format!("{host}:{rx_port}"));
    }
}

fn send_osc_floats3(
    socket: &UdpSocket,
    addr: &str,
    host: &str,
    rx_port: u16,
    a: f32,
    b: f32,
    c: f32,
) {
    use rosc::{encoder, OscMessage, OscType};
    let msg = OscPacket::Message(OscMessage {
        addr: addr.to_string(),
        args: vec![OscType::Float(a), OscType::Float(b), OscType::Float(c)],
    });
    if let Ok(data) = encoder::encode(&msg) {
        let _ = socket.send_to(&data, format!("{host}:{rx_port}"));
    }
}

fn send_osc_speaker_add(
    socket: &UdpSocket,
    host: &str,
    rx_port: u16,
    name: &str,
    azimuth: f32,
    elevation: f32,
    distance: f32,
    spatialize: i32,
    delay_ms: f32,
) {
    use rosc::{encoder, OscMessage, OscType};
    let msg = OscPacket::Message(OscMessage {
        addr: "/omniphony/control/speakers/add".to_string(),
        args: vec![
            OscType::String(name.to_string()),
            OscType::Float(azimuth),
            OscType::Float(elevation),
            OscType::Float(distance),
            OscType::Int(if spatialize != 0 { 1 } else { 0 }),
            OscType::Float(delay_ms),
        ],
    });
    if let Ok(data) = encoder::encode(&msg) {
        let _ = socket.send_to(&data, format!("{host}:{rx_port}"));
    }
}

fn send_osc_speakers_move(socket: &UdpSocket, host: &str, rx_port: u16, from: i32, to: i32) {
    use rosc::{encoder, OscMessage, OscType};
    let msg = OscPacket::Message(OscMessage {
        addr: "/omniphony/control/speakers/move".to_string(),
        args: vec![OscType::Int(from.max(0)), OscType::Int(to.max(0))],
    });
    if let Ok(data) = encoder::encode(&msg) {
        let _ = socket.send_to(&data, format!("{host}:{rx_port}"));
    }
}

fn send_register(socket: &UdpSocket, host: &str, rx_port: u16, listen_port: u16) {
    send_osc_int(
        socket,
        "/omniphony/register",
        host,
        rx_port,
        listen_port as i32,
    );
    log::info!("[osc] register sent → udp://{host}:{rx_port} listen_port={listen_port}");
}

fn send_metering_enabled(socket: &UdpSocket, host: &str, rx_port: u16, enabled: bool) {
    send_osc_int(
        socket,
        "/omniphony/control/metering",
        host,
        rx_port,
        if enabled { 1 } else { 0 },
    );
}

fn send_heartbeat(socket: &UdpSocket, host: &str, rx_port: u16, listen_port: u16) {
    send_osc_int(
        socket,
        "/omniphony/heartbeat",
        host,
        rx_port,
        listen_port as i32,
    );
}

fn emit_osc_status(app: &AppHandle, state: &Arc<Mutex<AppState>>, status: &str) {
    {
        let mut s = state.lock().unwrap();
        s.osc_status = Some(status.to_string());
        if status != "connected" {
            s.osc_snapshot_ready = false;
        }
    }
    let _ = app.emit("osc:status", serde_json::json!({ "status": status }));
}

// ── public spawn function ─────────────────────────────────────────────────

pub fn spawn_osc_task(
    app: AppHandle,
    state: Arc<Mutex<AppState>>,
    host: String,
    osc_port: u16,
    osc_rx_port: u16,
    ctrl_rx: UnboundedReceiver<OscControlMsg>,
    listen_port_out: Arc<Mutex<u16>>,
) {
    std::thread::spawn(move || {
        osc_thread(
            app,
            state,
            host,
            osc_port,
            osc_rx_port,
            ctrl_rx,
            listen_port_out,
        );
    });
}

fn osc_thread(
    app: AppHandle,
    state: Arc<Mutex<AppState>>,
    mut host: String,
    osc_port: u16,
    mut osc_rx_port: u16,
    mut ctrl_rx: UnboundedReceiver<OscControlMsg>,
    listen_port_out: Arc<Mutex<u16>>,
) {
    let bind_addr = format!("0.0.0.0:{osc_port}");
    let socket = match UdpSocket::bind(&bind_addr) {
        Ok(s) => s,
        Err(e) => {
            log::error!("[osc] bind failed: {e}");
            emit_osc_status(&app, &state, "error");
            return;
        }
    };
    socket
        .set_read_timeout(Some(Duration::from_millis(50)))
        .ok();

    let listen_port = socket.local_addr().map(|a| a.port()).unwrap_or(osc_port);
    *listen_port_out.lock().unwrap() = listen_port;
    log::info!("[osc] listening on udp://0.0.0.0:{listen_port}");

    send_register(&socket, &host, osc_rx_port, listen_port);
    let mut last_snapshot_request_at = Instant::now();
    let mut last_ack_at = Instant::now();
    let mut last_heartbeat_at = Instant::now();
    let mut is_connected = false;
    let mut metering_enabled = state.lock().unwrap().osc_metering_enabled.unwrap_or(0) != 0;
    send_metering_enabled(&socket, &host, osc_rx_port, metering_enabled);
    emit_osc_status(&app, &state, "reconnecting");

    let mut buf = [0u8; 65536];

    loop {
        // drain control messages (non-blocking)
        loop {
            match ctrl_rx.try_recv() {
                Ok(msg) => match msg {
                    OscControlMsg::SendFloat { address, value } => {
                        send_osc_float(&socket, &address, &host, osc_rx_port, value);
                    }
                    OscControlMsg::SendInt { address, value } => {
                        send_osc_int(&socket, &address, &host, osc_rx_port, value);
                    }
                    OscControlMsg::SendNoArgs { address } => {
                        send_osc_no_args(&socket, &address, &host, osc_rx_port);
                    }
                    OscControlMsg::SendString { address, value } => {
                        send_osc_string(&socket, &address, &host, osc_rx_port, &value);
                    }
                    OscControlMsg::SendFloats3 { address, a, b, c } => {
                        send_osc_floats3(&socket, &address, &host, osc_rx_port, a, b, c);
                    }
                    OscControlMsg::SendSpeakerAdd {
                        name,
                        azimuth,
                        elevation,
                        distance,
                        spatialize,
                        delay_ms,
                    } => {
                        send_osc_speaker_add(
                            &socket,
                            &host,
                            osc_rx_port,
                            &name,
                            azimuth,
                            elevation,
                            distance,
                            spatialize,
                            delay_ms,
                        );
                    }
                    OscControlMsg::SendSpeakersMove { from, to } => {
                        send_osc_speakers_move(&socket, &host, osc_rx_port, from, to);
                    }
                    OscControlMsg::Reconnect {
                        host: h,
                        rx_port,
                        listen_port: lp,
                    } => {
                        host = h;
                        osc_rx_port = rx_port;
                        send_register(&socket, &host, osc_rx_port, lp);
                        last_snapshot_request_at = Instant::now();
                        send_metering_enabled(&socket, &host, osc_rx_port, metering_enabled);
                        last_ack_at = Instant::now();
                        if is_connected {
                            is_connected = false;
                        }
                        emit_osc_status(&app, &state, "reconnecting");
                    }
                    OscControlMsg::SetMeteringEnabled { enabled } => {
                        metering_enabled = enabled;
                        send_metering_enabled(&socket, &host, osc_rx_port, enabled);
                    }
                },
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(_) => return, // channel closed
            }
        }

        let snapshot_ready = state.lock().unwrap().osc_snapshot_ready;
        if !snapshot_ready && last_snapshot_request_at.elapsed() >= SNAPSHOT_REQUEST_INTERVAL {
            send_register(&socket, &host, osc_rx_port, listen_port);
            send_metering_enabled(&socket, &host, osc_rx_port, metering_enabled);
            last_snapshot_request_at = Instant::now();
            log::debug!("[osc] snapshot not ready yet, re-requesting live state bundle");
        }

        // heartbeat timer
        if last_heartbeat_at.elapsed() >= HEARTBEAT_INTERVAL {
            last_heartbeat_at = Instant::now();
            send_heartbeat(&socket, &host, osc_rx_port, listen_port);

            if last_ack_at.elapsed() >= HEARTBEAT_ACK_TIMEOUT {
                log::warn!("[osc] heartbeat timeout, re-registering");
                if is_connected {
                    is_connected = false;
                    emit_osc_status(&app, &state, "reconnecting");
                }
                send_register(&socket, &host, osc_rx_port, listen_port);
                last_snapshot_request_at = Instant::now();
                send_metering_enabled(&socket, &host, osc_rx_port, metering_enabled);
            }
        }

        // receive packet
        let n = match socket.recv_from(&mut buf) {
            Ok((n, _)) => n,
            Err(_) => continue, // timeout
        };

        match decoder::decode_udp(&buf[..n]) {
            Ok((_, packet)) => {
                handle_packet(
                    packet,
                    &app,
                    &state,
                    &socket,
                    &host,
                    osc_rx_port,
                    listen_port,
                    metering_enabled,
                    &mut last_ack_at,
                    &mut is_connected,
                );
            }
            Err(_) => {}
        }
    }
}

fn handle_packet(
    packet: OscPacket,
    app: &AppHandle,
    state: &Arc<Mutex<AppState>>,
    socket: &UdpSocket,
    host: &str,
    osc_rx_port: u16,
    listen_port: u16,
    metering_enabled: bool,
    last_ack_at: &mut Instant,
    is_connected: &mut bool,
) {
    match packet {
        OscPacket::Message(msg) => {
            match is_heartbeat_address(&msg.addr) {
                HeartbeatResponse::Ack => {
                    *last_ack_at = Instant::now();
                    if !*is_connected {
                        *is_connected = true;
                        emit_osc_status(app, state, "connected");
                    }
                    return;
                }
                HeartbeatResponse::Unknown => {
                    log::info!("[osc] heartbeat/unknown → re-registering");
                    send_register(socket, host, osc_rx_port, listen_port);
                    send_metering_enabled(socket, host, osc_rx_port, metering_enabled);
                    *last_ack_at = Instant::now();
                    if *is_connected {
                        *is_connected = false;
                        emit_osc_status(app, state, "reconnecting");
                    }
                    return;
                }
                HeartbeatResponse::None => {}
            }

            let coordinate_format = {
                let s = state.lock().unwrap();
                if s.current_coordinate_format == 1 {
                    CoordinateFormat::Polar
                } else {
                    CoordinateFormat::Cartesian
                }
            };

            if let Some(ev) = parse_osc_message(&msg.addr, &msg.args, coordinate_format) {
                if !*is_connected {
                    *is_connected = true;
                    emit_osc_status(app, state, "connected");
                }
                handle_event(ev, app, state);
            }
        }
        OscPacket::Bundle(bundle) => {
            let mut config_events: Vec<OscEvent> = Vec::new();

            for pkt in bundle.content {
                match pkt {
                    OscPacket::Message(msg) => {
                        match is_heartbeat_address(&msg.addr) {
                            HeartbeatResponse::Ack => {
                                *last_ack_at = Instant::now();
                                if !*is_connected {
                                    *is_connected = true;
                                    emit_osc_status(app, state, "connected");
                                }
                                continue;
                            }
                            HeartbeatResponse::Unknown => {
                                send_register(socket, host, osc_rx_port, listen_port);
                                send_metering_enabled(socket, host, osc_rx_port, metering_enabled);
                                *last_ack_at = Instant::now();
                                if *is_connected {
                                    *is_connected = false;
                                    emit_osc_status(app, state, "reconnecting");
                                }
                                continue;
                            }
                            HeartbeatResponse::None => {}
                        }

                        let coordinate_format = {
                            let s = state.lock().unwrap();
                            if s.current_coordinate_format == 1 {
                                CoordinateFormat::Polar
                            } else {
                                CoordinateFormat::Cartesian
                            }
                        };

                        if let Some(ev) = parse_osc_message(&msg.addr, &msg.args, coordinate_format)
                        {
                            if !*is_connected {
                                *is_connected = true;
                                emit_osc_status(app, state, "connected");
                            }
                            let is_config = matches!(
                                &ev,
                                OscEvent::ConfigSpeakersCount { .. }
                                    | OscEvent::ConfigSpeaker { .. }
                            );
                            if is_config {
                                config_events.push(ev);
                            } else {
                                handle_event(ev, app, state);
                            }
                        }
                    }
                    OscPacket::Bundle(inner) => {
                        for pkt2 in inner.content {
                            if let OscPacket::Message(msg) = pkt2 {
                                let coordinate_format = {
                                    let s = state.lock().unwrap();
                                    if s.current_coordinate_format == 1 {
                                        CoordinateFormat::Polar
                                    } else {
                                        CoordinateFormat::Cartesian
                                    }
                                };

                                if let Some(ev) =
                                    parse_osc_message(&msg.addr, &msg.args, coordinate_format)
                                {
                                    if !*is_connected {
                                        *is_connected = true;
                                        emit_osc_status(app, state, "connected");
                                    }
                                    handle_event(ev, app, state);
                                }
                            }
                        }
                    }
                }
            }

            if !config_events.is_empty() {
                apply_speaker_config(config_events, app, state);
            }
        }
    }
}

fn apply_speaker_config(events: Vec<OscEvent>, app: &AppHandle, state: &Arc<Mutex<AppState>>) {
    let payload = {
        let mut s = state.lock().unwrap();

        for event in events {
            match event {
                OscEvent::ConfigSpeakersCount { count } => {
                    s.live_speaker_count = Some(count);
                    s.live_speakers.retain(|idx, _| *idx < count);
                }
                OscEvent::ConfigSpeaker {
                    index,
                    name,
                    coord_mode,
                    x,
                    y,
                    z,
                    azimuth_deg,
                    elevation_deg,
                    distance_m,
                    delay_ms,
                    spatialize,
                    position: _,
                    ..
                } => {
                    s.live_speakers.insert(
                        index,
                        crate::app_state::LiveSpeakerConfig {
                            name,
                            delay_ms,
                            spatialize,
                            coord_mode,
                            x,
                            y,
                            z,
                            azimuth_deg,
                            elevation_deg,
                            distance_m,
                        },
                    );
                }
                _ => {}
            }
        }

        s.layouts.retain(|l| l.key != "omniphony-live");
        if let Some(live) = build_live_layout_from_cache(&s.live_speakers, s.live_speaker_count) {
            s.layouts.insert(0, live.clone());
            s.selected_layout_key = Some(live.key.clone());
        }

        serde_json::json!({
            "layouts": s.layouts,
            "selectedLayoutKey": s.selected_layout_key
        })
    }; // mutex released here

    let _ = app.emit("layouts:update", payload);
}

fn handle_event(ev: OscEvent, app: &AppHandle, state: &Arc<Mutex<AppState>>) {
    // Update state under the lock, collect emit data, then release before emitting.
    let (to_emit, removed_ids): (Option<(&'static str, serde_json::Value)>, Vec<String>) = {
        let mut s = state.lock().unwrap();
        let mut removed_ids: Vec<String> = Vec::new();
        match ev {
            OscEvent::SpatialFrame {
                sample_pos,
                generation,
                object_count,
                coordinate_format,
            } => {
                let generation_changed = s
                    .current_content_generation
                    .is_some_and(|prev| prev != generation);
                let is_reset = generation_changed
                    || s.last_spatial_sample_pos
                        .is_some_and(|prev| sample_pos < prev);
                s.last_spatial_sample_pos = Some(sample_pos);
                s.current_content_generation = Some(generation);
                s.current_coordinate_format = coordinate_format;

                let stale_ids: Vec<String> = if is_reset {
                    s.sources.keys().cloned().collect()
                } else {
                    s.sources
                        .keys()
                        .filter_map(|id| {
                            id.parse::<u32>().ok().and_then(|idx| {
                                if idx >= object_count {
                                    Some(id.clone())
                                } else {
                                    None
                                }
                            })
                        })
                        .collect()
                };

                for id in &stale_ids {
                    s.sources.remove(id);
                    s.source_levels.remove(id);
                    s.object_speaker_gains.remove(id);
                    s.object_gains.remove(id);
                    s.object_mutes.remove(id);
                }
                removed_ids.extend(stale_ids);
                (
                    Some((
                        "spatial:frame",
                        serde_json::json!({
                            "samplePos": sample_pos,
                            "generation": generation,
                            "objectCount": object_count,
                            "coordinateFormat": coordinate_format,
                            "reset": is_reset
                        }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::Update { id, position, name } => {
                let current_generation = s.current_content_generation;
                let entry = s.sources.entry(id.clone()).or_default();
                entry.x = position.x;
                entry.y = position.y;
                entry.z = position.z;
                entry.coord_mode = Some(position.coord_mode.clone());
                entry.azimuth_deg = position.azimuth_deg;
                entry.elevation_deg = position.elevation_deg;
                entry.distance_m = position.distance_m;
                entry.generation = position.generation.or(current_generation);
                entry.direct_speaker_index = position.direct_speaker_index;
                if let Some(n) = name {
                    entry.name = Some(n);
                }
                let payload = serde_json::json!({
                    "id": id,
                    "position": {
                            "x": entry.x,
                            "y": entry.y,
                            "z": entry.z,
                            "coordMode": entry.coord_mode,
                            "azimuthDeg": entry.azimuth_deg,
                            "elevationDeg": entry.elevation_deg,
                            "distanceM": entry.distance_m,
                            "generation": entry.generation,
                            "directSpeakerIndex": entry.direct_speaker_index,
                            "name": entry.name
                        }
                });
                (Some(("source:update", payload)), removed_ids)
            }

            OscEvent::Remove { id } => {
                s.sources.remove(&id);
                s.source_levels.remove(&id);
                s.object_speaker_gains.remove(&id);
                s.object_gains.remove(&id);
                s.object_mutes.remove(&id);
                (
                    Some(("source:remove", serde_json::json!({ "id": id }))),
                    removed_ids,
                )
            }

            OscEvent::MeterObject {
                id,
                peak_dbfs,
                rms_dbfs,
            } => {
                s.source_levels.insert(
                    id.clone(),
                    Meter {
                        peak_dbfs,
                        rms_dbfs,
                    },
                );
                (
                    Some((
                        "source:meter",
                        serde_json::json!({
                            "id": id,
                            "meter": { "peakDbfs": peak_dbfs, "rmsDbfs": rms_dbfs }
                        }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::MeterObjectGains { id, gains } => {
                s.object_speaker_gains.insert(id.clone(), gains.clone());
                (
                    Some((
                        "source:gains",
                        serde_json::json!({ "id": id, "gains": gains }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::MeterSpeaker {
                id,
                peak_dbfs,
                rms_dbfs,
            } => {
                s.speaker_levels.insert(
                    id.clone(),
                    Meter {
                        peak_dbfs,
                        rms_dbfs,
                    },
                );
                (
                    Some((
                        "speaker:meter",
                        serde_json::json!({
                            "id": id,
                            "meter": { "peakDbfs": peak_dbfs, "rmsDbfs": rms_dbfs }
                        }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::StateObjectGain { id, gain } => {
                s.object_gains.insert(id.clone(), gain);
                (
                    Some(("object:gain", serde_json::json!({ "id": id, "gain": gain }))),
                    removed_ids,
                )
            }

            OscEvent::StateSpeakerGain { id, gain } => {
                s.speaker_gains.insert(id.clone(), gain);
                (
                    Some((
                        "speaker:gain",
                        serde_json::json!({ "id": id, "gain": gain }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateSpeakerDelay { id, delay_ms } => {
                if let Ok(index) = id.parse::<usize>() {
                    if let Some(layout_key) = s.selected_layout_key.clone() {
                        if let Some(layout) = s.layouts.iter_mut().find(|l| l.key == layout_key) {
                            if let Some(spk) = layout.speakers.get_mut(index) {
                                spk.delay_ms = delay_ms.max(0.0);
                            }
                        }
                    }
                }
                (
                    Some((
                        "speaker:delay",
                        serde_json::json!({ "id": id, "delayMs": delay_ms.max(0.0) }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::StateObjectMute { id, muted } => {
                if muted {
                    s.object_mutes.insert(id.clone(), 1);
                } else {
                    s.object_mutes.remove(&id);
                }
                (
                    Some((
                        "object:mute",
                        serde_json::json!({ "id": id, "muted": muted as u8 }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::StateSpeakerMute { id, muted } => {
                if muted {
                    s.speaker_mutes.insert(id.clone(), 1);
                } else {
                    s.speaker_mutes.remove(&id);
                }
                (
                    Some((
                        "speaker:mute",
                        serde_json::json!({ "id": id, "muted": muted as u8 }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateOscMetering { enabled } => {
                s.osc_metering_enabled = Some(if enabled { 1 } else { 0 });
                (
                    Some((
                        "osc:metering",
                        serde_json::json!({ "enabled": if enabled { 1 } else { 0 } }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateSpeakerSpatialize { id, spatialize } => {
                if let Ok(index) = id.parse::<usize>() {
                    if let Some(layout_key) = s.selected_layout_key.clone() {
                        if let Some(layout) = s.layouts.iter_mut().find(|l| l.key == layout_key) {
                            if let Some(spk) = layout.speakers.get_mut(index) {
                                spk.spatialize = if spatialize { 1 } else { 0 };
                            }
                        }
                    }
                }
                (
                    Some((
                        "speaker:spatialize",
                        serde_json::json!({ "id": id, "spatialize": if spatialize { 1 } else { 0 } }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateSpeakerName { id, name } => {
                if let Ok(index) = id.parse::<usize>() {
                    if let Some(layout_key) = s.selected_layout_key.clone() {
                        if let Some(layout) = s.layouts.iter_mut().find(|l| l.key == layout_key) {
                            if let Some(spk) = layout.speakers.get_mut(index) {
                                spk.id = name.clone();
                            }
                        }
                    }
                }
                (
                    Some((
                        "speaker:name",
                        serde_json::json!({ "id": id, "name": name }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::StateRoomRatio {
                width,
                length,
                height,
            } => {
                s.room_ratio.width = width;
                s.room_ratio.length = length;
                s.room_ratio.height = height;
                (
                    Some((
                        "room_ratio",
                        serde_json::json!({
                            "roomRatio": {
                                "width": width,
                                "length": length,
                                "height": height,
                                "rear": s.room_ratio.rear,
                                "lower": s.room_ratio.lower
                            }
                        }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateRoomRatioRear { value } => {
                s.room_ratio.rear = value;
                (
                    Some((
                        "room_ratio",
                        serde_json::json!({
                            "roomRatio": {
                                "width": s.room_ratio.width,
                                "length": s.room_ratio.length,
                                "height": s.room_ratio.height,
                                "rear": value,
                                "lower": s.room_ratio.lower
                            }
                        }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateRoomRatioLower { value } => {
                s.room_ratio.lower = value;
                (
                    Some((
                        "room_ratio",
                        serde_json::json!({
                            "roomRatio": {
                                "width": s.room_ratio.width,
                                "length": s.room_ratio.length,
                                "height": s.room_ratio.height,
                                "rear": s.room_ratio.rear,
                                "lower": value
                            }
                        }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateRoomRatioCenterBlend { value } => {
                s.room_ratio.center_blend = value.clamp(0.0, 1.0);
                (
                    Some((
                        "room_ratio",
                        serde_json::json!({
                            "roomRatio": {
                                "width": s.room_ratio.width,
                                "length": s.room_ratio.length,
                                "height": s.room_ratio.height,
                                "rear": s.room_ratio.rear,
                                "lower": s.room_ratio.lower,
                                "centerBlend": s.room_ratio.center_blend
                            }
                        }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateLayoutRadiusM { value } => {
                if let Some(layout_key) = s.selected_layout_key.clone() {
                    if let Some(layout) = s.layouts.iter_mut().find(|l| l.key == layout_key) {
                        layout.radius_m = value.max(0.01);
                    }
                }
                (
                    Some((
                        "layout:radius_m",
                        serde_json::json!({ "value": value.max(0.01) }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::StateSpreadMin { value } => {
                s.spread.min = Some(value);
                (
                    Some(("spread:min", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }

            OscEvent::StateSpreadMax { value } => {
                s.spread.max = Some(value);
                (
                    Some(("spread:max", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateSpreadFromDistance { enabled } => {
                s.spread.from_distance = Some(enabled);
                (
                    Some((
                        "spread:from_distance",
                        serde_json::json!({ "enabled": enabled }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateSpreadDistanceRange { value } => {
                s.spread.distance_range = Some(value);
                (
                    Some((
                        "spread:distance_range",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateSpreadDistanceCurve { value } => {
                s.spread.distance_curve = Some(value);
                (
                    Some((
                        "spread:distance_curve",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::StateDistanceModel { value } => {
                s.distance_model.value = Some(value.clone());
                (
                    Some(("distance_model", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }

            OscEvent::StateRenderBackend { value } => {
                s.render_backend_state.selection = Some(value.clone());
                (
                    Some(("render_backend", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }

            OscEvent::StateRenderBackendEffective { value } => {
                s.render_backend_state.effective = Some(value.clone());
                (
                    Some(("render_backend:effective", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }

            OscEvent::StateRenderEvaluationMode { value } => {
                s.render_evaluation_mode_state.selection = Some(value.clone());
                (
                    Some(("render_evaluation_mode", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }

            OscEvent::StateRenderEvaluationModeEffective { value } => {
                s.render_evaluation_mode_state.effective = Some(value.clone());
                (
                    Some((
                        "render_evaluation_mode:effective",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::StateDebugSpeakerHeatmapMeta { value } => (
                Some((
                    "speaker_heatmap:meta",
                    serde_json::from_str(&value).unwrap_or_else(|_| serde_json::json!({})),
                )),
                removed_ids,
            ),

            OscEvent::StateDebugSpeakerHeatmapSliceXy { value } => (
                Some((
                    "speaker_heatmap:slice_xy",
                    serde_json::from_str(&value).unwrap_or_else(|_| serde_json::json!({})),
                )),
                removed_ids,
            ),

            OscEvent::StateDebugSpeakerHeatmapSliceXz { value } => (
                Some((
                    "speaker_heatmap:slice_xz",
                    serde_json::from_str(&value).unwrap_or_else(|_| serde_json::json!({})),
                )),
                removed_ids,
            ),

            OscEvent::StateDebugSpeakerHeatmapSliceYz { value } => (
                Some((
                    "speaker_heatmap:slice_yz",
                    serde_json::from_str(&value).unwrap_or_else(|_| serde_json::json!({})),
                )),
                removed_ids,
            ),

            OscEvent::StateDebugSpeakerHeatmapUnavailable { value } => (
                Some((
                    "speaker_heatmap:unavailable",
                    serde_json::from_str(&value).unwrap_or_else(|_| serde_json::json!({})),
                )),
                removed_ids,
            ),

            OscEvent::StateSnapshotComplete => {
                s.osc_snapshot_ready = true;
                let snapshot = serde_json::to_value(&*s).unwrap_or(serde_json::Value::Null);
                (Some(("state:snapshot_ready", snapshot)), removed_ids)
            }

            OscEvent::StateLoudness { enabled } => {
                s.loudness = Some(if enabled { 1 } else { 0 });
                (
                    Some((
                        "loudness",
                        serde_json::json!({ "enabled": if enabled { 1 } else { 0 } }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::StateLoudnessSource { value } => {
                s.loudness_source = Some(value);
                (
                    Some(("loudness:source", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }

            OscEvent::StateLoudnessGain { value } => {
                s.loudness_gain = Some(value);
                (
                    Some(("loudness:gain", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }

            OscEvent::StateMasterGain { value } => {
                s.master_gain = Some(value);
                (
                    Some(("master:gain", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }

            OscEvent::StateLatency { value } => {
                let rounded = s.set_latency_value(value);
                (
                    Some(("latency", serde_json::json!({ "value": rounded }))),
                    removed_ids,
                )
            }
            OscEvent::StateLatencyInstant { value } => {
                let rounded = s.set_latency_instant_value(value);
                (
                    Some((
                        "latency:instant",
                        serde_json::json!({ "value": rounded }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateLatencyControl { value } => {
                let rounded = s.set_latency_control_value(value);
                (
                    Some((
                        "latency:control",
                        serde_json::json!({ "value": rounded }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateLatencyTarget { value } => {
                let rounded = s.set_latency_target_value(value);
                (
                    Some((
                        "latency:target",
                        serde_json::json!({ "value": rounded }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateLatencyTargetRequested { value } => {
                let rounded = s.set_latency_requested_value(value);
                (
                    Some((
                        "latency:requested",
                        serde_json::json!({ "value": rounded }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateDecodeTimeMs { value } => {
                s.decode_time_ms = Some(value);
                (
                    Some(("decode:time_ms", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateRenderTimeMs { value } => {
                s.render_time_ms = Some(value);
                (
                    Some(("render:time_ms", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateWriteTimeMs { value } => {
                s.write_time_ms = Some(value);
                (
                    Some(("write:time_ms", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateFrameDurationMs { value } => {
                s.frame_duration_ms = Some(value);
                (
                    Some(("frame:duration_ms", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }

            OscEvent::StateResampleRatio { value } => {
                s.resample_ratio = Some(value);
                (
                    Some(("resample_ratio", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateAudioSampleRate { value } => {
                s.set_audio_sample_rate_value(value);
                (
                    Some(("audio:sample_rate", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateRampMode { value } => {
                s.audio.ramp_mode = Some(value.clone());
                (
                    Some(("state:ramp_mode", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateAudioOutputDevice { value } => {
                s.set_audio_requested_output_device(&value);
                (
                    Some(("audio:output_device", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateAudioOutputDeviceRequested { value } => {
                s.set_audio_requested_output_device(&value);
                (
                    Some((
                        "audio:output_device:requested",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateAudioOutputDeviceEffective { value } => {
                s.set_audio_effective_output_device(&value);
                (
                    Some((
                        "audio:output_device:effective",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateAudioOutputDevices { values } => {
                let parsed = values
                    .first()
                    .and_then(|json| serde_json::from_str::<Vec<OutputDeviceOption>>(json).ok())
                    .unwrap_or_default();
                s.set_audio_output_devices(parsed.clone());
                (
                    Some((
                        "audio:output_devices",
                        serde_json::json!({ "values": parsed }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateAudioSampleFormat { value } => {
                s.set_audio_sample_format(value.clone());
                (
                    Some(("audio:sample_format", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateAudioError { value } => {
                s.set_audio_error(&value);
                (
                    Some(("audio:error", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateInputMode { value } => {
                s.input_mode = Some(value.clone());
                (
                    Some(("input:mode", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateInputActiveMode { value } => {
                s.input_active_mode = Some(value.clone());
                (
                    Some(("input:active_mode", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateInputApplyPending { enabled } => {
                s.input_apply_pending = Some(if enabled { 1 } else { 0 });
                (
                    Some((
                        "input:apply_pending",
                        serde_json::json!({ "enabled": if enabled { 1 } else { 0 } }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateInputBackend { value } => {
                s.input_backend = Some(value.clone());
                (
                    Some(("input:backend", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateInputChannels { value } => {
                s.input_channels = Some(value);
                (
                    Some(("input:channels", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateInputSampleRate { value } => {
                s.input_sample_rate = Some(value);
                (
                    Some(("input:sample_rate", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateInputStreamFormat { value } => {
                s.input_stream_format = Some(value.clone());
                (
                    Some(("input:stream_format", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateInputError { value } => {
                s.input_error = if value.trim().is_empty() {
                    None
                } else {
                    Some(value.clone())
                };
                (
                    Some(("input:error", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateInputLiveBackend { value } => {
                s.live_input.backend = Some(value.clone());
                (
                    Some(("input:live:backend", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateInputLiveNode { value } => {
                s.live_input.node = Some(value.clone());
                (
                    Some(("input:live:node", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateInputLiveDescription { value } => {
                s.live_input.description = Some(value.clone());
                (
                    Some((
                        "input:live:description",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateInputLiveLayout { value } => {
                s.live_input.layout = Some(value.clone());
                (
                    Some(("input:live:layout", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateInputLiveClockMode { value } => {
                s.live_input.clock_mode = Some(value.clone());
                (
                    Some((
                        "input:live:clock_mode",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateInputLiveChannels { value } => {
                s.live_input.channels = Some(value);
                (
                    Some(("input:live:channels", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateInputLiveSampleRate { value } => {
                s.live_input.sample_rate = Some(value);
                (
                    Some((
                        "input:live:sample_rate",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateInputLiveFormat { value } => {
                s.live_input.format = Some(value.clone());
                (
                    Some(("input:live:format", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateInputLiveMap { value } => {
                s.live_input.map = Some(value.clone());
                (
                    Some(("input:live:map", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateInputLiveLfeMode { value } => {
                s.live_input.lfe_mode = Some(value.clone());
                (
                    Some(("input:live:lfe_mode", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateInputPipe { value } => {
                s.orender_input_pipe = if value.trim().is_empty() {
                    None
                } else {
                    Some(value.clone())
                };
                (
                    Some(("state:input_pipe", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }

            OscEvent::StateLogLevel { value } => {
                s.log_level = Some(value.clone());
                (
                    Some(("state:log_level", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }

            OscEvent::Log { entry } => (
                Some((
                    "omniphony:log",
                    serde_json::to_value::<LogEntry>(entry).unwrap_or_default(),
                )),
                removed_ids,
            ),

            OscEvent::StateDistanceDiffuseEnabled { enabled } => {
                s.distance_diffuse.enabled = Some(enabled);
                (
                    Some((
                        "distance_diffuse:enabled",
                        serde_json::json!({ "enabled": enabled }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::StateDistanceDiffuseThreshold { value } => {
                s.distance_diffuse.threshold = Some(value);
                (
                    Some((
                        "distance_diffuse:threshold",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::StateDistanceDiffuseCurve { value } => {
                s.distance_diffuse.curve = Some(value);
                (
                    Some((
                        "distance_diffuse:curve",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateRenderEvaluationCartesianXSize { value } => {
                s.vbap_cartesian.x_size = Some(value);
                (
                    Some((
                        "render_evaluation:cartesian:x_size",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateRenderEvaluationCartesianYSize { value } => {
                s.vbap_cartesian.y_size = Some(value);
                (
                    Some((
                        "render_evaluation:cartesian:y_size",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateRenderEvaluationCartesianZSize { value } => {
                s.vbap_cartesian.z_size = Some(value);
                (
                    Some((
                        "render_evaluation:cartesian:z_size",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateRenderEvaluationCartesianZNegSize { value } => {
                s.vbap_cartesian.z_neg_size = Some(value);
                (
                    Some((
                        "render_evaluation:cartesian:z_neg_size",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateRenderEvaluationPolarAzimuthResolution { value } => {
                s.vbap_polar.azimuth_resolution = Some(value);
                (
                    Some((
                        "render_evaluation:polar:azimuth_resolution",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateRenderEvaluationPolarElevationResolution { value } => {
                s.vbap_polar.elevation_resolution = Some(value);
                (
                    Some((
                        "render_evaluation:polar:elevation_resolution",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateRenderEvaluationPolarDistanceRes { value } => {
                s.vbap_polar.distance_res = Some(value);
                (
                    Some((
                        "render_evaluation:polar:distance_res",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateRenderEvaluationPolarDistanceMax { value } => {
                s.vbap_polar.distance_max = Some(value);
                (
                    Some((
                        "render_evaluation:polar:distance_max",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateRenderEvaluationPositionInterpolation { enabled } => {
                s.vbap_polar.position_interpolation = Some(enabled);
                (
                    Some((
                        "render_evaluation:position_interpolation",
                        serde_json::json!({ "enabled": enabled }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateVbapAllowNegativeZ { enabled } => {
                s.vbap_allow_negative_z = Some(enabled);
                (
                    Some((
                        "vbap:allow_negative_z",
                        serde_json::json!({ "enabled": enabled }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateSpeakersRecomputing { enabled } => {
                s.vbap_recomputing = Some(enabled);
                (
                    Some((
                        "vbap:recomputing",
                        serde_json::json!({ "enabled": enabled }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateAdaptiveResampling { enabled } => {
                s.adaptive_resampling = Some(if enabled { 1 } else { 0 });
                (
                    Some((
                        "adaptive_resampling",
                        serde_json::json!({ "enabled": if enabled { 1 } else { 0 } }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateAdaptiveResamplingEnableFarMode { enabled } => {
                s.adaptive_resampling_enable_far_mode = Some(if enabled { 1 } else { 0 });
                (
                    Some((
                        "adaptive_resampling:enable_far_mode",
                        serde_json::json!({ "enabled": s.adaptive_resampling_enable_far_mode }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateAdaptiveResamplingForceSilenceInFarMode { enabled } => {
                s.adaptive_resampling_force_silence_in_far_mode = Some(if enabled { 1 } else { 0 });
                (
                    Some((
                        "adaptive_resampling:force_silence_in_far_mode",
                        serde_json::json!({
                            "enabled": s.adaptive_resampling_force_silence_in_far_mode
                        }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateAdaptiveResamplingHardRecoverHighInFarMode { enabled } => {
                s.adaptive_resampling_hard_recover_high_in_far_mode =
                    Some(if enabled { 1 } else { 0 });
                (
                    Some((
                        "adaptive_resampling:hard_recover_high_in_far_mode",
                        serde_json::json!({
                            "enabled": s.adaptive_resampling_hard_recover_high_in_far_mode
                        }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateAdaptiveResamplingHardRecoverLowInFarMode { enabled } => {
                s.adaptive_resampling_hard_recover_low_in_far_mode =
                    Some(if enabled { 1 } else { 0 });
                (
                    Some((
                        "adaptive_resampling:hard_recover_low_in_far_mode",
                        serde_json::json!({
                            "enabled": s.adaptive_resampling_hard_recover_low_in_far_mode
                        }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateAdaptiveResamplingFarModeReturnFadeInMs { value } => {
                s.adaptive_resampling_far_mode_return_fade_in_ms = Some(value.round() as i64);
                (
                    Some((
                        "adaptive_resampling:far_mode_return_fade_in_ms",
                        serde_json::json!({
                            "value": s.adaptive_resampling_far_mode_return_fade_in_ms
                        }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateAdaptiveResamplingKpNear { value } => {
                s.adaptive_resampling_kp_near = Some(value);
                (
                    Some((
                        "adaptive_resampling:kp_near",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateAdaptiveResamplingKi { value } => {
                s.adaptive_resampling_ki = Some(value);
                (
                    Some((
                        "adaptive_resampling:ki",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateAdaptiveResamplingIntegralDischargeRatio { value } => {
                s.adaptive_resampling_integral_discharge_ratio = Some(value);
                (
                    Some((
                        "adaptive_resampling:integral_discharge_ratio",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateAdaptiveResamplingMaxAdjust { value } => {
                s.adaptive_resampling_max_adjust = Some(value);
                (
                    Some((
                        "adaptive_resampling:max_adjust",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateAdaptiveResamplingUpdateIntervalCallbacks { value } => {
                s.adaptive_resampling_update_interval_callbacks = Some(value.round() as i64);
                (
                    Some((
                        "adaptive_resampling:update_interval_callbacks",
                        serde_json::json!({ "value": s.adaptive_resampling_update_interval_callbacks }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateAdaptiveResamplingNearFarThresholdMs { value } => {
                s.adaptive_resampling_near_far_threshold_ms = Some(value.round() as i64);
                (
                    Some((
                        "adaptive_resampling:near_far_threshold_ms",
                        serde_json::json!({ "value": s.adaptive_resampling_near_far_threshold_ms }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateAdaptiveResamplingBand { value } => {
                s.adaptive_resampling_band = Some(value.clone());
                (
                    Some((
                        "adaptive_resampling:band",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateAdaptiveResamplingState { value } => {
                s.adaptive_resampling_state = Some(value.clone());
                (
                    Some((
                        "adaptive_resampling:state",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::StateAdaptiveResamplingPaused { enabled } => {
                s.adaptive_resampling_paused = Some(if enabled { 1 } else { 0 });
                (
                    Some((
                        "adaptive_resampling:pause",
                        serde_json::json!({ "enabled": if enabled { 1 } else { 0 } }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::StateConfigSaved { saved } => {
                s.config_saved = Some(if saved { 1 } else { 0 });
                (
                    Some((
                        "config:saved",
                        serde_json::json!({ "saved": if saved { 1 } else { 0 } }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::ConfigSpeakersCount { .. } | OscEvent::ConfigSpeaker { .. } => {
                // handled in bundle context via apply_speaker_config
                (None, removed_ids)
            }
        }
    }; // mutex released here, before any emit

    for id in removed_ids {
        let _ = app.emit("source:remove", serde_json::json!({ "id": id }));
    }

    if let Some((event, payload)) = to_emit {
        let _ = app.emit(event, payload);
    }
}
