//! Worst-case block-time gate.
//!
//! A real-time renderer is not judged on mean throughput — it is judged on the
//! slowest block, because one block over the deadline is an audible dropout
//! while a thousand fast ones buy nothing. So this measures a high percentile
//! of per-block render time, not an average.
//!
//! **Why this is not flaky.** CI runners vary by several times in speed, so an
//! absolute microsecond budget would either be so tight it fails on a noisy
//! runner or so loose it catches nothing. The budget is instead expressed as a
//! fraction of the *block period* — the wall time the block's audio actually
//! occupies. That ratio is what determines whether the engine keeps up, it is
//! the quantity a regression would move, and it leaves enough headroom that
//! ordinary runner variance cannot reach it.
//!
//! Scenes come from `dsp_fixtures::scene`, the same generators the null tests
//! and the criterion benches use, so a regression here refers to the same
//! workload they do.
//!
//! **Release only.** `cargo test` builds in debug, where this same scene takes
//! 97 % of the block period instead of 4 % — a ~23× difference that says
//! nothing about the shipped binary. Gating an unoptimised build would be
//! measuring the wrong thing, so these are compiled out unless
//! `debug_assertions` is off:
//!
//! ```sh
//! cargo test --release -p renderer -- spatial_renderer::perf_gate --nocapture
//! ```
//!
//! That keeps them out of the debug test run. CI runs them in steps of their
//! own (`ci.yml`, "Run the perf gate"), on the release build it already makes
//! for the SIMD bit-identity tests.
//!
//! **Run alone.** They are additionally behind the `perf-gate` feature, because
//! a timing measurement must not share the machine with the rest of the suite
//! running in parallel — the high percentile then reports scheduler contention
//! rather than renderer cost, and the gate flakes. Observed: `block_time_steady`
//! failed inside a full `cargo test --release --workspace` while passing
//! comfortably on its own.
//!
//! ```sh
//! cargo test --release -p renderer --features perf-gate -- --test-threads=1 --nocapture
//! ```

#![cfg(not(debug_assertions))]

use std::time::Instant;

use dsp_fixtures::scene::{
    BLOCK_SAMPLES, CrossoverType, RampMode, SAMPLE_RATE, make_pcm, move_events, prepared,
    prepared_crossover,
};

/// Wall time one block of audio occupies: 40 samples at 48 kHz ≈ 833 µs.
const BLOCK_PERIOD_US: f64 = BLOCK_SAMPLES as f64 * 1e6 / SAMPLE_RATE as f64;

/// Fraction of the block period the slowest blocks may consume.
///
/// Measured headroom on a developer machine is ~2 % at 32 objects, so 25 %
/// tolerates a runner an order of magnitude slower while still catching any
/// regression that costs more than a few times the current budget.
const MAX_BLOCK_FRACTION: f64 = 0.25;

/// Blocks rendered per case. Enough that the percentile is meaningful without
/// adding real time to the suite.
const BLOCKS: usize = 2_000;

/// Objects in the gated scene.
const N_OBJECTS: usize = 32;

/// Percentile reported and gated. Not the max: a single outlier is usually the
/// OS descheduling the thread, not the renderer.
const PERCENTILE: f64 = 0.999;

fn percentile_us(mut samples: Vec<f64>, p: f64) -> f64 {
    samples.sort_by(|a, b| a.partial_cmp(b).expect("finite timings"));
    let idx = ((samples.len() as f64 - 1.0) * p).round() as usize;
    samples[idx]
}

/// Render `BLOCKS` blocks, timing each one individually.
fn block_times_us(move_every: usize) -> Vec<f64> {
    let (mut r, _) = prepared("7.1.4", N_OBJECTS, RampMode::Frame, true, false);
    let pcm = make_pcm(N_OBJECTS);
    let mut buf = Vec::new();
    let mut times = Vec::with_capacity(BLOCKS);
    for block in 0..BLOCKS {
        let events = if move_every > 0 && block % move_every == 0 {
            move_events(N_OBJECTS, block as u64 + 1)
        } else {
            Vec::new()
        };
        let start = Instant::now();
        let frame = r
            .render_frame(&pcm, N_OBJECTS, &events, buf, false)
            .expect("render_frame");
        times.push(start.elapsed().as_secs_f64() * 1e6);
        buf = frame.samples;
        buf.clear();
    }
    times
}

fn assert_within_budget(label: &str, times: Vec<f64>) {
    let p = percentile_us(times, PERCENTILE);
    let fraction = p / BLOCK_PERIOD_US;
    println!(
        "[measure] block_time {label}: p{:.1} = {p:.1} µs of a {BLOCK_PERIOD_US:.1} µs block \
         period ({:.1} % of real time, budget {:.0} %)",
        PERCENTILE * 100.0,
        fraction * 100.0,
        MAX_BLOCK_FRACTION * 100.0
    );
    // A wall-clock percentile also measures the machine, so this assertion is
    // only meaningful under the run conditions in the module doc: release
    // build, `--test-threads=1`, nothing else on the machine. The same scene
    // read 4.2 % on an idle box and 167 % while other test binaries ran.
    assert!(
        fraction <= MAX_BLOCK_FRACTION,
        "{label}: slowest blocks take {:.1} % of the block period (p{:.1} = {p:.1} µs of \
         {BLOCK_PERIOD_US:.1} µs), over the {:.0} % budget. The renderer is no longer \
         comfortably faster than real time for {N_OBJECTS} objects.",
        fraction * 100.0,
        PERCENTILE * 100.0,
        MAX_BLOCK_FRACTION * 100.0,
    );
}

/// Steady state: objects already placed, no metadata this block.
#[cfg(not(debug_assertions))]
#[test]
fn block_time_steady_is_within_budget() {
    assert_within_budget("steady", block_times_us(0));
}

/// Worst case: every object moves every block, so `update_metadata` runs and
/// every ramp restarts. This is the block that has to make the deadline.
#[cfg(not(debug_assertions))]
#[test]
fn block_time_all_objects_moving_is_within_budget() {
    assert_within_budget("all-moving", block_times_us(1));
}

/// Topology changes gated: a speaker edit or a backend switch republishes the
/// topology mid-stream.
const TOPOLOGY_CHANGES: usize = 8;

/// Render topology changes on `$r` mid-stream (a speaker moved in Studio) and
/// gate the blocks from each publish until the new band set is installed, the
/// first block mixed on it included. They keep the deadline because the set
/// is built by the stage's worker — gain tables, crossover bank and filter
/// memory — and not by the render thread.
///
/// Gated on the median over the changes of each one's slowest block: a build
/// back on the render thread blows every change, an OS hiccup only one.
///
/// A macro, not a function: the fixture's renderer is another instance of
/// this crate (the dev-dependency cycle), whose type cannot be named here.
#[cfg(not(debug_assertions))]
macro_rules! assert_topology_changes_within_budget {
    ($label:expr, $r:ident) => {{
        let control = $r.renderer_control();
        let pcm = make_pcm(N_OBJECTS);
        let mut buf = Vec::new();
        macro_rules! render {
            () => {{
                let start = Instant::now();
                let frame = $r
                    .render_frame(&pcm, N_OBJECTS, &[], std::mem::take(&mut buf), false)
                    .expect("render_frame");
                let us = start.elapsed().as_secs_f64() * 1e6;
                buf = frame.samples;
                buf.clear();
                us
            }};
        }
        let mut worst_per_change = Vec::with_capacity(TOPOLOGY_CHANGES);
        for _ in 0..TOPOLOGY_CHANGES {
            // The recompute thread's part, untimed: the topology it publishes.
            control.bump_geometry_generation();
            let plan = control.prepare_topology_rebuild().expect("plan");
            let topology = plan
                .build_topology_reusing(Some(&control.active_topology()))
                .expect("topology");
            let builds = $r.speaker_stage_builds();
            control.publish_topology(topology);

            let mut worst = render!();
            let deadline = Instant::now() + std::time::Duration::from_secs(60);
            while $r.speaker_stage_builds() == builds {
                assert!(Instant::now() < deadline, "the band worker never delivered");
                worst = worst.max(render!());
            }
            // The block after the install, which runs on the new bands.
            worst_per_change.push(worst.max(render!()));
        }
        let median = percentile_us(worst_per_change.clone(), 0.5);
        println!(
            "[measure] block_time {}: worst block per change {worst_per_change:.1?} µs",
            $label
        );
        assert!(
            median / BLOCK_PERIOD_US <= MAX_BLOCK_FRACTION,
            "{}: the slowest block around a change takes {:.1} % of the block period \
             (median over {TOPOLOGY_CHANGES} changes, {median:.1} µs of {BLOCK_PERIOD_US:.1} µs), \
             over the {:.0} % budget: is the band build back on the render thread?",
            $label,
            median / BLOCK_PERIOD_US * 100.0,
            MAX_BLOCK_FRACTION * 100.0,
        );
    }};
}

/// A layout without crossover: the one band's gain table is what a change
/// rebuilds.
#[cfg(not(debug_assertions))]
#[test]
fn block_time_across_a_topology_change_is_within_budget() {
    let (mut r, _) = prepared("7.1.4", N_OBJECTS, RampMode::Frame, true, false);
    assert_topology_changes_within_budget!("topology-change", r);
}

/// A crossover layout on the linear-phase engine, the costly one to swap: a
/// change also rebuilds the FIR bank and the filter memory of every object,
/// each a set of FFT buffers. Allocated on the first block mixed on the new
/// set, and the old ones freed there, they alone took that block over the
/// deadline.
#[cfg(not(debug_assertions))]
#[test]
fn block_time_across_a_topology_change_with_the_fir_crossover_is_within_budget() {
    let (mut r, pcm) = prepared_crossover(N_OBJECTS, RampMode::Frame);
    r.renderer_control().live.write().options.crossover_type = CrossoverType::Fir;
    // Onto the FIR bank, and past its first blocks.
    let builds = r.speaker_stage_builds();
    let deadline = Instant::now() + std::time::Duration::from_secs(60);
    let mut settled = 0;
    while settled < 64 {
        assert!(Instant::now() < deadline, "the band worker never delivered");
        r.render_frame(&pcm, N_OBJECTS, &[], Vec::new(), false)
            .expect("render_frame");
        if r.speaker_stage_builds() > builds {
            settled += 1;
        }
    }
    assert_topology_changes_within_budget!("topology-change, FIR crossover", r);
}
