//! OSC transport: the listener thread and the synthetic feed.
//!
//! Parsing is the Tauri host's `osc_parser.rs` reused verbatim ([`parser`]);
//! the parsed events are applied to the live model by [`dispatch`]. The
//! listener also owns the client side of the renderer protocol: register,
//! heartbeat, re-register on an unknown-client reply, and the NACK timer of
//! the chunked gain-table transfer.
//!
//! Repaint policy: after each packet that changed the model the listener calls
//! the [`Waker`] the UI gave it, coalesced so a burst of per-object messages
//! never asks for more than one frame per ~2.5 ms. When nothing arrives,
//! nothing repaints.

pub mod apply;
pub mod dispatch;
mod link;
#[allow(dead_code)]
pub mod parser;
mod playout;
pub mod state_sync;

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rosc::{OscBundle, OscError, OscMessage, OscPacket, OscTime, OscType, decoder, encoder};

use crate::host::runtime::{StopToken, Worker};
use dispatch::{Change, Live, apply_event};
use link::{Backoff, Link, Received};
use parser::{CoordinateFormat, HeartbeatResponse, is_heartbeat_address, parse_osc_message};
use playout::{Offer, Playout};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
/// Re-register when no heartbeat ack came back for this long. The host's
/// `HEARTBEAT_ACK_TIMEOUT`, so both clients give up after the same delay.
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(10);
const REPAINT_COALESCE: Duration = Duration::from_micros(2500);
/// How long a receive waits when no change is waiting to be shown, so the
/// heartbeat, the snapshot requests and the control channel keep running.
const READ_TIMEOUT: Duration = Duration::from_millis(100);
/// How often the receive wakes while messages are held, to release the ones
/// the listener has reached. Fixed rather than the exact time to the next one,
/// which would change the socket's timeout (a syscall) on every pass; a
/// quarter of a 60 Hz frame is as late as a release can be.
const PLAYOUT_TICK: Duration = Duration::from_millis(4);
/// While `osc_snapshot_ready` is false, re-register this often (host value).
const SNAPSHOT_REQUEST_INTERVAL: Duration = Duration::from_secs(1);
const RECV_BUF: usize = 65_536;
/// While a local renderer is reached by datagrams, how often the stream is
/// tried again: after a stream closed (the next one may already be up), and
/// for an engine that came up with the stream after this client registered.
/// A refused loopback connection costs next to nothing.
const TCP_PROBE_INTERVAL: Duration = Duration::from_secs(2);
/// Send buffer both sending sockets must have: larger than any UDP payload,
/// like [`RECV_BUF`].
const SEND_BUF: usize = 65_536;

pub type SharedLive = Arc<Mutex<Live>>;

/// Asks whoever shows the model to draw it again. The UI supplies it (egui's
/// `request_repaint` today), so the core never names the toolkit. It is called
/// from the listener thread and must be cheap and non-blocking.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// Transport state shared by panels and host commands. Only the listener
/// changes registration; drawing never guesses connectivity from frame timing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    Initializing,
    Connected,
    Reconnecting,
}

pub struct OscStats {
    pub packets: AtomicU64,
    pub messages: AtomicU64,
    pub applied: AtomicU64,
    pub ignored: AtomicU64,
    pub heartbeat_acks: AtomicU64,
    pub registered: AtomicBool,
    /// Advances once per newly acknowledged registration, not per snapshot.
    pub connection_epoch: AtomicU64,
    pub listen_port: AtomicU64,
    /// Milliseconds since `start` at the last received packet.
    pub last_packet_ms: AtomicU64,
    pub start: Instant,
    /// Renderer the client is registered with (None = listen only).
    pub target: Mutex<Option<SocketAddr>>,
    /// The renderer is reached over its stream transport (TCP) right now,
    /// not by datagrams: what sizes the large transfers (#680, step 3).
    pub stream_link: AtomicBool,
}

impl OscStats {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            packets: AtomicU64::new(0),
            messages: AtomicU64::new(0),
            applied: AtomicU64::new(0),
            ignored: AtomicU64::new(0),
            heartbeat_acks: AtomicU64::new(0),
            registered: AtomicBool::new(false),
            connection_epoch: AtomicU64::new(0),
            listen_port: AtomicU64::new(0),
            last_packet_ms: AtomicU64::new(0),
            start: Instant::now(),
            target: Mutex::new(None),
            stream_link: AtomicBool::new(false),
        })
    }

    pub fn connection_state(&self) -> ConnectionState {
        if self.registered.load(Ordering::Relaxed) {
            ConnectionState::Connected
        } else if self.target.lock().unwrap().is_some() {
            ConnectionState::Reconnecting
        } else {
            ConnectionState::Initializing
        }
    }

    pub fn since_last_packet(&self) -> Option<Duration> {
        if self.packets.load(Ordering::Relaxed) == 0 {
            return None;
        }
        let last = Duration::from_millis(self.last_packet_ms.load(Ordering::Relaxed));
        Some(self.start.elapsed().saturating_sub(last))
    }
}

pub struct ListenerConfig {
    pub listen_port: u16,
    pub register: Option<SocketAddr>,
    /// Ask the renderer for meter streams right after registering
    /// (`/omniphony/control/metering`, the host's `osc_metering_enabled`).
    pub metering: bool,
    /// Hold what describes a block of audio until it is heard (`playout`),
    /// the user's "follow the sound" switch.
    pub playout_sync: bool,
}

/// Messages the UI sends to the renderer through the listener's socket: the
/// control messages of the Tauri host's `OscControlMsg`, plus the debug
/// subscriptions of the energy volumes.
#[derive(Debug, Clone)]
pub enum Control {
    /// One OSC message to the registered renderer (dropped when there is no
    /// target). Every `control_*` command of the host ends up here.
    Send {
        address: String,
        args: Vec<OscType>,
    },
    /// Point the client at another renderer: register there, request the
    /// snapshot, restate the metering choice.
    Reconnect {
        request: u64,
        target: SocketAddr,
    },
    /// Toggle the meter streams (sent now and again after every register).
    SetMetering {
        enabled: bool,
    },
    /// Follow the sound: show each block when it is heard (see `playout`).
    SetPlayoutSync {
        enabled: bool,
    },
    SubscribeGainTable {
        have_version: i32,
        speaker_index: i32,
    },
    UnsubscribeGainTable,
}

pub type ControlTx = Sender<Control>;

/// Resolve a `host:port` spec to the renderer's address.
///
/// A literal address takes the fast path; anything else is a hostname, and a
/// hostname means a DNS lookup that can block for as long as the resolver
/// takes. That is why it lives here: nothing on the UI's thread should be
/// waiting on a network service to answer.
pub fn resolve(target: &str) -> Option<SocketAddr> {
    let target = target.trim();
    if let Ok(addr) = target.parse::<SocketAddr>() {
        return addr.is_ipv4().then_some(addr);
    }
    use std::net::ToSocketAddrs;
    target
        .to_socket_addrs()
        .ok()?
        .find(std::net::SocketAddr::is_ipv4)
}

/// Make sure `socket` can send the largest datagram the protocol carries.
///
/// macOS and the BSDs refuse a UDP send larger than the socket's send buffer
/// (`EMSGSIZE`), and that buffer starts at `net.inet.udp.maxdgram`: 9,216
/// bytes, where a backend script runs to 60,000. The buffer is only ever
/// raised: Linux starts well above this, and setting it there would shrink it.
///
/// A failure is logged and the socket kept, since everything under the old
/// limit still goes through.
fn ensure_send_buffer(socket: &UdpSocket) {
    let socket = socket2::SockRef::from(socket);
    if socket.send_buffer_size().is_ok_and(|size| size >= SEND_BUF) {
        return;
    }
    if let Err(e) = socket.set_send_buffer_size(SEND_BUF) {
        log::warn!(
            "[osc] could not raise the send buffer to {SEND_BUF} bytes, larger messages may be refused: {e}"
        );
    }
}

/// Bind the socket and start the listener thread. Returns the bound port so a
/// `0` request can be reported (and fed by the synthetic generator).
pub fn spawn_listener(
    live: SharedLive,
    waker: Waker,
    stats: Arc<OscStats>,
    cfg: ListenerConfig,
) -> std::io::Result<(u16, ControlTx, Worker)> {
    let socket = UdpSocket::bind(("0.0.0.0", cfg.listen_port))?;
    // Every control message leaves through this socket, backend files included.
    ensure_send_buffer(&socket);
    let port = socket.local_addr()?.port();
    socket.set_read_timeout(Some(READ_TIMEOUT))?;
    stats.listen_port.store(u64::from(port), Ordering::Relaxed);
    let (tx, rx) = mpsc::channel();
    let worker = Worker::spawn("osc-listener", move |stop| {
        listener_loop(
            socket,
            port,
            live,
            waker,
            stats,
            cfg.register,
            cfg.metering,
            cfg.playout_sync,
            rx,
            stop,
        )
    })?;
    Ok((port, tx, worker))
}

#[allow(clippy::too_many_arguments)]
fn listener_loop(
    socket: UdpSocket,
    port: u16,
    live: SharedLive,
    waker: Waker,
    stats: Arc<OscStats>,
    register: Option<SocketAddr>,
    metering: bool,
    playout_sync: bool,
    control: Receiver<Control>,
    stop: StopToken,
) {
    let mut buf = vec![0u8; RECV_BUF];
    let mut playout = Playout::new(playout_sync);
    let mut last_heartbeat = Instant::now();
    let mut last_ack = Instant::now();
    let mut last_repaint = Instant::now() - REPAINT_COALESCE;
    // A change applied to the model but not yet handed to the waker, because
    // it landed inside the coalescing window. While one is waiting, the socket
    // waits no longer than the window, so the last packet of a burst still gets
    // its frame when nothing follows it.
    let mut repaint_pending = false;
    let mut read_timeout = READ_TIMEOUT;
    let mut last_snapshot_request = Instant::now();
    let mut register = register;
    let mut metering = metering;
    *stats.target.lock().unwrap() = register;
    // The way to the renderer: a stream when it is on this machine and
    // serves one, datagrams otherwise (see `link`).
    let mut link = Link::Datagram;
    let mut backoff = Backoff::default();
    let mut next_tcp_probe = Instant::now() + TCP_PROBE_INTERVAL;

    if let Some(addr) = register {
        register_with(&mut link, &backoff, &socket, addr, port, metering);
    }

    loop {
        let stream_packet: Vec<u8>;
        let incoming = match link.receive(&socket, &mut buf, read_timeout, &mut backoff) {
            Ok(Received::Datagram(n, from)) => Some((&buf[..n], from)),
            Ok(Received::Packet(packet, from)) => {
                stream_packet = packet;
                Some((&stream_packet[..], from))
            }
            Ok(Received::Timeout) => None,
            Ok(Received::StreamClosed) => {
                // The renderer went away, or the port was not an engine's:
                // register again, which tries the stream again when allowed.
                stats.registered.store(false, Ordering::Relaxed);
                repaint_pending = true;
                if let Some(addr) = register {
                    register_with(&mut link, &backoff, &socket, addr, port, metering);
                }
                None
            }
            Err(e) => {
                log::error!("[osc] recv failed: {e}");
                std::thread::sleep(Duration::from_millis(50));
                None
            }
        };
        match incoming {
            // Replies use a separate ephemeral socket; reject other IPs
            // without starving control draining or shutdown below.
            Some((packet, from)) if accepts_sender(register, from) => {
                let n = packet.len();
                stats.packets.fetch_add(1, Ordering::Relaxed);
                stats
                    .last_packet_ms
                    .store(stats.start.elapsed().as_millis() as u64, Ordering::Relaxed);
                match decode_datagram(packet) {
                    Ok(packet) => {
                        let mut outcome = PacketOutcome::default();
                        {
                            let mut model = live.lock().unwrap();
                            handle_packet(
                                &packet,
                                &mut model,
                                &stats,
                                &mut outcome,
                                &mut playout,
                                Instant::now(),
                            );
                            if outcome.producer_changed {
                                playout.reset();
                            }
                            model.playout_delay = playout.delay(Instant::now());
                            // Asked here, where the model is locked anyway:
                            // packets keep coming (meters, the heartbeat
                            // acks), so a lost reply is asked again soon.
                            outcome.refresh = model.state_sync.refresh_due(Instant::now());
                        }
                        // A state update went missing: the snapshot again,
                        // and nothing else.
                        if outcome.refresh
                            && let Some(addr) = register
                        {
                            log::debug!("[osc] state generation fell behind, asking for a refresh");
                            send_int(
                                &mut link,
                                &socket,
                                addr,
                                crate::osc_contract::CONTROL_STATE_REFRESH,
                                i32::from(port),
                            );
                        }
                        if outcome.reregister
                            && let Some(addr) = register
                        {
                            if outcome.lost_registration {
                                // Once per episode, on the transition: the
                                // unknown-client reply keeps arriving on the
                                // renderer's own heartbeat cadence, so logging
                                // every one of them at `warn` would repeat
                                // indefinitely.
                                log::warn!(
                                    "[osc] renderer does not know this client; re-registering"
                                );
                            } else {
                                log::debug!(
                                    "[osc] renderer does not know this client; re-registering"
                                );
                            }
                            register_with(&mut link, &backoff, &socket, addr, port, metering);
                        }
                        if outcome.ack {
                            last_ack = Instant::now();
                        }
                        if outcome.change != Change::None {
                            repaint_pending = true;
                        }
                    }
                    Err(e) => log::debug!("[osc] undecodable packet ({n} bytes): {e:?}"),
                }
            }
            Some(_) | None => {}
        }

        // What the listener has now reached, out of the queue and into the
        // model.
        let now = Instant::now();
        if playout.next_due_in(now) == Some(Duration::ZERO) {
            let mut outcome = PacketOutcome::default();
            {
                let mut model = live.lock().unwrap();
                playout.release_due(now, |m| {
                    handle_message(&m, &mut model, &stats, &mut outcome);
                });
                model.playout_delay = playout.delay(now);
            }
            if outcome.change != Change::None {
                repaint_pending = true;
            }
        }

        if repaint_pending && last_repaint.elapsed() >= REPAINT_COALESCE {
            repaint_pending = false;
            last_repaint = Instant::now();
            waker();
        }
        let mut wanted = if repaint_pending {
            REPAINT_COALESCE
        } else {
            READ_TIMEOUT
        };
        // Wake for the held messages, not only for the next packet.
        if playout.next_due_in(Instant::now()).is_some() {
            wanted = wanted.min(PLAYOUT_TICK);
        }
        if wanted != read_timeout {
            match socket.set_read_timeout(Some(wanted)) {
                Ok(()) => read_timeout = wanted,
                Err(e) => log::debug!("[osc] set_read_timeout({wanted:?}): {e}"),
            }
        }

        // Drain the control channel. `Reconnect` works without a target;
        // everything else needs one and is dropped otherwise (the host does
        // the same: no socket target, no send).
        while let Ok(msg) = control.try_recv() {
            match msg {
                Control::Reconnect { target, request } => {
                    register = Some(target);
                    *stats.target.lock().unwrap() = register;
                    stats.registered.store(false, Ordering::Relaxed);
                    last_ack = Instant::now();
                    last_snapshot_request = Instant::now();
                    playout.reset();
                    {
                        let mut model = live.lock().unwrap();
                        apply_connection_reset(&mut model, request);
                    }
                    repaint_pending = true;
                    // Another renderer: its own stream, if it serves one.
                    link.close();
                    backoff.reset();
                    register_with(&mut link, &backoff, &socket, target, port, metering);
                }
                Control::SetMetering { enabled } => {
                    metering = enabled;
                    if let Some(addr) = register {
                        send_ints(
                            &mut link,
                            &socket,
                            addr,
                            "/omniphony/control/metering",
                            &[i32::from(enabled)],
                        );
                    }
                }
                Control::SetPlayoutSync { enabled } => playout.set_enabled(enabled),
                Control::Send { address, args } => {
                    if let Some(addr) = register {
                        send_args(&mut link, &socket, addr, &address, args);
                    }
                }
                Control::SubscribeGainTable {
                    have_version,
                    speaker_index,
                } => {
                    if let Some(addr) = register {
                        send_ints(
                            &mut link,
                            &socket,
                            addr,
                            "/omniphony/control/debug/speaker_gaintable/subscribe",
                            &[have_version, speaker_index],
                        );
                    }
                }
                Control::UnsubscribeGainTable => {
                    if let Some(addr) = register {
                        send_ints(
                            &mut link,
                            &socket,
                            addr,
                            "/omniphony/control/debug/speaker_gaintable/unsubscribe",
                            &[],
                        );
                    }
                }
            }
        }

        stats.stream_link.store(link.is_stream(), Ordering::Relaxed);
        if stop.cancelled() {
            stats.registered.store(false, Ordering::Relaxed);
            stats.stream_link.store(false, Ordering::Relaxed);
            link.close();
            return;
        }
        if let Some(addr) = register {
            let now = Instant::now();
            // On datagrams with a renderer on this machine: try the stream
            // again on a timer of its own. Nothing else would once the
            // session is up, since a healthy datagram session never needs to
            // register again.
            if !link.is_stream() && now >= next_tcp_probe {
                next_tcp_probe = now + TCP_PROBE_INTERVAL;
                if link.prepare(addr, &backoff) {
                    last_snapshot_request = now;
                    register_with(&mut link, &backoff, &socket, addr, port, metering);
                }
            }
            // Until the renderer's state bundle has fully arrived, keep asking
            // for it (the host's `SNAPSHOT_REQUEST_INTERVAL`).
            if now.duration_since(last_snapshot_request) >= SNAPSHOT_REQUEST_INTERVAL
                && !live.lock().unwrap().app.osc_snapshot_ready
            {
                last_snapshot_request = now;
                log::debug!("[osc] snapshot not ready yet, re-requesting the live state bundle");
                register_with(&mut link, &backoff, &socket, addr, port, metering);
            }
            if now.duration_since(last_heartbeat) >= HEARTBEAT_INTERVAL {
                last_heartbeat = now;
                send_int(
                    &mut link,
                    &socket,
                    addr,
                    "/omniphony/heartbeat",
                    i32::from(port),
                );
                if stats.registered.load(Ordering::Relaxed)
                    && now.duration_since(last_ack) >= HEARTBEAT_TIMEOUT
                {
                    log::warn!("[osc] heartbeat timeout, re-registering");
                    stats.registered.store(false, Ordering::Relaxed);
                    repaint_pending = true;
                    // A stream that stopped answering is dead too.
                    link.close();
                    register_with(&mut link, &backoff, &socket, addr, port, metering);
                }
            }
            // Recover lost gain-table chunks (remote renderer case).
            for (version, missing) in apply::gaintable_check_nack(now) {
                log::debug!(
                    "[osc] gaintable {version}: re-requesting {} chunks",
                    missing.len()
                );
                for args in apply::gaintable_nack_messages(version, &missing) {
                    send_args(
                        &mut link,
                        &socket,
                        addr,
                        "/omniphony/control/debug/speaker_gaintable/nack",
                        args,
                    );
                }
            }
        }
    }
}

fn accepts_sender(target: Option<SocketAddr>, sender: SocketAddr) -> bool {
    target.is_none_or(|target| target.ip() == sender.ip())
}

/// Decode a datagram the listener accepted: `rosc`'s decoder, refusing first
/// what nests deeper than the contract allows.
///
/// `rosc` decodes nested bundles by recursion and frees nested arrays by
/// recursion, and [`RECV_BUF`] holds thousands of levels of either. Decoded,
/// one such datagram overflows the listener thread's stack, which aborts the
/// whole Studio. The contract's walk reads the nesting off the raw bytes
/// instead, without recursing, as the engine's listener has it do.
fn decode_datagram(datagram: &[u8]) -> Result<OscPacket, OscError> {
    crate::osc_contract::nesting::check(datagram).map_err(OscError::BadPacket)?;
    decoder::decode_udp(datagram).map(|(_, packet)| packet)
}

fn apply_connection_reset(model: &mut Live, request: u64) {
    reset_connection_model(model);
    if model.queued_connection_request == Some(request) {
        model.queued_connection_request = None;
    }
}

fn reset_connection_model(model: &mut Live) {
    // Keep local choices and logs. Every measurement, test run and fetched
    // schema belongs to the old producer, including auto-tune's revert data.
    model.app.reset_runtime_state();
    let app = std::mem::replace(
        &mut model.app,
        crate::model::app_state::AppState::new(Vec::new()),
    );
    let mut fresh = Live::new(app);
    // Terminal outcomes must survive until the view consumes them, even if
    // several producers come and go while the window is hidden.
    fresh.backend_file_error = model
        .backend_file_pending
        .take()
        .map(crate::host::services::backend_files::interrupted)
        .or_else(|| model.backend_file_error.take())
        .or_else(|| {
            model
                .backend_file_content
                .take()
                .map(|file| dispatch::BackendFileError {
                    backend: file.backend,
                    key: file.key,
                    failure: dispatch::BackendFileFailure::ConnectionChanged,
                })
        });
    fresh.overlay_prefs = model.overlay_prefs.take();
    fresh.interests = std::mem::take(&mut model.interests);
    fresh.diagnostics = std::mem::take(&mut model.diagnostics);
    fresh.diagnostics.restart();
    fresh.resampling = std::mem::take(&mut model.resampling);
    fresh.resampling.restart();
    fresh.resample_wanted = model.resample_wanted;
    fresh.log = std::mem::take(&mut model.log);
    fresh.snapshot_epoch = model.snapshot_epoch.wrapping_add(1);
    fresh.layout_context_generation = model.layout_context_generation.wrapping_add(1);
    fresh.pending_connection_request = model.pending_connection_request;
    fresh.queued_connection_request = model.queued_connection_request;
    *model = fresh;
}

#[derive(Default)]
struct PacketOutcome {
    change: Change,
    ack: bool,
    reregister: bool,
    /// The unknown-client reply that set `reregister` ended a registered
    /// episode (as opposed to repeating while one is already lost).
    lost_registration: bool,
    /// Another renderer answers now: nothing held is about its stream.
    producer_changed: bool,
    /// The state generation fell behind: ask for the snapshot.
    refresh: bool,
}

impl Default for Change {
    fn default() -> Self {
        Change::None
    }
}

/// Recurses into bundles, which [`decode_datagram`] lets through no deeper
/// than the contract's `MAX_NESTING`.
fn handle_packet(
    packet: &OscPacket,
    live: &mut Live,
    stats: &OscStats,
    out: &mut PacketOutcome,
    playout: &mut Playout,
    now: Instant,
) {
    match packet {
        OscPacket::Bundle(OscBundle { content, .. }) => {
            for p in content {
                handle_packet(p, live, stats, out, playout, now);
            }
        }
        OscPacket::Message(m) => match playout.offer(m, now) {
            Offer::Apply => handle_message(m, live, stats, out),
            // Counted when released, if it is held.
            Offer::Taken => {}
        },
    }
}

fn handle_message(m: &OscMessage, live: &mut Live, stats: &OscStats, out: &mut PacketOutcome) {
    stats.messages.fetch_add(1, Ordering::Relaxed);
    if m.addr == crate::osc_contract::STATE_SHUTDOWN {
        stats.registered.store(false, Ordering::Relaxed);
        live.app.osc_snapshot_ready = false;
        out.change = out.change.max(Change::Snapshot);
        return;
    }
    match is_heartbeat_address(&m.addr) {
        HeartbeatResponse::Ack => {
            // `[epoch, state_generation]`; an engine older than contract
            // revision 1 sends the epoch alone.
            let mut ints = m.args.iter().filter_map(|arg| match arg {
                OscType::Int(value) => Some(*value),
                _ => None,
            });
            let epoch = ints.next();
            let generation = ints.next();
            if let Some(epoch) = epoch {
                let changed = live
                    .app
                    .producer_epoch
                    .is_some_and(|previous| previous != epoch);
                if changed {
                    reset_connection_model(live);
                    out.producer_changed = true;
                    stats.registered.store(false, Ordering::Relaxed);
                    out.reregister = true;
                    out.change = out.change.max(Change::Snapshot);
                }
                live.app.producer_epoch = Some(epoch);
            }
            if let Some(generation) = generation {
                live.state_sync.on_ack(generation);
            }
            if !stats.registered.swap(true, Ordering::Relaxed) {
                stats.connection_epoch.fetch_add(1, Ordering::Relaxed);
                out.change = out.change.max(Change::Snapshot);
            }
            stats.heartbeat_acks.fetch_add(1, Ordering::Relaxed);
            out.ack = true;
            return;
        }
        HeartbeatResponse::Unknown => {
            // Remember whether this reply ends a registered episode, so the
            // loop logs the transition once and the retries at `debug`.
            out.lost_registration = stats.registered.swap(false, Ordering::Relaxed);
            out.reregister = true;
            out.change = out.change.max(Change::Snapshot);
            return;
        }
        HeartbeatResponse::None => {}
    }
    let format = if live.app.current_coordinate_format == 1 {
        CoordinateFormat::Polar
    } else {
        CoordinateFormat::Cartesian
    };
    match parse_osc_message(&m.addr, &m.args, format) {
        Some(ev) => {
            stats.applied.fetch_add(1, Ordering::Relaxed);
            let change = apply_event(live, ev);
            out.change = out.change.max(change);
        }
        None => {
            stats.ignored.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Register with a renderer: `/omniphony/register <listen_port>` followed by
/// the metering choice, exactly like the host's `send_register` +
/// `send_metering_enabled` pair.
/// (Re)register with `to`, over a stream when one can be opened (see
/// [`Link::prepare`]). On a stream, a heartbeat follows at once: its ack is
/// what marks the client connected, and a stream has no reason to wait the
/// heartbeat interval for it.
fn register_with(
    link: &mut Link,
    backoff: &Backoff,
    socket: &UdpSocket,
    to: SocketAddr,
    listen_port: u16,
    metering: bool,
) {
    link.prepare(to, backoff);
    send_register(link, socket, to, listen_port, metering);
    if link.is_stream() {
        send_int(
            link,
            socket,
            to,
            "/omniphony/heartbeat",
            i32::from(listen_port),
        );
    }
}

fn send_register(
    link: &mut Link,
    socket: &UdpSocket,
    to: SocketAddr,
    listen_port: u16,
    metering: bool,
) {
    send_int(
        link,
        socket,
        to,
        "/omniphony/register",
        i32::from(listen_port),
    );
    send_int(
        link,
        socket,
        to,
        "/omniphony/control/metering",
        i32::from(metering),
    );
    // `debug`, not `info`: the snapshot-retry timer re-registers every
    // SNAPSHOT_REQUEST_INTERVAL until the state bundle has arrived, so at
    // `info` this line alone streams for as long as no renderer answers - the
    // normal standalone state.
    log::debug!("[osc] register sent to {to} (listen_port={listen_port}, metering={metering})");
}

fn send_int(link: &mut Link, socket: &UdpSocket, to: SocketAddr, addr: &str, value: i32) {
    send_ints(link, socket, to, addr, &[value]);
}

fn send_ints(link: &mut Link, socket: &UdpSocket, to: SocketAddr, addr: &str, values: &[i32]) {
    send_args(
        link,
        socket,
        to,
        addr,
        values.iter().map(|v| OscType::Int(*v)).collect(),
    );
}

fn send_args(link: &mut Link, socket: &UdpSocket, to: SocketAddr, addr: &str, args: Vec<OscType>) {
    let msg = OscPacket::Message(OscMessage {
        addr: addr.to_owned(),
        args,
    });
    match encoder::encode(&msg) {
        Ok(bytes) => {
            if let Err(e) = link.send(socket, to, &bytes) {
                log::warn!("[osc] send {addr} to {to} failed: {e}");
            }
        }
        Err(e) => log::warn!("[osc] encode {addr} failed: {e:?}"),
    }
}

// ---------------------------------------------------------------------------
// Synthetic feed
// ---------------------------------------------------------------------------

/// Object names cycled by the synthetic feed. Half of them are CJK on purpose:
/// the labels are the CJK-rendering gate.
const SYNTH_NAMES: &[&str] = &[
    "dialogue",
    "音楽",
    "ambience_rain",
    "効果音",
    "helicopter",
    "环境声",
    "score",
    "对白",
    "footsteps",
    "音效",
];

/// Send `count` moving objects at `rate_hz` to the listener over loopback,
/// as one OSC bundle per tick using the renderer's `xyz` payload layout, plus
/// a `meta` message per object at start so kinds and labels are exercised.
pub fn spawn_synthetic(
    count: u32,
    rate_hz: f32,
    target_port: u16,
    stop_after: Option<Duration>,
) -> std::io::Result<Worker> {
    let socket = UdpSocket::bind(("127.0.0.1", 0))?;
    // One bundle per tick: past some fifty objects it outgrows the default
    // send buffer of macOS.
    ensure_send_buffer(&socket);
    socket.connect(("127.0.0.1", target_port))?;
    let period = Duration::from_secs_f32(1.0 / rate_hz.max(1.0));
    Worker::spawn("osc-synthetic", move |stop| {
        let start = Instant::now();
        let mut next = start;
        let mut generation: i64 = 0;
        let mut tick: u64 = 0;
        log::info!("[synthetic] {count} objects at {rate_hz} Hz to udp/{target_port}");
        while !stop.cancelled() {
            if let Some(limit) = stop_after
                && start.elapsed() >= limit
            {
                log::info!("[synthetic] stopped after {:.1} s", limit.as_secs_f32());
                return;
            }
            let t = start.elapsed().as_secs_f32();
            let mut content: Vec<OscPacket> = Vec::with_capacity(count as usize + 2);
            content.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/spatial/frame".into(),
                args: vec![
                    OscType::Long((t * 48_000.0) as i64),
                    OscType::Long(1),
                    OscType::Int(count as i32),
                    OscType::Int(0),
                ],
            }));
            for i in 0..count {
                let phase = i as f32 * std::f32::consts::TAU / count.max(1) as f32;
                let x = 0.9 * (0.7 * t + phase).sin();
                let y = 0.9 * (0.5 * t + 1.3 * phase).cos();
                let z = 0.45 + 0.45 * (0.3 * t + 0.7 * phase).sin();
                let name = SYNTH_NAMES[i as usize % SYNTH_NAMES.len()];
                if tick == 0 {
                    // Every fourth object is a bed channel, every fifth a
                    // height object, to exercise the kind colouring.
                    let fixed = i % 4 == 0;
                    let kind = if i % 5 == 0 { "height" } else { "" };
                    content.push(OscPacket::Message(OscMessage {
                        addr: format!("/omniphony/object/{i}/meta"),
                        args: vec![
                            OscType::Int(i32::from(fixed)),
                            OscType::String(format!("{name} {i}")),
                            OscType::Long(1),
                            OscType::String(kind.to_owned()),
                        ],
                    }));
                }
                content.push(OscPacket::Message(OscMessage {
                    addr: format!("/omniphony/object/{i}/xyz"),
                    args: vec![
                        OscType::Float(x),
                        OscType::Float(y),
                        OscType::Float(z),
                        OscType::Int(-1),
                        OscType::Int(0),
                        OscType::Int(0),
                        OscType::Int(0),
                        OscType::Long(generation),
                        OscType::String(format!("{name} {i}")),
                    ],
                }));
                // A slow level sweep so meter-driven visuals move.
                let level = -40.0 + 30.0 * (0.9 * t + phase).sin().abs();
                content.push(OscPacket::Message(OscMessage {
                    addr: format!("/omniphony/meter/object/{i}"),
                    args: vec![OscType::Float(level), OscType::Float(level - 6.0)],
                }));
            }
            generation += 1;
            tick += 1;
            let bundle = OscPacket::Bundle(OscBundle {
                timetag: OscTime {
                    seconds: 0,
                    fractional: 1,
                },
                content,
            });
            match encoder::encode(&bundle) {
                Ok(bytes) => {
                    if let Err(e) = socket.send(&bytes) {
                        log::warn!("[synthetic] send failed: {e}");
                    }
                }
                Err(e) => log::warn!("[synthetic] encode failed: {e:?}"),
            }
            next += period;
            let now = Instant::now();
            if next > now {
                stop.wait(Some(next - now));
            } else {
                // Fell behind (e.g. suspended); resync instead of bursting.
                next = now;
            }
        }
    })
}

/// The contract's bound on nesting, where the Studio's datagrams arrive.
#[cfg(test)]
mod nesting_tests {
    use super::*;
    use crate::osc_contract::nesting::{MAX_NESTING, nested_arrays, nested_bundles};

    #[test]
    fn the_decoder_refuses_what_nests_past_the_contracts_limit() {
        for nested in [nested_bundles, nested_arrays] {
            assert!(decode_datagram(&nested(MAX_NESTING)).is_ok());
            let refused = decode_datagram(&nested(MAX_NESTING + 1)).unwrap_err();
            assert!(refused.to_string().contains("nested too deep"), "{refused}");
        }
    }

    /// Datagrams nested thousands of levels deep, in bundles and in arrays,
    /// are dropped and the listener goes on listening. Decoded, either one
    /// overflows the listener thread's stack, which aborts the whole process.
    /// A datagram one level past the limit is dropped the same way; one at
    /// the limit is decoded and its message handled.
    #[test]
    fn a_deeply_nested_datagram_does_not_take_the_listener_down() {
        let renderer = UdpSocket::bind("127.0.0.1:0").unwrap();
        let stats = OscStats::new();
        let (port, _control, mut worker) = spawn_listener(
            Arc::new(Mutex::new(Live::new(
                crate::model::app_state::AppState::new(Vec::new()),
            ))),
            Arc::new(|| {}),
            stats.clone(),
            ListenerConfig {
                listen_port: 0,
                register: Some(renderer.local_addr().unwrap()),
                metering: false,
                playout_sync: false,
            },
        )
        .unwrap();

        // Any socket at the renderer's address is listened to.
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        // The two large datagrams have the same send limit to get past as
        // the Studio's own.
        ensure_send_buffer(&sender);
        let awaited = |what: &str, counter: &AtomicU64, count: u64| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while counter.load(Ordering::Relaxed) != count {
                assert!(Instant::now() < deadline, "{what}");
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        // One at a time, so that none waits in the socket's buffer behind
        // another and all of them reach the decoder.
        let mut sent = 0;
        let mut send = |datagram: &[u8]| {
            sender.send_to(datagram, ("127.0.0.1", port)).unwrap();
            sent += 1;
            awaited("the listener receives the datagram", &stats.packets, sent);
        };

        for nested in [nested_bundles(3_000), nested_arrays(30_000)] {
            assert_eq!(nested.len(), 60_008);
            send(&nested);
        }
        send(&nested_bundles(MAX_NESTING + 1));
        send(&nested_arrays(MAX_NESTING + 1));
        send(&nested_bundles(MAX_NESTING));
        send(&nested_arrays(MAX_NESTING));
        let ack = encoder::encode(&OscPacket::Message(OscMessage {
            addr: crate::osc_contract::HEARTBEAT_ACK.into(),
            args: vec![],
        }))
        .unwrap();
        send(&ack);

        awaited("the ack is handled", &stats.heartbeat_acks, 1);
        // The one message of each datagram at the limit, and the ack: nothing
        // of the four refused ones was handled.
        assert_eq!(stats.messages.load(Ordering::Relaxed), 3);
        worker.shutdown();
    }
}

#[cfg(test)]
mod connection_tests {
    use super::*;

    #[test]
    fn listener_shutdown_flushes_final_commands_and_releases_its_port() {
        for _ in 0..4 {
            let renderer = UdpSocket::bind("127.0.0.1:0").unwrap();
            renderer
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let live = Arc::new(Mutex::new(Live::new(
                crate::model::app_state::AppState::new(Vec::new()),
            )));
            let (port, tx, mut worker) = spawn_listener(
                live.clone(),
                Arc::new(|| {}),
                OscStats::new(),
                ListenerConfig {
                    listen_port: 0,
                    register: Some(renderer.local_addr().unwrap()),
                    metering: false,
                    playout_sync: true,
                },
            )
            .unwrap();
            tx.send(Control::Send {
                address: "/test/final-stop".into(),
                args: vec![],
            })
            .unwrap();
            worker.shutdown();
            let mut observed = false;
            let mut buffer = [0; 2048];
            // Two registration messages precede the final command.
            for _ in 0..3 {
                let (size, _) = renderer.recv_from(&mut buffer).unwrap();
                if let Ok((_, OscPacket::Message(message))) = decoder::decode_udp(&buffer[..size]) {
                    observed |= message.addr == "/test/final-stop";
                }
            }
            assert!(observed);
            assert_eq!(Arc::strong_count(&live), 1);
            let rebound = UdpSocket::bind(("0.0.0.0", port)).unwrap();
            drop(rebound);
        }
    }

    #[test]
    fn replies_from_a_separate_renderer_socket_are_accepted() {
        let control = UdpSocket::bind("127.0.0.1:0").unwrap();
        let replies = UdpSocket::bind("127.0.0.1:0").unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        replies
            .send_to(b"reply", client.local_addr().unwrap())
            .unwrap();
        let (_, sender) = client.recv_from(&mut [0; 64]).unwrap();
        assert_ne!(control.local_addr().unwrap().port(), sender.port());
        assert!(accepts_sender(Some(control.local_addr().unwrap()), sender));
        assert!(!accepts_sender(
            Some("127.0.0.2:9000".parse().unwrap()),
            sender
        ));
        assert!(accepts_sender(None, sender));
    }

    #[test]
    fn registration_epochs_ignore_repeated_acks_and_track_producer_swaps() {
        let stats = OscStats::new();
        let mut live = Live::new(crate::model::app_state::AppState::new(Vec::new()));
        assert_eq!(stats.connection_state(), ConnectionState::Initializing);
        *stats.target.lock().unwrap() = Some("127.0.0.1:9000".parse().unwrap());
        assert_eq!(stats.connection_state(), ConnectionState::Reconnecting);
        let ack = |epoch| OscMessage {
            addr: "/omniphony/heartbeat/ack".into(),
            args: vec![OscType::Int(epoch)],
        };
        handle_message(&ack(1), &mut live, &stats, &mut PacketOutcome::default());
        assert_eq!(stats.connection_state(), ConnectionState::Connected);
        assert_eq!(stats.connection_epoch.load(Ordering::Relaxed), 1);
        let mut unchanged = PacketOutcome::default();
        handle_message(&ack(1), &mut live, &stats, &mut unchanged);
        assert_eq!(unchanged.change, Change::None);
        assert_eq!(stats.connection_epoch.load(Ordering::Relaxed), 1);
        live.app.orender_input_pipe = Some("stale".into());
        let mut swapped = PacketOutcome::default();
        handle_message(&ack(2), &mut live, &stats, &mut swapped);
        assert!(swapped.reregister);
        assert!(live.app.orender_input_pipe.is_none());
        assert_eq!(live.app.producer_epoch, Some(2));
        assert_eq!(stats.connection_epoch.load(Ordering::Relaxed), 2);
        handle_message(
            &OscMessage {
                addr: "/omniphony/heartbeat/unknown".into(),
                args: vec![],
            },
            &mut live,
            &stats,
            &mut PacketOutcome::default(),
        );
        assert_eq!(stats.connection_state(), ConnectionState::Reconnecting);
    }

    /// The review's loss case, through the real parser and dispatch: the
    /// first datagram of a two-part snapshot is lost and the last one arrives,
    /// `snapshot_complete` and all. The acknowledgement that follows reports
    /// the snapshot's generation, and the client still asks again, and is
    /// current once the refresh is in.
    #[test]
    fn a_split_snapshot_missing_its_first_part_is_asked_again() {
        use crate::osc_contract;
        let stats = OscStats::new();
        *stats.target.lock().unwrap() = Some("127.0.0.1:9000".parse().unwrap());
        let mut live = Live::new(crate::model::app_state::AppState::new(Vec::new()));
        let message = |addr: &str, args: Vec<OscType>| OscMessage {
            addr: addr.into(),
            args,
        };
        let generation = |part, parts| {
            message(
                osc_contract::STATE_GENERATION,
                vec![
                    OscType::Int(4),
                    OscType::Int(1),
                    OscType::Int(part),
                    OscType::Int(parts),
                ],
            )
        };
        let mut handle = |m: &OscMessage, live: &mut Live| {
            handle_message(m, live, &stats, &mut PacketOutcome::default())
        };
        // Part 0 of 2, carrying the log level, never arrives.
        handle(&generation(1, 2), &mut live);
        handle(
            &message(osc_contract::STATE_SNAPSHOT_COMPLETE, vec![OscType::Int(1)]),
            &mut live,
        );
        handle(
            &message(
                osc_contract::HEARTBEAT_ACK,
                vec![OscType::Int(7), OscType::Int(4)],
            ),
            &mut live,
        );
        assert!(live.app.osc_snapshot_ready);
        assert_ne!(
            live.app.log_level.as_deref(),
            Some("debug"),
            "the lost part's state is missing"
        );
        assert!(
            live.state_sync.refresh_due(Instant::now()),
            "and the client knows it"
        );

        // The refresh: the same generation, whole.
        for m in [
            generation(0, 1),
            message(
                osc_contract::STATE_LOG_LEVEL,
                vec![OscType::String("debug".into())],
            ),
            message(osc_contract::STATE_SNAPSHOT_COMPLETE, vec![OscType::Int(1)]),
        ] {
            handle(&m, &mut live);
        }
        assert_eq!(live.app.log_level.as_deref(), Some("debug"));
        assert!(!live.state_sync.is_stale());
    }

    #[test]
    fn graceful_shutdown_disconnects_immediately_after_a_fresh_ack() {
        let stats = OscStats::new();
        *stats.target.lock().unwrap() = Some("127.0.0.1:9000".parse().unwrap());
        let mut live = Live::new(crate::model::app_state::AppState::new(Vec::new()));
        handle_message(
            &OscMessage {
                addr: "/omniphony/heartbeat/ack".into(),
                args: vec![],
            },
            &mut live,
            &stats,
            &mut PacketOutcome::default(),
        );
        let mut outcome = PacketOutcome::default();
        handle_message(
            &OscMessage {
                addr: crate::osc_contract::STATE_SHUTDOWN.into(),
                args: vec![],
            },
            &mut live,
            &stats,
            &mut outcome,
        );
        assert_eq!(stats.connection_state(), ConnectionState::Reconnecting);
        assert_eq!(outcome.change, Change::Snapshot);
    }

    #[test]
    fn input_pipe_is_applied_and_repaints_only_on_change() {
        let mut live = Live::new(crate::model::app_state::AppState::new(Vec::new()));
        let event = || parser::OscEvent::StateInputPipe {
            value: "input.pipe".into(),
        };
        assert_eq!(apply_event(&mut live, event()), Change::Snapshot);
        assert_eq!(live.app.orender_input_pipe.as_deref(), Some("input.pipe"));
        assert_eq!(apply_event(&mut live, event()), Change::None);
    }
    #[test]
    fn native_file_choices_wait_for_the_matching_transport_transition() {
        use crate::host::commands::{app, layout_io::SessionToken};
        let state = crate::host::commands::tests::state();
        app::connect_to(&state, "127.0.0.1", 9000).unwrap();
        let request = state.read().queued_connection_request.unwrap();
        let queued_choice = SessionToken::new(&state);
        assert!(!queued_choice.is_current(&state));
        apply_connection_reset(&mut state.inner.lock().unwrap(), request);
        assert!(!queued_choice.is_current(&state));
        assert!(SessionToken::new(&state).is_current(&state));

        app::connect_to(&state, "127.0.0.2", 9000).unwrap();
        let queued = state.read().queued_connection_request.unwrap();
        let dns = app::begin_connection(&state);
        // A producer restart on the old endpoint cannot finish either phase.
        reset_connection_model(&mut state.inner.lock().unwrap());
        assert_eq!(state.read().pending_connection_request, Some(dns));
        assert_eq!(state.read().queued_connection_request, Some(queued));
        apply_connection_reset(&mut state.inner.lock().unwrap(), queued);
        assert_eq!(state.read().queued_connection_request, None);
        assert_eq!(state.read().pending_connection_request, Some(dns));
        assert!(!SessionToken::new(&state).is_current(&state));
    }

    #[test]
    fn draining_an_old_reconnect_does_not_release_a_newer_queued_transition() {
        use crate::host::commands::{app, layout_io::SessionToken};
        let state = crate::host::commands::tests::state();
        app::connect_to(&state, "127.0.0.1", 9000).unwrap();
        let first = state.read().queued_connection_request.unwrap();
        app::connect_to(&state, "127.0.0.2", 9000).unwrap();
        let last = state.read().queued_connection_request.unwrap();
        apply_connection_reset(&mut state.inner.lock().unwrap(), first);
        assert_eq!(state.read().queued_connection_request, Some(last));
        assert!(!SessionToken::new(&state).is_current(&state));
        apply_connection_reset(&mut state.inner.lock().unwrap(), last);
        assert!(SessionToken::new(&state).is_current(&state));
    }

    #[test]
    fn transport_reset_invalidates_native_file_choices_before_new_ack() {
        let state = crate::host::commands::tests::state();
        crate::host::commands::app::begin_connection(&state);
        let token = crate::host::commands::layout_io::SessionToken::new(&state);
        reset_connection_model(&mut state.inner.lock().unwrap());
        assert!(!token.is_current(&state));
    }
    #[test]
    fn resampler_arrivals_survive_no_frames_and_reconnect_resets_the_trace() {
        let state = crate::host::commands::tests::state();
        crate::host::diagnostics::select_resample(&state, true);
        let mut trace = crate::host::diagnostics::Trace::default();
        {
            let mut live = state.inner.lock().unwrap();
            for value in [10.0, 20.0, 30.0] {
                apply_event(&mut live, parser::OscEvent::StateLatencySmoothed { value });
            }
            apply_event(
                &mut live,
                parser::OscEvent::StateResampleRatio { value: 1.001 },
            );
            live.resampling.copy_to(&mut trace);
            live.resampling.copy_to(&mut trace);
            assert_eq!(trace.series["latency"].len(), 3);
            assert_eq!(trace.series["ppm"].len(), 1);
            reset_connection_model(&mut live);
            apply_event(
                &mut live,
                parser::OscEvent::StateLatencySmoothed { value: 40.0 },
            );
            live.resampling.copy_to(&mut trace);
            assert_eq!(trace.series["latency"].len(), 1);
            assert_eq!(trace.series["latency"][0].1, 40.0);
            assert!(trace.series["ppm"].is_empty());
        }
        crate::host::diagnostics::select_resample(&state, false);
        crate::host::diagnostics::select_resample(&state, true);
        state.read().resampling.copy_to(&mut trace);
        assert!(trace.series.values().all(|samples| samples.is_empty()));
    }

    #[test]
    fn reconnect_keeps_diagnostic_selection_without_a_ui_frame() {
        let mut live = Live::new(crate::model::app_state::AppState::new(Vec::new()));
        live.diagnostics
            .select(&std::collections::BTreeSet::from(["x".into()]));
        apply_event(
            &mut live,
            parser::OscEvent::StateDiagValues {
                value: "{\"x\":1}".into(),
            },
        );
        let mut trace = crate::host::diagnostics::Trace::default();
        live.diagnostics.copy_to(&mut trace);
        reset_connection_model(&mut live);
        apply_event(
            &mut live,
            parser::OscEvent::StateDiagValues {
                value: "{\"x\":2}".into(),
            },
        );
        live.diagnostics.copy_to(&mut trace);
        assert_eq!(trace.series["x"].len(), 1);
        assert_eq!(trace.series["x"][0].1, 2.0);
    }
    #[test]
    fn file_request_outcomes_survive_repeated_resets_until_consumed() {
        use crate::host::{commands::app, services::backend_files};
        use dispatch::BackendFileFailure;
        for complete in 0..3 {
            let state = crate::host::commands::tests::state();
            backend_files::begin(&state, "script", "file");
            if complete == 1 {
                let due = state.read().backend_file_pending.as_ref().unwrap().due;
                backend_files::tick(&state, due);
            } else if complete == 2 {
                apply_event(
                    &mut state.inner.lock().unwrap(),
                    parser::OscEvent::StateBackendFileContent {
                        backend: "script".into(),
                        key: "file".into(),
                        name: "file.lua".into(),
                        content: "saved".into(),
                        request_id: None,
                    },
                );
            }
            reset_connection_model(&mut state.inner.lock().unwrap());
            reset_connection_model(&mut state.inner.lock().unwrap());
            let failure = app::take_backend_file_error(&state, "script", "file").unwrap();
            assert!(matches!(
                (complete, failure),
                (1, BackendFileFailure::TimedOut) | (0 | 2, BackendFileFailure::ConnectionChanged)
            ));
            assert!(app::take_backend_file_error(&state, "script", "file").is_none());
        }
    }
}

/// What the two sending sockets must get past the operating system. macOS and
/// the BSDs refuse a UDP send larger than the socket's send buffer, which
/// starts at 9,216 bytes there, so these only pass on them when the socket has
/// had it raised.
#[cfg(test)]
mod send_size_tests {
    use super::*;

    /// The largest file the renderer accepts in a `backend/file/put` (its
    /// `BACKEND_FILE_MAX_BYTES`).
    const LARGEST_BACKEND_FILE: usize = 60_000;
    /// `net.inet.udp.maxdgram` as macOS ships it.
    const MACOS_DEFAULT_SEND_BUFFER: usize = 9_216;

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
        assert!(send_buffer(&socket) < SEND_BUF);
        ensure_send_buffer(&socket);
        assert!(send_buffer(&socket) >= SEND_BUF);
    }

    /// Asking for the minimum on a socket already past it would shrink it,
    /// which is what a plain `set` does to the Linux default.
    #[test]
    fn a_large_send_buffer_is_left_alone() {
        let socket = socket_with_send_buffer(2 * SEND_BUF);
        let before = send_buffer(&socket);
        assert!(before > SEND_BUF);
        ensure_send_buffer(&socket);
        assert_eq!(send_buffer(&socket), before);
    }

    #[test]
    fn a_maximum_size_backend_file_put_leaves_the_listener_socket() {
        let renderer = UdpSocket::bind("127.0.0.1:0").unwrap();
        renderer
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let live = Arc::new(Mutex::new(Live::new(
            crate::model::app_state::AppState::new(Vec::new()),
        )));
        let (_, tx, mut worker) = spawn_listener(
            live,
            Arc::new(|| {}),
            OscStats::new(),
            ListenerConfig {
                listen_port: 0,
                register: Some(renderer.local_addr().unwrap()),
                metering: false,
                playout_sync: true,
            },
        )
        .unwrap();
        let content = "x".repeat(LARGEST_BACKEND_FILE);
        tx.send(Control::Send {
            address: crate::osc_contract::CONTROL_BACKEND_FILE_PUT.into(),
            args: vec![
                OscType::String("script".into()),
                OscType::String("file".into()),
                OscType::String("big.lua".into()),
                OscType::String(content.clone()),
                OscType::String("request".into()),
            ],
        })
        .unwrap();
        // Shutting down sends what is still queued.
        worker.shutdown();

        let mut buf = vec![0u8; RECV_BUF];
        let mut sent = None;
        // The registration messages come first.
        while let Ok(len) = renderer.recv(&mut buf) {
            if let Ok((_, OscPacket::Message(message))) = decoder::decode_udp(&buf[..len])
                && message.addr == crate::osc_contract::CONTROL_BACKEND_FILE_PUT
            {
                sent = Some(message);
                break;
            }
        }
        let sent = sent.expect("the put reaches the renderer");
        assert_eq!(sent.args.get(3), Some(&OscType::String(content)));
    }

    #[test]
    fn a_synthetic_bundle_over_the_default_send_buffer_leaves_the_feed_socket() {
        // Every tick's bundle is then several times that default, and still
        // one datagram.
        const OBJECTS: u32 = 256;
        let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
        listener
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut worker = spawn_synthetic(OBJECTS, 50.0, port, None).unwrap();

        let mut buf = vec![0u8; RECV_BUF];
        let received = listener.recv(&mut buf);
        worker.shutdown();

        let len = received.expect("a bundle reaches the listener");
        let Ok((_, OscPacket::Bundle(bundle))) = decoder::decode_udp(&buf[..len]) else {
            panic!("the feed sends bundles");
        };
        let positions = bundle
            .content
            .iter()
            .filter(|packet| matches!(packet, OscPacket::Message(m) if m.addr.ends_with("/xyz")))
            .count();
        assert_eq!(positions, OBJECTS as usize);
        assert!(len > 2 * MACOS_DEFAULT_SEND_BUFFER, "only {len} bytes");
    }
}

/// The stream transport from Studio's side (#680, step 2), against a fake
/// engine: a UDP socket and, when the test wants one, a TCP listener on the
/// same port number.
#[cfg(test)]
mod stream_link_tests {
    use super::*;
    use crate::osc_contract::stream::{MAX_PACKET, frame, read_frame};
    use std::net::{TcpListener, TcpStream};

    struct FakeEngine {
        udp: UdpSocket,
        tcp: Option<TcpListener>,
    }

    impl FakeEngine {
        /// A UDP socket and, with `stream`, a TCP listener on its port number
        /// (another port is taken when that number is busy over TCP).
        fn new(stream: bool) -> Self {
            for _ in 0..16 {
                let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
                udp.set_read_timeout(Some(Duration::from_millis(100)))
                    .unwrap();
                if !stream {
                    return Self { udp, tcp: None };
                }
                let port = udp.local_addr().unwrap().port();
                if let Ok(tcp) = TcpListener::bind(("127.0.0.1", port)) {
                    return Self {
                        udp,
                        tcp: Some(tcp),
                    };
                }
            }
            panic!("no port free over both UDP and TCP");
        }

        fn addr(&self) -> SocketAddr {
            self.udp.local_addr().unwrap()
        }

        fn accept(&self, within: Duration) -> Option<TcpStream> {
            let tcp = self.tcp.as_ref()?;
            tcp.set_nonblocking(true).unwrap();
            let deadline = Instant::now() + within;
            while Instant::now() < deadline {
                if let Ok((stream, _)) = tcp.accept() {
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    return Some(stream);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            None
        }

        /// The address of the next datagram that arrives within `within`.
        fn datagram(&self, within: Duration) -> Option<String> {
            let deadline = Instant::now() + within;
            let mut buf = [0u8; 4096];
            while Instant::now() < deadline {
                if let Ok((n, _)) = self.udp.recv_from(&mut buf) {
                    if let Ok((_, OscPacket::Message(m))) = decoder::decode_udp(&buf[..n]) {
                        return Some(m.addr);
                    }
                }
            }
            None
        }
    }

    fn read_message(stream: &mut TcpStream) -> Option<OscMessage> {
        let packet = read_frame(stream, MAX_PACKET).ok()??;
        match decoder::decode_udp(&packet).ok()?.1 {
            OscPacket::Message(m) => Some(m),
            OscPacket::Bundle(_) => None,
        }
    }

    fn send_message(stream: &mut TcpStream, addr: &str, args: Vec<OscType>) {
        let bytes = encoder::encode(&OscPacket::Message(OscMessage {
            addr: addr.into(),
            args,
        }))
        .unwrap();
        stream.write_all(&frame(&bytes).unwrap()).unwrap();
    }

    fn start(engine: &FakeEngine) -> (Arc<OscStats>, ControlTx, Worker) {
        let (stats, tx, worker, _) = start_with_model(engine);
        (stats, tx, worker)
    }

    fn start_with_model(engine: &FakeEngine) -> (Arc<OscStats>, ControlTx, Worker, SharedLive) {
        let stats = OscStats::new();
        let live: SharedLive = Arc::new(Mutex::new(Live::new(
            crate::model::app_state::AppState::new(Vec::new()),
        )));
        let (_, tx, worker) = spawn_listener(
            live.clone(),
            Arc::new(|| {}),
            stats.clone(),
            ListenerConfig {
                listen_port: 0,
                register: Some(engine.addr()),
                metering: false,
                playout_sync: false,
            },
        )
        .unwrap();
        (stats, tx, worker, live)
    }

    fn wait_for(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "{what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    use std::io::Write;

    /// An engine on this machine that serves the stream gets the
    /// registration, the heartbeat and the controls over TCP, and nothing
    /// over UDP; its heartbeat ack marks the client connected at once.
    #[test]
    fn a_local_engine_with_a_stream_is_reached_over_tcp() {
        let engine = FakeEngine::new(true);
        let (stats, tx, mut worker) = start(&engine);
        let mut stream = engine
            .accept(Duration::from_secs(5))
            .expect("Studio connects over TCP");
        let first: Vec<_> = (0..3)
            .map(|_| read_message(&mut stream).expect("a framed message").addr)
            .collect();
        assert_eq!(
            first,
            [
                crate::osc_contract::REGISTER,
                crate::osc_contract::CONTROL_METERING,
                crate::osc_contract::HEARTBEAT,
            ]
        );
        send_message(
            &mut stream,
            crate::osc_contract::HEARTBEAT_ACK,
            vec![OscType::Int(7), OscType::Int(0)],
        );
        wait_for("the ack over TCP registers the client", || {
            stats.registered.load(Ordering::Relaxed)
        });

        tx.send(Control::Send {
            address: "/test/over-the-stream".into(),
            args: vec![],
        })
        .unwrap();
        let mut seen = false;
        for _ in 0..16 {
            match read_message(&mut stream) {
                Some(m) if m.addr == "/test/over-the-stream" => {
                    seen = true;
                    break;
                }
                Some(_) => {}
                None => break,
            }
        }
        assert!(seen, "controls go over the stream");
        assert_eq!(
            engine.datagram(Duration::from_millis(300)),
            None,
            "nothing by UDP"
        );
        worker.shutdown();
    }

    /// No stream at the engine's port: datagrams, as before revision 2.
    #[test]
    fn an_engine_without_a_stream_is_reached_by_datagrams() {
        let engine = FakeEngine::new(false);
        let (_stats, _tx, mut worker) = start(&engine);
        assert_eq!(
            engine.datagram(Duration::from_secs(2)).as_deref(),
            Some(crate::osc_contract::REGISTER)
        );
        worker.shutdown();
    }

    /// The engine closes the stream (a restart, a handoff): the client is no
    /// longer connected, and it connects again.
    #[test]
    fn a_closed_stream_is_opened_again() {
        let engine = FakeEngine::new(true);
        let (stats, _tx, mut worker) = start(&engine);
        let mut stream = engine.accept(Duration::from_secs(5)).unwrap();
        let _ = read_message(&mut stream);
        send_message(
            &mut stream,
            crate::osc_contract::HEARTBEAT_ACK,
            vec![OscType::Int(7), OscType::Int(0)],
        );
        wait_for("registered", || stats.registered.load(Ordering::Relaxed));
        drop(stream);
        wait_for("the close is noticed", || {
            !stats.registered.load(Ordering::Relaxed)
        });
        let mut again = engine
            .accept(Duration::from_secs(5))
            .expect("Studio connects again");
        assert_eq!(
            read_message(&mut again).map(|m| m.addr).as_deref(),
            Some(crate::osc_contract::REGISTER)
        );
        worker.shutdown();
    }

    /// Something that is not an engine owns the port number over TCP: it
    /// accepts and never speaks OSC. Studio gives up on it and registers by
    /// datagrams.
    #[test]
    fn a_silent_stream_is_left_for_datagrams() {
        let engine = FakeEngine::new(true);
        let (_stats, _tx, mut worker) = start(&engine);
        let _held = engine.accept(Duration::from_secs(5)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut registered_by_udp = false;
        while !registered_by_udp && Instant::now() < deadline {
            registered_by_udp = engine.datagram(Duration::from_millis(200)).as_deref()
                == Some(crate::osc_contract::REGISTER);
        }
        assert!(registered_by_udp, "datagrams once the stream proved silent");
        worker.shutdown();
    }

    /// A session that was fully up over TCP (snapshot complete) loses its
    /// stream and carries on over datagrams, answered there: nothing about
    /// that session asks to register again, yet the stream is tried again
    /// and taken back.
    #[test]
    fn an_initialized_session_goes_back_to_the_stream() {
        let engine = FakeEngine::new(true);
        let (stats, _tx, mut worker, live) = start_with_model(&engine);
        let mut stream = engine.accept(Duration::from_secs(5)).unwrap();
        let _ = read_message(&mut stream);
        send_message(
            &mut stream,
            crate::osc_contract::HEARTBEAT_ACK,
            vec![OscType::Int(7), OscType::Int(0)],
        );
        send_message(
            &mut stream,
            crate::osc_contract::STATE_SNAPSHOT_COMPLETE,
            vec![],
        );
        wait_for("registered with the snapshot", || {
            stats.registered.load(Ordering::Relaxed) && live.lock().unwrap().app.osc_snapshot_ready
        });
        drop(stream);

        // The engine answers by datagrams from now on: every heartbeat or
        // registration gets an ack and the snapshot's end, so the datagram
        // session is healthy.
        let udp = engine.udp.try_clone().unwrap();
        let answering = Arc::new(AtomicBool::new(true));
        let answer = {
            let answering = answering.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let reply = |addr: &str, args: Vec<OscType>| {
                    encoder::encode(&OscPacket::Message(OscMessage {
                        addr: addr.into(),
                        args,
                    }))
                    .unwrap()
                };
                while answering.load(Ordering::Relaxed) {
                    if let Ok((_, from)) = udp.recv_from(&mut buf) {
                        let ack = reply(
                            crate::osc_contract::HEARTBEAT_ACK,
                            vec![OscType::Int(7), OscType::Int(0)],
                        );
                        let done = reply(crate::osc_contract::STATE_SNAPSHOT_COMPLETE, vec![]);
                        let _ = udp.send_to(&ack, from);
                        let _ = udp.send_to(&done, from);
                    }
                }
            })
        };
        let again = engine.accept(Duration::from_secs(8));
        answering.store(false, Ordering::Relaxed);
        answer.join().unwrap();
        assert!(again.is_some(), "the stream is taken back");
        worker.shutdown();
    }
}
