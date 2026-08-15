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
let outline = null;
let halo = null;
let label = null;
/** Private trail, same shape as a `sourceTrails` entry so the builders accept it. */
let trail = null;
/** Last ADM position applied, so a room-ratio change can be re-projected. */
let lastPosition = [0, 1, 0];
let playing = false;

function build() {
  if (mesh) return;

  mesh = new THREE.Mesh(sourceGeometry, sourceMaterial.clone());
  mesh.material.color.copy(TEST_COLOR);
  mesh.material.depthWrite = false;
  // Mirrors what `getSourceMesh` puts on a real object's userData, so the
  // shared decoration rules below read the same fields they always do.
  mesh.userData.levelScale = 1;
  mesh.userData.objectTrailColor = TEST_TRAIL_COLOR.clone();

  outline = createSourceOutline();
  halo = createSourceDiffuseHalo();
  label = createSmallLabelSprite(t('objectTest.markerLabel'), '#5cff9a');
  trail = { line: createTrailRenderable(), positions: [], lastRebuildAt: 0 };

  for (const item of [mesh, outline, halo, label, trail.line]) {
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
export function updateObjectTestMarker({ position, visible, playing: isPlaying }) {
  if (!visible && !mesh) return; // never opened: build nothing
  build();
  playing = Boolean(isPlaying);

  const moved = Array.isArray(position)
    && position.length === 3
    && position.some((v, i) => v !== lastPosition[i]);
  if (Array.isArray(position) && position.length === 3) {
    lastPosition = position.slice();
  }

  const scenePos = normalizedOmniphonyToScenePosition({
    x: lastPosition[0],
    y: lastPosition[1],
    z: lastPosition[2]
  });
  mesh.position.set(scenePos.x, scenePos.y, scenePos.z);

  // Record the wake only while visible, and only on a real move: a stationary
  // source must not pile up coincident points that the decay then has to chew
  // through, and a hidden one has no wake to leave.
  if (visible && moved && app.trailsEnabled && shouldAppendTrailPoint(trail, performance.now())) {
    trail.positions.push({
      x: lastPosition[0],
      y: lastPosition[1],
      z: lastPosition[2],
      directSpeakerIndex: null,
      t: performance.now()
    });
    rebuildTrail();
  }

  applyDecorations(Boolean(visible));
}

/**
 * Per-frame upkeep, called from the animate loop: billboard the outline toward
 * the camera and expire trail points, exactly as the shared code does for real
 * objects (which do it in `app.js` and `decayTrails` respectively).
 */
export function tickObjectTestMarker(nowMs) {
  if (!mesh || !mesh.parent) return;
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
