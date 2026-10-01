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
//! `--ao=pcm`, which paces audio on its video clock). The source clock is the
//! arrival of the bytes; the output resamples to hold a fixed end-to-end
//! latency against it. Each stream (pipe open → EOF) is an epoch with its own
//! ring and output, so nothing carries over between files.
//!
//! The frames counted in (`N_in`) are the frames pushed into the ring, plus
//! the codec's constant decoder hold, at the arrival time of the bytes that
//! completed them. That is exact in rate; in absolute latency it is exact up
//! to the hold table (spike S4), which slice 4d replaces with transport-time
//! counting.
//!
//! Hidden while experimental. Linux only for now (PipeWire output).

mod reader;

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
use spdif::SpdifParser;

use reader::{ReaderMsg, spawn_reader};

/// `orender sync-play` arguments.
#[derive(Debug, Clone, Args)]
pub struct SyncPlayArgs {
    /// Input pipe or file (IEC 61937 or raw bitstream).
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// End-to-end latency to hold (ms).
    #[arg(long = "target-latency-ms", default_value_t = 150.0)]
    pub target_latency_ms: f64,

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

/// One stream's playback: ring, clock tap and output stream.
struct Epoch {
    producer: Producer,
    tap: Arc<SourceTap>,
    telemetry: Arc<SyncTelemetry>,
    output: PipewireSyncOutput,
    hold: u64,
    parser: SpdifParser,
    is_spdif: Option<bool>,
    pushed_frames: u64,
    dropped_frames: u64,
    started: Instant,
    /// Set at end of stream: the epoch is dropped once what it queued has
    /// played. The engine thread never waits for it.
    ending_at: Option<Instant>,
    channel_mismatch_logged: bool,
    /// `ORENDER_SYNC_TRACE`: one stderr line per chunk (arrival time, bytes
    /// so far, frames pushed so far), for studying a source's delivery.
    trace: bool,
    bytes_in: u64,
}

pub fn cmd_sync_play(args: &SyncPlayArgs, config_path: Option<PathBuf>) -> Result<()> {
    let config_path = config_path.or_else(renderer::config::default_config_path);
    let config = config_path
        .as_deref()
        .map(renderer::config::Config::load_or_default)
        .unwrap_or_default();
    let render = config.render.clone().unwrap_or_default();
    let bridge_path = render.bridge_path.clone();
    let mut engine = Engine::from_paths(
        config_path.as_deref(),
        None,
        bridge_path.as_deref(),
        None,
        RENDER_RATE,
    )
    .context("building the render engine")?;
    let channels = engine.channel_count() as usize;
    let positions: Vec<String> = engine
        .channel_layout()
        .into_iter()
        .map(|l| bridge_api::labels::canonical_name(l).to_string())
        .collect();
    let target = args
        .output_target
        .clone()
        .or_else(|| render.output_device.clone());
    log::info!(
        "sync host: {channels} channels ({}), target {} ms, output {}",
        positions.join(","),
        args.target_latency_ms,
        target.as_deref().unwrap_or("<session default>")
    );

    let (rx, stats) = spawn_reader(args.input.clone(), !args.no_drain_pipe)?;
    let started = Instant::now();
    let mut epoch: Option<Epoch> = None;
    let mut next_report = args.report_every;

    loop {
        if let Some(limit) = args.seconds
            && started.elapsed().as_secs_f64() >= limit
        {
            break;
        }
        if epoch
            .as_ref()
            .and_then(|ep| ep.ending_at)
            .is_some_and(|at| Instant::now() >= at)
            && let Some(ep) = epoch.take()
        {
            log::info!(
                "sync host: stream ended after {:.1} s; {}",
                ep.started.elapsed().as_secs_f64(),
                describe(&ep, &stats)
            );
        }
        let msg = match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(m) => Some(m),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        match msg {
            Some(ReaderMsg::Start) => {
                // A new stream replaces whatever was still playing out.
                drop(epoch.take());
                engine.reset();
                epoch = Some(start_epoch(args, channels, &positions, target.clone())?);
                log::info!("sync host: stream opened, new epoch");
            }
            Some(ReaderMsg::Chunk { t, bytes }) => {
                if let Some(ep) = epoch.as_mut() {
                    feed_chunk(&mut engine, ep, t, &bytes);
                }
            }
            Some(ReaderMsg::End) => {
                if let Some(ep) = epoch.take() {
                    // Let what is queued play out, then close the stream.
                    std::thread::sleep(Duration::from_secs_f64(
                        args.target_latency_ms / 1e3 + 0.05,
                    ));
                    log::info!(
                        "sync host: stream ended after {:.1} s; {}",
                        ep.started.elapsed().as_secs_f64(),
                        describe(&ep, &stats)
                    );
                }
            }
            None => {}
        }
        if args.report_every > 0.0 && started.elapsed().as_secs_f64() >= next_report {
            next_report += args.report_every;
            if let Some(ep) = epoch.as_ref() {
                println!(
                    "t={:7.1}s {}",
                    started.elapsed().as_secs_f64(),
                    describe(ep, &stats)
                );
            }
        }
    }
    Ok(())
}

fn start_epoch(
    args: &SyncPlayArgs,
    channels: usize,
    positions: &[String],
    target: Option<String>,
) -> Result<Epoch> {
    let capacity = (RING_SECONDS * RENDER_RATE as f64) as usize;
    let (producer, consumer) = frame_ring(channels, capacity);
    let tap = Arc::new(SourceTap::new());
    let telemetry = Arc::new(SyncTelemetry::new());
    let mut config = OutputCoreConfig::new(channels, RENDER_RATE, RENDER_RATE, 8192);
    let follow = audio_sync::ServoConfig::follow();
    config.servo.loop_bandwidth_hz = args.loop_bandwidth_hz.unwrap_or(follow.loop_bandwidth_hz);
    config.servo.target_latency_s = args.target_latency_ms / 1e3;
    let core = OutputCore::new(config, consumer, Arc::clone(&tap), Arc::clone(&telemetry));
    let output = PipewireSyncOutput::start(
        PipewireSyncOutputConfig {
            node_name: "omniphony-sync".into(),
            target,
            channels: channels as u32,
            positions: Some(positions.to_vec()),
            rate: RENDER_RATE,
            quantum: args.quantum,
        },
        core,
    )?;
    Ok(Epoch {
        producer,
        tap,
        telemetry,
        output,
        hold: 0,
        parser: SpdifParser::new(),
        is_spdif: None,
        pushed_frames: 0,
        dropped_frames: 0,
        started: Instant::now(),
        ending_at: None,
        channel_mismatch_logged: false,
        trace: std::env::var_os("ORENDER_SYNC_TRACE").is_some(),
        bytes_in: 0,
    })
}

/// Deframe, decode and render one chunk, push the audio, publish the clock.
fn feed_chunk(engine: &mut Engine, ep: &mut Epoch, t: f64, bytes: &[u8]) {
    let has_sync = spdif::contains_sync(bytes);
    if ep.is_spdif.is_none() && bytes.len() >= 4 {
        ep.is_spdif = Some(has_sync);
    } else if ep.is_spdif == Some(false) && has_sync {
        ep.is_spdif = Some(true);
        ep.parser.reset();
    }
    if ep.is_spdif == Some(true) {
        ep.parser.push_bytes(bytes);
        while let Some(packet) = ep.parser.get_next_packet() {
            ep.hold = decoder_hold_frames(packet.data_type);
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
    if ep.pushed_frames > 0 {
        ep.tap.publish(t, ep.pushed_frames + ep.hold);
    }
    ep.bytes_in += bytes.len() as u64;
    if ep.trace {
        eprintln!("SYNC_TRACE {t:.6} {} {}", ep.bytes_in, ep.pushed_frames);
    }
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
        "streaming={} phase={} latency={} err={:+.3}ms floor={:.1}ms src={:+.2}ppm dev={:+.2}ppm corr={:+.2}ppm realigns={} underruns={} short={} pushed={} ring_drops={} chunk_drops={} bytes={}",
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
        s.short_reads,
        ep.pushed_frames,
        ep.dropped_frames,
        stats.chunks_dropped.load(Ordering::Relaxed),
        stats.bytes_read.load(Ordering::Relaxed),
    )
}
