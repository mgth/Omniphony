use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering, fence};

use anyhow::Result;

use super::room_transform::room_scaled_position;
use super::{
    BackendCapabilities, GainModel, GainScratch, HintSlot, NeighbourHint, RenderRequest,
    foreign_scratch,
};
use crate::spatial_vbap::MAX_SPEAKERS;
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
/// it is small enough to leave the first two terms alone: see `RIDGE`.
///
/// With it the objective is strictly convex, so the answer is unique. It does
/// not depend on where a solver starts, and mirrored sources in a mirrored
/// layout get mirrored gains. It is computed exactly rather than iterated
/// towards: see `minimise`.
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
    /// The barycenter model for `speaker_positions`, or an error when there
    /// are more than [`MAX_SPEAKERS`] of them: the limit of the renderer's
    /// gain sets, which this backend no longer adds to (its solver works on a
    /// scratch sized for the layout). Refused here, at configuration time,
    /// where the error reaches the caller (a recompute reports it to Studio).
    pub fn try_new(speaker_positions: Vec<[f32; 3]>, localize: f32) -> Result<Self> {
        if speaker_positions.len() > MAX_SPEAKERS {
            anyhow::bail!(
                "the barycenter backend handles at most {MAX_SPEAKERS} spatialized speakers; \
                 this layout has {}. Pick another backend or spatialize fewer speakers.",
                speaker_positions.len()
            );
        }
        Ok(Self::new(speaker_positions, localize))
    }

    /// The barycenter model for `speaker_positions`.
    ///
    /// # Panics
    ///
    /// With more than [`MAX_SPEAKERS`] positions; [`Self::try_new`] returns an
    /// error instead.
    pub fn new(speaker_positions: Vec<[f32; 3]>, localize: f32) -> Self {
        assert!(
            speaker_positions.len() <= MAX_SPEAKERS,
            "barycenter backend speaker count {} exceeds MAX_SPEAKERS {}",
            speaker_positions.len(),
            MAX_SPEAKERS
        );
        // Start from the identity room so the memo always holds a valid entry.
        let room = RoomParams {
            ratio: [1.0, 1.0, 1.0],
            rear: 1.0,
            lower: 1.0,
            center_blend: 0.0,
        };
        let mut speakers = vec![[0.0f32; 3]; speaker_positions.len()];
        room.transform(&speaker_positions, &mut speakers);
        let room_memo = RoomMemo::new(&room, &speakers);
        Self {
            speaker_positions,
            localize: localize.max(0.0),
            room_memo,
        }
    }

    pub fn speaker_count(&self) -> usize {
        self.speaker_positions.len()
    }

    /// The working memory of one caller: the solver's arrays, one entry per
    /// speaker (see [`Solver`]).
    pub fn new_scratch(&self) -> GainScratch {
        GainScratch::new(Solver::new(self.speaker_positions.len()))
    }

    pub fn compute_gains(&self, req: &RenderRequest, scratch: &mut GainScratch, out: &mut [f32]) {
        self.solve(req, None, scratch, out)
    }

    /// Solve for `req`. With a `neighbour` slot, the solve starts from the
    /// weights the previous cell of a table row left there, and leaves its own
    /// for the next cell. The gains are the same either way; a start next to
    /// the answer only saves pivots.
    fn solve(
        &self,
        req: &RenderRequest,
        mut neighbour: Option<&mut HintSlot>,
        scratch: &mut GainScratch,
        gains: &mut [f32],
    ) {
        let speaker_count = self.speaker_positions.len();
        let Some(solver) = scratch
            .state::<Solver>()
            .filter(|solver| solver.speaker_count() == speaker_count)
        else {
            return foreign_scratch(gains);
        };
        debug_assert_eq!(gains.len(), speaker_count, "one gain per speaker");
        if gains.len() != speaker_count {
            return gains.fill(0.0);
        }

        let target = room_scaled_position(
            req.adm_position.map(|value| value as f32),
            req.room_ratio,
            req.room_ratio_rear,
            req.room_ratio_lower,
            req.room_ratio_center_blend,
        );

        gains.fill(0.0);
        if speaker_count == 0 {
            return;
        }

        // A table build asks for every cell with the same room parameters, and a
        // realtime render for every object: the transformed speakers are shared
        // by all those requests.
        let room_params = RoomParams::of(req);
        self.room_memo
            .get_or_compute(&room_params, &mut solver.room, |speakers| {
                room_params.transform(&self.speaker_positions, speakers)
            });

        // A source on a speaker belongs to that speaker alone.
        for (index, speaker) in solver.room.iter().enumerate() {
            if distance_sq(*speaker, target) <= f32::EPSILON {
                // Nothing was solved here: the next cell starts from scratch.
                if let Some(slot) = neighbour.as_deref_mut() {
                    slot.clear();
                }
                gains[index] = 1.0;
                return;
            }
        }

        solver.problem.load(&solver.room, target, self.localize);
        let started = neighbour
            .as_deref()
            .and_then(|slot| slot.values(speaker_count))
            .is_some_and(|weights| solver.problem.start_from(weights, &mut solver.work.weights));
        if !started {
            solver.problem.cold_start(&mut solver.work.weights);
        }
        minimise(&solver.problem, &mut solver.work);
        for ((gain, kept), &weight) in gains
            .iter_mut()
            .zip(solver.kept.iter_mut())
            .zip(&solver.work.weights)
        {
            *kept = weight.max(0.0) as f32;
            *gain = weight.max(0.0).sqrt() as f32;
        }
        if let Some(slot) = neighbour {
            slot.store(&solver.kept);
        }
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

    fn new_scratch(&self) -> GainScratch {
        BarycenterBackend::new_scratch(self)
    }

    fn compute_gains(&self, req: &RenderRequest, scratch: &mut GainScratch, out: &mut [f32]) {
        BarycenterBackend::compute_gains(self, req, scratch, out)
    }

    fn compute_gains_with_hint(
        &self,
        req: &RenderRequest,
        hint: &mut NeighbourHint,
        scratch: &mut GainScratch,
        out: &mut [f32],
    ) {
        self.solve(req, hint.slot(), scratch, out)
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

    /// What the solver needs from the room parameters alone: the speakers in
    /// room-scaled space, written into `speakers`.
    fn transform(&self, speaker_positions: &[[f32; 3]], speakers: &mut [[f32; 3]]) {
        for (transformed, speaker) in speakers.iter_mut().zip(speaker_positions) {
            *transformed = room_scaled_position(
                *speaker,
                self.ratio,
                self.rear,
                self.lower,
                self.center_blend,
            );
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

/// Single-entry memo of the room-scaled speakers of the latest room parameters.
///
/// `compute_gains` runs concurrently on the workers of a table build and on the
/// render thread, and must neither block nor allocate there. The memo is a
/// sequence lock over plain atomics: a reader copies the entry and keeps the
/// copy only if no writer was active meanwhile, a writer publishes only if it
/// wins the entry outright, and whoever loses computes the geometry in its own
/// scratch and moves on. Nobody waits for anybody.
struct RoomMemo {
    /// Even while the entry is stable, odd while a writer is replacing it.
    sequence: AtomicUsize,
    room: [AtomicU32; RoomParams::WORDS],
    /// One entry per speaker of the layout.
    speakers: Box<[[AtomicU32; 3]]>,
}

impl RoomMemo {
    fn new(room: &RoomParams, speakers: &[[f32; 3]]) -> Self {
        let room = room.to_bits();
        Self {
            sequence: AtomicUsize::new(0),
            room: std::array::from_fn(|word| AtomicU32::new(room[word])),
            speakers: speakers
                .iter()
                .map(|speaker| speaker.map(|coordinate| AtomicU32::new(coordinate.to_bits())))
                .collect(),
        }
    }

    /// Fill `speakers` with the geometry for `room`: the memoised one when the
    /// entry is for `room`, else what `compute` writes there, which then
    /// replaces the entry unless another thread is writing it.
    fn get_or_compute(
        &self,
        room: &RoomParams,
        speakers: &mut [[f32; 3]],
        compute: impl FnOnce(&mut [[f32; 3]]),
    ) {
        let room = room.to_bits();

        let sequence = self.sequence.load(Ordering::Acquire);
        let stable = sequence & 1 == 0;
        if stable
            && self
                .room
                .iter()
                .zip(&room)
                .all(|(held, wanted)| held.load(Ordering::Relaxed) == *wanted)
        {
            for (speaker, held) in speakers.iter_mut().zip(&self.speakers) {
                *speaker = held
                    .each_ref()
                    .map(|coordinate| f32::from_bits(coordinate.load(Ordering::Relaxed)));
            }
            // The copy is consistent only if the sequence has not moved since it
            // was first read. The fence orders this second read after the
            // relaxed loads above; it pairs with the writer's release stores.
            fence(Ordering::Acquire);
            if self.sequence.load(Ordering::Relaxed) == sequence {
                return;
            }
        }

        compute(speakers);
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
            for (held, speaker) in self.speakers.iter().zip(speakers.iter()) {
                for (held, coordinate) in held.iter().zip(speaker) {
                    held.store(coordinate.to_bits(), Ordering::Release);
                }
            }
            self.sequence
                .store(sequence.wrapping_add(2), Ordering::Release);
        }
    }
}

/// What a caller of [`BarycenterBackend`] holds between requests: every array
/// of a solve, one entry per speaker, sized for the layout when the scratch is
/// made so that a solve allocates nothing.
struct Solver {
    /// The speakers in the room of the request.
    room: Vec<[f32; 3]>,
    problem: Problem,
    work: Workspace,
    /// The solution's weights in single precision: what a table build hands
    /// to the next cell.
    kept: Vec<f32>,
}

impl Solver {
    fn new(speaker_count: usize) -> Self {
        Self {
            room: vec![[0.0; 3]; speaker_count],
            problem: Problem::new(speaker_count),
            work: Workspace::new(speaker_count),
            kept: vec![0.0; speaker_count],
        }
    }

    fn speaker_count(&self) -> usize {
        self.room.len()
    }
}

/// One request, as the solver sees it: in double precision.
struct Problem {
    speakers: Vec<[f64; 3]>,
    target: [f64; 3],
    localize: f64,
    speaker_count: usize,
    nearest: usize,
}

impl Problem {
    /// Room for a request on `speaker_count` speakers: [`Self::load`] one.
    fn new(speaker_count: usize) -> Self {
        Self {
            speakers: vec![[0.0; 3]; speaker_count],
            target: [0.0; 3],
            localize: 0.0,
            speaker_count,
            nearest: 0,
        }
    }

    /// Take a request on `speakers`, as many as this problem was made for.
    fn load(&mut self, speakers: &[[f32; 3]], target: [f32; 3], localize: f32) {
        let target = target.map(f64::from);
        self.target = target;
        self.localize = f64::from(localize);
        self.speaker_count = speakers.len().min(self.speakers.len());
        self.nearest = 0;
        let mut nearest_distance_sq = f64::INFINITY;
        for (index, (held, speaker)) in self.speakers.iter_mut().zip(speakers).enumerate() {
            let speaker = speaker.map(f64::from);
            let distance_sq = (speaker[0] - target[0]) * (speaker[0] - target[0])
                + (speaker[1] - target[1]) * (speaker[1] - target[1])
                + (speaker[2] - target[2]) * (speaker[2] - target[2]);
            *held = speaker;
            if distance_sq < nearest_distance_sq {
                nearest_distance_sq = distance_sq;
                self.nearest = index;
            }
        }
    }

    /// Where a solve starts when nothing better is known, written into
    /// `weights`. The minimiser does not depend on it, only the number of
    /// pivots does: with a localisation term the weight ends up on a few
    /// speakers around the source, so start from the nearest one; without, it
    /// spreads, so start from all of them.
    fn cold_start(&self, weights: &mut [f64]) {
        if self.localize > 0.0 {
            weights.fill(0.0);
            weights[self.nearest] = 1.0;
        } else {
            weights.fill(1.0 / self.speaker_count as f64);
        }
    }

    /// A start on the weights of a neighbouring solve, written into `weights`.
    /// False if they carry no weight at all: `weights` is then to be started
    /// some other way.
    fn start_from(&self, neighbour: &[f32], weights: &mut [f64]) -> bool {
        let mut sum = 0.0;
        for (weight, neighbour) in weights.iter_mut().zip(neighbour) {
            *weight = f64::from(neighbour.max(0.0));
            sum += *weight;
        }
        if !(sum > 0.0 && sum.is_finite()) {
            return false;
        }
        for weight in weights.iter_mut() {
            *weight /= sum;
        }
        true
    }

    /// The pivot budget of [`minimise`]: several times what any layout needs,
    /// there to bound the time of a solve whatever the input.
    fn max_pivots(&self) -> usize {
        4 * self.speaker_count + 8
    }
}

/// The arrays [`minimise`] works on, one entry per speaker.
struct Workspace {
    /// The start on the way in, the solution on the way out.
    weights: Vec<f64>,
    candidate: Vec<f64>,
    in_support: Vec<bool>,
    barred: Vec<bool>,
}

impl Workspace {
    fn new(speaker_count: usize) -> Self {
        Self {
            weights: vec![0.0; speaker_count],
            candidate: vec![0.0; speaker_count],
            in_support: vec![false; speaker_count],
            barred: vec![false; speaker_count],
        }
    }
}

/// How a [`minimise`] ended; the solution itself is in the workspace.
struct Outcome {
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
/// `work.weights` holds the start, any feasible set of weights, and is left
/// holding the result: the minimiser on the final support, computed from
/// scratch, so the start does not leak into it.
fn minimise(problem: &Problem, work: &mut Workspace) -> Outcome {
    let speaker_count = problem.speaker_count;
    let Workspace {
        weights,
        candidate,
        in_support,
        barred,
    } = work;
    // One length for the four of them and the problem, so the loops below
    // index them without a bounds check each.
    let (Some(weights), Some(candidate), Some(in_support), Some(barred)) = (
        weights.get_mut(..speaker_count),
        candidate.get_mut(..speaker_count),
        in_support.get_mut(..speaker_count),
        barred.get_mut(..speaker_count),
    ) else {
        return Outcome {
            converged: false,
            pivots: 0,
        };
    };
    for index in 0..speaker_count {
        in_support[index] = weights[index] > 0.0;
    }
    // A speaker that is pushed out by the very pivot that follows its entry
    // would take a weight below rounding: leave it out for good, or the two
    // pivots could repeat forever.
    barred.fill(false);
    let mut entered = usize::MAX;

    let max_pivots = problem.max_pivots();
    for pivot in 0..max_pivots {
        minimiser_on(problem, in_support, candidate);

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
            return Outcome {
                converged: true,
                pivots: pivot + 1,
            };
        }
        in_support[entering] = true;
        entered = entering;
    }

    Outcome {
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
fn minimiser_on(problem: &Problem, in_support: &[bool], candidate: &mut [f64]) {
    let speaker_count = problem.speaker_count;
    // The three arrays at one length: see `minimise`.
    let (Some(speakers), Some(in_support), Some(candidate)) = (
        problem.speakers.get(..speaker_count),
        in_support.get(..speaker_count),
        candidate.get_mut(..speaker_count),
    ) else {
        return;
    };

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

    /// The widest of [`layouts`].
    const WIDE: usize = 64;

    fn layouts() -> Vec<Vec<[f32; 3]>> {
        let home = home_layout();
        let mut rng = Lcg(7);
        let full: Vec<[f32; 3]> = (0..MAX_SPEAKERS)
            .map(|_| [rng.next(), rng.next(), rng.next()])
            .collect();
        // Wider than the renderer's gain sets: the solver has no width of
        // its own.
        let wide: Vec<[f32; 3]> = (0..WIDE)
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
            wide,
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
        let mut problem = Problem::new(speakers.len());
        problem.load(
            &speakers,
            scale(req.adm_position.map(|value| value as f32)),
            localize,
        );
        problem
    }

    /// A solve and what it left in its workspace.
    struct Solution {
        weights: Vec<f64>,
        converged: bool,
        pivots: usize,
    }

    /// [`minimise`] from `start`, on a workspace of its own.
    fn solve_from(problem: &Problem, start: &[f64]) -> Solution {
        let mut work = Workspace::new(problem.speaker_count);
        work.weights.copy_from_slice(start);
        let Outcome { converged, pivots } = minimise(problem, &mut work);
        Solution {
            weights: work.weights,
            converged,
            pivots,
        }
    }

    /// [`minimise`] from the problem's cold start.
    fn solve(problem: &Problem) -> Solution {
        let mut start = vec![0.0; problem.speaker_count];
        problem.cold_start(&mut start);
        solve_from(problem, &start)
    }

    fn distance_sq_to_target(problem: &Problem, index: usize) -> f64 {
        (0..3)
            .map(|axis| (problem.speakers[index][axis] - problem.target[axis]).powi(2))
            .sum()
    }

    /// The objective, straight from its definition.
    fn objective(problem: &Problem, weights: &[f64]) -> f64 {
        let mut error = problem.target.map(|coordinate| -coordinate);
        let mut linear = 0.0;
        let mut ridge = 0.0;
        for (index, (speaker, &weight)) in problem.speakers.iter().zip(weights).enumerate() {
            for (error, coordinate) in error.iter_mut().zip(speaker) {
                *error += coordinate * weight;
            }
            linear += problem.localize * distance_sq_to_target(problem, index) * weight;
            ridge += RIDGE * weight * weight;
        }
        error.iter().map(|e| e * e).sum::<f64>() + linear + ridge
    }

    /// How much the objective could still drop from `weights`, at most: the
    /// gap `∇f(w) · (w − v)` maximised over the simplex, which bounds
    /// `f(w) − min f` for a convex `f`.
    fn optimality_gap(problem: &Problem, weights: &[f64]) -> f64 {
        let mut error = problem.target.map(|coordinate| -coordinate);
        for (speaker, &weight) in problem.speakers.iter().zip(weights) {
            for (error, coordinate) in error.iter_mut().zip(speaker) {
                *error += coordinate * weight;
            }
        }
        let mut mean = 0.0;
        let mut least = f64::INFINITY;
        for (index, (speaker, &weight)) in problem.speakers.iter().zip(weights).enumerate() {
            let gradient = 2.0
                * (speaker[0] * error[0] + speaker[1] * error[1] + speaker[2] * error[2])
                + problem.localize * distance_sq_to_target(problem, index)
                + 2.0 * RIDGE * weight;
            mean += weight * gradient;
            least = least.min(gradient);
        }
        mean - least
    }

    /// The minimiser found the slow way: for every support, solve the
    /// stationarity equations as one dense linear system, and keep the best of
    /// the supports whose solution has no negative weight.
    fn minimise_by_enumeration(problem: &Problem) -> (Vec<f64>, f64) {
        let count = problem.speaker_count;
        let mut best = (vec![0.0; count], f64::INFINITY);
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
            let mut weights = vec![0.0f64; count];
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
                    let solution = solve(&problem);
                    let (expected, expected_value) = minimise_by_enumeration(&problem);
                    assert!(solution.converged);
                    let value = objective(&problem, &solution.weights);
                    assert!(
                        (value - expected_value).abs() <= 1e-8 * (1.0 + expected_value.abs()),
                        "objective {value:e} vs {expected_value:e} at {target:?}"
                    );
                    for (index, (weight, expected)) in
                        solution.weights.iter().zip(&expected).enumerate()
                    {
                        let gain = weight.sqrt();
                        let expected_gain = expected.sqrt();
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
                    let solution = solve(&problem);
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
        assert!(most_pivots <= 2 * WIDE + 8, "{most_pivots} pivots");
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
                    let reference = solve(&problem);

                    let uniform = vec![1.0 / count as f64; count];
                    let mut vertex = vec![0.0; count];
                    vertex[call % count] = 1.0;
                    // A random subset with random weights.
                    let mut scattered = vec![0.0; count];
                    let mut sum = 0.0;
                    for weight in &mut scattered {
                        *weight = f64::from(rng.next().max(0.0));
                        sum += *weight;
                    }
                    if sum == 0.0 {
                        scattered[0] = 1.0;
                        sum = 1.0;
                    }
                    for weight in &mut scattered {
                        *weight /= sum;
                    }

                    for start in [uniform, vertex, scattered] {
                        let solution = solve_from(&problem, &start);
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
                    let gains = backend.gains_at(&request_in(room, target));
                    let mirrored_target = [-target[0], target[1], target[2]];
                    let mirrored = backend.gains_at(&request_in(room, mirrored_target));
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
            let gains = backend.gains_at(&request([0.0, 0.0, 0.0]));
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
    fn a_solve_started_from_its_neighbour_returns_the_same_gains() {
        for positions in [layout_714(), layouts().swap_remove(0)] {
            for localize in [0.0, 0.5] {
                let backend = BarycenterBackend::new(positions.clone(), localize);
                let mut scratch = GainModel::new_scratch(&backend);
                let mut warm = vec![0.0f32; positions.len()];
                let mut hint = NeighbourHint::new();
                // Neighbouring targets, as along a table row, then unrelated ones.
                let row = (0..40).map(|step| [-1.0 + 0.05 * step as f64, 0.3, 0.2]);
                for target in row.chain(targets(3, 3, 3, 40)) {
                    let req = request_in(ROOMS[1], target);
                    hint.begin_cell();
                    GainModel::compute_gains_with_hint(
                        &backend,
                        &req,
                        &mut hint,
                        &mut scratch,
                        &mut warm,
                    );
                    let cold = backend.gains_at(&req);
                    assert!(
                        warm.iter()
                            .zip(cold.iter())
                            .all(|(a, b)| a.to_bits() == b.to_bits()),
                        "{} speakers, localize {localize}, target {target:?}",
                        positions.len()
                    );
                }
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
        let fresh =
            |req: &RenderRequest| BarycenterBackend::new(positions.clone(), localize).gains_at(req);
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
                            let gains = backend.gains_at(&req);
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

        let gains = backend.gains_at(&request([0.2, 0.4, 0.1]));
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
        let gains = backend.gains_at(&request(target));

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

        let gains = backend.gains_at(&request([1.0, 0.0, 0.0]));
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

        let base = base_backend.gains_at(&request([0.2, 0.0, 0.0]));
        let localized = localized_backend.gains_at(&request([0.2, 0.0, 0.0]));

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
