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
  objectGainCache,
  speakerGainCache,
  speakerDelays,
  layoutsByKey
} from './state.js';

import { updateSource, updateSourceLevel, updateSourceGains, removeSource } from './sources.js';
import {
  updateSpeakerLevel,
  renderLayout,
  renderSpeakerEditor,
  hydrateLayoutSelect,
  updateSpeakerVisualsFromState,
  setSpeakerSpatializeLocal,
  updateSpeakerControlsUI,
  updateObjectControlsUI
} from './speakers.js';

import {
  setLatencyInstantMs,
  updateLatencyDisplay,
  updateLatencyMeterUI,
  updateRenderTimeUI,
  setRenderTimeMs,
  setDecodeTimeMs,
  setWriteTimeMs,
  setFrameDurationMs,
  updateResampleRatioDisplay
} from './controls/latency.js';
import { updateMasterGainUI, updateLoudnessDisplay, updateDistanceModelUI } from './controls/master.js';
import { updateSpreadDisplay } from './controls/spread.js';
import {
  updateRenderBackend,
  updateEvaluationMode,
  updateVbapCartesian,
  updateVbapPolar,
  updateVbapPositionInterpolation,
  renderVbapStatus
} from './controls/vbap.js';
import { updateAudioFormatDisplay } from './controls/audio.js';
import { updateInputControlUI } from './controls/input.js';
import { updateAdaptiveResamplingUI } from './controls/adaptive.js';
import { updateDistanceDiffuseUI } from './controls/distance-diffuse.js';
import { renderOscStatus, setOscStatus } from './controls/osc.js';
import { updateConfigSavedUI } from './controls/config.js';
import { updateRoomRatioDisplay, applyRoomRatio } from './controls/room-geometry.js';
import { normalizeLogLevel, renderLogLevelControl, logState, pushLog } from './log.js';
import { applyInitState } from './init.js';

export function setupTauriBridge() {
  listen('state:snapshot_ready', ({ payload }) => {
    if (payload && typeof payload === 'object') {
      applyInitState(payload);
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

  listen('layout:radius_m', ({ payload }) => {
    const value = Math.max(0.01, Number(payload?.value) || 1.0);
    app.metersPerUnit = value;
    const layout = app.currentLayoutKey ? layoutsByKey.get(app.currentLayoutKey) : null;
    if (layout) {
      layout.radius_m = value;
    }
    updateRoomRatioDisplay();
    renderSpeakerEditor();
  });

  // -----------------------------------------------------------------------
  // Sources
  // -----------------------------------------------------------------------

  listen('source:update', ({ payload }) => {
    updateSource(payload.id, payload.position);
  });

  listen('source:remove', ({ payload }) => {
    removeSource(payload.id);
  });

  listen('source:meter', ({ payload }) => {
    updateSourceLevel(payload.id, payload.meter);
  });

  listen('source:gains', ({ payload }) => {
    updateSourceGains(payload.id, payload.gains);
  });

  listen('spatial:frame', ({ payload }) => {
    const isReset = Boolean(payload?.reset);
    const objectCount = Math.max(0, Number(payload?.objectCount ?? 0) | 0);

    if (isReset) {
      for (const trail of sourceTrails.values()) {
        trail.positions.length = 0;
        trail.line.geometry.dispose();
        trail.line.geometry = new THREE.BufferGeometry();
      }
    }

    // Ensure IDs [0..objectCount-1] exist, even if omniphony sends only deltas.
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
  });

  // -----------------------------------------------------------------------
  // Speakers
  // -----------------------------------------------------------------------

  listen('speaker:meter', ({ payload }) => {
    updateSpeakerLevel(Number(payload.id), payload.meter);
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

  // -----------------------------------------------------------------------
  // Objects
  // -----------------------------------------------------------------------

  listen('object:gain', ({ payload }) => {
    objectGainCache.set(String(payload.id), Number(payload.gain));
    updateObjectControlsUI();
  });

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

  // -----------------------------------------------------------------------
  // OSC
  // -----------------------------------------------------------------------

  listen('osc:status', ({ payload }) => {
    const next = payload?.status;
    if (next === 'initializing' || next === 'connected' || next === 'reconnecting' || next === 'error') {
      setOscStatus(next);
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

  listen('input:mode', ({ payload }) => {
    const value = String(payload?.value ?? '').trim().toLowerCase();
    if (value === 'bridge' || value === 'pipe_bridge' || value === 'live' || value === 'pipewire' || value === 'pipewire_bridge') {
      app.inputMode = value === 'bridge' ? 'pipe_bridge' : (value === 'live' ? 'pipewire' : value);
      updateInputControlUI();
    }
  });

  listen('input:active_mode', ({ payload }) => {
    const value = String(payload?.value ?? '').trim().toLowerCase();
    if (value === 'bridge' || value === 'pipe_bridge' || value === 'live' || value === 'pipewire' || value === 'pipewire_bridge') {
      app.inputActiveMode = value === 'bridge' ? 'pipe_bridge' : (value === 'live' ? 'pipewire' : value);
      updateInputControlUI();
    }
  });

  listen('input:apply_pending', ({ payload }) => {
    app.inputApplyPending = Number(payload?.enabled) !== 0;
    updateInputControlUI();
  });

  listen('input:backend', ({ payload }) => {
    app.inputBackend = String(payload?.value ?? '').trim() || null;
    updateInputControlUI();
  });

  listen('input:channels', ({ payload }) => {
    const value = Number(payload?.value);
    app.inputChannels = Number.isFinite(value) && value > 0 ? value : null;
    updateInputControlUI();
  });

  listen('input:sample_rate', ({ payload }) => {
    const value = Number(payload?.value);
    app.inputSampleRate = Number.isFinite(value) && value > 0 ? value : null;
    updateInputControlUI();
  });

  listen('input:stream_format', ({ payload }) => {
    app.inputStreamFormat = String(payload?.value ?? '').trim() || null;
    updateInputControlUI();
  });

  listen('input:error', ({ payload }) => {
    app.inputError = String(payload?.value ?? '').trim() || null;
    updateInputControlUI();
  });

  listen('input:live:backend', ({ payload }) => {
    app.liveInput.backend = String(payload?.value ?? '').trim().toLowerCase() || app.liveInput.backend;
    updateInputControlUI();
  });

  listen('input:live:node', ({ payload }) => {
    app.liveInput.node = String(payload?.value ?? '');
    updateInputControlUI();
  });

  listen('input:live:description', ({ payload }) => {
    app.liveInput.description = String(payload?.value ?? '');
    updateInputControlUI();
  });

  listen('input:live:layout', ({ payload }) => {
    app.liveInput.layout = String(payload?.value ?? '');
    updateInputControlUI();
  });

  listen('input:live:clock_mode', ({ payload }) => {
    app.liveInput.clockMode = String(payload?.value ?? '').trim().toLowerCase() || app.liveInput.clockMode;
    updateInputControlUI();
  });

  listen('input:live:channels', ({ payload }) => {
    const value = Number(payload?.value);
    if (Number.isFinite(value) && value > 0) {
      app.liveInput.channels = value;
    }
    updateInputControlUI();
  });

  listen('input:live:sample_rate', ({ payload }) => {
    const value = Number(payload?.value);
    if (Number.isFinite(value) && value > 0) {
      app.liveInput.sampleRate = value;
    }
    updateInputControlUI();
  });

  listen('input:live:format', ({ payload }) => {
    app.liveInput.format = String(payload?.value ?? '').trim().toLowerCase() || app.liveInput.format;
    updateInputControlUI();
  });

  listen('input:live:map', ({ payload }) => {
    app.liveInput.map = String(payload?.value ?? '').trim().toLowerCase() || app.liveInput.map;
    updateInputControlUI();
  });

  listen('input:live:lfe_mode', ({ payload }) => {
    app.liveInput.lfeMode = String(payload?.value ?? '').trim().toLowerCase() || app.liveInput.lfeMode;
    updateInputControlUI();
  });

  // -----------------------------------------------------------------------
  // Room ratio
  // -----------------------------------------------------------------------

  listen('room_ratio', ({ payload }) => {
    if (payload.roomRatio) {
      applyRoomRatio(payload.roomRatio);
    }
  });

  // -----------------------------------------------------------------------
  // Spread
  // -----------------------------------------------------------------------

  listen('spread:min', ({ payload }) => {
    app.spreadState.min = Number(payload.value);
    updateSpreadDisplay();
  });

  listen('spread:max', ({ payload }) => {
    app.spreadState.max = Number(payload.value);
    updateSpreadDisplay();
  });

  listen('spread:from_distance', ({ payload }) => {
    app.spreadState.fromDistance = payload.enabled === true;
    updateSpreadDisplay();
  });

  listen('spread:distance_range', ({ payload }) => {
    app.spreadState.distanceRange = Number(payload.value);
    updateSpreadDisplay();
  });

  listen('spread:distance_curve', ({ payload }) => {
    app.spreadState.distanceCurve = Number(payload.value);
    updateSpreadDisplay();
  });

  // -----------------------------------------------------------------------
  // VBAP
  // -----------------------------------------------------------------------

  listen('render_backend', ({ payload }) => {
    const value = String(payload?.value ?? '').trim().toLowerCase();
    app.renderBackendState.selection = ['vbap', 'experimental_distance'].includes(value) ? value : null;
    if (!['auto', 'realtime', 'precomputed_polar', 'precomputed_cartesian'].includes(app.evaluationModeState.selection)) {
      app.evaluationModeState.selection = 'auto';
    }
    updateRenderBackend();
  });

  listen('render_backend:effective', ({ payload }) => {
    const value = String(payload?.value ?? '').trim().toLowerCase();
    app.renderBackendState.effective = ['vbap', 'experimental_distance'].includes(value) ? value : null;
    if (!['realtime', 'precomputed_polar', 'precomputed_cartesian'].includes(app.evaluationModeState.effective)) {
      app.evaluationModeState.effective = null;
    }
    app.vbapRecomputing = false;
    renderVbapStatus();
    updateRenderBackend();
  });

  listen('render_evaluation_mode', ({ payload }) => {
    const value = String(payload?.value ?? '').trim().toLowerCase();
    app.evaluationModeState.selection =
      ['auto', 'realtime', 'precomputed_polar', 'precomputed_cartesian'].includes(value) ? value : null;
    updateEvaluationMode();
  });

  listen('render_evaluation_mode:effective', ({ payload }) => {
    const value = String(payload?.value ?? '').trim().toLowerCase();
    app.evaluationModeState.effective =
      ['realtime', 'precomputed_polar', 'precomputed_cartesian'].includes(value) ? value : null;
    app.vbapRecomputing = false;
    renderVbapStatus();
    updateEvaluationMode();
  });

  listen('vbap:recomputing', ({ payload }) => {
    app.vbapRecomputing = payload.enabled === true;
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

  listen('loudness', ({ payload }) => {
    app.loudnessEnabled = Number(payload.enabled) !== 0;
    updateLoudnessDisplay();
  });

  listen('loudness:source', ({ payload }) => {
    app.loudnessSource = Number(payload.value);
    updateLoudnessDisplay();
  });

  listen('loudness:gain', ({ payload }) => {
    app.loudnessGain = Number(payload.value);
    updateLoudnessDisplay();
  });

  // -----------------------------------------------------------------------
  // Master gain
  // -----------------------------------------------------------------------

  listen('master:gain', ({ payload }) => {
    app.masterGain = Number(payload.value);
    updateMasterGainUI();
  });

  // -----------------------------------------------------------------------
  // Distance model & diffuse
  // -----------------------------------------------------------------------

  listen('distance_model', ({ payload }) => {
    const value = String(payload?.value ?? '').trim().toLowerCase();
    if (!['none', 'linear', 'quadratic', 'inverse-square'].includes(value)) {
      return;
    }
    app.distanceModel = value;
    updateDistanceModelUI();
  });

  listen('distance_diffuse:enabled', ({ payload }) => {
    app.distanceDiffuseState.enabled = payload.enabled === true;
    updateDistanceDiffuseUI();
  });

  listen('distance_diffuse:threshold', ({ payload }) => {
    app.distanceDiffuseState.threshold = Number(payload.value);
    updateDistanceDiffuseUI();
  });

  listen('distance_diffuse:curve', ({ payload }) => {
    app.distanceDiffuseState.curve = Number(payload.value);
    updateDistanceDiffuseUI();
  });

  // -----------------------------------------------------------------------
  // Adaptive resampling
  // -----------------------------------------------------------------------

  listen('adaptive_resampling', ({ payload }) => {
    app.adaptiveResamplingEnabled = Number(payload.enabled) !== 0;
    updateAdaptiveResamplingUI();
  });

  listen('adaptive_resampling:enable_far_mode', ({ payload }) => {
    app.adaptiveResamplingEnableFarMode = Number(payload.enabled) !== 0;
    updateAdaptiveResamplingUI();
  });

  listen('adaptive_resampling:force_silence_in_far_mode', ({ payload }) => {
    app.adaptiveResamplingForceSilenceInFarMode = Number(payload.enabled) !== 0;
    updateAdaptiveResamplingUI();
  });

  listen('adaptive_resampling:hard_recover_high_in_far_mode', ({ payload }) => {
    app.adaptiveResamplingHardRecoverHighInFarMode = Number(payload.enabled) !== 0;
    updateAdaptiveResamplingUI();
  });

  listen('adaptive_resampling:hard_recover_low_in_far_mode', ({ payload }) => {
    app.adaptiveResamplingHardRecoverLowInFarMode = Number(payload.enabled) !== 0;
    updateAdaptiveResamplingUI();
  });

  listen('adaptive_resampling:far_mode_return_fade_in_ms', ({ payload }) => {
    app.adaptiveResamplingFarModeReturnFadeInMs = Number(payload.value) || 0;
    updateAdaptiveResamplingUI();
  });

  listen('adaptive_resampling:kp_near', ({ payload }) => {
    app.adaptiveResamplingKpNear = Number(payload.value);
    updateAdaptiveResamplingUI();
  });

  listen('adaptive_resampling:ki', ({ payload }) => {
    app.adaptiveResamplingKi = Number(payload.value);
    updateAdaptiveResamplingUI();
  });

  listen('adaptive_resampling:integral_discharge_ratio', ({ payload }) => {
    app.adaptiveResamplingIntegralDischargeRatio = Number(payload.value);
    updateAdaptiveResamplingUI();
  });

  listen('adaptive_resampling:max_adjust', ({ payload }) => {
    app.adaptiveResamplingMaxAdjust = Number(payload.value);
    updateAdaptiveResamplingUI();
  });

  listen('adaptive_resampling:near_far_threshold_ms', ({ payload }) => {
    app.adaptiveResamplingNearFarThresholdMs = Number(payload.value);
    updateAdaptiveResamplingUI();
  });

  listen('adaptive_resampling:update_interval_callbacks', ({ payload }) => {
    app.adaptiveResamplingUpdateIntervalCallbacks = Number(payload.value);
    updateAdaptiveResamplingUI();
  });

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

  listen('latency:target', ({ payload }) => {
    app.latencyTargetMs = Number(payload.value);
    updateLatencyDisplay();
    updateLatencyMeterUI();
  });

  listen('latency:requested', ({ payload }) => {
    app.latencyRequestedMs = Number(payload.value);
    updateLatencyDisplay();
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

  listen('audio:sample_rate', ({ payload }) => {
    const v = Number(payload.value);
    app.audioSampleRate = Number.isFinite(v) && v > 0 ? Math.round(v) : null;
    updateAudioFormatDisplay();
  });

  listen('state:ramp_mode', ({ payload }) => {
    const next = String(payload?.value || '').trim().toLowerCase();
    if (next === 'off' || next === 'frame' || next === 'sample') {
      app.rampMode = next;
      updateAudioFormatDisplay();
    }
  });

  listen('audio:output_device', ({ payload }) => {
    app.audioOutputDevice = typeof payload.value === 'string' ? (payload.value.trim() || null) : null;
    updateAudioFormatDisplay();
  });

  listen('audio:output_device:requested', ({ payload }) => {
    app.audioOutputDevice = typeof payload.value === 'string' ? (payload.value.trim() || null) : null;
    updateAudioFormatDisplay();
  });

  listen('audio:output_device:effective', ({ payload }) => {
    app.audioOutputDeviceEffective = typeof payload.value === 'string' ? (payload.value.trim() || null) : null;
    updateAudioFormatDisplay();
  });

  listen('audio:output_devices', ({ payload }) => {
    app.audioOutputDevices = Array.isArray(payload.values)
      ? payload.values
        .map((entry) => ({
          value: String(entry?.value || '').trim(),
          label: String(entry?.label || entry?.value || '').trim()
        }))
        .filter((entry) => entry.value.length > 0)
      : [];
    updateAudioFormatDisplay();
  });

  listen('audio:sample_format', ({ payload }) => {
    app.audioSampleFormat = typeof payload.value === 'string' ? (payload.value.trim() || null) : null;
    updateAudioFormatDisplay();
  });

  listen('audio:error', ({ payload }) => {
    app.audioError = typeof payload.value === 'string' ? (payload.value.trim() || null) : null;
    updateAudioFormatDisplay();
  });

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
    pushLog(level, target ? `[${target}] ${message}` : message);
  });
}
