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
use std::sync::{Arc, Condvar, Mutex};
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
/// Bytes received and not yet handled by the listener. Past it the reader
/// stops reading, so the connection fills and the engine sees a slow client:
/// it drops telemetry for it, and disconnects it rather than drop state.
/// Studio holds no more than this (plus one packet) in its queue.
const RECEIVE_BUDGET: usize = 8 << 20;

/// What the listener receives from a stream: a packet, or its end.
pub(super) enum Inbound {
    Packet(Vec<u8>),
    Closed,
}

pub(super) struct StreamLink {
    /// The write half; the reader thread owns a clone.
    stream: TcpStream,
    packets: Receiver<Inbound>,
    budget: Arc<Budget>,
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
                link.budget.release(packet.len());
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
        *self = Self::Datagram;
    }

    /// Before registering with `target`: open a stream when the renderer is
    /// on this machine and no stream is open, unless a recent failure says
    /// not to yet. Stays on datagrams when the connection is refused.
    ///
    /// Says whether it opened one now: the caller then registers over it.
    pub(super) fn prepare(&mut self, target: SocketAddr, backoff: &Backoff) -> bool {
        if self.is_stream() || !target.ip().is_loopback() || backoff.active() {
            return false;
        }
        match connect(target) {
            Ok(link) => {
                log::info!("[osc] connected to {target} over TCP");
                *self = Self::Stream(link);
                true
            }
            Err(e) => {
                log::debug!("[osc] no stream transport at {target} ({e}); datagrams");
                false
            }
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

impl Drop for StreamLink {
    /// Ends the connection and frees the reader, whether it waits on the
    /// socket or on the budget.
    fn drop(&mut self) {
        self.budget.close();
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

/// The bytes the reader has handed over and the listener not yet taken.
#[derive(Default)]
struct Budget {
    state: Mutex<BudgetState>,
    freed: Condvar,
}

#[derive(Default)]
struct BudgetState {
    held: usize,
    closed: bool,
}

impl Budget {
    /// Wait until `len` more bytes fit (one packet always does when nothing
    /// is held), then count them. False once the link is closed.
    fn reserve(&self, len: usize) -> bool {
        let mut state = self.state.lock().unwrap();
        while !state.closed && state.held > 0 && state.held + len > RECEIVE_BUDGET {
            state = self.freed.wait(state).unwrap();
        }
        if state.closed {
            return false;
        }
        state.held += len;
        true
    }

    fn release(&self, len: usize) {
        let mut state = self.state.lock().unwrap();
        state.held = state.held.saturating_sub(len);
        self.freed.notify_all();
    }

    fn close(&self) {
        self.state.lock().unwrap().closed = true;
        self.freed.notify_all();
    }

    #[cfg(test)]
    fn held(&self) -> usize {
        self.state.lock().unwrap().held
    }
}

fn connect(target: SocketAddr) -> std::io::Result<StreamLink> {
    let stream = TcpStream::connect_timeout(&target, CONNECT_TIMEOUT)?;
    stream.set_nodelay(true)?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    let mut reader = stream.try_clone()?;
    let (tx, packets) = mpsc::channel();
    let budget = Arc::new(Budget::default());
    let reader_budget = Arc::clone(&budget);
    std::thread::Builder::new()
        .name("osc-stream-rx".into())
        .spawn(move || {
            loop {
                match stream::read_frame(&mut reader, stream::MAX_PACKET) {
                    Ok(Some(packet)) => {
                        // Read no further than the listener keeps up with.
                        if !reader_budget.reserve(packet.len())
                            || tx.send(Inbound::Packet(packet)).is_err()
                        {
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
        budget,
        peer: target,
        opened: Instant::now(),
        confirmed: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// A renderer sending faster than the listener handles: the reader stops
    /// at the budget instead of holding everything, and goes on once the
    /// listener takes packets.
    #[test]
    fn the_reader_holds_no_more_than_its_budget() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let link = connect(listener.local_addr().unwrap()).unwrap();
        let (mut engine, _) = listener.accept().unwrap();
        let packet = vec![0u8; stream::MAX_PACKET - 64];
        let sender = std::thread::spawn(move || {
            let frame = stream::frame(&packet).unwrap();
            for _ in 0..32 {
                if engine.write_all(&frame).is_err() {
                    return;
                }
            }
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while link.budget.held() + stream::MAX_PACKET <= RECEIVE_BUDGET {
            assert!(Instant::now() < deadline, "the reader fills its budget");
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(100));
        let held = link.budget.held();
        assert!(
            held <= RECEIVE_BUDGET + stream::MAX_PACKET,
            "{held} bytes held for a budget of {RECEIVE_BUDGET}"
        );
        // Taking packets lets the reader go on.
        let mut taken = 0;
        while let Ok(Inbound::Packet(packet)) = link.packets.recv_timeout(Duration::from_secs(5)) {
            link.budget.release(packet.len());
            taken += 1;
            if taken == 32 {
                break;
            }
        }
        assert_eq!(taken, 32, "every packet arrives once the listener keeps up");
        drop(link);
        sender.join().unwrap();
    }
}
