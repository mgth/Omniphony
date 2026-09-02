//! Diffuse-field equalisation of an HRIR grid.
//!
//! The level normalisation in [`HrirSet`](super::hrir::HrirSet) aligns the
//! broadband energy of every set to one reference; it says nothing about
//! timbre. A measured set carries the colouration of the head and torso it
//! was measured on — the KEMAR's concha resonance, a broad presence bump —
//! which on headphones reads as a tonal signature rather than as space,
//! because a loudspeaker listener's own ears would have imprinted it on
//! *everything* and the brain discounts it.
//!
//! The classic remedy is to divide the set by its own **diffuse-field**
//! response: the power average of the responses over the sphere (cos(el)
//! weighted, both ears), smoothed over a third of an octave, inverted and
//! bounded. Applied as a minimum-phase filter to every kernel at build time
//! it costs nothing per sample, keeps every interaural difference exactly
//! (both ears get the same filter), and leaves the set spectrally neutral on
//! average — what a headphone listener expects a "flat" HRTF to sound like.

use realfft::RealFftPlanner;
use realfft::num_complex::Complex;

use super::hrir::{HRIR_LEN, HrirPair};

/// Transform size of the analysis and of the equaliser design.
const FFT_LEN: usize = 4096;
/// Third-octave smoothing: ± this many octaves around each bin.
const SMOOTH_OCT: f64 = 1.0 / 6.0;
/// Bound on the correction, in dB either way.
const MAX_GAIN_DB: f64 = 12.0;
/// Below the lower edge and above the upper one the correction fades to
/// unity over the transition: outside the band the diffuse-field estimate
/// is dominated by measurement noise (top) or by nothing at all (bottom).
const BAND_LO_HZ: (f64, f64) = (150.0, 250.0);
const BAND_HI_HZ: (f64, f64) = (14_000.0, 18_000.0);
/// Length of the minimum-phase equaliser convolved into every kernel.
pub const EQ_TAPS: usize = 256;

/// Diffuse-field power response of `grid` (per-node `weights`, taps `len`),
/// as `(frequency_hz, power)` per bin, both ears averaged.
pub fn diffuse_field_power(
    grid: &[HrirPair],
    weights: &[f32],
    len: usize,
    sample_rate: u32,
) -> Vec<(f32, f32)> {
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(FFT_LEN);
    let mut x = fft.make_input_vec();
    let mut spec = fft.make_output_vec();
    let mut power = vec![0.0f64; FFT_LEN / 2 + 1];
    let mut wsum = 0.0f64;
    for (pair, &w) in grid.iter().zip(weights) {
        if w <= 0.0 {
            continue;
        }
        for ear in [&pair.left, &pair.right] {
            x.fill(0.0);
            for (d, &s) in x.iter_mut().zip(&ear[..len]) {
                *d = s as f64;
            }
            fft.process(&mut x, &mut spec).expect("forward FFT");
            for (p, c) in power.iter_mut().zip(&spec) {
                *p += w as f64 * c.norm_sqr();
            }
            wsum += w as f64;
        }
    }
    let scale = if wsum > 0.0 { 1.0 / wsum } else { 0.0 };
    power
        .iter()
        .enumerate()
        .map(|(k, &p)| {
            (
                k as f32 * sample_rate as f32 / FFT_LEN as f32,
                (p * scale) as f32,
            )
        })
        .collect()
}

/// Smooth a per-bin power spectrum over ±[`SMOOTH_OCT`] octaves (bins
/// below 20 Hz are left alone; the window is symmetric in log frequency).
fn smooth_log(power: &[f32], sample_rate: u32) -> Vec<f64> {
    let n = power.len();
    let bin_hz = sample_rate as f64 / FFT_LEN as f64;
    let ratio = 2f64.powf(SMOOTH_OCT);
    let mut out = vec![0.0f64; n];
    for k in 0..n {
        let f = k as f64 * bin_hz;
        if f < 20.0 {
            out[k] = power[k] as f64;
            continue;
        }
        let lo = ((f / ratio) / bin_hz).floor().max(0.0) as usize;
        let hi = (((f * ratio) / bin_hz).ceil() as usize).min(n - 1);
        let mut acc = 0.0f64;
        for &p in &power[lo..=hi] {
            acc += p as f64;
        }
        out[k] = acc / (hi - lo + 1) as f64;
    }
    out
}

/// 0 outside the band, 1 inside, linear in log frequency across each edge.
fn band_weight(f: f64) -> f64 {
    let ramp = |(a, b): (f64, f64)| -> f64 {
        if f <= a {
            0.0
        } else if f >= b {
            1.0
        } else {
            (f.ln() - a.ln()) / (b.ln() - a.ln())
        }
    };
    if f < BAND_HI_HZ.0 {
        ramp(BAND_LO_HZ)
    } else {
        1.0 - ramp(BAND_HI_HZ)
    }
}

/// The equaliser for `grid`: a minimum-phase FIR of [`EQ_TAPS`] taps whose
/// magnitude is the bounded, smoothed inverse of the diffuse-field response
/// within the band (unity outside it). `None` when the grid is silent.
pub fn design(
    grid: &[HrirPair],
    weights: &[f32],
    len: usize,
    sample_rate: u32,
) -> Option<Vec<f32>> {
    let power: Vec<f32> = diffuse_field_power(grid, weights, len, sample_rate)
        .into_iter()
        .map(|(_, p)| p)
        .collect();
    design_from_power(&power, sample_rate)
}

/// [`design`] from an already computed per-bin diffuse-field power.
fn design_from_power(power: &[f32], sample_rate: u32) -> Option<Vec<f32>> {
    let peak = power.iter().cloned().fold(0.0f32, f32::max);
    if peak <= 0.0 {
        return None;
    }
    let smooth = smooth_log(power, sample_rate);
    // Reference: the band's mean power, so the correction is centred on
    // zero dB and the set's level stays where the normalisation puts it.
    let bin_hz = sample_rate as f64 / FFT_LEN as f64;
    let (mut ref_acc, mut ref_n) = (0.0f64, 0usize);
    for (k, &p) in smooth.iter().enumerate() {
        let f = k as f64 * bin_hz;
        if f >= BAND_LO_HZ.1 && f <= BAND_HI_HZ.0 && p > 0.0 {
            ref_acc += p.ln();
            ref_n += 1;
        }
    }
    if ref_n == 0 {
        return None;
    }
    let ref_power = (ref_acc / ref_n as f64).exp();
    let max_gain = 10f64.powf(MAX_GAIN_DB / 20.0);
    let mut gain: Vec<Complex<f64>> = smooth
        .iter()
        .enumerate()
        .map(|(k, &p)| {
            let f = k as f64 * bin_hz;
            let g = if p > 0.0 {
                (ref_power / p).sqrt().clamp(1.0 / max_gain, max_gain)
            } else {
                max_gain
            };
            // Fade the correction to unity outside the band, in dB.
            Complex::new(g.ln().mul_add(band_weight(f), 0.0).exp(), 0.0)
        })
        .collect();
    // Zero-phase response → minimum-phase filter of the same magnitude.
    let mut planner = RealFftPlanner::<f64>::new();
    let ifft = planner.plan_fft_inverse(FFT_LEN);
    let mut zero_phase = ifft.make_output_vec();
    ifft.process(&mut gain, &mut zero_phase)
        .expect("inverse FFT of the gain");
    let scale = 1.0 / FFT_LEN as f64;
    let mut zero_phase: Vec<f32> = zero_phase.iter().map(|&v| (v * scale) as f32).collect();
    // The inverse transform is circular: the symmetric response sits at
    // index 0 with its mirror half wrapped to the end. Centre it, so that
    // the *linear* transform the minimum-phase reconstruction takes has the
    // designed magnitude between the grid frequencies too, not only on
    // them (left wrapped, the reconstruction saw a rippled magnitude and
    // delivered about half of the correction).
    zero_phase.rotate_right(FFT_LEN / 2);
    let mut eq = super::measured::minimum_phase(&zero_phase);
    eq.truncate(EQ_TAPS);
    Some(eq)
}

/// Convolve every kernel of `grid` with `eq`, keeping `len` taps of the
/// result (the tail past `len` is faded before the cut, like a provider's).
pub fn apply(grid: &mut [HrirPair], eq: &[f32], len: usize) {
    let mut acc = [0.0f32; HRIR_LEN + EQ_TAPS];
    for pair in grid.iter_mut() {
        for ear in [&mut pair.left, &mut pair.right] {
            acc.fill(0.0);
            for (n, &h) in ear[..len].iter().enumerate() {
                if h == 0.0 {
                    continue;
                }
                for (k, &e) in eq.iter().enumerate() {
                    acc[n + k] += h * e;
                }
            }
            ear.fill(0.0);
            ear[..HRIR_LEN].copy_from_slice(&acc[..HRIR_LEN]);
            super::hrir::truncate_with_fade(ear, len);
        }
    }
}

/// Equalise `grid` in place: [`design`] then [`apply`]. Returns whether a
/// filter was applied (a silent grid is left alone).
pub fn equalise(grid: &mut [HrirPair], weights: &[f32], len: usize, sample_rate: u32) -> bool {
    match design(grid, weights, len, sample_rate) {
        Some(eq) => {
            apply(grid, &eq, len);
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binaural::hrir::HrirSet;
    use crate::binaural::measured::MeasuredHrirData;

    /// Peak-to-peak spread (dB) of the diffuse-field power between `lo` and
    /// `hi` Hz, on the set's own grid.
    fn spread_db(set: &HrirSet, lo: f32, hi: f32) -> f32 {
        let resp = set.diffuse_field_response();
        let smooth = smooth_log(&resp.iter().map(|(_, p)| *p).collect::<Vec<_>>(), 48_000);
        let vals: Vec<f64> = resp
            .iter()
            .zip(&smooth)
            .filter(|((f, _), _)| *f >= lo && *f <= hi)
            .map(|(_, &p)| 10.0 * p.max(1e-20).log10())
            .collect();
        let max = vals.iter().cloned().fold(f64::MIN, f64::max);
        let min = vals.iter().cloned().fold(f64::MAX, f64::min);
        (max - min) as f32
    }

    /// The equalised KEMAR set is spectrally flat on average across the
    /// band the kernel length can hold — a 128-tap kernel (2.7 ms at
    /// 48 kHz) cannot carry a correction finer than a few hundred hertz,
    /// so the octaves below 1 kHz are only partly corrected; the raw set is
    /// not flat anywhere.
    #[test]
    fn equalised_kemar_is_flat_in_the_diffuse_field() {
        let raw = HrirSet::new(&MeasuredHrirData::saf_kemar(), 48_000);
        let eq = HrirSet::build(&MeasuredHrirData::saf_kemar(), 48_000, true);
        for (lo, hi) in [
            (300.0f32, 1_000.0f32),
            (1_000.0, 4_000.0),
            (4_000.0, 12_000.0),
            (300.0, 12_000.0),
        ] {
            println!(
                "[measure] KEMAR diffuse-field spread {lo:.0}–{hi:.0} Hz: raw {:.1} dB, equalised {:.1} dB",
                spread_db(&raw, lo, hi),
                spread_db(&eq, lo, hi)
            );
        }
        let (before, after) = (
            spread_db(&raw, 1_000.0, 12_000.0),
            spread_db(&eq, 1_000.0, 12_000.0),
        );
        assert!(before > 4.0, "the raw set should be coloured: {before} dB");
        assert!(after < 2.5, "equalised set not flat: {after} dB");
    }

    /// Both ears get the same filter: the interaural level difference of a
    /// lateral source is unchanged at every frequency (the broadband energy
    /// ratio may move, since the filter reweights two different spectra —
    /// that is not the cue, the per-frequency ratio is).
    #[test]
    fn equalisation_preserves_interaural_differences() {
        let raw = HrirSet::new(&MeasuredHrirData::saf_kemar(), 48_000);
        let eq = HrirSet::build(&MeasuredHrirData::saf_kemar(), 48_000, true);
        let mag = |h: &[f32], f: f64| -> f64 {
            let (mut c, mut si) = (0.0f64, 0.0f64);
            for (i, &v) in h.iter().enumerate() {
                let ph = 2.0 * std::f64::consts::PI * f * i as f64 / 48_000.0;
                c += v as f64 * ph.cos();
                si += v as f64 * ph.sin();
            }
            (c * c + si * si).sqrt().max(1e-9)
        };
        let ild_db = |set: &HrirSet, f: f64| -> f64 {
            let mut p = HrirPair {
                left: [0.0; HRIR_LEN],
                right: [0.0; HRIR_LEN],
            };
            set.at(90.0, 0.0, &mut p);
            20.0 * (mag(&p.right, f) / mag(&p.left, f)).log10()
        };
        for f in [500.0f64, 1_000.0, 2_000.0, 4_000.0, 8_000.0] {
            let (a, b) = (ild_db(&raw, f), ild_db(&eq, f));
            assert!(
                (a - b).abs() < 1.0,
                "ILD at 90°, {f} Hz moved from {a:.1} to {b:.1} dB"
            );
        }
    }

    /// Diagnostic: the designed filter's magnitude against the intended
    /// inverse of the smoothed diffuse-field response, and the result after
    /// application, at a few frequencies.
    #[test]
    fn diagnostic_eq_magnitude_vs_target() {
        let raw = HrirSet::new(&MeasuredHrirData::saf_kemar(), 48_000);
        let power: Vec<f32> = raw
            .diffuse_field_response()
            .iter()
            .map(|(_, p)| *p)
            .collect();
        let smooth = smooth_log(&power, 48_000);
        let (mut acc, mut n) = (0.0f64, 0usize);
        for (k, &p) in smooth.iter().enumerate() {
            let f = k as f64 * 48_000.0 / FFT_LEN as f64;
            if (250.0..=14_000.0).contains(&f) && p > 0.0 {
                acc += p.ln();
                n += 1;
            }
        }
        let ref_power = (acc / n as f64).exp();
        let eq = design_from_power(&power, 48_000).expect("eq");
        let mag = |h: &[f32], f: f64| -> f64 {
            let (mut c, mut si) = (0.0f64, 0.0f64);
            for (i, &v) in h.iter().enumerate() {
                let ph = 2.0 * std::f64::consts::PI * f * i as f64 / 48_000.0;
                c += v as f64 * ph.cos();
                si += v as f64 * ph.sin();
            }
            (c * c + si * si).sqrt()
        };
        let eqd = HrirSet::build(&MeasuredHrirData::saf_kemar(), 48_000, true);
        let after: Vec<f32> = eqd
            .diffuse_field_response()
            .iter()
            .map(|(_, p)| *p)
            .collect();
        let after_s = smooth_log(&after, 48_000);
        let (mut acc2, mut n2) = (0.0f64, 0usize);
        for (k, &p) in after_s.iter().enumerate() {
            let f = k as f64 * 48_000.0 / FFT_LEN as f64;
            if (250.0..=14_000.0).contains(&f) && p > 0.0 {
                acc2 += p.ln();
                n2 += 1;
            }
        }
        let ref_after = (acc2 / n2 as f64).exp();
        for f in [
            500.0f64, 1_000.0, 2_000.0, 3_000.0, 4_000.0, 6_000.0, 8_000.0,
        ] {
            let k = (f * FFT_LEN as f64 / 48_000.0).round() as usize;
            let target = (ref_power / smooth[k]).sqrt();
            println!(
                "[measure] {f:>6.0} Hz: DF before {:+.1} dB, EQ target {:+.1} dB, EQ filter {:+.1} dB, DF after {:+.1} dB",
                10.0 * (smooth[k] / ref_power).log10(),
                20.0 * target.log10(),
                20.0 * mag(&eq, f).log10(),
                10.0 * (after_s[k] / ref_after).log10()
            );
        }
    }

    /// Off by default: `new` is `build` without the equaliser.
    #[test]
    fn new_is_build_without_equalisation() {
        let a = HrirSet::new(&MeasuredHrirData::saf_kemar(), 48_000);
        let b = HrirSet::build(&MeasuredHrirData::saf_kemar(), 48_000, false);
        let mut pa = HrirPair {
            left: [0.0; HRIR_LEN],
            right: [0.0; HRIR_LEN],
        };
        let mut pb = pa.clone();
        a.at(37.0, 12.0, &mut pa);
        b.at(37.0, 12.0, &mut pb);
        assert_eq!(pa.left, pb.left);
    }
}
