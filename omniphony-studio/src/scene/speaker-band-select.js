/**
 * Crossover-band selector — ONE selector for every heatmap.
 *
 * The band index drives the per-speaker heatmap, the global energy heatmap,
 * the discontinuity heatmap, `computeEffectiveRenderPosition` (sources.js) and
 * `getObjectDominantSpeakerText` (speakers.js). It used to be one selector per
 * display, but overlaying heatmaps of *different* bands only reads as
 * confusion, so a single `heatmapBandIndex`/`heatmapAllBands` pair is the band
 * context for the whole scene.
 *
 * The "All bands" entry (only offered for multi-band layouts) maps to
 * `heatmapAllBands`: each display renders its own composite — level-weighted
 * frequency colouring for the per-speaker volume, power sum for the energy
 * field, worst-band jump for the discontinuity field. The numeric index is
 * kept alongside for the readouts that always need a single band.
 */

import { app } from '../state.js';
import { t, onLocaleChange } from '../i18n.js';
import { crossoverBandLabels } from '../crossover-bands.js';
import { renderBandCursor } from '../controls/band-cursor.js';

/**
 * Refresh the shared crossover-band selector after a layout/band change.
 * Rebuilds the options when the labels change (comparing text as well as
 * value: the values are locale-independent, so a value-only check would leave
 * stale wording behind after a locale switch), and re-applies the current
 * selection.
 *
 * The band list is clamped for *display only* — `app.heatmapBandIndex` and
 * `app.heatmapAllBands` are never written here. This function runs against
 * whatever layout is currently known, and at boot that is no layout at all:
 * `loadEffectiveRenderPrefs` restores the persisted selection and syncs the UI
 * before the first layout event lands, so `currentLayoutCutoffs` is still
 * empty and the band list is the one-entry "Full band" fallback. Clamping the
 * state there would rewrite every restored selection to band 0 with
 * `heatmapAllBands` cleared, and the preference could never survive a restart.
 * The same applies to a single-band layout, which would otherwise wipe the
 * selection the user gets back when a multi-band layout returns.
 *
 * Leaving the state unclamped is safe: every consumer range-checks the index
 * itself (the volume shaders clamp to `nbands - 1`, the readouts fall back to
 * the full-band gains) and every all-bands composite is guarded on having more
 * than one band.
 */
export function syncCrossoverBandSelects() {
  const selectEl = document.getElementById('heatmapBandSelect');
  const labels = crossoverBandLabels(app.currentLayoutCutoffs, {
    includeSingleBand: true,
  }) || [t('heatmap.bandFull')];
  const maxIndex = Math.max(0, labels.length - 1);
  const desired = Math.max(0, Math.round(Number(app.heatmapBandIndex) || 0));
  // What the controls show for the current band list, without touching state.
  const shownValue = app.heatmapAllBands && labels.length > 1
    ? 'all'
    : String(Math.min(maxIndex, desired));
  // The floating cursor over the 3D view mirrors the same selection.
  renderBandCursor(labels, shownValue);
  if (!selectEl) return labels;

  // One option per band, plus "All bands" for multi-band layouts.
  const optionDefs = labels.map((label, index) => ({ value: String(index), text: label }));
  if (labels.length > 1) {
    optionDefs.push({ value: 'all', text: t('heatmap.bandAll') });
  }
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
  selectEl.value = shownValue;
  return labels;
}

// The option labels are prose ("Full band", "All bands"), so the list has to
// be rebuilt when the locale changes. Registered here rather than in app.js's
// central onLocaleChange handler because `setLocale` isolates each listener:
// an unrelated failure elsewhere in that handler can't then swallow this
// refresh.
onLocaleChange(syncCrossoverBandSelects);
