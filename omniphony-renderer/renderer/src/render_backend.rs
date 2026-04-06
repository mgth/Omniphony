use crate::spatial_vbap::{DistanceModel, Gains, VbapPanner, adm_to_spherical};
use crate::speaker_layout::SpeakerLayout;
use anyhow::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GainModelKind {
    Vbap,
    ExperimentalDistance,
}

impl GainModelKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Vbap => "vbap",
            Self::ExperimentalDistance => "experimental_distance",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "vbap" => Some(Self::Vbap),
            "experimental_distance" | "distance" | "distance_based" => {
                Some(Self::ExperimentalDistance)
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderBackendKind {
    Vbap,
    ExperimentalDistance,
}

impl RenderBackendKind {
    pub fn as_str(self) -> &'static str {
        GainModelKind::from(self).as_str()
    }

    pub fn from_str(value: &str) -> Option<Self> {
        GainModelKind::from_str(value).map(Self::from)
    }

    pub fn as_gain_model_kind(self) -> GainModelKind {
        self.into()
    }
}

impl From<GainModelKind> for RenderBackendKind {
    fn from(value: GainModelKind) -> Self {
        match value {
            GainModelKind::Vbap => Self::Vbap,
            GainModelKind::ExperimentalDistance => Self::ExperimentalDistance,
        }
    }
}

impl From<RenderBackendKind> for GainModelKind {
    fn from(value: RenderBackendKind) -> Self {
        match value {
            RenderBackendKind::Vbap => Self::Vbap,
            RenderBackendKind::ExperimentalDistance => Self::ExperimentalDistance,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RenderRequest {
    pub adm_position: [f64; 3],
    pub spread_min: f32,
    pub spread_max: f32,
    pub spread_from_distance: bool,
    pub spread_distance_range: f32,
    pub spread_distance_curve: f32,
    pub room_ratio: [f32; 3],
    pub room_ratio_rear: f32,
    pub room_ratio_lower: f32,
    pub room_ratio_center_blend: f32,
    pub use_distance_diffuse: bool,
    pub distance_diffuse_threshold: f32,
    pub distance_diffuse_curve: f32,
    pub distance_model: DistanceModel,
}

pub struct RenderResponse {
    pub gains: Gains,
}

pub enum RenderBackend {
    Vbap(VbapBackend),
    ExperimentalDistance(ExperimentalDistanceBackend),
}

impl RenderBackend {
    pub fn kind(&self) -> RenderBackendKind {
        match self {
            Self::Vbap(_) => RenderBackendKind::Vbap,
            Self::ExperimentalDistance(_) => RenderBackendKind::ExperimentalDistance,
        }
    }

    pub fn speaker_count(&self) -> usize {
        match self {
            Self::Vbap(backend) => backend.speaker_count(),
            Self::ExperimentalDistance(backend) => backend.speaker_count(),
        }
    }

    pub fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        match self {
            Self::Vbap(backend) => backend.compute_gains(req),
            Self::ExperimentalDistance(backend) => backend.compute_gains(req),
        }
    }

    pub fn effective_mode_name(&self) -> &'static str {
        match self {
            Self::Vbap(backend) => backend.effective_mode_name(),
            Self::ExperimentalDistance(backend) => backend.effective_mode_name(),
        }
    }

    pub fn save_to_file(
        &self,
        path: &std::path::Path,
        speaker_layout: &SpeakerLayout,
    ) -> Result<()> {
        match self {
            Self::Vbap(backend) => backend.save_to_file(path, speaker_layout),
            Self::ExperimentalDistance(backend) => backend.save_to_file(path, speaker_layout),
        }
    }
}

pub struct VbapBackend {
    panner: VbapPanner,
}

pub struct ExperimentalDistanceBackend {
    speaker_positions: Vec<[f32; 3]>,
}

#[derive(Clone, Copy)]
struct ExperimentalSpeakerCandidate {
    index: usize,
    transformed_position: [f32; 3],
    distance: f32,
}

const EXPERIMENTAL_DISTANCE_FLOOR: f32 = 0.05;
const EXPERIMENTAL_MIN_ACTIVE_SPEAKERS: usize = 2;
const EXPERIMENTAL_MAX_ACTIVE_SPEAKERS: usize = 8;
const EXPERIMENTAL_POSITION_ERROR_FLOOR: f32 = 0.08;
const EXPERIMENTAL_POSITION_ERROR_NEAREST_SCALE: f32 = 0.75;
const EXPERIMENTAL_POSITION_ERROR_SPAN_SCALE: f32 = 0.3;

impl VbapBackend {
    pub fn new(panner: VbapPanner) -> Self {
        Self { panner }
    }

    pub fn speaker_count(&self) -> usize {
        self.panner.num_speakers()
    }

    pub fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        let rendering_position = req.adm_position;
        let scaled_x = rendering_position[0] as f32 * req.room_ratio[0];
        let scaled_y = map_depth_with_room_ratios(
            rendering_position[1] as f32,
            req.room_ratio[1],
            req.room_ratio_rear,
            req.room_ratio_center_blend,
        );
        let scaled_z = if rendering_position[2] >= 0.0 {
            rendering_position[2] as f32 * req.room_ratio[2]
        } else {
            rendering_position[2] as f32 * req.room_ratio_lower
        };

        let gains = if self.panner.has_precomputed_effects() {
            self.panner.get_gains_cartesian(
                rendering_position[0] as f32,
                rendering_position[1] as f32,
                rendering_position[2] as f32,
                0.0,
                req.distance_model,
            )
        } else {
            let effective_spread = if req.spread_from_distance {
                let (_, _, dist) = adm_to_spherical(scaled_x, scaled_y, scaled_z);
                let t = (1.0 - dist / req.spread_distance_range)
                    .clamp(0.0, 1.0)
                    .powf(req.spread_distance_curve);
                (req.spread_min + t * (req.spread_max - req.spread_min)).clamp(0.0, 1.0)
            } else {
                req.spread_min.clamp(0.0, 1.0)
            };

            let direct_gains = self.panner.get_gains_cartesian(
                scaled_x,
                scaled_y,
                scaled_z,
                effective_spread,
                req.distance_model,
            );

            if req.use_distance_diffuse {
                let [rx, ry, rz] = rendering_position;
                let adm_dist = ((rx * rx + ry * ry + rz * rz) as f32).sqrt();
                let t = (adm_dist / req.distance_diffuse_threshold.max(1e-6))
                    .min(1.0)
                    .powf(req.distance_diffuse_curve);
                let alpha = 0.5 + 0.5 * t;
                let w_direct = alpha.sqrt();
                let w_mirror = (1.0 - alpha).sqrt();
                let mirror_gains = self.panner.get_gains_cartesian(
                    -scaled_x,
                    -scaled_y,
                    scaled_z,
                    effective_spread,
                    req.distance_model,
                );

                let n = direct_gains.len();
                let mut blended = Gains::zeroed(n);
                let mut energy_direct = 0.0f32;
                let mut energy_blended = 0.0f32;
                for i in 0..n {
                    let g = w_direct * direct_gains[i] + w_mirror * mirror_gains[i];
                    blended.set(i, g);
                    energy_direct += direct_gains[i] * direct_gains[i];
                    energy_blended += g * g;
                }

                if energy_blended > 1e-12 {
                    let scale = (energy_direct / energy_blended).sqrt();
                    for g in blended.iter_mut() {
                        *g *= scale;
                    }
                }

                blended
            } else {
                direct_gains
            }
        };

        RenderResponse { gains }
    }

    pub fn effective_mode_name(&self) -> &'static str {
        match self.panner.table_mode() {
            crate::spatial_vbap::VbapTableMode::Polar => "polar",
            crate::spatial_vbap::VbapTableMode::Cartesian { .. } => "cartesian",
        }
    }

    pub fn save_to_file(
        &self,
        path: &std::path::Path,
        speaker_layout: &SpeakerLayout,
    ) -> Result<()> {
        self.panner
            .save_to_file(path, speaker_layout)
            .map_err(|e| anyhow::anyhow!("Failed to save VBAP table: {}", e))
    }
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
        let active_count = select_experimental_active_count(target, &candidates);
        let energy = write_experimental_subset_gains(&mut gains, &candidates[..active_count]);
        if energy > 1e-12 {
            let norm = energy.sqrt();
            for gain in gains.iter_mut() {
                *gain /= norm;
            }
        }

        RenderResponse { gains }
    }

    pub fn effective_mode_name(&self) -> &'static str {
        "distance"
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
    let clamped = distance.max(EXPERIMENTAL_DISTANCE_FLOOR);
    1.0 / (clamped * clamped.sqrt())
}

fn write_experimental_subset_gains(
    gains: &mut Gains,
    candidates: &[ExperimentalSpeakerCandidate],
) -> f32 {
    let mut energy = 0.0f32;
    for candidate in candidates {
        let weight = experimental_distance_weight(candidate.distance);
        gains.set(candidate.index, weight);
        energy += weight * weight;
    }
    energy
}

fn select_experimental_active_count(
    target: [f32; 3],
    candidates: &[ExperimentalSpeakerCandidate],
) -> usize {
    if candidates.is_empty() {
        return 0;
    }

    let min_active = candidates
        .len()
        .min(EXPERIMENTAL_MIN_ACTIVE_SPEAKERS.max(1));
    let max_active = candidates
        .len()
        .min(EXPERIMENTAL_MAX_ACTIVE_SPEAKERS.max(1));
    let nearest_distance = candidates[0].distance;
    let mut best_count = 1usize;
    let mut best_error = f32::MAX;

    // Stop once the energy-weighted barycenter lands close enough to the target.
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
            let threshold = EXPERIMENTAL_POSITION_ERROR_FLOOR
                .max(nearest_distance * EXPERIMENTAL_POSITION_ERROR_NEAREST_SCALE)
                .max(span * EXPERIMENTAL_POSITION_ERROR_SPAN_SCALE);
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
        let weight = experimental_distance_weight(candidate.distance);
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
