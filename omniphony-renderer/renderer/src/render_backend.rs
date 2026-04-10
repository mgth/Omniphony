mod evaluation_artifact;
mod experimental_distance_backend;
mod file_loaded_evaluator;
mod vbap_backend;

use crate::spatial_vbap::{DistanceModel, Gains, adm_to_spherical, spherical_to_adm};
use crate::speaker_layout::SpeakerLayout;
use anyhow::Result;
use serde::Serialize;

pub use evaluation_artifact::{
    BackendRestoreSnapshot, LoadedEvaluationArtifact, SerializedEvaluationMode,
    build_backend_restore_snapshot, build_from_artifact_render_engine,
};
pub use experimental_distance_backend::ExperimentalDistanceBackend;
pub use file_loaded_evaluator::{LoadedVbapFile, build_from_file_render_engine};
pub use vbap_backend::VbapBackend;

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct BackendCapabilities {
    pub supports_realtime: bool,
    pub supports_precomputed_polar: bool,
    pub supports_precomputed_cartesian: bool,
    pub supports_position_interpolation: bool,
    pub supports_distance_model: bool,
    pub supports_spread: bool,
    pub supports_spread_from_distance: bool,
    pub supports_distance_diffuse: bool,
    pub supports_heatmap_cartesian: bool,
    pub supports_table_export: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct BackendDescriptor {
    pub kind: RenderBackendKind,
    pub gain_model_kind: GainModelKind,
    pub id: &'static str,
    pub label: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GainModelKind {
    Vbap,
    ExperimentalDistance,
    FromFile,
}

impl GainModelKind {
    pub fn as_str(self) -> &'static str {
        backend_descriptor_by_gain_model_kind(self).id
    }

    pub fn from_str(value: &str) -> Option<Self> {
        let normalized = value.trim().to_ascii_lowercase();
        if let Some(descriptor) = backend_descriptor_by_id(&normalized) {
            return Some(descriptor.gain_model_kind);
        }
        match normalized.as_str() {
            "from_file" => Some(Self::FromFile),
            "distance" | "distance_based" => Some(Self::ExperimentalDistance),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderBackendKind {
    Vbap,
    ExperimentalDistance,
    FromFile,
}

impl RenderBackendKind {
    pub fn as_str(self) -> &'static str {
        backend_descriptor(self).id
    }

    pub fn from_str(value: &str) -> Option<Self> {
        let normalized = value.trim().to_ascii_lowercase();
        if let Some(descriptor) = backend_descriptor_by_id(&normalized) {
            return Some(descriptor.kind);
        }
        match normalized.as_str() {
            "from_file" => Some(Self::FromFile),
            "distance" | "distance_based" => Some(Self::ExperimentalDistance),
            _ => None,
        }
    }

    pub fn as_gain_model_kind(self) -> GainModelKind {
        backend_descriptor(self).gain_model_kind
    }

    pub fn label(self) -> &'static str {
        backend_descriptor(self).label
    }
}

impl From<GainModelKind> for RenderBackendKind {
    fn from(value: GainModelKind) -> Self {
        match value {
            GainModelKind::Vbap => Self::Vbap,
            GainModelKind::ExperimentalDistance => Self::ExperimentalDistance,
            GainModelKind::FromFile => Self::FromFile,
        }
    }
}

impl From<RenderBackendKind> for GainModelKind {
    fn from(value: RenderBackendKind) -> Self {
        value.as_gain_model_kind()
    }
}

const BACKEND_DESCRIPTORS: [BackendDescriptor; 3] = [
    BackendDescriptor {
        kind: RenderBackendKind::Vbap,
        gain_model_kind: GainModelKind::Vbap,
        id: "vbap",
        label: "VBAP",
    },
    BackendDescriptor {
        kind: RenderBackendKind::ExperimentalDistance,
        gain_model_kind: GainModelKind::ExperimentalDistance,
        id: "experimental_distance",
        label: "Distance",
    },
    BackendDescriptor {
        kind: RenderBackendKind::FromFile,
        gain_model_kind: GainModelKind::FromFile,
        id: "from_file",
        label: "From File",
    },
];

pub fn backend_descriptors() -> &'static [BackendDescriptor] {
    &BACKEND_DESCRIPTORS
}

pub fn backend_descriptor(kind: RenderBackendKind) -> &'static BackendDescriptor {
    backend_descriptors()
        .iter()
        .find(|descriptor| descriptor.kind == kind)
        .expect("missing backend descriptor")
}

pub fn backend_descriptor_by_gain_model_kind(kind: GainModelKind) -> &'static BackendDescriptor {
    backend_descriptors()
        .iter()
        .find(|descriptor| descriptor.gain_model_kind == kind)
        .expect("missing backend descriptor")
}

pub fn backend_descriptor_by_id(id: &str) -> Option<&'static BackendDescriptor> {
    backend_descriptors()
        .iter()
        .find(|descriptor| descriptor.id == id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveEvaluationMode {
    Realtime,
    PrecomputedPolar,
    PrecomputedCartesian,
    FromFile,
}

impl EffectiveEvaluationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Realtime => "realtime",
            Self::PrecomputedPolar => "precomputed_polar",
            Self::PrecomputedCartesian => "precomputed_cartesian",
            Self::FromFile => "from_file",
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

#[derive(Clone, Copy)]
pub struct CartesianEvaluationConfig {
    pub x_size: usize,
    pub y_size: usize,
    pub z_size: usize,
    pub z_neg_size: usize,
}

#[derive(Clone, Copy)]
pub struct PolarEvaluationConfig {
    pub azimuth_values: usize,
    pub elevation_values: usize,
    pub distance_values: usize,
    pub distance_max: f32,
    pub allow_negative_z: bool,
}

#[derive(Clone, Copy)]
pub struct EvaluationBuildConfig {
    pub request_template: RenderRequest,
    pub position_interpolation: bool,
    pub cartesian: CartesianEvaluationConfig,
    pub polar: PolarEvaluationConfig,
}

pub trait GainModel: Send + Sync + 'static {
    fn kind(&self) -> GainModelKind;
    fn backend_id(&self) -> &'static str;
    fn backend_label(&self) -> &'static str;
    fn capabilities(&self) -> BackendCapabilities;
    fn speaker_count(&self) -> usize;
    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse;
    fn save_to_file(&self, path: &std::path::Path, speaker_layout: &SpeakerLayout) -> Result<()>;
}

pub trait EvaluationStrategy {
    fn effective_mode(&self) -> EffectiveEvaluationMode;
    fn prepare(
        self,
        model: Box<dyn GainModel>,
        config: &EvaluationBuildConfig,
    ) -> Result<Box<dyn PreparedEvaluator>>;
}

pub trait PreparedEvaluator: Send + Sync {
    fn speaker_count(&self) -> usize;
    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse;
    fn save_to_file(&self, path: &std::path::Path, speaker_layout: &SpeakerLayout) -> Result<()>;
    fn cartesian_slices_for_speaker(
        &self,
        speaker_index: usize,
        speaker_position: [f32; 3],
    ) -> Option<CartesianSpeakerHeatmapSlices> {
        let _ = (speaker_index, speaker_position);
        None
    }
    fn cartesian_volume_for_speaker(
        &self,
        speaker_index: usize,
        threshold: f32,
        max_samples: usize,
    ) -> Option<CartesianSpeakerHeatmapVolume> {
        let _ = (speaker_index, threshold, max_samples);
        None
    }
}

pub struct RealtimeEvaluator {
    model: Box<dyn GainModel>,
}

impl RealtimeEvaluator {
    pub fn new(model: Box<dyn GainModel>) -> Self {
        Self { model }
    }
}

impl PreparedEvaluator for RealtimeEvaluator {
    fn speaker_count(&self) -> usize {
        self.model.speaker_count()
    }

    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        self.model.compute_gains(req)
    }

    fn save_to_file(&self, path: &std::path::Path, speaker_layout: &SpeakerLayout) -> Result<()> {
        let _ = (path, speaker_layout);
        anyhow::bail!("only precomputed evaluators can be exported to a from-file artifact")
    }
}

pub struct SampledCartesianEvaluator {
    model: Box<dyn GainModel>,
    x_positions: Vec<f32>,
    y_positions: Vec<f32>,
    z_positions: Vec<f32>,
    gains: Vec<f32>,
    speaker_count: usize,
    position_interpolation: bool,
    frozen_request: RenderRequest,
    backend_restore_snapshot: Option<BackendRestoreSnapshot>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CartesianSpeakerHeatmapSlices {
    pub speaker_index: usize,
    pub speaker_position: [f32; 3],
    pub x_positions: Vec<f32>,
    pub y_positions: Vec<f32>,
    pub z_positions: Vec<f32>,
    pub xy_values: Vec<f32>,
    pub xz_values: Vec<f32>,
    pub yz_values: Vec<f32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CartesianSpeakerHeatmapVolume {
    pub speaker_index: usize,
    pub samples: Vec<f32>,
}

impl SampledCartesianEvaluator {
    pub fn new(model: Box<dyn GainModel>, config: &EvaluationBuildConfig) -> Self {
        // Intentionally sample and query the precomputed cartesian evaluator in native
        // ADM coordinates. The backend remains responsible for any room/depth transforms,
        // so the runtime can read gains directly from object positions without converting
        // into a backend-specific "effect space" first.
        let x_positions = evenly_spaced_axis(config.cartesian.x_size.max(2), -1.0, 1.0);
        let y_positions = evenly_spaced_axis(config.cartesian.y_size.max(2), -1.0, 1.0);
        let z_positions =
            cartesian_z_axis(config.cartesian.z_size.max(2), config.cartesian.z_neg_size);
        let speaker_count = model.speaker_count();
        let mut gains = Vec::with_capacity(
            x_positions.len() * y_positions.len() * z_positions.len() * speaker_count,
        );
        let mut request = config.request_template;
        for &z in &z_positions {
            for &y in &y_positions {
                for &x in &x_positions {
                    request.adm_position = [x as f64, y as f64, z as f64];
                    gains.extend_from_slice(&model.compute_gains(&request).gains);
                }
            }
        }
        let backend_restore_snapshot = build_backend_restore_snapshot(
            model.backend_id(),
            model.backend_label(),
            SerializedEvaluationMode::PrecomputedCartesian,
            config,
        );
        Self {
            model,
            x_positions,
            y_positions,
            z_positions,
            gains,
            speaker_count,
            position_interpolation: config.position_interpolation,
            frozen_request: config.request_template,
            backend_restore_snapshot,
        }
    }
}

impl PreparedEvaluator for SampledCartesianEvaluator {
    fn speaker_count(&self) -> usize {
        self.speaker_count
    }

    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        // Read the table directly from native ADM coordinates. This avoids a render-time
        // round-trip through spherical/effect-space conversions for the cartesian path.
        let gains = sample_cartesian_table(
            &self.gains,
            self.speaker_count,
            &self.x_positions,
            &self.y_positions,
            &self.z_positions,
            req.adm_position.map(|value| value as f32),
            self.position_interpolation,
        );
        RenderResponse { gains }
    }

    fn save_to_file(&self, path: &std::path::Path, speaker_layout: &SpeakerLayout) -> Result<()> {
        evaluation_artifact::LoadedEvaluationArtifact::from_sampled_cartesian(
            self.model.backend_id(),
            self.model.backend_label(),
            speaker_layout,
            self.frozen_request,
            self.position_interpolation,
            self.backend_restore_snapshot.as_ref(),
            &self.x_positions,
            &self.y_positions,
            &self.z_positions,
            &self.gains,
            self.speaker_count,
        )?
        .save_to_file(path)
    }

    fn cartesian_slices_for_speaker(
        &self,
        speaker_index: usize,
        speaker_position: [f32; 3],
    ) -> Option<CartesianSpeakerHeatmapSlices> {
        if speaker_index >= self.speaker_count {
            return None;
        }

        let mut xy_values = Vec::with_capacity(self.x_positions.len() * self.y_positions.len());
        for &y in &self.y_positions {
            for &x in &self.x_positions {
                xy_values.push(sample_cartesian_table_speaker_value(
                    &self.gains,
                    self.speaker_count,
                    &self.x_positions,
                    &self.y_positions,
                    &self.z_positions,
                    [x, y, speaker_position[2]],
                    self.position_interpolation,
                    speaker_index,
                ));
            }
        }

        let mut xz_values = Vec::with_capacity(self.x_positions.len() * self.z_positions.len());
        for &z in &self.z_positions {
            for &x in &self.x_positions {
                xz_values.push(sample_cartesian_table_speaker_value(
                    &self.gains,
                    self.speaker_count,
                    &self.x_positions,
                    &self.y_positions,
                    &self.z_positions,
                    [x, speaker_position[1], z],
                    self.position_interpolation,
                    speaker_index,
                ));
            }
        }

        let mut yz_values = Vec::with_capacity(self.y_positions.len() * self.z_positions.len());
        for &z in &self.z_positions {
            for &y in &self.y_positions {
                yz_values.push(sample_cartesian_table_speaker_value(
                    &self.gains,
                    self.speaker_count,
                    &self.x_positions,
                    &self.y_positions,
                    &self.z_positions,
                    [speaker_position[0], y, z],
                    self.position_interpolation,
                    speaker_index,
                ));
            }
        }

        Some(CartesianSpeakerHeatmapSlices {
            speaker_index,
            speaker_position,
            x_positions: self.x_positions.clone(),
            y_positions: self.y_positions.clone(),
            z_positions: self.z_positions.clone(),
            xy_values,
            xz_values,
            yz_values,
        })
    }

    fn cartesian_volume_for_speaker(
        &self,
        speaker_index: usize,
        threshold: f32,
        max_samples: usize,
    ) -> Option<CartesianSpeakerHeatmapVolume> {
        if speaker_index >= self.speaker_count {
            return None;
        }
        let mut weighted_samples = Vec::new();
        let threshold = threshold.max(0.0);
        for (z_index, &z) in self.z_positions.iter().enumerate() {
            for (y_index, &y) in self.y_positions.iter().enumerate() {
                for (x_index, &x) in self.x_positions.iter().enumerate() {
                    let value = read_flat_sample_value(
                        &self.gains,
                        self.speaker_count,
                        self.x_positions.len(),
                        self.y_positions.len(),
                        x_index,
                        y_index,
                        z_index,
                        speaker_index,
                    );
                    if value >= threshold {
                        weighted_samples.push((value, [x, y, z], x_index, y_index, z_index));
                    }
                }
            }
        }
        if max_samples > 0 && weighted_samples.len() > max_samples {
            let max_value = weighted_samples
                .iter()
                .map(|(value, _, _, _, _)| *value)
                .fold(0.0_f32, f32::max)
                .max(f32::EPSILON);
            let mut scored = Vec::with_capacity(weighted_samples.len());
            for sample @ (value, _position, x_index, y_index, z_index) in
                weighted_samples.into_iter()
            {
                // Weighted deterministic reservoir sampling.
                // We soften the distribution with sqrt so medium gains remain visible.
                let weight = (value / max_value).clamp(0.0, 1.0).sqrt().max(1e-6);
                let unit =
                    deterministic_unit_float_3d(x_index as u32, y_index as u32, z_index as u32)
                        as f64;
                let key = unit.ln() / (weight as f64);
                scored.push((key, sample));
            }
            scored.sort_by(|a, b| b.0.total_cmp(&a.0));
            weighted_samples = scored
                .into_iter()
                .take(max_samples)
                .map(|(_, sample)| sample)
                .collect();
        }
        let mut samples = Vec::with_capacity(weighted_samples.len() * 4);
        for (value, [x, y, z], _x_index, _y_index, _z_index) in weighted_samples {
            samples.extend_from_slice(&[x, y, z, value]);
        }
        Some(CartesianSpeakerHeatmapVolume {
            speaker_index,
            samples,
        })
    }
}

pub struct SampledPolarEvaluator {
    model: Box<dyn GainModel>,
    azimuth_positions: Vec<f32>,
    elevation_positions: Vec<f32>,
    distance_positions: Vec<f32>,
    gains: Vec<f32>,
    speaker_count: usize,
    position_interpolation: bool,
    frozen_request: RenderRequest,
    backend_restore_snapshot: Option<BackendRestoreSnapshot>,
}

impl SampledPolarEvaluator {
    pub fn new(model: Box<dyn GainModel>, config: &EvaluationBuildConfig) -> Self {
        let azimuth_positions = polar_azimuth_axis(config.polar.azimuth_values.max(2));
        let elevation_positions = polar_elevation_axis(
            config.polar.elevation_values.max(2),
            config.polar.allow_negative_z,
        );
        let distance_positions = evenly_spaced_axis(
            config.polar.distance_values.max(2),
            0.0,
            config.polar.distance_max.max(0.01),
        );
        let speaker_count = model.speaker_count();
        let mut gains = Vec::with_capacity(
            azimuth_positions.len()
                * elevation_positions.len()
                * distance_positions.len()
                * speaker_count,
        );
        let mut request = config.request_template;
        for &distance in &distance_positions {
            for &elevation in &elevation_positions {
                for &azimuth in &azimuth_positions {
                    let (x, y, z) = spherical_to_adm(azimuth, elevation, distance);
                    request.adm_position = [x as f64, y as f64, z as f64];
                    gains.extend_from_slice(&model.compute_gains(&request).gains);
                }
            }
        }
        let backend_restore_snapshot = build_backend_restore_snapshot(
            model.backend_id(),
            model.backend_label(),
            SerializedEvaluationMode::PrecomputedPolar,
            config,
        );
        Self {
            model,
            azimuth_positions,
            elevation_positions,
            distance_positions,
            gains,
            speaker_count,
            position_interpolation: config.position_interpolation,
            frozen_request: config.request_template,
            backend_restore_snapshot,
        }
    }
}

impl PreparedEvaluator for SampledPolarEvaluator {
    fn speaker_count(&self) -> usize {
        self.speaker_count
    }

    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        let (azimuth, elevation, distance) = adm_to_spherical(
            req.adm_position[0] as f32,
            req.adm_position[1] as f32,
            req.adm_position[2] as f32,
        );
        let gains = sample_polar_table(
            &self.gains,
            self.speaker_count,
            &self.azimuth_positions,
            &self.elevation_positions,
            &self.distance_positions,
            [azimuth, elevation, distance],
            self.position_interpolation,
        );
        RenderResponse { gains }
    }

    fn save_to_file(&self, path: &std::path::Path, speaker_layout: &SpeakerLayout) -> Result<()> {
        evaluation_artifact::LoadedEvaluationArtifact::from_sampled_polar(
            self.model.backend_id(),
            self.model.backend_label(),
            speaker_layout,
            self.frozen_request,
            self.position_interpolation,
            self.backend_restore_snapshot.as_ref(),
            &self.azimuth_positions,
            &self.elevation_positions,
            &self.distance_positions,
            &self.gains,
            self.speaker_count,
        )?
        .save_to_file(path)
    }
}

pub struct RealtimeStrategy;

impl EvaluationStrategy for RealtimeStrategy {
    fn effective_mode(&self) -> EffectiveEvaluationMode {
        EffectiveEvaluationMode::Realtime
    }

    fn prepare(
        self,
        model: Box<dyn GainModel>,
        _config: &EvaluationBuildConfig,
    ) -> Result<Box<dyn PreparedEvaluator>> {
        Ok(Box::new(RealtimeEvaluator::new(model)))
    }
}

pub struct PrecomputedCartesianStrategy;

impl EvaluationStrategy for PrecomputedCartesianStrategy {
    fn effective_mode(&self) -> EffectiveEvaluationMode {
        EffectiveEvaluationMode::PrecomputedCartesian
    }

    fn prepare(
        self,
        model: Box<dyn GainModel>,
        config: &EvaluationBuildConfig,
    ) -> Result<Box<dyn PreparedEvaluator>> {
        Ok(Box::new(SampledCartesianEvaluator::new(model, config)))
    }
}

pub struct PrecomputedPolarStrategy;

impl EvaluationStrategy for PrecomputedPolarStrategy {
    fn effective_mode(&self) -> EffectiveEvaluationMode {
        EffectiveEvaluationMode::PrecomputedPolar
    }

    fn prepare(
        self,
        model: Box<dyn GainModel>,
        config: &EvaluationBuildConfig,
    ) -> Result<Box<dyn PreparedEvaluator>> {
        Ok(Box::new(SampledPolarEvaluator::new(model, config)))
    }
}

pub struct PreparedRenderEngine {
    gain_model_kind: GainModelKind,
    backend_id: &'static str,
    backend_label: &'static str,
    capabilities: BackendCapabilities,
    evaluation_mode: EffectiveEvaluationMode,
    backend_restore_snapshot: Option<BackendRestoreSnapshot>,
    evaluator: Box<dyn PreparedEvaluator>,
}

impl PreparedRenderEngine {
    pub fn new(
        gain_model_kind: GainModelKind,
        backend_id: &'static str,
        backend_label: &'static str,
        capabilities: BackendCapabilities,
        evaluation_mode: EffectiveEvaluationMode,
        backend_restore_snapshot: Option<BackendRestoreSnapshot>,
        evaluator: Box<dyn PreparedEvaluator>,
    ) -> Self {
        Self {
            gain_model_kind,
            backend_id,
            backend_label,
            capabilities,
            evaluation_mode,
            backend_restore_snapshot,
            evaluator,
        }
    }

    pub fn kind(&self) -> RenderBackendKind {
        self.gain_model_kind.into()
    }

    pub fn gain_model_kind(&self) -> GainModelKind {
        self.gain_model_kind
    }

    pub fn backend_id(&self) -> &'static str {
        self.backend_id
    }

    pub fn backend_label(&self) -> &'static str {
        self.backend_label
    }

    pub fn capabilities(&self) -> BackendCapabilities {
        self.capabilities
    }

    pub fn evaluation_mode(&self) -> EffectiveEvaluationMode {
        self.evaluation_mode
    }

    pub fn has_backend_restore_snapshot(&self) -> bool {
        self.backend_restore_snapshot.is_some()
    }

    pub fn backend_restore_snapshot(&self) -> Option<&BackendRestoreSnapshot> {
        self.backend_restore_snapshot.as_ref()
    }

    pub fn speaker_count(&self) -> usize {
        self.evaluator.speaker_count()
    }

    pub fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        self.evaluator.compute_gains(req)
    }

    pub fn save_to_file(
        &self,
        path: &std::path::Path,
        speaker_layout: &SpeakerLayout,
    ) -> Result<()> {
        self.evaluator.save_to_file(path, speaker_layout)
    }

    pub fn cartesian_slices_for_speaker(
        &self,
        speaker_index: usize,
        speaker_position: [f32; 3],
    ) -> Option<CartesianSpeakerHeatmapSlices> {
        self.evaluator
            .cartesian_slices_for_speaker(speaker_index, speaker_position)
    }

    pub fn cartesian_volume_for_speaker(
        &self,
        speaker_index: usize,
        threshold: f32,
        max_samples: usize,
    ) -> Option<CartesianSpeakerHeatmapVolume> {
        self.evaluator
            .cartesian_volume_for_speaker(speaker_index, threshold, max_samples)
    }
}

pub fn build_prepared_render_engine(
    model: Box<dyn GainModel>,
    evaluation_mode: EffectiveEvaluationMode,
    config: &EvaluationBuildConfig,
) -> Result<PreparedRenderEngine> {
    let gain_model_kind = model.kind();
    let backend_id = model.backend_id();
    let backend_label = model.backend_label();
    let capabilities = model.capabilities();
    let evaluator = match evaluation_mode {
        EffectiveEvaluationMode::Realtime => RealtimeStrategy.prepare(model, config)?,
        EffectiveEvaluationMode::PrecomputedCartesian => {
            PrecomputedCartesianStrategy.prepare(model, config)?
        }
        EffectiveEvaluationMode::PrecomputedPolar => {
            PrecomputedPolarStrategy.prepare(model, config)?
        }
        EffectiveEvaluationMode::FromFile => {
            unreachable!("from_file evaluator is built without a gain model")
        }
    };
    Ok(PreparedRenderEngine::new(
        gain_model_kind,
        backend_id,
        backend_label,
        capabilities,
        evaluation_mode,
        None,
        evaluator,
    ))
}

#[derive(Clone, Copy)]
struct AxisSample {
    lower: usize,
    upper: usize,
    fraction: f32,
}

fn evenly_spaced_axis(count: usize, min: f32, max: f32) -> Vec<f32> {
    if count <= 1 {
        return vec![min];
    }
    let step = (max - min) / (count.saturating_sub(1) as f32);
    (0..count).map(|index| min + step * index as f32).collect()
}

fn deterministic_unit_float_3d(x: u32, y: u32, z: u32) -> f32 {
    fn splitmix64(mut state: u64) -> u64 {
        state = state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }

    let seed = ((x as u64) << 42) ^ ((y as u64) << 21) ^ (z as u64) ^ 0x6a09e667f3bcc909;
    let mixed = splitmix64(seed);
    ((mixed as f64 + 1.0) / (u64::MAX as f64 + 2.0)) as f32
}

fn cartesian_z_axis(z_size: usize, z_neg_size: usize) -> Vec<f32> {
    let mut values = Vec::with_capacity(z_neg_size + z_size);
    if z_neg_size > 0 {
        for index in 0..z_neg_size {
            let t = (index + 1) as f32 / z_neg_size as f32;
            values.push(-1.0 + (t - 1.0 / z_neg_size as f32));
        }
    }
    values.extend(evenly_spaced_axis(z_size.max(2), 0.0, 1.0));
    values
}

fn polar_azimuth_axis(count: usize) -> Vec<f32> {
    let count = count.max(2);
    let step = 360.0 / count as f32;
    (0..count)
        .map(|index| -180.0 + step * index as f32)
        .collect()
}

fn polar_elevation_axis(count: usize, allow_negative_z: bool) -> Vec<f32> {
    if allow_negative_z {
        evenly_spaced_axis(count.max(2), -90.0, 90.0)
    } else {
        evenly_spaced_axis(count.max(2), 0.0, 90.0)
    }
}

pub(crate) fn sample_cartesian_table(
    table: &[f32],
    speaker_count: usize,
    x_positions: &[f32],
    y_positions: &[f32],
    z_positions: &[f32],
    position: [f32; 3],
    interpolate: bool,
) -> Gains {
    let x = sample_axis(x_positions, position[0].clamp(-1.0, 1.0), interpolate);
    let y = sample_axis(y_positions, position[1].clamp(-1.0, 1.0), interpolate);
    let z = sample_axis(z_positions, position[2].clamp(-1.0, 1.0), interpolate);
    let mut gains = Gains::zeroed(speaker_count);
    if !interpolate {
        write_flat_sample(
            table,
            speaker_count,
            x_positions.len(),
            y_positions.len(),
            x.lower,
            y.lower,
            z.lower,
            &mut gains,
        );
        return gains;
    }

    for (iz, wz) in [(z.lower, 1.0 - z.fraction), (z.upper, z.fraction)] {
        for (iy, wy) in [(y.lower, 1.0 - y.fraction), (y.upper, y.fraction)] {
            for (ix, wx) in [(x.lower, 1.0 - x.fraction), (x.upper, x.fraction)] {
                let weight = wx * wy * wz;
                if weight <= 0.0 {
                    continue;
                }
                accumulate_flat_sample(
                    table,
                    speaker_count,
                    x_positions.len(),
                    y_positions.len(),
                    ix,
                    iy,
                    iz,
                    weight,
                    &mut gains,
                );
            }
        }
    }
    gains
}

fn sample_cartesian_table_speaker_value(
    table: &[f32],
    speaker_count: usize,
    x_positions: &[f32],
    y_positions: &[f32],
    z_positions: &[f32],
    position: [f32; 3],
    interpolate: bool,
    speaker_index: usize,
) -> f32 {
    let x = sample_axis(x_positions, position[0].clamp(-1.0, 1.0), interpolate);
    let y = sample_axis(y_positions, position[1].clamp(-1.0, 1.0), interpolate);
    let z = sample_axis(z_positions, position[2].clamp(-1.0, 1.0), interpolate);
    if !interpolate {
        return read_flat_sample_value(
            table,
            speaker_count,
            x_positions.len(),
            y_positions.len(),
            x.lower,
            y.lower,
            z.lower,
            speaker_index,
        );
    }

    let mut value = 0.0;
    for (iz, wz) in [(z.lower, 1.0 - z.fraction), (z.upper, z.fraction)] {
        for (iy, wy) in [(y.lower, 1.0 - y.fraction), (y.upper, y.fraction)] {
            for (ix, wx) in [(x.lower, 1.0 - x.fraction), (x.upper, x.fraction)] {
                let weight = wx * wy * wz;
                if weight <= 0.0 {
                    continue;
                }
                value += read_flat_sample_value(
                    table,
                    speaker_count,
                    x_positions.len(),
                    y_positions.len(),
                    ix,
                    iy,
                    iz,
                    speaker_index,
                ) * weight;
            }
        }
    }
    value
}

pub(crate) fn sample_polar_table(
    table: &[f32],
    speaker_count: usize,
    azimuth_positions: &[f32],
    elevation_positions: &[f32],
    distance_positions: &[f32],
    position: [f32; 3],
    interpolate: bool,
) -> Gains {
    let azimuth = sample_wrapped_axis(azimuth_positions, wrap_degrees(position[0]), interpolate);
    let elevation = sample_axis(
        elevation_positions,
        position[1].clamp(
            *elevation_positions.first().unwrap_or(&-90.0),
            *elevation_positions.last().unwrap_or(&90.0),
        ),
        interpolate,
    );
    let distance = sample_axis(
        distance_positions,
        position[2].clamp(0.0, *distance_positions.last().unwrap_or(&0.0)),
        interpolate,
    );
    let mut gains = Gains::zeroed(speaker_count);
    if !interpolate {
        write_flat_sample(
            table,
            speaker_count,
            azimuth_positions.len(),
            elevation_positions.len(),
            azimuth.lower,
            elevation.lower,
            distance.lower,
            &mut gains,
        );
        return gains;
    }

    for (id, wd) in [
        (distance.lower, 1.0 - distance.fraction),
        (distance.upper, distance.fraction),
    ] {
        for (ie, we) in [
            (elevation.lower, 1.0 - elevation.fraction),
            (elevation.upper, elevation.fraction),
        ] {
            for (ia, wa) in [
                (azimuth.lower, 1.0 - azimuth.fraction),
                (azimuth.upper, azimuth.fraction),
            ] {
                let weight = wa * we * wd;
                if weight <= 0.0 {
                    continue;
                }
                accumulate_flat_sample(
                    table,
                    speaker_count,
                    azimuth_positions.len(),
                    elevation_positions.len(),
                    ia,
                    ie,
                    id,
                    weight,
                    &mut gains,
                );
            }
        }
    }
    gains
}

fn read_flat_sample_value(
    table: &[f32],
    speaker_count: usize,
    x_len: usize,
    y_len: usize,
    x_index: usize,
    y_index: usize,
    z_index: usize,
    speaker_index: usize,
) -> f32 {
    let offset = (((z_index * y_len) + y_index) * x_len + x_index) * speaker_count + speaker_index;
    table.get(offset).copied().unwrap_or(0.0)
}

fn write_flat_sample(
    table: &[f32],
    speaker_count: usize,
    x_len: usize,
    y_len: usize,
    x_index: usize,
    y_index: usize,
    z_index: usize,
    gains: &mut Gains,
) {
    let offset = flat_sample_offset(speaker_count, x_len, y_len, x_index, y_index, z_index);
    for speaker in 0..speaker_count {
        gains.set(speaker, table[offset + speaker]);
    }
}

fn accumulate_flat_sample(
    table: &[f32],
    speaker_count: usize,
    x_len: usize,
    y_len: usize,
    x_index: usize,
    y_index: usize,
    z_index: usize,
    weight: f32,
    gains: &mut Gains,
) {
    let offset = flat_sample_offset(speaker_count, x_len, y_len, x_index, y_index, z_index);
    for speaker in 0..speaker_count {
        gains[speaker] += table[offset + speaker] * weight;
    }
}

fn flat_sample_offset(
    speaker_count: usize,
    x_len: usize,
    y_len: usize,
    x_index: usize,
    y_index: usize,
    z_index: usize,
) -> usize {
    (((z_index * y_len) + y_index) * x_len + x_index) * speaker_count
}

fn sample_axis(values: &[f32], position: f32, interpolate: bool) -> AxisSample {
    if values.len() <= 1 {
        return AxisSample {
            lower: 0,
            upper: 0,
            fraction: 0.0,
        };
    }
    if !interpolate {
        let nearest = values
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| ((*a - position).abs()).total_cmp(&((*b - position).abs())))
            .map(|(index, _)| index)
            .unwrap_or(0);
        return AxisSample {
            lower: nearest,
            upper: nearest,
            fraction: 0.0,
        };
    }
    if position <= values[0] {
        return AxisSample {
            lower: 0,
            upper: 0,
            fraction: 0.0,
        };
    }
    let upper = values.partition_point(|value| *value < position);
    if upper >= values.len() {
        let last = values.len() - 1;
        return AxisSample {
            lower: last,
            upper: last,
            fraction: 0.0,
        };
    }
    let lower = upper.saturating_sub(1);
    let span = (values[upper] - values[lower]).max(1e-6);
    AxisSample {
        lower,
        upper,
        fraction: ((position - values[lower]) / span).clamp(0.0, 1.0),
    }
}

fn sample_wrapped_axis(values: &[f32], position: f32, interpolate: bool) -> AxisSample {
    if values.len() <= 1 {
        return AxisSample {
            lower: 0,
            upper: 0,
            fraction: 0.0,
        };
    }
    if !interpolate {
        let nearest = values
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                wrapped_angle_distance(**a, position)
                    .total_cmp(&wrapped_angle_distance(**b, position))
            })
            .map(|(index, _)| index)
            .unwrap_or(0);
        return AxisSample {
            lower: nearest,
            upper: nearest,
            fraction: 0.0,
        };
    }
    let mut best = AxisSample {
        lower: 0,
        upper: 0,
        fraction: 0.0,
    };
    let mut best_distance = f32::MAX;
    for index in 0..values.len() {
        let next = (index + 1) % values.len();
        let start = values[index];
        let end = if next == 0 {
            values[0] + 360.0
        } else {
            values[next]
        };
        let value = if position < start {
            position + 360.0
        } else {
            position
        };
        if value < start || value > end {
            continue;
        }
        let span = (end - start).max(1e-6);
        return AxisSample {
            lower: index,
            upper: next,
            fraction: ((value - start) / span).clamp(0.0, 1.0),
        };
    }
    for (index, axis) in values.iter().enumerate() {
        let distance = wrapped_angle_distance(*axis, position);
        if distance < best_distance {
            best_distance = distance;
            best = AxisSample {
                lower: index,
                upper: index,
                fraction: 0.0,
            };
        }
    }
    best
}

#[inline]
fn wrap_degrees(value: f32) -> f32 {
    let wrapped = (value + 180.0).rem_euclid(360.0) - 180.0;
    if wrapped == -180.0 { 180.0 } else { wrapped }
}

#[inline]
fn wrapped_angle_distance(a: f32, b: f32) -> f32 {
    let delta = (a - b).abs().rem_euclid(360.0);
    delta.min(360.0 - delta)
}
