# Contributing to Omniphony

Thanks for your interest in Omniphony! This guide covers how to build, test, and
contribute to the suite — with a focus on the most common contribution:
**adding your own spatial render backend**.

Omniphony is two main components:

- **`omniphony-renderer/`** — the real-time decoding, spatial rendering, and OSC
  control engine (a Cargo workspace of several crates).
- **`omniphony-studio/`** — the supervision / 3D-visualization / live-control
  desktop app (Tauri + web frontend).

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
  bridge_api/                stable ABI for external format bridges
  spdif/                     IEC61937 / S/PDIF parsing
  example_backend/           reference backend — copy this to start your own
omniphony-studio/            Tauri control-surface app
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

CI (`.github/workflows/ci.yml`) is the Linux integration gate on pushes/PRs to
`main` and `release`. It runs exactly the three commands above (formatting,
build, full tests including doctests) and also builds the Studio frontend.
It deliberately does **not** bundle the Tauri app or build the Windows/ASIO
target. `clippy` is **not** gated yet (there is a known backlog of warnings,
some in hot audio loops); please don't introduce new ones, but a green build
does not require clippy.

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
- **`BackendFactory`** — declares the backend's id, label, and a data-driven
  parameter schema (Studio renders the controls automatically), and builds a
  `GainModel` from a speaker layout.

### Steps

1. **Copy `example_backend/`** as your starting point. It is a minimal, heavily
   commented cosine panner that depends on `renderer` through its **public API
   only**, and is built + tested in CI so it always stays in sync with the public
   surface. Read it alongside
   [`docs/custom-render-backend-integration.md`](docs/custom-render-backend-integration.md),
   the full walk-through.

2. **Implement `GainModel` + `BackendFactory`** for your panner.

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
strings have named constants in
`omniphony-renderer/runtime_control/src/osc_contract.rs` (the single source of
truth; `ALL_CONTROL` / `ALL_STATE` are the exhaustive lists).

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

By contributing, you agree that your contributions are licensed under the
project's `GPL-3.0-or-later` license.
