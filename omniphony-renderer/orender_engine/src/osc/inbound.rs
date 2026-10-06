//! Where the listener's packets come from (#680): datagrams on the OSC/UDP
//! socket, and packets on loopback TCP connections with OSC 1.0 stream
//! framing, on the same port number.
//!
//! One thread receives the datagrams, one accepts connections, and each
//! connection has a reader and a writer (see [`super::peer`]). Every packet
//! goes down one channel to the listener thread, which dispatches them all,
//! one at a time and in arrival order: the dispatch code and its state stay
//! single-threaded, as they were with one socket, and the packets of one
//! connection are handled in the order they were sent, which is what makes
//! `/omniphony/sync` a barrier.

use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

use super::RX_DATAGRAM_MAX;
use super::client_registry::OscClientRegistry;
use super::peer::{Peer, StreamPeer, read_frame};

/// Largest packet either side sends on a stream: a whole state snapshot
/// travels as one (see `export::MAX_STATE_STREAM_PACKET`).
pub(crate) const STREAM_PACKET_MAX: usize = 1 << 20;

/// Packets waiting for the listener thread. Datagrams beyond it are dropped,
/// as the kernel would drop them; a stream reader waits instead, which slows
/// only that connection.
const INBOUND_QUEUE: usize = 1024;

/// How often the receive and accept threads look at the stop flag.
const POLL: Duration = Duration::from_millis(50);

/// One packet for the listener, and who sent it.
pub(crate) struct Inbound {
    pub(crate) bytes: Vec<u8>,
    pub(crate) from: Peer,
}

/// The threads feeding the listener. Dropping it does not stop them: the
/// listener's stop flag does, then [`Self::join`].
pub(crate) struct Feeds {
    udp: Option<JoinHandle<()>>,
    tcp: Option<JoinHandle<()>>,
}

impl Feeds {
    /// Wait for the feeds to end, the stream listener first: once the
    /// datagram socket is released, the stream port is too, so a successor
    /// that bound the first can bind the second.
    pub(crate) fn join(mut self) {
        if let Some(handle) = self.tcp.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.udp.take() {
            let _ = handle.join();
        }
    }
}

/// Start feeding the listener from `udp` and, when it could be bound, from
/// `tcp`. Both threads end when `stop` is set.
pub(crate) fn spawn_feeds(
    udp: UdpSocket,
    tcp: Option<TcpListener>,
    clients: Arc<OscClientRegistry>,
    stop: Arc<AtomicBool>,
) -> std::io::Result<(Feeds, Receiver<Inbound>)> {
    let (tx, rx) = sync_channel(INBOUND_QUEUE);
    let udp = {
        let tx = tx.clone();
        let stop = Arc::clone(&stop);
        std::thread::Builder::new()
            .name("osc-udp-rx".into())
            .spawn(move || receive_datagrams(udp, tx, stop))?
    };
    let tcp = match tcp {
        Some(listener) => Some(
            std::thread::Builder::new()
                .name("osc-tcp-accept".into())
                .spawn(move || accept_streams(listener, tx, clients, stop))?,
        ),
        None => None,
    };
    Ok((
        Feeds {
            udp: Some(udp),
            tcp,
        },
        rx,
    ))
}

fn receive_datagrams(socket: UdpSocket, tx: SyncSender<Inbound>, stop: Arc<AtomicBool>) {
    let _ = socket.set_read_timeout(Some(POLL));
    // Large enough for any UDP datagram, allocated once: a truncated control
    // message (a backend file, a layout) would fail to decode and be lost.
    let mut buf = vec![0u8; RX_DATAGRAM_MAX];
    let mut dropped = 0u64;
    while !stop.load(Ordering::Relaxed) {
        match socket.recv_from(&mut buf) {
            Ok((len, src)) => {
                let packet = Inbound {
                    bytes: buf[..len].to_vec(),
                    from: Peer::Udp(src),
                };
                match tx.try_send(packet) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) => {
                        dropped += 1;
                        if dropped.is_power_of_two() {
                            log::warn!("OSC: listener busy, {dropped} datagram(s) dropped");
                        }
                    }
                    Err(TrySendError::Disconnected(_)) => return,
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) => {
                log::debug!("OSC recv error: {e}");
                // A failing socket returns at once: do not spin on it.
                std::thread::sleep(POLL);
            }
        }
    }
}

fn accept_streams(
    listener: TcpListener,
    tx: SyncSender<Inbound>,
    clients: Arc<OscClientRegistry>,
    stop: Arc<AtomicBool>,
) {
    if let Err(e) = listener.set_nonblocking(true) {
        log::warn!("OSC stream listener unusable ({e}); stream clients refused");
        return;
    }
    let mut open: Vec<Weak<StreamPeer>> = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, addr)) => match start_stream(stream, addr, tx.clone(), &clients) {
                Ok(peer) => {
                    open.retain(|p| p.strong_count() > 0);
                    open.push(Arc::downgrade(&peer));
                }
                Err(e) => log::warn!("OSC: could not start stream client {addr}: {e}"),
            },
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(POLL),
            Err(e) => {
                log::debug!("OSC stream accept error: {e}");
                std::thread::sleep(POLL);
            }
        }
    }
    // Leaving the port (standby, shutdown): the connections go with it, and
    // their clients reconnect to whoever takes the port next.
    for peer in open.iter().filter_map(Weak::upgrade) {
        peer.close();
    }
}

/// A new connection: its writer, its reader, and nothing in the registry
/// until it registers, like a datagram client.
fn start_stream(
    stream: TcpStream,
    addr: SocketAddr,
    tx: SyncSender<Inbound>,
    clients: &Arc<OscClientRegistry>,
) -> std::io::Result<Arc<StreamPeer>> {
    stream.set_nonblocking(false)?;
    // Packets are written whole, one at a time: no point waiting to coalesce.
    stream.set_nodelay(true)?;
    let peer = StreamPeer::new(stream.try_clone()?, addr);
    let writer_stream = stream.try_clone()?;
    {
        let peer = Arc::clone(&peer);
        std::thread::Builder::new()
            .name(format!("osc-tcp-tx-{}", peer.id()))
            .spawn(move || peer.run_writer(writer_stream))?;
    }
    {
        let peer = Arc::clone(&peer);
        let clients = Arc::clone(clients);
        std::thread::Builder::new()
            .name(format!("osc-tcp-rx-{}", peer.id()))
            .spawn(move || read_stream(stream, peer, tx, clients))?;
    }
    log::info!("OSC stream client connected: tcp#{} {addr}", peer.id());
    Ok(peer)
}

fn read_stream(
    mut stream: TcpStream,
    peer: Arc<StreamPeer>,
    tx: SyncSender<Inbound>,
    clients: Arc<OscClientRegistry>,
) {
    loop {
        match read_frame(&mut stream, STREAM_PACKET_MAX) {
            Ok(Some(bytes)) => {
                let packet = Inbound {
                    bytes,
                    from: Peer::Tcp(Arc::clone(&peer)),
                };
                if tx.send(packet).is_err() {
                    break;
                }
            }
            Ok(None) => break,
            Err(e) => {
                if !peer.is_closed() {
                    log::info!("OSC stream client tcp#{}: {e}", peer.id());
                }
                break;
            }
        }
    }
    peer.close();
    clients.remove(&Peer::Tcp(Arc::clone(&peer)));
    log::info!("OSC stream client disconnected: tcp#{}", peer.id());
}
