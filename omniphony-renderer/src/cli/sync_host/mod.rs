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
//! Hidden while experimental. Linux only for now (PipeWire output).

mod own_sink;
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

use reader::{ReaderMsg, Transport, spawn_reader};

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
    /// `own` source: the transport position (PCM frames) of the first burst
    /// the parser found, which is source frame 0 of this epoch.
    first_burst_frames: Option<f64>,
    last_transport: Option<Transport>,
    /// `follow` source on a pipe: its carrier, once the first burst named
    /// the data type, and the bytes pushed into the parser since its reset.
    pipe_carrier: Option<(u32, u32)>,
    parser_bytes: u64,
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

    let (rx, stats) = match args.source {
        SourceKind::Pipe => {
            let input = args
                .input
                .clone()
                .context("--source pipe needs an INPUT pipe")?;
            spawn_reader(input, !args.no_drain_pipe)?
        }
        SourceKind::Own => own_sink::spawn_own_sink(own_sink::OwnSinkConfig {
            node_name: args.sink_name.clone(),
            description: "Omniphony (sync)".into(),
            latency_ns: (args.target_latency_ms * 1e6) as i64,
            quantum: args.quantum,
            session_priority: args.sink_session_priority,
        })?,
    };
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
            Some(ReaderMsg::Chunk {
                t,
                bytes,
                transport,
            }) => {
                if let Some(ep) = epoch.as_mut() {
                    ep.last_transport = transport;
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
    if args.source == SourceKind::Pipe {
        let follow = audio_sync::ServoConfig::follow();
        config.servo = audio_sync::ServoConfig {
            source_rate_hz: config.servo.source_rate_hz,
            output_rate_hz: config.servo.output_rate_hz,
            resampler_lookahead_frames: config.servo.resampler_lookahead_frames,
            ..follow
        };
    }
    if let Some(hz) = args.loop_bandwidth_hz {
        config.servo.loop_bandwidth_hz = hz;
    }
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
        first_burst_frames: None,
        last_transport: None,
        pipe_carrier: None,
        parser_bytes: 0,
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
        ep.parser_bytes = 0;
    }
    if ep.is_spdif == Some(true) {
        ep.parser.push_bytes(bytes);
        ep.parser_bytes += bytes.len() as u64;
        while let Some(packet) = ep.parser.get_next_packet() {
            ep.hold = decoder_hold_frames(packet.data_type);
            if ep.last_transport.is_none() && ep.pipe_carrier.is_none() {
                ep.pipe_carrier = pipe_carrier(packet.data_type);
                if ep.pipe_carrier.is_none() {
                    log::info!(
                        "sync host: carrier of IEC 61937 data type {} unknown, \
                         counting decoded frames",
                        packet.data_type
                    );
                }
            }
            if ep.first_burst_frames.is_none()
                && let Some(tr) = transport(ep)
            {
                ep.first_burst_frames = Some(transport_frames(packet.start_byte, tr));
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
    match (transport(ep), ep.first_burst_frames) {
        // The exact transport position, counted from the first burst: from
        // the `own` sink, or from the bytes of a pipe whose carrier is known.
        // The decoder's hold and batching are inside the measurement, as
        // they are inside the latency. On a pipe this keeps the readings
        // free of the decoder's burst granularity, which would otherwise
        // show as up to a chunk of arrival lateness.
        (Some(tr), Some(first)) => {
            let received = transport_frames(tr.bytes_end, tr) - first;
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
    ep.bytes_in += bytes.len() as u64;
    if ep.trace {
        eprintln!("SYNC_TRACE {t:.6} {} {}", ep.bytes_in, ep.pushed_frames);
    }
}

/// Where the bytes fed so far end on the carrier's timeline, if known.
fn transport(ep: &Epoch) -> Option<Transport> {
    ep.last_transport.or_else(|| {
        ep.pipe_carrier.map(|(rate, channels)| Transport {
            rate,
            channels,
            bytes_end: ep.parser_bytes,
        })
    })
}

/// Source PCM frames (at the render rate) a byte offset on an IEC 958
/// carrier stands for: carriers run at the content rate times a fixed factor
/// (1 for AC-3/DTS at 48 kHz, 4 for E-AC-3/TrueHD/DTS-HD at 192 kHz).
fn transport_frames(bytes: u64, tr: Transport) -> f64 {
    let carrier_frames = bytes as f64 / (2.0 * tr.channels.max(1) as f64);
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
    #[test]
    fn pipe_carriers_count_one_second_per_second() {
        for data_type in [1u8, 11, 12, 13, 21, 22] {
            let (rate, channels) = pipe_carrier(data_type).unwrap();
            let tr = Transport {
                rate,
                channels,
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
    }
}
