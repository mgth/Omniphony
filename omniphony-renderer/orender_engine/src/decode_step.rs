//! One packet through a format bridge, as every host of the renderer decodes
//! it: the embedded engine, inline or on its decode thread (`engine.rs`), and
//! the standalone renderer's decoder thread (`src/cli/decode/decoder_thread.rs`).
//!
//! What they have in common lives here, so the hosts cannot drift apart:
//! timing the decode, the length of what it decoded, and when the bridge's
//! declaration has to be read with the packet.

use bridge_api::{
    FormatBridgeBox, RChannelLabel, RChannelPose, RDecodedFrame, RInputTransport, RPushResult,
};
use std::time::Instant;

/// What a bridge declares about the current presentation beyond its labels:
/// the poses it states for its channels (`FormatBridge::fixed_channel_poses`),
/// its source family, which selects the placement policy
/// (`FormatBridge::source_family`), and the name it gives the format
/// (`FormatBridge::source_label`).
///
/// Read right after the packet that may change it, and carried with that
/// packet: a host that decodes on a thread of its own renders a frame while
/// the bridge is already on later packets, so reading it then would match a
/// frame with a later packet's declaration.
#[derive(Clone, Debug, Default)]
pub struct Declaration {
    pub poses: Vec<RChannelPose>,
    pub family: String,
    pub label: String,
}

impl Declaration {
    pub fn read(bridge: &FormatBridgeBox) -> Self {
        Self {
            poses: bridge.fixed_channel_poses().into_iter().collect(),
            family: bridge.source_family().as_str().to_owned(),
            label: bridge.source_label().as_str().to_owned(),
        }
    }
}

/// Decides which packets carry the bridge's [`Declaration`]: the first one of a
/// stream, or since the host forgot the declaration it had; one the bridge
/// reset on; and one with a frame that starts a segment or changes the channel
/// labels. Between those, a steady stream compares one short slice per frame
/// and never calls into the bridge.
pub struct DeclarationTracker {
    last_labels: Vec<RChannelLabel>,
    fresh: bool,
}

impl Default for DeclarationTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl DeclarationTracker {
    pub fn new() -> Self {
        Self {
            last_labels: Vec::new(),
            fresh: true,
        }
    }

    /// The host has forgotten the labels it read the declaration for (a reset,
    /// a new stream): the next packet's labels are new to it, even when they
    /// are the ones before.
    pub fn forget(&mut self) {
        self.fresh = true;
    }

    /// The index of the first frame of `result` the declaration read after it
    /// belongs to, or `None` when no frame needs it. A packet that starts over
    /// (fresh, or the bridge reset) needs it from its first frame; when it has
    /// no frame at all, the next packet with frames still counts as new.
    pub fn first_frame_needing_it(&mut self, result: &RPushResult) -> Option<usize> {
        let mut first = None;
        if std::mem::take(&mut self.fresh) || result.did_reset {
            self.last_labels.clear();
            first = Some(0);
        }
        for (i, frame) in result.frames.iter().enumerate() {
            if frame.is_new_segment || frame.channel_labels[..] != self.last_labels[..] {
                first.get_or_insert(i);
                self.last_labels.clear();
                self.last_labels.extend_from_slice(&frame.channel_labels);
            }
        }
        first
    }
}

/// One packet the bridge has decoded.
pub struct DecodedPacket {
    pub result: RPushResult,
    /// Time spent in the bridge's decode.
    pub decode_ms: f32,
    /// The bridge's declaration as it stood right after this packet, when a
    /// [`DeclarationTracker`] said a frame needs it.
    pub declaration: Option<Declaration>,
    /// The first frame of `result` the declaration belongs to.
    pub declaration_frame: usize,
}

impl DecodedPacket {
    /// The packet's decode time spread evenly over its frames, so a meter fed
    /// per frame reports the same figure whatever the codec's packet size.
    pub fn decode_ms_per_frame(&self) -> f32 {
        self.decode_ms / self.result.frames.len().max(1) as f32
    }

    /// The audio the packet decoded to, in seconds.
    pub fn duration_secs(&self) -> f64 {
        self.result.frames.iter().map(frame_duration_secs).sum()
    }
}

/// A frame's length in seconds; 0 when it states no sampling frequency.
pub fn frame_duration_secs(frame: &RDecodedFrame) -> f64 {
    if frame.sampling_frequency == 0 {
        return 0.0;
    }
    f64::from(frame.sample_count) / f64::from(frame.sampling_frequency)
}

/// Push one packet through `bridge` and time it. With a `tracker`, the
/// bridge's declaration is read after it when a frame needs it; without one
/// the host reads the bridge live, which is right only when nothing else
/// decodes in between.
pub fn decode_packet(
    bridge: &mut FormatBridgeBox,
    data: &[u8],
    transport: RInputTransport,
    data_type: u8,
    tracker: Option<&mut DeclarationTracker>,
) -> DecodedPacket {
    let started = Instant::now();
    let result = bridge.push_packet(data.into(), transport, data_type);
    let decode_ms = started.elapsed().as_secs_f32() * 1000.0;
    let declaration_frame = tracker.and_then(|t| t.first_frame_needing_it(&result));
    DecodedPacket {
        declaration: declaration_frame.map(|_| Declaration::read(bridge)),
        declaration_frame: declaration_frame.unwrap_or(0),
        result,
        decode_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use abi_stable::std_types::{ROption, RString, RVec};

    fn frame(labels: &[RChannelLabel], new_segment: bool) -> RDecodedFrame {
        RDecodedFrame {
            sampling_frequency: 48_000,
            sample_count: 40,
            channel_count: labels.len() as u32,
            pcm: RVec::new(),
            channel_labels: labels.iter().copied().collect(),
            metadata: RVec::new(),
            drc_gain: 1.0,
            drc_ramp_duration: 0,
            dialogue_level: ROption::RNone,
            is_new_segment: new_segment,
        }
    }

    fn push(frames: Vec<RDecodedFrame>, did_reset: bool) -> RPushResult {
        RPushResult {
            frames: RVec::from(frames),
            error_message: RString::new(),
            did_reset,
        }
    }

    #[test]
    fn the_declaration_follows_label_changes_resets_and_segments() {
        use RChannelLabel::{C, L, R};
        let stereo = [L, R];
        let three = [L, R, C];
        let mut t = DeclarationTracker::new();
        // First packet: from its first frame.
        assert_eq!(
            t.first_frame_needing_it(&push(vec![frame(&stereo, false)], false)),
            Some(0)
        );
        // Steady: never.
        let steady = (0..3).map(|_| frame(&stereo, false)).collect();
        assert_eq!(t.first_frame_needing_it(&push(steady, false)), None);
        // A label change inside the packet: from that frame.
        let change = vec![
            frame(&stereo, false),
            frame(&three, false),
            frame(&three, false),
        ];
        assert_eq!(t.first_frame_needing_it(&push(change, false)), Some(1));
        // A segment start with the same labels.
        let segment = vec![frame(&three, false), frame(&three, true)];
        assert_eq!(t.first_frame_needing_it(&push(segment, false)), Some(1));
        // A bridge reset with the same labels.
        assert_eq!(
            t.first_frame_needing_it(&push(vec![frame(&three, false)], true)),
            Some(0)
        );
        // Forgotten, then a packet with no frame: the next one is still new.
        t.forget();
        assert_eq!(t.first_frame_needing_it(&push(Vec::new(), false)), Some(0));
        assert_eq!(
            t.first_frame_needing_it(&push(vec![frame(&three, false)], false)),
            Some(0)
        );
    }

    #[test]
    fn packet_lengths_and_per_frame_decode_time() {
        let packet = DecodedPacket {
            result: push((0..4).map(|_| frame(&[], false)).collect(), false),
            decode_ms: 2.0,
            declaration: None,
            declaration_frame: 0,
        };
        assert!((packet.duration_secs() - 4.0 * 40.0 / 48_000.0).abs() < 1e-12);
        assert_eq!(packet.decode_ms_per_frame(), 0.5);
        let mut silent = frame(&[], false);
        silent.sampling_frequency = 0;
        assert_eq!(frame_duration_secs(&silent), 0.0);
    }
}
