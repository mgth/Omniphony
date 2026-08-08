/**
 * Global energy heatmap — ray-marched volume of the TOTAL energy the renderer
 * puts out at each grid cell, all speakers summed.
 *
 * The renderer ships one amplitude per cell per band (`√Σ gᵢ²`, see
 * `renderer::band_gaintable::serialize_energy`), so `1.0` is exactly unit
 * energy. What is drawn is the deviation from it, in dB:
 *
 *     dB(cell) = 20·log10(amplitude)
 *
 * mapped to a diverging scale centred on 0: transparent at 0 dB, red towards
 * `+scale`, blue towards `−scale`. A panner that conserves energy is therefore
 * invisible, and everything that shows is a real gain or loss — a hot seam
 * between speaker triplets, or the hole an uncovered pole leaves.
 *
 * Unlike the per-speaker heatmap this uses an ABSOLUTE level scale (`maxLevel:
 * 1`): the alpha already means "fraction of the dB scale", so letting the
 * volume core self-normalise would make a 2 dB spread look like a 20 dB one.
 *
 * Runs alongside the per-speaker heatmap: both fields are subscribed for
 * independently, so showing this one does not stop the other from refreshing.
 *
 * Driven by `globalEnergyHeatmapEnabled` + `globalEnergyHeatmapScaleDb`.
 */

import { app } from '../state.js';
import { EnergyVolume } from './energy-volume-core.js';
import { VOLUME_REBUILD_INTERVAL_MS, clampVolumeGamma } from './object-energy-shared.js';
import { getGlobalEnergyTable } from './speaker-gaintable.js';

const volume = new EnergyVolume();

/** Bounds of the user-facing dB scale, and its default. */
export const GLOBAL_ENERGY_SCALE_MIN_DB = 1;
export const GLOBAL_ENERGY_SCALE_MAX_DB = 40;
export const GLOBAL_ENERGY_SCALE_DEFAULT_DB = 6;

// Below this amplitude a cell is treated as silent rather than "very negative":
// 20·log10 of a true zero is -Infinity, which would saturate the blue end and
// hide the difference between "a bit quiet" and "nothing at all".
const SILENCE_AMPLITUDE = 1e-6;

let lastBuildSig = null;
function sigEqual(a, b) {
  if (!a || !b || a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}

export function hideGlobalEnergyVolume() {
  volume.hide();
  lastBuildSig = null;
}

export function clearGlobalEnergyVolume() {
  volume.dispose();
}

export function globalEnergyScaleDb() {
  const raw = Number(app.globalEnergyHeatmapScaleDb);
  if (!Number.isFinite(raw) || raw <= 0) return GLOBAL_ENERGY_SCALE_DEFAULT_DB;
  return Math.max(GLOBAL_ENERGY_SCALE_MIN_DB, Math.min(GLOBAL_ENERGY_SCALE_MAX_DB, raw));
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

export function refreshGlobalEnergyVolume(nowMs) {
  const table = getGlobalEnergyTable();
  if (!app.globalEnergyHeatmapEnabled || !table) {
    hideGlobalEnergyVolume();
    return;
  }

  // Static field (a precomputed table, no live levels): rebuild only when an
  // input actually changes, or the n³ resample + texture upload would run on
  // every throttle tick — the freeze the per-speaker heatmap already learned about.
  const ratio = app.roomRatio || {};
  const scaleDb = globalEnergyScaleDb();
  const sig = [
    table,
    scaleDb,
    Number(app.globalEnergyHeatmapBandIndex) || 0,
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
  if (now - (app.lastGlobalEnergyVolumeAt || 0) < refreshMs) return;
  app.lastGlobalEnergyVolumeAt = now;
  lastBuildSig = sig;

  const { nx, ny, nz, bands, zPositions } = table;
  const nbands = bands.length;
  if (nbands < 1) {
    hideGlobalEnergyVolume();
    return;
  }
  const bandIndex = Math.max(
    0,
    Math.min(nbands - 1, Math.round(Number(app.globalEnergyHeatmapBandIndex) || 0)),
  );
  const amplitudes = bands[bandIndex].gains;
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

  const invScale = 1 / scaleDb;
  volume.update({
    resolution: app.objectEnergyHeatmapResolution,
    opacity: app.objectEnergyHeatmapOpacity,
    mix: app.objectEnergyVolumeMix,
    gammaAccumulate: clampVolumeGamma('accumulate', app.objectEnergyVolumeGammaAccumulate),
    gammaMip: clampVolumeGamma('mip', app.objectEnergyVolumeGammaMip),
    smooth: app.volumeSmoothInterpolation,
    // Absolute: alpha is already a fraction of the dB scale (see the header).
    maxLevel: 1,
    sampleColor: (ow, od, oh, out) => {
      const amp = amplitudes[cellIndex(ow, od, oh)];
      // Silence is the extreme deficit, not a -Infinity that breaks the scale.
      const t = amp > SILENCE_AMPLITUDE
        ? Math.max(-1, Math.min(1, 20 * Math.log10(amp) * invScale))
        : -1;
      if (t >= 0) {
        out[0] = 1; out[1] = 0; out[2] = 0; // excess → red
      } else {
        out[0] = 0; out[1] = 0.35; out[2] = 1; // deficit → blue
      }
      out[3] = Math.abs(t); // 0 dB → fully transparent
    },
  });
}
