mod position;

pub use position::PositionRampStrategy;

use crate::render_backend::RenderRequest;
use crate::spatial_vbap::{DistanceModel, MirrorAxes};

#[derive(Debug, Clone, Copy)]
pub struct RampRenderParams {
    pub room_ratio: [f32; 3],
    pub room_ratio_rear: f32,
    pub room_ratio_lower: f32,
    pub room_ratio_center_blend: f32,
    pub use_distance_diffuse: bool,
    pub distance_diffuse_threshold: f32,
    pub distance_diffuse_curve: f32,
    pub diffuse_mirror_axes: MirrorAxes,
    pub distance_model: DistanceModel,
}

impl RampRenderParams {
    #[inline]
    pub fn render_request_for_event(
        self,
        position: [f64; 3],
        event_size: [f32; 3],
    ) -> RenderRequest {
        RenderRequest {
            adm_position: position,
            event_size,
            room_ratio: self.room_ratio,
            room_ratio_rear: self.room_ratio_rear,
            room_ratio_lower: self.room_ratio_lower,
            room_ratio_center_blend: self.room_ratio_center_blend,
            use_distance_diffuse: self.use_distance_diffuse,
            distance_diffuse_threshold: self.distance_diffuse_threshold,
            distance_diffuse_curve: self.distance_diffuse_curve,
            diffuse_mirror_axes: self.diffuse_mirror_axes,
            distance_model: self.distance_model,
        }
    }
}

pub struct RampContext {
    render_params: RampRenderParams,
}

impl RampContext {
    pub fn new(render_params: RampRenderParams) -> Self {
        Self { render_params }
    }

    #[inline]
    pub fn render_params(&self) -> RampRenderParams {
        self.render_params
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RampTarget {
    pub position: [f64; 3],
    pub size: [f32; 3],
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
pub struct ChannelRampState {
    pub start_position: [f64; 3],
    pub start_size: [f32; 3],
    pub current_position: [f64; 3],
    pub current_size: [f32; 3],
    pub target_position: [f64; 3],
    pub target_size: [f32; 3],
    pub output_position: [f64; 3],
    pub ramp_length: u64,
    pub remaining_ramp_units: Option<u64>,
    pub target_sample_index: Option<u64>,
}

impl Default for ChannelRampState {
    fn default() -> Self {
        Self {
            start_position: [0.0; 3],
            start_size: [0.0; 3],
            current_position: [0.0; 3],
            current_size: [0.0; 3],
            target_position: [0.0; 3],
            target_size: [0.0; 3],
            output_position: [0.0; 3],
            ramp_length: 0,
            remaining_ramp_units: None,
            target_sample_index: None,
        }
    }
}

impl ChannelRampState {
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
                self.start_size = self.target_size;
                self.current_position = self.target_position;
                self.current_size = self.target_size;
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
        ctx: &RampContext,
    );

    fn evaluate(
        &self,
        state: &mut ChannelRampState,
        progress: RampProgress,
        ctx: &RampContext,
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
pub(crate) fn interpolate_size(current: [f32; 3], target: [f32; 3], fraction: f64) -> [f32; 3] {
    let f = fraction as f32;
    let inv = 1.0 - f;
    [
        current[0] * inv + target[0] * f,
        current[1] * inv + target[1] * f,
        current[2] * inv + target[2] * f,
    ]
}
