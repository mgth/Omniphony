//! Deterministic scene generation, shared by the null tests, the criterion
//! benches, and the future worst-case-block-time gate.
//!
//! Everything here is a pure function of its arguments and a fixed seed: the
//! same call sequence produces byte-identical PCM and event streams on every
//! machine. That is what makes committed goldens meaningful.
//!
//! Moved here from `renderer/benches/render_frame.rs` so the benches and the
//! validation tests cannot drift apart.

use renderer::live_params::{LiveEvaluationMode, PreferredEvaluationMode};
// Re-exported: tests living *inside* `renderer` see a second instantiation of
// that crate (the `--test` build), whose `RampMode` is a distinct type from the
// rlib one these fixtures are compiled against. Callers must name the type
// through this crate or the argument types will not match.
pub use renderer::live_params::RampMode;
use renderer::spatial_renderer::{SpatialChannelEvent, SpatialRenderer};
use renderer::spatial_vbap::{DistanceModel, VbapTableMode};
use renderer::speaker_layout::SpeakerLayout;

/// Samples per access unit fed to `render_frame`. Measured from a real TrueHD
/// Atmos stream through the engine (`ORENDER_PERF_LOG`): the bridge emits a
/// constant 40-sample block at 48 kHz, so this matches the live per-call cost.
pub const BLOCK_SAMPLES: usize = 40;
pub const SAMPLE_RATE: u32 = 48_000;

/// Build a renderer with defaults matching the live decode path for `preset`.
/// `cartesian` selects the precomputed cartesian table/evaluator (vs polar).
pub fn make_renderer(
    preset: &str,
    position_interpolation: bool,
    cartesian: bool,
) -> SpatialRenderer {
    build_renderer(
        SpeakerLayout::preset(preset).expect("known preset"),
        position_interpolation,
        cartesian,
    )
}

/// A "mixed speaker sizes" layout: a few speakers are band-limited (finite
/// `freq_low`), which makes `compute_bands` split rendering into several
/// frequency bands. Every band shares the same VBAP grid, so the per-band table
/// lookups localise the same cell — the case the crossover concept targets.
pub fn crossover_layout() -> SpeakerLayout {
    let mut layout = SpeakerLayout::preset("7.1.4").expect("known preset");
    // Band-limit the first three speakers at distinct cutoffs → edges {80,200,500}
    // → 4 bands; the remaining full-range speakers populate every band.
    for (sp, cutoff) in layout.speakers.iter_mut().zip([80.0, 200.0, 500.0]) {
        sp.freq_low = Some(cutoff);
    }
    layout
}

/// A renderer for **binaural-only** scenes, built with a coarse VBAP table.
///
/// `render_frame` takes the independent binaural branch before touching the
/// VBAP/crossover chain, so the precomputed panning tables are dead weight
/// here — and at the 1°×1° resolution [`build_renderer`] uses they cost
/// ~730 ms per construction, which dominated the ITD tests entirely. Coarsening
/// them changes nothing the binaural path can observe.
pub fn build_renderer_binaural(
    layout: SpeakerLayout,
    position_interpolation: bool,
    cartesian: bool,
) -> SpatialRenderer {
    let (table_mode, preferred, initial) = if cartesian {
        (
            VbapTableMode::Cartesian {
                x_size: 31,
                y_size: 31,
                z_size: 15,
                z_neg_size: 15,
            },
            PreferredEvaluationMode::PrecomputedCartesian,
            LiveEvaluationMode::PrecomputedCartesian,
        )
    } else {
        (
            VbapTableMode::Polar,
            PreferredEvaluationMode::PrecomputedPolar,
            LiveEvaluationMode::PrecomputedPolar,
        )
    };
    SpatialRenderer::new(
        layout,
        SAMPLE_RATE,
        15, // az_res_deg — coarse: the binaural path never reads the VBAP table
        15, // el_res_deg
        0.0,
        2.0,
        table_mode,
        false, // allow_negative_z
        position_interpolation,
        DistanceModel::Linear,
        false,
        1.0,
        1.0,
        0.0,
        1.0,
        false,           // log_object_positions
        [1.0, 2.0, 0.5], // room_ratio
        2.0,
        0.5,
        0.0,
        0.0,   // master_gain_db
        false, // auto_gain
        false, // use_loudness
        false, // distance_diffuse
        1.0,
        1.0,
        preferred,
        initial,
        31,
        31,
        15,
        15,
    )
    .expect("renderer build")
}

pub fn build_renderer(
    layout: SpeakerLayout,
    position_interpolation: bool,
    cartesian: bool,
) -> SpatialRenderer {
    let (table_mode, preferred, initial) = if cartesian {
        (
            VbapTableMode::Cartesian {
                x_size: 31,
                y_size: 31,
                z_size: 15,
                z_neg_size: 15,
            },
            PreferredEvaluationMode::PrecomputedCartesian,
            LiveEvaluationMode::PrecomputedCartesian,
        )
    } else {
        (
            VbapTableMode::Polar,
            PreferredEvaluationMode::PrecomputedPolar,
            LiveEvaluationMode::PrecomputedPolar,
        )
    };
    SpatialRenderer::new(
        layout,
        SAMPLE_RATE,
        1, // az_res_deg
        1, // el_res_deg
        0.0,
        2.0,
        table_mode,
        false, // allow_negative_z
        position_interpolation,
        DistanceModel::Linear,
        false,
        1.0,
        1.0,
        0.0,
        1.0,
        false,           // log_object_positions
        [1.0, 2.0, 0.5], // room_ratio
        2.0,
        0.5,
        0.0,
        0.0,   // master_gain_db
        false, // auto_gain
        false, // use_loudness
        false, // distance_diffuse
        1.0,
        1.0,
        preferred,
        initial,
        31,
        31,
        15,
        15,
    )
    .expect("renderer build")
}

/// Deterministic pseudo-random in [-1, 1] from an integer seed (no rng dep).
pub fn pseudo(seed: u64) -> f32 {
    // splitmix64-ish, mapped to [-1, 1].
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    ((x >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
}

/// Interleaved white-ish noise for `n_objects` channels × `BLOCK_SAMPLES`.
pub fn make_pcm(n_objects: usize) -> Vec<f32> {
    let mut pcm = vec![0.0f32; BLOCK_SAMPLES * n_objects];
    for (i, s) in pcm.iter_mut().enumerate() {
        *s = pseudo(i as u64) * 0.25;
    }
    pcm
}

/// Interleaved noise for block `block_index` of a *continuous* stream.
///
/// Unlike [`make_pcm`], successive blocks carry different samples, so a
/// multi-block capture is aperiodic. This matters for any measurement based on
/// cross-correlation: reusing one block as the excitation makes the signal
/// periodic at [`BLOCK_SAMPLES`], and correlation then resolves lag only modulo
/// 40 samples, which silently produces sign-flipped results.
pub fn make_pcm_block(n_objects: usize, block_index: usize) -> Vec<f32> {
    let base = (block_index * BLOCK_SAMPLES * n_objects) as u64;
    (0..BLOCK_SAMPLES * n_objects)
        .map(|i| pseudo(base + i as u64) * 0.25)
        .collect()
}

/// One movement event per object, positions spread deterministically over the
/// dome. `seed_round` rotates the positions so successive metadata frames
/// actually change the target (and thus start a ramp).
pub fn move_events(n_objects: usize, seed_round: u64) -> Vec<SpatialChannelEvent> {
    (0..n_objects)
        .map(|ch| {
            let p = ch as u64 + seed_round.wrapping_mul(2_654_435_761);
            SpatialChannelEvent {
                channel_idx: ch,
                is_bed: false,
                gain_db: Some(0),
                ramp_length: Some(BLOCK_SAMPLES as u32),
                size: Some([0.0, 0.0, 0.0]),
                position: Some([
                    pseudo(p) as f64,
                    pseudo(p ^ 0x1111) as f64,
                    (pseudo(p ^ 0x2222).abs()) as f64,
                ]),
                sample_pos: Some(0),
            }
        })
        .collect()
}

/// One movement event per object on a *continuous* trajectory: each object
/// circles the listener at its own angular rate, at its own fixed height.
///
/// This is the realistic counterpart to [`move_events`], which redraws every
/// position at random each round. Random redraw is a legitimate worst case, but
/// no stream produces it — it teleports every object across the dome every
/// [`BLOCK_SAMPLES`] samples, which by construction defeats anything that
/// exploits direction coherence between consecutive blocks, and so reports no
/// gain for optimisations that real content would benefit from.
///
/// Rates span 12°/s to 96°/s; at 1200 blocks/s that is 0.01° to 0.08° of
/// azimuth per block, the order of magnitude a panned object actually moves.
pub fn drift_events(n_objects: usize, seed_round: u64) -> Vec<SpatialChannelEvent> {
    (0..n_objects)
        .map(|ch| {
            let deg_per_block = (12.0 + (ch % 8) as f64 * 12.0) / 1200.0;
            let az = (ch as f64 * 37.0 + seed_round as f64 * deg_per_block).to_radians();
            SpatialChannelEvent {
                channel_idx: ch,
                is_bed: false,
                gain_db: Some(0),
                ramp_length: Some(BLOCK_SAMPLES as u32),
                size: Some([0.0, 0.0, 0.0]),
                position: Some([az.sin(), az.cos(), (ch % 5) as f64 * 0.25]),
                sample_pos: Some(0),
            }
        })
        .collect()
}

/// Build a renderer with `n_objects` already registered at initial positions,
/// returns it plus a reusable PCM buffer. The first `render_frame` consumes the
/// registration events so subsequent steady frames find populated channel state.
///
/// `ramp_mode` is forced explicitly (the constructor seeds `Sample`): the live
/// mpv default is now `Frame`, so the primary sweeps use `Frame` and a dedicated
/// group contrasts it against `Sample`.
pub fn prepared(
    preset: &str,
    n_objects: usize,
    ramp_mode: RampMode,
    position_interpolation: bool,
    cartesian: bool,
) -> (SpatialRenderer, Vec<f32>) {
    let mut r = make_renderer(preset, position_interpolation, cartesian);
    {
        let ctrl = r.renderer_control();
        ctrl.set_requested_ramp_mode(ramp_mode);
        ctrl.live.write().ramp_mode = ramp_mode;
    }
    let pcm = make_pcm(n_objects);
    let init = move_events(n_objects, 0);
    // Prime channel state + let the initial ramp settle so steady frames are
    // representative of the common case (objects mostly static between blocks).
    let mut buf = Vec::new();
    for round in 0..4 {
        let f = r
            .render_frame(&pcm, n_objects, &init, buf, false)
            .expect("prime render");
        buf = f.samples;
        let _ = round;
    }
    (r, pcm)
}

// Re-exported for the same reason as `RampMode` above: the dev-dependency
// cycle means `renderer`'s own test build is a distinct crate instance, so
// tests inside `renderer` must name this type through the fixture crate for
// the argument types to match.
pub use renderer::binaural::HrirSource;
use renderer::live_params::{BinauralMode, OutputMode};

/// A renderer switched to the independent binaural (headphone) path, with the
/// bundled SAF KEMAR set. Output is 2-channel regardless of the layout.
///
/// `HrirSource::SafKemar` is already the default, so it is not set explicitly —
/// the golden would silently change if that default moved, which is exactly the
/// kind of drift a null test should catch.
pub fn prepared_binaural(n_objects: usize, ramp_mode: RampMode) -> (SpatialRenderer, Vec<f32>) {
    let mut r = make_renderer("7.1.4", true, false);
    {
        let ctrl = r.renderer_control();
        ctrl.set_requested_ramp_mode(ramp_mode);
        let mut live = ctrl.live.write();
        live.ramp_mode = ramp_mode;
        live.binaural.output_mode = OutputMode::Binaural;
    }
    let pcm = make_pcm(n_objects);
    let init = move_events(n_objects, 0);
    let mut buf = Vec::new();
    for _ in 0..4 {
        let f = r
            .render_frame(&pcm, n_objects, &init, buf, false)
            .expect("prime binaural render");
        buf = f.samples;
    }
    (r, pcm)
}

/// Like [`prepared_binaural`] but on the cascaded virtual-speaker stage
/// (`binaural.mode = Cascaded`, default `cascade-12` layout). The priming
/// blocks force the lazy virtual-topology build, so benches and goldens
/// measure the steady state rather than the first-frame setup.
pub fn prepared_binaural_cascaded(
    n_objects: usize,
    ramp_mode: RampMode,
) -> (SpatialRenderer, Vec<f32>) {
    let mut r = make_renderer("7.1.4", true, false);
    {
        let ctrl = r.renderer_control();
        ctrl.set_requested_ramp_mode(ramp_mode);
        let mut live = ctrl.live.write();
        live.ramp_mode = ramp_mode;
        live.binaural.output_mode = OutputMode::Binaural;
        live.binaural.mode = BinauralMode::Cascaded;
    }
    let pcm = make_pcm(n_objects);
    let init = move_events(n_objects, 0);
    let mut buf = Vec::new();
    for _ in 0..4 {
        let f = r
            .render_frame(&pcm, n_objects, &init, buf, false)
            .expect("prime cascaded binaural render");
        buf = f.samples;
    }
    (r, pcm)
}

/// A renderer on the band-limited [`crossover_layout`], so `compute_bands`
/// splits rendering across four frequency bands and the LR4 bank runs.
pub fn prepared_crossover(n_objects: usize, ramp_mode: RampMode) -> (SpatialRenderer, Vec<f32>) {
    let mut r = build_renderer(crossover_layout(), true, false);
    {
        let ctrl = r.renderer_control();
        ctrl.set_requested_ramp_mode(ramp_mode);
        ctrl.live.write().ramp_mode = ramp_mode;
    }
    let pcm = make_pcm(n_objects);
    let init = move_events(n_objects, 0);
    let mut buf = Vec::new();
    for _ in 0..4 {
        let f = r
            .render_frame(&pcm, n_objects, &init, buf, false)
            .expect("prime crossover render");
        buf = f.samples;
    }
    (r, pcm)
}

/// Render `blocks` consecutive blocks, concatenating the interleaved output.
///
/// `move_every` controls how often fresh movement events are injected: every
/// `move_every`-th block gets `move_events(n_objects, round)`, the others carry
/// no events. `move_every = 0` means "never move after priming". This is what
/// makes a golden exercise both the ramping and the steady path.
pub fn render_blocks(
    r: &mut SpatialRenderer,
    pcm: &[f32],
    n_objects: usize,
    blocks: usize,
    move_every: usize,
) -> Vec<f32> {
    let mut out = Vec::new();
    let mut buf = Vec::new();
    for round in 0..blocks {
        let events = if move_every > 0 && round % move_every == 0 {
            move_events(n_objects, round as u64 + 1)
        } else {
            Vec::new()
        };
        let frame = r
            .render_frame(pcm, n_objects, &events, buf, false)
            .expect("render_frame in fixture scene");
        out.extend_from_slice(&frame.samples);
        buf = frame.samples;
        buf.clear();
    }
    out
}

/// Render blocks where only *some* channels ever receive metadata.
///
/// `metadata_channels` gives events; the rest never do. This exercises the
/// branch a channel takes when it has no cached metadata — a path the other
/// null scenes never reach, because they send events for every object. A
/// refactor once made that branch skip such channels entirely and every
/// existing golden still matched.
pub fn render_blocks_partial_metadata(
    r: &mut SpatialRenderer,
    pcm: &[f32],
    n_objects: usize,
    metadata_channels: usize,
    blocks: usize,
    move_every: usize,
) -> Vec<f32> {
    let mut out = Vec::new();
    let mut buf = Vec::new();
    for round in 0..blocks {
        let events = if move_every > 0 && round % move_every == 0 {
            move_events(metadata_channels, round as u64 + 1)
        } else {
            Vec::new()
        };
        let frame = r
            .render_frame(pcm, n_objects, &events, buf, false)
            .expect("render_frame in partial-metadata scene");
        out.extend_from_slice(&frame.samples);
        buf = frame.samples;
        buf.clear();
    }
    out
}

/// Render one object at a fixed horizontal azimuth through the binaural path,
/// returning deinterleaved `(left, right)`.
///
/// Position is set in Omniphony normalized Cartesian, where +x is right, +y is
/// front and +z is up (see `layouts/7.1.4.yaml`), so azimuth θ measured from
/// front toward the right is `(sin θ, cos θ, 0)`.
///
/// The binaural path uses `unit_scale_m` and ignores the anisotropic
/// `room_ratio` (see `BINAURAL.md`), so the azimuth is not distorted by the
/// `[1.0, 2.0, 0.5]` ratio the fixture renderer is built with.
///
/// Switching `hrir_source` away from the default is asynchronous: the request
/// goes to the rebuild worker and frames keep rendering with the previous grid
/// until it lands. So the render is settled in two ordered stages, and the
/// order matters:
///
/// 1. Drive frames until [`SpatialRenderer::binaural_rebuild_pending`] clears,
///    so the requested set is the one actually being convolved.
/// 2. *Then* discard `PRIME_BLOCKS` blocks, so the 20 ms gain slew, the
///    position ramp, and the delay/convolver history left by the previous grid
///    have all settled before the lag measurement.
///
/// Priming before the swap would measure a mixture of the two sets. That was a
/// real bug: the fixture used to prime only, and a run that lost the race
/// measured the default SAF KEMAR grid instead of the requested one — which
/// carries its own intrinsic interaural lag (up to ~7 samples at ±90°, see
/// `hrir_providers_return_time_aligned_pairs`) and so failed the ITD tests
/// intermittently under a loaded machine.
pub fn render_single_object_binaural(
    azimuth_deg: f32,
    blocks: usize,
    hrir_source: HrirSource,
) -> (Vec<f32>, Vec<f32>) {
    const PRIME_BLOCKS: usize = 64;

    let theta = (azimuth_deg as f64).to_radians();
    let position = [theta.sin(), theta.cos(), 0.0];

    let mut r = build_renderer_binaural(
        SpeakerLayout::preset("7.1.4").expect("known preset"),
        true,
        false,
    );
    {
        let ctrl = r.renderer_control();
        ctrl.set_requested_ramp_mode(RampMode::Frame);
        let mut live = ctrl.live.write();
        live.ramp_mode = RampMode::Frame;
        live.binaural.output_mode = OutputMode::Binaural;
        live.binaural.hrir_source = hrir_source.clone();
    }

    let event = vec![SpatialChannelEvent {
        channel_idx: 0,
        is_bed: false,
        gain_db: Some(0),
        ramp_length: Some(BLOCK_SAMPLES as u32),
        size: Some([0.0, 0.0, 0.0]),
        position: Some(position),
        sample_pos: Some(0),
    }];

    let mut buf = Vec::new();

    let mut render_one = |r: &mut SpatialRenderer, buf: Vec<f32>, seed: usize| {
        let f = r
            .render_frame(&make_pcm_block(1, seed), 1, &event, buf, false)
            .expect("binaural ITD render");
        let mut s = f.samples;
        s.clear();
        s
    };

    // Stage 1: wait for the requested HRIR grid to actually be live.
    //
    // The renderer only reads the live params from inside `render_frame`, so
    // the first frame is what registers the request — `binaural_rebuild_pending`
    // is still false before it. Hence render-then-check, not check-then-render.
    // The swap likewise only happens inside `render_frame`, so frames must keep
    // being driven; the yield keeps a fully-loaded machine (the whole test
    // suite in parallel) from starving the rebuild worker we are waiting on.
    //
    // These blocks take their excitation from a disjoint seed range: how many
    // of them run is timing-dependent, and the seeds the measurement depends on
    // must not be.
    const SETTLE_SEED_BASE: usize = 1 << 20;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut settled = 0usize;
    buf = render_one(&mut r, buf, SETTLE_SEED_BASE);
    while r.binaural_rebuild_pending() {
        assert!(
            std::time::Instant::now() < deadline,
            "binaural HRIR rebuild for {hrir_source:?} never landed"
        );
        std::thread::yield_now();
        settled += 1;
        buf = render_one(&mut r, buf, SETTLE_SEED_BASE + settled);
    }

    // Stage 2: now that the right grid is convolving, prime the DSP state with
    // a fixed excitation. Starting the sequence over here — rather than
    // continuing from stage 1 — is what makes the measurement reproducible: the
    // state entering the measurement window is then a pure function of these
    // 64 blocks, not of how many settle blocks the machine's load happened to
    // require.
    for block in 0..PRIME_BLOCKS {
        buf = render_one(&mut r, buf, block);
    }

    let mut left = Vec::with_capacity(blocks * BLOCK_SAMPLES);
    let mut right = Vec::with_capacity(blocks * BLOCK_SAMPLES);
    for block in 0..blocks {
        let f = r
            .render_frame(
                &make_pcm_block(1, PRIME_BLOCKS + block),
                1,
                &event,
                buf,
                false,
            )
            .expect("binaural ITD render");
        for frame in f.samples.chunks_exact(2) {
            left.push(frame[0]);
            right.push(frame[1]);
        }
        buf = f.samples;
        buf.clear();
    }
    (left, right)
}
