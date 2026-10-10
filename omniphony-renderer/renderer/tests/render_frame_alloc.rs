//! `render_frame` runs on the audio thread: once warmed up it must not touch
//! the heap, with beds routed directly and objects through the crossover,
//! metered or not — metered being every frame while Studio is connected, the
//! host then handing each frame back through `recycle_frame`. Its own test
//! binary, for the counting allocator.

use bridge_api::RChannelLabel;
use renderer::backend_conformance::{
    ConformanceOptions, CountingAllocator, ZeroAllocReport, check_zero_alloc, count_allocations,
};
use renderer::live_params::{
    BinauralMode, CrossoverType, LiveEvaluationMode, LiveParams, OutputMode,
    PreferredEvaluationMode, RampMode,
};
use renderer::render_backend::{
    CentralDistribution, GainModel, VbapBackend, VbapSpreadParams, VolumetricBackend,
    VolumetricParams,
};
use renderer::spatial_renderer::{
    ChannelRoute, RendererSpec, SpatialChannelEvent, SpatialRenderer,
};
use renderer::spatial_vbap::{DistanceModel, OutOfHullMode, VbapPanner, VbapTableMode};
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
    renderer_for(SpeakerLayout::preset("7.1.4").unwrap())
}

/// [`renderer`] on `layout`, its first three speakers band-limited.
fn renderer_for(mut layout: SpeakerLayout) -> SpatialRenderer {
    for (speaker, cutoff) in layout.speakers.iter_mut().zip([80.0, 200.0, 500.0]) {
        speaker.freq_low = Some(cutoff);
    }
    let r = SpatialRenderer::new(RendererSpec {
        speaker_layout: layout,
        sample_rate: 48_000,
        az_res_deg: 6,
        el_res_deg: 6,
        spread_resolution: 0.0,
        distance_max: 2.0,
        table_mode: VbapTableMode::Cartesian {
            x_size: 15,
            y_size: 15,
            z_size: 7,
            z_neg_size: 7,
        },
        allow_negative_z: false,
        vbap_position_interpolation: true,
        distance_model: DistanceModel::Linear,
        spread_from_distance: false,
        spread_distance_range: 1.0,
        spread_distance_curve: 1.0,
        spread_min: 0.0,
        spread_max: 1.0,
        log_object_positions: false,
        room_ratio: [1.0, 2.0, 0.5],
        room_ratio_rear: 2.0,
        room_ratio_lower: 0.5,
        room_ratio_center_blend: 0.0,
        master_gain_db: 0.0,
        auto_gain: false,
        use_loudness: false,
        distance_diffuse: false,
        distance_diffuse_threshold: 1.0,
        distance_diffuse_curve: 1.0,
        preferred_evaluation_mode: PreferredEvaluationMode::PrecomputedCartesian,
        initial_evaluation_mode: LiveEvaluationMode::PrecomputedCartesian,
        cartesian_default_x_size: 15,
        cartesian_default_y_size: 15,
        cartesian_default_z_size: 7,
        cartesian_default_z_neg_size: 7,
    })
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
    let r = renderer();
    configure(&mut r.renderer_control().live.write());
    count_steady_state_of(r, metered, blocks, counted)
}

/// [`count_steady_state`] on a renderer already configured.
fn count_steady_state_of(
    mut r: SpatialRenderer,
    metered: bool,
    blocks: usize,
    counted: usize,
) -> u64 {
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
                    live.options.crossover_type = crossover;
                    live.options.ramp_mode = ramp_mode;
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

/// The gain models that compute live, each with the spread its objects pan
/// with: VBAP and the volumetric model built on it pan a spread source as a
/// cloud of directions, on more working memory than a point source.
const REALTIME_MODELS: [(&str, f32); 5] = [
    ("barycenter", 0.0),
    ("vbap", 0.0),
    ("vbap", 0.3),
    ("volumetric", 0.0),
    ("volumetric", 0.3),
];

/// Gains computed live (`Realtime` evaluation) by a model that needs working
/// memory besides the gains it writes: the barycenter solver's arrays, the
/// gain row over VBAP's effective speakers (virtual ones included) with its
/// out-of-hull fold and spread cloud, the volumetric model's central
/// distribution, and the mirror image's gains of the distance-diffuse stage
/// wrapped around each. All are sized for the layout when the stage's bands
/// are built and handed back on every call, so the render thread allocates
/// nothing for them.
#[test]
fn a_warmed_up_realtime_render_does_not_allocate() {
    for (backend, spread) in REALTIME_MODELS {
        for crossover in [CrossoverType::Lr4, CrossoverType::Fir] {
            for ramp_mode in [RampMode::Frame, RampMode::Sample, RampMode::Interp] {
                let configure = |live: &mut LiveParams| {
                    live.options.crossover_type = crossover;
                    live.options.ramp_mode = ramp_mode;
                    live.use_distance_diffuse = true;
                    live.spread_min = spread;
                    live.backend_id = backend.to_string();
                    live.set_evaluation_mode(LiveEvaluationMode::Realtime);
                };
                let r = renderer();
                configure(&mut r.renderer_control().live.write());
                let control = r.renderer_control();
                let topology = control
                    .prepare_topology_rebuild()
                    .expect("rebuild plan")
                    .build_topology()
                    .expect("realtime topology");
                assert_eq!(topology.backend.backend_id(), backend);
                control.publish_topology(topology);
                let allocations = count_steady_state_of(r, false, 400, 200);
                assert_eq!(
                    allocations, 0,
                    "{backend}, spread {spread}, {crossover:?}, {ramp_mode:?}: {allocations} \
                     allocation(s) in 200 warmed-up blocks"
                );
            }
        }
    }
}

/// The same contract on the models themselves, without a renderer around
/// them: `compute_gains` on the scratch the model made allocates nothing,
/// whatever the out-of-hull mode folds or downmixes, on a layout closed by
/// virtual poles as on one with height speakers, for a point source and for
/// a spread one.
#[test]
fn the_vbap_models_compute_gains_without_allocating() {
    let modes = [
        OutOfHullMode::Fade,
        OutOfHullMode::default(),
        OutOfHullMode::VirtualPoles,
    ];
    for preset in ["5.1", "7.1.4"] {
        let layout = SpeakerLayout::preset(preset).unwrap();
        let dirs = layout
            .spatializable_positions_for_room([1.0; 3], 1.0, 1.0, 0.0)
            .0;
        let radii = layout.spatializable_radii_for_room([1.0; 3], 1.0, 1.0, 0.0);
        for mode in modes {
            for negative_z in [false, true] {
                for spread_min in [0.0, 0.3] {
                    let vbap = || {
                        let panner = VbapPanner::new(&dirs, 1, 1, 0.0, mode)
                            .expect("panner")
                            .with_negative_z(negative_z);
                        let spread = VbapSpreadParams {
                            spread_min,
                            ..Default::default()
                        };
                        VbapBackend::new(panner, spread)
                    };
                    let volumetric = |central| {
                        let params = VolumetricParams {
                            central,
                            ..Default::default()
                        };
                        VolumetricBackend::new(vbap(), &dirs, &radii, params).expect("volumetric")
                    };
                    let models: [Box<dyn GainModel>; 3] = [
                        Box::new(vbap()),
                        Box::new(volumetric(CentralDistribution::Antipode)),
                        Box::new(volumetric(CentralDistribution::Uniform)),
                    ];
                    for model in models {
                        let report = check_zero_alloc(&*model, &ConformanceOptions::default());
                        assert!(
                            matches!(report, ZeroAllocReport::Ran { allocations: 0 }),
                            "{} on {preset}, {mode:?}, negative z {negative_z}, spread \
                             {spread_min}: compute_gains allocated",
                            model.backend_id()
                        );
                    }
                }
            }
        }
    }
}

/// Nothing in a block is sized by a fixed speaker count: a dome of 128
/// speakers, from tables and from gains computed live, metered or not, is as
/// free of allocations once warmed up as a 7.1.4.
#[test]
fn a_warmed_up_render_on_128_speakers_does_not_allocate() {
    let dome = || renderer_for(renderer::test_support::dome_layout(128));
    for ramp_mode in [RampMode::Frame, RampMode::Sample, RampMode::Interp] {
        for metered in [false, true] {
            let r = dome();
            let control = r.renderer_control();
            {
                let mut live = control.live.write();
                live.options.crossover_type = CrossoverType::Fir;
                live.options.ramp_mode = ramp_mode;
            }
            let allocations = count_steady_state_of(r, metered, 240, 120);
            assert_eq!(
                allocations, 0,
                "tables, {ramp_mode:?}, metered {metered}: {allocations} allocation(s) in 120 \
                 warmed-up blocks"
            );
        }

        for (backend, spread) in REALTIME_MODELS {
            let r = dome();
            let control = r.renderer_control();
            {
                let mut live = control.live.write();
                live.options.crossover_type = CrossoverType::Lr4;
                live.options.ramp_mode = ramp_mode;
                live.spread_min = spread;
                live.backend_id = backend.to_string();
                live.set_evaluation_mode(LiveEvaluationMode::Realtime);
            }
            let topology = control
                .prepare_topology_rebuild()
                .expect("rebuild plan")
                .build_topology()
                .expect("realtime topology");
            assert_eq!(topology.backend.backend_id(), backend);
            control.publish_topology(topology);
            let allocations = count_steady_state_of(r, false, 240, 120);
            assert_eq!(
                allocations, 0,
                "realtime {backend}, spread {spread}, {ramp_mode:?}: {allocations} \
                 allocation(s) in 120 warmed-up blocks"
            );
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
