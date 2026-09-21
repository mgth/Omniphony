//! Binaural room impulse responses (BRIR): a measured room as raw kernel
//! pairs for the partitioned convolver.
//!
//! A BRIR set is the opposite of the free-field HRIR set the direct binaural
//! path uses. The HRIR loader ([`super::measured`]) keeps a few milliseconds
//! after the onset of each ear, makes it minimum-phase, supplies the
//! interaural delay analytically and synthesises the room separately. A room
//! response *is* the room: the propagation delay, the interaural delay, the
//! early reflections and the tail are the measurement, so this loader keeps
//! the pair intact and only:
//!
//! * removes the silence common to the whole set ahead of the earliest onset
//!   (the relative delays between emitters and ears survive);
//! * cuts each tail where its backward-integrated energy falls
//!   [`BrirLoadOptions::tail_floor_db`] below the total (Schroeder) and
//!   under [`BrirLoadOptions::max_length_s`], with a short raised-cosine fade;
//! * resamples to the engine rate with the crate's own windowed-sinc kernel
//!   (the SOFA reader's resampler works on a misread shape for
//!   `MultiSpeakerBRIR`, see below);
//! * normalises the set so the mean direct-sound energy is one, the same
//!   scale as an HRIR set, so switching between the two keeps the level.
//!
//! # Geometry
//!
//! The set is organised as **emitters × head orientations**. An emitter is a
//! virtual loudspeaker: the position a response was measured *from*,
//! relative to the listener, in the renderer's frame (`x` right, `y` front,
//! `z` up, metres). A head orientation is where the listener looked during
//! the measurement, as `(yaw, pitch)` in degrees with the renderer's sign
//! convention (yaw positive to the right, pitch positive up). Every SOFA
//! room convention maps onto that grid:
//!
//! * `MultiSpeakerBRIR` — `E` emitters per measurement (`Data.IR` is
//!   `[M][R][E][N]`), `M` head orientations in `ListenerView`;
//! * one-emitter conventions (`SingleRoomSRIR`, `SingleRoomDRIR`, or a
//!   `SimpleFreeFieldHRIR` that carries room-length responses, the ASH
//!   Toolset export): each measurement is one `(SourcePosition,
//!   ListenerView)`; distinct source positions are the emitters, distinct
//!   views the orientations.
//!
//! Every emitter must have been measured at every kept orientation.
//!
//! # SOFA shape
//!
//! `sofar` assumes `Data.IR` is `[M][R][N]` and takes the third axis for
//! `N`, so a `[M][R][E][N]` file reports `N = E` (issue #219). The shape is
//! therefore read from the HDF dataspace directly, and the reader is opened
//! at the file's own rate with normalisation off so its resampler and
//! normaliser never touch the misread arrays.

use rayon::prelude::*;

use super::hrir::HRIR_SPAN_S;
use super::measured::ResampleKernel;

/// Onset threshold relative to the response's peak (−40 dB).
const ONSET_FRAC: f32 = 0.01;
/// Samples kept ahead of the earliest onset of the set.
const LEAD_GUARD: usize = 32;
/// Raised-cosine fade at the tail cut, seconds.
const TAIL_FADE_S: f32 = 0.005;
/// Positions closer than this are the same emitter, metres.
const SAME_POINT_M: f32 = 0.01;
/// Orientations closer than this are the same, degrees.
const SAME_ANGLE_DEG: f32 = 0.05;
/// Below this peak a set is silence (the HRIR loader's bound).
const SILENT_PEAK: f32 = 1e-9;

/// Which measured head orientations to keep.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OrientationSelection {
    /// Every orientation in the file (head tracking over the full set).
    All,
    /// Only the orientation nearest to straight ahead — no head tracking,
    /// and the memory of a single orientation.
    FrontOnly,
    /// One orientation per `step_deg` of yaw within `±max_yaw_deg` (the
    /// nearest measured one each time), plus straight ahead.
    Decimated { step_deg: f32, max_yaw_deg: f32 },
}

/// Load-time choices for [`BrirSet::from_raw`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BrirLoadOptions {
    pub orientations: OrientationSelection,
    /// Upper bound on the kept response length in seconds, counted after
    /// the common lead is removed and including the fade. `0` = no bound.
    pub max_length_s: f32,
    /// Tail cut: decibels below the response's total energy at which the
    /// remaining tail is dropped (Schroeder backward integration).
    pub tail_floor_db: f32,
}

impl Default for BrirLoadOptions {
    fn default() -> Self {
        Self {
            orientations: OrientationSelection::All,
            max_length_s: 2.0,
            tail_floor_db: 60.0,
        }
    }
}

/// One measured pair, engine rate, equal lengths.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BrirPair {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl BrirPair {
    /// Kernel length in samples (both ears).
    pub fn taps(&self) -> usize {
        self.left.len()
    }
}

/// The raw arrays of a room-response SOFA file, decoupled from the reader so
/// the loader can be exercised in memory. Coordinates are cartesian in the
/// SOFA frame (`x` front, `y` left, `z` up, metres), as `sofar` delivers
/// them after opening.
#[derive(Clone, Copy, Debug)]
pub struct RawRoomIr<'a> {
    /// The file's `SOFAConventions` attribute (informational).
    pub conventions: &'a str,
    pub sample_rate: f32,
    /// Measurements, receivers, emitters, taps: the true `Data.IR` shape.
    pub m: usize,
    pub r: usize,
    pub e: usize,
    pub n: usize,
    /// `[M][C]` or `[I][C]`.
    pub source_position: &'a [f32],
    /// `[E][C][I]`, relative to the source.
    pub emitter_position: &'a [f32],
    /// `[M][C]` or `[I][C]`.
    pub listener_position: &'a [f32],
    /// `[M][C]` or `[I][C]`; a view vector.
    pub listener_view: &'a [f32],
    /// `[M][R][N]` (`E = 1`) or `[M][R][E][N]`.
    pub data_ir: &'a [f32],
    /// `[I][R]`, `[M][R]`, `[I][R][E]` or `[M][R][E]`; empty = none.
    pub data_delay: &'a [f32],
}

/// A loaded BRIR set: emitters × orientations kernel pairs at the engine
/// rate. Immutable once built; `Send + Sync` for the rebuild worker.
#[derive(Clone, Debug)]
pub struct BrirSet {
    sample_rate: u32,
    /// Virtual loudspeakers relative to the listener, renderer frame,
    /// metres.
    emitters: Vec<[f32; 3]>,
    /// `(yaw, pitch)` degrees, renderer convention, sorted by yaw then pitch.
    orientations: Vec<(f32, f32)>,
    /// `pairs[emitter * orientations.len() + orientation]`.
    pairs: Vec<BrirPair>,
    max_taps: usize,
    conventions: String,
}

/// Row `i` of a `[M][C]` or `[I][C]` array (the single row when the array
/// holds one), or the origin.
fn row3(arr: &[f32], i: usize) -> [f32; 3] {
    let at = |k: usize| [arr[k], arr[k + 1], arr[k + 2]];
    if arr.len() >= 3 * (i + 1) {
        at(3 * i)
    } else if arr.len() >= 3 {
        at(0)
    } else {
        [0.0; 3]
    }
}

/// SOFA cartesian (`x` front, `y` left, `z` up) → renderer (`x` right, `y`
/// front, `z` up).
fn to_renderer_frame(p: [f32; 3]) -> [f32; 3] {
    [-p[1], p[0], p[2]]
}

/// `(yaw, pitch)` in degrees, renderer convention, of a SOFA view vector.
/// A degenerate vector is straight ahead.
fn view_to_yaw_pitch(v: [f32; 3]) -> (f32, f32) {
    let horiz = (v[0] * v[0] + v[1] * v[1]).sqrt();
    if horiz + v[2].abs() < 1e-6 {
        return (0.0, 0.0);
    }
    // SOFA azimuth is counter-clockwise (left positive); ours is right positive.
    let yaw = (-v[1]).atan2(v[0]).to_degrees();
    let pitch = v[2].atan2(horiz).to_degrees();
    (wrap_deg(yaw), pitch)
}

/// Wrap into `(-180, 180]`.
fn wrap_deg(a: f32) -> f32 {
    let mut a = a.rem_euclid(360.0);
    if a > 180.0 {
        a -= 360.0;
    }
    a
}

/// Squared angular distance between two orientations, degrees².
fn orientation_dist2(a: (f32, f32), b: (f32, f32)) -> f32 {
    let dy = wrap_deg(a.0 - b.0);
    let dp = a.1 - b.1;
    dy * dy + dp * dp
}

/// First sample at or above `ONSET_FRAC` of the peak, or 0 for silence.
fn onset(ir: &[f32]) -> usize {
    let peak = ir.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
    if peak <= SILENT_PEAK {
        return 0;
    }
    let thresh = ONSET_FRAC * peak;
    ir.iter().position(|&x| x.abs() >= thresh).unwrap_or(0)
}

/// Length to keep so that the dropped tail holds less than `floor_db` below
/// the total energy (Schroeder backward integration). At least 1 for a
/// non-silent response.
fn tail_cut(ir: &[f32], floor_db: f32) -> usize {
    let total: f64 = ir.iter().map(|&x| (x as f64) * (x as f64)).sum();
    if total <= 0.0 {
        return 0;
    }
    let floor = total * 10f64.powf(-floor_db as f64 / 10.0);
    let mut acc = 0.0f64;
    for (k, &x) in ir.iter().enumerate().rev() {
        acc += (x as f64) * (x as f64);
        if acc > floor {
            return k + 1;
        }
    }
    1
}

/// Apply a raised-cosine fade over the last `fade` samples of `ir`.
fn fade_out(ir: &mut [f32], fade: usize) {
    let n = ir.len();
    let fade = fade.min(n);
    if fade < 2 {
        return;
    }
    for i in 0..fade {
        let t = (i + 1) as f32 / fade as f32;
        let w = 0.5 * (1.0 + (std::f32::consts::PI * t).cos());
        ir[n - fade + i] *= w;
    }
}

impl BrirSet {
    /// Build a set from raw SOFA arrays. Errors name the shape or geometry
    /// problem; a set that comes out silent is refused rather than rendered.
    pub fn from_raw(
        raw: &RawRoomIr<'_>,
        engine_rate: u32,
        opts: &BrirLoadOptions,
    ) -> anyhow::Result<Self> {
        let (m, r, e, n) = (raw.m, raw.r, raw.e, raw.n);
        if r < 2 {
            anyhow::bail!("{r} receiver(s); a binaural set needs the two ears");
        }
        if m == 0 || e == 0 || n == 0 {
            anyhow::bail!("empty set (M = {m}, E = {e}, N = {n})");
        }
        if raw.data_ir.len() != m * r * e * n {
            anyhow::bail!(
                "Data.IR holds {} values for M×R×E×N = {}×{}×{}×{}",
                raw.data_ir.len(),
                m,
                r,
                e,
                n
            );
        }
        if !raw.sample_rate.is_finite() || raw.sample_rate <= 0.0 {
            anyhow::bail!("invalid sampling rate {}", raw.sample_rate);
        }
        let file_rate = raw.sample_rate.round() as u32;
        if engine_rate == 0 {
            anyhow::bail!("engine rate is zero");
        }

        // --- geometry: (measurement, emitter slot) → (emitter, orientation)
        let mut emitters: Vec<[f32; 3]> = Vec::new();
        let mut orientations: Vec<(f32, f32)> = Vec::new();
        let mut find_or_push_emitter = |p: [f32; 3]| -> usize {
            if let Some(i) = emitters.iter().position(|q| {
                let d = [q[0] - p[0], q[1] - p[1], q[2] - p[2]];
                (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() <= SAME_POINT_M
            }) {
                i
            } else {
                emitters.push(p);
                emitters.len() - 1
            }
        };
        let mut find_or_push_orientation = |o: (f32, f32)| -> usize {
            if let Some(i) = orientations
                .iter()
                .position(|q| orientation_dist2(*q, o).sqrt() <= SAME_ANGLE_DEG)
            {
                i
            } else {
                orientations.push(o);
                orientations.len() - 1
            }
        };
        // Per (measurement, emitter slot): emitter index, orientation index.
        let mut meas: Vec<(usize, usize)> = Vec::with_capacity(m * e);
        for mi in 0..m {
            let listener = row3(raw.listener_position, mi);
            let source = row3(raw.source_position, mi);
            let view = row3(raw.listener_view, mi);
            let o = find_or_push_orientation(view_to_yaw_pitch(view));
            for k in 0..e {
                // The emitter is relative to the source; a one-emitter
                // convention leaves it at the source (zero), so the same sum
                // serves both.
                let em = row3(raw.emitter_position, k);
                let rel = [
                    source[0] + em[0] - listener[0],
                    source[1] + em[1] - listener[1],
                    source[2] + em[2] - listener[2],
                ];
                let ei = find_or_push_emitter(to_renderer_frame(rel));
                meas.push((ei, o));
            }
        }
        // Emitters at the origin cannot be a direction.
        if let Some(p) = emitters
            .iter()
            .find(|p| (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt() < SAME_POINT_M)
        {
            anyhow::bail!(
                "an emitter sits on the listener ({:.3}, {:.3}, {:.3}); no direction to render",
                p[0],
                p[1],
                p[2]
            );
        }

        // --- orientations: sort, then select
        let mut order: Vec<usize> = (0..orientations.len()).collect();
        order.sort_by(|&a, &b| {
            orientations[a]
                .partial_cmp(&orientations[b])
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let sorted: Vec<(f32, f32)> = order.iter().map(|&i| orientations[i]).collect();
        let mut remap = vec![0usize; orientations.len()];
        for (new, &old) in order.iter().enumerate() {
            remap[old] = new;
        }
        for (_, o) in meas.iter_mut() {
            *o = remap[*o];
        }
        let orientations = sorted;
        let kept = select_orientations(&orientations, opts.orientations);
        if kept.is_empty() {
            anyhow::bail!("no head orientation selected");
        }

        // --- completeness: one measurement per (emitter, kept orientation)
        let ne = emitters.len();
        let no = kept.len();
        let mut slot: Vec<Option<usize>> = vec![None; ne * no];
        for (idx, &(ei, oi)) in meas.iter().enumerate() {
            let Some(ki) = kept.iter().position(|&k| k == oi) else {
                continue;
            };
            let s = &mut slot[ei * no + ki];
            if s.is_none() {
                *s = Some(idx);
            } else {
                log::warn!(
                    "BRIR: emitter {ei} measured twice at orientation {:?}; keeping the first",
                    orientations[oi]
                );
            }
        }
        if let Some(missing) = slot.iter().position(Option::is_none) {
            let (ei, ki) = (missing / no, missing % no);
            anyhow::bail!(
                "emitter {ei} at ({:.2}, {:.2}, {:.2}) m has no measurement at orientation \
                 yaw {:.1}°, pitch {:.1}°",
                emitters[ei][0],
                emitters[ei][1],
                emitters[ei][2],
                orientations[kept[ki]].0,
                orientations[kept[ki]].1
            );
        }

        // --- extract the pairs (file rate), applying Data.Delay
        let delay_of = |mi: usize, ri: usize, k: usize| -> usize {
            let d = raw.data_delay;
            let v = if d.len() == m * r * e {
                d[(mi * r + ri) * e + k]
            } else if d.len() == r * e {
                d[ri * e + k]
            } else if d.len() == m * r {
                d[mi * r + ri]
            } else if d.len() == r {
                d[ri]
            } else {
                0.0
            };
            if v.is_finite() && v > 0.0 {
                v.round() as usize
            } else {
                0
            }
        };
        let extract = |mi: usize, ri: usize, k: usize| -> Vec<f32> {
            let base = ((mi * r + ri) * e + k) * n;
            let d = delay_of(mi, ri, k);
            let mut ir = vec![0.0f32; d + n];
            ir[d..].copy_from_slice(&raw.data_ir[base..base + n]);
            ir
        };
        let mut pairs: Vec<BrirPair> = Vec::with_capacity(ne * no);
        for s in &slot {
            let idx = s.expect("checked complete above");
            let (mi, k) = (idx / e, idx % e);
            pairs.push(BrirPair {
                left: extract(mi, 0, k),
                right: extract(mi, 1, k),
            });
        }

        // --- silence guard
        let peak = pairs
            .iter()
            .flat_map(|p| p.left.iter().chain(p.right.iter()))
            .fold(0.0f32, |m, &x| m.max(x.abs()));
        if peak <= SILENT_PEAK {
            anyhow::bail!("every response is silent (peak {peak:e})");
        }

        // --- common lead: keep the relative delays, drop the shared silence
        let lead = pairs
            .iter()
            .map(|p| onset(&p.left).min(onset(&p.right)))
            .min()
            .unwrap_or(0)
            .saturating_sub(LEAD_GUARD);

        // --- tail cut + fade, then resample
        let fade = (TAIL_FADE_S * file_rate as f32).round() as usize;
        let max_len = if opts.max_length_s > 0.0 {
            (opts.max_length_s * file_rate as f32).round() as usize
        } else {
            usize::MAX
        };
        let kernel =
            (file_rate != engine_rate).then(|| ResampleKernel::new(file_rate, engine_rate));
        let pairs: Vec<BrirPair> = pairs
            .into_par_iter()
            .map_init(Vec::new, |buf, mut p| {
                p.left.drain(..lead.min(p.left.len()));
                p.right.drain(..lead.min(p.right.len()));
                // The fade lies beyond the cut point, so what is faded is
                // already below the floor; a length bound is a hard limit
                // and may fade audible content instead.
                let cut = tail_cut(&p.left, opts.tail_floor_db)
                    .max(tail_cut(&p.right, opts.tail_floor_db));
                let keep = (cut + fade).min(max_len).max(1);
                p.left.resize(keep, 0.0);
                p.right.resize(keep, 0.0);
                fade_out(&mut p.left, fade);
                fade_out(&mut p.right, fade);
                if let Some(k) = &kernel {
                    k.resample_into(&p.left, buf);
                    p.left.clear();
                    p.left.extend_from_slice(buf);
                    k.resample_into(&p.right, buf);
                    p.right.clear();
                    p.right.extend_from_slice(buf);
                }
                p
            })
            .collect();

        // --- normalise: unit mean direct-sound energy (the HRIR scale)
        let window = (HRIR_SPAN_S * engine_rate as f32).ceil() as usize;
        let mut acc = 0.0f64;
        let mut count = 0usize;
        for p in &pairs {
            for ir in [&p.left, &p.right] {
                let start = onset(ir);
                let end = (start + window).min(ir.len());
                acc += ir[start..end]
                    .iter()
                    .map(|&x| (x as f64) * (x as f64))
                    .sum::<f64>();
                count += 1;
            }
        }
        let mean = if count > 0 { acc / count as f64 } else { 0.0 };
        let mut pairs = pairs;
        if mean > 0.0 {
            let gain = (1.0 / mean.sqrt()) as f32;
            for p in &mut pairs {
                for x in p.left.iter_mut().chain(p.right.iter_mut()) {
                    *x *= gain;
                }
            }
        }

        let max_taps = pairs.iter().map(BrirPair::taps).max().unwrap_or(0);
        let kept_orientations: Vec<(f32, f32)> = kept.iter().map(|&k| orientations[k]).collect();
        let set = Self {
            sample_rate: engine_rate,
            emitters,
            orientations: kept_orientations,
            pairs,
            max_taps,
            conventions: raw.conventions.to_string(),
        };
        log::info!(
            "BRIR: {} ({}): {} emitters × {} orientations, up to {} taps ({:.3} s) at {} Hz, {:.1} MiB",
            if raw.conventions.is_empty() {
                "unnamed convention"
            } else {
                raw.conventions
            },
            if file_rate == engine_rate {
                "native rate".to_string()
            } else {
                format!("resampled from {file_rate} Hz")
            },
            set.emitters.len(),
            set.orientations.len(),
            set.max_taps,
            set.max_taps as f32 / engine_rate as f32,
            engine_rate,
            set.bytes() as f32 / (1024.0 * 1024.0)
        );
        Ok(set)
    }

    /// Engine rate the pairs are at.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Virtual loudspeakers relative to the listener (renderer frame, metres).
    pub fn emitters(&self) -> &[[f32; 3]] {
        &self.emitters
    }

    /// Kept head orientations, `(yaw, pitch)` degrees, sorted.
    pub fn orientations(&self) -> &[(f32, f32)] {
        &self.orientations
    }

    /// The pair measured from emitter `e` at orientation `o`.
    pub fn pair(&self, e: usize, o: usize) -> &BrirPair {
        &self.pairs[e * self.orientations.len() + o]
    }

    /// Longest kernel of the set, in samples.
    pub fn max_taps(&self) -> usize {
        self.max_taps
    }

    /// The file's `SOFAConventions`, for status displays.
    pub fn conventions(&self) -> &str {
        &self.conventions
    }

    /// Index of the kept orientation nearest to `(yaw, pitch)` degrees
    /// (yaw wraps). Linear scan: at most a few hundred entries, evaluated
    /// once per head-pose update.
    pub fn nearest_orientation(&self, yaw_deg: f32, pitch_deg: f32) -> usize {
        let q = (yaw_deg, pitch_deg);
        let mut best = 0;
        let mut best_d = f32::INFINITY;
        for (i, &o) in self.orientations.iter().enumerate() {
            let d = orientation_dist2(o, q);
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        best
    }

    /// Resident size of the kernels, bytes.
    pub fn bytes(&self) -> usize {
        self.pairs
            .iter()
            .map(|p| (p.left.len() + p.right.len()) * std::mem::size_of::<f32>())
            .sum()
    }
}

/// Indices (into the sorted orientation list) to keep under `sel`.
fn select_orientations(orientations: &[(f32, f32)], sel: OrientationSelection) -> Vec<usize> {
    let nearest = |yaw: f32, pitch: f32| -> Option<usize> {
        orientations
            .iter()
            .enumerate()
            .map(|(i, &o)| (i, orientation_dist2(o, (yaw, pitch))))
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, _)| i)
    };
    match sel {
        OrientationSelection::All => (0..orientations.len()).collect(),
        OrientationSelection::FrontOnly => nearest(0.0, 0.0).into_iter().collect(),
        OrientationSelection::Decimated {
            step_deg,
            max_yaw_deg,
        } => {
            let step = step_deg.max(SAME_ANGLE_DEG);
            let max = max_yaw_deg.clamp(0.0, 180.0);
            let mut kept: Vec<usize> = Vec::new();
            let steps = (max / step).floor() as i32;
            for k in -steps..=steps {
                if let Some(i) = nearest(k as f32 * step, 0.0)
                    && !kept.contains(&i)
                {
                    kept.push(i);
                }
            }
            kept.sort_unstable();
            kept
        }
    }
}

#[cfg(feature = "sofa")]
impl BrirSet {
    /// Load a room-response SOFA file. The true `Data.IR` shape and the
    /// file's rate are read from the HDF structure first (see the module
    /// doc), then the arrays come from the reader opened at that rate with
    /// normalisation off.
    pub fn from_sofa(path: &str, engine_rate: u32, opts: &BrirLoadOptions) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path).map_err(|e| anyhow::anyhow!("read '{path}': {e}"))?;
        let (file_rate, shape) =
            sofa_ir_shape(&bytes).map_err(|e| anyhow::anyhow!("SOFA '{path}': {e}"))?;
        let mut open = sofar::reader::OpenOptions::new();
        open.sample_rate(file_rate).normalized(false);
        let sofa = open
            .open_data(&bytes)
            .map_err(|e| anyhow::anyhow!("open SOFA '{path}': {e:?}"))?;
        let h = sofa.hrtf();
        let conventions = h
            .attributes
            .get("SOFAConventions")
            .map(String::as_str)
            .unwrap_or("");
        let [m, r, e, n] = shape;
        let raw = RawRoomIr {
            conventions,
            sample_rate: file_rate,
            m,
            r,
            e,
            n,
            source_position: &h.source_position.values,
            emitter_position: &h.emitter_position.values,
            listener_position: &h.listener_position.values,
            listener_view: &h.listener_view.values,
            data_ir: &h.data_ir.values,
            data_delay: &h.data_delay.values,
        };
        Self::from_raw(&raw, engine_rate, opts).map_err(|e| anyhow::anyhow!("SOFA '{path}': {e}"))
    }
}

/// The file's sampling rate and the true `[M, R, E, N]` shape of `Data.IR`
/// (`E = 1` for a three-axis array), from the HDF structure.
#[cfg(feature = "sofa")]
fn sofa_ir_shape(bytes: &[u8]) -> anyhow::Result<(f32, [usize; 4])> {
    let parsed = sofar::hdf::parse_with_children(bytes)
        .map_err(|e| anyhow::anyhow!("not an HDF5/SOFA file: {e}"))?;
    let ir = parsed
        .get_child("Data.IR")
        .ok_or_else(|| anyhow::anyhow!("no Data.IR"))?
        .map_err(|e| anyhow::anyhow!("Data.IR: {e}"))?;
    let dims: Vec<usize> = ir.ds.dimension_size.iter().map(|&d| d as usize).collect();
    let shape = match dims.as_slice() {
        [m, r, n] => [*m, *r, 1, *n],
        [m, r, e, n] => [*m, *r, *e, *n],
        other => anyhow::bail!("Data.IR has {} axes, expected 3 or 4", other.len()),
    };
    let sr = parsed
        .get_child("Data.SamplingRate")
        .ok_or_else(|| anyhow::anyhow!("no Data.SamplingRate"))?
        .map_err(|e| anyhow::anyhow!("Data.SamplingRate: {e}"))?;
    let rate =
        hdf_first_float(&sr).ok_or_else(|| anyhow::anyhow!("unreadable Data.SamplingRate"))?;
    Ok((rate, shape))
}

/// First value of a floating-point HDF dataset (little-endian, as the SOFA
/// reader assumes).
#[cfg(feature = "sofa")]
fn hdf_first_float(obj: &sofar::hdf::DataObject) -> Option<f32> {
    if obj.dt.class_and_version & 0x0F != 1 {
        return None;
    }
    let precision = match obj.dt.data_fmt.as_ref() {
        Some(sofar::hdf::DataFormat::Float { bit_precision, .. }) => *bit_precision,
        _ => 64,
    };
    match precision {
        64 => obj
            .data
            .get(..8)
            .map(|b| f64::from_le_bytes(b.try_into().expect("8 bytes")) as f32),
        32 => obj
            .data
            .get(..4)
            .map(|b| f32::from_le_bytes(b.try_into().expect("4 bytes"))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SOFA spherical (az ccw-positive, el, r) → SOFA cartesian.
    fn sph(az_deg: f32, el_deg: f32, r: f32) -> [f32; 3] {
        let (az, el) = (az_deg.to_radians(), el_deg.to_radians());
        [
            r * el.cos() * az.cos(),
            r * el.cos() * az.sin(),
            r * el.sin(),
        ]
    }

    /// Marker amplitude identifying (emitter, orientation, ear).
    fn marker(e: usize, o: usize, ear: usize) -> f32 {
        let a = 0.5 + 0.01 * e as f32 + 0.001 * o as f32;
        if ear == 0 { a } else { -a }
    }

    /// A synthetic room response: impulse of `marker` amplitude at `delay`,
    /// then a decaying tail with `tail` peak amplitude.
    fn response(n: usize, delay: usize, amp: f32, tail: f32, seed: u32) -> Vec<f32> {
        let mut s = seed;
        let mut ir = vec![0.0f32; n];
        ir[delay] = amp;
        for (k, x) in ir.iter_mut().enumerate().skip(delay + 20) {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (s >> 8) as f32 / (1 << 24) as f32 * 2.0 - 1.0;
            let t = (k - delay) as f32 / n as f32;
            *x = tail * noise * (-8.0 * t).exp();
        }
        ir
    }

    struct Synth {
        m: usize,
        r: usize,
        e: usize,
        n: usize,
        source: Vec<f32>,
        emitter: Vec<f32>,
        listener: Vec<f32>,
        view: Vec<f32>,
        ir: Vec<f32>,
        delay: Vec<f32>,
        rate: f32,
        conventions: &'static str,
    }

    impl Synth {
        fn raw(&self) -> RawRoomIr<'_> {
            RawRoomIr {
                conventions: self.conventions,
                sample_rate: self.rate,
                m: self.m,
                r: self.r,
                e: self.e,
                n: self.n,
                source_position: &self.source,
                emitter_position: &self.emitter,
                listener_position: &self.listener,
                listener_view: &self.view,
                data_ir: &self.ir,
                data_delay: &self.delay,
            }
        }
    }

    /// `MultiSpeakerBRIR`-shaped: emitters at SOFA azimuths `spk_az` (2 m),
    /// listener views at SOFA azimuths `yaws`; direct sound at
    /// `base_delay + 10·e` samples, marker-coded.
    fn multi_speaker(spk_az: &[f32], yaws: &[f32], n: usize, rate: f32, tail: f32) -> Synth {
        let (m, r, e) = (yaws.len(), 2, spk_az.len());
        let base_delay = 100;
        let mut ir = vec![0.0f32; m * r * e * n];
        for (mi, _) in yaws.iter().enumerate() {
            for ri in 0..r {
                for (k, _) in spk_az.iter().enumerate() {
                    let base = ((mi * r + ri) * e + k) * n;
                    let resp = response(
                        n,
                        base_delay + 10 * k,
                        marker(k, mi, ri),
                        tail,
                        (mi * 7 + ri * 3 + k) as u32,
                    );
                    ir[base..base + n].copy_from_slice(&resp);
                }
            }
        }
        Synth {
            m,
            r,
            e,
            n,
            source: vec![0.0, 0.0, 0.0],
            emitter: spk_az.iter().flat_map(|&a| sph(a, 0.0, 2.0)).collect(),
            listener: vec![0.0, 0.0, 0.0],
            view: yaws.iter().flat_map(|&y| sph(y, 0.0, 1.0)).collect(),
            ir,
            delay: Vec::new(),
            rate,
            conventions: "MultiSpeakerBRIR",
        }
    }

    fn assert_close(a: f32, b: f32, tol: f32, what: &str) {
        assert!((a - b).abs() <= tol, "{what}: {a} vs {b}");
    }

    /// Marker position and amplitude of a pair's left ear (the largest
    /// sample), after normalisation is undone.
    fn peak_of(ir: &[f32]) -> (usize, f32) {
        ir.iter()
            .enumerate()
            .fold((0, 0.0f32), |(bi, bv), (i, &v)| {
                if v.abs() > bv.abs() { (i, v) } else { (bi, bv) }
            })
    }

    #[test]
    fn multi_speaker_brir_maps_emitters_and_orientations() {
        // L (+30 SOFA = left), R, C, and views −20, 0, +20 (SOFA, left +).
        let s = multi_speaker(&[30.0, -30.0, 0.0], &[-20.0, 0.0, 20.0], 400, 48000.0, 0.0);
        let set = BrirSet::from_raw(&s.raw(), 48000, &BrirLoadOptions::default()).unwrap();
        assert_eq!(set.sample_rate(), 48000);
        assert_eq!(set.emitters().len(), 3);
        assert_eq!(set.orientations().len(), 3);
        // Renderer frame: SOFA +30° (left) → x negative, y positive.
        let l = set.emitters()[0];
        assert!(
            l[0] < -0.9 && l[1] > 1.7,
            "left speaker in renderer frame: {l:?}"
        );
        let r = set.emitters()[1];
        assert!(
            r[0] > 0.9 && r[1] > 1.7,
            "right speaker in renderer frame: {r:?}"
        );
        let c = set.emitters()[2];
        assert_close(c[0], 0.0, 1e-4, "centre x");
        assert_close(c[1], 2.0, 1e-4, "centre y");
        // Views sorted by renderer yaw: SOFA +20 (left) → −20 here.
        let yaws: Vec<f32> = set.orientations().iter().map(|o| o.0).collect();
        assert_close(yaws[0], -20.0, 1e-3, "first yaw");
        assert_close(yaws[1], 0.0, 1e-3, "second yaw");
        assert_close(yaws[2], 20.0, 1e-3, "third yaw");
        // Orientation index 0 (renderer −20) is SOFA measurement index 2.
        // Pair (emitter 1, orientation 0) must carry marker(1, 2, ear).
        let p = set.pair(1, 0);
        let (il, vl) = peak_of(&p.left);
        let (ir_, vr) = peak_of(&p.right);
        assert_eq!(il, ir_, "ears keep their relative timing");
        assert_close(vl / -vr, 1.0, 1e-5, "right ear is the negated marker");
        let ratio = vl / marker(1, 2, 0);
        // Same normalisation gain for the whole set: check another pair.
        let (_, v2) = peak_of(&set.pair(2, 1).left);
        assert_close(v2 / marker(2, 1, 0), ratio, 1e-4, "uniform gain");
        // Relative delays survive: emitter 2 is 10 samples later than 1,
        // and the common lead is stripped down to the guard (emitter 0 is
        // the earliest).
        let (i2, _) = peak_of(&set.pair(2, 1).left);
        assert_eq!(i2, il + 10);
        let (i0, _) = peak_of(&set.pair(0, 2).left);
        assert_eq!(i0, LEAD_GUARD, "earliest onset lands at the guard");
        assert_eq!(il, LEAD_GUARD + 10);
    }

    #[test]
    fn nearest_orientation_wraps_around_yaw() {
        let s = multi_speaker(&[0.0], &[-170.0, 170.0, 0.0], 200, 48000.0, 0.0);
        let set = BrirSet::from_raw(&s.raw(), 48000, &BrirLoadOptions::default()).unwrap();
        // Sorted renderer yaws: −170 (SOFA +170), 0, 170 (SOFA −170).
        assert_eq!(set.nearest_orientation(179.0, 0.0), 2);
        assert_eq!(set.nearest_orientation(-179.0, 0.0), 0);
        assert_eq!(set.nearest_orientation(-100.0, 0.0), 0);
        assert_eq!(set.nearest_orientation(30.0, 0.0), 1);
    }

    #[test]
    fn front_only_keeps_the_orientation_nearest_straight_ahead() {
        let s = multi_speaker(&[30.0, -30.0], &[-20.0, -5.0, 20.0], 300, 48000.0, 0.0);
        let opts = BrirLoadOptions {
            orientations: OrientationSelection::FrontOnly,
            ..Default::default()
        };
        let set = BrirSet::from_raw(&s.raw(), 48000, &opts).unwrap();
        assert_eq!(set.orientations().len(), 1);
        assert_close(set.orientations()[0].0, 5.0, 1e-3, "SOFA −5 → renderer +5");
        // Only that orientation's measurement (SOFA index 1) is resident.
        let (_, v) = peak_of(&set.pair(0, 0).left);
        let (_, w) = peak_of(&set.pair(1, 0).left);
        assert_close(v / marker(0, 1, 0), w / marker(1, 1, 0), 1e-4, "same gain");
        let resident: usize = (0..2).map(|e| set.pair(e, 0).taps() * 2 * 4).sum();
        assert_eq!(set.bytes(), resident);
    }

    #[test]
    fn decimation_picks_nearest_measured_steps() {
        let yaws: Vec<f32> = (-9..=9).map(|k| k as f32 * 10.0).collect(); // −90..90 by 10
        let s = multi_speaker(&[0.0], &yaws, 200, 48000.0, 0.0);
        let opts = BrirLoadOptions {
            orientations: OrientationSelection::Decimated {
                step_deg: 30.0,
                max_yaw_deg: 60.0,
            },
            ..Default::default()
        };
        let set = BrirSet::from_raw(&s.raw(), 48000, &opts).unwrap();
        let kept: Vec<f32> = set.orientations().iter().map(|o| o.0.round()).collect();
        assert_eq!(kept, vec![-60.0, -30.0, 0.0, 30.0, 60.0]);
    }

    #[test]
    fn single_emitter_conventions_group_sources_and_views() {
        // DRIR-shaped: 4 sources, one view.
        let n = 300;
        let az = [30.0f32, -30.0, 0.0, 110.0];
        let mut ir = Vec::new();
        for (mi, _) in az.iter().enumerate() {
            for ri in 0..2 {
                ir.extend(response(n, 100 + 5 * mi, marker(mi, 0, ri), 0.0, mi as u32));
            }
        }
        let s = Synth {
            m: 4,
            r: 2,
            e: 1,
            n,
            source: az.iter().flat_map(|&a| sph(a, 0.0, 2.0)).collect(),
            emitter: vec![0.0, 0.0, 0.0],
            listener: vec![0.0, 0.0, 0.0],
            view: vec![1.0, 0.0, 0.0],
            ir,
            delay: Vec::new(),
            rate: 48000.0,
            conventions: "SingleRoomDRIR",
        };
        let set = BrirSet::from_raw(&s.raw(), 48000, &BrirLoadOptions::default()).unwrap();
        assert_eq!(set.emitters().len(), 4);
        assert_eq!(set.orientations(), &[(0.0, 0.0)]);
        let (i3, _) = peak_of(&set.pair(3, 0).left);
        let (i0, _) = peak_of(&set.pair(0, 0).left);
        assert_eq!(i3, i0 + 15);

        // SRIR-shaped: one source, 3 views.
        let views = [20.0f32, 0.0, -20.0];
        let mut ir = Vec::new();
        for (mi, _) in views.iter().enumerate() {
            for ri in 0..2 {
                ir.extend(response(n, 100, marker(0, mi, ri), 0.0, mi as u32));
            }
        }
        let s = Synth {
            m: 3,
            r: 2,
            e: 1,
            n,
            source: sph(30.0, 0.0, 2.0).to_vec(),
            emitter: vec![0.0, 0.0, 0.0],
            listener: vec![0.0, 0.0, 0.0],
            view: views.iter().flat_map(|&y| sph(y, 0.0, 1.0)).collect(),
            ir,
            delay: Vec::new(),
            rate: 48000.0,
            conventions: "SingleRoomSRIR",
        };
        let set = BrirSet::from_raw(&s.raw(), 48000, &BrirLoadOptions::default()).unwrap();
        assert_eq!(set.emitters().len(), 1);
        assert_eq!(set.orientations().len(), 3);
        // Renderer yaw +20 = SOFA −20 = measurement 2.
        let (_, v) = peak_of(&set.pair(0, 2).left);
        let (_, w) = peak_of(&set.pair(0, 0).left);
        assert_close(
            v / marker(0, 2, 0),
            w / marker(0, 0, 0),
            1e-4,
            "orientation mapping",
        );
    }

    #[test]
    fn tail_is_cut_at_the_floor_and_bounded() {
        // A 1 s response whose tail decays 8 nepers over its length: the −60 dB
        // point of the energy sits well inside.
        let s = multi_speaker(&[0.0], &[0.0], 48000, 48000.0, 0.3);
        let opts = BrirLoadOptions {
            max_length_s: 0.0,
            tail_floor_db: 60.0,
            orientations: OrientationSelection::All,
        };
        let set = BrirSet::from_raw(&s.raw(), 48000, &opts).unwrap();
        let taps = set.max_taps();
        assert!(taps < 48000 - 100, "tail cut shortens the response: {taps}");
        assert!(taps > 10000, "but keeps the audible decay: {taps}");
        let tail = &set.pair(0, 0).left;
        assert_eq!(
            *tail.last().unwrap(),
            0.0,
            "raised-cosine fade ends at zero"
        );
        // A tighter floor keeps less; a length bound wins when lower.
        let opts_40 = BrirLoadOptions {
            tail_floor_db: 40.0,
            ..opts
        };
        let shorter = BrirSet::from_raw(&s.raw(), 48000, &opts_40).unwrap();
        assert!(shorter.max_taps() < taps);
        let bounded = BrirLoadOptions {
            max_length_s: 0.1,
            ..opts
        };
        let set = BrirSet::from_raw(&s.raw(), 48000, &bounded).unwrap();
        assert_eq!(set.max_taps(), 4800);
    }

    #[test]
    fn resamples_to_the_engine_rate() {
        let s = multi_speaker(&[0.0, 90.0], &[0.0], 4410, 44100.0, 0.0);
        let set = BrirSet::from_raw(&s.raw(), 48000, &BrirLoadOptions::default()).unwrap();
        assert_eq!(set.sample_rate(), 48000);
        // The direct sound of emitter 1 (110 samples at 44.1 k) lands at the
        // guard + 10 samples scaled by 48/44.1.
        let (i0, _) = peak_of(&set.pair(0, 0).left);
        let (i1, _) = peak_of(&set.pair(1, 0).left);
        let want = ((LEAD_GUARD + 10) as f32 * 48000.0 / 44100.0).round() as usize;
        assert!((i1 as isize - want as isize).abs() <= 1, "{i1} vs {want}");
        assert!(
            (i0 as isize - (LEAD_GUARD as f32 * 48000.0 / 44100.0).round() as isize).abs() <= 1
        );
        // Pairs keep their own lengths; the later emitter is the longest.
        assert_eq!(set.max_taps(), set.pair(1, 0).taps());
        assert!(set.pair(0, 0).taps() < set.max_taps());
    }

    #[test]
    fn data_delay_shifts_each_receiver() {
        let mut s = multi_speaker(&[0.0], &[0.0], 300, 48000.0, 0.0);
        // [I][R][E]: right ear delayed by 7 samples.
        s.delay = vec![0.0, 7.0];
        let set = BrirSet::from_raw(&s.raw(), 48000, &BrirLoadOptions::default()).unwrap();
        let p = set.pair(0, 0);
        let (il, _) = peak_of(&p.left);
        let (ir_, _) = peak_of(&p.right);
        assert_eq!(ir_, il + 7);
    }

    #[test]
    fn silent_and_incomplete_sets_are_refused() {
        let mut s = multi_speaker(&[0.0, 30.0], &[0.0, 10.0], 200, 48000.0, 0.0);
        s.ir.iter_mut().for_each(|x| *x = 0.0);
        let err = BrirSet::from_raw(&s.raw(), 48000, &BrirLoadOptions::default()).unwrap_err();
        assert!(err.to_string().contains("silent"), "{err}");

        // One-emitter set where measurement views and sources vary jointly:
        // source A at view 0, source B at view 10 → B lacks view 0.
        let n = 200;
        let mut ir = Vec::new();
        for mi in 0..2 {
            for ri in 0..2 {
                ir.extend(response(n, 50, marker(mi, mi, ri), 0.0, mi as u32));
            }
        }
        let s = Synth {
            m: 2,
            r: 2,
            e: 1,
            n,
            source: [sph(30.0, 0.0, 2.0), sph(-30.0, 0.0, 2.0)].concat(),
            emitter: vec![0.0; 3],
            listener: vec![0.0; 3],
            view: [sph(0.0, 0.0, 1.0), sph(10.0, 0.0, 1.0)].concat(),
            ir,
            delay: Vec::new(),
            rate: 48000.0,
            conventions: "",
        };
        let err = BrirSet::from_raw(&s.raw(), 48000, &BrirLoadOptions::default()).unwrap_err();
        assert!(err.to_string().contains("no measurement"), "{err}");

        // Wrong element count.
        let mut s = multi_speaker(&[0.0], &[0.0], 300, 48000.0, 0.0);
        s.ir.pop();
        let err = BrirSet::from_raw(&s.raw(), 48000, &BrirLoadOptions::default()).unwrap_err();
        assert!(err.to_string().contains("Data.IR holds"), "{err}");
    }

    /// End-to-end through the SOFA reader on files generated outside the
    /// repo (see the loader's PR): skipped unless `BRIR_SOFA_DIR` points at
    /// a directory holding `msbrir.sofa` (48 k), `msbrir44.sofa` (44.1 k),
    /// `srir.sofa` and `longhrir.sofa`, all 5 emitters or views at
    /// 30/−30/0/110/−110° or −20..20°.
    #[cfg(feature = "sofa")]
    #[test]
    fn loads_generated_sofa_files() {
        let Some(dir) = std::env::var_os("BRIR_SOFA_DIR") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let load = |name: &str| {
            BrirSet::from_sofa(
                dir.join(name).to_str().unwrap(),
                48000,
                &BrirLoadOptions::default(),
            )
            .unwrap_or_else(|e| panic!("{name}: {e}"))
        };
        let ms = load("msbrir.sofa");
        assert_eq!(ms.conventions(), "MultiSpeakerBRIR");
        assert_eq!(ms.emitters().len(), 5);
        assert_eq!(ms.orientations().len(), 5);
        assert!(
            ms.max_taps() > 4800 && ms.max_taps() < 24000,
            "{}",
            ms.max_taps()
        );
        // Emitter 0 is the left speaker (SOFA +30°): renderer x < 0.
        assert!(ms.emitters()[0][0] < -0.9, "{:?}", ms.emitters()[0]);
        // Left-ear marker of (emitter 1, SOFA view index 0 = renderer +20 =
        // orientation 4) is 0.51 × ILD; the right ear is negative.
        let p = ms.pair(1, 4);
        let (il, vl) = peak_of(&p.left);
        let (_, vr) = peak_of(&p.right);
        assert!(vl > 0.0 && vr < 0.0, "ears: {vl} / {vr}");
        assert_eq!(il, LEAD_GUARD);

        let ms44 = load("msbrir44.sofa");
        assert_eq!(ms44.sample_rate(), 48000);
        assert!((ms44.max_taps() as f32 / ms.max_taps() as f32 - 1.0).abs() < 0.02);

        let sr = load("srir.sofa");
        assert_eq!(sr.emitters().len(), 1);
        assert_eq!(sr.orientations().len(), 5);

        let lh = load("longhrir.sofa");
        assert_eq!(lh.emitters().len(), 5);
        assert_eq!(lh.orientations().len(), 1);
    }
}
