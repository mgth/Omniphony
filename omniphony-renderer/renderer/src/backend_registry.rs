use std::sync::Arc;

use anyhow::Result;

use crate::live_params::{
    BackendRebuildParams, LiveEvaluationMode, LiveParams, PreferredEvaluationMode, RenderTopology,
};
use crate::render_backend::{
    BlendCurve, DegenerateVbapBackend, EffectiveEvaluationMode, EvaluationBuildConfig, GainModel,
    HybridBackend, PreparedRenderEngine, build_prepared_render_engine, wrap_prepared_engine,
};
use crate::speaker_layout::SpeakerLayout;

/// Reference positions probed by [`smoke_test_engine`]: scene centre, the eight
/// cube corners, and a few off-axis points. They are intentionally cheap and
/// fixed — the goal is to exercise a freshly built backend once, on the build
/// thread, not to characterise it.
const SMOKE_TEST_POSITIONS: [[f64; 3]; 11] = [
    [0.0, 0.0, 0.0],
    [1.0, 1.0, 1.0],
    [-1.0, -1.0, -1.0],
    [1.0, -1.0, 1.0],
    [-1.0, 1.0, -1.0],
    [1.0, 1.0, -1.0],
    [-1.0, -1.0, 1.0],
    [1.0, 0.0, 0.0],
    [0.0, 1.0, 0.0],
    [0.0, 0.0, 1.0],
    [0.5, -0.5, 0.25],
];

/// Extract a human-readable message from a `catch_unwind` panic payload.
fn panic_detail(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(msg) = payload.downcast_ref::<&'static str>() {
        (*msg).to_string()
    } else if let Some(msg) = payload.downcast_ref::<String>() {
        msg.clone()
    } else {
        "panic with non-string payload".to_string()
    }
}

/// Exercise a freshly built engine on a handful of reference positions, on the
/// build thread, so a misbehaving backend is rejected here instead of taking
/// down the realtime audio thread on its first frame.
///
/// This is the build-time guard behind the [`GainModel`] hot-path contract: a
/// contributor backend that panics, returns the wrong number of gains, or emits
/// a non-finite gain turns into a plain `Err` from topology construction (which
/// the OSC recompute path already surfaces to Studio), never an uncaught panic
/// in `SpatialRenderer::render_frame`.
fn smoke_test_engine(
    engine: &PreparedRenderEngine,
    config: &EvaluationBuildConfig,
    backend_id: &str,
) -> Result<()> {
    let expected = engine.speaker_count();
    for position in SMOKE_TEST_POSITIONS {
        let mut request = config.request_template;
        request.adm_position = position;

        let response = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            engine.compute_gains(&request)
        }))
        .map_err(|payload| {
            anyhow::anyhow!(
                "backend '{backend_id}' panicked in compute_gains at position {position:?}: {}",
                panic_detail(payload.as_ref())
            )
        })?;

        if response.gains.len() != expected {
            return Err(anyhow::anyhow!(
                "backend '{backend_id}' returned {} gains at position {position:?}, expected {expected}",
                response.gains.len()
            ));
        }
        if let Some((speaker, gain)) = response
            .gains
            .iter()
            .enumerate()
            .find(|(_, gain)| !gain.is_finite())
        {
            return Err(anyhow::anyhow!(
                "backend '{backend_id}' returned non-finite gain {gain} for speaker {speaker} at position {position:?}"
            ));
        }
    }
    Ok(())
}

/// A backend plan whose gain model is produced by an opaque builder closure
/// rather than one of the built-in typed variants. This is how an out-of-tree
/// backend (one whose `BackendFactory` lives in another crate) plugs into the
/// build pipeline: the factory captures whatever it needs from the build context
/// into the closure. Cloneable (the closure is shared via `Arc`) so it can ride
/// inside a `TopologyBuildPlan` like the typed variants.
#[derive(Clone)]
pub struct DynamicBackendPlan {
    id: &'static str,
    builder: Arc<dyn Fn() -> Result<Box<dyn GainModel>> + Send + Sync>,
}

impl DynamicBackendPlan {
    pub fn new(
        id: &'static str,
        builder: impl Fn() -> Result<Box<dyn GainModel>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            id,
            builder: Arc::new(builder),
        }
    }

    pub fn id(&self) -> &'static str {
        self.id
    }
}

#[derive(Clone)]
pub enum BackendBuildPlan {
    Vbap(VbapTopologyBuildPlan),
    /// Triangulation-free VBAP for degenerate geometry, where the panner cannot
    /// triangulate. Substituted for `Vbap` by `build_vbap_build_plan` when the
    /// resolved layout has fewer than 3 spatializable speakers, and by
    /// `VbapTopologyBuildPlan::build_gain_model` as a fallback when the full panner
    /// build fails on a ≥3 but still-degenerate set.
    Degenerate(DegenerateVbapBuildPlan),
    Barycenter(BarycenterBuildPlan),
    ExperimentalDistance(ExperimentalDistanceBuildPlan),
    Hybrid(HybridBuildPlan),
    /// A backend supplied by a registered [`BackendFactory`] that is not one of
    /// the built-in variants (e.g. a contributor backend in its own crate).
    Dynamic(DynamicBackendPlan),
}

impl BackendBuildPlan {
    /// Build the gain model for this plan as a realtime model. Used both when a
    /// backend is the top-level model and when it is an inner model of the
    /// hybrid backend (which queries `compute_gains` directly).
    pub fn build_gain_model(&self) -> Result<Box<dyn GainModel>> {
        match self {
            BackendBuildPlan::Vbap(plan) => plan.build_gain_model(LiveEvaluationMode::Realtime),
            BackendBuildPlan::Degenerate(plan) => plan.build_gain_model(),
            BackendBuildPlan::Barycenter(plan) => plan.build_gain_model(),
            BackendBuildPlan::ExperimentalDistance(plan) => plan.build_gain_model(),
            BackendBuildPlan::Hybrid(plan) => plan.build_gain_model(),
            BackendBuildPlan::Dynamic(plan) => (plan.builder)(),
        }
    }
}

#[derive(Clone)]
pub struct DegenerateVbapBuildPlan {
    /// Speaker `[azimuth, elevation]` in degrees (room-adjusted).
    pub positions: Vec<[f32; 2]>,
    /// `omni[i]` marks speakers at the listener (no usable direction, e.g. a
    /// spatialised LFE) that take a constant baseline share. Same length / order
    /// as `positions`.
    pub omni: Vec<bool>,
}

impl DegenerateVbapBuildPlan {
    pub fn build_gain_model(&self) -> Result<Box<dyn GainModel>> {
        Ok(Box::new(DegenerateVbapBackend::with_omni(
            self.positions.clone(),
            self.omni.clone(),
        )))
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
    /// Tuning params, baked into the model at build (no longer per-request).
    /// Sourced from the param bag (key = backend id); changing it triggers a rebuild.
    pub params: crate::live_params::ExperimentalDistanceLiveParams,
}

#[derive(Clone)]
pub struct BarycenterBuildPlan {
    pub speaker_positions: Vec<[f32; 3]>,
    /// Localisation bias, baked into the model at build (no longer a per-request
    /// field). Sourced from the param bag (key = backend id); rebuild on change.
    pub localize: f32,
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
    pub size_to_spread_mode: crate::render_backend::SizeToSpreadMode,
    pub room_ratio: [f32; 3],
    pub room_ratio_rear: f32,
    pub room_ratio_lower: f32,
    pub room_ratio_center_blend: f32,
    pub diffuse: bool,
    pub diffuse_thr: f32,
    pub diffuse_curve: f32,
    /// Out-of-hull rendering mode, baked into the panner at build (it shapes
    /// the triangulation in `VirtualPoles`). Sourced from the live options;
    /// a change rebuilds the topology (`OptionFlags::REBUILD`).
    pub out_of_hull_mode: crate::spatial_vbap::OutOfHullMode,
}

impl VbapTopologyBuildPlan {
    pub fn build_gain_model(
        &self,
        _evaluation_mode: LiveEvaluationMode,
    ) -> Result<Box<dyn GainModel>> {
        // The panner is geometry-only: it computes gains directly per position and
        // owns no table, so the evaluation mode does not affect how it is built.
        // Any precomputation happens in the evaluation layer that samples it.
        let vbap = match crate::spatial_vbap::VbapPanner::new(
            &self.positions,
            self.azimuth_resolution,
            self.elevation_resolution,
            0.0,
            self.out_of_hull_mode,
        ) {
            Ok(panner) => panner.with_negative_z(self.allow_negative_z),
            // Degenerate geometry (collinear/coplanar, or a speaker at the
            // listener) that can't be triangulated — most often a crossover band
            // that drops below a triangulable speaker set. Don't kill the engine:
            // degrade to the triangulation-free directional pan and warn loudly so
            // it surfaces in the log (stderr + Studio log panel) instead of failing
            // silently with no audio.
            Err(e) => {
                let names: Vec<&str> = self
                    .layout
                    .speakers
                    .iter()
                    .filter(|s| s.spatialize)
                    .map(|s| s.name.as_str())
                    .collect();
                log::warn!(
                    "VBAP triangulation failed for {} spatializable speaker(s) {:?}: {}. \
                     Falling back to degenerate directional pan (no triangulation) — audio \
                     continues, but this layout/band cannot use full VBAP. Check the speaker \
                     geometry (collinear/coplanar speakers, or one placed at the listener).",
                    self.positions.len(),
                    names,
                    e
                );
                return Ok(Box::new(DegenerateVbapBackend::with_omni(
                    self.positions.clone(),
                    collect_omni_mask(&self.layout),
                )));
            }
        };

        Ok(Box::new(crate::render_backend::VbapBackend::new(
            vbap,
            crate::render_backend::VbapSpreadParams {
                spread_min: self.spread_min,
                spread_max: self.spread_max,
                spread_from_distance: self.spread_from_distance,
                spread_distance_range: self.spread_distance_range,
                spread_distance_curve: self.spread_distance_curve,
                size_to_spread_mode: self.size_to_spread_mode,
            },
        )))
    }
}

impl ExperimentalDistanceBuildPlan {
    pub fn build_gain_model(&self) -> Result<Box<dyn GainModel>> {
        Ok(Box::new(
            crate::render_backend::ExperimentalDistanceBackend::new(
                self.speaker_positions.clone(),
                self.params,
            ),
        ))
    }
}

impl BarycenterBuildPlan {
    pub fn build_gain_model(&self) -> Result<Box<dyn GainModel>> {
        Ok(Box::new(crate::render_backend::BarycenterBackend::new(
            self.speaker_positions.clone(),
            self.localize,
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
            let topology = RenderTopology::new(Arc::new(engine), self.layout.clone())?
                .with_geometry_generation(self.geometry_generation);
            smoke_test_engine(
                &topology.backend,
                &self.evaluation_build_config,
                self.backend_id(),
            )?;
            return Ok(topology);
        }

        // The panner is geometry-only and ignores the evaluation mode, so the
        // shared realtime builder applies to every backend (the mode is resolved
        // later by `build_prepared_render_engine`'s evaluation wrapper).
        let model = self.backend_build.build_gain_model()?;
        let topology = RenderTopology::new(
            Arc::new(build_prepared_render_engine(
                model,
                effective_mode,
                &self.evaluation_build_config,
            )?),
            self.layout.clone(),
        )?
        .with_geometry_generation(self.geometry_generation);
        smoke_test_engine(
            &topology.backend,
            &self.evaluation_build_config,
            self.backend_id(),
        )?;
        Ok(topology)
    }

    pub fn backend_id(&self) -> &str {
        self.backend_id.as_str()
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
            BackendBuildPlan::Degenerate(plan) => format!(
                "gain_model=vbap(degenerate) evaluation_mode={} speakers={}",
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
            BackendBuildPlan::Dynamic(plan) => format!(
                "gain_model={} evaluation_mode={}",
                plan.id(),
                self.evaluation_mode().as_str()
            ),
        }
    }
}

fn inner_backend_summary(plan: &BackendBuildPlan) -> &'static str {
    match plan {
        BackendBuildPlan::Vbap(_) => "vbap",
        BackendBuildPlan::Degenerate(_) => "vbap",
        BackendBuildPlan::Barycenter(_) => "barycenter",
        BackendBuildPlan::ExperimentalDistance(_) => "experimental_distance",
        BackendBuildPlan::Hybrid(_) => "hybrid",
        BackendBuildPlan::Dynamic(plan) => plan.id(),
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

fn collect_spatializable_positions(layout: &SpeakerLayout) -> Vec<[f32; 3]> {
    layout
        .speakers
        .iter()
        .filter(|speaker| speaker.spatialize)
        .map(|speaker| [speaker.x, speaker.y, speaker.z])
        .collect()
}

/// Normalised magnitude below which a spatializable speaker is treated as sitting
/// at the listener (no usable direction), e.g. a spatialised LFE near the origin.
/// Such speakers get a constant baseline share in the degenerate backend instead
/// of being panned. Same `spatialize` filter / order as the panner positions.
pub(crate) const ORIGIN_DIR_EPS: f32 = 0.05;

pub(crate) fn collect_omni_mask(layout: &SpeakerLayout) -> Vec<bool> {
    layout
        .speakers
        .iter()
        .filter(|speaker| speaker.spatialize)
        .map(|speaker| {
            (speaker.x * speaker.x + speaker.y * speaker.y + speaker.z * speaker.z).sqrt()
                < ORIGIN_DIR_EPS
        })
        .collect()
}

/// Build the VBAP build plan for the given (already resolved) evaluation mode.
/// Shared by the top-level VBAP backend and by hybrid inner models (which pass
/// `Realtime`, since the hybrid backend queries `compute_gains` directly).
fn build_vbap_build_plan(
    layout: &SpeakerLayout,
    live: &LiveParams,
    rebuild_params: BackendRebuildParams,
    spread: crate::render_backend::VbapSpreadParams,
    out_of_hull_mode: crate::spatial_vbap::OutOfHullMode,
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
    // the degenerate-VBAP backend (same direction-only model) instead. Larger but
    // still-degenerate sets are caught at build time in
    // `VbapTopologyBuildPlan::build_gain_model`.
    if positions.len() < 3 {
        return Some(BackendBuildPlan::Degenerate(DegenerateVbapBuildPlan {
            positions,
            omni: collect_omni_mask(layout),
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
        spread_min: spread.spread_min,
        spread_max: spread.spread_max,
        spread_from_distance: spread.spread_from_distance,
        spread_distance_range: spread.spread_distance_range,
        spread_distance_curve: spread.spread_distance_curve,
        size_to_spread_mode: spread.size_to_spread_mode,
        room_ratio: live.room_ratio,
        room_ratio_rear: live.room_ratio_rear,
        room_ratio_lower: live.room_ratio_lower,
        room_ratio_center_blend: live.room_ratio_center_blend,
        diffuse: live.use_distance_diffuse,
        diffuse_thr: live.distance_diffuse_threshold,
        diffuse_curve: live.distance_diffuse_curve,
        out_of_hull_mode,
    }))
}

/// Out-of-hull rendering mode, read from the param bag under `backend_id`
/// (same generic path as every other backend param — persisted in
/// `render.backend_params`, set over `/omniphony/control/backend/param`, read
/// at build time only). Missing/invalid keys fall back to the schema defaults.
fn vbap_out_of_hull_mode(
    ctx: &BackendBuildCtx<'_>,
    backend_id: &str,
) -> crate::spatial_vbap::OutOfHullMode {
    use crate::backend_params::ParamValue;
    parse_out_of_hull_mode(
        ctx.backend_param(backend_id, "out_of_hull_mode")
            .and_then(ParamValue::as_str),
        ctx.backend_param(backend_id, "fold_blend_power")
            .and_then(ParamValue::as_f32),
    )
}

/// Resolve the raw bag values into the DSP-facing mode. Unknown mode spellings
/// and non-finite powers fall back to the schema defaults; the power is
/// clamped to the schema bounds like every numeric bag param.
fn parse_out_of_hull_mode(
    mode: Option<&str>,
    power: Option<f32>,
) -> crate::spatial_vbap::OutOfHullMode {
    use crate::spatial_vbap::OutOfHullMode;
    match mode
        .unwrap_or("virtual_poles")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "blend" | "fold" => OutOfHullMode::Blend {
            power: power
                .filter(|p| p.is_finite())
                .unwrap_or(OutOfHullMode::DEFAULT_BLEND_POWER)
                .clamp(1.0, 64.0),
        },
        "fade" | "original" | "legacy" => OutOfHullMode::Fade,
        _ => OutOfHullMode::VirtualPoles,
    }
}

/// VBAP spread tuning, read from the param bag under `backend_id`, with each
/// missing key falling back to the corresponding live value (the config / OSC
/// path still seeds `LiveParams` during the transition to the generic bag).
fn vbap_spread_params(
    ctx: &BackendBuildCtx<'_>,
    backend_id: &str,
) -> crate::render_backend::VbapSpreadParams {
    use crate::backend_params::ParamValue;
    let live = ctx.live;
    let float = |key: &str, fallback: f32| {
        ctx.backend_param(backend_id, key)
            .and_then(ParamValue::as_f32)
            .unwrap_or(fallback)
    };
    let from_distance = ctx
        .backend_param(backend_id, "spread_from_distance")
        .and_then(ParamValue::as_bool)
        .unwrap_or(live.spread_from_distance);
    let size_to_spread_mode = ctx
        .backend_param(backend_id, "size_to_spread_mode")
        .and_then(ParamValue::as_str)
        .and_then(crate::render_backend::SizeToSpreadMode::from_str)
        .unwrap_or(live.size_to_spread_mode);
    crate::render_backend::VbapSpreadParams {
        spread_min: float("spread_min", live.spread_min),
        spread_max: float("spread_max", live.spread_max),
        spread_from_distance: from_distance,
        spread_distance_range: float("spread_distance_range", live.spread_distance_range),
        spread_distance_curve: float("spread_distance_curve", live.spread_distance_curve),
        size_to_spread_mode,
    }
}

/// Barycenter `localize`, read from the param bag under `backend_id` (so it
/// works for the standalone backend and for a barycenter nested in hybrid),
/// falling back to the model default.
fn barycenter_localize(ctx: &BackendBuildCtx<'_>, backend_id: &str) -> f32 {
    ctx.backend_param(backend_id, "localize")
        .and_then(crate::backend_params::ParamValue::as_f32)
        .unwrap_or_else(|| crate::live_params::BarycenterLiveParams::default().localize)
}

/// The experimental_distance tuning params, read from the param bag under
/// `backend_id` with each missing key falling back to the model default.
fn experimental_params(
    ctx: &BackendBuildCtx<'_>,
    backend_id: &str,
) -> crate::live_params::ExperimentalDistanceLiveParams {
    use crate::backend_params::ParamValue;
    let defaults = crate::live_params::ExperimentalDistanceLiveParams::default();
    let float = |key: &str, default: f32| {
        ctx.backend_param(backend_id, key)
            .and_then(ParamValue::as_f32)
            .unwrap_or(default)
    };
    let count = |key: &str, default: usize| {
        ctx.backend_param(backend_id, key)
            .and_then(ParamValue::as_i64)
            .map(|v| v.max(1) as usize)
            .unwrap_or(default)
    };
    let min_active_speakers = count("min_active_speakers", defaults.min_active_speakers);
    crate::live_params::ExperimentalDistanceLiveParams {
        distance_floor: float("distance_floor", defaults.distance_floor),
        min_active_speakers,
        // max is re-asserted >= min here (the OSC alias no longer clamps on write).
        max_active_speakers: count("max_active_speakers", defaults.max_active_speakers)
            .max(min_active_speakers),
        position_error_floor: float("position_error_floor", defaults.position_error_floor),
        position_error_nearest_scale: float(
            "position_error_nearest_scale",
            defaults.position_error_nearest_scale,
        ),
        position_error_span_scale: float(
            "position_error_span_scale",
            defaults.position_error_span_scale,
        ),
    }
}

/// Build a `BackendBuildPlan` for one of the concrete (non-hybrid) backends.
/// Used by the top-level barycenter/experimental_distance/vbap factories. Tuning
/// params come from the bag keyed by `backend_id`. Returns `None` for an unknown
/// id or `"hybrid"`.
///
/// This deliberately does NOT dispatch through the registry: it is called from
/// the concrete factories' own `build_plan`, so routing back through the registry
/// would recurse. Composite backends use [`resolve_hybrid_inner_plan`] instead.
fn build_inner_backend_plan(
    ctx: &BackendBuildCtx<'_>,
    backend_id: &str,
) -> Option<BackendBuildPlan> {
    match backend_id {
        "barycenter" => Some(BackendBuildPlan::Barycenter(BarycenterBuildPlan {
            speaker_positions: collect_spatializable_positions(ctx.layout),
            localize: barycenter_localize(ctx, backend_id),
        })),
        "experimental_distance" => Some(BackendBuildPlan::ExperimentalDistance(
            ExperimentalDistanceBuildPlan {
                speaker_positions: collect_spatializable_positions(ctx.layout),
                params: experimental_params(ctx, backend_id),
            },
        )),
        "vbap" => build_vbap_build_plan(
            ctx.layout,
            ctx.live,
            ctx.backend_rebuild_params?,
            vbap_spread_params(ctx, backend_id),
            vbap_out_of_hull_mode(ctx, backend_id),
        ),
        _ => None,
    }
}

/// Resolve an inner model of the hybrid backend by id, dispatching through the
/// registry so *any* registered backend (built-in, scriptable, or contributor)
/// can be composed — not just the historical concrete ones. The inner factory's
/// own `build_plan` runs, so e.g. the scriptable backend yields a `Dynamic` plan.
///
/// Nested hybrid is rejected to keep this from recursing indefinitely.
fn resolve_hybrid_inner_plan(
    ctx: &BackendBuildCtx<'_>,
    backend_id: &str,
) -> Option<BackendBuildPlan> {
    if backend_id == "hybrid" {
        return None;
    }
    ctx.registry.get(backend_id)?.build_plan(ctx)
}

/// Whether both named backends are hot-path-safe per the registry. An
/// unregistered id is treated as realtime-capable (it will fail to build
/// elsewhere). Used to force a precomputed evaluation mode when a hybrid inner
/// backend (e.g. the scriptable backend) cannot run per sample.
fn inners_realtime_capable(
    registry: &BackendRegistry,
    external_id: &str,
    internal_id: &str,
) -> bool {
    let realtime = |id: &str| {
        registry
            .get(id)
            .map(|factory| factory.realtime_capable())
            .unwrap_or(true)
    };
    realtime(external_id) && realtime(internal_id)
}

fn preferred_evaluation_mode(
    backend_rebuild_params: Option<BackendRebuildParams>,
) -> PreferredEvaluationMode {
    backend_rebuild_params
        .map(|params| params.preferred_evaluation_mode())
        .unwrap_or(PreferredEvaluationMode::PrecomputedCartesian)
}

/// Inputs available to a [`BackendFactory`] when it builds its plan: the resolved
/// speaker layout, the live parameters, and the optional geometry rebuild params
/// (present for backends that need triangulation, e.g. VBAP).
pub struct BackendBuildCtx<'a> {
    pub layout: &'a SpeakerLayout,
    pub live: &'a LiveParams,
    pub backend_rebuild_params: Option<BackendRebuildParams>,
    /// The registry the active backend was looked up in. A composite backend
    /// (hybrid) resolves its inner models through this so any registered backend
    /// can be composed, not just a hard-coded set. See [`resolve_hybrid_inner_plan`].
    pub registry: &'a BackendRegistry,
    /// All host-set backend param values, keyed by backend id then param key
    /// (see [`crate::backend_params`]). Read at build time only — never on the
    /// audio hot path. A backend reads its own entry via [`backend_param`]; a
    /// composite backend (hybrid) reads its inner backends' entries by their id,
    /// which is why the whole store is exposed rather than just the active slice.
    ///
    /// [`backend_param`]: BackendBuildCtx::backend_param
    pub backend_params: &'a std::collections::HashMap<
        String,
        std::collections::HashMap<String, crate::backend_params::ParamValue>,
    >,
}

impl BackendBuildCtx<'_> {
    /// Host-set value of `key` for backend `backend_id`, or `None` to fall back
    /// to the backend's default.
    pub fn backend_param(
        &self,
        backend_id: &str,
        key: &str,
    ) -> Option<&crate::backend_params::ParamValue> {
        self.backend_params.get(backend_id).and_then(|m| m.get(key))
    }
}

/// A render backend's registration entry: a stable id plus how to build its gain
/// model plan for a given context.
///
/// Implement this and `register` it into a [`BackendRegistry`] to add a backend
/// without editing the central dispatch. Identity is data (a string id), not an
/// enum variant, so a backend can live in its own crate.
pub trait BackendFactory: Send + Sync {
    /// Stable identifier matched against `LiveParams::backend_id()` (e.g. `"vbap"`).
    fn id(&self) -> &'static str;
    /// Human-facing name shown in the UI. Defaults to the id.
    fn label(&self) -> &'static str {
        self.id()
    }
    /// Tunable parameters this backend exposes, as data. The host stores values
    /// generically and the UI renders controls from this; the backend reads the
    /// values via [`BackendBuildCtx::param`] when building. Defaults to none.
    fn param_schema(&self) -> Vec<crate::backend_params::ParamSpec> {
        Vec::new()
    }
    /// Like [`param_schema`](BackendFactory::param_schema), but with access to the
    /// backend's currently stored param values, so a backend whose schema depends
    /// on its own state can report a *dynamic* schema. The scriptable backend
    /// uses this to expose the params its currently-selected `.lua` declares.
    /// Defaults to the static schema. Called by the host whenever it publishes
    /// the backend list (i.e. after every param change).
    fn param_schema_for(
        &self,
        params: &std::collections::HashMap<String, crate::backend_params::ParamValue>,
    ) -> Vec<crate::backend_params::ParamSpec> {
        let _ = params;
        self.param_schema()
    }
    /// Whether this backend can run in the realtime (per-sample) evaluation mode.
    /// A backend whose `compute_gains` is not hot-path-safe (allocates, locks,
    /// crosses into an interpreter — e.g. the scriptable backend) returns `false`
    /// so the host forces a precomputed mode and never calls it per sample.
    fn realtime_capable(&self) -> bool {
        true
    }
    /// Build this backend's plan, or `None` if it cannot be prepared for the
    /// given context (e.g. VBAP without geometry rebuild params).
    fn build_plan(&self, ctx: &BackendBuildCtx<'_>) -> Option<BackendBuildPlan>;
}

/// A registered backend's UI-facing identity, reported by
/// [`BackendRegistry::available`] so a host can list the selectable backends
/// (built-in and contributor-registered alike) without a hard-coded table.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BackendListing {
    pub id: &'static str,
    pub label: &'static str,
    /// This backend's declared tunable parameters, so the UI can render controls
    /// for it when selected.
    pub params: Vec<crate::backend_params::ParamSpec>,
}

/// Reorder a published backend list so the composite `hybrid` backend always
/// comes last in the selection combo, regardless of registration order
/// (contributor backends register after the builtins, so registration order
/// alone does not keep hybrid last). Stable: every other backend keeps its
/// registration order. The bool key sorts `false` (non-hybrid) before `true`.
pub fn hybrid_last(mut listings: Vec<BackendListing>) -> Vec<BackendListing> {
    listings.sort_by_key(|listing| listing.id == "hybrid");
    listings
}

/// Ordered set of backend factories keyed by id. Use [`BackendRegistry::builtin`]
/// for the shipped backends; a host can `register` additional ones at startup.
pub struct BackendRegistry {
    factories: Vec<Box<dyn BackendFactory>>,
}

impl BackendRegistry {
    pub fn new() -> Self {
        Self {
            factories: Vec::new(),
        }
    }

    /// Registry preloaded with the built-in backends.
    pub fn builtin() -> Self {
        let mut registry = Self::new();
        registry.register(Box::new(VbapFactory));
        registry.register(Box::new(BarycenterFactory));
        registry.register(Box::new(ExperimentalDistanceFactory));
        registry.register(Box::new(HybridFactory));
        registry
    }

    /// Register a backend. A later registration with the same id replaces the
    /// earlier one, so a host can override a built-in.
    pub fn register(&mut self, factory: Box<dyn BackendFactory>) {
        let id = factory.id();
        match self.factories.iter_mut().find(|f| f.id() == id) {
            Some(slot) => *slot = factory,
            None => self.factories.push(factory),
        }
    }

    /// Look up a factory by id.
    pub fn get(&self, id: &str) -> Option<&dyn BackendFactory> {
        self.factories
            .iter()
            .find(|f| f.id() == id)
            .map(|f| f.as_ref())
    }

    /// Ids of all registered backends, in registration order.
    pub fn ids(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.factories.iter().map(|f| f.id())
    }

    /// Id + label of every registered backend, in registration order — the list
    /// a host publishes so the UI can offer them for selection.
    pub fn available(&self) -> Vec<BackendListing> {
        self.factories
            .iter()
            .map(|f| BackendListing {
                id: f.id(),
                label: f.label(),
                params: f.param_schema(),
            })
            .collect()
    }

    /// Like [`available`](BackendRegistry::available) but resolving each
    /// backend's *dynamic* schema against the current param store, so a backend
    /// whose controls depend on its own state (the scriptable backend) reports
    /// the right schema. `params_by_backend` is keyed by backend id.
    pub fn available_with(
        &self,
        params_by_backend: &std::collections::HashMap<
            String,
            std::collections::HashMap<String, crate::backend_params::ParamValue>,
        >,
    ) -> Vec<BackendListing> {
        let empty = std::collections::HashMap::new();
        self.factories
            .iter()
            .map(|f| BackendListing {
                id: f.id(),
                label: f.label(),
                params: f.param_schema_for(params_by_backend.get(f.id()).unwrap_or(&empty)),
            })
            .collect()
    }
}

impl Default for BackendRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

struct VbapFactory;
impl BackendFactory for VbapFactory {
    fn id(&self) -> &'static str {
        "vbap"
    }
    fn label(&self) -> &'static str {
        "VBAP"
    }
    fn param_schema(&self) -> Vec<crate::backend_params::ParamSpec> {
        use crate::backend_params::{ParamKind, ParamOption, ParamSpec, ParamValue};
        let enum_option = |value: &str, label: &str| ParamOption {
            value: value.to_string(),
            label: label.to_string(),
        };
        vec![
            ParamSpec::float("spread_min", "Spread min", 0.0, 1.0, 0.01, 0.0).help(
                "Lower bound of the spread range. A point source (no size, no distance \
                 spread) pans at this value.",
            ),
            ParamSpec::float("spread_max", "Spread max", 0.0, 1.0, 0.01, 1.0).help(
                "Upper bound of the spread range, reached by a full-size object or, with \
                 distance spread, at the listener.",
            ),
            ParamSpec::bool("spread_from_distance", "Spread from distance", false)
                .requires("supports_spread_from_distance")
                .help(
                    "Derive spread from the object's distance instead of its size: closer \
                     objects spread wider. This is a pure function of position, so it is \
                     baked into the precomputed table.",
                ),
            ParamSpec::float(
                "spread_distance_range",
                "Distance range",
                0.01,
                4.0,
                0.01,
                1.0,
            )
            .requires("supports_spread_from_distance")
            .help("Normalised distance at which distance-based spread falls back to zero."),
            ParamSpec::float(
                "spread_distance_curve",
                "Distance curve",
                0.1,
                8.0,
                0.1,
                1.0,
            )
            .requires("supports_spread_from_distance")
            .help("Curve exponent applied to the distance-based spread ramp."),
            ParamSpec {
                key: "size_to_spread_mode",
                label: "Object-size policy",
                kind: ParamKind::Enum {
                    options: vec![
                        enum_option("max", "Max axis"),
                        enum_option("mean", "Mean of axes"),
                        enum_option("projection_perpendicular", "Perpendicular projection"),
                    ],
                },
                default: ParamValue::Text("max".to_string()),
                requires: Some("supports_event_size"),
                help: Some(
                    "How a 3-D object-size triplet (w, d, h) is reduced to a scalar spread. \
                     Object-size spread is applied live and is honoured only in realtime \
                     evaluation.",
                ),
            },
            ParamSpec {
                key: "out_of_hull_mode",
                label: "Out-of-hull mode",
                kind: ParamKind::Enum {
                    options: vec![
                        enum_option("virtual_poles", "Virtual poles (BS.2127)"),
                        enum_option("blend", "Face blend"),
                        enum_option("fade", "Original (fade out)"),
                    ],
                },
                default: ParamValue::Text("virtual_poles".to_string()),
                requires: None,
                help: Some(
                    "How directions outside the speaker hull (overhead on layouts without \
                     heights, or below the listener) are rendered. Virtual poles (the \
                     default) follows ITU-R BS.2127: pole energy spreads evenly over the \
                     nearest speaker ring, at full level. Face blend also plays at full \
                     level but keeps the image as localised as the layout allows. Original \
                     is the historical behaviour: level fades with the fold angle, down to \
                     silence at an uncovered pole. A change rebuilds the topology.",
                ),
            },
            ParamSpec::float("fold_blend_power", "Blend sharpness", 1.0, 64.0, 1.0, 12.0).help(
                "Sharpness of the face blend (score exponent). Higher values snap to the \
                 closest boundary face for a tighter image; lower values blend more speakers \
                 near the poles. Only used in Face blend mode.",
            ),
        ]
    }
    fn build_plan(&self, ctx: &BackendBuildCtx<'_>) -> Option<BackendBuildPlan> {
        build_inner_backend_plan(ctx, self.id())
    }
}

struct BarycenterFactory;
impl BackendFactory for BarycenterFactory {
    fn id(&self) -> &'static str {
        "barycenter"
    }
    fn label(&self) -> &'static str {
        "Barycenter"
    }
    fn param_schema(&self) -> Vec<crate::backend_params::ParamSpec> {
        vec![
            crate::backend_params::ParamSpec::float("localize", "Localize", 0.0, 4.0, 0.05, 0.0)
                .help(
                    "Higher values collapse energy more tightly toward the target point; lower \
                     values stay broader and softer. Barycenter blends nearby speakers around a \
                     weighted center instead of VBAP triplet selection.",
                ),
        ]
    }
    fn build_plan(&self, ctx: &BackendBuildCtx<'_>) -> Option<BackendBuildPlan> {
        build_inner_backend_plan(ctx, self.id())
    }
}

struct ExperimentalDistanceFactory;
impl BackendFactory for ExperimentalDistanceFactory {
    fn id(&self) -> &'static str {
        "experimental_distance"
    }
    fn label(&self) -> &'static str {
        "Distance"
    }
    fn param_schema(&self) -> Vec<crate::backend_params::ParamSpec> {
        use crate::backend_params::ParamSpec;
        let d = crate::live_params::ExperimentalDistanceLiveParams::default();
        vec![
            ParamSpec::float(
                "distance_floor",
                "Distance floor",
                0.0,
                1.0,
                0.01,
                d.distance_floor,
            )
            .help(
                "Minimum distance used when scoring speakers, so a speaker right on the target \
                 point cannot dominate without bound.",
            ),
            ParamSpec::int(
                "min_active_speakers",
                "Min active speakers",
                1,
                32,
                d.min_active_speakers as i64,
            )
            .help("Fewest speakers allowed to share energy for one object."),
            ParamSpec::int(
                "max_active_speakers",
                "Max active speakers",
                1,
                32,
                d.max_active_speakers as i64,
            )
            .help("Most speakers allowed to share energy for one object."),
            ParamSpec::float(
                "position_error_floor",
                "Position error floor",
                0.0,
                1.0,
                0.01,
                d.position_error_floor,
            )
            .help("Position error tolerated before gains start to be clamped or spread wider."),
            ParamSpec::float(
                "position_error_nearest_scale",
                "Position error nearest scale",
                0.0,
                4.0,
                0.05,
                d.position_error_nearest_scale,
            )
            .help("How aggressively the nearest speakers dominate as position error grows."),
            ParamSpec::float(
                "position_error_span_scale",
                "Position error span scale",
                0.0,
                4.0,
                0.05,
                d.position_error_span_scale,
            )
            .help("How much energy spreads to more speakers as position error grows."),
        ]
    }
    fn build_plan(&self, ctx: &BackendBuildCtx<'_>) -> Option<BackendBuildPlan> {
        build_inner_backend_plan(ctx, self.id())
    }
}

struct HybridFactory;
impl BackendFactory for HybridFactory {
    fn id(&self) -> &'static str {
        "hybrid"
    }
    fn label(&self) -> &'static str {
        "Hybrid"
    }
    fn build_plan(&self, ctx: &BackendBuildCtx<'_>) -> Option<BackendBuildPlan> {
        // The hybrid backend composes two inner backends, resolved through the
        // registry so any registered backend can be an inner model (nested hybrid
        // excepted). Each inner model reads its own params from the bag keyed by
        // the inner backend id, so it is tuned exactly like the standalone one.
        let external = resolve_hybrid_inner_plan(ctx, &ctx.live.hybrid.external_backend_id)?;
        let internal = resolve_hybrid_inner_plan(ctx, &ctx.live.hybrid.internal_backend_id)?;
        Some(BackendBuildPlan::Hybrid(HybridBuildPlan {
            external: Box::new(external),
            internal: Box::new(internal),
            curve: ctx.live.hybrid.curve.clone(),
            curve_smoothing: ctx.live.hybrid.curve_smoothing,
            metric: ctx.live.hybrid.metric,
        }))
    }
}

pub fn prepare_topology_build_plan(
    registry: &BackendRegistry,
    layout: SpeakerLayout,
    live: &LiveParams,
    backend_rebuild_params: Option<BackendRebuildParams>,
    backend_params: &std::collections::HashMap<
        String,
        std::collections::HashMap<String, crate::backend_params::ParamValue>,
    >,
    evaluation_build_config: crate::render_backend::EvaluationBuildConfig,
) -> Option<TopologyBuildPlan> {
    // Dispatch through the registry instead of a hard-coded `match` on the id, so
    // a backend's construction lives with the backend rather than here.
    let factory = registry.get(live.backend_id())?;
    let ctx = BackendBuildCtx {
        layout: &layout,
        live,
        backend_rebuild_params,
        backend_params,
        registry,
    };
    let backend_build = factory.build_plan(&ctx)?;
    let preferred = preferred_evaluation_mode(backend_rebuild_params);
    let mut evaluation_mode = effective_live_evaluation_mode(live.evaluation.mode, preferred);
    // A backend that is not hot-path-safe (e.g. the scriptable backend) must
    // never run per sample: force a precomputed mode if realtime was requested.
    // For hybrid this is recursive — a non-realtime *inner* backend forces it too.
    let realtime_capable = factory.realtime_capable()
        && (live.backend_id() != "hybrid"
            || inners_realtime_capable(
                registry,
                &live.hybrid.external_backend_id,
                &live.hybrid.internal_backend_id,
            ));
    if evaluation_mode == LiveEvaluationMode::Realtime && !realtime_capable {
        evaluation_mode = match preferred {
            PreferredEvaluationMode::PrecomputedCartesian => {
                LiveEvaluationMode::PrecomputedCartesian
            }
            PreferredEvaluationMode::PrecomputedPolar => LiveEvaluationMode::PrecomputedPolar,
        };
    }
    Some(TopologyBuildPlan {
        layout,
        backend_id: live.backend_id().to_string(),
        backend_build,
        evaluation_mode,
        evaluation_build_config,
        geometry_generation: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render_backend::{
        BackendCapabilities, CartesianEvaluationConfig, PolarEvaluationConfig, RenderRequest,
        RenderResponse,
    };
    use crate::spatial_vbap::{DistanceMetric, DistanceModel, Gains, OutOfHullMode};

    #[test]
    fn out_of_hull_bag_values_resolve_with_schema_defaults() {
        // Absent keys → the schema default (BS.2127 virtual poles).
        assert_eq!(
            parse_out_of_hull_mode(None, None),
            OutOfHullMode::VirtualPoles
        );
        // Canonical and alias spellings select the pole downmix.
        for spelling in ["virtual_poles", "poles", "bs2127", " Virtual_Poles "] {
            assert_eq!(
                parse_out_of_hull_mode(Some(spelling), None),
                OutOfHullMode::VirtualPoles,
                "{spelling}"
            );
        }
        // The legacy fade and its aliases.
        for spelling in ["fade", "original", "legacy"] {
            assert_eq!(
                parse_out_of_hull_mode(Some(spelling), None),
                OutOfHullMode::Fade,
                "{spelling}"
            );
        }
        // Power follows the bag in blend mode, clamped to the schema bounds;
        // an absent power uses the historical constant.
        assert_eq!(
            parse_out_of_hull_mode(Some("blend"), None),
            OutOfHullMode::Blend {
                power: OutOfHullMode::DEFAULT_BLEND_POWER
            }
        );
        assert_eq!(
            parse_out_of_hull_mode(Some("blend"), Some(24.0)),
            OutOfHullMode::Blend { power: 24.0 }
        );
        assert_eq!(
            parse_out_of_hull_mode(Some("blend"), Some(1000.0)),
            OutOfHullMode::Blend { power: 64.0 }
        );
        assert_eq!(
            parse_out_of_hull_mode(Some("blend"), Some(f32::NAN)),
            OutOfHullMode::Blend {
                power: OutOfHullMode::DEFAULT_BLEND_POWER
            }
        );
        // Junk falls back to the default mode rather than poisoning the build.
        assert_eq!(
            parse_out_of_hull_mode(Some("nonsense"), Some(3.0)),
            OutOfHullMode::VirtualPoles
        );
    }

    const TEST_SPEAKERS: usize = 4;

    fn realtime_caps() -> BackendCapabilities {
        BackendCapabilities {
            supports_realtime: true,
            supports_precomputed_polar: false,
            supports_precomputed_cartesian: false,
            supports_position_interpolation: false,
            supports_distance_model: false,
            supports_spread: false,
            supports_spread_from_distance: false,
            supports_event_size: false,
            supports_distance_diffuse: false,
            supports_table_export: false,
        }
    }

    fn build_config() -> EvaluationBuildConfig {
        EvaluationBuildConfig {
            request_template: RenderRequest {
                adm_position: [0.0, 0.0, 0.0],
                event_size: [0.0; 3],
                room_ratio: [1.0, 1.0, 1.0],
                room_ratio_rear: 1.0,
                room_ratio_lower: 1.0,
                room_ratio_center_blend: 0.5,
                use_distance_diffuse: false,
                diffuse_mirror_axes: crate::spatial_vbap::MirrorAxes::default(),
                distance_diffuse_threshold: 1.0,
                distance_diffuse_curve: 1.0,
                distance_model: DistanceModel::default(),
            },
            position_interpolation: false,
            cartesian: CartesianEvaluationConfig {
                x_size: 5,
                y_size: 5,
                z_size: 3,
                z_neg_size: 0,
            },
            polar: PolarEvaluationConfig {
                azimuth_values: 8,
                elevation_values: 5,
                distance_values: 4,
                distance_max: 1.0,
                allow_negative_z: false,
            },
            distance_model_metric: DistanceMetric::default(),
            distance_diffuse_metric: DistanceMetric::default(),
            object_size_intervals: 0,
            object_size_mode: crate::render_backend::SizeToSpreadMode::default(),
        }
    }

    /// Build a realtime engine for `model`. Realtime wrapping does not sample the
    /// model, so a backend that misbehaves in `compute_gains` builds fine here and
    /// is only caught by the smoke test — exactly the case we want to cover.
    fn realtime_engine(model: Box<dyn GainModel>) -> PreparedRenderEngine {
        build_prepared_render_engine(model, EffectiveEvaluationMode::Realtime, &build_config())
            .expect("realtime engine builds")
    }

    macro_rules! fake_backend {
        ($name:ident, $id:literal, $compute:expr) => {
            struct $name;
            impl GainModel for $name {
                fn backend_id(&self) -> &'static str {
                    $id
                }
                fn backend_label(&self) -> &'static str {
                    $id
                }
                fn capabilities(&self) -> BackendCapabilities {
                    realtime_caps()
                }
                fn speaker_count(&self) -> usize {
                    TEST_SPEAKERS
                }
                fn compute_gains(&self, _req: &RenderRequest) -> RenderResponse {
                    $compute
                }
                fn save_to_file(
                    &self,
                    _path: &std::path::Path,
                    _layout: &SpeakerLayout,
                ) -> Result<()> {
                    Ok(())
                }
            }
        };
    }

    fake_backend!(
        PanicBackend,
        "panic_backend",
        panic!("boom from contributor backend")
    );
    fake_backend!(NonFiniteBackend, "nan_backend", {
        let mut gains = Gains::zeroed(TEST_SPEAKERS);
        gains.set(0, f32::NAN);
        RenderResponse { gains }
    });
    fake_backend!(
        WrongCountBackend,
        "wrong_count_backend",
        RenderResponse {
            gains: Gains::zeroed(TEST_SPEAKERS + 1),
        }
    );
    fake_backend!(
        GoodBackend,
        "good_backend",
        RenderResponse {
            gains: Gains::zeroed(TEST_SPEAKERS),
        }
    );

    #[test]
    fn smoke_test_rejects_panicking_backend() {
        let engine = realtime_engine(Box::new(PanicBackend));
        let err = smoke_test_engine(&engine, &build_config(), "panic_backend").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("panicked in compute_gains"), "got: {msg}");
        assert!(msg.contains("panic_backend"), "got: {msg}");
    }

    #[test]
    fn smoke_test_rejects_non_finite_gains() {
        let engine = realtime_engine(Box::new(NonFiniteBackend));
        let err = smoke_test_engine(&engine, &build_config(), "nan_backend").unwrap_err();
        assert!(err.to_string().contains("non-finite"), "got: {err}");
    }

    #[test]
    fn smoke_test_rejects_wrong_gain_count() {
        let engine = realtime_engine(Box::new(WrongCountBackend));
        let err = smoke_test_engine(&engine, &build_config(), "wrong_count_backend").unwrap_err();
        assert!(err.to_string().contains("expected"), "got: {err}");
    }

    #[test]
    fn smoke_test_accepts_well_behaved_backend() {
        let engine = realtime_engine(Box::new(GoodBackend));
        smoke_test_engine(&engine, &build_config(), "good_backend")
            .expect("well-behaved backend passes the smoke test");
    }

    struct DummyFactory(&'static str);
    impl BackendFactory for DummyFactory {
        fn id(&self) -> &'static str {
            self.0
        }
        fn build_plan(&self, _ctx: &BackendBuildCtx<'_>) -> Option<BackendBuildPlan> {
            None
        }
    }

    #[test]
    fn builtin_registry_exposes_known_backends() {
        let registry = BackendRegistry::builtin();
        let ids: Vec<_> = registry.ids().collect();
        for expected in ["vbap", "barycenter", "experimental_distance", "hybrid"] {
            assert!(
                ids.contains(&expected),
                "missing builtin backend {expected}"
            );
        }
        assert!(registry.get("vbap").is_some());
        assert!(registry.get("does_not_exist").is_none());
    }

    #[test]
    fn register_adds_then_overrides_by_id() {
        let mut registry = BackendRegistry::new();
        registry.register(Box::new(DummyFactory("custom")));
        assert!(registry.get("custom").is_some());

        let count = registry.ids().count();
        // Re-registering the same id replaces the entry instead of appending.
        registry.register(Box::new(DummyFactory("custom")));
        assert_eq!(registry.ids().count(), count);
    }

    /// A factory for an out-of-tree backend: it produces a `Dynamic` plan whose
    /// builder constructs an arbitrary `GainModel`, with no dedicated enum variant
    /// and no central `match` edit.
    struct DynamicFactory;
    impl BackendFactory for DynamicFactory {
        fn id(&self) -> &'static str {
            "dynamic_example"
        }
        fn build_plan(&self, _ctx: &BackendBuildCtx<'_>) -> Option<BackendBuildPlan> {
            Some(BackendBuildPlan::Dynamic(DynamicBackendPlan::new(
                "dynamic_example",
                || Ok(Box::new(GoodBackend)),
            )))
        }
    }

    #[test]
    fn dynamic_plan_builds_an_arbitrary_gain_model() {
        let plan = BackendBuildPlan::Dynamic(DynamicBackendPlan::new("dynamic_example", || {
            Ok(Box::new(GoodBackend))
        }));
        // The plan is cloneable (it rides inside a TopologyBuildPlan)...
        let cloned = plan.clone();
        // ...and both build the model the closure returns.
        assert_eq!(
            plan.build_gain_model().unwrap().speaker_count(),
            TEST_SPEAKERS
        );
        assert_eq!(
            cloned.build_gain_model().unwrap().backend_id(),
            "good_backend"
        );
    }

    #[test]
    fn available_lists_id_and_label() {
        let listings = BackendRegistry::builtin().available();
        let vbap = listings
            .iter()
            .find(|l| l.id == "vbap")
            .expect("vbap listed");
        assert_eq!(vbap.label, "VBAP");
        // Every builtin reports a non-empty label.
        assert!(listings.iter().all(|l| !l.label.is_empty()));
    }

    /// A registered factory that is not hot-path-safe (mirrors the scriptable
    /// backend), used to check the recursive realtime gate for hybrid inners.
    struct NonRealtimeFactory;
    impl BackendFactory for NonRealtimeFactory {
        fn id(&self) -> &'static str {
            "non_realtime"
        }
        fn realtime_capable(&self) -> bool {
            false
        }
        fn build_plan(&self, _ctx: &BackendBuildCtx<'_>) -> Option<BackendBuildPlan> {
            Some(BackendBuildPlan::Dynamic(DynamicBackendPlan::new(
                "non_realtime",
                || Ok(Box::new(GoodBackend)),
            )))
        }
    }

    #[test]
    fn inners_realtime_capable_reflects_registry() {
        let mut registry = BackendRegistry::builtin();
        registry.register(Box::new(NonRealtimeFactory));
        // Two realtime-capable builtins: hybrid can stay realtime.
        assert!(inners_realtime_capable(&registry, "vbap", "barycenter"));
        // A non-realtime inner forces the composite to be non-realtime.
        assert!(!inners_realtime_capable(
            &registry,
            "non_realtime",
            "barycenter"
        ));
        assert!(!inners_realtime_capable(&registry, "vbap", "non_realtime"));
        // An unregistered id is treated as realtime-capable (fails to build later).
        assert!(inners_realtime_capable(&registry, "vbap", "does_not_exist"));
    }

    #[test]
    fn hybrid_last_moves_hybrid_to_the_end() {
        let mk = |id: &'static str| BackendListing {
            id,
            label: id,
            params: Vec::new(),
        };
        // Hybrid sits mid-list, with backends registered after it (example,
        // script) — exactly the case registration order alone gets wrong.
        let out = hybrid_last(vec![mk("vbap"), mk("hybrid"), mk("example"), mk("script")]);
        let ids: Vec<_> = out.iter().map(|l| l.id).collect();
        assert_eq!(ids, ["vbap", "example", "script", "hybrid"]);
    }

    #[test]
    fn hybrid_last_keeps_order_when_no_hybrid() {
        let mk = |id: &'static str| BackendListing {
            id,
            label: id,
            params: Vec::new(),
        };
        let out = hybrid_last(vec![mk("vbap"), mk("barycenter"), mk("example")]);
        let ids: Vec<_> = out.iter().map(|l| l.id).collect();
        assert_eq!(ids, ["vbap", "barycenter", "example"]);
    }

    #[test]
    fn registered_dynamic_factory_is_retrievable() {
        let mut registry = BackendRegistry::builtin();
        registry.register(Box::new(DynamicFactory));
        // An out-of-tree factory registers and is found by id alongside builtins.
        assert!(registry.get("dynamic_example").is_some());
        assert!(registry.get("vbap").is_some());
    }
}
