use anyhow::Result;
use bridge_api::{FormatBridgeBox, RInputTransport};
use spdif::SpdifParser;
use std::io;
use std::sync::mpsc;
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

/// Messages sent from decoder thread to handler
pub struct DecodedAudioData {
    pub frame: bridge_api::RDecodedFrame,
    pub decode_time_ms: f32,
    pub sent_at: Instant,
}

pub enum DecoderMessage {
    /// A fully decoded audio frame (PCM + metadata + dialogue level).
    AudioData(DecodedAudioData),
    /// Request to flush audio buffers (after seek/decoder reset).
    FlushRequest,
    /// Stream ended — reset handler state (for continuous mode).
    StreamEnd,
}

pub struct DecoderThreadConfig {
    pub input_path: std::path::PathBuf,
    pub strict_mode: bool,
    pub continuous: bool,
    pub drain_pipe: bool,
    pub tx: mpsc::SyncSender<Result<DecoderMessage>>,
    /// The bridge owns the complete decode pipeline.
    pub bridge: FormatBridgeBox,
    /// Platform-agnostic shutdown signal for interrupt-aware I/O.
    pub shutdown_signal: sys::ShutdownSignal,
}

pub fn spawn_decoder_thread(config: DecoderThreadConfig) -> thread::JoinHandle<Result<()>> {
    thread::spawn(move || -> Result<()> {
        let DecoderThreadConfig {
            input_path,
            strict_mode,
            continuous,
            drain_pipe,
            tx,
            mut bridge,
            shutdown_signal,
        } = config;

        let mut frame_count: u64 = 0;
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

                let now = Instant::now();
                let chunk_gap_ms = last_chunk_at
                    .map(|last| now.saturating_duration_since(last).as_secs_f64() * 1000.0);
                if continuous && is_pipe_input {
                    last_chunk_at = Some(now);
                }

                let chunk_contains_spdif_sync = chunk.windows(4).any(|w| {
                    u16::from_le_bytes([w[0], w[1]]) == 0xF872
                        && u16::from_le_bytes([w[2], w[3]]) == 0x4E1F
                });

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
                // Accumulate (samples, sample_rate) per frame so that emitted_duration_ms
                // uses the input sample rate rather than a hardcoded 48 kHz constant.
                // Different frames within a chunk should share the same rate, but we
                // compute the sum correctly even if they don't.
                let mut emitted_duration_ms = 0.0f64;
                let packet_count = packets.len();
                for (transport, data_type, payload) in packets {
                    let decode_started_at = Instant::now();
                    let result =
                        bridge.push_packet(payload.as_slice().into(), transport, data_type);
                    let decode_time_ms = decode_started_at.elapsed().as_secs_f32() * 1000.0;
                    let payload_len = payload.len();
                    let emitted_frames = result.frames.len();
                    let emitted_samples: u32 =
                        result.frames.iter().map(|frame| frame.sample_count).sum();
                    let packet_emitted_ms: f64 = result.frames.iter().map(|frame| {
                        let rate = frame.sampling_frequency.max(1) as f64;
                        frame.sample_count as f64 / rate * 1000.0
                    }).sum();
                    emitted_duration_ms += packet_emitted_ms;
                    let metadata_frames = result
                        .frames
                        .iter()
                        .filter(|frame| !frame.metadata.is_empty())
                        .count();
                    let metadata_payloads: usize =
                        result.frames.iter().map(|frame| frame.metadata.len()).sum();
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
                            let min_event_id = meta.events.iter().map(|event| event.id).min();
                            let max_event_id = meta.events.iter().map(|event| event.id).max();
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
                    let new_segment_frames = result
                        .frames
                        .iter()
                        .filter(|frame| frame.is_new_segment)
                        .count();
                    let sample_count_min =
                        result.frames.iter().map(|frame| frame.sample_count).min();
                    let sample_count_max =
                        result.frames.iter().map(|frame| frame.sample_count).max();

                    if matches!(transport, RInputTransport::Iec61937) {
                        let should_warn = result.did_reset
                            || !result.error_message.is_empty()
                            || (metadata_frames > 0 && emitted_frames == 0)
                            || new_segment_frames > 0
                            || emitted_frames == 0;
                        if should_warn {
                            sys::live_log::emit_external_record(
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
                        if strict_mode && !result.error_message.is_empty() {
                            // Strict mode: propagate parse/decode error to handler.
                            let _ = tx.send(Err(anyhow::anyhow!("{}", result.error_message)));
                            return Ok(false);
                        }
                        // Non-strict: keep audio running through transient decoder resets.
                        // Flushing here turns a recoverable bridge reset into an audible
                        // dropout that can last much longer than the actual decode hiccup.
                        if strict_mode {
                            let _ = tx.send(Ok(DecoderMessage::FlushRequest));
                        } else {
                            log::debug!(
                                "Bridge reset in non-strict mode; keeping audio buffers intact"
                            );
                        }
                    }

                    let frame_count_in_packet = result.frames.len().max(1) as f32;
                    let per_frame_decode_time_ms = decode_time_ms / frame_count_in_packet;
                    let frames_in_packet = result.frames.len();
                    frames_emitted += frames_in_packet;
                    for frame in result.frames {
                        frame_count += 1;
                        let sent_at = Instant::now();
                        if tx
                            .send(Ok(DecoderMessage::AudioData(DecodedAudioData {
                                frame,
                                decode_time_ms: per_frame_decode_time_ms,
                                sent_at,
                            })))
                            .is_err()
                        {
                            return Ok(false);
                        }
                        let send_block_ms = sent_at.elapsed().as_secs_f64() * 1000.0;
                        if send_block_ms > 5.0 {
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
                        sys::live_log::emit_external_record(
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
                    sys::live_log::emit_external_record(
                        log::Level::Trace,
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
            if tx.send(Ok(DecoderMessage::StreamEnd)).is_err() {
                log::warn!("Failed to send StreamEnd message, receiver closed");
                break;
            }

            // Reset bridge for next stream.
            log::info!("Continuous mode: resetting bridge and waiting for new data...");
            bridge.reset();

            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        Ok(())
    })
}
