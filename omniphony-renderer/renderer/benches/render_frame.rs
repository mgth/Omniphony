//! Reliable, offline baseline benchmarks for the spatial render hot path.
//!
//! These run entirely in-process against the public `renderer` API — no bridge,
//! no mpv, no PipeWire — so they are deterministic and reproducible across
//! machines and CI. They exist to *quantify* the render-time spikes observed in
//! the live meter (`render_time_ms` avg ≈ 0.15, max ≈ 0.25) by isolating the two
//! suspected amplitude drivers one factor at a time:
//!
//!   * `render_steady/<n_objects>` — cost of a frame carrying NO spatial
//!     metadata, swept over the number of simultaneously active object channels.
//!     Confirms hypothesis #1: steady render cost scales with active objects.
//!
//!   * `render_metadata_frame/<n_objects>` — same object count, but every object
//!     moves this frame (worst-case OAMD block: `update_metadata` + fresh ramps).
//!     Confirms hypothesis #2: metadata-bearing frames cost more than steady ones.
//!
//!   * `render_ramp_mode/<frame|sample>` — at a fixed object count, the cost of
//!     the ramp mode itself. `Frame` is the live mpv default after the engine
//!     parity fix; `Sample` is the old per-sample `compute_gains` behaviour.
//!
//! Run with:  cargo bench -p renderer
//! A single scenario:  cargo bench -p renderer -- render_steady/32

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use dsp_fixtures::scene::{
    build_renderer, crossover_layout, drift_events, make_pcm, move_events, prepared,
    prepared_binaural, prepared_binaural_cascaded,
};
use renderer::live_params::RampMode;

fn bench_steady(c: &mut Criterion) {
    let mut group = c.benchmark_group("render_steady");
    for &n in &[1usize, 8, 16, 32, 64, 118] {
        let (mut r, pcm) = prepared("7.1.4", n, RampMode::Frame, false, false);
        let mut buf = Vec::new();
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter(|| {
                let f = r
                    .render_frame(
                        black_box(&pcm),
                        black_box(n),
                        &[],
                        std::mem::take(&mut buf),
                        false,
                    )
                    .expect("render");
                buf = f.samples;
                black_box(&buf);
            });
        });
    }
    group.finish();
}

fn bench_metadata_frame(c: &mut Criterion) {
    let mut group = c.benchmark_group("render_metadata_frame");
    for &n in &[1usize, 8, 16, 32, 64, 118] {
        let (mut r, pcm) = prepared("7.1.4", n, RampMode::Frame, false, false);
        let mut buf = Vec::new();
        let mut round = 1u64;
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter(|| {
                // Every iteration moves all objects → exercises update_metadata +
                // fresh ramps, the worst-case OAMD block.
                let events = move_events(n, round);
                round = round.wrapping_add(1);
                let f = r
                    .render_frame(
                        black_box(&pcm),
                        black_box(n),
                        &events,
                        std::mem::take(&mut buf),
                        false,
                    )
                    .expect("render");
                buf = f.samples;
                black_box(&buf);
            });
        });
    }
    group.finish();
}

/// Quantify the cost of the ramp mode itself at a fixed object count. `Frame`
/// is the live mpv default after the engine parity fix; `Sample` is the old
/// (per-sample `compute_gains`) behaviour the embedded host used to run in.
fn bench_ramp_mode(c: &mut Criterion) {
    let mut group = c.benchmark_group("render_ramp_mode");
    const N: usize = 32;
    for (label, mode) in [
        ("frame", RampMode::Frame),
        ("sample", RampMode::Sample),
        ("interp", RampMode::Interp),
    ] {
        let (mut r, pcm) = prepared("7.1.4", N, mode, false, false);
        let mut buf = Vec::new();
        let mut round = 1u64;
        group.bench_function(label, |b| {
            b.iter(|| {
                let events = move_events(N, round);
                round = round.wrapping_add(1);
                let f = r
                    .render_frame(
                        black_box(&pcm),
                        black_box(N),
                        &events,
                        std::mem::take(&mut buf),
                        false,
                    )
                    .expect("render");
                buf = f.samples;
                black_box(&buf);
            });
        });
    }
    group.finish();
}

/// The realistic Sample-mode common case: objects are NOT moving this block (no
/// metadata — ~97% of real frames). `frame` recomputes gains once; `sample`
/// recomputes per sample. Since the position is constant, the per-sample
/// `compute_gains` calls are redundant — this is what the static early-out
/// targets. Contrast with `render_ramp_mode` (objects move every frame).
fn bench_static(c: &mut Criterion) {
    let mut group = c.benchmark_group("render_static");
    const N: usize = 32;
    for (label, mode) in [
        ("frame", RampMode::Frame),
        ("sample", RampMode::Sample),
        ("interp", RampMode::Interp),
    ] {
        let (mut r, pcm) = prepared("7.1.4", N, mode, false, false);
        let mut buf = Vec::new();
        group.bench_function(label, |b| {
            b.iter(|| {
                let f = r
                    .render_frame(
                        black_box(&pcm),
                        black_box(N),
                        &[],
                        std::mem::take(&mut buf),
                        false,
                    )
                    .expect("render");
                buf = f.samples;
                black_box(&buf);
            });
        });
    }
    group.finish();
}

/// The genuinely-moving case: position interpolation ON and a fresh ramp armed
/// every block, so the object's interpolated position changes every sample and
/// `Sample` must recompute the VBAP gains per sample. This is where the cost
/// distribution shows: `sample` pays N × `compute_gains`, `interp` pays one
/// `compute_gains` plus a per-sample gain lerp, `frame` pays one `compute_gains`
/// and no per-sample smoothing.
fn bench_moving(c: &mut Criterion) {
    let mut group = c.benchmark_group("render_moving");
    const N: usize = 32;
    for (label, mode) in [
        ("frame", RampMode::Frame),
        ("sample", RampMode::Sample),
        ("interp", RampMode::Interp),
    ] {
        let (mut r, pcm) = prepared("7.1.4", N, mode, true, false);
        let mut buf = Vec::new();
        let mut round = 1u64;
        group.bench_function(label, |b| {
            b.iter(|| {
                let events = move_events(N, round);
                round = round.wrapping_add(1);
                let f = r
                    .render_frame(
                        black_box(&pcm),
                        black_box(N),
                        &events,
                        std::mem::take(&mut buf),
                        false,
                    )
                    .expect("render");
                buf = f.samples;
                black_box(&buf);
            });
        });
    }
    group.finish();
}

/// Same moving scenario as `render_moving` but with the precomputed CARTESIAN
/// table/evaluator (trilinear `sample_cartesian_table`) instead of polar, to
/// measure and optimise the cartesian `compute_gains` lookup specifically.
fn bench_cartesian(c: &mut Criterion) {
    let mut group = c.benchmark_group("render_cartesian");
    const N: usize = 32;
    for (label, mode) in [
        ("frame", RampMode::Frame),
        ("sample", RampMode::Sample),
        ("interp", RampMode::Interp),
    ] {
        let (mut r, pcm) = prepared("7.1.4", N, mode, true, true);
        let mut buf = Vec::new();
        let mut round = 1u64;
        group.bench_function(label, |b| {
            b.iter(|| {
                let events = move_events(N, round);
                round = round.wrapping_add(1);
                let f = r
                    .render_frame(
                        black_box(&pcm),
                        black_box(N),
                        &events,
                        std::mem::take(&mut buf),
                        false,
                    )
                    .expect("render");
                buf = f.samples;
                black_box(&buf);
            });
        });
    }
    group.finish();
}

/// Multi-band crossover (mixed speaker sizes) over the cartesian table, moving
/// case. Each frequency band runs its own table lookup at the SAME object
/// position, so the per-band cell localisation is currently recomputed N times.
/// This is the scenario where sharing the localisation across bands would pay
/// off; it also shows how cost scales with band count.
fn bench_crossover(c: &mut Criterion) {
    let mut group = c.benchmark_group("render_crossover");
    const N: usize = 32;
    for (label, mode) in [
        ("frame", RampMode::Frame),
        ("sample", RampMode::Sample),
        ("interp", RampMode::Interp),
    ] {
        let mut r = build_renderer(crossover_layout(), true, true);
        {
            let ctrl = r.renderer_control();
            ctrl.set_requested_ramp_mode(mode);
            ctrl.live.write().ramp_mode = mode;
        }
        let pcm = make_pcm(N);
        let init = move_events(N, 0);
        let mut buf = Vec::new();
        for _ in 0..4 {
            let f = r.render_frame(&pcm, N, &init, buf, false).expect("prime");
            buf = f.samples;
        }
        let mut round = 1u64;
        group.bench_function(label, |b| {
            b.iter(|| {
                let events = move_events(N, round);
                round = round.wrapping_add(1);
                let f = r
                    .render_frame(
                        black_box(&pcm),
                        black_box(N),
                        &events,
                        std::mem::take(&mut buf),
                        false,
                    )
                    .expect("render");
                buf = f.samples;
                black_box(&buf);
            });
        });
    }
    group.finish();
}

/// Same moving scenario as `bench_cartesian` but over the precomputed POLAR
/// table/evaluator (`sample_polar_table`), to measure and optimise the polar
/// `compute_gains` lookup (wrapped azimuth + elevation/distance brackets).
fn bench_polar(c: &mut Criterion) {
    let mut group = c.benchmark_group("render_polar");
    const N: usize = 32;
    for (label, mode) in [
        ("frame", RampMode::Frame),
        ("sample", RampMode::Sample),
        ("interp", RampMode::Interp),
    ] {
        let (mut r, pcm) = prepared("7.1.4", N, mode, true, false);
        let mut buf = Vec::new();
        let mut round = 1u64;
        group.bench_function(label, |b| {
            b.iter(|| {
                let events = move_events(N, round);
                round = round.wrapping_add(1);
                let f = r
                    .render_frame(
                        black_box(&pcm),
                        black_box(N),
                        &events,
                        std::mem::take(&mut buf),
                        false,
                    )
                    .expect("render");
                buf = f.samples;
                black_box(&buf);
            });
        });
    }
    group.finish();
}

/// Multi-band crossover (mixed speaker sizes) over the POLAR table, moving case —
/// the polar counterpart of `bench_crossover`. Each band currently runs its own
/// polar lookup at the same position, so cost scales with band count until the
/// unified multi-band polar table shares the localisation.
fn bench_polar_crossover(c: &mut Criterion) {
    let mut group = c.benchmark_group("render_polar_crossover");
    const N: usize = 32;
    for (label, mode) in [
        ("frame", RampMode::Frame),
        ("sample", RampMode::Sample),
        ("interp", RampMode::Interp),
    ] {
        let mut r = build_renderer(crossover_layout(), true, false);
        {
            let ctrl = r.renderer_control();
            ctrl.set_requested_ramp_mode(mode);
            ctrl.live.write().ramp_mode = mode;
        }
        let pcm = make_pcm(N);
        let init = move_events(N, 0);
        let mut buf = Vec::new();
        for _ in 0..4 {
            let f = r.render_frame(&pcm, N, &init, buf, false).expect("prime");
            buf = f.samples;
        }
        let mut round = 1u64;
        group.bench_function(label, |b| {
            b.iter(|| {
                let events = move_events(N, round);
                round = round.wrapping_add(1);
                let f = r
                    .render_frame(
                        black_box(&pcm),
                        black_box(N),
                        &events,
                        std::mem::take(&mut buf),
                        false,
                    )
                    .expect("render");
                buf = f.samples;
                black_box(&buf);
            });
        });
    }
    group.finish();
}

/// Direct per-object binaural vs the cascaded virtual-speaker stage
/// (issue #220), across object counts.
///
/// `static` renders with no metadata events after priming — the steady state,
/// where the direct path still convolves one HRIR pair per object while the
/// cascade convolves its fixed virtual speakers whatever `n` is. `moving`
/// re-arms a ramp every block, adding the per-block HRIR refresh (direct) vs
/// the per-block virtual re-pan (cascaded) on top.
///
/// `drifting` is the one that reflects real content: `moving` redraws every
/// position at random each block, which is not motion but teleportation, and
/// it hides any benefit from direction coherence between blocks. Read
/// `drifting` for the expected case and `moving` as the pathological bound.
fn bench_binaural(c: &mut Criterion) {
    for scenario in ["static", "moving", "drifting"] {
        let mut group = c.benchmark_group(format!("render_binaural_{scenario}"));
        for &n in &[1usize, 8, 16, 32, 64, 118] {
            for (label, cascaded) in [("direct", false), ("cascaded", true)] {
                let (mut r, pcm) = if cascaded {
                    prepared_binaural_cascaded(n, RampMode::Frame)
                } else {
                    prepared_binaural(n, RampMode::Frame)
                };
                let mut buf = Vec::new();
                let mut round = 1u64;
                group.bench_function(BenchmarkId::new(label, n), |b| {
                    b.iter(|| {
                        let events = match scenario {
                            "moving" => move_events(n, round),
                            "drifting" => drift_events(n, round),
                            _ => Vec::new(),
                        };
                        round = round.wrapping_add(1);
                        let f = r
                            .render_frame(
                                black_box(&pcm),
                                black_box(n),
                                &events,
                                std::mem::take(&mut buf),
                                false,
                            )
                            .expect("render");
                        buf = f.samples;
                        black_box(&buf);
                    });
                });
            }
        }
        group.finish();
    }
}

criterion_group!(
    benches,
    bench_steady,
    bench_metadata_frame,
    bench_ramp_mode,
    bench_static,
    bench_moving,
    bench_cartesian,
    bench_crossover,
    bench_polar,
    bench_polar_crossover,
    bench_binaural
);
criterion_main!(benches);
