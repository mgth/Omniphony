//! Late-reverberation tail for the binaural stage: a small stereo FDN
//! (feedback delay network).
//!
//! Purpose: the direct-to-reverberant ratio is the dominant distance cue past
//! ~1 m, and six discrete first-order reflections cannot provide the dense
//! decaying tail a real room has. This FDN models the LISTENING room — a
//! small, fairly dry space, constant across content (like the room around a
//! loudspeaker setup) — NOT the acoustics of the scene, which are already in
//! the mix and pass through untouched.
//!
//! Topology: two input buses (per-channel sends, panned by each source's
//! lateral position and summed by the caller) → pre-delay → 16 mutually-prime
//! delay lines with one-pole HF damping in the feedback path, mixed by a
//! Householder matrix (O(N) per sample) → two sign-pattern output taps →
//! interaural-coherence shaping → L/R.
//!
//! The left bus is injected into the even lines, the right bus into the odd
//! lines (each with its ear's tap signs, zero-sum, so neither excites the
//! network's persistent common mode). Laterality is then a matter of
//! **weights**, not signs — sign patterns alone cannot lateralise a
//! broadband signal, since delayed copies with opposite signs do not
//! cancel: each ear reads the lines of its own side at full weight and the
//! other side's at [`SIDE_WEIGHT`]. On the first lap a source on the right
//! is therefore heard mostly on the right; the Householder mixing then
//! spreads it over every line and the tail goes diffuse and balanced. The
//! weighted rows keep the properties issue #145 relies on: zero-sum, equal
//! norms, and orthogonal to each other. (A mono bus used to be injected with
//! alternating signs: the tail was identical for a source on the left and
//! one on the right.)
//!
//! Three properties keep the tail centred and natural (issue #145):
//! - The two tap sign vectors are zero-sum and mutually orthogonal, so the
//!   network's common mode (the all-ones Householder eigenvector, which the
//!   mixer preserves) reaches neither ear unevenly, and the two returns are
//!   decorrelated with equal broadband energy.
//! - The line lengths are slowly modulated (staggered sub-Hz LFOs, ~±0.05 %
//!   pitch). A sparse FDN has a comb-like per-tap response: a
//!   sustained tone parks on one tap's peak and the other's notch and reads
//!   as a hard L/R bias (measured −7.7 dB on a 440+660 Hz signal).
//!   Modulation sweeps each mode so tones time-average the combs; it also
//!   densifies the tail (less metallic ringing).
//! - Below `COHERENCE_XOVER_HZ` both ears receive the shared mid of the two
//!   returns: a physical diffuse field is interaurally coherent at low
//!   frequency (IC ≈ sinc(2πfd/c) ≈ 0.85 at 150 Hz for a human head), and
//!   modulation is too slow relative to the comb spacing down there to
//!   average tones out. Above the crossover the decorrelated returns pass
//!   as-is, preserving envelopment.
//!
//! Two more handles shape the tail the way a room's would (S2-15):
//! - **size** scales every line length together (0.5…2 × the nominal
//!   21…62 ms set, allocated for the maximum): shorter lines make a small,
//!   dense room, longer ones a sparser, larger one. The feedback gains are
//!   recomputed with the lengths so the RT60 stays what was asked, and the
//!   lines stretch at no more than [`SIZE_SLEW`] samples per sample — a
//!   size change is a second-long ±2 % pitch drift, not a splice.
//! - **per-band decay**: the loop gain of each line is a broadband value
//!   plus two first-order shelves, one below [`LOW_BAND_HZ`] and one above
//!   [`HIGH_BAND_HZ`], whose gains are the RT60 ratios of those bands
//!   (`rt60_low_ratio`, `rt60_high_ratio`, relative to the broadband
//!   RT60). A ratio of 1 leaves a shelf at exactly zero: the default tail
//!   is arithmetically the one before the shelves existed. The fixed
//!   one-pole [`DAMPING_48K`] stays underneath as the wall's own
//!   high-frequency loss; the high ratio acts on top of it.
//!
//! In a real room the reverberant field level is roughly independent of
//! source distance. The direct object level is authored (Atmos) and never
//! 1/d-attenuated here, so instead the caller raises the per-source reverb
//! send with distance (near-field roll-in): the DRR falls with distance
//! without ever touching the direct object level.

use crate::crossover::filter::{
    BiquadCoeffs, BiquadState, biquad, butterworth2_hp, butterworth2_lp,
};
use crate::live_params::BinauralReverb;

/// Number of delay lines. Sixteen: dense enough that a sustained tone no
/// longer parks on one tap's comb peak and the other's notch (issue #145),
/// which eight lines only managed under a deep, audible modulation.
const N: usize = 16;

/// Delay line lengths in samples at 48 kHz (mutually prime, ~21…62 ms),
/// scaled linearly for other rates.
const LENGTHS_48K: [usize; N] = [
    1031, 1129, 1201, 1327, 1409, 1523, 1613, 1801, 1907, 2053, 2203, 2311, 2467, 2617, 2749, 2903,
];

/// Range of the `size` scale on the line lengths (1 = the nominal set
/// above). The lines are allocated for [`SIZE_MAX`].
pub const SIZE_MIN: f32 = 0.5;
pub const SIZE_MAX: f32 = 2.0;

/// Range of the per-band decay ratios: the RT60 of the band over the
/// broadband RT60 (1 = the same decay everywhere).
pub const RT60_RATIO_MIN: f32 = 0.25;
pub const RT60_RATIO_MAX: f32 = 4.0;

/// Corner (Hz) of the low-band shelf in the loop: below it the feedback
/// gain settles on the low band's value.
const LOW_BAND_HZ: f32 = 250.0;
/// Corner (Hz) of the high-band shelf: above it the gain settles on the
/// high band's value.
const HIGH_BAND_HZ: f32 = 4_000.0;

/// Fastest the line lengths follow a size change, in samples per sample:
/// ±2 % of pitch while the lines stretch, the whole range in about a
/// second. The modulation LFOs move far slower and never hit it.
const SIZE_SLEW: f32 = 0.02;

/// Output tap sign patterns for the two returns. Both rows are zero-sum
/// (rejects the FDN's persistent common mode — the all-ones vector is a
/// Householder eigenvector), mutually orthogonal (decorrelated returns), and
/// orthogonal to the alternating input-injection pattern. An earlier
/// `SIGNS_R` had sum +2 and dot(L,R) = −2, which leaked the common mode into
/// the right ear only (issue #145).
/// Both rows are the eight-line patterns of #145 laid over each side of the
/// network — line `2k` and line `2k+1` share pattern index `k` — so every
/// property holds on the even lines and on the odd lines separately, which
/// is what the side weights and the per-side injection need.
const SIGNS_L: [f32; N] = [
    1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0, 1.0, -1.0, -1.0, 1.0, 1.0, -1.0, -1.0, -1.0, -1.0,
];
const SIGNS_R: [f32; N] = [
    1.0, 1.0, -1.0, -1.0, -1.0, -1.0, 1.0, 1.0, 1.0, 1.0, -1.0, -1.0, -1.0, -1.0, 1.0, 1.0,
];

/// Weight with which an ear reads the lines of the *other* side (its own
/// side's lines are read at 1). 0.35 puts a first-lap source ≈ 9 dB on its
/// own side; 1 would be the old, side-blind tail.
const SIDE_WEIGHT: f32 = 0.35;

/// Output weight of line `i` for the left ear (even lines are the left
/// side) and for the right ear (odd lines).
#[inline]
fn side_weights(i: usize) -> (f32, f32) {
    if i % 2 == 0 {
        (1.0, SIDE_WEIGHT)
    } else {
        (SIDE_WEIGHT, 1.0)
    }
}

/// One-pole HF damping coefficient in the feedback path at 48 kHz (higher =
/// darker tail; HF decays faster than the broadband RT60, like physical
/// walls). Other rates raise the pole to the rate ratio so the damping
/// corner stays at the same frequency (≈8 kHz) instead of climbing with
/// the rate.
const DAMPING_48K: f32 = 0.35;

/// Peak delay-line modulation depth in samples at 48 kHz (scaled with the
/// engine rate). With sixteen lines the combs are dense enough that the
/// #145 tone signal sits at +0.5 dB of L/R bias with no modulation at all,
/// and — measured by the `tonal_bias_sweep_over_modulation_depth`
/// instrumentation — the bias *grows* with depth once the side weights read
/// the two halves of the network differently. What is left is the
/// classic reason to modulate at all: breaking the periodicity of the
/// tail. ±3 samples does that at a peak pitch deviation of
/// depth·2π·rate/sr ≈ 0.04 % (under a cent) on the fastest line; the ±24
/// the eight-line network needed for balance (~7 cents) were audible as
/// chorus on held notes.
const MOD_DEPTH_48K: f32 = 3.0;

/// Largest depth the lines are allocated for (samples at 48 kHz): the
/// production depth and anything the sweep instrumentation tries must fit.
const MOD_DEPTH_CAPACITY_48K: f32 = 32.0;

/// Per-line modulation rates in Hz, mutually detuned so no two lines
/// breathe in step.
const MOD_RATES_HZ: [f32; N] = [
    0.23, 0.29, 0.31, 0.37, 0.41, 0.43, 0.47, 0.53, 0.59, 0.61, 0.67, 0.71, 0.79, 0.83, 0.97, 1.13,
];

/// Modulation targets are recomputed every this many samples and slewed
/// linearly in between, making the tail independent of the caller's block
/// size (a 128-sample update at the fastest LFO moves the delay by well
/// under a tenth of a sample).
const MOD_UPDATE: usize = 128;

/// Crossover of the interaural-coherence shaping (Hz): shared mid below,
/// decorrelated returns above (LR4 slopes, in phase — see module doc).
const COHERENCE_XOVER_HZ: f32 = 300.0;

pub struct Fdn {
    lines: Vec<Vec<f32>>,
    /// Nominal delay per line at size 1, in samples.
    base_len: [f32; N],
    /// Unmodulated delay per line at the current size, in samples: what
    /// the modulation breathes around and the slew heads for.
    size_len: [f32; N],
    /// Write position per line.
    pos: [usize; N],
    damp_state: [f32; N],
    /// Per-line broadband feedback gain derived from the current RT60 and
    /// line length.
    fb_gain: [f32; N],
    /// What the low band's gain adds to the broadband one (zero at a ratio
    /// of 1), and the high band's.
    fb_low: [f32; N],
    fb_high: [f32; N],
    /// Integrator states of the two shelves' one-poles (trapezoidal
    /// form: its low-pass and high-pass are exact complements, zero at
    /// Nyquist and at DC respectively, which the `exp` form is not).
    band_lo: [f32; N],
    band_hi: [f32; N],
    /// `g/(1+g)`, `g = tan(π·fc/fs)`, of the two one-poles at this rate.
    k_lo: f32,
    k_hi: f32,
    mod_phase: [f32; N],
    /// Current (slewed) modulated delay per line, in samples.
    cur_delay: [f32; N],
    /// Interaural-coherence crossover: LR4 low-pass (shared mid path) and
    /// LR4 high-pass (per-return paths) share these per-section coefficients.
    xover_lp: BiquadCoeffs,
    xover_hp: BiquadCoeffs,
    /// States: [mid lp1, mid lp2, L hp1, L hp2, R hp1, R hp2].
    xover_state: [BiquadState; 6],
    /// Pre-delay ring, one `[left, right]` pair per sample.
    predelay: Vec<[f32; 2]>,
    pre_pos: usize,
    pre_len: usize,
    sample_rate: u32,
    /// `(rt60, size, low ratio, high ratio)` the gains were computed for.
    cached: (f32, f32, f32, f32),
    /// Whether a block has been processed since the last clear: until then
    /// a size change snaps the lines to their length instead of slewing.
    primed: bool,
    /// `1 − damping` for this rate, applied per line per sample.
    damp_mix: f32,
    /// Peak line modulation depth in samples at this rate.
    mod_depth: f32,
}

impl Fdn {
    pub fn new(sample_rate: u32) -> Self {
        let scale = sample_rate as f32 / 48_000.0;
        let margin = (MOD_DEPTH_CAPACITY_48K * scale).ceil() as usize + 2;
        let mut base_len = [0.0f32; N];
        let lines: Vec<Vec<f32>> = LENGTHS_48K
            .iter()
            .enumerate()
            .map(|(i, &l)| {
                let base = ((l as f32 * scale) as usize).max(16);
                base_len[i] = base as f32;
                // Room for the longest size the line may be asked for.
                let cap = (base as f32 * SIZE_MAX).ceil() as usize + margin;
                vec![0.0f32; cap]
            })
            .collect();
        let mut mod_phase = [0.0f32; N];
        for (i, p) in mod_phase.iter_mut().enumerate() {
            *p = i as f32 * 2.4;
        }
        // 120 ms pre-delay capacity; the active length is set per block.
        let pre_cap = (sample_rate as usize * 120 / 1000).max(16);
        let one_pole = |hz: f32| {
            let g = (std::f32::consts::PI * hz / sample_rate as f32).tan();
            g / (1.0 + g)
        };
        Self {
            lines,
            base_len,
            size_len: base_len,
            pos: [0; N],
            damp_state: [0.0; N],
            fb_gain: [0.5; N],
            fb_low: [0.0; N],
            fb_high: [0.0; N],
            band_lo: [0.0; N],
            band_hi: [0.0; N],
            k_lo: one_pole(LOW_BAND_HZ),
            k_hi: one_pole(HIGH_BAND_HZ),
            mod_phase,
            cur_delay: base_len,
            xover_lp: butterworth2_lp(COHERENCE_XOVER_HZ, sample_rate),
            xover_hp: butterworth2_hp(COHERENCE_XOVER_HZ, sample_rate),
            xover_state: Default::default(),
            predelay: vec![[0.0; 2]; pre_cap],
            pre_pos: 0,
            pre_len: 1,
            sample_rate,
            cached: (0.0, 0.0, 0.0, 0.0),
            primed: false,
            damp_mix: 1.0 - DAMPING_48K.powf(48_000.0 / sample_rate as f32),
            mod_depth: MOD_DEPTH_48K * scale,
        }
    }

    /// Override the modulation depth (samples at 48 kHz, scaled to the
    /// rate). Test instrumentation for the tonal-balance sweep; the
    /// production depth is [`MOD_DEPTH_48K`].
    #[cfg(test)]
    fn set_modulation_depth_48k(&mut self, depth: f32) {
        self.mod_depth = depth.min(MOD_DEPTH_CAPACITY_48K) * self.sample_rate as f32 / 48_000.0;
    }

    /// Per-block parameter update: RT60 (s), size and the two band ratios →
    /// per-line lengths and feedback gains (recomputed only when one of
    /// them changed), and the active pre-delay length.
    pub fn set_params(&mut self, p: &BinauralReverb) {
        let rt60 = p.rt60_s.clamp(0.1, 3.0);
        let size = p.size.clamp(SIZE_MIN, SIZE_MAX);
        let low = p.rt60_low_ratio.clamp(RT60_RATIO_MIN, RT60_RATIO_MAX);
        let high = p.rt60_high_ratio.clamp(RT60_RATIO_MIN, RT60_RATIO_MAX);
        if self.cached != (rt60, size, low, high) {
            self.cached = (rt60, size, low, high);
            let fs = self.sample_rate as f32;
            for i in 0..N {
                self.size_len[i] = self.base_len[i] * size;
                // g = 10^(-3 * delay / (rt60 * sr)) → -60 dB after rt60
                // seconds; each band's gain is the same law for its own
                // RT60, `rt60 · ratio`, stored as what it adds to the
                // broadband gain — exactly zero at a ratio of 1.
                let exp = -3.0 * self.size_len[i] / (rt60 * fs);
                let mid = 10.0f32.powf(exp);
                self.fb_gain[i] = mid;
                self.fb_low[i] = 10.0f32.powf(exp / low) - mid;
                self.fb_high[i] = 10.0f32.powf(exp / high) - mid;
            }
            if !self.primed {
                self.cur_delay = self.size_len;
            }
        }
        let len = (p.predelay_ms.clamp(0.0, 100.0) * self.sample_rate as f32 / 1000.0) as usize;
        self.pre_len = len.clamp(1, self.predelay.len() - 1);
    }

    /// Silence the network in place: lines, damping, pre-delay and crossover
    /// states zeroed, parameters kept. What the reverb used to get by being
    /// dropped and rebuilt when switched off — without the free and the
    /// allocation on the audio thread.
    pub fn clear(&mut self) {
        for line in &mut self.lines {
            line.fill(0.0);
        }
        self.damp_state = [0.0; N];
        self.band_lo = [0.0; N];
        self.band_hi = [0.0; N];
        self.predelay.fill([0.0; 2]);
        self.xover_state = Default::default();
        self.cur_delay = self.size_len;
        self.primed = false;
    }

    /// Process one block: read the two send buses (one sample per frame each,
    /// `bus_l` and `bus_r` the same length) and ADD the stereo return ×
    /// `level` into `out` (interleaved L/R).
    pub fn process_block(&mut self, bus_l: &[f32], bus_r: &[f32], level: f32, out: &mut [f32]) {
        debug_assert_eq!(bus_l.len(), bus_r.len());
        debug_assert!(out.len() >= bus_l.len() * 2);
        // Normalise the output taps (N/2 lines at weight 1, N/2 at
        // SIDE_WEIGHT, ±1 signs) and fold the level in.
        let out_gain = level / ((N as f32 / 2.0) * (1.0 + SIDE_WEIGHT * SIDE_WEIGHT)).sqrt();
        let sr = self.sample_rate as f32;
        let depth = self.mod_depth;
        let (xover_lp, xover_hp) = (self.xover_lp, self.xover_hp);
        let damp_mix = self.damp_mix;
        let (k_lo, k_hi) = (self.k_lo, self.k_hi);
        let pre_cap = self.predelay.len();
        self.primed = true;

        let mut offset = 0usize;
        for (chunk_l, chunk_r) in bus_l.chunks(MOD_UPDATE).zip(bus_r.chunks(MOD_UPDATE)) {
            let chunk = chunk_l;
            // Advance the line LFOs to the end of this chunk and slew each
            // delay linearly toward its new target across the chunk — at
            // most `SIZE_SLEW` per sample, which only a size change reaches.
            let mut d_step = [0.0f32; N];
            for i in 0..N {
                self.mod_phase[i] = (self.mod_phase[i]
                    + std::f32::consts::TAU * MOD_RATES_HZ[i] * chunk.len() as f32 / sr)
                    % std::f32::consts::TAU;
                let target = self.size_len[i] + depth * self.mod_phase[i].sin();
                d_step[i] = ((target - self.cur_delay[i]) / chunk.len() as f32)
                    .clamp(-SIZE_SLEW, SIZE_SLEW);
            }

            for (s, (&in_l, &in_r)) in chunk_l.iter().zip(chunk_r).enumerate() {
                // Pre-delay (integer, fixed per block). `pre_len < pre_cap`,
                // so the read is at most one lap behind: a conditional add
                // wraps it, no division per sample.
                let read = if self.pre_pos >= self.pre_len {
                    self.pre_pos - self.pre_len
                } else {
                    self.pre_pos + pre_cap - self.pre_len
                };
                let [xl, xr] = self.predelay[read];
                self.predelay[self.pre_pos] = [in_l, in_r];
                self.pre_pos += 1;
                if self.pre_pos == pre_cap {
                    self.pre_pos = 0;
                }

                // Read all line outputs at their (modulated) fractional delay.
                // The modulated delay stays under `base·SIZE_MAX + depth <
                // cap` (the line has `margin` slots past its longest
                // length), so the same one-lap wrap applies.
                let mut o = [0.0f32; N];
                let mut sum = 0.0f32;
                for i in 0..N {
                    self.cur_delay[i] += d_step[i];
                    let d = self.cur_delay[i];
                    let cap = self.lines[i].len();
                    let di = d as usize;
                    let frac = d - di as f32;
                    let r0 = if self.pos[i] >= di {
                        self.pos[i] - di
                    } else {
                        self.pos[i] + cap - di
                    };
                    let r1 = if r0 == 0 { cap - 1 } else { r0 - 1 };
                    let line = &self.lines[i];
                    o[i] = line[r0] * (1.0 - frac) + line[r1] * frac;
                    sum += o[i];
                }

                // Output taps, then interaural-coherence shaping: shared mid
                // below the crossover, decorrelated returns above.
                let mut l = 0.0f32;
                let mut r = 0.0f32;
                for i in 0..N {
                    let (wl, wr) = side_weights(i);
                    l += wl * SIGNS_L[i] * o[i];
                    r += wr * SIGNS_R[i] * o[i];
                }
                let mid = (l + r) * std::f32::consts::FRAC_1_SQRT_2;
                let lo = biquad(
                    biquad(mid, xover_lp, &mut self.xover_state[0]),
                    xover_lp,
                    &mut self.xover_state[1],
                );
                let hl = biquad(
                    biquad(l, xover_hp, &mut self.xover_state[2]),
                    xover_hp,
                    &mut self.xover_state[3],
                );
                let hr = biquad(
                    biquad(r, xover_hp, &mut self.xover_state[4]),
                    xover_hp,
                    &mut self.xover_state[5],
                );
                let oidx = (offset + s) * 2;
                out[oidx] += (lo + hl) * out_gain;
                out[oidx + 1] += (lo + hr) * out_gain;

                // Householder feedback: H·o = o − (2/N)·Σo, then damping + gain,
                // plus the injection: left bus on the even lines with the left
                // tap's signs, right bus on the odd lines with the right tap's
                // (see the module doc).
                let k = 2.0 / N as f32 * sum;
                for i in 0..N {
                    let fb = o[i] - k;
                    // One-pole low-pass in the loop: HF dies faster than RT60.
                    self.damp_state[i] += (fb - self.damp_state[i]) * damp_mix;
                    let x = self.damp_state[i];
                    // Per-band decay: the broadband gain, plus what the low
                    // band adds below its corner and the high band above
                    // its own (two one-pole shelves; both terms are exactly
                    // zero at a ratio of 1).
                    let v = (x - self.band_lo[i]) * k_lo;
                    let lp_lo = v + self.band_lo[i];
                    self.band_lo[i] = lp_lo + v;
                    let v = (x - self.band_hi[i]) * k_hi;
                    let lp_hi = v + self.band_hi[i];
                    self.band_hi[i] = lp_hi + v;
                    let y = x * self.fb_gain[i]
                        + lp_lo * self.fb_low[i]
                        + (x - lp_hi) * self.fb_high[i];
                    let inject = if i % 2 == 0 {
                        SIGNS_L[i] * xl
                    } else {
                        SIGNS_R[i] * xr
                    };
                    let cap = self.lines[i].len();
                    self.lines[i][self.pos[i]] = y + inject;
                    self.pos[i] += 1;
                    if self.pos[i] == cap {
                        self.pos[i] = 0;
                    }
                }
            }
            offset += chunk.len();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RT60 and pre-delay with the size and band ratios at their defaults.
    fn params(rt60_s: f32, predelay_ms: f32) -> BinauralReverb {
        BinauralReverb {
            rt60_s,
            predelay_ms,
            ..BinauralReverb::default()
        }
    }

    fn tail_energy(out: &[f32], from: usize, to: usize) -> f32 {
        out[from * 2..to * 2].iter().map(|x| x * x).sum()
    }

    /// Deterministic white noise in [-0.1, 0.1] (xorshift32).
    fn noise(len: usize) -> Vec<f32> {
        let mut state = 0x1234_5678u32;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state as f32 / u32::MAX as f32 - 0.5) * 0.2
            })
            .collect()
    }

    /// The same send on both buses (a source in the median plane).
    fn render_stereo(bus: &[f32], rt60: f32) -> (Vec<f32>, Vec<f32>) {
        let mut fdn = Fdn::new(48_000);
        fdn.set_params(&params(rt60, 5.0));
        let mut out = vec![0.0f32; bus.len() * 2];
        fdn.process_block(bus, bus, 1.0, &mut out);
        (
            out.iter().step_by(2).copied().collect(),
            out.iter().skip(1).step_by(2).copied().collect(),
        )
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    fn correlation(l: &[f32], r: &[f32]) -> f32 {
        let dot: f32 = l.iter().zip(r).map(|(a, b)| a * b).sum();
        let el: f32 = l.iter().map(|x| x * x).sum();
        let er: f32 = r.iter().map(|x| x * x).sum();
        dot / (el.sqrt() * er.sqrt()).max(1e-12)
    }

    /// Cascade of two one-pole low-passes at `fc` (crude band isolator for
    /// the coherence assertions).
    fn lowpassed(x: &[f32], fc: f32) -> Vec<f32> {
        let a = (-std::f32::consts::TAU * fc / 48_000.0).exp();
        let mut s1 = 0.0f32;
        let mut s2 = 0.0f32;
        x.iter()
            .map(|&v| {
                s1 += (v - s1) * (1.0 - a);
                s2 += (s1 - s2) * (1.0 - a);
                s2
            })
            .collect()
    }

    fn highpassed(x: &[f32], fc: f32) -> Vec<f32> {
        let lp = lowpassed(x, fc);
        x.iter().zip(&lp).map(|(v, l)| v - l).collect()
    }

    #[test]
    fn output_tap_sign_vectors_kill_common_mode_and_are_orthogonal() {
        let sum_l: f32 = SIGNS_L.iter().sum();
        let sum_r: f32 = SIGNS_R.iter().sum();
        assert_eq!(sum_l, 0.0, "SIGNS_L must be zero-sum (common-mode reject)");
        assert_eq!(sum_r, 0.0, "SIGNS_R must be zero-sum (common-mode reject)");
        let dot: f32 = SIGNS_L.iter().zip(&SIGNS_R).map(|(a, b)| a * b).sum();
        assert_eq!(dot, 0.0, "tap rows must be orthogonal (decorrelated ears)");
        // The injection patterns (left bus: SIGNS_L on the even lines; right
        // bus: SIGNS_R on the odd lines) must be zero-sum so they do not
        // excite the common mode.
        let u_l: Vec<f32> = (0..N)
            .map(|i| if i % 2 == 0 { SIGNS_L[i] } else { 0.0 })
            .collect();
        let u_r: Vec<f32> = (0..N)
            .map(|i| if i % 2 == 1 { SIGNS_R[i] } else { 0.0 })
            .collect();
        assert_eq!(
            u_l.iter().sum::<f32>(),
            0.0,
            "left injection must be zero-sum"
        );
        assert_eq!(
            u_r.iter().sum::<f32>(),
            0.0,
            "right injection must be zero-sum"
        );
        // The *weighted* output rows keep the three properties: zero-sum,
        // orthogonal, equal norms — the side weights must not undo #145.
        let row_l: Vec<f32> = (0..N).map(|i| side_weights(i).0 * SIGNS_L[i]).collect();
        let row_r: Vec<f32> = (0..N).map(|i| side_weights(i).1 * SIGNS_R[i]).collect();
        let dot = |a: &[f32], b: &[f32]| -> f32 { a.iter().zip(b).map(|(x, y)| x * y).sum() };
        assert!(
            row_l.iter().sum::<f32>().abs() < 1e-6,
            "weighted L row not zero-sum"
        );
        assert!(
            row_r.iter().sum::<f32>().abs() < 1e-6,
            "weighted R row not zero-sum"
        );
        assert!(
            dot(&row_l, &row_r).abs() < 1e-6,
            "weighted rows not orthogonal"
        );
        assert!(
            (dot(&row_l, &row_l) - dot(&row_r, &row_r)).abs() < 1e-6,
            "unequal norms"
        );
    }

    /// A send on the right bus alone starts its tail in the right ear and
    /// ends diffuse: early on the right dominates, late on the two ears are
    /// within a few dB.
    #[test]
    fn a_right_send_starts_right_and_ends_diffuse() {
        let n = 48_000; // 1 s
        let mut fdn = Fdn::new(48_000);
        fdn.set_params(&params(0.6, 5.0));
        let silent = vec![0.0f32; n];
        // A 100 ms burst, then decay: a continuous one-sided send would keep
        // feeding fresh first-lap energy on its side, which is right, but
        // is not what "diffuse late tail" is about.
        let mut bus_r = noise(n);
        bus_r[4_800..].fill(0.0);
        let mut out = vec![0.0f32; n * 2];
        fdn.process_block(&silent, &bus_r, 1.0, &mut out);
        let (l, r): (Vec<f32>, Vec<f32>) = (
            out.iter().step_by(2).copied().collect(),
            out.iter().skip(1).step_by(2).copied().collect(),
        );
        let db = |a: &[f32], b: &[f32]| 20.0 * (rms(a) / rms(b)).log10();
        // Early: the first 40 ms, within the first lap of every line.
        let early = db(&r[..1_920], &l[..1_920]);
        assert!(early > 6.0, "early tail not on the right: {early:+.1} dB");
        // Late: the steady state, once the mixing has spread both buses.
        let late = db(&r[24_000..], &l[24_000..]);
        assert!(late.abs() < 3.0, "late tail not diffuse: {late:+.1} dB");
    }

    /// The damping pole follows the rate: at 48 kHz it is the published
    /// coefficient exactly, and at 96 kHz the equivalent one-pole corner.
    #[test]
    fn damping_corner_is_rate_invariant() {
        let corner_hz = |sample_rate: u32| -> f32 {
            let fdn = Fdn::new(sample_rate);
            let a = 1.0 - fdn.damp_mix;
            -a.ln() * sample_rate as f32 / std::f32::consts::TAU
        };
        assert_eq!(Fdn::new(48_000).damp_mix, 1.0 - DAMPING_48K);
        let (a, b) = (corner_hz(48_000), corner_hz(96_000));
        assert!(
            (a - b).abs() < 1.0,
            "corner moved with the rate: {a} vs {b} Hz"
        );
    }

    #[test]
    fn impulse_produces_a_decaying_tail() {
        let mut fdn = Fdn::new(48_000);
        fdn.set_params(&params(0.4, 10.0));
        let n = 48_000; // 1 s
        let mut bus = vec![0.0f32; n];
        bus[0] = 1.0;
        let mut out = vec![0.0f32; n * 2];
        fdn.process_block(&bus, &bus, 1.0, &mut out);

        // Dense energy well past the early-reflection window…
        let mid = tail_energy(&out, 5_000, 15_000); // ~104…312 ms
        assert!(mid > 1e-6, "no late tail: {mid}");
        // …and decaying: the second half of the second must be much quieter.
        let late = tail_energy(&out, 30_000, 40_000);
        assert!(late < mid * 0.5, "tail not decaying: mid={mid} late={late}");
    }

    #[test]
    fn rt60_controls_decay_speed() {
        let render = |rt60: f32| -> (f32, f32) {
            let mut fdn = Fdn::new(48_000);
            fdn.set_params(&params(rt60, 5.0));
            let n = 48_000;
            let mut bus = vec![0.0f32; n];
            bus[0] = 1.0;
            let mut out = vec![0.0f32; n * 2];
            fdn.process_block(&bus, &bus, 1.0, &mut out);
            (
                tail_energy(&out, 4_000, 8_000),
                tail_energy(&out, 24_000, 28_000),
            )
        };
        let (short_early, short_late) = render(0.2);
        let (long_early, long_late) = render(1.5);
        // Both ring early; the long RT60 must retain much more late energy
        // relative to its early energy than the short one.
        let short_ratio = short_late / short_early.max(1e-12);
        let long_ratio = long_late / long_early.max(1e-12);
        assert!(
            long_ratio > short_ratio * 10.0,
            "rt60 had no effect: short={short_ratio} long={long_ratio}"
        );
    }

    /// Broadband L/R balance: with zero-sum orthogonal taps the two returns
    /// carry equal energy. A gain-normalised correlation test cannot see a
    /// level imbalance, so this asserts energy directly (issue #145).
    #[test]
    fn left_and_right_returns_have_equal_energy() {
        let bus = noise(48_000 * 5);
        let (l, r) = render_stereo(&bus, 0.3);
        let skip = 48_000;
        let bias_db = 20.0 * (rms(&l[skip..]) / rms(&r[skip..])).log10();
        assert!(
            bias_db.abs() < 0.5,
            "broadband L/R bias too large: {bias_db:+.2} dB"
        );
    }

    /// Instrumentation, not a gate: the tonal L/R bias of the #145 signal
    /// for a range of modulation depths, to choose the production depth
    /// from data rather than by feel. Run with `-- --ignored --nocapture`.
    /// Last sweep (16 lines, side weight 0.35): +0.47 dB at 0, +0.96 at ±4,
    /// +1.51 at ±6, +2.11 at ±8, +2.65 at ±10, +3.03 at ±12.
    #[test]
    #[ignore = "instrumentation: prints the bias per depth, asserts nothing"]
    fn tonal_bias_sweep_over_modulation_depth() {
        let n = 48_000 * 6;
        let bus: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / 48_000.0;
                0.5 * (std::f32::consts::TAU * 440.0 * t).sin()
                    + 0.44 * (std::f32::consts::TAU * 660.0 * t).sin()
            })
            .collect();
        for depth in [0.0f32, 4.0, 6.0, 8.0, 10.0, 12.0, 16.0, 20.0, 24.0, 32.0] {
            let mut fdn = Fdn::new(48_000);
            fdn.set_params(&params(0.3, 5.0));
            fdn.set_modulation_depth_48k(depth);
            let mut out = vec![0.0f32; n * 2];
            fdn.process_block(&bus, &bus, 1.0, &mut out);
            let l: Vec<f32> = out.iter().step_by(2).copied().collect();
            let r: Vec<f32> = out.iter().skip(1).step_by(2).copied().collect();
            let skip = 48_000;
            let bias_db = 20.0 * (rms(&l[skip..]) / rms(&r[skip..])).log10();
            println!("[measure] tonal bias at depth ±{depth:>4.1}: {bias_db:+.2} dB");
        }
    }

    /// The issue #145 signature: sustained tones (the demo asset's 440 Hz
    /// ring + 660 Hz heights) must not park on one tap's comb peak and the
    /// other's notch. Line modulation time-averages the combs; without it
    /// this measures ≈ −8 dB.
    #[test]
    fn sustained_tones_stay_balanced() {
        let n = 48_000 * 6;
        let bus: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / 48_000.0;
                0.5 * (std::f32::consts::TAU * 440.0 * t).sin()
                    + 0.44 * (std::f32::consts::TAU * 660.0 * t).sin()
            })
            .collect();
        let (l, r) = render_stereo(&bus, 0.3);
        let skip = 48_000;
        let bias_db = 20.0 * (rms(&l[skip..]) / rms(&r[skip..])).log10();
        assert!(
            bias_db.abs() < 1.5,
            "tonal L/R bias too large: {bias_db:+.2} dB"
        );
    }

    /// Frequency-dependent interaural coherence: a physical diffuse field is
    /// coherent at low frequency and decorrelated higher up. Below the
    /// shaping crossover both ears share the mid; above it the orthogonal
    /// taps keep the returns decorrelated.
    /// Impulse response of a fresh network at `size`, unmodulated, with
    /// the given band ratios: the interleaved output over one second.
    fn impulse_response(size: f32, rt60: f32, low: f32, high: f32) -> Vec<f32> {
        let mut fdn = Fdn::new(48_000);
        fdn.set_modulation_depth_48k(0.0);
        fdn.set_params(&BinauralReverb {
            rt60_s: rt60,
            predelay_ms: 5.0,
            size,
            rt60_low_ratio: low,
            rt60_high_ratio: high,
            ..BinauralReverb::default()
        });
        let n = 48_000;
        let mut bus = vec![0.0f32; n];
        bus[0] = 1.0;
        let mut out = vec![0.0f32; n * 2];
        fdn.process_block(&bus, &bus, 1.0, &mut out);
        out
    }

    /// The size scales the line lengths: the first sample out of a fresh
    /// network lands after the pre-delay plus the shortest line at that
    /// size, and the RT60 is unchanged — the gains follow the lengths.
    #[test]
    fn size_scales_the_lines_and_keeps_the_decay() {
        let first_out = |out: &[f32]| out.iter().position(|x| x.abs() > 1e-9).unwrap() / 2;
        let pre = 240; // 5 ms
        for size in [SIZE_MIN, 1.0, SIZE_MAX] {
            let out = impulse_response(size, 0.5, 1.0, 1.0);
            let expected = pre + (LENGTHS_48K[0] as f32 * size) as usize;
            let got = first_out(&out);
            assert!(
                got.abs_diff(expected) <= 2,
                "size {size}: first output at {got}, expected ≈ {expected}"
            );
        }
        // Decay slope between two late windows, once the tail is dense:
        // the same at every size, within a couple of dB.
        let slope_db = |out: &[f32]| {
            10.0 * (tail_energy(out, 36_000, 40_000) / tail_energy(out, 20_000, 24_000)).log10()
        };
        let (small, big) = (
            slope_db(&impulse_response(SIZE_MIN, 0.5, 1.0, 1.0)),
            slope_db(&impulse_response(SIZE_MAX, 0.5, 1.0, 1.0)),
        );
        assert!(
            (small - big).abs() < 3.0,
            "decay slope moved with the size: {small:.1} dB vs {big:.1} dB"
        );
    }

    /// A size change on a running network slews the lines at `SIZE_SLEW`
    /// instead of jumping — and gets there.
    #[test]
    fn size_changes_are_slewed() {
        let mut fdn = Fdn::new(48_000);
        fdn.set_modulation_depth_48k(0.0);
        fdn.set_params(&params(0.5, 5.0));
        let silence = vec![0.0f32; 128];
        let mut out = vec![0.0f32; 256];
        fdn.process_block(&silence, &silence, 1.0, &mut out);
        let before = fdn.cur_delay[0];
        fdn.set_params(&BinauralReverb {
            size: SIZE_MAX,
            ..params(0.5, 5.0)
        });
        assert_eq!(fdn.cur_delay[0], before, "a running network must not snap");
        fdn.process_block(&silence, &silence, 1.0, &mut out);
        let moved = fdn.cur_delay[0] - before;
        assert!(
            moved > 0.0 && moved <= 128.0 * SIZE_SLEW * 1.01,
            "moved {moved} samples in one block, expected ≤ {}",
            128.0 * SIZE_SLEW
        );
        for _ in 0..(48_000 * 2 / 128) {
            fdn.process_block(&silence, &silence, 1.0, &mut out);
        }
        assert!(
            (fdn.cur_delay[0] - fdn.size_len[0]).abs() < 1e-2,
            "never reached the new length: {} vs {}",
            fdn.cur_delay[0],
            fdn.size_len[0]
        );
    }

    /// Energy of `x[from..to]` between `lo_hz` and `hi_hz`, off a
    /// Hann-windowed FFT of the window: the one-pole band isolators above
    /// leak too much of the mid band to see a treble tail that has fallen
    /// 80 dB.
    fn band_energy(x: &[f32], from: usize, to: usize, lo_hz: f32, hi_hz: f32) -> f32 {
        use realfft::RealFftPlanner;
        let n = to - from;
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(n);
        let mut input: Vec<f32> = x[from..to]
            .iter()
            .enumerate()
            .map(|(i, &v)| v * (0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos()))
            .collect();
        let mut spec = fft.make_output_vec();
        fft.process(&mut input, &mut spec).expect("band FFT");
        let bin = |hz: f32| ((hz * n as f32 / 48_000.0).round() as usize).min(spec.len() - 1);
        spec[bin(lo_hz)..=bin(hi_hz)]
            .iter()
            .map(|c| c.norm_sqr())
            .sum()
    }

    /// The band ratios lengthen or shorten the decay of their own band:
    /// bass at ×4 rings far longer, treble at ×¼ dies far sooner, measured
    /// as the late/early energy ratio of the band (0.25–0.35 s over
    /// 0.1–0.2 s, close enough that the treble is still above the
    /// window's leakage floor).
    #[test]
    fn band_ratios_shape_the_decay_by_band() {
        let mono = |out: &[f32]| -> Vec<f32> { out.chunks_exact(2).map(|f| f[0] + f[1]).collect() };
        let ratio = |x: &[f32], lo: f32, hi: f32| {
            band_energy(x, 12_000, 16_800, lo, hi) / band_energy(x, 4_800, 9_600, lo, hi).max(1e-30)
        };
        let (bass, treble) = ((40.0, 150.0), (4_000.0, 8_000.0));
        let flat = mono(&impulse_response(1.0, 0.5, 1.0, 1.0));
        let bassy = mono(&impulse_response(1.0, 0.5, RT60_RATIO_MAX, 1.0));
        let (flat_low, bassy_low) = (ratio(&flat, bass.0, bass.1), ratio(&bassy, bass.0, bass.1));
        assert!(
            bassy_low > flat_low * 10.0,
            "bass ratio ×4 did not lengthen the bass: {bassy_low:.3e} vs {flat_low:.3e}"
        );
        let dull = mono(&impulse_response(1.0, 0.5, 1.0, RT60_RATIO_MIN));
        let (flat_high, dull_high) = (
            ratio(&flat, treble.0, treble.1),
            ratio(&dull, treble.0, treble.1),
        );
        assert!(
            dull_high < flat_high * 0.1,
            "treble ratio ×¼ did not shorten the treble: {dull_high:.3e} vs {flat_high:.3e}"
        );
        // Each ratio leaves the other band's decay alone (within 3 dB).
        let bassy_high = ratio(&bassy, treble.0, treble.1);
        let dull_low = ratio(&dull, bass.0, bass.1);
        let db = |a: f32, b: f32| 10.0 * (a / b).log10();
        assert!(
            db(bassy_high, flat_high).abs() < 3.0 && db(dull_low, flat_low).abs() < 3.0,
            "a band ratio leaked into the other band: treble {:+.1} dB, bass {:+.1} dB",
            db(bassy_high, flat_high),
            db(dull_low, flat_low)
        );
    }

    #[test]
    fn coherence_is_high_at_low_freq_and_low_at_high_freq() {
        let bus = noise(48_000 * 5);
        let (l, r) = render_stereo(&bus, 0.3);
        let skip = 48_000;
        let (l, r) = (&l[skip..], &r[skip..]);
        let lo = correlation(&lowpassed(l, 120.0), &lowpassed(r, 120.0));
        assert!(lo > 0.8, "low band should be near-coherent: {lo:+.3}");
        let hi = correlation(&highpassed(l, 1_000.0), &highpassed(r, 1_000.0));
        assert!(
            hi.abs() < 0.35,
            "high band should stay decorrelated: {hi:+.3}"
        );
    }
}
