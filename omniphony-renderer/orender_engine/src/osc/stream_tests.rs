//! The stream transport end to end (#680): a real listener, a real loopback
//! TCP connection, OSC 1.0 stream framing.

use std::io::Write;
use std::net::TcpStream;
use std::time::{Duration, Instant};

use renderer::test_support::fixture_control;
use rosc::{OscMessage, OscPacket, OscType};
use runtime_control::osc_contract;

use super::peer::read_frame;
use super::test_support::{SERIAL, listening_sender};

struct Client {
    stream: TcpStream,
}

impl Client {
    fn connect(port: u16) -> Self {
        let stream = TcpStream::connect(("127.0.0.1", port)).expect("the stream transport listens");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        Self { stream }
    }

    fn send(&mut self, addr: &str, args: Vec<OscType>) {
        let bytes = rosc::encoder::encode(&OscPacket::Message(OscMessage {
            addr: addr.to_string(),
            args,
        }))
        .unwrap();
        let mut frame = (bytes.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(&bytes);
        self.stream.write_all(&frame).unwrap();
    }

    /// Every message received, in order, until `last` arrives (included), or
    /// `None` if the connection ends or stays silent for the read timeout (a
    /// long one: a timeout inside a frame would lose the stream's framing).
    fn until(&mut self, last: &str) -> Option<Vec<OscMessage>> {
        let mut messages = Vec::new();
        loop {
            let packet = match read_frame(&mut self.stream, super::inbound::STREAM_PACKET_MAX) {
                Ok(Some(packet)) => packet,
                Ok(None) => return None,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return None;
                }
                Err(_) => return None,
            };
            let (_, packet) = rosc::decoder::decode_udp(&packet).expect("an OSC packet");
            let mut found = false;
            flatten(packet, &mut |msg| {
                found |= msg.addr == last;
                messages.push(msg);
            });
            if found {
                return Some(messages);
            }
        }
    }
}

fn flatten(packet: OscPacket, out: &mut impl FnMut(OscMessage)) {
    match packet {
        OscPacket::Message(msg) => out(msg),
        OscPacket::Bundle(bundle) => {
            for packet in bundle.content {
                flatten(packet, out);
            }
        }
    }
}

fn position(messages: &[OscMessage], addr: &str) -> Option<usize> {
    messages.iter().position(|m| m.addr == addr)
}

/// A stream client registers like a datagram client and gets the snapshot,
/// in one part.
#[test]
fn a_stream_client_registers_and_gets_the_whole_snapshot_in_one_part() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let control = fixture_control();
    let (sender, port) = listening_sender(&control);
    let mut client = Client::connect(port);

    client.send(osc_contract::REGISTER, vec![OscType::Int(1)]);
    let messages = client
        .until(osc_contract::STATE_SNAPSHOT_COMPLETE)
        .expect("the snapshot reaches a stream client");
    let parts: Vec<_> = messages
        .iter()
        .filter(|m| m.addr == osc_contract::STATE_GENERATION)
        .map(|m| m.args.clone())
        .collect();
    assert_eq!(parts.len(), 1, "one snapshot part on a stream: {parts:?}");
    assert!(
        matches!(
            parts[0][..],
            [_, OscType::Int(1), OscType::Int(0), OscType::Int(1)]
        ),
        "full, part 0 of 1: {:?}",
        parts[0]
    );
    assert!(position(&messages, osc_contract::STATE_CAPABILITIES).is_some());
    drop(sender);
}

/// `/omniphony/sync` is a barrier: the refusals of the controls sent before
/// it, and the state a control changed, arrive before its ack, in order.
#[test]
fn the_sync_ack_follows_the_answers_to_every_earlier_control() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let control = fixture_control();
    let (sender, port) = listening_sender(&control);
    let mut client = Client::connect(port);
    client.send(osc_contract::REGISTER, vec![]);
    client.until(osc_contract::STATE_SNAPSHOT_COMPLETE).unwrap();

    client.send(osc_contract::CONTROL_UNKNOWN, vec![]);
    client.send(
        osc_contract::CONTROL_OVERLAY_ENABLED,
        vec![OscType::String("yes".into())],
    );
    client.send(osc_contract::CONTROL_GAIN, vec![OscType::Float(0.5)]);
    client.send(osc_contract::SYNC, vec![OscType::Int(7)]);
    let messages = client
        .until(osc_contract::SYNC_ACK)
        .expect("the sync is acked");

    let errors: Vec<_> = messages
        .iter()
        .filter(|m| m.addr == osc_contract::STATE_CONTROL_ERROR)
        .map(|m| m.args.get(1).cloned())
        .collect();
    assert_eq!(
        errors,
        [
            Some(OscType::String(
                osc_contract::CONTROL_ERROR_UNKNOWN_ADDRESS.into()
            )),
            Some(OscType::String(
                osc_contract::CONTROL_ERROR_INVALID_ARGUMENTS.into()
            )),
        ],
        "both refusals, in order, before the ack"
    );
    // The gain is this engine's own (the overlay, process-wide, may already
    // hold whatever a parallel test set): applying it marks the config
    // unsaved and republishes the live state, both before the ack.
    assert!(
        position(&messages, osc_contract::STATE_CONFIG_SAVED).is_some_and(|saved| {
            messages[saved].args == [OscType::Int(0)]
                && messages[saved..]
                    .iter()
                    .any(|m| m.addr == osc_contract::STATE_SNAPSHOT_COMPLETE)
        }),
        "the applied control's state is published before the ack"
    );
    assert_eq!(
        messages.last().map(|m| m.args.clone()),
        Some(vec![OscType::Int(7)]),
        "the ack echoes the sync's arguments"
    );
    drop(sender);
}

/// A control that starts a recompute is acked once it is dispatched: the
/// build's start (`recomputing = 1`) is before the ack, its end after.
#[test]
fn the_sync_ack_does_not_wait_for_a_recompute() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let control = fixture_control();
    let (sender, port) = listening_sender(&control);
    let mut client = Client::connect(port);
    client.send(osc_contract::REGISTER, vec![]);
    client.until(osc_contract::STATE_SNAPSHOT_COMPLETE).unwrap();

    client.send(
        osc_contract::CONTROL_ROOM_RATIO,
        vec![
            OscType::Float(1.5),
            OscType::Float(2.0),
            OscType::Float(1.0),
        ],
    );
    client.send(osc_contract::SYNC, vec![OscType::Int(1)]);
    let before = client
        .until(osc_contract::SYNC_ACK)
        .expect("the sync is acked");
    let started = before
        .iter()
        .any(|m| m.addr == osc_contract::STATE_SPEAKERS_RECOMPUTING && m.args == [OscType::Int(1)]);
    assert!(started, "the recompute's start is sent before the ack");

    // Its end comes after, as it does for a datagram client.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut ended = false;
    while !ended && Instant::now() < deadline {
        if let Some(messages) = client.until(osc_contract::STATE_SPEAKERS_RECOMPUTING) {
            ended = messages.iter().any(|m| {
                m.addr == osc_contract::STATE_SPEAKERS_RECOMPUTING && m.args == [OscType::Int(0)]
            });
        }
    }
    assert!(ended, "the recompute's end follows");
    drop(sender);
}

/// Closing the connection unregisters the client, and the listener keeps
/// serving the next one.
#[test]
fn a_closed_connection_is_forgotten_and_the_next_one_served() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let control = fixture_control();
    let (sender, port) = listening_sender(&control);
    {
        let mut client = Client::connect(port);
        client.send(osc_contract::REGISTER, vec![]);
        client.until(osc_contract::STATE_SNAPSHOT_COMPLETE).unwrap();
    }
    let mut client = Client::connect(port);
    client.send(osc_contract::SYNC, vec![]);
    assert!(client.until(osc_contract::SYNC_ACK).is_some());
    drop(sender);
}

/// Dropping the sender (a standby, a shutdown) ends the connections, so the
/// clients reconnect to whoever takes the port.
#[test]
fn stopping_the_listener_ends_the_connections() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let control = fixture_control();
    let (sender, port) = listening_sender(&control);
    let mut client = Client::connect(port);
    client.send(osc_contract::SYNC, vec![]);
    assert!(client.until(osc_contract::SYNC_ACK).is_some());
    sender
        .listener_stop
        .store(true, std::sync::atomic::Ordering::Relaxed);
    if let Some(handle) = sender.listener_thread.lock().unwrap().take() {
        handle.join().unwrap();
    }
    assert!(
        client.until("/never").is_none(),
        "the connection is closed once the listener stops"
    );
    assert!(
        TcpStream::connect(("127.0.0.1", port)).is_err(),
        "and the port is released"
    );
    drop(sender);
}
