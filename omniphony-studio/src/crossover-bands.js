function collectCutoffs(speakers) {
  if (!Array.isArray(speakers) || speakers.length === 0) {
    return [];
  }
  const cutoffs = [];
  speakers.forEach((speaker) => {
    if (Number(speaker?.spatialize) === 0) {
      return;
    }
    [speaker?.freqLow, speaker?.freqHigh].forEach((value) => {
      const numeric = Number(value);
      if (Number.isFinite(numeric) && numeric > 0) {
        cutoffs.push(numeric);
      }
    });
  });
  cutoffs.sort((a, b) => a - b);
  return cutoffs.filter((value, index) => index === 0 || Math.abs(value - cutoffs[index - 1]) >= 0.1);
}

function formatHz(value) {
  return value >= 1000 ? `${(value / 1000).toFixed(value % 1000 === 0 ? 0 : 1)}k` : `${value}`;
}

export function computeCrossoverBandEdges(speakers) {
  const cutoffs = collectCutoffs(speakers);
  if (cutoffs.length === 0) {
    return [0, Infinity];
  }
  return [0, ...cutoffs, Infinity];
}

export function computeCrossoverBandLabels(
  speakers,
  { includeSingleBand = false, singleBandLabel = 'Full band', useUnicodeGte = false, useUnicodeDash = false } = {},
) {
  const edges = computeCrossoverBandEdges(speakers);
  if (edges.length <= 2 && !includeSingleBand) {
    return null;
  }
  if (edges.length <= 2) {
    return [singleBandLabel];
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
