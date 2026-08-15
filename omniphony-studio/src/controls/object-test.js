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
/**
 * Bumped to v2 when the size became a radius instead of a diameter: the same
 * stored number now describes a circle twice as wide, so reusing v1 would
 * silently double every existing orbit.
 */
const ROTATION_KEY = 'objectTest.rotation.v2';

/** Peak dBFS, matching the speaker test's default loudness. */
const DEFAULT_LEVEL_DB = -8;

/**
 * The three projections, laid out as an orthographic multiview: front centre,
 * side to its left, floor BELOW it — first-angle projection, the ISO
 * convention.
 *
 * Each face maps two ADM axes onto its rectangle. The directions are not free
 * choices: they are what makes the arrangement a projection rather than three
 * unrelated pictures. Unfold the room box with the front wall held still, and
 * the shared edges decide the orientations —
 *
 * - the floor folds DOWN from the front wall, so the edge it shares with the
 *   front view (its top edge) is the FRONT of the room. Depth therefore runs
 *   downward *away* from the front view: front at the top, back at the bottom.
 * - the left wall folds out to the LEFT, so the edge it shares with the front
 *   view (its right edge) is again the front of the room. Depth runs leftward
 *   away from it: front at the right, back at the left.
 *
 * Both faces measure depth away from the front view, which is why a point's
 * distance from the floor view's top edge equals its distance from the side
 * view's right edge — the relationship the 45° mitre line draws.
 *
 * Third angle would put the floor view above with the front of the room at its
 * bottom edge. That is equally consistent, and it is what this drew first, but
 * a plan of a room with its front at the bottom reads backwards to anyone who
 * has ever looked at a floor plan: on a map, forward is up.
 */
const FACES = [
  {
    id: 'floor',
    labelKey: 'objectTest.facePlan',
    /** Horizontal axis: ADM x, left to right. */
    h: { axis: 0, invert: false },
    /** Vertical axis: ADM y, front at the top and back at the bottom. */
    v: { axis: 1, invert: true },
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
/**
 * The orbit. `radius: 0` means none, matching the renderer, so there is no
 * separate flag to keep in step with it.
 */
let rotation = { axis: 'z', radius: 0, period: 4, azimuth: 0, elevation: 0 };
let enabled = false;
/** Guards against redrawing faces when only the marker moved. */
let builtRatioKey = null;
/**
 * Whether the panel is open. The 3D marker follows this rather than `enabled`:
 * the source is worth seeing in the room while you are placing it, not only
 * once it is making noise.
 */
let panelOpen = false;

/**
 * Where the renderer says the source actually is, or null when it has not said.
 *
 * Only the renderer knows: it owns the orbit phase. The 2D faces deliberately
 * keep showing the *placed* position instead — those markers are the handle you
 * drag, and a handle that runs away from the pointer is not a handle.
 */
let reportedPosition = null;

/** The renderer reported the source's live position. */
export function setObjectTestReportedPosition(p) {
  if (!Array.isArray(p) || p.length !== 3 || !p.every(Number.isFinite)) return;
  reportedPosition = p;
  syncMarker();
}

/** Mirror the current state onto the 3D scene marker. */
function syncMarker() {
  updateObjectTestMarker({
    // The scene shows where the source *is*; the faces show where it was put.
    // While a test runs those differ by the orbit, and the 3D view is the one
    // that can afford to move.
    position,
    reported: enabled ? reportedPosition : null,
    visible: panelOpen || enabled,
    playing: enabled,
    orbit: orbitPath(128),
  });
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

// ── Radius: marked at the room's own distances ──────────────────────────────
//
// The marks are not decoration. In a room spanning [-1, 1] on every axis, the
// distance from the centre to a vertical edge is √2 and to a corner is √3 —
// the two radii at which a horizontal orbit passes exactly through the room's
// geometry rather than somewhere near it. Their doubles are the same reach
// from the far wall instead of the centre, which is where you put the centre
// when you want the orbit to sweep the whole room rather than ring the middle
// of it. 4 covers the largest of them with a little room to spare.
//
// Landing on one by dragging would otherwise be luck, so the value snaps when
// it comes close. The window is a hundredth of the range: enough to catch a
// deliberate approach, too small to fight a deliberate miss.

const RADIUS_MAX = 4;
const RADIUS_SNAP = 0.04;
const RADIUS_MARKS = [
  { value: Math.SQRT2, label: '√2' },
  { value: Math.sqrt(3), label: '√3' },
  { value: 2 * Math.SQRT2, label: '2√2' },
  { value: 2 * Math.sqrt(3), label: '2√3' },
];

/** Pull a near-miss onto a landmark, so the marks can actually be hit. */
function snapRadius(v) {
  const r = Math.min(RADIUS_MAX, Math.max(0, v));
  for (const mark of RADIUS_MARKS) {
    if (Math.abs(r - mark.value) <= RADIUS_SNAP) return mark.value;
  }
  return r;
}

/** Name the landmark when sitting on one — otherwise the snap is invisible. */
function formatRadius(r) {
  const mark = RADIUS_MARKS.find((m) => Math.abs(r - m.value) < 1e-6);
  return mark ? `${r.toFixed(2)} ${mark.label}` : r.toFixed(2);
}

// ── Turn time: a logarithmic control ────────────────────────────────────────
//
// A period is chosen by ratio, not by difference: the step from 1 s to 2 s is
// the same musical change as the one from 10 s to 20 s, and on a linear scale
// the first costs a thirtieth of the travel while the second costs a third.
// Linear spent 92% of the slider on turns slower than 3 s — the region where
// one second either way barely reads — and crammed everything quick into the
// first sliver. Equal travel now buys equal ratio.

const PERIOD_MIN = 0.5;
const PERIOD_MAX = 30;
/** Slider positions. Fine enough that the quantisation below does the rounding. */
const PERIOD_STEPS = 1000;

/** Slider position → seconds per turn. */
function sliderToPeriod(pos) {
  const t = Math.min(1, Math.max(0, pos / PERIOD_STEPS));
  const raw = PERIOD_MIN * (PERIOD_MAX / PERIOD_MIN) ** t;
  // Round to something a reader can hold, coarser as the number grows: a
  // hundredth of a second matters at half a second and is noise at twenty.
  if (raw < 1) return Math.round(raw * 20) / 20;
  if (raw < 10) return Math.round(raw * 10) / 10;
  return Math.round(raw * 2) / 2;
}

/** Seconds per turn → slider position. */
function periodToSlider(period) {
  const p = Math.min(PERIOD_MAX, Math.max(PERIOD_MIN, period));
  return Math.round(PERIOD_STEPS * (Math.log(p / PERIOD_MIN) / Math.log(PERIOD_MAX / PERIOD_MIN)));
}

/** Seconds per turn, written the way it was rounded. */
function formatPeriod(period) {
  return period < 1 ? `${period.toFixed(2)} s` : `${period.toFixed(1)} s`;
}

/** Send the orbit. Its own message, since it changes only when a knob does. */
function sendRotation() {
  invoke('control_object_test_rotation', {
    axis: rotation.axis,
    radius: rotation.radius,
    period: rotation.period,
    azimuth: rotation.azimuth,
    elevation: rotation.elevation,
  }).catch(() => { /* renderer gone */ });
}

// ── Orbit geometry, mirrored from the renderer ───────────────────────────────
//
// This duplicates `RotationAxis::frame` and `ObjectTestRotation::position_at`
// from renderer/src/live_params.rs, deliberately and for display only. The
// renderer owns the phase, so Studio can never know exactly where the source is
// at a given instant — but it can know the *path*, and drawing it is the only
// way the radius and axis controls mean anything before you press play.
//
// It mirrors the room clamp too. A drawn circle where the heard one is
// flattened against a wall would be a picture of something that is not
// happening, which is worse than no picture.

function normalize3(v) {
  const n = Math.hypot(v[0], v[1], v[2]);
  return n < 1e-6 ? [1, 0, 0] : [v[0] / n, v[1] / n, v[2] / n];
}

function cross3(a, b) {
  return [
    a[1] * b[2] - a[2] * b[1],
    a[2] * b[0] - a[0] * b[2],
    a[0] * b[1] - a[1] * b[0],
  ];
}

/** The plane the source circles in: two unit vectors spanning it. */
function orbitPlane() {
  switch (rotation.axis) {
    case 'x': return [[0, 1, 0], [0, 0, 1]];
    case 'y': return [[0, 0, 1], [1, 0, 0]];
    case 'free': {
      const az = (rotation.azimuth * Math.PI) / 180;
      const el = (rotation.elevation * Math.PI) / 180;
      const axis = [Math.cos(el) * Math.sin(az), Math.cos(el) * Math.cos(az), Math.sin(el)];
      const seed = Math.abs(axis[2]) < 0.9 ? [0, 0, 1] : [1, 0, 0];
      const u = normalize3(cross3(seed, axis));
      return [u, normalize3(cross3(axis, u))];
    }
    default: return [[1, 0, 0], [0, 1, 0]];
  }
}

/** Where the source sits `t` turns into the orbit. Clamped, like the renderer. */
function orbitPositionAt(t) {
  const [u, v] = orbitPlane();
  const theta = t * Math.PI * 2;
  const r = rotation.radius;
  const c = Math.cos(theta);
  const s = Math.sin(theta);
  return position.map((base, i) => clamp1(base + r * (u[i] * c + v[i] * s)));
}

/** The orbit sampled as a closed path, or null when it is off. */
function orbitPath(samples = 96) {
  if (!(rotation.radius > 0)) return null;
  const pts = [];
  for (let i = 0; i <= samples; i += 1) pts.push(orbitPositionAt(i / samples));
  return pts;
}

export function stopObjectTest({ force = false } = {}) {
  if (!enabled && !force) return;
  enabled = false;
  reportedPosition = null;
  send();
  renderObjectTestUI();
  syncMarker();
}

// ── Face drawing ─────────────────────────────────────────────────────────────

const SVG_NS = 'http://www.w3.org/2000/svg';

/**
 * Gutter between views, as a fraction of the room's largest extent. Wide enough
 * to hold a slider lane: the gutters are not empty space, they carry the
 * single-axis controls.
 */
const GUTTER = 0.24;
/** Marker radius, in sheet units (the sheet is normalised to 100). */
const MARKER_R = 2.4;

/**
 * Lay the three views out as a CAD sheet and return every rectangle in one
 * shared coordinate system.
 *
 *       side        front
 *        ·          floor           the empty corner carries the 45° mitre
 *
 * The whole point is the single scale factor `s`: one unit of room is the same
 * number of sheet units in all three views, so the side view's depth is
 * visibly the same length as the floor view's depth, and a tall room looks
 * tall next to its own plan. Three separately-fitted SVGs cannot do this —
 * each would be scaled to its own box — which is why this builds one sheet.
 *
 * Column widths are (depth, width) and the lower row is depth tall, so the
 * empty bottom-left cell is depth × depth: exactly square, which is what lets
 * the mitre run at a true 45°.
 */
function sheetLayout() {
  const r = roomRatio();
  const W = r.width;
  const D = r.length;
  const H = r.height;
  const g = GUTTER * Math.max(W, D, H);
  const rawW = D + g + W;
  const rawH = H + g + D;
  // Normalise the larger sheet dimension to 100 so stroke widths, marker size
  // and type size read the same whatever the room's proportions.
  const s = 100 / Math.max(rawW, rawH);
  const col2 = (D + g) * s;
  const row1 = 0;
  const row2 = (H + g) * s;
  return {
    sheet: { w: rawW * s, h: rawH * s },
    mitre: { x: 0, y: row2, w: D * s, h: D * s },
    gutter: g * s,
    rects: {
      side: { x: 0, y: row1, w: D * s, h: H * s },
      front: { x: col2, y: row1, w: W * s, h: H * s },
      floor: { x: col2, y: row2, w: W * s, h: D * s }
    }
  };
}

/**
 * The single-axis sliders, one per gutter.
 *
 *       side       front            [x] below the front view
 *       [y]        [x]              [z] left of the front view
 *        ·    [y]  floor            [y] below the side view, left of the floor
 *
 * The two horizontal ones share the middle gutter, between the elevations and
 * the plan — so every control sits inside the drawing's own frame rather than
 * hanging off its edge, and the sheet needs no lane of its own for them.
 *
 * Each lies in the gutter beside the view whose axis it drives, running
 * parallel to that axis *in that view* — so a slider and the marker it moves
 * always travel the same direction, and the control inherits the projection's
 * orientation instead of asserting its own. Nothing here names a direction:
 * the travel is read off the face's axis, so re-orienting a view carries its
 * slider with it.
 *
 * Depth gets two, because two views show it. They run visibly opposite ways —
 * the one beside the floor view has front at the top, the one above the side
 * view has front at the right — because each matches its own neighbour, which
 * is the rule that matters. They need no synchronising code: both read and
 * write the same coordinate, so they cannot disagree.
 */
const SLIDERS = [
  { id: 'x', face: 'front', use: 'h', lane: 'below', labelKey: 'objectTest.sliderX' },
  { id: 'z', face: 'front', use: 'v', lane: 'left', labelKey: 'objectTest.sliderZ' },
  { id: 'ySide', face: 'side', use: 'h', lane: 'below', labelKey: 'objectTest.sliderY' },
  { id: 'yFloor', face: 'floor', use: 'v', lane: 'left', labelKey: 'objectTest.sliderY' }
];

/** Track geometry for a slider, in sheet units. */
function sliderTrack(slider, layout) {
  const rect = layout.rects[slider.face];
  const half = layout.gutter / 2;
  return slider.lane === 'below'
    // Horizontal, centred in the gutter below its view, exactly as long as it.
    ? {
      x1: rect.x, y1: rect.y + rect.h + half,
      x2: rect.x + rect.w, y2: rect.y + rect.h + half,
      horizontal: true
    }
    // Vertical, centred in the gutter to its left.
    : { x1: rect.x - half, y1: rect.y, x2: rect.x - half, y2: rect.y + rect.h, horizontal: false };
}

/** The ADM axis a slider drives, and whether its travel is inverted. */
function sliderAxis(slider) {
  const face = FACES.find((f) => f.id === slider.face);
  return face[slider.use];
}

/** Current value of a slider's coordinate → a point on its track. */
function sliderThumb(slider, layout) {
  const track = sliderTrack(slider, layout);
  const axis = sliderAxis(slider);
  const v = position[axis.axis] * (axis.invert ? -1 : 1);
  const t = (v + 1) / 2;
  return {
    cx: track.x1 + (track.x2 - track.x1) * t,
    cy: track.y1 + (track.y2 - track.y1) * t
  };
}

/** A point on (or near) a slider's track → the coordinate it means. */
function sliderValueAt(slider, layout, px, py) {
  const track = sliderTrack(slider, layout);
  const t = track.horizontal
    ? (px - track.x1) / (track.x2 - track.x1)
    : (py - track.y1) / (track.y2 - track.y1);
  const axis = sliderAxis(slider);
  return clamp1((Math.min(1, Math.max(0, t)) * 2 - 1) * (axis.invert ? -1 : 1));
}

/** Write one coordinate, leaving the other two exactly as they were. */
function setAxisValue(slider, value) {
  const axis = sliderAxis(slider);
  const next = position.slice();
  next[axis.axis] = value;
  setPosition(next);
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
  // side view. It is not decoration — it is the statement that a point's
  // distance from the floor view's front edge (its top) and from the side
  // view's front edge (its right) are the same distance, which is what makes
  // these two views one drawing. The anti-diagonal serves both the first- and
  // third-angle arrangements; only which corner the cell sits in changes.
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

    // Depth ends. Which end is which is read off the axis rather than written
    // down: an inverted axis puts +1 (front) at the start of its travel. Doing
    // it this way means flipping a view's direction cannot leave the labels
    // behind describing the old one.
    if (face.depthEnds) {
      const axis = face[face.depthEnds];
      // The label at the start of the axis's travel — top for a vertical axis,
      // left for a horizontal one.
      const startKey = axis.invert ? 'objectTest.axisFront' : 'objectTest.axisBack';
      const endKey = axis.invert ? 'objectTest.axisBack' : 'objectTest.axisFront';
      const ends = face.depthEnds === 'v'
        // Vertical: both labels on the right, clear of the markers.
        ? [
          { key: startKey, x: rect.x + rect.w - 1, y: rect.y + 3.4, anchor: 'end' },
          { key: endKey, x: rect.x + rect.w - 1, y: rect.y + rect.h - 1.6, anchor: 'end' }
        ]
        : [
          { key: startKey, x: rect.x + 1, y: rect.y + 3.4, anchor: 'start' },
          { key: endKey, x: rect.x + rect.w - 1, y: rect.y + 3.4, anchor: 'end' }
        ];
      for (const e of ends) {
        const node = svgEl('text', {
          x: e.x, y: e.y, class: 'object-test-end-label', 'text-anchor': e.anchor
        });
        node.textContent = t(e.key);
        group.appendChild(node);
      }
    }

    // The orbit's shadow on this face. Drawn before the marker so the marker,
    // which is the thing you are placing, stays on top of it.
    const orbit = svgEl('polyline', { class: 'object-test-orbit', points: '' });
    orbit.dataset.role = 'orbit';
    group.appendChild(orbit);

    const marker = svgEl('circle', { r: MARKER_R, class: 'object-test-marker' });
    marker.dataset.role = 'marker';
    group.appendChild(marker);

    svg.appendChild(group);
  }

  const layout = { sheet, rects, mitre, gutter: sheetLayout().gutter };
  for (const slider of SLIDERS) {
    svg.appendChild(buildSlider(slider, layout));
  }

  attachSheetDrag(svg);
  host.appendChild(svg);
  updateMarkers();
}

/** Half-thickness of a slider's invisible grab area, in sheet units. */
const SLIDER_GRAB = 3.2;

function buildSlider(slider, layout) {
  const track = sliderTrack(slider, layout);
  const group = svgEl('g', {
    'data-slider-id': slider.id,
    class: 'object-test-slider',
    // Focusable and announced, so a coordinate can be nudged from the keyboard
    // rather than only dragged — which is the point of having an axis isolated
    // in the first place.
    tabindex: '0',
    role: 'slider',
    'aria-label': t(slider.labelKey),
    'aria-valuemin': '-1',
    'aria-valuemax': '1',
    'aria-orientation': track.horizontal ? 'horizontal' : 'vertical'
  });

  // A generous invisible grab area: the visible track is a hairline, and a
  // hairline is not a pointer target.
  group.appendChild(svgEl('rect', {
    x: Math.min(track.x1, track.x2) - SLIDER_GRAB,
    y: Math.min(track.y1, track.y2) - SLIDER_GRAB,
    width: Math.abs(track.x2 - track.x1) + SLIDER_GRAB * 2,
    height: Math.abs(track.y2 - track.y1) + SLIDER_GRAB * 2,
    class: 'object-test-slider-hit'
  }));

  group.appendChild(svgEl('line', {
    x1: track.x1, y1: track.y1, x2: track.x2, y2: track.y2,
    class: 'object-test-slider-track'
  }));

  // Centre tick: the room's midpoint on this axis, so zero is findable.
  const midX = (track.x1 + track.x2) / 2;
  const midY = (track.y1 + track.y2) / 2;
  group.appendChild(svgEl('line', {
    x1: track.horizontal ? midX : midX - 1.4,
    y1: track.horizontal ? midY - 1.4 : midY,
    x2: track.horizontal ? midX : midX + 1.4,
    y2: track.horizontal ? midY + 1.4 : midY,
    class: 'object-test-slider-tick'
  }));

  const thumb = svgEl('circle', { r: 2.1, class: 'object-test-slider-thumb' });
  thumb.dataset.role = 'thumb';
  group.appendChild(thumb);

  attachSliderKeys(group, slider);
  return group;
}

/** Arrow keys nudge, page keys step, home/end jump to the walls. */
function attachSliderKeys(group, slider) {
  group.addEventListener('keydown', (event) => {
    const axis = sliderAxis(slider);
    const current = position[axis.axis];
    let next = null;
    switch (event.key) {
      case 'ArrowRight': case 'ArrowUp': next = current + 0.02; break;
      case 'ArrowLeft': case 'ArrowDown': next = current - 0.02; break;
      case 'PageUp': next = current + 0.1; break;
      case 'PageDown': next = current - 0.1; break;
      case 'Home': next = -1; break;
      case 'End': next = 1; break;
      default: return;
    }
    event.preventDefault();
    setAxisValue(slider, clamp1(next));
  });
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

/** Which slider's grab area contains this sheet point, if any. */
function sliderAt(px, py) {
  const layout = sheetLayout();
  for (const slider of SLIDERS) {
    const track = sliderTrack(slider, layout);
    // Pad along the track as well as across it, so the two extremes are as
    // grabbable as the middle. Without this the last fraction of a unit at each
    // end is dead — and the extremes (against a wall, on the ceiling) are
    // exactly the positions this tool exists to try. The value is clamped
    // afterwards, so overshooting the end simply means the end.
    const lo = { x: Math.min(track.x1, track.x2) - SLIDER_GRAB, y: Math.min(track.y1, track.y2) - SLIDER_GRAB };
    const hi = { x: Math.max(track.x1, track.x2) + SLIDER_GRAB, y: Math.max(track.y1, track.y2) + SLIDER_GRAB };
    const within = track.horizontal
      ? px >= lo.x && px <= hi.x && Math.abs(py - track.y1) <= SLIDER_GRAB
      : py >= lo.y && py <= hi.y && Math.abs(px - track.x1) <= SLIDER_GRAB;
    if (within) return slider;
  }
  return null;
}

function attachSheetDrag(svg) {
  // One listener for the whole sheet, but a drag stays locked to what it
  // started on: the views and sliders are adjacent, and a pointer that strays
  // across a gutter mid-drag must not silently start driving something else.
  let active = null;

  const apply = (event) => {
    const p = pointInSheet(svg, event);
    if (!p || !active) return;
    if (active.slider) {
      setAxisValue(active.slider, sliderValueAt(active.slider, sheetLayout(), p.x, p.y));
      return;
    }
    setPosition(sheetToFace(active.face, active.rect, p.x, p.y));
  };

  svg.addEventListener('pointerdown', (event) => {
    const p = pointInSheet(svg, event);
    if (!p) return;
    // Sliders win over the views: their grab areas sit in the gutters, but a
    // generous one can overlap a view's edge, and the narrower intent wins.
    const slider = sliderAt(p.x, p.y);
    const hit = slider ? { slider } : faceAt(p.x, p.y);
    if (!hit) return; // a gutter or the mitre corner: not a placement
    event.preventDefault();
    if (slider) {
      const group = svg.querySelector(`g[data-slider-id="${slider.id}"]`);
      if (group) group.focus?.();
    }
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

/** Redraw every marker and slider thumb from the current position. */
function updateMarkers() {
  const host = el('objectTestFaces');
  if (!host) return;
  const layout = sheetLayout();
  const path = orbitPath();
  for (const face of FACES) {
    const group = host.querySelector(`g[data-face-id="${face.id}"]`);
    const marker = group?.querySelector('[data-role="marker"]');
    if (!marker) continue;
    const rect = layout.rects[face.id];
    const { cx, cy } = faceToSheet(face, rect, position);
    marker.setAttribute('cx', String(cx));
    marker.setAttribute('cy', String(cy));

    // A circle in 3D projects to an ellipse on a face — or, once the clamp
    // bites, to something with flats on it. Sampling the same function the
    // renderer uses draws whichever it really is, instead of assuming a shape.
    const orbit = group.querySelector('[data-role="orbit"]');
    if (orbit) {
      orbit.setAttribute('points', path
        ? path.map((p) => {
          const q = faceToSheet(face, rect, p);
          return `${q.cx.toFixed(2)},${q.cy.toFixed(2)}`;
        }).join(' ')
        : '');
    }
  }
  for (const slider of SLIDERS) {
    const group = host.querySelector(`g[data-slider-id="${slider.id}"]`);
    const thumb = group?.querySelector('[data-role="thumb"]');
    if (!thumb) continue;
    const { cx, cy } = sliderThumb(slider, layout);
    thumb.setAttribute('cx', String(cx));
    thumb.setAttribute('cy', String(cy));
    group.setAttribute('aria-valuenow', position[sliderAxis(slider).axis].toFixed(2));
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

function renderRotationUI() {
  const axis = el('objectTestRotationAxis');
  if (axis) axis.value = rotation.axis;
  const free = el('objectTestFreeAxisRows');
  if (free) free.style.display = rotation.axis === 'free' ? 'flex' : 'none';
  const rad = el('objectTestRadiusSlider');
  if (rad && document.activeElement !== rad) rad.value = String(rotation.radius);
  const radBox = el('objectTestRadiusBox');
  // "off" rather than "0.00": zero radius is a state, not a size.
  if (radBox) radBox.textContent = rotation.radius > 0 ? formatRadius(rotation.radius) : t('objectTest.radiusOff');
  const per = el('objectTestPeriodSlider');
  if (per && document.activeElement !== per) per.value = String(periodToSlider(rotation.period));
  const perBox = el('objectTestPeriodBox');
  if (perBox) perBox.textContent = formatPeriod(rotation.period);
  const az = el('objectTestAzimuthSlider');
  if (az && document.activeElement !== az) az.value = String(rotation.azimuth);
  const azBox = el('objectTestAzimuthBox');
  if (azBox) azBox.textContent = `${Math.round(rotation.azimuth)}°`;
  const elv = el('objectTestElevationSlider');
  if (elv && document.activeElement !== elv) elv.value = String(rotation.elevation);
  const elvBox = el('objectTestElevationBox');
  if (elvBox) elvBox.textContent = `${Math.round(rotation.elevation)}°`;
}

/** A rotation control changed: push it, redraw the path, save it. */
function applyRotation() {
  save(ROTATION_KEY, JSON.stringify(rotation));
  renderRotationUI();
  updateMarkers();
  syncMarker();
  sendRotation();
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
  renderRotationUI();
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
    // The renderer starts with no orbit; a restored one has to be stated.
    sendRotation();
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
  try {
    const saved = JSON.parse(load(ROTATION_KEY, 'null'));
    if (saved && typeof saved === 'object') {
      rotation = {
        axis: ['x', 'y', 'z', 'free'].includes(saved.axis) ? saved.axis : 'z',
        radius: Number.isFinite(saved.radius) ? Math.min(RADIUS_MAX, Math.max(0, saved.radius)) : 0,
        period: Number.isFinite(saved.period) ? Math.min(30, Math.max(0.5, saved.period)) : 4,
        azimuth: Number.isFinite(saved.azimuth) ? saved.azimuth : 0,
        elevation: Number.isFinite(saved.elevation) ? saved.elevation : 0,
      };
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

  const axisSel = el('objectTestRotationAxis');
  if (axisSel) {
    axisSel.addEventListener('change', () => {
      rotation.axis = axisSel.value;
      applyRotation();
    });
  }
  for (const [id, key, parse] of [
    ['objectTestRadiusSlider', 'radius', (v) => snapRadius(Number(v))],
    // The only control whose slider position is not its value.
    ['objectTestPeriodSlider', 'period', (v) => sliderToPeriod(Number(v))],
    ['objectTestAzimuthSlider', 'azimuth', Number],
    ['objectTestElevationSlider', 'elevation', Number],
  ]) {
    const node = el(id);
    if (!node) continue;
    node.addEventListener('input', () => {
      const v = parse(node.value);
      if (!Number.isFinite(v)) return;
      rotation[key] = v;
      applyRotation();
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
