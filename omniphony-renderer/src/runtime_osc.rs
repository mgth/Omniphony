use anyhow::Result;
use audio_output::AudioControl;
use rosc::{OscBundle, OscMessage, OscPacket, OscTime, OscType};
use runtime_control::command::{RuntimeCommand, parse_process_command};
use runtime_control::context::RuntimeControlContext;
use runtime_control::osc::{BroadcastValue, apply_simple_osc_control};
use std::collections::HashMap;
use std::net::{SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use renderer::live_params::RendererControl;

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

// -------------------------------------------------------------------------
// Speaker config bundle builder (used at startup + sent on registration)
// -------------------------------------------------------------------------

/// Build the pre-encoded speaker config bundle to send to newly registered clients.
///
/// Format:
///   `/omniphony/config/speakers`    [i count]
///   `/omniphony/config/speaker/{i}` [s name, f azimuth_deg, f elevation_deg, f distance_m, i spatialize, f delay_ms, s coord_mode, f x, f y, f z]
pub fn build_speaker_config_bundle(
    layout: &renderer::speaker_layout::SpeakerLayout,
) -> Result<Vec<u8>> {
    let mut messages = Vec::with_capacity(1 + layout.num_speakers());
    messages.push(OscPacket::Message(OscMessage {
        addr: "/omniphony/config/speakers".to_string(),
        args: vec![OscType::Int(layout.num_speakers() as i32)],
    }));
    for (idx, speaker) in layout.speakers.iter().enumerate() {
        messages.push(OscPacket::Message(OscMessage {
            addr: format!("/omniphony/config/speaker/{}", idx),
            args: vec![
                OscType::String(speaker.name.clone()),
                OscType::Float(speaker.azimuth),
                OscType::Float(speaker.elevation),
                OscType::Float(speaker.distance),
                OscType::Int(if speaker.spatialize { 1 } else { 0 }),
                OscType::Float(speaker.delay_ms),
                OscType::String(speaker.coord_mode.clone()),
                OscType::Float(speaker.x),
                OscType::Float(speaker.y),
                OscType::Float(speaker.z),
            ],
        }));
    }
    let bundle = OscPacket::Bundle(OscBundle {
        timetag: OscTime {
            seconds: 0,
            fractional: 1,
        },
        content: messages,
    });
    Ok(rosc::encoder::encode(&bundle)?)
}

// -------------------------------------------------------------------------
// Live-parameter control helpers (used by the listener thread)
// -------------------------------------------------------------------------

/// Staged speaker-position patch — fields that have been set by the client
/// for a given speaker index but not yet applied.
#[derive(Default)]
struct SpeakerPatch {
    az: Option<f32>,
    el: Option<f32>,
    distance: Option<f32>,
    x: Option<f32>,
    y: Option<f32>,
    z: Option<f32>,
    coord_mode: Option<String>,
    spatialize: Option<bool>,
    name: Option<String>,
}

fn broadcast_speaker_config(
    socket: &UdpSocket,
    clients: &Mutex<OscClients>,
    layout: &renderer::speaker_layout::SpeakerLayout,
) {
    match build_speaker_config_bundle(layout) {
        Ok(bytes) => send_raw(socket, clients, &bytes),
        Err(e) => log::warn!("OSC: failed to broadcast speaker config: {}", e),
    }
}

fn remap_live_speakers_remove(
    speakers: &mut std::collections::HashMap<usize, renderer::live_params::SpeakerLiveParams>,
    remove_idx: usize,
) {
    let mut next = std::collections::HashMap::new();
    for (idx, params) in speakers.drain() {
        if idx == remove_idx {
            continue;
        }
        let mapped = if idx > remove_idx { idx - 1 } else { idx };
        next.insert(mapped, params);
    }
    *speakers = next;
}

fn remap_live_speakers_move(
    speakers: &mut std::collections::HashMap<usize, renderer::live_params::SpeakerLiveParams>,
    from: usize,
    to: usize,
) {
    if from == to {
        return;
    }
    let moved = speakers.remove(&from);
    let mut next = std::collections::HashMap::new();
    for (idx, params) in speakers.drain() {
        let mapped = if from < to {
            if idx > from && idx <= to {
                idx - 1
            } else {
                idx
            }
        } else if idx >= to && idx < from {
            idx + 1
        } else {
            idx
        };
        next.insert(mapped, params);
    }
    if let Some(params) = moved {
        next.insert(to, params);
    }
    *speakers = next;
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
        if effects.mark_dirty {
            set_dirty(control, socket, clients);
        }
        for update in effects.broadcasts {
            match update.value {
                BroadcastValue::Int(value) => {
                    broadcast_int(socket, clients, &update.addr, value)
                }
                BroadcastValue::Float(value) => {
                    broadcast_float(socket, clients, &update.addr, value)
                }
                BroadcastValue::Fff(a, b, c) => broadcast_fff(socket, clients, &update.addr, a, b, c),
                BroadcastValue::String(value) => {
                    broadcast_string(socket, clients, &update.addr, &value)
                }
            }
        }
        if let Some(message) = effects.log_message {
            log::info!("{message}");
        }
        if effects.trigger_layout_recompute {
            trigger_layout_recompute(control, socket, clients);
        }
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

    // ── Speaker collection edit: /omniphony/control/speakers/{add|remove|move} ──
    if addr == "/omniphony/control/speakers/add" {
        pending_speakers.clear();
        let idx = control.editable_layout().speakers.len();
        let default_name = format!("spk-{}", idx);
        let name = match msg.args.first() {
            Some(OscType::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
            _ => default_name,
        };
        let az = match msg.args.get(1) {
            Some(OscType::Float(v)) => *v,
            _ => 0.0,
        };
        let el = match msg.args.get(2) {
            Some(OscType::Float(v)) => *v,
            _ => 0.0,
        };
        let distance = match msg.args.get(3) {
            Some(OscType::Float(v)) => v.max(0.01),
            _ => 1.0,
        };
        let spatialize = match msg.args.get(4) {
            Some(OscType::Int(v)) => *v != 0,
            Some(OscType::Float(v)) => *v != 0.0,
            _ => true,
        };
        let delay_ms = match msg.args.get(5) {
            Some(OscType::Float(v)) => v.max(0.0),
            _ => 0.0,
        };
        let layout = control.with_editable_layout(|layout| {
            layout
                .speakers
                .push(renderer::speaker_layout::Speaker::from_polar(
                    name,
                    az.clamp(-180.0, 180.0),
                    el.clamp(-90.0, 90.0),
                    distance,
                    spatialize,
                    delay_ms,
                ));
            layout.clone()
        });
        if delay_ms > 0.0 {
            control
                .live
                .write()
                .unwrap()
                .speakers
                .entry(idx)
                .or_default()
                .delay_ms = delay_ms;
            control.mark_speaker_params_dirty();
        }
        broadcast_speaker_config(socket, clients, &layout);
        set_dirty(control, socket, clients);
        trigger_layout_recompute(control, socket, clients);
        return;
    }

    if addr == "/omniphony/control/speakers/remove" {
        pending_speakers.clear();
        let remove_idx = match msg.args.first() {
            Some(OscType::Int(v)) if *v >= 0 => *v as usize,
            Some(OscType::Float(v)) if *v >= 0.0 => *v as usize,
            _ => return,
        };
        let Some(layout) = control.with_editable_layout(|layout| {
            if remove_idx >= layout.speakers.len() {
                return None;
            }
            layout.speakers.remove(remove_idx);
            Some(layout.clone())
        }) else {
            return;
        };
        broadcast_speaker_config(socket, clients, &layout);
        {
            let mut live = control.live.write().unwrap();
            remap_live_speakers_remove(&mut live.speakers, remove_idx);
        }
        control.mark_speaker_params_dirty();
        set_dirty(control, socket, clients);
        trigger_layout_recompute(control, socket, clients);
        return;
    }

    if addr == "/omniphony/control/speakers/move" {
        pending_speakers.clear();
        let from_idx = match msg.args.first() {
            Some(OscType::Int(v)) if *v >= 0 => *v as usize,
            Some(OscType::Float(v)) if *v >= 0.0 => *v as usize,
            _ => return,
        };
        let to_idx = match msg.args.get(1) {
            Some(OscType::Int(v)) if *v >= 0 => *v as usize,
            Some(OscType::Float(v)) if *v >= 0.0 => *v as usize,
            _ => return,
        };
        let Some(layout) = control.with_editable_layout(|layout| {
            let len = layout.speakers.len();
            if from_idx >= len || to_idx >= len || from_idx == to_idx {
                return None;
            }
            let speaker = layout.speakers.remove(from_idx);
            layout.speakers.insert(to_idx, speaker);
            Some(layout.clone())
        }) else {
            return;
        };
        broadcast_speaker_config(socket, clients, &layout);
        {
            let mut live = control.live.write().unwrap();
            remap_live_speakers_move(&mut live.speakers, from_idx, to_idx);
        }
        control.mark_speaker_params_dirty();
        set_dirty(control, socket, clients);
        trigger_layout_recompute(control, socket, clients);
        return;
    }

    // ── Speaker staging: /omniphony/control/speaker/{idx}/{az|el|distance|spatialize|name} ──
    if let Some(rest) = addr.strip_prefix("/omniphony/control/speaker/") {
        let parts: Vec<&str> = rest.splitn(2, '/').collect();
        if parts.len() == 2 {
            if let Ok(idx) = parts[0].parse::<usize>() {
                if parts[1] == "mute" {
                    // Accept both Int and Float for mute (some OSC clients send 0.0/1.0).
                    let muted_opt = match msg.args.first() {
                        Some(OscType::Int(i)) => Some(*i != 0),
                        Some(OscType::Float(f)) => Some(*f != 0.0),
                        _ => None,
                    };
                    if let Some(muted) = muted_opt {
                        control
                            .live
                            .write()
                            .unwrap()
                            .speakers
                            .entry(idx)
                            .or_default()
                            .muted = muted;
                        control.mark_speaker_params_dirty();
                        set_dirty(control, socket, clients);
                        broadcast_int(
                            socket,
                            clients,
                            &format!("/omniphony/state/speaker/{}/mute", idx),
                            if muted { 1 } else { 0 },
                        );
                        log::info!("OSC: speaker[{}] mute → {}", idx, muted);
                    }
                } else if parts[1] == "spatialize" {
                    let spatialize_opt = match msg.args.first() {
                        Some(OscType::Int(i)) => Some(*i != 0),
                        Some(OscType::Float(f)) => Some(*f != 0.0),
                        _ => None,
                    };
                    if let Some(spatialize) = spatialize_opt {
                        let patch = pending_speakers
                            .entry(idx)
                            .or_insert_with(SpeakerPatch::default);
                        patch.spatialize = Some(spatialize);
                    }
                } else if parts[1] == "name" {
                    if let Some(OscType::String(name)) = msg.args.first() {
                        let trimmed = name.trim();
                        if !trimmed.is_empty() {
                            let patch = pending_speakers
                                .entry(idx)
                                .or_insert_with(SpeakerPatch::default);
                            patch.name = Some(trimmed.to_string());
                        }
                    }
                } else if parts[1] == "coord_mode" {
                    if let Some(OscType::String(mode)) = msg.args.first() {
                        let normalized = if mode.eq_ignore_ascii_case("cartesian") {
                            "cartesian"
                        } else {
                            "polar"
                        };
                        let patch = pending_speakers
                            .entry(idx)
                            .or_insert_with(SpeakerPatch::default);
                        patch.coord_mode = Some(normalized.to_string());
                    }
                } else if let Some(OscType::Float(f)) = msg.args.first() {
                    let patch = pending_speakers
                        .entry(idx)
                        .or_insert_with(SpeakerPatch::default);
                    match parts[1] {
                        "az" => {
                            patch.az = Some(*f);
                        }
                        "el" => {
                            patch.el = Some(*f);
                        }
                        "distance" => {
                            patch.distance = Some(*f);
                        }
                        "x" => {
                            patch.x = Some(f.clamp(-1.0, 1.0));
                        }
                        "y" => {
                            patch.y = Some(f.clamp(-1.0, 1.0));
                        }
                        "z" => {
                            patch.z = Some(f.clamp(-1.0, 1.0));
                        }
                        "gain" => {
                            let gain = *f;
                            control
                                .live
                                .write()
                                .unwrap()
                                .speakers
                                .entry(idx)
                                .or_default()
                                .gain = gain;
                            control.mark_speaker_params_dirty();
                            set_dirty(control, socket, clients);
                            broadcast_float(
                                socket,
                                clients,
                                &format!("/omniphony/state/speaker/{}/gain", idx),
                                gain,
                            );
                        }
                        "delay" => {
                            let delay_ms = f.max(0.0);
                            control
                                .live
                                .write()
                                .unwrap()
                                .speakers
                                .entry(idx)
                                .or_default()
                                .delay_ms = delay_ms;
                            control.mark_speaker_params_dirty();
                            control.with_editable_layout(|layout| {
                                if let Some(spk) = layout.speakers.get_mut(idx) {
                                    spk.delay_ms = delay_ms;
                                }
                            });
                            set_dirty(control, socket, clients);
                            broadcast_float(
                                socket,
                                clients,
                                &format!("/omniphony/state/speaker/{}/delay", idx),
                                delay_ms,
                            );
                            log::info!("OSC: speaker[{}] delay → {:.2} ms", idx, delay_ms);
                        }
                        _ => {}
                    }
                }
            }
        }
        return;
    }

    // ── Apply staged speaker patches ─────────────────────────────────────────
    if addr == "/omniphony/control/speakers/apply" {
        apply_pending_speakers(pending_speakers, control, socket, clients);
        set_dirty(control, socket, clients);
        return;
    }

    // ── Reset staged speaker patches ─────────────────────────────────────────
    if addr == "/omniphony/control/speakers/reset" {
        pending_speakers.clear();
        return;
    }
}

/// Apply staged speaker-position patches, then trigger a background VBAP recompute.
fn apply_pending_speakers(
    pending: &mut HashMap<usize, SpeakerPatch>,
    control: &Arc<RendererControl>,
    socket: &Arc<UdpSocket>,
    clients: &Arc<Mutex<OscClients>>,
) {
    let layout = control.with_editable_layout(|layout| {
        for (idx, patch) in pending.iter() {
            if let Some(speaker) = layout.speakers.get_mut(*idx) {
                if let Some(az) = patch.az {
                    speaker.azimuth = az;
                }
                if let Some(el) = patch.el {
                    speaker.elevation = el;
                }
                if let Some(dist) = patch.distance {
                    speaker.distance = dist;
                }
                if let Some(x) = patch.x {
                    speaker.x = x.clamp(-1.0, 1.0);
                }
                if let Some(y) = patch.y {
                    speaker.y = y.clamp(-1.0, 1.0);
                }
                if let Some(z) = patch.z {
                    speaker.z = z.clamp(-1.0, 1.0);
                }
                if let Some(coord_mode) = &patch.coord_mode {
                    speaker.coord_mode = if coord_mode.eq_ignore_ascii_case("cartesian") {
                        "cartesian".to_string()
                    } else {
                        "polar".to_string()
                    };
                }
                if let Some(spatialize) = patch.spatialize {
                    speaker.spatialize = spatialize;
                }
                if let Some(name) = &patch.name {
                    speaker.name = name.clone();
                }
            }
        }
        layout.clone()
    });
    broadcast_speaker_config(socket, clients, &layout);
    pending.clear();
    trigger_layout_recompute(control, socket, clients);
}

fn trigger_layout_recompute(
    control: &Arc<RendererControl>,
    socket: &Arc<UdpSocket>,
    clients: &Arc<Mutex<OscClients>>,
) {
    #[cfg(not(feature = "saf_vbap"))]
    {
        let _ = control;
        log::warn!("OSC apply: VBAP recompute requires a build with the 'saf_vbap' feature");
        broadcast_int(socket, clients, "/omniphony/state/speakers/recomputing", 0);
        return;
    }

    #[cfg(feature = "saf_vbap")]
    {
        // 1. Recompute is only possible with the saf_vbap path.
        if control.vbap_rebuild_params.is_none() {
            log::warn!(
                "OSC apply: VBAP speaker positions cannot be updated — pre-loaded table does not support recompute"
            );
            broadcast_int(socket, clients, "/omniphony/state/speakers/recomputing", 0);
            return;
        }

        // 2. Reject if a recompute is already in progress.
        if control
            .recomputing
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            log::warn!("OSC apply: VBAP recompute already in progress, ignoring");
            broadcast_int(socket, clients, "/omniphony/state/speakers/recomputing", 1);
            return;
        }

        // 3. Snapshot a complete rebuild plan from the editable state.
        let rebuild_plan = match control.prepare_topology_rebuild() {
            Some(plan) => plan,
            None => {
                log::warn!("OSC apply: failed to prepare VBAP recompute plan");
                broadcast_int(socket, clients, "/omniphony/state/speakers/recomputing", 0);
                return;
            }
        };

        // 4. Mark recomputing.
        control
            .recomputing
            .store(true, std::sync::atomic::Ordering::Relaxed);

        // 5. Broadcast recomputing=1.
        broadcast_int(socket, clients, "/omniphony/state/speakers/recomputing", 1);

        // 6. Spawn background recompute thread (saf_vbap only).
        let control_clone = Arc::clone(control);
        let socket_clone = Arc::clone(socket);
        let clients_clone = Arc::clone(clients);
        let rebuild_plan_for_thread = rebuild_plan.clone();

        #[cfg(feature = "saf_vbap")]
        {
            std::thread::Builder::new()
            .name("vbap-recompute".into())
            .spawn(move || {
                log::info!(
                    "VBAP recompute started (azimuth_resolution={}, elevation_resolution={}, distance_res={}, distance_max={}, mode={:?})",
                    rebuild_plan_for_thread.azimuth_resolution,
                    rebuild_plan_for_thread.elevation_resolution,
                    rebuild_plan_for_thread.distance_res,
                    rebuild_plan_for_thread.distance_max,
                    rebuild_plan_for_thread.table_mode
                );
                match rebuild_plan_for_thread.build_topology() {
                    Ok(new_topology) => {
                        control_clone.publish_topology(new_topology);
                        control_clone
                            .recomputing
                            .store(false, std::sync::atomic::Ordering::Relaxed);
                        log::info!("VBAP updated with new speaker layout");
                        let effective_mode = match control_clone.active_topology().vbap.table_mode()
                        {
                            renderer::spatial_vbap::VbapTableMode::Polar => "polar",
                            renderer::spatial_vbap::VbapTableMode::Cartesian { .. } => "cartesian",
                        };
                        broadcast_string(
                            &socket_clone,
                            &clients_clone,
                            "/omniphony/state/vbap/effective_mode",
                            effective_mode,
                        );
                        broadcast_int(
                            &socket_clone,
                            &clients_clone,
                            "/omniphony/state/speakers/recomputing",
                            0,
                        );
                        // Broadcast updated speaker positions.
                        for (idx, speaker) in rebuild_plan_for_thread.layout.speakers.iter().enumerate() {
                            broadcast_fff(
                                &socket_clone,
                                &clients_clone,
                                &format!("/omniphony/state/speaker/{}", idx),
                                speaker.azimuth,
                                speaker.elevation,
                                speaker.distance,
                            );
                            broadcast_int(
                                &socket_clone,
                                &clients_clone,
                                &format!("/omniphony/state/speaker/{}/spatialize", idx),
                                if speaker.spatialize { 1 } else { 0 },
                            );
                            broadcast_string(
                                &socket_clone,
                                &clients_clone,
                                &format!("/omniphony/state/speaker/{}/name", idx),
                                &speaker.name,
                            );
                        }
                        log::info!("VBAP recompute completed");
                    }
                    Err(e) => {
                        log::error!("VBAP recompute failed: {}", e);
                        control_clone
                            .recomputing
                            .store(false, std::sync::atomic::Ordering::Relaxed);
                        broadcast_int(
                            &socket_clone,
                            &clients_clone,
                            "/omniphony/state/speakers/recomputing",
                            0,
                        );
                    }
                }
            })
            .expect("failed to spawn vbap-recompute thread");
        }
    }
}

/// Build an OSC bundle describing the current `LiveParams` state, to be sent
/// to a newly registered client so it can initialise its UI.
fn build_live_state_bundle(
    control: &Arc<RendererControl>,
    audio_control: Option<&Arc<AudioControl>>,
) -> Vec<u8> {
    runtime_control::snapshot::build_live_state_bundle(control, audio_control)
}

// -------------------------------------------------------------------------
// Low-level broadcast helpers (used by the listener thread which holds raw Arcs)
// -------------------------------------------------------------------------

/// Send a single-float OSC message to all live clients (no pruning).
fn broadcast_float(socket: &UdpSocket, clients: &Mutex<OscClients>, addr: &str, value: f32) {
    let msg = OscMessage {
        addr: addr.to_string(),
        args: vec![OscType::Float(value)],
    };
    if let Ok(bytes) = rosc::encoder::encode(&OscPacket::Message(msg)) {
        send_raw(socket, clients, &bytes);
    }
}

/// Send a single-int OSC message to all live clients (no pruning).
fn broadcast_int(socket: &UdpSocket, clients: &Mutex<OscClients>, addr: &str, value: i32) {
    let msg = OscMessage {
        addr: addr.to_string(),
        args: vec![OscType::Int(value)],
    };
    if let Ok(bytes) = rosc::encoder::encode(&OscPacket::Message(msg)) {
        send_raw(socket, clients, &bytes);
    }
}

/// Send a three-float OSC message to all live clients (no pruning).
fn broadcast_fff(
    socket: &UdpSocket,
    clients: &Mutex<OscClients>,
    addr: &str,
    a: f32,
    b: f32,
    c: f32,
) {
    let msg = OscMessage {
        addr: addr.to_string(),
        args: vec![OscType::Float(a), OscType::Float(b), OscType::Float(c)],
    };
    if let Ok(bytes) = rosc::encoder::encode(&OscPacket::Message(msg)) {
        send_raw(socket, clients, &bytes);
    }
}

/// Send a single-string OSC message to all live clients (no pruning).
fn broadcast_string(socket: &UdpSocket, clients: &Mutex<OscClients>, addr: &str, value: &str) {
    let packet = OscPacket::Message(OscMessage {
        addr: addr.to_string(),
        args: vec![OscType::String(value.to_string())],
    });
    if let Ok(data) = rosc::encoder::encode(&packet) {
        send_raw(socket, clients, &data);
    }
}

/// Mark live params as dirty (changed since last save) and broadcast the state.
fn set_dirty(control: &Arc<RendererControl>, socket: &UdpSocket, clients: &Mutex<OscClients>) {
    control.mark_dirty();
    broadcast_int(socket, clients, "/omniphony/state/config/saved", 0);
}

/// Save live-tunable params from `RendererControl` into the config file.
/// Loads the existing config first to preserve non-live fields (osc, etc.).
/// Broadcasts `/omniphony/state/config/saved 1` on success; logs error and stays dirty on failure.
fn save_live_config(
    control: &Arc<RendererControl>,
    audio_control: Option<&Arc<AudioControl>>,
    socket: &UdpSocket,
    clients: &Mutex<OscClients>,
) {
    match runtime_control::persist::save_live_config(control, audio_control) {
        Ok(result) => {
            broadcast_int(socket, clients, "/omniphony/state/config/saved", 1);
            send_raw(socket, clients, &result.state_bundle);
            log::info!("OSC: config saved to {}", result.path.display());
        }
        Err(e) => {
            log::error!("OSC: failed to save config: {}", e);
        }
    }
}

fn default_layout_export_name(layout: &renderer::speaker_layout::SpeakerLayout) -> String {
    let mut a: usize = 0;
    let mut b: usize = 0;
    let mut c: usize = 0;
    for speaker in &layout.speakers {
        if !speaker.spatialize {
            b += 1;
            continue;
        }
        let el = speaker.elevation.to_radians();
        let y = speaker.distance * el.sin();
        if y > 0.5 {
            c += 1;
        } else {
            a += 1;
        }
    }
    format!("{}.{}.{}", a, b, c)
}

fn sanitize_layout_name(name: &str) -> String {
    let sanitized: String = name
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = sanitized.trim_matches('.');
    if trimmed.is_empty() {
        "layout".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Export the current in-memory speaker layout to a standalone YAML file.
///
/// Output path: `<config_dir>/layouts/<name>.yaml`
fn export_current_layout(control: &Arc<RendererControl>, requested_name: Option<&str>) {
    let config_path = {
        let guard = control.config_path.lock().unwrap();
        guard.clone()
    };
    let base_dir = config_path
        .as_ref()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let out_dir = base_dir.join("layouts");
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        log::error!(
            "OSC: failed to create layout export directory {}: {}",
            out_dir.display(),
            e
        );
        return;
    }
    let layout = control.editable_layout();
    let base_name = requested_name
        .map(sanitize_layout_name)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default_layout_export_name(&layout));
    let file_name = if base_name.to_ascii_lowercase().ends_with(".yaml") {
        base_name
    } else {
        format!("{}.yaml", base_name)
    };
    let out_path = out_dir.join(file_name);
    match layout.save_to_file(&out_path) {
        Ok(()) => log::info!("OSC: layout exported to {}", out_path.display()),
        Err(e) => log::error!(
            "OSC: failed to export layout to {}: {}",
            out_path.display(),
            e
        ),
    }
}

fn encode_log_record(record: &sys::live_log::BufferedLogRecord) -> Option<Vec<u8>> {
    let packet = OscPacket::Message(OscMessage {
        addr: "/omniphony/log".to_string(),
        args: vec![
            OscType::Long(record.seq as i64),
            OscType::String(record.level.clone()),
            OscType::String(record.target.clone()),
            OscType::String(record.message.clone()),
        ],
    });
    rosc::encoder::encode(&packet).ok()
}

fn send_buffered_logs_to_client(socket: &UdpSocket, client: SocketAddr, last_seq: u64) {
    for record in sys::live_log::records_since(last_seq) {
        if let Some(bytes) = encode_log_record(&record) {
            if let Err(e) = socket.send_to(&bytes, client) {
                log::warn!("Failed to send log record to {}: {}", client, e);
                break;
            }
        }
    }
}

fn flush_pending_logs(socket: &UdpSocket, clients: &Mutex<OscClients>, last_seq: &mut u64) {
    let records = sys::live_log::records_since(*last_seq);
    if records.is_empty() {
        return;
    }
    for record in &records {
        if let Some(bytes) = encode_log_record(record) {
            send_raw(socket, clients, &bytes);
        }
    }
    if let Some(last) = records.last() {
        *last_seq = last.seq;
    }
}

/// Send raw bytes to all currently-known live clients without pruning.
fn send_raw(socket: &UdpSocket, clients: &Mutex<OscClients>, bytes: &[u8]) {
    send_raw_filtered(socket, clients, bytes, |_| true);
}

fn send_raw_filtered<F>(socket: &UdpSocket, clients: &Mutex<OscClients>, bytes: &[u8], predicate: F)
where
    F: Fn(&OscClientState) -> bool,
{
    let mut clients_locked = clients.lock().unwrap();
    let now = Instant::now();
    clients_locked.retain(|addr, client| match client.last_seen {
        None => true,
        Some(t) => {
            if now.duration_since(t) >= CLIENT_TIMEOUT {
                log::info!("OSC client timed out, removing: {}", addr);
                false
            } else {
                true
            }
        }
    });
    for (addr, client) in clients_locked.iter() {
        if predicate(client) {
            if let Err(e) = socket.send_to(bytes, *addr) {
                log::warn!("OSC broadcast error to {}: {}", addr, e);
            }
        }
    }
}

fn send_metering_state(socket: &UdpSocket, client: SocketAddr, enabled: bool) {
    let packet = OscPacket::Message(OscMessage {
        addr: "/omniphony/state/osc/metering".to_string(),
        args: vec![OscType::Int(if enabled { 1 } else { 0 })],
    });
    if let Ok(bytes) = rosc::encoder::encode(&packet) {
        if let Err(e) = socket.send_to(&bytes, client) {
            log::warn!("Failed to send metering state to {}: {}", client, e);
        }
    }
}

// -------------------------------------------------------------------------
// Helpers
// -------------------------------------------------------------------------

/// Resolve the client address for a `/omniphony/register` message.
/// If args contain `[Int(port)]`, override the source port with that value
/// (useful when the client's send socket port differs from its listen port).
fn resolve_register_addr(src: SocketAddr, args: &[OscType]) -> SocketAddr {
    if let Some(OscType::Int(port)) = args.first() {
        if let Ok(port) = u16::try_from(*port) {
            return match src {
                SocketAddr::V4(v4) => SocketAddr::V4(SocketAddrV4::new(*v4.ip(), port)),
                SocketAddr::V6(mut v6) => {
                    v6.set_port(port);
                    SocketAddr::V6(v6)
                }
            };
        }
    }
    src
}
