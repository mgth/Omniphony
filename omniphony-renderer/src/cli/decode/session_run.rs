use super::bootstrap::{NoBridgeInputs, init_no_bridge_handler, init_render_handler};
use super::config_resolution::{
    apply_explicit_renderer_args, apply_osc_settings, apply_render_cfg_overrides,
    effective_to_config, merge_render_config, renderer_params, resolve_osc_settings,
};
use super::decoder_thread::{
    DecodedAudioData, DecodedSource, DecoderMessage, DecoderThreadConfig, PipeInputDiag,
    spawn_decoder_thread,
};
use super::handler::DecodeHandler;
use super::idle_feed::{IdleFeedInputs, IdleFeeder};
use super::live_input::{LiveBridgeRuntimeConfig, spawn_live_input_manager};
use super::output::OutputClosed;
use super::state::FrameHandlerContext;
use crate::cli::command::{Cli, OutputBackend, RenderArgSources, RenderArgs};
use anyhow::{Context, Result};
use diag::DiagAtomicHandle;
use orender_engine::bridge_loader::{LoadedBridge, resolve_bridge_path};
use orender_engine::renderer_build::SpatialRendererParams;
use std::sync::mpsc;
use std::sync::{Arc, RwLock, atomic::AtomicU64};
use std::time::Duration;

const DEFAULT_DECODE_QUEUE_LATENCY_MS: u32 = 220;
const DECODE_QUEUE_MESSAGES_PER_MS: usize = 2;
const MIN_DECODE_QUEUE_CAPACITY: usize = 512;
const MAX_DECODE_QUEUE_CAPACITY: usize = 8192;

struct PreparedDecodeRun {
    /// A frame sender for the live-input manager, taken (or dropped) as soon
    /// as the producers are spawned. The render loop ends on `Disconnected`,
    /// i.e. once every producer is gone: a sender kept here beside the
    /// decoder thread's would keep the channel open past the end of a
    /// non-continuous input, and the run would never finish.
    live_input_tx: Option<mpsc::SyncSender<Result<DecoderMessage>>>,
    rx: mpsc::Receiver<Result<DecoderMessage>>,
    /// The DRC mode both bridge decoders follow (pipe and PipeWire sink),
    /// seeded with the configured one before the decoder thread starts.
    drc_mode: Arc<RwLock<String>>,
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

/// The format bridge could not be resolved or loaded. Attached as context to
/// those errors (and only those), so the caller tells them apart by type — it
/// used to match the loader's message texts, and a reworded message silently
/// turned "idle until a working bridge is set" into a hard exit.
#[derive(Debug)]
struct BridgeUnavailable;

impl std::fmt::Display for BridgeUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("format bridge unavailable")
    }
}

fn is_bridge_unavailable_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<BridgeUnavailable>().is_some()
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

    // `run.config` reads a file that fails to parse as defaults; writing over
    // it would lose the user's layout and profiles.
    renderer::config::Config::load_for_update(&path)?;
    // Nor over a fixed file when this run restored the live state a previous
    // instance handed over while it ran on that fallback.
    if run.config.live_from_parse_error {
        anyhow::bail!(
            "the live state restored for {} is the built-in defaults a previous instance fell \
             back to; reload the config before saving it",
            path.display()
        );
    }
    let config = effective_to_config(&run.args, arg_sources, cli, Some(&run.config))?;
    config.save(&path)?;
    log::info!("Config written to: {}", path.display());
    Ok(true)
}

/// The DRC mode a render starts with: the live params' seed
/// (`seed_runtime_state_from_render_config`), known before the renderer is
/// built so the decoders can start in it.
fn configured_drc_mode(render_cfg: &renderer::config::RenderConfig) -> &str {
    render_cfg.drc_mode.as_deref().unwrap_or("Off")
}

fn prepare_render_run(args: &RenderArgs, drc_mode: &str) -> Result<PreparedDecodeRun> {
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

    let bridge_path =
        resolve_bridge_path(args.bridge_path.as_deref()).context(BridgeUnavailable)?;
    log::info!("Loading format bridge: {}", bridge_path.display());
    // Only the load is "bridge unavailable"; a bridge that loads but
    // refuses the presentation is a configuration error, not a reason to idle.
    let LoadedBridge { lib, mut bridge } =
        LoadedBridge::load_with_params(&bridge_path).context(BridgeUnavailable)?;
    orender_engine::bridge_loader::configure_presentation(&mut bridge, &args.presentation)?;
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
    // Given to the decoder thread at spawn: the handler only exists once the
    // renderer is built, and a mode it sent then landed after however many
    // packets the thread had decoded meanwhile — a different render each run.
    let drc_mode = Arc::new(RwLock::new(drc_mode.to_owned()));
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
        requested_drc_mode: Arc::clone(&drc_mode),
        drain_tx: Some(drain_tx.clone()),
        pipe_input_diag: Some(pipe_input_diag.clone()),
        bridge,
        shutdown_signal,
    });

    Ok(PreparedDecodeRun {
        live_input_tx: Some(tx),
        rx,
        drc_mode,
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
    // The no-bridge runtime liborender also brings up, with this host's audio
    // controls attached: the bridge error in the live state (Studio's banner),
    // shortened to what a UI can show, instead of a generic "path missing" for
    // every failure (an ABI mismatch included).
    init_no_bridge_handler(
        &mut handler,
        NoBridgeInputs {
            args,
            render_cfg: &run.render_cfg,
            params: &run.renderer_params,
            input_path: idle_input_path(args),
            config_path: &run.config_path,
            bridge_error: format!("{bridge_error:#}"),
        },
    )?;

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
    // A property of the bridge, which outlives the stream too.
    let coordinate_format = handler.spatial.stream.coordinate_format;

    *handler = DecodeHandler::default();

    handler.spatial.stream.coordinate_format = coordinate_format;

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
    let declaration =
        super::state::resolve_declaration(handler.spatial_renderer.as_ref(), decoded.declaration);
    handler
        .spatial
        .take_declaration(decoded.source, declaration);
    if frame.is_new_segment {
        // Use the live-active backend (not the launch one) so a segment
        // restart preserves a Studio-requested switch (e.g. to `file`).
        handler.handle_stream_restart(handler.runtime.active_output_backend)?;
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
            Ok(DecoderMessage::BridgeReset(source)) => {
                if handler.should_accept_source(source) {
                    handler.handle_bridge_reset();
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
    handler.spatial.stream.coordinate_format = prepared.coordinate_format;
    // Live DRC changes reach both decoders through this value; the one the
    // live params were seeded with is already in it.
    handler.drc.shared = Some(Arc::clone(&prepared.drc_mode));

    if let Some(renderer) = &handler.spatial_renderer {
        let ctrl = renderer.renderer_control();
        ctrl.set_bridge_supported_drc_modes(prepared.supported_drc_modes.clone());
        orender_engine::bridge_loader::declare_source_families(&prepared.bridge_lib, &ctrl);
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

    // An offline render — a file in, a file out, no continuous input — has no
    // use for the live input: starting it would publish the PipeWire bridge
    // input sink (a config with `input_mode: pipewire`), a second "omniphony"
    // node beside the running renderer's, and feed its capture into the render.
    let offline =
        effective_args.output_backend == Some(OutputBackend::File) && !effective_args.continuous;
    // Nor any use for the binaural stages' background builds: an HRIR grid or
    // BRIR set would land at whichever block the worker finished by, and two
    // renders of the same file would differ there. Nothing waits on the
    // output, so the build can hold the frame that asks for it.
    if offline && let Some(renderer) = handler.spatial_renderer.as_mut() {
        renderer.set_synchronous_stage_builds(true);
    }
    // The band engines (a gain table per crossover band) are built here,
    // before the first frame, rather than by it.
    if let Some(renderer) = handler.spatial_renderer.as_mut() {
        renderer.prepare_speaker_stage()?;
    }
    // Taken whether the manager starts or not: when it does not, the decoder
    // thread is left the only producer, and the loop below ends once it has
    // delivered the last frame of the input.
    let live_input_tx = prepared.live_input_tx.take();
    let live_input_manager = handler
        .input_control
        .as_ref()
        .zip(handler.audio_control.as_ref())
        .filter(|_| !offline)
        .zip(live_input_tx)
        .map(|((input_control, audio_control), tx)| {
            spawn_live_input_manager(
                tx,
                input_control.clone(),
                audio_control.clone(),
                LiveBridgeRuntimeConfig {
                    lib: prepared.bridge_lib.clone(),
                    presentation: prepared.presentation.clone(),
                    clock_mode: input_control.requested_snapshot().clock_mode,
                    requested_drc_mode: Arc::clone(&prepared.drc_mode),
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
    sys::shutdown::set_restartable(true);
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

        let drc_mode = configured_drc_mode(&run.render_cfg);
        let bridge_path_after_run = match prepare_render_run(&run.args, drc_mode) {
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
            // overlay so the next iteration re-reads the config from disk. (A
            // restart that keeps the live state wrote a fresh sidecar on the
            // way down, which the next iteration reads before the cache.)
            renderer::config::clear_live_overlay_cache();
            log::info!("Restarting render pipeline from config");
            continue;
        }

        return Ok(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::command::{Commands, ParsedCli};

    fn render_args(extra: &[&str]) -> RenderArgs {
        let argv = ["orender", "render", "--output-backend", "file"]
            .iter()
            .chain(extra)
            .copied();
        let parsed = ParsedCli::parse_from(argv).expect("parse render args");
        match parsed.cli.command {
            Commands::Render(args) => args,
            _ => unreachable!("render subcommand"),
        }
    }

    /// A bridge that cannot be found or loaded is recognised by type — the
    /// idle runtime depends on it — whatever the loader's message says; any
    /// other startup error is not mistaken for it.
    #[test]
    fn only_bridge_failures_count_as_bridge_unavailable() {
        let missing = render_args(&["--bridge-path", "/nonexistent/libnone_bridge.so", "in.thd"]);
        let err = prepare_render_run(&missing, "Off")
            .err()
            .expect("missing bridge");
        assert!(is_bridge_unavailable_error(&err), "{err:#}");

        let no_input = render_args(&["--bridge-path", "/nonexistent/libnone_bridge.so"]);
        let err = prepare_render_run(&no_input, "Off")
            .err()
            .expect("missing input");
        assert!(!is_bridge_unavailable_error(&err), "{err:#}");
    }

    /// The live state a Studio registering with `control` would get, by
    /// address; what the engine's OSC export sends on top of the core bundle
    /// (the catalogues) included, the host handler's own messages not.
    fn published_state(
        control: &std::sync::Arc<renderer::live_params::RendererControl>,
        has_host_audio: bool,
    ) -> std::collections::BTreeMap<String, Vec<rosc::OscType>> {
        let mut state: std::collections::BTreeMap<_, _> =
            runtime_control::snapshot::build_live_state_bundle(
                control,
                has_host_audio,
                has_host_audio,
            )
            .into_iter()
            .filter_map(|packet| match packet {
                rosc::OscPacket::Message(msg) => Some((msg.addr, msg.args)),
                rosc::OscPacket::Bundle(_) => None,
            })
            .collect();
        state.insert(
            "object_generators".into(),
            vec![rosc::OscType::String(control.object_generators_json())],
        );
        state.insert(
            "phantom".into(),
            vec![rosc::OscType::String(control.phantom_json())],
        );
        state
    }

    /// Both hosts come up without a bridge through the same runtime
    /// (`orender_engine::degraded::NoBridgeRuntime`) and publish the same
    /// state — layout, bridge error and path, config path/status/profiles,
    /// seeded runtime state, catalogues — save for what is each host's own:
    /// the embedded host's C-ABI, the CLI's input pipe, audio/input domains
    /// and faster monitoring cadence. The embedded setup is the one liborender
    /// builds (`NoBridgeSetup::embedded`); this host's is its idle runtime's.
    #[test]
    fn both_hosts_publish_the_same_no_bridge_state() {
        const HOST_SPECIFIC: &[&str] = &[
            runtime_control::osc_contract::STATE_RENDER_ABI,
            runtime_control::osc_contract::STATE_INPUT_PIPE,
            runtime_control::osc_contract::STATE_MONITORING,
            runtime_control::osc_contract::STATE_CAPABILITIES,
        ];
        let dir =
            std::env::temp_dir().join(format!("orender-no-bridge-parity-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let config = dir.join("config.yaml");
        // A bridge path of its own (the flag's is unsaved state in both
        // hosts), a seeded field, and a small grid so the renderers build fast.
        std::fs::write(
            &config,
            "render:
  bridge_path: /nonexistent/libconfig_bridge.so
  ramp_mode: sample
  evaluation_cartesian_x_size: 9
  evaluation_cartesian_y_size: 9
  evaluation_cartesian_z_size: 5
",
        )
        .expect("write config");
        let rx_port = std::net::UdpSocket::bind("127.0.0.1:0")
            .and_then(|socket| socket.local_addr())
            .expect("free port")
            .port()
            .to_string();
        let bridge = "/nonexistent/libnone_bridge.so";
        let error = format!("bridge path '{bridge}' does not exist");

        let parsed = ParsedCli::parse_from([
            "orender",
            "--config",
            config.to_str().expect("utf-8 path"),
            "render",
            "--output-backend",
            "file",
            "--bridge-path",
            bridge,
            "--osc",
            "--osc-rx-port",
            &rx_port,
            "in.thd",
        ])
        .expect("parse render args");
        let Commands::Render(args) = &parsed.cli.command else {
            unreachable!("render subcommand")
        };
        let run = resolve_effective_decode_args(args, &parsed.cli, &parsed.render_sources());
        let mut handler = DecodeHandler::default();
        init_no_bridge_handler(
            &mut handler,
            NoBridgeInputs {
                args: &run.args,
                render_cfg: &run.render_cfg,
                params: &run.renderer_params,
                input_path: idle_input_path(&run.args),
                config_path: &run.config_path,
                bridge_error: error.clone(),
            },
        )
        .expect("CLI no-bridge runtime");
        assert!(
            handler
                .telemetry
                .osc_sender
                .as_ref()
                .is_some_and(|osc| osc.is_listening()),
            "the CLI's no-bridge runtime serves OSC"
        );
        let cli_control = handler
            .spatial_renderer
            .as_ref()
            .expect("no-bridge renderer")
            .renderer_control();

        let embedded =
            orender_engine::NoBridgeRuntime::build(orender_engine::NoBridgeSetup::embedded(
                Some(config.clone()),
                renderer::config::Config::load_or_default_with_live(&config)
                    .0
                    .render,
                None,
                Some(bridge.into()),
                48_000,
                error.clone(),
                Some((0, 1)),
            ))
            .expect("embedded no-bridge runtime");
        let embedded_control = embedded.control();

        assert_eq!(cli_control.bridge_error(), Some(error));
        assert_eq!(cli_control.bridge_path(), Some(bridge.into()));
        let mut cli_state = published_state(&cli_control, true);
        let mut embedded_state = published_state(&embedded_control, false);
        // The options schema differs by exactly the embedded engine's own
        // options (`decode_thread`), which the standalone host leaves out.
        let schema = |state: &std::collections::BTreeMap<String, Vec<rosc::OscType>>| match state
            .get(runtime_control::osc_contract::STATE_OPTIONS_SCHEMA)
            .and_then(|args| args.first())
        {
            Some(rosc::OscType::String(json)) => json.clone(),
            _ => panic!("no options schema published"),
        };
        let (cli_schema, embedded_schema) = (schema(&cli_state), schema(&embedded_state));
        let entries = |json: &str| json.matches("\"key\":").count();
        assert!(embedded_schema.contains("\"key\":\"decode_thread\""));
        assert!(!cli_schema.contains("\"key\":\"decode_thread\""));
        assert_eq!(entries(&cli_schema) + 1, entries(&embedded_schema));
        cli_state.remove(runtime_control::osc_contract::STATE_OPTIONS_SCHEMA);
        embedded_state.remove(runtime_control::osc_contract::STATE_OPTIONS_SCHEMA);
        for addr in HOST_SPECIFIC {
            assert!(cli_state.remove(*addr).is_some(), "{addr} not published");
            embedded_state.remove(*addr);
        }
        let differing: std::collections::BTreeSet<_> = cli_state
            .keys()
            .chain(embedded_state.keys())
            .filter(|addr| cli_state.get(*addr) != embedded_state.get(*addr))
            .collect();
        assert!(
            differing.is_empty(),
            "the hosts' no-bridge states differ at {differing:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
