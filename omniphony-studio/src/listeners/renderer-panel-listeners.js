import { invoke } from '@tauri-apps/api/core';
import { app } from '../state.js';
import { formatNumber } from '../coordinates.js';
import { renderVbapStatus, updateVbapMode, updateRenderBackend, updateVbapCartesian, updateVbapPolar, updateVbapPositionInterpolation } from '../controls/vbap.js';
import { updateSpreadDisplay } from '../controls/spread.js';
import { updateDistanceModelUI } from '../controls/master.js';
import { updateDistanceDiffuseUI } from '../controls/distance-diffuse.js';
import { renderVbapCartesianGridToggle, updateVbapCartesianFaceGrid } from '../scene/gizmos.js';

export function setupRendererPanelListeners() {
  const spreadMinSliderEl = document.getElementById('spreadMinSlider');
  const spreadMaxSliderEl = document.getElementById('spreadMaxSlider');
  const spreadFromDistanceToggleEl = document.getElementById('spreadFromDistanceToggle');
  const spreadDistanceRangeSliderEl = document.getElementById('spreadDistanceRangeSlider');
  const spreadDistanceCurveSliderEl = document.getElementById('spreadDistanceCurveSlider');
  const distanceModelSelectEl = document.getElementById('distanceModelSelect');
  const vbapCartXSizeInputEl = document.getElementById('vbapCartXSizeInput');
  const vbapCartYSizeInputEl = document.getElementById('vbapCartYSizeInput');
  const vbapCartZSizeInputEl = document.getElementById('vbapCartZSizeInput');
  const vbapCartZNegSizeInputEl = document.getElementById('vbapCartZNegSizeInput');
  const vbapCartesianGridToggleBtnEl = document.getElementById('vbapCartesianGridToggleBtn');
  const renderBackendSelectEl = document.getElementById('renderBackendSelect');
  const renderEvaluationModeSelectEl = document.getElementById('renderEvaluationModeSelect');
  const vbapPolarAzimuthResolutionInputEl = document.getElementById('vbapPolarAzimuthResolutionInput');
  const vbapPolarElevationResolutionInputEl = document.getElementById('vbapPolarElevationResolutionInput');
  const vbapPolarDistanceResInputEl = document.getElementById('vbapPolarDistanceResInput');
  const vbapPolarDistanceMaxInputEl = document.getElementById('vbapPolarDistanceMaxInput');
  const vbapPositionInterpolationToggleEl = document.getElementById('vbapPositionInterpolationToggleEl');
  const distanceDiffuseToggleEl = document.getElementById('distanceDiffuseToggle');
  const distanceDiffuseThresholdSliderEl = document.getElementById('distanceDiffuseThresholdSlider');
  const distanceDiffuseThresholdValEl = document.getElementById('distanceDiffuseThresholdVal');
  const distanceDiffuseCurveSliderEl = document.getElementById('distanceDiffuseCurveSlider');
  const distanceDiffuseCurveValEl = document.getElementById('distanceDiffuseCurveVal');

  if (spreadMinSliderEl) {
    spreadMinSliderEl.addEventListener('input', () => {
      const valueDeg = Number(spreadMinSliderEl.value);
      if (!Number.isFinite(valueDeg)) return;
      const valueNorm = Math.max(0, Math.min(180, valueDeg)) / 180.0;
      const maxValue = app.spreadState.max === null ? 1 : app.spreadState.max;
      app.spreadState.min = Math.min(valueNorm, maxValue);
      spreadMinSliderEl.value = String((app.spreadState.min ?? 0) * 180.0);
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateSpreadDisplay();
      invoke('control_spread_min', { value: app.spreadState.min });
    });
  }

  if (spreadMaxSliderEl) {
    spreadMaxSliderEl.addEventListener('input', () => {
      const valueDeg = Number(spreadMaxSliderEl.value);
      if (!Number.isFinite(valueDeg)) return;
      const valueNorm = Math.max(0, Math.min(180, valueDeg)) / 180.0;
      const minValue = app.spreadState.min === null ? 0 : app.spreadState.min;
      app.spreadState.max = Math.max(valueNorm, minValue);
      spreadMaxSliderEl.value = String((app.spreadState.max ?? 1) * 180.0);
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateSpreadDisplay();
      invoke('control_spread_max', { value: app.spreadState.max });
    });
  }

  if (spreadFromDistanceToggleEl) {
    spreadFromDistanceToggleEl.addEventListener('change', () => {
      const enabled = spreadFromDistanceToggleEl.checked;
      app.spreadState.fromDistance = enabled;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateSpreadDisplay();
      invoke('control_spread_from_distance', { enable: enabled ? 1 : 0 });
    });
  }

  if (spreadDistanceRangeSliderEl) {
    spreadDistanceRangeSliderEl.addEventListener('input', () => {
      const value = Number(spreadDistanceRangeSliderEl.value);
      if (!Number.isFinite(value)) return;
      app.spreadState.distanceRange = Math.max(0.01, value);
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateSpreadDisplay();
      invoke('control_spread_distance_range', { value: app.spreadState.distanceRange });
    });
  }

  if (spreadDistanceCurveSliderEl) {
    spreadDistanceCurveSliderEl.addEventListener('input', () => {
      const value = Number(spreadDistanceCurveSliderEl.value);
      if (!Number.isFinite(value)) return;
      app.spreadState.distanceCurve = Math.max(0, value);
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateSpreadDisplay();
      invoke('control_spread_distance_curve', { value: app.spreadState.distanceCurve });
    });
  }

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

  if (vbapCartXSizeInputEl) {
    vbapCartXSizeInputEl.addEventListener('change', () => {
      const value = Math.max(1, Math.round(Number(vbapCartXSizeInputEl.value) || 1));
      app.vbapCartesianState.xSize = value;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateVbapCartesian();
      invoke('control_vbap_cart_x_size', { value });
    });
  }

  if (vbapCartYSizeInputEl) {
    vbapCartYSizeInputEl.addEventListener('change', () => {
      const value = Math.max(1, Math.round(Number(vbapCartYSizeInputEl.value) || 1));
      app.vbapCartesianState.ySize = value;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateVbapCartesian();
      invoke('control_vbap_cart_y_size', { value });
    });
  }

  if (vbapCartZSizeInputEl) {
    vbapCartZSizeInputEl.addEventListener('change', () => {
      const value = Math.max(1, Math.round(Number(vbapCartZSizeInputEl.value) || 1));
      app.vbapCartesianState.zSize = value;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateVbapCartesian();
      invoke('control_vbap_cart_z_size', { value });
    });
  }

  if (vbapCartZNegSizeInputEl) {
    vbapCartZNegSizeInputEl.addEventListener('change', () => {
      const value = Math.max(0, Math.round(Number(vbapCartZNegSizeInputEl.value) || 0));
      app.vbapCartesianState.zNegSize = value;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateVbapCartesian();
      invoke('control_vbap_cart_z_neg_size', { value });
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
      if (!['vbap', 'experimental_distance'].includes(value)) return;
      if (app.renderBackendState.selection === value) return;
      app.renderBackendState.selection = value;
      app.evaluationModeState.selection = value === 'experimental_distance' ? 'realtime' : 'auto';
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateRenderBackend();
      invoke('control_render_backend', { value });
    });
  }

  if (renderEvaluationModeSelectEl) {
    renderEvaluationModeSelectEl.addEventListener('change', () => {
      const backend = app.renderBackendState.effective || app.renderBackendState.selection || 'vbap';
      const value = String(renderEvaluationModeSelectEl.value || '').trim().toLowerCase();
      const allowed = backend === 'experimental_distance'
        ? ['realtime']
        : ['auto', 'precomputed_polar', 'precomputed_cartesian'];
      if (!allowed.includes(value)) return;
      if (app.evaluationModeState.selection === value) return;
      app.evaluationModeState.selection = value;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateVbapMode();
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
      invoke('control_vbap_polar_azimuth_resolution', { value });
    });
  }

  if (vbapPolarElevationResolutionInputEl) {
    vbapPolarElevationResolutionInputEl.addEventListener('change', () => {
      const value = Math.max(1, Math.round(Number(vbapPolarElevationResolutionInputEl.value) || 1));
      app.vbapPolarState.elevationResolution = value;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateVbapPolar();
      invoke('control_vbap_polar_elevation_resolution', { value });
    });
  }

  if (vbapPolarDistanceResInputEl) {
    vbapPolarDistanceResInputEl.addEventListener('change', () => {
      const value = Math.max(1, Math.round(Number(vbapPolarDistanceResInputEl.value) || 1));
      app.vbapPolarState.distanceRes = value;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateVbapPolar();
      invoke('control_vbap_polar_distance_res', { value });
    });
  }

  if (vbapPolarDistanceMaxInputEl) {
    vbapPolarDistanceMaxInputEl.addEventListener('change', () => {
      const value = Math.max(0.01, Number(vbapPolarDistanceMaxInputEl.value) || 2);
      app.vbapPolarState.distanceMax = value;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateVbapPolar();
      invoke('control_vbap_polar_distance_max', { value });
    });
  }

  if (vbapPositionInterpolationToggleEl) {
    vbapPositionInterpolationToggleEl.addEventListener('change', () => {
      const enabled = vbapPositionInterpolationToggleEl.checked;
      app.vbapPositionInterpolation = enabled;
      app.vbapRecomputing = true;
      renderVbapStatus();
      updateVbapPositionInterpolation();
      invoke('control_vbap_position_interpolation', { enable: enabled ? 1 : 0 });
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
