//! Measured HRIR sets (e.g. the embedded SAF KEMAR data, or a loaded SOFA file).
//!
//! A [`MeasuredHrirData`] holds scattered-direction impulse responses. It
//! implements [`HrirProvider`] by blending the three measurements whose
//! spherical triangle contains the query — the measurement directions are
//! triangulated once by the convex hull of their unit vectors, and the
//! weights are the non-negative solution of `V·w = q` normalised to one
//! (the VBAP gains of that triangle, as barycentric coordinates on the
//! sphere) — with per-ear energy compensation, after truncation to
//! [`HRIR_LEN`], so it plugs straight into
//! [`HrirSet::new`](super::hrir::HrirSet::new) and reuses the regular-grid
//! bilinear interpolation. The three *nearest* measurements it used to blend
//! (inverse-angle weights) can all lie on one side of the query, which
//! extrapolates rather than interpolates; a set too small or too flat to
//! triangulate still falls back to them.
//!
//! Every stored impulse response is **minimum-phase** (see
//! [`minimum_phase`]): the magnitude response of the measurement is kept
//! exactly, and all of its energy is pulled to the start. That is what makes
//! the set interpolable — blending two responses whose onsets differ by a few
//! samples combs their shared content instead of averaging it, and the
//! threshold-based onset detection this replaced left up to seven samples of
//! interaural lag on the shadowed ear (see `docs/dsp-validation-report.md`).
//! The interaural delay itself is supplied analytically ([`super::itd`]), so
//! nothing of the measurement's phase is needed beyond its magnitude.

use realfft::RealFftPlanner;
use realfft::num_complex::Complex;

use super::hrir::{HRIR_LEN, HrirPair, HrirProvider};

/// Embedded SAF default HRIRs: Genelec Aural ID of a KEMAR dummy head @48 kHz,
/// ISC-licensed (© 2020 Leo McCormack; data by Aki Mäkivirta & Jaan Johansson).
/// Pre-aligned and truncated to `HRIR_LEN` by `tools/gen_saf_hrir.py`.
static SAF_KEMAR_BLOB: &[u8] = include_bytes!("data/saf_kemar.bin");

const BLOB_MAGIC: u32 = 0x4F48_4952; // 'OHIR'

/// Onset detection / alignment parameters (mirror the generator's). Kept for
/// responses that reach [`align_into`] without going through
/// [`MeasuredHrirData`] — a minimum-phase response has its onset at zero and
/// passes through unchanged.
const PRE_SAMPLES: usize = 8;
const ONSET_FRAC: f32 = 0.15;

/// Magnitude floor of the minimum-phase reconstruction, relative to the
/// spectral peak (−100 dB): keeps the log finite where a measurement has no
/// energy (a band-limited set at Nyquist) without shaping anything audible.
const MIN_PHASE_FLOOR: f64 = 1e-5;

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
    /// Spherical triangulation of the measurement directions (convex hull
    /// faces, indices into `vecs`), empty when the set cannot be
    /// triangulated.
    tri: Vec<[usize; 3]>,
    /// Per-triangle inverse of the 3×3 matrix whose columns are its vertex
    /// directions, row-major: `w = inv · q` are the query's weights.
    tri_inv: Vec<[f32; 9]>,
    /// Triangles incident to each vertex: where a query's search starts.
    vert_tris: Vec<Vec<u32>>,
}

/// Inverse of a row-major 3×3 matrix, or `None` when singular.
fn inv3x3(m: &[f32; 9]) -> Option<[f32; 9]> {
    let det = m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6])
        + m[2] * (m[3] * m[7] - m[4] * m[6]);
    if det.abs() < 1e-9 {
        return None;
    }
    let d = 1.0 / det;
    Some([
        (m[4] * m[8] - m[5] * m[7]) * d,
        (m[2] * m[7] - m[1] * m[8]) * d,
        (m[1] * m[5] - m[2] * m[4]) * d,
        (m[5] * m[6] - m[3] * m[8]) * d,
        (m[0] * m[8] - m[2] * m[6]) * d,
        (m[2] * m[3] - m[0] * m[5]) * d,
        (m[3] * m[7] - m[4] * m[6]) * d,
        (m[1] * m[6] - m[0] * m[7]) * d,
        (m[0] * m[4] - m[1] * m[3]) * d,
    ])
}

/// Triangulate unit directions by their convex hull and precompute, per face,
/// the inverse that turns a query direction into its three weights.
fn triangulate(vecs: &[[f32; 3]]) -> (Vec<[usize; 3]>, Vec<[f32; 9]>, Vec<Vec<u32>>) {
    let pts: Vec<[f64; 3]> = vecs
        .iter()
        .map(|v| [v[0] as f64, v[1] as f64, v[2] as f64])
        .collect();
    let Some(faces) = crate::spatial_vbap::convhull::convhull_3d_build(&pts) else {
        return (Vec::new(), Vec::new(), Vec::new());
    };
    let mut tri = Vec::with_capacity(faces.len());
    let mut tri_inv = Vec::with_capacity(faces.len());
    let mut vert_tris = vec![Vec::new(); vecs.len()];
    for f in faces {
        let (a, b, c) = (vecs[f[0]], vecs[f[1]], vecs[f[2]]);
        // Columns are the vertex directions: V·w = q.
        let m = [a[0], b[0], c[0], a[1], b[1], c[1], a[2], b[2], c[2]];
        let Some(inv) = inv3x3(&m) else { continue };
        let t = tri.len() as u32;
        tri.push(f);
        tri_inv.push(inv);
        for &v in &f {
            vert_tris[v].push(t);
        }
    }
    (tri, tri_inv, vert_tris)
}

impl MeasuredHrirData {
    /// Build from raw measurements. `dirs[i]` corresponds to `irs[i]`. Every
    /// response is converted to minimum phase on the way in (see the module
    /// doc); the measurement's own bulk delay, and any pre-alignment it was
    /// given, are discarded.
    pub fn new(sample_rate: u32, dirs: Vec<(f32, f32)>, irs: Vec<(Vec<f32>, Vec<f32>)>) -> Self {
        let vecs: Vec<[f32; 3]> = dirs.iter().map(|&(az, el)| dir_vec(az, el)).collect();
        let irs = irs
            .into_iter()
            .map(|(l, r)| (minimum_phase(&l), minimum_phase(&r)))
            .collect();
        let (tri, tri_inv, vert_tris) = triangulate(&vecs);
        Self {
            sample_rate,
            dirs,
            vecs,
            irs,
            tri,
            tri_inv,
            vert_tris,
        }
    }

    /// Whether the set is interpolated over its spherical triangulation
    /// (false: too few or too flat a set of directions — nearest-three).
    pub fn is_triangulated(&self) -> bool {
        !self.tri.is_empty()
    }

    /// The embedded SAF KEMAR set, freshly parsed (minimum-phase
    /// reconstruction and triangulation included). Prefer
    /// [`saf_kemar_shared`](Self::saf_kemar_shared) unless an owned set is
    /// needed.
    pub fn saf_kemar() -> Self {
        Self::from_blob(SAF_KEMAR_BLOB).expect("embedded SAF KEMAR blob is valid")
    }

    /// The embedded SAF KEMAR set at `sample_rate`, parsed once per process
    /// and rate and shared. Parsing the blob is the expensive part of a
    /// KEMAR grid build — 1 672 minimum-phase reconstructions, the
    /// resampling, the hull of 836 directions — and it is identical every
    /// time; only the grid interpolation depends on anything else. A
    /// switch back to KEMAR, or the fallback after a failed SOFA load, then
    /// costs the interpolation alone.
    pub fn saf_kemar_shared(sample_rate: u32) -> std::sync::Arc<Self> {
        static CACHE: std::sync::Mutex<Vec<(u32, std::sync::Arc<MeasuredHrirData>)>> =
            std::sync::Mutex::new(Vec::new());
        let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, set)) = cache.iter().find(|(fs, _)| *fs == sample_rate) {
            return std::sync::Arc::clone(set);
        }
        let set = std::sync::Arc::new(Self::saf_kemar().resampled_to(sample_rate));
        cache.push((sample_rate, std::sync::Arc::clone(&set)));
        set
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
    ///
    /// Resampling a minimum-phase response leaves it only approximately so
    /// (the interpolation kernel is linear-phase), hence the reconstruction
    /// runs again on the result.
    pub fn resampled_to(self, target: u32) -> Self {
        if self.sample_rate == target {
            return self;
        }
        let from = self.sample_rate;
        let irs = self
            .irs
            .iter()
            .map(|(l, r)| {
                (
                    minimum_phase(&resample_ir(l, from, target)),
                    minimum_phase(&resample_ir(r, from, target)),
                )
            })
            .collect();
        Self {
            sample_rate: target,
            dirs: self.dirs,
            vecs: self.vecs,
            irs,
            tri: self.tri,
            tri_inv: self.tri_inv,
            vert_tris: self.vert_tris,
        }
    }

    /// Weights of a query inside triangle `t`, if it lies inside (all
    /// non-negative, within a small tolerance for queries on an edge).
    fn weights_in(&self, t: usize, q: [f32; 3]) -> Option<[f32; 3]> {
        let m = &self.tri_inv[t];
        let w = [
            m[0] * q[0] + m[1] * q[1] + m[2] * q[2],
            m[3] * q[0] + m[4] * q[1] + m[5] * q[2],
            m[6] * q[0] + m[7] * q[1] + m[8] * q[2],
        ];
        const EPS: f32 = -1e-4;
        if w.iter().all(|&x| x >= EPS) {
            let sum: f32 = w.iter().map(|x| x.max(0.0)).sum();
            if sum > 1e-9 {
                return Some([
                    w[0].max(0.0) / sum,
                    w[1].max(0.0) / sum,
                    w[2].max(0.0) / sum,
                ]);
            }
        }
        None
    }

    /// The measurements a query direction is blended from, with their
    /// weights (sum 1): the vertices of the spherical triangle that contains
    /// it, searched first among the triangles around the nearest
    /// measurement, then everywhere; failing that (a set that could not be
    /// triangulated, or a query outside its hull), the three nearest by
    /// inverse angle, as before.
    pub fn support(&self, az_deg: f32, el_deg: f32) -> [(usize, f32); 3] {
        let near = self.nearest3(az_deg, el_deg);
        if !self.tri.is_empty() {
            let q = dir_vec(az_deg, el_deg);
            let local = self.vert_tris[near[0].0].iter().map(|&t| t as usize);
            let all = 0..self.tri.len();
            for t in local.chain(all) {
                if let Some(w) = self.weights_in(t, q) {
                    let f = self.tri[t];
                    return [(f[0], w[0]), (f[1], w[1]), (f[2], w[2])];
                }
            }
        }
        let mut weights = [0.0f32; 3];
        let mut wsum = 0.0f32;
        for (k, &(_, ang)) in near.iter().enumerate() {
            // Floored so a second measurement at (almost) the same direction —
            // a duplicated point, or one repeated at another radius — cannot
            // turn the weight infinite and the blend NaN.
            weights[k] = 1.0 / ang.max(1e-4);
            wsum += weights[k];
        }
        [
            (near[0].0, weights[0] / wsum),
            (near[1].0, weights[1] / wsum),
            (near[2].0, weights[2] / wsum),
        ]
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

        let mut aligned = [0.0f32; HRIR_LEN];
        let mut target_l = 0.0f32;
        let mut target_r = 0.0f32;
        for (idx, w) in self.support(az_deg, el_deg) {
            if w <= 0.0 {
                continue;
            }
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
/// `sample_rate`. Requires the `sofa` build feature.
///
/// The file's measurements are read **raw** and go through the same
/// [`MeasuredHrirData`] path as the embedded set — the three-nearest blend
/// over responses that have been time-aligned first. `sofar`'s own
/// interpolation is deliberately not used: it sums the neighbouring
/// responses as stored and averages their `Data.Delay` separately, which is
/// only sound when the delay actually lives in `Data.Delay`. In most of the
/// public sets (HUTUBS, ARI, CIPIC, the MIT KEMAR) it lives in the response
/// itself and `Data.Delay` is zero, so blending before aligning combs the
/// shared content of the neighbours. Aligning after the blend cannot undo
/// that.
#[cfg(feature = "sofa")]
pub fn hrir_set_from_sofa(
    path: &str,
    sample_rate: u32,
    diffuse_field_eq: bool,
) -> anyhow::Result<super::hrir::HrirSet> {
    use sofar::reader::OpenOptions;
    let mut opts = OpenOptions::new();
    opts.sample_rate(sample_rate as f32);
    let sofa = opts
        .open(path)
        .map_err(|e| anyhow::anyhow!("open SOFA '{path}': {e:?}"))?;
    let filter_len = sofa.filter_len();
    let data = MeasuredHrirData::from_sofa(&sofa, sample_rate)
        .map_err(|e| anyhow::anyhow!("SOFA '{path}': {e}"))?;
    let set = super::hrir::HrirSet::build(&data, sample_rate, diffuse_field_eq);
    check_loaded_set(&set, path, filter_len)?;
    Ok(set)
}

#[cfg(feature = "sofa")]
impl MeasuredHrirData {
    /// The raw measurements of an opened SOFA file: one `(left, right)` pair
    /// per source position, at the rate `sofar` was asked to deliver. Any
    /// `Data.Delay` is ignored on purpose — a pure delay is exactly what the
    /// alignment discards, and the interaural delay is supplied analytically.
    fn from_sofa(sofa: &sofar::reader::Sofar, sample_rate: u32) -> anyhow::Result<Self> {
        let hrtf = sofa.hrtf();
        let dims = hrtf.dimensions();
        let (m, r, n, c) = (
            dims.m as usize,
            dims.r as usize,
            dims.n as usize,
            dims.c as usize,
        );
        if r < 2 {
            anyhow::bail!("{r} receiver(s); a binaural set needs the two ears");
        }
        if m == 0 || n == 0 {
            anyhow::bail!("no measurements (M = {m}, N = {n})");
        }
        let pos = &hrtf.source_position.values;
        let ir = &hrtf.data_ir.values;
        if pos.len() < m * c || c < 3 {
            anyhow::bail!("SourcePosition holds {} values for M = {m}", pos.len());
        }
        if ir.len() < m * r * n {
            anyhow::bail!(
                "Data.IR holds {} values for M×R×N = {}",
                ir.len(),
                m * r * n
            );
        }
        let mut positions = Vec::with_capacity(m);
        let mut irs = Vec::with_capacity(m);
        for i in 0..m {
            positions.push([pos[i * c], pos[i * c + 1], pos[i * c + 2]]);
            let base = i * r * n;
            irs.push((
                ir[base..base + n].to_vec(),
                ir[base + n..base + 2 * n].to_vec(),
            ));
        }
        Ok(Self::from_sofa_measurements(sample_rate, &positions, irs))
    }
}

impl MeasuredHrirData {
    /// Build from measurements given in SOFA Cartesian coordinates (metres;
    /// `x` front, `y` left, `z` up), converted to the renderer's direction
    /// convention (`az` 0 = front, +`az` = right; `el` +90 = up).
    ///
    /// A set measured at several distances repeats each direction once per
    /// radius. Only the radius band of the median measurement is kept
    /// (within 10 %): the free-field set is a function of direction alone,
    /// and duplicated directions would otherwise pair up in the blend.
    pub fn from_sofa_measurements(
        sample_rate: u32,
        positions: &[[f32; 3]],
        irs: Vec<(Vec<f32>, Vec<f32>)>,
    ) -> Self {
        let radius = |p: &[f32; 3]| (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
        let mut radii: Vec<f32> = positions.iter().map(radius).collect();
        radii.sort_by(|a, b| a.total_cmp(b));
        let median = radii.get(radii.len() / 2).copied().unwrap_or(1.0);
        let keep = |p: &[f32; 3]| {
            let r = radius(p);
            median <= 0.0 || (r - median).abs() <= 0.1 * median
        };
        let mut dirs = Vec::with_capacity(positions.len());
        let mut kept = Vec::with_capacity(positions.len());
        for (p, pair) in positions.iter().zip(irs) {
            if !keep(p) {
                continue;
            }
            let (x, y, z) = (p[0], p[1], p[2]);
            // SOFA +y is the listener's left; the renderer's +az is right.
            let az = (-y).atan2(x).to_degrees().rem_euclid(360.0);
            let el = z.atan2((x * x + y * y).sqrt()).to_degrees();
            dirs.push((az, el));
            kept.push(pair);
        }
        if kept.len() != positions.len() {
            log::info!(
                "SOFA: kept {} of {} measurements at radius {median:.2} m (±10 %)",
                kept.len(),
                positions.len()
            );
        }
        Self::new(sample_rate, dirs, kept)
    }
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

/// Minimum-phase reconstruction of `ir` (real-cepstrum method): same
/// magnitude response, all energy pulled to the start, no bulk delay.
///
/// `ln|H|` is transformed to the cepstrum, folded onto the causal side
/// (`c[0]`, `2·c[n]` for `0 < n < M/2`, `c[M/2]`), transformed back and
/// exponentiated; the inverse transform of that spectrum is the minimum-phase
/// response. The transform size is sixteen times the response length so the
/// cepstrum does not alias onto itself. Build-time only (`f64`, four
/// transforms per response).
pub fn minimum_phase(ir: &[f32]) -> Vec<f32> {
    let n = ir.len();
    if n == 0 {
        return Vec::new();
    }
    let m = (16 * n).next_power_of_two().max(2048);
    let half = m / 2;
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(m);
    let ifft = planner.plan_fft_inverse(m);
    let scale = 1.0 / m as f64;

    let mut x = fft.make_input_vec();
    for (dst, &v) in x.iter_mut().zip(ir) {
        *dst = v as f64;
    }
    let mut spec = fft.make_output_vec();
    fft.process(&mut x, &mut spec).expect("forward FFT");

    let peak = spec.iter().map(|c| c.norm()).fold(0.0f64, f64::max);
    if peak <= 0.0 {
        return vec![0.0; n];
    }
    let floor = peak * MIN_PHASE_FLOOR;
    let mut log_mag: Vec<Complex<f64>> = spec
        .iter()
        .map(|c| Complex::new(c.norm().max(floor).ln(), 0.0))
        .collect();
    let mut cepstrum = ifft.make_output_vec();
    ifft.process(&mut log_mag, &mut cepstrum)
        .expect("inverse FFT of the log magnitude");

    let mut folded = fft.make_input_vec();
    folded[0] = cepstrum[0] * scale;
    for k in 1..half {
        folded[k] = 2.0 * cepstrum[k] * scale;
    }
    folded[half] = cepstrum[half] * scale;
    let mut log_h = fft.make_output_vec();
    fft.process(&mut folded, &mut log_h)
        .expect("forward FFT of the folded cepstrum");

    let mut h_min: Vec<Complex<f64>> = log_h.iter().map(|c| c.exp()).collect();
    // A real spectrum has real DC and Nyquist bins; the transforms above
    // leave rounding noise on them, which the real inverse refuses.
    h_min[0].im = 0.0;
    h_min[half].im = 0.0;
    let mut out = ifft.make_output_vec();
    ifft.process(&mut h_min, &mut out)
        .expect("inverse FFT of the minimum-phase spectrum");
    out.iter().take(n).map(|&v| (v * scale) as f32).collect()
}

/// Onset-align `ir` and copy `HRIR_LEN` taps into `out`. Idempotent for an
/// already-aligned IR (onset ≈ 0) — a minimum-phase response passes through
/// unchanged, so for [`MeasuredHrirData`] this is the truncation only.
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

    /// Magnitude at a few frequencies by direct projection (no FFT bin grid).
    fn magnitude_at(x: &[f32], f: f64, fs: f64) -> f64 {
        let (mut c, mut s) = (0.0f64, 0.0f64);
        for (i, &v) in x.iter().enumerate() {
            let ph = 2.0 * std::f64::consts::PI * f * i as f64 / fs;
            c += v as f64 * ph.cos();
            s += v as f64 * ph.sin();
        }
        (c * c + s * s).sqrt()
    }

    /// A delayed, decaying response with a couple of notches: the
    /// reconstruction must keep its magnitude and drop the delay.
    fn test_ir() -> Vec<f32> {
        let mut ir = vec![0.0f32; 128];
        for (i, v) in ir.iter_mut().enumerate().skip(23) {
            let t = (i - 23) as f32;
            *v = (-t / 12.0).exp() * (0.7 * t).cos() + 0.3 * (-t / 30.0).exp() * (2.1 * t).sin();
        }
        ir
    }

    #[test]
    fn minimum_phase_preserves_the_magnitude_response() {
        let ir = test_ir();
        let mp = minimum_phase(&ir);
        assert_eq!(mp.len(), ir.len());
        for f in [200.0, 1_000.0, 3_700.0, 8_000.0, 14_500.0, 21_000.0] {
            let a = magnitude_at(&ir, f, 48_000.0);
            let b = magnitude_at(&mp, f, 48_000.0);
            assert!(
                (a - b).abs() <= 2e-3 * a.max(1e-3),
                "magnitude moved at {f} Hz: {a} → {b}"
            );
        }
    }

    /// The defining property of a minimum-phase response: among all responses
    /// with the same magnitude, its partial energy `Σ_{k≤n} h[k]²` is the
    /// largest at every `n`. The original is silent for 23 samples, so the
    /// reconstruction leads it from the very first tap.
    #[test]
    fn minimum_phase_pulls_the_energy_to_the_start() {
        let ir = test_ir();
        let mp = minimum_phase(&ir);
        let total: f32 = ir.iter().map(|x| x * x).sum();
        let (mut acc_ir, mut acc_mp) = (0.0f32, 0.0f32);
        for (n, (a, b)) in ir.iter().zip(&mp).enumerate() {
            acc_ir += a * a;
            acc_mp += b * b;
            assert!(
                acc_mp >= acc_ir - 1e-3 * total,
                "partial energy fell behind the original at tap {n}: {acc_mp} < {acc_ir}"
            );
        }
        assert!(mp[0].abs() > 0.1, "no energy at the origin: {}", mp[0]);
        let head: f32 = mp[..24].iter().map(|x| x * x).sum();
        assert!(
            head > 0.5 * total,
            "energy not front-loaded: {head}/{total}"
        );
    }

    #[test]
    fn minimum_phase_of_a_delayed_impulse_is_an_impulse() {
        let mut ir = vec![0.0f32; 64];
        ir[17] = 0.5;
        let mp = minimum_phase(&ir);
        assert!((mp[0] - 0.5).abs() < 1e-4, "peak not at zero: {}", mp[0]);
        assert!(mp[1..].iter().all(|x| x.abs() < 1e-4), "not an impulse");
    }

    /// The stored set carries no bulk delay: the peak of every response is at
    /// the origin. That is the property the three-nearest blend relies on.
    #[test]
    fn stored_kemar_responses_start_at_the_origin() {
        let d = MeasuredHrirData::saf_kemar();
        let late = d
            .irs
            .iter()
            .flat_map(|(l, r)| [l, r])
            .filter(|ir| {
                let peak = ir.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
                ir.iter().position(|&x| x.abs() >= 0.5 * peak).unwrap_or(0) > 2
            })
            .count();
        assert_eq!(
            late, 0,
            "{late} responses reach half their peak after tap 2"
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

    /// A small full-sphere set: the six axis directions and the eight
    /// octant diagonals (an octahedron with its faces capped).
    fn octant_set() -> MeasuredHrirData {
        let mut dirs = vec![
            (0.0f32, 0.0f32),
            (90.0, 0.0),
            (180.0, 0.0),
            (270.0, 0.0),
            (0.0, 90.0),
            (0.0, -90.0),
        ];
        for az in [45.0f32, 135.0, 225.0, 315.0] {
            for el in [35.264f32, -35.264] {
                dirs.push((az, el));
            }
        }
        let irs = (0..dirs.len())
            .map(|k| {
                let mut v = vec![0.0f32; 32];
                v[0] = 1.0 + 0.1 * k as f32;
                (v.clone(), v)
            })
            .collect();
        MeasuredHrirData::new(48_000, dirs, irs)
    }

    /// Weights are a partition of unity, non-negative, and come from the
    /// triangle that actually contains the query: on a vertex the whole
    /// weight is that vertex; on an edge only its two ends carry weight.
    #[test]
    fn spherical_barycentric_weights_partition_unity() {
        let d = octant_set();
        assert!(d.is_triangulated());
        for (az, el) in [
            (10.0f32, 5.0f32),
            (100.0, -20.0),
            (200.0, 60.0),
            (300.0, -70.0),
        ] {
            let w = d.support(az, el);
            let sum: f32 = w.iter().map(|(_, x)| x).sum();
            assert!((sum - 1.0).abs() < 1e-5, "({az},{el}) sum {sum}");
            assert!(
                w.iter().all(|(_, x)| *x >= 0.0),
                "({az},{el}) negative weight: {w:?}"
            );
        }
        // (45°, 0°) is the centre of the rhombus front / upper diagonal /
        // right / lower diagonal, which the hull splits along one of its two
        // diagonals: the query sits on that edge and weighs its two ends,
        // half each, and nothing else.
        let w = d.support(45.0, 0.0);
        let mut active: Vec<(usize, f32)> = w.iter().copied().filter(|(_, x)| *x > 1e-4).collect();
        active.sort_by_key(|(i, _)| *i);
        let ids: Vec<usize> = active.iter().map(|(i, _)| *i).collect();
        assert!(
            ids == vec![0, 1] || ids == vec![6, 7],
            "edge midpoint must weigh its two ends only: {w:?}"
        );
        assert!(active.iter().all(|(_, x)| (x - 0.5).abs() < 1e-3), "{w:?}");
        // Exactly on a vertex: that vertex alone.
        let w = d.support(180.0, 0.0);
        let top = w.iter().max_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
        assert_eq!(top.0, 2);
        assert!(top.1 > 0.999);
    }

    /// The three nearest measurements can all lie on one side of the query;
    /// the triangle containing it never does. A query between the front
    /// vertex and the octant diagonals must draw on the front vertex, which
    /// a nearest-three pick around a diagonal cluster could skip.
    #[test]
    fn support_is_the_containing_triangle_not_the_nearest_cluster() {
        let d = octant_set();
        // Just off the front vertex, toward the upper-right octant diagonal.
        let w = d.support(4.0, 3.0);
        let front = w
            .iter()
            .find(|(i, _)| *i == 0)
            .map(|(_, x)| *x)
            .unwrap_or(0.0);
        assert!(front > 0.8, "front vertex should dominate: {w:?}");
    }

    /// A set too small to triangulate keeps the nearest-three fallback.
    #[test]
    fn a_tiny_set_falls_back_to_nearest_three() {
        let dirs = vec![(0.0f32, 0.0f32), (10.0, 0.0), (0.0, 10.0)];
        let irs = (0..3)
            .map(|k| {
                let mut v = vec![0.0f32; 16];
                v[0] = 1.0 + k as f32;
                (v.clone(), v)
            })
            .collect();
        let d = MeasuredHrirData::new(48_000, dirs, irs);
        assert!(!d.is_triangulated());
        let p = d.render(5.0, 5.0, 48_000);
        assert!(p.left.iter().all(|x| x.is_finite()) && p.left[0] > 0.0);
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
        let support = d.support(az0 + 2.0, el0 + 2.0);
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
        for &(idx, w) in &support {
            if w <= 0.0 {
                continue;
            }
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

    /// SOFA Cartesian (x front, y left, z up) lands on the renderer's
    /// convention (+az right, +el up), and a set measured at two distances
    /// keeps one radius band only.
    #[test]
    fn sofa_measurements_map_to_renderer_directions_and_one_radius() {
        let ir = |k: usize| {
            let mut v = vec![0.0f32; 32];
            v[k] = 1.0;
            (v.clone(), v)
        };
        let positions = [
            [1.0f32, 0.0, 0.0], // front
            [0.0, -1.0, 0.0],   // SOFA right (−y)
            [0.0, 1.0, 0.0],    // SOFA left (+y)
            [0.0, 0.0, 1.0],    // up
            [-1.0, 0.0, 0.0],   // back
            [3.0, 0.0, 0.0],    // front again, at 3 m: dropped
        ];
        let irs: Vec<_> = (0..positions.len()).map(|k| ir(k % 4)).collect();
        let d = MeasuredHrirData::from_sofa_measurements(48_000, &positions, irs);
        assert_eq!(d.len(), 5, "the 3 m duplicate must be dropped");
        let dir = |i: usize| (d.dirs[i].0.round(), d.dirs[i].1.round());
        assert_eq!(dir(0), (0.0, 0.0));
        assert_eq!(dir(1), (90.0, 0.0), "SOFA −y is the renderer's right");
        assert_eq!(dir(2), (270.0, 0.0));
        assert_eq!(dir(3).1, 90.0);
        assert_eq!(dir(4), (180.0, 0.0));
    }

    /// Two measurements at the same direction (one file, two radii within
    /// the band, or a plain duplicate) must not turn a blend weight infinite.
    #[test]
    fn a_duplicated_direction_does_not_poison_the_blend() {
        let mut dirs = vec![(0.0f32, 0.0f32), (10.0, 0.0), (20.0, 0.0), (0.0, 10.0)];
        dirs.push(dirs[1]); // exact duplicate of the 10° point
        let irs: Vec<_> = (0..dirs.len())
            .map(|k| {
                let mut v = vec![0.0f32; 32];
                v[2] = 0.5 + 0.1 * k as f32;
                (v.clone(), v)
            })
            .collect();
        let d = MeasuredHrirData::new(48_000, dirs, irs);
        // Query next to the duplicated point: it and its twin are both among
        // the three nearest, at an identical (tiny) angle.
        let p = d.render(10.5, 0.3, 48_000);
        assert!(
            p.left.iter().chain(p.right.iter()).all(|x| x.is_finite()),
            "blend produced a non-finite tap"
        );
        assert!(energy(&p.left) > 0.0);
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
