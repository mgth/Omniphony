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

pub use crate::dsp::iir::BiquadState;
use crate::dsp::iir::{BiquadCoeffs, biquad};

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
    /// The CPU has AVX2: [`Self::process_block_lanes`] runs its 8 lanes in
    /// one register instead of two. Detected once, here.
    #[cfg(target_arch = "x86_64")]
    avx2: bool,
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
                    lp1: BiquadCoeffs::butterworth2_lp(fc, sample_rate),
                    lp2: BiquadCoeffs::butterworth2_lp(fc, sample_rate),
                    hp1: BiquadCoeffs::butterworth2_hp(fc, sample_rate),
                    hp2: BiquadCoeffs::butterworth2_hp(fc, sample_rate),
                    ap: BiquadCoeffs::butterworth2_allpass(fc, sample_rate),
                }
            })
            .collect::<Vec<_>>();
        let num_bands = splitters.len() + 1;
        Self {
            splitters,
            num_bands,
            #[cfg(target_arch = "x86_64")]
            avx2: std::arch::is_x86_feature_detected!("avx2"),
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

/// Objects filtered together by [`LR4CrossoverBank::process_block_lanes`]
/// (#750). Every object goes through the same bank with the same
/// coefficients, only the states differ, so one instruction can advance a
/// biquad for several objects: 8 lanes are one AVX2 register, two SSE or two
/// NEON ones.
pub const LANES: usize = 8;

/// Lane-major filter memory for one group of [`LANES`] objects: entry `i`
/// holds state `i` of every lane, so a lane-wide load fetches one biquad's
/// `z1` (or `z2`) for the whole group. Filled from each object's own
/// `Vec<BiquadState>` before a block and written back after it
/// ([`Self::load`], [`Self::store`]), so the per-object states keep their
/// lifetime and reset rules; that costs two copies of the states per block,
/// against `samples × biquads` filter evaluations.
pub struct LaneStates {
    z1: Vec<[f32; LANES]>,
    z2: Vec<[f32; LANES]>,
}

impl LaneStates {
    /// Room for `state_count` states per lane. Grown by [`Self::ensure`],
    /// never shrunk.
    pub fn new(state_count: usize) -> Self {
        Self {
            z1: vec![[0.0; LANES]; state_count],
            z2: vec![[0.0; LANES]; state_count],
        }
    }

    /// Make room for `state_count` states per lane (no-op once big enough).
    pub fn ensure(&mut self, state_count: usize) {
        if self.z1.len() < state_count {
            self.z1.resize(state_count, [0.0; LANES]);
            self.z2.resize(state_count, [0.0; LANES]);
        }
    }

    /// Copy one object's states into `lane`.
    pub fn load(&mut self, lane: usize, states: &[BiquadState]) {
        for (i, state) in states.iter().enumerate() {
            let (z1, z2) = state.delays();
            self.z1[i][lane] = z1;
            self.z2[i][lane] = z2;
        }
    }

    /// Zero `lane`'s first `state_count` states: a lane with no object this
    /// block runs on silence and is never stored.
    pub fn clear(&mut self, lane: usize, state_count: usize) {
        for i in 0..state_count {
            self.z1[i][lane] = 0.0;
            self.z2[i][lane] = 0.0;
        }
    }

    /// Copy `lane` back into one object's states.
    pub fn store(&self, lane: usize, states: &mut [BiquadState]) {
        for (i, state) in states.iter_mut().enumerate() {
            *state = BiquadState::from_delays(self.z1[i][lane], self.z2[i][lane]);
        }
    }
}

/// [`biquad`] on every lane: the same three statements in the same order per
/// lane, so each lane's result is bit-identical to the scalar filter (Rust
/// never contracts `a * b + c` into a fused multiply-add).
#[inline(always)]
fn biquad_lanes(
    input: [f32; LANES],
    c: BiquadCoeffs,
    z1: &mut [f32; LANES],
    z2: &mut [f32; LANES],
) -> [f32; LANES] {
    let [b0, b1, b2, a1, a2] = c.0;
    let mut out = [0.0; LANES];
    for l in 0..LANES {
        out[l] = b0 * input[l] + z1[l];
        z1[l] = b1 * input[l] - a1 * out[l] + z2[l];
        z2[l] = b2 * input[l] - a2 * out[l];
    }
    out
}

impl LR4CrossoverBank {
    /// [`Self::process_sample`] for [`LANES`] objects at once, on lane-major
    /// states. Same splitter order, same compensation allpasses.
    #[inline(always)]
    fn process_sample_lanes(
        &self,
        input: [f32; LANES],
        states: &mut LaneStates,
        bands: &mut [[f32; LANES]; 8],
    ) {
        let mut signal = input;
        let ap_base = self.splitters.len() * 4;
        let (z1, z2) = (&mut states.z1, &mut states.z2);
        for (si, splitter) in self.splitters.iter().enumerate() {
            let base = si * 4;
            let lp1 = biquad_lanes(signal, splitter.lp1, &mut z1[base], &mut z2[base]);
            let lp = biquad_lanes(lp1, splitter.lp2, &mut z1[base + 1], &mut z2[base + 1]);
            let hp1 = biquad_lanes(signal, splitter.hp1, &mut z1[base + 2], &mut z2[base + 2]);
            let hp = biquad_lanes(hp1, splitter.hp2, &mut z1[base + 3], &mut z2[base + 3]);
            let ap_row = ap_base + si * si.saturating_sub(1) / 2;
            for b in 0..si {
                bands[b] = biquad_lanes(
                    bands[b],
                    splitter.ap,
                    &mut z1[ap_row + b],
                    &mut z2[ap_row + b],
                );
            }
            bands[si] = lp;
            signal = hp;
        }
        bands[self.splitters.len()] = signal;
    }

    #[inline(always)]
    fn process_block_lanes_body(
        &self,
        inputs: &[[f32; LANES]],
        states: &mut LaneStates,
        active: [bool; LANES],
        bands_out: &mut [[Vec<f32>; 8]],
    ) {
        let num_bands = self.num_bands;
        for (lane, out) in bands_out.iter_mut().enumerate().take(LANES) {
            if active[lane] {
                for band in out.iter_mut().take(num_bands) {
                    band.resize(inputs.len(), 0.0);
                }
            }
        }
        let mut bands = [[0.0f32; LANES]; 8];
        for (sample_idx, &input) in inputs.iter().enumerate() {
            self.process_sample_lanes(input, states, &mut bands);
            for (lane, out) in bands_out.iter_mut().enumerate().take(LANES) {
                if !active[lane] {
                    continue;
                }
                for (band, values) in bands.iter().enumerate().take(num_bands) {
                    out[band][sample_idx] = values[lane];
                }
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    fn process_block_lanes_avx2(
        &self,
        inputs: &[[f32; LANES]],
        states: &mut LaneStates,
        active: [bool; LANES],
        bands_out: &mut [[Vec<f32>; 8]],
    ) {
        self.process_block_lanes_body(inputs, states, active, bands_out)
    }

    /// Split a block for up to [`LANES`] objects at once (#750).
    ///
    /// `inputs[s][lane]` is lane `lane`'s input sample `s`; `states` holds
    /// the lanes' filter memory (see [`LaneStates`]). For every lane marked
    /// `active`, the first `num_bands` buffers of `bands_out[lane]` are
    /// resized to the block and overwritten with exactly what
    /// [`Self::process_block`] produces for that object alone; inactive lanes
    /// are computed (on whatever their states hold) and not written.
    pub fn process_block_lanes(
        &self,
        inputs: &[[f32; LANES]],
        states: &mut LaneStates,
        active: [bool; LANES],
        bands_out: &mut [[Vec<f32>; 8]],
    ) {
        debug_assert!(bands_out.len() >= LANES);
        debug_assert!(states.z1.len() >= self.state_count());
        #[cfg(target_arch = "x86_64")]
        if self.avx2 {
            // SAFETY: `avx2` is set from runtime detection in `new`.
            unsafe { self.process_block_lanes_avx2(inputs, states, active, bands_out) };
            return;
        }
        self.process_block_lanes_body(inputs, states, active, bands_out)
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

    /// A deterministic pseudo-random input in [-1, 1).
    fn noise(seed: &mut u32) -> f32 {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (*seed >> 8) as f32 / (1u32 << 23) as f32 - 1.0
    }

    /// Runs `bank` on `objects` objects over three blocks of different
    /// lengths, once per object through the scalar `process_block` and once
    /// through `process_block_lanes` (via `lanes`), and requires every band
    /// sample and every state to match bit for bit.
    fn lanes_match_scalar(
        bank: &LR4CrossoverBank,
        objects: usize,
        lanes: impl Fn(&[[f32; LANES]], &mut LaneStates, [bool; LANES], &mut [[Vec<f32>; 8]]),
    ) {
        let count = bank.state_count();
        let mut scalar_states = vec![vec![BiquadState::default(); count]; objects];
        let mut lane_object_states = scalar_states.clone();
        let mut lane_states = LaneStates::new(count);
        let mut scalar_out: [Vec<f32>; 8] = std::array::from_fn(|_| Vec::new());
        let mut lane_out: Vec<[Vec<f32>; 8]> = (0..LANES)
            .map(|_| std::array::from_fn(|_| Vec::new()))
            .collect();
        let mut seed = 0x5eed_u32;
        for len in [40usize, 1, 333] {
            let inputs: Vec<[f32; LANES]> = (0..len)
                .map(|_| {
                    std::array::from_fn(|lane| {
                        if lane < objects {
                            noise(&mut seed)
                        } else {
                            0.0
                        }
                    })
                })
                .collect();
            let mut active = [false; LANES];
            for lane in 0..LANES {
                if lane < objects {
                    active[lane] = true;
                    lane_states.load(lane, &lane_object_states[lane]);
                } else {
                    lane_states.clear(lane, count);
                }
            }
            lanes(&inputs, &mut lane_states, active, &mut lane_out);
            for lane in 0..objects {
                lane_states.store(lane, &mut lane_object_states[lane]);
                bank.process_block(len, &mut scalar_states[lane], &mut scalar_out, |s| {
                    inputs[s][lane]
                });
                for band in 0..bank.num_bands {
                    let (a, b) = (&scalar_out[band][..len], &lane_out[lane][band][..len]);
                    assert!(
                        a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits()),
                        "lane {lane} band {band} differs from the scalar filter (len {len})"
                    );
                }
                for (x, y) in scalar_states[lane].iter().zip(&lane_object_states[lane]) {
                    assert_eq!(x.delays().0.to_bits(), y.delays().0.to_bits());
                    assert_eq!(x.delays().1.to_bits(), y.delays().1.to_bits());
                }
            }
        }
    }

    /// #750: the lane filter is the scalar filter, bit for bit, for every
    /// lane count and band count, across blocks (states carried through
    /// load/store).
    #[test]
    fn lanes_are_bit_identical_to_the_scalar_filter() {
        for cutoffs in [
            &[120.0f32][..],
            &[80.0, 500.0],
            &[60.0, 250.0, 2000.0, 8000.0],
        ] {
            let bank = LR4CrossoverBank::new(cutoffs, 48_000);
            for objects in 1..=LANES {
                lanes_match_scalar(&bank, objects, |i, s, a, o| {
                    bank.process_block_lanes(i, s, a, o)
                });
                lanes_match_scalar(&bank, objects, |i, s, a, o| {
                    bank.process_block_lanes_body(i, s, a, o)
                });
            }
        }
    }

    /// Whatever the dispatcher picked above, the AVX2 build also matches.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn avx2_lanes_are_bit_identical_to_the_scalar_filter() {
        if !std::arch::is_x86_feature_detected!("avx2") {
            return;
        }
        let bank = LR4CrossoverBank::new(&[60.0, 250.0, 2000.0], 48_000);
        for objects in [1, 5, LANES] {
            // SAFETY: AVX2 detected just above.
            lanes_match_scalar(&bank, objects, |i, s, a, o| unsafe {
                bank.process_block_lanes_avx2(i, s, a, o)
            });
        }
    }

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
