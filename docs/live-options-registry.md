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
- **Phase 3 (in progress)**: remaining `LiveParams` scalars fold in
  opportunistically, as they get touched. See "Current state" below for what
  the registry declares today.
- **Groups, step 1 landed**: `OptionGroup` (mode + apply effect), the
  `FloatArray` kind, `apply_batch` and the grouped `/control/options`
  setter, validated on the `room` group (see "Groups" below). Next steps, one
  PR each: the evaluation, distance and backend groups; the binaural groups
  (HRIR source, BRIR, crossover, head tracking) and the ungrouped binaural
  scalars; then `Staged` groups declared by the host (audio output, live
  input), with the JSON patches and their `/apply` as exact aliases and the
  options published per host (`decode_thread` by the embedded engine only).

### Current state

Declared options (`renderer::options::LIVE_OPTIONS`):

| Key | Kind | Default | Flags | Legacy alias |
|---|---|---|---|---|
| `surround_placement` | `Enum` side / back | `side` | REPLAN | `/control/surround_placement` |
| `synthetic_objects_enabled` | `Bool` | `false` | REPLAN | `/control/synthetic_objects` |
| `decode_thread` | `Bool` | `false` | — | `/control/decode_thread` |
| `output_channel_mapping` | `Enum` by_index / by_name | `by_index` | — | `/control/output_channel_mapping` |
| `object_generator_id` | `Str` | `""` | REPLAN | `/control/object_generator` |
| `phantom_extract_mode` | `Enum` off / broadband / spectral | `off` | REPLAN | `/control/phantom_extract` |
| `crossover_type` | `Enum` lr4 / fir | `lr4` | — | `/control/crossover_type` |
| `crossover_fir_transition_ratio` | `Float` 0.05–2.0, step 0.05 | `0.5` | — | `/control/crossover_fir_transition_ratio` |
| `hrir_update_lattice` | `Enum` exact / fine / balanced / coarse | `exact` | — | `/control/binaural/hrir_update_lattice` |
| `auto_gain` | `Bool` | `false` | — | `/control/auto_gain` |
| `auto_gain_ceiling_db` | `Float` −12–0 dBFS, step 0.1 | `-1` | — | `/control/auto_gain_ceiling` |
| `use_loudness` | `Bool` | `false` | — | `/control/loudness` |
| `ramp_mode` | `Enum` off / frame / interp / sample | `frame` | — | `/control/ramp_mode` |
| `drc_mode` | `Str` (the bridge's modes) | `Off` | — | `/control/input/drc_mode` |
| `drc_weight` | `Float` 0–1, step 0.01 | `1` | — | `/control/input/drc_weight` |
| `room_ratio` | `FloatArray` ×3, 0.01–100, step 0.01 | `[1, 2, 1]` | group `room` | `/control/room_ratio` |
| `room_ratio_rear` | `Float` 0.01–100, step 0.01 | `2` | group `room` | `/control/room_ratio_rear` |
| `room_ratio_lower` | `Float` 0.01–100, step 0.01 | `0.5` | group `room` | `/control/room_ratio_lower` |
| `room_ratio_center_blend` | `Float` 0–1, step 0.01 | `0.5` | group `room` | `/control/room_ratio_center_blend` |

(Aliases are under `/omniphony`; the contract constants live in
`osc-contract/src/lib.rs`.) Every other live setting is still a hand-wired
`LiveParams` field with its own OSC handler; the conformance net keeps a
table of the hand-wired options that predate the registry.

What the implementation settled on, where it differs from the proposal below:

- **Kinds**: `Bool`, `Enum(&[&str])`, `Str` (free-form, e.g. a registry id),
  `Float { min, max, step }` (the proposal's `F32`; `step` is a UI hint,
  the setter clamps to `[min, max]`) and `FloatArray { len, min, max, step }`
  (`len` numbers set together, each clamped; `len` wire arguments — the
  kind's `arity()`).
- **Flags**: only `REPLAN` (bump `RendererControl::options_epoch` on a real
  change). `NEEDS_TOPOLOGY` and `ADVANCED` were never needed and do not exist.
  `PERSIST`, a write to `config.yaml` on every OSC set, was removed: options
  change what is heard, so every row reaches the file through the Save button
  only and is seeded from it at boot (`docs/persistence-policy.md`). An OSC
  set marks the config dirty only when the value actually changed.
- **Setters**: `raw_bool` / `raw_str` / `raw_float` read a raw value by
  shape; a `Float` row states its bounds once, in its `kind` constant, and
  the setter (`raw_float`) and the seed (`clamp_to`) read them from there.
- **Storage**: no store and no macro-generated `OptionId`. Options stay typed
  `LiveParams` fields read directly on the audio path; a row carries
  `set` / `get_json` / `config_store` / `config_seed` function pointers that
  reach them. The registry is the declaration and plumbing layer, not the
  storage — which is what the realtime rules asked for anyway.
- **Per-row extras**: `help_i18n_key` is optional; `legacy_control_addr` names
  the pre-registry address kept as an alias.
- **Profile switch**: `reset_live_to_defaults` puts every declared option (and
  the param bags and placement) back to its declared default before the new
  profile's `seed_live_from_config`, because a seed only assigns the keys the
  config pins — a skip-if-default key would otherwise keep the previous
  profile's value.
- **Groups**: see "Groups" below.
- **Not built**: the phase-3 check "a config key outside the registry and the
  known legacy list fails" does not exist; the conformance net only proves
  that each declared (or hand-wired) option reaches every layer.

### Groups

Some options only make sense together — the room's width, length, height,
rear, lower and centre blend; later a backend and its parameters, or an
output device, its rate and its buffer. Written one address at a time, each
write started its own topology rebuild, the first one on a half-written room
(`trigger_layout_recompute` queues one catch-up rebuild behind a running one,
but cannot know more writes are coming).

An `OptionGroup` is declared once in `options.rs` and named by its member
rows (`OptionSpec::group`). It carries:

- a **mode**: `Live` — applied as it arrives, a multi-key write applied as
  one. (`Staged` — requested value ≠ applied value, a pending flag, applied
  atomically on command — arrives with the host-declared audio groups.)
- an **apply effect** (`ApplyEffect`): `None` (read where it is used, or
  compared by the stage that built from it), `Replan` (bump the options
  epoch, like the `REPLAN` flag), `Topology` (rebuild backend geometry and
  evaluation) or `Evaluation` (rebuild the evaluation layer, reuse the gain
  models). Reload and restart effects join with the groups that need them.

`options::apply_batch` applies a list of (option, value) pairs under one
write lock, then — only if something changed — marks the config dirty once,
bumps the options epoch at most once, and returns the widest `Rebuild` any
changed option's group asks for. `/control/option`, the legacy aliases and
`/control/options` all go through it (`runtime_control::live_control`), so a
rebuild is only asked for when a value actually moved: Studio still sends the
four room addresses on every room edit, and now only the one that changed
rebuilds.

The profile switch and a config load already apply a whole config as one
batch: `apply_render_config_live` resets and seeds every option, then
`apply_switched_profile` triggers a single rebuild; at boot the renderer is
built from the resolved config and seeded before its first rebuild.

The schema entry of a grouped option carries its group:
`"group": {"key", "mode", "effect", "i18nKey"}` — enough for Studio to draw a
generic group header or, for a `Staged` group, an Apply button.

The room keeps its file representation: `config_fields::room` writes metres
against the layout radius (`store_ratio` / `store_rear` / `store_lower`) and
reads ratios back through `room::resolve`, the single reading shared by the
renderer build, the live seed and the profile switch — so the dependent
default (an absent rear follows the length) lives in one place.

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
| 5 | OSC control handler | `runtime_control/src/live_control.rs` (+ `osc_contract`) |
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

> Historical: this is the design as proposed. Where the implementation
> differs (kinds, flags, no store / `OptionId`), "Current state" above wins.

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
- **Persist + seed**: one generic save (every option, on Save) and one
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
