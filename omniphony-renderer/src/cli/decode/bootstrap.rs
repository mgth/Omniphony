use super::handler::DecodeHandler;
use crate::cli::command::{OutputBackend, RenderArgs, VbapTableModeArg};
use crate::runtime_osc::{OscSender, build_speaker_config_bundle};
use anyhow::Result;
use audio_input::{
    InputBackend, InputClockMode, InputControl, InputLfeMode, InputMapMode, InputMode,
    InputSampleFormat, RequestedAudioInputConfig,
};
#[cfg(target_os = "linux")]
use audio_output::pipewire::{PipewireBufferConfig, list_pipewire_output_devices};
use audio_output::{
    AdaptiveResamplingConfig, AudioControl, OutputDeviceOption, RequestedAudioOutputConfig,
};
use renderer::metering::AudioMeter;
use renderer::speaker_layout::SpeakerLayout;
use std::sync::Arc;

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

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn list_available_output_devices(_backend: OutputBackend) -> Vec<OutputDeviceOption> {
    Vec::new()
}

fn render_config_from_path(
    config_path: &Option<std::path::PathBuf>,
) -> Option<renderer::config::RenderConfig> {
    config_path
        .as_deref()
        .map(renderer::config::Config::load_or_default)
        .and_then(|cfg| cfg.render)
}

fn build_adaptive_resampling_config(
    args: &RenderArgs,
    render_cfg: Option<&renderer::config::RenderConfig>,
) -> AdaptiveResamplingConfig {
    let defaults = AdaptiveResamplingConfig::default();
    AdaptiveResamplingConfig {
        enable_far_mode: render_cfg
            .and_then(|cfg| cfg.adaptive_resampling_enable_far_mode)
            .unwrap_or(defaults.enable_far_mode),
        force_silence_in_far_mode: render_cfg
            .and_then(|cfg| cfg.adaptive_resampling_force_silence_in_far_mode)
            .unwrap_or(defaults.force_silence_in_far_mode),
        hard_recover_high_in_far_mode: render_cfg
            .and_then(|cfg| cfg.adaptive_resampling_hard_recover_high_in_far_mode)
            .unwrap_or(defaults.hard_recover_high_in_far_mode),
        hard_recover_low_in_far_mode: render_cfg
            .and_then(|cfg| cfg.adaptive_resampling_hard_recover_low_in_far_mode)
            .unwrap_or(defaults.hard_recover_low_in_far_mode),
        far_mode_return_fade_in_ms: render_cfg
            .and_then(|cfg| cfg.adaptive_resampling_far_mode_return_fade_in_ms)
            .unwrap_or(defaults.far_mode_return_fade_in_ms),
        kp_near: render_cfg
            .and_then(|cfg| cfg.adaptive_resampling_kp_near)
            .map(|v| v as f64)
            .unwrap_or(defaults.kp_near),
        ki: render_cfg
            .and_then(|cfg| cfg.adaptive_resampling_ki)
            .map(|v| v as f64)
            .unwrap_or(defaults.ki),
        integral_discharge_ratio: render_cfg
            .and_then(|cfg| cfg.adaptive_resampling_integral_discharge_ratio)
            .map(|v| v as f64)
            .unwrap_or(defaults.integral_discharge_ratio),
        max_adjust: render_cfg
            .and_then(|cfg| cfg.adaptive_resampling_max_adjust)
            .map(|v| v as f64)
            .unwrap_or(defaults.max_adjust),
        update_interval_callbacks: args
            .adaptive_resampling_update_interval_callbacks
            .or_else(|| {
                render_cfg.and_then(|cfg| cfg.adaptive_resampling_update_interval_callbacks)
            })
            .unwrap_or(defaults.update_interval_callbacks)
            .max(1),
        near_far_threshold_ms: render_cfg
            .and_then(|cfg| cfg.adaptive_resampling_near_far_threshold_ms)
            .unwrap_or(defaults.near_far_threshold_ms),
        paused: false,
    }
}

fn build_requested_input_config(
    render_cfg: Option<&renderer::config::RenderConfig>,
) -> RequestedAudioInputConfig {
    let mut requested = RequestedAudioInputConfig::default();

    if let Some(render_cfg) = render_cfg {
        requested.mode = match render_cfg.input_mode {
            Some(renderer::config::InputModeConfig::Live) => InputMode::Live,
            Some(renderer::config::InputModeConfig::PipewireBridge) => InputMode::PipewireBridge,
            _ => InputMode::Bridge,
        };

        if let Some(live_input) = render_cfg.live_input.as_ref() {
            requested.backend = live_input.backend.as_ref().map(|backend| match backend {
                renderer::config::InputBackendConfig::Pipewire => InputBackend::Pipewire,
                renderer::config::InputBackendConfig::Asio => InputBackend::Asio,
            });
            requested.node_name = live_input.node.clone();
            requested.node_description = live_input.description.clone();
            requested.layout_path = live_input.layout.clone();
            requested.current_layout = live_input.current_layout.clone();
            requested.clock_mode = match live_input.clock_mode {
                Some(renderer::config::InputClockModeConfig::Pipewire) => InputClockMode::Pipewire,
                Some(renderer::config::InputClockModeConfig::Dac) | None => InputClockMode::Dac,
            };
            requested.channels = live_input.channels;
            requested.sample_rate_hz = live_input.sample_rate;
            requested.sample_format = live_input.sample_format.as_deref().and_then(|format| {
                match format.trim().to_ascii_lowercase().as_str() {
                    "f32" => Some(InputSampleFormat::F32),
                    "s16" => Some(InputSampleFormat::S16),
                    _ => None,
                }
            });
            requested.map_mode = match live_input.map {
                Some(renderer::config::InputMapModeConfig::SevenOneFixed) | None => {
                    InputMapMode::SevenOneFixed
                }
            };
            requested.lfe_mode = match live_input.lfe_mode {
                Some(renderer::config::InputLfeModeConfig::Object) => InputLfeMode::Object,
                Some(renderer::config::InputLfeModeConfig::Drop) => InputLfeMode::Drop,
                Some(renderer::config::InputLfeModeConfig::Direct) | None => InputLfeMode::Direct,
            };
        }
    }

    requested
}

#[cfg(target_os = "linux")]
fn configure_linux_runtime_output(
    handler: &mut DecodeHandler,
    args: &RenderArgs,
    render_cfg: Option<&renderer::config::RenderConfig>,
) {
    handler.runtime.output_device = args.output_device.clone();
    let defaults = PipewireBufferConfig::default();
    let latency_ms = args.latency_target_ms.unwrap_or(defaults.latency_ms);
    handler.runtime.pw_buffer_config = PipewireBufferConfig {
        latency_ms,
        max_latency_ms: latency_ms * 2,
        quantum_frames: args.pw_quantum.unwrap_or(defaults.quantum_frames),
    };
    handler.runtime.adaptive_resampling_config = build_adaptive_resampling_config(args, render_cfg);
}

#[cfg(target_os = "windows")]
fn configure_windows_runtime_output(
    handler: &mut DecodeHandler,
    args: &RenderArgs,
    render_cfg: Option<&renderer::config::RenderConfig>,
) {
    handler.runtime.output_device = args.output_device.clone();
    handler.runtime.adaptive_resampling_config = build_adaptive_resampling_config(args, render_cfg);
}

fn parse_room_ratio(args: &RenderArgs) -> Result<([f32; 3], f32, f32, f32)> {
    let parts: Vec<&str> = args.room_ratio.split(',').collect();
    if parts.len() != 3 {
        anyhow::bail!(
            "Invalid room-ratio format '{}'. Expected 'width,length,height' (e.g., '1.0,2.0,0.5')",
            args.room_ratio
        );
    }
    let room_ratio = [
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
    ];
    let room_ratio_rear = args.room_ratio_rear.unwrap_or(room_ratio[1]).max(0.01);
    let room_ratio_lower = args.room_ratio_lower.unwrap_or(0.5).max(0.01);
    let room_ratio_center_blend = args.room_ratio_center_blend.unwrap_or(0.5).clamp(0.0, 1.0);
    Ok((
        room_ratio,
        room_ratio_rear,
        room_ratio_lower,
        room_ratio_center_blend,
    ))
}

fn resolve_layout(
    args: &RenderArgs,
    current_layout_from_config: &Option<SpeakerLayout>,
) -> Result<SpeakerLayout> {
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
}

#[cfg(feature = "saf_vbap")]
fn resolve_vbap_table_mode(
    args: &RenderArgs,
    vbap_cartesian_defaults: bridge_api::RVbapCartesianDefaults,
) -> Result<(renderer::spatial_vbap::VbapTableMode, bool)> {
    use renderer::spatial_vbap::VbapTableMode;

    let vbap_allow_negative_z = if args.vbap_allow_negative_z {
        true
    } else if args.no_vbap_allow_negative_z {
        false
    } else {
        vbap_cartesian_defaults.allow_negative_z
    };
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
    Ok((vbap_table_mode, vbap_allow_negative_z))
}

fn init_spatial_renderer(
    handler: &mut DecodeHandler,
    args: &RenderArgs,
    current_layout_from_config: &Option<SpeakerLayout>,
    vbap_cartesian_defaults: bridge_api::RVbapCartesianDefaults,
    preferred_vbap_table_mode: bridge_api::RVbapTableMode,
    vbap_table_mode_explicit: bool,
) -> Result<()> {
    #[cfg(not(feature = "saf_vbap"))]
    let _ = (
        preferred_vbap_table_mode,
        vbap_table_mode_explicit,
        vbap_cartesian_defaults,
    );

    if !args.enable_vbap {
        return Ok(());
    }

    use renderer::spatial_renderer::SpatialRenderer;
    use renderer::spatial_vbap::{DistanceModel, VbapPanner};
    use std::str::FromStr;

    let distance_model = DistanceModel::from_str(&args.vbap_distance_model)
        .map_err(|e| anyhow::anyhow!("Invalid distance model: {}", e))?;
    let (room_ratio, room_ratio_rear, room_ratio_lower, room_ratio_center_blend) =
        parse_room_ratio(args)?;

    #[cfg(feature = "saf_vbap")]
    let (vbap_table_mode, vbap_allow_negative_z) =
        resolve_vbap_table_mode(args, vbap_cartesian_defaults)?;
    #[cfg(not(feature = "saf_vbap"))]
    let vbap_allow_negative_z = if args.vbap_allow_negative_z {
        true
    } else if args.no_vbap_allow_negative_z {
        false
    } else {
        false
    };

    log::info!("VBAP allow_negative_z: {}", vbap_allow_negative_z);

    let renderer = if let Some(ref vbap_table_path) = args.vbap_table {
        if args.vbap_table_mode == VbapTableModeArg::Cartesian {
            log::warn!(
                "Ignoring --vbap-table-mode=cartesian because --vbap-table is provided (mode is defined by the file)"
            );
        }
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

        let layout = if let Some(layout) = loaded_layout {
            log::info!(
                "Using speaker layout from VBAP table: {} speakers ({})",
                layout.num_speakers(),
                layout.speaker_names().join(", ")
            );
            layout
        } else {
            log::warn!("VBAP table does not include speaker layout (old format)");
            resolve_layout(args, current_layout_from_config)?
        };

        log::info!(
            "Speaker layout: {} speakers ({})",
            layout.num_speakers(),
            layout.speaker_names().join(", ")
        );

        SpatialRenderer::from_vbap(
            vbap,
            layout,
            48000,
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
        #[cfg(feature = "saf_vbap")]
        {
            let layout = resolve_layout(args, current_layout_from_config)?;
            log::info!(
                "Speaker layout: {} speakers ({})",
                layout.num_speakers(),
                layout.speaker_names().join(", ")
            );
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
                48000,
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
                        VbapTableModeArg::Polar => renderer::live_params::LiveVbapTableMode::Polar,
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
    Ok(())
}

fn init_osc_runtime(
    handler: &mut DecodeHandler,
    args: &RenderArgs,
    input_path: &std::path::Path,
    config_path: &Option<std::path::PathBuf>,
) -> Result<()> {
    let render_cfg = render_config_from_path(config_path);

    if args.osc {
        use std::net::SocketAddrV4;
        use std::str::FromStr;
        let osc_addr = SocketAddrV4::from_str(&format!("{}:{}", args.osc_host, args.osc_port))?;
        match OscSender::new(osc_addr) {
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

    if let Some(renderer) = &handler.spatial_renderer {
        let ctrl = renderer.renderer_control();
        ctrl.set_input_path(Some(input_path.display().to_string()));
        ctrl.set_requested_ramp_mode(args.ramp_mode.into());
        ctrl.live.write().unwrap().ramp_mode = args.ramp_mode.into();

        let requested_latency_target_ms = {
            #[cfg(target_os = "linux")]
            {
                let defaults = PipewireBufferConfig::default();
                Some(args.latency_target_ms.unwrap_or(defaults.latency_ms))
            }
            #[cfg(target_os = "windows")]
            {
                Some(
                    args.latency_target_ms
                        .unwrap_or(handler.runtime.latency_target_ms),
                )
            }
            #[cfg(not(any(target_os = "linux", target_os = "windows")))]
            {
                None
            }
        };

        let audio_control = Arc::new(AudioControl::new(RequestedAudioOutputConfig {
            output_device: args.output_device.clone(),
            output_sample_rate_hz: args.output_sample_rate,
            latency_target_ms: requested_latency_target_ms,
            adaptive_enabled: args.enable_adaptive_resampling,
            adaptive: handler.runtime.adaptive_resampling_config.clone(),
        }));
        let input_control = Arc::new(InputControl::new(build_requested_input_config(
            render_cfg.as_ref(),
        )));

        if let Some(backend) = args.output_backend.or_else(OutputBackend::platform_default) {
            audio_control.set_available_output_devices(list_available_output_devices(backend));
            audio_control.set_device_list_fetcher(move || list_available_output_devices(backend));
        } else {
            audio_control.set_available_output_devices(Vec::new());
        }

        let input_requested = input_control.requested_snapshot();
        input_control.set_input_state(
            InputMode::Bridge,
            None,
            input_requested.channels,
            input_requested.sample_rate_hz,
            input_requested.node_name.clone(),
            input_requested.sample_format.map(|format| match format {
                InputSampleFormat::F32 => "f32".to_string(),
                InputSampleFormat::S16 => "s16".to_string(),
            }),
        );

        handler.audio_control = Some(Arc::clone(&audio_control));
        handler.input_control = Some(Arc::clone(&input_control));
        if let Some(path) = config_path {
            ctrl.set_config_path(path.clone());
        }
        if let Some(sender) = &mut handler.telemetry.osc_sender {
            sender.attach_renderer_control(ctrl);
            sender.attach_audio_control(audio_control);
            sender.attach_input_control(input_control);
        }
    }

    if let (Some(renderer), Some(sender)) =
        (&handler.spatial_renderer, &handler.telemetry.osc_sender)
    {
        let layout = renderer.speaker_layout();
        let config_bytes = build_speaker_config_bundle(&layout)?;
        sender.start_listener(args.osc_rx_port, config_bytes)?;
    }

    Ok(())
}

pub fn init_render_handler(
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

    let render_cfg = render_config_from_path(config_path);

    #[cfg(target_os = "linux")]
    configure_linux_runtime_output(handler, args, render_cfg.as_ref());
    #[cfg(target_os = "windows")]
    configure_windows_runtime_output(handler, args, render_cfg.as_ref());

    handler.runtime.output_sample_rate = args.output_sample_rate;
    handler.runtime.enable_adaptive_resampling = args.enable_adaptive_resampling;

    init_spatial_renderer(
        handler,
        args,
        &current_layout_from_config,
        vbap_cartesian_defaults,
        preferred_vbap_table_mode,
        vbap_table_mode_explicit,
    )?;
    init_osc_runtime(handler, args, input_path, config_path)?;
    Ok(())
}
