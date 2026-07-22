# RFC: a declared registry for live options

Status: accepted — written after the Side/Back state-sync bug (see "The
incident" below); related fix: `fix/studio-live-options-state-sync`.

Progress:

- **Phase 0 landed**: the live-options conformance net
  (`omniphony-renderer/runtime_control/tests/live_options_conformance.rs`) and
  the knip dead-code gate in the Studio CI.
- **Phase 1 landed**: the registry itself (`renderer/src/options.rs`) with the
  fixed-channel-source family migrated; the generic `/omniphony/control/option` setter
  (legacy addresses are aliases); one shared config seed used by BOTH the CLI
  bootstrap and `Engine::from_paths`; one shared store used by the full save
  and the targeted persists; the snapshot `options` block + the
  `/state/options_schema` publication; the Tauri/JS `options` passthrough; and
  the options-schema ↔ Studio i18n contract check in CI. The plan
  signatures compare `RendererControl::options_epoch` — bumped by
  `options::apply_to_control` only when a `REPLAN`-flagged option **actually
  changes value** (a redundant re-send must not re-prime the stages) — instead
  of enumerating options field by field, so a new re-planning option cannot be
  forgotten in a signature.
- **Phase 2 landed**: the Studio `data-option` binder
  (`omniphony-studio/src/options-binder.js`). A control declares its option in
  markup (`data-option="surround_placement"` + `data-option-value` /
  `data-option-on`/`-off` / `data-option-empty` by shape); the binder wires
  both directions through the single generic `control_option` Tauri command.
  The five per-option apply functions, listener blocks and Tauri commands are
  deleted; the scalar fields left the typed `LiveOptionsState` mirror (the
  document-valued companions stay); the hard-coded `state.js` defaults are
  gone — values come from the snapshot's `options` block, pre-snapshot
  defaults from the published `/state/options_schema`, and pre-connect the
  controls keep their baked HTML default.
- **Next (phase 3)**: fold remaining `LiveParams` scalars opportunistically;
  CLI flags for the newly-seeded options
  (`docs/option-surface-parity.fr.md`).

## Adding a live option today (post-phase-2)

1. Add the typed field to `LiveParams` (+ its `RenderConfig`/`config_fields`
   descriptor).
2. Add ONE `OptionSpec` row in `renderer/src/options.rs` (+ Studio i18n keys).
3. Add the control markup with its `data-option` attribute (a switch, a
   toggle-btn pair or a select — no JS).
4. Done: OSC (generic + schema), persistence, CLI/FFI seeding, replan
   invalidation, the snapshot block, the UI wiring and the CI contract checks
   all derive from the row + the markup. The conformance net fails if a layer
   is missing. Only an option with bespoke UI side effects needs code (one
   entry in the binder's `AFTER_SET`).

## The problem

Adding ONE live-tunable option (say `surround_placement`) currently means
declaring it in up to **ten** places, each one silently optional:

| # | Layer | Where |
|---|-------|-------|
| 1 | Live storage + default | `renderer/src/live_params.rs` |
| 2 | Config persistence key + default | `renderer/src/config_fields.rs` |
| 3 | CLI flag + config resolution + bootstrap seed | `src/cli/command.rs`, `config_resolution.rs`, `bootstrap.rs` |
| 4 | FFI seed from config (CLI parity) | `orender_engine/src/engine.rs` |
| 5 | OSC control handler + targeted persist | `orender_engine/src/osc/dispatch.rs` (+ `osc_contract`) |
| 6 | State snapshot emit | `runtime_control/src/snapshot.rs` |
| 7 | Studio Tauri mirror | `src-tauri/src/osc_listener.rs` domain struct + `app_state.rs` field + apply copy |
| 8 | Studio JS | `state.js` default + snapshot ingestion + UI update fn + click handler + Tauri command |
| 9 | i18n label + help | 8 locale files |
| 10 | Plan invalidation | every `PlanSig` that depends on it |

Every row is an opportunity to forget one, and forgetting is **silent**: the
option keeps working in the layers where it exists and quietly lies in the
others. This is a recurring class of bug, not a one-off.

### The incident (2026-07-04)

The Side/Back surround placement showed "Side" active in Studio while a
long-lived renderer instance was actually rendering "Back". Three independent
gaps of the same class stacked up:

1. `app_state.rs` / `RendererDomainState` never mirrored `surroundPlacement`
   (row 7 forgotten) — the snapshot value was dropped Rust-side.
2. The only JS ingestion lived in `runtime-audio-state.js`, a module **orphaned
   since April** (commit `4ba0330` replaced its call with a partial inline
   copy). Every ingestion added to it afterwards — `channelRenderMode`,
   `surroundPlacement`, `objectGenerator*`, `phantom*`, `outputChannelMapping`,
   `virtualBed` — was dead code (row 8 forgotten, invisibly).
3. Both `PlanSig`s ignored `ctx.surround_placement`, so synthesized objects
   did not re-plan on a live toggle (row 10 forgotten).

Nothing failed loudly at any point. That is the property to design away.

## What already works here: the param-schema precedent

The object-generator and phantom params solved this at f32-slider scale:
`ObjectGenParamSpec` declares `{key, label, i18n_key, min, max, step, default,
unit}` ONCE, and everything else is generic — storage is a sparse
`HashMap<String, f32>` live param, the OSC handler takes any key, persistence
saves the whole map, the schema is published over OSC, and Studio builds
sliders/switches from it dynamically. Adding a param = one array entry + i18n.
It has survived several features (PAD, DirAC, relocalize, per-band method)
with **zero** plumbing churn.

This RFC generalizes that pattern to all live options.

## Proposal: `LIVE_OPTIONS` registry

### 1. One declaration

```rust
// renderer/src/options.rs
pub enum OptionKind {
    Bool,
    Enum(&'static [&'static str]),      // "side" | "back"
    F32 { min: f32, max: f32, step: f32 },
}

bitflags OptionFlags: PERSIST | REPLAN | NEEDS_TOPOLOGY | ADVANCED;

pub struct OptionSpec {
    pub key: &'static str,        // "surround_placement" — the single name.
                                  // Derives: OSC path, config key, snapshot
                                  // key (camelCase), UI binding id.
    pub kind: OptionKind,
    pub default: OptionValue,
    pub flags: OptionFlags,
    pub i18n_key: &'static str,
    pub help_i18n_key: &'static str,
}

pub static LIVE_OPTIONS: &[OptionSpec] = &[ /* ... */ ];
```

A macro generates an `OptionId` enum from the list, so hot-path reads are a
fixed-size array index (`store.get(OptionId::SurroundPlacement)`) — no hashmap
lookups or allocation in the audio thread, matching the realtime rules.

### 2. Everything else derived, once

- **OSC**: one generic `/omniphony/control/option <key> <value>` handler
  validating against the spec. Existing addresses stay as aliases until
  migration completes.
- **Persist + seed**: one generic save (options flagged `PERSIST`) and one
  generic config→store seed used by BOTH the CLI bootstrap and
  `Engine::from_paths` — the FFI/CLI parity bug class dies structurally.
- **Snapshot**: one loop emits `"options": { key: value, ... }` (plus the flat
  legacy keys during migration).
- **Studio Tauri**: a single passthrough field
  `options: serde_json::Value` (the precedent is the existing `binaural`
  passthrough) — the typed mirror disappears for registry options.
- **Studio JS**: generic ingestion `Object.assign(app.options, payload.options)`
  + a small **binder**: a control declares `data-option="surround_placement"`
  and the binder wires both directions (click → generic `control_option`
  invoke; state → reflect). Simple options (bool, enum) need no hand-written
  JS at all; the schema even carries what kind of control to render, exactly
  like the param sliders today. JS defaults come from the published schema —
  `state.js` hard-coded defaults (the lying `'side'`) disappear.
- **Plan invalidation**: the store keeps a monotonic `epoch` bumped whenever an
  option flagged `REPLAN` changes. `PlanSig`s compare **one epoch field**
  instead of enumerating options — a forgotten-sig-field is no longer possible
  for registry options.

Contributor cost for a new option: **one `OptionSpec` entry + i18n keys.**

### 3. Safety nets (land these first — they catch the class even before the registry)

1. **Renderer conformance test**: iterate the registry; assert every key
   appears in the snapshot JSON, round-trips through config store/get, and is
   accepted by the OSC dispatcher. One test that grows automatically.
2. **Studio contract check** (CI, node): the renderer build dumps
   `options-schema.json`; a script asserts every key has i18n coverage and
   either a bound control or an explicit "headless" annotation.
3. **Dead-export lint**: add `knip` (or eslint `no-unused-modules`) to the
   Studio CI. The April orphaning of `runtime-audio-state.js` — the root
   enabler of the incident — would have failed CI the day it happened.

## Alternatives considered

- **Discipline + CI checks only** (safety nets without the registry): cheap
  and worth doing immediately, but the 10-row table stays; checks catch
  *emitted-but-not-mirrored*, not *never-declared-anywhere-but-one-layer*.
  Insufficient alone — this class has already recurred several times.
- **Full passthrough state** (drop the typed `AppState` mirror wholesale):
  simplifies one layer but does nothing for the engine-side scatter
  (config/CLI/FFI/OSC/PlanSig) or the UI binding gap.
- **Registry (recommended)**: the only option that makes the contributor cost
  O(1) and removes the failure modes structurally rather than detecting them.
  The pattern is already proven in-repo at param scale.

## Migration plan (incremental, no big-bang)

- **Phase 0** — safety nets: conformance test + schema dump + knip. Small PRs,
  immediate protection for the current hand-wired options.
- **Phase 1** — registry core: `options.rs` types + store + epoch + generic
  OSC/persist/seed/snapshot + Tauri passthrough + JS generic ingestion.
  Migrate the fixed-channel-source family first (`surround_placement`,
  `output_channel_mapping`, `synthetic_objects_enabled`,
  `object_generator_id`, `phantom_extract_mode`) — the repeat offenders.
- **Phase 2** — Studio binder: `data-option` bindings for the migrated
  controls; delete their hand-written listeners/commands.
- **Phase 3** — fold remaining `LiveParams` scalars opportunistically as they
  get touched. New options MUST go through the registry (CONTRIBUTING note +
  the conformance test enforces it: a config key outside the registry and the
  known legacy list fails).

## Out of scope (adjacent, tracked separately)

- **Stale-instance state**: a long-lived engine keeps in-memory values that
  survive config normalization (the second half of the incident: two orender
  services alive on the same pipe/OSC port, one from the previous day). The
  registry makes the UI *show the truth*; it does not decide which instance
  should be alive. The existing heartbeat/epoch handshake already detects
  instance swaps — the yield protocol owns that problem.
- **Schema-driven layout/geometry state** (layouts, speakers): different shape
  (documents, not scalar options), stays on the domain-state path.
