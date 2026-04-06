import { invoke } from '@tauri-apps/api/core';
import { app } from '../state.js';
import { t } from '../i18n.js';
import { updateMasterGainUI, updateLoudnessDisplay } from '../controls/master.js';
import { updateAdaptiveResamplingUI, resetAdaptiveResamplingAdvancedDirtyState } from '../controls/adaptive.js';
import {
  closeAudioSampleRateMenu, openAudioSampleRateMenu, updateAudioFormatDisplay,
  applyAudioSampleRateNow, applyAudioOutputDeviceNow, applyRampModeNow
} from '../controls/audio.js';
import { applyLatencyTargetNow, updateLatencyDisplay } from '../controls/latency.js';

export function setupAudioPanelListeners() {
  const masterGainSliderEl = document.getElementById('masterGainSlider');
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
  const adaptiveKpNearInputEl = document.getElementById('adaptiveKpNearInput');
  const adaptiveKiInputEl = document.getElementById('adaptiveKiInput');
  const adaptiveIntegralDischargeRatioInputEl = document.getElementById('adaptiveIntegralDischargeRatioInput');
  const adaptiveMaxAdjustInputEl = document.getElementById('adaptiveMaxAdjustInput');
  const adaptiveNearFarThresholdInputEl = document.getElementById('adaptiveNearFarThresholdInput');
  const adaptiveUpdateIntervalCallbacksInputEl = document.getElementById('adaptiveUpdateIntervalCallbacksInput');
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
      invoke('control_adaptive_resampling', { enable: enabled });
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
    invoke('control_adaptive_resampling_enable_far_mode', { enable: enableFarMode ? 1 : 0 });
  }

  if (adaptiveFarHardRecoverHighToggleEl) {
    adaptiveFarHardRecoverHighToggleEl.addEventListener('change', () => {
      const enable = adaptiveFarHardRecoverHighToggleEl.checked ? 1 : 0;
      app.adaptiveResamplingHardRecoverHighInFarMode = enable === 1;
      syncAdaptiveFarModeDerived();
      updateAdaptiveResamplingUI();
      invoke('control_adaptive_resampling_hard_recover_high_in_far_mode', { enable });
    });
  }

  if (adaptiveFarHardRecoverLowToggleEl) {
    adaptiveFarHardRecoverLowToggleEl.addEventListener('change', () => {
      const enable = adaptiveFarHardRecoverLowToggleEl.checked ? 1 : 0;
      app.adaptiveResamplingHardRecoverLowInFarMode = enable === 1;
      syncAdaptiveFarModeDerived();
      updateAdaptiveResamplingUI();
      invoke('control_adaptive_resampling_hard_recover_low_in_far_mode', { enable });
    });
  }

  if (adaptiveFarSilenceToggleEl) {
    adaptiveFarSilenceToggleEl.addEventListener('change', () => {
      const enable = adaptiveFarSilenceToggleEl.checked ? 1 : 0;
      app.adaptiveResamplingForceSilenceInFarMode = enable === 1;
      syncAdaptiveFarModeDerived();
      updateAdaptiveResamplingUI();
      invoke('control_adaptive_resampling_force_silence_in_far_mode', { enable });
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
      const ki = Math.max(0.01, Number(adaptiveKiInputEl?.value) || 0);
      const integralDischargeRatio = Math.min(1, Math.max(0, Number(adaptiveIntegralDischargeRatioInputEl?.value) || 0));
      const maxAdjustPpm = Math.max(1, Math.round(Number(adaptiveMaxAdjustInputEl?.value) || 0));
      const maxAdjust = Math.max(0.000001, maxAdjustPpm / 1_000_000);
      const nearFarThresholdMs = Math.max(1, Math.round(Number(adaptiveNearFarThresholdInputEl?.value) || 0));
      const updateIntervalCallbacks = Math.max(1, Math.round(Number(adaptiveUpdateIntervalCallbacksInputEl?.value) || 0));
      const farModeReturnFadeInMs = Math.max(0, Math.round(Number(adaptiveFarFadeInMsInputEl?.value) || 0));

      app.adaptiveResamplingKpNear = kpNear;
      app.adaptiveResamplingKi = ki;
      app.adaptiveResamplingIntegralDischargeRatio = integralDischargeRatio;
      app.adaptiveResamplingMaxAdjust = maxAdjust;
      app.adaptiveResamplingNearFarThresholdMs = nearFarThresholdMs;
      app.adaptiveResamplingUpdateIntervalCallbacks = updateIntervalCallbacks;
      app.adaptiveResamplingFarModeReturnFadeInMs = farModeReturnFadeInMs;
      updateAdaptiveResamplingUI();

      invoke('control_adaptive_resampling_kp_near', { value: kpNear });
      invoke('control_adaptive_resampling_ki', { value: ki });
      invoke('control_adaptive_resampling_integral_discharge_ratio', { value: integralDischargeRatio });
      invoke('control_adaptive_resampling_max_adjust', { value: maxAdjust });
      invoke('control_adaptive_resampling_near_far_threshold_ms', { value: nearFarThresholdMs });
      invoke('control_adaptive_resampling_update_interval_callbacks', { value: updateIntervalCallbacks });
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

  if (adaptivePauseBtnEl) {
    adaptivePauseBtnEl.addEventListener('click', () => {
      const enable = app.adaptiveResamplingPaused ? 0 : 1;
      app.adaptiveResamplingPaused = enable === 1;
      updateAdaptiveResamplingUI();
      invoke('control_adaptive_resampling_pause', { enable });
    });
  }

  if (adaptiveRatioResetBtnEl) {
    adaptiveRatioResetBtnEl.addEventListener('click', () => {
      invoke('control_adaptive_resampling_reset_ratio');
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

  if (rampModeSelectEl) {
    rampModeSelectEl.addEventListener('change', () => {
      applyRampModeNow();
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
}
