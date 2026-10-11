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

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use audio_output::sync_output::{
    OutputCore, OutputCoreConfig, PipewireSyncOutput, PipewireSyncOutputConfig, SourceTap,
    SyncSnapshot, SyncTelemetry,
};
use audio_rt::{Producer, frame_ring};
use bridge_api::RInputTransport;
use clap::Args;
use orender_engine::engine::{Engine, RenderedAudio};
use orender_engine::{OscOverrides, OscSettings};
use renderer::live_params::OutputChannelMapping;
use spdif::SpdifParser;

use crate::cli::decode::idle_feed::{IdleFeedInputs, IdleFeeder};
use audio_output::control::SourceClock;
use audio_output::sync_output::reference_now_s;
use audio_output::{CafChannelDesc, FileAudioWriter, FileSinkFormat};
use controls::{Controls, OutputSettings};
use reader::{CarrierFormat, ReaderMsg, Transport, spawn_file_reader, spawn_reader};
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

/// The source clock asked for: the config's `render.source_clock`, else the
/// input's own. A name this build does not know is the input's own too.
pub fn resolve_source_clock(config: Option<&str>) -> SourceClock {
    config
        .and_then(SourceClock::from_name)
        .unwrap_or(SourceClock::Auto)
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

/// The rate the engine is built at: what it renders before a stream sets its
/// own (the render follows the stream's rate, and so does the output).
const ENGINE_RATE: u32 = 48_000;
/// Ring capacity: comfortably above any latency target.
const RING_SECONDS: f64 = 4.0;
/// The device's quantum is asked for as a duration: this many frames at
/// this rate, whatever the device's own rate.
const QUANTUM_RATE: u32 = 48_000;

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
/// a data type, as mpv's spdif wrapper sets it up. The rate is the nominal
/// one, for the log: the count goes by [`content_per_carrier_frame`]. DTS-HD
/// (17) is left out: HRA rides 2 channels and MA 8, which the burst alone
/// does not tell.
fn pipe_carrier(data_type: u8) -> Option<(u32, u32)> {
    match data_type {
        1 | 11..=13 => Some((48_000, 2)), // AC-3, DTS core
        21 => Some((192_000, 2)),         // E-AC-3
        22 => Some((192_000, 8)),         // TrueHD/MAT
        _ => None,
    }
}

/// Source frames one frame of a carrier stands for, for a stream rendered at
/// `stream_rate`; `None` while that rate is needed and not known yet (no
/// block rendered).
///
/// The IEC 61937 framing fixes it for the core codecs, whatever rate the
/// player declares for the carrier (mpv declares 192 kHz for every E-AC-3
/// and TrueHD stream): an AC-3 or DTS burst spans as many carrier frames as
/// it holds samples, an E-AC-3 one four times as many. A MAT frame holds
/// 1/200 s of the top rate of the content's family. Anything else (DTS-HD,
/// linear PCM) lasts on its carrier what it lasts played.
fn content_per_carrier_frame(
    tr: Transport,
    data_type: Option<u8>,
    stream_rate: Option<u32>,
) -> Option<f64> {
    match (tr.format, data_type) {
        (CarrierFormat::Iec958, Some(1 | 11..=13)) => Some(1.0),
        (CarrierFormat::Iec958, Some(21)) => Some(0.25),
        (CarrierFormat::Iec958, Some(22)) => stream_rate.map(|rate| {
            let family_top = if rate % 11_025 == 0 { 176_400 } else { 192_000 };
            f64::from(rate) / f64::from(family_top)
        }),
        _ => stream_rate.map(|rate| f64::from(rate) / f64::from(tr.rate.max(1))),
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

/// What a rendered block is made of, and so what an output is opened for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BlockFormat {
    /// The stream's rate, which the render follows (Hz).
    rate: u32,
    /// The output's width: the layout's speakers, or the binaural pair.
    channels: usize,
}

/// The device side of an epoch: opened for the format of the first block
/// rendered, and again when that format changes (another stream rate, the
/// output switched between speakers and headphones) or when the channel
/// mapping does.
struct Device {
    producer: Producer,
    output: PipewireSyncOutput,
    format: BlockFormat,
    mapping: OutputChannelMapping,
    /// The rate the device runs at: the stream's, or the one asked for.
    device_rate: u32,
}

/// The file the run writes. One for the run, shared by its epochs: a capture
/// holds every stream of the run, and a regular file opened again would
/// start over.
type FileOut = Rc<RefCell<FileState>>;

/// See [`FileOut`]. The writer is opened for the first block's format. A
/// block in another format (a stream at another rate, another output width)
/// cannot continue what is there: the samples are written as they come, so
/// they would sit under a header that describes the ones before them. That
/// block starts the file over, as the legacy host does.
struct FileState {
    destination: String,
    container: FileSinkFormat,
    open: Option<(FileAudioWriter, BlockFormat)>,
}

impl FileState {
    /// Write a block. `speakers` gives the speakers' positions (azimuth,
    /// elevation, distance) when the render is one of a speaker array.
    fn write(
        &mut self,
        format: BlockFormat,
        samples: &[f32],
        speakers: impl FnOnce() -> Option<Vec<[f32; 3]>>,
    ) -> Result<()> {
        if self.open.as_ref().is_none_or(|(_, open)| *open != format) {
            if let Some((mut writer, was)) = self.open.take() {
                log::warn!(
                    "sync host: {} Hz x {} cannot continue the {} Hz x {} capture in '{}': \
                     starting the file over",
                    format.rate,
                    format.channels,
                    was.rate,
                    was.channels,
                    self.destination
                );
                let _ = writer.flush();
            }
            // A CAF capture of a speaker-array render says where its
            // speakers are.
            let speakers = speakers()
                .filter(|positions| positions.len() == format.channels)
                .map(|positions| {
                    positions
                        .into_iter()
                        .map(|[azimuth, elevation, distance]| {
                            CafChannelDesc::spherical(azimuth, elevation, distance)
                        })
                        .collect()
                });
            let writer = FileAudioWriter::new(
                &self.destination,
                self.container,
                format.rate,
                format.channels as u32,
                speakers,
            )
            .with_context(|| {
                format!(
                    "opening the audio output destination '{}'",
                    self.destination
                )
            })?;
            self.open = Some((writer, format));
        }
        let (writer, _) = self.open.as_mut().context("the writer just opened")?;
        writer
            .write_samples(samples)
            .with_context(|| format!("writing '{}'", self.destination))
    }

    fn flush(&mut self) {
        if let Some((writer, _)) = self.open.as_mut()
            && let Err(e) = writer.flush()
        {
            log::error!("sync host: flushing the output file: {e}");
        }
    }
}

/// One stream's playback: ring, clock tap and output stream.
struct Epoch {
    kind: EpochKind,
    /// The device's ring and stream, once a block has said what to open it
    /// for; always `None` when the output is a file.
    device: Option<Device>,
    /// The device could not be opened: not tried again for this epoch.
    device_failed: bool,
    /// The file the run writes, when the output is one.
    file: Option<FileOut>,
    /// The stream's rate, once a block has been rendered: what its frames
    /// are counted at on the transport.
    stream_rate: Option<u32>,
    tap: Arc<SourceTap>,
    telemetry: Arc<SyncTelemetry>,
    /// The device paces the source: rendered blocks wait in `pending` until
    /// the ring is below `pace_limit_frames`, and nothing is dropped.
    paced: bool,
    pace_limit_frames: usize,
    /// Rendered blocks not in the ring yet, each with the frames of it
    /// already pushed.
    pending: VecDeque<(RenderedAudio, usize)>,
    /// The stream is over: the epoch closes once `pending` is empty and what
    /// is in the ring has played.
    finishing: bool,
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
    /// `follow` source on a pipe: the carrier (frame rate, channels) its
    /// bytes are counted on, set from the data type of its bursts. `None`
    /// counts decoded frames instead.
    pipe_carrier: Option<(u32, u32)>,
    pipe_carrier_known: bool,
    /// IEC 61937 data type of the last burst: a change is a new stream.
    data_type: Option<u8>,
    parser_bytes: u64,
    started: Instant,
    /// When the device was last opened: how long it has had to start.
    device_opened: Instant,
    /// The servo's last state on each device this epoch closed on the way
    /// (the output reopened for another format): the host adds their counts
    /// to its totals.
    closed: Vec<SyncSnapshot>,
    /// The output file could not be written: the run ends on it.
    fatal: Option<anyhow::Error>,
    /// What the clients were last told of this epoch's output.
    output_report: OutputReport,
    /// Set at end of stream: the epoch is dropped once what it queued has
    /// played. The engine thread never waits for it.
    ending_at: Option<Instant>,
    write_error_logged: bool,
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
    /// A regular file, or standard input (`-`): one stream read to its end,
    /// with no clock (`none`).
    File { path: PathBuf },
}

impl Source {
    /// Whether the device paces this source under the clock asked for: it
    /// has no clock of its own, so playback runs at the device's pace and the
    /// source is held back. Always for a file; for a pipe when its writer is
    /// said to have none (`none`: faster than real time); never for the sink,
    /// which the renderer drives. A clock the input cannot have is the
    /// input's own instead.
    fn device_paced(&self, clock: SourceClock) -> bool {
        match self {
            Source::File { .. } => true,
            Source::Pipe { .. } => clock == SourceClock::None,
            Source::OwnSink { .. } => false,
        }
    }

    /// Whether the source's clock is read from the arrival of its bytes.
    fn follows_the_writer(&self, clock: SourceClock) -> bool {
        matches!(self, Source::Pipe { .. }) && !self.device_paced(clock)
    }
}

/// Where the rendered audio goes.
#[derive(Debug, Clone, PartialEq)]
pub enum OutputKind {
    /// The audio device, through the sync output.
    Device,
    /// A file, a FIFO or standard output (`-`): no device and no clock, the
    /// samples are written as they are rendered.
    File {
        destination: String,
        format: FileSinkFormat,
    },
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
    /// Where the rendered audio goes.
    pub output: OutputKind,
    /// The clock the source is taken to run on, as asked for.
    pub source_clock: SourceClock,
    /// Stay up after a stream ends, for the next one. Without it the run
    /// ends with the first stream.
    pub continuous: bool,
    /// An offline render (a file in, a file out, once): nothing waits on the
    /// output, so the render holds a frame for what it needs instead of
    /// building it in the background.
    pub offline: bool,
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
        // A file is read once: a run on the same file reads it again.
        let reusable = !matches!(config.source, Source::File { .. });
        if reusable && self.capture.as_ref().is_some_and(|(k, _)| *k == key) {
            log::info!("sync host: keeping the capture open");
        } else {
            self.capture = None;
            let capture = match &config.source {
                Source::File { path } => spawn_file_reader(path)
                    .with_context(|| format!("opening the input {}", path.display()))?,
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
        output: OutputKind::Device,
        source_clock: resolve_source_clock(render.source_clock.as_deref()),
        continuous: true,
        offline: false,
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
            ENGINE_RATE,
        ),
        None => Engine::from_paths(
            config.config_path.as_deref(),
            config.speaker_layout.as_deref(),
            &config.bridge_paths,
            None,
            ENGINE_RATE,
        ),
    }
    .context("building the render engine")?;
    if let Some(mode) = config.channel_render_mode {
        engine.renderer_control().live.write().channel_render_mode = mode;
    }
    if config.offline {
        engine.set_synchronous_stage_builds(true);
    }
    let controls = Controls::new(config, engine.renderer_control());
    let mut settings = controls.settings();
    log::info!(
        "sync host: target {} ms, output {}",
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
    let mut idle_feeder = IdleFeeder::default();
    // The capture is done (a file read to its end): nothing more will come.
    let mut capture_gone = false;
    // A stream has ended: without `continuous`, the run ends with it.
    let mut stream_ended = false;

    let capture = session.capture(config)?;
    let file_out = match &config.output {
        OutputKind::Device => None,
        OutputKind::File {
            destination,
            format,
        } => Some(Rc::new(RefCell::new(FileState {
            destination: destination.clone(),
            container: *format,
            open: None,
        }))),
    };
    let env = EpochEnv {
        config,
        controls: &controls,
        file: file_out.as_ref(),
    };
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
            if let Some(mut ep) = epoch.take() {
                close_epoch(&mut telemetry, &mut ep);
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
            if let Some(mut ep) = epoch.take() {
                close_epoch(&mut telemetry, &mut ep);
            }
        }
        // What a block needs to open the output it is for.
        let ctx = PlayCtx {
            env: &env,
            settings: &settings,
            target_ms: telemetry.held_target_ms(),
        };
        // The setpoint the player syncs to: the target, or what the servo
        // had to raise it to. (A store of the same value changes nothing.)
        capture.advertise_latency_ms(telemetry.held_target_ms());
        capture.set_paced(config.source.device_paced(settings.source_clock));
        if let Some(ep) = epoch.as_mut() {
            ep.follow_mapping(engine.output_channel_mapping());
            ep.pump_pending(&mut engine);
            ep.finish_when_played(telemetry.held_target_ms());
        }
        if epoch
            .as_ref()
            .and_then(|ep| ep.ending_at)
            .is_some_and(|at| Instant::now() >= at)
            && let Some(mut ep) = epoch.take()
        {
            if let Some(file) = &ep.file {
                file.borrow_mut().flush();
            }
            close_epoch(&mut telemetry, &mut ep);
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
        // Nothing left to play, and nothing more to come: the run is over,
        // unless it stays up for what a client may ask of it.
        if epoch.is_none() && (stream_ended || capture_gone) && !config.continuous {
            break HostExit::Finished;
        }
        // A paced source is held back here: while rendered audio waits for
        // room in the ring, nothing more is taken from it.
        let backlog = epoch.as_ref().is_some_and(|ep| !ep.pending.is_empty());
        let msg = if backlog {
            std::thread::sleep(Duration::from_millis(2));
            None
        } else if capture_gone {
            std::thread::sleep(Duration::from_millis(20));
            None
        } else {
            match capture.recv_timeout(Duration::from_millis(20)) {
                Ok(m) => Some(m),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    capture_gone = true;
                    None
                }
            }
        };
        match msg {
            Some(ReaderMsg::Start) => {
                // A new stream replaces whatever was still playing out.
                if let Some(mut ep) = epoch.take() {
                    close_epoch(&mut telemetry, &mut ep);
                }
                engine.reset();
                stream_ended = false;
                epoch = Some(open_epoch(EpochKind::Stream, &env));
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
                if epoch.as_ref().is_none_or(|ep| ep.kind == EpochKind::Idle) {
                    if let Some(mut idle) = epoch.take() {
                        close_epoch(&mut telemetry, &mut idle);
                    }
                    engine.reset();
                    epoch = Some(open_epoch(EpochKind::Stream, &env));
                    log::info!("sync host: stream already running, new epoch");
                }
                idle_feeder.note_real_frame(Instant::now());
                if let Some(ep) = epoch.as_mut() {
                    ep.last_transport =
                        transport_in_epoch(&mut ep.transport_origin, transport, bytes.len());
                    feed_chunk(&mut engine, ep, &ctx, t, &bytes);
                }
            }
            Some(ReaderMsg::End) => {
                stream_ended = true;
                // (An idle epoch is not the stream's to end.)
                if let Some(ep) = epoch
                    .as_mut()
                    .filter(|ep| ep.kind == EpochKind::Stream && !ep.finishing)
                {
                    finish_stream(&mut engine, ep, &ctx);
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
        if !stream_playing && config.output == OutputKind::Device {
            let t = reference_now_s();
            match idle_feeder.poll(Instant::now(), &idle_feed_inputs(&engine)) {
                Some(chunk) => {
                    if epoch.is_none() {
                        engine.reset();
                        epoch = Some(open_epoch(EpochKind::Idle, &env));
                        log::info!("sync host: test armed with nothing playing, idle epoch");
                    }
                    if let Some(ep) = epoch.as_mut().filter(|ep| ep.ending_at.is_none()) {
                        feed_silence(&mut engine, ep, &ctx, t, chunk.sample_count);
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
        if let Some(ep) = epoch.as_mut() {
            // A file that cannot be written ends the run: there is nothing
            // else it was asked for.
            if let Some(e) = ep.fatal.take() {
                return Err(e);
            }
            for closed in ep.closed.drain(..) {
                telemetry.close_epoch(&closed);
            }
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
    drop(epoch);
    if let Some(file) = &file_out {
        file.borrow_mut().flush();
    }
    if exit == HostExit::Shutdown {
        sys::notify_stopping();
    }
    Ok(HostRun {
        exit,
        bridge_paths: engine.bridge_paths(),
    })
}

/// An epoch is over: its counts, and those of the devices it closed on the
/// way, stay in the totals.
fn close_epoch(telemetry: &mut HostTelemetry, ep: &mut Epoch) {
    for closed in ep.closed.drain(..) {
        telemetry.close_epoch(&closed);
    }
    telemetry.close_epoch(&ep.telemetry.snapshot());
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
        let Some(device) = &self.device else {
            return;
        };
        let playing = device.output.is_streaming();
        let report = match (self.output_report, playing) {
            (OutputReport::Pending | OutputReport::Missing, true) => {
                controls.opened(settings, device.device_rate);
                OutputReport::Playing
            }
            (OutputReport::Playing, false) => OutputReport::Missing,
            (OutputReport::Pending, false) if self.device_opened.elapsed() >= OUTPUT_WAIT => {
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

    fn new(kind: EpochKind, file: Option<FileOut>) -> Self {
        Self {
            kind,
            device: None,
            device_failed: false,
            file,
            stream_rate: None,
            tap: Arc::new(SourceTap::new()),
            telemetry: Arc::new(SyncTelemetry::new()),
            paced: false,
            pace_limit_frames: 0,
            pending: VecDeque::new(),
            finishing: false,
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
            device_opened: Instant::now(),
            closed: Vec::new(),
            fatal: None,
            output_report: OutputReport::Pending,
            ending_at: None,
            write_error_logged: false,
            pcm_shape_logged: false,
            trace: std::env::var_os("ORENDER_SYNC_TRACE").is_some(),
            bytes_in: 0,
        }
    }

    /// Open the device for `format`, in place of the one open for another
    /// format if there is one.
    fn open_device(&mut self, engine: &Engine, ctx: &PlayCtx<'_>, format: BlockFormat) {
        if let Some(old) = &self.device {
            log::info!(
                "sync host: output format {} Hz x {} -> {} Hz x {}, output reopened",
                old.format.rate,
                old.format.channels,
                format.rate,
                format.channels
            );
        }
        self.close_device();
        if self.stream_rate.is_some_and(|rate| rate != format.rate) {
            // Another rate with no new stream announced: what was counted
            // was in the old rate's frames. The count starts over at the
            // next burst.
            self.transport_base = None;
        }
        self.stream_rate = Some(format.rate);
        self.device_opened = Instant::now();
        match start_device(self.kind, engine, ctx, format, &self.tap, &self.telemetry) {
            Ok((device, pace_limit)) => {
                self.paced = pace_limit.is_some();
                self.pace_limit_frames = pace_limit.unwrap_or(0);
                self.device = Some(device);
            }
            Err(e) => {
                log::error!("sync host: opening the output: {e:#}");
                ctx.env.controls.failed(format!("{e:#}"));
                self.device_failed = true;
            }
        }
    }

    /// Close the device, if one is open: the next block opens another.
    fn close_device(&mut self) {
        if self.device.take().is_some() {
            self.closed.push(self.telemetry.snapshot());
            self.restart_count();
        }
    }

    /// A device was closed under a stream that goes on: the next one starts
    /// with an empty ring, a new servo and a new clock tap, and counts its
    /// frames from 0. The source's count is moved by what went to the old
    /// ring, so the next ring's first frame is still the stream's frame that
    /// follows them, and the decoder's hold stays inside the measurement.
    fn restart_count(&mut self) {
        // What waited for the old ring was made for it.
        let waiting: u64 = self
            .pending
            .drain(..)
            .map(|(block, offset)| (block.n_frames - offset) as u64)
            .sum();
        self.dropped_frames += waiting;
        let gone = (self.pushed_frames + waiting) as f64;
        self.transport_base = self
            .transport_base
            .map(|(bytes, frames)| (bytes, frames - gone));
        self.pushed_frames = 0;
        self.tap = Arc::new(SourceTap::new());
        self.telemetry = Arc::new(SyncTelemetry::new());
        self.output_report = OutputReport::Pending;
    }

    /// The channel mapping is a live option: a device opened under another
    /// one is closed, and the next block opens it again.
    fn follow_mapping(&mut self, mapping: OutputChannelMapping) {
        if self.device.as_ref().is_some_and(|d| d.mapping != mapping) {
            log::info!("sync host: output channel mapping changed, output reopened");
            self.close_device();
        }
    }

    /// Move rendered blocks that wait on a paced source into the ring, up to
    /// the level the source keeps ahead of the device.
    fn pump_pending(&mut self, engine: &mut Engine) {
        let Some(device) = self.device.as_mut() else {
            return;
        };
        let (pushed, done) = pump_into(
            &mut self.pending,
            &mut device.producer,
            device.format.channels,
            self.pace_limit_frames,
        );
        self.pushed_frames += pushed;
        if !done.is_empty() {
            engine.recycle(done);
        }
    }

    /// Once the stream is over and everything rendered is in the ring (or
    /// the file): set when the epoch closes, after what is queued has played.
    fn finish_when_played(&mut self, held_target_ms: f64) {
        if !self.finishing || self.ending_at.is_some() || !self.pending.is_empty() {
            return;
        }
        let play_out_s = match &self.device {
            None => 0.0,
            // What the ring holds, at the device's pace.
            Some(device) if self.paced => {
                let level = device.producer.capacity() - device.producer.free();
                level as f64 / device.format.rate as f64 + 0.1
            }
            // What the latency holds.
            Some(_) => held_target_ms / 1e3 + 0.05,
        };
        self.ending_at = Some(Instant::now() + Duration::from_secs_f64(play_out_s));
    }
}

/// What every epoch of a run is opened with.
struct EpochEnv<'a> {
    config: &'a HostConfig,
    controls: &'a Controls,
    /// The file the run writes, when the output is one.
    file: Option<&'a FileOut>,
}

/// What playing a block may need: the run's environment, and the settings
/// and latency in force now (an output is opened when a block says for what).
struct PlayCtx<'a> {
    env: &'a EpochEnv<'a>,
    settings: &'a OutputSettings,
    /// The latency a device starts at: the configured target, or what an
    /// earlier stream showed the pipeline needs.
    target_ms: f64,
}

/// A new epoch. Its output is opened by its first block.
fn open_epoch(kind: EpochKind, env: &EpochEnv<'_>) -> Epoch {
    // The idle feed plays on the device, whatever the run writes.
    let file = env.file.filter(|_| kind == EpochKind::Stream);
    Epoch::new(kind, file.map(Rc::clone))
}

/// Open the device for blocks of `format`: the ring, the output core with
/// its servo, and the PipeWire stream. Returns it with the level a paced
/// source keeps ahead, when the device paces this one.
fn start_device(
    kind: EpochKind,
    engine: &Engine,
    ctx: &PlayCtx<'_>,
    format: BlockFormat,
    tap: &Arc<SourceTap>,
    telemetry: &Arc<SyncTelemetry>,
) -> Result<(Device, Option<usize>)> {
    let (config, settings) = (ctx.env.config, ctx.settings);
    // The device runs at the stream's rate unless another is asked for, in
    // which case the output core converts (its drift resampler does both).
    let device_rate = settings.output_rate_hz.unwrap_or(format.rate);
    let capacity = (RING_SECONDS * format.rate as f64) as usize;
    let (producer, consumer) = frame_ring(format.channels, capacity);
    let quantum = (u64::from(config.quantum) * u64::from(device_rate) / u64::from(QUANTUM_RATE))
        .clamp(64, 8192) as u32;
    let mut core_config = OutputCoreConfig::new(format.channels, format.rate, device_rate, 8192);
    // The device paces a source with no clock: nothing to measure or hold.
    let paced = kind == EpochKind::Stream && config.source.device_paced(settings.source_clock);
    if paced {
        core_config.servo.pacing = audio_sync::Pacing::Device;
    }
    // The idle feed is the host's own clock, read exactly: nothing to follow.
    if kind == EpochKind::Stream && config.source.follows_the_writer(settings.source_clock) {
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
    core_config.servo.target_latency_s = ctx.target_ms / 1e3;
    let core = OutputCore::new(
        core_config,
        consumer,
        Arc::clone(tap),
        Arc::clone(telemetry),
    );
    // The live option says how the device routes the channels: by the
    // position each is tagged with, or one to one in order (positionless
    // names), port N being the layout's speaker N.
    let mapping = engine.output_channel_mapping();
    let names = match mapping {
        OutputChannelMapping::ByName => {
            Some(engine.output_channel_names()).filter(|names| names.len() == format.channels)
        }
        OutputChannelMapping::ByIndex => {
            Some((0..format.channels).map(|i| format!("AUX{i}")).collect())
        }
    };
    log::info!(
        "sync host: output {} Hz x {} ({}), device at {device_rate} Hz, quantum {quantum}",
        format.rate,
        format.channels,
        names.as_ref().map_or("default map".into(), |n| n.join(",")),
    );
    let output = PipewireSyncOutput::start(
        PipewireSyncOutputConfig {
            node_name: "omniphony-sync".into(),
            target: settings.output_target.clone(),
            channels: format.channels as u32,
            positions: names,
            rate: device_rate,
            quantum,
        },
        core,
    )?;
    // What the source keeps ahead of the device: the latency target, as a
    // buffer level (there is no end-to-end latency without a source clock).
    let pace_limit = paced.then(|| (ctx.target_ms / 1e3 * format.rate as f64).round() as usize);
    Ok((
        Device {
            producer,
            output,
            format,
            mapping,
            device_rate,
        },
        pace_limit,
    ))
}

/// Push the blocks in `pending`, oldest first, until the ring holds `limit`
/// frames: a block that does not fit whole goes in by parts, in order, and
/// stays at the front with what is left of it. Returns the frames pushed and
/// the blocks that went in whole, for their buffers to be reused.
fn pump_into(
    pending: &mut VecDeque<(RenderedAudio, usize)>,
    producer: &mut Producer,
    channels: usize,
    limit: usize,
) -> (u64, Vec<RenderedAudio>) {
    let mut pushed_total = 0u64;
    let mut done = Vec::new();
    while let Some((block, offset)) = pending.front_mut() {
        let level = producer.capacity() - producer.free();
        let room = limit.saturating_sub(level);
        if room == 0 {
            break;
        }
        let frames = (block.n_frames - *offset).min(room);
        let (from, to) = (*offset * channels, (*offset + frames) * channels);
        let pushed = producer.push(&block.samples[from..to]);
        *offset += pushed;
        pushed_total += pushed as u64;
        if *offset < block.n_frames {
            break;
        }
        if let Some((block, _)) = pending.pop_front() {
            done.push(block);
        }
    }
    (pushed_total, done)
}

/// The stream is over: render what the decoder still holds, then, for a
/// device, a cycle of silence, so that the last frames are not left in the
/// ring for want of a whole cycle to play them with. The epoch closes once
/// that has played ([`Epoch::finish_when_played`]).
fn finish_stream(engine: &mut Engine, ep: &mut Epoch, ctx: &PlayCtx<'_>) {
    loop {
        match engine.drain() {
            Ok(blocks) if blocks.is_empty() => break,
            Ok(blocks) => push_blocks(engine, ep, ctx, blocks),
            Err(e) => {
                log::debug!("sync host: drain error: {e:#}");
                break;
            }
        }
    }
    if let Some(format) = ep.device.as_ref().map(|d| d.format) {
        let cycle =
            u64::from(ctx.env.config.quantum) * u64::from(format.rate) / u64::from(QUANTUM_RATE);
        let frames = cycle as usize + 64;
        push_blocks(
            engine,
            ep,
            ctx,
            vec![RenderedAudio {
                samples: vec![0.0; frames * format.channels],
                n_channels: format.channels as u32,
                n_frames: frames,
                sample_rate: format.rate,
                sample_pos: 0,
                input_pts_us: None,
            }],
        );
    }
    ep.finishing = true;
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
        sample_rate: engine.render_rate(),
    }
}

/// Render `frames` of silence into the idle epoch and publish the host's own
/// clock as its source: `t` is when the feeder counted them.
fn feed_silence(engine: &mut Engine, ep: &mut Epoch, ctx: &PlayCtx<'_>, t: f64, frames: u32) {
    match engine.render_silence(frames) {
        Ok(Some(block)) => push_blocks(engine, ep, ctx, vec![block]),
        Ok(None) => {}
        Err(e) => log::debug!("sync host: idle render error: {e:#}"),
    }
    if ep.pushed_frames > 0 {
        ep.tap.publish(t, ep.pushed_frames);
    }
}

/// Render one chunk of the sink's linear PCM and push the audio. The clock
/// is published by the caller, from the transport, as for a bitstream.
fn feed_pcm(engine: &mut Engine, ep: &mut Epoch, ctx: &PlayCtx<'_>, tr: Transport, bytes: &[u8]) {
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
        Ok(Some(block)) => push_blocks(engine, ep, ctx, vec![block]),
        Ok(None) => {}
        Err(e) => log::debug!("sync host: PCM render error: {e:#}"),
    }
}

/// Deframe, decode and render one chunk, push the audio, publish the clock.
fn feed_chunk(engine: &mut Engine, ep: &mut Epoch, ctx: &PlayCtx<'_>, t: f64, bytes: &[u8]) {
    let pcm_transport = ep
        .last_transport
        .filter(|tr| tr.format == CarrierFormat::PcmF32);
    if let Some(tr) = pcm_transport {
        feed_pcm(engine, ep, ctx, tr, bytes);
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
            // The first burst is the count's origin, whether or not the
            // carrier it is counted on is known yet.
            ep.transport_base
                .get_or_insert((packet.start_byte, ep.pushed_frames as f64));
            match engine.process(&packet.payload, RInputTransport::Iec61937, packet.data_type) {
                Ok(blocks) => push_blocks(engine, ep, ctx, blocks),
                Err(e) => log::debug!("sync host: decode error: {e:#}"),
            }
        }
    } else {
        match engine.process(bytes, RInputTransport::Raw, 0) {
            Ok(blocks) => push_blocks(engine, ep, ctx, blocks),
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
            // (Nothing to say while the count's scale waits for the stream's
            // rate: nothing has been rendered yet.)
            let bytes = tr.bytes_end.saturating_sub(base_bytes);
            if let Some(frames) = transport_frames(bytes, tr, ep.data_type, ep.stream_rate) {
                let received = base_frames + frames;
                if received > 0.0 {
                    ep.tap.publish(t, received.round() as u64);
                }
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

/// Source frames a byte count on a carrier stands for
/// ([`content_per_carrier_frame`]).
fn transport_frames(
    bytes: u64,
    tr: Transport,
    data_type: Option<u8>,
    stream_rate: Option<u32>,
) -> Option<f64> {
    let frame_bytes = tr.format.sample_bytes() * tr.channels.max(1);
    let carrier_frames = bytes as f64 / frame_bytes as f64;
    Some(carrier_frames * content_per_carrier_frame(tr, data_type, stream_rate)?)
}

/// Play rendered blocks: into the file the run writes, or into the device's
/// ring, opened for the first block's format and again when a block comes in
/// another one.
fn push_blocks(engine: &mut Engine, ep: &mut Epoch, ctx: &PlayCtx<'_>, blocks: Vec<RenderedAudio>) {
    let mut done = Vec::with_capacity(blocks.len());
    for block in blocks {
        let frames = block.n_frames as u64;
        if frames == 0 {
            done.push(block);
            continue;
        }
        let format = BlockFormat {
            rate: block.sample_rate,
            channels: block.n_channels as usize,
        };
        if let Some(file) = &ep.file {
            let written = file
                .borrow_mut()
                .write(format, &block.samples, || engine.output_speaker_positions());
            match written {
                Ok(()) => ep.pushed_frames += frames,
                Err(e) => {
                    ep.dropped_frames += frames;
                    ep.fatal.get_or_insert(e);
                }
            }
            ep.stream_rate = Some(format.rate);
            done.push(block);
            continue;
        }
        if ep.device.as_ref().is_none_or(|d| d.format != format) && !ep.device_failed {
            ep.open_device(engine, ctx, format);
        }
        let Some(device) = ep.device.as_mut().filter(|d| d.format == format) else {
            // No output to play it on (it could not be opened).
            ep.dropped_frames += frames;
            done.push(block);
            continue;
        };
        if ep.paced {
            // A paced source: the block waits its turn, nothing is dropped.
            ep.pending.push_back((block, 0));
            continue;
        }
        let pushed = device.producer.push(&block.samples) as u64;
        ep.pushed_frames += pushed;
        // A full ring: the output stopped draining. Counted.
        ep.dropped_frames += frames - pushed;
        done.push(block);
    }
    engine.recycle(done);
    ep.pump_pending(engine);
}

fn describe(ep: &Epoch, stats: &reader::ReaderStats) -> String {
    use std::sync::atomic::Ordering;
    let s = ep.telemetry.snapshot();
    format!(
        "streaming={} phase={} latency={} err={:+.3}ms floor={:.1}ms src={:+.2}ppm dev={:+.2}ppm corr={:+.2}ppm realigns={} underruns={} rephases={} short={} pushed={} ring_drops={} chunk_drops={} bytes={}",
        ep.device.as_ref().is_some_and(|d| d.output.is_streaming()),
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
    fn block(frames: usize, first: f32) -> RenderedAudio {
        RenderedAudio {
            // Two channels, each sample its own frame index from `first`.
            samples: (0..frames)
                .flat_map(|i| [first + i as f32, first + i as f32])
                .collect(),
            n_channels: 2,
            n_frames: frames,
            sample_rate: 48_000,
            sample_pos: 0,
            input_pts_us: None,
        }
    }

    /// A paced source's blocks go into the ring up to the level it keeps
    /// ahead and no further, in order, by parts when one does not fit, and
    /// the rest follows as the device takes what is there. Nothing is lost.
    #[test]
    fn paced_blocks_fill_the_ring_to_the_limit_and_follow_the_device() {
        let (mut producer, mut consumer) = frame_ring(2, 4096);
        let mut pending: VecDeque<(RenderedAudio, usize)> = VecDeque::new();
        for i in 0..3 {
            pending.push_back((block(300, (i * 300) as f32), 0));
        }
        let (pushed, done) = pump_into(&mut pending, &mut producer, 2, 500);
        assert_eq!(pushed, 500);
        assert_eq!(done.len(), 1, "the first block went in whole");
        assert_eq!(pending.front().map(|(_, offset)| *offset), Some(200));
        // Full to the limit: nothing more goes in.
        assert_eq!(pump_into(&mut pending, &mut producer, 2, 500).0, 0);

        // The device takes 300 frames: 300 more go in.
        let mut played = vec![0.0f32; 300 * 2];
        assert!(consumer.read_exact(&mut played));
        let (pushed, done) = pump_into(&mut pending, &mut producer, 2, 500);
        assert_eq!(pushed, 300);
        assert_eq!(done.len(), 1);
        // And the rest, once there is room for it.
        let mut more = vec![0.0f32; 500 * 2];
        assert!(consumer.read_exact(&mut more));
        played.extend_from_slice(&more);
        let (pushed, done) = pump_into(&mut pending, &mut producer, 2, 500);
        assert_eq!((pushed, done.len()), (100, 1));
        assert!(pending.is_empty());
        let mut last = vec![0.0f32; 100 * 2];
        assert!(consumer.read_exact(&mut last));
        played.extend_from_slice(&last);

        // Every frame, once, in order.
        let frames: Vec<f32> = played.chunks(2).map(|f| f[0]).collect();
        let expected: Vec<f32> = (0..900).map(|i| i as f32).collect();
        assert_eq!(frames, expected);
    }

    /// The device paces a file whatever is asked, a pipe when its writer is
    /// said to have no clock, and never the sink.
    #[test]
    fn the_input_decides_what_a_source_clock_can_be() {
        let file = Source::File { path: "a".into() };
        let pipe = Source::Pipe {
            path: "p".into(),
            drain: true,
        };
        let sink = Source::OwnSink {
            node_name: "omniphony".into(),
            description: "Omniphony".into(),
            session_priority: None,
        };
        for clock in [
            SourceClock::Auto,
            SourceClock::Own,
            SourceClock::Follow,
            SourceClock::None,
        ] {
            assert!(file.device_paced(clock));
            assert!(!sink.device_paced(clock));
            assert!(!file.follows_the_writer(clock) && !sink.follows_the_writer(clock));
            let none = clock == SourceClock::None;
            assert_eq!(pipe.device_paced(clock), none);
            assert_eq!(pipe.follows_the_writer(clock), !none);
        }
    }

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
            output: OutputKind::Device,
            source_clock: SourceClock::Auto,
            continuous: true,
            offline: false,
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

    /// What a second of content takes on its carrier counts as a second of
    /// the stream, at the stream's rate, whatever rate the carrier is said
    /// to run at.
    #[test]
    fn a_carrier_is_counted_in_the_streams_frames() {
        let iec = |rate, channels| Transport {
            rate,
            channels,
            format: CarrierFormat::Iec958,
            bytes_end: 0,
        };
        // Bytes of `frames` carrier frames on `channels` 16-bit channels.
        let bytes = |frames: u64, channels: u64| frames * channels * 2;
        let count = |b, tr, data_type, rate| transport_frames(b, tr, Some(data_type), rate);

        // The known pipe carriers at 48 kHz, as before.
        for data_type in [1u8, 11, 12, 13, 21, 22] {
            let (rate, channels) = pipe_carrier(data_type).unwrap();
            let second = bytes(u64::from(rate), u64::from(channels));
            assert_eq!(
                count(second, iec(rate, channels), data_type, Some(48_000)),
                Some(48_000.0),
                "data type {data_type}"
            );
        }
        // DTS-HD rides 2 or 8 channels: not guessed.
        assert_eq!(pipe_carrier(17), None);

        // AC-3 and DTS at 44.1 kHz: a frame of carrier per sample. E-AC-3:
        // four. Neither needs the stream's rate, nor believes the carrier's.
        assert_eq!(
            count(bytes(44_100, 2), iec(48_000, 2), 1, None),
            Some(44_100.0)
        );
        assert_eq!(
            count(bytes(44_100, 2), iec(44_100, 2), 11, None),
            Some(44_100.0)
        );
        assert_eq!(
            count(bytes(176_400, 2), iec(192_000, 2), 21, None),
            Some(44_100.0)
        );

        // TrueHD: a MAT frame (3840 carrier frames) is 1/200 s of the top
        // rate of the family, so 960 samples at 48 kHz, 1920 at 96 kHz, 3840
        // at 192 kHz, and 960 at 44.1 kHz. Unknown until a block was rendered.
        let mat = bytes(3840, 8);
        assert_eq!(count(mat, iec(192_000, 8), 22, None), None);
        assert_eq!(count(mat, iec(192_000, 8), 22, Some(48_000)), Some(960.0));
        assert_eq!(count(mat, iec(192_000, 8), 22, Some(96_000)), Some(1920.0));
        assert_eq!(count(mat, iec(192_000, 8), 22, Some(192_000)), Some(3840.0));
        assert_eq!(count(mat, iec(192_000, 8), 22, Some(44_100)), Some(960.0));

        // DTS-HD lasts on its carrier what it lasts played.
        let second = bytes(192_000, 8);
        assert_eq!(
            count(second, iec(192_000, 8), 17, Some(96_000)),
            Some(96_000.0)
        );

        // The sink's linear PCM: 8 channels of 4-byte floats at 48 kHz.
        let pcm = Transport {
            rate: own_sink::PCM_RATE_HZ,
            channels: own_sink::PCM_CHANNELS,
            format: CarrierFormat::PcmF32,
            bytes_end: 0,
        };
        assert_eq!(
            transport_frames(48_000 * 8 * 4, pcm, None, Some(48_000)),
            Some(48_000.0)
        );
    }

    /// An output reopened under a running stream starts its ring at 0, and
    /// the source's count moves with it: the frame the new ring starts on is
    /// the one that follows what the old ring was given.
    #[test]
    fn a_reopened_output_keeps_the_streams_count() {
        let mut ep = Epoch::new(EpochKind::Stream, None);
        // 2048 frames of hold before the first block, then 48000 pushed and
        // a block of 300 with 100 already in the ring.
        ep.transport_base = Some((4096, 0.0));
        ep.pushed_frames = 48_000;
        ep.pending.push_back((block(300, 0.0), 100));
        let tap = Arc::clone(&ep.tap);

        ep.restart_count();

        assert_eq!(ep.pushed_frames, 0);
        assert!(ep.pending.is_empty());
        assert_eq!(ep.dropped_frames, 200);
        // 50248 received when 48200 had gone to the old ring: 2048 ahead of
        // the new ring's frame 0, the same hold as before.
        assert_eq!(ep.transport_base, Some((4096, -48_200.0)));
        assert!(!Arc::ptr_eq(&tap, &ep.tap), "the new servo reads a new tap");
    }

    /// A file holds one format: a block in another starts it over, so the
    /// header always describes the samples under it.
    #[test]
    fn a_file_starts_over_on_another_format() {
        let dir = std::env::temp_dir().join(format!("sync-host-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.raw");
        let mut file = FileState {
            destination: path.to_string_lossy().into_owned(),
            container: FileSinkFormat::RawF32,
            open: None,
        };
        let stereo_48 = BlockFormat {
            rate: 48_000,
            channels: 2,
        };
        let stereo_44 = BlockFormat {
            rate: 44_100,
            channels: 2,
        };
        file.write(stereo_48, &[1.0; 8], || None).unwrap();
        file.write(stereo_48, &[1.0; 8], || None).unwrap();
        file.flush();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 16 * 4);
        file.write(stereo_44, &[2.0; 6], || None).unwrap();
        file.flush();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 6 * 4);
        drop(file);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
