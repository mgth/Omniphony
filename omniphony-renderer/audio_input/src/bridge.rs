use spdif::SpdifParser;
use std::sync::mpsc;

/// Extracts the IEC 61937 bursts from what the PipeWire sink captures and
/// queues them for the host's bridge decoder (the standalone renderer's
/// `src/cli/decode/live_bridge.rs`), which decodes them through the same
/// decode step as every other input.
pub struct LiveBridgeIngestRuntime {
    raw_tx: mpsc::SyncSender<(u8, Vec<u8>)>,
    spdif_parser: SpdifParser,
}

impl LiveBridgeIngestRuntime {
    pub fn new(raw_tx: mpsc::SyncSender<(u8, Vec<u8>)>) -> Self {
        Self {
            raw_tx,
            spdif_parser: SpdifParser::new(),
        }
    }

    pub fn process_chunk(&mut self, chunk: &[u8]) -> (usize, usize) {
        let mut packet_count = 0usize;
        let mut queued_count = 0usize;
        self.spdif_parser.push_bytes(chunk);
        while let Some(packet) = self.spdif_parser.get_next_packet() {
            packet_count += 1;
            if self
                .raw_tx
                .try_send((packet.data_type, packet.payload))
                .is_ok()
            {
                queued_count += 1;
            }
        }
        if packet_count > 0 {
            log::debug!(
                "LiveBridgeIngestRuntime: extracted {} SPDIF packet(s), queued {}",
                packet_count,
                queued_count
            );
            if queued_count < packet_count {
                log::warn!(
                    "LiveBridgeIngestRuntime dropped {} SPDIF packet(s): bridge decode queue is full or disconnected",
                    packet_count - queued_count
                );
            }
        }
        (packet_count, queued_count)
    }
}
