/**
 * Apply the full initial state snapshot received from the Rust backend.
 *
 * Called once after `invoke('get_state')` resolves, before event listeners
 * start streaming incremental updates.
 */

import {
  app,
  speakerMuted,
  objectMuted,
  speakerManualMuted,
  objectManualMuted,
  objectGainCache,
  speakerGainCache
} from './state.js';

import { updateSource, updateSourceLevel, updateSourceGains } from './sources.js';
import {
  updateSpeakerLevel,
  renderSpeakerEditor,
  hydrateLayoutSelect,
  refreshOverlayLists
} from './speakers.js';

import { setLatencyInstantMs, updateLatencyDisplay, updateLatencyMeterUI, updateRenderTimeUI, setRenderTimeMs, setDecodeTimeMs, setWriteTimeMs, updateResampleRatioDisplay } from './controls/latency.js';
import { updateMasterGainUI, updateMasterMeterUI } from './controls/master.js';
import { updateSpreadDisplay } from './controls/spread.js';
import {
  updateVbapMode,
  updateVbapCartesian,
  updateVbapPolar,
  updateVbapPositionInterpolation,
  renderVbapStatus
} from './controls/vbap.js';
import { updateAudioFormatDisplay } from './controls/audio.js';
import { updateAdaptiveResamplingUI } from './controls/adaptive.js';
import { updateDistanceDiffuseUI } from './controls/distance-diffuse.js';
import { setOscStatus } from './controls/osc.js';
import { updateLoudnessDisplay, updateDistanceModelUI } from './controls/master.js';
import { updateRoomRatioDisplay, applyRoomRatio, applyRoomRatioToScene } from './controls/room-geometry.js';
import { updateConfigSavedUI } from './controls/config.js';
import { normalizeLogLevel, renderLogLevelControl, logState } from './log.js';

export function applyInitState(payload) {
  speakerMuted.clear();
  objectMuted.clear();
  speakerManualMuted.clear();
  objectManualMuted.clear();

  Object.entries(payload.sources || {}).forEach(([id, position]) => {
    updateSource(id, position);
  });
  Object.entries(payload.sourceLevels || {}).forEach(([id, meter]) => {
    updateSourceLevel(id, meter);
  });
  Object.entries(payload.speakerLevels || {}).forEach(([index, meter]) => {
    updateSpeakerLevel(Number(index), meter);
  });
  Object.entries(payload.objectSpeakerGains || {}).forEach(([id, gains]) => {
    updateSourceGains(id, gains);
  });
  Object.entries(payload.objectGains || {}).forEach(([id, gain]) => {
    objectGainCache.set(String(id), Number(gain));
  });
  Object.entries(payload.speakerGains || {}).forEach(([id, gain]) => {
    speakerGainCache.set(String(id), Number(gain));
  });
  Object.entries(payload.objectMutes || {}).forEach(([id, muted]) => {
    const key = String(id);
    if (Number(muted)) {
      objectMuted.add(key);
    }
  });
  Object.entries(payload.speakerMutes || {}).forEach(([id, muted]) => {
    const key = String(id);
    if (Number(muted)) {
      speakerMuted.add(key);
    }
  });

  if (payload.roomRatio) {
    applyRoomRatio(payload.roomRatio);
  } else {
    updateRoomRatioDisplay();
    applyRoomRatioToScene();
  }
  if (payload.spread) {
    if (typeof payload.spread.min === 'number') {
      app.spreadState.min = payload.spread.min;
    }
    if (typeof payload.spread.max === 'number') {
      app.spreadState.max = payload.spread.max;
    }
    if (typeof payload.spread.fromDistance === 'boolean') {
      app.spreadState.fromDistance = payload.spread.fromDistance;
    }
    if (typeof payload.spread.distanceRange === 'number') {
      app.spreadState.distanceRange = payload.spread.distanceRange;
    }
    if (typeof payload.spread.distanceCurve === 'number') {
      app.spreadState.distanceCurve = payload.spread.distanceCurve;
    }
  }
  updateSpreadDisplay();
  if (payload.vbapCartesian) {
    if (typeof payload.vbapCartesian.xSize === 'number') {
      app.vbapCartesianState.xSize = payload.vbapCartesian.xSize > 0 ? payload.vbapCartesian.xSize : null;
    }
    if (typeof payload.vbapCartesian.ySize === 'number') {
      app.vbapCartesianState.ySize = payload.vbapCartesian.ySize > 0 ? payload.vbapCartesian.ySize : null;
    }
    if (typeof payload.vbapCartesian.zSize === 'number') {
      app.vbapCartesianState.zSize = payload.vbapCartesian.zSize > 0 ? payload.vbapCartesian.zSize : null;
    }
    if (typeof payload.vbapCartesian.zNegSize === 'number') {
      app.vbapCartesianState.zNegSize = payload.vbapCartesian.zNegSize >= 0 ? payload.vbapCartesian.zNegSize : 0;
    }
  }
  updateVbapCartesian();
  if (payload.vbapMode && typeof payload.vbapMode.selection === 'string') {
    const selection = payload.vbapMode.selection.trim().toLowerCase();
    if (selection === 'auto' || selection === 'polar' || selection === 'cartesian') {
      app.vbapModeState.selection = selection;
    }
  }
  if (payload.vbapMode && typeof payload.vbapMode.effectiveMode === 'string') {
    const effectiveMode = payload.vbapMode.effectiveMode.trim().toLowerCase();
    if (effectiveMode === 'polar' || effectiveMode === 'cartesian') {
      app.vbapModeState.effectiveMode = effectiveMode;
    }
  }
  updateVbapMode();
  if (payload.vbapPolar) {
    if (typeof payload.vbapPolar.azimuthResolution === 'number') {
      app.vbapPolarState.azimuthResolution = payload.vbapPolar.azimuthResolution > 0 ? payload.vbapPolar.azimuthResolution : null;
    }
    if (typeof payload.vbapPolar.elevationResolution === 'number') {
      app.vbapPolarState.elevationResolution = payload.vbapPolar.elevationResolution > 0 ? payload.vbapPolar.elevationResolution : null;
    }
    if (typeof payload.vbapPolar.distanceRes === 'number') {
      app.vbapPolarState.distanceRes = payload.vbapPolar.distanceRes > 0 ? payload.vbapPolar.distanceRes : null;
    }
    if (typeof payload.vbapPolar.distanceMax === 'number') {
      app.vbapPolarState.distanceMax = payload.vbapPolar.distanceMax > 0 ? payload.vbapPolar.distanceMax : null;
    }
    if (typeof payload.vbapPolar.positionInterpolation === 'boolean') {
      app.vbapPositionInterpolation = payload.vbapPolar.positionInterpolation;
    }
  }
  if (typeof payload.vbapAllowNegativeZ === 'boolean') {
    app.vbapAllowNegativeZ = payload.vbapAllowNegativeZ;
  }
  updateVbapPolar();
  updateVbapPositionInterpolation();
  if (typeof payload.vbapRecomputing === 'boolean') {
    app.vbapRecomputing = payload.vbapRecomputing;
  }
  renderVbapStatus();
  if (typeof payload.renderTimeMs === 'number') {
    setRenderTimeMs(payload.renderTimeMs);
  }
  if (typeof payload.decodeTimeMs === 'number') {
    setDecodeTimeMs(payload.decodeTimeMs);
  }
  if (typeof payload.writeTimeMs === 'number') {
    setWriteTimeMs(payload.writeTimeMs);
  }
  updateRenderTimeUI();
  if (typeof payload.loudness === 'number') {
    app.loudnessEnabled = payload.loudness !== 0;
  }
  if (typeof payload.loudnessSource === 'number') {
    app.loudnessSource = payload.loudnessSource;
  }
  if (typeof payload.loudnessGain === 'number') {
    app.loudnessGain = payload.loudnessGain;
  }
  updateLoudnessDisplay();
  if (typeof payload.masterGain === 'number') {
    app.masterGain = payload.masterGain;
  }
  updateMasterGainUI();
  if (payload.distanceModel && typeof payload.distanceModel.value === 'string') {
    const value = payload.distanceModel.value.trim().toLowerCase();
    if (['none', 'linear', 'quadratic', 'inverse-square'].includes(value)) {
      app.distanceModel = value;
    }
  }
  updateDistanceModelUI();
  if (payload.distanceDiffuse) {
    if (typeof payload.distanceDiffuse.enabled === 'boolean') {
      app.distanceDiffuseState.enabled = payload.distanceDiffuse.enabled;
    }
    if (typeof payload.distanceDiffuse.threshold === 'number') {
      app.distanceDiffuseState.threshold = payload.distanceDiffuse.threshold;
    }
    if (typeof payload.distanceDiffuse.curve === 'number') {
      app.distanceDiffuseState.curve = payload.distanceDiffuse.curve;
    }
  }
  updateDistanceDiffuseUI();
  if (typeof payload.adaptiveResampling === 'number') {
    app.adaptiveResamplingEnabled = payload.adaptiveResampling !== 0;
  }
  if (typeof payload.adaptiveResamplingPaused === 'number') {
    app.adaptiveResamplingPaused = payload.adaptiveResamplingPaused !== 0;
  }
  if (typeof payload.adaptiveResamplingEnableFarMode === 'number') {
    app.adaptiveResamplingEnableFarMode = payload.adaptiveResamplingEnableFarMode !== 0;
  }
  if (typeof payload.adaptiveResamplingForceSilenceInFarMode === 'number') {
    app.adaptiveResamplingForceSilenceInFarMode =
      payload.adaptiveResamplingForceSilenceInFarMode !== 0;
  }
  if (typeof payload.adaptiveResamplingHardRecoverHighInFarMode === 'number') {
    app.adaptiveResamplingHardRecoverHighInFarMode =
      payload.adaptiveResamplingHardRecoverHighInFarMode !== 0;
  }
  if (typeof payload.adaptiveResamplingHardRecoverLowInFarMode === 'number') {
    app.adaptiveResamplingHardRecoverLowInFarMode =
      payload.adaptiveResamplingHardRecoverLowInFarMode !== 0;
  }
  if (typeof payload.adaptiveResamplingFarModeReturnFadeInMs === 'number') {
    app.adaptiveResamplingFarModeReturnFadeInMs =
      payload.adaptiveResamplingFarModeReturnFadeInMs;
  }
  if (typeof payload.adaptiveResamplingKpNear === 'number') {
    app.adaptiveResamplingKpNear = payload.adaptiveResamplingKpNear;
  }
  if (typeof payload.adaptiveResamplingKi === 'number') {
    app.adaptiveResamplingKi = payload.adaptiveResamplingKi;
  }
  if (typeof payload.adaptiveResamplingIntegralDischargeRatio === 'number') {
    app.adaptiveResamplingIntegralDischargeRatio = payload.adaptiveResamplingIntegralDischargeRatio;
  }
  if (typeof payload.adaptiveResamplingMaxAdjust === 'number') {
    app.adaptiveResamplingMaxAdjust = payload.adaptiveResamplingMaxAdjust;
  }
  if (typeof payload.adaptiveResamplingNearFarThresholdMs === 'number') {
    app.adaptiveResamplingNearFarThresholdMs = payload.adaptiveResamplingNearFarThresholdMs;
  }
  if (typeof payload.adaptiveResamplingUpdateIntervalCallbacks === 'number') {
    app.adaptiveResamplingUpdateIntervalCallbacks = payload.adaptiveResamplingUpdateIntervalCallbacks;
  }
  if (typeof payload.adaptiveResamplingBand === 'string') {
    app.adaptiveResamplingBand = payload.adaptiveResamplingBand;
  }
  updateAdaptiveResamplingUI();
  if (typeof payload.configSaved === 'number') {
    app.configSaved = payload.configSaved !== 0;
  }
  updateConfigSavedUI();
  if (typeof payload.latencyMs === 'number') {
    app.latencyMs = payload.latencyMs;
  }
  if (typeof payload.latencyInstantMs === 'number') {
    setLatencyInstantMs(payload.latencyInstantMs);
  }
  if (typeof payload.latencyControlMs === 'number') {
    app.latencyControlMs = payload.latencyControlMs;
  }
  if (typeof payload.latencyTargetMs === 'number') {
    app.latencyTargetMs = payload.latencyTargetMs;
  }
  if (typeof payload.latencyRequestedMs === 'number') {
    app.latencyRequestedMs = payload.latencyRequestedMs;
  }
  if (typeof payload.decodeTimeMs === 'number') {
    app.decodeTimeMs = payload.decodeTimeMs;
  }
  if (typeof payload.renderTimeMs === 'number') {
    app.renderTimeMs = payload.renderTimeMs;
  }
  if (typeof payload.writeTimeMs === 'number') {
    app.writeTimeMs = payload.writeTimeMs;
  }
  if (typeof payload.frameDurationMs === 'number') {
    app.frameDurationMs = payload.frameDurationMs;
  }
  if (typeof payload.resampleRatio === 'number') {
    app.resampleRatio = payload.resampleRatio;
  }
  if (typeof payload.audioSampleRate === 'number') {
    app.audioSampleRate = payload.audioSampleRate > 0 ? payload.audioSampleRate : null;
  }
  if (typeof payload.rampMode === 'string') {
    const next = payload.rampMode.trim().toLowerCase();
    if (next === 'off' || next === 'frame' || next === 'sample') {
      app.rampMode = next;
    }
  }
  if (typeof payload.audioOutputDevice === 'string') {
    app.audioOutputDevice = payload.audioOutputDevice.trim() || null;
  }
  if (Array.isArray(payload.audioOutputDevices)) {
    app.audioOutputDevices = payload.audioOutputDevices
      .map((entry) => ({
        value: String(entry?.value || '').trim(),
        label: String(entry?.label || entry?.value || '').trim()
      }))
      .filter((entry) => entry.value.length > 0);
  }
  if (typeof payload.audioSampleFormat === 'string') {
    app.audioSampleFormat = payload.audioSampleFormat.trim() || null;
  }
  if (typeof payload.audioError === 'string') {
    app.audioError = payload.audioError.trim() || null;
  }
  if (typeof payload.orenderInputPipe === 'string') {
    app.orenderInputPipe = payload.orenderInputPipe.trim() || null;
  }
  if (typeof payload.oscStatus === 'string') {
    const s = payload.oscStatus;
    if (s === 'initializing' || s === 'connected' || s === 'reconnecting' || s === 'error') {
      setOscStatus(s);
    }
  }
  if (typeof payload.oscMeteringEnabled === 'number') {
    app.oscMeteringEnabled = payload.oscMeteringEnabled !== 0;
    const oscMeteringToggleEl = document.getElementById('oscMeteringToggle');
    if (oscMeteringToggleEl) oscMeteringToggleEl.checked = app.oscMeteringEnabled;
  }
  if (typeof payload.logLevel === 'string') {
    logState.backendLogLevel = normalizeLogLevel(payload.logLevel);
  }
  updateLatencyDisplay();
  updateLatencyMeterUI();
  updateResampleRatioDisplay();
  updateAudioFormatDisplay();
  updateMasterMeterUI();
  renderLogLevelControl();

  hydrateLayoutSelect(payload.layouts || [], payload.selectedLayoutKey);
  refreshOverlayLists();
  renderSpeakerEditor();
}
