//! Loading a WAV file to use as the object test's signal.
//!
//! Everything here happens **off the audio thread**: a clip is read, downmixed,
//! resampled and peak-normalised once, when the file is chosen, and the render
//! path then does nothing but walk an array. File I/O on the render thread would
//! be a dropout waiting for a cold cache.
//!
//! The reader is deliberately small — canonical RIFF/WAVE, PCM and IEEE float,
//! including `WAVE_FORMAT_EXTENSIBLE`. That is what a test signal arrives as. A
//! compressed or exotic file is refused with a message rather than half-read;
//! the point of the feature is to hear a known signal, so a file that cannot be
//! read exactly should not be guessed at.

use std::path::Path;

/// Longest clip kept, in seconds. A test that outlives the safety cap cannot be
/// heard anyway, and the array is resident in the renderer for as long as it is
/// loaded.
const MAX_SECONDS: usize = 120;

/// A file loaded and ready to play: mono, at the render rate, peak-normalised.
///
/// Peak-normalised so the level control keeps meaning what it means everywhere
/// else — the injected peak is exactly `level`, whatever the file was mastered
/// at. Loudness differences between clips survive normalisation, which is
/// correct: that is the file's character, not the test's calibration.
pub struct ObjectTestClip {
    /// Where it came from, for the UI to show and for change detection.
    pub path: String,
    /// Mono samples at the render sample rate, peak 1.0.
    pub samples: Vec<f32>,
    /// Rate the samples are at — the render rate they were resampled to.
    pub sample_rate: u32,
    /// What the file itself was, for the UI.
    pub source_rate: u32,
    pub source_channels: u16,
    /// True when the tail was dropped at [`MAX_SECONDS`].
    pub truncated: bool,
}

impl ObjectTestClip {
    pub fn duration_s(&self) -> f32 {
        self.samples.len() as f32 / self.sample_rate.max(1) as f32
    }
}

/// Read `path` and prepare it for playback at `target_rate`.
pub fn load(path: &str, target_rate: u32) -> Result<ObjectTestClip, String> {
    let bytes = std::fs::read(Path::new(path)).map_err(|e| format!("{path}: {e}"))?;
    let wav = parse_wav(&bytes)?;
    let mono = downmix(&wav.samples, wav.channels);
    let resampled = if wav.sample_rate == target_rate {
        mono
    } else {
        resample(&mono, wav.sample_rate, target_rate)
    };
    let max_len = MAX_SECONDS * target_rate.max(1) as usize;
    let truncated = resampled.len() > max_len;
    let mut samples = resampled;
    if truncated {
        samples.truncate(max_len);
    }
    if samples.is_empty() {
        return Err(format!("{path}: no audio samples"));
    }
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak <= 0.0 {
        return Err(format!("{path}: silent"));
    }
    let norm = 1.0 / peak;
    for s in samples.iter_mut() {
        *s *= norm;
    }
    Ok(ObjectTestClip {
        path: path.to_string(),
        samples,
        sample_rate: target_rate,
        source_rate: wav.sample_rate,
        source_channels: wav.channels,
        truncated,
    })
}

struct Wav {
    samples: Vec<f32>,
    channels: u16,
    sample_rate: u32,
}

fn u16le(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn parse_wav(b: &[u8]) -> Result<Wav, String> {
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".to_string());
    }
    let mut pos = 12usize;
    let mut fmt: Option<(u16, u16, u32, u16)> = None; // (tag, channels, rate, bits)
    let mut data: Option<&[u8]> = None;
    while pos + 8 <= b.len() {
        let id = &b[pos..pos + 4];
        let size = u32le(b, pos + 4) as usize;
        let body_at = pos + 8;
        let body_end = body_at.saturating_add(size).min(b.len());
        if id == b"fmt " && body_end - body_at >= 16 {
            let f = &b[body_at..body_end];
            let mut tag = u16le(f, 0);
            let channels = u16le(f, 2);
            let rate = u32le(f, 4);
            let bits = u16le(f, 14);
            // WAVE_FORMAT_EXTENSIBLE hides the real format in the sub-format
            // GUID, whose first two little-endian bytes are the effective tag.
            if tag == 0xFFFE && f.len() >= 26 {
                tag = u16le(f, 24);
            }
            fmt = Some((tag, channels, rate, bits));
        } else if id == b"data" {
            data = Some(&b[body_at..body_end]);
        }
        // Chunks are word-aligned: an odd size is followed by a pad byte.
        pos = body_at + size + (size & 1);
    }
    let (tag, channels, sample_rate, bits) = fmt.ok_or("no fmt chunk")?;
    let data = data.ok_or("no data chunk")?;
    if channels == 0 || sample_rate == 0 {
        return Err("fmt chunk declares no channels or no sample rate".to_string());
    }
    let samples = match (tag, bits) {
        (1, 8) => data.iter().map(|&v| (v as f32 - 128.0) / 128.0).collect(),
        (1, 16) => data
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32_768.0)
            .collect(),
        (1, 24) => data
            .chunks_exact(3)
            .map(|c| {
                // Sign-extend by putting the three bytes in the top of an i32.
                let v = i32::from_le_bytes([0, c[0], c[1], c[2]]);
                v as f32 / 2_147_483_648.0
            })
            .collect(),
        (1, 32) => data
            .chunks_exact(4)
            .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f32 / 2_147_483_648.0)
            .collect(),
        (3, 32) => data
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
        (3, 64) => data
            .chunks_exact(8)
            .map(|c| f64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]) as f32)
            .collect(),
        _ => {
            return Err(format!(
                "unsupported WAVE format (tag {tag}, {bits}-bit) — PCM 8/16/24/32 \
                 and float 32/64 only"
            ));
        }
    };
    Ok(Wav {
        samples,
        channels,
        sample_rate,
    })
}

/// Average the channels. A test object is a point source, so it gets one
/// signal; averaging rather than taking channel 0 keeps whatever was panned
/// across the front from disappearing.
fn downmix(interleaved: &[f32], channels: u16) -> Vec<f32> {
    let n = channels.max(1) as usize;
    if n == 1 {
        return interleaved.to_vec();
    }
    let scale = 1.0 / n as f32;
    interleaved
        .chunks_exact(n)
        .map(|f| f.iter().sum::<f32>() * scale)
        .collect()
}

/// Windowed-sinc resampling to the render rate.
///
/// Offline, so the quality is worth paying for: linear interpolation of 44.1 →
/// 48 kHz folds audible rubbish into exactly the top octaves that carry the
/// spectral cues this test exists to judge. A 32-tap Blackman-windowed sinc
/// costs a fraction of a second on a clip and leaves them alone.
fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    const HALF_TAPS: i64 = 16;
    if input.is_empty() || from == 0 || to == 0 || from == to {
        return input.to_vec();
    }
    let ratio = to as f64 / from as f64;
    // Downsampling has to lower the cutoff to the new Nyquist or it aliases.
    let cutoff = if ratio < 1.0 { ratio } else { 1.0 };
    let out_len = ((input.len() as f64) * ratio).floor() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src = i as f64 / ratio;
        let base = src.floor() as i64;
        let frac = src - base as f64;
        let mut acc = 0.0f64;
        let mut norm = 0.0f64;
        for k in -HALF_TAPS..HALF_TAPS {
            let idx = base + k;
            if idx < 0 || idx as usize >= input.len() {
                continue;
            }
            let x = k as f64 - frac;
            let sinc = if x.abs() < 1e-9 {
                cutoff
            } else {
                (std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * x)
            };
            // Blackman window over the tap span.
            let t = (x + HALF_TAPS as f64) / (2.0 * HALF_TAPS as f64);
            let w = 0.42 - 0.5 * (std::f64::consts::TAU * t).cos()
                + 0.08 * (2.0 * std::f64::consts::TAU * t).cos();
            let h = sinc * w;
            acc += input[idx as usize] as f64 * h;
            norm += h;
        }
        // Normalising by the realised window keeps the gain flat at the edges,
        // where part of the kernel hangs off the end of the input.
        out.push(if norm.abs() > 1e-12 {
            (acc / norm) as f32
        } else {
            0.0
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a canonical 16-bit PCM WAV in memory.
    fn wav16(channels: u16, rate: u32, frames: &[Vec<i16>]) -> Vec<u8> {
        let mut data = Vec::new();
        for f in frames {
            for s in f {
                data.extend_from_slice(&s.to_le_bytes());
            }
        }
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36u32 + data.len() as u32).to_le_bytes());
        b.extend_from_slice(b"WAVE");
        b.extend_from_slice(b"fmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes()); // PCM
        b.extend_from_slice(&channels.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
        b.extend_from_slice(&(channels * 2).to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(data.len() as u32).to_le_bytes());
        b.extend_from_slice(&data);
        b
    }

    #[test]
    fn reads_a_canonical_16_bit_file() {
        let bytes = wav16(2, 48_000, &[vec![16_384, -16_384], vec![8_192, -8_192]]);
        let w = parse_wav(&bytes).expect("parse");
        assert_eq!(w.channels, 2);
        assert_eq!(w.sample_rate, 48_000);
        assert_eq!(w.samples.len(), 4);
        assert!((w.samples[0] - 0.5).abs() < 1e-3);
    }

    /// A stereo file must average, not take the first channel: a source panned
    /// hard right would otherwise load as silence.
    #[test]
    fn downmix_averages_the_channels() {
        let mono = downmix(&[0.0, 1.0, 0.0, 1.0], 2);
        assert_eq!(mono, vec![0.5, 0.5]);
    }

    /// Resampling must preserve a tone's frequency and amplitude, which is the
    /// property a localisation test depends on — the spectral cues are the
    /// signal.
    #[test]
    fn resampling_preserves_a_tone() {
        let from = 44_100u32;
        let to = 48_000u32;
        let f = 1_000.0f64;
        let input: Vec<f32> = (0..from as usize)
            .map(|i| (std::f64::consts::TAU * f * i as f64 / from as f64).sin() as f32)
            .collect();
        let out = resample(&input, from, to);
        assert!(
            (out.len() as i64 - to as i64).abs() < 4,
            "expected ~{to} samples, got {}",
            out.len()
        );
        // Amplitude held (away from the edges, where the kernel hangs off).
        let peak = out[1000..out.len() - 1000]
            .iter()
            .fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((peak - 1.0).abs() < 0.02, "peak drifted to {peak}");
        // Frequency held: count zero crossings over the steady middle.
        let mid = &out[1000..out.len() - 1000];
        let crossings = mid.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        let seconds = mid.len() as f64 / to as f64;
        let measured = crossings as f64 / seconds;
        assert!(
            (measured - f).abs() < 5.0,
            "tone came out at {measured} Hz instead of {f}"
        );
    }

    #[test]
    fn a_file_that_is_not_a_wav_is_refused_rather_than_guessed_at() {
        assert!(parse_wav(b"not a wav at all").is_err());
    }
}
