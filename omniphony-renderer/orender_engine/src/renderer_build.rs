//! Construction of a [`SpatialRenderer`] from neutral, host-agnostic parameters.
//!
//! Both the `orender` CLI and `orender_ffi` build the renderer through this one
//! function so that, given the same parameters, they produce an identical
//! renderer (and therefore bit-identical audio). The CLI fills
//! [`SpatialRendererParams`] from its parsed args; the FFI fills it from a YAML
//! [`RenderConfig`].

use anyhow::{Result, anyhow, bail};
use bridge_api::{RVbapCartesianDefaults, RVbapTableMode};
use renderer::config::RenderConfig;
use renderer::live_params::{LiveEvaluationMode, PreferredEvaluationMode};
use renderer::render_backend::canonical_builtin_backend_id;
use renderer::spatial_renderer::SpatialRenderer;
use renderer::spatial_vbap::{DistanceModel, VbapTableMode};
use renderer::speaker_layout::SpeakerLayout;
use std::path::PathBuf;
use std::str::FromStr;

/// VBAP pre-computed table mode (mirror of the CLI's `EvaluationModeArg`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalMode {
    Polar,
    Cartesian,
}

/// Host-neutral inputs to [`build_spatial_renderer`]. Field names and semantics
/// mirror the `render` CLI args / config keys.
#[derive(Debug, Clone)]
pub struct SpatialRendererParams {
    pub vbap_table: Option<PathBuf>,
    pub evaluation_polar_azimuth_resolution: i32,
    pub evaluation_polar_elevation_resolution: i32,
    pub evaluation_polar_distance_res: i32,
    pub evaluation_polar_distance_max: f32,
    /// Evaluation table mode chosen by the user (CLI flag or config YAML).
    /// `None` means "no explicit choice" — the engine then follows the
    /// `preferred_evaluation_mode` advertised by the format bridge.
    pub render_evaluation_mode: Option<EvalMode>,
    pub evaluation_mode_explicit: bool,
    pub evaluation_cartesian_x_size: Option<usize>,
    pub evaluation_cartesian_y_size: Option<usize>,
    pub evaluation_cartesian_z_size: Option<usize>,
    pub evaluation_cartesian_z_neg_size: Option<usize>,
    pub vbap_allow_negative_z: bool,
    pub no_vbap_allow_negative_z: bool,
    pub render_evaluation_position_interpolation: bool,
    pub vbap_distance_model: String,
    pub spread_from_distance: bool,
    pub spread_distance_range: f32,
    pub spread_distance_curve: f32,
    pub vbap_spread_min: f32,
    pub vbap_spread_max: f32,
    pub log_object_positions: bool,
    pub room_ratio: String,
    pub room_ratio_rear: Option<f32>,
    pub room_ratio_lower: Option<f32>,
    pub room_ratio_center_blend: Option<f32>,
    pub master_gain: f32,
    pub auto_gain: bool,
    pub use_loudness: bool,
    pub distance_diffuse: bool,
    pub distance_diffuse_threshold: f32,
    pub distance_diffuse_curve: f32,
}

impl SpatialRendererParams {
    /// Resolve renderer params from a YAML render config, applying the same
    /// defaults the CLI uses (mirrors `config_resolution::merge_render_config`)
    /// so the FFI and CLI build an identical renderer from the same config.
    ///
    /// `log_object_positions` and precomputed `vbap_table` loading are CLI-only
    /// and stay off here. `render_evaluation_mode` is `None` when the config
    /// doesn't specify one — the engine then defers to the bridge's
    /// preferred mode (cartesian for OAMD/spatial sources). A config-set mode
    /// is honored but, like the CLI, not treated as "explicit" so the live
    /// evaluation mode starts at `Auto`.
    pub fn from_render_config(cfg: Option<&RenderConfig>) -> Self {
        let mode = cfg.and_then(|c| c.render_evaluation_mode.as_deref());
        let render_evaluation_mode = match mode {
            Some(v)
                if v.eq_ignore_ascii_case("precomputed_cartesian")
                    || v.eq_ignore_ascii_case("cartesian") =>
            {
                Some(EvalMode::Cartesian)
            }
            Some(v)
                if v.eq_ignore_ascii_case("precomputed_polar")
                    || v.eq_ignore_ascii_case("polar") =>
            {
                Some(EvalMode::Polar)
            }
            _ => None,
        };
        Self {
            vbap_table: None,
            evaluation_polar_azimuth_resolution: cfg
                .and_then(renderer::config_fields::vbap_azimuth_resolution::get)
                .unwrap_or(renderer::config_fields::vbap_azimuth_resolution::DEFAULT),
            evaluation_polar_elevation_resolution: cfg
                .and_then(renderer::config_fields::vbap_elevation_resolution::get)
                .unwrap_or(renderer::config_fields::vbap_elevation_resolution::DEFAULT),
            evaluation_polar_distance_res: cfg
                .and_then(renderer::config_fields::vbap_distance_res::get)
                .unwrap_or(renderer::config_fields::vbap_distance_res::DEFAULT),
            evaluation_polar_distance_max: cfg
                .and_then(renderer::config_fields::vbap_distance_max::get)
                .unwrap_or(renderer::config_fields::vbap_distance_max::DEFAULT),
            render_evaluation_mode,
            evaluation_mode_explicit: false,
            evaluation_cartesian_x_size: cfg.and_then(|c| c.evaluation_cartesian_x_size),
            evaluation_cartesian_y_size: cfg.and_then(|c| c.evaluation_cartesian_y_size),
            evaluation_cartesian_z_size: cfg.and_then(|c| c.evaluation_cartesian_z_size),
            evaluation_cartesian_z_neg_size: cfg.and_then(|c| c.evaluation_cartesian_z_neg_size),
            vbap_allow_negative_z: matches!(cfg.and_then(|c| c.vbap_allow_negative_z), Some(true)),
            no_vbap_allow_negative_z: matches!(
                cfg.and_then(|c| c.vbap_allow_negative_z),
                Some(false)
            ),
            render_evaluation_position_interpolation: cfg
                .and_then(renderer::config_fields::render_evaluation_position_interpolation::get)
                .unwrap_or(
                    renderer::config_fields::render_evaluation_position_interpolation::DEFAULT,
                ),
            vbap_distance_model: cfg
                .and_then(renderer::config_fields::vbap_distance_model::get)
                .unwrap_or_else(|| {
                    renderer::config_fields::vbap_distance_model::DEFAULT.to_string()
                }),
            spread_from_distance: cfg
                .and_then(renderer::config_fields::spread_from_distance::get)
                .unwrap_or(renderer::config_fields::spread_from_distance::DEFAULT),
            spread_distance_range: cfg
                .and_then(renderer::config_fields::spread_distance_range::get)
                .unwrap_or(renderer::config_fields::spread_distance_range::DEFAULT),
            spread_distance_curve: cfg
                .and_then(renderer::config_fields::spread_distance_curve::get)
                .unwrap_or(renderer::config_fields::spread_distance_curve::DEFAULT),
            vbap_spread_min: cfg
                .and_then(renderer::config_fields::vbap_spread_min::get)
                .unwrap_or(renderer::config_fields::vbap_spread_min::DEFAULT),
            vbap_spread_max: cfg
                .and_then(renderer::config_fields::vbap_spread_max::get)
                .unwrap_or(renderer::config_fields::vbap_spread_max::DEFAULT),
            log_object_positions: false,
            room_ratio: cfg
                .and_then(|c| c.room_ratio.clone())
                .unwrap_or_else(|| "1.0,2.0,1.0".to_string()),
            room_ratio_rear: cfg.and_then(|c| c.room_ratio_rear),
            room_ratio_lower: cfg.and_then(|c| c.room_ratio_lower),
            room_ratio_center_blend: cfg.and_then(|c| c.room_ratio_center_blend),
            master_gain: cfg
                .and_then(renderer::config_fields::master_gain::get)
                .unwrap_or(renderer::config_fields::master_gain::DEFAULT),
            auto_gain: cfg
                .and_then(renderer::config_fields::auto_gain::get)
                .unwrap_or(renderer::config_fields::auto_gain::DEFAULT),
            use_loudness: cfg
                .and_then(renderer::config_fields::use_loudness::get)
                .unwrap_or(renderer::config_fields::use_loudness::DEFAULT),
            distance_diffuse: cfg
                .and_then(renderer::config_fields::distance_diffuse::get)
                .unwrap_or(renderer::config_fields::distance_diffuse::DEFAULT),
            distance_diffuse_threshold: cfg
                .and_then(renderer::config_fields::distance_diffuse_threshold::get)
                .unwrap_or(renderer::config_fields::distance_diffuse_threshold::DEFAULT),
            distance_diffuse_curve: cfg
                .and_then(renderer::config_fields::distance_diffuse_curve::get)
                .unwrap_or(renderer::config_fields::distance_diffuse_curve::DEFAULT),
        }
    }
}

fn parse_room_ratio(params: &SpatialRendererParams) -> Result<([f32; 3], f32, f32, f32)> {
    let parts: Vec<&str> = params.room_ratio.split(',').collect();
    if parts.len() != 3 {
        bail!(
            "Invalid room-ratio format '{}'. Expected 'width,length,height' (e.g., '1.0,2.0,0.5')",
            params.room_ratio
        );
    }
    let room_ratio = [
        parts[0]
            .trim()
            .parse::<f32>()
            .map_err(|_| anyhow!("Invalid room-ratio width: '{}'", parts[0]))?,
        parts[1]
            .trim()
            .parse::<f32>()
            .map_err(|_| anyhow!("Invalid room-ratio length: '{}'", parts[1]))?,
        parts[2]
            .trim()
            .parse::<f32>()
            .map_err(|_| anyhow!("Invalid room-ratio height: '{}'", parts[2]))?,
    ];
    let room_ratio_rear = params.room_ratio_rear.unwrap_or(room_ratio[1]).max(0.01);
    let room_ratio_lower = params.room_ratio_lower.unwrap_or(0.5).max(0.01);
    let room_ratio_center_blend = params
        .room_ratio_center_blend
        .unwrap_or(0.5)
        .clamp(0.0, 1.0);
    Ok((
        room_ratio,
        room_ratio_rear,
        room_ratio_lower,
        room_ratio_center_blend,
    ))
}

fn resolve_evaluation_table_mode(
    params: &SpatialRendererParams,
    vbap_cartesian_defaults: RVbapCartesianDefaults,
    preferred_evaluation_mode: RVbapTableMode,
) -> Result<(VbapTableMode, bool)> {
    let vbap_allow_negative_z = if params.vbap_allow_negative_z {
        true
    } else if params.no_vbap_allow_negative_z {
        false
    } else {
        vbap_cartesian_defaults.allow_negative_z
    };
    // If the user didn't pick a mode (CLI default + no config entry),
    // honor the format bridge's preference — cartesian for OAMD/spatial
    // sources, which is dramatically faster to precompute than the polar
    // grid (the polar default could take ~6 s for 12 speakers).
    let effective_mode =
        params
            .render_evaluation_mode
            .unwrap_or_else(|| match preferred_evaluation_mode {
                RVbapTableMode::Cartesian => EvalMode::Cartesian,
                RVbapTableMode::Polar => EvalMode::Polar,
            });
    let vbap_table_mode = match effective_mode {
        EvalMode::Polar => VbapTableMode::Polar,
        EvalMode::Cartesian => {
            let x_cells = params
                .evaluation_cartesian_x_size
                .unwrap_or(vbap_cartesian_defaults.x_size as usize);
            let y_cells = params
                .evaluation_cartesian_y_size
                .unwrap_or(vbap_cartesian_defaults.y_size as usize);
            let z_cells = params
                .evaluation_cartesian_z_size
                .unwrap_or(vbap_cartesian_defaults.z_size as usize);
            let z_neg_cells = params.evaluation_cartesian_z_neg_size.unwrap_or(0);
            if x_cells < 1 || y_cells < 1 || z_cells < 1 {
                bail!(
                    "Invalid cartesian VBAP cell count: x={}, y={}, z+={} (each must be >= 1)",
                    x_cells,
                    y_cells,
                    z_cells
                );
            }
            VbapTableMode::Cartesian {
                x_size: x_cells + 1,
                y_size: y_cells + 1,
                z_size: z_cells + 1,
                z_neg_size: z_neg_cells,
            }
        }
    };
    Ok((vbap_table_mode, vbap_allow_negative_z))
}

/// Build a fully-configured [`SpatialRenderer`] from `params` and the bridge's
/// suggested defaults, applying any backend/evaluation/experimental-distance
/// overrides from `render_cfg`.
pub fn build_spatial_renderer(
    params: &SpatialRendererParams,
    layout: SpeakerLayout,
    sample_rate: u32,
    vbap_cartesian_defaults: RVbapCartesianDefaults,
    preferred_evaluation_mode: RVbapTableMode,
    render_cfg: Option<&RenderConfig>,
) -> Result<SpatialRenderer> {
    let distance_model = DistanceModel::from_str(&params.vbap_distance_model)
        .map_err(|e| anyhow!("Invalid distance model: {}", e))?;
    let (room_ratio, room_ratio_rear, room_ratio_lower, room_ratio_center_blend) =
        parse_room_ratio(params)?;

    let (vbap_table_mode, vbap_allow_negative_z) =
        resolve_evaluation_table_mode(params, vbap_cartesian_defaults, preferred_evaluation_mode)?;

    log::info!("VBAP allow_negative_z: {}", vbap_allow_negative_z);

    if let Some(ref vbap_table_path) = params.vbap_table {
        bail!(
            "loading precomputed renderer state from file is no longer supported ({})",
            vbap_table_path.display()
        );
    }

    let renderer = {
        log::info!(
            "Speaker layout: {} speakers ({})",
            layout.num_speakers(),
            layout.speaker_names().join(", ")
        );
        log::info!("Generating VBAP table at runtime (this may take a few seconds)...");
        let start_time = std::time::Instant::now();
        let azimuth_cells = params.evaluation_polar_azimuth_resolution.max(1);
        let elevation_cells = params.evaluation_polar_elevation_resolution.max(1);
        let distance_cells = params.evaluation_polar_distance_res.max(1);
        let azimuth_step_deg = (360.0f32 / (azimuth_cells as f32)).max(1.0).round() as i32;
        let elevation_step_deg = (((if vbap_allow_negative_z { 180.0 } else { 90.0 })
            / (elevation_cells as f32))
            .max(1.0)
            .round()) as i32;
        let distance_step =
            params.evaluation_polar_distance_max.max(0.01) / (distance_cells as f32);

        let renderer = SpatialRenderer::new(
            layout,
            sample_rate,
            azimuth_step_deg,
            elevation_step_deg,
            distance_step,
            params.evaluation_polar_distance_max,
            vbap_table_mode,
            vbap_allow_negative_z,
            params.render_evaluation_position_interpolation,
            distance_model,
            params.spread_from_distance,
            params.spread_distance_range,
            params.spread_distance_curve,
            params.vbap_spread_min,
            params.vbap_spread_max,
            params.log_object_positions,
            room_ratio,
            room_ratio_rear,
            room_ratio_lower,
            room_ratio_center_blend,
            params.master_gain,
            params.auto_gain,
            params.use_loudness,
            params.distance_diffuse,
            params.distance_diffuse_threshold,
            params.distance_diffuse_curve,
            match preferred_evaluation_mode {
                RVbapTableMode::Polar => PreferredEvaluationMode::PrecomputedPolar,
                RVbapTableMode::Cartesian => PreferredEvaluationMode::PrecomputedCartesian,
            },
            if params.evaluation_mode_explicit {
                match params.render_evaluation_mode {
                    Some(EvalMode::Polar) => LiveEvaluationMode::PrecomputedPolar,
                    Some(EvalMode::Cartesian) => LiveEvaluationMode::PrecomputedCartesian,
                    // evaluation_mode_explicit but no mode set is a logic error
                    // upstream; fall back to Auto rather than panicking.
                    None => LiveEvaluationMode::Auto,
                }
            } else {
                LiveEvaluationMode::Auto
            },
            params
                .evaluation_cartesian_x_size
                .unwrap_or(vbap_cartesian_defaults.x_size as usize),
            params
                .evaluation_cartesian_y_size
                .unwrap_or(vbap_cartesian_defaults.y_size as usize),
            params
                .evaluation_cartesian_z_size
                .unwrap_or(vbap_cartesian_defaults.z_size as usize),
            params.evaluation_cartesian_z_neg_size.unwrap_or(0),
        )?;
        let elapsed = start_time.elapsed();
        log::info!("VBAP table generated in {:.2}s", elapsed.as_secs_f64());
        renderer
    };

    log::info!("VBAP spatial rendering enabled");
    // Raw configured backend id; resolved against the enum aliases *and* the
    // registry below (so a registered out-of-tree backend id is selectable too).
    let configured_backend_cfg = render_cfg.and_then(|cfg| cfg.render_backend.as_deref());
    let configured_evaluation = render_cfg
        .and_then(|cfg| cfg.render_evaluation_mode.as_deref())
        .and_then(LiveEvaluationMode::from_str);
    {
        let control = renderer.renderer_control();
        // Register the demonstration backend so `backend_id = "example"` resolves.
        control.register_backend(Box::new(example_backend::ExampleFactory));
        // User-scriptable (Lua) backend; selecting `backend_id = "script"` routes
        // a rebuild through it, reading its `.lua` path from the param store.
        control.register_backend(Box::new(script_backend::ScriptFactory));
        // Resolved after registration so any registered backend (not just the
        // historical concrete ones) is accepted as a hybrid inner model; a nested
        // hybrid or an unregistered id falls back to the default.
        let hybrid_cfg = render_cfg.map(|cfg| {
            let defaults = renderer::live_params::HybridLiveParams::default();
            let valid_inner = |id: &str| id != "hybrid" && control.has_backend(id);
            renderer::live_params::HybridLiveParams {
                external_backend_id: cfg
                    .hybrid_external_backend
                    .clone()
                    .filter(|id| valid_inner(id))
                    .unwrap_or(defaults.external_backend_id),
                internal_backend_id: cfg
                    .hybrid_internal_backend
                    .clone()
                    .filter(|id| valid_inner(id))
                    .unwrap_or(defaults.internal_backend_id),
                curve: cfg
                    .hybrid_curve
                    .clone()
                    .filter(|points| points.len() >= 2)
                    .unwrap_or(defaults.curve),
                curve_smoothing: cfg
                    .hybrid_curve_smoothing
                    .map(|v| v.clamp(0.0, 1.0))
                    .unwrap_or(defaults.curve_smoothing),
                metric: cfg
                    .hybrid_metric
                    .as_deref()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(defaults.metric),
            }
        });
        // Resolve the configured backend id: built-in ids/aliases first (e.g.
        // "distance" -> experimental_distance), then any registered backend id.
        let configured_backend = configured_backend_cfg.and_then(|raw| {
            canonical_builtin_backend_id(raw)
                .map(|id| id.to_string())
                .or_else(|| control.has_backend(raw).then(|| raw.to_string()))
        });
        let mut requires_rebuild = false;
        // Replay persisted generic backend param values, and migrate the legacy
        // dedicated keys (barycenter_localize / experimental_distance_*) into the
        // same bag so old configs keep working. All are read at the rebuild below
        // via each backend's schema.
        if let Some(cfg) = render_cfg {
            use renderer::backend_params::ParamValue;
            if !cfg.backend_params.is_empty() {
                requires_rebuild = true;
            }
            for (backend_id, params) in &cfg.backend_params {
                for (key, value) in params {
                    control.set_backend_param(backend_id, key, value.clone());
                }
            }
            let mut migrate = |backend_id: &str, key: &str, value: Option<ParamValue>| {
                if let Some(value) = value {
                    control.set_backend_param(backend_id, key, value);
                    requires_rebuild = true;
                }
            };
            migrate(
                "barycenter",
                "localize",
                cfg.barycenter_localize.map(ParamValue::Float),
            );
            migrate(
                "experimental_distance",
                "distance_floor",
                cfg.experimental_distance_distance_floor
                    .map(ParamValue::Float),
            );
            migrate(
                "experimental_distance",
                "min_active_speakers",
                cfg.experimental_distance_min_active_speakers
                    .map(|v| ParamValue::Int(v as i64)),
            );
            migrate(
                "experimental_distance",
                "max_active_speakers",
                cfg.experimental_distance_max_active_speakers
                    .map(|v| ParamValue::Int(v as i64)),
            );
            migrate(
                "experimental_distance",
                "position_error_floor",
                cfg.experimental_distance_position_error_floor
                    .map(ParamValue::Float),
            );
            migrate(
                "experimental_distance",
                "position_error_nearest_scale",
                cfg.experimental_distance_position_error_nearest_scale
                    .map(ParamValue::Float),
            );
            migrate(
                "experimental_distance",
                "position_error_span_scale",
                cfg.experimental_distance_position_error_span_scale
                    .map(ParamValue::Float),
            );
            // VBAP spread tuning moved from dedicated config keys / LiveParams
            // into the same bag; migrate legacy keys so old configs keep working.
            migrate(
                "vbap",
                "spread_min",
                renderer::config_fields::vbap_spread_min::get(cfg).map(ParamValue::Float),
            );
            migrate(
                "vbap",
                "spread_max",
                renderer::config_fields::vbap_spread_max::get(cfg).map(ParamValue::Float),
            );
            migrate(
                "vbap",
                "spread_from_distance",
                renderer::config_fields::spread_from_distance::get(cfg).map(ParamValue::Bool),
            );
            migrate(
                "vbap",
                "spread_distance_range",
                renderer::config_fields::spread_distance_range::get(cfg).map(ParamValue::Float),
            );
            migrate(
                "vbap",
                "spread_distance_curve",
                renderer::config_fields::spread_distance_curve::get(cfg).map(ParamValue::Float),
            );
            migrate(
                "vbap",
                "size_to_spread_mode",
                cfg.size_to_spread_mode
                    .map(|mode| ParamValue::Text(mode.as_str().to_string())),
            );
        }
        {
            let mut live = control.live.write();
            if let Some(configured_backend) = &configured_backend {
                if live.backend_id() != configured_backend {
                    live.backend_id = configured_backend.clone();
                    requires_rebuild = true;
                }
            }
            if let Some(configured_evaluation) = configured_evaluation {
                if live.evaluation.mode != configured_evaluation {
                    live.set_evaluation_mode(configured_evaluation);
                    requires_rebuild = true;
                }
            }
            if let Some(intervals) = render_cfg.and_then(|c| c.evaluation_object_size_intervals) {
                if live.evaluation.object_size_intervals != intervals {
                    live.evaluation.object_size_intervals = intervals;
                    requires_rebuild = true;
                }
            }
            if let Some(hybrid) = hybrid_cfg {
                if live.hybrid.external_backend_id != hybrid.external_backend_id
                    || live.hybrid.internal_backend_id != hybrid.internal_backend_id
                    || live.hybrid.curve != hybrid.curve
                    || (live.hybrid.curve_smoothing - hybrid.curve_smoothing).abs() > 1e-6
                    || live.hybrid.metric != hybrid.metric
                {
                    live.hybrid = hybrid;
                    requires_rebuild = true;
                }
            }
            if let Some(metric) = render_cfg
                .and_then(|cfg| cfg.distance_model_metric.as_deref())
                .and_then(|s| s.parse::<renderer::spatial_vbap::DistanceMetric>().ok())
            {
                if live.distance_model_metric != metric {
                    live.distance_model_metric = metric;
                    requires_rebuild = true;
                }
            }
            if let Some(metric) = render_cfg
                .and_then(|cfg| cfg.distance_diffuse_metric.as_deref())
                .and_then(|s| s.parse::<renderer::spatial_vbap::DistanceMetric>().ok())
            {
                if live.distance_diffuse_metric != metric {
                    live.distance_diffuse_metric = metric;
                    requires_rebuild = true;
                }
            }
            if let Some(axes) = render_cfg
                .and_then(|cfg| cfg.distance_diffuse_mirror_axes.as_deref())
                .and_then(|s| s.parse::<renderer::spatial_vbap::MirrorAxes>().ok())
            {
                if live.distance_diffuse_mirror_axes != axes {
                    live.distance_diffuse_mirror_axes = axes;
                    requires_rebuild = true;
                }
            }
            // Per-frame live params (no topology rebuild): seed from config so a
            // saved value is honoured at startup, not only after an OSC tweak.
            if let Some(mode) = render_cfg.and_then(|cfg| cfg.size_to_spread_mode) {
                live.size_to_spread_mode = mode;
            }
            if let Some(ceiling) =
                render_cfg.and_then(renderer::config_fields::auto_gain_ceiling_db::get)
            {
                live.auto_gain_ceiling_db = ceiling;
            }
            // Binaural (headphone) stage: seed from config so a saved mode/scale is
            // honoured at startup. No topology rebuild — the binaural path does not
            // use the speaker topology.
            if let Some(bin) = render_cfg.and_then(|cfg| cfg.binaural.as_ref()) {
                if let Some(mode) = bin
                    .output_mode
                    .as_deref()
                    .and_then(renderer::live_params::OutputMode::from_str)
                {
                    live.binaural.output_mode = mode;
                }
                if let Some(scale) = bin.unit_scale_m {
                    if scale.is_finite() && scale > 0.0 {
                        live.binaural.unit_scale_m = scale;
                    }
                }
                if let Some(radius) = bin.head_radius_m {
                    if radius.is_finite() && radius > 0.0 {
                        live.binaural.head_radius_m = radius.clamp(0.05, 0.15);
                    }
                }
                if let Some(refl) = bin.reflections.as_ref() {
                    let r = &mut live.binaural.reflections;
                    if let Some(en) = refl.enabled {
                        r.enabled = en;
                    }
                    for (slot, v) in [
                        (0usize, refl.room_width_m),
                        (1, refl.room_depth_m),
                        (2, refl.room_height_m),
                    ] {
                        if let Some(v) = v {
                            if v.is_finite() && v > 0.0 {
                                r.room_size_m[slot] = v.clamp(
                                    renderer::binaural::reflections::MIN_ROOM_M,
                                    renderer::binaural::reflections::MAX_ROOM_M,
                                );
                            }
                        }
                    }
                    if let Some(level) = refl.level {
                        if level.is_finite() {
                            r.level = level.clamp(0.0, 1.0);
                        }
                    }
                }
                if let Some(rev) = bin.reverb.as_ref() {
                    let r = &mut live.binaural.reverb;
                    if let Some(en) = rev.enabled {
                        r.enabled = en;
                    }
                    if let Some(level) = rev.level {
                        if level.is_finite() {
                            r.level = level.clamp(0.0, 1.0);
                        }
                    }
                    if let Some(rt60) = rev.rt60_s {
                        if rt60.is_finite() && rt60 > 0.0 {
                            r.rt60_s = rt60.clamp(0.1, 3.0);
                        }
                    }
                    if let Some(pd) = rev.predelay_ms {
                        if pd.is_finite() && pd >= 0.0 {
                            r.predelay_ms = pd.clamp(0.0, 100.0);
                        }
                    }
                }
                if let Some(air) = bin.air_absorption {
                    live.binaural.air_absorption = air;
                }
                if let Some(ht) = bin.head_tracking.as_ref() {
                    if let Some(addr) = ht.osc_address.as_ref() {
                        live.binaural.tracking.address = (!addr.is_empty()).then(|| addr.clone());
                    }
                    if let Some(fmt) = ht
                        .format
                        .as_deref()
                        .and_then(renderer::binaural::HeadTrackingFormat::from_str)
                    {
                        live.binaural.tracking.format = fmt;
                    }
                    // Restore the persisted recenter reference so the centering
                    // survives an engine rebuild (mpv track change) and a restart.
                    // `head_pose`/`last_raw` stay at their defaults: the first
                    // incoming OSC packet re-derives the centered pose.
                    if let Some(q) = ht.reference_quat {
                        live.binaural.tracking.reference =
                            renderer::binaural::HeadPose::from_quat_array(q);
                    }
                }
                // HRIR source: a "sofa" selector resolves its path from
                // `hrtf_sofa_path` (or an inline "sofa:<path>").
                if let Some(src) = bin
                    .hrir_source
                    .as_deref()
                    .and_then(renderer::binaural::HrirSource::from_str)
                {
                    live.binaural.hrir_source = match src {
                        renderer::binaural::HrirSource::Sofa(p) if p.is_empty() => {
                            match bin.hrtf_sofa_path.as_ref() {
                                Some(path) => renderer::binaural::HrirSource::Sofa(
                                    path.to_string_lossy().into_owned(),
                                ),
                                None => renderer::binaural::HrirSource::SafKemar,
                            }
                        }
                        other => other,
                    };
                }
            }
        }
        if requires_rebuild {
            if let Some(plan) = control.prepare_topology_rebuild() {
                let topology = plan.build_topology()?;
                control.publish_topology(topology);
            }
        }
    }

    Ok(renderer)
}
