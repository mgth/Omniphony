use std::net::{SocketAddr, SocketAddrV4, UdpSocket};

use rosc::{OscBundle, OscMessage, OscPacket, OscTime, OscType};
use runtime_control::osc::{BroadcastUpdate, BroadcastValue};

use super::client_registry::OscClientRegistry;
use runtime_control::osc_contract;

/// Send buffer every socket that sends state or replies must have: larger
/// than any UDP payload, like the listener's receive buffer.
pub(crate) const TX_DATAGRAM_MAX: usize = 65_536;

/// Make sure `socket` can send the largest datagram the protocol carries.
///
/// macOS and the BSDs refuse a UDP send larger than the socket's send buffer
/// (`EMSGSIZE`), and that buffer starts at `net.inet.udp.maxdgram`: 9,216
/// bytes, where a state bundle runs to 65,000 and a backend file to 60,000.
/// The buffer is only ever raised: Linux starts well above this, and setting
/// it there would shrink it.
///
/// A failure is logged and the socket kept, since everything under the old
/// limit still goes through.
pub(crate) fn ensure_send_buffer(socket: &UdpSocket) {
    let socket = socket2::SockRef::from(socket);
    if socket
        .send_buffer_size()
        .is_ok_and(|size| size >= TX_DATAGRAM_MAX)
    {
        return;
    }
    if let Err(e) = socket.set_send_buffer_size(TX_DATAGRAM_MAX) {
        log::warn!(
            "OSC: could not raise the send buffer to {TX_DATAGRAM_MAX} bytes, larger datagrams may be refused: {e}"
        );
    }
}

/// Timetag of every bundle of state: "immediately".
pub(crate) const STATE_TIMETAG: OscTime = OscTime {
    seconds: 0,
    fractional: 1,
};

/// `/omniphony/state/generation [generation, full]`: what closes a snapshot
/// (`full`) or follows a single state update (see
/// [`osc_contract::STATE_GENERATION`]).
pub(crate) fn state_generation_message(generation: u32, full: bool) -> OscPacket {
    OscPacket::Message(OscMessage {
        addr: osc_contract::STATE_GENERATION.to_string(),
        // The wire's int is signed; the count is compared for equality only.
        args: vec![
            OscType::Int(generation as i32),
            OscType::Int(i32::from(full)),
        ],
    })
}

/// Broadcast one state update, versioned: it goes out in a bundle with the
/// next state generation, so a client that missed the one before it can tell.
/// For control-plane state only — a telemetry stream sent this way would move
/// the generation on every reading, and every client would keep asking for
/// snapshots.
fn broadcast_state(socket: &UdpSocket, clients: &OscClientRegistry, msg: OscMessage) {
    let generation = clients.advance_state_generation();
    let bundle = OscPacket::Bundle(OscBundle {
        timetag: STATE_TIMETAG,
        content: vec![
            OscPacket::Message(msg),
            state_generation_message(generation, false),
        ],
    });
    if let Ok(bytes) = rosc::encoder::encode(&bundle) {
        send_raw(socket, clients, &bytes);
    }
}

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
    broadcast_state(socket, clients, msg);
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
    broadcast_state(socket, clients, msg);
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
    broadcast_state(socket, clients, msg);
}

/// Not versioned, unlike the helpers above: its one user is the head pose,
/// a ~30 Hz stream.
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
    let msg = OscMessage {
        addr: addr.to_string(),
        args: vec![OscType::String(value.to_string())],
    };
    broadcast_state(socket, clients, msg);
}

/// Broadcast a single OSC `blob` arg (raw bytes). For bulk binary payloads such
/// as the chunked, compressed speaker gain table — which has its own versions
/// and resend, so this one is not versioned.
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

pub(crate) fn encode_log_record(record: &live_log::BufferedLogRecord) -> Option<Vec<u8>> {
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
    for record in live_log::records_since(last_seq) {
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
    let records = live_log::records_since(*last_seq);
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

/// `/omniphony/state/control_error [address, code, message]` to the sender of
/// a control the engine did not apply.
pub(crate) fn send_control_error(
    socket: &UdpSocket,
    client: SocketAddr,
    address: &str,
    code: &str,
    message: &str,
) {
    send_message_to_client(
        socket,
        client,
        osc_contract::STATE_CONTROL_ERROR,
        vec![
            OscType::String(address.to_string()),
            OscType::String(code.to_string()),
            OscType::String(message.to_string()),
        ],
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    fn send_buffer(socket: &UdpSocket) -> usize {
        socket2::SockRef::from(socket).send_buffer_size().unwrap()
    }

    fn socket_with_send_buffer(size: usize) -> UdpSocket {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket2::SockRef::from(&socket)
            .set_send_buffer_size(size)
            .unwrap();
        socket
    }

    /// "At least", since Linux reports twice what it was asked for.
    #[test]
    fn a_small_send_buffer_is_raised() {
        let socket = socket_with_send_buffer(8_192);
        assert!(send_buffer(&socket) < TX_DATAGRAM_MAX);
        ensure_send_buffer(&socket);
        assert!(send_buffer(&socket) >= TX_DATAGRAM_MAX);
    }

    /// Asking for the minimum on a socket already past it would shrink it,
    /// which is what a plain `set` does to the Linux default.
    #[test]
    fn a_large_send_buffer_is_left_alone() {
        let socket = socket_with_send_buffer(2 * TX_DATAGRAM_MAX);
        let before = send_buffer(&socket);
        assert!(before > TX_DATAGRAM_MAX);
        ensure_send_buffer(&socket);
        assert_eq!(send_buffer(&socket), before);
    }
}
