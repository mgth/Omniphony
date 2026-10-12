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

use crate::binaural::measured::ResampleKernel;

/// Longest clip kept, in seconds. A test that outlives the safety cap cannot be
/// heard anyway, and the array is resident in the renderer for as long as it is
/// loaded.
const MAX_SECONDS: usize = 120;

/// Input samples kept past the cap so the resampler's kernel still has its
/// support at the last output sample it produces.
const RESAMPLE_MARGIN: usize = 64;

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
    /// True when the tail was dropped at `MAX_SECONDS`.
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
    from_bytes(path, &bytes, target_rate)
}

/// Cut `mono` at the cap, in its own rate, before it is resampled; true when
/// something was dropped.
///
/// Cutting only after resampling is not enough: the output is the input times
/// target/source, and a header declaring a rate of a few hertz would have a
/// short file expand to gigabytes first. The kernel's width of extra input
/// keeps the last sample kept after resampling exact.
fn cut_before_resampling(mut mono: Vec<f32>, source_rate: u32) -> (Vec<f32>, bool) {
    // The rate is the file's own u32: on a 32-bit target 120 s of a header
    // claiming tens of megahertz would wrap a plain multiplication.
    let cap = MAX_SECONDS
        .saturating_mul(source_rate as usize)
        .saturating_add(RESAMPLE_MARGIN);
    let truncated = mono.len() > cap;
    mono.truncate(cap);
    (mono, truncated)
}

/// [`load`] on the file's bytes; `path` only names it.
fn from_bytes(path: &str, bytes: &[u8], target_rate: u32) -> Result<ObjectTestClip, String> {
    let wav = parse_wav(bytes)?;
    let mono = downmix(&wav.samples, wav.channels);
    let (mono, mut truncated) = cut_before_resampling(mono, wav.sample_rate);
    let mut samples = if wav.sample_rate == target_rate {
        mono
    } else {
        resample(&mono, wav.sample_rate, target_rate)
    };
    let max_len = MAX_SECONDS * target_rate.max(1) as usize;
    if samples.len() > max_len {
        truncated = true;
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

fn finite_or_silence(v: f32) -> f32 {
    if v.is_finite() { v } else { 0.0 }
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
        // A float file can hold NaN or infinities; they read as silence; else
        // one would survive the peak normalisation (f32::max skips NaN) and
        // reach the render, or an infinite peak would zero the whole clip.
        (3, 32) => data
            .chunks_exact(4)
            .map(|c| finite_or_silence(f32::from_le_bytes([c[0], c[1], c[2], c[3]])))
            .collect(),
        (3, 64) => data
            .chunks_exact(8)
            .map(|c| {
                finite_or_silence(f64::from_le_bytes([
                    c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7],
                ]) as f32)
            })
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
/// spectral cues this test exists to judge. The kernel is the measured-HRIR
/// one (32-tap Blackman-windowed sinc, low-passed at the lower Nyquist, taps
/// tabulated per phase), normalised by the taps that land on the clip so the
/// gain stays flat at its edges.
fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if input.is_empty() || from == 0 || to == 0 || from == to {
        return input.to_vec();
    }
    let mut out = Vec::new();
    ResampleKernel::new(from, to).resample_normalized_into(input, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The clip loader's own resampler before it moved onto `ResampleKernel`,
    /// kept as the reference the shared kernel is checked against.
    ///
    /// Windowed-sinc resampling to the render rate.
    ///
    /// Offline, so the quality is worth paying for: linear interpolation of 44.1 →
    /// 48 kHz folds audible rubbish into exactly the top octaves that carry the
    /// spectral cues this test exists to judge. A 32-tap Blackman-windowed sinc
    /// costs a fraction of a second on a clip and leaves them alone.
    fn reference_resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
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

    /// The shared kernel reproduces the clip loader's former resampler within
    /// a hair: same window, cutoff and tap count; it differs only in the
    /// kernel's centring (symmetric around the output position instead of one
    /// tap early), its integer phase stepping, and rounding the length rather
    /// than truncating it. Offline test-signal loading, so that is fine.
    #[test]
    fn shared_kernel_matches_the_former_clip_resampler() {
        for (from, to) in [(44_100u32, 48_000u32), (96_000, 48_000), (22_050, 48_000)] {
            let len = from as usize / 2;
            let input: Vec<f32> = (0..len)
                .map(|i| {
                    let t = i as f64 / from as f64;
                    (0.5 * (std::f64::consts::TAU * 997.0 * t).sin()
                        + 0.25 * (std::f64::consts::TAU * 5_003.0 * t).sin())
                        as f32
                })
                .collect();
            let old = reference_resample(&input, from, to);
            let new = resample(&input, from, to);
            assert!(
                (old.len() as i64 - new.len() as i64).abs() <= 1,
                "{from}->{to}: lengths {} vs {}",
                old.len(),
                new.len()
            );
            let n = old.len().min(new.len());
            let max_diff = old[..n]
                .iter()
                .zip(&new[..n])
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max);
            eprintln!("{from}->{to}: max difference {max_diff:e}");
            assert!(max_diff < 2e-4, "{from}->{to}: max difference {max_diff}");
        }
    }

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

    /// A WAV of `chunks` (id, body) after the RIFF/WAVE header, each padded to
    /// an even size as the format requires.
    fn riff(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut body = b"WAVE".to_vec();
        for (id, data) in chunks {
            body.extend_from_slice(*id);
            body.extend_from_slice(&(data.len() as u32).to_le_bytes());
            body.extend_from_slice(data);
            if data.len() % 2 == 1 {
                body.push(0);
            }
        }
        let mut b = b"RIFF".to_vec();
        b.extend_from_slice(&(body.len() as u32).to_le_bytes());
        b.extend_from_slice(&body);
        b
    }

    /// A 16-byte `fmt ` body.
    fn fmt(tag: u16, channels: u16, rate: u32, bits: u16) -> Vec<u8> {
        let align = channels * bits / 8;
        let mut f = Vec::new();
        f.extend_from_slice(&tag.to_le_bytes());
        f.extend_from_slice(&channels.to_le_bytes());
        f.extend_from_slice(&rate.to_le_bytes());
        f.extend_from_slice(&(rate * align as u32).to_le_bytes());
        f.extend_from_slice(&align.to_le_bytes());
        f.extend_from_slice(&bits.to_le_bytes());
        f
    }

    fn error_of(bytes: &[u8]) -> String {
        match parse_wav(bytes) {
            Ok(_) => panic!("the file was accepted"),
            Err(e) => e,
        }
    }

    #[test]
    fn a_malformed_header_is_refused_with_its_reason() {
        let pcm = vec![0u8; 8];
        assert!(error_of(&riff(&[(b"data", pcm.clone())])).contains("no fmt chunk"));
        assert!(error_of(&riff(&[(b"fmt ", fmt(1, 1, 48_000, 16))])).contains("no data chunk"));
        let zero_channels = riff(&[(b"fmt ", fmt(1, 0, 48_000, 16)), (b"data", pcm.clone())]);
        assert!(error_of(&zero_channels).contains("no channels or no sample rate"));
        let zero_rate = riff(&[(b"fmt ", fmt(1, 1, 0, 16)), (b"data", pcm.clone())]);
        assert!(error_of(&zero_rate).contains("no channels or no sample rate"));
        for (tag, bits) in [(1, 12), (3, 16), (2, 16), (0x55, 0)] {
            let file = riff(&[(b"fmt ", fmt(tag, 1, 48_000, bits)), (b"data", pcm.clone())]);
            assert!(
                error_of(&file).contains("unsupported WAVE format"),
                "tag {tag}, {bits}-bit"
            );
        }
        // A fmt chunk too short to hold a format is no fmt chunk at all.
        let short = riff(&[(b"fmt ", vec![1, 0, 1, 0]), (b"data", pcm)]);
        assert!(error_of(&short).contains("no fmt chunk"));
    }

    #[test]
    fn odd_chunks_are_skipped_with_their_pad_byte() {
        let pcm: Vec<u8> = [16_384i16, -16_384]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let file = riff(&[
            (b"LIST", vec![7; 5]),
            (b"fmt ", fmt(1, 1, 48_000, 16)),
            (b"data", pcm),
        ]);
        let w = parse_wav(&file).expect("parse");
        assert_eq!(w.samples.len(), 2);
        assert!((w.samples[0] - 0.5).abs() < 1e-3 && (w.samples[1] + 0.5).abs() < 1e-3);
    }

    #[test]
    fn a_data_chunk_longer_than_the_file_reads_what_is_there() {
        let mut file = riff(&[
            (b"fmt ", fmt(1, 1, 48_000, 16)),
            (b"data", vec![0, 64, 0, 64]),
        ]);
        // Claim far more data than follows, as a truncated download would.
        let at = file.len() - 4 - 4;
        file[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let w = parse_wav(&file).expect("parse");
        assert_eq!(w.samples.len(), 2);
        // A trailing partial sample is dropped, not read past the end.
        file.push(0x40);
        assert_eq!(parse_wav(&file).expect("parse").samples.len(), 2);
    }

    #[test]
    fn non_finite_float_samples_read_as_silence() {
        let samples = [0.5f32, f32::NAN, f32::INFINITY, -0.25, f32::NEG_INFINITY];
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let file = riff(&[(b"fmt ", fmt(3, 1, 48_000, 32)), (b"data", data)]);
        let clip = from_bytes("nan.wav", &file, 48_000).expect("load");
        assert!(
            clip.samples.iter().all(|s| s.is_finite()),
            "{:?}",
            clip.samples
        );
        // The finite samples keep their shape: peak-normalised to 1.0.
        assert_eq!(clip.samples, vec![1.0, 0.0, 0.0, -0.5, 0.0]);

        let big = [1e300f64, -0.5];
        let data: Vec<u8> = big.iter().flat_map(|s| s.to_le_bytes()).collect();
        let file = riff(&[(b"fmt ", fmt(3, 1, 48_000, 64)), (b"data", data)]);
        let clip = from_bytes("big.wav", &file, 48_000).expect("load");
        assert_eq!(
            clip.samples,
            vec![0.0, -1.0],
            "out of f32 range reads as silence"
        );
    }

    #[test]
    fn an_all_invalid_or_silent_file_is_refused() {
        let data: Vec<u8> = [f32::NAN; 4].iter().flat_map(|s| s.to_le_bytes()).collect();
        let file = riff(&[(b"fmt ", fmt(3, 1, 48_000, 32)), (b"data", data)]);
        assert!(
            from_bytes("nan.wav", &file, 48_000)
                .err()
                .unwrap()
                .contains("silent")
        );
        let file = riff(&[(b"fmt ", fmt(1, 1, 48_000, 16)), (b"data", Vec::new())]);
        assert!(
            from_bytes("empty.wav", &file, 48_000)
                .err()
                .unwrap()
                .contains("no audio")
        );
    }

    /// A header declaring a rate of a few hertz must not make a small file
    /// expand past the cap while resampling: the input is cut first.
    #[test]
    fn a_tiny_declared_rate_is_cut_before_resampling() {
        let source_rate = 2u32;
        // Ten times the cap at the declared rate.
        let frames = MAX_SECONDS * source_rate as usize * 10;
        let data: Vec<u8> = (0..frames).map(|i| (i % 255) as u8 + 1).collect();
        let file = riff(&[(b"fmt ", fmt(1, 1, source_rate, 8)), (b"data", data)]);
        let clip = from_bytes("slow.wav", &file, 48_000).expect("load");
        assert!(clip.truncated);
        assert_eq!(clip.samples.len(), MAX_SECONDS * 48_000);

        // What reaches the resampler: the cap in the source's own rate.
        let (cut, truncated) = cut_before_resampling(vec![0.0; frames], source_rate);
        assert!(truncated);
        assert_eq!(
            cut.len(),
            MAX_SECONDS * source_rate as usize + RESAMPLE_MARGIN
        );
        let (kept, truncated) = cut_before_resampling(vec![0.0; 10], 48_000);
        assert!(!truncated && kept.len() == 10, "a short clip is left whole");
        // The largest rate a header can declare saturates the cap instead of
        // wrapping it (it would wrap on a 32-bit target).
        let (kept, truncated) = cut_before_resampling(vec![0.0; 10], u32::MAX);
        assert!(!truncated && kept.len() == 10);
    }
}
