/**
 * Distance-diffuse controls.
 *
 * Extracted from app.js (lines 4042-4078).
 */

import { app, dirty } from '../state.js';
import { formatNumber } from '../coordinates.js';
import { scheduleUIFlush } from '../flush.js';

// DOM refs
const distanceDiffuseToggleEl = document.getElementById('distanceDiffuseToggle');
const distanceDiffuseParamsEl = document.getElementById('distanceDiffuseParams');
const distanceDiffuseThresholdSliderEl = document.getElementById('distanceDiffuseThresholdSlider');
const distanceDiffuseThresholdValEl = document.getElementById('distanceDiffuseThresholdVal');
const distanceDiffuseCurveSliderEl = document.getElementById('distanceDiffuseCurveSlider');
const distanceDiffuseCurveValEl = document.getElementById('distanceDiffuseCurveVal');
const distanceDiffuseInfoModalEl = document.getElementById('distanceDiffuseInfoModal');
const spreadFromDistanceInfoModalEl = document.getElementById('spreadFromDistanceInfoModal');

export function renderDistanceDiffuseUI() {
  if (distanceDiffuseToggleEl) {
    distanceDiffuseToggleEl.checked = app.distanceDiffuseState.enabled === true;
  }
  if (distanceDiffuseParamsEl) {
    distanceDiffuseParamsEl.classList.toggle('open', app.distanceDiffuseState.enabled === true);
  }
  if (distanceDiffuseThresholdSliderEl && app.distanceDiffuseState.threshold !== null) {
    distanceDiffuseThresholdSliderEl.value = String(app.distanceDiffuseState.threshold);
  }
  if (distanceDiffuseThresholdValEl) {
    const v = app.distanceDiffuseState.threshold === null ? '—' : formatNumber(app.distanceDiffuseState.threshold, 2);
    distanceDiffuseThresholdValEl.textContent = v;
  }
  if (distanceDiffuseCurveSliderEl && app.distanceDiffuseState.curve !== null) {
    distanceDiffuseCurveSliderEl.value = String(app.distanceDiffuseState.curve);
  }
  if (distanceDiffuseCurveValEl) {
    const v = app.distanceDiffuseState.curve === null ? '—' : formatNumber(app.distanceDiffuseState.curve, 2);
    distanceDiffuseCurveValEl.textContent = v;
  }
}

export function updateDistanceDiffuseUI() {
  dirty.distanceDiffuse = true;
  scheduleUIFlush();
}

export function setDistanceDiffuseInfoModalOpen(open) {
  if (!distanceDiffuseInfoModalEl) return;
  distanceDiffuseInfoModalEl.classList.toggle('open', Boolean(open));
}

export function setSpreadFromDistanceInfoModalOpen(open) {
  if (!spreadFromDistanceInfoModalEl) return;
  spreadFromDistanceInfoModalEl.classList.toggle('open', Boolean(open));
}
