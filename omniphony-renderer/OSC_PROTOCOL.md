# OSC Protocol

This document describes the OSC messages exchanged between `orender` and a visualizer
or control client.

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

1. a config bundle with the speaker layout
2. a state bundle with the current live renderer state

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

### Initial Configuration

#### `/omniphony/config/speakers`

| Argument | Type | Description |
|---|---|---|
| `count` | `i32` | Total speaker count |

#### `/omniphony/config/speaker/{idx}`

| Argument | Type | Description |
|---|---|---|
| `name` | `string` | Speaker name, for example `FL`, `TFR`, `LFE` |
| `azimuth` | `f32` | Degrees |
| `elevation` | `f32` | Degrees |
| `distance` | `f32` | Metres, for visualization and UI |
| `spatialize` | `i32` | `1` if the speaker participates in VBAP, `0` otherwise |

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

Common addresses include:

- `/omniphony/state/gain`
- `/omniphony/state/input_pipe`
- `/omniphony/state/input/mode`
- `/omniphony/state/input/active_mode`
- `/omniphony/state/input/apply_pending`
- `/omniphony/state/input/backend`
- `/omniphony/state/input/channels`
- `/omniphony/state/input/sample_rate`
- `/omniphony/state/input/node`
- `/omniphony/state/input/stream_format`
- `/omniphony/state/input/error`
- `/omniphony/state/input/live/backend`
- `/omniphony/state/input/live/node`
- `/omniphony/state/input/live/description`
- `/omniphony/state/input/live/layout`
- `/omniphony/state/input/live/channels`
- `/omniphony/state/input/live/sample_rate`
- `/omniphony/state/input/live/format`
- `/omniphony/state/input/live/map`
- `/omniphony/state/input/live/lfe_mode`
- `/omniphony/state/object/{idx}/gain`
- `/omniphony/state/object/{idx}/mute`
- `/omniphony/state/speaker/{idx}/gain`
- `/omniphony/state/speaker/{idx}/mute`
- `/omniphony/state/speaker/{idx}`
- `/omniphony/state/speakers/recomputing`
- `/omniphony/state/spread/min`
- `/omniphony/state/spread/max`
- `/omniphony/state/spread/from_distance`
- `/omniphony/state/spread/distance_range`
- `/omniphony/state/spread/distance_curve`
- `/omniphony/state/loudness`
- `/omniphony/state/loudness/source`
- `/omniphony/state/loudness/gain`
- `/omniphony/state/room_ratio`
- `/omniphony/state/vbap/table_mode`
- `/omniphony/state/vbap/effective_mode`
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
- `/omniphony/control/object/{idx}/gain`
- `/omniphony/control/object/{idx}/mute`
- `/omniphony/control/speaker/{idx}/gain`
- `/omniphony/control/speaker/{idx}/mute`
- `/omniphony/control/spread/min`
- `/omniphony/control/spread/max`
- `/omniphony/control/spread/from_distance`
- `/omniphony/control/spread/distance_range`
- `/omniphony/control/spread/distance_curve`
- `/omniphony/control/loudness`
- `/omniphony/control/room_ratio`
- `/omniphony/control/vbap/table_mode`
- `/omniphony/control/speaker/{idx}/az`
- `/omniphony/control/speaker/{idx}/el`
- `/omniphony/control/speaker/{idx}/distance`
- `/omniphony/control/speaker/{idx}/spatialize`
- `/omniphony/control/speakers/apply`
- `/omniphony/control/speakers/reset`
- `/omniphony/control/save_config`
- `/omniphony/control/reload_config`
- `/omniphony/control/log_level`
- `/omniphony/control/ramp_mode`

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

- `/omniphony/state/input/mode`
  - staged mode requested by Studio

- `/omniphony/state/input/active_mode`
  - mode currently active in the runtime

- `/omniphony/state/input/apply_pending`
  - `1` after staged edits are ready to apply, `0` after apply has been consumed

- `/omniphony/state/input/error`
  - last runtime error for the input path

- `/omniphony/state/input/live/...`
  - staged settings requested by Studio

- `/omniphony/state/input/backend`, `/channels`, `/sample_rate`, `/node`, `/stream_format`
  - currently applied runtime values

## Speaker Recompute Flow

Speaker position edits are staged first, then applied atomically through:

- `/omniphony/control/speakers/apply`

During recompute, `orender` broadcasts:

- `/omniphony/state/speakers/recomputing i 1`

When the new topology is published, it broadcasts:

- `/omniphony/state/speakers/recomputing i 0`
- updated `/omniphony/state/speaker/{idx}` messages
- updated `/omniphony/state/speaker/{idx}/spatialize` messages
- `/omniphony/state/vbap/effective_mode`

## Notes

- Speaker gains and mutes apply after VBAP mixing.
- Object controls address PCM channel indices.
- Layout recompute requires runtime VBAP support and is not available when using a precomputed VBAP table.
- `room_ratio` scales ADM coordinates before VBAP rendering.

## Recommended Next Step

The bridge API is documented separately in
[BRIDGE_API.md](BRIDGE_API.md). This file
only describes the OSC surface exposed by `orender`.
