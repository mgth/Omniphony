use crate::spatial_vbap::{
    DistanceModel, Gains, VbapPanner, adm_to_spherical, spherical_to_adm,
};
use crate::speaker_layout::SpeakerLayout;
use anyhow::Result;
use serde::Serialize;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveEvaluationMode {
    Realtime,
    PrecomputedPolar,
    PrecomputedCartesian,
}

impl EffectiveEvaluationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Realtime => "realtime",
            Self::PrecomputedPolar => "precomputed_polar",
            Self::PrecomputedCartesian => "precomputed_cartesian",
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
    fn speaker_count(&self) -> usize;
    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse;
    fn save_to_file(
        &self,
        path: &std::path::Path,
        speaker_layout: &SpeakerLayout,
    ) -> Result<()>;
}

pub trait EvaluationStrategy<M: GainModel> {
    fn effective_mode(&self) -> EffectiveEvaluationMode;
    fn prepare(
        self,
        model: M,
        config: &EvaluationBuildConfig,
    ) -> Result<Box<dyn PreparedEvaluator>>;
}

pub enum GainModelInstance {
    Vbap(VbapBackend),
    ExperimentalDistance(ExperimentalDistanceBackend),
}

impl GainModelInstance {
    pub fn kind(&self) -> GainModelKind {
        match self {
            Self::Vbap(_) => GainModelKind::Vbap,
            Self::ExperimentalDistance(_) => GainModelKind::ExperimentalDistance,
        }
    }
}

pub trait PreparedEvaluator: Send + Sync {
    fn speaker_count(&self) -> usize;
    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse;
    fn save_to_file(
        &self,
        path: &std::path::Path,
        speaker_layout: &SpeakerLayout,
    ) -> Result<()>;
    fn cartesian_slices_for_speaker(
        &self,
        speaker_index: usize,
        speaker_position: [f32; 3],
    ) -> Option<CartesianSpeakerHeatmapSlices> {
        let _ = (speaker_index, speaker_position);
        None
    }
}

pub struct RealtimeEvaluator<M: GainModel> {
    model: M,
}

impl<M: GainModel> RealtimeEvaluator<M> {
    pub fn new(model: M) -> Self {
        Self { model }
    }
}

impl<M: GainModel> PreparedEvaluator for RealtimeEvaluator<M> {
    fn speaker_count(&self) -> usize {
        self.model.speaker_count()
    }

    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        self.model.compute_gains(req)
    }

    fn save_to_file(
        &self,
        path: &std::path::Path,
        speaker_layout: &SpeakerLayout,
    ) -> Result<()> {
        self.model.save_to_file(path, speaker_layout)
    }
}

pub struct SampledCartesianEvaluator<M: GainModel> {
    model: M,
    x_positions: Vec<f32>,
    y_positions: Vec<f32>,
    z_positions: Vec<f32>,
    gains: Vec<f32>,
    speaker_count: usize,
    position_interpolation: bool,
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

impl<M: GainModel> SampledCartesianEvaluator<M> {
    pub fn new(model: M, config: &EvaluationBuildConfig) -> Self {
        let x_positions = evenly_spaced_axis(config.cartesian.x_size.max(2), -1.0, 1.0);
        let y_positions = evenly_spaced_axis(config.cartesian.y_size.max(2), -1.0, 1.0);
        let z_positions = cartesian_z_axis(
            config.cartesian.z_size.max(2),
            config.cartesian.z_neg_size,
        );
        let speaker_count = model.speaker_count();
        let mut gains =
            Vec::with_capacity(x_positions.len() * y_positions.len() * z_positions.len() * speaker_count);
        let mut request = config.request_template;
        for &z in &z_positions {
            for &y in &y_positions {
                for &x in &x_positions {
                    request.adm_position = [x as f64, y as f64, z as f64];
                    gains.extend_from_slice(&model.compute_gains(&request).gains);
                }
            }
        }
        Self {
            model,
            x_positions,
            y_positions,
            z_positions,
            gains,
            speaker_count,
            position_interpolation: config.position_interpolation,
        }
    }
}

impl<M: GainModel> PreparedEvaluator for SampledCartesianEvaluator<M> {
    fn speaker_count(&self) -> usize {
        self.speaker_count
    }

    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
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

    fn save_to_file(
        &self,
        path: &std::path::Path,
        speaker_layout: &SpeakerLayout,
    ) -> Result<()> {
        self.model.save_to_file(path, speaker_layout)
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
}

pub struct SampledPolarEvaluator<M: GainModel> {
    model: M,
    azimuth_positions: Vec<f32>,
    elevation_positions: Vec<f32>,
    distance_positions: Vec<f32>,
    gains: Vec<f32>,
    speaker_count: usize,
    position_interpolation: bool,
}

impl<M: GainModel> SampledPolarEvaluator<M> {
    pub fn new(model: M, config: &EvaluationBuildConfig) -> Self {
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
        Self {
            model,
            azimuth_positions,
            elevation_positions,
            distance_positions,
            gains,
            speaker_count,
            position_interpolation: config.position_interpolation,
        }
    }
}

impl<M: GainModel> PreparedEvaluator for SampledPolarEvaluator<M> {
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

    fn save_to_file(
        &self,
        path: &std::path::Path,
        speaker_layout: &SpeakerLayout,
    ) -> Result<()> {
        self.model.save_to_file(path, speaker_layout)
    }
}

pub struct RealtimeStrategy;

impl<M: GainModel> EvaluationStrategy<M> for RealtimeStrategy {
    fn effective_mode(&self) -> EffectiveEvaluationMode {
        EffectiveEvaluationMode::Realtime
    }

    fn prepare(
        self,
        model: M,
        _config: &EvaluationBuildConfig,
    ) -> Result<Box<dyn PreparedEvaluator>> {
        Ok(Box::new(RealtimeEvaluator::new(model)))
    }
}

pub struct PrecomputedCartesianStrategy;

impl<M: GainModel> EvaluationStrategy<M> for PrecomputedCartesianStrategy {
    fn effective_mode(&self) -> EffectiveEvaluationMode {
        EffectiveEvaluationMode::PrecomputedCartesian
    }

    fn prepare(
        self,
        model: M,
        config: &EvaluationBuildConfig,
    ) -> Result<Box<dyn PreparedEvaluator>> {
        Ok(Box::new(SampledCartesianEvaluator::new(model, config)))
    }
}

pub struct PrecomputedPolarStrategy;

impl<M: GainModel> EvaluationStrategy<M> for PrecomputedPolarStrategy {
    fn effective_mode(&self) -> EffectiveEvaluationMode {
        EffectiveEvaluationMode::PrecomputedPolar
    }

    fn prepare(
        self,
        model: M,
        config: &EvaluationBuildConfig,
    ) -> Result<Box<dyn PreparedEvaluator>> {
        Ok(Box::new(SampledPolarEvaluator::new(model, config)))
    }
}

pub struct PreparedRenderEngine {
    gain_model_kind: GainModelKind,
    evaluation_mode: EffectiveEvaluationMode,
    evaluator: Box<dyn PreparedEvaluator>,
}

impl PreparedRenderEngine {
    pub fn new(
        gain_model_kind: GainModelKind,
        evaluation_mode: EffectiveEvaluationMode,
        evaluator: Box<dyn PreparedEvaluator>,
    ) -> Self {
        Self {
            gain_model_kind,
            evaluation_mode,
            evaluator,
        }
    }

    pub fn kind(&self) -> RenderBackendKind {
        self.gain_model_kind.into()
    }

    pub fn gain_model_kind(&self) -> GainModelKind {
        self.gain_model_kind
    }

    pub fn evaluation_mode(&self) -> EffectiveEvaluationMode {
        self.evaluation_mode
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
}

pub fn build_prepared_render_engine(
    model: GainModelInstance,
    evaluation_mode: EffectiveEvaluationMode,
    config: &EvaluationBuildConfig,
) -> Result<PreparedRenderEngine> {
    match (model, evaluation_mode) {
        (GainModelInstance::Vbap(model), EffectiveEvaluationMode::Realtime) => {
            let strategy = RealtimeStrategy;
            Ok(PreparedRenderEngine::new(
                GainModelKind::Vbap,
                EffectiveEvaluationMode::Realtime,
                strategy.prepare(model, config)?,
            ))
        }
        (GainModelInstance::Vbap(model), EffectiveEvaluationMode::PrecomputedCartesian) => {
            let strategy = PrecomputedCartesianStrategy;
            Ok(PreparedRenderEngine::new(
                GainModelKind::Vbap,
                EffectiveEvaluationMode::PrecomputedCartesian,
                strategy.prepare(model, config)?,
            ))
        }
        (GainModelInstance::Vbap(model), EffectiveEvaluationMode::PrecomputedPolar) => {
            let strategy = PrecomputedPolarStrategy;
            Ok(PreparedRenderEngine::new(
                GainModelKind::Vbap,
                EffectiveEvaluationMode::PrecomputedPolar,
                strategy.prepare(model, config)?,
            ))
        }
        (GainModelInstance::ExperimentalDistance(model), EffectiveEvaluationMode::Realtime) => {
            let strategy = RealtimeStrategy;
            Ok(PreparedRenderEngine::new(
                GainModelKind::ExperimentalDistance,
                EffectiveEvaluationMode::Realtime,
                strategy.prepare(model, config)?,
            ))
        }
        (
            GainModelInstance::ExperimentalDistance(model),
            EffectiveEvaluationMode::PrecomputedCartesian,
        ) => {
            let strategy = PrecomputedCartesianStrategy;
            Ok(PreparedRenderEngine::new(
                GainModelKind::ExperimentalDistance,
                EffectiveEvaluationMode::PrecomputedCartesian,
                strategy.prepare(model, config)?,
            ))
        }
        (
            GainModelInstance::ExperimentalDistance(model),
            EffectiveEvaluationMode::PrecomputedPolar,
        ) => {
            let strategy = PrecomputedPolarStrategy;
            Ok(PreparedRenderEngine::new(
                GainModelKind::ExperimentalDistance,
                EffectiveEvaluationMode::PrecomputedPolar,
                strategy.prepare(model, config)?,
            ))
        }
    }
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
    (0..count).map(|index| -180.0 + step * index as f32).collect()
}

fn polar_elevation_axis(count: usize, allow_negative_z: bool) -> Vec<f32> {
    if allow_negative_z {
        evenly_spaced_axis(count.max(2), -90.0, 90.0)
    } else {
        evenly_spaced_axis(count.max(2), 0.0, 90.0)
    }
}

fn sample_cartesian_table(
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

fn sample_polar_table(
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
                wrapped_angle_distance(**a, position).total_cmp(&wrapped_angle_distance(**b, position))
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
        let value = if position < start { position + 360.0 } else { position };
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

impl GainModel for VbapBackend {
    fn kind(&self) -> GainModelKind {
        GainModelKind::Vbap
    }

    fn speaker_count(&self) -> usize {
        VbapBackend::speaker_count(self)
    }

    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        VbapBackend::compute_gains(self, req)
    }

    fn save_to_file(
        &self,
        path: &std::path::Path,
        speaker_layout: &SpeakerLayout,
    ) -> Result<()> {
        VbapBackend::save_to_file(self, path, speaker_layout)
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

    fn speaker_count(&self) -> usize {
        ExperimentalDistanceBackend::speaker_count(self)
    }

    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        ExperimentalDistanceBackend::compute_gains(self, req)
    }

    fn save_to_file(
        &self,
        path: &std::path::Path,
        speaker_layout: &SpeakerLayout,
    ) -> Result<()> {
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
