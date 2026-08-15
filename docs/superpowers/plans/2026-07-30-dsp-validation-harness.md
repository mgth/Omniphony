# DSP Validation Harness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a deterministic DSP validation harness for `omniphony-renderer` — a golden/null test that protects render-path refactors, plus theory-derived acceptance measurements for the LR4 crossover, VBAP, and binaural ITD.

**Architecture:** A new dev-only workspace crate `dsp_fixtures` holds deterministic scene generation, golden I/O, residual math, and signal analysis. It is consumed as a path dev-dependency of `renderer`; all tests live in-lib inside `renderer` so they can reach `pub(crate)` internals and add no new test-binary link step. The existing benches migrate onto the same fixture module so the null test and the future perf gate measure identical scenes.

**Tech Stack:** Rust 2024 edition, `realfft 3.5` (already a workspace dependency), `criterion 0.5` (existing bench harness). No new third-party dependencies.

## Global Constraints

- Comparison policy: peak residual below **−120 dBFS**, where dBFS is `20·log10(|x|)` with full scale at `1.0`. Never bit-exact.
- Added CI cost: **under 2 s** on `cargo test --workspace`. Measured baseline is 7.42 s; the suite must stay under 10 s.
- **No new CI job.** `.github/workflows/ci.yml:106` already runs `cargo test --workspace`.
- Acceptance thresholds are **theory-derived**, never ratcheted from current behaviour.
- `#[ignore]` is reserved **exclusively** for tracked deferrals of failing theory thresholds. The wide matrix uses the `wide-matrix` cargo feature instead.
- Phase 1 (Tasks 1–10) **measures and reports**; only the null test asserts. Phase 2 (Tasks 11–12) converts measurements into gates.
- Rust edition/version floor: `edition = "2024"`, `rust-version = "1.87.0"` (match the workspace root).
- Scene constants are fixed: `BLOCK_SAMPLES = 40`, `SAMPLE_RATE = 48_000`. Goldens are 0.25 s = 300 blocks = 12 000 frames per channel.
- Golden files are raw little-endian `f32`, no header.
- All paths below are relative to the repository root, `/home/rex/Omniphony`.

---

## File Structure

**Created:**

| Path | Responsibility |
| --- | --- |
| `omniphony-renderer/dsp_fixtures/Cargo.toml` | Crate manifest; `wide-matrix` feature |
| `omniphony-renderer/dsp_fixtures/src/lib.rs` | Module declarations and re-exports |
| `omniphony-renderer/dsp_fixtures/src/residual.rs` | dBFS conversion, peak/RMS residual, worst-deviation locator |
| `omniphony-renderer/dsp_fixtures/src/golden.rs` | Golden read/write, bless workflow, `assert_matches_golden` |
| `omniphony-renderer/dsp_fixtures/src/scene.rs` | Deterministic scene generation (migrated from the bench) |
| `omniphony-renderer/dsp_fixtures/src/analysis.rs` | Magnitude response via realfft; cross-correlation lag estimator |
| `omniphony-renderer/dsp_fixtures/src/dirs.rs` | Fibonacci sphere lattice; meridian and ring sweeps |
| `omniphony-renderer/dsp_fixtures/goldens/*.f32` | Three committed golden renders (~1.05 MB total) |
| `omniphony-renderer/renderer/src/spatial_renderer/golden_tests.rs` | Null tests |
| `omniphony-renderer/renderer/src/crossover/validation.rs` | LR4 reconstruction flatness |
| `omniphony-renderer/renderer/src/spatial_vbap/panner/native_validation.rs` | VBAP energy + seam continuity |
| `omniphony-renderer/renderer/src/binaural/validation.rs` | End-to-end ITD |
| `docs/dsp-validation-report.md` | Phase 1 measurement report |

**Modified:**

| Path | Change |
| --- | --- |
| `omniphony-renderer/Cargo.toml:2` | Add `dsp_fixtures` to `workspace.members` |
| `omniphony-renderer/renderer/Cargo.toml:30-31` | Add `dsp_fixtures` dev-dependency |
| `omniphony-renderer/renderer/benches/render_frame.rs:30-196` | Delete local generators; import from `dsp_fixtures::scene` |
| `omniphony-renderer/renderer/src/spatial_renderer/mod.rs:1520-1521` | Add `#[cfg(test)] mod golden_tests;` |
| `omniphony-renderer/renderer/src/crossover/mod.rs` | Add `#[cfg(test)] mod validation;` |
| `omniphony-renderer/renderer/src/spatial_vbap/panner.rs:219-220` | Add `#[cfg(test)] mod native_validation;` |
| `omniphony-renderer/renderer/src/binaural/mod.rs:31` | Add `#[cfg(test)] mod validation;` |

---

# Phase 1 — Measure

## Task 1: `dsp_fixtures` crate and residual math

**Files:**
- Create: `omniphony-renderer/dsp_fixtures/Cargo.toml`
- Create: `omniphony-renderer/dsp_fixtures/src/lib.rs`
- Create: `omniphony-renderer/dsp_fixtures/src/residual.rs`
- Modify: `omniphony-renderer/Cargo.toml:2`

**Interfaces:**
- Consumes: nothing.
- Produces: `dsp_fixtures::residual::{lin_to_dbfs, peak_dbfs, peak_residual_dbfs, rms_residual_dbfs, worst_deviation}`. Signatures:
  - `lin_to_dbfs(x: f32) -> f32`
  - `peak_dbfs(x: &[f32]) -> f32`
  - `peak_residual_dbfs(a: &[f32], b: &[f32]) -> f32`
  - `rms_residual_dbfs(a: &[f32], b: &[f32]) -> f32`
  - `worst_deviation(a: &[f32], b: &[f32], channels: usize) -> (usize, usize, f32)` returning `(frame, channel, delta)`

- [ ] **Step 1: Create the crate manifest**

`omniphony-renderer/dsp_fixtures/Cargo.toml`:

```toml
[package]
name = "dsp_fixtures"
version = "0.1.0"
edition = "2024"
rust-version = "1.87.0"
license = "GPL-3.0-or-later"
description = "Deterministic scenes, goldens and signal analysis for Omniphony DSP validation. Dev-only: nothing in the shipped binaries depends on this."
publish = false

[dependencies]
renderer = { path = "../renderer" }
realfft = "3.5"
```

No `[features]` section: the wide-matrix cases are `#[cfg(feature = ...)]` on
tests that live inside `renderer`, so the feature must be declared on `renderer`
(Task 12). A `wide-matrix` feature here would be dead.

- [ ] **Step 2: Register the crate in the workspace**

In `omniphony-renderer/Cargo.toml`, add `"dsp_fixtures"` to the end of the `members` array on line 2:

```toml
members = ["renderer", "audio_input", "audio_output", "sys", "bridge_api", "spdif", "runtime_control", "orender_engine", "orender_ffi", "host_audio", "example_backend", "script_backend", "reference_bridge", "dsp_fixtures"]
```

- [ ] **Step 3: Write the failing tests for residual math**

`omniphony-renderer/dsp_fixtures/src/residual.rs`:

```rust
//! Null-comparison arithmetic: differences between two renders expressed in
//! dBFS, plus the locator used to report *where* a mismatch is worst.
//!
//! dBFS here is `20·log10(|x|)` with full scale at `1.0`, matching the
//! renderer's f32 sample convention.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_signals_have_negative_infinite_residual() {
        let a = vec![0.1, -0.5, 0.9, 0.0];
        let r = peak_residual_dbfs(&a, &a);
        assert_eq!(
            r,
            f32::NEG_INFINITY,
            "identical inputs must be -inf dBFS, not NaN or a finite value"
        );
    }

    #[test]
    fn constant_offset_gives_the_analytic_value() {
        // A difference of exactly 1e-6 is exactly -120 dBFS.
        let a = vec![0.0f32; 64];
        let b = vec![1e-6f32; 64];
        let peak = peak_residual_dbfs(&a, &b);
        assert!(
            (peak - -120.0).abs() < 0.01,
            "expected -120 dBFS for a 1e-6 offset, got {peak}"
        );
        // Every sample differs by the same amount, so RMS equals peak.
        let rms = rms_residual_dbfs(&a, &b);
        assert!(
            (rms - -120.0).abs() < 0.01,
            "expected -120 dBFS RMS for a constant offset, got {rms}"
        );
    }

    #[test]
    fn peak_dbfs_reads_full_scale_as_zero() {
        assert!((peak_dbfs(&[0.0, -1.0, 0.5]) - 0.0).abs() < 1e-6);
        assert_eq!(peak_dbfs(&[0.0, 0.0]), f32::NEG_INFINITY);
    }

    #[test]
    fn worst_deviation_locates_frame_and_channel() {
        // 3 channels, 4 frames. Plant the largest error at frame 2, channel 1.
        let a = vec![0.0f32; 12];
        let mut b = vec![0.0f32; 12];
        b[1 * 3 + 0] = 0.01; // frame 1, channel 0 — smaller
        b[2 * 3 + 1] = 0.50; // frame 2, channel 1 — largest
        let (frame, channel, delta) = worst_deviation(&a, &b, 3);
        assert_eq!((frame, channel), (2, 1));
        assert!((delta - 0.50).abs() < 1e-6, "got {delta}");
    }
}
```

- [ ] **Step 4: Run the tests to verify they fail**

Run: `cd omniphony-renderer && cargo test -p dsp_fixtures`
Expected: FAIL — compile errors, `cannot find function peak_residual_dbfs`.

- [ ] **Step 5: Implement the residual functions**

Prepend to `omniphony-renderer/dsp_fixtures/src/residual.rs`, above the `mod tests` block:

```rust
/// Linear amplitude to dBFS. Returns [`f32::NEG_INFINITY`] for zero or
/// negative input rather than NaN, so a perfect match reports as `-inf`.
pub fn lin_to_dbfs(x: f32) -> f32 {
    if x <= 0.0 {
        f32::NEG_INFINITY
    } else {
        20.0 * x.log10()
    }
}

/// Peak absolute level of a signal, in dBFS.
pub fn peak_dbfs(x: &[f32]) -> f32 {
    lin_to_dbfs(x.iter().map(|v| v.abs()).fold(0.0f32, f32::max))
}

/// Largest absolute sample-by-sample difference, in dBFS. This is the gate.
///
/// Panics if the slices differ in length — callers must check shape first so
/// the failure names the real problem instead of silently truncating.
pub fn peak_residual_dbfs(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "residual needs equal-length signals");
    let peak = a
        .iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max);
    lin_to_dbfs(peak)
}

/// RMS of the difference, in dBFS. Reported alongside the peak for context;
/// not itself a gate. Accumulates in `f64` so long renders do not lose
/// precision in the sum.
pub fn rms_residual_dbfs(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "residual needs equal-length signals");
    if a.is_empty() {
        return f32::NEG_INFINITY;
    }
    let sum_sq: f64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| {
            let d = (*x - *y) as f64;
            d * d
        })
        .sum();
    lin_to_dbfs((sum_sq / a.len() as f64).sqrt() as f32)
}

/// Locate the largest deviation in an interleaved pair: `(frame, channel, delta)`.
///
/// Used only for failure messages — a bare "golden mismatch" is not actionable.
pub fn worst_deviation(a: &[f32], b: &[f32], channels: usize) -> (usize, usize, f32) {
    assert_eq!(a.len(), b.len(), "residual needs equal-length signals");
    assert!(channels > 0, "channels must be non-zero");
    let mut best = (0usize, 0usize, 0.0f32);
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        let d = (x - y).abs();
        if d > best.2 {
            best = (i / channels, i % channels, d);
        }
    }
    best
}
```

- [ ] **Step 6: Create the crate root**

`omniphony-renderer/dsp_fixtures/src/lib.rs`:

```rust
//! Deterministic fixtures and analysis for Omniphony DSP validation.
//!
//! Dev-only: this crate is a path dev-dependency of `renderer`, and nothing in
//! the dependency graph of `orender` or `liborender` references it, so release
//! builds never compile it.
//!
//! It exists so that the null test, the criterion benches, and the future
//! worst-case-block-time gate all measure *the same* scenes. Duplicating scene
//! generation between those consumers is how they silently drift apart.

pub mod residual;
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cd omniphony-renderer && cargo test -p dsp_fixtures`
Expected: PASS — 4 tests.

- [ ] **Step 8: Commit**

```bash
git add omniphony-renderer/Cargo.toml omniphony-renderer/dsp_fixtures
git commit -m "test(fixtures): add dsp_fixtures crate with residual math

Null-comparison arithmetic in dBFS, with unit tests against analytically
known values so a broken harness cannot silently pass everything."
```

---

## Task 2: Migrate scene generation out of the bench

**Files:**
- Create: `omniphony-renderer/dsp_fixtures/src/scene.rs`
- Modify: `omniphony-renderer/dsp_fixtures/src/lib.rs`
- Modify: `omniphony-renderer/renderer/benches/render_frame.rs:24-196`

**Interfaces:**
- Consumes: nothing from Task 1.
- Produces: `dsp_fixtures::scene::{BLOCK_SAMPLES, SAMPLE_RATE, pseudo, make_pcm, move_events, crossover_layout, build_renderer, make_renderer, prepared, render_blocks}`. Signatures:
  - `BLOCK_SAMPLES: usize = 40`, `SAMPLE_RATE: u32 = 48_000`
  - `pseudo(seed: u64) -> f32`
  - `make_pcm(n_objects: usize) -> Vec<f32>`
  - `move_events(n_objects: usize, seed_round: u64) -> Vec<SpatialChannelEvent>`
  - `crossover_layout() -> SpeakerLayout`
  - `build_renderer(layout: SpeakerLayout, position_interpolation: bool, cartesian: bool) -> SpatialRenderer`
  - `make_renderer(preset: &str, position_interpolation: bool, cartesian: bool) -> SpatialRenderer`
  - `prepared(preset: &str, n_objects: usize, ramp_mode: RampMode, position_interpolation: bool, cartesian: bool) -> (SpatialRenderer, Vec<f32>)`
  - `render_blocks(r: &mut SpatialRenderer, pcm: &[f32], n_objects: usize, blocks: usize, move_every: usize) -> Vec<f32>`

- [ ] **Step 1: Create `scene.rs` by moving the bench generators verbatim**

Create `omniphony-renderer/dsp_fixtures/src/scene.rs`. Move these items **verbatim** from `omniphony-renderer/renderer/benches/render_frame.rs`, changing only what is listed below:

| Item | Source lines |
| --- | --- |
| `BLOCK_SAMPLES`, `SAMPLE_RATE` consts | 35-36 |
| `make_renderer` | 40-47 |
| `crossover_layout` | 53-61 |
| `build_renderer` | 62-119 |
| `pseudo` | 123-131 |
| `make_pcm` | 133-142 |
| `move_events` | 144-166 |
| `prepared` | 172-196 |

Changes required during the move, and no others:

1. Add `pub` to every moved item, including the two consts.
2. Prepend this module header and imports:

```rust
//! Deterministic scene generation, shared by the null tests, the criterion
//! benches, and the future worst-case-block-time gate.
//!
//! Everything here is a pure function of its arguments and a fixed seed: the
//! same call sequence produces byte-identical PCM and event streams on every
//! machine. That is what makes committed goldens meaningful.
//!
//! Moved here from `renderer/benches/render_frame.rs` so the benches and the
//! validation tests cannot drift apart.

use renderer::live_params::{LiveEvaluationMode, PreferredEvaluationMode, RampMode};
use renderer::spatial_renderer::{SpatialChannelEvent, SpatialRenderer};
use renderer::spatial_vbap::{DistanceModel, VbapTableMode};
use renderer::speaker_layout::SpeakerLayout;
```

3. In `prepared`, keep the body exactly as-is including the four priming rounds and the `let _ = round;`.

- [ ] **Step 2: Add `render_blocks` for multi-block capture**

The bench only ever renders one block at a time. The null test needs a
concatenated multi-block render. Append to `scene.rs`:

```rust
/// Render `blocks` consecutive blocks, concatenating the interleaved output.
///
/// `move_every` controls how often fresh movement events are injected: every
/// `move_every`-th block gets `move_events(n_objects, round)`, the others carry
/// no events. `move_every = 0` means "never move after priming". This is what
/// makes a golden exercise both the ramping and the steady path.
pub fn render_blocks(
    r: &mut SpatialRenderer,
    pcm: &[f32],
    n_objects: usize,
    blocks: usize,
    move_every: usize,
) -> Vec<f32> {
    let mut out = Vec::new();
    let mut buf = Vec::new();
    for round in 0..blocks {
        let events = if move_every > 0 && round % move_every == 0 {
            move_events(n_objects, round as u64 + 1)
        } else {
            Vec::new()
        };
        let frame = r
            .render_frame(pcm, n_objects, &events, buf, false)
            .expect("render_frame in fixture scene");
        out.extend_from_slice(&frame.samples);
        buf = frame.samples;
        buf.clear();
    }
    out
}
```

- [ ] **Step 3: Declare the module**

In `omniphony-renderer/dsp_fixtures/src/lib.rs`, add after `pub mod residual;`:

```rust
pub mod scene;
```

- [ ] **Step 4: Add the dev-dependency to `renderer`**

In `omniphony-renderer/renderer/Cargo.toml`, extend the `[dev-dependencies]` block at line 30:

```toml
[dev-dependencies]
criterion = "0.5"
dsp_fixtures = { path = "../dsp_fixtures" }
```

- [ ] **Step 5: Point the bench at the shared module**

In `omniphony-renderer/renderer/benches/render_frame.rs`, delete lines 27-30 (the four `use renderer::...` imports) and lines 35-196 (every moved item), then insert in their place:

```rust
use dsp_fixtures::scene::{
    BLOCK_SAMPLES, crossover_layout, build_renderer, make_pcm, make_renderer, move_events,
    prepared, pseudo,
};
use renderer::live_params::RampMode;
use renderer::spatial_renderer::SpatialChannelEvent;
use renderer::speaker_layout::SpeakerLayout;
```

Leave the module doc comment (lines 1-23), `use std::hint::black_box;`, the
`criterion` import, and every `bench_*` function untouched.

- [ ] **Step 6: Verify the bench still compiles and the workspace still builds**

Run: `cd omniphony-renderer && cargo build --workspace && cargo bench -p renderer --no-run`
Expected: both succeed. If the bench reports an unused import, remove only that import — do not change any `bench_*` body.

- [ ] **Step 7: Re-baseline the benches**

The move must not change performance, but Task 9 of improvement #9 will depend
on these numbers, so record them rather than assuming.

Run: `cd omniphony-renderer && cargo bench -p renderer -- render_steady`
Record the reported times for each object count in the commit message. If any
figure moved by more than 5 %, stop and investigate before committing — the
migration was supposed to be a pure move.

- [ ] **Step 8: Commit**

```bash
git add omniphony-renderer/dsp_fixtures omniphony-renderer/renderer/Cargo.toml \
        omniphony-renderer/renderer/benches/render_frame.rs
git commit -m "test(fixtures): move bench scene generation into dsp_fixtures

The null test, the benches and the future block-time gate must measure
the same scenes; duplicated generators drift. Pure move plus a new
render_blocks helper for multi-block capture.

Bench re-baseline (render_steady): <paste figures>"
```

---

## Task 3: Golden I/O and the first null test

**Files:**
- Create: `omniphony-renderer/dsp_fixtures/src/golden.rs`
- Create: `omniphony-renderer/dsp_fixtures/goldens/.gitignore`
- Create: `omniphony-renderer/renderer/src/spatial_renderer/golden_tests.rs`
- Modify: `omniphony-renderer/dsp_fixtures/src/lib.rs`
- Modify: `omniphony-renderer/renderer/src/spatial_renderer/mod.rs:1520-1521`

**Interfaces:**
- Consumes: `dsp_fixtures::residual::{peak_dbfs, peak_residual_dbfs, rms_residual_dbfs, worst_deviation}` (Task 1); `dsp_fixtures::scene::{prepared, render_blocks, BLOCK_SAMPLES}` (Task 2).
- Produces: `dsp_fixtures::golden::{golden_path, read_golden, write_golden, bless_enabled, assert_matches_golden}`. Signatures:
  - `golden_path(name: &str) -> std::path::PathBuf`
  - `read_golden(name: &str) -> std::io::Result<Vec<f32>>`
  - `write_golden(name: &str, samples: &[f32]) -> std::io::Result<()>`
  - `bless_enabled() -> bool`
  - `assert_matches_golden(name: &str, rendered: &[f32], channels: usize)`

- [ ] **Step 1: Ignore `*.actual.f32` dumps**

`omniphony-renderer/dsp_fixtures/goldens/.gitignore`:

```gitignore
# Failing renders are dumped here for inspection; never commit them.
*.actual.f32
```

- [ ] **Step 2: Write the failing tests for golden I/O**

`omniphony-renderer/dsp_fixtures/src/golden.rs`:

```rust
//! Golden render storage and the null comparison.
//!
//! Goldens are raw little-endian `f32`, headerless — the same layout the
//! renderer's file sink writes with `--output-file-format raw-f32`, so a golden
//! can be auditioned directly:
//!
//! ```sh
//! ffplay -f f32le -ar 48000 -ac 12 goldens/speaker_714_32obj.f32
//! ```

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
```

- [ ] **Step 3: Run to verify it fails**

Run: `cd omniphony-renderer && cargo test -p dsp_fixtures golden`
Expected: FAIL — `cannot find function write_golden`.

- [ ] **Step 4: Implement golden I/O and the comparison**

Prepend to `omniphony-renderer/dsp_fixtures/src/golden.rs`, above `mod tests`:

```rust
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
            Err(_) => println!("[bless] {name}: creating new golden ({} samples)", rendered.len()),
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
    if peak > RESIDUAL_GATE_DBFS {
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
             peak residual {peak:.1} dBFS (gate {RESIDUAL_GATE_DBFS:.1})\n  \
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
```

- [ ] **Step 5: Declare the module**

In `omniphony-renderer/dsp_fixtures/src/lib.rs`, add after `pub mod scene;`:

```rust
pub mod golden;
```

- [ ] **Step 6: Run the golden I/O tests to verify they pass**

Run: `cd omniphony-renderer && cargo test -p dsp_fixtures golden`
Expected: PASS — 2 tests.

- [ ] **Step 7: Write the 7.1.4 null test**

`omniphony-renderer/renderer/src/spatial_renderer/golden_tests.rs`:

```rust
//! Null tests: render fixed scenes and compare against committed goldens.
//!
//! These are the safety net for render-path refactors. A change that preserves
//! behaviour leaves the peak residual below the gate; a change that alters a
//! single sample audibly does not.
//!
//! Regenerate after an intended change:
//!   OMNIPHONY_BLESS_GOLDENS=1 cargo test -p renderer
//! and quote the printed residual in the pull request.

use dsp_fixtures::golden::assert_matches_golden;
use dsp_fixtures::scene::{make_pcm, prepared, render_blocks};
use renderer::live_params::RampMode;

/// 0.25 s at 48 kHz. `GAIN_SLEW_SECS` is 0.02, so this covers the 20 ms
/// fade-in plus ~230 ms of steady motion.
const BLOCKS: usize = 300;

/// Fresh movement events every 8th block, so the golden exercises both the
/// ramping path and the steady path.
const MOVE_EVERY: usize = 8;

const N_OBJECTS: usize = 32;

#[test]
fn null_speaker_714_32obj() {
    let (mut r, _) = prepared("7.1.4", N_OBJECTS, RampMode::Frame, true, false);
    let pcm = make_pcm(N_OBJECTS);
    let out = render_blocks(&mut r, &pcm, N_OBJECTS, BLOCKS, MOVE_EVERY);
    let channels = out.len() / (BLOCKS * dsp_fixtures::scene::BLOCK_SAMPLES);
    assert_eq!(channels, 12, "7.1.4 must render 12 speaker channels");
    assert_matches_golden("speaker_714_32obj", &out, channels);
}
```

- [ ] **Step 8: Declare the test module**

In `omniphony-renderer/renderer/src/spatial_renderer/mod.rs`, after line 1521
(`mod tests;`), append:

```rust
#[cfg(test)]
mod golden_tests;
```

- [ ] **Step 9: Run to verify it fails with a missing golden**

Run: `cd omniphony-renderer && cargo test -p renderer null_speaker_714_32obj`
Expected: FAIL — panic containing "cannot read golden" and the bless command.
This confirms the guard fires rather than silently creating a golden.

- [ ] **Step 10: Generate the golden, then verify the test passes**

```bash
cd omniphony-renderer
OMNIPHONY_BLESS_GOLDENS=1 cargo test -p renderer null_speaker_714_32obj -- --nocapture
cargo test -p renderer null_speaker_714_32obj
```

Expected: the first prints `[bless] speaker_714_32obj: creating new golden (…)`;
the second PASSES. Confirm the file is ~576 KB:

```bash
ls -l dsp_fixtures/goldens/speaker_714_32obj.f32
```

- [ ] **Step 11: Verify the test actually detects a change**

Temporarily perturb the render to prove the net works. In
`omniphony-renderer/renderer/src/spatial_renderer/mod.rs`, change
`GAIN_SLEW_SECS` on line 157 from `0.02` to `0.021`, then:

Run: `cd omniphony-renderer && cargo test -p renderer null_speaker_714_32obj`
Expected: FAIL, reporting a peak residual well above −120 dBFS, a frame/channel
location, and diverging samples.

**Revert line 157 to `0.02`** and confirm the test passes again. Delete the
`speaker_714_32obj.actual.f32` dump.

- [ ] **Step 12: Commit**

```bash
git add omniphony-renderer/dsp_fixtures omniphony-renderer/renderer/src/spatial_renderer
git commit -m "test(golden): null test for the 7.1.4 speaker render

Committed golden compared at a -120 dBFS peak-residual gate, with shape,
non-silence and finiteness checked first so a degenerate render cannot
pass. Verified the net detects a deliberate GAIN_SLEW_SECS perturbation."
```

---

## Task 4: Binaural and crossover null scenes

**Files:**
- Modify: `omniphony-renderer/dsp_fixtures/src/scene.rs`
- Modify: `omniphony-renderer/renderer/src/spatial_renderer/golden_tests.rs`

**Interfaces:**
- Consumes: everything from Tasks 1-3.
- Produces: `dsp_fixtures::scene::{prepared_binaural, prepared_crossover}`. Signatures:
  - `prepared_binaural(n_objects: usize, ramp_mode: RampMode) -> (SpatialRenderer, Vec<f32>)`
  - `prepared_crossover(n_objects: usize, ramp_mode: RampMode) -> (SpatialRenderer, Vec<f32>)`

- [ ] **Step 1: Add the two scene builders**

Append to `omniphony-renderer/dsp_fixtures/src/scene.rs`:

```rust
use renderer::live_params::OutputMode;

/// A renderer switched to the independent binaural (headphone) path, with the
/// bundled SAF KEMAR set. Output is 2-channel regardless of the layout.
///
/// `HrirSource::SafKemar` is already the default, so it is not set explicitly —
/// the golden would silently change if that default moved, which is exactly the
/// kind of drift a null test should catch.
pub fn prepared_binaural(n_objects: usize, ramp_mode: RampMode) -> (SpatialRenderer, Vec<f32>) {
    let mut r = make_renderer("7.1.4", true, false);
    {
        let ctrl = r.renderer_control();
        ctrl.set_requested_ramp_mode(ramp_mode);
        let mut live = ctrl.live.write();
        live.ramp_mode = ramp_mode;
        live.binaural.output_mode = OutputMode::Binaural;
    }
    let pcm = make_pcm(n_objects);
    let init = move_events(n_objects, 0);
    let mut buf = Vec::new();
    for _ in 0..4 {
        let f = r
            .render_frame(&pcm, n_objects, &init, buf, false)
            .expect("prime binaural render");
        buf = f.samples;
    }
    (r, pcm)
}

/// A renderer on the band-limited [`crossover_layout`], so `compute_bands`
/// splits rendering across four frequency bands and the LR4 bank runs.
pub fn prepared_crossover(n_objects: usize, ramp_mode: RampMode) -> (SpatialRenderer, Vec<f32>) {
    let mut r = build_renderer(crossover_layout(), true, false);
    {
        let ctrl = r.renderer_control();
        ctrl.set_requested_ramp_mode(ramp_mode);
        ctrl.live.write().ramp_mode = ramp_mode;
    }
    let pcm = make_pcm(n_objects);
    let init = move_events(n_objects, 0);
    let mut buf = Vec::new();
    for _ in 0..4 {
        let f = r
            .render_frame(&pcm, n_objects, &init, buf, false)
            .expect("prime crossover render");
        buf = f.samples;
    }
    (r, pcm)
}
```

- [ ] **Step 2: Write the two failing null tests**

Append to `omniphony-renderer/renderer/src/spatial_renderer/golden_tests.rs`:

```rust
#[test]
fn null_binaural_kemar() {
    let (mut r, pcm) = dsp_fixtures::scene::prepared_binaural(N_OBJECTS, RampMode::Frame);
    let out = render_blocks(&mut r, &pcm, N_OBJECTS, BLOCKS, MOVE_EVERY);
    let channels = out.len() / (BLOCKS * dsp_fixtures::scene::BLOCK_SAMPLES);
    assert_eq!(channels, 2, "the binaural path must render 2 channels");
    assert_matches_golden("binaural_kemar", &out, channels);
}

#[test]
fn null_crossover_5_1_2() {
    let (mut r, pcm) = dsp_fixtures::scene::prepared_crossover(N_OBJECTS, RampMode::Frame);
    let out = render_blocks(&mut r, &pcm, N_OBJECTS, BLOCKS, MOVE_EVERY);
    let channels = out.len() / (BLOCKS * dsp_fixtures::scene::BLOCK_SAMPLES);
    assert_eq!(
        channels, 12,
        "crossover_layout is 7.1.4 band-limited — still 12 speakers"
    );
    assert_matches_golden("crossover_5_1_2", &out, channels);
}
```

Note: `crossover_layout()` band-limits three speakers of the 7.1.4 preset, so
the channel count stays 12. The golden name is kept as `crossover_5_1_2` only
if the layout is genuinely 5.1.2; since it is not, **rename it**: use
`crossover_bands` for both the test name (`null_crossover_bands`) and the golden.
Apply that rename before running.

- [ ] **Step 3: Run to verify both fail on a missing golden**

Run: `cd omniphony-renderer && cargo test -p renderer null_binaural null_crossover`
Expected: FAIL — both panic with "cannot read golden".

- [ ] **Step 4: Generate both goldens and verify they pass**

```bash
cd omniphony-renderer
OMNIPHONY_BLESS_GOLDENS=1 cargo test -p renderer null_ -- --nocapture
cargo test -p renderer null_
ls -l dsp_fixtures/goldens/
```

Expected: three tests pass. Sizes ≈ 576 KB, 96 KB, 576 KB.

- [ ] **Step 5: Confirm the time budget**

Run: `cd omniphony-renderer && cargo test -p renderer null_ -- --nocapture 2>&1 | tail -3`
Expected: the reported suite time for these three tests is well under 2 s. If it
exceeds 2 s, reduce `BLOCKS` to 150 (0.125 s) and regenerate the goldens — the
budget is a hard constraint.

- [ ] **Step 6: Commit**

```bash
git add omniphony-renderer/dsp_fixtures omniphony-renderer/renderer/src/spatial_renderer
git commit -m "test(golden): null scenes for the binaural and crossover paths

Binaural exercises the independent headphone path with bundled KEMAR;
the band-limited layout drives the LR4 bank through compute_bands."
```

---

## Task 5: Signal analysis — magnitude response and lag estimation

**Files:**
- Create: `omniphony-renderer/dsp_fixtures/src/analysis.rs`
- Modify: `omniphony-renderer/dsp_fixtures/src/lib.rs`

**Interfaces:**
- Consumes: `dsp_fixtures::residual::lin_to_dbfs` (Task 1).
- Produces: `dsp_fixtures::analysis::{magnitude_response_db, estimate_lag_samples}`. Signatures:
  - `magnitude_response_db(ir: &[f32], sample_rate: u32) -> Vec<(f32, f32)>` returning `(freq_hz, mag_db)`
  - `estimate_lag_samples(left: &[f32], right: &[f32], max_lag: usize) -> f32`

- [ ] **Step 1: Write the failing tests**

`omniphony-renderer/dsp_fixtures/src/analysis.rs`:

```rust
//! Measurement helpers for the acceptance tests: frequency response of an
//! impulse response, and interaural lag by cross-correlation.
//!
//! Both are verified below against analytically known answers. A validation
//! harness whose own measurements are wrong passes everything.

#[cfg(test)]
mod tests {
    use super::*;

    /// Bandlimited unit pulse centred at `delay` samples (possibly fractional).
    /// A windowed sinc is the right test signal: it has a known sub-sample
    /// position, unlike a bare impulse.
    fn sinc_pulse(len: usize, delay: f64) -> Vec<f32> {
        (0..len)
            .map(|n| {
                let x = n as f64 - delay;
                let s = if x.abs() < 1e-12 {
                    1.0
                } else {
                    (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
                };
                // Hann window over the whole buffer keeps the edges tame.
                let w = 0.5
                    - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / (len - 1) as f64).cos();
                (s * w) as f32
            })
            .collect()
    }

    #[test]
    fn unit_impulse_is_flat_at_zero_db() {
        let mut ir = vec![0.0f32; 1024];
        ir[0] = 1.0;
        let resp = magnitude_response_db(&ir, 48_000);
        assert_eq!(resp.len(), 1024 / 2 + 1, "realfft returns n/2+1 bins");
        for (freq, db) in &resp {
            assert!(
                db.abs() < 1e-3,
                "unit impulse must be 0 dB everywhere; got {db} dB at {freq} Hz"
            );
        }
    }

    #[test]
    fn magnitude_response_reports_a_known_gain() {
        let mut ir = vec![0.0f32; 512];
        ir[0] = 0.5; // -6.0206 dB, flat
        let resp = magnitude_response_db(&ir, 48_000);
        for (_, db) in &resp {
            assert!((db - -6.0206).abs() < 1e-2, "expected -6.02 dB, got {db}");
        }
    }

    #[test]
    fn bin_frequencies_span_dc_to_nyquist() {
        let ir = vec![0.0f32; 480];
        let resp = magnitude_response_db(&ir, 48_000);
        assert!((resp[0].0 - 0.0).abs() < 1e-6, "first bin is DC");
        let last = resp.last().expect("non-empty").0;
        assert!((last - 24_000.0).abs() < 1.0, "last bin is Nyquist, got {last}");
    }

    #[test]
    fn recovers_an_integer_lag() {
        let left = sinc_pulse(512, 100.0);
        let right = sinc_pulse(512, 107.0);
        // right is delayed by 7 samples relative to left.
        let lag = estimate_lag_samples(&left, &right, 64);
        assert!((lag - 7.0).abs() < 0.02, "expected +7.0, got {lag}");
    }

    #[test]
    fn recovers_a_fractional_lag() {
        let left = sinc_pulse(512, 100.0);
        let right = sinc_pulse(512, 107.5);
        let lag = estimate_lag_samples(&left, &right, 64);
        assert!((lag - 7.5).abs() < 0.1, "expected +7.5, got {lag}");
    }

    #[test]
    fn lag_sign_is_negative_when_left_is_delayed() {
        let left = sinc_pulse(512, 107.0);
        let right = sinc_pulse(512, 100.0);
        let lag = estimate_lag_samples(&left, &right, 64);
        assert!((lag - -7.0).abs() < 0.02, "expected -7.0, got {lag}");
    }

    #[test]
    fn identical_channels_have_zero_lag() {
        let s = sinc_pulse(512, 100.0);
        let lag = estimate_lag_samples(&s, &s, 64);
        assert!(lag.abs() < 0.02, "expected 0.0, got {lag}");
    }
}
```

- [ ] **Step 2: Run to verify the tests fail**

Run: `cd omniphony-renderer && cargo test -p dsp_fixtures analysis`
Expected: FAIL — `cannot find function magnitude_response_db`.

- [ ] **Step 3: Implement the analysis functions**

Prepend to `omniphony-renderer/dsp_fixtures/src/analysis.rs`, above `mod tests`:

```rust
use realfft::RealFftPlanner;

use crate::residual::lin_to_dbfs;

/// Magnitude response of `ir`, as `(frequency_hz, magnitude_db)` for every
/// real-FFT bin (`ir.len()/2 + 1` of them).
///
/// No window is applied: callers pass an impulse response long enough that its
/// tail has decayed. Windowing an already-decayed IR would only smear the
/// response, and truncating an undecayed one shows up as passband ripple —
/// which is why the LR4 test uses 32768 samples.
pub fn magnitude_response_db(ir: &[f32], sample_rate: u32) -> Vec<(f32, f32)> {
    assert!(ir.len() >= 2, "need at least 2 samples for an FFT");
    let n = ir.len();
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(n);
    let mut input = ir.to_vec();
    let mut spectrum = fft.make_output_vec();
    fft.process(&mut input, &mut spectrum)
        .expect("realfft forward transform");
    spectrum
        .iter()
        .enumerate()
        .map(|(k, c)| {
            let freq = k as f32 * sample_rate as f32 / n as f32;
            (freq, lin_to_dbfs(c.norm()))
        })
        .collect()
}

/// Lag, in samples, by which `right` is delayed relative to `left`.
///
/// Positive means `right[n] ≈ left[n - lag]`. For a binaural render this means
/// a source on the **right** returns a *negative* value: the contralateral
/// (left) ear is the delayed one.
///
/// The integer cross-correlation peak is refined by parabolic interpolation, so
/// sub-sample delays are recovered — necessary because ITD at 48 kHz is only
/// ~31 samples at full deflection and the interesting differences are fractions
/// of a sample.
pub fn estimate_lag_samples(left: &[f32], right: &[f32], max_lag: usize) -> f32 {
    assert_eq!(left.len(), right.len(), "channels must be equal length");
    assert!(
        left.len() > 2 * max_lag + 2,
        "signal ({}) too short for a ±{max_lag} lag search",
        left.len()
    );

    let n = left.len() as i64;
    let corr = |lag: i64| -> f64 {
        let mut acc = 0.0f64;
        let start = lag.max(0);
        let end = (n + lag).min(n);
        for i in start..end {
            acc += left[(i - lag) as usize] as f64 * right[i as usize] as f64;
        }
        acc
    };

    let ml = max_lag as i64;
    let mut best = 0i64;
    let mut best_v = f64::NEG_INFINITY;
    for lag in -ml..=ml {
        let v = corr(lag);
        if v > best_v {
            best_v = v;
            best = lag;
        }
    }

    // Parabolic refinement around the peak. Skipped at the search edges, where
    // one neighbour is unavailable.
    if best > -ml && best < ml {
        let cm = corr(best - 1);
        let cp = corr(best + 1);
        let denom = cm - 2.0 * best_v + cp;
        if denom.abs() > f64::EPSILON {
            return best as f32 + (0.5 * (cm - cp) / denom) as f32;
        }
    }
    best as f32
}
```

- [ ] **Step 4: Declare the module**

In `omniphony-renderer/dsp_fixtures/src/lib.rs`, add after `pub mod golden;`:

```rust
pub mod analysis;
```

- [ ] **Step 5: Run to verify the tests pass**

Run: `cd omniphony-renderer && cargo test -p dsp_fixtures analysis`
Expected: PASS — 7 tests.

- [ ] **Step 6: Commit**

```bash
git add omniphony-renderer/dsp_fixtures
git commit -m "test(fixtures): magnitude response and cross-correlation lag

Both verified against analytically known answers — a windowed sinc at a
known fractional offset for the lag estimator, a scaled unit impulse for
the response. Documents the lag sign convention explicitly."
```

---

## Task 6: Direction sets

**Files:**
- Create: `omniphony-renderer/dsp_fixtures/src/dirs.rs`
- Modify: `omniphony-renderer/dsp_fixtures/src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `dsp_fixtures::dirs::{fibonacci_sphere, horizontal_ring, meridian}`. Signatures, all returning `Vec<(f32, f32)>` of `(azimuth_deg, elevation_deg)`:
  - `fibonacci_sphere(n: usize) -> Vec<(f32, f32)>`
  - `horizontal_ring(step_deg: f32) -> Vec<(f32, f32)>`
  - `meridian(azimuth_deg: f32, step_deg: f32) -> Vec<(f32, f32)>`

- [ ] **Step 1: Write the failing tests**

`omniphony-renderer/dsp_fixtures/src/dirs.rs`:

```rust
//! Direction sets for sphere sweeps.
//!
//! Azimuth is degrees in `[-180, 180]` with 0 = front and +90 = right;
//! elevation is degrees in `[-90, 90]` with 0 = horizontal. This matches
//! `renderer::speaker_layout::Speaker`.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fibonacci_returns_the_requested_count() {
        assert_eq!(fibonacci_sphere(512).len(), 512);
        assert_eq!(fibonacci_sphere(1).len(), 1);
    }

    #[test]
    fn fibonacci_directions_are_in_range() {
        for (az, el) in fibonacci_sphere(2048) {
            assert!((-180.0..=180.0).contains(&az), "azimuth {az} out of range");
            assert!((-90.0..=90.0).contains(&el), "elevation {el} out of range");
        }
    }

    #[test]
    fn fibonacci_covers_both_hemispheres() {
        let dirs = fibonacci_sphere(512);
        assert!(dirs.iter().any(|(_, el)| *el > 60.0), "no near-zenith point");
        assert!(dirs.iter().any(|(_, el)| *el < -60.0), "no near-nadir point");
    }

    #[test]
    fn fibonacci_has_no_duplicate_directions() {
        let dirs = fibonacci_sphere(512);
        for i in 0..dirs.len() {
            for j in (i + 1)..dirs.len() {
                let (a, b) = (dirs[i], dirs[j]);
                assert!(
                    (a.0 - b.0).abs() > 1e-4 || (a.1 - b.1).abs() > 1e-4,
                    "duplicate direction at {i} and {j}: {a:?}"
                );
            }
        }
    }

    #[test]
    fn horizontal_ring_is_flat_and_closed() {
        let ring = horizontal_ring(10.0);
        assert_eq!(ring.len(), 36, "360/10 points, endpoint excluded");
        assert!(ring.iter().all(|(_, el)| el.abs() < 1e-6));
    }

    #[test]
    fn meridian_spans_pole_to_pole() {
        let m = meridian(30.0, 15.0);
        assert!(m.iter().all(|(az, _)| (az - 30.0).abs() < 1e-6));
        assert!((m.first().expect("non-empty").1 - -90.0).abs() < 1e-6);
        assert!((m.last().expect("non-empty").1 - 90.0).abs() < 1e-6);
    }
}
```

- [ ] **Step 2: Run to verify the tests fail**

Run: `cd omniphony-renderer && cargo test -p dsp_fixtures dirs`
Expected: FAIL — `cannot find function fibonacci_sphere`.

- [ ] **Step 3: Implement the direction sets**

Prepend to `omniphony-renderer/dsp_fixtures/src/dirs.rs`, above `mod tests`:

```rust
/// `n` approximately-uniform directions over the whole sphere, via a Fibonacci
/// lattice.
///
/// Preferred over a lat/long grid: a grid oversamples the poles badly, which
/// for a VBAP sweep means most of the test budget is spent re-measuring the
/// same two triplets.
pub fn fibonacci_sphere(n: usize) -> Vec<(f32, f32)> {
    assert!(n > 0, "fibonacci_sphere needs n > 0");
    // Golden angle: π(3 − √5).
    let golden_angle = std::f64::consts::PI * (3.0 - 5.0_f64.sqrt());
    (0..n)
        .map(|i| {
            // Half-step offsets give equal area per point and avoid landing
            // exactly on the poles.
            let z = 1.0 - 2.0 * (i as f64 + 0.5) / n as f64;
            let elevation = z.clamp(-1.0, 1.0).asin().to_degrees();
            let mut azimuth = (golden_angle * i as f64).to_degrees() % 360.0;
            if azimuth > 180.0 {
                azimuth -= 360.0;
            }
            (azimuth as f32, elevation as f32)
        })
        .collect()
}

/// Directions around the horizontal plane, `step_deg` apart, excluding the
/// duplicate endpoint at +180°.
pub fn horizontal_ring(step_deg: f32) -> Vec<(f32, f32)> {
    assert!(step_deg > 0.0, "step must be positive");
    let count = (360.0 / step_deg).round() as usize;
    (0..count)
        .map(|i| {
            let mut az = i as f32 * step_deg;
            if az > 180.0 {
                az -= 360.0;
            }
            (az, 0.0)
        })
        .collect()
}

/// Directions along one meridian at fixed azimuth, from nadir to zenith
/// inclusive.
pub fn meridian(azimuth_deg: f32, step_deg: f32) -> Vec<(f32, f32)> {
    assert!(step_deg > 0.0, "step must be positive");
    let steps = (180.0 / step_deg).round() as usize;
    (0..=steps)
        .map(|i| (azimuth_deg, -90.0 + i as f32 * step_deg))
        .collect()
}
```

- [ ] **Step 4: Declare the module**

In `omniphony-renderer/dsp_fixtures/src/lib.rs`, add after `pub mod analysis;`:

```rust
pub mod dirs;
```

- [ ] **Step 5: Run to verify the tests pass**

Run: `cd omniphony-renderer && cargo test -p dsp_fixtures dirs`
Expected: PASS — 6 tests.

- [ ] **Step 6: Commit**

```bash
git add omniphony-renderer/dsp_fixtures
git commit -m "test(fixtures): Fibonacci sphere lattice and sweep direction sets

A lat/long grid oversamples the poles, so a VBAP sweep on one would
spend most of its budget re-measuring the same two triplets."
```

---

## Task 7: LR4 reconstruction flatness — measurement

**Files:**
- Create: `omniphony-renderer/renderer/src/crossover/validation.rs`
- Modify: `omniphony-renderer/renderer/src/crossover/mod.rs`

**Interfaces:**
- Consumes: `dsp_fixtures::analysis::magnitude_response_db` (Task 5).
- Produces: nothing consumed by later tasks. Task 11 converts this measurement into an assertion.

- [ ] **Step 1: Write the measurement**

`omniphony-renderer/renderer/src/crossover/validation.rs`:

```rust
//! LR4 crossover reconstruction flatness.
//!
//! `filter.rs` documents that each split sums to a 2nd-order allpass, and that
//! for N bands every already-emitted band is passed through the current
//! splitter's compensating allpass, so the total is a cascade of N−1 allpasses.
//! An allpass has magnitude exactly 1, therefore:
//!
//!   **the sum of all bands must be magnitude-flat at 0 dB.**
//!
//! Any deviation is coefficient or float error, not a design property. That is
//! what makes this a theory-derived threshold rather than a ratchet.
//!
//! **Phase is deliberately not asserted.** Allpass summing rotates phase by
//! design; asserting flat phase would be asserting the filter is broken.

use dsp_fixtures::analysis::magnitude_response_db;

use super::filter::{BiquadState, LR4CrossoverBank};

/// Impulse response length. LR4 ringing at the lowest cutoff takes tens of
/// milliseconds; truncating it leaks into the spectrum and reads as passband
/// ripple, so this must stay long.
const IR_LEN: usize = 32_768;

/// The cutoffs the shipped band-limited layout produces (see
/// `dsp_fixtures::scene::crossover_layout`): three band edges, four bands.
const DEFAULT_CUTOFFS: [f32; 3] = [80.0, 200.0, 500.0];

const SAMPLE_RATE: u32 = 48_000;

/// Sum of all band outputs for a unit impulse — the reconstruction IR.
fn reconstruction_ir(cutoffs: &[f32], sample_rate: u32) -> Vec<f32> {
    let bank = LR4CrossoverBank::new(cutoffs, sample_rate);
    let mut states = vec![BiquadState::default(); bank.state_count()];
    (0..IR_LEN)
        .map(|i| {
            let x = if i == 0 { 1.0 } else { 0.0 };
            let bands = bank.process_sample(x, &mut states);
            (0..bands.len()).map(|b| bands.get(b)).sum()
        })
        .collect()
}

/// Worst deviation from 0 dB over the asserted band, as `(freq_hz, dev_db)`.
///
/// The band is `[4·fc_min, min(20 kHz, 0.45·fs)]`: bounded below because the
/// truncated IR is unreliable near DC, and above so the 44.1 kHz case does not
/// assert flatness into the anti-alias region near Nyquist.
fn worst_flatness_deviation(cutoffs: &[f32], sample_rate: u32) -> (f32, f32) {
    let ir = reconstruction_ir(cutoffs, sample_rate);
    let resp = magnitude_response_db(&ir, sample_rate);
    let fc_min = cutoffs.iter().copied().fold(f32::INFINITY, f32::min);
    let lo = 4.0 * fc_min;
    let hi = 20_000.0f32.min(0.45 * sample_rate as f32);
    let mut worst = (0.0f32, 0.0f32);
    for (freq, db) in resp {
        if freq < lo || freq > hi {
            continue;
        }
        if db.abs() > worst.1.abs() {
            worst = (freq, db);
        }
    }
    worst
}

#[test]
fn measure_lr4_reconstruction_flatness() {
    // PHASE 1: report only. Task 11 converts this into an assertion.
    let (freq, dev) = worst_flatness_deviation(&DEFAULT_CUTOFFS, SAMPLE_RATE);
    println!(
        "[measure] lr4_flatness cutoffs={DEFAULT_CUTOFFS:?} fs={SAMPLE_RATE}: \
         worst deviation {dev:+.4} dB at {freq:.1} Hz (target ±0.25 dB)"
    );
}
```

- [ ] **Step 2: Declare the module**

Append to `omniphony-renderer/renderer/src/crossover/mod.rs`:

```rust
#[cfg(test)]
mod validation;
```

- [ ] **Step 3: Run the measurement and record the result**

Run: `cd omniphony-renderer && cargo test -p renderer measure_lr4 -- --nocapture`
Expected: PASS, printing one `[measure] lr4_flatness …` line.

Record the printed deviation — Task 10 collects it.

- [ ] **Step 4: Sanity-check the measurement is meaningful**

A measurement that cannot fail is worthless. Temporarily change `IR_LEN` to
`512` and re-run: the deviation must grow substantially (truncation leakage).
**Restore `IR_LEN` to `32_768`** and confirm the original figure returns.

- [ ] **Step 5: Commit**

```bash
git add omniphony-renderer/renderer/src/crossover
git commit -m "test(crossover): measure LR4 reconstruction flatness

Reports worst deviation from 0 dB over [4*fc_min, min(20k, 0.45*fs)].
Report-only for now; Task 11 gates it at the theory-derived +/-0.25 dB.
Phase is deliberately not asserted — allpass summing rotates it."
```

---

## Task 8: VBAP energy and seam continuity — measurement

**Files:**
- Create: `omniphony-renderer/renderer/src/spatial_vbap/panner/native_validation.rs`
- Modify: `omniphony-renderer/renderer/src/spatial_vbap/panner.rs:219-220`

**Interfaces:**
- Consumes: `dsp_fixtures::dirs::fibonacci_sphere` (Task 6).
- Produces: nothing consumed by later tasks. Task 11 converts this into assertions.

- [ ] **Step 1: Write the measurement**

`omniphony-renderer/renderer/src/spatial_vbap/panner/native_validation.rs`:

```rust
//! VBAP energy conservation and seam continuity over the whole sphere.
//!
//! Extends the existing tests in `native_backend.rs`, which check
//! `|rms − 1| < 0.05` (±0.42 dB) at five elevations along the azimuth-0
//! meridian of a synthetic 7-speaker layout. This measures the shipped 7.1.4
//! layout over a full sphere lattice, and adds the metric energy conservation
//! cannot see.
//!
//! **Energy** — VBAP normalises so that `Σg² = 1`. In dB: `10·log10(Σg²) = 0`.
//!
//! **Seams** — VBAP is continuous by construction: gains fall to zero at a
//! triplet edge as the adjacent triplet takes over. So `‖g(θ+Δ) − g(θ)‖₂` must
//! scale with Δ. Measuring at Δ = 1° and Δ = 0.5°, the ratio must be ≈ 0.5. A
//! jump discontinuity at a triplet boundary does *not* halve — and it is
//! invisible to the energy check, since the image can jump while energy stays
//! perfectly conserved. Expressing the criterion as a ratio avoids inventing a
//! Lipschitz constant.

use dsp_fixtures::dirs::fibonacci_sphere;

use crate::speaker_layout::SpeakerLayout;

use super::native_backend::NativeVbapLayout;

/// Directions in the PR-gate sweep. 512 points is dense enough to land inside
/// every triplet of a 7.1.4 layout several times over.
const LATTICE_POINTS: usize = 512;

/// Build the VBAP panner for a shipped preset, using only speakers that
/// participate in spatialization (LFE has `spatialize: false` and must not
/// appear in the energy sum).
fn panner_for(preset: &str) -> (NativeVbapLayout, usize) {
    let layout = SpeakerLayout::preset(preset).expect("known preset");
    let dirs: Vec<[f32; 2]> = layout
        .speakers
        .iter()
        .filter(|s| s.spatialize)
        .map(|s| [s.azimuth, s.elevation])
        .collect();
    let n = dirs.len();
    (
        NativeVbapLayout::from_speaker_dirs(&dirs).expect("triplet search"),
        n,
    )
}

/// `10·log10(Σg²)` — deviation from 0 dB is the energy error.
fn energy_db(panner: &NativeVbapLayout, az: f32, el: f32) -> f32 {
    let g = panner.vbap_gains(az, el, 0.0).expect("vbap gains");
    let mut sum_sq = 0.0f32;
    for i in 0..g.len() {
        sum_sq += g[i] * g[i];
    }
    if sum_sq <= 0.0 {
        f32::NEG_INFINITY
    } else {
        10.0 * sum_sq.log10()
    }
}

/// `‖g(az+Δ) − g(az)‖₂` at fixed elevation.
fn gain_step_norm(panner: &NativeVbapLayout, az: f32, el: f32, delta: f32) -> f32 {
    let a = panner.vbap_gains(az, el, 0.0).expect("vbap gains");
    let b = panner.vbap_gains(az + delta, el, 0.0).expect("vbap gains");
    let mut acc = 0.0f32;
    for i in 0..a.len() {
        let d = a[i] - b[i];
        acc += d * d;
    }
    acc.sqrt()
}

#[test]
fn measure_vbap_energy_conservation() {
    // PHASE 1: report only. Task 11 converts this into an assertion.
    let (panner, n_spk) = panner_for("7.1.4");
    let mut worst = (0.0f32, 0.0f32, 0.0f32); // (az, el, dev_db)
    for (az, el) in fibonacci_sphere(LATTICE_POINTS) {
        let dev = energy_db(&panner, az, el);
        if !dev.is_finite() {
            println!("[measure] vbap_energy: SILENT direction az={az:.1} el={el:.1}");
            continue;
        }
        if dev.abs() > worst.2.abs() {
            worst = (az, el, dev);
        }
    }
    println!(
        "[measure] vbap_energy 7.1.4 ({n_spk} spatialized speakers, \
         {LATTICE_POINTS} directions): worst {:+.4} dB at az={:.1} el={:.1} \
         (target ±0.25 dB)",
        worst.2, worst.0, worst.1
    );
}

#[test]
fn measure_vbap_seam_continuity() {
    // PHASE 1: report only. Task 11 converts this into an assertion.
    let (panner, _) = panner_for("7.1.4");
    // Skip directions where the gain barely moves — the ratio is 0/0 there and
    // carries no information about continuity.
    const MIN_STEP_NORM: f32 = 1e-4;
    let mut worst = (0.0f32, 0.0f32, 0.0f32); // (az, el, ratio)
    for (az, el) in fibonacci_sphere(LATTICE_POINTS) {
        let coarse = gain_step_norm(&panner, az, el, 1.0);
        if coarse < MIN_STEP_NORM {
            continue;
        }
        let fine = gain_step_norm(&panner, az, el, 0.5);
        let ratio = fine / coarse;
        if ratio > worst.2 {
            worst = (az, el, ratio);
        }
    }
    println!(
        "[measure] vbap_seams 7.1.4 ({LATTICE_POINTS} directions): worst \
         ‖Δ0.5°‖/‖Δ1°‖ ratio {:.4} at az={:.1} el={:.1} \
         (continuous ⇒ ≈0.5, target <0.65)",
        worst.2, worst.0, worst.1
    );
}
```

- [ ] **Step 2: Declare the module**

In `omniphony-renderer/renderer/src/spatial_vbap/panner.rs`, after line 220
(`mod tests;`), append:

```rust
#[cfg(test)]
mod native_validation;
```

- [ ] **Step 3: Run and record**

Run: `cd omniphony-renderer && cargo test -p renderer measure_vbap -- --nocapture`
Expected: PASS, printing two `[measure] vbap_…` lines.

Record both printed figures — Task 10 collects them.

- [ ] **Step 4: Sanity-check the seam metric can fail**

Verify the ratio metric distinguishes continuous from discontinuous. Add this
temporary test, run it, then delete it:

```rust
#[test]
fn tmp_seam_metric_detects_a_step() {
    // A synthetic discontinuous "gain function": jumps at az = 0.
    let f = |az: f32| -> Vec<f32> { if az < 0.0 { vec![1.0, 0.0] } else { vec![0.0, 1.0] } };
    let norm = |a: &[f32], b: &[f32]| -> f32 {
        a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum::<f32>().sqrt()
    };
    let coarse = norm(&f(-0.5), &f(0.5));
    let fine = norm(&f(-0.25), &f(0.25));
    let ratio = fine / coarse;
    assert!(ratio > 0.65, "a step must not halve; ratio was {ratio}");
}
```

Run: `cd omniphony-renderer && cargo test -p renderer tmp_seam_metric -- --nocapture`
Expected: PASS (ratio 1.0). Then **delete this test**.

- [ ] **Step 5: Commit**

```bash
git add omniphony-renderer/renderer/src/spatial_vbap
git commit -m "test(vbap): measure energy conservation and seam continuity

Full-sphere Fibonacci sweep on the shipped 7.1.4 layout, versus the
existing azimuth-0 meridian check on a synthetic 7-speaker layout.

Seams use a threshold-free ratio: halving the angular step must halve
the gain-vector difference, since VBAP is continuous by construction.
A triplet-boundary jump does not halve, and is invisible to the energy
check because energy stays conserved while the image jumps."
```

---

## Task 9: End-to-end binaural ITD — measurement

**Files:**
- Create: `omniphony-renderer/renderer/src/binaural/validation.rs`
- Modify: `omniphony-renderer/renderer/src/binaural/mod.rs:31`
- Modify: `omniphony-renderer/dsp_fixtures/src/scene.rs`

**Interfaces:**
- Consumes: `dsp_fixtures::analysis::estimate_lag_samples` (Task 5); `dsp_fixtures::scene::prepared_binaural` (Task 4).
- Produces: `dsp_fixtures::scene::render_single_object_binaural`. Signature:
  - `render_single_object_binaural(azimuth_deg: f32, blocks: usize) -> (Vec<f32>, Vec<f32>)` returning `(left, right)` deinterleaved.

- [ ] **Step 1: Add the single-object binaural scene helper**

Append to `omniphony-renderer/dsp_fixtures/src/scene.rs`:

```rust
/// Render one object at a fixed horizontal azimuth through the binaural path,
/// returning deinterleaved `(left, right)`.
///
/// Position is set in Omniphony normalized Cartesian, where +x is right, +y is
/// front and +z is up (see `layouts/7.1.4.yaml`), so azimuth θ measured from
/// front toward the right is `(sin θ, cos θ, 0)`.
///
/// The binaural path uses `unit_scale_m` and ignores the anisotropic
/// `room_ratio` (see `BINAURAL.md`), so the azimuth is not distorted by the
/// `[1.0, 2.0, 0.5]` ratio the fixture renderer is built with.
///
/// The first `PRIME_BLOCKS` blocks are discarded: the 20 ms gain slew and the
/// position ramp must settle before the lag measurement is meaningful.
pub fn render_single_object_binaural(azimuth_deg: f32, blocks: usize) -> (Vec<f32>, Vec<f32>) {
    const PRIME_BLOCKS: usize = 64;

    let theta = (azimuth_deg as f64).to_radians();
    let position = [theta.sin(), theta.cos(), 0.0];

    let mut r = make_renderer("7.1.4", true, false);
    {
        let ctrl = r.renderer_control();
        ctrl.set_requested_ramp_mode(RampMode::Frame);
        let mut live = ctrl.live.write();
        live.ramp_mode = RampMode::Frame;
        live.binaural.output_mode = OutputMode::Binaural;
    }

    let pcm = make_pcm(1);
    let event = vec![SpatialChannelEvent {
        channel_idx: 0,
        is_bed: false,
        gain_db: Some(0),
        ramp_length: Some(BLOCK_SAMPLES as u32),
        size: Some([0.0, 0.0, 0.0]),
        position: Some(position),
        sample_pos: Some(0),
    }];

    let mut buf = Vec::new();
    for _ in 0..PRIME_BLOCKS {
        let f = r
            .render_frame(&pcm, 1, &event, buf, false)
            .expect("prime binaural ITD render");
        buf = f.samples;
        buf.clear();
    }

    let mut left = Vec::with_capacity(blocks * BLOCK_SAMPLES);
    let mut right = Vec::with_capacity(blocks * BLOCK_SAMPLES);
    for _ in 0..blocks {
        let f = r
            .render_frame(&pcm, 1, &event, buf, false)
            .expect("binaural ITD render");
        for frame in f.samples.chunks_exact(2) {
            left.push(frame[0]);
            right.push(frame[1]);
        }
        buf = f.samples;
        buf.clear();
    }
    (left, right)
}
```

- [ ] **Step 2: Write the measurement**

`omniphony-renderer/renderer/src/binaural/validation.rs`:

```rust
//! End-to-end interaural time difference.
//!
//! This deliberately does **not** compare `itd::ear_delays_seconds` against
//! Woodworth's formula — `itd.rs` *implements* Woodworth, so such a test would
//! be circular and would prove nothing. Instead it measures the lag between the
//! left and right channels of an actual binaural render, which exercises the
//! delay lines, the convolver, the interpolation and the head-pose rotation as
//! a chain.
//!
//! Three properties, because per-ear HRIR group delay biases any raw comparison
//! against the model:
//!
//! 1. **Antisymmetry** — `lag(+az) = −lag(−az)`, and `lag(0°) ≈ 0`. Structural,
//!    so it is immune to that bias.
//! 2. **Monotonicity** — |lag| grows from 0° toward 90°.
//! 3. **Magnitude** — within ±3 samples of the model, the tolerance absorbing
//!    the group delay.

use dsp_fixtures::analysis::estimate_lag_samples;
use dsp_fixtures::scene::render_single_object_binaural;

use super::itd::{DEFAULT_HEAD_RADIUS_M, ear_delays_seconds};

/// 128 blocks of 40 samples = 5120 samples, ample for a ±64-sample search.
const BLOCKS: usize = 128;
const MAX_LAG: usize = 64;
const SAMPLE_RATE: f32 = 48_000.0;

/// Azimuths measured in the PR gate. 0 and ±90 bracket the range; the
/// intermediate angles catch a sign error that the extremes would not.
const AZIMUTHS: [f32; 7] = [0.0, 30.0, -30.0, 60.0, -60.0, 90.0, -90.0];

/// Measured lag in samples: positive means the right channel is delayed, so a
/// source on the right (positive azimuth) yields a negative value.
fn measured_lag(azimuth_deg: f32) -> f32 {
    let (left, right) = render_single_object_binaural(azimuth_deg, BLOCKS);
    estimate_lag_samples(&left, &right, MAX_LAG)
}

/// Model lag in samples, matching the sign convention of [`measured_lag`].
///
/// `ear_delays_seconds` returns `(left_delay, right_delay)`, both ≥ 0, with the
/// far ear carrying the delay. `right_delay − left_delay` is therefore positive
/// when the right ear is the far one, which is the same convention as the
/// cross-correlation estimate.
fn model_lag(azimuth_deg: f32) -> f32 {
    let (l, r) = ear_delays_seconds(
        (azimuth_deg).to_radians(),
        0.0,
        DEFAULT_HEAD_RADIUS_M,
    );
    (r - l) * SAMPLE_RATE
}

#[test]
fn measure_binaural_itd() {
    // PHASE 1: report only. Task 11 converts this into assertions.
    for az in AZIMUTHS {
        let measured = measured_lag(az);
        let model = model_lag(az);
        println!(
            "[measure] itd az={az:+6.1}°: measured {measured:+7.3} samples \
             ({:+7.1} µs), model {model:+7.3} samples, delta {:+.3} samples \
             (target ±3)",
            measured / SAMPLE_RATE * 1e6,
            measured - model
        );
    }

    // Antisymmetry and monotonicity, reported as derived quantities.
    for az in [30.0f32, 60.0, 90.0] {
        let pos = measured_lag(az);
        let neg = measured_lag(-az);
        println!(
            "[measure] itd antisymmetry ±{az:.0}°: {pos:+.3} vs {neg:+.3}, \
             sum {:+.3} samples (target |sum| ≤ 1)",
            pos + neg
        );
    }
    let mags: Vec<f32> = [0.0f32, 30.0, 60.0, 90.0]
        .iter()
        .map(|az| measured_lag(*az).abs())
        .collect();
    println!(
        "[measure] itd monotonicity |lag| at 0/30/60/90°: {mags:?} \
         (target strictly increasing)"
    );
}
```

- [ ] **Step 3: Declare the module**

In `omniphony-renderer/renderer/src/binaural/mod.rs`, after line 31
(`pub mod tracking;`), append:

```rust
#[cfg(test)]
mod validation;
```

- [ ] **Step 4: Run and record**

Run: `cd omniphony-renderer && cargo test -p renderer measure_binaural_itd -- --nocapture`
Expected: PASS, printing 7 per-azimuth lines, 3 antisymmetry lines and 1
monotonicity line.

If `DEFAULT_HEAD_RADIUS_M` or `ear_delays_seconds` is not importable as written,
check the actual visibility in `omniphony-renderer/renderer/src/binaural/itd.rs`
— both are `pub` there — and adjust the `use` path only.

Record all figures — Task 10 collects them.

- [ ] **Step 5: Verify the measurement is sensitive to the head radius**

A measurement that ignores its input is broken. Temporarily change the
`head_radius_m` in `render_single_object_binaural` by setting
`live.binaural.head_radius_m = 0.15;` next to the `output_mode` line, re-run,
and confirm the measured lags grow. **Remove that line** afterwards and confirm
the original figures return.

- [ ] **Step 6: Commit**

```bash
git add omniphony-renderer/renderer/src/binaural omniphony-renderer/dsp_fixtures
git commit -m "test(binaural): measure end-to-end ITD from rendered output

Cross-correlates the rendered L/R pair rather than re-asserting
Woodworth against itd.rs, which implements it — that comparison would
be circular. Reports antisymmetry and monotonicity alongside the
magnitude, since per-ear HRIR group delay biases the raw comparison."
```

---

## Task 10: Measurement report

**Files:**
- Create: `docs/dsp-validation-report.md`

**Interfaces:**
- Consumes: the printed output of Tasks 7, 8, 9.
- Produces: the document Task 11 reads to decide which thresholds become gates.

- [ ] **Step 1: Run the whole measurement set and capture output**

```bash
cd omniphony-renderer
cargo test -p renderer -- --nocapture 2>&1 | grep '^\[measure\]' | tee /tmp/measure.txt
```

Expected: every `[measure]` line from Tasks 7, 8 and 9.

- [ ] **Step 2: Write the report**

Create `docs/dsp-validation-report.md` with this structure, filling the
**Measured** column from `/tmp/measure.txt` and the **Verdict** column with
`meets` or `misses`:

```markdown
# DSP Validation — Phase 1 Measurement Report

Measured on <date>, commit <sha>, x86_64 Linux, `cargo test -p renderer`.

Thresholds are theory-derived (see
`docs/superpowers/specs/2026-07-30-dsp-validation-harness-design.md`, D2). A
"misses" verdict is a finding about the engine, not a defect in the test.

| Metric | Theoretical target | Measured | Verdict |
| --- | --- | --- | --- |
| LR4 reconstruction flatness (4 bands, 48 kHz) | ±0.25 dB | | |
| VBAP energy conservation (7.1.4, 512 dirs) | ±0.25 dB | | |
| VBAP seam continuity ratio (7.1.4, 512 dirs) | < 0.65 | | |
| ITD magnitude vs model (worst of 7 azimuths) | ±3 samples | | |
| ITD antisymmetry (worst of ±30/60/90°) | \|sum\| ≤ 1 sample | | |
| ITD monotonicity (0/30/60/90°) | strictly increasing | | |

## Raw output

```
<paste /tmp/measure.txt verbatim>
```

## Gating decision

For each metric marked "misses": file an issue, record its number here, and
note that Task 11 will land it as a tracked deferral rather than a gate.

| Metric | Issue | Deferred value recorded in `#[ignore]` |
| --- | --- | --- |
```

- [ ] **Step 3: Commit**

```bash
git add docs/dsp-validation-report.md
git commit -m "docs: phase 1 DSP validation measurement report

Records what the engine achieves against each theory-derived target,
so the gating decision in phase 2 is made against evidence rather than
discovered as CI failures."
```

- [ ] **Step 4: Report to the user before proceeding**

Phase 2 is a decision point, not a mechanical continuation. Summarize for the
user: which metrics meet their target, which miss and by how much, and what
the follow-up backlog looks like. **Wait for confirmation** before starting
Task 11.

---

# Phase 2 — Gate

## Task 11: Convert measurements into gates or tracked deferrals

**Files:**
- Modify: `omniphony-renderer/renderer/src/crossover/validation.rs`
- Modify: `omniphony-renderer/renderer/src/spatial_vbap/panner/native_validation.rs`
- Modify: `omniphony-renderer/renderer/src/binaural/validation.rs`

**Interfaces:**
- Consumes: `docs/dsp-validation-report.md` (Task 10).
- Produces: nothing consumed by later tasks.

**For every metric below:** if the report says *meets*, rename the test from
`measure_*` to the gate name given and add the assertion. If the report says
*misses*, do the same **and** add
`#[ignore = "engine misses this: measured <X>, target <Y> — see <issue>"]`
immediately above the `#[test]` attribute. Keep the `println!` in both cases —
a passing gate that also reports its margin is far easier to trend.

- [ ] **Step 1: Gate LR4 flatness**

In `omniphony-renderer/renderer/src/crossover/validation.rs`, add the constant
and replace the test:

```rust
/// Theory-derived: the band sum is a cascade of allpasses, so magnitude is
/// exactly 1. This bound is float and coefficient error only.
const FLATNESS_TOLERANCE_DB: f32 = 0.25;

#[test]
fn lr4_reconstruction_is_magnitude_flat() {
    let (freq, dev) = worst_flatness_deviation(&DEFAULT_CUTOFFS, SAMPLE_RATE);
    println!(
        "[measure] lr4_flatness cutoffs={DEFAULT_CUTOFFS:?} fs={SAMPLE_RATE}: \
         worst deviation {dev:+.4} dB at {freq:.1} Hz"
    );
    assert!(
        dev.abs() <= FLATNESS_TOLERANCE_DB,
        "LR4 band sum deviates {dev:+.4} dB from flat at {freq:.1} Hz, \
         tolerance ±{FLATNESS_TOLERANCE_DB} dB. The sum of LR4 bands is an \
         allpass cascade, so its magnitude must be 1 — a deviation means the \
         coefficients or the multiway compensation are wrong."
    );
}
```

- [ ] **Step 2: Gate VBAP energy and seams**

In `omniphony-renderer/renderer/src/spatial_vbap/panner/native_validation.rs`,
add the constants and replace both tests:

```rust
/// Theory-derived: VBAP normalises to `Σg² = 1`, i.e. 0 dB.
const ENERGY_TOLERANCE_DB: f32 = 0.25;

/// Halving the angular step must roughly halve the gain-vector difference.
/// 0.65 leaves headroom over the ideal 0.5 for curvature within a triplet,
/// while still rejecting a jump discontinuity (ratio ≈ 1).
const MAX_SEAM_RATIO: f32 = 0.65;

#[test]
fn vbap_conserves_energy_over_the_sphere() {
    let (panner, n_spk) = panner_for("7.1.4");
    let mut worst = (0.0f32, 0.0f32, 0.0f32);
    for (az, el) in fibonacci_sphere(LATTICE_POINTS) {
        let dev = energy_db(&panner, az, el);
        assert!(
            dev.is_finite(),
            "silent direction az={az:.1} el={el:.1}: no speaker receives energy"
        );
        if dev.abs() > worst.2.abs() {
            worst = (az, el, dev);
        }
    }
    println!(
        "[measure] vbap_energy 7.1.4 ({n_spk} speakers, {LATTICE_POINTS} dirs): \
         worst {:+.4} dB at az={:.1} el={:.1}",
        worst.2, worst.0, worst.1
    );
    assert!(
        worst.2.abs() <= ENERGY_TOLERANCE_DB,
        "VBAP energy off by {:+.4} dB at az={:.1} el={:.1}, tolerance \
         ±{ENERGY_TOLERANCE_DB} dB",
        worst.2,
        worst.0,
        worst.1
    );
}

#[test]
fn vbap_gains_are_continuous_across_triplet_boundaries() {
    let (panner, _) = panner_for("7.1.4");
    const MIN_STEP_NORM: f32 = 1e-4;
    let mut worst = (0.0f32, 0.0f32, 0.0f32);
    for (az, el) in fibonacci_sphere(LATTICE_POINTS) {
        let coarse = gain_step_norm(&panner, az, el, 1.0);
        if coarse < MIN_STEP_NORM {
            continue;
        }
        let ratio = gain_step_norm(&panner, az, el, 0.5) / coarse;
        if ratio > worst.2 {
            worst = (az, el, ratio);
        }
    }
    println!(
        "[measure] vbap_seams 7.1.4 ({LATTICE_POINTS} dirs): worst ratio \
         {:.4} at az={:.1} el={:.1}",
        worst.2, worst.0, worst.1
    );
    assert!(
        worst.2 <= MAX_SEAM_RATIO,
        "gain vector does not halve when the step halves at az={:.1} el={:.1} \
         (ratio {:.4}, max {MAX_SEAM_RATIO}) — a seam, i.e. the panned image \
         jumps at a triplet boundary even though energy stays conserved",
        worst.0,
        worst.1,
        worst.2
    );
}
```

- [ ] **Step 3: Gate the three ITD properties**

In `omniphony-renderer/renderer/src/binaural/validation.rs`, replace the single
measurement test with three gates:

```rust
/// Absorbs per-ear HRIR group delay, which is not part of the Woodworth model.
const MAGNITUDE_TOLERANCE_SAMPLES: f32 = 3.0;

/// Antisymmetry is structural, so the bound is tight.
const ANTISYMMETRY_TOLERANCE_SAMPLES: f32 = 1.0;

#[test]
fn itd_magnitude_tracks_the_model() {
    for az in AZIMUTHS {
        let measured = measured_lag(az);
        let model = model_lag(az);
        let delta = measured - model;
        println!(
            "[measure] itd az={az:+6.1}°: measured {measured:+7.3}, \
             model {model:+7.3}, delta {delta:+.3} samples"
        );
        assert!(
            delta.abs() <= MAGNITUDE_TOLERANCE_SAMPLES,
            "ITD at az={az:+.1}° is {measured:+.3} samples but the model says \
             {model:+.3} (delta {delta:+.3}, tolerance \
             ±{MAGNITUDE_TOLERANCE_SAMPLES})"
        );
    }
}

#[test]
fn itd_is_antisymmetric_about_the_median_plane() {
    let centre = measured_lag(0.0);
    println!("[measure] itd az=0°: {centre:+.3} samples");
    assert!(
        centre.abs() <= ANTISYMMETRY_TOLERANCE_SAMPLES,
        "a source dead ahead must have no ITD, measured {centre:+.3} samples"
    );
    for az in [30.0f32, 60.0, 90.0] {
        let pos = measured_lag(az);
        let neg = measured_lag(-az);
        println!(
            "[measure] itd antisymmetry ±{az:.0}°: {pos:+.3} vs {neg:+.3}, \
             sum {:+.3}",
            pos + neg
        );
        assert!(
            (pos + neg).abs() <= ANTISYMMETRY_TOLERANCE_SAMPLES,
            "ITD must be antisymmetric: az=+{az:.0}° gives {pos:+.3} and \
             az=-{az:.0}° gives {neg:+.3}, sum {:+.3} exceeds \
             ±{ANTISYMMETRY_TOLERANCE_SAMPLES}",
            pos + neg
        );
    }
}

#[test]
fn itd_magnitude_grows_toward_the_interaural_axis() {
    let mags: Vec<f32> = [0.0f32, 30.0, 60.0, 90.0]
        .iter()
        .map(|az| measured_lag(*az).abs())
        .collect();
    println!("[measure] itd monotonicity |lag| at 0/30/60/90°: {mags:?}");
    for w in mags.windows(2) {
        assert!(
            w[1] > w[0],
            "|ITD| must increase toward the interaural axis, got {mags:?}"
        );
    }
}
```

- [ ] **Step 4: Run the full suite**

Run: `cd omniphony-renderer && cargo test --workspace`
Expected: PASS. Any metric the report marked *misses* must be `#[ignore]`d with
its measured value in the reason, so the suite is green.

- [ ] **Step 5: Confirm the deferrals are visible**

Run: `cd omniphony-renderer && cargo test --workspace -- --ignored`
Expected: only the deferred gates run, and they fail with their real numbers.
This is the command that shows the outstanding backlog.

- [ ] **Step 6: Confirm the time budget still holds**

Run: `cd omniphony-renderer && /usr/bin/time -f "%e s" cargo test --workspace`
Expected: under 10 s total. If over, reduce `LATTICE_POINTS` to 256 and
`BLOCKS` in the ITD test to 64, then re-verify.

- [ ] **Step 7: Commit**

```bash
git add omniphony-renderer/renderer/src
git commit -m "test(dsp): gate the acceptance metrics at theory-derived bounds

LR4 magnitude flatness +/-0.25 dB, VBAP energy +/-0.25 dB, VBAP seam
ratio <0.65, ITD magnitude +/-3 samples with antisymmetry and
monotonicity. Metrics the engine currently misses are #[ignore]d with
their measured value and an issue reference, per the spec's triage
policy — a tracked deferral, never a silent pass."
```

---

## Task 12: Wide matrix behind a feature, and documentation

**Files:**
- Modify: `omniphony-renderer/renderer/Cargo.toml`
- Modify: `omniphony-renderer/renderer/src/crossover/validation.rs`
- Modify: `omniphony-renderer/renderer/src/spatial_vbap/panner/native_validation.rs`
- Modify: `omniphony-renderer/renderer/src/binaural/validation.rs`
- Create: `omniphony-renderer/dsp_fixtures/README.md`
- Modify: `CONTRIBUTING.md`

**Interfaces:**
- Consumes: everything from Tasks 1-11.
- Produces: nothing.

- [ ] **Step 1: Expose the feature through `renderer`**

In `omniphony-renderer/renderer/Cargo.toml`, add to the `[features]` section
(create it if absent):

```toml
[features]
# Opt-in wide DSP validation matrix. Off by default so the PR gate stays
# inside its CI time budget; #[ignore] is reserved for tracked deferrals.
# Gates only `#[cfg(feature = "wide-matrix")]` test code in this crate — it
# forwards to nothing, so it cannot affect a release build.
wide-matrix = []
```

- [ ] **Step 2: Add the wide LR4 cases**

Append to `omniphony-renderer/renderer/src/crossover/validation.rs`:

```rust
/// The wide matrix: every band count against every supported sample rate.
/// Compiled only with `--features wide-matrix`.
#[cfg(feature = "wide-matrix")]
#[test]
fn lr4_reconstruction_is_magnitude_flat_wide() {
    const CUTOFF_SETS: [&[f32]; 3] = [&[500.0], &[200.0, 2000.0], &[80.0, 200.0, 500.0]];
    for fs in [44_100u32, 48_000, 96_000] {
        for cutoffs in CUTOFF_SETS {
            let (freq, dev) = worst_flatness_deviation(cutoffs, fs);
            println!(
                "[measure] lr4_flatness_wide cutoffs={cutoffs:?} fs={fs}: \
                 {dev:+.4} dB at {freq:.1} Hz"
            );
            assert!(
                dev.abs() <= FLATNESS_TOLERANCE_DB,
                "LR4 sum deviates {dev:+.4} dB at {freq:.1} Hz \
                 (cutoffs {cutoffs:?}, fs {fs}), tolerance \
                 ±{FLATNESS_TOLERANCE_DB} dB"
            );
        }
    }
}
```

- [ ] **Step 3: Add the wide VBAP cases**

Append to `omniphony-renderer/renderer/src/spatial_vbap/panner/native_validation.rs`:

```rust
/// The wide matrix: every shipped layout at a denser lattice, plus spread.
/// Compiled only with `--features wide-matrix`.
#[cfg(feature = "wide-matrix")]
#[test]
fn vbap_conserves_energy_over_the_sphere_wide() {
    const WIDE_POINTS: usize = 8192;
    for preset in ["5.1.2", "7.1.2", "7.1.4", "9.1.6"] {
        let (panner, n_spk) = panner_for(preset);
        for spread in [0.0f32, 0.25, 0.5, 1.0] {
            let mut worst = (0.0f32, 0.0f32, 0.0f32);
            for (az, el) in fibonacci_sphere(WIDE_POINTS) {
                let g = panner.vbap_gains(az, el, spread).expect("vbap gains");
                let mut sum_sq = 0.0f32;
                for i in 0..g.len() {
                    sum_sq += g[i] * g[i];
                }
                assert!(
                    sum_sq > 0.0,
                    "{preset} spread={spread}: silent direction az={az:.1} el={el:.1}"
                );
                let dev = 10.0 * sum_sq.log10();
                if dev.abs() > worst.2.abs() {
                    worst = (az, el, dev);
                }
            }
            println!(
                "[measure] vbap_energy_wide {preset} spread={spread} \
                 ({n_spk} speakers): worst {:+.4} dB at az={:.1} el={:.1}",
                worst.2, worst.0, worst.1
            );
            assert!(
                worst.2.abs() <= ENERGY_TOLERANCE_DB,
                "{preset} spread={spread}: energy off by {:+.4} dB at \
                 az={:.1} el={:.1}",
                worst.2,
                worst.0,
                worst.1
            );
        }
    }
}
```

- [ ] **Step 4: Add the wide ITD cases**

Append to `omniphony-renderer/renderer/src/binaural/validation.rs`:

```rust
/// The wide matrix: a full azimuth grid at several elevations.
/// Compiled only with `--features wide-matrix`.
#[cfg(feature = "wide-matrix")]
#[test]
fn itd_magnitude_tracks_the_model_wide() {
    for az_i in -6..=6 {
        let az = az_i as f32 * 30.0;
        let measured = measured_lag(az);
        let model = model_lag(az);
        let delta = measured - model;
        println!(
            "[measure] itd_wide az={az:+6.1}°: measured {measured:+7.3}, \
             model {model:+7.3}, delta {delta:+.3}"
        );
        assert!(
            delta.abs() <= MAGNITUDE_TOLERANCE_SAMPLES,
            "ITD at az={az:+.1}°: delta {delta:+.3} samples exceeds \
             ±{MAGNITUDE_TOLERANCE_SAMPLES}"
        );
    }
}
```

- [ ] **Step 5: Verify both invocations**

```bash
cd omniphony-renderer
cargo test --workspace                                  # gate: wide cases absent
cargo test --workspace --features renderer/wide-matrix  # wide cases run
```

Expected: the first does not run any `*_wide` test; the second does. Confirm by
comparing the reported test counts.

- [ ] **Step 6: Write the harness README**

`omniphony-renderer/dsp_fixtures/README.md`:

```markdown
# dsp_fixtures

Deterministic scenes, committed goldens and signal analysis for Omniphony's DSP
validation. **Dev-only** — nothing in the dependency graph of `orender` or
`liborender` references this crate, so release builds never compile it.

It exists so the null test, the criterion benches, and the worst-case
block-time gate all measure *the same* scenes. Duplicating scene generation
between those consumers is how they silently drift apart.

## Running

```sh
cargo test --workspace                                  # the PR gate
cargo test --workspace --features renderer/wide-matrix  # full validation matrix
cargo test --workspace -- --ignored                     # known-failing gates only
```

`--ignored` is **not** how you run the wide matrix. `#[ignore]` is reserved for
gates the engine currently misses, each carrying its measured value and an issue
reference. That command shows the outstanding DSP backlog.

## Goldens

Raw little-endian `f32`, headerless — the same layout the renderer's file sink
writes, so a golden can be auditioned directly:

```sh
ffplay -f f32le -ar 48000 -ac 12 goldens/speaker_714_32obj.f32
```

Comparison is **not** bit-exact. It passes when the peak residual is below
−120 dBFS, which is immune to LLVM re-vectorization changing the last mantissa
bit and lets the harness run on aarch64.

### Regenerating a golden

```sh
OMNIPHONY_BLESS_GOLDENS=1 cargo test -p renderer -- --nocapture
```

This prints the residual it replaced. **Quote that number in the pull request.**
The files are binary, so the diff carries no information — the printed residual
is the only reviewable artifact, and without it "I regenerated the goldens"
silently voids the safety net.

A failing null test dumps its render beside the golden as `<name>.actual.f32`
(gitignored) for offline inspection.

## Thresholds

Acceptance thresholds are derived from what each algorithm should achieve, never
measured from current behaviour. See
`docs/superpowers/specs/2026-07-30-dsp-validation-harness-design.md` and the
measurement report at `docs/dsp-validation-report.md`.
```

- [ ] **Step 7: Point CONTRIBUTING.md at the harness**

Append to `CONTRIBUTING.md`:

```markdown
## DSP validation

Changes to the render path, the crossover, the VBAP panner or the binaural
stage are covered by a golden/null test and a set of acceptance measurements.
They run as part of `cargo test --workspace`, so a behaviour change shows up as
a failing null test rather than as a surprise in a listening session.

If your change *intentionally* alters the rendered output, regenerate the
goldens and **quote the printed residual in your pull request**:

```sh
cd omniphony-renderer
OMNIPHONY_BLESS_GOLDENS=1 cargo test -p renderer -- --nocapture
```

See `omniphony-renderer/dsp_fixtures/README.md` for the full contract, the wide
matrix, and how deferred thresholds are tracked.
```

- [ ] **Step 8: Final verification**

```bash
cd omniphony-renderer
cargo fmt --all -- --check
cargo build --workspace
/usr/bin/time -f "TOTAL %e s" cargo test --workspace
```

Expected: formatting clean, build clean, suite green and under 10 s.

- [ ] **Step 9: Commit**

```bash
git add omniphony-renderer CONTRIBUTING.md
git commit -m "test(dsp): wide matrix behind a feature, plus harness docs

The wide matrix is opt-in via --features renderer/wide-matrix, keeping
#[ignore] free for tracked deferrals so each command has a meaningful
exit code. Documents the bless workflow and why the printed residual
must be quoted in a PR."
```

---

## Self-Review Notes

**Spec coverage.** Every section of the design document maps to a task: D1 →
Task 3 (`RESIDUAL_GATE_DBFS`); D2 → Tasks 7-9 report-only, Task 11 triage; D2a →
the Phase 1 / Phase 2 split with the checkpoint at Task 10 Step 4; D3 → Tasks 1-2;
D4 → Task 12. Architecture, null test, all three acceptance families, harness
self-tests, CI integration and failure ergonomics each have a task. The
`scene.rs`/bench-migration risk from the spec's Risks section is addressed by
Task 2 Step 7.

**Three spec deviations, all deliberate.**

1. The third golden is named `crossover_bands`, not `crossover_5_1_2` —
   `crossover_layout()` band-limits the 7.1.4 preset rather than using a 5.1.2
   layout, so it renders 12 channels and the old name would have been wrong
   (Task 4 Step 2 carries the rename).
2. Total golden size is ~1.25 MB rather than the spec's ~1.05 MB, because that
   third scene is 12-channel rather than 8-channel.
3. The `wide-matrix` feature lives on **`renderer`**, not on `dsp_fixtures` as
   D4 states. The wide cases are `#[cfg]`-gated test code inside `renderer`, so
   the feature has to be declared there; on `dsp_fixtures` it would be dead. The
   invocation is therefore
   `cargo test --workspace --features renderer/wide-matrix`. Update D4 in the
   spec to match.

**Verification steps are deliberate.** Tasks 3, 7, 8 and 9 each include a step
that perturbs the system to prove the test can fail. A green validation harness
that cannot detect a regression is worse than none, because it is trusted.
