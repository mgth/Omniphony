//! LR4 crossover reconstruction flatness.
//!
//! `filter.rs` documents that each split sums to a 2nd-order allpass, and that
//! for N bands every already-emitted band is passed through the current
//! splitter's compensating allpass, so the total is a cascade of N−1 allpasses.
//! An allpass has magnitude exactly 1, therefore:
//!
//!   **the sum of all bands must be magnitude-flat at 0 dB.**
//!
//! Any deviation is coefficient or float error, not a design property. That is
//! what makes this a theory-derived threshold rather than a ratchet.
//!
//! **Phase is deliberately not asserted.** Allpass summing rotates phase by
//! design; asserting flat phase would be asserting the filter is broken.

use dsp_fixtures::analysis::magnitude_response_db;

use super::filter::{BiquadState, LR4CrossoverBank};

/// Impulse response length. LR4 ringing at the lowest cutoff takes tens of
/// milliseconds; truncating it leaks into the spectrum and reads as passband
/// ripple, so this must stay long.
const IR_LEN: usize = 32_768;

/// The cutoffs the shipped band-limited layout produces (see
/// `dsp_fixtures::scene::crossover_layout`): three band edges, four bands.
const DEFAULT_CUTOFFS: [f32; 3] = [80.0, 200.0, 500.0];

const SAMPLE_RATE: u32 = 48_000;

/// Sum of all band outputs for a unit impulse — the reconstruction IR.
fn reconstruction_ir(cutoffs: &[f32], sample_rate: u32) -> Vec<f32> {
    let bank = LR4CrossoverBank::new(cutoffs, sample_rate);
    let mut states = vec![BiquadState::default(); bank.state_count()];
    (0..IR_LEN)
        .map(|i| {
            let x = if i == 0 { 1.0 } else { 0.0 };
            let bands = bank.process_sample(x, &mut states);
            (0..bands.len()).map(|b| bands.get(b)).sum()
        })
        .collect()
}

/// Worst deviation from 0 dB over the asserted band, as `(freq_hz, dev_db)`.
///
/// The band is `[4·fc_min, min(20 kHz, 0.45·fs)]`: bounded below because the
/// truncated IR is unreliable near DC, and above so the 44.1 kHz case does not
/// assert flatness into the anti-alias region near Nyquist.
fn worst_flatness_deviation(cutoffs: &[f32], sample_rate: u32) -> (f32, f32) {
    let ir = reconstruction_ir(cutoffs, sample_rate);
    let resp = magnitude_response_db(&ir, sample_rate);
    let fc_min = cutoffs.iter().copied().fold(f32::INFINITY, f32::min);
    let lo = 4.0 * fc_min;
    let hi = 20_000.0f32.min(0.45 * sample_rate as f32);
    let mut worst = (0.0f32, 0.0f32);
    for (freq, db) in resp {
        if freq < lo || freq > hi {
            continue;
        }
        if db.abs() > worst.1.abs() {
            worst = (freq, db);
        }
    }
    worst
}

/// Theory-derived: the band sum is a cascade of allpasses, so magnitude is
/// exactly 1. This bound is float and coefficient error only.
const FLATNESS_TOLERANCE_DB: f32 = 0.25;

#[test]
fn lr4_reconstruction_is_magnitude_flat() {
    let (freq, dev) = worst_flatness_deviation(&DEFAULT_CUTOFFS, SAMPLE_RATE);
    println!(
        "[measure] lr4_flatness cutoffs={DEFAULT_CUTOFFS:?} fs={SAMPLE_RATE}: \
         worst deviation {dev:+.4} dB at {freq:.1} Hz"
    );
    assert!(
        dev.abs() <= FLATNESS_TOLERANCE_DB,
        "LR4 band sum deviates {dev:+.4} dB from flat at {freq:.1} Hz, \
         tolerance ±{FLATNESS_TOLERANCE_DB} dB. The sum of LR4 bands is an \
         allpass cascade, so its magnitude must be 1 — a deviation means the \
         coefficients or the multiway compensation are wrong."
    );
}

/// The wide matrix: every band count against every supported sample rate.
/// Compiled only with `--features wide-matrix`.
#[cfg(feature = "wide-matrix")]
#[test]
fn lr4_reconstruction_is_magnitude_flat_wide() {
    const CUTOFF_SETS: [&[f32]; 3] = [&[500.0], &[200.0, 2000.0], &[80.0, 200.0, 500.0]];
    for fs in [44_100u32, 48_000, 96_000] {
        for cutoffs in CUTOFF_SETS {
            let (freq, dev) = worst_flatness_deviation(cutoffs, fs);
            println!(
                "[measure] lr4_flatness_wide cutoffs={cutoffs:?} fs={fs}: \
                 {dev:+.4} dB at {freq:.1} Hz"
            );
            assert!(
                dev.abs() <= FLATNESS_TOLERANCE_DB,
                "LR4 sum deviates {dev:+.4} dB at {freq:.1} Hz \
                 (cutoffs {cutoffs:?}, fs {fs}), tolerance \
                 ±{FLATNESS_TOLERANCE_DB} dB"
            );
        }
    }
}
