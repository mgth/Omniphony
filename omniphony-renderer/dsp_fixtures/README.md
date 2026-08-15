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
