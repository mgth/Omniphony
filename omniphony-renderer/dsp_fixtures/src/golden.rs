//! Golden render storage and the null comparison.
//!
//! Goldens are raw little-endian `f32`, headerless — the same layout the
//! renderer's file sink writes with `--output-file-format raw-f32`, so a golden
//! can be auditioned directly:
//!
//! ```sh
//! ffplay -f f32le -ar 48000 -ac 12 goldens/speaker_714_32obj.f32
//! ```

use std::io::{Read, Write};
use std::path::PathBuf;

use crate::residual::{peak_dbfs, peak_residual_dbfs, rms_residual_dbfs, worst_deviation};

/// Gate threshold: the largest permitted peak residual, in dBFS.
///
/// Not bit-exact by design — CI's toolchain is unpinned, and an LLVM upgrade
/// can re-vectorize the mix loops and shift the last mantissa bit with no
/// source change. −120 dBFS is ~100 dB below anything audible or structurally
/// meaningful, while being immune to that churn.
pub const RESIDUAL_GATE_DBFS: f32 = -120.0;

/// Gate for goldens produced by the **binaural** path, in dBFS.
///
/// HRIR convolution accumulates far more floating-point error than the gain-mix
/// paths: hundreds of multiply-adds per output sample, over a filter whose taps
/// are themselves interpolated between measurements. The order those adds are
/// vectorized in differs between hosts, so the residual between two machines
/// running the *same* source is an order of magnitude above the mix paths'.
///
/// Measured: a golden blessed on a developer workstation reads −109.8 dBFS peak
/// (−126.0 dBFS rms) against a GitHub-hosted runner, with no source difference —
/// which failed the −120 gate on `main` for every run after the harness landed.
/// −100 dBFS sits above that cross-host noise while staying ~100 dB below any
/// behavioural change (a wrong HRIR, a shifted ITD or a gain error lands tens of
/// dB higher, not fractions of one).
pub const BINAURAL_RESIDUAL_GATE_DBFS: f32 = -100.0;

/// Floor below which a render is treated as degenerate, in dBFS. Guards
/// against a golden of zeros matching a silent render and "passing".
pub const NON_SILENT_FLOOR_DBFS: f32 = -60.0;

/// Absolute path of a golden, resolved against this crate's manifest directory
/// so it works regardless of the caller's working directory.
pub fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("goldens")
        .join(format!("{name}.f32"))
}

/// Read a golden as raw little-endian `f32`.
pub fn read_golden(name: &str) -> std::io::Result<Vec<f32>> {
    let mut bytes = Vec::new();
    std::fs::File::open(golden_path(name))?.read_to_end(&mut bytes)?;
    if bytes.len() % 4 != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{name}.f32 length {} is not a multiple of 4", bytes.len()),
        ));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}

/// Write a golden as raw little-endian `f32`, creating `goldens/` if needed.
pub fn write_golden(name: &str, samples: &[f32]) -> std::io::Result<()> {
    let path = golden_path(name);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut out = Vec::with_capacity(samples.len() * 4);
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::File::create(path)?.write_all(&out)
}

/// True when `OMNIPHONY_BLESS_GOLDENS=1` is set.
pub fn bless_enabled() -> bool {
    std::env::var("OMNIPHONY_BLESS_GOLDENS").is_ok_and(|v| v == "1")
}

/// Compare a render against its golden, or rewrite the golden when blessing.
///
/// Assertion order matters: shape, then non-degeneracy, then finiteness, then
/// the residual. Checking the residual first would let a zero-length or silent
/// render report a flattering `-inf`.
///
/// On mismatch the render is dumped beside the golden as `<name>.actual.f32`
/// so it can be auditioned or diffed offline.
pub fn assert_matches_golden(name: &str, rendered: &[f32], channels: usize) {
    assert_matches_golden_with_gate(name, rendered, channels, RESIDUAL_GATE_DBFS)
}

/// [`assert_matches_golden`] with an explicit residual gate, for paths whose
/// cross-host floating-point noise exceeds the default (see
/// [`BINAURAL_RESIDUAL_GATE_DBFS`]). Prefer the default gate: widening one is a
/// statement about a path's numerical behaviour and belongs next to a measured
/// value, not a convenience.
pub fn assert_matches_golden_with_gate(
    name: &str,
    rendered: &[f32],
    channels: usize,
    gate_dbfs: f32,
) {
    assert!(channels > 0, "channels must be non-zero");
    assert_eq!(
        rendered.len() % channels,
        0,
        "{name}: render length {} is not a whole number of {channels}-channel frames",
        rendered.len()
    );

    let render_peak = peak_dbfs(rendered);
    assert!(
        render_peak > NON_SILENT_FLOOR_DBFS,
        "{name}: render is silent or near-silent (peak {render_peak:.1} dBFS). \
         A degenerate render must never be compared — it would match a zero golden."
    );
    assert!(
        rendered.iter().all(|s| s.is_finite()),
        "{name}: render contains NaN or Inf"
    );

    if bless_enabled() {
        // Report what is being replaced: the golden files are binary, so this
        // printed residual is the only reviewable artifact of a bless.
        match read_golden(name) {
            Ok(old) if old.len() == rendered.len() => {
                println!(
                    "[bless] {name}: replacing golden — peak residual {:.1} dBFS, \
                     rms residual {:.1} dBFS",
                    peak_residual_dbfs(&old, rendered),
                    rms_residual_dbfs(&old, rendered)
                );
            }
            Ok(old) => println!(
                "[bless] {name}: replacing golden — length changed {} -> {}",
                old.len(),
                rendered.len()
            ),
            Err(_) => println!(
                "[bless] {name}: creating new golden ({} samples)",
                rendered.len()
            ),
        }
        write_golden(name, rendered).expect("write golden");
        return;
    }

    let golden = read_golden(name).unwrap_or_else(|e| {
        panic!(
            "{name}: cannot read golden ({e}). \
             Create it with OMNIPHONY_BLESS_GOLDENS=1 cargo test -p renderer"
        )
    });

    if golden.len() != rendered.len() {
        let _ = write_golden(&format!("{name}.actual"), rendered);
        panic!(
            "{name}: length mismatch — golden {} samples ({} frames), \
             render {} samples ({} frames). Never compared as truncated.",
            golden.len(),
            golden.len() / channels,
            rendered.len(),
            rendered.len() / channels
        );
    }

    let peak = peak_residual_dbfs(&golden, rendered);
    if peak > gate_dbfs {
        let rms = rms_residual_dbfs(&golden, rendered);
        let (frame, channel, delta) = worst_deviation(&golden, rendered, channels);
        let mut diverging = String::new();
        for (i, (g, r)) in golden.iter().zip(rendered).enumerate() {
            if (g - r).abs() > 0.0 {
                diverging.push_str(&format!(
                    "\n    frame {:>6} ch {:>2}: golden {:+.9} render {:+.9}",
                    i / channels,
                    i % channels,
                    g,
                    r
                ));
                if diverging.matches('\n').count() >= 8 {
                    break;
                }
            }
        }
        let _ = write_golden(&format!("{name}.actual"), rendered);
        panic!(
            "{name}: null test failed.\n  \
             peak residual {peak:.1} dBFS (gate {gate_dbfs:.1})\n  \
             rms residual  {rms:.1} dBFS\n  \
             worst at frame {frame} channel {channel}, delta {delta:.9}\n  \
             first diverging samples:{diverging}\n  \
             render dumped to {}\n  \
             If this change is intended: OMNIPHONY_BLESS_GOLDENS=1 cargo test -p renderer \
             and quote the printed residual in the PR.",
            golden_path(&format!("{name}.actual")).display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_preserves_samples_exactly() {
        let name = "roundtrip_selftest";
        let data: Vec<f32> = (0..256).map(|i| (i as f32 / 256.0) - 0.5).collect();
        write_golden(name, &data).expect("write");
        let back = read_golden(name).expect("read");
        assert_eq!(data, back, "golden roundtrip must be bit-exact on disk");
        std::fs::remove_file(golden_path(name)).expect("cleanup");
    }

    #[test]
    fn missing_golden_is_an_error_not_a_panic() {
        let r = read_golden("definitely_does_not_exist");
        assert!(r.is_err(), "a missing golden must surface as Err");
    }
}
