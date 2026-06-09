/**
 * Shared core for the object energy field overlays.
 *
 * The energy field is a purely client-side, object-driven scalar field: for each
 * live object we deposit a theoretical acoustic energy gradient (inverse-square
 * falloff from the object's current audio level) and accumulate every object's
 * contribution over a grid. Consumed by `object-energy-volume.js`, the full-3D
 * ray-marched volume renderer (Data3DTexture).
 *
 * No renderer/Tauri dependency: it reads `sourcePositionsRaw` + `sourceLevels`
 * directly. Updated on a throttled tick from the animation loop, like trails.
 */

import { sourcePositionsRaw, sourceLevels, objectMuted } from '../state.js';

// Below this RMS the object contributes nothing (mirror of trails.js).
export const SILENT_RMS_DBFS = -100;
// Minimum interval between full field rebuilds (mirror of TRAIL_MIN_REBUILD_INTERVAL_MS).
export const MIN_REBUILD_INTERVAL_MS = 70;

// The ray-marched energy volumes upload a full n³ RGBA-float 3D texture on every
// rebuild (≈4 MB at the default resolution 64). At 70 ms that is ~56 MB/s of GPU
// texture traffic, which balloons the WebView2 GPU process memory on Windows
// (ANGLE/D3D) — a confirmed major contributor to the Studio memory climb. The
// volume tracks slowly-changing object levels, so a lower refresh rate is
// visually fine and cuts the upload churn ~2.3×. Used only for the volume
// providers (object energy field, speaker solo volume), not the trails.
export const VOLUME_REBUILD_INTERVAL_MS = 160;

// Colour stops blue → cyan → green → yellow → red, interpolated in RGB.
// Adjacent stops share a channel at 1.0 so every transition stays vivid (no dark
// bands), and the stop spacing widens the warm (yellow) band, which a uniform
// hue sweep renders too narrow. Kept in sync with the mpv overlay's
// `heatmap_rgb` (orender_engine/overlay.rs).
export const HEATMAP_STOPS = [
  [0.00, 0.0, 0.0, 1.0], // blue
  [0.25, 0.0, 1.0, 1.0], // cyan
  [0.48, 0.0, 1.0, 0.0], // green
  [0.70, 1.0, 1.0, 0.0], // yellow
  [1.00, 1.0, 0.0, 0.0], // red
];

// Available colour gradients for the object energy field (shared by the planes
// and the volume renderer). Index order matches the GLSL `uColormap` uniform.
//   heatmap   — blue→cyan→green→yellow→red (value encoded in colour + alpha)
//   blueWhite — blue→white                 (value in colour + alpha)
//   whiteRed  — white→red                   (value in colour + alpha)
//   red       — constant red               (value carried by the alpha only)
//   custom    — user-editable stops (per consumer); same index across the JS, the
//               GLSL shader (uColormap == 4) and the mpv overlay.
export const OBJECT_ENERGY_COLORMAPS = ['heatmap', 'blueWhite', 'whiteRed', 'red', 'custom'];

// Max stops in the custom gradient. Matches the GLSL `uCustomStops[]` array size
// and the editor's add-stop cap; keep all three in sync.
export const MAX_CUSTOM_STOPS = 8;

export function colormapIndex(colormap) {
  const i = OBJECT_ENERGY_COLORMAPS.indexOf(colormap);
  return i >= 0 ? i : 0;
}

// Interpolate a custom gradient (`stops`, assumed sorted by pos) at t∈[0,1].
// Allocation-free linear scan over ≤8 stops (called per cell in the precoloured
// path). Falls back to greyscale if there are no stops.
function customStopsColor(stops, t, target) {
  const n = Array.isArray(stops) ? stops.length : 0;
  if (n === 0) return target.setRGB(t, t, t);
  if (t <= stops[0].pos) return target.setRGB(stops[0].r, stops[0].g, stops[0].b);
  const last = stops[n - 1];
  if (t >= last.pos) return target.setRGB(last.r, last.g, last.b);
  for (let i = 0; i + 1 < n; i += 1) {
    const a = stops[i];
    const b = stops[i + 1];
    if (t <= b.pos) {
      const f = b.pos > a.pos ? (t - a.pos) / (b.pos - a.pos) : 0;
      return target.setRGB(a.r + (b.r - a.r) * f, a.g + (b.g - a.g) * f, a.b + (b.b - a.b) * f);
    }
  }
  return target.setRGB(last.r, last.g, last.b);
}

function heatmapStopsColor(t, target) {
  let i = 0;
  while (i + 1 < HEATMAP_STOPS.length && t > HEATMAP_STOPS[i + 1][0]) {
    i += 1;
  }
  const [t0, r0, g0, b0] = HEATMAP_STOPS[i];
  const [t1, r1, g1, b1] = HEATMAP_STOPS[Math.min(i + 1, HEATMAP_STOPS.length - 1)];
  const f = t1 > t0 ? (t - t0) / (t1 - t0) : 0;
  return target.setRGB(r0 + (r1 - r0) * f, g0 + (g1 - g0) * f, b0 + (b1 - b0) * f);
}

// Map a normalised value [0,1] to an RGB colour for the chosen colormap. The
// alpha is handled by the caller (it always encodes the value); 'red' makes the
// colour constant so only the alpha conveys energy.
export function objectEnergyColor(colormap, value, target, customStops) {
  const t = Math.max(0, Math.min(1, Number(value) || 0));
  if (colormap === 'red') {
    return target.setRGB(1.0, 0.0, 0.0);
  }
  if (colormap === 'blueWhite') {
    return target.setRGB(t, t, 1.0);
  }
  if (colormap === 'whiteRed') {
    return target.setRGB(1.0, 1.0 - t, 1.0 - t);
  }
  if (colormap === 'custom') {
    return customStopsColor(customStops, t, target);
  }
  return heatmapStopsColor(t, target);
}

// GLSL counterpart of `objectEnergyColor`, embedded in the volume ray-march
// shader so the ramps match the planes mode exactly. Branches on the `uColormap`
// uniform (declared by the fragment shader, before this snippet is injected).
export const HEATMAP_GLSL = /* glsl */`
// Custom gradient stops as (pos, r, g, b); count in uCustomStopCount. Declared by
// the fragment shader (energy-volume-core.js) so they're shared with this snippet.
vec3 customStopsColor(float t) {
  int n = uCustomStopCount;
  if (n <= 0) { return vec3(t); }
  if (t <= uCustomStops[0].x) { return uCustomStops[0].yzw; }
  for (int i = 0; i + 1 < ${MAX_CUSTOM_STOPS}; i++) {
    if (i + 1 >= n) { break; }
    vec4 a = uCustomStops[i];
    vec4 b = uCustomStops[i + 1];
    if (t <= b.x) {
      float f = b.x > a.x ? (t - a.x) / (b.x - a.x) : 0.0;
      return mix(a.yzw, b.yzw, f);
    }
  }
  return uCustomStops[n - 1].yzw;
}

vec3 heatmapColor(float value) {
  float t = clamp(value, 0.0, 1.0);
  if (uColormap == 4) { return customStopsColor(t); }       // custom stops
  if (uColormap == 3) { return vec3(1.0, 0.0, 0.0); }       // red: alpha-only
  if (uColormap == 2) { return vec3(1.0, 1.0 - t, 1.0 - t); } // white → red
  if (uColormap == 1) { return vec3(t, t, 1.0); }            // blue → white
  vec3 c;                                                    // heatmap rainbow
  if (t < 0.25) {
    c = mix(vec3(0.0, 0.0, 1.0), vec3(0.0, 1.0, 1.0), (t - 0.00) / 0.25);
  } else if (t < 0.48) {
    c = mix(vec3(0.0, 1.0, 1.0), vec3(0.0, 1.0, 0.0), (t - 0.25) / 0.23);
  } else if (t < 0.70) {
    c = mix(vec3(0.0, 1.0, 0.0), vec3(1.0, 1.0, 0.0), (t - 0.48) / 0.22);
  } else {
    c = mix(vec3(1.0, 1.0, 0.0), vec3(1.0, 0.0, 0.0), (t - 0.70) / 0.30);
  }
  return c;
}
`;

// Alpha transfer-curve (γ) slider range, per volume projection. The two
// projections need different ranges: 'accumulate' integrates the whole ray so it
// wants a high γ to stay readable, while 'mip' weighs a single peak sample and
// needs to go below 1.0 to lift mid/low cells.
export const VOLUME_GAMMA_RANGE = {
  accumulate: { min: 1.0, max: 10.0, step: 0.1, default: 2.5 },
  mip: { min: 0.2, max: 3.0, step: 0.05, default: 0.8 },
};

export function clampVolumeGamma(projection, value) {
  const r = VOLUME_GAMMA_RANGE[projection === 'mip' ? 'mip' : 'accumulate'];
  const v = Number(value);
  return Math.max(r.min, Math.min(r.max, Number.isFinite(v) ? v : r.default));
}

// dBFS RMS → linear power, with the silence floor folded in.
export function objectEnergyLinear(rmsDbfs) {
  const db = Number(rmsDbfs);
  if (!Number.isFinite(db) || db <= SILENT_RMS_DBFS) {
    return 0;
  }
  return Math.pow(10, db / 10);
}

// Reusable scratch list of active objects, refilled each tick (no per-tick alloc
// once the array has grown to its working size). Shared across both renderers —
// only one runs per frame, so there is no contention.
export const activeObjects = [];

// Refill `activeObjects` with { x, y, z, energy } for every audible, non-muted
// object. Returns the count of usable entries. Coordinates stay in Omniphony
// normalised space [-1, 1] (raw object positions), matching the energy math.
export function collectActiveObjects() {
  let count = 0;
  sourcePositionsRaw.forEach((raw, id) => {
    if (objectMuted.has(id)) {
      return;
    }
    const level = sourceLevels.get(id);
    const energy = objectEnergyLinear(level?.rmsDbfs);
    if (energy <= 0) {
      return;
    }
    const x = Number(raw?.x);
    const y = Number(raw?.y);
    const z = Number(raw?.z);
    if (!Number.isFinite(x) || !Number.isFinite(y) || !Number.isFinite(z)) {
      return;
    }
    let entry = activeObjects[count];
    if (!entry) {
      entry = { x: 0, y: 0, z: 0, energy: 0 };
      activeObjects[count] = entry;
    }
    entry.x = x;
    entry.y = y;
    entry.z = z;
    entry.energy = energy;
    count += 1;
  });
  return count;
}
