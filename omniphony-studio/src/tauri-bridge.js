/**
 * Tauri event bridge.
 *
 * Registers all `listen(...)` handlers that receive incremental state updates
 * from the Rust backend and apply them to the frontend state + UI.
 */

import * as THREE from 'three';
import { listen } from '@tauri-apps/api/event';

import {
  app,
  sourceMeshes,
  sourceTrails,
  speakerMuted,
  objectMuted,
  speakerManualMuted,
  objectManualMuted,
  speakerGainCache,
  speakerDelays,
  layoutsByKey,
  usesNumericSpatialPlaceholders
} from './state.js';

import { updateSource, updateSourceLevel, updateSourceGains, updateSourceBandGains, updateSourceSize, updateSourceTag, removeSource } from './sources.js';
import { syncVirtualBedObjects } from './controls/virtual-bed.js';
import {
  updateSpeakerLevel,
  updateMasterLevel,
  renderLayout,
  renderSpeakerEditor,
  hydrateLayoutSelect,
  updateSpeakerVisualsFromState,
  setSpeakerSpatializeLocal,
  updateSpeakerControlsUI,
  updateObjectControlsUI,
  flashSpeakerClip
} from './speakers.js';

import {
  setLatencyInstantMs,
  updateLatencyDisplay,
  updateLatencyMeterUI,
  updateRenderTimeUI,
  setRenderTimeMs,
  setDecodeTimeMs,
  setCrossoverTimeMs,
  setWriteTimeMs,
  setFrameDurationMs,
  updateResampleRatioDisplay
} from './controls/latency.js';
import { updateMasterGainUI, updateLoudnessDisplay, updateDistanceModelUI, flashClipIndicator } from './controls/master.js';
import {
  updateRenderBackend,
  updateEvaluationMode,
  updateVbapCartesian,
  updateVbapPolar,
  updateVbapPositionInterpolation,
  renderVbapStatus
} from './controls/vbap.js';
import { invoke } from '@tauri-apps/api/core';
import { setSpeakerGainTable } from './scene/speaker-gaintable.js';
import { updateAudioFormatDisplay, rebuildObjectGeneratorControls, rebuildPhantomControls } from './controls/audio.js';
import { reflectBoundOptions } from './options-binder.js';
import { updateInputControlUI } from './controls/input.js';
import { updateDrcMeterUI } from './controls/drc.js';
import { updateAdaptiveResamplingUI } from './controls/adaptive.js';
import { updateDistanceDiffuseUI } from './controls/distance-diffuse.js';
import { renderOscStatus, setOscStatus } from './controls/osc.js';
import { updateConfigSavedUI, updateAboutConfigPath, updateAboutRendererVersion } from './controls/config.js';
import {
  updateRoomRatioDisplay,
  applyRoomRatio,
  refreshRoomGeometryInputState
} from './controls/room-geometry.js';
import { normalizeLogLevel, renderLogLevelControl, logState, pushLog } from './log.js';
import { t } from './i18n.js';
import { applyInitState } from './init.js';
import { setHeadPoseQuat } from './scene/head-pose.js';
import { setInputSectionOpen } from './modals.js';
import { syncSpeakerHeatmapBandSelect } from './scene/speaker-band-select.js';

// Apply one coalesced high-frequency event. The Rust side batches positions and
// meters (one `app.emit` per object/speaker per frame grew WebView2 memory
// without bound on Windows) into a single `state:batch`; we replay each entry
// through the same handlers the individual `listen(...)` calls below use.
function applyBatchedEvent(event, payload) {
  switch (event) {
    case 'source:update':
      updateSource(payload.id, payload.position);
      break;
    case 'source:meter':
      updateSourceLevel(payload.id, payload.meter);
      break;
    case 'source:gains':
      updateSourceGains(payload.id, payload.gains);
      break;
    case 'source:band_gains':
      updateSourceBandGains(payload.id, payload.band, payload.gains);
      break;
    case 'speaker:meter':
      updateSpeakerLevel(Number(payload.id), payload.meter);
      break;
    case 'master:meter':
      updateMasterLevel(payload.meter);
      break;
    case 'meter:drc_gain':
      updateDrcMeterUI(Number(payload.value));
      break;
    case 'binaural:head_pose':
      setHeadPoseQuat(payload);
      break;
    default:
      break;
  }
}

export function setupTauriBridge() {
  listen('state:batch', ({ payload }) => {
    const events = payload && Array.isArray(payload.events) ? payload.events : null;
    if (!events) return;
    for (const entry of events) {
      if (entry && typeof entry.event === 'string') {
        applyBatchedEvent(entry.event, entry.payload);
      }
    }
  });

  listen('state:snapshot_ready', ({ payload }) => {
    if (payload && typeof payload === 'object') {
      applyInitState(payload);
      // Heatmap is push-based now: the renderer pushes new tiles automatically
      // to the active subscription. No need to re-request on every state echo
      // — that was the engine of the heatmap storm. Re-subscribe only on
      // explicit user action (speaker selection, heatmap toggle, etc.).
    }
  });

  // -----------------------------------------------------------------------
  // Layouts
  // -----------------------------------------------------------------------

  listen('layouts:update', ({ payload }) => {
    hydrateLayoutSelect(payload.layouts || [], payload.selectedLayoutKey);
  });

  listen('layout:selected', ({ payload }) => {
    if (payload.key && layoutsByKey.has(payload.key)) {
      const layoutSelectEl = document.getElementById('layoutSelect');
      if (layoutSelectEl) layoutSelectEl.value = payload.key;
      renderLayout(payload.key);
    }
  });

  // -----------------------------------------------------------------------
  // Sources
  // -----------------------------------------------------------------------

  listen('source:update', ({ payload }) => {
    updateSource(payload.id, payload.position);
    // A live object arrived → the spatial stream is active. Mark it (a
    // source:update may precede the spatial:frame header in the OSC burst) so the
    // reconcile keeps the live objects and drops the synthetic at-rest markers,
    // rather than treating the just-added object as stale.
    app.lastSpatialFrameAt = performance.now();
    syncVirtualBedObjects();
  });

  listen('source:size', ({ payload }) => {
    updateSourceSize(payload.id, payload.size);
  });

  listen('source:remove', ({ payload }) => {
    removeSource(payload.id);
    // The stream may have gone idle → restore the synthetic at-rest bed markers.
    syncVirtualBedObjects();
  });

  listen('source:meter', ({ payload }) => {
    updateSourceLevel(payload.id, payload.meter);
  });

  // Deduped: the 5 s subscribe heartbeat re-triggers `unavailable` every tick
  // while a volume is on in a non-cartesian mode — log only on transitions.
  let lastGaintableUnavailable = null;

  listen('speaker_gaintable', ({ payload }) => {
    pushLog('info', `gaintable: loaded spk=${payload?.speakerIndex} `
      + `${payload?.xCount}x${payload?.yCount}x${payload?.zCount} bands=${payload?.bandCount}`);
    setSpeakerGainTable(payload);
    lastGaintableUnavailable = null;
  });

  listen('speaker_gaintable:unavailable', ({ payload }) => {
    const reason = JSON.stringify(payload);
    if (reason !== lastGaintableUnavailable) {
      lastGaintableUnavailable = reason;
      pushLog('warn', `gaintable: unavailable ${reason}`);
    }
  });
  // `speaker_gaintable:uptodate` (subscribe ack when our version already matches)
  // is intentionally not handled: it fires on every 5 s heartbeat and carries no
  // action for the client.

  listen('source:gains', ({ payload }) => {
    updateSourceGains(payload.id, payload.gains);
  });

  listen('source:band_gains', ({ payload }) => {
    updateSourceBandGains(payload.id, payload.band, payload.gains);
  });

  listen('meter:drc_gain', ({ payload }) => {
    updateDrcMeterUI(Number(payload.value));
  });

  listen('spatial:frame', ({ payload }) => {
    const isReset = Boolean(payload?.reset);
    const objectCount = Math.max(0, Number(payload?.objectCount ?? 0) | 0);
    // Mark the stream as active so the at-rest synthetic bed objects aren't
    // spawned during playback — including the brief gap right after a seek, where
    // live objects momentarily disappear but frames are still flowing.
    app.lastSpatialFrameAt = performance.now();

    if (isReset) {
      for (const trail of sourceTrails.values()) {
        trail.positions.length = 0;
        trail.line.geometry.dispose();
        trail.line.geometry = new THREE.BufferGeometry();
      }
    }

    if (usesNumericSpatialPlaceholders()) {
      // Ensure IDs [0..objectCount-1] exist for renderer snapshots that use numeric IDs.
      for (let i = 0; i < objectCount; i += 1) {
        const id = String(i);
        if (!sourceMeshes.has(id)) {
          updateSource(id, { x: 0, y: 0, z: 0, name: `Object_${i}`, _noTrail: true });
        }
      }

      // Safety purge in case stale objects remain locally.
      for (const id of Array.from(sourceMeshes.keys())) {
        const idx = Number(id);
        if (Number.isInteger(idx) && idx >= objectCount) {
          removeSource(id);
        }
      }
    }

    // The stream is now active again → drop any synthetic at-rest bed markers so
    // they don't coexist with the live objects.
    syncVirtualBedObjects();
  });

  // -----------------------------------------------------------------------
  // Speakers
  // -----------------------------------------------------------------------

  listen('speaker:meter', ({ payload }) => {
    updateSpeakerLevel(Number(payload.id), payload.meter);
  });

  listen('master:meter', ({ payload }) => {
    updateMasterLevel(payload.meter);
  });

  listen('speaker:gain', ({ payload }) => {
    speakerGainCache.set(String(payload.id), Number(payload.gain));
    updateSpeakerControlsUI();
  });

  listen('speaker:delay', ({ payload }) => {
    const id = String(payload.id);
    const delayMs = Math.max(0, Number(payload.delayMs) || 0);
    speakerDelays.set(id, delayMs);
    renderSpeakerEditor();
    updateSpeakerControlsUI();
  });

  listen('speaker:mute', ({ payload }) => {
    const key = String(payload.id);
    if (Number(payload.muted)) {
      speakerMuted.add(key);
    } else {
      speakerMuted.delete(key);
      speakerManualMuted.delete(key);
    }
    updateSpeakerControlsUI();
  });

  listen('speaker:spatialize', ({ payload }) => {
    const index = Number(payload.id);
    if (!Number.isInteger(index) || index < 0) {
      return;
    }
    const next = Number(payload.spatialize) === 0 ? 0 : 1;
    setSpeakerSpatializeLocal(index, next);
    updateSpeakerControlsUI();
  });

  listen('speaker:name', ({ payload }) => {
    const index = Number(payload.id);
    if (!Number.isInteger(index) || index < 0) {
      return;
    }
    const speaker = app.currentLayoutSpeakers[index];
    if (!speaker) {
      return;
    }
    speaker.id = String(payload.name ?? speaker.id ?? index);
    updateSpeakerVisualsFromState(index);
    updateSpeakerControlsUI();
  });

  listen('speaker:freq_low', ({ payload }) => {
    const index = Number(payload.id);
    if (!Number.isInteger(index) || index < 0) return;
    const speaker = app.currentLayoutSpeakers[index];
    if (!speaker) return;
    const fl = payload.freq_low;
    speaker.freqLow = fl != null && fl > 0 ? fl : null;
    syncSpeakerHeatmapBandSelect();
    if (app.selectedSpeakerIndex === index) renderSpeakerEditor();
  });

  listen('speaker:freq_high', ({ payload }) => {
    const index = Number(payload.id);
    if (!Number.isInteger(index) || index < 0) return;
    const speaker = app.currentLayoutSpeakers[index];
    if (!speaker) return;
    const fh = payload.freq_high;
    speaker.freqHigh = fh != null && fh > 0 ? fh : null;
    syncSpeakerHeatmapBandSelect();
    if (app.selectedSpeakerIndex === index) renderSpeakerEditor();
  });

  // -----------------------------------------------------------------------
  // Objects
  // -----------------------------------------------------------------------

  listen('object:mute', ({ payload }) => {
    const key = String(payload.id);
    if (Number(payload.muted)) {
      objectMuted.add(key);
    } else {
      objectMuted.delete(key);
      objectManualMuted.delete(key);
    }
    updateObjectControlsUI();
  });

  listen('object:source_tag', ({ payload }) => {
    updateSourceTag(payload.id, payload.sourceTag);
  });

  // -----------------------------------------------------------------------
  // OSC
  // -----------------------------------------------------------------------

  listen('osc:status', ({ payload }) => {
    const next = payload?.status;
    if (next === 'initializing' || next === 'connected' || next === 'reconnecting' || next === 'error') {
      setOscStatus(next);
    }
  });

  listen('orender:autostart', ({ payload }) => {
    const status = payload?.status;
    if (status === 'launched') {
      pushLog('info', t('log.orenderAutostartLaunched'));
    } else if (status === 'failed') {
      pushLog('error', t('log.orenderAutostartFailed'));
      setOscStatus('error');
    }
  });

  listen('osc:metering', ({ payload }) => {
    app.oscMeteringEnabled = Number(payload?.enabled) !== 0;
    const oscMeteringToggleEl = document.getElementById('oscMeteringToggle');
    if (oscMeteringToggleEl) oscMeteringToggleEl.checked = app.oscMeteringEnabled;
    if (!app.oscMeteringEnabled) {
      app.decodeTimeMs = null;
      app.decodeTimeWindow = [];
      app.renderTimeMs = null;
      app.renderTimeWindow = [];
      app.writeTimeMs = null;
      app.writeTimeWindow = [];
    }
    updateRenderTimeUI();
  });

  // -----------------------------------------------------------------------
  // Audio input
  // -----------------------------------------------------------------------

  listen('render:bridge_path', ({ payload }) => {
    app.renderBridgePath = String(payload?.value ?? '').trim() || null;
    updateInputControlUI();
  });

  listen('render:config_path', ({ payload }) => {
    app.renderConfigPath = String(payload?.value ?? '').trim() || null;
    updateAboutConfigPath();
  });

  listen('render:config_status', ({ payload }) => {
    app.renderConfigStatus = String(payload?.value ?? '').trim() || null;
    updateAboutConfigPath();
  });

  listen('render:version', ({ payload }) => {
    app.renderVersion = String(payload?.value ?? '').trim() || null;
    updateAboutRendererVersion();
  });

  listen('render:abi', ({ payload }) => {
    app.renderAbi = String(payload?.value ?? '').trim() || null;
    updateAboutRendererVersion();
  });

  listen('render:bridge_error', ({ payload }) => {
    app.renderBridgeError = String(payload?.value ?? '').trim() || null;
    renderOscStatus();
  });

  // -----------------------------------------------------------------------
  // Room ratio
  // -----------------------------------------------------------------------

  // -----------------------------------------------------------------------
  // VBAP
  // -----------------------------------------------------------------------

  listen('vbap:recomputing', ({ payload }) => {
    app.vbapRecomputing = payload.enabled === true;
    if (app.vbapRecomputing) {
      app.recomputeError = null;
    }
    renderVbapStatus();
  });

  listen('speakers:recompute_error', ({ payload }) => {
    const message = typeof payload?.message === 'string' ? payload.message.trim() : '';
    app.recomputeError = message.length > 0 ? message : null;
    if (app.recomputeError) {
      app.vbapRecomputing = false;
    }
    renderVbapStatus();
  });

  listen('render_evaluation:cartesian:x_size', ({ payload }) => {
    const value = Number(payload.value);
    app.vbapCartesianState.xSize = value > 0 ? value : null;
    updateVbapCartesian();
  });

  listen('render_evaluation:cartesian:y_size', ({ payload }) => {
    const value = Number(payload.value);
    app.vbapCartesianState.ySize = value > 0 ? value : null;
    updateVbapCartesian();
  });

  listen('render_evaluation:cartesian:z_size', ({ payload }) => {
    const value = Number(payload.value);
    app.vbapCartesianState.zSize = value > 0 ? value : null;
    updateVbapCartesian();
  });

  listen('render_evaluation:cartesian:z_neg_size', ({ payload }) => {
    const value = Number(payload.value);
    app.vbapCartesianState.zNegSize = value >= 0 ? value : 0;
    updateVbapCartesian();
  });

  listen('render_evaluation:polar:azimuth_resolution', ({ payload }) => {
    const value = Number(payload.value);
    app.vbapPolarState.azimuthResolution = value > 0 ? value : null;
    updateVbapPolar();
  });

  listen('render_evaluation:polar:elevation_resolution', ({ payload }) => {
    const value = Number(payload.value);
    app.vbapPolarState.elevationResolution = value > 0 ? value : null;
    updateVbapPolar();
  });

  listen('render_evaluation:polar:distance_res', ({ payload }) => {
    const value = Number(payload.value);
    app.vbapPolarState.distanceRes = value > 0 ? value : null;
    updateVbapPolar();
  });

  listen('render_evaluation:polar:distance_max', ({ payload }) => {
    const value = Number(payload.value);
    app.vbapPolarState.distanceMax = value > 0 ? value : null;
    updateVbapPolar();
  });

  listen('render_evaluation:position_interpolation', ({ payload }) => {
    app.vbapPositionInterpolation = payload.enabled === true;
    updateVbapPositionInterpolation();
  });

  listen('vbap:allow_negative_z', ({ payload }) => {
    app.vbapAllowNegativeZ = payload.enabled === true;
    updateVbapPolar();
  });

  // -----------------------------------------------------------------------
  // Render / decode / write timing
  // -----------------------------------------------------------------------

  listen('decode:time_ms', ({ payload }) => {
    const value = Number(payload?.value);
    if (Number.isFinite(value)) {
      setDecodeTimeMs(value);
    } else {
      app.decodeTimeMs = null;
      app.decodeTimeWindow = [];
    }
    updateRenderTimeUI();
  });

  listen('render:time_ms', ({ payload }) => {
    const value = Number(payload?.value);
    if (Number.isFinite(value)) {
      setRenderTimeMs(value);
    } else {
      app.renderTimeMs = null;
      app.renderTimeWindow = [];
    }
    updateRenderTimeUI();
  });

  listen('crossover:time_ms', ({ payload }) => {
    const value = Number(payload?.value);
    if (Number.isFinite(value)) {
      setCrossoverTimeMs(value);
    } else {
      app.crossoverTimeMs = null;
      app.crossoverTimeWindow = [];
    }
    updateRenderTimeUI();
  });

  listen('write:time_ms', ({ payload }) => {
    const value = Number(payload?.value);
    if (Number.isFinite(value)) {
      setWriteTimeMs(value);
    } else {
      app.writeTimeMs = null;
      app.writeTimeWindow = [];
    }
    updateRenderTimeUI();
  });

  listen('frame:duration_ms', ({ payload }) => {
    const value = Number(payload?.value);
    if (Number.isFinite(value)) {
      setFrameDurationMs(value);
    } else {
      app.frameDurationMs = null;
    }
    updateRenderTimeUI();
  });

  // -----------------------------------------------------------------------
  // Loudness
  // -----------------------------------------------------------------------

  // -----------------------------------------------------------------------
  // Master gain
  // -----------------------------------------------------------------------

  listen('master:gain', ({ payload }) => {
    app.masterGain = Number(payload.value);
    updateMasterGainUI();
  });

  // Clip indicator: renderer flags any output clip (independent of auto-gain),
  // carrying the offending speaker index so its row also flashes red.
  listen('clip:detected', ({ payload }) => {
    flashClipIndicator();
    flashSpeakerClip(Number(payload?.speaker));
  });

  // -----------------------------------------------------------------------
  // Distance model & diffuse
  // -----------------------------------------------------------------------

  // -----------------------------------------------------------------------
  // Adaptive resampling
  // -----------------------------------------------------------------------

  listen('adaptive_resampling:band', ({ payload }) => {
    app.adaptiveResamplingBand = typeof payload.value === 'string' ? payload.value : null;
    updateAdaptiveResamplingUI();
  });

  listen('adaptive_resampling:state', ({ payload }) => {
    app.adaptiveResamplingState = typeof payload.value === 'string' ? payload.value : null;
    updateAdaptiveResamplingUI();
  });

  listen('adaptive_resampling:pause', ({ payload }) => {
    app.adaptiveResamplingPaused = payload.enabled !== 0;
    updateAdaptiveResamplingUI();
  });

  // -----------------------------------------------------------------------
  // Config saved
  // -----------------------------------------------------------------------

  listen('config:saved', ({ payload }) => {
    app.configSaved = payload.saved !== 0;
    app.saveError = null;
    app.saveRequested = false;
    updateConfigSavedUI();
  });

  listen('config:save_error', ({ payload }) => {
    const message = typeof payload?.message === 'string' ? payload.message.trim() : '';
    app.saveError = message.length > 0 ? message : null;
    app.saveRequested = false;
    if (app.saveError) {
      pushLog('error', app.saveError);
    }
    updateConfigSavedUI();
  });

  // -----------------------------------------------------------------------
  // Latency
  // -----------------------------------------------------------------------

  listen('latency', ({ payload }) => {
    app.latencyMs = Number(payload.value);
    updateLatencyDisplay();
    updateLatencyMeterUI();
  });

  listen('latency:instant', ({ payload }) => {
    setLatencyInstantMs(payload.value);
    updateLatencyDisplay();
    updateLatencyMeterUI();
  });

  listen('latency:control', ({ payload }) => {
    app.latencyControlMs = Number(payload.value);
    updateLatencyDisplay();
  });

  listen('latency:smoothed', ({ payload }) => {
    app.latencySmoothedMs = Number(payload.value);
    updateLatencyDisplay();
    updateLatencyMeterUI();
  });

  listen('latency:downstream', ({ payload }) => {
    app.latencyDownstreamMs = Number(payload.value);
    updateLatencyDisplay();
  });

  // Generic diag registry: schema (list of available metrics) + values map.
  // Lets the diag plot dynamically offer any metric the renderer registers,
  // with zero studio-side change per new metric.
  // The renderer side ships the schema/values as a JSON string inside the
  // OSC payload — parse it here so the plot always sees a real JS object.
  // Idempotent: if Tauri ever starts forwarding it as a structured value
  // directly, the typeof check skips the redundant parse.
  const parseDiagPayload = (payload) => {
    const raw = payload && payload.value !== undefined ? payload.value : null;
    if (typeof raw === 'string') {
      try { return JSON.parse(raw); } catch (_) { return null; }
    }
    return raw;
  };

  listen('diag:schema', ({ payload }) => {
    app.diagSchema = parseDiagPayload(payload);
  });

  listen('diag:values', ({ payload }) => {
    app.diagValues = parseDiagPayload(payload);
  });

  // Declared fixed-bed→height object-generator schema → build the selector +
  // parameter sliders dynamically.
  listen('objectGenerators:schema', ({ payload }) => {
    try {
      app.objectGenerators = JSON.parse(payload?.value ?? '[]') || [];
    } catch (_) {
      app.objectGenerators = [];
    }
    rebuildObjectGeneratorControls();
  });

  // Declared phantom-extraction param schema → build its sliders dynamically.
  listen('phantom:schema', ({ payload }) => {
    try {
      app.phantomSchema = JSON.parse(payload?.value ?? '[]') || [];
    } catch (_) {
      app.phantomSchema = [];
    }
    rebuildPhantomControls();
  });

  // Declared live-options schema (registry rows). Provides the binder's
  // pre-snapshot defaults, so reflect once it lands.
  listen('options:schema', ({ payload }) => {
    try {
      app.optionsSchema = JSON.parse(payload?.value ?? '[]') || [];
    } catch (_) {
      app.optionsSchema = [];
    }
    reflectBoundOptions();
  });

  listen('latency:target', ({ payload }) => {
    const value = Number(payload.value);
    app.latencyTargetMs = Number.isFinite(value) ? value : null;
    updateLatencyDisplay();
    updateLatencyMeterUI();
  });

  listen('latency:requested', ({ payload }) => {
    const value = Number(payload.value);
    app.latencyRequestedMs = Number.isFinite(value) ? value : null;
    if (app.latencyTargetMs === null && Number.isFinite(value)) {
      app.latencyTargetMs = value;
    }
    if (app.latencyMs === null && Number.isFinite(value)) {
      app.latencyMs = value;
    }
    updateLatencyDisplay();
    updateLatencyMeterUI();
  });

  // -----------------------------------------------------------------------
  // Resample ratio
  // -----------------------------------------------------------------------

  listen('resample_ratio', ({ payload }) => {
    app.resampleRatio = Number(payload.value);
    updateResampleRatioDisplay();
  });

  // -----------------------------------------------------------------------
  // Audio
  // -----------------------------------------------------------------------

  // -----------------------------------------------------------------------
  // Input pipe
  // -----------------------------------------------------------------------

  listen('state:input_pipe', ({ payload }) => {
    app.orenderInputPipe = typeof payload.value === 'string' ? (payload.value.trim() || null) : null;
    renderOscStatus();
    updateInputControlUI();
  });

  // -----------------------------------------------------------------------
  // Log level
  // -----------------------------------------------------------------------

  listen('state:log_level', ({ payload }) => {
    logState.backendLogLevel = normalizeLogLevel(payload?.value);
    renderLogLevelControl();
  });

  listen('omniphony:log', ({ payload }) => {
    const level = normalizeLogLevel(payload?.level);
    const target = String(payload?.target || '').trim();
    const message = String(payload?.message || '').trim();
    if (!message) return;
    pushLog(level, message, target);
  });
}
