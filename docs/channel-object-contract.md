# RFC: one channel/object contract across bridge, renderer and hosts

Status: accepted — written after the DTS:X fixed-7.1.4 campaign was rolled back
(bridge `research/spatial-object-layer`, Omniphony reverts `92117b2`/`ec2ae04`).
Companion review notes live in the bridge repo
(`docs/dtsx-objects-campaign.md`).

Progress:

- **Phase 0 (this document)**: contract specification and migration plan.
- Phases 1–5: not started (see "Migration plan" below).

## The incident

To ship the DTS:X fixed 7.1.4 presentation, the bridge fabricated an
`RMetadataFrame` for a stream that has no dynamic objects: eight positionless
bed events on the 0–9 bed-id scheme plus four "objects" pinned at cube
corners. That single decision cascaded:

- the engine switched to the spatial path (`has_objects`), so the fixed
  heights were VBAP-panned at corner positions instead of routed one-hot to
  matching speakers, unlike every other fixed presentation;
- Omniphony grew a reverse-lookup hack (`canonical_bed_name`) to re-derive
  display names the labels already carried;
- Studio inferred "this is really a speaker feed" from `directSpeakerIndex`
  to fix its icons;
- the bridge encoded a wrong id mapping (DCA Cs → bed id 6 = Lb) because the
  0–9 scheme cannot express what the format contains;
- static, byte-identical metadata was rebuilt and re-emitted with fresh
  allocations on every ~512-sample frame.

None of this would have existed if the stream had been presented the way the
decoder already labels it: twelve fixed channels. The root cause is not the
campaign; it is that the stack has **two parallel models** for the same
concept and no written rule for choosing between them.

## What we have today

Two id spaces describing the same thing:

- `RChannelLabel` (24 values, `bridge_api`): per-channel labels on every
  decoded frame;
- bed ids 0–9 (`RMetadataFrame.bed_indices` + `REvent.id < 10`): a second,
  narrower channel vocabulary, related to the first only by convention.

Four places that map between names, labels, ids and speakers:

1. `orender_engine/src/channel_layout.rs` — speaker name → `RChannelLabel`
   (alias-tolerant matcher);
2. `renderer/src/speaker_layout.rs::bed_to_speaker_mapping` — a second,
   near-identical alias table, bed id → speaker index;
3. `orender_engine/src/spatial.rs::canonical_bed_name` (reverted) — bed id →
   display name;
4. each bridge pipeline — format channel → bed id.

Two render paths in the engine: the spatial path (driven by
`RMetadataFrame`, beds via `configure_beds` + OSC objects) and the
channel-label path (`virtual_bed::plan_channel_render`, driven by
`channel_labels` + the user's placement layout + the live channel mode).
`Bridge::is_spatial()` selects between them and leaks all the way to mpv as
a *mode*, latched at session prep before any packet is decoded.

## The contract

### Model

Every presentation is exactly this, for every format:

- **Fixed channels** — PCM channels with a stable spatial meaning, described
  by `RChannelLabel` in `RDecodedFrame.channel_labels`. Always present,
  always authoritative. A plain 5.1 track, a DTS:X fixed 7.1.4
  presentation and the bed part of a TrueHD Atmos stream are all just fixed
  channels.
- **Dynamic objects** — PCM channels whose position is driven by decoded
  metadata. Declared explicitly by the bridge; absent for most formats.

The bridge **describes**; the renderer **decides**. A bridge never fabricates
positions, never pre-spatialises, never assumes an output layout. What
happens to a fixed channel (direct one-hot routing, VBAP virtualisation at a
placement-layout pose, host passthrough) is exclusively the renderer's
decision, driven by the placement layout and the live channel mode — for all
formats identically.

There is **no spatial mode**. "This presentation currently carries dynamic
objects" (`has_objects`) is an observable, live *fact* about the stream —
hosts may consult it (e.g. mpv keeps object-bearing tracks on the renderer
in Host mode, because a host cannot render objects) but nothing latches it
as a rendering mode.

### Glossary

- **Fixed channel** — a PCM channel with a static spatial meaning, named by
  `RChannelLabel`.
- **Dynamic object** (or just *object*) — a PCM channel positioned by
  metadata events.
- **Placement layout** — the user-editable layout that gives virtualised
  fixed channels their poses (today called the *virtual bed*; the old name
  overloads "bed" and should fade out).
- **Channel plan** — the renderer's per-stream decision table: for each
  fixed channel, direct speaker / virtual pose / passthrough. Computed once
  per declaration, not per frame.
- **Declaration** — the parts of the contract that change rarely and are
  cached by the consumer: channel labels, object set, object names.

### ABI (`bridge_api` v2)

`RDecodedFrame` is unchanged: `channel_labels` (length == `channel_count`)
describes every channel. Channels that carry object audio are labeled with a
new variant `RChannelLabel::Object`; a frame is thus self-describing even
before its metadata is examined.

`RMetadataFrame` describes objects only:

```rust
pub struct RMetadataFrame {
    /// Position/gain/size events, keyed by object id.
    pub events: RVec<REvent>,
    /// Sparse declaration: which PCM channel carries which object.
    /// Emitted on the first metadata frame, on any change, and after reset().
    pub object_channels: RVec<RObjectChannel>,   // { id: u32, channel: u32 }
    /// Sparse object-name updates, keyed by object id.
    pub name_updates: RVec<RNameUpdate>,
    pub sample_pos: u64,
    pub ramp_duration: u32,
}
```

Removed: `bed_indices`, the 0–9 bed-id scheme, positionless bed events, and
the `id ≥ 10 → channel num_beds + (id − 10)` arithmetic. `REvent.has_pos`
loses its "bed channel" meaning (every event belongs to an object; a
position-less event is a gain/ramp-only update).

Object ids are opaque, bridge-chosen, stable for the lifetime of the object
(they survive seeks and channel remaps; Studio keys solo/mute on them). The
`object_channels` mapping is the only link between an id and its PCM
channel.

Emission rules:

- labels: every frame (they ride the frame struct; cheap);
- events: whenever the format provides them, with `ramp_duration`;
- declarations (`object_channels`, `name_updates`): sparse — first
  metadata frame, on change, after `reset()`. Consumers cache them
  unconditionally (whether or not a UI is attached), so a late-attaching
  consumer sees stable state.
- A presentation with no objects emits **no** `RMetadataFrame` at all.

`FormatBridge::is_spatial()` is renamed `has_objects()` and re-specified: it
reports the *current* observed state, may flip in either direction
mid-stream, and must not be latched by callers. (During phase 2 the old
method remains as a deprecated alias so the trait change and the call-site
migration can land separately.)

### Rendering (engine/CLI)

One render path. Per stream, the engine builds a **channel plan** from
`channel_labels` × placement layout × live channel mode (the existing
`plan_channel_render` logic, generalised): each fixed channel is planned as
direct-to-speaker or virtual-at-pose; object channels are excluded from the
plan and rendered from their events, exactly as the spatial path does today.

- The plan is cached; per-frame work is table lookups only. No allocation,
  no hashmap access, no re-derivation of static data in the audio path.
- The plan is recomputed only on a **declaration change** (labels changed,
  object set changed, mode/layout changed). Every plan transition is ramped
  (`PLAN_TRANSITION_RAMP`, proposed 20 ms) — this covers stream start
  (pre-metadata frames), an extension appearing/disappearing mid-stream
  (the DTS:X fold/unfold click), and live mode switches, with one mechanism.
- The CLI shares the same code path; `is_spatial_presentation` latching at
  session prep disappears.

### Naming

One table in `bridge_api` (new module, e.g. `labels.rs`) is the single
source of truth per `RChannelLabel`: canonical short name (`"TFL"`), display
name, and accepted aliases. `channel_layout::label_for_speaker_name` and
`speaker_layout::bed_to_speaker_mapping` become views over it (the latter
re-typed label → speaker index). Fixed channels are displayed under their
label's canonical name — no reverse-lookup table, no `Obj_<n>` fallback for
fixed channels. Objects are displayed from `name_updates`, falling back to
`Obj_<id>`.

### Hosts

- **liborender / mpv**: new symbol `orender_has_objects` with the live-fact
  semantics; `orender_is_spatial` stays exported as a deprecated alias (ABI
  0.5 → 0.6, additive). mpv's routing rule (already implemented by the
  pending `fix(ad): keep object content on spatial renderer`): object
  content stays on the renderer regardless of channel mode; channel-based
  content follows the live mode. Track-info profile strings ("DTS:X",
  "Atmos") come from the container/decoder profile, not from the routing
  fact — a fixed-presentation DTS:X track displays as DTS:X while rendering
  as labeled channels.
- **OSC / Studio**: the per-source payload gains an explicit
  `fixed: bool` + `label: string` (serde layer: `app_state.rs`, as usual).
  Fixed channels stay listed as sources (solo/mute, position icon at their
  planned pose — direct or virtual); dynamic objects keep today's
  presentation. `directSpeakerIndex` remains as position information but no
  longer encodes "this is a speaker feed".

## Migration plan

Each phase is one reviewable PR; the stack stays shippable between phases.

- **Phase 1 — one label table (no ABI change).** Add `bridge_api::labels`;
  rewrite `label_for_speaker_name` and `bed_to_speaker_mapping` as views;
  parity tests against the current tables. Also: give the bridge repo a
  real push/PR CI gate (fmt, build, tests) — the campaign shipped nine
  commits with zero CI.
- **Phase 2 — ABI v2 + unified render path.** `RMetadataFrame` v2 +
  `RChannelLabel::Object` + `has_objects()`; TrueHD and E-AC-3/JOC pipelines
  emit labeled fixed channels + `object_channels`; `reference_bridge`
  updated; engine renders through the cached channel plan with ramped
  transitions; CLI latch removed. Re-lands the reverted sparse-names caching
  in its correct form (declarations cached before any consumer attaches).
  `bridge_api` version bump; bridge and renderer ship together per the
  stack release model.
- **Phase 3 — liborender + mpv.** `orender_has_objects` (+ deprecated
  alias), ABI 0.6; adapt the pending mpv fix, port it to `orender-master`;
  regenerate mpv-omniphony patches at the next release.
- **Phase 4 — Studio/OSC.** `fixed` + `label` in the source payload and
  `app_state.rs`; icon logic reads the explicit flag; i18n for any new
  strings.
- **Phase 5 — DTS:X re-landing (bridge).** Decoder + review fixes (non-fatal
  EXSS tail parse, `checked_sub`, bed length validation, integer-domain
  unfold) + twelve labeled channels. No fabricated metadata, no
  Omniphony-side code. A/B listening pass on the DTS:X corpus before merge;
  fix the inconsistent corpus dump path in the gated tests.

## Non-goals

- No change to how true dynamic objects are rendered (VBAP, ramps, extent).
- No change to decode internals of any format (phase 5 is a separate,
  already-reviewed effort).
- No new user-facing rendering options; the channel modes and placement
  layout keep their semantics, they just apply uniformly.

## Open questions

- `PLAN_TRANSITION_RAMP` duration: fixed 20 ms vs. frame-quantised vs. a
  (non-live) config constant.
- How far to push the *placement layout* rename in code and UI (the
  `virtual_bed` module, YAML keys, Studio strings) — cosmetic, can trail.
- Whether `object_channels` should also carry a per-object `RChannelLabel`
  hint for formats whose objects have a nominal home speaker (DTS:X nav
  metadata may provide this); reserved field vs. later ABI addition.
- Exact `abi_stable` versioning mechanics for the bridge dylib ↔ renderer
  pairing (root-module version bump vs. crate version gate).
