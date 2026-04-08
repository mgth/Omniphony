import { invoke } from '@tauri-apps/api/core';
import { app, isSpeakerLayoutFrozen, speakerBaseGains, speakerDelays } from '../state.js';
import {
  renderSpeakerEditor, requestAddSpeaker, requestMoveSpeaker, requestRemoveSpeaker,
  applySpeakerCartesianEdit, applySpeakerPolarEdit,
  setSpeakerSpatializeLocal, setSpeakerCoordMode,
  updateSpeakerVisualsFromState, updateSpeakerGizmo, updateControlsForEditMode,
  computeAndApplySpeakerDelays, adjustSpeakerDistancesFromDelays,
  samplesToDelayMs
} from '../speakers.js';
import { applyGroupGains } from '../mute-solo.js';

export function setupSpeakerEditorListeners() {
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

  if (editModeSelectEl) {
    editModeSelectEl.addEventListener('change', () => {
      if (isSpeakerLayoutFrozen()) return;
      app.activeEditMode = editModeSelectEl.value;
      updateSpeakerGizmo();
      updateControlsForEditMode();
    });
  }

  if (speakerEditCartesianGizmoBtnEl) {
    speakerEditCartesianGizmoBtnEl.addEventListener('click', () => {
      if (isSpeakerLayoutFrozen()) return;
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
      if (isSpeakerLayoutFrozen()) return;
      requestAddSpeaker();
    });
  }

  if (speakerMoveUpBtnEl) {
    speakerMoveUpBtnEl.addEventListener('click', () => {
      if (isSpeakerLayoutFrozen()) return;
      requestMoveSpeaker(-1);
    });
  }

  if (speakerMoveDownBtnEl) {
    speakerMoveDownBtnEl.addEventListener('click', () => {
      if (isSpeakerLayoutFrozen()) return;
      requestMoveSpeaker(1);
    });
  }

  if (speakerRemoveBtnEl) {
    speakerRemoveBtnEl.addEventListener('click', () => {
      if (isSpeakerLayoutFrozen()) return;
      requestRemoveSpeaker();
    });
  }

  if (speakerEditPolarGizmoBtnEl) {
    speakerEditPolarGizmoBtnEl.addEventListener('click', () => {
      if (isSpeakerLayoutFrozen()) return;
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
      if (isSpeakerLayoutFrozen()) return;
      if (app.selectedSpeakerIndex === null) return;
      const id = String(app.selectedSpeakerIndex);
      const value = Number(speakerEditGainSliderEl.value);
      if (!Number.isFinite(value)) return;
      speakerBaseGains.set(id, value);
      applyGroupGains('speaker');
      renderSpeakerEditor();
    });
    speakerEditGainSliderEl.addEventListener('dblclick', () => {
      if (isSpeakerLayoutFrozen()) return;
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
      if (isSpeakerLayoutFrozen()) return;
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
      if (isSpeakerLayoutFrozen()) return;
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
      if (isSpeakerLayoutFrozen()) return;
      computeAndApplySpeakerDelays();
    });
  }

  if (speakerEditDelayToDistanceBtnEl) {
    speakerEditDelayToDistanceBtnEl.addEventListener('click', () => {
      if (isSpeakerLayoutFrozen()) return;
      adjustSpeakerDistancesFromDelays();
    });
  }

  if (speakerEditNameInputEl) {
    speakerEditNameInputEl.addEventListener('change', () => {
      if (isSpeakerLayoutFrozen()) return;
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

  function bindSpeakerCoordChange(inputEl, getter) {
    if (!inputEl) return;
    inputEl.addEventListener('change', () => {
      if (isSpeakerLayoutFrozen()) return;
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

  if (speakerEditSpatializeToggleEl) {
    speakerEditSpatializeToggleEl.addEventListener('change', () => {
      if (isSpeakerLayoutFrozen()) return;
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
      if (isSpeakerLayoutFrozen()) return;
      if (app.selectedSpeakerIndex === null || !speakerEditCartesianModeEl.checked) return;
      setSpeakerCoordMode(app.selectedSpeakerIndex, 'cartesian');
    });
  }

  if (speakerEditPolarModeEl) {
    speakerEditPolarModeEl.addEventListener('change', () => {
      if (isSpeakerLayoutFrozen()) return;
      if (app.selectedSpeakerIndex === null || !speakerEditPolarModeEl.checked) return;
      setSpeakerCoordMode(app.selectedSpeakerIndex, 'polar');
    });
  }
}
