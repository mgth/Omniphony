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
  speakerGainCache,
  dirty,
  hasProducerDomain,
  hasControlConfig,
  isEmbeddedProducer
} from './state.js';

import { updateSource, updateSourceLevel, updateSourceGains } from './sources.js';
import {
  updateSpeakerLevel,
  renderSpeakerEditor,
  hydrateLayoutSelect,
  refreshOverlayLists
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
import { updateMasterGainUI, updateMasterMeterUI, updateAutoGainUI, updateAutoGainCeilingUI } from './controls/master.js';
import {
  updateRenderBackend,
  updateEvaluationMode,
  updateVbapCartesian,
  updateVbapPolar,
  updateVbapPositionInterpolation,
  renderVbapStatus
} from './controls/vbap.js';
import { updateAudioFormatDisplay, updateOutputChannelMappingUI } from './controls/audio.js';
import { materializeDefaultVirtualBed } from './controls/virtual-bed.js';
import { updateInputControlUI } from './controls/input.js';
import { updateAdaptiveResamplingUI } from './controls/adaptive.js';
import { syncMeterRateFromRenderer } from './controls/osc.js';
import { syncDiagRateFromRenderer } from './controls/diag-plot.js';
import { updateDistanceDiffuseUI } from './controls/distance-diffuse.js';
import { setOscStatus } from './controls/osc.js';
import { renderDrcUI } from './controls/drc.js';
import { applyBinauralState } from './controls/binaural.js';
import { setHeadPoseTarget } from './scene/head-pose.js';
import { updateLoudnessDisplay, updateDistanceModelUI } from './controls/master.js';
import { updateRoomRatioDisplay, applyRoomRatio, applyRoomRatioToScene } from './controls/room-geometry.js';
import { updateConfigSavedUI, updateAboutConfigPath, updateAboutRendererVersion } from './controls/config.js';
import { normalizeLogLevel, renderLogLevelControl, logState } from './log.js';
import { syncRuntimeConnectionLock } from './runtime-connection.js';
import { setInputSectionOpen } from './modals.js';
import { syncMpvOverlayPrefs } from './mpvOverlay.js';

// Show/hide panels that only apply to a renderer with an audio-output stage.
// The embedded (mpv/liborender) variant advertises no `audio` domain and no
// `adaptive_resampling` controlConfig, so its output-device, resampler and
// latency-target panels are hidden via body classes (CSS uses !important so it
// wins over the panels' own open/close toggles). Before the handshake
// (capabilities null) nothing is hidden — the default standalone UI is shown.
export function applyProducerCapabilityVisibility() {
  const known = !!app.producerCapabilities;
  document.body.classList.toggle('cap-no-audio', known && !hasProducerDomain('audio'));
  document.body.classList.toggle('cap-no-resampler', known && !hasControlConfig('adaptive_resampling'));
  // Embedded (mpv) host: Studio doesn't manage the orender process or audio
  // input, so the connect/service/launch buttons and the audio-input controls
  // are hidden; only the decoder bridge path stays accessible. This only holds
  // while the link is actually up: the capability handshake is NOT re-sent on
  // disconnect, so the moment we leave 'connected' (reconnecting/error/
  // initializing) the buttons must reappear so the user can bring up a
  // standalone renderer. Hence the live-status gate, not capabilities alone.
  document.body.classList.toggle(
    'cap-embedded',
    known && isEmbeddedProducer() && app.oscStatusState === 'connected'
  );
}

export function applyInitState(payload) {
  applyBinauralState(payload?.binaural);
  setHeadPoseTarget(payload?.binaural);
  app.producerCapabilities =
    payload?.producerCapabilities && typeof payload.producerCapabilities === 'object'
      ? payload.producerCapabilities
      : null;
  applyProducerCapabilityVisibility();
  app.producerSession =
    payload?.producerSession && typeof payload.producerSession === 'object'
      ? payload.producerSession
      : null;
  if (typeof payload.oscSnapshotReady === 'boolean') {
    app.oscSnapshotReady = payload.oscSnapshotReady;
    syncRuntimeConnectionLock();
  }
  // Monitoring cadences: the renderer is the source of truth — sync the UI
  // selects from its broadcast value (/omniphony/state/monitoring).
  if (typeof payload.meterRateHz === 'number') syncMeterRateFromRenderer(payload.meterRateHz);
  if (typeof payload.diagRateHz === 'number') syncDiagRateFromRenderer(payload.diagRateHz);
  speakerGainCache.clear();
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
  if (payload.renderBackendState && typeof payload.renderBackendState === 'object') {
    // Accept any backend id the engine reports (built-in or contributor-
    // registered); the engine already validated it against its registry.
    if (typeof payload.renderBackendState.selection === 'string') {
      const selection = payload.renderBackendState.selection.trim().toLowerCase();
      if (selection) {
        app.renderBackendState.selection = selection;
      }
    }
    if (typeof payload.renderBackendState.effective === 'string') {
      const effective = payload.renderBackendState.effective.trim().toLowerCase();
      if (effective) {
        app.renderBackendState.effective = effective;
      }
    }
    app.renderBackendState.effectiveLabel = typeof payload.renderBackendState.effectiveLabel === 'string'
      ? payload.renderBackendState.effectiveLabel
      : null;
    app.renderBackendState.capabilities = payload.renderBackendState.capabilities && typeof payload.renderBackendState.capabilities === 'object'
      ? payload.renderBackendState.capabilities
      : null;
    app.renderBackendState.allowedEvaluationModes = Array.isArray(payload.renderBackendState.allowedEvaluationModes)
      ? payload.renderBackendState.allowedEvaluationModes
        .map((value) => String(value ?? '').trim().toLowerCase())
        .filter((value) => value.length > 0)
      : [];
    app.renderBackendState.frozenRoomRatio = payload.renderBackendState.frozenRoomRatio === true;
    app.renderBackendState.frozenSpeakers = payload.renderBackendState.frozenSpeakers === true;
    app.renderBackendState.restoreBackendAvailable = payload.renderBackendState.restoreBackendAvailable === true;
    // Engine-published list of selectable backends (id + label + param schema)
    // and every backend's current param values — drive the dynamic dropdown and
    // the generated per-backend controls (including hybrid inner-backend tabs).
    app.renderBackendState.availableBackends = Array.isArray(payload.renderBackendState.availableBackends)
      ? payload.renderBackendState.availableBackends
      : [];
    app.renderBackendState.backendParamValuesById = payload.renderBackendState.backendParamValuesById
      && typeof payload.renderBackendState.backendParamValuesById === 'object'
      ? payload.renderBackendState.backendParamValuesById
      : {};
    const hybrid = payload.renderBackendState.hybrid;
    if (hybrid && typeof hybrid === 'object') {
      // Any registered backend can be a hybrid inner model, except a nested
      // hybrid. The dropdown is populated from `availableBackends`; accept an id
      // present in that list (or any non-hybrid id if the list isn't published).
      const available = app.renderBackendState.availableBackends;
      const validInner = (id) =>
        typeof id === 'string'
        && id.length > 0
        && id !== 'hybrid'
        && (!Array.isArray(available)
          || available.length === 0
          || available.some((b) => String(b.id) === id));
      const external = typeof hybrid.externalBackend === 'string'
        ? hybrid.externalBackend.trim().toLowerCase()
        : null;
      const internal = typeof hybrid.internalBackend === 'string'
        ? hybrid.internalBackend.trim().toLowerCase()
        : null;
      app.renderBackendState.hybrid.externalBackend = validInner(external) ? external : null;
      app.renderBackendState.hybrid.internalBackend = validInner(internal) ? internal : null;
      app.renderBackendState.hybrid.curve = Array.isArray(hybrid.curve)
        ? hybrid.curve
          .filter((point) => Array.isArray(point) && point.length === 2
            && Number.isFinite(point[0]) && Number.isFinite(point[1]))
          .map((point) => [
            Math.min(1, Math.max(0, point[0])),
            Math.min(1, Math.max(0, point[1]))
          ])
        : null;
      if (typeof hybrid.metric === 'string') {
        const metric = hybrid.metric.trim().toLowerCase();
        if (['spherical', 'chebyshev'].includes(metric)) {
          app.renderBackendState.hybrid.metric = metric;
        }
      }
      if (Number.isFinite(hybrid.curveSmoothing)) {
        app.renderBackendState.hybrid.curveSmoothing = Math.min(1, Math.max(0, hybrid.curveSmoothing));
      }
    }
  }
  if (typeof payload.objectSizeIntervals === 'number' && payload.objectSizeIntervals >= 0) {
    app.objectSizeIntervals = Math.round(payload.objectSizeIntervals);
  }
  if (payload.renderEvaluationModeState && typeof payload.renderEvaluationModeState === 'object') {
    if (typeof payload.renderEvaluationModeState.selection === 'string') {
      const selection = payload.renderEvaluationModeState.selection.trim().toLowerCase();
      if (['auto', 'realtime', 'precomputed_polar', 'precomputed_cartesian'].includes(selection)) {
        app.evaluationModeState.selection = selection;
      }
    }
    if (typeof payload.renderEvaluationModeState.effective === 'string') {
      const effective = payload.renderEvaluationModeState.effective.trim().toLowerCase();
      if (['realtime', 'precomputed_polar', 'precomputed_cartesian'].includes(effective)) {
        app.evaluationModeState.effective = effective;
      }
    }
  }
  updateRenderBackend();
  updateEvaluationMode();
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
  if (typeof payload.autoGain === 'boolean') {
    app.autoGain = payload.autoGain;
  }
  if (typeof payload.autoGainCeilingDb === 'number') {
    app.autoGainCeilingDb = payload.autoGainCeilingDb;
  }
  updateMasterGainUI();
  updateAutoGainUI();
  updateAutoGainCeilingUI();
  const distanceModelValue =
    typeof payload.distanceModel === 'string'
      ? payload.distanceModel
      : payload?.distanceModel?.value;
  if (typeof distanceModelValue === 'string') {
    const value = distanceModelValue.trim().toLowerCase();
    if (['none', 'linear', 'quadratic', 'inverse-square'].includes(value)) {
      app.distanceModel = value;
    }
  }
  const distanceModelMetric =
    typeof payload.distanceModelMetric === 'string'
      ? payload.distanceModelMetric
      : payload?.distanceModel?.metric;
  if (typeof distanceModelMetric === 'string') {
    const metric = distanceModelMetric.trim().toLowerCase();
    if (['spherical', 'chebyshev'].includes(metric)) {
      app.distanceModelMetric = metric;
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
    if (typeof payload.distanceDiffuse.metric === 'string') {
      const metric = payload.distanceDiffuse.metric.trim().toLowerCase();
      if (['spherical', 'chebyshev'].includes(metric)) {
        app.distanceDiffuseState.metric = metric;
      }
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
  if (typeof payload.adaptiveResamplingHighRecoverEntryMarginMs === 'number') {
    app.adaptiveResamplingHighRecoverEntryMarginMs = payload.adaptiveResamplingHighRecoverEntryMarginMs;
  }
  if (typeof payload.adaptiveResamplingUpdateIntervalCallbacks === 'number') {
    app.adaptiveResamplingUpdateIntervalCallbacks = payload.adaptiveResamplingUpdateIntervalCallbacks;
  }
  if (typeof payload.adaptiveResamplingLowRecoverSettleStableMs === 'number') {
    app.adaptiveResamplingLowRecoverSettleStableMs = payload.adaptiveResamplingLowRecoverSettleStableMs;
  }
  if (typeof payload.adaptiveResamplingLowRecoverEntryMarginMs === 'number') {
    app.adaptiveResamplingLowRecoverEntryMarginMs = payload.adaptiveResamplingLowRecoverEntryMarginMs;
  }
  if (typeof payload.adaptiveResamplingLowRecoverExitMarginMs === 'number') {
    app.adaptiveResamplingLowRecoverExitMarginMs = payload.adaptiveResamplingLowRecoverExitMarginMs;
  }
  if (typeof payload.adaptiveResamplingLowRecoverSettleMarginMs === 'number') {
    app.adaptiveResamplingLowRecoverSettleMarginMs = payload.adaptiveResamplingLowRecoverSettleMarginMs;
  }
  if (typeof payload.adaptiveResamplingLowRecoverRefillDeltaAlpha === 'number') {
    app.adaptiveResamplingLowRecoverRefillDeltaAlpha = payload.adaptiveResamplingLowRecoverRefillDeltaAlpha;
  }
  if (typeof payload.adaptiveResamplingControlSmoothingCutoffHz === 'number') {
    app.adaptiveResamplingControlSmoothingCutoffHz = payload.adaptiveResamplingControlSmoothingCutoffHz;
  }
  if (typeof payload.adaptiveResamplingControlSmoothingOrder === 'number') {
    app.adaptiveResamplingControlSmoothingOrder = payload.adaptiveResamplingControlSmoothingOrder;
  }
  if (typeof payload.adaptiveResamplingUsePreBridgeClock === 'number') {
    app.adaptiveResamplingUsePreBridgeClock = payload.adaptiveResamplingUsePreBridgeClock !== 0;
  }
  if (typeof payload.adaptiveResamplingUseOutputPacing === 'number') {
    app.adaptiveResamplingUseOutputPacing = payload.adaptiveResamplingUseOutputPacing !== 0;
  }
  if (typeof payload.adaptiveResamplingDisableBackpressure === 'number') {
    app.adaptiveResamplingDisableBackpressure = payload.adaptiveResamplingDisableBackpressure !== 0;
  }
  if (typeof payload.adaptiveResamplingBand === 'string') {
    app.adaptiveResamplingBand = payload.adaptiveResamplingBand;
  }
  if (typeof payload.adaptiveResamplingState === 'string') {
    app.adaptiveResamplingState = payload.adaptiveResamplingState;
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
  if (typeof payload.latencySmoothedMs === 'number') {
    app.latencySmoothedMs = payload.latencySmoothedMs;
  }
  if (typeof payload.latencyDownstreamMs === 'number') {
    app.latencyDownstreamMs = payload.latencyDownstreamMs;
  }
  if (typeof payload.latencyTargetMs === 'number') {
    app.latencyTargetMs = payload.latencyTargetMs;
  }
  if (typeof payload.latencyRequestedMs === 'number') {
    app.latencyRequestedMs = payload.latencyRequestedMs;
    if (app.latencyTargetMs === null) {
      app.latencyTargetMs = payload.latencyRequestedMs;
    }
    if (app.latencyMs === null) {
      app.latencyMs = payload.latencyRequestedMs;
    }
  }
  if (typeof payload.decodeTimeMs === 'number') {
    setDecodeTimeMs(payload.decodeTimeMs);
  }
  if (typeof payload.renderTimeMs === 'number') {
    setRenderTimeMs(payload.renderTimeMs);
  }
  if (typeof payload.writeTimeMs === 'number') {
    setWriteTimeMs(payload.writeTimeMs);
  }
  if (typeof payload.frameDurationMs === 'number') {
    setFrameDurationMs(payload.frameDurationMs);
  }
  if (typeof payload.resampleRatio === 'number') {
    app.resampleRatio = payload.resampleRatio;
  }
  if (typeof payload.audioSampleRate === 'number') {
    app.audioSampleRate = payload.audioSampleRate > 0 ? payload.audioSampleRate : null;
  }
  if (typeof payload.rampMode === 'string') {
    const next = payload.rampMode.trim().toLowerCase();
    if (next === 'off' || next === 'frame' || next === 'sample' || next === 'interp') {
      app.rampMode = next;
    }
  }
  // Live 2D-sources / routing options: mirror the renderer's authoritative
  // values so the panel reflects the ENGINE state at (re)connect, not the JS
  // defaults. Without this the Side/Back buttons, spatialize toggle, object
  // generator and phantom controls silently show stale defaults while the
  // engine renders something else.
  if (typeof payload.channelRenderMode === 'string') {
    const next = payload.channelRenderMode.trim().toLowerCase();
    // `direct`/`virtual` are legacy values that now collapse to `spatial`.
    if (next === 'host') {
      app.channelRenderMode = 'host';
    } else if (next === 'spatial' || next === 'direct' || next === 'virtual') {
      app.channelRenderMode = 'spatial';
    }
  }
  if (typeof payload.surroundPlacement === 'string') {
    const next = payload.surroundPlacement.trim().toLowerCase();
    if (next === 'side' || next === 'back') {
      app.surroundPlacement = next;
    }
  }
  if (typeof payload.objectGeneratorId === 'string') {
    // Empty string from the renderer means "off"; normalise to 'none'.
    const next = payload.objectGeneratorId.trim().toLowerCase();
    app.objectGeneratorId = next === '' ? 'none' : next;
  }
  if (payload.objectGeneratorParams && typeof payload.objectGeneratorParams === 'object') {
    app.objectGeneratorParams = payload.objectGeneratorParams;
  }
  if (typeof payload.objectGeneratorLayoutHasHeight === 'boolean') {
    app.objectGeneratorLayoutHasHeight = payload.objectGeneratorLayoutHasHeight;
  }
  if (typeof payload.phantomEnabled === 'boolean') {
    app.phantomEnabled = payload.phantomEnabled;
  }
  if (payload.phantomParams && typeof payload.phantomParams === 'object') {
    app.phantomParams = payload.phantomParams;
  }
  if (typeof payload.outputChannelMapping === 'string') {
    const next = payload.outputChannelMapping.trim().toLowerCase();
    if (next === 'by_index' || next === 'by_name') {
      app.outputChannelMapping = next;
    }
  }
  if (Array.isArray(payload.outputChannelMappingUnroutable)) {
    app.outputChannelMappingUnroutable = payload.outputChannelMappingUnroutable.filter(
      (n) => typeof n === 'string'
    );
  }
  if (Object.prototype.hasOwnProperty.call(payload, 'virtualBed')) {
    // null = renderer is on the built-in canonical poses; an object is the
    // configured/live bed. The editor seeds defaults when this is null.
    app.virtualBed =
      payload.virtualBed && typeof payload.virtualBed === 'object' ? payload.virtualBed : null;
    // First authoritative snapshot reporting no saved bed (and not in host mode):
    // materialise the canonical cartesian bed so the editor's values persist to
    // config.yaml (like `current_layout`) and are used in priority, instead of
    // relying on a built-in default. One-shot; once a bed exists this is skipped.
    if (!app.virtualBed && !app.virtualBedMaterialized && app.channelRenderMode !== 'host') {
      app.virtualBedMaterialized = true;
      materializeDefaultVirtualBed();
    }
  }
  // Declared live options passthrough (registry RFC phase 1) — ingested
  // generically; no per-option line needed here for registry options.
  if (payload.options && typeof payload.options === 'object') {
    Object.assign(app.options, payload.options);
  }
  if (typeof payload.audioOutputDevice === 'string') {
    app.audioOutputDevice = payload.audioOutputDevice.trim() || null;
  }
  if (typeof payload.audioOutputDeviceEffective === 'string') {
    app.audioOutputDeviceEffective = payload.audioOutputDeviceEffective.trim() || null;
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
  if (typeof payload.inputMode === 'string') {
    const value = payload.inputMode.trim().toLowerCase();
    if (value === 'bridge' || value === 'pipe_bridge' || value === 'live' || value === 'pipewire' || value === 'pipewire_bridge') {
      const normalized = value === 'bridge' ? 'pipe_bridge' : (value === 'live' ? 'pipewire' : value);
      if (!app.inputModeDirty || normalized === app.inputMode) {
        app.inputMode = normalized;
        app.inputModeDirty = false;
      }
    }
  }
  if (typeof payload.inputActiveMode === 'string') {
    const value = payload.inputActiveMode.trim().toLowerCase();
    if (value === 'bridge' || value === 'pipe_bridge' || value === 'live' || value === 'pipewire' || value === 'pipewire_bridge') {
      app.inputActiveMode = value === 'bridge' ? 'pipe_bridge' : (value === 'live' ? 'pipewire' : value);
    }
  }
  if (typeof payload.inputApplyPending === 'number') {
    const pending = payload.inputApplyPending !== 0;
    if (app.inputApplyAwaitingAck) {
      if (pending) {
        app.inputApplyPending = true;
        app.inputApplyAwaitingAck = false;
      }
    } else {
      app.inputApplyPending = pending;
    }
  }
  if (typeof payload.drcMode === 'string') {
    app.drcMode = payload.drcMode;
    dirty.drcUI = true;
  }
  if (typeof payload.drcWeight === 'number') {
    const clamped = Math.max(0, Math.min(1, payload.drcWeight));
    if (app.drcWeight !== clamped) {
      app.drcWeight = clamped;
      dirty.drcUI = true;
    }
  }
  if (Array.isArray(payload.supportedDrcModes)) {
    app.supportedDrcModes = payload.supportedDrcModes.map(m => String(m));
    dirty.drcUI = true;
  }
  if (typeof payload.inputBackend === 'string') {
    app.inputBackend = payload.inputBackend.trim() || null;
  }
  if (typeof payload.inputChannels === 'number') {
    app.inputChannels = payload.inputChannels > 0 ? payload.inputChannels : null;
  }
  if (typeof payload.inputSampleRate === 'number') {
    app.inputSampleRate = payload.inputSampleRate > 0 ? payload.inputSampleRate : null;
  }
  if (typeof payload.inputNode === 'string') {
    app.inputNode = payload.inputNode.trim() || null;
  }
  if (typeof payload.inputDescription === 'string') {
    app.inputDescription = payload.inputDescription.trim() || null;
  }
  if (typeof payload.inputStreamFormat === 'string') {
    app.inputStreamFormat = payload.inputStreamFormat.trim() || null;
  }
  if (typeof payload.inputError === 'string') {
    app.inputError = payload.inputError.trim() || null;
  }
  if (typeof payload.renderBridgePath === 'string') {
    app.renderBridgePath = payload.renderBridgePath.trim() || null;
  }
  if (typeof payload.renderConfigPath === 'string') {
    app.renderConfigPath = payload.renderConfigPath.trim() || null;
  }
  if (typeof payload.renderConfigStatus === 'string') {
    app.renderConfigStatus = payload.renderConfigStatus.trim() || null;
  }
  if (typeof payload.renderVersion === 'string') {
    app.renderVersion = payload.renderVersion.trim() || null;
  }
  if (typeof payload.renderAbi === 'string') {
    app.renderAbi = payload.renderAbi.trim() || null;
  }
  if (typeof payload.renderBridgeError === 'string') {
    app.renderBridgeError = payload.renderBridgeError.trim() || null;
  }
  updateAboutConfigPath();
  updateAboutRendererVersion();
  if (payload.liveInput && typeof payload.liveInput === 'object') {
    if (typeof payload.liveInput.backend === 'string') {
      app.liveInput.backend = payload.liveInput.backend.trim().toLowerCase() || app.liveInput.backend;
    }
    if (typeof payload.liveInput.node === 'string') {
      app.liveInput.node = payload.liveInput.node;
    }
    if (typeof payload.liveInput.description === 'string') {
      app.liveInput.description = payload.liveInput.description;
    }
    if (typeof payload.liveInput.layout === 'string') {
      app.liveInput.layout = payload.liveInput.layout;
    }
    if (typeof payload.liveInput.clockMode === 'string') {
      const clockMode = payload.liveInput.clockMode.trim().toLowerCase();
      if (!app.liveInputClockModeDirty || clockMode === app.liveInput.clockMode) {
        app.liveInput.clockMode = clockMode || app.liveInput.clockMode;
        app.liveInputClockModeDirty = false;
      }
    }
    if (typeof payload.liveInput.channels === 'number' && payload.liveInput.channels > 0) {
      app.liveInput.channels = payload.liveInput.channels;
    }
    if (typeof payload.liveInput.sampleRate === 'number' && payload.liveInput.sampleRate > 0) {
      app.liveInput.sampleRate = payload.liveInput.sampleRate;
    }
    if (typeof payload.liveInput.format === 'string') {
      app.liveInput.format = payload.liveInput.format.trim().toLowerCase() || app.liveInput.format;
    }
    if (typeof payload.liveInput.map === 'string') {
      app.liveInput.map = payload.liveInput.map.trim().toLowerCase() || app.liveInput.map;
    }
    if (typeof payload.liveInput.lfeMode === 'string') {
      app.liveInput.lfeMode = payload.liveInput.lfeMode.trim().toLowerCase() || app.liveInput.lfeMode;
    }
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
  syncRuntimeConnectionLock();
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
  updateRenderTimeUI();
  updateResampleRatioDisplay();
  updateAudioFormatDisplay();
  updateOutputChannelMappingUI();
  updateInputControlUI();
  renderDrcUI();
  // Auto-surface the Audio Input section on a bridge-class error, but only on
  // the error's rising edge: this runs on EVERY state snapshot, and while the
  // error persists it would otherwise reopen the panel each time the user
  // closes it (re-armed per connection in setOscStatus).
  if (
    app.inputError
    && /bridge path missing|no bridge plugin found|render\.bridge_path/i.test(app.inputError)
  ) {
    if (app.lastAutoOpenedInputError !== app.inputError) {
      app.lastAutoOpenedInputError = app.inputError;
      setInputSectionOpen(true);
    }
  } else {
    app.lastAutoOpenedInputError = null;
  }
  updateMasterMeterUI();
  renderLogLevelControl();

  hydrateLayoutSelect(payload.layouts || [], payload.selectedLayoutKey);
  refreshOverlayLists();
  updateMasterMeterUI();
  renderSpeakerEditor();

  // Push the persisted overlay display prefs to the renderer on (re)connect, so
  // they apply at startup rather than only on manual toggle.
  syncMpvOverlayPrefs();
}
