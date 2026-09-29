//! The PipeWire sink's bridge decoder: the IEC 61937 bursts the sink extracts
//! (`audio_input::bridge::LiveBridgeIngestRuntime`), decoded on a thread of
//! their own and handed to the render handler.
//!
//! It goes through the same decode step as the pipe/file decoder thread
//! (`orender_engine::decode_step`), so what reaches the handler is the same
//! whatever the input: the frames, their decode time, and the bridge's
//! declaration (family, declared poses, label) with the frame it belongs to.

use super::decoder_thread::{DecodedAudioData, DecodedSource, DecoderMessage};
use anyhow::{Result, anyhow};
use bridge_api::{FormatBridgeBox, RInputTransport};
use orender_engine::decode_step::{
    Declaration, DeclarationTracker, DecodedPacket, DrcModeSync, decode_packet,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock, mpsc};
use std::thread;
use std::time::Instant;

/// Diagnostic metrics the worker publishes, as `f64` bits (see
/// `diag::DiagRegistry`).
pub struct LiveBridgeDiag {
    /// Number of decoded frames returned by the most recent `push_packet`.
    pub frames_per_push_packet: Arc<AtomicU64>,
    /// Wall-clock interval between consecutive `push_packet` entries (µs).
    pub push_packet_dt_us: Arc<AtomicU64>,
    /// Sample count of the last decoded frame.
    pub frame_samples: Arc<AtomicU64>,
    /// Interval between decoded frames (µs), published only above 1 ms.
    pub frame_dt_us: Arc<AtomicU64>,
    /// Decoded frames so far.
    pub frame_count: Arc<AtomicU64>,
}

/// Decode the packets `raw_rx` delivers with `bridge`, and hand each frame to
/// `tx` without ever blocking: a live source cannot wait, so a frame the
/// handler has no room for is dropped. A declaration it carried is not: it
/// rides on the next frame that is delivered, unless a newer one replaced it.
///
/// `requested_drc_mode` is the DRC mode the handler asks for; it reaches the
/// bridge before the next packet whenever it changes.
pub fn spawn_live_bridge_decoder(
    bridge: FormatBridgeBox,
    raw_rx: mpsc::Receiver<(u8, Vec<u8>)>,
    requested_drc_mode: Option<Arc<RwLock<String>>>,
    diag: Option<LiveBridgeDiag>,
    tx: mpsc::SyncSender<Result<DecoderMessage>>,
) -> Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("bridge-decode".to_string())
        .spawn(move || {
            run_live_bridge_decoder(bridge, raw_rx, requested_drc_mode, diag, tx);
        })
        .map_err(|e| anyhow!("Failed to spawn bridge decode worker: {e}"))
}

fn run_live_bridge_decoder(
    mut bridge: FormatBridgeBox,
    raw_rx: mpsc::Receiver<(u8, Vec<u8>)>,
    requested_drc_mode: Option<Arc<RwLock<String>>>,
    diag: Option<LiveBridgeDiag>,
    tx: mpsc::SyncSender<Result<DecoderMessage>>,
) {
    let mut first_frame_logs_remaining = 16usize;
    let mut drc_mode = DrcModeSync::new();
    let mut declarations = DeclarationTracker::new();
    // A declaration whose frame the handler had no room for.
    let mut undelivered: Option<Declaration> = None;
    let mut last_push_packet_at: Option<Instant> = None;
    let mut last_frame_at: Option<Instant> = None;
    let mut frame_count: u64 = 0;
    while let Ok((data_type, payload)) = raw_rx.recv() {
        if let Some(requested) = requested_drc_mode.as_ref() {
            let requested = requested.read().unwrap_or_else(|e| e.into_inner());
            drc_mode.apply(&requested, &mut bridge);
        }
        let push_packet_at = Instant::now();
        let push_packet_dt_us = last_push_packet_at
            .map(|prev| push_packet_at.saturating_duration_since(prev).as_micros() as u64)
            .unwrap_or(0);
        last_push_packet_at = Some(push_packet_at);
        let packet = decode_packet(
            &mut bridge,
            &payload,
            RInputTransport::Iec61937,
            data_type,
            &mut declarations,
        );
        let per_frame_decode_time_ms = packet.decode_ms_per_frame();
        let DecodedPacket {
            result,
            mut declaration,
            declaration_frame,
            ..
        } = packet;
        if let Some(d) = diag.as_ref() {
            d.frames_per_push_packet
                .store((result.frames.len() as f64).to_bits(), Ordering::Relaxed);
            d.push_packet_dt_us
                .store((push_packet_dt_us as f64).to_bits(), Ordering::Relaxed);
        }
        if !result.error_message.is_empty() || result.did_reset {
            log::warn!(
                "PipeWire bridge packet: data_type=0x{:02X} payload_bytes={} frames={} reset={} error={}",
                data_type,
                payload.len(),
                result.frames.len(),
                result.did_reset,
                result.error_message
            );
        }
        // Bridge reset: keep audio running, never abort or flush. In live
        // rendering we never want a premature stop (strict mode removed).
        for (i, frame) in result.frames.into_iter().enumerate() {
            if first_frame_logs_remaining > 0 {
                first_frame_logs_remaining -= 1;
                let frame_ms =
                    frame.sample_count as f64 / frame.sampling_frequency.max(1) as f64 * 1000.0;
                log::debug!(
                    "PipeWire bridge decoded frame: sr={} sample_count={} ch={} frame_ms={:.3} data_type=0x{:02X} payload_bytes={}",
                    frame.sampling_frequency,
                    frame.sample_count,
                    frame.channel_count,
                    frame_ms,
                    data_type,
                    payload.len()
                );
            }
            if let Some(d) = diag.as_ref() {
                let now = Instant::now();
                let dt_us = last_frame_at
                    .map(|prev| now.saturating_duration_since(prev).as_micros() as u64)
                    .unwrap_or(0);
                last_frame_at = Some(now);
                frame_count = frame_count.saturating_add(1);
                d.frame_samples
                    .store((frame.sample_count as f64).to_bits(), Ordering::Relaxed);
                // The bridge emits several frames per packet, dispatched in a
                // tight loop microseconds apart: meaningless as a cadence. Only
                // the gaps between batches (> 1 ms) are published.
                if dt_us >= 1000 {
                    d.frame_dt_us
                        .store((dt_us as f64).to_bits(), Ordering::Relaxed);
                }
                d.frame_count
                    .store((frame_count as f64).to_bits(), Ordering::Relaxed);
            }
            let declaration = if i == declaration_frame {
                declaration.take()
            } else {
                None
            };
            deliver(
                &tx,
                DecodedAudioData {
                    source: DecodedSource::Bridge,
                    frame,
                    declaration,
                    decode_time_ms: per_frame_decode_time_ms,
                    sent_at: Instant::now(),
                },
                &mut undelivered,
            );
        }
    }
}

/// Hand `data` to the handler if it has room, else drop it, but keep the
/// declaration it carried in `undelivered` for the next frame: the handler
/// keeps the last one it received, so losing it would leave it on the previous
/// layout's. A frame's own declaration is newer and replaces a waiting one.
fn deliver(
    tx: &mpsc::SyncSender<Result<DecoderMessage>>,
    mut data: DecodedAudioData,
    undelivered: &mut Option<Declaration>,
) {
    if data.declaration.is_none() {
        data.declaration = undelivered.take();
    } else {
        *undelivered = None;
    }
    if let Err(mpsc::TrySendError::Full(Ok(DecoderMessage::AudioData(dropped)))) =
        tx.try_send(Ok(DecoderMessage::AudioData(data)))
    {
        *undelivered = dropped.declaration;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use abi_stable::sabi_trait::prelude::TD_Opaque;
    use abi_stable::std_types::{ROption, RSlice, RStr, RString, RVec};
    use bridge_api::*;
    use std::sync::Mutex;

    /// Packet byte 0 picks the labels (0: stereo, else 5.1) of the one frame
    /// it decodes to; the declaration names the family it was read for.
    struct ScriptedBridge {
        labels: Vec<RChannelLabel>,
        drc_modes: Arc<Mutex<Vec<String>>>,
    }

    impl FormatBridge for ScriptedBridge {
        fn push_packet(&mut self, data: RSlice<'_, u8>, _: RInputTransport, _: u8) -> RPushResult {
            use RChannelLabel::*;
            self.labels = if data[0] == 0 {
                vec![L, R]
            } else {
                vec![L, R, C, LFE, Ls, Rs]
            };
            let channels = self.labels.len();
            RPushResult {
                frames: RVec::from(vec![RDecodedFrame {
                    sampling_frequency: 48_000,
                    sample_count: 40,
                    channel_count: channels as u32,
                    pcm: RVec::from(vec![0; 40 * channels]),
                    channel_labels: self.labels.iter().copied().collect(),
                    metadata: RVec::new(),
                    drc_gain: 1.0,
                    drc_ramp_duration: 0,
                    dialogue_level: ROption::RNone,
                    is_new_segment: false,
                }]),
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
            self.drc_modes
                .lock()
                .unwrap()
                .push(mode.as_str().to_owned());
            true
        }
        fn fixed_channel_poses(&self) -> RVec<RChannelPose> {
            self.labels
                .iter()
                .map(|&label| RChannelPose {
                    label,
                    azimuth_deg: 30.0,
                    elevation_deg: 0.0,
                })
                .collect()
        }
        fn source_family(&self) -> RString {
            RString::from(if self.labels.len() == 2 { "pcm" } else { "dts" })
        }
        fn source_label(&self) -> RString {
            RString::from(format!("{} channels", self.labels.len()))
        }
    }

    fn bridge(drc_modes: &Arc<Mutex<Vec<String>>>) -> FormatBridgeBox {
        FormatBridge_TO::from_value(
            ScriptedBridge {
                labels: Vec::new(),
                drc_modes: Arc::clone(drc_modes),
            },
            TD_Opaque,
        )
    }

    /// Run the worker over `packets` (byte 0 of each: its layout) with a
    /// handler queue of `capacity`, draining it only at the end.
    fn run_frames(
        packets: &[u8],
        capacity: usize,
        drc: Option<Arc<RwLock<String>>>,
    ) -> (Vec<DecodedAudioData>, Vec<String>) {
        let drc_modes = Arc::default();
        let (raw_tx, raw_rx) = mpsc::sync_channel(packets.len());
        let (tx, rx) = mpsc::sync_channel(capacity);
        for &p in packets {
            raw_tx.send((0x0B, vec![p])).unwrap();
        }
        drop(raw_tx);
        run_live_bridge_decoder(bridge(&drc_modes), raw_rx, drc, None, tx);
        let frames = rx
            .try_iter()
            .map(|m| match m.unwrap() {
                DecoderMessage::AudioData(d) => d,
                _ => panic!("only audio is sent"),
            })
            .collect();
        let drc_modes = drc_modes.lock().unwrap().clone();
        (frames, drc_modes)
    }

    /// [`run_frames`], keeping only the declarations.
    fn run(
        packets: &[u8],
        capacity: usize,
        drc: Option<Arc<RwLock<String>>>,
    ) -> (Vec<Option<Declaration>>, Vec<String>) {
        let (frames, drc_modes) = run_frames(packets, capacity, drc);
        (
            frames.into_iter().map(|d| d.declaration).collect(),
            drc_modes,
        )
    }

    fn family(d: &Option<Declaration>) -> Option<&str> {
        d.as_ref().map(|d| d.family.as_str())
    }

    /// What the sink decodes reaches the handler with the bridge's
    /// declaration, on the first frame and on each layout change, as through
    /// the pipe: before, it was always `None`, and the renderer kept the
    /// generic family and no declared poses for every bitstream on the sink.
    #[test]
    fn the_declaration_reaches_the_handler_with_its_frame() {
        let (declarations, _) = run(&[0, 0, 1, 1, 0], 8, None);
        let families: Vec<_> = declarations.iter().map(family).collect();
        assert_eq!(
            families,
            [Some("pcm"), None, Some("dts"), None, Some("pcm")]
        );
        let dts = declarations[2].as_ref().unwrap();
        assert_eq!(dts.poses.len(), 6);
        assert_eq!(dts.label, "6 channels");
    }

    fn audio(declaration: Option<Declaration>) -> DecodedAudioData {
        DecodedAudioData {
            source: DecodedSource::Bridge,
            frame: RDecodedFrame {
                sampling_frequency: 48_000,
                sample_count: 0,
                channel_count: 0,
                pcm: RVec::new(),
                channel_labels: RVec::new(),
                metadata: RVec::new(),
                drc_gain: 1.0,
                drc_ramp_duration: 0,
                dialogue_level: ROption::RNone,
                is_new_segment: false,
            },
            declaration,
            decode_time_ms: 0.0,
            sent_at: Instant::now(),
        }
    }

    fn declared(family: &str) -> Option<Declaration> {
        Some(Declaration {
            family: family.to_owned(),
            ..Declaration::default()
        })
    }

    fn received(rx: &mpsc::Receiver<Result<DecoderMessage>>) -> Option<Declaration> {
        match rx.try_recv().unwrap().unwrap() {
            DecoderMessage::AudioData(d) => d.declaration,
            _ => panic!("only audio is sent"),
        }
    }

    /// A frame the handler has no room for is dropped, but not the
    /// declaration it carried: the next delivered frame brings it, unless a
    /// newer one came first.
    #[test]
    fn a_dropped_frames_declaration_rides_on_the_next_one() {
        let (tx, rx) = mpsc::sync_channel(1);
        let mut undelivered = None;
        deliver(&tx, audio(declared("pcm")), &mut undelivered);
        deliver(&tx, audio(declared("dts")), &mut undelivered); // queue full
        assert_eq!(family(&received(&rx)), Some("pcm"));
        deliver(&tx, audio(None), &mut undelivered);
        assert_eq!(family(&received(&rx)), Some("dts"));
        deliver(&tx, audio(None), &mut undelivered);
        assert_eq!(family(&received(&rx)), None, "delivered once");

        deliver(&tx, audio(None), &mut undelivered);
        deliver(&tx, audio(declared("dts")), &mut undelivered); // queue full
        deliver(&tx, audio(declared("auro")), &mut undelivered); // queue full
        assert_eq!(family(&received(&rx)), None);
        deliver(&tx, audio(None), &mut undelivered);
        assert_eq!(family(&received(&rx)), Some("auro"), "the newer one");
    }

    /// The sink's plain PCM carries no declaration, and its bridge never sees
    /// it. After a bitstream, the PCM must still reach the renderer as PCM —
    /// its own family, no declared poses — not with the bitstream's DTS
    /// angles; and the bitstream that follows must get its declaration back,
    /// although its bridge, whose labels did not change, does not send it
    /// again.
    #[test]
    fn the_sinks_pcm_does_not_inherit_the_bitstreams_declaration() {
        use super::super::state::SpatialState;
        use renderer::placement::SourceFamily;

        let (bitstream, _) = run_frames(&[1, 1, 1], 8, None);
        assert_eq!(
            bitstream
                .iter()
                .map(|d| family(&d.declaration))
                .collect::<Vec<_>>(),
            [Some("dts"), None, None],
            "the bridge declares once"
        );
        let live_pcm = || DecodedAudioData {
            source: DecodedSource::Live,
            ..audio(None)
        };
        let mut bitstream = bitstream.into_iter();
        let mut spatial = SpatialState::default();
        let mut seen = Vec::new();
        for data in [
            bitstream.next().unwrap(),
            bitstream.next().unwrap(),
            live_pcm(),
            live_pcm(),
            bitstream.next().unwrap(),
        ] {
            spatial.take_declaration(data.source, data.declaration);
            seen.push((
                spatial.source_family,
                spatial.declared_poses.len(),
                spatial.source_label.clone(),
            ));
        }
        let dts = (SourceFamily::Dts, 6, "6 channels".to_owned());
        let pcm = (SourceFamily::Pcm, 0, "PCM".to_owned());
        assert_eq!(seen, [dts.clone(), dts.clone(), pcm.clone(), pcm, dts]);
    }

    /// The requested DRC mode reaches the bridge once, then on changes only.
    #[test]
    fn the_drc_mode_is_pushed_on_changes_only() {
        let requested = Arc::new(RwLock::new("Off".to_string()));
        let (_, pushed) = run(&[0, 0, 0], 8, Some(Arc::clone(&requested)));
        assert_eq!(pushed, ["Off"]);
    }
}
