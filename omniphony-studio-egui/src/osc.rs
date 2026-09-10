//! OSC transport: the listener thread (same addresses as the Tauri Studio's
//! `osc_parser.rs`, reduced to positions, meta and removals) and the
//! synthetic feed used for load measurements.
//!
//! Repaint policy: the listener asks egui for a repaint after each packet that
//! changed the scene, coalesced so a burst of per-object messages never
//! requests more than one frame per ~2.5 ms. When nothing arrives, nothing
//! repaints — that is what the idle-CPU gate measures.

use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rosc::{OscBundle, OscMessage, OscPacket, OscTime, OscType, decoder, encoder};

use crate::scene::{Scene, SharedScene};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const REPAINT_COALESCE: Duration = Duration::from_micros(2500);
const RECV_BUF: usize = 65_536;

pub struct OscStats {
    pub packets: AtomicU64,
    pub messages: AtomicU64,
    pub position_updates: AtomicU64,
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
            position_updates: AtomicU64::new(0),
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
    scene: SharedScene,
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
        .spawn(move || listener_loop(socket, port, scene, ctx, stats, cfg.register))?;
    Ok(port)
}

fn listener_loop(
    socket: UdpSocket,
    port: u16,
    scene: SharedScene,
    ctx: egui::Context,
    stats: Arc<OscStats>,
    register: Option<SocketAddr>,
) {
    let mut buf = vec![0u8; RECV_BUF];
    let mut last_heartbeat = Instant::now();
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
                        let changed = {
                            let mut s = scene.lock().unwrap();
                            handle_packet(&packet, &mut s, &stats)
                        };
                        if changed && last_repaint.elapsed() >= REPAINT_COALESCE {
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

        if let Some(addr) = register
            && last_heartbeat.elapsed() >= HEARTBEAT_INTERVAL
        {
            last_heartbeat = Instant::now();
            send_int(&socket, addr, "/omniphony/heartbeat", i32::from(port));
        }
    }
}

/// Returns true when the scene changed.
fn handle_packet(packet: &OscPacket, scene: &mut Scene, stats: &OscStats) -> bool {
    match packet {
        OscPacket::Bundle(b) => {
            let mut changed = false;
            for p in &b.content {
                changed |= handle_packet(p, scene, stats);
            }
            changed
        }
        OscPacket::Message(m) => handle_message(m, scene, stats),
    }
}

fn handle_message(m: &OscMessage, scene: &mut Scene, stats: &OscStats) -> bool {
    stats.messages.fetch_add(1, Ordering::Relaxed);
    let parts: Vec<&str> = m.addr.split('/').filter(|s| !s.is_empty()).collect();
    let args = &m.args;

    match parts.as_slice() {
        // Renderer contract: `/omniphony/object/<id>/xyz x y z [speaker gain prio ramp gen name]`.
        ["omniphony", "object", id, "xyz"] => {
            let Some(id) = parse_id(id) else {
                return ignored(stats);
            };
            let Some(pos) = xyz(args, 0) else {
                return ignored(stats);
            };
            scene.upsert_position(id, pos, trailing_string(args));
            stats.position_updates.fetch_add(1, Ordering::Relaxed);
            true
        }
        ["omniphony", "object", id, kind] if is_polar(kind) => {
            let Some(id) = parse_id(id) else {
                return ignored(stats);
            };
            let Some(pos) = aed(args, 0) else {
                return ignored(stats);
            };
            scene.upsert_position(id, pos, trailing_string(args));
            stats.position_updates.fetch_add(1, Ordering::Relaxed);
            true
        }
        // `/omniphony/object/<id>/meta [Int fixed, String label, Long generation, String kind]`.
        ["omniphony", "object", id, "meta"] => {
            let Some(id) = parse_id(id) else {
                return ignored(stats);
            };
            let fixed = args
                .first()
                .and_then(num)
                .map(|v| v != 0.0)
                .unwrap_or(false);
            scene.upsert_meta(id, fixed, args.get(1).and_then(string));
            true
        }
        // Prototype formats still accepted by the Studio.
        ["source", id, "position"] | ["object", id, "position"] | ["channel", id, "position"] => {
            let Some(id) = parse_id(id) else {
                return ignored(stats);
            };
            let Some(pos) = xyz(args, 0) else {
                return ignored(stats);
            };
            scene.upsert_position(id, pos, None);
            stats.position_updates.fetch_add(1, Ordering::Relaxed);
            true
        }
        ["source", "position"] => {
            let Some(id) = args.first().and_then(num).map(|v| v as u32) else {
                return ignored(stats);
            };
            let Some(pos) = xyz(args, 1) else {
                return ignored(stats);
            };
            scene.upsert_position(id, pos, None);
            stats.position_updates.fetch_add(1, Ordering::Relaxed);
            true
        }
        ["source", id, kind] if is_polar(kind) => {
            let Some(id) = parse_id(id) else {
                return ignored(stats);
            };
            let Some(pos) = aed(args, 0) else {
                return ignored(stats);
            };
            scene.upsert_position(id, pos, None);
            stats.position_updates.fetch_add(1, Ordering::Relaxed);
            true
        }
        ["source", "remove"] => {
            if let Some(id) = args.first().and_then(num) {
                scene.remove(id as u32);
            }
            true
        }
        ["source", id, "remove"] => {
            if let Some(id) = parse_id(id) {
                scene.remove(id);
            }
            true
        }
        ["omniphony", "heartbeat", "ack"] => {
            stats.registered.store(true, Ordering::Relaxed);
            stats.heartbeat_acks.fetch_add(1, Ordering::Relaxed);
            false
        }
        _ => ignored(stats),
    }
}

fn ignored(stats: &OscStats) -> bool {
    stats.ignored.fetch_add(1, Ordering::Relaxed);
    false
}

fn is_polar(kind: &str) -> bool {
    matches!(kind, "aed" | "spherical" | "polar")
}

fn parse_id(s: &str) -> Option<u32> {
    s.parse().ok()
}

fn num(a: &OscType) -> Option<f32> {
    match a {
        OscType::Float(v) => Some(*v),
        OscType::Double(v) => Some(*v as f32),
        OscType::Int(v) => Some(*v as f32),
        OscType::Long(v) => Some(*v as f32),
        OscType::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

fn string(a: &OscType) -> Option<&str> {
    match a {
        OscType::String(s) if !s.trim().is_empty() => Some(s.as_str()),
        _ => None,
    }
}

/// The renderer appends the object name as the last string argument; its
/// index moved twice over the contract's history, so scan from the end.
fn trailing_string(args: &[OscType]) -> Option<&str> {
    args.iter().rev().find_map(string)
}

fn xyz(args: &[OscType], at: usize) -> Option<[f32; 3]> {
    Some([
        num(args.get(at)?)?,
        num(args.get(at + 1)?)?,
        num(args.get(at + 2)?)?,
    ])
}

fn aed(args: &[OscType], at: usize) -> Option<[f32; 3]> {
    let az = num(args.get(at)?)?;
    let el = num(args.get(at + 1)?)?;
    let dist = num(args.get(at + 2)?)?;
    let (x, y, z) = omniphony_geometry::f32::from_spherical(az, el, dist);
    Some([x, y, z])
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
/// as one OSC bundle per tick using the renderer's `xyz` payload layout.
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
            log::info!("[synthetic] {count} objects at {rate_hz} Hz to udp/{target_port}");
            loop {
                if let Some(limit) = stop_after
                    && start.elapsed() >= limit
                {
                    log::info!("[synthetic] stopped after {:.1} s", limit.as_secs_f32());
                    return;
                }
                let t = start.elapsed().as_secs_f32();
                let content: Vec<OscPacket> = (0..count)
                    .map(|i| {
                        let phase = i as f32 * std::f32::consts::TAU / count.max(1) as f32;
                        let x = 0.9 * (0.7 * t + phase).sin();
                        let y = 0.9 * (0.5 * t + 1.3 * phase).cos();
                        let z = 0.45 + 0.45 * (0.3 * t + 0.7 * phase).sin();
                        let name = SYNTH_NAMES[i as usize % SYNTH_NAMES.len()];
                        OscPacket::Message(OscMessage {
                            addr: format!("/omniphony/object/{}/xyz", i + 1),
                            args: vec![
                                OscType::Float(x),
                                OscType::Float(y),
                                OscType::Float(z),
                                OscType::Int(-1),
                                OscType::Int(0),
                                OscType::Int(0),
                                OscType::Int(0),
                                OscType::Long(generation),
                                OscType::String(format!("{name} {}", i + 1)),
                            ],
                        })
                    })
                    .collect();
                generation += 1;
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
