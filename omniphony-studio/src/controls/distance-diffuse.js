/**
 * Distance-diffuse controls.
 *
 * Extracted from app.js (lines 4042-4078).
 */

import { app, dirty } from '../state.js';
import { formatNumber } from '../coordinates.js';
import { scheduleUIFlush } from '../flush.js';
import { inRendererInfoModals, inRendererPanel } from '../ui/panel-roots.js';

function getDistanceDiffuseToggleEl() { return inRendererPanel('distanceDiffuseToggle'); }
function getDistanceDiffuseParamsEl() { return inRendererPanel('distanceDiffuseParams'); }
function getDistanceDiffuseThresholdSliderEl() { return inRendererPanel('distanceDiffuseThresholdSlider'); }
function getDistanceDiffuseThresholdValEl() { return inRendererPanel('distanceDiffuseThresholdVal'); }
function getDistanceDiffuseCurveSliderEl() { return inRendererPanel('distanceDiffuseCurveSlider'); }
function getDistanceDiffuseCurveValEl() { return inRendererPanel('distanceDiffuseCurveVal'); }
function getDistanceDiffuseInfoModalEl() { return inRendererInfoModals('distanceDiffuseInfoModal'); }
function getSpreadFromDistanceInfoModalEl() { return inRendererInfoModals('spreadFromDistanceInfoModal'); }

export function renderDistanceDiffuseUI() {
  const distanceDiffuseToggleEl = getDistanceDiffuseToggleEl();
  const distanceDiffuseParamsEl = getDistanceDiffuseParamsEl();
  const distanceDiffuseThresholdSliderEl = getDistanceDiffuseThresholdSliderEl();
  const distanceDiffuseThresholdValEl = getDistanceDiffuseThresholdValEl();
  const distanceDiffuseCurveSliderEl = getDistanceDiffuseCurveSliderEl();
  const distanceDiffuseCurveValEl = getDistanceDiffuseCurveValEl();
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
  const distanceDiffuseInfoModalEl = getDistanceDiffuseInfoModalEl();
  if (!distanceDiffuseInfoModalEl) return;
  distanceDiffuseInfoModalEl.classList.toggle('open', Boolean(open));
}

export function setSpreadFromDistanceInfoModalOpen(open) {
  const spreadFromDistanceInfoModalEl = getSpreadFromDistanceInfoModalEl();
  if (!spreadFromDistanceInfoModalEl) return;
  spreadFromDistanceInfoModalEl.classList.toggle('open', Boolean(open));
}
