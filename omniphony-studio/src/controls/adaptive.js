/**
 * Adaptive resampling controls.
 *
 * Extracted from app.js (lines 3899-4040).
 */

import { app, dirty } from '../state.js';
import { formatNumber } from '../coordinates.js';
import { scheduleUIFlush } from '../flush.js';

// DOM refs
const adaptiveResamplingToggleEl = document.getElementById('adaptiveResamplingToggle');
const adaptiveFarModeToggleEl = document.getElementById('adaptiveFarModeToggle');
const adaptiveFarSilenceToggleEl = document.getElementById('adaptiveFarSilenceToggle');
const adaptiveFarSilenceRowEl = document.getElementById('adaptiveFarSilenceRow');
const adaptiveFarHardRecoverToggleEl = document.getElementById('adaptiveFarHardRecoverToggle');
const adaptiveFarHardRecoverRowEl = document.getElementById('adaptiveFarHardRecoverRow');
const adaptiveFarFadeRowEl = document.getElementById('adaptiveFarFadeRow');
const adaptiveFarFadeInMsInputEl = document.getElementById('adaptiveFarFadeInMsInput');
const adaptiveKpNearInputEl = document.getElementById('adaptiveKpNearInput');
const adaptiveKiInputEl = document.getElementById('adaptiveKiInput');
const adaptiveIntegralDischargeRatioInputEl = document.getElementById('adaptiveIntegralDischargeRatioInput');
const adaptiveMaxAdjustInputEl = document.getElementById('adaptiveMaxAdjustInput');
const adaptiveNearFarThresholdRowEl = document.getElementById('adaptiveNearFarThresholdRow');
const adaptiveNearFarThresholdSymbolEl = document.getElementById('adaptiveNearFarThresholdSymbol');
const adaptiveNearFarThresholdInputEl = document.getElementById('adaptiveNearFarThresholdInput');
const adaptiveUpdateIntervalCallbacksInputEl = document.getElementById('adaptiveUpdateIntervalCallbacksInput');
const adaptiveResamplingAdvancedApplyBtnEl = document.getElementById('adaptiveResamplingAdvancedApplyBtn');
const adaptiveResamplingAdvancedCancelBtnEl = document.getElementById('adaptiveResamplingAdvancedCancelBtn');
const adaptiveBandDotEl = document.getElementById('adaptiveBandDot');
const adaptiveBandTextEl = document.getElementById('adaptiveBandText');
const adaptivePauseBtnEl = document.getElementById('adaptivePauseBtn');
const adaptiveRatioResetBtnEl = document.getElementById('adaptiveRatioResetBtn');

export function renderAdaptiveResamplingUI() {
  if (!adaptiveResamplingToggleEl) return;
  const farModeEnabled = app.adaptiveResamplingEnableFarMode === true;
  adaptiveResamplingToggleEl.checked = app.adaptiveResamplingEnabled === true;
  if (adaptiveFarModeToggleEl) {
    adaptiveFarModeToggleEl.checked = app.adaptiveResamplingEnableFarMode === true;
  }
  if (adaptiveFarSilenceToggleEl) {
    adaptiveFarSilenceToggleEl.checked = app.adaptiveResamplingForceSilenceInFarMode === true;
    adaptiveFarSilenceToggleEl.disabled = !farModeEnabled;
  }
  if (adaptiveFarSilenceRowEl) {
    adaptiveFarSilenceRowEl.classList.toggle('adaptive-param-disabled', !farModeEnabled);
  }
  const farSilenceEnabled = farModeEnabled && app.adaptiveResamplingForceSilenceInFarMode === true;
  if (adaptiveFarHardRecoverToggleEl) {
    adaptiveFarHardRecoverToggleEl.checked = app.adaptiveResamplingHardRecoverInFarMode === true;
    adaptiveFarHardRecoverToggleEl.disabled = !farSilenceEnabled;
  }
  if (adaptiveFarHardRecoverRowEl) {
    adaptiveFarHardRecoverRowEl.classList.toggle('adaptive-param-disabled', !farSilenceEnabled);
  }
  if (adaptiveFarFadeRowEl) {
    adaptiveFarFadeRowEl.classList.toggle('adaptive-param-disabled', !farSilenceEnabled);
  }
  if (adaptiveFarFadeInMsInputEl) {
    adaptiveFarFadeInMsInputEl.disabled = !farSilenceEnabled;
  }
  if (adaptiveNearFarThresholdInputEl) {
    adaptiveNearFarThresholdInputEl.disabled = !farModeEnabled;
  }
  if (adaptiveNearFarThresholdRowEl) {
    adaptiveNearFarThresholdRowEl.classList.toggle('adaptive-param-disabled', !farModeEnabled);
  }
  if (adaptiveNearFarThresholdSymbolEl) {
    adaptiveNearFarThresholdSymbolEl.style.opacity = farModeEnabled ? '1' : '0.42';
  }
  if (adaptiveFarFadeInMsInputEl && !app.adaptiveFarFadeInMsEditing && !app.adaptiveFarFadeInMsDirty) {
    adaptiveFarFadeInMsInputEl.value = String(Math.max(0, Math.round(app.adaptiveResamplingFarModeReturnFadeInMs ?? 0)));
  }
  if (adaptiveKpNearInputEl && !app.adaptiveKpNearEditing && !app.adaptiveKpNearDirty) {
    adaptiveKpNearInputEl.value = app.adaptiveResamplingKpNear === null ? '' : Number(app.adaptiveResamplingKpNear).toFixed(3);
  }
  if (adaptiveKiInputEl && !app.adaptiveKiEditing && !app.adaptiveKiDirty) {
    adaptiveKiInputEl.value = app.adaptiveResamplingKi === null ? '' : Number(app.adaptiveResamplingKi).toFixed(3);
  }
  if (
    adaptiveIntegralDischargeRatioInputEl &&
    !app.adaptiveIntegralDischargeRatioEditing &&
    !app.adaptiveIntegralDischargeRatioDirty
  ) {
    adaptiveIntegralDischargeRatioInputEl.value =
      app.adaptiveResamplingIntegralDischargeRatio === null
        ? ''
        : Number(app.adaptiveResamplingIntegralDischargeRatio).toFixed(3);
  }
  if (adaptiveMaxAdjustInputEl && !app.adaptiveMaxAdjustEditing && !app.adaptiveMaxAdjustDirty) {
    adaptiveMaxAdjustInputEl.value = app.adaptiveResamplingMaxAdjust === null ? '' : Math.round(Number(app.adaptiveResamplingMaxAdjust) * 1_000_000);
  }
  if (adaptiveNearFarThresholdInputEl && !app.adaptiveNearFarThresholdEditing && !app.adaptiveNearFarThresholdDirty) {
    adaptiveNearFarThresholdInputEl.value = app.adaptiveResamplingNearFarThresholdMs === null ? '' : String(Math.max(1, Math.round(app.adaptiveResamplingNearFarThresholdMs)));
  }
  if (adaptiveUpdateIntervalCallbacksInputEl && !app.adaptiveUpdateIntervalCallbacksEditing && !app.adaptiveUpdateIntervalCallbacksDirty) {
    adaptiveUpdateIntervalCallbacksInputEl.value = app.adaptiveResamplingUpdateIntervalCallbacks === null ? '' : String(Math.max(1, Math.round(app.adaptiveResamplingUpdateIntervalCallbacks)));
  }
  if (adaptiveBandTextEl) {
    adaptiveBandTextEl.textContent = app.adaptiveResamplingBand ?? '—';
  }
  if (adaptiveBandDotEl) {
    adaptiveBandDotEl.style.background =
      app.adaptiveResamplingBand === 'hard'
        ? '#ff4d4d'
        :
      app.adaptiveResamplingBand === 'far'
        ? '#ff9a5c'
        : app.adaptiveResamplingBand === 'near'
          ? '#52e2a2'
          : 'rgba(255,255,255,0.25)';
  }
  const isPaused = app.adaptiveResamplingPaused === true;
  if (adaptivePauseBtnEl) {
    adaptivePauseBtnEl.textContent = isPaused ? '▶ Resume' : '⏸ Pause';
    adaptivePauseBtnEl.style.background = isPaused ? 'rgba(255,180,0,0.18)' : 'rgba(255,255,255,0.08)';
    adaptivePauseBtnEl.style.borderColor = isPaused ? 'rgba(255,180,0,0.5)' : 'rgba(255,255,255,0.2)';
    adaptivePauseBtnEl.style.color = isPaused ? '#ffd87a' : '#d9ecff';
  }
  if (adaptiveRatioResetBtnEl) {
    adaptiveRatioResetBtnEl.style.display = isPaused ? '' : 'none';
  }
  const adaptiveDirty =
    app.adaptiveKpNearDirty ||
    app.adaptiveKiDirty ||
    app.adaptiveIntegralDischargeRatioDirty ||
    app.adaptiveMaxAdjustDirty ||
    app.adaptiveNearFarThresholdDirty ||
    app.adaptiveUpdateIntervalCallbacksDirty ||
    app.adaptiveFarFadeInMsDirty;
  if (adaptiveResamplingAdvancedApplyBtnEl) {
    adaptiveResamplingAdvancedApplyBtnEl.disabled = !adaptiveDirty;
    adaptiveResamplingAdvancedApplyBtnEl.style.opacity = adaptiveDirty ? '1' : '0.45';
    adaptiveResamplingAdvancedApplyBtnEl.style.cursor = adaptiveDirty ? 'pointer' : 'default';
  }
  if (adaptiveResamplingAdvancedCancelBtnEl) {
    adaptiveResamplingAdvancedCancelBtnEl.disabled = !adaptiveDirty;
    adaptiveResamplingAdvancedCancelBtnEl.style.opacity = adaptiveDirty ? '1' : '0.45';
    adaptiveResamplingAdvancedCancelBtnEl.style.cursor = adaptiveDirty ? 'pointer' : 'default';
  }
}

export function updateAdaptiveResamplingUI() {
  dirty.adaptiveResampling = true;
  dirty.resample = true;
  scheduleUIFlush();
}

export function resetAdaptiveResamplingAdvancedDirtyState() {
  app.adaptiveKpNearDirty = false;
  app.adaptiveKpNearEditing = false;
  app.adaptiveKiDirty = false;
  app.adaptiveKiEditing = false;
  app.adaptiveIntegralDischargeRatioDirty = false;
  app.adaptiveIntegralDischargeRatioEditing = false;
  app.adaptiveMaxAdjustDirty = false;
  app.adaptiveMaxAdjustEditing = false;
  app.adaptiveNearFarThresholdDirty = false;
  app.adaptiveNearFarThresholdEditing = false;
  app.adaptiveUpdateIntervalCallbacksDirty = false;
  app.adaptiveUpdateIntervalCallbacksEditing = false;
  app.adaptiveFarFadeInMsDirty = false;
  app.adaptiveFarFadeInMsEditing = false;
}
