use std::sync::Arc;

use anyhow::Result;

use crate::live_params::{
    BackendRebuildParams, LiveEvaluationMode, LiveParams, PreferredEvaluationMode, RenderTopology,
};
use crate::render_backend::{
    EffectiveEvaluationMode, GainModel, GainModelKind, RenderBackendKind, backend_descriptor_by_id,
    build_prepared_render_engine,
};
use crate::spatial_vbap::VbapTableMode;
use crate::speaker_layout::SpeakerLayout;

#[derive(Clone)]
pub enum BackendBuildPlan {
    Vbap(VbapTopologyBuildPlan),
    Barycenter(BarycenterBuildPlan),
    ExperimentalDistance(ExperimentalDistanceBuildPlan),
}

#[derive(Clone)]
pub struct ExperimentalDistanceBuildPlan {
    pub speaker_positions: Vec<[f32; 3]>,
}

#[derive(Clone)]
pub struct BarycenterBuildPlan {
    pub speaker_positions: Vec<[f32; 3]>,
}

#[derive(Clone)]
pub struct VbapTopologyBuildPlan {
    pub layout: SpeakerLayout,
    pub positions: Vec<[f32; 2]>,
    pub azimuth_resolution: i32,
    pub elevation_resolution: i32,
    pub distance_res: f32,
    pub distance_max: f32,
    pub position_interpolation: bool,
    pub table_mode: VbapTableMode,
    pub allow_negative_z: bool,
    pub distance_model: crate::spatial_vbap::DistanceModel,
    pub spread_min: f32,
    pub spread_max: f32,
    pub spread_from_distance: bool,
    pub spread_distance_range: f32,
    pub spread_distance_curve: f32,
    pub room_ratio: [f32; 3],
    pub room_ratio_rear: f32,
    pub room_ratio_lower: f32,
    pub room_ratio_center_blend: f32,
    pub diffuse: bool,
    pub diffuse_thr: f32,
    pub diffuse_curve: f32,
}

impl VbapTopologyBuildPlan {
    pub fn build_gain_model(
        &self,
        _evaluation_mode: LiveEvaluationMode,
    ) -> Result<Box<dyn GainModel>> {
        let vbap = crate::spatial_vbap::VbapPanner::new_with_mode(
            &self.positions,
            self.azimuth_resolution,
            self.elevation_resolution,
            0.0,
            self.table_mode,
        )
        .map_err(|e| anyhow::anyhow!("Failed to create VBAP panner: {}", e))?
        .with_negative_z(self.allow_negative_z)
        .with_position_interpolation(self.position_interpolation);

        Ok(Box::new(crate::render_backend::VbapBackend::new(vbap)))
    }
}

impl ExperimentalDistanceBuildPlan {
    pub fn build_gain_model(&self) -> Result<Box<dyn GainModel>> {
        Ok(Box::new(
            crate::render_backend::ExperimentalDistanceBackend::new(self.speaker_positions.clone()),
        ))
    }
}

impl BarycenterBuildPlan {
    pub fn build_gain_model(&self) -> Result<Box<dyn GainModel>> {
        Ok(Box::new(crate::render_backend::BarycenterBackend::new(
            self.speaker_positions.clone(),
        )))
    }
}

#[derive(Clone)]
pub struct TopologyBuildPlan {
    pub layout: SpeakerLayout,
    pub backend_id: String,
    pub backend_build: BackendBuildPlan,
    pub evaluation_mode: LiveEvaluationMode,
    pub evaluation_build_config: crate::render_backend::EvaluationBuildConfig,
}

impl TopologyBuildPlan {
    pub fn build_topology(&self) -> Result<RenderTopology> {
        let model = match &self.backend_build {
            BackendBuildPlan::Vbap(plan) => plan.build_gain_model(self.evaluation_mode)?,
            BackendBuildPlan::Barycenter(plan) => plan.build_gain_model()?,
            BackendBuildPlan::ExperimentalDistance(plan) => plan.build_gain_model()?,
        };
        let effective_mode = match self.evaluation_mode {
            LiveEvaluationMode::Realtime => EffectiveEvaluationMode::Realtime,
            LiveEvaluationMode::PrecomputedPolar => EffectiveEvaluationMode::PrecomputedPolar,
            LiveEvaluationMode::PrecomputedCartesian => {
                EffectiveEvaluationMode::PrecomputedCartesian
            }
            LiveEvaluationMode::Auto => unreachable!("topology build plan must resolve auto mode"),
        };
        RenderTopology::new(
            Arc::new(build_prepared_render_engine(
                model,
                effective_mode,
                &self.evaluation_build_config,
            )?),
            self.layout.clone(),
        )
    }

    pub fn backend_id(&self) -> &str {
        self.backend_id.as_str()
    }

    pub fn backend_kind(&self) -> Option<RenderBackendKind> {
        RenderBackendKind::from_str(self.backend_id())
    }

    pub fn gain_model_kind(&self) -> GainModelKind {
        backend_descriptor_by_id(self.backend_id())
            .map(|descriptor| descriptor.gain_model_kind)
            .unwrap_or(GainModelKind::Vbap)
    }

    pub fn evaluation_mode(&self) -> LiveEvaluationMode {
        self.evaluation_mode
    }

    pub fn layout(&self) -> &SpeakerLayout {
        &self.layout
    }

    pub fn log_summary(&self) -> String {
        match &self.backend_build {
            BackendBuildPlan::Vbap(plan) => format!(
                "gain_model=vbap evaluation_mode={} azimuth_resolution={} elevation_resolution={} distance_res={} distance_max={} mode={:?}",
                self.evaluation_mode().as_str(),
                plan.azimuth_resolution,
                plan.elevation_resolution,
                plan.distance_res,
                plan.distance_max,
                plan.table_mode
            ),
            BackendBuildPlan::ExperimentalDistance(plan) => format!(
                "gain_model=experimental_distance evaluation_mode={} speakers={}",
                self.evaluation_mode().as_str(),
                plan.speaker_positions.len()
            ),
            BackendBuildPlan::Barycenter(plan) => format!(
                "gain_model=barycenter evaluation_mode={} speakers={}",
                self.evaluation_mode().as_str(),
                plan.speaker_positions.len()
            ),
        }
    }
}

fn effective_live_evaluation_mode(
    requested: LiveEvaluationMode,
    preferred: PreferredEvaluationMode,
) -> LiveEvaluationMode {
    match requested {
        LiveEvaluationMode::Auto => match preferred {
            PreferredEvaluationMode::PrecomputedPolar => LiveEvaluationMode::PrecomputedPolar,
            PreferredEvaluationMode::PrecomputedCartesian => {
                LiveEvaluationMode::PrecomputedCartesian
            }
        },
        mode => mode,
    }
}

pub fn prepare_topology_build_plan(
    layout: SpeakerLayout,
    live: &LiveParams,
    backend_rebuild_params: Option<BackendRebuildParams>,
    evaluation_build_config: crate::render_backend::EvaluationBuildConfig,
) -> Option<TopologyBuildPlan> {
    match live.backend_id() {
        "barycenter" => {
            let speaker_positions = layout
                .speakers
                .iter()
                .filter(|speaker| speaker.spatialize)
                .map(|speaker| [speaker.x, speaker.y, speaker.z])
                .collect();
            let preferred = backend_rebuild_params
                .map(|params| params.preferred_evaluation_mode())
                .unwrap_or(PreferredEvaluationMode::PrecomputedCartesian);
            Some(TopologyBuildPlan {
                layout,
                backend_id: live.backend_id().to_string(),
                backend_build: BackendBuildPlan::Barycenter(BarycenterBuildPlan {
                    speaker_positions,
                }),
                evaluation_mode: effective_live_evaluation_mode(live.evaluation.mode, preferred),
                evaluation_build_config,
            })
        }
        "experimental_distance" => {
            let speaker_positions = layout
                .speakers
                .iter()
                .filter(|speaker| speaker.spatialize)
                .map(|speaker| [speaker.x, speaker.y, speaker.z])
                .collect();
            let preferred = backend_rebuild_params
                .map(|params| params.preferred_evaluation_mode())
                .unwrap_or(PreferredEvaluationMode::PrecomputedCartesian);
            Some(TopologyBuildPlan {
                layout,
                backend_id: live.backend_id().to_string(),
                backend_build: BackendBuildPlan::ExperimentalDistance(
                    ExperimentalDistanceBuildPlan { speaker_positions },
                ),
                evaluation_mode: effective_live_evaluation_mode(live.evaluation.mode, preferred),
                evaluation_build_config,
            })
        }
        "vbap" => {
            let rebuild_params = backend_rebuild_params?;
            let rebuild = rebuild_params.vbap?;
            let positions = layout
                .spatializable_positions_for_room(
                    live.room_ratio,
                    live.room_ratio_rear,
                    live.room_ratio_lower,
                    live.room_ratio_center_blend,
                )
                .0;
            let effective_mode = effective_live_evaluation_mode(
                live.evaluation.mode,
                rebuild_params.preferred_evaluation_mode(),
            );
            let table_mode = match effective_mode {
                LiveEvaluationMode::Realtime => rebuild.table_mode,
                LiveEvaluationMode::PrecomputedPolar => VbapTableMode::Polar,
                LiveEvaluationMode::PrecomputedCartesian => VbapTableMode::Cartesian {
                    x_size: live
                        .evaluation
                        .cartesian
                        .x_size
                        .max(rebuild.cartesian_default_x_size)
                        .max(1)
                        + 1,
                    y_size: live
                        .evaluation
                        .cartesian
                        .y_size
                        .max(rebuild.cartesian_default_y_size)
                        .max(1)
                        + 1,
                    z_size: live
                        .evaluation
                        .cartesian
                        .z_size
                        .max(rebuild.cartesian_default_z_size)
                        .max(1)
                        + 1,
                    z_neg_size: live
                        .evaluation
                        .cartesian
                        .z_neg_size
                        .max(rebuild.cartesian_default_z_neg_size),
                },
                LiveEvaluationMode::Auto => {
                    unreachable!("evaluation mode must be resolved before building")
                }
            };
            let azimuth_resolution = if live.evaluation.polar.azimuth_values > 0 {
                ((360.0f32 / (live.evaluation.polar.azimuth_values as f32)).round() as i32)
                    .clamp(1, 360)
            } else {
                rebuild.az_res_deg.clamp(1, 360)
            };
            let elevation_resolution = if live.evaluation.polar.elevation_values > 0 {
                (((if rebuild.allow_negative_z {
                    180.0
                } else {
                    90.0
                }) / (live.evaluation.polar.elevation_values as f32))
                    .round() as i32)
                    .clamp(1, if rebuild.allow_negative_z { 180 } else { 90 })
            } else {
                rebuild
                    .el_res_deg
                    .clamp(1, if rebuild.allow_negative_z { 180 } else { 90 })
            };
            let distance_max = if live.evaluation.polar.distance_max > 0.0 {
                live.evaluation.polar.distance_max
            } else {
                rebuild.distance_max.max(0.01)
            };
            let distance_res = if live.evaluation.polar.distance_res > 0 {
                distance_max / (live.evaluation.polar.distance_res as f32)
            } else if rebuild.spread_resolution > 0.0 {
                rebuild.spread_resolution
            } else {
                0.25
            };

            Some(TopologyBuildPlan {
                layout: layout.clone(),
                backend_id: rebuild_params.backend_id.to_string(),
                backend_build: BackendBuildPlan::Vbap(VbapTopologyBuildPlan {
                    layout,
                    positions,
                    azimuth_resolution,
                    elevation_resolution,
                    distance_res,
                    distance_max,
                    position_interpolation: live.evaluation.position_interpolation,
                    table_mode,
                    allow_negative_z: rebuild.allow_negative_z,
                    distance_model: live.distance_model,
                    spread_min: live.spread_min,
                    spread_max: live.spread_max,
                    spread_from_distance: live.spread_from_distance,
                    spread_distance_range: live.spread_distance_range,
                    spread_distance_curve: live.spread_distance_curve,
                    room_ratio: live.room_ratio,
                    room_ratio_rear: live.room_ratio_rear,
                    room_ratio_lower: live.room_ratio_lower,
                    room_ratio_center_blend: live.room_ratio_center_blend,
                    diffuse: live.use_distance_diffuse,
                    diffuse_thr: live.distance_diffuse_threshold,
                    diffuse_curve: live.distance_diffuse_curve,
                }),
                evaluation_mode: effective_mode,
                evaluation_build_config,
            })
        }
        _ => None,
    }
}
