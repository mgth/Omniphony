//! Measured HRIR sets (e.g. the embedded SAF KEMAR data, or a loaded SOFA file).
//!
//! A [`MeasuredHrirData`] holds scattered-direction impulse responses. It
//! implements [`HrirProvider`] by blending the three nearest measurements
//! (inverse-angular-distance weights, per-ear energy compensation) after
//! onset-alignment and truncation to [`HRIR_LEN`], so it plugs straight into
//! [`HrirSet::new`](super::hrir::HrirSet::new) and reuses the regular-grid
//! bilinear interpolation. Time alignment (the interaural delay is supplied
//! analytically, see [`super::itd`]) keeps both the blend and the grid
//! interpolable without comb-filtering.

use super::hrir::{HRIR_LEN, HrirPair, HrirProvider};

/// Embedded SAF default HRIRs: Genelec Aural ID of a KEMAR dummy head @48 kHz,
/// ISC-licensed (© 2020 Leo McCormack; data by Aki Mäkivirta & Jaan Johansson).
/// Pre-aligned and truncated to `HRIR_LEN` by `tools/gen_saf_hrir.py`.
static SAF_KEMAR_BLOB: &[u8] = include_bytes!("data/saf_kemar.bin");

const BLOB_MAGIC: u32 = 0x4F48_4952; // 'OHIR'

/// Onset detection / alignment parameters (mirror the generator's, used for any
/// not-yet-aligned source such as SOFA).
const PRE_SAMPLES: usize = 8;
const ONSET_FRAC: f32 = 0.15;

/// A scattered set of measured HRIR pairs with their directions (renderer
/// convention: az 0 = front, +az = right; el 0 = horizontal, +90 = up).
pub struct MeasuredHrirData {
    pub sample_rate: u32,
    /// `(azimuth_deg, elevation_deg)` per measurement.
    dirs: Vec<(f32, f32)>,
    /// Unit direction vectors, parallel to `dirs`, for nearest lookup.
    vecs: Vec<[f32; 3]>,
    /// Left/right impulse responses per measurement (arbitrary length).
    irs: Vec<(Vec<f32>, Vec<f32>)>,
}

impl MeasuredHrirData {
    /// Build from raw measurements. `dirs[i]` corresponds to `irs[i]`.
    pub fn new(sample_rate: u32, dirs: Vec<(f32, f32)>, irs: Vec<(Vec<f32>, Vec<f32>)>) -> Self {
        let vecs = dirs.iter().map(|&(az, el)| dir_vec(az, el)).collect();
        Self {
            sample_rate,
            dirs,
            vecs,
            irs,
        }
    }

    /// The embedded SAF KEMAR set.
    pub fn saf_kemar() -> Self {
        Self::from_blob(SAF_KEMAR_BLOB).expect("embedded SAF KEMAR blob is valid")
    }

    pub fn len(&self) -> usize {
        self.dirs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.dirs.is_empty()
    }

    fn from_blob(blob: &[u8]) -> Option<Self> {
        let u32_at = |off: usize| -> Option<u32> {
            blob.get(off..off + 4)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        };
        let f32_at =
            |off: usize| -> f32 { f32::from_le_bytes(blob[off..off + 4].try_into().unwrap()) };
        if u32_at(0)? != BLOB_MAGIC {
            return None;
        }
        let count = u32_at(8)? as usize;
        let ir_len = u32_at(12)? as usize;
        let fs = u32_at(16)?;
        let mut off = 20;
        let rec = 8 + ir_len * 2 * 4;
        let mut dirs = Vec::with_capacity(count);
        let mut irs = Vec::with_capacity(count);
        for _ in 0..count {
            if off + rec > blob.len() {
                return None;
            }
            let az = f32_at(off);
            let el = f32_at(off + 4);
            let mut p = off + 8;
            let mut left = Vec::with_capacity(ir_len);
            let mut right = Vec::with_capacity(ir_len);
            for _ in 0..ir_len {
                left.push(f32_at(p));
                p += 4;
            }
            for _ in 0..ir_len {
                right.push(f32_at(p));
                p += 4;
            }
            dirs.push((az, el));
            irs.push((left, right));
            off += rec;
        }
        Some(Self::new(fs, dirs, irs))
    }

    /// This set resampled to `target` Hz (no-op if already there).
    ///
    /// Runs once at build time, never in the audio loop. Without it the
    /// 48 kHz KEMAR taps play verbatim at any engine rate, shifting every
    /// HRTF feature by the rate ratio (issue #151).
    pub fn resampled_to(self, target: u32) -> Self {
        if self.sample_rate == target {
            return self;
        }
        let from = self.sample_rate;
        let irs = self
            .irs
            .iter()
            .map(|(l, r)| (resample_ir(l, from, target), resample_ir(r, from, target)))
            .collect();
        Self {
            sample_rate: target,
            dirs: self.dirs,
            vecs: self.vecs,
            irs,
        }
    }

    /// The three measurements nearest to a query direction, as
    /// `(index, angle_rad)` sorted nearest-first.
    fn nearest3(&self, az_deg: f32, el_deg: f32) -> [(usize, f32); 3] {
        let q = dir_vec(az_deg, el_deg);
        // (dot, index): best three by dot product in one pass.
        let mut best = [(f32::NEG_INFINITY, usize::MAX); 3];
        for (i, v) in self.vecs.iter().enumerate() {
            let d = q[0] * v[0] + q[1] * v[1] + q[2] * v[2];
            if d > best[0].0 {
                best[2] = best[1];
                best[1] = best[0];
                best[0] = (d, i);
            } else if d > best[1].0 {
                best[2] = best[1];
                best[1] = (d, i);
            } else if d > best[2].0 {
                best[2] = (d, i);
            }
        }
        best.map(|(d, i)| {
            let i = if i == usize::MAX { 0 } else { i };
            (i, d.clamp(-1.0, 1.0).acos())
        })
    }
}

impl HrirProvider for MeasuredHrirData {
    // `_sample_rate` is deliberately unused: the set is brought to the engine
    // rate once via [`MeasuredHrirData::resampled_to`] before grid building.
    //
    // Spatial interpolation (issue #158): instead of snapping each grid node
    // to the single nearest measurement — which decimates a set denser than
    // the grid and steps discontinuously between cells — the three nearest
    // measurements are blended with inverse-angular-distance weights. The
    // per-ear blend is then rescaled to the weighted mean of the source
    // energies: onset-aligned neighbours are largely coherent, but their
    // residual decorrelation would otherwise dip the level between
    // measurement points. A query landing (nearly) on a measurement takes
    // that measurement alone.
    fn render(&self, az_deg: f32, el_deg: f32, _sample_rate: u32) -> HrirPair {
        let mut pair = HrirPair {
            left: [0.0; HRIR_LEN],
            right: [0.0; HRIR_LEN],
        };
        if self.irs.is_empty() {
            return pair;
        }
        let near = self.nearest3(az_deg, el_deg);

        // On (within ~0.06° of) a measurement point, or a tiny set: exact.
        if near[0].1 < 1e-3 || self.irs.len() < 3 {
            let (l, r) = &self.irs[near[0].0];
            align_into(l, &mut pair.left);
            align_into(r, &mut pair.right);
            return pair;
        }

        let mut weights = [0.0f32; 3];
        let mut wsum = 0.0f32;
        for (k, &(_, ang)) in near.iter().enumerate() {
            weights[k] = 1.0 / ang;
            wsum += weights[k];
        }

        let mut aligned = [0.0f32; HRIR_LEN];
        let mut target_l = 0.0f32;
        let mut target_r = 0.0f32;
        for (k, &(idx, _)) in near.iter().enumerate() {
            let w = weights[k] / wsum;
            let (l, r) = &self.irs[idx];
            align_into(l, &mut aligned);
            target_l += w * energy_of(&aligned);
            for (o, a) in pair.left.iter_mut().zip(&aligned) {
                *o += w * a;
            }
            align_into(r, &mut aligned);
            target_r += w * energy_of(&aligned);
            for (o, a) in pair.right.iter_mut().zip(&aligned) {
                *o += w * a;
            }
        }

        // Per-ear energy compensation toward the weighted source mean.
        for (ear, target) in [(&mut pair.left, target_l), (&mut pair.right, target_r)] {
            let e = energy_of(ear);
            if e > 1e-12 {
                let g = (target / e).sqrt();
                for v in ear.iter_mut() {
                    *v *= g;
                }
            }
        }
        pair
    }
}

fn energy_of(h: &[f32; HRIR_LEN]) -> f32 {
    h.iter().map(|&x| x * x).sum()
}

/// Build an [`HrirSet`](super::hrir::HrirSet) from a SOFA file, resampled to
/// `sample_rate`. `sofar` does the spatial interpolation and delay extraction,
/// so the returned per-ear IRs are time-aligned (the renderer adds the analytic
/// ITD). Requires the `sofa` build feature.
#[cfg(feature = "sofa")]
pub fn hrir_set_from_sofa(path: &str, sample_rate: u32) -> anyhow::Result<super::hrir::HrirSet> {
    use sofar::reader::OpenOptions;
    let mut opts = OpenOptions::new();
    opts.sample_rate(sample_rate as f32);
    let sofa = opts
        .open(path)
        .map_err(|e| anyhow::anyhow!("open SOFA '{path}': {e:?}"))?;
    let filter_len = sofa.filter_len();
    let provider = SofaProvider { sofa, filter_len };
    let set = super::hrir::HrirSet::new(&provider, sample_rate);
    check_loaded_set(&set, path, filter_len)?;
    Ok(set)
}

/// Below this peak a set is silence: [`HrirSet::new`](super::hrir::HrirSet::new)
/// normalizes any usable set to unit mean energy, so a surviving one peaks
/// around 1 — six orders of magnitude clear of this bound.
const SILENT_PEAK: f32 = 1e-9;

/// Refuse an HRIR set a SOFA file cannot actually drive.
///
/// `sofar` reports no error when it fails to locate the impulse responses: it
/// fills every query with zeros (the fallback in `Sofar::filter`), so a file
/// whose layout it misreads yields a complete, entirely silent grid. Left
/// alone that reaches the render path and mutes the binaural output — every
/// channel except the LFE, which bypasses the binaural stage, hence "only the
/// LFE is audible" (issue #219). Failing here turns that silence into a
/// message and lets the caller keep the previous set.
///
/// The known trigger is a room impulse response. `MultiSpeakerBRIR` stores
/// `Data.IR` as `[M][R][E][N]`; `sofar` reads it as `[M][R][N]`, takes the
/// emitter count for the filter length, and so slices the handful of samples
/// that *precede* the direct sound — all zeros, in every direction.
fn check_loaded_set(
    set: &super::hrir::HrirSet,
    path: &str,
    filter_len: usize,
) -> anyhow::Result<()> {
    if set.peak() <= SILENT_PEAK {
        anyhow::bail!(
            "SOFA '{path}' builds a silent HRIR set (filter length {filter_len}): the reader \
             returned no impulse response for any direction. Room impulse responses \
             (MultiSpeakerBRIR, SingleRoom*SRIR) are not supported — the binaural stage needs \
             a free-field set such as SimpleFreeFieldHRIR."
        );
    }
    if set.is_direction_invariant() {
        log::warn!(
            "SOFA '{path}': every direction resolves to the same impulse response, so the \
             binaural image will not move. The file carries no per-direction measurement the \
             reader can use — typically a single SourcePosition, with the directions held in \
             ListenerView (the SingleRoomSRIR convention)."
        );
    }
    Ok(())
}

/// Adapts a loaded SOFA file to [`HrirProvider`]: maps the renderer's direction
/// convention to SOFA Cartesian (x = front, y = left, z = up) and lets `sofar`
/// interpolate the nearest measurements.
#[cfg(feature = "sofa")]
struct SofaProvider {
    sofa: sofar::reader::Sofar,
    filter_len: usize,
}

#[cfg(feature = "sofa")]
impl HrirProvider for SofaProvider {
    fn render(&self, az_deg: f32, el_deg: f32, _sample_rate: u32) -> HrirPair {
        let az = az_deg.to_radians();
        let el = el_deg.to_radians();
        let ce = el.cos();
        // renderer (+az=right, +Y=front) → SOFA (x=front, y=left, z=up).
        let x = az.cos() * ce;
        let y = -(az.sin() * ce);
        let z = el.sin();
        let mut filter = sofar::reader::Filter::new(self.filter_len);
        self.sofa.filter(x, y, z, &mut filter);
        let mut pair = HrirPair {
            left: [0.0; HRIR_LEN],
            right: [0.0; HRIR_LEN],
        };
        align_into(&filter.left, &mut pair.left);
        align_into(&filter.right, &mut pair.right);
        pair
    }
}

/// Unit vector for a direction (az 0 = front/+Y, +az = right/+X; el up = +Z).
fn dir_vec(az_deg: f32, el_deg: f32) -> [f32; 3] {
    let az = az_deg.to_radians();
    let el = el_deg.to_radians();
    let ce = el.cos();
    [ce * az.sin(), ce * az.cos(), el.sin()]
}

/// Offline windowed-sinc resampler for measured IRs (Blackman window,
/// half-width 16 input samples, low-passed at the lower of the two Nyquists
/// so downsampling does not alias). Build-time only — O(len·32) per IR.
fn resample_ir(x: &[f32], from: u32, to: u32) -> Vec<f32> {
    const HALF_WIDTH: isize = 16;
    let ratio = to as f64 / from as f64;
    let cutoff = ratio.min(1.0);
    let out_len = ((x.len() as f64) * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for n in 0..out_len {
        // Position of output sample `n` on the input's sample axis.
        let t = n as f64 / ratio;
        let k0 = t.floor() as isize;
        let mut acc = 0.0f64;
        for k in (k0 - HALF_WIDTH + 1)..=(k0 + HALF_WIDTH) {
            if k < 0 || k as usize >= x.len() {
                continue;
            }
            let d = t - k as f64;
            let w = 0.42
                + 0.5 * (std::f64::consts::PI * d / HALF_WIDTH as f64).cos()
                + 0.08 * (2.0 * std::f64::consts::PI * d / HALF_WIDTH as f64).cos();
            acc += x[k as usize] as f64 * cutoff * sinc(std::f64::consts::PI * cutoff * d) * w;
        }
        out.push(acc as f32);
    }
    out
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 { 1.0 } else { x.sin() / x }
}

/// Onset-align `ir` and copy `HRIR_LEN` taps into `out`. Idempotent for an
/// already-aligned IR (onset ≈ 0).
fn align_into(ir: &[f32], out: &mut [f32; HRIR_LEN]) {
    if ir.is_empty() {
        out.fill(0.0);
        return;
    }
    let peak = ir.iter().fold(0.0f32, |m, &x| m.max(x.abs())).max(1e-12);
    let thresh = ONSET_FRAC * peak;
    let onset = ir.iter().position(|&x| x.abs() >= thresh).unwrap_or(0);
    let start = onset.saturating_sub(PRE_SAMPLES);
    for (k, slot) in out.iter_mut().enumerate() {
        *slot = ir.get(start + k).copied().unwrap_or(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binaural::hrir::HrirSet;

    #[test]
    fn embedded_saf_loads() {
        let d = MeasuredHrirData::saf_kemar();
        assert_eq!(d.len(), 836);
        assert_eq!(d.sample_rate, 48_000);
    }

    fn energy(h: &[f32]) -> f32 {
        h.iter().map(|&x| x * x).sum()
    }

    #[test]
    fn measured_right_source_is_louder_in_right_ear() {
        // Validates the SAF→renderer azimuth handedness (+az = right).
        let set = HrirSet::new(&MeasuredHrirData::saf_kemar(), 48_000);
        let mut p = HrirPair {
            left: [0.0; HRIR_LEN],
            right: [0.0; HRIR_LEN],
        };
        set.at(90.0, 0.0, &mut p);
        assert!(
            energy(&p.right) > energy(&p.left),
            "L>R: handedness flipped?"
        );
    }

    #[test]
    fn measured_front_is_roughly_symmetric() {
        let set = HrirSet::new(&MeasuredHrirData::saf_kemar(), 48_000);
        let mut p = HrirPair {
            left: [0.0; HRIR_LEN],
            right: [0.0; HRIR_LEN],
        };
        set.at(0.0, 0.0, &mut p);
        let (el, er) = (energy(&p.left), energy(&p.right));
        let ratio = el / er;
        assert!(
            (0.5..2.0).contains(&ratio),
            "front asymmetric L={el} R={er}"
        );
    }

    /// The resampler must move signal content to the new sample axis: a tone
    /// resampled 48 k → 44.1 k stays at its absolute frequency.
    #[test]
    fn resampler_preserves_tone_frequency() {
        let (from, to) = (48_000u32, 44_100u32);
        let f0 = 1_000.0f64;
        let n = 480;
        // Hann-windowed tone so edge truncation does not pollute the check.
        let x: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f64 / from as f64;
                let w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos();
                ((2.0 * std::f64::consts::PI * f0 * t).sin() * w) as f32
            })
            .collect();
        let y = resample_ir(&x, from, to);
        assert_eq!(y.len(), 441);
        // Quadrature projection at f0 on the target rate vs. an off frequency.
        let project = |f: f64| -> f64 {
            let (mut c, mut s) = (0.0f64, 0.0f64);
            for (i, &v) in y.iter().enumerate() {
                let ph = 2.0 * std::f64::consts::PI * f * i as f64 / to as f64;
                c += v as f64 * ph.cos();
                s += v as f64 * ph.sin();
            }
            (c * c + s * s).sqrt()
        };
        let on = project(f0);
        let off = project(f0 * 1.35);
        assert!(
            on > off * 5.0,
            "tone did not stay at {f0} Hz: on={on} off={off}"
        );
    }

    /// Building the SAF set at a non-native rate must actually change the
    /// IRs (they used to be bit-identical at every rate — issue #151) while
    /// keeping their energy in the same ballpark.
    #[test]
    fn saf_resampled_to_441_differs_and_preserves_energy() {
        let native = MeasuredHrirData::saf_kemar();
        let resampled = MeasuredHrirData::saf_kemar().resampled_to(44_100);
        assert_eq!(resampled.sample_rate, 44_100);
        assert_eq!(resampled.len(), native.len());

        let grid_native = HrirSet::new(&native, 48_000);
        let grid_resampled = HrirSet::new(&resampled, 44_100);
        let mut a = HrirPair {
            left: [0.0; HRIR_LEN],
            right: [0.0; HRIR_LEN],
        };
        let mut b = a.clone();
        let mut max_diff = 0.0f32;
        for (az, el) in [(0.0, 0.0), (90.0, 0.0), (-30.0, 40.0), (150.0, -20.0)] {
            grid_native.at(az, el, &mut a);
            grid_resampled.at(az, el, &mut b);
            for (x, y) in a.left.iter().zip(&b.left) {
                max_diff = max_diff.max((x - y).abs());
            }
            let (ea, eb) = (energy(&a.left), energy(&b.left));
            let ratio = eb / ea.max(1e-12);
            assert!(
                (0.5..1.5).contains(&ratio),
                "energy drifted at ({az},{el}): native={ea} resampled={eb}"
            );
        }
        assert!(
            max_diff > 1e-4,
            "44.1 kHz grid is identical to the 48 kHz one — no resampling happened"
        );
    }

    /// A query landing exactly on a measurement must return that measurement
    /// (aligned), not a blend — interpolation only fills the space between.
    #[test]
    fn render_is_exact_on_measurement_points() {
        let d = MeasuredHrirData::saf_kemar();
        let (az, el) = d.dirs[100];
        let got = d.render(az, el, 48_000);
        let mut expected = HrirPair {
            left: [0.0; HRIR_LEN],
            right: [0.0; HRIR_LEN],
        };
        align_into(&d.irs[100].0, &mut expected.left);
        align_into(&d.irs[100].1, &mut expected.right);
        assert_eq!(got.left, expected.left);
        assert_eq!(got.right, expected.right);
    }

    /// Between measurement points the provider must actually blend (differ
    /// from the plain nearest measurement) and keep the per-ear energy at the
    /// weighted mean of its sources — no level dip from residual
    /// decorrelation between neighbours (issue #158).
    #[test]
    fn between_points_blends_and_preserves_energy() {
        let d = MeasuredHrirData::saf_kemar();
        // Midpoint between two real directions, at ear level-ish.
        let (az0, el0) = d.dirs[100];
        let near = d.nearest3(az0 + 2.0, el0 + 2.0);
        assert!(near[0].1 > 1e-3, "query must sit between measurements");
        let got = d.render(az0 + 2.0, el0 + 2.0, 48_000);

        // Differs from pure nearest-neighbour.
        let mut nn = HrirPair {
            left: [0.0; HRIR_LEN],
            right: [0.0; HRIR_LEN],
        };
        align_into(&d.irs[near[0].0].0, &mut nn.left);
        let diff: f32 = got
            .left
            .iter()
            .zip(&nn.left)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(diff > 1e-6, "midpoint query must blend, not snap");

        // Energy sits inside the range spanned by its three sources.
        let mut aligned = [0.0f32; HRIR_LEN];
        let mut src_energies = Vec::new();
        for &(idx, _) in &near {
            align_into(&d.irs[idx].0, &mut aligned);
            src_energies.push(energy(&aligned));
        }
        let e = energy(&got.left);
        let (lo, hi) = (
            src_energies.iter().cloned().fold(f32::MAX, f32::min),
            src_energies.iter().cloned().fold(0.0f32, f32::max),
        );
        assert!(
            e >= lo * 0.99 && e <= hi * 1.01,
            "blend energy {e} outside source range [{lo}, {hi}]"
        );
    }

    /// What `sofar` hands back for every direction once it has failed to
    /// locate the impulse responses: zeros, with no error (issue #219).
    struct SilentProvider;
    impl HrirProvider for SilentProvider {
        fn render(&self, _az: f32, _el: f32, _fs: u32) -> HrirPair {
            HrirPair {
                left: [0.0; HRIR_LEN],
                right: [0.0; HRIR_LEN],
            }
        }
    }

    /// One fixed response whatever the direction — a set whose lookup
    /// collapsed onto a single measurement.
    struct ConstantProvider;
    impl HrirProvider for ConstantProvider {
        fn render(&self, _az: f32, _el: f32, _fs: u32) -> HrirPair {
            let mut pair = HrirPair {
                left: [0.0; HRIR_LEN],
                right: [0.0; HRIR_LEN],
            };
            pair.left[3] = 0.5;
            pair.right[5] = 0.25;
            pair
        }
    }

    /// A silent set must be refused rather than handed to the render path,
    /// where it would mute everything but the LFE (issue #219).
    #[test]
    fn a_silent_set_is_refused() {
        let set = HrirSet::new(&SilentProvider, 48_000);
        assert_eq!(set.peak(), 0.0, "the fixture must really be silent");
        let err = check_loaded_set(&set, "silent.sofa", 7).expect_err("a silent set must not load");
        let msg = err.to_string();
        assert!(
            msg.contains("silent.sofa") && msg.contains("filter length 7"),
            "{msg}"
        );
    }

    /// A direction-invariant set is degenerate but audible: it must load (with
    /// a warning), not fail — refusing it would take sound away from a file
    /// the listener can still hear.
    #[test]
    fn a_direction_invariant_set_still_loads() {
        let set = HrirSet::new(&ConstantProvider, 48_000);
        assert!(set.is_direction_invariant());
        assert!(check_loaded_set(&set, "flat.sofa", 128).is_ok());
    }

    /// The guard must not reject a real set: the bundled KEMAR data is the
    /// reference for "this is what a usable set looks like".
    #[test]
    fn a_usable_set_passes_the_guard() {
        let set = HrirSet::new(&MeasuredHrirData::saf_kemar(), 48_000);
        assert!(check_loaded_set(&set, "kemar", 128).is_ok());
        assert!(!set.is_direction_invariant());
    }

    /// At the native rate the resample must be a strict no-op.
    #[test]
    fn resample_is_noop_at_native_rate() {
        let native = MeasuredHrirData::saf_kemar();
        let same = MeasuredHrirData::saf_kemar().resampled_to(48_000);
        let grid_a = HrirSet::new(&native, 48_000);
        let grid_b = HrirSet::new(&same, 48_000);
        let mut a = HrirPair {
            left: [0.0; HRIR_LEN],
            right: [0.0; HRIR_LEN],
        };
        let mut b = a.clone();
        grid_a.at(37.0, 12.0, &mut a);
        grid_b.at(37.0, 12.0, &mut b);
        assert_eq!(a.left, b.left);
        assert_eq!(a.right, b.right);
    }
}
