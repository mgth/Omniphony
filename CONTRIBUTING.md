# Contributing to Omniphony

Thanks for your interest in Omniphony! This guide covers how to build, test, and
contribute to the suite — with a focus on the most common contribution:
**adding your own spatial render backend**.

Omniphony has a renderer and a Studio frontend:

- **`omniphony-renderer/`** — the real-time decoding, spatial rendering, and OSC
  control engine (a Cargo workspace of several crates).
- **`omniphony-studio-egui/`** — Omniphony Studio, the supervision /
  3D-visualization / live-control desktop app (native egui/wgpu), with
  separate core, scene and UI crates. Start with its
  [contributor guide](omniphony-studio-egui/CONTRIBUTING.md) for a first panel
  change or a toolkit upgrade.

Most of this guide is about the renderer, since that is where rendering backends
live and where the realtime contract matters.

## Repository layout

```
omniphony-renderer/          Cargo workspace (the engine)
  renderer/                  VBAP engine, layouts, backend traits & registry,
                             the backend-conformance harness, runtime config
  audio_output/              PipeWire / ASIO output + adaptive-resampling servo
  audio_input/               live PipeWire capture
  orender_engine/            engine glue: bridge loading, decode loop, OSC
  runtime_control/           shared control/state types and OSC plumbing
  bridge_api/                versioned ABI for external format bridges
  spdif/                     IEC61937 / S/PDIF parsing
  example_backend/           reference backend — copy this to start your own
omniphony-studio-egui/       Omniphony Studio (its own Cargo workspace)
docs/                        design notes and deep-dive guides
```

## Building & testing

You need a recent stable Rust toolchain (see `rust-version` in
`omniphony-renderer/Cargo.toml`). All engine commands run from
`omniphony-renderer/`:

```sh
cd omniphony-renderer

cargo build                      # build the workspace
cargo test                       # run the full test suite (incl. doctests)
cargo fmt --all -- --check       # formatting must be clean
```

CI (`.github/workflows/ci.yml`) checks formatting, builds and tests the renderer
and the Studio, checks the contracts, and compiles the
platform targets. Consult the workflow for the exact current matrix; release
bundling is separate. It also gates:

- **clippy**, as a ratchet: warnings per crate and lint may only go down
  (`omniphony-renderer/clippy-baseline.txt`, which also pins the clippy
  toolchain). Run `omniphony-renderer/scripts/clippy-ratchet.sh`; when you
  remove warnings, lock it in with `UPDATE_CLIPPY_BASELINE=1` and commit the
  baseline with your change.
- **cargo-deny** (`cargo deny --workspace check` in `omniphony-renderer/`):
  advisories, licences, sources and duplicate crate versions, per `deny.toml`.
- **the MSRV**: the renderer workspace must still build on its declared
  `rust-version`.
- **pinned actions**: every third-party action is referenced by commit SHA.

Before opening a PR, make sure `cargo fmt --all -- --check`, `cargo build`, and
`cargo test` all pass locally.

## Adding a render backend

This is the headline extension point, and it is designed to be cheap: adding a
backend costs **one new file and one registration line** — no edits to any
central enum, `match`, serde bridge, or Studio JavaScript. A buggy contributor
backend is rejected at build time and can never crash the audio thread.

A backend is two small traits, both implementable from **your own crate** using
only the renderer's public API:

- **`GainModel`** — maps an object position (+ live render params) to a
  per-speaker gain vector. This is the realtime hot path.
- **`PluginFactory` + `BackendFactory`** — declares the backend's id, label,
  and a data-driven parameter schema (Studio renders the controls
  automatically), and builds a `GainModel` from a speaker layout. The first
  half is the contract every plugin shares, object generators included: see
  [`docs/plugin-contract.md`](docs/plugin-contract.md).

### Steps

1. **Copy `example_backend/`** as your starting point. It is a minimal, heavily
   commented cosine panner that depends on `renderer` through its **public API
   only**, and is built + tested in CI so it always stays in sync with the public
   surface. Read it alongside
   [`docs/custom-render-backend-integration.md`](docs/custom-render-backend-integration.md),
   the full walk-through.

2. **Implement `GainModel`, `PluginFactory` and `BackendFactory`** for your
   panner.

3. **Register it** — one line where the engine wires up its backends
   (`orender_engine/src/renderer_build.rs`):

   ```rust
   control.register_backend(Box::new(my_backend::MyFactory));
   ```

   Selecting `backend_id = "my_id"` then routes a topology rebuild through your
   factory. There is no central enum or `match` to extend.

### The hot-path contract (read before writing `compute_gains`)

`compute_gains` runs on the realtime audio thread, once per object per band per
frame. It **must not** panic, allocate on the heap, lock, or block, and **must**
return exactly `speaker_count()` finite gains. Do all expensive setup
(triangulation, tables, caches) when the model is built, never in
`compute_gains`. See the `GainModel` trait docs for the full contract.

### Prove your backend conforms

The renderer ships a public conformance harness,
`renderer::backend_conformance`, so you can verify the contract from your own
crate's tests before wiring anything in. `example_backend` uses it as a template:

```rust
use renderer::backend_conformance::{check, ConformanceOptions};

#[test]
fn my_backend_conforms() {
    let model = MyBackend::new(/* … */);
    check(&model, &ConformanceOptions::default()).assert_passed();
}
```

It checks the contract (no panic, correct count, finite, non-negative, no
runaway gains), an energy floor, and continuity. To also prove `compute_gains`
is allocation-free, install the provided `CountingAllocator` as your test
binary's `#[global_allocator]` and call `check_zero_alloc` — again, see
`example_backend` for the full pattern.

## OSC / state contract

The engine is controlled and observed over OSC: clients send `/omniphony/control/…`
messages and receive `/omniphony/state/…` updates. If you are writing an
alternative client or host integration (rather than a backend), this is the
surface you target. The full contract — every address, its direction, arguments
and semantics — is documented in
[`docs/osc-control-contract.md`](docs/osc-control-contract.md), and the address
strings have named constants in the dependency-free `osc-contract` crate
(`osc-contract/src/lib.rs`, the single source of truth; `ALL_CONTROL` /
`ALL_STATE` / `ALL_SESSION` are the exhaustive lists).

## What gets saved, and when

One rule, written down in
[`docs/persistence-policy.md`](docs/persistence-policy.md): **display and
cosmetic state is kept the moment it changes; anything that changes what is
heard, or how the engine behaves, reaches `config.yaml` only through the Save
button.** Classify a new setting or control before wiring it — the policy says
how each class is plumbed on both sides. Two tripwires hold it:
`runtime_control/tests/persistence_policy.rs` in the renderer (no new write
that bypasses Save without a stated reason) and the `save-config` rule of the
native Studio's architecture test.

## Coding conventions

- **Write everything in English** — commit messages, PR titles/descriptions,
  code comments, and docs. (The older history contains French; new work is
  English-only.)
- **Performance matters in realtime paths.** Prefer designs that minimize
  per-frame and per-sample allocations, repeated recomputation, and branchy
  special cases in hot loops. Assume constrained hardware as a long-term target.
- **Keep it formatted.** Run `cargo fmt --all` before committing.
- **Branch names**: use generic, descriptive names (e.g. `fix/spdif-parser`,
  `feat/my-backend`).

## Writing tests

A test earns its place by failing when the behaviour it names breaks. The
rules below come from a review that found tests passing without checking
anything; each names the failure it prevents.

- **A test must be able to fail.** Never `return` early when a fixture, a
  device or an environment variable is missing: the run reports `ok` for a
  test that did not happen. Use `#[ignore = "why"]`, so the reason shows in
  every run, and make the test fail with a message when run without what it
  needs. Prefer a committed fixture (the reference bridge and the bundled demo
  WAV run the engine tests everywhere).
- **Guard against vacuous comparisons.** Comparing two renders, two state
  dumps or two lists proves nothing when both are empty: assert that there is
  something to compare. A negative test checks *which* error it got (the
  reason in the message), not just `is_err()`: an input that is wrong in two
  ways fails for the wrong one. A sweep counts what it reached and asserts the
  count.
- **Check that it bites.** After writing a test for a fix, revert the fix (or
  break the code the test guards) and watch it fail. Say so in the PR.
- **Test through public seams.** Assert what a caller sees — the rendered
  output, the published state, the returned error — not private fields, exact
  call counts or a re-implementation of the code under test. Those break on a
  correct refactor and pass on a wrong one that keeps the structure.
- **Stay hermetic.** Tests run in parallel, in one process, on machines where
  a renderer and Studio may be running: bind sockets to port 0 and send only
  to sockets the test owns (never 9000 or `OMNIPHONY_OSC_PORT`); write under a
  temp directory of the test's own; change an environment variable only under
  the one lock its crate's tests share for that, and restore it even on panic;
  hold a module's test lock before touching its process-global state (the
  overlay, the OSC port registry).
- **Every external input gets negative tests**: OSC datagrams, config.yaml,
  SOFA/WAV files, the C ABI. Malformed input must be refused or bounded, never
  panic, allocate without bound, or reach the render as NaN. The registry
  sweeps (`live_options_conformance.rs`, and the engine's control sweeps
  where they exist) pick up every option registry row on their own: a new
  option declared there joins them without a test of its own. Anything
  outside the registry does not: a hand-wired address family (such as
  `hybrid/curve`) or a lifecycle command needs its own cases, valid and
  malformed, in the sweep's address list or in a test beside its handler.
- **Keep fixtures shared and fast.** Reuse the helpers that exist
  (`dsp_fixtures` for scenes, signals and analysis; `saf_kemar_shared` rather
  than parsing the embedded set again) and build an expensive fixture once per
  test, not once per case.
- **Coverage guides, it does not gate.** A covered line is not a checked one.
  To find untested code, measure locally — no global threshold, which would
  reward tests that execute without asserting:

  ```sh
  cd omniphony-renderer
  RUSTFLAGS="-C instrument-coverage" LLVM_PROFILE_FILE="$PWD/../cov/%p-%m.profraw" \
    CARGO_TARGET_DIR=../cov/target cargo test --workspace
  ```

  then merge the profiles with `llvm-profdata` and report with `llvm-cov`,
  using an LLVM matching the toolchain's (`rustc -vV`).

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

See [`omniphony-renderer/dsp_fixtures/README.md`](omniphony-renderer/dsp_fixtures/README.md)
for the full contract, the wide matrix, and how deferred thresholds are tracked.

## Pull requests

- Target `main`.
- Keep PRs focused; describe what changed and why.
- Make sure the three CI commands (fmt check, build, test) pass locally first.
- **Changed a `Cargo.toml`? Commit the regenerated `Cargo.lock` with it.** CI
  builds with `--locked`, so it fails rather than resolving a dependency the
  repository has not recorded. Each workspace has its own lock:
  `omniphony-renderer/` and `omniphony-studio-egui/`.

By contributing, you agree that your contributions are licensed under the
project's `GPL-3.0-or-later` license.
