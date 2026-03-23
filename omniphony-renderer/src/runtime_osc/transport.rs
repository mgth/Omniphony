use std::net::{SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::Mutex;
use std::time::Instant;

use anyhow::Result;
use rosc::{OscBundle, OscMessage, OscPacket, OscTime, OscType};

use super::{CLIENT_TIMEOUT, OscClientState, OscClients};

pub(crate) fn build_speaker_config_bundle(
    layout: &renderer::speaker_layout::SpeakerLayout,
) -> Result<Vec<u8>> {
    let mut messages = Vec::with_capacity(1 + layout.num_speakers());
    messages.push(OscPacket::Message(OscMessage {
        addr: "/omniphony/config/speakers".to_string(),
        args: vec![OscType::Int(layout.num_speakers() as i32)],
    }));
    for (idx, speaker) in layout.speakers.iter().enumerate() {
        messages.push(OscPacket::Message(OscMessage {
            addr: format!("/omniphony/config/speaker/{}", idx),
            args: vec![
                OscType::String(speaker.name.clone()),
                OscType::Float(speaker.azimuth),
                OscType::Float(speaker.elevation),
                OscType::Float(speaker.distance),
                OscType::Int(if speaker.spatialize { 1 } else { 0 }),
                OscType::Float(speaker.delay_ms),
                OscType::String(speaker.coord_mode.clone()),
                OscType::Float(speaker.x),
                OscType::Float(speaker.y),
                OscType::Float(speaker.z),
            ],
        }));
    }
    let bundle = OscPacket::Bundle(OscBundle {
        timetag: OscTime {
            seconds: 0,
            fractional: 1,
        },
        content: messages,
    });
    Ok(rosc::encoder::encode(&bundle)?)
}

pub(crate) fn broadcast_speaker_config(
    socket: &UdpSocket,
    clients: &Mutex<OscClients>,
    layout: &renderer::speaker_layout::SpeakerLayout,
) {
    match build_speaker_config_bundle(layout) {
        Ok(bytes) => send_raw(socket, clients, &bytes),
        Err(e) => log::warn!("OSC: failed to broadcast speaker config: {}", e),
    }
}

pub(crate) fn broadcast_float(
    socket: &UdpSocket,
    clients: &Mutex<OscClients>,
    addr: &str,
    value: f32,
) {
    let msg = OscMessage {
        addr: addr.to_string(),
        args: vec![OscType::Float(value)],
    };
    if let Ok(bytes) = rosc::encoder::encode(&OscPacket::Message(msg)) {
        send_raw(socket, clients, &bytes);
    }
}

pub(crate) fn broadcast_int(
    socket: &UdpSocket,
    clients: &Mutex<OscClients>,
    addr: &str,
    value: i32,
) {
    let msg = OscMessage {
        addr: addr.to_string(),
        args: vec![OscType::Int(value)],
    };
    if let Ok(bytes) = rosc::encoder::encode(&OscPacket::Message(msg)) {
        send_raw(socket, clients, &bytes);
    }
}

pub(crate) fn broadcast_fff(
    socket: &UdpSocket,
    clients: &Mutex<OscClients>,
    addr: &str,
    a: f32,
    b: f32,
    c: f32,
) {
    let msg = OscMessage {
        addr: addr.to_string(),
        args: vec![OscType::Float(a), OscType::Float(b), OscType::Float(c)],
    };
    if let Ok(bytes) = rosc::encoder::encode(&OscPacket::Message(msg)) {
        send_raw(socket, clients, &bytes);
    }
}

pub(crate) fn broadcast_string(
    socket: &UdpSocket,
    clients: &Mutex<OscClients>,
    addr: &str,
    value: &str,
) {
    let packet = OscPacket::Message(OscMessage {
        addr: addr.to_string(),
        args: vec![OscType::String(value.to_string())],
    });
    if let Ok(data) = rosc::encoder::encode(&packet) {
        send_raw(socket, clients, &data);
    }
}

pub(crate) fn encode_log_record(
    record: &sys::live_log::BufferedLogRecord,
) -> Option<Vec<u8>> {
    let packet = OscPacket::Message(OscMessage {
        addr: "/omniphony/log".to_string(),
        args: vec![
            OscType::Long(record.seq as i64),
            OscType::String(record.level.clone()),
            OscType::String(record.target.clone()),
            OscType::String(record.message.clone()),
        ],
    });
    rosc::encoder::encode(&packet).ok()
}

pub(crate) fn send_buffered_logs_to_client(socket: &UdpSocket, client: SocketAddr, last_seq: u64) {
    for record in sys::live_log::records_since(last_seq) {
        if let Some(bytes) = encode_log_record(&record) {
            if let Err(e) = socket.send_to(&bytes, client) {
                log::warn!("Failed to send log record to {}: {}", client, e);
                break;
            }
        }
    }
}

pub(crate) fn flush_pending_logs(
    socket: &UdpSocket,
    clients: &Mutex<OscClients>,
    last_seq: &mut u64,
) {
    let records = sys::live_log::records_since(*last_seq);
    if records.is_empty() {
        return;
    }
    for record in &records {
        if let Some(bytes) = encode_log_record(record) {
            send_raw(socket, clients, &bytes);
        }
    }
    if let Some(last) = records.last() {
        *last_seq = last.seq;
    }
}

pub(crate) fn send_raw(socket: &UdpSocket, clients: &Mutex<OscClients>, bytes: &[u8]) {
    send_raw_filtered(socket, clients, bytes, |_| true);
}

pub(crate) fn send_raw_filtered<F>(
    socket: &UdpSocket,
    clients: &Mutex<OscClients>,
    bytes: &[u8],
    predicate: F,
) where
    F: Fn(&OscClientState) -> bool,
{
    let mut clients_locked = clients.lock().unwrap();
    let now = Instant::now();
    clients_locked.retain(|addr, client| match client.last_seen {
        None => true,
        Some(t) => {
            if now.duration_since(t) >= CLIENT_TIMEOUT {
                log::info!("OSC client timed out, removing: {}", addr);
                false
            } else {
                true
            }
        }
    });
    for (addr, client) in clients_locked.iter() {
        if predicate(client) {
            if let Err(e) = socket.send_to(bytes, *addr) {
                log::warn!("OSC broadcast error to {}: {}", addr, e);
            }
        }
    }
}

pub(crate) fn send_metering_state(socket: &UdpSocket, client: SocketAddr, enabled: bool) {
    let packet = OscPacket::Message(OscMessage {
        addr: "/omniphony/state/osc/metering".to_string(),
        args: vec![OscType::Int(if enabled { 1 } else { 0 })],
    });
    if let Ok(bytes) = rosc::encoder::encode(&packet) {
        if let Err(e) = socket.send_to(&bytes, client) {
            log::warn!("Failed to send metering state to {}: {}", client, e);
        }
    }
}

pub(crate) fn resolve_register_addr(src: SocketAddr, args: &[OscType]) -> SocketAddr {
    if let Some(OscType::Int(port)) = args.first() {
        if let Ok(port) = u16::try_from(*port) {
            return match src {
                SocketAddr::V4(v4) => SocketAddr::V4(SocketAddrV4::new(*v4.ip(), port)),
                SocketAddr::V6(mut v6) => {
                    v6.set_port(port);
                    SocketAddr::V6(v6)
                }
            };
        }
    }
    src
}
