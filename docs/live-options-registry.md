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
  setter, validated on the `room` group (see "Groups" below).
- **Groups, step 2 landed**: the `distance_model`, `distance_diffuse`,
  `evaluation` and `backend` groups (+ the ungrouped position
  interpolation); `OptionEnv`, `LegacyAddr::Prefixed`, the `Int` and
  `DynamicEnum` kinds and the `Build` default; the rebuilding rows seeded
  before the first rebuild.
- **Groups, step 3 landed**: the binaural stage — the `hrir_source`,
  `brir`, `crossover` (the two existing crossover rows) and `head_tracking`
  groups, the ungrouped binaural scalars (output and binaural modes, unit
  scale, head radius, air absorption, diffuse-field EQ, reflections, reverb,
  both ear gains) and the master gain; the `Reload` effect and
  `LegacyAddr::None`.
- **Groups, step 4 landed**: host-declared options (`HostOptionSpec<H>`,
  published and set through `HostControlHandler`'s option methods) — the
  standalone renderer's `audio_output` (live, restarts the output),
  `adaptive_resampling` (live) and `live_input` (`Staged`, restarts the input
  when applied) groups, 38 options; the JSON patches and their `/apply` as
  aliases; `/control/options/apply`; `/state/host_options`; the
  `OptionalInt` kind, the `Unset` default and a `Null` raw value;
  `OptionFlags::EMBEDDED_ONLY` (`decode_thread`).
- **Groups, step 5 landed** (native Studio): the Studio reads `/state/host_options`
  and derives the `staged` groups from the schema (`Live::staged_groups`); a
  generic row (`panels/staged_apply.rs`) offers the Apply of a group while it
  holds staged values, through `/control/options/apply`. The Audio Input
  section shows it under its own Apply, which still sends the section's edits
  as one document. The web Studio does not have it.

### Current state

Declared options (`renderer::options::LIVE_OPTIONS`):

<!-- BEGIN GENERATED live-options -->
| Key | Value | Default | Group (mode, effect) | Flags | Alias |
|---|---|---|---|---|---|
| `surround_placement` | `side` \| `back` | `"side"` | — | replan | `/control/surround_placement` |
| `synthetic_objects_enabled` | bool | `false` | — | replan | `/control/synthetic_objects` |
| `decode_thread` | bool | `false` | — | embedded only | `/control/decode_thread` |
| `output_channel_mapping` | `by_index` \| `by_name` | `"by_index"` | — | — | `/control/output_channel_mapping` |
| `object_generator_id` | string | `""` | — | replan | `/control/object_generator` |
| `phantom_extract_mode` | `off` \| `broadband` \| `spectral` | `"off"` | — | replan | `/control/phantom_extract` |
| `crossover_type` | `lr4` \| `fir` | `"lr4"` | `crossover` (live, reload) | — | `/control/crossover_type` |
| `crossover_fir_transition_ratio` | float [0.05, 2], step 0.05 | `0.5` | `crossover` (live, reload) | — | `/control/crossover_fir_transition_ratio` |
| `auto_gain` | bool | `false` | — | — | `/control/auto_gain` |
| `auto_gain_ceiling_db` | float [-12, 0], step 0.1 | `-1` | — | — | `/control/auto_gain_ceiling` |
| `use_loudness` | bool | `false` | — | — | `/control/loudness` |
| `ramp_mode` | `off` \| `frame` \| `interp` \| `sample` | `"frame"` | — | — | `/control/ramp_mode` |
| `sample_ramp_stride` | int [1, 32] | `8` | — | — | — |
| `drc_mode` | string | `"Off"` | — | — | `/control/input/drc_mode` |
| `drc_weight` | float [0, 1], step 0.01 | `1` | — | — | `/control/input/drc_weight` |
| `dialogue_gain_db` | float [-12, 12], step 0.5 | `0` | — | — | — |
| `hrir_update_lattice` | `exact` \| `fine` \| `balanced` \| `coarse` | `"exact"` | `hrir_source` (live, reload) | — | `/control/binaural/hrir_update_lattice` |
| `room_ratio` | 3 floats [0.01, 100], step 0.01 | `[1, 2, 1]` | `room` (live, topology) | — | `/control/room_ratio` |
| `room_ratio_rear` | float [0.01, 100], step 0.01 | `2` | `room` (live, topology) | — | `/control/room_ratio_rear` |
| `room_ratio_lower` | float [0.01, 100], step 0.01 | `0.5` | `room` (live, topology) | — | `/control/room_ratio_lower` |
| `room_ratio_center_blend` | float [0, 1], step 0.01 | `0.5` | `room` (live, topology) | — | `/control/room_ratio_center_blend` |
| `vbap_distance_model` | `none` \| `linear` \| `quadratic` \| `inverse-square` | `"none"` | `distance_model` (live, topology) | — | `/control/distance_model` |
| `distance_model_metric` | `spherical` \| `chebyshev` | `"spherical"` | `distance_model` (live, topology) | — | `/control/distance_model_metric` |
| `distance_diffuse` | bool | `false` | `distance_diffuse` (live, topology) | — | `/control/distance_diffuse/enabled` |
| `distance_diffuse_threshold` | float [0.000001, 100], step 0.01 | `1` | `distance_diffuse` (live, topology) | — | `/control/distance_diffuse/threshold` |
| `distance_diffuse_curve` | float [0, 100], step 0.05 | `1` | `distance_diffuse` (live, topology) | — | `/control/distance_diffuse/curve` |
| `distance_diffuse_metric` | `spherical` \| `chebyshev` | `"spherical"` | `distance_diffuse` (live, topology) | — | `/control/distance_diffuse/metric` |
| `distance_diffuse_mirror_axes` | `none` \| `x` \| `y` \| `z` \| `xy` \| `xz` \| `yz` \| `xyz` | `"xy"` | `distance_diffuse` (live, topology) | — | `/control/distance_diffuse/mirror_axes` |
| `render_evaluation_mode` | `auto` \| `realtime` \| `precomputed_polar` \| `precomputed_cartesian` | `"auto"` | `evaluation` (live, evaluation) | — | `/control/render_evaluation_mode` |
| `evaluation_object_size_intervals` | int ≥ 0 | `0` | `evaluation` (live, evaluation) | — | `/control/render_evaluation/object_size_intervals` |
| `evaluation_cartesian_x_size` | int ≥ 1 | as built | `evaluation` (live, evaluation) | — | `/control/render_evaluation/cartesian/x_size` |
| `evaluation_cartesian_y_size` | int ≥ 1 | as built | `evaluation` (live, evaluation) | — | `/control/render_evaluation/cartesian/y_size` |
| `evaluation_cartesian_z_size` | int ≥ 1 | as built | `evaluation` (live, evaluation) | — | `/control/render_evaluation/cartesian/z_size` |
| `evaluation_cartesian_z_neg_size` | int ≥ 1 | as built | `evaluation` (live, evaluation) | — | `/control/render_evaluation/cartesian/z_neg_size` |
| `vbap_azimuth_resolution` | int ≥ 1 | `360` | `evaluation` (live, evaluation) | — | `/control/render_evaluation/polar/azimuth_resolution` |
| `vbap_elevation_resolution` | int ≥ 1 | as built | `evaluation` (live, evaluation) | — | `/control/render_evaluation/polar/elevation_resolution` |
| `vbap_distance_res` | int ≥ 1 | `8` | `evaluation` (live, evaluation) | — | `/control/render_evaluation/polar/distance_res` |
| `vbap_distance_max` | float [0.01, 1000], step 0.1 | `2` | `evaluation` (live, evaluation) | — | `/control/render_evaluation/polar/distance_max` |
| `render_evaluation_position_interpolation` | bool | `true` | — | — | `/control/render_evaluation/position_interpolation` |
| `render_backend` | one of `backends` | `"vbap"` | `backend` (live, topology) | — | `/control/render_backend` |
| `hybrid_external_backend` | one of `backends` | `"vbap"` | `backend` (live, topology) | — | `/control/hybrid/external_backend` |
| `hybrid_internal_backend` | one of `backends` | `"barycenter"` | `backend` (live, topology) | — | `/control/hybrid/internal_backend` |
| `hybrid_curve_smoothing` | float [0, 1], step 0.01 | `0` | `backend` (live, topology) | — | `/control/hybrid/curve_smoothing` |
| `hybrid_metric` | `spherical` \| `chebyshev` | `"chebyshev"` | `backend` (live, topology) | — | `/control/hybrid/metric` |
| `output_mode` | `speaker` \| `binaural` | `"speaker"` | — | — | `/control/output_mode` |
| `binaural_mode` | `direct` \| `cascaded` | `"direct"` | — | — | `/control/binaural_mode` |
| `hrir_source` | string | `"saf"` | `hrir_source` (live, reload) | — | `/control/binaural/hrir_source` |
| `brir_head_tracking` | `auto` \| `on` \| `off` | `"auto"` | `brir` (live, reload) | — | `/control/binaural/brir/head_tracking` |
| `brir_max_length_s` | float [0, 10], step 0.1 | `2` | `brir` (live, reload) | — | `/control/binaural/brir/max_length` |
| `brir_tail_floor_db` | float [20, 120], step 1 | `60` | `brir` (live, reload) | — | `/control/binaural/brir/tail_floor` |
| `binaural_unit_scale_m` | float [0.01, 100], step 0.01 | `1` | — | — | `/control/binaural/unit_scale` |
| `binaural_head_radius_m` | float [0.05, 0.15], step 0.001 | `0.0875` | — | — | `/control/binaural/head_radius` |
| `binaural_air_absorption` | bool | `true` | — | — | `/control/binaural/air_absorption` |
| `binaural_diffuse_field_eq` | bool | `false` | — | — | `/control/binaural/diffuse_field_eq` |
| `reflections_enabled` | bool | `false` | — | — | `/control/binaural/reflections/enabled` |
| `reflections_level` | float [0, 1], step 0.01 | `0.5` | — | — | `/control/binaural/reflections/level` |
| `reflections_wall_cutoff_hz` | float [1000, 20000], step 100 | `6000` | — | — | `/control/binaural/reflections/wall_cutoff` |
| `reflections_room_width_m` | float [1, 20], step 0.1 | `4` | — | — | `/control/binaural/reflections/room_width` |
| `reflections_room_depth_m` | float [1, 20], step 0.1 | `5` | — | — | `/control/binaural/reflections/room_depth` |
| `reflections_room_height_m` | float [1, 20], step 0.1 | `2.7` | — | — | `/control/binaural/reflections/room_height` |
| `reverb_enabled` | bool | `false` | — | — | `/control/binaural/reverb/enabled` |
| `reverb_level` | float [0, 1], step 0.01 | `0.25` | — | — | `/control/binaural/reverb/level` |
| `reverb_rt60_s` | float [0.1, 3], step 0.01 | `0.35` | — | — | `/control/binaural/reverb/rt60` |
| `reverb_predelay_ms` | float [0, 100], step 1 | `20` | — | — | `/control/binaural/reverb/predelay` |
| `reverb_size` | float [0.5, 2], step 0.05 | `1` | — | — | `/control/binaural/reverb/size` |
| `reverb_rt60_low_ratio` | float [0.25, 4], step 0.05 | `1` | — | — | `/control/binaural/reverb/rt60_low_ratio` |
| `reverb_rt60_high_ratio` | float [0.25, 4], step 0.05 | `1` | — | — | `/control/binaural/reverb/rt60_high_ratio` |
| `head_tracking_smoothing` | float [0, 0.999], step 0.01 | `0.2` | `head_tracking` (live, none) | — | `/control/head/tracking/smoothing` |
| `head_tracking_invert` | bool | `false` | `head_tracking` (live, none) | — | `/control/head/tracking/invert` |
| `head_tracking_osc_address` | string | `""` | `head_tracking` (live, none) | — | `/control/head/tracking/address` |
| `head_tracking_format` | `auto` \| `quat` \| `rotvec` \| `euler` | `"auto"` | `head_tracking` (live, none) | — | `/control/head/tracking/format` |
| `binaural_ear_gains` | 2 floats [0, 4], step 0.01 | `[1, 1]` | — | — | — |
| `master_gain` | float [0, 1000], step 0.01 | `1` | — | — | `/control/gain` |
<!-- END GENERATED live-options -->

Generated from the registry (`renderer::options::doc_table`; a test fails
when the table drifts, `UPDATE_DOC_TABLES=1 cargo test -p host_audio
doc_tables` rewrites it). `hrir_source` takes a selector: `saf`,
`sofa:<path>`, `pinna:<preset>:<d>:<depth>`, …; `binaural_ear_gains` has no
address of its own (`/control/binaural/ear_gain` sets one ear and stays
hand-wired).

(Aliases are under `/omniphony`; the contract constants live in
`osc-contract/src/lib.rs`.) Every other live setting is still a hand-wired
`LiveParams` field with its own OSC handler; the conformance net keeps a
table of the hand-wired options that predate the registry.

What the implementation settled on, where it differs from the proposal below:

- **Kinds**: `Bool`, `Enum(&[&str])`, `Str` (free-form, e.g. a registry id),
  `Float { min, max, step }` (the proposal's `F32`; `step` is a UI hint,
  the setter clamps to `[min, max]`) and `FloatArray { len, min, max, step }`
  (`len` numbers set together, each clamped; `len` wire arguments — the
  kind's `arity()`), `Int { min, max }` (a number rounded to the nearest
  integer, clamped) and `DynamicEnum { source }` (one of a set the host
  provides at runtime — `backends` — validated against the running host).
- **Defaults**: `OptionDefault::Build` declares no fixed default — the value
  the renderer was built with (the cartesian grid the bridge suggests, the
  polar elevation count, which depends on whether negative elevations are
  rendered). It is published as `null`, and a profile reset leaves the option
  to the incoming profile's seed.
- **Environment**: every `set` / `config_seed` / `config_store` receives an
  `OptionEnv`: the registered backends (`has_backend`) and the facts the
  running renderer was built with (`build_facts`: preferred evaluation mode,
  negative elevations). Never the live params — a setter runs inside their
  write guard. `OptionEnv::detached()` serves code without a control.
- **Aliases**: `LegacyAddr::Exact(addr)` for a whole address,
  `LegacyAddr::Prefixed { prefix, tail }` for the contract's prefix families
  (`distance_diffuse/…`, `hybrid/…`, `render_evaluation/{cartesian,polar}/…`).
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
- **Declared rows** (#682): an option whose live value is a top-level field
  is one row of `declared_options!` (`renderer/src/options/declared.rs`):
  `key: Category = default => { kind, flags, group, i18n, help, alias }`,
  the category being `Bool`, `Float`, `Int`, `Str` or `Enum(Type)`. The
  macro generates the `LiveParams::options` field and its default, the
  `RenderConfig::options` field (flattened: the YAML key is the option key;
  an enum is read through its `from_str`, aliases included, and an unknown
  value is kept like any other enum key's), the `options::defaults`
  constant and the registry row, whose four functions follow from the
  category (a float is saved to six decimals and omitted within 1e-4 of its
  default; a config value is bounded like a client write). A row may
  replace any of them (`set:` / `store:` / `seed:`) for a legacy wire shape
  or a value always written. The other options sit inside a larger structure
  (binaural, room, evaluation, hybrid) and stay hand-written rows
  (`HAND_WIRED_ROWS`); `LIVE_OPTIONS` is the declared rows, then those.
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

`options::apply_batch` applies a list of (option, value) pairs in one
write of the live params (published once), then — only if something changed — marks the config dirty once,
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

The rows whose group asks for a rebuild must be in place before that first
rebuild, and must say whether they call for it. `seed_control_from_render_config`
(the construction path, replayed by the profile switch) therefore runs
`options::seed_rebuilding_rows_from_config`, which seeds those rows and
returns the widest `Rebuild` their changes ask for: a backend, hybrid leg or
metric the construction did not apply forces the models to be rebuilt, a
realtime evaluation or object-size intervals the evaluation layer. What the
construction already applied (the room, the distance model and diffuse, the
grids — the polar grid laid out with the build's own quantization) seeds to
the same values and asks for nothing, so a boot is not rebuilt twice
(`renderer_build` tests: `the_seed_rebuilds_only_for_what_the_construction_left_out`,
and `a_profile_switch_lands_where_a_boot_on_the_same_config_lands`).

The binaural groups' effect is `Reload`: the HRIR grid, the BRIR set and
the crossover bank are rebuilt by the stage that uses them when it sees the
value change, so the engine has nothing to trigger — the effect is declared
so a client knows the change is not instant. `head_tracking` has none: each
incoming packet is matched against the address as it arrives. Rows whose
pre-registry address takes another shape (one ear of the pair, by index)
declare `LegacyAddr::None` and are reached through the generic setters only.

Kept hand-wired on purpose (docs/persistence-policy.md): the ear mutes and
the manual head pose (transient state), the head-tracking recenter and axis
calibration (written at once, the policy's exception; `persist` still
carries them through an explicit Save and `seed_control_from_render_config`
still seeds them) and the SOFA upload.

### Host options and `Staged` groups

The renderer crate knows nothing of audio devices, so the settings a host
owns are declared by the host, in its own crate: `HostOptionSpec<H>` rows
over its state `H` (`host_audio::options::HOST_OPTIONS`, over `HostAudio`).
A row has the same declaration as a core row (key, kind, default, flags,
group, i18n, legacy alias) and functions reaching the host's state: `set`
(the requested value), `get_json`, an optional `applied_json` (the value in
force, for a `Staged` group) and `config_store`. It has no seed: the host
seeds its state at its own bootstrap (the CLI's argument resolution).

<!-- BEGIN GENERATED host-options -->
| Key | Value | Default | Group (mode, effect) | Flags | Alias |
|---|---|---|---|---|---|
| `output_device` | string | `""` | `audio_output` (live, restart_output) | — | `/control/audio/output_device` |
| `output_backend` | string | `""` | `audio_output` (live, restart_output) | — | `/control/audio/output_backend` |
| `output_file` | string | `""` | `audio_output` (live, restart_output) | — | `/control/audio/output_file` |
| `output_file_format` | string | `""` | `audio_output` (live, restart_output) | — | `/control/audio/output_file_format` |
| `output_sample_rate` | int or null [1, 768000] | unset | `audio_output` (live, restart_output) | — | `/control/audio/sample_rate` |
| `latency_target` | int or null [1, 10000] | unset | `audio_output` (live, restart_output) | — | `/control/latency_target` |
| `enable_adaptive_resampling` | bool | `false` | `adaptive_resampling` (live, none) | — | `/control/adaptive_resampling` |
| `adaptive_resampling_enable_far_mode` | bool | `true` | `adaptive_resampling` (live, none) | — | `/control/adaptive_resampling/enable_far_mode` |
| `adaptive_resampling_force_silence_in_far_mode` | bool | `true` | `adaptive_resampling` (live, none) | — | `/control/adaptive_resampling/force_silence_in_far_mode` |
| `adaptive_resampling_hard_recover_high_in_far_mode` | bool | `true` | `adaptive_resampling` (live, none) | — | `/control/adaptive_resampling/hard_recover_high_in_far_mode` |
| `adaptive_resampling_hard_recover_low_in_far_mode` | bool | `false` | `adaptive_resampling` (live, none) | — | `/control/adaptive_resampling/hard_recover_low_in_far_mode` |
| `adaptive_resampling_far_mode_return_fade_in_ms` | int ≥ 0 | `500` | `adaptive_resampling` (live, none) | — | `/control/adaptive_resampling/far_mode_return_fade_in_ms` |
| `adaptive_resampling_kp_near` | float [0, 1000000], step 0.01 | `1` | `adaptive_resampling` (live, none) | — | `/control/adaptive_resampling/kp_near` |
| `adaptive_resampling_ki` | float [0, 1000000], step 0.01 | `1` | `adaptive_resampling` (live, none) | — | `/control/adaptive_resampling/ki` |
| `adaptive_resampling_integral_discharge_ratio` | float [0, 1], step 0.01 | `0.25` | `adaptive_resampling` (live, none) | — | `/control/adaptive_resampling/integral_discharge_ratio` |
| `adaptive_resampling_max_adjust` | float [0, 1000000], step 0.01 | `0.01` | `adaptive_resampling` (live, none) | — | `/control/adaptive_resampling/max_adjust` |
| `adaptive_resampling_update_interval_callbacks` | int ≥ 1 | `1` | `adaptive_resampling` (live, none) | — | `/control/adaptive_resampling/update_interval_callbacks` |
| `adaptive_resampling_high_recover_entry_margin_ms` | int ≥ 1 | `1000` | `adaptive_resampling` (live, none) | — | `/control/adaptive_resampling/high_recover_entry_margin_ms` |
| `adaptive_resampling_low_recover_settle_stable_ms` | float [0, 1000000], step 0.1 | `200` | `adaptive_resampling` (live, none) | — | — |
| `adaptive_resampling_low_recover_entry_margin_ms` | float [0, 1000000], step 0.1 | `18` | `adaptive_resampling` (live, none) | — | — |
| `adaptive_resampling_low_recover_exit_margin_ms` | float [0, 1000000], step 0.1 | `6` | `adaptive_resampling` (live, none) | — | — |
| `adaptive_resampling_low_recover_settle_margin_ms` | float [0, 1000000], step 0.1 | `6` | `adaptive_resampling` (live, none) | — | — |
| `adaptive_resampling_low_recover_refill_delta_alpha` | float [0, 1], step 0.01 | `0.5` | `adaptive_resampling` (live, none) | — | — |
| `adaptive_resampling_control_smoothing_cutoff_hz` | float [0.001, 1000], step 0.001 | `0.5` | `adaptive_resampling` (live, none) | — | — |
| `adaptive_resampling_control_smoothing_order` | int [1, 2] | `1` | `adaptive_resampling` (live, none) | — | — |
| `adaptive_resampling_use_pre_bridge_clock` | bool | `false` | `adaptive_resampling` (live, none) | — | — |
| `adaptive_resampling_use_output_pacing` | bool | `false` | `adaptive_resampling` (live, none) | — | — |
| `adaptive_resampling_disable_backpressure` | bool | `false` | `adaptive_resampling` (live, none) | — | — |
| `input_mode` | `pipe_bridge` \| `pipewire` | `"pipe_bridge"` | `live_input` (staged, restart_input) | — | `/control/input/mode` |
| `live_input_backend` | string | `""` | `live_input` (staged, restart_input) | — | `/control/input/live/backend` |
| `live_input_node` | string | `""` | `live_input` (staged, restart_input) | — | `/control/input/live/node` |
| `live_input_description` | string | `""` | `live_input` (staged, restart_input) | — | `/control/input/live/description` |
| `live_input_layout` | string | `""` | `live_input` (staged, restart_input) | — | `/control/input/live/layout` |
| `live_input_clock_mode` | `dac` \| `pipewire` \| `upstream` | `"dac"` | `live_input` (staged, restart_input) | — | `/control/input/live/clock_mode` |
| `live_input_channels` | int or null [1, 64] | unset | `live_input` (staged, restart_input) | — | `/control/input/live/channels` |
| `live_input_sample_rate` | int or null [1, 768000] | unset | `live_input` (staged, restart_input) | — | `/control/input/live/sample_rate` |
| `live_input_map` | string | `"7.1-fixed"` | `live_input` (staged, restart_input) | — | `/control/input/live/map` |
| `live_input_lfe_mode` | `object` \| `direct` \| `drop` | `"direct"` | `live_input` (staged, restart_input) | — | `/control/input/live/lfe_mode` |
<!-- END GENERATED host-options -->

The host exposes its rows through `runtime_control::HostControlHandler`'s
option methods — `option_kind`, `apply_options`, `apply_option_group`,
`options_schema`, `options_json`, `options_applied_json`,
`option_groups_pending` — each a one-line call to the `renderer::options`
`host_*` helpers. The core then treats them like its own:
`/control/option(s)` split a message between the core batch and the host
batch (one notification), the schema appends the host's entries, and
`/state/host_options` carries the requested and applied values and the
pending flags. The `/state/renderer` `options` block stays the core's: that
message is also sent by the topology rebuild, which knows no host.

A `Staged` group separates the requested value from the value in force: a
write stages (and lights Save), `/control/options/apply <group>` hands every
staged value over at once, and the snapshot shows both with a pending flag.
Only the live input is staged — the host already kept requested and applied
input state apart. The audio output is declared `Live` with the
`RestartOutput` effect because that is what it does: the host compares
requested and running values on every poll and restarts the output as soon
as they differ; staging it would change what its addresses do.

Scoping by host: a row flagged `EMBEDDED_ONLY` (`decode_thread`) exists only
on a host without audio I/O of its own — the embedded engine, the `embedded`
variant of `/state/capabilities`. The standalone renderer leaves it out of
its schema, refuses a write to it and, on Save, keeps what the file says
(both hosts share the config).

Kept out of the registry, as structured data: the hybrid curve (a point
list; the grouped setter's arity-based parser cannot delimit it) and the
per-backend parameter bag (`/control/backend/param`, keys declared by each
backend's own schema).

The schema entry of a grouped option carries its group:
`"group": {"key", "mode", "effect", "i18nKey"}` — enough for Studio to draw a
generic group header or, for a `Staged` group, an Apply button.

The room keeps its file representation: `config_fields::room` writes metres
against the layout radius (`store_ratio` / `store_rear` / `store_lower`) and
reads ratios back through `room::resolve`, the single reading shared by the
renderer build, the live seed and the profile switch — so the dependent
default (an absent rear follows the length) lives in one place.

### Command-line flags

`orender render` takes every registered option as a flag generated from the
registries (`src/cli/options.rs`): `--<key>` with `-` for `_`, or the pair
`--<key>` / `--no-<key>` for a boolean; an enum lists its values, a float
array takes `n,n,n`, an optional integer takes `none`. Core rows offered on
a host with audio I/O and the standalone host's rows (`HOST_OPTIONS`) are
generated; `master_gain` stays a hand-written flag in decibels (the option's
wire value is linear) and `adaptive_resampling_integral_discharge_ratio` has
no flag. A value given goes into the run's config through its row, as a
save of the same live change writes it (`options::store_client_values`, on
a scratch `LiveParams` seeded from the file; `host_audio::store_host_values`
on a blank `HostIo`): validated and bounded as an OSC write, refused with an
error naming the flag, kept by `--save-config`. A room flag stores the whole
room (its metres are read only as a set).

### Outside the registry: the command tables

Not every control address is an option. The rest are declared in one table
per layer (`runtime_control::command_table`: an address, or several handled
alike, or a prefix, and its handler), instead of a chain of address
comparisons:

| Table | Layer | What it holds |
|---|---|---|
| `LIVE_CONTROL_COMMANDS` | `runtime_control::live_control` | the generic setters and the group apply; the metering and diag cadences; the generator and phantom parameters; the placement |
| `SIMPLE_CONTROL_COMMANDS` | `runtime_control::osc` | layout and speaker patches; the test signals; ear gain and mute; the manual head pose, recenter and calibration; the SOFA upload; backend parameters and the spread aliases; the layout radius; the hybrid curve; object mutes |
| `ENGINE_COMMANDS` | `orender_engine::osc::dispatch` | the mpv overlay; metering, diag and gain-table subscriptions; the realtime gains; the bridge and input paths; the profiles; the backend files; the layout export |
| `HOST_COMMANDS` | `host_audio` | the audio and input JSON patches and group applies; the device refresh; the input layout import; the resampling hold and reset |

The process commands (save, reload, restart, quit, yield, resume, log
level) stay one `match` (`runtime_control::command::PROCESS_COMMANDS`). Each
table is checked by a test: every address is in `osc_contract::ALL_CONTROL`,
none is claimed twice or by another layer, and none is a registry option's
alias (an option is reached through the registry only).

Why these are not options:

- **Commands**: save, apply, refresh, upload, recenter, calibrate, a profile
  switch, a file get/put, an export. They do something; they hold no value.
- **Transient state**: the test signals, speaker / ear / object mutes, the
  manual head pose, the resampling hold. A listening gesture, published and
  never saved (`docs/persistence-policy.md`).
- **Per-client subscriptions**: metering, diag, gain tables. They belong to
  the client that asked, not to the renderer.
- **View state**: the overlay switches and the monitoring cadences. Saved as
  they change, never behind the Save button, which is the only way an option
  reaches the file.
- **Values of another shape**: a gain per speaker (`realtime/speaker_gain`,
  the speaker patch's delay), a mode or a layout per family (placement), a
  point list (the hybrid curve), a dynamic key/value bag (backend, generator
  and phantom parameters), a value on the engine rather than the live params
  (bridge and input paths, layout radius). Each would need an indexed,
  variable-length or engine-side option kind; none is planned.

## Adding a live option today

1. Add ONE row to `declared_options!` in `renderer/src/options/declared.rs`
   (+ Studio i18n keys). It generates the live field, the config key, the
   default and the registry row. (An option inside the binaural, room,
   evaluation or hybrid structures is a hand-written `OptionSpec` in
   `HAND_WIRED_ROWS`, next to the field it reaches.) Regenerate the options
   tables of this RFC and of the OSC contract:
   `UPDATE_DOC_TABLES=1 cargo test -p host_audio doc_tables` (the same test
   fails in CI when they drift).
2. Add the control markup with its `data-option` attribute (a switch, a
   toggle-btn pair or a select — no JS).
3. Done: OSC (generic + schema), the `orender render --<key>` flag,
   persistence, CLI/FFI seeding, replan
   invalidation, the snapshot block, the UI wiring and the CI contract checks
   all derive from the row + the markup. The conformance net fails if a layer
   is missing, and derives its non-default sample from the row (only a
   free-form string needs one listed). Only an option with bespoke UI side
   effects needs code (one entry in the binder's `AFTER_SET`).

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

> Since then the generator and phantom parameters have joined the backends'
> contract instead (`ParamSpec`, one value store, one published format):
> see [the plugin contract](plugin-contract.md). `ObjectGenParamSpec` is gone.

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
