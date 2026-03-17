# Bridge API

This document describes the runtime plugin ABI used by `gsrd` to load external
decoder bridges.

The ABI is defined in:
- [bridge_api/src/lib.rs](/home/user/dev/spatial-renderer/gsrd/bridge_api/src/lib.rs)
- [src/bridge_loader.rs](/home/user/dev/spatial-renderer/gsrd/src/bridge_loader.rs)

`gsrd` does not decode immersive formats directly. A bridge plugin owns the
format-specific parsing, decode pipeline, and spatial metadata extraction.

## Loading Model

Bridge lookup order:
1. `--bridge-path <FILE>`
2. `render.bridge_path` in the config file
3. the first file matching `lib*_bridge.so`, `lib*_bridge.dll`, or
   `lib*_bridge.dylib` next to the `gsrd` executable

Without a bridge plugin, `gsrd` will not start.

## Exported Root Module

Each plugin must export the `format_bridge` root module expected by
`abi_stable`:

```rust
#[repr(C)]
#[derive(StableAbi)]
#[sabi(kind(Prefix(prefix_ref = BridgeLibRef)))]
pub struct BridgeLib {
    pub new_bridge: extern "C" fn(strict: bool) -> FormatBridgeBox,
}
```

Fixed names:
- `BASE_NAME = "format_bridge"`
- `NAME = "format_bridge"`

## Bridge Lifecycle

Host lifecycle:
1. load the shared library
2. resolve `new_bridge`
3. create a bridge instance with `strict`
4. call `configure(...)` as needed
5. query capability/hints
6. feed input via `push_packet(...)`
7. call `reset()` on seek/discontinuity/end-of-stream reset

Important expectations:
- `configure(...)` happens before the first `push_packet(...)`
- `is_spatial()` is meaningful after configuration
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
    fn is_spatial(&self) -> bool;
    fn configure(&mut self, key: RStr<'_>, value: RStr<'_>) -> bool;
    fn coordinate_format(&self) -> RCoordinateFormat;
    fn vbap_cartesian_defaults(&self) -> RVbapCartesianDefaults;
    fn preferred_vbap_table_mode(&self) -> RVbapTableMode;
}
```

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
  - non-empty for fatal bridge errors
  - mainly relevant in strict mode
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
    pub dialogue_level: ROption<i8>,
    pub is_new_segment: bool,
}
```

PCM rules:
- interleaved signed 32-bit integer samples
- layout:
  `[s0c0, s0c1, …, s1c0, s1c1, …]`
- `channel_labels.len()` must match `channel_count`

## Spatial Metadata

```rust
pub struct RMetadataFrame {
    pub events: RVec<REvent>,
    pub bed_indices: RVec<usize>,
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
    pub spread: f64,
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

If `has_pos == false`, the event is non-positional. That is typical for direct
bed-style channels.

### Beds and names

- `bed_indices`
  - format-provided bed channel IDs in the same ID space as `REvent.id`
- `name_updates`
  - sparse object-name updates keyed by object ID

## Capability and Host Hint Methods

### `is_ready()`
- `true` once the bridge has successfully decoded at least one frame

### `is_spatial()`
- `true` if the current configured presentation may carry spatial objects
- called after configuration and before decode starts

### `coordinate_format()`
- declares how `REvent.pos` must be interpreted

### `vbap_cartesian_defaults()`
- provides default Cartesian VBAP grid sizes
- also advertises `allow_negative_z`

### `preferred_vbap_table_mode()`
- bridge hint when the user did not force VBAP mode explicitly

These are host hints, not host commands.

## Configuration Keys

`configure(key, value)` is bridge-defined.

`gsrd` currently relies on:
- `presentation`
  - used to select the presentation / substream / best presentation according
    to bridge-specific semantics

Return value:
- `true`
  - recognised key
- `false`
  - unknown key

## Strict vs Non-Strict

The constructor receives `strict: bool`.

Expected behavior:
- strict mode
  - fatal parse/decode problems should surface via `error_message`
- non-strict mode
  - the bridge may recover by resetting internally and continuing

In both modes, `did_reset` should report internal recovery resets.

## Minimal Responsibilities of a Bridge

A usable bridge plugin must:
- export the `format_bridge` root module
- create a valid bridge object in `new_bridge`
- accept input through `push_packet(...)`
- emit interleaved PCM frames
- emit one channel label per PCM channel
- expose coherent metadata when spatial objects are present
- support `reset()`

## Related Host Code

- [src/bridge_loader.rs](/home/user/dev/spatial-renderer/gsrd/src/bridge_loader.rs)
- [src/cli/decode/decode_impl.rs](/home/user/dev/spatial-renderer/gsrd/src/cli/decode/decode_impl.rs)
- [src/cli/decode/decoder_thread.rs](/home/user/dev/spatial-renderer/gsrd/src/cli/decode/decoder_thread.rs)
