use super::handler::DecodeHandler;
use crate::cli::command::{OutputBackend, RenderArgs};
use anyhow::Result;
use audio_input::{
    InputBackend, InputClockMode, InputControl, InputLfeMode, InputMapMode, InputMode,
    RequestedAudioInputConfig,
};
#[cfg(target_os = "linux")]
use audio_output::pipewire::{PipewireBufferConfig, list_pipewire_output_devices};
use audio_output::{
    AdaptiveResamplingConfig, AudioControl, OutputDeviceOption, RequestedAudioOutputConfig,
};
use orender_engine::osc::OscSender;
use renderer::live_params::RendererControl;
use renderer::metering::AudioMeter;
use renderer::speaker_layout::SpeakerLayout;
use std::sync::Arc;

/// Monitoring cadences this host falls back to when the config declares none.
///
/// Higher than the embedded host's: this is the renderer Studio talks to, and
/// the meters and diag plots are only as smooth as this rate.
/// Meter then diag, in Hz.
const CLI_CADENCE_DEFAULTS_HZ: (f32, f32) = (50.0, 50.0);

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

fn build_adaptive_resampling_config(
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
        update_interval_callbacks: render_cfg
            .and_then(|cfg| cfg.adaptive_resampling_update_interval_callbacks)
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
        let input_mode = render_cfg.input_mode_or_default();
        requested.mode = match input_mode {
            renderer::config::InputModeConfig::Pipewire => InputMode::Pipewire,
            renderer::config::InputModeConfig::Bridge => InputMode::Bridge,
        };

        if let Some(live_input) = render_cfg.live_input.as_ref() {
            requested.backend = live_input.backend.as_ref().map(|backend| match backend {
                renderer::config::InputBackendConfig::Pipewire => InputBackend::Pipewire,
            });
            requested.node_name = live_input.node.clone();
            requested.node_description = live_input.description.clone();
            requested.layout_path = live_input.layout.clone();
            requested.current_layout = live_input.current_layout.clone();
            requested.clock_mode = match live_input.clock_mode_or_default(&input_mode) {
                renderer::config::InputClockModeConfig::Pipewire => InputClockMode::Pipewire,
                renderer::config::InputClockModeConfig::Upstream => InputClockMode::Upstream,
                renderer::config::InputClockModeConfig::Dac => InputClockMode::Dac,
            };
            requested.channels = live_input.channels;
            requested.sample_rate_hz = live_input.sample_rate;
            requested.map_mode = match live_input.map_or_default() {
                renderer::config::InputMapModeConfig::SevenOneFixed => InputMapMode::SevenOneFixed,
            };
            requested.lfe_mode = match live_input.lfe_mode_or_default() {
                renderer::config::InputLfeModeConfig::Object => InputLfeMode::Object,
                renderer::config::InputLfeModeConfig::Drop => InputLfeMode::Drop,
                renderer::config::InputLfeModeConfig::Direct => InputLfeMode::Direct,
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
    handler.runtime.adaptive_resampling_config = build_adaptive_resampling_config(render_cfg);
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
    handler.runtime.adaptive_resampling_config = build_adaptive_resampling_config(render_cfg);
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
    render_cfg: &renderer::config::RenderConfig,
    params: &orender_engine::renderer_build::SpatialRendererParams,
    current_layout_from_config: &Option<SpeakerLayout>,
    vbap_cartesian_defaults: bridge_api::RVbapCartesianDefaults,
    preferred_evaluation_mode: bridge_api::RVbapTableMode,
) -> Result<()> {
    if !args.enable_vbap {
        return Ok(());
    }

    let layout = resolve_layout(args, current_layout_from_config)?;
    // Built before any frame is decoded, so at the rate of the formats this
    // host is fed; the first frame at another rate re-targets the renderer
    // (`follow_stream_rate`, as the embedded engine does).
    let renderer = orender_engine::renderer_build::build_spatial_renderer(
        params,
        layout,
        48000,
        vbap_cartesian_defaults,
        preferred_evaluation_mode,
        Some(render_cfg),
    )?;
    handler.spatial_renderer = Some(renderer);
    Ok(())
}

fn init_osc_runtime(
    handler: &mut DecodeHandler,
    args: &RenderArgs,
    render_cfg: &renderer::config::RenderConfig,
    input_path: &std::path::Path,
    config_path: &Option<std::path::PathBuf>,
) -> Result<()> {
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

    if let Some(ctrl) = handler
        .spatial_renderer
        .as_ref()
        .map(|renderer| renderer.renderer_control())
    {
        // The channel-object stages' schemas and the fixed-channel catalogue,
        // as the engine publishes them. Without them this host sent Studio
        // empty lists: no height generator to pick, no phantom-extraction
        // parameters and no channel catalogue for the bed editor.
        handler
            .spatial
            .stream
            .channel_objects
            .publish_static_state(&ctrl);
        // Bridge path, config path/status/profiles, a restored handoff's
        // unsaved mark, monitoring cadences and the runtime seed (ramp mode,
        // declared live options + their param bags and the virtual bed, DRC
        // selection): the shared host seed, also run by the no-bridge runtime
        // of both hosts. `args.bridge_path` is the flag, else the config's (or
        // the path a live reload switched to): recorded as asked, dirty when
        // it is not the config's (`record_bridge_path`). This host's cadence
        // fallback is faster than the embedded one (Studio's meters and diag
        // plots read it), and a later profile switch falls back to it too.
        orender_engine::renderer_build::seed_host_state(
            &ctrl,
            &orender_engine::renderer_build::HostStateSeed {
                config_path: config_path.as_deref(),
                render_cfg: Some(render_cfg),
                requested_bridge_path: args.bridge_path.as_deref(),
                cadence_defaults_hz: CLI_CADENCE_DEFAULTS_HZ,
            },
        );
        let host = attach_cli_host_state(handler, args, render_cfg, input_path, &ctrl);
        if let Some(sender) = &mut handler.telemetry.osc_sender {
            sender.attach_renderer_control(Arc::clone(&ctrl));
            sender.attach_host_handler(host);
        }
    }

    // After `attach_cli_host_state`, so the meter and diag cadence pick up
    // the shared rate atomics.
    init_telemetry(handler);

    if let (Some(_renderer), Some(sender)) =
        (&handler.spatial_renderer, &mut handler.telemetry.osc_sender)
    {
        sender.start_listener(args.osc_rx_port, true)?;
    }

    Ok(())
}

/// This host's own part of a renderer's state, on top of the shared host seed:
/// the input path, the flag-backed live settings, and the audio output/input
/// controls behind the returned OSC handler (`host_audio::HostAudio`). The
/// engine and the embedded mpv host never reference audio_output/audio_input.
/// Shared by the render bootstrap and the no-bridge idle runtime.
fn attach_cli_host_state(
    handler: &mut DecodeHandler,
    args: &RenderArgs,
    render_cfg: &renderer::config::RenderConfig,
    input_path: &std::path::Path,
    ctrl: &Arc<RendererControl>,
) -> Arc<dyn runtime_control::HostControlHandler> {
    ctrl.set_input_path(Some(input_path.display().to_string()));
    // The channel render mode is a command-line override, never a setting:
    // after the seed. (The registered options' flags are in `render_cfg`,
    // which the seed already applied.)
    ctrl.live.write().channel_render_mode = args.channel_render_mode.into();

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
    let input_control = Arc::new(InputControl::new(build_requested_input_config(Some(
        render_cfg,
    ))));

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
        None,
    );

    handler.audio_control = Some(Arc::clone(&audio_control));
    handler.input_control = Some(Arc::clone(&input_control));
    Arc::new(host_audio::HostAudio::new(
        Arc::clone(ctrl),
        audio_control,
        input_control,
    ))
}

/// Wire the audio meter and the diag publication cadence to the shared rate
/// atomics OSC handlers update live, when both a renderer and an OSC sender
/// exist. Run once `handler.audio_control` is attached.
fn init_telemetry(handler: &mut DecodeHandler) {
    if handler.telemetry.osc_sender.is_none() {
        return;
    }
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

/// The launch-resolved output runtime (device, buffer, adaptive resampling,
/// file output) the handler's output coordinator works from.
fn configure_host_runtime(
    handler: &mut DecodeHandler,
    args: &RenderArgs,
    render_cfg: &renderer::config::RenderConfig,
) {
    #[cfg(target_os = "linux")]
    configure_linux_runtime_output(handler, args, Some(render_cfg));
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    configure_cpal_runtime_output(handler, args, Some(render_cfg));

    handler.runtime.output_sample_rate = args.output_sample_rate;
    handler.runtime.enable_adaptive_resampling = args.enable_adaptive_resampling;
    handler.runtime.output_file = args.output_file.clone();
    handler.runtime.output_file_format = args.output_file_format;
}

/// What the idle runtime needs to come up without a bridge: the resolved run.
pub struct NoBridgeInputs<'a> {
    pub args: &'a RenderArgs,
    pub render_cfg: &'a renderer::config::RenderConfig,
    pub params: &'a orender_engine::renderer_build::SpatialRendererParams,
    pub input_path: &'a std::path::Path,
    pub config_path: &'a Option<std::path::PathBuf>,
    /// The bridge load error, in full (logged; published shortened).
    pub bridge_error: String,
}

/// Bring the handler up without a bridge: the shared no-bridge runtime
/// (`orender_engine::degraded::NoBridgeRuntime`, the one liborender keeps up
/// for mpv) plus this host's part — its output runtime, input path,
/// flag-backed settings and audio controls, attached before the OSC server
/// starts, so Studio can still pick an output device and fix the bridge path.
/// Unlike a render, it always builds its renderer, `--enable-vbap` or not: the
/// renderer never renders here, it is what the OSC state is served from.
pub fn init_no_bridge_handler(
    handler: &mut DecodeHandler,
    inputs: NoBridgeInputs<'_>,
) -> Result<()> {
    let NoBridgeInputs {
        args,
        render_cfg,
        params,
        input_path,
        config_path,
        bridge_error,
    } = inputs;
    configure_host_runtime(handler, args, render_cfg);
    let mut runtime = orender_engine::NoBridgeRuntime::build(orender_engine::NoBridgeSetup {
        config_path: config_path.clone(),
        render_cfg: Some(render_cfg.clone()),
        renderer_params: params.clone(),
        speaker_layout_path: args.speaker_layout.clone(),
        requested_bridge_path: args.bridge_path.clone(),
        // As a render builds it before any frame (`init_spatial_renderer`).
        sample_rate: 48000,
        cadence_defaults_hz: CLI_CADENCE_DEFAULTS_HZ,
        bridge_error,
        host_abi: None,
    })?;
    let ctrl = runtime.control();
    let host = attach_cli_host_state(handler, args, render_cfg, input_path, &ctrl);
    runtime.start_osc(
        &orender_engine::OscOptions {
            host: args.osc_host.clone(),
            port_out: args.osc_port,
            port_in: args.osc_rx_port,
            metering: args.osc_metering,
        },
        Some(host),
        true,
    )?;
    let (renderer, osc_sender) = runtime.into_parts();
    handler.spatial_renderer = Some(renderer);
    handler.telemetry.osc_sender = osc_sender;
    handler.spatial.stream.coordinate_format =
        orender_engine::degraded::NO_BRIDGE_COORDINATE_FORMAT;
    init_telemetry(handler);

    // The input panel says what to do about it.
    if let (Some(input_control), Some(summary)) =
        (handler.input_control.as_ref(), ctrl.bridge_error())
    {
        input_control.set_input_error(Some(format!(
            "Bridge unavailable ({summary}). Set a working bridge binary path and Apply."
        )));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn init_render_handler(
    handler: &mut DecodeHandler,
    args: &RenderArgs,
    render_cfg: &renderer::config::RenderConfig,
    params: &orender_engine::renderer_build::SpatialRendererParams,
    input_path: &std::path::Path,
    config_path: &Option<std::path::PathBuf>,
    current_layout_from_config: Option<renderer::speaker_layout::SpeakerLayout>,
    vbap_cartesian_defaults: bridge_api::RVbapCartesianDefaults,
    preferred_evaluation_mode: bridge_api::RVbapTableMode,
) -> Result<()> {
    configure_host_runtime(handler, args, render_cfg);
    init_spatial_renderer(
        handler,
        args,
        render_cfg,
        params,
        &current_layout_from_config,
        vbap_cartesian_defaults,
        preferred_evaluation_mode,
    )?;
    init_osc_runtime(handler, args, render_cfg, input_path, config_path)?;
    Ok(())
}
