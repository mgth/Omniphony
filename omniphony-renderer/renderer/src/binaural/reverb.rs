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
//! Topology: one mono input bus (per-channel sends, summed by the caller) →
//! pre-delay → 8 mutually-prime delay lines with one-pole HF damping in the
//! feedback path, mixed by a Householder matrix (O(N) per sample) → two
//! sign-pattern output taps → interaural-coherence shaping → L/R.
//!
//! Three properties keep the tail centred and natural (issue #145):
//! - The two tap sign vectors are zero-sum and mutually orthogonal, so the
//!   network's common mode (the all-ones Householder eigenvector, which the
//!   mixer preserves) reaches neither ear unevenly, and the two returns are
//!   decorrelated with equal broadband energy.
//! - The line lengths are slowly modulated (staggered sub-Hz LFOs, ~±0.05 %
//!   pitch). A sparse 8-line FDN has a comb-like per-tap response: a
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
//! In a real room the reverberant field level is roughly independent of
//! source distance. The direct object level is authored (Atmos) and never
//! 1/d-attenuated here, so instead the caller raises the per-source reverb
//! send with distance (near-field roll-in): the DRR falls with distance
//! without ever touching the direct object level.

use crate::crossover::filter::{
    BiquadCoeffs, BiquadState, biquad, butterworth2_hp, butterworth2_lp,
};

/// Number of delay lines.
const N: usize = 8;

/// Delay line lengths in samples at 48 kHz (mutually prime, ~21…62 ms),
/// scaled linearly for other rates.
const LENGTHS_48K: [usize; N] = [1031, 1327, 1523, 1801, 2053, 2311, 2617, 2903];

/// Output tap sign patterns for the two returns. Both rows are zero-sum
/// (rejects the FDN's persistent common mode — the all-ones vector is a
/// Householder eigenvector), mutually orthogonal (decorrelated returns), and
/// orthogonal to the alternating input-injection pattern. An earlier
/// `SIGNS_R` had sum +2 and dot(L,R) = −2, which leaked the common mode into
/// the right ear only (issue #145).
const SIGNS_L: [f32; N] = [1.0, -1.0, 1.0, 1.0, -1.0, 1.0, -1.0, -1.0];
const SIGNS_R: [f32; N] = [1.0, -1.0, -1.0, 1.0, 1.0, -1.0, -1.0, 1.0];

/// One-pole HF damping coefficient in the feedback path at 48 kHz (higher =
/// darker tail; HF decays faster than the broadband RT60, like physical
/// walls). Other rates raise the pole to the rate ratio so the damping
/// corner stays at the same frequency (≈8 kHz) instead of climbing with
/// the rate.
const DAMPING_48K: f32 = 0.35;

/// Peak delay-line modulation depth in samples at 48 kHz (scaled with the
/// engine rate). ±24 samples spreads each mode by ~±1–2 %, enough for a
/// sustained mid/high tone to average over the comb period; peak pitch
/// deviation is depth·2π·rate/sr ≈ 0.4 % (~7 cents) on the fastest line.
const MOD_DEPTH_48K: f32 = 24.0;

/// Per-line modulation rates in Hz, mutually detuned so no two lines
/// breathe in step.
const MOD_RATES_HZ: [f32; N] = [0.31, 0.41, 0.53, 0.67, 0.79, 0.97, 1.13, 1.31];

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
    /// Nominal (unmodulated) delay per line, in samples.
    base_len: [f32; N],
    /// Write position per line.
    pos: [usize; N],
    damp_state: [f32; N],
    /// Per-line feedback gain derived from the current RT60.
    fb_gain: [f32; N],
    mod_phase: [f32; N],
    /// Current (slewed) modulated delay per line, in samples.
    cur_delay: [f32; N],
    /// Interaural-coherence crossover: LR4 low-pass (shared mid path) and
    /// LR4 high-pass (per-return paths) share these per-section coefficients.
    xover_lp: BiquadCoeffs,
    xover_hp: BiquadCoeffs,
    /// States: [mid lp1, mid lp2, L hp1, L hp2, R hp1, R hp2].
    xover_state: [BiquadState; 6],
    predelay: Vec<f32>,
    pre_pos: usize,
    pre_len: usize,
    sample_rate: u32,
    rt60_cached: f32,
    /// `1 − damping` for this rate, applied per line per sample.
    damp_mix: f32,
}

impl Fdn {
    pub fn new(sample_rate: u32) -> Self {
        let scale = sample_rate as f32 / 48_000.0;
        let margin = (MOD_DEPTH_48K * scale).ceil() as usize + 2;
        let mut base_len = [0.0f32; N];
        let lines: Vec<Vec<f32>> = LENGTHS_48K
            .iter()
            .enumerate()
            .map(|(i, &l)| {
                let base = ((l as f32 * scale) as usize).max(16);
                base_len[i] = base as f32;
                vec![0.0f32; base + margin]
            })
            .collect();
        let mut mod_phase = [0.0f32; N];
        for (i, p) in mod_phase.iter_mut().enumerate() {
            *p = i as f32 * 2.4;
        }
        // 120 ms pre-delay capacity; the active length is set per block.
        let pre_cap = (sample_rate as usize * 120 / 1000).max(16);
        Self {
            lines,
            base_len,
            pos: [0; N],
            damp_state: [0.0; N],
            fb_gain: [0.5; N],
            mod_phase,
            cur_delay: base_len,
            xover_lp: butterworth2_lp(COHERENCE_XOVER_HZ, sample_rate),
            xover_hp: butterworth2_hp(COHERENCE_XOVER_HZ, sample_rate),
            xover_state: Default::default(),
            predelay: vec![0.0; pre_cap],
            pre_pos: 0,
            pre_len: 1,
            sample_rate,
            rt60_cached: 0.0,
            damp_mix: 1.0 - DAMPING_48K.powf(48_000.0 / sample_rate as f32),
        }
    }

    /// Per-block parameter update: RT60 (s) → per-line feedback gains, and the
    /// active pre-delay length.
    pub fn set_params(&mut self, rt60_s: f32, predelay_ms: f32) {
        let rt60 = rt60_s.clamp(0.1, 3.0);
        if (rt60 - self.rt60_cached).abs() > 1e-6 {
            self.rt60_cached = rt60;
            for i in 0..N {
                // g = 10^(-3 * delay / (rt60 * sr)) → -60 dB after rt60 seconds.
                let exp = -3.0 * self.base_len[i] / (rt60 * self.sample_rate as f32);
                self.fb_gain[i] = 10.0f32.powf(exp);
            }
        }
        let len = (predelay_ms.clamp(0.0, 100.0) * self.sample_rate as f32 / 1000.0) as usize;
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
        self.predelay.fill(0.0);
        self.xover_state = Default::default();
        self.cur_delay = self.base_len;
    }

    /// Process one block: read `bus` (mono send sum, one sample per frame) and
    /// ADD the stereo return × `level` into `out` (interleaved L/R).
    pub fn process_block(&mut self, bus: &[f32], level: f32, out: &mut [f32]) {
        debug_assert!(out.len() >= bus.len() * 2);
        // Normalise the output taps (N lines, ±1 signs) and fold the level in.
        let out_gain = level / (N as f32).sqrt();
        let sr = self.sample_rate as f32;
        let depth = MOD_DEPTH_48K * sr / 48_000.0;
        let (xover_lp, xover_hp) = (self.xover_lp, self.xover_hp);
        let damp_mix = self.damp_mix;
        let pre_cap = self.predelay.len();

        let mut offset = 0usize;
        for chunk in bus.chunks(MOD_UPDATE) {
            // Advance the line LFOs to the end of this chunk and slew each
            // delay linearly toward its new target across the chunk.
            let mut d_step = [0.0f32; N];
            for i in 0..N {
                self.mod_phase[i] = (self.mod_phase[i]
                    + std::f32::consts::TAU * MOD_RATES_HZ[i] * chunk.len() as f32 / sr)
                    % std::f32::consts::TAU;
                let target = self.base_len[i] + depth * self.mod_phase[i].sin();
                d_step[i] = (target - self.cur_delay[i]) / chunk.len() as f32;
            }

            for (s, &input) in chunk.iter().enumerate() {
                // Pre-delay (integer, fixed per block). `pre_len < pre_cap`,
                // so the read is at most one lap behind: a conditional add
                // wraps it, no division per sample.
                let read = if self.pre_pos >= self.pre_len {
                    self.pre_pos - self.pre_len
                } else {
                    self.pre_pos + pre_cap - self.pre_len
                };
                let x = self.predelay[read];
                self.predelay[self.pre_pos] = input;
                self.pre_pos += 1;
                if self.pre_pos == pre_cap {
                    self.pre_pos = 0;
                }

                // Read all line outputs at their (modulated) fractional delay.
                // The modulated delay stays under `base + depth < cap`
                // (the line has `margin` slots past its base length), so
                // the same one-lap wrap applies.
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
                    l += SIGNS_L[i] * o[i];
                    r += SIGNS_R[i] * o[i];
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
                // plus the (sign-alternated) input injection.
                let k = 2.0 / N as f32 * sum;
                for i in 0..N {
                    let fb = o[i] - k;
                    // One-pole low-pass in the loop: HF dies faster than RT60.
                    self.damp_state[i] += (fb - self.damp_state[i]) * damp_mix;
                    let inject = if i % 2 == 0 { x } else { -x };
                    let cap = self.lines[i].len();
                    self.lines[i][self.pos[i]] = self.damp_state[i] * self.fb_gain[i] + inject;
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

    fn render_stereo(bus: &[f32], rt60: f32) -> (Vec<f32>, Vec<f32>) {
        let mut fdn = Fdn::new(48_000);
        fdn.set_params(rt60, 5.0);
        let mut out = vec![0.0f32; bus.len() * 2];
        fdn.process_block(bus, 1.0, &mut out);
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
        // Both rows must also ignore the alternating input-injection pattern,
        // so neither ear favours the freshly injected signal.
        for (name, row) in [("SIGNS_L", &SIGNS_L), ("SIGNS_R", &SIGNS_R)] {
            let d: f32 = row
                .iter()
                .enumerate()
                .map(|(i, v)| if i % 2 == 0 { *v } else { -*v })
                .sum();
            assert_eq!(d, 0.0, "{name} must be orthogonal to the injection");
        }
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
        fdn.set_params(0.4, 10.0);
        let n = 48_000; // 1 s
        let mut bus = vec![0.0f32; n];
        bus[0] = 1.0;
        let mut out = vec![0.0f32; n * 2];
        fdn.process_block(&bus, 1.0, &mut out);

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
            fdn.set_params(rt60, 5.0);
            let n = 48_000;
            let mut bus = vec![0.0f32; n];
            bus[0] = 1.0;
            let mut out = vec![0.0f32; n * 2];
            fdn.process_block(&bus, 1.0, &mut out);
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
