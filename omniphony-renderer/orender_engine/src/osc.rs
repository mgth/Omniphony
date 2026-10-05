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
mod decode;
mod dispatch;
mod export;
mod gaintable;
mod metadata_emit;
mod playout;
mod profiles;
mod recompute;
mod state_emit;
mod telemetry;
mod transport;

pub use self::telemetry::MeterTimings;

use self::client_registry::OscClientRegistry;
use self::dispatch::{RealtimeSeqState, handle_control_message};
use self::export::build_live_state;
use self::gaintable::GaintableCache;
use self::transport::{
    broadcast_string, ensure_send_buffer, flush_pending_logs, resolve_register_addr,
    send_buffered_logs_to_client, send_metering_state, send_raw_filtered,
};
use runtime_control::osc_contract;

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
pub(crate) const STANDBY_RESUME_REPLY: &str = osc_contract::YIELD_RESUME_PORT;

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
pub struct ObjectMeta {
    pub name: String,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub coord_mode: String,
    pub direct_speaker_index: Option<u32>,
    /// Gain in dB (integer, -128 = silent).
    pub gain: f32,
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
    /// What this object is, as the generator that made it knows. Explicit for
    /// the same reason as `fixed`: clients used to read it off the name with a
    /// regular expression, so a rename silently reclassified everything.
    pub kind: crate::object_gen::ObjectKind,
}

impl Clone for ObjectMeta {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            coord_mode: self.coord_mode.clone(),
            label: self.label.clone(),
            ..*self
        }
    }

    /// Field by field, so the strings reuse their buffers: the render path
    /// copies each object frame into a list the telemetry thread handed back.
    fn clone_from(&mut self, source: &Self) {
        self.name.clone_from(&source.name);
        self.x = source.x;
        self.y = source.y;
        self.z = source.z;
        self.coord_mode.clone_from(&source.coord_mode);
        self.direct_speaker_index = source.direct_speaker_index;
        self.gain = source.gain;
        self.priority = source.priority;
        self.size = source.size;
        self.fixed = source.fixed;
        self.label.clone_from(&source.label);
        self.kind = source.kind;
    }
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
    gain: f32,
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
    /// Set by the listener when a client registers: the telemetry thread sends
    /// the next object frame in full.
    force_full_next: Arc<AtomicBool>,
    /// The stream telemetry's queue to its thread (#670): what the render path
    /// reports goes out from there, never from the caller.
    telemetry: telemetry::Telemetry,
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
    /// Set by `resume` so the listener it starts adopts the live state the
    /// departing host handed off in the sidecar. Only a resume takes over from
    /// another instance; the first `start_listener` of a process follows the
    /// engine's own startup load, which already consumed any sidecar.
    adopt_live_on_listen: bool,
}

/// Receive buffer of the control listener: larger than any UDP payload, so no
/// datagram is ever truncated on receipt. Control messages run to tens of
/// kilobytes (a backend file of up to 60 000 bytes, a whole-layout JSON).
const RX_DATAGRAM_MAX: usize = 65_536;

/// Rate limit for a warning the listener would otherwise log once per
/// datagram, so a lost control message is visible without a misbehaving
/// sender flooding the log. One occurrence is logged at `warn`; the ones that
/// follow within [`Self::INTERVAL`] are held back (logged at `debug` only) and
/// their count is reported with the next warning, or on its own once the
/// interval is over.
#[derive(Default)]
struct WarnLimiter {
    last: Option<std::time::Instant>,
    held_back: u32,
}

impl WarnLimiter {
    const INTERVAL: Duration = Duration::from_secs(5);

    /// Counts one occurrence at `now`: `Some(n)` when it is to be logged, `n`
    /// being the occurrences held back since the last one that was; `None`
    /// when it is held back itself.
    fn record(&mut self, now: std::time::Instant) -> Option<u32> {
        if self
            .last
            .is_some_and(|last| now.duration_since(last) < Self::INTERVAL)
        {
            self.held_back = self.held_back.saturating_add(1);
            return None;
        }
        self.last = Some(now);
        Some(std::mem::take(&mut self.held_back))
    }

    /// The count still held back once the interval is over, for when no
    /// further occurrence comes to carry it. Returned once.
    fn overdue(&mut self, now: std::time::Instant) -> Option<u32> {
        if self.held_back == 0 {
            return None;
        }
        let last = self.last?;
        (now.duration_since(last) >= Self::INTERVAL).then(|| std::mem::take(&mut self.held_back))
    }

    /// Logs `message` at `warn`, or at `debug` when it is held back.
    fn report(&mut self, message: std::fmt::Arguments<'_>) {
        match self.record(std::time::Instant::now()) {
            Some(0) => log::warn!("{message}"),
            Some(held_back) => {
                log::warn!("{message} ({held_back} more since the last report)")
            }
            None => log::debug!("{message}"),
        }
    }

    /// Reports the occurrences still held back when the interval ended with
    /// none to carry them: the tail of a burst. `what` names them.
    fn flush(&mut self, what: &str) {
        if self.held_back == 0 {
            return;
        }
        if let Some(held_back) = self.overdue(std::time::Instant::now()) {
            log::warn!("OSC: {held_back} more {what} since the last report");
        }
    }
}

impl OscSender {
    pub fn new(default_target: SocketAddrV4) -> Result<Self> {
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        // Every state bundle and every reply leaves through this socket.
        ensure_send_buffer(&socket);
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
        let socket = Arc::new(socket);
        let force_full_next = Arc::new(AtomicBool::new(true));
        let telemetry = telemetry::Telemetry::spawn(
            Arc::clone(&socket),
            Arc::clone(&clients),
            Arc::clone(&force_full_next),
        )?;
        Ok(Self {
            socket,
            clients,
            control: None,
            host_handler: None,
            force_full_next,
            telemetry,
            instance_epoch,
            listener_stop: Arc::new(AtomicBool::new(false)),
            listener_thread: Mutex::new(None),
            rx_port: 0,
            standby_stop: Arc::new(AtomicBool::new(false)),
            standby_thread: Mutex::new(None),
            listener_bound: false,
            adopt_live_on_listen: false,
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

        // Taken only once the bind above succeeded: a resume that could not
        // re-acquire the port re-arms standby, and the next attempt must still
        // adopt the handoff.
        let adopt_live = std::mem::take(&mut self.adopt_live_on_listen);

        let handle = std::thread::Builder::new()
            .name("osc-listener".into())
            .spawn(move || {
                let mut realtime_seq = RealtimeSeqState::default();
                // Serialized gain table is cached here and shared with the
                // recompute threads this loop spawns, so it's re-serialized only
                // when the topology actually changes (not per push/heartbeat).
                let gaintable_cache = Arc::new(GaintableCache::new());
                let mut last_log_seq = live_log::records_since(0)
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

                // Resuming from standby: take over the live state the departing
                // host left in the sidecar. Deliberately *after* the generation
                // snapshots above — the adoption bumps the live-state generation
                // so the poll below sees it move and broadcasts the adopted
                // values. Sampling the generation after the mutation instead
                // would leave Studio showing the state we just replaced.
                if adopt_live {
                    if let Some(ref ctrl) = control {
                        profiles::adopt_handoff_live_state(
                            ctrl,
                            &socket,
                            &clients,
                            &gaintable_cache,
                        );
                    }
                }

                // Large enough for any UDP datagram, allocated once: a
                // truncated control message (a backend file, a layout) would
                // fail to decode and be lost.
                let mut buf = vec![0u8; RX_DATAGRAM_MAX];
                let mut decode_errors = WarnLimiter::default();
                let mut recv_errors = WarnLimiter::default();
                loop {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    decode_errors.flush("undecodable datagram(s) dropped");
                    recv_errors.flush("recv error(s)");
                    flush_pending_logs(&socket, &clients, &mut last_log_seq);
                    if let Some(host) = host_handler.as_ref() {
                        let generation = host.state_generation();
                        if last_host_state_generation != Some(generation) {
                            last_host_state_generation = Some(generation);
                            if let Some(ref ctrl) = control {
                                build_live_state(ctrl, Some(host)).broadcast(&socket, &clients);
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
                            build_live_state(ctrl, host_handler.as_ref())
                                .broadcast(&socket, &clients);
                        }
                        // One-shot clip notification carrying the offending speaker
                        // index (set on the audio thread on any detected clip,
                        // regardless of auto-gain). Coalesced to the poll cadence so a
                        // loud passage emits at most one per tick.
                        if let Some(speaker_idx) = ctrl.take_clip_pending() {
                            if let Ok(bytes) =
                                rosc::encoder::encode(&OscPacket::Message(OscMessage {
                                    addr: osc_contract::STATE_CLIP.to_string(),
                                    args: vec![rosc::OscType::Int(speaker_idx as i32)],
                                }))
                            {
                                send_raw_filtered(&socket, &clients, &bytes, |_| true);
                            }
                        }
                        // A band set the speaker stage's worker could not
                        // build (the previous bands keep rendering), or the
                        // empty string once a later build went through: on
                        // the address a failed topology rebuild reports to,
                        // which is what it is to a client.
                        if let Some(message) = ctrl.take_band_build_error() {
                            broadcast_string(
                                &socket,
                                &clients,
                                osc_contract::STATE_SPEAKERS_RECOMPUTE_ERROR,
                                &message,
                            );
                        }
                    }
                    match rx_socket.recv_from(&mut buf) {
                        Ok((len, src)) => {
                            match decode::decode_datagram(&buf[..len]) {
                                Ok((_, OscPacket::Message(msg)))
                                    if msg.addr == osc_contract::REGISTER =>
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
                                        build_live_state(ctrl, host_handler.as_ref())
                                            .send_to(&socket, client);
                                    }
                                    send_buffered_logs_to_client(&socket, client, 0);
                                    send_metering_state(&socket, client, metering_enabled);
                                }
                                Ok((_, OscPacket::Message(msg)))
                                    if msg.addr == osc_contract::HEARTBEAT =>
                                {
                                    let client = resolve_register_addr(src, &msg.args);
                                    let is_known = clients.heartbeat(client);
                                    let reply_addr = if is_known {
                                        log::trace!("OSC heartbeat/ack → {}", client);
                                        osc_contract::HEARTBEAT_ACK
                                    } else {
                                        osc_contract::HEARTBEAT_UNKNOWN
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
                                Err(e) => decode_errors.report(format_args!(
                                    "OSC: dropped an undecodable {len}-byte datagram from {src}: {e}"
                                )),
                            }
                        }
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) => {}
                        // A failing socket returns at once, every pass: the
                        // same limit as for a misbehaving sender.
                        Err(e) => recv_errors.report(format_args!("OSC recv error: {e}")),
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
                renderer::config::clear_live_overlay_cache(&path);
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
        // The host that just handed the port back wrote its unsaved live state
        // to the sidecar as it went; the listener we are about to start picks
        // it up. Without this we would come back on our pre-yield state and
        // silently discard everything done while the other host held the port.
        self.adopt_live_on_listen = true;
        let rx_port = self.rx_port;
        self.start_listener(rx_port, true)
    }

    /// Whether the OSC RX listener is currently bound and running. After
    /// [`resume`], `false` means the port could not be re-acquired (still held
    /// by mpv): the caller should re-arm standby rather than run portless.
    pub fn is_listening(&self) -> bool {
        self.listener_bound
    }

    /// Whether any client is live. Lock-free, for the render path: the
    /// answer is refreshed on every registry change and every telemetry tick,
    /// so a client that timed out is noticed within one.
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
        // What the render path queued goes out before the goodbye below.
        self.telemetry.shutdown();

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
        // is "discard live state and re-read the config"; kept on a restart
        // that hands the live state over to the next pipeline.
        let reloading = sys::ShutdownHandle::is_restart_from_config_requested();
        if !reloading || sys::ShutdownHandle::is_restart_keeping_live() {
            self.write_live_handoff_sidecar();
        }
        if !reloading {
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

/// Numeric OSC args → `f32`, dropping anything that is not a number.
///
/// Sensor apps pick their own tag for the same reading, so this goes through
/// the shared parser rather than deciding again which tags count.
fn collect_f32(args: &[rosc::OscType]) -> Vec<f32> {
    args.iter()
        .filter_map(|a| runtime_control::osc::parse_f32_arg(Some(a)))
        .collect()
}

/// Apply a head-tracking packet if its address matches the configured tracking
/// address. Recurses into bundles (sensor apps often batch readings). Reads the
/// config from the live params and writes them only on a match.
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
                    live.binaural.head_pose =
                        live.binaural
                            .tracking
                            .ingest(raw, current, std::time::Instant::now());
                }
                // The moving pose rides the dedicated ~30 Hz `/state/head_pose`
                // channel (see `maybe_broadcast_head_pose`); the full live-state
                // bundle only needs to go out when the *stream* (re)starts, so
                // Studio's tracking gate arms from a snapshot that carries a
                // valid pose. Bumping it per packet — even throttled — kept the
                // entire state bundle rebroadcasting for as long as the tracker
                // ran, and every idle client re-applied an unchanged snapshot.
                static LAST_PACKET_MS: std::sync::atomic::AtomicU64 =
                    std::sync::atomic::AtomicU64::new(0);
                const STREAM_RESUME_GAP_MS: u64 = 2_000;
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                let last = LAST_PACKET_MS.swap(now_ms, std::sync::atomic::Ordering::Relaxed);
                if last == 0 || now_ms.saturating_sub(last) >= STREAM_RESUME_GAP_MS {
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
        osc_contract::STATE_HEAD_POSE,
        pose.w as f32,
        pose.x as f32,
        pose.y as f32,
        pose.z as f32,
    );
}

/// Scaffolding shared by the tests, here and in the submodules, that start a
/// real listener or otherwise reach the process-wide port state.
#[cfg(test)]
mod test_support {
    use super::*;

    /// Serialises the tests that exercise a port-contention path or start a
    /// listener, since they share the process-global [`LOCAL_RX_RELEASE`]
    /// registry and [`RESUME_TARGET`] slot.
    pub(super) static SERIAL: Mutex<()> = Mutex::new(());

    /// Grab a free UDP port by binding port 0, then release it.
    pub(super) fn free_port() -> u16 {
        let s = UdpSocket::bind("127.0.0.1:0").unwrap();
        s.local_addr().unwrap().port()
    }

    pub(super) fn test_sender() -> OscSender {
        OscSender::new(SocketAddrV4::new(std::net::Ipv4Addr::LOCALHOST, 1)).unwrap()
    }

    /// A sender driving `control` whose listener is bound, and its port.
    ///
    /// [`free_port`] leaves the port unclaimed until the listener binds it,
    /// and a test that does not hold [`SERIAL`] may be handed it meanwhile.
    /// `start_listener` reports that as "not listening": take another port.
    pub(super) fn listening_sender(control: &Arc<RendererControl>) -> (OscSender, u16) {
        let mut sender = test_sender();
        sender.attach_renderer_control(Arc::clone(control));
        for _ in 0..8 {
            let port = free_port();
            sender.start_listener(port, false).unwrap();
            if sender.is_listening() {
                return (sender, port);
            }
        }
        panic!("no free port for the test listener");
    }
}

#[cfg(test)]
mod warn_limiter_tests {
    use super::*;
    use std::time::Instant;

    const INTERVAL: Duration = WarnLimiter::INTERVAL;

    #[test]
    fn one_warning_per_interval_carries_the_count_held_back() {
        let start = Instant::now();
        let mut limiter = WarnLimiter::default();
        assert_eq!(limiter.record(start), Some(0), "the first one is logged");
        assert_eq!(limiter.record(start + INTERVAL / 4), None);
        assert_eq!(limiter.record(start + INTERVAL / 2), None);
        assert_eq!(
            limiter.record(start + INTERVAL),
            Some(2),
            "the next one past the interval reports the two held back"
        );
        // The interval runs again from that warning, with a fresh count.
        assert_eq!(limiter.record(start + INTERVAL + INTERVAL / 2), None);
        assert_eq!(limiter.record(start + INTERVAL * 2), Some(1));
    }

    #[test]
    fn the_tail_of_a_burst_is_reported_once_the_interval_is_over() {
        let start = Instant::now();
        let mut limiter = WarnLimiter::default();
        assert_eq!(limiter.overdue(start), None, "nothing happened yet");
        assert_eq!(limiter.record(start), Some(0));
        assert_eq!(limiter.record(start + INTERVAL / 4), None);
        assert_eq!(limiter.record(start + INTERVAL / 2), None);
        assert_eq!(
            limiter.overdue(start + INTERVAL / 2),
            None,
            "a later occurrence may still carry the count"
        );
        assert_eq!(limiter.overdue(start + INTERVAL), Some(2));
        assert_eq!(limiter.overdue(start + INTERVAL * 2), None, "reported once");
        // The count went out on its own: the next occurrence has none to carry.
        assert_eq!(limiter.record(start + INTERVAL * 2), Some(0));
    }
}

#[cfg(test)]
mod yield_tests {
    use super::test_support::{SERIAL, free_port, test_sender};
    use super::*;

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

    /// A resume that re-acquires the port must arm the handoff adoption and then
    /// consume the flag, so the listener adopts exactly once. `control` is unset
    /// here, so the adoption itself no-ops — what is pinned is the arming.
    #[test]
    fn resume_arms_then_consumes_the_handoff_adoption() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut sender = test_sender();
        sender.rx_port = free_port();
        assert!(
            !sender.adopt_live_on_listen,
            "nothing to adopt before a resume"
        );

        sender.resume().expect("re-acquires a free port");
        assert!(sender.is_listening());
        assert!(
            !sender.adopt_live_on_listen,
            "the started listener must consume the flag, so a later plain \
             start_listener does not adopt a second time"
        );

        sender.listener_stop.store(true, Ordering::Relaxed);
        if let Some(handle) = sender.listener_thread.lock().unwrap().take() {
            let _ = handle.join();
        }
        *LOCAL_RX_RELEASE.lock().unwrap() = None;
    }

    /// A resume that cannot re-acquire the port re-arms standby and tries again
    /// later. The adoption flag must survive that failure — dropping it there
    /// would strand the departing host's live state in a sidecar nobody reads.
    #[test]
    fn failed_resume_keeps_the_handoff_adoption_armed() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let port = free_port();
        // A squatter that never answers the yield: bind_rx_socket exhausts its
        // budget and start_listener returns without a listener.
        let _squatter = UdpSocket::bind(("0.0.0.0", port)).unwrap();
        *LOCAL_RX_RELEASE.lock().unwrap() = None;

        let mut sender = test_sender();
        sender.rx_port = port;
        sender
            .resume()
            .expect("a busy port is not an error, just no listener");

        assert!(!sender.is_listening(), "the port was held throughout");
        assert!(
            sender.adopt_live_on_listen,
            "the adoption must stay armed for the retry that follows re-arming standby"
        );
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
        // Losing the `free_port` window makes the negotiation ask the local
        // listener to release the port: whichever one a sibling test started.
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
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

#[cfg(test)]
mod send_size_tests {
    use super::export::MAX_STATE_DATAGRAM;
    use super::*;

    /// A datagram of the largest size the live state is split into leaves the
    /// sender's own socket and arrives whole. macOS and the BSDs refuse a UDP
    /// send larger than the socket's send buffer, which starts at 9,216 bytes
    /// there, so this only passes on them when the sender has raised it.
    #[test]
    fn a_maximum_size_state_datagram_leaves_the_sender_socket() {
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let SocketAddr::V4(target) = receiver.local_addr().unwrap() else {
            unreachable!("bound to an IPv4 address");
        };
        let sender = OscSender::new(target).unwrap();

        send_raw_filtered(
            &sender.socket,
            &sender.clients,
            &vec![0x5a; MAX_STATE_DATAGRAM],
            |_| true,
        );

        let mut buf = vec![0u8; 70_000];
        let len = receiver.recv(&mut buf).expect("the datagram arrives");
        assert_eq!(len, MAX_STATE_DATAGRAM);
    }
}
