//! The sync host: `orender sync-play`, the new playback path of the
//! resampling rework (`docs/resampling-rework-plan.md` §10).
//!
//! ```text
//! pipe ─► reader thread ─► engine thread: IEC 61937 deframe ─► Engine (decode + render)
//!                                         ─► ring ─► OutputCore ─► PipeWire
//!                         └─ arrival time + frames ─► SourceTap ─┘
//! ```
//!
//! Slice 4a: the `follow` source clock for a pipe written in real time (mpv
//! `--ao=pcm`; with `--ao-pcm-timed` it writes at the pace of its own clock,
//! in small regular periods, plan §15). The source clock is the arrival of
//! the bytes; the output resamples to hold a fixed end-to-end latency against
//! it. Each stream (pipe open → EOF) is an epoch with its own ring and
//! output, so nothing carries over between files.
//!
//! The frames counted in (`N_in`) are counted in transport time: the carrier
//! bytes that have arrived, from the first burst, at the carrier's rate (the
//! `own` sink knows it from its format, a pipe from the IEC 61937 data type).
//! A pipe whose carrier is unknown (DTS-HD, raw PCM) counts the frames pushed
//! into the ring plus the codec's constant decoder hold instead, at the
//! arrival of the bytes that completed them: exact in rate, but the decoder's
//! burst granularity shows as up to a chunk of arrival lateness.
//!
//! With the OSC server on (`--osc`, or the config's `render.osc`), the host
//! publishes the latency it holds, the latency it measures and the servo's
//! quantities ([`telemetry`], plan §16).
//!
//! Hidden while experimental. Linux only for now (PipeWire output).

mod controls;
mod own_sink;
mod pcm;
mod reader;
mod telemetry;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use audio_output::sync_output::{
    OutputCore, OutputCoreConfig, PipewireSyncOutput, PipewireSyncOutputConfig, SourceTap,
    SyncTelemetry,
};
use audio_rt::{Producer, frame_ring};
use bridge_api::RInputTransport;
use clap::Args;
use orender_engine::engine::{Engine, RenderedAudio};
use orender_engine::{OscOverrides, OscSettings};
use spdif::SpdifParser;

use crate::cli::decode::idle_feed::{IdleFeedInputs, IdleFeeder};
use audio_output::sync_output::reference_now_s;
use controls::{Controls, OutputSettings};
use reader::{CarrierFormat, ReaderMsg, Transport, spawn_reader};
use telemetry::HostTelemetry;

/// `orender sync-play` arguments.
#[derive(Debug, Clone, Args)]
pub struct SyncPlayArgs {
    /// Input pipe (IEC 61937 or raw bitstream), for `--source pipe`.
    #[arg(value_name = "INPUT")]
    pub input: Option<PathBuf>,

    /// Where the stream comes from: `pipe` (a writer on its own clock,
    /// `follow`) or `own` (orender's PipeWire sink on orender's clock).
    #[arg(long, value_enum, default_value_t = SourceKind::Pipe)]
    pub source: SourceKind,

    /// Node name of the `own` sink.
    #[arg(long = "sink-name", default_value = "omniphony-sync")]
    pub sink_name: String,

    /// `priority.session` of the `own` sink (0 keeps it from ever being
    /// picked as the default sink, e.g. for tests).
    #[arg(long = "sink-session-priority")]
    pub sink_session_priority: Option<u32>,

    /// End-to-end latency to hold (ms). Defaults to the config's
    /// `render.latency_target`, else 150.
    #[arg(long = "target-latency-ms")]
    pub target_latency_ms: Option<f64>,

    /// Phase-loop bandwidth (Hz). Defaults to the `follow` value.
    #[arg(long = "loop-bandwidth-hz")]
    pub loop_bandwidth_hz: Option<f64>,

    /// PipeWire sink to play into (pinned); defaults to the config's
    /// `render.output_device`, else the session's choice.
    #[arg(long = "output-target")]
    pub output_target: Option<String>,

    /// PipeWire quantum requested (frames).
    #[arg(long, default_value_t = 1024)]
    pub quantum: u32,

    /// Stop after this many seconds (tests).
    #[arg(long)]
    pub seconds: Option<f64>,

    /// Print the servo telemetry every N seconds (0 = never).
    #[arg(long = "report-every", default_value_t = 5.0)]
    pub report_every: f64,

    /// Keep bytes already queued in the pipe when it opens.
    #[arg(long = "no-drain-pipe")]
    pub no_drain_pipe: bool,

    /// Run the OSC server (Studio control and monitoring), as `render --osc`.
    /// Without this or `--no-osc`, the config's `render.osc` decides.
    #[arg(long, conflicts_with = "no_osc")]
    pub osc: bool,

    /// Keep the OSC server off whatever the config says.
    #[arg(long, conflicts_with = "osc")]
    pub no_osc: bool,

    /// OSC target host (default: the config's, else localhost).
    #[arg(long, value_name = "HOST")]
    pub osc_host: Option<String>,

    /// OSC target port (default: the config's).
    #[arg(long, value_name = "PORT")]
    pub osc_port: Option<u16>,

    /// OSC registration listener port (default: the config's).
    #[arg(long, value_name = "PORT")]
    pub osc_rx_port: Option<u16>,
}

/// Latency held when neither the command line nor the config names one (ms).
const DEFAULT_TARGET_LATENCY_MS: f64 = 150.0;

/// The realign threshold (ms): the config's `render.realign_threshold_ms`,
/// else the default.
pub fn resolve_realign_threshold_ms(config: Option<f32>) -> f64 {
    f64::from(config.unwrap_or(audio_output::control::DEFAULT_REALIGN_THRESHOLD_MS))
}

/// The end-to-end latency to hold (ms): the flag, else the config's
/// `render.latency_target`, else [`DEFAULT_TARGET_LATENCY_MS`].
pub fn resolve_target_latency_ms(flag: Option<f64>, config: Option<u32>) -> f64 {
    flag.or(config.map(f64::from))
        .unwrap_or(DEFAULT_TARGET_LATENCY_MS)
}

/// Where the stream comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum SourceKind {
    Pipe,
    Own,
}

/// Rate the engine renders at. Slice 4a assumes a 48 kHz family source.
const RENDER_RATE: u32 = 48_000;
/// Ring capacity: comfortably above any latency target.
const RING_SECONDS: f64 = 4.0;

/// Constant decoder hold (frames) by IEC 61937 data type (spike S4 §2).
/// E-AC-3 with objects holds nothing but cannot be told apart at this level;
/// slice 4d counts in transport time and makes this table unnecessary.
fn decoder_hold_frames(data_type: u8) -> u64 {
    match data_type {
        1 | 21 => 1536, // AC-3, E-AC-3
        11..=13 => 512, // DTS core
        _ => 0,         // TrueHD/MAT (22), DTS-HD (17), raw
    }
}

/// IEC 61937 carrier (frame rate, channels) a player writing a pipe uses for
/// a data type, as mpv's spdif wrapper sets it up. DTS-HD (17) is left out:
/// HRA rides 2 channels and MA 8, which the burst alone does not tell.
fn pipe_carrier(data_type: u8) -> Option<(u32, u32)> {
    match data_type {
        1 | 11..=13 => Some((48_000, 2)), // AC-3, DTS core
        21 => Some((192_000, 2)),         // E-AC-3
        22 => Some((192_000, 8)),         // TrueHD/MAT
        _ => None,
    }
}

/// What the clients have been told of an epoch's output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputReport {
    /// Opened, not seen playing yet.
    Pending,
    Playing,
    /// Not playing: said so.
    Missing,
}

/// How long an output may take to start playing before the clients are told
/// it is not (a device that is not there leaves the stream waiting for it).
const OUTPUT_WAIT: Duration = Duration::from_secs(2);

/// What an epoch plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EpochKind {
    /// A stream of the capture.
    Stream,
    /// Nothing: silence the host feeds on its own clock while a speaker or
    /// object test is armed, for the render to inject the test into.
    Idle,
}

/// One stream's playback: ring, clock tap and output stream.
struct Epoch {
    kind: EpochKind,
    producer: Producer,
    tap: Arc<SourceTap>,
    telemetry: Arc<SyncTelemetry>,
    output: PipewireSyncOutput,
    hold: u64,
    parser: SpdifParser,
    is_spdif: Option<bool>,
    pushed_frames: u64,
    dropped_frames: u64,
    /// Where the transport count starts: a byte offset and the source frames
    /// counted up to it. The first burst the parser found is frame 0 of the
    /// epoch; a pipe's carrier change rebases it.
    transport_base: Option<(u64, f64)>,
    last_transport: Option<Transport>,
    /// The capture's byte count where this epoch's first chunk started: 0
    /// for a stream the epoch saw open, more for one picked up on the way.
    transport_origin: Option<u64>,
    /// `follow` source on a pipe: its carrier, once the first burst named
    /// the data type, and the bytes pushed into the parser since its reset.
    pipe_carrier: Option<(u32, u32)>,
    /// Whether a burst has named the pipe's carrier (or said it is unknown).
    pipe_carrier_known: bool,
    /// IEC 61937 data type of the last burst: a change is a new stream.
    data_type: Option<u8>,
    parser_bytes: u64,
    started: Instant,
    /// What the clients were last told of this epoch's output.
    output_report: OutputReport,
    /// Set at end of stream: the epoch is dropped once what it queued has
    /// played. The engine thread never waits for it.
    ending_at: Option<Instant>,
    channel_mismatch_logged: bool,
    /// A PCM chunk on another channel count than the fixed input map's was
    /// refused, and said so.
    pcm_shape_logged: bool,
    /// `ORENDER_SYNC_TRACE`: one stderr line per chunk (arrival time, bytes
    /// so far, frames pushed so far), for studying a source's delivery.
    trace: bool,
    bytes_in: u64,
}

/// Where the stream comes from, and with it the source clock.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    /// A pipe a player writes on its own clock (`follow`).
    Pipe { path: PathBuf, drain: bool },
    /// orender's own PipeWire sink, on orender's clock (`own`).
    OwnSink {
        node_name: String,
        description: String,
        /// `priority.session`, if set.
        session_priority: Option<u32>,
    },
}

impl Source {
    fn follows_the_writer(&self) -> bool {
        matches!(self, Source::Pipe { .. })
    }
}

/// Everything a run of the host is given: what `sync-play` and `render`
/// each resolve from their own arguments and the config. The latency, the
/// loop settings and the output device are what the run starts with: OSC
/// clients change them while it runs ([`controls`]).
#[derive(Debug, Clone)]
pub struct HostConfig {
    pub config_path: Option<PathBuf>,
    /// The config as the host resolved it (the file with its own flags
    /// folded in), with whether a live-handoff sidecar came with it; `None`
    /// lets the engine read `config_path` itself.
    pub resolved_config: Option<(renderer::config::Config, bool)>,
    /// A speaker layout file that overrides the config's.
    pub speaker_layout: Option<PathBuf>,
    /// The channel render mode a host pins, over the config's.
    pub channel_render_mode: Option<renderer::live_params::ChannelRenderMode>,
    /// The decoder bridges, in load order; empty takes the config's, else
    /// auto-discovery (as [`Engine::from_paths`]).
    pub bridge_paths: Vec<PathBuf>,
    pub source: Source,
    /// End-to-end latency to hold (ms).
    pub target_latency_ms: f64,
    /// Phase-loop bandwidth (Hz); `None` is the source's own default.
    pub loop_bandwidth_hz: Option<f64>,
    /// Latency error beyond which the output realigns (ms).
    pub realign_threshold_ms: f64,
    /// PipeWire sink to play into (pinned); `None` is the session's choice.
    pub output_target: Option<String>,
    /// PipeWire quantum requested (frames).
    pub quantum: u32,
    pub osc: OscSettings,
    /// Stop after this many seconds (tests).
    pub seconds: Option<f64>,
    /// Print the servo telemetry every this many seconds (0 = never).
    pub report_every: f64,
}

/// What outlives a run of the host: the capture.
///
/// A restart from the config builds a new engine, but the pipe must not lose
/// its reader meanwhile (its writer would be killed by `SIGPIPE`), nor the
/// sink leave the graph (its player would be moved elsewhere). The capture
/// is kept as long as the next run asks for the same one.
#[derive(Default)]
pub struct HostSession {
    capture: Option<(CaptureKey, reader::Capture)>,
}

/// What a capture was opened with: a change in any of it opens another.
#[derive(Debug, Clone, PartialEq)]
struct CaptureKey {
    source: Source,
    /// The quantum the `own` sink asks for; the pipe has none. (The latency
    /// it advertises is not here: the sink advertises a new one in place.)
    sink_quantum: u32,
}

impl CaptureKey {
    fn of(config: &HostConfig) -> Self {
        let own = matches!(config.source, Source::OwnSink { .. });
        Self {
            source: config.source.clone(),
            sink_quantum: if own { config.quantum } else { 0 },
        }
    }
}

impl HostSession {
    /// The capture `config` asks for: the one already running when it is the
    /// same, with what it queued meanwhile discarded; else a new one, opened
    /// once the old one is closed.
    fn capture(&mut self, config: &HostConfig) -> Result<&reader::Capture> {
        let key = CaptureKey::of(config);
        if self.capture.as_ref().is_some_and(|(k, _)| *k == key) {
            log::info!("sync host: keeping the capture open");
        } else {
            self.capture = None;
            let capture = match &config.source {
                Source::Pipe { path, drain } => spawn_reader(path.clone(), *drain)?,
                Source::OwnSink {
                    node_name,
                    description,
                    session_priority,
                } => own_sink::spawn_own_sink(own_sink::OwnSinkConfig {
                    node_name: node_name.clone(),
                    description: description.clone(),
                    latency_ns: (config.target_latency_ms * 1e6) as i64,
                    quantum: config.quantum,
                    session_priority: *session_priority,
                })?,
            };
            self.capture = Some((key, capture));
        }
        let (_, capture) = self.capture.as_ref().context("the capture just opened")?;
        capture.discard_pending();
        Ok(capture)
    }
}

/// How a run of the host ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRun {
    pub exit: HostExit,
    /// The bridges in force when it ended: a live change of the bridge path
    /// is what a restart from the config must load.
    pub bridge_paths: Vec<PathBuf>,
}

/// Why a run of the host ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostExit {
    /// The capture ended or the time given ran out.
    Finished,
    /// The process was asked to stop.
    Shutdown,
    /// A reload of the config was asked for: the caller builds a new run.
    RestartFromConfig,
}

pub fn cmd_sync_play(args: &SyncPlayArgs, config_path: Option<PathBuf>) -> Result<()> {
    let config_path = config_path.or_else(renderer::config::default_config_path);
    let config = config_path
        .as_deref()
        .map(renderer::config::Config::load_or_default)
        .unwrap_or_default();
    let render = config.render.clone().unwrap_or_default();
    let source = match args.source {
        SourceKind::Pipe => Source::Pipe {
            path: args
                .input
                .clone()
                .context("--source pipe needs an INPUT pipe")?,
            drain: !args.no_drain_pipe,
        },
        SourceKind::Own => Source::OwnSink {
            node_name: args.sink_name.clone(),
            description: "Omniphony (sync)".into(),
            session_priority: args.sink_session_priority,
        },
    };
    // The same resolution as every host (flag, config, workflow port); off
    // unless one of them asks, as for `render`.
    let osc = OscSettings::resolve(
        Some(&render),
        &OscOverrides {
            enabled: args.osc.then_some(true).or(args.no_osc.then_some(false)),
            host: args.osc_host.clone(),
            port_out: args.osc_port,
            port_in: args.osc_rx_port,
            metering: None,
        },
        false,
    );
    let config = HostConfig {
        resolved_config: None,
        speaker_layout: None,
        channel_render_mode: None,
        bridge_paths: render.bridges(),
        source,
        target_latency_ms: resolve_target_latency_ms(args.target_latency_ms, render.latency_target),
        loop_bandwidth_hz: args
            .loop_bandwidth_hz
            .or(render.loop_bandwidth_hz.map(f64::from)),
        realign_threshold_ms: resolve_realign_threshold_ms(render.realign_threshold_ms),
        output_target: args
            .output_target
            .clone()
            .or_else(|| render.output_device.clone()),
        quantum: args.quantum,
        osc,
        seconds: args.seconds,
        report_every: args.report_every,
        config_path,
    };
    run(&config, &mut HostSession::default()).map(|_| ())
}

/// Run the host until its capture ends, its time runs out, or the process is
/// asked to stop or to restart from the config. `session` carries the
/// capture from one run to the next.
pub fn run(config: &HostConfig, session: &mut HostSession) -> Result<HostRun> {
    let mut engine = match &config.resolved_config {
        Some((resolved, live_restored)) => Engine::from_config(
            config.config_path.as_deref(),
            Some(resolved.clone()),
            *live_restored,
            config.speaker_layout.as_deref(),
            &config.bridge_paths,
            None,
            RENDER_RATE,
        ),
        None => Engine::from_paths(
            config.config_path.as_deref(),
            config.speaker_layout.as_deref(),
            &config.bridge_paths,
            None,
            RENDER_RATE,
        ),
    }
    .context("building the render engine")?;
    if let Some(mode) = config.channel_render_mode {
        engine.renderer_control().live.write().channel_render_mode = mode;
    }
    let channels = engine.channel_count() as usize;
    let positions: Vec<String> = engine
        .channel_layout()
        .into_iter()
        .map(|l| bridge_api::labels::canonical_name(l).to_string())
        .collect();
    let controls = Controls::new(config, engine.renderer_control());
    let mut settings = controls.settings();
    log::info!(
        "sync host: {channels} channels ({}), target {} ms, output {}",
        positions.join(","),
        settings.target_ms,
        settings
            .output_target
            .as_deref()
            .unwrap_or("<session default>")
    );

    if let Some(options) = config.osc.options() {
        log::info!("sync host: OSC on {}:{}", options.host, options.port_out);
        engine
            .enable_osc_with_host(options, Some(controls.handler()))
            .context("starting the OSC server")?;
    }
    let mut telemetry = HostTelemetry::new(settings.target_ms);
    // An output that failed to open is not retried on every chunk: only
    // when a stream starts or the settings change.
    let mut output_failed = false;
    let mut idle_feeder = IdleFeeder::default();

    let capture = session.capture(config)?;
    sys::notify_ready();
    let started = Instant::now();
    let mut epoch: Option<Epoch> = None;
    let mut next_report = config.report_every;

    let exit = loop {
        if sys::ShutdownHandle::is_requested() {
            break HostExit::Shutdown;
        }
        if sys::ShutdownHandle::is_restart_from_config_requested() {
            break HostExit::RestartFromConfig;
        }
        if sys::shutdown::is_standby_requested() {
            sys::shutdown::take_standby_request();
            // The audio output goes with the port: the player that asked for
            // it plays through its own.
            if let Some(ep) = epoch.take() {
                telemetry.close_epoch(&ep.telemetry.snapshot());
            }
            stand_by(&mut engine, capture);
            continue;
        }
        if let Some(limit) = config.seconds
            && started.elapsed().as_secs_f64() >= limit
        {
            break HostExit::Finished;
        }
        // What a client asked for since the last turn. The stream playing is
        // closed; its next chunk opens it again with the new settings, as
        // for a run that takes a running stream over.
        let wanted = controls.settings();
        if wanted != settings {
            log::info!("sync host: output settings changed: {wanted:?}");
            if wanted.target_ms != settings.target_ms {
                telemetry.set_configured_target(wanted.target_ms);
                controls.held_target(None);
            }
            settings = wanted;
            output_failed = false;
            if let Some(ep) = epoch.take() {
                telemetry.close_epoch(&ep.telemetry.snapshot());
            }
        }
        // The setpoint the player syncs to: the target, or what the servo
        // had to raise it to. (A store of the same value changes nothing.)
        capture.advertise_latency_ms(telemetry.held_target_ms());
        if epoch
            .as_ref()
            .and_then(|ep| ep.ending_at)
            .is_some_and(|at| Instant::now() >= at)
            && let Some(ep) = epoch.take()
        {
            telemetry.close_epoch(&ep.telemetry.snapshot());
            log::info!(
                "sync host: {} after {:.1} s; {}",
                match ep.kind {
                    EpochKind::Stream => "stream ended",
                    EpochKind::Idle => "idle epoch closed",
                },
                ep.started.elapsed().as_secs_f64(),
                describe(&ep, &capture.stats)
            );
        }
        let msg = match capture.recv_timeout(Duration::from_millis(20)) {
            Ok(m) => Some(m),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break HostExit::Finished,
        };
        match msg {
            Some(ReaderMsg::Start) => {
                // A new stream replaces whatever was still playing out.
                if let Some(ep) = epoch.take() {
                    telemetry.close_epoch(&ep.telemetry.snapshot());
                }
                engine.reset();
                epoch = open_epoch(
                    EpochKind::Stream,
                    config,
                    &settings,
                    telemetry.held_target_ms(),
                    channels,
                    &positions,
                    &controls,
                );
                output_failed = epoch.is_none();
                log::info!("sync host: stream opened, new epoch");
            }
            Some(ReaderMsg::Chunk {
                t,
                bytes,
                transport,
            }) => {
                // No epoch for it: the stream was already running when this
                // run took the capture over (a restart from the config, the
                // end of a standby). It is picked up where it is.
                // (Or the idle feed was playing: the stream takes over.)
                if epoch.as_ref().is_none_or(|ep| ep.kind == EpochKind::Idle) && !output_failed {
                    if let Some(idle) = epoch.take() {
                        telemetry.close_epoch(&idle.telemetry.snapshot());
                    }
                    engine.reset();
                    epoch = open_epoch(
                        EpochKind::Stream,
                        config,
                        &settings,
                        telemetry.held_target_ms(),
                        channels,
                        &positions,
                        &controls,
                    );
                    output_failed = epoch.is_none();
                    log::info!("sync host: stream already running, new epoch");
                }
                idle_feeder.note_real_frame(Instant::now());
                if let Some(ep) = epoch.as_mut() {
                    ep.last_transport =
                        transport_in_epoch(&mut ep.transport_origin, transport, bytes.len());
                    feed_chunk(&mut engine, ep, t, &bytes);
                }
            }
            Some(ReaderMsg::End) => {
                // (An idle epoch is not the stream's to end.)
                if let Some(ep) = epoch.take_if(|ep| ep.kind == EpochKind::Stream) {
                    // Let what is queued play out, then close the stream.
                    std::thread::sleep(Duration::from_secs_f64(
                        telemetry.held_target_ms() / 1e3 + 0.05,
                    ));
                    telemetry.close_epoch(&ep.telemetry.snapshot());
                    log::info!(
                        "sync host: stream ended after {:.1} s; {}",
                        ep.started.elapsed().as_secs_f64(),
                        describe(&ep, &capture.stats)
                    );
                }
            }
            None => {}
        }
        // With no stream playing, a speaker or object test is only heard if
        // something is rendered for it to be injected into: silence, paced on
        // the host's clock, through an epoch of its own.
        let stream_playing = epoch
            .as_ref()
            .is_some_and(|ep| ep.kind == EpochKind::Stream);
        if !stream_playing {
            let t = reference_now_s();
            match idle_feeder.poll(Instant::now(), &idle_feed_inputs(&engine)) {
                Some(chunk) => {
                    if epoch.is_none() && !output_failed {
                        engine.reset();
                        epoch = open_epoch(
                            EpochKind::Idle,
                            config,
                            &settings,
                            telemetry.held_target_ms(),
                            channels,
                            &positions,
                            &controls,
                        );
                        output_failed = epoch.is_none();
                        log::info!("sync host: test armed with nothing playing, idle epoch");
                    }
                    if let Some(ep) = epoch.as_mut().filter(|ep| ep.ending_at.is_none()) {
                        feed_silence(&mut engine, ep, t, chunk.sample_count);
                    }
                }
                None if !idle_feeder.is_feeding() => {
                    // Disarmed: let what is queued play out, then close.
                    if let Some(ep) = epoch.as_mut().filter(|ep| ep.ending_at.is_none()) {
                        ep.ending_at = Some(
                            Instant::now()
                                + Duration::from_secs_f64(telemetry.held_target_ms() / 1e3 + 0.05),
                        );
                    }
                }
                None => {}
            }
        }
        if let Some(ep) = epoch.as_mut().filter(|ep| ep.ending_at.is_none()) {
            ep.report_output(&controls, &settings);
        }
        telemetry.publish(
            &mut engine,
            epoch.as_ref().map(|ep| ep.telemetry.snapshot()).as_ref(),
        );
        if let Some(held_ms) = telemetry.take_raise() {
            controls.held_target(Some(held_ms));
        }
        if config.report_every > 0.0 && started.elapsed().as_secs_f64() >= next_report {
            next_report += config.report_every;
            if let Some(ep) = epoch.as_ref() {
                println!(
                    "t={:7.1}s {}",
                    started.elapsed().as_secs_f64(),
                    describe(ep, &capture.stats)
                );
            }
        }
    };
    if exit == HostExit::Shutdown {
        sys::notify_stopping();
    }
    Ok(HostRun {
        exit,
        bridge_paths: engine.bridge_paths(),
    })
}

/// Standby: a player on this machine asked for the OSC port (an
/// mpv-embedded engine). The port is released, nothing is played and what
/// the capture delivers is discarded, until the player hands the port back
/// or the process is asked to stop or restart.
fn stand_by(engine: &mut Engine, capture: &reader::Capture) {
    loop {
        log::info!("sync host: standby, OSC port and audio output released");
        engine.osc_enter_standby();
        loop {
            if sys::ShutdownHandle::is_requested()
                || sys::ShutdownHandle::is_restart_from_config_requested()
            {
                return;
            }
            if sys::shutdown::take_resume_request() {
                break;
            }
            capture.discard_pending();
            std::thread::sleep(Duration::from_millis(50));
        }
        if engine.osc_resume() {
            log::info!("sync host: resumed from standby");
            return;
        }
        // The port is still held (a resume that came early, the player still
        // owns it): running on without a listener would strand the Studio.
        // Stand by again; the port probe resumes for real once it is free.
        log::warn!("sync host: the OSC port is still held, standing by again");
    }
}

impl Epoch {
    /// Tell the clients what became of the output: playing on the device
    /// asked for, or not playing (the stream waits for a device that is not
    /// there, or lost it). A device that comes back is reported again.
    fn report_output(&mut self, controls: &Controls, settings: &OutputSettings) {
        let playing = self.output.is_streaming();
        let report = match (self.output_report, playing) {
            (OutputReport::Pending | OutputReport::Missing, true) => {
                controls.opened(settings);
                OutputReport::Playing
            }
            (OutputReport::Playing, false) => OutputReport::Missing,
            (OutputReport::Pending, false) if self.started.elapsed() >= OUTPUT_WAIT => {
                OutputReport::Missing
            }
            (report, _) => report,
        };
        if report == OutputReport::Missing && self.output_report != OutputReport::Missing {
            let device = settings
                .output_target
                .as_deref()
                .unwrap_or("the session's default output");
            log::warn!("sync host: {device} is not playing");
            controls.failed(format!("{device} is not playing: is the device there?"));
        }
        self.output_report = report;
    }
}

/// [`start_epoch`], with the outcome published to the clients: what plays,
/// or why nothing does. An output that cannot be opened (a device that is
/// gone) leaves the host running without one, for the client to pick
/// another, instead of ending the renderer.
fn open_epoch(
    kind: EpochKind,
    config: &HostConfig,
    settings: &OutputSettings,
    target_ms: f64,
    channels: usize,
    positions: &[String],
    controls: &Controls,
) -> Option<Epoch> {
    match start_epoch(kind, config, settings, target_ms, channels, positions) {
        // What plays is published once it does ([`Epoch::report_output`]).
        Ok(epoch) => Some(epoch),
        Err(e) => {
            log::error!("sync host: opening the output: {e:#}");
            controls.failed(format!("{e:#}"));
            None
        }
    }
}

/// Open a stream's playback. `target_ms` is the latency it starts at: the
/// configured target, or what an earlier stream showed the pipeline needs.
fn start_epoch(
    kind: EpochKind,
    config: &HostConfig,
    settings: &OutputSettings,
    target_ms: f64,
    channels: usize,
    positions: &[String],
) -> Result<Epoch> {
    let capacity = (RING_SECONDS * RENDER_RATE as f64) as usize;
    let (producer, consumer) = frame_ring(channels, capacity);
    let tap = Arc::new(SourceTap::new());
    let telemetry = Arc::new(SyncTelemetry::new());
    let mut core_config = OutputCoreConfig::new(channels, RENDER_RATE, RENDER_RATE, 8192);
    // The idle feed is the host's own clock, read exactly: nothing to follow.
    if kind == EpochKind::Stream && config.source.follows_the_writer() {
        let follow = audio_sync::ServoConfig::follow();
        core_config.servo = audio_sync::ServoConfig {
            source_rate_hz: core_config.servo.source_rate_hz,
            output_rate_hz: core_config.servo.output_rate_hz,
            resampler_lookahead_frames: core_config.servo.resampler_lookahead_frames,
            ..follow
        };
    }
    if let Some(hz) = settings.loop_bandwidth_hz {
        core_config.servo.loop_bandwidth_hz = hz;
    }
    core_config.servo.realign_threshold_s = settings.realign_threshold_ms / 1e3;
    core_config.servo.target_latency_s = target_ms / 1e3;
    let core = OutputCore::new(
        core_config,
        consumer,
        Arc::clone(&tap),
        Arc::clone(&telemetry),
    );
    let output = PipewireSyncOutput::start(
        PipewireSyncOutputConfig {
            node_name: "omniphony-sync".into(),
            target: settings.output_target.clone(),
            channels: channels as u32,
            positions: Some(positions.to_vec()),
            rate: RENDER_RATE,
            quantum: config.quantum,
        },
        core,
    )?;
    Ok(Epoch {
        kind,
        producer,
        tap,
        telemetry,
        output,
        hold: 0,
        parser: SpdifParser::new(),
        is_spdif: None,
        pushed_frames: 0,
        dropped_frames: 0,
        transport_base: None,
        last_transport: None,
        transport_origin: None,
        pipe_carrier: None,
        pipe_carrier_known: false,
        data_type: None,
        parser_bytes: 0,
        started: Instant::now(),
        output_report: OutputReport::Pending,
        ending_at: None,
        channel_mismatch_logged: false,
        pcm_shape_logged: false,
        trace: std::env::var_os("ORENDER_SYNC_TRACE").is_some(),
        bytes_in: 0,
    })
}

/// A chunk's transport position counted from an epoch's first chunk, as the
/// parser counts the bytes it is fed: the capture counts from the start of
/// its stream, which an epoch that picked the stream up on the way never saw.
/// `origin` is the epoch's: the capture's count where its first chunk began.
fn transport_in_epoch(
    origin: &mut Option<u64>,
    transport: Option<Transport>,
    len: usize,
) -> Option<Transport> {
    let transport = transport?;
    let origin = *origin.get_or_insert(transport.bytes_end.saturating_sub(len as u64));
    Some(Transport {
        bytes_end: transport.bytes_end.saturating_sub(origin),
        ..transport
    })
}

/// What the engine reads to decide the idle feed: the arm a client's test
/// pane keeps alive, and whether a test is running.
fn idle_feed_inputs(engine: &Engine) -> IdleFeedInputs {
    let control = engine.renderer_control();
    let live = control.live.read();
    IdleFeedInputs {
        arm_gen: live.speaker_test_idle_feed_gen,
        test_active: live.speaker_test.is_some() || live.object_test.is_some(),
        sample_rate: RENDER_RATE,
    }
}

/// Render `frames` of silence into the idle epoch and publish the host's own
/// clock as its source: `t` is when the feeder counted them.
fn feed_silence(engine: &mut Engine, ep: &mut Epoch, t: f64, frames: u32) {
    match engine.render_silence(frames) {
        Ok(Some(block)) => push_blocks(engine, ep, vec![block]),
        Ok(None) => {}
        Err(e) => log::debug!("sync host: idle render error: {e:#}"),
    }
    if ep.pushed_frames > 0 {
        ep.tap.publish(t, ep.pushed_frames);
    }
}

/// Render one chunk of the sink's linear PCM and push the audio. The clock
/// is published by the caller, from the transport, as for a bitstream.
fn feed_pcm(engine: &mut Engine, ep: &mut Epoch, tr: Transport, bytes: &[u8]) {
    if tr.channels as usize != pcm::INPUT_MAP.len() {
        if !ep.pcm_shape_logged {
            ep.pcm_shape_logged = true;
            log::warn!(
                "sync host: linear PCM on {} channels dropped: the input map has {}",
                tr.channels,
                pcm::INPUT_MAP.len()
            );
        }
        return;
    }
    // The count starts with the stream: there is no burst to find first.
    ep.transport_base.get_or_insert((0, 0.0));
    match engine.render_host_pcm(&pcm::frame(bytes, tr.rate)) {
        Ok(Some(block)) => push_blocks(engine, ep, vec![block]),
        Ok(None) => {}
        Err(e) => log::debug!("sync host: PCM render error: {e:#}"),
    }
}

/// Deframe, decode and render one chunk, push the audio, publish the clock.
fn feed_chunk(engine: &mut Engine, ep: &mut Epoch, t: f64, bytes: &[u8]) {
    let pcm_transport = ep
        .last_transport
        .filter(|tr| tr.format == CarrierFormat::PcmF32);
    if let Some(tr) = pcm_transport {
        feed_pcm(engine, ep, tr, bytes);
        publish_clock(ep, t, bytes.len());
        return;
    }
    let has_sync = spdif::contains_sync(bytes);
    if ep.is_spdif.is_none() && bytes.len() >= 4 {
        ep.is_spdif = Some(has_sync);
    } else if ep.is_spdif == Some(false) && has_sync {
        ep.is_spdif = Some(true);
        ep.parser.reset();
        ep.parser_bytes = 0;
        ep.transport_base = None;
    }
    if ep.is_spdif == Some(true) {
        ep.parser.push_bytes(bytes);
        ep.parser_bytes += bytes.len() as u64;
        while let Some(packet) = ep.parser.get_next_packet() {
            ep.hold = decoder_hold_frames(packet.data_type);
            if ep.data_type.is_some_and(|d| d != packet.data_type) {
                // A track change: the content and its timing start over. The
                // count restarts from where the ring is, as at the start of an
                // epoch: what the old stream's decoder never delivered must
                // not stay counted (it would raise the floor at every change).
                log::info!(
                    "sync host: IEC 61937 data type {} -> {}, new phase",
                    ep.data_type.unwrap_or_default(),
                    packet.data_type
                );
                ep.transport_base = None;
                ep.tap.mark_break();
            }
            ep.data_type = Some(packet.data_type);
            if ep.last_transport.is_none() {
                follow_pipe_carrier(ep, packet.data_type);
            }
            if ep.transport_base.is_none() && transport(ep).is_some() {
                ep.transport_base = Some((packet.start_byte, ep.pushed_frames as f64));
            }
            match engine.process(&packet.payload, RInputTransport::Iec61937, packet.data_type) {
                Ok(blocks) => push_blocks(engine, ep, blocks),
                Err(e) => log::debug!("sync host: decode error: {e:#}"),
            }
        }
    } else {
        match engine.process(bytes, RInputTransport::Raw, 0) {
            Ok(blocks) => push_blocks(engine, ep, blocks),
            Err(e) => log::debug!("sync host: decode error: {e:#}"),
        }
    }
    publish_clock(ep, t, bytes.len());
}

/// Publish where the source stood when the chunk that ends at `t` arrived.
fn publish_clock(ep: &mut Epoch, t: f64, chunk_bytes: usize) {
    match (transport(ep), ep.transport_base) {
        // The exact transport position, counted from the first burst: from
        // the `own` sink, or from the bytes of a pipe whose carrier is known
        // (rebased where a track change moved it to another carrier).
        // The decoder's hold and batching are inside the measurement, as
        // they are inside the latency. On a pipe this keeps the readings
        // free of the decoder's burst granularity, which would otherwise
        // show as up to a chunk of arrival lateness.
        (Some(tr), Some((base_bytes, base_frames))) => {
            let received =
                base_frames + transport_frames(tr.bytes_end.saturating_sub(base_bytes), tr);
            if received > 0.0 {
                ep.tap.publish(t, received.round() as u64);
            }
        }
        (Some(_), None) => {}
        // A pipe of unknown carrier: what was decoded, plus the codec's hold.
        (None, _) => {
            if ep.pushed_frames > 0 {
                ep.tap.publish(t, ep.pushed_frames + ep.hold);
            }
        }
    }
    ep.bytes_in += chunk_bytes as u64;
    if ep.trace {
        eprintln!("SYNC_TRACE {t:.6} {} {}", ep.bytes_in, ep.pushed_frames);
    }
}

/// Keep a pipe's carrier in step with the data type of its bursts: a track
/// change can move the player to another carrier (E-AC-3 on 192 kHz to AC-3
/// on 48 kHz). The count restarts with it (a data type change is a break).
fn follow_pipe_carrier(ep: &mut Epoch, data_type: u8) {
    let carrier = pipe_carrier(data_type);
    if carrier == ep.pipe_carrier && (carrier.is_some() || ep.pipe_carrier_known) {
        return;
    }
    if ep.pipe_carrier_known {
        log::info!("sync host: IEC 61937 data type {data_type}, carrier now {carrier:?}");
    } else if carrier.is_none() {
        log::info!(
            "sync host: carrier of IEC 61937 data type {data_type} unknown, \
             counting decoded frames"
        );
    }
    ep.pipe_carrier = carrier;
    ep.pipe_carrier_known = true;
}

/// Where the bytes fed so far end on the carrier's timeline, if known.
fn transport(ep: &Epoch) -> Option<Transport> {
    ep.last_transport.or_else(|| {
        ep.pipe_carrier.map(|(rate, channels)| Transport {
            rate,
            channels,
            format: CarrierFormat::Iec958,
            bytes_end: ep.parser_bytes,
        })
    })
}

/// Source PCM frames (at the render rate) a byte offset on a carrier stands
/// for. IEC 958 carriers run at the content rate times a fixed factor (1 for
/// AC-3/DTS at 48 kHz, 4 for E-AC-3/TrueHD/DTS-HD at 192 kHz); linear PCM is
/// its own carrier.
fn transport_frames(bytes: u64, tr: Transport) -> f64 {
    let frame_bytes = tr.format.sample_bytes() * tr.channels.max(1);
    let carrier_frames = bytes as f64 / frame_bytes as f64;
    carrier_frames * RENDER_RATE as f64 / tr.rate.max(1) as f64
}

fn push_blocks(engine: &mut Engine, ep: &mut Epoch, blocks: Vec<RenderedAudio>) {
    for block in &blocks {
        if block.n_channels as usize != ep.producer.channels() {
            if !ep.channel_mismatch_logged {
                ep.channel_mismatch_logged = true;
                log::error!(
                    "sync host: rendered {} channels into a {}-channel output; block dropped",
                    block.n_channels,
                    ep.producer.channels()
                );
            }
            continue;
        }
        let frames = block.n_frames as u64;
        let pushed = ep.producer.push(&block.samples) as u64;
        ep.pushed_frames += pushed;
        if pushed < frames {
            // The ring is full: the output stopped draining. Counted, and the
            // frames never enter `N_in`, so the latency measurement stays true.
            ep.dropped_frames += frames - pushed;
        }
    }
    engine.recycle(blocks);
}

fn describe(ep: &Epoch, stats: &reader::ReaderStats) -> String {
    use std::sync::atomic::Ordering;
    let s = ep.telemetry.snapshot();
    format!(
        "streaming={} phase={} latency={} err={:+.3}ms floor={:.1}ms src={:+.2}ppm dev={:+.2}ppm corr={:+.2}ppm realigns={} underruns={} rephases={} short={} pushed={} ring_drops={} chunk_drops={} bytes={}",
        ep.output.is_streaming(),
        s.phase,
        s.latency_ms
            .map(|l| format!("{l:.3}ms"))
            .unwrap_or_else(|| "-".into()),
        s.error_ms,
        s.floor_ms,
        s.source_ppm,
        s.device_ppm,
        s.correction_ppm,
        s.realigns,
        s.underruns,
        s.source_rephases,
        s.short_reads,
        ep.pushed_frames,
        ep.dropped_frames,
        stats.chunks_dropped.load(Ordering::Relaxed),
        stats.bytes_read.load(Ordering::Relaxed),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A second of each known pipe carrier is a second of source frames at
    /// the render rate.
    /// The flag wins over the config's `latency_target`, which wins over the
    /// built-in default.
    #[test]
    fn the_latency_target_is_the_flag_then_the_config_then_the_default() {
        assert_eq!(resolve_target_latency_ms(Some(80.0), Some(500)), 80.0);
        assert_eq!(resolve_target_latency_ms(None, Some(500)), 500.0);
        assert_eq!(
            resolve_target_latency_ms(None, None),
            DEFAULT_TARGET_LATENCY_MS
        );
    }

    fn config(source: Source, target_latency_ms: f64) -> HostConfig {
        HostConfig {
            config_path: None,
            resolved_config: None,
            speaker_layout: None,
            channel_render_mode: None,
            bridge_paths: Vec::new(),
            source,
            target_latency_ms,
            loop_bandwidth_hz: None,
            realign_threshold_ms: 20.0,
            output_target: None,
            quantum: 1024,
            osc: OscSettings::resolve(None, &OscOverrides::default(), false),
            seconds: None,
            report_every: 0.0,
        }
    }

    /// A restart from the config keeps the capture unless the config asks
    /// for another one. Another latency is not another capture: the pipe
    /// advertises none, and the `own` sink advertises the new one in place.
    #[test]
    fn a_capture_is_reopened_only_when_what_it_was_opened_with_changes() {
        let pipe = |path: &str| Source::Pipe {
            path: path.into(),
            drain: true,
        };
        let sink = || Source::OwnSink {
            node_name: "omniphony".into(),
            description: "Omniphony".into(),
            session_priority: None,
        };
        let key = |source, ms| CaptureKey::of(&config(source, ms));
        assert_eq!(key(pipe("/a"), 150.0), key(pipe("/a"), 200.0));
        assert_ne!(key(pipe("/a"), 150.0), key(pipe("/b"), 150.0));
        assert_eq!(key(sink(), 150.0), key(sink(), 200.0));
        assert_ne!(key(sink(), 150.0), key(pipe("/a"), 150.0));
    }

    /// An epoch that picks a stream up on the way counts the transport from
    /// its own first chunk, as its parser counts the bytes it is fed; one
    /// that saw the stream open counts as the capture does.
    #[test]
    fn an_epoch_counts_the_transport_from_its_first_chunk() {
        let transport = |bytes_end| {
            Some(Transport {
                rate: 192_000,
                channels: 2,
                format: CarrierFormat::Iec958,
                bytes_end,
            })
        };
        let mut origin = None;
        let mut in_epoch = |t: Option<Transport>, len: usize| {
            transport_in_epoch(&mut origin, t, len).map(|t| t.bytes_end)
        };
        // Picked up 1 MiB into the stream, with a 4096-byte chunk.
        assert_eq!(in_epoch(transport((1 << 20) + 4096), 4096), Some(4096));
        assert_eq!(in_epoch(transport((1 << 20) + 12288), 8192), Some(12288));
        // A pipe has no transport: nothing to rebase.
        assert_eq!(in_epoch(None, 4096), None);

        let mut origin = None;
        let from_start = transport_in_epoch(&mut origin, transport(4096), 4096);
        assert_eq!(from_start.map(|t| t.bytes_end), Some(4096));
        assert_eq!(origin, Some(0));
    }

    #[test]
    fn pipe_carriers_count_one_second_per_second() {
        for data_type in [1u8, 11, 12, 13, 21, 22] {
            let (rate, channels) = pipe_carrier(data_type).unwrap();
            let tr = Transport {
                rate,
                channels,
                format: CarrierFormat::Iec958,
                bytes_end: 0,
            };
            let second = rate as u64 * channels as u64 * 2;
            assert_eq!(
                transport_frames(second, tr),
                RENDER_RATE as f64,
                "data type {data_type}"
            );
        }
        // DTS-HD rides 2 or 8 channels: not guessed.
        assert_eq!(pipe_carrier(17), None);
        // The sink's linear PCM: 8 channels of 4-byte floats at 48 kHz.
        let pcm = Transport {
            rate: own_sink::PCM_RATE_HZ,
            channels: own_sink::PCM_CHANNELS,
            format: CarrierFormat::PcmF32,
            bytes_end: 0,
        };
        assert_eq!(transport_frames(48_000 * 8 * 4, pcm), RENDER_RATE as f64);
    }
}
