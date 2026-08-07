use crate::cli::command::{
    Cli, EvaluationModeArg, LogFormat, LogLevel, OutputBackend, OutputFileFormatArg, RampModeArg,
    RenderArgSources, RenderArgs,
};
use anyhow::Result;

/// Parse the persisted `output_file_format` string into the CLI enum.
fn parse_output_file_format(s: &str) -> Option<OutputFileFormatArg> {
    match s.trim().to_ascii_lowercase().as_str() {
        "caf" => Some(OutputFileFormatArg::Caf),
        "raw_f32" | "rawf32" | "raw" | "f32" => Some(OutputFileFormatArg::RawF32),
        _ => None,
    }
}

/// String form of [`OutputFileFormatArg`] for persistence.
fn output_file_format_str(fmt: OutputFileFormatArg) -> &'static str {
    match fmt {
        OutputFileFormatArg::RawF32 => "raw_f32",
        OutputFileFormatArg::Caf => "caf",
    }
}

/// Apply the override-only render args onto a `RenderConfig`.
///
/// These options (backend selection + backend-specific params, distance
/// metrics, size-to-spread) are sourced by the renderer from the *config*
/// rather than from `RenderArgs` directly (`build_spatial_renderer` reads
/// `render_cfg`). Each arg is `Option`: `Some` overrides, `None` keeps whatever
/// the on-disk config (or another host) already set. Shared by the save path
/// (`effective_to_config`) and the runtime path (`render_config_from_path`) so
/// a CLI flag takes effect on a live run and on `--save-config` identically.
pub(super) fn apply_render_cfg_overrides(
    render: &mut renderer::config::RenderConfig,
    args: &RenderArgs,
) {
    if let Some(backend) = args.render_backend {
        render.render_backend = Some(backend.as_config_str().to_string());
    }
    if let Some(localize) = args.barycenter_localize {
        render.barycenter_localize = Some(localize);
    }
    if let Some(ceiling) = args.auto_gain_ceiling {
        render.auto_gain_ceiling_db = Some(ceiling);
    }
    if let Some(b) = args.hybrid_external_backend {
        render.hybrid_external_backend = Some(b.as_config_str().to_string());
    }
    if let Some(b) = args.hybrid_internal_backend {
        render.hybrid_internal_backend = Some(b.as_config_str().to_string());
    }
    if let Some(s) = args.hybrid_curve_smoothing {
        render.hybrid_curve_smoothing = Some(s);
    }
    if let Some(m) = args.hybrid_metric {
        render.hybrid_metric = Some(m.as_config_str().to_string());
    }
    if let Some(v) = args.experimental_distance_distance_floor {
        render.experimental_distance_distance_floor = Some(v);
    }
    if let Some(v) = args.experimental_distance_min_active_speakers {
        render.experimental_distance_min_active_speakers = Some(v);
    }
    if let Some(v) = args.experimental_distance_max_active_speakers {
        render.experimental_distance_max_active_speakers = Some(v);
    }
    if let Some(v) = args.experimental_distance_position_error_floor {
        render.experimental_distance_position_error_floor = Some(v);
    }
    if let Some(v) = args.experimental_distance_position_error_nearest_scale {
        render.experimental_distance_position_error_nearest_scale = Some(v);
    }
    if let Some(v) = args.experimental_distance_position_error_span_scale {
        render.experimental_distance_position_error_span_scale = Some(v);
    }
    if let Some(m) = args.distance_model_metric {
        render.distance_model_metric = Some(m.as_config_str().to_string());
    }
    if let Some(m) = args.distance_diffuse_metric {
        render.distance_diffuse_metric = Some(m.as_config_str().to_string());
    }
    if let Some(axes) = args.distance_diffuse_mirror_axes.as_deref() {
        render.distance_diffuse_mirror_axes = Some(axes.to_string());
    }
    if let Some(mode) = args.size_to_spread_mode {
        render.size_to_spread_mode = Some(mode.into());
    }
    apply_adaptive_resampling_overrides(render, args);
}

/// Apply the override-only adaptive-resampling PI tuning args onto a config.
/// Standalone (host-audio) only; `build_adaptive_resampling_config` reads these
/// from `render_cfg`. `integral_discharge_ratio` is intentionally not exposed.
fn apply_adaptive_resampling_overrides(
    render: &mut renderer::config::RenderConfig,
    args: &RenderArgs,
) {
    if let Some(v) = args.adaptive_resampling_kp_near {
        render.adaptive_resampling_kp_near = Some(v);
    }
    if let Some(v) = args.adaptive_resampling_ki {
        render.adaptive_resampling_ki = Some(v);
    }
    if let Some(v) = args.adaptive_resampling_max_adjust {
        render.adaptive_resampling_max_adjust = Some(v);
    }
    if let Some(v) = args.adaptive_resampling_enable_far_mode {
        render.adaptive_resampling_enable_far_mode = Some(v);
    }
    if let Some(v) = args.adaptive_resampling_force_silence_in_far_mode {
        render.adaptive_resampling_force_silence_in_far_mode = Some(v);
    }
    if let Some(v) = args.adaptive_resampling_hard_recover_high_in_far_mode {
        render.adaptive_resampling_hard_recover_high_in_far_mode = Some(v);
    }
    if let Some(v) = args.adaptive_resampling_hard_recover_low_in_far_mode {
        render.adaptive_resampling_hard_recover_low_in_far_mode = Some(v);
    }
    if let Some(v) = args.adaptive_resampling_far_mode_return_fade_in_ms {
        render.adaptive_resampling_far_mode_return_fade_in_ms = Some(v);
    }
    if let Some(v) = args.adaptive_resampling_high_recover_entry_margin_ms {
        render.adaptive_resampling_high_recover_entry_margin_ms = Some(v);
    }
    if let Some(v) = args.adaptive_resampling_low_recover_settle_stable_ms {
        render.adaptive_resampling_low_recover_settle_stable_ms = Some(v);
    }
    if let Some(v) = args.adaptive_resampling_low_recover_entry_margin_ms {
        render.adaptive_resampling_low_recover_entry_margin_ms = Some(v);
    }
    if let Some(v) = args.adaptive_resampling_low_recover_exit_margin_ms {
        render.adaptive_resampling_low_recover_exit_margin_ms = Some(v);
    }
    if let Some(v) = args.adaptive_resampling_low_recover_settle_margin_ms {
        render.adaptive_resampling_low_recover_settle_margin_ms = Some(v);
    }
    if let Some(v) = args.adaptive_resampling_low_recover_refill_delta_alpha {
        render.adaptive_resampling_low_recover_refill_delta_alpha = Some(v);
    }
    if let Some(v) = args.adaptive_resampling_control_smoothing_cutoff_hz {
        render.adaptive_resampling_control_smoothing_cutoff_hz = Some(v);
    }
    if let Some(v) = args.adaptive_resampling_control_smoothing_order {
        render.adaptive_resampling_control_smoothing_order = Some(v);
    }
    if let Some(v) = args.adaptive_resampling_use_pre_bridge_clock {
        render.adaptive_resampling_use_pre_bridge_clock = Some(v);
    }
    if let Some(v) = args.adaptive_resampling_use_output_pacing {
        render.adaptive_resampling_use_output_pacing = Some(v);
    }
    if let Some(v) = args.adaptive_resampling_disable_backpressure {
        render.adaptive_resampling_disable_backpressure = Some(v);
    }
}

pub(super) fn merge_render_config(
    cfg: &renderer::config::RenderConfig,
    args: &mut RenderArgs,
    arg_sources: &RenderArgSources<'_>,
) {
    use std::str::FromStr;

    // --- Option fields: fill only when None ---
    if args.speaker_layout.is_none() {
        args.speaker_layout = cfg.speaker_layout.clone();
    }
    if args.vbap_table.is_none() {
        args.vbap_table = cfg.vbap_table.clone();
    }
    if args.output_sample_rate.is_none() {
        args.output_sample_rate = cfg.output_sample_rate;
    }
    if !arg_sources.is_explicit("ramp_mode") {
        if let Some(v) = renderer::config_fields::ramp_mode::get(cfg) {
            if let Some(mode) = renderer::live_params::RampMode::from_str(&v) {
                args.ramp_mode = match mode {
                    renderer::live_params::RampMode::Off => RampModeArg::Off,
                    renderer::live_params::RampMode::Frame => RampModeArg::Frame,
                    renderer::live_params::RampMode::Sample => RampModeArg::Sample,
                    renderer::live_params::RampMode::Interp => RampModeArg::Interp,
                };
            }
        }
    }
    // `render.channel_render_mode` is a read-only legacy key. Fixed-channel
    // processing defaults to Omniphony; an explicit CLI flag remains available
    // for diagnostics/raw sink passthrough without becoming global config.
    if !arg_sources.is_explicit("surround_placement") {
        if let Some(placement) = renderer::config_fields::surround_placement::get(cfg) {
            args.surround_placement = placement.into();
        }
    }
    if args.bridge_path.is_none() {
        args.bridge_path = cfg.bridge_path.clone();
    }
    // In continuous (studio bridge) mode the config is the source of truth for the
    // input pipe, overriding the positional default the studio passes at launch.
    if args.continuous {
        if let Some(ref input_pipe) = cfg.input_pipe {
            args.input = Some(input_pipe.clone());
        }
    }
    // Note: drc_mode currently doesn't have a CLI arg, it's OSC/config only.
    // --- Fields with defaults: apply config only when value equals the clap default ---
    // (If the user explicitly passes the default value, config is ignored — acceptable edge case.)
    if !arg_sources.is_explicit("output_backend") {
        if let Some(ref s) = cfg.output_backend {
            if let Ok(f) = OutputBackend::from_str(s) {
                args.output_backend = Some(f);
            }
        }
    }
    if !arg_sources.is_explicit("output_file") {
        if let Some(ref s) = cfg.output_file {
            args.output_file = s.clone();
        }
    }
    if !arg_sources.is_explicit("output_file_format") {
        if let Some(ref s) = cfg.output_file_format {
            if let Some(fmt) = parse_output_file_format(s) {
                args.output_file_format = fmt;
            }
        }
    }
    if !arg_sources.is_explicit("presentation") {
        if let Some(p) = renderer::config_fields::presentation::get(cfg) {
            args.presentation = p.to_string();
        }
    }
    if !arg_sources.is_explicit("osc_host") {
        if let Some(h) = renderer::config_fields::osc_host::get(cfg) {
            args.osc_host = h;
        }
    }
    if !arg_sources.is_explicit("osc_port") {
        if let Some(p) = renderer::config_fields::osc_port::get(cfg) {
            args.osc_port = p;
        }
    }
    if !arg_sources.is_explicit("evaluation_polar_azimuth_resolution") {
        if let Some(v) = renderer::config_fields::vbap_azimuth_resolution::get(cfg) {
            args.evaluation_polar_azimuth_resolution = v;
        }
    }
    if !arg_sources.is_explicit("evaluation_polar_elevation_resolution") {
        if let Some(v) = renderer::config_fields::vbap_elevation_resolution::get(cfg) {
            args.evaluation_polar_elevation_resolution = v;
        }
    }
    if !arg_sources.is_explicit("vbap_spread") {
        if let Some(v) = renderer::config_fields::vbap_spread::get(cfg) {
            args.vbap_spread = v;
        }
    }
    if !arg_sources.is_explicit("evaluation_polar_distance_res") {
        if let Some(v) = renderer::config_fields::vbap_distance_res::get(cfg) {
            args.evaluation_polar_distance_res = v;
        }
    }
    if !arg_sources.is_explicit("evaluation_polar_distance_max") {
        if let Some(v) = renderer::config_fields::vbap_distance_max::get(cfg) {
            args.evaluation_polar_distance_max = v;
        }
    }
    if !arg_sources.is_explicit("render_evaluation_position_interpolation")
        && !arg_sources.is_explicit("no_render_evaluation_position_interpolation")
    {
        args.render_evaluation_position_interpolation =
            renderer::config_fields::render_evaluation_position_interpolation::get(cfg).unwrap_or(
                renderer::config_fields::render_evaluation_position_interpolation::DEFAULT,
            );
    } else if args.no_render_evaluation_position_interpolation {
        args.render_evaluation_position_interpolation = false;
    }
    if !arg_sources.is_explicit("render_evaluation_mode") {
        if let Some(ref v) = cfg.render_evaluation_mode {
            if v.eq_ignore_ascii_case("precomputed_cartesian")
                || v.eq_ignore_ascii_case("cartesian")
            {
                args.render_evaluation_mode = EvaluationModeArg::Cartesian;
            } else if v.eq_ignore_ascii_case("precomputed_polar") || v.eq_ignore_ascii_case("polar")
            {
                args.render_evaluation_mode = EvaluationModeArg::Polar;
            }
        }
    }
    if args.evaluation_cartesian_x_size.is_none() {
        args.evaluation_cartesian_x_size = cfg.evaluation_cartesian_x_size;
    }
    if args.evaluation_cartesian_y_size.is_none() {
        args.evaluation_cartesian_y_size = cfg.evaluation_cartesian_y_size;
    }
    if args.evaluation_cartesian_z_size.is_none() {
        args.evaluation_cartesian_z_size = cfg.evaluation_cartesian_z_size;
    }
    if args.evaluation_cartesian_z_neg_size.is_none() {
        args.evaluation_cartesian_z_neg_size = cfg.evaluation_cartesian_z_neg_size;
    }
    if !arg_sources.is_explicit("vbap_allow_negative_z")
        && !arg_sources.is_explicit("no_vbap_allow_negative_z")
    {
        match cfg.vbap_allow_negative_z {
            Some(true) => args.vbap_allow_negative_z = true,
            Some(false) => args.no_vbap_allow_negative_z = true,
            None => {}
        }
    }
    if !arg_sources.is_explicit("vbap_distance_model") {
        if let Some(v) = renderer::config_fields::vbap_distance_model::get(cfg) {
            args.vbap_distance_model = v;
        }
    }
    if !arg_sources.is_explicit("master_gain") {
        if let Some(v) = renderer::config_fields::master_gain::get(cfg) {
            args.master_gain = v;
        }
    }
    if !arg_sources.is_explicit("room_ratio") {
        if let Some(ref v) = cfg.room_ratio {
            args.room_ratio = v.clone();
        }
    }
    if args.room_ratio_rear.is_none() {
        args.room_ratio_rear = cfg.room_ratio_rear;
    }
    if args.room_ratio_lower.is_none() {
        args.room_ratio_lower = cfg.room_ratio_lower;
    }
    if args.room_ratio_center_blend.is_none() {
        args.room_ratio_center_blend = cfg.room_ratio_center_blend;
    }
    if !arg_sources.is_explicit("spread_distance_range") {
        if let Some(v) = renderer::config_fields::spread_distance_range::get(cfg) {
            args.spread_distance_range = v;
        }
    }
    if !arg_sources.is_explicit("spread_distance_curve") {
        if let Some(v) = renderer::config_fields::spread_distance_curve::get(cfg) {
            args.spread_distance_curve = v;
        }
    }
    if !arg_sources.is_explicit("vbap_spread_min") {
        if let Some(v) = renderer::config_fields::vbap_spread_min::get(cfg) {
            args.vbap_spread_min = v;
        }
    }
    if !arg_sources.is_explicit("vbap_spread_max") {
        if let Some(v) = renderer::config_fields::vbap_spread_max::get(cfg) {
            args.vbap_spread_max = v;
        }
    }

    // Platform-specific Option fields
    #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
    if args.latency_target_ms.is_none() {
        args.latency_target_ms = cfg.latency_target;
    }
    #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
    if args.output_device.is_none() {
        if let Some(ref s) = cfg.output_device {
            args.output_device = Some(s.clone());
        }
    }

    // --- Bool fields: CLI enable/disable flags override config; absent → use config ---
    // enable_vbap
    if !arg_sources.is_explicit("enable_vbap") && !arg_sources.is_explicit("disable_vbap") {
        args.enable_vbap = renderer::config_fields::enable_vbap::get(cfg)
            .unwrap_or(renderer::config_fields::enable_vbap::DEFAULT);
    } else if args.disable_vbap {
        args.enable_vbap = false;
    }
    // osc
    if !arg_sources.is_explicit("osc") && !arg_sources.is_explicit("no_osc") {
        args.osc =
            renderer::config_fields::osc::get(cfg).unwrap_or(renderer::config_fields::osc::DEFAULT);
    } else if args.no_osc {
        args.osc = false;
    }
    // osc_metering (was never read from config → the flag had no effect)
    if !arg_sources.is_explicit("osc_metering") && !arg_sources.is_explicit("no_osc_metering") {
        args.osc_metering = renderer::config_fields::osc_metering::get(cfg)
            .unwrap_or(renderer::config_fields::osc_metering::DEFAULT);
    } else if args.no_osc_metering {
        args.osc_metering = false;
    }
    // osc_rx_port (config can override the default 9000)
    if !arg_sources.is_explicit("osc_rx_port") {
        if let Some(p) = renderer::config_fields::osc_rx_port::get(cfg) {
            args.osc_rx_port = p;
        }
    }
    // continuous
    if !arg_sources.is_explicit("continuous") && !arg_sources.is_explicit("no_continuous") {
        args.continuous = renderer::config_fields::continuous::get(cfg)
            .unwrap_or(renderer::config_fields::continuous::DEFAULT);
    } else if args.no_continuous {
        args.continuous = false;
    }
    // use_loudness
    if !arg_sources.is_explicit("use_loudness") && !arg_sources.is_explicit("no_loudness") {
        args.use_loudness = renderer::config_fields::use_loudness::get(cfg)
            .unwrap_or(renderer::config_fields::use_loudness::DEFAULT);
    } else if args.no_loudness {
        args.use_loudness = false;
    }
    // auto_gain
    if !arg_sources.is_explicit("auto_gain") && !arg_sources.is_explicit("no_auto_gain") {
        args.auto_gain = renderer::config_fields::auto_gain::get(cfg)
            .unwrap_or(renderer::config_fields::auto_gain::DEFAULT);
    } else if args.no_auto_gain {
        args.auto_gain = false;
    }
    // bed_conform
    if !arg_sources.is_explicit("bed_conform") && !arg_sources.is_explicit("no_bed_conform") {
        args.bed_conform = renderer::config_fields::bed_conform::get(cfg)
            .unwrap_or(renderer::config_fields::bed_conform::DEFAULT);
    } else if args.no_bed_conform {
        args.bed_conform = false;
    }
    // enable_adaptive_resampling
    if !arg_sources.is_explicit("enable_adaptive_resampling")
        && !arg_sources.is_explicit("disable_adaptive_resampling")
    {
        args.enable_adaptive_resampling =
            renderer::config_fields::enable_adaptive_resampling::get(cfg)
                .unwrap_or(renderer::config_fields::enable_adaptive_resampling::DEFAULT);
    } else if args.disable_adaptive_resampling {
        args.enable_adaptive_resampling = false;
    }
    // The file/FIFO/stdout backend has no device clock to track, so adaptive
    // resampling is meaningless there — force it off (warn if it was asked for).
    if args.output_backend == Some(OutputBackend::File) && args.enable_adaptive_resampling {
        if arg_sources.is_explicit("enable_adaptive_resampling") {
            log::warn!(
                "Adaptive resampling has no effect with --output-backend file; ignoring it."
            );
        }
        args.enable_adaptive_resampling = false;
    }
    // spread_from_distance
    if !arg_sources.is_explicit("spread_from_distance")
        && !arg_sources.is_explicit("no_spread_from_distance")
    {
        args.spread_from_distance = renderer::config_fields::spread_from_distance::get(cfg)
            .unwrap_or(renderer::config_fields::spread_from_distance::DEFAULT);
    } else if args.no_spread_from_distance {
        args.spread_from_distance = false;
    }
    // distance_diffuse (bool flag — no --no- override needed, just the flag)
    if !arg_sources.is_explicit("distance_diffuse") {
        args.distance_diffuse = renderer::config_fields::distance_diffuse::get(cfg)
            .unwrap_or(renderer::config_fields::distance_diffuse::DEFAULT);
    }
    if args.no_vbap_allow_negative_z {
        args.vbap_allow_negative_z = false;
    }
    if !arg_sources.is_explicit("distance_diffuse_threshold") {
        if let Some(v) = renderer::config_fields::distance_diffuse_threshold::get(cfg) {
            args.distance_diffuse_threshold = v;
        }
    }
    if !arg_sources.is_explicit("distance_diffuse_curve") {
        if let Some(v) = renderer::config_fields::distance_diffuse_curve::get(cfg) {
            args.distance_diffuse_curve = v;
        }
    }
}

pub(super) fn effective_to_config(
    args: &RenderArgs,
    cli: &Cli,
    existing_render_cfg: Option<&renderer::config::RenderConfig>,
) -> Result<renderer::config::Config> {
    use renderer::config::{Config, GlobalConfig};
    use renderer::speaker_layout::SpeakerLayout;

    let global = GlobalConfig {
        loglevel: if cli.loglevel != LogLevel::default() {
            Some(format!("{:?}", cli.loglevel).to_lowercase())
        } else {
            None
        },
        log_format: if cli.log_format != LogFormat::default() {
            Some(format!("{:?}", cli.log_format).to_lowercase())
        } else {
            None
        },
        extra: Default::default(),
    };

    // Start from the existing config so every field the CLI cannot express
    // (live_input, embedded current_layout, render_backend, hybrid_*,
    // experimental_*, distance metrics, adaptive tuning, DRC, monitoring
    // cadences, size_to_spread, and any unknown `extra` keys) is preserved
    // verbatim instead of being erased. `merge_render_config` has already
    // folded the on-disk config into `args`, so re-storing the args value
    // re-persists anything the user did not explicitly override on the CLI.
    let mut render = existing_render_cfg.cloned().unwrap_or_default();

    render.input_pipe = if args.continuous {
        args.input.clone()
    } else {
        None
    };
    render.output_backend = match args.output_backend {
        Some(value) if Some(value) != OutputBackend::platform_default() => {
            Some(format!("{:?}", value).to_lowercase())
        }
        _ => None,
    };
    // Persist file-backend destination/format only when non-default, mirroring
    // the skip-if-default policy used for `output_backend` above.
    render.output_file = (args.output_file != "-").then(|| args.output_file.clone());
    render.output_file_format = (args.output_file_format != OutputFileFormatArg::RawF32)
        .then(|| output_file_format_str(args.output_file_format).to_string());
    renderer::config_fields::presentation::store(&mut render, &args.presentation);
    render.bridge_path = args.bridge_path.clone();
    renderer::config_fields::enable_vbap::store(&mut render, args.enable_vbap);
    // Persist the embedded layout instead of a path link. Only override when a
    // layout path is supplied on the CLI; otherwise keep the config's existing
    // embedded `current_layout` (Studio-saved) intact.
    if let Some(ref layout_path) = args.speaker_layout {
        render.current_layout = Some(SpeakerLayout::from_file(layout_path)?);
        render.speaker_layout = None;
    }
    render.vbap_table = args.vbap_table.clone();
    renderer::config_fields::vbap_azimuth_resolution::store(
        &mut render,
        args.evaluation_polar_azimuth_resolution,
    );
    renderer::config_fields::vbap_elevation_resolution::store(
        &mut render,
        args.evaluation_polar_elevation_resolution,
    );
    renderer::config_fields::vbap_spread::store(&mut render, args.vbap_spread);
    renderer::config_fields::vbap_distance_res::store(
        &mut render,
        args.evaluation_polar_distance_res,
    );
    renderer::config_fields::vbap_distance_max::store(
        &mut render,
        args.evaluation_polar_distance_max,
    );
    renderer::config_fields::render_evaluation_position_interpolation::store(
        &mut render,
        args.render_evaluation_position_interpolation,
    );
    render.render_evaluation_mode = if args.render_evaluation_mode != EvaluationModeArg::Polar {
        Some(format!("{:?}", args.render_evaluation_mode).to_lowercase())
    } else {
        None
    };
    render.evaluation_cartesian_x_size = args.evaluation_cartesian_x_size;
    render.evaluation_cartesian_y_size = args.evaluation_cartesian_y_size;
    render.evaluation_cartesian_z_size = args.evaluation_cartesian_z_size;
    render.evaluation_cartesian_z_neg_size = args.evaluation_cartesian_z_neg_size;
    render.vbap_allow_negative_z = if args.vbap_allow_negative_z {
        Some(true)
    } else if args.no_vbap_allow_negative_z {
        Some(false)
    } else {
        None
    };
    renderer::config_fields::vbap_distance_model::store(&mut render, &args.vbap_distance_model);
    renderer::config_fields::master_gain::store(&mut render, args.master_gain);
    // The CLI works in ratios; metres are a save-time representation only, so
    // clear any metre fields a prior Studio save left behind to keep the ratio
    // representation authoritative on the next load.
    render.room_width_m = None;
    render.room_front_m = None;
    render.room_rear_m = None;
    render.room_height_m = None;
    render.room_lower_m = None;
    render.room_ratio = if args.room_ratio != "1.0,2.0,1.0" {
        Some(args.room_ratio.clone())
    } else {
        None
    };
    render.room_ratio_rear = args.room_ratio_rear;
    render.room_ratio_lower = args.room_ratio_lower;
    render.room_ratio_center_blend = args.room_ratio_center_blend;
    renderer::config_fields::osc::store(&mut render, args.osc);
    renderer::config_fields::osc_metering::store(&mut render, args.osc_metering);
    renderer::config_fields::osc_rx_port::store(&mut render, args.osc_rx_port);
    renderer::config_fields::osc_host::store(&mut render, &args.osc_host);
    renderer::config_fields::osc_port::store(&mut render, args.osc_port);
    #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
    {
        render.output_device = args.output_device.clone();
        render.latency_target = args.latency_target_ms;
    }
    renderer::config_fields::continuous::store(&mut render, args.continuous);
    renderer::config_fields::use_loudness::store(&mut render, args.use_loudness);
    renderer::config_fields::auto_gain::store(&mut render, args.auto_gain);
    renderer::config_fields::bed_conform::store(&mut render, args.bed_conform);
    renderer::config_fields::spread_from_distance::store(&mut render, args.spread_from_distance);
    renderer::config_fields::spread_distance_range::store(&mut render, args.spread_distance_range);
    renderer::config_fields::spread_distance_curve::store(&mut render, args.spread_distance_curve);
    renderer::config_fields::vbap_spread_min::store(&mut render, args.vbap_spread_min);
    renderer::config_fields::vbap_spread_max::store(&mut render, args.vbap_spread_max);
    renderer::config_fields::enable_adaptive_resampling::store(
        &mut render,
        args.enable_adaptive_resampling,
    );
    render.adaptive_resampling_update_interval_callbacks =
        args.adaptive_resampling_update_interval_callbacks;
    render.output_sample_rate = args.output_sample_rate;
    renderer::config_fields::ramp_mode::store(
        &mut render,
        match args.ramp_mode {
            RampModeArg::Off => "off",
            RampModeArg::Frame => "frame",
            RampModeArg::Sample => "sample",
            RampModeArg::Interp => "interp",
        },
    );
    render.channel_render_mode = None;
    renderer::config_fields::surround_placement::store(&mut render, args.surround_placement.into());
    renderer::config_fields::distance_diffuse::store(&mut render, args.distance_diffuse);
    renderer::config_fields::distance_diffuse_threshold::store(
        &mut render,
        args.distance_diffuse_threshold,
    );
    renderer::config_fields::distance_diffuse_curve::store(
        &mut render,
        args.distance_diffuse_curve,
    );
    // Backend selection, backend-specific params, distance metrics and
    // size-to-spread: override-only fields shared with the runtime path.
    apply_render_cfg_overrides(&mut render, args);

    let global_opt = if global.loglevel.is_none() && global.log_format.is_none() {
        None
    } else {
        Some(global)
    };

    Ok(Config {
        global: global_opt,
        render: Some(render),
        extra: Default::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::effective_to_config;
    use crate::cli::command::{Commands, ParsedCli};

    /// Build a fully-defaulted `render` arg set + `Cli` straight from clap, so
    /// the test exercises the real defaults rather than a hand-rolled struct.
    fn default_render_invocation() -> (crate::cli::command::Cli, crate::cli::command::RenderArgs) {
        let parsed = ParsedCli::parse_from(["orender", "render"]).expect("parse defaults");
        let cli = parsed.cli;
        let args = match cli.command {
            Commands::Render(ref a) => a.clone(),
            _ => unreachable!("explicit render subcommand"),
        };
        (cli, args)
    }

    /// Regression guard for the report's §8.2: `--save-config` must NOT erase
    /// fields the CLI cannot express. Previously `effective_to_config` rebuilt
    /// the config from scratch and dropped these; now it mutates the existing
    /// config, so they survive a save with no `--speaker-layout`.
    #[test]
    fn save_config_preserves_cli_unexpressible_fields() {
        let (cli, args) = default_render_invocation();

        let existing = renderer::config::RenderConfig {
            live_input: Some(renderer::config::LiveInputConfig {
                node: Some("omniphony_live".to_string()),
                ..Default::default()
            }),
            input_mode: Some(renderer::config::InputModeConfig::Live),
            current_layout: Some(renderer::speaker_layout::SpeakerLayout {
                radius_m: 1.5,
                speakers: vec![],
            }),
            hybrid_external_backend: Some("cube".to_string()),
            experimental_distance_min_active_speakers: Some(3),
            distance_model_metric: Some("chebyshev".to_string()),
            render_backend: Some("barycenter".to_string()),
            ..Default::default()
        };

        let out = effective_to_config(&args, &cli, Some(&existing)).expect("build config");
        let render = out.render.expect("render section present");

        assert!(render.live_input.is_some(), "live_input erased");
        assert_eq!(
            render.input_mode,
            Some(renderer::config::InputModeConfig::Live)
        );
        assert_eq!(
            render.current_layout.map(|l| l.radius_m),
            Some(1.5),
            "embedded current_layout erased"
        );
        assert_eq!(render.hybrid_external_backend.as_deref(), Some("cube"));
        assert_eq!(render.experimental_distance_min_active_speakers, Some(3));
        assert_eq!(render.distance_model_metric.as_deref(), Some("chebyshev"));
        assert_eq!(render.render_backend.as_deref(), Some("barycenter"));
    }

    /// A CLI value still wins and is persisted (skip-if-default via the
    /// descriptor) even when starting from an existing config.
    #[test]
    fn save_config_persists_pilot_field_from_args() {
        let (cli, mut args) = default_render_invocation();
        args.evaluation_polar_distance_res = 12;

        let out = effective_to_config(&args, &cli, None).expect("build config");
        assert_eq!(out.render.unwrap().vbap_distance_res, Some(12));
    }

    /// The `file` output backend destination + format survive a save → load
    /// round-trip: `effective_to_config` persists them, `merge_render_config`
    /// reads them back into a fresh (un-overridden) arg set.
    #[test]
    fn file_backend_args_round_trip_through_config() {
        use crate::cli::command::{OutputBackend, OutputFileFormatArg};

        // Save direction.
        let parsed = ParsedCli::parse_from([
            "orender",
            "render",
            "--output-backend",
            "file",
            "--output-file",
            "/tmp/out.caf",
            "--output-file-format",
            "caf",
        ])
        .expect("parse file-backend args");
        let cli = parsed.cli.clone();
        let args = match cli.command {
            Commands::Render(ref a) => a.clone(),
            _ => unreachable!("render subcommand"),
        };
        let out = effective_to_config(&args, &cli, None).expect("build config");
        let render = out.render.expect("render section present");
        assert_eq!(render.output_backend.as_deref(), Some("file"));
        assert_eq!(render.output_file.as_deref(), Some("/tmp/out.caf"));
        assert_eq!(render.output_file_format.as_deref(), Some("caf"));

        // Load direction: merge into fresh defaults with no CLI overrides.
        let defaults = ParsedCli::parse_from(["orender", "render"]).expect("parse defaults");
        let mut merged = match defaults.cli.command {
            Commands::Render(ref a) => a.clone(),
            _ => unreachable!("render subcommand"),
        };
        super::merge_render_config(&render, &mut merged, &defaults.render_sources());
        assert_eq!(merged.output_backend, Some(OutputBackend::File));
        assert_eq!(merged.output_file, "/tmp/out.caf");
        assert_eq!(merged.output_file_format, OutputFileFormatArg::Caf);
    }
}
