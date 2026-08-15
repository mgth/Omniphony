//! Null tests: render fixed scenes and compare against committed goldens.
//!
//! These are the safety net for render-path refactors. A change that preserves
//! behaviour leaves the peak residual below the gate; a change that alters a
//! single sample audibly does not.
//!
//! Regenerate after an intended change:
//!   OMNIPHONY_BLESS_GOLDENS=1 cargo test -p renderer
//! and quote the printed residual in the pull request.

use dsp_fixtures::golden::{
    BINAURAL_RESIDUAL_GATE_DBFS, assert_matches_golden, assert_matches_golden_with_gate,
};
use dsp_fixtures::scene::{RampMode, make_pcm, prepared, render_blocks};

/// 0.125 s at 48 kHz. `GAIN_SLEW_SECS` is 0.02, so this covers the 20 ms
/// fade-in plus ~105 ms of steady motion.
///
/// Halved from 300 blocks to hold the suite inside its time budget: at 300 the
/// three null tests took 2.20 s and `cargo test --workspace` reached 9.96 s
/// against a 10 s ceiling.
const BLOCKS: usize = 150;

/// Fresh movement events every 8th block, so the golden exercises both the
/// ramping path and the steady path.
const MOVE_EVERY: usize = 8;

const N_OBJECTS: usize = 32;

#[test]
fn null_speaker_714_32obj() {
    let (mut r, _) = prepared("7.1.4", N_OBJECTS, RampMode::Frame, true, false);
    let pcm = make_pcm(N_OBJECTS);
    let out = render_blocks(&mut r, &pcm, N_OBJECTS, BLOCKS, MOVE_EVERY);
    let channels = out.len() / (BLOCKS * dsp_fixtures::scene::BLOCK_SAMPLES);
    assert_eq!(channels, 12, "7.1.4 must render 12 speaker channels");
    assert_matches_golden("speaker_714_32obj", &out, channels);
}

#[test]
fn null_binaural_kemar() {
    let (mut r, pcm) = dsp_fixtures::scene::prepared_binaural(N_OBJECTS, RampMode::Frame);
    let out = render_blocks(&mut r, &pcm, N_OBJECTS, BLOCKS, MOVE_EVERY);
    let channels = out.len() / (BLOCKS * dsp_fixtures::scene::BLOCK_SAMPLES);
    assert_eq!(channels, 2, "the binaural path must render 2 channels");
    // Wider gate than the mix paths: HRIR convolution's cross-host FP noise
    // exceeds −120 dBFS on its own (see `BINAURAL_RESIDUAL_GATE_DBFS`).
    assert_matches_golden_with_gate(
        "binaural_kemar",
        &out,
        channels,
        BINAURAL_RESIDUAL_GATE_DBFS,
    );
}

#[test]
fn null_crossover_bands() {
    let (mut r, pcm) = dsp_fixtures::scene::prepared_crossover(N_OBJECTS, RampMode::Frame);
    let out = render_blocks(&mut r, &pcm, N_OBJECTS, BLOCKS, MOVE_EVERY);
    let channels = out.len() / (BLOCKS * dsp_fixtures::scene::BLOCK_SAMPLES);
    assert_eq!(
        channels, 12,
        "crossover_layout is 7.1.4 band-limited — still 12 speakers"
    );
    assert_matches_golden("crossover_bands", &out, channels);
}

/// Half the channels never receive metadata.
///
/// The other scenes send events for every object, so none of them exercises
/// what a channel does with no cached metadata at all.
///
/// Honest scope: this scene does *not* discriminate the one case that motivated
/// it. Filtering the channel-state lookup on `initialized` — which makes the
/// "missing cached metadata, skipping" arm reachable — leaves this golden
/// unchanged, because such a channel still reads a default 0 dB gain and
/// renders the same either way. The scene is kept for the coverage it does add:
/// it is the only null test where the object count and the metadata count
/// differ, which is the shape a real stream takes when objects appear
/// mid-stream.
#[test]
fn null_partial_metadata() {
    let (mut r, _) = prepared("7.1.4", N_OBJECTS, RampMode::Frame, true, false);
    let pcm = make_pcm(N_OBJECTS);
    let out = dsp_fixtures::scene::render_blocks_partial_metadata(
        &mut r,
        &pcm,
        N_OBJECTS,
        N_OBJECTS / 2,
        BLOCKS,
        MOVE_EVERY,
    );
    let channels = out.len() / (BLOCKS * dsp_fixtures::scene::BLOCK_SAMPLES);
    assert_eq!(channels, 12, "7.1.4 must render 12 speaker channels");
    assert_matches_golden("partial_metadata", &out, channels);
}

/// `reset_runtime_state` must erase the previous stream.
///
/// The channel-state refactor changed this from a synchronous clear under the
/// mutex to a flag consumed at the top of the next `render_frame`. That is what
/// keeps the render path lock-free, but it moves *when* the clear happens, and
/// four call sites outside the renderer depend on it
/// (`orender_engine/src/engine.rs` 649/810/871,
/// `src/cli/decode/spatial_metadata.rs:86` — decoder-reset and stream-restart
/// paths).
///
/// Two renderers are given *different* prior content, both reset, then fed
/// identical blocks. If a reset erases the previous stream, the outputs match.
///
/// Comparing a reset renderer against a *fresh* one instead would be wrong, and
/// reports a −20.3 dBFS difference that looks like a leak: `prepared` primes
/// four rounds of events, so a fresh renderer carries a ramped-up `slewed_gain`
/// while a reset one restarts from silence. That difference is the documented
/// fade-in, not leakage.
#[test]
fn reset_runtime_state_erases_the_previous_stream() {
    const WARMUP: usize = 40;
    const AFTER: usize = 60;

    let pcm = make_pcm(N_OBJECTS);

    let render_after_reset = |warmup_move_every: usize| -> Vec<f32> {
        let (mut r, _) = prepared("7.1.4", N_OBJECTS, RampMode::Frame, true, false);
        let _ = render_blocks(&mut r, &pcm, N_OBJECTS, WARMUP, warmup_move_every);
        r.reset_runtime_state();
        render_blocks(&mut r, &pcm, N_OBJECTS, AFTER, MOVE_EVERY)
    };

    // Different histories: objects moving every 4th block versus every block.
    let a = render_after_reset(4);
    let b = render_after_reset(1);

    assert_eq!(
        a.len(),
        b.len(),
        "reset renderers produced different lengths"
    );
    let residual = dsp_fixtures::residual::peak_residual_dbfs(&a, &b);
    assert!(
        residual <= dsp_fixtures::golden::RESIDUAL_GATE_DBFS,
        "two renderers with different prior streams still differ after \
         reset_runtime_state: peak residual {residual:.1} dBFS (gate {:.1}). \
         The previous stream is leaking past the reset.",
        dsp_fixtures::golden::RESIDUAL_GATE_DBFS
    );
}
