# Omniphony OSC control / state contract

The engine is driven and observed entirely over **OSC** (UDP). This document is
the human-readable contract for client authors (Omniphony Studio, alternative
front-ends, automation). The machine-readable single source of truth for the
address strings is the dependency-free
[`omniphony-osc-contract`](../osc-contract/src/lib.rs) crate (re-exported to the
engine as `runtime_control::osc_contract`, and used by Studio too) — every
address below has a named constant there, and the exhaustive lists are
`ALL_CONTROL`, `ALL_STATE` and `ALL_SESSION`. The [address index](#address-index)
at the end lists every one of them; a test in the contract crate fails when an
address is missing from it. Keep this document and that crate in sync.

## Directions

- **Control** — `/omniphony/control/…`, sent **client → engine** to change state
  or trigger an action.
- **State** — `/omniphony/state/…`, emitted **engine → subscribed clients** when
  something changes (and as snapshots on connect).

## Argument conventions

- **Booleans** are accepted as OSC `int` (`0`/non-zero), `float`, or `bool`; the
  engine coerces. Most togglish controls take a single int `0`/`1`.
- **Enums** are lowercase strings; an unrecognised value is refused, and the
  sender told so on `/state/control_error` (see
  [Session and reliability](#session-and-reliability)).
- **Nesting** is bounded: bundles may nest 8 deep, and so may arrays within a
  message's arguments. The engine and the Studio each drop a datagram that
  goes deeper, whole and before decoding it (logged as undecodable); the limit
  and the check are the contract crate's (`osc-contract`, module `nesting`),
  for any other listener to use. The engine's own bundles are one level deep
  and it sends no array.
- **Realtime gain** controls (`/control/realtime/*`) carry a trailing monotonic
  **sequence int** so the engine can drop stale updates that arrive out of order.
- Larger structured payloads (layout / speakers / audio / input config) are sent
  as a single **JSON string** argument.

## Datagram size

Every OSC packet travels as one UDP datagram, and some are large: a snapshot
bundle runs up to 65 000 bytes, and `/control/backend/file/put` and its
`/state/backend/file/content` reply carry a file of up to 60 000 bytes. A
client therefore needs to:

- **receive** into a buffer that fits any UDP payload (65 536 bytes): a shorter
  one truncates or loses the datagram;
- **raise its socket's send buffer** (`SO_SNDBUF`) to 65 536 bytes when it is
  lower, before sending a large message. macOS and the BSDs refuse a UDP send
  larger than that buffer (`EMSGSIZE`, "Message too long"), and it starts at
  `net.inet.udp.maxdgram`: 9 216 bytes. Only ever raise it: Linux starts
  higher, and setting it there would shrink it.

The engine and Studio do both on their own sockets.

## Notification

Every control write falls in one of the classes of
[`docs/persistence-policy.md`](persistence-policy.md), and the class decides
what it does to the Save button:

- **Render / engine** — a write that changes state the config file holds marks
  the config dirty and tells **every** registered client, not only the sender:
  `/state/config/saved` drops to `0` at once, and the new value reaches
  everyone through the live-state snapshot (`/state/renderer`,
  `/state/speakers`, …). Discrete edits send the snapshot right away;
  slider-driven ones (generator / phantom params, placement entries, realtime
  gains) let it ride the OSC loop's poll, at most one snapshot per 200 ms tick.
  The value reaches `config.yaml` only through `/control/save_config`.
- **View** — the monitoring cadences (`metering/rate_hz`, `diag/rate_hz`) and
  the head-tracker calibration: written to `config.yaml` at once, published,
  and the config stays clean. The mpv overlay's display switches are written
  to `overlay-prefs.conf` the same way.
- **Transient** — mutes, a manual head pose, test signals, subscriptions,
  actions: published when they carry state, never written, never dirtying.

Registry options are render state like any other. Profile operations never
commit unsaved edits either: a switch discards them unless it is sent with
`"save"`, create copies the live state into the new profile only, rename and
delete are bookkeeping. A targeted write never touches the other unsaved edits:
they stay pending, and a live-handoff sidecar holding them is amended rather
than discarded.

## Session and reliability

UDP loses datagrams and the engine answers nothing by default, so the session
carries what a client needs to notice either. The contract crate's
`CONTRACT_REVISION` (`osc-contract`) is the revision this section describes:
**5**.

- **Stream transport** (revision 2) — the engine also listens on TCP, on
  loopback, on the OSC/UDP control port's number. A connection carries the
  same packets, each preceded by its size as a big-endian int32 (OSC 1.0
  stream framing), up to 1 MiB each way. A connected client registers with
  `/omniphony/register` like a datagram client (its argument is ignored: the
  connection is the reply address) and sends no heartbeat: the connection is
  the session, and closing it unregisters the client. It is sent everything a
  datagram client is, in order and without loss, except telemetry and the log
  relay, which it loses as a datagram client would when it falls behind
  (8 MiB queued); a
  client too slow for the state is disconnected instead, and gets a fresh
  snapshot when it reconnects. Its snapshot comes in one part, and the
  gain-table stream in chunks of up to 512 KiB rather than 1 KiB (the meta
  says which); an HRTF upload may send chunks of the same size. An engine that
  cannot bind the port runs on datagrams only. See
  `docs/control-transport.md`.

- **Registration** — `/omniphony/register [reply_port]` registers the sender
  (the port is optional; the source port otherwise) and sends it the
  live-state snapshot, its log backlog and its metering state. A registered
  datagram client sends `/omniphony/heartbeat [reply_port]` every 5 s and is
  dropped after 15 s of silence.
- **Heartbeat acknowledgement** — `/omniphony/heartbeat/ack [epoch,
  generation]`, or `/omniphony/heartbeat/unknown` to a client the engine does
  not know (it re-registers). `epoch` is random per engine instance: when it
  changes, another engine answers on the port. `generation` is the state
  generation below. An engine before revision 1 sends `epoch` alone.
- **State generation** — `/omniphony/state/generation [generation, full,
  part, parts]`. The engine counts the control-plane state it publishes: every
  state update (`config/saved`, `log_level`, `speakers/recomputing`, a
  control's echo, `overlay`, a recompute's `renderer`/`layout`/`speakers`, …)
  travels in a bundle with the next count, `full = 0`, part 0 of 1. Every
  datagram of a snapshot opens on the count with `full = 1`, its index and the
  snapshot's datagram count (a broadcast snapshot advances the count, one sent
  to a single client does not). A client holding `g` expects `g + 1` next, and
  holds a snapshot's generation only once it has every part of it; anything
  else — a count that skips, a snapshot whose last part arrives with an
  earlier one missing, an acknowledgement reporting another count — means it
  missed something. It then sends `/omniphony/control/state/refresh
  [reply_port]`, which resends the snapshot to it and nothing else. The engine
  takes the count with the state it captures, under one lock, so a later count
  never carries an older state. Telemetry is not counted: the meter bundle
  (timings, latencies, the object test position), diagnostics, the head pose,
  the realtime gain echoes (sequenced on their own), the gain-table stream
  (versioned and resent on its own) and the object and meter streams. The
  count wraps; compare it for equality only.
- **Control errors** — a control the engine does not apply is answered, to
  its sender only, with `/omniphony/state/control_error [address, code,
  message]`. `code` is one of `unknown_address` (no handler knows it),
  `invalid_arguments` (its handler refused the arguments; a grouped
  `/control/options` write reports the pairs it dropped and applies the
  rest), `not_applied` (a catalogued address nothing on this engine took: its
  arguments were refused without a reason, or the host does not implement it,
  such as an audio-output control sent to an engine embedded in mpv) and
  `undecodable` (not OSC the engine can read; `address` is empty, and these
  are answered at most once per 5 s) and, from revision 3, `not_allowed` (a
  process-lifecycle control, `quit`, `yield_port` or `resume`, from another
  machine: the OSC/UDP socket listens on the network for head tracking and
  remote clients, but only a client on this machine may stop the engine or
  take its port). `message` is for a person. A control
  taken and found to change nothing is not answered.
- **Sync** (revision 2) — `/omniphony/sync [args…]` is answered with
  `/omniphony/sync/ack [args…]` (the same arguments) once every packet the
  client sent before it has been dispatched. On a stream connection, whose
  packets are handled in order, the ack is a dispatch barrier: each earlier
  control was applied (the state it changed published before the ack),
  refused (its `control_error` before the ack), or started asynchronous work.
  The ack says nothing about that work: it may have ended before the ack or
  end after it. A layout or speaker rebuild reports itself as it always has:
  `speakers/recomputing 1` then `speakers/recompute_error ""` when a build
  starts; `speakers/recompute_error <message>` on failure, then
  `speakers/recomputing 0`, when it ends. A change that arrives while a build
  runs queues one follow-up build, which starts right after the first one's
  `0`: a `0` says that a build ended, not that every earlier change is built.
  Over UDP the ack only says the engine heard the sync.
- **Evaluation grid** (revision 5) — the grid the gain table is sampled on
  follows the active bridge or is forced (option `evaluation_grid`,
  `docs/multi-bridge.md`). `/state/renderer` carries `evaluationGrid`
  (`bridge` or `custom`) and `evaluationGridBridge`, the grid the active
  bridge hints (`{mode, xSize, ySize, zSize, zNegSize, allowNegativeZ}`, null
  until known). While the grid follows the bridge, a write of a grid value
  (options flagged `bridge grid` below, their dedicated addresses included) is
  refused with `invalid_arguments` and a message naming the key and saying
  `the grid follows the bridge`; the other pairs of the same
  `/control/options` write still apply, and a write that forces the grid
  (`evaluation_grid custom`) lets the grid values after it through.
  `vbap_allow_negative_z` is a registry option, and
  `/state/vbap/allow_negative_z` is its live value (off by default; it said
  on before the renderer reported one).
- **Contract revision** — `/state/capabilities` carries `contractRevision`.
  A client compares it with its own and says so when they differ; an engine
  that advertises none predates revisions and counts as 0. The revision moves
  with any change to the wire surface: an address added, removed or renamed,
  or an address's arguments. The contract crate pins the address set to the
  revision, so a catalogue change without a bump fails its tests.
- **Argument shapes** — `osc_contract::shapes::STATE` gives the arguments of
  every state address. The engine's tests hold what it sends to it, and
  Studio's conformance test parses a message of every shape, so a state
  address Studio cannot read fails a test rather than being dropped at run
  time.

---

## Control — client → engine

### Spatialisation: spread & distance

The six `/control/spread/*` addresses are aliases over the generic backend
parameter setter: they write the `vbap` backend's param bag
(`/control/backend/param ["vbap", "spread_min", 0.2]` is the same write) and
rebuild the topology.

| Address | Args | Meaning |
|---|---|---|
| `/control/spread/min` | f `[0,1]` | Minimum effective spread. |
| `/control/spread/max` | f `[0,1]` | Maximum effective spread. |
| `/control/spread/from_distance` | int bool | Derive spread from distance instead of object size. |
| `/control/spread/distance_range` | f `>0` | Distance at which distance-derived spread reaches 0. |
| `/control/spread/distance_curve` | f `≥0` | Curve exponent for distance-derived spread. |
| `/control/spread/size_to_spread_mode` | s | `max` \| `mean` \| `projection_perpendicular`. |
| `/control/distance_model` | s | `none` \| `linear` \| `quadratic` \| `inverse-square`. Registry option `vbap_distance_model` (group `distance_model`). |
| `/control/distance_model_metric` | s | `spherical` \| `chebyshev`. Registry option `distance_model_metric` (group `distance_model`). |
| `/control/distance_diffuse/enabled` | int bool | Enable the mirrored distance-diffuse blend. Registry option `distance_diffuse` (group `distance_diffuse`). |
| `/control/distance_diffuse/threshold` | f `>0` | ADM distance at which the blend reaches 100 % direct. Registry option `distance_diffuse_threshold` (group `distance_diffuse`). |
| `/control/distance_diffuse/curve` | f `≥0` | Blend-weight curve exponent. Registry option `distance_diffuse_curve` (group `distance_diffuse`). |
| `/control/distance_diffuse/metric` | s | `spherical` \| `chebyshev`. Registry option `distance_diffuse_metric` (group `distance_diffuse`). |
| `/control/distance_diffuse/mirror_axes` | s | ADM axes negated to build the mirror image: any combination of `x`, `y`, `z` (`xy` — the default half-turn about the vertical axis — `y` for a front/back reflection, `xyz` for an inversion through the origin), or `none`. Registry option `distance_diffuse_mirror_axes` (group `distance_diffuse`). |
| `/control/room_ratio` | f×3 `[0.01,100]` | Room proportions `[w, l, h]` used to scale ADM coords. Registry option alias (group `room`). |
| `/control/room_ratio_rear` | f `[0.01,100]` | Rear scaling factor. Registry option alias (group `room`). |
| `/control/room_ratio_lower` | f `[0.01,100]` | Lower-hemisphere scaling factor. Registry option alias (group `room`). |
| `/control/room_ratio_center_blend` | f `[0,1]` | Centre-blend factor. Registry option alias (group `room`). |

### Render backend selection & parameters

| Address | Args | Meaning |
|---|---|---|
| `/control/render_backend` | s | Select active backend by id (built-in or contributor). Registry option `render_backend` (group `backend`). |
| `/control/render_backend/restore` | — | No longer supported: logged and ignored. |
| `/control/backend/param` | `[key, value]` or `[backend_id, key, value]` | Generic backend parameter setter (schema-driven). With an explicit backend id, targets that backend (e.g. a hybrid inner backend); otherwise the selected one. |
| `/control/hybrid/external_backend` | s | Hybrid outer backend id. Registry option `hybrid_external_backend` (group `backend`). |
| `/control/hybrid/internal_backend` | s | Hybrid inner backend id. Registry option `hybrid_internal_backend` (group `backend`). |
| `/control/hybrid/metric` | s | `spherical` \| `chebyshev`. Registry option `hybrid_metric` (group `backend`). |
| `/control/hybrid/curve_smoothing` | f `[0,1]` | Blend-curve smoothing. Registry option `hybrid_curve_smoothing` (group `backend`). |
| `/control/hybrid/curve` | f×2N | Flattened `(x,y)` blend control points, each `[0,1]`. Not a registry option (a point list). |
| `/control/backend/file/get` | backend_id s, key s, name s?, request_id s? | Read an editable backend file (e.g. the scriptable backend's `.lua`) — `name` from the managed store, else the param's current handle. Replies point-to-point on `/state/backend/file/content` or `/state/backend/file/error`. |
| `/control/backend/file/list` | backend_id s | List the managed store's files; replies `/state/backend/file/list` `[backend_id, json array]`. |
| `/control/backend/file/put` | backend_id s, key s, name s, content s, request_id s? | Write the file (≤ 60 000 bytes), set the param to its handle and rebuild the backend; replies `/state/backend/file/content` as the save ack. An absolute path is only honoured from a loopback client. |

A client advertising `fileRequestIds` gets its optional `request_id` echoed as
the last argument of the reply; older clients omit it and get the original
payload shape.

### Render evaluation (precomputed tables)

| Address | Args | Meaning |
|---|---|---|
| `/control/render_evaluation_mode` | s | `auto` \| `realtime` \| `precomputed_polar` \| `precomputed_cartesian`. Registry option `render_evaluation_mode` (group `evaluation`). |
| `/control/render_evaluation_mode/from_file` | — | No longer supported: logged and ignored. |
| `/control/render_evaluation/position_interpolation` | int bool | Nearest-cell vs trilinear table lookup. Registry option `render_evaluation_position_interpolation` (no group: read at lookup time, no rebuild). |
| `/control/render_evaluation/cartesian/{x_size,y_size,z_size,z_neg_size}` | int `≥1` | Cartesian table resolution per axis (a config may set `z_neg` to 0; a control write is floored at 1, as it always was). Registry options `evaluation_cartesian_{x,y,z,z_neg}_size` (group `evaluation`). |
| `/control/render_evaluation/polar/azimuth_resolution` | int `≥1` | Azimuth cells. Registry option `vbap_azimuth_resolution` (group `evaluation`). |
| `/control/render_evaluation/polar/elevation_resolution` | int `≥1` | Elevation cells. Registry option `vbap_elevation_resolution` (group `evaluation`). |
| `/control/render_evaluation/polar/distance_res` | int `≥1` | Distance cells. Registry option `vbap_distance_res` (group `evaluation`). |
| `/control/render_evaluation/polar/distance_max` | f `>0` | Max table distance. Registry option `vbap_distance_max` (group `evaluation`). |
| `/control/render_evaluation/object_size_intervals` | int `≥0` | Object-size interval count of the precomputed tables (`0` = off). Registry option `evaluation_object_size_intervals` (group `evaluation`). |

### Gain, mute & loudness

| Address | Args | Meaning |
|---|---|---|
| `/control/realtime/master_gain` | f `≥0` (linear), seq int | Master gain; echoed on `/state/realtime/master_gain`. Studio sends `[0,2]`. |
| `/control/realtime/speaker_gain` | id int, f `≥0` (linear), seq int | Per-speaker output gain; echoed on `/state/realtime/speaker_gain`. Saved as the layout speaker's `gain_db` (0.1 dB), which seeds it at the next start. |
| `/control/gain` | f `≥0` (linear) | Master gain without a sequence number (scripts). Same field as the realtime address. Registry option `master_gain` (the file stores dB). |
| `/control/object/{id}/mute` | int bool | Per-object mute. Transient: never saved. |
| `/control/config/speakers` | json | Speaker edits: delay (saved) and mute (transient, never saved). |
| `/control/loudness` | int bool | Dialogue-norm / loudness correction. Registry option alias (`use_loudness`). |
| `/control/auto_gain` | int bool | Auto gain-reduction on clipping. Registry option alias. |
| `/control/auto_gain_ceiling` | f `[-12,0]` dB | Auto-gain target ceiling. Registry option alias (`auto_gain_ceiling_db`). |

### Adaptive resampling (output clock servo)

All under `/control/adaptive_resampling/…`. Master toggle: bare
`/control/adaptive_resampling` (int bool). Tunables: `kp_near`, `ki`,
`max_adjust`, `update_interval_callbacks`, `high_recover_entry_margin_ms`,
`integral_discharge_ratio`, `near_far_threshold_ms`, `reset_ratio` and `pause`
(both transient: an action and a diagnostic hold, never saved),
and the far-mode group `enable_far_mode`, `force_silence_in_far_mode`,
`hard_recover_high_in_far_mode`, `hard_recover_low_in_far_mode`,
`far_mode_return_fade_in_ms`. `/control/latency_target` sets the target buffer
latency. All but `reset_ratio` and `pause` are host options of the
`adaptive_resampling` group (`latency_target` of `audio_output`), keyed by
their config key (`enable_adaptive_resampling`, `adaptive_resampling_kp_near`,
…); the low-recover and smoothing tunables, which have no address of their
own, are reached through `/control/config/audio` or `/control/option(s)`. See
`PI_TUNING_PROCEDURE.md` and `docs/latency-regulation.md`.

### Audio output & live input

Declared by the standalone renderer's host (`host_audio`), as host options
of three groups — `audio_output` (live, restarts the output), 
`adaptive_resampling` (live) and `live_input` (**staged**, restarts the input
when applied) — so every one is also reachable through `/control/option(s)`
under its `render.*` config key (`output_device`, `output_sample_rate`,
`adaptive_resampling_kp_near`, `live_input_channels`, …), with its schema in
`/state/options_schema` and its values in `/state/host_options` (see [Live
options](#live-options)). The embedded engine declares none of them. The
JSON patches below are aliases of a batch of these options: same fields,
same wire format; `null` unsets a nullable field.

| Address | Args | Meaning |
|---|---|---|
| `/control/config/audio`, `/control/config/audio/apply` | json | Audio output and adaptive-resampling config as one batch. The apply is an alias of `/control/options/apply audio_output`: the output applies its values as they arrive, so it only acknowledges. |
| `/control/audio/output_device` | s | Select output device. Host option `output_device`. |
| `/control/audio/output_backend` | s | Requested output backend (`pipewire`, `asio`, `file`, …; `""` = platform default). Takes effect on the next output (re)start. Host option `output_backend`. On Windows, `asio` falls back to WASAPI shared mode when ASIO has no output device or the requested device is a WASAPI one; the `outputHost` field of `/state/audio` names the host the open stream plays through (`ASIO`, `WASAPI (fallback: no ASIO driver)`, `CoreAudio`; `""` when none is open or the backend names none). |
| `/control/audio/output_file` | s | Destination for the `file` backend (`-`, a path or a FIFO; `""` = unset). Host option `output_file`. |
| `/control/audio/output_file_format` | s | Container/format for the `file` backend (`""` = unset). Host option `output_file_format`. |
| `/control/audio/output_devices/refresh` | — | Re-enumerate output devices. |
| `/control/audio/sample_rate` | int | Output sample rate (≤ 0 = the device's). Host option `output_sample_rate`. |
| `/control/config/input`, `/control/config/input/apply`, `/control/input/apply` | json | Live-input config as one batch of staged values; the two applies are aliases of `/control/options/apply live_input`. An apply is an action: it publishes the new state and lights no Save (the staged writes already did). |
| `/control/input/mode` | s | Input source mode. Host option `input_mode` (staged). |
| `/control/input/refresh` | — | Re-enumerate input sources. |
| `/control/input/drc_mode` | s | Dynamic-range-control mode (one of the bridge's `supportedDrcModes`). Registry option alias. |
| `/control/input/drc_weight` | f `[0,1]` | DRC weight. Registry option alias. |
| `/control/option dialogue_gain_db <f>` | f `[-12,12]` | Level of the channels the bridge tags as dialogue (`channelTags` on `/state/input`: `[{kind, language, label, channels}]`, empty when the stream tags nothing). Registry option, no alias. |
| `/control/input/live/{backend,node,description,layout,layout_import,channels,sample_rate,clock_mode,map,lfe_mode}` | varies | Live-capture parameters, staged. `backend` accepts only `pipewire`; the retired `asio` value (never implemented) and any other value are rejected with a warning, leaving the staged backend unchanged. All but `layout_import` (an imported layout, structured) are host options `live_input_*`; a non-positive `channels` / `sample_rate` sent here is ignored, as before (through `/control/option(s)` it unsets the value, as the JSON patch does). |
| `/control/render/bridge_path` | s | Path to one decoder bridge library (`""`: auto-discovery). |
| `/control/render/bridge_paths` | s… | The decoder bridge libraries, in load order, one argument each (none: auto-discovery). Saved as `render.bridge_path(s)`; takes effect on restart or `reload_config`. |
| `/control/render/input_pipe` | s | Named-pipe input path. |

### Head tracking (binaural)

Live head-pose control for the binaural (headphone) path. The orientation
*feed* itself arrives on a **user-configured** address
(`head_tracking.osc_address`, e.g. `/gamerotationvector` for Sensors2OSC /
`nxosc`) parsed per `head_tracking.format` — that is config, not a fixed
contract address. See `omniphony-renderer/BINAURAL.md`.

| Address | Args | Meaning |
|---|---|---|
| `/control/head/orientation` | f×3 (euler °) | Set head pose directly (yaw, pitch, roll). Transient, like the tracker feed. |
| `/control/head/quat` | f×4 | Set head pose directly (quaternion). Transient. |
| `/control/head/recenter` | — | Capture the current orientation as "front" (persisted to `config.yaml` right away). |
| `/control/head/calibrate` | s | Three-pose sensor-axis calibration, one step per message: `front` (also recenters), `left`, `up`, or `reset`. The result is persisted right away. |
| `/control/head/tracking/address` | s | Feed address the engine listens on (`""` disables tracking). Registry option `head_tracking_osc_address` (group `head_tracking`). |
| `/control/head/tracking/format` | s | `auto` \| `quat` \| `rotvec` \| `euler`. Registry option `head_tracking_format` (group `head_tracking`). |
| `/control/head/tracking/smoothing` | f `[0,0.999]` | Pose smoothing (higher = smoother/laggier). Registry option `head_tracking_smoothing` (group `head_tracking`). |
| `/control/head/tracking/invert` | int bool | Mirror the applied rotation. Registry option `head_tracking_invert` (group `head_tracking`). |

### Binaural (headphone) stage

See `omniphony-renderer/BINAURAL.md` for what each stage does. Values out of
range are clamped (an ear gain out of range is dropped instead); non-finite
values are dropped.

| Address | Args | Meaning |
|---|---|---|
| `/control/output_mode` | s | `speaker` (render to the layout) \| `binaural` (stereo for headphones). Registry option `output_mode`. |
| `/control/binaural_mode` | s | `direct` (one HRIR pair per object) \| `cascaded` (pan onto a virtual layout, binauralise its speakers). Registry option `binaural_mode`. |
| `/control/binaural/hrir_source` | s | `synthetic` \| `saf_kemar` \| `sofa[:<path>]` \| `brir[:<path>]` \| `pinna[:<preset>:<d_scale %>:<depth %>]` \| `prtf[:<freq_scale %>:<depth %>]`. Registry option `hrir_source` (group `hrir_source`). With `brir` on the headphones, once the set is loaded the render pans onto the set's own loudspeakers (one per emitter, plus a direct `LFE`; no per-speaker gain, mute, delay or band) instead of the editable layout, which is kept as is: `/state/renderer` carries them as `binaural.brir.layout` (layout-state shape, `null` otherwise) and `binaural.brir.layoutError` says why a set's loudspeakers cannot be used. The render then pans in the set's measured room rather than the user's room ratio (#803): the loudspeakers are placed as fractions of the box the file states (`RoomCornerA`/`RoomCornerB`, grown to hold them) or, without one, of a box estimated around them, and `binaural.brir.room` publishes that room (`boxM`, `estimated`, and `ratio` in the `roomRatio` shape with `scaleM` the metres to one unit; `null` with `layout`). A `room_ratio` write changes nothing of that render. |
| `/control/binaural/hrtf_upload/begin` | name s, total_bytes int | Start uploading a SOFA file (≤ 1 GiB; one upload at a time). |
| `/control/binaural/hrtf_upload/chunk` | index int, blob | One chunk, in order. |
| `/control/binaural/hrtf_upload/end` | chunk_count int | Finish: the file is written to `hrtf/` next to the default config file and selected as the `sofa` source. |
| `/control/binaural/unit_scale` | f `[0.01,100]` m | Metres per ADM unit. Registry option `binaural_unit_scale_m`. |
| `/control/binaural/head_radius` | f `[0.05,0.15]` m | Head radius for the ITD model. Registry option `binaural_head_radius_m`. |
| `/control/binaural/ear_gain` | ear int (`0` L, `1` R), f `[0,4]` | Headphone output gain per ear. Hand-wired (one ear by index); both ears at once is the registry option `binaural_ear_gains`. |
| `/control/binaural/ear_mute` | ear int, int bool | Headphone mute per ear. |
| `/control/binaural/reflections/enabled` | int bool | Early reflections of the virtual room. Registry option `reflections_enabled`. |
| `/control/binaural/reflections/level` | f `[0,1]` | Reflection level relative to the direct sound. Registry option `reflections_level`. |
| `/control/binaural/reflections/wall_cutoff` | f `[1000,20000]` Hz | Wall absorption low-pass. Registry option `reflections_wall_cutoff_hz`. |
| `/control/binaural/reflections/{room_width,room_depth,room_height}` | f `[1,20]` m | Virtual room size. Registry options `reflections_room_{width,depth,height}_m`. |
| `/control/binaural/reverb/enabled` | int bool | Late reverb. Registry option `reverb_enabled`. |
| `/control/binaural/reverb/level` | f `[0,1]` | Reverb level. Registry option `reverb_level`. |
| `/control/binaural/reverb/rt60` | f `[0.1,3]` s | Decay time. Registry option `reverb_rt60_s`. |
| `/control/binaural/reverb/predelay` | f `[0,100]` ms | Pre-delay. Registry option `reverb_predelay_ms`. |
| `/control/binaural/reverb/size` | f `[0.5,2]` | Room-size factor. Registry option `reverb_size`. |
| `/control/binaural/reverb/{rt60_low_ratio,rt60_high_ratio}` | f `[0.25,4]` | Low / high band decay relative to `rt60`. Registry options `reverb_rt60_{low,high}_ratio`. |
| `/control/binaural/diffuse_field_eq` | int bool | Diffuse-field equalisation of the HRIR set. Registry option `binaural_diffuse_field_eq`. |
| `/control/binaural/air_absorption` | int bool | Distance-dependent air absorption. Registry option `binaural_air_absorption`. |
| `/control/binaural/brir/head_tracking` | int bool, or `auto` | Which measured head orientations of a room response stay resident: all (`1`), front only (`0`), or `auto` (all when a head-tracking address is set). Registry option `brir_head_tracking` (`auto` \| `on` \| `off`; group `brir`). |
| `/control/binaural/brir/max_length` | f `[0,10]` s | Truncate the room response (`0` = whole). Registry option `brir_max_length_s` (group `brir`). |
| `/control/binaural/brir/tail_floor` | f `[20,120]` dB | Cut the tail this far below the response's energy. Registry option `brir_tail_floor_db` (group `brir`). |
| `/control/binaural/hrir_update_lattice` | s | `exact` \| `fine` \| `balanced` \| `coarse` — how far an object must turn before its HRIR is rebuilt. Registry option alias (see [Live options](#live-options)). |

### Fixed-channel sources

Channel-based content: placement of the fixed channels, and the synthesized
objects stages (height generator, phantom extraction).

| Address | Args | Meaning |
|---|---|---|
| `/control/synthetic_objects` | int bool | Master switch of every synthesized-object stage (keeps the child selections). Registry option alias. |
| `/control/object_generator` | s | Bed→height generator id (`""`/`none` = off). Schema on `/state/object_generators`. Registry option alias. |
| `/control/object_generator/param` | [generator s,] key s, value (f/i/T-F/s) | One generator parameter (keys from the listing), for the selected generator or the one named. Read in the declared type (a number for a switch: on at `>= 0.5`). Not persisted until Save. See `docs/plugin-contract.md`. |
| `/control/phantom_extract` | s | `off` \| `broadband` \| `spectral` (legacy int `0`/`1` = off/broadband). Registry option alias. |
| `/control/phantom_extract/param` | key s, value (f/i/T-F/s) | One phantom-extraction parameter (listing on `/state/phantom`), read in the declared type. Not persisted until Save. |
| `/control/surround_placement` | s | `side` \| `back`: where a 4.x/5.x surround pair goes when there are no back channels. Registry option alias. |
| `/control/output_channel_mapping` | s | `by_index` \| `by_name`: how output channels map to device ports. Registry option alias. |
| `/control/placement/mode` | family s, mode s | Placement mode of one source family (`generic`, `dolby`, `dts`, `auro`, `pcm`): `sphere` \| `room` \| `manual`, or `inherit`. Re-plans the stream. |
| `/control/placement/layout` | family s, yaml s | One family's own entries (a YAML `SpeakerLayout`; `""` clears). Re-plans the stream. |
| `/control/virtual_bed` | yaml s | Legacy: the `generic` family's entries. |

### Speaker stage & engine

| Address | Args | Meaning |
|---|---|---|
| `/control/crossover_type` | s | `lr4` (IIR, zero latency) \| `fir` (linear phase, constant latency). Registry option alias. |
| `/control/crossover_fir_transition_ratio` | f `[0.05,2]` | FIR transition width relative to the lowest cutoff. Registry option alias. |
| `/control/decode_thread` | int bool | Decode on a thread of its own in the liborender engine, when its host lets the option decide. Registry option alias. |

### Live options

`/control/option [key (string), value]` sets any option declared in the
`renderer::options` registry (schema on `/state/options_schema`, values in the
`options` block of `/state/renderer`). `value` is as many arguments as the
option's kind takes: one, or `len` numbers for a `float_array` option such as
`room_ratio`; anything after it is ignored. Every option waits for
`/control/save_config`, and a set marks the config dirty only when the value
actually changed. Every dedicated address marked "Registry option" above
is an alias of it, with the same arguments and bounds; the keys are the
`render.*` config keys (`vbap_distance_model` for `/control/distance_model`,
`evaluation_cartesian_x_size` for `…/cartesian/x_size`, …) and the schema
lists them all. An option of kind `int` takes a number rounded to the
nearest integer; a `dynamic_enum` takes one of the ids of the set its
`source` names (`backends`: `renderBackendState.available_backends` in
`/state/renderer`). See `docs/live-options-registry.md`.

The declared options (generated from the registry; the alias is the
dedicated address, under `/omniphony`):

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
| `evaluation_grid` | `bridge` \| `custom` | `"bridge"` | `evaluation` (live, evaluation) | — | — |
| `render_evaluation_mode` | `auto` \| `realtime` \| `precomputed_polar` \| `precomputed_cartesian` | `"auto"` | `evaluation` (live, evaluation) | bridge grid | `/control/render_evaluation_mode` |
| `evaluation_object_size_intervals` | int ≥ 0 | `0` | `evaluation` (live, evaluation) | — | `/control/render_evaluation/object_size_intervals` |
| `evaluation_cartesian_x_size` | int ≥ 1 | as built | `evaluation` (live, evaluation) | bridge grid | `/control/render_evaluation/cartesian/x_size` |
| `evaluation_cartesian_y_size` | int ≥ 1 | as built | `evaluation` (live, evaluation) | bridge grid | `/control/render_evaluation/cartesian/y_size` |
| `evaluation_cartesian_z_size` | int ≥ 1 | as built | `evaluation` (live, evaluation) | bridge grid | `/control/render_evaluation/cartesian/z_size` |
| `evaluation_cartesian_z_neg_size` | int ≥ 0 | as built | `evaluation` (live, evaluation) | bridge grid | `/control/render_evaluation/cartesian/z_neg_size` |
| `vbap_allow_negative_z` | bool | as built | `negative_z` (live, topology) | bridge grid | — |
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

The standalone renderer's host declares its own options (audio output,
adaptive resampling, live input — see [Audio output & live
input](#audio-output--live-input)): the same setters take their keys, their
schema entries follow the core's in `/state/options_schema`, and their values
go out in `/state/host_options` (`{"options": {key: requested}, "applied":
{key: in force}, "pending": {group: bool}}`) — the `/state/renderer`
`options` block keeps the core's. A host with audio I/O leaves out the
options only the embedded engine offers (`decode_thread`, flagged
`embedded_only`): not in its schema, a write refused, a save keeps the
file's value.

The host's options (standalone renderer):

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

`/control/options/apply [group]` applies a group: a `staged` group
(`live_input`) hands over every value staged since its last apply; a `live`
group has nothing waiting and is only acknowledged.

`/control/options [key, value, key, value, …]` sets several at once. Every
valid pair is applied before anything is rebuilt, and the whole message costs
at most one rebuild (the widest its options' groups ask for: an `evaluation`
change re-samples the tables and keeps the gain models, a `room`, distance or
`backend` change rebuilds the topology; the binaural groups — `hrir_source`,
`brir`, `crossover` — reload in their own stage and `head_tracking` needs
nothing) and one live-state bundle. `binaural_ear_gains` (both ears) has no
dedicated address and is set through these two setters only. An unknown key or a
truncated value drops the whole message — past it, where the next key starts
is unknowable; an invalid value drops only its own pair. A change of an
option whose group asks for no rebuild is read where it is used, as with
`/control/option`.

### Config profiles

Named profiles (`docs/config-profiles.md`). Every mutation writes the profile
list to `config.yaml` and re-broadcasts `/state/profiles` and the live-state
snapshot, but none commits the user's unsaved edits
(`docs/persistence-policy.md`).

| Address | Args | Meaning |
|---|---|---|
| `/control/profile/switch` | name s, mode s? | Switch to a profile (refused if its layout file is missing). The outgoing profile's unsaved edits are discarded, unless `mode` is `"save"`: then they are saved into it first (the full Save), and a failed save cancels the switch. |
| `/control/profile/create` | name s | Create a profile from the current state, unsaved edits included; the active profile's file and unsaved state are untouched. |
| `/control/profile/delete` | name s | Delete a profile. Unsaved edits stay pending. |
| `/control/profile/rename` | old s, new s | Rename a profile. Unsaved edits stay pending. |

### Layout

| Address | Args | Meaning |
|---|---|---|
| `/control/config/layout`, `/control/config/layout/apply` | json | Layout config (stage / apply). |
| `/control/layout/radius_m` | f | Layout radius (m). |
| `/control/layout/export` | s (optional name) | Export the current layout. |

### Speaker test signal

| Address | Args | Meaning |
|---|---|---|
| `/control/speaker_test` | idx int, level f `[0,1]`, isolation s | Play band-limited pink noise on one speaker; negative idx stops. |
| `/control/object_test` | on int bool, x f, y f, z f `[-1,1]`, level f `[0,1]`, size f `[0,1]`, isolation s | Play pink noise as an object at a position, panned by the active backend; `on = 0` stops. |
| `/control/object_test/rotation` | axis s (`x`\|`y`\|`z`\|`free`), diameter f `[0,2]`, period_s f, azimuth f, elevation f | Orbit the object test around its placed position; diameter `0` stops it. |
| `/control/object_test/clip` | path s | Choose the WAV file the `clip` signal plays (`""` clears). Loaded once, on the control thread; the result comes back on `/state/object_test/clip`. |
| `/control/speaker_test/idle_feed` | int bool | Keep the output chain warm so a test is heard immediately. Serves both tests. |

`isolation` is one of `test_only` (mute the programme on that speaker only),
`with_programme`, or `test_only_solo` (mute every other speaker).

**`level` is a peak, not an RMS.** The engine bounds the injected signal to
`±level`, so `1.0` is exactly full scale and no accepted level can make the test
clip on its own; Studio's slider reads the same figure in peak dBFS. Pink noise
has a crest factor around 13 dB, so the signal *sounds* about that much quieter
than the number suggests — a test at 0 dBFS peak averages near -13 dBFS.

Two limits of that bound are deliberate. It covers the test's own contribution,
so `with_programme` can still clip against a loud mix. And the level is
referenced at the injection point, which sits before per-speaker and master gain
— the test is scaled by those exactly like programme audio, because the point is
to hear what the speaker will really do.

Reading the level as RMS is the bug this contract exists to prevent: applied
directly to the unit-RMS generator, a -6 dBFS test measured peaks near
+6 dBFS on a 7.1.4 render, and nothing reported it because a running test
suppresses peak tracking so it cannot drive the auto-gain.

The trigger policy (hold, fixed burst, toggle) belongs to the client; the engine
only ever hears "start this" or "stop", and keeps a safety cap so a client that
dies mid-test cannot leave a speaker making noise. All three addresses are
transient: never persisted, cleared on a fresh start.

#### The object test

The complement to the speaker test. A speaker test asks *what does this speaker
do*; an object test asks *where does the renderer put a source I place here*.
So it is not written into a channel — it is **panned**, by asking whichever
backend is live for the per-speaker gains at that ADM position and mixing
`noise × gain[s]` into every speaker. What you hear is therefore what the
renderer would do with a real object there, including the out-of-hull mode, the
distance model and the spread currently configured. Nothing about it is
VBAP-specific; a contributed backend works without knowing the feature exists.

Position is ADM Cartesian — x left/right, y back/front, z floor/ceiling, each in
`[-1, 1]` — and `size` is an isotropic extent, `0` being a point source.
`level` carries exactly the same peak contract as above; the bound survives
panning because backend gains are power-normalised (`Σ g² = 1`, so every
`g ≤ 1`), and clamping the mono noise before it is panned bounds every speaker's
share of it.

**Re-sending with a new position is how the object moves, and it must not
restart the signal.** The engine keeps position out of the generator's identity
and ramps the gains instead — the same per-sample interpolation `RampMode::Interp`
uses for a real object — so a stream of these messages, one per pointer move
while dragging, slides the source continuously with no gap and no click. Level
and isolation *do* restart it, deliberately: those are a "try that again", and
the ear expects the clock to restart with them.

`test_only` and `test_only_solo` mean the same thing here (silence the programme
everywhere): an object has no single speaker to solo. When both tests run at
once the speaker test's isolation wins on the speaker it targets, being the
narrower statement.

##### The orbit

`/control/object_test/rotation` turns the source around the position it was
placed at, in the plane perpendicular to `axis`. The two angles are read only
for `free`, where they give the axis direction in the usual ADM convention.
Diameter is in ADM units, `0` stops it, and there is deliberately no separate
on/off flag.

**The renderer advances the phase, not the client.** The whole point of this
test is to judge how smoothly the panning moves; a client stepping the angle
over OSC would hand that judgement to its own UI thread's worst moment, and one
long layout or a throttled timer would produce a stutter the listener would
blame on the renderer. Phase advances on the block clock, sampled once per block
at its midpoint.

Moving the source while it orbits slides the circle's centre and leaves the
phase alone, so a drag does not snap the source back to the start of its turn —
the same principle that keeps the noise from restarting.

The circle is **clamped per axis to the room**, which changes its shape rather
than its motion. Clamping acts on each axis separately, so a circle centred near
a wall keeps sweeping the axes that still fit: it becomes a D, running straight
along the wall for that part of the turn instead of arcing through it. On a
circle of diameter 2 centred at `x = 0.9`, 47% of the turn runs along the wall
and the source never stops moving. The alternative — shrinking the diameter
until the circle fits — would quietly hand back a smaller circle than the one
asked for.

The safety cap is unchanged and still applies: an orbit is not proof that anyone
is listening, so an unattended rotating test stops with all the others.

Unlike a speaker test, an object test works in `output_mode: binaural`: it is an
object, so it renders through the HRIR path — direction, ITD, air absorption,
early reflections and reverb send — from a source slot of its own past the input
channels. In cascaded binaural it is panned onto the virtual layout and
binauralised with it, so it hears the virtual room rather than bypassing it.

### Overlay (Studio 3D / mpv overlay)

`/control/overlay/{enabled,labels,objects,trails,tag,heatmap_enabled,
heatmap_bands,heatmap_colormap,heatmap_custom_stops}` — visualisation toggles
and heatmap configuration.

### Diagnostics & engine lifecycle

| Address | Args | Meaning |
|---|---|---|
| `/control/metering` | int bool | Subscribe (or not) the sending client to the meter stream; acknowledged on `/state/osc/metering`. |
| `/control/metering/rate_hz` | f `[1,1000]` | Metering publication rate. View state: written to `config.yaml` at once, never dirties the config; resending the current value does nothing. |
| `/control/diag/rate_hz` | f `[1,1000]` | Diagnostics publication rate. View state, like the metering rate. |
| `/control/diag/enabled` | int bool | Subscribe (or not) the sending client to diagnostics; acknowledged on `/state/osc/diag`. |
| `/control/debug/speaker_gaintable/subscribe` | have_version int, speaker int | Subscribe to a speaker's gain-table field. |
| `/control/debug/speaker_gaintable/unsubscribe` | — | Release the gain-table subscription. |
| `/control/debug/speaker_gaintable/nack` | … | Request missing chunks / version. |
| `/control/log_level` | s | `off`\|`error`\|`warn`\|`info`\|`debug`\|`trace`. |
| `/control/ramp_mode` | s | Object-transition ramp: `off` \| `frame` \| `interp` \| `sample`. Registry option alias. |
| `/control/option` | s key, value | Generic setter for any declared live option — see [Live options](#live-options). |
| `/control/options` | (s key, value)… | Grouped setter: several declared live options applied at once, one rebuild and one notification — see [Live options](#live-options). |
| `/control/options/apply` | s group | Apply a group of declared options (a `staged` group's staged values; a `live` group is acknowledged) — see [Live options](#live-options). |
| `/control/save_config` | — | Persist the current config. |
| `/control/reload_config` | — | Discard the live state (including a handoff sidecar) and reload config from disk. The CLI renderer restarts its pipeline; an embedded (mpv) renderer re-applies the config in place — layout, live params, active profile — while host-owned fields (output device, live input, bridge path) wait for the next engine start. |
| `/control/restart` | — | Restart the render pipeline keeping the unsaved live state, which comes back unsaved (it rides the live-handoff sidecar). For a change only a restart applies, such as a new bridge. CLI renderer only; an embedded renderer ignores it. |
| `/control/quit` | — | Shut the engine down. |
| `/control/yield_port` | — | Ask this instance to free the OSC RX port. Honoured only by instances started with `--osc-yield` (a Studio-launched standby renderer); ignored otherwise, so an embedded (mpv) renderer can never be evicted. Sent automatically by a starting instance that finds the port busy. The instance replies `/omniphony/yield/resume_port [port]` and stands by. |
| `/control/resume` | — | Sent to a standing-by instance, on the resume port it advertised, to re-acquire the OSC port and audio. |
| `/control/state/refresh` | int reply port (optional) | Resend the live-state snapshot to the sender, and nothing else: for a client whose state generation fell behind (see [Session and reliability](#session-and-reliability)). |

`/control/unknown` is a sentinel for tests (an address no handler takes).

---

## State — engine → clients

`/state/options_schema` carries the declared live-options schema (JSON:
`[{key, kind, values?, default, flags, i18nKey, helpI18nKey?}]`), mirroring the
generator/phantom param-schema pattern; option values ride in the `options`
block of the renderer snapshot.

`/state/host_options` carries the options the standalone renderer's host
declares (JSON: `options` — the requested values —, `applied` — the values in
force, for a `staged` group's options that report one —, `pending` — per
`staged` group, whether it holds values not applied yet). Sent with every
live-state bundle by a host that declares options; the embedded engine sends
none.

The full state snapshot is published as `/omniphony/state/renderer` (JSON);
individual deltas use the addresses below. `osc_contract::ALL_STATE` is the
exhaustive machine-readable list.

- **Snapshot / lifecycle** — `renderer` (full JSON), `snapshot_complete`,
  `generation` (the state count, see
  [Session and reliability](#session-and-reliability)), `capabilities`
  (including `contractRevision`), `control_error` (to the sender of a control
  that was not applied), `config/saved`, `config/save_error`, `shutdown` (goodbye
  broadcast on graceful engine teardown, one string arg with the reason;
  clients should treat the connection as gone and re-register with the next
  instance). The snapshot travels as one OSC bundle, or as several
  consecutive bundles when it would not fit a UDP datagram (65 000 bytes);
  `snapshot_complete` is always its last message, so a client acts on that
  marker, never on the bundle boundary.
- **Render** — `render/version`, `render/executable` (path of the process
  serving the engine), `render/abi` (C-ABI `major.minor` of the liborender
  shim, `""` for the CLI), `render/bridge_api` (the `bridge_api` version the
  engine was built against; a decoder bridge loads only if built against the
  same minor), `render/config_path`, `render/config_status`
  (`loaded`, `missing`, `parse_error` — running on built-in defaults — or
  `newer_schema` — written by a newer build, read as far as this one
  understands it and never written; `""` without a config path),
  `render/bridge_path` (the first bridge asked for), `render/bridges` (JSON:
  `requested`, the bridge paths asked for, empty for auto-discovery, and
  `bridges`, each bridge loaded with its `path` and the source `families` it
  declares, then each one asked for or found that did not load, with its
  `path` and `error`), `render/bridge_error` (set only when no bridge loaded;
  bounded to 2 KB: the first
  line and the distinct verdicts of a plugin load failure, the full report
  stays in the renderer log; it contains `no decoder bridge found`
  (`BRIDGE_ERROR_NONE_FOUND`) when none was asked for and auto-discovery found
  none, a normal state for a standby renderer, and anything else is a failed
  load), `vbap/allow_negative_z` (the live value the gain models are built
  with, see the option),
  `render_evaluation/*` (mirrors of the control resolutions), `speakers`,
  `speakers/recomputing`, `speakers/recompute_error`, `layout`.
- **Schemas & profiles** — `options_schema`, `object_generators` (the height
  generators' plugin listings: id, label, `ParamSpec` params — the format of
  `availableBackends`), `phantom` (the phantom-extraction stage's listing),
  `profiles` (`{"active", "names"}`). See `docs/plugin-contract.md`.
- **Overlay** — `overlay` (display preferences as JSON, republished whenever
  they change, including from mpv keybinds).
- **Object test** — `object_test/position` (`x, y, z, peak dB, rms dB` of the
  running test), `object_test/clip` (the loaded clip as JSON, `{"error"}`, or
  `{}` when cleared).
- **Backend files** — `backend/file/content`, `backend/file/list`,
  `backend/file/error`: point-to-point replies to `/control/backend/file/*`.
- **Head tracking** — `head_pose` (4-float quaternion `w,x,y,z`, broadcast at
  ~30 Hz while a tracking feed is active, for low-latency client display).
- **Metering / timing** — `clip`, `decode_time_ms`, `render_time_ms`,
  `write_time_ms`, `crossover_time_ms`, `frame_duration_ms`, `monitoring`,
  `loudness`, `realtime/{master_gain,speaker_gain}`.
- **Latency & resampling** — `latency`, `latency_instant`, `latency_smoothed`,
  `latency_control`, `latency_target`, `latency_avail_input`,
  `latency_output_fifo`, `latency_resampler_pending`, `latency_downstream`,
  `resample_ratio`, `adaptive_resampling/state`, `adaptive_resampling/band`.
- **Input / config echoes** — `input`, `input_pipe`, `audio`, `log_level`.
- **OSC publication flags** — `osc/metering`, `osc/diag`.
- **Diagnostics** — `diag_schema`, `diag_values`.
- **Gain-table stream** — `debug/speaker_gaintable/{meta,chunk,uptodate,
  unavailable}`.

### The stream's rate

The stream messages (`spatial/frame` and the `object/*` messages it
precedes, `timestamp`, the meter and timing bundles, `playout/*`, `loudness`)
leave from a thread of their own, every 10 ms, in the order the engine
produced them. Of the object frames and the timestamps, which the engine
produces for every block it renders, only the latest of each 480-sample
window of the timeline goes out: at most 100 per second of audio each at
48 kHz. The window is one of audio, not of wall-clock time, so a host that
renders ahead of playback in bursts still sends a pose for every window of
what will be heard. Nothing a client holds goes stale for it: the object
messages are sent for what changed since the last frame *sent*, and a frame
that forces a full resend (a new content generation, a seek, a client
registering) passes that on to the frame that supersedes it. The meter and
diag bundles keep the rates set by `/control/metering/rate_hz` and
`/control/diag/rate_hz`.

---

## Adding or changing an address

1. Add/rename the constant in `osc-contract/src/lib.rs` (and to `ALL_CONTROL`,
   `ALL_STATE` or `ALL_SESSION`). A state address also gets its arguments in
   `osc-contract/src/shapes.rs`, and Studio's parser an arm that reads them.
2. Bump `CONTRACT_REVISION` — for any change to an address or its arguments —
   and pin the new address set where the contract's test says.
3. Reference the constant from the dispatcher / producer instead of a literal.
4. Document it above, and list it in the [address index](#address-index).

The contract crate's tests guard the structural invariants (every control const
is a control address, every state const a state address, no duplicate wire
addresses), that the engine and Studio sources reference constants instead of
spelling addresses, that every catalogued address appears in the index
below, that every state address has a shape, and that the address set has not
changed under the same `CONTRACT_REVISION`.

## Address index

Every address the contract catalogues, generated from `ALL_CONTROL`,
`ALL_STATE` and `ALL_SESSION` (sorted). The contract crate's
`every_catalogued_address_is_indexed_in_the_contract_doc` test fails when one
is missing here. Families with a dynamic tail are matched by prefix and are
not catalogued: `/omniphony/control/object/{id}/mute`,
`/omniphony/control/distance_diffuse/…`, `/omniphony/control/hybrid/…`,
`/omniphony/control/render_evaluation/cartesian/…` and
`/omniphony/control/render_evaluation/polar/…` (see above), plus the
per-object streams `/omniphony/object/{id}/…` and `/omniphony/meter/object/{id}`.

<details><summary>Control (162)</summary>

- `/omniphony/control/adaptive_resampling`
- `/omniphony/control/adaptive_resampling/enable_far_mode`
- `/omniphony/control/adaptive_resampling/far_mode_return_fade_in_ms`
- `/omniphony/control/adaptive_resampling/force_silence_in_far_mode`
- `/omniphony/control/adaptive_resampling/hard_recover_high_in_far_mode`
- `/omniphony/control/adaptive_resampling/hard_recover_in_far_mode`
- `/omniphony/control/adaptive_resampling/hard_recover_low_in_far_mode`
- `/omniphony/control/adaptive_resampling/high_recover_entry_margin_ms`
- `/omniphony/control/adaptive_resampling/integral_discharge_ratio`
- `/omniphony/control/adaptive_resampling/ki`
- `/omniphony/control/adaptive_resampling/kp_near`
- `/omniphony/control/adaptive_resampling/max_adjust`
- `/omniphony/control/adaptive_resampling/near_far_threshold_ms`
- `/omniphony/control/adaptive_resampling/pause`
- `/omniphony/control/adaptive_resampling/reset_ratio`
- `/omniphony/control/adaptive_resampling/update_interval_callbacks`
- `/omniphony/control/audio/output_backend`
- `/omniphony/control/audio/output_device`
- `/omniphony/control/audio/output_devices/refresh`
- `/omniphony/control/audio/output_file`
- `/omniphony/control/audio/output_file_format`
- `/omniphony/control/audio/sample_rate`
- `/omniphony/control/auto_gain`
- `/omniphony/control/auto_gain_ceiling`
- `/omniphony/control/backend/file/get`
- `/omniphony/control/backend/file/list`
- `/omniphony/control/backend/file/put`
- `/omniphony/control/backend/param`
- `/omniphony/control/binaural/air_absorption`
- `/omniphony/control/binaural/brir/head_tracking`
- `/omniphony/control/binaural/brir/max_length`
- `/omniphony/control/binaural/brir/tail_floor`
- `/omniphony/control/binaural/diffuse_field_eq`
- `/omniphony/control/binaural/ear_gain`
- `/omniphony/control/binaural/ear_mute`
- `/omniphony/control/binaural/head_radius`
- `/omniphony/control/binaural/hrir_source`
- `/omniphony/control/binaural/hrir_update_lattice`
- `/omniphony/control/binaural/hrtf_upload/begin`
- `/omniphony/control/binaural/hrtf_upload/chunk`
- `/omniphony/control/binaural/hrtf_upload/end`
- `/omniphony/control/binaural/reflections/enabled`
- `/omniphony/control/binaural/reflections/level`
- `/omniphony/control/binaural/reflections/room_depth`
- `/omniphony/control/binaural/reflections/room_height`
- `/omniphony/control/binaural/reflections/room_width`
- `/omniphony/control/binaural/reflections/wall_cutoff`
- `/omniphony/control/binaural/reverb/enabled`
- `/omniphony/control/binaural/reverb/level`
- `/omniphony/control/binaural/reverb/predelay`
- `/omniphony/control/binaural/reverb/rt60`
- `/omniphony/control/binaural/reverb/rt60_high_ratio`
- `/omniphony/control/binaural/reverb/rt60_low_ratio`
- `/omniphony/control/binaural/reverb/size`
- `/omniphony/control/binaural/unit_scale`
- `/omniphony/control/binaural_mode`
- `/omniphony/control/config/audio`
- `/omniphony/control/config/audio/apply`
- `/omniphony/control/config/input`
- `/omniphony/control/config/input/apply`
- `/omniphony/control/config/layout`
- `/omniphony/control/config/layout/apply`
- `/omniphony/control/config/speakers`
- `/omniphony/control/crossover_fir_transition_ratio`
- `/omniphony/control/crossover_type`
- `/omniphony/control/debug/speaker_gaintable/nack`
- `/omniphony/control/debug/speaker_gaintable/subscribe`
- `/omniphony/control/debug/speaker_gaintable/unsubscribe`
- `/omniphony/control/decode_thread`
- `/omniphony/control/diag/enabled`
- `/omniphony/control/diag/rate_hz`
- `/omniphony/control/distance_model`
- `/omniphony/control/distance_model_metric`
- `/omniphony/control/gain`
- `/omniphony/control/head/calibrate`
- `/omniphony/control/head/orientation`
- `/omniphony/control/head/quat`
- `/omniphony/control/head/recenter`
- `/omniphony/control/head/tracking/address`
- `/omniphony/control/head/tracking/format`
- `/omniphony/control/head/tracking/invert`
- `/omniphony/control/head/tracking/smoothing`
- `/omniphony/control/input/apply`
- `/omniphony/control/input/drc_mode`
- `/omniphony/control/input/drc_weight`
- `/omniphony/control/input/live/backend`
- `/omniphony/control/input/live/channels`
- `/omniphony/control/input/live/clock_mode`
- `/omniphony/control/input/live/description`
- `/omniphony/control/input/live/layout`
- `/omniphony/control/input/live/layout_import`
- `/omniphony/control/input/live/lfe_mode`
- `/omniphony/control/input/live/map`
- `/omniphony/control/input/live/node`
- `/omniphony/control/input/live/sample_rate`
- `/omniphony/control/input/mode`
- `/omniphony/control/input/refresh`
- `/omniphony/control/latency_target`
- `/omniphony/control/layout/export`
- `/omniphony/control/layout/radius_m`
- `/omniphony/control/log_level`
- `/omniphony/control/loudness`
- `/omniphony/control/metering`
- `/omniphony/control/metering/rate_hz`
- `/omniphony/control/object_generator`
- `/omniphony/control/object_generator/param`
- `/omniphony/control/object_test`
- `/omniphony/control/object_test/clip`
- `/omniphony/control/object_test/rotation`
- `/omniphony/control/option`
- `/omniphony/control/options`
- `/omniphony/control/options/apply`
- `/omniphony/control/output_channel_mapping`
- `/omniphony/control/output_mode`
- `/omniphony/control/overlay/enabled`
- `/omniphony/control/overlay/heatmap_bands`
- `/omniphony/control/overlay/heatmap_colormap`
- `/omniphony/control/overlay/heatmap_custom_stops`
- `/omniphony/control/overlay/heatmap_enabled`
- `/omniphony/control/overlay/labels`
- `/omniphony/control/overlay/objects`
- `/omniphony/control/overlay/tag`
- `/omniphony/control/overlay/trails`
- `/omniphony/control/phantom_extract`
- `/omniphony/control/phantom_extract/param`
- `/omniphony/control/placement/layout`
- `/omniphony/control/placement/mode`
- `/omniphony/control/profile/create`
- `/omniphony/control/profile/delete`
- `/omniphony/control/profile/rename`
- `/omniphony/control/profile/switch`
- `/omniphony/control/quit`
- `/omniphony/control/ramp_mode`
- `/omniphony/control/realtime/master_gain`
- `/omniphony/control/realtime/speaker_gain`
- `/omniphony/control/reload_config`
- `/omniphony/control/restart`
- `/omniphony/control/render/bridge_path`
- `/omniphony/control/render/bridge_paths`
- `/omniphony/control/render/input_pipe`
- `/omniphony/control/render_backend`
- `/omniphony/control/render_backend/restore`
- `/omniphony/control/render_evaluation/object_size_intervals`
- `/omniphony/control/render_evaluation/position_interpolation`
- `/omniphony/control/render_evaluation_mode`
- `/omniphony/control/render_evaluation_mode/from_file`
- `/omniphony/control/resume`
- `/omniphony/control/room_ratio`
- `/omniphony/control/room_ratio_center_blend`
- `/omniphony/control/room_ratio_lower`
- `/omniphony/control/room_ratio_rear`
- `/omniphony/control/save_config`
- `/omniphony/control/speaker_test`
- `/omniphony/control/speaker_test/idle_feed`
- `/omniphony/control/spread/distance_curve`
- `/omniphony/control/spread/distance_range`
- `/omniphony/control/spread/from_distance`
- `/omniphony/control/spread/max`
- `/omniphony/control/spread/min`
- `/omniphony/control/spread/size_to_spread_mode`
- `/omniphony/control/state/refresh`
- `/omniphony/control/surround_placement`
- `/omniphony/control/synthetic_objects`
- `/omniphony/control/unknown`
- `/omniphony/control/virtual_bed`
- `/omniphony/control/yield_port`

</details>

<details><summary>State (77)</summary>

- `/omniphony/state/adaptive_resampling/band`
- `/omniphony/state/adaptive_resampling/state`
- `/omniphony/state/audio`
- `/omniphony/state/backend/file/content`
- `/omniphony/state/backend/file/error`
- `/omniphony/state/backend/file/list`
- `/omniphony/state/capabilities`
- `/omniphony/state/clip`
- `/omniphony/state/config/save_error`
- `/omniphony/state/config/saved`
- `/omniphony/state/control_error`
- `/omniphony/state/crossover_time_ms`
- `/omniphony/state/debug/speaker_gaintable/chunk`
- `/omniphony/state/debug/speaker_gaintable/meta`
- `/omniphony/state/debug/speaker_gaintable/unavailable`
- `/omniphony/state/debug/speaker_gaintable/uptodate`
- `/omniphony/state/decode_time_ms`
- `/omniphony/state/diag_schema`
- `/omniphony/state/diag_values`
- `/omniphony/state/frame_duration_ms`
- `/omniphony/state/generation`
- `/omniphony/state/head_pose`
- `/omniphony/state/input`
- `/omniphony/state/input_pipe`
- `/omniphony/state/latency`
- `/omniphony/state/latency_avail_input`
- `/omniphony/state/latency_control`
- `/omniphony/state/latency_downstream`
- `/omniphony/state/latency_instant`
- `/omniphony/state/latency_output_fifo`
- `/omniphony/state/latency_resampler_pending`
- `/omniphony/state/latency_smoothed`
- `/omniphony/state/latency_target`
- `/omniphony/state/layout`
- `/omniphony/state/log_level`
- `/omniphony/state/loudness`
- `/omniphony/state/monitoring`
- `/omniphony/state/object_generators`
- `/omniphony/state/object_test/clip`
- `/omniphony/state/object_test/position`
- `/omniphony/state/options_schema`
- `/omniphony/state/host_options`
- `/omniphony/state/osc/diag`
- `/omniphony/state/osc/metering`
- `/omniphony/state/overlay`
- `/omniphony/state/phantom`
- `/omniphony/state/profiles`
- `/omniphony/state/realtime/master_gain`
- `/omniphony/state/realtime/speaker_gain`
- `/omniphony/state/render/abi`
- `/omniphony/state/render/bridge_api`
- `/omniphony/state/render/bridge_error`
- `/omniphony/state/render/bridge_path`
- `/omniphony/state/render/bridges`
- `/omniphony/state/render/config_path`
- `/omniphony/state/render/config_status`
- `/omniphony/state/render/executable`
- `/omniphony/state/render/version`
- `/omniphony/state/render_evaluation/cartesian/x_size`
- `/omniphony/state/render_evaluation/cartesian/y_size`
- `/omniphony/state/render_evaluation/cartesian/z_neg_size`
- `/omniphony/state/render_evaluation/cartesian/z_size`
- `/omniphony/state/render_evaluation/object_size_intervals`
- `/omniphony/state/render_evaluation/polar/azimuth_resolution`
- `/omniphony/state/render_evaluation/polar/distance_max`
- `/omniphony/state/render_evaluation/polar/distance_res`
- `/omniphony/state/render_evaluation/polar/elevation_resolution`
- `/omniphony/state/render_evaluation/position_interpolation`
- `/omniphony/state/render_time_ms`
- `/omniphony/state/renderer`
- `/omniphony/state/resample_ratio`
- `/omniphony/state/shutdown`
- `/omniphony/state/snapshot_complete`
- `/omniphony/state/speakers`
- `/omniphony/state/speakers/recompute_error`
- `/omniphony/state/speakers/recomputing`
- `/omniphony/state/vbap/allow_negative_z`
- `/omniphony/state/write_time_ms`

</details>

<details><summary>Session and streams (15)</summary>

- `/omniphony/bed/config`
- `/omniphony/heartbeat`
- `/omniphony/heartbeat/ack`
- `/omniphony/heartbeat/unknown`
- `/omniphony/log`
- `/omniphony/meter/drc_gain`
- `/omniphony/meter/master`
- `/omniphony/playout/block`
- `/omniphony/playout/heard`
- `/omniphony/register`
- `/omniphony/spatial/frame`
- `/omniphony/sync`
- `/omniphony/sync/ack`
- `/omniphony/timestamp`
- `/omniphony/yield/resume_port`

</details>
