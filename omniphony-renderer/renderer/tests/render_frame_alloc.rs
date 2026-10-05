//! `render_frame` runs on the audio thread: once warmed up it must not touch
//! the heap, with beds routed directly and objects through the crossover,
//! metered or not — metered being every frame while Studio is connected, the
//! host then handing each frame back through `recycle_frame`. Its own test
//! binary, for the counting allocator.

use bridge_api::RChannelLabel;
use renderer::backend_conformance::{CountingAllocator, count_allocations};
use renderer::live_params::{
    BinauralMode, CrossoverType, LiveEvaluationMode, LiveParams, OutputMode,
    PreferredEvaluationMode, RampMode,
};
use renderer::spatial_renderer::{ChannelRoute, SpatialChannelEvent, SpatialRenderer};
use renderer::spatial_vbap::{DistanceModel, VbapTableMode};
use renderer::speaker_layout::SpeakerLayout;

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

const BEDS: [RChannelLabel; 3] = [RChannelLabel::L, RChannelLabel::R, RChannelLabel::LFE];
const OBJECTS: usize = 6;
const CHANNELS: usize = BEDS.len() + OBJECTS;
const BLOCK: usize = 40;

/// A 7.1.4 renderer whose front speakers are band-limited, so objects go
/// through a 4-band crossover and the unified table.
fn renderer() -> SpatialRenderer {
    let mut layout = SpeakerLayout::preset("7.1.4").unwrap();
    for (speaker, cutoff) in layout.speakers.iter_mut().zip([80.0, 200.0, 500.0]) {
        speaker.freq_low = Some(cutoff);
    }
    let r = SpatialRenderer::new(
        layout,
        48_000,
        6,
        6,
        0.0,
        2.0,
        VbapTableMode::Cartesian {
            x_size: 15,
            y_size: 15,
            z_size: 7,
            z_neg_size: 7,
        },
        false,
        true,
        DistanceModel::Linear,
        false,
        1.0,
        1.0,
        0.0,
        1.0,
        false,
        [1.0, 2.0, 0.5],
        2.0,
        0.5,
        0.0,
        0.0,
        false,
        false,
        false,
        1.0,
        1.0,
        PreferredEvaluationMode::PrecomputedCartesian,
        LiveEvaluationMode::PrecomputedCartesian,
        15,
        15,
        7,
        7,
    )
    .unwrap();
    let routes: Vec<ChannelRoute> = BEDS.iter().map(|&l| ChannelRoute::Direct(l)).collect();
    r.configure_channel_routing(&routes);
    r
}

/// Bed gains and objects on slow circles, a new ramp every block.
fn events(block: usize, out: &mut Vec<SpatialChannelEvent>) {
    out.clear();
    out.extend((0..BEDS.len()).map(|ch| SpatialChannelEvent {
        channel_idx: ch,
        is_bed: true,
        gain_db: Some(-3.0),
        ramp_length: None,
        size: None,
        position: None,
        sample_pos: None,
    }));
    out.extend((0..OBJECTS).map(|obj| {
        let az = (obj as f64 * 53.0 + block as f64 * (0.7 + obj as f64 * 0.4)).to_radians();
        SpatialChannelEvent {
            channel_idx: BEDS.len() + obj,
            is_bed: false,
            gain_db: Some(-6.0),
            ramp_length: Some(BLOCK as u32),
            size: Some([0.0; 3]),
            position: Some([az.sin() * 0.8, az.cos() * 0.8, (obj % 3) as f64 * 0.3]),
            sample_pos: Some(0),
        }
    }));
}

/// Render `blocks` blocks the way a host does — donated sample buffer, every
/// frame handed back — and count the allocations of the last `counted`.
fn count_steady_state(
    configure: impl Fn(&mut LiveParams),
    metered: bool,
    blocks: usize,
    counted: usize,
) -> u64 {
    let mut r = renderer();
    configure(&mut r.renderer_control().live.write());
    let speaker_gains =
        r.renderer_control().live.read().binaural.output_mode == OutputMode::SpeakerArray;
    let pcm: Vec<f32> = (0..BLOCK * CHANNELS)
        .map(|i| ((i as u32).wrapping_mul(2_654_435_761) >> 16) as f32 / 65535.0 - 0.5)
        .collect();
    let mut evs = Vec::with_capacity(CHANNELS);
    let mut buf = Vec::new();
    let mut allocations = 0;
    for block in 0..blocks {
        events(block, &mut evs);
        let ((), n) = count_allocations(|| {
            let frame = r
                .render_frame(&pcm, CHANNELS, &evs, std::mem::take(&mut buf), metered)
                .unwrap();
            // Unmetered, nothing is published; metered, the speaker path
            // publishes each object's gains (the headphone path has none).
            if !metered || speaker_gains {
                assert_eq!(frame.object_gains.is_empty(), !metered);
            }
            if !speaker_gains {
                assert_eq!(
                    frame.samples.len(),
                    BLOCK * 2,
                    "two ears: the headphone path ran"
                );
            }
            buf = r.recycle_frame(frame);
        });
        if block >= blocks - counted {
            allocations += n;
        }
    }
    allocations
}

#[test]
fn a_warmed_up_render_does_not_allocate() {
    let (_, counted) = count_allocations(|| vec![0u8; 16]);
    assert_eq!(counted, 1, "the counting allocator is the global one");

    for crossover in [CrossoverType::Lr4, CrossoverType::Fir] {
        for ramp_mode in [RampMode::Frame, RampMode::Sample, RampMode::Interp] {
            for metered in [false, true] {
                // The FIR bank works in 1024-sample bursts: count well past a
                // few of them.
                let configure = |live: &mut LiveParams| {
                    live.crossover_type = crossover;
                    live.ramp_mode = ramp_mode;
                };
                let allocations = count_steady_state(configure, metered, 400, 200);
                assert_eq!(
                    allocations, 0,
                    "{crossover:?}, {ramp_mode:?}, metered {metered}: {allocations} allocation(s) \
                     in 200 warmed-up blocks"
                );
            }
        }
    }
}

/// The headphone path on the audio thread: per-object HRIRs (`Direct`) and
/// the virtual-room cascade, with the early reflections and the late reverb
/// on, objects moving every block. The HRIR builds a direction change asks
/// for run on worker threads; the count is the render thread's own.
#[test]
fn a_warmed_up_binaural_render_does_not_allocate() {
    for mode in [BinauralMode::Direct, BinauralMode::Cascaded] {
        for metered in [false, true] {
            let configure = |live: &mut LiveParams| {
                live.binaural.output_mode = OutputMode::Binaural;
                live.binaural.mode = mode;
                live.binaural.reflections.enabled = true;
                live.binaural.reverb.enabled = true;
            };
            let allocations = count_steady_state(configure, metered, 400, 200);
            assert_eq!(
                allocations, 0,
                "binaural {mode:?}, metered {metered}: {allocations} allocation(s) in 200 \
                 warmed-up blocks"
            );
        }
    }
}
