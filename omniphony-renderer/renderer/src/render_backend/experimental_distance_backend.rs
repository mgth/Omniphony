use anyhow::Result;

use super::{BackendCapabilities, GainModel, GainModelKind, RenderRequest, RenderResponse};
use crate::spatial_vbap::Gains;
use crate::speaker_layout::SpeakerLayout;

pub struct ExperimentalDistanceBackend {
    speaker_positions: Vec<[f32; 3]>,
}

#[derive(Clone, Copy)]
struct ExperimentalSpeakerCandidate {
    index: usize,
    transformed_position: [f32; 3],
    distance: f32,
}

impl ExperimentalDistanceBackend {
    pub fn new(speaker_positions: Vec<[f32; 3]>) -> Self {
        Self { speaker_positions }
    }

    pub fn speaker_count(&self) -> usize {
        self.speaker_positions.len()
    }

    pub fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        let target = transform_position(
            req.adm_position.map(|v| v as f32),
            req.room_ratio,
            req.room_ratio_rear,
            req.room_ratio_lower,
            req.room_ratio_center_blend,
        );

        let mut candidates = Vec::with_capacity(self.speaker_positions.len());
        let mut nearest = None::<(usize, f32)>;
        for (index, speaker) in self.speaker_positions.iter().copied().enumerate() {
            let transformed_position = transform_position(
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
            candidates.push(ExperimentalSpeakerCandidate {
                index,
                transformed_position,
                distance,
            });
        }

        let mut gains = Gains::zeroed(self.speaker_positions.len());
        let Some((nearest_index, nearest_distance)) = nearest else {
            return RenderResponse { gains };
        };

        if nearest_distance <= f32::EPSILON {
            gains.set(nearest_index, 1.0);
            return RenderResponse { gains };
        }

        candidates.sort_unstable_by(|a, b| a.distance.total_cmp(&b.distance));
        let active_count = select_experimental_active_count(target, &candidates, req);
        let energy = write_experimental_subset_gains(&mut gains, &candidates[..active_count], req);
        if energy > 1e-12 {
            let norm = energy.sqrt();
            for gain in gains.iter_mut() {
                *gain /= norm;
            }
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

impl GainModel for ExperimentalDistanceBackend {
    fn kind(&self) -> GainModelKind {
        GainModelKind::ExperimentalDistance
    }

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
            supports_distance_diffuse: false,
            supports_heatmap_cartesian: true,
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
fn transform_position(
    position: [f32; 3],
    room_ratio: [f32; 3],
    room_ratio_rear: f32,
    room_ratio_lower: f32,
    room_ratio_center_blend: f32,
) -> [f32; 3] {
    [
        position[0] * room_ratio[0],
        map_depth_with_room_ratios(
            position[1],
            room_ratio[1],
            room_ratio_rear,
            room_ratio_center_blend,
        ),
        if position[2] >= 0.0 {
            position[2] * room_ratio[2]
        } else {
            position[2] * room_ratio_lower
        },
    ]
}

#[inline]
fn euclidean_distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    let dz = a[2] - b[2];
    (dx * dx + dy * dy + dz * dz).sqrt()
}

#[inline]
fn experimental_distance_weight(distance: f32) -> f32 {
    let clamped = distance.max(0.000_001);
    1.0 / (clamped * clamped.sqrt())
}

fn write_experimental_subset_gains(
    gains: &mut Gains,
    candidates: &[ExperimentalSpeakerCandidate],
    req: &RenderRequest,
) -> f32 {
    let mut energy = 0.0f32;
    for candidate in candidates {
        let weight = experimental_distance_weight(
            candidate
                .distance
                .max(req.experimental_distance_distance_floor.max(0.0)),
        );
        gains.set(candidate.index, weight);
        energy += weight * weight;
    }
    energy
}

fn select_experimental_active_count(
    target: [f32; 3],
    candidates: &[ExperimentalSpeakerCandidate],
    req: &RenderRequest,
) -> usize {
    if candidates.is_empty() {
        return 0;
    }

    let min_active = candidates
        .len()
        .min(req.experimental_distance_min_active_speakers.max(1));
    let max_active = candidates
        .len()
        .min(req.experimental_distance_max_active_speakers.max(1));
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
            let threshold = req
                .experimental_distance_position_error_floor
                .max(
                    nearest_distance
                        * req
                            .experimental_distance_position_error_nearest_scale
                            .max(0.0),
                )
                .max(span * req.experimental_distance_position_error_span_scale.max(0.0));
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

#[inline]
fn map_depth_with_room_ratios(
    depth: f32,
    front_ratio: f32,
    rear_ratio: f32,
    center_blend: f32,
) -> f32 {
    let d = depth.clamp(-1.0, 1.0);
    let blend = center_blend.clamp(0.0, 1.0);
    let center_ratio = rear_ratio + (front_ratio - rear_ratio) * blend;
    if d >= 0.0 {
        let t = d;
        let a = center_ratio - front_ratio;
        let b = 2.0 * (front_ratio - center_ratio);
        a * t * t * t + b * t * t + center_ratio * t
    } else {
        let t = -d;
        let a = center_ratio - rear_ratio;
        let b = 2.0 * (rear_ratio - center_ratio);
        -(a * t * t * t + b * t * t + center_ratio * t)
    }
}
