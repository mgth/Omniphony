use super::handler::DecodeHandler;
use crate::cli::command::{EvaluationModeArg, OutputBackend, RenderArgs};
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
use orender_engine::osc::OscSender;
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

#[cfg(target_os = "macos")]
fn list_available_output_devices(_backend: OutputBackend) -> Vec<OutputDeviceOption> {
    audio_output::list_coreaudio_devices()
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

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
fn list_available_output_devices(_backend: OutputBackend) -> Vec<OutputDeviceOption> {
    Vec::new()
}

/// Load the on-disk render config and fold the CLI's override-only render args
/// on top of it, so backend selection, backend params, distance metrics,
/// size-to-spread and adaptive PI tuning take effect on a live run (the
/// renderer sources those from `render_cfg`, not from `RenderArgs`). This is the
/// same effective view that `--save-config` writes.
fn render_config_from_path(
    args: &RenderArgs,
    config_path: &Option<std::path::PathBuf>,
) -> Option<renderer::config::RenderConfig> {
    let mut render = config_path
        .as_deref()
        .map(|p| renderer::config::Config::load_or_default_with_live(p).0)
        .and_then(|cfg| cfg.render)
        .unwrap_or_default();
    super::config_resolution::apply_render_cfg_overrides(&mut render, args);
    Some(render)
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
        high_recover_entry_margin_ms: render_cfg
            .and_then(|cfg| cfg.adaptive_resampling_high_recover_entry_margin_ms)
            .unwrap_or(defaults.high_recover_entry_margin_ms),
        low_recover_settle_stable_ms: render_cfg
            .and_then(|cfg| cfg.adaptive_resampling_low_recover_settle_stable_ms)
            .unwrap_or(defaults.low_recover_settle_stable_ms),
        low_recover_entry_margin_ms: render_cfg
            .and_then(|cfg| cfg.adaptive_resampling_low_recover_entry_margin_ms)
            .unwrap_or(defaults.low_recover_entry_margin_ms),
        low_recover_exit_margin_ms: render_cfg
            .and_then(|cfg| cfg.adaptive_resampling_low_recover_exit_margin_ms)
            .unwrap_or(defaults.low_recover_exit_margin_ms),
        low_recover_settle_margin_ms: render_cfg
            .and_then(|cfg| cfg.adaptive_resampling_low_recover_settle_margin_ms)
            .unwrap_or(defaults.low_recover_settle_margin_ms),
        low_recover_refill_delta_alpha: render_cfg
            .and_then(|cfg| cfg.adaptive_resampling_low_recover_refill_delta_alpha)
            .unwrap_or(defaults.low_recover_refill_delta_alpha),
        control_smoothing_cutoff_hz: render_cfg
            .and_then(|cfg| cfg.adaptive_resampling_control_smoothing_cutoff_hz)
            .map(|v| v as f64)
            .unwrap_or(defaults.control_smoothing_cutoff_hz),
        control_smoothing_order: render_cfg
            .and_then(|cfg| cfg.adaptive_resampling_control_smoothing_order)
            .unwrap_or(defaults.control_smoothing_order),
        paused: false,
        use_pre_bridge_clock: render_cfg
            .and_then(|cfg| cfg.adaptive_resampling_use_pre_bridge_clock)
            .unwrap_or(defaults.use_pre_bridge_clock),
        use_output_pacing: render_cfg
            .and_then(|cfg| cfg.adaptive_resampling_use_output_pacing)
            .unwrap_or(defaults.use_output_pacing),
        disable_backpressure: render_cfg
            .and_then(|cfg| cfg.adaptive_resampling_disable_backpressure)
            .unwrap_or(defaults.disable_backpressure),
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
                Some(renderer::config::InputClockModeConfig::Upstream) => InputClockMode::Upstream,
                Some(renderer::config::InputClockModeConfig::Dac) => InputClockMode::Dac,
                None if requested.mode == InputMode::PipewireBridge => InputClockMode::Upstream,
                None => InputClockMode::Dac,
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

// ASIO (Windows) and CoreAudio (macOS) share the same runtime-output setup:
// just the device name + adaptive resampling config (no PipeWire buffer tuning).
#[cfg(any(target_os = "windows", target_os = "macos"))]
fn configure_cpal_runtime_output(
    handler: &mut DecodeHandler,
    args: &RenderArgs,
    render_cfg: Option<&renderer::config::RenderConfig>,
) {
    handler.runtime.output_device = args.output_device.clone();
    handler.runtime.adaptive_resampling_config = build_adaptive_resampling_config(args, render_cfg);
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

fn init_spatial_renderer(
    handler: &mut DecodeHandler,
    args: &RenderArgs,
    render_cfg: Option<&renderer::config::RenderConfig>,
    current_layout_from_config: &Option<SpeakerLayout>,
    vbap_cartesian_defaults: bridge_api::RVbapCartesianDefaults,
    preferred_evaluation_mode: bridge_api::RVbapTableMode,
    evaluation_mode_explicit: bool,
) -> Result<()> {
    if !args.enable_vbap {
        return Ok(());
    }

    let layout = resolve_layout(args, current_layout_from_config)?;
    let params = orender_engine::renderer_build::SpatialRendererParams {
        vbap_table: args.vbap_table.clone(),
        evaluation_polar_azimuth_resolution: args.evaluation_polar_azimuth_resolution,
        evaluation_polar_elevation_resolution: args.evaluation_polar_elevation_resolution,
        evaluation_polar_distance_res: args.evaluation_polar_distance_res,
        evaluation_polar_distance_max: args.evaluation_polar_distance_max,
        // None lets the engine follow the bridge's preferred mode (cartesian
        // for OAMD/spatial). Only commit to a concrete EvalMode when the user
        // explicitly picked one via CLI or config, so an unset --eval-mode
        // doesn't lock the pre-compute to the CLI's default Polar.
        render_evaluation_mode: if evaluation_mode_explicit {
            Some(match args.render_evaluation_mode {
                EvaluationModeArg::Polar => orender_engine::renderer_build::EvalMode::Polar,
                EvaluationModeArg::Cartesian => orender_engine::renderer_build::EvalMode::Cartesian,
            })
        } else {
            None
        },
        evaluation_mode_explicit,
        evaluation_cartesian_x_size: args.evaluation_cartesian_x_size,
        evaluation_cartesian_y_size: args.evaluation_cartesian_y_size,
        evaluation_cartesian_z_size: args.evaluation_cartesian_z_size,
        evaluation_cartesian_z_neg_size: args.evaluation_cartesian_z_neg_size,
        vbap_allow_negative_z: args.vbap_allow_negative_z,
        no_vbap_allow_negative_z: args.no_vbap_allow_negative_z,
        render_evaluation_position_interpolation: args.render_evaluation_position_interpolation,
        vbap_distance_model: args.vbap_distance_model.clone(),
        spread_from_distance: args.spread_from_distance,
        spread_distance_range: args.spread_distance_range,
        spread_distance_curve: args.spread_distance_curve,
        vbap_spread_min: args.vbap_spread_min,
        vbap_spread_max: args.vbap_spread_max,
        log_object_positions: args.log_object_positions,
        room_ratio: args.room_ratio.clone(),
        room_ratio_rear: args.room_ratio_rear,
        room_ratio_lower: args.room_ratio_lower,
        room_ratio_center_blend: args.room_ratio_center_blend,
        master_gain: args.master_gain,
        auto_gain: args.auto_gain,
        use_loudness: args.use_loudness,
        distance_diffuse: args.distance_diffuse,
        distance_diffuse_threshold: args.distance_diffuse_threshold,
        distance_diffuse_curve: args.distance_diffuse_curve,
    };

    // The CLI keeps using the stream's native 48 kHz here, matching the
    // previous behaviour; the FFI passes its host sample rate instead.
    let renderer = orender_engine::renderer_build::build_spatial_renderer(
        &params,
        layout,
        48000,
        vbap_cartesian_defaults,
        preferred_evaluation_mode,
        render_cfg,
    )?;
    handler.spatial_renderer = Some(renderer);
    Ok(())
}

fn init_osc_runtime(
    handler: &mut DecodeHandler,
    args: &RenderArgs,
    input_path: &std::path::Path,
    config_path: &Option<std::path::PathBuf>,
) -> Result<()> {
    let render_cfg = render_config_from_path(args, config_path);

    if args.osc {
        use std::net::SocketAddrV4;
        use std::str::FromStr;
        let osc_addr = SocketAddrV4::from_str(&format!("{}:{}", args.osc_host, args.osc_port))?;
        match OscSender::new(osc_addr) {
            Ok(sender) => {
                log::info!("OSC output enabled: {}:{}", args.osc_host, args.osc_port);
                if args.osc_metering {
                    // Pre-subscribe the configured default target to meter
                    // bundles. Without this, metering only flows once a client
                    // (e.g. Studio) sends a runtime enable, so `--osc-metering`
                    // had no effect on a headless/config-driven target.
                    sender.set_default_metering(true);
                    log::info!(
                        "OSC metering pre-enabled for default target {}:{} (--osc-metering)",
                        args.osc_host,
                        args.osc_port
                    );
                }
                handler.telemetry.osc_sender = Some(sender);
            }
            Err(e) => {
                log::error!("Failed to create OSC sender: {}", e);
                return Err(e);
            }
        }
    }

    // Audio meter + diag cadence are initialised AFTER audio_control is
    // attached (see `handler.audio_control = Some(...)` below) so they pick
    // up the shared rate atomics. Reserve a placeholder here so subsequent
    // code can rely on telemetry.audio_meter being Some when the renderer
    // and OSC sender both exist.
    let needs_telemetry =
        matches!(&handler.spatial_renderer, Some(_)) && handler.telemetry.osc_sender.is_some();

    if let Some(renderer) = &handler.spatial_renderer {
        let ctrl = renderer.renderer_control();
        ctrl.set_input_path(Some(input_path.display().to_string()));
        ctrl.set_bridge_path(args.bridge_path.clone());
        let persisted_bridge_path = render_cfg.as_ref().and_then(|cfg| cfg.bridge_path.clone());
        if persisted_bridge_path != args.bridge_path {
            ctrl.mark_dirty();
        }
        // State restored from a live-handoff sidecar is by definition unsaved.
        if config_path
            .as_deref()
            .is_some_and(renderer::config::live_overlay_active)
        {
            ctrl.mark_dirty();
        }

        let drc_mode = render_cfg
            .as_ref()
            .and_then(|cfg| cfg.drc_mode.clone())
            .unwrap_or_else(|| "Off".to_string());
        ctrl.live.write().drc_mode = drc_mode;

        let drc_weight = render_cfg
            .as_ref()
            .and_then(|cfg| cfg.drc_weight)
            .unwrap_or(1.0)
            .clamp(0.0, 1.0);
        ctrl.live.write().drc_weight = drc_weight;

        // Monitoring cadences: seed RendererControl from config (CLI default
        // 50 Hz). Renderer is the source of truth — OSC-adjustable + persisted.
        ctrl.set_meter_rate_hz(
            render_cfg
                .as_ref()
                .and_then(|cfg| cfg.meter_rate)
                .unwrap_or(50.0),
        );
        ctrl.set_diag_rate_hz(
            render_cfg
                .as_ref()
                .and_then(|cfg| cfg.diag_rate)
                .unwrap_or(50.0),
        );

        ctrl.set_requested_ramp_mode(args.ramp_mode.into());
        ctrl.live.write().ramp_mode = args.ramp_mode.into();

        // Declared live options + their param-bag companions and the virtual
        // bed: seeded from the effective config through the shared registry
        // seed — the same call as the embedded host (`Engine::from_paths`), so
        // the two boot paths cannot drift (FFI/CLI parity by construction).
        // The flag-backed options are then overridden from the resolved CLI
        // args, which already folded config through flag > config > default.
        if let Some(render) = render_cfg.as_ref() {
            renderer::options::seed_live_from_config(&mut ctrl.live.write(), render);
        }
        ctrl.live.write().channel_render_mode = args.channel_render_mode.into();
        ctrl.live.write().surround_placement = args.surround_placement.into();

        let requested_latency_target_ms = {
            #[cfg(target_os = "linux")]
            {
                let defaults = PipewireBufferConfig::default();
                Some(args.latency_target_ms.unwrap_or(defaults.latency_ms))
            }
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            {
                Some(
                    args.latency_target_ms
                        .unwrap_or(handler.runtime.latency_target_ms),
                )
            }
            #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
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
            // Live output-backend/file requests start unset; `runtime` holds the
            // launch-resolved values and Studio populates these on demand.
            ..Default::default()
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
            input_requested.node_description.clone(),
            input_requested.sample_format.map(|format| match format {
                InputSampleFormat::F32 => "f32".to_string(),
                InputSampleFormat::S16 => "s16".to_string(),
            }),
        );

        handler.audio_control = Some(Arc::clone(&audio_control));
        handler.input_control = Some(Arc::clone(&input_control));
        if let Some(path) = config_path {
            ctrl.set_config_path(path.clone());
            // Mirror the engine (FFI/mpv) path: record whether the config
            // actually loaded so Studio's About can compare CLI vs host.
            ctrl.set_config_status(Some(
                renderer::config::Config::load_status(path)
                    .as_str()
                    .to_string(),
            ));
        }
        if let Some(sender) = &mut handler.telemetry.osc_sender {
            sender.attach_renderer_control(Arc::clone(&ctrl));
            // The audio output/input layer (audio_output + audio_input + their
            // OSC handlers) lives in the host_audio crate, registered here as
            // the engine's HostControlHandler. The audio-free engine + the
            // embedded mpv host never reference audio_output/audio_input.
            let host = std::sync::Arc::new(host_audio::HostAudio::new(
                ctrl,
                audio_control,
                input_control,
            )) as Arc<dyn runtime_control::HostControlHandler>;
            sender.attach_host_handler(host);
        }
    }

    // Now that `handler.audio_control` is attached, wire the audio meter
    // and the diag publication cadence to the shared rate atomics that
    // OSC handlers update live. Done AFTER the audio_control assignment
    // above — earlier and these reads would all see None and the cadence
    // would never tick.
    if needs_telemetry {
        if let Some(renderer) = &handler.spatial_renderer {
            let num_speakers = renderer.num_speakers();
            // Both monitoring cadences come from RendererControl (source of
            // truth, OSC-adjustable, persisted to config).
            let control = renderer.renderer_control();
            handler.telemetry.audio_meter = Some(AudioMeter::new_with_rate_atomic(
                num_speakers,
                control.meter_rate_atomic(),
            ));
            handler.telemetry.diag_cadence = Some(super::state::DiagPublishCadence::new(
                control.diag_rate_atomic(),
            ));
            log::info!(
                "OSC metering available per client ({} speakers, default 50 Hz, adjustable via /omniphony/control/metering/rate_hz; diag publication via /omniphony/control/diag/rate_hz)",
                num_speakers
            );
        }
    }

    if let (Some(_renderer), Some(sender)) =
        (&handler.spatial_renderer, &mut handler.telemetry.osc_sender)
    {
        sender.start_listener(args.osc_rx_port, true)?;
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
    preferred_evaluation_mode: bridge_api::RVbapTableMode,
    evaluation_mode_explicit: bool,
) -> Result<()> {
    let render_cfg = render_config_from_path(args, config_path);

    #[cfg(target_os = "linux")]
    configure_linux_runtime_output(handler, args, render_cfg.as_ref());
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    configure_cpal_runtime_output(handler, args, render_cfg.as_ref());

    handler.runtime.output_sample_rate = args.output_sample_rate;
    handler.runtime.enable_adaptive_resampling = args.enable_adaptive_resampling;
    handler.runtime.output_file = args.output_file.clone();
    handler.runtime.output_file_format = args.output_file_format;

    init_spatial_renderer(
        handler,
        args,
        render_cfg.as_ref(),
        &current_layout_from_config,
        vbap_cartesian_defaults,
        preferred_evaluation_mode,
        evaluation_mode_explicit,
    )?;
    init_osc_runtime(handler, args, input_path, config_path)?;
    Ok(())
}
