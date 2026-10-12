//! Deterministic generator for the committed demo asset.
//!
//! Synthesises a short 7.1.4 (12-channel) 48 kHz WAV in which a tone rotates
//! around the ear-level ring (L → C → R → Rs → Rb → Lb → Ls → L) while a
//! distinct steady tone sits in the two top-front height channels. Rendered
//! through the reference bridge + orender's binaural stage, the moving source
//! sweeps around the listener and the height tone stays clearly "above and in
//! front" — an obvious spatial demonstration with no proprietary content.
//!
//! Run with:
//!   cargo run -r -p reference_bridge --example gen_demo_wav
//! Output defaults to `omniphony-renderer/assets/demo/spatial-demo.wav`; pass a
//! path argument to override.

use std::f32::consts::PI;
use std::io::Write;
use std::path::PathBuf;

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: usize = 12; // 7.1.4
const DURATION_S: f32 = 2.5; // trimmed to keep the committed WAV < ~3 MB
const ROTATIONS: f32 = 2.0; // full ring revolutions over the clip
const RING_FREQ_HZ: f32 = 440.0;
const HEIGHT_FREQ_HZ: f32 = 660.0;
const AMP_RING: f32 = 0.5;
const AMP_HEIGHT: f32 = 0.22;
const FADE_S: f32 = 0.05;

// 7.1.4 in the WAVE order (the `dwChannelMask` bit order: backs before sides):
// 0=L 1=R 2=C 3=LFE 4=Lb 5=Rb 6=Ls 7=Rs 8=Tfl 9=Tfr 10=Tbl 11=Tbr
/// FL FR FC LFE BL BR SL SR TFL TFR TBL TBR, written in the header so the file
/// says which speaker each channel feeds (any tool reads it the same way).
const CHANNEL_MASK: u32 = 0x2D63F;
const CH_TFL: usize = 8;
const CH_TFR: usize = 9;

/// Ear-level ring as (channel index, azimuth in degrees), ascending in [0,360).
/// 0° = front, increasing clockwise toward the right.
const RING: &[(usize, f32)] = &[
    (2, 0.0),   // C   front
    (1, 30.0),  // R   front-right
    (7, 90.0),  // Rs  side-right
    (5, 150.0), // Rb  back-right
    (4, 210.0), // Lb  back-left
    (6, 270.0), // Ls  side-left
    (0, 330.0), // L   front-left
];

fn main() {
    let out_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../assets/demo/spatial-demo.wav")
        });

    let total_frames = (DURATION_S * SAMPLE_RATE as f32) as usize;
    let fade_frames = (FADE_S * SAMPLE_RATE as f32) as usize;
    let mut samples = vec![0.0f32; total_frames * CHANNELS];

    for n in 0..total_frames {
        let t = n as f32 / SAMPLE_RATE as f32;

        // Smooth fade-in/out envelope to avoid clicks at the boundaries.
        let env = {
            let fin = (n as f32 / fade_frames as f32).min(1.0);
            let fout = ((total_frames - n) as f32 / fade_frames as f32).min(1.0);
            fin.min(fout)
        };

        // Rotating ring source: equal-power crossfade between the two ring
        // speakers adjacent to the current azimuth.
        let theta = (t / DURATION_S * ROTATIONS * 360.0).rem_euclid(360.0);
        let tone = (2.0 * PI * RING_FREQ_HZ * t).sin() * AMP_RING * env;
        let (lo_ch, lo_az, hi_ch, hi_az) = ring_segment(theta);
        let phase = (theta - lo_az) / (hi_az - lo_az);
        let g_lo = (phase * PI / 2.0).cos();
        let g_hi = (phase * PI / 2.0).sin();
        samples[n * CHANNELS + lo_ch] += g_lo * tone;
        samples[n * CHANNELS + hi_ch] += g_hi * tone;

        // Steady height source in the two top-front channels.
        let h = (2.0 * PI * HEIGHT_FREQ_HZ * t).sin() * AMP_HEIGHT * env;
        samples[n * CHANNELS + CH_TFL] += h;
        samples[n * CHANNELS + CH_TFR] += h;
    }

    write_wav16(
        &out_path,
        SAMPLE_RATE,
        CHANNELS as u16,
        CHANNEL_MASK,
        &samples,
    )
    .unwrap_or_else(|e| panic!("failed to write {}: {e}", out_path.display()));

    let bytes = HEADER_BYTES + samples.len() * 2;
    println!(
        "wrote {} ({} frames, {} ch, {} Hz, {:.2}s, {} bytes)",
        out_path.display(),
        total_frames,
        CHANNELS,
        SAMPLE_RATE,
        DURATION_S,
        bytes
    );
}

/// Return `(lo_ch, lo_az, hi_ch, hi_az)` for the ring segment containing
/// `theta` (degrees), wrapping the last segment back to the first speaker.
fn ring_segment(theta: f32) -> (usize, f32, usize, f32) {
    for i in 0..RING.len() {
        let (lo_ch, lo_az) = RING[i];
        let (hi_ch, hi_az_raw) = RING[(i + 1) % RING.len()];
        // The wrap segment spans from the last azimuth to first + 360.
        let hi_az = if i + 1 == RING.len() {
            hi_az_raw + 360.0
        } else {
            hi_az_raw
        };
        if theta >= lo_az && theta < hi_az {
            return (lo_ch, lo_az, hi_ch, hi_az);
        }
    }
    // theta in [last_az, 360): handled by the wrap segment above; fall back to
    // the first speaker for any numerical edge case.
    let (ch, az) = RING[0];
    (ch, az, ch, az + 360.0)
}

/// RIFF + `fmt ` (40-byte `WAVE_FORMAT_EXTENSIBLE` body) + `data` headers.
const HEADER_BYTES: usize = 12 + 8 + 40 + 8;

/// `KSDATAFORMAT_SUBTYPE_PCM`, the `SubFormat` GUID of integer PCM.
const SUBFORMAT_PCM: [u8; 16] = [
    0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
];

/// Write interleaved `f32` samples (range ~[-1,1]) as a 16-bit PCM
/// `WAVE_FORMAT_EXTENSIBLE` WAV carrying `channel_mask`.
fn write_wav16(
    path: &PathBuf,
    sample_rate: u32,
    channels: u16,
    channel_mask: u32,
    samples: &[f32],
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let data_len = (samples.len() * 2) as u32;
    let byte_rate = sample_rate * channels as u32 * 2;
    let block_align = channels * 2;

    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    f.write_all(b"RIFF")?;
    f.write_all(&(HEADER_BYTES as u32 - 8 + data_len).to_le_bytes())?;
    f.write_all(b"WAVE")?;
    f.write_all(b"fmt ")?;
    f.write_all(&40u32.to_le_bytes())?;
    f.write_all(&0xFFFEu16.to_le_bytes())?; // WAVE_FORMAT_EXTENSIBLE
    f.write_all(&channels.to_le_bytes())?;
    f.write_all(&sample_rate.to_le_bytes())?;
    f.write_all(&byte_rate.to_le_bytes())?;
    f.write_all(&block_align.to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?; // bits per sample
    f.write_all(&22u16.to_le_bytes())?; // extension size
    f.write_all(&16u16.to_le_bytes())?; // valid bits per sample
    f.write_all(&channel_mask.to_le_bytes())?;
    f.write_all(&SUBFORMAT_PCM)?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
        f.write_all(&v.to_le_bytes())?;
    }
    f.flush()?;
    Ok(())
}
