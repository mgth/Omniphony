/**
 * Object injection: place a test source in the room and hear where the renderer
 * puts it.
 *
 * The complement to the speaker test. That one asks "what does this speaker
 * do"; this one asks "where does the renderer put a source I put here" — so the
 * renderer pans it through whichever backend is live rather than writing it into
 * a channel, and what you hear includes the out-of-hull mode, distance model and
 * spread currently configured.
 *
 * The UI is three orthogonal projections of the room box — plan, front wall,
 * side wall. Each face carries two of the three axes, sets exactly those two,
 * and leaves the third untouched; the marker is drawn on all three, so a click
 * on one face visibly moves the source on the other two. That is the cheapest
 * honest way to place a point in 3D with a pointer, and it needs no camera.
 *
 * As with the speaker test, the renderer contract stays small — "a source is at
 * this position at this level", or "stop" — and every question of *when* lives
 * here. Moving is deliberately not a stop/start: the renderer keeps position out
 * of the signal's identity and ramps the gains, so dragging slides the source
 * without ever restarting the noise.
 */

import { invoke } from '@tauri-apps/api/core';
import { app } from '../state.js';
import { t, onLocaleChange } from '../i18n.js';
import { setIdleFeedRequest } from './test-idle-feed.js';
import {
  updateObjectTestMarker,
  refreshObjectTestMarkerProjection,
  relabelObjectTestMarker,
  clearObjectTestTrail
} from '../scene/object-test-marker.js';

const LEVEL_KEY = 'objectTest.levelDb.v1';
const ISOLATION_KEY = 'objectTest.isolation.v1';
const POSITION_KEY = 'objectTest.position.v1';

/** Peak dBFS, matching the speaker test's default loudness. */
const DEFAULT_LEVEL_DB = -8;

/**
 * Height of each face in CSS pixels. Fixed rather than derived from the room
 * proportions so that reconfiguring the room cannot change the panel's height:
 * the room box is fitted *inside* this box by the SVG's own aspect handling, and
 * the overlay's extents stay put. Panel growth that moves the 3D viewport is a
 * known way to break the scene view.
 */
const FACE_H = 86;

/** The three projections. Each names the axes it drives and how they map. */
const FACES = [
  {
    id: 'plan',
    labelKey: 'objectTest.facePlan',
    // Plan view from above: X across, Y up the picture with front at the top.
    axes: { h: 0, v: 1 },
    flipV: false,
    ratio: (r) => [r.width, r.length],
    endsKey: { hLeft: 'objectTest.axisLeft', hRight: 'objectTest.axisRight', vTop: 'objectTest.axisFront', vBottom: 'objectTest.axisBack' },
  },
  {
    id: 'front',
    labelKey: 'objectTest.faceFront',
    // Front wall, seen from the listening position: X across, Z up.
    axes: { h: 0, v: 2 },
    flipV: false,
    ratio: (r) => [r.width, r.height],
    endsKey: { hLeft: 'objectTest.axisLeft', hRight: 'objectTest.axisRight', vTop: 'objectTest.axisCeiling', vBottom: 'objectTest.axisFloor' },
  },
  {
    id: 'side',
    labelKey: 'objectTest.faceSide',
    // Side wall: Y across with back at the left, Z up. Reading left-to-right as
    // back-to-front matches how a room is drawn in section.
    axes: { h: 1, v: 2 },
    flipV: false,
    ratio: (r) => [r.length, r.height],
    endsKey: { hLeft: 'objectTest.axisBack', hRight: 'objectTest.axisFront', vTop: 'objectTest.axisCeiling', vBottom: 'objectTest.axisFloor' },
  },
];

/** Current source position, ADM Cartesian. Front-centre at ear level. */
let position = [0, 1, 0];
let enabled = false;
/** Guards against redrawing faces when only the marker moved. */
let builtRatioKey = null;
/**
 * Whether the panel is open. The 3D marker follows this rather than `enabled`:
 * the source is worth seeing in the room while you are placing it, not only
 * once it is making noise.
 */
let panelOpen = false;

/** Mirror the current state onto the 3D scene marker. */
function syncMarker() {
  updateObjectTestMarker({ position, visible: panelOpen || enabled, playing: enabled });
}

function el(id) { return document.getElementById(id); }

function load(key, fallback) {
  try {
    const v = localStorage.getItem(key);
    return v === null ? fallback : v;
  } catch (_) { return fallback; }
}

function save(key, value) {
  try { localStorage.setItem(key, String(value)); } catch (_) { /* ignore */ }
}

/**
 * Test level in **peak** dBFS — the renderer bounds the test to this peak, so 0
 * is full scale and still cannot clip on its own.
 */
function levelDb() {
  const n = Number(load(LEVEL_KEY, DEFAULT_LEVEL_DB));
  return Number.isFinite(n) ? Math.min(0, Math.max(-60, n)) : DEFAULT_LEVEL_DB;
}

function levelLinear() {
  return 10 ** (levelDb() / 20);
}

function isolation() {
  return load(ISOLATION_KEY, 'test_only');
}

function clamp1(v) {
  return Math.min(1, Math.max(-1, v));
}

/** Room proportions, defaulting to a cube if the renderer has not said yet. */
function roomRatio() {
  const r = app.roomRatio || {};
  const num = (v, d) => (Number.isFinite(Number(v)) && Number(v) > 0 ? Number(v) : d);
  return { width: num(r.width, 1), length: num(r.length, 1), height: num(r.height, 1) };
}

/** Push the current state to the renderer. */
function send() {
  invoke('control_object_test', {
    on: enabled,
    x: position[0],
    y: position[1],
    z: position[2],
    level: levelLinear(),
    size: 0,
    isolation: isolation(),
  }).catch(() => { /* renderer gone */ });
}

export function stopObjectTest({ force = false } = {}) {
  if (!enabled && !force) return;
  enabled = false;
  send();
  renderObjectTestUI();
  syncMarker();
}

// ── Face drawing ─────────────────────────────────────────────────────────────

const SVG_NS = 'http://www.w3.org/2000/svg';

/**
 * Build the three faces. Each SVG's viewBox carries the room's proportions for
 * that projection, and `preserveAspectRatio` fits it inside a fixed-height box —
 * so the drawing matches the configured room while the panel's height does not
 * depend on it.
 */
function buildFaces() {
  const host = el('objectTestFaces');
  if (!host) return;
  const r = roomRatio();
  const key = `${r.width}:${r.length}:${r.height}`;
  if (builtRatioKey === key && host.childElementCount) return;
  builtRatioKey = key;
  host.textContent = '';

  for (const face of FACES) {
    const [rw, rh] = face.ratio(r);
    // Scale the larger side to 100 user units so stroke widths read the same on
    // every face whatever the room's proportions.
    const s = 100 / Math.max(rw, rh);
    const w = rw * s;
    const h = rh * s;
    // A source against a wall sits exactly on the box edge, where half the
    // marker would fall outside the viewBox and be clipped — precisely the
    // positions (hard left, ceiling, back wall) this tool exists to try. Pad the
    // viewBox by the marker's radius so an extreme still reads as a full dot.
    const mr = Math.max(3, s * 0.05);
    const pad = mr + 1;

    const wrap = document.createElement('div');
    wrap.style.cssText = 'display:flex;flex-direction:column;gap:0.1rem';

    const caption = document.createElement('div');
    caption.style.cssText = 'font-size:10px;opacity:0.75;display:flex;justify-content:space-between';
    const name = document.createElement('span');
    name.setAttribute('data-i18n', face.labelKey);
    name.textContent = t(face.labelKey);
    const ends = document.createElement('span');
    ends.style.cssText = 'opacity:0.7';
    ends.textContent = `${t(face.endsKey.hLeft)} → ${t(face.endsKey.hRight)}`;
    caption.append(name, ends);

    const svg = document.createElementNS(SVG_NS, 'svg');
    svg.setAttribute('viewBox', `${-pad} ${-pad} ${w + 2 * pad} ${h + 2 * pad}`);
    // The room box's own extent, which the pointer maths and the marker both
    // work in — the viewBox is only ever wider, by the padding above.
    svg.dataset.boxW = String(w);
    svg.dataset.boxH = String(h);
    svg.setAttribute('preserveAspectRatio', 'xMidYMid meet');
    // touch-action:none so a drag is not stolen by the panel's scroll.
    svg.style.cssText = `width:100%;height:${FACE_H}px;display:block;cursor:crosshair;touch-action:none;user-select:none`;
    svg.dataset.faceId = face.id;

    const box = document.createElementNS(SVG_NS, 'rect');
    box.setAttribute('x', '0');
    box.setAttribute('y', '0');
    box.setAttribute('width', String(w));
    box.setAttribute('height', String(h));
    box.setAttribute('class', 'object-test-face-box');
    svg.appendChild(box);

    // Centre cross-hairs: the room's midlines, so "dead centre" is visible.
    for (const [x1, y1, x2, y2] of [[w / 2, 0, w / 2, h], [0, h / 2, w, h / 2]]) {
      const line = document.createElementNS(SVG_NS, 'line');
      line.setAttribute('x1', String(x1));
      line.setAttribute('y1', String(y1));
      line.setAttribute('x2', String(x2));
      line.setAttribute('y2', String(y2));
      line.setAttribute('class', 'object-test-face-axis');
      svg.appendChild(line);
    }

    const marker = document.createElementNS(SVG_NS, 'circle');
    marker.setAttribute('r', String(mr));
    marker.setAttribute('class', 'object-test-marker');
    marker.dataset.role = 'marker';
    svg.appendChild(marker);

    attachFaceDrag(svg, face, w, h);
    wrap.append(caption, svg);
    host.appendChild(wrap);
  }
  updateMarkers();
}

/** Screen point → this SVG's user units, honouring the letterboxing. */
function pointInSvg(svg, event) {
  const ctm = svg.getScreenCTM();
  if (!ctm) return null;
  const pt = svg.createSVGPoint();
  pt.x = event.clientX;
  pt.y = event.clientY;
  return pt.matrixTransform(ctm.inverse());
}

function attachFaceDrag(svg, face, w, h) {
  const apply = (event) => {
    const p = pointInSvg(svg, event);
    if (!p) return;
    // User units → [-1, 1] per axis. The vertical axis is inverted because SVG
    // y grows downward while both of ours (front, ceiling) grow upward.
    const hv = clamp1((p.x / w) * 2 - 1);
    const vv = clamp1(1 - (p.y / h) * 2);
    const next = position.slice();
    next[face.axes.h] = hv;
    next[face.axes.v] = face.flipV ? -vv : vv;
    setPosition(next);
  };

  svg.addEventListener('pointerdown', (event) => {
    event.preventDefault();
    try { svg.setPointerCapture(event.pointerId); } catch (_) { /* ignore */ }
    apply(event);
    const move = (ev) => apply(ev);
    const up = () => {
      svg.removeEventListener('pointermove', move);
      svg.removeEventListener('pointerup', up);
      svg.removeEventListener('pointercancel', up);
    };
    svg.addEventListener('pointermove', move);
    svg.addEventListener('pointerup', up);
    svg.addEventListener('pointercancel', up);
  });
}

/** Redraw every marker from the current position. */
function updateMarkers() {
  const host = el('objectTestFaces');
  if (!host) return;
  for (const svg of host.querySelectorAll('svg[data-face-id]')) {
    const face = FACES.find((f) => f.id === svg.dataset.faceId);
    const marker = svg.querySelector('[data-role="marker"]');
    if (!face || !marker) continue;
    const w = Number(svg.dataset.boxW);
    const h = Number(svg.dataset.boxH);
    const hv = position[face.axes.h];
    const vv = face.flipV ? -position[face.axes.v] : position[face.axes.v];
    marker.setAttribute('cx', String(((hv + 1) / 2) * w));
    marker.setAttribute('cy', String(((1 - vv) / 2) * h));
  }
}

// ── State ────────────────────────────────────────────────────────────────────

function setPosition(next) {
  const changed = next.some((v, i) => v !== position[i]);
  position = next.map(clamp1);
  if (!changed) return;
  save(POSITION_KEY, JSON.stringify(position));
  updateMarkers();
  syncMarker();
  renderCoords();
  // While running, every move goes straight out: the renderer ramps to it, so
  // this is a slide rather than a restart, and holding back would only add lag.
  if (enabled) send();
}

function renderCoords() {
  const [x, y, z] = position.map((v) => v.toFixed(2));
  const box = el('objectTestCoords');
  if (box) box.textContent = `x ${x}   y ${y}   z ${z}`;
  // The header summary is what remains visible once the section is collapsed,
  // so it has to follow the source too — not just the open panel's readout.
  const summary = el('objectTestSummary');
  if (summary) summary.textContent = enabled ? `▶ ${x}, ${y}, ${z}` : `${x}, ${y}, ${z}`;
}

export function renderObjectTestUI() {
  const toggle = el('objectTestEnableToggle');
  if (toggle) toggle.checked = enabled;
  const box = el('objectTestLevelBox');
  if (box) box.textContent = `${levelDb()} dBFS`;
  const slider = el('objectTestLevelSlider');
  if (slider && document.activeElement !== slider) slider.value = String(levelDb());
  const iso = el('objectTestIsolationSelect');
  if (iso) iso.value = isolation();
  renderCoords();
}

/**
 * Called when the panel opens or closes, and when the room proportions change.
 * Opening arms the idle feed so the chain is warm before the switch is flipped;
 * closing stops the test, because a panel the user cannot see must not be
 * leaving noise in the room.
 */
export function onObjectTestPanelToggled(open) {
  panelOpen = Boolean(open);
  setIdleFeedRequest('object-test', panelOpen);
  if (panelOpen) {
    buildFaces();
    renderObjectTestUI();
  } else {
    if (enabled) {
      // stopObjectTest() re-syncs the marker, which then hides with the panel.
      stopObjectTest();
    }
    // Drop the wake with the panel: reopening should start from a clean room,
    // not from the path of a session the user has already left behind.
    clearObjectTestTrail();
  }
  syncMarker();
}

/** Room proportions changed: the faces' aspect ratios are now wrong. */
export function onRoomRatioChanged() {
  // The ADM position is unchanged, but where it lands in the room is not — so
  // the 3D marker re-projects whether or not the panel is open.
  refreshObjectTestMarkerProjection();
  if (!app.objectTestSectionOpen) {
    builtRatioKey = null; // rebuild lazily on next open
    return;
  }
  builtRatioKey = null;
  buildFaces();
}

export function setupObjectTestListeners() {
  // Restore the last position so reopening the panel resumes where it was.
  try {
    const saved = JSON.parse(load(POSITION_KEY, 'null'));
    if (Array.isArray(saved) && saved.length === 3 && saved.every((v) => Number.isFinite(v))) {
      position = saved.map(clamp1);
    }
  } catch (_) { /* keep the default */ }

  const toggle = el('objectTestEnableToggle');
  if (toggle) {
    toggle.addEventListener('change', () => {
      enabled = toggle.checked;
      send();
      renderObjectTestUI();
      syncMarker();
    });
  }

  const slider = el('objectTestLevelSlider');
  if (slider) {
    slider.addEventListener('input', () => {
      save(LEVEL_KEY, slider.value);
      renderObjectTestUI();
      if (enabled) send();
    });
  }

  const iso = el('objectTestIsolationSelect');
  if (iso) {
    iso.addEventListener('change', () => {
      save(ISOLATION_KEY, iso.value);
      if (enabled) send();
    });
  }

  const centre = el('objectTestCentreBtn');
  if (centre) {
    centre.addEventListener('click', (event) => {
      event.preventDefault();
      setPosition([0, 1, 0]);
    });
  }

  // The scene label is drawn into a canvas texture, so it does not follow
  // `data-i18n` like the panel's markup does and has to be redrawn by hand.
  onLocaleChange(() => {
    relabelObjectTestMarker();
    // The faces' end captions ("left → right") are built in JS too.
    if (panelOpen) {
      builtRatioKey = null;
      buildFaces();
    }
  });

  // Closing the window must not leave a source droning in the room.
  window.addEventListener('beforeunload', () => stopObjectTest({ force: true }));

  renderObjectTestUI();
}
