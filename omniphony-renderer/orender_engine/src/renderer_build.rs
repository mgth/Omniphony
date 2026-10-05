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
use renderer::live_params::{LiveEvaluationMode, PreferredEvaluationMode, RendererControl};
use renderer::spatial_renderer::{RendererSpec, SpatialRenderer};
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

/// Room proportions (`width,length,height`) when neither the config nor a flag
/// sets `room_ratio`. Shared by the config resolution below and the CLI's
/// `--room-ratio` default so the two cannot drift; declared with the rest of
/// the room's config reading in `renderer::config_fields::room`.
pub const DEFAULT_ROOM_RATIO: &str = renderer::config_fields::room::DEFAULT_RATIO;

/// Parse a configured evaluation table mode (`render.render_evaluation_mode`)
/// into the precomputed-table choice, or `None` for anything else (`auto`,
/// `realtime`, unknown), which leaves the choice to the bridge.
pub fn parse_eval_mode(value: &str) -> Option<EvalMode> {
    if value.eq_ignore_ascii_case("precomputed_cartesian")
        || value.eq_ignore_ascii_case("cartesian")
    {
        Some(EvalMode::Cartesian)
    } else if value.eq_ignore_ascii_case("precomputed_polar") || value.eq_ignore_ascii_case("polar")
    {
        Some(EvalMode::Polar)
    } else {
        None
    }
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
    /// `preferred_evaluation_mode` advertised by the format bridge and the
    /// live evaluation mode starts at `Auto`; a choice also starts the live
    /// mode on it.
    pub render_evaluation_mode: Option<EvalMode>,
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
    /// Resolve renderer params from a YAML render config, applying the
    /// built-in defaults for absent keys. This is the one config→params
    /// resolution: the embedded engine uses it as is, the CLI lays its
    /// explicit flags over it.
    ///
    /// `log_object_positions` and precomputed `vbap_table` loading are CLI-only
    /// and stay off here. `render_evaluation_mode` is `None` when the config
    /// doesn't pick a precomputed table — the engine then defers to the
    /// bridge's preferred mode (cartesian for OAMD/spatial sources). A
    /// config-set table mode is an explicit choice: the live evaluation mode
    /// starts on it, so the config seed that follows construction finds it
    /// already applied and does not rebuild the topology a second time.
    pub fn from_render_config(cfg: Option<&RenderConfig>) -> Self {
        let render_evaluation_mode = cfg
            .and_then(|c| c.render_evaluation_mode.as_deref())
            .and_then(parse_eval_mode);
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
                .unwrap_or_else(|| DEFAULT_ROOM_RATIO.to_string()),
            room_ratio_rear: cfg.and_then(|c| c.room_ratio_rear),
            room_ratio_lower: cfg.and_then(|c| c.room_ratio_lower),
            room_ratio_center_blend: cfg.and_then(|c| c.room_ratio_center_blend),
            master_gain: cfg
                .and_then(renderer::config_fields::master_gain::get)
                .unwrap_or(renderer::config_fields::master_gain::DEFAULT),
            auto_gain: cfg
                .and_then(|c| c.options.auto_gain)
                .unwrap_or(renderer::options::defaults::auto_gain),
            use_loudness: cfg
                .and_then(|c| c.options.use_loudness)
                .unwrap_or(renderer::options::defaults::use_loudness),
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

/// The room `params` describe, read by the same rule as the live seed
/// (`renderer::config_fields::room`).
fn parse_room_ratio(params: &SpatialRendererParams) -> Result<([f32; 3], f32, f32, f32)> {
    let room = renderer::config_fields::room::parse(
        &params.room_ratio,
        params.room_ratio_rear,
        params.room_ratio_lower,
        params.room_ratio_center_blend,
    )
    .map_err(|e| anyhow!(e))?;
    Ok((room.ratio, room.rear, room.lower, room.center_blend))
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

        let renderer = SpatialRenderer::new(RendererSpec {
            speaker_layout: layout,
            sample_rate,
            az_res_deg: azimuth_step_deg,
            el_res_deg: elevation_step_deg,
            spread_resolution: distance_step,
            distance_max: params.evaluation_polar_distance_max,
            table_mode: vbap_table_mode,
            allow_negative_z: vbap_allow_negative_z,
            vbap_position_interpolation: params.render_evaluation_position_interpolation,
            distance_model,
            spread_from_distance: params.spread_from_distance,
            spread_distance_range: params.spread_distance_range,
            spread_distance_curve: params.spread_distance_curve,
            spread_min: params.vbap_spread_min,
            spread_max: params.vbap_spread_max,
            log_object_positions: params.log_object_positions,
            room_ratio,
            room_ratio_rear,
            room_ratio_lower,
            room_ratio_center_blend,
            master_gain_db: params.master_gain,
            auto_gain: params.auto_gain,
            use_loudness: params.use_loudness,
            distance_diffuse: params.distance_diffuse,
            distance_diffuse_threshold: params.distance_diffuse_threshold,
            distance_diffuse_curve: params.distance_diffuse_curve,
            preferred_evaluation_mode: match preferred_evaluation_mode {
                RVbapTableMode::Polar => PreferredEvaluationMode::PrecomputedPolar,
                RVbapTableMode::Cartesian => PreferredEvaluationMode::PrecomputedCartesian,
            },
            initial_evaluation_mode: match params.render_evaluation_mode {
                Some(EvalMode::Polar) => LiveEvaluationMode::PrecomputedPolar,
                Some(EvalMode::Cartesian) => LiveEvaluationMode::PrecomputedCartesian,
                None => LiveEvaluationMode::Auto,
            },
            cartesian_default_x_size: params
                .evaluation_cartesian_x_size
                .unwrap_or(vbap_cartesian_defaults.x_size as usize),
            cartesian_default_y_size: params
                .evaluation_cartesian_y_size
                .unwrap_or(vbap_cartesian_defaults.y_size as usize),
            cartesian_default_z_size: params
                .evaluation_cartesian_z_size
                .unwrap_or(vbap_cartesian_defaults.z_size as usize),
            cartesian_default_z_neg_size: params.evaluation_cartesian_z_neg_size.unwrap_or(0),
        })?;
        let elapsed = start_time.elapsed();
        // No gain table yet: the speaker stage samples one per crossover band
        // on the first frame, once the config seed below has landed.
        log::info!("Spatial renderer built in {:.2}s", elapsed.as_secs_f64());
        renderer
    };

    log::info!("VBAP spatial rendering enabled");
    {
        let control = renderer.renderer_control();
        // The demonstration backend (`backend_id = "example"`), only in builds
        // made with the `example-backend` feature.
        #[cfg(feature = "example-backend")]
        control.register_backend(Box::new(example_backend::ExampleFactory));
        // User-scriptable (Lua) backend; selecting `backend_id = "script"` routes
        // a rebuild through it, reading its `.lua` path from the param store.
        control.register_backend(Box::new(script_backend::ScriptFactory));
        if seed_control_from_render_config(&control, render_cfg) {
            if let Some(plan) = control.prepare_topology_rebuild() {
                let topology = plan.build_topology()?;
                control.publish_topology(topology);
            }
        }
    }

    Ok(renderer)
}

/// Seed a running control's live params and backend param store from a render
/// config — the config→control half of [`build_spatial_renderer`], shared with
/// the live profile switch (docs/config-profiles.md) so a profile activated at
/// boot and the same one activated live cannot drift. Returns whether the
/// seeded changes require a topology rebuild; the caller owns the rebuild.
pub fn seed_control_from_render_config(
    control: &RendererControl,
    render_cfg: Option<&RenderConfig>,
) -> bool {
    // `requires_rebuild`: evaluation-only changes (mode, size intervals) that a
    // rebuild can serve by re-wrapping the current gain models. `model_changed`:
    // the backend, its params or its metrics changed, so the models themselves
    // must be rebuilt.
    let mut requires_rebuild = false;
    let mut model_changed = false;
    {
        // The hybrid curve, a point list kept out of the registry (the legs,
        // smoothing and metric are registry rows, seeded below). A curve of
        // fewer than two points falls back to the default.
        let hybrid_curve = render_cfg.map(|cfg| {
            cfg.hybrid_curve
                .clone()
                .filter(|points| points.len() >= 2)
                .unwrap_or_else(|| renderer::live_params::HybridLiveParams::default().curve)
        });
        // Replay persisted generic plugin param values, and migrate the legacy
        // dedicated keys (barycenter_localize / experimental_distance_*) into the
        // same bag so old configs keep working. All are read at the rebuild below
        // via each backend's schema.
        if let Some(cfg) = render_cfg {
            use renderer::backend_params::ParamValue;
            if !cfg.backend_params.is_empty() {
                model_changed = true;
            }
            // Every plugin kind's values (the generators' and the phantom
            // stage's legacy keys migrated), in one store.
            control.seed_plugin_params(renderer::plugin::PluginParams::from_config(cfg));
            let mut migrate = |backend_id: &str, key: &str, value: Option<ParamValue>| {
                if let Some(value) = value {
                    control.set_backend_param(backend_id, key, value);
                    model_changed = true;
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
            if let Some(curve) = hybrid_curve {
                if live.hybrid.curve != curve {
                    live.hybrid.curve = curve;
                    model_changed = true;
                }
            }
            // Declared options whose groups shape the topology or the
            // evaluation layer (room, distance, …): seeded here, before the
            // caller decides whether to rebuild, and folded into that decision.
            if let Some(render) = render_cfg {
                match renderer::options::seed_rebuilding_rows_from_config(
                    &mut live,
                    render,
                    &renderer::options::OptionEnv::of(control),
                ) {
                    renderer::options::Rebuild::Topology => model_changed = true,
                    renderer::options::Rebuild::Evaluation => requires_rebuild = true,
                    renderer::options::Rebuild::None => {}
                }
            }
            // Per-frame live params (no topology rebuild): seed from config so a
            // saved value is honoured at startup, not only after an OSC tweak.
            if let Some(mode) = render_cfg.and_then(|cfg| cfg.size_to_spread_mode) {
                live.size_to_spread_mode = mode;
            }
            if let Some(ceiling) = render_cfg.and_then(|cfg| cfg.options.auto_gain_ceiling_db) {
                live.options.auto_gain_ceiling_db = ceiling;
            }
            // Binaural: the options are registry rows (seeded with the others
            // by `seed_live_from_config`). Seeded here: the ear mutes, and the
            // persisted recenter reference and axis calibration, so the
            // centering survives an engine rebuild (mpv track change) and a
            // restart. `head_pose` / `last_raw` stay at their defaults: the
            // first incoming OSC packet re-derives the centered pose.
            if let Some(bin) = render_cfg.and_then(|cfg| cfg.binaural.as_ref()) {
                if let Some(mutes) = bin.ear_mutes {
                    for (ear, muted) in live.binaural.ears.iter_mut().zip(mutes) {
                        ear.muted = muted;
                    }
                }
                if let Some(ht) = bin.head_tracking.as_ref() {
                    if let Some(q) = ht.reference_quat {
                        live.binaural.tracking.reference =
                            renderer::binaural::HeadPose::from_quat_array(q);
                    }
                    if let Some(q) = ht.axes_quat {
                        live.binaural.tracking.axes =
                            renderer::binaural::HeadPose::from_quat_array(q);
                    }
                }
            }
        }
    }
    if model_changed {
        // Same rule as an OSC change to the model (`osc::dispatch`): bump the
        // geometry generation so every rebuild after this seed, the crossover
        // band renderers' included, builds new gain models instead of
        // re-wrapping the ones built before the config was applied.
        control.bump_geometry_generation();
    }
    requires_rebuild || model_changed
}

/// Seed the host-runtime live state (monitoring cadences, ramp mode, declared
/// options, DRC selection) from a render config. Shared by the embedded engine
/// boot ([`crate::engine::Engine::from_paths`]) and the live profile switch;
/// the CLI seeds the same fields through its arg-aware bootstrap.
pub fn seed_runtime_state_from_render_config(
    control: &RendererControl,
    render_cfg: Option<&RenderConfig>,
) {
    // Fallback comes from the host (recorded on the control at boot), not from
    // a literal here: this same seed is replayed by the live profile switch,
    // which has no idea which host it is running in.
    control.seed_cadences_from_config(
        render_cfg.and_then(|c| c.meter_rate),
        render_cfg.and_then(|c| c.diag_rate),
    );

    // Object-transition ramp mode: honour `render.ramp_mode` (default "frame";
    // per-sample `compute_gains` is the most expensive path and must be an
    // explicit choice). Both the requested-mode mutex and the live snapshot
    // field must be set — the render loop reads the latter.
    let ramp_mode = render_cfg
        .and_then(|cfg| cfg.options.ramp_mode)
        .unwrap_or(renderer::options::defaults::ramp_mode);
    control.live.write().options.ramp_mode = ramp_mode;

    // Declared live options (registry rows) plus their param bags and the
    // virtual bed: one shared registry seed, same call as the CLI bootstrap.
    if let Some(render) = render_cfg {
        renderer::options::seed_live_from_config(
            &mut control.live.write(),
            render,
            &renderer::options::OptionEnv::of(control),
        );
    }

    // DRC selection. The decode-side mode is pushed to the bridge lazily by
    // the host (see the engine's `sync_live_options`); the bridge's supported-mode
    // list is host knowledge and stays with the host.
    {
        let mut live = control.live.write();
        live.options.drc_mode = render_cfg
            .and_then(|c| c.options.drc_mode.clone())
            .unwrap_or_else(|| "Off".to_string());
        live.options.drc_weight = render_cfg
            .and_then(|c| c.options.drc_weight)
            .unwrap_or(1.0)
            .clamp(0.0, 1.0);
    }
}

/// Record the bridge a host runs with as the live `render.bridge_path` — what
/// Studio shows and edits, and what a save writes back. Every host records the
/// path it was *asked* for: its own override (a CLI flag, the C config's
/// `bridge_path`) else the config's. Never an auto-discovered one: a bridge
/// found next to the host binary is that host's, and saving it would write,
/// say, the mpv bundle's bridge into the config every host shares. A host
/// override that differs from the config is unsaved state.
pub fn record_bridge_path(
    control: &RendererControl,
    host_override: Option<&std::path::Path>,
    config_bridge: Option<&std::path::Path>,
) {
    let recorded = host_override.or(config_bridge);
    if recorded != config_bridge {
        control.mark_dirty();
    }
    control.set_bridge_path(recorded.map(std::path::Path::to_path_buf));
}

/// Record the config's `render.input_pipe` as the live input path, so a save
/// writes it back. A host that reads a different input records that one after
/// this (the CLI records the path it actually opened). Without it, a host that
/// never sets an input path — the embedded engine, its no-bridge runtime —
/// saves an unset input path, which erases `render.input_pipe` from the
/// config every host shares.
pub fn record_input_path(control: &RendererControl, render_cfg: Option<&RenderConfig>) {
    control.set_input_path(
        render_cfg
            .and_then(|cfg| cfg.input_pipe.as_deref())
            .map(|path| path.display().to_string()),
    );
}

/// What a host records on a freshly built renderer's control besides the
/// renderer itself. See [`seed_host_state`].
pub struct HostStateSeed<'a> {
    /// The config file the host runs on: its path and load status (About),
    /// its profiles view, and where Save writes.
    pub config_path: Option<&'a std::path::Path>,
    /// The render section the host resolved (the file, live-handoff sidecar
    /// included, plus the host's own overrides).
    pub render_cfg: Option<&'a RenderConfig>,
    /// The bridge path the host itself was asked for (a CLI flag, the C
    /// config's field); the config's own comes from `render_cfg`.
    pub requested_bridge_path: Option<&'a std::path::Path>,
    /// This host's monitoring cadence fallback, meter then diag, in Hz.
    pub cadence_defaults_hz: (f32, f32),
}

/// Record the host-side state every live-state bundle carries: the bridge path
/// ([`record_bridge_path`]), the config path, load status and profiles view,
/// the unsaved mark of a restored live handoff, the cadence fallback and the
/// runtime seed ([`seed_runtime_state_from_render_config`]).
///
/// Shared by the CLI's render bootstrap and the no-bridge runtime of both
/// hosts ([`crate::degraded::NoBridgeRuntime`]), so a renderer that came up
/// without a decoder publishes — and saves — the same state as one that did.
pub fn seed_host_state(control: &RendererControl, seed: &HostStateSeed<'_>) {
    record_bridge_path(
        control,
        seed.requested_bridge_path,
        seed.render_cfg.and_then(|c| c.bridge_path.as_deref()),
    );
    record_input_path(control, seed.render_cfg);
    if let Some(path) = seed.config_path {
        // State restored from a live-handoff sidecar is by definition unsaved.
        if renderer::config::live_overlay_active(path) {
            control.mark_dirty();
        }
        control.set_config_path(path.to_path_buf());
        // Whether the config actually loaded, so Studio's About can compare
        // hosts; `render_cfg` can't tell, `load_or_default` collapses a
        // missing or broken file into defaults. A restored sidecar that was
        // the previous instance's fallback keeps parse_error.
        control.set_config_status(Some(
            renderer::config::boot_load_status(path)
                .as_str()
                .to_string(),
        ));
        // Client-visible profiles view (active name + list); see
        // docs/config-profiles.md.
        control.set_profiles_info(renderer::config::Config::load_or_default(path).profiles_info());
    }
    // Declared before the seed, which falls back to it; a later profile
    // switch, which replays the same seed, falls back to it too.
    let (meter_hz, diag_hz) = seed.cadence_defaults_hz;
    control.set_cadence_defaults_hz(meter_hz, diag_hz);
    seed_runtime_state_from_render_config(control, seed.render_cfg);
}

/// Re-apply a render config to a RUNNING engine — the live profile switch
/// (docs/config-profiles.md). Covers the construction-path seeding minus what
/// needs a new renderer instance (input plumbing, output device, bridge):
/// those land in the saved config and take effect at the next start. The
/// caller stages the new layout and triggers the topology rebuild — a switch
/// always rebuilds, so the `requires_rebuild` result is not surfaced here.
pub fn apply_render_config_live(
    control: &RendererControl,
    render_cfg: &RenderConfig,
) -> Result<()> {
    let params = SpatialRendererParams::from_render_config(Some(render_cfg));
    // A malformed room or distance model fails the switch, as it fails a
    // build; both are declared options, reset and seeded with the others
    // below.
    parse_room_ratio(&params)?;
    DistanceModel::from_str(&params.vbap_distance_model)
        .map_err(|e| anyhow!("Invalid distance model: {}", e))?;
    {
        let mut live = control.live.write();
        // Absent-means-default first: the shared seeds below only assign the
        // fields the config pins. That is correct at construction, where the
        // live params start at their defaults — but on a running control an
        // absent key would silently keep the OUTGOING profile's value (the
        // persist layer deliberately stores defaults as absence), and the
        // next profile op would then commit that leak into the incoming
        // profile. Return every profile-covered field to its default before
        // seeding.
        live.hybrid.curve = renderer::live_params::HybridLiveParams::default().curve;
        live.size_to_spread_mode = Default::default();
        live.options.auto_gain_ceiling_db = renderer::options::defaults::auto_gain_ceiling_db;
        live.binaural = renderer::live_params::BinauralLiveParams::default();
        renderer::options::reset_live_to_defaults(
            &mut live,
            &renderer::options::OptionEnv::of(control),
        );

        // Construction-time scalars that also exist as live params: the same
        // values `SpatialRenderer::new` would receive for this config
        // (`params` already encodes the config defaults for absent keys).
        live.options.auto_gain = params.auto_gain;
        live.options.use_loudness = params.use_loudness;
        // Spread fallbacks (used when the vbap param bag has no entry) —
        // construction seeds these from the same params.
        live.spread_min = params.vbap_spread_min;
        live.spread_max = params.vbap_spread_max;
        live.spread_from_distance = params.spread_from_distance;
        live.spread_distance_range = params.spread_distance_range;
        live.spread_distance_curve = params.spread_distance_curve;
    }
    // The replay in `seed_control_from_render_config` only inserts; without
    // the clear, the outgoing profile's plugin params would survive the
    // switch (and be committed into the incoming profile on the next save).
    control.clear_plugin_params();
    seed_control_from_render_config(control, Some(render_cfg));
    seed_runtime_state_from_render_config(control, Some(render_cfg));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_renderer() -> renderer::spatial_renderer::SpatialRenderer {
        let params = SpatialRendererParams::from_render_config(None);
        let layout = SpeakerLayout::preset("7.1.4").expect("preset layout");
        build_spatial_renderer(
            &params,
            layout,
            48_000,
            bridge_api::RVbapCartesianDefaults {
                x_size: 9,
                y_size: 9,
                z_size: 5,
                allow_negative_z: true,
            },
            bridge_api::RVbapTableMode::Cartesian,
            None,
        )
        .expect("renderer")
    }

    /// A config-set evaluation table mode is where the live mode starts, so
    /// the config seed that follows construction finds nothing left to
    /// change. It used to start at `Auto` in the embedded host (unlike the
    /// CLI), and every engine creation — every track a player opens — with
    /// such a config then built the VBAP topology a second time.
    #[test]
    fn a_configured_table_mode_needs_no_second_topology_build() {
        let cfg = RenderConfig {
            render_evaluation_mode: Some("precomputed_cartesian".to_string()),
            ..Default::default()
        };
        let params = SpatialRendererParams::from_render_config(Some(&cfg));
        let renderer = build_spatial_renderer(
            &params,
            SpeakerLayout::preset("7.1.4").expect("preset layout"),
            48_000,
            bridge_api::RVbapCartesianDefaults {
                x_size: 9,
                y_size: 9,
                z_size: 5,
                allow_negative_z: true,
            },
            // The bridge prefers the other table: the config must win.
            bridge_api::RVbapTableMode::Polar,
            None,
        )
        .expect("renderer");
        let control = renderer.renderer_control();
        assert_eq!(
            control.live.read().evaluation.mode,
            LiveEvaluationMode::PrecomputedCartesian
        );
        assert!(!seed_control_from_render_config(&control, Some(&cfg)));
    }

    /// A host that never sets an input path of its own (the embedded engine,
    /// the no-bridge runtimes) must save the config's `render.input_pipe`
    /// back as it was, not erase it.
    #[test]
    fn a_save_keeps_the_configured_input_pipe() {
        let renderer = test_renderer();
        let control = renderer.renderer_control();
        let cfg = RenderConfig {
            input_pipe: Some(std::path::PathBuf::from("/tmp/orender.pipe")),
            ..Default::default()
        };
        seed_host_state(
            &control,
            &HostStateSeed {
                config_path: None,
                render_cfg: Some(&cfg),
                requested_bridge_path: None,
                cadence_defaults_hz: (10.0, 10.0),
            },
        );
        assert_eq!(control.input_path().as_deref(), Some("/tmp/orender.pipe"));

        let mut saved = renderer::config::Config::default();
        runtime_control::persist::store_live_into_config(&control, None, &mut saved);
        assert_eq!(
            saved.render.and_then(|r| r.input_pipe),
            Some(std::path::PathBuf::from("/tmp/orender.pipe"))
        );
    }

    /// Configs whose declared options the renderer construction already
    /// applies, and one per group that only the seed applies.
    fn option_configs() -> Vec<(&'static str, RenderConfig)> {
        vec![
            (
                "polar grid",
                RenderConfig {
                    render_evaluation_mode: Some("precomputed_polar".into()),
                    vbap_azimuth_resolution: Some(100),
                    vbap_elevation_resolution: Some(45),
                    vbap_distance_res: Some(5),
                    vbap_distance_max: Some(3.0),
                    ..Default::default()
                },
            ),
            (
                "polar grid, negative elevations",
                RenderConfig {
                    render_evaluation_mode: Some("precomputed_polar".into()),
                    vbap_allow_negative_z: Some(true),
                    vbap_elevation_resolution: Some(60),
                    ..Default::default()
                },
            ),
            (
                "cartesian grid",
                RenderConfig {
                    render_evaluation_mode: Some("precomputed_cartesian".into()),
                    evaluation_cartesian_x_size: Some(7),
                    evaluation_cartesian_y_size: Some(6),
                    evaluation_cartesian_z_size: Some(4),
                    evaluation_cartesian_z_neg_size: Some(2),
                    render_evaluation_position_interpolation: Some(false),
                    ..Default::default()
                },
            ),
            (
                "room and distance",
                RenderConfig {
                    room_ratio: Some("1.0,1.5,0.7".into()),
                    room_ratio_lower: Some(0.3),
                    room_ratio_center_blend: Some(0.2),
                    vbap_distance_model: Some("linear".into()),
                    distance_diffuse: Some(true),
                    distance_diffuse_threshold: Some(0.5),
                    distance_diffuse_curve: Some(2.0),
                    ..Default::default()
                },
            ),
            (
                "binaural and master gain",
                RenderConfig {
                    master_gain: Some(-6.0),
                    binaural: Some(renderer::config::BinauralConfig {
                        output_mode: Some("binaural".into()),
                        mode: Some("cascaded".into()),
                        ear_gains: Some([0.8, 1.2]),
                        ear_mutes: Some([false, true]),
                        unit_scale_m: Some(2.0),
                        head_radius_m: Some(0.2),
                        hrir_source: Some("sofa".into()),
                        hrtf_sofa_path: Some("/data/hrtf/test.sofa".into()),
                        brir_head_tracking: Some(false),
                        brir_max_length_s: Some(1.5),
                        brir_tail_floor_db: Some(200.0),
                        head_tracking: Some(renderer::config::HeadTrackingConfig {
                            osc_address: Some("/rotation".into()),
                            format: Some("euler".into()),
                            smoothing: Some(0.5),
                            invert: Some(true),
                            ..Default::default()
                        }),
                        reverb: Some(renderer::config::ReverbConfig {
                            enabled: Some(true),
                            rt60_s: Some(9.0),
                            predelay_ms: Some(10.0),
                            ..Default::default()
                        }),
                        reflections: Some(renderer::config::ReflectionsConfig {
                            enabled: Some(true),
                            room_width_m: Some(50.0),
                            wall_cutoff_hz: Some(8000.0),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ),
            (
                "seed-only settings",
                RenderConfig {
                    render_evaluation_mode: Some("realtime".into()),
                    evaluation_object_size_intervals: Some(2),
                    distance_model_metric: Some("chebyshev".into()),
                    distance_diffuse_metric: Some("chebyshev".into()),
                    distance_diffuse_mirror_axes: Some("z".into()),
                    render_backend: Some("hybrid".into()),
                    hybrid_external_backend: Some("experimental_distance".into()),
                    hybrid_internal_backend: Some("vbap".into()),
                    hybrid_curve_smoothing: Some(0.5),
                    hybrid_metric: Some("spherical".into()),
                    ..Default::default()
                },
            ),
        ]
    }

    fn built_with(cfg: Option<&RenderConfig>) -> renderer::spatial_renderer::SpatialRenderer {
        let params = SpatialRendererParams::from_render_config(cfg);
        build_spatial_renderer(
            &params,
            SpeakerLayout::preset("7.1.4").expect("preset layout"),
            48_000,
            bridge_api::RVbapCartesianDefaults {
                x_size: 9,
                y_size: 9,
                z_size: 5,
                allow_negative_z: false,
            },
            bridge_api::RVbapTableMode::Cartesian,
            cfg,
        )
        .expect("renderer")
    }

    /// The seed before the first rebuild asks for one only for what the
    /// construction did not already apply: a renderer built from a config
    /// finds the room, the distance model and diffuse, and the grids already
    /// in force — laid out the same way, the polar grid's quantization
    /// included — and is not rebuilt a second time at every boot.
    #[test]
    fn the_seed_rebuilds_only_for_what_the_construction_left_out() {
        for (name, cfg) in option_configs() {
            let params = SpatialRendererParams::from_render_config(Some(&cfg));
            // Constructed without the config seed…
            let renderer = build_spatial_renderer(
                &params,
                SpeakerLayout::preset("7.1.4").expect("preset layout"),
                48_000,
                bridge_api::RVbapCartesianDefaults {
                    x_size: 9,
                    y_size: 9,
                    z_size: 5,
                    allow_negative_z: false,
                },
                bridge_api::RVbapTableMode::Cartesian,
                None,
            )
            .expect("renderer");
            let control = renderer.renderer_control();
            // …then seeded, as the build does.
            let rebuild = seed_control_from_render_config(&control, Some(&cfg));
            assert_eq!(rebuild, name == "seed-only settings", "{name}");
        }
    }

    /// A live profile switch lands every declared option where a boot on the
    /// same config lands (docs/config-profiles.md).
    #[test]
    fn a_profile_switch_lands_where_a_boot_on_the_same_config_lands() {
        for (name, cfg) in option_configs() {
            let booted = built_with(Some(&cfg));
            let booted = booted.renderer_control();
            seed_runtime_state_from_render_config(&booted, Some(&cfg));

            let switched = built_with(None);
            let switched = switched.renderer_control();
            seed_runtime_state_from_render_config(&switched, None);
            apply_render_config_live(&switched, &cfg).expect("switch");

            let booted = renderer::options::options_json(&booted.live.read());
            let switched = renderer::options::options_json(&switched.live.read());
            assert_eq!(booted, switched, "{name}");
        }
    }

    /// The demonstration backend is for contributors: a release build (no
    /// `example-backend` feature) must not offer it.
    #[test]
    fn the_example_backend_is_registered_only_with_its_feature() {
        let renderer = test_renderer();
        assert_eq!(
            renderer.renderer_control().has_backend("example"),
            cfg!(feature = "example-backend")
        );
    }

    /// A configured backend is applied after construction, so it must reach
    /// every model built afterwards, the crossover band renderers' included:
    /// the seed bumps the geometry generation, which they compare before
    /// reusing a model. An evaluation-only change keeps the generation, so its
    /// rebuild can still re-wrap the models.
    #[test]
    fn a_configured_backend_invalidates_the_models_built_before_it() {
        let renderer = test_renderer();
        let control = renderer.renderer_control();

        let before = control.geometry_generation();
        let eval_only = RenderConfig {
            render_evaluation_mode: Some("realtime".to_string()),
            ..Default::default()
        };
        assert!(seed_control_from_render_config(&control, Some(&eval_only)));
        assert_eq!(control.geometry_generation(), before);

        let backend = RenderConfig {
            render_backend: Some("barycenter".to_string()),
            ..Default::default()
        };
        assert!(seed_control_from_render_config(&control, Some(&backend)));
        assert!(control.geometry_generation() > before);
        let plan = control.prepare_topology_rebuild().expect("rebuild plan");
        let rebuilt = plan
            .build_topology_reusing(Some(&control.active_topology()))
            .expect("rebuild");
        assert_eq!(rebuilt.model_backend_id, "barycenter");
    }

    /// A host boot builds the crossover band engines exactly once, with the
    /// configured backend and crossover engine. They used to be built at
    /// construction on the defaults (VBAP, LR4) and rebuilt by the first frame
    /// once the config seed had landed: every band gain table sampled twice
    /// per start-up. A later live change still rebuilds them.
    #[test]
    fn a_boot_builds_the_band_tables_once_with_the_configured_options() {
        use renderer::live_params::CrossoverType;
        let cfg = RenderConfig {
            render_backend: Some("hybrid".to_string()),
            options: renderer::options::DeclaredOptionsConfig {
                crossover_type: Some(CrossoverType::Fir),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut layout = SpeakerLayout::preset("7.1.4").expect("preset layout");
        for (speaker, cutoff) in layout.speakers.iter_mut().zip([80.0, 200.0, 500.0]) {
            speaker.freq_low = Some(cutoff);
        }
        let params = SpatialRendererParams::from_render_config(Some(&cfg));
        let mut renderer = build_spatial_renderer(
            &params,
            layout,
            48_000,
            bridge_api::RVbapCartesianDefaults {
                x_size: 9,
                y_size: 9,
                z_size: 5,
                allow_negative_z: true,
            },
            bridge_api::RVbapTableMode::Cartesian,
            Some(&cfg),
        )
        .expect("renderer");
        let control = renderer.renderer_control();
        // The host's own seed (crossover engine among the declared options).
        seed_runtime_state_from_render_config(&control, Some(&cfg));
        assert_eq!(renderer.speaker_stage_builds(), 0, "nothing built yet");

        let silence = vec![0.0f32; 40 * 2];
        for _ in 0..3 {
            renderer
                .render_frame(&silence, 2, &[], Vec::new(), false)
                .expect("render");
        }
        assert_eq!(renderer.speaker_stage_builds(), 1, "built once");
        let info = control.crossover_info().expect("crossover info");
        assert_eq!(info.engine, CrossoverType::Fir);
        assert!(info.bands > 1, "the layout has crossover bands");
        assert_eq!(control.active_topology().model_backend_id, "hybrid");
        // The published topology names the backend and the mode; only the
        // bands sampled tables.
        let topology = control.active_topology();
        assert_eq!(topology.backend.backend_id(), "hybrid");
        assert_ne!(
            topology.backend.evaluation_mode(),
            renderer::render_backend::EffectiveEvaluationMode::Realtime
        );
        assert!(!topology.backend.has_sampled_table());

        // A live crossover flip (Studio) still rebuilds: the band worker
        // builds the new set while the old one renders on.
        control.live.write().options.crossover_type = CrossoverType::Lr4;
        renderer
            .render_frame(&silence, 2, &[], Vec::new(), false)
            .expect("render");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while renderer.speaker_stage_rebuild_pending() {
            assert!(
                std::time::Instant::now() < deadline,
                "the worker never delivered"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
            renderer
                .render_frame(&silence, 2, &[], Vec::new(), false)
                .expect("render");
        }
        assert_eq!(renderer.speaker_stage_builds(), 2);
        assert_eq!(
            control.crossover_info().expect("crossover info").engine,
            CrossoverType::Lr4
        );
        // `prepare_speaker_stage` on an up-to-date stage is a no-op.
        renderer.prepare_speaker_stage().expect("prepare");
        assert_eq!(renderer.speaker_stage_builds(), 2);
    }

    /// The recorded bridge path is the one asked for, and asking for another
    /// than the config's is unsaved state.
    #[test]
    fn the_recorded_bridge_path_is_the_requested_one() {
        use std::path::Path;
        let renderer = test_renderer();
        let control = renderer.renderer_control();
        let dirty = || {
            control
                .config_dirty
                .load(std::sync::atomic::Ordering::Relaxed)
        };
        let config = Path::new("/cfg/libbridge.so");

        record_bridge_path(&control, None, Some(config));
        assert_eq!(control.bridge_path().as_deref(), Some(config));
        assert!(!dirty());

        record_bridge_path(&control, None, None);
        assert_eq!(control.bridge_path(), None);
        assert!(!dirty());

        let host = Path::new("/host/libbridge.so");
        record_bridge_path(&control, Some(host), Some(config));
        assert_eq!(control.bridge_path().as_deref(), Some(host));
        assert!(dirty());
    }

    /// The shared runtime seed must not carry a cadence of its own.
    ///
    /// It is replayed wholesale by the live profile switch, on whichever host
    /// happens to be running. When it hard-coded the embedded host's 10 Hz, a
    /// profile switch on the CLI silently dropped Studio's meters from 50 Hz to
    /// 10 — the bug this pins.
    #[test]
    fn the_runtime_seed_keeps_the_host_cadence_default() {
        let renderer = test_renderer();
        let control = renderer.renderer_control();

        control.set_cadence_defaults_hz(50.0, 50.0);
        seed_runtime_state_from_render_config(&control, None);
        assert_eq!(control.meter_rate_hz(), 50.0);
        assert_eq!(control.diag_rate_hz(), 50.0);

        // A second host, slower on purpose, gets its own value from the same
        // call — the seed reads the host, it does not decide.
        control.set_cadence_defaults_hz(10.0, 10.0);
        seed_runtime_state_from_render_config(&control, None);
        assert_eq!(control.meter_rate_hz(), 10.0);
        assert_eq!(control.diag_rate_hz(), 10.0);
    }
}
