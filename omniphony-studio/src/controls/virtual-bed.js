/**
 * Virtual-bed editor: per-channel placement for fixed-channel sources.
 *
 * Each input channel is either routed direct to its speaker (spatialize:false,
 * e.g. LFE → sub) or virtualized as an object at a position (spatialize:true).
 * The bed is a `SpeakerLayout` (one entry per channel label) pushed live to the
 * renderer via `control_virtual_bed` (a YAML/JSON layout string; an empty string
 * resets to the built-in canonical poses).
 *
 * Editing reuses the speaker-editing mechanic: the channels appear in the Objects
 * list and selecting one opens a parameter panel below it (cartesian / polar,
 * normalized / real, + Direct/Virtual + gain). When no live stream is playing,
 * Studio creates editor-only scene markers (`syncVirtualBedObjects`) so the bed
 * stays visible and editable at rest; live stream objects take over while playing.
 */

import { invoke } from '@tauri-apps/api/core';
import { OBJECT_TEST_SOURCE_ID } from '../object-test-id.js';
import {
  app,
  sourceDirectSpeakerIndices,
  sourceMeshes,
  sourceNames,
  sourcePositionsRaw
} from '../state.js';
import { t, tf } from '../i18n.js';
import { updateSource, removeSource } from '../sources.js';
import {
  sphericalToCartesianDeg,
  cartesianToSpherical,
  scenePositionToNormalizedOmniphony,
  normalizedOmniphonyToScenePosition,
  normalizedToMeters,
  formatNumber
} from '../coordinates.js';
import { buildChannelAliasMap, normalizeChannelName } from '../channel-aliases.js';

// Fallback editable fixed-channel set: the default room corner (ADM
// cartesian: X left/right, Y rear/front, Z down/up; ear level Z = 0) and the
// nominal direction on the sphere (azimuth, elevation) of every channel, used
// until the renderer publishes its catalogue. LFE channels default to direct
// because they cannot be VBAP-panned. The height tier's corner is on the wall
// above its floor speaker, 30° up in a cube. Mirrors the renderer's catalogue.
const FALLBACK_BED = [
  { name: 'L', x: -1, y: 1, z: 0, spatialize: true, azimuth: -30, elevation: 0 },
  { name: 'R', x: 1, y: 1, z: 0, spatialize: true, azimuth: 30, elevation: 0 },
  { name: 'C', x: 0, y: 1, z: 0, spatialize: true, azimuth: 0, elevation: 0 },
  { name: 'LFE', x: 0, y: 1, z: 0, spatialize: false, azimuth: 0, elevation: 0 },
  { name: 'Ls', x: -1, y: 0, z: 0, spatialize: true, azimuth: -110, elevation: 0 },
  { name: 'Rs', x: 1, y: 0, z: 0, spatialize: true, azimuth: 110, elevation: 0 },
  { name: 'Lb', x: -1, y: -1, z: 0, spatialize: true, azimuth: -135, elevation: 0 },
  { name: 'Rb', x: 1, y: -1, z: 0, spatialize: true, azimuth: 135, elevation: 0 },
  { name: 'TFL', x: -1, y: 1, z: 1, spatialize: true, azimuth: -45, elevation: 45 },
  { name: 'TFR', x: 1, y: 1, z: 1, spatialize: true, azimuth: 45, elevation: 45 },
  { name: 'TBL', x: -1, y: -1, z: 1, spatialize: true, azimuth: -135, elevation: 45 },
  { name: 'TBR', x: 1, y: -1, z: 1, spatialize: true, azimuth: 135, elevation: 45 },
  { name: 'Lsc', x: -0.5, y: 1, z: 0, spatialize: true, azimuth: -15, elevation: 0 },
  { name: 'Rsc', x: 0.5, y: 1, z: 0, spatialize: true, azimuth: 15, elevation: 0 },
  { name: 'Cb', x: 0, y: -1, z: 0, spatialize: true, azimuth: 180, elevation: 0 },
  { name: 'Lsd', x: -1, y: -0.5, z: 0, spatialize: true, azimuth: -120, elevation: 0 },
  { name: 'Rsd', x: 1, y: -0.5, z: 0, spatialize: true, azimuth: 120, elevation: 0 },
  { name: 'Lw', x: -1, y: 0.5, z: 0, spatialize: true, azimuth: -60, elevation: 0 },
  { name: 'Rw', x: 1, y: 0.5, z: 0, spatialize: true, azimuth: 60, elevation: 0 },
  { name: 'LFE2', x: 0, y: 1, z: 0, spatialize: false, azimuth: 0, elevation: 0 },
  { name: 'TSL', x: -1, y: 0, z: 1, spatialize: true, azimuth: -90, elevation: 45 },
  { name: 'TSR', x: 1, y: 0, z: 1, spatialize: true, azimuth: 90, elevation: 45 },
  { name: 'TC', x: 0, y: 0, z: 1, spatialize: true, azimuth: 0, elevation: 90 },
  { name: 'TFC', x: 0, y: 1, z: 1, spatialize: true, azimuth: 0, elevation: 45 },
  { name: 'Lh', x: -1, y: 1, z: 0.8165, spatialize: true, azimuth: -30, elevation: 30 },
  { name: 'Rh', x: 1, y: 1, z: 0.8165, spatialize: true, azimuth: 30, elevation: 30 },
  { name: 'Ch', x: 0, y: 1, z: 0.5774, spatialize: true, azimuth: 0, elevation: 30 },
  { name: 'Lhs', x: -1, y: 0, z: 0.5774, spatialize: true, azimuth: -110, elevation: 30 },
  { name: 'Rhs', x: 1, y: 0, z: 0.5774, spatialize: true, azimuth: 110, elevation: 30 }
];

// ---------------------------------------------------------------------------
// Families and modes (docs/placement.md)
// ---------------------------------------------------------------------------

// The source families the renderer's placement policy knows, in tab order.
// `generic` is the base the others inherit from, and what an undeclared
// format gets.
const PLACEMENT_FAMILIES = ['generic', 'dolby', 'dts', 'auro', 'pcm'];
const PLACEMENT_MODES = ['sphere', 'room', 'manual'];
// The renderer's built-in default when neither the family nor generic sets
// a mode: Auro-3D is a sphere, the rest a room.
const BUILTIN_MODE = { auro: 'sphere' };

function familyBlock(family) {
  const block = app.placement && typeof app.placement === 'object' ? app.placement[family] : null;
  return block && typeof block === 'object' ? block : null;
}
function ownMode(family) {
  const mode = familyBlock(family)?.mode;
  return PLACEMENT_MODES.includes(mode) ? mode : null;
}
function ownSpeakers(family) {
  const speakers = familyBlock(family)?.layout?.speakers;
  return Array.isArray(speakers) ? speakers : null;
}
function legacyBedSpeakers() {
  return Array.isArray(app.virtualBed?.speakers) ? app.virtualBed.speakers : null;
}

// One family's placement as the renderer reports it, with the inheritance
// resolved here by the renderer's own rule — so an offline Studio, or one
// whose edit has not been echoed yet, shows the same answer.
function familyPlacement(family) {
  const own = ownMode(family);
  const effectiveMode = own || ownMode('generic') || BUILTIN_MODE[family] || 'room';
  const layoutSource = ownSpeakers(family)
    ? 'own'
    : ownSpeakers('generic') || legacyBedSpeakers()
      ? 'generic'
      : 'none';
  return { ownMode: own, effectiveMode, layoutSource };
}

// The entries a family uses: its own, else the generic family's, else the
// legacy single bed a renderer from before placement reports.
function familySpeakers(family) {
  return ownSpeakers(family) || ownSpeakers('generic') || legacyBedSpeakers() || [];
}

// The family of the stream the renderer is rendering, if a fixed-channel
// stream is playing (`fixedChannelProcessing.family`).
function playingFamily() {
  const processing = app.fixedChannelProcessing;
  if (!processing || processing.stream === 'idle') return null;
  return PLACEMENT_FAMILIES.includes(processing.family) ? processing.family : null;
}

export function editingFamily() {
  return PLACEMENT_FAMILIES.includes(app.placementFamily) ? app.placementFamily : 'generic';
}

// A new stream's family becomes the one being edited, once: a tab picked
// while it plays stays picked.
export function followPlayingFamily() {
  const family = playingFamily();
  if (family && app.placementFollowed !== family) {
    app.placementFollowed = family;
    app.placementFamily = family;
  }
}

// The family's object in the mirrored block, created on demand so an
// optimistic edit lands somewhere even before the first echo.
function familyBlockForWrite(family) {
  if (!app.placement || typeof app.placement !== 'object') app.placement = {};
  if (!app.placement[family] || typeof app.placement[family] !== 'object') {
    app.placement[family] = {};
  }
  return app.placement[family];
}

// Memoised view of the renderer-published fixed-channel catalogue: the alias map
// (normalised spelling → canonical label, from each entry's 'aliases' field) and
// the canonical order list. Rebuilt only when the catalogue reference or length
// changes — the renderer publishes it once at start-up and then keeps it static.
let catalogCache = { catalog: null, length: -1, bySpelling: new Map(), order: [] };
function catalogView() {
  const catalog = Array.isArray(app.fixedChannelCatalog) ? app.fixedChannelCatalog : [];
  if (catalogCache.catalog !== catalog || catalogCache.length !== catalog.length) {
    catalogCache = {
      catalog,
      length: catalog.length,
      bySpelling: buildChannelAliasMap(catalog),
      order: catalog.map((e) => e?.label).filter((l) => typeof l === 'string' && l.trim())
    };
  }
  return catalogCache;
}

// Canonical channel key (L/R/C/LFE/Ls/Rs/Lb/Rb/…) for any spelling the renderer
// accepts, or null. The alias table comes from the published catalogue, so the
// editor matches bed and object entries with exactly the same tolerance as
// layout YAMLs on the renderer.
export function canonicalChannelName(name) {
  if (typeof name !== 'string') return null;
  const norm = normalizeChannelName(name);
  if (!norm) return null;
  const view = catalogView();
  if (view.bySpelling.has(norm)) return view.bySpelling.get(norm);
  // Fallback: match the published catalogue / processing labels directly, in case
  // a label the renderer knows about is missing from its alias table.
  const published = [
    ...view.order,
    ...(Array.isArray(app.fixedChannelProcessing?.labels) ? app.fixedChannelProcessing.labels : []),
    ...(Array.isArray(app.virtualBed?.speakers) ? app.virtualBed.speakers.map((e) => e?.name) : [])
  ].find((label) => typeof label === 'string' && normalizeChannelName(label) === norm);
  if (published) return published.trim();
  // Offline last resort (no catalogue published yet): exact canonical bed names
  // still resolve against the fallback bed; aliases wait for the renderer.
  const fallback = FALLBACK_BED.find((c) => normalizeChannelName(c.name) === norm);
  if (fallback) return fallback.name;
  return null;
}

// Stable fixed-channel order, with the common 7.1.4 set first: the renderer's
// catalogue order when it is published, otherwise the fallback bed order.
function canonicalOrderList() {
  const view = catalogView();
  return view.order.length ? view.order : FALLBACK_BED.map((c) => c.name);
}

// Rank of a channel (by any alias) in the canonical order, or -1 if it is not a
// bed channel. Used to order the objects list for bed sources by the classic
// 5.1/7.1 channel order instead of alphabetically.
export function canonicalChannelOrder(name) {
  const key = canonicalChannelName(name);
  return key ? canonicalOrderList().indexOf(key) : -1;
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

// The room model's channel: the catalogue corner, cartesian, with the polar
// form derived so the editor can show either.
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

// The sphere model's channel: the nominal direction, polar, at unit
// distance — or the room corner when the catalogue gives no direction.
function sphereEntry(base) {
  if (!Number.isFinite(Number(base.azimuth))) return defaultEntry(base);
  const azimuth = Number(base.azimuth);
  const elevation = Number(base.elevation) || 0;
  const norm = polarToAdm(azimuth, elevation, 1.0);
  return {
    name: base.name,
    coordMode: 'polar',
    azimuth,
    elevation,
    distance: 1.0,
    x: norm.x,
    y: norm.y,
    z: norm.z,
    spatialize: base.spatialize !== false,
    gainDb: 0
  };
}

// The channel as the family's `mode` renders it: manual reads the entry's
// pose; room and sphere take the model's pose and only the entry's routing
// and trim.
function channelInMode(base, match, mode) {
  if (mode === 'manual') return readEntry(base, match);
  const channel = mode === 'sphere' ? sphereEntry(base) : defaultEntry(base);
  if (match) {
    if (typeof match.spatialize === 'boolean') channel.spatialize = match.spatialize;
    if (Number.isFinite(Number(match.gain_db))) {
      channel.gainDb = Math.round(Number(match.gain_db) * 10) / 10;
    }
  }
  return channel;
}

// Read a configured entry as a manual-mode channel, falling back to the room
// corner when it can't be parsed.
function readEntry(base, match) {
  if (!match) return defaultEntry(base);
  const cartesian = String(match.coord_mode || '').toLowerCase() === 'cartesian';
  const gainDb = Number.isFinite(Number(match.gain_db))
    ? Math.round(Number(match.gain_db) * 10) / 10
    : 0;
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

// The full editable channel set of one family, as its effective mode renders
// it: the catalogue's channels with the family's entries applied (routing and
// trim in every mode, the pose in manual mode), plus any channel the entries
// or the stream mention that the catalogue does not.
function effectiveChannels(family = editingFamily()) {
  const configured = familySpeakers(family);
  const mode = familyPlacement(family).effectiveMode;
  const published = Array.isArray(app.fixedChannelCatalog) && app.fixedChannelCatalog.length
    ? app.fixedChannelCatalog.map((entry) => ({
        name: entry.label,
        x: Number(entry.x) || 0,
        y: Number(entry.y) || 0,
        z: Number(entry.z) || 0,
        spatialize: entry.spatialize !== false,
        azimuth: entry.azimuth,
        elevation: entry.elevation
      }))
    : FALLBACK_BED;
  const bases = [...published];
  const addBase = (name, source = null) => {
    const key = canonicalChannelName(name) || String(name || '').trim();
    if (!key || bases.some((base) => canonicalChannelName(base.name) === key)) return;
    bases.push({
      name: key,
      x: Number(source?.x) || 0,
      y: Number(source?.y) || 0,
      z: Number(source?.z) || 0,
      spatialize: source?.spatialize !== false
    });
  };
  configured.forEach((entry) => addBase(entry?.name, entry));
  (app.fixedChannelProcessing?.labels || []).forEach((label) => addBase(label));
  return bases.map((base) => {
    const baseKey = canonicalChannelName(base.name) || base.name;
    const match = configured.find((s) => canonicalChannelName(s?.name) === baseKey);
    return channelInMode(base, match, mode);
  });
}

function channelByName(name, family = editingFamily()) {
  const key = canonicalChannelName(name);
  if (!key) return null;
  return effectiveChannels(family).find((c) => c.name === key) || null;
}

function buildLayoutPayload(channels, family = editingFamily()) {
  const stored = Number(familyBlock(family)?.layout?.radius_m);
  const radius = stored > 0 ? stored : 1.0;
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
      const gainDb = Math.round((Number(c.gainDb) || 0) * 10) / 10;
      if (gainDb !== 0) entry.gain_db = gainDb;
      return entry;
    })
  };
}

// A family's own entries, applied to the model at once (so the editor, the 3D
// view and the audio agree before the renderer echoes) and sent.
function setPlacementLayout(family, payload) {
  familyBlockForWrite(family).layout = payload;
  if (family === 'generic') app.virtualBed = payload;
  invoke('control_placement_layout', { family, value: JSON.stringify(payload) });
  syncVirtualBedObjects(true);
  renderChannelEditor(true);
  renderPlacementPanel();
}

// Update one channel entry (by canonical name) via `mutate`, push the whole
// layout of the family being edited to the renderer, and refresh the
// synthetic objects + panel.
function commitChannel(name, mutate) {
  const key = canonicalChannelName(name);
  if (!key) return;
  const family = editingFamily();
  const channels = effectiveChannels(family);
  const target = channels.find((c) => c.name === key);
  if (!target) return;
  mutate(target);
  setPlacementLayout(family, buildLayoutPayload(channels, family));
}

// Which family the channel editor and the at-rest markers show.
export function setPlacementFamily(family) {
  if (!PLACEMENT_FAMILIES.includes(family) || app.placementFamily === family) return;
  app.placementFamily = family;
  syncVirtualBedObjects(true);
  renderChannelEditor(true);
  renderPlacementPanel();
}

// A family's placement mode: null clears the family's own choice, so it
// inherits (the generic mode, else its built-in default).
export function setPlacementMode(family, mode) {
  if (!PLACEMENT_FAMILIES.includes(family)) return;
  if (mode !== null && !PLACEMENT_MODES.includes(mode)) return;
  familyBlockForWrite(family).mode = mode;
  invoke('control_placement_mode', { family, mode: mode ?? 'inherit' });
  syncVirtualBedObjects(true);
  renderChannelEditor(true);
  renderPlacementPanel();
}

// Switch a family to manual mode with the poses it renders right now as its
// entries — what you hear becomes what you edit, with no jump. The renderer's
// own fixed-channel positions are taken when that family is playing (they
// carry the declared angles and the Side/Back choice); the model's poses
// otherwise.
export function switchPlacementToManual(family) {
  if (!PLACEMENT_FAMILIES.includes(family)) return;
  const channels = effectiveChannels(family);
  if (playingFamily() === family) {
    for (const channel of channels) {
      const id = liveFixedSourceId(channel.name);
      const raw = id === null ? null : sourcePositionsRaw.get(String(id));
      if (!raw || raw.fixed !== true) continue;
      const x = Number(raw.x);
      const y = Number(raw.y);
      const z = Number(raw.z);
      if (![x, y, z].every(Number.isFinite)) continue;
      const polar = admToPolar(x, y, z);
      Object.assign(channel, { coordMode: 'cartesian', x, y, z, ...polar });
    }
  }
  // Entries first, then the mode: the plan flips once, with the entries
  // already in place.
  setPlacementLayout(family, buildLayoutPayload(channels, family));
  setPlacementMode(family, 'manual');
}

// The live source that stands for a fixed channel of the playing stream.
function liveFixedSourceId(name) {
  const key = canonicalChannelName(name);
  if (!key) return null;
  for (const [id, sourceName] of sourceNames) {
    if (syntheticIds.has(id)) continue;
    if (canonicalChannelName(sourceName) === key) return id;
  }
  return null;
}

// True when the family being edited places its channels by hand: the only
// mode in which a position is the editor's to move.
export function channelEditable(name) {
  if (channelPlacement(name) !== 'virtual') return false;
  return familyPlacement(editingFamily()).effectiveMode === 'manual';
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
function channelPlacement(name) {
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
    // 0.1 dB resolution, matching the per-speaker output gain.
    c.gainDb = Math.round((Number(gainDb) || 0) * 10) / 10;
  });
}

export function applyChannelPlacement(name, spatialize) {
  commitChannel(name, (c) => {
    c.spatialize = Boolean(spatialize);
  });
}

// Clear a family's own entries: it then uses the generic ones — or, for the
// generic family itself, the defaults (LFE direct, unity trims, the model's
// poses).
export function clearPlacementLayout(family = editingFamily()) {
  if (!PLACEMENT_FAMILIES.includes(family)) return;
  familyBlockForWrite(family).layout = null;
  if (family === 'generic') app.virtualBed = null;
  invoke('control_placement_layout', { family, value: '' });
  syncVirtualBedObjects(true);
  renderChannelEditor(true);
  renderPlacementPanel();
}

// The placement block of the fixed-channel panel: the family tabs (the one
// the renderer is playing is marked), the mode, what the family resolves to,
// and the way back to the generic entries.
export function renderPlacementPanel() {
  const tabs = el('placementFamilyTabs');
  if (!tabs) return;
  const family = editingFamily();
  const placement = familyPlacement(family);
  const playing = playingFamily();
  tabs.replaceChildren(
    ...PLACEMENT_FAMILIES.map((f) => {
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'toggle-btn' + (f === family ? ' active' : '');
      button.dataset.placementFamily = f;
      button.textContent = playing === f ? `${t(`placement.family.${f}`)} ●` : t(`placement.family.${f}`);
      if (playing === f) button.title = t('placement.playing');
      return button;
    })
  );
  const modes = el('placementModeButtons');
  if (modes) {
    const choices = family === 'generic' ? PLACEMENT_MODES : ['inherit', ...PLACEMENT_MODES];
    const current = placement.ownMode ?? 'inherit';
    modes.replaceChildren(
      ...choices.map((mode) => {
        const button = document.createElement('button');
        button.type = 'button';
        button.className = 'toggle-btn' + (mode === current ? ' active' : '');
        button.dataset.placementMode = mode;
        button.textContent = t(`placement.mode.${mode}`);
        return button;
      })
    );
  }
  const modeNote = el('placementModeNote');
  if (modeNote) {
    const modeName = t(`placement.mode.${placement.effectiveMode}`);
    if (placement.ownMode) {
      modeNote.style.display = 'none';
    } else {
      modeNote.style.display = '';
      const inherited = family !== 'generic' && familyPlacement('generic').ownMode;
      modeNote.textContent = tf(inherited ? 'placement.inherited' : 'placement.builtin', { mode: modeName });
    }
  }
  const layoutNote = el('placementLayoutNote');
  if (layoutNote) layoutNote.textContent = t(`placement.layout.${placement.layoutSource}`);
  const actions = el('virtualBedActions');
  if (actions) actions.style.display = placement.layoutSource === 'own' ? 'flex' : 'none';
  const resetBtn = el('virtualBedResetBtn');
  if (resetBtn) {
    resetBtn.textContent = family === 'generic' ? t('virtualBed.reset') : t('placement.useGeneric');
  }
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
// e.g. when an embedding host stops feeding the engine. Left in place they
// linger as stale meshes and double the next stream's objects (or the offline
// channel markers) on resume. They are dropped whenever the stream is not
// active; the engine re-sends them when rendering resumes.
function removeLiveObjects() {
  for (const id of [...sourceMeshes.keys()]) {
    // The injected test source is neither live nor a bed channel: Studio
    // invented it and only its own switch may remove it. Without this the
    // at-rest sweep would delete it the moment the stream stopped — which is
    // exactly when the injection is most likely to be in use.
    if (String(id) === OBJECT_TEST_SOURCE_ID) continue;
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
    fixed: true,
    label: ch.name,
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

// Resolve the output speaker actually used by the selected direct channel.
// Prefer the renderer-reported index (it includes routing choices such as a
// 5.1 surround mapped to the back row), then fall back to the label match so
// the Channel Editor remains informative while offline.
function directSpeakerTarget(name) {
  const selectedId = app.selectedSourceId;
  const reported = selectedId !== null && selectedId !== undefined
    ? sourceDirectSpeakerIndices.get(String(selectedId))
    : undefined;
  const index = Number.isInteger(reported) ? reported : directSpeakerFor(name);
  const speaker = Number.isInteger(index) ? app.currentLayoutSpeakers?.[index] : null;
  if (!speaker) return null;
  return {
    index,
    name: String(speaker.id ?? speaker.name ?? index),
    speaker
  };
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
 * Materialize one editor marker per channel when no live stream is present;
 * remove them otherwise. These are Studio-only scene markers, not renderer
 * metadata or synthesized audio objects. Signature-guarded to avoid per-flush
 * churn unless `force` is set.
 */
export function syncVirtualBedObjects(force = false) {
  // The live OSC stream owns the scene only while it is actively streaming
  // spatial frames (recent spatial:frame, which also covers the post-seek gap).
  // While streaming, live objects are the truth — drop the offline channel
  // markers and keep them.
  const streaming = streamActive();
  if (streaming) {
    removeSyntheticObjects();
    return;
  }
  // Not streaming: any live objects are stale leftovers with no remove event,
  // so drop them before showing the offline catalogue below.
  removeLiveObjects();

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
 * (Re)build the channel editor from the selected object's channel. Hidden when
 * the selected object is not a fixed channel. Skips the rebuild while a field is
 * focused (so typing isn't clobbered) unless `force` is set.
 */
export function renderChannelEditor(force = false) {
  const section = el('channelEditSection');
  if (!section) return;

  // During an active gizmo drag the numeric fields are driven live by
  // previewChannelEditorFromScene; don't let a background flush rebuild them from
  // the (not-yet-committed) bed and clobber the preview.
  if (!force && app.isDraggingVirtualBed) return;

  const key = selectedChannelName();
  if (!key) {
    section.style.display = 'none';
    lastEditorKey = null;
    return;
  }
  if (!force && key === lastEditorKey && section.contains(document.activeElement)) return;
  lastEditorKey = key;

  const family = editingFamily();
  const ch = channelByName(key, family);
  if (!ch) {
    section.style.display = 'none';
    return;
  }
  section.style.display = '';

  const titleEl = el('channelEditTitle');
  if (titleEl) {
    titleEl.textContent = `${t('channelEdit.title')} — ${ch.name} · ${t(`placement.family.${family}`)}`;
  }

  const spatialize = ch.spatialize !== false;
  // A virtual channel is placed by hand in manual mode only; in the sphere
  // and room modes its family's model places it, and the coordinates below
  // are what that model gives.
  const mode = familyPlacement(family).effectiveMode;
  const manual = mode === 'manual';
  const modeNote = el('channelEditModeNote');
  if (modeNote) {
    modeNote.style.display = spatialize && !manual ? '' : 'none';
    modeNote.textContent = tf('placement.positionsFollow', { mode: t(`placement.mode.${mode}`) });
  }
  const modeActions = el('channelEditModeActions');
  if (modeActions) modeActions.style.display = spatialize && !manual ? 'flex' : 'none';
  const directTarget = spatialize ? null : directSpeakerTarget(ch.name);
  const toggle = el('channelEditSpatializeToggle');
  if (toggle) toggle.checked = spatialize;
  const toggleText = el('channelEditSpatializeText');
  if (toggleText) toggleText.textContent = spatialize ? t('virtualBed.virtual') : t('virtualBed.direct');

  const targetRow = el('channelEditDirectTarget');
  if (targetRow) targetRow.style.display = spatialize ? 'none' : 'grid';
  const targetName = el('channelEditDirectTargetName');
  if (targetName) {
    targetName.textContent = directTarget?.name || t('channelEdit.noMatchingSpeaker');
  }

  const coordMode = !spatialize && directTarget
    ? (String(directTarget.speaker.coordMode || directTarget.speaker.coord_mode || '').toLowerCase() === 'polar'
        ? 'polar'
        : 'cartesian')
    : (app.channelEditCoordMode === 'cartesian' ? 'cartesian' : 'polar');
  const cartMode = el('channelEditCartesianMode');
  const polarMode = el('channelEditPolarMode');
  if (cartMode) cartMode.checked = coordMode === 'cartesian';
  if (polarMode) polarMode.checked = coordMode === 'polar';

  // Direct mode displays the destination speaker's real position; the channel's
  // stored virtual pose is deliberately hidden because it is not used. Virtual
  // mode continues to display/edit the channel pose itself.
  const displayPosition = directTarget?.speaker || ch;
  if (!spatialize && !directTarget) {
    for (const inputId of [
      'channelEditXInput', 'channelEditYInput', 'channelEditZInput',
      'channelEditXMetersInput', 'channelEditYMetersInput', 'channelEditZMetersInput',
      'channelEditAzInput', 'channelEditElInput', 'channelEditRInput',
      'channelEditRMetersInput'
    ]) {
      setValueUnlessEditing(inputId, '');
    }
  } else {
    const norm = {
      x: Number(displayPosition.x) || 0,
      y: Number(displayPosition.y) || 0,
      z: Number(displayPosition.z) || 0
    };
    const meters = normalizedToMeters(norm);
    const scene = normalizedOmniphonyToScenePosition(norm);
    const spherical = cartesianToSpherical(scene);
    const azimuth = Number.isFinite(Number(displayPosition.azimuthDeg))
      ? Number(displayPosition.azimuthDeg)
      : spherical.az;
    const elevation = Number.isFinite(Number(displayPosition.elevationDeg))
      ? Number(displayPosition.elevationDeg)
      : spherical.el;
    const distance = Number.isFinite(Number(displayPosition.distanceM))
      ? Number(displayPosition.distanceM)
      : spherical.dist;
    const rMeters = Math.hypot(meters.x, meters.y, meters.z);
    setValueUnlessEditing('channelEditXInput', formatNumber(norm.x, 3));
    setValueUnlessEditing('channelEditYInput', formatNumber(norm.y, 3));
    setValueUnlessEditing('channelEditZInput', formatNumber(norm.z, 3));
    setValueUnlessEditing('channelEditXMetersInput', formatNumber(meters.x, 2));
    setValueUnlessEditing('channelEditYMetersInput', formatNumber(meters.y, 2));
    setValueUnlessEditing('channelEditZMetersInput', formatNumber(meters.z, 2));
    setValueUnlessEditing('channelEditAzInput', formatNumber(azimuth, 1));
    setValueUnlessEditing('channelEditElInput', formatNumber(elevation, 1));
    setValueUnlessEditing('channelEditRInput', formatNumber(distance, 3));
    setValueUnlessEditing('channelEditRMetersInput', formatNumber(rMeters, 2));
  }

  const gainSlider = el('channelEditGainSlider');
  if (gainSlider) gainSlider.value = String(ch.gainDb || 0);
  const gainBox = el('channelEditGainBox');
  if (gainBox) {
    const g = Number(ch.gainDb) || 0;
    gainBox.textContent = `${g > 0 ? '+' : ''}${g.toFixed(1)} dB`;
  }

  // Direct channels are pinned to their speaker, and a sphere or room
  // channel to its model: only Direct/Virtual + gain edit then.
  const positionInputs = [
    'channelEditXInput', 'channelEditYInput', 'channelEditZInput',
    'channelEditXMetersInput', 'channelEditYMetersInput', 'channelEditZMetersInput',
    'channelEditAzInput', 'channelEditElInput', 'channelEditRInput', 'channelEditRMetersInput',
    'channelEditCartesianMode', 'channelEditPolarMode',
    'channelEditCartesianGizmoBtn', 'channelEditPolarGizmoBtn'
  ];
  for (const inputId of positionInputs) {
    const node = el(inputId);
    if (node) node.disabled = !spatialize || !manual;
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
