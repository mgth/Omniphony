/**
 * Spread display controls.
 *
 * Extracted from app.js (lines 3662-3710).
 */

import { app, dirty } from '../state.js';
import { t, tf } from '../i18n.js';
import { formatNumber } from '../coordinates.js';
import { scheduleUIFlush } from '../flush.js';
import { inRendererPanel } from '../ui/panel-roots.js';

function getSpreadInfoEl() { return inRendererPanel('spreadInfo'); }
function getSpreadMinSliderEl() { return inRendererPanel('spreadMinSlider'); }
function getSpreadMaxSliderEl() { return inRendererPanel('spreadMaxSlider'); }
function getSpreadMinValEl() { return inRendererPanel('spreadMinVal'); }
function getSpreadMaxValEl() { return inRendererPanel('spreadMaxVal'); }
function getSpreadFromDistanceToggleEl() { return inRendererPanel('spreadFromDistanceToggle'); }
function getSpreadFromDistanceParamsEl() { return inRendererPanel('spreadFromDistanceParams'); }
function getSpreadDistanceRangeSliderEl() { return inRendererPanel('spreadDistanceRangeSlider'); }
function getSpreadDistanceRangeValEl() { return inRendererPanel('spreadDistanceRangeVal'); }
function getSpreadDistanceCurveSliderEl() { return inRendererPanel('spreadDistanceCurveSlider'); }
function getSpreadDistanceCurveValEl() { return inRendererPanel('spreadDistanceCurveVal'); }

export function renderSpreadDisplay() {
  const spreadInfoEl = getSpreadInfoEl();
  const spreadMinSliderEl = getSpreadMinSliderEl();
  const spreadMaxSliderEl = getSpreadMaxSliderEl();
  const spreadMinValEl = getSpreadMinValEl();
  const spreadMaxValEl = getSpreadMaxValEl();
  const spreadFromDistanceToggleEl = getSpreadFromDistanceToggleEl();
  const spreadFromDistanceParamsEl = getSpreadFromDistanceParamsEl();
  const spreadDistanceRangeSliderEl = getSpreadDistanceRangeSliderEl();
  const spreadDistanceRangeValEl = getSpreadDistanceRangeValEl();
  const spreadDistanceCurveSliderEl = getSpreadDistanceCurveSliderEl();
  const spreadDistanceCurveValEl = getSpreadDistanceCurveValEl();
  if (!spreadInfoEl) return;
  const minDeg = app.spreadState.min === null ? null : app.spreadState.min * 180.0;
  const maxDeg = app.spreadState.max === null ? null : app.spreadState.max * 180.0;
  const minText = minDeg === null ? '—' : formatNumber(minDeg, 0);
  const maxText = maxDeg === null ? '—' : formatNumber(maxDeg, 0);
  const modeText = app.spreadState.fromDistance === null ? '—' : app.spreadState.fromDistance ? t('spread.mode.distance') : t('spread.mode.objectSize');
  spreadInfoEl.textContent = tf('spread.summary', {
    min: `${minText}°`,
    max: `${maxText}°`,
    mode: modeText
  });
  if (spreadMinSliderEl) {
    const value = minDeg === null ? 0 : minDeg;
    spreadMinSliderEl.value = String(value);
  }
  if (spreadMaxSliderEl) {
    const value = maxDeg === null ? 180 : maxDeg;
    spreadMaxSliderEl.value = String(value);
  }
  if (spreadMinValEl) {
    spreadMinValEl.textContent = minDeg === null ? '—' : `${formatNumber(minDeg, 0)}°`;
  }
  if (spreadMaxValEl) {
    spreadMaxValEl.textContent = maxDeg === null ? '—' : `${formatNumber(maxDeg, 0)}°`;
  }
  if (spreadFromDistanceToggleEl) {
    spreadFromDistanceToggleEl.checked = app.spreadState.fromDistance === true;
  }
  if (spreadFromDistanceParamsEl) {
    spreadFromDistanceParamsEl.classList.toggle('open', app.spreadState.fromDistance === true);
  }
  if (spreadDistanceRangeSliderEl && app.spreadState.distanceRange !== null) {
    spreadDistanceRangeSliderEl.value = String(app.spreadState.distanceRange);
  }
  if (spreadDistanceRangeValEl) {
    const v = app.spreadState.distanceRange === null ? '—' : formatNumber(app.spreadState.distanceRange, 2);
    spreadDistanceRangeValEl.textContent = v;
  }
  if (spreadDistanceCurveSliderEl && app.spreadState.distanceCurve !== null) {
    spreadDistanceCurveSliderEl.value = String(app.spreadState.distanceCurve);
  }
  if (spreadDistanceCurveValEl) {
    const v = app.spreadState.distanceCurve === null ? '—' : formatNumber(app.spreadState.distanceCurve, 2);
    spreadDistanceCurveValEl.textContent = v;
  }
}

export function updateSpreadDisplay() {
  dirty.spread = true;
  scheduleUIFlush();
}
