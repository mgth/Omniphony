use crate::cli::command::{
    Cli, LogFormat, LogLevel, OutputBackend, RampModeArg, RenderArgSources, RenderArgs,
    VbapTableModeArg,
};
use anyhow::Result;

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
        if let Some(ref v) = cfg.ramp_mode {
            if let Some(mode) = renderer::live_params::RampMode::from_str(v) {
                args.ramp_mode = match mode {
                    renderer::live_params::RampMode::Off => RampModeArg::Off,
                    renderer::live_params::RampMode::Frame => RampModeArg::Frame,
                    renderer::live_params::RampMode::Sample => RampModeArg::Sample,
                };
            }
        }
    }
    if args.bridge_path.is_none() {
        args.bridge_path = cfg.bridge_path.clone();
    }
    // --- Fields with defaults: apply config only when value equals the clap default ---
    // (If the user explicitly passes the default value, config is ignored — acceptable edge case.)
    if !arg_sources.is_explicit("output_backend") {
        if let Some(ref s) = cfg.output_backend {
            if let Ok(f) = OutputBackend::from_str(s) {
                args.output_backend = Some(f);
            }
        }
    }
    if !arg_sources.is_explicit("presentation") {
        if let Some(p) = cfg.presentation {
            args.presentation = p.to_string();
        }
    }
    if !arg_sources.is_explicit("osc_host") {
        if let Some(ref h) = cfg.osc_host {
            args.osc_host = h.clone();
        }
    }
    if !arg_sources.is_explicit("osc_port") {
        if let Some(p) = cfg.osc_port {
            args.osc_port = p;
        }
    }
    if !arg_sources.is_explicit("vbap_azimuth_resolution") {
        if let Some(v) = cfg.vbap_azimuth_resolution {
            args.vbap_azimuth_resolution = v;
        }
    }
    if !arg_sources.is_explicit("vbap_elevation_resolution") {
        if let Some(v) = cfg.vbap_elevation_resolution {
            args.vbap_elevation_resolution = v;
        }
    }
    if !arg_sources.is_explicit("vbap_spread") {
        if let Some(v) = cfg.vbap_spread {
            args.vbap_spread = v;
        }
    }
    if !arg_sources.is_explicit("vbap_distance_res") {
        if let Some(v) = cfg.vbap_distance_res {
            args.vbap_distance_res = v;
        }
    }
    if !arg_sources.is_explicit("vbap_distance_max") {
        if let Some(v) = cfg.vbap_distance_max {
            args.vbap_distance_max = v;
        }
    }
    if !arg_sources.is_explicit("vbap_position_interpolation")
        && !arg_sources.is_explicit("no_vbap_position_interpolation")
    {
        args.vbap_position_interpolation = cfg.vbap_position_interpolation.unwrap_or(true);
    } else if args.no_vbap_position_interpolation {
        args.vbap_position_interpolation = false;
    }
    if !arg_sources.is_explicit("vbap_table_mode") {
        if let Some(ref v) = cfg.vbap_table_mode {
            args.vbap_table_mode = if v.eq_ignore_ascii_case("cartesian") {
                VbapTableModeArg::Cartesian
            } else {
                VbapTableModeArg::Polar
            };
        }
    }
    if args.vbap_cart_x_size.is_none() {
        args.vbap_cart_x_size = cfg.vbap_cart_x_size;
    }
    if args.vbap_cart_y_size.is_none() {
        args.vbap_cart_y_size = cfg.vbap_cart_y_size;
    }
    if args.vbap_cart_z_size.is_none() {
        args.vbap_cart_z_size = cfg.vbap_cart_z_size;
    }
    if args.vbap_cart_z_neg_size.is_none() {
        args.vbap_cart_z_neg_size = cfg.vbap_cart_z_neg_size;
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
        if let Some(ref v) = cfg.vbap_distance_model {
            args.vbap_distance_model = v.clone();
        }
    }
    if !arg_sources.is_explicit("master_gain") {
        if let Some(v) = cfg.master_gain {
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
        if let Some(v) = cfg.spread_distance_range {
            args.spread_distance_range = v;
        }
    }
    if !arg_sources.is_explicit("spread_distance_curve") {
        if let Some(v) = cfg.spread_distance_curve {
            args.spread_distance_curve = v;
        }
    }
    if !arg_sources.is_explicit("vbap_spread_min") {
        if let Some(v) = cfg.vbap_spread_min {
            args.vbap_spread_min = v;
        }
    }
    if !arg_sources.is_explicit("vbap_spread_max") {
        if let Some(v) = cfg.vbap_spread_max {
            args.vbap_spread_max = v;
        }
    }

    // Platform-specific Option fields
    #[cfg(target_os = "linux")]
    if args.pw_latency.is_none() {
        args.pw_latency = cfg.pw_latency;
    }
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    if args.output_device.is_none() {
        if let Some(ref s) = cfg.output_device {
            args.output_device = Some(s.clone());
        }
    }

    // --- Bool fields: CLI enable/disable flags override config; absent → use config ---
    // enable_vbap
    if !arg_sources.is_explicit("enable_vbap") && !arg_sources.is_explicit("disable_vbap") {
        args.enable_vbap = cfg.enable_vbap.unwrap_or(false);
    } else if args.disable_vbap {
        args.enable_vbap = false;
    }
    // osc
    if !arg_sources.is_explicit("osc") && !arg_sources.is_explicit("no_osc") {
        args.osc = cfg.osc.unwrap_or(false);
    } else if args.no_osc {
        args.osc = false;
    }
    // osc_rx_port (config can override the default 9000)
    if !arg_sources.is_explicit("osc_rx_port") {
        if let Some(p) = cfg.osc_rx_port {
            args.osc_rx_port = p;
        }
    }
    // continuous
    if !arg_sources.is_explicit("continuous") && !arg_sources.is_explicit("no_continuous") {
        args.continuous = cfg.continuous.unwrap_or(false);
    } else if args.no_continuous {
        args.continuous = false;
    }
    // use_loudness
    if !arg_sources.is_explicit("use_loudness") && !arg_sources.is_explicit("no_loudness") {
        args.use_loudness = cfg.use_loudness.unwrap_or(false);
    } else if args.no_loudness {
        args.use_loudness = false;
    }
    // auto_gain
    if !arg_sources.is_explicit("auto_gain") && !arg_sources.is_explicit("no_auto_gain") {
        args.auto_gain = cfg.auto_gain.unwrap_or(false);
    } else if args.no_auto_gain {
        args.auto_gain = false;
    }
    // bed_conform
    if !arg_sources.is_explicit("bed_conform") && !arg_sources.is_explicit("no_bed_conform") {
        args.bed_conform = cfg.bed_conform.unwrap_or(false);
    } else if args.no_bed_conform {
        args.bed_conform = false;
    }
    // enable_adaptive_resampling
    if !arg_sources.is_explicit("enable_adaptive_resampling")
        && !arg_sources.is_explicit("disable_adaptive_resampling")
    {
        args.enable_adaptive_resampling = cfg.enable_adaptive_resampling.unwrap_or(false);
    } else if args.disable_adaptive_resampling {
        args.enable_adaptive_resampling = false;
    }
    // spread_from_distance
    if !arg_sources.is_explicit("spread_from_distance")
        && !arg_sources.is_explicit("no_spread_from_distance")
    {
        args.spread_from_distance = cfg.spread_from_distance.unwrap_or(false);
    } else if args.no_spread_from_distance {
        args.spread_from_distance = false;
    }
    // distance_diffuse (bool flag — no --no- override needed, just the flag)
    if !arg_sources.is_explicit("distance_diffuse") {
        args.distance_diffuse = cfg.distance_diffuse.unwrap_or(false);
    }
    if args.no_vbap_allow_negative_z {
        args.vbap_allow_negative_z = false;
    }
    if !arg_sources.is_explicit("distance_diffuse_threshold") {
        if let Some(v) = cfg.distance_diffuse_threshold {
            args.distance_diffuse_threshold = v;
        }
    }
    if !arg_sources.is_explicit("distance_diffuse_curve") {
        if let Some(v) = cfg.distance_diffuse_curve {
            args.distance_diffuse_curve = v;
        }
    }
}

pub(super) fn effective_to_config(args: &RenderArgs, cli: &Cli) -> Result<renderer::config::Config> {
    use renderer::config::{Config, GlobalConfig, RenderConfig};
    use renderer::speaker_layout::SpeakerLayout;

    let current_layout = if let Some(ref layout_path) = args.speaker_layout {
        Some(SpeakerLayout::from_file(layout_path)?)
    } else {
        None
    };

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
        strict: if cli.strict { Some(true) } else { None },
    };

    let render = RenderConfig {
        output_backend: match args.output_backend {
            Some(value) if Some(value) != OutputBackend::platform_default() => {
                Some(format!("{:?}", value).to_lowercase())
            }
            _ => None,
        },
        presentation: if args.presentation != "best" {
            args.presentation.parse::<u8>().ok()
        } else {
            None
        },
        bridge_path: args.bridge_path.clone(),
        enable_vbap: if args.enable_vbap { Some(true) } else { None },
        // Persist embedded layout instead of path link.
        speaker_layout: None,
        current_layout,
        vbap_table: args.vbap_table.clone(),
        vbap_azimuth_resolution: if args.vbap_azimuth_resolution != 360 {
            Some(args.vbap_azimuth_resolution)
        } else {
            None
        },
        vbap_elevation_resolution: if args.vbap_elevation_resolution != 180 {
            Some(args.vbap_elevation_resolution)
        } else {
            None
        },
        vbap_spread: if args.vbap_spread != 0.0 {
            Some(args.vbap_spread)
        } else {
            None
        },
        vbap_distance_res: if args.vbap_distance_res != 8 {
            Some(args.vbap_distance_res)
        } else {
            None
        },
        vbap_distance_max: if (args.vbap_distance_max - 2.0).abs() > f32::EPSILON {
            Some(args.vbap_distance_max)
        } else {
            None
        },
        vbap_position_interpolation: if args.vbap_position_interpolation {
            None
        } else {
            Some(false)
        },
        vbap_table_mode: if args.vbap_table_mode != VbapTableModeArg::Polar {
            Some(format!("{:?}", args.vbap_table_mode).to_lowercase())
        } else {
            None
        },
        vbap_cart_x_size: args.vbap_cart_x_size,
        vbap_cart_y_size: args.vbap_cart_y_size,
        vbap_cart_z_size: args.vbap_cart_z_size,
        vbap_cart_z_neg_size: args.vbap_cart_z_neg_size,
        vbap_allow_negative_z: if args.vbap_allow_negative_z {
            Some(true)
        } else if args.no_vbap_allow_negative_z {
            Some(false)
        } else {
            None
        },
        vbap_distance_model: if args.vbap_distance_model != "none" {
            Some(args.vbap_distance_model.clone())
        } else {
            None
        },
        master_gain: if args.master_gain != 0.0 {
            Some(args.master_gain)
        } else {
            None
        },
        room_ratio: if args.room_ratio != "1.0,2.0,1.0" {
            Some(args.room_ratio.clone())
        } else {
            None
        },
        room_ratio_rear: args.room_ratio_rear,
        room_ratio_lower: args.room_ratio_lower,
        room_ratio_center_blend: args.room_ratio_center_blend,
        osc: if args.osc { Some(true) } else { None },
        osc_metering: if args.osc_metering { Some(true) } else { None },
        osc_rx_port: if args.osc_rx_port != 9000 {
            Some(args.osc_rx_port)
        } else {
            None
        },
        osc_host: if args.osc_host != "127.0.0.1" {
            Some(args.osc_host.clone())
        } else {
            None
        },
        osc_port: if args.osc_port != 9000 {
            Some(args.osc_port)
        } else {
            None
        },
        output_device: {
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            {
                args.output_device.clone()
            }
            #[cfg(not(any(target_os = "linux", target_os = "windows")))]
            {
                None
            }
        },
        pw_latency: {
            #[cfg(target_os = "linux")]
            {
                args.pw_latency
            }
            #[cfg(not(target_os = "linux"))]
            {
                None
            }
        },
        continuous: if args.continuous { Some(true) } else { None },
        use_loudness: if args.use_loudness { Some(true) } else { None },
        auto_gain: if args.auto_gain { Some(true) } else { None },
        bed_conform: if args.bed_conform { Some(true) } else { None },
        spread_from_distance: if args.spread_from_distance {
            Some(true)
        } else {
            None
        },
        spread_distance_range: if args.spread_distance_range != 1.0 {
            Some(args.spread_distance_range)
        } else {
            None
        },
        spread_distance_curve: if args.spread_distance_curve != 1.0 {
            Some(args.spread_distance_curve)
        } else {
            None
        },
        vbap_spread_min: if args.vbap_spread_min != 0.0 {
            Some(args.vbap_spread_min)
        } else {
            None
        },
        vbap_spread_max: if args.vbap_spread_max != 1.0 {
            Some(args.vbap_spread_max)
        } else {
            None
        },
        enable_adaptive_resampling: if args.enable_adaptive_resampling {
            Some(true)
        } else {
            None
        },
        adaptive_resampling_enable_far_mode: None,
        adaptive_resampling_force_silence_in_far_mode: None,
        adaptive_resampling_hard_recover_in_far_mode: None,
        adaptive_resampling_far_mode_return_fade_in_ms: None,
        adaptive_resampling_kp_near: None,
        adaptive_resampling_kp_far: None,
        adaptive_resampling_ki: None,
        adaptive_resampling_max_adjust: None,
        adaptive_resampling_max_adjust_far: None,
        adaptive_resampling_update_interval_callbacks: args
            .adaptive_resampling_update_interval_callbacks,
        adaptive_resampling_near_far_threshold_ms: None,
        adaptive_resampling_measurement_smoothing_alpha: None,
        output_sample_rate: args.output_sample_rate,
        ramp_mode: if args.ramp_mode != RampModeArg::Sample {
            Some(match args.ramp_mode {
                RampModeArg::Off => "off".to_string(),
                RampModeArg::Frame => "frame".to_string(),
                RampModeArg::Sample => "sample".to_string(),
            })
        } else {
            None
        },
        distance_diffuse: if args.distance_diffuse {
            Some(true)
        } else {
            None
        },
        distance_diffuse_threshold: if args.distance_diffuse_threshold != 1.0 {
            Some(args.distance_diffuse_threshold)
        } else {
            None
        },
        distance_diffuse_curve: if args.distance_diffuse_curve != 1.0 {
            Some(args.distance_diffuse_curve)
        } else {
            None
        },
    };

    let global_opt =
        if global.loglevel.is_none() && global.log_format.is_none() && global.strict.is_none() {
            None
        } else {
            Some(global)
        };

    Ok(Config {
        global: global_opt,
        render: Some(render),
    })
}
