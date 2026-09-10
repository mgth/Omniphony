//! OSC transport: the listener thread and the synthetic feed.
//!
//! Parsing is the Tauri host's `osc_parser.rs` reused verbatim ([`parser`]);
//! the parsed events are applied to the live model by [`dispatch`]. The
//! listener also owns the client side of the renderer protocol: register,
//! heartbeat, re-register on an unknown-client reply, and the NACK timer of
//! the chunked gain-table transfer.
//!
//! Repaint policy: after each packet that changed the model the listener asks
//! egui for a repaint, coalesced so a burst of per-object messages never
//! requests more than one frame per ~2.5 ms. When nothing arrives, nothing
//! repaints.

pub mod apply;
pub mod dispatch;
#[allow(dead_code)]
pub mod parser;

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rosc::{OscBundle, OscMessage, OscPacket, OscTime, OscType, decoder, encoder};

use dispatch::{Change, Live, apply_event};
use parser::{CoordinateFormat, HeartbeatResponse, is_heartbeat_address, parse_osc_message};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
/// Re-register when no ack came back for this long (the host uses the same
/// three-heartbeat window).
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(16);
const REPAINT_COALESCE: Duration = Duration::from_micros(2500);
const RECV_BUF: usize = 65_536;

pub type SharedLive = Arc<Mutex<Live>>;

pub struct OscStats {
    pub packets: AtomicU64,
    pub messages: AtomicU64,
    pub applied: AtomicU64,
    pub ignored: AtomicU64,
    pub heartbeat_acks: AtomicU64,
    pub registered: AtomicBool,
    pub listen_port: AtomicU64,
    /// Milliseconds since `start` at the last received packet.
    pub last_packet_ms: AtomicU64,
    pub start: Instant,
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
            listen_port: AtomicU64::new(0),
            last_packet_ms: AtomicU64::new(0),
            start: Instant::now(),
        })
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
}

/// Bind the socket and start the listener thread. Returns the bound port so a
/// `0` request can be reported (and fed by the synthetic generator).
pub fn spawn_listener(
    live: SharedLive,
    ctx: egui::Context,
    stats: Arc<OscStats>,
    cfg: ListenerConfig,
) -> std::io::Result<u16> {
    let socket = UdpSocket::bind(("0.0.0.0", cfg.listen_port))?;
    let port = socket.local_addr()?.port();
    socket.set_read_timeout(Some(Duration::from_millis(100)))?;
    stats.listen_port.store(u64::from(port), Ordering::Relaxed);
    std::thread::Builder::new()
        .name("osc-listener".into())
        .spawn(move || listener_loop(socket, port, live, ctx, stats, cfg.register))?;
    Ok(port)
}

fn listener_loop(
    socket: UdpSocket,
    port: u16,
    live: SharedLive,
    ctx: egui::Context,
    stats: Arc<OscStats>,
    register: Option<SocketAddr>,
) {
    let mut buf = vec![0u8; RECV_BUF];
    let mut last_heartbeat = Instant::now();
    let mut last_ack = Instant::now();
    let mut last_repaint = Instant::now() - REPAINT_COALESCE;

    if let Some(addr) = register {
        send_int(&socket, addr, "/omniphony/register", i32::from(port));
        log::info!("[osc] register sent to {addr} (listen_port={port})");
    }

    loop {
        match socket.recv_from(&mut buf) {
            Ok((n, _from)) => {
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
                            log::warn!("[osc] renderer does not know this client; re-registering");
                            send_int(&socket, addr, "/omniphony/register", i32::from(port));
                        }
                        if outcome.ack {
                            last_ack = Instant::now();
                        }
                        if outcome.change != Change::None
                            && last_repaint.elapsed() >= REPAINT_COALESCE
                        {
                            last_repaint = Instant::now();
                            ctx.request_repaint();
                        }
                    }
                    Err(e) => log::debug!("[osc] undecodable packet ({n} bytes): {e:?}"),
                }
            }
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

        if let Some(addr) = register {
            let now = Instant::now();
            if now.duration_since(last_heartbeat) >= HEARTBEAT_INTERVAL {
                last_heartbeat = now;
                send_int(&socket, addr, "/omniphony/heartbeat", i32::from(port));
                if stats.registered.load(Ordering::Relaxed)
                    && now.duration_since(last_ack) >= HEARTBEAT_TIMEOUT
                {
                    log::warn!("[osc] heartbeat timeout, re-registering");
                    stats.registered.store(false, Ordering::Relaxed);
                    send_int(&socket, addr, "/omniphony/register", i32::from(port));
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

#[derive(Default)]
struct PacketOutcome {
    change: Change,
    ack: bool,
    reregister: bool,
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
    match is_heartbeat_address(&m.addr) {
        HeartbeatResponse::Ack => {
            stats.registered.store(true, Ordering::Relaxed);
            stats.heartbeat_acks.fetch_add(1, Ordering::Relaxed);
            out.ack = true;
            return;
        }
        HeartbeatResponse::Unknown => {
            stats.registered.store(false, Ordering::Relaxed);
            out.reregister = true;
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

fn send_int(socket: &UdpSocket, to: SocketAddr, addr: &str, value: i32) {
    let msg = OscPacket::Message(OscMessage {
        addr: addr.to_owned(),
        args: vec![OscType::Int(value)],
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
) -> std::io::Result<()> {
    let socket = UdpSocket::bind(("127.0.0.1", 0))?;
    socket.connect(("127.0.0.1", target_port))?;
    let period = Duration::from_secs_f32(1.0 / rate_hz.max(1.0));
    std::thread::Builder::new()
        .name("osc-synthetic".into())
        .spawn(move || {
            let start = Instant::now();
            let mut next = start;
            let mut generation: i64 = 0;
            let mut tick: u64 = 0;
            log::info!("[synthetic] {count} objects at {rate_hz} Hz to udp/{target_port}");
            loop {
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
                    std::thread::sleep(next - now);
                } else {
                    // Fell behind (e.g. suspended); resync instead of bursting.
                    next = now;
                }
            }
        })?;
    Ok(())
}
