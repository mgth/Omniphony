import { invoke } from '@tauri-apps/api/core';
import { app } from '../state.js';
import { t } from '../i18n.js';
import { updateMasterGainUI, updateLoudnessDisplay, updateAutoGainUI, updateAutoGainCeilingUI } from '../controls/master.js';
import { updateAdaptiveResamplingUI, resetAdaptiveResamplingAdvancedDirtyState } from '../controls/adaptive.js';
import {
  closeAudioSampleRateMenu, openAudioSampleRateMenu, updateAudioFormatDisplay,
  applyAudioSampleRateNow, applyAudioOutputDeviceNow, applyRampModeNow,
  sendAudioConfig,
  applyAudioOutputBackendNow, applyAudioOutputFileNow, applyAudioOutputFileFormatNow,
  applyAudioOutputNamedPipeNow
} from '../controls/audio.js';
import { bindOptionControls } from '../options-binder.js';
import { resetVirtualBed } from '../controls/virtual-bed.js';
import { applyLatencyTargetNow, updateLatencyDisplay } from '../controls/latency.js';
import { openAutoTuneWizard } from '../auto-tune/wizard-ui.js';
import { toggleResamplePlot } from '../controls/resample-plot.js';
import { toggleDiagPlot } from '../controls/diag-plot.js';

export function setupAudioPanelListeners() {
  const masterGainSliderEl = document.getElementById('masterGainSlider');
  const autoGainToggleEl = document.getElementById('autoGainToggle');
  const autoGainCeilingSliderEl = document.getElementById('autoGainCeilingSlider');
  const loudnessToggleEl = document.getElementById('loudnessToggle');
  const adaptiveResamplingToggleEl = document.getElementById('adaptiveResamplingToggle');
  const adaptiveFarHardRecoverHighToggleEl = document.getElementById('adaptiveFarHardRecoverHighToggle');
  const adaptiveFarHardRecoverLowToggleEl = document.getElementById('adaptiveFarHardRecoverLowToggle');
  const adaptiveFarSilenceToggleEl = document.getElementById('adaptiveFarSilenceToggle');
  const adaptiveFarFadeInMsInputEl = document.getElementById('adaptiveFarFadeInMsInput');
  const adaptiveResamplingAdvancedApplyBtnEl = document.getElementById('adaptiveResamplingAdvancedApplyBtn');
  const adaptiveResamplingAdvancedCancelBtnEl = document.getElementById('adaptiveResamplingAdvancedCancelBtn');
  const adaptivePauseBtnEl = document.getElementById('adaptivePauseBtn');
  const adaptiveRatioResetBtnEl = document.getElementById('adaptiveRatioResetBtn');
  const autoTunePiBtnEl = document.getElementById('autoTunePiBtn');
  const resamplePlotToggleBtnEl = document.getElementById('resamplePlotToggleBtn');
  const diagPlotToggleBtnEl = document.getElementById('diagPlotToggleBtn');
  const adaptiveKpNearInputEl = document.getElementById('adaptiveKpNearInput');
  const adaptiveKiInputEl = document.getElementById('adaptiveKiInput');
  const adaptiveIntegralDischargeRatioInputEl = document.getElementById('adaptiveIntegralDischargeRatioInput');
  const adaptiveMaxAdjustInputEl = document.getElementById('adaptiveMaxAdjustInput');
  const adaptiveHighRecoverEntryMarginInputEl = document.getElementById('adaptiveHighRecoverEntryMarginInput');
  const adaptiveUpdateIntervalCallbacksInputEl = document.getElementById('adaptiveUpdateIntervalCallbacksInput');
  const adaptiveLowRecoverSettleStableMsInputEl = document.getElementById('adaptiveLowRecoverSettleStableMsInput');
  const adaptiveLowRecoverEntryMarginMsInputEl = document.getElementById('adaptiveLowRecoverEntryMarginMsInput');
  const adaptiveLowRecoverExitMarginMsInputEl = document.getElementById('adaptiveLowRecoverExitMarginMsInput');
  const adaptiveLowRecoverSettleMarginMsInputEl = document.getElementById('adaptiveLowRecoverSettleMarginMsInput');
  const adaptiveLowRecoverRefillDeltaAlphaInputEl = document.getElementById('adaptiveLowRecoverRefillDeltaAlphaInput');
  const adaptiveControlSmoothingCutoffHzInputEl = document.getElementById('adaptiveControlSmoothingCutoffHzInput');
  const adaptiveControlSmoothingOrderSelectEl = document.getElementById('adaptiveControlSmoothingOrderSelect');
  const adaptiveUsePreBridgeClockToggleEl = document.getElementById('adaptiveUsePreBridgeClockToggle');
  const adaptiveUseOutputPacingToggleEl = document.getElementById('adaptiveUseOutputPacingToggle');
  const adaptiveDisableBackpressureToggleEl = document.getElementById('adaptiveDisableBackpressureToggle');
  const latencyTargetInputEl = document.getElementById('latencyTargetInput');
  const latencyTargetApplyBtnEl = document.getElementById('latencyTargetApplyBtn');
  const audioSampleRateMenuBtnEl = document.getElementById('audioSampleRateMenuBtn');
  const audioSampleRateMenuEl = document.getElementById('audioSampleRateMenu');
  const audioSampleRateInputEl = document.getElementById('audioSampleRateInput');
  const audioSampleRateControlEl = document.getElementById('audioSampleRateControl');
  const audioOutputDeviceSelectEl = document.getElementById('audioOutputDeviceSelect');
  const refreshOutputDevicesBtnEl = document.getElementById('refreshOutputDevicesBtn');
  const rampModeSelectEl = document.getElementById('rampModeSelect');

  if (masterGainSliderEl) {
    masterGainSliderEl.addEventListener('input', () => {
      if (!app.oscSnapshotReady) {
        updateMasterGainUI();
        return;
      }
      const value = Number(masterGainSliderEl.value);
      if (!Number.isFinite(value) || value <= 0) {
        return;
      }
      app.masterGain = value;
      updateMasterGainUI();
      invoke('control_master_gain', { gain: app.masterGain });
    });

    masterGainSliderEl.addEventListener('dblclick', () => {
      if (!app.oscSnapshotReady) {
        updateMasterGainUI();
        return;
      }
      app.masterGain = 1;
      updateMasterGainUI();
      invoke('control_master_gain', { gain: app.masterGain });
    });
  }

  if (autoGainToggleEl) {
    autoGainToggleEl.addEventListener('change', () => {
      const enabled = autoGainToggleEl.checked;
      app.autoGain = enabled;
      updateAutoGainUI();
      invoke('control_auto_gain', { enable: enabled ? 1 : 0 });
    });
  }

  if (autoGainCeilingSliderEl) {
    autoGainCeilingSliderEl.addEventListener('input', () => {
      const db = Number(autoGainCeilingSliderEl.value);
      if (!Number.isFinite(db)) {
        return;
      }
      app.autoGainCeilingDb = db;
      updateAutoGainCeilingUI();
      invoke('control_auto_gain_ceiling', { db });
    });
  }

  if (loudnessToggleEl) {
    loudnessToggleEl.addEventListener('change', () => {
      const enabled = loudnessToggleEl.checked ? 1 : 0;
      app.loudnessEnabled = enabled === 1;
      updateLoudnessDisplay();
      invoke('control_loudness', { enable: enabled });
    });
  }

  if (adaptiveResamplingToggleEl) {
    adaptiveResamplingToggleEl.addEventListener('change', () => {
      const enabled = adaptiveResamplingToggleEl.checked ? 1 : 0;
      app.adaptiveResamplingEnabled = enabled === 1;
      updateAdaptiveResamplingUI();
      sendAudioConfig();
    });
  }

  function syncAdaptiveFarModeDerived() {
    const enableFarMode =
      app.adaptiveResamplingHardRecoverHighInFarMode === true
      || app.adaptiveResamplingHardRecoverLowInFarMode === true
      || app.adaptiveResamplingForceSilenceInFarMode === true;
    if (app.adaptiveResamplingEnableFarMode === enableFarMode) {
      return;
    }
    app.adaptiveResamplingEnableFarMode = enableFarMode;
  }

  if (adaptiveFarHardRecoverHighToggleEl) {
    adaptiveFarHardRecoverHighToggleEl.addEventListener('change', () => {
      const enable = adaptiveFarHardRecoverHighToggleEl.checked ? 1 : 0;
      app.adaptiveResamplingHardRecoverHighInFarMode = enable === 1;
      syncAdaptiveFarModeDerived();
      updateAdaptiveResamplingUI();
      sendAudioConfig();
    });
  }

  if (adaptiveFarHardRecoverLowToggleEl) {
    adaptiveFarHardRecoverLowToggleEl.addEventListener('change', () => {
      const enable = adaptiveFarHardRecoverLowToggleEl.checked ? 1 : 0;
      app.adaptiveResamplingHardRecoverLowInFarMode = enable === 1;
      syncAdaptiveFarModeDerived();
      updateAdaptiveResamplingUI();
      sendAudioConfig();
    });
  }

  if (adaptiveFarSilenceToggleEl) {
    adaptiveFarSilenceToggleEl.addEventListener('change', () => {
      const enable = adaptiveFarSilenceToggleEl.checked ? 1 : 0;
      app.adaptiveResamplingForceSilenceInFarMode = enable === 1;
      syncAdaptiveFarModeDerived();
      updateAdaptiveResamplingUI();
      sendAudioConfig();
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

  if (adaptiveIntegralDischargeRatioInputEl) {
    adaptiveIntegralDischargeRatioInputEl.addEventListener('focus', () => {
      app.adaptiveIntegralDischargeRatioEditing = true;
      adaptiveIntegralDischargeRatioInputEl.select();
    });
    adaptiveIntegralDischargeRatioInputEl.addEventListener('input', () => {
      app.adaptiveIntegralDischargeRatioEditing = true;
      app.adaptiveIntegralDischargeRatioDirty = true;
      updateAdaptiveResamplingUI();
    });
  }

  if (adaptiveResamplingAdvancedApplyBtnEl) {
    adaptiveResamplingAdvancedApplyBtnEl.addEventListener('click', () => {
      if (adaptiveResamplingAdvancedApplyBtnEl.disabled) return;
      const kpNear = Math.max(0.01, Number(adaptiveKpNearInputEl?.value) || 0);
      const ki = Math.max(0, Number(adaptiveKiInputEl?.value) || 0);
      const integralDischargeRatio = Math.min(1, Math.max(0, Number(adaptiveIntegralDischargeRatioInputEl?.value) || 0));
      const maxAdjustPpm = Math.max(1, Math.round(Number(adaptiveMaxAdjustInputEl?.value) || 0));
      const maxAdjust = Math.max(0.000001, maxAdjustPpm / 1_000_000);
      const highRecoverEntryMarginMs = Math.max(1, Math.round(Number(adaptiveHighRecoverEntryMarginInputEl?.value) || 0));
      const updateIntervalCallbacks = Math.max(1, Math.round(Number(adaptiveUpdateIntervalCallbacksInputEl?.value) || 0));
      const farModeReturnFadeInMs = Math.max(0, Math.round(Number(adaptiveFarFadeInMsInputEl?.value) || 0));
      const lowRecoverSettleStableMs = Math.max(0, Math.round(Number(adaptiveLowRecoverSettleStableMsInputEl?.value) || 0));
      const lowRecoverEntryMarginMs = Math.max(0, Number(adaptiveLowRecoverEntryMarginMsInputEl?.value) || 0);
      // The exit margin must stay strictly below the entry margin so the
      // low-recover hysteresis is well-formed (you enter Refill deeper than you
      // leave it). Clamp to at most entry − one step, and reflect the corrected
      // value back into the input so the user sees the safe value.
      const exitMarginStep = 0.1;
      const maxLowRecoverExitMarginMs = Math.max(0, lowRecoverEntryMarginMs - exitMarginStep);
      const lowRecoverExitMarginMs = Math.min(
        Math.max(0, Number(adaptiveLowRecoverExitMarginMsInputEl?.value) || 0),
        maxLowRecoverExitMarginMs
      );
      if (adaptiveLowRecoverExitMarginMsInputEl) {
        adaptiveLowRecoverExitMarginMsInputEl.value = String(lowRecoverExitMarginMs);
      }
      const lowRecoverSettleMarginMs = Math.max(0, Number(adaptiveLowRecoverSettleMarginMsInputEl?.value) || 0);
      const lowRecoverRefillDeltaAlpha = Math.min(1, Math.max(0, Number(adaptiveLowRecoverRefillDeltaAlphaInputEl?.value) || 0));
      const controlSmoothingCutoffHz = Math.max(0.001, Number(adaptiveControlSmoothingCutoffHzInputEl?.value) || 0.5);
      const controlSmoothingOrder = Math.min(2, Math.max(1, Math.round(Number(adaptiveControlSmoothingOrderSelectEl?.value) || 1)));

      app.adaptiveResamplingKpNear = kpNear;
      app.adaptiveResamplingKi = ki;
      app.adaptiveResamplingIntegralDischargeRatio = integralDischargeRatio;
      app.adaptiveResamplingMaxAdjust = maxAdjust;
      app.adaptiveResamplingHighRecoverEntryMarginMs = highRecoverEntryMarginMs;
      app.adaptiveResamplingUpdateIntervalCallbacks = updateIntervalCallbacks;
      app.adaptiveResamplingFarModeReturnFadeInMs = farModeReturnFadeInMs;
      app.adaptiveResamplingLowRecoverSettleStableMs = lowRecoverSettleStableMs;
      app.adaptiveResamplingLowRecoverEntryMarginMs = lowRecoverEntryMarginMs;
      app.adaptiveResamplingLowRecoverExitMarginMs = lowRecoverExitMarginMs;
      app.adaptiveResamplingLowRecoverSettleMarginMs = lowRecoverSettleMarginMs;
      app.adaptiveResamplingLowRecoverRefillDeltaAlpha = lowRecoverRefillDeltaAlpha;
      app.adaptiveResamplingControlSmoothingCutoffHz = controlSmoothingCutoffHz;
      app.adaptiveResamplingControlSmoothingOrder = controlSmoothingOrder;
      updateAdaptiveResamplingUI();
      sendAudioConfig();

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

  if (adaptivePauseBtnEl) {
    // Use pointerup instead of click: the button gets `disabled = !adaptiveEnabled`
    // rewritten every UI flush (~rAF), and any transient `false` between mousedown
    // and mouseup makes the browser cancel the click event entirely. pointerup is
    // not affected by the disabled state in the same way.
    let pointerDownOnButton = false;
    adaptivePauseBtnEl.addEventListener('pointerdown', (event) => {
      if (event.button !== 0) return;
      pointerDownOnButton = true;
    });
    adaptivePauseBtnEl.addEventListener('pointercancel', () => {
      pointerDownOnButton = false;
    });
    adaptivePauseBtnEl.addEventListener('pointerup', (event) => {
      if (event.button !== 0) return;
      if (!pointerDownOnButton) return;
      pointerDownOnButton = false;
      if (adaptivePauseBtnEl.disabled) return;
      const enable = app.adaptiveResamplingPaused ? 0 : 1;
      app.adaptiveResamplingPaused = enable === 1;
      updateAdaptiveResamplingUI();
      sendAudioConfig();
    });
  }

  if (adaptiveRatioResetBtnEl) {
    adaptiveRatioResetBtnEl.addEventListener('click', () => {
      invoke('control_adaptive_resampling_reset_ratio');
    });
  }

  if (autoTunePiBtnEl) {
    autoTunePiBtnEl.addEventListener('click', () => {
      openAutoTuneWizard();
    });
  }

  if (resamplePlotToggleBtnEl) {
    resamplePlotToggleBtnEl.addEventListener('click', () => {
      toggleResamplePlot();
    });
  }

  if (diagPlotToggleBtnEl) {
    diagPlotToggleBtnEl.addEventListener('click', () => {
      toggleDiagPlot();
    });
  }

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

  if (adaptiveHighRecoverEntryMarginInputEl) {
    adaptiveHighRecoverEntryMarginInputEl.addEventListener('focus', () => {
      app.adaptiveHighRecoverEntryMarginEditing = true;
      adaptiveHighRecoverEntryMarginInputEl.select();
    });
    adaptiveHighRecoverEntryMarginInputEl.addEventListener('input', () => {
      app.adaptiveHighRecoverEntryMarginEditing = true;
      app.adaptiveHighRecoverEntryMarginDirty = true;
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

  if (adaptiveLowRecoverSettleStableMsInputEl) {
    adaptiveLowRecoverSettleStableMsInputEl.addEventListener('focus', () => {
      app.adaptiveLowRecoverSettleStableMsEditing = true;
      adaptiveLowRecoverSettleStableMsInputEl.select();
    });
    adaptiveLowRecoverSettleStableMsInputEl.addEventListener('input', () => {
      app.adaptiveLowRecoverSettleStableMsEditing = true;
      app.adaptiveLowRecoverSettleStableMsDirty = true;
      updateAdaptiveResamplingUI();
    });
  }

  if (adaptiveLowRecoverEntryMarginMsInputEl) {
    adaptiveLowRecoverEntryMarginMsInputEl.addEventListener('focus', () => {
      app.adaptiveLowRecoverEntryMarginMsEditing = true;
      adaptiveLowRecoverEntryMarginMsInputEl.select();
    });
    adaptiveLowRecoverEntryMarginMsInputEl.addEventListener('input', () => {
      app.adaptiveLowRecoverEntryMarginMsEditing = true;
      app.adaptiveLowRecoverEntryMarginMsDirty = true;
      updateAdaptiveResamplingUI();
    });
  }

  if (adaptiveLowRecoverExitMarginMsInputEl) {
    adaptiveLowRecoverExitMarginMsInputEl.addEventListener('focus', () => {
      app.adaptiveLowRecoverExitMarginMsEditing = true;
      adaptiveLowRecoverExitMarginMsInputEl.select();
    });
    adaptiveLowRecoverExitMarginMsInputEl.addEventListener('input', () => {
      app.adaptiveLowRecoverExitMarginMsEditing = true;
      app.adaptiveLowRecoverExitMarginMsDirty = true;
      updateAdaptiveResamplingUI();
    });
  }

  if (adaptiveLowRecoverSettleMarginMsInputEl) {
    adaptiveLowRecoverSettleMarginMsInputEl.addEventListener('focus', () => {
      app.adaptiveLowRecoverSettleMarginMsEditing = true;
      adaptiveLowRecoverSettleMarginMsInputEl.select();
    });
    adaptiveLowRecoverSettleMarginMsInputEl.addEventListener('input', () => {
      app.adaptiveLowRecoverSettleMarginMsEditing = true;
      app.adaptiveLowRecoverSettleMarginMsDirty = true;
      updateAdaptiveResamplingUI();
    });
  }

  if (adaptiveLowRecoverRefillDeltaAlphaInputEl) {
    adaptiveLowRecoverRefillDeltaAlphaInputEl.addEventListener('focus', () => {
      app.adaptiveLowRecoverRefillDeltaAlphaEditing = true;
      adaptiveLowRecoverRefillDeltaAlphaInputEl.select();
    });
    adaptiveLowRecoverRefillDeltaAlphaInputEl.addEventListener('input', () => {
      app.adaptiveLowRecoverRefillDeltaAlphaEditing = true;
      app.adaptiveLowRecoverRefillDeltaAlphaDirty = true;
      updateAdaptiveResamplingUI();
    });
  }

  if (adaptiveControlSmoothingCutoffHzInputEl) {
    adaptiveControlSmoothingCutoffHzInputEl.addEventListener('focus', () => {
      app.adaptiveControlSmoothingCutoffHzEditing = true;
      adaptiveControlSmoothingCutoffHzInputEl.select();
    });
    adaptiveControlSmoothingCutoffHzInputEl.addEventListener('input', () => {
      app.adaptiveControlSmoothingCutoffHzEditing = true;
      app.adaptiveControlSmoothingCutoffHzDirty = true;
      updateAdaptiveResamplingUI();
    });
  }

  if (adaptiveControlSmoothingOrderSelectEl) {
    adaptiveControlSmoothingOrderSelectEl.addEventListener('change', () => {
      app.adaptiveControlSmoothingOrderDirty = true;
      updateAdaptiveResamplingUI();
    });
  }

  if (adaptiveUsePreBridgeClockToggleEl) {
    adaptiveUsePreBridgeClockToggleEl.addEventListener('change', () => {
      app.adaptiveResamplingUsePreBridgeClock =
        adaptiveUsePreBridgeClockToggleEl.checked;
      updateAdaptiveResamplingUI();
      sendAudioConfig();
    });
  }

  if (adaptiveUseOutputPacingToggleEl) {
    adaptiveUseOutputPacingToggleEl.addEventListener('change', () => {
      app.adaptiveResamplingUseOutputPacing =
        adaptiveUseOutputPacingToggleEl.checked;
      updateAdaptiveResamplingUI();
      sendAudioConfig();
    });
  }

  if (adaptiveDisableBackpressureToggleEl) {
    adaptiveDisableBackpressureToggleEl.addEventListener('change', () => {
      app.adaptiveResamplingDisableBackpressure =
        adaptiveDisableBackpressureToggleEl.checked;
      updateAdaptiveResamplingUI();
      sendAudioConfig();
    });
  }

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

  const audioOutputBackendSelectEl = document.getElementById('audioOutputBackendSelect');
  if (audioOutputBackendSelectEl) {
    audioOutputBackendSelectEl.addEventListener('change', () => {
      applyAudioOutputBackendNow();
    });
  }

  const audioOutputPipeToggleEl = document.getElementById('audioOutputPipeToggle');
  if (audioOutputPipeToggleEl) {
    audioOutputPipeToggleEl.addEventListener('change', () => {
      applyAudioOutputNamedPipeNow();
    });
  }

  const audioOutputFileInputEl = document.getElementById('audioOutputFileInput');
  if (audioOutputFileInputEl) {
    audioOutputFileInputEl.addEventListener('focus', () => {
      app.audioOutputFileEditing = true;
      audioOutputFileInputEl.select();
    });
    audioOutputFileInputEl.addEventListener('change', () => {
      applyAudioOutputFileNow();
    });
    audioOutputFileInputEl.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') applyAudioOutputFileNow();
    });
  }

  const audioOutputFileFormatSelectEl = document.getElementById('audioOutputFileFormatSelect');
  if (audioOutputFileFormatSelectEl) {
    audioOutputFileFormatSelectEl.addEventListener('change', () => {
      applyAudioOutputFileFormatNow();
    });
  }

  if (rampModeSelectEl) {
    rampModeSelectEl.addEventListener('change', () => {
      applyRampModeNow();
    });
  }

  // Declared live options (spatialize switch, Side/Back, generator select,
  // phantom switch, by-index/by-name) are wired generically by the registry
  // binder from their data-option markup — no per-option listener here.
  // Per-generator/phantom parameter sliders are built dynamically from the
  // declared schemas; their listeners are wired at creation.
  bindOptionControls();

  const virtualBedResetBtnEl = document.getElementById('virtualBedResetBtn');
  if (virtualBedResetBtnEl) {
    virtualBedResetBtnEl.addEventListener('click', () => {
      if (!window.confirm(t('confirm.resetVirtualBed'))) return;
      resetVirtualBed();
    });
  }

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

  document.addEventListener('pointerdown', (event) => {
    if (!audioOutputFileInputEl) return;
    const target = event.target;
    if (!(target instanceof Node)) return;
    if (target !== audioOutputFileInputEl) {
      app.audioOutputFileEditing = false;
    }
  });
}
