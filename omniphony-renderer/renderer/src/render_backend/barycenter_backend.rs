use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering, fence};

use anyhow::Result;

use super::room_transform::room_scaled_position;
use super::{BackendCapabilities, GainModel, RenderRequest, RenderResponse};
use crate::spatial_vbap::{Gains, MAX_SPEAKERS};
use crate::speaker_layout::SpeakerLayout;
use omniphony_geometry::f32::vec3::distance_sq;

/// Pans a source by choosing speaker weights (powers, summing to one) whose
/// weighted mean position lands on the source.
///
/// The weights minimise, over the simplex,
///
/// ```text
/// |Σ wᵢ pᵢ − t|²  +  localize · Σ wᵢ |pᵢ − t|²  +  RIDGE · Σ wᵢ²
/// ```
///
/// with `pᵢ` the speakers and `t` the source, both in room-scaled space. The
/// first term places the phantom source, the second favours speakers near it.
/// Neither makes the answer unique: wherever more speakers than needed can
/// reach the source, a whole family of weights does equally well. The third
/// term settles it. Among equally good weights it picks the most evenly spread
/// ones (the least `Σ wᵢ²`, equivalently the nearest to uniform weights), and
/// it is small enough to leave the first two terms alone: see [`RIDGE`].
///
/// With it the objective is strictly convex, so the answer is unique. It does
/// not depend on where a solver starts, and mirrored sources in a mirrored
/// layout get mirrored gains. It is computed exactly rather than iterated
/// towards: see [`minimise`].
pub struct BarycenterBackend {
    speaker_positions: Vec<[f32; 3]>,
    /// Localisation bias toward nearer speakers, baked at construction. It only
    /// changes via a topology rebuild, so it is not a per-request input.
    localize: f32,
    /// Speaker positions in the room of the latest request.
    room_memo: RoomMemo,
}

/// Weight of the `Σ wᵢ²` term that makes the minimiser unique.
///
/// It is a regulariser, so it also has a price. A speaker `L` away from a
/// source that sits on another speaker receives a weight of about `RIDGE / L²`
/// instead of none, a gain 60 dB down at this value; and speaker offsets
/// smaller than about `√RIDGE` (a millimetre per metre) are not exploited to
/// shave the position error. Much smaller values buy nothing audible and cost
/// digits: the solve works on quantities scaled by `1 / RIDGE`.
const RIDGE: f64 = 1e-6;

/// A speaker outside the support joins it only if it would take more weight
/// than this, which keeps rounding noise from moving speakers in and out. A
/// weight this small is a gain 90 dB down.
const ENTRY_TOLERANCE: f64 = 1e-9;

impl BarycenterBackend {
    pub fn new(speaker_positions: Vec<[f32; 3]>, localize: f32) -> Self {
        // Start from the identity room so the memo always holds a valid entry.
        let room = RoomParams {
            ratio: [1.0, 1.0, 1.0],
            rear: 1.0,
            lower: 1.0,
            center_blend: 0.0,
        };
        let room_memo = RoomMemo::new(&room, &RoomGeometry::new(&speaker_positions, &room));
        Self {
            speaker_positions,
            localize: localize.max(0.0),
            room_memo,
        }
    }

    pub fn speaker_count(&self) -> usize {
        self.speaker_positions.len()
    }

    pub fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        debug_assert!(
            self.speaker_positions.len() <= MAX_SPEAKERS,
            "barycenter backend speaker count {} exceeds MAX_SPEAKERS {}",
            self.speaker_positions.len(),
            MAX_SPEAKERS
        );

        let target = room_scaled_position(
            req.adm_position.map(|value| value as f32),
            req.room_ratio,
            req.room_ratio_rear,
            req.room_ratio_lower,
            req.room_ratio_center_blend,
        );

        let speaker_count = self.speaker_positions.len();
        let mut gains = Gains::zeroed(speaker_count);
        if speaker_count == 0 {
            return RenderResponse { gains };
        }

        // A table build asks for every cell with the same room parameters, and a
        // realtime render for every object: the transformed speakers are shared
        // by all those requests.
        let room_params = RoomParams::of(req);
        let room = self
            .room_memo
            .get_or_compute(&room_params, speaker_count, || {
                RoomGeometry::new(&self.speaker_positions, &room_params)
            });

        // A source on a speaker belongs to that speaker alone.
        for index in 0..speaker_count {
            if distance_sq(room.speakers[index], target) <= f32::EPSILON {
                gains.set(index, 1.0);
                return RenderResponse { gains };
            }
        }

        let problem = Problem::new(&room.speakers[..speaker_count], target, self.localize);
        let start = problem.cold_start();
        let solution = minimise(&problem, &start);
        for index in 0..speaker_count {
            gains.set(index, solution.weights[index].max(0.0).sqrt() as f32);
        }

        RenderResponse { gains }
    }

    pub fn save_to_file(
        &self,
        _path: &std::path::Path,
        _speaker_layout: &SpeakerLayout,
    ) -> Result<()> {
        Err(anyhow::anyhow!(
            "Saving a precomputed table is only supported for the VBAP backend"
        ))
    }
}

impl GainModel for BarycenterBackend {
    fn backend_id(&self) -> &'static str {
        "barycenter"
    }

    fn backend_label(&self) -> &'static str {
        "Barycenter"
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            supports_realtime: true,
            supports_precomputed_polar: true,
            supports_precomputed_cartesian: true,
            supports_position_interpolation: true,
            supports_distance_model: false,
            supports_spread: false,
            supports_spread_from_distance: false,
            supports_event_size: false,
            supports_distance_diffuse: false,
            supports_table_export: false,
        }
    }

    fn speaker_count(&self) -> usize {
        BarycenterBackend::speaker_count(self)
    }

    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        BarycenterBackend::compute_gains(self, req)
    }

    fn save_to_file(&self, path: &std::path::Path, speaker_layout: &SpeakerLayout) -> Result<()> {
        BarycenterBackend::save_to_file(self, path, speaker_layout)
    }
}

/// The room parameters of a request: everything `room_scaled_position` reads
/// besides the position itself.
#[derive(Clone, Copy)]
struct RoomParams {
    ratio: [f32; 3],
    rear: f32,
    lower: f32,
    center_blend: f32,
}

impl RoomParams {
    const WORDS: usize = 6;

    fn of(req: &RenderRequest) -> Self {
        Self {
            ratio: req.room_ratio,
            rear: req.room_ratio_rear,
            lower: req.room_ratio_lower,
            center_blend: req.room_ratio_center_blend,
        }
    }

    /// Bit patterns, so that two rooms are the same only when the transform
    /// cannot tell them apart (`0.0` and `-0.0` differ, a NaN equals itself).
    fn to_bits(self) -> [u32; Self::WORDS] {
        [
            self.ratio[0].to_bits(),
            self.ratio[1].to_bits(),
            self.ratio[2].to_bits(),
            self.rear.to_bits(),
            self.lower.to_bits(),
            self.center_blend.to_bits(),
        ]
    }
}

/// What the solver needs from the room parameters alone: the speakers in
/// room-scaled space.
#[derive(Clone, Copy)]
struct RoomGeometry {
    speakers: [[f32; 3]; MAX_SPEAKERS],
}

impl RoomGeometry {
    fn new(speaker_positions: &[[f32; 3]], room: &RoomParams) -> Self {
        let mut speakers = [[0.0f32; 3]; MAX_SPEAKERS];
        for (transformed, speaker) in speakers.iter_mut().zip(speaker_positions) {
            *transformed = room_scaled_position(
                *speaker,
                room.ratio,
                room.rear,
                room.lower,
                room.center_blend,
            );
        }
        Self { speakers }
    }
}

/// Single-entry memo of the [`RoomGeometry`] of the latest room parameters.
///
/// `compute_gains` runs concurrently on the workers of a table build and on the
/// render thread, and must neither block nor allocate there. The memo is a
/// sequence lock over plain atomics: a reader copies the entry and keeps the
/// copy only if no writer was active meanwhile, a writer publishes only if it
/// wins the entry outright, and whoever loses computes the geometry on its own
/// stack and moves on. Nobody waits for anybody.
struct RoomMemo {
    /// Even while the entry is stable, odd while a writer is replacing it.
    sequence: AtomicUsize,
    room: [AtomicU32; RoomParams::WORDS],
    speakers: [[AtomicU32; 3]; MAX_SPEAKERS],
}

impl RoomMemo {
    fn new(room: &RoomParams, geometry: &RoomGeometry) -> Self {
        let room = room.to_bits();
        Self {
            sequence: AtomicUsize::new(0),
            room: std::array::from_fn(|word| AtomicU32::new(room[word])),
            speakers: std::array::from_fn(|index| {
                geometry.speakers[index].map(|coordinate| AtomicU32::new(coordinate.to_bits()))
            }),
        }
    }

    /// The geometry for `room`: the memoised one when the entry is for `room`,
    /// else `compute()`, which then replaces the entry unless another thread is
    /// writing it.
    fn get_or_compute(
        &self,
        room: &RoomParams,
        speaker_count: usize,
        compute: impl FnOnce() -> RoomGeometry,
    ) -> RoomGeometry {
        let room = room.to_bits();
        let speaker_count = speaker_count.min(MAX_SPEAKERS);

        let sequence = self.sequence.load(Ordering::Acquire);
        let stable = sequence & 1 == 0;
        if stable
            && self
                .room
                .iter()
                .zip(&room)
                .all(|(held, wanted)| held.load(Ordering::Relaxed) == *wanted)
        {
            let mut geometry = RoomGeometry {
                speakers: [[0.0; 3]; MAX_SPEAKERS],
            };
            for (speaker, held) in geometry.speakers[..speaker_count]
                .iter_mut()
                .zip(&self.speakers)
            {
                *speaker = held
                    .each_ref()
                    .map(|coordinate| f32::from_bits(coordinate.load(Ordering::Relaxed)));
            }
            // The copy is consistent only if the sequence has not moved since it
            // was first read. The fence orders this second read after the
            // relaxed loads above; it pairs with the writer's release stores.
            fence(Ordering::Acquire);
            if self.sequence.load(Ordering::Relaxed) == sequence {
                return geometry;
            }
        }

        let geometry = compute();
        if stable
            && self
                .sequence
                .compare_exchange(
                    sequence,
                    sequence.wrapping_add(1),
                    Ordering::Acquire,
                    Ordering::Relaxed,
                )
                .is_ok()
        {
            for (held, word) in self.room.iter().zip(room) {
                held.store(word, Ordering::Release);
            }
            for (held, speaker) in self.speakers[..speaker_count]
                .iter()
                .zip(&geometry.speakers)
            {
                for (held, coordinate) in held.iter().zip(speaker) {
                    held.store(coordinate.to_bits(), Ordering::Release);
                }
            }
            self.sequence
                .store(sequence.wrapping_add(2), Ordering::Release);
        }
        geometry
    }
}

/// One request, as the solver sees it: in double precision.
struct Problem {
    speakers: [[f64; 3]; MAX_SPEAKERS],
    target: [f64; 3],
    localize: f64,
    speaker_count: usize,
    nearest: usize,
}

impl Problem {
    fn new(speakers: &[[f32; 3]], target: [f32; 3], localize: f32) -> Self {
        let speaker_count = speakers.len().min(MAX_SPEAKERS);
        let target = target.map(f64::from);
        let mut problem = Self {
            speakers: [[0.0; 3]; MAX_SPEAKERS],
            target,
            localize: f64::from(localize),
            speaker_count,
            nearest: 0,
        };
        let mut nearest_distance_sq = f64::INFINITY;
        for (index, speaker) in speakers.iter().take(speaker_count).enumerate() {
            let speaker = speaker.map(f64::from);
            let distance_sq = (speaker[0] - target[0]) * (speaker[0] - target[0])
                + (speaker[1] - target[1]) * (speaker[1] - target[1])
                + (speaker[2] - target[2]) * (speaker[2] - target[2]);
            problem.speakers[index] = speaker;
            if distance_sq < nearest_distance_sq {
                nearest_distance_sq = distance_sq;
                problem.nearest = index;
            }
        }
        problem
    }

    /// Where a solve starts when nothing better is known. The minimiser does
    /// not depend on it, only the number of pivots does: with a localisation
    /// term the weight ends up on a few speakers around the source, so start
    /// from the nearest one; without, it spreads, so start from all of them.
    fn cold_start(&self) -> [f64; MAX_SPEAKERS] {
        let mut weights = [0.0; MAX_SPEAKERS];
        if self.localize > 0.0 {
            weights[self.nearest] = 1.0;
        } else {
            weights[..self.speaker_count].fill(1.0 / self.speaker_count as f64);
        }
        weights
    }

    /// The pivot budget of [`minimise`]: several times what any layout needs,
    /// there to bound the time of a solve whatever the input.
    fn max_pivots(&self) -> usize {
        4 * self.speaker_count + 8
    }
}

struct Solution {
    weights: [f64; MAX_SPEAKERS],
    /// False when the pivot budget ran out first. The weights are then a valid
    /// set (non-negative, summing to one) but not the minimiser; the backend
    /// plays them as they are rather than fail on the render thread.
    #[cfg_attr(not(test), allow(dead_code))]
    converged: bool,
    #[cfg_attr(not(test), allow(dead_code))]
    pivots: usize,
}

/// Minimise the objective of [`BarycenterBackend`] over the simplex, exactly.
///
/// A primal active-set method. It keeps a feasible set of weights and a
/// support (the speakers allowed a non-zero weight), and repeats: find the
/// minimiser on the support with the signs left free ([`minimiser_on`]); if it
/// would make a weight negative, move towards it until the first weight
/// reaches zero and take that speaker out; otherwise adopt it and let in the
/// speaker outside the support that most wants weight, or stop when none does.
/// Every move lowers the objective and the supports are finitely many, so it
/// ends at the minimiser: a few pivots, where a gradient iteration needs
/// hundreds of steps to settle.
///
/// `start` is any feasible set of weights. The result is the minimiser on the
/// final support, computed from scratch, so the start does not leak into it.
fn minimise(problem: &Problem, start: &[f64; MAX_SPEAKERS]) -> Solution {
    let speaker_count = problem.speaker_count;
    let mut weights = *start;
    let mut in_support = [false; MAX_SPEAKERS];
    for index in 0..speaker_count {
        in_support[index] = weights[index] > 0.0;
    }
    // A speaker that is pushed out by the very pivot that follows its entry
    // would take a weight below rounding: leave it out for good, or the two
    // pivots could repeat forever.
    let mut barred = [false; MAX_SPEAKERS];
    let mut entered = usize::MAX;
    let mut candidate = [0.0f64; MAX_SPEAKERS];

    let max_pivots = problem.max_pivots();
    for pivot in 0..max_pivots {
        minimiser_on(problem, &in_support, &mut candidate);

        // How far the weights can move towards the candidate before one of
        // them reaches zero.
        let mut reach = 1.0f64;
        let mut leaving = usize::MAX;
        for index in 0..speaker_count {
            if in_support[index] && candidate[index] < 0.0 {
                let limit = weights[index] / (weights[index] - candidate[index]);
                if limit < reach {
                    reach = limit;
                    leaving = index;
                }
            }
        }
        if leaving != usize::MAX {
            for index in 0..speaker_count {
                if in_support[index] {
                    weights[index] += reach * (candidate[index] - weights[index]);
                }
            }
            weights[leaving] = 0.0;
            in_support[leaving] = false;
            barred[leaving] |= leaving == entered;
            entered = usize::MAX;
            continue;
        }

        let mut entering = usize::MAX;
        let mut most = ENTRY_TOLERANCE;
        for index in 0..speaker_count {
            if in_support[index] {
                weights[index] = candidate[index];
            } else if !barred[index] && candidate[index] > most {
                most = candidate[index];
                entering = index;
            }
        }
        if entering == usize::MAX {
            return Solution {
                weights,
                converged: true,
                pivots: pivot + 1,
            };
        }
        in_support[entering] = true;
        entered = entering;
    }

    Solution {
        weights,
        converged: false,
        pivots: max_pivots,
    }
}

/// The minimiser of the objective over the weights of `in_support` that sum to
/// one, signs left free.
///
/// `candidate` receives it for the speakers of the support. For the others it
/// receives the weight they are being denied, in the same units: positive when
/// letting the speaker in would lower the objective.
///
/// Setting the gradient equal on the support gives every weight in terms of
/// one 3-vector `u`: with `k` the size of the support, `p̄` its centroid,
/// `qᵢ = pᵢ − p̄` and `κ = localize / (2 · RIDGE)`,
///
/// ```text
/// wᵢ = 1/k − κ (|qᵢ|² − mean |q|²) − qᵢ · u
/// (S + RIDGE · I) u = (1 + localize)(p̄ − t) − κ Σ qᵢ (|qᵢ|² − mean |q|²)
/// ```
///
/// where `S = Σ qᵢ qᵢᵀ` is the scatter matrix of the support and the sums run
/// over it. Measuring the localisation term from the centroid rather than from
/// the source keeps the `1 / RIDGE` terms, which cancel in the weights, as
/// small as the support is tight. The cost is linear in the speaker count.
fn minimiser_on(
    problem: &Problem,
    in_support: &[bool; MAX_SPEAKERS],
    candidate: &mut [f64; MAX_SPEAKERS],
) {
    let speaker_count = problem.speaker_count;
    let speakers = &problem.speakers;

    let mut size = 0.0f64;
    let mut centroid = [0.0f64; 3];
    for index in 0..speaker_count {
        if in_support[index] {
            size += 1.0;
            centroid[0] += speakers[index][0];
            centroid[1] += speakers[index][1];
            centroid[2] += speakers[index][2];
        }
    }
    let share = 1.0 / size;
    centroid = centroid.map(|sum| sum * share);

    // Scatter matrix of the support (upper triangle) and third moments.
    let (mut sxx, mut sxy, mut sxz, mut syy, mut syz, mut szz) = (0.0f64, 0.0, 0.0, 0.0, 0.0, 0.0);
    let mut cubic = [0.0f64; 3];
    for index in 0..speaker_count {
        if in_support[index] {
            let x = speakers[index][0] - centroid[0];
            let y = speakers[index][1] - centroid[1];
            let z = speakers[index][2] - centroid[2];
            let radius_sq = x * x + y * y + z * z;
            sxx += x * x;
            sxy += x * y;
            sxz += x * z;
            syy += y * y;
            syz += y * z;
            szz += z * z;
            cubic[0] += x * radius_sq;
            cubic[1] += y * radius_sq;
            cubic[2] += z * radius_sq;
        }
    }
    // Σ qᵢ = 0, so the mean of |q|² drops out of the right-hand side.
    let kappa = problem.localize / (2.0 * RIDGE);
    let mean_radius_sq = (sxx + syy + szz) * share;
    let aim = 1.0 + problem.localize;
    let rhs = [
        aim * (centroid[0] - problem.target[0]) - kappa * cubic[0],
        aim * (centroid[1] - problem.target[1]) - kappa * cubic[1],
        aim * (centroid[2] - problem.target[2]) - kappa * cubic[2],
    ];

    // Solve (S + RIDGE · I) u = rhs by LDLᵀ. The ridge keeps every pivot at or
    // above RIDGE, however flat the support.
    let d0 = sxx + RIDGE;
    let l10 = sxy / d0;
    let l20 = sxz / d0;
    let d1 = syy + RIDGE - l10 * sxy;
    let l21 = (syz - l20 * sxy) / d1;
    let d2 = szz + RIDGE - l20 * sxz - l21 * l21 * d1;
    let f0 = rhs[0];
    let f1 = rhs[1] - l10 * f0;
    let f2 = rhs[2] - l20 * f0 - l21 * f1;
    let u2 = f2 / d2;
    let u1 = f1 / d1 - l21 * u2;
    let u0 = f0 / d0 - l10 * u1 - l20 * u2;

    for index in 0..speaker_count {
        let x = speakers[index][0] - centroid[0];
        let y = speakers[index][1] - centroid[1];
        let z = speakers[index][2] - centroid[2];
        candidate[index] =
            share - kappa * (x * x + y * y + z * z - mean_radius_sq) - (x * u0 + y * u1 + z * u2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(position: [f64; 3]) -> RenderRequest {
        RenderRequest {
            adm_position: position,
            event_size: [0.0, 0.0, 0.0],
            room_ratio: [1.0, 1.0, 1.0],
            room_ratio_rear: 1.0,
            room_ratio_lower: 1.0,
            room_ratio_center_blend: 0.0,
            use_distance_diffuse: false,
            diffuse_mirror_axes: crate::spatial_vbap::MirrorAxes::default(),
            distance_diffuse_threshold: 1.0,
            distance_diffuse_curve: 1.0,
            distance_model: crate::spatial_vbap::DistanceModel::None,
        }
    }

    #[derive(Clone, Copy)]
    struct Room {
        ratio: [f32; 3],
        rear: f32,
        lower: f32,
        center_blend: f32,
    }

    const ROOMS: [Room; 4] = [
        Room {
            ratio: [1.0, 1.0, 1.0],
            rear: 1.0,
            lower: 1.0,
            center_blend: 0.0,
        },
        // Deep front, shallow floor, off-centre depth warp.
        Room {
            ratio: [1.0, 2.0, 1.0],
            rear: 1.0,
            lower: 0.47,
            center_blend: 0.59,
        },
        Room {
            ratio: [0.6, 1.4, 0.8],
            rear: 0.5,
            lower: 0.3,
            center_blend: 1.0,
        },
        Room {
            ratio: [1.7, 0.75, 1.2],
            rear: 1.3,
            lower: 1.0,
            center_blend: 0.25,
        },
    ];

    fn request_in(room: Room, position: [f64; 3]) -> RenderRequest {
        RenderRequest {
            room_ratio: room.ratio,
            room_ratio_rear: room.rear,
            room_ratio_lower: room.lower,
            room_ratio_center_blend: room.center_blend,
            ..request(position)
        }
    }

    /// A 14-speaker home layout: an asymmetric bed, a rear centre below the
    /// horizon, a sub at the nadir and two overheads off the room's axes.
    fn home_layout() -> Vec<[f32; 3]> {
        vec![
            [-1.0, 1.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, -1.0, -0.14787422],
            [-1.0, -1.0, 0.0],
            [1.0, -1.0, 0.0],
            [-1.0, 1.0, 1.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            [1.0, -1.0, 1.0],
            [-1.0, -1.0, 1.0],
            [0.0, 0.0, -1.0],
            [-0.48387095, 1.8626451e-09, 1.0],
            [0.5483871, 1.8626451e-09, 1.0],
        ]
    }

    fn subset(layout: &[[f32; 3]], indices: &[usize]) -> Vec<[f32; 3]> {
        indices.iter().map(|&index| layout[index]).collect()
    }

    fn layout_714() -> Vec<[f32; 3]> {
        vec![
            [-1.0, 1.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [-1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [-1.0, -1.0, 0.0],
            [1.0, -1.0, 0.0],
            [-1.0, 1.0, 1.0],
            [1.0, 1.0, 1.0],
            [-1.0, -1.0, 1.0],
            [1.0, -1.0, 1.0],
        ]
    }

    /// Deterministic values in `[-1, 1)`.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> f32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 40) as f32 / (1u64 << 23) as f32) - 1.0
        }
    }

    fn layouts() -> Vec<Vec<[f32; 3]>> {
        let home = home_layout();
        let mut rng = Lcg(7);
        let full: Vec<[f32; 3]> = (0..MAX_SPEAKERS)
            .map(|_| [rng.next(), rng.next(), rng.next()])
            .collect();
        vec![
            // The whole layout, its spatializable speakers, and the speaker
            // sets of its crossover bands.
            subset(&home, &[0, 1, 2, 3, 5, 7, 9, 10, 11, 12, 13]),
            subset(&home, &[0, 1, 2, 3, 5, 7, 9, 10, 12, 13]),
            subset(&home, &[0, 1, 2, 3, 5, 7, 12, 13]),
            subset(&home, &[0, 1, 2, 5, 11, 12, 13]),
            subset(&home, &[11, 12, 13]),
            home,
            layout_714(),
            vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
            ],
            vec![[-1.0, 0.5, 0.0], [1.0, 0.5, 0.0]],
            vec![[0.3, -0.2, 0.9]],
            full,
        ]
    }

    /// An `nx × ny × nz` lattice over the cube (speakers sit on its nodes, so it
    /// covers the exact-speaker exit), then points in and around the cube.
    fn targets(nx: usize, ny: usize, nz: usize, scattered: usize) -> Vec<[f64; 3]> {
        let axis = |index: usize, count: usize| -1.0 + 2.0 * index as f64 / (count - 1) as f64;
        let mut targets = Vec::with_capacity(nx * ny * nz + scattered);
        for zi in 0..nz {
            for yi in 0..ny {
                for xi in 0..nx {
                    targets.push([axis(xi, nx), axis(yi, ny), axis(zi, nz)]);
                }
            }
        }
        let mut rng = Lcg(11);
        for _ in 0..scattered {
            targets.push([
                1.25 * rng.next() as f64,
                1.25 * rng.next() as f64,
                1.25 * rng.next() as f64,
            ]);
        }
        targets
    }

    fn problem(positions: &[[f32; 3]], localize: f32, req: &RenderRequest) -> Problem {
        let scale = |position: [f32; 3]| {
            room_scaled_position(
                position,
                req.room_ratio,
                req.room_ratio_rear,
                req.room_ratio_lower,
                req.room_ratio_center_blend,
            )
        };
        let speakers: Vec<[f32; 3]> = positions.iter().map(|speaker| scale(*speaker)).collect();
        Problem::new(
            &speakers,
            scale(req.adm_position.map(|value| value as f32)),
            localize,
        )
    }

    fn distance_sq_to_target(problem: &Problem, index: usize) -> f64 {
        (0..3)
            .map(|axis| (problem.speakers[index][axis] - problem.target[axis]).powi(2))
            .sum()
    }

    /// The objective, straight from its definition.
    fn objective(problem: &Problem, weights: &[f64]) -> f64 {
        let count = problem.speaker_count;
        let mut error = problem.target.map(|coordinate| -coordinate);
        let mut linear = 0.0;
        let mut ridge = 0.0;
        for index in 0..count {
            for axis in 0..3 {
                error[axis] += problem.speakers[index][axis] * weights[index];
            }
            linear += problem.localize * distance_sq_to_target(problem, index) * weights[index];
            ridge += RIDGE * weights[index] * weights[index];
        }
        error.iter().map(|e| e * e).sum::<f64>() + linear + ridge
    }

    /// How much the objective could still drop from `weights`, at most: the
    /// gap `∇f(w) · (w − v)` maximised over the simplex, which bounds
    /// `f(w) − min f` for a convex `f`.
    fn optimality_gap(problem: &Problem, weights: &[f64]) -> f64 {
        let count = problem.speaker_count;
        let mut error = problem.target.map(|coordinate| -coordinate);
        for index in 0..count {
            for axis in 0..3 {
                error[axis] += problem.speakers[index][axis] * weights[index];
            }
        }
        let mut mean = 0.0;
        let mut least = f64::INFINITY;
        for index in 0..count {
            let speaker = problem.speakers[index];
            let gradient = 2.0
                * (speaker[0] * error[0] + speaker[1] * error[1] + speaker[2] * error[2])
                + problem.localize * distance_sq_to_target(problem, index)
                + 2.0 * RIDGE * weights[index];
            mean += weights[index] * gradient;
            least = least.min(gradient);
        }
        mean - least
    }

    /// The minimiser found the slow way: for every support, solve the
    /// stationarity equations as one dense linear system, and keep the best of
    /// the supports whose solution has no negative weight.
    fn minimise_by_enumeration(problem: &Problem) -> ([f64; MAX_SPEAKERS], f64) {
        let count = problem.speaker_count;
        let mut best = ([0.0; MAX_SPEAKERS], f64::INFINITY);
        for mask in 1u32..(1 << count) {
            let support: Vec<usize> = (0..count).filter(|index| mask >> index & 1 == 1).collect();
            let size = support.len();
            // Unknowns: the weights of the support, then the multiplier of the
            // sum-to-one constraint.
            let mut system = vec![vec![0.0f64; size + 2]; size + 1];
            for (row, &i) in support.iter().enumerate() {
                for (column, &j) in support.iter().enumerate() {
                    let (a, b) = (problem.speakers[i], problem.speakers[j]);
                    system[row][column] = 2.0 * (a[0] * b[0] + a[1] * b[1] + a[2] * b[2]);
                }
                system[row][row] += 2.0 * RIDGE;
                system[row][size] = -1.0;
                let a = problem.speakers[i];
                let t = problem.target;
                system[row][size + 1] = 2.0 * (a[0] * t[0] + a[1] * t[1] + a[2] * t[2])
                    - problem.localize * distance_sq_to_target(problem, i);
                system[size][row] = 1.0;
            }
            system[size][size + 1] = 1.0;
            // Gaussian elimination with partial pivoting.
            let unknowns = size + 1;
            for column in 0..unknowns {
                let pivot = (column..unknowns)
                    .max_by(|a, b| {
                        system[*a][column]
                            .abs()
                            .total_cmp(&system[*b][column].abs())
                    })
                    .unwrap();
                system.swap(column, pivot);
                for row in 0..unknowns {
                    if row != column {
                        let factor = system[row][column] / system[column][column];
                        for k in column..=unknowns {
                            system[row][k] -= factor * system[column][k];
                        }
                    }
                }
            }
            // The system is as ill-conditioned as the ridge is small, so a
            // weight of the true minimiser may come out a hair below zero.
            let mut weights = [0.0f64; MAX_SPEAKERS];
            let mut feasible = true;
            let mut sum = 0.0;
            for (row, &i) in support.iter().enumerate() {
                weights[i] = system[row][unknowns] / system[row][row];
                feasible &= weights[i] >= -1e-8;
                weights[i] = weights[i].max(0.0);
                sum += weights[i];
            }
            for weight in &mut weights {
                *weight /= sum;
            }
            let value = objective(problem, &weights);
            if feasible && value < best.1 {
                best = (weights, value);
            }
        }
        best
    }

    #[test]
    fn solver_agrees_with_enumerating_every_support() {
        let layouts = layouts();
        // The layouts small enough to enumerate: 2 to 8 speakers.
        let small: Vec<&Vec<[f32; 3]>> = layouts
            .iter()
            .filter(|layout| (2..=8).contains(&layout.len()))
            .collect();
        assert!(small.len() >= 5);
        let mut worst = 0.0f64;
        for positions in small {
            for localize in [0.0, 0.5, 4.0] {
                for (call, target) in targets(4, 4, 3, 24).into_iter().enumerate() {
                    let req = request_in(ROOMS[call % ROOMS.len()], target);
                    let problem = problem(positions, localize, &req);
                    let solution = minimise(&problem, &problem.cold_start());
                    let (expected, expected_value) = minimise_by_enumeration(&problem);
                    assert!(solution.converged);
                    let value = objective(&problem, &solution.weights);
                    assert!(
                        (value - expected_value).abs() <= 1e-8 * (1.0 + expected_value.abs()),
                        "objective {value:e} vs {expected_value:e} at {target:?}"
                    );
                    for index in 0..positions.len() {
                        let gain = solution.weights[index].sqrt();
                        let expected_gain = expected[index].sqrt();
                        worst = worst.max((gain - expected_gain).abs());
                        assert!(
                            (gain - expected_gain).abs() <= 1e-4,
                            "{} speakers, localize {localize}, target {target:?}, speaker \
                             {index}: gain {gain:e} vs {expected_gain:e}",
                            positions.len()
                        );
                    }
                }
            }
        }
        println!("largest gain difference against enumeration: {worst:e}");
    }

    #[test]
    fn every_solve_converges_to_the_minimiser() {
        let mut most_pivots = 0;
        let mut worst_gap = 0.0f64;
        for (layout, positions) in layouts().into_iter().enumerate() {
            for localize in [0.0, 0.05, 0.5, 4.0] {
                for (call, target) in targets(11, 11, 7, 300).into_iter().enumerate() {
                    let req = request_in(ROOMS[call % ROOMS.len()], target);
                    let problem = problem(&positions, localize, &req);
                    let solution = minimise(&problem, &problem.cold_start());
                    let context =
                        || format!("layout {layout}, localize {localize}, target {target:?}");
                    assert!(solution.converged, "out of pivots: {}", context());
                    most_pivots = most_pivots.max(solution.pivots);

                    let weights = &solution.weights[..positions.len()];
                    assert!(weights.iter().all(|weight| *weight >= 0.0), "{}", context());
                    let sum: f64 = weights.iter().sum();
                    assert!((sum - 1.0).abs() <= 1e-7, "sum {sum}: {}", context());

                    let gap = optimality_gap(&problem, weights);
                    worst_gap = worst_gap.max(gap);
                    assert!(gap <= 1e-7, "gap {gap:e}: {}", context());
                }
            }
        }
        println!("most pivots {most_pivots}, largest optimality gap {worst_gap:e}");
        // The budget is 4 n + 8: nothing should come near it.
        assert!(most_pivots <= 2 * MAX_SPEAKERS + 8, "{most_pivots} pivots");
    }

    #[test]
    fn the_minimiser_does_not_depend_on_the_start() {
        let mut rng = Lcg(5);
        let mut worst = 0.0f64;
        for positions in layouts() {
            let count = positions.len();
            for localize in [0.0, 0.5] {
                for (call, target) in targets(7, 7, 5, 100).into_iter().enumerate() {
                    let req = request_in(ROOMS[call % ROOMS.len()], target);
                    let problem = problem(&positions, localize, &req);
                    let reference = minimise(&problem, &problem.cold_start());

                    let mut uniform = [0.0; MAX_SPEAKERS];
                    uniform[..count].fill(1.0 / count as f64);
                    let mut vertex = [0.0; MAX_SPEAKERS];
                    vertex[call % count] = 1.0;
                    // A random subset with random weights.
                    let mut scattered = [0.0; MAX_SPEAKERS];
                    let mut sum = 0.0;
                    for weight in &mut scattered[..count] {
                        *weight = f64::from(rng.next().max(0.0));
                        sum += *weight;
                    }
                    if sum == 0.0 {
                        scattered[0] = 1.0;
                        sum = 1.0;
                    }
                    for weight in &mut scattered[..count] {
                        *weight /= sum;
                    }

                    for start in [uniform, vertex, scattered] {
                        let solution = minimise(&problem, &start);
                        assert!(solution.converged);
                        for index in 0..count {
                            let difference = (solution.weights[index].sqrt()
                                - reference.weights[index].sqrt())
                            .abs();
                            worst = worst.max(difference);
                            assert!(
                                difference <= 1e-4,
                                "{count} speakers, localize {localize}, target {target:?}, \
                                 speaker {index}: {:e} vs {:e}",
                                solution.weights[index],
                                reference.weights[index]
                            );
                        }
                    }
                }
            }
        }
        println!("largest gain difference between starts: {worst:e}");
    }

    #[test]
    fn mirrored_targets_get_mirrored_gains() {
        // Left/right pairs of the 7.1.4, and the centre on itself.
        let mirror = [1, 0, 2, 4, 3, 6, 5, 8, 7, 10, 9];
        let positions = layout_714();
        let mut worst = 0.0f32;
        for localize in [0.0, 0.5, 2.0] {
            let backend = BarycenterBackend::new(positions.clone(), localize);
            for room in [ROOMS[0], ROOMS[1]] {
                for target in targets(21, 21, 11, 500) {
                    let gains = backend.compute_gains(&request_in(room, target)).gains;
                    let mirrored_target = [-target[0], target[1], target[2]];
                    let mirrored = backend
                        .compute_gains(&request_in(room, mirrored_target))
                        .gains;
                    for index in 0..positions.len() {
                        let difference = (gains[index] - mirrored[mirror[index]]).abs();
                        worst = worst.max(difference);
                        assert!(
                            difference <= 1e-4,
                            "localize {localize}, target {target:?}, speaker {index}: {} vs {}",
                            gains[index],
                            mirrored[mirror[index]]
                        );
                    }
                }
            }
        }
        println!("largest left/right asymmetry: {worst:e}");
    }

    #[test]
    fn a_source_between_coplanar_speakers_spreads_evenly() {
        // Four speakers at the corners of a square reach its centre with any
        // weights `(a, b, a, b)`, `a + b = 1/2`: the rule picks the even ones.
        let positions = vec![
            [-1.0, -1.0, 0.0],
            [1.0, -1.0, 0.0],
            [1.0, 1.0, 0.0],
            [-1.0, 1.0, 0.0],
        ];
        for localize in [0.0, 0.5] {
            let backend = BarycenterBackend::new(positions.clone(), localize);
            let gains = backend.compute_gains(&request([0.0, 0.0, 0.0])).gains;
            for gain in gains.iter() {
                assert!(
                    (gain - 0.5).abs() < 1e-4,
                    "localize {localize}: {:?}",
                    &gains[..]
                );
            }
        }
    }

    #[test]
    fn backend_meets_the_gain_model_contract() {
        use crate::backend_conformance::{ConformanceOptions, check};

        // Finite, non-negative gains of unit energy, and no jump in the gains
        // for a small step of the source, in a regular and in an uneven layout.
        for positions in [layout_714(), layouts().swap_remove(0)] {
            for localize in [0.0, 0.5, 4.0] {
                let backend = BarycenterBackend::new(positions.clone(), localize);
                let options = ConformanceOptions {
                    energy_bounds: Some((0.999, 1.001)),
                    ..ConformanceOptions::default()
                };
                check(&backend, &options).assert_passed();
            }
        }
    }

    #[test]
    fn the_room_memo_is_safe_across_rooms_and_threads() {
        let positions = layouts().swap_remove(0);
        let localize = 0.5;
        let backend = BarycenterBackend::new(positions.clone(), localize);
        let targets = targets(9, 9, 7, 200);
        // A backend used once has only ever seen the room of that request.
        let fresh = |req: &RenderRequest| {
            BarycenterBackend::new(positions.clone(), localize)
                .compute_gains(req)
                .gains
        };
        std::thread::scope(|scope| {
            for thread in 0..8usize {
                let (backend, targets, fresh) = (&backend, &targets, &fresh);
                scope.spawn(move || {
                    // Half of the threads hold one room each, as the workers of a
                    // table build do; the others switch rooms at different paces,
                    // so the memo is read while it is being replaced.
                    for round in 0..3 {
                        for (call, target) in targets.iter().enumerate() {
                            let room = if thread < 4 {
                                thread % 2
                            } else {
                                (call / (thread - 3) + round) % ROOMS.len()
                            };
                            let req = request_in(ROOMS[room], *target);
                            let gains = backend.compute_gains(&req).gains;
                            let expected = fresh(&req);
                            assert!(
                                gains
                                    .iter()
                                    .zip(expected.iter())
                                    .all(|(a, b)| a.to_bits() == b.to_bits()),
                                "thread {thread}, room {room}, target {target:?}"
                            );
                        }
                    }
                });
            }
        });
    }

    #[test]
    fn barycenter_backend_normalizes_energy() {
        let backend = BarycenterBackend::new(
            vec![
                [-1.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
            ],
            0.0,
        );

        let gains = backend.compute_gains(&request([0.2, 0.4, 0.1])).gains;
        let energy: f32 = gains.iter().map(|gain| gain * gain).sum();
        assert!((energy - 1.0).abs() < 1e-4, "energy={energy}");
    }

    #[test]
    fn barycenter_backend_reconstructs_interior_target() {
        let backend = BarycenterBackend::new(
            vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
            ],
            0.0,
        );

        let target = [0.2, 0.3, 0.1];
        let gains = backend.compute_gains(&request(target)).gains;

        let mut effective = [0.0f32; 3];
        for (index, gain) in gains.iter().copied().enumerate() {
            let weight = gain * gain;
            effective[0] += backend.speaker_positions[index][0] * weight;
            effective[1] += backend.speaker_positions[index][1] * weight;
            effective[2] += backend.speaker_positions[index][2] * weight;
        }

        assert!((effective[0] - target[0] as f32).abs() < 1e-3);
        assert!((effective[1] - target[1] as f32).abs() < 1e-3);
        assert!((effective[2] - target[2] as f32).abs() < 1e-3);
    }

    #[test]
    fn barycenter_backend_hits_exact_speaker() {
        let backend = BarycenterBackend::new(
            vec![[-1.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            0.0,
        );

        let gains = backend.compute_gains(&request([1.0, 0.0, 0.0])).gains;
        assert!(gains[1] > 0.999);
        assert!(gains[0] < 1e-6);
        assert!(gains[2] < 1e-6);
    }

    #[test]
    fn barycenter_backend_localize_biases_toward_near_speakers() {
        let positions = vec![
            [-1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0],
        ];
        // `localize` is now baked at construction, so compare two backends.
        let base_backend = BarycenterBackend::new(positions.clone(), 0.0);
        let localized_backend = BarycenterBackend::new(positions, 2.0);

        let base = base_backend.compute_gains(&request([0.2, 0.0, 0.0])).gains;
        let localized = localized_backend
            .compute_gains(&request([0.2, 0.0, 0.0]))
            .gains;

        assert!(
            localized[1] > base[1],
            "expected right speaker gain to increase"
        );
        assert!(
            localized[0] < base[0],
            "expected left speaker gain to decrease"
        );
    }
}
