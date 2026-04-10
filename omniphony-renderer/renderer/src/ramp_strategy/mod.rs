mod gain_table;
mod position;

pub use gain_table::GainTableRampStrategy;
pub use position::PositionRampStrategy;

use crate::render_backend::{PreparedRenderEngine, RenderRequest};
use crate::spatial_vbap::{DistanceModel, Gains};

#[derive(Debug, Clone, Copy)]
pub struct RampRenderParams {
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

impl RampRenderParams {
    #[inline]
    pub fn render_request(self, position: [f64; 3]) -> RenderRequest {
        RenderRequest {
            adm_position: position,
            spread_min: self.spread_min,
            spread_max: self.spread_max,
            spread_from_distance: self.spread_from_distance,
            spread_distance_range: self.spread_distance_range,
            spread_distance_curve: self.spread_distance_curve,
            room_ratio: self.room_ratio,
            room_ratio_rear: self.room_ratio_rear,
            room_ratio_lower: self.room_ratio_lower,
            room_ratio_center_blend: self.room_ratio_center_blend,
            use_distance_diffuse: self.use_distance_diffuse,
            distance_diffuse_threshold: self.distance_diffuse_threshold,
            distance_diffuse_curve: self.distance_diffuse_curve,
            distance_model: self.distance_model,
        }
    }
}

pub struct RampContext<'a> {
    backend: &'a PreparedRenderEngine,
    topology_identity: usize,
    render_params: RampRenderParams,
}

impl<'a> RampContext<'a> {
    pub fn new(
        backend: &'a PreparedRenderEngine,
        topology_identity: usize,
        render_params: RampRenderParams,
    ) -> Self {
        Self {
            backend,
            topology_identity,
            render_params,
        }
    }

    #[inline]
    pub fn topology_identity(&self) -> usize {
        self.topology_identity
    }

    #[inline]
    pub fn speaker_count(&self) -> usize {
        self.backend.speaker_count()
    }

    #[inline]
    pub fn render_params(&self) -> RampRenderParams {
        self.render_params
    }

    #[inline]
    pub fn compute_gains(&self, position: [f64; 3]) -> Gains {
        self.backend
            .compute_gains(&self.render_params.render_request(position))
            .gains
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RampTarget {
    pub position: [f64; 3],
    pub spread: f32,
    pub ramp_length: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RampStatus {
    Ramping,
    Finished,
    Idle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RampProgress {
    pub completed_units: u64,
    pub total_units: u64,
}

impl RampProgress {
    #[inline]
    pub fn fraction(self) -> f64 {
        if self.total_units == 0 {
            1.0
        } else {
            (self.completed_units as f64 / self.total_units as f64).clamp(0.0, 1.0)
        }
    }

    #[inline]
    pub fn is_finished(self) -> bool {
        self.completed_units >= self.total_units
    }
}

#[derive(Clone)]
struct GainCache {
    topology_identity: usize,
    position_bits: [u64; 3],
    room_ratio_bits: [u32; 3],
    room_ratio_rear_bits: u32,
    room_ratio_lower_bits: u32,
    room_ratio_center_blend_bits: u32,
    spread_min_bits: u32,
    spread_max_bits: u32,
    spread_distance_range_bits: u32,
    spread_distance_curve_bits: u32,
    distance_diffuse_threshold_bits: u32,
    distance_diffuse_curve_bits: u32,
    spread_from_distance: bool,
    use_distance_diffuse: bool,
    distance_model: DistanceModel,
    valid: bool,
    gains: Gains,
}

impl Default for GainCache {
    fn default() -> Self {
        Self {
            topology_identity: 0,
            position_bits: [0; 3],
            room_ratio_bits: [0; 3],
            room_ratio_rear_bits: 0,
            room_ratio_lower_bits: 0,
            room_ratio_center_blend_bits: 0,
            spread_min_bits: 0,
            spread_max_bits: 0,
            spread_distance_range_bits: 0,
            spread_distance_curve_bits: 0,
            distance_diffuse_threshold_bits: 0,
            distance_diffuse_curve_bits: 0,
            spread_from_distance: false,
            use_distance_diffuse: false,
            distance_model: DistanceModel::Linear,
            valid: false,
            gains: Gains::zeroed(0),
        }
    }
}

impl GainCache {
    fn matches(&self, position: [f64; 3], ctx: &RampContext<'_>) -> bool {
        let render = ctx.render_params();
        self.valid
            && self.topology_identity == ctx.topology_identity()
            && self.position_bits == position.map(f64::to_bits)
            && self.room_ratio_bits == render.room_ratio.map(f32::to_bits)
            && self.room_ratio_rear_bits == render.room_ratio_rear.to_bits()
            && self.room_ratio_lower_bits == render.room_ratio_lower.to_bits()
            && self.room_ratio_center_blend_bits == render.room_ratio_center_blend.to_bits()
            && self.spread_min_bits == render.spread_min.to_bits()
            && self.spread_max_bits == render.spread_max.to_bits()
            && self.spread_distance_range_bits == render.spread_distance_range.to_bits()
            && self.spread_distance_curve_bits == render.spread_distance_curve.to_bits()
            && self.distance_diffuse_threshold_bits == render.distance_diffuse_threshold.to_bits()
            && self.distance_diffuse_curve_bits == render.distance_diffuse_curve.to_bits()
            && self.spread_from_distance == render.spread_from_distance
            && self.use_distance_diffuse == render.use_distance_diffuse
            && self.distance_model == render.distance_model
    }

    fn store(&mut self, position: [f64; 3], ctx: &RampContext<'_>, gains: &Gains) {
        let render = ctx.render_params();
        self.topology_identity = ctx.topology_identity();
        self.position_bits = position.map(f64::to_bits);
        self.room_ratio_bits = render.room_ratio.map(f32::to_bits);
        self.room_ratio_rear_bits = render.room_ratio_rear.to_bits();
        self.room_ratio_lower_bits = render.room_ratio_lower.to_bits();
        self.room_ratio_center_blend_bits = render.room_ratio_center_blend.to_bits();
        self.spread_min_bits = render.spread_min.to_bits();
        self.spread_max_bits = render.spread_max.to_bits();
        self.spread_distance_range_bits = render.spread_distance_range.to_bits();
        self.spread_distance_curve_bits = render.spread_distance_curve.to_bits();
        self.distance_diffuse_threshold_bits = render.distance_diffuse_threshold.to_bits();
        self.distance_diffuse_curve_bits = render.distance_diffuse_curve.to_bits();
        self.spread_from_distance = render.spread_from_distance;
        self.use_distance_diffuse = render.use_distance_diffuse;
        self.distance_model = render.distance_model;
        self.valid = true;
        self.gains = gains.clone();
    }
}

#[derive(Clone)]
pub struct ChannelRampState {
    pub start_position: [f64; 3],
    pub start_spread: f32,
    pub current_position: [f64; 3],
    pub current_spread: f32,
    pub target_position: [f64; 3],
    pub target_spread: f32,
    pub output_position: [f64; 3],
    pub ramp_length: u64,
    pub remaining_ramp_units: Option<u64>,
    pub target_sample_index: Option<u64>,
    pub start_gains: Gains,
    pub target_gains: Gains,
    pub output_gains: Gains,
    pub(crate) gains_initialized: bool,
    cache: GainCache,
}

impl Default for ChannelRampState {
    fn default() -> Self {
        Self {
            start_position: [0.0; 3],
            start_spread: 0.0,
            current_position: [0.0; 3],
            current_spread: 0.0,
            target_position: [0.0; 3],
            target_spread: 0.0,
            output_position: [0.0; 3],
            ramp_length: 0,
            remaining_ramp_units: None,
            target_sample_index: None,
            start_gains: Gains::zeroed(0),
            target_gains: Gains::zeroed(0),
            output_gains: Gains::zeroed(0),
            gains_initialized: false,
            cache: GainCache::default(),
        }
    }
}

impl ChannelRampState {
    pub fn ensure_speaker_count(&mut self, speaker_count: usize) {
        if self.output_gains.len() == speaker_count {
            return;
        }
        self.start_gains = Gains::zeroed(speaker_count);
        self.target_gains = Gains::zeroed(speaker_count);
        self.output_gains = Gains::zeroed(speaker_count);
        self.cache.gains = Gains::zeroed(speaker_count);
        self.gains_initialized = false;
        self.cache.valid = false;
    }

    pub fn output_gains(&self) -> &Gains {
        &self.output_gains
    }

    pub fn cached_gains(&self, position: [f64; 3], ctx: &RampContext<'_>) -> Option<&Gains> {
        self.cache
            .matches(position, ctx)
            .then_some(&self.cache.gains)
    }

    pub fn store_cached_gains(&mut self, position: [f64; 3], ctx: &RampContext<'_>, gains: &Gains) {
        self.cache.store(position, ctx, gains);
    }

    pub fn invalidate_cache(&mut self) {
        self.cache.valid = false;
    }

    pub fn current_progress(&self) -> Option<RampProgress> {
        self.remaining_ramp_units.map(|remaining| {
            let total = self.ramp_length.max(1);
            RampProgress {
                completed_units: total.saturating_sub(remaining.min(total)),
                total_units: total,
            }
        })
    }

    pub fn advance_ramp(&mut self, step_units: u64) -> RampStatus {
        match self.remaining_ramp_units {
            Some(remaining) if remaining > step_units => {
                self.remaining_ramp_units = Some(remaining - step_units);
                RampStatus::Ramping
            }
            Some(_) => {
                self.remaining_ramp_units = None;
                self.start_position = self.target_position;
                self.start_spread = self.target_spread;
                self.current_position = self.target_position;
                self.current_spread = self.target_spread;
                self.start_gains = self.target_gains.clone();
                self.output_gains = self.target_gains.clone();
                RampStatus::Finished
            }
            None => RampStatus::Idle,
        }
    }

    pub fn commit_output_position(&mut self) {
        self.current_position = self.output_position;
    }
}

pub trait RampStrategy: Send + Sync {
    fn name(&self) -> &'static str;

    fn update_target(
        &self,
        state: &mut ChannelRampState,
        target: RampTarget,
        sample_index: Option<u64>,
        ctx: &RampContext<'_>,
    );

    fn evaluate(
        &self,
        state: &mut ChannelRampState,
        progress: RampProgress,
        ctx: &RampContext<'_>,
    ) -> RampStatus;
}

#[inline]
pub(crate) fn interpolate_position(current: [f64; 3], target: [f64; 3], fraction: f64) -> [f64; 3] {
    let inv = 1.0 - fraction;
    [
        current[0] * inv + target[0] * fraction,
        current[1] * inv + target[1] * fraction,
        current[2] * inv + target[2] * fraction,
    ]
}

#[inline]
pub(crate) fn interpolate_scalar(current: f32, target: f32, fraction: f64) -> f32 {
    let inv = (1.0 - fraction) as f32;
    current * inv + target * fraction as f32
}

pub(crate) fn compute_cached_or_direct(
    state: &mut ChannelRampState,
    position: [f64; 3],
    ctx: &RampContext<'_>,
) {
    if let Some(cached) = state.cached_gains(position, ctx) {
        state.output_gains = cached.clone();
    } else {
        let gains = ctx.compute_gains(position);
        state.store_cached_gains(position, ctx, &gains);
        state.output_gains = gains;
    }
    state.start_gains = state.output_gains.clone();
    state.gains_initialized = true;
}
