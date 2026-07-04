//! Per-band ("spectral") phantom-source extraction — the frequency-domain
//! alternative to the broadband pairwise method in [`crate::phantom_extract`].
//!
//! The bed is analysed by a short-time Fourier transform and encoded per bin to
//! a virtual horizontal B-format (W + front/right dipoles) — the same model as
//! the DirAC object generator — but here the **direction** of the smoothed
//! active-intensity vector is kept, not just its magnitude: each bin gets a
//! direction of arrival (DOA) and a directness `1 − ψ`. The direct part of
//! every bin is routed to one of eight fixed azimuth *sector* objects (softly,
//! between the two adjacent sectors) and subtracted from all bed channels in
//! the frequency domain, so simultaneous sources at different positions **and
//! different frequencies** are extracted independently — the case the
//! broadband method collapses to a single averaged pan per channel pair.
//!
//! The residual bed is resynthesized through the same analysis/synthesis pair
//! (exact WOLA identity when nothing is extracted), and the channels that
//! bypass the STFT (LFE, unpositioned) ride a plain delay line, so the whole
//! bed and the sector objects stay mutually aligned at a fixed latency of one
//! FFT frame (1024 samples ≈ 21 ms at 48 kHz).
//!
//! Model limits (shared with the DirAC object generator, which uses the same
//! virtual B-format): the diffuseness estimate is exact only for
//! direction-balanced content. Independent channels whose direction vectors do
//! not sum to zero (e.g. front-heavy noise in L+R+C) read partly "direct"
//! toward the set's mean direction and get partially extracted there. And two
//! sources overlapping in the same time-frequency bin share one DOA (the usual
//! W-disjointness assumption) — spectrally disjoint sources are the case this
//! method wins on.
//!
//! Runs in the realtime audio thread: all allocation happens in
//! [`SpectralExtractor::prepare`]; [`SpectralExtractor::process`] is
//! allocation-free and never panics.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

use crate::object_gen::{
    PrepareCtx, SynthObjectSpec, channel_top_position, flush_denorm, input_has_back, one_pole_coeff,
};
use crate::stft::{DelayLine, OlaFifo, sine_window};

/// STFT size and 50% hop — matches the DirAC generator (≈21 ms frame at
/// 48 kHz), and so does the fixed one-frame latency.
const SPEC_FFT_SIZE: usize = 1024;
const SPEC_HOP: usize = SPEC_FFT_SIZE / 2;
/// Time constant (ms) of the per-bin one-pole smoothers (intensity + energy)
/// and of the per-sector position statistics. Long enough that neither the
/// extraction depth nor the object positions jitter on tonal material.
const SPEC_STAT_TC_MS: f32 = 80.0;
/// Relative regularisation on the energy denominator (fraction of the frame's
/// mean bin energy) plus a tiny absolute floor, so silence reads ψ→1 (nothing
/// to extract, no noise-floor objects).
const SPEC_REG: f32 = 1.0e-2;
const SPEC_FLOOR: f32 = 1.0e-12;
/// Largest input block the output FIFOs are sized for (same as the DirAC
/// generator's bound).
const SPEC_MAX_BLOCK: usize = 4096;
/// Number of fixed azimuth sectors (45° each, centred on the compass
/// directions starting at front). Fixed count = stable renderer channel slots.
const SPEC_SECTORS: usize = 8;
/// Clamp on the per-bin power-matching gain (guards the W≈0 phase-cancellation
/// corner, where the extraction depth is ~0 anyway).
const SPEC_NORM_CLAMP: f32 = 4.0;
/// A sector whose current frame carries less weighted energy than this skips
/// its inverse FFT (its output is the draining OLA tail only).
const SPEC_SILENT: f32 = 1.0e-12;
/// Below this smoothed sector energy the object parks at its sector centre.
const SPEC_POS_FLOOR: f32 = 1.0e-9;
const SPEC_GAIN_DB: i8 = 0;
/// Point-like, matching the broadband phantoms (localized sources, not the
/// large diffuse objects).
const SPEC_SIZE: [f32; 3] = [0.3, 0.3, 0.3];

/// Sector display names, in azimuth order from front, clockwise (+90° = right).
const SECTOR_NAMES: [&str; SPEC_SECTORS] = [
    "Direct_F",
    "Direct_FR",
    "Direct_R",
    "Direct_BR",
    "Direct_B",
    "Direct_BL",
    "Direct_L",
    "Direct_FL",
];

/// Project an azimuth onto the room-square perimeter (the bed channel
/// convention: corners at |x| = |y| = 1) with a fixed height.
fn sector_position(az: f32, lift: f64) -> [f64; 3] {
    let (s, c) = az.sin_cos();
    let m = s.abs().max(c.abs()).max(1.0e-6);
    [(s / m) as f64, (c / m) as f64, lift]
}

/// Frequency-domain phantom extractor state. Owned by
/// [`crate::phantom_extract::PhantomExtractStage`] when the `method` param
/// selects the per-band method.
pub(crate) struct SpectralExtractor {
    /// `(input channel index, cos(az) = front, sin(az) = right)` per
    /// positionable bed channel — the virtual B-format encode weights. NOTE the
    /// engine azimuth convention is `az = atan2(x, y)` with x = right,
    /// y = front (0 = front, +90° = right).
    enc: Vec<(usize, f32, f32)>,
    /// Channels that bypass the STFT (LFE, unpositioned): delayed to stay
    /// aligned with the transformed ones.
    other: Vec<(usize, DelayLine)>,
    fft_fwd: Arc<dyn RealToComplex<f32>>,
    fft_inv: Arc<dyn ComplexToReal<f32>>,
    /// Sine (√Hann) window, applied on both analysis and synthesis (their
    /// product is Hann → exact COLA at 50% hop).
    win: Vec<f32>,
    /// Ring frame buffer per positionable channel (length N, shared `widx`).
    frames: Vec<Vec<f32>>,
    widx: usize,
    /// Samples accumulated toward the next hop (0..H).
    fill: usize,
    real_buf: Vec<f32>,
    real_out: Vec<f32>,
    fft_scratch: Vec<Complex<f32>>,
    ifft_scratch: Vec<Complex<f32>>,
    /// Per-channel spectra; masked in place into the residual.
    spec_ch: Vec<Vec<Complex<f32>>>,
    /// Virtual B-format spectra (linear combinations of `spec_ch`).
    spec_w: Vec<Complex<f32>>,
    spec_f: Vec<Complex<f32>>,
    spec_r: Vec<Complex<f32>>,
    /// Per-sector object spectra, rebuilt every hop.
    spec_sec: Vec<Vec<Complex<f32>>>,
    /// Per-bin smoothed active intensity (front/right) and energy → ψ + DOA.
    i_front: Vec<f32>,
    i_right: Vec<f32>,
    e: Vec<f32>,
    /// Residual synthesis per positionable channel / per sector object.
    ch_ola: Vec<OlaFifo>,
    obj_ola: Vec<OlaFifo>,
    /// Smoothed per-sector energy + direction sums → dynamic object positions.
    sec_e: [f32; SPEC_SECTORS],
    sec_sx: [f32; SPEC_SECTORS],
    sec_cy: [f32; SPEC_SECTORS],
    /// Per-hop accumulators for the above (reset every frame).
    hop_e: [f32; SPEC_SECTORS],
    hop_sx: [f32; SPEC_SECTORS],
    hop_cy: [f32; SPEC_SECTORS],
    /// Sector centre azimuths (position fallback when a sector is silent).
    centers: [f32; SPEC_SECTORS],
    /// Per-hop one-pole coefficient.
    alpha: f32,
    /// Test hook: force `1 − ψ ≡ 1` to verify extraction/reconstruction gain
    /// in isolation from the diffuseness estimate.
    #[cfg(test)]
    pub(crate) force_direct: bool,
}

impl SpectralExtractor {
    /// Build the extractor for the current input, or `None` when fewer than
    /// two positionable channels are present (no direction to estimate).
    pub(crate) fn prepare(ctx: &PrepareCtx) -> Option<Self> {
        let use_7_1 = input_has_back(ctx.input_labels);
        let mut enc = Vec::new();
        let mut other = Vec::new();
        for (idx, &label) in ctx.input_labels.iter().enumerate() {
            match channel_top_position(label, use_7_1, ctx.surround_placement) {
                Some(pos) => {
                    let (x, y) = (pos[0] as f32, pos[1] as f32);
                    let r = (x * x + y * y).sqrt();
                    let (ca, sa) = if r > 1.0e-6 {
                        (y / r, x / r)
                    } else {
                        (0.0, 0.0)
                    };
                    enc.push((idx, ca, sa));
                }
                None => other.push((idx, DelayLine::new(SPEC_FFT_SIZE))),
            }
        }
        if enc.len() < 2 {
            return None;
        }

        let mut planner = RealFftPlanner::<f32>::new();
        let fft_fwd = planner.plan_fft_forward(SPEC_FFT_SIZE);
        let fft_inv = planner.plan_fft_inverse(SPEC_FFT_SIZE);
        let fs = ctx.sample_rate.max(1) as f32;
        let n = SPEC_FFT_SIZE;
        let nb = n / 2 + 1;
        let c_count = enc.len();

        let mut centers = [0.0f32; SPEC_SECTORS];
        for (k, az) in centers.iter_mut().enumerate() {
            *az = k as f32 * std::f32::consts::FRAC_PI_4;
        }

        Some(Self {
            enc,
            other,
            win: sine_window(n),
            frames: vec![vec![0.0; n]; c_count],
            widx: 0,
            fill: 0,
            real_buf: fft_fwd.make_input_vec(),
            real_out: fft_inv.make_output_vec(),
            fft_scratch: vec![Complex::new(0.0, 0.0); fft_fwd.get_scratch_len()],
            ifft_scratch: vec![Complex::new(0.0, 0.0); fft_inv.get_scratch_len()],
            spec_ch: vec![fft_fwd.make_output_vec(); c_count],
            spec_w: fft_fwd.make_output_vec(),
            spec_f: fft_fwd.make_output_vec(),
            spec_r: fft_fwd.make_output_vec(),
            spec_sec: vec![fft_fwd.make_output_vec(); SPEC_SECTORS],
            i_front: vec![0.0; nb],
            i_right: vec![0.0; nb],
            e: vec![0.0; nb],
            ch_ola: (0..c_count)
                .map(|_| OlaFifo::new(n, SPEC_HOP, SPEC_MAX_BLOCK))
                .collect(),
            obj_ola: (0..SPEC_SECTORS)
                .map(|_| OlaFifo::new(n, SPEC_HOP, SPEC_MAX_BLOCK))
                .collect(),
            sec_e: [0.0; SPEC_SECTORS],
            sec_sx: [0.0; SPEC_SECTORS],
            sec_cy: [0.0; SPEC_SECTORS],
            hop_e: [0.0; SPEC_SECTORS],
            hop_sx: [0.0; SPEC_SECTORS],
            hop_cy: [0.0; SPEC_SECTORS],
            centers,
            // The smoothers step once per hop, not per sample.
            alpha: one_pole_coeff(SPEC_STAT_TC_MS, fs / SPEC_HOP as f32),
            fft_fwd,
            fft_inv,
            #[cfg(test)]
            force_direct: false,
        })
    }

    /// The eight sector objects, at their static sector-centre positions.
    pub(crate) fn specs(&self, lift: f64) -> Vec<SynthObjectSpec> {
        SECTOR_NAMES
            .iter()
            .zip(self.centers.iter())
            .map(|(name, &az)| SynthObjectSpec {
                name: name.to_string(),
                position: sector_position(az, lift),
                gain_db: SPEC_GAIN_DB,
                size: SPEC_SIZE,
            })
            .collect()
    }

    /// Refresh the dynamic sector positions from the smoothed energy-weighted
    /// mean DOA; a silent sector parks at its centre. Cheap, called per frame.
    pub(crate) fn refresh_positions(&self, lift: f64, specs: &mut [SynthObjectSpec]) {
        for (k, spec) in specs.iter_mut().enumerate().take(SPEC_SECTORS) {
            let az = if self.sec_e[k] > SPEC_POS_FLOOR {
                self.sec_sx[k].atan2(self.sec_cy[k])
            } else {
                self.centers[k]
            };
            spec.position = sector_position(az, lift);
        }
    }

    /// Run the extraction on one block: mutates `bed` in place (all channels
    /// come out delayed by one FFT frame, positionable ones with the direct
    /// part removed) and writes the sector object audio into `planar`.
    pub(crate) fn process(
        &mut self,
        bed: &mut [f32],
        c: usize,
        n: usize,
        strength: f32,
        planar: &mut [Vec<f32>],
    ) {
        for s in 0..n {
            let base = s * c;
            for (k, &(ch, _, _)) in self.enc.iter().enumerate() {
                self.frames[k][self.widx] = if ch < c { bed[base + ch] } else { 0.0 };
            }
            for (ch, dl) in self.other.iter_mut() {
                if *ch < c {
                    bed[base + *ch] = dl.push_pop(bed[base + *ch]);
                }
            }
            self.widx = (self.widx + 1) % SPEC_FFT_SIZE;
            self.fill += 1;
            if self.fill >= SPEC_HOP {
                self.fill = 0;
                self.run_frame(strength);
            }
            for (k, ola) in self.ch_ola.iter_mut().enumerate() {
                let ch = self.enc[k].0;
                if ch < c {
                    bed[base + ch] = ola.pop();
                }
            }
            for (j, buf) in planar.iter_mut().enumerate().take(SPEC_SECTORS) {
                buf[s] = self.obj_ola[j].pop();
            }
        }
    }

    /// One STFT frame: analyse every positionable channel, estimate per-bin
    /// DOA + directness from the virtual B-format, split each bin between the
    /// residual bed and the sector objects, and resynthesize everything.
    fn run_frame(&mut self, strength: f32) {
        let n = SPEC_FFT_SIZE;
        let nb = self.spec_w.len();

        // Analyse each channel (oldest→newest from the shared ring).
        for k in 0..self.enc.len() {
            for i in 0..n {
                self.real_buf[i] = self.win[i] * self.frames[k][(self.widx + i) % n];
            }
            let _ = self.fft_fwd.process_with_scratch(
                &mut self.real_buf,
                &mut self.spec_ch[k],
                &mut self.fft_scratch,
            );
        }

        // Virtual B-format per bin — complex linear combinations of the channel
        // spectra (FFT linearity: no extra transforms). Relative regularisation
        // from the frame's broadband pressure energy.
        let mut e_sum = 0.0f32;
        for b in 0..nb {
            let mut w = Complex::new(0.0f32, 0.0);
            let mut f = Complex::new(0.0f32, 0.0);
            let mut r = Complex::new(0.0f32, 0.0);
            for (k, &(_, ca, sa)) in self.enc.iter().enumerate() {
                let x = self.spec_ch[k][b];
                w += x;
                f += x * ca;
                r += x * sa;
            }
            self.spec_w[b] = w;
            self.spec_f[b] = f;
            self.spec_r[b] = r;
            e_sum += w.norm_sqr();
        }
        let reg = SPEC_REG * (e_sum / nb.max(1) as f32) + SPEC_FLOOR;

        for sec in self.spec_sec.iter_mut() {
            for v in sec.iter_mut() {
                *v = Complex::new(0.0, 0.0);
            }
        }
        self.hop_e = [0.0; SPEC_SECTORS];
        self.hop_sx = [0.0; SPEC_SECTORS];
        self.hop_cy = [0.0; SPEC_SECTORS];

        let alpha = self.alpha;
        #[cfg(test)]
        let force = self.force_direct;
        #[cfg(not(test))]
        let force = false;

        // DC and Nyquist are excluded: they carry no direction and must stay
        // purely real for the inverse real FFT — they simply remain in the bed.
        for b in 1..nb.saturating_sub(1) {
            let w = self.spec_w[b];
            let f = self.spec_f[b];
            let r = self.spec_r[b];
            // Active intensity components Re(conj(W)·F), Re(conj(W)·R) and
            // pressure energy |W|², smoothed per bin (ψ and the DOA are ratios
            // of expectations).
            let i_f = w.re * f.re + w.im * f.im;
            let i_r = w.re * r.re + w.im * r.im;
            let ew = w.norm_sqr();
            self.i_front[b] = flush_denorm(self.i_front[b] + alpha * (i_f - self.i_front[b]));
            self.i_right[b] = flush_denorm(self.i_right[b] + alpha * (i_r - self.i_right[b]));
            self.e[b] = flush_denorm(self.e[b] + alpha * (ew - self.e[b]));

            // Directness = 1 − ψ = ‖I‖ / E. `d ≤ strength` doubles as the
            // over-subtraction floor: the residual gain never drops below
            // 1 − strength.
            let imag =
                (self.i_front[b] * self.i_front[b] + self.i_right[b] * self.i_right[b]).sqrt();
            let direct = if force {
                1.0
            } else {
                (imag / (self.e[b] + reg)).clamp(0.0, 1.0)
            };
            let d = strength * direct;
            if d <= 0.0 {
                continue;
            }

            // DOA from the smoothed intensity, softly assigned to the two
            // adjacent sectors (constant-sum linear weights: no energy pumping,
            // no hard switching between sectors).
            let az = self.i_right[b].atan2(self.i_front[b]);
            let t = az * (4.0 / std::f32::consts::PI); // 45° sectors
            let tf = t.floor();
            let frac = t - tf;
            let k0 = (tf as i32).rem_euclid(SPEC_SECTORS as i32) as usize;
            let k1 = (k0 + 1) % SPEC_SECTORS;

            // Power-match the extracted object to what leaves the bed: the
            // virtual W amplitude-sums correlated channels, so rescale to the
            // channels' summed power (the broadband extractor's `gnorm`
            // convention).
            let mut ch_pow = 0.0f32;
            for k in 0..self.enc.len() {
                ch_pow += self.spec_ch[k][b].norm_sqr();
            }
            let norm = (ch_pow / (ew + SPEC_FLOOR)).sqrt().min(SPEC_NORM_CLAMP);
            let ext = w * (d * norm);
            self.spec_sec[k0][b] += ext * (1.0 - frac);
            self.spec_sec[k1][b] += ext * frac;

            let res = 1.0 - d;
            for k in 0..self.enc.len() {
                self.spec_ch[k][b] = self.spec_ch[k][b] * res;
            }

            // Position statistics (instantaneous, smoothed per sector below).
            let (saz, caz) = az.sin_cos();
            let p = d * ew;
            self.hop_e[k0] += (1.0 - frac) * p;
            self.hop_sx[k0] += (1.0 - frac) * p * saz;
            self.hop_cy[k0] += (1.0 - frac) * p * caz;
            self.hop_e[k1] += frac * p;
            self.hop_sx[k1] += frac * p * saz;
            self.hop_cy[k1] += frac * p * caz;
        }

        for k in 0..SPEC_SECTORS {
            self.sec_e[k] = flush_denorm(self.sec_e[k] + alpha * (self.hop_e[k] - self.sec_e[k]));
            self.sec_sx[k] =
                flush_denorm(self.sec_sx[k] + alpha * (self.hop_sx[k] - self.sec_sx[k]));
            self.sec_cy[k] =
                flush_denorm(self.sec_cy[k] + alpha * (self.hop_cy[k] - self.sec_cy[k]));
        }

        // WOLA scale: only the unnormalised-iFFT factor — the sine·sine window
        // pair already overlap-adds to exactly 1.
        let scale = 1.0 / n as f32;
        for k in 0..self.enc.len() {
            let _ = self.fft_inv.process_with_scratch(
                &mut self.spec_ch[k],
                &mut self.real_out,
                &mut self.ifft_scratch,
            );
            self.ch_ola[k].add_frame(&self.real_out, &self.win, scale);
        }
        // Sector objects: skip the inverse FFT for sectors this frame left
        // silent (typical content lands in 2–4 sectors) — their output is the
        // draining OLA tail.
        for k in 0..SPEC_SECTORS {
            if self.hop_e[k] > SPEC_SILENT {
                let _ = self.fft_inv.process_with_scratch(
                    &mut self.spec_sec[k],
                    &mut self.real_out,
                    &mut self.ifft_scratch,
                );
                self.obj_ola[k].add_frame(&self.real_out, &self.win, scale);
            } else {
                self.obj_ola[k].add_silent_frame();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_api::RChannelLabel;
    use renderer::speaker_layout::{Speaker, SpeakerLayout};

    const LABELS_5_1: [RChannelLabel; 6] = [
        RChannelLabel::L,
        RChannelLabel::R,
        RChannelLabel::C,
        RChannelLabel::LFE,
        RChannelLabel::Ls,
        RChannelLabel::Rs,
    ];

    const LABELS_7_1: [RChannelLabel; 8] = [
        RChannelLabel::L,
        RChannelLabel::R,
        RChannelLabel::C,
        RChannelLabel::LFE,
        RChannelLabel::Ls,
        RChannelLabel::Rs,
        RChannelLabel::Lb,
        RChannelLabel::Rb,
    ];

    fn dummy_layout() -> SpeakerLayout {
        SpeakerLayout {
            radius_m: 1.0,
            speakers: vec![
                Speaker::from_cartesian("FL", -1.0, 1.0, 0.0, true, 0.0),
                Speaker::from_cartesian("FR", 1.0, 1.0, 0.0, true, 0.0),
            ],
        }
    }

    fn ctx<'a>(labels: &'a [RChannelLabel], layout: &'a SpeakerLayout) -> PrepareCtx<'a> {
        PrepareCtx {
            input_labels: labels,
            output_layout: layout,
            sample_rate: 48_000,
            surround_placement: renderer::live_params::SurroundPlacement::Side,
        }
    }

    fn sine(f: f32) -> impl Fn(usize) -> f32 {
        move |s: usize| (std::f32::consts::TAU * f * s as f32 / 48_000.0).sin()
    }

    fn xorshift(state: &mut u32) -> f32 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        *state = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    /// Quadrature projection: mean-square amplitude of the `f` Hz component
    /// over `samples` (relative comparisons only).
    fn tone_energy(samples: &[f32], f: f32) -> f32 {
        let mut c = 0.0f64;
        let mut s = 0.0f64;
        for (i, &x) in samples.iter().enumerate() {
            let ph = std::f64::consts::TAU * f as f64 * i as f64 / 48_000.0;
            c += x as f64 * ph.cos();
            s += x as f64 * ph.sin();
        }
        (((c * c + s * s).sqrt() / samples.len() as f64) as f32).powi(2)
    }

    fn channel_tail(pcm: &[f32], c: usize, ch: usize, from: usize, to: usize) -> Vec<f32> {
        (from..to).map(|s| pcm[s * c + ch]).collect()
    }

    fn make_planar(n: usize) -> Vec<Vec<f32>> {
        (0..SPEC_SECTORS).map(|_| vec![0.0; n]).collect()
    }

    #[test]
    fn plans_eight_sector_objects() {
        let layout = dummy_layout();
        for labels in [&LABELS_5_1[..], &LABELS_7_1[..]] {
            let ext = SpectralExtractor::prepare(&ctx(labels, &layout)).expect("prepare");
            let specs = ext.specs(0.0);
            assert_eq!(specs.len(), 8);
            assert_eq!(specs[0].name, "Direct_F");
            assert_eq!(specs[6].name, "Direct_L");
            // Front sector sits at the front centre, right sector at the right.
            assert!(specs[0].position[1] > 0.9 && specs[0].position[0].abs() < 0.1);
            assert!(specs[2].position[0] > 0.9 && specs[2].position[1].abs() < 0.1);
        }
    }

    #[test]
    fn too_few_positionable_channels_is_none() {
        let layout = dummy_layout();
        let labels = [RChannelLabel::C, RChannelLabel::LFE];
        assert!(SpectralExtractor::prepare(&ctx(&labels, &layout)).is_none());
    }

    #[test]
    fn strength_zero_is_delayed_identity() {
        let layout = dummy_layout();
        let mut ext = SpectralExtractor::prepare(&ctx(&LABELS_5_1, &layout)).unwrap();
        let c = 6usize;
        let n = 8192usize;
        let mut state = 0x1234_5678u32;
        let input: Vec<f32> = (0..c * n).map(|_| 0.5 * xorshift(&mut state)).collect();
        let mut bed = input.clone();
        let mut planar = make_planar(n);
        ext.process(&mut bed, c, n, 0.0, &mut planar);
        for s in SPEC_FFT_SIZE..n {
            for ch in 0..c {
                let got = bed[s * c + ch];
                let want = input[(s - SPEC_FFT_SIZE) * c + ch];
                let tol = if ch == 3 { 1.0e-7 } else { 1.0e-3 }; // LFE rides the exact delay line
                assert!(
                    (got - want).abs() < tol,
                    "ch {ch} sample {s}: {got} vs {want}"
                );
            }
        }
        for buf in planar.iter() {
            assert!(
                buf.iter().all(|x| x.abs() < 1.0e-9),
                "objects must be silent"
            );
        }
    }

    #[test]
    fn extracts_panned_source_per_band() {
        let layout = dummy_layout();
        let mut ext = SpectralExtractor::prepare(&ctx(&LABELS_5_1, &layout)).unwrap();
        let c = 6usize;
        let n = 24_000usize;
        let s = sine(700.0);
        let mut bed = vec![0.0f32; c * n];
        for i in 0..n {
            let v = s(i) * 0.5;
            bed[i * c] = 0.8 * v; // L
            bed[i * c + 2] = 0.2 * v; // C
        }
        let in_l = tone_energy(&channel_tail(&bed, c, 0, n / 2, n), 700.0);
        let mut planar = make_planar(n);
        ext.process(&mut bed, c, n, 1.0, &mut planar);
        let tail = n / 2..n;
        // The source azimuth (≈ −36°) lands in the front-left sector (7).
        let fl = tone_energy(&planar[7][tail.clone()], 700.0);
        let l_res = tone_energy(&channel_tail(&bed, c, 0, n / 2, n), 700.0);
        assert!(
            fl > 0.2 * in_l,
            "front-left object should carry the panned energy ({fl} vs in_l {in_l})"
        );
        assert!(
            l_res < 0.1 * in_l,
            "L should be largely emptied at 700 Hz ({l_res} vs {in_l})"
        );
        let mut specs = ext.specs(0.0);
        ext.refresh_positions(0.0, &mut specs);
        let pos = specs[7].position;
        assert!(
            pos[0] < -0.4 && pos[1] > 0.6,
            "object should localize front-left, got {pos:?}"
        );
    }

    #[test]
    fn two_simultaneous_sources_resolved_independently() {
        // THE differentiating case vs the broadband method: a 400 Hz source in
        // L (front-left) and a 3.2 kHz source in Rs (right), at the same time.
        // Each must land in its own sector with its own band, and each bed
        // channel must be reduced only where its source lives.
        let layout = dummy_layout();
        let mut ext = SpectralExtractor::prepare(&ctx(&LABELS_5_1, &layout)).unwrap();
        let c = 6usize;
        let n = 24_000usize;
        let (s1, s2) = (sine(400.0), sine(3200.0));
        let mut bed = vec![0.0f32; c * n];
        for i in 0..n {
            bed[i * c] = 0.5 * s1(i); // L
            bed[i * c + 5] = 0.5 * s2(i); // Rs
        }
        let in_l = tone_energy(&channel_tail(&bed, c, 0, n / 2, n), 400.0);
        let in_rs = tone_energy(&channel_tail(&bed, c, 5, n / 2, n), 3200.0);
        let mut planar = make_planar(n);
        ext.process(&mut bed, c, n, 1.0, &mut planar);
        let tail = n / 2..n;
        // L sits at az −45° → front-left sector (7); Rs (Side) at az +90° →
        // right sector (2).
        let fl_400 = tone_energy(&planar[7][tail.clone()], 400.0);
        let fl_3200 = tone_energy(&planar[7][tail.clone()], 3200.0);
        let r_400 = tone_energy(&planar[2][tail.clone()], 400.0);
        let r_3200 = tone_energy(&planar[2][tail.clone()], 3200.0);
        assert!(
            fl_400 > 0.2 * in_l,
            "front-left object should carry 400 Hz ({fl_400} vs {in_l})"
        );
        assert!(
            r_3200 > 0.2 * in_rs,
            "right object should carry 3.2 kHz ({r_3200} vs {in_rs})"
        );
        assert!(
            fl_3200 < 0.05 * in_rs,
            "front-left object must not carry the 3.2 kHz source ({fl_3200} vs {in_rs})"
        );
        assert!(
            r_400 < 0.05 * in_l,
            "right object must not carry the 400 Hz source ({r_400} vs {in_l})"
        );
        let l_res = tone_energy(&channel_tail(&bed, c, 0, n / 2, n), 400.0);
        let rs_res = tone_energy(&channel_tail(&bed, c, 5, n / 2, n), 3200.0);
        assert!(
            l_res < 0.1 * in_l,
            "L emptied at 400 Hz ({l_res} vs {in_l})"
        );
        assert!(
            rs_res < 0.1 * in_rs,
            "Rs emptied at 3.2 kHz ({rs_res} vs {in_rs})"
        );
    }

    #[test]
    fn diffuse_field_stays_in_bed() {
        // Direction-balanced independent noise (7.1 without C: fronts cancel
        // backs, sides cancel each other) — the honest "ideally diffuse" case
        // for the virtual B-format. An unbalanced set (e.g. L+R+Ls+Rs only)
        // legitimately reads partly direct toward its mean direction; see the
        // module docs.
        let layout = dummy_layout();
        let mut ext = SpectralExtractor::prepare(&ctx(&LABELS_7_1, &layout)).unwrap();
        let c = 8usize;
        let n = 24_000usize;
        let mut states = [1u32, 99, 12345, 777_777, 0xdead_beef, 0x00c0_ffee];
        let chans = [0usize, 1, 4, 5, 6, 7]; // L, R, Ls, Rs, Lb, Rb
        let mut bed = vec![0.0f32; c * n];
        for i in 0..n {
            for (k, &ch) in chans.iter().enumerate() {
                bed[i * c + ch] = 0.4 * xorshift(&mut states[k]);
            }
        }
        let in_pow: f32 = (n / 2..n)
            .flat_map(|s| chans.iter().map(move |&ch| (s, ch)))
            .map(|(s, ch)| bed[s * c + ch].powi(2))
            .sum();
        let mut planar = make_planar(n);
        ext.process(&mut bed, c, n, 1.0, &mut planar);
        let obj_pow: f32 = planar
            .iter()
            .flat_map(|buf| buf[n / 2..n].iter())
            .map(|x| x * x)
            .sum();
        let bed_pow: f32 = (n / 2..n)
            .flat_map(|s| chans.iter().map(move |&ch| (s, ch)))
            .map(|(s, ch)| bed[s * c + ch].powi(2))
            .sum();
        assert!(
            obj_pow < 0.1 * in_pow,
            "decorrelated field → near-silent objects ({obj_pow} vs {in_pow})"
        );
        assert!(
            bed_pow > 0.55 * in_pow,
            "decorrelated bed should be mostly retained ({bed_pow} vs {in_pow})"
        );
    }

    #[test]
    fn force_direct_reconstruction_gain() {
        // With 1 − ψ forced to 1 and a single-channel source (power-match gain
        // = 1), full strength must move the source to its sector object at
        // unity gain and empty the bed channel.
        let layout = dummy_layout();
        let mut ext = SpectralExtractor::prepare(&ctx(&LABELS_5_1, &layout)).unwrap();
        ext.force_direct = true;
        let c = 6usize;
        let n = 24_000usize;
        let s = sine(700.0);
        let mut bed = vec![0.0f32; c * n];
        for i in 0..n {
            bed[i * c + 2] = 0.5 * s(i); // C → front sector (0)
        }
        let in_c = tone_energy(&channel_tail(&bed, c, 2, n / 2, n), 700.0);
        let mut planar = make_planar(n);
        ext.process(&mut bed, c, n, 1.0, &mut planar);
        let f = tone_energy(&planar[0][n / 2..n], 700.0);
        let c_res = tone_energy(&channel_tail(&bed, c, 2, n / 2, n), 700.0);
        assert!(
            (f - in_c).abs() < 0.05 * in_c,
            "front object should reconstruct the source at unity ({f} vs {in_c})"
        );
        assert!(
            c_res < 1.0e-4 * in_c,
            "C must be emptied ({c_res} vs {in_c})"
        );
    }

    /// Perf probe (not a correctness test): `cargo test --release -p
    /// orender_engine perf_throughput -- --ignored --nocapture`. The engine's
    /// `perf_probe` example streams object-carrying content, which bypasses the
    /// phantom stage entirely — this is the substitute measurement.
    #[test]
    #[ignore = "perf probe — run explicitly in release"]
    fn perf_throughput_7_1() {
        let layout = dummy_layout();
        let mut ext = SpectralExtractor::prepare(&ctx(&LABELS_7_1, &layout)).unwrap();
        let c = 8usize;
        let block = 512usize;
        let seconds = 60usize;
        let blocks = 48_000 * seconds / block;
        let mut state = 7u32;
        let mut bed = vec![0.0f32; c * block];
        let mut planar = make_planar(block);
        let start = std::time::Instant::now();
        for _ in 0..blocks {
            for v in bed.iter_mut() {
                *v = 0.3 * xorshift(&mut state);
            }
            ext.process(&mut bed, c, block, 0.7, &mut planar);
        }
        let elapsed = start.elapsed().as_secs_f64();
        let ratio = seconds as f64 / elapsed;
        println!("spectral 7.1: {seconds} s of audio in {elapsed:.3} s → {ratio:.0}× realtime");
        assert!(
            ratio > 10.0,
            "must be far faster than realtime ({ratio:.1}×)"
        );
    }

    #[test]
    fn finite_bounded_and_silence_decays() {
        let layout = dummy_layout();
        for strength in [0.0f32, 0.3, 0.7, 1.0] {
            let mut ext = SpectralExtractor::prepare(&ctx(&LABELS_7_1, &layout)).unwrap();
            let c = 8usize;
            let n = 12_000usize;
            let s = sine(440.0);
            let mut state = 42u32;
            let mut bed = vec![0.0f32; c * n];
            for i in 0..n / 2 {
                // Content half: tones + noise, then hard silence.
                bed[i * c] = 0.5 * s(i);
                bed[i * c + 4] = 0.4 * xorshift(&mut state);
            }
            let mut planar = make_planar(n);
            ext.process(&mut bed, c, n, strength, &mut planar);
            assert!(bed.iter().all(|x| x.is_finite()));
            assert!(planar.iter().flatten().all(|x| x.is_finite()));
            // Well past the content + one frame of latency: everything decays.
            for s in n - 1000..n {
                for ch in 0..c {
                    assert!(
                        bed[s * c + ch].abs() < 1.0e-6,
                        "bed ch {ch} should decay to silence (strength {strength})"
                    );
                }
            }
            assert!(
                planar
                    .iter()
                    .all(|buf| buf[n - 1000..].iter().all(|x| x.abs() < 1.0e-6)),
                "objects should decay to silence (strength {strength})"
            );
        }
    }
}
