//! How the listener reaches its renderer (#680, step 2): datagrams on the
//! listener's UDP socket, as always, or the engine's stream transport, a
//! loopback TCP connection on the renderer's control port number carrying the
//! same OSC packets with OSC 1.0 stream framing (`osc_contract::stream`,
//! `docs/control-transport.md`).
//!
//! A renderer on this machine is tried over TCP first, every time the
//! listener (re)registers; a refused connection (an engine before contract
//! revision 2, or one that could not bind the port) falls back to datagrams at
//! once. A remote renderer is always reached by datagrams.
//!
//! On a stream nothing is lost and nothing arrives out of order, so the
//! repairs the listener runs for datagrams (snapshot re-requests, state
//! refreshes, gain-table NACKs) have nothing to repair; they stay in place for
//! the datagram fallback.

use std::io::Write;
use std::net::{Shutdown, SocketAddr, TcpStream, UdpSocket};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::osc_contract::stream;

/// A loopback connection is accepted or refused at once; this only bounds a
/// stuck one.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(300);
/// A renderer that stops reading for this long is gone; the write fails and
/// the listener reconnects.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
/// A connection that has not delivered one decodable OSC packet this long
/// after it opened is not an engine (some other service owns the port
/// number): it is dropped.
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(3);
/// After a connection that was not an engine, datagrams only for this long.
const UNCONFIRMED_BACKOFF: Duration = Duration::from_secs(30);
/// After an engine's connection closed, wait this long before trying TCP
/// again, so a renderer closing connections at once cannot spin the listener.
const CLOSED_BACKOFF: Duration = Duration::from_secs(1);

/// What the listener receives from a stream: a packet, or its end.
pub(super) enum Inbound {
    Packet(Vec<u8>),
    Closed,
}

pub(super) struct StreamLink {
    /// The write half; the reader thread owns a clone.
    stream: TcpStream,
    packets: Receiver<Inbound>,
    peer: SocketAddr,
    opened: Instant,
    /// A decodable OSC packet has arrived: this is an engine.
    confirmed: bool,
}

/// The listener's way to its renderer.
pub(super) enum Link {
    Datagram,
    Stream(StreamLink),
}

/// What one wait on the link gave.
pub(super) enum Received {
    /// A datagram, the first `n` bytes of the receive buffer.
    Datagram(usize, SocketAddr),
    /// A packet off the stream.
    Packet(Vec<u8>, SocketAddr),
    Timeout,
    /// The stream ended (or was dropped as not an engine): the listener is
    /// back on datagrams and must register again.
    StreamClosed,
}

impl Link {
    pub(super) fn is_stream(&self) -> bool {
        matches!(self, Self::Stream(_))
    }

    /// Send one OSC packet to the renderer at `to`. A stream that fails to
    /// take it is closed, which the next [`Self::receive`] reports.
    pub(super) fn send(
        &mut self,
        socket: &UdpSocket,
        to: SocketAddr,
        packet: &[u8],
    ) -> std::io::Result<()> {
        match self {
            Self::Datagram => socket.send_to(packet, to).map(|_| ()),
            Self::Stream(link) => {
                let framed = stream::frame(packet)?;
                let written = link.stream.write_all(&framed);
                if written.is_err() {
                    let _ = link.stream.shutdown(Shutdown::Both);
                }
                written
            }
        }
    }

    /// Wait up to `timeout` for the next packet: from the socket on
    /// datagrams, from the reader thread on a stream.
    pub(super) fn receive(
        &mut self,
        socket: &UdpSocket,
        buf: &mut [u8],
        timeout: Duration,
        backoff: &mut Backoff,
    ) -> std::io::Result<Received> {
        let Self::Stream(link) = self else {
            return match socket.recv_from(buf) {
                Ok((n, from)) => Ok(Received::Datagram(n, from)),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    Ok(Received::Timeout)
                }
                Err(e) => Err(e),
            };
        };
        let outcome = match link.packets.recv_timeout(timeout) {
            Ok(Inbound::Packet(packet)) => {
                if !link.confirmed {
                    link.confirmed = super::decode_datagram(&packet).is_ok();
                }
                return Ok(Received::Packet(packet, link.peer));
            }
            Ok(Inbound::Closed) | Err(RecvTimeoutError::Disconnected) => {
                if link.confirmed {
                    log::info!("[osc] stream to {} closed; back to datagrams", link.peer);
                    CLOSED_BACKOFF
                } else {
                    log::warn!(
                        "[osc] {} closed the stream before any OSC: not an engine",
                        link.peer
                    );
                    UNCONFIRMED_BACKOFF
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if link.confirmed || link.opened.elapsed() < CONFIRM_TIMEOUT {
                    return Ok(Received::Timeout);
                }
                log::warn!(
                    "[osc] no OSC from {} over TCP; using datagrams for {}s",
                    link.peer,
                    UNCONFIRMED_BACKOFF.as_secs()
                );
                UNCONFIRMED_BACKOFF
            }
        };
        backoff.until = Some(Instant::now() + outcome);
        self.close();
        Ok(Received::StreamClosed)
    }

    /// Drop the stream, if any, and go back to datagrams.
    pub(super) fn close(&mut self) {
        if let Self::Stream(link) = std::mem::replace(self, Self::Datagram) {
            let _ = link.stream.shutdown(Shutdown::Both);
        }
    }

    /// Before registering with `target`: open a stream when the renderer is
    /// on this machine and no stream is open, unless a recent failure says
    /// not to yet. Stays on datagrams when the connection is refused.
    pub(super) fn prepare(&mut self, target: SocketAddr, backoff: &Backoff) {
        if self.is_stream() || !target.ip().is_loopback() || backoff.active() {
            return;
        }
        match connect(target) {
            Ok(link) => {
                log::info!("[osc] connected to {target} over TCP");
                *self = Self::Stream(link);
            }
            Err(e) => log::debug!("[osc] no stream transport at {target} ({e}); datagrams"),
        }
    }
}

/// When the next TCP attempt may be made.
#[derive(Default)]
pub(super) struct Backoff {
    until: Option<Instant>,
}

impl Backoff {
    fn active(&self) -> bool {
        self.until.is_some_and(|until| Instant::now() < until)
    }

    /// A new target: whatever failed with the old one says nothing about it.
    pub(super) fn reset(&mut self) {
        self.until = None;
    }
}

fn connect(target: SocketAddr) -> std::io::Result<StreamLink> {
    let stream = TcpStream::connect_timeout(&target, CONNECT_TIMEOUT)?;
    stream.set_nodelay(true)?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    let mut reader = stream.try_clone()?;
    let (tx, packets) = mpsc::channel();
    std::thread::Builder::new()
        .name("osc-stream-rx".into())
        .spawn(move || {
            loop {
                match stream::read_frame(&mut reader, stream::MAX_PACKET) {
                    Ok(Some(packet)) => {
                        if tx.send(Inbound::Packet(packet)).is_err() {
                            return;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        log::debug!("[osc] stream read: {e}");
                        break;
                    }
                }
            }
            let _ = tx.send(Inbound::Closed);
        })?;
    Ok(StreamLink {
        stream,
        packets,
        peer: target,
        opened: Instant::now(),
        confirmed: false,
    })
}
