//! IIR building blocks: the transposed direct-form-II biquad with its
//! coefficient designs, and the one-pole coefficient helpers.
//!
//! Every function keeps the exact arithmetic of the copy it replaced, so
//! swapping a caller onto it changes no output bit.

use std::f32::consts::{PI, TAU};

/// State for a single Direct-Form-II Transposed biquad section.
#[derive(Clone, Copy, Default)]
pub struct BiquadState {
    z1: f32,
    z2: f32,
}

/// Biquad coefficients: `[b0, b1, b2, a1, a2]` in Direct-Form-II Transposed
/// (`a0` normalised to 1).
#[derive(Clone, Copy)]
pub struct BiquadCoeffs(pub [f32; 5]);

/// Process one sample through a biquad (Direct Form II Transposed). The
/// free-function form serves banks that share one set of coefficients across
/// many states (the crossover); [`Biquad`] bundles both for a lone filter.
#[inline(always)]
pub fn biquad(input: f32, c: BiquadCoeffs, s: &mut BiquadState) -> f32 {
    let [b0, b1, b2, a1, a2] = c.0;
    let out = b0 * input + s.z1;
    s.z1 = b1 * input - a1 * out + s.z2;
    s.z2 = b2 * input - a2 * out;
    out
}

/// Shared bilinear-transform pieces of a 2nd-order Butterworth section at
/// `fc` Hz: `(k², norm, a1, a2)`. LP, HP and the matching allpass all use
/// the same denominator.
fn butterworth2_parts(fc: f32, sample_rate: u32) -> (f32, f32, f32, f32) {
    let k = (PI * fc / sample_rate as f32).tan();
    // Butterworth damping: Q = 1/√2. Two cascaded sections then form a true
    // Linkwitz-Riley 4th-order filter (flat passband, −6 dB at fc). A Q of
    // √2 here would instead peak +3 dB per section (+6 dB at fc combined).
    let q = std::f32::consts::FRAC_1_SQRT_2;
    let norm = 1.0 + k / q + k * k;
    let a1 = 2.0 * (k * k - 1.0) / norm;
    let a2 = (1.0 - k / q + k * k) / norm;
    (k * k, norm, a1, a2)
}

impl BiquadCoeffs {
    /// 2nd-order Butterworth low-pass at `fc` Hz (bilinear transform).
    pub fn butterworth2_lp(fc: f32, sample_rate: u32) -> Self {
        let (k2, norm, a1, a2) = butterworth2_parts(fc, sample_rate);
        let b0 = k2 / norm;
        Self([b0, 2.0 * b0, b0, a1, a2])
    }

    /// 2nd-order Butterworth high-pass at `fc` Hz (bilinear transform).
    pub fn butterworth2_hp(fc: f32, sample_rate: u32) -> Self {
        let (_, norm, a1, a2) = butterworth2_parts(fc, sample_rate);
        let b0 = 1.0 / norm;
        Self([b0, -2.0 * b0, b0, a1, a2])
    }

    /// 2nd-order allpass at `fc` Hz with the same poles as the Butterworth
    /// sections — exactly LR4_LP(fc) + LR4_HP(fc).
    pub fn butterworth2_allpass(fc: f32, sample_rate: u32) -> Self {
        let (_, _, a1, a2) = butterworth2_parts(fc, sample_rate);
        Self([a2, a1, 1.0, a1, a2])
    }

    /// RBJ-cookbook high-pass at `fc` Hz, quality `q`, sample rate `fs` Hz.
    /// The normalised cutoff is clamped to `[1e-4, 0.49]` so any input
    /// yields a stable filter.
    pub fn rbj_highpass(fs: f32, fc: f32, q: f32) -> Self {
        let w0 = TAU * (fc / fs).clamp(1.0e-4, 0.49);
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self([
            ((1.0 + cos) / 2.0) / a0,
            (-(1.0 + cos)) / a0,
            ((1.0 + cos) / 2.0) / a0,
            (-2.0 * cos) / a0,
            (1.0 - alpha) / a0,
        ])
    }
}

/// A self-contained biquad: coefficients plus their state.
#[derive(Clone, Copy)]
pub struct Biquad {
    coeffs: BiquadCoeffs,
    state: BiquadState,
}

impl Biquad {
    /// A filter with `coeffs` and silent state.
    pub fn new(coeffs: BiquadCoeffs) -> Self {
        Self {
            coeffs,
            state: BiquadState::default(),
        }
    }

    /// Swap the coefficients, keeping the state, so a live retune does not
    /// click.
    pub fn set_coeffs(&mut self, coeffs: BiquadCoeffs) {
        self.coeffs = coeffs;
    }

    /// RBJ-cookbook high-pass (see [`BiquadCoeffs::rbj_highpass`]).
    pub fn highpass(fs: f32, fc: f32, q: f32) -> Self {
        Self::new(BiquadCoeffs::rbj_highpass(fs, fc, q))
    }

    /// Retune to an RBJ high-pass, keeping the state so a live cutoff change
    /// does not click.
    pub fn set_highpass(&mut self, fs: f32, fc: f32, q: f32) {
        self.set_coeffs(BiquadCoeffs::rbj_highpass(fs, fc, q));
    }

    /// Zero the state, keeping the coefficients.
    pub fn reset(&mut self) {
        self.state = BiquadState::default();
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        biquad(x, self.coeffs, &mut self.state)
    }
}

/// Pole of the one-pole low-pass `y += (x − y)·(1 − p)` with its corner at
/// `cutoff_hz`: `p = exp(−2π·fc/fs)`.
#[inline]
pub fn one_pole_pole(cutoff_hz: f32, sample_rate: u32) -> f32 {
    (-TAU * cutoff_hz / sample_rate as f32).exp()
}

/// Per-step smoothing factor `1 − exp(−dt/τ)` of a one-pole with time
/// constant `tau`, advanced by `dt` (same unit as `tau`). With `dt = 1` and
/// `tau` in samples it is the per-sample factor.
#[inline]
pub fn one_pole_smoothing(dt: f32, tau: f32) -> f32 {
    1.0 - (-dt / tau).exp()
}

/// Trapezoidal (bilinear) one-pole integrator gain `g/(1+g)` with
/// `g = tan(π·fc/fs)`: its low-pass and high-pass outputs are exact
/// complements, zero at Nyquist and at DC respectively.
#[inline]
pub fn one_pole_bilinear_gain(cutoff_hz: f32, sample_rate: u32) -> f32 {
    let g = (PI * cutoff_hz / sample_rate as f32).tan();
    g / (1.0 + g)
}

/// A one-pole pole specified at 48 kHz, carried to `sample_rate` with the
/// same time constant: `p^(48000/fs)`, so a smoother fades over the same
/// milliseconds at every rate.
#[inline]
pub fn pole_at_rate(pole_48k: f32, sample_rate: u32) -> f32 {
    pole_48k.powf(48_000.0 / sample_rate as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_coeffs_keeps_the_state() {
        let mut a = Biquad::new(BiquadCoeffs::butterworth2_lp(1000.0, 48_000));
        let mut b = a;
        for i in 0..32 {
            let x = (i % 5) as f32 - 2.0;
            a.process(x);
            b.process(x);
        }
        // Retune one mid-stream; its next output must still carry the
        // history, i.e. differ from a freshly reset filter's.
        let hp = BiquadCoeffs::butterworth2_hp(500.0, 48_000);
        a.set_coeffs(hp);
        b.reset();
        b.set_coeffs(hp);
        assert_ne!(a.process(0.0), b.process(0.0));
    }

    #[test]
    fn pole_at_rate_is_identity_at_48k() {
        assert_eq!(pole_at_rate(0.35, 48_000), 0.35);
    }
}
