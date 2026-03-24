/**
 * UI event listener registrations.
 *
 * Extracted from app.js lines 6772-8028 (plus locale/layout select listeners).
 * Every `if (xxxEl) { xxxEl.addEventListener(...) }` block and every
 * `document.addEventListener(...)` call lives here.
 */

import { invoke } from '@tauri-apps/api/core';
import { app, speakerBaseGains, speakerDelays, sourceTrails, sourceNames } from './state.js';
import { formatNumber } from './coordinates.js';
import { t, tf, i18nState, setLocale, normalizeLocalePreference, applyStaticTranslations, LOCALE_STORAGE_KEY } from './i18n.js';
import {
  pushLog, logState, renderLogPanel, renderLogLevelControl,
  normalizeLogLevel, normalizeLogError, setLogExpanded, copyLogsToClipboard
} from './log.js';
import {
  setTrailInfoModalOpen, setEffectiveRenderInfoModalOpen,
  setOscInfoModalOpen, setAboutModalOpen, setRoomGeometryInfoModalOpen,
  setAdaptiveResamplingInfoModalOpen, setTelemetryGaugesInfoModalOpen,
  setRampModeInfoModalOpen, setVbapPositionInterpolationInfoModalOpen,
  setSpreadFromDistanceInfoModalOpen, setDistanceDiffuseInfoModalOpen,
  setAdaptiveResamplingAdvancedOpen, setTelemetryGaugesOpen,
  setDisplaySectionOpen, setAudioOutputSectionOpen, setRendererSectionOpen
} from './modals.js';
import { updateMasterGainUI, updateLoudnessDisplay, updateDistanceModelUI } from './controls/master.js';
import { updateSpreadDisplay } from './controls/spread.js';
import { renderVbapStatus, updateVbapMode, updateVbapCartesian, updateVbapPolar, updateVbapPositionInterpolation } from './controls/vbap.js';
import { updateDistanceDiffuseUI } from './controls/distance-diffuse.js';
import { updateAdaptiveResamplingUI, resetAdaptiveResamplingAdvancedDirtyState } from './controls/adaptive.js';
import {
  closeAudioSampleRateMenu, openAudioSampleRateMenu, updateAudioFormatDisplay,
  applyAudioSampleRateNow, applyAudioOutputDeviceNow, applyRampModeNow
} from './controls/audio.js';
import { updateConfigSavedUI } from './controls/config.js';
import { applyLatencyTargetNow, updateLatencyDisplay } from './controls/latency.js';
import {
  persistRoomGeometryPrefs, getRoomCenterBlendFromInput, renderRoomCenterBlendControl,
  normalizeRoomGeometryInputDisplays, updateRoomGeometryButtonsState,
  applyRoomGeometryNow, scheduleRoomGeometryApply, applyRoomGeometryStateToInputs,
  updateRoomGeometryLivePreview, refreshRoomGeometryInputState,
  persistTrailPrefs, persistEffectiveRenderPrefs, refreshEffectiveRenderVisibility,
  getRoomDriverValue
} from './controls/room-geometry.js';
import {
  renderSpeakerEditor, requestAddSpeaker, requestMoveSpeaker, requestRemoveSpeaker,
  applySpeakerCartesianEdit, applySpeakerPolarEdit,
  setSpeakerSpatializeLocal, setSpeakerCoordMode,
  updateSpeakerVisualsFromState, updateSpeakerGizmo, updateControlsForEditMode,
  computeAndApplySpeakerDelays, adjustSpeakerDistancesFromDelays,
  samplesToDelayMs,
  sanitizeLayoutExportName, defaultLayoutExportNameFromSpeakers, serializeCurrentLayoutForExport,
  refreshOverlayLists, hydrateLayoutSelect
} from './speakers.js';
import { applyGroupGains } from './mute-solo.js';
import { rebuildTrailGeometry, createTrailRenderable } from './trails.js';
import { scene } from './scene/setup.js';
import { renderVbapCartesianGridToggle, updateVbapCartesianFaceGrid } from './scene/gizmos.js';
import { setOscStatus } from './controls/osc.js';

export function setupUIListeners() {
  // ── DOM element queries ─────────────────────────────────────────────────

  const masterGainSliderEl = document.getElementById('masterGainSlider');
  const loudnessToggleEl = document.getElementById('loudnessToggle');
  const adaptiveResamplingToggleEl = document.getElementById('adaptiveResamplingToggle');
  const adaptiveFarModeToggleEl = document.getElementById('adaptiveFarModeToggle');
  const adaptiveFarSilenceToggleEl = document.getElementById('adaptiveFarSilenceToggle');
  const adaptiveFarHardRecoverToggleEl = document.getElementById('adaptiveFarHardRecoverToggle');
  const adaptiveFarFadeInMsInputEl = document.getElementById('adaptiveFarFadeInMsInput');
  const adaptiveResamplingAdvancedApplyBtnEl = document.getElementById('adaptiveResamplingAdvancedApplyBtn');
  const adaptiveResamplingAdvancedCancelBtnEl = document.getElementById('adaptiveResamplingAdvancedCancelBtn');
  const adaptiveKpNearInputEl = document.getElementById('adaptiveKpNearInput');
  const adaptiveKiInputEl = document.getElementById('adaptiveKiInput');
  const adaptiveMaxAdjustInputEl = document.getElementById('adaptiveMaxAdjustInput');
  const adaptiveNearFarThresholdInputEl = document.getElementById('adaptiveNearFarThresholdInput');
  const adaptiveUpdateIntervalCallbacksInputEl = document.getElementById('adaptiveUpdateIntervalCallbacksInput');
  const adaptiveMeasurementSmoothingAlphaInputEl = document.getElementById('adaptiveMeasurementSmoothingAlphaInput');
  const spreadMinSliderEl = document.getElementById('spreadMinSlider');
  const spreadMaxSliderEl = document.getElementById('spreadMaxSlider');
  const spreadFromDistanceToggleEl = document.getElementById('spreadFromDistanceToggle');
  const spreadDistanceRangeSliderEl = document.getElementById('spreadDistanceRangeSlider');
  const spreadDistanceCurveSliderEl = document.getElementById('spreadDistanceCurveSlider');
  const distanceModelSelectEl = document.getElementById('distanceModelSelect');
  const vbapCartXSizeInputEl = document.getElementById('vbapCartXSizeInput');
  const vbapCartYSizeInputEl = document.getElementById('vbapCartYSizeInput');
  const vbapCartZSizeInputEl = document.getElementById('vbapCartZSizeInput');
  const vbapCartZNegSizeInputEl = document.getElementById('vbapCartZNegSizeInput');
  const vbapCartesianGridToggleBtnEl = document.getElementById('vbapCartesianGridToggleBtn');
  const vbapModeAutoBtnEl = document.getElementById('vbapModeAutoBtn');
  const vbapModePolarBtnEl = document.getElementById('vbapModePolarBtn');
  const vbapModeCartesianBtnEl = document.getElementById('vbapModeCartesianBtn');
  const vbapPolarAzimuthResolutionInputEl = document.getElementById('vbapPolarAzimuthResolutionInput');
  const vbapPolarElevationResolutionInputEl = document.getElementById('vbapPolarElevationResolutionInput');
  const vbapPolarDistanceResInputEl = document.getElementById('vbapPolarDistanceResInput');
  const vbapPolarDistanceMaxInputEl = document.getElementById('vbapPolarDistanceMaxInput');
  const vbapPositionInterpolationToggleEl = document.getElementById('vbapPositionInterpolationToggleEl');
  const distanceDiffuseToggleEl = document.getElementById('distanceDiffuseToggle');
  const distanceDiffuseThresholdSliderEl = document.getElementById('distanceDiffuseThresholdSlider');
  const distanceDiffuseThresholdValEl = document.getElementById('distanceDiffuseThresholdVal');
  const distanceDiffuseCurveSliderEl = document.getElementById('distanceDiffuseCurveSlider');
  const distanceDiffuseCurveValEl = document.getElementById('distanceDiffuseCurveVal');
  const spreadFromDistanceInfoBtnEl = document.getElementById('spreadFromDistanceInfoBtn');
  const spreadFromDistanceInfoModalEl = document.getElementById('spreadFromDistanceInfoModal');
  const spreadFromDistanceInfoCloseBtnEl = document.getElementById('spreadFromDistanceInfoCloseBtn');
  const distanceDiffuseInfoBtnEl = document.getElementById('distanceDiffuseInfoBtn');
  const distanceDiffuseInfoModalEl = document.getElementById('distanceDiffuseInfoModal');
  const distanceDiffuseInfoCloseBtnEl = document.getElementById('distanceDiffuseInfoCloseBtn');
  const trailInfoBtnEl = document.getElementById('trailInfoBtn');
  const effectiveRenderInfoBtnEl = document.getElementById('effectiveRenderInfoBtn');
  const trailInfoCloseBtnEl = document.getElementById('trailInfoCloseBtn');
  const effectiveRenderInfoCloseBtnEl = document.getElementById('effectiveRenderInfoCloseBtn');
  const trailInfoModalEl = document.getElementById('trailInfoModal');
  const effectiveRenderInfoModalEl = document.getElementById('effectiveRenderInfoModal');
  const oscInfoBtnEl = document.getElementById('oscInfoBtn');
  const aboutBtnEl = document.getElementById('aboutBtn');
  const aboutCloseBtnEl = document.getElementById('aboutCloseBtn');
  const aboutModalEl = document.getElementById('aboutModal');
  const oscInfoCloseBtnEl = document.getElementById('oscInfoCloseBtn');
  const oscInfoModalEl = document.getElementById('oscInfoModal');
  const roomGeometryInfoBtnEl = document.getElementById('roomGeometryInfoBtn');
  const roomGeometryInfoCloseBtnEl = document.getElementById('roomGeometryInfoCloseBtn');
  const roomGeometryInfoModalEl = document.getElementById('roomGeometryInfoModal');
  const adaptiveResamplingInfoBtnEl = document.getElementById('adaptiveResamplingInfoBtn');
  const adaptiveResamplingInfoCloseBtnEl = document.getElementById('adaptiveResamplingInfoCloseBtn');
  const adaptiveResamplingInfoModalEl = document.getElementById('adaptiveResamplingInfoModal');
  const telemetryGaugesInfoBtnEl = document.getElementById('telemetryGaugesInfoBtn');
  const telemetryGaugesInfoCloseBtnEl = document.getElementById('telemetryGaugesInfoCloseBtn');
  const telemetryGaugesInfoModalEl = document.getElementById('telemetryGaugesInfoModal');
  const rampModeInfoBtnEl = document.getElementById('rampModeInfoBtn');
  const rampModeInfoCloseBtnEl = document.getElementById('rampModeInfoCloseBtn');
  const rampModeInfoModalEl = document.getElementById('rampModeInfoModal');
  const vbapPositionInterpolationInfoBtnEl = document.getElementById('vbapPositionInterpolationInfoBtn');
  const vbapPositionInterpolationInfoCloseBtnEl = document.getElementById('vbapPositionInterpolationInfoCloseBtn');
  const vbapPositionInterpolationInfoModalEl = document.getElementById('vbapPositionInterpolationInfoModal');
  const adaptiveResamplingAdvancedToggleBtnEl = document.getElementById('adaptiveResamplingAdvancedToggleBtn');
  const telemetryGaugesToggleBtnEl = document.getElementById('telemetryGaugesToggleBtn');
  const displaySectionToggleBtnEl = document.getElementById('displaySectionToggleBtn');
  const audioOutputSectionToggleBtnEl = document.getElementById('audioOutputSectionToggleBtn');
  const rendererSectionToggleBtnEl = document.getElementById('rendererSectionToggleBtn');
  const roomGeometryCancelBtnEl = document.getElementById('roomGeometryCancelBtn');
  const roomMasterAxisInputs = Array.from(document.querySelectorAll('input[name="roomMasterAxis"]'));
  const roomDriverWidthEl = document.getElementById('roomDriverWidth');
  const roomDriverLengthEl = document.getElementById('roomDriverLength');
  const roomDriverHeightEl = document.getElementById('roomDriverHeight');
  const roomDriverRearEl = document.getElementById('roomDriverRear');
  const roomDriverLowerEl = document.getElementById('roomDriverLower');
  const roomDimWidthInputEl = document.getElementById('roomDimWidthInput');
  const roomDimLengthInputEl = document.getElementById('roomDimLengthInput');
  const roomDimHeightInputEl = document.getElementById('roomDimHeightInput');
  const roomDimRearInputEl = document.getElementById('roomDimRearInput');
  const roomDimLowerInputEl = document.getElementById('roomDimLowerInput');
  const roomRatioWidthInputEl = document.getElementById('roomRatioWidthInput');
  const roomRatioLengthInputEl = document.getElementById('roomRatioLengthInput');
  const roomRatioHeightInputEl = document.getElementById('roomRatioHeightInput');
  const roomRatioRearInputEl = document.getElementById('roomRatioRearInput');
  const roomRatioLowerInputEl = document.getElementById('roomRatioLowerInput');
  const roomRatioCenterBlendSliderEl = document.getElementById('roomRatioCenterBlendSlider');
  const roomRatioCenterBlendValueEl = document.getElementById('roomRatioCenterBlendValue');
  const saveConfigBtnEl = document.getElementById('saveConfigBtn');
  const reloadConfigBtnEl = document.getElementById('reloadConfigBtn');
  const logToggleBtnEl = document.getElementById('logToggleBtn');
  const logClearBtnEl = document.getElementById('logClearBtn');
  const logCopyBtnEl = document.getElementById('logCopyBtn');
  const logLevelSelectEl = document.getElementById('logLevelSelect');
  const latencyTargetInputEl = document.getElementById('latencyTargetInput');
  const adaptiveKpFarInputEl = document.getElementById('adaptiveKpFarInput');
  const adaptiveMaxAdjustFarInputEl = document.getElementById('adaptiveMaxAdjustFarInput');
  const audioSampleRateMenuBtnEl = document.getElementById('audioSampleRateMenuBtn');
  const audioSampleRateMenuEl = document.getElementById('audioSampleRateMenu');
  const audioSampleRateInputEl = document.getElementById('audioSampleRateInput');
  const audioSampleRateControlEl = document.getElementById('audioSampleRateControl');
  const audioOutputDeviceSelectEl = document.getElementById('audioOutputDeviceSelect');
  const rampModeSelectEl = document.getElementById('rampModeSelect');
  const exportLayoutBtnEl = document.getElementById('exportLayoutBtn');
  const importLayoutBtnEl = document.getElementById('importLayoutBtn');
  const editModeSelectEl = document.getElementById('editModeSelect');
  const speakerEditCartesianGizmoBtnEl = document.getElementById('speakerEditCartesianGizmoBtn');
  const speakerAddBtnEl = document.getElementById('speakerAddBtn');
  const speakerMoveUpBtnEl = document.getElementById('speakerMoveUpBtn');
  const speakerMoveDownBtnEl = document.getElementById('speakerMoveDownBtn');
  const speakerRemoveBtnEl = document.getElementById('speakerRemoveBtn');
  const speakerEditPolarGizmoBtnEl = document.getElementById('speakerEditPolarGizmoBtn');
  const speakerEditGainSliderEl = document.getElementById('speakerEditGainSlider');
  const speakerEditDelayMsInputEl = document.getElementById('speakerEditDelayMsInput');
  const speakerEditDelaySamplesInputEl = document.getElementById('speakerEditDelaySamplesInput');
  const speakerEditAutoDelayBtnEl = document.getElementById('speakerEditAutoDelayBtn');
  const speakerEditDelayToDistanceBtnEl = document.getElementById('speakerEditDelayToDistanceBtn');
  const speakerEditNameInputEl = document.getElementById('speakerEditNameInput');
  const speakerEditXInputEl = document.getElementById('speakerEditXInput');
  const speakerEditYInputEl = document.getElementById('speakerEditYInput');
  const speakerEditZInputEl = document.getElementById('speakerEditZInput');
  const speakerEditAzInputEl = document.getElementById('speakerEditAzInput');
  const speakerEditElInputEl = document.getElementById('speakerEditElInput');
  const speakerEditRInputEl = document.getElementById('speakerEditRInput');
  const speakerEditSpatializeToggleEl = document.getElementById('speakerEditSpatializeToggle');
  const speakerEditCartesianModeEl = document.getElementById('speakerEditCartesianMode');
  const speakerEditPolarModeEl = document.getElementById('speakerEditPolarMode');
  const trailToggleEl = document.getElementById('trailToggle');
  const effectiveRenderToggleEl = document.getElementById('effectiveRenderToggle');
  const trailModeSelectEl = document.getElementById('trailModeSelect');
  const trailTtlSliderEl = document.getElementById('trailTtlSlider');
  const trailTtlValEl = document.getElementById('trailTtlVal');
  const localeSelectEl = document.getElementById('localeSelect');
  const layoutSelectEl = document.getElementById('layoutSelect');
  const refreshOutputDevicesBtnEl = document.getElementById('refreshOutputDevicesBtn');

  // ── Master gain ─────────────────────────────────────────────────────────

  if (masterGainSliderEl) {
    masterGainSliderEl.addEventListener('input', () => {
      const value = Number(masterGainSliderEl.value);
      if (!Number.isFinite(value)) {
        return;
      }
      app.masterGain = value;
      updateMasterGainUI();
      invoke('control_master_gain', { gain: app.masterGain });
    });

    masterGainSliderEl.addEventListener('dblclick', () => {
      app.masterGain = 1;
      updateMasterGainUI();
      invoke('control_master_gain', { gain: app.masterGain });
    });
  }

  // ── Loudness ────────────────────────────────────────────────────────────

  if (loudnessToggleEl) {
    loudnessToggleEl.addEventListener('change', () => {
      const enabled = loudnessToggleEl.checked ? 1 : 0;
      app.loudnessEnabled = enabled === 1;
      updateLoudnessDisplay();
      invoke('control_loudness', { enable: enabled });
    });
  }

  // ── Adaptive resampling ─────────────────────────────────────────────────

  if (adaptiveResamplingToggleEl) {
    adaptiveResamplingToggleEl.addEventListener('change', () => {
      const enabled = adaptiveResamplingToggleEl.checked ? 1 : 0;
      app.adaptiveResamplingEnabled = enabled === 1;
      updateAdaptiveResamplingUI();
      invoke('control_adaptive_resampling', { enable: enabled });
    });
  }

  if (adaptiveFarModeToggleEl) {
    adaptiveFarModeToggleEl.addEventListener('change', () => {
      const enable = adaptiveFarModeToggleEl.checked ? 1 : 0;
      app.adaptiveResamplingEnableFarMode = enable === 1;
      updateAdaptiveResamplingUI();
      invoke('control_adaptive_resampling_enable_far_mode', { enable });
    });
  }

  if (adaptiveFarSilenceToggleEl) {
    adaptiveFarSilenceToggleEl.addEventListener('change', () => {
      const enable = adaptiveFarSilenceToggleEl.checked ? 1 : 0;
      app.adaptiveResamplingForceSilenceInFarMode = enable === 1;
      updateAdaptiveResamplingUI();
      invoke('control_adaptive_resampling_force_silence_in_far_mode', { enable });
    });
  }

  if (adaptiveFarHardRecoverToggleEl) {
    adaptiveFarHardRecoverToggleEl.addEventListener('change', () => {
      const enable = adaptiveFarHardRecoverToggleEl.checked ? 1 : 0;
      app.adaptiveResamplingHardRecoverInFarMode = enable === 1;
      updateAdaptiveResamplingUI();
      invoke('control_adaptive_resampling_hard_recover_in_far_mode', { enable });
    });
  }

  if (adaptiveFarFadeInMsInputEl) {
    adaptiveFarFadeInMsInputEl.addEventListener('focus', () => {
      app.adaptiveFarFadeInMsEditing = true;
      adaptiveFarFadeInMsInputEl.select();
    });
    adaptiveFarFadeInMsInputEl.addEventListener('input', () => {
      app.adaptiveFarFadeInMsEditing = true;
      app.adaptiveFarFadeInMsDirty = true;
      updateAdaptiveResamplingUI();
    });
  }

  if (adaptiveResamplingAdvancedApplyBtnEl) {
    adaptiveResamplingAdvancedApplyBtnEl.addEventListener('click', () => {
      if (adaptiveResamplingAdvancedApplyBtnEl.disabled) return;
      const kpNear = Math.max(0.00000001, Number(adaptiveKpNearInputEl?.value) || 0);
      const ki = Math.max(0.00000001, Number(adaptiveKiInputEl?.value) || 0);
      const maxAdjust = Math.max(0.000001, Number(adaptiveMaxAdjustInputEl?.value) || 0);
      const nearFarThresholdMs = Math.max(1, Math.round(Number(adaptiveNearFarThresholdInputEl?.value) || 0));
      const updateIntervalCallbacks = Math.max(1, Math.round(Number(adaptiveUpdateIntervalCallbacksInputEl?.value) || 0));
      const measurementSmoothingAlpha = Math.min(1, Math.max(0, Number(adaptiveMeasurementSmoothingAlphaInputEl?.value) || 0));
      const farModeReturnFadeInMs = Math.max(0, Math.round(Number(adaptiveFarFadeInMsInputEl?.value) || 0));

      app.adaptiveResamplingKpNear = kpNear;
      app.adaptiveResamplingKpFar = kpNear;
      app.adaptiveResamplingKi = ki;
      app.adaptiveResamplingMaxAdjust = maxAdjust;
      app.adaptiveResamplingMaxAdjustFar = maxAdjust;
      app.adaptiveResamplingNearFarThresholdMs = nearFarThresholdMs;
      app.adaptiveResamplingUpdateIntervalCallbacks = updateIntervalCallbacks;
      app.adaptiveResamplingMeasurementSmoothingAlpha = measurementSmoothingAlpha;
      app.adaptiveResamplingFarModeReturnFadeInMs = farModeReturnFadeInMs;
      updateAdaptiveResamplingUI();

      invoke('control_adaptive_resampling_kp_near', { value: kpNear });
      invoke('control_adaptive_resampling_ki', { value: ki });
      invoke('control_adaptive_resampling_max_adjust', { value: maxAdjust });
      invoke('control_adaptive_resampling_near_far_threshold_ms', { value: nearFarThresholdMs });
      invoke('control_adaptive_resampling_update_interval_callbacks', { value: updateIntervalCallbacks });
      invoke('control_adaptive_resampling_measurement_smoothing_alpha', { value: measurementSmoothingAlpha });
      invoke('control_adaptive_resampling_far_mode_return_fade_in_ms', { value: farModeReturnFadeInMs });

      resetAdaptiveResamplingAdvancedDirtyState();
      updateAdaptiveResamplingUI();
    });
  }

  if (adaptiveResamplingAdvancedCancelBtnEl) {
    adaptiveResamplingAdvancedCancelBtnEl.addEventListener('click', () => {
      if (adaptiveResamplingAdvancedCancelBtnEl.disabled) return;
      resetAdaptiveResamplingAdvancedDirtyState();
      updateAdaptiveResamplingUI();
    });
  }

  // ── Spread ──────────────────────────────────────────────────────────────

  if (spreadMinSliderEl) {
    spreadMinSliderEl.addEventListener('input', () => {
      const valueDeg = Number(spreadMinSliderEl.value);
      if (!Number.isFinite(valueDeg)) {
        return;
      }
      const valueNorm = Math.max(0, Math.min(180, valueDeg)) / 180.0;
      const maxValue = app.spreadState.max === null ? 1 : app.spreadState.max;
      app.spreadState.min = Math.min(valueNorm, maxValue);
      spreadMinSliderEl.value = String((app.spreadState.min ?? 0) * 180.0);
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateSpreadDisplay();
      invoke('control_spread_min', { value: app.spreadState.min });
    });
  }

  if (spreadMaxSliderEl) {
    spreadMaxSliderEl.addEventListener('input', () => {
      const valueDeg = Number(spreadMaxSliderEl.value);
      if (!Number.isFinite(valueDeg)) {
        return;
      }
      const valueNorm = Math.max(0, Math.min(180, valueDeg)) / 180.0;
      const minValue = app.spreadState.min === null ? 0 : app.spreadState.min;
      app.spreadState.max = Math.max(valueNorm, minValue);
      spreadMaxSliderEl.value = String((app.spreadState.max ?? 1) * 180.0);
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateSpreadDisplay();
      invoke('control_spread_max', { value: app.spreadState.max });
    });
  }

  if (spreadFromDistanceToggleEl) {
    spreadFromDistanceToggleEl.addEventListener('change', () => {
      const enabled = spreadFromDistanceToggleEl.checked;
      app.spreadState.fromDistance = enabled;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateSpreadDisplay();
      invoke('control_spread_from_distance', { enable: enabled ? 1 : 0 });
    });
  }

  if (spreadDistanceRangeSliderEl) {
    spreadDistanceRangeSliderEl.addEventListener('input', () => {
      const value = Number(spreadDistanceRangeSliderEl.value);
      if (!Number.isFinite(value)) return;
      app.spreadState.distanceRange = Math.max(0.01, value);
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateSpreadDisplay();
      invoke('control_spread_distance_range', { value: app.spreadState.distanceRange });
    });
  }

  if (spreadDistanceCurveSliderEl) {
    spreadDistanceCurveSliderEl.addEventListener('input', () => {
      const value = Number(spreadDistanceCurveSliderEl.value);
      if (!Number.isFinite(value)) return;
      app.spreadState.distanceCurve = Math.max(0, value);
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateSpreadDisplay();
      invoke('control_spread_distance_curve', { value: app.spreadState.distanceCurve });
    });
  }

  // ── Distance model ──────────────────────────────────────────────────────

  if (distanceModelSelectEl) {
    distanceModelSelectEl.addEventListener('change', () => {
      const value = String(distanceModelSelectEl.value || '').trim().toLowerCase();
      if (!['none', 'linear', 'quadratic', 'inverse-square'].includes(value)) {
        return;
      }
      app.distanceModel = value;
      updateDistanceModelUI();
      app.vbapRecomputing = true;
      renderVbapStatus();
      invoke('control_distance_model', { value });
    });
  }

  // ── VBAP cartesian ──────────────────────────────────────────────────────

  if (vbapCartXSizeInputEl) {
    vbapCartXSizeInputEl.addEventListener('change', () => {
      const value = Math.max(1, Math.round(Number(vbapCartXSizeInputEl.value) || 1));
      app.vbapCartesianState.xSize = value;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateVbapCartesian();
      invoke('control_vbap_cart_x_size', { value });
    });
  }

  if (vbapCartYSizeInputEl) {
    vbapCartYSizeInputEl.addEventListener('change', () => {
      const value = Math.max(1, Math.round(Number(vbapCartYSizeInputEl.value) || 1));
      app.vbapCartesianState.ySize = value;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateVbapCartesian();
      invoke('control_vbap_cart_y_size', { value });
    });
  }

  if (vbapCartZSizeInputEl) {
    vbapCartZSizeInputEl.addEventListener('change', () => {
      const value = Math.max(1, Math.round(Number(vbapCartZSizeInputEl.value) || 1));
      app.vbapCartesianState.zSize = value;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateVbapCartesian();
      invoke('control_vbap_cart_z_size', { value });
    });
  }

  if (vbapCartZNegSizeInputEl) {
    vbapCartZNegSizeInputEl.addEventListener('change', () => {
      const value = Math.max(0, Math.round(Number(vbapCartZNegSizeInputEl.value) || 0));
      app.vbapCartesianState.zNegSize = value;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateVbapCartesian();
      invoke('control_vbap_cart_z_neg_size', { value });
    });
  }

  if (vbapCartesianGridToggleBtnEl) {
    vbapCartesianGridToggleBtnEl.addEventListener('change', () => {
      app.vbapCartesianFaceGridEnabled = Boolean(vbapCartesianGridToggleBtnEl.checked);
      renderVbapCartesianGridToggle();
      updateVbapCartesianFaceGrid();
    });
  }

  // ── VBAP mode buttons ───────────────────────────────────────────────────

  [
    ['auto', vbapModeAutoBtnEl],
    ['polar', vbapModePolarBtnEl],
    ['cartesian', vbapModeCartesianBtnEl]
  ].forEach(([mode, button]) => {
    if (!button) return;
    button.addEventListener('click', () => {
      if (app.vbapModeState.selection === mode) return;
      app.vbapModeState.selection = mode;
      updateVbapMode();
      invoke('control_vbap_table_mode', { mode });
    });
  });

  // ── VBAP polar ──────────────────────────────────────────────────────────

  if (vbapPolarAzimuthResolutionInputEl) {
    vbapPolarAzimuthResolutionInputEl.addEventListener('change', () => {
      const value = Math.max(1, Math.round(Number(vbapPolarAzimuthResolutionInputEl.value) || 1));
      app.vbapPolarState.azimuthResolution = value;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateVbapPolar();
      invoke('control_vbap_polar_azimuth_resolution', { value });
    });
  }

  if (vbapPolarElevationResolutionInputEl) {
    vbapPolarElevationResolutionInputEl.addEventListener('change', () => {
      const value = Math.max(1, Math.round(Number(vbapPolarElevationResolutionInputEl.value) || 1));
      app.vbapPolarState.elevationResolution = value;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateVbapPolar();
      invoke('control_vbap_polar_elevation_resolution', { value });
    });
  }

  if (vbapPolarDistanceResInputEl) {
    vbapPolarDistanceResInputEl.addEventListener('change', () => {
      const value = Math.max(1, Math.round(Number(vbapPolarDistanceResInputEl.value) || 1));
      app.vbapPolarState.distanceRes = value;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateVbapPolar();
      invoke('control_vbap_polar_distance_res', { value });
    });
  }

  if (vbapPolarDistanceMaxInputEl) {
    vbapPolarDistanceMaxInputEl.addEventListener('change', () => {
      const value = Math.max(0.01, Number(vbapPolarDistanceMaxInputEl.value) || 2);
      app.vbapPolarState.distanceMax = value;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateVbapPolar();
      invoke('control_vbap_polar_distance_max', { value });
    });
  }

  // ── VBAP position interpolation ─────────────────────────────────────────

  if (vbapPositionInterpolationToggleEl) {
    vbapPositionInterpolationToggleEl.addEventListener('change', () => {
      const enabled = vbapPositionInterpolationToggleEl.checked;
      app.vbapPositionInterpolation = enabled;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateVbapPositionInterpolation();
      invoke('control_vbap_position_interpolation', { enable: enabled ? 1 : 0 });
    });
  }

  // ── Distance diffuse ────────────────────────────────────────────────────

  if (distanceDiffuseToggleEl) {
    distanceDiffuseToggleEl.addEventListener('change', () => {
      const enabled = distanceDiffuseToggleEl.checked;
      app.distanceDiffuseState.enabled = enabled;
      updateDistanceDiffuseUI();
      invoke('control_distance_diffuse_enabled', { enable: enabled ? 1 : 0 });
    });
  }

  if (distanceDiffuseThresholdSliderEl) {
    distanceDiffuseThresholdSliderEl.addEventListener('input', () => {
      const value = Number(distanceDiffuseThresholdSliderEl.value);
      if (!Number.isFinite(value)) return;
      app.distanceDiffuseState.threshold = value;
      if (distanceDiffuseThresholdValEl) distanceDiffuseThresholdValEl.textContent = formatNumber(value, 2);
      invoke('control_distance_diffuse_threshold', { value });
    });
  }

  if (distanceDiffuseCurveSliderEl) {
    distanceDiffuseCurveSliderEl.addEventListener('input', () => {
      const value = Number(distanceDiffuseCurveSliderEl.value);
      if (!Number.isFinite(value)) return;
      app.distanceDiffuseState.curve = value;
      if (distanceDiffuseCurveValEl) distanceDiffuseCurveValEl.textContent = formatNumber(value, 2);
      invoke('control_distance_diffuse_curve', { value });
    });
  }

  // ── Info modal open/close buttons ───────────────────────────────────────

  if (spreadFromDistanceInfoBtnEl) {
    spreadFromDistanceInfoBtnEl.addEventListener('click', () => {
      setSpreadFromDistanceInfoModalOpen(true);
    });
  }

  if (spreadFromDistanceInfoCloseBtnEl) {
    spreadFromDistanceInfoCloseBtnEl.addEventListener('click', () => {
      setSpreadFromDistanceInfoModalOpen(false);
    });
  }

  if (spreadFromDistanceInfoModalEl) {
    spreadFromDistanceInfoModalEl.addEventListener('click', (event) => {
      if (event.target === spreadFromDistanceInfoModalEl) {
        setSpreadFromDistanceInfoModalOpen(false);
      }
    });
  }

  if (distanceDiffuseInfoBtnEl) {
    distanceDiffuseInfoBtnEl.addEventListener('click', () => {
      setDistanceDiffuseInfoModalOpen(true);
    });
  }

  if (distanceDiffuseInfoCloseBtnEl) {
    distanceDiffuseInfoCloseBtnEl.addEventListener('click', () => {
      setDistanceDiffuseInfoModalOpen(false);
    });
  }

  if (distanceDiffuseInfoModalEl) {
    distanceDiffuseInfoModalEl.addEventListener('click', (event) => {
      if (event.target === distanceDiffuseInfoModalEl) {
        setDistanceDiffuseInfoModalOpen(false);
      }
    });
  }

  if (trailInfoBtnEl) {
    trailInfoBtnEl.addEventListener('click', () => {
      setTrailInfoModalOpen(true);
    });
  }

  if (effectiveRenderInfoBtnEl) {
    effectiveRenderInfoBtnEl.addEventListener('click', () => {
      setEffectiveRenderInfoModalOpen(true);
    });
  }

  if (trailInfoCloseBtnEl) {
    trailInfoCloseBtnEl.addEventListener('click', () => {
      setTrailInfoModalOpen(false);
    });
  }

  if (effectiveRenderInfoCloseBtnEl) {
    effectiveRenderInfoCloseBtnEl.addEventListener('click', () => {
      setEffectiveRenderInfoModalOpen(false);
    });
  }

  if (trailInfoModalEl) {
    trailInfoModalEl.addEventListener('click', (event) => {
      if (event.target === trailInfoModalEl) {
        setTrailInfoModalOpen(false);
      }
    });
  }

  if (effectiveRenderInfoModalEl) {
    effectiveRenderInfoModalEl.addEventListener('click', (event) => {
      if (event.target === effectiveRenderInfoModalEl) {
        setEffectiveRenderInfoModalOpen(false);
      }
    });
  }

  if (oscInfoBtnEl) {
    oscInfoBtnEl.addEventListener('click', () => {
      setOscInfoModalOpen(true);
    });
  }

  if (aboutBtnEl) {
    aboutBtnEl.addEventListener('click', () => {
      setAboutModalOpen(true);
    });
  }

  if (aboutCloseBtnEl) {
    aboutCloseBtnEl.addEventListener('click', () => {
      setAboutModalOpen(false);
    });
  }

  if (aboutModalEl) {
    aboutModalEl.addEventListener('click', (event) => {
      if (event.target === aboutModalEl) {
        setAboutModalOpen(false);
      }
    });
  }

  if (oscInfoCloseBtnEl) {
    oscInfoCloseBtnEl.addEventListener('click', () => {
      setOscInfoModalOpen(false);
    });
  }

  if (oscInfoModalEl) {
    oscInfoModalEl.addEventListener('click', (event) => {
      if (event.target === oscInfoModalEl) {
        setOscInfoModalOpen(false);
      }
    });
  }

  if (roomGeometryInfoBtnEl) {
    roomGeometryInfoBtnEl.addEventListener('click', () => {
      setRoomGeometryInfoModalOpen(true);
    });
  }

  if (roomGeometryInfoCloseBtnEl) {
    roomGeometryInfoCloseBtnEl.addEventListener('click', () => {
      setRoomGeometryInfoModalOpen(false);
    });
  }

  if (roomGeometryInfoModalEl) {
    roomGeometryInfoModalEl.addEventListener('click', (event) => {
      if (event.target === roomGeometryInfoModalEl) {
        setRoomGeometryInfoModalOpen(false);
      }
    });
  }

  if (adaptiveResamplingInfoBtnEl) {
    adaptiveResamplingInfoBtnEl.addEventListener('click', () => {
      setAdaptiveResamplingInfoModalOpen(true);
    });
  }

  if (adaptiveResamplingInfoCloseBtnEl) {
    adaptiveResamplingInfoCloseBtnEl.addEventListener('click', () => {
      setAdaptiveResamplingInfoModalOpen(false);
    });
  }

  if (adaptiveResamplingInfoModalEl) {
    adaptiveResamplingInfoModalEl.addEventListener('click', (event) => {
      if (event.target === adaptiveResamplingInfoModalEl) {
        setAdaptiveResamplingInfoModalOpen(false);
      }
    });
  }

  if (telemetryGaugesInfoBtnEl) {
    telemetryGaugesInfoBtnEl.addEventListener('click', () => {
      setTelemetryGaugesInfoModalOpen(true);
    });
  }

  if (telemetryGaugesInfoCloseBtnEl) {
    telemetryGaugesInfoCloseBtnEl.addEventListener('click', () => {
      setTelemetryGaugesInfoModalOpen(false);
    });
  }

  if (telemetryGaugesInfoModalEl) {
    telemetryGaugesInfoModalEl.addEventListener('click', (event) => {
      if (event.target === telemetryGaugesInfoModalEl) {
        setTelemetryGaugesInfoModalOpen(false);
      }
    });
  }

  if (rampModeInfoBtnEl) {
    rampModeInfoBtnEl.addEventListener('click', () => {
      setRampModeInfoModalOpen(true);
    });
  }

  if (rampModeInfoCloseBtnEl) {
    rampModeInfoCloseBtnEl.addEventListener('click', () => {
      setRampModeInfoModalOpen(false);
    });
  }

  if (rampModeInfoModalEl) {
    rampModeInfoModalEl.addEventListener('click', (event) => {
      if (event.target === rampModeInfoModalEl) {
        setRampModeInfoModalOpen(false);
      }
    });
  }

  if (vbapPositionInterpolationInfoBtnEl) {
    vbapPositionInterpolationInfoBtnEl.addEventListener('click', () => {
      setVbapPositionInterpolationInfoModalOpen(true);
    });
  }

  if (vbapPositionInterpolationInfoCloseBtnEl) {
    vbapPositionInterpolationInfoCloseBtnEl.addEventListener('click', () => {
      setVbapPositionInterpolationInfoModalOpen(false);
    });
  }

  if (vbapPositionInterpolationInfoModalEl) {
    vbapPositionInterpolationInfoModalEl.addEventListener('click', (event) => {
      if (event.target === vbapPositionInterpolationInfoModalEl) {
        setVbapPositionInterpolationInfoModalOpen(false);
      }
    });
  }

  // ── Collapsible section toggles ─────────────────────────────────────────

  if (adaptiveResamplingAdvancedToggleBtnEl) {
    adaptiveResamplingAdvancedToggleBtnEl.addEventListener('click', () => {
      setAdaptiveResamplingAdvancedOpen(!app.adaptiveResamplingAdvancedOpen);
    });
  }

  if (telemetryGaugesToggleBtnEl) {
    telemetryGaugesToggleBtnEl.addEventListener('click', () => {
      setTelemetryGaugesOpen(!app.telemetryGaugesOpen);
    });
  }

  if (displaySectionToggleBtnEl) {
    displaySectionToggleBtnEl.addEventListener('click', () => {
      setDisplaySectionOpen(!app.displaySectionOpen);
    });
  }

  if (audioOutputSectionToggleBtnEl) {
    audioOutputSectionToggleBtnEl.addEventListener('click', () => {
      setAudioOutputSectionOpen(!app.audioOutputSectionOpen);
    });
  }

  if (rendererSectionToggleBtnEl) {
    rendererSectionToggleBtnEl.addEventListener('click', () => {
      setRendererSectionOpen(!app.rendererSectionOpen);
    });
  }

  // ── Keyboard escape ─────────────────────────────────────────────────────

  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') {
      setDistanceDiffuseInfoModalOpen(false);
      setAdaptiveResamplingInfoModalOpen(false);
      setTrailInfoModalOpen(false);
      setOscInfoModalOpen(false);
      setRoomGeometryInfoModalOpen(false);
      setTelemetryGaugesInfoModalOpen(false);
      setRampModeInfoModalOpen(false);
    }
  });

  // ── Room geometry ───────────────────────────────────────────────────────

  if (roomGeometryCancelBtnEl) {
    roomGeometryCancelBtnEl.addEventListener('click', () => {
      if (roomGeometryCancelBtnEl.disabled || !app.roomGeometryBaselineKey) return;
      if (app.roomGeometryApplyTimer !== null) {
        clearTimeout(app.roomGeometryApplyTimer);
        app.roomGeometryApplyTimer = null;
      }
      try {
        const baseline = JSON.parse(app.roomGeometryBaselineKey);
        applyRoomGeometryStateToInputs(baseline);
      } catch (_e) {
        // Ignore invalid baseline payload.
      }
    });
  }

  roomMasterAxisInputs.forEach((input) => {
    input.addEventListener('change', () => {
      if (!input.checked) return;
      app.roomMasterAxis = input.value;
      refreshRoomGeometryInputState();
      persistRoomGeometryPrefs();
      updateRoomGeometryLivePreview();
      updateRoomGeometryButtonsState();
      applyRoomGeometryNow();
    });
  });

  [
    ['width', roomDriverWidthEl],
    ['length', roomDriverLengthEl],
    ['height', roomDriverHeightEl],
    ['rear', roomDriverRearEl],
    ['lower', roomDriverLowerEl]
  ].forEach(([axis, el]) => {
    if (!el) return;
    el.addEventListener('change', () => {
      app.roomAxisDrivers[axis] = getRoomDriverValue(axis);
      refreshRoomGeometryInputState();
      persistRoomGeometryPrefs();
      updateRoomGeometryLivePreview();
      updateRoomGeometryButtonsState();
      applyRoomGeometryNow();
    });
  });

  [
    roomDimWidthInputEl,
    roomDimLengthInputEl,
    roomDimHeightInputEl,
    roomDimRearInputEl,
    roomDimLowerInputEl,
    roomRatioWidthInputEl,
    roomRatioLengthInputEl,
    roomRatioHeightInputEl,
    roomRatioRearInputEl,
    roomRatioLowerInputEl
  ].forEach((el) => {
    if (!el) return;
    el.addEventListener('input', () => {
      updateRoomGeometryLivePreview();
      updateRoomGeometryButtonsState();
      scheduleRoomGeometryApply();
    });
    el.addEventListener('change', () => {
      normalizeRoomGeometryInputDisplays();
      updateRoomGeometryLivePreview();
      updateRoomGeometryButtonsState();
      applyRoomGeometryNow();
    });
  });

  if (roomRatioCenterBlendSliderEl) {
    roomRatioCenterBlendSliderEl.addEventListener('input', () => {
      renderRoomCenterBlendControl(getRoomCenterBlendFromInput());
      updateRoomGeometryLivePreview();
      updateRoomGeometryButtonsState();
      scheduleRoomGeometryApply();
    });
    roomRatioCenterBlendSliderEl.addEventListener('change', () => {
      renderRoomCenterBlendControl(getRoomCenterBlendFromInput());
      updateRoomGeometryLivePreview();
      updateRoomGeometryButtonsState();
      applyRoomGeometryNow();
    });
    roomRatioCenterBlendSliderEl.addEventListener('dblclick', () => {
      renderRoomCenterBlendControl(0.5);
      updateRoomGeometryLivePreview();
      updateRoomGeometryButtonsState();
      applyRoomGeometryNow();
    });
  }

  if (roomRatioCenterBlendValueEl) {
    roomRatioCenterBlendValueEl.addEventListener('dblclick', () => {
      renderRoomCenterBlendControl(0.5);
      updateRoomGeometryLivePreview();
      updateRoomGeometryButtonsState();
      applyRoomGeometryNow();
    });
  }

  // ── Save / reload config ────────────────────────────────────────────────

  if (saveConfigBtnEl) {
    saveConfigBtnEl.addEventListener('click', () => {
      pushLog('info', t('log.saveRequested'));
      invoke('control_save_config');
    });
  }

  if (reloadConfigBtnEl) {
    reloadConfigBtnEl.addEventListener('click', () => {
      pushLog('info', t('log.reloadRequested'));
      invoke('control_reload_config');
    });
  }

  // ── Log panel ───────────────────────────────────────────────────────────

  if (logToggleBtnEl) {
    logToggleBtnEl.addEventListener('click', () => {
      setLogExpanded(!logState.expanded);
    });
  }

  if (logClearBtnEl) {
    logClearBtnEl.addEventListener('click', () => {
      logState.entries = [];
      renderLogPanel();
    });
  }

  if (logCopyBtnEl) {
    logCopyBtnEl.addEventListener('click', () => {
      copyLogsToClipboard();
    });
  }

  if (logLevelSelectEl) {
    logLevelSelectEl.addEventListener('change', () => {
      const value = normalizeLogLevel(logLevelSelectEl.value);
      logState.backendLogLevel = value;
      renderLogLevelControl();
      pushLog('info', tf('log.levelChanged', { value }));
      invoke('control_log_level', { value }).catch((e) => {
        pushLog('error', tf('log.oscConfigFailed', { error: normalizeLogError(e) }));
      });
    });
  }

  // ── Latency target ──────────────────────────────────────────────────────

  const latencyTargetApplyBtnEl = document.getElementById('latencyTargetApplyBtn');
  if (latencyTargetInputEl) {
    latencyTargetInputEl.addEventListener('focus', () => {
      app.latencyTargetEditing = true;
      latencyTargetInputEl.select();
    });
    latencyTargetInputEl.addEventListener('input', () => {
      app.latencyTargetEditing = true;
      app.latencyTargetDirty = true;
      updateLatencyDisplay();
    });
    latencyTargetInputEl.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') applyLatencyTargetNow();
    });
  }
  if (latencyTargetApplyBtnEl) {
    latencyTargetApplyBtnEl.addEventListener('click', () => {
      applyLatencyTargetNow();
    });
  }

  // ── Adaptive resampling advanced inputs ─────────────────────────────────

  if (adaptiveKpNearInputEl) {
    adaptiveKpNearInputEl.addEventListener('focus', () => {
      app.adaptiveKpNearEditing = true;
      adaptiveKpNearInputEl.select();
    });
    adaptiveKpNearInputEl.addEventListener('input', () => {
      app.adaptiveKpNearEditing = true;
      app.adaptiveKpNearDirty = true;
      updateAdaptiveResamplingUI();
    });
  }

  if (adaptiveKpFarInputEl) {
    adaptiveKpFarInputEl.addEventListener('focus', () => {
      app.adaptiveKpFarEditing = true;
      adaptiveKpFarInputEl.select();
    });
    adaptiveKpFarInputEl.addEventListener('input', () => {
      app.adaptiveKpFarEditing = true;
      app.adaptiveKpFarDirty = true;
      updateAdaptiveResamplingUI();
    });
  }

  if (adaptiveKiInputEl) {
    adaptiveKiInputEl.addEventListener('focus', () => {
      app.adaptiveKiEditing = true;
      adaptiveKiInputEl.select();
    });
    adaptiveKiInputEl.addEventListener('input', () => {
      app.adaptiveKiEditing = true;
      app.adaptiveKiDirty = true;
      updateAdaptiveResamplingUI();
    });
  }

  if (adaptiveMaxAdjustInputEl) {
    adaptiveMaxAdjustInputEl.addEventListener('focus', () => {
      app.adaptiveMaxAdjustEditing = true;
      adaptiveMaxAdjustInputEl.select();
    });
    adaptiveMaxAdjustInputEl.addEventListener('input', () => {
      app.adaptiveMaxAdjustEditing = true;
      app.adaptiveMaxAdjustDirty = true;
      updateAdaptiveResamplingUI();
    });
  }

  if (adaptiveMaxAdjustFarInputEl) {
    adaptiveMaxAdjustFarInputEl.addEventListener('focus', () => {
      app.adaptiveMaxAdjustFarEditing = true;
      adaptiveMaxAdjustFarInputEl.select();
    });
    adaptiveMaxAdjustFarInputEl.addEventListener('input', () => {
      app.adaptiveMaxAdjustFarEditing = true;
      app.adaptiveMaxAdjustFarDirty = true;
      updateAdaptiveResamplingUI();
    });
  }

  if (adaptiveNearFarThresholdInputEl) {
    adaptiveNearFarThresholdInputEl.addEventListener('focus', () => {
      app.adaptiveNearFarThresholdEditing = true;
      adaptiveNearFarThresholdInputEl.select();
    });
    adaptiveNearFarThresholdInputEl.addEventListener('input', () => {
      app.adaptiveNearFarThresholdEditing = true;
      app.adaptiveNearFarThresholdDirty = true;
      updateAdaptiveResamplingUI();
    });
  }

  if (adaptiveUpdateIntervalCallbacksInputEl) {
    adaptiveUpdateIntervalCallbacksInputEl.addEventListener('focus', () => {
      app.adaptiveUpdateIntervalCallbacksEditing = true;
      adaptiveUpdateIntervalCallbacksInputEl.select();
    });
    adaptiveUpdateIntervalCallbacksInputEl.addEventListener('input', () => {
      app.adaptiveUpdateIntervalCallbacksEditing = true;
      app.adaptiveUpdateIntervalCallbacksDirty = true;
      updateAdaptiveResamplingUI();
    });
  }

  if (adaptiveMeasurementSmoothingAlphaInputEl) {
    adaptiveMeasurementSmoothingAlphaInputEl.addEventListener('focus', () => {
      app.adaptiveMeasurementSmoothingAlphaEditing = true;
      adaptiveMeasurementSmoothingAlphaInputEl.select();
    });
    adaptiveMeasurementSmoothingAlphaInputEl.addEventListener('input', () => {
      app.adaptiveMeasurementSmoothingAlphaEditing = true;
      app.adaptiveMeasurementSmoothingAlphaDirty = true;
      updateAdaptiveResamplingUI();
    });
  }

  // ── Audio sample rate menu ──────────────────────────────────────────────

  if (audioSampleRateMenuBtnEl) {
    audioSampleRateMenuBtnEl.addEventListener('click', (event) => {
      event.stopPropagation();
      if (!audioSampleRateMenuEl) return;
      if (audioSampleRateMenuEl.style.display === 'block') {
        closeAudioSampleRateMenu();
      } else {
        openAudioSampleRateMenu();
      }
    });
  }

  if (audioSampleRateInputEl) {
    audioSampleRateInputEl.addEventListener('focus', () => {
      app.audioSampleRateEditing = true;
      audioSampleRateInputEl.select();
    });
    audioSampleRateInputEl.addEventListener('change', () => {
      applyAudioSampleRateNow();
    });
  }

  // ── Audio output device ─────────────────────────────────────────────────

  if (audioOutputDeviceSelectEl) {
    audioOutputDeviceSelectEl.addEventListener('focus', () => {
      app.audioOutputDeviceEditing = true;
    });
    audioOutputDeviceSelectEl.addEventListener('change', () => {
      app.audioOutputDeviceEditing = true;
      applyAudioOutputDeviceNow();
    });
  }

  if (refreshOutputDevicesBtnEl) {
    refreshOutputDevicesBtnEl.addEventListener('click', () => {
      invoke('refresh_output_devices');
    });
  }

  // ── Ramp mode ───────────────────────────────────────────────────────────

  if (rampModeSelectEl) {
    rampModeSelectEl.addEventListener('change', () => {
      applyRampModeNow();
    });
  }

  // ── Document-level pointerdown handlers ─────────────────────────────────

  document.addEventListener('pointerdown', (event) => {
    if (!audioSampleRateMenuEl || audioSampleRateMenuEl.style.display !== 'block') return;
    const target = event.target;
    if (!(target instanceof Node)) return;
    if (audioSampleRateMenuEl.contains(target) || audioSampleRateMenuBtnEl?.contains(target)) return;
    closeAudioSampleRateMenu();
  });

  document.addEventListener('pointerdown', (event) => {
    if (!audioSampleRateControlEl) return;
    const target = event.target;
    if (!(target instanceof Node)) return;
    if (!audioSampleRateControlEl.contains(target)) {
      app.audioSampleRateEditing = false;
    }
  });

  document.addEventListener('pointerdown', (event) => {
    if (!audioOutputDeviceSelectEl) return;
    const target = event.target;
    if (!(target instanceof Node)) return;
    if (target !== audioOutputDeviceSelectEl) {
      app.audioOutputDeviceEditing = false;
    }
  });

  document.addEventListener('pointerdown', (event) => {
    if (!latencyTargetInputEl) return;
    const target = event.target;
    if (!(target instanceof Node)) return;
    if (target !== latencyTargetInputEl) {
      app.latencyTargetEditing = false;
    }
  });

  // ── Layout export / import ──────────────────────────────────────────────

  if (exportLayoutBtnEl) {
    exportLayoutBtnEl.addEventListener('click', () => {
      const fallbackName = sanitizeLayoutExportName(defaultLayoutExportNameFromSpeakers(app.currentLayoutSpeakers));
      invoke('pick_export_layout_path', { suggestedName: fallbackName })
        .then((path) => {
          const trimmed = typeof path === 'string' ? path.trim() : '';
          if (!trimmed) return;
          const layout = serializeCurrentLayoutForExport();
          if (!layout) return;
          return invoke('export_layout_to_path', { path: trimmed, layout })
            .then(() => {
              pushLog('info', tf('log.layoutExported', { path: trimmed }));
            });
        })
        .catch((e) => {
          console.error('[layout export]', e);
          pushLog('error', tf('log.layoutExportFailed', { error: normalizeLogError(e) }));
        });
    });
  }

  if (importLayoutBtnEl) {
    importLayoutBtnEl.addEventListener('click', () => {
      invoke('pick_import_layout_path')
        .then((path) => {
          const trimmed = typeof path === 'string' ? path.trim() : '';
          if (!trimmed) return;
          pushLog('info', tf('log.layoutImportRequested', { path: trimmed }));
          return invoke('import_layout_from_path', { path: trimmed })
            .then((payload) => {
              hydrateLayoutSelect(payload.layouts || [], payload.selectedLayoutKey);
              app.configSaved = false;
              updateConfigSavedUI();
              refreshOverlayLists();
              renderSpeakerEditor();
              pushLog('info', tf('log.layoutImported', { path: trimmed }));
            });
        })
        .catch((e) => {
          console.error('[layout import]', e);
          pushLog('error', tf('log.layoutImportFailed', { error: normalizeLogError(e) }));
        });
    });
  }

  // ── Speaker editor ──────────────────────────────────────────────────────

  if (editModeSelectEl) {
    editModeSelectEl.addEventListener('change', () => {
      app.activeEditMode = editModeSelectEl.value;
      updateSpeakerGizmo();
      updateControlsForEditMode();
    });
  }

  if (speakerEditCartesianGizmoBtnEl) {
    speakerEditCartesianGizmoBtnEl.addEventListener('click', () => {
      if (app.selectedSpeakerIndex === null) return;
      app.activeEditMode = 'cartesian';
      if (editModeSelectEl) editModeSelectEl.value = 'cartesian';
      app.cartesianEditArmed = !app.cartesianEditArmed;
      if (app.cartesianEditArmed) {
        app.polarEditArmed = false;
      }
      renderSpeakerEditor();
      updateSpeakerGizmo();
    });
  }

  if (speakerAddBtnEl) {
    speakerAddBtnEl.addEventListener('click', () => {
      requestAddSpeaker();
    });
  }

  if (speakerMoveUpBtnEl) {
    speakerMoveUpBtnEl.addEventListener('click', () => {
      requestMoveSpeaker(-1);
    });
  }

  if (speakerMoveDownBtnEl) {
    speakerMoveDownBtnEl.addEventListener('click', () => {
      requestMoveSpeaker(1);
    });
  }

  if (speakerRemoveBtnEl) {
    speakerRemoveBtnEl.addEventListener('click', () => {
      requestRemoveSpeaker();
    });
  }

  if (speakerEditPolarGizmoBtnEl) {
    speakerEditPolarGizmoBtnEl.addEventListener('click', () => {
      if (app.selectedSpeakerIndex === null) return;
      app.activeEditMode = 'polar';
      if (editModeSelectEl) editModeSelectEl.value = 'polar';
      app.polarEditArmed = !app.polarEditArmed;
      if (app.polarEditArmed) {
        app.cartesianEditArmed = false;
      }
      renderSpeakerEditor();
      updateSpeakerGizmo();
    });
  }

  if (speakerEditGainSliderEl) {
    speakerEditGainSliderEl.addEventListener('input', () => {
      if (app.selectedSpeakerIndex === null) return;
      const id = String(app.selectedSpeakerIndex);
      const value = Number(speakerEditGainSliderEl.value);
      if (!Number.isFinite(value)) return;
      speakerBaseGains.set(id, value);
      applyGroupGains('speaker');
      renderSpeakerEditor();
    });
    speakerEditGainSliderEl.addEventListener('dblclick', () => {
      if (app.selectedSpeakerIndex === null) return;
      speakerEditGainSliderEl.value = '1';
      const id = String(app.selectedSpeakerIndex);
      speakerBaseGains.set(id, 1);
      applyGroupGains('speaker');
      renderSpeakerEditor();
    });
  }

  if (speakerEditDelayMsInputEl) {
    speakerEditDelayMsInputEl.addEventListener('change', () => {
      if (app.selectedSpeakerIndex === null) return;
      const id = String(app.selectedSpeakerIndex);
      const value = Math.max(0, Number(speakerEditDelayMsInputEl.value) || 0);
      speakerDelays.set(id, value);
      speakerEditDelayMsInputEl.value = String(value);
      invoke('control_speaker_delay', { id: Number(id), delayMs: value });
      renderSpeakerEditor();
    });
  }

  if (speakerEditDelaySamplesInputEl) {
    speakerEditDelaySamplesInputEl.addEventListener('change', () => {
      if (app.selectedSpeakerIndex === null) return;
      const id = String(app.selectedSpeakerIndex);
      const samples = Math.max(0, Math.round(Number(speakerEditDelaySamplesInputEl.value) || 0));
      const delayMs = samplesToDelayMs(samples);
      speakerDelays.set(id, delayMs);
      invoke('control_speaker_delay', { id: Number(id), delayMs });
      renderSpeakerEditor();
    });
  }

  if (speakerEditAutoDelayBtnEl) {
    speakerEditAutoDelayBtnEl.addEventListener('click', () => {
      computeAndApplySpeakerDelays();
    });
  }

  if (speakerEditDelayToDistanceBtnEl) {
    speakerEditDelayToDistanceBtnEl.addEventListener('click', () => {
      adjustSpeakerDistancesFromDelays();
    });
  }

  if (speakerEditNameInputEl) {
    speakerEditNameInputEl.addEventListener('change', () => {
      if (app.selectedSpeakerIndex === null) return;
      const speaker = app.currentLayoutSpeakers[app.selectedSpeakerIndex];
      if (!speaker) return;
      const nextName = speakerEditNameInputEl.value.trim() || `spk-${app.selectedSpeakerIndex}`;
      speaker.id = nextName;
      invoke('control_speaker_name', { id: app.selectedSpeakerIndex, name: nextName });
      invoke('control_speakers_apply');
      updateSpeakerVisualsFromState(app.selectedSpeakerIndex);
      renderSpeakerEditor();
    });
  }

  // ── Speaker coordinate change helpers ───────────────────────────────────

  function bindSpeakerCoordChange(inputEl, getter) {
    if (!inputEl) return;
    inputEl.addEventListener('change', () => {
      if (app.selectedSpeakerIndex === null) return;
      getter(app.selectedSpeakerIndex);
    });
  }

  bindSpeakerCoordChange(speakerEditXInputEl, (idx) => {
    const gx = Number(speakerEditXInputEl?.value);
    const gy = Number(speakerEditYInputEl?.value);
    const gz = Number(speakerEditZInputEl?.value);
    applySpeakerCartesianEdit(idx, gx, gy, gz, true);
  });

  bindSpeakerCoordChange(speakerEditYInputEl, (idx) => {
    const gx = Number(speakerEditXInputEl?.value);
    const gy = Number(speakerEditYInputEl?.value);
    const gz = Number(speakerEditZInputEl?.value);
    applySpeakerCartesianEdit(idx, gx, gy, gz, true);
  });

  bindSpeakerCoordChange(speakerEditZInputEl, (idx) => {
    const gx = Number(speakerEditXInputEl?.value);
    const gy = Number(speakerEditYInputEl?.value);
    const gz = Number(speakerEditZInputEl?.value);
    applySpeakerCartesianEdit(idx, gx, gy, gz, true);
  });

  bindSpeakerCoordChange(speakerEditAzInputEl, (idx) => {
    const az = Number(speakerEditAzInputEl?.value);
    const el = Number(speakerEditElInputEl?.value);
    const r = Number(speakerEditRInputEl?.value);
    applySpeakerPolarEdit(idx, az, el, r, true);
  });

  bindSpeakerCoordChange(speakerEditElInputEl, (idx) => {
    const az = Number(speakerEditAzInputEl?.value);
    const el = Number(speakerEditElInputEl?.value);
    const r = Number(speakerEditRInputEl?.value);
    applySpeakerPolarEdit(idx, az, el, r, true);
  });

  bindSpeakerCoordChange(speakerEditRInputEl, (idx) => {
    const az = Number(speakerEditAzInputEl?.value);
    const el = Number(speakerEditElInputEl?.value);
    const r = Number(speakerEditRInputEl?.value);
    applySpeakerPolarEdit(idx, az, el, r, true);
  });

  // ── Speaker spatialize / coord mode ─────────────────────────────────────

  if (speakerEditSpatializeToggleEl) {
    speakerEditSpatializeToggleEl.addEventListener('change', () => {
      if (app.selectedSpeakerIndex === null) return;
      const index = app.selectedSpeakerIndex;
      const nextSpatialize = speakerEditSpatializeToggleEl.checked ? 1 : 0;
      setSpeakerSpatializeLocal(index, nextSpatialize);
      invoke('control_speaker_spatialize', { id: index, spatialize: nextSpatialize });
      invoke('control_speakers_apply');
      renderSpeakerEditor();
    });
  }

  if (speakerEditCartesianModeEl) {
    speakerEditCartesianModeEl.addEventListener('change', () => {
      if (app.selectedSpeakerIndex === null || !speakerEditCartesianModeEl.checked) return;
      setSpeakerCoordMode(app.selectedSpeakerIndex, 'cartesian');
    });
  }

  if (speakerEditPolarModeEl) {
    speakerEditPolarModeEl.addEventListener('change', () => {
      if (app.selectedSpeakerIndex === null || !speakerEditPolarModeEl.checked) return;
      setSpeakerCoordMode(app.selectedSpeakerIndex, 'polar');
    });
  }

  // ── Trails & effective render ───────────────────────────────────────────

  if (trailToggleEl) {
    trailToggleEl.addEventListener('change', () => {
      app.trailsEnabled = trailToggleEl.checked;
      sourceTrails.forEach((trail, id) => {
        trail.line.visible = app.trailsEnabled;
        if (app.trailsEnabled) {
          rebuildTrailGeometry(id);
        }
      });
      persistTrailPrefs();
    });
  }

  if (effectiveRenderToggleEl) {
    effectiveRenderToggleEl.addEventListener('change', () => {
      app.effectiveRenderEnabled = effectiveRenderToggleEl.checked;
      refreshEffectiveRenderVisibility();
      persistEffectiveRenderPrefs();
    });
  }

  if (trailModeSelectEl) {
    trailModeSelectEl.value = app.trailRenderMode;
    trailModeSelectEl.addEventListener('change', () => {
      app.trailRenderMode = trailModeSelectEl.value === 'line' ? 'line' : 'diffuse';
      sourceTrails.forEach((trail, id) => {
        const wasVisible = trail.line.visible;
        scene.remove(trail.line);
        trail.line.geometry.dispose();
        trail.line.material.dispose();
        trail.line = createTrailRenderable();
        trail.line.visible = wasVisible;
        scene.add(trail.line);
        if (app.trailsEnabled) {
          rebuildTrailGeometry(id);
        }
      });
      persistTrailPrefs();
    });
  }

  if (trailTtlSliderEl) {
    trailTtlSliderEl.addEventListener('input', () => {
      const seconds = Number(trailTtlSliderEl.value);
      app.trailPointTtlMs = Math.max(500, seconds * 1000);
      if (trailTtlValEl) trailTtlValEl.textContent = `${seconds.toFixed(1)}s`;
      persistTrailPrefs();
    });
  }

  // ── Locale select ───────────────────────────────────────────────────────

  if (localeSelectEl) {
    localeSelectEl.addEventListener('change', () => {
      setLocale(localeSelectEl.value || 'auto');
    });
  }

  // ── Layout select ───────────────────────────────────────────────────────

  if (layoutSelectEl) {
    layoutSelectEl.addEventListener('change', () => {
      invoke('select_layout', { key: layoutSelectEl.value });
    });
  }

  // ── Boot-time calls ─────────────────────────────────────────────────────

  applyStaticTranslations();
  setOscStatus('initializing');
  pushLog('info', t('log.boot'));
}
