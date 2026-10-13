//! IEC 61937 words carried as PCM float samples
//! (`docs/iamf-transport-plan.md`).
//!
//! A link that only carries linear PCM (a PipeWire graph, a netjack2 link)
//! can carry a burst stream all the same: each 16-bit word travels as the
//! sample it would be, `word / 32768`. A 32-bit float holds every 16-bit
//! value exactly, so the words come back as they left as long as nothing on
//! the way changes the samples: no volume, no mixing, no resampling, no
//! dither.
//!
//! This is the receiving end: floats back to words, and telling a carrier
//! from audio.

use crate::burst::{BURST_HEADER_BYTES, DATA_TYPE_IAMF_PRIVATE, iamf_payload_bytes};
use crate::parser::{SYNCWORD_PA, SYNCWORD_PB};

/// The float a 16-bit word travels as is the word over this.
const WORD_SCALE: f32 = 32768.0;

/// Bytes of one float sample.
pub const SAMPLE_BYTES: usize = 4;

/// The float a word travels as.
fn word_sample(word: u16) -> f32 {
    f32::from(word as i16) / WORD_SCALE
}

/// Append to `words` the 16-bit little-endian words the `f32` little-endian
/// `samples` carry. Returns how many of them were not a 16-bit word: a
/// sample that something scaled, mixed or resampled on the way. Such a
/// sample is rounded to the nearest word; the stream it is part of is
/// damaged, and the count is how a receiver knows.
///
/// Bytes past the last whole sample are left out.
pub fn words_from_f32le(samples: &[u8], words: &mut Vec<u8>) -> usize {
    let (samples, _) = samples.as_chunks::<SAMPLE_BYTES>();
    words.reserve(samples.len() * 2);
    let mut off_grid = 0;
    for bytes in samples {
        let scaled = f32::from_le_bytes(*bytes) * WORD_SCALE;
        let rounded = scaled.round();
        // (A NaN is neither equal to its rounding nor in range.)
        if rounded != scaled || !(-WORD_SCALE..WORD_SCALE).contains(&rounded) {
            off_grid += 1;
        }
        // `as` saturates, and takes a NaN to 0.
        words.extend_from_slice(&(rounded as i16).to_le_bytes());
    }
    off_grid
}

/// The IAMF bursts that start in `samples` (`f32` little-endian, `channels`
/// to a frame): for each, the frame it starts on and the bytes it takes on
/// the carrier, preamble and payload (an odd payload's pad byte with it).
///
/// A burst starts on a frame boundary, with Pa, Pb, a Pc of the private IAMF
/// data type with no error flag, and Pd, as the first four words of the
/// frame. A frame of less than four channels cannot be told this way: none
/// is found on one.
///
/// Sixteen-bit audio matches by chance once in 2^48 frames a channel pair.
pub fn iamf_bursts(samples: &[u8], channels: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
    let pa = word_sample(SYNCWORD_PA).to_le_bytes();
    let pb = word_sample(SYNCWORD_PB).to_le_bytes();
    let frame_bytes = channels * SAMPLE_BYTES;
    samples
        .chunks_exact(frame_bytes.max(1))
        .enumerate()
        .filter(move |(_, frame)| {
            channels >= 4
                && frame[..SAMPLE_BYTES] == pa
                && frame[SAMPLE_BYTES..2 * SAMPLE_BYTES] == pb
        })
        .filter_map(|(index, frame)| {
            let [data_type, pc_high] = word_at(frame, 2)?.to_le_bytes();
            // The private data type, subdata type 0, no error flag,
            // bitstream 0; the rest of the high byte is the top of the
            // payload length.
            if data_type != DATA_TYPE_IAMF_PRIVATE || pc_high & 0xE0 != 0 {
                return None;
            }
            let payload = iamf_payload_bytes(pc_high, word_at(frame, 3)?);
            Some((index, BURST_HEADER_BYTES + payload + payload % 2))
        })
}

/// The 16-bit word the `index`-th sample of `frame` carries, if it is one.
fn word_at(frame: &[u8], index: usize) -> Option<u16> {
    let bytes = frame.get(index * SAMPLE_BYTES..(index + 1) * SAMPLE_BYTES)?;
    let scaled = f32::from_le_bytes(bytes.try_into().ok()?) * WORD_SCALE;
    (scaled.round() == scaled && (-WORD_SCALE..WORD_SCALE).contains(&scaled))
        .then_some(scaled as i16 as u16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::burst::write_iamf_burst;

    /// The samples a carrier of 16-bit words is on a float link.
    fn as_f32le(words: &[u8]) -> Vec<u8> {
        words
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|w| word_sample(u16::from_le_bytes(*w)).to_le_bytes())
            .collect()
    }

    /// Every 16-bit word survives the trip through a float sample.
    #[test]
    fn every_word_comes_back() {
        let words: Vec<u8> = (0..=u16::MAX).flat_map(u16::to_le_bytes).collect();
        let mut back = Vec::new();
        assert_eq!(words_from_f32le(&as_f32le(&words), &mut back), 0);
        assert_eq!(back, words);
    }

    /// A sample that is not a word is counted and rounded; one out of range
    /// is clamped; a partial sample at the end is left out.
    #[test]
    fn a_sample_off_the_grid_is_counted() {
        let scaled: Vec<u8> = [0.5f32, 0.36 * 0.5, 1.0, -1.0, f32::NAN, 0.0]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .chain([1, 2, 3])
            .collect();
        let mut words = Vec::new();
        // 0.18 is no word, 1.0 is past the top one, and NaN is nothing.
        assert_eq!(words_from_f32le(&scaled, &mut words), 3);
        let words: Vec<i16> = words
            .as_chunks::<2>()
            .0
            .iter()
            .map(|w| i16::from_le_bytes(*w))
            .collect();
        assert_eq!(words, [16384, 5898, i16::MAX, i16::MIN, 0, 0]);
    }

    /// A burst is found where it starts, on a frame boundary, by its data
    /// type, with the bytes it takes; another data type, a preamble across a
    /// boundary and audio are not.
    #[test]
    fn an_iamf_burst_is_found_on_its_frame() {
        const CHANNELS: usize = 8;
        let period = 64 * CHANNELS * 2;
        let mut carrier = vec![0u8; period];
        write_iamf_burst(&mut carrier, &[0xAA; 101], period).unwrap();
        write_iamf_burst(&mut carrier, &[0xBB; 70_000], 8192 * CHANNELS * 2).unwrap();
        let samples = as_f32le(&carrier);
        assert_eq!(
            iamf_bursts(&samples, CHANNELS).collect::<Vec<_>>(),
            // (An odd payload is followed by its pad byte.)
            [(64, 8 + 102), (128, 8 + 70_000)]
        );

        // The same words one sample late: no frame starts with them.
        let mut shifted = 0.25f32.to_le_bytes().to_vec();
        shifted.extend_from_slice(&samples);
        assert_eq!(iamf_bursts(&shifted, CHANNELS).count(), 0);

        // Another data type (AC-3) is not this carrier's.
        let mut ac3 = carrier.clone();
        ac3[period + 4] = 0x01;
        assert_eq!(
            iamf_bursts(&as_f32le(&ac3), CHANNELS).collect::<Vec<_>>(),
            [(128, 8 + 70_000)]
        );

        // The words at another level are not words any more.
        let quieter: Vec<u8> = samples
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|s| (f32::from_le_bytes(*s) * 0.36).to_le_bytes())
            .collect();
        assert_eq!(iamf_bursts(&quieter, CHANNELS).count(), 0);

        // Too narrow to hold the preamble in a frame.
        assert_eq!(iamf_bursts(&samples, 2).count(), 0);
    }
}
