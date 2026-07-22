# Omniphony OSC control / state contract

The engine is driven and observed entirely over **OSC** (UDP). This document is
the human-readable contract for client authors (Omniphony Studio, alternative
front-ends, automation). The machine-readable single source of truth for the
address strings is
[`runtime_control::osc_contract`](../omniphony-renderer/runtime_control/src/osc_contract.rs)
— every address below has a named constant there, and the exhaustive lists are
`osc_contract::ALL_CONTROL` and `osc_contract::ALL_STATE`. Keep this document and
that module in sync.

## Directions

- **Control** — `/omniphony/control/…`, sent **client → engine** to change state
  or trigger an action.
- **State** — `/omniphony/state/…`, emitted **engine → subscribed clients** when
  something changes (and as snapshots on connect).

## Argument conventions

- **Booleans** are accepted as OSC `int` (`0`/non-zero), `float`, or `bool`; the
  engine coerces. Most togglish controls take a single int `0`/`1`.
- **Enums** are lowercase strings; an unrecognised value is ignored (the engine
  validates and drops bad input rather than erroring).
- **Realtime gain** controls (`/control/realtime/*`) carry a trailing monotonic
  **sequence int** so the engine can drop stale updates that arrive out of order.
- Larger structured payloads (layout / speakers / audio / input config) are sent
  as a single **JSON string** argument.

---

## Control — client → engine

### Spatialisation: spread & distance

| Address | Args | Meaning |
|---|---|---|
| `/control/spread/min` | f `[0,1]` | Minimum effective spread. |
| `/control/spread/max` | f `[0,1]` | Maximum effective spread. |
| `/control/spread/from_distance` | int bool | Derive spread from distance instead of object size. |
| `/control/spread/distance_range` | f `>0` | Distance at which distance-derived spread reaches 0. |
| `/control/spread/distance_curve` | f `≥0` | Curve exponent for distance-derived spread. |
| `/control/spread/size_to_spread_mode` | s | `max` \| `mean` \| `projection_perpendicular`. |
| `/control/distance_model` | s | `none` \| `linear` \| `quadratic` \| `inverse-square`. |
| `/control/distance_model_metric` | s | `spherical` \| `chebyshev`. |
| `/control/distance_diffuse/enabled` | int bool | Enable antipodal distance-diffuse blend. |
| `/control/distance_diffuse/threshold` | f `>0` | ADM distance at which the blend reaches 100 % direct. |
| `/control/distance_diffuse/curve` | f `≥0` | Blend-weight curve exponent. |
| `/control/distance_diffuse/metric` | s | `spherical` \| `chebyshev`. |
| `/control/room_ratio` | f×3 | Room proportions `[w, l, h]` used to scale ADM coords. |
| `/control/room_ratio_rear` | f | Rear scaling factor. |
| `/control/room_ratio_lower` | f | Lower-hemisphere scaling factor. |
| `/control/room_ratio_center_blend` | f | Centre-blend factor. |

### Render backend selection & parameters

| Address | Args | Meaning |
|---|---|---|
| `/control/render_backend` | s | Select active backend by id (built-in or contributor). |
| `/control/render_backend/restore` | int | Restore the previously selected backend. |
| `/control/backend/param` | `[key, value]` or `[backend_id, key, value]` | Generic backend parameter setter (schema-driven). With an explicit backend id, targets that backend (e.g. a hybrid inner backend); otherwise the selected one. |
| `/control/hybrid/external_backend` | s | Hybrid outer backend id. |
| `/control/hybrid/internal_backend` | s | Hybrid inner backend id. |
| `/control/hybrid/metric` | s | `spherical` \| `chebyshev`. |
| `/control/hybrid/curve_smoothing` | f `[0,1]` | Blend-curve smoothing. |
| `/control/hybrid/curve` | f×2N | Flattened `(x,y)` blend control points, each `[0,1]`. |

### Render evaluation (precomputed tables)

| Address | Args | Meaning |
|---|---|---|
| `/control/render_evaluation_mode` | s | `auto` \| `realtime` \| `precomputed_polar` \| `precomputed_cartesian`. |
| `/control/render_evaluation_mode/from_file` | s | Load a precomputed evaluator artifact. |
| `/control/render_evaluation/position_interpolation` | int bool | Nearest-cell vs trilinear table lookup. |
| `/control/render_evaluation/cartesian/{x_size,y_size,z_size,z_neg_size}` | int `≥1` (z_neg `≥0`) | Cartesian table resolution per axis. |
| `/control/render_evaluation/polar/azimuth_resolution` | int `≥1` | Azimuth cells. |
| `/control/render_evaluation/polar/elevation_resolution` | int `≥1` | Elevation cells. |
| `/control/render_evaluation/polar/distance_res` | int `≥1` | Distance cells. |
| `/control/render_evaluation/polar/distance_max` | f `>0` | Max table distance. |

### Gain, mute & loudness

| Address | Args | Meaning |
|---|---|---|
| `/control/realtime/master_gain` | f `[0,2]`, seq int | Master gain. |
| `/control/realtime/speaker_gain` | id int, f `[0,2]`, seq int | Per-speaker gain. |
| `/control/realtime/object_gain` | id s, f `[0,2]`, seq int | Per-object gain. |
| `/control/object/{id}/mute` | int bool | Per-object mute. |
| `/control/config/speakers` | json | Speaker edits (incl. per-speaker mute). |
| `/control/loudness` | int bool | Dialogue-norm / loudness correction. |
| `/control/auto_gain` | int bool | Auto gain-reduction on clipping. |
| `/control/auto_gain_ceiling` | f `[-12,0]` dB | Auto-gain target ceiling. |

### Adaptive resampling (output clock servo)

All under `/control/adaptive_resampling/…`. Master toggle: bare
`/control/adaptive_resampling` (int bool). Tunables: `kp_near`, `ki`,
`max_adjust`, `update_interval_callbacks`, `high_recover_entry_margin_ms`,
`integral_discharge_ratio`, `near_far_threshold_ms`, `reset_ratio`, `pause`,
and the far-mode group `enable_far_mode`, `force_silence_in_far_mode`,
`hard_recover_high_in_far_mode`, `hard_recover_low_in_far_mode`,
`far_mode_return_fade_in_ms`. `/control/latency_target` sets the target buffer
latency. See `PI_TUNING_PROCEDURE.md` and `docs/latency-regulation.md`.

### Audio output & live input

| Address | Args | Meaning |
|---|---|---|
| `/control/config/audio`, `/control/config/audio/apply` | json | Audio output config (stage / apply). |
| `/control/audio/output_device` | s | Select output device. |
| `/control/audio/output_devices/refresh` | — | Re-enumerate output devices. |
| `/control/audio/sample_rate` | int | Output sample rate. |
| `/control/config/input`, `/control/config/input/apply`, `/control/input/apply` | json | Input config (stage / apply). |
| `/control/input/mode` | s | Input source mode. |
| `/control/input/refresh` | — | Re-enumerate input sources. |
| `/control/input/drc_mode` | s | Dynamic-range-control mode. |
| `/control/input/drc_weight` | f `[0,1]` | DRC weight. |
| `/control/input/live/{backend,node,description,layout,layout_import,channels,sample_rate,format,clock_mode,map,lfe_mode}` | varies | Live-capture parameters. |
| `/control/render/bridge_path` | s | Path to the format bridge library. |
| `/control/render/input_pipe` | s | Named-pipe input path. |

### Head tracking (binaural)

Live head-pose control for the binaural (headphone) path. The orientation
*feed* itself arrives on a **user-configured** address
(`head_tracking.osc_address`, e.g. `/gamerotationvector` for Sensors2OSC /
`nxosc`) parsed per `head_tracking.format` — that is config, not a fixed
contract address. See `omniphony-renderer/BINAURAL.md`.

| Address | Args | Meaning |
|---|---|---|
| `/control/head/orientation` | f×3 (euler °) | Set head pose directly (yaw, pitch, roll). |
| `/control/head/quat` | f×4 | Set head pose directly (quaternion). |
| `/control/head/recenter` | — | Capture the current orientation as "front". |
| `/control/head/tracking/address` | s | Feed address the engine listens on (`""` disables tracking). |
| `/control/head/tracking/format` | s | `auto` \| `quat` \| `rotvec` \| `euler`. |
| `/control/head/tracking/smoothing` | f `[0,0.99]` | Pose smoothing (higher = smoother/laggier). |
| `/control/head/tracking/invert` | int bool | Mirror the applied rotation. |

### Layout

| Address | Args | Meaning |
|---|---|---|
| `/control/config/layout`, `/control/config/layout/apply` | json | Layout config (stage / apply). |
| `/control/layout/radius_m` | f | Layout radius (m). |
| `/control/layout/export` | s (optional name) | Export the current layout. |

### Overlay (Studio 3D / mpv overlay)

`/control/overlay/{enabled,labels,objects,trails,tag,heatmap_enabled,
heatmap_bands,heatmap_colormap,heatmap_custom_stops}` — visualisation toggles
and heatmap configuration.

### Diagnostics & engine lifecycle

| Address | Args | Meaning |
|---|---|---|
| `/control/metering/rate_hz` | f `[1,1000]` | Metering publication rate. |
| `/control/diag/rate_hz` | f `[1,1000]` | Diagnostics publication rate. |
| `/control/diag/enabled` | int bool | Enable diagnostics publication. |
| `/control/debug/speaker_gaintable/subscribe` | have_version int, speaker int | Subscribe to a speaker's gain-table field. |
| `/control/debug/speaker_gaintable/unsubscribe` | — | Release the gain-table subscription. |
| `/control/debug/speaker_gaintable/nack` | … | Request missing chunks / version. |
| `/control/log_level` | s | `off`\|`error`\|`warn`\|`info`\|`debug`\|`trace`. |
| `/control/ramp_mode` | s | `off` \| `frame` \| `sample`. |
| `/control/option` | s key, value | Generic setter for any declared live option (`renderer::options` registry; schema on `/state/options_schema`). The dedicated addresses (`synthetic_objects`, `surround_placement`, `output_channel_mapping`, `object_generator`, `phantom_extract`) are aliases of this. |
| `/control/save_config` | — | Persist the current config. |
| `/control/reload_config` | — | Reload config from disk. |
| `/control/quit` | — | Shut the engine down. |
| `/control/yield_port` | — | Ask this instance to shut down and free the OSC RX port. Honoured only by instances started with `--osc-yield` (a Studio-launched standby renderer); ignored otherwise, so an embedded (mpv) renderer can never be evicted. Sent automatically by a starting instance that finds the port busy. |

---

## State — engine → clients

`/state/options_schema` carries the declared live-options schema (JSON:
`[{key, kind, values?, default, flags, i18nKey, helpI18nKey?}]`), mirroring the
generator/phantom param-schema pattern; option values ride in the `options`
block of the renderer snapshot.

The full state snapshot is published as `/omniphony/state/renderer` (JSON);
individual deltas use the addresses below. `osc_contract::ALL_STATE` is the
exhaustive machine-readable list.

- **Snapshot / lifecycle** — `renderer` (full JSON), `snapshot_complete`,
  `capabilities`, `config/saved`, `config/save_error`, `shutdown` (goodbye
  broadcast on graceful engine teardown, one string arg with the reason;
  clients should treat the connection as gone and re-register with the next
  instance).
- **Render** — `render/version`, `render/config_path`, `render/config_status`,
  `render/bridge_path`, `render/bridge_error`, `vbap/allow_negative_z`,
  `render_evaluation/*` (mirrors of the control resolutions), `speakers`,
  `speakers/recomputing`, `speakers/recompute_error`, `layout`.
- **Head tracking** — `head_pose` (4-float quaternion `w,x,y,z`, broadcast at
  ~30 Hz while a tracking feed is active, for low-latency client display).
- **Metering / timing** — `clip`, `decode_time_ms`, `render_time_ms`,
  `write_time_ms`, `crossover_time_ms`, `frame_duration_ms`, `monitoring`,
  `loudness`, `realtime/{master_gain,object_gain,speaker_gain}`.
- **Latency & resampling** — `latency`, `latency_instant`, `latency_smoothed`,
  `latency_control`, `latency_target`, `latency_avail_input`,
  `latency_output_fifo`, `latency_resampler_pending`, `latency_downstream`,
  `resample_ratio`, `adaptive_resampling/state`, `adaptive_resampling/band`.
- **Input / config echoes** — `input`, `input_pipe`, `audio`, `log_level`.
- **OSC publication flags** — `osc/metering`, `osc/diag`.
- **Diagnostics** — `diag_schema`, `diag_values`.
- **Gain-table stream** — `debug/speaker_gaintable/{meta,chunk,uptodate,
  unavailable}`.

---

## Adding or changing an address

1. Add/rename the constant in `runtime_control/src/osc_contract.rs` (and to
   `ALL_CONTROL` / `ALL_STATE`).
2. Reference the constant from the dispatcher / producer instead of a literal.
3. Update this document.

The `osc_contract` test module guards the structural invariants (every control
const is a control address, every state const a state address, no duplicate wire
addresses).
