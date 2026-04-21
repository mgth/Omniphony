//! Cascaded biquad crossover filter bank with perfect reconstruction.
//!
//! Each splitter stage produces:
//!   LP  = BW2_LP(BW2_LP(input))  — 24 dB/oct Linkwitz-Riley LP
//!   HP  = input − LP              — exact complement; LP + HP = input always
//!
//! For N bands, N-1 splitters are chained:
//!   Band 0   = LP of splitter 0
//!   Band k   = LP of splitter k  (applied to HP of splitter k-1)
//!   Band N-1 = HP of splitter N-2
//!
//! Sum of all bands = original signal (perfect reconstruction by induction).
//!
//! State layout per splitter (2 entries):
//!   states[0] : stage 1 BW2-LP
//!   states[1] : stage 2 BW2-LP
//!
//! Total state count per object = 2 × (N-1).

/// State for a single Direct-Form-II Transposed biquad section.
#[derive(Clone, Default)]
pub struct BiquadState {
    z1: f32,
    z2: f32,
}

/// Biquad coefficients: `[b0, b1, b2, a1, a2]` in Direct-Form-II Transposed.
#[derive(Clone, Copy)]
struct BiquadCoeffs([f32; 5]);

/// Process one sample through a biquad (Direct Form II Transposed).
#[inline(always)]
fn biquad(input: f32, c: BiquadCoeffs, s: &mut BiquadState) -> f32 {
    let [b0, b1, b2, a1, a2] = c.0;
    let out = b0 * input + s.z1;
    s.z1 = b1 * input - a1 * out + s.z2;
    s.z2 = b2 * input - a2 * out;
    out
}

/// Compute 2nd-order Butterworth LP biquad coefficients at `fc` Hz.
fn butterworth2_lp(fc: f32, sample_rate: u32) -> BiquadCoeffs {
    let k = (std::f32::consts::PI * fc / sample_rate as f32).tan();
    let q = std::f32::consts::SQRT_2;
    let norm = 1.0 + k / q + k * k;
    let b0 = k * k / norm;
    let b1 = 2.0 * b0;
    let b2 = b0;
    let a1 = 2.0 * (k * k - 1.0) / norm;
    let a2 = (1.0 - k / q + k * k) / norm;
    BiquadCoeffs([b0, b1, b2, a1, a2])
}

/// Pre-computed coefficients for one cascaded-LP splitter at a given cutoff.
struct Splitter {
    /// First Butterworth LP stage.
    stage1: BiquadCoeffs,
    /// Second Butterworth LP stage (LR4 LP = stage1 → stage2).
    stage2: BiquadCoeffs,
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
                    stage1: butterworth2_lp(fc, sample_rate),
                    stage2: butterworth2_lp(fc, sample_rate),
                }
            })
            .collect::<Vec<_>>();
        let num_bands = splitters.len() + 1;
        Self { splitters, num_bands }
    }

    /// Number of `BiquadState` entries required per object.
    ///
    /// Allocate `vec![BiquadState::default(); state_count()]` for each object.
    pub fn state_count(&self) -> usize {
        self.splitters.len() * 2
    }

    /// Split `input` into `num_bands` band samples using the per-object `states`.
    ///
    /// `states` must have length `state_count()`.  The returned `SmallBands` has
    /// length `num_bands`.  Caller owns the allocation (stack-backed inline vec).
    pub fn process_sample(&self, input: f32, states: &mut [BiquadState]) -> SmallBands {
        let mut signal = input;
        let mut bands = SmallBands::new(self.num_bands);

        for (si, splitter) in self.splitters.iter().enumerate() {
            let base = si * 2;
            let after_stage1 = biquad(signal, splitter.stage1, &mut states[base]);
            let lp_out = biquad(after_stage1, splitter.stage2, &mut states[base + 1]);
            // HP = exact complement: LP + HP = signal always.
            let hp_out = signal - lp_out;

            bands.set(si, lp_out);
            signal = hp_out;
        }
        bands.set(self.splitters.len(), signal);
        bands
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
    fn new(len: usize) -> Self {
        debug_assert!(len <= 8, "SmallBands supports at most 8 bands");
        Self { data: [0.0; 8], len }
    }

    /// Passthrough: wraps a single sample as a 1-band `SmallBands` (no filtering).
    pub fn single(v: f32) -> Self {
        Self { data: [v, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], len: 1 }
    }

    fn set(&mut self, i: usize, v: f32) {
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

    /// LP + HP of a single LR4 splitter must sum to input within ±1e-5 (exact complement).
    #[test]
    fn test_lr4_reconstruction() {
        let sample_rate = 48000u32;
        let bank = LR4CrossoverBank::new(&[80.0], sample_rate);
        assert_eq!(bank.num_bands, 2);
        let mut states = vec![BiquadState::default(); bank.state_count()];

        // Run 8192 samples of a 1 kHz sine; check reconstruction after steady state.
        let freq = 1000.0_f32;
        let mut max_error: f32 = 0.0;
        for i in 0..8192 {
            let t = i as f32 / sample_rate as f32;
            let x = (2.0 * std::f32::consts::PI * freq * t).sin();
            let bands = bank.process_sample(x, &mut states);
            if i > 4096 {
                let reconstructed = bands.get(0) + bands.get(1);
                max_error = max_error.max((reconstructed - x).abs());
            }
        }
        assert!(max_error < 1e-5, "max reconstruction error = {max_error}");
    }

    /// Multi-band reconstruction: 3 bands must also sum to input.
    #[test]
    fn test_lr4_3band_reconstruction() {
        let sample_rate = 48000u32;
        let bank = LR4CrossoverBank::new(&[80.0, 8000.0], sample_rate);
        assert_eq!(bank.num_bands, 3);
        let mut states = vec![BiquadState::default(); bank.state_count()];

        let freq = 440.0_f32;
        let mut max_error: f32 = 0.0;
        for i in 0..8192 {
            let t = i as f32 / sample_rate as f32;
            let x = (2.0 * std::f32::consts::PI * freq * t).sin();
            let bands = bank.process_sample(x, &mut states);
            if i > 4096 {
                let reconstructed = bands.get(0) + bands.get(1) + bands.get(2);
                max_error = max_error.max((reconstructed - x).abs());
            }
        }
        assert!(max_error < 1e-5, "max reconstruction error = {max_error}");
    }
}
