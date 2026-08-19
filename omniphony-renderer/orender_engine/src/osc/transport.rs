use std::net::{SocketAddr, SocketAddrV4, UdpSocket};

use rosc::{OscMessage, OscPacket, OscType};
use runtime_control::osc::{BroadcastUpdate, BroadcastValue};

use super::client_registry::OscClientRegistry;
use runtime_control::osc_contract;

pub(crate) fn broadcast_float(
    socket: &UdpSocket,
    clients: &OscClientRegistry,
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
    clients: &OscClientRegistry,
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
    clients: &OscClientRegistry,
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

pub(crate) fn broadcast_ffff(
    socket: &UdpSocket,
    clients: &OscClientRegistry,
    addr: &str,
    a: f32,
    b: f32,
    c: f32,
    d: f32,
) {
    let msg = OscMessage {
        addr: addr.to_string(),
        args: vec![
            OscType::Float(a),
            OscType::Float(b),
            OscType::Float(c),
            OscType::Float(d),
        ],
    };
    if let Ok(bytes) = rosc::encoder::encode(&OscPacket::Message(msg)) {
        send_raw(socket, clients, &bytes);
    }
}

pub(crate) fn broadcast_string(
    socket: &UdpSocket,
    clients: &OscClientRegistry,
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

/// Broadcast a single OSC `blob` arg (raw bytes). For bulk binary payloads such
/// as the chunked, compressed speaker gain table.
pub(crate) fn broadcast_blob(
    socket: &UdpSocket,
    clients: &OscClientRegistry,
    addr: &str,
    bytes: &[u8],
) {
    let packet = OscPacket::Message(OscMessage {
        addr: addr.to_string(),
        args: vec![OscType::Blob(bytes.to_vec())],
    });
    if let Ok(data) = rosc::encoder::encode(&packet) {
        send_raw(socket, clients, &data);
    }
}

pub(crate) fn encode_log_record(record: &sys::live_log::BufferedLogRecord) -> Option<Vec<u8>> {
    let packet = OscPacket::Message(OscMessage {
        addr: osc_contract::LOG.to_string(),
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
    clients: &OscClientRegistry,
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

pub(crate) fn send_raw(socket: &UdpSocket, clients: &OscClientRegistry, bytes: &[u8]) {
    send_raw_filtered(socket, clients, bytes, |_| true);
}

pub(crate) fn send_raw_filtered<F>(
    socket: &UdpSocket,
    clients: &OscClientRegistry,
    bytes: &[u8],
    predicate: F,
) where
    F: Fn(&super::client_registry::OscClientState) -> bool,
{
    clients.send_filtered(socket, bytes, predicate);
}

pub(crate) fn send_metering_state(socket: &UdpSocket, client: SocketAddr, enabled: bool) {
    let packet = OscPacket::Message(OscMessage {
        addr: osc_contract::STATE_OSC_METERING.to_string(),
        args: vec![OscType::Int(if enabled { 1 } else { 0 })],
    });
    if let Ok(bytes) = rosc::encoder::encode(&packet) {
        if let Err(e) = socket.send_to(&bytes, client) {
            log::warn!("Failed to send metering state to {}: {}", client, e);
        }
    }
}

pub(crate) fn send_diag_state(socket: &UdpSocket, client: SocketAddr, enabled: bool) {
    let packet = OscPacket::Message(OscMessage {
        addr: osc_contract::STATE_OSC_DIAG.to_string(),
        args: vec![OscType::Int(if enabled { 1 } else { 0 })],
    });
    if let Ok(bytes) = rosc::encoder::encode(&packet) {
        if let Err(e) = socket.send_to(&bytes, client) {
            log::warn!("Failed to send diag state to {}: {}", client, e);
        }
    }
}

/// Send a single [`BroadcastUpdate`] to one specific client (unicast), bypassing
/// the registry fan-out. Used to push the gain table only to the subscriber(s)
/// that asked for it.
pub(crate) fn send_update_to_client(
    socket: &UdpSocket,
    client: SocketAddr,
    update: &BroadcastUpdate,
) {
    let args = match &update.value {
        BroadcastValue::Int(i) => vec![OscType::Int(*i)],
        BroadcastValue::Float(f) => vec![OscType::Float(*f)],
        BroadcastValue::Fff(a, b, c) => {
            vec![OscType::Float(*a), OscType::Float(*b), OscType::Float(*c)]
        }
        BroadcastValue::String(s) => vec![OscType::String(s.clone())],
        BroadcastValue::Blob(b) => vec![OscType::Blob(b.clone())],
    };
    let packet = OscPacket::Message(OscMessage {
        addr: update.addr.clone(),
        args,
    });
    if let Ok(bytes) = rosc::encoder::encode(&packet) {
        if let Err(e) = socket.send_to(&bytes, client) {
            log::warn!("Failed to send {} to {}: {}", update.addr, client, e);
        }
    }
}

/// Send a multi-arg OSC message straight to one client (the requester), rather
/// than broadcasting to all registered clients. Used for point-to-point replies
/// such as backend-file content/list/error, which are only of interest to the
/// editor that asked.
pub(crate) fn send_message_to_client(
    socket: &UdpSocket,
    client: SocketAddr,
    addr: &str,
    args: Vec<OscType>,
) {
    let packet = OscPacket::Message(OscMessage {
        addr: addr.to_string(),
        args,
    });
    if let Ok(bytes) = rosc::encoder::encode(&packet) {
        if let Err(e) = socket.send_to(&bytes, client) {
            log::warn!("Failed to send {addr} to {client}: {e}");
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
