# Integrating a Custom Render Backend

## Purpose

This document explains how to add your own gain model / render backend to `omniphony-renderer` after the recent backend refactor.

The goal is to help a contributor:

- understand where to plug in
- implement a custom gain computation
- declare backend capabilities
- expose the backend to the runtime and to Studio

In current Omniphony terminology, a "backend" means:

- a concrete **gain model**
- prepared and executed through the render pipeline

## Overview

The relevant architecture is now split into four layers:

1. `GainModel`
2. `PreparedRenderEngine`
3. `TopologyBuildPlan`
4. UI/runtime driven by `backend_id` and backend capabilities

The main entry points are:

- [`render_backend.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/renderer/src/render_backend.rs)
- [`backend_registry.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/renderer/src/backend_registry.rs)
- [`live_params.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/renderer/src/live_params.rs)
- [`snapshot.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/runtime_control/src/snapshot.rs)
- [`vbap.js`](/home/user/dev/spatial-renderer/Omniphony/omniphony-studio/src/controls/vbap.js)

## Current Architecture

### 1. Backend identity

Backend product identity is centralized in:

- [`render_backend.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/renderer/src/render_backend.rs)

The static registry contains:

- `BackendDescriptor`
- `backend_descriptors()`
- `backend_descriptor()`
- `backend_descriptor_by_id()`

Each backend has:

- a stable `backend_id`, for example `vbap`
- a user-facing label, for example `VBAP`
- a `RenderBackendKind`
- a `GainModelKind`

### 2. Model contract

A concrete backend implements the `GainModel` trait in:

- [`render_backend.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/renderer/src/render_backend.rs)

The current contract is:

```rust
pub trait GainModel: Send + Sync + 'static {
    fn kind(&self) -> GainModelKind;
    fn backend_id(&self) -> &'static str;
    fn backend_label(&self) -> &'static str;
    fn capabilities(&self) -> BackendCapabilities;
    fn speaker_count(&self) -> usize;
    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse;
    fn save_to_file(
        &self,
        path: &std::path::Path,
        speaker_layout: &SpeakerLayout,
    ) -> Result<()>;
}
```

The audio hot path consumes a `PreparedRenderEngine`, not your concrete type directly.

### 3. Backend build / rebuild

Topology preparation is centralized in:

- [`backend_registry.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/renderer/src/backend_registry.rs)

This module contains:

- `BackendBuildPlan`
- one concrete build plan per backend
- `TopologyBuildPlan`
- `prepare_topology_build_plan(...)`

The runtime no longer selects its backend through a large backend-specific `match` inside `live_params.rs`.
`live_params.rs` now gathers live inputs and delegates plan construction to the registry.

### 4. Backend capabilities

Capabilities are exposed through `BackendCapabilities` in:

- [`render_backend.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/renderer/src/render_backend.rs)

They drive:

- the runtime snapshot
- OSC state publishing
- Studio visibility and controls

Studio should no longer reason with:

- `if backend == vbap`

but with capabilities such as:

- `supports_spread`
- `supports_distance_model`
- `supports_precomputed_cartesian`
- `supports_precomputed_polar`

## Integration Procedure

## Step 1: Declare backend identity

Add a descriptor to the registry in:

- [`render_backend.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/renderer/src/render_backend.rs)

Conceptual example:

```rust
BackendDescriptor {
    kind: RenderBackendKind::MyModel,
    gain_model_kind: GainModelKind::MyModel,
    id: "my_model",
    label: "My Model",
}
```

At the moment this still requires adding enum variants to:

- `RenderBackendKind`
- `GainModelKind`

But the identity cost is now localized to that area.

## Step 2: Implement the gain model

Add your concrete struct in:

- [`render_backend.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/renderer/src/render_backend.rs)

or move it into a dedicated module if it grows large.

Minimal example:

```rust
pub struct MyModelBackend {
    speaker_positions: Vec<[f32; 3]>,
}

impl GainModel for MyModelBackend {
    fn kind(&self) -> GainModelKind { ... }
    fn backend_id(&self) -> &'static str { "my_model" }
    fn backend_label(&self) -> &'static str { "My Model" }
    fn capabilities(&self) -> BackendCapabilities { ... }
    fn speaker_count(&self) -> usize { ... }
    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse { ... }
    fn save_to_file(&self, path: &Path, speaker_layout: &SpeakerLayout) -> Result<()> { ... }
}
```

### Runtime recommendations

- do not allocate in `compute_gains()`
- do not use `HashMap` in the hot path
- build lookup tables or caches during topology preparation if needed
- keep the output in `Gains` form

## Step 3: Declare capabilities

In `capabilities()`, declare only what is actually supported.

Example:

```rust
BackendCapabilities {
    supports_realtime: true,
    supports_precomputed_polar: false,
    supports_precomputed_cartesian: true,
    supports_position_interpolation: true,
    supports_distance_model: false,
    supports_spread: false,
    supports_spread_from_distance: false,
    supports_distance_diffuse: false,
    supports_heatmap_cartesian: true,
    supports_table_export: false,
}
```

These flags directly affect:

- available evaluation modes
- which Studio sections are visible
- debug heatmap support
- table export support

Do not over-declare a capability "for later". The UI and runtime trust these flags.

## Step 4: Add a backend build plan

Add a concrete build plan in:

- [`backend_registry.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/renderer/src/backend_registry.rs)

Example:

```rust
#[derive(Clone)]
pub struct MyModelBuildPlan {
    pub speaker_positions: Vec<[f32; 3]>,
    pub custom_param: f32,
}

impl MyModelBuildPlan {
    pub fn build_gain_model(&self) -> Result<Box<dyn GainModel>> {
        Ok(Box::new(MyModelBackend::new(...)))
    }
}
```

Then wire that plan into:

- `BackendBuildPlan`
- `TopologyBuildPlan::build_topology()`
- `TopologyBuildPlan::log_summary()`

## Step 5: Hook plan preparation into the registry

The final integration point is:

- `prepare_topology_build_plan(...)`
  in [`backend_registry.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/renderer/src/backend_registry.rs)

This function receives:

- the `layout`
- the `LiveParams`
- the `BackendRebuildParams`
- the evaluation config built by the runtime

It must:

1. recognize `backend_id`
2. build the matching `BackendBuildPlan`
3. choose the effective `evaluation_mode`
4. return a `TopologyBuildPlan`

Conceptual example:

```rust
match live.backend_id() {
    "my_model" => Some(TopologyBuildPlan {
        layout,
        backend_id: "my_model".to_string(),
        backend_build: BackendBuildPlan::MyModel(MyModelBuildPlan { ... }),
        evaluation_mode: ...,
        evaluation_build_config,
    }),
    ...
}
```

## Step 6: Define rebuild parameters

If your backend needs persistent backend-specific data to rebuild topology after a live update, extend:

- [`live_params.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/renderer/src/live_params.rs)

`BackendRebuildParams` still contains a `vbap` block today.

If your backend also needs rebuild-specific state:

- add an equivalent backend-specific block
- keep `backend_id` as the selection key
- avoid duplicating backend selection logic elsewhere

## Step 7: Expose the backend to OSC and config

Backend parsing still goes through:

- [`render_backend.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/renderer/src/render_backend.rs)
  via `RenderBackendKind::from_str()`

and is used notably in:

- [`runtime_control/src/osc.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/runtime_control/src/osc.rs)
- [`src/cli/decode/bootstrap.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/src/cli/decode/bootstrap.rs)

So you must:

- add your backend to the identity registry
- make sure `from_str()` accepts it

If you want it to be selectable from config, also verify:

- [`runtime_control/src/persist.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/runtime_control/src/persist.rs)

## Step 8: Expose capabilities to Studio

The runtime snapshot publishes backend state in:

- [`snapshot.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/runtime_control/src/snapshot.rs)

Studio consumes it in:

- [`tauri-bridge.js`](/home/user/dev/spatial-renderer/Omniphony/omniphony-studio/src/tauri-bridge.js)
- [`vbap.js`](/home/user/dev/spatial-renderer/Omniphony/omniphony-studio/src/controls/vbap.js)

In theory, if your capabilities are correct, most UI sections will already adapt correctly.

In practice, verify at least:

- backend label rendering
- available evaluation modes
- section visibility
- heatmap behavior if supported

## Step 9: Validate

Minimum recommended validation:

1. `cargo fmt`
2. `cargo check` in `omniphony-renderer`
3. `cargo check` in `omniphony-studio/src-tauri`
4. manual backend selection test
5. manual rebuild test after layout changes

## Quick checklist

- add a backend descriptor
- add or extend `RenderBackendKind`
- add or extend `GainModelKind`
- implement `GainModel`
- declare `BackendCapabilities`
- add a backend build plan
- wire `prepare_topology_build_plan(...)`
- verify `from_str()` / config / OSC
- verify Studio behavior
- verify `cargo check`

## Design advice

### If your model is purely realtime

- support only `supports_realtime`
- leave `supports_precomputed_* = false`
- keep the build plan simple

### If your model needs caches

- build them during topology preparation
- not inside `compute_gains()`

### If your model cannot export a table

- keep `supports_table_export = false`
- return an explicit error in `save_to_file()`

### If your model does not support `spread` or `distance_model`

- set the relevant flags to `false`
- do not let the UI expose controls that have no meaning

## Current limitations

The architecture is now much more contribution-friendly than before, but it is not fully dynamic yet.

Some identity points are still enum-based:

- `RenderBackendKind`
- `GainModelKind`

So adding a backend is not yet a pure external drop-in module.

However, the cost is now localized:

- identity in `render_backend.rs`
- build logic in `backend_registry.rs`
- model implementation in your backend code

The audio core, live runtime, and Studio no longer need to be rethought for every new model.
