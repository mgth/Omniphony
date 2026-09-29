use anyhow::Result;

use super::room_transform::room_scaled_position;
use super::{BackendCapabilities, GainModel, RenderRequest, RenderResponse};
use crate::spatial_vbap::{Gains, MAX_SPEAKERS};
use crate::speaker_layout::SpeakerLayout;
use omniphony_geometry::f32::vec3::distance as euclidean_distance;

pub struct ExperimentalDistanceBackend {
    speaker_positions: Vec<[f32; 3]>,
    /// Tuning params, baked at construction. They only change via a topology
    /// rebuild, so they are not per-request inputs.
    params: crate::live_params::ExperimentalDistanceLiveParams,
}

#[derive(Clone, Copy)]
struct ExperimentalSpeakerCandidate {
    index: usize,
    transformed_position: [f32; 3],
    distance: f32,
}

impl ExperimentalSpeakerCandidate {
    const EMPTY: Self = Self {
        index: 0,
        transformed_position: [0.0; 3],
        distance: 0.0,
    };
}

impl ExperimentalDistanceBackend {
    pub fn new(
        speaker_positions: Vec<[f32; 3]>,
        params: crate::live_params::ExperimentalDistanceLiveParams,
    ) -> Self {
        Self {
            speaker_positions,
            params,
        }
    }

    pub fn speaker_count(&self) -> usize {
        self.speaker_positions.len()
    }

    pub fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        let target = room_scaled_position(
            req.adm_position.map(|v| v as f32),
            req.room_ratio,
            req.room_ratio_rear,
            req.room_ratio_lower,
            req.room_ratio_center_blend,
        );

        // Fixed-size scratch on the stack: this runs per object per frame
        // when the backend is evaluated directly, so no heap allocation. The
        // layout never exceeds `MAX_SPEAKERS` (the `Gains` capacity).
        debug_assert!(
            self.speaker_positions.len() <= MAX_SPEAKERS,
            "experimental distance backend speaker count {} exceeds MAX_SPEAKERS {}",
            self.speaker_positions.len(),
            MAX_SPEAKERS
        );
        let mut candidate_buf = [ExperimentalSpeakerCandidate::EMPTY; MAX_SPEAKERS];
        let mut candidate_count = 0usize;
        let mut nearest = None::<(usize, f32)>;
        for (index, speaker) in self.speaker_positions.iter().copied().enumerate() {
            let transformed_position = room_scaled_position(
                speaker,
                req.room_ratio,
                req.room_ratio_rear,
                req.room_ratio_lower,
                req.room_ratio_center_blend,
            );
            let distance = euclidean_distance(target, transformed_position);
            match nearest {
                Some((_, best_distance)) if distance >= best_distance => {}
                _ => nearest = Some((index, distance)),
            }
            candidate_buf[candidate_count] = ExperimentalSpeakerCandidate {
                index,
                transformed_position,
                distance,
            };
            candidate_count += 1;
        }
        let candidates = &mut candidate_buf[..candidate_count];

        let mut gains = Gains::zeroed(self.speaker_positions.len());
        let Some((nearest_index, nearest_distance)) = nearest else {
            return RenderResponse { gains };
        };

        if nearest_distance <= f32::EPSILON {
            gains.set(nearest_index, 1.0);
            return RenderResponse { gains };
        }

        candidates.sort_unstable_by(|a, b| a.distance.total_cmp(&b.distance));
        let active_count = select_experimental_active_count(target, &candidates, &self.params);
        let energy =
            write_experimental_subset_gains(&mut gains, &candidates[..active_count], &self.params);
        gains.normalize_to_unit_energy(energy);

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

impl GainModel for ExperimentalDistanceBackend {
    fn backend_id(&self) -> &'static str {
        "experimental_distance"
    }

    fn backend_label(&self) -> &'static str {
        "Distance"
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
        ExperimentalDistanceBackend::speaker_count(self)
    }

    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        ExperimentalDistanceBackend::compute_gains(self, req)
    }

    fn save_to_file(&self, path: &std::path::Path, speaker_layout: &SpeakerLayout) -> Result<()> {
        ExperimentalDistanceBackend::save_to_file(self, path, speaker_layout)
    }
}

#[inline]
fn experimental_distance_weight(distance: f32) -> f32 {
    let clamped = distance.max(0.000_001);
    1.0 / (clamped * clamped.sqrt())
}

fn write_experimental_subset_gains(
    gains: &mut Gains,
    candidates: &[ExperimentalSpeakerCandidate],
    params: &crate::live_params::ExperimentalDistanceLiveParams,
) -> f32 {
    let mut energy = 0.0f32;
    for candidate in candidates {
        let weight =
            experimental_distance_weight(candidate.distance.max(params.distance_floor.max(0.0)));
        gains.set(candidate.index, weight);
        energy += weight * weight;
    }
    energy
}

fn select_experimental_active_count(
    target: [f32; 3],
    candidates: &[ExperimentalSpeakerCandidate],
    params: &crate::live_params::ExperimentalDistanceLiveParams,
) -> usize {
    if candidates.is_empty() {
        return 0;
    }

    let min_active = candidates.len().min(params.min_active_speakers.max(1));
    let max_active = candidates.len().min(params.max_active_speakers.max(1));
    let nearest_distance = candidates[0].distance;
    let mut best_count = 1usize;
    let mut best_error = f32::MAX;

    for count in 1..=max_active {
        let subset = &candidates[..count];
        let reconstructed = reconstruct_experimental_position(subset);
        let error = euclidean_distance(target, reconstructed);
        if error < best_error {
            best_error = error;
            best_count = count;
        }

        if count >= min_active {
            let span = candidate_subset_span(subset);
            let threshold = params
                .position_error_floor
                .max(nearest_distance * params.position_error_nearest_scale.max(0.0))
                .max(span * params.position_error_span_scale.max(0.0));
            if error <= threshold {
                return count;
            }
        }
    }

    best_count.max(min_active.min(max_active))
}

fn reconstruct_experimental_position(candidates: &[ExperimentalSpeakerCandidate]) -> [f32; 3] {
    let mut weighted = [0.0f32; 3];
    let mut energy = 0.0f32;
    for candidate in candidates {
        let weight = experimental_distance_weight(candidate.distance.max(0.000_001));
        let contribution = weight * weight;
        weighted[0] += candidate.transformed_position[0] * contribution;
        weighted[1] += candidate.transformed_position[1] * contribution;
        weighted[2] += candidate.transformed_position[2] * contribution;
        energy += contribution;
    }

    if energy <= 1e-12 {
        return candidates[0].transformed_position;
    }

    [
        weighted[0] / energy,
        weighted[1] / energy,
        weighted[2] / energy,
    ]
}

fn candidate_subset_span(candidates: &[ExperimentalSpeakerCandidate]) -> f32 {
    let mut span = 0.0f32;
    for i in 0..candidates.len() {
        for j in (i + 1)..candidates.len() {
            span = span.max(euclidean_distance(
                candidates[i].transformed_position,
                candidates[j].transformed_position,
            ));
        }
    }
    span
}
