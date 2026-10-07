//! Who a client is, and how a packet reaches it (#680).
//!
//! A client talks to the engine either in datagrams on the OSC/UDP socket, or
//! over a loopback TCP connection carrying the same OSC packets with OSC 1.0
//! stream framing (each packet preceded by its size, a big-endian int32). See
//! `docs/control-transport.md`. Everything that answers a client or fans
//! state out to them goes through [`Peer::send`], whatever the transport.
//!
//! A stream client has a bounded queue and a writer thread of its own:
//! publishers only push onto the queue, so a slow reader never holds up the
//! listener, the telemetry thread or a state publication (which send while
//! holding the registry and publication locks). When the queue is full,
//! telemetry is dropped, as a datagram would be; state never is: the client
//! is disconnected instead, and gets a fresh snapshot when it reconnects.

use std::collections::VecDeque;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::io::{self, Write};
use std::net::{Shutdown, SocketAddr, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

/// Bytes a stream client may have queued before it counts as too slow: a
/// few full snapshots (one is ~65 KB per part, a handful of parts).
pub(crate) const STREAM_QUEUE_MAX_BYTES: usize = 8 << 20;

/// Whether a packet may be lost when a stream client's queue is full.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Delivery {
    /// State and replies: never dropped. A client too slow for them is
    /// disconnected.
    Reliable,
    /// Telemetry (frames, meters, diag, timing, head pose, logs): dropped
    /// for a client too slow to take it, as a datagram is.
    Droppable,
}

/// A client: the address of a datagram client, or a stream connection.
#[derive(Clone)]
pub(crate) enum Peer {
    Udp(SocketAddr),
    Tcp(Arc<StreamPeer>),
}

impl Peer {
    /// Send one OSC packet (unframed: the stream framing is added here).
    pub(crate) fn send(&self, socket: &UdpSocket, bytes: &[u8]) -> io::Result<()> {
        self.send_with(socket, bytes, Delivery::Reliable)
    }

    pub(crate) fn send_with(
        &self,
        socket: &UdpSocket,
        bytes: &[u8],
        delivery: Delivery,
    ) -> io::Result<()> {
        match self {
            Self::Udp(addr) => socket.send_to(bytes, addr).map(|_| ()),
            Self::Tcp(peer) => peer.push(bytes, delivery),
        }
    }

    /// Whether the client is on this machine: what gates absolute paths in
    /// the backend-file controls. A stream client always is (the listener
    /// binds loopback only).
    pub(crate) fn is_loopback(&self) -> bool {
        match self {
            Self::Udp(addr) => addr.ip().is_loopback(),
            Self::Tcp(_) => true,
        }
    }

    /// A stream client whose connection has ended: it is forgotten.
    pub(crate) fn is_closed(&self) -> bool {
        match self {
            Self::Udp(_) => false,
            Self::Tcp(peer) => peer.is_closed(),
        }
    }

    /// A stream client's liveness is its connection, not a heartbeat.
    pub(crate) fn is_stream(&self) -> bool {
        matches!(self, Self::Tcp(_))
    }
}

impl From<SocketAddr> for Peer {
    fn from(addr: SocketAddr) -> Self {
        Self::Udp(addr)
    }
}

impl PartialEq for Peer {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Udp(a), Self::Udp(b)) => a == b,
            (Self::Tcp(a), Self::Tcp(b)) => a.id == b.id,
            _ => false,
        }
    }
}

impl Eq for Peer {}

impl Hash for Peer {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Self::Udp(addr) => {
                0u8.hash(state);
                addr.hash(state);
            }
            Self::Tcp(peer) => {
                1u8.hash(state);
                peer.id.hash(state);
            }
        }
    }
}

impl fmt::Display for Peer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Udp(addr) => write!(f, "{addr}"),
            Self::Tcp(peer) => write!(f, "tcp#{} {}", peer.id, peer.addr),
        }
    }
}

impl fmt::Debug for Peer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

static NEXT_STREAM_ID: AtomicU64 = AtomicU64::new(1);

/// One stream connection's sending side: the framed packets waiting for its
/// writer thread.
pub(crate) struct StreamPeer {
    id: u64,
    addr: SocketAddr,
    /// A handle on the connection, to shut it down from any thread (which
    /// also wakes the reader blocked on it).
    stream: TcpStream,
    queue: Mutex<Outbox>,
    ready: Condvar,
}

#[derive(Default)]
struct Outbox {
    frames: VecDeque<Vec<u8>>,
    bytes: usize,
    closed: bool,
    /// Telemetry packets dropped for a full queue since the last report.
    dropped: u64,
}

impl StreamPeer {
    /// Wrap an accepted connection. `stream` is a clone kept for shutdown;
    /// the reader and the writer own their own.
    pub(crate) fn new(stream: TcpStream, addr: SocketAddr) -> Arc<Self> {
        Arc::new(Self {
            id: NEXT_STREAM_ID.fetch_add(1, Ordering::Relaxed),
            addr,
            stream,
            queue: Mutex::new(Outbox::default()),
            ready: Condvar::new(),
        })
    }

    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.queue.lock().unwrap().closed
    }

    /// Queue one packet, framed. Never blocks on the connection.
    fn push(&self, packet: &[u8], delivery: Delivery) -> io::Result<()> {
        let Ok(size) = u32::try_from(packet.len()) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "packet too large",
            ));
        };
        let mut queue = self.queue.lock().unwrap();
        if queue.closed {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let framed_len = packet.len() + 4;
        if queue.bytes + framed_len > STREAM_QUEUE_MAX_BYTES {
            match delivery {
                Delivery::Droppable => {
                    queue.dropped += 1;
                    return Ok(());
                }
                Delivery::Reliable => {
                    drop(queue);
                    log::warn!(
                        "OSC: stream client tcp#{} {} is not reading its state; disconnecting it",
                        self.id,
                        self.addr
                    );
                    self.close();
                    return Err(io::ErrorKind::WouldBlock.into());
                }
            }
        }
        let mut frame = Vec::with_capacity(framed_len);
        frame.extend_from_slice(&size.to_be_bytes());
        frame.extend_from_slice(packet);
        queue.bytes += framed_len;
        queue.frames.push_back(frame);
        drop(queue);
        self.ready.notify_one();
        Ok(())
    }

    /// End the connection: nothing more is queued, the writer stops once
    /// woken, and the reader's next read fails.
    pub(crate) fn close(&self) {
        self.queue.lock().unwrap().closed = true;
        self.ready.notify_all();
        let _ = self.stream.shutdown(Shutdown::Both);
    }

    /// The writer thread's loop: write queued frames in order until the
    /// connection closes or a write fails.
    pub(crate) fn run_writer(&self, mut out: TcpStream) {
        loop {
            let (frame, dropped) = {
                let mut queue = self.queue.lock().unwrap();
                while queue.frames.is_empty() && !queue.closed {
                    queue = self.ready.wait(queue).unwrap();
                }
                if queue.closed {
                    return;
                }
                let frame = queue.frames.pop_front().expect("not empty");
                queue.bytes -= frame.len();
                (frame, std::mem::take(&mut queue.dropped))
            };
            if dropped > 0 {
                log::debug!(
                    "OSC: dropped {dropped} telemetry packet(s) for slow stream client tcp#{}",
                    self.id
                );
            }
            if let Err(e) = out.write_all(&frame) {
                log::debug!("OSC: stream client tcp#{} write failed: {e}", self.id);
                self.close();
                return;
            }
        }
    }
}

/// Read one framed packet from a stream: `Ok(None)` at a clean end of
/// stream, an error for a size over `max` or a connection that failed.
pub(crate) fn read_frame(input: &mut impl io::Read, max: usize) -> io::Result<Option<Vec<u8>>> {
    let mut size = [0u8; 4];
    match input.read_exact(&mut size) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let size = u32::from_be_bytes(size) as usize;
    if size > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a {size}-byte packet, over the {max}-byte limit"),
        ));
    }
    let mut packet = vec![0u8; size];
    input.read_exact(&mut packet)?;
    Ok(Some(packet))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn pair() -> (Arc<StreamPeer>, TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, addr) = listener.accept().unwrap();
        let peer = StreamPeer::new(server.try_clone().unwrap(), addr);
        (peer, server, client)
    }

    #[test]
    fn frames_round_trip_in_order() {
        let (peer, server, mut client) = pair();
        let writer = {
            let peer = Arc::clone(&peer);
            std::thread::spawn(move || peer.run_writer(server))
        };
        for packet in [&b"one"[..], b"", b"three!!"] {
            peer.push(packet, Delivery::Reliable).unwrap();
        }
        for want in [&b"one"[..], b"", b"three!!"] {
            assert_eq!(read_frame(&mut client, 64).unwrap().as_deref(), Some(want));
        }
        peer.close();
        writer.join().unwrap();
        assert!(peer.push(b"late", Delivery::Reliable).is_err());
    }

    #[test]
    fn an_oversized_frame_is_refused() {
        let mut bytes = (100u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(&[0; 100]);
        assert!(read_frame(&mut &bytes[..], 64).is_err());
        assert_eq!(
            read_frame(&mut &bytes[..], 100).unwrap().unwrap().len(),
            100
        );
        assert!(read_frame(&mut &[][..], 64).unwrap().is_none());
    }

    /// No writer drains the queue, as with a client that stopped reading:
    /// telemetry is dropped, then the first state that does not fit closes
    /// the connection.
    #[test]
    fn a_full_queue_drops_telemetry_and_closes_on_state() {
        let (peer, _server, _client) = pair();
        let packet = vec![0u8; 64 << 10];
        let fit = STREAM_QUEUE_MAX_BYTES / (packet.len() + 4);
        for _ in 0..fit {
            peer.push(&packet, Delivery::Reliable).unwrap();
        }
        peer.push(&packet, Delivery::Droppable).unwrap();
        assert!(!peer.is_closed(), "telemetry is dropped, not fatal");
        assert!(peer.push(&packet, Delivery::Reliable).is_err());
        assert!(
            peer.is_closed(),
            "state that cannot be queued closes the client"
        );
    }
}
