import * as THREE from 'three';
import { scene, roomGroup, roomFaces, roomBounds } from './setup.js';
import { createSmallLabelSprite } from './labels.js';
import { app } from '../state.js';
import { mapRoomPosition } from '../coordinates.js';

// ---------------------------------------------------------------------------
// VBAP Cartesian face grids
// ---------------------------------------------------------------------------

const vbapCartesianFaceGridMaterial = new THREE.LineBasicMaterial({
  color: 0x66d8ff,
  transparent: true,
  opacity: 0.42,
  depthWrite: false,
  depthTest: false
});
const vbapCartesianFaceGrids = {
  posX: new THREE.LineSegments(new THREE.BufferGeometry(), vbapCartesianFaceGridMaterial),
  negX: new THREE.LineSegments(new THREE.BufferGeometry(), vbapCartesianFaceGridMaterial),
  posY: new THREE.LineSegments(new THREE.BufferGeometry(), vbapCartesianFaceGridMaterial),
  negY: new THREE.LineSegments(new THREE.BufferGeometry(), vbapCartesianFaceGridMaterial),
  posZ: new THREE.LineSegments(new THREE.BufferGeometry(), vbapCartesianFaceGridMaterial),
  negZ: new THREE.LineSegments(new THREE.BufferGeometry(), vbapCartesianFaceGridMaterial)
};
Object.values(vbapCartesianFaceGrids).forEach((grid) => {
  grid.visible = false;
  grid.renderOrder = 6;
  roomGroup.add(grid);
});

export function axisValues(min, max, count) {
  const n = Number(count);
  if (!Number.isFinite(n) || n < 2) return [];
  const out = [];
  for (let i = 0; i < n; i += 1) {
    const t = i / (n - 1);
    out.push(min + (max - min) * t);
  }
  return out;
}

export function setFaceGridGeometry(faceKey, verts) {
  const grid = vbapCartesianFaceGrids[faceKey];
  if (!grid) return;
  grid.geometry.dispose();
  const geom = new THREE.BufferGeometry();
  if (verts.length > 0) {
    geom.setAttribute('position', new THREE.Float32BufferAttribute(verts, 3));
  }
  grid.geometry = geom;
}

export function syncVbapCartesianFaceGridVisibility() {
  Object.entries(vbapCartesianFaceGrids).forEach(([key, grid]) => {
    const hasLines = Boolean(grid.geometry.getAttribute('position'));
    grid.visible = app.vbapCartesianFaceGridEnabled && hasLines && Boolean(roomFaces[key]?.visible);
  });
}

export function updateVbapCartesianFaceGrid() {
  if (!app.vbapCartesianFaceGridEnabled) {
    Object.values(vbapCartesianFaceGrids).forEach((grid) => { grid.visible = false; });
    return;
  }
  const xN = Number(app.vbapCartesianState.xSize);
  const yN = Number(app.vbapCartesianState.ySize);
  const zN = Number(app.vbapCartesianState.zSize);
  const zNegN = Math.max(0, Math.round(Number(app.vbapCartesianState.zNegSize) || 0));
  if (!Number.isFinite(xN) || !Number.isFinite(yN) || !Number.isFinite(zN) || xN < 2 || yN < 2 || zN < 2) {
    Object.values(vbapCartesianFaceGrids).forEach((grid) => { grid.visible = false; });
    return;
  }

  const xMin = roomBounds.xMin;
  const xMax = roomBounds.xMax;
  const yMin = roomBounds.yMin;
  const yMax = roomBounds.yMax;
  const zMin = roomBounds.zMin;
  const zMax = roomBounds.zMax;

  // Cart axes mapping in scene:
  // cart_x -> scene x (depth), cart_y -> scene z (width), cart_z -> scene y (height)
  const xsRaw = axisValues(-1, 1, xN + 1);
  const xs = xsRaw.map((x) => mapRoomPosition({ x, y: 0, z: 0 }).x);
  const positiveYs = axisValues(0, yMax, zN + 1);
  const negativeYs = zNegN > 0 ? axisValues(yMin, 0, zNegN + 1).slice(0, -1) : [];
  const ys = negativeYs.concat(positiveYs);
  const zs = axisValues(zMin, zMax, yN + 1);

  const line = (x0, y0, z0, x1, y1, z1) => {
    return [x0, y0, z0, x1, y1, z1];
  };

  const posX = [];
  const negX = [];
  const posY = [];
  const negY = [];
  const posZ = [];
  const negZ = [];

  for (const y of ys) {
    posX.push(...line(xMax, y, zMin, xMax, y, zMax));
    negX.push(...line(xMin, y, zMin, xMin, y, zMax));
  }
  for (const z of zs) {
    posX.push(...line(xMax, yMin, z, xMax, yMax, z));
    negX.push(...line(xMin, yMin, z, xMin, yMax, z));
  }

  for (const x of xs) {
    posY.push(...line(x, yMax, zMin, x, yMax, zMax));
    negY.push(...line(x, yMin, zMin, x, yMin, zMax));
  }
  for (const z of zs) {
    posY.push(...line(xMin, yMax, z, xMax, yMax, z));
    negY.push(...line(xMin, yMin, z, xMax, yMin, z));
  }

  for (const x of xs) {
    posZ.push(...line(x, yMin, zMax, x, yMax, zMax));
    negZ.push(...line(x, yMin, zMin, x, yMax, zMin));
  }
  for (const y of ys) {
    posZ.push(...line(xMin, y, zMax, xMax, y, zMax));
    negZ.push(...line(xMin, y, zMin, xMax, y, zMin));
  }

  setFaceGridGeometry('posX', posX);
  setFaceGridGeometry('negX', negX);
  setFaceGridGeometry('posY', posY);
  setFaceGridGeometry('negY', negY);
  setFaceGridGeometry('posZ', posZ);
  setFaceGridGeometry('negZ', negZ);
  syncVbapCartesianFaceGridVisibility();
}

const vbapCartesianGridToggleBtnEl = document.getElementById('vbapCartesianGridToggleBtn');

export function renderVbapCartesianGridToggle() {
  if (!vbapCartesianGridToggleBtnEl) return;
  vbapCartesianGridToggleBtnEl.checked = app.vbapCartesianFaceGridEnabled;
}

// ---------------------------------------------------------------------------
// Speaker gizmo (ring + arc + labels + ticks)
// ---------------------------------------------------------------------------

const ringPoints = Array.from({ length: 64 }, (_, i) => {
  const a = (i / 64) * Math.PI * 2;
  return new THREE.Vector3(Math.cos(a), 0, Math.sin(a));
});
const arcPoints = Array.from({ length: 48 }, (_, i) => {
  const t = (i / 47) * Math.PI - Math.PI / 2;
  return new THREE.Vector3(Math.cos(t), Math.sin(t), 0);
});

const ringTickPoints = [];
for (let i = 0; i < 72; i += 1) {
  const a = (i / 72) * Math.PI * 2;
  const inner = 1.0;
  const outer = 1.08;
  ringTickPoints.push(new THREE.Vector3(Math.cos(a) * inner, 0, Math.sin(a) * inner));
  ringTickPoints.push(new THREE.Vector3(Math.cos(a) * outer, 0, Math.sin(a) * outer));
}

const ringMinorTickPoints = [];
for (let i = 0; i < 360; i += 1) {
  const a = (i / 360) * Math.PI * 2;
  const inner = 1.01;
  const outer = 1.05;
  ringMinorTickPoints.push(new THREE.Vector3(Math.cos(a) * inner, 0, Math.sin(a) * inner));
  ringMinorTickPoints.push(new THREE.Vector3(Math.cos(a) * outer, 0, Math.sin(a) * outer));
}

const arcTickPoints = [];
for (let angle = -90; angle <= 90; angle += 5) {
  const t = (angle * Math.PI) / 180;
  const inner = 1.0;
  const outer = 1.08;
  arcTickPoints.push(new THREE.Vector3(Math.cos(t) * inner, Math.sin(t) * inner, 0));
  arcTickPoints.push(new THREE.Vector3(Math.cos(t) * outer, Math.sin(t) * outer, 0));
}

const arcMinorTickPoints = [];
for (let i = 0; i <= 180; i += 1) {
  const t = (i / 180) * Math.PI - Math.PI / 2;
  const inner = 1.01;
  const outer = 1.05;
  arcMinorTickPoints.push(new THREE.Vector3(Math.cos(t) * inner, Math.sin(t) * inner, 0));
  arcMinorTickPoints.push(new THREE.Vector3(Math.cos(t) * outer, Math.sin(t) * outer, 0));
}

export const speakerGizmo = {
  ring: new THREE.LineLoop(
    new THREE.BufferGeometry().setFromPoints(ringPoints),
    new THREE.LineBasicMaterial({ color: 0x9ef7ff, transparent: true, opacity: 0.6 })
  ),
  ringTicks: new THREE.LineSegments(
    new THREE.BufferGeometry().setFromPoints(ringTickPoints),
    new THREE.LineBasicMaterial({ color: 0x9ef7ff, transparent: true, opacity: 0.5 })
  ),
  ringMinorTicks: new THREE.LineSegments(
    new THREE.BufferGeometry().setFromPoints(ringMinorTickPoints),
    new THREE.LineBasicMaterial({ color: 0x9ef7ff, transparent: true, opacity: 0.35 })
  ),
  arc: new THREE.LineLoop(
    new THREE.BufferGeometry().setFromPoints(arcPoints),
    new THREE.LineBasicMaterial({ color: 0xffd27a, transparent: true, opacity: 0.75 })
  ),
  arcTicks: new THREE.LineSegments(
    new THREE.BufferGeometry().setFromPoints(arcTickPoints),
    new THREE.LineBasicMaterial({ color: 0xffd27a, transparent: true, opacity: 0.55 })
  ),
  arcMinorTicks: new THREE.LineSegments(
    new THREE.BufferGeometry().setFromPoints(arcMinorTickPoints),
    new THREE.LineBasicMaterial({ color: 0xffd27a, transparent: true, opacity: 0.38 })
  ),
  ringLabels: new THREE.Group(),
  arcLabels: new THREE.Group(),
  ringCurrent: new THREE.Group(),
  arcCurrent: new THREE.Group()
};
speakerGizmo.ring.visible = false;
speakerGizmo.ringTicks.visible = false;
speakerGizmo.ringMinorTicks.visible = false;
speakerGizmo.arc.visible = false;
speakerGizmo.arcTicks.visible = false;
speakerGizmo.arcMinorTicks.visible = false;
scene.add(speakerGizmo.ring);
scene.add(speakerGizmo.ringTicks);
scene.add(speakerGizmo.ringMinorTicks);
scene.add(speakerGizmo.arc);
scene.add(speakerGizmo.arcTicks);
scene.add(speakerGizmo.arcMinorTicks);
scene.add(speakerGizmo.ringLabels);
scene.add(speakerGizmo.arcLabels);
scene.add(speakerGizmo.ringCurrent);
scene.add(speakerGizmo.arcCurrent);

speakerGizmo.ringCurrentLabel = createSmallLabelSprite('0', '#9ef7ff');
speakerGizmo.arcCurrentLabel = createSmallLabelSprite('0', '#ffd27a');
speakerGizmo.ringCurrentLabel.renderOrder = 5;
speakerGizmo.arcCurrentLabel.renderOrder = 5;
speakerGizmo.ringCurrent.add(speakerGizmo.ringCurrentLabel);
speakerGizmo.arcCurrent.add(speakerGizmo.arcCurrentLabel);

// ---------------------------------------------------------------------------
// Distance gizmo
// ---------------------------------------------------------------------------

export const distanceGizmo = {
  group: new THREE.Group(),
  line: new THREE.Line(
    new THREE.BufferGeometry().setFromPoints([new THREE.Vector3(), new THREE.Vector3()]),
    new THREE.LineBasicMaterial({ color: 0xa8ffbf, transparent: true, opacity: 0.7 })
  ),
  arrowA: new THREE.Mesh(
    new THREE.ConeGeometry(0.02, 0.06, 8),
    new THREE.MeshBasicMaterial({ color: 0xa8ffbf, transparent: true, opacity: 0.7 })
  ),
  arrowB: new THREE.Mesh(
    new THREE.ConeGeometry(0.02, 0.06, 8),
    new THREE.MeshBasicMaterial({ color: 0xa8ffbf, transparent: true, opacity: 0.7 })
  ),
  label: createSmallLabelSprite('0.00', '#7bff6a')
};
distanceGizmo.arrowA.renderOrder = 5;
distanceGizmo.arrowB.renderOrder = 5;
distanceGizmo.label.renderOrder = 5;
distanceGizmo.group.add(distanceGizmo.line);
distanceGizmo.group.add(distanceGizmo.arrowA);
distanceGizmo.group.add(distanceGizmo.arrowB);
distanceGizmo.group.add(distanceGizmo.label);
distanceGizmo.group.visible = false;
scene.add(distanceGizmo.group);

// ---------------------------------------------------------------------------
// Face shadows (speaker + object selection)
// ---------------------------------------------------------------------------

function createSpeakerFaceShadow(color = 0x000000, opacity = 0.18) {
  return new THREE.Mesh(
    new THREE.CircleGeometry(1, 24),
    new THREE.MeshBasicMaterial({
      color,
      transparent: true,
      opacity,
      side: THREE.DoubleSide,
      depthWrite: false,
      depthTest: false
    })
  );
}

export const selectedSpeakerShadows = {
  posX: createSpeakerFaceShadow(),
  negX: createSpeakerFaceShadow(),
  posY: createSpeakerFaceShadow(),
  negY: createSpeakerFaceShadow(),
  posZ: createSpeakerFaceShadow(),
  negZ: createSpeakerFaceShadow()
};

selectedSpeakerShadows.posX.rotation.y = Math.PI / 2;
selectedSpeakerShadows.negX.rotation.y = -Math.PI / 2;
selectedSpeakerShadows.posY.rotation.x = -Math.PI / 2;
selectedSpeakerShadows.negY.rotation.x = Math.PI / 2;
selectedSpeakerShadows.posZ.rotation.y = Math.PI;
selectedSpeakerShadows.negZ.rotation.y = 0;

Object.values(selectedSpeakerShadows).forEach((shadow) => {
  shadow.visible = false;
  shadow.renderOrder = 3;
  scene.add(shadow);
});

export const selectedObjectShadows = {
  posX: createSpeakerFaceShadow(),
  negX: createSpeakerFaceShadow(),
  posY: createSpeakerFaceShadow(),
  negY: createSpeakerFaceShadow(),
  posZ: createSpeakerFaceShadow(),
  negZ: createSpeakerFaceShadow()
};

selectedObjectShadows.posX.rotation.y = Math.PI / 2;
selectedObjectShadows.negX.rotation.y = -Math.PI / 2;
selectedObjectShadows.posY.rotation.x = -Math.PI / 2;
selectedObjectShadows.negY.rotation.x = Math.PI / 2;
selectedObjectShadows.posZ.rotation.y = Math.PI;
selectedObjectShadows.negZ.rotation.y = 0;

Object.values(selectedObjectShadows).forEach((shadow) => {
  shadow.visible = false;
  shadow.renderOrder = 3;
  scene.add(shadow);
});

// ---------------------------------------------------------------------------
// Cartesian gizmo (XYZ line handles)
// ---------------------------------------------------------------------------

export const cartesianGizmo = {
  group: new THREE.Group(),
  xLine: new THREE.Line(
    new THREE.BufferGeometry().setFromPoints([new THREE.Vector3(), new THREE.Vector3(0.45, 0, 0)]),
    new THREE.LineBasicMaterial({ color: 0xff6b6b, transparent: true, opacity: 0.85 })
  ),
  yLine: new THREE.Line(
    new THREE.BufferGeometry().setFromPoints([new THREE.Vector3(), new THREE.Vector3(0, 0.45, 0)]),
    new THREE.LineBasicMaterial({ color: 0x7fff7f, transparent: true, opacity: 0.85 })
  ),
  zLine: new THREE.Line(
    new THREE.BufferGeometry().setFromPoints([new THREE.Vector3(), new THREE.Vector3(0, 0, 0.45)]),
    new THREE.LineBasicMaterial({ color: 0x6bb8ff, transparent: true, opacity: 0.85 })
  ),
  xHandle: new THREE.Mesh(
    new THREE.SphereGeometry(0.045, 16, 16),
    new THREE.MeshBasicMaterial({ color: 0xff6b6b, transparent: true, opacity: 0.95 })
  ),
  yHandle: new THREE.Mesh(
    new THREE.SphereGeometry(0.045, 16, 16),
    new THREE.MeshBasicMaterial({ color: 0x7fff7f, transparent: true, opacity: 0.95 })
  ),
  zHandle: new THREE.Mesh(
    new THREE.SphereGeometry(0.045, 16, 16),
    new THREE.MeshBasicMaterial({ color: 0x6bb8ff, transparent: true, opacity: 0.95 })
  )
};
cartesianGizmo.xHandle.position.set(0.45, 0, 0);
cartesianGizmo.yHandle.position.set(0, 0.45, 0);
cartesianGizmo.zHandle.position.set(0, 0, 0.45);
cartesianGizmo.xHandle.userData.axis = 'x';
cartesianGizmo.yHandle.userData.axis = 'y';
cartesianGizmo.zHandle.userData.axis = 'z';
cartesianGizmo.group.add(cartesianGizmo.xLine);
cartesianGizmo.group.add(cartesianGizmo.yLine);
cartesianGizmo.group.add(cartesianGizmo.zLine);
cartesianGizmo.group.add(cartesianGizmo.xHandle);
cartesianGizmo.group.add(cartesianGizmo.yHandle);
cartesianGizmo.group.add(cartesianGizmo.zHandle);
cartesianGizmo.group.visible = false;
scene.add(cartesianGizmo.group);

// ---------------------------------------------------------------------------
// Speaker gizmo ring / arc labels
// ---------------------------------------------------------------------------

const ringLabelAngles = Array.from({ length: 24 }, (_, i) => -180 + i * 15);
const arcLabelAngles = Array.from({ length: 13 }, (_, i) => -90 + i * 15);

ringLabelAngles.forEach((angle) => {
  const sprite = createSmallLabelSprite(`${angle}`);
  speakerGizmo.ringLabels.add(sprite);
});

arcLabelAngles.forEach((angle) => {
  const sprite = createSmallLabelSprite(`${angle}`);
  speakerGizmo.arcLabels.add(sprite);
});

export { vbapCartesianFaceGrids as vbapCartesianFaceGrid };
export { ringLabelAngles, arcLabelAngles };
