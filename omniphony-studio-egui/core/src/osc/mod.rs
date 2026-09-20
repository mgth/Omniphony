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
#[allow(dead_code)]
pub mod parser;

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rosc::{OscBundle, OscMessage, OscPacket, OscTime, OscType, decoder, encoder};

use crate::host::runtime::{StopToken, Worker};
use dispatch::{Change, Live, apply_event};
use parser::{CoordinateFormat, HeartbeatResponse, is_heartbeat_address, parse_osc_message};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
/// Re-register when no heartbeat ack came back for this long. The host's
/// `HEARTBEAT_ACK_TIMEOUT`, so both clients give up after the same delay.
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(10);
const REPAINT_COALESCE: Duration = Duration::from_micros(2500);
/// How long a receive waits when no change is waiting to be shown, so the
/// heartbeat, the snapshot requests and the control channel keep running.
const READ_TIMEOUT: Duration = Duration::from_millis(100);
/// While `osc_snapshot_ready` is false, re-register this often (host value).
const SNAPSHOT_REQUEST_INTERVAL: Duration = Duration::from_secs(1);
const RECV_BUF: usize = 65_536;

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

/// Bind the socket and start the listener thread. Returns the bound port so a
/// `0` request can be reported (and fed by the synthetic generator).
pub fn spawn_listener(
    live: SharedLive,
    waker: Waker,
    stats: Arc<OscStats>,
    cfg: ListenerConfig,
) -> std::io::Result<(u16, ControlTx, Worker)> {
    let socket = UdpSocket::bind(("0.0.0.0", cfg.listen_port))?;
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
    control: Receiver<Control>,
    stop: StopToken,
) {
    let mut buf = vec![0u8; RECV_BUF];
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

    if let Some(addr) = register {
        send_register(&socket, addr, port, metering);
    }

    loop {
        match socket.recv_from(&mut buf) {
            // Replies use a separate ephemeral socket; reject other IPs
            // without starving control draining or shutdown below.
            Ok((n, from)) if accepts_sender(register, from) => {
                stats.packets.fetch_add(1, Ordering::Relaxed);
                stats
                    .last_packet_ms
                    .store(stats.start.elapsed().as_millis() as u64, Ordering::Relaxed);
                match decoder::decode_udp(&buf[..n]) {
                    Ok((_, packet)) => {
                        let mut outcome = PacketOutcome::default();
                        {
                            let mut model = live.lock().unwrap();
                            handle_packet(&packet, &mut model, &stats, &mut outcome);
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
                            send_register(&socket, addr, port, metering);
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
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) => {
                log::error!("[osc] recv failed: {e}");
                std::thread::sleep(Duration::from_millis(50));
            }
        }

        if repaint_pending && last_repaint.elapsed() >= REPAINT_COALESCE {
            repaint_pending = false;
            last_repaint = Instant::now();
            waker();
        }
        let wanted = if repaint_pending {
            REPAINT_COALESCE
        } else {
            READ_TIMEOUT
        };
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
                    {
                        let mut model = live.lock().unwrap();
                        apply_connection_reset(&mut model, request);
                    }
                    repaint_pending = true;
                    send_register(&socket, target, port, metering);
                }
                Control::SetMetering { enabled } => {
                    metering = enabled;
                    if let Some(addr) = register {
                        send_ints(
                            &socket,
                            addr,
                            "/omniphony/control/metering",
                            &[i32::from(enabled)],
                        );
                    }
                }
                Control::Send { address, args } => {
                    if let Some(addr) = register {
                        send_args(&socket, addr, &address, args);
                    }
                }
                Control::SubscribeGainTable {
                    have_version,
                    speaker_index,
                } => {
                    if let Some(addr) = register {
                        send_ints(
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
                            &socket,
                            addr,
                            "/omniphony/control/debug/speaker_gaintable/unsubscribe",
                            &[],
                        );
                    }
                }
            }
        }

        if stop.cancelled() {
            stats.registered.store(false, Ordering::Relaxed);
            return;
        }
        if let Some(addr) = register {
            let now = Instant::now();
            // Until the renderer's state bundle has fully arrived, keep asking
            // for it (the host's `SNAPSHOT_REQUEST_INTERVAL`).
            if now.duration_since(last_snapshot_request) >= SNAPSHOT_REQUEST_INTERVAL
                && !live.lock().unwrap().app.osc_snapshot_ready
            {
                last_snapshot_request = now;
                log::debug!("[osc] snapshot not ready yet, re-requesting the live state bundle");
                send_register(&socket, addr, port, metering);
            }
            if now.duration_since(last_heartbeat) >= HEARTBEAT_INTERVAL {
                last_heartbeat = now;
                send_int(&socket, addr, "/omniphony/heartbeat", i32::from(port));
                if stats.registered.load(Ordering::Relaxed)
                    && now.duration_since(last_ack) >= HEARTBEAT_TIMEOUT
                {
                    log::warn!("[osc] heartbeat timeout, re-registering");
                    stats.registered.store(false, Ordering::Relaxed);
                    repaint_pending = true;
                    send_register(&socket, addr, port, metering);
                }
            }
            // Recover lost gain-table chunks (remote renderer case).
            for (version, missing) in apply::gaintable_check_nack(now) {
                log::debug!(
                    "[osc] gaintable {version}: re-requesting {} chunks",
                    missing.len()
                );
                apply::send_gaintable_nack(
                    &socket,
                    &addr.ip().to_string(),
                    addr.port(),
                    version,
                    &missing,
                );
            }
        }
    }
}

fn accepts_sender(target: Option<SocketAddr>, sender: SocketAddr) -> bool {
    target.is_none_or(|target| target.ip() == sender.ip())
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
}

impl Default for Change {
    fn default() -> Self {
        Change::None
    }
}

fn handle_packet(packet: &OscPacket, live: &mut Live, stats: &OscStats, out: &mut PacketOutcome) {
    match packet {
        OscPacket::Bundle(OscBundle { content, .. }) => {
            for p in content {
                handle_packet(p, live, stats, out);
            }
        }
        OscPacket::Message(m) => handle_message(m, live, stats, out),
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
            if let Some(epoch) = m.args.iter().find_map(|arg| match arg {
                OscType::Int(epoch) => Some(*epoch),
                _ => None,
            }) {
                let changed = live
                    .app
                    .producer_epoch
                    .is_some_and(|previous| previous != epoch);
                if changed {
                    reset_connection_model(live);
                    stats.registered.store(false, Ordering::Relaxed);
                    out.reregister = true;
                    out.change = out.change.max(Change::Snapshot);
                }
                live.app.producer_epoch = Some(epoch);
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
fn send_register(socket: &UdpSocket, to: SocketAddr, listen_port: u16, metering: bool) {
    send_int(socket, to, "/omniphony/register", i32::from(listen_port));
    send_int(
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

fn send_int(socket: &UdpSocket, to: SocketAddr, addr: &str, value: i32) {
    send_ints(socket, to, addr, &[value]);
}

fn send_ints(socket: &UdpSocket, to: SocketAddr, addr: &str, values: &[i32]) {
    send_args(
        socket,
        to,
        addr,
        values.iter().map(|v| OscType::Int(*v)).collect(),
    );
}

fn send_args(socket: &UdpSocket, to: SocketAddr, addr: &str, args: Vec<OscType>) {
    let msg = OscPacket::Message(OscMessage {
        addr: addr.to_owned(),
        args,
    });
    match encoder::encode(&msg) {
        Ok(bytes) => {
            if let Err(e) = socket.send_to(&bytes, to) {
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
