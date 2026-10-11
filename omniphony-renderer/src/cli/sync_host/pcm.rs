//! The `own` sink's linear PCM (desktop audio, a player that does not pass
//! its stream through): 32-bit float, interleaved, on the fixed 7.1 input
//! map. No bridge decodes it: the host turns each chunk into a frame and the
//! engine renders it as it renders a bridge's channel-based frame
//! ([`Engine::render_host_pcm`](orender_engine::engine::Engine::render_host_pcm)).

use bridge_api::{I32_PCM_FULL_SCALE, RChannelLabel, RDecodedFrame};

/// The fixed input map, in the order the sink's channels come.
pub const INPUT_MAP: [RChannelLabel; 8] = [
    RChannelLabel::L,
    RChannelLabel::R,
    RChannelLabel::C,
    RChannelLabel::LFE,
    RChannelLabel::Ls,
    RChannelLabel::Rs,
    RChannelLabel::Lb,
    RChannelLabel::Rb,
];

/// A frame of the whole sample frames in `bytes` (interleaved `f32`, little
/// endian, [`INPUT_MAP`] channels) at `rate_hz`. Bytes past the last whole
/// frame are left out: a cycle carries whole frames, so there are none.
pub fn frame(bytes: &[u8], rate_hz: u32) -> RDecodedFrame {
    let channels = INPUT_MAP.len();
    let frame_bytes = channels * std::mem::size_of::<f32>();
    let frames = bytes.len() / frame_bytes;
    let pcm: Vec<i32> = bytes[..frames * frame_bytes]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| {
            let sample = f32::from_le_bytes(*b);
            // Unity is 2^23, the scale every decoded frame comes in.
            (sample.clamp(-1.0, 1.0) * I32_PCM_FULL_SCALE as f32).round() as i32
        })
        .collect();
    RDecodedFrame {
        sampling_frequency: rate_hz,
        sample_count: frames as u32,
        channel_count: channels as u32,
        pcm: pcm.into(),
        channel_labels: INPUT_MAP.to_vec().into(),
        metadata: Vec::new().into(),
        drc_gain: 1.0,
        drc_ramp_duration: 0,
        dialogue_level: abi_stable::std_types::ROption::RNone,
        is_new_segment: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(samples: &[f32]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    /// Full scale is 2^23 like every decoded frame, the channels keep their
    /// order on the fixed map, and a float beyond full scale is clamped.
    #[test]
    fn a_chunk_becomes_a_frame_at_the_decoded_scale() {
        let mut samples = vec![0.0f32; 16];
        samples[0] = 1.0; // frame 0, L
        samples[3] = -0.5; // frame 0, LFE
        samples[8 + 7] = 2.0; // frame 1, Rb: clamped
        let f = frame(&bytes(&samples), 48_000);
        assert_eq!((f.sample_count, f.channel_count), (2, 8));
        assert_eq!(f.sampling_frequency, 48_000);
        assert_eq!(f.channel_labels.as_slice(), INPUT_MAP.as_slice());
        assert_eq!(f.pcm[0], I32_PCM_FULL_SCALE);
        assert_eq!(f.pcm[3], -I32_PCM_FULL_SCALE / 2);
        assert_eq!(f.pcm[15], I32_PCM_FULL_SCALE);
        assert!(f.metadata.is_empty());
    }

    /// Only whole frames are taken.
    #[test]
    fn a_partial_frame_is_left_out() {
        let mut b = bytes(&[0.25f32; 8]);
        b.extend_from_slice(&[0u8; 5]);
        let f = frame(&b, 48_000);
        assert_eq!(f.sample_count, 1);
        assert_eq!(f.pcm.len(), 8);
    }
}
