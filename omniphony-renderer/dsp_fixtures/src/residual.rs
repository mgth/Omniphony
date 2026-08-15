//! Null-comparison arithmetic: differences between two renders expressed in
//! dBFS, plus the locator used to report *where* a mismatch is worst.
//!
//! dBFS here is `20·log10(|x|)` with full scale at `1.0`, matching the
//! renderer's f32 sample convention.

/// Linear amplitude to dBFS. Returns [`f32::NEG_INFINITY`] for zero or
/// negative input rather than NaN, so a perfect match reports as `-inf`.
pub fn lin_to_dbfs(x: f32) -> f32 {
    if x <= 0.0 {
        f32::NEG_INFINITY
    } else {
        20.0 * x.log10()
    }
}

/// Peak absolute level of a signal, in dBFS.
pub fn peak_dbfs(x: &[f32]) -> f32 {
    lin_to_dbfs(x.iter().map(|v| v.abs()).fold(0.0f32, f32::max))
}

/// Largest absolute sample-by-sample difference, in dBFS. This is the gate.
///
/// Panics if the slices differ in length — callers must check shape first so
/// the failure names the real problem instead of silently truncating.
pub fn peak_residual_dbfs(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "residual needs equal-length signals");
    let peak = a
        .iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max);
    lin_to_dbfs(peak)
}

/// RMS of the difference, in dBFS. Reported alongside the peak for context;
/// not itself a gate. Accumulates in `f64` so long renders do not lose
/// precision in the sum.
pub fn rms_residual_dbfs(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "residual needs equal-length signals");
    if a.is_empty() {
        return f32::NEG_INFINITY;
    }
    let sum_sq: f64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| {
            let d = (*x - *y) as f64;
            d * d
        })
        .sum();
    lin_to_dbfs((sum_sq / a.len() as f64).sqrt() as f32)
}

/// Locate the largest deviation in an interleaved pair: `(frame, channel, delta)`.
///
/// Used only for failure messages — a bare "golden mismatch" is not actionable.
pub fn worst_deviation(a: &[f32], b: &[f32], channels: usize) -> (usize, usize, f32) {
    assert_eq!(a.len(), b.len(), "residual needs equal-length signals");
    assert!(channels > 0, "channels must be non-zero");
    let mut best = (0usize, 0usize, 0.0f32);
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        let d = (x - y).abs();
        if d > best.2 {
            best = (i / channels, i % channels, d);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_signals_have_negative_infinite_residual() {
        let a = vec![0.1, -0.5, 0.9, 0.0];
        let r = peak_residual_dbfs(&a, &a);
        assert_eq!(
            r,
            f32::NEG_INFINITY,
            "identical inputs must be -inf dBFS, not NaN or a finite value"
        );
    }

    #[test]
    fn constant_offset_gives_the_analytic_value() {
        // A difference of exactly 1e-6 is exactly -120 dBFS.
        let a = vec![0.0f32; 64];
        let b = vec![1e-6f32; 64];
        let peak = peak_residual_dbfs(&a, &b);
        assert!(
            (peak - -120.0).abs() < 0.01,
            "expected -120 dBFS for a 1e-6 offset, got {peak}"
        );
        // Every sample differs by the same amount, so RMS equals peak.
        let rms = rms_residual_dbfs(&a, &b);
        assert!(
            (rms - -120.0).abs() < 0.01,
            "expected -120 dBFS RMS for a constant offset, got {rms}"
        );
    }

    #[test]
    fn peak_dbfs_reads_full_scale_as_zero() {
        assert!((peak_dbfs(&[0.0, -1.0, 0.5]) - 0.0).abs() < 1e-6);
        assert_eq!(peak_dbfs(&[0.0, 0.0]), f32::NEG_INFINITY);
    }

    #[test]
    fn worst_deviation_locates_frame_and_channel() {
        // 3 channels, 4 frames. Plant the largest error at frame 2, channel 1.
        let a = vec![0.0f32; 12];
        let mut b = vec![0.0f32; 12];
        b[1 * 3 + 0] = 0.01; // frame 1, channel 0 — smaller
        b[2 * 3 + 1] = 0.50; // frame 2, channel 1 — largest
        let (frame, channel, delta) = worst_deviation(&a, &b, 3);
        assert_eq!((frame, channel), (2, 1));
        assert!((delta - 0.50).abs() < 1e-6, "got {delta}");
    }
}
