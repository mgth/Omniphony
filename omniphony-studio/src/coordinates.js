/**
 * Coordinate conversion utilities.
 *
 * Pure functions (no side-effects, no shared state) except for the room-ratio
 * aware helpers which read from the shared `roomRatio` state.
 */

import { app } from './state.js';

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

export function formatNumber(value, digits = 2) {
  if (typeof value !== 'number' || Number.isNaN(value)) {
    return '—';
  }
  return value.toFixed(digits);
}

// ---------------------------------------------------------------------------
// Scene ↔ Omniphony axis swizzle
// ---------------------------------------------------------------------------

export function sceneToOmniphonyCartesian(position) {
  return {
    x: Number(position?.z) || 0,
    y: Number(position?.x) || 0,
    z: Number(position?.y) || 0
  };
}

export function omniphonyToSceneCartesian(position) {
  return {
    x: Number(position?.y) || 0,
    y: Number(position?.z) || 0,
    z: Number(position?.x) || 0
  };
}

// ---------------------------------------------------------------------------
// Cartesian ↔ Spherical
// ---------------------------------------------------------------------------

export function cartesianToSpherical(position) {
  const x = Number(position.x) || 0;
  const y = Number(position.y) || 0;
  const z = Number(position.z) || 0;
  const dist = Math.sqrt(x * x + y * y + z * z);
  const az = (Math.atan2(z, x) * 180) / Math.PI;
  const el = dist > 0 ? (Math.atan2(y, Math.sqrt(x * x + z * z)) * 180) / Math.PI : 0;
  return { az, el, dist };
}

export function sphericalToCartesianDeg(az, el, dist) {
  const azRad = (az * Math.PI) / 180;
  const elRad = (el * Math.PI) / 180;
  const x = dist * Math.cos(elRad) * Math.cos(azRad);
  const y = dist * Math.sin(elRad);
  const z = dist * Math.cos(elRad) * Math.sin(azRad);
  return { x, y, z };
}

// ---------------------------------------------------------------------------
// Angle helpers
// ---------------------------------------------------------------------------

export function normalizeAngleDeg(angle) {
  let a = angle;
  while (a > 180) a -= 360;
  while (a < -180) a += 360;
  return a;
}

export function snapAngleDeg(angle, step, threshold) {
  const snapped = Math.round(angle / step) * step;
  return Math.abs(angle - snapped) <= threshold ? snapped : angle;
}

// ---------------------------------------------------------------------------
// General math
// ---------------------------------------------------------------------------

export function clampNumber(value, min, max) {
  return Math.max(min, Math.min(max, value));
}

// ---------------------------------------------------------------------------
// Room-ratio depth warping
// ---------------------------------------------------------------------------

export function depthWarpWithRatios(rawDepth, frontRatio, rearRatio, centerBlend = 0.5) {
  const d = Math.max(-1, Math.min(1, Number(rawDepth) || 0));
  const f = Number(frontRatio) || 1;
  const r = Number(rearRatio) || 1;
  const blend = Math.max(0, Math.min(1, Number(centerBlend)));
  const center = r + (f - r) * blend;
  if (d >= 0) {
    const t = d;
    const a = center - f;
    const b = 2 * (f - center);
    return a * t * t * t + b * t * t + center * t;
  }
  const t = -d;
  const a = center - r;
  const b = 2 * (r - center);
  return -(a * t * t * t + b * t * t + center * t);
}

export function mapRoomDepth(rawX) {
  return depthWarpWithRatios(rawX, app.roomRatio.length, app.roomRatio.rear, app.roomRatio.centerBlend);
}

export function mapRoomPosition(rawPosition) {
  const rawY = Number(rawPosition?.y) || 0;
  return {
    x: mapRoomDepth(Number(rawPosition?.x) || 0),
    y: rawY >= 0 ? rawY * app.roomRatio.height : rawY * app.roomRatio.lower,
    z: (Number(rawPosition?.z) || 0) * app.roomRatio.width
  };
}

export function inverseMapRoomDepth(mappedDepth) {
  const front = Math.max(0.001, Number(app.roomRatio.length) || 1);
  const rear = Math.max(0.001, Number(app.roomRatio.rear) || 1);
  const blend = Math.max(0, Math.min(1, Number(app.roomRatio.centerBlend) || 0.5));
  if (mappedDepth >= 0) {
    const target = clampNumber(mappedDepth, 0, front);
    let lo = 0;
    let hi = 1;
    for (let i = 0; i < 28; i += 1) {
      const mid = (lo + hi) * 0.5;
      const val = depthWarpWithRatios(mid, front, rear, blend);
      if (val < target) lo = mid;
      else hi = mid;
    }
    return (lo + hi) * 0.5;
  }

  const target = clampNumber(mappedDepth, -rear, 0);
  let lo = -1;
  let hi = 0;
  for (let i = 0; i < 28; i += 1) {
    const mid = (lo + hi) * 0.5;
    const val = depthWarpWithRatios(mid, front, rear, blend);
    if (val < target) lo = mid;
    else hi = mid;
  }
  return (lo + hi) * 0.5;
}

// ---------------------------------------------------------------------------
// Combined transforms (Omniphony normalised ↔ scene with room ratio)
// ---------------------------------------------------------------------------

export function normalizedOmniphonyToScenePosition(position) {
  const rawScene = omniphonyToSceneCartesian(position);
  return mapRoomPosition(rawScene);
}

export function scenePositionToNormalizedOmniphony(position) {
  const rawScene = {
    x: inverseMapRoomDepth(Number(position?.x) || 0),
    y: (Number(position?.y) || 0) >= 0
      ? (Number(position?.y) || 0) / Math.max(0.001, Number(app.roomRatio.height) || 1)
      : (Number(position?.y) || 0) / Math.max(0.001, Number(app.roomRatio.lower) || 0.5),
    z: (Number(position?.z) || 0) / Math.max(0.001, Number(app.roomRatio.width) || 1)
  };
  const omni = sceneToOmniphonyCartesian(rawScene);
  return {
    x: clampNumber(omni.x, -1, 1),
    y: clampNumber(omni.y, -1, 1),
    z: clampNumber(omni.z, -1, 1)
  };
}

// ---------------------------------------------------------------------------
// Coord-mode detection & hydration
// ---------------------------------------------------------------------------

export function getSpeakerCoordMode(speaker) {
  return String(speaker?.coordMode || 'polar').toLowerCase() === 'cartesian' ? 'cartesian' : 'polar';
}

export function getObjectCoordMode(position) {
  const raw = String(position?.coordMode || '').toLowerCase();
  if (raw === 'cartesian' || raw === 'polar') {
    return raw;
  }
  if (
    Number.isFinite(Number(position?.azimuthDeg))
    || Number.isFinite(Number(position?.elevationDeg))
    || Number.isFinite(Number(position?.distanceM))
  ) {
    return 'polar';
  }
  return 'cartesian';
}

export function hydrateObjectCoordinateState(position) {
  if (!position) return null;

  const mode = getObjectCoordMode(position);
  if (mode === 'cartesian') {
    const x = clampNumber(Number(position.x) || 0, -1, 1);
    const y = clampNumber(Number(position.y) || 0, -1, 1);
    const z = clampNumber(Number(position.z) || 0, -1, 1);
    const scene = normalizedOmniphonyToScenePosition({ x, y, z });
    const sph = cartesianToSpherical(scene);
    position.x = x;
    position.y = y;
    position.z = z;
    position.azimuthDeg = sph.az;
    position.elevationDeg = sph.el;
    position.distanceM = Math.max(0.01, sph.dist);
  } else {
    const az = Number.isFinite(Number(position.azimuthDeg)) ? Number(position.azimuthDeg) : 0;
    const el = Number.isFinite(Number(position.elevationDeg)) ? Number(position.elevationDeg) : 0;
    const dist = Math.max(0.01, Number(position.distanceM) || 1);
    const scene = sphericalToCartesianDeg(az, el, dist);
    const omni = scenePositionToNormalizedOmniphony(scene);
    position.azimuthDeg = az;
    position.elevationDeg = el;
    position.distanceM = dist;
    position.x = omni.x;
    position.y = omni.y;
    position.z = omni.z;
  }
  position.coordMode = mode;
  return position;
}

export function hydrateSpeakerCoordinateState(speaker) {
  if (!speaker) return null;

  const mode = getSpeakerCoordMode(speaker);
  if (mode === 'cartesian') {
    const x = clampNumber(Number(speaker.x) || 0, -1, 1);
    const y = clampNumber(Number(speaker.y) || 0, -1, 1);
    const z = clampNumber(Number(speaker.z) || 0, -1, 1);
    const scene = normalizedOmniphonyToScenePosition({ x, y, z });
    const sph = cartesianToSpherical(scene);
    speaker.x = x;
    speaker.y = y;
    speaker.z = z;
    speaker.azimuthDeg = sph.az;
    speaker.elevationDeg = sph.el;
    speaker.distanceM = Math.max(0.01, sph.dist);
  } else {
    const az = Number.isFinite(Number(speaker.azimuthDeg)) ? Number(speaker.azimuthDeg) : 0;
    const el = Number.isFinite(Number(speaker.elevationDeg)) ? Number(speaker.elevationDeg) : 0;
    const dist = Math.max(0.01, Number(speaker.distanceM) || 1);
    const scene = sphericalToCartesianDeg(az, el, dist);
    const omni = scenePositionToNormalizedOmniphony(scene);
    speaker.azimuthDeg = az;
    speaker.elevationDeg = el;
    speaker.distanceM = dist;
    speaker.x = omni.x;
    speaker.y = omni.y;
    speaker.z = omni.z;
  }
  speaker.coordMode = mode;
  return speaker;
}

// ---------------------------------------------------------------------------
// Position formatting
// ---------------------------------------------------------------------------

export function formatPosition(position) {
  if (!position) {
    return 'x:— y:— z:—';
  }
  const x = Number(position.x);
  const y = Number(position.y);
  const z = Number(position.z);
  if (!Number.isFinite(x) || !Number.isFinite(y) || !Number.isFinite(z)) {
    return 'x:— y:— z:—';
  }

  if (typeof position.azimuthDeg === 'number') {
    const az = position.azimuthDeg;
    const el = position.elevationDeg;
    const r = position.distanceM;
    return `x:${formatNumber(x, 1)} y:${formatNumber(y, 1)} z:${formatNumber(z, 1)} | az:${formatNumber(az, 1)} el:${formatNumber(el, 1)} r:${formatNumber(r, 2)}`;
  }

  const az = (Math.atan2(x, y) * 180) / Math.PI;
  const planar = Math.sqrt((x * x) + (y * y));
  const el = (Math.atan2(z, planar) * 180) / Math.PI;
  const dist = Math.sqrt(x * x + y * y + z * z);

  return `x:${formatNumber(x, 1)} y:${formatNumber(y, 1)} z:${formatNumber(z, 1)} | az:${formatNumber(az, 1)} el:${formatNumber(el, 1)} r:${formatNumber(dist, 2)}`;
}

export function getSpeakerSpatializeValue(speaker) {
  return Number(speaker?.spatialize) === 0 ? 0 : 1;
}

export function getSpeakerBaseOpacity(speaker) {
  return getSpeakerSpatializeValue(speaker) === 0 ? 0.3 : 0.65;
}
