import * as THREE from 'three';
import { invoke } from '@tauri-apps/api/core';

import { app } from '../state.js';
import { normalizedOmniphonyToScenePosition } from '../coordinates.js';
import { scene } from './setup.js';

const heatmapGroup = new THREE.Group();
heatmapGroup.visible = false;
scene.add(heatmapGroup);

const planeKeys = ['xy', 'xz', 'yz'];
const planeMeshes = new Map();
const planeMaterials = new Map();

const heatmapState = {
  currentRequestId: 0,
  pendingRequestId: 0,
  pendingSlices: new Map(),
  data: null,
};

function createPlaneMaterial() {
  return new THREE.MeshBasicMaterial({
    vertexColors: true,
    transparent: true,
    opacity: 0.58,
    side: THREE.DoubleSide,
    depthWrite: false,
    depthTest: false,
    toneMapped: false,
    polygonOffset: true,
    polygonOffsetFactor: -1,
    polygonOffsetUnits: -1,
  });
}

function ensurePlaneMesh(key) {
  if (planeMeshes.has(key)) {
    return planeMeshes.get(key);
  }
  const material = createPlaneMaterial();
  const mesh = new THREE.Mesh(new THREE.BufferGeometry(), material);
  mesh.renderOrder = 24;
  mesh.frustumCulled = false;
  heatmapGroup.add(mesh);
  planeMeshes.set(key, mesh);
  planeMaterials.set(key, material);
  return mesh;
}

function heatmapColor(value) {
  const t = Math.max(0, Math.min(1, Number(value) || 0));
  const r = Math.round(255 * Math.max(0, Math.min(1, (t - 0.45) / 0.4)));
  const g = Math.round(255 * Math.max(0, Math.min(1, 1.0 - Math.abs(t - 0.55) / 0.35)));
  const b = Math.round(255 * Math.max(0, Math.min(1, (0.7 - t) / 0.55)));
  const a = Math.round(255 * Math.max(0.06, t * 0.9));
  return [r, g, b, a];
}

function buildSliceGeometry(planeKey, axisA, axisB, fixedAxisValue) {
  const width = axisA.length;
  const height = axisB.length;
  const positions = new Float32Array(width * height * 3);
  const colors = new Float32Array(width * height * 4);
  const indices = [];

  let vertexIndex = 0;
  for (let row = 0; row < height; row += 1) {
    for (let col = 0; col < width; col += 1) {
      let omni;
      if (planeKey === 'xy') {
        omni = { x: axisA[col], y: axisB[row], z: fixedAxisValue };
      } else if (planeKey === 'xz') {
        omni = { x: axisA[col], y: fixedAxisValue, z: axisB[row] };
      } else {
        omni = { x: fixedAxisValue, y: axisA[col], z: axisB[row] };
      }
      const pos = normalizedOmniphonyToScenePosition(omni);
      positions[vertexIndex * 3] = pos.x;
      positions[vertexIndex * 3 + 1] = pos.y;
      positions[vertexIndex * 3 + 2] = pos.z;
      vertexIndex += 1;
    }
  }

  for (let row = 0; row < height - 1; row += 1) {
    for (let col = 0; col < width - 1; col += 1) {
      const a = row * width + col;
      const b = a + 1;
      const c = a + width;
      const d = c + 1;
      indices.push(a, c, b, b, c, d);
    }
  }

  const geometry = new THREE.BufferGeometry();
  geometry.setAttribute('position', new THREE.BufferAttribute(positions, 3));
  geometry.setAttribute('color', new THREE.BufferAttribute(colors, 4));
  geometry.setIndex(indices);
  return geometry;
}

function disposeMeshResources(mesh) {
  if (!mesh) return;
  if (mesh.geometry) {
    mesh.geometry.dispose();
  }
}

function hideHeatmap() {
  heatmapGroup.visible = false;
  for (const key of planeKeys) {
    const mesh = planeMeshes.get(key);
    if (mesh) {
      mesh.visible = false;
    }
  }
}

function canShowHeatmap() {
  return Number.isInteger(app.selectedSpeakerIndex)
    && app.selectedSpeakerIndex >= 0
    && app.evaluationModeState.effective === 'precomputed_cartesian';
}

function applyHeatmapData() {
  if (!canShowHeatmap() || !heatmapState.data) {
    hideHeatmap();
    return;
  }

  for (const key of planeKeys) {
    const slice = heatmapState.data[key];
    if (!slice) {
      continue;
    }
    const mesh = ensurePlaneMesh(key);
    disposeMeshResources(mesh);
    mesh.geometry = buildSliceGeometry(
      key,
      slice.axisA,
      slice.axisB,
      slice.fixedAxisValue,
    );
    const colorAttr = mesh.geometry.getAttribute('color');
    for (let index = 0; index < slice.values.length; index += 1) {
      const [r, g, b, a] = heatmapColor(slice.values[index]);
      colorAttr.setXYZW(index, r / 255, g / 255, b / 255, a / 255);
    }
    colorAttr.needsUpdate = true;
    mesh.material.needsUpdate = true;
    mesh.visible = true;
  }
  heatmapGroup.visible = true;
}

function resetPending(requestId) {
  heatmapState.pendingRequestId = requestId;
  heatmapState.pendingSlices = new Map();
}

function maybeFinalizePending(requestId) {
  if (requestId !== heatmapState.pendingRequestId) {
    return;
  }
  if (!heatmapState.pendingSlices.has('meta')) {
    return;
  }
  if (!heatmapState.pendingSlices.has('xy')
      || !heatmapState.pendingSlices.has('xz')
      || !heatmapState.pendingSlices.has('yz')) {
    return;
  }
  heatmapState.data = {
    meta: heatmapState.pendingSlices.get('meta'),
    xy: heatmapState.pendingSlices.get('xy'),
    xz: heatmapState.pendingSlices.get('xz'),
    yz: heatmapState.pendingSlices.get('yz'),
  };
  applyHeatmapData();
}

export function clearSpeakerHeatmap() {
  heatmapState.data = null;
  heatmapState.pendingSlices.clear();
  hideHeatmap();
}

export function refreshSpeakerHeatmapScene() {
  applyHeatmapData();
}

export function requestSpeakerHeatmapIfNeeded() {
  if (!canShowHeatmap()) {
    clearSpeakerHeatmap();
    return;
  }
  const speakerIndex = app.selectedSpeakerIndex;
  heatmapState.currentRequestId += 1;
  const requestId = heatmapState.currentRequestId;
  resetPending(requestId);
  hideHeatmap();
  invoke('request_speaker_heatmap', {
    speakerIndex,
    requestId,
  }).catch(() => {
    if (requestId === heatmapState.pendingRequestId) {
      clearSpeakerHeatmap();
    }
  });
}

export function handleSpeakerHeatmapMeta(payload) {
  const requestId = Number(payload?.request_id);
  if (!Number.isInteger(requestId) || requestId !== heatmapState.pendingRequestId) {
    return;
  }
  heatmapState.pendingSlices.set('meta', payload);
  maybeFinalizePending(requestId);
}

function normalizeSlicePayload(payload) {
  return {
    requestId: Number(payload?.request_id),
    speakerIndex: Number(payload?.speaker_index),
    fixedAxisValue: Number(payload?.fixed_axis_value ?? 0),
    axisA: Array.isArray(payload?.axis_a) ? payload.axis_a.map((value) => Number(value) || 0) : [],
    axisB: Array.isArray(payload?.axis_b) ? payload.axis_b.map((value) => Number(value) || 0) : [],
    values: Array.isArray(payload?.values) ? payload.values.map((value) => Number(value) || 0) : [],
  };
}

export function handleSpeakerHeatmapSlice(planeKey, payload) {
  const normalized = normalizeSlicePayload(payload);
  if (!Number.isInteger(normalized.requestId) || normalized.requestId !== heatmapState.pendingRequestId) {
    return;
  }
  heatmapState.pendingSlices.set(planeKey, normalized);
  maybeFinalizePending(normalized.requestId);
}

export function handleSpeakerHeatmapUnavailable(payload) {
  const requestId = Number(payload?.request_id);
  if (!Number.isInteger(requestId) || requestId !== heatmapState.pendingRequestId) {
    return;
  }
  clearSpeakerHeatmap();
}
