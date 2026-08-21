//! Linear-phase FIR crossover filter bank.
//!
//! Quality-first alternative to [`super::filter::LR4CrossoverBank`] for
//! offline-tolerant playback (film): every band is a symmetric (type I) FIR,
//! so all bands share one constant group delay and the recombined output is a
//! *pure delay* of the input — flat in magnitude AND phase, sample-exact up to
//! f32 rounding. The LR4 bank only achieves magnitude flatness; its allpass
//! cascade rotates phase around every cutoff.
//!
//! Band construction — differences of lowpasses:
//!   LP_k     = Kaiser-windowed sinc lowpass at cutoff k (odd length, linear
//!              phase, delay D = (taps−1)/2, −6 dB at fc like LR4)
//!   Band 0   = LP_0
//!   Band k   = LP_k − LP_{k−1}
//!   Band N−1 = δ_D − LP_{N−2}       (input delayed by D, minus the last LP)
//!
//! The sum telescopes to δ_D exactly: reconstruction is perfect by
//! construction, independent of the window quality. Subtracting from a
//! delayed impulse is only viable because the phases match at every
//! frequency; with the IIR bank the same trick degrades to ~6 dB/oct
//! rejection (see the LR4 module doc), whereas here the complement's
//! stopband rejection equals the lowpass's passband ripple (≈ the design
//! stopband attenuation).
//!
//! Runtime: uniform-partitioned overlap-save convolution. The input is
//! blocked into `BLOCK`-sample hops; one forward FFT per hop is shared by all
//! bands, then each of the N−1 lowpasses costs one spectrum
//! multiply-accumulate over the partition delay line plus one inverse FFT.
//! Steady state performs no allocations. Latency is
//! `(taps−1)/2 + BLOCK − 1` samples — constant, reported by
//! [`FirCrossoverBank::latency_samples`] so other signal paths can be
//! delay-compensated against the filtered ones.

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

use super::filter::SmallBands;

/// Internal hop size (samples). Each hop triggers one forward FFT of
/// `2 * BLOCK`; the filter kernels are partitioned into `BLOCK`-sized chunks.
const BLOCK: usize = 1024;

/// Design parameters for [`FirCrossoverBank::new`].
#[derive(Clone, Copy)]
pub struct FirCrossoverSpec {
    /// Kaiser stopband attenuation target in dB (also bounds passband ripple).
    pub stopband_atten_db: f32,
    /// Total transition width as a fraction of the lowest cutoff frequency
    /// (the transition band is `fc ± ratio·fc_min/2` around each cutoff).
    /// The lowest cutoff needs the narrowest absolute transition, so it
    /// dictates the shared filter length.
    pub transition_ratio: f32,
    /// Upper bound on the filter length, guarding pathological configs
    /// (e.g. a 1 Hz cutoff would otherwise request millions of taps).
    pub max_taps: usize,
}

impl Default for FirCrossoverSpec {
    fn default() -> Self {
        Self {
            stopband_atten_db: 100.0,
            transition_ratio: 0.5,
            max_taps: 65535,
        }
    }
}

/// Zeroth-order modified Bessel function of the first kind (series expansion,
/// converges quickly for the β range Kaiser designs use).
fn bessel_i0(x: f64) -> f64 {
    let half = x / 2.0;
    let mut sum = 1.0;
    let mut term = 1.0;
    for k in 1..=64 {
        let f = half / k as f64;
        term *= f * f;
        sum += term;
        if term < sum * 1e-16 {
            break;
        }
    }
    sum
}

/// Kaiser window shape parameter for a given stopband attenuation.
fn kaiser_beta(atten_db: f64) -> f64 {
    if atten_db > 50.0 {
        0.1102 * (atten_db - 8.7)
    } else if atten_db >= 21.0 {
        0.5842 * (atten_db - 21.0).powf(0.4) + 0.07886 * (atten_db - 21.0)
    } else {
        0.0
    }
}

/// Kaiser-windowed-sinc linear-phase lowpass, normalized to unity DC gain.
/// `taps` must be odd (type I FIR: symmetric, integer group delay).
fn design_lowpass(fc: f64, sample_rate: f64, taps: usize, beta: f64) -> Vec<f32> {
    debug_assert!(taps % 2 == 1);
    let mid = ((taps - 1) / 2) as f64;
    let wc = 2.0 * std::f64::consts::PI * fc / sample_rate;
    let i0_beta = bessel_i0(beta);
    let mut h = vec![0.0f64; taps];
    for (n, tap) in h.iter_mut().enumerate() {
        let k = n as f64 - mid;
        let sinc = if k == 0.0 {
            wc / std::f64::consts::PI
        } else {
            (wc * k).sin() / (std::f64::consts::PI * k)
        };
        let r = k / mid;
        let window = bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / i0_beta;
        *tap = sinc * window;
    }
    let dc: f64 = h.iter().sum();
    h.iter().map(|&v| (v / dc) as f32).collect()
}

/// A linear-phase FIR crossover bank for `cutoffs.len() + 1` bands.
///
/// Build once per topology (like [`super::filter::LR4CrossoverBank`]); create
/// one [`FirCrossoverState`] per filtered channel with [`Self::make_state`]
/// and feed samples through [`Self::process_sample`] or
/// [`Self::process_block`].
pub struct FirCrossoverBank {
    /// Number of output bands (= cutoffs + 1). At most 8 (`SmallBands` cap).
    pub num_bands: usize,
    /// Shared odd length of every band kernel.
    taps: usize,
    /// Kernel group delay in samples: `(taps − 1) / 2`.
    kernel_delay: usize,
    /// Kernel partitions per lowpass: `ceil(taps / BLOCK)`.
    partitions: usize,
    fft: Arc<dyn RealToComplex<f32>>,
    ifft: Arc<dyn ComplexToReal<f32>>,
    /// Partitioned kernel spectra, `[lowpass][partition][bin]`,
    /// `BLOCK + 1` bins each (unnormalized; the inverse FFT applies 1/(2·BLOCK)).
    kernel_spectra: Vec<Vec<Vec<Complex<f32>>>>,
}

/// Per-channel streaming state for [`FirCrossoverBank`]. All buffers are
/// allocated up front; processing never allocates.
pub struct FirCrossoverState {
    /// Incoming samples not yet processed (fills up to `BLOCK`).
    pending: Vec<f32>,
    /// Previous input block (overlap-save history for the forward FFT).
    prev_block: Vec<f32>,
    /// Frequency-domain delay line: the last `partitions` input spectra.
    fdl: Vec<Vec<Complex<f32>>>,
    /// Index of the most recent spectrum in `fdl`.
    fdl_pos: usize,
    /// Last `kernel_delay` input samples, feeding the top band's delayed-input
    /// term (`δ_D − LP`).
    delay_hist: Vec<f32>,
    /// Scratch concatenation of `delay_hist` and the current block.
    delay_work: Vec<f32>,
    /// Lowpass block outputs, `[lowpass][sample]`.
    lp_out: Vec<Vec<f32>>,
    /// Current band output blocks, `[band][sample]`, read by `read_idx`.
    out: Vec<Vec<f32>>,
    read_idx: usize,
    fft_in: Vec<f32>,
    spec_acc: Vec<Complex<f32>>,
    ifft_out: Vec<f32>,
    fft_scratch: Vec<Complex<f32>>,
    ifft_scratch: Vec<Complex<f32>>,
}

impl FirCrossoverBank {
    /// Create a bank for the given cutoffs (Hz) and sample rate, sizing the
    /// filter from `spec` (Kaiser length estimate for the requested
    /// attenuation and transition width at the lowest cutoff).
    pub fn new(cutoffs: &[f32], sample_rate: u32) -> Self {
        Self::with_spec(cutoffs, sample_rate, FirCrossoverSpec::default())
    }

    /// [`Self::new`] with explicit design parameters.
    pub fn with_spec(cutoffs: &[f32], sample_rate: u32, spec: FirCrossoverSpec) -> Self {
        let nyquist = sample_rate as f64 / 2.0;
        let min_fc = cutoffs
            .iter()
            .fold(f64::INFINITY, |m, &fc| m.min(fc as f64))
            .clamp(1.0, nyquist - 1.0);
        let transition_hz = (spec.transition_ratio as f64 * min_fc).max(1.0);
        let atten = spec.stopband_atten_db as f64;
        // Kaiser length estimate: N ≈ (A − 8) / (2.285 · Δω) + 1.
        let d_omega = 2.0 * std::f64::consts::PI * transition_hz / sample_rate as f64;
        let est = ((atten - 8.0) / (2.285 * d_omega)).ceil() as usize + 1;
        let taps = est.clamp(63, spec.max_taps.max(63)) | 1;
        Self::with_taps(cutoffs, sample_rate, taps, spec.stopband_atten_db)
    }

    /// Create a bank with an explicit kernel length (`taps` is rounded up to
    /// odd). Latency and CPU scale with `taps`; transition width shrinks as
    /// `taps` grows.
    pub fn with_taps(
        cutoffs: &[f32],
        sample_rate: u32,
        taps: usize,
        stopband_atten_db: f32,
    ) -> Self {
        assert!(
            !cutoffs.is_empty(),
            "FIR crossover needs at least one cutoff"
        );
        assert!(
            cutoffs.len() + 1 <= 8,
            "SmallBands supports at most 8 bands"
        );
        debug_assert!(
            cutoffs.windows(2).all(|w| w[0] < w[1]),
            "cutoffs must be sorted ascending"
        );
        let taps = taps.max(63) | 1;
        let nyquist = sample_rate as f64 / 2.0;
        let beta = kaiser_beta(stopband_atten_db as f64);
        let lowpasses: Vec<Vec<f32>> = cutoffs
            .iter()
            .map(|&fc| {
                let fc = (fc as f64).clamp(1.0, nyquist - 1.0);
                design_lowpass(fc, sample_rate as f64, taps, beta)
            })
            .collect();

        let fft_len = 2 * BLOCK;
        let partitions = taps.div_ceil(BLOCK);
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(fft_len);
        let ifft = planner.plan_fft_inverse(fft_len);
        let mut scratch = fft.make_scratch_vec();
        let kernel_spectra = lowpasses
            .iter()
            .map(|h| {
                (0..partitions)
                    .map(|p| {
                        let chunk = &h[p * BLOCK..taps.min((p + 1) * BLOCK)];
                        let mut time = vec![0.0f32; fft_len];
                        time[..chunk.len()].copy_from_slice(chunk);
                        let mut spec = fft.make_output_vec();
                        fft.process_with_scratch(&mut time, &mut spec, &mut scratch)
                            .expect("kernel FFT sizes are fixed by construction");
                        spec
                    })
                    .collect()
            })
            .collect();

        Self {
            num_bands: cutoffs.len() + 1,
            taps,
            kernel_delay: (taps - 1) / 2,
            partitions,
            fft,
            ifft,
            kernel_spectra,
        }
    }

    /// Shared kernel length in samples.
    pub fn taps(&self) -> usize {
        self.taps
    }

    /// Total input→output delay in samples: kernel group delay plus block
    /// buffering. Identical for every band; unfiltered signal paths must be
    /// delayed by this amount to stay time-aligned.
    pub fn latency_samples(&self) -> usize {
        self.kernel_delay + BLOCK - 1
    }

    /// Allocate the streaming state for one channel.
    pub fn make_state(&self) -> FirCrossoverState {
        FirCrossoverState {
            pending: Vec::with_capacity(BLOCK),
            prev_block: vec![0.0; BLOCK],
            fdl: (0..self.partitions)
                .map(|_| self.fft.make_output_vec())
                .collect(),
            fdl_pos: 0,
            delay_hist: vec![0.0; self.kernel_delay],
            delay_work: vec![0.0; self.kernel_delay + BLOCK],
            lp_out: vec![vec![0.0; BLOCK]; self.num_bands - 1],
            out: vec![vec![0.0; BLOCK]; self.num_bands],
            read_idx: 0,
            fft_in: vec![0.0; 2 * BLOCK],
            spec_acc: self.fft.make_output_vec(),
            ifft_out: vec![0.0; 2 * BLOCK],
            fft_scratch: self.fft.make_scratch_vec(),
            ifft_scratch: self.ifft.make_scratch_vec(),
        }
    }

    /// Split `input` into `num_bands` band samples using the per-channel
    /// `state`. Output lags input by [`Self::latency_samples`] (zeros are
    /// emitted until the pipeline fills).
    pub fn process_sample(&self, input: f32, state: &mut FirCrossoverState) -> SmallBands {
        state.pending.push(input);
        if state.pending.len() == BLOCK {
            self.process_pending(state);
        }
        let mut bands = SmallBands::new(self.num_bands);
        for b in 0..self.num_bands {
            bands.set(b, state.out[b][state.read_idx]);
        }
        state.read_idx += 1;
        bands
    }

    /// Split a whole mono block into reusable per-band scratch buffers.
    /// Same contract as [`super::filter::LR4CrossoverBank::process_block`].
    pub fn process_block<F>(
        &self,
        input_len: usize,
        state: &mut FirCrossoverState,
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
            let split = self.process_sample(sample_at(sample_idx), state);
            for band_idx in 0..self.num_bands {
                bands_out[band_idx][sample_idx] = split.get(band_idx);
            }
        }
    }

    /// Consume the pending block: one shared forward FFT, one partitioned
    /// convolution per lowpass, then band outputs by telescoping differences.
    fn process_pending(&self, state: &mut FirCrossoverState) {
        let scale = 1.0 / (2 * BLOCK) as f32;

        // Overlap-save forward transform of [previous block | new block].
        state.fft_in[..BLOCK].copy_from_slice(&state.prev_block);
        state.fft_in[BLOCK..].copy_from_slice(&state.pending);
        state.prev_block.copy_from_slice(&state.pending);
        state.fdl_pos = (state.fdl_pos + 1) % self.partitions;
        let pos = state.fdl_pos;
        self.fft
            .process_with_scratch(
                &mut state.fft_in,
                &mut state.fdl[pos],
                &mut state.fft_scratch,
            )
            .expect("streaming FFT sizes are fixed by construction");

        for (lp, lp_out) in state.lp_out.iter_mut().enumerate() {
            state.spec_acc.fill(Complex::default());
            for p in 0..self.partitions {
                let src = &state.fdl[(pos + self.partitions - p) % self.partitions];
                let ker = &self.kernel_spectra[lp][p];
                for bin in 0..state.spec_acc.len() {
                    state.spec_acc[bin] += src[bin] * ker[bin];
                }
            }
            self.ifft
                .process_with_scratch(
                    &mut state.spec_acc,
                    &mut state.ifft_out,
                    &mut state.ifft_scratch,
                )
                .expect("streaming FFT sizes are fixed by construction");
            // Overlap-save: the first BLOCK samples are circular garbage.
            for (o, &v) in lp_out.iter_mut().zip(&state.ifft_out[BLOCK..]) {
                *o = v * scale;
            }
        }

        // Input delayed by the kernel group delay, for the top band.
        let d = self.kernel_delay;
        state.delay_work[..d].copy_from_slice(&state.delay_hist);
        state.delay_work[d..].copy_from_slice(&state.pending);
        state.delay_hist.copy_from_slice(&state.delay_work[BLOCK..]);

        // Band outputs telescope so their sum is exactly the delayed input.
        let n = self.num_bands;
        state.out[0].copy_from_slice(&state.lp_out[0]);
        for k in 1..n - 1 {
            for i in 0..BLOCK {
                state.out[k][i] = state.lp_out[k][i] - state.lp_out[k - 1][i];
            }
        }
        for i in 0..BLOCK {
            state.out[n - 1][i] = state.delay_work[i] - state.lp_out[n - 2][i];
        }

        state.pending.clear();
        state.read_idx = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic full-band test signal in [−1, 1] (LCG; no rand dep).
    fn noise(len: usize) -> Vec<f32> {
        let mut s = 0x1234_5678u32;
        (0..len)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (s >> 8) as f32 / (1 << 24) as f32 * 2.0 - 1.0
            })
            .collect()
    }

    /// The defining property: bands sum to a pure delay of the input, exact
    /// up to f32 rounding, for arbitrary full-band material. This holds by
    /// construction (telescoping) regardless of the window design, so the
    /// tolerance is float error only.
    #[test]
    fn bands_sum_to_pure_delay() {
        let sample_rate = 48000u32;
        let bank = FirCrossoverBank::with_taps(&[120.0, 2000.0], sample_rate, 1023, 90.0);
        assert_eq!(bank.num_bands, 3);
        let mut state = bank.make_state();
        let lat = bank.latency_samples();
        let x = noise(48000);
        let mut max_err = 0.0f32;
        for (i, &s) in x.iter().enumerate() {
            let bands = bank.process_sample(s, &mut state);
            let sum: f32 = (0..bands.len()).map(|b| bands.get(b)).sum();
            let expected = if i >= lat { x[i - lat] } else { 0.0 };
            max_err = max_err.max((sum - expected).abs());
        }
        assert!(
            max_err < 1e-5,
            "band sum must reconstruct the delayed input exactly, max err {max_err}"
        );
    }

    /// A unit impulse must come back (summed) as a unit impulse at exactly
    /// `latency_samples()` — this pins the latency accounting.
    #[test]
    fn impulse_reconstruction_peaks_at_latency() {
        let sample_rate = 48000u32;
        let bank = FirCrossoverBank::with_taps(&[120.0], sample_rate, 1023, 90.0);
        let mut state = bank.make_state();
        let lat = bank.latency_samples();
        let run = lat + 4 * BLOCK;
        let mut peak_idx = 0;
        let mut peak = 0.0f32;
        for i in 0..run {
            let x = if i == 0 { 1.0 } else { 0.0 };
            let bands = bank.process_sample(x, &mut state);
            let sum: f32 = (0..bands.len()).map(|b| bands.get(b)).sum();
            if sum.abs() > peak {
                peak = sum.abs();
                peak_idx = i;
            }
        }
        assert_eq!(
            peak_idx, lat,
            "reconstruction impulse must land at the reported latency"
        );
        assert!(
            (peak - 1.0).abs() < 1e-5,
            "impulse must survive with unit gain, got {peak}"
        );
    }

    /// Steady-state amplitude of one band for a unit sine, RMS·√2 over the
    /// second half of a 2 s run (settle covers latency + kernel length).
    fn band_amplitude(bank: &FirCrossoverBank, band: usize, freq: f32, sample_rate: u32) -> f32 {
        let mut state = bank.make_state();
        let n = 2 * sample_rate as usize;
        let mut acc = 0.0f64;
        for i in 0..n {
            let t = i as f32 / sample_rate as f32;
            let x = (2.0 * std::f32::consts::PI * freq * t).sin();
            let bands = bank.process_sample(x, &mut state);
            if i >= n / 2 {
                let v = bands.get(band) as f64;
                acc += v * v;
            }
        }
        ((acc / (n / 2) as f64).sqrt() * std::f64::consts::SQRT_2) as f32
    }

    /// Lowpass band: flat passband (within the Kaiser ripple), −6 dB at the
    /// cutoff (windowed sinc hits 0.5 at fc, matching the LR4 convention),
    /// and the designed stopband depth past the transition band.
    /// 1023 taps at 48 kHz give a ±134 Hz transition half-width, so a 1 kHz
    /// cutoff keeps 250–500 Hz in the passband and 2 kHz in the stopband.
    #[test]
    fn fir_lowpass_frequency_response() {
        let sample_rate = 48000u32;
        let bank = FirCrossoverBank::with_taps(&[1000.0], sample_rate, 1023, 90.0);
        for freq in [250.0, 500.0] {
            let a = band_amplitude(&bank, 0, freq, sample_rate);
            assert!(
                (0.999..=1.001).contains(&a),
                "LP passband must be flat at {freq} Hz, got {a}"
            );
        }
        let at_fc = band_amplitude(&bank, 0, 1000.0, sample_rate);
        assert!(
            (at_fc - 0.5).abs() < 0.02,
            "LP at fc must sit at −6 dB (0.5), got {at_fc}"
        );
        let stop = band_amplitude(&bank, 0, 2000.0, sample_rate);
        assert!(
            stop < 2e-4,
            "LP one octave up must reach the design stopband (≤ −74 dB), got {stop}"
        );
    }

    /// Complement band: flat passband above the cutoff and stopband rejection
    /// below equal to the lowpass's passband ripple — the property the
    /// subtractive IIR design lacked (only ~6 dB/oct there).
    #[test]
    fn fir_highpass_frequency_response() {
        let sample_rate = 48000u32;
        let bank = FirCrossoverBank::with_taps(&[1000.0], sample_rate, 1023, 90.0);
        for freq in [2000.0, 4000.0, 8000.0] {
            let a = band_amplitude(&bank, 1, freq, sample_rate);
            assert!(
                (0.999..=1.001).contains(&a),
                "HP passband must be flat at {freq} Hz, got {a}"
            );
        }
        let at_fc = band_amplitude(&bank, 1, 1000.0, sample_rate);
        assert!(
            (at_fc - 0.5).abs() < 0.02,
            "HP at fc must sit at −6 dB (0.5), got {at_fc}"
        );
        for freq in [250.0, 500.0] {
            let a = band_amplitude(&bank, 1, freq, sample_rate);
            assert!(
                a < 2e-4,
                "HP stopband at {freq} Hz must reach the design depth, got {a}"
            );
        }
    }

    /// Each band impulse response must be symmetric about the reported
    /// latency — that symmetry IS linear phase / constant group delay.
    #[test]
    fn band_impulse_responses_are_linear_phase() {
        let sample_rate = 48000u32;
        let taps = 1023;
        let bank = FirCrossoverBank::with_taps(&[1000.0], sample_rate, taps, 90.0);
        let lat = bank.latency_samples();
        let mut state = bank.make_state();
        let run = lat + taps + 2 * BLOCK;
        let mut irs: Vec<Vec<f32>> = vec![Vec::with_capacity(run); bank.num_bands];
        for i in 0..run {
            let x = if i == 0 { 1.0 } else { 0.0 };
            let bands = bank.process_sample(x, &mut state);
            for (b, ir) in irs.iter_mut().enumerate() {
                ir.push(bands.get(b));
            }
        }
        let half = (taps - 1) / 2;
        for (b, ir) in irs.iter().enumerate() {
            for k in 1..=half {
                let err = (ir[lat - k] - ir[lat + k]).abs();
                assert!(
                    err < 1e-6,
                    "band {b} impulse response must be symmetric (k={k}, err={err})"
                );
            }
        }
    }

    /// The default spec must size the kernel from the lowest cutoff and keep
    /// the latency accounting consistent.
    #[test]
    fn default_spec_sizes_from_lowest_cutoff() {
        let bank = FirCrossoverBank::new(&[80.0, 3000.0], 48000);
        assert_eq!(bank.taps() % 2, 1, "type I FIR needs an odd length");
        assert!(
            (3000..=20000).contains(&bank.taps()),
            "default design for an 80 Hz cutoff should land in the thousands of taps, got {}",
            bank.taps()
        );
        assert_eq!(bank.latency_samples(), (bank.taps() - 1) / 2 + BLOCK - 1);
    }
}
