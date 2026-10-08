# Bridge API

This document describes the runtime plugin ABI used by `omniphony-renderer` to load external
decoder bridges.

The ABI is defined in:
- [bridge_api/src/lib.rs](bridge_api/src/lib.rs)
- [orender_engine/src/bridge_loader.rs](orender_engine/src/bridge_loader.rs)

`omniphony-renderer` does not decode immersive formats directly. A bridge plugin owns the
format-specific parsing, decode pipeline, and spatial metadata extraction.

## Versioning

**A bridge loads only in a host built against the same `bridge_api` minor
version.** Rebuild the bridge with the host: a bridge built against
`bridge_api` 0.6.x loads in every host built against 0.6.x, and in no other.

- **What bumps the minor.** Any change to what crosses the boundary: a
  `FormatBridge` method, a `BridgeLib` field, a field, a variant or a
  discriminant of a type the two sides exchange. Adding a method with a
  default body is a change too: the vtable grows. The bump goes in
  `bridge_api/Cargo.toml` and in the workspace dependency.
- **What a patch release may change.** Documentation, constants, plain Rust
  helpers: anything that leaves the layout alone.
- **The check.** `bridge_api/tests/abi_baseline.rs` writes the layout a
  bridge sees — from the root module down to every type it reaches, plus the
  host log callback `set_host_log_sink` receives as a `usize` — with each
  type's size and alignment (as on 64-bit targets, where the test runs), and
  compares it with the committed `bridge_api/abi-baseline.txt`. A layout
  change without a minor bump fails it, whatever the environment says; with
  the bump, regenerate the baseline and commit it with the change:

  ```sh
  UPDATE_BRIDGE_ABI_BASELINE=1 cargo test -p bridge_api --test abi_baseline
  ```

- **At load.** The host reads the version a plugin declares in its header
  before anything else, and refuses another minor with an error naming both
  versions. (abi_stable refuses it anyway, since it compares each type's
  package minor, but only after a layout comparison whose error — "too many
  fields" — says nothing a user can act on.)
- **In CI.** The reference bridge built at the last release tag is loaded
  into the current host, which must load it when both share a minor and
  refuse it by version otherwise (`orender_engine/tests/previous_release_bridge.rs`).

Why this policy rather than a C vtable with optional, probed slots (the way
`liborender`'s ABI keeps older players working): a bridge is rebuilt and
released together with the host it targets, while players outlive several
engine releases. The cost is that a host whose minor changed needs a new
bridge; releases name the `bridge_api` version they expect.

## Panics

**Nothing may unwind out of a bridge.** A panic that reaches the ABI boundary
ends the process: abi_stable's method shims call `exit(1)` on it, and an
`extern "C" fn` aborts. When the engine runs as `liborender`, that process is
the media player.

- Catch panics in every method that does real work — `push_packet` above
  all, and `configure`, `reset`, `set_drc_mode` when they parse or rebuild
  anything — with `std::panic::catch_unwind`, and turn a caught one into the
  method's failure value. For `push_packet` that is the same as any chunk the
  bridge could not decode: reset the pipeline and set `did_reset` (and, for a
  strict bridge, `error_message`).
- The root-module entry points (`new_bridge`, `set_host_log_sink`,
  `source_families`, `probe`, `input_codecs`) are `extern "C"`: keep them free of work that can fail.
  `new_bridge` constructs; fallible setup belongs in `configure` or the
  first `push_packet`.
- `reference_bridge` shows the pattern (`WavBridge::recover_from_panic`).

## Loading Model

A host loads one or several bridges and routes each stream to the one that
decodes it ("Probing"; [`docs/multi-bridge.md`](../docs/multi-bridge.md)).

Bridge lookup order:
1. `--bridge-path <FILE>`, repeatable (or the C config's `bridge_path`, a path
   list in the platform's syntax: `:` on Unix, `;` on Windows)
2. `render.bridge_paths` in the config file, else its single
   `render.bridge_path` (a Save writes the latter when there is one bridge, so
   older builds still read it)
3. else auto-discovery:
   1. `$ORENDER_BRIDGE_FILE`, a path list: the files it names that exist
      (Studio sets it, see below). Names of no file are logged and skipped;
      when none exists, the folders are searched;
   2. else every `*_bridge.so`, `*_bridge.dll` or `*_bridge.dylib`
      (in name order) of the first of these folders that holds a usable one
      (one whose header passes the version and layout check; the others are
      reported and do not stop the search). Folders are not merged:
      1. the folder of the host executable (`orender`, or the player that loads
         liborender, e.g. mpv-omniphony);
      2. `$ORENDER_BRIDGE_DIR`;
      3. the per-user engine folder, where Studio deploys liborender and
         mpv-omniphony's loader looks for it: `$XDG_DATA_HOME/omniphony/lib`
         (default `~/.local/share/omniphony/lib`) on Linux,
         `~/Library/Application Support/omniphony/lib` on macOS,
         `%LOCALAPPDATA%\omniphony\lib` on Windows;
      4. the system plugin folder, `/usr/lib/orender` on Unix (where the AUR's
         `harletty-bridge` installs it; packagers override it with
         `ORENDER_BRIDGE_DIR` at build time). None on Windows.

A path named in 1 or 2 must exist: it is never replaced by a discovered one.
One that does not, or a bridge that does not load, is skipped and reported
(`/omniphony/state/render/bridges`) while the others load; the host fails only
when none does. One exception, for one release: a named
`libharletty_bridge.so` / `harletty_bridge.dll` / `libharletty_bridge.dylib`,
the combined library of `bridge_api` 0.5, stands for the `harletty_*_bridge`
family libraries in the same folder (auto-discovery when there are none), and
the next Save writes them instead.

When nothing is named and nothing is found, `orender` still starts, without a
decoder: PCM and channel input work, and the published
`/omniphony/state/render/bridge_error` contains `no decoder bridge found`,
which Studio shows as a warning rather than an error.

Studio also uses the bridge path from `mpv.conf`: before it spawns its own
`orender`, it reads mpv-omniphony's `ad-orender-bridge-path` from the player's
config (mpv's own lookup: `$MPV_HOME`, else `$XDG_CONFIG_HOME/mpv` or
`~/.config/mpv`, `~/.mpv`, `/etc/mpv`; `%APPDATA%\mpv` on Windows; default
profile only) and, when it names an existing file, passes that exact file as
`$ORENDER_BRIDGE_FILE` (step 3.1). Files, not their folder: the folder scan
loads every bridge of the folder, which need not be the ones named. A bridge named in the engine's own config (`render.bridge_path`)
or on its command line still comes first, and when Studio's own environment
already sets `ORENDER_BRIDGE_FILE` or `ORENDER_BRIDGE_DIR`, the renderer
inherits that and `mpv.conf` is not read. A bridge that only sits next to the
player, with no `mpv.conf` line naming it, stays unknown to Studio.

## Exported Root Module

Each plugin must export the `format_bridge` root module expected by
`abi_stable`:

```rust
#[repr(C)]
#[derive(StableAbi)]
#[sabi(kind(Prefix(prefix_ref = BridgeLibRef)))]
pub struct BridgeLib {
    pub new_bridge: extern "C" fn(strict: bool) -> FormatBridgeBox,
    pub set_host_log_sink: extern "C" fn(usize),
    pub source_families: extern "C" fn() -> RVec<RSourceFamily>,
    pub probe: extern "C" fn(data: RSlice<'_, u8>, transport: RInputTransport, data_type: u8) -> RProbe,
    #[sabi(last_prefix_field)]
    pub input_codecs: extern "C" fn() -> RVec<RString>,
}
```

Every field is required (since 0.6).

`source_families` is the plugin's catalogue of source families: every name
`FormatBridge::source_family` can return, with a label for user interfaces
and a default placement mode (`room` or `sphere`). The renderer knows no
format by name — its own families are only `generic` and `pcm` — so a
family the catalogue does not list renders as `generic` and cannot be set
apart in Studio. The host reads it once at load (see
[`docs/placement.md`](../docs/placement.md)). A plugin with nothing of its
own to declare returns an empty list.

Fixed names:
- `BASE_NAME = "format_bridge"`
- `NAME = "format_bridge"`

## Probing

`probe` and `input_codecs` let a host that holds several bridges route each
stream to the one that decodes it ([`docs/multi-bridge.md`](../docs/multi-bridge.md)).
A host with one bridge may ignore them.

`input_codecs` lists the codec names the bridge decodes, lower case
(`truehd`, `eac3`, `dts`, `iamf`, …). A host that was told the codec (a
player knows it) routes by this list and sends the name as `input_codec`
(see "Configuration Keys"); every listed name must be one `input_codec`
accepts. A bridge that is only reached by probing (the reference bridge:
nothing names WAV as a codec) returns an empty list.

`probe(data, transport, data_type)` says where, if anywhere, a stream the
bridge decodes starts in `data`. It is stateless and cheap: the host calls it
before it creates or picks an instance, and only while a stream's route is
undecided. The answer is an `RProbe { verdict, offset, needed }`:

| `verdict` | Meaning | `offset` | `needed` |
|---|---|---|---|
| `Claim` | a stream of this bridge starts here, validated | where its first frame starts (not its sync word) | 0 |
| `Pending` | a stream may start here, header incomplete | where it would start | bytes from `offset` needed to answer again, more than shown |
| `None` | nothing of this bridge before `offset` | every byte before it is ruled out | 0 |

- **IEC 61937**: `data` is a burst payload and `data_type` its burst type.
  Answer `Claim` at 0 for a burst type the bridge decodes, `None` otherwise.
- **Raw**: `data` is a window of undecided bytes, which may start mid-frame
  or end inside a header, and `data_type` is 0. A `Claim` must rest on the
  format's own validation (a header checksum, the next frame's sync at the
  declared frame size, a well-formed container header), reached within a
  bounded number of bytes from the start; a candidate that reaches that bound
  without validating is not this bridge's stream. Answer for the **earliest**
  start in `data`: the host routes to the earliest claimed start among its
  bridges.
- A probe must not be the only check: `push_packet` still validates what it
  decodes.

## Bridge Lifecycle

Host lifecycle:
1. load the shared library
2. resolve `new_bridge`
3. create a bridge instance (`new_bridge(false)`: see "Strict vs Non-Strict")
4. call `configure(...)` as needed
5. query capability/hints
6. feed input via `push_packet(...)`
7. call `reset()` on seek/discontinuity/end-of-stream reset

Important expectations:
- `configure(...)` happens before the first `push_packet(...)`, except
  `log_level`, which the host sends again between packets when its own level
  changes (see "Configuration Keys")
- `has_objects()` is meaningful after configuration
- `coordinate_format()` should stay stable for the instance lifetime

## Main Trait

```rust
pub trait FormatBridge: Send + Sync + 'static {
    fn push_packet(
        &mut self,
        data: RSlice<'_, u8>,
        transport: RInputTransport,
        data_type: u8,
    ) -> RPushResult;

    fn reset(&mut self);
    fn is_ready(&self) -> bool;
    fn has_objects(&self) -> bool;
    fn configure(&mut self, key: RStr<'_>, value: RStr<'_>) -> bool;
    fn coordinate_format(&self) -> RCoordinateFormat;
    fn vbap_cartesian_defaults(&self) -> RVbapCartesianDefaults;
    fn preferred_vbap_table_mode(&self) -> RVbapTableMode;
    fn supported_drc_modes(&self) -> RVec<RString>;
    fn set_drc_mode(&mut self, mode: RStr<'_>) -> bool;
    fn fixed_channel_poses(&self) -> RVec<RChannelPose>;

    // With a default body: a bridge that does not implement them still builds.
    fn source_family(&self) -> RString;
    fn source_label(&self) -> RString;
    fn channel_tags(&self) -> RVec<RChannelTag>;
}
```

`bridge_api/src/lib.rs` documents each method; this file covers the contract
around them.

## Input Contract

`push_packet(...)` receives one payload plus transport metadata.

Supported transports:
- `RInputTransport::Raw`
  - raw bytestream input
  - `data_type` must be `0`
- `RInputTransport::Iec61937`
  - extracted IEC 61937 payload
  - `data_type` is the IEC 61937 type byte

The bridge validates whether it supports the provided payload.

## Result Contract

`push_packet(...)` returns:

```rust
pub struct RPushResult {
    pub frames: RVec<RDecodedFrame>,
    pub error_message: RString,
    pub did_reset: bool,
}
```

Semantics:
- `frames`
  - zero or more fully decoded PCM frames
- `error_message`
  - non-empty when the bridge could not decode the chunk and did not recover
  - the engine fails the call on it; the live PipeWire input only logs it
- `did_reset`
  - the bridge internally reset its pipeline during recovery

## Decoded PCM Frame

```rust
pub struct RDecodedFrame {
    pub sampling_frequency: u32,
    pub sample_count: u32,
    pub channel_count: u32,
    pub pcm: RVec<i32>,
    pub channel_labels: RVec<RChannelLabel>,
    pub metadata: RVec<RMetadataFrame>,
    pub drc_gain: f32,
    pub drc_ramp_duration: u32,
    pub dialogue_level: ROption<i8>,
    pub is_new_segment: bool,
}
```

PCM rules:
- **24-bit samples, sign-extended into an `i32`**: full scale is
  `I32_PCM_FULL_SCALE` = 2^23, not `i32::MAX`. A bridge that fills the whole
  32-bit range plays 256 times (+48 dB) too loud.
- interleaved, layout:
  `[s0c0, s0c1, …, s1c0, s1c1, …]`
- `channel_labels.len()` must match `channel_count`

The listings in this document are checked against `bridge_api/src/lib.rs`
by `bridge_api/tests/doc_listings.rs`: a field or a method changed in the code
and not here fails CI.

## Spatial Metadata

```rust
pub struct RMetadataFrame {
    pub events: RVec<REvent>,
    pub object_channels: RVec<RObjectChannel>,
    pub channel_gains: RVec<RChannelGain>,
    pub name_updates: RVec<RNameUpdate>,
    pub sample_pos: u64,
    pub ramp_duration: u32,
}
```

### Object event

```rust
pub struct REvent {
    pub id: u32,
    pub sample_pos: u64,
    pub has_pos: bool,
    pub pos: [f64; 3],
    pub gain_db: i8,
    pub size: [f64; 3],
    pub ramp_duration: u32,
}
```

`pos` depends on `coordinate_format()`:

- `Cartesian`
  - `[x, y, z]`
- `Polar`
  - `[azimuth_deg, elevation_deg, distance]`
  - azimuth:
    - `0°` = front
    - `-90°` = left
    - `+90°` = right
  - elevation in `[-90°, +90°]`
  - distance non-negative

If `has_pos == false`, the event is a gain/ramp-only update for its object.

### Objects, channel gains and names

- `object_channels`
  - binds each dynamic object ID to the PCM channel carrying its audio (see
    [`docs/channel-object-contract.md`](../docs/channel-object-contract.md))
- `channel_gains`
  - metadata-driven gain automation for fixed channels
- `name_updates`
  - sparse object-name updates keyed by object ID

## Capability and Host Hint Methods

### `is_ready()`
- `true` once the bridge has successfully decoded at least one frame

### `has_objects()`
- `true` while the current presentation carries dynamic objects
- may flip mid-stream; callers must not latch it

### `coordinate_format()`
- declares how `REvent.pos` must be interpreted

### `vbap_cartesian_defaults()`
- provides default Cartesian VBAP grid sizes, below the floor included
  (`z_neg_size`, since 0.6; 0 for none)
- also advertises `allow_negative_z`

### `preferred_vbap_table_mode()`
- bridge hint when the user did not force VBAP mode explicitly

These are host hints, not host commands.

## Configuration Keys

`configure(key, value)` is bridge-defined.

`omniphony-renderer` currently relies on:
- `presentation`
  - used to select the presentation / substream / best presentation according
    to bridge-specific semantics
- `log_level` (`off`, `error`, `warn`, `info`, `debug`, `trace`)
  - the host's log level: the most verbose diagnostic worth formatting and
    handing to the host log sink, since the host drops the rest. Sent when the
    bridge is created and again before the next packet whenever the host's
    level changes (`log_level` over OSC). A bridge may apply it process-wide.
    A bridge that returns `false` (one that predates the key) keeps its own
    level, and the host stops sending it changes
- `input_codec`
  - the codec of the raw access units the host will push, when it knows it
    (a player names the stream's codec): one of the names the bridge lists in
    `input_codecs`. Sent once, before the first packet; the bridge decodes
    those units as that codec rather than detecting it. Empty or `auto`
    restores detection

Return value:
- `true`
  - recognised key
- `false`
  - unknown key

## Strict vs Non-Strict

The constructor receives `strict: bool`. It is a legacy flag kept for ABI
compatibility: the hosts in this repository always pass `false`, and a bridge
may ignore it.

Expected behavior:
- non-strict mode (what hosts use)
  - the bridge may recover by resetting internally and continuing
- strict mode (only if a bridge chooses to honour the flag)
  - fatal parse/decode problems should surface via `error_message`

In both modes, `did_reset` should report internal recovery resets.

## Minimal Responsibilities of a Bridge

A usable bridge plugin must:
- export the `format_bridge` root module
- create a valid bridge object in `new_bridge`
- accept input through `push_packet(...)`
- emit interleaved PCM frames, scaled to `I32_PCM_FULL_SCALE`
- emit one channel label per PCM channel
- expose coherent metadata when spatial objects are present
- support `reset()`

## Related Host Code

- [orender_engine/src/bridge_loader.rs](orender_engine/src/bridge_loader.rs)
- [orender_engine/src/decode_step.rs](orender_engine/src/decode_step.rs)
- [src/cli/decode/session_run.rs](src/cli/decode/session_run.rs)
- [src/cli/decode/decoder_thread.rs](src/cli/decode/decoder_thread.rs)
