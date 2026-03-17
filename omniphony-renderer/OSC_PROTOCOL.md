# OSC Protocol

This document describes the OSC messages exchanged between `gsrd` and a visualizer
or control client.

## Overview

`gsrd` can:

- broadcast decoded spatial metadata
- broadcast live renderer state
- accept control messages for gain, mute, spread, room ratio, and speaker layout edits
- expose an OSC registration endpoint for dynamic clients

## Ports

| CLI option | Default | Purpose |
|---|---|---|
| `--osc-host` | `127.0.0.1` | Fixed OSC client target |
| `--osc-port` | `9000` | Fixed OSC client port |
| `--osc-rx-port` | `9000` | `gsrd` receive port for registration and control |

The fixed client defined by `--osc-host:--osc-port` always receives broadcasts.
Additional clients can register dynamically.

## Registration

### `/gsrd/register`

Sent by a client to `--osc-rx-port`.

Arguments:

| Name | Type | Optional | Description |
|---|---|---|---|
| `listen_port` | `i32` | Yes | Client receive port if different from the UDP source port |

After registration, `gsrd` sends:

1. a config bundle with the speaker layout
2. a state bundle with the current live renderer state

### `/gsrd/heartbeat`

Arguments:

| Name | Type | Optional | Description |
|---|---|---|---|
| `listen_port` | `i32` | Yes | Same convention as `/gsrd/register` |

Responses:

- `/gsrd/heartbeat/ack`
- `/gsrd/heartbeat/unknown`

Dynamic clients should send heartbeats periodically to stay registered.

## Messages Sent by gsrd

### Initial Configuration

#### `/gsrd/config/speakers`

| Argument | Type | Description |
|---|---|---|
| `count` | `i32` | Total speaker count |

#### `/gsrd/config/speaker/{idx}`

| Argument | Type | Description |
|---|---|---|
| `name` | `string` | Speaker name, for example `FL`, `TFR`, `LFE` |
| `azimuth` | `f32` | Degrees |
| `elevation` | `f32` | Degrees |
| `distance` | `f32` | Metres, for visualization and UI |
| `spatialize` | `i32` | `1` if the speaker participates in VBAP, `0` otherwise |

### Spatial Metadata

#### `/gsrd/spatial/frame`

| Argument | Type | Description |
|---|---|---|
| `sample_pos` | `i64` | Sample position from start of stream |
| `generation` | `i64` | Monotonic content generation ID |
| `object_count` | `i32` | Number of active objects in this frame |
| `coordinate_format` | `i32` | `0=cartesian`, `1=polar` |

#### `/gsrd/object/{idx}/xyz`

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

#### `/gsrd/meter/object/{idx}`

| Argument | Type | Description |
|---|---|---|
| `peak_dbfs` | `f32` | Object peak level |
| `rms_dbfs` | `f32` | Object RMS level |

#### `/gsrd/meter/object/{idx}/gains`

Variable-length list of linear gains, one value per output speaker.

#### `/gsrd/meter/speaker/{idx}`

| Argument | Type | Description |
|---|---|---|
| `peak_dbfs` | `f32` | Speaker peak level |
| `rms_dbfs` | `f32` | Speaker RMS level |

### Timestamp

#### `/gsrd/timestamp`

| Argument | Type | Description |
|---|---|---|
| `sample_pos` | `i64` | Sample position |
| `seconds` | `f64` | Time from start of stream |

### Live State

These messages are broadcast whenever a live parameter changes, and are also sent
to newly registered clients as part of the initial state bundle.

Common addresses include:

- `/gsrd/state/gain`
- `/gsrd/state/object/{idx}/gain`
- `/gsrd/state/object/{idx}/mute`
- `/gsrd/state/speaker/{idx}/gain`
- `/gsrd/state/speaker/{idx}/mute`
- `/gsrd/state/speaker/{idx}`
- `/gsrd/state/speakers/recomputing`
- `/gsrd/state/spread/min`
- `/gsrd/state/spread/max`
- `/gsrd/state/spread/from_distance`
- `/gsrd/state/spread/distance_range`
- `/gsrd/state/spread/distance_curve`
- `/gsrd/state/loudness`
- `/gsrd/state/loudness/source`
- `/gsrd/state/loudness/gain`
- `/gsrd/state/room_ratio`
- `/gsrd/state/vbap/table_mode`
- `/gsrd/state/vbap/effective_mode`
- `/gsrd/state/log_level`

### Log Stream

#### `/gsrd/log`

| Argument | Type | Description |
|---|---|---|
| `seq` | `i64` | Monotonic log sequence number |
| `level` | `string` | `error`, `warn`, `info`, `debug` or `trace` |
| `target` | `string` | Rust log target/module |
| `message` | `string` | Log message text |

## Messages Sent to gsrd

All control messages are sent to `--osc-rx-port`.

Common control addresses include:

- `/gsrd/control/gain`
- `/gsrd/control/object/{idx}/gain`
- `/gsrd/control/object/{idx}/mute`
- `/gsrd/control/speaker/{idx}/gain`
- `/gsrd/control/speaker/{idx}/mute`
- `/gsrd/control/spread/min`
- `/gsrd/control/spread/max`
- `/gsrd/control/spread/from_distance`
- `/gsrd/control/spread/distance_range`
- `/gsrd/control/spread/distance_curve`
- `/gsrd/control/loudness`
- `/gsrd/control/room_ratio`
- `/gsrd/control/vbap/table_mode`
- `/gsrd/control/speaker/{idx}/az`
- `/gsrd/control/speaker/{idx}/el`
- `/gsrd/control/speaker/{idx}/distance`
- `/gsrd/control/speaker/{idx}/spatialize`
- `/gsrd/control/speakers/apply`
- `/gsrd/control/speakers/reset`
- `/gsrd/control/save_config`
- `/gsrd/control/reload_config`
- `/gsrd/control/log_level`
- `/gsrd/control/ramp_mode`

`/gsrd/control/reload_config` requests a full render restart so `gsrd` re-resolves
its effective options from the config file and restarts the current stream with
those settings.

`/gsrd/control/log_level s <level>` changes the runtime log filter immediately.
Accepted values are `off`, `error`, `warn`, `info`, `debug`, `trace`.

`/gsrd/control/ramp_mode s <mode>` changes how object ramps are rendered.
Accepted values are:

- `off`: no interpolation, jump directly to the target
- `frame`: one interpolation step per decoded audio frame
- `sample`: one interpolation step per rendered sample

## Speaker Recompute Flow

Speaker position edits are staged first, then applied atomically through:

- `/gsrd/control/speakers/apply`

During recompute, `gsrd` broadcasts:

- `/gsrd/state/speakers/recomputing i 1`

When the new topology is published, it broadcasts:

- `/gsrd/state/speakers/recomputing i 0`
- updated `/gsrd/state/speaker/{idx}` messages
- updated `/gsrd/state/speaker/{idx}/spatialize` messages
- `/gsrd/state/vbap/effective_mode`

## Notes

- Speaker gains and mutes apply after VBAP mixing.
- Object controls address PCM channel indices.
- Layout recompute requires runtime VBAP support and is not available when using a precomputed VBAP table.
- `room_ratio` scales ADM coordinates before VBAP rendering.

## Recommended Next Step

The bridge API is documented separately in
[BRIDGE_API.md](/home/user/dev/spatial-renderer/gsrd/BRIDGE_API.md). This file
only describes the OSC surface exposed by `gsrd`.
