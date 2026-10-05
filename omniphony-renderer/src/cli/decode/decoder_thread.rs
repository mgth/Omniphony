use anyhow::Result;
use bridge_api::{FormatBridgeBox, RInputTransport};
use orender_engine::decode_step::{
    DeclarationTracker, DecodedPacket, DrcModeSync, LogLevelSync, decode_packet,
};
use spdif::SpdifParser;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock, mpsc};
use std::thread;
use std::time::Instant;
use sys::InputReader;

/// Returns the current CLOCK_MONOTONIC timestamp in microseconds.
/// Used for systemd RELOADING=1 notifications (MONOTONIC_USEC is required by
/// systemd ≥ 253). Returns 0 on platforms where the clock is unavailable.
fn monotonic_usec_now() -> u64 {
    #[cfg(unix)]
    unsafe {
        let mut ts: libc::timespec = std::mem::zeroed();
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
        (ts.tv_sec as u64)
            .saturating_mul(1_000_000)
            .saturating_add(ts.tv_nsec as u64 / 1_000)
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// Messages sent from asynchronous input producers to the handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodedSource {
    Bridge,
    Live,
}

pub struct DecodedAudioData {
    pub source: DecodedSource,
    pub frame: bridge_api::RDecodedFrame,
    /// The bridge's declaration, sent with the frame a
    /// [`DeclarationTracker`] says needs it (the bridge lives on the decoding
    /// thread, this one or the PipeWire sink's; the handler keeps the last
    /// value it received). Never set on the sink's plain PCM: the handler
    /// declares that itself when the input switches to it
    /// (`SpatialState::take_declaration`).
    pub declaration: Option<Declaration>,
    pub decode_time_ms: f32,
    pub sent_at: Instant,
}

pub use orender_engine::decode_step::Declaration;

pub enum DecoderMessage {
    /// A fully decoded audio frame (PCM + metadata + dialogue level).
    AudioData(DecodedAudioData),
    /// The bridge reset itself (sync loss, seek) before the frames that
    /// follow: the spatial state starts over, as in the embedded engine. Not
    /// a flush — the audio buffers are left alone.
    BridgeReset(DecodedSource),
    /// Stream ended — reset handler state (for continuous mode).
    StreamEnd(DecodedSource),
}

#[derive(Clone)]
pub struct PipeInputDiag {
    pub chunk_bytes: Arc<AtomicU64>,
    pub chunk_dt_us: Arc<AtomicU64>,
    pub audio_ms_per_chunk: Arc<AtomicU64>,
    pub gap_over_audio_ms: Arc<AtomicU64>,
}

pub struct DecoderThreadConfig {
    pub input_path: std::path::PathBuf,
    pub continuous: bool,
    pub drain_pipe: bool,
    pub tx: mpsc::SyncSender<Result<DecoderMessage>>,
    /// The DRC mode the handler asks for, shared with the PipeWire sink's
    /// bridge decoder. Seeded with the configured mode before the thread
    /// starts, so the bridge decodes the first packet in that mode; a later
    /// change reaches it before the next input chunk.
    pub requested_drc_mode: Arc<RwLock<String>>,
    /// Post-rendering output pacer drain clock (the token clock), for the
    /// frames this thread decodes, in pure pipe mode and beside a PipeWire
    /// capture alike; it stands down while a capture stream delivers. Each
    /// decoded packet posts its emitted source duration (microseconds) here,
    /// before the (potentially blocking) frame send. An independent drain
    /// thread converts that to output frames and drains the pacer FIFO into
    /// the ring — keeping the drain off this thread avoids the backpressure
    /// deadlock (send blocks → FIFO never drains → send stays blocked).
    /// `None` when output pacing is unused.
    pub drain_tx: Option<mpsc::Sender<u64>>,
    /// Optional diag handles for the named-pipe / stdin input path. Published
    /// from the decoder thread so we can correlate upstream delivery cadence
    /// with downstream latency sawtooths.
    pub pipe_input_diag: Option<PipeInputDiag>,
    /// The bridge owns the complete decode pipeline.
    pub bridge: FormatBridgeBox,
    /// The log level `bridge` was opened with (`LoadedBridge::log_level`).
    pub log_level: LogLevelSync,
    /// Platform-agnostic shutdown signal for interrupt-aware I/O.
    pub shutdown_signal: sys::ShutdownSignal,
}

pub fn spawn_decoder_thread(config: DecoderThreadConfig) -> thread::JoinHandle<Result<()>> {
    thread::spawn(move || -> Result<()> {
        let DecoderThreadConfig {
            input_path,
            continuous,
            drain_pipe,
            tx,
            requested_drc_mode,
            drain_tx,
            pipe_input_diag,
            mut bridge,
            mut log_level,
            shutdown_signal,
        } = config;

        let mut frame_count: u64 = 0;
        let mut drc_mode = DrcModeSync::new();
        // When a frame carries the bridge's declaration: the same rule as the
        // embedded engine and the PipeWire sink's bridge decoder.
        let mut declarations = DeclarationTracker::new();
        loop {
            // Check for shutdown — do not restart after SIGTERM/SIGINT.
            if sys::ShutdownHandle::is_requested() {
                log::info!("Shutdown requested, stopping decoder loop");
                break;
            }

            if sys::ShutdownHandle::is_restart_from_config_requested() {
                log::info!("Restart from config requested, stopping decoder loop");
                break;
            }

            // Check for SIGHUP reload — clear the flag and notify systemd
            // before reopening the input. The stream restarts naturally by
            // continuing the loop (StreamEnd will be sent at the bottom).
            if sys::ShutdownHandle::is_reload_requested() {
                sys::ShutdownHandle::clear_reload();
                log::info!("SIGHUP received, reloading stream...");
                sys::notify_reloading(monotonic_usec_now());
            }

            let mut input_reader = match InputReader::new(&input_path, drain_pipe) {
                Ok(reader) => reader,
                Err(err) => {
                    let interrupted = err
                        .downcast_ref::<io::Error>()
                        .is_some_and(|io_err| io_err.kind() == io::ErrorKind::Interrupted);
                    if interrupted && sys::ShutdownHandle::is_requested() {
                        log::info!("Shutdown requested while waiting for input connection");
                        break;
                    }
                    return Err(err);
                }
            };
            let is_pipe_input = input_reader.is_pipe();

            // S/PDIF demux state — fresh per stream, naturally reset on restart.
            let mut is_spdif: Option<bool> = None;
            let mut spdif_parser = SpdifParser::new();
            let mut last_chunk_at: Option<Instant> = None;

            // Throughput diagnostics: log input rate once per second to diagnose
            // below-real-time delivery (e.g. mpv ao=pcm on Windows).
            let mut throughput_window_start = Instant::now();
            let mut throughput_bytes: u64 = 0;
            let mut throughput_audio_ms: f64 = 0.0;
            let mut throughput_chunks: u64 = 0;
            let session_throughput_started_at = Instant::now();
            let mut session_throughput_bytes: u64 = 0;
            let mut session_throughput_audio_ms: f64 = 0.0;
            let mut session_throughput_chunks: u64 = 0;

            let mut process_chunk = |chunk: &[u8]| -> Result<bool> {
                // Secondary check: interrupt the current stream on shutdown or reload.
                if sys::ShutdownHandle::is_requested()
                    || sys::ShutdownHandle::is_reload_requested()
                    || sys::ShutdownHandle::is_restart_from_config_requested()
                {
                    return Ok(false);
                }

                // Before any packet of this chunk: the first time, the mode
                // the thread was spawned with; then on changes only.
                {
                    let requested = requested_drc_mode.read().unwrap_or_else(|e| e.into_inner());
                    drc_mode.apply(&requested, &mut bridge);
                }
                // The bridge's diagnostics follow `log_level` changes made over OSC.
                log_level.apply(live_log::current_runtime_level(), &mut bridge);

                let now = Instant::now();
                let chunk_gap_ms = last_chunk_at
                    .map(|last| now.saturating_duration_since(last).as_secs_f64() * 1000.0);
                let chunk_dt_us = last_chunk_at
                    .map(|last| now.saturating_duration_since(last).as_micros() as u64)
                    .unwrap_or(0);
                if continuous && is_pipe_input {
                    last_chunk_at = Some(now);
                }

                let chunk_contains_spdif_sync = spdif::contains_sync(chunk);

                // Detect transport format on the first chunk. Do not require the
                // syncword to be at offset 0: named pipes can reconnect or resume
                // mid-burst, and the parser can resynchronise from the next marker.
                if is_spdif.is_none() && chunk.len() >= 4 {
                    if chunk_contains_spdif_sync {
                        is_spdif = Some(true);
                        log::info!("Detected S/PDIF encapsulated stream");
                    } else {
                        is_spdif = Some(false);
                        log::info!("Detected raw stream");
                    }
                } else if is_spdif == Some(false) && chunk_contains_spdif_sync {
                    log::warn!(
                        "Recovered S/PDIF sync after raw detection; switching parser back to IEC61937 mode"
                    );
                    is_spdif = Some(true);
                    spdif_parser.reset();
                }

                // Collect input units: unwrapped IEC 61937 packets or the raw chunk.
                let packets: Vec<(RInputTransport, u8, Vec<u8>)> = if is_spdif.unwrap_or(false) {
                    spdif_parser.push_bytes(chunk);
                    let mut out = Vec::new();
                    while let Some(packet) = spdif_parser.get_next_packet() {
                        out.push((RInputTransport::Iec61937, packet.data_type, packet.payload));
                    }
                    out
                } else {
                    vec![(RInputTransport::Raw, 0, chunk.to_vec())]
                };

                let mut frames_emitted = 0usize;
                // Audio emitted by this chunk, each frame at its own rate
                // (`DecodedPacket::duration_secs`).
                let mut emitted_duration_ms = 0.0f64;
                let packet_count = packets.len();
                for (transport, data_type, payload) in packets {
                    let packet = decode_packet(
                        &mut bridge,
                        &payload,
                        transport,
                        data_type,
                        &mut declarations,
                    );
                    let per_frame_decode_time_ms = packet.decode_ms_per_frame();
                    let packet_emitted_ms = packet.duration_secs() * 1000.0;
                    let DecodedPacket {
                        result,
                        mut declaration,
                        declaration_frame,
                        ..
                    } = packet;
                    let payload_len = payload.len();
                    let emitted_frames = result.frames.len();
                    let emitted_samples: u32 =
                        result.frames.iter().map(|frame| frame.sample_count).sum();
                    emitted_duration_ms += packet_emitted_ms;
                    let metadata_frames = result
                        .frames
                        .iter()
                        .filter(|frame| !frame.metadata.is_empty())
                        .count();
                    let metadata_payloads: usize =
                        result.frames.iter().map(|frame| frame.metadata.len()).sum();
                    let new_segment_frames = result
                        .frames
                        .iter()
                        .filter(|frame| frame.is_new_segment)
                        .count();
                    if matches!(transport, RInputTransport::Iec61937) {
                        let should_warn = result.did_reset
                            || !result.error_message.is_empty()
                            || (metadata_frames > 0 && emitted_frames == 0)
                            || new_segment_frames > 0;
                        if should_warn {
                            let sample_count_min =
                                result.frames.iter().map(|frame| frame.sample_count).min();
                            let sample_count_max =
                                result.frames.iter().map(|frame| frame.sample_count).max();
                            let metadata_summary = result
                                .frames
                                .iter()
                                .flat_map(|frame| frame.metadata.iter())
                                .map(|meta| {
                                    let event_count = meta.events.len();
                                    let min_event_sample_pos =
                                        meta.events.iter().map(|event| event.sample_pos).min();
                                    let max_event_sample_pos =
                                        meta.events.iter().map(|event| event.sample_pos).max();
                                    let min_event_id =
                                        meta.events.iter().map(|event| event.id).min();
                                    let max_event_id =
                                        meta.events.iter().map(|event| event.id).max();
                                    format!(
                                        "meta[pos={} ramp={} events={} ev_pos={:?}..{:?} ev_id={:?}..{:?}]",
                                        meta.sample_pos,
                                        meta.ramp_duration,
                                        event_count,
                                        min_event_sample_pos,
                                        max_event_sample_pos,
                                        min_event_id,
                                        max_event_id
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join(" ");
                            live_log::emit_external_record(
                                log::Level::Warn,
                                "orender::bridge",
                                &format!(
                                    "Bridge packet result: payload_bytes={} data_type=0x{:02X} frames={} samples={} sample_count_range={:?}..{:?} metadata_frames={} metadata_payloads={} new_segment_frames={} did_reset={} error={} {}",
                                    payload_len,
                                    data_type,
                                    emitted_frames,
                                    emitted_samples,
                                    sample_count_min,
                                    sample_count_max,
                                    metadata_frames,
                                    metadata_payloads,
                                    new_segment_frames,
                                    result.did_reset,
                                    result.error_message,
                                    metadata_summary
                                ),
                            );
                        }
                    }

                    if result.did_reset {
                        // Keep audio running through transient decoder resets — never
                        // abort or flush. Flushing would turn a recoverable bridge reset
                        // into an audible dropout much longer than the actual decode
                        // hiccup; in live rendering we never want a premature stop.
                        // Only the spatial state starts over (stale objects, ramps).
                        log::debug!("Bridge reset; keeping audio buffers intact");
                        if tx
                            .send(Ok(DecoderMessage::BridgeReset(DecodedSource::Bridge)))
                            .is_err()
                        {
                            return Ok(false);
                        }
                    }

                    let frames_in_packet = result.frames.len();
                    frames_emitted += frames_in_packet;
                    // Drive the output-pacer drain at the source clock: post this
                    // packet's emitted audio duration BEFORE sending its frames,
                    // so the drain keeps relieving the pacer FIFO even if the
                    // frame send below blocks on handler backpressure.
                    if let Some(ref drain_tx) = drain_tx {
                        if packet_emitted_ms > 0.0 {
                            let _ = drain_tx.send((packet_emitted_ms * 1000.0).round() as u64);
                        }
                    }
                    for (i, frame) in result.frames.into_iter().enumerate() {
                        frame_count += 1;
                        let declaration = if i == declaration_frame {
                            declaration.take()
                        } else {
                            None
                        };
                        let sent_at = Instant::now();
                        if tx
                            .send(Ok(DecoderMessage::AudioData(DecodedAudioData {
                                source: DecodedSource::Bridge,
                                frame,
                                declaration,
                                decode_time_ms: per_frame_decode_time_ms,
                                sent_at,
                            })))
                            .is_err()
                        {
                            return Ok(false);
                        }
                        let send_block_ms = sent_at.elapsed().as_secs_f64() * 1000.0;
                        // A stall on a pipe means the handler fell behind a
                        // live source. A regular file is read as fast as the
                        // handler takes frames, so blocking here is the
                        // expected pacing, not a fault: keep it out of the log.
                        if send_block_ms > 5.0 && is_pipe_input {
                            log::warn!(
                                "Decoder channel backpressure: send_block_ms={:.3} frames_in_packet={} payload_bytes={} transport={:?}",
                                send_block_ms,
                                frames_in_packet,
                                payload_len,
                                transport
                            );
                        }
                    }
                }

                if let Some(diag) = pipe_input_diag.as_ref() {
                    diag.chunk_bytes
                        .store((chunk.len() as f64).to_bits(), Ordering::Relaxed);
                    diag.chunk_dt_us
                        .store((chunk_dt_us as f64).to_bits(), Ordering::Relaxed);
                    diag.audio_ms_per_chunk
                        .store(emitted_duration_ms.to_bits(), Ordering::Relaxed);
                    let gap_over_audio_ms = chunk_gap_ms
                        .map(|gap_ms| (gap_ms - emitted_duration_ms).max(0.0))
                        .unwrap_or(0.0);
                    diag.gap_over_audio_ms
                        .store(gap_over_audio_ms.to_bits(), Ordering::Relaxed);
                }

                if let Some(gap_ms) = chunk_gap_ms.filter(|gap_ms| *gap_ms > 10.0) {
                    let gap_over_emitted_ms = (gap_ms - emitted_duration_ms).max(0.0);
                    let session_elapsed_secs =
                        session_throughput_started_at.elapsed().as_secs_f64();
                    let session_rate = if session_elapsed_secs > 0.0 {
                        session_throughput_audio_ms / (session_elapsed_secs * 1000.0)
                    } else {
                        0.0
                    };
                    let pathological_gap =
                        gap_over_emitted_ms >= 200.0 || gap_ms >= 300.0 || frames_emitted == 0;
                    let sustained_input_deficit =
                        session_elapsed_secs >= 5.0 && session_rate < 0.98;
                    if pathological_gap && sustained_input_deficit {
                        live_log::emit_external_record(
                            log::Level::Warn,
                            "orender::cli::decode::decoder_thread",
                            &format!(
                                "Decoder input chunk gap: gap_ms={:.3} chunk_bytes={} packets={} emitted_frames={} emitted_ms={:.3} gap_over_emitted_ms={:.3} spdif={}",
                                gap_ms,
                                chunk.len(),
                                packet_count,
                                frames_emitted,
                                emitted_duration_ms,
                                gap_over_emitted_ms,
                                is_spdif.unwrap_or(false)
                            ),
                        );
                    }
                }

                // Accumulate throughput stats and log once per second.
                throughput_bytes += chunk.len() as u64;
                throughput_audio_ms += emitted_duration_ms;
                throughput_chunks += 1;
                session_throughput_bytes += chunk.len() as u64;
                session_throughput_audio_ms += emitted_duration_ms;
                session_throughput_chunks += 1;
                let elapsed_secs = throughput_window_start.elapsed().as_secs_f64();
                if elapsed_secs >= 1.0 {
                    let window_rate = if elapsed_secs > 0.0 {
                        throughput_audio_ms / (elapsed_secs * 1000.0)
                    } else {
                        0.0
                    };
                    let session_elapsed_secs =
                        session_throughput_started_at.elapsed().as_secs_f64();
                    let session_rate = if session_elapsed_secs > 0.0 {
                        session_throughput_audio_ms / (session_elapsed_secs * 1000.0)
                    } else {
                        0.0
                    };
                    let session_audio_balance_ms =
                        session_throughput_audio_ms - session_elapsed_secs * 1000.0;
                    let throughput_level = if window_rate < 0.95 || window_rate > 1.05 {
                        log::Level::Warn
                    } else {
                        log::Level::Trace
                    };
                    live_log::emit_external_record(
                        throughput_level,
                        "orender::cli::decode::decoder_thread",
                        &format!(
                            "Input throughput: window_bytes_per_s={:.0} window_audio_ms={:.0} window_wall_ms={:.0} window_rate={:.3}x total_audio_ms={:.0} total_wall_ms={:.0} total_rate={:.3}x total_balance_ms={:+.0} window_chunks={} total_chunks={}",
                            throughput_bytes as f64 / elapsed_secs,
                            throughput_audio_ms,
                            elapsed_secs * 1000.0,
                            window_rate,
                            session_throughput_audio_ms,
                            session_elapsed_secs * 1000.0,
                            session_rate,
                            session_audio_balance_ms,
                            throughput_chunks,
                            session_throughput_chunks,
                        ),
                    );
                    throughput_window_start = Instant::now();
                    throughput_bytes = 0;
                    throughput_audio_ms = 0.0;
                    throughput_chunks = 0;
                }

                Ok(true)
            };

            // Use interrupt-aware I/O so shutdown signals are detected promptly
            // even when the read is blocked waiting for data on a pipe.
            input_reader.process_chunks_with_shutdown(
                64 * 1024,
                &shutdown_signal,
                &mut process_chunk,
            )?;

            log::info!("Processing complete: {frame_count} frames");

            if !continuous {
                break;
            }

            // In continuous mode, check for shutdown before restarting.
            if sys::ShutdownHandle::is_requested() {
                log::info!("Shutdown requested in continuous mode, not restarting stream");
                break;
            }

            // In continuous mode, send StreamEnd message to reset handler state.
            log::info!("Continuous mode: stream ended, signaling handler to reset...");
            if tx
                .send(Ok(DecoderMessage::StreamEnd(DecodedSource::Bridge)))
                .is_err()
            {
                log::warn!("Failed to send StreamEnd message, receiver closed");
                break;
            }

            // Reset bridge for next stream.
            log::info!("Continuous mode: resetting bridge and waiting for new data...");
            bridge.reset();
            declarations.forget();

            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        Ok(())
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use abi_stable::sabi_trait::prelude::TD_Opaque;
    use abi_stable::std_types::{RSlice, RStr, RString, RVec};
    use bridge_api::*;
    use std::sync::Mutex;

    /// Records the DRC mode it is in at each `push_packet`; starts in "Off",
    /// like the real bridges.
    struct DrcRecordingBridge {
        mode: String,
        modes_at_push: Arc<Mutex<Vec<String>>>,
    }

    impl FormatBridge for DrcRecordingBridge {
        fn push_packet(&mut self, _: RSlice<'_, u8>, _: RInputTransport, _: u8) -> RPushResult {
            self.modes_at_push.lock().unwrap().push(self.mode.clone());
            RPushResult {
                frames: RVec::new(),
                error_message: RString::new(),
                did_reset: false,
            }
        }
        fn reset(&mut self) {}
        fn is_ready(&self) -> bool {
            true
        }
        fn has_objects(&self) -> bool {
            false
        }
        fn configure(&mut self, _: RStr<'_>, _: RStr<'_>) -> bool {
            true
        }
        fn coordinate_format(&self) -> RCoordinateFormat {
            RCoordinateFormat::Cartesian
        }
        fn vbap_cartesian_defaults(&self) -> RVbapCartesianDefaults {
            RVbapCartesianDefaults {
                x_size: 3,
                y_size: 3,
                z_size: 3,
                allow_negative_z: false,
            }
        }
        fn preferred_vbap_table_mode(&self) -> RVbapTableMode {
            RVbapTableMode::Cartesian
        }
        fn supported_drc_modes(&self) -> RVec<RString> {
            RVec::new()
        }
        fn set_drc_mode(&mut self, mode: RStr<'_>) -> bool {
            self.mode = mode.as_str().to_owned();
            true
        }
        fn fixed_channel_poses(&self) -> RVec<RChannelPose> {
            RVec::new()
        }
    }

    /// The configured DRC mode is the bridge's from the very first packet,
    /// with nothing sent after the spawn. It used to be a command the handler
    /// sent once the renderer was built, which reached the bridge after however
    /// many packets the thread had decoded by then: with `drc_mode: standard`,
    /// the same file rendered differently from run to run.
    #[test]
    fn the_first_packet_is_decoded_in_the_configured_drc_mode() {
        let input =
            std::env::temp_dir().join(format!("orender-decoder-drc-{}.raw", std::process::id()));
        std::fs::write(&input, [0u8; 16]).unwrap();
        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let modes_at_push = Arc::default();
        let (tx, _rx) = mpsc::sync_channel(16);
        let result = spawn_decoder_thread(DecoderThreadConfig {
            input_path: input.clone(),
            continuous: false,
            drain_pipe: false,
            tx,
            requested_drc_mode: Arc::new(RwLock::new("Standard".to_owned())),
            drain_tx: None,
            pipe_input_diag: None,
            bridge: FormatBridge_TO::from_value(
                DrcRecordingBridge {
                    mode: "Off".to_owned(),
                    modes_at_push: Arc::clone(&modes_at_push),
                },
                TD_Opaque,
            ),
            log_level: LogLevelSync::new(),
            shutdown_signal: sys::ShutdownSignal { fd: fds[0] },
        })
        .join()
        .unwrap();
        let _ = std::fs::remove_file(&input);
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
        result.unwrap();
        assert_eq!(*modes_at_push.lock().unwrap(), ["Standard"]);
    }
}
