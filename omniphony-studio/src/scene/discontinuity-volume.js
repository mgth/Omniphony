/**
 * Discontinuity heatmap — ray-marched volume of the BREAKS in speaker usage:
 * per grid cell, the worst jump to any of its 6 neighbours, as computed by the
 * renderer (`renderer::band_gaintable`, targets −2/−3).
 *
 * Two metrics, selected by `discontinuityHeatmapMode`:
 * - 'gain': L2 distance between the cells' energy-normalised gain vectors.
 *   Level is stripped (the energy heatmap already shows it), so what remains
 *   is pure configuration change: a smooth pan or a triplet-edge crossing
 *   reads near 0, a hard switch between disjoint speaker sets reads √2.
 * - 'centroid': room distance the gain²-weighted speaker centroid jumps —
 *   how far the sound image physically moves between adjacent positions.
 *
 * The values are raw jumps, not gradients: a true discontinuity keeps its
 * value as the grid refines while a fast-but-smooth transition fades, so in
 * the volume a break shows as a thin bright sheet, not a diffuse glow.
 *
 * Sequential amber scale on an ABSOLUTE reference (`maxLevel: 1`), transparent
 * at 0 and saturating at `discontinuityHeatmapScale` — self-normalising would
 * make a field of tiny seams look identical to one full of hard breaks.
 */

import { app } from '../state.js';
import { EnergyVolume } from './energy-volume-core.js';
import { VOLUME_REBUILD_INTERVAL_MS, clampVolumeGamma } from './object-energy-shared.js';
import { getDiscontinuityTable } from './speaker-gaintable.js';

const volume = new EnergyVolume();

/** Bounds of the user-facing full-scale jump, and its default. */
export const DISCONTINUITY_SCALE_MIN = 0.05;
export const DISCONTINUITY_SCALE_MAX = 2;
export const DISCONTINUITY_SCALE_DEFAULT = 0.5;

let lastBuildSig = null;
function sigEqual(a, b) {
  if (!a || !b || a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}

export function hideDiscontinuityVolume() {
  volume.hide();
  lastBuildSig = null;
}

export function clearDiscontinuityVolume() {
  volume.dispose();
}

export function discontinuityScale() {
  const raw = Number(app.discontinuityHeatmapScale);
  if (!Number.isFinite(raw) || raw <= 0) return DISCONTINUITY_SCALE_DEFAULT;
  return Math.max(DISCONTINUITY_SCALE_MIN, Math.min(DISCONTINUITY_SCALE_MAX, raw));
}

function clampIdx(value, n) {
  if (value < 0) return 0;
  if (value > n - 1) return n - 1;
  return value;
}

function nearestIndex(positions, n, value) {
  let best = 0;
  let bestDist = Infinity;
  for (let i = 0; i < n; i += 1) {
    const d = Math.abs(positions[i] - value);
    if (d < bestDist) {
      bestDist = d;
      best = i;
    }
  }
  return best;
}

export function refreshDiscontinuityVolume(nowMs) {
  const table = getDiscontinuityTable();
  if (!app.discontinuityHeatmapEnabled || !table) {
    hideDiscontinuityVolume();
    return;
  }

  // Static field (a precomputed table, no live levels): rebuild only when an
  // input actually changes, or the n³ resample + texture upload would run on
  // every throttle tick — the freeze the per-speaker heatmap already learned about.
  const ratio = app.roomRatio || {};
  const scale = discontinuityScale();
  const sig = [
    table,
    scale,
    Number(app.heatmapBandIndex) || 0,
    app.heatmapAllBands ? 1 : 0,
    app.volumeSmoothInterpolation ? 1 : 0,
    app.objectEnergyHeatmapResolution,
    app.objectEnergyHeatmapOpacity,
    app.objectEnergyVolumeMix,
    app.objectEnergyVolumeGammaAccumulate,
    app.objectEnergyVolumeGammaMip,
    ratio.height, ratio.lower, ratio.width, ratio.rear, ratio.length,
  ];
  if (sigEqual(sig, lastBuildSig)) return;

  const now = Number.isFinite(nowMs) ? nowMs : performance.now();
  const refreshMs = Number(app.volumeRefreshMs) > 0
    ? Number(app.volumeRefreshMs)
    : VOLUME_REBUILD_INTERVAL_MS;
  if (now - (app.lastDiscontinuityVolumeAt || 0) < refreshMs) return;
  app.lastDiscontinuityVolumeAt = now;
  lastBuildSig = sig;

  const { nx, ny, nz, bands, zPositions } = table;
  const nbands = bands.length;
  if (nbands < 1) {
    hideDiscontinuityVolume();
    return;
  }
  const bandIndex = Math.max(
    0,
    Math.min(nbands - 1, Math.round(Number(app.heatmapBandIndex) || 0)),
  );
  let jumps = bands[bandIndex].gains;
  if (app.heatmapAllBands && nbands > 1) {
    // All-bands composite: the worst break in ANY band — a seam that only
    // exists in the height band is still a seam.
    const cells = jumps.length;
    const worst = new Float32Array(cells);
    for (let b = 0; b < nbands; b += 1) {
      const g = bands[b].gains;
      for (let i = 0; i < cells; i += 1) {
        if (g[i] > worst[i]) worst[i] = g[i];
      }
    }
    jumps = worst;
  }
  const nxh = nx - 1;
  const nyh = ny - 1;
  const nzh = nz - 1;

  let cachedOh = NaN;
  let cachedZi = 0;
  const lookupZi = (oh) => {
    if (oh === cachedOh) return cachedZi;
    cachedOh = oh;
    cachedZi = zPositions
      ? nearestIndex(zPositions, nz, oh)
      : clampIdx(Math.round(((oh + 1) * 0.5) * nzh), nz);
    return cachedZi;
  };
  const cellIndex = (ow, od, oh) => {
    const xi = clampIdx(Math.round(((ow + 1) * 0.5) * nxh), nx);
    const yi = clampIdx(Math.round(((od + 1) * 0.5) * nyh), ny);
    return xi + nx * (yi + ny * lookupZi(oh));
  };

  const invScale = 1 / scale;
  volume.update({
    resolution: app.objectEnergyHeatmapResolution,
    opacity: app.objectEnergyHeatmapOpacity,
    mix: app.objectEnergyVolumeMix,
    gammaAccumulate: clampVolumeGamma('accumulate', app.objectEnergyVolumeGammaAccumulate),
    gammaMip: clampVolumeGamma('mip', app.objectEnergyVolumeGammaMip),
    smooth: app.volumeSmoothInterpolation,
    // Absolute: alpha is already a fraction of the jump scale (see the header).
    maxLevel: 1,
    sampleColor: (ow, od, oh, out) => {
      const t = Math.max(0, Math.min(1, jumps[cellIndex(ow, od, oh)] * invScale));
      // Amber, warming towards red as the jump saturates — distinct from the
      // energy heatmap's red/blue diverging scale.
      out[0] = 1;
      out[1] = 0.65 - 0.45 * t;
      out[2] = 0.05;
      out[3] = t; // no jump → fully transparent
    },
  });
}
