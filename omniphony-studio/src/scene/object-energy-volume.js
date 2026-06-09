/**
 * Object energy field — ray-marched volume (object-driven, inverse-square).
 *
 * Thin provider over `EnergyVolume` (energy-volume-core.js): the per-cell energy
 * is the inverse-square deposit of every live object's level. The Three.js volume
 * machinery (texture, shader, box) lives in the shared core.
 */

import { app } from '../state.js';
import { EnergyVolume } from './energy-volume-core.js';
import {
  VOLUME_REBUILD_INTERVAL_MS,
  activeObjects,
  collectActiveObjects,
  clampVolumeGamma,
  colormapIndex,
} from './object-energy-shared.js';

const volume = new EnergyVolume();

export function hideObjectEnergyVolume() {
  volume.hide();
}

export function clearObjectEnergyVolume() {
  volume.dispose();
}

export function refreshObjectEnergyVolume(nowMs) {
  if (!app.objectEnergyHeatmapEnabled) {
    volume.hide();
    return;
  }

  const now = Number.isFinite(nowMs) ? nowMs : performance.now();
  const refreshMs = Number(app.volumeRefreshMs) > 0 ? Number(app.volumeRefreshMs) : VOLUME_REBUILD_INTERVAL_MS;
  if (now - (app.lastObjectEnergyHeatmapAt || 0) < refreshMs) {
    return;
  }
  app.lastObjectEnergyHeatmapAt = now;

  const objectCount = collectActiveObjects();
  if (objectCount === 0) {
    volume.hide();
    return;
  }

  const r0 = Math.max(0.01, Math.min(1.0, Number(app.objectEnergyHeatmapFalloffRadius) || 0.12));
  const r0sq = r0 * r0;

  volume.update({
    resolution: app.objectEnergyHeatmapResolution,
    opacity: app.objectEnergyHeatmapOpacity,
    mix: app.objectEnergyVolumeMix,
    gammaAccumulate: clampVolumeGamma('accumulate', app.objectEnergyVolumeGammaAccumulate),
    gammaMip: clampVolumeGamma('mip', app.objectEnergyVolumeGammaMip),
    colormap: colormapIndex(app.objectEnergyColormap),
    customStops: app.objectCustomGradientStops,
    smooth: app.volumeSmoothInterpolation,
    sampleEnergy: (ow, od, oh) => {
      let energy = 0;
      for (let o = 0; o < objectCount; o += 1) {
        const obj = activeObjects[o];
        const dx = ow - obj.x;
        const dy = od - obj.y;
        const dz = oh - obj.z;
        energy += obj.energy / (dx * dx + dy * dy + dz * dz + r0sq);
      }
      return energy;
    },
  });
}
