# From-File Evaluator Architecture

This document captures the intended architecture for `from_file` evaluation in `omniphony-renderer`.

It is based on the following product goal:

- a serialized evaluation artifact must be independent from the backend implementation
- the artifact must contain the precomputed evaluation tables, not a backend-specific model
- the artifact must embed the room-ratio transform context and the speaker layout
- loading the artifact must create an evaluator directly, without constructing a backend
- when `from_file` is active, room-ratio and speaker editing must be disabled
- later, the file may also embed a serialized backend snapshot so Studio can offer a `restore backend` action
- long-term, this must support embedded targets where no backend implementation is compiled at all

## Core Distinction

The important distinction is:

- `backend`: computes gains from a render request
- `prepared evaluator`: answers render requests efficiently, potentially by table lookup

The `from_file` feature is explicitly about the second category.

It is not "load a VBAP backend from disk".
It is "load a frozen evaluator artifact from disk".

So the file format belongs to the evaluation layer, not to `VbapPanner`, not to `VbapBackend`, and not to any backend-specific module.

## What Must Be Serialized

The file must represent a fully prepared evaluation state.

At minimum it should contain:

- evaluator storage format/version
- evaluation domain kind
  - `precomputed_cartesian`
  - `precomputed_polar`
- table axes
  - Cartesian: `x`, `y`, `z`
  - Polar: `azimuth`, `elevation`, `distance`
- interpolation mode assumptions
- fully baked gain tables
- frozen speaker layout
- frozen room transform parameters
  - `room_ratio`
  - `room_ratio_rear`
  - `room_ratio_lower`
  - `room_ratio_center_blend`
- metadata describing whether negative Z is allowed

The artifact must be sufficient to answer `compute_gains()` without access to any backend implementation.

## What Must Not Remain Runtime-Parametric

When a file-backed evaluator is loaded, some parameters must stop being editable because they are already baked into the artifact.

That includes:

- speaker layout / speaker positions
- room ratios
- room ratio rear
- room ratio lower
- room ratio center blend

Those parameters must be treated as frozen runtime state while `from_file` is active.

The runtime and Studio should not present them as editable values in that mode.

## Room Ratios and Evaluation Space

The intended model is:

- `PrecomputedCartesian` is sampled in native ADM Cartesian space
- room-ratio deformation is part of the baked evaluation function
- the file stores the room-ratio configuration used to build the table

This is deliberate.

It means file-backed Cartesian evaluation keeps the same conceptual advantage as current `PrecomputedCartesian`:

- lookup happens in native input coordinates
- there is no hidden "effect-space" remapping at lookup time

The room-ratio state exists in the file to document and freeze the function that was sampled, not to request a second transformation layer on top of lookup.

## Rendering Semantics

When `from_file` is active:

- rendering must use the loaded evaluator directly
- backend construction must not happen
- backend gain computation must not happen
- room-ratio editing must be disabled
- speaker editing must be disabled

`compute_gains()` should behave as a lookup into the frozen evaluator domain.

That means the evaluator consumes native request coordinates and returns gains according to the baked table.

The evaluator may still need some request-time fields if they are not baked yet, but the intended end state is to minimize those dependencies.

The priority is:

1. no backend dependency
2. frozen room/speaker configuration
3. deterministic lookup from the serialized evaluator

## Relationship With Backends

Backends are still responsible for producing `PreparedEvaluator`s in normal operation.

But the file artifact must not be described as "saved backend state".

Instead, the flow is:

1. user selects a backend
2. runtime builds a prepared evaluator
3. if the effective mode is `precomputed_cartesian` or `precomputed_polar`, that evaluator can be serialized as a frozen artifact
4. later, the artifact can be loaded directly as a `from_file` evaluator

So the file format is tied to prepared evaluation modes, not to `VbapBackend` or any future backend.

## Save Semantics

The artifact should be exportable from:

- `PrecomputedCartesian`
- `PrecomputedPolar`

It should not depend on whether the source backend was:

- VBAP
- experimental distance
- another future model

If two different backends produce the same prepared evaluator structure, the saved file should still only describe the evaluator artifact.

## Restore Backend

Later, the file can be extended to also embed a serialized backend snapshot.

That backend snapshot is optional metadata, not the primary meaning of the file.

The intended Studio behavior is:

- `from_file` active:
  - room ratio controls disabled
  - speaker editing disabled
  - backend controls disabled or read-only
- `restore backend`:
  - deserialize the stored backend snapshot
  - rebuild a normal backend-driven topology
  - re-enable room-ratio and speaker editing

This gives two separate capabilities:

- deployment/runtime use without compiled backend code
- authoring recovery when backend state is available

## Why This Matters For Embedded

The long-term target is to support systems where only:

- the evaluator runtime
- the artifact loader
- the mixer

are shipped.

In that environment:

- backend-specific code may be absent
- backend triangulation or geometry code may be absent
- the device only needs fast lookup and gain application

That is why `from_file` must not depend on backend construction.

## Consequences For Current Code

The final architecture implies the following code organization:

- backend code:
  - builds `PreparedEvaluator`s
  - does not own file loading
- evaluation code:
  - defines the serialized evaluator artifact
  - loads `from_file`
  - exposes save/load for prepared evaluation data
- runtime control:
  - knows when current mode is frozen
  - disables mutable topology controls accordingly
- Studio:
  - reflects frozen state in UI
  - offers `restore backend` if backend metadata exists in the file

## Mismatch With The Current Partial Refactor

The current partial implementation on the branch is only a step in this direction.

It still diverges from the intended target in these ways:

- the loader is still VBAP-file specific
- the loaded artifact still reflects the historical VBAP table format
- the loaded evaluator still uses runtime room-ratio inputs instead of treating them as fully frozen file state
- the renderer constructor still receives an external speaker layout instead of always deriving the frozen layout from the artifact

So that implementation should be treated as transitional, not as the target architecture.

## Implementation Target

The implementation we should build next is:

1. define a generic serialized evaluator artifact type
2. support export from `PrecomputedCartesian` and `PrecomputedPolar`
3. store baked room-ratio state and speaker layout in the artifact
4. load the artifact into a backend-free `from_file` evaluator
5. expose frozen-state flags to runtime and Studio
6. disable room-ratio and speaker editing when `from_file` is active
7. later, add optional backend serialization plus `restore backend`

## Short Design Rule

The design rule is simple:

- `from_file` is a frozen prepared evaluator
- it is not a serialized backend
- it must remain usable when no backend implementation is compiled
