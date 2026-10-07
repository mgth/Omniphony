//! The decode thread's queue, with a bridge scripted in-process: how much audio
//! it holds, what a drain hands back per call, and where the bridge's
//! declaration is read after a seek. Needs no bridge library or sample, unlike
//! `decode_thread.rs`.

use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use abi_stable::std_types::{ROption, RSlice, RStr, RString, RVec};
use abi_stable::{prefix_type::PrefixTypeTrait, sabi_trait::prelude::TD_Opaque};
use bridge_api::*;
use orender_engine::bridge_loader::LoadedBridge;
use orender_engine::decode_step::LogLevelSync;
use orender_engine::renderer_build::{SpatialRendererParams, build_spatial_renderer};
use orender_engine::{DecodeThreadMode, Engine, RenderedAudio};
use renderer::live_params::RendererControl;
use renderer::speaker_layout::SpeakerLayout;

/// A packet is `[frames lo, frames hi, decode ms]`: one stereo frame of that
/// many samples at 48 kHz (none for 0), after sleeping that long, so a test
/// can make decoding the slower half.
fn packet(frames: u16, decode_ms: u8) -> [u8; 3] {
    let [lo, hi] = frames.to_le_bytes();
    [lo, hi, decode_ms]
}

/// [`packet`] with its layout (`0`: stereo, `1`: L R C, `3`: an L R bed and
/// two objects, left and right) and, with `restart`,
/// a second frame of it that starts a segment.
fn packet_in(frames: u16, layout: u8, restart: bool) -> [u8; 5] {
    let [lo, hi, ms] = packet(frames, 0);
    [lo, hi, ms, layout, u8::from(restart)]
}

/// [`packet_in`] that the bridge decodes but reports an error for.
fn failing_packet_in(frames: u16, layout: u8) -> [u8; 5] {
    let [lo, hi, ms] = packet(frames, 0);
    [lo, hi, ms, layout, 2]
}

struct ScriptedBridge {
    /// The name of the thread each read of the declaration came from.
    declaration_reads: Arc<Mutex<Vec<String>>>,
    /// Channel labels of the last frame decoded.
    labels: Vec<RChannelLabel>,
}

impl FormatBridge for ScriptedBridge {
    fn push_packet(&mut self, data: RSlice<'_, u8>, _: RInputTransport, _: u8) -> RPushResult {
        use RChannelLabel::{C, L, Object, R};
        std::thread::sleep(Duration::from_millis(u64::from(data[2])));
        let frames = u16::from_le_bytes([data[0], data[1]]) as u32;
        self.labels = match data.get(3) {
            Some(1) => vec![L, R, C],
            Some(3) => vec![L, R, Object, Object],
            _ => vec![L, R],
        };
        // The objects' positions, as an object format's metadata carries
        // them: object 0 hard left, object 1 hard right (ADM cartesian).
        let has_objects = self.labels.contains(&Object);
        let metadata = || -> RVec<RMetadataFrame> {
            if !has_objects {
                return RVec::new();
            }
            let event = |id: u32, x: f64| REvent {
                id,
                sample_pos: 0,
                has_pos: true,
                pos: [x, 1.0, 0.0],
                gain_db: 0,
                size: [0.0; 3],
                ramp_duration: 0,
            };
            RVec::from(vec![RMetadataFrame {
                events: RVec::from(vec![event(0, -1.0), event(1, 1.0)]),
                object_channels: RVec::from(vec![
                    RObjectChannel { id: 0, channel: 2 },
                    RObjectChannel { id: 1, channel: 3 },
                ]),
                channel_gains: RVec::new(),
                name_updates: RVec::new(),
                sample_pos: 0,
                ramp_duration: 0,
            }])
        };
        let restart = data.get(4) == Some(&1);
        let channels = self.labels.len() as u32;
        let frame = |is_new_segment| RDecodedFrame {
            sampling_frequency: 48_000,
            sample_count: frames,
            channel_count: channels,
            // With objects, only the objects carry signal: what comes out is
            // theirs, not the bed's.
            pcm: (0..frames)
                .flat_map(|_| self.labels.iter())
                .map(|&label| {
                    if has_objects && label != Object {
                        0
                    } else {
                        1_000_000
                    }
                })
                .collect(),
            channel_labels: self.labels.iter().copied().collect(),
            metadata: metadata(),
            drc_gain: 1.0,
            drc_ramp_duration: 0,
            dialogue_level: ROption::RNone,
            is_new_segment,
        };
        let mut out = Vec::new();
        if frames > 0 {
            out.push(frame(false));
            if restart {
                out.push(frame(true));
            }
        }
        RPushResult {
            frames: RVec::from(out),
            error_message: RString::from(if data.get(4) == Some(&2) {
                "scripted error"
            } else {
                ""
            }),
            did_reset: false,
        }
    }
    fn reset(&mut self) {}
    fn is_ready(&self) -> bool {
        true
    }
    fn has_objects(&self) -> bool {
        self.labels.contains(&RChannelLabel::Object)
    }
    fn configure(&mut self, _: RStr<'_>, _: RStr<'_>) -> bool {
        true
    }
    fn coordinate_format(&self) -> RCoordinateFormat {
        RCoordinateFormat::Cartesian
    }
    fn vbap_cartesian_defaults(&self) -> RVbapCartesianDefaults {
        RVbapCartesianDefaults {
            x_size: 3,
            y_size: 3,
            z_size: 3,
            z_neg_size: 0,
            allow_negative_z: false,
        }
    }
    fn preferred_vbap_table_mode(&self) -> RVbapTableMode {
        RVbapTableMode::Cartesian
    }
    fn supported_drc_modes(&self) -> RVec<RString> {
        RVec::new()
    }
    fn set_drc_mode(&mut self, _: RStr<'_>) -> bool {
        false
    }
    fn fixed_channel_poses(&self) -> RVec<RChannelPose> {
        let thread = std::thread::current();
        self.declaration_reads
            .lock()
            .unwrap()
            .push(thread.name().unwrap_or_default().to_owned());
        RVec::new()
    }
    fn source_label(&self) -> RString {
        RString::from(format!("{} channels", self.labels.len()))
    }
}

extern "C" fn new_bridge(_: bool) -> FormatBridgeBox {
    FormatBridge_TO::from_value(
        ScriptedBridge {
            declaration_reads: Arc::default(),
            labels: Vec::new(),
        },
        TD_Opaque,
    )
}
extern "C" fn log_sink(_: usize) {}

extern "C" fn source_families() -> RVec<bridge_api::RSourceFamily> {
    RVec::new()
}

/// An engine on a [`ScriptedBridge`] with the decode thread on, and the log of
/// the bridge's declaration reads.
fn engine() -> (Engine, Arc<Mutex<Vec<String>>>) {
    let (mut engine, reads, _) = engine_with_control();
    engine.set_decode_thread(true).unwrap();
    (engine, reads)
}

/// An engine on a [`ScriptedBridge`], decode thread off, with its live
/// parameters' control.
fn engine_with_control() -> (Engine, Arc<Mutex<Vec<String>>>, Arc<RendererControl>) {
    let declaration_reads = Arc::<Mutex<Vec<String>>>::default();
    let bridge = FormatBridge_TO::from_value(
        ScriptedBridge {
            declaration_reads: Arc::clone(&declaration_reads),
            labels: Vec::new(),
        },
        TD_Opaque,
    );
    let renderer = build_spatial_renderer(
        &SpatialRendererParams::from_render_config(None),
        SpeakerLayout::preset_stereo().unwrap(),
        48_000,
        bridge.vbap_cartesian_defaults(),
        bridge.preferred_vbap_table_mode(),
        None,
    )
    .unwrap();
    let control = renderer.renderer_control();
    let lib = BridgeLib {
        new_bridge,
        set_host_log_sink: log_sink,
        source_families,
        probe,
        input_codecs,
    }
    .leak_into_prefix();
    let engine = Engine::new(
        LoadedBridge {
            lib,
            bridge,
            log_level: LogLevelSync::new(),
        },
        renderer,
        48_000,
    );
    (engine, declaration_reads, control)
}

fn frames(engine: &mut Engine, chunks: Vec<RenderedAudio>) -> usize {
    let frames = chunks.iter().map(|c| c.n_frames).sum();
    engine.recycle(chunks);
    frames
}

fn feed<const N: usize>(engine: &mut Engine, packets: impl IntoIterator<Item = [u8; N]>) -> usize {
    packets
        .into_iter()
        .map(|p| {
            let chunks = engine.process_raw(&p).unwrap();
            frames(engine, chunks)
        })
        .sum()
}

/// What each drain call returned, until one returned nothing.
fn drain(engine: &mut Engine) -> Vec<usize> {
    let mut calls = Vec::new();
    loop {
        let chunks = engine.drain().unwrap();
        match frames(engine, chunks) {
            0 => return calls,
            n => calls.push(n),
        }
    }
}

/// Decoding slower than the host feeds fills the queue up to its limit, and
/// the limit is about 30 ms of audio whatever the codec's packet length -
/// never fewer than one packet, never more than 32.
#[test]
fn the_queue_holds_about_30_ms_whatever_the_packet_length() {
    // An E-AC-3 syncframe, a DTS core frame, a TrueHD access unit.
    for (length, held) in [(1536u16, 1usize), (512, 3), (40, 32)] {
        let (mut engine, _) = engine();
        let fed = feed(&mut engine, (0..60).map(|_| packet(length, 5)));
        let drained = drain(&mut engine);
        assert_eq!(
            drained,
            vec![usize::from(length); held],
            "{length}-sample packets: one packet per drain call, {held} of them"
        );
        assert_eq!(
            fed + drained.iter().sum::<usize>(),
            60 * usize::from(length)
        );
    }
}

/// Once packets get longer the limit comes down, and the queue with it: a
/// call past the limit returns one more packet while one is ready.
#[test]
fn the_queue_shrinks_when_packets_get_longer() {
    let (mut engine, _) = engine();
    let short = feed(&mut engine, (0..40).map(|_| packet(40, 5)));
    let long = feed(&mut engine, (0..300).map(|_| packet(1536, 0)));
    let drained = drain(&mut engine);
    assert_eq!(drained, vec![1536], "the queue still holds {drained:?}");
    assert_eq!(
        short + long + drained.iter().sum::<usize>(),
        40 * 40 + 300 * 1536
    );
}

/// A drain call skips packets that decode to nothing, so 0 frames means the
/// queue is empty, not that one packet was: here one is queued ahead of the
/// last packet with audio.
#[test]
fn a_drain_returns_nothing_only_when_nothing_is_left() {
    let (mut engine, _) = engine();
    let fed = feed(
        &mut engine,
        (0..60).flat_map(|_| [packet(0, 5), packet(1536, 5)]),
    );
    let drained = drain(&mut engine);
    assert!(!drained.is_empty());
    assert!(drained.iter().all(|&n| n == 1536), "{drained:?}");
    assert_eq!(fed + drained.iter().sum::<usize>(), 60 * 1536);
    assert!(
        drain(&mut engine).is_empty(),
        "a second drain returns nothing"
    );
}

/// A drain call with too small a buffer keeps that audio for its retry, and
/// input is refused until the retry has collected it - also through the
/// bounded-buffer call, which must not take it for a packet's held audio.
#[test]
fn a_short_drain_buffer_keeps_the_audio_for_its_retry() {
    let (mut engine, _) = engine();
    let fed = feed(&mut engine, (0..10).map(|_| packet(512, 5)));
    assert!(engine.drain_with_capacity(16).unwrap().is_none());
    assert!(engine.process_raw(&packet(512, 0)).is_err());
    assert!(
        engine
            .process_raw_within(&packet(512, 0), usize::MAX)
            .is_err()
    );
    let chunks = engine
        .drain_with_capacity(usize::MAX)
        .unwrap()
        .expect("an unbounded buffer always fits");
    let first = frames(&mut engine, chunks);
    assert_eq!(first, 512, "the held packet comes back whole");
    let rest: usize = drain(&mut engine).iter().sum();
    assert_eq!(fed + first + rest, 10 * 512, "nothing lost, nothing twice");
}

/// After a seek whose first packet decodes to nothing, the declaration still
/// comes with the first frames, read on the decode thread under the decode's
/// lock - not live from the engine's thread, which would wait for a decode.
#[test]
fn after_a_seek_the_declaration_is_read_with_the_packet() {
    let (mut engine, reads) = engine();
    feed(&mut engine, (0..4).map(|_| packet(1536, 0)));
    drain(&mut engine);
    engine.reset();
    feed(
        &mut engine,
        std::iter::once(packet(0, 0)).chain((0..4).map(|_| packet(1536, 0))),
    );
    drain(&mut engine);
    let reads = reads.lock().unwrap();
    assert!(
        reads.len() >= 2,
        "read before and after the seek: {reads:?}"
    );
    assert!(
        reads.iter().all(|t| t == "orender-decode"),
        "read off the decode thread: {reads:?}"
    );
}

/// The bridge's declaration goes with the frames it was read for: after each
/// call, the engine names the format of the last audio it handed back, not
/// the one of the packet the decode thread has reached. A segment start keeps
/// it (the bridge declares it again with that frame), where the engine used to
/// clear it and read the bridge live, ahead of the audio.
#[test]
fn the_declaration_follows_the_audio_handed_back() {
    // Packet i is 40 samples; layouts change every 5 packets, and the first
    // packet of each run has a second frame that starts a segment.
    let layout = |i: usize| ((i / 5) % 2) as u8;
    let label = |layout: u8| {
        if layout == 0 {
            "2 channels"
        } else {
            "3 channels"
        }
    };
    // Where each packet's audio starts: a restart packet renders two frames.
    let packet_at = |sample_pos: u64| {
        (0..)
            .scan(0u64, |pos, p: usize| {
                let start = *pos;
                *pos += if p % 5 == 0 { 80 } else { 40 };
                Some((p, start))
            })
            .take_while(|&(_, start)| start <= sample_pos)
            .last()
            .unwrap()
            .0
    };
    for threaded in [false, true] {
        let (mut engine, _, _) = engine_with_control();
        engine.set_decode_thread(threaded).unwrap();
        let mut rendered = 0;
        let mut check = |engine: &mut Engine, chunks: Vec<RenderedAudio>| {
            if let Some(last) = chunks.last() {
                let p = packet_at(last.sample_pos);
                assert_eq!(
                    engine.source_label(),
                    label(layout(p)),
                    "threaded={threaded}: after packet {p}'s audio"
                );
            }
            rendered += frames(engine, chunks);
        };
        for i in 0..40 {
            let chunks = engine
                .process_raw(&packet_in(40, layout(i), i % 5 == 0))
                .unwrap();
            check(&mut engine, chunks);
        }
        // Whatever the thread still held: checked the same way, so a slow
        // machine that hands most of it back here tests just as much.
        loop {
            let chunks = engine.drain().unwrap();
            if chunks.is_empty() {
                break;
            }
            check(&mut engine, chunks);
        }
        assert_eq!(rendered, 48 * 40, "threaded={threaded}: every frame");
    }
}

/// A packet the bridge reports an error for is refused, but what the bridge
/// declared with it is kept: the frames after it have the same labels, so no
/// new declaration comes with them.
#[test]
fn a_failed_packets_declaration_is_not_lost() {
    for threaded in [false, true] {
        let (mut engine, _, _) = engine_with_control();
        engine.set_decode_thread(threaded).unwrap();
        feed(&mut engine, (0..4).map(|_| packet_in(40, 0, false)));
        drain(&mut engine);
        assert_eq!(engine.source_label(), "2 channels");
        let failed = engine.process_raw(&failing_packet_in(40, 1));
        let failed = match failed {
            Ok(chunks) if threaded => {
                // The thread hands the error back with the packet's result.
                frames(&mut engine, chunks);
                engine.drain().map(|c| frames(&mut engine, c))
            }
            other => other.map(|c| frames(&mut engine, c)),
        };
        assert!(
            failed.is_err(),
            "threaded={threaded}: the error reaches the host"
        );
        feed(&mut engine, (0..4).map(|_| packet_in(40, 1, false)));
        drain(&mut engine);
        assert_eq!(
            engine.source_label(),
            "3 channels",
            "threaded={threaded}: declared with the failed packet"
        );
    }
}

/// Each packet's audio carries the timestamp the host gave that packet, even
/// when it comes out several calls later: the first block of a packet has it,
/// and a packet of `n` samples starts `n` samples after the one before.
#[test]
fn a_packets_audio_carries_its_own_timestamp() {
    let (mut engine, _) = engine();
    let mut stamped = Vec::new();
    let mut collect = |engine: &mut Engine, chunks: Vec<RenderedAudio>| {
        for c in &chunks {
            if let Some(pts) = c.input_pts_us {
                stamped.push((pts, c.sample_pos));
            }
        }
        engine.recycle(chunks);
    };
    for i in 0..60i64 {
        engine.set_input_pts(Some(i * 1000));
        let chunks = engine.process_raw(&packet(40, 1)).unwrap();
        collect(&mut engine, chunks);
    }
    loop {
        let chunks = engine.drain().unwrap();
        if chunks.is_empty() {
            break;
        }
        collect(&mut engine, chunks);
    }
    assert_eq!(stamped.len(), 60, "one stamp per packet");
    for (pts, sample_pos) in stamped {
        assert_eq!(sample_pos, (pts / 1000) as u64 * 40, "packet {pts}");
    }
}

/// With the host leaving it to the live option, the engine starts the thread
/// when the option turns on and, when it turns off with packets in flight,
/// winds the thread down a packet per call and goes back inline — losing no
/// audio and reordering none.
#[test]
fn the_live_option_switches_the_thread_both_ways_mid_stream() {
    let (mut engine, _, control) = engine_with_control();
    engine
        .set_decode_thread_mode(DecodeThreadMode::Live)
        .unwrap();
    assert!(
        !engine.decode_thread(),
        "off until the option says otherwise"
    );

    let mut positions = Vec::new();
    let mut run = |engine: &mut Engine, n: usize| {
        for _ in 0..n {
            let chunks = engine.process_raw(&packet(40, 1)).unwrap();
            positions.extend(chunks.iter().map(|c| (c.sample_pos, c.n_frames)));
            engine.recycle(chunks);
        }
    };
    run(&mut engine, 20);
    assert!(!engine.decode_thread());

    control.live.write().options.decode_thread = true;
    run(&mut engine, 60);
    assert!(engine.decode_thread(), "on at the next packet");

    control.live.write().options.decode_thread = false;
    run(&mut engine, 1);
    assert!(
        engine.decode_thread(),
        "still winding down with packets in flight"
    );
    run(&mut engine, 80);
    assert!(
        !engine.decode_thread(),
        "inline again once the queue emptied"
    );
    assert!(
        engine.drain().unwrap().is_empty(),
        "nothing left on a thread"
    );

    // 161 packets of 40 samples, in order, none missing or repeated.
    let expected: Vec<(u64, usize)> = (0..161).map(|i| (i * 40, 40)).collect();
    assert_eq!(positions, expected);
}

/// The host keeps the last word: forcing the thread off ignores the option.
#[test]
fn a_host_that_forces_the_thread_ignores_the_option() {
    let (mut engine, _, control) = engine_with_control();
    control.live.write().options.decode_thread = true;
    engine
        .set_decode_thread_mode(DecodeThreadMode::Off)
        .unwrap();
    feed(&mut engine, (0..10).map(|_| packet(40, 0)));
    assert!(!engine.decode_thread());
    engine
        .set_decode_thread_mode(DecodeThreadMode::Live)
        .unwrap();
    assert!(
        engine.decode_thread(),
        "live mode picks the option up at once"
    );
}

/// The render path never waits for the live parameters (#670): with a control
/// write held open on another thread, mid-edit as an OSC handler is, packets
/// still decode and render — the engine's DRC and decode-thread sync, the
/// stream's DRC and dialogue gains, the channel stages and the renderer all
/// read them.
#[test]
fn a_control_write_in_progress_does_not_stall_the_render_path() {
    let (mut engine, _, control) = engine_with_control();
    feed(&mut engine, (0..4).map(|_| packet_in(40, 1, false)));

    let (held_tx, held_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let writer = {
        let control = Arc::clone(&control);
        std::thread::spawn(move || {
            let mut live = control.live.write();
            live.master_gain = 0.5;
            held_tx.send(()).unwrap();
            // Held until the render below is done. A render that waits for
            // it gets it after the timeout, so the test fails, not hangs.
            done_rx.recv_timeout(Duration::from_secs(5)).is_err()
        })
    };
    held_rx.recv().unwrap();
    let rendered = feed(&mut engine, (0..20).map(|_| packet_in(40, 1, false)));
    let _ = done_tx.send(());
    let timed_out = writer.join().unwrap();

    assert!(!timed_out, "the render path waited for the live write");
    assert_eq!(rendered, 20 * 40);
    assert_eq!(control.live.read().master_gain, 0.5, "published on release");
}

/// A params generation bumped while the live write guard is still held could
/// be seen with the params from before the write, and the render thread's
/// cache would keep them under the new generation: debug builds refuse it.
#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "while their write guard is held")]
fn a_params_generation_bumped_inside_the_write_guard_is_refused() {
    let (_engine, _, control) = engine_with_control();
    let mut live = control.live.write();
    live.objects.entry(0).or_default().muted = true;
    control.mark_object_params_dirty();
}

/// Every OSC message that reached `socket` within `wait`, bundles flattened,
/// in arrival order.
fn osc_messages(socket: &std::net::UdpSocket, wait: Duration) -> Vec<rosc::OscMessage> {
    fn flatten(packet: rosc::OscPacket, out: &mut Vec<rosc::OscMessage>) {
        match packet {
            rosc::OscPacket::Message(m) => out.push(m),
            rosc::OscPacket::Bundle(b) => b.content.into_iter().for_each(|p| flatten(p, out)),
        }
    }
    let deadline = std::time::Instant::now() + wait;
    let mut out = Vec::new();
    let mut buf = vec![0u8; 65_536];
    while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
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

/// Feed `count` 1536-sample packets slowly enough for the meter cadence to
/// publish, and collect what the OSC target received meanwhile.
fn feed_and_listen(
    engine: &mut Engine,
    socket: &std::net::UdpSocket,
    count: usize,
) -> Vec<rosc::OscMessage> {
    let mut messages = Vec::new();
    for _ in 0..count {
        feed(engine, [packet(1536, 0)]);
        messages.extend(osc_messages(socket, Duration::from_millis(60)));
    }
    messages
}

fn long_arg(m: &rosc::OscMessage, i: usize) -> Option<i64> {
    match m.args.get(i) {
        Some(rosc::OscType::Long(v)) => Some(*v),
        _ => None,
    }
}

/// The engine says where each block starts and where the listener is, once a
/// host has said the latter, and never holds anything back: a client of a host
/// that does not report sees the stream exactly as before.
#[test]
fn heard_us_publishes_the_listener_and_marks_each_block() {
    use runtime_control::osc_contract::{METER_MASTER, PLAYOUT_BLOCK, PLAYOUT_HEARD};

    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let (mut engine, _, _) = engine_with_control();
    engine
        .enable_osc(orender_engine::OscOptions {
            host: "127.0.0.1".into(),
            port_out: socket.local_addr().unwrap().port(),
            port_in: 0,
            metering: true,
        })
        .unwrap();

    let before = feed_and_listen(&mut engine, &socket, 8);
    assert!(
        before.iter().any(|m| m.addr == METER_MASTER),
        "the meters must flow for this test to mean anything"
    );
    assert!(
        !before
            .iter()
            .any(|m| m.addr == PLAYOUT_BLOCK || m.addr == PLAYOUT_HEARD),
        "nothing about playout before a host reports"
    );

    // Half a second in: 24 000 samples at 48 kHz.
    engine.set_heard_us(500_000);
    let after = feed_and_listen(&mut engine, &socket, 8);
    let heard: Vec<_> = after.iter().filter(|m| m.addr == PLAYOUT_HEARD).collect();
    assert_eq!(heard.len(), 1, "{heard:?}");
    assert_eq!(long_arg(heard[0], 0), Some(24_000));
    assert_eq!(heard[0].args.get(1), Some(&rosc::OscType::Int(48_000)));

    // Every meter bundle is preceded by the marker of its block, and the
    // markers name 1536-sample blocks, in order, each once. From the heard
    // message on: the telemetry thread sends what the render path queued in
    // order, so a meter of a block rendered before the report, late past
    // the listening window above on a slow runner, comes first, unmarked.
    let mut last_block = None;
    let mut blocks = Vec::new();
    for m in after.iter().skip_while(|m| m.addr != PLAYOUT_HEARD) {
        if m.addr == PLAYOUT_BLOCK {
            let pos = long_arg(m, 0).unwrap();
            assert_eq!(pos % 1536, 0, "{pos}");
            last_block = Some(pos);
            blocks.push(pos);
        } else if m.addr == METER_MASTER {
            assert!(last_block.is_some(), "a meter bundle before any marker");
        }
    }
    assert!(!blocks.is_empty());
    assert!(blocks.windows(2).all(|w| w[0] < w[1]), "{blocks:?}");
    assert!(blocks[0] >= 8 * 1536, "the timeline runs on: {blocks:?}");

    // A reset starts the timeline again, and its first block is marked even
    // though it starts where an earlier one did. Markers of blocks rendered
    // before the reset can still arrive first, for the same reason as above
    // (block 15 on a macOS runner): they carry on the old timeline, past the
    // last block heard from it, so the new timeline's first marker is the
    // first one at or before that.
    let last_before = *blocks.last().unwrap();
    engine.reset();
    let again = feed_and_listen(&mut engine, &socket, 4);
    let first = again
        .iter()
        .filter(|m| m.addr == PLAYOUT_BLOCK)
        .filter_map(|m| long_arg(m, 0))
        .find(|&pos| pos <= last_before);
    assert!(first.is_some_and(|pos| pos < 4 * 1536), "{first:?}");
}

/// The object path, end to end on the engine: a stream whose frames carry
/// objects is declared and counted as one, its bed is reported as the bed,
/// its objects render and their positions go out over OSC; the same stream
/// turning into plain channels drops the object state — a live fact, not a
/// latched one (docs/channel-object-contract.md).
#[test]
fn an_object_stream_is_rendered_counted_and_broadcast() {
    use runtime_control::osc_contract::SPATIAL_FRAME;
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let (mut engine, _, _) = engine_with_control();
    engine
        .enable_osc(orender_engine::OscOptions {
            host: "127.0.0.1".into(),
            port_out: socket.local_addr().unwrap().port(),
            port_in: 0,
            metering: false,
        })
        .unwrap();

    let mut peak = 0.0f32;
    for _ in 0..8 {
        let chunks = engine.process_raw(&packet_in(480, 3, false)).unwrap();
        for c in &chunks {
            assert!(c.samples.iter().all(|s| s.is_finite()), "non-finite output");
            peak = c.samples.iter().fold(peak, |m, s| m.max(s.abs()));
        }
        engine.recycle(chunks);
    }
    assert!(engine.has_objects(), "the stream carries objects");
    assert_eq!(engine.object_count(), 2);
    assert_eq!(engine.bed_labels(), &[RChannelLabel::L, RChannelLabel::R]);
    assert!(peak > 0.0, "the objects render (the bed is silent)");
    let messages = osc_messages(&socket, Duration::from_millis(300));
    // The frame announces the bed's two channels and the two objects, and the
    // objects' positions follow it: an empty object list would still send
    // the frame header.
    assert!(
        messages
            .iter()
            .any(|m| m.addr == SPATIAL_FRAME && m.args.get(2) == Some(&rosc::OscType::Int(4))),
        "the objects' frame is broadcast with four entries"
    );
    let position = |id: usize| {
        messages
            .iter()
            .rev()
            .find(|m| m.addr == format!("/omniphony/object/{id}/xyz"))
            .map(|m| {
                m.args[..3]
                    .iter()
                    .map(|a| match a {
                        rosc::OscType::Float(v) => *v,
                        other => panic!("position argument {other:?}"),
                    })
                    .collect::<Vec<_>>()
            })
    };
    assert_eq!(position(2), Some(vec![-1.0, 1.0, 0.0]), "{messages:?}");
    assert_eq!(position(3), Some(vec![1.0, 1.0, 0.0]), "{messages:?}");

    // The same stream, now plain stereo: no objects left.
    feed(&mut engine, (0..4).map(|_| packet_in(480, 0, false)));
    assert!(!engine.has_objects(), "has_objects follows the stream");
    assert_eq!(engine.object_count(), 0);
}

/// Claims nothing: these tests hand their bridge every packet themselves.
extern "C" fn probe(
    data: abi_stable::std_types::RSlice<'_, u8>,
    _transport: bridge_api::RInputTransport,
    _data_type: u8,
) -> bridge_api::RProbe {
    bridge_api::RProbe::none(data.len() as u32)
}

extern "C" fn input_codecs() -> abi_stable::std_types::RVec<abi_stable::std_types::RString> {
    abi_stable::std_types::RVec::new()
}
