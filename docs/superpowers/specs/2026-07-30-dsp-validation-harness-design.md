# Objective DSP Validation Harness — Design

**Date:** 2026-07-30
**Status:** Approved for planning
**Scope:** `omniphony-renderer` — new `dsp_fixtures` crate, in-lib tests, no new CI job

## Problem

The renderer has ~440 unit tests but no test that renders audio end to end and
checks the result numerically. The only end-to-end harness,
`omniphony-renderer/orender_engine/tests/parity.rs`, is skipped unless
`ORENDER_BRIDGE` and `ORENDER_SAMPLE` point at a proprietary bridge and sample,
so CI never renders a frame of audio. There are no golden files anywhere.

Two consequences:

- A refactor of the render path cannot be shown to preserve output. This blocks
  the render-thread real-time-safety work (improvement #7), where the whole
  claim is "same samples, no allocations".
- Correctness claims about the DSP are qualitative. Nothing asserts that VBAP
  conserves energy, that the crossover reconstructs flat, or that binaural ITD
  matches the model it is derived from.

## Goals

1. A **null test**: render fixed scenes, compare against committed goldens, fail
   on a residual above a stated threshold.
2. **Acceptance tests** for three DSP units, with thresholds derived from what
   the algorithms should achieve rather than from what they currently do.
3. A **shared deterministic fixture layer**, so the null test, the existing
   benches, and the future performance gate (#9) all measure the same scenes.
4. Stay cheap: **under 2 s added** to `cargo test --workspace`, **no new CI job**.

## Non-goals

- Bit-exact reproducibility across toolchains or architectures.
- Perceptual or listening-based evaluation.
- Anything belonging to #7 (RT refactor), #9 (perf gate), #4 (loudness), or #10
  (motion-to-sound latency). This harness is the foundation those build on, not
  a home for them.

## Decisions

### D1 — Comparison is tolerance-gated in dB, not bit-exact

Pass if the **peak residual is below −120 dBFS**, where dBFS is
`20·log10(|x|)` with full scale at `1.0` — i.e. the largest absolute
sample-by-sample difference between render and golden, expressed in dB.

CI uses `dtolnay/rust-toolchain@stable`, unpinned. A new LLVM can re-vectorize
the mix loops and shift the last mantissa bit with no source change, so
bit-exact goldens would go red for no reason and would lock the harness to
x86_64 forever. A residual expressed in dB is how audio null tests are
specified in practice, is immune to that churn, and lets the harness run on
aarch64 whenever wanted.

Accepted cost: a genuine 1-LSB error could hide. It would sit ~100 dB below
anything audible or structurally meaningful.

### D2 — Thresholds are theory-derived, and may fail on day one

Thresholds come from what each algorithm should achieve, not from measuring
current behaviour and locking it in. A red test on first run is a **finding
about the engine**, not a broken test.

**Triage policy** (required, or `main` stays red indefinitely): a threshold the
engine misses gets an issue filed, and its test is marked
`#[ignore = "engine misses this: measured X, target Y — see <issue>"]`. The test
stays in the tree and the ignore is removed when the underlying defect is fixed.
Ignoring is a tracked deferral, never a silent pass.

`#[ignore]` is reserved **exclusively** for these tracked deferrals. The wide
matrix uses a separate mechanism (D4) so that "run the wide matrix" and "run the
known-failing tests" are different actions — otherwise the deferral escape hatch
and the opt-in matrix collide, and neither command has a meaningful exit code.

### D2a — Measure before gating

Implementation runs in two phases, so the findings are known before any
threshold becomes a merge gate.

**Phase 1 — measure.** Build the fixtures, the null test, and all three
acceptance measurements, but have each acceptance case *report* its metric
rather than assert on it. Produce one measurement report: per case, the
theoretical target and the value the engine actually achieves. The null test is
asserted from the start — it has no theoretical threshold to discover, and it is
what protects #7.

**Phase 2 — gate.** With the report in hand, convert each acceptance measurement
into an assertion. Cases that meet their target become PR gates; cases that miss
become tracked deferrals under D2, with the measured value recorded.

This keeps the discovery of defects separate from the decision to block merges on
them, and means the size of any follow-up backlog is known before it is
inherited.

### D3 — Fixtures live in a separate dev-only crate

`omniphony-renderer/dsp_fixtures/`, consumed as a path dev-dependency of
`renderer`. The resulting dev-dependency cycle (`renderer` dev-depends on
`dsp_fixtures`, which depends on `renderer`) is supported by Cargo. A minimal
two-crate reproduction of exactly this shape was built and confirmed to compile
the depended-on crate **once**, not twice — so the cycle costs no extra build
time. Worth re-confirming against the real crates during implementation, since
feature unification can differ at scale.

Rejected: a `#[doc(hidden)] pub mod` inside `renderer` (leaks test-only surface
into the library), and duplicating generators between benches and tests (they
drift, and then the perf gate and the null test stop measuring the same scene).

### D4 — Minimal PR gate, wide matrix opt-in

One case per family plus three null scenes on every PR.

The wide matrix is gated on a **`wide-matrix` cargo feature**, not on
`#[ignore]` (see D2). Wide cases are `#[cfg(feature = "wide-matrix")]`, so they
are compiled only when the feature is on.

The feature is declared on **`renderer`**, not on `dsp_fixtures`: the wide cases
are test code inside `renderer`, so a feature on the fixture crate could not
gate them.

```sh
cargo test --workspace                                   # PR gate
cargo test --workspace --features renderer/wide-matrix   # full matrix
cargo test --workspace -- --ignored                       # known-failing only
```

Each command then has a meaningful exit code. macOS stays build-only.

## Architecture

```
omniphony-renderer/dsp_fixtures/
  Cargo.toml            deps: renderer (path), realfft
  src/
    lib.rs              re-exports
    scene.rs            deterministic scene generation
    golden.rs           golden read/write, path resolution, RawF32 layout
    residual.rs         peak/RMS residual in dBFS
    analysis.rs         magnitude response (realfft); cross-correlation ITD
    dirs.rs             Fibonacci sphere lattice; meridian sweeps
  goldens/
    speaker_714_32obj.f32       ~576 KB
    binaural_kemar.f32           ~96 KB
    crossover_5_1_2.f32           ~384 KB
```

`dsp_fixtures` is a workspace member, so `cargo test --workspace` runs its own
unit tests. It uses only `renderer`'s public API. `realfft 3.5` is already a
workspace dependency (`orender_engine`), so it adds no meaningful compile time.

### Where tests live

**All tests stay in-lib** (`#[cfg(test)]` within `renderer`), not in
`renderer/tests/`. Two reasons: no new test-binary link step, and
`NativeVbapLayout` is `pub(crate)`
(`renderer/src/spatial_vbap/panner/native_backend.rs:24`), so the VBAP sweep
cannot reach it from an external test target without widening public API.
In-lib tests see dev-dependencies, so they can still use `dsp_fixtures`.

### Bench migration

`renderer/benches/render_frame.rs` currently holds the scene generators
(`make_pcm`, `move_events`, seeded `pseudo`, preset and layout builders). These
move to `dsp_fixtures::scene`, and the bench imports them. This is what makes
#9's perf gate and the null test share one definition of "the canonical scene".

Bench numbers must be **re-baselined after the move** — the migration should not
change them, but #9 will depend on those baselines, so they get re-measured
rather than assumed.

## Component: the null test

Each scene pins sample rate, block size, layout, preset, ramp mode, object
count, and event schedule. Input PCM comes from the seeded `pseudo()` PRNG.
Scenes are **0.25 s at 48 kHz** — `GAIN_SLEW_SECS` is 0.02, so this covers the
20 ms fade-in plus ~230 ms of steady motion.

Three scenes:

| Scene | Layout | Content |
| --- | --- | --- |
| `speaker_714_32obj` | 7.1.4 | 32 moving objects, interpolating ramp mode |
| `binaural_kemar` | — | same trajectory through the binaural path, bundled KEMAR |
| `crossover_5_1_2` | 5.1.2 | crossover active |

Comparison asserts, in order:

1. **Shape** — length and channel count match exactly. Never compare to the
   shorter of the two.
2. **Non-degeneracy** — the render's peak is above −60 dBFS. A golden of zeros
   must not pass trivially.
3. **Finiteness** — no NaN or Inf on either side.
4. **Residual** — peak residual below −120 dBFS. RMS residual is reported
   alongside but is not itself a gate.

### Blessing goldens

`OMNIPHONY_BLESS_GOLDENS=1 cargo test -p renderer` rewrites the goldens and
**prints the residual it replaced**.

The files are binary, so a PR diff carries no information. That printed residual
is the review artifact: a PR that blesses a golden must quote it, so
"regenerated the goldens" cannot silently void the harness.

## Component: acceptance tests

### (a) LR4 crossover reconstruction flatness

Instantiate `LR4CrossoverBank`, feed an impulse, sum all band outputs, take the
magnitude response with `realfft`.

**Assert:** `|H_sum(f)|` within **±0.25 dB of 0 dB** over
`[4·fc_min, min(20 kHz, 0.45·fs)]`. The upper bound tracks the sample rate so
the 44.1 kHz case does not assert flatness into the anti-alias region near
Nyquist.

`renderer/src/crossover/filter.rs` documents that each split sums to a
2nd-order allpass, and that N bands carry per-band allpass compensation so the
total is a cascade of N−1 allpasses. Magnitude is therefore exactly 1 in theory,
and any deviation is coefficient or float error.

Two details that decide whether this test is meaningful:

- The impulse response must be **32768 samples** at 48 kHz. LR4 ringing at low
  `fc` takes tens of milliseconds; truncating it produces spectral leakage that
  reads as passband ripple.
- The asserted band is bounded below at `4·fc_min` for the same reason.

**Phase is deliberately not asserted.** Allpass summing rotates phase by design.
This is recorded so the behaviour is not later "fixed".

PR gate: the shipped default band configuration.
Wide matrix: 2/3/4 bands × 44.1 / 48 / 96 kHz.

### (b) VBAP energy conservation and seam continuity

In-lib, on the shipped **7.1.4** layout rather than the synthetic
`horizontal_7_layout()` the current tests use. Speakers with
`spatialize: false` (LFE) are excluded from the energy sum.

**Energy:** `Σg² = 1` within **±0.25 dB** over a ~512-point Fibonacci sphere
lattice. This is tighter than the existing `test_energy_conservation_at_pole`
(`|rms − 1| < 0.05`, ±0.42 dB) and covers the whole sphere instead of the
azimuth-0 meridian.

Directions below the speaker hull are **included**. The dummy-redistribution
path claims to conserve energy there; if it does not, that is a finding under
D2.

**Seams — threshold-free:** measure `‖g(θ+Δ) − g(θ)‖₂` at Δ = 1° and again at
Δ = 0.5°, and assert the ratio is **< 0.65**.

VBAP is continuous by construction — gains fall to zero at a triplet edge as the
adjacent triplet takes over — so halving the step must roughly halve the
difference. A jump discontinuity does not halve. This detects triplet-boundary
seams, which energy conservation cannot see (the image can jump while energy
stays perfectly conserved), and it needs no invented Lipschitz constant.

PR gate: 7.1.4, 512-point lattice.
Wide matrix: all shipped layouts, 8192-point lattice, all spread values.

### (c) End-to-end binaural ITD

Render one object at a known direction through the binaural path with bundled
KEMAR, then estimate the interaural lag by parabolic-interpolated
cross-correlation of L against R.

This is deliberately **not** a comparison of `ear_delays_seconds()` against
Woodworth's formula — `renderer/src/binaural/itd.rs` *implements* Woodworth, so
such a test would be circular. Measuring the rendered output instead exercises
the delay lines, convolver, interpolation, and head-pose rotation as a chain.

Three assertions, because per-ear HRIR group delay biases any raw comparison to
the model:

1. **Antisymmetry** — `ITD(+az) = −ITD(−az)` within ±1 sample, and
   `ITD(0°) ≈ 0`. Structural; immune to group-delay bias.
2. **Monotonicity** — magnitude increases from 0° to 90°.
3. **Magnitude** — within **±3 samples (±62 µs at 48 kHz)** of
   `ear_delays_seconds()`. The tolerance absorbs HRIR group delay.

The bundled SAF KEMAR set is ISC-licensed
(`renderer/src/binaural/data/saf_kemar.LICENSE`), so this test is hermetic and
never touches sofacoustics.org.

PR gate: 8 horizontal directions.
Wide matrix: full azimuth × elevation grid, multiple head radii.

## Testing the harness itself

A validation harness that is silently broken passes everything, so
`dsp_fixtures` carries its own unit tests, run by `cargo test --workspace`:

- `residual.rs` — a signal against itself returns `f32::NEG_INFINITY` (not NaN,
  and not a large finite number); against a copy offset by a known constant, the
  analytically expected dBFS value.
- `analysis.rs` — the ITD estimator recovers a known integer and known
  fractional delay from a synthetic delayed pair; the magnitude-response helper
  returns flat 0 dB for a unit impulse.
- `dirs.rs` — the Fibonacci lattice returns the requested count, all unit-norm,
  with no duplicate directions.
- `scene.rs` — scene generation is reproducible: same seed, identical PCM and
  event stream.

## CI integration

No new job. The tests are in-lib, so the existing `cargo test --workspace`
(`.github/workflows/ci.yml:106`) picks them up.

**Budget:** under 2 s added. Measured baseline for the full workspace suite is
7.42 s, so the suite stays under 10 s.

The wide matrix runs via
`cargo test --workspace --features dsp_fixtures/wide-matrix`. Its natural later
home is `release.yml` as a pre-release gate, which costs PRs nothing — out of
scope here.

## Failure ergonomics

An assertion that prints only "failed" is not actionable, so each family reports
enough to act on:

- **Null** — scene name, peak and RMS residual in dBFS, channel and sample index
  of the worst deviation, first 8 diverging sample pairs, and the failing render
  dumped beside the golden as `*.actual.f32` for inspection in ffplay or
  Audacity.
- **LR4** — worst frequency and its deviation in dB.
- **VBAP** — offending azimuth and elevation with the measured value; for seams,
  both difference norms and their ratio.
- **ITD** — direction, measured lag in samples and µs, the model value, and
  which of the three assertions failed.

## Risks

- **Day-one failures.** Expected by D2, and handled by the two-phase order in
  D2a: the measurement report quantifies the backlog before any threshold
  becomes a gate, so the scope of follow-up work is a decision rather than a
  surprise.
- **Golden churn from legitimate changes.** Mitigated by the blessing workflow
  printing the residual it replaced. Requires review discipline, not just tooling.
- **Repo weight.** ~1.05 MB of goldens. Acceptable next to the existing 2.8 MB
  demo WAV and 862 KB KEMAR set.
- **Bench migration changing bench numbers.** Should be a pure move; re-baselined
  explicitly rather than assumed, since #9 depends on those numbers.

## Follow-on work (out of scope)

This harness unblocks, in order: **#7** render-thread RT safety (the null test is
its safety net), **#9** worst-case block-time perf gate (reuses
`dsp_fixtures::scene`), **#4** BS.1770-4 / EBU R128 loudness metering (validated
by this harness), **#10** motion-to-sound latency measurement.
