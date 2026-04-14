/**
 * Adaptive resampling controls.
 *
 * Extracted from app.js (lines 3899-4040).
 */

import { app, dirty } from '../state.js';
import { formatNumber } from '../coordinates.js';
import { scheduleUIFlush } from '../flush.js';
import { t } from '../i18n.js';
import { inAudioPanel } from '../ui/panel-roots.js';

function getAdaptiveResamplingToggleEl() { return inAudioPanel('adaptiveResamplingToggle'); }
function getAdaptiveFarHardRecoverHighToggleEl() { return inAudioPanel('adaptiveFarHardRecoverHighToggle'); }
function getAdaptiveFarHardRecoverLowToggleEl() { return inAudioPanel('adaptiveFarHardRecoverLowToggle'); }
function getAdaptiveFarSilenceToggleEl() { return inAudioPanel('adaptiveFarSilenceToggle'); }
function getAdaptiveFarSilenceRowEl() { return inAudioPanel('adaptiveFarSilenceRow'); }
function getAdaptiveFarFadeRowEl() { return inAudioPanel('adaptiveFarFadeRow'); }
function getAdaptiveFarFadeInMsInputEl() { return inAudioPanel('adaptiveFarFadeInMsInput'); }
function getAdaptiveUpdateIntervalRowEl() { return inAudioPanel('adaptiveUpdateIntervalRow'); }
function getAdaptiveKpNearInputEl() { return inAudioPanel('adaptiveKpNearInput'); }
function getAdaptiveKpNearRowEl() { return inAudioPanel('adaptiveKpNearRow'); }
function getAdaptiveKiInputEl() { return inAudioPanel('adaptiveKiInput'); }
function getAdaptiveKiRowEl() { return inAudioPanel('adaptiveKiRow'); }
function getAdaptiveIntegralDischargeRatioInputEl() { return inAudioPanel('adaptiveIntegralDischargeRatioInput'); }
function getAdaptiveIntegralDischargeRowEl() { return inAudioPanel('adaptiveIntegralDischargeRow'); }
function getAdaptiveMaxAdjustInputEl() { return inAudioPanel('adaptiveMaxAdjustInput'); }
function getAdaptiveMaxAdjustRowEl() { return inAudioPanel('adaptiveMaxAdjustRow'); }
function getAdaptiveNearFarThresholdRowEl() { return inAudioPanel('adaptiveNearFarThresholdRow'); }
function getAdaptiveNearFarThresholdSymbolEl() { return inAudioPanel('adaptiveNearFarThresholdSymbol'); }
function getAdaptiveNearFarThresholdInputEl() { return inAudioPanel('adaptiveNearFarThresholdInput'); }
function getAdaptiveUpdateIntervalCallbacksInputEl() { return inAudioPanel('adaptiveUpdateIntervalCallbacksInput'); }
function getAdaptiveResamplingAdvancedApplyBtnEl() { return inAudioPanel('adaptiveResamplingAdvancedApplyBtn'); }
function getAdaptiveResamplingAdvancedCancelBtnEl() { return inAudioPanel('adaptiveResamplingAdvancedCancelBtn'); }
function getAdaptiveBandDotEl() { return inAudioPanel('adaptiveBandDot'); }
function getAdaptiveBandTextEl() { return inAudioPanel('adaptiveBandText'); }
function getAdaptiveRuntimeStateTextEl() { return inAudioPanel('adaptiveRuntimeStateText'); }
function getAdaptivePauseBtnEl() { return inAudioPanel('adaptivePauseBtn'); }
function getAdaptiveRatioResetBtnEl() { return inAudioPanel('adaptiveRatioResetBtn'); }

export function renderAdaptiveResamplingUI() {
  const adaptiveResamplingToggleEl = getAdaptiveResamplingToggleEl();
  const adaptiveFarHardRecoverHighToggleEl = getAdaptiveFarHardRecoverHighToggleEl();
  const adaptiveFarHardRecoverLowToggleEl = getAdaptiveFarHardRecoverLowToggleEl();
  const adaptiveFarSilenceToggleEl = getAdaptiveFarSilenceToggleEl();
  const adaptiveFarSilenceRowEl = getAdaptiveFarSilenceRowEl();
  const adaptiveFarFadeRowEl = getAdaptiveFarFadeRowEl();
  const adaptiveFarFadeInMsInputEl = getAdaptiveFarFadeInMsInputEl();
  const adaptiveUpdateIntervalRowEl = getAdaptiveUpdateIntervalRowEl();
  const adaptiveKpNearInputEl = getAdaptiveKpNearInputEl();
  const adaptiveKpNearRowEl = getAdaptiveKpNearRowEl();
  const adaptiveKiInputEl = getAdaptiveKiInputEl();
  const adaptiveKiRowEl = getAdaptiveKiRowEl();
  const adaptiveIntegralDischargeRatioInputEl = getAdaptiveIntegralDischargeRatioInputEl();
  const adaptiveIntegralDischargeRowEl = getAdaptiveIntegralDischargeRowEl();
  const adaptiveMaxAdjustInputEl = getAdaptiveMaxAdjustInputEl();
  const adaptiveMaxAdjustRowEl = getAdaptiveMaxAdjustRowEl();
  const adaptiveNearFarThresholdRowEl = getAdaptiveNearFarThresholdRowEl();
  const adaptiveNearFarThresholdSymbolEl = getAdaptiveNearFarThresholdSymbolEl();
  const adaptiveNearFarThresholdInputEl = getAdaptiveNearFarThresholdInputEl();
  const adaptiveUpdateIntervalCallbacksInputEl = getAdaptiveUpdateIntervalCallbacksInputEl();
  const adaptiveResamplingAdvancedApplyBtnEl = getAdaptiveResamplingAdvancedApplyBtnEl();
  const adaptiveResamplingAdvancedCancelBtnEl = getAdaptiveResamplingAdvancedCancelBtnEl();
  const adaptiveBandDotEl = getAdaptiveBandDotEl();
  const adaptiveBandTextEl = getAdaptiveBandTextEl();
  const adaptiveRuntimeStateTextEl = getAdaptiveRuntimeStateTextEl();
  const adaptivePauseBtnEl = getAdaptivePauseBtnEl();
  const adaptiveRatioResetBtnEl = getAdaptiveRatioResetBtnEl();
  if (!adaptiveResamplingToggleEl) return;
  const farModeEnabled =
    app.adaptiveResamplingHardRecoverHighInFarMode === true
    || app.adaptiveResamplingHardRecoverLowInFarMode === true
    || app.adaptiveResamplingForceSilenceInFarMode === true;
  const adaptiveEnabled = app.adaptiveResamplingEnabled === true;
  adaptiveResamplingToggleEl.checked = app.adaptiveResamplingEnabled === true;
  if (adaptiveFarHardRecoverHighToggleEl) {
    adaptiveFarHardRecoverHighToggleEl.checked = app.adaptiveResamplingHardRecoverHighInFarMode === true;
  }
  if (adaptiveFarHardRecoverLowToggleEl) {
    adaptiveFarHardRecoverLowToggleEl.checked = app.adaptiveResamplingHardRecoverLowInFarMode === true;
  }
  if (adaptiveFarSilenceToggleEl) {
    adaptiveFarSilenceToggleEl.checked = app.adaptiveResamplingForceSilenceInFarMode === true;
  }
  if (adaptiveFarSilenceRowEl) {
    adaptiveFarSilenceRowEl.classList.toggle('adaptive-param-disabled', false);
  }
  const farSilenceEnabled = app.adaptiveResamplingForceSilenceInFarMode === true;
  if (adaptiveFarFadeRowEl) {
    adaptiveFarFadeRowEl.classList.toggle('adaptive-param-disabled', !farSilenceEnabled);
  }
  if (adaptiveFarFadeInMsInputEl) {
    adaptiveFarFadeInMsInputEl.disabled = !farSilenceEnabled;
  }
  if (adaptiveUpdateIntervalRowEl) {
    adaptiveUpdateIntervalRowEl.classList.toggle('adaptive-param-disabled', !adaptiveEnabled);
  }
  if (adaptiveUpdateIntervalCallbacksInputEl) {
    adaptiveUpdateIntervalCallbacksInputEl.disabled = !adaptiveEnabled;
  }
  if (adaptiveMaxAdjustRowEl) {
    adaptiveMaxAdjustRowEl.classList.toggle('adaptive-param-disabled', !adaptiveEnabled);
  }
  if (adaptiveMaxAdjustInputEl) {
    adaptiveMaxAdjustInputEl.disabled = !adaptiveEnabled;
  }
  if (adaptiveKpNearRowEl) {
    adaptiveKpNearRowEl.classList.toggle('adaptive-param-disabled', !adaptiveEnabled);
  }
  if (adaptiveKpNearInputEl) {
    adaptiveKpNearInputEl.disabled = !adaptiveEnabled;
  }
  if (adaptiveKiRowEl) {
    adaptiveKiRowEl.classList.toggle('adaptive-param-disabled', !adaptiveEnabled);
  }
  if (adaptiveKiInputEl) {
    adaptiveKiInputEl.disabled = !adaptiveEnabled;
  }
  if (adaptiveIntegralDischargeRowEl) {
    adaptiveIntegralDischargeRowEl.classList.toggle('adaptive-param-disabled', !adaptiveEnabled);
  }
  if (adaptiveIntegralDischargeRatioInputEl) {
    adaptiveIntegralDischargeRatioInputEl.disabled = !adaptiveEnabled;
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
  if (adaptiveRuntimeStateTextEl) {
    adaptiveRuntimeStateTextEl.textContent = app.adaptiveResamplingState ?? '—';
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
    adaptivePauseBtnEl.textContent = isPaused ? `▶ ${t('adaptive.resume')}` : `⏸ ${t('adaptive.pause')}`;
    adaptivePauseBtnEl.style.background = isPaused ? 'rgba(255,180,0,0.18)' : 'rgba(255,255,255,0.08)';
    adaptivePauseBtnEl.style.borderColor = isPaused ? 'rgba(255,180,0,0.5)' : 'rgba(255,255,255,0.2)';
    adaptivePauseBtnEl.style.color = isPaused ? '#ffd87a' : '#d9ecff';
    adaptivePauseBtnEl.disabled = !adaptiveEnabled;
    adaptivePauseBtnEl.style.opacity = adaptiveEnabled ? '1' : '0.45';
    adaptivePauseBtnEl.style.cursor = adaptiveEnabled ? 'pointer' : 'default';
  }
  if (adaptiveRatioResetBtnEl) {
    adaptiveRatioResetBtnEl.style.display = adaptiveEnabled && isPaused ? '' : 'none';
    adaptiveRatioResetBtnEl.disabled = !adaptiveEnabled;
    adaptiveRatioResetBtnEl.style.opacity = adaptiveEnabled ? '1' : '0.45';
    adaptiveRatioResetBtnEl.style.cursor = adaptiveEnabled ? 'pointer' : 'default';
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
