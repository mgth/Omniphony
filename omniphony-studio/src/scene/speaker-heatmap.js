import * as THREE from 'three';
import { invoke } from '@tauri-apps/api/core';

import { app } from '../state.js';
import { normalizedOmniphonyToScenePosition } from '../coordinates.js';
import { scene } from './setup.js';

const SLICE_PLANE_KEYS = ['xy', 'xz', 'yz'];
const VOLUME_BASE_SCALE = 0.012;

const heatmapGroup = new THREE.Group();
heatmapGroup.visible = false;
scene.add(heatmapGroup);

const sliceGroup = new THREE.Group();
sliceGroup.visible = false;
heatmapGroup.add(sliceGroup);

const volumeGroup = new THREE.Group();
volumeGroup.visible = false;
heatmapGroup.add(volumeGroup);

const sliceMeshes = new Map();
let volumeMesh = null;

const heatmapState = {
  currentRequestId: 0,
  slices: {
    pendingRequestId: 0,
    pendingMeta: null,
    pendingSlices: new Map(),
    data: null,
  },
  volume: {
    pendingRequestId: 0,
    pendingMeta: null,
    pendingVolumeChunks: new Map(),
    pendingVolumeChunkCount: null,
    data: null,
  },
};

function createSliceMaterial() {
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

function createVolumeMaterial() {
  return new THREE.MeshBasicMaterial({
    color: 0xffffff,
    transparent: true,
    opacity: 0.72,
    depthWrite: false,
    depthTest: false,
    toneMapped: false,
  });
}

function heatmapColor(value) {
  const t = Math.max(0, Math.min(1, Number(value) || 0));
  const r = Math.round(255 * Math.max(0, Math.min(1, (t - 0.45) / 0.4)));
  const g = Math.round(255 * Math.max(0, Math.min(1, 1.0 - Math.abs(t - 0.55) / 0.35)));
  const b = Math.round(255 * Math.max(0, Math.min(1, (0.7 - t) / 0.55)));
  return new THREE.Color(r / 255, g / 255, b / 255);
}

function ensureSliceMesh(key) {
  if (sliceMeshes.has(key)) {
    return sliceMeshes.get(key);
  }
  const mesh = new THREE.Mesh(new THREE.BufferGeometry(), createSliceMaterial());
  mesh.renderOrder = 24;
  mesh.frustumCulled = false;
  sliceGroup.add(mesh);
  sliceMeshes.set(key, mesh);
  return mesh;
}

function buildSliceGeometry(planeKey, axisA, axisB, fixedAxisValue, values) {
  const width = axisA.length;
  const height = axisB.length;
  const positions = new Float32Array(width * height * 3);
  const colors = new Float32Array(width * height * 3);
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

      const color = heatmapColor(values[vertexIndex]);
      colors[vertexIndex * 3] = color.r;
      colors[vertexIndex * 3 + 1] = color.g;
      colors[vertexIndex * 3 + 2] = color.b;
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
  geometry.setAttribute('color', new THREE.BufferAttribute(colors, 3));
  geometry.setIndex(indices);
  return geometry;
}

function disposeMeshResources(mesh) {
  if (mesh?.geometry) {
    mesh.geometry.dispose();
  }
}

function disposeVolumeMesh() {
  if (!volumeMesh) return;
  volumeGroup.remove(volumeMesh);
  volumeMesh.geometry.dispose();
  volumeMesh.material.dispose();
  volumeMesh = null;
}

function hideAllHeatmap() {
  heatmapGroup.visible = false;
  sliceGroup.visible = false;
  volumeGroup.visible = false;
  for (const mesh of sliceMeshes.values()) {
    mesh.visible = false;
  }
  if (volumeMesh) {
    volumeMesh.visible = false;
  }
}

function canShowHeatmap() {
  return Number.isInteger(app.selectedSpeakerIndex)
    && app.selectedSpeakerIndex >= 0
    && app.evaluationModeState.effective === 'precomputed_cartesian'
    && (app.speakerHeatmapSlicesEnabled || app.speakerHeatmapVolumeEnabled);
}

function renderSlices(slices) {
  for (const planeKey of SLICE_PLANE_KEYS) {
    const slice = slices[planeKey];
    const mesh = ensureSliceMesh(planeKey);
    disposeMeshResources(mesh);
    mesh.geometry = buildSliceGeometry(
      planeKey,
      slice.axisA,
      slice.axisB,
      slice.fixedAxisValue,
      slice.values,
    );
    mesh.visible = true;
  }
  sliceGroup.visible = true;
}

function buildVolumeMesh(samples) {
  const sampleCount = Math.floor(samples.length / 4);
  const geometry = new THREE.IcosahedronGeometry(1, 1);
  const material = createVolumeMaterial();
  const mesh = new THREE.InstancedMesh(geometry, material, sampleCount);
  mesh.instanceMatrix.setUsage(THREE.DynamicDrawUsage);
  mesh.renderOrder = 23;
  mesh.frustumCulled = false;

  const matrix = new THREE.Matrix4();
  const position = new THREE.Vector3();
  const scale = new THREE.Vector3();
  const quaternion = new THREE.Quaternion();
  let maxGain = 0;

  for (let index = 0; index < sampleCount; index += 1) {
    const gain = Math.max(0, Number(samples[index * 4 + 3]) || 0);
    if (gain > maxGain) {
      maxGain = gain;
    }
  }
  const gainNormalizer = maxGain > 0 ? maxGain : 1;

  for (let index = 0; index < sampleCount; index += 1) {
    const base = index * 4;
    const omni = {
      x: samples[base],
      y: samples[base + 1],
      z: samples[base + 2],
    };
    const gain = Math.max(0, Number(samples[base + 3]) || 0);
    const normalizedGain = gain / gainNormalizer;
    const scenePos = normalizedOmniphonyToScenePosition(omni);
    position.set(scenePos.x, scenePos.y, scenePos.z);
    const maxSphereSize = Math.max(VOLUME_BASE_SCALE, Number(app.speakerHeatmapMaxSphereSize) || 0.062);
    const radius = VOLUME_BASE_SCALE + Math.sqrt(normalizedGain) * (maxSphereSize - VOLUME_BASE_SCALE);
    scale.setScalar(radius);
    matrix.compose(position, quaternion, scale);
    mesh.setMatrixAt(index, matrix);
    mesh.setColorAt(index, heatmapColor(normalizedGain));
  }
  mesh.instanceMatrix.needsUpdate = true;
  if (mesh.instanceColor) {
    mesh.instanceColor.needsUpdate = true;
  }
  return mesh;
}

function renderVolume(samples) {
  disposeVolumeMesh();
  volumeMesh = buildVolumeMesh(samples);
  volumeGroup.add(volumeMesh);
  volumeGroup.visible = true;
}

function applyHeatmapData() {
  if (!canShowHeatmap()) {
    hideAllHeatmap();
    return;
  }

  hideAllHeatmap();
  let hasVisibleContent = false;
  if (app.speakerHeatmapSlicesEnabled && heatmapState.slices.data) {
    renderSlices(heatmapState.slices.data);
    hasVisibleContent = true;
  }
  if (app.speakerHeatmapVolumeEnabled && heatmapState.volume.data) {
    renderVolume(heatmapState.volume.data);
    hasVisibleContent = true;
  }
  heatmapGroup.visible = hasVisibleContent;
}

function resetPending(kind, requestId) {
  if (kind === 'slices') {
    heatmapState.slices.pendingRequestId = requestId;
    heatmapState.slices.pendingMeta = null;
    heatmapState.slices.pendingSlices = new Map();
    return;
  }
  heatmapState.volume.pendingRequestId = requestId;
  heatmapState.volume.pendingMeta = null;
  heatmapState.volume.pendingVolumeChunks = new Map();
  heatmapState.volume.pendingVolumeChunkCount = null;
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

function normalizeVolumeChunkPayload(payload) {
  return {
    requestId: Number(payload?.request_id),
    speakerIndex: Number(payload?.speaker_index),
    chunkIndex: Number(payload?.chunk_index),
    chunkCount: Number(payload?.chunk_count),
    samples: Array.isArray(payload?.samples) ? payload.samples.map((value) => Number(value) || 0) : [],
  };
}

function maybeFinalizeSlices(requestId) {
  if (requestId !== heatmapState.slices.pendingRequestId || !heatmapState.slices.pendingMeta) {
    return;
  }
  if (!SLICE_PLANE_KEYS.every((key) => heatmapState.slices.pendingSlices.has(key))) {
    return;
  }
  heatmapState.slices.data = {
    xy: heatmapState.slices.pendingSlices.get('xy'),
    xz: heatmapState.slices.pendingSlices.get('xz'),
    yz: heatmapState.slices.pendingSlices.get('yz'),
  };
  applyHeatmapData();
}

function maybeFinalizeVolume(requestId) {
  if (requestId !== heatmapState.volume.pendingRequestId || !heatmapState.volume.pendingMeta) {
    return;
  }
  if (heatmapState.volume.pendingVolumeChunks.size === 0) {
    return;
  }
  const samples = [];
  const chunkIndices = Array.from(heatmapState.volume.pendingVolumeChunks.keys()).sort((a, b) => a - b);
  for (const index of chunkIndices) {
    const chunk = heatmapState.volume.pendingVolumeChunks.get(index) || [];
    samples.push(...chunk);
  }
  heatmapState.volume.data = samples;
  applyHeatmapData();
}

export function clearSpeakerHeatmap() {
  heatmapState.slices.pendingMeta = null;
  heatmapState.slices.pendingSlices.clear();
  heatmapState.slices.data = null;
  heatmapState.volume.pendingMeta = null;
  heatmapState.volume.pendingVolumeChunks.clear();
  heatmapState.volume.pendingVolumeChunkCount = null;
  heatmapState.volume.data = null;
  hideAllHeatmap();
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
  hideAllHeatmap();
  if (app.speakerHeatmapSlicesEnabled) {
    heatmapState.currentRequestId += 1;
    const requestId = heatmapState.currentRequestId;
    resetPending('slices', requestId);
    invoke('request_speaker_heatmap', {
      speakerIndex,
      requestId,
      mode: 'slices',
      maxSamples: app.speakerHeatmapSampleCount,
    }).catch(() => {
      if (requestId === heatmapState.slices.pendingRequestId) {
        heatmapState.slices.data = null;
        applyHeatmapData();
      }
    });
  } else {
    heatmapState.slices.data = null;
  }
  if (app.speakerHeatmapVolumeEnabled) {
    heatmapState.currentRequestId += 1;
    const requestId = heatmapState.currentRequestId;
    resetPending('volume', requestId);
    invoke('request_speaker_heatmap', {
      speakerIndex,
      requestId,
      mode: 'volume',
      maxSamples: app.speakerHeatmapSampleCount,
    }).catch(() => {
      if (requestId === heatmapState.volume.pendingRequestId) {
        heatmapState.volume.data = null;
        applyHeatmapData();
      }
    });
  } else {
    heatmapState.volume.data = null;
  }
}

export function handleSpeakerHeatmapMeta(payload) {
  const requestId = Number(payload?.request_id);
  if (!Number.isInteger(requestId)) {
    return;
  }
  if (requestId === heatmapState.slices.pendingRequestId) {
    heatmapState.slices.pendingMeta = payload;
    maybeFinalizeSlices(requestId);
  }
  if (requestId === heatmapState.volume.pendingRequestId) {
    heatmapState.volume.pendingMeta = payload;
    maybeFinalizeVolume(requestId);
  }
}

export function handleSpeakerHeatmapSlice(planeKey, payload) {
  const normalized = normalizeSlicePayload(payload);
  if (!Number.isInteger(normalized.requestId) || normalized.requestId !== heatmapState.slices.pendingRequestId) {
    return;
  }
  heatmapState.slices.pendingSlices.set(planeKey, normalized);
  maybeFinalizeSlices(normalized.requestId);
}

export function handleSpeakerHeatmapVolumeChunk(payload) {
  const normalized = normalizeVolumeChunkPayload(payload);
  if (!Number.isInteger(normalized.requestId) || normalized.requestId !== heatmapState.volume.pendingRequestId) {
    return;
  }
  heatmapState.volume.pendingVolumeChunkCount = Number.isInteger(normalized.chunkCount)
    ? normalized.chunkCount
    : 0;
  heatmapState.volume.pendingVolumeChunks.set(normalized.chunkIndex, normalized.samples);
  maybeFinalizeVolume(normalized.requestId);
}

export function handleSpeakerHeatmapUnavailable(payload) {
  const requestId = Number(payload?.request_id);
  if (!Number.isInteger(requestId)) {
    return;
  }
  if (requestId === heatmapState.slices.pendingRequestId) {
    heatmapState.slices.data = null;
  }
  if (requestId === heatmapState.volume.pendingRequestId) {
    heatmapState.volume.data = null;
  }
  applyHeatmapData();
}
