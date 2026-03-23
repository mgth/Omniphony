use anyhow::Result;
use audio_output::AudioControl;
use rosc::{OscMessage, OscPacket};
use runtime_control::osc::SpeakerPatch;
use std::collections::HashMap;
use std::net::{SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use renderer::live_params::RendererControl;

mod client_registry;
mod export;
mod dispatch;
mod metadata_emit;
mod recompute;
mod state_emit;
mod transport;

use self::dispatch::handle_control_message;
use self::export::build_live_state_bundle;
use self::client_registry::OscClientRegistry;
use self::transport::{
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
    clients: Arc<OscClientRegistry>,
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
        let clients = Arc::new(OscClientRegistry::new(CLIENT_TIMEOUT));
        clients.insert_permanent(SocketAddr::V4(default_target));
        Ok(Self {
            socket: Arc::new(socket),
            clients,
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
                                    let (is_new, metering_enabled) = clients.register(client);
                                    if is_new {
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
                                    let is_known = clients.heartbeat(client);
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
        self.clients.is_any_live()
    }

    pub fn has_metering_clients(&self) -> bool {
        self.clients.is_any_metering_live()
    }
}
