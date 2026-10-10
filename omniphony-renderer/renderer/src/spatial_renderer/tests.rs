//! Unit tests for the spatial render path, split out of `mod.rs` to keep the
//! core renderer file focused. Child module of `spatial_renderer`, so `super`
//! resolves to the renderer module and its private items.

use super::*;
// Types the tests construct directly. Imported here (not relied upon via
// `super::*`) so the production `mod.rs` only imports what its own code uses.
use crate::live_params::{LiveEvaluationMode, PreferredEvaluationMode};
use crate::render_backend::EffectiveEvaluationMode;
use crate::spatial_vbap::VbapTableMode;
use crate::speaker_layout::SpeakerLayout;
use crate::test_support;

/// Build two identical renderers with `build`, keep the unified multi-band
/// table on one (it must have built one: `why` says why it should) and force
/// the other onto the per-band path, feed both the same moving object, and
/// require matching output.
fn assert_unified_table_matches_per_band(build: fn() -> SpatialRenderer, why: &str) {
    let mut unified = build();
    unified.prepare_speaker_stage().unwrap();
    assert!(unified.speaker_stage.unified_table.is_some(), "{why}");
    let mut per_band = build();
    per_band.prepare_speaker_stage().unwrap();
    per_band.speaker_stage.unified_table = None;

    let pcm: Vec<f32> = (0..40).map(|i| (i * 7 % 13) as f32 / 13.0 - 0.5).collect();
    let event = vec![SpatialChannelEvent {
        channel_idx: 0,
        is_bed: false,
        gain_db: Some(0.0),
        ramp_length: Some(40),
        size: Some([0.0, 0.0, 0.0]),
        position: Some([0.3, -0.2, 0.4]),
        sample_pos: Some(0),
    }];

    let a = unified
        .render_frame(&pcm, 1, &event, Vec::new(), false)
        .unwrap();
    let b = per_band
        .render_frame(&pcm, 1, &event, Vec::new(), false)
        .unwrap();
    assert_eq!(a.samples.len(), b.samples.len());
    let max_diff = a
        .samples
        .iter()
        .zip(&b.samples)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max);
    assert!(
        max_diff < 1e-6,
        "unified vs per-band output mismatch: max diff {max_diff}"
    );
}

/// The unified multi-band cartesian table must render bit-equivalently to the
/// per-band path it replaces.
#[test]
fn unified_crossover_matches_per_band() {
    fn build() -> SpatialRenderer {
        let mut layout = SpeakerLayout::preset("7.1.4").unwrap();
        for (sp, cutoff) in layout.speakers.iter_mut().zip([80.0, 200.0, 500.0]) {
            sp.freq_low = Some(cutoff);
        }
        SpatialRenderer::new(RendererSpec {
            vbap_position_interpolation: true, // position interpolation → trilinear lookup + per-sample motion
            ..test_support::spec(layout)
        })
        .unwrap()
    }

    assert_unified_table_matches_per_band(build, "crossover layout should build a unified table");
}

/// Polar counterpart of `unified_crossover_matches_per_band`: the unified
/// multi-band POLAR table must render bit-equivalently to the per-band polar
/// path. Same crossover layout, but a precomputed polar evaluator.
#[test]
fn unified_polar_matches_per_band() {
    fn build() -> SpatialRenderer {
        let mut layout = SpeakerLayout::preset("7.1.4").unwrap();
        for (sp, cutoff) in layout.speakers.iter_mut().zip([80.0, 200.0, 500.0]) {
            sp.freq_low = Some(cutoff);
        }
        SpatialRenderer::new(RendererSpec {
            table_mode: VbapTableMode::Polar,
            vbap_position_interpolation: true, // position interpolation → trilinear lookup + per-sample motion
            preferred_evaluation_mode: PreferredEvaluationMode::PrecomputedPolar,
            initial_evaluation_mode: LiveEvaluationMode::PrecomputedPolar,
            cartesian_default_x_size: 31,
            cartesian_default_y_size: 31,
            cartesian_default_z_size: 15,
            cartesian_default_z_neg_size: 15,
            ..test_support::spec(layout)
        })
        .unwrap()
    }

    assert_unified_table_matches_per_band(
        build,
        "polar crossover layout should build a unified table",
    );
}

/// A crossover band with only 1–2 speakers used to have no engine (hardcoded
/// equal-power), which disabled the unified table for the whole crossover.
/// Now such a band carries a `DegenerateVbapBackend`, so the unified table builds
/// and must stay bit-equivalent to the per-band path. Here the top band keeps
/// exactly 2 spatializable speakers (pairwise-VBAP fallback).
#[test]
fn unified_table_with_two_speaker_fallback_band() {
    fn build() -> SpatialRenderer {
        let mut layout = SpeakerLayout::preset("7.1.4").unwrap();
        // Cut all spatializable speakers at 200 Hz except the first two, so the
        // [200, ∞) band has exactly 2 speakers (a fallback band) and the
        // [0, 200) band keeps the rest (a normal ≥3 VBAP band).
        let mut kept = 0;
        for sp in layout.speakers.iter_mut() {
            if !sp.spatialize {
                continue;
            }
            if kept < 2 {
                kept += 1;
                continue;
            }
            sp.freq_high = Some(200.0);
        }
        SpatialRenderer::new(test_support::spec(layout)).unwrap()
    }

    assert_unified_table_matches_per_band(
        build,
        "a 2-speaker fallback band must not disable the unified table",
    );
}

/// An evaluation-mode change must reuse the triangulated gain model (the
/// geometry is mode-independent), rebuilding only the evaluation wrapper. A
/// geometry change (bumped generation) must rebuild the model. Verified via
/// `Arc::ptr_eq` on the decorated model.
#[test]
fn eval_mode_change_reuses_geometry() {
    let layout = SpeakerLayout::preset("7.1.4").unwrap();
    let r = SpatialRenderer::new(test_support::spec(layout)).unwrap();
    let control = r.renderer_control();
    let topo0 = control.active_topology();
    let model0 = topo0
        .backend
        .decorated_model()
        .expect("vbap backend exposes a decorated model");

    // Evaluation-mode-only change: geometry generation unchanged → reuse model.
    control
        .live
        .write()
        .set_evaluation_mode(LiveEvaluationMode::Realtime);
    let plan = control.prepare_topology_rebuild().expect("rebuild plan");
    let reused = plan
        .build_topology_reusing(Some(&topo0))
        .expect("reuse build");
    assert_eq!(
        reused.backend.evaluation_mode(),
        EffectiveEvaluationMode::Realtime
    );
    assert!(
        Arc::ptr_eq(&model0, &reused.backend.decorated_model().unwrap()),
        "evaluation-mode change must reuse the triangulated gain model"
    );

    // Geometry change bumps the generation → full rebuild (different model).
    control.bump_geometry_generation();
    let plan2 = control.prepare_topology_rebuild().expect("rebuild plan 2");
    let rebuilt = plan2.build_topology_reusing(Some(&topo0)).expect("rebuild");
    assert!(
        !Arc::ptr_eq(&model0, &rebuilt.backend.decorated_model().unwrap()),
        "a geometry change must rebuild the gain model"
    );

    // Backend switch at an unchanged generation (a config applied after
    // construction): the vbap model must not be re-wrapped for barycenter.
    control.live.write().backend_id = "barycenter".to_string();
    let plan3 = control.prepare_topology_rebuild().expect("rebuild plan 3");
    let switched = plan3
        .build_topology_reusing(Some(&rebuilt))
        .expect("backend switch build");
    assert_eq!(switched.model_backend_id, "barycenter");
    assert!(
        !Arc::ptr_eq(
            &rebuilt.backend.decorated_model().unwrap(),
            &switched.backend.decorated_model().unwrap()
        ),
        "a backend switch must build the new backend's gain model"
    );
}

/// A gain model that counts its `compute_gains` calls (equal gains over its
/// speakers), so a test can tell a table build (one call per grid cell) from
/// the build's smoke test (one call per reference position).
struct CountingModel {
    speakers: usize,
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl crate::render_backend::GainModel for CountingModel {
    fn backend_id(&self) -> &'static str {
        "counting"
    }
    fn backend_label(&self) -> &'static str {
        "counting"
    }
    fn capabilities(&self) -> crate::render_backend::BackendCapabilities {
        crate::render_backend::BackendCapabilities {
            supports_realtime: true,
            supports_precomputed_polar: true,
            supports_precomputed_cartesian: true,
            ..Default::default()
        }
    }
    fn speaker_count(&self) -> usize {
        self.speakers
    }
    fn compute_gains(
        &self,
        _req: &crate::render_backend::RenderRequest,
        _scratch: &mut crate::render_backend::GainScratch,
        gains: &mut [f32],
    ) {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        gains.fill(1.0 / (self.speakers as f32).sqrt());
    }
    fn save_to_file(&self, _path: &std::path::Path, _layout: &SpeakerLayout) -> Result<()> {
        Ok(())
    }
}

struct CountingFactory(Arc<std::sync::atomic::AtomicUsize>);

impl crate::plugin::PluginFactory for CountingFactory {
    fn id(&self) -> &'static str {
        "counting"
    }
}

impl crate::backend_registry::BackendFactory for CountingFactory {
    fn build_plan(
        &self,
        ctx: &crate::backend_registry::BackendBuildCtx<'_>,
    ) -> Option<crate::backend_registry::BackendBuildPlan> {
        let speakers = ctx.layout.spatializable_positions().1.len();
        let calls = Arc::clone(&self.0);
        Some(crate::backend_registry::BackendBuildPlan::Dynamic(
            crate::backend_registry::DynamicBackendPlan::new("counting", move || {
                Ok(Box::new(CountingModel {
                    speakers,
                    calls: Arc::clone(&calls),
                }))
            }),
        ))
    }
}

/// The topology published on the control samples no gain table, at
/// construction, at the host's boot rebuild or at a live recompute: nothing
/// renders through it, every crossover band (here the single band of a layout
/// without crossover) samples its own. Its engine still names the backend and
/// the effective (precomputed) mode, smoke-tests the model, and hands its
/// model to a geometry-unchanged recompute; the band engines, the audio and
/// the Studio band gain table keep working from their own tables.
#[test]
fn the_published_topology_samples_no_gain_table() {
    use std::sync::atomic::Ordering;
    let smoke = crate::backend_registry::SMOKE_TEST_POSITIONS.len();
    let layout = SpeakerLayout::preset("7.1.4").unwrap();
    let mut r = SpatialRenderer::new(test_support::spec(layout)).unwrap();
    let control = r.renderer_control();

    // Construction: the default VBAP topology reports its mode, samples nothing.
    let constructed = control.active_topology();
    assert_eq!(
        constructed.backend.evaluation_mode(),
        EffectiveEvaluationMode::PrecomputedCartesian
    );
    assert!(!constructed.backend.has_sampled_table());
    assert!(constructed.backend.cartesian_parts().is_none());

    // The host's boot rebuild, onto a backend that counts its calls.
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    control.register_backend(Box::new(CountingFactory(Arc::clone(&calls))));
    control.live.write().backend_id = "counting".to_string();
    let plan = control.prepare_topology_rebuild().expect("plan");
    let booted = plan
        .build_topology_reusing(Some(&control.active_topology()))
        .expect("boot topology");
    assert_eq!(
        calls.load(Ordering::Relaxed),
        smoke,
        "the boot topology only smoke-tests the model"
    );
    assert!(!booted.backend.has_sampled_table());
    assert_eq!(booted.backend.backend_id(), "counting");
    assert_eq!(
        booted.backend.evaluation_mode(),
        EffectiveEvaluationMode::PrecomputedCartesian
    );
    control.publish_topology(booted);

    // The single band samples its own table, on the first frame.
    let pcm = vec![0.5f32; 40];
    let event = vec![SpatialChannelEvent {
        channel_idx: 0,
        is_bed: false,
        gain_db: Some(0.0),
        ramp_length: Some(0),
        size: Some([0.0, 0.0, 0.0]),
        position: Some([0.3, -0.2, 0.4]),
        sample_pos: Some(0),
    }];
    let frame = r.render_frame(&pcm, 1, &event, Vec::new(), false).unwrap();
    assert_eq!(r.speaker_stage_builds(), 1);
    assert_eq!(r.speaker_stage.render_bands.len(), 1);
    let band = r.speaker_stage.render_bands[0]
        .engine()
        .expect("band engine");
    assert!(band.has_sampled_table());
    let band_calls = calls.load(Ordering::Relaxed) - smoke;
    assert!(
        band_calls > 1000,
        "the band samples its grid ({band_calls} calls)"
    );
    assert!(
        frame.samples.iter().any(|s| *s != 0.0),
        "the object renders through the band table"
    );

    // A live recompute after a speaker edit: the model is rebuilt and
    // smoke-tested, nothing sampled; the next frame re-samples the band.
    let before = calls.load(Ordering::Relaxed);
    control.bump_geometry_generation();
    let plan = control.prepare_topology_rebuild().expect("plan");
    let recomputed = plan
        .build_topology_reusing(Some(&control.active_topology()))
        .expect("recompute");
    assert_eq!(calls.load(Ordering::Relaxed) - before, smoke);
    assert!(!recomputed.backend.has_sampled_table());
    control.publish_topology(recomputed);
    // The render thread samples nothing for it: it asks the band worker and
    // keeps rendering the bands it has until the new ones land.
    let sampled_here = crate::backend_registry::tables_sampled_on_this_thread();
    let old_band = Arc::clone(r.speaker_stage.render_bands[0].engine().unwrap());
    r.render_frame(&pcm, 1, &event, Vec::new(), false).unwrap();
    assert!(r.speaker_stage_rebuild_pending());
    assert_eq!(r.speaker_stage_builds(), 1, "the old bands still render");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while r.speaker_stage_rebuild_pending() {
        assert!(
            std::time::Instant::now() < deadline,
            "the worker never delivered"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
        r.render_frame(&pcm, 1, &event, Vec::new(), false).unwrap();
    }
    assert_eq!(
        r.speaker_stage_builds(),
        2,
        "the worker's bands are installed"
    );
    assert!(!Arc::ptr_eq(
        &old_band,
        r.speaker_stage.render_bands[0].engine().unwrap()
    ));
    assert_eq!(
        crate::backend_registry::tables_sampled_on_this_thread(),
        sampled_here,
        "no table sampled on the render thread"
    );
    assert_eq!(calls.load(Ordering::Relaxed) - before, smoke + band_calls);

    // An evaluation-only recompute (grid size) reuses the published model.
    let current = control.active_topology();
    control.live.write().evaluation.cartesian.x_size = 11;
    let plan = control.prepare_topology_rebuild().expect("plan");
    let before = calls.load(Ordering::Relaxed);
    let resized = plan
        .build_topology_reusing(Some(&current))
        .expect("evaluation-only recompute");
    assert_eq!(calls.load(Ordering::Relaxed) - before, smoke);
    assert!(Arc::ptr_eq(
        &current.backend.decorated_model().unwrap(),
        &resized.backend.decorated_model().unwrap()
    ));

    // The Studio band gain table samples its own band topologies.
    let before = calls.load(Ordering::Relaxed);
    let table = control
        .build_band_gaintable_full()
        .expect("band gain table");
    assert_eq!(table.bands.len(), 1);
    assert!(table.bands[0].gains.iter().any(|g| *g > 0.0));
    assert!(calls.load(Ordering::Relaxed) - before > smoke);
}

/// What [`FlakyFactory`] does with the next gain model it is asked for.
const FLAKY_BUILDS: u8 = 0;
const FLAKY_FAILS: u8 = 1;
const FLAKY_PANICS: u8 = 2;

/// A backend whose model build can be made to fail or to panic.
struct FlakyFactory(Arc<std::sync::atomic::AtomicU8>);

impl crate::plugin::PluginFactory for FlakyFactory {
    fn id(&self) -> &'static str {
        "flaky"
    }
}

impl crate::backend_registry::BackendFactory for FlakyFactory {
    fn build_plan(
        &self,
        ctx: &crate::backend_registry::BackendBuildCtx<'_>,
    ) -> Option<crate::backend_registry::BackendBuildPlan> {
        let speakers = ctx.layout.spatializable_positions().1.len();
        let mode = Arc::clone(&self.0);
        Some(crate::backend_registry::BackendBuildPlan::Dynamic(
            crate::backend_registry::DynamicBackendPlan::new("flaky", move || {
                match mode.load(std::sync::atomic::Ordering::Relaxed) {
                    FLAKY_FAILS => Err(anyhow::anyhow!("no hull")),
                    FLAKY_PANICS => panic!("backend bug"),
                    _ => Ok(Box::new(CountingModel {
                        speakers,
                        calls: Arc::default(),
                    })),
                }
            }),
        ))
    }
}

/// Render a frame, which asks the band worker for the set a change needs, then
/// frames until the worker has answered.
fn settle(r: &mut SpatialRenderer, pcm: &[f32]) {
    r.render_frame(pcm, 1, &[], Vec::new(), false).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while r.speaker_stage_rebuild_pending() {
        assert!(
            std::time::Instant::now() < deadline,
            "the worker never answered"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
        r.render_frame(pcm, 1, &[], Vec::new(), false).unwrap();
    }
}

/// The same panic on a build the calling thread makes itself — at start-up
/// ([`SpatialRenderer::prepare_speaker_stage`]) or in synchronous mode
/// (offline renders) — is an error the caller gets, not a panic through the
/// engine or the render thread.
#[test]
fn a_band_build_that_panics_on_the_calling_thread_is_an_error() {
    use std::sync::atomic::Ordering;
    let mut r = build_table_renderer(true, false);
    let control = r.renderer_control();
    let mode = Arc::new(std::sync::atomic::AtomicU8::new(FLAKY_BUILDS));
    control.register_backend(Box::new(FlakyFactory(Arc::clone(&mode))));
    control.live.write().backend_id = "flaky".to_string();
    control.bump_geometry_generation();
    let plan = control.prepare_topology_rebuild().expect("plan");
    let topology = plan
        .build_topology_reusing(Some(&control.active_topology()))
        .expect("topology");
    mode.store(FLAKY_PANICS, Ordering::Relaxed);
    control.publish_topology(topology);

    let prepared =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| r.prepare_speaker_stage()))
            .expect("no panic out of prepare_speaker_stage");
    let error = format!("{:#}", prepared.expect_err("the build failed"));
    assert!(error.contains("backend bug"), "{error}");

    r.set_synchronous_stage_builds(true);
    let pcm = vec![0.25f32; 40];
    let rendered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        r.render_frame(&pcm, 1, &[], Vec::new(), false).map(|_| ())
    }))
    .expect("no panic out of a synchronous render");
    let error = format!("{:#}", rendered.expect_err("the build failed"));
    assert!(error.contains("backend bug"), "{error}");
}

/// A band set the worker cannot build — its backend fails, or panics — is
/// answered all the same: the stage stops waiting, keeps the bands it has,
/// does not ask again every frame, and the reason reaches the control for the
/// clients. The worker survives the panic and builds the next set, which
/// takes the error back.
#[test]
fn a_band_build_that_fails_on_the_worker_is_reported_and_not_awaited() {
    use std::sync::atomic::Ordering;
    let mut r = build_table_renderer(true, false);
    let control = r.renderer_control();
    let mode = Arc::new(std::sync::atomic::AtomicU8::new(FLAKY_BUILDS));
    control.register_backend(Box::new(FlakyFactory(Arc::clone(&mode))));
    control.live.write().backend_id = "flaky".to_string();

    let pcm = vec![0.25f32; 40];
    // A speaker edit: the recompute builds and publishes the topology while
    // the backend still works, then the band build meets `then`.
    let publish_then = |then: u8| {
        mode.store(FLAKY_BUILDS, Ordering::Relaxed);
        control.bump_geometry_generation();
        let plan = control.prepare_topology_rebuild().expect("plan");
        let topology = plan
            .build_topology_reusing(Some(&control.active_topology()))
            .expect("topology");
        mode.store(then, Ordering::Relaxed);
        control.publish_topology(topology);
    };
    // The flaky backend, working: its bands are installed.
    publish_then(FLAKY_BUILDS);
    settle(&mut r, &pcm);
    assert!(!r.speaker_stage_rebuild_failed());
    let builds = r.speaker_stage_builds();
    assert_eq!(control.take_band_build_error(), None);

    for (then, reason) in [(FLAKY_FAILS, "no hull"), (FLAKY_PANICS, "backend bug")] {
        publish_then(then);
        settle(&mut r, &pcm);
        assert!(r.speaker_stage_rebuild_failed());
        assert_eq!(
            r.speaker_stage_builds(),
            builds,
            "the previous bands keep rendering"
        );
        let error = control.take_band_build_error().expect("reported");
        assert!(error.contains(reason), "{error}");
        // Not asked again while the key stays on it.
        for _ in 0..4 {
            r.render_frame(&pcm, 1, &[], Vec::new(), false).unwrap();
            assert!(!r.speaker_stage_rebuild_pending());
        }
        assert!(r.speaker_stage_rebuild_failed());
        assert_eq!(control.take_band_build_error(), None);
    }

    // The worker outlived the panic: the next edit is built and installed.
    publish_then(FLAKY_BUILDS);
    settle(&mut r, &pcm);
    assert!(!r.speaker_stage_rebuild_failed());
    assert_eq!(r.speaker_stage_builds(), builds + 1);
    assert_eq!(
        control.take_band_build_error().as_deref(),
        Some(""),
        "the error is taken back"
    );
}

/// A grid size past the evaluation table budget — typed into config.yaml or
/// sent over OSC — is refused by the band build before anything is
/// allocated: the reason reaches the clients, and the previous bands keep
/// rendering.
#[test]
fn an_oversized_evaluation_grid_is_refused_and_reported() {
    let mut r = build_table_renderer(true, false);
    let control = r.renderer_control();
    let pcm = vec![0.25f32; 40];
    settle(&mut r, &pcm);
    assert!(!r.speaker_stage_rebuild_failed());
    let builds = r.speaker_stage_builds();
    assert_eq!(control.take_band_build_error(), None);

    control.live.write().evaluation.cartesian.x_size = 1_000_000_000;
    control.bump_geometry_generation();
    let plan = control.prepare_topology_rebuild().expect("plan");
    let topology = plan
        .build_topology_reusing(Some(&control.active_topology()))
        .expect("the topology itself samples no table");
    control.publish_topology(topology);
    settle(&mut r, &pcm);

    assert!(r.speaker_stage_rebuild_failed());
    assert_eq!(
        r.speaker_stage_builds(),
        builds,
        "the previous bands keep rendering"
    );
    let error = control.take_band_build_error().expect("reported");
    assert!(error.contains("budget"), "{error}");
    // The band gain table Studio subscribes to samples the same grid.
    let err = control.build_band_gaintable_full().err().expect("refused");
    assert!(err.to_string().contains("budget"), "{err}");
}

/// The band gain table Studio subscribes to keeps every band's gains: its
/// budget counts them all. On a four-band layout, a grid one band's table
/// would fit in is refused, before anything is allocated.
#[test]
fn the_band_gain_table_budget_counts_every_band() {
    use crate::render_backend::MAX_EVALUATION_TABLE_BYTES;
    let r = build_table_renderer(true, true);
    let control = r.renderer_control();
    let layout = control.active_topology().speaker_layout.clone();
    assert_eq!(crate::crossover::compute_bands(&layout).len(), 4);
    let speakers = layout.speakers.len();
    // The largest cubic grid one band's table fits in (the build samples
    // each axis at its size plus one).
    let table = |side: usize| (side + 1).pow(3) * speakers * 4;
    let mut side = 2;
    while table(side + 1) <= MAX_EVALUATION_TABLE_BYTES {
        side += 1;
    }
    assert!(4 * table(side) > MAX_EVALUATION_TABLE_BYTES);
    {
        let mut live = control.live.write();
        let g = &mut live.evaluation.cartesian;
        (g.x_size, g.y_size, g.z_size, g.z_neg_size) = (side, side, side, 0);
    }
    let err = control
        .build_band_gaintable_full()
        .err()
        .expect("four bands do not fit");
    assert!(err.to_string().contains("budget"), "{err}");
}

/// In cascaded binaural mode the virtual speakers stand where the installed
/// bands place them. After a speaker move the bands of the previous layout
/// render on until the worker's set lands, and for good if it cannot be built:
/// the geometry binauralised must be theirs all that time, not the published
/// one, or gains computed for one placement feed sources standing at another.
#[test]
fn the_cascade_geometry_follows_the_installed_bands() {
    use std::sync::atomic::Ordering;
    let mut r = build_cascade_test_renderer(LiveEvaluationMode::PrecomputedCartesian, false);
    {
        let mut live = r.control.live.write();
        live.binaural.output_mode = crate::live_params::OutputMode::Binaural;
        live.binaural.mode = crate::live_params::BinauralMode::Cascaded;
    }
    let control = r.renderer_control();
    let mode = Arc::new(std::sync::atomic::AtomicU8::new(FLAKY_BUILDS));
    control.register_backend(Box::new(FlakyFactory(Arc::clone(&mode))));
    control.live.write().backend_id = "flaky".to_string();

    let pcm = vec![0.25f32; 40];
    // A speaker moved in Studio: the recompute publishes the topology while
    // the backend still works, then the band build meets `then`.
    let publish_move_then = |then: u8| {
        mode.store(FLAKY_BUILDS, Ordering::Relaxed);
        control.with_editable_layout(|layout| layout.speakers[0].x -= 0.05);
        control.bump_geometry_generation();
        let plan = control.prepare_topology_rebuild().expect("plan");
        let topology = plan
            .build_topology_reusing(Some(&control.active_topology()))
            .expect("topology");
        mode.store(then, Ordering::Relaxed);
        control.publish_topology(topology);
    };
    let positions = |r: &SpatialRenderer| r.cascade.as_ref().expect("cascade").bin_pos.clone();
    let engine =
        |r: &SpatialRenderer| Arc::clone(r.speaker_stage.render_bands[0].engine().expect("engine"));

    r.render_frame(&pcm, 1, &[], Vec::new(), false).unwrap();
    let (first_positions, first_engine) = (positions(&r), engine(&r));

    // A move the worker builds. On the frame after the publish the previous
    // bands still render, onto virtual speakers that have not moved.
    publish_move_then(FLAKY_BUILDS);
    r.render_frame(&pcm, 1, &[], Vec::new(), false).unwrap();
    assert!(r.speaker_stage_rebuild_pending());
    assert!(Arc::ptr_eq(&first_engine, &engine(&r)));
    assert_eq!(positions(&r), first_positions);
    // They move with the bands.
    settle(&mut r, &pcm);
    assert!(!Arc::ptr_eq(&first_engine, &engine(&r)));
    let moved = positions(&r);
    assert_ne!(moved, first_positions);

    // A move whose bands cannot be built: neither changes.
    let moved_engine = engine(&r);
    publish_move_then(FLAKY_FAILS);
    settle(&mut r, &pcm);
    assert!(r.speaker_stage_rebuild_failed());
    r.render_frame(&pcm, 1, &[], Vec::new(), false).unwrap();
    assert!(Arc::ptr_eq(&moved_engine, &engine(&r)));
    assert_eq!(positions(&r), moved);

    // The next move that builds brings both to the published layout.
    publish_move_then(FLAKY_BUILDS);
    settle(&mut r, &pcm);
    assert!(!Arc::ptr_eq(&moved_engine, &engine(&r)));
    let published = &control.active_topology().speaker_layout.speakers[0];
    assert_eq!(positions(&r)[0][0], published.x as f64);
}

/// The band gain models are recorded under the geometry generation of the
/// topology they are cut from, not the one the control has reached when they
/// are built. An edit made after a topology was published and before its
/// bands were built (the worker was busy, or simply the frame had not come)
/// has bumped the control already: recorded under that generation, the bands
/// of the old layout would be reused as they are for the topology of that
/// edit, and the last speaker move would never reach the audio.
#[test]
fn bands_built_after_a_later_edit_are_not_reused_for_it() {
    let mut r = build_table_renderer(true, false);
    let control = r.renderer_control();
    let band_model = |r: &SpatialRenderer| {
        r.speaker_stage.render_bands[0]
            .engine()
            .expect("band engine")
            .decorated_model()
            .expect("model")
    };
    let recompute = || {
        let plan = control.prepare_topology_rebuild().expect("plan");
        plan.build_topology_reusing(Some(&control.active_topology()))
            .expect("topology")
    };

    // A first edit, published.
    control.bump_geometry_generation();
    control.publish_topology(recompute());
    // A second one lands before the stage has built the bands of the first.
    control.bump_geometry_generation();
    r.prepare_speaker_stage().unwrap();
    let first_edit = band_model(&r);

    // Its own topology: the bands are built anew.
    control.publish_topology(recompute());
    r.prepare_speaker_stage().unwrap();
    assert!(
        !Arc::ptr_eq(&first_edit, &band_model(&r)),
        "the bands of the second edit reuse the gain model of the first"
    );

    // Whereas an evaluation-only recompute, at the same generation, does
    // reuse it.
    let second_edit = band_model(&r);
    control.publish_topology(recompute());
    r.prepare_speaker_stage().unwrap();
    assert!(Arc::ptr_eq(&second_edit, &band_model(&r)));
}

/// A sample-rate change rebuilds everything timed in samples, the crossover
/// bank among it, but takes the band engines over: a gain table does not
/// depend on the rate. A host that prepared the stage before it knew the
/// stream's rate (the CLI always does) would otherwise sample every table a
/// second time, on the first frame.
#[test]
fn a_sample_rate_change_takes_the_band_engines_over() {
    let mut r = build_table_renderer(true, true);
    let engines = |r: &SpatialRenderer| -> Vec<_> {
        r.speaker_stage
            .render_bands
            .iter()
            .map(|band| Arc::clone(band.engine().expect("band engine")))
            .collect()
    };
    let before = engines(&r);
    assert!(before.len() > 1, "a crossover layout");
    let builds = r.speaker_stage_builds();
    let sampled = crate::backend_registry::tables_sampled_on_this_thread();

    r.set_sample_rate(96_000).unwrap();
    assert!(
        r.speaker_stage.crossover_filter_bank.is_none(),
        "the stage is rebuilt by the next frame"
    );
    let pcm = vec![0.0f32; 40];
    r.render_frame(&pcm, 1, &[], Vec::new(), false).unwrap();

    assert_eq!(r.speaker_stage_builds(), builds + 1);
    assert_eq!(
        crate::backend_registry::tables_sampled_on_this_thread(),
        sampled,
        "no gain table is sampled again"
    );
    let after = engines(&r);
    assert_eq!(before.len(), after.len());
    assert!(
        before.iter().zip(&after).all(|(a, b)| Arc::ptr_eq(a, b)),
        "the band engines are the same"
    );
    assert!(r.speaker_stage.unified_table.is_some());
    assert!(r.speaker_stage.crossover_filter_bank.is_some());
    assert_eq!(
        r.control
            .crossover_info()
            .expect("crossover info")
            .sample_rate,
        96_000,
        "the bank is built for the new rate"
    );
}

#[test]
fn test_renderer_creation() {
    let layout = SpeakerLayout::preset("7.1.4").unwrap();
    let renderer = SpatialRenderer::new(RendererSpec {
        table_mode: VbapTableMode::Polar,
        vbap_position_interpolation: false,
        preferred_evaluation_mode: PreferredEvaluationMode::PrecomputedPolar,
        initial_evaluation_mode: LiveEvaluationMode::PrecomputedPolar,
        cartesian_default_x_size: 31,
        cartesian_default_y_size: 31,
        cartesian_default_z_size: 15,
        cartesian_default_z_neg_size: 15,
        ..test_support::spec(layout)
    });

    assert!(renderer.is_ok());

    let renderer = renderer.unwrap();
    assert_eq!(renderer.num_speakers(), 12);
}

/// The parametrable virtual bed mixes direct and virtualized channels in one
/// frame: `bed_indices` is full-length and carries `usize::MAX` for a channel
/// that must be VBAP-panned (object) even though its index is within
/// `num_beds`. This guards the render-loop generalisation from positional
/// (`idx < num_beds`) to sentinel-aware routing: channel 0 (sentinel) must
/// spread via VBAP while channel 1 (bed id 3 = LFE, a non-prefix bed) routes
/// one-hot to the LFE speaker.
#[test]
fn virtual_bed_mixes_direct_and_virtualized_channels() {
    fn build() -> SpatialRenderer {
        let layout = SpeakerLayout::preset("7.1.4").unwrap();
        SpatialRenderer::new(test_support::spec(layout)).unwrap()
    }

    // LFE is speaker index 3 in the 7.1.4 preset (spatialize:false).
    const LFE_SPK: usize = 3;
    let num_speakers = 12;
    let sample_length = 4;

    // Per-speaker summed |energy| across the block.
    let energy = |out: &[f32]| -> Vec<f32> {
        let mut e = vec![0.0f32; num_speakers];
        for s in 0..sample_length {
            for (spk, slot) in e.iter_mut().enumerate() {
                *slot += out[s * num_speakers + spk].abs();
            }
        }
        e
    };

    // Routing: channel 0 = virtual (object), channel 1 = direct LFE.
    let beds = [
        ChannelRoute::Virtual,
        ChannelRoute::Direct(bridge_api::RChannelLabel::LFE),
    ];

    // Pass A: only the object channel (0) carries signal.
    let mut ra = build();
    ra.configure_channel_routing(&beds);
    let pcm_a: Vec<f32> = (0..sample_length).flat_map(|_| [0.6f32, 0.0]).collect();
    let events_a = vec![
        SpatialChannelEvent {
            channel_idx: 0,
            is_bed: false,
            gain_db: Some(0.0),
            ramp_length: Some(0),
            size: Some([0.0, 0.0, 0.0]),
            position: Some([0.0, 1.0, 0.0]), // front-centre object
            sample_pos: Some(0),
        },
        SpatialChannelEvent {
            channel_idx: 1,
            is_bed: true,
            gain_db: Some(0.0),
            ramp_length: Some(0),
            size: None,
            position: None,
            sample_pos: Some(0),
        },
    ];
    let ea = energy(
        &ra.render_frame(&pcm_a, 2, &events_a, Vec::new(), false)
            .unwrap()
            .samples,
    );
    assert!(
        ea.iter().sum::<f32>() > 0.0,
        "object channel must produce output"
    );
    assert!(
        ea[LFE_SPK] < 1e-6,
        "front object must not leak into the non-spatialized LFE speaker (got {})",
        ea[LFE_SPK]
    );

    // Pass B: only the bed channel (1) carries signal → one-hot at the LFE.
    let mut rb = build();
    rb.configure_channel_routing(&beds);
    let pcm_b: Vec<f32> = (0..sample_length).flat_map(|_| [0.0f32, 0.6]).collect();
    let eb = energy(
        &rb.render_frame(&pcm_b, 2, &events_a, Vec::new(), false)
            .unwrap()
            .samples,
    );
    assert!(eb[LFE_SPK] > 0.0, "bed channel must reach the LFE speaker");
    for (spk, e) in eb.iter().enumerate() {
        if spk != LFE_SPK {
            assert!(
                *e < 1e-6,
                "bed routing must be one-hot; speaker {spk} got {e}"
            );
        }
    }
}

/// Locks the documented subwoofer bass-management recipe: flip the LFE to
/// `spatialize: true` with `freq_high: 120` while every other spatialized
/// speaker carries `freq_low: 120`. The sub is then alone in the `[0, 120)`
/// band, so the single-speaker degenerate rule routes the low band of every
/// object to it; the bands above exclude it entirely; and the stream's own
/// LFE bed channel keeps its direct one-hot feed (bed routing is keyed on
/// the speaker name, not on `spatialize`).
#[test]
fn spatialized_lfe_alone_in_low_band_routes_object_bass() {
    const CUTOFF: f32 = 120.0;
    const LFE_SPK: usize = 3; // 7.1.4 preset order
    fn build() -> SpatialRenderer {
        let mut layout = SpeakerLayout::preset("7.1.4").unwrap();
        for (idx, sp) in layout.speakers.iter_mut().enumerate() {
            if idx == LFE_SPK {
                sp.spatialize = true;
                sp.freq_high = Some(CUTOFF);
            } else {
                sp.freq_low = Some(CUTOFF);
            }
        }
        SpatialRenderer::new(test_support::spec(layout)).unwrap()
    }

    let num_speakers = 12;
    let sample_length = 9_600; // 200 ms — lets the 20 Hz tone settle
    let sample_rate = 48_000.0f32;

    // Per-speaker RMS over the second half of the block (filter steady state).
    let rms = |out: &[f32]| -> Vec<f32> {
        let half = sample_length / 2;
        let mut e = vec![0.0f32; num_speakers];
        for s in half..sample_length {
            for (spk, slot) in e.iter_mut().enumerate() {
                let v = out[s * num_speakers + spk];
                *slot += v * v;
            }
        }
        e.iter().map(|x| (x / half as f32).sqrt()).collect()
    };

    // A front-centre object playing a sine at `freq`.
    let render_tone = |freq: f32| -> Vec<f32> {
        let mut r = build();
        let pcm: Vec<f32> = (0..sample_length)
            .map(|i| 0.5 * (2.0 * std::f32::consts::PI * freq * i as f32 / sample_rate).sin())
            .collect();
        let events = vec![SpatialChannelEvent {
            channel_idx: 0,
            is_bed: false,
            gain_db: Some(0.0),
            ramp_length: Some(0),
            size: Some([0.0, 0.0, 0.0]),
            position: Some([0.0, 1.0, 0.0]),
            sample_pos: Some(0),
        }];
        rms(&r
            .render_frame(&pcm, 1, &events, Vec::new(), false)
            .unwrap()
            .samples)
    };

    // Deep bass: the sub must carry the tone and the mains must be genuinely
    // relieved of it — the LR4 high-pass rejects 20 Hz (fc/6) by ~60 dB.
    // Absolute levels include the renderer's distance attenuation for this
    // object position (identical across both tones), so the thresholds below
    // are calibrated with ample margin rather than derived from the input.
    let low = render_tone(20.0);
    assert!(
        low[LFE_SPK] > 0.08,
        "sub must carry the 20 Hz object tone, got RMS {}",
        low[LFE_SPK]
    );
    let max_main = low
        .iter()
        .enumerate()
        .filter(|(spk, _)| *spk != LFE_SPK)
        .map(|(_, e)| *e)
        .fold(0.0f32, f32::max);
    assert!(
        low[LFE_SPK] > 1.5 * max_main,
        "sub must dominate at 20 Hz: sub {} vs loudest main {}",
        low[LFE_SPK],
        max_main
    );
    assert!(
        max_main < 0.02,
        "mains must be relieved of the 20 Hz band (24 dB/oct high-pass), loudest main RMS {max_main}"
    );

    // Treble: the sub is out of the upper bands and the low-pass rejects
    // 4 kHz by >100 dB — it must stay silent.
    let high = render_tone(4_000.0);
    assert!(
        high[LFE_SPK] < 1e-4,
        "sub must not receive a 4 kHz object tone, got RMS {}",
        high[LFE_SPK]
    );
    assert!(
        high.iter().sum::<f32>() > 0.05,
        "the 4 kHz tone must reach the mains"
    );

    // The stream's own LFE bed channel still routes one-hot to the sub even
    // though the speaker is now spatialized.
    let mut r = build();
    let beds = [
        ChannelRoute::Virtual,
        ChannelRoute::Direct(bridge_api::RChannelLabel::LFE),
    ];
    r.configure_channel_routing(&beds);
    let bed_len = 8usize;
    let pcm: Vec<f32> = (0..bed_len).flat_map(|_| [0.0f32, 0.6]).collect();
    let events = vec![
        SpatialChannelEvent {
            channel_idx: 0,
            is_bed: false,
            gain_db: Some(0.0),
            ramp_length: Some(0),
            size: Some([0.0, 0.0, 0.0]),
            position: Some([0.0, 1.0, 0.0]),
            sample_pos: Some(0),
        },
        SpatialChannelEvent {
            channel_idx: 1,
            is_bed: true,
            gain_db: Some(0.0),
            ramp_length: Some(0),
            size: None,
            position: None,
            sample_pos: Some(0),
        },
    ];
    let out = r
        .render_frame(&pcm, 2, &events, Vec::new(), false)
        .unwrap()
        .samples;
    let mut e = vec![0.0f32; num_speakers];
    for s in 0..bed_len {
        for (spk, slot) in e.iter_mut().enumerate() {
            *slot += out[s * num_speakers + spk].abs();
        }
    }
    assert!(
        e[LFE_SPK] > 0.0,
        "the LFE bed channel must still reach the spatialized sub"
    );
    for (spk, v) in e.iter().enumerate() {
        if spk != LFE_SPK {
            assert!(
                *v < 1e-6,
                "LFE bed routing must stay one-hot with spatialize:true; speaker {spk} got {v}"
            );
        }
    }
}

/// Guard rail: the four ramp modes must stay wired and each keep its own
/// behaviour. `Off` snaps to the target, `Frame` holds the block-start
/// position, `Sample` interpolates the position per sample, and `Interp`
/// interpolates the gains per sample from the previous block's end. We render
/// TWO blocks per mode with a position change in between (the first block
/// seeds `Interp`'s start gains, so its ramp only shows on the second) and
/// compare the second block: every output must be finite, non-silent, and
/// the modes must not collapse onto one another for a moving object.
#[test]
fn all_four_ramp_modes_render_distinctly() {
    fn build() -> SpatialRenderer {
        let layout = SpeakerLayout::preset("7.1.4").unwrap();
        SpatialRenderer::new(RendererSpec {
            vbap_position_interpolation: true, // position interpolation → trilinear lookup + per-sample motion
            ..test_support::spec(layout)
        })
        .unwrap()
    }

    let pcm = vec![0.5f32; 40];
    let event_at = |position: [f64; 3]| {
        vec![SpatialChannelEvent {
            channel_idx: 0,
            is_bed: false,
            gain_db: Some(0.0),
            ramp_length: Some(40),
            size: Some([0.0, 0.0, 0.0]),
            position: Some(position),
            sample_pos: Some(0),
        }]
    };
    let block_a = event_at([-0.7, 0.5, 0.2]);
    let block_b = event_at([0.8, -0.6, 0.5]);

    let render = |mode: RampMode| -> Vec<f32> {
        let mut r = build();
        r.control.live.write().options.ramp_mode = mode;
        // First block establishes a position (and seeds Interp's start gains).
        r.render_frame(&pcm, 1, &block_a, Vec::new(), false)
            .unwrap();
        // Second block moves the object — this is what we compare.
        r.render_frame(&pcm, 1, &block_b, Vec::new(), false)
            .unwrap()
            .samples
    };

    let off = render(RampMode::Off);
    let frame = render(RampMode::Frame);
    let sample = render(RampMode::Sample);
    let interp = render(RampMode::Interp);

    let expected_len = 40 * 12;
    for (name, out) in [
        ("off", &off),
        ("frame", &frame),
        ("sample", &sample),
        ("interp", &interp),
    ] {
        assert_eq!(out.len(), expected_len, "{name}: wrong output length");
        assert!(
            out.iter().all(|x| x.is_finite()),
            "{name}: non-finite output"
        );
        let energy: f32 = out.iter().map(|x| x * x).sum();
        assert!(energy > 0.0, "{name}: produced silence");
    }

    let max_diff = |a: &[f32], b: &[f32]| {
        a.iter()
            .zip(b)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0f32, f32::max)
    };

    assert!(max_diff(&off, &frame) > 1e-3, "Off vs Frame collapsed");
    assert!(max_diff(&off, &sample) > 1e-3, "Off vs Sample collapsed");
    assert!(max_diff(&off, &interp) > 1e-3, "Off vs Interp collapsed");
    assert!(
        max_diff(&frame, &sample) > 1e-3,
        "Frame vs Sample collapsed"
    );
    // Sample (position-space) and Interp (gain-space) interpolate the same
    // endpoints differently, so they diverge mid-block too.
    assert!(
        max_diff(&sample, &interp) > 1e-3,
        "Sample vs Interp collapsed"
    );
}

/// Regression: in binaural mode the object position ramps MUST advance.
/// The VBAP mix loop that normally drives `advance_ramp` is bypassed, so the
/// binaural branch advances them itself; before that fix every object stayed
/// at the ramp default [0,0,0] — dead centre, and rotation-invariant (the
/// zero vector ignores the head pose) — which rendered as near-mono audio
/// that did not react to head tracking.
#[test]
fn binaural_object_ramp_advances_and_lateralizes() {
    let layout = SpeakerLayout::preset("7.1.4").unwrap();
    let mut r = SpatialRenderer::new(test_support::spec(layout)).unwrap();
    r.control.live.write().binaural.output_mode = crate::live_params::OutputMode::Binaural;

    // One object channel ramping from the default [0,0,0] to hard right.
    // Broadband pseudo-noise input: head-shadow ILD is a high-frequency
    // phenomenon, so a DC input would show almost no ear asymmetry.
    let mut lcg: u32 = 0x1234_5678;
    let mut noise_block = move || -> Vec<f32> {
        (0..40)
            .map(|_| {
                lcg = lcg.wrapping_mul(1664525).wrapping_add(1013904223);
                (lcg >> 8) as f32 / (1u32 << 24) as f32 - 0.5
            })
            .collect()
    };
    let pcm = noise_block();
    let event = vec![SpatialChannelEvent {
        channel_idx: 0,
        is_bed: false,
        gain_db: Some(0.0),
        ramp_length: Some(40),
        size: Some([0.0, 0.0, 0.0]),
        position: Some([1.0, 0.0, 0.0]),
        sample_pos: Some(0),
    }];

    let first = r.render_frame(&pcm, 1, &event, Vec::new(), false).unwrap();
    assert_eq!(
        first.samples.len(),
        40 * 2,
        "binaural output must be stereo"
    );

    // Let the ramp finish and the ITD delay lines / HRIR tails settle, then
    // measure ear energies over a few blocks.
    let (mut e_l, mut e_r) = (0.0f32, 0.0f32);
    for i in 0..8 {
        let pcm = noise_block();
        let out = r.render_frame(&pcm, 1, &[], Vec::new(), false).unwrap();
        if i >= 4 {
            for s in out.samples.chunks_exact(2) {
                e_l += s[0] * s[0];
                e_r += s[1] * s[1];
            }
        }
    }

    let pos = r
        .channel_states
        .get(0)
        .expect("channel state")
        .ramp
        .current_position;
    assert!(
        pos[0] > 0.99,
        "object ramp did not advance in binaural mode: current_position = {pos:?}"
    );
    assert!(e_l + e_r > 0.0, "binaural output is silent");
    assert!(
        e_r > 1.5 * e_l,
        "hard-right object not lateralized: E_L={e_l} E_R={e_r}"
    );
}

/// Regression: the master gain must scale the binaural output exactly like
/// it scales the speaker path (it used to be applied only in the VBAP
/// branch, so the master control was inert on headphones).
#[test]
fn binaural_output_follows_master_gain() {
    fn build() -> SpatialRenderer {
        let layout = SpeakerLayout::preset("7.1.4").unwrap();
        SpatialRenderer::new(test_support::spec(layout)).unwrap()
    }

    let pcm: Vec<f32> = (0..40).map(|i| (i * 7 % 13) as f32 / 13.0 - 0.5).collect();
    let event = vec![SpatialChannelEvent {
        channel_idx: 0,
        is_bed: false,
        gain_db: Some(0.0),
        ramp_length: Some(40),
        size: Some([0.0, 0.0, 0.0]),
        position: Some([0.5, 1.0, 0.0]),
        sample_pos: Some(0),
    }];

    let render = |master: f32| -> Vec<f32> {
        let mut r = build();
        {
            let mut live = r.control.live.write();
            live.binaural.output_mode = crate::live_params::OutputMode::Binaural;
            live.master_gain = master;
        }
        let mut out = Vec::new();
        for i in 0..4 {
            let ev: &[SpatialChannelEvent] = if i == 0 { &event } else { &[] };
            out = r
                .render_frame(&pcm, 1, ev, Vec::new(), false)
                .unwrap()
                .samples;
        }
        out
    };

    let unity = render(1.0);
    let double = render(2.0);
    assert!(unity.iter().any(|x| x.abs() > 1e-6), "silent baseline");
    for (a, b) in unity.iter().zip(&double) {
        assert!(
            (b - a * 2.0).abs() <= a.abs() * 1e-4 + 1e-6,
            "master gain not applied: {a} vs {b}"
        );
    }
}

/// The binaural ears carry dedicated live params (they used to ride the
/// first two speaker slots, which now belong to the virtual FL/FR rows in
/// cascaded mode): muting ear 0 must silence the left ear and leave the
/// right ear untouched.
#[test]
fn binaural_ear_mute_uses_dedicated_ear_params() {
    let layout = SpeakerLayout::preset("7.1.4").unwrap();
    let mut r = SpatialRenderer::new(test_support::spec(layout)).unwrap();
    {
        let mut live = r.control.live.write();
        live.binaural.output_mode = crate::live_params::OutputMode::Binaural;
        live.binaural.ears[0].muted = true;
    }

    let pcm: Vec<f32> = (0..40).map(|i| (i * 7 % 13) as f32 / 13.0 - 0.5).collect();
    let event = vec![SpatialChannelEvent {
        channel_idx: 0,
        is_bed: false,
        gain_db: Some(0.0),
        ramp_length: Some(40),
        size: Some([0.0, 0.0, 0.0]),
        position: Some([0.0, 1.0, 0.0]),
        sample_pos: Some(0),
    }];
    let mut out = Vec::new();
    for i in 0..4 {
        let ev: &[SpatialChannelEvent] = if i == 0 { &event } else { &[] };
        out = r
            .render_frame(&pcm, 1, ev, Vec::new(), false)
            .unwrap()
            .samples;
    }
    let e_l: f32 = out.iter().step_by(2).map(|x| x * x).sum();
    let e_r: f32 = out.iter().skip(1).step_by(2).map(|x| x * x).sum();
    assert!(e_l == 0.0, "left ear not silenced: {e_l}");
    assert!(e_r > 1e-6, "right ear should still play: {e_r}");
}

/// Binaural mode must carry the speaker path's overload policy (issue #149):
/// the clip flag always fires above 0 dBFS (with the ear in the first two
/// speaker slots, the same slots the headphone L/R rows ride), and auto-gain
/// folds the correction into the shared master gain.
#[test]
fn binaural_clipping_flags_ear_and_auto_gain_reduces_master() {
    fn build() -> SpatialRenderer {
        let layout = SpeakerLayout::preset("7.1.4").unwrap();
        SpatialRenderer::new(test_support::spec(layout)).unwrap()
    }

    let pcm: Vec<f32> = (0..40).map(|i| (i * 7 % 13) as f32 / 13.0 - 0.5).collect();
    let event = vec![SpatialChannelEvent {
        channel_idx: 0,
        is_bed: false,
        gain_db: Some(0.0),
        ramp_length: Some(40),
        size: Some([0.0, 0.0, 0.0]),
        position: Some([0.5, 1.0, 0.0]),
        sample_pos: Some(0),
    }];

    let hot = |auto_gain: bool| -> SpatialRenderer {
        let r = build();
        let mut live = r.control.live.write();
        live.binaural.output_mode = crate::live_params::OutputMode::Binaural;
        // Hot enough that the HRIR-summed stereo bus exceeds 0 dBFS.
        live.master_gain = 16.0;
        live.options.auto_gain = auto_gain;
        drop(live);
        r
    };
    let frames = |r: &mut SpatialRenderer, n: usize| {
        for i in 0..n {
            let ev: &[SpatialChannelEvent] = if i == 0 { &event } else { &[] };
            r.render_frame(&pcm, 1, ev, Vec::new(), false).unwrap();
        }
    };
    let render = |auto_gain: bool| -> SpatialRenderer {
        let mut r = hot(auto_gain);
        frames(&mut r, 4);
        r
    };

    // Auto-gain off: the flag must still fire (UI indicators), the master
    // gain must stay untouched.
    let r = render(false);
    let clip = r.control.take_clip_pending();
    assert!(
        matches!(clip, Some(0) | Some(1)),
        "clip flag not raised for an ear slot: {clip:?}"
    );
    assert!(!r.auto_gain_triggered(), "auto-gain fired while disabled");
    assert_eq!(r.control.live.read().master_gain, 16.0);

    // Auto-gain on: correction folded into the master gain, trigger visible.
    let r = render(true);
    assert!(
        matches!(r.control.take_clip_pending(), Some(0) | Some(1)),
        "clip flag not raised with auto-gain on"
    );
    assert!(r.auto_gain_triggered(), "auto-gain did not trigger");
    let master = r.control.live.read().master_gain;
    assert!(
        master < 16.0,
        "master gain not reduced by auto-gain: {master}"
    );

    // A control write in progress (#670): the render thread does not wait
    // for it — on one thread, waiting would never end — it skips the fold,
    // and a clipping frame after the write folds instead.
    let mut r = hot(true);
    let control = r.renderer_control();
    let held = control.live.write();
    frames(&mut r, 4);
    assert!(
        matches!(r.control.take_clip_pending(), Some(0) | Some(1)),
        "clip flag not raised while a write is held"
    );
    assert!(!r.auto_gain_triggered(), "folded through a held write");
    drop(held);
    frames(&mut r, 1);
    assert!(
        r.auto_gain_triggered(),
        "auto-gain did not fold after the write"
    );
    assert!(r.control.live.read().master_gain < 16.0);
}

/// In binaural mode a bed mapped to a `spatialize: false` speaker (the LFE)
/// keeps its direct-routing intent (issue #156): both ears receive the
/// identical dry feed at constant power — no HRIR tail, no ITD, no head-pose
/// effect — instead of being HRTF-spatialized at the sub's direction.
#[test]
fn binaural_lfe_bed_feeds_both_ears_equally_and_dry() {
    let layout = SpeakerLayout::preset("7.1.4").unwrap();
    let mut r = SpatialRenderer::new(test_support::spec(layout)).unwrap();
    // Channel 0 = direct LFE → the LFE speaker (index 3, spatialize:false).
    r.configure_channel_routing(&[ChannelRoute::Direct(bridge_api::RChannelLabel::LFE)]);
    {
        let mut live = r.control.live.write();
        live.binaural.output_mode = crate::live_params::OutputMode::Binaural;
    }

    let n = 40;
    let mut pcm = vec![0.0f32; n];
    pcm[0] = 0.8; // impulse: any post-render tail would expose a convolver
    let event = vec![SpatialChannelEvent {
        channel_idx: 0,
        is_bed: true,
        gain_db: Some(0.0),
        ramp_length: Some(0),
        size: None,
        position: None,
        sample_pos: Some(0),
    }];
    // Warm up the per-channel gain slew (channels fade in over
    // GAIN_SLEW_SECS from silence) with one long silent block, so the
    // asserted block runs at settled gain.
    let warmup = vec![0.0f32; 4096];
    r.render_frame(&warmup, 1, &event, Vec::new(), false)
        .unwrap();
    let out = r
        .render_frame(&pcm, 1, &event, Vec::new(), false)
        .unwrap()
        .samples;

    assert_eq!(out.len(), n * 2);
    let expected = 0.8 * std::f32::consts::FRAC_1_SQRT_2;
    assert!(
        (out[0] - expected).abs() < 1e-6,
        "constant-power direct feed expected {expected}, got {}",
        out[0]
    );
    assert_eq!(out[0], out[1], "both ears must carry the identical feed");
    assert!(
        out[2..].iter().all(|&x| x == 0.0),
        "direct feed must not ring (no HRIR/ITD tail)"
    );
}

/// After claiming the FP environment, subnormal arithmetic must flush to
/// zero on this thread (issue #154) — without FTZ/DAZ the product below
/// stays a nonzero subnormal.
#[test]
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn render_thread_flushes_denormals() {
    ensure_denormals_flushed();
    // MIN_POSITIVE is the smallest *normal*; dividing it makes a subnormal.
    let tiny = std::hint::black_box(f32::MIN_POSITIVE) / std::hint::black_box(4.0f32);
    let prod = std::hint::black_box(tiny) * std::hint::black_box(1.0f32);
    assert_eq!(
        prod, 0.0,
        "subnormals must flush to zero on the render thread (got {prod:e})"
    );
}

/// A layout without back floor speakers (5.1.4-style: sides + four tops) must
/// still render content placed at the back floor corners — the direction sits
/// outside the speaker hull (below the SL↔TBL / SR↔TBR faces) and must fold
/// onto the closest hull face, never to silence (issue #169). Mirrors the field
/// config: precomputed cartesian table, zero-length ramp, steady state.
#[test]
fn back_floor_position_renders_on_layout_without_back_speakers() {
    const LAYOUT_5_1_4: &str = r#"
radius_m: 1.0
speakers:
- { name: FL,  coord_mode: cartesian, x: -1.0, y:  1.0, z: 0.0, spatialize: true }
- { name: FR,  coord_mode: cartesian, x:  1.0, y:  1.0, z: 0.0, spatialize: true }
- { name: C,   coord_mode: cartesian, x:  0.0, y:  1.0, z: 0.0, spatialize: true }
- { name: LFE, coord_mode: cartesian, x:  1.0, y:  1.0, z: -1.0, spatialize: false }
- { name: SL,  coord_mode: cartesian, x: -1.0, y:  0.0, z: 0.0, spatialize: true }
- { name: SR,  coord_mode: cartesian, x:  1.0, y:  0.0, z: 0.0, spatialize: true }
- { name: TFL, coord_mode: cartesian, x: -1.0, y:  1.0, z: 1.0, spatialize: true }
- { name: TFR, coord_mode: cartesian, x:  1.0, y:  1.0, z: 1.0, spatialize: true }
- { name: TBL, coord_mode: cartesian, x: -1.0, y: -1.0, z: 1.0, spatialize: true }
- { name: TBR, coord_mode: cartesian, x:  1.0, y: -1.0, z: 1.0, spatialize: true }
"#;

    fn build() -> SpatialRenderer {
        SpatialRenderer::new(RendererSpec {
            el_res_deg: 90,
            spread_resolution: 0.25,
            table_mode: VbapTableMode::Cartesian {
                x_size: 63,
                y_size: 63,
                z_size: 16,
                z_neg_size: 0,
            },
            distance_model: DistanceModel::None,
            room_ratio: [2.0, 2.0, 1.0],
            room_ratio_rear: 1.0,
            room_ratio_lower: 0.466667,
            room_ratio_center_blend: 0.5,
            use_loudness: true,
            distance_diffuse: true,
            cartesian_default_x_size: 63,
            cartesian_default_y_size: 63,
            cartesian_default_z_size: 16,
            cartesian_default_z_neg_size: 0,
            ..test_support::spec(SpeakerLayout::from_yaml_str(LAYOUT_5_1_4).unwrap())
        })
        .unwrap()
    }

    let pcm: Vec<f32> = (0..40)
        .map(|i| if i % 2 == 0 { 0.5 } else { -0.5 })
        .collect();
    // Steady-state per-speaker peaks: apply the event, then measure a second
    // frame so a ramp (if any) cannot mask a zero steady-state gain.
    let peaks_at = |pos: [f64; 3]| -> Vec<f32> {
        let event = vec![SpatialChannelEvent {
            channel_idx: 0,
            is_bed: false,
            gain_db: Some(0.0),
            ramp_length: Some(0),
            size: None,
            position: Some(pos),
            sample_pos: Some(0),
        }];
        let mut r = build();
        r.render_frame(&pcm, 1, &event, Vec::new(), false).unwrap();
        let out = r.render_frame(&pcm, 1, &[], Vec::new(), false).unwrap();
        let n = 10usize;
        let mut peaks = vec![0.0f32; n];
        for (k, &s) in out.samples.iter().enumerate() {
            let c = k % n;
            peaks[c] = peaks[c].max(s.abs());
        }
        peaks
    };

    // Control: the exact side position renders on SL.
    let side = peaks_at([-1.0, 0.0, 0.0]);
    assert!(
        side[4] > 1e-3,
        "side-left content must render on SL (peaks {side:?})"
    );

    // Back floor corners: outside the hull; must fold onto the nearest face
    // (side surround and/or top back of that side), never to silence.
    for (pos, near) in [
        ([-1.0f64, -1.0, 0.0], [4usize, 8]), // SL / TBL
        ([1.0, -1.0, 0.0], [5, 9]),          // SR / TBR
    ] {
        let peaks = peaks_at(pos);
        let near_peak = near.iter().map(|&c| peaks[c]).fold(0.0f32, f32::max);
        let total: f32 = peaks.iter().sum();
        assert!(
            near_peak > 1e-3,
            "back-floor content at {pos:?} must fold onto the near speakers (peaks {peaks:?})"
        );
        assert!(
            near_peak >= total * 0.5,
            "back-floor fold at {pos:?} should stay local (peaks {peaks:?})"
        );
    }
}

/// Builder shared by the cascaded-binaural tests: 7.1.4 main layout, same
/// arguments as the other binaural tests in this file, evaluation mode
/// selectable (the equivalence test needs `Realtime` so VBAP is exactly
/// one-hot at a virtual speaker direction, without table interpolation).
///
/// `neutral_room` builds with an identity room mapping and no distance
/// attenuation: the cascade honours the full speaker-path options (that is
/// its point), so only under neutral settings does it collapse to the direct
/// per-object render at a virtual speaker direction.
fn build_cascade_test_renderer(eval: LiveEvaluationMode, neutral_room: bool) -> SpatialRenderer {
    let layout = SpeakerLayout::preset("7.1.4").unwrap();
    let (distance_model, room_ratio, rear, lower) = if neutral_room {
        (DistanceModel::None, [1.0f32, 1.0, 1.0], 1.0f32, 1.0f32)
    } else {
        (DistanceModel::Linear, [1.0f32, 2.0, 0.5], 2.0f32, 0.5f32)
    };
    SpatialRenderer::new(RendererSpec {
        distance_model,
        room_ratio,
        room_ratio_rear: rear,
        room_ratio_lower: lower,
        initial_evaluation_mode: eval,
        ..test_support::spec(layout)
    })
    .unwrap()
}

/// The cascaded stage must build its virtual topology lazily on the first
/// active frame, produce stereo, and preserve lateralization: a hard-right
/// object pans onto right-side virtual speakers, whose HRIRs favour the
/// right ear.
#[test]
fn cascaded_binaural_builds_stage_and_lateralizes() {
    let mut r = build_cascade_test_renderer(LiveEvaluationMode::PrecomputedCartesian, false);
    {
        let mut live = r.control.live.write();
        live.binaural.output_mode = crate::live_params::OutputMode::Binaural;
        live.binaural.mode = crate::live_params::BinauralMode::Cascaded;
    }

    let mut lcg: u32 = 0x1234_5678;
    let mut noise_block = move || -> Vec<f32> {
        (0..40)
            .map(|_| {
                lcg = lcg.wrapping_mul(1664525).wrapping_add(1013904223);
                (lcg >> 8) as f32 / (1u32 << 24) as f32 - 0.5
            })
            .collect()
    };
    let pcm = noise_block();
    let event = vec![SpatialChannelEvent {
        channel_idx: 0,
        is_bed: false,
        gain_db: Some(0.0),
        ramp_length: Some(40),
        size: Some([0.0, 0.0, 0.0]),
        position: Some([1.0, 0.0, 0.0]),
        sample_pos: Some(0),
    }];

    let first = r.render_frame(&pcm, 1, &event, Vec::new(), false).unwrap();
    assert_eq!(
        first.samples.len(),
        40 * 2,
        "cascaded binaural output must be stereo"
    );
    assert!(
        r.cascade.is_some(),
        "cascaded stage must be built on the first active frame"
    );
    let stage = r.cascade.as_ref().unwrap();
    assert_eq!(
        stage.num_buses(),
        12,
        "the virtual room must mirror the app layout (7.1.4 = 12 speakers)"
    );
    assert_eq!(
        stage.bin_direct.iter().filter(|&&d| d).count(),
        1,
        "exactly the LFE entry must bypass the HRTF as a direct bus"
    );

    let (mut e_l, mut e_r) = (0.0f32, 0.0f32);
    for i in 0..30 {
        let pcm = noise_block();
        let out = r.render_frame(&pcm, 1, &[], Vec::new(), false).unwrap();
        if i >= 26 {
            for s in out.samples.chunks_exact(2) {
                e_l += s[0] * s[0];
                e_r += s[1] * s[1];
            }
        }
    }
    assert!(e_l + e_r > 0.0, "cascaded binaural output is silent");
    assert!(
        e_r > 1.5 * e_l,
        "hard-right object not lateralized through the cascade: E_L={e_l} E_R={e_r}"
    );
}

/// A point source sitting exactly on a virtual speaker direction must render
/// (near-)identically through the cascade and the direct per-object path:
/// realtime VBAP is one-hot there, so the cascade collapses to the same
/// single HRIR pair the direct path convolves.
#[test]
fn cascaded_matches_direct_at_virtual_speaker_direction() {
    // cascade-12 "FL": azimuth −45°, elevation 0 — [−√2/2, √2/2, 0] in ADM.
    let pos = [
        -std::f64::consts::FRAC_1_SQRT_2,
        std::f64::consts::FRAC_1_SQRT_2,
        0.0,
    ];
    let render = |cascaded: bool| -> Vec<f32> {
        let mut r = build_cascade_test_renderer(LiveEvaluationMode::Realtime, true);
        {
            let mut live = r.control.live.write();
            live.binaural.output_mode = crate::live_params::OutputMode::Binaural;
            live.binaural.mode = if cascaded {
                crate::live_params::BinauralMode::Cascaded
            } else {
                crate::live_params::BinauralMode::Direct
            };
        }
        let event = vec![SpatialChannelEvent {
            channel_idx: 0,
            is_bed: false,
            gain_db: Some(0.0),
            ramp_length: Some(0),
            size: Some([0.0, 0.0, 0.0]),
            position: Some(pos),
            sample_pos: Some(0),
        }];
        // Deterministic pseudo-noise, same seed for both runs.
        let mut lcg: u32 = 0xBEEF_CAFE;
        let mut out = Vec::new();
        for i in 0..40 {
            let pcm: Vec<f32> = (0..40)
                .map(|_| {
                    lcg = lcg.wrapping_mul(1664525).wrapping_add(1013904223);
                    (lcg >> 8) as f32 / (1u32 << 24) as f32 - 0.5
                })
                .collect();
            let ev: &[SpatialChannelEvent] = if i == 0 { &event } else { &[] };
            let f = r.render_frame(&pcm, 1, ev, Vec::new(), false).unwrap();
            // Compare well after the gain slew (20 ms ≈ 24 blocks) and the
            // HRIR fade-in have settled.
            if i >= 30 {
                out.extend_from_slice(&f.samples);
            }
        }
        out
    };

    let direct = render(false);
    let cascaded = render(true);
    assert_eq!(direct.len(), cascaded.len());
    let energy: f32 = direct.iter().map(|x| x * x).sum();
    assert!(energy > 0.0, "silent baseline");
    let err: f32 = direct
        .iter()
        .zip(&cascaded)
        .map(|(a, b)| (a - b) * (a - b))
        .sum();
    assert!(
        err <= energy * 1e-4,
        "cascade must collapse to the direct render at a virtual speaker \
         direction: relative error {}",
        (err / energy).sqrt()
    );
}

/// Master gain must scale the cascaded output exactly like every other path
/// (applied once, on the final stereo — never inside the virtual mix too).
#[test]
fn cascaded_binaural_follows_master_gain() {
    let pcm: Vec<f32> = (0..40).map(|i| (i * 7 % 13) as f32 / 13.0 - 0.5).collect();
    let event = vec![SpatialChannelEvent {
        channel_idx: 0,
        is_bed: false,
        gain_db: Some(0.0),
        ramp_length: Some(40),
        size: Some([0.0, 0.0, 0.0]),
        position: Some([0.5, 1.0, 0.0]),
        sample_pos: Some(0),
    }];

    let render = |master: f32| -> Vec<f32> {
        let mut r = build_cascade_test_renderer(LiveEvaluationMode::PrecomputedCartesian, false);
        {
            let mut live = r.control.live.write();
            live.binaural.output_mode = crate::live_params::OutputMode::Binaural;
            live.binaural.mode = crate::live_params::BinauralMode::Cascaded;
            live.master_gain = master;
        }
        let mut out = Vec::new();
        for i in 0..4 {
            let ev: &[SpatialChannelEvent] = if i == 0 { &event } else { &[] };
            out = r
                .render_frame(&pcm, 1, ev, Vec::new(), false)
                .unwrap()
                .samples;
        }
        out
    };

    let unity = render(1.0);
    let double = render(2.0);
    assert!(unity.iter().any(|x| x.abs() > 1e-6), "silent baseline");
    for (a, b) in unity.iter().zip(&double) {
        assert!(
            (b - a * 2.0).abs() <= a.abs() * 1e-4 + 1e-6,
            "master gain must scale the cascade exactly once: {a} vs {b}"
        );
    }
}

/// An LFE-routed bed must bypass the virtual pan onto the direct bus and keep
/// the LFE policy: both ears equal at −3 dB, no HRTF colouration.
#[test]
fn cascaded_lfe_routes_direct_to_both_ears() {
    let mut r = build_cascade_test_renderer(LiveEvaluationMode::PrecomputedCartesian, false);
    {
        let mut live = r.control.live.write();
        live.binaural.output_mode = crate::live_params::OutputMode::Binaural;
        live.binaural.mode = crate::live_params::BinauralMode::Cascaded;
    }
    r.configure_channel_routing(&[ChannelRoute::Direct(bridge_api::RChannelLabel::LFE)]);

    let event = vec![SpatialChannelEvent {
        channel_idx: 0,
        is_bed: true,
        gain_db: Some(0.0),
        ramp_length: Some(0),
        size: None,
        position: None,
        sample_pos: Some(0),
    }];
    const IN: f32 = 0.5;
    let pcm = vec![IN; 40];
    let mut out = Vec::new();
    // Render past the 20 ms gain slew (24 blocks of 40 samples).
    for i in 0..30 {
        let ev: &[SpatialChannelEvent] = if i == 0 { &event } else { &[] };
        out = r
            .render_frame(&pcm, 1, ev, Vec::new(), false)
            .unwrap()
            .samples;
    }
    let expected = IN * std::f32::consts::FRAC_1_SQRT_2;
    for s in out.chunks_exact(2) {
        assert!(
            (s[0] - s[1]).abs() < 1e-6,
            "LFE must feed both ears equally: L={} R={}",
            s[0],
            s[1]
        );
        assert!(
            (s[0] - expected).abs() < 1e-4,
            "LFE must land at −3 dB dry: got {} expected {expected}",
            s[0]
        );
    }
}

/// Regression for the shared-`ChannelState` width hazard: `RampMode::Interp`
/// caches per-band gains sized to the mixing layout; switching the active mix
/// stage between the physical layout (12) and the virtual cascade layout (13)
/// must re-seed them instead of indexing stale widths (which used to be an
/// out-of-bounds panic risk). Round-trips speaker → cascaded binaural →
/// speaker with a live object under Interp and checks output stays sane.
#[test]
fn interp_survives_speaker_cascade_width_switch() {
    let mut r = build_cascade_test_renderer(LiveEvaluationMode::PrecomputedCartesian, false);
    {
        let ctrl = r.control.clone();
        let mut live = ctrl.live.write();
        live.options.ramp_mode = crate::live_params::RampMode::Interp;
        live.binaural.mode = crate::live_params::BinauralMode::Cascaded;
    }
    let pcm: Vec<f32> = (0..40).map(|i| (i * 7 % 13) as f32 / 13.0 - 0.5).collect();
    let event = vec![SpatialChannelEvent {
        channel_idx: 0,
        is_bed: false,
        gain_db: Some(0.0),
        ramp_length: Some(40),
        size: Some([0.0, 0.0, 0.0]),
        position: Some([0.4, 0.6, 0.2]),
        sample_pos: Some(0),
    }];

    let set_mode = |r: &mut SpatialRenderer, mode: crate::live_params::OutputMode| {
        r.control.live.write().binaural.output_mode = mode;
    };
    // Seed interp state on the 12-wide speaker path.
    let out = r.render_frame(&pcm, 1, &event, Vec::new(), false).unwrap();
    assert_eq!(out.samples.len(), 40 * 12);
    // Switch to the 13-wide cascade, render, and back — must not panic and
    // must keep producing signal. A mode change is deliberately not instant:
    // the old path keeps rendering while it fades out (see `OutputModeFade`),
    // so pump frames until the new width appears.
    set_mode(&mut r, crate::live_params::OutputMode::Binaural);
    let out = render_until_width(&mut r, &pcm, 40 * 2);
    assert_eq!(out.samples.len(), 40 * 2, "cascade output must be stereo");
    set_mode(&mut r, crate::live_params::OutputMode::SpeakerArray);
    let out = render_until_width(&mut r, &pcm, 40 * 12);
    assert_eq!(out.samples.len(), 40 * 12);
    assert!(
        out.samples.iter().any(|s| s.abs() > 1e-6),
        "speaker output must survive the round-trip"
    );
}

// TODO: Add integration test with real spatial metadata
// For now, testing is done via real spatial audio content decoding

/// A headphone request made before the first frame (a config read after the
/// renderer was built) is the width the host is told, and the width that
/// frame comes out at: the first render takes the request without a fade, so
/// sizing the sink for the speakers lost the opening block to a rebuild.
#[test]
fn width_before_the_first_frame_is_the_width_it_renders_at() {
    let mut r = renderer_for_layout(SpeakerLayout::preset("7.1.4").unwrap());
    assert_eq!(r.output_channel_count(), 12);
    r.control.live.write().binaural.output_mode = crate::live_params::OutputMode::Binaural;
    assert_eq!(r.output_channel_count(), 2, "the width the host sizes from");
    assert!(!r.output_is_speaker_array());
    assert_eq!(r.output_channel_names(), ["FL", "FR"]);
    let pcm = vec![0.25f32; 40];
    let event = vec![SpatialChannelEvent {
        channel_idx: 0,
        is_bed: false,
        gain_db: Some(0.0),
        ramp_length: Some(40),
        size: Some([0.0, 0.0, 0.0]),
        position: Some([0.0, 1.0, 0.0]),
        sample_pos: Some(0),
    }];
    let out = r.render_frame(&pcm, 1, &event, Vec::new(), false).unwrap();
    assert_eq!(out.n_channels, 2, "the width the first frame renders at");
    // From then on a request goes through the fade: the width stays the
    // rendered one until the fade swaps the chains.
    r.control.live.write().binaural.output_mode = crate::live_params::OutputMode::SpeakerArray;
    assert_eq!(r.output_channel_count(), 2);
}

/// Render until the output-mode cross-fade has settled at `expect_samples`.
///
/// A live mode change is deferred: the outgoing path keeps rendering while it
/// ramps to silence, so the new channel width only appears a few blocks later
/// (see `OutputModeFade`). Every frame in between is still self-consistent —
/// that is the invariant the fade protects — so this just pumps until the width
/// changes, and fails loudly rather than looping forever.
fn render_until_width(
    r: &mut SpatialRenderer,
    pcm: &[f32],
    expect_samples: usize,
) -> RenderedFrame {
    for _ in 0..64 {
        let out = r.render_frame(pcm, 1, &[], Vec::new(), false).unwrap();
        assert_eq!(
            out.samples.len(),
            out.n_channels * (pcm.len() / 1),
            "every frame must describe its own geometry, mid-fade included"
        );
        if out.samples.len() == expect_samples {
            return out;
        }
    }
    panic!("output-mode cross-fade never settled at {expect_samples} samples");
}

/// A rendered frame must describe its own geometry, so a live output-mode flip
/// cannot make the caller mis-read it.
///
/// The engine used to pair `rendered.samples` with a freshly-read
/// `output_channel_count()`. The OSC listener flips that mode on another
/// thread, so a switch to speakers landing after a binaural render published
/// "12 channels" for 2-channel data; the host then copied `n_frames * 12`
/// floats out of a buffer holding a sixth of that, reading adjacent heap and
/// playing it as PCM. Only on the way back to speakers, because that is the
/// direction where the count grows.
#[test]
fn rendered_frame_reports_the_geometry_it_produced() {
    let mut renderer = SpatialRenderer::new(RendererSpec {
        vbap_position_interpolation: false,
        ..test_support::spec(SpeakerLayout::preset("7.1.4").unwrap())
    })
    .unwrap();

    let frames = 40;
    let pcm: Vec<f32> = (0..frames)
        .map(|i| (i * 7 % 13) as f32 / 13.0 - 0.5)
        .collect();
    let event = vec![SpatialChannelEvent {
        channel_idx: 0,
        is_bed: false,
        gain_db: Some(0.0),
        ramp_length: Some(frames as u32),
        size: Some([0.0, 0.0, 0.0]),
        position: Some([0.3, -0.2, 0.4]),
        sample_pos: Some(0),
    }];

    let speakers = renderer.num_speakers();
    assert!(
        speakers > 2,
        "7.1.4 must have more channels than a stereo pair"
    );

    let spk = renderer
        .render_frame(&pcm, 1, &event, Vec::new(), false)
        .unwrap();
    assert_eq!(
        spk.n_channels, speakers,
        "speaker render reports its own width"
    );
    assert_eq!(spk.samples.len(), frames * spk.n_channels);

    renderer.control.live.write().binaural.output_mode = crate::live_params::OutputMode::Binaural;

    // The switch is deferred by the cross-fade, so the width changes a few
    // blocks later. `render_until_width` asserts the geometry invariant on every
    // frame it pumps, mid-fade ones included.
    let bin = render_until_width(&mut renderer, &pcm, frames * 2);
    assert_eq!(bin.n_channels, 2, "binaural render is a stereo ear pair");
    assert_eq!(bin.samples.len(), frames * bin.n_channels);

    // The regression: flipping back to speakers after the render must not
    // retroactively change what this frame says about itself.
    renderer.control.live.write().binaural.output_mode =
        crate::live_params::OutputMode::SpeakerArray;
    assert_eq!(
        bin.n_channels, 2,
        "a completed binaural frame must keep reporting 2 channels even though \
         the live mode now says speakers — this is exactly the mismatch that \
         made the host read past the end of the buffer"
    );
    assert_eq!(bin.samples.len(), frames * bin.n_channels);
}

/// What Studio does to a playing renderer: the layout swapped for one with
/// fewer, then more speakers, and the stream's sample rate changed. The
/// output keeps the width it was opened with: a smaller layout fills its
/// first channels, a larger one is refused with a reason for the clients and
/// the previous layout keeps playing (its gains would land past the stage's
/// gain sets). Every frame stays finite and the renderer keeps sounding; a
/// per-speaker setting left on a speaker the new layout does not have is
/// ignored, not a crash.
#[test]
fn a_playing_renderer_survives_layout_and_sample_rate_changes() {
    const FRAMES: usize = 480;
    const WIDTH: usize = 12;
    let mut r = renderer_for_layout(SpeakerLayout::preset("7.1.4").unwrap());
    let control = r.renderer_control();
    let event = SpatialChannelEvent {
        channel_idx: 0,
        is_bed: false,
        gain_db: Some(0.0),
        ramp_length: Some(0),
        size: Some([0.0, 0.0, 0.0]),
        position: Some([0.3, 0.8, 0.2]),
        sample_pos: Some(0),
    };
    let mut block = 0;
    // Render until a frame sounds on channels `from..used` and on none past
    // `used`, checking every frame on the way. (The object sounds on a
    // height speaker of 7.1.4, channel 9: `from` 6 tells 7.1.4 from 5.1.)
    // The bands are built on a worker thread: wait for it by the clock, not
    // by a count of blocks, so a slow runner is not reported as a failure.
    const PATIENCE: std::time::Duration = std::time::Duration::from_secs(20);
    let mut play_until = |r: &mut SpatialRenderer, from: usize, used: usize| {
        let deadline = std::time::Instant::now() + PATIENCE;
        while std::time::Instant::now() < deadline {
            // The object's metadata in every block, as a stream carries it: a
            // sample-rate change is a new stream and resets what it knew.
            let events = std::slice::from_ref(&event);
            let frame = r
                .render_frame(&noise_block(1, FRAMES, block), 1, events, Vec::new(), false)
                .unwrap();
            block += 1;
            assert_eq!(
                frame.n_channels, WIDTH,
                "the output keeps the width it was opened with"
            );
            assert_eq!(frame.samples.len(), FRAMES * WIDTH);
            assert!(
                frame.samples.iter().all(|s| s.is_finite()),
                "non-finite output"
            );
            let energy = |c: usize| {
                frame
                    .samples
                    .iter()
                    .skip(c)
                    .step_by(WIDTH)
                    .map(|x| x * x)
                    .sum::<f32>()
            };
            if (from..used).any(|c| energy(c) > 0.0) && (used..WIDTH).all(|c| energy(c) == 0.0) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("the renderer never settled on channels {from}..{used}");
    };
    let publish = |preset: &str| {
        control.with_editable_layout(|l| *l = SpeakerLayout::preset(preset).unwrap());
        control.bump_geometry_generation();
        let plan = control.prepare_topology_rebuild().expect("plan");
        let topology = plan
            .build_topology_reusing(Some(&control.active_topology()))
            .expect("topology");
        control.publish_topology(topology);
    };
    play_until(&mut r, 6, WIDTH);

    // A setting for the last 7.1.4 speaker, which 5.1 does not have.
    control.live.write().speakers.entry(11).or_default().gain = 0.0;
    control.mark_speaker_params_dirty();
    publish("5.1");
    play_until(&mut r, 0, 6);
    assert_eq!(control.take_band_build_error(), None);

    // Wider than the output: refused, the 5.1 bands keep playing.
    publish("9.1.6");
    // The 5.1 bands stay installed meanwhile, so every block settles at once:
    // wait for the worker's reply, with the 5.1 output checked on the way.
    let deadline = std::time::Instant::now() + PATIENCE;
    let error = loop {
        play_until(&mut r, 0, 6);
        if let Some(error) = control.take_band_build_error() {
            break error;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the wider layout is refused with a reason"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    };
    assert!(
        error.contains("16 speakers") && error.contains("restart"),
        "{error}"
    );
    play_until(&mut r, 0, 6);

    // Back to a layout that fits: built, and the error taken back.
    publish("7.1.4");
    control.live.write().speakers.remove(&11);
    control.mark_speaker_params_dirty();
    play_until(&mut r, 6, WIDTH);
    assert_eq!(control.take_band_build_error().as_deref(), Some(""));

    for rate in [44_100, 96_000, 48_000] {
        r.set_sample_rate(rate).expect("sample rate");
        play_until(&mut r, 6, WIDTH);
    }
}

/// A mode change must be ramped, not stepped.
///
/// The binaural and speaker paths are independent DSP chains; swapping them
/// mid-sample steps the waveform and clicks. The switch therefore fades the
/// outgoing path to silence, changes mode at the bottom, and fades the incoming
/// one back up. This pins both halves: the last block of the old width must end
/// near silence, and the first block of the new width must start there.
#[test]
fn an_output_mode_change_is_ramped_not_stepped() {
    let mut r = SpatialRenderer::new(RendererSpec {
        vbap_position_interpolation: false,
        ..test_support::spec(SpeakerLayout::preset("7.1.4").unwrap())
    })
    .unwrap();

    // Steady input, so any envelope in the output is the fade and not the
    // programme.
    let frames = 40;
    let pcm: Vec<f32> = vec![0.5; frames];
    let event = vec![SpatialChannelEvent {
        channel_idx: 0,
        is_bed: false,
        gain_db: Some(0.0),
        ramp_length: Some(frames as u32),
        size: Some([0.0, 0.0, 0.0]),
        position: Some([0.3, -0.2, 0.4]),
        sample_pos: Some(0),
    }];

    let speakers = r.num_speakers();
    // Settle on the speaker path and confirm it is actually producing signal.
    let mut last = r.render_frame(&pcm, 1, &event, Vec::new(), false).unwrap();
    for _ in 0..4 {
        last = r.render_frame(&pcm, 1, &event, Vec::new(), false).unwrap();
    }
    let steady_peak = last.samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(
        steady_peak > 1e-3,
        "speaker path must produce signal to fade"
    );

    r.control.live.write().binaural.output_mode = crate::live_params::OutputMode::Binaural;

    // Pump until the width flips, keeping the final old-width block.
    let mut last_old = None;
    let mut first_new = None;
    for _ in 0..64 {
        let out = r.render_frame(&pcm, 1, &event, Vec::new(), false).unwrap();
        if out.n_channels == speakers {
            last_old = Some(out);
        } else {
            first_new = Some(out);
            break;
        }
    }
    let last_old = last_old.expect("expected blocks on the outgoing width");
    let first_new = first_new.expect("the cross-fade never reached the new width");

    let tail_peak = last_old.samples[last_old.samples.len() - speakers..]
        .iter()
        .fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(
        tail_peak < steady_peak * 0.2,
        "the outgoing path must ramp to near silence before the mode changes \
         (tail {tail_peak:.5} vs steady {steady_peak:.5})"
    );

    let head_peak = first_new.samples[..2]
        .iter()
        .fold(0.0f32, |m, s| m.max(s.abs()));
    let new_peak = first_new.samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(
        head_peak <= new_peak * 0.5,
        "the incoming path must start from silence, not at full level \
         (head {head_peak:.5} vs block peak {new_peak:.5})"
    );
}

/// Build a 7.1.4 renderer whose first three speakers are split into distinct
/// crossover bands, so a test signal can be checked against band assignment.
fn crossover_renderer() -> SpatialRenderer {
    let mut layout = SpeakerLayout::preset("7.1.4").unwrap();
    // speaker 0: sub (…80 Hz), speaker 1: mid (80…2000), speaker 2: top (2000…)
    layout.speakers[0].freq_low = None;
    layout.speakers[0].freq_high = Some(80.0);
    layout.speakers[1].freq_low = Some(80.0);
    layout.speakers[1].freq_high = Some(2000.0);
    layout.speakers[2].freq_low = Some(2000.0);
    layout.speakers[2].freq_high = None;
    renderer_for_layout(layout)
}

/// Build a renderer over an arbitrary layout with the same defaults as
/// [`crossover_renderer`].
fn renderer_for_layout(layout: SpeakerLayout) -> SpatialRenderer {
    try_renderer_for_layout(layout).unwrap()
}

fn try_renderer_for_layout(layout: SpeakerLayout) -> Result<SpatialRenderer> {
    SpatialRenderer::new(RendererSpec {
        vbap_position_interpolation: false,
        ..test_support::spec(layout)
    })
}

/// A layout wider than the 24 speakers the renderer's gain sets used to hold
/// (#745) builds and renders like any other, in every ramp mode, with and
/// without a crossover. An object taken round the room reaches every speaker
/// of the layout, those past the 24th included, and what each speaker plays
/// is the object's signal at the gain the renderer reports for it.
#[test]
fn a_layout_wider_than_24_speakers_renders() {
    use crate::speaker_layout::Speaker;
    const BLOCK: usize = 64;
    // Two rings of speakers, ear level and 40° up.
    let ring = |n: usize, crossover: bool| {
        let mut speakers: Vec<Speaker> = (0..n)
            .map(|i| {
                Speaker::new(
                    format!("S{i}"),
                    -180.0 + 360.0 * (i / 2) as f32 / n.div_ceil(2) as f32,
                    if i % 2 == 0 { 0.0 } else { 40.0 },
                )
            })
            .collect();
        if crossover {
            // Band-limited speakers: the objects go through the crossover
            // and the unified table.
            speakers[0].freq_low = Some(80.0);
            speakers[1].freq_low = Some(200.0);
        }
        SpeakerLayout::from_speakers(speakers).unwrap()
    };
    for (n, crossover, ramp_mode) in [
        (25, false, RampMode::Frame),
        (32, false, RampMode::Sample),
        (64, false, RampMode::Interp),
        (40, true, RampMode::Frame),
        (64, true, RampMode::Sample),
        (33, true, RampMode::Interp),
    ] {
        let context = |step: usize| {
            format!("{n} speakers, crossover {crossover}, {ramp_mode:?}, step {step}")
        };
        let mut r = try_renderer_for_layout(ring(n, crossover))
            .unwrap_or_else(|e| panic!("{}: {e:#}", context(0)));
        r.set_synchronous_stage_builds(true);
        r.renderer_control().live.write().options.ramp_mode = ramp_mode;
        let pcm = noise_block(1, BLOCK, n);
        // Let the object's gain slew settle at unity before anything is
        // measured: a quarter of a second.
        let at = |azimuth: f32| {
            let (x, y, z) = crate::spatial_vbap::spherical_to_adm(azimuth, 20.0, 1.0);
            [SpatialChannelEvent {
                channel_idx: 0,
                is_bed: false,
                gain_db: Some(0.0),
                ramp_length: Some(0),
                size: Some([0.0; 3]),
                position: Some([x as f64, y as f64, z as f64]),
                sample_pos: Some(0),
            }]
        };
        let mut samples = Vec::new();
        for block in 0..48_000 / 4 / BLOCK {
            let events = at(0.0);
            let events: &[SpatialChannelEvent] = if block == 0 { &events } else { &[] };
            samples = r
                .render_frame(&pcm, 1, events, samples, false)
                .expect("render")
                .samples;
        }
        let mut heard = vec![false; n];
        // Round the room between the two rings, a step per speaker pair.
        for step in 0..n {
            let events = at(-180.0 + 360.0 * step as f32 / n as f32);
            let mut frame = r
                .render_frame(&pcm, 1, &events, samples, true)
                .expect("render");
            // Two more blocks on that position: the gains hold across the
            // last one in every ramp mode.
            for _ in 0..2 {
                frame = r
                    .render_frame(&pcm, 1, &[], frame.samples, true)
                    .expect("render");
            }
            assert_eq!(frame.n_channels, n, "{}", context(step));
            assert_eq!(frame.samples.len(), BLOCK * n, "{}", context(step));
            let (_, gains) = frame
                .object_gains
                .iter()
                .find(|(channel, _)| *channel == 0)
                .unwrap_or_else(|| panic!("{}: no metered gains", context(step)));
            assert_eq!(gains.len(), n, "{}", context(step));
            assert!(
                gains.iter().all(|gain| gain.is_finite() && *gain >= 0.0),
                "{}: {gains:?}",
                context(step)
            );
            for (speaker, heard) in heard.iter_mut().enumerate() {
                let peak = frame
                    .samples
                    .chunks_exact(n)
                    .map(|sample| sample[speaker].abs())
                    .fold(0.0f32, f32::max);
                assert!(peak.is_finite(), "{}", context(step));
                // A speaker plays if and only if the object has gain on it.
                assert_eq!(
                    peak > 0.0,
                    gains[speaker] > 0.0,
                    "{}: speaker {speaker} peaks at {peak} with gain {}",
                    context(step),
                    gains[speaker]
                );
                *heard |= peak > 0.0;
                if !crossover {
                    // One band, no delay, unit master gain: the speaker's
                    // signal is the object's, times its gain.
                    for (sample, input) in frame.samples.chunks_exact(n).zip(&pcm) {
                        assert!(
                            (sample[speaker] - input * gains[speaker]).abs() <= 1e-5,
                            "{}: speaker {speaker} plays {} for {input} at gain {}",
                            context(step),
                            sample[speaker],
                            gains[speaker]
                        );
                    }
                }
            }
            samples = frame.samples;
        }
        let silent: Vec<usize> = (0..n).filter(|speaker| !heard[*speaker]).collect();
        assert!(
            silent.is_empty(),
            "{}: speakers {silent:?} never played",
            context(n)
        );
    }
}

/// Render one object between two speakers on the plain 7.1.4 layout, after
/// `setup` has set the per-speaker live params, and return each speaker's
/// signal over the blocks after a settling run (identical input every time).
fn per_speaker_streams(setup: impl Fn(&RendererControl)) -> Vec<Vec<f32>> {
    const BLOCK: usize = 480;
    const SETTLE: usize = 20;
    const KEEP: usize = 10;
    let mut r = renderer_for_layout(SpeakerLayout::preset("7.1.4").unwrap());
    let control = r.renderer_control();
    setup(&control);
    control.mark_speaker_params_dirty();
    let event = SpatialChannelEvent {
        channel_idx: 0,
        is_bed: false,
        gain_db: Some(0.0),
        ramp_length: Some(0),
        size: Some([0.0, 0.0, 0.0]),
        position: Some([-0.4, 1.0, 0.0]),
        sample_pos: Some(0),
    };
    let mut streams: Vec<Vec<f32>> = Vec::new();
    for block in 0..SETTLE + KEEP {
        let pcm = noise_block(1, BLOCK, block);
        let events = if block == 0 {
            std::slice::from_ref(&event)
        } else {
            &[]
        };
        let out = r.render_frame(&pcm, 1, events, Vec::new(), false).unwrap();
        let n = out.n_channels;
        streams.resize(n, Vec::new());
        if block >= SETTLE {
            for (spk, stream) in streams.iter_mut().enumerate() {
                stream.extend(out.samples.iter().skip(spk).step_by(n).copied());
            }
        }
    }
    streams
}

fn energy(stream: &[f32]) -> f64 {
    stream.iter().map(|&s| s as f64 * s as f64).sum()
}

/// The output stage's per-speaker controls — gain, mute, delay — act on
/// their speaker and on nothing else. Measured against the same render with
/// no override: the object lands on two speakers, and each control is set on
/// the louder one while the other must come out bit-identical.
#[test]
fn per_speaker_gain_mute_and_delay_shape_only_their_speaker() {
    let reference = per_speaker_streams(|_| {});
    let mut by_energy: Vec<usize> = (0..reference.len()).collect();
    by_energy.sort_by(|&a, &b| energy(&reference[b]).total_cmp(&energy(&reference[a])));
    let (target, other) = (by_energy[0], by_energy[1]);
    assert!(
        energy(&reference[other]) > 1e-3 * energy(&reference[target]),
        "the object must land on two speakers for the comparison to mean anything"
    );
    let set = |f: fn(&mut crate::live_params::SpeakerLiveParams)| {
        per_speaker_streams(move |control| {
            f(control.live.write().speakers.entry(target).or_default())
        })
    };
    let untouched = |streams: &[Vec<f32>], what: &str| {
        for (spk, stream) in streams.iter().enumerate() {
            if spk != target {
                assert_eq!(stream, &reference[spk], "{what} changed speaker {spk}");
            }
        }
    };

    let halved = set(|p| p.gain = 0.5);
    untouched(&halved, "a gain");
    for (got, want) in halved[target].iter().zip(&reference[target]) {
        assert!(
            (got - 0.5 * want).abs() <= 1e-6,
            "gain 0.5: {got} vs {want}"
        );
    }

    let muted = set(|p| {
        p.gain = 0.5;
        p.muted = true;
    });
    untouched(&muted, "a mute");
    assert!(
        muted[target].iter().all(|&s| s == 0.0),
        "a muted speaker is silent"
    );

    // 1 ms at 48 kHz: the speaker's signal, 48 samples later.
    let delayed = set(|p| p.delay_ms = 1.0);
    untouched(&delayed, "a delay");
    let shift = 48;
    for (n, (got, want)) in delayed[target][shift..]
        .iter()
        .zip(&reference[target])
        .enumerate()
    {
        assert!(
            (got - want).abs() <= 1e-5,
            "delay: sample {n}: {got} vs {want}"
        );
    }
    assert!(energy(&delayed[target]) > 0.5 * energy(&reference[target]));
}

/// What a decoder or a bridge hands `render_frame` is not to be trusted: no
/// channel, a buffer that is not a whole number of frames, an event for a
/// channel that does not exist (up to the last index), a position, size or
/// gain that is NaN or infinite, a ramp of four billion samples. Each is answered with an error
/// or with finite output — never a panic, never NaN on a speaker — and the
/// renderer renders normally afterwards. (Non-finite PCM is not among them:
/// the engine hands over integer PCM from the bridge ABI, finite by
/// construction, and checking every sample here would cost the hot loop.)
#[test]
fn hostile_render_inputs_never_panic_or_reach_the_output_as_nan() {
    let mut r = renderer_for_layout(SpeakerLayout::preset("7.1.4").unwrap());
    let object = |position: [f64; 3]| SpatialChannelEvent {
        channel_idx: 0,
        is_bed: false,
        gain_db: Some(0.0),
        ramp_length: Some(0),
        size: Some([0.0, 0.0, 0.0]),
        position: Some(position),
        sample_pos: Some(0),
    };
    let nan = f64::NAN;
    let inf = f64::INFINITY;
    let mut events: Vec<(&str, Vec<SpatialChannelEvent>)> = vec![
        ("NaN position", vec![object([nan, nan, nan])]),
        ("infinite position", vec![object([inf, -inf, inf])]),
        ("huge position", vec![object([1e30, -1e30, 1e30])]),
        (
            "NaN size",
            vec![SpatialChannelEvent {
                size: Some([f32::NAN; 3]),
                ..object([0.0, 1.0, 0.0])
            }],
        ),
        (
            "NaN gain",
            vec![SpatialChannelEvent {
                gain_db: Some(f32::NAN),
                ..object([0.0, 1.0, 0.0])
            }],
        ),
        (
            "infinite gain",
            vec![SpatialChannelEvent {
                gain_db: Some(f32::INFINITY),
                ..object([0.0, 1.0, 0.0])
            }],
        ),
        (
            "endless ramp",
            vec![SpatialChannelEvent {
                ramp_length: Some(u32::MAX),
                ..object([1.0, 0.0, 0.0])
            }],
        ),
        (
            "far sample position",
            vec![SpatialChannelEvent {
                sample_pos: Some(u64::MAX),
                ..object([0.0, 1.0, 0.0])
            }],
        ),
        (
            "unknown channel",
            vec![SpatialChannelEvent {
                channel_idx: 99,
                ..object([0.0, 1.0, 0.0])
            }],
        ),
        (
            "channel one billion",
            vec![SpatialChannelEvent {
                channel_idx: 1_000_000_000,
                ..object([0.0, 1.0, 0.0])
            }],
        ),
        (
            "last channel index",
            vec![SpatialChannelEvent {
                channel_idx: usize::MAX,
                ..object([0.0, 1.0, 0.0])
            }],
        ),
    ];
    events.push(("no event", Vec::new()));
    let buffers: Vec<(&str, Vec<f32>, usize)> = vec![
        ("one channel", noise_block(1, 480, 0), 1),
        ("empty", Vec::new(), 1),
        ("no channel", noise_block(1, 480, 1), 0),
        ("partial frame", noise_block(1, 7, 2), 2),
    ];
    for (what_events, evs) in &events {
        for (what_pcm, pcm, channels) in &buffers {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                r.render_frame(pcm, *channels, evs, Vec::new(), false)
            }));
            let case = format!("{what_events} / {what_pcm}");
            let Ok(result) = outcome else {
                panic!("{case}: render_frame panicked");
            };
            if let Ok(frame) = result {
                assert!(
                    frame.samples.iter().all(|s| s.is_finite()),
                    "{case}: non-finite output"
                );
            }
        }
    }
    // And the renderer is still a renderer.
    let after = r
        .render_frame(
            &noise_block(1, 480, 9),
            1,
            &[object([0.0, 1.0, 0.0])],
            Vec::new(),
            false,
        )
        .expect("a normal frame after the hostile ones");
    assert!(after.samples.iter().all(|s| s.is_finite()));
    assert!(
        after.samples.iter().any(|&s| s != 0.0),
        "it still renders sound"
    );
}

/// The test signal must reach only the speaker under test.
///
/// A speaker test that bleeds into its neighbours is worse than none: the whole
/// point is to answer "is THIS the speaker I think it is".
#[test]
fn speaker_test_reaches_only_the_speaker_under_test() {
    let mut r = crossover_renderer();
    let frames = 512;
    let pcm = vec![0.0f32; frames]; // silent programme, so anything heard is the test
    let target = 1usize;

    r.control.live.write().speaker_test = Some(crate::live_params::SpeakerTest {
        speaker_idx: target,
        level: 0.1,
        isolation: crate::live_params::TestIsolation::TestOnly,
    });

    let out = r.render_frame(&pcm, 1, &[], Vec::new(), false).unwrap();
    let n = out.n_channels;
    for spk in 0..n {
        let energy: f32 = (0..frames).map(|f| out.samples[f * n + spk].abs()).sum();
        if spk == target {
            assert!(
                energy > 0.0,
                "the speaker under test must receive the signal"
            );
        } else {
            assert_eq!(
                energy, 0.0,
                "speaker {spk} must stay silent during the test"
            );
        }
    }
}

/// The signal must be band-limited to what the speaker reproduces.
///
/// Measured by how fast the waveform moves, not by how much energy it carries.
/// Total energy is the wrong discriminator here and was tried first: pink noise
/// carries equal energy per octave, so a sub covering 2-3 octaves and a tweeter
/// covering 3.6 land within a factor of 1.4 of each other even when the filter
/// works perfectly — a threshold tight enough to catch a bug would also fail on
/// correct output.
///
/// The mean sample-to-sample step, normalised by amplitude, separates them
/// cleanly instead: a signal low-passed at 80 Hz barely changes between
/// consecutive samples at 48 kHz, while one high-passed at 2 kHz changes a lot.
/// A regression that skipped band assignment would send identical full-range
/// noise to both and the two figures would converge.
#[test]
fn speaker_test_is_limited_to_the_speakers_bands() {
    let frames = 4096;
    let pcm = vec![0.0f32; frames];

    let slew_ratio_for = |idx: usize| -> f32 {
        let mut r = crossover_renderer();
        r.control.live.write().speaker_test = Some(crate::live_params::SpeakerTest {
            speaker_idx: idx,
            level: 0.1,
            isolation: crate::live_params::TestIsolation::TestOnly,
        });
        let out = r.render_frame(&pcm, 1, &[], Vec::new(), false).unwrap();
        let n = out.n_channels;
        let ch: Vec<f32> = (0..frames).map(|f| out.samples[f * n + idx]).collect();
        let amp: f32 = ch.iter().map(|s| s.abs()).sum::<f32>() / frames as f32;
        let step: f32 =
            ch.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f32>() / (frames - 1) as f32;
        assert!(amp > 0.0, "speaker {idx} produced no signal");
        step / amp
    };

    let sub = slew_ratio_for(0);
    let top = slew_ratio_for(2);
    assert!(
        sub < top * 0.25,
        "the sub's band must move far more slowly than the tweeter's \
         (sub {sub:.4}, top {top:.4})"
    );
}

/// A direct (non-spatialized) speaker must still produce a test signal when a
/// crossover is active.
///
/// Band membership is computed over spatialized speakers only, so a direct
/// speaker appears in no band; the original band-summing injection summed
/// nothing for it and the test was silent. A direct speaker that declares no
/// frequency range plays the test unfiltered (programme audio reaches it by
/// bypassing the filter bank, and nothing says what to cut) — asserted by
/// comparing its slew ratio against the sub's: identical low-passed noise on
/// both would mean the fallback did not engage.
#[test]
fn speaker_test_reaches_a_direct_speaker_despite_the_crossover() {
    let frames = 4096;
    let pcm = vec![0.0f32; frames];

    // The fixture is the 7.1.4 preset: speaker 3 is the LFE, the preset's one
    // non-spatialized speaker.
    assert!(!SpeakerLayout::preset("7.1.4").unwrap().speakers[3].spatialize);

    let channel_for = |idx: usize| -> Vec<f32> {
        let mut r = crossover_renderer();
        r.control.live.write().speaker_test = Some(crate::live_params::SpeakerTest {
            speaker_idx: idx,
            level: 0.1,
            isolation: crate::live_params::TestIsolation::TestOnly,
        });
        let out = r.render_frame(&pcm, 1, &[], Vec::new(), false).unwrap();
        let n = out.n_channels;
        (0..frames).map(|f| out.samples[f * n + idx]).collect()
    };

    let direct = channel_for(3);
    let amp: f32 = direct.iter().map(|s| s.abs()).sum::<f32>() / frames as f32;
    assert!(
        amp > 0.0,
        "the direct (non-spatialized) speaker must produce a test signal"
    );

    // Unfiltered, not accidentally low-passed: full-range noise slews far
    // faster than the sub's 80 Hz band.
    let sub = channel_for(0);
    let slew = |ch: &[f32]| -> f32 {
        let amp: f32 = ch.iter().map(|s| s.abs()).sum::<f32>() / ch.len() as f32;
        let step: f32 =
            ch.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f32>() / (ch.len() - 1) as f32;
        step / amp
    };
    assert!(
        slew(&direct) > slew(&sub) * 4.0,
        "the direct speaker's test must be full-range, not band-limited \
         (direct {:.4}, sub {:.4})",
        slew(&direct),
        slew(&sub)
    );
}

/// A direct speaker that declares a frequency range gets a band-limited test,
/// even though it belongs to no crossover band.
///
/// Programme audio bypasses the filter bank on its way to a direct speaker,
/// but `freq_low`/`freq_high` still describe what it can reproduce — a
/// direct-routed sub must not be fed full-range noise. The injection builds a
/// dedicated LR4 split at the speaker's own edges instead. Asserted like
/// `speaker_test_is_limited_to_the_speakers_bands`: a direct sub cut at 80 Hz
/// must slew like the spatialized sub's band, far below the tweeter's.
#[test]
fn a_direct_speakers_test_honours_its_declared_frequency_range() {
    let frames = 4096;
    let pcm = vec![0.0f32; frames];

    let channel_for = |idx: usize, freq_high: Option<f32>| -> Vec<f32> {
        let mut layout = SpeakerLayout::preset("7.1.4").unwrap();
        layout.speakers[0].freq_low = None;
        layout.speakers[0].freq_high = Some(80.0);
        layout.speakers[1].freq_low = Some(80.0);
        layout.speakers[1].freq_high = Some(2000.0);
        layout.speakers[2].freq_low = Some(2000.0);
        layout.speakers[2].freq_high = None;
        // Speaker 3 is the LFE, the preset's one non-spatialized speaker.
        assert!(!layout.speakers[3].spatialize);
        layout.speakers[3].freq_high = freq_high;
        let mut r = renderer_for_layout(layout);
        r.control.live.write().speaker_test = Some(crate::live_params::SpeakerTest {
            speaker_idx: idx,
            level: 0.1,
            isolation: crate::live_params::TestIsolation::TestOnly,
        });
        let out = r.render_frame(&pcm, 1, &[], Vec::new(), false).unwrap();
        let n = out.n_channels;
        (0..frames).map(|f| out.samples[f * n + idx]).collect()
    };

    let slew = |ch: &[f32]| -> f32 {
        let amp: f32 = ch.iter().map(|s| s.abs()).sum::<f32>() / ch.len() as f32;
        assert!(amp > 0.0, "speaker produced no test signal");
        let step: f32 =
            ch.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f32>() / (ch.len() - 1) as f32;
        step / amp
    };

    let direct_sub = slew(&channel_for(3, Some(80.0)));
    let spatial_sub = slew(&channel_for(0, None));
    let top = slew(&channel_for(2, None));

    assert!(
        direct_sub < top * 0.25,
        "a direct speaker cut at 80 Hz must be band-limited, not full-range \
         (direct sub {direct_sub:.4}, top {top:.4})"
    );
    assert!(
        direct_sub < spatial_sub * 2.0 && spatial_sub < direct_sub * 2.0,
        "the direct sub's band must move like the spatialized sub's \
         (direct sub {direct_sub:.4}, spatialized sub {spatial_sub:.4})"
    );
}

/// A test at full scale must not put a single sample past full scale.
///
/// This is the regression guard for the bug that shipped in the original test
/// signal: the level was applied straight to a unit-RMS generator, so it set the
/// *RMS* and the peaks landed a crest factor above it — a -6 dBFS test measured
/// peaks up to +5.9 dBFS on a live 7.1.4 render, which clips on real hardware.
/// Nothing caught it, because a running test deliberately suppresses peak
/// tracking so it cannot drive the auto-gain.
///
/// Asserted at level 1.0 (full scale) rather than a comfortable level so the
/// margin under test is zero: at any lower level a bug of this size could hide.
/// Every speaker is checked, sub through tweeter, because the clamp sits after
/// the band sum and each speaker sums a different set of bands.
#[test]
fn a_full_scale_test_signal_never_exceeds_full_scale() {
    let frames = 8192;
    let pcm = vec![0.0f32; frames];

    for idx in 0..3 {
        let mut r = crossover_renderer();
        r.control.live.write().speaker_test = Some(crate::live_params::SpeakerTest {
            speaker_idx: idx,
            level: 1.0,
            isolation: crate::live_params::TestIsolation::TestOnly,
        });

        let mut peak = 0.0f32;
        let mut heard = 0.0f32;
        // Several blocks: the generator carries state across them, and a long
        // run is exactly where the crest of pink noise grows.
        for _ in 0..8 {
            let out = r.render_frame(&pcm, 1, &[], Vec::new(), false).unwrap();
            for s in &out.samples {
                peak = peak.max(s.abs());
                heard += s.abs();
            }
        }

        assert!(heard > 0.0, "speaker {idx} produced no test signal");
        assert!(
            peak <= 1.0,
            "speaker {idx}: a full-scale test peaked at {peak} — anything above \
             1.0 clips on a real device"
        );
    }
}

/// Clearing the test must stop it, and must not leave the speaker attenuated or
/// the programme suppressed.
#[test]
fn clearing_the_test_restores_normal_output() {
    let mut r = crossover_renderer();
    let frames = 256;
    let pcm = vec![0.0f32; frames];

    r.control.live.write().speaker_test = Some(crate::live_params::SpeakerTest {
        speaker_idx: 1,
        level: 0.1,
        isolation: crate::live_params::TestIsolation::TestOnly,
    });
    let during = r.render_frame(&pcm, 1, &[], Vec::new(), false).unwrap();
    let n = during.n_channels;
    let heard: f32 = (0..frames).map(|f| during.samples[f * n + 1].abs()).sum();
    assert!(heard > 0.0, "the test must be audible while it runs");

    r.control.live.write().speaker_test = None;
    let after = r.render_frame(&pcm, 1, &[], Vec::new(), false).unwrap();
    let silent: f32 = after.samples.iter().map(|s| s.abs()).sum();
    assert_eq!(
        silent, 0.0,
        "with the test cleared and a silent programme the output must be silent"
    );
}

/// The monitoring cadences fall back to the *host's* default, not to a literal
/// living in whichever code path happens to re-seed them.
///
/// This is a regression test for a shipped bug: the CLI host booted its meters
/// at 50 Hz, but the shared runtime seed — replayed wholesale by the live
/// profile switch — carried the embedded host's 10 Hz. Switching profile on the
/// CLI therefore dropped the cadence to a fifth, silently, with the only symptom
/// being Studio's meters going choppy.
#[test]
fn cadences_fall_back_to_the_host_default() {
    let layout = SpeakerLayout::preset("7.1.4").unwrap();
    let renderer = SpatialRenderer::new(RendererSpec {
        table_mode: VbapTableMode::Cartesian {
            x_size: 9,
            y_size: 9,
            z_size: 5,
            z_neg_size: 5,
        },
        vbap_position_interpolation: false,
        cartesian_default_x_size: 9,
        cartesian_default_y_size: 9,
        cartesian_default_z_size: 5,
        cartesian_default_z_neg_size: 5,
        ..test_support::spec(layout)
    })
    .unwrap();
    let control = renderer.renderer_control();

    // An embedded host declares a slower cadence, then seeds from a config
    // that says nothing about it.
    control.set_cadence_defaults_hz(10.0, 10.0);
    control.seed_cadences_from_config(None, None);
    assert_eq!(control.meter_rate_hz(), 10.0);
    assert_eq!(control.diag_rate_hz(), 10.0);

    // A host that wants faster monitoring keeps it across a re-seed — this is
    // the assertion that used to fail, because the re-seed carried 10 Hz.
    control.set_cadence_defaults_hz(50.0, 50.0);
    control.seed_cadences_from_config(None, None);
    assert_eq!(control.meter_rate_hz(), 50.0);
    assert_eq!(control.diag_rate_hz(), 50.0);

    // A config that does declare a cadence still wins over the host default.
    control.seed_cadences_from_config(Some(25.0), Some(4.0));
    assert_eq!(control.meter_rate_hz(), 25.0);
    assert_eq!(control.diag_rate_hz(), 4.0);

    // And the declared default survives a config-driven seed, so the *next*
    // profile switch still lands on the host's value rather than the last
    // config's.
    control.seed_cadences_from_config(None, None);
    assert_eq!(control.meter_rate_hz(), 50.0);
    assert_eq!(control.diag_rate_hz(), 50.0);
}

/// A donated buffer is cleared and resized, so handing back a *used* one is
/// identical to handing over a fresh one.
///
/// The embedded host relies on this to pool its output buffers instead of
/// allocating one per rendered block. If `render_frame` ever appended to the
/// donated buffer rather than clearing it, or trusted its existing length, the
/// symptom would be stale audio from two frames ago rather than a crash — so it
/// is worth pinning rather than assuming.
#[test]
fn a_recycled_output_buffer_renders_identically_to_a_fresh_one() {
    fn build() -> SpatialRenderer {
        SpatialRenderer::new(RendererSpec {
            table_mode: VbapTableMode::Cartesian {
                x_size: 9,
                y_size: 9,
                z_size: 5,
                z_neg_size: 5,
            },
            vbap_position_interpolation: false,
            cartesian_default_x_size: 9,
            cartesian_default_y_size: 9,
            cartesian_default_z_size: 5,
            cartesian_default_z_neg_size: 5,
            ..test_support::spec(SpeakerLayout::preset("7.1.4").unwrap())
        })
        .unwrap()
    }

    let pcm: Vec<f32> = (0..40).map(|i| (i * 7 % 13) as f32 / 13.0 - 0.5).collect();
    let event = vec![SpatialChannelEvent {
        channel_idx: 0,
        is_bed: false,
        gain_db: Some(0.0),
        ramp_length: Some(40),
        size: Some([0.0, 0.0, 0.0]),
        position: Some([0.3, -0.2, 0.4]),
        sample_pos: Some(0),
    }];

    let fresh = build()
        .render_frame(&pcm, 1, &event, Vec::new(), false)
        .unwrap();

    // Deliberately hostile: non-zero content, and a length that is both wrong
    // and longer than what this frame needs.
    let dirty = vec![-7.5f32; fresh.samples.len() * 3 + 11];
    let recycled = build().render_frame(&pcm, 1, &event, dirty, false).unwrap();

    assert_eq!(
        recycled.samples.len(),
        fresh.samples.len(),
        "the donated buffer must be resized to this frame, not left long"
    );
    assert_eq!(
        recycled.samples, fresh.samples,
        "no sample of the previous contents may survive into the render"
    );
}

/// With the linear-phase FIR crossover selected, every path through the
/// speaker stage carries the same constant delay: an impulse fed to a direct
/// (bed) channel and one fed to an object channel must land at the same
/// output sample index. The object path is delayed by the FIR bank itself;
/// the bed path bypasses the bank and relies on the compensating
/// `IntegerDelay` — this test fails if that compensation is missing or wrong.
#[test]
fn fir_crossover_keeps_beds_aligned_with_objects() {
    let mut r = crossover_renderer();
    r.control.live.write().options.crossover_type = crate::live_params::CrossoverType::Fir;
    // Channel 0: direct LFE bed (7.1.4 speaker 3). Channel 1: trailing object.
    r.configure_channel_routing(&[ChannelRoute::Direct(bridge_api::RChannelLabel::LFE)]);
    const LFE_SPK: usize = 3;
    const IMPULSE_AT: usize = 2_000; // past the gain slew (20 ms = 960 samples)

    let events = vec![
        SpatialChannelEvent {
            channel_idx: 0,
            is_bed: true,
            gain_db: Some(0.0),
            ramp_length: Some(0),
            size: None,
            position: None,
            sample_pos: Some(0),
        },
        SpatialChannelEvent {
            channel_idx: 1,
            is_bed: false,
            gain_db: Some(0.0),
            ramp_length: Some(0),
            size: Some([0.0, 0.0, 0.0]),
            position: Some([0.0, 1.0, 0.0]),
            sample_pos: Some(0),
        },
    ];

    // Render in host-sized frames rather than one huge block: the gain slew
    // is constant-rate but block-granular, so a single 16k-sample frame would
    // still be ramping at the impulse and scale it down.
    let frame_len = 1_024;
    let sample_length = 16 * frame_len;
    let mut pcm = vec![0.0f32; sample_length * 2];
    pcm[IMPULSE_AT * 2] = 1.0; // bed channel
    pcm[IMPULSE_AT * 2 + 1] = 1.0; // object channel

    let mut samples = Vec::with_capacity(sample_length * 12);
    let mut n = 0;
    for f in 0..sample_length / frame_len {
        let ev: &[SpatialChannelEvent] = if f == 0 { &events } else { &[] };
        let out = r
            .render_frame(
                &pcm[f * frame_len * 2..(f + 1) * frame_len * 2],
                2,
                ev,
                Vec::new(),
                false,
            )
            .unwrap();
        n = out.n_channels;
        samples.extend_from_slice(&out.samples);
    }
    let out = RenderedFrame {
        samples,
        n_channels: n,
        object_gains: Vec::new(),
        object_band_gains: Vec::new(),
        object_band_sq: Vec::new(),
        object_test_position: None,
        object_test_level: None,
        crossover_time_ms: 0.0,
    };

    let latency = r
        .speaker_stage
        .crossover_filter_bank
        .as_ref()
        .expect("crossover layout must build a bank")
        .latency_samples();
    assert!(latency > 0, "the FIR engine must report its latency");
    assert!(
        IMPULSE_AT + latency < sample_length,
        "test frame too short for the bank latency ({latency})"
    );
    // The host-facing accessor must report the same figure the mix used —
    // this is what liborender forwards to mpv for A/V sync compensation.
    assert_eq!(
        r.output_latency_samples(),
        latency,
        "output_latency_samples must reflect the rendered path"
    );

    // Arrival time = argmax of per-sample magnitude, per path.
    let argmax = |value_at: &dyn Fn(usize) -> f32| -> usize {
        (0..sample_length)
            .max_by(|&a, &b| value_at(a).total_cmp(&value_at(b)))
            .unwrap()
    };
    let bed_at = |s: usize| out.samples[s * n + LFE_SPK].abs();
    let obj_at = |s: usize| -> f32 {
        (0..n)
            .filter(|&spk| spk != LFE_SPK)
            .map(|spk| out.samples[s * n + spk].abs())
            .sum()
    };

    let bed_peak = argmax(&bed_at);
    let obj_peak = argmax(&obj_at);
    assert_eq!(
        bed_peak,
        IMPULSE_AT + latency,
        "bed impulse must be delayed by exactly the bank latency"
    );
    assert_eq!(
        obj_peak,
        IMPULSE_AT + latency,
        "object impulse must arrive at the bank latency"
    );
    // The bed path is a pure delay: the impulse must come through at unity.
    let v = out.samples[bed_peak * n + LFE_SPK];
    assert!(
        (v - 1.0).abs() < 1e-3,
        "bed impulse must survive the compensation delay at unity gain, got {v}"
    );
}

/// A `brir` HRIR source forces the virtual-speaker path whatever the binaural
/// mode says, renders the buses through the set — a hard-right object lands
/// on right-side emitters, whose synthetic pairs favour the right ear — and
/// adds the BRIR stage's block to the reported latency.
#[test]
fn brir_source_forces_the_cascade_and_convolves_the_set() {
    use crate::binaural::brir_stage::{BRIR_BLOCK, test_support::synth_set};

    let mut r = build_cascade_test_renderer(LiveEvaluationMode::PrecomputedCartesian, false);
    {
        let mut live = r.control.live.write();
        live.binaural.output_mode = crate::live_params::OutputMode::Binaural;
        // Direct mode: the source alone must select the cascade.
        live.binaural.mode = crate::live_params::BinauralMode::Direct;
        live.binaural.hrir_source =
            crate::binaural::HrirSource::Brir("synthetic (installed below)".into());
    }
    // The 7.1 horizontal loudspeakers as emitters (SOFA azimuths, left
    // positive); the app's 7.1.4 heights map onto their nearest ones.
    let set = synth_set(&[30.0, -30.0, 0.0, 90.0, -90.0, 150.0, -150.0], &[0.0], 400);
    r.brir.install_set(set, 12);

    let mut lcg: u32 = 0x1234_5678;
    let mut noise_block = move || -> Vec<f32> {
        (0..40)
            .map(|_| {
                lcg = lcg.wrapping_mul(1664525).wrapping_add(1013904223);
                (lcg >> 8) as f32 / (1u32 << 24) as f32 - 0.5
            })
            .collect()
    };
    let pcm = noise_block();
    let event = vec![SpatialChannelEvent {
        channel_idx: 0,
        is_bed: false,
        gain_db: Some(0.0),
        ramp_length: Some(40),
        size: Some([0.0, 0.0, 0.0]),
        position: Some([1.0, 0.0, 0.0]),
        sample_pos: Some(0),
    }];
    let first = r.render_frame(&pcm, 1, &event, Vec::new(), false).unwrap();
    assert_eq!(first.samples.len(), 40 * 2, "stereo out");
    assert!(
        r.cascade.is_some(),
        "a BRIR source runs the virtual-speaker path"
    );
    assert_eq!(
        r.output_latency_samples(),
        BRIR_BLOCK - 1,
        "the BRIR stage's block is reported as latency"
    );
    assert_eq!(
        r.brir.bus_emitters().iter().filter(|e| e.is_none()).count(),
        1,
        "the LFE bus is direct"
    );

    let (mut e_l, mut e_r) = (0.0f32, 0.0f32);
    for i in 0..60 {
        let pcm = noise_block();
        let out = r.render_frame(&pcm, 1, &[], Vec::new(), false).unwrap();
        if i >= 30 {
            for s in out.samples.chunks_exact(2) {
                e_l += s[0] * s[0];
                e_r += s[1] * s[1];
            }
        }
    }
    assert!(e_r > 0.0, "the set is convolved");
    assert!(
        e_r > 1.5 * e_l,
        "a hard-right object favours the right ear through the set: L {e_l:.4} R {e_r:.4}"
    );
}

/// A renderer on the 7.1.4 layout with a resident BRIR set of the 7.1
/// horizontal loudspeakers (SOFA azimuths, left positive), selected as the
/// headphone source and reported loaded the way a real load reports it.
/// Synchronous builds when `synchronous` (an offline render).
fn brir_layout_test_renderer(synchronous: bool) -> SpatialRenderer {
    use crate::binaural::brir_stage::test_support::synth_set;

    let mut r = build_cascade_test_renderer(LiveEvaluationMode::PrecomputedCartesian, false);
    r.set_synchronous_stage_builds(synchronous);
    let path = "synthetic-7.1.sofa";
    let opts = {
        let mut live = r.control.live.write();
        live.binaural.output_mode = crate::live_params::OutputMode::Binaural;
        live.binaural.hrir_source = crate::binaural::HrirSource::Brir(path.into());
        cascade::brir_load_options(&live.binaural)
    };
    let set = synth_set(&[0.0, 30.0, -30.0, 90.0, -90.0, 135.0, -135.0], &[0.0], 400);
    r.brir.install_set_as(path, opts, set, 12);
    r
}

/// A renderer like [`brir_layout_test_renderer`] whose resident set's
/// loudspeakers stand at `emitters` (metres, renderer frame: `x` right, `y`
/// front, `z` up): a measured room with a geometry of its own, which the
/// user's room (the fixture's `1 × 2 × 0.5`, rear 2) does not describe.
fn brir_room_test_renderer(emitters: &[[f32; 3]]) -> SpatialRenderer {
    use crate::binaural::brir_stage::test_support::synth_set_at;

    let mut r = build_cascade_test_renderer(LiveEvaluationMode::PrecomputedCartesian, false);
    r.set_synchronous_stage_builds(true);
    let path = "synthetic-room.sofa";
    let opts = {
        let mut live = r.control.live.write();
        live.binaural.output_mode = crate::live_params::OutputMode::Binaural;
        live.binaural.hrir_source = crate::binaural::HrirSource::Brir(path.into());
        cascade::brir_load_options(&live.binaural)
    };
    // SOFA frame: `x` front, `y` left.
    let sofa: Vec<[f32; 3]> = emitters.iter().map(|&[x, y, z]| [y, -x, z]).collect();
    let set = synth_set_at(&sofa, &[0.0], 400);
    r.brir.install_set_as(path, opts, set, 12);
    r
}

/// A measured room the listener is not centred in: fronts far, sides near,
/// backs in between, four heights over the fronts and the backs. Eleven
/// loudspeakers, the stage's width less the LFE.
const ELONGATED_ROOM_EMITTERS: [[f32; 3]; 11] = [
    [0.0, 3.0, 0.0],
    [-1.5, 3.0, 0.0],
    [1.5, 3.0, 0.0],
    [-2.0, 0.0, 0.0],
    [2.0, 0.0, 0.0],
    [-1.5, -2.0, 0.0],
    [1.5, -2.0, 0.0],
    [-1.5, 3.0, 1.5],
    [1.5, 3.0, 1.5],
    [-1.5, -2.0, 1.5],
    [1.5, -2.0, 1.5],
];

/// The gains the set's topology pans a normalized position with, in layout
/// speaker order (the LFE's entry stays 0), read off the published
/// topology's model in the room it pans in.
fn brir_layout_gains(
    topology: &crate::live_params::RenderTopology,
    position: [f32; 3],
) -> Vec<f32> {
    let room = topology.room;
    let gains = topology
        .backend
        .gains_at(&crate::render_backend::RenderRequest {
            adm_position: [position[0] as f64, position[1] as f64, position[2] as f64],
            event_size: [0.0; 3],
            room_ratio: room.ratio,
            room_ratio_rear: room.rear,
            room_ratio_lower: room.lower,
            room_ratio_center_blend: room.center_blend,
            use_distance_diffuse: false,
            distance_diffuse_threshold: 1.0,
            distance_diffuse_curve: 1.0,
            diffuse_mirror_axes: crate::spatial_vbap::MirrorAxes::default(),
            distance_model: crate::spatial_vbap::DistanceModel::None,
        });
    (0..topology.num_speakers)
        .map(|speaker| {
            topology
                .backend_speaker_index_for_layout_speaker(speaker)
                .map_or(0.0, |i| gains[i])
        })
        .collect()
}

/// #803: a BRIR set's loudspeakers are panned onto in their measured room's
/// own geometry. The topology's room is derived from the set (the
/// loudspeakers' box, an estimate without corners in the file), its
/// speakers are placed in it as fractions, and the objects pan in it: an
/// object at a loudspeaker's place lands on it alone, one halfway in angle
/// between two neighbours splits evenly between them, and the user's room
/// ratio changes none of it.
#[test]
fn a_measured_room_pans_in_its_own_geometry_whatever_the_users_room() {
    let topology_in = |user_room: Option<([f32; 3], f32, f32)>| {
        let mut r = brir_room_test_renderer(&ELONGATED_ROOM_EMITTERS);
        if let Some((ratio, rear, lower)) = user_room {
            let mut live = r.control.live.write();
            live.room_ratio = ratio;
            live.room_ratio_rear = rear;
            live.room_ratio_lower = lower;
        }
        render_noise_object(&mut r, 2);
        let topology = r.control.active_topology();
        assert!(topology.brir_layout, "the topology is the set's");
        topology
    };
    let topology = topology_in(None);
    let measured = topology.measured_room.as_ref().expect("the measured room");
    assert!(measured.estimated, "no corners in a synthetic file");
    // With the user's front/rear blend: a panning policy, not a room.
    let blend = test_support::spec(SpeakerLayout::preset("7.1.4").unwrap()).room_ratio_center_blend;
    assert_eq!(
        topology.room,
        measured.ratios(blend),
        "the stage pans in the measured room"
    );
    assert_ne!(topology.room.ratio, [1.0, 2.0, 0.5], "not in the user's");
    let radius = measured.radius_m();
    let layout = &topology.speaker_layout;

    // 1. A loudspeaker's own place: all of the gain on it.
    for (i, speaker) in layout.speakers.iter().enumerate().take(11) {
        let gains = brir_layout_gains(&topology, [speaker.x, speaker.y, speaker.z]);
        let peak = gains.iter().cloned().fold(0.0f32, f32::max);
        assert!(peak > 0.0);
        for (j, g) in gains.iter().enumerate() {
            if j == i {
                assert!(
                    (g - peak).abs() < 1e-6,
                    "{}: its own gain is the peak",
                    speaker.name
                );
            } else {
                assert!(
                    g.abs() < 1e-3 * peak,
                    "{}: {} gets {g}",
                    speaker.name,
                    layout.speakers[j].name
                );
            }
        }
    }

    // 2. Halfway in angle between two neighbours, in the room's metric: an
    //    even split. The direction bisects the loudspeakers' directions in
    //    metres; the cube reading of a point along it is the inverse warp.
    let bisector = |a: usize, b: usize| {
        let unit = |p: [f32; 3]| {
            let n = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
            [p[0] / n, p[1] / n, p[2] / n]
        };
        let (ua, ub) = (
            unit(ELONGATED_ROOM_EMITTERS[a]),
            unit(ELONGATED_ROOM_EMITTERS[b]),
        );
        let d = unit([ua[0] + ub[0], ua[1] + ub[1], ua[2] + ub[2]]);
        // One metre out, well inside the room.
        topology
            .room
            .inverse([d[0] / radius, d[1] / radius, d[2] / radius])
    };
    for (a, b) in [(0usize, 2usize), (4, 6), (1, 3)] {
        let gains = brir_layout_gains(&topology, bisector(a, b));
        let (ga, gb) = (gains[a], gains[b]);
        assert!(ga > 0.0 && gb > 0.0, "{a}/{b}: both play: {gains:?}");
        assert!(
            (ga - gb).abs() < 1e-3 * ga.max(gb),
            "{}/{}: an even split, got {ga} / {gb}",
            layout.speakers[a].name,
            layout.speakers[b].name
        );
        for (j, g) in gains.iter().enumerate() {
            if j != a && j != b {
                assert!(
                    g.abs() < 1e-3 * ga,
                    "{}: {g} on a bisector of others",
                    layout.speakers[j].name
                );
            }
        }
    }

    // 3. The user's room no longer plays: a cube gives the same answers.
    let cube = topology_in(Some(([1.0, 1.0, 1.0], 1.0, 1.0)));
    assert_eq!(cube.room, topology.room);
    for position in [
        [
            layout.speakers[1].x,
            layout.speakers[1].y,
            layout.speakers[1].z,
        ],
        bisector(0, 2),
        [0.3, -0.6, 0.4],
    ] {
        let (a, b) = (
            brir_layout_gains(&topology, position),
            brir_layout_gains(&cube, position),
        );
        for (ga, gb) in a.iter().zip(&b) {
            assert!((ga - gb).abs() < 1e-6, "{position:?}: {a:?} vs {b:?}");
        }
    }
}

/// The per-frame render reads the room off the topology too: with the
/// user's room stretched to an absurd depth, an object at a loudspeaker's
/// place still feeds that loudspeaker's bus alone.
#[test]
fn a_measured_rooms_buses_ignore_the_users_room_per_frame() {
    let mut r = brir_room_test_renderer(&ELONGATED_ROOM_EMITTERS);
    {
        let mut live = r.control.live.write();
        live.room_ratio = [1.0, 5.0, 1.0];
        live.room_ratio_rear = 5.0;
    }
    render_noise_object(&mut r, 2);
    let topology = r.control.active_topology();
    assert!(topology.brir_layout);
    // The front-left loudspeaker's place in the cube.
    let fl = &topology.speaker_layout.speakers[1];
    assert_eq!(fl.name, "FL");
    let position = [fl.x as f64, fl.y as f64, fl.z as f64];
    drop(topology);

    let mut lcg: u32 = 0x1234_5678;
    let mut noise_block = move || -> Vec<f32> {
        (0..40)
            .map(|_| {
                lcg = lcg.wrapping_mul(1664525).wrapping_add(1013904223);
                (lcg >> 8) as f32 / (1u32 << 24) as f32 - 0.5
            })
            .collect()
    };
    let event = vec![SpatialChannelEvent {
        channel_idx: 0,
        is_bed: false,
        gain_db: Some(0.0),
        ramp_length: Some(40),
        size: Some([0.0, 0.0, 0.0]),
        position: Some(position),
        sample_pos: Some(0),
    }];
    let mut energy = vec![0.0f32; 12];
    for i in 0..40 {
        let pcm = noise_block();
        let events: &[SpatialChannelEvent] = if i == 0 { &event } else { &[] };
        r.render_frame(&pcm, 1, events, Vec::new(), false).unwrap();
        if i >= 20 {
            let (bus, buses) = r.virtual_bus().expect("a cascaded frame");
            for frame in bus.chunks(buses) {
                for (e, s) in energy.iter_mut().zip(frame) {
                    *e += s * s;
                }
            }
        }
    }
    let total: f32 = energy.iter().sum();
    assert!(total > 0.0);
    // The bands read a precomputed cartesian table, interpolated between
    // its cells: a sliver reaches the neighbours. In the user's room the
    // depth would be stretched fivefold and the fronts would take most.
    assert!(energy[1] > 0.99 * total, "FL alone: {energy:?}");

    // The cascade's virtual speakers stand where the stage pans onto them:
    // the room fractions warped with the measured room point at the
    // emitters (a cube reading of a fraction would not: the heights of an
    // elongated room read 15° too high), so the BRIR stage maps each bus
    // to its own emitter and the HRTF stage, on another set, would convolve
    // the right direction.
    let cascade = r.cascade.as_ref().expect("the cascade");
    for (i, e) in ELONGATED_ROOM_EMITTERS.iter().enumerate() {
        let p = cascade.bin_pos[i];
        let az = |x: f64, y: f64| x.atan2(y).to_degrees();
        let el = |x: f64, y: f64, z: f64| z.atan2((x * x + y * y).sqrt()).to_degrees();
        let (want_az, want_el) = (
            az(e[0] as f64, e[1] as f64),
            el(e[0] as f64, e[1] as f64, e[2] as f64),
        );
        let (got_az, got_el) = (az(p[0], p[1]), el(p[0], p[1], p[2]));
        assert!(
            (got_az - want_az).abs() < 0.05 && (got_el - want_el).abs() < 0.05,
            "bus {i}: virtual speaker at {got_az:.1}/{got_el:.1}, emitter at {want_az:.1}/{want_el:.1}"
        );
    }
    assert_eq!(
        &r.brir.bus_emitters()[..11],
        &(0..11).map(Some).collect::<Vec<_>>()[..],
        "each bus on its own emitter"
    );
}

/// A hard-right noise object for the BRIR layout tests: `frames` blocks of
/// 40 samples, the stereo energy of the last half returned as `(L, R)`.
fn render_noise_object(r: &mut SpatialRenderer, frames: usize) -> (f32, f32) {
    let mut lcg: u32 = 0x1234_5678;
    let mut noise_block = move || -> Vec<f32> {
        (0..40)
            .map(|_| {
                lcg = lcg.wrapping_mul(1664525).wrapping_add(1013904223);
                (lcg >> 8) as f32 / (1u32 << 24) as f32 - 0.5
            })
            .collect()
    };
    let event = vec![SpatialChannelEvent {
        channel_idx: 0,
        is_bed: false,
        gain_db: Some(0.0),
        ramp_length: Some(40),
        size: Some([0.0, 0.0, 0.0]),
        position: Some([1.0, 0.0, 0.0]),
        sample_pos: Some(0),
    }];
    let (mut e_l, mut e_r) = (0.0f32, 0.0f32);
    for i in 0..frames {
        let pcm = noise_block();
        let events: &[SpatialChannelEvent] = if i == 0 { &event } else { &[] };
        let out = r.render_frame(&pcm, 1, events, Vec::new(), false).unwrap();
        if i >= frames / 2 {
            for s in out.samples.as_chunks::<2>().0 {
                e_l += s[0] * s[0];
                e_r += s[1] * s[1];
            }
        }
    }
    (e_l, e_r)
}

/// With a BRIR source on the headphones, the render pans onto the set's own
/// loudspeakers, not the editable layout: the topology is rebuilt on them
/// (offline, on the frame that selects the set), bus `n` is emitter `n`, the
/// LFE bus is direct, and the editable layout is left as it was. Back on an
/// HRTF source, or on the speakers, the editable layout renders again.
#[test]
fn a_brir_set_renders_on_its_own_loudspeakers() {
    let mut r = brir_layout_test_renderer(true);
    let editable = r.control.editable_layout();
    assert!(
        r.control.render_layout_outdated(),
        "a resident set the render uses outdates the editable layout's topology"
    );

    let (e_l, e_r) = render_noise_object(&mut r, 60);
    let topology = r.control.active_topology();
    assert!(topology.brir_layout, "the topology is the set's");
    let names: Vec<&str> = topology.speaker_layout.speaker_names();
    assert_eq!(names, ["C", "FL", "FR", "SL", "SR", "BL", "BR", "LFE"]);
    assert_eq!(
        r.brir.bus_emitters(),
        &[
            Some(0),
            Some(1),
            Some(2),
            Some(3),
            Some(4),
            Some(5),
            Some(6),
            // The LFE, then the stage's 4 unused channels: direct buses.
            None,
            None,
            None,
            None,
            None,
        ],
        "one bus per emitter, in the set's order"
    );
    assert!(!r.control.render_layout_outdated());
    assert_eq!(
        r.control.editable_layout(),
        editable,
        "the editable layout is never replaced"
    );
    assert!(
        e_r > 1.5 * e_l,
        "a hard-right object favours the right ear through the set: L {e_l:.4} R {e_r:.4}"
    );

    // Another source: the editable layout renders again.
    r.control.live.write().binaural.hrir_source = crate::binaural::HrirSource::Synthetic;
    render_noise_object(&mut r, 2);
    let topology = r.control.active_topology();
    assert!(!topology.brir_layout);
    assert_eq!(topology.speaker_layout, editable);

    // The set again, then the speakers: the physical layout, whatever the
    // headphone source.
    r.control.live.write().binaural.hrir_source =
        crate::binaural::HrirSource::Brir("synthetic-7.1.sofa".into());
    render_noise_object(&mut r, 2);
    assert!(r.control.active_topology().brir_layout);
    r.control.live.write().binaural.output_mode = crate::live_params::OutputMode::SpeakerArray;
    assert!(r.control.render_layout_outdated());
    let pcm = vec![0.0f32; 40];
    r.render_frame(&pcm, 1, &[], Vec::new(), false).unwrap();
    let topology = r.control.active_topology();
    assert!(!topology.brir_layout);
    assert_eq!(topology.speaker_layout, editable);
}

/// The editable layout's per-speaker rows (gain, mute, delay) belong to its
/// speakers: on a BRIR set's loudspeakers none applies — every row muted,
/// the set still sounds — while the HRTF virtual room still honours them.
#[test]
fn the_editable_layouts_speaker_rows_do_not_apply_to_a_brir_set() {
    let mute_every_row = |r: &SpatialRenderer| {
        {
            let mut live = r.control.live.write();
            for idx in 0..12 {
                live.speakers.entry(idx).or_default().muted = true;
            }
        }
        r.control.mark_speaker_params_dirty();
    };

    let mut r = brir_layout_test_renderer(true);
    mute_every_row(&r);
    let (e_l, e_r) = render_noise_object(&mut r, 60);
    assert!(r.control.active_topology().brir_layout);
    assert!(
        e_l + e_r > 0.0,
        "the set's loudspeakers ignore the editable layout's mutes"
    );

    let mut r = build_cascade_test_renderer(LiveEvaluationMode::PrecomputedCartesian, false);
    r.control.live.write().binaural.output_mode = crate::live_params::OutputMode::Binaural;
    r.control.live.write().binaural.mode = crate::live_params::BinauralMode::Cascaded;
    mute_every_row(&r);
    let (e_l, e_r) = render_noise_object(&mut r, 60);
    assert_eq!(
        e_l + e_r,
        0.0,
        "the HRTF virtual room is the editable layout, rows included"
    );
}

/// Bands built for a BRIR set are headphone-only: on a switch to the
/// speakers, until the speaker layout's bands are installed, the speaker
/// path is silent rather than sending the set's buses to the wrong outputs.
#[test]
fn the_speaker_path_is_silent_while_a_brir_sets_bands_are_installed() {
    let mut r = brir_layout_test_renderer(false);
    let plan = r.control.prepare_topology_rebuild().expect("plan");
    assert!(plan.brir_layout);
    let topology = plan.build_topology().unwrap();
    r.control.publish_topology(topology);
    r.prepare_speaker_stage().unwrap();
    assert!(
        r.speaker_stage
            .installed_topology()
            .is_some_and(|t| t.brir_layout)
    );

    r.control.live.write().binaural.output_mode = crate::live_params::OutputMode::SpeakerArray;
    let pcm = vec![0.5f32; 40];
    let event = vec![SpatialChannelEvent {
        channel_idx: 0,
        is_bed: false,
        gain_db: Some(0.0),
        ramp_length: Some(40),
        size: Some([0.0, 0.0, 0.0]),
        position: Some([1.0, 0.0, 0.0]),
        sample_pos: Some(0),
    }];
    let out = r.render_frame(&pcm, 1, &event, Vec::new(), false).unwrap();
    assert!(!out.samples.is_empty());
    assert!(
        out.samples.iter().all(|&s| s == 0.0),
        "no BRIR bus reaches a physical speaker"
    );
}

/// A set whose loudspeakers (with the LFE) outnumber the channels the
/// renderer was opened with cannot replace the layout: it is reported, and
/// the render stays on the editable layout (the nearest-emitter mapping).
#[test]
fn a_brir_set_wider_than_the_renderer_keeps_the_editable_layout() {
    use crate::binaural::brir_stage::test_support::synth_set;

    let mut r = build_cascade_test_renderer(LiveEvaluationMode::PrecomputedCartesian, false);
    r.set_synchronous_stage_builds(true);
    let path = "synthetic-wide.sofa";
    let opts = {
        let mut live = r.control.live.write();
        live.binaural.output_mode = crate::live_params::OutputMode::Binaural;
        live.binaural.hrir_source = crate::binaural::HrirSource::Brir(path.into());
        cascade::brir_load_options(&live.binaural)
    };
    // 12 emitters + the LFE: one more than the 7.1.4 renderer's 12 channels.
    let azimuths: Vec<f32> = (0..12).map(|i| i as f32 * 30.0).collect();
    r.brir
        .install_set_as(path, opts, synth_set(&azimuths, &[0.0], 400), 12);
    let error = r.control.brir_layout().expect_err("too wide");
    assert!(error.contains("13 virtual speakers"), "{error}");
    assert!(!r.control.render_layout_outdated());
    render_noise_object(&mut r, 2);
    assert!(!r.control.active_topology().brir_layout);
}

/// A set landing makes the next rebuild another layout, whatever asked for
/// it: an evaluation-only one (which does not bump the geometry) must not
/// reuse the editable layout's gain model for the set's loudspeakers.
#[test]
fn a_rebuild_onto_a_brir_set_never_reuses_the_editable_layouts_model() {
    let r = brir_layout_test_renderer(false);
    let current = r.control.active_topology();
    assert!(!current.brir_layout);
    let plan = r.control.prepare_topology_rebuild().expect("plan");
    assert!(plan.brir_layout);
    let topology = plan
        .build_topology_reusing(Some(&current))
        .expect("the set's layout gets a gain model of its own");
    assert!(topology.brir_layout);
    assert_eq!(topology.num_speakers, 8);
}

/// A real-time host with no OSC listener still moves onto a set's
/// loudspeakers: the renderer's own follower rebuilds the topology off the
/// render thread. While a host claims the rebuilds, it stands down.
#[test]
fn a_real_time_host_without_osc_moves_onto_the_brir_set() {
    let render_until =
        |r: &mut SpatialRenderer, secs: f32, done: &dyn Fn(&SpatialRenderer) -> bool| {
            let pcm = vec![0.0f32; 40];
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f32(secs);
            while std::time::Instant::now() < deadline {
                r.render_frame(&pcm, 1, &[], Vec::new(), false).unwrap();
                if done(r) {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            false
        };

    let mut r = brir_layout_test_renderer(false);
    assert!(
        render_until(&mut r, 5.0, &|r| r.control.active_topology().brir_layout),
        "the follower publishes the set's layout"
    );
    assert!(!r.control.render_layout_outdated());

    let mut r = brir_layout_test_renderer(false);
    r.control.set_relayout_by_host(true);
    assert!(
        !render_until(&mut r, 0.5, &|r| r.control.active_topology().brir_layout),
        "a host that claims the rebuilds is left to do them"
    );
    assert!(r.control.render_layout_outdated());
}

/// Synchronous stage builds (offline renders): a source change is live on the
/// frame that requests it, and the setting survives the stage rebuild a
/// sample-rate change does. Without it the same frame still renders the old
/// grid — the swap only happens on a later frame, whenever the worker is done.
#[test]
fn synchronous_stage_builds_land_on_the_requesting_frame() {
    let first_frame_pending = |synchronous: bool| {
        let mut r = build_cascade_test_renderer(LiveEvaluationMode::PrecomputedCartesian, false);
        r.set_synchronous_stage_builds(synchronous);
        r.set_sample_rate(44_100).unwrap();
        {
            let mut live = r.control.live.write();
            live.binaural.output_mode = crate::live_params::OutputMode::Binaural;
            live.binaural.hrir_source = crate::binaural::HrirSource::Synthetic;
        }
        let pcm = vec![0.0f32; 40];
        r.render_frame(&pcm, 1, &[], Vec::new(), false).unwrap();
        r.binaural_rebuild_pending()
    };
    assert!(
        !first_frame_pending(true),
        "a synchronous build is live on the frame that asked for it"
    );
    assert!(
        first_frame_pending(false),
        "the live path hands the build to the worker"
    );
}

/// A 7.1.4 renderer on a precomputed table, cartesian or polar. With
/// `band_limited` its first three speakers are band-limited, so objects render
/// through several crossover bands; without, through a single band. Either way
/// through the unified table (a test that wants the per-band path clears it).
/// Coarse grids: the tests that use it compare renders with each other, not
/// with a geometry.
pub(super) fn build_table_renderer(cartesian: bool, band_limited: bool) -> SpatialRenderer {
    let mut layout = SpeakerLayout::preset("7.1.4").unwrap();
    if band_limited {
        for (sp, cutoff) in layout.speakers.iter_mut().zip([80.0, 200.0, 500.0]) {
            sp.freq_low = Some(cutoff);
        }
    }
    let (table_mode, preferred, live) = if cartesian {
        (
            VbapTableMode::Cartesian {
                x_size: 15,
                y_size: 15,
                z_size: 7,
                z_neg_size: 7,
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
    let mut r = SpatialRenderer::new(RendererSpec {
        az_res_deg: 6,
        el_res_deg: 6,
        table_mode,
        preferred_evaluation_mode: preferred,
        initial_evaluation_mode: live,
        cartesian_default_x_size: 15,
        cartesian_default_y_size: 15,
        cartesian_default_z_size: 7,
        cartesian_default_z_neg_size: 7,
        ..test_support::spec(layout)
    })
    .unwrap();
    r.prepare_speaker_stage().unwrap();
    assert!(
        r.speaker_stage.unified_table.is_some(),
        "every precomputed layout renders through the unified table"
    );
    r
}

/// Deterministic noise in `[-0.25, 0.25]`, a different block each time.
pub(super) fn noise_block(n_channels: usize, sample_length: usize, block: usize) -> Vec<f32> {
    let base = (block * sample_length * n_channels) as u32;
    (0..(sample_length * n_channels) as u32)
        .map(|i| {
            let x = (base + i).wrapping_mul(2_654_435_761) ^ 0x9E37_79B9;
            ((x >> 8) & 0xffff) as f32 / 65535.0 * 0.5 - 0.25
        })
        .collect()
}

/// A layout without crossover renders through the unified table too, its one
/// band merged like a crossover's: the lookup localises the cell once and
/// reads the corner cache. It must render exactly what the band's own
/// evaluator renders — the same bits, in the sample ramp, objects moving.
#[test]
fn a_single_band_renders_the_same_bits_through_the_unified_table() {
    for cartesian in [true, false] {
        let mut unified = build_table_renderer(cartesian, false);
        assert!(unified.speaker_stage.unified_table.is_some(), "{cartesian}");
        let mut per_band = build_table_renderer(cartesian, false);
        per_band.speaker_stage.unified_table = None;
        for r in [&mut unified, &mut per_band] {
            r.control.live.write().options.ramp_mode = RampMode::Sample;
        }
        const OBJECTS: usize = 4;
        for block in 0..24 {
            let events = circling_events(OBJECTS, block, 120);
            let pcm = noise_block(OBJECTS, 40, block);
            let a = unified
                .render_frame(&pcm, OBJECTS, &events, Vec::new(), false)
                .unwrap();
            let b = per_band
                .render_frame(&pcm, OBJECTS, &events, Vec::new(), false)
                .unwrap();
            assert_eq!(a.samples.len(), b.samples.len());
            assert!(
                a.samples.iter().any(|x| x.abs() > 1e-3),
                "block {block} is silent"
            );
            let first = a
                .samples
                .iter()
                .zip(&b.samples)
                .position(|(x, y)| x.to_bits() != y.to_bits());
            assert_eq!(
                first,
                None,
                "cartesian {cartesian}, block {block}: unified {:?} vs per band {:?}",
                first.map(|i| a.samples[i]),
                first.map(|i| b.samples[i])
            );
        }
    }
}

/// One event per object on a slow circle round the listener, each object at
/// its own rate and height: successive `step`s start a new ramp of
/// `ramp_length` samples towards a nearby position.
fn circling_events(n_objects: usize, step: usize, ramp_length: u32) -> Vec<SpatialChannelEvent> {
    (0..n_objects)
        .map(|ch| {
            let degrees = ch as f64 * 37.0 + step as f64 * (0.4 + ch as f64 * 0.3);
            let az = degrees.to_radians();
            SpatialChannelEvent {
                channel_idx: ch,
                is_bed: false,
                gain_db: Some(0.0),
                ramp_length: Some(ramp_length),
                size: Some([0.0, 0.0, 0.0]),
                position: Some([0.9 * az.sin(), 0.9 * az.cos(), (ch % 4) as f64 * 0.3]),
                sample_pos: Some(0),
            }
        })
        .collect()
}

/// The per-channel cell caches must never change what is rendered: a renderer
/// whose caches are emptied before every block, so that every block refills
/// them from the table, renders the same bits as one that keeps them — in
/// every ramp mode, on both table geometries, and across a live switch to
/// nearest-cell lookups (which bypass the caches) and back.
#[test]
fn cell_caches_do_not_change_the_render() {
    const N_OBJECTS: usize = 6;
    const BLOCK: usize = 40;
    const BLOCKS_PER_MODE: usize = 15;
    const MODES: [RampMode; 4] = [
        RampMode::Sample,
        RampMode::Frame,
        RampMode::Interp,
        RampMode::Off,
    ];

    let render = |cartesian: bool, keep_caches: bool| -> Vec<u32> {
        let mut r = build_table_renderer(cartesian, true);
        let mut out = Vec::new();
        let mut buf = Vec::new();
        for block in 0..MODES.len() * BLOCKS_PER_MODE {
            {
                let mut live = r.control.live.write();
                live.options.ramp_mode = MODES[block / BLOCKS_PER_MODE];
                // Within each mode: trilinear, then nearest, then trilinear.
                live.evaluation.position_interpolation =
                    !(5..10).contains(&(block % BLOCKS_PER_MODE));
            }
            if !keep_caches {
                for cache in &mut r.speaker_stage.table_caches {
                    cache.invalidate();
                }
            }
            // Objects move on most blocks and hold still on some, so both the
            // ramping and the settled lookups are covered.
            let events = if block % 4 == 3 {
                Vec::new()
            } else {
                circling_events(N_OBJECTS, block, BLOCK as u32)
            };
            let pcm = noise_block(N_OBJECTS, BLOCK, block);
            let frame = r
                .render_frame(&pcm, N_OBJECTS, &events, buf, false)
                .expect("render_frame");
            out.extend(frame.samples.iter().map(|v| v.to_bits()));
            buf = frame.samples;
        }
        assert!(
            r.speaker_stage.table_caches.len() >= N_OBJECTS,
            "every object channel must own a cell cache"
        );
        out
    };

    for cartesian in [true, false] {
        let kept = render(cartesian, true);
        let refilled = render(cartesian, false);
        let per_mode = kept.len() / MODES.len();
        for (m, mode) in MODES.iter().enumerate() {
            let span = m * per_mode..(m + 1) * per_mode;
            assert!(
                kept[span.clone()] == refilled[span.clone()],
                "{mode:?} (cartesian={cartesian}): cached cells changed the render"
            );
            assert!(
                kept[span].iter().any(|&bits| f32::from_bits(bits) != 0.0),
                "{mode:?} (cartesian={cartesian}): the scene rendered silence"
            );
        }
    }
}

/// The gains the published topology's model pans a normalized position with,
/// in layout speaker order (the LFE's entry stays 0), in the fixture's room.
fn topology_gains(topology: &crate::live_params::RenderTopology, position: [f32; 3]) -> Vec<f32> {
    let gains = topology
        .backend
        .gains_at(&crate::render_backend::RenderRequest {
            adm_position: [position[0] as f64, position[1] as f64, position[2] as f64],
            event_size: [0.0; 3],
            room_ratio: [1.0, 2.0, 0.5],
            room_ratio_rear: 2.0,
            room_ratio_lower: 0.5,
            room_ratio_center_blend: 0.0,
            use_distance_diffuse: false,
            distance_diffuse_threshold: 1.0,
            distance_diffuse_curve: 1.0,
            diffuse_mirror_axes: crate::spatial_vbap::MirrorAxes::default(),
            distance_model: crate::spatial_vbap::DistanceModel::None,
        });
    (0..topology.num_speakers)
        .map(|speaker| {
            topology
                .backend_speaker_index_for_layout_speaker(speaker)
                .map_or(0.0, |i| gains[i])
        })
        .collect()
}

/// The convex hull split every planar face of a layout — the rear quad, the
/// ceiling — along a diagonal picked by its tie-breaking jitter, so an object
/// dead centre at the rear of a 9.1.6 played 0.81 / 0.30 on the two backs
/// and a position's mirror image did not get the mirrored gains. Each such
/// face is now fanned around a virtual centre
/// (`spatial_vbap::vbap_native::Triangulation`): a position and its mirror
/// across the median plane pan alike, and a position on that plane plays
/// each left/right pair alike.
#[test]
fn a_position_and_its_mirror_pan_alike_across_planar_faces() {
    let probes: [[f32; 3]; 12] = [
        [0.4, -0.9, 0.0],
        [0.4, -0.9, 0.5],
        [0.0, -0.9, 0.5],
        [0.3, 0.2, 0.95],
        [0.0, -0.3, 0.9],
        [0.0, 0.0, 1.0],
        [0.0, 0.3, 0.9],
        [0.9, 0.0, 0.7],
        [0.4, 0.95, 0.6],
        [0.6, -0.6, 0.8],
        [0.2, -1.0, 0.3],
        [0.5, -0.5, -0.4],
    ];
    let mirror = |name: &str| -> String {
        if let Some(stem) = name.strip_suffix('L') {
            format!("{stem}R")
        } else if let Some(stem) = name.strip_suffix('R') {
            format!("{stem}L")
        } else {
            name.to_string()
        }
    };
    for preset in ["7.1.4", "9.1.6"] {
        let layout = SpeakerLayout::preset(preset).unwrap();
        let r = SpatialRenderer::new(test_support::spec(layout)).unwrap();
        let control = r.renderer_control();
        let topology = control.active_topology();
        let names: Vec<&str> = topology
            .speaker_layout
            .speakers
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        let index_of = |name: &str| {
            names
                .iter()
                .position(|n| *n == name)
                .unwrap_or_else(|| panic!("{preset} has no {name}"))
        };
        for p in probes {
            let g = topology_gains(&topology, p);
            let m = topology_gains(&topology, [-p[0], p[1], p[2]]);
            for (i, name) in names.iter().enumerate() {
                let j = index_of(&mirror(name));
                assert!(
                    (g[i] - m[j]).abs() < 1e-4,
                    "{preset} {p:?}: {name} {} vs {} {} on the mirror",
                    g[i],
                    names[j],
                    m[j]
                );
            }
        }
        // Dead centre at the rear, half way up: the rear face's four
        // loudspeakers, each left/right pair alike.
        let g = topology_gains(&topology, [0.0, -0.9, 0.5]);
        for (left, right) in [("BL", "BR"), ("TBL", "TBR")] {
            let (l, r) = (g[index_of(left)], g[index_of(right)]);
            assert!(
                (l - r).abs() < 1e-4 && l > 0.1,
                "{preset}: {left} {l} vs {right} {r}"
            );
        }
    }
}
