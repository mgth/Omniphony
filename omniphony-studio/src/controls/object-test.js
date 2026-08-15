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
import {
  normalizedOmniphonyToScenePosition,
  scenePositionToNormalizedOmniphony
} from '../coordinates.js';
import { t, onLocaleChange, i18nState } from '../i18n.js';
import { setIdleFeedRequest } from './test-idle-feed.js';
import { updateSource, removeSource, setSelectedSource, updateSourceLevel } from '../sources.js';
import { OBJECT_TEST_SOURCE_ID } from '../object-test-id.js';

export { OBJECT_TEST_SOURCE_ID };


const LEVEL_KEY = 'objectTest.levelDb.v1';
const FEATURE_KEY = 'objectTest.feature.v1';
const SNAP_KEY = 'objectTest.snap.v1';
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
    /** Horizontal: the room's width. Vertical: its depth, front at the top. */
    h: { axis: 'lateral', invert: false },
    v: { axis: 'depth', invert: true },
    // Depth is the only direction a reader cannot guess — left/right and
    // floor/ceiling speak for themselves — and it is the one the projection
    // decides rather than intuition. So it is the only one labelled.
    depthEnds: 'v',
  },
  {
    id: 'front',
    labelKey: 'objectTest.faceFront',
    h: { axis: 'lateral', invert: false },
    /** Height, ceiling at the top. */
    v: { axis: 'height', invert: true },
  },
  {
    id: 'side',
    labelKey: 'objectTest.faceSide',
    /** Depth, back at the left and front at the right, against the front view. */
    h: { axis: 'depth', invert: false },
    v: { axis: 'height', invert: true },
    depthEnds: 'h',
  },
];


/** Set from storage at setup, applied once the scene can take it. */
let bootFeatureOn = false;
/** Muted from the object list's M/S buttons. Distinct from not playing. */
let testMuted = false;
/** Snap placed positions to the renderer's Cartesian grid. */
let snapOn = false;

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
 * Whether the injected object exists at all. Separate from `enabled`, which is
 * whether it is making noise: the object is worth having in the room — visible,
 * selectable, placeable — before anything is heard.
 */
let featureOn = false;

/**
 * Where the renderer says the source actually is, or null when it has not said.
 * Only the renderer knows: it owns the orbit phase.
 */
let reportedPosition = null;

/**
 * The renderer reported the source's live position and level.
 *
 * The level goes through the same `updateSourceLevel` every other object's
 * meter uses, so the row's gauge, its decay and its peak hold are the shared
 * ones. Studio could not compute this itself: the level control is a target
 * the generator is scaled towards and clamped at, so what is produced is a
 * little under it — a meter fed from the request would be a label.
 */
export function setObjectTestReportedPosition(p, meter) {
  if (!Array.isArray(p) || p.length !== 3 || !p.every(Number.isFinite)) return;
  reportedPosition = p;
  pushSource();
  if (featureOn && meter) {
    updateSourceLevel(OBJECT_TEST_SOURCE_ID, {
      peakDbfs: Number(meter.peakDbfs ?? -100),
      rmsDbfs: Number(meter.rmsDbfs ?? -100),
    });
  }
}

/**
 * Mute the injected source.
 *
 * Routed here rather than to `control_object_mute`, which addresses objects by
 * *number* — this one has a name, so that command would send NaN and the M
 * button would do nothing at all, which is what it did. Muting simply stops
 * sending the signal while remembering that it was playing, so unmuting
 * resumes rather than requiring the play switch again.
 */
export function setObjectTestMuted(muted) {
  const next = Boolean(muted);
  if (next === testMuted) return;
  testMuted = next;
  send();
}

/**
 * Publish the injected object into the source registry, so everything that
 * draws, lists, meters and selects an object handles this one too.
 *
 * Nothing here draws anything. That is the point of making it a real source:
 * the sphere, the outline, the trail, the label, the list row and the level
 * meter are the ones every other object gets, and they follow the user's
 * display settings without this module knowing they exist.
 *
 * The scene shows where the source *is* — the renderer's reported position
 * while it plays, the placed one otherwise. The 2D faces keep showing the
 * placed position regardless: those markers are the handle you drag, and a
 * handle that runs away from the pointer is not a handle.
 */
function pushSource() {
  if (!featureOn) return;
  const at = enabled && reportedPosition ? reportedPosition : position;
  updateSource(OBJECT_TEST_SOURCE_ID, {
    x: at[0],
    y: at[1],
    z: at[2],
    coordMode: 'cartesian',
    name: t('objectTest.markerLabel'),
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

/**
 * The room's extent along each scene axis, as half-spans.
 *
 * A room is not a cube and is not symmetric: it usually reaches further in
 * front than behind and further up than down. Those are separate ratios
 * (`length`/`rear`, `height`/`lower`), and the depth axis is warped on top of
 * that by `centerBlend`. Drawing the faces from `width`/`length`/`height`
 * alone — as this did — draws a room nobody configured, and puts the marker
 * somewhere the 3D view does not.
 */
function roomExtent() {
  const r = app.roomRatio || {};
  const num = (v, d) => (Number.isFinite(Number(v)) && Number(v) > 0 ? Number(v) : d);
  return {
    lateral: { min: -num(r.width, 1), max: num(r.width, 1) },
    depth: { min: -num(r.rear, 1), max: num(r.length, 1) },
    height: { min: -num(r.lower, 0.5), max: num(r.height, 1) },
  };
}

/** ADM → the room space the 3D scene draws in: {depth, height, lateral}. */
function admToRoom(pos) {
  const s = normalizedOmniphonyToScenePosition({ x: pos[0], y: pos[1], z: pos[2] });
  return { depth: s.x, height: s.y, lateral: s.z };
}

/** Room space → ADM, the exact inverse of `admToRoom`. */
function roomToAdm(room) {
  const p = scenePositionToNormalizedOmniphony({ x: room.depth, y: room.height, z: room.lateral });
  return [clamp1(p.x), clamp1(p.y), clamp1(p.z)];
}

/** Push the current state to the renderer. */
function send() {
  invoke('control_object_test', {
    // Muted counts as off to the renderer: there is no separate mute for a
    // source it does not otherwise know about.
    on: enabled && !testMuted,
    x: position[0],
    y: position[1],
    z: position[2],
    level: levelLinear(),
    size: 0,
    isolation: isolation(),
  }).catch(() => { /* renderer gone */ });
}

// ── Snap to the renderer's Cartesian grid ───────────────────────────────────
//
// When the render backend is precomputed on a Cartesian grid, a position
// between two nodes is not a position between two answers: with position
// interpolation off the lookup is nearest-cell, so everything inside a cell
// renders identically. Snapping puts the source on the node it would be
// rounded to anyway, which turns "somewhere near here" into "this cell".
//
// **The published sizes are INTERVAL counts, not node counts.** `snapshot.rs`
// sends `live.evaluation.cartesian.*`, which the renderer turns into an
// evaluator config by adding one (`live_params.rs`: `x_size.max(1) + 1`) before
// handing it to `evenly_spaced_axis`. So a published 62 means 63 nodes at a
// step of 2/62 — and, because 62 is even, a node exactly at zero.
//
// That is not a coincidence, and it is why reading this wrong matters. The
// bridge picks the default to mirror the OAMD position quantisation: Atmos
// encodes x and y on 6 bits at a scale of 1/62 and the bridge maps them with
// `(x - 0.5) * 2`, so the decodable positions are exactly `code/31 - 1` for
// code 0..62 — 63 values, centred on zero. z is a sign bit plus 4 bits at 1/15,
// which is why its two halves join seamlessly at a step of 1/15. Every position
// an Atmos stream can express is a node of this grid. Snapping to a grid built
// from the wrong count would miss all of them.
//
// z is still not one axis: its negative half is `z_neg_size` nodes spaced
// 1/z_neg_size and stopping short of zero (that node belongs to the positive
// half), its positive half is `z_size + 1` nodes from zero. Hence an explicit
// list of nodes rather than a step.

/** The grid the renderer is actually sampling on, or null when there is none. */
function gridAxes() {
  const g = app.vbapCartesianState || {};
  const n = (v) => (Number.isFinite(Number(v)) ? Math.round(Number(v)) : 0);
  // Intervals, as published.
  const xI = n(g.xSize);
  const yI = n(g.ySize);
  const zI = n(g.zSize);
  const zNegNodes = Math.max(0, n(g.zNegSize));
  if (xI < 1 || yI < 1 || zI < 1) return null;

  const evenly = (count, min, max) => {
    if (count <= 1) return [min];
    const step = (max - min) / (count - 1);
    return Array.from({ length: count }, (_, i) => min + step * i);
  };
  const zAxis = [];
  for (let i = 0; i < zNegNodes; i += 1) zAxis.push(-1 + i / zNegNodes);
  zAxis.push(...evenly(zI + 1, 0, 1));
  return [evenly(xI + 1, -1, 1), evenly(yI + 1, -1, 1), zAxis];
}

/** Nearest node on one axis. */
function nearest(nodes, v) {
  let best = nodes[0];
  let bestD = Math.abs(v - best);
  for (const n of nodes) {
    const d = Math.abs(v - n);
    if (d < bestD) { bestD = d; best = n; }
  }
  return best;
}

/**
 * True while a gesture is deliberately ignoring the grid.
 *
 * Snapping is a help until the moment you want the position *between* two
 * nodes — to hear whether the cell boundary is where you think it is, say. A
 * switch to flip and flip back for one drag is worse than the drag itself, so
 * holding Alt suspends it for that gesture, the way every drawing tool does.
 */
let snapBypass = false;

/** Pull a position onto the grid, or return it untouched when there is none. */
function snapToGrid(p) {
  if (!snapOn || snapBypass) return p;
  const axes = gridAxes();
  if (!axes) return p;
  return [nearest(axes[0], p[0]), nearest(axes[1], p[1]), nearest(axes[2], p[2])];
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
  testMuted = false;
  reportedPosition = null;
  send();
  renderObjectTestUI();
  pushSource();
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
  const e = roomExtent();
  // True spans, not half-spans doubled: the room reaches further one way than
  // the other on both depth and height, and the sheet has to show that.
  const W = e.lateral.max - e.lateral.min;
  const D = e.depth.max - e.depth.min;
  const H = e.height.max - e.height.min;
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
    // The mitre cell is square on the depth span, so the 45° still carries
    // depth between the two views that show it.
    mitre: { x: 0, y: row2, w: D * s, h: D * s },
    gutter: g * s,
    extent: e,
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

/** The room axis a slider drives, and whether its travel is inverted. */
function sliderAxis(slider) {
  const face = FACES.find((f) => f.id === slider.face);
  return face[slider.use];
}

/** Which ADM component a room axis corresponds to. */
const ADM_OF_ROOM_AXIS = { lateral: 0, depth: 1, height: 2 };

/** Current value of a slider's coordinate → a point on its track. */
function sliderThumb(slider, layout) {
  const track = sliderTrack(slider, layout);
  const axis = sliderAxis(slider);
  const room = admToRoom(position);
  const { min, max } = layout.extent[axis.axis];
  let t = (room[axis.axis] - min) / Math.max(1e-9, max - min);
  if (axis.invert) t = 1 - t;
  t = Math.min(1, Math.max(0, t));
  return {
    cx: track.x1 + (track.x2 - track.x1) * t,
    cy: track.y1 + (track.y2 - track.y1) * t
  };
}

/**
 * A point on (or near) a slider's track → the room-space value it means.
 *
 * In room space, like the faces: a slider lying alongside a view has to travel
 * with the marker on it, and the marker follows the room's warp.
 */
function sliderValueAt(slider, layout, px, py) {
  const track = sliderTrack(slider, layout);
  let t = track.horizontal
    ? (px - track.x1) / Math.max(1e-9, track.x2 - track.x1)
    : (py - track.y1) / Math.max(1e-9, track.y2 - track.y1);
  t = Math.min(1, Math.max(0, t));
  const axis = sliderAxis(slider);
  if (axis.invert) t = 1 - t;
  const { min, max } = layout.extent[axis.axis];
  return min + t * (max - min);
}

/** Write one room-space coordinate, leaving the other two as they were. */
function setAxisValue(slider, roomValue) {
  const axis = sliderAxis(slider);
  const room = admToRoom(position);
  room[axis.axis] = roomValue;
  setPosition(roomToAdm(room));
}

/** Nudge one ADM coordinate, for the keyboard — where a step in the room's
 *  warped space would grow and shrink as the source moved. */
function nudgeAxis(slider, delta, absolute = null) {
  const axis = sliderAxis(slider);
  const idx = ADM_OF_ROOM_AXIS[axis.axis];
  const next = position.slice();
  next[idx] = clamp1(absolute !== null ? absolute : next[idx] + delta);
  setPosition(next);
}

/**
 * ADM position → a point inside `rect`, for one face.
 *
 * Goes through room space rather than mapping ADM linearly onto the rectangle.
 * The room's depth is warped (`centerBlend`) and its halves are unequal, so a
 * source at ADM y = 0.5 is not half way to the front wall. Projecting the same
 * way the 3D scene does is what makes the two views agree — before this, a
 * marker on a face and the object in the room were in different places.
 */
function faceToSheet(face, rect, pos, extent) {
  const room = admToRoom(pos);
  const along = (axis, value, size, invert) => {
    const { min, max } = extent[axis];
    const t = (value - min) / Math.max(1e-9, max - min);
    return (invert ? 1 - t : t) * size;
  };
  return {
    cx: rect.x + along(face.h.axis, room[face.h.axis], rect.w, face.h.invert),
    cy: rect.y + along(face.v.axis, room[face.v.axis], rect.h, face.v.invert)
  };
}

/** A point inside `rect` → the two ADM axes that face drives. */
function sheetToFace(face, rect, px, py, extent) {
  // Start from where the source is, so the axis this face does not carry is
  // preserved exactly rather than round-tripped through the warp.
  const room = admToRoom(position);
  const back = (axis, px0, size, invert) => {
    const { min, max } = extent[axis];
    let t = Math.min(1, Math.max(0, px0 / Math.max(1e-9, size)));
    if (invert) t = 1 - t;
    return min + t * (max - min);
  };
  room[face.h.axis] = back(face.h.axis, px - rect.x, rect.w, face.h.invert);
  room[face.v.axis] = back(face.v.axis, py - rect.y, rect.h, face.v.invert);
  return roomToAdm(room);
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
  // Every ratio the drawing depends on belongs in the key. It used to hold
  // three of the six, which was harmless while the faces were symmetric boxes
  // and silently wrong the moment they started following the real room: a
  // change to `rear`, `lower` or the depth warp would have left the sheet
  // drawn for the previous shape.
  const e = roomExtent();
  const key = [
    e.lateral.min, e.lateral.max,
    e.depth.min, e.depth.max,
    e.height.min, e.height.max,
    app.roomRatio?.centerBlend,
    i18nState.locale,
  ].join(':');
  if (builtRatioKey === key && host.childElementCount) return;
  builtRatioKey = key;
  host.textContent = '';

  const layout = sheetLayout();
  const { sheet, rects, mitre } = layout;
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

    // The room's axes through the ORIGIN, not through the middle of the
    // rectangle. Those were the same thing while the faces mapped ADM linearly
    // onto a symmetric box; they stopped being the same the moment the faces
    // took the room's true extents. A room that reaches twice as far forward as
    // back has its origin a third of the way up the plan view, and drawing the
    // cross at the halfway mark puts it where the listener is not.
    //
    // Projected with the same function as the marker, so the two cannot drift:
    // wherever the cross meets, placing the source at 0, 0, 0 lands on it.
    const origin = faceToSheet(face, rect, [0, 0, 0], layout.extent);
    group.appendChild(svgEl('line', {
      x1: origin.cx, y1: rect.y, x2: origin.cx, y2: rect.y + rect.h,
      class: 'object-test-face-axis'
    }));
    group.appendChild(svgEl('line', {
      x1: rect.x, y1: origin.cy, x2: rect.x + rect.w, y2: origin.cy,
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

    // The grid the snap lands on, drawn under everything else. Without it the
    // snap is erratic rather than helpful: the room's depth is warped, so the
    // nodes are not evenly spaced on screen and a drag appears to stick at
    // irregular intervals for no visible reason. Shown only while snapping.
    const grid = svgEl('path', { class: 'object-test-grid', d: '' });
    grid.dataset.role = 'grid';
    group.appendChild(grid);

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

  // `layout` already carries gutter and extent — it used to be rebuilt here
  // from a second sheetLayout() call, which was one chance for the two to
  // disagree about a room that had changed in between.
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
    let delta = null;
    let absolute = null;
    switch (event.key) {
      case 'ArrowRight': case 'ArrowUp': delta = 0.02; break;
      case 'ArrowLeft': case 'ArrowDown': delta = -0.02; break;
      case 'PageUp': delta = 0.1; break;
      case 'PageDown': delta = -0.1; break;
      case 'Home': absolute = -1; break;
      case 'End': absolute = 1; break;
      default: return;
    }
    event.preventDefault();
    nudgeAxis(slider, delta ?? 0, absolute);
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
    setPosition(sheetToFace(active.face, active.rect, p.x, p.y, sheetLayout().extent));
  };

  // Track Alt for the whole gesture, including if it is pressed or released
  // mid-drag: the pointer events carry it, so the state cannot go stale.
  const readBypass = (event) => { snapBypass = Boolean(event.altKey); };

  svg.addEventListener('pointerdown', (event) => {
    readBypass(event);
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
    const move = (ev) => { readBypass(ev); apply(ev); };
    const up = () => {
      active = null;
      snapBypass = false;
      svg.removeEventListener('pointermove', move);
      svg.removeEventListener('pointerup', up);
      svg.removeEventListener('pointercancel', up);
    };
    svg.addEventListener('pointermove', move);
    svg.addEventListener('pointerup', up);
    svg.addEventListener('pointercancel', up);
  });
}

/**
 * The snap grid, projected onto one face.
 *
 * Nodes, not lines: at 63 intervals a ruled grid on a 100-unit face is a grey
 * wash. Ticks along each edge say where the nodes are without covering the
 * drawing, and they thin out automatically — if the nodes would fall closer
 * than a stroke apart, only every nth is drawn, so the marks stay countable
 * instead of merging.
 */
function gridPathFor(face, rect, layout, axes) {
  const ticks = [];
  const TICK = 1.6;
  const MIN_GAP = 1.2;

  const edge = (which) => {
    const axisName = face[which].axis;
    const admIdx = ADM_OF_ROOM_AXIS[axisName];
    const nodes = axes[admIdx];
    if (!nodes || nodes.length < 2) return;
    const size = which === 'h' ? rect.w : rect.h;
    // Project every node, then decide how many to skip from the tightest gap.
    const pts = nodes.map((n) => {
      const probe = position.slice();
      probe[admIdx] = n;
      const p = faceToSheet(face, rect, probe, layout.extent);
      return which === 'h' ? p.cx : p.cy;
    }).sort((a, b) => a - b);
    let tightest = Infinity;
    for (let i = 1; i < pts.length; i += 1) tightest = Math.min(tightest, pts[i] - pts[i - 1]);
    const step = Math.max(1, Math.ceil(MIN_GAP / Math.max(1e-6, tightest)));
    for (let i = 0; i < pts.length; i += step) {
      const v = pts[i];
      if (which === 'h') {
        ticks.push(`M${v.toFixed(2)},${rect.y}v${TICK}`);
        ticks.push(`M${v.toFixed(2)},${(rect.y + rect.h).toFixed(2)}v${-TICK}`);
      } else {
        ticks.push(`M${rect.x},${v.toFixed(2)}h${TICK}`);
        ticks.push(`M${(rect.x + rect.w).toFixed(2)},${v.toFixed(2)}h${-TICK}`);
      }
    }
    void size;
  };
  edge('h');
  edge('v');
  return ticks.join(' ');
}

/** Redraw every marker and slider thumb from the current position. */
function updateMarkers() {
  const host = el('objectTestFaces');
  if (!host) return;
  const layout = sheetLayout();
  const path = orbitPath();
  const gridNodes = snapOn ? gridAxes() : null;
  for (const face of FACES) {
    const group = host.querySelector(`g[data-face-id="${face.id}"]`);
    const marker = group?.querySelector('[data-role="marker"]');
    if (!marker) continue;
    const rect = layout.rects[face.id];
    const { cx, cy } = faceToSheet(face, rect, position, layout.extent);
    marker.setAttribute('cx', String(cx));
    marker.setAttribute('cy', String(cy));

    // A circle in 3D projects to an ellipse on a face — or, once the clamp
    // bites, to something with flats on it. Sampling the same function the
    // renderer uses draws whichever it really is, instead of assuming a shape.
    const grid = group.querySelector('[data-role="grid"]');
    if (grid) {
      grid.setAttribute('d', gridNodes ? gridPathFor(face, rect, layout, gridNodes) : '');
    }

    const orbit = group.querySelector('[data-role="orbit"]');
    if (orbit) {
      orbit.setAttribute('points', path
        ? path.map((p) => {
          const q = faceToSheet(face, rect, p, layout.extent);
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
    group.setAttribute('aria-valuenow', position[ADM_OF_ROOM_AXIS[sliderAxis(slider).axis]].toFixed(2));
  }
}

// ── State ────────────────────────────────────────────────────────────────────

function setPosition(next) {
  // Placement snaps; the orbit does not. Rounding a continuous sweep onto a
  // grid would turn smooth motion into a series of jumps, which is the one
  // thing this whole feature is built to avoid.
  const snapped = snapToGrid(next.map(clamp1));
  const changed = snapped.some((v, i) => v !== position[i]);
  position = snapped;
  if (!changed) return;
  save(POSITION_KEY, JSON.stringify(position));
  updateMarkers();
  pushSource();
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
  pushSource();
  sendRotation();
}

export function renderObjectTestUI() {
  const feature = el('objectTestFeatureToggle');
  if (feature) feature.checked = featureOn;
  const snap = el('objectTestSnapToggle');
  const axes = gridAxes();
  if (snap) {
    snap.checked = snapOn;
    // Offered but inert without a grid: the control stays visible so its state
    // is not silently forgotten, and the note says why nothing is happening.
    snap.disabled = !axes;
  }
  const snapNote = el('objectTestSnapNote');
  if (snapNote) {
    snapNote.style.display = snapOn && !axes ? 'block' : 'none';
    if (snapOn && !axes) snapNote.textContent = t('objectTest.snapNoGrid');
  }
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
 * Turn object injection on or off.
 *
 * On: the object joins the source registry, so it appears at the end of the
 * objects list and can be selected like any other; the idle feed is armed so a
 * test started from silence is heard at once; and it is selected, since making
 * something appear that the user then has to hunt for is a poor trade.
 *
 * Off: the test stops and the object is removed. An invented source must not
 * outlive the switch that invented it — least of all in a list where every
 * other entry came from the stream.
 */
export function setObjectTestFeatureOn(on) {
  const next = Boolean(on);
  if (next === featureOn) return;
  featureOn = next;
  save(FEATURE_KEY, featureOn ? '1' : '0');
  setIdleFeedRequest('object-test', featureOn);
  if (featureOn) {
    pushSource();
    // The renderer starts with no orbit; a restored one has to be stated.
    sendRotation();
    setSelectedSource(OBJECT_TEST_SOURCE_ID);
  } else {
    if (enabled) stopObjectTest();
    reportedPosition = null;
    removeSource(OBJECT_TEST_SOURCE_ID);
    if (app.selectedSourceId === OBJECT_TEST_SOURCE_ID) setSelectedSource(null);
  }
  renderObjectTestUI();
  renderObjectTestEditor();
}

export function isObjectTestFeatureOn() {
  return featureOn;
}

/**
 * Show the injection editor exactly when its object is the selected one.
 *
 * It shares the pinned slot with the channel editor, so the two must not both
 * claim it: this one takes it when the injected object is selected, and the
 * channel editor already stands down for any id it does not recognise as a bed
 * channel — which this id is not.
 */
export function renderObjectTestEditor() {
  const section = el('objectTestEditSection');
  if (!section) return;
  const showing = featureOn && app.selectedSourceId === OBJECT_TEST_SOURCE_ID;
  section.style.display = showing ? '' : 'none';
  if (showing) {
    buildFaces();
    renderObjectTestUI();
  }
}

/** Room proportions changed: the faces' aspect ratios are now wrong. */
export function onRoomRatioChanged() {
  // The faces are drawn at the room's proportions, so they are stale; the
  // object itself re-projects on its own, being an ordinary source now.
  builtRatioKey = null;
  if (el('objectTestEditSection')?.style.display !== 'none') buildFaces();
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
      pushSource();
    });
  }

  // Restore the switch, but do not act on it here: the source registry and the
  // 3D scene are not ready this early in boot. `objectTestBoot()` does it once
  // they are.
  bootFeatureOn = load(FEATURE_KEY, '0') === '1';
  snapOn = load(SNAP_KEY, '0') === '1';

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

  const feature = el('objectTestFeatureToggle');
  if (feature) {
    feature.addEventListener('change', () => setObjectTestFeatureOn(feature.checked));
  }

  const snap = el('objectTestSnapToggle');
  if (snap) {
    snap.addEventListener('change', () => {
      snapOn = snap.checked;
      save(SNAP_KEY, snapOn ? '1' : '0');
      // Turning it on pulls the source onto the grid at once, rather than
      // waiting for the next drag to reveal what it does.
      if (snapOn) setPosition(position.slice());
      renderObjectTestUI();
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
      // The room's origin, not the front-centre the source starts at. This is
      // the listener's own position: the one place with no direction at all,
      // which is exactly what makes it worth being able to reach in one click
      // — it is the reference every other placement is heard against.
      //
      // It is also an exact grid node whenever the interval counts are even,
      // which the OAMD-derived default of 62 is, so snapping leaves it alone.
      setPosition([0, 0, 0]);
    });
  }

  // The faces' captions are built in JS, so they do not follow `data-i18n`;
  // neither does the object's name in the list, which is pushed as source data.
  onLocaleChange(() => {
    builtRatioKey = null;
    if (featureOn) pushSource();
    renderObjectTestEditor();
  });

  // Closing the window must not leave a source droning in the room.
  window.addEventListener('beforeunload', () => stopObjectTest({ force: true }));

  renderObjectTestUI();
}

/**
 * Apply the remembered switch state, once the scene and the source registry
 * exist. Called from the boot sequence rather than from `setup`, which runs
 * before either is ready.
 */
export function objectTestBoot() {
  if (bootFeatureOn) setObjectTestFeatureOn(true);
  renderObjectTestEditor();
}
