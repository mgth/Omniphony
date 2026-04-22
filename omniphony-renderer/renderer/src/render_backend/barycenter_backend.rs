use anyhow::Result;

use super::{BackendCapabilities, GainModel, GainModelKind, RenderRequest, RenderResponse};
use crate::spatial_vbap::{Gains, MAX_SPEAKERS};
use crate::speaker_layout::SpeakerLayout;

pub struct BarycenterBackend {
    speaker_positions: Vec<[f32; 3]>,
}

const MAX_PROJECTED_GRADIENT_ITERS: usize = 48;
const RESIDUAL_TOLERANCE_SQ: f32 = 1e-8;
const WEIGHT_TOLERANCE: f32 = 1e-6;

impl BarycenterBackend {
    pub fn new(speaker_positions: Vec<[f32; 3]>) -> Self {
        Self { speaker_positions }
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

        let target = transform_position(
            req.adm_position.map(|value| value as f32),
            req.room_ratio,
            req.room_ratio_rear,
            req.room_ratio_lower,
            req.room_ratio_center_blend,
        );

        let mut gains = Gains::zeroed(self.speaker_positions.len());
        if self.speaker_positions.is_empty() {
            return RenderResponse { gains };
        }

        let speaker_count = self.speaker_positions.len();
        let mut transformed_speakers = [[0.0f32; 3]; MAX_SPEAKERS];
        for (index, speaker) in self.speaker_positions.iter().copied().enumerate() {
            transformed_speakers[index] = transform_position(
                speaker,
                req.room_ratio,
                req.room_ratio_rear,
                req.room_ratio_lower,
                req.room_ratio_center_blend,
            );
            if euclidean_distance_sq(target, transformed_speakers[index]) <= f32::EPSILON {
                gains.set(index, 1.0);
                return RenderResponse { gains };
            }
        }

        let mut weights = [0.0f32; MAX_SPEAKERS];
        let mut trial_weights = [0.0f32; MAX_SPEAKERS];
        let mut sort_buffer = [0.0f32; MAX_SPEAKERS];
        let mut gradient = [0.0f32; MAX_SPEAKERS];

        let uniform_weight = 1.0 / speaker_count as f32;
        weights[..speaker_count].fill(uniform_weight);

        let step_size = projected_gradient_step_size(&transformed_speakers, speaker_count);
        let localize = req.barycenter_localize.max(0.0);
        for _ in 0..MAX_PROJECTED_GRADIENT_ITERS {
            let rendered = weighted_position(&transformed_speakers, &weights, speaker_count);
            let residual = subtract(rendered, target);
            if dot(residual, residual) <= RESIDUAL_TOLERANCE_SQ {
                break;
            }

            for index in 0..speaker_count {
                let local_distance_sq = euclidean_distance_sq(transformed_speakers[index], target);
                gradient[index] =
                    2.0 * dot(transformed_speakers[index], residual) + localize * local_distance_sq;
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
    fn kind(&self) -> GainModelKind {
        GainModelKind::Barycenter
    }

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
            supports_distance_diffuse: false,
            supports_heatmap_cartesian: true,
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

#[inline]
fn euclidean_distance_sq(a: [f32; 3], b: [f32; 3]) -> f32 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    let dz = a[2] - b[2];
    dx * dx + dy * dy + dz * dz
}

#[inline]
fn subtract(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
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

fn project_onto_simplex(values: &mut [f32], scratch: &mut [f32]) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn request(position: [f64; 3]) -> RenderRequest {
        RenderRequest {
            adm_position: position,
            spread_min: 0.0,
            spread_max: 0.0,
            spread_from_distance: false,
            spread_distance_range: 1.0,
            spread_distance_curve: 1.0,
            room_ratio: [1.0, 1.0, 1.0],
            room_ratio_rear: 1.0,
            room_ratio_lower: 1.0,
            room_ratio_center_blend: 0.0,
            use_distance_diffuse: false,
            distance_diffuse_threshold: 1.0,
            distance_diffuse_curve: 1.0,
            distance_model: crate::spatial_vbap::DistanceModel::None,
            barycenter_localize: 0.0,
            experimental_distance_distance_floor: 0.0,
            experimental_distance_min_active_speakers: 1,
            experimental_distance_max_active_speakers: 1,
            experimental_distance_position_error_floor: 0.0,
            experimental_distance_position_error_nearest_scale: 0.0,
            experimental_distance_position_error_span_scale: 0.0,
        }
    }

    #[test]
    fn barycenter_backend_normalizes_energy() {
        let backend = BarycenterBackend::new(vec![
            [-1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ]);

        let gains = backend.compute_gains(&request([0.2, 0.4, 0.1])).gains;
        let energy: f32 = gains.iter().map(|gain| gain * gain).sum();
        assert!((energy - 1.0).abs() < 1e-4, "energy={energy}");
    }

    #[test]
    fn barycenter_backend_reconstructs_interior_target() {
        let backend = BarycenterBackend::new(vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ]);

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
        let backend =
            BarycenterBackend::new(vec![[-1.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]);

        let gains = backend.compute_gains(&request([1.0, 0.0, 0.0])).gains;
        assert!(gains[1] > 0.999);
        assert!(gains[0] < 1e-6);
        assert!(gains[2] < 1e-6);
    }

    #[test]
    fn barycenter_backend_localize_biases_toward_near_speakers() {
        let backend = BarycenterBackend::new(vec![
            [-1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0],
        ]);

        let base = backend.compute_gains(&request([0.2, 0.0, 0.0])).gains;
        let mut localized_req = request([0.2, 0.0, 0.0]);
        localized_req.barycenter_localize = 2.0;
        let localized = backend.compute_gains(&localized_req).gains;

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
