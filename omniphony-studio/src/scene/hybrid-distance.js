/**
 * Translucent iso-distance shape for the hybrid blend curve.
 *
 * When a curve point is selected, this draws the surface at that point's blend
 * distance: a sphere for the spherical metric, a cube for Chebyshev. The hybrid
 * measures the blend distance on the *raw* ADM position, so the surface is a
 * perfect sphere/cube in ADM space.
 *
 * The room transform is separable per axis, so we deform every vertex through
 * the exact ADM→scene mapping (width/height linear, depth via the non-linear
 * depth warp) — this respects the warp curve and the asymmetric front/back
 * ratios instead of an ellipsoid approximation.
 */

import * as THREE from 'three';

import { app } from '../state.js';
import { depthWarpWithRatios } from '../coordinates.js';
import { scene } from './setup.js';

let mesh = null;
let currentShape = null;
// Undeformed unit-shape vertices, interpreted as ADM (omni) coordinates.
let baseAdm = null;
// Last requested shape, kept so room-ratio changes can re-deform it.
let lastSpec = null;

function ensureMesh(shape) {
  if (mesh && currentShape === shape) return;
  if (mesh) {
    scene.remove(mesh);
    mesh.geometry.dispose();
    mesh = null;
  }
  const geometry = shape === 'cube'
    ? new THREE.BoxGeometry(2, 2, 2)
    : new THREE.SphereGeometry(1, 40, 28);
  baseAdm = Float32Array.from(geometry.attributes.position.array);
  const material = new THREE.MeshBasicMaterial({
    color: 0xffd166,
    transparent: true,
    opacity: 0.14,
    depthWrite: false,
    side: THREE.DoubleSide
  });
  mesh = new THREE.Mesh(geometry, material);
  mesh.renderOrder = 3;
  mesh.frustumCulled = false;
  scene.add(mesh);
  currentShape = shape;
}

/**
 * Show/update the iso-distance shape.
 * @param {{shape:'sphere'|'cube', radius:number}|null} spec radius is the ADM
 *   distance (half-extent for the cube); `null` hides the shape.
 */
export function updateHybridDistanceShape(spec) {
  lastSpec = spec && spec.radius > 1e-4 ? spec : null;
  applyShape();
}

/** Re-deform the current shape with the latest room ratios (no spec change). */
export function redrawHybridDistanceShape() {
  applyShape();
}

function applyShape() {
  const spec = lastSpec;
  if (!spec) {
    if (mesh) mesh.visible = false;
    return;
  }
  ensureMesh(spec.shape === 'cube' ? 'cube' : 'sphere');
  mesh.visible = true;

  const r = spec.radius;
  const rr = app.roomRatio || {};
  const length = Number(rr.length) || 1;
  const rear = Number(rr.rear) || 1;
  const blend = Number.isFinite(rr.centerBlend) ? rr.centerBlend : 0.5;
  const height = Number(rr.height) || 1;
  const lower = Number(rr.lower) || 0.5;
  const width = Number(rr.width) || 1;

  // Deform each vertex through the exact ADM→scene room transform. ADM axes:
  // x = L/R, y = front/back, z = floor/ceiling. Scene axes: x = depth (omni y),
  // y = height (omni z), z = width (omni x).
  const positions = mesh.geometry.attributes.position;
  const out = positions.array;
  for (let i = 0; i < baseAdm.length; i += 3) {
    const omniX = baseAdm[i] * r;
    const omniY = baseAdm[i + 1] * r;
    const omniZ = baseAdm[i + 2] * r;
    out[i] = depthWarpWithRatios(omniY, length, rear, blend);
    out[i + 1] = omniZ >= 0 ? omniZ * height : omniZ * lower;
    out[i + 2] = omniX * width;
  }
  positions.needsUpdate = true;
}
