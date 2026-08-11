# Named configuration profiles

Status: accepted — implemented behind this document.

Users listen to the same machine through different setups: a speaker array in
the living room, headphones at the desk, a second listening position with its
own delays and gains. Today switching between those means hand-editing
`config.yaml` or re-doing a dozen Studio tweaks. A *profile* names one complete
listening setup — speaker layout, render options, output mode (speaker /
binaural direct / binaural cascaded), backend and its parameters, gains — and
the stack can switch between profiles live, from Studio or over OSC.

This is phase C of the speaker-stage roadmap; it builds on the
SpeakerRenderStage extraction (#222), the cascaded binaural mode (#223), the
Studio output-mode combo (#224) and the live options registry
([live-options-registry.md](live-options-registry.md), #180–#182).

## Config schema

Two new **typed, top-level** keys in `config.yaml`:

```yaml
global: { … }

render:            # ← unchanged: the ACTIVE profile's content, authoritative
  current_layout: { … }
  render_backend: vbap
  binaural: { output_mode: speaker, … }
  …

active_profile: speakers

profiles:          # name → full render section (same schema as `render:`)
  speakers:
    current_layout: { … }
    …
  headphones:
    binaural: { output_mode: binaural, mode: cascaded }
    …
```

Design rules:

- **`render:` stays the single source the engine loads.** `Engine::from_paths`,
  the CLI bootstrap, the targeted option persist, `persist::save_live_config`
  and the `config.live.yaml` sidecar all keep operating on `config.render`
  exactly as before. That section *is* the active profile. Live OSC tweaks
  therefore persist into the active profile with **zero changes** to the
  existing persistence machinery.
- **`profiles:` mirrors every profile by name**, including the active one.
  `Config::save()` upserts `profiles[active_profile] = render` before
  serialising, so the mirror can never be more current than `render:` when
  written by a binary that knows about profiles.
- **Load rule:** the effective render config is `render:`, always. If a
  hand-edited file makes `profiles[active_profile]` differ from `render:`,
  `render:` wins for the running engine and the mirror is realigned on the
  next save. To activate a different profile by hand, edit `active_profile`
  *and* let the engine switch (or copy the section); the OSC/Studio switch is
  the supported path.
- Each profile entry is a full `RenderConfig`, so unknown keys inside a
  profile ride the existing `#[serde(flatten)] extra` convention and
  round-trip untouched. The two new top-level keys are typed on `Config`;
  older binaries park them in `Config::extra` and preserve them verbatim.

### Migration

Lazy, on first save by a profiles-aware binary: if `active_profile` is absent
it becomes `"default"` and `profiles.default` is written as a mirror of
`render:`. A flat legacy file is therefore valid as-is (implicitly one
"default" profile) and is only rewritten when something saves — no load-time
file mutation, no flag day. Older binaries keep reading `render:` and their
saves keep the new keys through `extra`.

### What a profile covers

Everything in `render:` — with one carve-out at **switch time**: the
machine-level input plumbing (`input_mode`, `input_pipe`, `live_input`,
`bridge_path`) is carried over from the outgoing render section instead of
taken from the incoming profile. Profiles describe the *output/listening*
side; the input side belongs to the machine. Output-side device fields stay
per-profile on purpose (a different speaker set is often a different DAC).

Fields that are only read at engine construction (input plumbing, output
backend/device, VBAP grid sizes) still land in `render:` on switch and take
effect at the next engine start, consistent with the existing
restart-required behaviour elsewhere.

## Switching semantics

`switch(new_name)` on the control thread. Switching to the already-active
name is a strict no-op (state re-broadcast only), and the target profile's
layout is preflighted first: a profile whose `speaker_layout` file no longer
loads refuses the switch outright instead of half-applying its parameters on
the previous layout.

1. **Commit** the current live state into `render:`
   (`options::store_live_to_config` + the same core field set as
   `persist::save_live_config`), then mirror it into `profiles[old_name]`.
   Pending unsaved tweaks are thus captured by the outgoing profile rather
   than lost — same spirit as the live-handoff sidecar. Host-owned fields
   (output device, live input, resampling, latency) are deliberately NOT
   amended here: they only take effect at engine start, so after a switch the
   running host state describes the previous profile; the on-disk values stay
   as persisted.
2. **Swap**: `render = profiles[new_name]` (with the input-plumbing carve-over),
   `active_profile = new_name`, `Config::save()`, delete the sidecar and clear
   the overlay cache (a deliberate state change supersedes any pending
   handoff).
3. **Re-seed** the live params from the new render section through the same
   seeding functions the construction path uses (see parity below) —
   preceded by a reset of every profile-covered live field to its default.
   The reset matters: the persist layer stores defaults as *absent* keys and
   the shared seeds only assign pinned values, so without it the outgoing
   profile's values (backend, evaluation mode, registry options, backend
   param bag) would leak into the incoming profile and be committed into it
   by the next save. A profile switch always requests a rebuild because the
   layout may have changed; if one is already running, the request is queued
   and re-triggered when it finishes rather than dropped.
4. **Stage + rebuild**: install the new profile's layout in
   `editable_layout`, bump the geometry generation and run the existing
   background recompute (`catch_unwind`, `recompute_error` broadcast on
   failure — a broken profile shows the standard Studio error banner and the
   audio keeps playing on the previous topology).
5. **Publish**: the new topology lands via the `ArcSwap` swap; the audio
   thread picks it up at the next frame boundary, band engines and cascade
   geometry refresh off the topology-identity check. No crossfade — a layout
   change is discontinuous by nature, exactly like today's layout edits.

Cheap live fields (output mode, binaural mode, gains) apply immediately in
step 3; the topology follows a few tens of milliseconds later. The transient
window is harmless: cascaded binaural re-derives its geometry when the new
topology publishes.

Failure modes: unknown profile name → error log + state re-broadcast, no
change. Rebuild failure → live params already reflect the new profile, the
error banner points at what to fix, the previous topology keeps rendering.

## Profile operations and OSC surface

Primitive operations, composed by Studio:

| Address                              | Args           | Effect |
|--------------------------------------|----------------|--------|
| `/omniphony/control/profile/switch`  | `s name`       | switch as above |
| `/omniphony/control/profile/create`  | `s name`       | commit live → clone into `profiles[name]` (no switch) |
| `/omniphony/control/profile/delete`  | `s name`       | remove; refused for the active profile |
| `/omniphony/control/profile/rename`  | `s old, s new` | rename key; follows `active_profile` if it was active |
| `/omniphony/state/profiles`          | `s json`       | broadcast `{"active": …, "names": […]}` |

Names are trimmed, non-empty, unique; create refuses an existing name,
rename refuses a colliding target. Every mutation saves the config and
re-broadcasts the profiles state; the state also rides the periodic snapshot.

**Why not a registry `OptionSpec` row?** The registry models scalar fields of
`LiveParams` with pure `set`/`config_store` functions; a profile switch is a
whole-config transaction with file I/O, layout staging and a forced rebuild.
It follows the registry's *conventions* (contract constants in
`osc_contract::ALL_CONTROL`, snapshot block, conformance test) without
pretending to be an option.

## FFI/CLI parity

The config→control seeding half of `build_spatial_renderer` (backend id and
params, evaluation mode, hybrid, distance metrics, binaural block, plus the
registry seed and the ramp/DRC/meter-rate seeds currently duplicated in
`Engine::from_paths` and the CLI bootstrap) is extracted into
`seed_control_from_render_config(control, render_cfg)`. Construction (both
hosts go through `build_spatial_renderer`) and the live switch call the same
function, so a profile activated at boot and the same profile activated live
cannot drift. `RendererControl` gains a small `profiles` info block (active
name + name list) seeded at boot by both hosts and updated by the ops above;
`Engine::from_paths` remains the FFI anchor per the existing parity rule.

## Studio

A compact always-visible row at the top of the left overlay (above the
accordion sections, so expanding it never resizes the 3D viewport): a
`form-select` profile picker plus create / rename / delete icon buttons
(`user-select: none`, WebKitGTK first-click rule). The picker follows the
output-mode combo pattern: change → Tauri command → OSC, state echoes applied
back with an `applying` guard, never trusted locally. `app_state.rs` mirrors
`{activeProfile, profileNames}` (camelCase renames), fed from
`/omniphony/state/profiles`. Create prompts for a name, then sends
`create` + `switch`. All labels through i18n, 8-locale parity gated by
`npm run i18n:check -- --strict`.
