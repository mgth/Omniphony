# OSC Protocol

This document describes the OSC messages exchanged between `orender`, `omniphony-studio`,
and compatible metadata producers such as `adm-player`.

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

### Metering

Enabled with `--osc-metering`.

#### `/omniphony/meter/object/{idx}`

| Argument | Type | Description |
|---|---|---|
| `peak_dbfs` | `f32` | Object peak level |
| `rms_dbfs` | `f32` | Object RMS level |

#### `/omniphony/meter/object/{idx}/gains`

Variable-length list of linear gains, one value per output speaker.

#### `/omniphony/meter/speaker/{idx}`

| Argument | Type | Description |
|---|---|---|
| `peak_dbfs` | `f32` | Speaker peak level |
| `rms_dbfs` | `f32` | Speaker RMS level |

### Timestamp

#### `/omniphony/timestamp`

| Argument | Type | Description |
|---|---|---|
| `sample_pos` | `i64` | Sample position |
| `seconds` | `f64` | Time from start of stream |

### Live State

These messages are broadcast whenever a live parameter changes, and are also sent
to newly registered clients as part of the initial state bundle.

Canonical serialized domain messages:

- `/omniphony/state/capabilities s <json>`
- `/omniphony/state/renderer s <json>`
- `/omniphony/state/audio s <json>`
- `/omniphony/state/layout s <json>`
- `/omniphony/state/input s <json>`
- `/omniphony/state/loudness s <json>`
- `/omniphony/state/session s <json>` for metadata-oriented producers such as `adm-player`

Common addresses include:

- `/omniphony/state/input_pipe`
- `/omniphony/state/object/{idx}/mute`
- `/omniphony/state/speaker/{idx}/gain`
- `/omniphony/state/speaker/{idx}/mute`
- `/omniphony/state/speaker/{idx}`
- `/omniphony/state/speakers/recomputing`
- `/omniphony/state/log_level`

### Log Stream

#### `/omniphony/log`

| Argument | Type | Description |
|---|---|---|
| `seq` | `i64` | Monotonic log sequence number |
| `level` | `string` | `error`, `warn`, `info`, `debug` or `trace` |
| `target` | `string` | Rust log target/module |
| `message` | `string` | Log message text |

## Messages Sent to orender

All control messages are sent to `--osc-rx-port`.

Sequenced realtime controls use `latest-wins` semantics:

- `/omniphony/control/realtime/master_gain [f32 value, i32 seq]`
- `/omniphony/control/realtime/speaker_gain [i32 id, f32 value, i32 seq]`

Serialized config-domain controls use JSON patches:

- `/omniphony/control/config/audio s <json>`
- `/omniphony/control/config/audio/apply`
- `/omniphony/control/config/input s <json>`
- `/omniphony/control/config/input/apply`
- `/omniphony/control/config/layout s <json>`
- `/omniphony/control/config/layout/apply`
- `/omniphony/control/config/speakers s <json>`

Canonical acknowledgements:

- `/omniphony/state/realtime/master_gain [f32 value, i32 seq]`
- `/omniphony/state/realtime/speaker_gain [i32 id, f32 value, i32 seq]`

Common control addresses include:

- `/omniphony/control/input/refresh`
- `/omniphony/control/input/mode`
- `/omniphony/control/input/live/backend`
- `/omniphony/control/input/live/node`
- `/omniphony/control/input/live/description`
- `/omniphony/control/input/live/layout`
- `/omniphony/control/input/live/channels`
- `/omniphony/control/input/live/sample_rate`
- `/omniphony/control/input/live/format`
- `/omniphony/control/input/live/map`
- `/omniphony/control/input/live/lfe_mode`
- `/omniphony/control/input/apply`
- `/omniphony/control/audio/output_devices/refresh`
- `/omniphony/control/gain`
- `/omniphony/control/object/{idx}/mute`
- `/omniphony/control/speaker/{idx}/gain`
- `/omniphony/control/spread/min`
- `/omniphony/control/spread/max`
- `/omniphony/control/spread/from_distance`
- `/omniphony/control/spread/distance_range`
- `/omniphony/control/spread/distance_curve`
- `/omniphony/control/loudness`
- `/omniphony/control/room_ratio`
- `/omniphony/control/render_evaluation_mode`
- `/omniphony/control/save_config`
- `/omniphony/control/reload_config`
- `/omniphony/control/log_level`
- `/omniphony/control/ramp_mode`

Speaker topology and metadata edits should use `control/config/layout`.
Speaker runtime edits such as `mute` and `delayMs` should use `control/config/speakers`.
Fast speaker gain drags should use `control/realtime/speaker_gain`.

`/omniphony/control/reload_config` requests a full render restart so `orender` re-resolves
its effective options from the config file and restarts the current stream with
those settings.

`/omniphony/control/log_level s <level>` changes the runtime log filter immediately.
Accepted values are `off`, `error`, `warn`, `info`, `debug`, `trace`.

`/omniphony/control/ramp_mode s <mode>` changes how object ramps are rendered.
Accepted values are:

- `off`: no interpolation, jump directly to the target
- `frame`: one interpolation step per decoded audio frame
- `sample`: one interpolation step per rendered sample

### Live Input Control for Studio

The live-input surface is designed for staged editing from a controller such as
Studio.

Recommended flow:

1. send one or more staged values under `/omniphony/control/input/...`
2. send `/omniphony/control/input/apply`
3. observe `/omniphony/state/input/...` for the applied runtime state

Important addresses:

- `/omniphony/control/input/refresh`
  - forces `orender` to rebroadcast the full current state bundle
  - useful if Studio reconnects without sending `/omniphony/register`

- `/omniphony/control/input/mode s <bridge|live>`
  - stages the requested active source mode

- `/omniphony/control/input/live/backend s <pipewire|asio>`
  - stages the backend used when `mode=live`

- `/omniphony/control/input/live/node s <name>`
  - stages the live input node name

- `/omniphony/control/input/live/description s <label>`
  - stages the human-readable live input node label

- `/omniphony/control/input/live/layout s <path>`
  - stages the source layout path used for fixed object positioning

- `/omniphony/control/input/live/channels i <count>`
  - stages the requested live input channel count

- `/omniphony/control/input/live/sample_rate i <hz>`
  - stages the requested live input sample rate

- `/omniphony/control/input/live/format s <f32|s16>`
  - stages the requested input sample format

- `/omniphony/control/input/live/map s <7.1-fixed>`
  - stages the fixed object mapping mode

- `/omniphony/control/input/live/lfe_mode s <object|direct|drop>`
  - stages the LFE policy

- `/omniphony/control/input/apply`
  - applies the staged live-input request atomically

State semantics:

- `state/input`
  - serialized canonical input domain carrying both staged and active runtime values

## Speaker Recompute Flow

Speaker position edits are staged first, then applied atomically through:

- `/omniphony/control/speakers/apply`

During recompute, `orender` broadcasts:

- `/omniphony/state/speakers/recomputing i 1`

When the new topology is published, it broadcasts:

- `/omniphony/state/speakers/recomputing i 0`
- updated `/omniphony/state/layout s <json>`
- updated `/omniphony/state/speakers s <json>`

## Notes

- Speaker gains and mutes apply after VBAP mixing.
- Object controls address PCM channel indices.
- Layout recompute requires runtime VBAP support and is not available when using a precomputed VBAP table.
- room geometry and distance-diffuse settings live in `state/renderer`.

## Recommended Next Step

The bridge API is documented separately in
[BRIDGE_API.md](BRIDGE_API.md). This file
only describes the OSC surface exposed by `orender`.
