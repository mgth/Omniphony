/**
 * Per-speaker heatmap — ray-marched volume of a single speaker's gain field,
 * crossover-band aware.
 *
 * Reads the local band-aware gain table (one VBAP field per crossover band) and
 * renders the selected speaker:
 *  - a single band → energy(cell) = gain_band(cell, speaker)², coloured by the
 *    speaker gradient (`speakerHeatmapVolumeColormap`);
 *  - "all bands" (`heatmapAllBands`) → per cell, a level-weighted average
 *    of each band's colour (band colour = gradient at the band's log-frequency
 *    position), with alpha = total level. Uses the volume core's precoloured path.
 *
 * Driven by "Heatmap volume" (`speakerHeatmapVolumeEnabled`) + `selectedSpeakerIndex`.
 * Static (no live levels) — shows where the speaker is panned per band.
 */

import * as THREE from 'three';

import { app } from '../state.js';
import { EnergyVolume } from './energy-volume-core.js';
import {
  VOLUME_REBUILD_INTERVAL_MS,
  clampVolumeGamma,
  colormapIndex,
  makeCellIndexer,
  objectEnergyColor,
  signaturesEqual,
} from './object-energy-shared.js';
import { getSpeakerGainTable } from './speaker-gaintable.js';

const volume = new EnergyVolume();

// Signature of the last built field. The speaker field is static, so we skip the
// whole rebuild while nothing that shapes it changes (see refreshSpeakerSoloVolume).
let lastBuildSig = null;

// Audible-frequency span used to place each band on the colour gradient (log scale).
const FMIN_HZ = 20;
const FMAX_HZ = 20000;
const LOG_FMIN = Math.log(FMIN_HZ);
const LOG_SPAN = Math.log(FMAX_HZ) - LOG_FMIN;

export function hideSpeakerSoloVolume() {
  volume.hide();
}

export function clearSpeakerSoloVolume() {
  volume.dispose();
}

/** Position of a band on the [0,1] colour gradient from its log-frequency centre. */
function bandLogT(band) {
  const lo = Math.max(FMIN_HZ, Number(band.lowHz) || 0);
  const hiRaw = band.highHz == null ? FMAX_HZ : Number(band.highHz);
  const hi = Math.min(FMAX_HZ, Number.isFinite(hiRaw) && hiRaw > 0 ? hiRaw : FMAX_HZ);
  const f = Math.sqrt(lo * Math.max(lo, hi));
  const t = (Math.log(Math.max(FMIN_HZ, Math.min(FMAX_HZ, f))) - LOG_FMIN) / LOG_SPAN;
  return Math.max(0, Math.min(1, t));
}

export function refreshSpeakerSoloVolume(nowMs) {
  const table = getSpeakerGainTable();
  const speaker = app.selectedSpeakerIndex;
  if (
    !app.speakerHeatmapVolumeEnabled
    || !table
    || !Number.isInteger(speaker)
    || speaker < 0
    // The cached table is for one speaker; if the selection changed we wait for
    // the re-fetch (refreshGaintableSubscription) rather than show another speaker.
    || table.speakerIndex !== speaker
  ) {
    volume.hide();
    lastBuildSig = null;
    return;
  }

  // The speaker field is STATIC (a precomputed gain table, no live levels), so it
  // only changes when its inputs do. Skip the rebuild otherwise — the built volume
  // simply stays up. Without this the full n³ field (up to 64³ cells) + 3D-texture
  // upload ran on every throttle tick (~14 Hz), stalling the UI for as long as the
  // heatmap was visible (this is what made loading/switching feel like a freeze).
  const ratio = app.roomRatio || {};
  const sig = [
    table, speaker,
    app.heatmapAllBands ? 1 : 0,
    Number(app.heatmapBandIndex) || 0,
    app.speakerHeatmapVolumeColormap,
    app.speakerCustomGradientVersion,
    app.volumeSmoothInterpolation ? 1 : 0,
    app.objectEnergyHeatmapResolution,
    app.objectEnergyHeatmapOpacity,
    app.objectEnergyVolumeMix,
    app.objectEnergyVolumeGammaAccumulate,
    app.objectEnergyVolumeGammaMip,
    ratio.height, ratio.lower, ratio.width, ratio.rear, ratio.length,
  ];
  if (signaturesEqual(sig, lastBuildSig)) return;

  const now = Number.isFinite(nowMs) ? nowMs : performance.now();
  const refreshMs = Number(app.volumeRefreshMs) > 0 ? Number(app.volumeRefreshMs) : VOLUME_REBUILD_INTERVAL_MS;
  if (now - (app.lastSpeakerSoloVolumeAt || 0) < refreshMs) {
    return;
  }
  app.lastSpeakerSoloVolumeAt = now;
  lastBuildSig = sig;

  const { bands } = table;
  const nbands = bands.length;
  const cellIndex = makeCellIndexer(table);

  const common = {
    resolution: app.objectEnergyHeatmapResolution,
    opacity: app.objectEnergyHeatmapOpacity,
    mix: app.objectEnergyVolumeMix,
    gammaAccumulate: clampVolumeGamma('accumulate', app.objectEnergyVolumeGammaAccumulate),
    gammaMip: clampVolumeGamma('mip', app.objectEnergyVolumeGammaMip),
    customStops: app.speakerCustomGradientStops,
    smooth: app.volumeSmoothInterpolation,
  };

  if (app.heatmapAllBands && nbands > 1) {
    // Precompute each band's gradient colour from its log-frequency position.
    const scratch = new THREE.Color();
    const colormap = app.speakerHeatmapVolumeColormap;
    const bandRGB = bands.map((band) => {
      objectEnergyColor(colormap, bandLogT(band), scratch, app.speakerCustomGradientStops);
      return [scratch.r, scratch.g, scratch.b];
    });
    volume.update({
      ...common,
      sampleColor: (ow, od, oh, out) => {
        const base = cellIndex(ow, od, oh);
        let sumW = 0;
        let r = 0;
        let g = 0;
        let b = 0;
        for (let bi = 0; bi < nbands; bi += 1) {
          const gg = bands[bi].gains[base];
          const lvl = gg * gg;
          if (lvl > 0) {
            const c = bandRGB[bi];
            sumW += lvl;
            r += lvl * c[0];
            g += lvl * c[1];
            b += lvl * c[2];
          }
        }
        if (sumW > 0) {
          out[0] = r / sumW;
          out[1] = g / sumW;
          out[2] = b / sumW;
          out[3] = sumW;
        } else {
          out[0] = 0;
          out[1] = 0;
          out[2] = 0;
          out[3] = 0;
        }
      },
    });
    return;
  }

  // Single band.
  const bandIndex = Math.max(0, Math.min(nbands - 1, Math.round(Number(app.heatmapBandIndex) || 0)));
  const gains = bands[bandIndex].gains;
  volume.update({
    ...common,
    colormap: colormapIndex(app.speakerHeatmapVolumeColormap),
    sampleEnergy: (ow, od, oh) => {
      const g = gains[cellIndex(ow, od, oh)];
      return g * g;
    },
  });
}
