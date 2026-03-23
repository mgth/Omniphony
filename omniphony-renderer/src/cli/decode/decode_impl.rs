use super::decoder_thread::{DecodedAudioData, DecoderMessage, DecoderThreadConfig, spawn_decoder_thread};
use super::handler::{DecodeHandler, FrameHandlerContext, WriterState};
use crate::bridge_loader::{LoadedBridge, resolve_bridge_path};
use crate::cli::command::{
    Cli, LogFormat, LogLevel, OutputBackend, RampModeArg, RenderArgs, VbapTableModeArg,
};
use anyhow::Result;
#[cfg(target_os = "linux")]
use audio_output::pipewire::{PipewireBufferConfig, list_pipewire_output_devices};
use audio_output::AdaptiveResamplingConfig;
use log::Level;
use renderer::live_params::OutputDeviceOption;
use renderer::metering::AudioMeter;
use std::sync::mpsc;

#[cfg(target_os = "windows")]
fn list_available_output_devices(_backend: OutputBackend) -> Vec<OutputDeviceOption> {
    audio_output::list_asio_devices()
        .unwrap_or_default()
        .into_iter()
        .map(|name| OutputDeviceOption {
            value: name.clone(),
            label: name,
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn list_available_output_devices(backend: OutputBackend) -> Vec<OutputDeviceOption> {
    match backend {
        OutputBackend::Pipewire => list_pipewire_output_devices()
            .unwrap_or_default()
            .into_iter()
            .map(|(value, label)| OutputDeviceOption { value, label })
            .collect(),
        #[allow(unreachable_patterns)]
        _ => Vec::new(),
    }
}

#[cfg(not(any(
    target_os = "windows",
    target_os = "linux"
)))]
fn list_available_output_devices(_backend: OutputBackend) -> Vec<OutputDeviceOption> {
    Vec::new()
}

fn merge_render_config(cfg: &renderer::config::RenderConfig, args: &mut RenderArgs) {
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
    if args.ramp_mode == RampModeArg::Sample {
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
    if args.output_backend.is_none() {
        if let Some(ref s) = cfg.output_backend {
            if let Ok(f) = OutputBackend::from_str(s) {
                args.output_backend = Some(f);
            }
        }
    }
    if args.presentation == "best" {
        if let Some(p) = cfg.presentation {
            args.presentation = p.to_string();
        }
    }
    if args.osc_host == "127.0.0.1" {
        if let Some(ref h) = cfg.osc_host {
            args.osc_host = h.clone();
        }
    }
    if args.osc_port == 9000 {
        if let Some(p) = cfg.osc_port {
            args.osc_port = p;
        }
    }
    if args.vbap_azimuth_resolution == 360 {
        if let Some(v) = cfg.vbap_azimuth_resolution {
            args.vbap_azimuth_resolution = v;
        }
    }
    if args.vbap_elevation_resolution == 180 {
        if let Some(v) = cfg.vbap_elevation_resolution {
            args.vbap_elevation_resolution = v;
        }
    }
    if args.vbap_spread == 0.0 {
        if let Some(v) = cfg.vbap_spread {
            args.vbap_spread = v;
        }
    }
    if args.vbap_distance_res == 8 {
        if let Some(v) = cfg.vbap_distance_res {
            args.vbap_distance_res = v;
        }
    }
    if (args.vbap_distance_max - 2.0).abs() < f32::EPSILON {
        if let Some(v) = cfg.vbap_distance_max {
            args.vbap_distance_max = v;
        }
    }
    if !args.vbap_position_interpolation && !args.no_vbap_position_interpolation {
        args.vbap_position_interpolation = cfg.vbap_position_interpolation.unwrap_or(true);
    } else if args.no_vbap_position_interpolation {
        args.vbap_position_interpolation = false;
    }
    if args.vbap_table_mode == VbapTableModeArg::Polar {
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
    if !args.vbap_allow_negative_z && !args.no_vbap_allow_negative_z {
        match cfg.vbap_allow_negative_z {
            Some(true) => args.vbap_allow_negative_z = true,
            Some(false) => args.no_vbap_allow_negative_z = true,
            None => {}
        }
    }
    if args.vbap_distance_model == "none" {
        if let Some(ref v) = cfg.vbap_distance_model {
            args.vbap_distance_model = v.clone();
        }
    }
    if args.master_gain == 0.0 {
        if let Some(v) = cfg.master_gain {
            args.master_gain = v;
        }
    }
    if args.room_ratio == "1.0,2.0,1.0" {
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
    if args.spread_distance_range == 1.0 {
        if let Some(v) = cfg.spread_distance_range {
            args.spread_distance_range = v;
        }
    }
    if args.spread_distance_curve == 1.0 {
        if let Some(v) = cfg.spread_distance_curve {
            args.spread_distance_curve = v;
        }
    }
    if args.vbap_spread_min == 0.0 {
        if let Some(v) = cfg.vbap_spread_min {
            args.vbap_spread_min = v;
        }
    }
    if args.vbap_spread_max == 1.0 {
        if let Some(v) = cfg.vbap_spread_max {
            args.vbap_spread_max = v;
        }
    }

    // Platform-specific Option fields
    #[cfg(target_os = "linux")]
    if args.pw_latency.is_none() {
        args.pw_latency = cfg.pw_latency;
    }
    #[cfg(any(
        target_os = "linux",
        target_os = "windows"
    ))]
    if args.output_device.is_none() {
        if let Some(ref s) = cfg.output_device {
            args.output_device = Some(s.clone());
        }
    }

    // --- Bool fields: CLI enable/disable flags override config; absent → use config ---
    // enable_vbap
    if !args.enable_vbap && !args.disable_vbap {
        args.enable_vbap = cfg.enable_vbap.unwrap_or(false);
    } else if args.disable_vbap {
        args.enable_vbap = false;
    }
    // osc
    if !args.osc && !args.no_osc {
        args.osc = cfg.osc.unwrap_or(false);
    } else if args.no_osc {
        args.osc = false;
    }
    // osc_rx_port (config can override the default 9000)
    if args.osc_rx_port == 9000 {
        if let Some(p) = cfg.osc_rx_port {
            args.osc_rx_port = p;
        }
    }
    // continuous
    if !args.continuous && !args.no_continuous {
        args.continuous = cfg.continuous.unwrap_or(false);
    } else if args.no_continuous {
        args.continuous = false;
    }
    // use_loudness
    if !args.use_loudness && !args.no_loudness {
        args.use_loudness = cfg.use_loudness.unwrap_or(false);
    } else if args.no_loudness {
        args.use_loudness = false;
    }
    // auto_gain
    if !args.auto_gain && !args.no_auto_gain {
        args.auto_gain = cfg.auto_gain.unwrap_or(false);
    } else if args.no_auto_gain {
        args.auto_gain = false;
    }
    // bed_conform
    if !args.bed_conform && !args.no_bed_conform {
        args.bed_conform = cfg.bed_conform.unwrap_or(false);
    } else if args.no_bed_conform {
        args.bed_conform = false;
    }
    // enable_adaptive_resampling
    if !args.enable_adaptive_resampling && !args.disable_adaptive_resampling {
        args.enable_adaptive_resampling = cfg.enable_adaptive_resampling.unwrap_or(false);
    } else if args.disable_adaptive_resampling {
        args.enable_adaptive_resampling = false;
    }
    // spread_from_distance
    if !args.spread_from_distance && !args.no_spread_from_distance {
        args.spread_from_distance = cfg.spread_from_distance.unwrap_or(false);
    } else if args.no_spread_from_distance {
        args.spread_from_distance = false;
    }
    // distance_diffuse (bool flag — no --no- override needed, just the flag)
    if !args.distance_diffuse {
        args.distance_diffuse = cfg.distance_diffuse.unwrap_or(false);
    }
    if args.no_vbap_allow_negative_z {
        args.vbap_allow_negative_z = false;
    }
    if args.distance_diffuse_threshold == 1.0 {
        if let Some(v) = cfg.distance_diffuse_threshold {
            args.distance_diffuse_threshold = v;
        }
    }
    if args.distance_diffuse_curve == 1.0 {
        if let Some(v) = cfg.distance_diffuse_curve {
            args.distance_diffuse_curve = v;
        }
    }
}

fn effective_to_config(args: &RenderArgs, cli: &Cli) -> Result<renderer::config::Config> {
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
            #[cfg(any(
                target_os = "linux",
                target_os = "windows"
            ))]
            {
                args.output_device.clone()
            }
            #[cfg(not(any(
                target_os = "linux",
                target_os = "windows"
            )))]
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
        adaptive_resampling_update_interval_callbacks:
            args.adaptive_resampling_update_interval_callbacks,
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

struct PreparedDecodeRun {
    state: WriterState,
    rx: mpsc::Receiver<Result<DecoderMessage>>,
    decode_thread: std::thread::JoinHandle<Result<()>>,
    _shutdown: sys::ShutdownHandle,
    _bridge_lib: bridge_api::BridgeLibRef,
    input_path: std::path::PathBuf,
    is_spatial_presentation: bool,
    coordinate_format: bridge_api::RCoordinateFormat,
    vbap_cartesian_defaults: bridge_api::RVbapCartesianDefaults,
    preferred_vbap_table_mode: bridge_api::RVbapTableMode,
}

fn resolve_effective_decode_args(
    args: &RenderArgs,
    cli: &Cli,
) -> (
    Option<std::path::PathBuf>,
    RenderArgs,
    Option<renderer::speaker_layout::SpeakerLayout>,
    bool,
) {
    let config_path = cli
        .config
        .clone()
        .or_else(renderer::config::default_config_path);
    let cfg = config_path
        .as_deref()
        .map(renderer::config::Config::load_or_default)
        .unwrap_or_default();

    let mut effective = args.clone();
    let vbap_table_mode_explicit = args.vbap_table_mode != VbapTableModeArg::Polar
        || cfg
            .render
            .as_ref()
            .and_then(|rc| rc.vbap_table_mode.as_ref())
            .is_some();
    if let Some(rc) = &cfg.render {
        merge_render_config(rc, &mut effective);
    }

    let current_layout = cfg.render.and_then(|rc| rc.current_layout);
    (
        config_path,
        effective,
        current_layout,
        vbap_table_mode_explicit,
    )
}

fn maybe_save_effective_config(
    cli: &Cli,
    args: &RenderArgs,
    config_path: &Option<std::path::PathBuf>,
) -> Result<bool> {
    if !cli.save_config {
        return Ok(false);
    }

    let path = config_path.clone().ok_or_else(|| {
        anyhow::anyhow!("Cannot determine config path; use --config to specify one")
    })?;

    let config = effective_to_config(args, cli)?;
    config.save(&path)?;
    log::info!("Config written to: {}", path.display());
    Ok(true)
}

fn prepare_render_run(args: &RenderArgs, cli: &Cli) -> Result<PreparedDecodeRun> {
    let input = args
        .input
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Must specify INPUT file"))?
        .clone();

    log::info!(
        "Decoding stream from file: {} (strict mode: {}, presentation: {})",
        input.display(),
        cli.strict,
        args.presentation
    );

    let resolved_backend = args
        .output_backend
        .or_else(OutputBackend::platform_default)
        .unwrap_or(OutputBackend::Unsupported);
    if resolved_backend == OutputBackend::Unsupported {
        return Err(anyhow::anyhow!(
            "No realtime audio output backend is compiled in. Enable 'pipewire' or 'asio'."
        ));
    }

    let strict_mode = cli.strict;
    let fail_level = if strict_mode {
        Level::Warn
    } else {
        Level::Error
    };
    let state = WriterState { fail_level };

    // Load the format-bridge plugin (required for spatial audio processing).
    // The bridge owns the full decode pipeline.
    let bridge_path = resolve_bridge_path(args.bridge_path.as_deref())?;
    log::info!("Loading format bridge: {}", bridge_path.display());
    let LoadedBridge { lib, mut bridge } =
        LoadedBridge::load_with_params(&bridge_path, strict_mode)?;
    if !bridge.configure("presentation".into(), args.presentation.as_str().into()) {
        return Err(anyhow::anyhow!(
            "Bridge rejected presentation value '{}'",
            args.presentation
        ));
    }
    let is_spatial_presentation = bridge.is_spatial();
    let coordinate_format = bridge.coordinate_format();
    let vbap_cartesian_defaults = bridge.vbap_cartesian_defaults();
    let preferred_vbap_table_mode = bridge.preferred_vbap_table_mode();
    log::info!("Bridge coordinate format: {:?}", coordinate_format);
    log::info!(
        "Bridge cartesian VBAP defaults: x={}, y={}, z={}, allow_negative_z={}",
        vbap_cartesian_defaults.x_size,
        vbap_cartesian_defaults.y_size,
        vbap_cartesian_defaults.z_size,
        vbap_cartesian_defaults.allow_negative_z
    );
    log::info!(
        "Bridge preferred VBAP table mode: {:?}",
        preferred_vbap_table_mode
    );

    // On Linux, use a bounded channel to prevent memory accumulation when PipeWire can't keep up.
    // Buffer size: 100 frames (~0.08s of audio at 48kHz) prevents OOM while allowing smooth playback.
    let (tx, rx) = mpsc::sync_channel(100);

    // Install signal handlers for graceful daemon shutdown.
    let shutdown = sys::shutdown::ShutdownHandle::install()?;
    let shutdown_signal = shutdown.shutdown_signal();

    // Spawn decoder thread — bridge moves into the thread.
    let decode_thread = spawn_decoder_thread(DecoderThreadConfig {
        input_path: input.clone(),
        strict_mode,
        continuous: args.continuous,
        drain_pipe: !args.no_drain_pipe, // Drain by default unless --no-drain-pipe
        tx,
        bridge,
        shutdown_signal,
    });

    Ok(PreparedDecodeRun {
        state,
        rx,
        decode_thread,
        _shutdown: shutdown,
        _bridge_lib: lib,
        input_path: input,
        is_spatial_presentation,
        coordinate_format,
        vbap_cartesian_defaults,
        preferred_vbap_table_mode,
    })
}

fn init_render_handler(
    handler: &mut DecodeHandler,
    args: &RenderArgs,
    input_path: &std::path::Path,
    config_path: &Option<std::path::PathBuf>,
    current_layout_from_config: Option<renderer::speaker_layout::SpeakerLayout>,
    vbap_cartesian_defaults: bridge_api::RVbapCartesianDefaults,
    preferred_vbap_table_mode: bridge_api::RVbapTableMode,
    vbap_table_mode_explicit: bool,
) -> Result<()> {
    #[cfg(not(feature = "saf_vbap"))]
    let _ = (preferred_vbap_table_mode, vbap_table_mode_explicit);

    // Set PipeWire output target and buffer config (Linux only)
    #[cfg(target_os = "linux")]
    {
        handler.runtime.output_device = args.output_device.clone();
        let defaults = PipewireBufferConfig::default();
        let adaptive_defaults = AdaptiveResamplingConfig::default();
        let render_cfg = config_path
            .as_deref()
            .map(renderer::config::Config::load_or_default)
            .and_then(|cfg| cfg.render);
        let latency_ms = args.pw_latency.unwrap_or(defaults.latency_ms);
        handler.runtime.pw_buffer_config = PipewireBufferConfig {
            latency_ms,
            max_latency_ms: args.pw_max_latency.unwrap_or(latency_ms * 2),
            quantum_frames: args.pw_quantum.unwrap_or(defaults.quantum_frames),
        };
        handler.runtime.adaptive_resampling_config = AdaptiveResamplingConfig {
            enable_far_mode: render_cfg
                .as_ref()
                .and_then(|cfg| cfg.adaptive_resampling_enable_far_mode)
                .unwrap_or(adaptive_defaults.enable_far_mode),
            force_silence_in_far_mode: render_cfg
                .as_ref()
                .and_then(|cfg| cfg.adaptive_resampling_force_silence_in_far_mode)
                .unwrap_or(adaptive_defaults.force_silence_in_far_mode),
            hard_recover_in_far_mode: true,
            far_mode_return_fade_in_ms: render_cfg
                .as_ref()
                .and_then(|cfg| cfg.adaptive_resampling_far_mode_return_fade_in_ms)
                .unwrap_or(adaptive_defaults.far_mode_return_fade_in_ms),
            kp_near: render_cfg
                .as_ref()
                .and_then(|cfg| cfg.adaptive_resampling_kp_near)
                .map(|v| v as f64)
                .unwrap_or(adaptive_defaults.kp_near),
            kp_far: render_cfg
                .as_ref()
                .and_then(|cfg| cfg.adaptive_resampling_kp_near)
                .map(|v| v as f64)
                .unwrap_or(adaptive_defaults.kp_near),
            ki: render_cfg
                .as_ref()
                .and_then(|cfg| cfg.adaptive_resampling_ki)
                .map(|v| v as f64)
                .unwrap_or(adaptive_defaults.ki),
            max_adjust: render_cfg
                .as_ref()
                .and_then(|cfg| cfg.adaptive_resampling_max_adjust)
                .map(|v| v as f64)
                .unwrap_or(adaptive_defaults.max_adjust),
            max_adjust_far: render_cfg
                .as_ref()
                .and_then(|cfg| cfg.adaptive_resampling_max_adjust)
                .map(|v| v as f64)
                .unwrap_or(adaptive_defaults.max_adjust),
            update_interval_callbacks: args
                .adaptive_resampling_update_interval_callbacks
                .or_else(|| {
                    render_cfg
                        .as_ref()
                        .and_then(|cfg| cfg.adaptive_resampling_update_interval_callbacks)
                })
                .unwrap_or(adaptive_defaults.update_interval_callbacks)
                .max(1),
            near_far_threshold_ms: render_cfg
                .as_ref()
                .and_then(|cfg| cfg.adaptive_resampling_near_far_threshold_ms)
                .unwrap_or(adaptive_defaults.near_far_threshold_ms),
            measurement_smoothing_alpha: render_cfg
                .as_ref()
                .and_then(|cfg| cfg.adaptive_resampling_measurement_smoothing_alpha)
                .map(|v| v as f64)
                .unwrap_or(adaptive_defaults.measurement_smoothing_alpha),
        };
    }

    // Set ASIO output device if specified (Windows only)
    #[cfg(target_os = "windows")]
    {
        handler.runtime.output_device = args.output_device.clone();
        let adaptive_defaults = AdaptiveResamplingConfig::default();
        let render_cfg = config_path
            .as_deref()
            .map(renderer::config::Config::load_or_default)
            .and_then(|cfg| cfg.render);
        handler.runtime.adaptive_resampling_config = AdaptiveResamplingConfig {
            enable_far_mode: render_cfg
                .as_ref()
                .and_then(|cfg| cfg.adaptive_resampling_enable_far_mode)
                .unwrap_or(adaptive_defaults.enable_far_mode),
            force_silence_in_far_mode: render_cfg
                .as_ref()
                .and_then(|cfg| cfg.adaptive_resampling_force_silence_in_far_mode)
                .unwrap_or(adaptive_defaults.force_silence_in_far_mode),
            hard_recover_in_far_mode: true,
            far_mode_return_fade_in_ms: render_cfg
                .as_ref()
                .and_then(|cfg| cfg.adaptive_resampling_far_mode_return_fade_in_ms)
                .unwrap_or(adaptive_defaults.far_mode_return_fade_in_ms),
            kp_near: render_cfg
                .as_ref()
                .and_then(|cfg| cfg.adaptive_resampling_kp_near)
                .map(|v| v as f64)
                .unwrap_or(adaptive_defaults.kp_near),
            kp_far: render_cfg
                .as_ref()
                .and_then(|cfg| cfg.adaptive_resampling_kp_near)
                .map(|v| v as f64)
                .unwrap_or(adaptive_defaults.kp_near),
            ki: render_cfg
                .as_ref()
                .and_then(|cfg| cfg.adaptive_resampling_ki)
                .map(|v| v as f64)
                .unwrap_or(adaptive_defaults.ki),
            max_adjust: render_cfg
                .as_ref()
                .and_then(|cfg| cfg.adaptive_resampling_max_adjust)
                .map(|v| v as f64)
                .unwrap_or(adaptive_defaults.max_adjust),
            max_adjust_far: render_cfg
                .as_ref()
                .and_then(|cfg| cfg.adaptive_resampling_max_adjust)
                .map(|v| v as f64)
                .unwrap_or(adaptive_defaults.max_adjust),
            update_interval_callbacks: args
                .adaptive_resampling_update_interval_callbacks
                .or_else(|| {
                    render_cfg
                        .as_ref()
                        .and_then(|cfg| cfg.adaptive_resampling_update_interval_callbacks)
                })
                .unwrap_or(adaptive_defaults.update_interval_callbacks)
                .max(1),
            near_far_threshold_ms: render_cfg
                .as_ref()
                .and_then(|cfg| cfg.adaptive_resampling_near_far_threshold_ms)
                .unwrap_or(adaptive_defaults.near_far_threshold_ms),
            measurement_smoothing_alpha: render_cfg
                .as_ref()
                .and_then(|cfg| cfg.adaptive_resampling_measurement_smoothing_alpha)
                .map(|v| v as f64)
                .unwrap_or(adaptive_defaults.measurement_smoothing_alpha),
        };
    }

    // Set output sample rate (works for both Linux PipeWire and Windows ASIO)
    handler.runtime.output_sample_rate = args.output_sample_rate;

    // Set adaptive resampling (works on both Linux PipeWire and Windows ASIO)
    handler.runtime.enable_adaptive_resampling = args.enable_adaptive_resampling;

    // Initialize VBAP spatial renderer if requested
    if args.enable_vbap {
        use renderer::spatial_renderer::SpatialRenderer;
        #[cfg(feature = "saf_vbap")]
        use renderer::spatial_vbap::VbapTableMode;
        use renderer::spatial_vbap::{DistanceModel, VbapPanner};
        use renderer::speaker_layout::SpeakerLayout;
        use std::str::FromStr;
        let _ = vbap_cartesian_defaults;

        // Parse distance model
        let distance_model = DistanceModel::from_str(&args.vbap_distance_model)
            .map_err(|e| anyhow::anyhow!("Invalid distance model: {}", e))?;

        // Parse room ratio (width,length,height)
        let room_ratio: [f32; 3] = {
            let parts: Vec<&str> = args.room_ratio.split(',').collect();
            if parts.len() != 3 {
                anyhow::bail!(
                    "Invalid room-ratio format '{}'. Expected 'width,length,height' (e.g., '1.0,2.0,0.5')",
                    args.room_ratio
                );
            }
            [
                parts[0]
                    .trim()
                    .parse::<f32>()
                    .map_err(|_| anyhow::anyhow!("Invalid room-ratio width: '{}'", parts[0]))?,
                parts[1]
                    .trim()
                    .parse::<f32>()
                    .map_err(|_| anyhow::anyhow!("Invalid room-ratio length: '{}'", parts[1]))?,
                parts[2]
                    .trim()
                    .parse::<f32>()
                    .map_err(|_| anyhow::anyhow!("Invalid room-ratio height: '{}'", parts[2]))?,
            ]
        };
        let room_ratio_rear = args
            .room_ratio_rear
            // Rear ratio defaults to the front depth ratio (room_ratio length).
            .unwrap_or(room_ratio[1])
            .max(0.01);
        let room_ratio_lower = args.room_ratio_lower.unwrap_or(0.5).max(0.01);
        let room_ratio_center_blend = args.room_ratio_center_blend.unwrap_or(0.5).clamp(0.0, 1.0);
        #[cfg(feature = "saf_vbap")]
        let vbap_table_mode = match args.vbap_table_mode {
            VbapTableModeArg::Polar => VbapTableMode::Polar,
            VbapTableModeArg::Cartesian => {
                let x_cells = args
                    .vbap_cart_x_size
                    .unwrap_or(vbap_cartesian_defaults.x_size as usize);
                let y_cells = args
                    .vbap_cart_y_size
                    .unwrap_or(vbap_cartesian_defaults.y_size as usize);
                let z_cells = args
                    .vbap_cart_z_size
                    .unwrap_or(vbap_cartesian_defaults.z_size as usize);
                let z_neg_cells = args.vbap_cart_z_neg_size.unwrap_or(0);
                if x_cells < 1 || y_cells < 1 || z_cells < 1 {
                    anyhow::bail!(
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
        let vbap_allow_negative_z = if args.vbap_allow_negative_z {
            true
        } else if args.no_vbap_allow_negative_z {
            false
        } else {
            vbap_cartesian_defaults.allow_negative_z
        };
        log::info!("VBAP allow_negative_z: {}", vbap_allow_negative_z);

        let resolve_layout = |args: &RenderArgs,
                              current_layout_from_config: &Option<SpeakerLayout>|
         -> Result<SpeakerLayout> {
            if let Some(ref layout_path) = args.speaker_layout {
                log::info!("Loading speaker layout from: {}", layout_path.display());
                SpeakerLayout::from_file(layout_path)
            } else if let Some(layout) = current_layout_from_config.clone() {
                log::info!(
                    "Using embedded current_layout from config: {} speakers ({})",
                    layout.num_speakers(),
                    layout.speaker_names().join(", ")
                );
                Ok(layout)
            } else {
                log::info!("No speaker layout specified, using 7.1.4 preset");
                SpeakerLayout::preset("7.1.4")
            }
        };

        // Create spatial renderer (either from pre-computed table or generate new)
        // Note: presentation 3 is always 48kHz
        let renderer = if let Some(ref vbap_table_path) = args.vbap_table {
            if args.vbap_table_mode == VbapTableModeArg::Cartesian {
                log::warn!(
                    "Ignoring --vbap-table-mode=cartesian because --vbap-table is provided (mode is defined by the file)"
                );
            }
            // Load pre-computed VBAP table from file
            log::info!(
                "Loading pre-computed VBAP table from: {}",
                vbap_table_path.display()
            );
            let start_time = std::time::Instant::now();

            let (vbap, loaded_layout) = VbapPanner::load_from_file(vbap_table_path)
                .map_err(|e| anyhow::anyhow!("Failed to load VBAP table: {}", e))?;
            let vbap = vbap.with_negative_z(vbap_allow_negative_z);

            let elapsed = start_time.elapsed();
            log::info!(
                "VBAP table loaded in {:.3}s (az={}°, el={}°, spread_res={}, {} triangles)",
                elapsed.as_secs_f64(),
                vbap.azimuth_resolution(),
                vbap.elevation_resolution(),
                vbap.spread_resolution(),
                vbap.num_triangles()
            );

            // Use speaker layout from file if available, otherwise load from YAML or use default
            let layout = if let Some(layout) = loaded_layout {
                log::info!(
                    "Using speaker layout from VBAP table: {} speakers ({})",
                    layout.num_speakers(),
                    layout.speaker_names().join(", ")
                );
                layout
            } else {
                // v1/v2 format: layout not included, load from YAML or use default
                log::warn!("VBAP table does not include speaker layout (old format)");
                resolve_layout(args, &current_layout_from_config)?
            };

            log::info!(
                "Speaker layout: {} speakers ({})",
                layout.num_speakers(),
                layout.speaker_names().join(", ")
            );

            SpatialRenderer::from_vbap(
                vbap,
                layout,
                48000, // Sample rate (standard for this presentation)
                vbap_allow_negative_z,
                args.vbap_position_interpolation,
                distance_model,
                args.vbap_distance_max,
                args.spread_from_distance,
                args.spread_distance_range,
                args.spread_distance_curve,
                args.vbap_spread_min,
                args.vbap_spread_max,
                args.log_object_positions,
                room_ratio,
                room_ratio_rear,
                room_ratio_lower,
                room_ratio_center_blend,
                args.master_gain,
                args.auto_gain,
                args.use_loudness,
                args.distance_diffuse,
                args.distance_diffuse_threshold,
                args.distance_diffuse_curve,
            )?
        } else {
            // Runtime VBAP generation (requires 'saf_vbap' feature)
            #[cfg(feature = "saf_vbap")]
            {
                // Load speaker layout for runtime generation
                let layout = resolve_layout(args, &current_layout_from_config)?;

                log::info!(
                    "Speaker layout: {} speakers ({})",
                    layout.num_speakers(),
                    layout.speaker_names().join(", ")
                );

                // Generate VBAP table at runtime
                log::info!("Generating VBAP table at runtime (this may take a few seconds)...");
                let start_time = std::time::Instant::now();
                let azimuth_cells = args.vbap_azimuth_resolution.max(1);
                let elevation_cells = args.vbap_elevation_resolution.max(1);
                let distance_cells = args.vbap_distance_res.max(1);
                let azimuth_step_deg = (360.0f32 / (azimuth_cells as f32)).max(1.0).round() as i32;
                let elevation_step_deg = (((if vbap_allow_negative_z { 180.0 } else { 90.0 })
                    / (elevation_cells as f32))
                    .max(1.0)
                    .round()) as i32;
                let distance_step = args.vbap_distance_max.max(0.01) / (distance_cells as f32);

                let renderer = SpatialRenderer::new(
                    layout,
                    48000, // Sample rate (standard for this presentation)
                    azimuth_step_deg,
                    elevation_step_deg,
                    distance_step,
                    args.vbap_distance_max,
                    vbap_table_mode,
                    vbap_allow_negative_z,
                    args.vbap_position_interpolation,
                    distance_model,
                    args.spread_from_distance,
                    args.spread_distance_range,
                    args.spread_distance_curve,
                    args.vbap_spread_min,
                    args.vbap_spread_max,
                    args.log_object_positions,
                    room_ratio,
                    room_ratio_rear,
                    room_ratio_lower,
                    room_ratio_center_blend,
                    args.master_gain,
                    args.auto_gain,
                    args.use_loudness,
                    args.distance_diffuse,
                    args.distance_diffuse_threshold,
                    args.distance_diffuse_curve,
                    match preferred_vbap_table_mode {
                        bridge_api::RVbapTableMode::Polar => {
                            renderer::live_params::VbapBackendMode::Polar
                        }
                        bridge_api::RVbapTableMode::Cartesian => {
                            renderer::live_params::VbapBackendMode::Cartesian
                        }
                    },
                    if vbap_table_mode_explicit {
                        match args.vbap_table_mode {
                            VbapTableModeArg::Polar => {
                                renderer::live_params::LiveVbapTableMode::Polar
                            }
                            VbapTableModeArg::Cartesian => {
                                renderer::live_params::LiveVbapTableMode::Cartesian
                            }
                        }
                    } else {
                        renderer::live_params::LiveVbapTableMode::Auto
                    },
                    args.vbap_cart_x_size
                        .unwrap_or(vbap_cartesian_defaults.x_size as usize),
                    args.vbap_cart_y_size
                        .unwrap_or(vbap_cartesian_defaults.y_size as usize),
                    args.vbap_cart_z_size
                        .unwrap_or(vbap_cartesian_defaults.z_size as usize),
                    args.vbap_cart_z_neg_size.unwrap_or(0),
                )?;

                let elapsed = start_time.elapsed();
                log::info!("VBAP table generated in {:.2}s", elapsed.as_secs_f64());

                renderer
            }
            #[cfg(not(feature = "saf_vbap"))]
            {
                anyhow::bail!(
                    "VBAP table generation not available (built without 'saf_vbap' feature).\n\
                     Please provide a pre-generated VBAP table using --vbap-table <file>.\n\
                     Generate tables on a system with SAF VBAP support using:\n  \
                     orender generate-vbap --speaker-layout <layout.yaml> --output <table.vbap>"
                );
            }
        };

        log::info!("VBAP spatial rendering enabled");
        handler.spatial_renderer = Some(renderer);
    }

    // Initialize OSC sender if requested
    if args.osc {
        use std::net::SocketAddrV4;
        use std::str::FromStr;
        let osc_addr = SocketAddrV4::from_str(&format!("{}:{}", args.osc_host, args.osc_port))?;
        match renderer::osc_output::OscSender::new(osc_addr) {
            Ok(sender) => {
                log::info!("OSC output enabled: {}:{}", args.osc_host, args.osc_port);
                handler.telemetry.osc_sender = Some(sender);
            }
            Err(e) => {
                log::error!("Failed to create OSC sender: {}", e);
                return Err(e);
            }
        }
    }

    // Initialize the audio meter whenever VBAP + OSC are enabled.
    // Delivery is controlled per OSC client via `/omniphony/control/metering`.
    match (&handler.spatial_renderer, &handler.telemetry.osc_sender) {
        (Some(renderer), Some(_)) => {
            let num_speakers = renderer.num_speakers();
            handler.telemetry.audio_meter = Some(AudioMeter::new(num_speakers, 20.0));
            log::info!(
                "OSC metering available per client ({} speakers, 20 Hz)",
                num_speakers
            );
        }
        _ => {}
    }

    // Wire the renderer control into the OSC sender so the listener can read/write live params.
    if let Some(renderer) = &handler.spatial_renderer {
        let ctrl = renderer.renderer_control();
        #[cfg(any(
            target_os = "linux",
            target_os = "windows"
        ))]
        ctrl.set_requested_output_device(args.output_device.clone());
        ctrl.set_input_path(Some(input_path.display().to_string()));
        if let Some(backend) = args.output_backend.or_else(OutputBackend::platform_default) {
            ctrl.set_available_output_devices(list_available_output_devices(backend));
            ctrl.set_device_list_fetcher(move || list_available_output_devices(backend));
        } else {
            ctrl.set_available_output_devices(Vec::new());
        }
        ctrl.set_requested_output_sample_rate(args.output_sample_rate);
        ctrl.set_requested_adaptive_resampling(args.enable_adaptive_resampling);
        ctrl.set_requested_adaptive_resampling_enable_far_mode(
            handler.runtime.adaptive_resampling_config.enable_far_mode,
        );
        ctrl.set_requested_adaptive_resampling_force_silence_in_far_mode(
            handler
                .runtime
                .adaptive_resampling_config
                .force_silence_in_far_mode,
        );
        ctrl.set_requested_adaptive_resampling_hard_recover_in_far_mode(
            handler
                .runtime
                .adaptive_resampling_config
                .hard_recover_in_far_mode,
        );
        ctrl.set_requested_adaptive_resampling_far_mode_return_fade_in_ms(
            handler
                .runtime
                .adaptive_resampling_config
                .far_mode_return_fade_in_ms,
        );
        ctrl.set_requested_ramp_mode(args.ramp_mode.into());
        ctrl.live.write().unwrap().ramp_mode = args.ramp_mode.into();
        #[cfg(target_os = "linux")]
        {
            let defaults = PipewireBufferConfig::default();
            let adaptive = &handler.runtime.adaptive_resampling_config;
            ctrl.set_requested_latency_target_ms(Some(
                args.pw_latency.unwrap_or(defaults.latency_ms),
            ));
            ctrl.set_requested_adaptive_resampling_force_silence_in_far_mode(
                adaptive.force_silence_in_far_mode,
            );
            ctrl.set_requested_adaptive_resampling_hard_recover_in_far_mode(
                adaptive.hard_recover_in_far_mode,
            );
            ctrl.set_requested_adaptive_resampling_far_mode_return_fade_in_ms(
                adaptive.far_mode_return_fade_in_ms,
            );
            ctrl.set_requested_adaptive_resampling_kp_near(adaptive.kp_near as f32);
            ctrl.set_requested_adaptive_resampling_kp_far(adaptive.kp_far as f32);
            ctrl.set_requested_adaptive_resampling_ki(adaptive.ki as f32);
            ctrl.set_requested_adaptive_resampling_max_adjust(adaptive.max_adjust as f32);
            ctrl.set_requested_adaptive_resampling_max_adjust_far(adaptive.max_adjust_far as f32);
            ctrl.set_requested_adaptive_resampling_update_interval_callbacks(
                adaptive.update_interval_callbacks,
            );
            ctrl.set_requested_adaptive_resampling_near_far_threshold_ms(
                adaptive.near_far_threshold_ms,
            );
            ctrl.set_requested_adaptive_resampling_measurement_smoothing_alpha(
                adaptive.measurement_smoothing_alpha as f32,
            );
        }
        #[cfg(target_os = "windows")]
        {
            ctrl.set_requested_latency_target_ms(Some(handler.runtime.asio_target_latency_ms));
            let adaptive = &handler.runtime.adaptive_resampling_config;
            ctrl.set_requested_adaptive_resampling_force_silence_in_far_mode(
                adaptive.force_silence_in_far_mode,
            );
            ctrl.set_requested_adaptive_resampling_hard_recover_in_far_mode(
                adaptive.hard_recover_in_far_mode,
            );
            ctrl.set_requested_adaptive_resampling_far_mode_return_fade_in_ms(
                adaptive.far_mode_return_fade_in_ms,
            );
            ctrl.set_requested_adaptive_resampling_kp_near(adaptive.kp_near as f32);
            ctrl.set_requested_adaptive_resampling_kp_far(adaptive.kp_far as f32);
            ctrl.set_requested_adaptive_resampling_ki(adaptive.ki as f32);
            ctrl.set_requested_adaptive_resampling_max_adjust(adaptive.max_adjust as f32);
            ctrl.set_requested_adaptive_resampling_max_adjust_far(adaptive.max_adjust_far as f32);
            ctrl.set_requested_adaptive_resampling_update_interval_callbacks(
                adaptive.update_interval_callbacks,
            );
            ctrl.set_requested_adaptive_resampling_near_far_threshold_ms(
                adaptive.near_far_threshold_ms,
            );
            ctrl.set_requested_adaptive_resampling_measurement_smoothing_alpha(
                adaptive.measurement_smoothing_alpha as f32,
            );
        }
        // Pass the config path so the save-config OSC handler can persist params.
        if let Some(path) = config_path {
            ctrl.set_config_path(path.clone());
        }
        if let Some(sender) = &mut handler.telemetry.osc_sender {
            sender.attach_renderer_control(ctrl);
        }
    }

    // Start OSC registration listener (active whenever OSC + VBAP are both enabled)
    if let (Some(renderer), Some(sender)) =
        (&handler.spatial_renderer, &handler.telemetry.osc_sender)
    {
        let layout = renderer.speaker_layout();
        let config_bytes = renderer::osc_output::build_speaker_config_bundle(&layout)?;
        sender.start_listener(args.osc_rx_port, config_bytes)?;
    }

    Ok(())
}

fn effective_output_backend(
    args: &RenderArgs,
    is_spatial_presentation: bool,
) -> Result<OutputBackend> {
    let resolved_backend = args
        .output_backend
        .or_else(OutputBackend::platform_default)
        .unwrap_or(OutputBackend::Unsupported);
    if resolved_backend == OutputBackend::Unsupported {
        anyhow::bail!("No supported realtime audio output backend is available");
    }
    if is_spatial_presentation && !args.enable_vbap {
        anyhow::bail!(
            "Spatial presentations require VBAP rendering with a realtime output backend. Re-run with --enable-vbap."
        );
    }
    Ok(resolved_backend)
}

fn log_auto_gain_summary(handler: &DecodeHandler) {
    if let Some(ref renderer) = handler.spatial_renderer {
        let auto_gain_db = renderer.get_auto_gain_db();
        if auto_gain_db < 0.0 {
            log::warn!(
                "Auto-gain: {:.1} dB attenuation was needed to avoid clipping. \
                 Use --master-gain {:.1} for future playback without clipping.",
                auto_gain_db,
                auto_gain_db
            );
        } else {
            log::info!("Auto-gain: No clipping detected, no attenuation needed.");
        }
    }
}

fn handle_stream_end(handler: &mut DecodeHandler, args: &RenderArgs) -> Result<()> {
    // Stream ended in continuous mode - finalize current handler and reset
    log::info!("Stream ended, finalizing current output and resetting handler...");
    handler.finalize()?;

    // Report auto-gain attenuation before reset
    if args.auto_gain {
        log_auto_gain_summary(handler);
    }

    // Preserve some state before resetting handler
    let spatial_renderer = handler.spatial_renderer.take();
    let osc_sender = handler.telemetry.osc_sender.take();
    let audio_meter = handler.telemetry.audio_meter.take();
    let runtime = handler.runtime.clone();

    // Reset handler to default state for next stream.
    *handler = DecodeHandler::default();

    // Restore preserved state (bridge lives in decoder thread — no restore needed).
    handler.spatial_renderer = spatial_renderer;
    // Preserve OSC sender and meter across resets so that:
    //   - the listener thread keeps running on the same port,
    //   - registered clients remain in the client list, and
    //   - the renderer-control Arc stays wired up.
    // Recreating OscSender would orphan the old listener thread (still
    // holding a port-9000 socket) and lose all registered clients.
    handler.telemetry.osc_sender = osc_sender;
    handler.telemetry.audio_meter = audio_meter;
    handler.runtime = runtime;
    if let Some(ref mut osc_sender) = handler.telemetry.osc_sender {
        osc_sender.bump_content_generation();
    }

    log::info!("Handler reset complete, ready for next stream");
    // Tell the service manager the service is ready again after the reload.
    sys::notify_ready();

    Ok(())
}

struct DecodeRunContext<'a> {
    args: &'a RenderArgs,
    effective_output_backend: OutputBackend,
    state: &'a WriterState,
}

fn handle_audio_message(
    handler: &mut DecodeHandler,
    decoded: DecodedAudioData,
    ctx: &DecodeRunContext<'_>,
) -> Result<()> {
    let frame = decoded.frame;
    // Check if substream info changed and handle it before processing the frame.
    if frame.is_new_segment {
        // Store the current sample position as the start of the new segment.
        handler.spatial.segment_start_samples = handler.session.decoded_samples;

        // Handle stream restart with actual sample rate and channel count from decoded frame.
        handler.handle_stream_restart(
            ctx.effective_output_backend,
            frame.sampling_frequency,
            frame.channel_count as usize,
            ctx.args.bed_conform,
        )?;
        handler.spatial.is_segmented = true; // Mark that we're now in segmented mode
    }

    let ctx = FrameHandlerContext {
        output_backend: ctx.effective_output_backend,
        state: ctx.state,
        bed_conform: ctx.args.bed_conform,
        use_loudness: ctx.args.use_loudness,
        decode_time_ms: decoded.decode_time_ms,
        queue_delay_ms: decoded.sent_at.elapsed().as_secs_f32() * 1000.0,
    };
    handler.handle_decoded_frame(frame, &ctx)
}

fn handle_flush_request(handler: &mut DecodeHandler) {
    // Seek/decoder reset: discard stale buffered audio and purge cached
    // spatial state so resumed audio cannot reuse stale object positions.
    handler.handle_decoder_flush_request();
}

fn handle_decode_error(ctx: &DecodeRunContext<'_>, err: anyhow::Error) -> Result<()> {
    let _ = ctx;
    Err(err)
}

fn process_decoder_messages(
    rx: &mpsc::Receiver<Result<DecoderMessage>>,
    handler: &mut DecodeHandler,
    ctx: &DecodeRunContext<'_>,
) -> Result<()> {
    while let Ok(result) = rx.recv() {
        match result {
            Ok(DecoderMessage::AudioData(frame)) => handle_audio_message(handler, frame, ctx)?,
            Ok(DecoderMessage::FlushRequest) => handle_flush_request(handler),
            Ok(DecoderMessage::StreamEnd) => {
                handle_stream_end(handler, ctx.args)?;
            }
            Err(e) => return handle_decode_error(ctx, e),
        }
    }

    Ok(())
}

fn begin_shutdown_if_requested() -> bool {
    let is_shutdown = sys::shutdown::ShutdownHandle::is_requested();
    if is_shutdown {
        sys::notify_stopping();
        log::info!("Shutdown signal received, flushing audio output...");
    }
    is_shutdown
}

fn finalize_output_for_exit(handler: &mut DecodeHandler, is_shutdown: bool) -> Result<()> {
    // Finalize output (flushes PipeWire ring buffer, closes file writers, etc.).
    // During a clean shutdown, errors here are logged but do not affect the
    // exit code — the signal was handled correctly regardless.
    if is_shutdown {
        if let Err(e) = handler.finalize() {
            log::warn!("Error flushing audio during shutdown (ignored): {e}");
        }
        Ok(())
    } else {
        handler.finalize()
    }
}

fn complete_render_run(
    prepared: PreparedDecodeRun,
    handler: &DecodeHandler,
    args: &RenderArgs,
    is_shutdown: bool,
) -> Result<()> {
    match prepared.decode_thread.join() {
        Ok(Ok(())) => {
            if is_shutdown {
                log::info!("Decoder stopped cleanly");
            } else {
                log::info!("Decoding completed successfully");

                // Report final auto-gain attenuation if enabled
                if args.auto_gain {
                    log_auto_gain_summary(handler);
                }
            }
            Ok(())
        }
        Ok(Err(e)) => Err(e),
        Err(_) => Err(anyhow::anyhow!("Decode thread panicked")),
    }
}

pub fn cmd_render(args: &RenderArgs, cli: &Cli) -> Result<()> {
    loop {
        let (config_path, effective_args, current_layout_from_config, vbap_table_mode_explicit) =
            resolve_effective_decode_args(args, cli);
        let args = &effective_args;

        if maybe_save_effective_config(cli, args, &config_path)? {
            return Ok(());
        }

        let prepared = prepare_render_run(args, cli)?;
        run_prepared_render(
            prepared,
            args,
            &config_path,
            current_layout_from_config,
            vbap_table_mode_explicit,
        )?;

        if sys::ShutdownHandle::is_restart_from_config_requested() {
            sys::ShutdownHandle::clear_restart_from_config();
            if sys::ShutdownHandle::is_requested() {
                return Ok(());
            }
            log::info!("Restarting render pipeline from config");
            continue;
        }

        return Ok(());
    }
}

fn run_prepared_render(
    prepared: PreparedDecodeRun,
    args: &RenderArgs,
    config_path: &Option<std::path::PathBuf>,
    current_layout_from_config: Option<renderer::speaker_layout::SpeakerLayout>,
    vbap_table_mode_explicit: bool,
) -> Result<()> {
    let mut effective_args = args.clone();
    if !vbap_table_mode_explicit {
        effective_args.vbap_table_mode = match prepared.preferred_vbap_table_mode {
            bridge_api::RVbapTableMode::Polar => VbapTableModeArg::Polar,
            bridge_api::RVbapTableMode::Cartesian => VbapTableModeArg::Cartesian,
        };
        log::info!(
            "Using bridge-preferred VBAP table mode: {:?}",
            effective_args.vbap_table_mode
        );
    }

    let mut handler = DecodeHandler::default();
    init_render_handler(
        &mut handler,
        &effective_args,
        &prepared.input_path,
        config_path,
        current_layout_from_config,
        prepared.vbap_cartesian_defaults,
        prepared.preferred_vbap_table_mode,
        vbap_table_mode_explicit,
    )?;
    handler.spatial.coordinate_format = prepared.coordinate_format;

    run_render_message_phase(&prepared, &mut handler, &effective_args)?;
    finalize_render_run(prepared, &mut handler, &effective_args)
}

fn run_render_message_phase(
    prepared: &PreparedDecodeRun,
    handler: &mut DecodeHandler,
    args: &RenderArgs,
) -> Result<()> {
    let effective_output_backend =
        effective_output_backend(args, prepared.is_spatial_presentation)?;
    let run_ctx = DecodeRunContext {
        args,
        effective_output_backend,
        state: &prepared.state,
    };

    // All initialisation done (VBAP tables loaded, signal handlers installed).
    // Notify systemd that the service is ready to process audio frames,
    // then start the watchdog thread if WATCHDOG_USEC is configured.
    sys::notify_ready();
    process_decoder_messages(&prepared.rx, handler, &run_ctx)
}

fn finalize_render_run(
    prepared: PreparedDecodeRun,
    handler: &mut DecodeHandler,
    args: &RenderArgs,
) -> Result<()> {
    let is_shutdown = begin_shutdown_if_requested();
    finalize_output_for_exit(handler, is_shutdown)?;
    complete_render_run(prepared, handler, args, is_shutdown)
}
