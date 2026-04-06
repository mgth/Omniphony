/**
 * VBAP status, mode, cartesian, polar, and position interpolation controls.
 *
 * Extracted from app.js (lines 3711-3852).
 */

import { app, dirty } from '../state.js';
import { t, tf } from '../i18n.js';
import { formatNumber } from '../coordinates.js';
import { scheduleUIFlush } from '../flush.js';
import { inRendererPanel } from '../ui/panel-roots.js';

// DOM refs
const vbapStatusEl = inRendererPanel('vbapStatus');
const renderBackendSelectEl = inRendererPanel('renderBackendSelect');
const renderBackendEffectiveEl = inRendererPanel('renderBackendEffective');
const renderEvaluationModeSelectEl = inRendererPanel('renderEvaluationModeSelect');
const renderEvaluationModeEffectiveEl = inRendererPanel('renderEvaluationModeEffective');
const rendererSummaryEl = inRendererPanel('rendererSummary');
const vbapSectionEl = inRendererPanel('vbapSection');
const distanceModelControlRowEl = inRendererPanel('distanceModelControlRow');
const spreadSectionEl = inRendererPanel('spreadSection');
const distanceDiffuseSectionEl = inRendererPanel('distanceDiffuseSection');
const spreadFromDistanceSectionEl = inRendererPanel('spreadFromDistanceSection');
const vbapCartXSizeInputEl = inRendererPanel('vbapCartXSizeInput');
const vbapCartYSizeInputEl = inRendererPanel('vbapCartYSizeInput');
const vbapCartZSizeInputEl = inRendererPanel('vbapCartZSizeInput');
const vbapCartZNegSizeInputEl = inRendererPanel('vbapCartZNegSizeInput');
const vbapCartXStepInfoEl = inRendererPanel('vbapCartXStepInfo');
const vbapCartYStepInfoEl = inRendererPanel('vbapCartYStepInfo');
const vbapCartZStepInfoEl = inRendererPanel('vbapCartZStepInfo');
const vbapCartZNegStepInfoEl = inRendererPanel('vbapCartZNegStepInfo');
const vbapPolarAzimuthResolutionInputEl = inRendererPanel('vbapPolarAzimuthResolutionInput');
const vbapPolarElevationResolutionInputEl = inRendererPanel('vbapPolarElevationResolutionInput');
const vbapPolarDistanceResInputEl = inRendererPanel('vbapPolarDistanceResInput');
const vbapPolarDistanceMaxInputEl = inRendererPanel('vbapPolarDistanceMaxInput');
const vbapElevationRangeInfoEl = inRendererPanel('vbapElevationRangeInfo');
const vbapAzimuthRangeInfoEl = inRendererPanel('vbapAzimuthRangeInfo');
const vbapPolarAzStepInfoEl = inRendererPanel('vbapPolarAzStepInfo');
const vbapPolarElStepInfoEl = inRendererPanel('vbapPolarElStepInfo');
const vbapPolarDistStepInfoEl = inRendererPanel('vbapPolarDistStepInfo');
const vbapPositionInterpolationToggleEl = inRendererPanel('vbapPositionInterpolationToggleEl');

// These are called from renderVbapCartesian but defined elsewhere in app.js.
// They must be provided via the callback registry or imported separately.
// For now we reference them as imported stubs that the wiring code will supply.
import { flushCallbacks } from '../flush.js';

function updateVbapCartesianFaceGrid() {
  if (typeof flushCallbacks.updateVbapCartesianFaceGrid === 'function') {
    flushCallbacks.updateVbapCartesianFaceGrid();
  }
}

function renderVbapCartesianGridToggle() {
  if (typeof flushCallbacks.renderVbapCartesianGridToggle === 'function') {
    flushCallbacks.renderVbapCartesianGridToggle();
  }
}

function applyRendererBackendVisibility(backend) {
  const isVbap = backend !== 'experimental_distance';
  if (vbapSectionEl) {
    vbapSectionEl.style.display = isVbap ? '' : 'none';
  }
  if (distanceModelControlRowEl) {
    distanceModelControlRowEl.style.display = isVbap ? '' : 'none';
  }
  if (spreadSectionEl) {
    spreadSectionEl.style.display = isVbap ? '' : 'none';
  }
  if (distanceDiffuseSectionEl) {
    distanceDiffuseSectionEl.style.display = isVbap ? '' : 'none';
  }
  if (spreadFromDistanceSectionEl) {
    spreadFromDistanceSectionEl.style.display = isVbap ? '' : 'none';
  }
}

function allowedEvaluationModesForBackend(backend) {
  if (backend === 'experimental_distance') {
    return ['realtime'];
  }
  return ['auto', 'precomputed_polar', 'precomputed_cartesian'];
}

function formatEvaluationModeLabel(mode) {
  switch (mode) {
    case 'auto': return 'Auto';
    case 'realtime': return 'Realtime';
    case 'precomputed_polar': return 'Polar';
    case 'precomputed_cartesian': return 'Cartesian';
    default: return '—';
  }
}

export function renderVbapStatus() {
  if (!vbapStatusEl) return;
  vbapStatusEl.classList.remove('computing', 'ready');
  if (app.vbapRecomputing === true) {
    vbapStatusEl.textContent = t('vbap.status.computing');
    vbapStatusEl.classList.add('computing');
  } else if (app.vbapRecomputing === false) {
    vbapStatusEl.textContent = t('vbap.status.ready');
    vbapStatusEl.classList.add('ready');
  } else {
    vbapStatusEl.textContent = t('vbap.status.idle');
  }
}

export function renderVbapMode() {
  const backend = app.renderBackendState.effective || app.renderBackendState.selection || 'vbap';
  const allowedModes = allowedEvaluationModesForBackend(backend);
  const selection = typeof app.evaluationModeState.selection === 'string' ? app.evaluationModeState.selection : null;
  const effectiveMode = typeof app.evaluationModeState.effective === 'string' ? app.evaluationModeState.effective : null;
  if (renderEvaluationModeSelectEl) {
    const nextValue = allowedModes.includes(selection) ? selection : allowedModes[0];
    renderEvaluationModeSelectEl.value = nextValue;
    renderEvaluationModeSelectEl.disabled = allowedModes.length === 1;
  }
  if (renderEvaluationModeEffectiveEl) {
    renderEvaluationModeEffectiveEl.textContent = formatEvaluationModeLabel(effectiveMode);
  }
  if (rendererSummaryEl) {
    const mode = effectiveMode || selection;
    const modeText = formatEvaluationModeLabel(mode);
    const backendText = backend === 'experimental_distance'
      ? 'Distance'
      : backend === 'vbap'
        ? 'VBAP'
        : '—';
    rendererSummaryEl.textContent = `${backendText} / ${tf('renderer.summary', { mode: modeText })}`;
  }
}

export function updateVbapMode() {
  dirty.vbapMode = true;
  scheduleUIFlush();
}

export function renderRenderBackend() {
  const selection = typeof app.renderBackendState.selection === 'string' ? app.renderBackendState.selection : 'vbap';
  const effective = typeof app.renderBackendState.effective === 'string' ? app.renderBackendState.effective : null;
  const visibleBackend = effective || selection;
  if (renderBackendSelectEl) {
    renderBackendSelectEl.value = selection;
  }
  if (renderBackendEffectiveEl) {
    renderBackendEffectiveEl.textContent =
      effective === 'experimental_distance'
        ? 'Distance'
        : effective === 'vbap'
          ? 'VBAP'
          : '—';
  }
  applyRendererBackendVisibility(visibleBackend);
  renderVbapMode();
}

export function updateRenderBackend() {
  dirty.renderBackend = true;
  dirty.vbapMode = true;
  scheduleUIFlush();
}

export function renderVbapCartesian() {
  if (vbapCartXSizeInputEl) {
    vbapCartXSizeInputEl.value = app.vbapCartesianState.xSize === null ? '' : String(app.vbapCartesianState.xSize);
  }
  if (vbapCartYSizeInputEl) {
    vbapCartYSizeInputEl.value = app.vbapCartesianState.ySize === null ? '' : String(app.vbapCartesianState.ySize);
  }
  if (vbapCartZSizeInputEl) {
    vbapCartZSizeInputEl.value = app.vbapCartesianState.zSize === null ? '' : String(app.vbapCartesianState.zSize);
  }
  if (vbapCartZNegSizeInputEl) {
    vbapCartZNegSizeInputEl.value = String(Math.max(0, Math.round(Number(app.vbapCartesianState.zNegSize) || 0)));
  }
  const metersPerUnit = app.metersPerUnit ?? 1;
  const xStep = app.vbapCartesianState.xSize && app.vbapCartesianState.xSize > 0
    ? 2.0 / app.vbapCartesianState.xSize
    : null;
  const yStep = app.vbapCartesianState.ySize && app.vbapCartesianState.ySize > 0
    ? 2.0 / app.vbapCartesianState.ySize
    : null;
  const zStep = app.vbapCartesianState.zSize && app.vbapCartesianState.zSize > 0
    ? 1.0 / app.vbapCartesianState.zSize
    : null;
  const zNegStep = app.vbapAllowNegativeZ === false
    ? null
    : (Number(app.vbapCartesianState.zNegSize) || 0) > 0
      ? 1.0 / Number(app.vbapCartesianState.zNegSize)
    : null;
  const xStepMm = xStep === null ? null : xStep * metersPerUnit * 1000.0;
  const yStepMm = yStep === null ? null : yStep * metersPerUnit * 1000.0;
  const zStepMm = zStep === null ? null : zStep * metersPerUnit * 1000.0;
  const zNegStepMm = zNegStep === null ? null : zNegStep * metersPerUnit * 1000.0;
  if (vbapCartXStepInfoEl) vbapCartXStepInfoEl.textContent = xStepMm === null ? '—' : `${formatNumber(xStepMm, 1)}mm`;
  if (vbapCartYStepInfoEl) vbapCartYStepInfoEl.textContent = yStepMm === null ? '—' : `${formatNumber(yStepMm, 1)}mm`;
  if (vbapCartZStepInfoEl) vbapCartZStepInfoEl.textContent = zStepMm === null ? '—' : `${formatNumber(zStepMm, 1)}mm`;
  if (vbapCartZNegStepInfoEl) vbapCartZNegStepInfoEl.textContent = zNegStepMm === null ? '—' : `${formatNumber(zNegStepMm, 1)}mm`;
  updateVbapCartesianFaceGrid();
  renderVbapCartesianGridToggle();
}

export function updateVbapCartesian() {
  dirty.vbapCartesian = true;
  scheduleUIFlush();
}

export function renderVbapPolar() {
  if (vbapPolarAzimuthResolutionInputEl) {
    vbapPolarAzimuthResolutionInputEl.value =
      app.vbapPolarState.azimuthResolution === null ? '' : String(app.vbapPolarState.azimuthResolution);
  }
  if (vbapPolarElevationResolutionInputEl) {
    vbapPolarElevationResolutionInputEl.value =
      app.vbapPolarState.elevationResolution === null ? '' : String(app.vbapPolarState.elevationResolution);
  }
  if (vbapPolarDistanceResInputEl) {
    vbapPolarDistanceResInputEl.value =
      app.vbapPolarState.distanceRes === null ? '' : String(app.vbapPolarState.distanceRes);
  }
  if (vbapPolarDistanceMaxInputEl) {
    vbapPolarDistanceMaxInputEl.value =
      app.vbapPolarState.distanceMax === null ? '' : String(app.vbapPolarState.distanceMax);
  }
  if (vbapElevationRangeInfoEl) {
    const txt = app.vbapAllowNegativeZ === null
      ? '—'
      : app.vbapAllowNegativeZ
        ? '-90..90'
        : '0..90';
    vbapElevationRangeInfoEl.textContent = txt;
  }
  if (vbapAzimuthRangeInfoEl) {
    vbapAzimuthRangeInfoEl.textContent = '-180..180';
  }
  const azStep = app.vbapPolarState.azimuthResolution && app.vbapPolarState.azimuthResolution > 0
    ? 360.0 / app.vbapPolarState.azimuthResolution
    : null;
  const elRange = app.vbapAllowNegativeZ === false ? 90.0 : 180.0;
  const elStep = app.vbapPolarState.elevationResolution && app.vbapPolarState.elevationResolution > 0
    ? elRange / app.vbapPolarState.elevationResolution
    : null;
  const dStep = (app.vbapPolarState.distanceRes && app.vbapPolarState.distanceRes > 0 && app.vbapPolarState.distanceMax && app.vbapPolarState.distanceMax > 0)
    ? app.vbapPolarState.distanceMax / app.vbapPolarState.distanceRes
    : null;
  if (vbapPolarAzStepInfoEl) vbapPolarAzStepInfoEl.textContent = azStep === null ? '—' : `${formatNumber(azStep, 2)}°`;
  if (vbapPolarElStepInfoEl) vbapPolarElStepInfoEl.textContent = elStep === null ? '—' : `${formatNumber(elStep, 2)}°`;
  if (vbapPolarDistStepInfoEl) vbapPolarDistStepInfoEl.textContent = dStep === null ? '—' : `${formatNumber(dStep, 3)}`;
}

export function updateVbapPolar() {
  dirty.vbapPolar = true;
  scheduleUIFlush();
}

export function renderVbapPositionInterpolation() {
  if (!vbapPositionInterpolationToggleEl) return;
  vbapPositionInterpolationToggleEl.checked = app.vbapPositionInterpolation !== false;
}

export function updateVbapPositionInterpolation() {
  dirty.vbapPolar = true;
  scheduleUIFlush();
}
