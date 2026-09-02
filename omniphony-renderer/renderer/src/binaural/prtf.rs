//! Spagnol/Geronazzo/Avanzini structural PRTF model — the "resonances + notches"
//! pinna alternative to the Brown-Duda echo model
//! ([`super::hrir::ParametricPinnaHrir`]).
//!
//! Layered on the analytic head shadow ([`SyntheticHrir`]), it adds:
//!   - a *resonance block*: two parallel second-order peak filters (concha
//!     modes) — the "ear timbre" the synthetic / Brown-Duda models lack, and
//!     the dominant cue against in-head localization;
//!   - a *reflection block*: three cascaded second-order notch filters whose
//!     center frequency rises with elevation (the elevation cue).
//!
//! Filters are exactly those of Geronazzo, Spagnol & Avanzini, "A HRTF Model
//! for Real-Time Customized 3-D Sound Rendering", SITIS 2011 (eqs 2–9). The
//! parameters are the **population average** read from that paper's mean PRTF
//! plots (Fig. 1/2/3c) plus Shaw's resonance modes; notch center frequencies
//! follow the reflection law f = c/(2d) (Spagnol et al., IEEE TASLP 2013). They
//! are NOT the exact (unpublished) order-5 polynomials — a representative
//! generic set, individualizable later from a pinna photo.

use super::hrir::{HRIR_LEN, HrirPair, HrirProvider, SyntheticHrir, ear_exposure, pinna_shade};

const PI: f32 = std::f32::consts::PI;

/// A normalised second-order section (`a0` = 1).
#[derive(Clone, Copy)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl Biquad {
    /// Zölzer peak/notch (boost/cut) section — SITIS eqs 2–6, with the `cut`
    /// variant of `k` (eq 9) for notches. `g_db > 0` boosts (`cut = false`),
    /// `g_db < 0` cuts (`cut = true`).
    fn peak_notch(cf: f32, g_db: f32, bw: f32, fs: f32, cut: bool) -> Self {
        let v0 = 10f32.powf(g_db / 20.0);
        let h0 = v0 - 1.0;
        let t = (PI * bw / fs).tan();
        let k = if cut {
            (t - v0) / (t + v0)
        } else {
            (t - 1.0) / (t + 1.0)
        };
        let l = -(2.0 * PI * cf / fs).cos();
        Self {
            b0: 1.0 + (1.0 + k) * h0 / 2.0,
            b1: l * (1.0 - k),
            b2: -k - (1.0 + k) * h0 / 2.0,
            a1: l * (1.0 - k),
            a2: -k,
        }
    }

    /// Bandpass-shaped peak with zero gain at DC/Nyquist (SITIS eqs 7–8). Used
    /// for the second resonance so the *parallel* sum keeps unity low-frequency
    /// gain (the first peak already carries the signal through).
    fn bandpass_peak(cf: f32, g_db: f32, bw: f32, fs: f32) -> Self {
        let v0 = 10f32.powf(g_db / 20.0);
        let h = 1.0 / (1.0 + (PI * bw / fs).tan());
        let l = -(2.0 * PI * cf / fs).cos();
        Self {
            b0: v0 * (1.0 - h),
            b1: 0.0,
            b2: -v0 * (1.0 - h),
            a1: 2.0 * l * h,
            a2: 2.0 * h - 1.0,
        }
    }

    /// Filter `x` (direct form I), returning `HRIR_LEN` output samples (the IIR
    /// tail beyond the FIR length is truncated — fine for these mild sections).
    fn process(&self, x: &[f32; HRIR_LEN]) -> [f32; HRIR_LEN] {
        let mut y = [0.0f32; HRIR_LEN];
        let (mut x1, mut x2, mut y1, mut y2) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        for n in 0..HRIR_LEN {
            let xn = x[n];
            let yn = self.b0 * xn + self.b1 * x1 + self.b2 * x2 - self.a1 * y1 - self.a2 * y2;
            y[n] = yn;
            x2 = x1;
            x1 = xn;
            y2 = y1;
            y1 = yn;
        }
        y
    }
}

/// The five second-order sections at one direction (shared by both ears since
/// the PRTF is azimuth-invariant up to ~30° off the median plane).
struct PrtfBlocks {
    res1: Biquad,
    res2: Biquad,
    n1: Biquad,
    n2: Biquad,
    n3: Biquad,
}

/// Structural PRTF pinna model (Spagnol et al.) with the population-average
/// preset. Resonances run in parallel, the three notches in series.
pub struct SpagnolPrtfHrir {
    /// Pinna coloration amount in [0, 1] — a dry/wet mix between the bare head
    /// shadow and the full PRTF. 0 ⇒ ≈ synthetic, 1 ⇒ full resonances + notches.
    pub depth: f32,
    /// Center-frequency scale (1.0 = published average). <1 lowers all notches
    /// and resonances (larger pinna), >1 raises them — crude 1-DOF
    /// individualization standing in for the photo-derived notch frequencies.
    pub freq_scale: f32,
    /// Head radius of the underlying head-shadow stage (see
    /// [`SyntheticHrir::head_radius_m`]).
    pub head_radius_m: f32,
}

impl SpagnolPrtfHrir {
    fn clamp_cf(cf: f32, fs: f32) -> f32 {
        cf.clamp(200.0, 0.49 * fs)
    }

    /// Resonance (parallel) + notch (cascade) blocks on one ear's head-shadow
    /// IR, then dry/wet-mixed by `depth` (this ear's, see
    /// [`pinna_shade`]).
    fn apply(&self, base: &[f32; HRIR_LEN], b: &PrtfBlocks, depth: f32) -> [f32; HRIR_LEN] {
        // Resonance block: H_res1 (unity + peak) in parallel with H_res2 (pure
        // bandpass), summed.
        let r1 = b.res1.process(base);
        let r2 = b.res2.process(base);
        let mut res = [0.0f32; HRIR_LEN];
        for n in 0..HRIR_LEN {
            res[n] = r1[n] + r2[n];
        }
        // Reflection block: three notches in series.
        let wet = b.n3.process(&b.n2.process(&b.n1.process(&res)));
        // Dry/wet mix — `depth` is the amount of pinna coloration.
        let d = depth.clamp(0.0, 1.0);
        let mut out = [0.0f32; HRIR_LEN];
        for n in 0..HRIR_LEN {
            out[n] = (1.0 - d) * base[n] + d * wet[n];
        }
        out
    }
}

impl HrirProvider for SpagnolPrtfHrir {
    fn render(&self, az_deg: f32, el_deg: f32, sample_rate: u32) -> HrirPair {
        // Base: analytic head shadow + ILD (identical to SyntheticHrir).
        let head = SyntheticHrir {
            head_radius_m: self.head_radius_m,
        };
        let mut pair = head.render(az_deg, el_deg, sample_rate);
        if self.depth <= 1e-4 {
            return pair;
        }
        let fs = sample_rate as f32;
        let sc = self.freq_scale.clamp(0.5, 1.5);
        let cf = |f: f32| Self::clamp_cf(f * sc, fs);

        // Elevation interpolation: t = 0 at -45°, 1 at +45° (the model's valid
        // frontal range). Below -45° hold the -45° fit; above +45° fade the
        // notches out (PRTFs lose their notch structure overhead).
        let phi = el_deg;
        let t = ((phi.min(45.0) + 45.0) / 90.0).clamp(0.0, 1.0);
        let fade = if phi > 45.0 {
            ((90.0 - phi) / 45.0).clamp(0.0, 1.0)
        } else {
            1.0
        };

        // Population-average parameters (SITIS 2011 Fig. 1/2/3c + Shaw modes),
        // approximated as linear ramps in elevation.
        let g2 = 10.0 - 7.0 * t; // 2nd resonance: strong low, fades upward
        let nd = (-13.0 + 5.0 * t) * fade; // notch depth: deeper low, shallower high
        let bw = 1800.0 + 700.0 * t; // notch 3-dB bandwidth
        let blocks = PrtfBlocks {
            res1: Biquad::peak_notch(cf(4200.0), 12.0, 2500.0, fs, false), // Shaw mode 1
            res2: Biquad::bandpass_peak(cf(13000.0), g2, 4000.0, fs),
            n1: Biquad::peak_notch(cf(6000.0 + 3000.0 * t), nd, bw, fs, true),
            n2: Biquad::peak_notch(cf(7500.0 + 3500.0 * t), nd, bw, fs, true),
            n3: Biquad::peak_notch(cf(10000.0 + 4000.0 * t), nd, bw, fs, true),
        };

        // Each ear gets the pinna colouration to the extent it faces the
        // source; the blocks themselves are shared (the PRTF is azimuth-
        // invariant near the median plane).
        let (exp_l, exp_r) = ear_exposure(az_deg, el_deg);
        pair.left = self.apply(&pair.left, &blocks, self.depth * pinna_shade(exp_l));
        pair.right = self.apply(&pair.right, &blocks, self.depth * pinna_shade(exp_r));
        pair
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sumsq_diff(a: &[f32; HRIR_LEN], b: &[f32; HRIR_LEN]) -> f32 {
        a.iter().zip(b.iter()).map(|(x, y)| (x - y) * (x - y)).sum()
    }

    #[test]
    fn prtf_depth_zero_matches_synthetic() {
        let p = SpagnolPrtfHrir {
            depth: 0.0,
            freq_scale: 1.0,
            head_radius_m: crate::binaural::itd::DEFAULT_HEAD_RADIUS_M,
        }
        .render(0.0, 0.0, 48_000);
        let s = SyntheticHrir::default().render(0.0, 0.0, 48_000);
        assert_eq!(p.left, s.left);
        assert_eq!(p.right, s.right);
    }

    #[test]
    fn prtf_full_colours_the_spectrum() {
        // depth = 1 must clearly differ from the bare head shadow (resonances
        // + notches reshape the spectrum).
        let p = SpagnolPrtfHrir {
            depth: 1.0,
            freq_scale: 1.0,
            head_radius_m: crate::binaural::itd::DEFAULT_HEAD_RADIUS_M,
        }
        .render(0.0, 0.0, 48_000);
        let s = SyntheticHrir::default().render(0.0, 0.0, 48_000);
        assert!(
            sumsq_diff(&p.left, &s.left) > 1e-3,
            "PRTF did not colour the spectrum"
        );
    }

    #[test]
    fn prtf_depends_on_elevation() {
        // Notch frequencies rise with elevation, so low vs high must differ.
        let m = SpagnolPrtfHrir {
            depth: 1.0,
            freq_scale: 1.0,
            head_radius_m: crate::binaural::itd::DEFAULT_HEAD_RADIUS_M,
        };
        let lo = m.render(0.0, -45.0, 48_000);
        let hi = m.render(0.0, 45.0, 48_000);
        assert!(
            sumsq_diff(&lo.left, &hi.left) > 1e-3,
            "PRTF not elevation-dependent"
        );
    }

    #[test]
    fn prtf_freq_scale_shifts_response() {
        // The individualization knob must actually move the filters.
        let a = SpagnolPrtfHrir {
            depth: 1.0,
            freq_scale: 1.0,
            head_radius_m: crate::binaural::itd::DEFAULT_HEAD_RADIUS_M,
        }
        .render(0.0, 0.0, 48_000);
        let b = SpagnolPrtfHrir {
            depth: 1.0,
            freq_scale: 1.3,
            head_radius_m: crate::binaural::itd::DEFAULT_HEAD_RADIUS_M,
        }
        .render(0.0, 0.0, 48_000);
        assert!(
            sumsq_diff(&a.left, &b.left) > 1e-4,
            "freq_scale had no effect"
        );
    }

    /// The shadowed ear keeps a shallower version of the PRTF colouration
    /// (dry/wet at the floor); the median plane is untouched.
    #[test]
    fn prtf_is_shallower_on_the_shadowed_ear() {
        let m = SpagnolPrtfHrir {
            depth: 1.0,
            freq_scale: 1.0,
            head_radius_m: crate::binaural::itd::DEFAULT_HEAD_RADIUS_M,
        };
        // Relative to each ear's own head-shadow base (see the pinna test).
        let colour = |az: f32, ear: fn(&HrirPair) -> &[f32; HRIR_LEN]| -> f32 {
            let p = m.render(az, 0.0, 48_000);
            let s = SyntheticHrir::default().render(az, 0.0, 48_000);
            let base: f32 = ear(&s).iter().map(|x| x * x).sum();
            sumsq_diff(ear(&p), ear(&s)) / base
        };
        let (right, left) = (colour(90.0, |p| &p.right), colour(90.0, |p| &p.left));
        let ratio = left / right;
        assert!(
            ratio < 0.2 && ratio > 0.04,
            "shadowed/facing ratio {ratio}, expected ≈ 0.09"
        );
        let (fl, fr) = (colour(0.0, |p| &p.left), colour(0.0, |p| &p.right));
        assert!((fl - fr).abs() < 1e-6 * fl.max(1e-12));
    }

    #[test]
    fn prtf_output_is_finite() {
        // Guard against unstable sections across the whole grid range.
        let m = SpagnolPrtfHrir {
            depth: 1.0,
            freq_scale: 1.5,
            head_radius_m: crate::binaural::itd::DEFAULT_HEAD_RADIUS_M,
        };
        for el in [-45.0, -20.0, 0.0, 20.0, 45.0, 70.0, 90.0] {
            let p = m.render(30.0, el, 48_000);
            assert!(
                p.left.iter().all(|x| x.is_finite()) && p.right.iter().all(|x| x.is_finite()),
                "non-finite PRTF at elevation {el}"
            );
        }
    }
}
