use super::bootstrap::init_render_handler;
use super::config_resolution::{effective_to_config, merge_render_config};
use super::decoder_thread::{
    DecodedAudioData, DecoderMessage, DecoderThreadConfig, spawn_decoder_thread,
};
use super::handler::DecodeHandler;
use super::live_input::{LiveBridgeRuntimeConfig, spawn_live_input_manager};
use super::state::{FrameHandlerContext, WriterState};
use crate::bridge_loader::{LoadedBridge, resolve_bridge_path};
use crate::cli::command::{Cli, EvaluationModeArg, OutputBackend, RenderArgSources, RenderArgs};
use anyhow::Result;
use log::Level;
use std::sync::mpsc;
use std::time::Duration;

const DEFAULT_DECODE_QUEUE_LATENCY_MS: u32 = 220;
const DECODE_QUEUE_MESSAGES_PER_MS: usize = 2;
const MIN_DECODE_QUEUE_CAPACITY: usize = 512;
const MAX_DECODE_QUEUE_CAPACITY: usize = 8192;

const IDLE_BRIDGE_COORDINATE_FORMAT: bridge_api::RCoordinateFormat =
    bridge_api::RCoordinateFormat::Cartesian;
const IDLE_BRIDGE_VBAP_DEFAULTS: bridge_api::RVbapCartesianDefaults =
    bridge_api::RVbapCartesianDefaults {
        x_size: 62,
        y_size: 62,
        z_size: 15,
        allow_negative_z: false,
    };
const IDLE_BRIDGE_PREFERRED_EVALUATION_MODE: bridge_api::RVbapTableMode =
    bridge_api::RVbapTableMode::Cartesian;

struct PreparedDecodeRun {
    state: WriterState,
    tx: mpsc::SyncSender<Result<DecoderMessage>>,
    rx: mpsc::Receiver<Result<DecoderMessage>>,
    decode_thread: std::thread::JoinHandle<Result<()>>,
    _shutdown: sys::ShutdownHandle,
    bridge_lib: bridge_api::BridgeLibRef,
    input_path: std::path::PathBuf,
    strict_mode: bool,
    presentation: String,
    is_spatial_presentation: bool,
    coordinate_format: bridge_api::RCoordinateFormat,
    vbap_cartesian_defaults: bridge_api::RVbapCartesianDefaults,
    preferred_evaluation_mode: bridge_api::RVbapTableMode,
}

fn resolve_effective_decode_args(
    args: &RenderArgs,
    cli: &Cli,
    arg_sources: &RenderArgSources<'_>,
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
    let evaluation_mode_explicit = arg_sources.is_explicit("render_evaluation_mode")
        || cfg
            .render
            .as_ref()
            .and_then(|rc| rc.render_evaluation_mode.as_ref())
            .is_some();
    if let Some(rc) = &cfg.render {
        merge_render_config(rc, &mut effective, arg_sources);
    }

    let current_layout = cfg.render.and_then(|rc| rc.current_layout);
    (
        config_path,
        effective,
        current_layout,
        evaluation_mode_explicit,
    )
}

fn decode_queue_capacity(latency_target_ms: Option<u32>) -> usize {
    let target_ms = latency_target_ms
        .unwrap_or(DEFAULT_DECODE_QUEUE_LATENCY_MS)
        .max(1);
    (target_ms as usize)
        .saturating_mul(DECODE_QUEUE_MESSAGES_PER_MS)
        .clamp(MIN_DECODE_QUEUE_CAPACITY, MAX_DECODE_QUEUE_CAPACITY)
}

fn is_bridge_unavailable_error(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        let text = cause.to_string();
        text.contains("No bridge plugin found")
            || text.contains("Bridge path '")
            || text.contains("Failed to load bridge plugin from")
            || text.contains("Bridge plugin is missing the `new_bridge` export")
    })
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
    let preferred_evaluation_mode = bridge.preferred_vbap_table_mode();
    log::info!("Bridge coordinate format: {:?}", coordinate_format);
    log::info!(
        "Bridge cartesian VBAP defaults: x={}, y={}, z={}, allow_negative_z={}",
        vbap_cartesian_defaults.x_size,
        vbap_cartesian_defaults.y_size,
        vbap_cartesian_defaults.z_size,
        vbap_cartesian_defaults.allow_negative_z
    );
    log::info!(
        "Bridge preferred evaluation mode: {:?}",
        preferred_evaluation_mode
    );

    let queue_capacity = decode_queue_capacity(args.latency_target_ms);
    log::info!(
        "Decode queue capacity: {} messages (~{} ms at 40-sample frames)",
        queue_capacity,
        queue_capacity / DECODE_QUEUE_MESSAGES_PER_MS
    );
    let (tx, rx) = mpsc::sync_channel(queue_capacity);
    let shutdown = sys::shutdown::ShutdownHandle::install()?;
    let shutdown_signal = shutdown.shutdown_signal();

    let decode_thread = spawn_decoder_thread(DecoderThreadConfig {
        input_path: input.clone(),
        strict_mode,
        continuous: args.continuous,
        drain_pipe: !args.no_drain_pipe,
        tx: tx.clone(),
        bridge,
        shutdown_signal,
    });

    Ok(PreparedDecodeRun {
        state,
        tx,
        rx,
        decode_thread,
        _shutdown: shutdown,
        bridge_lib: lib,
        input_path: input,
        strict_mode,
        presentation: args.presentation.clone(),
        is_spatial_presentation,
        coordinate_format,
        vbap_cartesian_defaults,
        preferred_evaluation_mode,
    })
}

fn idle_input_path(args: &RenderArgs) -> &std::path::Path {
    args.input
        .as_deref()
        .unwrap_or_else(|| std::path::Path::new("-"))
}

fn run_idle_runtime(
    args: &RenderArgs,
    config_path: &Option<std::path::PathBuf>,
    current_layout_from_config: Option<renderer::speaker_layout::SpeakerLayout>,
    evaluation_mode_explicit: bool,
    bridge_error: &anyhow::Error,
) -> Result<Option<std::path::PathBuf>> {
    let shutdown = sys::shutdown::ShutdownHandle::install()?;
    let mut handler = DecodeHandler::default();
    init_render_handler(
        &mut handler,
        args,
        idle_input_path(args),
        config_path,
        current_layout_from_config,
        IDLE_BRIDGE_VBAP_DEFAULTS,
        IDLE_BRIDGE_PREFERRED_EVALUATION_MODE,
        evaluation_mode_explicit,
    )?;
    handler.spatial.coordinate_format = IDLE_BRIDGE_COORDINATE_FORMAT;
    if let Some(input_control) = handler.input_control.as_ref() {
        input_control.set_input_error(Some(
            "Bridge path missing. Set a bridge binary path and Apply.".to_string(),
        ));
    }

    log::warn!(
        "Bridge unavailable, starting idle OSC runtime without decode/audio session: {bridge_error:#}"
    );
    log::warn!(
        "The renderer will stay idle until /omniphony/control/reload_config is requested with a valid render.bridge_path."
    );

    let _shutdown = shutdown;
    sys::notify_ready();
    while !sys::ShutdownHandle::is_requested()
        && !sys::ShutdownHandle::is_restart_from_config_requested()
    {
        handler.poll_runtime_state()?;
        std::thread::sleep(Duration::from_millis(50));
    }

    if sys::ShutdownHandle::is_requested() {
        sys::notify_stopping();
    }

    Ok(handler
        .spatial_renderer
        .as_ref()
        .map(|renderer| renderer.renderer_control().bridge_path())
        .unwrap_or_else(|| args.bridge_path.clone()))
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
    log::info!("Stream ended, finalizing current output and resetting handler...");
    handler.finalize()?;

    if args.auto_gain {
        log_auto_gain_summary(handler);
    }

    let spatial_renderer = handler.spatial_renderer.take();
    let audio_control = handler.audio_control.take();
    let input_control = handler.input_control.take();
    let osc_sender = handler.telemetry.osc_sender.take();
    let audio_meter = handler.telemetry.audio_meter.take();
    let runtime = handler.runtime.clone();

    *handler = DecodeHandler::default();

    handler.spatial_renderer = spatial_renderer;
    handler.audio_control = audio_control;
    handler.input_control = input_control;
    handler.telemetry.osc_sender = osc_sender;
    handler.telemetry.audio_meter = audio_meter;
    handler.runtime = runtime;
    if let Some(ref mut osc_sender) = handler.telemetry.osc_sender {
        osc_sender.bump_content_generation();
    }

    log::info!("Handler reset complete, ready for next stream");
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
    if !handler.should_accept_source(decoded.source) {
        return handler.poll_runtime_state();
    }
    let frame = decoded.frame;
    if frame.is_new_segment {
        handler.spatial.segment_start_samples = handler.session.decoded_samples;
        handler.handle_stream_restart(
            ctx.effective_output_backend,
            frame.sampling_frequency,
            frame.channel_count as usize,
            ctx.args.bed_conform,
        )?;
        handler.spatial.is_segmented = true;
    }

    let ctx = FrameHandlerContext {
        output_backend: ctx.effective_output_backend,
        state: ctx.state,
        bed_conform: ctx.args.bed_conform,
        use_loudness: ctx.args.use_loudness,
        decode_time_ms: decoded.decode_time_ms,
        queue_delay_ms: decoded.sent_at.elapsed().as_secs_f32() * 1000.0,
    };
    handler.handle_decoded_frame(decoded.source, frame, &ctx)
}

fn process_decoder_messages(
    rx: &mpsc::Receiver<Result<DecoderMessage>>,
    handler: &mut DecodeHandler,
    ctx: &DecodeRunContext<'_>,
) -> Result<()> {
    loop {
        let result = match rx.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if sys::ShutdownHandle::is_requested()
                    || sys::ShutdownHandle::is_restart_from_config_requested()
                {
                    break;
                }
                handler.poll_runtime_state()?;
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        match result {
            Ok(DecoderMessage::AudioData(frame)) => handle_audio_message(handler, frame, ctx)?,
            Ok(DecoderMessage::FlushRequest(source)) => {
                if handler.should_accept_source(source) {
                    handler.handle_decoder_flush_request();
                } else {
                    handler.poll_runtime_state()?;
                }
            }
            Ok(DecoderMessage::StreamEnd(source)) => {
                if handler.should_accept_source(source) {
                    handle_stream_end(handler, ctx.args)?;
                } else {
                    handler.poll_runtime_state()?;
                }
            }
            Err(err) => return Err(err),
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
    if is_shutdown {
        if let Err(err) = handler.finalize() {
            log::warn!("Error flushing audio during shutdown (ignored): {err}");
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
                if args.auto_gain {
                    log_auto_gain_summary(handler);
                }
            }
            Ok(())
        }
        Ok(Err(err)) => Err(err),
        Err(_) => Err(anyhow::anyhow!("Decode thread panicked")),
    }
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

fn run_prepared_render(
    prepared: PreparedDecodeRun,
    args: &RenderArgs,
    config_path: &Option<std::path::PathBuf>,
    current_layout_from_config: Option<renderer::speaker_layout::SpeakerLayout>,
    evaluation_mode_explicit: bool,
) -> Result<Option<std::path::PathBuf>> {
    let mut effective_args = args.clone();
    if !evaluation_mode_explicit {
        effective_args.render_evaluation_mode = match prepared.preferred_evaluation_mode {
            bridge_api::RVbapTableMode::Polar => EvaluationModeArg::Polar,
            bridge_api::RVbapTableMode::Cartesian => EvaluationModeArg::Cartesian,
        };
        log::info!(
            "Using bridge-preferred evaluation mode: {:?}",
            effective_args.render_evaluation_mode
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
        prepared.preferred_evaluation_mode,
        evaluation_mode_explicit,
    )?;
    handler.spatial.coordinate_format = prepared.coordinate_format;
    let live_input_manager = handler
        .input_control
        .as_ref()
        .zip(handler.audio_control.as_ref())
        .map(|(input_control, audio_control)| {
            spawn_live_input_manager(
                prepared.tx.clone(),
                input_control.clone(),
                audio_control.clone(),
                LiveBridgeRuntimeConfig {
                    lib: prepared.bridge_lib.clone(),
                    strict_mode: prepared.strict_mode,
                    presentation: prepared.presentation.clone(),
                    clock_mode: input_control.requested_snapshot().clock_mode,
                },
            )
        });

    let run_result = run_render_message_phase(&prepared, &mut handler, &effective_args);
    if let Some(manager) = live_input_manager {
        manager.stop();
    }
    run_result?;
    let current_bridge_path = handler
        .spatial_renderer
        .as_ref()
        .map(|renderer| renderer.renderer_control().bridge_path())
        .unwrap_or_else(|| effective_args.bridge_path.clone());
    finalize_render_run(prepared, &mut handler, &effective_args)?;
    Ok(current_bridge_path)
}

pub fn cmd_render(args: &RenderArgs, cli: &Cli, arg_sources: &RenderArgSources<'_>) -> Result<()> {
    let mut restart_bridge_path_override: Option<Option<std::path::PathBuf>> = None;
    loop {
        let (config_path, mut effective_args, current_layout_from_config, evaluation_mode_explicit) =
            resolve_effective_decode_args(args, cli, arg_sources);
        if let Some(bridge_path) = restart_bridge_path_override.take() {
            effective_args.bridge_path = bridge_path;
        }
        let args = &effective_args;

        if maybe_save_effective_config(cli, args, &config_path)? {
            return Ok(());
        }

        let bridge_path_after_run = match prepare_render_run(args, cli) {
            Ok(prepared) => run_prepared_render(
                prepared,
                args,
                &config_path,
                current_layout_from_config,
                evaluation_mode_explicit,
            )?,
            Err(err) if args.osc && is_bridge_unavailable_error(&err) => run_idle_runtime(
                args,
                &config_path,
                current_layout_from_config,
                evaluation_mode_explicit,
                &err,
            )?,
            Err(err) => return Err(err),
        };

        if sys::ShutdownHandle::is_restart_from_config_requested() {
            sys::ShutdownHandle::clear_restart_from_config();
            if sys::ShutdownHandle::is_requested() {
                return Ok(());
            }
            restart_bridge_path_override = Some(bridge_path_after_run);
            log::info!("Restarting render pipeline from config");
            continue;
        }

        return Ok(());
    }
}
