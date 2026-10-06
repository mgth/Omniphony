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
//! the timestamps describing the same [`WINDOW`] of the render timeline only
//! the latest goes out. Those two are the ones the render path emits per block
//! with no rate of their own: a 40-sample access unit renders 1,200 blocks a
//! second, and a channel bed's object frame went out for every one of them,
//! where a client draws at display rate. The window is one of audio, not of
//! wall-clock time: a host that renders ahead of playback, in bursts, still
//! gets a pose for every window of what will be heard, which a client
//! following the sound ([`super::playout`]) shows when it is heard. Object
//! poses are delta-encoded against what was last *sent*, so nothing a client
//! holds goes stale, and a content-generation change or a forced full resend
//! rides the frame that supersedes the one it came with. Everything else
//! already has its own cadence (the meter and diag rates a client sets, the
//! heard position's interval) and passes through as it is.
//!
//! **Back-pressure.** The ring never blocks the render thread. When it is full
//! (this thread stalled on a socket), a sample a later one replaces (a meter,
//! a timestamp, a heard position) is dropped and counted, and the drops are
//! logged here. What nothing may come to replace is held on the render path's
//! side instead and pushed again, ahead of anything else, once the ring has
//! room: the latest object frame (the poses a client keeps once the stream
//! stops), the loudness (published once a segment) and a live-state refresh.
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
use super::export::broadcast_live_state;
use super::metadata_emit::ObjectDeltas;
use super::{ObjectMeta, WarnLimiter};

/// How often the thread sends.
pub(super) const TICK: Duration = Duration::from_millis(10);

/// Of the object frames and timestamps describing blocks within the same
/// window of this many samples, only the latest goes out: at most 100 a second
/// of audio each at 48 kHz.
pub(super) const WINDOW: u64 = 480;

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

impl Block {
    /// The [`WINDOW`] of the timeline this block starts in.
    fn window(self) -> (u32, u64) {
        (self.restarts, self.pos / WINDOW)
    }
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

// `Meter` is far larger than the other variants, and stays inline: boxing it
// would allocate on every metered block, which the recycled `MeterLists`
// exist to avoid. The ring's slots are allocated once, so the size costs
// memory at start-up and nothing per block.
#[allow(clippy::large_enum_variant)]
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
    /// The loudness is read when it is published, under the publication
    /// lock, not when it is queued.
    Loudness {
        control: Arc<RendererControl>,
    },
    LiveState {
        control: Arc<RendererControl>,
        host: Option<Arc<dyn HostControlHandler>>,
    },
}

/// The kinds of which only the latest of a window goes out: see the module
/// doc.
const COALESCED: usize = 2;

impl Event {
    /// The coalesced kind of this event, and the block it describes.
    fn coalesced(&self) -> Option<(usize, Block)> {
        match self {
            Event::Objects(frame) => Some((0, frame.block)),
            Event::Timestamp { block, .. } => Some((1, *block)),
            _ => None,
        }
    }
}

/// Kinds held back by a full ring rather than dropped: see the module doc.
const HELD: usize = 3;

/// Where a held event of this kind waits, in the order they are pushed again.
fn held_slot(event: &Event) -> Option<usize> {
    match event {
        Event::Objects(_) => Some(0),
        Event::Loudness { .. } => Some(1),
        Event::LiveState { .. } => Some(2),
        _ => None,
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
    /// Events the full ring refused that no later sample replaces, one per
    /// kind ([`held_slot`]), pushed again in that order before anything else.
    held: [Option<Event>; HELD],
    /// Meter lists of a report the full ring refused, for the next one.
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
            held: [None, None, None],
            held_meter: None,
            shared,
            thread: Some(thread),
            block: Block::default(),
            generation: 0,
            force_full: true,
            heard: HeardThrottle::new(),
        })
    }

    /// Queue `event`, after what an earlier full ring held back. `false`
    /// when the ring is full: the event is held if nothing would replace it,
    /// dropped and counted otherwise.
    pub(super) fn push(&mut self, event: Event) -> bool {
        if !self.retry_held() {
            self.refused(event);
            return false;
        }
        match self.events.push(event) {
            Ok(()) => true,
            Err(rtrb::PushError::Full(event)) => {
                self.refused(event);
                false
            }
        }
    }

    /// Push again what a full ring held back. `false` while it is still full.
    /// A check and nothing else when nothing is held, so cheap enough for
    /// every block.
    pub(super) fn retry_held(&mut self) -> bool {
        for i in 0..HELD {
            let Some(event) = self.held[i].take() else {
                continue;
            };
            if let Err(rtrb::PushError::Full(event)) = self.events.push(event) {
                self.held[i] = Some(event);
                return false;
            }
        }
        true
    }

    fn refused(&mut self, mut event: Event) {
        match held_slot(&event) {
            Some(i) => {
                // The latest of a kind supersedes the one held, but a forced
                // resend carries over.
                if let (Some(Event::Objects(old)), Event::Objects(new)) =
                    (self.held[i].take(), &mut event)
                {
                    new.force_full |= old.force_full;
                }
                self.held[i] = Some(event);
            }
            None => {
                self.shared.dropped.fetch_add(1, Ordering::Relaxed);
                if let Event::Meter(report) = event {
                    self.held_meter = Some((report.object_gains, report.object_band_gains));
                }
            }
        }
    }

    /// An object list to fill, with the strings of an earlier one to reuse,
    /// and whether the frame it held was to go out in full.
    ///
    /// A frame a full ring held back comes first: the new frame supersedes it,
    /// so it takes over its list and its flag rather than leaving it to be
    /// freed here while the ring stays full.
    pub(super) fn object_list(&mut self) -> (Vec<ObjectMeta>, bool) {
        if let Some(Event::Objects(held)) = self.held[0].take() {
            return (held.objects, held.force_full);
        }
        (self.spare_objects.pop().unwrap_or_default(), false)
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
        self.retry_held();
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
    fn marker_for(&mut self, block: Block) -> Option<Block> {
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
        let Some(block) = self.marks.marker_for(block) else {
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
    /// supersedes: a coalesced one describing a block in the same window.
    fn absorb(&mut self, mut event: Event) {
        if let Some((kind, block)) = event.coalesced() {
            let pending = &self.pending;
            let superseded = self.latest[kind]
                .filter(|&i| {
                    pending[i]
                        .as_ref()
                        .and_then(Event::coalesced)
                        .is_some_and(|(_, latest)| latest.window() == block.window())
                })
                .and_then(|i| self.pending[i].take());
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
            Event::Loudness { control } => {
                super::transport::publish_state(&self.out.socket, &self.out.clients, || {
                    vec![super::state_emit::loudness_state_message(&control)]
                });
            }
            Event::LiveState { control, host } => {
                broadcast_live_state(&control, host.as_ref(), &self.out.socket, &self.out.clients);
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
        assert_eq!(m.marker_for(block(480)), None);
        m.active = true;
        assert_eq!(m.marker_for(block(480)), Some(block(480)));
    }

    #[test]
    fn a_block_is_marked_once_however_many_messages_describe_it() {
        let mut m = Marks {
            active: true,
            ..Default::default()
        };
        assert_eq!(
            m.marker_for(block(0)),
            Some(block(0)),
            "position 0 is a block too"
        );
        assert_eq!(m.marker_for(block(0)), None);
        assert_eq!(m.marker_for(block(960)), Some(block(960)));
        assert_eq!(m.marker_for(block(960)), None);
    }

    #[test]
    fn a_restarted_timeline_is_marked_where_it_was_before() {
        let mut m = Marks {
            active: true,
            ..Default::default()
        };
        assert!(m.marker_for(block(0)).is_some());
        let again = Block {
            restarts: 1,
            pos: 0,
        };
        assert_eq!(m.marker_for(again), Some(again));
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
        // A burst is hundreds of datagrams at once; the default buffer can
        // overflow while the whole suite runs, and a lost datagram is not
        // what these tests are about. The kernel may grant less.
        let _ = socket2::SockRef::from(&socket).set_recv_buffer_size(4 << 20);
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
            .next_back();
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
        overfill(&mut sender);
        let elapsed = started.elapsed();
        let dropped = sender.telemetry.shared.dropped.load(Ordering::Relaxed);
        // Asserted with the registry released: a panic holding it would
        // poison it for the telemetry thread.
        drop(held);
        assert!(elapsed < Duration::from_secs(1));
        assert!(dropped > 0);
    }

    /// What a client following the sound shows once the listener hears
    /// `heard`, as Studio's playout queue decides it: each stream message
    /// belongs to the block the last marker named, and is applied once the
    /// listener reaches that block. The `x` of object 0's last pose applied.
    fn x_heard_at(messages: &[OscMessage], heard: u64) -> Option<f32> {
        let mut block = None;
        let mut x = None;
        for m in messages {
            if m.addr == osc_contract::PLAYOUT_BLOCK {
                block = match m.args.first() {
                    Some(OscType::Long(pos)) => Some(*pos as u64),
                    _ => None,
                };
            } else if m.addr == "/omniphony/object/0/xyz" && block.is_some_and(|b| b <= heard) {
                x = first_float(m);
            }
        }
        x
    }

    /// A host that renders ahead of playback hands a quarter of a second over
    /// in one burst, faster than a tick: every window of it keeps its pose, and a
    /// client following the sound shows, half-way through, the pose of the
    /// block it hears rather than nothing until the end of the burst.
    #[test]
    fn a_read_ahead_burst_keeps_a_pose_for_every_window_of_audio() {
        let (mut sender, socket) = sender_to_test_socket();
        sender.send_heard(0, 48_000);
        const FRAMES: u64 = 50;
        for i in 0..FRAMES {
            let pos = i * WINDOW;
            sender.render_at(pos);
            sender.send_object_frame(pos, 0, 0, &[object_at(i as f32 / FRAMES as f32)]);
        }
        let messages = received(&socket, Duration::from_millis(300));
        let frames = messages
            .iter()
            .filter(|m| m.addr == osc_contract::SPATIAL_FRAME)
            .count();
        assert_eq!(frames, FRAMES as usize);
        assert_eq!(x_heard_at(&messages, 12_000), Some(0.5));
        assert_eq!(x_heard_at(&messages, 23_520), Some(0.98));
    }

    /// What nothing would come to replace survives a full queue: once the
    /// telemetry thread catches up, the loudness, the live state and the last
    /// object frame go out, with no other message after them to carry them.
    #[test]
    fn a_full_queue_holds_back_what_no_later_message_replaces() {
        let (mut sender, socket) = sender_to_test_socket();
        sender.attach_renderer_control(fixture_control());
        let clients = Arc::clone(&sender.clients);
        let held = clients.lock_for_test();
        overfill(&mut sender);
        sender.send_object_frame(0, 0, 0, &[object_at(0.75)]);
        sender.send_loudness_state();
        sender.send_live_state_bundle();
        let all_held = sender.telemetry.held.iter().all(Option::is_some);
        drop(held);
        assert!(all_held, "the queue was full");
        // The thread catches up on the stalled queue.
        let _ = received(&socket, Duration::from_millis(200));

        sender.render_at(40);
        let addrs: Vec<(String, Option<f32>)> = received(&socket, Duration::from_millis(500))
            .iter()
            .map(|m| (m.addr.clone(), first_float(m)))
            .collect();
        for addr in [
            osc_contract::STATE_LOUDNESS,
            osc_contract::STATE_SNAPSHOT_COMPLETE,
        ] {
            assert!(
                addrs.iter().any(|(a, _)| a == addr),
                "{addr} not in {addrs:?}"
            );
        }
        assert!(
            addrs.contains(&("/omniphony/object/0/xyz".to_string(), Some(0.75))),
            "{addrs:?}"
        );
    }

    /// Fill the ring while the telemetry thread is stalled on the registry.
    /// The thread drains it once before it blocks, whenever its tick comes:
    /// wait for that, then fill it again, which then holds.
    fn overfill(sender: &mut super::super::OscSender) {
        let fill = |sender: &mut super::super::OscSender| {
            for i in 0..(EVENT_CAPACITY as u64 * 2) {
                sender.send_timestamp(i, 0.0);
            }
        };
        fill(sender);
        std::thread::sleep(IDLE_TICK * 2);
        fill(sender);
    }

    #[global_allocator]
    static GLOBAL: renderer::backend_conformance::CountingAllocator =
        renderer::backend_conformance::CountingAllocator;

    /// While the queue stays full, a frame refused after another takes over
    /// the held one's list: the render path neither allocates a new one nor
    /// frees the old.
    #[test]
    fn a_full_queue_refuses_object_frames_without_allocating() {
        let (mut sender, _socket) = sender_to_test_socket();
        let clients = Arc::clone(&sender.clients);
        let held = clients.lock_for_test();
        overfill(&mut sender);
        let objects: Vec<ObjectMeta> = (0..8).map(|i| object_at(i as f32 / 8.0)).collect();
        // The first refused frame is held, in a list of its own.
        sender.send_object_frame(0, 0, 0, &objects);
        let first_held = sender.telemetry.held[0].is_some();

        let ((), allocations) = renderer::backend_conformance::count_allocations(|| {
            for i in 1..=100 {
                sender.send_object_frame(i * 40, 0, 0, &objects);
            }
        });
        let still_held = sender.telemetry.held[0].is_some();
        drop(held);
        assert!(first_held && still_held, "the queue was full");
        assert_eq!(allocations, 0);
    }
}
