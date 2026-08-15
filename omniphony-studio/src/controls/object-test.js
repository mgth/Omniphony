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
import { t, onLocaleChange, i18nState } from '../i18n.js';
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
 * The three projections, laid out as an orthographic multiview: front centre,
 * side to its left, floor above it — third-angle projection.
 *
 * Each face maps two ADM axes onto its rectangle. The directions are not free
 * choices: they are what makes the arrangement a projection rather than three
 * unrelated pictures. Unfold the room box with the front wall held still, and
 * the shared edges decide the orientations —
 *
 * - the floor folds UP from the front wall, so the edge it shares with the
 *   front view (its bottom edge) is the FRONT of the room. Depth therefore runs
 *   downward: back at the top, front at the bottom, against the front view.
 * - the left wall folds out to the LEFT, so the edge it shares with the front
 *   view (its right edge) is again the front of the room. Depth runs rightward:
 *   back at the left, front at the right.
 *
 * Both faces then measure depth *away from the front view*, which is why a
 * point's distance from the floor view's bottom edge equals its distance from
 * the side view's right edge — the relationship the 45° mitre line draws.
 */
const FACES = [
  {
    id: 'floor',
    labelKey: 'objectTest.facePlan',
    /** Horizontal axis: ADM x, left to right. */
    h: { axis: 0, invert: false },
    /** Vertical axis: ADM y, back at the top and front at the bottom. */
    v: { axis: 1, invert: false },
    // Depth is the only direction a reader cannot guess — left/right and
    // floor/ceiling speak for themselves — and it is the one the projection
    // decides rather than intuition. So it is the only one labelled.
    depthEnds: 'v',
  },
  {
    id: 'front',
    labelKey: 'objectTest.faceFront',
    h: { axis: 0, invert: false },
    /** ADM z, ceiling at the top. */
    v: { axis: 2, invert: true },
  },
  {
    id: 'side',
    labelKey: 'objectTest.faceSide',
    /** ADM y, back at the left and front at the right, against the front view. */
    h: { axis: 1, invert: false },
    v: { axis: 2, invert: true },
    depthEnds: 'h',
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

/** Gutter between views, as a fraction of the room's largest extent. */
const GUTTER = 0.16;
/** Marker radius, in sheet units (the sheet is normalised to 100). */
const MARKER_R = 2.4;

/**
 * Lay the three views out as a CAD sheet and return every rectangle in one
 * shared coordinate system.
 *
 *        ·          floor          the empty corner carries the 45° mitre
 *       side        front
 *
 * The whole point is the single scale factor `s`: one unit of room is the same
 * number of sheet units in all three views, so the side view's depth is
 * visibly the same length as the floor view's depth, and a tall room looks
 * tall next to its own plan. Three separately-fitted SVGs cannot do this —
 * each would be scaled to its own box — which is why this builds one sheet.
 *
 * Column widths are (depth, width) and row heights are (depth, height), so the
 * empty top-left cell is depth × depth: exactly square, which is what lets the
 * mitre run at a true 45°.
 */
function sheetLayout() {
  const r = roomRatio();
  const W = r.width;
  const D = r.length;
  const H = r.height;
  const g = GUTTER * Math.max(W, D, H);
  const rawW = D + g + W;
  const rawH = D + g + H;
  // Normalise the larger sheet dimension to 100 so stroke widths, marker size
  // and type size read the same whatever the room's proportions.
  const s = 100 / Math.max(rawW, rawH);
  const col2 = (D + g) * s;
  const row2 = (D + g) * s;
  return {
    sheet: { w: rawW * s, h: rawH * s },
    mitre: { x: 0, y: 0, w: D * s, h: D * s },
    rects: {
      floor: { x: col2, y: 0, w: W * s, h: D * s },
      side: { x: 0, y: row2, w: D * s, h: H * s },
      front: { x: col2, y: row2, w: W * s, h: H * s }
    }
  };
}

/** ADM position → a point inside `rect`, for one face. */
function faceToSheet(face, rect, pos) {
  const hv = pos[face.h.axis] * (face.h.invert ? -1 : 1);
  const vv = pos[face.v.axis] * (face.v.invert ? -1 : 1);
  return {
    cx: rect.x + ((hv + 1) / 2) * rect.w,
    cy: rect.y + ((vv + 1) / 2) * rect.h
  };
}

/** A point inside `rect` → the two ADM axes that face drives. */
function sheetToFace(face, rect, px, py) {
  const hv = clamp1(((px - rect.x) / rect.w) * 2 - 1);
  const vv = clamp1(((py - rect.y) / rect.h) * 2 - 1);
  const next = position.slice();
  next[face.h.axis] = hv * (face.h.invert ? -1 : 1);
  next[face.v.axis] = vv * (face.v.invert ? -1 : 1);
  return next;
}

function svgEl(name, attrs) {
  const node = document.createElementNS(SVG_NS, name);
  for (const [k, v] of Object.entries(attrs)) node.setAttribute(k, String(v));
  return node;
}

/** Build the CAD sheet. Rebuilt only when the room proportions change. */
function buildFaces() {
  const host = el('objectTestFaces');
  if (!host) return;
  const r = roomRatio();
  const key = `${r.width}:${r.length}:${r.height}:${i18nState.locale}`;
  if (builtRatioKey === key && host.childElementCount) return;
  builtRatioKey = key;
  host.textContent = '';

  const { sheet, rects, mitre } = sheetLayout();
  // A source against a wall sits exactly on a box edge, where half the marker
  // would fall outside the sheet and be clipped — precisely the positions
  // (hard left, ceiling, back wall) this tool exists to try.
  const pad = MARKER_R + 1;

  const svg = svgEl('svg', {
    viewBox: `${-pad} ${-pad} ${sheet.w + 2 * pad} ${sheet.h + 2 * pad}`,
    preserveAspectRatio: 'xMidYMid meet'
  });
  // touch-action:none so a drag is not stolen by the panel's scroll.
  svg.style.cssText = 'width:100%;height:auto;max-height:300px;display:block;'
    + 'cursor:crosshair;touch-action:none;user-select:none';

  // The mitre: the 45° line that carries depth between the floor view and the
  // side view. It is not decoration — it is the statement that the distance
  // from the floor view's front edge and from the side view's front edge are
  // the same distance, which is what makes these two views one drawing.
  svg.appendChild(svgEl('line', {
    x1: mitre.x, y1: mitre.y + mitre.h, x2: mitre.x + mitre.w, y2: mitre.y,
    class: 'object-test-mitre'
  }));

  for (const face of FACES) {
    const rect = rects[face.id];
    const group = svgEl('g', { 'data-face-id': face.id });

    group.appendChild(svgEl('rect', {
      x: rect.x, y: rect.y, width: rect.w, height: rect.h,
      class: 'object-test-face-box'
    }));

    // The room's midlines, so "dead centre" is visible without measuring.
    group.appendChild(svgEl('line', {
      x1: rect.x + rect.w / 2, y1: rect.y, x2: rect.x + rect.w / 2, y2: rect.y + rect.h,
      class: 'object-test-face-axis'
    }));
    group.appendChild(svgEl('line', {
      x1: rect.x, y1: rect.y + rect.h / 2, x2: rect.x + rect.w, y2: rect.y + rect.h / 2,
      class: 'object-test-face-axis'
    }));

    const caption = svgEl('text', {
      x: rect.x + 1, y: rect.y + rect.h - 1.6, class: 'object-test-face-caption'
    });
    caption.textContent = t(face.labelKey);
    group.appendChild(caption);

    // Depth ends. Both views measure depth away from the front view, so "front"
    // always lands on the edge nearest it: the bottom of the floor view, the
    // right of the side view.
    if (face.depthEnds === 'v') {
      const back = svgEl('text', { x: rect.x + rect.w - 1, y: rect.y + 3.4, class: 'object-test-end-label', 'text-anchor': 'end' });
      back.textContent = t('objectTest.axisBack');
      const front = svgEl('text', { x: rect.x + rect.w - 1, y: rect.y + rect.h - 1.6, class: 'object-test-end-label', 'text-anchor': 'end' });
      front.textContent = t('objectTest.axisFront');
      group.append(back, front);
    } else if (face.depthEnds === 'h') {
      const back = svgEl('text', { x: rect.x + 1, y: rect.y + 3.4, class: 'object-test-end-label' });
      back.textContent = t('objectTest.axisBack');
      const front = svgEl('text', { x: rect.x + rect.w - 1, y: rect.y + 3.4, class: 'object-test-end-label', 'text-anchor': 'end' });
      front.textContent = t('objectTest.axisFront');
      group.append(back, front);
    }

    const marker = svgEl('circle', { r: MARKER_R, class: 'object-test-marker' });
    marker.dataset.role = 'marker';
    group.appendChild(marker);

    svg.appendChild(group);
  }

  attachSheetDrag(svg);
  host.appendChild(svg);
  updateMarkers();
}

/** Screen point → sheet units, honouring the letterboxing. */
function pointInSheet(svg, event) {
  const ctm = svg.getScreenCTM();
  if (!ctm) return null;
  const pt = svg.createSVGPoint();
  pt.x = event.clientX;
  pt.y = event.clientY;
  return pt.matrixTransform(ctm.inverse());
}

/** Which view contains this sheet point, if any. */
function faceAt(px, py) {
  const { rects } = sheetLayout();
  for (const face of FACES) {
    const rect = rects[face.id];
    if (px >= rect.x && px <= rect.x + rect.w && py >= rect.y && py <= rect.y + rect.h) {
      return { face, rect };
    }
  }
  return null;
}

function attachSheetDrag(svg) {
  // One listener for the whole sheet, but a drag stays locked to the view it
  // started in: the views are adjacent, and a pointer that strays across a
  // gutter mid-drag must not silently start driving a different pair of axes.
  let active = null;

  const apply = (event) => {
    const p = pointInSheet(svg, event);
    if (!p || !active) return;
    setPosition(sheetToFace(active.face, active.rect, p.x, p.y));
  };

  svg.addEventListener('pointerdown', (event) => {
    const p = pointInSheet(svg, event);
    if (!p) return;
    const hit = faceAt(p.x, p.y);
    if (!hit) return; // a gutter or the mitre corner: not a placement
    event.preventDefault();
    active = hit;
    try { svg.setPointerCapture(event.pointerId); } catch (_) { /* ignore */ }
    apply(event);
    const move = (ev) => apply(ev);
    const up = () => {
      active = null;
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
  const { rects } = sheetLayout();
  for (const face of FACES) {
    const marker = host.querySelector(`g[data-face-id="${face.id}"] [data-role="marker"]`);
    if (!marker) continue;
    const { cx, cy } = faceToSheet(face, rects[face.id], position);
    marker.setAttribute('cx', String(cx));
    marker.setAttribute('cy', String(cy));
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
