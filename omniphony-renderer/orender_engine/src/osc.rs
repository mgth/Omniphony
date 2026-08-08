use anyhow::Result;
use rosc::{OscMessage, OscPacket};
use std::net::{SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use renderer::live_params::RendererControl;
use runtime_control::HostControlHandler;

mod client_registry;
mod dispatch;
mod export;
mod gaintable;
mod metadata_emit;
mod recompute;
mod state_emit;
mod transport;

use self::client_registry::OscClientRegistry;
use self::dispatch::{RealtimeSeqState, handle_control_message};
use self::export::build_live_state_bundle;
use self::gaintable::GaintableCache;
use self::transport::{
    flush_pending_logs, resolve_register_addr, send_buffered_logs_to_client, send_metering_state,
    send_raw_filtered,
};

/// Timeout after which a registered client (one that must heartbeat) is considered dead.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a starting instance waits for a yieldable holder of the OSC RX
/// port to shut down after `/omniphony/control/yield_port`. Generous on
/// purpose: the standby flushes its audio output and joins its threads before
/// the port is released.
const YIELD_REBIND_BUDGET: Duration = Duration::from_secs(5);

/// Poll interval while waiting for the RX port to free up after a yield request.
const YIELD_REBIND_POLL: Duration = Duration::from_millis(50);

/// Dynamic resume port advertised by a standby instance we displaced. When this
/// (port-taking) instance later releases the RX port — at `OscSender::Drop`,
/// e.g. mpv quitting — we send `/omniphony/control/resume` here so the standby
/// comes back. `None` until a `yield_port` reply carries it.
static RESUME_TARGET: Mutex<Option<u16>> = Mutex::new(None);

/// Send `/omniphony/control/yield_port` to the local holder of `rx_port` and,
/// briefly, listen for its reply advertising a dynamic resume port. A
/// standby-capable holder replies with [`STANDBY_RESUME_REPLY`] carrying the
/// port on which it will accept a later `resume`; we stash it in
/// [`RESUME_TARGET`]. A holder that just shuts down (older, or non-standby)
/// sends no reply — harmless.
fn send_yield_request(rx_port: u16) {
    let Ok(socket) = UdpSocket::bind("127.0.0.1:0") else {
        return;
    };
    let msg = OscMessage {
        addr: runtime_control::osc_contract::CONTROL_YIELD_PORT.to_string(),
        args: vec![],
    };
    if let Ok(bytes) = rosc::encoder::encode(&OscPacket::Message(msg)) {
        let _ = socket.send_to(&bytes, ("127.0.0.1", rx_port));
    }
    // Wait briefly for the standby's resume-port reply (point-to-point).
    let _ = socket.set_read_timeout(Some(Duration::from_millis(500)));
    let mut buf = [0u8; 256];
    if let Ok((len, _)) = socket.recv_from(&mut buf) {
        if let Ok((_, OscPacket::Message(reply))) = rosc::decoder::decode_udp(&buf[..len]) {
            if reply.addr == STANDBY_RESUME_REPLY {
                if let Some(rosc::OscType::Int(port)) = reply.args.first() {
                    if let Ok(port) = u16::try_from(*port) {
                        *RESUME_TARGET.lock().unwrap() = Some(port);
                        log::info!("standby holder will resume on port {port}");
                    }
                }
            }
        }
    }
}

/// Send `/omniphony/control/resume` to the standby port we recorded when we took
/// the RX port over, so the displaced standby re-acquires it. Called as this
/// instance releases the port (mpv quit / `orender_destroy`).
fn send_resume_to_standby() {
    let target = RESUME_TARGET.lock().unwrap().take();
    let Some(port) = target else { return };
    let Ok(socket) = UdpSocket::bind("127.0.0.1:0") else {
        return;
    };
    let msg = OscMessage {
        addr: runtime_control::osc_contract::CONTROL_RESUME.to_string(),
        args: vec![],
    };
    if let Ok(bytes) = rosc::encoder::encode(&OscPacket::Message(msg)) {
        let _ = socket.send_to(&bytes, ("127.0.0.1", port));
        log::info!("resume sent to standby on port {port}");
    }
}

/// Point-to-point reply address: a standby-capable instance answers a
/// `yield_port` with this, carrying the dynamic UDP port (Int) on which it will
/// listen for `/omniphony/control/resume`. Both sides live in this crate.
pub(crate) const STANDBY_RESUME_REPLY: &str = "/omniphony/yield/resume_port";

/// Dynamic resume socket allocated by the yield handler when this instance is
/// asked to stand by. The render loop's standby path takes it to listen for
/// `CONTROL_RESUME` (and it is what frees once we re-acquire the RX port).
static RESUME_SOCKET: Mutex<Option<UdpSocket>> = Mutex::new(None);

/// Allocate the dynamic resume port for a standby handoff and stash its socket.
/// Returns the chosen port so the yield handler can advertise it to the
/// requester (mpv). The socket is non-blocking so the standby listener can poll.
pub(crate) fn prepare_standby_resume_port() -> Option<u16> {
    let sock = UdpSocket::bind("127.0.0.1:0").ok()?;
    sock.set_nonblocking(true).ok()?;
    let port = sock.local_addr().ok()?.port();
    *RESUME_SOCKET.lock().unwrap() = Some(sock);
    Some(port)
}

/// Reservation socket from [`negotiate_rx_port`]: keeps the RX port HELD
/// between the pre-flight negotiation and the real listener bind. Without it
/// the port would sit free during the whole engine build (bridge load, table
/// generation — seconds), long enough for Studio's auto-start watchdog to
/// probe it, spawn a fresh standby, and have that standby steal the
/// live-state sidecar meant for this instance.
static PORT_RESERVATION: Mutex<Option<(u16, UdpSocket)>> = Mutex::new(None);

/// Stop-flag of the OSC listener currently bound in THIS process. A successor
/// engine built while the old one is still alive — mpv switching audio tracks
/// creates the new track's engine before tearing the old one down — signals it
/// to drop the port directly, instead of the multi-second UDP yield wait that a
/// same-process, non-`--osc-yield` holder would just ignore (the cause of the
/// seconds-long stall on track change). External standby holders live in another
/// process and are handled by the UDP yield dance below, untouched.
static LOCAL_RX_RELEASE: Mutex<Option<Arc<AtomicBool>>> = Mutex::new(None);

/// How long to wait for the local listener to notice its stop-flag, exit, and
/// drop its socket (its read timeout is 200 ms). Far below [`YIELD_REBIND_BUDGET`].
const LOCAL_RELEASE_GRACE: Duration = Duration::from_millis(800);

/// Bind the OSC RX socket. On `AddrInUse` with `request_yield`, ask the local
/// holder to yield (honoured only by `--osc-yield` instances) and poll for the
/// port to free up within `budget`. The non-conflict path is a single bind.
/// Releases this process's own port reservation first.
fn bind_rx_socket(
    rx_port: u16,
    request_yield: bool,
    budget: Duration,
) -> std::io::Result<UdpSocket> {
    {
        let mut reservation = PORT_RESERVATION.lock().unwrap();
        if reservation
            .as_ref()
            .is_some_and(|(port, _)| *port == rx_port)
        {
            *reservation = None;
        }
    }
    let first_err = match UdpSocket::bind(("0.0.0.0", rx_port)) {
        Ok(socket) => return Ok(socket),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse && request_yield => e,
        Err(e) => return Err(e),
    };

    // Fast path for a same-process holder (mpv switching tracks: the old track's
    // engine still holds the port). It won't honour the UDP yield (not a
    // `--osc-yield` standby), so signal its listener to drop the port directly —
    // it exits within a poll and frees the port in ~250 ms instead of stalling
    // the new engine (and playback) for the full yield budget.
    let local_release = LOCAL_RX_RELEASE.lock().unwrap().clone();
    if let Some(stop) = local_release {
        stop.store(true, Ordering::Relaxed);
        let deadline = std::time::Instant::now() + LOCAL_RELEASE_GRACE;
        loop {
            std::thread::sleep(YIELD_REBIND_POLL);
            match UdpSocket::bind(("0.0.0.0", rx_port)) {
                Ok(socket) => {
                    log::info!("OSC RX port {} reclaimed from the local listener", rx_port);
                    return Ok(socket);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                    if std::time::Instant::now() >= deadline {
                        break; // fall through to the external-holder yield dance
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    log::info!(
        "OSC RX port {} is busy; asking the holder to yield",
        rx_port
    );
    send_yield_request(rx_port);
    let start = std::time::Instant::now();
    let mut resent = false;
    loop {
        std::thread::sleep(YIELD_REBIND_POLL);
        match UdpSocket::bind(("0.0.0.0", rx_port)) {
            Ok(socket) => {
                log::info!("OSC RX port {} acquired after yield", rx_port);
                return Ok(socket);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                if start.elapsed() >= budget {
                    return Err(first_err);
                }
                // One UDP retry in case the first request was lost.
                if !resent && start.elapsed() >= budget / 2 {
                    resent = true;
                    send_yield_request(rx_port);
                }
            }
            Err(e) => return Err(e),
        }
    }
}

/// Pre-flight port negotiation for hosts that must settle ownership of the RX
/// port *before* loading config (the FFI host consumes the live-state sidecar
/// a yielded instance writes on shutdown). On success the bound socket is kept
/// as a process-wide reservation, released when the real listener (or the
/// degraded reporter) binds via [`bind_rx_socket`] — so the port is never
/// observably free between negotiation and the listener coming up.
pub fn negotiate_rx_port(rx_port: u16) -> bool {
    match bind_rx_socket(rx_port, true, YIELD_REBIND_BUDGET) {
        Ok(socket) => {
            *PORT_RESERVATION.lock().unwrap() = Some((rx_port, socket));
            true
        }
        Err(_) => false,
    }
}

/// Generic description of a single spatial audio object for OSC broadcast.
/// Built by the caller from whatever source format it uses.
#[derive(Clone)]
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
    /// Per-axis object spatial extent (w, d, h), each in [0.0, 1.0].
    /// `[0.0, 0.0, 0.0]` denotes a point source.
    pub size: [f32; 3],
    /// `true` for a fixed channel (its pose comes from the channel plan —
    /// direct or virtualized), `false` for a dynamic object. Explicit per
    /// `docs/channel-object-contract.md` phase 4: clients must not infer
    /// this from `direct_speaker_index` (which stays as position info).
    pub fixed: bool,
    /// Canonical channel-label name for a fixed channel (`"L"`, `"TFL"`…);
    /// empty for dynamic objects.
    pub label: String,
}

/// Epsilon for position/float comparison in delta OSC sending.
const OBJECT_EPSILON: f32 = 1e-6;

/// Snapshot of an object's comparable fields for delta detection.
#[derive(Clone)]
struct ObjectSnapshot {
    name: String,
    fixed: bool,
    label: String,
    x: f32,
    y: f32,
    z: f32,
    coord_mode: String,
    direct_speaker_index: Option<u32>,
    gain: i32,
    priority: f32,
    size: [f32; 3],
}

impl ObjectSnapshot {
    fn from_meta(o: &ObjectMeta) -> Self {
        Self {
            name: o.name.clone(),
            fixed: o.fixed,
            label: o.label.clone(),
            x: o.x,
            y: o.y,
            z: o.z,
            coord_mode: o.coord_mode.clone(),
            direct_speaker_index: o.direct_speaker_index,
            gain: o.gain,
            priority: o.priority,
            size: o.size,
        }
    }

    fn matches_position(&self, o: &ObjectMeta) -> bool {
        self.name == o.name
            && self.gain == o.gain
            && self.coord_mode == o.coord_mode
            && self.direct_speaker_index == o.direct_speaker_index
            && (self.x - o.x).abs() < OBJECT_EPSILON
            && (self.y - o.y).abs() < OBJECT_EPSILON
            && (self.z - o.z).abs() < OBJECT_EPSILON
            && (self.priority - o.priority).abs() < OBJECT_EPSILON
    }

    fn matches_meta(&self, o: &ObjectMeta) -> bool {
        self.fixed == o.fixed && self.label == o.label
    }

    fn matches_size(&self, o: &ObjectMeta) -> bool {
        (self.size[0] - o.size[0]).abs() < OBJECT_EPSILON
            && (self.size[1] - o.size[1]).abs() < OBJECT_EPSILON
            && (self.size[2] - o.size[2]).abs() < OBJECT_EPSILON
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
    /// Optional host-owned control handler (audio output/input). Set by hosts
    /// that bring their own audio layer (the CLI's `host_audio::HostAudio`);
    /// unset for the embedded liborender host so the core stays audio-free.
    /// Receives /control/{audio,input}/* messages the core doesn't handle and
    /// contributes /state/audio + /state/input to the live-state bundle.
    host_handler: Option<Arc<dyn HostControlHandler>>,
    /// Previous frame's object snapshots for delta detection.
    prev_objects: Option<Vec<ObjectSnapshot>>,
    /// Force next send_object_frame call to emit all objects.
    force_full_next: Arc<AtomicBool>,
    /// Monotonic identifier for the current logical content generation.
    content_generation: u64,
    /// Random identifier for THIS producer instance, echoed in every
    /// `/omniphony/heartbeat/ack`. A client that sees this value change knows a
    /// *different* renderer instance now answers on the same RX port (a CLI⇄mpv
    /// swap) even when the link never visibly dropped, and can re-handshake.
    /// Stable for the lifetime of the instance (incl. standby/resume cycles).
    instance_epoch: i32,
    /// Stop flag for the background OSC listener thread.
    listener_stop: Arc<AtomicBool>,
    /// Join handle for the background OSC listener thread.
    listener_thread: Mutex<Option<JoinHandle<()>>>,
    /// RX port the listener last bound, so `resume` re-acquires the same one.
    rx_port: u16,
    /// Stop flag + handle for the standby resume-listener thread (active only
    /// while this instance is standing by, waiting for `resume`).
    standby_stop: Arc<AtomicBool>,
    standby_thread: Mutex<Option<JoinHandle<()>>>,
    /// Whether the OSC RX listener is currently bound and running. Set once the
    /// listener thread is spawned, cleared on `enter_standby` and on a swallowed
    /// bind failure in `start_listener`. Lets the standby loop tell a real resume
    /// (port re-acquired) from one that failed because the port is still held, so
    /// it can re-arm standby instead of running portless (which strands Studio).
    listener_bound: bool,
}

impl OscSender {
    pub fn new(default_target: SocketAddrV4) -> Result<Self> {
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        let clients = Arc::new(OscClientRegistry::new(CLIENT_TIMEOUT));
        clients.insert_permanent(SocketAddr::V4(default_target));
        // Per-instance id: mixes pid and a sub-second timestamp so it differs
        // both across processes (CLI vs the mpv-embedded host) and across
        // successive instances in the same process. Only its *change* matters,
        // not its distribution, so a cheap hash avoids pulling in an RNG crate.
        let instance_epoch = {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0);
            (std::process::id() ^ nanos.rotate_left(13)) as i32
        };
        Ok(Self {
            socket: Arc::new(socket),
            clients,
            control: None,
            host_handler: None,
            prev_objects: None,
            force_full_next: Arc::new(AtomicBool::new(true)),
            content_generation: 0,
            instance_epoch,
            listener_stop: Arc::new(AtomicBool::new(false)),
            listener_thread: Mutex::new(None),
            rx_port: 0,
            standby_stop: Arc::new(AtomicBool::new(false)),
            standby_thread: Mutex::new(None),
            listener_bound: false,
        })
    }

    /// Attach the renderer control object so the OSC listener can read/write live params
    /// and trigger VBAP recomputes.  Must be called **before** `start_listener`.
    pub fn attach_renderer_control(&mut self, control: Arc<RendererControl>) {
        self.control = Some(control);
    }

    /// Attach a host control handler (audio output/input layer). Hosts that
    /// own audio (the CLI) register their `host_audio::HostAudio` here; the
    /// embedded liborender host registers nothing so the core stays audio-free.
    pub fn attach_host_handler(&mut self, handler: Arc<dyn HostControlHandler>) {
        self.host_handler = Some(handler);
    }

    /// Start the OSC registration listener on `rx_port`.
    ///
    /// Clients send `/omniphony/register [i listen_port?]` from their listening socket.
    /// If the optional `Int` arg is present it overrides the source port (useful when
    /// the client's send and receive ports differ).
    /// On registration the client immediately receives the current live-state bundle.
    ///
    /// `request_yield`: on a port conflict, ask the local holder to yield
    /// (honoured only by `--osc-yield` standby instances) and retry. If the
    /// port still can't be bound the engine keeps running without a listener
    /// (loud error, no audio regression) — a port squatter must never cost the
    /// listener spatial audio.
    pub fn start_listener(&mut self, rx_port: u16, request_yield: bool) -> Result<()> {
        self.rx_port = rx_port;
        let socket = Arc::clone(&self.socket);
        let clients = Arc::clone(&self.clients);
        let control = self.control.clone();
        let host_handler = self.host_handler.clone();
        let force_full_next = Arc::clone(&self.force_full_next);
        let instance_epoch = self.instance_epoch;
        let stop = Arc::clone(&self.listener_stop);

        if let Some(handle) = self.listener_thread.lock().unwrap().take() {
            self.listener_stop.store(true, Ordering::Relaxed);
            let _ = handle.join();
            self.listener_stop.store(false, Ordering::Relaxed);
        }

        let rx_socket = match bind_rx_socket(rx_port, request_yield, YIELD_REBIND_BUDGET) {
            Ok(socket) => socket,
            Err(e) => {
                log::error!(
                    "OSC listener: failed to bind port {} ({}); running without OSC control",
                    rx_port,
                    e
                );
                self.listener_bound = false;
                return Ok(());
            }
        };
        let _ = rx_socket.set_read_timeout(Some(Duration::from_millis(200)));
        // Register this listener so a same-process successor (mpv track switch)
        // can reclaim the port instantly instead of timing out the UDP yield.
        *LOCAL_RX_RELEASE.lock().unwrap() = Some(Arc::clone(&stop));
        log::info!("OSC listener ready on port {}", rx_port);

        let handle = std::thread::Builder::new()
            .name("osc-listener".into())
            .spawn(move || {
                let mut realtime_seq = RealtimeSeqState::default();
                // Serialized gain table is cached here and shared with the
                // recompute threads this loop spawns, so it's re-serialized only
                // when the topology actually changes (not per push/heartbeat).
                let gaintable_cache = Arc::new(GaintableCache::new());
                let mut last_log_seq = sys::live_log::records_since(0)
                    .last()
                    .map(|record| record.seq)
                    .unwrap_or(0);
                let mut last_host_state_generation =
                    host_handler.as_ref().map(|h| h.state_generation());
                let mut last_live_state_generation =
                    control.as_ref().map(|c| c.live_state_generation());
                // Overlay display prefs: the same generation-poll pattern. The
                // mpv shim flips them through the FFI toggles, so a client that
                // only ever hears its own OSC pushes would drift.
                let mut last_overlay_generation: Option<u64> = None;

                let mut buf = [0u8; 4096];
                loop {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    flush_pending_logs(&socket, &clients, &mut last_log_seq);
                    if let Some(host) = host_handler.as_ref() {
                        let generation = host.state_generation();
                        if last_host_state_generation != Some(generation) {
                            last_host_state_generation = Some(generation);
                            if let Some(ref ctrl) = control {
                                let state_bytes = build_live_state_bundle(ctrl, Some(host));
                                send_raw_filtered(&socket, &clients, &state_bytes, |_| true);
                            }
                        }
                    }
                    {
                        let generation = crate::overlay::state_generation();
                        if last_overlay_generation != Some(generation) {
                            last_overlay_generation = Some(generation);
                            if let Ok(bytes) =
                                rosc::encoder::encode(&OscPacket::Message(OscMessage {
                                    addr: runtime_control::osc_contract::STATE_OVERLAY.to_string(),
                                    args: vec![rosc::OscType::String(
                                        crate::overlay::display_state_json(),
                                    )],
                                }))
                            {
                                send_raw_filtered(&socket, &clients, &bytes, |_| true);
                            }
                        }
                    }
                    // Re-broadcast when core live state changed asynchronously on the
                    // audio thread (e.g. auto-gain lowering the master gain). Coalesced
                    // to this loop's poll cadence (≤200 ms) so loud passages can't flood.
                    if let Some(ref ctrl) = control {
                        let generation = ctrl.live_state_generation();
                        if last_live_state_generation != Some(generation) {
                            last_live_state_generation = Some(generation);
                            let state_bytes = build_live_state_bundle(ctrl, host_handler.as_ref());
                            send_raw_filtered(&socket, &clients, &state_bytes, |_| true);
                        }
                        // One-shot clip notification carrying the offending speaker
                        // index (set on the audio thread on any detected clip,
                        // regardless of auto-gain). Coalesced to the poll cadence so a
                        // loud passage emits at most one per tick.
                        if let Some(speaker_idx) = ctrl.take_clip_pending() {
                            if let Ok(bytes) =
                                rosc::encoder::encode(&OscPacket::Message(OscMessage {
                                    addr: "/omniphony/state/clip".to_string(),
                                    args: vec![rosc::OscType::Int(speaker_idx as i32)],
                                }))
                            {
                                send_raw_filtered(&socket, &clients, &bytes, |_| true);
                            }
                        }
                    }
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
                                    // Send the current state bundle, including layout and speakers.
                                    if let Some(ref ctrl) = control {
                                        let state_bytes =
                                            build_live_state_bundle(ctrl, host_handler.as_ref());
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
                                    // Echo this instance's epoch so the client can
                                    // detect a producer swap behind the same port.
                                    let reply = OscMessage {
                                        addr: reply_addr.to_string(),
                                        args: vec![rosc::OscType::Int(instance_epoch)],
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
                                            host_handler.as_ref(),
                                            &mut realtime_seq,
                                            &socket,
                                            &clients,
                                            &gaintable_cache,
                                        );
                                    }
                                }

                                // Any other packet (incl. bundles) may be a
                                // head-tracking feed on a user-configured address
                                // (e.g. SensorsOSC `/android/rotationvector`).
                                Ok((_, packet)) => {
                                    if let Some(ref ctrl) = control {
                                        if apply_head_tracking_packet(&packet, ctrl) {
                                            maybe_broadcast_head_pose(ctrl, &socket, &clients);
                                        }
                                    }
                                }
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

        *self.listener_thread.lock().unwrap() = Some(handle);
        self.listener_bound = true;

        Ok(())
    }

    /// Write the unsaved live-state handoff sidecar so a successor instance
    /// (a restarting CLI, or the mpv-embedded renderer taking the RX port over)
    /// picks the changes up. Best-effort and a no-op when there's nothing dirty
    /// or no config path. Callers must invoke this **while the RX port is still
    /// held**: the FFI host settles port ownership before reading the config
    /// (`negotiate_rx_port` → `from_paths` → `load_or_default_with_live`), so a
    /// sidecar written before the port frees is guaranteed to be consumed.
    fn write_live_handoff_sidecar(&self) {
        let Some(control) = self.control.as_ref() else {
            return;
        };
        // Only unsaved changes are worth handing over; a clean state would just
        // make the successor flag a phantom "unsaved" diff.
        if !control.config_dirty.load(Ordering::Relaxed) {
            return;
        }
        let Some(path) = control.config_path.lock().as_ref().cloned() else {
            return;
        };
        let sidecar = renderer::config::live_sidecar_path(&path);
        match runtime_control::persist::save_live_config_to_path(
            control,
            self.host_handler.as_deref(),
            &path,
            &sidecar,
        ) {
            Ok(()) => {
                // A fresh sidecar invalidates any overlay this process consumed
                // earlier (destroy→create cycles of the FFI host re-read it).
                renderer::config::clear_live_overlay_cache();
                log::info!("live state handed off to {}", sidecar.display());
            }
            Err(e) => log::warn!("failed to write live-state sidecar: {e}"),
        }
    }

    /// Enter standby: stop the OSC RX listener (freeing `rx_port` for an
    /// mpv-embedded renderer) and start a tiny resume-watch thread on the
    /// dynamic socket the yield handler advertised. It resumes this instance
    /// when `/omniphony/control/resume` arrives there, or — as a crash safety
    /// net — when `rx_port` becomes bindable again. The process stays alive; the
    /// render loop releases its audio output in parallel.
    pub fn enter_standby(&mut self) {
        // Hand off any unsaved live state to the successor (e.g. the mpv-embedded
        // renderer that just asked us to yield) *before* we release the RX port.
        // The successor's `negotiate_rx_port` waits for the port to free, then
        // `from_paths` reads `config + sidecar` — so writing here, while the port
        // is still ours, guarantees it sees the file. Without this a live import
        // (or any unsaved edit) made in this instance was stranded in memory and
        // mpv came up on the stale on-disk config. Mirrors the `Drop` handoff,
        // except we stay alive and keep our in-memory state for a later resume.
        self.write_live_handoff_sidecar();

        // Release the RX port: stop + join the listener thread.
        self.listener_stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.listener_thread.lock().unwrap().take() {
            let _ = handle.join();
        }
        self.listener_stop.store(false, Ordering::Relaxed);
        self.listener_bound = false;

        let resume_socket = RESUME_SOCKET.lock().unwrap().take();
        let rx_port = self.rx_port;
        let stop = Arc::clone(&self.standby_stop);
        stop.store(false, Ordering::Relaxed);
        let handle = std::thread::Builder::new()
            .name("osc-standby".into())
            .spawn(move || {
                let mut buf = [0u8; 1024];
                let mut last_probe = std::time::Instant::now();
                loop {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    // Primary path: a `resume` on the advertised dynamic port.
                    if let Some(ref sock) = resume_socket {
                        if let Ok((len, _)) = sock.recv_from(&mut buf) {
                            if let Ok((_, OscPacket::Message(msg))) =
                                rosc::decoder::decode_udp(&buf[..len])
                            {
                                if msg.addr == runtime_control::osc_contract::CONTROL_RESUME {
                                    sys::shutdown::request_resume();
                                    return;
                                }
                            }
                        }
                    }
                    // Safety net: if mpv crashed without sending `resume`, the RX
                    // port simply frees up. Probe it occasionally (test-bind,
                    // release immediately so `resume` can re-bind cleanly).
                    if last_probe.elapsed() >= Duration::from_secs(2) {
                        last_probe = std::time::Instant::now();
                        if let Ok(probe) = UdpSocket::bind(("0.0.0.0", rx_port)) {
                            drop(probe);
                            sys::shutdown::request_resume();
                            return;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            })
            .ok();
        *self.standby_thread.lock().unwrap() = handle;
    }

    /// Resume from standby: stop the resume-watch thread and re-acquire the RX
    /// port (re-binding `rx_port`, asking any lingering holder to yield).
    pub fn resume(&mut self) -> Result<()> {
        self.standby_stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.standby_thread.lock().unwrap().take() {
            let _ = handle.join();
        }
        self.standby_stop.store(false, Ordering::Relaxed);
        *RESUME_SOCKET.lock().unwrap() = None;
        let rx_port = self.rx_port;
        self.start_listener(rx_port, true)
    }

    /// Whether the OSC RX listener is currently bound and running. After
    /// [`resume`], `false` means the port could not be re-acquired (still held
    /// by mpv): the caller should re-arm standby rather than run portless.
    pub fn is_listening(&self) -> bool {
        self.listener_bound
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

    pub(crate) fn send_to_diag_clients(&self, bytes: &[u8]) {
        send_raw_filtered(&self.socket, &self.clients, bytes, |client| {
            client.diag_enabled
        });
    }

    pub fn has_osc_clients(&self) -> bool {
        self.clients.is_any_live()
    }

    pub fn has_metering_clients(&self) -> bool {
        self.clients.is_any_metering_live()
    }

    /// Pre-enable (or disable) metering on the permanent default target so that
    /// `--osc-metering` / `render.osc_metering` makes meter bundles flow to the
    /// configured OSC host without requiring a runtime enable message.
    pub fn set_default_metering(&self, enabled: bool) {
        self.clients.set_metering_for_permanent(enabled);
    }

    pub fn has_diag_clients(&self) -> bool {
        self.clients.is_any_diag_live()
    }
}

impl Drop for OscSender {
    fn drop(&mut self) {
        // Are we still the current same-process RX-port registrant? A successor
        // engine (mpv switching audio tracks) overwrites LOCAL_RX_RELEASE with
        // its own stop flag when it reclaims the port in `start_listener`, which
        // runs *before* this (now superseded) engine is dropped. If we no longer
        // match, this drop is a handoff to that successor, not a real release.
        let still_port_owner = LOCAL_RX_RELEASE
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|s| Arc::ptr_eq(s, &self.listener_stop));

        // If we took the RX port over from a standby instance, wake it back up
        // as we release the port (e.g. mpv quitting). Done while the port is
        // still ours, so the standby's re-bind races nothing — but only on a
        // real release. On a same-process track-switch handoff the successor
        // already holds the port, so resuming the external standby now would be
        // premature: it would fail to re-bind, drop out of standby and run
        // portless, stranding Studio on `reconnecting`. Keep RESUME_TARGET so
        // the eventual final release (real mpv quit) delivers the resume.
        if still_port_owner {
            send_resume_to_standby();
        }

        // Graceful-shutdown handoff, done while the RX port is still held so a
        // successor polling for the port is guaranteed to see the sidecar by
        // the time the port frees up. Skipped on reload_config, whose contract
        // is "discard live state and re-read the config".
        let reloading = sys::ShutdownHandle::is_restart_from_config_requested();
        if !reloading {
            self.write_live_handoff_sidecar();
            // Goodbye broadcast: lets clients reconnect to the next instance
            // immediately instead of waiting out their heartbeat timeout.
            let goodbye = OscMessage {
                addr: runtime_control::osc_contract::STATE_SHUTDOWN.to_string(),
                args: vec![rosc::OscType::String("shutdown".to_string())],
            };
            if let Ok(bytes) = rosc::encoder::encode(&OscPacket::Message(goodbye)) {
                send_raw_filtered(&self.socket, &self.clients, &bytes, |_| true);
            }
        }

        self.listener_stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.listener_thread.lock().unwrap().take() {
            let _ = handle.join();
        }
        // Also stop + join the standby resume-watch thread if this engine is
        // dropped while in standby (RX port yielded to an mpv-embedded renderer).
        // Without this it outlives the OscSender, probing `rx_port` after its
        // owner is gone — a live thread leaked per destroy/create cycle.
        self.standby_stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.standby_thread.lock().unwrap().take() {
            let _ = handle.join();
        }
        // Deregister from the same-process release registry only if we are still
        // the entry there (a successor may have already overwritten it — see
        // `still_port_owner` above, computed atomically at the start of drop).
        if still_port_owner {
            *LOCAL_RX_RELEASE.lock().unwrap() = None;
        }
    }
}

/// Numeric OSC args → `f32`, accepting the common scalar types a sensor app may
/// emit (float, double, int).
fn collect_f32(args: &[rosc::OscType]) -> Vec<f32> {
    args.iter()
        .filter_map(|a| match a {
            rosc::OscType::Float(f) => Some(*f),
            rosc::OscType::Double(d) => Some(*d as f32),
            rosc::OscType::Int(i) => Some(*i as f32),
            _ => None,
        })
        .collect()
}

/// Apply a head-tracking packet if its address matches the configured tracking
/// address. Recurses into bundles (sensor apps often batch readings). Reads the
/// config under a short read lock and only takes the write lock on a match.
/// Returns `true` if the pose was updated.
fn apply_head_tracking_packet(packet: &OscPacket, ctrl: &RendererControl) -> bool {
    match packet {
        OscPacket::Message(msg) => {
            let format = {
                let live = ctrl.live.read();
                if !live.binaural.tracking.matches(&msg.addr) {
                    return false;
                }
                live.binaural.tracking.format
            };
            let args = collect_f32(&msg.args);
            if let Some(raw) = format.parse(&args) {
                {
                    let mut live = ctrl.live.write();
                    let current = live.binaural.head_pose;
                    live.binaural.head_pose = live.binaural.tracking.ingest(raw, current);
                }
                // Re-broadcast the live state so connected clients (Studio) see the
                // moving pose. Throttled to ~10 Hz so a 60–100 Hz sensor stream
                // doesn't resend the full state bundle on every packet. (The 3D
                // head view rides the lighter 30 Hz `/state/head_pose` channel —
                // see `maybe_broadcast_head_pose`.)
                static LAST_BUMP_MS: std::sync::atomic::AtomicU64 =
                    std::sync::atomic::AtomicU64::new(0);
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                let last = LAST_BUMP_MS.load(std::sync::atomic::Ordering::Relaxed);
                if now_ms.saturating_sub(last) >= 100 {
                    LAST_BUMP_MS.store(now_ms, std::sync::atomic::Ordering::Relaxed);
                    ctrl.bump_live_state();
                }
                return true;
            }
            false
        }
        OscPacket::Bundle(bundle) => {
            let mut updated = false;
            for inner in &bundle.content {
                updated |= apply_head_tracking_packet(inner, ctrl);
            }
            updated
        }
    }
}

/// Lightweight head-pose channel: a 4-float `/omniphony/state/head_pose`
/// message at ~30 Hz, so the Studio 3D head can follow tracking with low
/// latency without re-sending the full state JSON (which stays at 10 Hz for
/// the text readout).
fn maybe_broadcast_head_pose(
    ctrl: &RendererControl,
    socket: &std::net::UdpSocket,
    clients: &OscClientRegistry,
) {
    static LAST_POSE_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let last = LAST_POSE_MS.load(std::sync::atomic::Ordering::Relaxed);
    if now_ms.saturating_sub(last) < 33 {
        return;
    }
    LAST_POSE_MS.store(now_ms, std::sync::atomic::Ordering::Relaxed);
    let pose = ctrl.live.read().binaural.head_pose;
    transport::broadcast_ffff(
        socket,
        clients,
        "/omniphony/state/head_pose",
        pose.w as f32,
        pose.x as f32,
        pose.y as f32,
        pose.z as f32,
    );
}

#[cfg(test)]
mod yield_tests {
    use super::*;

    /// Serialises the tests that exercise a port-contention path, since they
    /// share the process-global [`LOCAL_RX_RELEASE`] registry and
    /// [`RESUME_TARGET`] slot.
    static SERIAL: Mutex<()> = Mutex::new(());

    /// Grab a free UDP port by binding port 0, then release it.
    fn free_port() -> u16 {
        let s = UdpSocket::bind("127.0.0.1:0").unwrap();
        s.local_addr().unwrap().port()
    }

    /// A same-process holder (the previous track's listener) is reclaimed via the
    /// direct release registry, not the multi-second external yield dance.
    #[test]
    fn local_listener_releases_port_without_yield() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let port = free_port();
        let holder = UdpSocket::bind(("0.0.0.0", port)).unwrap();
        // Simulate the osc-listener thread: drop its socket once the stop-flag is
        // set, freeing the port — like its read-timeout tick does on shutdown.
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let h = std::thread::spawn(move || {
            while !stop_thread.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(20));
            }
            drop(holder);
        });
        *LOCAL_RX_RELEASE.lock().unwrap() = Some(Arc::clone(&stop));

        let socket = bind_rx_socket(port, true, YIELD_REBIND_BUDGET)
            .expect("reclaims the port from the local listener");
        assert_eq!(socket.local_addr().unwrap().port(), port);
        // The local listener was signalled (the fast path ran, not a yield).
        assert!(stop.load(Ordering::Relaxed));

        *LOCAL_RX_RELEASE.lock().unwrap() = None;
        h.join().unwrap();
    }

    /// A standby holder that replies to a `yield_port` with a dynamic resume
    /// port must have that port captured by the taker, which then delivers
    /// `resume` there as it releases the port.
    #[test]
    fn standby_resume_port_is_captured_and_resume_delivered() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let holder = UdpSocket::bind("127.0.0.1:0").unwrap();
        holder
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let rx_port = holder.local_addr().unwrap().port();
        let resume = UdpSocket::bind("127.0.0.1:0").unwrap();
        resume
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let resume_port = resume.local_addr().unwrap().port();

        // Holder side: on yield, advertise the resume port to the sender.
        let h = std::thread::spawn(move || {
            let mut buf = [0u8; 256];
            if let Ok((_, src)) = holder.recv_from(&mut buf) {
                let reply = OscMessage {
                    addr: STANDBY_RESUME_REPLY.to_string(),
                    args: vec![rosc::OscType::Int(resume_port as i32)],
                };
                let bytes = rosc::encoder::encode(&OscPacket::Message(reply)).unwrap();
                let _ = holder.send_to(&bytes, src);
            }
        });

        *RESUME_TARGET.lock().unwrap() = None;
        send_yield_request(rx_port);
        assert_eq!(*RESUME_TARGET.lock().unwrap(), Some(resume_port));

        send_resume_to_standby();
        let mut buf = [0u8; 256];
        let (len, _) = resume.recv_from(&mut buf).expect("resume delivered");
        let (_, pkt) = rosc::decoder::decode_udp(&buf[..len]).unwrap();
        match pkt {
            OscPacket::Message(m) => {
                assert_eq!(m.addr, runtime_control::osc_contract::CONTROL_RESUME)
            }
            _ => panic!("expected a resume message"),
        }
        // Taking it cleared the target.
        assert!(RESUME_TARGET.lock().unwrap().is_none());
        h.join().unwrap();
    }

    /// A *superseded* engine — a same-process successor (mpv switching audio
    /// tracks) has already overwritten [`LOCAL_RX_RELEASE`] with its own stop
    /// flag — must NOT resume the external standby when dropped: the successor
    /// still holds the RX port, so resuming now would make the standby fail to
    /// re-bind and strand Studio. [`RESUME_TARGET`] is preserved for the eventual
    /// real release. Regression guard for the multi-track-then-quit stuck bug.
    #[test]
    fn superseded_drop_preserves_resume_target() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let target: SocketAddrV4 = "127.0.0.1:9000".parse().unwrap();
        let sender = OscSender::new(target).unwrap();

        // A successor reclaimed the port: the registry points at a *different*
        // stop flag than this (now superseded) sender's.
        let successor_stop = Arc::new(AtomicBool::new(false));
        *LOCAL_RX_RELEASE.lock().unwrap() = Some(Arc::clone(&successor_stop));
        *RESUME_TARGET.lock().unwrap() = Some(12345);

        drop(sender);

        assert_eq!(
            *RESUME_TARGET.lock().unwrap(),
            Some(12345),
            "a track-switch handoff must not consume the standby resume target"
        );
        // The superseded drop must not clear the successor's registry entry.
        assert!(LOCAL_RX_RELEASE.lock().unwrap().is_some());

        *LOCAL_RX_RELEASE.lock().unwrap() = None;
        *RESUME_TARGET.lock().unwrap() = None;
    }

    /// The current RX-port owner (no successor took over) DOES resume the standby
    /// on drop — the real port release, e.g. mpv quitting — clearing
    /// [`RESUME_TARGET`] and deregistering its own [`LOCAL_RX_RELEASE`] entry.
    #[test]
    fn owner_drop_resumes_standby_and_clears_registry() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let target: SocketAddrV4 = "127.0.0.1:9000".parse().unwrap();
        let sender = OscSender::new(target).unwrap();

        // This sender is still the current registrant (no handoff happened).
        *LOCAL_RX_RELEASE.lock().unwrap() = Some(Arc::clone(&sender.listener_stop));
        *RESUME_TARGET.lock().unwrap() = Some(23456);

        drop(sender);

        assert!(
            RESUME_TARGET.lock().unwrap().is_none(),
            "a real release fires resume to (and clears) the standby target"
        );
        assert!(
            LOCAL_RX_RELEASE.lock().unwrap().is_none(),
            "the owner deregisters itself on drop"
        );
    }

    #[test]
    fn bind_succeeds_on_free_port() {
        // Take SERIAL like every other test in this module. `free_port` binds
        // port 0, reads the assigned port and drops the socket, so the port is
        // free-but-unclaimed until `bind_rx_socket` takes it. Without the lock
        // a sibling test can win that window and this one fails with
        // EADDRINUSE — observed as a flake in a repeated release run.
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let port = free_port();
        let socket = bind_rx_socket(port, true, Duration::from_millis(200)).expect("free port");
        assert_eq!(socket.local_addr().unwrap().port(), port);
    }

    #[test]
    fn bind_fails_after_budget_when_holder_keeps_port_and_yield_was_sent() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let port = free_port();
        let holder = UdpSocket::bind(("0.0.0.0", port)).unwrap();
        holder
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();

        let err = bind_rx_socket(port, true, Duration::from_millis(200))
            .expect_err("holder never releases the port");
        assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);

        // The holder must have received exactly the yield request.
        let mut buf = [0u8; 256];
        let (len, _) = holder.recv_from(&mut buf).expect("yield datagram");
        let (_, packet) = rosc::decoder::decode_udp(&buf[..len]).expect("valid OSC");
        match packet {
            OscPacket::Message(msg) => {
                assert_eq!(msg.addr, runtime_control::osc_contract::CONTROL_YIELD_PORT)
            }
            other => panic!("expected a message, got {other:?}"),
        }
    }

    #[test]
    fn negotiation_reservation_holds_the_port_until_the_listener_binds() {
        let port = free_port();
        assert!(negotiate_rx_port(port), "free port must negotiate");
        // The reservation keeps the port held: an external bind must fail …
        assert_eq!(
            UdpSocket::bind(("0.0.0.0", port)).unwrap_err().kind(),
            std::io::ErrorKind::AddrInUse
        );
        // … but this process's own listener bind releases it and succeeds.
        let socket = bind_rx_socket(port, false, Duration::from_millis(100))
            .expect("listener bind must reuse the reserved port");
        assert_eq!(socket.local_addr().unwrap().port(), port);
    }

    #[test]
    fn bind_recovers_when_holder_yields() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let port = free_port();
        let holder = UdpSocket::bind(("0.0.0.0", port)).unwrap();
        holder
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        // Holder thread: release the port upon receiving the yield request.
        let t = std::thread::spawn(move || {
            let mut buf = [0u8; 256];
            let _ = holder.recv_from(&mut buf);
            drop(holder);
        });

        let socket =
            bind_rx_socket(port, true, Duration::from_secs(5)).expect("port freed after yield");
        assert_eq!(socket.local_addr().unwrap().port(), port);
        t.join().unwrap();
    }
}
