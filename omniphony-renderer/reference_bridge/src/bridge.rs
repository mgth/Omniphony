//! `FormatBridge` implementation that turns a multichannel WAV/PCM file into a
//! channel bed for the renderer.
//!
//! The bridge buffers the raw bytes delivered through `push_packet`, parses the
//! RIFF/WAVE header once, then converts the accumulated PCM into
//! [`RDecodedFrame`]s. Each frame carries one [`RChannelLabel`] per channel and
//! **empty** metadata: that is exactly how the renderer recognises a plain
//! channel bed (`Engine::process_decoded_frame` treats any non-empty metadata as
//! object content and skips the bed path). The renderer then spatialises the bed
//! through its virtual-bed / VBAP stage according to the per-channel labels.

use abi_stable::std_types::{RSlice, RStr, RString, RVec};
use bridge_api::{
    FormatBridge, RChannelLabel, RChannelPose, RCoordinateFormat, RDecodedFrame, RInputTransport,
    RMetadataFrame, RPushResult, RVbapCartesianDefaults, RVbapTableMode,
};

use crate::logging::bridge_diag_log;
use crate::wav::{HeaderParse, WavFormat, parse_header};

/// Maximum number of sample-frames emitted in a single [`RDecodedFrame`].
/// Bounds per-frame allocation and keeps the renderer's per-frame work modest
/// while staying large enough to avoid per-call overhead dominating.
const BLOCK_FRAMES: usize = 2048;

/// Streaming parse state.
enum State {
    /// Still accumulating bytes until the WAVE header can be parsed.
    Header,
    /// Header parsed; `format` known and PCM is being streamed. `remaining`
    /// counts the data-chunk bytes still expected (`u64::MAX` = until EOF).
    Data { format: WavFormat, remaining: u64 },
}

pub(crate) struct WavBridge {
    /// Accumulates raw input bytes across `push_packet` calls.
    buf: Vec<u8>,
    state: State,
    /// Cached per-channel labels for the active format (computed once, cloned
    /// per emitted frame — never per sample).
    labels: Vec<RChannelLabel>,
    strict: bool,
    frames_emitted: u64,
}

impl WavBridge {
    pub(crate) fn new(strict: bool) -> Self {
        Self {
            buf: Vec::new(),
            state: State::Header,
            labels: Vec::new(),
            strict,
            frames_emitted: 0,
        }
    }

    fn reset_state(&mut self) {
        self.buf.clear();
        self.state = State::Header;
        self.labels.clear();
    }

    /// Run one `push_packet` body, catching a panic instead of letting it
    /// unwind into the host: across the ABI boundary a panic ends the process,
    /// which, when the engine is loaded as `liborender`, is the media player
    /// (`BRIDGE_API.md`, "Panics"). A caught panic is a failed chunk: the
    /// parser is reset like on any other decode failure, which a non-strict
    /// host sees as `did_reset` and a strict one as an error.
    fn recover_from_panic(&mut self, decode: impl FnOnce(&mut Self) -> RPushResult) -> RPushResult {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decode(self))) {
            Ok(result) => result,
            Err(payload) => {
                let reason = payload
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                    .unwrap_or("unknown panic");
                let mut result = RPushResult {
                    frames: RVec::new(),
                    error_message: RString::new(),
                    did_reset: false,
                };
                self.fail(
                    &mut result,
                    &format!("reference-bridge: decode panicked: {reason}"),
                );
                result
            }
        }
    }

    /// The `push_packet` body: buffer `data`, parse the header once, then
    /// emit the PCM it completes.
    fn decode(&mut self, data: &[u8]) -> RPushResult {
        let mut result = RPushResult {
            frames: RVec::new(),
            error_message: RString::new(),
            did_reset: false,
        };

        // The bridge is byte-stream oriented; both Raw and any extracted payload
        // are simply appended. (The orender file-decode path always uses Raw.)
        self.buf.extend_from_slice(data);

        if matches!(self.state, State::Header) && !self.try_parse_header(&mut result) {
            return result;
        }
        self.drain_pcm(&mut result);
        result
    }

    /// Emit one error into `result`, resetting the parser. In strict mode the
    /// message is surfaced via `error_message`; otherwise it is logged only.
    fn fail(&mut self, result: &mut RPushResult, message: &str) {
        bridge_diag_log(log::Level::Warn, message);
        self.reset_state();
        result.did_reset = true;
        if self.strict {
            result.error_message = RString::from(message);
        }
    }

    /// Try to parse the header from the front of `buf`. On success transitions to
    /// [`State::Data`] and drains the consumed header bytes. Returns `true` once
    /// streaming can proceed.
    fn try_parse_header(&mut self, result: &mut RPushResult) -> bool {
        match parse_header(&self.buf) {
            HeaderParse::NeedMore => false,
            HeaderParse::Invalid(reason) => {
                self.fail(result, &format!("reference-bridge: invalid WAV: {reason}"));
                false
            }
            HeaderParse::Found {
                format,
                data_offset,
                data_len,
            } => {
                self.labels = channel_labels(format.channels, format.channel_mask);
                self.buf.drain(0..data_offset);
                bridge_diag_log(
                    log::Level::Info,
                    &format!(
                        "reference-bridge: WAV header parsed: {} ch, {} Hz, {:?}",
                        format.channels, format.sample_rate, format.sample_format
                    ),
                );
                self.state = State::Data {
                    format,
                    remaining: data_len,
                };
                true
            }
        }
    }

    /// Convert all complete sample-frames currently buffered into decoded frames.
    fn drain_pcm(&mut self, result: &mut RPushResult) {
        let State::Data { format, remaining } = &mut self.state else {
            return;
        };
        let format = *format;
        let bytes_per_sample = format.sample_format.bytes_per_sample();
        let channels = format.channels as usize;
        let bytes_per_frame = format.bytes_per_frame();
        if bytes_per_frame == 0 {
            return;
        }

        // Honour the declared data size: never read past the data chunk.
        let available_bytes = if *remaining == u64::MAX {
            self.buf.len()
        } else {
            self.buf.len().min(*remaining as usize)
        };
        let total_frames = available_bytes / bytes_per_frame;
        if total_frames == 0 {
            return;
        }

        let mut frame_start = 0usize; // running byte cursor into `self.buf`
        let mut frames_left = total_frames;
        while frames_left > 0 {
            let n = frames_left.min(BLOCK_FRAMES);
            let sample_total = n * channels;
            let mut pcm: RVec<i32> = RVec::with_capacity(sample_total);

            // Interleaved conversion. One reserved allocation for the whole
            // block; no per-sample heap activity.
            let mut byte_idx = frame_start;
            for _ in 0..sample_total {
                let s = format
                    .sample_format
                    .decode_sample(&self.buf[byte_idx..byte_idx + bytes_per_sample]);
                pcm.push(s);
                byte_idx += bytes_per_sample;
            }

            result.frames.push(RDecodedFrame {
                sampling_frequency: format.sample_rate,
                sample_count: n as u32,
                channel_count: format.channels as u32,
                pcm,
                channel_labels: RVec::from(self.labels.clone()),
                // Empty metadata ⇒ the renderer treats this as a channel bed.
                metadata: RVec::<RMetadataFrame>::new(),
                drc_gain: 1.0,
                drc_ramp_duration: 0,
                dialogue_level: abi_stable::std_types::ROption::RNone,
                is_new_segment: false,
            });

            frame_start += n * bytes_per_frame;
            frames_left -= n;
        }

        self.frames_emitted += total_frames as u64;
        let consumed = total_frames * bytes_per_frame;
        if let State::Data { remaining, .. } = &mut self.state {
            if *remaining != u64::MAX {
                *remaining -= consumed as u64;
            }
        }
        // Single O(remaining) compaction per call; leftover is < one block.
        self.buf.drain(0..consumed);
    }
}

/// Map a WAV's channels to canonical [`RChannelLabel`]s.
///
/// A `WAVE_FORMAT_EXTENSIBLE` header's `dwChannelMask` names the positions
/// (see [`labels_from_mask`]); that is what ffmpeg and most tools write for
/// more than two channels, in the WAVE order, where a 7.1 is
/// `FL FR FC LFE BL BR SL SR` — backs before sides.
///
/// Without a mask, the channels are read in that same WAVE order: known counts
/// take their standard layout's mask (see [`default_channel_mask`]), so a file
/// means the same thing with or without one. Any other count labels its leading
/// channels in the 7.1.4 WAVE order and marks the rest `Unknown` (still
/// rendered, just without a canonical position).
fn channel_labels(channel_count: u16, channel_mask: u32) -> Vec<RChannelLabel> {
    let mask = match channel_mask {
        0 => default_channel_mask(channel_count),
        mask => mask,
    };
    labels_from_mask(channel_count, mask)
}

/// `dwChannelMask` of the standard layout for a channel count, for files that
/// carry none: mono, stereo, 5.1 (sides), 7.1 and 7.1.4 in the WAVE order.
/// Other counts fall back to the 7.1.4 mask, whose positions label as many
/// leading channels as there are.
fn default_channel_mask(channel_count: u16) -> u32 {
    match channel_count {
        1 => MASK_MONO,
        2 => MASK_STEREO,
        6 => MASK_5_1_SIDE,
        8 => MASK_7_1,
        _ => MASK_7_1_4,
    }
}

const MASK_MONO: u32 = 0x4; // FC
const MASK_STEREO: u32 = 0x3; // FL FR
const MASK_5_1_SIDE: u32 = 0x60F; // FL FR FC LFE SL SR
const MASK_7_1: u32 = 0x63F; // FL FR FC LFE BL BR SL SR
const MASK_7_1_4: u32 = 0x2D63F; // 7.1 + TFL TFR TBL TBR

/// Speaker positions of the `dwChannelMask` bits, lowest bit first (the order
/// the channels are interleaved in). `SPEAKER_TOP_BACK_CENTER` has no
/// renderer label.
const MASK_POSITIONS: [RChannelLabel; 18] = {
    use RChannelLabel::*;
    [
        L,       // FRONT_LEFT
        R,       // FRONT_RIGHT
        C,       // FRONT_CENTER
        LFE,     // LOW_FREQUENCY
        Lb,      // BACK_LEFT
        Rb,      // BACK_RIGHT
        Lsc,     // FRONT_LEFT_OF_CENTER
        Rsc,     // FRONT_RIGHT_OF_CENTER
        Cb,      // BACK_CENTER
        Ls,      // SIDE_LEFT
        Rs,      // SIDE_RIGHT
        Tc,      // TOP_CENTER
        Tfl,     // TOP_FRONT_LEFT
        Tfc,     // TOP_FRONT_CENTER
        Tfr,     // TOP_FRONT_RIGHT
        Tbl,     // TOP_BACK_LEFT
        Unknown, // TOP_BACK_CENTER
        Tbr,     // TOP_BACK_RIGHT
    ]
};

const MASK_BACK_PAIR: u32 = 0b11 << 4;
const MASK_SIDE_PAIR: u32 = 0b11 << 9;

/// Label channels from a `dwChannelMask`: one channel per set bit, in
/// ascending bit order. Channels beyond the mask's positions (or on a
/// position with no renderer label) are `Unknown`.
///
/// A back pair with no side pair is the surround pair of a 5.1 written as
/// `FL FR FC LFE BL BR` (ffmpeg's `5.1`, as opposed to `5.1(side)`), so it is
/// labelled `Ls`/`Rs`, as a 5.1's surrounds are everywhere else. With both
/// pairs present, the backs are `Lb`/`Rb` and the sides `Ls`/`Rs`.
fn labels_from_mask(channel_count: u16, channel_mask: u32) -> Vec<RChannelLabel> {
    use RChannelLabel::*;
    let backs_are_surrounds =
        channel_mask & MASK_BACK_PAIR != 0 && channel_mask & MASK_SIDE_PAIR == 0;
    let mut positions = MASK_POSITIONS
        .iter()
        .enumerate()
        .filter(|&(bit, _)| channel_mask & (1 << bit) != 0)
        .map(|(_, &label)| match label {
            Lb if backs_are_surrounds => Ls,
            Rb if backs_are_surrounds => Rs,
            label => label,
        });
    (0..channel_count)
        .map(|_| positions.next().unwrap_or(Unknown))
        .collect()
}

impl FormatBridge for WavBridge {
    fn push_packet(
        &mut self,
        data: RSlice<'_, u8>,
        _transport: RInputTransport,
        _data_type: u8,
    ) -> RPushResult {
        self.recover_from_panic(|bridge| bridge.decode(data.as_slice()))
    }

    fn reset(&mut self) {
        self.reset_state();
    }

    fn is_ready(&self) -> bool {
        self.frames_emitted > 0
    }

    fn has_objects(&self) -> bool {
        // A WAV file carries fixed channels only: no dynamic objects.
        false
    }

    fn configure(&mut self, key: RStr<'_>, _value: RStr<'_>) -> bool {
        // A WAV file exposes a single presentation, so the host's mandatory
        // `presentation` selection is accepted (and ignored) — returning false
        // here makes the CLI abort with "Bridge rejected presentation value".
        // All other keys are unrecognised.
        key.as_str() == "presentation"
    }

    fn coordinate_format(&self) -> RCoordinateFormat {
        RCoordinateFormat::Cartesian
    }

    fn vbap_cartesian_defaults(&self) -> RVbapCartesianDefaults {
        // The balanced default grid; WAV beds have no position below the floor.
        RVbapCartesianDefaults::BALANCED
    }

    fn preferred_vbap_table_mode(&self) -> RVbapTableMode {
        RVbapTableMode::Cartesian
    }

    fn supported_drc_modes(&self) -> RVec<RString> {
        // Linear PCM carries no dynamic-range metadata.
        RVec::new()
    }

    fn set_drc_mode(&mut self, _mode: RStr<'_>) -> bool {
        false
    }

    fn fixed_channel_poses(&self) -> RVec<RChannelPose> {
        // A WAV file states no angle for its channels: every one takes the
        // renderer's own pose for its label.
        RVec::new()
    }

    fn source_family(&self) -> RString {
        RString::from("pcm")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_wav(channels: u16, sample_rate: u32, frames: &[Vec<i16>]) -> Vec<u8> {
        let mut data = Vec::new();
        for frame in frames {
            for &s in frame {
                data.extend_from_slice(&s.to_le_bytes());
            }
        }
        let mut buf = Vec::new();
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        buf.extend_from_slice(b"WAVE");
        buf.extend_from_slice(b"fmt ");
        buf.extend_from_slice(&16u32.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.extend_from_slice(&channels.to_le_bytes());
        buf.extend_from_slice(&sample_rate.to_le_bytes());
        buf.extend_from_slice(&(sample_rate * channels as u32 * 2).to_le_bytes());
        buf.extend_from_slice(&(channels * 2).to_le_bytes());
        buf.extend_from_slice(&16u16.to_le_bytes());
        buf.extend_from_slice(b"data");
        buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
        buf.extend_from_slice(&data);
        buf
    }

    #[test]
    fn labels_for_supported_counts() {
        use RChannelLabel::*;
        assert_eq!(channel_labels(2, 0), vec![L, R]);
        assert_eq!(channel_labels(6, 0), vec![L, R, C, LFE, Ls, Rs]);
        // Without a mask, 7.1 and 7.1.4 read in the WAVE order (backs
        // before sides), exactly like the same file with its mask.
        assert_eq!(channel_labels(8, 0), vec![L, R, C, LFE, Lb, Rb, Ls, Rs]);
        assert_eq!(channel_labels(8, 0), channel_labels(8, 0x63F));
        assert_eq!(
            channel_labels(12, 0),
            vec![L, R, C, LFE, Lb, Rb, Ls, Rs, Tfl, Tfr, Tbl, Tbr]
        );
        assert_eq!(channel_labels(12, 0), channel_labels(12, 0x2D63F));
        assert_eq!(channel_labels(1, 0), vec![C]);
        // Unsupported count: 7.1.4 WAVE-order prefix, then Unknown.
        assert_eq!(channel_labels(3, 0), vec![L, R, C]);
        assert_eq!(channel_labels(7, 0), vec![L, R, C, LFE, Lb, Rb, Ls]);
        assert_eq!(&channel_labels(14, 0)[12..], &[Unknown, Unknown]);
    }

    #[test]
    fn channel_mask_orders_backs_before_sides() {
        use RChannelLabel::*;
        // KSAUDIO_SPEAKER_7POINT1_SURROUND, what ffmpeg writes for 7.1:
        // FL FR FC LFE BL BR SL SR.
        assert_eq!(channel_labels(8, 0x63F), vec![L, R, C, LFE, Lb, Rb, Ls, Rs]);
        // 7.1.4: the four top channels follow, in bit order.
        assert_eq!(
            channel_labels(12, 0x2D63F),
            vec![L, R, C, LFE, Lb, Rb, Ls, Rs, Tfl, Tfr, Tbl, Tbr]
        );
    }

    #[test]
    fn channel_mask_five_one_surrounds() {
        use RChannelLabel::*;
        // 5.1(side) and the older back-pair 5.1 are both a 5.1's surrounds.
        assert_eq!(channel_labels(6, 0x60F), vec![L, R, C, LFE, Ls, Rs]);
        assert_eq!(channel_labels(6, 0x3F), vec![L, R, C, LFE, Ls, Rs]);
    }

    #[test]
    fn channel_mask_shorter_than_channel_count() {
        use RChannelLabel::*;
        // Stereo mask on four channels: the extra two have no position.
        assert_eq!(channel_labels(4, 0x3), vec![L, R, Unknown, Unknown]);
        // More positions than channels: only the first ones are used.
        assert_eq!(channel_labels(2, 0x63F), vec![L, R]);
    }

    /// A 16-bit `WAVE_FORMAT_EXTENSIBLE` file carrying `channel_mask`.
    fn write_extensible_wav(channels: u16, channel_mask: u32, frames: usize) -> Vec<u8> {
        let sample_rate = 48_000u32;
        let data = vec![0u8; frames * channels as usize * 2];
        let mut buf = Vec::new();
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&(60 + data.len() as u32).to_le_bytes());
        buf.extend_from_slice(b"WAVE");
        buf.extend_from_slice(b"fmt ");
        buf.extend_from_slice(&40u32.to_le_bytes());
        buf.extend_from_slice(&0xFFFEu16.to_le_bytes());
        buf.extend_from_slice(&channels.to_le_bytes());
        buf.extend_from_slice(&sample_rate.to_le_bytes());
        buf.extend_from_slice(&(sample_rate * channels as u32 * 2).to_le_bytes());
        buf.extend_from_slice(&(channels * 2).to_le_bytes());
        buf.extend_from_slice(&16u16.to_le_bytes());
        buf.extend_from_slice(&22u16.to_le_bytes()); // cbSize
        buf.extend_from_slice(&16u16.to_le_bytes()); // wValidBitsPerSample
        buf.extend_from_slice(&channel_mask.to_le_bytes());
        // KSDATAFORMAT_SUBTYPE_PCM: 00000001-0000-0010-8000-00aa00389b71
        buf.extend_from_slice(&[
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38,
            0x9B, 0x71,
        ]);
        buf.extend_from_slice(b"data");
        buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
        buf.extend_from_slice(&data);
        buf
    }

    #[test]
    fn extensible_file_is_labelled_from_its_channel_mask() {
        use RChannelLabel::*;
        let wav = write_extensible_wav(8, 0x63F, 4);
        let mut bridge = WavBridge::new(false);
        let r = bridge.push_packet(RSlice::from_slice(&wav), RInputTransport::Raw, 0);
        assert!(r.error_message.is_empty());
        let f = &r.frames[0];
        assert_eq!(f.channel_count, 8);
        assert_eq!(f.channel_labels.as_slice(), &[L, R, C, LFE, Lb, Rb, Ls, Rs]);
    }

    #[test]
    fn decodes_full_file_in_one_push() {
        let frames = vec![vec![100i16, -100], vec![200, -200], vec![300, -300]];
        let wav = write_wav(2, 48_000, &frames);
        let mut bridge = WavBridge::new(false);
        let result = bridge.push_packet(RSlice::from_slice(&wav), RInputTransport::Raw, 0);
        assert!(result.error_message.is_empty());
        assert!(bridge.is_ready());
        assert!(!bridge.has_objects());
        let total: u32 = result.frames.iter().map(|f| f.sample_count).sum();
        assert_eq!(total, 3);
        let f = &result.frames[0];
        assert_eq!(f.channel_count, 2);
        assert_eq!(f.sampling_frequency, 48_000);
        assert!(f.metadata.is_empty(), "bed frames must carry no metadata");
        // 16-bit value 100 → 24-bit scaled (<< 8).
        assert_eq!(f.pcm[0], 100 << 8);
        assert_eq!(f.pcm[1], -100 << 8);
    }

    #[test]
    fn decodes_across_byte_split_chunks() {
        let frames: Vec<Vec<i16>> = (0..50).map(|i| vec![i as i16, -(i as i16)]).collect();
        let wav = write_wav(2, 48_000, &frames);
        let mut bridge = WavBridge::new(false);
        let mut total = 0u32;
        // Feed 7 bytes at a time to exercise header/PCM straddling.
        for chunk in wav.chunks(7) {
            let r = bridge.push_packet(RSlice::from_slice(chunk), RInputTransport::Raw, 0);
            assert!(r.error_message.is_empty());
            total += r.frames.iter().map(|f| f.sample_count).sum::<u32>();
        }
        assert_eq!(total, 50);
    }

    /// A panic in a decode is caught at the boundary: the chunk counts as
    /// failed, the parser starts over, and nothing unwinds into the host.
    #[test]
    fn a_panic_in_a_decode_resets_the_parser_instead_of_unwinding() {
        let wav = write_wav(2, 48_000, &[vec![1i16, 2]]);
        let mut bridge = WavBridge::new(false);
        bridge.push_packet(RSlice::from_slice(&wav), RInputTransport::Raw, 0);
        assert!(matches!(bridge.state, State::Data { .. }));

        let r = bridge.recover_from_panic(|_| panic!("boom"));
        assert!(r.did_reset && r.frames.is_empty());
        assert!(
            r.error_message.is_empty(),
            "a non-strict bridge only resets"
        );
        assert!(matches!(bridge.state, State::Header));

        let mut strict = WavBridge::new(true);
        let r = strict.recover_from_panic(|_| panic!("boom {}", 2));
        assert!(r.did_reset);
        assert!(
            r.error_message.as_str().contains("boom 2"),
            "{}",
            r.error_message
        );

        // The bridge decodes again from the next file start.
        let r = bridge.push_packet(RSlice::from_slice(&wav), RInputTransport::Raw, 0);
        assert_eq!(r.frames.iter().map(|f| f.sample_count).sum::<u32>(), 1);
    }

    #[test]
    fn honours_declared_data_size() {
        // Append trailing bytes after the data chunk; they must not be decoded.
        let frames = vec![vec![1i16, 2], vec![3, 4]];
        let mut wav = write_wav(2, 48_000, &frames);
        wav.extend_from_slice(b"LIST\x04\x00\x00\x00junk");
        let mut bridge = WavBridge::new(false);
        let r = bridge.push_packet(RSlice::from_slice(&wav), RInputTransport::Raw, 0);
        let total: u32 = r.frames.iter().map(|f| f.sample_count).sum();
        assert_eq!(total, 2, "trailing chunk must not be read as PCM");
    }
}
