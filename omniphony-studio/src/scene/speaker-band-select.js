/**
 * Crossover-band selector ("Heatmap band").
 *
 * The per-speaker heatmap that originally owned this control is gone, but the
 * band index it drives is still read by `computeEffectiveRenderPosition`
 * (sources.js) and `getObjectDominantSpeakerText` (speakers.js) to pick which
 * crossover band's gains to visualise. Keep this tiny sync helper so those
 * features still work for multi-band layouts.
 */

import { app } from '../state.js';
import { t, onLocaleChange } from '../i18n.js';
import { computeCrossoverBandLabels } from '../crossover-bands.js';

function getSpeakerHeatmapBandSelectEl() {
  return document.getElementById('speakerHeatmapBandSelect');
}

export function syncSpeakerHeatmapBandSelect() {
  const selectEl = getSpeakerHeatmapBandSelectEl();
  const labels = computeCrossoverBandLabels(app.currentLayoutSpeakers, { includeSingleBand: true });
  const hasLayoutSpeakers = Array.isArray(app.currentLayoutSpeakers) && app.currentLayoutSpeakers.length > 0;
  const desiredIndex = Math.max(0, Math.round(Number(app.speakerHeatmapBandIndex) || 0));
  const maxIndex = Math.max(0, labels.length - 1);
  if (hasLayoutSpeakers) {
    app.speakerHeatmapBandIndex = Math.max(0, Math.min(maxIndex, desiredIndex));
  }
  if (!selectEl) {
    return labels;
  }
  const visibleIndex = hasLayoutSpeakers
    ? app.speakerHeatmapBandIndex
    : Math.max(0, Math.min(maxIndex, desiredIndex));

  // Option list: one per crossover band, plus an "All bands" entry (only when
  // there's more than one band) for the heatmap's all-bands composite.
  const optionDefs = labels.map((label, index) => ({ value: String(index), text: label }));
  if (labels.length > 1) {
    optionDefs.push({ value: 'all', text: t('heatmap.bandAll') });
  }
  // Compare labels as well as values: the values are locale-independent, so a
  // value-only check would leave stale wording behind after a locale switch.
  const existing = Array.from(selectEl.options);
  const needsRebuild = existing.length !== optionDefs.length
    || existing.some((option, index) => option.value !== optionDefs[index].value
      || option.textContent !== optionDefs[index].text);
  if (needsRebuild) {
    selectEl.replaceChildren();
    optionDefs.forEach((def) => {
      const option = document.createElement('option');
      option.value = def.value;
      option.textContent = def.text;
      selectEl.appendChild(option);
    });
  }
  selectEl.value = app.speakerHeatmapAllBands && labels.length > 1
    ? 'all'
    : String(visibleIndex);
  return labels;
}

/**
 * Refresh EVERY crossover-band selector after a layout/band change.
 *
 * Call this rather than a single selector's sync: the band list is derived from
 * the speaker layout, so a caller that refreshes one and not the other leaves a
 * stale dropdown — which is exactly how the global heatmap's selector stopped
 * following new bands. One entry point means a future selector is picked up by
 * every existing call site for free.
 */
export function syncCrossoverBandSelects() {
  const labels = syncSpeakerHeatmapBandSelect();
  syncGlobalEnergyBandSelect();
  return labels;
}

/**
 * Same options for the global energy heatmap, minus the all-bands entry: that
 * composite blends band *colours*, which a diverging dB scale has no room for.
 */
export function syncGlobalEnergyBandSelect() {
  const selectEl = document.getElementById('globalEnergyHeatmapBandSelect');
  const labels = computeCrossoverBandLabels(app.currentLayoutSpeakers, {
    includeSingleBand: true,
  }) || [t('heatmap.bandFull')];
  const maxIndex = Math.max(0, labels.length - 1);
  const desired = Math.max(0, Math.round(Number(app.globalEnergyHeatmapBandIndex) || 0));
  app.globalEnergyHeatmapBandIndex = Math.min(maxIndex, desired);
  if (!selectEl) return labels;

  // Compare the text too: the values are locale-independent, so a value-only
  // check would leave stale wording behind after a locale switch.
  const existing = Array.from(selectEl.options);
  const needsRebuild = existing.length !== labels.length
    || existing.some((option, index) => option.value !== String(index)
      || option.textContent !== labels[index]);
  if (needsRebuild) {
    selectEl.replaceChildren();
    labels.forEach((label, index) => {
      const option = document.createElement('option');
      option.value = String(index);
      option.textContent = label;
      selectEl.appendChild(option);
    });
  }
  selectEl.value = String(app.globalEnergyHeatmapBandIndex);
  return labels;
}

// Several option labels are prose ("Full band", "All bands"), so every list has
// to be rebuilt when the locale changes — hence the shared entry point rather
// than one selector's sync. Registered here rather than in app.js's central
// onLocaleChange handler because `setLocale` isolates each listener: an
// unrelated failure elsewhere in that handler can't then swallow this refresh.
onLocaleChange(syncCrossoverBandSelects);
