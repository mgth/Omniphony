//! The stream telemetry, sent from its own thread rather than the render
//! thread (#670).
//!
//! What the render path tells clients about the audio it renders (object
//! frames, timestamps, meter and timing bundles, the heard position, loudness
//! and live-state refreshes) leaves from the `osc-telemetry` thread. The render
//! thread fills plain values into a slot of a wait-free single-producer ring
//! and moves on: it encodes nothing, takes no client-registry lock and never
//! reaches `send_to`. Encoding, the registry and the socket are this thread's.
//!
//! **Rate cap.** The thread wakes every [`TICK`] and sends what arrived since
//! the previous tick, in arrival order, except that of the object frames and
//! the timestamps only the latest goes out. Those two are the ones the render
//! path emits per block with no rate of their own: a 40-sample access unit
//! renders 1,200 blocks a second, and a channel bed's object frame went out for
//! every one of them, where a client draws at display rate. Both are state a
//! later value supersedes: object poses are delta-encoded against what was
//! last *sent*, so nothing a client holds goes stale, and a content-generation
//! change or a forced full resend rides the frame that supersedes the one it
//! came with. Everything else already has its own cadence (the meter and diag
//! rates a client sets, the heard position's interval) and passes through as
//! it is.
//!
//! **Back-pressure.** The ring never blocks the render thread. When it is full
//! (this thread stalled on a socket), the event is dropped and counted (a later
//! one supersedes it), and the drops are logged here.
//!
//! The buffers the events carry come back over a second pair of rings, so once
//! they are primed the render path refills the object list, and hands the
//! renderer back its meter lists, without allocating.

use std::net::UdpSocket;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use renderer::live_params::RendererControl;
use renderer::metering::MeterSnapshot;
use renderer::spatial_vbap::Gains;
use rosc::{OscMessage, OscPacket, OscType};
use runtime_control::HostControlHandler;
use runtime_control::osc_contract;

use super::client_registry::{OscClientRegistry, OscClientState};
use super::export::build_live_state;
use super::metadata_emit::ObjectDeltas;
use super::{ObjectMeta, WarnLimiter};

/// How often the thread sends: at most one object frame and one timestamp per
/// tick, so their cap is 100 a second.
pub(super) const TICK: Duration = Duration::from_millis(10);

/// After this many ticks without an event (a second), the thread wakes less
/// often: nothing is playing.
const IDLE_TICKS: u32 = 100;
const IDLE_TICK: Duration = Duration::from_millis(50);

/// Events the ring holds. A 40-sample stream pushes about 15 a tick (its
/// object frames, plus meters and heard positions), so this is over 150 ms of
/// a stalled thread before anything is dropped.
const EVENT_CAPACITY: usize = 256;

/// Buffers kept for reuse on the way back: more than a tick's worth of
/// object frames at 1,200 a second, so the render path has one for each.
const SPARE_CAPACITY: usize = 32;

/// The heard position goes out at most this often. A client extrapolates
/// between two of them, so a faster report only costs packets.
pub(super) const HEARD_INTERVAL_MS: u64 = 20;

/// Where a stream message belongs on the timeline: the block it describes, and
/// how many times the timeline started over (a reset), so the first block after
/// a reset is marked even where it starts at a position marked before it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Block {
    pub(super) restarts: u32,
    pub(super) pos: u64,
}

/// One object frame, as the render path describes it.
pub(super) struct ObjectFrame {
    pub(super) block: Block,
    pub(super) sample_pos: u64,
    pub(super) ramp_duration: u32,
    pub(super) coordinate_format: i32,
    /// The content generation the frame belongs to.
    pub(super) generation: u64,
    /// Every object goes out in full, not only the changed ones.
    pub(super) force_full: bool,
    pub(super) objects: Vec<ObjectMeta>,
}

/// The figures a meter bundle carries besides the levels: the timings of the
/// frame, and the output stage's latency and resampling (`None` where the host
/// has no output stage, as in the embedded engine).
#[derive(Clone, Copy, Debug, Default)]
pub struct MeterTimings {
    pub decode_time_ms: Option<f32>,
    pub crossover_time_ms: Option<f32>,
    pub render_time_ms: Option<f32>,
    pub write_time_ms: Option<f32>,
    pub frame_duration_ms: Option<f32>,
    pub latency_instant_ms: Option<f32>,
    pub latency_control_ms: Option<f32>,
    pub latency_smoothed_ms: Option<f32>,
    pub latency_target_ms: Option<f32>,
    pub latency_downstream_ms: Option<f32>,
    pub latency_avail_input_ms: Option<f32>,
    pub latency_output_fifo_ms: Option<f32>,
    pub latency_resampler_pending_ms: Option<f32>,
    pub resample_ratio: Option<f32>,
    pub adaptive_band: Option<&'static str>,
    pub adaptive_state: Option<&'static str>,
    pub drc_gain: Option<f32>,
}

/// The renderer's per-object gain lists, lent to a meter report and returned.
pub(super) type MeterLists = (Vec<(usize, Gains)>, Vec<(usize, Vec<Gains>)>);

/// One meter bundle, as the render path describes it.
pub(super) struct MeterReport {
    pub(super) block: Block,
    pub(super) snapshot: MeterSnapshot,
    pub(super) object_gains: Vec<(usize, Gains)>,
    pub(super) object_band_gains: Vec<(usize, Vec<Gains>)>,
    pub(super) object_test_position: Option<[f32; 3]>,
    pub(super) object_test_level: Option<(f32, f32)>,
    pub(super) timings: MeterTimings,
}

pub(super) enum Event {
    Objects(ObjectFrame),
    Timestamp {
        block: Block,
        sample_pos: u64,
        seconds: f64,
    },
    Heard {
        pos: u64,
        rate: u32,
    },
    Meter(MeterReport),
    Timing {
        block: Block,
        decode_ms: Option<f32>,
        render_ms: Option<f32>,
        write_ms: Option<f32>,
    },
    Diag {
        schema: Option<String>,
        values: Option<String>,
    },
    Loudness {
        enabled: bool,
        source: Option<i8>,
    },
    LiveState {
        control: Arc<RendererControl>,
        host: Option<Arc<dyn HostControlHandler>>,
    },
}

/// The kinds of which a tick sends only the latest: see the module doc.
const COALESCED: usize = 2;

impl Event {
    fn coalesced_kind(&self) -> Option<usize> {
        match self {
            Event::Objects(_) => Some(0),
            Event::Timestamp { .. } => Some(1),
            _ => None,
        }
    }
}

struct Shared {
    stop: AtomicBool,
    /// Events the full ring refused since the thread last reported them.
    dropped: AtomicU64,
}

/// The render path's end: pushes events, keeps the stream state they are
/// stamped with, and takes the returned buffers back.
pub(super) struct Telemetry {
    events: rtrb::Producer<Event>,
    spare_objects: rtrb::Consumer<Vec<ObjectMeta>>,
    spare_meter: rtrb::Consumer<MeterLists>,
    /// Buffers of an event the full ring refused, for the next one.
    held_objects: Option<Vec<ObjectMeta>>,
    held_meter: Option<MeterLists>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    /// The block the stream messages pushed from now on describe.
    pub(super) block: Block,
    /// The content generation stamped on object frames.
    pub(super) generation: u64,
    /// The next object frame goes out in full; cleared once one is queued.
    pub(super) force_full: bool,
    pub(super) heard: HeardThrottle,
}

impl Telemetry {
    pub(super) fn spawn(
        socket: Arc<UdpSocket>,
        clients: Arc<OscClientRegistry>,
        force_full_next: Arc<AtomicBool>,
    ) -> std::io::Result<Self> {
        let (events, events_rx) = rtrb::RingBuffer::new(EVENT_CAPACITY);
        let (spare_objects_tx, spare_objects) = rtrb::RingBuffer::new(SPARE_CAPACITY);
        let (spare_meter_tx, spare_meter) = rtrb::RingBuffer::new(SPARE_CAPACITY);
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
        });
        let worker = Worker {
            events: events_rx,
            spare_objects: spare_objects_tx,
            spare_meter: spare_meter_tx,
            shared: Arc::clone(&shared),
            out: Out {
                socket,
                clients,
                marks: Marks::default(),
            },
            force_full_next,
            objects: ObjectDeltas::default(),
            pending: Vec::with_capacity(EVENT_CAPACITY),
            latest: [None; COALESCED],
            drops: WarnLimiter::default(),
        };
        let thread = std::thread::Builder::new()
            .name("osc-telemetry".into())
            .spawn(move || worker.run())?;
        Ok(Self {
            events,
            spare_objects,
            spare_meter,
            held_objects: None,
            held_meter: None,
            shared,
            thread: Some(thread),
            block: Block::default(),
            generation: 0,
            force_full: true,
            heard: HeardThrottle::new(),
        })
    }

    /// Queue `event`. `false` when the ring is full: the event is dropped and
    /// counted, and the buffers it carries are kept for the next one.
    pub(super) fn push(&mut self, event: Event) -> bool {
        match self.events.push(event) {
            Ok(()) => true,
            Err(rtrb::PushError::Full(event)) => {
                self.shared.dropped.fetch_add(1, Ordering::Relaxed);
                match event {
                    Event::Objects(frame) => self.held_objects = Some(frame.objects),
                    Event::Meter(report) => {
                        self.held_meter = Some((report.object_gains, report.object_band_gains))
                    }
                    _ => {}
                }
                false
            }
        }
    }

    /// An object list to fill, with the strings of an earlier one to reuse.
    pub(super) fn object_list(&mut self) -> Vec<ObjectMeta> {
        self.held_objects
            .take()
            .or_else(|| self.spare_objects.pop().ok())
            .unwrap_or_default()
    }

    /// Meter lists to hand the renderer in place of the ones a report takes.
    pub(super) fn meter_lists(&mut self) -> MeterLists {
        self.held_meter
            .take()
            .or_else(|| self.spare_meter.pop().ok())
            .unwrap_or_default()
    }

    /// Stop the thread once it has sent what was queued. Idempotent.
    pub(super) fn shutdown(&mut self) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        self.shared.stop.store(true, Ordering::Release);
        thread.thread().unpark();
        let _ = thread.join();
    }
}

impl Drop for Telemetry {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// When the heard position is due, on the render path's side: thinning it
/// there keeps it from filling the ring.
pub(super) struct HeardThrottle {
    /// When the last one went out, in ms since `epoch`, plus one so that 0
    /// means never.
    sent_ms: u64,
    epoch: Instant,
}

impl HeardThrottle {
    fn new() -> Self {
        Self {
            sent_ms: 0,
            epoch: Instant::now(),
        }
    }

    pub(super) fn due_now(&mut self) -> bool {
        let now_ms = self.epoch.elapsed().as_millis() as u64;
        self.due(now_ms)
    }

    /// Whether a heard position reported `now_ms` after `epoch` goes out.
    fn due(&mut self, now_ms: u64) -> bool {
        if self.sent_ms != 0 && now_ms + 1 < self.sent_ms + HEARD_INTERVAL_MS {
            return false;
        }
        self.sent_ms = now_ms + 1;
        true
    }
}

/// The block markers, on the thread's side: where a block starts goes out
/// ahead of the first stream message describing it, once a heard position has
/// been published (until then there is nothing to compare a block with, so the
/// stream is exactly what it was). See [`super::playout`].
#[derive(Default)]
pub(super) struct Marks {
    active: bool,
    marked: Option<Block>,
}

impl Marks {
    /// The block a marker must name before the next stream message of
    /// `block`, if that message is the first of it.
    fn to_mark(&mut self, block: Block) -> Option<Block> {
        if !self.active || self.marked == Some(block) {
            return None;
        }
        self.marked = Some(block);
        Some(block)
    }
}

/// The sending end: the socket, the clients, and the block markers.
pub(super) struct Out {
    socket: Arc<UdpSocket>,
    clients: Arc<OscClientRegistry>,
    marks: Marks,
}

impl Out {
    fn send_filtered(&self, bytes: &[u8], predicate: impl Fn(&OscClientState) -> bool) {
        self.clients.send_filtered(&self.socket, bytes, predicate);
    }

    /// To every live client, unmarked.
    pub(super) fn send_all(&self, bytes: &[u8]) {
        self.send_filtered(bytes, |_| true);
    }

    /// A stream message describing `block`, to every live client.
    pub(super) fn send_stream(&mut self, block: Block, bytes: &[u8]) {
        self.mark(block);
        self.send_all(bytes);
    }

    /// A meter or timing bundle describing `block`: stream too, marked like
    /// [`Self::send_stream`], to the metering subscribers.
    fn send_metering(&mut self, block: Block, bytes: &[u8]) {
        self.mark(block);
        self.send_filtered(bytes, |client| client.metering_enabled);
    }

    fn send_diag(&self, bytes: &[u8]) {
        self.send_filtered(bytes, |client| client.diag_enabled);
    }

    fn mark(&mut self, block: Block) {
        let Some(block) = self.marks.to_mark(block) else {
            return;
        };
        if let Some(bytes) = encode(
            osc_contract::PLAYOUT_BLOCK,
            vec![OscType::Long(block.pos.min(i64::MAX as u64) as i64)],
        ) {
            self.send_all(&bytes);
        }
    }

    /// The listener is hearing sample `pos`; the first one turns the block
    /// markers on.
    fn heard(&mut self, pos: u64, rate: u32) {
        self.marks.active = true;
        if let Some(bytes) = encode(
            osc_contract::PLAYOUT_HEARD,
            vec![
                OscType::Long(pos.min(i64::MAX as u64) as i64),
                OscType::Int(rate.min(i32::MAX as u32) as i32),
            ],
        ) {
            self.send_all(&bytes);
        }
    }
}

/// One OSC message, encoded.
pub(super) fn encode(addr: &str, args: Vec<OscType>) -> Option<Vec<u8>> {
    rosc::encoder::encode(&OscPacket::Message(OscMessage {
        addr: addr.to_string(),
        args,
    }))
    .ok()
}

/// The thread's end.
struct Worker {
    events: rtrb::Consumer<Event>,
    spare_objects: rtrb::Producer<Vec<ObjectMeta>>,
    spare_meter: rtrb::Producer<MeterLists>,
    shared: Arc<Shared>,
    out: Out,
    /// Set by the listener when a client registers: it needs every object.
    force_full_next: Arc<AtomicBool>,
    objects: ObjectDeltas,
    /// What arrived since the last tick, in arrival order; a coalesced event
    /// leaves an empty slot behind when a later one supersedes it.
    pending: Vec<Option<Event>>,
    /// Where in `pending` the latest event of each coalesced kind is.
    latest: [Option<usize>; COALESCED],
    drops: WarnLimiter,
}

impl Worker {
    fn run(mut self) {
        let mut idle_ticks = 0u32;
        loop {
            // Read before draining: whatever was queued before the stop is
            // sent.
            let stopping = self.shared.stop.load(Ordering::Acquire);
            let mut received = false;
            while let Ok(event) = self.events.pop() {
                received = true;
                self.absorb(event);
            }
            self.flush();
            self.report_drops();
            self.out.clients.refresh_presence();
            if stopping {
                break;
            }
            idle_ticks = if received {
                0
            } else {
                idle_ticks.saturating_add(1)
            };
            std::thread::park_timeout(if idle_ticks >= IDLE_TICKS {
                IDLE_TICK
            } else {
                TICK
            });
        }
    }

    /// Queue `event` for this tick, in place of the one of its kind it
    /// supersedes, if it is coalesced.
    fn absorb(&mut self, mut event: Event) {
        if let Some(kind) = event.coalesced_kind() {
            let superseded = self.latest[kind].and_then(|i| self.pending[i].take());
            if let (Some(Event::Objects(old)), Event::Objects(new)) = (superseded, &mut event) {
                new.force_full |= old.force_full;
                self.recycle_objects(old.objects);
            }
            self.latest[kind] = Some(self.pending.len());
        }
        self.pending.push(Some(event));
    }

    /// Send what this tick holds, in arrival order.
    fn flush(&mut self) {
        let mut pending = std::mem::take(&mut self.pending);
        for event in pending.drain(..).flatten() {
            self.send(event);
        }
        self.pending = pending;
        self.latest = [None; COALESCED];
    }

    fn send(&mut self, event: Event) {
        match event {
            Event::Objects(frame) => {
                if let Err(e) = self
                    .objects
                    .emit(&mut self.out, &frame, &self.force_full_next)
                {
                    log::warn!("Failed to send OSC object frame: {e}");
                }
                self.recycle_objects(frame.objects);
            }
            Event::Timestamp {
                block,
                sample_pos,
                seconds,
            } => {
                if let Some(bytes) = encode(
                    osc_contract::TIMESTAMP,
                    vec![OscType::Long(sample_pos as i64), OscType::Double(seconds)],
                ) {
                    self.out.send_stream(block, &bytes);
                }
            }
            Event::Heard { pos, rate } => self.out.heard(pos, rate),
            Event::Meter(report) => {
                match super::state_emit::encode_meter_bundle(&report) {
                    Ok(bytes) => self.out.send_metering(report.block, &bytes),
                    Err(e) => log::warn!("Failed to send meter OSC bundle: {e}"),
                }
                self.recycle_meter(report);
            }
            Event::Timing {
                block,
                decode_ms,
                render_ms,
                write_ms,
            } => {
                if let Some(bytes) =
                    super::state_emit::encode_timing_update(decode_ms, render_ms, write_ms)
                {
                    self.out.send_metering(block, &bytes);
                }
            }
            Event::Diag { schema, values } => {
                if let Some(bytes) = super::state_emit::encode_diag_bundle(schema, values) {
                    self.out.send_diag(&bytes);
                }
            }
            Event::Loudness { enabled, source } => {
                let bytes = super::state_emit::encode_loudness_state(enabled, source);
                if let Some(bytes) = bytes {
                    self.out.send_all(&bytes);
                }
            }
            Event::LiveState { control, host } => {
                build_live_state(&control, host.as_ref())
                    .broadcast(&self.out.socket, &self.out.clients);
            }
        }
    }

    fn recycle_objects(&mut self, objects: Vec<ObjectMeta>) {
        // Kept whole: the strings are what the render path reuses.
        let _ = self.spare_objects.push(objects);
    }

    fn recycle_meter(&mut self, report: MeterReport) {
        let _ = self
            .spare_meter
            .push((report.object_gains, report.object_band_gains));
    }

    fn report_drops(&mut self) {
        let dropped = self.shared.dropped.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            self.drops.report(format_args!(
                "OSC telemetry: the queue was full, {dropped} event(s) dropped"
            ));
        }
        self.drops.flush("full-queue report(s)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(pos: u64) -> Block {
        Block { restarts: 0, pos }
    }

    #[test]
    fn nothing_is_marked_before_a_heard_position() {
        let mut m = Marks::default();
        assert_eq!(m.to_mark(block(480)), None);
        m.active = true;
        assert_eq!(m.to_mark(block(480)), Some(block(480)));
    }

    #[test]
    fn a_block_is_marked_once_however_many_messages_describe_it() {
        let mut m = Marks {
            active: true,
            ..Default::default()
        };
        assert_eq!(
            m.to_mark(block(0)),
            Some(block(0)),
            "position 0 is a block too"
        );
        assert_eq!(m.to_mark(block(0)), None);
        assert_eq!(m.to_mark(block(960)), Some(block(960)));
        assert_eq!(m.to_mark(block(960)), None);
    }

    #[test]
    fn a_restarted_timeline_is_marked_where_it_was_before() {
        let mut m = Marks {
            active: true,
            ..Default::default()
        };
        assert!(m.to_mark(block(0)).is_some());
        let again = Block {
            restarts: 1,
            pos: 0,
        };
        assert_eq!(m.to_mark(again), Some(again));
    }

    #[test]
    fn heard_positions_are_thinned_to_the_interval() {
        let mut h = HeardThrottle::new();
        assert!(h.due(100));
        assert!(!h.due(100 + HEARD_INTERVAL_MS - 1));
        assert!(h.due(100 + HEARD_INTERVAL_MS));
        assert!(h.due(1_000));
    }

    #[test]
    fn heard_at_time_zero_is_not_mistaken_for_never() {
        let mut h = HeardThrottle::new();
        assert!(h.due(0));
        assert!(!h.due(1), "0 ms was a real send");
    }

    /// The OSC messages that reach `socket` within `wait`, bundles flattened,
    /// in arrival order.
    fn received(socket: &UdpSocket, wait: Duration) -> Vec<OscMessage> {
        fn flatten(packet: OscPacket, out: &mut Vec<OscMessage>) {
            match packet {
                OscPacket::Message(m) => out.push(m),
                OscPacket::Bundle(b) => b.content.into_iter().for_each(|p| flatten(p, out)),
            }
        }
        let deadline = Instant::now() + wait;
        let mut out = Vec::new();
        let mut buf = vec![0u8; 65_536];
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            socket
                .set_read_timeout(Some(left.max(Duration::from_millis(1))))
                .unwrap();
            let Ok(n) = socket.recv(&mut buf) else { break };
            if let Ok((_, packet)) = rosc::decoder::decode_udp(&buf[..n]) {
                flatten(packet, &mut out);
            }
        }
        out
    }

    /// A sender whose default target is a socket of the test's.
    fn sender_to_test_socket() -> (super::super::OscSender, UdpSocket) {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let std::net::SocketAddr::V4(target) = socket.local_addr().unwrap() else {
            unreachable!("bound to an IPv4 address");
        };
        (super::super::OscSender::new(target).unwrap(), socket)
    }

    fn object_at(x: f32) -> ObjectMeta {
        ObjectMeta {
            name: "Dialog".into(),
            x,
            y: 0.5,
            z: 0.0,
            coord_mode: "cartesian".into(),
            direct_speaker_index: None,
            gain: 0.0,
            priority: 0.0,
            size: [0.0; 3],
            fixed: false,
            label: String::new(),
            kind: Default::default(),
        }
    }

    fn first_float(m: &OscMessage) -> Option<f32> {
        match m.args.first() {
            Some(OscType::Float(v)) => Some(*v),
            _ => None,
        }
    }

    fn fixture_control() -> Arc<RendererControl> {
        crate::renderer_build::build_spatial_renderer(
            &crate::renderer_build::SpatialRendererParams::from_render_config(None),
            renderer::speaker_layout::SpeakerLayout::preset_stereo().expect("preset"),
            48_000,
            bridge_api::RVbapCartesianDefaults {
                x_size: 5,
                y_size: 5,
                z_size: 3,
                allow_negative_z: false,
            },
            bridge_api::RVbapTableMode::Cartesian,
            None,
        )
        .expect("renderer")
        .renderer_control()
    }

    /// Nothing the render path calls on the sender waits for the client
    /// registry or the socket (#670): with the registry held on another
    /// thread, as a slow send or a registering client holds it, every call
    /// returns, and what they queued goes out once it is released.
    #[test]
    fn the_render_path_never_waits_for_the_client_registry() {
        let (mut sender, socket) = sender_to_test_socket();
        sender.set_default_metering(true);
        sender.attach_renderer_control(fixture_control());

        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let holder = {
            let clients = Arc::clone(&sender.clients);
            std::thread::spawn(move || {
                let _held = clients.lock_for_test();
                held_tx.send(()).unwrap();
                // Held until the calls below are done. One that waits for it
                // gets it after the timeout, so the test fails, not hangs.
                done_rx.recv_timeout(Duration::from_secs(5)).is_err()
            })
        };
        held_rx.recv().unwrap();

        sender.render_at(0);
        assert!(sender.has_osc_clients());
        assert!(sender.has_metering_clients());
        assert!(!sender.has_diag_clients());
        sender.send_heard(0, 48_000);
        sender.send_object_frame(0, 0, 0, &[object_at(0.25)]);
        sender.send_timestamp(0, 0.0);
        let mut rendered = renderer::spatial_renderer::RenderedFrame {
            samples: Vec::new(),
            n_channels: 2,
            object_gains: Vec::new(),
            object_band_gains: Vec::new(),
            object_band_sq: Vec::new(),
            object_test_position: None,
            object_test_level: None,
            crossover_time_ms: 0.0,
        };
        let snapshot = MeterSnapshot {
            object_levels: vec![(0, -6.0, -9.0)],
            object_band_levels: Vec::new(),
            speaker_levels: vec![(-6.0, -9.0); 2],
            ear_levels: None,
            master_peak: -6.0,
            master_rms: -9.0,
        };
        sender.send_meter_bundle(snapshot, &mut rendered, MeterTimings::default());
        sender.send_timing_update(None, None, Some(1.0));
        sender.send_diag_bundle(None, Some("{}".into()));
        sender.send_loudness_state();
        sender.send_live_state_bundle();
        sender.bump_content_generation();
        sender.request_full_object_resend();
        sender.rewind_playout();

        let _ = done_tx.send(());
        let timed_out = holder.join().unwrap();
        assert!(!timed_out, "the render path waited for the client registry");

        let addrs: Vec<String> = received(&socket, Duration::from_millis(500))
            .into_iter()
            .map(|m| m.addr)
            .collect();
        for addr in [
            osc_contract::PLAYOUT_HEARD,
            osc_contract::PLAYOUT_BLOCK,
            osc_contract::SPATIAL_FRAME,
            "/omniphony/object/0/xyz",
            osc_contract::TIMESTAMP,
            osc_contract::METER_MASTER,
            osc_contract::STATE_WRITE_TIME_MS,
            osc_contract::STATE_LOUDNESS,
        ] {
            assert!(addrs.iter().any(|a| a == addr), "{addr} not in {addrs:?}");
        }
    }

    /// A burst of object frames goes out as fewer frames, ending on the
    /// burst's last state.
    #[test]
    fn a_burst_of_object_frames_goes_out_as_its_latest_state() {
        let (mut sender, socket) = sender_to_test_socket();
        const BURST: u32 = 200;
        for i in 0..BURST {
            sender.render_at(u64::from(i) * 40);
            let x = (i + 1) as f32 / BURST as f32;
            sender.send_object_frame(u64::from(i) * 40, 0, 0, &[object_at(x)]);
        }
        let messages = received(&socket, Duration::from_millis(300));
        let frames = messages
            .iter()
            .filter(|m| m.addr == osc_contract::SPATIAL_FRAME)
            .count();
        assert!((1..BURST as usize).contains(&frames), "{frames} frames");
        let last_x = messages
            .iter()
            .filter(|m| m.addr == "/omniphony/object/0/xyz")
            .filter_map(first_float)
            .last();
        assert_eq!(last_x, Some(1.0));
    }

    /// A forced resend is not lost when the frame it rode is superseded by a
    /// later one in the same tick: the object goes out again in full even
    /// though nothing about it changed.
    #[test]
    fn a_forced_resend_survives_coalescing() {
        let (mut sender, socket) = sender_to_test_socket();
        sender.send_object_frame(0, 0, 0, &[object_at(0.5)]);
        let first = received(&socket, Duration::from_millis(100));
        assert!(first.iter().any(|m| m.addr == "/omniphony/object/0/xyz"));

        sender.request_full_object_resend();
        sender.send_object_frame(40, 0, 0, &[object_at(0.5)]);
        sender.send_object_frame(80, 0, 0, &[object_at(0.5)]);
        let again = received(&socket, Duration::from_millis(100));
        assert!(
            again.iter().any(|m| m.addr == "/omniphony/object/0/xyz"),
            "{again:?}"
        );
    }

    /// Without one, an unchanged object is not sent again.
    #[test]
    fn an_unchanged_object_is_not_resent() {
        let (mut sender, socket) = sender_to_test_socket();
        sender.send_object_frame(0, 0, 0, &[object_at(0.5)]);
        let _ = received(&socket, Duration::from_millis(100));
        sender.send_object_frame(40, 0, 0, &[object_at(0.5)]);
        let again = received(&socket, Duration::from_millis(100));
        assert!(again.iter().any(|m| m.addr == osc_contract::SPATIAL_FRAME));
        assert!(!again.iter().any(|m| m.addr == "/omniphony/object/0/xyz"));
    }

    /// A full queue drops events instead of blocking the render path.
    #[test]
    fn a_full_queue_drops_instead_of_blocking() {
        let (mut sender, _socket) = sender_to_test_socket();
        let clients = Arc::clone(&sender.clients);
        // The thread stalls on the registry, so the queue fills.
        let held = clients.lock_for_test();
        let started = Instant::now();
        for i in 0..(EVENT_CAPACITY as u64 * 4) {
            sender.send_timestamp(i, 0.0);
        }
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(sender.telemetry.shared.dropped.load(Ordering::Relaxed) > 0);
        drop(held);
    }
}
