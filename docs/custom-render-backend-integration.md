# Integrating a Custom Render Backend

## Purpose

This document explains how to add your own gain model / render backend to
`omniphony-renderer`. A backend is a **gain model**: it maps an object position
(plus live render parameters) to a per-speaker gain vector, and is then prepared
and executed by the render pipeline.

The goal of the current design is that adding a backend costs **one new file and
one registration line** — no edits to any central enum, `match`, serde bridge,
or Studio JavaScript. A buggy contributor backend is rejected at build time and
can never crash the audio thread.

> **Starting point:** copy the `example_backend` crate
> (`omniphony-renderer/example_backend/`). It is a minimal, heavily commented
> backend that depends on `renderer` through its **public API only**, and it is
> built and tested as a workspace member in CI — so it always stays in sync with
> the public surface a backend needs. Everything below is implemented there; read
> it alongside this guide.
>
> The hosts only register it when built with the `example-backend` feature
> (`cargo build --features example-backend` for `orender`, the same feature on
> `orender_ffi` for `liborender`), so release builds don't offer it in Studio.
> Build with the feature to select `backend_id = "example"` and try it end to end.

## The two traits

A backend is two small pieces, both implementable from your own crate:

1. [`GainModel`](../omniphony-renderer/renderer/src/render_backend.rs) — the
   model itself: identity, capabilities, and the hot-path gain computation.
2. [`BackendFactory`](../omniphony-renderer/renderer/src/backend_registry.rs) —
   how the runtime builds your model: a stable id, a label, a declarative
   parameter schema, and a `build_plan` that captures what it needs from the
   build context and returns a builder closure.

There is **no** `BackendDescriptor`, `RenderBackendKind`, or `GainModelKind` to
extend any more. Identity is a plain string id carried on the model and the
factory.

## Step 1 — Implement `GainModel`

```rust
pub trait GainModel: Send + Sync + 'static {
    fn backend_id(&self) -> &'static str;
    fn backend_label(&self) -> &'static str;
    fn capabilities(&self) -> BackendCapabilities;
    fn speaker_count(&self) -> usize;
    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse;
    fn save_to_file(&self, path: &Path, speaker_layout: &SpeakerLayout) -> Result<()>;
}
```

`backend_id` is the stable selection key (e.g. `"my_model"`); `backend_label` is
what the UI shows. The audio hot path runs `compute_gains` through a
`PreparedRenderEngine` wrapping your model — you never wire that up yourself.

### The hot-path contract (read before writing `compute_gains`)

`compute_gains` runs in the realtime audio thread, once per object per band per
frame. It **MUST**:

- **not panic** — return a best-effort gain vector instead (e.g. zeroed);
- **not allocate** on the heap, lock, or block;
- return exactly `speaker_count()` finite gains.

Do any expensive setup (triangulation, lookup tables, caches) when the model is
*built*, not here. As a safety net the engine smoke-tests every freshly built
backend on a few reference positions on the build thread: a model that panics or
returns a malformed gain vector is rejected at topology build time (surfaced to
Studio as a recompute error) instead of crashing the audio thread. That guard
only covers the build-time probe, so honouring the contract above is still
required for correct realtime behaviour.

Use the stack-backed `Gains` buffer for output (`Gains::zeroed(n)` does not
allocate). See `example_backend`'s `compute_gains` for a complete, allocation-free
example.

## Step 2 — Declare capabilities

In `capabilities()`, declare only what your model actually supports:

```rust
BackendCapabilities {
    supports_realtime: true,
    supports_precomputed_polar: false,
    supports_precomputed_cartesian: false,
    supports_position_interpolation: false,
    supports_distance_model: false,
    supports_spread: false,
    supports_spread_from_distance: false,
    supports_event_size: false,
    supports_distance_diffuse: false,
    supports_table_export: false,
}
```

These flags drive the available evaluation modes, which Studio sections are
visible, and table-export support. The UI and runtime trust them, so do **not**
over-declare a capability "for later". Studio reasons with capabilities
(`supports_spread`, `supports_distance_model`, …), never with `if backend == vbap`.

If `supports_table_export` is `false`, return an explicit error from
`save_to_file` rather than silently succeeding.

## Step 3 — Implement `BackendFactory`

```rust
pub trait BackendFactory: Send + Sync {
    fn id(&self) -> &'static str;
    fn label(&self) -> &'static str { self.id() }            // defaults to id
    fn param_schema(&self) -> Vec<ParamSpec> { Vec::new() }  // defaults to none
    fn build_plan(&self, ctx: &BackendBuildCtx<'_>) -> Option<BackendBuildPlan>;
}
```

`build_plan` is where you turn the live build context into a model. Capture
everything you need from `ctx` (it is only borrowed) into a closure, and return a
`BackendBuildPlan::Dynamic` — the open variant for backends that are not one of
the shipped built-ins:

```rust
fn build_plan(&self, ctx: &BackendBuildCtx<'_>) -> Option<BackendBuildPlan> {
    // Read geometry from the layout on the build thread.
    let (azimuth_elevation, _) = ctx.layout.spatializable_positions();
    let speaker_positions: Vec<[f32; 3]> = azimuth_elevation
        .iter()
        .map(|[az, el]| { let (x, y, z) = spherical_to_adm(*az, *el, 1.0); [x, y, z] })
        .collect();

    // Resolve any host-set parameters (Step 4), with a default fallback.
    let sharpness = ctx
        .backend_param(self.id(), "sharpness")
        .and_then(ParamValue::as_f32)
        .unwrap_or(DEFAULT_SHARPNESS);

    Some(BackendBuildPlan::Dynamic(DynamicBackendPlan::new(
        "my_model",
        move || Ok(Box::new(MyModelBackend::new(&speaker_positions, sharpness))),
    )))
}
```

Return `None` if the backend cannot be prepared for the given context. The
closure runs on the build thread, never on the audio thread, so it may allocate
and do heavy setup.

`BackendBuildCtx` exposes `layout`, `live` (the `LiveParams`),
`backend_rebuild_params`, and the full `backend_params` store; read your own
parameter values through `ctx.backend_param(self.id(), key)`.

## Step 4 — Declare tunable parameters (optional)

Parameters are **declared as data** in `param_schema()`; the host stores values
generically and Studio renders the matching control (slider / checkbox / select)
automatically. There is no typed field to add anywhere in the renderer and no
Studio code to touch.

```rust
fn param_schema(&self) -> Vec<ParamSpec> {
    vec![
        ParamSpec::float("sharpness", "Sharpness", 0.5, 8.0, 0.1, DEFAULT_SHARPNESS),
        // ParamSpec::int(key, label, min, max, default)
        // ParamSpec::bool(key, label, default)
    ]
}
```

Read the values at build time via `BackendBuildCtx::backend_param` (Step 3).
Values set over OSC (`/omniphony/control/backend/param`) or loaded from config
are replayed into the store and trigger a rebuild, so your backend picks them up
on the next build.

## Step 5 — Register it (the one line)

A host registers your factory at startup through `RendererControl`:

```rust
control.register_backend(Box::new(my_backend::MyFactory));
```

The built-in host does this in
[`renderer_build.rs`](../omniphony-renderer/orender_engine/src/renderer_build.rs)
(see the `example_backend::ExampleFactory` registration, behind the
`example-backend` feature). After that line,
selecting `backend_id = "my_model"` — from config (`render_backend = "my_model"`),
over OSC, or from the Studio dropdown — routes a topology rebuild through your
factory. A later registration with the same id replaces an earlier one, so a host
can override a built-in.

## Step 6 — It appears in Studio automatically

The runtime snapshot
([`snapshot.rs`](../omniphony-renderer/runtime_control/src/snapshot.rs)) publishes
the registry's `available_backends` (id, label, **and parameter schema**) plus the
current param values. Studio populates its backend dropdown and generates the
parameter controls from that snapshot — no per-backend JavaScript, no manual
serde bridge. If your capabilities are correct, the surrounding UI sections adapt
on their own.

There is nothing backend-specific to add in `vbap.js`, `app_state.rs`, or the
OSC/state plumbing for a backend that uses the generic parameter schema.

## Step 7 — Validate

1. `cargo fmt`
2. `cargo build --workspace` and `cargo test --workspace` in `omniphony-renderer`
   (CI runs the same, including the build-time smoke test that rejects a
   misbehaving backend).
3. Select your backend in Studio and confirm: label, generated parameter
   controls, available evaluation modes, and section visibility.
4. Change the layout and confirm a clean rebuild.

## Quick checklist

- [ ] implement `GainModel` (id, label, capabilities, `compute_gains`, …)
- [ ] honour the hot-path contract in `compute_gains`
- [ ] declare honest `BackendCapabilities`
- [ ] implement `BackendFactory` returning a `BackendBuildPlan::Dynamic`
- [ ] declare any tunables in `param_schema()` and read them via `backend_param`
- [ ] `register_backend(...)` your factory in the host (one line)
- [ ] `cargo fmt` + `cargo build/test --workspace`
- [ ] verify selection, parameters, and rebuild in Studio

## Design advice

- **Purely realtime model:** set only `supports_realtime = true`; leave the
  `supports_precomputed_*` flags `false` and keep `build_plan` simple.
- **Needs caches/tables:** build them in `build_plan`'s closure (build thread),
  never in `compute_gains`.
- **Cannot export a table:** keep `supports_table_export = false` and return an
  explicit error from `save_to_file`.
- **No spread / distance model:** set those flags `false` so the UI does not
  expose controls that have no meaning for your model.

## Built-in backends and typed plans

The shipped backends (VBAP, Barycenter, Distance, Hybrid) use *typed*
`BackendBuildPlan` variants (`Vbap`, `Barycenter`, …) rather than `Dynamic`,
because they share geometry/evaluation machinery and the composite Hybrid backend
builds inner backends by id. Contributor backends do **not** need a typed
variant: `Dynamic` carries an arbitrary builder closure and is a first-class
plan, prepared and reused exactly like the typed ones. Reach for a typed variant
only if you are extending a built-in inside the `renderer` crate itself.
