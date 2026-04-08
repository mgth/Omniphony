/**
 * Room geometry panel controls.
 *
 * Extracted from app.js (lines 3113-3660).
 */

import * as THREE from 'three';
import { app, dirty, layoutsByKey, sourceMeshes, sourcePositionsRaw, speakerMeshes, speakerLabels } from '../state.js';
import { formatNumber, normalizedOmniphonyToScenePosition, hydrateSpeakerCoordinateState } from '../coordinates.js';
import { scheduleUIFlush } from '../flush.js';
import { flushCallbacks } from '../flush.js';
import { invoke } from '@tauri-apps/api/core';
import { roomDimensionGroup, roomBounds, roomGroup, room, roomEdges, roomFaces, fitScreenToUpperHalf } from '../scene/setup.js';
import { updateVbapCartesianFaceGrid } from '../scene/gizmos.js';
import { updateSourceDecorations } from '../sources.js';
import { rebuildTrailGeometry } from '../trails.js';
import { renderSpeakerEditor } from '../speakers.js';
import { inDisplayPanel, inRoomGeometryPanel, roomGeometryPanelQueryAll } from '../ui/panel-roots.js';

const ROOM_GEOM_PREFS_STORAGE_KEY = 'spatialviz.room_geometry_prefs';
const TRAIL_PREFS_STORAGE_KEY = 'spatialviz.trail_prefs';
const EFFECTIVE_RENDER_PREFS_STORAGE_KEY = 'spatialviz.effective_render_prefs';

// DOM refs
const roomGeometrySummaryEl = inRoomGeometryPanel('roomGeometrySummary');
const roomGeometrySummaryScaleEl = inRoomGeometryPanel('roomGeometrySummaryScale');
const roomGeometrySummarySizeEl = inRoomGeometryPanel('roomGeometrySummarySize');
const roomGeometrySummaryRatioEl = inRoomGeometryPanel('roomGeometrySummaryRatio');
const roomDimWidthInputEl = inRoomGeometryPanel('roomDimWidthInput');
const roomDimLengthInputEl = inRoomGeometryPanel('roomDimLengthInput');
const roomDimHeightInputEl = inRoomGeometryPanel('roomDimHeightInput');
const roomDimRearInputEl = inRoomGeometryPanel('roomDimRearInput');
const roomDimLowerInputEl = inRoomGeometryPanel('roomDimLowerInput');
const roomRatioWidthInputEl = inRoomGeometryPanel('roomRatioWidthInput');
const roomRatioLengthInputEl = inRoomGeometryPanel('roomRatioLengthInput');
const roomRatioHeightInputEl = inRoomGeometryPanel('roomRatioHeightInput');
const roomRatioRearInputEl = inRoomGeometryPanel('roomRatioRearInput');
const roomRatioLowerInputEl = inRoomGeometryPanel('roomRatioLowerInput');
const roomRatioCenterBlendSliderEl = inRoomGeometryPanel('roomRatioCenterBlendSlider');
const roomRatioCenterBlendValueEl = inRoomGeometryPanel('roomRatioCenterBlendValue');
const roomMasterAxisInputs = roomGeometryPanelQueryAll('input[name="roomMasterAxis"]');
const roomDriverWidthEl = inRoomGeometryPanel('roomDriverWidth');
const roomDriverLengthEl = inRoomGeometryPanel('roomDriverLength');
const roomDriverHeightEl = inRoomGeometryPanel('roomDriverHeight');
const roomDriverRearEl = inRoomGeometryPanel('roomDriverRear');
const roomDriverLowerEl = inRoomGeometryPanel('roomDriverLower');
const roomMasterMpuWidthEl = inRoomGeometryPanel('roomMasterMpuWidth');
const roomMasterMpuLengthEl = inRoomGeometryPanel('roomMasterMpuLength');
const roomMasterMpuRearEl = inRoomGeometryPanel('roomMasterMpuRear');
const roomMasterMpuHeightEl = inRoomGeometryPanel('roomMasterMpuHeight');
const roomMasterMpuLowerEl = inRoomGeometryPanel('roomMasterMpuLower');
const roomGeometryCancelBtnEl = inRoomGeometryPanel('roomGeometryCancelBtn');
const trailToggleEl = inDisplayPanel('trailToggle');
const trailModeSelectEl = inDisplayPanel('trailModeSelect');
const trailTtlSliderEl = inDisplayPanel('trailTtlSlider');
const trailTtlValEl = inDisplayPanel('trailTtlVal');
const effectiveRenderToggleEl = inDisplayPanel('effectiveRenderToggle');
const objectColorsToggleEl = inDisplayPanel('objectColorsToggle');
const speakerHeatmapSlicesToggleEl = inDisplayPanel('speakerHeatmapSlicesToggle');
const speakerHeatmapVolumeToggleEl = inDisplayPanel('speakerHeatmapVolumeToggle');
const speakerHeatmapSampleCountInputEl = inDisplayPanel('speakerHeatmapSampleCountInput');
const speakerHeatmapMaxSphereSizeSliderEl = inDisplayPanel('speakerHeatmapMaxSphereSizeSlider');
const speakerHeatmapMaxSphereSizeValEl = inDisplayPanel('speakerHeatmapMaxSphereSizeVal');

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function roomAxisFactor(axis) {
  return axis === 'width' ? 2 : 1;
}

export function persistRoomGeometryPrefs() {
  try {
    const payload = {
      master: app.roomMasterAxis,
      drivers: {
        width: app.roomAxisDrivers.width === 'ratio' ? 'ratio' : 'size',
        length: app.roomAxisDrivers.length === 'ratio' ? 'ratio' : 'size',
        height: app.roomAxisDrivers.height === 'ratio' ? 'ratio' : 'size',
        rear: app.roomAxisDrivers.rear === 'ratio' ? 'ratio' : 'size',
        lower: app.roomAxisDrivers.lower === 'ratio' ? 'ratio' : 'size'
      }
    };
    localStorage.setItem(ROOM_GEOM_PREFS_STORAGE_KEY, JSON.stringify(payload));
  } catch (_e) {
    // Ignore storage errors (private mode, quota, etc.).
  }
}

export function loadRoomGeometryPrefs() {
  try {
    const raw = localStorage.getItem(ROOM_GEOM_PREFS_STORAGE_KEY);
    if (!raw) return;
    const parsed = JSON.parse(raw);
    const axes = ['width', 'length', 'height', 'rear', 'lower'];
    if (axes.includes(parsed?.master)) {
      app.roomMasterAxis = parsed.master;
    }
    const drivers = parsed?.drivers || {};
    axes.forEach((axis) => {
      app.roomAxisDrivers[axis] = drivers[axis] === 'ratio' ? 'ratio' : 'size';
    });
  } catch (_e) {
    // Ignore malformed payloads.
  }
}

export function persistTrailPrefs() {
  try {
    const payload = {
      enabled: app.trailsEnabled,
      mode: app.trailRenderMode === 'line' ? 'line' : 'diffuse',
      duration_ms: app.trailPointTtlMs
    };
    localStorage.setItem(TRAIL_PREFS_STORAGE_KEY, JSON.stringify(payload));
  } catch (_e) {
    // Ignore storage errors (private mode, quota, etc.).
  }
}

export function persistEffectiveRenderPrefs() {
  try {
    localStorage.setItem(EFFECTIVE_RENDER_PREFS_STORAGE_KEY, JSON.stringify({
      enabled: app.effectiveRenderEnabled,
      objectColors: app.objectColorsEnabled,
      speakerHeatmapSlicesEnabled: app.speakerHeatmapSlicesEnabled,
      speakerHeatmapVolumeEnabled: app.speakerHeatmapVolumeEnabled,
      speakerHeatmapSampleCount: app.speakerHeatmapSampleCount,
      speakerHeatmapMaxSphereSize: app.speakerHeatmapMaxSphereSize
    }));
  } catch (_e) {
    // Ignore storage errors (private mode, quota, etc.).
  }
}

export function applyTrailPrefsToUi() {
  if (trailToggleEl) {
    trailToggleEl.checked = app.trailsEnabled;
  }
  if (trailModeSelectEl) {
    trailModeSelectEl.value = app.trailRenderMode;
  }
  if (trailTtlSliderEl) {
    trailTtlSliderEl.value = (app.trailPointTtlMs / 1000).toFixed(1);
  }
  if (trailTtlValEl) {
    trailTtlValEl.textContent = `${(app.trailPointTtlMs / 1000).toFixed(1)}s`;
  }
}

export function applyEffectiveRenderPrefsToUi() {
  if (effectiveRenderToggleEl) {
    effectiveRenderToggleEl.checked = app.effectiveRenderEnabled;
  }
  if (objectColorsToggleEl) {
    objectColorsToggleEl.checked = app.objectColorsEnabled;
  }
  if (speakerHeatmapSlicesToggleEl) {
    speakerHeatmapSlicesToggleEl.checked = app.speakerHeatmapSlicesEnabled;
  }
  if (speakerHeatmapVolumeToggleEl) {
    speakerHeatmapVolumeToggleEl.checked = app.speakerHeatmapVolumeEnabled;
  }
  if (speakerHeatmapSampleCountInputEl) {
    speakerHeatmapSampleCountInputEl.value = String(app.speakerHeatmapSampleCount);
  }
  if (speakerHeatmapMaxSphereSizeSliderEl) {
    speakerHeatmapMaxSphereSizeSliderEl.value = String(app.speakerHeatmapMaxSphereSize);
  }
  if (speakerHeatmapMaxSphereSizeValEl) {
    speakerHeatmapMaxSphereSizeValEl.textContent = app.speakerHeatmapMaxSphereSize.toFixed(3);
  }
}

export function loadTrailPrefs() {
  try {
    const raw = localStorage.getItem(TRAIL_PREFS_STORAGE_KEY);
    if (!raw) {
      applyTrailPrefsToUi();
      return;
    }
    const parsed = JSON.parse(raw);
    app.trailsEnabled = Boolean(parsed?.enabled);
    app.trailRenderMode = parsed?.mode === 'line' ? 'line' : 'diffuse';
    const durationMs = Number(parsed?.duration_ms);
    if (Number.isFinite(durationMs)) {
      app.trailPointTtlMs = Math.max(500, durationMs);
    }
  } catch (_e) {
    // Ignore malformed payloads.
  }
  applyTrailPrefsToUi();
}

export function loadEffectiveRenderPrefs() {
  try {
    const raw = localStorage.getItem(EFFECTIVE_RENDER_PREFS_STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw);
      app.effectiveRenderEnabled = Boolean(parsed?.enabled);
      app.objectColorsEnabled = Boolean(parsed?.objectColors);
      if (typeof parsed?.speakerHeatmapSlicesEnabled === 'boolean') {
        app.speakerHeatmapSlicesEnabled = parsed.speakerHeatmapSlicesEnabled;
      }
      if (typeof parsed?.speakerHeatmapVolumeEnabled === 'boolean') {
        app.speakerHeatmapVolumeEnabled = parsed.speakerHeatmapVolumeEnabled;
      }
      const sampleCount = Number(parsed?.speakerHeatmapSampleCount);
      if (Number.isFinite(sampleCount)) {
        app.speakerHeatmapSampleCount = Math.max(128, Math.min(20000, Math.round(sampleCount)));
      }
      const maxSphereSize = Number(parsed?.speakerHeatmapMaxSphereSize);
      if (Number.isFinite(maxSphereSize)) {
        app.speakerHeatmapMaxSphereSize = Math.max(0.01, Math.min(0.2, maxSphereSize));
      }
    }
  } catch (_e) {
    // Ignore malformed payloads.
  }
  applyEffectiveRenderPrefsToUi();
}

export function refreshEffectiveRenderVisibility() {
  if (typeof flushCallbacks.refreshEffectiveRenderVisibility === 'function') {
    flushCallbacks.refreshEffectiveRenderVisibility();
  }
}

function getRoomDriverEl(axis) {
  if (axis === 'width') return roomDriverWidthEl;
  if (axis === 'length') return roomDriverLengthEl;
  if (axis === 'height') return roomDriverHeightEl;
  if (axis === 'rear') return roomDriverRearEl;
  if (axis === 'lower') return roomDriverLowerEl;
  return null;
}

export function getRoomDriverValue(axis) {
  const el = getRoomDriverEl(axis);
  return el?.checked ? 'ratio' : 'size';
}

function setRoomDriverValue(axis, value) {
  const el = getRoomDriverEl(axis);
  if (!el) return;
  el.checked = value === 'ratio';
}

export function getRoomSizeInputEl(axis) {
  if (axis === 'width') return roomDimWidthInputEl;
  if (axis === 'length') return roomDimLengthInputEl;
  if (axis === 'height') return roomDimHeightInputEl;
  if (axis === 'rear') return roomDimRearInputEl;
  if (axis === 'lower') return roomDimLowerInputEl;
  return null;
}

export function getRoomRatioInputEl(axis) {
  if (axis === 'width') return roomRatioWidthInputEl;
  if (axis === 'length') return roomRatioLengthInputEl;
  if (axis === 'height') return roomRatioHeightInputEl;
  if (axis === 'rear') return roomRatioRearInputEl;
  if (axis === 'lower') return roomRatioLowerInputEl;
  return null;
}

function roundRoomGeom(value) {
  const n = Number(value);
  if (!Number.isFinite(n)) return 0;
  return Math.round(n * 1e6) / 1e6;
}

export function getRoomCenterBlendFromInput(fallback = app.roomRatio.centerBlend) {
  const n = Number(roomRatioCenterBlendSliderEl?.value);
  const fallbackNum = Number(fallback);
  if (!Number.isFinite(n)) return Number.isFinite(fallbackNum) ? fallbackNum : 0.5;
  return Math.max(0, Math.min(1, n / 100));
}

export function renderRoomCenterBlendControl(value = app.roomRatio.centerBlend) {
  const parsed = Number(value);
  const blend = Math.max(0, Math.min(1, Number.isFinite(parsed) ? parsed : 0.5));
  if (roomRatioCenterBlendSliderEl) {
    roomRatioCenterBlendSliderEl.value = String(Math.round(blend * 100));
  }
  if (roomRatioCenterBlendValueEl) {
    roomRatioCenterBlendValueEl.textContent = `${Math.round(blend * 100)}/${Math.round((1 - blend) * 100)}`;
  }
}

export function roomGeometryStateFromInputs() {
  const axes = ['width', 'length', 'height', 'rear', 'lower'];
  const preview = computeRoomGeometryFromInputs();
  const state = {
    mpu: roundRoomGeom(preview.mpu),
    master: app.roomMasterAxis,
    centerBlend: roundRoomGeom(getRoomCenterBlendFromInput()),
    drivers: {},
    size: {},
    ratio: {}
  };
  axes.forEach((axis) => {
    state.drivers[axis] = app.roomAxisDrivers[axis] === 'ratio' ? 'ratio' : 'size';
    state.size[axis] = roundRoomGeom(getRoomSizeInputEl(axis)?.value);
    state.ratio[axis] = roundRoomGeom(getRoomRatioInputEl(axis)?.value);
  });
  return state;
}

export function normalizeRoomGeometryInputDisplays() {
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
    const n = Number(el.value);
    if (!Number.isFinite(n)) return;
    el.value = formatNumber(n, 2);
  });
}

export function roomGeometryStateKey(state) {
  const s = state || roomGeometryStateFromInputs();
  return JSON.stringify({
    mpu: s.mpu,
    master: s.master,
    centerBlend: s.centerBlend,
    drivers: s.drivers,
    size: s.size,
    ratio: s.ratio
  });
}

export function updateRoomGeometryButtonsState() {
  const currentKey = roomGeometryStateKey();
  const unchanged = app.roomGeometryBaselineKey !== '' && currentKey === app.roomGeometryBaselineKey;
  if (roomGeometryCancelBtnEl) {
    roomGeometryCancelBtnEl.disabled = unchanged;
    roomGeometryCancelBtnEl.style.opacity = unchanged ? '0.55' : '1';
    roomGeometryCancelBtnEl.style.cursor = unchanged ? 'default' : 'pointer';
  }
}

export function applyRoomGeometryNow() {
  const preview = computeRoomGeometryFromInputs();
  app.roomMasterAxis = preview.master;
  const width = preview.ratio.width;
  const length = preview.ratio.length;
  const height = preview.ratio.height;
  const rear = preview.ratio.rear;
  const lower = preview.ratio.lower;
  const centerBlend = getRoomCenterBlendFromInput();
  const mpu = preview.mpu;

  app.metersPerUnit = mpu;
  const layout = app.currentLayoutKey ? layoutsByKey.get(app.currentLayoutKey) : null;
  if (layout) {
    layout.radius_m = mpu;
  }

  applyRoomRatio({ width, length, height, rear, lower, centerBlend });
  invoke('control_layout_radius_m', { value: mpu });
  invoke('control_room_ratio_center_blend', { value: centerBlend });
  invoke('control_room_ratio', { width, length, height });
  invoke('control_room_ratio_rear', { value: rear });
  invoke('control_room_ratio_lower', { value: lower });
  renderSpeakerEditor();
  normalizeRoomGeometryInputDisplays();
  setRoomGeometryBaselineFromInputs();
}

export function scheduleRoomGeometryApply(delayMs = 120) {
  if (app.roomGeometryApplyTimer !== null) {
    clearTimeout(app.roomGeometryApplyTimer);
  }
  app.roomGeometryApplyTimer = window.setTimeout(() => {
    app.roomGeometryApplyTimer = null;
    applyRoomGeometryNow();
  }, delayMs);
}

export function setRoomGeometryBaselineFromInputs() {
  app.roomGeometryBaselineKey = roomGeometryStateKey();
  updateRoomGeometryButtonsState();
}

export function renderRoomGeometrySummary(preview = null) {
  if (!roomGeometrySummaryEl) return;
  const metersPerUnit = app.metersPerUnit ?? 1;
  const ratioWidth = Number(preview?.ratio?.width ?? app.roomRatio.width) || 1;
  const ratioLength = Number(preview?.ratio?.length ?? app.roomRatio.length) || 1;
  const ratioRear = Number(preview?.ratio?.rear ?? app.roomRatio.rear) || 1;
  const ratioHeight = Number(preview?.ratio?.height ?? app.roomRatio.height) || 1;
  const ratioLower = Number(preview?.ratio?.lower ?? app.roomRatio.lower) || 0.5;
  const mpuValue = Number(preview?.mpu ?? metersPerUnit) || 1;
  const sizeWidth = ratioWidth * mpuValue * 2;
  const sizeFront = ratioLength * mpuValue;
  const sizeRear = ratioRear * mpuValue;
  const sizeHeight = ratioHeight * mpuValue;
  const sizeLower = ratioLower * mpuValue;

  if (roomGeometrySummaryScaleEl) {
    roomGeometrySummaryScaleEl.textContent = `m/u: ${formatNumber(mpuValue, 2)}`;
  }
  if (roomGeometrySummarySizeEl) {
    roomGeometrySummarySizeEl.textContent =
      `X: ${formatNumber(sizeWidth, 2)}m | Y+: ${formatNumber(sizeFront, 2)}m | Y-: ${formatNumber(sizeRear, 2)}m | Z+: ${formatNumber(sizeHeight, 2)}m | Z-: ${formatNumber(sizeLower, 2)}m`;
  }
  if (roomGeometrySummaryRatioEl) {
    roomGeometrySummaryRatioEl.textContent =
      `X: ${formatNumber(ratioWidth, 2)} | Y+: ${formatNumber(ratioLength, 2)} | Y-: ${formatNumber(ratioRear, 2)} | Z+: ${formatNumber(ratioHeight, 2)} | Z-: ${formatNumber(ratioLower, 2)}`;
  }
}

export function applyRoomGeometryStateToInputs(state) {
  if (!state) return;
  const axes = ['width', 'length', 'height', 'rear', 'lower'];
  app.roomMasterAxis = axes.includes(state.master) ? state.master : app.roomMasterAxis;
  axes.forEach((axis) => {
    app.roomAxisDrivers[axis] = state.drivers?.[axis] === 'ratio' ? 'ratio' : 'size';
    const sizeEl = getRoomSizeInputEl(axis);
    const ratioEl = getRoomRatioInputEl(axis);
    if (sizeEl && Number.isFinite(state.size?.[axis])) sizeEl.value = String(state.size[axis]);
    if (ratioEl && Number.isFinite(state.ratio?.[axis])) ratioEl.value = String(state.ratio[axis]);
  });
  const centerBlend = Number.isFinite(state.centerBlend) ? state.centerBlend : app.roomRatio.centerBlend;
  renderRoomCenterBlendControl(centerBlend);
  normalizeRoomGeometryInputDisplays();
  refreshRoomGeometryInputState();
  updateRoomGeometryButtonsState();
}

export function computeRoomGeometryFromInputs() {
  const axes = ['width', 'length', 'height', 'rear', 'lower'];
  const metersPerUnit = app.metersPerUnit ?? 1;
  const safeNumber = (value, fallback, min = 0.01) => {
    const n = Number(value);
    if (!Number.isFinite(n)) return fallback;
    return Math.max(min, n);
  };

  const inputData = {};
  axes.forEach((axis) => {
    const ratioNow = axis === 'width' ? app.roomRatio.width
      : axis === 'length' ? app.roomRatio.length
        : axis === 'height' ? app.roomRatio.height
          : axis === 'rear' ? app.roomRatio.rear
            : app.roomRatio.lower;
    const defaultSize = ratioNow * metersPerUnit * roomAxisFactor(axis);
    const sizeEl = getRoomSizeInputEl(axis);
    const ratioEl = getRoomRatioInputEl(axis);
    inputData[axis] = {
      size: safeNumber(sizeEl?.value, Math.max(0.01, defaultSize)),
      ratio: safeNumber(ratioEl?.value, Math.max(0.01, ratioNow))
    };
  });

  let master = app.roomMasterAxis;
  if (!axes.includes(master)) master = 'width';

  const masterRatio = inputData[master].ratio;
  const masterSize = inputData[master].size;
  const masterFactor = roomAxisFactor(master);
  const mpu = safeNumber(masterSize / Math.max(0.01, masterRatio * masterFactor), Number(metersPerUnit) || 1);

  const ratios = {};
  axes.forEach((axis) => {
    if (axis === master) {
      ratios[axis] = masterRatio;
      return;
    }
    const driver = app.roomAxisDrivers[axis] === 'ratio' ? 'ratio' : 'size';
    if (driver === 'ratio') {
      ratios[axis] = inputData[axis].ratio;
    } else {
      ratios[axis] = safeNumber(inputData[axis].size / Math.max(0.01, mpu * roomAxisFactor(axis)), 1);
    }
  });

  return {
    master,
    mpu,
    ratio: {
      width: ratios.width,
      length: ratios.length,
      height: ratios.height,
      rear: ratios.rear,
      lower: ratios.lower
    },
    size: {
      width: ratios.width * mpu * roomAxisFactor('width'),
      length: ratios.length * mpu * roomAxisFactor('length'),
      height: ratios.height * mpu * roomAxisFactor('height'),
      rear: ratios.rear * mpu * roomAxisFactor('rear'),
      lower: ratios.lower * mpu * roomAxisFactor('lower')
    }
  };
}

export function updateRoomGeometryLivePreview() {
  const preview = computeRoomGeometryFromInputs();
  const axes = ['width', 'length', 'height', 'rear', 'lower'];
  axes.forEach((axis) => {
    const isMaster = axis === app.roomMasterAxis;
    const driver = app.roomAxisDrivers[axis] === 'ratio' ? 'ratio' : 'size';
    const sizeEditable = isMaster || driver === 'size';
    const ratioEditable = isMaster || driver === 'ratio';
    const sizeEl = getRoomSizeInputEl(axis);
    const ratioEl = getRoomRatioInputEl(axis);
    if (!sizeEditable && sizeEl) sizeEl.value = formatNumber(preview.size[axis], 2);
    if (!ratioEditable && ratioEl) ratioEl.value = formatNumber(preview.ratio[axis], 2);
  });
  renderRoomGeometryMasterMpu(preview);
  renderRoomGeometrySummary(preview);
  updateRoomDimensionGuides(preview);
}

export function renderRoomGeometryMasterMpu(preview = null) {
  const metersPerUnit = app.metersPerUnit ?? 1;
  const mpuValue = Number(preview?.mpu ?? metersPerUnit) || 1;
  const text = `m/u ${formatNumber(mpuValue, 2)}`;
  const values = {
    width: app.roomMasterAxis === 'width' ? text : '',
    length: app.roomMasterAxis === 'length' ? text : '',
    rear: app.roomMasterAxis === 'rear' ? text : '',
    height: app.roomMasterAxis === 'height' ? text : '',
    lower: app.roomMasterAxis === 'lower' ? text : ''
  };
  if (roomMasterMpuWidthEl) roomMasterMpuWidthEl.textContent = values.width;
  if (roomMasterMpuLengthEl) roomMasterMpuLengthEl.textContent = values.length;
  if (roomMasterMpuRearEl) roomMasterMpuRearEl.textContent = values.rear;
  if (roomMasterMpuHeightEl) roomMasterMpuHeightEl.textContent = values.height;
  if (roomMasterMpuLowerEl) roomMasterMpuLowerEl.textContent = values.lower;
}

function setRoomFieldEditable(inputEl, editable) {
  if (!inputEl) return;
  inputEl.readOnly = !editable;
  inputEl.tabIndex = editable ? 0 : -1;
  inputEl.style.pointerEvents = editable ? 'auto' : 'none';
  inputEl.classList.toggle('derived-field', !editable);
  inputEl.style.background = editable ? 'rgba(255,255,255,0.08)' : 'transparent';
  inputEl.style.border = editable ? '1px solid rgba(255,255,255,0.2)' : '1px solid transparent';
  inputEl.style.color = editable ? '#dfe8f3' : 'rgba(223,232,243,0.88)';
  inputEl.style.boxShadow = 'none';
}

function syncRoomMasterAxisUI() {
  roomMasterAxisInputs.forEach((input) => {
    input.checked = input.value === app.roomMasterAxis;
  });
}

export function refreshRoomGeometryInputState() {
  const axes = ['width', 'length', 'height', 'rear', 'lower'];
  syncRoomMasterAxisUI();

  axes.forEach((axis) => {
    const isMaster = axis === app.roomMasterAxis;
    const sizeEl = getRoomSizeInputEl(axis);
    const ratioEl = getRoomRatioInputEl(axis);
    const driverEl = getRoomDriverEl(axis);
    const driver = app.roomAxisDrivers[axis] === 'ratio' ? 'ratio' : 'size';

    if (driverEl) {
      setRoomDriverValue(axis, driver);
      driverEl.disabled = isMaster;
    }
    const sizeEditable = isMaster || driver === 'size';
    const ratioEditable = isMaster || driver === 'ratio';
    setRoomFieldEditable(sizeEl, sizeEditable);
    setRoomFieldEditable(ratioEl, ratioEditable);
  });
  updateRoomGeometryLivePreview();
  updateRoomGeometryButtonsState();
}

export function renderRoomRatioDisplay() {
  const metersPerUnit = app.metersPerUnit ?? 1;
  const dimW = app.roomRatio.width * metersPerUnit * 2;
  const dimL = app.roomRatio.length * metersPerUnit;
  const dimH = app.roomRatio.height * metersPerUnit;
  const dimRear = app.roomRatio.rear * metersPerUnit;
  const dimLower = app.roomRatio.lower * metersPerUnit;
  if (roomDimWidthInputEl) roomDimWidthInputEl.value = formatNumber(dimW, 2);
  if (roomDimLengthInputEl) roomDimLengthInputEl.value = formatNumber(dimL, 2);
  if (roomDimHeightInputEl) roomDimHeightInputEl.value = formatNumber(dimH, 2);
  if (roomDimRearInputEl) roomDimRearInputEl.value = formatNumber(dimRear, 2);
  if (roomDimLowerInputEl) roomDimLowerInputEl.value = formatNumber(dimLower, 2);
  if (roomRatioWidthInputEl) roomRatioWidthInputEl.value = formatNumber(app.roomRatio.width, 2);
  if (roomRatioLengthInputEl) roomRatioLengthInputEl.value = formatNumber(app.roomRatio.length, 2);
  if (roomRatioHeightInputEl) roomRatioHeightInputEl.value = formatNumber(app.roomRatio.height, 2);
  if (roomRatioRearInputEl) roomRatioRearInputEl.value = formatNumber(app.roomRatio.rear, 2);
  if (roomRatioLowerInputEl) roomRatioLowerInputEl.value = formatNumber(app.roomRatio.lower, 2);
  renderRoomCenterBlendControl(app.roomRatio.centerBlend);
  renderRoomGeometryMasterMpu();
  renderRoomGeometrySummary();
  normalizeRoomGeometryInputDisplays();
  refreshRoomGeometryInputState();
  setRoomGeometryBaselineFromInputs();
}

export function updateRoomRatioDisplay() {
  dirty.roomRatio = true;
  scheduleUIFlush();
}

// ---------------------------------------------------------------------------
// Room geometry expansion toggle
// ---------------------------------------------------------------------------

export function setRoomGeometryExpanded(expanded) {
  app.roomGeometryExpanded = Boolean(expanded);
  const roomGeometryFormEl = inRoomGeometryPanel('roomGeometryForm');
  const roomGeometrySummaryEl = inRoomGeometryPanel('roomGeometrySummary');
  const roomGeometryToggleBtnEl = inRoomGeometryPanel('roomGeometryToggleBtn');
  if (roomGeometryFormEl) {
    roomGeometryFormEl.classList.toggle('open', app.roomGeometryExpanded);
  }
  if (roomGeometrySummaryEl) {
    roomGeometrySummaryEl.style.display = 'none';
  }
  if (roomGeometryToggleBtnEl) {
    roomGeometryToggleBtnEl.textContent = app.roomGeometryExpanded ? '\u25be' : '\u25b8';
  }
  roomDimensionGroup.visible = false;
}

// ---------------------------------------------------------------------------
// Room dimension guides (3D measurement overlays)
// ---------------------------------------------------------------------------

function createRoomDimensionGuide(color = 0x9dd3ff) {
  const line = new THREE.LineSegments(
    new THREE.BufferGeometry(),
    new THREE.LineBasicMaterial({ color, transparent: true, opacity: 0.85, depthTest: false })
  );
  line.renderOrder = 30;
  const group = new THREE.Group();
  group.add(line);
  roomDimensionGroup.add(group);
  return { group, line };
}

const roomDimensionGuides = {
  width: createRoomDimensionGuide(0x88c7ff),
  front: createRoomDimensionGuide(0xa0ffd1),
  rear: createRoomDimensionGuide(0xffd08a),
  total: createRoomDimensionGuide(0xb8b8ff),
  height: createRoomDimensionGuide(0xff9ed8),
  lower: createRoomDimensionGuide(0xff7a7a),
  totalHeight: createRoomDimensionGuide(0xffb3e6)
};

export function rebuildRoomDimensionGuideResources() {
  Object.values(roomDimensionGuides).forEach((guide) => {
    if (guide?.line?.material) {
      guide.line.material.needsUpdate = true;
    }
  });
}

function updateRoomDimensionGuide(guide, start, end, tickDir, _labelText) {
  const tick = tickDir.clone().normalize().multiplyScalar(0.04);
  const points = [
    start, end,
    start.clone().sub(tick), start.clone().add(tick),
    end.clone().sub(tick), end.clone().add(tick)
  ];
  guide.line.geometry.dispose();
  guide.line.geometry = new THREE.BufferGeometry().setFromPoints(points);
}

export function updateRoomDimensionGuides(preview = null) {
  const ratioWidth = Number(preview?.ratio?.width ?? app.roomRatio.width) || 1;
  const ratioLength = Number(preview?.ratio?.length ?? app.roomRatio.length) || 1;
  const ratioHeight = Number(preview?.ratio?.height ?? app.roomRatio.height) || 1;
  const ratioRear = Number(preview?.ratio?.rear ?? app.roomRatio.rear) || 1;
  const ratioLower = Number(preview?.ratio?.lower ?? app.roomRatio.lower) || 0.5;
  const mpuValue = Number(preview?.mpu ?? app.metersPerUnit) || 1;
  const xMin = roomBounds.xMin;
  const xMax = roomBounds.xMax;
  const yMin = roomBounds.yMin;
  const yMax = roomBounds.yMax;
  const zMin = roomBounds.zMin;
  const zMax = roomBounds.zMax;
  const yTop = yMax + 0.06;
  const off = 0.08;

  updateRoomDimensionGuide(
    roomDimensionGuides.width,
    new THREE.Vector3(xMax + off, yTop, zMin),
    new THREE.Vector3(xMax + off, yTop, zMax),
    new THREE.Vector3(1, 0, 0),
    `${formatNumber(ratioWidth * mpuValue * 2, 2)}m`
  );
  updateRoomDimensionGuide(
    roomDimensionGuides.front,
    new THREE.Vector3(0, yTop, zMax + off),
    new THREE.Vector3(xMax, yTop, zMax + off),
    new THREE.Vector3(0, 0, 1),
    `${formatNumber(ratioLength * mpuValue, 2)}m`
  );
  updateRoomDimensionGuide(
    roomDimensionGuides.rear,
    new THREE.Vector3(xMin, yTop, zMax + off),
    new THREE.Vector3(0, yTop, zMax + off),
    new THREE.Vector3(0, 0, 1),
    `${formatNumber(ratioRear * mpuValue, 2)}m`
  );
  updateRoomDimensionGuide(
    roomDimensionGuides.total,
    new THREE.Vector3(xMin, yTop, zMin - off),
    new THREE.Vector3(xMax, yTop, zMin - off),
    new THREE.Vector3(0, 0, 1),
    `${formatNumber((ratioLength + ratioRear) * mpuValue, 2)}m`
  );
  updateRoomDimensionGuide(
    roomDimensionGuides.height,
    new THREE.Vector3(xMax + off, 0, zMax + off),
    new THREE.Vector3(xMax + off, yMax, zMax + off),
    new THREE.Vector3(1, 0, 0),
    `${formatNumber(ratioHeight * mpuValue, 2)}m`
  );
  updateRoomDimensionGuide(
    roomDimensionGuides.lower,
    new THREE.Vector3(xMax + off, yMin, zMax + off),
    new THREE.Vector3(xMax + off, 0, zMax + off),
    new THREE.Vector3(1, 0, 0),
    `${formatNumber(ratioLower * mpuValue, 2)}m`
  );
  updateRoomDimensionGuide(
    roomDimensionGuides.totalHeight,
    new THREE.Vector3(xMax + off, yMin, zMin - off),
    new THREE.Vector3(xMax + off, yMax, zMin - off),
    new THREE.Vector3(1, 0, 0),
    `${formatNumber((ratioHeight + ratioLower) * mpuValue, 2)}m`
  );

  roomDimensionGroup.visible = false;
}

// ---------------------------------------------------------------------------
// Apply room ratio to 3D scene objects
// ---------------------------------------------------------------------------

export function applyRoomRatioToScene() {
  const xMax = Math.max(0.001, Number(app.roomRatio.length) || 1);
  const xMin = -Math.max(0.001, Number(app.roomRatio.rear) || 1);
  const yMax = Math.max(0.001, Number(app.roomRatio.height) || 1);
  const yMin = -Math.max(0.001, Number(app.roomRatio.lower) || 0.5);
  const halfZ = Math.max(0.001, Number(app.roomRatio.width) || 1);
  const depthHalfX = Math.max(0.001, (xMax - xMin) * 0.5);
  const xCenter = (xMin + xMax) * 0.5;
  const yCenter = (yMin + yMax) * 0.5;
  const totalHeight = yMax - yMin;

  roomBounds.xMin = xMin;
  roomBounds.xMax = xMax;
  roomBounds.yMin = yMin;
  roomBounds.yMax = yMax;
  roomBounds.zMin = -halfZ;
  roomBounds.zMax = halfZ;

  roomGroup.scale.set(1, 1, 1);

  room.scale.set(depthHalfX, totalHeight, halfZ);
  room.position.set(xCenter, yCenter, 0);
  roomEdges.scale.set(depthHalfX, totalHeight, halfZ);
  roomEdges.position.set(xCenter, yCenter, 0);

  roomFaces.posX.position.set(xMax, yCenter, 0);
  roomFaces.posX.scale.set(halfZ, totalHeight, 1);
  roomFaces.negX.position.set(xMin, yCenter, 0);
  roomFaces.negX.scale.set(halfZ, totalHeight, 1);
  roomFaces.posY.position.set(xCenter, yMax, 0);
  roomFaces.posY.scale.set(depthHalfX, halfZ, 1);
  roomFaces.negY.position.set(xCenter, yMin, 0);
  roomFaces.negY.scale.set(depthHalfX, halfZ, 1);
  roomFaces.posZ.position.set(xCenter, yCenter, halfZ);
  roomFaces.posZ.scale.set(depthHalfX, totalHeight, 1);
  roomFaces.negZ.position.set(xCenter, yCenter, -halfZ);
  roomFaces.negZ.scale.set(depthHalfX, totalHeight, 1);

  fitScreenToUpperHalf();
  updateRoomDimensionGuides();
  updateVbapCartesianFaceGrid();
  if (typeof flushCallbacks.refreshSpeakerHeatmapScene === 'function') {
    flushCallbacks.refreshSpeakerHeatmapScene();
  }
}

// ---------------------------------------------------------------------------
// Apply room ratio (reposition all sources and speakers)
// ---------------------------------------------------------------------------

export function applyRoomRatio(nextRatio) {
  app.roomRatio.width = Number(nextRatio.width) || 1;
  app.roomRatio.length = Number(nextRatio.length) || 1;
  app.roomRatio.height = Number(nextRatio.height) || 1;
  const rearValue = Number(nextRatio.rear);
  const lowerValue = Number(nextRatio.lower);
  app.roomRatio.rear = Number.isFinite(rearValue) && rearValue > 0 ? rearValue : app.roomRatio.rear;
  app.roomRatio.lower = Number.isFinite(lowerValue) && lowerValue > 0 ? lowerValue : app.roomRatio.lower;
  const centerBlendValue = Number(nextRatio.centerBlend);
  app.roomRatio.centerBlend = Number.isFinite(centerBlendValue)
    ? Math.max(0, Math.min(1, centerBlendValue))
    : app.roomRatio.centerBlend;
  updateRoomRatioDisplay();
  applyRoomRatioToScene();

  sourceMeshes.forEach((mesh, id) => {
    const raw = sourcePositionsRaw.get(String(id));
    if (!raw) return;
    if (raw.directSpeakerIndex !== null && raw.directSpeakerIndex !== undefined) {
      const speakerMesh = speakerMeshes[raw.directSpeakerIndex];
      if (speakerMesh) {
        mesh.position.copy(speakerMesh.position);
      } else {
        const pos = normalizedOmniphonyToScenePosition(raw);
        mesh.position.set(pos.x, pos.y, pos.z);
      }
    } else {
      const pos = normalizedOmniphonyToScenePosition(raw);
      mesh.position.set(pos.x, pos.y, pos.z);
    }
    updateSourceDecorations(id);
    rebuildTrailGeometry(id);
  });

  speakerMeshes.forEach((mesh, index) => {
    const speaker = app.currentLayoutSpeakers[index];
    if (!speaker) return;
    hydrateSpeakerCoordinateState(speaker);
    const scenePosition = normalizedOmniphonyToScenePosition(speaker);
    mesh.position.set(scenePosition.x, scenePosition.y, scenePosition.z);
    const label = speakerLabels[index];
    if (label) {
      label.position.set(scenePosition.x, scenePosition.y + 0.12, scenePosition.z);
    }
  });

  renderSpeakerEditor();
}
