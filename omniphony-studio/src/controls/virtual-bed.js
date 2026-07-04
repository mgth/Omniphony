/**
 * Virtual-bed editor: per-channel placement for 2D (channel-based) sources.
 *
 * Each input channel is either routed direct to its speaker (spatialize:false,
 * e.g. LFE → sub) or virtualized as an object at a position (spatialize:true).
 * The bed is a `SpeakerLayout` (one entry per channel label) pushed live to the
 * renderer via `control_virtual_bed` (a YAML/JSON layout string; an empty string
 * resets to the built-in canonical poses).
 *
 * Editing reuses the speaker-editing mechanic: the channels appear in the Objects
 * list and selecting one opens a parameter panel below it (cartesian / polar,
 * normalized / real, + Direct/Virtual + gain). When no live stream is playing the
 * channels are synthesized as objects (`syncVirtualBedObjects`) so the bed stays
 * visible and editable at rest; live stream objects take over while playing.
 */

import { invoke } from '@tauri-apps/api/core';
import { app, sourceMeshes, sourceNames, getLiveOption } from '../state.js';
import { t } from '../i18n.js';
import { updateSource, removeSource } from '../sources.js';
import {
  sphericalToCartesianDeg,
  cartesianToSpherical,
  scenePositionToNormalizedOmniphony,
  normalizedOmniphonyToScenePosition,
  normalizedToMeters,
  formatNumber
} from '../coordinates.js';

// Canonical editable channel set (7.1) with the default poses as ADM cartesian
// corners (X left/right, Y rear/front, Z down/up; ear level Z = 0). LFE defaults
// to direct (it cannot be VBAP-panned).
const CANONICAL_BED = [
  { name: 'L', x: -1, y: 1, z: 0, spatialize: true },
  { name: 'R', x: 1, y: 1, z: 0, spatialize: true },
  { name: 'C', x: 0, y: 1, z: 0, spatialize: true },
  { name: 'LFE', x: 0, y: 1, z: 0, spatialize: false },
  { name: 'Ls', x: -1, y: 0, z: 0, spatialize: true },
  { name: 'Rs', x: 1, y: 0, z: 0, spatialize: true },
  { name: 'Lb', x: -1, y: -1, z: 0, spatialize: true },
  { name: 'Rb', x: 1, y: -1, z: 0, spatialize: true }
];

// Name aliases per canonical channel (mirrors the renderer's label_aliases) so
// the editor matches a bed/object entry however it is named (L/FL, Ls/SL, …).
const CHANNEL_ALIASES = {
  L: ['l', 'fl', 'frontleft', 'leftfront'],
  R: ['r', 'fr', 'frontright', 'rightfront'],
  C: ['c', 'fc', 'center', 'centre'],
  LFE: ['lfe', 'lfe1', 'sub', 'subwoofer', 'sw'],
  Ls: ['ls', 'sl', 'leftsurround', 'surroundleft'],
  Rs: ['rs', 'sr', 'rightsurround', 'surroundright'],
  Lb: ['lb', 'bl', 'lrs', 'backleft', 'leftback', 'rearleft', 'leftrear'],
  Rb: ['rb', 'br', 'rrs', 'backright', 'rightback', 'rightrear', 'rearright']
};

// Canonical channel key (L/R/C/LFE/Ls/Rs/Lb/Rb) for any alias, or null.
export function canonicalChannelName(name) {
  if (typeof name !== 'string') return null;
  const lower = name.trim().toLowerCase();
  for (const [key, aliases] of Object.entries(CHANNEL_ALIASES)) {
    if (aliases.includes(lower)) return key;
  }
  return null;
}

// Canonical 5.1/7.1 channel order (L, R, C, LFE, Ls, Rs, Lb, Rb).
const CANONICAL_CHANNEL_ORDER = CANONICAL_BED.map((c) => c.name);

// Rank of a channel (by any alias) in the canonical order, or -1 if it is not a
// bed channel. Used to order the objects list for bed sources by the classic
// 5.1/7.1 channel order instead of alphabetically.
export function canonicalChannelOrder(name) {
  const key = canonicalChannelName(name);
  return key ? CANONICAL_CHANNEL_ORDER.indexOf(key) : -1;
}

// ---------------------------------------------------------------------------
// Model: the editable channel set
// ---------------------------------------------------------------------------

// Polar (az/el/dist) → ADM normalized cartesian, exactly like the speaker editor:
// the room-warp is inverted and the result clamped to [-1, 1]. "Norm" is this
// ADM position, not a raw axis swizzle.
function polarToAdm(azimuth, elevation, distance) {
  return scenePositionToNormalizedOmniphony(sphericalToCartesianDeg(azimuth, elevation, distance));
}

// ADM normalized cartesian → polar, via the same scene round-trip the speaker
// editor uses (re-applies the room-warp, then derives spherical).
function admToPolar(x, y, z) {
  const sph = cartesianToSpherical(normalizedOmniphonyToScenePosition({ x, y, z }));
  return { azimuth: sph.az, elevation: sph.el, distance: Math.max(0.01, sph.dist) };
}

// Default model entry for a channel: the canonical ADM cartesian corner, with
// the polar form derived so the editor/renderer can use either.
function defaultEntry(base) {
  const polar = admToPolar(base.x, base.y, base.z);
  return {
    name: base.name,
    coordMode: 'cartesian',
    x: base.x,
    y: base.y,
    z: base.z,
    ...polar,
    spatialize: base.spatialize,
    gainDb: 0
  };
}

// Read a configured bed entry (polar or cartesian) as a normalized model entry,
// falling back to the canonical default when it can't be parsed.
function readEntry(base, match) {
  if (!match) return defaultEntry(base);
  const cartesian = String(match.coord_mode || '').toLowerCase() === 'cartesian';
  const gainDb = Number.isFinite(Number(match.gain_db)) ? Math.round(Number(match.gain_db)) : 0;
  const spatialize = match.spatialize !== false;
  if (cartesian && Number.isFinite(Number(match.x))) {
    const x = Number(match.x) || 0;
    const y = Number(match.y) || 0;
    const z = Number(match.z) || 0;
    const polar = admToPolar(x, y, z);
    return { name: base.name, coordMode: 'cartesian', x, y, z, ...polar, spatialize, gainDb };
  }
  if (Number.isFinite(Number(match.azimuth))) {
    const azimuth = Number(match.azimuth);
    const elevation = Number(match.elevation) || 0;
    const distance = Number(match.distance) > 0 ? Number(match.distance) : 1.0;
    const norm = polarToAdm(azimuth, elevation, distance);
    return {
      name: base.name,
      coordMode: 'polar',
      azimuth,
      elevation,
      distance,
      x: norm.x,
      y: norm.y,
      z: norm.z,
      spatialize,
      gainDb
    };
  }
  return defaultEntry(base);
}

// The full editable channel set: canonical defaults overridden by any matching
// entry from the live virtual bed (matched by alias, case-insensitive).
export function effectiveChannels() {
  const configured = Array.isArray(app.virtualBed?.speakers) ? app.virtualBed.speakers : [];
  return CANONICAL_BED.map((base) => {
    const match = configured.find((s) => canonicalChannelName(s?.name) === base.name);
    return readEntry(base, match);
  });
}

function channelByName(name) {
  const key = canonicalChannelName(name);
  if (!key) return null;
  return effectiveChannels().find((c) => c.name === key) || null;
}

function buildLayoutPayload(channels) {
  const radius = Number(app.virtualBed?.radius_m) > 0 ? Number(app.virtualBed.radius_m) : 1.0;
  // Ship the block matching each channel's coord_mode, exactly like the speaker
  // editor. A cartesian channel sends its ADM-normalized x/y/z and the RENDERER
  // derives the pose (the same code path the output speakers use); a polar
  // channel sends azimuth/elevation/distance. Forcing polar here used to replace
  // the user's cartesian edit with a Studio-side conversion that didn't match the
  // renderer, so the channel landed at the polar-derived spot instead.
  const clamp = (v) => Math.max(-1, Math.min(1, Number(v) || 0));
  return {
    radius_m: radius,
    speakers: channels.map((c) => {
      const cartesian = c.coordMode === 'cartesian';
      const entry = {
        name: c.name,
        coord_mode: cartesian ? 'cartesian' : 'polar',
        spatialize: Boolean(c.spatialize)
      };
      if (cartesian) {
        entry.x = clamp(c.x);
        entry.y = clamp(c.y);
        entry.z = clamp(c.z);
      } else {
        entry.azimuth = Number(c.azimuth) || 0;
        entry.elevation = Number(c.elevation) || 0;
        entry.distance = Number(c.distance) > 0 ? Number(c.distance) : 0.01;
      }
      if (Math.round(c.gainDb || 0) !== 0) entry.gain_db = Math.round(c.gainDb);
      return entry;
    })
  };
}

// Update one channel entry (by canonical name) via `mutate`, push the whole bed
// to the renderer, and refresh the synthetic objects + panel.
function commitChannel(name, mutate) {
  const key = canonicalChannelName(name);
  if (!key) return;
  const channels = effectiveChannels();
  const target = channels.find((c) => c.name === key);
  if (!target) return;
  mutate(target);
  app.virtualBed = buildLayoutPayload(channels);
  invoke('control_virtual_bed', { value: JSON.stringify(app.virtualBed) });
  syncVirtualBedObjects(true);
  renderChannelEditor(true);
}

// Current placement of a channel as polar (az/el/dist) + pure normalized
// cartesian (x/y/z), so editor inputs can pull untouched axes from canonical
// state instead of re-reading rounded DOM values (mirrors the speaker editor).
export function getChannelPosition(name) {
  const ch = channelByName(name);
  if (!ch) return null;
  // Read the stored axes untouched (both representations are kept in sync on
  // every edit), so changing one cartesian axis doesn't drift the others through
  // a polar round-trip — mirrors how the speaker editor pulls untouched axes.
  const norm = { x: ch.x, y: ch.y, z: ch.z };
  const meters = normalizedToMeters(norm);
  return {
    azimuth: ch.azimuth,
    elevation: ch.elevation,
    distance: ch.distance,
    x: norm.x,
    y: norm.y,
    z: norm.z,
    mx: meters.x,
    my: meters.y,
    mz: meters.z,
    gainDb: ch.gainDb || 0
  };
}

// Placement of a channel by name: 'virtual' (draggable object), 'direct'
// (anchored to its speaker), or null (not a bed channel).
export function channelPlacement(name) {
  const ch = channelByName(name);
  if (!ch) return null;
  return ch.spatialize ? 'virtual' : 'direct';
}

// ---------------------------------------------------------------------------
// Commit helpers (used by the panel inputs and the 3D drag)
// ---------------------------------------------------------------------------

// Commit a polar placement: coord_mode = polar, store az/el/dist and derive the
// normalized cartesian for display. Both representations are kept in sync (like
// the speaker editor); the wire payload then ships the polar block.
export function applyChannelPolar(name, azimuth, elevation, distance) {
  const az = Number(azimuth) || 0;
  const el = Number(elevation) || 0;
  const dist = Number(distance) > 0 ? Number(distance) : 0.01;
  const norm = polarToAdm(az, el, dist);
  commitChannel(name, (c) => {
    c.coordMode = 'polar';
    c.azimuth = az;
    c.elevation = el;
    c.distance = dist;
    c.x = norm.x;
    c.y = norm.y;
    c.z = norm.z;
  });
}

// Commit a cartesian placement from ADM normalized [-1, 1] (the "Norm" fields):
// coord_mode = cartesian, store x/y/z and derive the polar form for display. The
// wire payload ships the cartesian block and the renderer derives the pose — the
// same path the output speakers use — so the channel lands exactly where set.
export function applyChannelCartesian(name, x, y, z) {
  const nx = Math.max(-1, Math.min(1, Number(x) || 0));
  const ny = Math.max(-1, Math.min(1, Number(y) || 0));
  const nz = Math.max(-1, Math.min(1, Number(z) || 0));
  const polar = admToPolar(nx, ny, nz);
  commitChannel(name, (c) => {
    c.coordMode = 'cartesian';
    c.x = nx;
    c.y = ny;
    c.z = nz;
    c.azimuth = polar.azimuth;
    c.elevation = polar.elevation;
    c.distance = polar.distance;
  });
}

// Commit from a scene-space cartesian position (the 3D gizmo drag): keep the
// channel's active coord_mode, mirroring applySpeakerSceneCartesianEdit (which
// stores both forms and sends the block matching the mode).
export function applyChannelSceneCartesian(name, sx, sy, sz) {
  const ch = channelByName(name);
  if (ch && ch.coordMode === 'cartesian') {
    const norm = scenePositionToNormalizedOmniphony({ x: sx, y: sy, z: sz });
    applyChannelCartesian(name, norm.x, norm.y, norm.z);
  } else {
    const sph = cartesianToSpherical({ x: sx, y: sy, z: sz });
    applyChannelPolar(name, sph.az, sph.el, Math.max(0.01, sph.dist));
  }
}

export function applyChannelGain(name, gainDb) {
  commitChannel(name, (c) => {
    c.gainDb = Math.round(Number(gainDb) || 0);
  });
}

export function applyChannelPlacement(name, spatialize) {
  commitChannel(name, (c) => {
    c.spatialize = Boolean(spatialize);
  });
}

// Reset the bed to the canonical defaults: every channel at its cube-corner
// position in CARTESIAN mode (the CANONICAL_BED corners), not the renderer's
// polar fallback angles. Sending an empty string used to hand the renderer its
// built-in polar poses, which the editor then displayed as cartesian corners —
// so the polar form changed while the cartesian fields stayed stale even though
// the coord-mode read "cartesian". Pushing the explicit cartesian bed keeps the
// editor, the 3D view and the audio render in agreement.
export function resetVirtualBed() {
  const channels = CANONICAL_BED.map((base) => defaultEntry(base));
  app.virtualBed = buildLayoutPayload(channels);
  invoke('control_virtual_bed', { value: JSON.stringify(app.virtualBed) });
  syncVirtualBedObjects(true);
  renderChannelEditor(true);
}

// Materialise the canonical bed into the renderer/config the first time we learn
// the renderer has no saved bed, so the editor's values live in config.yaml (like
// the speaker layout's `current_layout`) and are used in priority — rather than
// the renderer falling back to a built-in default. Same explicit cartesian push
// as the manual Reset; the one-shot guard lives in the caller.
export function materializeDefaultVirtualBed() {
  resetVirtualBed();
}

// ---------------------------------------------------------------------------
// Synthetic virtual objects (visible/editable at rest)
// ---------------------------------------------------------------------------

// Source ids we created from the bed; used to tell synthetic from live objects.
const syntheticIds = new Set();
let lastSyntheticSignature = null;

// How long after the last spatial:frame the session still counts as "streaming".
// Covers the brief post-seek gap where live objects vanish but frames still flow,
// so synthetic objects don't momentarily double the live ones.
const STREAM_IDLE_MS = 800;

function streamActive() {
  return performance.now() - (app.lastSpatialFrameAt || 0) < STREAM_IDLE_MS;
}

// Drop every live (non-synthetic) object mesh. Live objects come from the OSC
// stream and get NO remove event when the engine simply stops emitting them —
// e.g. switching to host mode, where ad_orender stops feeding the engine. Left
// in place they linger as stale meshes and double the next stream's objects (or
// the synthetic bed) on resume. They are dropped whenever the stream is not
// active; the engine re-sends them when spatial rendering resumes.
function removeLiveObjects() {
  for (const id of [...sourceMeshes.keys()]) {
    if (!syntheticIds.has(String(id))) {
      removeSource(id);
    }
  }
}

function syntheticPosition(ch) {
  const directSpeakerIndex = ch.spatialize ? undefined : directSpeakerFor(ch.name);
  // Position per the channel's coord_mode so the at-rest marker uses the same
  // representation that is sent to the renderer (cartesian via x/y/z, polar via
  // az/el/dist). Both blocks are passed; updateSource picks per coordMode.
  return {
    coordMode: ch.coordMode === 'cartesian' ? 'cartesian' : 'polar',
    x: Number(ch.x) || 0,
    y: Number(ch.y) || 0,
    z: Number(ch.z) || 0,
    azimuthDeg: ch.azimuth,
    elevationDeg: ch.elevation,
    distanceM: Number(ch.distance) > 0 ? Number(ch.distance) : 1.0,
    name: ch.name,
    gainDb: ch.gainDb,
    directSpeakerIndex,
    _noTrail: true
  };
}

// Best-effort output-speaker index for a direct channel, so the synthetic object
// snaps onto its speaker mesh (mirrors the renderer's direct_speaker_index).
function directSpeakerFor(name) {
  const key = canonicalChannelName(name);
  if (!key) return undefined;
  const speakers = Array.isArray(app.currentLayoutSpeakers) ? app.currentLayoutSpeakers : [];
  const idx = speakers.findIndex((s) => canonicalChannelName(s?.id ?? s?.name) === key);
  return idx >= 0 ? idx : undefined;
}

function removeSyntheticObjects() {
  if (syntheticIds.size === 0) return;
  for (const id of [...syntheticIds]) {
    removeSource(id);
    syntheticIds.delete(id);
  }
  lastSyntheticSignature = null;
}

/**
 * Materialize one object per channel from the bed when spatial mode is on and no
 * live stream is present; remove them otherwise. Signature-guarded to avoid
 * per-flush churn unless `force` is set.
 */
export function syncVirtualBedObjects(force = false) {
  const spatial = getLiveOption('channel_render_mode') !== 'host';
  // The live OSC stream owns the scene only while it is actively streaming
  // spatial frames (recent spatial:frame, which also covers the post-seek gap).
  // Host mode is never streaming. While streaming, the live objects are the
  // truth — drop the synthetic bed and keep them.
  const streaming = spatial && streamActive();
  if (streaming) {
    removeSyntheticObjects();
    return;
  }
  // Not streaming (host mode, or spatial at rest): any live objects are stale
  // leftovers with no remove event, so drop them — this is what stops them
  // doubling on the next resume. In host mode the scene is then empty; in
  // spatial-at-rest we show the synthetic bed below.
  removeLiveObjects();
  if (!spatial) {
    removeSyntheticObjects();
    return;
  }

  const channels = effectiveChannels();
  const signature = JSON.stringify(channels);
  if (!force && signature === lastSyntheticSignature && syntheticIds.size === channels.length) {
    return;
  }
  lastSyntheticSignature = signature;

  const wanted = new Set(channels.map((c) => c.name));
  for (const id of [...syntheticIds]) {
    if (!wanted.has(id)) {
      removeSource(id);
      syntheticIds.delete(id);
    }
  }
  for (const ch of channels) {
    syntheticIds.add(ch.name);
    updateSource(ch.name, syntheticPosition(ch));
  }
}

// ---------------------------------------------------------------------------
// Channel editor panel (mirrors the speaker editor)
// ---------------------------------------------------------------------------

function el(id) { return document.getElementById(id); }

function selectedChannelName() {
  if (app.selectedSourceId === null || app.selectedSourceId === undefined) return null;
  const name = sourceNames.get(String(app.selectedSourceId));
  return canonicalChannelName(name);
}

let lastEditorKey = null;

/**
 * (Re)build the channel editor from the selected object's channel. Hidden in
 * `host` mode or when the selected object is not a bed channel. Skips the rebuild
 * while a field is focused (so typing isn't clobbered) unless `force` is set.
 */
export function renderChannelEditor(force = false) {
  const section = el('channelEditSection');
  if (!section) return;

  // During an active gizmo drag the numeric fields are driven live by
  // previewChannelEditorFromScene; don't let a background flush rebuild them from
  // the (not-yet-committed) bed and clobber the preview.
  if (!force && app.isDraggingVirtualBed) return;

  const key = getLiveOption('channel_render_mode') === 'host' ? null : selectedChannelName();
  if (!key) {
    section.style.display = 'none';
    lastEditorKey = null;
    return;
  }
  if (!force && key === lastEditorKey && section.contains(document.activeElement)) return;
  lastEditorKey = key;

  const ch = channelByName(key);
  if (!ch) {
    section.style.display = 'none';
    return;
  }
  section.style.display = '';

  const titleEl = el('channelEditTitle');
  if (titleEl) titleEl.textContent = `${t('channelEdit.title')} — ${ch.name}`;

  const spatialize = ch.spatialize !== false;
  const toggle = el('channelEditSpatializeToggle');
  if (toggle) toggle.checked = spatialize;
  const toggleText = el('channelEditSpatializeText');
  if (toggleText) toggleText.textContent = spatialize ? t('virtualBed.virtual') : t('virtualBed.direct');

  const mode = app.channelEditCoordMode === 'cartesian' ? 'cartesian' : 'polar';
  const cartMode = el('channelEditCartesianMode');
  const polarMode = el('channelEditPolarMode');
  if (cartMode) cartMode.checked = mode === 'cartesian';
  if (polarMode) polarMode.checked = mode === 'polar';

  // ADM normalized cartesian + real-world metres, computed exactly like the
  // speaker editor (room-warp inverted, clamped).
  const norm = polarToAdm(ch.azimuth, ch.elevation, ch.distance);
  const meters = normalizedToMeters(norm);
  const rMeters = Math.hypot(meters.x, meters.y, meters.z);
  setValueUnlessEditing('channelEditXInput', formatNumber(norm.x, 3));
  setValueUnlessEditing('channelEditYInput', formatNumber(norm.y, 3));
  setValueUnlessEditing('channelEditZInput', formatNumber(norm.z, 3));
  setValueUnlessEditing('channelEditXMetersInput', formatNumber(meters.x, 2));
  setValueUnlessEditing('channelEditYMetersInput', formatNumber(meters.y, 2));
  setValueUnlessEditing('channelEditZMetersInput', formatNumber(meters.z, 2));
  setValueUnlessEditing('channelEditAzInput', formatNumber(ch.azimuth, 1));
  setValueUnlessEditing('channelEditElInput', formatNumber(ch.elevation, 1));
  setValueUnlessEditing('channelEditRInput', formatNumber(ch.distance, 3));
  setValueUnlessEditing('channelEditRMetersInput', formatNumber(rMeters, 2));

  const gainSlider = el('channelEditGainSlider');
  if (gainSlider) gainSlider.value = String(ch.gainDb || 0);
  const gainBox = el('channelEditGainBox');
  if (gainBox) gainBox.textContent = `${ch.gainDb > 0 ? '+' : ''}${ch.gainDb || 0} dB`;

  // Direct channels are pinned to their speaker: only Direct/Virtual + gain edit.
  const positionInputs = [
    'channelEditXInput', 'channelEditYInput', 'channelEditZInput',
    'channelEditXMetersInput', 'channelEditYMetersInput', 'channelEditZMetersInput',
    'channelEditAzInput', 'channelEditElInput', 'channelEditRInput', 'channelEditRMetersInput',
    'channelEditCartesianMode', 'channelEditPolarMode',
    'channelEditCartesianGizmoBtn', 'channelEditPolarGizmoBtn'
  ];
  for (const inputId of positionInputs) {
    const node = el(inputId);
    if (node) node.disabled = !spatialize;
  }

  const cartGizmoBtn = el('channelEditCartesianGizmoBtn');
  if (cartGizmoBtn) cartGizmoBtn.classList.toggle('active', app.cartesianEditArmed && app.activeEditMode === 'cartesian');
  const polarGizmoBtn = el('channelEditPolarGizmoBtn');
  if (polarGizmoBtn) polarGizmoBtn.classList.toggle('active', app.polarEditArmed && app.activeEditMode === 'polar');
}

function setValueUnlessEditing(id, value) {
  const node = el(id);
  if (!node) return;
  if (document.activeElement === node) return;
  node.value = value;
}

/**
 * Live-update the editor's numeric fields from a scene-space position during a
 * 3D-gizmo drag, without committing to the bed or sending OSC — mirrors how the
 * speaker editor's fields track the mesh while dragging. The authoritative
 * commit happens on release.
 */
export function previewChannelEditorFromScene(x, y, z) {
  const section = el('channelEditSection');
  if (!section || section.style.display === 'none') return;
  const norm = scenePositionToNormalizedOmniphony({ x, y, z });
  const meters = normalizedToMeters(norm);
  const rMeters = Math.hypot(meters.x, meters.y, meters.z);
  const sph = cartesianToSpherical({ x, y, z });
  setValueUnlessEditing('channelEditXInput', formatNumber(norm.x, 3));
  setValueUnlessEditing('channelEditYInput', formatNumber(norm.y, 3));
  setValueUnlessEditing('channelEditZInput', formatNumber(norm.z, 3));
  setValueUnlessEditing('channelEditXMetersInput', formatNumber(meters.x, 2));
  setValueUnlessEditing('channelEditYMetersInput', formatNumber(meters.y, 2));
  setValueUnlessEditing('channelEditZMetersInput', formatNumber(meters.z, 2));
  setValueUnlessEditing('channelEditAzInput', formatNumber(sph.az, 1));
  setValueUnlessEditing('channelEditElInput', formatNumber(sph.el, 1));
  setValueUnlessEditing('channelEditRInput', formatNumber(Math.max(0.01, sph.dist), 3));
  setValueUnlessEditing('channelEditRMetersInput', formatNumber(rMeters, 2));
}
