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
  discovery folder that holds at least one **usable** bridge, sorted by file
  name. A candidate is usable when its header passes the version and layout
  check; the refused ones are reported in `bridge_error`, and a folder that
  only holds refused candidates (a leftover 0.5 `libharletty_bridge` next to
  the executable) does not stop the search. It does not merge folders: a
  stale per-user bridge must not be added to the system ones. The folders and
  their order are unchanged.
- **The combined library's path**: a configured path (config, CLI,
  `ORENDER_BRIDGE_FILE`, mpv's `ad-orender-bridge-path`) whose file name is
  the combined harletty library of 0.5 (`libharletty_bridge.so`,
  `harletty_bridge.dll`, `libharletty_bridge.dylib`) stands for the family
  libraries that replace it: the host loads the `harletty_*_bridge` files
  found in the same folder, whether the old file is still there or not, and
  logs the substitution. If that folder holds none, it falls back to
  auto-discovery with a warning. The next Save writes the resolved list.
  This is a transition rule, the only place where the host knows a plugin's
  file name: it is removed in the minor release after the one that ships
  the family libraries.
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
/// Where, if anywhere, a stream this bridge decodes starts in `data`.
/// Stateless: the host calls it before it creates or picks an instance.
/// For `Iec61937`, `data` is the burst payload and `data_type` the burst
/// type, and only `Claim` at 0 or `None` make sense. For `Raw`, `data` is a
/// window of undecided bytes, which may start mid-frame or end inside a
/// header (see Routing).
pub probe: extern "C" fn(data: RSlice<'_, u8>, transport: RInputTransport, data_type: u8) -> RProbe,

/// The `input_codec` names this bridge decodes ("truehd", "eac3", "dts", …),
/// lower case. The host routes a forced codec by this list.
pub input_codecs: extern "C" fn() -> RVec<RString>,
```

```rust
#[repr(C)]
pub struct RProbe {
    pub verdict: RProbeVerdict,
    /// Claim / Pending: the offset of the frame the stream starts with
    /// (not of its sync word). None: every byte before it is ruled out,
    /// the host need not show them to this bridge again.
    pub offset: u32,
    /// Pending only: how many bytes from `offset` the bridge needs before it
    /// can answer again (a header length, then a frame length once the
    /// header gives it). Must exceed what it was shown; the host does not
    /// call this bridge again until that many bytes are buffered.
    pub needed: u32,
}

#[repr(u8)]
pub enum RProbeVerdict {
    /// A validated stream start at `offset`, by the family's own criteria
    /// (table below), within that family's bounded probe length.
    Claim,
    /// A possible start at `offset` whose header is not complete yet.
    Pending,
    /// No possible start at or after `offset` in `data` so far.
    None,
}
```

The answer is still yes or no, not a confidence score; what it adds is
*where* and *not decidable yet*, which the host needs to route a byte stream
the same way whatever its read sizes.

**What a claim validates** is the family's business, but each family states
its criteria and the most bytes it ever needs from a start, so a stream with
a single header is always claimed within a known distance:

| Family | Claim when | At most |
|---|---|---|
| TrueHD | access-unit header, major sync, `major_sync_info` checksum, which follows the optional `extra_channel_meaning` extension (2 × (n + 1) bytes, n on 4 bits): `Pending` asks for the length the extension declares | 64 bytes |
| E-AC-3 / AC-3 | sync word, valid frame size and rate codes, frame CRC | one frame (4 KiB) |
| DTS core | sync word, valid header fields, the next frame's sync at the declared frame size (its header CRC is optional) | one frame + 4 bytes (16 KiB + 4) |
| DTS-HD substream with no core | substream sync word, header size and fields, header CRC (the frame itself can exceed the buffer) | the substream header (4 KiB) |
| IAMF | IA Sequence Header OBU: OBU header type 31, a well-formed LEB128 `obu_size` of at least the syntax it carries, the optional fields its header flags announce (the extension when `obu_extension_flag` is set: its LEB128 size and that many bytes, before the payload), then the `iamf` code and known primary and additional profiles. Nothing after these fields is required: `obu_size` may extend past them (IAMF 1.1 §3.2), and reserved OBUs may follow before the codec config (§3.3). `Pending` asks for the length the extension declares | 15 bytes without extension; with one, 1 + the two LEB128 fields (up to 8 bytes each) + the declared extension + 6, so at most 23 + `extension_header_size` |

IAMF has no CRC and need not repeat its sequence header, so its criteria are
the sequence header's own fields, not a second sync or the OBUs after it: an
OBU type, a 32-bit code and two constrained profile bytes at fixed places
make a chance match negligible, and the claim never waits on a declared size
or on a following OBU.

Each bound above is the format's own: the longest header its declared
lengths allow, computed from the stream (`Pending.needed`), not a fixed
cap. A candidate whose header has been read to its declared end and does not
validate is not that family's stream: the probe answers `None` past it.
The host's undecided buffer (`MAX_UNDECIDED_RAW`) is a separate, resource
limit: a candidate that declares more than the buffer holds (an IAMF
extension of tens of kilobytes) cannot be confirmed by probing and is
abandoned with a warning, which says nothing about the stream itself. Such a
stream still plays when its codec is named (`input_codec`, as a player does).

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
  two pushes, and a reader may hand over one byte at a time. With no route
  (and no forced bridge: an `input_codec` naming one makes it the route at
  once), the host keeps the undecided bytes in a bounded buffer
  (`MAX_UNDECIDED_RAW`, 64 KiB, allocated once at load) and routes by
  **stream start**, not by any occurrence of a sync word:
  - **The earliest start wins.** After each push, the host decides as soon as
    some bridge claims an offset `o` and every other bridge has either
    answered at a later offset or ruled out every byte up to `o` (its scan
    position, below, is past `o`); a `Pending` start at or before `o` holds
    the decision. Between claims at the same offset, load order decides.
    A sync word inside another stream's payload (an IAMF prefix can carry
    reserved OBUs whose bytes are arbitrary) lies after that stream's own
    start, so it loses whether it arrives in the same read or a later one.
    Since a probe only claims a validated header and keeps a start pending
    until it can validate it, the decision does not depend on how the bytes
    were split into reads.
  - **Replay**: the chosen bridge receives the buffered bytes from its
    claimed offset in one push, then the live ones. Bytes before it belong
    to no stream and are dropped, counted in a rate-limited warning.
  - **Bounded work**: each bridge keeps a scan position. A `None` answer
    moves it to the returned offset, so the next probe sees only the new
    bytes plus the bridge's own overlap (at most its longest header). A
    `Pending` answer keeps it at the pending start and says how many bytes it
    `needed`; the host does not call that bridge again until they are
    buffered, so a candidate costs a few calls (header, then frame), each
    presenting at most the family's bound above, not one call per received
    byte. Bytes every bridge has ruled out leave the buffer at once. Each
    received byte is thus presented a bounded number of times per bridge,
    whatever the read size.
  - **Overflow**: only a pending start can hold the buffer. If it reaches
    the bound, that start is abandoned (the bridge's scan position moves past
    it) with a rate-limited warning, an error in strict mode.

  The route then holds until `reset`, as a bridge's own codec lock does today.
- **Resume after a seek**: `reset` (which a seek issues) resets the bridges
  but keeps the last route as the **fallback**, without locking it: until a
  bridge claims a start again, the stream is on probation, as today's
  `resolve_raw_codec` leaves `raw_codec` unset after its fallback. Each push
  is probed **at its first byte only**:
  - a `Claim` at offset 0 locks the route there: the fallback bridge itself
    (TrueHD at its next major sync) or another one (a new stream starting
    on the read that follows the reset, as the next file in a continuous
    pipe does);
  - `None` at 0: the push goes to the fallback bridge at once, and the next
    push is probed again;
  - `Pending` at 0: the push is **held**, with the next ones appended, until
    the candidate is decided (within its family's bound): `Claim` locks the
    route there and the held bytes go to that bridge; `None` sends the held
    bytes to the fallback bridge. Either way every byte is delivered once,
    in order, to one bridge.

  This is what the bridges do today on their own: harletty's router sniffs
  only the start of a packet, sends a sync-less packet after a reset to IAMF
  while its sequence is still configured (temporal units carry no header to
  probe) and otherwise to TrueHD, and keeps sniffing until a sync locks the
  codec. Probing only the first byte keeps a sync-like pattern inside the
  resumed stream's payload from pulling it to another bridge. A resumed IAMF
  stream stays on probation, as it does today; the cost is one probe at the
  first byte of each push, bounded by the families' header checks. The
  fallback is cleared only when the host loads a new set or the input is
  closed; with no fallback (first stream), the undecided-buffer rule above
  applies.
- **IEC 61937**: the bridge for a burst type is found by probing once and
  cached in `iec_route`. When the type moves to a different bridge mid-stream
  (a live input switching from E-AC-3 to DTS), the old bridge is reset, the
  new one becomes active and the push result carries `did_reset`, so the host
  starts a new segment as it does for a bridge-internal reset.
- **A single bridge is not probed**: with one bridge loaded, every packet
  goes to it, as today, whatever `probe` answers. Routing, buffering and
  probation only exist between several bridges, so a host with one bridge
  behaves exactly as before, including with a bridge whose probe only looks
  at offset 0 (harletty's combined bridge until the family plugins ship).
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
- **Existing configs** (no `evaluation_grid` key) are migrated once, when
  the bridges are loaded. Their grid keys may be a deliberate choice or only
  the bridge's defaults that Save pinned: Save writes the sizes whenever
  Cartesian is in force, even with the mode on `auto`. The rule:
  - `render_evaluation_mode` set to a concrete mode (`realtime`,
    `precomputed_polar`, `precomputed_cartesian`) → `custom`, with the stored
    grid, **even if its values equal the bridge's hint**: a mode was chosen,
    so the grid was frozen on purpose;
  - mode absent or `auto`, and sizes absent or equal to the first loaded
    bridge's hint → `bridge`;
  - mode absent or `auto`, and any size different from the hint → `custom`;
  - `vbap_allow_negative_z` present and different from the hint → `custom`,
    whatever the mode and sizes (the engine has always honoured it over the
    bridge's), with that value kept in the custom grid.

  The one case the rule cannot tell apart is a mode left on `auto` with
  sizes set by hand to exactly the bridge's values: it is indistinguishable
  from what Save pins on its own and becomes `bridge`. This is deliberate,
  so that following the bridge is the default for the configs that never
  chose a grid. Otherwise the render and its memory cost do not change. The outcome is logged and marks the config
  dirty, so the user sees it and Save records it; nothing is written before.
  A config written by this build always carries the key, so the migration
  runs only once. Adding a key needs no schema bump.
- **`auto` has no place in `custom`**: entering `custom` (by the switch or by
  the migration) resolves `auto` to the mode in force, and Save writes that
  concrete mode. In `custom` the mode control offers the concrete modes the
  backend supports (`allowed_evaluation_modes`): `realtime`,
  `precomputed_polar`, `precomputed_cartesian`. In `realtime` there is no
  table, so the grid fields are inactive. A forced grid never depends on
  which bridge was active first or at the last restart. In `bridge` the mode
  is the bridge's.
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
  | `harletty_dolby_bridge` | `bridge-family-dolby` | TrueHD major sync, E-AC-3/AC-3 sync | 0x01, 0x15, 0x16 |
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
  `did_reset`; the first bridge wins on a double claim; a single loaded
  bridge receives every packet unprobed; family union;
  disagreeing coordinate formats are refused.
- Fragmented raw input: the opening header of each format split at every
  byte offset, one-byte pushes, and undecided bytes before the first start;
  the chosen bridge receives exactly the bytes from its start, in order. A
  stream whose payload holds another family's sync word (an IAMF prefix with
  a reserved OBU carrying an E-AC-3 frame) routes to its own bridge, fed in
  one block and byte by byte. Fragmentation combined with a seek reset.
  A pending start that reaches the bound is abandoned and probing goes on.
- Probe work budget: 1 MiB of undecidable bytes pushed one byte at a time,
  and the same with plausible candidates that stay `Pending` up to their
  family's bound and are then rejected; the bytes presented to each bridge's
  probe stay within a small constant times the input plus the bounds,
  counted by an instrumented fake bridge.
- Claim criteria: an ordinary IAMF sequence with a single sequence header is
  claimed within 15 bytes, and so are one whose sequence header declares an
  `obu_size` beyond its syntax (64, with 58 ignored bytes) and one where a
  reserved OBU follows it; one with `obu_extension_flag` set and a 16-byte
  extension before the `iamf` code is claimed at 25 bytes, and one with an
  empty extension and both LEB128 fields on 8 bytes at 23; each also
  fragmented at every offset; an extension larger than the host buffer is
  abandoned with the resource warning, not refused as another format; a TrueHD
  major sync with the longest `extra_channel_meaning` extension is claimed at
  64 bytes, fragmented at every offset up to its CRC; each family's real probe on its own corpus
  streams and on the other families' streams (no cross-claim).
- Seek: IAMF auto-detected (no forced codec) → `reset` → temporal units with
  no sequence header are decoded by the IAMF bridge; a TrueHD stream resumes
  on its next major sync; a different format's header after the reset moves
  the route. A → reset → B's opening header split at every offset ends on
  B, with every byte delivered once and in order; A → reset → A's
  continuation with a `Pending` false start at a push boundary goes back to
  A without loss.
- DRC: legacy aliases (`Standard`, `Line`, `Heavy`, `RF`) from an old config
  reach the Dolby bridge and take effect; a mode no bridge accepts warns.
- Combined-library path: a config pointing at `libharletty_bridge.so`, with
  and without the old file present, loads the family libraries beside it;
  a folder without them falls back to discovery, and discovery passes over
  a priority folder that still holds the old file alone; any other missing
  path still fails.
- Evaluation grid: in `bridge`, a stream switch to a bridge with other hints
  triggers one evaluation-only rebuild and none with the same hints; grid
  edits are refused; in `custom`, no rebuild on a switch and edits apply.
  During a rebuild: `bridge → custom` while B's grid is building keeps A
  installed and discards B; A → B → A ends on A. Migration: an old config
  with `auto` and default-equal sizes becomes `bridge`; one with
  `precomputed_polar`, `realtime`, a reduced grid, or `precomputed_cartesian`
  with sizes equal to bridge A's becomes `custom` and renders as before, also
  when a stream from bridge B with other hints follows; `auto` with an
  explicit `vbap_allow_negative_z` (`true` and `false`) becomes `custom` when
  it differs from the hint and `bridge` when it equals it; its first Save writes
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
2. **Probe**: no confidence score. It answers where a validated stream
   start is, or that one is pending, or that there is none so far; the host
   routes to the earliest start, so a sync word inside another stream's
   payload never wins, whatever the read sizes.
3. **Evaluation grid**: follows the active bridge by default and is rebuilt
   when a stream brings other hints; a `custom` setting forces it (see
   "Evaluation grid").
