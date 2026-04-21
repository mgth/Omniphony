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

function getVbapStatusEl() { return inRendererPanel('vbapStatus'); }
function getRenderBackendSelectEl() { return inRendererPanel('renderBackendSelect'); }
function getRestoreBackendBtnEl() { return inRendererPanel('restoreBackendBtn'); }
function getRenderBackendEffectiveEl() { return inRendererPanel('renderBackendEffective'); }
function getRenderEvaluationModeSelectEl() { return inRendererPanel('renderEvaluationModeSelect'); }
function getRenderEvaluationModeEffectiveEl() { return inRendererPanel('renderEvaluationModeEffective'); }
function getRendererSummaryEl() { return inRendererPanel('rendererSummary'); }
function getBackendParametersSectionEl() { return inRendererPanel('backendParametersSection'); }
function getBackendSpecificParamsSectionEl() { return inRendererPanel('backendSpecificParamsSection'); }
function getEvaluationSectionEl() { return inRendererPanel('evaluationSection'); }
function getDistanceModelControlRowEl() { return inRendererPanel('distanceModelControlRow'); }
function getSpreadSectionEl() { return inRendererPanel('spreadSection'); }
function getDistanceDiffuseSectionEl() { return inRendererPanel('distanceDiffuseSection'); }
function getSpreadFromDistanceSectionEl() { return inRendererPanel('spreadFromDistanceSection'); }
function getBarycenterSectionEl() { return inRendererPanel('barycenterSection'); }
function getExperimentalDistanceSectionEl() { return inRendererPanel('experimentalDistanceSection'); }
function getVbapCartXSizeInputEl() { return inRendererPanel('vbapCartXSizeInput'); }
function getVbapCartYSizeInputEl() { return inRendererPanel('vbapCartYSizeInput'); }
function getVbapCartZSizeInputEl() { return inRendererPanel('vbapCartZSizeInput'); }
function getVbapCartZNegSizeInputEl() { return inRendererPanel('vbapCartZNegSizeInput'); }
function getVbapCartXStepInfoEl() { return inRendererPanel('vbapCartXStepInfo'); }
function getVbapCartYStepInfoEl() { return inRendererPanel('vbapCartYStepInfo'); }
function getVbapCartZStepInfoEl() { return inRendererPanel('vbapCartZStepInfo'); }
function getVbapCartZNegStepInfoEl() { return inRendererPanel('vbapCartZNegStepInfo'); }
function getVbapPolarAzimuthResolutionInputEl() { return inRendererPanel('vbapPolarAzimuthResolutionInput'); }
function getVbapPolarElevationResolutionInputEl() { return inRendererPanel('vbapPolarElevationResolutionInput'); }
function getVbapPolarDistanceResInputEl() { return inRendererPanel('vbapPolarDistanceResInput'); }
function getVbapPolarDistanceMaxInputEl() { return inRendererPanel('vbapPolarDistanceMaxInput'); }
function getVbapElevationRangeInfoEl() { return inRendererPanel('vbapElevationRangeInfo'); }
function getVbapAzimuthRangeInfoEl() { return inRendererPanel('vbapAzimuthRangeInfo'); }
function getVbapPolarAzStepInfoEl() { return inRendererPanel('vbapPolarAzStepInfo'); }
function getVbapPolarElStepInfoEl() { return inRendererPanel('vbapPolarElStepInfo'); }
function getVbapPolarDistStepInfoEl() { return inRendererPanel('vbapPolarDistStepInfo'); }
function getVbapPositionInterpolationToggleEl() { return inRendererPanel('vbapPositionInterpolationToggleEl'); }
function getBarycenterLocalizeInputEl() { return inRendererPanel('barycenterLocalizeInput'); }
function getBarycenterLocalizeValEl() { return inRendererPanel('barycenterLocalizeVal'); }
function getExperimentalDistanceFloorInputEl() { return inRendererPanel('experimentalDistanceFloorInput'); }
function getExperimentalDistanceMinActiveInputEl() { return inRendererPanel('experimentalDistanceMinActiveInput'); }
function getExperimentalDistanceMaxActiveInputEl() { return inRendererPanel('experimentalDistanceMaxActiveInput'); }
function getExperimentalDistanceErrorFloorInputEl() { return inRendererPanel('experimentalDistanceErrorFloorInput'); }
function getExperimentalDistanceNearestScaleInputEl() { return inRendererPanel('experimentalDistanceNearestScaleInput'); }
function getExperimentalDistanceSpanScaleInputEl() { return inRendererPanel('experimentalDistanceSpanScaleInput'); }

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

function backendCapabilities() {
  return app.renderBackendState.capabilities || null;
}


function backendLabel(backend) {
  if (backend === (app.renderBackendState.effective || '')) {
    return app.renderBackendState.effectiveLabel || backend || '—';
  }
  if (backend === 'vbap') return 'VBAP';
  if (backend === 'barycenter') return 'Barycenter';
  if (backend === 'experimental_distance') return 'Distance';
  return backend || '—';
}

function applyRendererBackendVisibility(backend) {
  const backendParametersSectionEl = getBackendParametersSectionEl();
  const evaluationSectionEl = getEvaluationSectionEl();
  const backendSpecificParamsSectionEl = getBackendSpecificParamsSectionEl();
  const distanceModelControlRowEl = getDistanceModelControlRowEl();
  const spreadSectionEl = getSpreadSectionEl();
  const distanceDiffuseSectionEl = getDistanceDiffuseSectionEl();
  const spreadFromDistanceSectionEl = getSpreadFromDistanceSectionEl();
  const experimentalDistanceSectionEl = getExperimentalDistanceSectionEl();
  const barycenterSectionEl = getBarycenterSectionEl();
  const capabilities = backendCapabilities();
  const supportsDistanceModel = capabilities?.supportsDistanceModel === true;
  const supportsSpread = capabilities?.supportsSpread === true;
  const supportsDistanceDiffuse = capabilities?.supportsDistanceDiffuse === true;
  const showsBarycenter = backend === 'barycenter';
  const showsExperimentalDistance = backend === 'experimental_distance';
  if (backendParametersSectionEl) {
    backendParametersSectionEl.style.display = '';
  }
  if (evaluationSectionEl) {
    evaluationSectionEl.style.display = '';
  }
  if (backendSpecificParamsSectionEl) {
    backendSpecificParamsSectionEl.style.display =
      supportsDistanceModel || supportsSpread || supportsDistanceDiffuse || showsBarycenter || showsExperimentalDistance
        ? ''
        : 'none';
  }
  if (distanceModelControlRowEl) {
    distanceModelControlRowEl.style.display = supportsDistanceModel ? '' : 'none';
  }
  if (spreadSectionEl) {
    spreadSectionEl.style.display = supportsSpread ? '' : 'none';
  }
  if (distanceDiffuseSectionEl) {
    distanceDiffuseSectionEl.style.display = supportsDistanceDiffuse ? '' : 'none';
  }
  if (spreadFromDistanceSectionEl) {
    spreadFromDistanceSectionEl.style.display =
      capabilities?.supportsSpreadFromDistance === true ? '' : 'none';
  }
  if (experimentalDistanceSectionEl) {
    experimentalDistanceSectionEl.style.display = showsExperimentalDistance ? '' : 'none';
  }
  if (barycenterSectionEl) {
    barycenterSectionEl.style.display = showsBarycenter ? '' : 'none';
  }
}

function applyEvaluationModeVisibility(mode) {
  const capabilities = backendCapabilities();
  const showsCartesian =
    mode === 'precomputed_cartesian' && capabilities?.supportsPrecomputedCartesian === true;
  const showsPolar =
    mode === 'precomputed_polar' && capabilities?.supportsPrecomputedPolar === true;
  const showsInterpolation = showsCartesian || showsPolar;
  const renderEvaluationCartesianBlockEl = inRendererPanel('renderEvaluationCartesianBlock');
  const renderEvaluationPolarBlockEl = inRendererPanel('renderEvaluationPolarBlock');
  const renderEvaluationPositionInterpolationRowEl = inRendererPanel('renderEvaluationPositionInterpolationRow');
  if (renderEvaluationCartesianBlockEl) {
    renderEvaluationCartesianBlockEl.hidden = !showsCartesian;
    renderEvaluationCartesianBlockEl.style.setProperty('display', showsCartesian ? '' : 'none', 'important');
  }
  if (renderEvaluationPolarBlockEl) {
    renderEvaluationPolarBlockEl.hidden = !showsPolar;
    renderEvaluationPolarBlockEl.style.setProperty('display', showsPolar ? '' : 'none', 'important');
  }
  if (renderEvaluationPositionInterpolationRowEl) {
    renderEvaluationPositionInterpolationRowEl.hidden = !showsInterpolation;
    renderEvaluationPositionInterpolationRowEl.style.setProperty('display', showsInterpolation ? '' : 'none', 'important');
  }
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
  const vbapStatusEl = getVbapStatusEl();
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

export function renderEvaluationMode() {
  const renderEvaluationModeSelectEl = getRenderEvaluationModeSelectEl();
  const renderEvaluationModeEffectiveEl = getRenderEvaluationModeEffectiveEl();
  const rendererSummaryEl = getRendererSummaryEl();
  const backend = app.renderBackendState.effective || app.renderBackendState.selection || 'vbap';
  const allowedModes = Array.isArray(app.renderBackendState.allowedEvaluationModes)
    && app.renderBackendState.allowedEvaluationModes.length > 0
    ? app.renderBackendState.allowedEvaluationModes
    : ['auto', 'realtime', 'precomputed_polar', 'precomputed_cartesian'];
  const selection = typeof app.evaluationModeState.selection === 'string' ? app.evaluationModeState.selection : null;
  const effectiveMode = typeof app.evaluationModeState.effective === 'string' ? app.evaluationModeState.effective : null;
  const visibleModes = [...allowedModes];
  if (selection && !visibleModes.includes(selection)) {
    visibleModes.push(selection);
  }
  if (effectiveMode && !visibleModes.includes(effectiveMode)) {
    visibleModes.push(effectiveMode);
  }
  const nextValue = visibleModes.includes(selection) ? selection : visibleModes[0];
  if (renderEvaluationModeSelectEl) {
    const currentOptions = Array.from(renderEvaluationModeSelectEl.options).map((option) => option.value);
    if (
      currentOptions.length !== visibleModes.length
      || currentOptions.some((value, index) => value !== visibleModes[index])
    ) {
      renderEvaluationModeSelectEl.innerHTML = '';
      visibleModes.forEach((mode) => {
        const option = document.createElement('option');
        option.value = mode;
        option.textContent = formatEvaluationModeLabel(mode);
        renderEvaluationModeSelectEl.append(option);
      });
    }
    renderEvaluationModeSelectEl.value = nextValue;
    renderEvaluationModeSelectEl.disabled = allowedModes.length === 0;
  }
  if (renderEvaluationModeEffectiveEl) {
    renderEvaluationModeEffectiveEl.textContent = formatEvaluationModeLabel(effectiveMode);
  }
  if (rendererSummaryEl) {
    const mode = effectiveMode || selection;
    const modeText = formatEvaluationModeLabel(mode);
    const backendText = backendLabel(backend);
    rendererSummaryEl.textContent = `${backendText} / ${tf('renderer.summary', { mode: modeText })}`;
  }
  const visibleMode = nextValue === 'auto'
    ? (effectiveMode || nextValue)
    : nextValue;
  applyEvaluationModeVisibility(visibleMode);
}

export function updateEvaluationMode() {
  dirty.vbapMode = true;
  scheduleUIFlush();
}

export function renderRenderBackend() {
  const renderBackendSelectEl = getRenderBackendSelectEl();
  const restoreBackendBtnEl = getRestoreBackendBtnEl();
  const renderBackendEffectiveEl = getRenderBackendEffectiveEl();
  const selection = typeof app.renderBackendState.selection === 'string' ? app.renderBackendState.selection : 'vbap';
  const effective = typeof app.renderBackendState.effective === 'string' ? app.renderBackendState.effective : null;
  const visibleBackend = effective || selection;
  const frozen = app.renderBackendState.frozenSpeakers === true;
  if (renderBackendSelectEl) {
    renderBackendSelectEl.value = selection;
    renderBackendSelectEl.disabled = frozen;
  }
  if (restoreBackendBtnEl) {
    restoreBackendBtnEl.style.display = 'none';
    restoreBackendBtnEl.disabled = true;
  }
  if (renderBackendEffectiveEl) {
    renderBackendEffectiveEl.textContent = backendLabel(effective);
  }
  const layoutSelectEl = document.getElementById('layoutSelect');
  const importLayoutBtnEl = document.getElementById('importLayoutBtn');
  const exportLayoutBtnEl = document.getElementById('exportLayoutBtn');
  const layoutFrozen = frozen;
  if (layoutSelectEl) {
    layoutSelectEl.disabled = layoutFrozen || layoutSelectEl.options.length === 0;
  }
  if (importLayoutBtnEl) importLayoutBtnEl.disabled = layoutFrozen;
  if (exportLayoutBtnEl) exportLayoutBtnEl.disabled = layoutFrozen;
  applyRendererBackendVisibility(visibleBackend);
  renderBarycenterOptions();
  renderExperimentalDistanceOptions();
  renderEvaluationMode();
}

export function updateRenderBackend() {
  dirty.renderBackend = true;
  dirty.barycenter = true;
  dirty.experimentalDistance = true;
  dirty.vbapMode = true;
  scheduleUIFlush();
}

export function renderBarycenterOptions() {
  const barycenterLocalizeInputEl = getBarycenterLocalizeInputEl();
  const barycenterLocalizeValEl = getBarycenterLocalizeValEl();
  const state = app.renderBackendState.barycenter || {};
  const value = typeof state.localize === 'number' ? state.localize : 0;
  if (barycenterLocalizeInputEl) {
    barycenterLocalizeInputEl.value = String(value);
  }
  if (barycenterLocalizeValEl) {
    barycenterLocalizeValEl.textContent = formatNumber(value, 2);
  }
}

export function updateBarycenterOptions() {
  dirty.barycenter = true;
  scheduleUIFlush();
}

export function renderExperimentalDistanceOptions() {
  const experimentalDistanceFloorInputEl = getExperimentalDistanceFloorInputEl();
  const experimentalDistanceMinActiveInputEl = getExperimentalDistanceMinActiveInputEl();
  const experimentalDistanceMaxActiveInputEl = getExperimentalDistanceMaxActiveInputEl();
  const experimentalDistanceErrorFloorInputEl = getExperimentalDistanceErrorFloorInputEl();
  const experimentalDistanceNearestScaleInputEl = getExperimentalDistanceNearestScaleInputEl();
  const experimentalDistanceSpanScaleInputEl = getExperimentalDistanceSpanScaleInputEl();
  const state = app.renderBackendState.experimentalDistance || {};
  if (experimentalDistanceFloorInputEl) {
    experimentalDistanceFloorInputEl.value =
      typeof state.distanceFloor === 'number' ? String(state.distanceFloor) : '';
  }
  if (experimentalDistanceMinActiveInputEl) {
    experimentalDistanceMinActiveInputEl.value =
      typeof state.minActiveSpeakers === 'number' ? String(state.minActiveSpeakers) : '';
  }
  if (experimentalDistanceMaxActiveInputEl) {
    experimentalDistanceMaxActiveInputEl.value =
      typeof state.maxActiveSpeakers === 'number' ? String(state.maxActiveSpeakers) : '';
  }
  if (experimentalDistanceErrorFloorInputEl) {
    experimentalDistanceErrorFloorInputEl.value =
      typeof state.positionErrorFloor === 'number' ? String(state.positionErrorFloor) : '';
  }
  if (experimentalDistanceNearestScaleInputEl) {
    experimentalDistanceNearestScaleInputEl.value =
      typeof state.positionErrorNearestScale === 'number' ? String(state.positionErrorNearestScale) : '';
  }
  if (experimentalDistanceSpanScaleInputEl) {
    experimentalDistanceSpanScaleInputEl.value =
      typeof state.positionErrorSpanScale === 'number' ? String(state.positionErrorSpanScale) : '';
  }
}

export function updateExperimentalDistanceOptions() {
  dirty.experimentalDistance = true;
  scheduleUIFlush();
}

export function renderVbapCartesian() {
  const vbapCartXSizeInputEl = getVbapCartXSizeInputEl();
  const vbapCartYSizeInputEl = getVbapCartYSizeInputEl();
  const vbapCartZSizeInputEl = getVbapCartZSizeInputEl();
  const vbapCartZNegSizeInputEl = getVbapCartZNegSizeInputEl();
  const vbapCartXStepInfoEl = getVbapCartXStepInfoEl();
  const vbapCartYStepInfoEl = getVbapCartYStepInfoEl();
  const vbapCartZStepInfoEl = getVbapCartZStepInfoEl();
  const vbapCartZNegStepInfoEl = getVbapCartZNegStepInfoEl();
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
  const vbapPolarAzimuthResolutionInputEl = getVbapPolarAzimuthResolutionInputEl();
  const vbapPolarElevationResolutionInputEl = getVbapPolarElevationResolutionInputEl();
  const vbapPolarDistanceResInputEl = getVbapPolarDistanceResInputEl();
  const vbapPolarDistanceMaxInputEl = getVbapPolarDistanceMaxInputEl();
  const vbapElevationRangeInfoEl = getVbapElevationRangeInfoEl();
  const vbapAzimuthRangeInfoEl = getVbapAzimuthRangeInfoEl();
  const vbapPolarAzStepInfoEl = getVbapPolarAzStepInfoEl();
  const vbapPolarElStepInfoEl = getVbapPolarElStepInfoEl();
  const vbapPolarDistStepInfoEl = getVbapPolarDistStepInfoEl();
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
  const vbapPositionInterpolationToggleEl = getVbapPositionInterpolationToggleEl();
  if (!vbapPositionInterpolationToggleEl) return;
  vbapPositionInterpolationToggleEl.checked = app.vbapPositionInterpolation !== false;
}

export function updateVbapPositionInterpolation() {
  dirty.vbapPolar = true;
  scheduleUIFlush();
}
