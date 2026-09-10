# Phase 2 spec — the state & event contract between the web Studio and its Tauri host

Scope: everything that crosses the host↔webview boundary in
`omniphony-studio/` — the events the Rust host emits, the derived numbers it
computes before emitting, the connection/auto-start state machine, the JS
`app` state object those events write into, the repaint (`dirty`) scheduling,
the log pipeline, and the `get_state` snapshot. It says, per item, whether the
native egui crate (`omniphony-studio-egui/`) already covers it.

Path shorthand used throughout:

* **WEB** = `omniphony-studio`
* **NAT** = `omniphony-studio-egui`
* **RND** = `omniphony-renderer`

Tauri `control_*` commands are **out of scope** (already ported to
`NAT/src/host/commands/*.rs`). This document is only about the *inbound*
direction and the state model.

Verdict vocabulary used in the tables:

| Mark | Meaning |
|---|---|
| **APPLIED** | `NAT/src/osc/dispatch.rs` already mutates the equivalent state; nothing to port for the data itself. |
| **DERIVED** | The host computes something the OSC stream does not carry (peak hold, sliding windows, derived master meter, connection status, watchdog). Not present in the native crate — must be ported. |
| **MISSING** | The parser produces the `OscEvent`, but `dispatch.rs` drops it through its `_ => Change::None` catch-all (`NAT/src/osc/dispatch.rs:644-646`). Must be ported. |
| **UI-ONLY** | No model state; the payload only drives a transient UI effect (a flash, a scroll, a panel auto-open). |

---

## 0. Architecture delta in one paragraph

The Tauri host owns an `AppState` (`WEB/src-tauri/src/app_state.rs`) and pushes
*camelCase JSON events* over the webview IPC; `WEB/src/tauri-bridge.js` is the
single `listen(...)` registry that translates each event into writes on the
`app` object of `WEB/src/state.js` plus a call into a render function. The
native crate removes the whole serialisation layer: `NAT/src/osc/dispatch.rs`
applies parsed `OscEvent`s straight onto a reused `AppState` (`NAT/src/model/
app_state.rs`, a verbatim copy) wrapped in `Live`, and the UI reads that model
directly. Consequently:

* every event whose only job was *"carry a field of AppState to JS"* is already
  covered natively — there is nothing to port but the widget that reads it;
* the events that carried **host-computed** values (peak hold, timing windows,
  derived master meter, connection status, autostart) have no native
  equivalent, because the computation itself never moved;
* a handful of parser events are simply not wired into `dispatch.rs` yet.

---

## 1. Event inventory: host → webview

### 1.1 Transport: immediate vs batched

`WEB/src-tauri/src/osc_listener.rs:1756-1785`

```rust
const BATCH_FLUSH_INTERVAL: Duration = Duration::from_millis(16);

const BATCHED_EVENTS: &[&str] = &[
    "binaural:head_pose",
    "source:update",
    "source:meter",
    "source:gains",
    "source:band_gains",
    "speaker:meter",
    "ear:meter",
    "master:meter",
    "meter:drc_gain",
];
```

* Queueing (`queue_batched_emit`, `osc_listener.rs:2001-2006`) inserts into a
  thread-local `HashMap<String, (&'static str, Value)>` keyed by
  `batch_dedup_key` (`osc_listener.rs:1988-1999`):
  * `"{event}|{id}"` normally (`id` is `payload["id"].to_string()`, empty when
    absent — so `master:meter`, `meter:drc_gain` and `binaural:head_pose`
    collapse to a single slot);
  * `"{event}|{id}|{band}"` for `source:band_gains`.
  **Only the latest payload per key in the 16 ms window survives.**
* Flush (`flush_emit_batch`, `osc_listener.rs:2008-2025`) drains the map and
  emits **one** event named `state:batch` with payload
  `{ "events": [ { "event": <name>, "payload": <payload> }, … ] }`.
  Nothing is emitted when the map is empty.
* The flush is driven from the OSC loop, independent of message arrival
  (`osc_listener.rs:972-975`).
* JS replay: `WEB/src/tauri-bridge.js:90-139`. `applyBatchedEvent` switches on
  the inner name and calls exactly the same handlers the individual
  `listen(...)` registrations use. Note the switch also handles
  `objectTest:position` even though that event is *not* in `BATCHED_EVENTS` —
  harmless dead arm.
* Non-batched events go out immediately, with a byte-level dedup on two
  channels only (`osc_listener.rs:3421-3440`):
  * `state:snapshot_ready` → `AppState::last_snapshot_emit_hash`
  * `overlay:state` → `AppState::last_overlay_emit_hash`
  `already_emitted()` (`osc_listener.rs:2046-2053`) hashes
  `payload.to_string()` with `DefaultHasher` and skips an identical repeat.

**Native equivalent.** None needed. `NAT/src/osc/mod.rs:195-200` coalesces at
the *repaint* level instead: `REPAINT_COALESCE = 2500 µs`, and a packet whose
`Change != Change::None` requests at most one egui repaint per 2.5 ms. The
batching, the dedup keys and `state:batch` are all transport artefacts of the
webview IPC and must **not** be reproduced.

### 1.2 Master event table

Ordered by the `handle_event` arm that produces it
(`WEB/src-tauri/src/osc_listener.rs:2055-3441`).

| Event | Payload | Trigger (`OscEvent`) | JS handler → `app` effect | Verdict |
|---|---|---|---|---|
| `state:batch` | `{events:[{event,payload}]}` | 16 ms timer | `tauri-bridge.js:131-139` replays each | transport, N/A |
| `spatial:frame` | `{samplePos:i64, generation:u64, objectCount:u32, coordinateFormat:u8, reset:bool}` | `SpatialFrame` | `tauri-bridge.js:242-272`: `app.lastSpatialFrameAt = performance.now()`; on `reset` wipes every `sourceTrails` entry's geometry; `syncVirtualBedObjects()` | **APPLIED** (`dispatch.rs:217-270`, incl. `last_frame_reset`, trail clear, stale-slot purge) |
| `source:update` | `{id, position:{x,y,z,coordMode,azimuthDeg,elevationDeg,distanceM,gainDb,generation,directSpeakerIndex,fixed,label,kind,sourceTag,name}}` | `Update`, `UpdateMeta` | `updateSource(id,position)`; sets `app.lastSpatialFrameAt`; `syncVirtualBedObjects()` | **APPLIED** (`dispatch.rs:271-313`) |
| `source:size` | `{id, size:{w,d,h}, generation}` | `UpdateSize` | `updateSourceSize` → `sourceSizes` map | **APPLIED** (`live.object_sizes`, `dispatch.rs:314-317`) |
| `source:remove` | `{id}` | `Remove`, plus one per `removed_ids` after a frame purge (`osc_listener.rs:3407-3412`) | `removeSource`; `syncVirtualBedObjects()` | **APPLIED** (`dispatch.rs:160-169, 318-322`) |
| `object:mute` (re-emit) | `{id, muted:1}` | reset-restore loop (`osc_listener.rs:3417-3419`) | re-adds to `objectMuted` | **APPLIED** — native never wipes the mutes on reset, so no restore pass is needed (`dispatch.rs:253-257`) |
| `source:meter` | `{id, meter:{peakDbfs, rmsDbfs, bandRmsDbfs:[f64], peakHoldDbfs}}` | `MeterObject` | `updateSourceLevel` | data **APPLIED** (`dispatch.rs:323-340`); `peakHoldDbfs` **DERIVED** |
| `source:gains` | `{id, gains:[f64]}` | `MeterObjectGains` | `updateSourceGains` | **APPLIED** |
| `source:band_gains` | `{id, band:usize, gains:[f64]}` | `MeterObjectBandGains` | `updateSourceBandGains` | **APPLIED** |
| `speaker:meter` | `{id, meter:{peakDbfs, rmsDbfs, peakHoldDbfs}}` | `MeterSpeaker` | `updateSpeakerLevel(Number(id), meter)` | data **APPLIED**; `peakHoldDbfs` **DERIVED**; the *derived master fallback* it triggers is **DERIVED** |
| `ear:meter` | `{id, meter:{peakDbfs, rmsDbfs, peakHoldDbfs}}` | `MeterEar` | `updateHeadphoneMeter(Number(id), {peakDbfs, rmsDbfs})` — defaults `-100` when absent | data **APPLIED** (`live.ear_levels`); `peakHoldDbfs` **DERIVED** |
| `master:meter` | `{meter:{peakDbfs, rmsDbfs, peakHoldDbfs}}` or `{meter:{…}, derived:true}` | `MeterMaster`, or synthesised in the `MeterSpeaker` arm | `updateMasterLevel` → `setMasterLevel`, `dirty.masterMeter` | reported form **APPLIED**; `peakHoldDbfs` + the `derived:true` reconstruction **DERIVED** |
| `meter:drc_gain` | `{value:f64}` | `MeterDrcGain` | `updateDrcMeterUI(Number(value))` | **APPLIED** (`live.drc_gain`) |
| `speaker:gain` | `{id, gain:f64}` | `StateSpeakerGain`, `StateRealtimeSpeakerGain` | `speakerGainCache.set(String(id), Number(gain))`; `updateSpeakerControlsUI()` | **APPLIED** |
| `speaker:delay` | `{id, delayMs:f64}` (already `max(0)`) | `StateSpeakerDelay` | `speakerDelays.set(id, max(0,…))`; `renderSpeakerEditor()`; `updateSpeakerControlsUI()` | **APPLIED** (writes the selected layout's `Speaker::delay_ms`) |
| `object:mute` | `{id, muted:0|1}` | `StateObjectMute` | add/remove in `objectMuted`; on unmute also `objectManualMuted.delete`; `updateObjectControlsUI()` | **APPLIED** (manual-mute bookkeeping is UI-side) |
| `object:source_tag` | `{id, sourceTag:String}` | `StateObjectSourceTag` | `updateSourceTag` | **APPLIED** |
| `speaker:mute` | `{id, muted:0|1}` | `StateSpeakerMute` | add/remove in `speakerMuted`; unmute also clears `speakerManualMuted`; `updateSpeakerControlsUI()` | **APPLIED** |
| `osc:metering` | `{enabled:0|1}` | `StateOscMetering` | `app.oscMeteringEnabled`; syncs `#oscMeteringToggle.checked`; **when disabled clears `app.decodeTimeMs/renderTimeMs/writeTimeMs` to null**; `updateRenderTimeUI()` | state **APPLIED**; the clearing side effect is **UI-ONLY, not ported** |
| `speaker:spatialize` | `{id, spatialize:0|1, crossoverCutoffs:[f64]}` | `StateSpeakerSpatialize` | `setSpeakerSpatializeLocal(index,next)`; `app.currentLayoutCutoffs = crossoverCutoffs`; `syncCrossoverBandSelects()`; `updateSpeakerControlsUI()` | flag **APPLIED**; `crossoverCutoffs` is **DERIVED** by the host but the native UI can call `model::layouts::crossover_cutoffs(live.selected_speakers())` directly (`NAT/src/model/layouts.rs:565-579`) |
| `speaker:name` | `{id, name:String}` | `StateSpeakerName` | `app.currentLayoutSpeakers[i].id = name`; `updateSpeakerVisualsFromState(i)`; `updateSpeakerControlsUI()` | **APPLIED** |
| `speaker:freq_low` | `{id, freq_low:f32|null, crossoverCutoffs:[f64]}` (snake_case key!) | `StateSpeakerFreqLow` | `speaker.freqLow = fl>0 ? fl : null`; cutoffs; `syncCrossoverBandSelects()`; re-render editor when `app.selectedSpeakerIndex === index` | **APPLIED** (cutoffs derivable locally) |
| `speaker:freq_high` | `{id, freq_high, crossoverCutoffs}` | `StateSpeakerFreqHigh` | mirror of the above | **APPLIED** |
| `clip:detected` | `{speaker:i32}` | `StateClip` | `flashClipIndicator()` + `flashSpeakerClip(Number(speaker))` | **APPLIED** (`live.clip = Some((idx, Instant::now()))`); the 1 s flash is UI |
| `overlay:state` | the renderer's overlay JSON, parsed | `StateOverlay` | `applyOverlayState(payload)` | **APPLIED** (`live.overlay`); the emit-dedup hash is transport-only |
| `binaural:head_pose` | `{w,x,y,z}` (f32) | `StateHeadPose` | `setHeadPoseQuat(payload)` | **APPLIED** (`live.head_pose`) |
| `state:snapshot_ready` | the **whole serialised `AppState`** | `StateRenderer`, `StateAudio`, `StateSpeakers`, `StateInput`, `StateLoudness`, `StateMonitoring`, `StateProfiles` (each when its applier returned `true`) and `StateSnapshotComplete` | `applyInitState(payload)` — full UI rebuild (see §6) | domain appliers **APPLIED** (`dispatch.rs:472-495`, `512-515`); the snapshot emit is replaced by `Change::Snapshot` + `live.snapshot_epoch += 1` |
| `layouts:update` | `{layouts:[Layout], selectedLayoutKey}` | `StateLayout` (only when `apply_layout_domain_state` returned `true`) | `hydrateLayoutSelect(layouts, selectedLayoutKey)` | **APPLIED** |
| `speaker_gaintable` | decoded table, see §1.4 | `StateDebugSpeakerGaintableChunk` completing a transfer | `pushLog('info', …)` then `setSpeakerGainTable(payload)` | **APPLIED** (`live.gain_tables`, keyed by `speaker_index()`) |
| `speaker_gaintable:unavailable` | renderer JSON | `StateDebugSpeakerGaintableUnavailable` | logs only on *transition* (`lastGaintableUnavailable` dedup, `tauri-bridge.js:204-219`) | **APPLIED** (`live.gaintable_unavailable`) |
| `speaker_gaintable:uptodate` | `{version:i32}` | `StateDebugSpeakerGaintableUptodate` | **deliberately not listened to** (`tauri-bridge.js:220-222`) — fires on every 5 s heartbeat | no-op; native also ignores it. Do **not** port. |
| `master:gain` | `{value:f64}` | `StateRealtimeMasterGain` | `app.masterGain = Number(value)`; `updateMasterGainUI()` | **APPLIED** |
| `latency` | `{value:i64}` (rounded) | `StateLatency` | `app.latencyMs`; `updateLatencyDisplay()`, `updateLatencyMeterUI()` | **APPLIED** |
| `latency:instant` | `{value:i64}` | `StateLatencyInstant` | `setLatencyInstantMs(value)` + both latency renders | state **APPLIED**; the `record_timing(Latency, value)` side effect is **DERIVED** |
| `latency:control` | `{value:i64}` | `StateLatencyControl` | `app.latencyControlMs` | **APPLIED** |
| `latency:smoothed` | `{value:f64}` (**not** rounded) | `StateLatencySmoothed` | `app.latencySmoothedMs` | **APPLIED** |
| `latency:downstream` | `{value:i64}` | `StateLatencyDownstream` | `app.latencyDownstreamMs` | **APPLIED** |
| `latency:target` | `{value:i64}` | `StateLatencyTarget` | `app.latencyTargetMs = finite ? value : null` | **APPLIED** |
| `latency:requested` | `{value:i64}` | `StateLatencyTargetRequested` | `app.latencyRequestedMs`; **back-fills** `latencyTargetMs` and `latencyMs` when either is still `null` | **APPLIED** for the field; the back-fill is a UI fallback (reproduce in the readout, see §2.4) |
| `latency:avail_input` | `{value:f64}` | `StateLatencyAvailInput` | *no listener* — reaches only the snapshot | **APPLIED** |
| `latency:output_fifo` | `{value:f64}` | `StateLatencyOutputFifo` | *no listener* | **APPLIED** |
| `latency:resampler_pending` | `{value:f64}` | `StateLatencyResamplerPending` | *no listener* | **APPLIED** |
| `latency:stats` | see §2.3 | 250 ms timer | `app.timingStats = payload`; `updateLatencyMeterUI()`, `updateRenderTimeUI()` | **DERIVED** — whole event |
| `diag:schema` | `{value: <JSON string>}` | `StateDiagSchema` | `app.diagSchema = JSON.parse(value)` (`parseDiagPayload`, idempotent if already an object) | **APPLIED** (`live.app.latency.diag_schema`) |
| `diag:values` | `{value: <JSON string>}` | `StateDiagValues` | `app.diagValues = …` | **APPLIED** |
| `objectGenerators:schema` | `{value: <JSON string>}` | `StateObjectGenerators` | `app.objectGenerators = JSON.parse(value) || []` (catch → `[]`); `rebuildObjectGeneratorControls()` | **APPLIED** (`live.object_generators_schema`) |
| `phantom:schema` | `{value: <JSON string>}` | `StatePhantom` | `app.phantomSchema = …`; `rebuildPhantomControls()` | **APPLIED** (`live.phantom_schema`) |
| `options:schema` | `{value: <JSON string>}` | `StateOptionsSchema` | `app.optionsSchema = …`; `reflectBoundOptions()` | **APPLIED** (`live.options_schema`) |
| `decode:time_ms` | `{value:f64}` | `StateDecodeTimeMs` | finite → `setDecodeTimeMs`, else `app.decodeTimeMs = null`; `updateRenderTimeUI()` | state **APPLIED**; `record_timing(Decode)` **DERIVED** |
| `render:time_ms` | `{value:f64}` | `StateRenderTimeMs` | same shape | state **APPLIED**; `record_timing(Render)` **DERIVED** |
| `crossover:time_ms` | `{value:f64}` | `StateCrossoverTimeMs` | same shape | state **APPLIED**; `record_timing(Crossover)` **DERIVED** |
| `write:time_ms` | `{value:f64}` | `StateWriteTimeMs` | same shape | state **APPLIED**; `record_timing(Write)` **DERIVED** |
| `frame:duration_ms` | `{value:f64}` | `StateFrameDurationMs` | `setFrameDurationMs` (ignores ≤0 / non-finite) | **APPLIED** — but note the native `dispatch.rs:608-611` stores the raw value with **no `>0` guard**; the guard must move into the readout |
| `objectTest:position` | `{x,y,z,peakDbfs,rmsDbfs}` | `StateObjectTestPosition` | `setObjectTestReportedPosition([x,y,z], payload)` | **APPLIED** (`live.object_test_position`) |
| `resample_ratio` | `{value:f64}` | `StateResampleRatio` | `app.resampleRatio`; `updateResampleRatioDisplay()` | **APPLIED** |
| `render:bridge_path` | `{value:String}` | `StateRenderBridgePath` | `app.renderBridgePath = trim() || null`; `updateInputControlUI()` | **APPLIED** |
| `render:config_path` | `{value}` | `StateRenderConfigPath` | `app.renderConfigPath`; `updateAboutConfigPath()` | **APPLIED** |
| `render:config_status` | `{value}` | `StateRenderConfigStatus` | `app.renderConfigStatus`; `updateAboutConfigPath()` | **APPLIED** |
| `render:version` | `{value}` | `StateRenderVersion` | `app.renderVersion`; `updateAboutRendererVersion()` | **APPLIED** |
| `render:executable` | `{value}` | `StateRenderExecutable` | `app.renderExecutable`; `updateAboutRendererVersion()`; `renderOscStatus()` (foreign-renderer banner) | **APPLIED** |
| `render:abi` | `{value}` | `StateRenderAbi` | `app.renderAbi`; `updateAboutRendererVersion()` | **APPLIED** |
| `render:bridge_error` | `{value}` | `StateRenderBridgeError` | `app.renderBridgeError = trim() || null`; `renderOscStatus()` → red banner | **APPLIED** |
| `state:input_pipe` | `{value:String}` | `StateInputPipe` | `app.orenderInputPipe = trim()||null`; `renderOscStatus()`, `updateInputControlUI()` | **APPLIED** |
| `state:object_test_clip` | `{value:<JSON string>}` | `StateObjectTestClip` | `JSON.parse` in a try/catch → `setObjectTestClipState(state|null)` | **MISSING** |
| `state:log_level` | `{value:String}` | `StateLogLevel` | `logState.backendLogLevel = normalizeLogLevel(value)`; `renderLogLevelControl()` | **MISSING** (`AppState::log_level` exists but is never written by dispatch) |
| `omniphony:log` | `{seq:u64, level:String, target:String, message:String}` | `Log` | see §5 | **MISSING** |
| `render_evaluation:cartesian:x_size` | `{value:u32}` | `StateRenderEvaluationCartesianXSize` | `app.vbapCartesianState.xSize = v>0 ? v : null`; `updateVbapCartesian()` | **MISSING** (target field `AppState::vbap_cartesian.x_size` exists) |
| `render_evaluation:cartesian:y_size` | `{value}` | …`YSize` | `ySize = v>0?v:null` | **MISSING** |
| `render_evaluation:cartesian:z_size` | `{value}` | …`ZSize` | `zSize = v>0?v:null` | **MISSING** |
| `render_evaluation:cartesian:z_neg_size` | `{value}` | …`ZNegSize` | `zNegSize = v>=0 ? v : 0` (**note `>=`**) | **MISSING** |
| `render_evaluation:polar:azimuth_resolution` | `{value:u32}` | …`PolarAzimuthResolution` | `>0 ? v : null`; `updateVbapPolar()` | **MISSING** |
| `render_evaluation:polar:elevation_resolution` | `{value:u32}` | …`PolarElevationResolution` | idem | **MISSING** |
| `render_evaluation:polar:distance_res` | `{value:u32}` | …`PolarDistanceRes` | idem | **MISSING** |
| `render_evaluation:polar:distance_max` | `{value:f64}` | …`PolarDistanceMax` | idem | **MISSING** |
| `render_evaluation:position_interpolation` | `{enabled:bool}` | …`PositionInterpolation` | `app.vbapPositionInterpolation = enabled === true`; `updateVbapPositionInterpolation()` | **MISSING** (field `vbap_polar.position_interpolation`) |
| `vbap:allow_negative_z` | `{enabled:bool}` | `StateVbapAllowNegativeZ` | `app.vbapAllowNegativeZ`; `updateVbapPolar()` | **MISSING** |
| `vbap:recomputing` | `{enabled:bool}` | `StateSpeakersRecomputing` | `clearRecomputeAckWatchdog()`; `app.vbapRecomputing = enabled===true`; when true also `app.recomputeError = null`; `renderVbapStatus()` | **MISSING** |
| `speakers:recompute_error` | `{message:String}` | `StateSpeakersRecomputeError` | `app.recomputeError = trimmed || null`; if set also `app.vbapRecomputing = false`; `renderVbapStatus()` | **MISSING** |
| `backend-file:content` | `{backend,key,name,content}` | `StateBackendFileContent` | *(code-editor modal — later phase; no listener in `tauri-bridge.js`)* | **MISSING** (later phase) |
| `backend-file:list` | `{backend, names:<parsed JSON array, `[]` on failure>}` | `StateBackendFileList` | idem | **MISSING** (later phase) |
| `backend-file:error` | `{backend,key,message}` | `StateBackendFileError` | idem | **MISSING** (later phase) |
| `adaptive_resampling` | `{enabled:0|1}` | `StateAdaptiveResampling` | *(no dedicated listener; reaches the UI via the snapshot)* | **MISSING** |
| `adaptive_resampling:enable_far_mode` | `{enabled:0|1}` (echoes the stored `Option<u8>`) | …`EnableFarMode` | via snapshot | **MISSING** |
| `adaptive_resampling:force_silence_in_far_mode` | `{enabled}` | … | via snapshot | **MISSING** |
| `adaptive_resampling:hard_recover_high_in_far_mode` | `{enabled}` | … | via snapshot | **MISSING** |
| `adaptive_resampling:hard_recover_low_in_far_mode` | `{enabled}` | … | via snapshot | **MISSING** |
| `adaptive_resampling:far_mode_return_fade_in_ms` | `{value:i64}` (`value.round()`) | … | via snapshot | **MISSING** |
| `adaptive_resampling:kp_near` | `{value:f64}` | … | via snapshot | **MISSING** |
| `adaptive_resampling:ki` | `{value:f64}` | … | via snapshot | **MISSING** |
| `adaptive_resampling:integral_discharge_ratio` | `{value:f64}` | … | via snapshot | **MISSING** |
| `adaptive_resampling:max_adjust` | `{value:f64}` | … | via snapshot | **MISSING** |
| `adaptive_resampling:update_interval_callbacks` | `{value:i64}` (rounded) | … | via snapshot | **MISSING** |
| `adaptive_resampling:high_recover_entry_margin_ms` | `{value:i64}` (rounded) | … | via snapshot | **MISSING** |
| `adaptive_resampling:band` | `{value:String}` | …`Band` | `app.adaptiveResamplingBand = string ? value : null`; `updateAdaptiveResamplingUI()` | **MISSING** |
| `adaptive_resampling:state` | `{value:String}` | …`State` | `app.adaptiveResamplingState`; `updateAdaptiveResamplingUI()` | **MISSING** |
| `adaptive_resampling:pause` | `{enabled:0|1}` | …`Paused` | `app.adaptiveResamplingPaused = payload.enabled !== 0`; `updateAdaptiveResamplingUI()` | **MISSING** |
| `config:saved` | `{saved:0|1}` | `StateConfigSaved` | `app.configSaved = saved !== 0`; `app.saveError = null`; `app.saveRequested = false`; `updateConfigSavedUI()` | **MISSING** |
| `config:save_error` | `{message:String}` | `StateConfigSaveError` | `app.saveError = trimmed || null`; `app.saveRequested = false`; `pushLog('error', …)` when set; `updateConfigSavedUI()` | **MISSING** |
| `osc:status` | `{status:"initializing"\|"connected"\|"reconnecting"\|"error"}` | connection FSM, §3 | `setOscStatus(next)` (only the four values above) | **DERIVED** |
| `orender:autostart` | `{status:"launched"}` / `{status:"failed"[, error:String]}` | watchdog, §3 | `launched` → `pushLog('info', t('log.orenderAutostartLaunched'))`; `failed` → `pushLog('error', t('log.orenderAutostartFailed'))` **and** `setOscStatus('error')` | **DERIVED** |
| `sofa:download_progress` | `{bytes:u64, total:u64|null}` | SOFA browser download | *(SOFA browser modal — later phase)* | **DERIVED**, later phase |
| `auto_tune:event` | `{event:String, payload:Object}` | auto-tune runner | *(wizard — later phase)* | **DERIVED**, later phase |
| `layout:selected` | `{key}` | **never emitted** | `tauri-bridge.js:159-165` listens and would set `#layoutSelect.value` + `renderLayout(key)` | dead code. Do not port. |

### 1.3 `sofa:download_progress` (later phase, documented for completeness)

`WEB/src-tauri/src/commands/sofa_browser.rs:25` — `const PROGRESS_EVENT: &str =
"sofa:download_progress"`. Emitted:

* once with `{bytes: expected, total: expected}` when a cached file already has
  the expected `Content-Length` (line 394-397) — the download is skipped;
* every ≥100 ms during the transfer, `{bytes: done, total}` where `total` is
  `Option<u64>` from `Content-Length` (line 428-431);
* once more at EOF with the final `done` (line 434-437).

Cancellation (`CANCEL` atomic) deletes the `.part` file and returns
`Err("cancelled")` with no final event. On success the `.part` is renamed to the
flattened destination (`hrtf/<rel with '/'→'_'>`).

### 1.4 `speaker_gaintable` payload shapes

`WEB/src-tauri/src/osc_listener.rs:1548-1744`. Chunks arrive as
`[version u32 LE][chunk_index u32 LE][bytes…]`, are reassembled per version,
concatenated, then decoded by magic:

**`"OEVL"` (evaluation artifact)** — header `MAGIC(4) | version(4) |
metadata_len u32 LE @8 | payload_len u32 LE @12`, then `metadata_len` bytes of
JSON, then a zlib payload of f32 LE. `metadata.domain.kind` selects:

* `"cartesian"` → `{version, domain:"cartesian", speakerCount, xCount, yCount,
  zCount, xPositions:[f32], yPositions, zPositions, gains:[f32 of
  x*y*z*speakers]}`
* `"polar"` → `{version, domain:"polar", speakerCount, azimuthCount,
  elevationCount, distanceCount, azimuthPositions, elevationPositions,
  distancePositions, gains}`

**`"OBGT"` (band gain table)** — same header layout; metadata carries
`x_count`, `y_count`, `z_count`, `band_count`, `speaker_index` (**signed**: `-1`
is the all-speaker energy field), `bands:[{low_hz, high_hz|null}]`. Emitted as
`{version, domain:"cartesian_bands", speakerIndex, xCount, yCount, zCount,
bandCount, bands:[{lowHz, highHz|null}], dataB64}` where `dataB64` is a
hand-rolled standard base64 of the *inflated* bytes (`base64_encode`,
`osc_listener.rs:1577-1599`) so JS can build `Float32Array` views without
per-number parsing.

**Native**: `NAT/src/osc/apply.rs:891` `gaintable_on_chunk` returns a typed
`GainTable` with `speaker_index()` / `version()`; `dispatch.rs:500-507` stores
it in `live.gain_tables` keyed by `speaker_index()`. No base64, no JSON. The
NACK reliability layer is ported too (`apply::gaintable_check_nack`,
`apply::send_gaintable_nack`, driven from `NAT/src/osc/mod.rs:294-307`).

Reliability constants (identical both sides):
`GAINTABLE_MAX_INFLIGHT = 6`, `GAINTABLE_NACK_TIMEOUT = 120 ms`,
`GAINTABLE_MAX_NACK_ROUNDS = 12`, `GAINTABLE_NACK_MAX_INDICES = 256`
(`osc_listener.rs:1434-1443`). NACK address:
`/omniphony/control/debug/speaker_gaintable/nack [version, idx…]`.

---

## 2. Host-derived computations the native crate lacks

### 2.1 `peak_hold.rs` — already copied, never driven

`NAT/src/host/peak_hold.rs` is `WEB/src-tauri/src/peak_hold.rs` verbatim plus a
leading `#![allow(dead_code)]`. **Nothing in the native crate calls it**
(only `NAT/src/host/mod.rs:9` declares the module). What must be ported is the
*driving*, not the algorithm.

Constants (`peak_hold.rs:29-45`):

| Name | Value | Role |
|---|---|---|
| `METER_DB_MIN` | `-60.0` | bottom of the meter scale (mirrors `METER_DB_MIN` in `WEB/src/mute-solo.js:52`) |
| `METER_DB_MAX` | `6.0` | top of the scale (`mute-solo.js:53`) |
| `HOLD` | `1000 ms` | how long the cursor sits before falling |
| `DECAY_DB_PER_SEC` | `120.0` | fall rate = the old JS "2 dB per repaint at 60 Hz", made time-based |
| `REARM_PERCENT_EPSILON` | `0.1` | re-arm tolerance, in **bar percent** |

`db_to_meter_percent(db) = clamp(((db - (-60)) / (6 - (-60))) * 100, 0, 100)`,
with non-finite → `METER_DB_MIN`. 0 dBFS ≈ 90.909 %.

`PeakHolds::update(key, peak_db, now)`:

1. non-finite `peak_db` → `METER_DB_MIN`;
2. no hold, or `peak_db >= hold.db` → store a fresh hold `{db: peak_db,
   falls_after: now + HOLD, updated_at: now}` and return it (`>=`, so a
   sustained level pins the cursor);
3. `now <= falls_after` → return `hold.db`, but bump `updated_at = now`;
4. otherwise `decayed = max(hold.db - 120 * elapsed_secs, peak_db)` where
   `elapsed = now - hold.updated_at`; if
   `db_to_meter_percent(decayed) <= db_to_meter_percent(peak_db) + 0.1`, the
   hold re-arms (`falls_after = now + HOLD`); store and return `decayed`.

`forget(key)` drops the state.

**How the host drives it** (`WEB/src-tauri/src/osc_listener.rs`):

* thread-local `PEAK_HOLDS` on the OSC thread only, no lock (line 1783-1784);
* thin wrappers `peak_hold(key, peak_dbfs)` (1788-1790) and
  `forget_peak_hold(key)` (1794-1796), both using `Instant::now()`;
* **sampled once per incoming meter message**, inside the `handle_event` arm,
  and its result placed in the payload as `peakHoldDbfs`:

| Meter | key | site |
|---|---|---|
| object | `format!("src:{id}")` | `osc_listener.rs:2280` |
| speaker | `format!("spk:{id}")` | `2344` |
| ear | `format!("ear:{id}")` | `2364` |
| master (reported) | `"master"` | `2386` |
| master (derived) | `"master"` | `1980` |

* forgotten on `OscEvent::Remove` (`2243`) and for every id in `removed_ids`
  after a frame purge/reset (`3410`) — object ids are reused across seeks and
  tracks, and an inherited cursor would show a peak the new object never made.

**Port note.** In the native host, call `PeakHolds::update` from the same
places `dispatch.rs` inserts into `source_levels` / `speaker_levels` /
`ear_levels` / `master_level`, storing the returned dBFS next to the meter (or
in a parallel map keyed the same way), and call `forget` in `Live::remove_source`
and the `SpatialFrame` stale-slot loop. Keep the meter-arrival cadence: the hold
is *not* advanced on repaint, so a UI that only reads it draws a frozen cursor
between messages — which is exactly what the web UI does.

### 2.2 `timing_stats.rs` — already copied, never driven

`NAT/src/host/timing_stats.rs` = `WEB/src-tauri/src/timing_stats.rs` verbatim +
`#![allow(dead_code)]`. Unused.

Data structure (`timing_stats.rs:28-141`): `BUCKETS = 200` fixed time buckets
per series, `bucket_ms = max(span_ms / 200, 1)`, each bucket
`{epoch:u64, count:u32, sum:f64, min:f64, max:f64}` with
`Bucket::EMPTY.epoch = u64::MAX`.

* `TimeWindow::new(span_ms)` — the only allocation the window ever performs.
* `record(now_ms, value)` — non-finite values are **ignored** (not recorded,
  not clearing); `epoch = now_ms / bucket_ms`, `slot = epoch % 200`; same epoch
  accumulates, older epoch restarts the bucket. O(1), no allocation.
* `stats(now_ms, span_ms) -> Option<WindowStats{min,max,mean,count}>` — walks
  all 200 buckets, skipping `bucket.epoch > current || bucket.epoch < oldest`
  where `oldest = current.saturating_sub(min(span_ms / bucket_ms, 199))`;
  `None` when `count == 0`. `span_ms` may be **shorter** than the window's own
  span — that is how one series serves both a long max and a short mean.
  Granularity: window edges land on bucket boundaries, so a query covers its
  span ± one bucket.

**How the host drives it** (`osc_listener.rs:1798-1916`):

```rust
const LATENCY_RAW_WINDOW_MS: u64          = 4000;   // latency min/max markers
const RENDER_TIME_WINDOW_MS: u64          = 5000;   // per-stage max markers
const RENDER_TIME_AVERAGE_WINDOW_MS: u64  = 1000;   // per-stage bar averages
const TIMING_STATS_INTERVAL: Duration     = 250 ms; // emit cadence (4 Hz)
```

* thread-local `TIMING_WINDOWS: TimingWindows` (line 1779-1780) holding five
  `TimeWindow`s: `latency` (4000 ms), `decode`/`render`/`crossover`/`write`
  (5000 ms each) — `TimingWindows::new()`, line 1820-1830.
* `now_ms()` is a **monotonic origin of the struct's own**: `started` is set
  lazily on first use and `now_ms = started.elapsed().as_millis()`
  (1834-1837) — never wall clock.
* `record_timing(series, value)` (1857-1874) — **if `value.is_finite()` record
  it, else `window.clear()`**. This is the important asymmetry with
  `TimeWindow::record`, which merely ignores a non-finite sample: a non-finite
  telemetry value means "this stage has nothing to report" (no output running,
  no crossover configured) and must **drop the whole series history**, so a max
  marker cannot keep showing a spike from a stage that has gone quiet.
* Call sites: `StateLatencyInstant` → `Latency` (2760); `StateDecodeTimeMs` →
  `Decode` (2882); `StateRenderTimeMs` → `Render` (2904);
  `StateCrossoverTimeMs` → `Crossover` (2912); `StateWriteTimeMs` → `Write`
  (2920). **Only `latency_instant` feeds the latency series — not `latency`,
  `latency_control` or `latency_smoothed`.**
* Emission every 250 ms from the OSC loop (`980-983` → `emit_timing_stats`,
  1892-1916), with a payload-hash dedup (`already_emitted` against a local
  `last_timing_stats_hash`) so an idle renderer costs nothing instead of four
  all-null events per second.

Payload (`latency:stats`):

```jsonc
{
  "latency": {"min":…, "max":…, "mean":…} | null,   // stats(now, 4000)
  "decode":  { "avg": {…}|null, "max": {…}|null },  // avg = stats(now,1000), max = stats(now,5000)
  "render":  { "avg": …, "max": … },
  "crossover": { "avg": …, "max": … },
  "write":   { "avg": …, "max": … }
}
```

`stats_json` (1878-1883) emits only `{min, max, mean}` — **`count` is dropped**.
`null` means "nothing in the window": the frontend hides the marker rather than
drawing a stale one.

### 2.3 How the UI consumes `latency:stats`

`WEB/src/controls/latency.js`, `app.timingStats`:

**Latency meter** (`renderLatencyMeterUI`, 262-406):

* scale: `targetForScale = latencyRequestedMs ?? latencyTargetMs ?? latencyMs`;
  `maxMs = targetForScale === null ? 2000 : max(100, targetForScale * 2)`.
* fill: `raw = latencyInstantMs ?? latencyTargetMs ?? latencyMs`; percent
  `min(100, max(0, raw)/maxMs*100)`, `toFixed(1)`.
* `rawMin = timingStats?.latency?.min`, `rawMax = …?.max` (null when the window
  is empty). Both masks hide when `rawMin===null || rawMax===null || rawMax <
  rawMin`. Min mask width = min percent; max mask width = `100 - maxPercent`.
* markers: min, max, `ctrl = latencyControlMs ?? latencyTargetMs ?? latencyMs`,
  `smoothed = latencySmoothedMs`, `target = latencyTargetMs ?? latencyMs`
  (target offset `- 2px`, the rest `- 1px`).
* readouts: `tf('status.minValue'|'status.maxValue', {value: formatNumber(v,0)})`,
  `'—'` when null.
* threshold dots from the adaptive-resampling settings, all clamped to
  `[0, maxMs]`, all dimmed to opacity `0.28` when
  `adaptiveResamplingEnableFarMode !== true`:
  * near-low = `target - adaptiveResamplingLowRecoverEntryMarginMs`
  * low-exit = `target - adaptiveResamplingLowRecoverExitMarginMs`
  * near-high = `target + adaptiveResamplingHighRecoverEntryMarginMs`

**Renderer performance gauge** (`renderRenderTimeUI`, 415-576):

* the whole block is hidden unless `app.oscMeteringEnabled === true`.
* instantaneous segments (from the per-message values, not the windows):
  `dec = max(0, decodeTimeMs)`, `rndTotal = max(0, renderTimeMs)`,
  `cro = min(rndTotal, max(0, crossoverTimeMs))`, `rnd = max(0, rndTotal - cro)`,
  `wri = max(0, writeTimeMs)` — i.e. **crossover is carved out of render**.
* `stageMax(name) = timingStats?.[name]?.max?.max ?? null`,
  `stageAvg(name) = timingStats?.[name]?.avg?.mean ?? null`.
  `croMax = min(rndTotalMax ?? +∞, croMaxRaw)`,
  `rndMax = max(0, rndTotalMax - (croMax ?? 0))`; same carve-out for the avgs.
* scale: `frameDurationMs` when finite and > 0, else
  `max(0.01, dec+cro+rnd+wri, decMax+croMax+rndMax+wriMax)`.
* the four bars are drawn as *stacked* `clipPath: inset(0 R% 0 L%)` segments in
  the order decode → crossover → render → write; the max markers are placed at
  the **cumulative** sums (`decMax`, `decMax+croMax`, `+rndMax`, `+wriMax`),
  each hidden only when *every* contributing term is null.
* numeric readouts use the **averages**, passed through a 350 ms hold
  (`RENDER_TIME_DISPLAY_HOLD_MS = 350`, `getStableRenderPerfValue`, 116-128) so
  the text does not flicker: a new value is adopted only when the previous one
  is null or ≥350 ms old; a non-finite value clears immediately.
* format `msWithPct(ms)`: `'—'` when null/non-finite, else
  `` `${formatNumber(ms,3)} ms` ``, and when a frame budget exists append
  `` ` (${formatNumber(pct, pct>=10 ? 0 : 1)}%)` `` with
  `pct = ms / frameDurationMs * 100`.
* i18n keys: `renderer.perf.decode|render|crossover|write|max|frame`.

### 2.4 Latency statistics — the complete list

Raw window span: `LATENCY_RAW_WINDOW_MS = 4000` (`osc_listener.rs:1800`), fed
**only** by `/omniphony/state/latency_instant`.

| `latency:*` event | AppState field (`RuntimeLatencyState`) | Transform |
|---|---|---|
| `latency` | `latencyMs: Option<i64>` | `value.round() as i64` |
| `latency:instant` | `latencyInstantMs: Option<i64>` | `round()`; **also** `record_timing(Latency, value)` with the *unrounded* f64 |
| `latency:control` | `latencyControlMs: Option<i64>` | `round()` |
| `latency:smoothed` | `latencySmoothedMs: Option<f64>` | stored as-is |
| `latency:downstream` | `latencyDownstreamMs: Option<i64>` | `round()` |
| `latency:target` | `latencyTargetMs: Option<i64>` | `round()` |
| `latency:requested` | `latencyRequestedMs: Option<i64>` | `round()` |
| `latency:avail_input` | `latencyAvailInputMs: Option<f64>` | as-is (no listener; snapshot only) |
| `latency:output_fifo` | `latencyOutputFifoMs: Option<f64>` | as-is |
| `latency:resampler_pending` | `latencyResamplerPendingMs: Option<f64>` | as-is |
| `latency:stats` | not stored in AppState | `{latency:{min,max,mean}|null, …}` |

Also: `apply_audio_domain_state` sets **both** `latency_target_ms` and
`latency_requested_ms` from the audio domain's `latencyTargetMs`
(`osc_listener.rs:530-533`) — already covered natively by `apply.rs`.

Display formatting (`renderLatencyDisplay`, `latency.js:132-170`):

* raw `` `${formatNumber(latencyInstantMs, 0)} ms` `` or `'—'`
* ctrl `` `ctrl ${formatNumber(latencyControlMs, 0)} ms` `` or `'ctrl —'`
* smoothed `` `smoothed ${formatNumber(latencySmoothedMs, 2)} ms` `` — **2
  decimals**, the only one — or `'smoothed —'`
* downstream `` `path ${formatNumber(latencyDownstreamMs, 0)} ms` `` or `'path —'`
* fallback single line when there is no raw element:
  `tf('status.latencyFallback', {raw, ctrl})`
* target input: `String(max(1, round(latencyRequestedMs ?? latencyTargetMs ??
  latencyMs)))`, only written while **not** `latencyTargetEditing` and not
  `latencyTargetDirty`. Its Apply button is enabled iff `latencyTargetDirty`.
* `applyLatencyTargetNow` (583-593): `requested = max(1, round(input))`,
  optimistically writes `latencyRequestedMs` and `latencyTargetMs`, clears both
  flags, then `invoke('control_latency_target', {value: requested})`.

### 2.5 Meter dB mapping and the derived master meter

Two different `METER_DB_MIN` values exist and must not be confused:

* `-60.0` — the **meter scale bottom**: `WEB/src/mute-solo.js:52`,
  `WEB/src-tauri/src/peak_hold.rs:30`, and the local
  `osc_listener.rs:1939 METER_DB_MIN` used by the derived master meter.
* `-100` — the raw clamp the OSC parser applies to levels, and the JS
  fallback used in `updateSpeakerLevel` / `updateMasterLevel` /
  `updateHeadphoneMeter` when a field is absent (`speakers.js:2385-2387`,
  `tauri-bridge.js:112-113`) and the floor of the meter decay.

`dbToMeterPercent(db)` (`mute-solo.js:55-59`) — identical to
`peak_hold::db_to_meter_percent`:
`clamp(((db - (-60)) / (6 - (-60))) * 100, 0, 100)`, non-finite → `-60`.

`updateMeterUI` (`mute-solo.js:88-110`):

* `rmsDb = meter.rmsDbfs ?? METER_DB_MIN`, `peakDb = meter.peakDbfs ?? rmsDb`;
* **bar height is the PEAK**, not the RMS (so the fill rises to the hold marker
  on transients); the numeric readout is the RMS,
  `` `${formatNumber(rmsDb, 1)} dB` ``;
* cursor at `dbToMeterPercent(meter.peakHoldDbfs ?? peakDb)`, opacity `1` when
  `holdPercent > 0.1` else `0`, CSS class `over` toggled on `holdDb >= 0`
  (cursor turns red inside the headroom zone);
* when the entry has no `peakCursor` element, the readout falls back to
  `formatLevel(meter)` = `'— dB'` when null, else `${formatNumber(rmsDbfs,1)} dB`.

**Derived master meter** (`derived_master_meter`, `osc_listener.rs:1955-1986`).
Used **only** when the renderer has never published
`/omniphony/meter/master` (`s.master_level.is_none()`), checked in the
`MeterSpeaker` arm (2331-2335) and queued as a *batched* `master:meter`, so it
adds one entry to the 16 ms window rather than an emit per speaker message.

```
if speaker_levels.is_empty() -> None
peak_dbfs = max(METER_DB_MIN=-60, max over speakers of meter.peak_dbfs)
sum_squares = Σ (10^(rms_dbfs/20))²
rms_linear  = sqrt(sum_squares / speaker_count)
rms_dbfs    = rms_linear > 0 ? 20*log10(rms_linear) : METER_DB_MIN
payload = { meter: {peakDbfs, rmsDbfs, peakHoldDbfs: peak_hold("master", peak_dbfs)},
            derived: true }
```

Rationale and pinned behaviour (tests at `osc_listener.rs:3487-3587`): peak is
the loudest speaker peak; N equal speakers sum to their own level (power
addition, not amplitude); a quiet level like −80 dB converts normally and is
**not** snapped to the floor — the floor only applies when the summed linear
energy underflows to exactly 0; the peak floor is −60, not the parser's −100;
and `derived: true` lets the UI tell a reconstructed meter from a reported one
(for display/diagnosis, never for choosing a code path).

**Meter decay** (frontend-side, `WEB/src/speakers.js:2410-2461`, driven from the
rAF loop `app.js:345`) — this is UI ballistics, separate from the peak hold:

* `METER_DECAY_START_MS = 250`, `METER_DECAY_DB_PER_SEC = 45`
  (`WEB/src/state.js:625-626`);
* a meter starts decaying only once `now - lastSeen >= 250 ms`
  (`sourceLevelLastSeen` / `speakerLevelLastSeen`, which the native crate
  already mirrors as `Live::source_level_seen` / `speaker_level_seen`);
* `decayDb = 45 * dt_seconds`, applied to **both** `peakDbfs` and `rmsDbfs`,
  floored at `-100`;
* when any speaker changed, `dirty.masterMeter = true`.

### 2.6 Clip detection

* Source: `/omniphony/state/clip` → `OscEvent::StateClip { speaker: i32 }` →
  `clip:detected {speaker}`. The renderer flags **any** output clip,
  independently of auto-gain, and carries the offending speaker index.
* `flashClipIndicator()` (`WEB/src/controls/master.js:89-102`): adds
  `clip-active` to the master clip indicator, clearing/restarting a **1000 ms**
  timer that removes it.
* `flashSpeakerClip(index)` (`WEB/src/speakers.js:779-804`): guards
  `Number.isInteger(index) && index >= 0`, targets `speakerItems.get(String(index))
  .idStrip`, removes `clip-flash`, forces a reflow (`void
  target.offsetWidth`) so the animation replays, re-adds `clip-flash`, and
  clears it after **1000 ms** (per-speaker timer map, restarted on repeat).
* Separately, the *peak cursor* turns red (`.over`) whenever the held peak
  reaches 0 dBFS — that is a different, level-driven indication.
* Native: `live.clip = Some((speaker_index, Instant::now()))` already exists
  (`dispatch.rs:460-463`); the 1 s decay of the highlight is the UI's job.

---

## 3. Local-renderer auto-start watchdog and the connection state machine

### 3.1 Shared state

`WEB/src-tauri/src/main.rs:49-73`:

```rust
#[derive(Default)]
pub(crate) struct WatchdogControl {
    pub attempts: u8,                          // consecutive fast-fail spawns since the last re-arm
    pub cooldown_until: Option<Instant>,       // no spawn before this instant
    pub last_spawn_at: Option<Instant>,        // fast-fail detection window
    pub check_requested_at: Option<Instant>,   // set on a goodbye broadcast
    pub suppressed: bool,                      // set by a manual Stop
}
impl WatchdogControl { pub fn rearm(&mut self) { self.attempts = 0; self.cooldown_until = None; self.suppressed = false; } }
```

Held as `SharedState::watchdog: Arc<Mutex<WatchdogControl>>` alongside
`renderer_child: Arc<Mutex<Option<std::process::Child>>>` (`main.rs:75-86`).

Who touches it:

| Action | Effect |
|---|---|
| `save_osc_config` (`commands/app.rs:74-75`) | `rearm()` — a settings change re-arms after a failure streak |
| `launch_orender` (`commands/orender.rs:808-809`) | `rearm()` before spawning — a manual launch is deliberate |
| `stop_orender` (`commands/orender.rs:815-818`) | `suppressed = true`, then sends `/omniphony/control/quit` |
| `install_orender_service` (`commands/orender.rs:467`) | `suppressed = true` — a service-managed renderer is someone else's job |
| `spawn_orender_process` (`commands/orender.rs:755-759`) | `last_spawn_at = now`, `check_requested_at = None` |
| goodbye broadcast (`osc_listener.rs:1266-1267`) | `check_requested_at = Some(now)` |

### 3.2 Constants

`WEB/src-tauri/src/osc_listener.rs:22-41`

| Constant | Value | Meaning |
|---|---|---|
| `HEARTBEAT_INTERVAL` | 5 s | heartbeat cadence |
| `HEARTBEAT_ACK_TIMEOUT` | 10 s | no ack for this long → drop to `reconnecting` + re-register |
| `SNAPSHOT_REQUEST_INTERVAL` | 1 s | while `!osc_snapshot_ready`, re-register this often |
| `WATCHDOG_INTERVAL` | 1 s | watchdog tick cadence inside the OSC loop |
| `WATCHDOG_DISCONNECT_DEBOUNCE` | 6 s | how long the link must be down before auto-starting |
| `WATCHDOG_GOODBYE_GRACE` | 500 ms | after a goodbye, wait this long before probing the port |
| `WATCHDOG_FAST_FAIL_WINDOW` | 5 s | a child exiting within this of its spawn counts as a failure |
| `WATCHDOG_COOLDOWN` | 5 s | backoff after a failed attempt |
| `WATCHDOG_MAX_ATTEMPTS` | 3 | failure streak after which the watchdog gives up until re-armed |

Native today: `HEARTBEAT_INTERVAL = 5 s`, `HEARTBEAT_TIMEOUT = 16 s` (**not**
10 s), `SNAPSHOT_REQUEST_INTERVAL = 1 s` (`NAT/src/osc/mod.rs:30-36`). No
watchdog, no status FSM, no producer-epoch check, no goodbye handling.

### 3.3 Connection state machine

`emit_osc_status(app, state, status)` (`osc_listener.rs:843-853`) is the only
writer of `osc:status`. Its side effect matters as much as the event:

```rust
if status != "connected" {
    s.reset_runtime_state();      // app_state.rs:675-685
    s.osc_snapshot_ready = false;
}
s.osc_status = Some(status);
app.emit("osc:status", { "status": status });
```

`reset_runtime_state` rebuilds the whole `AppState` from scratch, **keeping
only** `layouts`, `selected_layout_key`, `osc_metering_enabled` and `log_level`.
Everything else — sources, meters, gains, mutes, renderer/audio/input domains,
`producer_capabilities`, `producer_epoch`, the dedup hashes — is dropped.

Transitions:

| From | Event | To | Where |
|---|---|---|---|
| — | socket bind failure | `error` (then the thread returns) | 920-927 |
| — | thread start, after the initial `register` + metering | `reconnecting` | 949 |
| any | `/omniphony/state/shutdown` (goodbye broadcast) | `reconnecting`, `is_connected = false`, `watchdog.check_requested_at = Some(now)` | 1262-1269 |
| any | `/omniphony/heartbeat/ack` **with a changed producer epoch** | re-register + re-send metering, `is_connected = false`, `reconnecting` | 1272-1282 (and 1325-1339 inside a bundle) |
| not connected | `/omniphony/heartbeat/ack` (epoch unchanged or first) | `connected` | 1283-1286 |
| connected | `/omniphony/heartbeat/unknown` | re-register + metering, `last_ack_at = now`, `reconnecting` | 1289-1299 |
| not connected | **any** successfully parsed OSC message | `connected` | 1312-1316 |
| connected | `HEARTBEAT_ACK_TIMEOUT` (10 s) elapsed at a heartbeat tick | `reconnecting`, then re-register + `last_snapshot_request_at = now` + metering | 1042-1056 |
| any | `OscControlMsg::Reconnect{host, rx_port, listen_port}` | re-register, reset timers, `is_connected = false`, `reconnecting` | 1007-1022 |

**Producer-epoch check** (`producer_epoch_changed`, 861-880): the first `Int`
argument of `/heartbeat/ack` is the renderer's instance epoch.

* no epoch latched yet → latch it, return `false` (a fresh connection is not a
  change);
* same epoch → `false`;
* different epoch → latch the new one, return `true` — a *different renderer
  instance* now answers on the RX port (a CLI⇄mpv swap behind an otherwise
  unbroken link), so a full re-handshake is forced to refresh capabilities, the
  object snapshot (names) and the metering subscription.

An ack from an older renderer that carries no `Int` argument is never a change.
`producer_epoch` is `#[serde(skip)]` (never sent to the UI) and is cleared by
`reset_runtime_state`, so it re-latches on each connection.

**Snapshot re-registration** (1033-1039): every loop tick, if
`!osc_snapshot_ready` and `last_snapshot_request_at.elapsed() >= 1 s`, re-send
`/omniphony/register <listen_port>` **and** `/omniphony/control/metering`, and
reset the timer. `osc_snapshot_ready` is set true only by
`OscEvent::StateSnapshotComplete` (2728-2732) and cleared by every non-connected
status change.

**Frontend reaction** — `setOscStatus(next)`, `WEB/src/controls/osc.js:287-342`:

* on any non-`connected`: `app.oscSnapshotReady = false`;
  `app.lastAutoOpenedInputError = null` (re-arms the Audio Input auto-open for
  the next connection);
* leaving `connected` (`disconnected`): `clearOscLaunchPending()` (clears
  `oscLaunchPending` and its 12 s timer) and `sourceNames.clear()` — so a
  renderer taking over the port cannot inherit stale object labels;
* → `connected`: cancel the config auto-open timer; if a launch was pending,
  clear it and close the config panel; if the producer is embedded (mpv), close
  the config panel;
* → `initializing`: cancel the auto-open timer and **open** the config panel;
* → `reconnecting`: if the previous state was `initializing`, or a launch is
  pending, or we just disconnected → `scheduleOscConfigAutoOpen()` (a **3000 ms**
  timer that opens the panel if still not connected);
* → `error`: cancel the timer, open the config panel, clear the launch pending;
* on an actual change: `pushLog('info', tf('log.oscStatus', {status:
  t(`status.${next}`)}))`.
* Always: `updateConfigSavedUI()` then `renderOscStatus()`.

`renderOscStatus` (osc.js:48-175) additionally calls `syncRuntimeConnectionLock()`
(`WEB/src/runtime-connection.js`) which **disables every control in
`#overlay`/`#speakersOverlay`** while `app.oscStatusState !== 'connected'`,
except an explicit exempt list (the OSC config form and its buttons, the panel
toggle buttons, the panel collapse buttons, the renderer/binaural tab buttons,
and `mpvOrenderToggle`). It remembers each element's previous `disabled` state
in `dataset.runtimeLockPrevDisabled` and restores it on reconnect. Status dot
colours: `initializing #89a3ff`, `connected #52e2a2`, `reconnecting #ffb347`,
`error #ff5d5d`, unknown `#7f8a99`.

Status text is `t('status.' + oscStatusState)`, and while connected with a
capability handshake it is suffixed `` ` · ${flavour}` `` where flavour is
`producerHost() || producerVariant()` for an embedded producer ("mpv"),
`'service'` when `app.orenderServiceRunning`, else `producerHost() ||
producerVariant()` ("cli").

### 3.4 `watchdog_tick`

`osc_listener.rs:1101-1242`, called every `WATCHDOG_INTERVAL` (1 s) from the
OSC loop (959-968) with the current `is_connected` and a
`disconnected_since: Option<Instant>` that starts as `Some(Instant::now())`.

**Step 1 — reap the child, whatever the connection state** (1111-1153). Locks
are taken sequentially, never nested, to keep one lock order with the spawn path
and the exit hook.

* `renderer_child.try_wait()` → `Ok(Some(status))` means the child exited:
  clear the slot and remember the status.
* If it exited, take `watchdog.last_spawn_at`; `fast_fail = last_spawn_at
  .elapsed() < WATCHDOG_FAST_FAIL_WINDOW` (5 s), `false` when there was no spawn
  record.
  * fast fail → `attempts += 1`, `cooldown_until = now + 5 s`, warn; and when
    `attempts >= 3`, log an error and emit
    `orender:autostart {"status":"failed"}` (**no `error` field on this path**).
  * otherwise (yielded to mpv, manual/Studio quit) → log info, `attempts = 0`.

**Step 2 — bail out while connected** (1155-1159): `disconnected_since = None`,
`watchdog.check_requested_at = None`, return.

**Step 3 — gate the spawn.** In order:

1. `since = *disconnected_since.get_or_insert_with(Instant::now)`.
2. `goodbye_ready = check_requested_at.map(|at| at.elapsed() >= 500 ms)
   .unwrap_or(false)`. If `!goodbye_ready && since.elapsed() < 6 s` → return.
   (A goodbye short-circuits the 6 s debounce after only 500 ms.)
3. Re-read `config.yaml` **at check time** so panel edits apply immediately
   (`crate::config::load_config`). Return unless
   `cfg.auto_start_renderer && host_is_local(host)`.
   `host_is_local` (`commands/app.rs:37-44`): empty, `localhost`
   (case-insensitive), `::1`, or a `127.` prefix.
4. `wd.suppressed` → return (a manual Stop keeps the renderer stopped until the
   user acts again).
5. `wd.attempts >= WATCHDOG_MAX_ATTEMPTS` → return.
6. `wd.cooldown_until` in the future → return.
7. A tracked child still running (`try_wait() == Ok(None)`) → return (it is
   still starting up).
8. `crate::commands::orender::orender_service_running()` → return (a
   service-managed renderer is someone else's responsibility).
9. **Port probe**: `UdpSocket::bind(("0.0.0.0", osc_rx_port))` — wildcard, the
   way renderers bind. On success drop the probe and continue; on failure
   (something holds the port, e.g. an mpv-embedded renderer we lost contact
   with) set `check_requested_at = None` and return.

**Step 4 — spawn** (1220-1241): `autostart_orender(app, shared)` resolves a
launch spec from the saved OSC config (binary discovery only — no user-supplied
path, no log level; `commands/orender.rs:768-786`) and spawns it.

* `Ok(info)` → log the command and emit `orender:autostart {"status":"launched"}`.
* `Err(e)` → `attempts += 1`, `cooldown_until = now + 5 s`, log the error, and
  **only when `attempts >= 3`** emit
  `orender:autostart {"status":"failed", "error": e}`.

`spawn_orender_process` (`commands/orender.rs:715-766`) writes the child's
stdout+stderr to `default_orender_log_path()`, uses `stdin(null)`, sets
`CREATE_NO_WINDOW | NORMAL_PRIORITY_CLASS` on Windows, stores the `Child` in
`renderer_child` (a still-running previous child is left alone — it owns the
OSC port and the new instance negotiates it via `--osc-yield`), then sets
`last_spawn_at = now` and `check_requested_at = None`. It returns
`{command, logPath}`.

**Exit hook** (`main.rs:432-477`): on `RunEvent::ExitRequested`, a running
auto-tune is cancelled first (the renderer may outlive Studio on half-swept
values); then, unless `config.keep_renderer_alive_on_quit`, Studio sends
`/omniphony/control/quit`, polls `try_wait()` for **2 s** at 50 ms intervals,
and kills the child if it has not exited.

**Foreign-renderer detection** (`WEB/src/controls/config.js:126-132`) is a
frontend concept, not a host one:

```js
rendererIsForeign():
  isEmbeddedProducer()            -> false   // mpv is never the binary Studio would launch
  running  = app.renderExecutable ?? ''      // from /state/render/executable
  expected = app.expectedOrenderPath ?? ''   // from invoke('expected_orender_path') at boot
  !running || !expected           -> null    // cannot cry wolf before the first snapshot
  else                            -> running !== expected
```

The banner (`osc.js:100-111`) shows `tf('status.foreignRendererDetail',
{running, expected})` only when the result is strictly `true`.

**Bridge-error detection** (`osc.js:81-97`): `bridgeError =
app.renderBridgeError.trim()`. When non-empty, a red banner is shown with the
detail text and the generic "no renderer connected" hint is suppressed even
while `reconnecting`/`initializing`.

---

## 4. `src/state.js` — the `app` object, `dirty` flags, `flush.js`, `getLiveOption`

### 4.1 Collections exported alongside `app` (`WEB/src/state.js:15-56`)

Mutable-by-reference `Map`/`Set`/array registries. Native equivalents in
`AppState`/`Live` noted where they exist.

| Export | Contents | Native |
|---|---|---|
| `sourceMeshes`, `sourceLabels`, `sourceOutlines`, `sourceEffectiveMarkers`, `sourceEffectiveLines`, `sourceBaseColors` | three.js scene objects per object id | view-layer, N/A |
| `sourceLevels` | id → `{peakDbfs, rmsDbfs}` (mutated in place by the decay) | `AppState::source_levels` |
| `speakerLevels` | index → meter | `AppState::speaker_levels` |
| `masterLevel` (live binding + `setMasterLevel`) | `{peakDbfs, rmsDbfs}` or `null` | `AppState::master_level` |
| `sourceLevelLastSeen`, `speakerLevelLastSeen` | id → `performance.now()` | `Live::source_level_seen`, `Live::speaker_level_seen` |
| `sourceGains` | id → `[f64]` per-speaker gains | `AppState::object_speaker_gains` |
| `sourceBandGains` | id → `[[f64]]` per band | `AppState::object_band_gains` |
| `speakerGainCache` | id → gain | `AppState::speaker_gains` |
| `speakerBaseGains` | id → pre-solo gain | UI-only (solo bookkeeping) |
| `speakerDelays` | id → ms | `Speaker::delay_ms` in the live layout |
| `speakerMuted`, `objectMuted` | id sets | `AppState::speaker_mutes`, `object_mutes` |
| `speakerManualMuted`, `objectManualMuted` | ids the *user* muted (vs solo-implied) | UI-only |
| `speakerItems`, `objectItems` | id → DOM row handles | UI-only |
| `sourceNames`, `sourceTags` | id → label / source tag | `SourcePosition::name`, `.source_tag` |
| `sourcePositionsRaw` | id → last position payload | `AppState::sources` |
| `sourceSizes` | id → `{w,d,h}` in [0,1] | `Live::object_sizes` (as `[f32;3]`) |
| `sourceDirectSpeakerIndices` | id → speaker index | `SourcePosition::direct_speaker_index` |
| `sourceTrails` | id → `{positions, line}` | `Live::trails` (`TrailPoint`, `TRAIL_MIN_POINT_INTERVAL = 70 ms`, `TRAIL_MAX_POINTS = 240`) |
| `layoutsByKey` | key → layout | `AppState::layouts` |
| `speakerMeshes`, `speakerLabels`, `speakerBandBars` | arrays indexed by speaker slot | view-layer |
| `speakerReorderAnimations` | `WeakMap` | view-layer |

### 4.2 `dirty` flags and per-item dirty sets

`WEB/src/state.js:68-94`:

```js
export const dirtyObjectMeters   = new Set();
export const dirtySpeakerMeters  = new Set();
export const dirtyObjectPositions= new Set();
export const dirtyObjectLabels   = new Set();

export const dirty = {
  masterMeter:false, roomRatio:false, hybrid:false, vbapMode:false,
  renderBackend:false, vbapCartesian:false, vbapPolar:false, loudness:false,
  adaptiveResampling:false, distanceDiffuse:false, distanceModel:false,
  configSaved:false, latency:false, renderTime:false, resample:false,
  audioFormat:false, drcUI:false, masterGain:false, autoGain:false,
  autoGainCeiling:false
};
```

Scheduling (`WEB/src/flush.js:72-78`): `scheduleUIFlush()` sets
`app.uiFlushScheduled = true` and queues `flushUI` on the next
`requestAnimationFrame`; re-entrant calls are ignored. `flushUI` clears the
flag first, then processes in this fixed order (flush.js:80-234):

1. `dirtyObjectMeters` → `updateMeterUI(entry, sourceLevels.get(id), 'source',
   id)` + `flushCallbacks.updateObjectContributionUI`; then `.clear()`
2. `dirtySpeakerMeters` → `updateMeterUI(entry, speakerLevels.get(id),
   'speaker', id)` + `updateSpeakerContributionUI`; `.clear()`
3. `dirtyObjectPositions` → per-axis text `` `${axis}:${coords[axis]}` `` when
   the row has `axisElems`, else `formatPosition(pos)`, plus
   `updateObjectPositionIcon`; `.clear()`
4. `dirtyObjectLabels` → `flushCallbacks.applyObjectIdentity(entry, id)`, or a
   raw-name fallback; `.clear()`
5. the boolean flags below, each running its callback then resetting itself.

| Flag | Callback(s) run in `flushUI` | Set by (`file:line`) |
|---|---|---|
| `masterMeter` | `updateMasterMeterUI` | `speakers.js:2395` (`updateSpeakerLevel`), `:2406` (`updateMasterLevel`), `:2458` (decay changed a speaker) |
| `roomRatio` | `renderRoomRatioDisplay` **and** `onRoomRatioChanged()` (object-injection faces are drawn at the room's proportions) | `controls/room-geometry.js:792` |
| `vbapMode` | `renderEvaluationMode` | `controls/vbap.js:360`, `:679` |
| `renderBackend` | `renderRenderBackend` | `controls/vbap.js:677` |
| `hybrid` | `renderHybridOptions` | `controls/vbap.js:678`, `:750` |
| `vbapCartesian` | `renderVbapCartesian` | `controls/vbap.js:804` |
| `vbapPolar` | `renderVbapPolar` | `controls/vbap.js:865`, `:876` |
| `loudness` | `renderLoudnessDisplay` | `controls/master.js:197` |
| `adaptiveResampling` | `renderAdaptiveResamplingUI` | `controls/adaptive.js:360`, `controls/audio.js:91` |
| `distanceDiffuse` | `renderDistanceDiffuseUI` | `controls/distance-diffuse.js:93` |
| `distanceModel` | `renderDistanceModelUI` | `controls/master.js:226` |
| `configSaved` | `renderConfigSavedUI` | `controls/config.js:46` |
| `latency` | `renderLatencyDisplay` **and** `renderLatencyMeterUI` (one flag, two renders) | `controls/latency.js:173` (`updateLatencyDisplay`), `:409` (`updateLatencyMeterUI`) |
| `renderTime` | `renderRenderTimeUI` | `controls/latency.js:579` |
| `resample` | `renderResampleRatioDisplay` | `controls/latency.js:256`, `controls/adaptive.js:361` |
| `audioFormat` | `renderAudioFormatDisplay` | `controls/audio.js:297`, `controls/osc.js:249`, `options-binder.js:46` |
| `drcUI` | `renderDrcUI` | `init.js:571`, `:577`, `:582` |
| `autoGain` | `renderAutoGainUI` | `controls/master.js:62` |
| `autoGainCeiling` | `renderAutoGainCeilingUI` | `controls/master.js:80` |
| `masterGain` | `renderMasterGainUI` | `controls/master.js:44` |

Direct (non-batched) helper: `updateObjectSizeUI(id)` writes three CSS widths
`` `${clamp01(v)*100 |> toFixed(1)}%` `` immediately — "three CSS-width writes
are cheap" (`flush.js:272-283`).

`updateItemClasses(entry, isMuted, isDimmed)` toggles `is-muted` / `is-dimmed`
on the row root (`flush.js:240-243`).

**Port note.** egui repaints the whole frame; the dirty machinery is a DOM
optimisation and should **not** be reproduced. The useful residue is the
*grouping*: each flag names a coherent chunk of panel state, and the callback it
runs is the render function whose logic the corresponding egui widget must
reproduce. `Live::snapshot_epoch` (bumped on every `Change::Snapshot`,
`dispatch.rs:207-213`) is the native cache-invalidation signal.

### 4.3 The `app` object, field by field

`WEB/src/state.js:100-579`. "Writer" lists the authoritative writers; the
snapshot merge (`applyInitState`, §6) writes most of them too.

**Producer handshake**

| Field | Default | Meaning / writer |
|---|---|---|
| `producerCapabilities` | `null` | `/state/capabilities` JSON; via snapshot. Helpers: `hasProducerDomain(d)` (`capabilities.domains` includes `d`), `hasControlConfig(k)` (`capabilities.controlConfig`), `producerVariant()` (`capabilities.variant`, default `'standalone'`), `producerHost()` (`capabilities.host` or null), `isEmbeddedProducer()`, `supportsRealtimeKey(k)` (`capabilities.realtime`) |
| `producerSession` | `null` | `/state/session` JSON; via snapshot |

**Room geometry**

| Field | Default | Notes |
|---|---|---|
| `roomRatio` | `{width:1, length:2, height:1, rear:1, lower:0.5, centerBlend:0.5}` | renderer-facing ratios; Width pins the scale (`radius_m = Width/2`) |
| `roomGeometryExpanded` | `false` | UI |
| `roomGeometryBaselineKey` | `''` | dirty detection for the geometry form |
| `roomGeometryApplyTimer` | `null` | debounce handle |
| `metersPerUnit` | `1.0` | restored from `roomRatio.scaleM` in `applyRoomRatio` |

**Render backend / evaluation**

| Field | Default |
|---|---|
| `vbapCartesianState` | `{xSize:null, ySize:null, zSize:null, zNegSize:0}` |
| `vbapPolarState` | `{azimuthResolution:null, elevationResolution:null, distanceRes:null, distanceMax:null}` |
| `evaluationModeState` | `{selection:null, effective:null}` |
| `objectSizeIntervals` | `0` (0 = single table) |
| `renderBackendState` | `{selection:null, effective:null, effectiveLabel:null, capabilities:null, allowedEvaluationModes:[], frozenRoomRatio:false, frozenSpeakers:false, restoreBackendAvailable:false, availableBackends:[], backendParamValuesById:{}, hybrid:{externalBackend:null, internalBackend:null, curve:null, curveSmoothing:0, metric:'chebyshev'}}` |
| `hybridParamTab` | `null` — which inner backend's param tab is shown |
| `vbapPositionInterpolation` | `null` |
| `vbapAllowNegativeZ` | `null` |
| `vbapRecomputing` | `null` |
| `recomputeError` | `null` |
| `saveRequested` | `false` — set true by the Save button, cleared by `config:saved` / `config:save_error` |
| `saveError` | `null` |
| `vbapCartesianFaceGridEnabled` | `false` |

Guards: `isRoomRatioFrozen()` = `renderBackendState.frozenRoomRatio === true`;
`isSpeakerLayoutFrozen()` = `renderBackendState.frozenSpeakers === true`.

**Distance / master / loudness / config**

| Field | Default |
|---|---|
| `distanceDiffuseState` | `{enabled:null, threshold:null, curve:null, metric:'spherical', mirrorAxes:{x:true,y:true,z:false}}` |
| `distanceModel` | `'none'` (accepted: `none`, `linear`, `quadratic`, `inverse-square`) |
| `distanceModelMetric` | `'spherical'` (accepted: `spherical`, `chebyshev`) |
| `masterGain` | `null` |
| `autoGain` | `null` |
| `autoGainCeilingDb` | `-1.0` |
| `loudnessEnabled` / `loudnessSource` / `loudnessGain` | `null` |
| `configSaved` | `null` |

**Adaptive resampling** (defaults are the *pre-connect* UI values; the renderer
overwrites them on the first snapshot)

| Field | Default |
|---|---|
| `adaptiveResamplingEnabled` | `false` |
| `adaptiveResamplingPaused` | `false` |
| `adaptiveResamplingEnableFarMode` | `true` |
| `adaptiveResamplingForceSilenceInFarMode` | `false` |
| `adaptiveResamplingHardRecoverHighInFarMode` | `true` |
| `adaptiveResamplingHardRecoverLowInFarMode` | `false` |
| `adaptiveResamplingFarModeReturnFadeInMs` | `0` |
| `adaptiveResamplingKpNear` | `10.0` |
| `adaptiveResamplingKi` | `50.0` |
| `adaptiveResamplingIntegralDischargeRatio` | `0.25` (**inoperative — excluded from tuning**) |
| `adaptiveResamplingMaxAdjust` | `0.01` |
| `adaptiveResamplingHighRecoverEntryMarginMs` | `120` |
| `adaptiveResamplingUpdateIntervalCallbacks` | `10` |
| `adaptiveResamplingLowRecoverSettleStableMs` | `200` |
| `adaptiveResamplingLowRecoverEntryMarginMs` | `18` |
| `adaptiveResamplingLowRecoverExitMarginMs` | `6` |
| `adaptiveResamplingLowRecoverSettleMarginMs` | `6` |
| `adaptiveResamplingLowRecoverRefillDeltaAlpha` | `0.5` |
| `adaptiveResamplingControlSmoothingCutoffHz` | `0.5` |
| `adaptiveResamplingControlSmoothingOrder` | `1` |
| `adaptiveResamplingUsePreBridgeClock` | `false` |
| `adaptiveResamplingUseOutputPacing` | `false` |
| `adaptiveResamplingDisableBackpressure` | `false` |
| `adaptiveResamplingBand` | `null` (string from the renderer) |
| `adaptiveResamplingState` | `null` |

**Latency & performance**

`latencyMs`, `latencyInstantMs`, `latencyControlMs`, `latencySmoothedMs`,
`latencyDownstreamMs`, `latencyTargetMs`, `latencyRequestedMs` — all `null`.
`diagSchema` `null` (`{items:[{name,label,group,unit}]}`), `diagValues` `null`
(`{name: value}`), `decodeTimeMs`/`renderTimeMs`/`crossoverTimeMs`/
`writeTimeMs`/`frameDurationMs` `null`, `timingStats` `null` (§2.2),
`resampleRatio` `null`, `latencyTargetApplyTimer` `null`.

**Audio / options / input**

| Field | Default | Notes |
|---|---|---|
| `audioSampleRate` | `null` | |
| `rampMode` | `'sample'` | accepted: `off`, `frame`, `sample`, `interp` |
| `objectGenerators` | `[]` | schema from `objectGenerators:schema` |
| `objectGeneratorParams` | `{}` | live overrides (key→value) |
| `objectGeneratorLayoutHasHeight` | `true` | assume yes until told otherwise |
| `phantomSchema` | `[]` | from `phantom:schema` |
| `phantomParams` | `{}` | |
| `fixedChannelCatalog` | `[]` | |
| `fixedChannelProcessing` | `{stream:'idle', labels:[], phantom:'no_stream', height:'no_stream'}` | |
| `crossover` | `null` | `{engine, bands, cutoffsHz, taps, latencyMs}` |
| `outputChannelMappingUnroutable` | `[]` | speaker names unroutable in `by_name` |
| `virtualBed` | `null` | null = built-in canonical poses |
| `options` | `{}` | **the** value store for registry options |
| `optionsSchema` | `[]` | |
| `virtualBedMaterialized` | `false` | one-shot guard |
| `audioOutputDevice`, `audioOutputDeviceEffective` | `null` | |
| `audioOutputDevices` | `[]` | `[{value,label}]` |
| `audioOutputBackend` | `'device'` | `'device'` or `'file'` |
| `audioOutputFile` | `'-'` | |
| `audioOutputPipePath` | `''` | remembered so stdout↔pipe restores it |
| `audioOutputFileFormat` | `'raw_f32'` | |
| `audioOutputFileEditing` | `false` | |
| `orenderInputPipe` | `null` | |
| `audioSampleFormat`, `audioError` | `null` | |
| `inputMode` | `'pipe_bridge'` | |
| `inputModeDirty` | `false` | suppresses snapshot overwrite while dirty |
| `inputActiveMode` | `'pipe_bridge'` | |
| `inputApplyPending` | `false` | |
| `inputApplyAwaitingAck` | `false` | |
| `inputBackend`, `inputChannels`, `inputSampleRate`, `inputNode`, `inputDescription`, `inputStreamFormat`, `inputError` | `null` | |
| `drcMode` | `null` | |
| `supportedDrcModes` | `[]` | |
| `drcGain` | `1.0` | |
| `drcWeight` | `1.0` | clamped to `[0,1]` |
| `renderBridgePath`, `renderConfigPath`, `renderConfigStatus`, `renderVersion`, `renderExecutable`, `renderAbi`, `renderBridgeError` | `null` | `renderConfigStatus ∈ {'loaded','missing','parse_error', null}` |
| `expectedOrenderPath` | `null` | from `invoke('expected_orender_path')` at boot (`app.js:324-331`) |
| `liveInput` | `{backend:'pipewire', node:'', description:'', layout:'', clockMode:'dac', channels:2, sampleRate:192000, map:'7.1-fixed', lfeMode:'object'}` | |
| `liveInputClockModeDirty` | `false` | |

**OSC / service**

| Field | Default |
|---|---|
| `oscMeteringEnabled` | `false` |
| `oscSnapshotReady` | `false` |
| `oscStatusState` | `'initializing'` |
| `oscConfigAutoOpenTimer` | `null` (3000 ms) |
| `oscLaunchPending` | `false` |
| `oscLaunchPendingTimer` | `null` (12000 ms safety net, `osc.js:363-369`) |
| `oscConfiguredOrenderPath` | `''` |
| `oscConfigBaselineKey` | `''` |
| `orenderServiceInstalled` / `orenderServiceRunning` | `false` |
| `orenderServiceManager` | `null` |
| `orenderServicePending` | `false` |

**Editing / dirty guards** (`state.js:363-405`) — one `…Editing` + one `…Dirty`
pair per numeric input, so an incoming snapshot never clobbers a field the user
is typing in: `audioOutputDeviceEditing`, `audioSampleRateEditing`,
`latencyTargetEditing`/`latencyTargetDirty`, and the fourteen `adaptive*`
pairs (`adaptiveKpNear`, `adaptiveKi`, `adaptiveIntegralDischargeRatio`,
`adaptiveMaxAdjust`, `adaptiveHighRecoverEntryMargin`,
`adaptiveUpdateIntervalCallbacks`, `adaptiveFarFadeInMs`,
`adaptiveLowRecoverSettleStableMs`, `adaptiveLowRecoverEntryMarginMs`,
`adaptiveLowRecoverExitMarginMs`, `adaptiveLowRecoverSettleMarginMs`,
`adaptiveLowRecoverRefillDeltaAlpha`, `adaptiveControlSmoothingAlpha`), all
`false`. **This pattern must be reproduced in egui**: a text field bound to a
model value that the OSC stream also writes needs the same "do not overwrite
while focused/dirty" guard.

**Panel open flags** (all `false`): `telemetryGaugesOpen`,
`audioOutputSectionOpen`, `inputSectionOpen`, `rendererSectionOpen`,
`displaySectionOpen`, `drcSectionOpen`, `twoDSourcesSectionOpen`,
`autoGainSectionOpen`. Plus `lastAutoOpenedInputError: null` (rising-edge guard
for the Audio Input auto-open, §6).

**Selection & drag** (`state.js:407-459`) — `selectedSourceId`,
`selectedSpeakerIndex`, `draggedSpeakerIndex`, `draggedSpeakerInitialIndex`,
`draggedSpeakerDidDrop`, `draggedSpeakerRoot`, `polarEditArmed`,
`cartesianEditArmed`, `activeEditMode:'polar'`,
`channelEditCoordMode:'cartesian'`, `lastSpatialFrameAt:0`,
`isDraggingSpeaker`, `dragMode`, `dragAxis`, `dragAxisOrigin`,
`dragAxisDirection`, `dragSpeakerStartPosition`, `dragAxisStartT`,
`dragAzimuthDeg`, `dragElevationDeg`, `dragDistance:1`, `dragAzimuthDelta:1`,
`dragElevationDelta:1`, `pointerDownPosition`, `draggingPointerId`,
`dragEditTarget`, `isDraggingVirtualBed`, `draggingVirtualBedSourceId`,
`draggingVirtualBedChannel`, `channelEditPinId`, `channelEditPinPos`,
`channelEditPinUntil:0`. (Viewport concerns; phase 1 territory.)

**Trails & display** (`state.js:461-564`) — `trailsEnabled:true`,
`trailRenderMode:'diffuse'`, `trailPointTtlMs:7000`,
`trailTeleportThreshold:0.5`, `speakerHeatmapVolumeEnabled:false`,
`speakerHeatmapVolumeColormap:'heatmap'`, `heatmapBandIndex:0`,
`heatmapAllBands:true`, `globalEnergyHeatmapEnabled:false`,
`globalEnergyHeatmapScaleDb:6`, `discontinuityHeatmapEnabled:false`,
`discontinuityHeatmapMode:'gain'`, `discontinuityHeatmapScale:0.5`,
`objectEnergyHeatmapEnabled:false`, `objectEnergyColormap:'blueWhite'`,
`objectCustomGradientStops` / `speakerCustomGradientStops` (both
`[{pos:0,r:0,g:0,b:1},{pos:0.5,r:0,g:1,b:0},{pos:1,r:1,g:0,b:0}]`, 2..8 stops
kept sorted by `pos`), `speakerCustomGradientVersion:0`,
`objectEnergyVolumeMix:0.6`, `objectEnergyVolumeGammaAccumulate:4`,
`objectEnergyVolumeGammaMip:3`, `objectEnergyHeatmapResolution:64`,
`objectEnergyHeatmapFalloffRadius:0.5`, `objectEnergyHeatmapOpacity:1`,
`volumeRefreshMs:160`, `volumeSmoothInterpolation:false`,
`objectEnergyHeatmapBandCount:12`, `lastObjectEnergyHeatmapAt:0`,
`lastSpeakerSoloVolumeAt:0`, `lastGlobalEnergyVolumeAt:0`, `speakerSize:0.08`,
`effectiveRenderEnabled:false`, `objectsVisible:true`,
`objectColorsEnabled:false`, `objectLabelsEnabled:true`,
`showObjectDetails:true`, `speakerLabelsEnabled:false`,
`speakerBandBarsEnabled:false`, `speakerFaceListenerEnabled:false`,
`objectDisplayMode:'circle'`, `objectSphereSize:0.07`, `lastTrailDecayAt:0`.
These are **local display preferences**, not renderer state (except the overlay
ones mirrored through `overlay:state` / the `mpv_overlay_set_*` commands).

**Layout / flush / decay** — `currentLayoutKey:null`,
`currentLayoutSpeakers:[]`, `currentLayoutCutoffs:[]` (interior crossover edges
of the live layout), `uiFlushScheduled:false`, `lastMeterDecayAt:0`.

**Module constants** (`state.js:625-632`): `METER_DECAY_START_MS = 250`,
`METER_DECAY_DB_PER_SEC = 45`, `DEFAULT_SAMPLE_RATE_HZ = 48000`,
`AUDIO_SAMPLE_RATE_PRESETS = [0, 32000, 44100, 48000, 88200, 96000, 176400,
192000]`, `isLinux`. The timing-window spans deliberately **do not** live here
any more (comment at 628-630 points at `osc_listener.rs`).

### 4.4 `getLiveOption` and the options registry

`WEB/src/state.js:638-643`:

```js
export function getLiveOption(key) {
  const v = app.options[key];
  if (v !== undefined) return v;
  const spec = (app.optionsSchema || []).find((s) => s && s.key === key);
  return spec ? spec.default : undefined;
}
```

Precedence: **renderer snapshot (`app.options`) → published schema default →
`undefined`**. `undefined` means pre-connect, and callers fall back to their own
safe interpretation (e.g. `!== 'host'` still means spatial). There are no
per-option JS mirrors — "the lying hard-coded defaults died with phase 2".

`app.options` is filled by `Object.assign(app.options, payload.options)` in
`applyInitState` (`init.js:477-479`) from the `options` block of
`/state/renderer`, and optimistically by the binder on a local set.

**The binder** (`WEB/src/options-binder.js`). Markup contract:

```html
<button data-option="surround_placement" data-option-value="side">      <!-- toggle pair -->
<input type="checkbox" data-option="synthetic_objects_enabled">          <!-- bool -->
<input type="checkbox" data-option="…" data-option-on="X" data-option-off="Y"> <!-- enum-as-switch -->
<select data-option="phantom_extract_mode">…</select>
<select data-option="object_generator_id" data-option-empty="none">
<input type="number" data-option="crossover_fir_transition_ratio">
```

* `setOption(key, value)` (lines 36-49): optimistic `app.options[key] = value`;
  run the `AFTER_SET[key]` hook (only one exists: `object_generator_id` resets
  `app.objectGeneratorParams = {}`, mirroring the renderer); then
  `invoke('control_option', {key, value})` → OSC
  `/omniphony/control/option [key, value]`; then `reflectBoundOptions()`,
  `dirty.audioFormat = true`, `scheduleUIFlush()`. A failed invoke is logged to
  the console — "an optimistically-reflected control must never hide a dead send
  chain".
* `reflectBoundOptions()` (lines 81-101): for each `[data-option]`, read
  `getLiveOption`; `undefined` → leave the baked HTML default. BUTTON toggles
  `active` on `String(v) === dataset.optionValue`; SELECT skips while focused
  and maps `''`/`null` to `dataset.optionEmpty`; number input skips while
  focused; checkbox uses `String(v) === dataset.optionOn` when present, else
  `!!v`.
* `bindOptionControls()` is idempotent (`dataset.optionBound` guard) so
  late-mounted panels may re-run it.

**Schema payload shape** — produced by `RND/renderer/src/options.rs:554-591`
`schema_json()`, published on `/omniphony/state/options_schema` and forwarded as
`options:schema {value: <the JSON string>}`. It is a JSON **array** of:

```jsonc
{
  "key":     "<canonical snake_case>",
  "kind":    "bool" | "enum" | "string" | "float",
  "default": <bool|string|number>,
  "flags":   ["persist"?, "replan"?],
  "i18nKey": "<label key>",
  "values":  ["…"],          // enum only
  "min": …, "max": …, "step": …,  // float only (step is a UI hint, not a grid)
  "helpI18nKey": "…"         // only when the spec declares one
}
```

The current registry (`options.rs:140-372`) — the native panels can rely on
these but must still render generically from the received schema:

| key | kind | values | default | flags | i18nKey | helpI18nKey | legacy control address |
|---|---|---|---|---|---|---|---|
| `surround_placement` | enum | `side`, `back` | `"side"` | persist, replan | `twoDSources.surroundLabel` | — | `/omniphony/control/surround_placement` |
| `synthetic_objects_enabled` | bool | — | `false` | persist, replan | `twoDSources.syntheticObjectsLabel` | `help.syntheticObjects` | `/omniphony/control/synthetic_objects` |
| `output_channel_mapping` | enum | `by_index`, `by_name` | `"by_index"` | persist | `audio.channelMapping` | — | `/omniphony/control/output_channel_mapping` |
| `object_generator_id` | string | — | `""` | persist, replan | `twoDSources.objectGeneratorLabel` | `help.objectGenerator` | `/omniphony/control/object_generator` |
| `phantom_extract_mode` | enum | `off`, `broadband`, `spectral` | `"off"` | persist, replan | `twoDSources.phantomLabel` | `help.phantomExtract` | `/omniphony/control/phantom_extract` |
| `crossover_type` | enum | `lr4`, `fir` | `"lr4"` | persist | `renderer.crossoverTypeLabel` | `help.crossoverType` | `/omniphony/control/crossover_type` |
| `crossover_fir_transition_ratio` | float | — | `0.5` | persist | `renderer.crossoverTransitionLabel` | `help.crossoverFirTransition` | `/omniphony/control/crossover_fir_transition_ratio` |
| `hrir_update_lattice` | enum | `exact`, `fine`, `balanced`, `coarse` | `"exact"` | persist | `binaural.hrirUpdateLatticeLabel` | `help.hrirUpdateLattice` | `/omniphony/control/binaural/hrir_update_lattice` |

`flags`: `persist` = the renderer commits to `config.yaml` immediately on set;
`replan` = a change bumps `RendererControl::options_epoch` and re-plans
synthesized-object stages. Neither changes what the UI does — they are
informational.

The renderer's current values arrive as
`options_json` (`options.rs:543-549`), a flat `{key: value}` map inside the
`options` block of `/state/renderer` (mirrored into `AppState::options`,
`osc_listener.rs:650-652` / `NAT/src/osc/apply.rs`).

**`objectGenerators:schema`** — array of
`{id, label, i18nKey, requiresHeightLayer, params:[{key,label,i18nKey,min,max,step,default,unit}]}`
(documented at `state.js:239-249`). Live overrides live in
`app.objectGeneratorParams`, and `app.objectGeneratorLayoutHasHeight` (from the
renderer domain) gates whether a configured generator can actually run — when
false the control stays editable but the generator cannot run.

**`phantom:schema`** — array of
`{key,label,i18nKey,min,max,step,default,unit}` (`state.js:250-254`); overrides
in `app.phantomParams`.

Both are `{value: <JSON string>}` on the wire and are `JSON.parse`d in the
listener with a `catch → []`. Native `dispatch.rs:564-575` parses them straight
into `live.object_generators_schema` / `live.phantom_schema` /
`live.options_schema` as `Option<serde_json::Value>` (a parse failure leaves
`None`, not `[]` — a difference to be aware of when the UI iterates).

---

## 5. `src/log.js` and the `omniphony:log` event

### 5.1 The wire event

Parser: `WEB/src-tauri/src/osc_parser.rs:656-676` `parse_omniphony_log`.
Address must be exactly `/omniphony/log` (2 parts). Arguments, in order:

| # | Type | Field | Rule |
|---|---|---|---|
| 0 | `Long` or `Int`, **≥ 0** | `seq: u64` | anything else → the message is dropped entirely |
| 1 | `String` | `level` | required |
| 2 | `String` | `target` | required |
| 3 | `String` | `message` | required |

`LogEntry` (`osc_parser.rs:141-147`) serialises as
`{seq, level, target, message}` — that **is** the `omniphony:log` payload
(`osc_listener.rs:3051-3057`; the entry is serialised directly, not nested).

### 5.2 Frontend handling

`WEB/src/tauri-bridge.js:832-838`:

```js
const level   = normalizeLogLevel(payload?.level);
const target  = String(payload?.target || '').trim();
const message = String(payload?.message || '').trim();
if (!message) return;              // empty message -> dropped
pushLog(level, message, target);
```

Note `seq` is **discarded** — it is not used for ordering or de-duplication on
the JS side.

### 5.3 `log.js` internals (`WEB/src/log.js`)

```js
const LOG_ENTRY_LIMIT = 120;
export const LOG_LEVEL_VALUES = ['off','error','warn','info','debug','trace'];
export const logState = { expanded:false, entries:[], backendLogLevel:'info', filterText:'' };
```

* **Ring size 120.** `pushLog` appends then
  `entries.splice(0, len - 120)` when over — a plain array trimmed from the
  front, so the newest 120 survive (`log.js:98-110`).
* **Entry shape**: `{id: `${Date.now()}-${random36.slice(2,8)}`, level, target,
  message, timestamp: new Date()}`. `level` is coerced: only
  `warn|error|debug|trace` pass through, **everything else becomes `info`**
  (note: `off` therefore becomes `info` here, even though it is a valid
  *backend level*). `target` is `String.trim()`ed, `message` is `String(x ?? '')`.
* `normalizeLogLevel(v)` (93-96): lowercase-trim, must be in
  `LOG_LEVEL_VALUES`, else `'info'`.
* `normalizeLogError(e)` (81-91): `Error` → `message || name || String(e)`;
  string → itself; else `JSON.stringify` with a `String(e)` fallback.
* **Rendered message** (`buildRenderedMessage`, 50-56): if the message already
  starts with `[`, use it verbatim; else prefix `` `[${target}] ` `` when a
  target exists; else the bare message.
* **Filter** (`getFilteredEntries`, 69-73): case-insensitive substring of
  `buildSearchableText` = rendered message + target + level, joined by spaces,
  lowercased. Empty filter returns everything (the same array, not a copy).
* **Summary line** (`renderLogPanel`, 134-184): the *last* entry of the filtered
  list when a filter is active, otherwise the last entry overall; rendered with
  `buildRenderedMessage`; `t('log.empty')` when there is none.
* Panel classes: `expanded` / `collapsed` toggled on `#logOverlay`; toggle
  button text `▴` when expanded else `▾`, title `t('log.collapse')` /
  `t('log.expand')`.
* Empty-state text: `t('log.empty')` when there are no entries at all,
  `t('log.noMatch')` when a filter hid them all.
* Entries are rendered **newest first** (`visibleEntries.slice().reverse()`),
  each as three cells: time (`Intl.DateTimeFormat(locale, {hour:'2-digit',
  minute:'2-digit', second:'2-digit'})`), level (class
  `log-entry-level <level>`, text `t('log.level.' + level)`), and message
  (`log-entry-message`).
* **Copy** (`copyLogsToClipboard`, 191-216): copies the *filtered* entries via
  `serializeLogsForClipboard` — one line each, `` `[YYYY-MM-DD HH:MM:SS] LEVEL
  message` `` with the level uppercased — using `navigator.clipboard.writeText`
  with a hidden-`textarea` + `execCommand('copy')` fallback. Success pushes
  `tf('log.copySuccess', {count})`, failure `tf('log.copyFailed', {error})`,
  and an empty list pushes `t('log.copyEmpty')` without copying.
* **Clear** (`WEB/src/setup-listeners.js:81-86`): `#logClearBtn` sets
  `logState.entries = []` and re-renders. It does **not** touch the backend.
* **Level control** (`renderLogLevelControl`, 117-124) writes
  `#logLevelSelect.value` from `normalizeLogLevel(logState.backendLogLevel)`
  only when it differs (so it does not fight the user).
  On `change` (`setup-listeners.js:94-104`): normalise, set
  `logState.backendLogLevel`, re-render, `pushLog('info',
  tf('log.levelChanged', {value}))`, then `invoke('control_log_level', {value})`
  with a `pushLog('error', tf('log.oscConfigFailed', {error}))` on failure.
  The host command (`WEB/src-tauri/src/commands/engine.rs:30-46`) **re-validates**
  against `off|error|warn|info|debug|trace` and silently drops an invalid value,
  otherwise sends `/omniphony/control/log_level <value>` as a string.
* Inbound `state:log_level {value}` sets `logState.backendLogLevel =
  normalizeLogLevel(value)` and re-renders the select — that is how the
  renderer's own level reaches the UI (also carried in the snapshot as
  `logLevel`, `init.js:676-678`).
* i18n keys used: `log.title`, `log.levelLabel`, `log.filterLabel`,
  `log.filterPlaceholder`, `log.empty`, `log.noMatch`, `log.copy`,
  `log.copyEmpty`, `log.copySuccess`, `log.copyFailed`, `log.clear`,
  `log.expand`, `log.collapse`, `log.levelOption.{off,error,warn,info,debug,trace}`,
  `log.level.{info,warn,error,debug,trace}`.

**Other producers of log entries** (they share the same ring, so the native log
panel must accept programmatic pushes too): boot (`log.boot`), OSC status
changes (`log.oscStatus`), autostart (`log.orenderAutostartLaunched` /
`…Failed`), metering toggles, save/reload requests, layout import/export,
gaintable load/unavailable (`tauri-bridge.js:206-218`), config save errors,
mpv.conf changes, and every `.catch()` in the command wrappers.

**Native status: MISSING.** `OscEvent::Log` and `OscEvent::StateLogLevel` both
fall into `dispatch.rs`'s catch-all. `AppState::log_level` exists and is
preserved across `reset_runtime_state`, but nothing writes it.

---

## 6. `get_state` / `app_state.rs` serde output, and how `state.js` merges it

### 6.1 The command

`WEB/src-tauri/src/commands/app.rs:26-30`:

```rust
#[tauri::command]
pub fn get_state(state: State<SharedState>) -> serde_json::Value {
    serde_json::to_value(&*state.inner.lock().unwrap()).unwrap_or(Value::Null)
}
```

Called once at boot (`WEB/src/app.js:283-296`), with a staleness guard: if
`app.oscSnapshotReady` is already true and the returned
`payload.oscSnapshotReady === false`, the payload is **ignored** (a live OSC
snapshot has already overtaken the boot fetch) and `pushLog('debug', …)` is
emitted. Otherwise `applyInitState(payload)` + `pushLog('info',
t('log.stateLoaded'))`.

The same serialisation is what `state:snapshot_ready` carries
(`serde_json::to_value(&*s)` at `osc_listener.rs:2599`, `2612`, `2635`, `2648`,
`2661`, `2674`, `2687`, `2730`).

### 6.2 Snapshot shape

`WEB/src-tauri/src/app_state.rs:417-655`. All keys are camelCase (explicit
`#[serde(rename)]`); three sub-structs are `#[serde(flatten)]`ed, so their keys
sit at the top level.

**Top-level own fields**

`sources: {id: SourcePosition}`, `binaural: Value?` (skipped when None),
`options: Value?` (skipped when None), `sourceLevels: {id: Meter}`,
`speakerLevels: {id: Meter}`, `masterLevel: Meter?`,
`objectSpeakerGains: {id: [f64]}`, `speakerGains: {id: f64}`,
`objectMutes: {id: u8}`, `speakerMutes: {id: u8}`, `roomRatio: RoomRatio`,
`spread: SpreadState`, `loudness: u8?`, `loudnessSource: f64?`,
`loudnessGain: f64?`, `masterGain: f64?`, `autoGain: bool?`,
`autoGainCeilingDb: f64?`, `distanceDiffuse: DistanceDiffuse`,
`distanceModel: DistanceModelState`, `vbapCartesian: VbapCartesian`,
`vbapPolar: VbapPolar`, `renderBackendState: RenderBackendState`,
`renderEvaluationModeState: {selection, effective}`,
`objectSizeIntervals: u32`, `vbapAllowNegativeZ: bool?`, the 24
`adaptiveResampling*` fields (see §4.3 for the JS names — they match one for
one), `vbapRecomputing: bool?`, `recomputeError: String?` (skipped when None),
`saveError: String?` (skipped when None), `configSaved: u8?`,
`decodeTimeMs`/`renderTimeMs`/`crossoverTimeMs`/`writeTimeMs`/
`frameDurationMs`/`resampleRatio` (`f64?`), `inputMode`, `inputActiveMode`,
`inputApplyPending: u8?`, `drcMode`, `drcWeight: f32?`, `meterRateHz: f32?`,
`diagRateHz: f32?`, `supportedDrcModes: [String]`, `inputBackend`,
`inputChannels: u32?`, `inputSampleRate: u32?`, `inputNode`,
`inputDescription`, `inputStreamFormat`, `inputError`, `renderBridgePath`,
`renderConfigPath`, `renderConfigStatus`, `renderVersion`, `renderExecutable`,
`renderAbi`, `activeProfile: String?`, `profileNames: [String]`,
`renderBridgeError`, `liveInput: LiveInputState`, `orenderInputPipe`,
`oscStatus: String?`, `producerCapabilities: Value?`, `producerSession: Value?`,
`oscMeteringEnabled: u8?`, `logLevel: String?`, `lastSpatialSamplePos: i64?`,
`currentCoordinateFormat: u8`, `layouts: [Layout]`, `selectedLayoutKey: String?`,
`oscSnapshotReady: bool`.

**Never serialised** (`#[serde(skip)]`): `producer_epoch`,
`current_content_generation`, `object_band_gains`, `last_layout_state_hash`,
`last_snapshot_emit_hash`, `last_overlay_emit_hash`.

**Flattened `RuntimeLatencyState`** (`app_state.rs:319-355`): `latencyMs: i64?`,
`latencyInstantMs: i64?`, `latencyControlMs: i64?`, `latencySmoothedMs: f64?`,
`latencyDownstreamMs: i64?`, `latencyTargetMs: i64?`, `latencyRequestedMs: i64?`,
`latencyAvailInputMs: f64?`, `latencyOutputFifoMs: f64?`,
`latencyResamplerPendingMs: f64?`, `diagSchema: Value?`, `diagValues: Value?`.

**Flattened `RuntimeAudioState`** (357-379): `audioSampleRate: u32?`,
`rampMode: String?`, `audioOutputDevice`, `audioOutputDeviceEffective`,
`audioOutputDevices: [{value,label}]`, `audioOutputBackend`, `audioOutputFile`,
`audioOutputFileFormat`, `audioSampleFormat`, `audioError`.

**Flattened `LiveOptionsState`** (395-415, `rename_all = "camelCase"`):
`objectGeneratorParams: Value?`, `objectGeneratorLayoutHasHeight: bool?`,
`crossover: Value?`, `phantomParams: Value?`, `fixedChannelCatalog: Value?`,
`fixedChannelProcessing: Value?`, `outputChannelMappingUnroutable: [String]?`,
`virtualBed: Value?`. **`virtualBed` is deliberately not skipped when `None`**:
an explicit `"virtualBed": null` means "renderer reports no saved bed" and
triggers a one-shot canonical-bed materialisation.

**Nested value objects**

* `SourcePosition` (`app_state.rs:6-42`): `x`, `y`, `z`, then
  skip-if-none `coordMode`, `azimuthDeg`, `elevationDeg`, `distanceM`,
  `gainDb: i32`, `generation: u64`, `directSpeakerIndex: u32`, `fixed: bool`,
  `label`, `kind` (`"height"`/`"phantom"`), `name`, `sourceTag`.
* `Meter`: `{peakDbfs, rmsDbfs}`.
* `RoomRatio`: `{width, length, height, rear, lower, centerBlend, scaleM}`,
  default `{1, 2, 1, 1, 0.5, 0.5, 1.0}`.
* `SpreadState`: `{min, max, fromDistance, distanceRange, distanceCurve}`.
* `DistanceDiffuse`: `{enabled, threshold, curve, metric}` — note the wire
  carries `mirrorAxes` too and `applyInitState` reads it, but the typed struct
  does **not** declare it, so it only survives when the renderer sends it inside
  a domain payload the UI reads directly.
* `DistanceModelState`: `{value, metric}`.
* `VbapCartesian`: `{xSize, ySize, zSize, zNegSize}` (`u32?`).
* `VbapPolar`: `{azimuthResolution, elevationResolution, distanceRes,
  distanceMax, positionInterpolation}`.
* `RenderBackendState` (269-311): `{selection, effective, effectiveLabel,
  capabilities: BackendCapabilitiesState?, allowedEvaluationModes:[String],
  frozenRoomRatio, frozenSpeakers, restoreBackendAvailable, hybrid:
  {externalBackend, internalBackend, curve:[[f64;2]], curveSmoothing, metric},
  availableBackends: Value, backendParamValuesById: Value}`.
  `BackendCapabilitiesState`: `supportsRealtime`, `supportsPrecomputedPolar`,
  `supportsPrecomputedCartesian`, `supportsPositionInterpolation`,
  `supportsDistanceModel`, `supportsSpread`, `supportsSpreadFromDistance`,
  `supportsDistanceDiffuse`, `supportsTableExport` (all `bool`, each with a
  snake_case alias for input).
* `LiveInputState`: `{backend, node, description, layout, clockMode, channels,
  sampleRate, map, lfeMode}`.

**`RenderBackendState::sanitize()`** (`app_state.rs:194-262`) runs in the
**host** before the state is stored (`osc_listener.rs:644-649`), so the UI
receives validated data and does not re-check it: ids/modes/metric are trimmed
and lowercased (empty → None), hybrid inner backends must be a known non-hybrid
id (an empty `availableBackends` means "unknown", so any non-hybrid id is
accepted), curve points are dropped when non-finite and otherwise clamped to
`[0,1]²`, `curveSmoothing` clamped to `[0,1]`, and `metric` restricted to
`spherical|chebyshev`. **This must keep running natively** — `apply.rs` calls it
via `apply_renderer_domain_state`.

### 6.3 `applyInitState` — the merge rules

`WEB/src/init.js:93-716`. This is the single most important function to
reproduce in behaviour (not in structure — egui reads the model, so the
"re-render everything" half disappears; the *value-guard* half does not).

Order and semantics:

1. `applyProfilesState(payload)`, `applyBinauralState(payload.binaural)`,
   `setHeadPoseTarget(payload.binaural)`.
2. `producerCapabilities` / `producerSession` accepted only when they are
   objects, else `null`; then `applyProducerCapabilityVisibility()`.
3. `oscSnapshotReady` (boolean only) → `app.oscSnapshotReady` +
   `syncRuntimeConnectionLock()`.
4. `meterRateHz` / `diagRateHz` (numbers) → `syncMeterRateFromRenderer` /
   `syncDiagRateFromRenderer` (renderer is the source of truth; the UI mirrors
   into `localStorage`, never the other way round — meter rates allowed:
   `[10,20,50,100,200]`, default 50, key `audioMetering.rateHz.v1`, legacy key
   `diagPlot.meterRateHz.v1`).
5. **Hard reset** of `speakerGainCache`, `speakerMuted`, `objectMuted`,
   `speakerManualMuted`, `objectManualMuted`, then repopulate from `sources`,
   `sourceLevels`, `speakerLevels`, `objectSpeakerGains`, `speakerGains`,
   `objectMutes`, `speakerMutes` (a `0` value in the mute maps does **not** add
   the key).
6. `roomRatio` → `applyRoomRatio`, else `updateRoomRatioDisplay()` +
   `applyRoomRatioToScene()`.
7. `vbapCartesian`: `xSize`/`ySize`/`zSize` accepted per field only when
   `typeof === 'number'`, stored as `v > 0 ? v : null`; `zNegSize` as
   `v >= 0 ? v : 0`.
8. `renderBackendState`: hydration only (see `sanitize()` above). `selection`
   and `effective` only when strings; `effectiveLabel`/`capabilities` set to
   `null` when not the right type; `allowedEvaluationModes` → `[]` when not an
   array; the three `frozen*`/`restore*` booleans compared `=== true`;
   `availableBackends` → `[]` when not an array; `backendParamValuesById` →
   `{}` when not an object; `hybrid.externalBackend/internalBackend` via `??
   null`; `hybrid.curve` `Array.isArray ? … : null`; `metric` only when string;
   `curveSmoothing` only when `Number.isFinite`.
9. `objectSizeIntervals` when a number `>= 0`, `Math.round`ed.
10. `renderEvaluationModeState.selection` accepted only from
    `['auto','realtime','precomputed_polar','precomputed_cartesian']`;
    `.effective` from the same list **without `auto`**. Both trimmed+lowercased.
11. `vbapPolar` fields `> 0 ? v : null`; `positionInterpolation` boolean →
    `app.vbapPositionInterpolation`. `vbapAllowNegativeZ` boolean.
    `vbapRecomputing` boolean.
12. Timing values: `renderTimeMs`, `decodeTimeMs`, `writeTimeMs` (applied
    twice — once at line 259-268 and again at 449-457; `crossoverTimeMs` is
    **not** read here at all, a gap), then `frameDurationMs`.
13. `loudness` (number → `!== 0`), `loudnessSource`, `loudnessGain`.
14. `masterGain` (number), `autoGain` (boolean), `autoGainCeilingDb` (number).
15. `distanceModel`: accepts either a bare string or `payload.distanceModel
    .value`; whitelist `['none','linear','quadratic','inverse-square']`.
    `distanceModelMetric` likewise from `payload.distanceModelMetric` or
    `payload.distanceModel.metric`, whitelist `['spherical','chebyshev']`.
16. `distanceDiffuse`: `enabled` bool, `threshold`/`curve` numbers, `metric`
    whitelisted, `mirrorAxes.{x,y,z}` booleans applied individually.
17. All 24 `adaptiveResampling*` fields, each guarded on its wire type;
    the `u8`-valued ones become booleans via `!== 0`.
18. `configSaved` number → `!== 0`.
19. All ten latency fields, plus the **back-fill**: when `latencyRequestedMs`
    is present, `latencyTargetMs` and `latencyMs` are seeded from it **only if
    they are still `null`** (`init.js:440-448`, mirrored by the
    `latency:requested` listener at `tauri-bridge.js:787-798`).
20. `resampleRatio`, `audioSampleRate` (`> 0 ? v : null`), `rampMode`
    (whitelist `off|frame|sample|interp`).
21. `options` → `Object.assign(app.options, payload.options)` (**merge**, never
    replace).
22. `objectGeneratorParams` (object), `objectGeneratorLayoutHasHeight`
    (boolean), `phantomParams` (object), `fixedChannelCatalog` (array),
    `fixedChannelProcessing` (object), `outputChannelMappingUnroutable` (array,
    filtered to strings).
23. `crossover`: guarded with `hasOwnProperty` so an explicit `null` clears it;
    then `updateAudioFormatDisplay()`.
24. `virtualBed`: also `hasOwnProperty`-guarded. `null` → the first
    authoritative "no saved bed" triggers the **one-shot**
    `materializeDefaultVirtualBed()` guarded by `app.virtualBedMaterialized`.
25. Audio output fields, each `trim() || null`.
26. `inputMode`: **skipped while `app.inputModeDirty` is set**, unless the
    incoming value already equals `app.inputMode` (in which case the dirty flag
    is cleared). Protocol aliases are already canonicalised host-side by
    `normalize_input_mode`, so no whitelist is needed here.
27. `inputActiveMode` (string). `inputApplyPending`: when
    `app.inputApplyAwaitingAck`, only a `pending === true` is accepted (and
    clears the awaiting flag); otherwise the value is taken as-is.
28. `drcMode` (string → `dirty.drcUI`), `drcWeight` (clamped `[0,1]`, only
    marks dirty when it actually changed), `supportedDrcModes` (array →
    `String`s, `dirty.drcUI`).
29. Input applied fields, each `trim() || null`.
30. `renderBridgePath`, `renderConfigPath`, `renderConfigStatus`,
    `renderExecutable`, `renderVersion`, `renderAbi`, `renderBridgeError` —
    `trim() || null`; then `updateAboutConfigPath()`,
    `updateAboutRendererVersion()`.
31. `liveInput`: `backend`/`map`/`lfeMode` lowercased with a
    "keep previous when empty" fallback; `node`/`description`/`layout` verbatim;
    `channels`/`sampleRate` only when `> 0`; `clockMode` **skipped while
    `app.liveInputClockModeDirty`** unless it matches (same pattern as
    `inputMode`).
32. `orenderInputPipe` `trim() || null`.
33. `oscStatus` → `setOscStatus(s)` for the four known values; then
    `syncRuntimeConnectionLock()`.
34. `oscMeteringEnabled` number → boolean + syncs the toggle checkbox.
35. `logLevel` string → `logState.backendLogLevel = normalizeLogLevel(...)`.
36. A block of unconditional re-renders: `updateLatencyDisplay`,
    `updateLatencyMeterUI`, `updateRenderTimeUI`, `updateResampleRatioDisplay`,
    `updateAudioFormatDisplay`, `reflectBoundOptions`,
    `updateOutputChannelMappingUI`, `updateInputControlUI`, `renderDrcUI`.
37. **Audio-Input auto-open, rising edge only** (`init.js:694-704`): when
    `app.inputError` matches
    `/bridge path missing|no bridge plugin found|render\.bridge_path|duplicate PipeWire sink/i`
    **and** it differs from `app.lastAutoOpenedInputError`, remember it and
    `setInputSectionOpen(true)`. Otherwise reset `lastAutoOpenedInputError` to
    `null`. Without the rising-edge guard the panel would reopen on every
    snapshot while the error persists. (`setOscStatus` re-arms it per
    connection.)
38. `updateMasterMeterUI()`, `renderLogLevelControl()`,
    `hydrateLayoutSelect(payload.layouts || [], payload.selectedLayoutKey)`,
    `refreshOverlayLists()`, `updateMasterMeterUI()` (again),
    `renderSpeakerEditor()`, and finally `syncMpvOverlayPrefs()` — which
    **pushes** the persisted overlay display prefs to the renderer on every
    (re)connect so they apply at startup rather than only on a manual toggle.

**Port note.** In the native crate the snapshot *is* the model
(`apply_*_domain_state` writes `AppState` directly, and the same
whitelists/clamps live in `NAT/src/osc/apply.rs`), so steps 6-32 mostly
disappear. What must be reproduced explicitly:

* the **editing/dirty guards** (`inputMode`, `liveInput.clockMode`, the
  latency target, the adaptive numeric fields) — the native model has no such
  guard, so a text field must hold its own draft;
* the `latencyRequested → latencyTarget/latencyMs` back-fill;
* the `virtualBed === null` one-shot materialisation;
* the input-error rising-edge auto-open;
* `syncMpvOverlayPrefs()` on (re)connect;
* the mute-set rebuild semantics (a `0` value is not a mute).

---

## 7. Porting checklist (what is actually missing natively)

**A. Wire the two already-copied host modules.**

1. `NAT/src/host/peak_hold.rs` — instantiate one `PeakHolds` next to `Live`,
   call `update` on every object/speaker/ear/master meter with the keys
   `src:{id}`, `spk:{id}`, `ear:{id}`, `master`; call `forget` in
   `Live::remove_source` and in the `SpatialFrame` purge loop.
2. `NAT/src/host/timing_stats.rs` — hold five `TimeWindow`s (`latency` 4000 ms;
   `decode`/`render`/`crossover`/`write` 5000 ms), a monotonic `now_ms` origin,
   the `record_timing` non-finite→`clear()` rule, and expose
   `stats(now, 4000)` / `stats(now, 1000)` / `stats(now, 5000)` to the gauges.
   No 250 ms emit timer is needed — the gauge can query on repaint.

**B. Port the derived meter reconstruction**: `derived_master_meter`, used only
while `master_level.is_none()`, with `METER_DB_MIN = -60` and the power-sum RMS,
flagged as "derived" for display.

**C. Port the connection FSM**: `osc_status` (`initializing`/`connected`/
`reconnecting`/`error`), `reset_runtime_state()` on every non-connected
transition, the producer-epoch re-handshake, the `/omniphony/state/shutdown`
goodbye, and `HEARTBEAT_ACK_TIMEOUT = 10 s` (the native listener currently uses
16 s). Add the UI reactions: the runtime lock, the status dot colours, the
config-panel auto-open timers, `sourceNames.clear()` on disconnect.

**D. Port the watchdog** (`WatchdogControl` + `watchdog_tick` + the
`rearm`/`suppress` call sites + the exit hook) and surface
`orender:autostart` as log lines.

**E. Wire the MISSING dispatch arms** (all currently swallowed by
`dispatch.rs:646`): `StateObjectTestClip`, `StateLogLevel`, `Log`, the nine
`StateRenderEvaluation*`, `StateVbapAllowNegativeZ`,
`StateSpeakersRecomputing`, `StateSpeakersRecomputeError`, the three
`StateBackendFile*`, the sixteen `StateAdaptiveResampling*`,
`StateConfigSaved`, `StateConfigSaveError`. Note the per-field value rules in
the table of §1.2 (`>0 ? v : null` for most, `>=0 ? v : 0` for `zNegSize`,
`round()` for the three integer adaptive fields).

**F. Build the log ring** (120 entries, the level coercion, the `[target]`
prefix rule, the filter, the summary line, copy/clear) and the level select
bound to `/omniphony/control/log_level` with the same six-value whitelist.

**G. Do NOT port**: `state:batch`/`BATCHED_EVENTS`/`BATCH_FLUSH_INTERVAL`, the
`state:snapshot_ready`/`overlay:state` emit-hash dedup, the base64 gaintable
encoding, `speaker_gaintable:uptodate`, `layout:selected`, and the `dirty`/
`flushUI` machinery.

---

## 8. Open questions / things I could not determine

1. **`crossoverTimeMs` is never read by `applyInitState`.** Steps 12 apply
   `renderTimeMs`, `decodeTimeMs`, `writeTimeMs` (twice) and `frameDurationMs`,
   but the snapshot's `crossoverTimeMs` is dropped. I could not tell whether
   that is deliberate or a bug; the native UI should probably read it.
2. **`DistanceDiffuse.mirrorAxes`** is read by `applyInitState`
   (`init.js:328-335`) and defaulted in `state.js`, but the typed
   `DistanceDiffuse` struct in `app_state.rs:100-106` has no such field, so it
   cannot survive `get_state`/`state:snapshot_ready`. I could not find where the
   UI would ever receive it — it may only arrive through a path I did not read.
3. **`frame:duration_ms` guard.** The web setter rejects non-finite and `<= 0`
   values; `dispatch.rs:608-611` stores whatever arrives. I assumed the guard
   should move into the readout, but did not verify whether any renderer
   actually emits `0`.
4. **Native `HEARTBEAT_TIMEOUT = 16 s` vs host `10 s`.** The native comment
   claims "the host uses the same three-heartbeat window", which does not match
   `HEARTBEAT_ACK_TIMEOUT = 10 s`. I did not determine which value is intended.
5. **`auto_tune:event` payload semantics** are documented at the wire level
   (`WEB/src-tauri/src/auto_tune/wire.rs`) but I did not read
   `wizard-ui.js`, so the per-step field expectations of the wizard are only
   partially captured here (the `wire.rs` doc comment names
   `payload.palierStats?.peakToPeakPpm` and `payload.verdict?.reason` as fields
   the UI reads directly). Scheduled for a later phase anyway.
6. **`sofa:download_progress`** — I documented the emit sites but not the modal
   that consumes it (SOFA browser is a later phase and has no listener in
   `tauri-bridge.js`; it is presumably registered inside the modal module).
7. I did not audit `controls/adaptive.js`, `controls/vbap.js`,
   `controls/audio.js` etc. beyond the lines that set `dirty` flags, so the
   *rendering* rules of those panels (ranges, formats, enable conditions) are
   out of this document's scope — they belong to the per-panel specs.
