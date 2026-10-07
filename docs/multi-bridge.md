# RFC: several bridges in one host, one bridge per codec family

Status: **proposal**. Nothing here is built.

Paths are under `omniphony-renderer/` unless stated.

## Problem

A host loads exactly one bridge library. `render.bridge_path` names one file,
auto-discovery stops at the first `*_bridge.{so,dll,dylib}` it finds
(`orender_engine/src/bridge_loader.rs`, `find_bridge_in_dirs`), and everything
downstream holds one `FormatBridgeBox`.

So a bridge has to decode every format a user may play, and harletty ships a
single library with every codec family in it. harletty-bridge already builds
each family as its own crate (`bridge-family-dolby`, `-dts`, `-iamf`) and can
compile any subset into its cdylib. What it cannot do is ship them as separate
plugins: the host would only ever see one of them.

There is also a latent bug in the loader. `BridgeLibRef::load_from_file` goes
through abi_stable's `RootModule::load_from`, which caches the first root
module it loads **for the whole process** (`declare_root_module_statics!`). A
later load of a different path returns that first module. Two paths hit it
today:

- the CLI restarting from a reloaded config whose `bridge_path` changed
  (`src/cli/decode/session_run.rs`, `reload_config`);
- mpv creating a second engine for another track with a different bridge in
  the same process.

`check_bridge_api_version` reads the header of the *new* file, so the check
passes and the old bridge silently keeps decoding.

## Proposal

1. **The host loads every configured bridge** and routes each stream to one of
   them. A `BridgeSet` in `orender_engine` owns the bridges and answers the
   rest of the host exactly as a single bridge does today, so `Engine`,
   `FramePipeline`, the CLI decoder thread, the live S/PDIF thread and
   `sync-play` change only in what they hold.
2. **Each bridge says which packets it decodes** through a new `probe` entry
   in its root module, and which `input_codec` names it answers to. Detection
   stays in the plugins: the host knows no sync word.
3. **harletty ships one bridge per codec family**: `harletty_dolby_bridge`,
   `harletty_dts_bridge`, `harletty_iamf_bridge`. The combined
   `harletty_bridge` library is no longer released.

### Loading

- Each library is opened with `lib_header_from_path(path)?.init_root_module()`,
  which checks the version and the layout exactly as `load_from` does but
  bypasses abi_stable's process-wide cache. The loader keeps its own map from
  canonical path to root module, so loading the same file twice reuses it and
  loading a different file really loads it. This fix lands first, on its own,
  under the current `bridge_api` 0.5.
- **Config**: a new `render.bridge_paths: [path, …]`. An existing
  `render.bridge_path` is read as a one-element list. Save writes
  `bridge_paths` and drops `bridge_path`, which moves a key, so
  `CONFIG_SCHEMA_VERSION` goes to 2: an older build refuses to save over the
  file rather than losing the list. The list changes what the engine
  decodes, so it is saved only through Save, as `bridge_path` is today
  (`docs/persistence-policy.md`).
- **CLI**: `--bridge-path` becomes repeatable.
  `ORENDER_BRIDGE_FILE` accepts a list in the platform's path-list syntax
  (`:` or `;`).
- **Auto-discovery** (no paths configured) loads **every** bridge in the first
  discovery folder that holds at least one, sorted by file name. It does not
  merge folders: a stale per-user bridge must not be added to the system ones.
  The folders and their order are unchanged.
- **The combined library's path**: a configured path (config, CLI,
  `ORENDER_BRIDGE_FILE`, mpv's `ad-orender-bridge-path`) whose file name is
  the combined harletty library of 0.5 (`libharletty_bridge.so`,
  `harletty_bridge.dll`, `libharletty_bridge.dylib`) stands for the family
  libraries that replace it: the host loads the `harletty_*_bridge` files
  found in the same folder, whether the old file is still there or not, and
  logs the substitution. If that folder holds none, it falls back to
  auto-discovery with a warning. The next Save writes the resolved list.
  Every other path keeps the strict rule: it must exist and load.
- **Partial failure**: a bridge that fails to load (missing file, ABI
  mismatch) is reported in `bridge_error` and skipped; the host runs if at
  least one bridge loaded. A leftover `libharletty_bridge.so` from 0.5 is
  therefore refused by name and ignored, not fatal.
- **The same family twice**: when two loaded bridges both accept a packet, the
  one earlier in the list wins. This is the documented way to override one
  family with another bridge.

### ABI: `bridge_api` 0.6

The root module gains two required entries (a layout change, so a minor bump
under the policy in `BRIDGE_API.md`; `abi-baseline.txt` is regenerated):

```rust
/// Whether this bridge decodes the stream `data` belongs to. Stateless: the
/// host calls it before it creates or picks an instance. For `Raw`, `data` is
/// every undecided byte the host holds, which may start mid-frame or end
/// inside a header: the probe looks for a sync anywhere in it and answers
/// `false` until one is complete. For `Iec61937`, `data` is the burst payload
/// and `data_type` the burst type.
pub probe: extern "C" fn(data: RSlice<'_, u8>, transport: RInputTransport, data_type: u8) -> bool,

/// The `input_codec` names this bridge decodes ("truehd", "eac3", "dts", …),
/// lower case. The host routes a forced codec by this list.
pub input_codecs: extern "C" fn() -> RVec<RString>,
```

`input_codec`, which the host already sends from mpv's codec name, becomes a
documented configure key in `BRIDGE_API.md`, with its accepted values.

`RVbapCartesianDefaults` gains `z_neg_size`, so a bridge's grid hint is
complete (today the host fills in 0 below the floor). harletty keeps 0, its
current effective value, so output does not change. The reference bridge's
`allow_negative_z` and the doc comment on `BALANCED`, which claims it matches
harletty's (it does not: harletty allows negative z), are reconciled.

`FormatBridge` itself does not change otherwise. Every bridge, the reference bridge
included, must be rebuilt for 0.6, which the policy already requires for any
minor bump.

### Routing: `BridgeSet`

```rust
pub struct BridgeSet {
    bridges: Vec<Slot>,          // load order; Slot = root module + instance + its declaration
    active: Option<u8>,          // the bridge decoding the current stream
    forced_codec: Option<u8>,    // the bridge named by input_codec, kept across resets
    iec_route: [u8; 32],         // IEC data_type → bridge index, NONE when unknown
}
```

- **Raw transport**: raw input is a byte stream, not a packet stream: the
  CLI forwards each read as it comes, so a sync word or header can straddle
  two pushes, and a reader may hand over one byte at a time. With no route,
  the host therefore keeps the undecided bytes in a bounded buffer
  (`MAX_UNDECIDED_RAW`, 64 KiB, allocated once at load) and probes the whole
  buffer after each push, in load order:
  - the forced bridge, if `input_codec` named one, is the route at once;
  - the first bridge whose `probe` accepts becomes the route, and receives
    the buffered bytes in one push before the live ones (a replay: nothing it
    would have seen is lost);
  - if the buffer fills with no taker, its older half is dropped with a
    rate-limited warning (an error in strict mode) and probing goes on.

  The route then holds until `reset`, as a bridge's own codec lock does today.
  The probe cost is bounded by the buffer and paid only while undecided.
- **Resume after a seek**: `reset` (which a seek issues) resets the bridges
  but keeps the last route as the **fallback**. The next push is probed as
  above; if no bridge accepts it, it goes to the fallback bridge instead of
  waiting. This is what the bridges do today on their own: harletty's router
  sends a sync-less packet after a reset to IAMF while its sequence is still
  configured (temporal units carry no header to probe) and otherwise to
  TrueHD, which resynchronises on its next major sync. A probe hit from
  another bridge moves the route (a real stream change), with the same
  precedence as today's sniff-before-continuation order. The fallback is
  cleared only when the host loads a new set or the input is closed.
- **IEC 61937**: the bridge for a burst type is found by probing once and
  cached in `iec_route`. When the type moves to a different bridge mid-stream
  (a live input switching from E-AC-3 to DTS), the old bridge is reset, the
  new one becomes active and the push result carries `did_reset`, so the host
  starts a new segment as it does for a bridge-internal reset.
- **One instance per bridge**, created at load and kept. Only the active one
  receives packets; the hot path adds one index lookup per packet, and the
  probe runs only while the route is undecided or on a burst-type change.
- **Per-stream answers** (`is_ready`, `has_objects`, `source_family`,
  `source_label`, `fixed_channel_poses`, `channel_tags`) come from the active
  bridge, and from the first bridge while idle.
- **Global answers**:
  - `source_families`: the union in load order, a family declared twice kept
    once (first wins);
  - `supported_drc_modes`: the union, for display only. It is not the list
    of values a bridge accepts: harletty also takes `Standard`, `Line`,
    `Heavy` and `RF`, which configs may hold. `set_drc_mode` is sent to
    every bridge, only when the setting changes (and once at load), and each
    bridge decides from the value; the mode is in force if at least one
    bridge accepts it, and the host warns only when none does, as today;
  - `configure("log_level")` and `configure("presentation")` go to every
    bridge (each bridge library has its own log state); a family without
    presentations already accepts the default as a no-op;
  - `coordinate_format` is read before the renderer is built; the loaded
    bridges must agree, and the host refuses to start a set that does not
    (every bridge today answers Cartesian).
- **Grid hints** (`vbap_cartesian_defaults`, `preferred_vbap_table_mode`) are
  per stream: they are read from the active bridge with its declaration, and
  the evaluation grid follows them unless the user forces one (next section).
- **Instances for a second consumer** (the PipeWire live sink opens its own
  instance today): `BridgeSet::instantiate` opens a fresh instance of every
  bridge from the already-loaded root modules.

### Evaluation grid: follow the bridge, or forced

The VBAP gain table is sampled on a grid whose mode and size come from the
bridge's hints unless the user set them. With several bridges the hints can
differ from one stream to the next, so where the grid comes from becomes an
explicit setting:

- New option `render.evaluation_grid: bridge | custom`, **default `bridge`**
  (also when the key is absent). A registry option: it changes the render,
  so only Save writes it.
- **`bridge`**: the grid is the active bridge's hint, all of it: evaluation
  mode, Cartesian x / y / z+ / z− sizes, negative z. When a new stream's
  declaration carries hints that differ from the grid in force, the host
  starts an evaluation-only rebuild, the one a grid edit triggers today
  (`osc/recompute.rs`, band worker at idle priority): the installed table
  keeps rendering until the new one is swapped in, so a codec switch never
  blocks or drops audio; the first frames of the new stream may render on
  the previous grid. Same hints, no rebuild. In this mode the grid cannot be
  edited: the grid controls answer `/state/control_error` ("the grid follows
  the bridge"), the CLI grid flags are refused with the same message, and
  Studio shows the values read-only, with the bridge they come from.
- **Which request wins**: a rebuild carries a **grid request** (grid source,
  mode, sizes, negative z) with a generation number. The host records the
  latest request; `osc/recompute.rs` checks it before publishing a topology,
  and the band worker checks it again before installing a table. A result
  whose request is no longer the latest is discarded, whatever its topology
  identity. So a switch to `custom` while bridge B's grid is still being
  built keeps table A in force and drops B's; A → B → A during B's build
  ends on A, with no rebuild if A is still installed.
- **`custom`**: the grid is the user's, fixed whatever the stream: no rebuild
  on a codec switch. Switching from `bridge` to `custom` starts from the grid
  in force, that is the installed table, not a pending one, so nothing moves
  until the user edits it. `vbap_allow_negative_z`
  becomes a registry option in this mode (live, Save), so a forced grid is
  complete; today it can only be set from the config file or the CLI.
- **Existing configs** (no `evaluation_grid` key) are migrated
  conservatively, once, when the bridges are loaded. Their grid keys may be a
  deliberate choice (`precomputed_polar`, a coarser grid to save memory) or
  only the bridge's defaults that Save pinned (it writes the sizes whenever
  Cartesian is in force). The host compares them with the first loaded
  bridge's hint: every key absent, `auto`, or equal to the hint → `bridge`;
  any other value → `custom`, with the stored grid, so the render and its
  memory cost do not change. The outcome is logged and marks the config
  dirty, so the user sees it and Save records it; nothing is written before.
  A config written by this build always carries the key, so the migration
  runs only once. Adding a key needs no schema bump.
- **`auto` has no place in `custom`**: entering `custom` (by the switch or by
  the migration) resolves `auto` to the mode in force, and Save writes that
  concrete mode. In `custom` the mode control offers polar and Cartesian
  only, so a forced grid never depends on which bridge was active first or
  at the last restart. In `bridge` the mode is the bridge's.
- **State**: `/omniphony/state/renderer` gains `evaluationGrid` (`bridge` or
  `custom`) and `evaluationGridBridge` (the hint of the active bridge), next
  to the effective values it already publishes. While at it,
  `/state/vbap/allow_negative_z` stops defaulting to `true` when the rebuild
  parameters are unset; the engine itself defaults to `false`.

### OSC and Studio

- New state `/omniphony/state/render/bridges`: one entry per configured or
  discovered bridge (path, `bridge_api`, families, error). The existing
  `bridge_path` state stays, holding the first entry, for clients that only
  know it. `CONTRACT_REVISION` is bumped.
- New control `/omniphony/control/render/bridge_paths` (the full list);
  `render/bridge_path` sets a one-element list.
- Native Studio, Renderer panel: a switch "Follow the bridge" above the grid
  fields; on, the fields are read-only and show the bridge's values.
- Native Studio (`omniphony-studio-egui`): the Input panel lists the loaded
  bridges with their families and status, and edits the list (add with the
  file picker, remove, reorder). The panel only draws; the list lives in the
  core, per `ARCHITECTURE.md`. The Tauri Studio, which is being retired, keeps
  its single field, mapped to the first entry.

### harletty-bridge

- `bridge-common` gets the `FamilyPipeline` trait the step-1 plan sketched,
  and a generic `PluginBridge<F: FamilyPipeline>` implementing `FormatBridge`
  around one family. The plugin-level logic the router holds today moves into
  it once: the panic guard, reset on `AfterPush`, strict handling of an
  unsupported burst type, `log_level`, `input_codec` and `presentation`.
- Three thin cdylib crates, each exporting a root module with its family's
  `probe`, `input_codecs` and `source_families`:

  | Library | Family crate | Raw probe | IEC burst types |
  |---|---|---|---|
  | `harletty_dolby_bridge` | `bridge-family-dolby` | TrueHD major sync, E-AC-3/AC-3 sync | 0x15, 0x16 |
  | `harletty_dts_bridge` | `bridge-family-dts` | DTS core / substream sync | 0x0B–0x0D, 0x11 |
  | `harletty_iamf_bridge` | `bridge-family-iamf` | IAMF sequence header | none |

- The combined router in `bridge/` stops being a cdylib. It stays an rlib for
  the fuzz target, the bench and the bit-exactness kit while those move to
  the per-family plugins; removing it is a follow-up.
- `release.yml` packages the three libraries in each platform archive. The
  IAMF library links libopus, which the Linux build takes from the system;
  Windows and macOS build it from source and link it statically.
- `check-crate-isolation.sh` checks each plugin's crate graph holds only its
  own family's decoders.

### Compatibility

- A 0.6 host refuses a 0.5 bridge, and the reverse, by name, as today.
- Configs with `render.bridge_path` keep working, including one that names
  the combined library (see Loading); the first Save rewrites them as
  `bridge_paths`, and a pre-0.6 build then refuses to save over that file
  (schema version 2).
- Packaging and install: the AUR `harletty-bridge` package, the installers
  and `scripts/wfbuild.sh` install three libraries and remove the old
  `libharletty_bridge`. The install pages list the three.
- mpv needs no code change: it hands its `ad-orender-bridge-path` string (or
  none) to liborender, which splits an explicit path as a path list, like
  `ORENDER_BRIDGE_FILE`. Its error text that names `render.bridge_path` is
  updated with the docs.

### Tests

- Loader: two different bridge files in one process return two different
  root modules (the regression test for the cache bug); the same file twice
  returns one.
- `BridgeSet` with in-process fake bridges (`BridgeLib{..}.leak_into_prefix()`,
  as `decode_queue.rs` does): routing by probe, by `input_codec`, by IEC burst
  type; a mid-stream burst-type switch resets the old bridge and reports
  `did_reset`; the first bridge wins on a double claim; family union;
  disagreeing coordinate formats are refused.
- Fragmented raw input: the opening header of each format split at every
  byte offset, one-byte pushes, and undecided bytes before the first sync;
  the chosen bridge receives exactly the input bytes, in order. A full
  undecided buffer drops its older half and keeps probing.
- Seek: IAMF auto-detected (no forced codec) → `reset` → temporal units with
  no sequence header are decoded by the IAMF bridge; a TrueHD stream resumes
  on its next major sync; a different format's header after the reset moves
  the route.
- DRC: legacy aliases (`Standard`, `Line`, `Heavy`, `RF`) from an old config
  reach the Dolby bridge and take effect; a mode no bridge accepts warns.
- Combined-library path: a config pointing at `libharletty_bridge.so`, with
  and without the old file present, loads the family libraries beside it;
  a folder without them falls back to discovery; any other missing path
  still fails.
- Evaluation grid: in `bridge`, a stream switch to a bridge with other hints
  triggers one evaluation-only rebuild and none with the same hints; grid
  edits are refused; in `custom`, no rebuild on a switch and edits apply.
  During a rebuild: `bridge → custom` while B's grid is building keeps A
  installed and discards B; A → B → A ends on A. Migration: an old config
  with default-equal keys becomes `bridge`, one with `precomputed_polar` or a
  reduced grid becomes `custom` and renders as before; its first Save writes
  the key and a concrete mode. Save then restart with another bridge active
  first gives the same grid in `custom`.
- Discovery: all bridges of the first non-empty folder, none from later
  folders; a refused bridge is reported and skipped.
- Config: `bridge_path` read as a list, `bridge_paths` round-trips, schema
  version 2 refused by an older build's rules.
- harletty: each plugin's tests run against its own crate alone; the IAMF
  plugin's crate graph holds no other decoder.

### Verification

- **Bit-exactness**: the baseline corpus of the step-1 split (one or more
  streams per family, raw and IEC 61937), located through
  `HARLETTY_BASELINE_CORPUS` (a manifest of stream files and expected
  hashes; not part of the repository), decoded through the host with the
  three family bridges, against the combined bridge on `main`. Fragmented
  replays of the same streams (fixed small reads) must give the same hashes.
  Any difference is a bug.
- **Hot path**: `bridge_bench` per family through `BridgeSet`; no regression
  beyond the kit's noise floor.
- **Listening**: mpv on one stream per family, and the live S/PDIF input
  switching codec mid-stream.

## Plan

Each step is one PR, merged before the next is built on it.

| # | Repo | Change |
|---|---|---|
| 1 | Omniphony | Loader: load by path without abi_stable's process-wide cache; regression test. `bridge_api` stays 0.5. |
| 2 | Omniphony | `bridge_api` 0.6: `probe`, `input_codecs`; reference bridge; ABI baseline; `BRIDGE_API.md` (incl. `input_codec`). |
| 3 | Omniphony | `BridgeSet` in `orender_engine` (undecided-byte buffer, fallback route after a seek, DRC forwarding); Engine, CLI, live sink and `sync-play` hold it. |
| 4 | Omniphony | `render.bridge_paths`, discovery of every bridge, combined-library path substitution, repeatable flag, env list, OSC state and control, schema version 2, docs. |
| 4b | Omniphony | `render.evaluation_grid` (`bridge` / `custom`): grid requests with a generation, rebuild on a hint change, conservative migration, edits refused in `bridge`, negative z as an option, state, Studio switch. |
| 5 | harletty-bridge | `FamilyPipeline` and `PluginBridge` in `bridge-common`; the combined router built on them; output unchanged. |
| 6 | harletty-bridge | Three plugin crates on `bridge_api` 0.6; bit-exactness through the host. |
| 7 | harletty-bridge | Release, build scripts, CI matrix, isolation check per plugin; the combined cdylib leaves the release. |
| 8 | Omniphony | Studio egui bridge list. |
| 9 | Omniphony | Packaging, installers, install pages, `wfbuild.sh`. |

Steps 5 and 8 depend on nothing but their predecessors in the same repo and
can run alongside 2–4.

## Decisions

1. **libopus on Windows and macOS**: built from source in `release.yml` and
   linked statically into the IAMF plugin, which keeps one file per plugin.
2. **Probe**: a boolean. The sync words of the formats we have do not
   collide; a confidence score waits until a format needs one. Incomplete
   headers are handled by the host's undecided-byte buffer, not by a third
   answer.
3. **Evaluation grid**: follows the active bridge by default and is rebuilt
   when a stream brings other hints; a `custom` setting forces it (see
   "Evaluation grid").
