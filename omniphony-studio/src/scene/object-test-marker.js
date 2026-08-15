/**
 * The object-injection test source, drawn in the 3D scene as an object.
 *
 * The placement panel shows the source on three flat faces; this puts it in the
 * room itself, which is where the question "is that where I meant" is actually
 * answered — the faces tell you the coordinates, the scene tells you the place.
 *
 * It is styled as a **normal object**, not as a special marker: same sphere,
 * same material, same outline/halo display modes, same trail. It *is* an
 * object — the renderer pans it through the very same path — so making it look
 * like one is the honest depiction, and it means a drag leaves the same visible
 * wake a moving object would. What identifies it is its label and its colour,
 * exactly as for any other object.
 *
 * The styling is imported from `sources.js` rather than copied, so an object
 * that changes appearance (display mode, sphere size, trail render mode) takes
 * the test source with it.
 *
 * What it deliberately does NOT do is join the `sourceMeshes` / `sourceTrails`
 * registries. Those are the renderer's object list: entries there show up in
 * the objects panel, in mute/solo, in the virtual-bed editor, and get swept by
 * the stale-object purge in `tauri-bridge`. A source Studio invented locally
 * has no business in any of that, so it keeps its own handles and reuses only
 * the drawing code.
 */

import * as THREE from 'three';

import { scene, camera } from './setup.js';
import { createSmallLabelSprite } from './labels.js';
import { sourceGeometry, sourceMaterial, SOURCE_BASE_RADIUS } from './materials.js';
import {
  createSourceOutline,
  createSourceDiffuseHalo,
  getObjectDisplayMode,
  getObjectSphereSizeScale
} from '../sources.js';
import {
  createTrailRenderable,
  mapTrailRawToScene,
  rebuildLineTrailGeometry,
  rebuildDiffuseTrailGeometry,
  shouldAppendTrailPoint
} from '../trails.js';
import { normalizedOmniphonyToScenePosition } from '../coordinates.js';
import { app } from '../state.js';
import { t } from '../i18n.js';

/**
 * Its own colour in the object palette — the same green the panel's 2D faces
 * use for the marker, so the two views read as one thing. Objects already come
 * in different colours, so this identifies the source without making it a
 * stylistic exception.
 */
const TEST_COLOR = new THREE.Color(0x5cff9a);
const TEST_TRAIL_COLOR = new THREE.Color(0x2fd47a);

let mesh = null;
let orbitLine = null;
let outline = null;
let halo = null;
let label = null;
/** Private trail, same shape as a `sourceTrails` entry so the builders accept it. */
let trail = null;
/** Last ADM position applied, so a room-ratio change can be re-projected. */
let lastPosition = [0, 1, 0];
let playing = false;
/**
 * Where the renderer last said the source is, in scene units, or null when it
 * is not reporting. Held separately from the placed position because they are
 * different facts: one is where you put the source, the other is where its
 * orbit has carried it.
 */
let reportedTarget = null;
/** The smoothed position actually drawn. See `tickObjectTestMarker`. */
const drawn = new THREE.Vector3();
let drawnValid = false;

function build() {
  if (mesh) return;

  mesh = new THREE.Mesh(sourceGeometry, sourceMaterial.clone());
  mesh.material.color.copy(TEST_COLOR);
  mesh.material.depthWrite = false;
  // Mirrors what `getSourceMesh` puts on a real object's userData, so the
  // shared decoration rules below read the same fields they always do.
  mesh.userData.levelScale = 1;
  mesh.userData.objectTrailColor = TEST_TRAIL_COLOR.clone();

  // The orbit path. Drawn rather than animated: the renderer owns the phase, so
  // Studio cannot know where the source is at this instant — but it knows the
  // path, and a line that is right beats a dot that is plausibly wrong.
  orbitLine = new THREE.Line(
    new THREE.BufferGeometry(),
    new THREE.LineBasicMaterial({
      color: TEST_TRAIL_COLOR.clone(),
      transparent: true,
      opacity: 0.5,
      depthWrite: false
    })
  );
  orbitLine.renderOrder = 14;

  outline = createSourceOutline();
  halo = createSourceDiffuseHalo();
  label = createSmallLabelSprite(t('objectTest.markerLabel'), '#5cff9a');
  trail = { line: createTrailRenderable(), positions: [], lastRebuildAt: 0 };

  for (const item of [mesh, orbitLine, outline, halo, label, trail.line]) {
    item.visible = false;
    scene.add(item);
  }
}

/** Rebuild the trail line from its points, honouring the current render mode. */
function rebuildTrail() {
  if (!trail) return;
  trail.lastRebuildAt = performance.now();
  if (trail.positions.length < 2) {
    trail.line.geometry.dispose();
    trail.line.geometry = new THREE.BufferGeometry();
    return;
  }
  const mapped = trail.positions.map((raw) => mapTrailRawToScene(raw));
  const colors = trail.positions.map(() => TEST_TRAIL_COLOR.clone());
  if (app.trailRenderMode === 'line') {
    rebuildLineTrailGeometry(trail, mapped, colors, trail.positions);
  } else {
    rebuildDiffuseTrailGeometry(trail, mapped, colors, mesh.userData.levelScale, trail.positions);
  }
}

/** Apply the shared object decoration rules to the current position/state. */
function applyDecorations(visible) {
  const displayMode = getObjectDisplayMode();
  const scale = getObjectSphereSizeScale();
  mesh.scale.setScalar(scale);
  mesh.visible = visible && displayMode !== 'circle';
  // A running test is opaque, a merely placed one is faint: the room must not
  // look identical with the signal on and off.
  mesh.material.opacity = playing ? 0.85 : 0.3;

  outline.visible = visible && displayMode === 'circle';
  outline.position.copy(mesh.position);
  outline.scale.setScalar(SOURCE_BASE_RADIUS * scale * 1.08);
  outline.material.opacity = playing ? 0.98 : 0.5;

  halo.visible = visible && displayMode === 'diffuse-sphere';
  halo.position.copy(mesh.position);
  const haloScale = 0.26 * mesh.scale.x * 2.15;
  halo.scale.set(haloScale, haloScale, 1);

  label.visible = visible && app.objectLabelsEnabled;
  label.position.set(mesh.position.x, mesh.position.y + 0.14, mesh.position.z);

  trail.line.visible = visible && app.trailsEnabled;
}

/**
 * Place and show/hide the source.
 *
 * `playing` drives opacity rather than visibility: while the panel is open the
 * source is visible whether or not it is making sound, because placing it is
 * the reason the panel is open.
 */
export function updateObjectTestMarker({ position, reported, visible, playing: isPlaying, orbit }) {
  if (!visible && !mesh) return; // never opened: build nothing
  build();
  playing = Boolean(isPlaying);

  const moved = Array.isArray(position)
    && position.length === 3
    && position.some((v, i) => v !== lastPosition[i]);
  if (Array.isArray(position) && position.length === 3) {
    lastPosition = position.slice();
  }

  // The renderer's report wins when it is talking: it is the only party that
  // knows the orbit phase. Falling back to the placed position keeps a
  // stationary test — and a test that is merely placed, not playing — exactly
  // where the faces say it is.
  const shown = Array.isArray(reported) && reported.length === 3 ? reported : lastPosition;
  const scenePos = normalizedOmniphonyToScenePosition({
    x: shown[0],
    y: shown[1],
    z: shown[2]
  });
  if (Array.isArray(reported) && reported.length === 3) {
    reportedTarget = new THREE.Vector3(scenePos.x, scenePos.y, scenePos.z);
  } else {
    reportedTarget = null;
    drawnValid = false;
    mesh.position.set(scenePos.x, scenePos.y, scenePos.z);
  }
  if (reportedTarget && !drawnValid) {
    // First report of a run: start where it says, rather than sliding in from
    // wherever the source was last parked.
    drawn.copy(reportedTarget);
    drawnValid = true;
    mesh.position.copy(drawn);
  }

  // Record the wake only while visible, and only on a real move: a stationary
  // source must not pile up coincident points that the decay then has to chew
  // through, and a hidden one has no wake to leave.
  const wakeAt = Array.isArray(reported) && reported.length === 3 ? reported : lastPosition;
  const leavesWake = Array.isArray(reported) ? visible : (visible && moved);
  if (leavesWake && app.trailsEnabled && shouldAppendTrailPoint(trail, performance.now())) {
    trail.positions.push({
      x: wakeAt[0],
      y: wakeAt[1],
      z: wakeAt[2],
      directSpeakerIndex: null,
      t: performance.now()
    });
    rebuildTrail();
  }

  // Project the orbit into the scene with the same mapping the source uses, so
  // the path lands on the room geometry rather than beside it.
  if (Array.isArray(orbit) && orbit.length > 1) {
    const pts = orbit.map((p) => {
      const s = normalizedOmniphonyToScenePosition({ x: p[0], y: p[1], z: p[2] });
      return new THREE.Vector3(s.x, s.y, s.z);
    });
    orbitLine.geometry.dispose();
    orbitLine.geometry = new THREE.BufferGeometry().setFromPoints(pts);
    orbitLine.visible = Boolean(visible);
  } else {
    orbitLine.visible = false;
  }
  orbitLine.material.opacity = playing ? 0.75 : 0.35;

  applyDecorations(Boolean(visible));
}

/**
 * Per-frame upkeep, called from the animate loop: billboard the outline toward
 * the camera and expire trail points, exactly as the shared code does for real
 * objects (which do it in `app.js` and `decayTrails` respectively).
 */
export function tickObjectTestMarker(nowMs) {
  if (!mesh || !mesh.parent) return;

  // Chase the reported position instead of snapping to it. The renderer reports
  // on the metering clock — 10 Hz on the CLI's default — while the scene draws
  // at frame rate, so snapping would step the source round the orbit in visible
  // jerks. A short chase costs a few milliseconds of lag and buys continuous
  // motion; it also cuts the corners slightly, so a fast orbit reads a hair
  // smaller than it is.
  if (reportedTarget && drawnValid) {
    drawn.lerp(reportedTarget, 0.25);
    mesh.position.copy(drawn);
    applyDecorations(mesh.visible || outline.visible || halo.visible);
  }

  outline.quaternion.copy(camera.quaternion);
  if (!trail.positions.length) return;
  const cutoff = nowMs - app.trailPointTtlMs;
  const before = trail.positions.length;
  trail.positions = trail.positions.filter((p) => typeof p.t === 'number' && p.t >= cutoff);
  if (trail.positions.length !== before) rebuildTrail();
}

/**
 * Re-project after the room proportions change: the ADM position is unchanged,
 * but where it lands in the room is not — and so is every trail point.
 */
export function refreshObjectTestMarkerProjection() {
  if (!mesh || !mesh.visible) return;
  updateObjectTestMarker({ position: lastPosition, visible: true, playing });
  rebuildTrail();
}

/**
 * The trail render mode changed (line ↔ diffuse): the renderable itself has to
 * be swapped, not just refilled. Mirrors `replaceTrailRenderable`.
 */
export function replaceObjectTestTrailRenderable() {
  if (!trail?.line) return;
  const previous = trail.line;
  const next = createTrailRenderable();
  next.visible = previous.visible;
  scene.add(next);
  scene.remove(previous);
  previous.geometry.dispose();
  previous.material.dispose();
  trail.line = next;
  rebuildTrail();
}

/** Drop the wake — on stop, so a new placement does not inherit the old path. */
export function clearObjectTestTrail() {
  if (!trail) return;
  trail.positions = [];
  rebuildTrail();
}

/** Re-label after a locale change (canvas texture, so `data-i18n` cannot). */
export function relabelObjectTestMarker() {
  if (!label) return;
  const wasVisible = label.visible;
  const previous = label;
  label = createSmallLabelSprite(t('objectTest.markerLabel'), '#5cff9a');
  label.visible = wasVisible;
  label.position.copy(previous.position);
  scene.add(label);
  scene.remove(previous);
}
