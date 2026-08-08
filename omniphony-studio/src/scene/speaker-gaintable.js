/**
 * Shared store + subscription for the renderer's per-band speaker gain table.
 *
 * A display shows ONE field, so we subscribe per field: the renderer keeps the
 * full table cached and ships just that field's per-band values (one value per
 * cell per crossover band) — a speaker's slice, or the all-speaker energy sum.
 * Several displays can be up at once, so the subscription is per *consumer*:
 * every field on screen is subscribed for, and stays refreshed. It arrives as a base64 binary
 * blob (`dataB64`) which we slice into Float32Array views — no per-number JSON
 * parsing. Each speaker's decoded table is kept in a per-speaker cache:
 *   { nx, ny, nz, speakerIndex, bands: [{ lowHz, highHz|null, gains: Float32Array }],
 *     xPositions, yPositions, zPositions }
 *   gain(cell, band) = bands[band].gains[xi + nx*(yi + ny*zi)]
 *
 * Because we cache per speaker (and remember each speaker's last version), going
 * back to an already-loaded speaker is instant: the render reads the cached table
 * immediately and the subscribe carries that speaker's version, so the renderer
 * replies `uptodate` instead of re-transferring. A fresh speaker (version 0) is
 * fetched once. Topology rebuilds bump the version, so a stale cache is refreshed.
 *
 * Pub/sub: displays call `acquireGainTable(id)`; each acquire subscribes for that
 * consumer's field (carrying the version we hold for it) and arms a 5 s
 * heartbeat, which doubles as the repair path — it re-declares the held version
 * per target, so a transfer lost in flight is refetched instead of leaving a
 * display stale. Speaker changes re-subscribe via `refreshGaintableSubscription`.
 * Releasing a consumer drops ITS cached field, so re-enabling refetches rather
 * than painting what was current when it was last shown.
 */

import { invoke } from '@tauri-apps/api/core';

import { app } from '../state.js';

// speaker index → decoded table; speaker index → last version we hold for it.
// The global energy field is cached under GLOBAL_ENERGY_INDEX like any speaker.
const tables = new Map();
const versions = new Map();

/**
 * Subscription target for the all-speaker energy field (`√Σ gᵢ²` per cell),
 * mirroring `renderer::band_gaintable::GLOBAL_ENERGY_INDEX`.
 */
export const GLOBAL_ENERGY_INDEX = -1;

/**
 * Subscription targets for the discontinuity fields, mirroring
 * `renderer::band_gaintable::{GAIN_DISCONTINUITY_INDEX, CENTROID_JUMP_INDEX}`:
 * per cell, the worst jump to a grid neighbour — of the energy-normalised gain
 * vector (configuration change), or of the gain²-weighted speaker centroid
 * (room distance the sound image moves).
 */
export const GAIN_DISCONTINUITY_INDEX = -2;
export const CENTROID_JUMP_INDEX = -3;

/** The engine target the discontinuity display needs for the current mode. */
function discontinuityTarget() {
  return app.discontinuityHeatmapMode === 'centroid'
    ? CENTROID_JUMP_INDEX
    : GAIN_DISCONTINUITY_INDEX;
}

/**
 * Active consumers, mapped to the field each one needs.
 *
 * A map, not a set: every displayed field must be subscribed for. Deriving a
 * single target from the app state instead meant enabling one heatmap silently
 * starved the other — it kept rendering its cached field, which the renderer
 * had stopped refreshing, so it went stale without any sign.
 */
const consumers = new Map();
let heartbeatTimer = null;
const HEARTBEAT_MS = 5000;

function currentSpeaker() {
  const s = app.selectedSpeakerIndex;
  return Number.isInteger(s) && s >= 0 ? s : 0;
}

/** Target resolvers per consumer id, evaluated at each subscribe. */
const TARGET_OF = {
  speakerSoloVolume: () => currentSpeaker(),
  globalEnergyVolume: () => GLOBAL_ENERGY_INDEX,
  discontinuityVolume: discontinuityTarget,
};

function targetsInUse() {
  const targets = new Set();
  for (const id of consumers.keys()) {
    const resolve = TARGET_OF[id];
    if (resolve) targets.add(resolve());
  }
  return targets;
}

/** Subscribe for every field currently displayed, each with the version we hold. */
function sendSubscribe() {
  for (const target of targetsInUse()) {
    invoke('subscribe_speaker_gaintable', {
      haveVersion: versions.get(target) | 0,
      speakerIndex: target,
    }).catch(() => {});
  }
}

function startHeartbeat() {
  if (heartbeatTimer !== null) return;
  heartbeatTimer = setInterval(() => {
    // The heartbeat is also the repair path: it re-declares the version we
    // actually hold for each target, so a transfer lost in flight is refetched
    // instead of leaving a display stale forever.
    if (consumers.size > 0) sendSubscribe();
  }, HEARTBEAT_MS);
}

function stopHeartbeat() {
  if (heartbeatTimer === null) return;
  clearInterval(heartbeatTimer);
  heartbeatTimer = null;
}

/** Register a consumer that needs the gain table, and subscribe for its field. */
export function acquireGainTable(id) {
  const wasEmpty = consumers.size === 0;
  consumers.set(id, true);
  sendSubscribe();
  if (wasEmpty) startHeartbeat();
}

/**
 * Drop a consumer. Its cached field is discarded: re-enabling the display must
 * refetch rather than paint whatever was current when it was last shown.
 */
export function releaseGainTable(id) {
  if (!consumers.delete(id)) return;
  const resolve = TARGET_OF[id];
  if (resolve && !targetsInUse().has(resolve())) {
    const target = resolve();
    tables.delete(target);
    versions.delete(target);
  }
  if (consumers.size === 0) {
    stopHeartbeat();
    invoke('unsubscribe_speaker_gaintable').catch(() => {});
  }
}

/** Re-subscribe immediately for every displayed field. */
export function refreshGaintableSubscription() {
  if (consumers.size > 0) sendSubscribe();
}

function base64ToArrayBuffer(b64) {
  const bin = atob(b64);
  const len = bin.length;
  const bytes = new Uint8Array(len);
  for (let i = 0; i < len; i += 1) bytes[i] = bin.charCodeAt(i);
  return bytes.buffer;
}

/** Store the per-speaker band table decoded from the binary payload. */
export function setSpeakerGainTable(payload) {
  if (!payload || payload.domain !== 'cartesian_bands' || typeof payload.dataB64 !== 'string') {
    return;
  }
  const nx = Number(payload.xCount) | 0;
  const ny = Number(payload.yCount) | 0;
  const nz = Number(payload.zCount) | 0;
  const nb = Number(payload.bandCount) | 0;
  // Signed: GLOBAL_ENERGY_INDEX keys the global field, not speaker 0.
  const speakerIndex = Math.trunc(Number(payload.speakerIndex)) || 0;
  const bandMeta = Array.isArray(payload.bands) ? payload.bands : [];
  if (nx < 1 || ny < 1 || nz < 1 || nb < 1) {
    return;
  }
  let buffer;
  try {
    buffer = base64ToArrayBuffer(payload.dataB64);
  } catch {
    return;
  }
  const cells = nx * ny * nz;
  const need = (nx + ny + nz + nb * cells) * 4;
  if (buffer.byteLength < need) {
    return;
  }
  let off = 0;
  const xPositions = new Float32Array(buffer, off, nx); off += nx * 4;
  const yPositions = new Float32Array(buffer, off, ny); off += ny * 4;
  const zPositions = new Float32Array(buffer, off, nz); off += nz * 4;
  const bands = [];
  for (let b = 0; b < nb; b += 1) {
    const gains = new Float32Array(buffer, off, cells); off += cells * 4;
    bands.push({
      lowHz: Number(bandMeta[b]?.lowHz) || 0,
      highHz: bandMeta[b]?.highHz == null ? null : Number(bandMeta[b].highHz),
      gains,
    });
  }
  tables.set(speakerIndex, { nx, ny, nz, speakerIndex, bands, xPositions, yPositions, zPositions });
  if (Number.isFinite(Number(payload.version))) {
    versions.set(speakerIndex, Number(payload.version) | 0);
  }
}

export function getSpeakerGainTable() {
  return tables.get(currentSpeaker()) ?? null;
}

/** The all-speaker energy field, or `null` until it has been received. */
export function getGlobalEnergyTable() {
  return tables.get(GLOBAL_ENERGY_INDEX) ?? null;
}

/**
 * The discontinuity field for the CURRENT mode, or `null` until received.
 * Both modes' tables stay cached, so flipping back to an already-fetched mode
 * paints immediately while the subscribe answers `uptodate`.
 */
export function getDiscontinuityTable() {
  return tables.get(discontinuityTarget()) ?? null;
}
