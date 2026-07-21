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
    FormatBridge, RChannelLabel, RCoordinateFormat, RDecodedFrame, RInputTransport, RMetadataFrame,
    RPushResult, RVbapCartesianDefaults, RVbapTableMode,
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
                self.labels = channel_labels(format.channels);
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

/// Map a channel count to canonical [`RChannelLabel`]s.
///
/// Known layouts use the conventional interleave order; any unrecognised count
/// labels as many leading channels as it can and marks the rest `Unknown` (still
/// rendered, just without a canonical position).
fn channel_labels(channel_count: u16) -> Vec<RChannelLabel> {
    use RChannelLabel::*;
    let canonical: &[RChannelLabel] = match channel_count {
        1 => &[C],
        2 => &[L, R],
        6 => &[L, R, C, LFE, Ls, Rs],
        8 => &[L, R, C, LFE, Ls, Rs, Lb, Rb],
        12 => &[L, R, C, LFE, Ls, Rs, Lb, Rb, Tfl, Tfr, Tbl, Tbr],
        _ => &[],
    };
    if !canonical.is_empty() {
        return canonical.to_vec();
    }
    // Best-effort fallback for unsupported counts.
    const BEST_EFFORT: &[RChannelLabel] = &[L, R, C, LFE, Ls, Rs, Lb, Rb, Tfl, Tfr, Tbl, Tbr];
    (0..channel_count as usize)
        .map(|i| {
            BEST_EFFORT
                .get(i)
                .copied()
                .unwrap_or(RChannelLabel::Unknown)
        })
        .collect()
}

impl FormatBridge for WavBridge {
    fn push_packet(
        &mut self,
        data: RSlice<'_, u8>,
        _transport: RInputTransport,
        _data_type: u8,
    ) -> RPushResult {
        let mut result = RPushResult {
            frames: RVec::new(),
            error_message: RString::new(),
            did_reset: false,
        };

        // The bridge is byte-stream oriented; both Raw and any extracted payload
        // are simply appended. (The orender file-decode path always uses Raw.)
        self.buf.extend_from_slice(data.as_slice());

        if matches!(self.state, State::Header) && !self.try_parse_header(&mut result) {
            return result;
        }
        self.drain_pcm(&mut result);
        result
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
        // Balanced default grid, matching the production bridge's hint.
        RVbapCartesianDefaults {
            x_size: 62,
            y_size: 62,
            z_size: 15,
            allow_negative_z: false,
        }
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
        assert_eq!(channel_labels(2), vec![L, R]);
        assert_eq!(channel_labels(6), vec![L, R, C, LFE, Ls, Rs]);
        assert_eq!(
            channel_labels(12),
            vec![L, R, C, LFE, Ls, Rs, Lb, Rb, Tfl, Tfr, Tbl, Tbr]
        );
        // Unsupported count: best-effort prefix then Unknown.
        let three = channel_labels(3);
        assert_eq!(three, vec![L, R, C]);
        let seven = channel_labels(7);
        assert_eq!(&seven[..6], &[L, R, C, LFE, Ls, Rs]);
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
