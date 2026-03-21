use anyhow::Result;
use bridge_api::{FormatBridgeBox, RInputTransport};
use spdif::SpdifParser;
use std::io;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use sys::InputReader;

const PIPE_STREAM_GAP_RESET_THRESHOLD: Duration = Duration::from_millis(500);

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

            let mut process_chunk = |chunk: &[u8]| -> Result<bool> {
                // Secondary check: interrupt the current stream on shutdown or reload.
                if sys::ShutdownHandle::is_requested()
                    || sys::ShutdownHandle::is_reload_requested()
                    || sys::ShutdownHandle::is_restart_from_config_requested()
                {
                    return Ok(false);
                }

                let now = Instant::now();
                if continuous && is_pipe_input {
                    if let Some(last) = last_chunk_at {
                        let gap = now.saturating_duration_since(last);
                        if gap >= PIPE_STREAM_GAP_RESET_THRESHOLD && frame_count > 0 {
                            log::info!(
                                "Detected input gap of {:.0} ms on pipe, treating next data as a new stream",
                                gap.as_secs_f64() * 1000.0
                            );
                            if tx.send(Ok(DecoderMessage::StreamEnd)).is_err() {
                                log::warn!("Failed to send StreamEnd message, receiver closed");
                                return Ok(false);
                            }
                            bridge.reset();
                            is_spdif = None;
                            spdif_parser = SpdifParser::new();
                        }
                    }
                    last_chunk_at = Some(now);
                }

                // Detect transport format on the first chunk.
                if is_spdif.is_none() && chunk.len() >= 4 {
                    if u16::from_le_bytes([chunk[0], chunk[1]]) == 0xF872
                        && u16::from_le_bytes([chunk[2], chunk[3]]) == 0x4E1F
                    {
                        is_spdif = Some(true);
                        log::info!("Detected S/PDIF encapsulated stream");
                    } else {
                        is_spdif = Some(false);
                        log::info!("Detected raw stream");
                    }
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

                for (transport, data_type, payload) in packets {
                    let decode_started_at = Instant::now();
                    let result =
                        bridge.push_packet(payload.as_slice().into(), transport, data_type);
                    let decode_time_ms = decode_started_at.elapsed().as_secs_f32() * 1000.0;

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
                    for frame in result.frames {
                        frame_count += 1;
                        if tx
                            .send(Ok(DecoderMessage::AudioData(DecodedAudioData {
                                frame,
                                decode_time_ms: per_frame_decode_time_ms,
                            })))
                            .is_err()
                        {
                            return Ok(false);
                        }
                    }
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
