use super::bootstrap::init_render_handler;
use super::config_resolution::{effective_to_config, merge_render_config};
use super::decoder_thread::{
    DecodedAudioData, DecoderCommand, DecoderMessage, DecoderThreadConfig, PipeInputDiag,
    spawn_decoder_thread,
};
use super::handler::DecodeHandler;
use super::live_input::{LiveBridgeRuntimeConfig, spawn_live_input_manager};
use super::state::FrameHandlerContext;
use crate::cli::command::{Cli, EvaluationModeArg, OutputBackend, RenderArgSources, RenderArgs};
use anyhow::Result;
use orender_engine::bridge_loader::{LoadedBridge, resolve_bridge_path};
use std::sync::mpsc;
use std::sync::{Arc, atomic::AtomicU64};
use std::time::Duration;
use sys::diag::DiagAtomicHandle;

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
    tx: mpsc::SyncSender<Result<DecoderMessage>>,
    rx: mpsc::Receiver<Result<DecoderMessage>>,
    cmd_tx: mpsc::Sender<DecoderCommand>,
    decode_thread: std::thread::JoinHandle<Result<()>>,
    /// Receives per-packet emitted audio duration (microseconds) from the
    /// decoder thread; consumed by the pure pipe-bridge pacer drain thread.
    drain_rx: Option<mpsc::Receiver<u64>>,
    pipe_input_diag: PipeInputDiag,
    pacer_bridge_diag: PacerBridgeDiag,
    _shutdown: sys::ShutdownHandle,
    bridge_lib: bridge_api::BridgeLibRef,
    input_path: std::path::PathBuf,
    presentation: String,
    is_spatial_presentation: bool,
    coordinate_format: bridge_api::RCoordinateFormat,
    vbap_cartesian_defaults: bridge_api::RVbapCartesianDefaults,
    preferred_evaluation_mode: bridge_api::RVbapTableMode,
    supported_drc_modes: Vec<String>,
}

#[derive(Clone)]
struct PacerBridgeDiag {
    emitted_us: Arc<AtomicU64>,
    drain_samples: Arc<AtomicU64>,
    frac_frames: Arc<AtomicU64>,
    drain_dt_us: Arc<AtomicU64>,
}

impl PacerBridgeDiag {
    fn handles(&self) -> [DiagAtomicHandle; 4] {
        [
            DiagAtomicHandle {
                name: "pacer_bridge_emitted_us",
                label: "Pacer token duration",
                group: "pacer_pipe",
                unit: "us",
                atomic: Arc::clone(&self.emitted_us),
            },
            DiagAtomicHandle {
                name: "pacer_bridge_drain_samples",
                label: "Pacer drain samples",
                group: "pacer_pipe",
                unit: "samples",
                atomic: Arc::clone(&self.drain_samples),
            },
            DiagAtomicHandle {
                name: "pacer_bridge_frac_frames",
                label: "Pacer fractional frames",
                group: "pacer_pipe",
                unit: "frames",
                atomic: Arc::clone(&self.frac_frames),
            },
            DiagAtomicHandle {
                name: "pacer_bridge_drain_dt_us",
                label: "Pacer drain dt",
                group: "pacer_pipe",
                unit: "us",
                atomic: Arc::clone(&self.drain_dt_us),
            },
        ]
    }
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
    // Sidecar-aware load: when a yielded predecessor handed over unsaved live
    // state, the args fold below must see it (apply_render_cfg_overrides later
    // writes folded values like master_gain back over the render config, so a
    // base-config fold would silently undo the handoff).
    let cfg = config_path
        .as_deref()
        .map(|p| renderer::config::Config::load_or_default_with_live(p).0)
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
            // Matches both `resolve_bridge_path` messages: "bridge path '…'" (CLI)
            // and "render.bridge_path '…' (from config)". The previous
            // "Bridge path '" (capital B) matched neither, so a bad/missing
            // bridge path hard-exited instead of entering the idle OSC runtime.
            || text.contains("does not exist or is not a file")
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

    let existing_render_cfg = renderer::config::Config::load_or_default(&path).render;
    let config = effective_to_config(args, cli, existing_render_cfg.as_ref())?;
    config.save(&path)?;
    log::info!("Config written to: {}", path.display());
    Ok(true)
}

fn prepare_render_run(args: &RenderArgs) -> Result<PreparedDecodeRun> {
    let input = args
        .input
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Must specify INPUT file"))?
        .clone();

    log::info!(
        "Decoding stream from file: {} (presentation: {})",
        input.display(),
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

    let bridge_path = resolve_bridge_path(args.bridge_path.as_deref())?;
    log::info!("Loading format bridge: {}", bridge_path.display());
    let LoadedBridge { lib, mut bridge } = LoadedBridge::load_with_params(&bridge_path)?;
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
    let supported_drc_modes: Vec<String> = bridge
        .supported_drc_modes()
        .iter()
        .map(|s: &abi_stable::std_types::RString| s.to_string())
        .collect();
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
    let (cmd_tx, cmd_rx) = mpsc::channel();
    // Unbounded so the decoder never blocks posting a drain token (a bounded
    // channel here would re-introduce the very backpressure deadlock this
    // pacer drain path exists to avoid).
    let (drain_tx, drain_rx) = mpsc::channel::<u64>();
    let pipe_input_diag = PipeInputDiag {
        chunk_bytes: Arc::new(AtomicU64::new(0)),
        chunk_dt_us: Arc::new(AtomicU64::new(0)),
        audio_ms_per_chunk: Arc::new(AtomicU64::new(0)),
        gap_over_audio_ms: Arc::new(AtomicU64::new(0)),
    };
    let pacer_bridge_diag = PacerBridgeDiag {
        emitted_us: Arc::new(AtomicU64::new(0)),
        drain_samples: Arc::new(AtomicU64::new(0)),
        frac_frames: Arc::new(AtomicU64::new(0)),
        drain_dt_us: Arc::new(AtomicU64::new(0)),
    };
    let shutdown = sys::shutdown::ShutdownHandle::install()?;
    let shutdown_signal = shutdown.shutdown_signal();

    let decode_thread = spawn_decoder_thread(DecoderThreadConfig {
        input_path: input.clone(),
        continuous: args.continuous,
        drain_pipe: !args.no_drain_pipe,
        tx: tx.clone(),
        cmd_rx,
        drain_tx: Some(drain_tx),
        pipe_input_diag: Some(pipe_input_diag.clone()),
        bridge,
        shutdown_signal,
    });

    Ok(PreparedDecodeRun {
        tx,
        rx,
        cmd_tx,
        decode_thread,
        drain_rx: Some(drain_rx),
        pipe_input_diag,
        pacer_bridge_diag,
        _shutdown: shutdown,
        bridge_lib: lib,
        input_path: input,
        presentation: args.presentation.clone(),
        is_spatial_presentation,
        coordinate_format,
        vbap_cartesian_defaults,
        preferred_evaluation_mode,
        supported_drc_modes,
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
        // An idle holder of the OSC port must still yield to an mpv-embedded
        // renderer: release the port, idle, and re-acquire it on resume — exactly
        // like the decode loop. Without this the port-9000 handoff never happens
        // when the standby has no bridge configured.
        if sys::shutdown::is_standby_requested() {
            sys::shutdown::take_standby_request();
            standby_idle_until_resume(&mut handler, || {});
        }
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
        if renderer.auto_gain_triggered() {
            let master_gain = renderer.renderer_control().live.read().master_gain;
            log::warn!(
                "Auto-gain: master gain was lowered to {:.4} ({:.1} dB) to avoid clipping; \
                 save the config to keep it for future playback.",
                master_gain,
                20.0 * master_gain.log10()
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
        // Use the live-active backend (not the launch one) so a segment
        // restart preserves a Studio-requested switch (e.g. to `file`).
        handler.handle_stream_restart(
            handler.runtime.active_output_backend,
            frame.sampling_frequency,
            frame.channel_count as usize,
            ctx.args.bed_conform,
        )?;
        handler.spatial.is_segmented = true;
    }

    let ctx = FrameHandlerContext {
        bed_conform: ctx.args.bed_conform,
        use_loudness: ctx.args.use_loudness,
        decode_time_ms: decoded.decode_time_ms,
        queue_delay_ms: decoded.sent_at.elapsed().as_secs_f32() * 1000.0,
    };
    handler.handle_decoded_frame(decoded.source, frame, &ctx)
}

/// Standby handoff: an mpv-embedded renderer asked for the OSC port. Release the
/// audio output (so an exclusive backend like ASIO frees the device for mpv) and
/// the OSC RX port, then idle — keeping the engine + VBAP table warm — until a
/// `resume` arrives (mpv exited) or the process is asked to quit. The audio
/// writer rebuilds itself lazily on the first decoded frame after resume.
fn run_standby_until_resume(
    rx: &mpsc::Receiver<Result<DecoderMessage>>,
    handler: &mut DecodeHandler,
) {
    sys::shutdown::take_standby_request();
    handler.output.audio_writer = None;
    // Discard any frames the decoder rendered while standing by so the decoder
    // thread never blocks on a full channel and no stale audio is played on
    // resume.
    standby_idle_until_resume(handler, || while rx.try_recv().is_ok() {});
}

/// Shared standby core: release the OSC RX port (and let the caller release the
/// audio output beforehand), then idle until a `resume` arrives on the dynamic
/// port (mpv exited) or the process is asked to quit/restart. `drain` runs each
/// tick to discard buffered work (a no-op for the idle runtime, which has no
/// decoder channel). On resume the OSC port is re-acquired.
///
/// Used by both the decode loop ([`run_standby_until_resume`]) and the
/// bridge-unavailable idle runtime, so an idle holder of the OSC port yields to
/// mpv exactly like an actively-decoding one.
fn standby_idle_until_resume(handler: &mut DecodeHandler, mut drain: impl FnMut()) {
    loop {
        log::info!("Entering standby: releasing OSC port (and audio output) for mpv");
        if let Some(osc) = handler.telemetry.osc_sender.as_mut() {
            osc.enter_standby();
        }
        loop {
            if sys::ShutdownHandle::is_requested()
                || sys::ShutdownHandle::is_restart_from_config_requested()
            {
                return;
            }
            if sys::shutdown::take_resume_request() {
                break;
            }
            drain();
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        log::info!("Resuming from standby: re-acquiring OSC port (and audio output)");
        let reacquired = match handler.telemetry.osc_sender.as_mut() {
            Some(osc) => {
                if let Err(e) = osc.resume() {
                    log::error!("standby resume: failed to re-bind the OSC port: {e}");
                }
                osc.is_listening()
            }
            // No OSC sender → nothing to re-acquire; treat the resume as done.
            None => true,
        };
        if reacquired {
            return;
        }
        // The resume could not re-bind the RX port: it is still held (a
        // premature/lost resume, e.g. mpv still owns it after a track switch).
        // Returning here would run the decoder with no OSC listener — a "zombie"
        // that strands Studio on `reconnecting`. Re-arm standby instead; the
        // watch thread's 2 s port-probe safety net resumes for real once the
        // port frees (mpv quits).
        log::warn!(
            "standby resume could not re-acquire the OSC port (still held); re-arming standby"
        );
    }
}

fn process_decoder_messages(
    rx: &mpsc::Receiver<Result<DecoderMessage>>,
    handler: &mut DecodeHandler,
    ctx: &DecodeRunContext<'_>,
) -> Result<()> {
    loop {
        if sys::shutdown::is_standby_requested() {
            run_standby_until_resume(rx, handler);
        }
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
    // Seed the live-mutable active backend so a Studio switch (e.g. to `file`)
    // has a defined starting point.
    handler.runtime.active_output_backend =
        effective_output_backend(args, prepared.is_spatial_presentation)?;
    let run_ctx = DecodeRunContext { args };

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

/// Drains the post-rendering output pacer FIFO into the ring for pure
/// pipe-bridge mode, where no PipeWire input RT callback exists to do it.
///
/// The clock is the decoder's source clock, conveyed as per-packet emitted
/// audio durations over `drain_rx`. Running on its own thread (independent of
/// the decoder→handler fill chain) is what makes the drain deadlock-free: it
/// keeps relieving the FIFO even while the decoder is blocked sending and the
/// handler is blocked in `write_samples`.
///
/// Only one component may own the FIFO drain at a time, so this thread acts
/// only when pacing is enabled AND the active input mode is `Bridge`; in
/// Live / PipewireBridge the input RT callback owns it and tokens are dropped.
fn spawn_pacer_drain_thread(
    input_control: std::sync::Arc<audio_input::InputControl>,
    drain_rx: mpsc::Receiver<u64>,
    diag: PacerBridgeDiag,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("pacer-bridge-drain".to_string())
        .spawn(move || {
            // Carry the sub-frame remainder across packets so per-packet
            // rounding can't accumulate into audible drift over a long stream.
            let mut frac_frames: f64 = 0.0;
            let mut last_drain_at = None;
            loop {
                if sys::ShutdownHandle::is_requested()
                    || sys::ShutdownHandle::is_restart_from_config_requested()
                {
                    break;
                }
                let emitted_us = match drain_rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(value) => value,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                let Some(pacer) = input_control.output_pacer() else {
                    frac_frames = 0.0;
                    continue;
                };
                if !pacer.enabled.load(std::sync::atomic::Ordering::Relaxed)
                    || input_control.applied_snapshot().active_mode
                        != audio_input::InputMode::Bridge
                {
                    frac_frames = 0.0;
                    continue;
                }
                let now = std::time::Instant::now();
                let drain_dt_us = last_drain_at
                    .map(|prev| now.saturating_duration_since(prev).as_micros() as u64)
                    .unwrap_or(0);
                last_drain_at = Some(now);
                let exact_frames =
                    emitted_us as f64 * pacer.out_sample_rate as f64 / 1_000_000.0 + frac_frames;
                let drain_frames = exact_frames.floor();
                frac_frames = exact_frames - drain_frames;
                let drain_samples = drain_frames as usize * pacer.out_channels as usize;
                diag.emitted_us.store(
                    (emitted_us as f64).to_bits(),
                    std::sync::atomic::Ordering::Relaxed,
                );
                diag.drain_samples.store(
                    (drain_samples as f64).to_bits(),
                    std::sync::atomic::Ordering::Relaxed,
                );
                diag.frac_frames
                    .store(frac_frames.to_bits(), std::sync::atomic::Ordering::Relaxed);
                diag.drain_dt_us.store(
                    (drain_dt_us as f64).to_bits(),
                    std::sync::atomic::Ordering::Relaxed,
                );
                if drain_samples > 0 {
                    pacer.drain(drain_samples);
                }
            }
        })
        .expect("failed to spawn pacer drain thread")
}

fn run_prepared_render(
    mut prepared: PreparedDecodeRun,
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
    handler.drc_mode_cmd_tx = Some(prepared.cmd_tx.clone());

    let live_drc_mode = std::sync::Arc::new(std::sync::RwLock::new(String::new()));
    handler.live_drc_mode = Some(live_drc_mode.clone());

    if let Some(renderer) = &handler.spatial_renderer {
        let ctrl = renderer.renderer_control();
        ctrl.set_bridge_supported_drc_modes(prepared.supported_drc_modes.clone());

        let initial_mode = ctrl.live.read().drc_mode.clone();
        *live_drc_mode.write().unwrap() = initial_mode.clone();
        prepared
            .cmd_tx
            .send(DecoderCommand::SetDrcMode(initial_mode))?;
    }

    if let Some(input_control) = handler.input_control.as_ref() {
        let diag = input_control.diag_registry();
        for handle in [
            DiagAtomicHandle {
                name: "pipe_chunk_bytes",
                label: "Pipe chunk bytes",
                group: "pipe_input",
                unit: "B",
                atomic: Arc::clone(&prepared.pipe_input_diag.chunk_bytes),
            },
            DiagAtomicHandle {
                name: "pipe_chunk_dt_us",
                label: "Pipe chunk dt",
                group: "pipe_input",
                unit: "us",
                atomic: Arc::clone(&prepared.pipe_input_diag.chunk_dt_us),
            },
            DiagAtomicHandle {
                name: "pipe_audio_ms_per_chunk",
                label: "Pipe audio per chunk",
                group: "pipe_input",
                unit: "ms",
                atomic: Arc::clone(&prepared.pipe_input_diag.audio_ms_per_chunk),
            },
            DiagAtomicHandle {
                name: "pipe_gap_over_audio_ms",
                label: "Pipe gap minus audio",
                group: "pipe_input",
                unit: "ms",
                atomic: Arc::clone(&prepared.pipe_input_diag.gap_over_audio_ms),
            },
        ] {
            diag.register_external(
                handle.name,
                handle.label,
                handle.group,
                handle.unit,
                handle.atomic,
            );
        }
        for handle in prepared.pacer_bridge_diag.handles() {
            diag.register_external(
                handle.name,
                handle.label,
                handle.group,
                handle.unit,
                handle.atomic,
            );
        }
    }

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
                    presentation: prepared.presentation.clone(),
                    clock_mode: input_control.requested_snapshot().clock_mode,
                    requested_drc_mode: live_drc_mode.clone(),
                },
            )
        });

    // Drain thread for the post-rendering pacer in pure pipe-bridge mode.
    // Detached: it self-terminates when the decoder drops its sender
    // (Disconnected) or on shutdown/restart, so there is no join-on-error
    // hang to worry about.
    let _pacer_drain_thread = handler
        .input_control
        .as_ref()
        .zip(prepared.drain_rx.take())
        .map(|(input_control, drain_rx)| {
            spawn_pacer_drain_thread(
                input_control.clone(),
                drain_rx,
                prepared.pacer_bridge_diag.clone(),
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

/// Pre-flight OSC port negotiation, run BEFORE any config load of a render
/// iteration: a yielded holder writes its live-state sidecar while still
/// holding the port, and everything downstream (args fold, render-config
/// seeding) must see that sidecar. OSC enablement and the RX port are never
/// live-modified, so peeking them from the base config is exact.
fn negotiate_osc_port_if_enabled(args: &RenderArgs, cli: &Cli, arg_sources: &RenderArgSources<'_>) {
    let config_path = cli
        .config
        .clone()
        .or_else(renderer::config::default_config_path);
    let render_cfg = config_path
        .as_deref()
        .map(renderer::config::Config::load_or_default)
        .unwrap_or_default()
        .render;
    // Mirror merge_render_config's osc / osc_rx_port resolution.
    let osc_on = if arg_sources.is_explicit("osc") || arg_sources.is_explicit("no_osc") {
        args.osc && !args.no_osc
    } else {
        render_cfg
            .as_ref()
            .and_then(|rc| rc.osc)
            .unwrap_or(renderer::config_fields::osc::DEFAULT)
    };
    if !osc_on {
        return;
    }
    let rx_port = if arg_sources.is_explicit("osc_rx_port") {
        args.osc_rx_port
    } else {
        render_cfg
            .as_ref()
            .and_then(|rc| rc.osc_rx_port)
            .unwrap_or(args.osc_rx_port)
    };
    let _ = orender_engine::osc::negotiate_rx_port(rx_port);
}

pub fn cmd_render(args: &RenderArgs, cli: &Cli, arg_sources: &RenderArgSources<'_>) -> Result<()> {
    sys::shutdown::set_yieldable(args.osc_yield);
    let mut restart_bridge_path_override: Option<Option<std::path::PathBuf>> = None;
    loop {
        negotiate_osc_port_if_enabled(args, cli, arg_sources);
        let (config_path, mut effective_args, current_layout_from_config, evaluation_mode_explicit) =
            resolve_effective_decode_args(args, cli, arg_sources);
        if let Some(bridge_path) = restart_bridge_path_override.take() {
            effective_args.bridge_path = bridge_path;
        }
        let args = &effective_args;

        if maybe_save_effective_config(cli, args, &config_path)? {
            return Ok(());
        }

        let bridge_path_after_run = match prepare_render_run(args) {
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
            // reload_config discards live state: forget any consumed handoff
            // overlay so the next iteration re-reads the config from disk.
            renderer::config::clear_live_overlay_cache();
            log::info!("Restarting render pipeline from config");
            continue;
        }

        return Ok(());
    }
}
