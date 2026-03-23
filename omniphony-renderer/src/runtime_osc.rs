use anyhow::Result;
use audio_output::AudioControl;
use rosc::{OscBundle, OscMessage, OscPacket, OscTime, OscType};
use runtime_control::command::{RuntimeCommand, parse_process_command};
use runtime_control::context::RuntimeControlContext;
use runtime_control::osc::{BroadcastValue, SpeakerPatch, apply_simple_osc_control, apply_speaker_osc_control};
use std::collections::HashMap;
use std::net::{SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use renderer::live_params::RendererControl;

mod export;
mod recompute;
mod transport;

use self::export::{build_live_state_bundle, export_current_layout, save_live_config};
use self::recompute::trigger_layout_recompute;
use self::transport::{
    broadcast_fff, broadcast_float, broadcast_int, broadcast_speaker_config, broadcast_string,
    flush_pending_logs, resolve_register_addr, send_buffered_logs_to_client, send_metering_state,
    send_raw_filtered,
};
pub(crate) use self::transport::build_speaker_config_bundle;

/// Timeout after which a registered client (one that must heartbeat) is considered dead.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(15);

/// Generic description of a single spatial audio object for OSC broadcast.
/// Built by the caller from whatever source format it uses.
pub struct ObjectMeta {
    pub name: String,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub coord_mode: String,
    pub direct_speaker_index: Option<u32>,
    /// Gain in dB (integer, -128 = silent).
    pub gain: i32,
    pub priority: f32,
    pub divergence: f32,
}

/// Epsilon for position/float comparison in delta OSC sending.
const OBJECT_EPSILON: f32 = 1e-6;

#[derive(Clone, Copy)]
struct OscClientState {
    last_seen: Option<Instant>,
    metering_enabled: bool,
}

type OscClients = HashMap<SocketAddr, OscClientState>;

/// Snapshot of an object's comparable fields for delta detection.
#[derive(Clone)]
struct ObjectSnapshot {
    name: String,
    x: f32,
    y: f32,
    z: f32,
    coord_mode: String,
    direct_speaker_index: Option<u32>,
    gain: i32,
    priority: f32,
    divergence: f32,
}

impl ObjectSnapshot {
    fn from_meta(o: &ObjectMeta) -> Self {
        Self {
            name: o.name.clone(),
            x: o.x,
            y: o.y,
            z: o.z,
            coord_mode: o.coord_mode.clone(),
            direct_speaker_index: o.direct_speaker_index,
            gain: o.gain,
            priority: o.priority,
            divergence: o.divergence,
        }
    }

    fn matches(&self, o: &ObjectMeta) -> bool {
        self.name == o.name
            && self.gain == o.gain
            && self.coord_mode == o.coord_mode
            && self.direct_speaker_index == o.direct_speaker_index
            && (self.x - o.x).abs() < OBJECT_EPSILON
            && (self.y - o.y).abs() < OBJECT_EPSILON
            && (self.z - o.z).abs() < OBJECT_EPSILON
            && (self.priority - o.priority).abs() < OBJECT_EPSILON
            && (self.divergence - o.divergence).abs() < OBJECT_EPSILON
    }
}

pub struct OscSender {
    socket: Arc<UdpSocket>,
    /// Maps client address → last heartbeat time.
    /// `None`       = permanent client (the fixed `--osc-host` target), never times out.
    /// `Some(t)`    = registered via `/omniphony/register`, must send `/omniphony/heartbeat`
    ///                every <CLIENT_TIMEOUT/2 seconds or it will be dropped.
    clients: Arc<Mutex<OscClients>>,
    /// Shared live parameters + pending VBAP swap.
    /// Set by `attach_renderer_control` before `start_listener` is called.
    control: Option<Arc<RendererControl>>,
    /// Shared audio runtime control for output-device and adaptive-resampling state.
    audio_control: Option<Arc<AudioControl>>,
    /// Previous frame's object snapshots for delta detection.
    prev_objects: Option<Vec<ObjectSnapshot>>,
    /// Force next send_object_frame call to emit all objects.
    force_full_next: Arc<AtomicBool>,
    /// Monotonic identifier for the current logical content generation.
    content_generation: u64,
}

impl OscSender {
    pub fn new(default_target: SocketAddrV4) -> Result<Self> {
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        let mut clients = HashMap::new();
        // The fixed CLI target is permanent — it never times out.
        clients.insert(
            SocketAddr::V4(default_target),
            OscClientState {
                last_seen: None,
                metering_enabled: false,
            },
        );
        Ok(Self {
            socket: Arc::new(socket),
            clients: Arc::new(Mutex::new(clients)),
            control: None,
            audio_control: None,
            prev_objects: None,
            force_full_next: Arc::new(AtomicBool::new(true)),
            content_generation: 0,
        })
    }

    /// Attach the renderer control object so the OSC listener can read/write live params
    /// and trigger VBAP recomputes.  Must be called **before** `start_listener`.
    pub fn attach_renderer_control(&mut self, control: Arc<RendererControl>) {
        self.control = Some(control);
    }

    pub fn attach_audio_control(&mut self, control: Arc<AudioControl>) {
        self.audio_control = Some(control);
    }

    /// Start the OSC registration listener on `rx_port`.
    ///
    /// Clients send `/omniphony/register [i listen_port?]` from their listening socket.
    /// If the optional `Int` arg is present it overrides the source port (useful when
    /// the client's send and receive ports differ).
    /// On registration the client immediately receives `config_bundle_bytes`
    /// (pre-encoded speaker layout bundle) and the current live-parameter state.
    pub fn start_listener(&self, rx_port: u16, config_bundle_bytes: Vec<u8>) -> Result<()> {
        let socket = Arc::clone(&self.socket);
        let clients = Arc::clone(&self.clients);
        let config = Arc::new(config_bundle_bytes);
        let control = self.control.clone();
        let audio_control = self.audio_control.clone();
        let force_full_next = Arc::clone(&self.force_full_next);

        std::thread::Builder::new()
            .name("osc-listener".into())
            .spawn(move || {
                let rx_socket = match UdpSocket::bind(format!("0.0.0.0:{}", rx_port)) {
                    Ok(s) => s,
                    Err(e) => {
                        log::error!("OSC listener: failed to bind port {}: {}", rx_port, e);
                        return;
                    }
                };
                let _ = rx_socket.set_read_timeout(Some(Duration::from_millis(200)));
                log::info!("OSC listener ready on port {}", rx_port);

                // Pending speaker patches staged by
                // /control/speaker/{idx}/{az|el|distance|spatialize}.
                // Applied atomically by /control/speakers/apply.
                let mut pending_speakers: HashMap<usize, SpeakerPatch> = HashMap::new();
                let mut last_log_seq = sys::live_log::records_since(0)
                    .last()
                    .map(|record| record.seq)
                    .unwrap_or(0);

                let mut buf = [0u8; 4096];
                loop {
                    flush_pending_logs(&socket, &clients, &mut last_log_seq);
                    match rx_socket.recv_from(&mut buf) {
                        Ok((len, src)) => {
                            match rosc::decoder::decode_udp(&buf[..len]) {
                                Ok((_, OscPacket::Message(msg)))
                                    if msg.addr == "/omniphony/register" =>
                                {
                                    let client = resolve_register_addr(src, &msg.args);
                                    let mut clients_guard = clients.lock().unwrap();
                                    let metering_enabled = clients_guard
                                        .get(&client)
                                        .map(|entry| entry.metering_enabled)
                                        .unwrap_or(false);
                                    let prev = clients_guard.insert(
                                        client,
                                        OscClientState {
                                            last_seen: Some(Instant::now()),
                                            metering_enabled,
                                        },
                                    );
                                    drop(clients_guard);
                                    if prev.is_none() {
                                        log::info!("OSC client registered: {}", client);
                                    }
                                    // A new/reconnected client needs a complete object snapshot.
                                    force_full_next.store(true, Ordering::Relaxed);
                                    // Send speaker config bundle.
                                    if let Err(e) = socket.send_to(&config, client) {
                                        log::warn!("Failed to send config to {}: {}", client, e);
                                    }
                                    // Send live-state bundle (gain, spread, etc.).
                                    if let Some(ref ctrl) = control {
                                        let state_bytes =
                                            build_live_state_bundle(ctrl, audio_control.as_ref());
                                        if let Err(e) = socket.send_to(&state_bytes, client) {
                                            log::warn!(
                                                "Failed to send live state to {}: {}",
                                                client,
                                                e
                                            );
                                        }
                                    }
                                    send_buffered_logs_to_client(&socket, client, 0);
                                    send_metering_state(&socket, client, metering_enabled);
                                }
                                Ok((_, OscPacket::Message(msg)))
                                    if msg.addr == "/omniphony/heartbeat" =>
                                {
                                    let client = resolve_register_addr(src, &msg.args);
                                    // Update last_seen while holding the lock, then release
                                    // before doing I/O.
                                    let is_known = {
                                        let mut map = clients.lock().unwrap();
                                        if let Some(entry) = map.get_mut(&client) {
                                            // Don't overwrite a permanent entry.
                                            if entry.last_seen.is_some() {
                                                entry.last_seen = Some(Instant::now());
                                            }
                                            true
                                        } else {
                                            false
                                        }
                                    };
                                    let reply_addr = if is_known {
                                        log::trace!("OSC heartbeat/ack → {}", client);
                                        "/omniphony/heartbeat/ack"
                                    } else {
                                        "/omniphony/heartbeat/unknown"
                                    };
                                    let reply = OscMessage {
                                        addr: reply_addr.to_string(),
                                        args: vec![],
                                    };
                                    match rosc::encoder::encode(&OscPacket::Message(reply)) {
                                        Ok(bytes) => {
                                            if let Err(e) = socket.send_to(&bytes, client) {
                                                log::warn!(
                                                    "Failed to send heartbeat reply to {}: {}",
                                                    client,
                                                    e
                                                );
                                            }
                                        }
                                        Err(e) => {
                                            log::warn!("Failed to encode heartbeat reply: {}", e)
                                        }
                                    }
                                }

                                // ── Live-parameter control messages ─────────────────────────────────
                                Ok((_, OscPacket::Message(msg)))
                                    if msg.addr.starts_with("/omniphony/control/") =>
                                {
                                    if let Some(ref ctrl) = control {
                                        handle_control_message(
                                            &msg,
                                            src,
                                            ctrl,
                                            audio_control.as_ref(),
                                            &mut pending_speakers,
                                            &socket,
                                            &clients,
                                        );
                                    }
                                }

                                Ok(_) => {}
                                Err(e) => {
                                    log::debug!("OSC decode error from {}: {}", src, e)
                                }
                            }
                        }
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) => {}
                        Err(e) => log::warn!("OSC recv error: {}", e),
                    }
                }
            })?;

        Ok(())
    }

    /// Send bytes to every live client.
    ///
    /// Clients with a timed entry (`Some(t)`) are dropped if `t.elapsed() >= CLIENT_TIMEOUT`.
    /// Permanent clients (`None`) are never dropped.
    fn send_to_all(&self, bytes: &[u8]) {
        send_raw_filtered(&self.socket, &self.clients, bytes, |_| true);
    }

    fn send_to_metering_clients(&self, bytes: &[u8]) {
        send_raw_filtered(&self.socket, &self.clients, bytes, |client| {
            client.metering_enabled
        });
    }

    pub fn has_osc_clients(&self) -> bool {
        let clients = self.clients.lock().unwrap();
        let now = Instant::now();
        clients.values().any(|client| {
            client
                .last_seen
                .map(|t| now.duration_since(t) < CLIENT_TIMEOUT)
                .unwrap_or(true)
        })
    }

    pub fn has_metering_clients(&self) -> bool {
        let clients = self.clients.lock().unwrap();
        let now = Instant::now();
        clients.values().any(|client| {
            client.metering_enabled
                && client
                    .last_seen
                    .map(|t| now.duration_since(t) < CLIENT_TIMEOUT)
                    .unwrap_or(true)
        })
    }

    // -------------------------------------------------------------------------
    // Loudness metadata state broadcast
    // -------------------------------------------------------------------------

    /// Broadcast current source loudness metadata and applied correction gain.
    ///
    /// Should be called whenever `dialogue_level` is first received from the
    /// bitstream, and whenever `use_loudness` is toggled.
    ///
    /// - `/omniphony/state/loudness/source i <dBFS>` - source loudness metadata value from stream
    /// - `/omniphony/state/loudness/gain f <linear>` - correction gain as linear ratio (1.0 if disabled)
    pub fn send_loudness_state(&self) {
        let control = match self.control {
            Some(ref c) => c,
            None => return,
        };
        let live = control.live.read().unwrap();
        let socket = &self.socket;
        let clients = &self.clients;

        if let Some(dl) = live.dialogue_level {
            broadcast_int(
                socket,
                clients,
                "/omniphony/state/loudness/source",
                dl as i32,
            );
        }

        let gain_linear: f32 = match (live.use_loudness, live.dialogue_level) {
            (true, Some(dl)) => 10.0_f32.powf((-31 - dl as i32) as f32 / 20.0),
            _ => 1.0,
        };
        broadcast_float(
            socket,
            clients,
            "/omniphony/state/loudness/gain",
            gain_linear,
        );
    }

    // -------------------------------------------------------------------------
    // Metadata messages
    // -------------------------------------------------------------------------

    /// Send object placement metadata via OSC
    /// Format: /omniphony/object/{object_id} x y z
    pub fn send_object_position(&self, object_id: u32, x: f32, y: f32, z: f32) -> Result<()> {
        let msg = OscMessage {
            addr: format!("/omniphony/object/{}", object_id),
            args: vec![OscType::Float(x), OscType::Float(y), OscType::Float(z)],
        };
        let bytes = rosc::encoder::encode(&OscPacket::Message(msg))?;
        self.send_to_all(&bytes);
        Ok(())
    }

    /// Send bed channel configuration via OSC
    /// Format: /omniphony/bed/config channel_count
    pub fn send_bed_config(&self, channel_count: u32) -> Result<()> {
        let msg = OscMessage {
            addr: "/omniphony/bed/config".to_string(),
            args: vec![OscType::Int(channel_count as i32)],
        };
        let bytes = rosc::encoder::encode(&OscPacket::Message(msg))?;
        self.send_to_all(&bytes);
        Ok(())
    }

    /// Send sample position (timestamp) via OSC
    /// Format: /omniphony/timestamp samples seconds
    pub fn send_timestamp(&self, sample_pos: u64, seconds: f64) -> Result<()> {
        let msg = OscMessage {
            addr: "/omniphony/timestamp".to_string(),
            args: vec![OscType::Long(sample_pos as i64), OscType::Double(seconds)],
        };
        let bytes = rosc::encoder::encode(&OscPacket::Message(msg))?;
        self.send_to_all(&bytes);
        Ok(())
    }

    /// Send a frame of spatial object metadata via OSC.
    ///
    /// Always emits `/omniphony/spatial/frame` with the total object count.
    /// Per-object messages are emitted in the bridge native format:
    /// `/omniphony/object/{id}/xyz` for cartesian and `/omniphony/object/{id}/aed` for polar.
    /// They are only sent for objects
    /// whose position/gain/priority/divergence changed since the last call
    /// (epsilon-compared), or unconditionally when the object count changes.
    pub fn send_object_frame(
        &mut self,
        sample_pos: u64,
        ramp_duration: u32,
        coordinate_format: i32,
        objects: &[ObjectMeta],
    ) -> Result<()> {
        let frame_msg = OscMessage {
            addr: "/omniphony/spatial/frame".to_string(),
            args: vec![
                OscType::Long(sample_pos as i64),
                OscType::Long(self.content_generation as i64),
                OscType::Int(objects.len() as i32),
                OscType::Int(coordinate_format),
            ],
        };
        let bytes = rosc::encoder::encode(&OscPacket::Message(frame_msg))?;
        self.send_to_all(&bytes);

        let prev_len = self.prev_objects.as_ref().map_or(0, |prev| prev.len());
        let force_full = self
            .prev_objects
            .as_ref()
            .map_or(true, |prev| prev.len() != objects.len())
            || self.force_full_next.swap(false, Ordering::Relaxed);
        // Object list shrank: emit tombstones for stale IDs so viewers can remove them.
        // We use the previous native position suffix with gain=-128 (silence) and empty name
        // for backward compatibility with current listeners.
        for stale_id in objects.len()..prev_len {
            let suffix = self
                .prev_objects
                .as_ref()
                .and_then(|prev| prev.get(stale_id))
                .map(|obj| {
                    if obj.coord_mode.eq_ignore_ascii_case("cartesian") {
                        "xyz"
                    } else {
                        "aed"
                    }
                })
                .unwrap_or(if coordinate_format == 1 { "aed" } else { "xyz" });
            let msg = OscMessage {
                addr: format!("/omniphony/object/{}/{}", stale_id, suffix),
                args: vec![
                    OscType::Float(0.0),
                    OscType::Float(0.0),
                    OscType::Float(0.0),
                    OscType::Int(-1),
                    OscType::Int(-128),
                    OscType::Float(0.0),
                    OscType::Float(0.0),
                    OscType::Int(ramp_duration as i32),
                    OscType::Long(self.content_generation as i64),
                    OscType::String(String::new()),
                ],
            };
            let bytes = rosc::encoder::encode(&OscPacket::Message(msg))?;
            self.send_to_all(&bytes);
        }

        for (object_id, obj) in objects.iter().enumerate() {
            let changed =
                force_full || !self.prev_objects.as_ref().unwrap()[object_id].matches(obj);

            if changed {
                let suffix = if obj.coord_mode.eq_ignore_ascii_case("cartesian") {
                    "xyz"
                } else {
                    "aed"
                };
                let msg = OscMessage {
                    addr: format!("/omniphony/object/{}/{}", object_id, suffix),
                    args: vec![
                        OscType::Float(obj.x),
                        OscType::Float(obj.y),
                        OscType::Float(obj.z),
                        OscType::Int(obj.direct_speaker_index.map(|v| v as i32).unwrap_or(-1)),
                        OscType::Int(obj.gain),
                        OscType::Float(obj.priority),
                        OscType::Float(obj.divergence),
                        OscType::Int(ramp_duration as i32),
                        OscType::Long(self.content_generation as i64),
                        OscType::String(obj.name.clone()),
                    ],
                };
                let bytes = rosc::encoder::encode(&OscPacket::Message(msg))?;
                self.send_to_all(&bytes);
            }
        }

        self.prev_objects = Some(objects.iter().map(ObjectSnapshot::from_meta).collect());
        Ok(())
    }

    pub fn bump_content_generation(&mut self) {
        self.content_generation = self.content_generation.saturating_add(1);
        self.prev_objects = None;
        self.force_full_next.store(true, Ordering::Relaxed);
    }

    // -------------------------------------------------------------------------
    // Metering bundle
    // -------------------------------------------------------------------------

    /// Send a metering bundle (peak + RMS dBFS per object, speaker gains per object,
    /// and peak + RMS per speaker) via OSC. All messages in a single UDP packet.
    ///
    /// `object_gains`: slice of `(channel_idx, gains_per_speaker)` from
    /// `SpatialRenderer::last_object_gains()`. For each object, emits:
    ///   `/omniphony/meter/object/{idx}`        [f peak_dBFS, f rms_dBFS]
    ///   `/omniphony/meter/object/{idx}/gains`  [f g0, f g1, ..., f gN]
    pub fn send_meter_bundle(
        &self,
        snapshot: &renderer::metering::MeterSnapshot,
        object_gains: &[(usize, renderer::spatial_vbap::Gains)],
        decode_time_ms: Option<f32>,
        render_time_ms: Option<f32>,
        write_time_ms: Option<f32>,
        frame_duration_ms: Option<f32>,
        latency_instant_ms: Option<f32>,
        latency_control_ms: Option<f32>,
        latency_target_ms: Option<f32>,
        resample_ratio: Option<f32>,
        adaptive_band: Option<&str>,
    ) -> Result<()> {
        // Build an indexable lookup table once (avoids per-frame HashMap hashing).
        let max_gain_id = object_gains.iter().map(|(idx, _)| *idx).max().unwrap_or(0);
        let mut gains_by_id: Vec<Option<&renderer::spatial_vbap::Gains>> =
            vec![None; max_gain_id.saturating_add(1)];
        for (idx, g) in object_gains {
            if *idx < gains_by_id.len() {
                gains_by_id[*idx] = Some(g);
            }
        }

        let mut messages = Vec::with_capacity(
            snapshot.object_levels.len() * 2 + snapshot.speaker_levels.len() + 1,
        );

        // Backward-compatible aggregate latency for existing clients.
        // Prefer target/compensated delay, else fall back to instantaneous delay.
        if let Some(ms) = latency_target_ms.or(latency_instant_ms) {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/latency".to_string(),
                args: vec![OscType::Float(ms)],
            }));
        }
        if let Some(ms) = decode_time_ms {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/decode_time_ms".to_string(),
                args: vec![OscType::Float(ms.max(0.0))],
            }));
        }
        if let Some(ms) = render_time_ms {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/render_time_ms".to_string(),
                args: vec![OscType::Float(ms.max(0.0))],
            }));
        }
        if let Some(ms) = write_time_ms {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/write_time_ms".to_string(),
                args: vec![OscType::Float(ms.max(0.0))],
            }));
        }
        if let Some(ms) = frame_duration_ms {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/frame_duration_ms".to_string(),
                args: vec![OscType::Float(ms.max(0.0))],
            }));
        }
        if let Some(ms) = latency_instant_ms {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/latency_instant".to_string(),
                args: vec![OscType::Float(ms)],
            }));
        }
        if let Some(ms) = latency_control_ms {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/latency_control".to_string(),
                args: vec![OscType::Float(ms)],
            }));
        }
        if let Some(ms) = latency_target_ms {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/latency_target".to_string(),
                args: vec![OscType::Float(ms)],
            }));
        }

        // PI controller rate-adjust factor (1.0 = nominal). Only sent when adaptive
        // resampling is active, so the visualiser knows it is meaningful.
        if let Some(ratio) = resample_ratio {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/resample_ratio".to_string(),
                args: vec![OscType::Float(ratio)],
            }));
        }
        if let Some(band) = adaptive_band {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/adaptive_resampling/band".to_string(),
                args: vec![OscType::String(band.to_string())],
            }));
        }

        // Current audio output format state (sample rate + sample format).
        if let Some(ref control) = self.audio_control {
            let (current_rate_opt, fmt) = control.audio_state();
            let rate_opt = current_rate_opt.or_else(|| control.requested_output_sample_rate());
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/audio/output_device".to_string(),
                args: vec![OscType::String(
                    control.requested_output_device().unwrap_or_default(),
                )],
            }));
            if let Some(rate) = rate_opt {
                messages.push(OscPacket::Message(OscMessage {
                    addr: "/omniphony/state/audio/sample_rate".to_string(),
                    args: vec![OscType::Int(rate as i32)],
                }));
            }
            if !fmt.is_empty() {
                messages.push(OscPacket::Message(OscMessage {
                    addr: "/omniphony/state/audio/sample_format".to_string(),
                    args: vec![OscType::String(fmt)],
                }));
            }
            if let Some(error) = control.audio_error() {
                messages.push(OscPacket::Message(OscMessage {
                    addr: "/omniphony/state/audio/error".to_string(),
                    args: vec![OscType::String(error)],
                }));
            }
        }

        for &(id, peak, rms) in &snapshot.object_levels {
            messages.push(OscPacket::Message(OscMessage {
                addr: format!("/omniphony/meter/object/{}", id),
                args: vec![OscType::Float(peak), OscType::Float(rms)],
            }));
            if let Some(gains) = gains_by_id.get(id as usize).and_then(|entry| *entry) {
                messages.push(OscPacket::Message(OscMessage {
                    addr: format!("/omniphony/meter/object/{}/gains", id),
                    args: gains.iter().map(|&g| OscType::Float(g)).collect(),
                }));
            }
        }
        for (idx, &(peak, rms)) in snapshot.speaker_levels.iter().enumerate() {
            messages.push(OscPacket::Message(OscMessage {
                addr: format!("/omniphony/meter/speaker/{}", idx),
                args: vec![OscType::Float(peak), OscType::Float(rms)],
            }));
        }

        let bundle = OscPacket::Bundle(OscBundle {
            timetag: OscTime {
                seconds: 0,
                fractional: 1,
            },
            content: messages,
        });

        let bytes = rosc::encoder::encode(&bundle)?;
        self.send_to_metering_clients(&bytes);
        Ok(())
    }

    pub fn send_timing_update(
        &self,
        decode_time_ms: Option<f32>,
        render_time_ms: Option<f32>,
        write_time_ms: Option<f32>,
    ) -> Result<()> {
        let mut messages = Vec::new();
        if let Some(ms) = decode_time_ms {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/decode_time_ms".to_string(),
                args: vec![OscType::Float(ms.max(0.0))],
            }));
        }
        if let Some(ms) = render_time_ms {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/render_time_ms".to_string(),
                args: vec![OscType::Float(ms.max(0.0))],
            }));
        }
        if let Some(ms) = write_time_ms {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/write_time_ms".to_string(),
                args: vec![OscType::Float(ms.max(0.0))],
            }));
        }
        if messages.is_empty() {
            return Ok(());
        }
        let packet = OscPacket::Bundle(OscBundle {
            timetag: OscTime::from((0, 1)),
            content: messages,
        });
        let bytes = rosc::encoder::encode(&packet)?;
        self.send_to_metering_clients(&bytes);
        Ok(())
    }

    /// Send current audio output format state.
    pub fn send_audio_state(&self, sample_rate_hz: u32, sample_format: &str) -> Result<()> {
        let requested_output_device = self
            .audio_control
            .as_ref()
            .and_then(|control| control.requested_output_device())
            .unwrap_or_default();
        let audio_error = self
            .audio_control
            .as_ref()
            .and_then(|control| control.audio_error())
            .unwrap_or_default();
        let output_devices_json = self
            .audio_control
            .as_ref()
            .and_then(|control| serde_json::to_string(&control.available_output_devices()).ok())
            .unwrap_or_else(|| "[]".to_string());
        let announced_rate = self
            .audio_control
            .as_ref()
            .and_then(|control| control.requested_output_sample_rate())
            .unwrap_or(sample_rate_hz);
        let bundle = OscPacket::Bundle(OscBundle {
            timetag: OscTime {
                seconds: 0,
                fractional: 1,
            },
            content: vec![
                OscPacket::Message(OscMessage {
                    addr: "/omniphony/state/audio/output_devices".to_string(),
                    args: vec![OscType::String(output_devices_json)],
                }),
                OscPacket::Message(OscMessage {
                    addr: "/omniphony/state/audio/output_device".to_string(),
                    args: vec![OscType::String(requested_output_device)],
                }),
                OscPacket::Message(OscMessage {
                    addr: "/omniphony/state/audio/sample_rate".to_string(),
                    args: vec![OscType::Int(announced_rate as i32)],
                }),
                OscPacket::Message(OscMessage {
                    addr: "/omniphony/state/audio/sample_format".to_string(),
                    args: vec![OscType::String(sample_format.to_string())],
                }),
                OscPacket::Message(OscMessage {
                    addr: "/omniphony/state/audio/error".to_string(),
                    args: vec![OscType::String(audio_error)],
                }),
            ],
        });
        let bytes = rosc::encoder::encode(&bundle)?;
        self.send_to_all(&bytes);
        Ok(())
    }
}

/// Dispatch a `/omniphony/control/…` message.
fn handle_control_message(
    msg: &OscMessage,
    src: SocketAddr,
    control: &Arc<RendererControl>,
    audio_control: Option<&Arc<AudioControl>>,
    pending_speakers: &mut HashMap<usize, SpeakerPatch>,
    socket: &Arc<UdpSocket>,
    clients: &Arc<Mutex<OscClients>>,
) {
    let addr = msg.addr.as_str();
    let runtime_ctx = RuntimeControlContext::new(Arc::clone(control), audio_control.cloned());

    if addr == "/omniphony/control/metering" {
        let enabled = match msg.args.first() {
            Some(OscType::Int(i)) => *i != 0,
            Some(OscType::Float(f)) => *f != 0.0,
            _ => return,
        };
        let client = resolve_register_addr(src, &[]);
        let mut map = clients.lock().unwrap();
        if let Some(entry) = map.get_mut(&client) {
            entry.metering_enabled = enabled;
            drop(map);
            send_metering_state(socket, client, enabled);
        }
        return;
    }

    // ── Save config ──────────────────────────────────────────────────────────
    if let Some(command) = parse_process_command(msg) {
        match command {
            RuntimeCommand::SaveConfig => save_live_config(control, audio_control, socket, clients),
            RuntimeCommand::ReloadConfig => {
                log::info!("OSC reload_config requested");
                sys::shutdown::request_restart_from_config();
            }
            RuntimeCommand::Quit => {
                log::info!("OSC quit requested");
                sys::shutdown::request_shutdown();
            }
            RuntimeCommand::SetLogLevel(requested) => {
                sys::live_log::set_runtime_level(requested);
                broadcast_string(
                    socket,
                    clients,
                    "/omniphony/state/log_level",
                    sys::live_log::current_runtime_level_name(),
                );
                log::info!(
                    "OSC: log_level → {}",
                    sys::live_log::current_runtime_level_name()
                );
            }
        }
        return;
    }

    if let Some(effects) = apply_simple_osc_control(msg, &runtime_ctx) {
        apply_control_effects(effects, control, socket, clients);
        return;
    }

    if let Some(effects) = apply_speaker_osc_control(msg, &runtime_ctx, pending_speakers) {
        apply_control_effects(effects, control, socket, clients);
        return;
    }

    // ── Export current layout to its own YAML file ──────────────────────────
    if addr == "/omniphony/control/layout/export" {
        let requested_name = match msg.args.first() {
            Some(OscType::String(s)) if !s.trim().is_empty() => Some(s.trim()),
            _ => None,
        };
        export_current_layout(control, requested_name);
        return;
    }
}

/// Mark live params as dirty (changed since last save) and broadcast the state.
fn set_dirty(control: &Arc<RendererControl>, socket: &UdpSocket, clients: &Mutex<OscClients>) {
    control.mark_dirty();
    broadcast_int(socket, clients, "/omniphony/state/config/saved", 0);
}

fn apply_control_effects(
    effects: runtime_control::osc::ControlEffects,
    control: &Arc<RendererControl>,
    socket: &Arc<UdpSocket>,
    clients: &Arc<Mutex<OscClients>>,
) {
    if effects.mark_dirty {
        set_dirty(control, socket, clients);
    }
    if let Some(layout) = effects.speaker_layout_broadcast.as_ref() {
        broadcast_speaker_config(socket, clients, layout);
    }
    for update in effects.broadcasts {
        match update.value {
            BroadcastValue::Int(value) => broadcast_int(socket, clients, &update.addr, value),
            BroadcastValue::Float(value) => broadcast_float(socket, clients, &update.addr, value),
            BroadcastValue::Fff(a, b, c) => broadcast_fff(socket, clients, &update.addr, a, b, c),
            BroadcastValue::String(value) => broadcast_string(socket, clients, &update.addr, &value),
        }
    }
    if let Some(message) = effects.log_message {
        log::info!("{message}");
    }
    if effects.trigger_layout_recompute {
        trigger_layout_recompute(control, socket, clients);
    }
}
