use super::bootstrap::init_render_handler;
use super::config_resolution::{
    apply_explicit_renderer_args, apply_osc_settings, apply_render_cfg_overrides,
    effective_to_config, merge_render_config, renderer_params, resolve_osc_settings,
};
use super::decoder_thread::{
    DecodedAudioData, DecodedSource, DecoderCommand, DecoderMessage, DecoderThreadConfig,
    PipeInputDiag, spawn_decoder_thread,
};
use super::handler::DecodeHandler;
use super::idle_feed::{IdleFeedInputs, IdleFeeder};
use super::live_input::{LiveBridgeRuntimeConfig, spawn_live_input_manager};
use super::output::OutputClosed;
use super::state::FrameHandlerContext;
use crate::cli::command::{Cli, OutputBackend, RenderArgSources, RenderArgs};
use anyhow::Result;
use orender_engine::bridge_loader::{LoadedBridge, resolve_bridge_path};
use orender_engine::renderer_build::SpatialRendererParams;
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
    /// Sender side of the pacer drain clock, kept so the speaker-test idle
    /// feed can post tokens for its fabricated frames — in pure pipe-bridge
    /// mode nothing else drains the output FIFO, so silence fed without a
    /// matching token would fill the pacer and never reach the device.
    drain_tx: mpsc::Sender<u64>,
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

/// Everything one render iteration resolves from the command line and the
/// config file before it starts.
struct ResolvedRun {
    config_path: Option<std::path::PathBuf>,
    /// The args with the config folded in (flag > config > default).
    args: RenderArgs,
    /// The config file as this run loaded it (live-handoff sidecar included):
    /// the base `--save-config` writes the flags over.
    config: renderer::config::Config,
    /// The render section this run uses: the file with the CLI flags applied.
    render_cfg: renderer::config::RenderConfig,
    /// Renderer construction params, resolved from `render_cfg` by the same
    /// call as the embedded engine.
    renderer_params: SpatialRendererParams,
    current_layout: Option<renderer::speaker_layout::SpeakerLayout>,
}

fn resolve_effective_decode_args(
    args: &RenderArgs,
    cli: &Cli,
    arg_sources: &RenderArgSources<'_>,
) -> ResolvedRun {
    let config_path = cli
        .config
        .clone()
        .or_else(renderer::config::default_config_path);
    // Sidecar-aware load: when a yielded predecessor handed over unsaved live
    // state, the args fold and the render config below must both see it, or
    // the renderer would start on the base file and silently undo the handoff.
    let config = config_path
        .as_deref()
        .map(|p| renderer::config::Config::load_or_default_with_live(p).0)
        .unwrap_or_default();

    let mut effective = args.clone();
    if let Some(rc) = &config.render {
        merge_render_config(rc, &mut effective, arg_sources);
    }
    let osc = resolve_osc_settings(config.render.as_ref(), &effective, arg_sources);
    apply_osc_settings(&mut effective, osc);

    let mut render_cfg = config.render.clone().unwrap_or_default();
    apply_render_cfg_overrides(&mut render_cfg, &effective);
    apply_explicit_renderer_args(&mut render_cfg, &effective, arg_sources);
    let renderer_params = renderer_params(&render_cfg, &effective);

    let current_layout = config
        .render
        .as_ref()
        .and_then(|rc| rc.current_layout.clone());
    ResolvedRun {
        config_path,
        args: effective,
        config,
        render_cfg,
        renderer_params,
        current_layout,
    }
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
    run: &ResolvedRun,
    arg_sources: &RenderArgSources<'_>,
) -> Result<bool> {
    if !cli.save_config {
        return Ok(false);
    }

    let path = run.config_path.clone().ok_or_else(|| {
        anyhow::anyhow!("Cannot determine config path; use --config to specify one")
    })?;

    let config = effective_to_config(&run.args, arg_sources, cli, Some(&run.config))?;
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
    let LoadedBridge { lib, bridge } =
        LoadedBridge::load_for_presentation(&bridge_path, &args.presentation)?;
    let is_spatial_presentation = bridge.has_objects();
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
        drain_tx: Some(drain_tx.clone()),
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
        drain_tx,
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
    run: &ResolvedRun,
    bridge_error: &anyhow::Error,
) -> Result<Option<std::path::PathBuf>> {
    let args = &run.args;
    let shutdown = sys::shutdown::ShutdownHandle::install()?;
    let mut handler = DecodeHandler::default();
    init_render_handler(
        &mut handler,
        args,
        &run.render_cfg,
        &run.renderer_params,
        idle_input_path(args),
        &run.config_path,
        run.current_layout.clone(),
        IDLE_BRIDGE_VBAP_DEFAULTS,
        IDLE_BRIDGE_PREFERRED_EVALUATION_MODE,
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

/// Report what auto-gain did, when it is on — live, as Studio may have turned
/// it on or off since the start.
fn log_auto_gain_summary(handler: &DecodeHandler) {
    if let Some(ref renderer) = handler.spatial_renderer {
        if !renderer.renderer_control().live.read().auto_gain {
            return;
        }
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

fn handle_stream_end(handler: &mut DecodeHandler) -> Result<()> {
    log::info!("Stream ended, finalizing current output and resetting handler...");
    handler.finalize()?;

    log_auto_gain_summary(handler);

    let spatial_renderer = handler.spatial_renderer.take();
    let audio_control = handler.audio_control.take();
    let input_control = handler.input_control.take();
    // The decoders outlive the stream: without their DRC links, a mode picked
    // after the first stream end never reached them.
    let drc = std::mem::take(&mut handler.drc);
    let osc_sender = handler.telemetry.osc_sender.take();
    let audio_meter = handler.telemetry.audio_meter.take();
    let runtime = handler.runtime.clone();

    *handler = DecodeHandler::default();

    handler.spatial_renderer = spatial_renderer;
    handler.audio_control = audio_control;
    handler.input_control = input_control;
    handler.drc = drc;
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
    if let Some(declaration) = decoded.declaration {
        handler.spatial.source_family =
            renderer::placement::SourceFamily::from_declared(&declaration.family);
        handler.spatial.declared_poses = declaration.poses;
    }
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
    let _ = handler
        .output
        .invalidate_writer(handler.input_control.as_deref());
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

/// While the speaker-test idle feed is armed and no real input flows,
/// fabricate a chunk of decoded silence and push it through the exact same
/// path a real frame takes (`handle_audio_message`), so the writer, the
/// adaptive latency controller and the metering all see an ordinary stream.
/// Rendering silence still runs `inject_speaker_test`, which is what makes a
/// test audible — and immediate — with no programme playing.
fn pump_idle_feed(
    feeder: &mut IdleFeeder,
    handler: &mut DecodeHandler,
    ctx: &DecodeRunContext<'_>,
    drain_tx: &mpsc::Sender<u64>,
) -> Result<()> {
    // No spatial renderer means no speaker test to keep warm.
    let Some(renderer) = handler.spatial_renderer.as_ref() else {
        return Ok(());
    };
    let (arm_gen, test_active) = {
        let control = renderer.renderer_control();
        let live = control.live.read();
        // Either test keeps the feed alive: both are inaudible from idle if the
        // output chain is cold, and the feed is not specific to either.
        (
            live.speaker_test_idle_feed_gen,
            live.speaker_test.is_some() || live.object_test.is_some(),
        )
    };
    let inputs = IdleFeedInputs {
        arm_gen,
        test_active,
        sample_rate: handler.session.final_sample_rate,
    };
    let Some(chunk) = feeder.poll(std::time::Instant::now(), &inputs) else {
        return Ok(());
    };

    let channel_count = 2u32;
    let frame = bridge_api::RDecodedFrame {
        sampling_frequency: chunk.sample_rate,
        sample_count: chunk.sample_count,
        channel_count,
        pcm: vec![0i32; (chunk.sample_count * channel_count) as usize].into(),
        channel_labels: vec![bridge_api::RChannelLabel::L, bridge_api::RChannelLabel::R].into(),
        metadata: abi_stable::std_types::RVec::new(),
        drc_gain: 1.0,
        drc_ramp_duration: 0,
        dialogue_level: abi_stable::std_types::ROption::RNone,
        is_new_segment: false,
    };
    // Post the pacer drain token first, exactly like the decoder thread does
    // for a real packet. Ignored (by the drain thread) outside pure
    // pipe-bridge mode; a closed channel just means the run is winding down.
    let emitted_us = chunk.sample_count as u64 * 1_000_000 / chunk.sample_rate.max(1) as u64;
    let _ = drain_tx.send(emitted_us);
    handle_audio_message(
        handler,
        DecodedAudioData {
            source: DecodedSource::Bridge,
            frame,
            declaration: None,
            decode_time_ms: 0.0,
            sent_at: std::time::Instant::now(),
        },
        ctx,
    )
}

fn process_decoder_messages(
    rx: &mpsc::Receiver<Result<DecoderMessage>>,
    handler: &mut DecodeHandler,
    ctx: &DecodeRunContext<'_>,
    drain_tx: &mpsc::Sender<u64>,
) -> Result<()> {
    let mut idle_feeder = IdleFeeder::default();
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
                if let Err(err) = pump_idle_feed(&mut idle_feeder, handler, ctx, drain_tx) {
                    if end_run_if_output_closed(&err) {
                        break;
                    }
                    return Err(err);
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        match result {
            Ok(DecoderMessage::AudioData(frame)) => {
                // Real input showing up (whatever its cadence) silences the
                // idle feed; only frames the handler will actually accept
                // count, so a stray producer can't starve the feed.
                if handler.should_accept_source(frame.source) {
                    idle_feeder.note_real_frame(std::time::Instant::now());
                }
                if let Err(err) = handle_audio_message(handler, frame, ctx) {
                    if end_run_if_output_closed(&err) {
                        break;
                    }
                    return Err(err);
                }
            }
            Ok(DecoderMessage::FlushRequest(source)) => {
                if handler.should_accept_source(source) {
                    handler.handle_decoder_flush_request();
                } else {
                    handler.poll_runtime_state()?;
                }
            }
            Ok(DecoderMessage::StreamEnd(source)) => {
                if handler.should_accept_source(source) {
                    handle_stream_end(handler)?;
                } else {
                    handler.poll_runtime_state()?;
                }
            }
            Err(err) => return Err(err),
        }
    }

    Ok(())
}

/// A downstream consumer closing the `file` sink is the end of the output,
/// not a fault: log it once, ask for a clean shutdown (the decoder thread is
/// still reading its input and needs waking) and let the caller leave the
/// loop. Any other error is left to propagate.
fn end_run_if_output_closed(err: &anyhow::Error) -> bool {
    if !err.is::<OutputClosed>() {
        return false;
    }
    log::info!("Output consumer closed the pipe; stopping the render loop");
    sys::shutdown::request_shutdown();
    true
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
    is_shutdown: bool,
) -> Result<()> {
    // Close the frame channel before joining. It is bounded, and the decoder
    // may be blocked in `send` on a full one (file-sink backpressure at the
    // moment the run stopped); with nothing receiving any more that send would
    // never return, and neither would the join. A closed channel makes it
    // fail, and the decoder stops on that.
    let PreparedDecodeRun {
        rx, decode_thread, ..
    } = prepared;
    drop(rx);
    match decode_thread.join() {
        Ok(Ok(())) => {
            if is_shutdown {
                log::info!("Decoder stopped cleanly");
            } else {
                log::info!("Decoding completed successfully");
                log_auto_gain_summary(handler);
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
    process_decoder_messages(&prepared.rx, handler, &run_ctx, &prepared.drain_tx)
}

fn finalize_render_run(prepared: PreparedDecodeRun, handler: &mut DecodeHandler) -> Result<()> {
    let is_shutdown = begin_shutdown_if_requested();
    finalize_output_for_exit(handler, is_shutdown)?;
    complete_render_run(prepared, handler, is_shutdown)
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
/// `Pipewire` the input RT callback owns it and tokens are dropped.
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
                if !pacer.enabled
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
    run: &ResolvedRun,
) -> Result<Option<std::path::PathBuf>> {
    let effective_args = &run.args;
    if run.renderer_params.render_evaluation_mode.is_none() {
        log::info!(
            "Using bridge-preferred evaluation mode: {:?}",
            prepared.preferred_evaluation_mode
        );
    }

    let mut handler = DecodeHandler::default();
    init_render_handler(
        &mut handler,
        effective_args,
        &run.render_cfg,
        &run.renderer_params,
        &prepared.input_path,
        &run.config_path,
        run.current_layout.clone(),
        prepared.vbap_cartesian_defaults,
        prepared.preferred_evaluation_mode,
    )?;
    handler.spatial.coordinate_format = prepared.coordinate_format;
    handler.drc.cmd_tx = Some(prepared.cmd_tx.clone());

    let live_drc_mode = std::sync::Arc::new(std::sync::RwLock::new(String::new()));
    handler.drc.shared = Some(live_drc_mode.clone());

    if let Some(renderer) = &handler.spatial_renderer {
        let ctrl = renderer.renderer_control();
        ctrl.set_bridge_supported_drc_modes(prepared.supported_drc_modes.clone());

        let initial_mode = ctrl.live.read().drc_mode.clone();
        *live_drc_mode.write().unwrap() = initial_mode.clone();
        // Best-effort initial DRC sync. The decoder thread already defaults to
        // this same mode, and on a fast/short file decode it can finish and drop
        // the command receiver before we reach this point — so a closed channel
        // here is benign and must not abort the whole render.
        let _ = prepared
            .cmd_tx
            .send(DecoderCommand::SetDrcMode(initial_mode));
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

    let run_result = run_render_message_phase(&prepared, &mut handler, effective_args);
    if let Some(manager) = live_input_manager {
        manager.stop();
    }
    run_result?;
    let current_bridge_path = handler
        .spatial_renderer
        .as_ref()
        .map(|renderer| renderer.renderer_control().bridge_path())
        .unwrap_or_else(|| effective_args.bridge_path.clone());
    finalize_render_run(prepared, &mut handler)?;
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
    // The same resolution the run itself folds into its args.
    let osc = resolve_osc_settings(render_cfg.as_ref(), args, arg_sources);
    if osc.enabled {
        let _ = orender_engine::osc::negotiate_rx_port(osc.port_in);
    }
}

pub fn cmd_render(args: &RenderArgs, cli: &Cli, arg_sources: &RenderArgSources<'_>) -> Result<()> {
    sys::shutdown::set_yieldable(args.osc_yield);
    let mut restart_bridge_path_override: Option<Option<std::path::PathBuf>> = None;
    loop {
        negotiate_osc_port_if_enabled(args, cli, arg_sources);
        let mut run = resolve_effective_decode_args(args, cli, arg_sources);
        if let Some(bridge_path) = restart_bridge_path_override.take() {
            run.args.bridge_path = bridge_path;
        }

        if maybe_save_effective_config(cli, &run, arg_sources)? {
            return Ok(());
        }

        let bridge_path_after_run = match prepare_render_run(&run.args) {
            Ok(prepared) => run_prepared_render(prepared, &run)?,
            Err(err) if run.args.osc && is_bridge_unavailable_error(&err) => {
                run_idle_runtime(&run, &err)?
            }
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
