import { invoke } from '@tauri-apps/api/core';
import { t } from '../i18n.js';
import { app, isSpeakerLayoutFrozen, speakerBaseGains, speakerDelays } from '../state.js';
import { normalizedToMeters, metersToSceneUnits, metersPerUnit } from '../coordinates.js';
import { syncCrossoverBandSelects } from '../scene/speaker-band-select.js';
import {
  renderSpeakerEditor, renderSpeakersList, requestAddSpeaker, requestMoveSpeaker, requestRemoveSpeaker,
  applySpeakerCartesianEdit, applySpeakerPolarEdit, applySpeakerSceneCartesianEdit,
  setSpeakerSpatializeLocal, setSpeakerCoordMode, updateSpeakerLayoutPatch, sendSpeakersPatch,
  updateSpeakerVisualsFromState, updateSpeakerGizmo, updateControlsForEditMode,
  computeAndApplySpeakerDelays, adjustSpeakerDistancesFromDelays,
  samplesToDelayMs
} from '../speakers.js';
import { applySpeakerGroupGains } from '../mute-solo.js';

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
  const speakerEditXMetersInputEl = document.getElementById('speakerEditXMetersInput');
  const speakerEditYMetersInputEl = document.getElementById('speakerEditYMetersInput');
  const speakerEditZMetersInputEl = document.getElementById('speakerEditZMetersInput');
  const speakerEditAzInputEl = document.getElementById('speakerEditAzInput');
  const speakerEditElInputEl = document.getElementById('speakerEditElInput');
  const speakerEditRInputEl = document.getElementById('speakerEditRInput');
  const speakerEditRMetersInputEl = document.getElementById('speakerEditRMetersInput');
  const speakerEditSpatializeToggleEl = document.getElementById('speakerEditSpatializeToggle');
  const speakerEditFreqLowInputEl = document.getElementById('speakerEditFreqLowInput');
  const speakerEditFreqHighInputEl = document.getElementById('speakerEditFreqHighInput');
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
      applySpeakerGroupGains();
      renderSpeakerEditor();
    });
    speakerEditGainSliderEl.addEventListener('dblclick', () => {
      if (isSpeakerLayoutFrozen()) return;
      if (app.selectedSpeakerIndex === null) return;
      speakerEditGainSliderEl.value = '1';
      const id = String(app.selectedSpeakerIndex);
      speakerBaseGains.set(id, 1);
      applySpeakerGroupGains();
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
      sendSpeakersPatch({ speakerEdits: [{ id: Number(id), delayMs: value }] });
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
      sendSpeakersPatch({ speakerEdits: [{ id: Number(id), delayMs }] });
      renderSpeakerEditor();
    });
  }

  if (speakerEditAutoDelayBtnEl) {
    speakerEditAutoDelayBtnEl.addEventListener('click', () => {
      if (isSpeakerLayoutFrozen()) return;
      // Bulk action: overwrites every speaker's delay. Confirm first.
      if (!window.confirm(t('confirm.calcDelays'))) return;
      computeAndApplySpeakerDelays();
    });
  }

  if (speakerEditDelayToDistanceBtnEl) {
    speakerEditDelayToDistanceBtnEl.addEventListener('click', () => {
      if (isSpeakerLayoutFrozen()) return;
      // Bulk action: moves every speaker (overwrites positions). Confirm first.
      if (!window.confirm(t('confirm.delayToDist'))) return;
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
      updateSpeakerLayoutPatch(app.selectedSpeakerIndex, { name: nextName }, { apply: true });
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

  // Read only the field that fired; pull the other axes from the canonical
  // speaker state. Reading them back from the DOM would re-inject the
  // 3-decimal-rounded display value and drift the untouched axes on every edit.
  bindSpeakerCoordChange(speakerEditXInputEl, (idx) => {
    const speaker = app.currentLayoutSpeakers[idx];
    if (!speaker) return;
    const x = Number(speakerEditXInputEl?.value);
    if (!Number.isFinite(x)) return;
    applySpeakerCartesianEdit(idx, x, Number(speaker.y) || 0, Number(speaker.z) || 0, true);
  });

  bindSpeakerCoordChange(speakerEditYInputEl, (idx) => {
    const speaker = app.currentLayoutSpeakers[idx];
    if (!speaker) return;
    const y = Number(speakerEditYInputEl?.value);
    if (!Number.isFinite(y)) return;
    applySpeakerCartesianEdit(idx, Number(speaker.x) || 0, y, Number(speaker.z) || 0, true);
  });

  bindSpeakerCoordChange(speakerEditZInputEl, (idx) => {
    const speaker = app.currentLayoutSpeakers[idx];
    if (!speaker) return;
    const z = Number(speakerEditZInputEl?.value);
    if (!Number.isFinite(z)) return;
    applySpeakerCartesianEdit(idx, Number(speaker.x) || 0, Number(speaker.y) || 0, z, true);
  });

  // Real-world metres mirror the cartesian X/Y/Z above. The edited axis comes
  // from the field that fired; the other two are pulled from the canonical
  // speaker state (converted to metres), never re-read from the DOM, so they
  // don't drift on the display rounding — same rule as the normalized inputs.
  function bindSpeakerMetersChange(inputEl, axis) {
    bindSpeakerCoordChange(inputEl, (idx) => {
      const speaker = app.currentLayoutSpeakers[idx];
      if (!speaker) return;
      const value = Number(inputEl?.value);
      if (!Number.isFinite(value)) return;
      const meters = normalizedToMeters(speaker);
      meters[axis] = value;
      const scn = metersToSceneUnits(meters);
      applySpeakerSceneCartesianEdit(idx, scn.x, scn.y, scn.z, true);
    });
  }

  bindSpeakerMetersChange(speakerEditXMetersInputEl, 'x');
  bindSpeakerMetersChange(speakerEditYMetersInputEl, 'y');
  bindSpeakerMetersChange(speakerEditZMetersInputEl, 'z');

  bindSpeakerCoordChange(speakerEditAzInputEl, (idx) => {
    const speaker = app.currentLayoutSpeakers[idx];
    if (!speaker) return;
    const az = Number(speakerEditAzInputEl?.value);
    if (!Number.isFinite(az)) return;
    applySpeakerPolarEdit(
      idx,
      az,
      Number(speaker.elevationDeg) || 0,
      Number(speaker.distanceM) || 0.01,
      true,
    );
  });

  bindSpeakerCoordChange(speakerEditElInputEl, (idx) => {
    const speaker = app.currentLayoutSpeakers[idx];
    if (!speaker) return;
    const el = Number(speakerEditElInputEl?.value);
    if (!Number.isFinite(el)) return;
    applySpeakerPolarEdit(
      idx,
      Number(speaker.azimuthDeg) || 0,
      el,
      Number(speaker.distanceM) || 0.01,
      true,
    );
  });

  bindSpeakerCoordChange(speakerEditRInputEl, (idx) => {
    const speaker = app.currentLayoutSpeakers[idx];
    if (!speaker) return;
    const r = Number(speakerEditRInputEl?.value);
    if (!Number.isFinite(r)) return;
    applySpeakerPolarEdit(
      idx,
      Number(speaker.azimuthDeg) || 0,
      Number(speaker.elevationDeg) || 0,
      r,
      true,
    );
  });

  // Real-world distance in metres: convert back to a scene-space radius
  // (metres / metersPerUnit) and keep the current azimuth/elevation.
  bindSpeakerCoordChange(speakerEditRMetersInputEl, (idx) => {
    const speaker = app.currentLayoutSpeakers[idx];
    if (!speaker) return;
    const rMeters = Number(speakerEditRMetersInputEl?.value);
    if (!Number.isFinite(rMeters)) return;
    applySpeakerPolarEdit(
      idx,
      Number(speaker.azimuthDeg) || 0,
      Number(speaker.elevationDeg) || 0,
      rMeters / metersPerUnit(),
      true,
    );
  });

  if (speakerEditSpatializeToggleEl) {
    speakerEditSpatializeToggleEl.addEventListener('change', () => {
      if (isSpeakerLayoutFrozen()) return;
      if (app.selectedSpeakerIndex === null) return;
      const index = app.selectedSpeakerIndex;
      const nextSpatialize = speakerEditSpatializeToggleEl.checked ? 1 : 0;
      setSpeakerSpatializeLocal(index, nextSpatialize);
      updateSpeakerLayoutPatch(index, { spatialize: nextSpatialize !== 0 }, { apply: true });
      renderSpeakerEditor();
    });
  }

  function bindSpeakerFrequencyInput(inputEl, field) {
    if (!inputEl) return;
    let skipNextChange = false;

    const applyFrequency = () => {
      if (isSpeakerLayoutFrozen()) return;
      if (app.selectedSpeakerIndex === null) return;
      const index = app.selectedSpeakerIndex;
      const raw = inputEl.value.trim();
      const freq = raw === '' ? 0 : Math.max(0, Number(raw));
      const speaker = app.currentLayoutSpeakers[index];
      if (speaker) {
        speaker[field] = Number.isFinite(freq) && freq > 0 ? freq : null;
      }
      updateSpeakerLayoutPatch(
        index,
        { [field]: Number.isFinite(freq) && freq > 0 ? freq : null },
        { apply: true }
      );
      syncCrossoverBandSelects();
      renderSpeakerEditor();
      // Refresh the speaker list so its per-speaker filter icon reflects the new
      // low/high cutoff immediately.
      renderSpeakersList();
    };

    inputEl.addEventListener('change', () => {
      if (skipNextChange) {
        skipNextChange = false;
        return;
      }
      applyFrequency();
    });
    inputEl.addEventListener('keydown', (event) => {
      if (event.key !== 'Enter') return;
      event.preventDefault();
      applyFrequency();
      skipNextChange = true;
      inputEl.blur();
    });
  }

  bindSpeakerFrequencyInput(speakerEditFreqLowInputEl, 'freqLow');
  bindSpeakerFrequencyInput(speakerEditFreqHighInputEl, 'freqHigh');

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
