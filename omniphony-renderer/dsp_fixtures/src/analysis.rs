//! Measurement helpers for the acceptance tests: frequency response of an
//! impulse response, and interaural lag by cross-correlation.
//!
//! Both are verified below against analytically known answers. A validation
//! harness whose own measurements are wrong passes everything.

use realfft::RealFftPlanner;

use crate::residual::lin_to_dbfs;

/// Magnitude response of `ir`, as `(frequency_hz, magnitude_db)` for every
/// real-FFT bin (`ir.len()/2 + 1` of them).
///
/// No window is applied: callers pass an impulse response long enough that its
/// tail has decayed. Windowing an already-decayed IR would only smear the
/// response, and truncating an undecayed one shows up as passband ripple —
/// which is why the LR4 test uses 32768 samples.
pub fn magnitude_response_db(ir: &[f32], sample_rate: u32) -> Vec<(f32, f32)> {
    assert!(ir.len() >= 2, "need at least 2 samples for an FFT");
    let n = ir.len();
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(n);
    let mut input = ir.to_vec();
    let mut spectrum = fft.make_output_vec();
    fft.process(&mut input, &mut spectrum)
        .expect("realfft forward transform");
    spectrum
        .iter()
        .enumerate()
        .map(|(k, c)| {
            let freq = k as f32 * sample_rate as f32 / n as f32;
            (freq, lin_to_dbfs(c.norm()))
        })
        .collect()
}

/// Minimum lag separation, in samples, before a second correlation peak counts
/// as a competing interpretation rather than part of the same main lobe.
const AMBIGUITY_SEPARATION: i64 = 4;

/// A competing peak scoring at least this fraction of the best one makes the
/// estimate ambiguous.
const AMBIGUITY_RATIO: f64 = 0.95;

/// Lag, in samples, by which `right` is delayed relative to `left` — or an
/// error describing why the signals cannot yield an unambiguous answer.
///
/// Positive means `right[n] ≈ left[n - lag]`. For a binaural render this means
/// a source on the **right** returns a *negative* value: the contralateral
/// (left) ear is the delayed one.
///
/// The integer cross-correlation peak is refined by parabolic interpolation, so
/// sub-sample delays are recovered — necessary because ITD at 48 kHz is only
/// ~31 samples at full deflection.
///
/// # Why this is checked
///
/// Cross-correlation resolves lag only modulo the period of the excitation. A
/// periodic input produces several equally good peaks, and whichever wins is
/// decided by noise — which silently yields a confidently wrong, often
/// sign-flipped answer. That is not hypothetical: it is exactly what a
/// 40-sample-periodic excitation did to the binaural ITD measurement, and it
/// looked like an engine defect for as long as the estimator kept quiet about
/// it. So a competing peak at least [`AMBIGUITY_SEPARATION`] away scoring
/// [`AMBIGUITY_RATIO`] of the best is reported as an error rather than resolved.
pub fn estimate_lag_checked(left: &[f32], right: &[f32], max_lag: usize) -> Result<f32, String> {
    if left.len() != right.len() {
        return Err(format!(
            "channels must be equal length, got {} and {}",
            left.len(),
            right.len()
        ));
    }
    if left.len() <= 2 * max_lag + 2 {
        return Err(format!(
            "signal ({}) too short for a ±{max_lag} lag search",
            left.len()
        ));
    }

    let n = left.len() as i64;
    let corr = |lag: i64| -> f64 {
        let mut acc = 0.0f64;
        let start = lag.max(0);
        let end = (n + lag).min(n);
        for i in start..end {
            acc += left[(i - lag) as usize] as f64 * right[i as usize] as f64;
        }
        acc
    };

    let ml = max_lag as i64;
    let scores: Vec<(i64, f64)> = (-ml..=ml).map(|lag| (lag, corr(lag))).collect();
    let &(best_lag, best) = scores
        .iter()
        .max_by(|a, b| a.1.partial_cmp(&b.1).expect("correlation is finite"))
        .expect("search range is non-empty");

    if best <= 0.0 {
        return Err(format!(
            "no positive correlation peak (best {best:.4} at lag {best_lag}); \
             the channels are uncorrelated or inverted"
        ));
    }

    // A competing peak far enough away to be a different interpretation.
    if let Some(&(rival_lag, rival)) = scores
        .iter()
        .filter(|(lag, _)| (lag - best_lag).abs() >= AMBIGUITY_SEPARATION)
        .max_by(|a, b| a.1.partial_cmp(&b.1).expect("correlation is finite"))
        && rival >= AMBIGUITY_RATIO * best
    {
        return Err(format!(
            "ambiguous lag: peak {best:.4} at lag {best_lag} but a competing \
             peak scores {rival:.4} at lag {rival_lag} ({:.2}% of the best). \
             The excitation is probably periodic — cross-correlation resolves \
             lag only modulo that period, so the winner is decided by noise.",
            100.0 * rival / best
        ));
    }

    // Parabolic refinement around the peak. Skipped at the search edges, where
    // one neighbour is unavailable.
    if best_lag > -ml && best_lag < ml {
        let cm = corr(best_lag - 1);
        let cp = corr(best_lag + 1);
        let denom = cm - 2.0 * best + cp;
        if denom.abs() > f64::EPSILON {
            return Ok(best_lag as f32 + (0.5 * (cm - cp) / denom) as f32);
        }
    }
    Ok(best_lag as f32)
}

/// [`estimate_lag_checked`], panicking on an ambiguous or degenerate estimate.
///
/// Test code wants a bare `f32`; a panic converts a silently wrong measurement
/// into a loud, explained failure.
pub fn estimate_lag_samples(left: &[f32], right: &[f32], max_lag: usize) -> f32 {
    estimate_lag_checked(left, right, max_lag).expect("lag estimate")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bandlimited unit pulse centred at `delay` samples (possibly fractional).
    /// A windowed sinc is the right test signal: it has a known sub-sample
    /// position, unlike a bare impulse.
    fn sinc_pulse(len: usize, delay: f64) -> Vec<f32> {
        (0..len)
            .map(|n| {
                let x = n as f64 - delay;
                let s = if x.abs() < 1e-12 {
                    1.0
                } else {
                    (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
                };
                // Hann window over the whole buffer keeps the edges tame.
                let w =
                    0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / (len - 1) as f64).cos();
                (s * w) as f32
            })
            .collect()
    }

    #[test]
    fn unit_impulse_is_flat_at_zero_db() {
        let mut ir = vec![0.0f32; 1024];
        ir[0] = 1.0;
        let resp = magnitude_response_db(&ir, 48_000);
        assert_eq!(resp.len(), 1024 / 2 + 1, "realfft returns n/2+1 bins");
        for (freq, db) in &resp {
            assert!(
                db.abs() < 1e-3,
                "unit impulse must be 0 dB everywhere; got {db} dB at {freq} Hz"
            );
        }
    }

    #[test]
    fn magnitude_response_reports_a_known_gain() {
        let mut ir = vec![0.0f32; 512];
        ir[0] = 0.5; // -6.0206 dB, flat
        let resp = magnitude_response_db(&ir, 48_000);
        for (_, db) in &resp {
            assert!((db - -6.0206).abs() < 1e-2, "expected -6.02 dB, got {db}");
        }
    }

    #[test]
    fn bin_frequencies_span_dc_to_nyquist() {
        let ir = vec![0.0f32; 480];
        let resp = magnitude_response_db(&ir, 48_000);
        assert!((resp[0].0 - 0.0).abs() < 1e-6, "first bin is DC");
        let last = resp.last().expect("non-empty").0;
        assert!(
            (last - 24_000.0).abs() < 1.0,
            "last bin is Nyquist, got {last}"
        );
    }

    #[test]
    fn recovers_an_integer_lag() {
        let left = sinc_pulse(512, 100.0);
        let right = sinc_pulse(512, 107.0);
        // right is delayed by 7 samples relative to left.
        let lag = estimate_lag_samples(&left, &right, 64);
        assert!((lag - 7.0).abs() < 0.02, "expected +7.0, got {lag}");
    }

    #[test]
    fn recovers_a_fractional_lag() {
        let left = sinc_pulse(512, 100.0);
        let right = sinc_pulse(512, 107.5);
        let lag = estimate_lag_samples(&left, &right, 64);
        assert!((lag - 7.5).abs() < 0.1, "expected +7.5, got {lag}");
    }

    #[test]
    fn lag_sign_is_negative_when_left_is_delayed() {
        let left = sinc_pulse(512, 107.0);
        let right = sinc_pulse(512, 100.0);
        let lag = estimate_lag_samples(&left, &right, 64);
        assert!((lag - -7.0).abs() < 0.02, "expected -7.0, got {lag}");
    }

    /// A signal that repeats every `period` samples, delayed by `delay`.
    /// Cross-correlation cannot resolve its lag beyond one period.
    fn periodic_signal(len: usize, period: usize, delay: usize) -> Vec<f32> {
        (0..len)
            .map(|n| {
                let phase = (n + period - delay % period) % period;
                // Deterministic pseudo-noise within one period, then repeated.
                let mut x = (phase as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
                x ^= x >> 30;
                ((x >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
            })
            .collect()
    }

    #[test]
    fn periodic_excitation_is_reported_as_ambiguous() {
        // Period 40 is BLOCK_SAMPLES: exactly the case that made the binaural
        // ITD measurement report a sign-flipped lag. The true delay is 7, but
        // lags 7, 47 and -33 all correlate equally well.
        let left = periodic_signal(2048, 40, 0);
        let right = periodic_signal(2048, 40, 7);
        let err = estimate_lag_checked(&left, &right, 64)
            .expect_err("a period-40 signal must be rejected, not silently resolved");
        assert!(
            err.contains("ambiguous"),
            "error should name the ambiguity, got: {err}"
        );
    }

    #[test]
    fn aperiodic_excitation_is_accepted() {
        let left = sinc_pulse(2048, 100.0);
        let right = sinc_pulse(2048, 107.0);
        let lag = estimate_lag_checked(&left, &right, 64).expect("unambiguous");
        assert!((lag - 7.0).abs() < 0.02, "expected +7.0, got {lag}");
    }

    #[test]
    fn identical_channels_have_zero_lag() {
        let s = sinc_pulse(512, 100.0);
        let lag = estimate_lag_samples(&s, &s, 64);
        assert!(lag.abs() < 0.02, "expected 0.0, got {lag}");
    }
}
