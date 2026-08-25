//! Cascaded Linkwitz-Riley (LR4) crossover filter bank.
//!
//! Each splitter stage at cutoff `fc` produces, from its input `x`:
//!   LP = BW2_LP(BW2_LP(x))   — LR4 low-pass:  −6 dB at fc, 24 dB/oct above
//!   HP = BW2_HP(BW2_HP(x))   — LR4 high-pass: −6 dB at fc, 24 dB/oct below
//!
//! The two outputs are in phase at every frequency, and their sum is exactly
//! the 2nd-order allpass AP(fc) sharing the same poles (s⁴ + ωc⁴ factors into
//! D(s)·D(−s); the identity survives the bilinear transform). A split
//! therefore sums back flat in magnitude with a benign phase rotation — the
//! standard speaker-crossover behaviour. Unlike the previous subtractive
//! design (HP = input − LP), the high side genuinely rejects lows at
//! 24 dB/oct instead of ~6 dB/oct, at the price of the sample-exact sum.
//!
//! For N bands, N−1 splitters are chained on the HP branch:
//!   Band 0   = LP of splitter 0
//!   Band k   = LP of splitter k  (applied to HP of splitter k−1)
//!   Band N−1 = HP of splitter N−2
//!
//! Multiway phase compensation: when splitter k runs, every band already
//! emitted (0..k) is passed through this splitter's AP(fc_k), so the total
//! sum stays magnitude-flat around every cutoff. The sum of all bands then
//! equals the input through the cascade of the N−1 allpasses.
//!
//! State layout per object (`state_count()` entries):
//!   4 per splitter — [lp1, lp2, hp1, hp2] — for splitters 0..N−1, followed
//!   by the compensation allpass states: splitter k owns k entries (one per
//!   earlier band). Total = 4·(N−1) + (N−1)(N−2)/2.

/// State for a single Direct-Form-II Transposed biquad section.
#[derive(Clone, Default)]
pub struct BiquadState {
    z1: f32,
    z2: f32,
}

/// Biquad coefficients: `[b0, b1, b2, a1, a2]` in Direct-Form-II Transposed.
#[derive(Clone, Copy)]
pub(crate) struct BiquadCoeffs(pub(crate) [f32; 5]);

/// Process one sample through a biquad (Direct Form II Transposed).
#[inline(always)]
pub(crate) fn biquad(input: f32, c: BiquadCoeffs, s: &mut BiquadState) -> f32 {
    let [b0, b1, b2, a1, a2] = c.0;
    let out = b0 * input + s.z1;
    s.z1 = b1 * input - a1 * out + s.z2;
    s.z2 = b2 * input - a2 * out;
    out
}

/// Shared bilinear-transform pieces of a 2nd-order Butterworth section at
/// `fc` Hz: `(k², norm, a1, a2)`. LP, HP and the compensation allpass all use
/// the same denominator.
fn butterworth2_parts(fc: f32, sample_rate: u32) -> (f32, f32, f32, f32) {
    let k = (std::f32::consts::PI * fc / sample_rate as f32).tan();
    // Butterworth damping: Q = 1/√2. Two cascaded sections then form a true
    // Linkwitz-Riley 4th-order filter (flat passband, −6 dB at fc). A Q of
    // √2 here would instead peak +3 dB per section (+6 dB at fc combined).
    let q = std::f32::consts::FRAC_1_SQRT_2;
    let norm = 1.0 + k / q + k * k;
    let a1 = 2.0 * (k * k - 1.0) / norm;
    let a2 = (1.0 - k / q + k * k) / norm;
    (k * k, norm, a1, a2)
}

/// 2nd-order Butterworth low-pass biquad at `fc` Hz.
pub(crate) fn butterworth2_lp(fc: f32, sample_rate: u32) -> BiquadCoeffs {
    let (k2, norm, a1, a2) = butterworth2_parts(fc, sample_rate);
    let b0 = k2 / norm;
    BiquadCoeffs([b0, 2.0 * b0, b0, a1, a2])
}

/// 2nd-order Butterworth high-pass biquad at `fc` Hz.
pub(crate) fn butterworth2_hp(fc: f32, sample_rate: u32) -> BiquadCoeffs {
    let (_, norm, a1, a2) = butterworth2_parts(fc, sample_rate);
    let b0 = 1.0 / norm;
    BiquadCoeffs([b0, -2.0 * b0, b0, a1, a2])
}

/// 2nd-order allpass at `fc` Hz with the same poles as the Butterworth
/// sections — exactly LR4_LP(fc) + LR4_HP(fc). Used to phase-align earlier
/// bands when a later splitter runs.
fn allpass2(fc: f32, sample_rate: u32) -> BiquadCoeffs {
    let (_, _, a1, a2) = butterworth2_parts(fc, sample_rate);
    BiquadCoeffs([a2, a1, 1.0, a1, a2])
}

/// Pre-computed coefficients for one LR4 splitter at a given cutoff.
struct Splitter {
    /// Cascaded low-pass stages (LR4 LP = lp1 → lp2).
    lp1: BiquadCoeffs,
    lp2: BiquadCoeffs,
    /// Cascaded high-pass stages (LR4 HP = hp1 → hp2).
    hp1: BiquadCoeffs,
    hp2: BiquadCoeffs,
    /// Compensation allpass applied to every band emitted before this splitter.
    ap: BiquadCoeffs,
}

/// A bank of crossover filters for B = `cutoffs.len() + 1` bands.
///
/// Build once per renderer construction from the frequency band cutoffs.
/// Call [`process_sample`] with per-object mutable state each audio sample.
pub struct LR4CrossoverBank {
    /// One splitter per cutoff frequency.
    splitters: Vec<Splitter>,
    /// Number of output bands (= splitters.len() + 1).
    pub num_bands: usize,
}

impl LR4CrossoverBank {
    /// Create a new bank for the given cutoffs (in Hz) and sample rate.
    pub fn new(cutoffs: &[f32], sample_rate: u32) -> Self {
        let nyquist = sample_rate as f32 / 2.0;
        let splitters = cutoffs
            .iter()
            .map(|&fc| {
                let fc = fc.clamp(1.0, nyquist - 1.0);
                Splitter {
                    lp1: butterworth2_lp(fc, sample_rate),
                    lp2: butterworth2_lp(fc, sample_rate),
                    hp1: butterworth2_hp(fc, sample_rate),
                    hp2: butterworth2_hp(fc, sample_rate),
                    ap: allpass2(fc, sample_rate),
                }
            })
            .collect::<Vec<_>>();
        let num_bands = splitters.len() + 1;
        Self {
            splitters,
            num_bands,
        }
    }

    /// Number of `BiquadState` entries required per object.
    ///
    /// Allocate `vec![BiquadState::default(); state_count()]` for each object.
    /// See the module doc for the layout: 4 filter states per splitter, then
    /// the triangular block of compensation-allpass states.
    pub fn state_count(&self) -> usize {
        let s = self.splitters.len();
        s * 4 + s * s.saturating_sub(1) / 2
    }

    /// Split `input` into `num_bands` band samples using the per-object `states`.
    ///
    /// `states` must have length `state_count()`.  The returned `SmallBands` has
    /// length `num_bands`.  Caller owns the allocation (stack-backed inline vec).
    pub fn process_sample(&self, input: f32, states: &mut [BiquadState]) -> SmallBands {
        let mut signal = input;
        let mut bands = SmallBands::new(self.num_bands);
        let ap_base = self.splitters.len() * 4;

        for (si, splitter) in self.splitters.iter().enumerate() {
            let base = si * 4;
            let lp = biquad(
                biquad(signal, splitter.lp1, &mut states[base]),
                splitter.lp2,
                &mut states[base + 1],
            );
            let hp = biquad(
                biquad(signal, splitter.hp1, &mut states[base + 2]),
                splitter.hp2,
                &mut states[base + 3],
            );

            // Phase-align the bands already emitted: this splitter's LP + HP
            // sum to its AP, so earlier bands must pass through the same AP
            // for the total to stay magnitude-flat around this cutoff.
            let ap_row = ap_base + si * si.saturating_sub(1) / 2;
            for b in 0..si {
                let aligned = biquad(bands.get(b), splitter.ap, &mut states[ap_row + b]);
                bands.set(b, aligned);
            }

            bands.set(si, lp);
            signal = hp;
        }
        bands.set(self.splitters.len(), signal);
        bands
    }

    /// Split a whole mono block into reusable per-band scratch buffers.
    ///
    /// The first `num_bands` entries of `bands_out` are resized to `input_len` and
    /// overwritten in place.
    pub fn process_block<F>(
        &self,
        input_len: usize,
        states: &mut [BiquadState],
        bands_out: &mut [Vec<f32>],
        mut sample_at: F,
    ) where
        F: FnMut(usize) -> f32,
    {
        debug_assert!(bands_out.len() >= self.num_bands);
        for band in bands_out.iter_mut().take(self.num_bands) {
            band.resize(input_len, 0.0);
        }
        for sample_idx in 0..input_len {
            let split = self.process_sample(sample_at(sample_idx), states);
            for band_idx in 0..self.num_bands {
                bands_out[band_idx][sample_idx] = split.get(band_idx);
            }
        }
    }
}

/// Stack-backed fixed-capacity array for band samples (avoids heap allocation in hot path).
///
/// Maximum 8 bands (7 crossover points), which covers all practical use cases.
pub struct SmallBands {
    data: [f32; 8],
    len: usize,
}

impl SmallBands {
    pub(crate) fn new(len: usize) -> Self {
        debug_assert!(len <= 8, "SmallBands supports at most 8 bands");
        Self {
            data: [0.0; 8],
            len,
        }
    }

    /// Passthrough: wraps a single sample as a 1-band `SmallBands` (no filtering).
    pub fn single(v: f32) -> Self {
        Self {
            data: [v, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            len: 1,
        }
    }

    pub(crate) fn set(&mut self, i: usize, v: f32) {
        self.data[i] = v;
    }

    #[inline]
    pub fn get(&self, i: usize) -> f32 {
        self.data[i]
    }

    pub fn len(&self) -> usize {
        self.len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Steady-state amplitude of the summed bands for a unit sine, estimated
    /// as RMS·√2 over the second half of a 1 s run. RMS is phase-independent
    /// (unlike the sampled peak, which under-reads when only a few samples
    /// hit each period, e.g. 6 samples/period at 8 kHz). Every frequency the
    /// tests use completes an integer number of periods in the 0.5 s window,
    /// so the estimate is exact up to f32 rounding.
    fn band_sum_amplitude(bank: &LR4CrossoverBank, freq: f32, sample_rate: u32) -> f32 {
        let mut states = vec![BiquadState::default(); bank.state_count()];
        let n = 48000;
        let mut acc = 0.0f64;
        for i in 0..n {
            let t = i as f32 / sample_rate as f32;
            let x = (2.0 * std::f32::consts::PI * freq * t).sin();
            let bands = bank.process_sample(x, &mut states);
            if i >= n / 2 {
                let sum: f32 = (0..bands.len()).map(|b| bands.get(b)).sum();
                acc += (sum as f64) * (sum as f64);
            }
        }
        ((acc / (n / 2) as f64).sqrt() * std::f64::consts::SQRT_2) as f32
    }

    /// LP + HP of one LR4 splitter sum to an allpass: the recombined bands
    /// must stay flat in magnitude (±0.15 dB) across the spectrum, including
    /// right at the cutoff. (The sum is no longer sample-exact — the phase
    /// rotates — which is the standard speaker-crossover trade.)
    #[test]
    fn two_band_sum_is_magnitude_flat() {
        let sample_rate = 48000u32;
        let bank = LR4CrossoverBank::new(&[80.0], sample_rate);
        assert_eq!(bank.num_bands, 2);
        for freq in [20.0, 40.0, 80.0, 160.0, 1000.0, 8000.0] {
            let a = band_sum_amplitude(&bank, freq, sample_rate);
            assert!(
                (0.983..=1.017).contains(&a),
                "band sum must be magnitude-flat at {freq} Hz, got {a}"
            );
        }
    }

    /// The low band must be a real Linkwitz-Riley low-pass: flat passband
    /// (no resonant peak), −6 dB at the cutoff, 24 dB/oct beyond. Guards the
    /// Butterworth Q of the cascaded sections — a Q of √2 instead of 1/√2
    /// turns the crossover into a +6 dB resonator at the cutoff.
    #[test]
    fn lr4_lowpass_frequency_response() {
        let sample_rate = 48000u32;
        let fc = 120.0f32;
        let bank = LR4CrossoverBank::new(&[fc], sample_rate);

        // Steady-state peak amplitude of the LP band for a unit sine.
        let lp_amplitude = |freq: f32| -> f32 {
            let mut states = vec![BiquadState::default(); bank.state_count()];
            let n = 48000;
            let mut peak = 0.0f32;
            for i in 0..n {
                let t = i as f32 / sample_rate as f32;
                let x = (2.0 * std::f32::consts::PI * freq * t).sin();
                let bands = bank.process_sample(x, &mut states);
                if i > n / 2 {
                    peak = peak.max(bands.get(0).abs());
                }
            }
            peak
        };

        // Flat passband: within (−1.2, +0.2) dB up to fc/2, never peaking.
        for freq in [20.0, 30.0, 60.0] {
            let a = lp_amplitude(freq);
            assert!(a < 1.02, "LP passband peaks at {freq} Hz: {a}");
            assert!(a > 0.87, "LP passband droops at {freq} Hz: {a}");
        }
        // −6 dB at the cutoff — the Linkwitz-Riley signature.
        let at_fc = lp_amplitude(fc);
        assert!(
            (at_fc - 0.5).abs() < 0.03,
            "LP at fc must sit at −6 dB (0.5), got {at_fc}"
        );
        // 24 dB/oct: two octaves up ≈ −48 dB.
        let at_4fc = lp_amplitude(4.0 * fc);
        assert!(
            at_4fc < 0.008,
            "LP two octaves up must be ≤ −42 dB, got {at_4fc}"
        );
    }

    /// Three bands must also recombine flat — this exercises the multiway
    /// phase compensation: without the allpass applied to band 0 at the
    /// second splitter, the sum would ripple around the upper cutoff.
    #[test]
    fn three_band_sum_is_magnitude_flat() {
        let sample_rate = 48000u32;
        let bank = LR4CrossoverBank::new(&[80.0, 8000.0], sample_rate);
        assert_eq!(bank.num_bands, 3);
        for freq in [30.0, 80.0, 440.0, 4000.0, 8000.0, 12000.0] {
            let a = band_sum_amplitude(&bank, freq, sample_rate);
            assert!(
                (0.983..=1.017).contains(&a),
                "3-band sum must be magnitude-flat at {freq} Hz, got {a}"
            );
        }
    }

    /// Mirror of the low-pass response test: the high band must be a real
    /// LR4 high-pass — flat passband above, −6 dB at the cutoff, 24 dB/oct
    /// below. This is the property the subtractive design (HP = input − LP)
    /// lacked: its low-frequency rejection was only ~6 dB/oct.
    #[test]
    fn lr4_highpass_frequency_response() {
        let sample_rate = 48000u32;
        let fc = 120.0f32;
        let bank = LR4CrossoverBank::new(&[fc], sample_rate);

        let hp_amplitude = |freq: f32| -> f32 {
            let mut states = vec![BiquadState::default(); bank.state_count()];
            let n = 48000;
            let mut peak = 0.0f32;
            for i in 0..n {
                let t = i as f32 / sample_rate as f32;
                let x = (2.0 * std::f32::consts::PI * freq * t).sin();
                let bands = bank.process_sample(x, &mut states);
                if i > n / 2 {
                    peak = peak.max(bands.get(1).abs());
                }
            }
            peak
        };

        // Flat passband above the cutoff, never peaking.
        for freq in [240.0, 480.0, 1000.0] {
            let a = hp_amplitude(freq);
            assert!(a < 1.02, "HP passband peaks at {freq} Hz: {a}");
            assert!(a > 0.87, "HP passband droops at {freq} Hz: {a}");
        }
        // −6 dB at the cutoff.
        let at_fc = hp_amplitude(fc);
        assert!(
            (at_fc - 0.5).abs() < 0.03,
            "HP at fc must sit at −6 dB (0.5), got {at_fc}"
        );
        // 24 dB/oct: two octaves down ≈ −48 dB.
        let at_quarter = hp_amplitude(fc / 4.0);
        assert!(
            at_quarter < 0.008,
            "HP two octaves down must be ≤ −42 dB, got {at_quarter}"
        );
    }
}
