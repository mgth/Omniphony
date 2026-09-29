//! The decode thread's queue, with a bridge scripted in-process: how much audio
//! it holds, what a drain hands back per call, and where the bridge's
//! declaration is read after a seek. Needs no bridge library or sample, unlike
//! `decode_thread.rs`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use abi_stable::std_types::{ROption, RSlice, RStr, RString, RVec};
use abi_stable::{prefix_type::PrefixTypeTrait, sabi_trait::prelude::TD_Opaque};
use bridge_api::*;
use orender_engine::bridge_loader::LoadedBridge;
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

struct ScriptedBridge {
    /// The name of the thread each read of the declaration came from.
    declaration_reads: Arc<Mutex<Vec<String>>>,
}

impl FormatBridge for ScriptedBridge {
    fn push_packet(&mut self, data: RSlice<'_, u8>, _: RInputTransport, _: u8) -> RPushResult {
        std::thread::sleep(Duration::from_millis(u64::from(data[2])));
        let frames = u16::from_le_bytes([data[0], data[1]]) as u32;
        let frame = (frames > 0).then(|| RDecodedFrame {
            sampling_frequency: 48_000,
            sample_count: frames,
            channel_count: 2,
            pcm: RVec::from(vec![1_000_000; 2 * frames as usize]),
            channel_labels: RVec::from(vec![RChannelLabel::L, RChannelLabel::R]),
            metadata: RVec::new(),
            drc_gain: 1.0,
            drc_ramp_duration: 0,
            dialogue_level: ROption::RNone,
            is_new_segment: false,
        });
        RPushResult {
            frames: frame.into_iter().collect(),
            error_message: RString::new(),
            did_reset: false,
        }
    }
    fn reset(&mut self) {}
    fn is_ready(&self) -> bool {
        true
    }
    fn has_objects(&self) -> bool {
        false
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
}

extern "C" fn new_bridge(_: bool) -> FormatBridgeBox {
    FormatBridge_TO::from_value(
        ScriptedBridge {
            declaration_reads: Arc::default(),
        },
        TD_Opaque,
    )
}
extern "C" fn log_sink(_: usize) {}

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
    }
    .leak_into_prefix();
    let engine = Engine::new(LoadedBridge { lib, bridge }, renderer, 48_000);
    (engine, declaration_reads, control)
}

fn frames(engine: &mut Engine, chunks: Vec<RenderedAudio>) -> usize {
    let frames = chunks.iter().map(|c| c.n_frames).sum();
    engine.recycle(chunks);
    frames
}

fn feed(engine: &mut Engine, packets: impl IntoIterator<Item = [u8; 3]>) -> usize {
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

    control.live.write().decode_thread = true;
    run(&mut engine, 60);
    assert!(engine.decode_thread(), "on at the next packet");

    control.live.write().decode_thread = false;
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
    control.live.write().decode_thread = true;
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
