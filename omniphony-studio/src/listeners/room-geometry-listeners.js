import { app } from '../state.js';
import {
  persistRoomGeometryPrefs, getRoomCenterBlendFromInput, renderRoomCenterBlendControl,
  normalizeRoomGeometryInputDisplays, updateRoomGeometryButtonsState,
  applyRoomGeometryNow, scheduleRoomGeometryApply, applyRoomGeometryStateToInputs,
  updateRoomGeometryLivePreview, refreshRoomGeometryInputState, setRoomGeometryExpanded,
  getRoomDriverValue
} from '../controls/room-geometry.js';

export function setupRoomGeometryListeners() {
  const roomGeometryToggleBtnEl = document.getElementById('roomGeometryToggleBtn');
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

  if (roomGeometryToggleBtnEl) {
    roomGeometryToggleBtnEl.addEventListener('click', () => {
      setRoomGeometryExpanded(!app.roomGeometryExpanded);
    });
  }

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
}
