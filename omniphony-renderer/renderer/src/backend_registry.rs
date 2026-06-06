use std::sync::Arc;

use anyhow::Result;
use parking_lot::Mutex;

use crate::live_params::{
    BackendRebuildParams, LiveEvaluationMode, LiveParams, PreferredEvaluationMode, RenderTopology,
};
use crate::render_backend::{
    BlendCurve, EffectiveEvaluationMode, FewSpeakerBackend, GainModel, GainModelKind,
    HybridBackend, RenderBackendKind, ScriptBackend, ScriptParams, backend_descriptor_by_id,
    build_prepared_render_engine, wrap_prepared_engine,
};
use crate::speaker_layout::SpeakerLayout;

#[derive(Clone)]
pub enum BackendBuildPlan {
    Vbap(VbapTopologyBuildPlan),
    /// VBAP for degenerate (1–2 speaker) geometry, where the panner cannot
    /// triangulate. Substituted for `Vbap` by `build_vbap_build_plan` when the
    /// resolved layout has fewer than 3 spatializable speakers.
    FewSpeaker(FewSpeakerBuildPlan),
    Barycenter(BarycenterBuildPlan),
    ExperimentalDistance(ExperimentalDistanceBuildPlan),
    Hybrid(HybridBuildPlan),
    Script(ScriptBuildPlan),
}

impl BackendBuildPlan {
    /// Build the gain model for this plan as a realtime model. Used both when a
    /// backend is the top-level model and when it is an inner model of the
    /// hybrid backend (which queries `compute_gains` directly).
    pub fn build_gain_model(&self) -> Result<Box<dyn GainModel>> {
        match self {
            BackendBuildPlan::Vbap(plan) => plan.build_gain_model(LiveEvaluationMode::Realtime),
            BackendBuildPlan::FewSpeaker(plan) => plan.build_gain_model(),
            BackendBuildPlan::Barycenter(plan) => plan.build_gain_model(),
            BackendBuildPlan::ExperimentalDistance(plan) => plan.build_gain_model(),
            BackendBuildPlan::Hybrid(plan) => plan.build_gain_model(),
            BackendBuildPlan::Script(plan) => plan.build_gain_model(),
        }
    }

    /// Check for an error that could only surface while the table was being
    /// sampled (currently: a Lua runtime error in the scriptable backend that
    /// triggers at some grid positions). No-op for native backends.
    pub fn post_build_check(&self) -> Result<()> {
        match self {
            BackendBuildPlan::Script(plan) => plan.post_build_check(),
            _ => Ok(()),
        }
    }
}

#[derive(Clone)]
pub struct FewSpeakerBuildPlan {
    /// Speaker `[azimuth, elevation]` in degrees (room-adjusted), 1 or 2 entries.
    pub positions: Vec<[f32; 2]>,
}

impl FewSpeakerBuildPlan {
    pub fn build_gain_model(&self) -> Result<Box<dyn GainModel>> {
        Ok(Box::new(FewSpeakerBackend::new(self.positions.clone())))
    }
}

#[derive(Clone)]
pub struct HybridBuildPlan {
    pub external: Box<BackendBuildPlan>,
    pub internal: Box<BackendBuildPlan>,
    pub curve: Vec<[f32; 2]>,
    pub curve_smoothing: f32,
    pub metric: crate::spatial_vbap::DistanceMetric,
}

impl HybridBuildPlan {
    pub fn build_gain_model(&self) -> Result<Box<dyn GainModel>> {
        let external = self.external.build_gain_model()?;
        let internal = self.internal.build_gain_model()?;
        Ok(Box::new(HybridBackend::new(
            external,
            internal,
            BlendCurve::new(self.curve.clone(), self.curve_smoothing),
            self.metric,
        )))
    }
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
pub struct ScriptBuildPlan {
    pub speaker_positions: Vec<[f32; 3]>,
    /// Lua source for the user backend.
    pub source: String,
    /// Numeric parameters exposed to the script as a Lua table.
    pub params: ScriptParams,
    /// Error slot shared with the constructed [`ScriptBackend`], so a failure
    /// that only manifests during sampling can be surfaced by
    /// [`ScriptBuildPlan::post_build_check`] after the table is built.
    pub error: Arc<Mutex<Option<String>>>,
}

impl ScriptBuildPlan {
    pub fn build_gain_model(&self) -> Result<Box<dyn GainModel>> {
        *self.error.lock() = None;
        let backend = ScriptBackend::with_error_slot(
            self.source.as_str(),
            self.speaker_positions.clone(),
            self.params.clone(),
            Arc::clone(&self.error),
        )?;
        Ok(Box::new(backend))
    }

    pub fn post_build_check(&self) -> Result<()> {
        if let Some(message) = self.error.lock().take() {
            anyhow::bail!("{message}");
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct VbapTopologyBuildPlan {
    pub layout: SpeakerLayout,
    pub positions: Vec<[f32; 2]>,
    pub azimuth_resolution: i32,
    pub elevation_resolution: i32,
    pub distance_res: f32,
    pub distance_max: f32,
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
        // The panner is geometry-only: it computes gains directly per position and
        // owns no table, so the evaluation mode does not affect how it is built.
        // Any precomputation happens in the evaluation layer that samples it.
        let vbap = crate::spatial_vbap::VbapPanner::new(
            &self.positions,
            self.azimuth_resolution,
            self.elevation_resolution,
            0.0,
        )
        .map_err(|e| anyhow::anyhow!("Failed to create VBAP panner: {}", e))?
        .with_negative_z(self.allow_negative_z);

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
    /// The geometry generation captured when this plan was prepared. The built
    /// topology records it; a later recompute compares to decide whether the gain
    /// models can be reused (see `build_topology_reusing`). Set by
    /// `RendererControl::prepare_topology_rebuild_for_layout`.
    pub geometry_generation: u64,
}

impl TopologyBuildPlan {
    pub fn build_topology(&self) -> Result<RenderTopology> {
        self.build_topology_reusing(None)
    }

    /// Build the topology, reusing `current`'s decorated gain model when the
    /// geometry generation is unchanged (only the evaluation mode / grid changed).
    /// Reuse skips re-triangulation: realtime just re-wraps the model, precomputed
    /// re-samples it. A geometry change (different generation, or no current model)
    /// falls back to a full rebuild.
    pub fn build_topology_reusing(
        &self,
        current: Option<&RenderTopology>,
    ) -> Result<RenderTopology> {
        let effective_mode = match self.evaluation_mode {
            LiveEvaluationMode::Realtime => EffectiveEvaluationMode::Realtime,
            LiveEvaluationMode::PrecomputedPolar => EffectiveEvaluationMode::PrecomputedPolar,
            LiveEvaluationMode::PrecomputedCartesian => {
                EffectiveEvaluationMode::PrecomputedCartesian
            }
            LiveEvaluationMode::Auto => unreachable!("topology build plan must resolve auto mode"),
        };

        if let Some(model) = current.and_then(|cur| {
            (cur.geometry_generation == self.geometry_generation)
                .then(|| cur.backend.decorated_model())
                .flatten()
        }) {
            let engine =
                wrap_prepared_engine(model, effective_mode, &self.evaluation_build_config)?;
            return Ok(RenderTopology::new(Arc::new(engine), self.layout.clone())?
                .with_geometry_generation(self.geometry_generation));
        }

        // The panner is geometry-only and ignores the evaluation mode, so the
        // shared realtime builder applies to every backend (the mode is resolved
        // later by `build_prepared_render_engine`'s evaluation wrapper).
        let model = self.backend_build.build_gain_model()?;
        let engine =
            build_prepared_render_engine(model, effective_mode, &self.evaluation_build_config)?;
        // A scriptable backend can fail only at some sampled positions; the
        // table is built by now, so convert any recorded error into a build
        // failure (surfaced to Studio via the recompute error broadcast).
        self.backend_build.post_build_check()?;
        Ok(RenderTopology::new(Arc::new(engine), self.layout.clone())?
            .with_geometry_generation(self.geometry_generation))
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
                "gain_model=vbap evaluation_mode={} azimuth_resolution={} elevation_resolution={} distance_res={} distance_max={}",
                self.evaluation_mode().as_str(),
                plan.azimuth_resolution,
                plan.elevation_resolution,
                plan.distance_res,
                plan.distance_max,
            ),
            BackendBuildPlan::FewSpeaker(plan) => format!(
                "gain_model=vbap(few-speaker) evaluation_mode={} speakers={}",
                self.evaluation_mode().as_str(),
                plan.positions.len()
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
            BackendBuildPlan::Hybrid(plan) => format!(
                "gain_model=hybrid evaluation_mode={} external={} internal={} curve_points={}",
                self.evaluation_mode().as_str(),
                inner_backend_summary(&plan.external),
                inner_backend_summary(&plan.internal),
                plan.curve.len()
            ),
            BackendBuildPlan::Script(plan) => format!(
                "gain_model=script evaluation_mode={} speakers={} params={}",
                self.evaluation_mode().as_str(),
                plan.speaker_positions.len(),
                plan.params.0.len()
            ),
        }
    }
}

fn inner_backend_summary(plan: &BackendBuildPlan) -> &'static str {
    match plan {
        BackendBuildPlan::Vbap(_) => "vbap",
        BackendBuildPlan::FewSpeaker(_) => "vbap",
        BackendBuildPlan::Barycenter(_) => "barycenter",
        BackendBuildPlan::ExperimentalDistance(_) => "experimental_distance",
        BackendBuildPlan::Hybrid(_) => "hybrid",
        BackendBuildPlan::Script(_) => "script",
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

/// Evaluation mode for the script backend, which has `supports_realtime =
/// false`: an `Auto` or `Realtime` request is forced to a precomputed mode
/// (the script runs only while the table is sampled, never per audio sample).
/// An explicitly chosen precomputed mode is respected as-is.
fn resolve_script_evaluation_mode(
    requested: LiveEvaluationMode,
    preferred: PreferredEvaluationMode,
) -> LiveEvaluationMode {
    match effective_live_evaluation_mode(requested, preferred) {
        LiveEvaluationMode::Realtime => match preferred {
            PreferredEvaluationMode::PrecomputedCartesian => {
                LiveEvaluationMode::PrecomputedCartesian
            }
            PreferredEvaluationMode::PrecomputedPolar => LiveEvaluationMode::PrecomputedPolar,
        },
        resolved => resolved,
    }
}

fn collect_spatializable_positions(layout: &SpeakerLayout) -> Vec<[f32; 3]> {
    layout
        .speakers
        .iter()
        .filter(|speaker| speaker.spatialize)
        .map(|speaker| [speaker.x, speaker.y, speaker.z])
        .collect()
}

/// Build the VBAP build plan for the given (already resolved) evaluation mode.
/// Shared by the top-level VBAP backend and by hybrid inner models (which pass
/// `Realtime`, since the hybrid backend queries `compute_gains` directly).
fn build_vbap_build_plan(
    layout: &SpeakerLayout,
    live: &LiveParams,
    rebuild_params: BackendRebuildParams,
) -> Option<BackendBuildPlan> {
    let rebuild = rebuild_params.vbap?;
    let positions = layout
        .spatializable_positions_for_room(
            live.room_ratio,
            live.room_ratio_rear,
            live.room_ratio_lower,
            live.room_ratio_center_blend,
        )
        .0;
    let azimuth_resolution = if live.evaluation.polar.azimuth_values > 0 {
        ((360.0f32 / (live.evaluation.polar.azimuth_values as f32)).round() as i32).clamp(1, 360)
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

    // Fewer than 3 spatializable speakers can't be triangulated: pan them with
    // the degenerate-VBAP backend (same direction-only model) instead.
    if positions.len() < 3 {
        return Some(BackendBuildPlan::FewSpeaker(FewSpeakerBuildPlan {
            positions,
        }));
    }

    Some(BackendBuildPlan::Vbap(VbapTopologyBuildPlan {
        layout: layout.clone(),
        positions,
        azimuth_resolution,
        elevation_resolution,
        distance_res,
        distance_max,
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
    }))
}

/// Build a `BackendBuildPlan` for one of the concrete (non-hybrid) backends.
/// Used directly by the top-level barycenter/experimental_distance branches and
/// by the hybrid backend for each of its inner models. Returns `None` for an
/// unknown id or `"hybrid"` (no nested hybrids).
fn build_inner_backend_plan(
    backend_id: &str,
    layout: &SpeakerLayout,
    live: &LiveParams,
    backend_rebuild_params: Option<BackendRebuildParams>,
) -> Option<BackendBuildPlan> {
    match backend_id {
        "barycenter" => Some(BackendBuildPlan::Barycenter(BarycenterBuildPlan {
            speaker_positions: collect_spatializable_positions(layout),
        })),
        "experimental_distance" => Some(BackendBuildPlan::ExperimentalDistance(
            ExperimentalDistanceBuildPlan {
                speaker_positions: collect_spatializable_positions(layout),
            },
        )),
        "vbap" => {
            let rebuild_params = backend_rebuild_params?;
            build_vbap_build_plan(layout, live, rebuild_params)
        }
        _ => None,
    }
}

fn preferred_evaluation_mode(
    backend_rebuild_params: Option<BackendRebuildParams>,
) -> PreferredEvaluationMode {
    backend_rebuild_params
        .map(|params| params.preferred_evaluation_mode())
        .unwrap_or(PreferredEvaluationMode::PrecomputedCartesian)
}

pub fn prepare_topology_build_plan(
    layout: SpeakerLayout,
    live: &LiveParams,
    backend_rebuild_params: Option<BackendRebuildParams>,
    evaluation_build_config: crate::render_backend::EvaluationBuildConfig,
) -> Option<TopologyBuildPlan> {
    match live.backend_id() {
        "barycenter" | "experimental_distance" => {
            let backend_build =
                build_inner_backend_plan(live.backend_id(), &layout, live, backend_rebuild_params)?;
            let preferred = preferred_evaluation_mode(backend_rebuild_params);
            Some(TopologyBuildPlan {
                layout,
                backend_id: live.backend_id().to_string(),
                backend_build,
                evaluation_mode: effective_live_evaluation_mode(live.evaluation.mode, preferred),
                evaluation_build_config,
                geometry_generation: 0,
            })
        }
        "hybrid" => {
            let external = build_inner_backend_plan(
                &live.hybrid.external_backend_id,
                &layout,
                live,
                backend_rebuild_params,
            )?;
            let internal = build_inner_backend_plan(
                &live.hybrid.internal_backend_id,
                &layout,
                live,
                backend_rebuild_params,
            )?;
            let preferred = preferred_evaluation_mode(backend_rebuild_params);
            Some(TopologyBuildPlan {
                layout,
                backend_id: live.backend_id().to_string(),
                backend_build: BackendBuildPlan::Hybrid(HybridBuildPlan {
                    external: Box::new(external),
                    internal: Box::new(internal),
                    curve: live.hybrid.curve.clone(),
                    curve_smoothing: live.hybrid.curve_smoothing,
                    metric: live.hybrid.metric,
                }),
                evaluation_mode: effective_live_evaluation_mode(live.evaluation.mode, preferred),
                evaluation_build_config,
                geometry_generation: 0,
            })
        }
        "vbap" => {
            let rebuild_params = backend_rebuild_params?;
            let effective_mode = effective_live_evaluation_mode(
                live.evaluation.mode,
                rebuild_params.preferred_evaluation_mode(),
            );
            let backend_build = build_vbap_build_plan(&layout, live, rebuild_params)?;
            Some(TopologyBuildPlan {
                layout,
                backend_id: live.backend_id().to_string(),
                backend_build,
                evaluation_mode: effective_mode,
                evaluation_build_config,
                geometry_generation: 0,
            })
        }
        "script" => {
            // The script backend cannot run per audio sample, so realtime is
            // never honoured: any Auto/Realtime request is forced to a
            // precomputed mode (the script then runs only at table-build time).
            let preferred = preferred_evaluation_mode(backend_rebuild_params);
            let evaluation_mode = resolve_script_evaluation_mode(live.evaluation.mode, preferred);
            let speaker_positions = collect_spatializable_positions(&layout);
            Some(TopologyBuildPlan {
                backend_id: live.backend_id().to_string(),
                backend_build: BackendBuildPlan::Script(ScriptBuildPlan {
                    speaker_positions,
                    source: live.script.source.clone(),
                    params: ScriptParams(live.script.params.clone()),
                    error: Arc::new(Mutex::new(None)),
                }),
                evaluation_mode,
                evaluation_build_config,
                geometry_generation: 0,
                layout,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_realtime_is_forced_to_a_precomputed_mode() {
        // Realtime is never honoured for the script backend.
        assert_eq!(
            resolve_script_evaluation_mode(
                LiveEvaluationMode::Realtime,
                PreferredEvaluationMode::PrecomputedPolar
            ),
            LiveEvaluationMode::PrecomputedPolar
        );
        assert_eq!(
            resolve_script_evaluation_mode(
                LiveEvaluationMode::Realtime,
                PreferredEvaluationMode::PrecomputedCartesian
            ),
            LiveEvaluationMode::PrecomputedCartesian
        );
    }

    #[test]
    fn script_auto_follows_the_preferred_precomputed_mode() {
        assert_eq!(
            resolve_script_evaluation_mode(
                LiveEvaluationMode::Auto,
                PreferredEvaluationMode::PrecomputedCartesian
            ),
            LiveEvaluationMode::PrecomputedCartesian
        );
        assert_eq!(
            resolve_script_evaluation_mode(
                LiveEvaluationMode::Auto,
                PreferredEvaluationMode::PrecomputedPolar
            ),
            LiveEvaluationMode::PrecomputedPolar
        );
    }

    #[test]
    fn script_explicit_precomputed_mode_is_respected() {
        // An explicitly chosen precomputed mode is left untouched, regardless of
        // the preferred fallback.
        assert_eq!(
            resolve_script_evaluation_mode(
                LiveEvaluationMode::PrecomputedPolar,
                PreferredEvaluationMode::PrecomputedCartesian
            ),
            LiveEvaluationMode::PrecomputedPolar
        );
        assert_eq!(
            resolve_script_evaluation_mode(
                LiveEvaluationMode::PrecomputedCartesian,
                PreferredEvaluationMode::PrecomputedPolar
            ),
            LiveEvaluationMode::PrecomputedCartesian
        );
    }
}
