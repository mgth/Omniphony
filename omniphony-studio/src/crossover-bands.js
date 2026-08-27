import { t } from './i18n.js';

// Band edges are derived by the backend (`layouts::crossover_cutoffs`) and
// arrive with every layout as `crossoverCutoffs`, plus on the events that move
// them (a speaker's frequency limits, or its spatialize flag — a
// non-spatialized speaker's cutoffs are not band boundaries for the objects
// being panned). This module keeps only the label *formatting*, which is an
// i18n concern.

function formatHz(value) {
  return value >= 1000 ? `${(value / 1000).toFixed(value % 1000 === 0 ? 0 : 1)}k` : `${value}`;
}

/**
 * Band edges from the backend's interior cutoffs: `[0, ...cutoffs, Infinity]`.
 *
 * Only the interior edges cross the wire — JSON has no infinity — so the open
 * ends are capped here.
 */
export function crossoverBandEdges(cutoffs) {
  if (!Array.isArray(cutoffs) || cutoffs.length === 0) {
    return [0, Infinity];
  }
  return [0, ...cutoffs, Infinity];
}

// Band labels are frequency ranges ("< 100 Hz", "1k–4k Hz"), which need no
// translation, except the single-band case whose label is prose. It defaults to
// the localized "Full band" and is re-resolved on every call, so callers that
// rebuild on locale change pick the new wording up for free.
export function crossoverBandLabels(
  cutoffs,
  { includeSingleBand = false, singleBandLabel = null, useUnicodeGte = false, useUnicodeDash = false } = {},
) {
  const edges = crossoverBandEdges(cutoffs);
  if (edges.length <= 2 && !includeSingleBand) {
    return null;
  }
  if (edges.length <= 2) {
    return [singleBandLabel ?? t('heatmap.bandFull')];
  }
  const gte = useUnicodeGte ? '\u2265' : '>=';
  const dash = useUnicodeDash ? '\u2013' : '-';
  return edges.slice(0, -1).map((lo, index) => {
    const hi = edges[index + 1];
    if (lo === 0) return `< ${formatHz(hi)} Hz`;
    if (hi === Infinity) return `${gte} ${formatHz(lo)} Hz`;
    return `${formatHz(lo)}${dash}${formatHz(hi)} Hz`;
  });
}
