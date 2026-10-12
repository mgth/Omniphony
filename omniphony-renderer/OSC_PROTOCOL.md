# OSC Protocol

This document describes the OSC messages exchanged between `orender`, Omniphony Studio
(`omniphony-studio-egui`), and compatible metadata producers such as `adm-player`.

## Overview

`orender` can:

- broadcast decoded spatial metadata
- broadcast live renderer state
- accept control messages for gain, mute, spread, room ratio, and speaker layout edits
- expose an OSC registration endpoint for dynamic clients

## Ports

| CLI option | Default | Purpose |
|---|---|---|
| `--osc-host` | `127.0.0.1` | Fixed OSC client target |
| `--osc-port` | `9000` | Fixed OSC client port |
| `--osc-rx-port` | `9000` | `orender` receive port for registration and control |

The fixed client defined by `--osc-host:--osc-port` always receives broadcasts.
Additional clients can register dynamically.

## Registration

### `/omniphony/register`

Sent by a client to `--osc-rx-port`.

Arguments:

| Name | Type | Optional | Description |
|---|---|---|---|
| `listen_port` | `i32` | Yes | Client receive port if different from the UDP source port |

After registration, `orender` sends:

1. a state bundle with the current live renderer state, including `state/layout` and `state/speakers`

### `/omniphony/heartbeat`

Arguments:

| Name | Type | Optional | Description |
|---|---|---|---|
| `listen_port` | `i32` | Yes | Same convention as `/omniphony/register` |

Responses:

- `/omniphony/heartbeat/ack`
- `/omniphony/heartbeat/unknown`

Dynamic clients should send heartbeats periodically to stay registered.

## Messages Sent by orender

### Serialized State

#### `/omniphony/state/layout`

Serialized JSON layout snapshot with speaker geometry and static metadata.

#### `/omniphony/state/speakers`

Serialized JSON speaker runtime/config snapshot with per-speaker `gain`, `delayMs`, and `muted`.

### Spatial Metadata

#### `/omniphony/spatial/frame`

| Argument | Type | Description |
|---|---|---|
| `sample_pos` | `i64` | Sample position from start of stream |
| `generation` | `i64` | Monotonic content generation ID |
| `object_count` | `i32` | Number of active objects in this frame |
| `coordinate_format` | `i32` | `0=cartesian`, `1=polar` |

#### `/omniphony/object/{idx}/xyz`

| Argument | Type | Description |
|---|---|---|
| `x` | `f32` | ADM X coordinate |
| `y` | `f32` | ADM Y coordinate |
| `z` | `f32` | ADM Z coordinate |
| `gain_db` | `i32` | Per-object gain in dBFS |
| `priority` | `f32` | Object priority |
| `divergence` | `f32` | Object divergence |
| `ramp_duration` | `i32` | Ramp duration in audio frames |
| `generation` | `i64` | Monotonic content generation ID |
| `name` | `string` | Object or bed label |

#### `/omniphony/object/{idx}/remove`

Sent when an object's slot goes away — the frame's `object_count` shrank past
it, or the content changed.

| Argument | Type | Description |
|---|---|---|
| `generation` | `i64` | Monotonic content generation ID |

A slot going away is also signalled the older way, by zeroing its position,
`/size` and `/meta`, and that is still sent for clients that predate this
message. Both are emitted for the same slot: the zeroed triple first, then this.

A client that only watches `object_count` has to infer which slots are gone,
which is what leaves ghost objects behind after a seek — the count can stay the
same while the objects behind it change.

### Metering

Pre-enabled for the fixed client with `--osc-metering`; a registered client
subscribes itself with `/omniphony/control/metering i 1`.

#### `/omniphony/meter/object/{idx}`

| Argument | Type | Description |
|---|---|---|
| `peak_dbfs` | `f32` | Object peak level |
| `rms_dbfs` | `f32` | Object RMS level |

#### `/omniphony/meter/object/{idx}/gains`

Variable-length list of linear gains, one value per output speaker.

#### `/omniphony/meter/object/{idx}/band/{band}/gains`

Per-band gains, the same shape, when the layout has a crossover.

#### `/omniphony/meter/speaker/{idx}`

| Argument | Type | Description |
|---|---|---|
| `peak_dbfs` | `f32` | Speaker peak level |
| `rms_dbfs` | `f32` | Speaker RMS level |

#### `/omniphony/meter/ear/{idx}`

Headphone ear levels (`0` left, `1` right) in binaural output mode, same
arguments.

### Timestamp

#### `/omniphony/timestamp`

| Argument | Type | Description |
|---|---|---|
| `sample_pos` | `i64` | Sample position |
| `seconds` | `f64` | Time from start of stream |

### Playout

The messages above describe a block of audio and are sent as it is rendered.
The sound comes out later, by everything buffered behind the render: the
output ring and the device for the standalone `orender` (which measures it),
or whatever an embedding host holds — seconds, behind Kodi — which the host
reports through the C ABI's `heard_us` option. A client that wants to show each
block when it is heard, as Studio does, needs two things, and the engine sends
them once it knows where the listener is (never before, so a client of an
engine that cannot tell sees the stream exactly as before):

#### `/omniphony/playout/block`

| Argument | Type | Description |
|---|---|---|
| `pos` | `i64` | The object frames, timestamps, bed config and meter bundles that follow describe the block of audio starting at this sample |

Sent ahead of the first such message of each block, not for every block. The
timeline is the engine's own (samples rendered since the start, or since the
last reset for an embedded engine), not the bridge's `sample_pos`; a position
lower than the previous one means the timeline started again and anything held
about the old one will never be heard.

#### `/omniphony/playout/heard`

| Argument | Type | Description |
|---|---|---|
| `pos` | `i64` | The sample the listener is hearing, on the same timeline |
| `rate` | `i32` | Samples per second the timeline advances at while it plays |

At most every 20 ms while the audio plays. When it stops coming, so has the
sound (a pause): a client extrapolating between two reports should not carry
the listener on for long.

The engine holds nothing back; a client that wants the description early (to
prepare for it) ignores both. Studio queues the stream messages and applies
each one once the listener reaches its block, and applies everything else —
state, replies to an edit — at once.

### Live State

The control and state surface — every `/omniphony/control/…` address a client
can send and every `/omniphony/state/…` address the engine publishes, with
argument types and semantics — is documented in
[`docs/osc-control-contract.md`](../docs/osc-control-contract.md), generated
against the `osc-contract` crate that names each address. This file only covers
the session handshake and the streams above.

In short: a newly registered client receives the full live-state snapshot (one
OSC bundle, or several consecutive ones when it would not fit a datagram, always
ending with `/omniphony/state/snapshot_complete`). The serialized domain
messages are `/omniphony/state/{capabilities,renderer,layout,speakers,input,
loudness,monitoring}` (`s <json>`), plus `/omniphony/state/audio` when the host
owns audio output.

### Log Stream

#### `/omniphony/log`

| Argument | Type | Description |
|---|---|---|
| `seq` | `i64` | Monotonic log sequence number |
| `level` | `string` | `error`, `warn`, `info`, `debug` or `trace` |
| `target` | `string` | Rust log target/module |
| `message` | `string` | Log message text |

## Messages Sent to orender

All control messages are sent to `--osc-rx-port`; the full list is in
[`docs/osc-control-contract.md`](../docs/osc-control-contract.md). Two usage
patterns are worth knowing:

- **Realtime controls** carry a trailing sequence number and use latest-wins
  semantics: `/omniphony/control/realtime/master_gain [f32 value, i32 seq]`
  and `/omniphony/control/realtime/speaker_gain [i32 id, f32 value, i32 seq]`,
  acknowledged on `/omniphony/state/realtime/{master_gain,speaker_gain}`. Fast
  gain drags should use them.
- **Config-domain controls** take a JSON patch and are staged, then applied:
  `/omniphony/control/config/{audio,input,layout} s <json>` followed by
  `/omniphony/control/config/{audio,input,layout}/apply`, and
  `/omniphony/control/config/speakers s <json>` for runtime speaker edits
  (`muted`, `delayMs`). Speaker topology and metadata edits go through
  `config/layout`.

### Live Input Control for Studio

The live-input surface is designed for staged editing from a controller such as
Studio:

1. send one or more staged values under `/omniphony/control/input/…` (`mode`,
   `live/{backend,node,description,layout,layout_import,channels,sample_rate,
   clock_mode,map,lfe_mode}`),
2. send `/omniphony/control/input/apply`,
3. observe `/omniphony/state/input`, the serialized input domain carrying both
   the staged and the active runtime values.

`/omniphony/control/input/refresh` makes `orender` rebroadcast the full state
bundle, for a client that reconnects without sending `/omniphony/register`.

## Speaker Recompute Flow

Speaker position edits are staged with `/omniphony/control/config/layout` and
applied atomically with `/omniphony/control/config/layout/apply`.

During recompute, `orender` broadcasts:

- `/omniphony/state/speakers/recomputing i 1`

When the new topology is published, it broadcasts:

- `/omniphony/state/speakers/recomputing i 0`
- updated `/omniphony/state/layout s <json>`
- updated `/omniphony/state/speakers s <json>`

A failed rebuild is reported on `/omniphony/state/speakers/recompute_error`.
So is a published topology (or a crossover change) whose band engines could
not be built: the previous bands keep rendering, and an empty string follows
once a later build goes through.

## Notes

- Speaker gains and mutes apply after VBAP mixing.
- Object controls address PCM channel indices.
- Layout recompute requires runtime VBAP support and is not available when using a precomputed VBAP table.
- room geometry and distance-diffuse settings live in `state/renderer`.

## Recommended Next Step

The bridge API is documented separately in
[BRIDGE_API.md](BRIDGE_API.md). This file
only describes the OSC surface exposed by `orender`.
