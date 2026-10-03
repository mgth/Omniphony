use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering, fence};

use anyhow::Result;

use super::room_transform::room_scaled_position;
use super::{BackendCapabilities, GainModel, RenderRequest, RenderResponse};
use crate::spatial_vbap::{Gains, MAX_SPEAKERS};
use crate::speaker_layout::SpeakerLayout;
use omniphony_geometry::f32::vec3::{distance_sq, dot, sub};

pub struct BarycenterBackend {
    speaker_positions: Vec<[f32; 3]>,
    /// Localisation bias toward nearer speakers, baked at construction. It only
    /// changes via a topology rebuild, so it is not a per-request input.
    localize: f32,
    /// Solver inputs derived from the room parameters of the latest request.
    room_memo: RoomMemo,
}

const MAX_PROJECTED_GRADIENT_ITERS: usize = 48;
const RESIDUAL_TOLERANCE_SQ: f32 = 1e-8;
const WEIGHT_TOLERANCE: f32 = 1e-6;

// The simplex projection tracks speakers by `u8` index.
const _: () = assert!(MAX_SPEAKERS <= u8::MAX as usize + 1);

impl BarycenterBackend {
    /// The barycenter model for `speaker_positions`, or an error when there
    /// are more than [`MAX_SPEAKERS`] of them. The solver works on fixed
    /// `MAX_SPEAKERS`-wide arrays on purpose (no allocation per request), so a
    /// larger layout is refused here, at configuration time, where the error
    /// reaches the caller (a recompute reports it to Studio), instead of
    /// panicking out of bounds in `compute_gains` on the render thread.
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
        // realtime render for every object: the transformed speakers and the step
        // size are shared by all those requests.
        let room_params = RoomParams::of(req);
        let room = self
            .room_memo
            .get_or_compute(&room_params, speaker_count, || {
                RoomGeometry::new(&self.speaker_positions, &room_params)
            });

        // The localisation term of the gradient depends on the target, not on the
        // weights, so it is the same for every iteration.
        let mut localize_bias = [0.0f32; MAX_SPEAKERS];
        for index in 0..speaker_count {
            let local_distance_sq = distance_sq(room.speakers[index], target);
            if local_distance_sq <= f32::EPSILON {
                gains.set(index, 1.0);
                return RenderResponse { gains };
            }
            localize_bias[index] = self.localize * local_distance_sq;
        }

        let mut weights = [0.0f32; MAX_SPEAKERS];
        let mut trial_weights = [0.0f32; MAX_SPEAKERS];
        // Speaker indices by descending trial weight, carried from one iteration
        // to the next (see `project_onto_simplex`).
        let mut order: [u8; MAX_SPEAKERS] = std::array::from_fn(|index| index as u8);

        let uniform_weight = 1.0 / speaker_count as f32;
        weights[..speaker_count].fill(uniform_weight);

        for _ in 0..MAX_PROJECTED_GRADIENT_ITERS {
            let rendered = weighted_position(&room.speakers, &weights, speaker_count);
            let residual = sub(rendered, target);
            if dot(residual, residual) <= RESIDUAL_TOLERANCE_SQ {
                break;
            }

            for index in 0..speaker_count {
                let gradient = 2.0 * dot(room.speakers[index], residual) + localize_bias[index];
                trial_weights[index] = weights[index] - room.step_size * gradient;
            }

            project_onto_simplex(
                &mut trial_weights[..speaker_count],
                &mut order[..speaker_count],
            );

            // Converged once no weight moves by more than the tolerance. Asking
            // "did any weight move" rather than "what is the largest move" keeps
            // the test off a serial chain of `max` and lets it vectorise.
            let mut moved = false;
            for index in 0..speaker_count {
                moved |= (trial_weights[index] - weights[index]).abs() > WEIGHT_TOLERANCE;
                weights[index] = trial_weights[index];
            }
            if !moved {
                break;
            }
        }

        for index in 0..speaker_count {
            gains.set(index, weights[index].max(0.0).sqrt());
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
/// room-scaled space and the gradient step size they allow.
#[derive(Clone, Copy)]
struct RoomGeometry {
    speakers: [[f32; 3]; MAX_SPEAKERS],
    step_size: f32,
}

impl RoomGeometry {
    fn new(speaker_positions: &[[f32; 3]], room: &RoomParams) -> Self {
        let speaker_count = speaker_positions.len().min(MAX_SPEAKERS);
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
        let step_size = projected_gradient_step_size(&speakers, speaker_count);
        Self {
            speakers,
            step_size,
        }
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
    step_size: AtomicU32,
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
            step_size: AtomicU32::new(geometry.step_size.to_bits()),
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
                step_size: f32::from_bits(self.step_size.load(Ordering::Relaxed)),
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
            self.step_size
                .store(geometry.step_size.to_bits(), Ordering::Release);
            self.sequence
                .store(sequence.wrapping_add(2), Ordering::Release);
        }
        geometry
    }
}

fn weighted_position(
    transformed_speakers: &[[f32; 3]; MAX_SPEAKERS],
    weights: &[f32; MAX_SPEAKERS],
    speaker_count: usize,
) -> [f32; 3] {
    let mut weighted = [0.0f32; 3];
    for index in 0..speaker_count {
        let weight = weights[index];
        weighted[0] += transformed_speakers[index][0] * weight;
        weighted[1] += transformed_speakers[index][1] * weight;
        weighted[2] += transformed_speakers[index][2] * weight;
    }
    weighted
}

fn projected_gradient_step_size(
    transformed_speakers: &[[f32; 3]; MAX_SPEAKERS],
    speaker_count: usize,
) -> f32 {
    let mut gram = [[0.0f32; 3]; 3];
    for speaker in transformed_speakers.iter().take(speaker_count) {
        gram[0][0] += speaker[0] * speaker[0];
        gram[0][1] += speaker[0] * speaker[1];
        gram[0][2] += speaker[0] * speaker[2];
        gram[1][0] += speaker[1] * speaker[0];
        gram[1][1] += speaker[1] * speaker[1];
        gram[1][2] += speaker[1] * speaker[2];
        gram[2][0] += speaker[2] * speaker[0];
        gram[2][1] += speaker[2] * speaker[1];
        gram[2][2] += speaker[2] * speaker[2];
    }

    let spectral_norm_sq = largest_eigenvalue_sym_3x3(gram).max(1e-6);
    1.0 / (2.0 * spectral_norm_sq)
}

fn largest_eigenvalue_sym_3x3(matrix: [[f32; 3]; 3]) -> f32 {
    let mut v = [1.0f32, 1.0, 1.0];
    let mut norm = dot(v, v).sqrt();
    if norm <= 1e-12 {
        return 0.0;
    }
    v[0] /= norm;
    v[1] /= norm;
    v[2] /= norm;

    for _ in 0..8 {
        let next = [
            matrix[0][0] * v[0] + matrix[0][1] * v[1] + matrix[0][2] * v[2],
            matrix[1][0] * v[0] + matrix[1][1] * v[1] + matrix[1][2] * v[2],
            matrix[2][0] * v[0] + matrix[2][1] * v[1] + matrix[2][2] * v[2],
        ];
        norm = dot(next, next).sqrt();
        if norm <= 1e-12 {
            return 0.0;
        }
        v = [next[0] / norm, next[1] / norm, next[2] / norm];
    }

    let mv = [
        matrix[0][0] * v[0] + matrix[0][1] * v[1] + matrix[0][2] * v[2],
        matrix[1][0] * v[0] + matrix[1][1] * v[1] + matrix[1][2] * v[2],
        matrix[2][0] * v[0] + matrix[2][1] * v[1] + matrix[2][2] * v[2],
    ];
    dot(v, mv).max(0.0)
}

/// Euclidean projection of `values` onto the probability simplex.
///
/// `order` is a permutation of the indices of `values` and comes back sorted by
/// descending value. The solver hands the same `order` in again on the next
/// iteration, where the weights have barely moved, so the insertion sort mostly
/// confirms an order that is already right.
fn project_onto_simplex(values: &mut [f32], order: &mut [u8]) {
    debug_assert_eq!(values.len(), order.len());
    debug_assert!(values.len() <= MAX_SPEAKERS);
    if values.is_empty() {
        return;
    }

    let mut keys = [0i32; MAX_SPEAKERS];
    for (key, value) in keys.iter_mut().zip(values.iter()) {
        *key = total_order_key(*value);
    }
    for sorted in 1..order.len() {
        let slot = order[sorted];
        let key = keys[slot as usize];
        let mut hole = sorted;
        while hole > 0 && keys[order[hole - 1] as usize] < key {
            order[hole] = order[hole - 1];
            hole -= 1;
        }
        order[hole] = slot;
    }

    // The threshold is the one of the last sorted value that still exceeds its
    // own running threshold (the first value when none does).
    let mut cumulative = 0.0f32;
    let mut theta = 0.0f32;
    for (index, slot) in order.iter().copied().enumerate() {
        let value = values[slot as usize];
        cumulative += value;
        let running_theta = (cumulative - 1.0) / (index as f32 + 1.0);
        if index == 0 || value > running_theta {
            theta = running_theta;
        }
    }

    for value in values.iter_mut() {
        *value = (*value - theta).max(0.0);
    }
}

/// Integer that orders like `f32::total_cmp`: two values compare the way their
/// keys do, and equal keys are the same bit pattern, so sorting on the key
/// yields the same sequence of values whatever the sort does with ties.
#[inline]
fn total_order_key(value: f32) -> i32 {
    let bits = value.to_bits() as i32;
    bits ^ ((((bits >> 31) as u32) >> 1) as i32)
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

    /// The solver written the plain way: every quantity recomputed where it is
    /// used and a general-purpose sort in the projection. The backend must
    /// return the same gains bit for bit.
    mod reference {
        use super::super::*;

        pub fn compute_gains(
            speaker_positions: &[[f32; 3]],
            localize: f32,
            req: &RenderRequest,
        ) -> Gains {
            let target = room_scaled_position(
                req.adm_position.map(|value| value as f32),
                req.room_ratio,
                req.room_ratio_rear,
                req.room_ratio_lower,
                req.room_ratio_center_blend,
            );

            let mut gains = Gains::zeroed(speaker_positions.len());
            if speaker_positions.is_empty() {
                return gains;
            }

            let speaker_count = speaker_positions.len();
            let mut transformed_speakers = [[0.0f32; 3]; MAX_SPEAKERS];
            for (index, speaker) in speaker_positions.iter().copied().enumerate() {
                transformed_speakers[index] = room_scaled_position(
                    speaker,
                    req.room_ratio,
                    req.room_ratio_rear,
                    req.room_ratio_lower,
                    req.room_ratio_center_blend,
                );
                if distance_sq(target, transformed_speakers[index]) <= f32::EPSILON {
                    gains.set(index, 1.0);
                    return gains;
                }
            }

            let mut weights = [0.0f32; MAX_SPEAKERS];
            let mut trial_weights = [0.0f32; MAX_SPEAKERS];
            let mut sort_buffer = [0.0f32; MAX_SPEAKERS];
            let mut gradient = [0.0f32; MAX_SPEAKERS];

            let uniform_weight = 1.0 / speaker_count as f32;
            weights[..speaker_count].fill(uniform_weight);

            let step_size = projected_gradient_step_size(&transformed_speakers, speaker_count);
            for _ in 0..MAX_PROJECTED_GRADIENT_ITERS {
                let rendered = weighted_position(&transformed_speakers, &weights, speaker_count);
                let residual = sub(rendered, target);
                if dot(residual, residual) <= RESIDUAL_TOLERANCE_SQ {
                    break;
                }

                for index in 0..speaker_count {
                    let local_distance_sq = distance_sq(transformed_speakers[index], target);
                    gradient[index] = 2.0 * dot(transformed_speakers[index], residual)
                        + localize * local_distance_sq;
                    trial_weights[index] = weights[index] - step_size * gradient[index];
                }

                project_onto_simplex(
                    &mut trial_weights[..speaker_count],
                    &mut sort_buffer[..speaker_count],
                );

                let mut max_delta = 0.0f32;
                for index in 0..speaker_count {
                    max_delta = max_delta.max((trial_weights[index] - weights[index]).abs());
                    weights[index] = trial_weights[index];
                }
                if max_delta <= WEIGHT_TOLERANCE {
                    break;
                }
            }

            for index in 0..speaker_count {
                gains.set(index, weights[index].max(0.0).sqrt());
            }
            gains
        }

        pub fn project_onto_simplex(values: &mut [f32], scratch: &mut [f32]) {
            debug_assert_eq!(values.len(), scratch.len());
            if values.is_empty() {
                return;
            }

            scratch.copy_from_slice(values);
            scratch.sort_unstable_by(|a, b| b.total_cmp(a));

            let mut cumulative = 0.0f32;
            let mut rho = 0usize;
            for (index, value) in scratch.iter().copied().enumerate() {
                cumulative += value;
                let theta = (cumulative - 1.0) / (index as f32 + 1.0);
                if value > theta {
                    rho = index;
                }
            }

            let theta = (scratch[..=rho].iter().copied().sum::<f32>() - 1.0) / (rho as f32 + 1.0);
            for value in values.iter_mut() {
                *value = (*value - theta).max(0.0);
            }
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

    fn assert_same_bits(actual: &Gains, expected: &Gains, context: impl Fn() -> String) {
        assert_eq!(actual.len(), expected.len(), "{}", context());
        for (speaker, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
            assert!(
                a.to_bits() == e.to_bits(),
                "speaker {speaker}: {a:e} ({:#010x}) != {e:e} ({:#010x}) — {}",
                a.to_bits(),
                e.to_bits(),
                context()
            );
        }
    }

    #[test]
    fn gains_match_reference_on_a_dense_grid() {
        let positions = layouts().swap_remove(0);
        let localize = 0.5;
        let backend = BarycenterBackend::new(positions.clone(), localize);
        for target in targets(25, 25, 13, 500) {
            let req = request_in(ROOMS[1], target);
            assert_same_bits(
                &backend.compute_gains(&req).gains,
                &reference::compute_gains(&positions, localize, &req),
                || format!("target {target:?}"),
            );
        }
    }

    #[test]
    fn gains_match_reference_when_the_room_changes_between_calls() {
        let targets = targets(9, 9, 7, 200);
        for (layout, positions) in layouts().into_iter().enumerate() {
            for localize in [0.0, 0.5, 2.0] {
                let backend = BarycenterBackend::new(positions.clone(), localize);
                // `stride` 1 changes the room on every call; 3 also leaves it in
                // place for a few calls, so both a fresh and a memoised geometry
                // are exercised for every room.
                for stride in [1, 3] {
                    for (call, target) in targets.iter().enumerate() {
                        let room = (call / stride) % ROOMS.len();
                        let req = request_in(ROOMS[room], *target);
                        assert_same_bits(
                            &backend.compute_gains(&req).gains,
                            &reference::compute_gains(&positions, localize, &req),
                            || {
                                format!(
                                    "layout {layout}, localize {localize}, room {room}, \
                                     target {target:?}"
                                )
                            },
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn gains_match_reference_from_several_threads() {
        let positions = layouts().swap_remove(0);
        let localize = 0.5;
        let backend = BarycenterBackend::new(positions.clone(), localize);
        let targets = targets(9, 9, 7, 200);
        std::thread::scope(|scope| {
            for thread in 0..8usize {
                let (backend, positions, targets) = (&backend, &positions, &targets);
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
                            assert_same_bits(
                                &backend.compute_gains(&req).gains,
                                &reference::compute_gains(positions, localize, &req),
                                || format!("thread {thread}, room {room}, target {target:?}"),
                            );
                        }
                    }
                });
            }
        });
    }

    #[test]
    fn projection_matches_reference() {
        let mut rng = Lcg(3);
        let specials = [0.0, -0.0, 1.0, -1.0, 0.25, 1e-30, -1e-30, 3.5e20, -3.5e20];
        for case in 0..20_000usize {
            let len = 1 + case % MAX_SPEAKERS;
            let scale = [1.0, 0.1, 4.0, 1e-3][(case / MAX_SPEAKERS) % 4];
            let mut values = [0.0f32; MAX_SPEAKERS];
            for value in &mut values[..len] {
                *value = scale * rng.next();
            }
            // Ties, signed zeros and extreme magnitudes among ordinary values.
            if case % 3 == 0 {
                for _ in 0..1 + case % 5 {
                    let slot = ((rng.next() + 1.0) * 0.5 * len as f32) as usize % len;
                    let pick = ((rng.next() + 1.0) * 0.5 * specials.len() as f32) as usize;
                    values[slot] = specials[pick % specials.len()];
                }
            }
            if case % 7 == 0 && len > 1 {
                values[len - 1] = values[0];
            }

            let mut expected = values;
            let mut scratch = [0.0f32; MAX_SPEAKERS];
            reference::project_onto_simplex(&mut expected[..len], &mut scratch[..len]);

            // Whatever order the previous iteration left behind, the result is
            // the same.
            let mut order: [u8; MAX_SPEAKERS] = std::array::from_fn(|index| index as u8);
            order[..len].rotate_left(case % len);
            if case % 2 == 0 {
                order[..len].reverse();
            }
            let mut actual = values;
            project_onto_simplex(&mut actual[..len], &mut order[..len]);

            for index in 0..len {
                assert!(
                    actual[index].to_bits() == expected[index].to_bits(),
                    "case {case}, index {index}: {:e} != {:e} for {:?}",
                    actual[index],
                    expected[index],
                    &values[..len]
                );
            }
            let mut sorted = order;
            sorted[..len].sort_unstable();
            assert!(
                sorted[..len].iter().copied().eq(0..len as u8),
                "case {case}: order is no longer a permutation"
            );
            assert!(
                order[..len].windows(2).all(|pair| values[pair[0] as usize]
                    .total_cmp(&values[pair[1] as usize])
                    .is_ge()),
                "case {case}: order is not descending"
            );
        }
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
