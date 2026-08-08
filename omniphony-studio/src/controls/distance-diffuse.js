/**
 * Distance-diffuse controls.
 *
 * Extracted from app.js (lines 4042-4078).
 */

import { app, dirty } from '../state.js';
import { formatNumber } from '../coordinates.js';
import { scheduleUIFlush } from '../flush.js';
import { t } from '../i18n.js';
import { inRendererInfoModals, inRendererPanel } from '../ui/panel-roots.js';

export const MIRROR_AXES = ['x', 'y', 'z'];

/**
 * Name of the symmetry the selected sign flips compose into. The flips combine
 * into a single mirror image, so the parity of the set decides: one flip is a
 * reflection in the plane normal to that axis, two flips a half-turn about the
 * axis left untouched, and three a point inversion through the origin.
 *
 * Returns an i18n key so the label follows the locale like any other string.
 */
export function symmetryI18nKey({ x, y, z }) {
  const flips = MIRROR_AXES.filter((axis) => ({ x, y, z })[axis]);
  switch (flips.length) {
    case 0: return 'distance.symmetry.none';
    case 1: return `distance.symmetry.plane${flips[0].toUpperCase()}`;
    // Two flips leave one axis fixed: that is the rotation axis.
    case 2: return `distance.symmetry.axis${MIRROR_AXES.find((a) => !flips.includes(a)).toUpperCase()}`;
    default: return 'distance.symmetry.origin';
  }
}

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
  const distanceDiffuseMetricSelectEl = inRendererPanel('distanceDiffuseMetricSelect');
  if (distanceDiffuseMetricSelectEl) {
    distanceDiffuseMetricSelectEl.value = ['spherical', 'chebyshev'].includes(app.distanceDiffuseState.metric)
      ? app.distanceDiffuseState.metric
      : 'spherical';
  }
  const axes = app.distanceDiffuseState.mirrorAxes;
  for (const axis of MIRROR_AXES) {
    const el = inRendererPanel(`distanceDiffuseMirror${axis.toUpperCase()}`);
    if (el) el.checked = axes[axis] === true;
  }
  const symmetryEl = inRendererPanel('distanceDiffuseSymmetry');
  if (symmetryEl) {
    // Carry the key on the element too: `applyStaticTranslations` re-reads
    // data-i18n on every locale change, so the derived label follows along
    // without the panel having to re-render.
    const key = symmetryI18nKey(axes);
    symmetryEl.setAttribute('data-i18n', key);
    symmetryEl.textContent = t(key);
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
