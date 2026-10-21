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
//!   views the orientations;
//! * a `GeneralFIR` set that moves its one emitter from measurement to
//!   measurement (`EmitterPosition` as `[E][C][M]`) and turns the head
//!   without a `ListenerView`, its ears alone recording where it looked (the
//!   IoSR listening room's 22.2 set): the head faces across its ears, and
//!   the set is turned so that its first measurement looks straight ahead.
//!
//! A set whose head orientations cover only an arc of 90° or more, leaving
//! the rest of the circle out, is turned to face the middle of that arc
//! instead, but only when a loudspeaker stands there and none straight ahead
//! of the file's frame (TU Berlin's Calypso set, measured from 0° to 180°
//! about its centre loudspeaker). Without that evidence the file's `+x` stays
//! the front: a set may well turn the head further to one side than the
//! other.
//!
//! Every emitter must have been measured at every kept orientation. A room
//! whose loudspeakers are the 24 of ITU-R BS.2051 Sound System H (22.2)
//! measures its two LFE loudspeakers too: those carry the LFE
//! ([`lfe_emitters`]), and nothing is panned to them.
//!
//! # SOFA shape
//!
//! `sofar` assumes `Data.IR` is `[M][R][N]` and takes the third axis for
//! `N`, so a `[M][R][E][N]` file reports `N = E` (issue #219). The file is
//! therefore opened without its responses (`LazySofa`, which neither
//! resamples nor normalises), the shape is read from the HDF dataspace, and
//! only the measurements the kept orientations come from are read: one head
//! orientation of a 274 MB set when the listener is not tracked.

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
    /// `[E][C][I]`, or `[E][C][M]` where each measurement places its
    /// emitters itself; relative to the source.
    pub emitter_position: &'a [f32],
    /// `[M][C]` or `[I][C]`.
    pub listener_position: &'a [f32],
    /// `[M][C]` or `[I][C]`; a view vector. Empty when the file has none:
    /// the ears then say where the head looked (see [`RoomPlan::new`]).
    pub listener_view: &'a [f32],
    /// `[I][C]` or `[M][C]`; empty = `z` is up.
    pub listener_up: &'a [f32],
    /// `[R][C][I]` or `[R][C][M]`, the left ear first.
    pub receiver_position: &'a [f32],
    /// `[M][R][N]` (`E = 1`) or `[M][R][E][N]`: every measurement, or the
    /// run from `ir_first` that [`ExtractedRoom::measurements`] names for a
    /// selection.
    pub data_ir: &'a [f32],
    /// The measurement `data_ir` starts at: 0 for the whole set.
    pub ir_first: usize,
    /// `[I][R]`, `[M][R]`, `[I][R][E]` or `[M][R][E]`; empty = none.
    pub data_delay: &'a [f32],
    /// The file's `RoomType` attribute, when it states one.
    pub room_type: Option<&'a str>,
    /// The room's corners around the listener, renderer frame, metres
    /// ([`room_corners_relative`]), when the file states them.
    pub room_corners: Option<[[f32; 3]; 2]>,
}

/// A loaded BRIR set: emitters × orientations kernel pairs at the engine
/// rate. Immutable once built; `Send + Sync` for the rebuild worker.
#[derive(Clone, Debug)]
pub struct BrirSet {
    sample_rate: u32,
    /// Virtual loudspeakers relative to the listener, renderer frame,
    /// metres.
    emitters: Vec<[f32; 3]>,
    /// Per emitter: one of the room's LFE loudspeakers ([`lfe_emitters`]).
    lfe: Vec<bool>,
    /// `(yaw, pitch)` degrees, renderer convention, sorted by yaw then pitch.
    orientations: Vec<(f32, f32)>,
    /// `pairs[emitter * orientations.len() + orientation]`.
    pairs: Vec<BrirPair>,
    max_taps: usize,
    conventions: String,
    /// The file's `RoomType` attribute (`shoebox`, `reverberant`, …), when
    /// it states one.
    room_type: Option<String>,
    /// The room's two opposite corners (`RoomCornerA`, `RoomCornerB`), when
    /// the file states them: relative to the listener, renderer frame,
    /// metres — the box the loudspeakers stand in.
    room_corners: Option<[[f32; 3]; 2]>,
}

/// The two corners of a SOFA shoebox room, SOFA frame, taken relative to
/// the listener and into the renderer's frame: the box a client draws the
/// set's loudspeakers in.
pub fn room_corners_relative(corners: [[f32; 3]; 2], listener: [f32; 3]) -> [[f32; 3]; 2] {
    let relative = |c: [f32; 3]| {
        omniphony_geometry::f32::sofa_to_adm([
            c[0] - listener[0],
            c[1] - listener[1],
            c[2] - listener[2],
        ])
    };
    [relative(corners[0]), relative(corners[1])]
}

/// The room a BRIR set's loudspeakers stand in, relative to the listener in
/// the renderer's frame (`x` right, `y` front, `z` up, metres): the box the
/// stage pans in while the set renders, in place of the user's room
/// ([`crate::live_params::RoomRatios`]). The file's two corners when it
/// states them (`RoomCornerA`/`RoomCornerB`), grown to contain every
/// loudspeaker so that each keeps its place in the panning space; else an
/// estimate from the loudspeakers' bounding box — a margin on every side, a
/// floor under the listener's ears and some headroom — which the state says
/// is one (`estimated`).
#[derive(Clone, Debug, PartialEq)]
pub struct MeasuredRoom {
    /// `[min, max]` corners.
    pub box_m: [[f32; 3]; 2],
    /// The box is the loudspeakers' bounding box with margins, not the
    /// file's room.
    pub estimated: bool,
}

impl MeasuredRoom {
    /// Metres kept beyond the farthest loudspeaker on each side when the
    /// file states no room.
    pub const BOX_MARGIN_M: f32 = 0.3;
    /// The floor at least this far below the listener's ears, and this much
    /// headroom above, when the file states no room.
    pub const FLOOR_M: f32 = 1.2;
    pub const HEADROOM_M: f32 = 1.0;

    /// The room of a set whose loudspeakers stand at `emitters` (relative
    /// to the listener, renderer frame, metres), inside `corners` when the
    /// file states them.
    pub fn of(emitters: &[[f32; 3]], corners: Option<[[f32; 3]; 2]>) -> Self {
        let mut lo = [f32::INFINITY; 3];
        let mut hi = [f32::NEG_INFINITY; 3];
        let mut grow = |p: [f32; 3]| {
            for axis in 0..3 {
                lo[axis] = lo[axis].min(p[axis]);
                hi[axis] = hi[axis].max(p[axis]);
            }
        };
        for &e in emitters {
            grow(e);
        }
        if let Some([a, b]) = corners {
            grow(a);
            grow(b);
            // The listener stands in the room too.
            grow([0.0; 3]);
            return Self {
                box_m: [lo, hi],
                estimated: false,
            };
        }
        if emitters.is_empty() {
            lo = [0.0; 3];
            hi = [0.0; 3];
        }
        for axis in 0..3 {
            lo[axis] -= Self::BOX_MARGIN_M;
            hi[axis] += Self::BOX_MARGIN_M;
        }
        lo[2] = lo[2].min(-Self::FLOOR_M);
        hi[2] = hi[2].max(Self::HEADROOM_M);
        Self {
            box_m: [lo, hi],
            estimated: true,
        }
    }

    /// Metres to one unit of the room's ratios: the half-width, as the
    /// user's room counts it (`config_fields::room`), taken to the farther
    /// side wall so that every loudspeaker reads as a fraction within the
    /// cube — the ratios describe a room the listener is centred in
    /// across, which a measured room need not be.
    pub fn radius_m(&self) -> f32 {
        let [lo, hi] = self.box_m;
        lo[0].abs().max(hi[0].abs()).max(0.01)
    }

    /// The room as the stage pans in it: width 1 (the reference, like the
    /// user's room), the other extents as multiples of [`Self::radius_m`].
    /// `center_blend` is the user's front/rear blend — how the cube's depth
    /// is mapped into a room the listener is not centred in along, a
    /// panning policy rather than a measurement.
    pub fn ratios(&self, center_blend: f32) -> crate::live_params::RoomRatios {
        let radius = self.radius_m();
        let [lo, hi] = self.box_m;
        let extent = |m: f32| (m / radius).max(omniphony_geometry::f32::MIN_ROOM_RATIO);
        crate::live_params::RoomRatios {
            ratio: [1.0, extent(hi[1]), extent(hi[2])],
            rear: extent(-lo[1]),
            lower: extent(-lo[2]),
            center_blend: center_blend.clamp(0.0, 1.0),
        }
    }
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

/// Row `i` of a `[X][C][M]` array at measurement `mi` when the file gives
/// one per measurement (`m` of them), else of `[X][C]` (`[X][C][I]`) as
/// [`row3`] reads it.
fn row3_at(arr: &[f32], rows: usize, m: usize, i: usize, mi: usize) -> [f32; 3] {
    if m > 1 && arr.len() == rows * 3 * m {
        let at = |c: usize| arr[(i * 3 + c) * m + mi];
        [at(0), at(1), at(2)]
    } else {
        row3(arr, i)
    }
}

/// Where the head looked at measurement `mi` by its ears: ahead across the
/// axis from the left ear to the right, about `ListenerUp` (`z` when the
/// file has none). `None` when the file gives no two ears, or they do not
/// stand across up.
fn ears_view(raw: &RawRoomIr<'_>, mi: usize) -> Option<[f32; 3]> {
    if raw.r < 2 || raw.receiver_position.len() < 6 {
        return None;
    }
    let ear = |ri: usize| row3_at(raw.receiver_position, raw.r, raw.m, ri, mi);
    let (left, right) = (ear(0), ear(1));
    let across = [right[0] - left[0], right[1] - left[1], right[2] - left[2]];
    let up = if raw.listener_up.len() >= 3 {
        row3(raw.listener_up, mi)
    } else {
        [0.0, 0.0, 1.0]
    };
    // up × (left → right) is ahead: with x ahead and y to the left, the
    // right ear lies along −y, and z × −y = +x.
    let v = [
        up[1] * across[2] - up[2] * across[1],
        up[2] * across[0] - up[0] * across[2],
        up[0] * across[1] - up[1] * across[0],
    ];
    let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    (n.is_finite() && n > 1e-6).then(|| [v[0] / n, v[1] / n, v[2] / n])
}

/// A gap in a set's head orientations at least this wide, degrees, and
/// several times its usual step, is the part of the circle it left out.
const ARC_GAP_DEG: f32 = 30.0;

/// The narrowest arc of head orientations, degrees, taken to be measured
/// on both sides of where the listener faces: the sets that leave part of
/// the circle out turn the head a quarter circle or more each way (TuBuRo
/// ±80°, Calypso ±90°); a narrower one that starts straight ahead is a
/// partial set of its front.
const ARC_MIN_SPAN_DEG: f32 = 90.0;

/// A loudspeaker within this many degrees of azimuth of a direction stands
/// there, for the arc rule of [`front_azimuth`].
const ARC_SPEAKER_DEG: f32 = 5.0;

/// The highest a loudspeaker may stand, degrees, and still count for the
/// arc rule of [`front_azimuth`]: the ear-level ring a front is set by.
const ARC_SPEAKER_MAX_EL_DEG: f32 = 30.0;

/// The SOFA azimuth, radians, of where a set's listener faces, from the head
/// orientations `views` of its measurements and the SOFA azimuths, degrees,
/// of its ear-level loudspeakers (`speakers`, relative to the listener).
///
/// When the views leave part of the circle out, and a loudspeaker stands at
/// the middle of the arc they cover while none stands straight ahead (`+x`),
/// it is that middle: the set was measured turning from one side to the
/// other of where the listener faces, as TU Berlin's Calypso 5.0 set does
/// from 0° to 180° about its centre loudspeaker at 90°. The midpoint alone is
/// no evidence - `ListenerView` is the head's orientation in the file's own
/// frame, and a set may turn the head 120° one way and none the other - so
/// without a loudspeaker to confirm it, or with one at `+x`, the front stays
/// straight ahead. With the circle covered it is straight ahead too, or for
/// views the ears gave (`derived`) where the head looked at the first
/// measurement, the IoSR set's front loudspeaker. A set of one orientation
/// faces it.
fn front_azimuth(views: &[[f32; 3]], derived: bool, speakers: &[f32]) -> f32 {
    let mut az: Vec<f32> = views
        .iter()
        .filter(|v| v[0].abs() + v[1].abs() > 1e-6)
        .map(|v| v[1].atan2(v[0]).to_degrees().rem_euclid(360.0))
        .collect();
    az.sort_by(f32::total_cmp);
    az.dedup_by(|a, b| (*a - *b).abs() < SAME_ANGLE_DEG);
    // Straight ahead is exactly no turn, so that a set facing it is read
    // to the bit as it was before this looked.
    let facing = |deg: f32| {
        let deg = wrap_deg(deg);
        if deg.abs() < SAME_ANGLE_DEG {
            0.0
        } else {
            deg.to_radians()
        }
    };
    match az.len() {
        0 => return 0.0,
        1 => return facing(az[0]),
        _ => {}
    }
    let gaps: Vec<f32> = (0..az.len())
        .map(|i| az.get(i + 1).copied().unwrap_or(az[0] + 360.0) - az[i])
        .collect();
    let (widest, gap) = gaps
        .iter()
        .copied()
        .enumerate()
        .fold((0, f32::NEG_INFINITY), |a, x| if x.1 > a.1 { x } else { a });
    let mut sorted = gaps.clone();
    sorted.sort_by(f32::total_cmp);
    let usual = sorted[sorted.len() / 2];
    if gap >= ARC_GAP_DEG && gap > 3.0 * usual && 360.0 - gap >= ARC_MIN_SPAN_DEG {
        // The arc runs from the orientation after the gap round to the one
        // before it.
        let start = az[(widest + 1) % az.len()];
        let middle = start + (360.0 - gap) / 2.0;
        let stands_at = |deg: f32| {
            speakers
                .iter()
                .any(|&s| wrap_deg(s - deg).abs() <= ARC_SPEAKER_DEG)
        };
        if stands_at(middle) && !stands_at(0.0) {
            return facing(middle);
        }
    }
    if derived {
        let v = views[0];
        v[1].atan2(v[0])
    } else {
        0.0
    }
}

/// `p` turned by `radians` about `z`, anticlockwise seen from above: the
/// same turn in the SOFA frame and the renderer's, both right-handed with
/// `z` up.
fn turn_about_z(p: [f32; 3], radians: f32) -> [f32; 3] {
    if radians == 0.0 {
        return p;
    }
    let (s, c) = radians.sin_cos();
    [p[0] * c - p[1] * s, p[0] * s + p[1] * c, p[2]]
}

/// The box `corners` (renderer frame, `[min, max]`) turned by `radians`
/// about `z`: the box its four turned walls stand in.
fn turn_box(corners: [[f32; 3]; 2], radians: f32) -> [[f32; 3]; 2] {
    if radians == 0.0 {
        return corners;
    }
    let [a, b] = corners;
    let mut lo = [f32::INFINITY, f32::INFINITY, a[2].min(b[2])];
    let mut hi = [f32::NEG_INFINITY, f32::NEG_INFINITY, a[2].max(b[2])];
    for x in [a[0], b[0]] {
        for y in [a[1], b[1]] {
            let p = turn_about_z([x, y, 0.0], radians);
            for axis in 0..2 {
                lo[axis] = lo[axis].min(p[axis]);
                hi[axis] = hi[axis].max(p[axis]);
            }
        }
    }
    [lo, hi]
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
///
/// Not `omniphony_geometry::wrap_deg`: that one adds 180 before reducing,
/// which rounds away the low bits of small angles (0.1 → 0.100006), and the
/// nearest-orientation scans below compare these distances for ties.
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

/// Longest `Data.Delay` a set may declare, in seconds.
const MAX_DATA_DELAY_S: f32 = 1.0;

/// A room's measured pairs at the kept head orientations, before anything is
/// done to them: at the file's rate, `Data.Delay` applied, full length. What
/// [`BrirSet::from_raw`] extracts from a file before [`BrirSet::finish`]
/// trims, resamples and normalises it.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtractedRoom {
    conventions: String,
    file_rate: u32,
    /// Virtual loudspeakers relative to the listener, renderer frame, metres.
    emitters: Vec<[f32; 3]>,
    /// `(yaw, pitch)` degrees, renderer convention, sorted.
    orientations: Vec<(f32, f32)>,
    /// `pairs[emitter * orientations.len() + orientation]`.
    pairs: Vec<BrirPair>,
    /// The file's `RoomType`, when it states one (see [`BrirSet`]).
    room_type: Option<String>,
    /// The room's corners around the listener, renderer frame, metres, when
    /// the file states them (see [`BrirSet`]).
    room_corners: Option<[[f32; 3]; 2]>,
}

impl BrirSet {
    /// Build a set from raw SOFA arrays. Errors name the shape or geometry
    /// problem; a set that comes out silent is refused rather than rendered.
    pub fn from_raw(
        raw: &RawRoomIr<'_>,
        engine_rate: u32,
        opts: &BrirLoadOptions,
    ) -> anyhow::Result<Self> {
        if engine_rate == 0 {
            anyhow::bail!("engine rate is zero");
        }
        Self::finish(
            ExtractedRoom::extract(raw, opts.orientations)?,
            engine_rate,
            opts,
        )
    }
}

impl ExtractedRoom {
    /// The pairs of `raw` at the orientations `selection` keeps, untouched.
    /// Errors name the shape or geometry problem, as [`BrirSet::from_raw`]'s.
    pub fn extract(raw: &RawRoomIr<'_>, selection: OrientationSelection) -> anyhow::Result<Self> {
        RoomPlan::new(raw, selection)?.fill(raw)
    }

    /// How many loudspeakers `raw`'s geometry places, its responses unread.
    pub fn emitter_count(raw: &RawRoomIr<'_>) -> anyhow::Result<usize> {
        Ok(RoomPlan::new(raw, OrientationSelection::FrontOnly)?
            .emitters
            .len())
    }

    /// The run of measurements the orientations `selection` keeps are read
    /// from: what a reader needs of `Data.IR` (passed as `data_ir` from
    /// `ir_first`) to extract them. Only `raw`'s geometry is consulted, so
    /// its `data_ir` may be empty; the errors are [`Self::extract`]'s.
    pub fn measurements(
        raw: &RawRoomIr<'_>,
        selection: OrientationSelection,
    ) -> anyhow::Result<std::ops::Range<usize>> {
        Ok(RoomPlan::new(raw, selection)?.measurements(raw.e))
    }
}

/// What [`ExtractedRoom::extract`] decides from a file's geometry before it
/// touches a response: the loudspeakers, the kept orientations, and the
/// measurement each kept pair is read from.
struct RoomPlan {
    file_rate: u32,
    emitters: Vec<[f32; 3]>,
    /// The kept orientations, sorted.
    orientations: Vec<(f32, f32)>,
    /// Per pair (`emitter * orientations.len() + orientation`): the slot
    /// `measurement * E + emitter slot` it is read from.
    slots: Vec<usize>,
    /// The file's room around the listener, turned as the set is.
    room_corners: Option<[[f32; 3]; 2]>,
}

impl RoomPlan {
    fn new(raw: &RawRoomIr<'_>, selection: OrientationSelection) -> anyhow::Result<Self> {
        let (m, r, e, n) = (raw.m, raw.r, raw.e, raw.n);
        if r < 2 {
            anyhow::bail!("{r} receiver(s); a binaural set needs the two ears");
        }
        if m == 0 || e == 0 || n == 0 {
            anyhow::bail!("empty set (M = {m}, E = {e}, N = {n})");
        }
        // The dimensions come from the file: their product must not wrap.
        let total = m
            .checked_mul(r)
            .and_then(|v| v.checked_mul(e))
            .and_then(|v| v.checked_mul(n));
        if total.is_none() {
            anyhow::bail!(
                "Data.IR holds {} values for M×R×E×N = {}×{}×{}×{}",
                raw.data_ir.len(),
                m,
                r,
                e,
                n
            );
        }
        if !raw.sample_rate.is_finite() || raw.sample_rate < 1.0 {
            anyhow::bail!("invalid sampling rate {}", raw.sample_rate);
        }
        let file_rate = raw.sample_rate.round() as u32;
        // Data.Delay pads each response with that many samples; a direct-path
        // delay is milliseconds. One past a second is a broken file, and
        // honouring it would allocate the padding for every response.
        let max_delay = file_rate as f32 * MAX_DATA_DELAY_S;
        if let Some(d) = raw
            .data_delay
            .iter()
            .find(|d| d.is_finite() && **d > max_delay)
        {
            anyhow::bail!("Data.Delay of {d} samples is implausible (over {MAX_DATA_DELAY_S} s)");
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
        // Without a ListenerView the file states no head direction. SOFA's
        // default looks along +x, which is right for a set that never turned
        // the head; one that did records it in the ears alone, and the head
        // faced across them (the IoSR set).
        let derived_views = raw.listener_view.len() < 3;
        let view_at = |mi: usize| -> [f32; 3] {
            if derived_views {
                ears_view(raw, mi).unwrap_or([1.0, 0.0, 0.0])
            } else {
                row3(raw.listener_view, mi)
            }
        };
        // The whole set turns to put the listener's front straight ahead:
        // the frame a file is written in is the room's, not the listener's
        // (see `front_azimuth`).
        let views: Vec<[f32; 3]> = (0..m).map(view_at).collect();
        // The ear-level loudspeakers' SOFA azimuths, as the file places them
        // around the listener: what confirms the middle of an arc as a front.
        let mut speakers: Vec<f32> = Vec::new();
        for mi in 0..m {
            let listener = row3(raw.listener_position, mi);
            let source = row3(raw.source_position, mi);
            for k in 0..e {
                let em = row3_at(raw.emitter_position, e, m, k, mi);
                let p = [
                    source[0] + em[0] - listener[0],
                    source[1] + em[1] - listener[1],
                    source[2] + em[2] - listener[2],
                ];
                let horiz = (p[0] * p[0] + p[1] * p[1]).sqrt();
                if horiz < SAME_POINT_M
                    || p[2].atan2(horiz).to_degrees().abs() > ARC_SPEAKER_MAX_EL_DEG
                {
                    continue;
                }
                let az = p[1].atan2(p[0]).to_degrees();
                if !speakers
                    .iter()
                    .any(|&s| wrap_deg(s - az).abs() < SAME_ANGLE_DEG)
                {
                    speakers.push(az);
                }
            }
        }
        let turn = -front_azimuth(&views, derived_views, &speakers);

        // Per (measurement, emitter slot): emitter index, orientation index.
        let mut meas: Vec<(usize, usize)> = Vec::with_capacity(m * e);
        for mi in 0..m {
            let listener = row3(raw.listener_position, mi);
            let source = row3(raw.source_position, mi);
            let view = turn_about_z(view_at(mi), turn);
            let o = find_or_push_orientation(view_to_yaw_pitch(view));
            for k in 0..e {
                // The emitter is relative to the source; a one-emitter
                // convention leaves it at the source (zero), so the same sum
                // serves both.
                let em = row3_at(raw.emitter_position, e, m, k, mi);
                let rel = turn_about_z(
                    [
                        source[0] + em[0] - listener[0],
                        source[1] + em[1] - listener[1],
                        source[2] + em[2] - listener[2],
                    ],
                    turn,
                );
                let ei = find_or_push_emitter(omniphony_geometry::f32::sofa_to_adm(rel));
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
        let kept = select_orientations(&orientations, selection);
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
        Ok(Self {
            file_rate,
            emitters,
            orientations: kept.iter().map(|&k| orientations[k]).collect(),
            slots: slot
                .into_iter()
                .map(|s| s.expect("checked complete above"))
                .collect(),
            room_corners: raw.room_corners.map(|c| turn_box(c, turn)),
        })
    }

    /// The run of measurements the kept pairs are read from.
    fn measurements(&self, e: usize) -> std::ops::Range<usize> {
        let first = self.slots.iter().map(|s| s / e).min().unwrap_or(0);
        let last = self.slots.iter().map(|s| s / e).max().unwrap_or(0);
        first..last + 1
    }

    /// Read the kept pairs out of `raw.data_ir`, applying `Data.Delay`: the
    /// whole set, or exactly the run [`Self::measurements`] names.
    fn fill(self, raw: &RawRoomIr<'_>) -> anyhow::Result<ExtractedRoom> {
        let (m, r, e, n) = (raw.m, raw.r, raw.e, raw.n);
        // The product was checked in `new`.
        let row = r * e * n;
        let run = self.measurements(e);
        let whole = raw.ir_first == 0 && raw.data_ir.len() == m * row;
        let just_run = raw.ir_first == run.start && raw.data_ir.len() == run.len() * row;
        if !whole && !just_run {
            anyhow::bail!(
                "Data.IR holds {} values{} for M×R×E×N = {}×{}×{}×{}",
                raw.data_ir.len(),
                if raw.ir_first > 0 {
                    format!(" from measurement {}", raw.ir_first)
                } else {
                    String::new()
                },
                m,
                r,
                e,
                n
            );
        }
        // A NaN would pass the silence guard (max skips it) and reach the
        // convolver, which would then output nothing but NaN.
        if let Some(i) = raw.data_ir.iter().position(|v| !v.is_finite()) {
            anyhow::bail!(
                "Data.IR value {} is not finite ({})",
                raw.ir_first * row + i,
                raw.data_ir[i]
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
            let base = (((mi - raw.ir_first) * r + ri) * e + k) * n;
            let d = delay_of(mi, ri, k);
            let mut ir = vec![0.0f32; d + n];
            ir[d..].copy_from_slice(&raw.data_ir[base..base + n]);
            ir
        };
        let pairs: Vec<BrirPair> = self
            .slots
            .iter()
            .map(|&idx| {
                let (mi, k) = (idx / e, idx % e);
                BrirPair {
                    left: extract(mi, 0, k),
                    right: extract(mi, 1, k),
                }
            })
            .collect();
        Ok(ExtractedRoom {
            conventions: raw.conventions.to_string(),
            file_rate: self.file_rate,
            emitters: self.emitters,
            orientations: self.orientations,
            pairs,
            room_type: raw.room_type.map(str::to_string),
            room_corners: self.room_corners,
        })
    }
}

/// Samples of silence every pair of a set starts with, less
/// [`LEAD_GUARD`]: what [`BrirSet::finish`] drops so the relative delays
/// between emitters and ears survive.
fn common_lead(pairs: &[BrirPair]) -> usize {
    pairs
        .iter()
        .map(|p| onset(&p.left).min(onset(&p.right)))
        .min()
        .unwrap_or(0)
        .saturating_sub(LEAD_GUARD)
}

impl BrirSet {
    /// Make an extracted room renderable at `engine_rate`: drop the silence
    /// common to the set, cut each tail under `opts` with a short fade,
    /// resample, and normalise to unit mean direct-sound energy (see the
    /// module doc). `opts.orientations` is not consulted: the room holds the
    /// orientations it was extracted with. A set that comes out silent is
    /// refused rather than rendered.
    pub fn finish(
        room: ExtractedRoom,
        engine_rate: u32,
        opts: &BrirLoadOptions,
    ) -> anyhow::Result<Self> {
        if engine_rate == 0 {
            anyhow::bail!("engine rate is zero");
        }
        let ExtractedRoom {
            conventions,
            file_rate,
            emitters,
            orientations,
            pairs,
            room_type,
            room_corners,
        } = room;

        // --- silence guard
        let peak = pairs
            .iter()
            .flat_map(|p| p.left.iter().chain(p.right.iter()))
            .fold(0.0f32, |m, &x| m.max(x.abs()));
        if peak <= SILENT_PEAK {
            anyhow::bail!("every response is silent (peak {peak:e})");
        }

        // --- common lead: keep the relative delays, drop the shared silence
        let lead = common_lead(&pairs);

        // --- tail cut + fade, then resample
        let fade = (TAIL_FADE_S * file_rate as f32).round() as usize;
        let max_len = if opts.max_length_s > 0.0 {
            (opts.max_length_s * file_rate as f32).round() as usize
        } else {
            usize::MAX
        };
        let kernel =
            (file_rate != engine_rate).then(|| ResampleKernel::new(file_rate, engine_rate));
        let pairs: Vec<BrirPair> = crate::background_pool::install(|| {
            pairs
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
                .collect()
        });

        // --- normalise: unit mean direct-sound energy (the HRIR scale), over
        // the loudspeakers the programme is panned to; the LFE loudspeakers
        // keep their level relative to them.
        let lfe = lfe_emitters(&emitters);
        let per_emitter = orientations.len().max(1);
        let window = (HRIR_SPAN_S * engine_rate as f32).ceil() as usize;
        let mut acc = 0.0f64;
        let mut count = 0usize;
        for (i, p) in pairs.iter().enumerate() {
            if lfe.get(i / per_emitter).copied().unwrap_or(false) {
                continue;
            }
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
        let lfe_count = lfe.iter().filter(|&&l| l).count();
        let set = Self {
            sample_rate: engine_rate,
            emitters,
            lfe,
            orientations,
            pairs,
            max_taps,
            conventions,
            room_type,
            room_corners,
        };
        if lfe_count > 0 {
            log::info!(
                "BRIR: a 22.2 room (ITU-R BS.2051 System H): its {lfe_count} LFE loudspeakers \
                 carry the LFE"
            );
        }
        log::info!(
            "BRIR: {} ({}): {} emitters × {} orientations, up to {} taps ({:.3} s) at {} Hz, {:.1} MiB",
            if set.conventions.is_empty() {
                "unnamed convention"
            } else {
                set.conventions.as_str()
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

    /// A set holding `pairs` exactly as given
    /// (`pairs[emitter * orientations.len() + orientation]`, orientations
    /// sorted): no lead removal, tail cut or normalisation. For tests that
    /// need kernels of exact lengths.
    #[cfg(test)]
    pub(crate) fn from_pairs(
        emitters: Vec<[f32; 3]>,
        orientations: Vec<(f32, f32)>,
        pairs: Vec<BrirPair>,
    ) -> Self {
        assert_eq!(pairs.len(), emitters.len() * orientations.len());
        let max_taps = pairs.iter().map(BrirPair::taps).max().unwrap_or(0);
        Self {
            sample_rate: 48_000,
            lfe: lfe_emitters(&emitters),
            emitters,
            orientations,
            pairs,
            max_taps,
            conventions: "test".to_string(),
            room_type: None,
            room_corners: None,
        }
    }

    /// The file's `RoomType`, when it states one.
    pub fn room_type(&self) -> Option<&str> {
        self.room_type.as_deref()
    }

    /// The room's two opposite corners, relative to the listener in the
    /// renderer's frame, when the file states them.
    pub fn room_corners(&self) -> Option<[[f32; 3]; 2]> {
        self.room_corners
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

    /// Whether emitter `e` is one of the room's LFE loudspeakers
    /// ([`lfe_emitters`]): it carries the LFE, and nothing is panned to it.
    pub fn is_lfe(&self, e: usize) -> bool {
        self.lfe.get(e).copied().unwrap_or(false)
    }

    /// The room's LFE at orientation `o`: its LFE loudspeakers' pairs played
    /// together, each at `1/n`, so that the `n` of them - in phase at the
    /// frequencies they reproduce - sum to one loudspeaker's level, the one
    /// a single LFE channel is meant to have. `None` for a room without LFE
    /// loudspeakers.
    pub fn lfe_pair(&self, o: usize) -> Option<BrirPair> {
        let subs: Vec<usize> = (0..self.emitters.len())
            .filter(|&e| self.is_lfe(e))
            .collect();
        if subs.is_empty() {
            return None;
        }
        let w = 1.0 / subs.len() as f32;
        let taps = subs
            .iter()
            .map(|&e| self.pair(e, o).taps())
            .max()
            .unwrap_or(0);
        let mut sum = BrirPair {
            left: vec![0.0; taps],
            right: vec![0.0; taps],
        };
        for &e in &subs {
            let p = self.pair(e, o);
            for (s, &x) in sum.left.iter_mut().zip(&p.left) {
                *s += w * x;
            }
            for (s, &x) in sum.right.iter_mut().zip(&p.right) {
                *s += w * x;
            }
        }
        Some(sum)
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

impl ExtractedRoom {
    /// The file's `SOFAConventions`.
    pub fn conventions(&self) -> &str {
        &self.conventions
    }

    /// Sampling rate of the pairs, Hz.
    pub fn file_rate(&self) -> u32 {
        self.file_rate
    }

    /// Virtual loudspeakers relative to the listener, renderer frame (`x`
    /// right, `y` front, `z` up), metres.
    pub fn emitters(&self) -> &[[f32; 3]] {
        &self.emitters
    }

    /// Kept head orientations, `(yaw, pitch)` degrees, sorted.
    pub fn orientations(&self) -> &[(f32, f32)] {
        &self.orientations
    }

    /// The room's corners around the listener, renderer frame, metres, when
    /// the file states them.
    pub fn room_corners(&self) -> Option<[[f32; 3]; 2]> {
        self.room_corners
    }

    /// Keep only the orientations `selection` picks of those held. Picking
    /// again from what a selection kept picks the same.
    pub fn select(self, selection: OrientationSelection) -> anyhow::Result<Self> {
        let kept = select_orientations(&self.orientations, selection);
        if kept.is_empty() {
            anyhow::bail!("no head orientation selected");
        }
        if kept.len() == self.orientations.len() {
            return Ok(self);
        }
        let no = self.orientations.len();
        let pairs = (0..self.emitters.len())
            .flat_map(|e| kept.iter().map(move |&k| e * no + k))
            .map(|i| self.pairs[i].clone())
            .collect();
        Ok(Self {
            orientations: kept.iter().map(|&k| self.orientations[k]).collect(),
            pairs,
            ..self
        })
    }
}

/// The IoSR listening room's 24 loudspeakers, a 22.2 room (Table 2 of its
/// documentation), in System H's channel order: `(azimuth, elevation)` in
/// degrees, the azimuth anticlockwise as BS.2051 counts it.
#[cfg(test)]
pub(crate) const IOSR_22_2: [(f32, f32); 24] = [
    (60.0, 0.0),
    (-60.0, 0.0),
    (0.0, 0.0),
    (39.0, -18.0),
    (135.0, 0.0),
    (-135.0, 0.0),
    (30.0, 0.0),
    (-30.0, 0.0),
    (180.0, 0.0),
    (-39.0, -18.0),
    (90.0, 0.0),
    (-90.0, 0.0),
    (45.0, 30.0),
    (-45.0, 30.0),
    (0.0, 30.0),
    (0.0, 90.0),
    (135.0, 30.0),
    (-135.0, 30.0),
    (90.0, 30.0),
    (-90.0, 30.0),
    (180.0, 30.0),
    (0.0, -30.0),
    (45.0, -30.0),
    (-45.0, -30.0),
];

/// [`IOSR_22_2`] `metres` from the listener, in the renderer's frame.
#[cfg(test)]
pub(crate) fn iosr_22_2_emitters(metres: f32) -> Vec<[f32; 3]> {
    IOSR_22_2
        .iter()
        .map(|&(az, el)| {
            let (az, el) = (az.to_radians(), el.to_radians());
            [
                -metres * az.sin() * el.cos(),
                metres * az.cos() * el.cos(),
                metres * el.sin(),
            ]
        })
        .collect()
}

/// The buses a room's layout ([`measured_room_layout`]) takes: one per
/// panned loudspeaker, and the LFE.
pub fn measured_room_buses(emitters: &[[f32; 3]]) -> usize {
    emitters.len() - lfe_emitters(emitters).iter().filter(|&&l| l).count() + 1
}

/// The layout a session renders a room on, as
/// [`crate::live_params::RendererControl::brir_layout`] builds it once the
/// room is resident: its loudspeakers placed in the room they were measured
/// in ([`MeasuredRoom`]), with the user's front/rear `center_blend`. A host
/// that builds its session on it before the room loads renders the same
/// array throughout.
pub fn measured_room_layout(
    emitters: &[[f32; 3]],
    corners: Option<[[f32; 3]; 2]>,
    center_blend: f32,
) -> anyhow::Result<crate::speaker_layout::SpeakerLayout> {
    let measured = MeasuredRoom::of(emitters, corners);
    crate::speaker_layout::SpeakerLayout::from_brir_emitters(
        emitters,
        &measured.ratios(center_blend),
        measured.radius_m(),
    )
}

/// The fewest loudspeakers a room renders on: the panner places a sound
/// between three of them.
#[cfg(feature = "sofa")]
const ROOM_MIN_EMITTERS: usize = 3;

/// Refuse fewer loudspeakers than a sound can be placed between (the WDR
/// control rooms' files measure one monitor each), before a response is read.
#[cfg(feature = "sofa")]
fn check_room_size(emitters: usize) -> anyhow::Result<()> {
    if emitters < ROOM_MIN_EMITTERS {
        anyhow::bail!(
            "{emitters} loudspeaker{} measured: a room needs at least {ROOM_MIN_EMITTERS} \
             around the listener to place sound between",
            if emitters == 1 { "" } else { "s" }
        );
    }
    Ok(())
}

/// A channel of [`SYSTEM_H`]: its azimuth range and its elevation range in
/// degrees, the azimuth anticlockwise (left positive) as BS.2051 counts it
/// and `None` straight up, where it is undefined; and whether it is an LFE.
type SystemHChannel = (Option<(f32, f32)>, (f32, f32), bool);

/// ITU-R BS.2051-3 Sound System H (9+10+3, "22.2"), Table 10, in its
/// channel order.
const SYSTEM_H: [SystemHChannel; 24] = [
    (Some((45.0, 60.0)), (0.0, 5.0), false),       // FL
    (Some((-60.0, -45.0)), (0.0, 5.0), false),     // FR
    (Some((0.0, 0.0)), (0.0, 5.0), false),         // FC
    (Some((30.0, 90.0)), (-30.0, -15.0), true),    // LFE1
    (Some((110.0, 135.0)), (0.0, 15.0), false),    // BL
    (Some((-135.0, -110.0)), (0.0, 15.0), false),  // BR
    (Some((22.5, 30.0)), (0.0, 5.0), false),       // FLc
    (Some((-30.0, -22.5)), (0.0, 5.0), false),     // FRc
    (Some((180.0, 180.0)), (0.0, 15.0), false),    // BC
    (Some((-90.0, -30.0)), (-30.0, -15.0), true),  // LFE2
    (Some((90.0, 90.0)), (0.0, 15.0), false),      // SiL
    (Some((-90.0, -90.0)), (0.0, 15.0), false),    // SiR
    (Some((45.0, 60.0)), (30.0, 45.0), false),     // TpFL
    (Some((-60.0, -45.0)), (30.0, 45.0), false),   // TpFR
    (Some((0.0, 0.0)), (30.0, 45.0), false),       // TpFC
    (None, (90.0, 90.0), false),                   // TpC
    (Some((110.0, 135.0)), (30.0, 45.0), false),   // TpBL
    (Some((-135.0, -110.0)), (30.0, 45.0), false), // TpBR
    (Some((90.0, 90.0)), (30.0, 45.0), false),     // TpSiL
    (Some((-90.0, -90.0)), (30.0, 45.0), false),   // TpSiR
    (Some((180.0, 180.0)), (30.0, 45.0), false),   // TpBC
    (Some((0.0, 0.0)), (-30.0, -15.0), false),     // BtFC
    (Some((45.0, 60.0)), (-30.0, -15.0), false),   // BtFL
    (Some((-60.0, -45.0)), (-30.0, -15.0), false), // BtFR
];

/// Degrees a measured loudspeaker may stand outside its System H range:
/// what a room's own coordinates, written to a few millimetres, round to.
const SYSTEM_H_SLACK_DEG: f32 = 2.0;

/// Whether a loudspeaker at `p` (renderer frame) stands where System H
/// channel `ch` does.
fn system_h_fits(p: [f32; 3], ch: &SystemHChannel) -> bool {
    let (azimuths, (el_lo, el_hi), _) = *ch;
    let el = p[2].atan2((p[0] * p[0] + p[1] * p[1]).sqrt()).to_degrees();
    if el < el_lo - SYSTEM_H_SLACK_DEG || el > el_hi + SYSTEM_H_SLACK_DEG {
        return false;
    }
    let Some((lo, hi)) = azimuths else {
        return true;
    };
    // The renderer's x is to the right: anticlockwise azimuth is atan2(−x, y).
    let az = (-p[0]).atan2(p[1]).to_degrees();
    let inside = wrap_deg(az - lo) >= 0.0 && wrap_deg(hi - az) >= 0.0;
    inside
        || wrap_deg(az - lo).abs() <= SYSTEM_H_SLACK_DEG
        || wrap_deg(az - hi).abs() <= SYSTEM_H_SLACK_DEG
}

/// A seating of every loudspeaker in a System H channel of its own that it
/// fits (`fits[e][ch]`), as the channel of each; `off_lfe` is kept off the
/// LFE channels. Kuhn's augmenting paths, on 24 × 24.
fn seat_in_system_h(fits: &[[bool; 24]], off_lfe: Option<usize>) -> Option<Vec<usize>> {
    fn seat(
        e: usize,
        fits: &[[bool; 24]],
        off_lfe: Option<usize>,
        owner: &mut [Option<usize>; 24],
        tried: &mut [bool; 24],
    ) -> bool {
        for ch in 0..SYSTEM_H.len() {
            if !fits[e][ch] || tried[ch] || (off_lfe == Some(e) && SYSTEM_H[ch].2) {
                continue;
            }
            tried[ch] = true;
            if owner[ch].is_none_or(|other| seat(other, fits, off_lfe, owner, tried)) {
                owner[ch] = Some(e);
                return true;
            }
        }
        false
    }
    let mut owner = [None; 24];
    for e in 0..fits.len() {
        if !seat(e, fits, off_lfe, &mut owner, &mut [false; 24]) {
            return None;
        }
    }
    let mut channel = vec![0; fits.len()];
    for (ch, e) in owner.iter().enumerate() {
        if let Some(e) = *e {
            channel[e] = ch;
        }
    }
    Some(channel)
}

/// Which of a room's loudspeakers (renderer frame, the room's order) are its
/// LFE loudspeakers: the two of a room that is ITU-R BS.2051 Sound System H
/// (22.2) - exactly its 24 loudspeakers, each where its channel stands - and
/// none otherwise. The LFE channels' ranges overlap the bottom front
/// corners', so a loudspeaker is an LFE only when every seating of the room
/// in System H's channels makes it one; a room that can be seated either way
/// has none.
pub fn lfe_emitters(emitters: &[[f32; 3]]) -> Vec<bool> {
    let none = vec![false; emitters.len()];
    if emitters.len() != SYSTEM_H.len() {
        return none;
    }
    let fits: Vec<[bool; 24]> = emitters
        .iter()
        .map(|&p| std::array::from_fn(|ch| system_h_fits(p, &SYSTEM_H[ch])))
        .collect();
    let Some(channel) = seat_in_system_h(&fits, None) else {
        return none;
    };
    let lfe: Vec<bool> = channel.iter().map(|&ch| SYSTEM_H[ch].2).collect();
    let certain = lfe
        .iter()
        .enumerate()
        .filter(|&(_, &is)| is)
        .all(|(e, _)| seat_in_system_h(&fits, Some(e)).is_none());
    if certain { lfe } else { none }
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
    /// Load a room-response SOFA file: its geometry, then the responses of
    /// the orientations `opts` keeps (see the module doc).
    pub fn from_sofa(path: &str, engine_rate: u32, opts: &BrirLoadOptions) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path).map_err(|e| anyhow::anyhow!("read '{path}': {e}"))?;
        Self::from_sofa_bytes(path, &bytes, engine_rate, opts)
    }

    /// [`Self::from_sofa`] on the file's bytes; `path` names it in errors.
    fn from_sofa_bytes(
        path: &str,
        bytes: &[u8],
        engine_rate: u32,
        opts: &BrirLoadOptions,
    ) -> anyhow::Result<Self> {
        with_sofa_room(bytes, opts.orientations, |raw| {
            Self::from_raw(raw, engine_rate, opts)
        })
        .map_err(|e| anyhow::anyhow!("SOFA '{path}': {e}"))
    }
}

/// `RoomCornerA` and `RoomCornerB` of a SOFA file in SOFA cartesian metres,
/// when both are present and their encoding is understood
/// ([`room_corner_metadata`] says where the encoding is read from).
#[cfg(feature = "sofa")]
fn sofa_room_corners(
    parsed: &sofar::hdf::ParsedHdf<'_>,
    globals: &std::collections::HashMap<String, String>,
) -> Option<[[f32; 3]; 2]> {
    use std::collections::HashMap;
    let attributes_of = |obj: &sofar::hdf::DataObject| -> HashMap<String, String> {
        obj.parsed_attributes
            .iter()
            .filter_map(|a| a.value.as_ref().map(|v| (a.name.clone(), v.clone())))
            .collect()
    };
    // The convention's `RoomCorners` variable exists for its attributes
    // alone: the two corners' `Type` and `Units`.
    let shared = parsed
        .get_child("RoomCorners")
        .and_then(|r| r.ok())
        .map(|obj| attributes_of(&obj))
        .unwrap_or_default();
    let corner = |name: &str| -> Option<[f32; 3]> {
        let obj = parsed.get_child(name)?.ok()?;
        let values = hdf_floats(&obj, 3)?;
        let (kind, units) = room_corner_metadata(&shared, &attributes_of(&obj), globals);
        room_corner_metres(
            [values[0], values[1], values[2]],
            kind.as_deref(),
            units.as_deref(),
        )
    };
    Some([corner("RoomCornerA")?, corner("RoomCornerB")?])
}

/// The coordinate metadata (`Type`, `Units`) of a room corner, from where
/// a SOFA file keeps it, in the order it is looked for: the `RoomCorners`
/// variable the convention includes for that alone (`shared`: its
/// `RoomCorners:Type` / `RoomCorners:Units` are that variable's
/// attributes), then the corner variable's own attributes (`own`, which
/// some writers duplicate), then the same names among the global
/// attributes (`globals`, where a writer that knows no `RoomCorners`
/// variable leaves them). `None` where none states it: the convention's
/// default, cartesian metres, applies.
pub fn room_corner_metadata(
    shared: &std::collections::HashMap<String, String>,
    own: &std::collections::HashMap<String, String>,
    globals: &std::collections::HashMap<String, String>,
) -> (Option<String>, Option<String>) {
    let find = |key: &str| {
        shared
            .get(key)
            .or_else(|| own.get(key))
            .or_else(|| globals.get(&format!("RoomCorners:{key}")))
            .cloned()
    };
    (find("Type"), find("Units"))
}

/// A room corner in SOFA cartesian metres, from its stored triplet and its
/// coordinate metadata: cartesian in metres as stored, spherical (azimuth
/// and elevation in degrees, radius in metres) converted, absent metadata
/// read as the convention's default, cartesian metres. Another type or
/// unit is refused — the loudspeakers' own box then stands for the room —
/// rather than published as metres it is not.
pub fn room_corner_metres(
    values: [f32; 3],
    coord_type: Option<&str>,
    units: Option<&str>,
) -> Option<[f32; 3]> {
    if !values.iter().all(|v| v.is_finite()) {
        return None;
    }
    let lower = |s: Option<&str>| s.map(|s| s.trim().to_ascii_lowercase());
    let metres = |unit: &str| matches!(unit.trim(), "metre" | "metres" | "meter" | "meters" | "m");
    let kind = lower(coord_type).unwrap_or_else(|| "cartesian".to_owned());
    match kind.as_str() {
        "cartesian" => {
            let units = lower(units).unwrap_or_else(|| "metre".to_owned());
            metres(&units).then_some(values)
        }
        "spherical" => {
            // "degree, degree, metre" is the convention's spelling; the
            // radius is the last unit named.
            let units = lower(units).unwrap_or_else(|| "degree, degree, metre".to_owned());
            let radius_unit = units.rsplit(',').next().unwrap_or("");
            if !metres(radius_unit) {
                return None;
            }
            let [azimuth, elevation, radius] = values;
            let (az, el) = (azimuth.to_radians(), elevation.to_radians());
            let horizontal = el.cos() * radius;
            Some([
                az.cos() * horizontal,
                az.sin() * horizontal,
                el.sin() * radius,
            ])
        }
        _ => None,
    }
}

/// Hand `f` the raw arrays of the room-response SOFA file in `bytes`, with
/// `Data.IR` holding only the measurements `selection` is extracted from
/// ([`ExtractedRoom::measurements`]): the geometry is read first, then that
/// run of responses, and nothing else of them is read or inflated.
#[cfg(feature = "sofa")]
fn with_sofa_room<T>(
    bytes: &[u8],
    selection: OrientationSelection,
    f: impl FnOnce(&RawRoomIr<'_>) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let sofa = sofar::reader::LazySofa::open(bytes).map_err(|e| anyhow::anyhow!("open: {e}"))?;
    with_sofa_geometry(&sofa, |raw| {
        check_room_size(ExtractedRoom::emitter_count(&raw)?)?;
        let run = ExtractedRoom::measurements(&raw, selection)?;
        let data_ir = sofa
            .read_ir(run.start, run.len())
            .map_err(|e| anyhow::anyhow!("Data.IR: {e}"))?;
        f(&RawRoomIr {
            data_ir: &data_ir,
            ir_first: run.start,
            ..raw
        })
    })
}

/// `f` of an open SOFA file's room as its geometry describes it, with no
/// responses read (`data_ir` empty): the loudspeakers and orientations, and
/// the room they stand in where the file says - its `RoomType` attribute and
/// its `RoomCornerA`/`RoomCornerB` variables, read from the file as parsed.
#[cfg(feature = "sofa")]
fn with_sofa_geometry<T>(
    sofa: &sofar::reader::LazySofa<'_>,
    f: impl FnOnce(RawRoomIr<'_>) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let [m, r, e, n] = match sofa.ir_shape().as_slice() {
        &[m, r, n] => [m, r, 1, n],
        &[m, r, e, n] => [m, r, e, n],
        other => anyhow::bail!("Data.IR has {} axes, expected 3 or 4", other.len()),
    };
    let h = sofa.hrtf();
    let sample_rate = *h
        .data_sampling_rate
        .values
        .first()
        .ok_or_else(|| anyhow::anyhow!("no Data.SamplingRate"))?;
    let conventions = h
        .attributes
        .get("SOFAConventions")
        .map(String::as_str)
        .unwrap_or("");
    let room_type = h
        .attributes
        .get("RoomType")
        .map(|t| t.trim())
        .filter(|t| !t.is_empty());
    let room_corners = sofa_room_corners(sofa.hdf(), &h.attributes)
        .map(|c| room_corners_relative(c, row3(&h.listener_position.values, 0)));
    let raw = RawRoomIr {
        conventions,
        sample_rate,
        m,
        r,
        e,
        n,
        source_position: &h.source_position.values,
        emitter_position: &h.emitter_position.values,
        listener_position: &h.listener_position.values,
        listener_view: &h.listener_view.values,
        listener_up: &h.listener_up.values,
        receiver_position: &h.receiver_position.values,
        data_ir: &[],
        ir_first: 0,
        data_delay: &h.data_delay.values,
        room_type,
        room_corners,
    };
    f(raw)
}

/// The first `count` values of a floating-point HDF dataset (little-endian,
/// as the SOFA reader assumes); `None` for another class, or too short.
#[cfg(feature = "sofa")]
fn hdf_floats(obj: &sofar::hdf::DataObject, count: usize) -> Option<Vec<f32>> {
    if obj.dt.class_and_version & 0x0F != 1 {
        return None;
    }
    let precision = match obj.dt.data_fmt.as_ref() {
        Some(sofar::hdf::DataFormat::Float { bit_precision, .. }) => *bit_precision,
        _ => 64,
    };
    let width = match precision {
        64 => 8,
        32 => 4,
        _ => return None,
    };
    let bytes = obj.data.get(..width * count)?;
    Some(
        bytes
            .chunks_exact(width)
            .map(|b| match width {
                8 => f64::from_le_bytes(b.try_into().expect("8 bytes")) as f32,
                _ => f32::from_le_bytes(b.try_into().expect("4 bytes")),
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A room's corners are published around the listener in the renderer's
    /// frame: a listener at (3, 2, 1.2) in a 6 × 4 × 2.5 m SOFA room has
    /// the corners 3 m behind and ahead, 2 m to either side, 1.2 m down and
    /// 1.3 m up, with SOFA's left-positive y becoming the renderer's
    /// right-positive x.
    #[test]
    fn room_corners_are_taken_around_the_listener_in_the_renderers_frame() {
        let [a, b] = room_corners_relative([[0.0, 0.0, 0.0], [6.0, 4.0, 2.5]], [3.0, 2.0, 1.2]);
        let near =
            |got: [f32; 3], want: [f32; 3]| got.iter().zip(want).all(|(g, w)| (g - w).abs() < 1e-6);
        assert!(near(a, [2.0, -3.0, -1.2]), "{a:?}");
        assert!(near(b, [-2.0, 3.0, 1.3]), "{b:?}");
    }

    /// The corners' encoding is read where the convention keeps it, the
    /// `RoomCorners` variable's attributes, before a corner's own duplicate
    /// or a global copy; a file stating it nowhere gets the default.
    #[test]
    fn room_corner_metadata_comes_from_the_room_corners_variable_first() {
        use std::collections::HashMap;
        let map = |pairs: &[(&str, &str)]| -> HashMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let shared = map(&[("Type", "spherical"), ("Units", "degree, degree, metre")]);
        let own = map(&[("Type", "cartesian"), ("Units", "metre")]);
        let globals = map(&[
            ("RoomCorners:Type", "cartesian"),
            ("RoomCorners:Units", "metre"),
        ]);
        let none = HashMap::new();
        assert_eq!(
            room_corner_metadata(&shared, &own, &globals),
            (
                Some("spherical".into()),
                Some("degree, degree, metre".into())
            )
        );
        assert_eq!(
            room_corner_metadata(&none, &own, &globals),
            (Some("cartesian".into()), Some("metre".into()))
        );
        let globals = map(&[("RoomCorners:Type", "spherical")]);
        assert_eq!(
            room_corner_metadata(&none, &none, &globals),
            (Some("spherical".into()), None)
        );
        assert_eq!(room_corner_metadata(&none, &none, &none), (None, None));
        // Shared metadata applies to both corners: a spherical pair under
        // it lands on its cartesian twins.
        let r = (36.0f32 + 16.0 + 6.25).sqrt();
        let spherical = [
            4.0f32.atan2(6.0).to_degrees(),
            (2.5 / r).asin().to_degrees(),
            r,
        ];
        let (kind, units) = room_corner_metadata(&shared, &none, &none);
        let got = room_corner_metres(spherical, kind.as_deref(), units.as_deref()).expect("read");
        assert!(
            got.iter()
                .zip([6.0, 4.0, 2.5])
                .all(|(g, w)| (g - w).abs() < 1e-3),
            "{got:?}"
        );
    }

    /// A corner stored as spherical degrees and metres lands where its
    /// cartesian twin does; absent metadata is cartesian metres; another
    /// type or unit is refused rather than read as metres.
    #[test]
    fn room_corners_are_read_in_the_encoding_the_file_states() {
        let cartesian = [6.0, 4.0, 2.5];
        let near =
            |got: [f32; 3], want: [f32; 3]| got.iter().zip(want).all(|(g, w)| (g - w).abs() < 1e-3);
        assert_eq!(room_corner_metres(cartesian, None, None), Some(cartesian));
        assert_eq!(
            room_corner_metres(cartesian, Some("cartesian"), Some("metre")),
            Some(cartesian)
        );
        let r = (36.0f32 + 16.0 + 6.25).sqrt();
        let spherical = [
            4.0f32.atan2(6.0).to_degrees(),
            (2.5 / r).asin().to_degrees(),
            r,
        ];
        let got = room_corner_metres(spherical, Some("spherical"), Some("degree, degree, metre"))
            .expect("spherical corners are read");
        assert!(near(got, cartesian), "{got:?}");
        let got = room_corner_metres(spherical, Some("Spherical"), None).expect("default units");
        assert!(near(got, cartesian), "{got:?}");
        assert_eq!(
            room_corner_metres(cartesian, Some("cartesian"), Some("feet")),
            None
        );
        assert_eq!(
            room_corner_metres(spherical, Some("spherical"), Some("degree, degree, foot")),
            None
        );
        assert_eq!(room_corner_metres(cartesian, Some("geodesic"), None), None);
        assert_eq!(room_corner_metres([f32::NAN, 0.0, 0.0], None, None), None);
    }

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
                listener_up: &[],
                receiver_position: &[],
                data_ir: &self.ir,
                ir_first: 0,
                data_delay: &self.delay,
                room_type: None,
                room_corners: None,
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

    fn refusal(s: &Synth, engine_rate: u32) -> String {
        match BrirSet::from_raw(&s.raw(), engine_rate, &BrirLoadOptions::default()) {
            Ok(_) => panic!("the set was accepted"),
            Err(e) => e.to_string(),
        }
    }

    /// Each field a file can get wrong is refused with what is wrong, before
    /// anything is allocated from it.
    #[test]
    fn a_malformed_set_is_refused_with_its_reason() {
        let good = || multi_speaker(&[0.0, 30.0], &[0.0], 200, 48000.0, 0.0);
        BrirSet::from_raw(&good().raw(), 48000, &BrirLoadOptions::default())
            .expect("the control set is valid");

        let mut s = good();
        s.r = 1;
        assert!(refusal(&s, 48000).contains("needs the two ears"));
        for field in 0..3 {
            let mut s = good();
            match field {
                0 => s.m = 0,
                1 => s.e = 0,
                _ => s.n = 0,
            }
            assert!(refusal(&s, 48000).contains("empty set"), "field {field}");
        }
        // Dimensions whose product wraps around: refused, not trusted.
        let mut s = good();
        s.m = usize::MAX / 2 + 1;
        assert!(refusal(&s, 48000).contains("Data.IR holds"));

        for rate in [0.0, 0.4, -48000.0, f32::NAN, f32::INFINITY] {
            let mut s = good();
            s.rate = rate;
            assert!(
                refusal(&s, 48000).contains("invalid sampling rate"),
                "rate {rate}"
            );
        }
        assert!(refusal(&good(), 0).contains("engine rate is zero"));

        for bad in [f32::NAN, f32::INFINITY] {
            let mut s = good();
            s.ir[5] = bad;
            assert!(refusal(&s, 48000).contains("not finite"), "{bad}");
        }

        // An emitter on the listener has no direction.
        let mut s = good();
        s.emitter[..3].fill(0.0);
        assert!(refusal(&s, 48000).contains("sits on the listener"));
    }

    #[test]
    fn an_implausible_data_delay_is_refused_before_it_is_allocated() {
        let mut s = multi_speaker(&[0.0], &[0.0], 200, 48000.0, 0.0);
        // [R]: a right ear "delayed" by ten billion samples (40 GB of padding).
        s.delay = vec![0.0, 1e10];
        assert!(refusal(&s, 48000).contains("Data.Delay"));
        // Up to a second is honoured; negative and NaN delays read as none.
        s.delay = vec![f32::NAN, 48000.0];
        BrirSet::from_raw(&s.raw(), 48000, &BrirLoadOptions::default()).expect("one second");
        s.delay = vec![-5.0, 0.0];
        BrirSet::from_raw(&s.raw(), 48000, &BrirLoadOptions::default()).expect("negative");
    }
    /// `raw` with `Data.IR` cut down to `run`, as a reader that read only
    /// those measurements hands it over.
    fn cut<'a>(raw: &RawRoomIr<'a>, run: std::ops::Range<usize>) -> RawRoomIr<'a> {
        let row = raw.r * raw.e * raw.n;
        RawRoomIr {
            data_ir: &raw.data_ir[run.start * row..run.end * row],
            ir_first: run.start,
            ..*raw
        }
    }

    /// One emitter per measurement, sources at three azimuths each measured
    /// at two head orientations, interleaved: the front orientation's
    /// measurements (0, 2, 4) are not a contiguous run.
    fn interleaved() -> Synth {
        let (m, r, n) = (6usize, 2usize, 200usize);
        let mut ir = vec![0.0f32; m * r * n];
        for mi in 0..m {
            for ri in 0..r {
                let resp = response(n, 100, marker(0, mi, ri), 0.0, (mi * 7 + ri) as u32);
                ir[(mi * r + ri) * n..][..n].copy_from_slice(&resp);
            }
        }
        Synth {
            m,
            r,
            e: 1,
            n,
            source: (0..m)
                .flat_map(|mi| sph([30.0, -30.0, 0.0][mi / 2], 0.0, 2.0))
                .collect(),
            emitter: vec![0.0; 3],
            listener: vec![0.0; 3],
            view: (0..m)
                .flat_map(|mi| sph([0.0, 10.0][mi % 2], 0.0, 1.0))
                .collect(),
            ir,
            delay: (0..m * r).map(|i| (i % 5) as f32).collect(),
            rate: 48000.0,
            conventions: "SingleRoomSRIR",
        }
    }

    /// A reader that reads only the measurements a selection is extracted
    /// from gets the room the whole set gives, `Data.Delay` included, for
    /// every selection: one orientation of a multi-speaker set is one
    /// measurement, and a run may hold measurements nothing keeps.
    #[test]
    fn the_measurements_a_selection_needs_extract_as_the_whole_set_does() {
        let decimated = OrientationSelection::Decimated {
            step_deg: 20.0,
            max_yaw_deg: 20.0,
        };
        let mut multi = multi_speaker(
            &[30.0, -30.0, 0.0],
            &[-40.0, -20.0, 0.0, 20.0, 40.0],
            200,
            48000.0,
            0.3,
        );
        // [M][R][E], so a run that starts past 0 must still find its own.
        multi.delay = (0..5 * 2 * 3).map(|i| (i % 7) as f32).collect();
        let cases = [
            (&multi, OrientationSelection::FrontOnly, 2..3),
            (&multi, decimated, 1..4),
            (&multi, OrientationSelection::All, 0..5),
        ];
        let inter = interleaved();
        for (s, selection, expected) in cases.into_iter().chain([
            (&inter, OrientationSelection::FrontOnly, 0..5),
            (&inter, OrientationSelection::All, 0..6),
        ]) {
            let raw = s.raw();
            let run = ExtractedRoom::measurements(&raw, selection).expect("plans");
            assert_eq!(run, expected, "{selection:?}");
            let whole = ExtractedRoom::extract(&raw, selection).expect("extracts");
            let part = ExtractedRoom::extract(&cut(&raw, run), selection).expect("extracts");
            assert_eq!(part, whole, "{selection:?}");
        }
        // Only the geometry is consulted to plan.
        let raw = RawRoomIr {
            data_ir: &[],
            ..multi.raw()
        };
        assert_eq!(
            ExtractedRoom::measurements(&raw, OrientationSelection::FrontOnly).unwrap(),
            2..3
        );
    }

    /// Responses that are neither the whole set nor exactly the run the
    /// selection needs are refused, not read out of place.
    #[test]
    fn measurements_other_than_the_run_needed_are_refused() {
        let s = multi_speaker(&[30.0, -30.0], &[-20.0, 0.0, 20.0], 200, 48000.0, 0.0);
        let raw = s.raw();
        for run in [0..1, 2..3, 0..2, 1..3] {
            let err =
                ExtractedRoom::extract(&cut(&raw, run.clone()), OrientationSelection::FrontOnly)
                    .expect_err("refused");
            assert!(err.to_string().contains("Data.IR holds"), "{run:?}: {err}");
        }
        ExtractedRoom::extract(&cut(&raw, 1..2), OrientationSelection::FrontOnly).expect("the run");
        ExtractedRoom::extract(&cut(&raw, 0..3), OrientationSelection::FrontOnly).expect("the set");
    }

    /// Through the SOFA reader: the front orientation of a file is read as
    /// its one measurement, and comes out as the whole file's front.
    /// `rows_multispeaker_brir.sofa` holds seven orientations, three in
    /// each of its chunks, and an `[M][R][E]` `Data.Delay`.
    #[cfg(feature = "sofa")]
    #[test]
    fn a_file_is_read_for_the_measurements_it_is_extracted_from() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/sofa/rows_multispeaker_brir.sofa"
        );
        let bytes = std::fs::read(path).expect("fixture");
        let front = with_sofa_room(&bytes, OrientationSelection::FrontOnly, |raw| {
            assert_eq!((raw.m, raw.r, raw.e, raw.n), (7, 2, 3, 50));
            assert_eq!((raw.ir_first, raw.data_ir.len()), (0, 2 * 3 * 50));
            ExtractedRoom::extract(raw, OrientationSelection::FrontOnly)
        })
        .expect("reads");
        let whole = with_sofa_room(&bytes, OrientationSelection::All, |raw| {
            assert_eq!(raw.data_ir.len(), 7 * 2 * 3 * 50);
            ExtractedRoom::extract(raw, OrientationSelection::All)
        })
        .expect("reads");
        assert_eq!(whole.orientations().len(), 7);
        assert_eq!(
            front,
            whole
                .clone()
                .select(OrientationSelection::FrontOnly)
                .unwrap()
        );
        // The fixture's Data.Delay is m·100 + r·10 + e samples; the front
        // is measurement 0.
        let lengths: Vec<(usize, usize)> = front
            .pairs
            .iter()
            .map(|p| (p.left.len(), p.right.len()))
            .collect();
        assert_eq!(lengths, [(50, 60), (51, 61), (52, 62)]);

        // Views turn 10° a measurement: ±20° keeps measurements 0 and 2.
        let decimated = OrientationSelection::Decimated {
            step_deg: 20.0,
            max_yaw_deg: 20.0,
        };
        let some = with_sofa_room(&bytes, decimated, |raw| {
            assert_eq!((raw.ir_first, raw.data_ir.len()), (0, 3 * 2 * 3 * 50));
            ExtractedRoom::extract(raw, decimated)
        })
        .expect("reads");
        assert_eq!(some.orientations().len(), 2);
        assert_eq!(some, whole.select(decimated).unwrap());
    }

    /// End-to-end through the SOFA reader on files generated outside the
    /// repo (see the loader's PR): `BRIR_SOFA_DIR` must point at a directory
    /// holding `msbrir.sofa` (48 k), `msbrir44.sofa` (44.1 k), `srir.sofa` and
    /// `longhrir.sofa`, all 5 emitters or views at 30/−30/0/110/−110° or
    /// −20..20°. Run it with `--ignored`.
    #[cfg(feature = "sofa")]
    #[test]
    #[ignore = "needs BRIR_SOFA_DIR: SOFA files generated outside the repository"]
    fn loads_generated_sofa_files() {
        let dir = std::env::var_os("BRIR_SOFA_DIR")
            .expect("set BRIR_SOFA_DIR to the directory of generated SOFA files");
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

    /// The room a set's loudspeakers stand in: the file's corners when it
    /// states them, grown to hold every loudspeaker (and the listener);
    /// else the loudspeakers' bounding box with a margin, a floor and
    /// headroom, flagged as an estimate.
    #[test]
    fn a_measured_room_is_the_files_box_or_an_estimate_around_the_loudspeakers() {
        let emitters = [
            [-1.5, 3.0, 0.0],
            [1.5, 3.0, 0.0],
            [-2.0, 0.0, 0.0],
            [2.0, -1.0, 1.2],
        ];
        let stated = MeasuredRoom::of(&emitters, Some([[2.5, -2.0, -1.3], [-2.5, 3.5, 1.4]]));
        assert!(!stated.estimated);
        assert_eq!(
            stated.box_m,
            [[-2.5, -2.0, -1.3], [2.5, 3.5, 1.4]],
            "corners in any order"
        );
        // A loudspeaker outside the stated box keeps its place in the room.
        let grown = MeasuredRoom::of(&emitters, Some([[-2.5, -2.0, -1.3], [2.5, 2.0, 1.4]]));
        assert_eq!(
            grown.box_m[1][1], 3.0,
            "the front wall moved out to the fronts"
        );
        assert!(!grown.estimated);

        let estimate = MeasuredRoom::of(&emitters, None);
        assert!(estimate.estimated);
        let m = MeasuredRoom::BOX_MARGIN_M;
        assert_eq!(
            estimate.box_m[0],
            [-2.0 - m, -1.0 - m, -MeasuredRoom::FLOOR_M]
        );
        assert_eq!(estimate.box_m[1], [2.0 + m, 3.0 + m, 1.2 + m]);
        // Headroom applies when the loudspeakers are lower than it.
        let flat = MeasuredRoom::of(&[[-1.0, 1.0, 0.0], [1.0, 1.0, 0.0]], None);
        assert_eq!(flat.box_m[1][2], MeasuredRoom::HEADROOM_M);
    }

    /// The stage's ratios of a measured room: the half-width is the unit,
    /// taken to the farther side wall, the other walls as multiples of it,
    /// the user's blend kept. A cube the listener is centred in is the unit
    /// cube: the direction reading of the direct path (#783), which a
    /// measured room generalises (#803).
    #[test]
    fn a_measured_rooms_ratios_put_the_listener_where_it_was_measured() {
        let room = MeasuredRoom {
            box_m: [[-2.0, -1.0, -1.2], [3.0, 4.5, 1.8]],
            estimated: false,
        };
        assert_eq!(room.radius_m(), 3.0, "the farther side wall");
        let ratios = room.ratios(0.25);
        assert_eq!(ratios.ratio[0], 1.0);
        assert!((ratios.ratio[1] - 1.5).abs() < 1e-6 && (ratios.ratio[2] - 0.6).abs() < 1e-6);
        assert!((ratios.rear - 1.0 / 3.0).abs() < 1e-6);
        assert!((ratios.lower - 0.4).abs() < 1e-6);
        assert_eq!(ratios.center_blend, 0.25);

        let cube = MeasuredRoom {
            box_m: [[-2.5; 3], [2.5; 3]],
            estimated: true,
        };
        assert_eq!(
            cube.ratios(0.0),
            crate::live_params::RoomRatios::UNIT,
            "a centred cube pans as the direction reading does"
        );
        // A wall at the listener never collapses an axis.
        let flat = MeasuredRoom {
            box_m: [[-1.0, 0.0, 0.0], [1.0, 2.0, 0.0]],
            estimated: true,
        };
        let ratios = flat.ratios(0.5);
        assert!(ratios.rear > 0.0 && ratios.lower > 0.0 && ratios.ratio[2] > 0.0);
    }

    /// The IoSR listening room's set as its SOFA file has it: `GeneralFIR`,
    /// one loudspeaker per measurement in `EmitterPosition` (`[E][C][M]`),
    /// `SourcePosition` and `ListenerPosition` at the room's centre, no
    /// `ListenerView`, the head's turn in the ears (`ReceiverPosition` as
    /// `[R][C][M]`), and a room frame whose +y is where the listener faces
    /// at the first head angle. Head angles `heads` (anticlockwise, degrees)
    /// × the 24 loudspeakers, each response an impulse of `marker(k, h, ear)`
    /// at `10 + k` samples. Returns (emitter, receiver, ir, M).
    fn iosr_like(heads: &[f32], n: usize) -> (Vec<f32>, Vec<f32>, Vec<f32>, usize) {
        let m = heads.len() * IOSR_22_2.len();
        let to_room = |p: [f32; 3]| turn_about_z(p, 90f32.to_radians());
        let (mut emitter, mut receiver) = (vec![0.0; 3 * m], vec![0.0; 2 * 3 * m]);
        let mut ir = vec![0.0f32; m * 2 * n];
        for (h, &head) in heads.iter().enumerate() {
            for (k, &(az, el)) in IOSR_22_2.iter().enumerate() {
                let mi = h * IOSR_22_2.len() + k;
                let speaker = to_room(sph(az, el, 2.4));
                for c in 0..3 {
                    emitter[c * m + mi] = speaker[c];
                }
                for (ri, side) in [(0, 0.095f32), (1, -0.095)] {
                    let ear = to_room(turn_about_z([0.0, side, 0.0], head.to_radians()));
                    for c in 0..3 {
                        receiver[(ri * 3 + c) * m + mi] = ear[c];
                    }
                    ir[(mi * 2 + ri) * n + 10 + k] = marker(k, h, ri);
                }
            }
        }
        (emitter, receiver, ir, m)
    }

    /// A set that keeps its head's turn in its ears faces across them, and is
    /// turned so that its first head angle looks straight ahead: the IoSR
    /// room's front loudspeaker, at +y in the file, renders from the front,
    /// its left ones from the left. As a 22.2 room its two LFE loudspeakers
    /// carry the LFE, take no speaker of the layout and are named for their
    /// channels; and its room turns with it.
    #[test]
    fn a_room_turned_in_its_ears_is_read_from_where_the_head_looked() {
        let n = 64;
        let heads = [0.0, 90.0];
        let (emitter, receiver, ir, m) = iosr_like(&heads, n);
        let zeros = vec![0.0; 3 * m];
        let corners =
            room_corners_relative([[-3.675, -2.85, -1.25], [3.675, 2.85, 1.25]], [0.0; 3]);
        let raw = RawRoomIr {
            conventions: "GeneralFIR",
            sample_rate: 48000.0,
            m,
            r: 2,
            e: 1,
            n,
            source_position: &zeros,
            emitter_position: &emitter,
            listener_position: &zeros,
            listener_view: &[],
            listener_up: &[],
            receiver_position: &receiver,
            data_ir: &ir,
            ir_first: 0,
            data_delay: &[],
            room_type: None,
            room_corners: Some(corners),
        };

        let front = ExtractedRoom::extract(&raw, OrientationSelection::FrontOnly).unwrap();
        assert_eq!(front.emitters.len(), 24);
        assert_eq!(front.orientations.len(), 1);
        assert!(
            front.orientations[0].0.abs() < 1e-3,
            "{:?}",
            front.orientations
        );
        let want = iosr_22_2_emitters(2.4);
        for (k, (got, want)) in front.emitters.iter().zip(&want).enumerate() {
            let d = (0..3)
                .map(|c| (got[c] - want[c]).abs())
                .fold(0.0f32, f32::max);
            assert!(d < 1e-4, "loudspeaker {k}: {got:?}, want {want:?}");
        }
        // The pairs are the first head angle's.
        for (k, pair) in front.pairs.iter().enumerate() {
            assert_eq!(pair.left[10 + k], marker(k, 0, 0), "loudspeaker {k}");
            assert_eq!(pair.right[10 + k], marker(k, 0, 1), "loudspeaker {k}");
        }
        // The head turned to the left at the second: yaw −90, ours.
        let all = ExtractedRoom::extract(&raw, OrientationSelection::All).unwrap();
        assert_eq!(all.orientations.len(), 2);
        assert!(
            all.orientations.iter().any(|o| (o.0 + 90.0).abs() < 1e-3),
            "{:?}",
            all.orientations
        );
        // The room is 7.35 m across the listener and 5.70 m deep.
        let [lo, hi] = front.room_corners.unwrap();
        for (got, want) in lo
            .iter()
            .chain(&hi)
            .zip([-3.675, -2.85, -1.25, 3.675, 2.85, 1.25])
        {
            assert!((got - want).abs() < 1e-4, "{lo:?} {hi:?}");
        }

        let lfe = lfe_emitters(&front.emitters);
        let at: Vec<usize> = (0..24).filter(|&e| lfe[e]).collect();
        assert_eq!(at, [3, 9]);
        assert_eq!(measured_room_buses(&front.emitters), 23);
        let layout = measured_room_layout(&front.emitters, front.room_corners, 0.5).unwrap();
        assert_eq!(layout.num_speakers(), 23);
        assert!(
            !layout
                .speaker_names()
                .iter()
                .any(|n| n.starts_with("LFE") && *n != "LFE")
        );

        let set = BrirSet::finish(
            front,
            48000,
            &BrirLoadOptions {
                max_length_s: 0.0,
                tail_floor_db: 120.0,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(set.is_lfe(3) && set.is_lfe(9) && !set.is_lfe(2));
        // Each response is cut where its own tail ends: the sum is as long as
        // the longer.
        let sum = set.lfe_pair(0).unwrap();
        let at = |e: usize, i: usize| set.pair(e, 0).left.get(i).copied().unwrap_or(0.0);
        assert_eq!(sum.taps(), set.pair(3, 0).taps().max(set.pair(9, 0).taps()));
        for (i, &x) in sum.left.iter().enumerate() {
            assert!((x - 0.5 * (at(3, i) + at(9, i))).abs() < 1e-7);
        }
    }

    /// A room is a 22.2 room only when all of its loudspeakers stand where
    /// System H's do; one moved off its place, or one too few, and there are
    /// no LFE loudspeakers to find.
    #[test]
    fn lfe_loudspeakers_are_found_only_in_a_whole_22_2_room() {
        let room = iosr_22_2_emitters(2.0);
        assert_eq!(lfe_emitters(&room).iter().filter(|&&l| l).count(), 2);
        assert!(!lfe_emitters(&room[..23]).contains(&true));
        let mut moved = room.clone();
        moved[0] = [0.0, -2.0, 0.0];
        assert!(!lfe_emitters(&moved).contains(&true));
        // Every other room keeps its emitters' count and a layout bus each.
        assert_eq!(measured_room_buses(&room[..5]), 6);
    }

    /// A set measured turning from one side to the other of where the
    /// listener faces faces the middle of that arc; one covering the circle
    /// keeps straight ahead, exactly, as does an arc centred on it.
    #[test]
    fn a_set_measured_over_an_arc_faces_its_middle() {
        let views = |azs: &mut dyn Iterator<Item = f32>| -> Vec<[f32; 3]> {
            azs.map(|a| sph(a, 0.0, 1.0)).collect()
        };
        // TU Berlin's Calypso: 0..180° in 1° steps about the centre at 90°,
        // where a loudspeaker stands and none at the file's +x.
        let calypso_speakers = [120.0, 90.0, 60.0, -160.0, -20.0];
        let calypso = views(&mut (0..=180).map(|a| a as f32));
        assert!(
            (front_azimuth(&calypso, false, &calypso_speakers).to_degrees() - 90.0).abs() < 1e-3
        );
        // The same arc without that evidence keeps the file's front: no
        // loudspeaker at its middle, or one straight ahead as well.
        assert_eq!(front_azimuth(&calypso, false, &[30.0, -30.0]), 0.0);
        assert_eq!(front_azimuth(&calypso, false, &[90.0, 0.0]), 0.0);
        // The BBC's: the circle in 2° steps.
        let bbc = views(&mut (0..180).map(|i| i as f32 * 2.0));
        assert_eq!(front_azimuth(&bbc, false, &[90.0]), 0.0);
        // TuBuRo's: ±80° in 2° steps.
        let tuburo = views(&mut (-40..=40).map(|i| i as f32 * 2.0));
        assert_eq!(front_azimuth(&tuburo, false, &[]), 0.0);
        // A circle with one measurement missing is still the circle.
        let gap = views(&mut (0..72).filter(|&i| i != 9).map(|i| i as f32 * 5.0));
        assert_eq!(front_azimuth(&gap, false, &[]), 0.0);
        // A narrow arc from straight ahead is a partial set of the front
        // (`rows_multispeaker_brir.sofa`: 0° to 60°).
        let partial = views(&mut (0..=6).map(|i| i as f32 * 10.0));
        assert_eq!(front_azimuth(&partial, false, &[30.0]), 0.0);
        // One orientation is where the listener faced.
        assert!(
            (front_azimuth(&views(&mut [30.0f32].into_iter()), false, &[]).to_degrees() - 30.0)
                .abs()
                < 1e-3
        );
    }

    /// The Calypso 5.0 set as its file has it - loudspeakers at SOFA
    /// azimuths 120 (L), 90 (C), 60 (R), -160 (Ls) and -20 (Rs), the head
    /// turned from 0° to 180° - is read facing its centre loudspeaker, from
    /// the measurement made facing it.
    #[test]
    fn the_calypso_set_is_read_facing_its_centre_loudspeaker() {
        let yaws: Vec<f32> = (0..=18).map(|i| i as f32 * 10.0).collect();
        let s = multi_speaker(
            &[120.0, 90.0, 60.0, -160.0, -20.0],
            &yaws,
            400,
            48000.0,
            0.0,
        );
        let room = ExtractedRoom::extract(&s.raw(), OrientationSelection::FrontOnly).unwrap();
        assert_eq!(room.orientations.len(), 1);
        assert!(
            room.orientations[0].0.abs() < 1e-3,
            "{:?}",
            room.orientations
        );
        let az = |p: [f32; 3]| p[0].atan2(p[1]).to_degrees();
        let got: Vec<f32> = room.emitters.iter().map(|&p| az(p)).collect();
        for (g, want) in got.iter().zip([-30.0, 0.0, 30.0, -110.0, 110.0]) {
            assert!((g - want).abs() < 0.5, "{got:?}");
        }
        // The pairs of the measurement facing the centre, the tenth.
        for (k, pair) in room.pairs.iter().enumerate() {
            let peak = pair
                .left
                .iter()
                .fold(0.0f32, |m, &x| if x.abs() > m.abs() { x } else { m });
            assert!(
                (peak - marker(k, 9, 0)).abs() < 1e-6,
                "loudspeaker {k}: {peak}"
            );
        }
    }

    /// A set that turns the head further one way than the other keeps the
    /// file's own front: views from 0° to 120° in 10° steps, a loudspeaker
    /// straight ahead and two at ±30°. The middle of that range (60°) is no
    /// front - no loudspeaker stands there - so the front-only selection is
    /// the measurement made facing +x, the first, and the centre loudspeaker
    /// stays in front.
    #[test]
    fn an_asymmetric_range_of_head_turns_keeps_the_files_front() {
        let yaws: Vec<f32> = (0..=12).map(|i| i as f32 * 10.0).collect();
        let s = multi_speaker(&[0.0, 30.0, -30.0], &yaws, 400, 48000.0, 0.0);
        let raw = s.raw();
        assert_eq!(
            ExtractedRoom::measurements(&raw, OrientationSelection::FrontOnly).unwrap(),
            0..1
        );
        let room = ExtractedRoom::extract(&raw, OrientationSelection::FrontOnly).unwrap();
        assert_eq!(room.orientations.len(), 1);
        assert!(
            room.orientations[0].0.abs() < 1e-3,
            "{:?}",
            room.orientations
        );
        let c = room.emitters[0];
        assert!(
            c[0].abs() < 1e-4 && (c[1] - 2.0).abs() < 1e-4,
            "centre loudspeaker: {c:?}"
        );
        // SOFA +30° (left) is the renderer's left.
        assert!(room.emitters[1][0] < -0.9, "{:?}", room.emitters[1]);
        for (k, pair) in room.pairs.iter().enumerate() {
            let peak = pair
                .left
                .iter()
                .fold(0.0f32, |m, &x| if x.abs() > m.abs() { x } else { m });
            assert!(
                (peak - marker(k, 0, 0)).abs() < 1e-6,
                "loudspeaker {k}: {peak}"
            );
        }
        // Every orientation keeps its own yaw, unturned: SOFA +120 (left)
        // is the renderer's -120.
        let all = ExtractedRoom::extract(&raw, OrientationSelection::All).unwrap();
        assert!(
            (all.orientations[0].0 + 120.0).abs() < 1e-3,
            "{:?}",
            all.orientations
        );
    }

    /// Fewer loudspeakers than a sound can be placed between are no room:
    /// the WDR control rooms' files measure one monitor each.
    #[cfg(feature = "sofa")]
    #[test]
    fn a_room_needs_three_loudspeakers() {
        for n in [1, 2] {
            let e = check_room_size(n).unwrap_err().to_string();
            assert!(e.contains("a room needs at least 3"), "{e}");
        }
        assert!(check_room_size(3).is_ok());
    }
}
