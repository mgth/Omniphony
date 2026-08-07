import { invoke } from '@tauri-apps/api/core';
import { app } from '../state.js';
import { formatNumber } from '../coordinates.js';
import {
  renderVbapStatus,
  updateEvaluationMode,
  updateRenderBackend,
  updateVbapCartesian,
  updateVbapPolar,
  updateVbapPositionInterpolation
} from '../controls/vbap.js';
import { renderHybridCurve, setupHybridCurveEditor } from '../controls/hybrid-curve.js';
import { updateDistanceModelUI } from '../controls/master.js';
import { MIRROR_AXES, updateDistanceDiffuseUI } from '../controls/distance-diffuse.js';
import { renderVbapCartesianGridToggle, updateVbapCartesianFaceGrid } from '../scene/gizmos.js';

export function setupRendererPanelListeners() {
  const distanceModelSelectEl = document.getElementById('distanceModelSelect');
  const distanceModelMetricSelectEl = document.getElementById('distanceModelMetricSelect');
  const distanceDiffuseMetricSelectEl = document.getElementById('distanceDiffuseMetricSelect');
  const vbapCartXSizeInputEl = document.getElementById('vbapCartXSizeInput');
  const vbapCartYSizeInputEl = document.getElementById('vbapCartYSizeInput');
  const vbapCartZSizeInputEl = document.getElementById('vbapCartZSizeInput');
  const vbapCartZNegSizeInputEl = document.getElementById('vbapCartZNegSizeInput');
  const vbapCartesianGridToggleBtnEl = document.getElementById('vbapCartesianGridToggleBtn');
  const renderBackendSelectEl = document.getElementById('renderBackendSelect');
  const restoreBackendBtnEl = document.getElementById('restoreBackendBtn');
  const renderEvaluationModeSelectEl = document.getElementById('renderEvaluationModeSelect');
  const vbapPolarAzimuthResolutionInputEl = document.getElementById('vbapPolarAzimuthResolutionInput');
  const vbapPolarElevationResolutionInputEl = document.getElementById('vbapPolarElevationResolutionInput');
  const vbapPolarDistanceResInputEl = document.getElementById('vbapPolarDistanceResInput');
  const vbapPolarDistanceMaxInputEl = document.getElementById('vbapPolarDistanceMaxInput');
  const vbapPositionInterpolationToggleEl = document.getElementById('vbapPositionInterpolationToggleEl');
  const objectSizeIntervalsInputEl = document.getElementById('objectSizeIntervalsInput');
  const distanceDiffuseToggleEl = document.getElementById('distanceDiffuseToggle');
  const distanceDiffuseThresholdSliderEl = document.getElementById('distanceDiffuseThresholdSlider');
  const distanceDiffuseThresholdValEl = document.getElementById('distanceDiffuseThresholdVal');
  const distanceDiffuseCurveSliderEl = document.getElementById('distanceDiffuseCurveSlider');
  const distanceDiffuseCurveValEl = document.getElementById('distanceDiffuseCurveVal');
  const hybridExternalBackendSelectEl = document.getElementById('hybridExternalBackendSelect');
  const hybridInternalBackendSelectEl = document.getElementById('hybridInternalBackendSelect');
  const hybridMetricSelectEl = document.getElementById('hybridMetricSelect');
  const hybridCurveSmoothingSliderEl = document.getElementById('hybridCurveSmoothingSlider');
  const hybridCurveSmoothingValEl = document.getElementById('hybridCurveSmoothingVal');

  if (distanceModelSelectEl) {
    distanceModelSelectEl.addEventListener('change', () => {
      const value = String(distanceModelSelectEl.value || '').trim().toLowerCase();
      if (!['none', 'linear', 'quadratic', 'inverse-square'].includes(value)) return;
      app.distanceModel = value;
      updateDistanceModelUI();
      app.vbapRecomputing = true;
      renderVbapStatus();
      invoke('control_distance_model', { value });
    });
  }

  if (distanceModelMetricSelectEl) {
    distanceModelMetricSelectEl.addEventListener('change', () => {
      const value = String(distanceModelMetricSelectEl.value || '').trim().toLowerCase();
      if (!['spherical', 'chebyshev'].includes(value)) return;
      app.distanceModelMetric = value;
      updateDistanceModelUI();
      app.vbapRecomputing = true;
      renderVbapStatus();
      invoke('control_distance_model_metric', { value });
    });
  }

  if (distanceDiffuseMetricSelectEl) {
    distanceDiffuseMetricSelectEl.addEventListener('change', () => {
      const value = String(distanceDiffuseMetricSelectEl.value || '').trim().toLowerCase();
      if (!['spherical', 'chebyshev'].includes(value)) return;
      app.distanceDiffuseState.metric = value;
      updateDistanceDiffuseUI();
      app.vbapRecomputing = true;
      renderVbapStatus();
      invoke('control_distance_diffuse_metric', { value });
    });
  }

  for (const axis of MIRROR_AXES) {
    const el = document.getElementById(`distanceDiffuseMirror${axis.toUpperCase()}`);
    if (!el) continue;
    el.addEventListener('change', () => {
      app.distanceDiffuseState.mirrorAxes[axis] = el.checked === true;
      updateDistanceDiffuseUI();
      app.vbapRecomputing = true;
      renderVbapStatus();
      // The renderer takes the whole set as one string, since the flips compose
      // into a single mirror rather than acting independently.
      const value = MIRROR_AXES.filter((a) => app.distanceDiffuseState.mirrorAxes[a]).join('') || 'none';
      invoke('control_distance_diffuse_mirror_axes', { value });
    });
  }

  if (vbapCartXSizeInputEl) {
    vbapCartXSizeInputEl.addEventListener('change', () => {
      const value = Math.max(1, Math.round(Number(vbapCartXSizeInputEl.value) || 1));
      app.vbapCartesianState.xSize = value;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateVbapCartesian();
      invoke('control_render_evaluation_cartesian_x_size', { value });
    });
  }

  if (objectSizeIntervalsInputEl) {
    objectSizeIntervalsInputEl.addEventListener('change', () => {
      const value = Math.max(0, Math.round(Number(objectSizeIntervalsInputEl.value) || 0));
      app.objectSizeIntervals = value;
      objectSizeIntervalsInputEl.value = String(value);
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateEvaluationMode();
      invoke('control_render_evaluation_object_size_intervals', { value });
    });
  }

  if (vbapCartYSizeInputEl) {
    vbapCartYSizeInputEl.addEventListener('change', () => {
      const value = Math.max(1, Math.round(Number(vbapCartYSizeInputEl.value) || 1));
      app.vbapCartesianState.ySize = value;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateVbapCartesian();
      invoke('control_render_evaluation_cartesian_y_size', { value });
    });
  }

  if (vbapCartZSizeInputEl) {
    vbapCartZSizeInputEl.addEventListener('change', () => {
      const value = Math.max(1, Math.round(Number(vbapCartZSizeInputEl.value) || 1));
      app.vbapCartesianState.zSize = value;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateVbapCartesian();
      invoke('control_render_evaluation_cartesian_z_size', { value });
    });
  }

  if (vbapCartZNegSizeInputEl) {
    vbapCartZNegSizeInputEl.addEventListener('change', () => {
      const value = Math.max(0, Math.round(Number(vbapCartZNegSizeInputEl.value) || 0));
      app.vbapCartesianState.zNegSize = value;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateVbapCartesian();
      invoke('control_render_evaluation_cartesian_z_neg_size', { value });
    });
  }

  if (vbapCartesianGridToggleBtnEl) {
    vbapCartesianGridToggleBtnEl.addEventListener('change', () => {
      app.vbapCartesianFaceGridEnabled = Boolean(vbapCartesianGridToggleBtnEl.checked);
      renderVbapCartesianGridToggle();
      updateVbapCartesianFaceGrid();
    });
  }

  if (renderBackendSelectEl) {
    renderBackendSelectEl.addEventListener('change', () => {
      const value = String(renderBackendSelectEl.value || '').trim().toLowerCase();
      if (!value) return;
      if (app.renderBackendState.selection === value) return;
      app.renderBackendState.selection = value;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateRenderBackend();
      invoke('control_render_backend', { value });
    });
  }

  // A valid hybrid inner model is any registered backend except a nested hybrid.
  const isValidHybridInner = (value) => {
    if (!value || value === 'hybrid') return false;
    const available = Array.isArray(app.renderBackendState.availableBackends)
      ? app.renderBackendState.availableBackends
      : null;
    // If the engine hasn't published a list yet, accept any non-hybrid id.
    return !available || available.some((b) => String(b.id) === value);
  };

  if (hybridExternalBackendSelectEl) {
    hybridExternalBackendSelectEl.addEventListener('change', () => {
      const value = String(hybridExternalBackendSelectEl.value || '').trim().toLowerCase();
      if (!isValidHybridInner(value)) return;
      app.renderBackendState.hybrid.externalBackend = value;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateRenderBackend();
      invoke('control_hybrid_external_backend', { value });
    });
  }

  if (hybridInternalBackendSelectEl) {
    hybridInternalBackendSelectEl.addEventListener('change', () => {
      const value = String(hybridInternalBackendSelectEl.value || '').trim().toLowerCase();
      if (!isValidHybridInner(value)) return;
      app.renderBackendState.hybrid.internalBackend = value;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateRenderBackend();
      invoke('control_hybrid_internal_backend', { value });
    });
  }

  if (hybridMetricSelectEl) {
    hybridMetricSelectEl.addEventListener('change', () => {
      const value = String(hybridMetricSelectEl.value || '').trim().toLowerCase();
      if (!['spherical', 'chebyshev'].includes(value)) return;
      app.renderBackendState.hybrid.metric = value;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateRenderBackend();
      invoke('control_hybrid_metric', { value });
    });
  }

  if (hybridCurveSmoothingSliderEl) {
    // Live preview on input (local redraw only); push to the renderer on release.
    hybridCurveSmoothingSliderEl.addEventListener('input', () => {
      const value = Math.min(1, Math.max(0, Number(hybridCurveSmoothingSliderEl.value) || 0));
      app.renderBackendState.hybrid.curveSmoothing = value;
      if (hybridCurveSmoothingValEl) hybridCurveSmoothingValEl.textContent = formatNumber(value, 2);
      renderHybridCurve();
    });
    hybridCurveSmoothingSliderEl.addEventListener('change', () => {
      const value = Math.min(1, Math.max(0, Number(hybridCurveSmoothingSliderEl.value) || 0));
      app.renderBackendState.hybrid.curveSmoothing = value;
      app.vbapRecomputing = true;
      renderVbapStatus();
      invoke('control_hybrid_curve_smoothing', { value });
    });
  }

  setupHybridCurveEditor();

  if (restoreBackendBtnEl) {
    restoreBackendBtnEl.addEventListener('click', () => {
      if (app.renderBackendState.restoreBackendAvailable !== true) return;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateRenderBackend();
      invoke('control_restore_render_backend');
    });
  }

  if (renderEvaluationModeSelectEl) {
    renderEvaluationModeSelectEl.addEventListener('change', () => {
      const value = String(renderEvaluationModeSelectEl.value || '').trim().toLowerCase();
      const allowed = Array.isArray(app.renderBackendState.allowedEvaluationModes)
        && app.renderBackendState.allowedEvaluationModes.length > 0
        ? app.renderBackendState.allowedEvaluationModes
        : ['auto', 'realtime', 'precomputed_polar', 'precomputed_cartesian'];
      if (!allowed.includes(value)) return;
      if (app.evaluationModeState.selection === value) return;
      app.evaluationModeState.selection = value;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateEvaluationMode();
      invoke('control_render_evaluation_mode', { value });
    });
  }

  if (vbapPolarAzimuthResolutionInputEl) {
    vbapPolarAzimuthResolutionInputEl.addEventListener('change', () => {
      const value = Math.max(1, Math.round(Number(vbapPolarAzimuthResolutionInputEl.value) || 1));
      app.vbapPolarState.azimuthResolution = value;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateVbapPolar();
      invoke('control_render_evaluation_polar_azimuth_resolution', { value });
    });
  }

  if (vbapPolarElevationResolutionInputEl) {
    vbapPolarElevationResolutionInputEl.addEventListener('change', () => {
      const value = Math.max(1, Math.round(Number(vbapPolarElevationResolutionInputEl.value) || 1));
      app.vbapPolarState.elevationResolution = value;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateVbapPolar();
      invoke('control_render_evaluation_polar_elevation_resolution', { value });
    });
  }

  if (vbapPolarDistanceResInputEl) {
    vbapPolarDistanceResInputEl.addEventListener('change', () => {
      const value = Math.max(1, Math.round(Number(vbapPolarDistanceResInputEl.value) || 1));
      app.vbapPolarState.distanceRes = value;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateVbapPolar();
      invoke('control_render_evaluation_polar_distance_res', { value });
    });
  }

  if (vbapPolarDistanceMaxInputEl) {
    vbapPolarDistanceMaxInputEl.addEventListener('change', () => {
      const value = Math.max(0.01, Number(vbapPolarDistanceMaxInputEl.value) || 2);
      app.vbapPolarState.distanceMax = value;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateVbapPolar();
      invoke('control_render_evaluation_polar_distance_max', { value });
    });
  }

  if (vbapPositionInterpolationToggleEl) {
    vbapPositionInterpolationToggleEl.addEventListener('change', () => {
      const enabled = vbapPositionInterpolationToggleEl.checked;
      app.vbapPositionInterpolation = enabled;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateVbapPositionInterpolation();
      invoke('control_render_evaluation_position_interpolation', { enable: enabled ? 1 : 0 });
    });
  }

  if (distanceDiffuseToggleEl) {
    distanceDiffuseToggleEl.addEventListener('change', () => {
      const enabled = distanceDiffuseToggleEl.checked;
      app.distanceDiffuseState.enabled = enabled;
      updateDistanceDiffuseUI();
      invoke('control_distance_diffuse_enabled', { enable: enabled ? 1 : 0 });
    });
  }

  if (distanceDiffuseThresholdSliderEl) {
    distanceDiffuseThresholdSliderEl.addEventListener('input', () => {
      const value = Number(distanceDiffuseThresholdSliderEl.value);
      if (!Number.isFinite(value)) return;
      app.distanceDiffuseState.threshold = value;
      if (distanceDiffuseThresholdValEl) distanceDiffuseThresholdValEl.textContent = formatNumber(value, 2);
      invoke('control_distance_diffuse_threshold', { value });
    });
  }

  if (distanceDiffuseCurveSliderEl) {
    distanceDiffuseCurveSliderEl.addEventListener('input', () => {
      const value = Number(distanceDiffuseCurveSliderEl.value);
      if (!Number.isFinite(value)) return;
      app.distanceDiffuseState.curve = value;
      if (distanceDiffuseCurveValEl) distanceDiffuseCurveValEl.textContent = formatNumber(value, 2);
      invoke('control_distance_diffuse_curve', { value });
    });
  }
}
