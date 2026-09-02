//! End-to-end interaural time difference.
//!
//! This deliberately does **not** compare `itd::ear_delays_seconds` against
//! Woodworth's formula — `itd.rs` *implements* Woodworth, so such a test would
//! be circular and would prove nothing. Instead it measures the lag between the
//! left and right channels of an actual binaural render, which exercises the
//! delay lines, the convolver, the interpolation and the head-pose rotation as
//! a chain.
//!
//! Three properties, because per-ear HRIR group delay biases any raw comparison
//! against the model:
//!
//! 1. **Antisymmetry** — `lag(+az) = −lag(−az)`, and `lag(0°) ≈ 0`. Structural,
//!    so it is immune to that bias.
//! 2. **Monotonicity** — |lag| grows from 0° toward 90°.
//! 3. **Magnitude** — within ±3 samples of the model, the tolerance absorbing
//!    the group delay.

use dsp_fixtures::analysis::estimate_lag_samples;
use dsp_fixtures::scene::{
    HrirSource, render_single_object_binaural, render_single_object_binaural_at,
};

use super::itd::{DEFAULT_HEAD_RADIUS_M, ear_delays_seconds};

/// 128 blocks of 40 samples = 5120 samples, ample for a ±64-sample search.
const BLOCKS: usize = 128;
const MAX_LAG: usize = 64;
const SAMPLE_RATE: f32 = 48_000.0;

/// Azimuths measured in the PR gate. 0 and ±90 bracket the range; the
/// intermediate angles catch a sign error that the extremes would not.
const AZIMUTHS: [f32; 7] = [0.0, 30.0, -30.0, 60.0, -60.0, 90.0, -90.0];

/// Measured lag in samples: positive means the right channel is delayed, so a
/// source on the right (positive azimuth) yields a negative value.
fn measured_lag(azimuth_deg: f32) -> f32 {
    // The synthetic provider is symmetric and time-aligned by construction, so
    // these tests measure the *engine's* ITD rather than the bundled KEMAR
    // set's own left/right asymmetry. See
    // `hrir_providers_return_time_aligned_pairs` for the test that holds a
    // provider to the time-alignment contract.
    let (left, right) = render_single_object_binaural(azimuth_deg, BLOCKS, HrirSource::Synthetic);
    estimate_lag_samples(&left, &right, MAX_LAG)
}

/// Model lag in samples, matching the sign convention of [`measured_lag`].
///
/// `ear_delays_seconds` returns `(left_delay, right_delay)`, both ≥ 0, with the
/// far ear carrying the delay. `right_delay − left_delay` is therefore positive
/// when the right ear is the far one, which is the same convention as the
/// cross-correlation estimate.
fn model_lag(azimuth_deg: f32) -> f32 {
    let (l, r) = ear_delays_seconds((azimuth_deg).to_radians(), 0.0, DEFAULT_HEAD_RADIUS_M);
    (r - l) * SAMPLE_RATE
}

/// Absorbs per-ear HRIR group delay, which is not part of the Woodworth model.
const MAGNITUDE_TOLERANCE_SAMPLES: f32 = 3.0;

/// Antisymmetry is structural, so the bound is tight.
const ANTISYMMETRY_TOLERANCE_SAMPLES: f32 = 1.0;

#[test]
fn itd_magnitude_tracks_the_model() {
    for az in AZIMUTHS {
        let measured = measured_lag(az);
        let model = model_lag(az);
        let delta = measured - model;
        println!(
            "[measure] itd az={az:+6.1}°: measured {measured:+7.3}, \
             model {model:+7.3}, delta {delta:+.3} samples"
        );
        assert!(
            delta.abs() <= MAGNITUDE_TOLERANCE_SAMPLES,
            "ITD at az={az:+.1}° is {measured:+.3} samples but the model says \
             {model:+.3} (delta {delta:+.3}, tolerance \
             ±{MAGNITUDE_TOLERANCE_SAMPLES})"
        );
    }
}

/// Previously deferred as "`measured_lag` is not deterministic under test
/// parallelism", with this evidence from a single full-suite run:
///
/// ```text
/// itd_magnitude_tracks_the_model (passed):
///   az=  0.0 -> +0.000    az=+30.0 -> -12.836    az=-30.0 -> +12.836
/// itd_is_antisymmetric_about_the_median_plane (failed, same run):
///   az=  0.0 -> +0.025    az=+30.0 -> -13.460    az=-30.0 -> +12.435
/// ```
///
/// The cause was not thread-local FP state and not shared global state: it was
/// the fixture racing its own HRIR request. `render_single_object_binaural`
/// asks for [`HrirSource::Synthetic`](super::HrirSource::Synthetic), but that
/// switch is asynchronous by design — the grid is built off the audio thread
/// and swapped in later. A loaded machine could delay the swap past the prime
/// blocks, so the measurement convolved the *default* SAF KEMAR grid instead.
/// KEMAR is not time-aligned (see `hrir_providers_return_time_aligned_pairs`),
/// and its intrinsic interaural lag is exactly the error seen above: −1.103
/// samples at +30° gives −13.46 against the synthetic −12.836, and its
/// left/right asymmetry moves 0° off zero.
///
/// The fixture now drives frames until `binaural_rebuild_pending()` clears
/// before priming, so the requested grid is provably the one measured.
#[test]
fn itd_is_antisymmetric_about_the_median_plane() {
    let centre = measured_lag(0.0);
    println!("[measure] itd az=0°: {centre:+.3} samples");
    assert!(
        centre.abs() <= ANTISYMMETRY_TOLERANCE_SAMPLES,
        "a source dead ahead must have no ITD, measured {centre:+.3} samples"
    );
    for az in [30.0f32, 60.0, 90.0] {
        let pos = measured_lag(az);
        let neg = measured_lag(-az);
        println!(
            "[measure] itd antisymmetry ±{az:.0}°: {pos:+.3} vs {neg:+.3}, \
             sum {:+.3}",
            pos + neg
        );
        assert!(
            (pos + neg).abs() <= ANTISYMMETRY_TOLERANCE_SAMPLES,
            "ITD must be antisymmetric: az=+{az:.0}° gives {pos:+.3} and \
             az=-{az:.0}° gives {neg:+.3}, sum {:+.3} exceeds \
             ±{ANTISYMMETRY_TOLERANCE_SAMPLES}",
            pos + neg
        );
    }
}

#[test]
fn itd_magnitude_grows_toward_the_interaural_axis() {
    let mags: Vec<f32> = [0.0f32, 30.0, 60.0, 90.0]
        .iter()
        .map(|az| measured_lag(*az).abs())
        .collect();
    println!("[measure] itd monotonicity |lag| at 0/30/60/90°: {mags:?}");
    for w in mags.windows(2) {
        assert!(
            w[1] > w[0],
            "|ITD| must increase toward the interaural axis, got {mags:?}"
        );
    }
}

/// Height directions. The ITD is a function of the lateral angle
/// `asin(cos el · sin az)`, and the horizontal plane cannot distinguish that
/// form from one that scales the azimuth value by `cos el` — the two agree at
/// el = 0. The side height directions are where they part (+16 % at
/// (90°, 30°), +22 % at (90°, 45°) for the scaled form), so the chain is
/// measured there against the closed form, with the same tolerance as the
/// horizontal gate.
#[test]
fn itd_at_elevation_tracks_the_lateral_angle_model() {
    for (az, el) in [(90.0f32, 30.0f32), (90.0, 45.0), (60.0, 30.0), (30.0, 60.0)] {
        let (left, right) = render_single_object_binaural_at(az, el, BLOCKS, HrirSource::Synthetic);
        let measured = estimate_lag_samples(&left, &right, MAX_LAG);
        let (l, r) = ear_delays_seconds(az.to_radians(), el.to_radians(), DEFAULT_HEAD_RADIUS_M);
        let model = (r - l) * SAMPLE_RATE;
        let delta = measured - model;
        println!(
            "[measure] itd az={az:+6.1}° el={el:+5.1}°: measured {measured:+7.3}, \
             model {model:+7.3}, delta {delta:+.3} samples"
        );
        assert!(
            delta.abs() <= MAGNITUDE_TOLERANCE_SAMPLES,
            "ITD at az={az:+.1}° el={el:+.1}° is {measured:+.3} samples but the \
             model says {model:+.3} (delta {delta:+.3}, tolerance \
             ±{MAGNITUDE_TOLERANCE_SAMPLES})"
        );
    }
}

/// The wide matrix: a full azimuth grid at several elevations.
/// Compiled only with `--features wide-matrix`.
#[cfg(feature = "wide-matrix")]
#[test]
fn itd_magnitude_tracks_the_model_wide() {
    for az_i in -6..=6 {
        let az = az_i as f32 * 30.0;
        let measured = measured_lag(az);
        let model = model_lag(az);
        let delta = measured - model;
        println!(
            "[measure] itd_wide az={az:+6.1}°: measured {measured:+7.3}, \
             model {model:+7.3}, delta {delta:+.3}"
        );
        assert!(
            delta.abs() <= MAGNITUDE_TOLERANCE_SAMPLES,
            "ITD at az={az:+.1}°: delta {delta:+.3} samples exceeds \
             ±{MAGNITUDE_TOLERANCE_SAMPLES}"
        );
    }
}

/// A time-aligned response has its onset at the origin: the first tap to reach
/// half of the peak must be one of the first three.
const TIME_ALIGNMENT_ONSET_TAPS: usize = 2;

/// [`HrirProvider`](super::hrir::HrirProvider) documents that implementors
/// "must return time-aligned FIRs (no bulk interaural delay) for safe
/// interpolation". The engine relies on that in two places: it adds its own
/// Woodworth ITD as a separate per-ear delay, and [`HrirSet`] blends the three
/// nearest measurements. Blending FIRs that are not time-aligned combs their
/// shared content instead of interpolating it.
///
/// The bundled set is stored minimum-phase, which puts the onset of every
/// response at the origin by construction. That is the property asserted
/// here, per ear and per direction, on the grid the engine convolves with.
///
/// It used to be measured as the cross-correlation lag between the two ears
/// against a ±1 sample bound, and deferred: the onset-aligned KEMAR set
/// carried −6.998 samples at +90° and was unresolvable at −90°. That lag
/// estimate is not the right instrument for the contract, though. The
/// shadowed ear's response is a heavily low-passed version of the exposed
/// one, and the correlation peak between two spectrally dissimilar responses
/// sits at their group-delay difference — with minimum-phase pairs it still
/// reads −4.3 samples at +90° while both onsets are at tap 0. That
/// difference is part of the head shadow, not a bulk delay.
#[test]
fn hrir_providers_return_time_aligned_pairs() {
    use super::hrir::{HRIR_LEN, HrirPair, HrirSet};
    use super::measured::MeasuredHrirData;

    let set = HrirSet::new(&MeasuredHrirData::saf_kemar(), 48_000);
    let mut pair = HrirPair {
        left: [0.0; HRIR_LEN],
        right: [0.0; HRIR_LEN],
    };
    let onset = |h: &[f32; HRIR_LEN]| -> usize {
        let peak = h.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
        h.iter()
            .position(|&x| x.abs() >= 0.5 * peak)
            .unwrap_or(usize::MAX)
    };
    let mut worst = (0.0f32, 0.0f32, 0usize);
    for el_i in -3..=6 {
        for az_i in 0..36 {
            let (az, el) = (az_i as f32 * 10.0, el_i as f32 * 15.0);
            set.at(az, el, &mut pair);
            for ear in [&pair.left, &pair.right] {
                let n = onset(ear);
                if n > worst.2 {
                    worst = (az, el, n);
                }
            }
        }
    }
    println!(
        "[measure] hrir_time_alignment: latest onset {} taps at az={:+.1} el={:+.1}",
        worst.2, worst.0, worst.1
    );
    assert!(
        worst.2 <= TIME_ALIGNMENT_ONSET_TAPS,
        "HRIR at az={:.1}° el={:.1}° reaches half its peak only at tap {}, past the \
         {TIME_ALIGNMENT_ONSET_TAPS}-tap time-alignment contract. The engine adds \
         Woodworth ITD on top of the pair, and HrirSet blends neighbouring \
         measurements — both assume the responses carry no bulk delay.",
        worst.0,
        worst.1,
        worst.2
    );
}
