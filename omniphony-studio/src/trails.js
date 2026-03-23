import * as THREE from 'three';
import { app, sourceTrails, sourcePositionsRaw, sourceMeshes, speakerMeshes, objectItems } from './state.js';
import { normalizedOmniphonyToScenePosition, mapRoomPosition, omniphonyToSceneCartesian, hydrateObjectCoordinateState } from './coordinates.js';

// ── Module-level trail state ──────────────────────────────────────────
let trailRenderMode = 'diffuse';
let trailPointTtlMs = 7000;
let lastTrailDecayAt = 0;

// Fallback colour used when a source mesh has no material colour.
const SOURCE_FALLBACK_COLOR = new THREE.Color(0xcc6640);
// Mirror of sourceMaterial.color for captureTrailPointColor fallback.
const SOURCE_MATERIAL_COLOR = new THREE.Color(0xff7c4d);

// ── Renderable constructors ───────────────────────────────────────────

export function createDiffuseTrailRenderable() {
  const canvas = document.createElement('canvas');
  canvas.width = 64;
  canvas.height = 64;
  const ctx = canvas.getContext('2d');
  const gradient = ctx.createRadialGradient(32, 32, 4, 32, 32, 32);
  gradient.addColorStop(0.0, 'rgba(255,255,255,1.0)');
  gradient.addColorStop(0.35, 'rgba(255,255,255,0.65)');
  gradient.addColorStop(1.0, 'rgba(255,255,255,0.0)');
  ctx.clearRect(0, 0, 64, 64);
  ctx.fillStyle = gradient;
  ctx.fillRect(0, 0, 64, 64);

  const texture = new THREE.CanvasTexture(canvas);
  texture.minFilter = THREE.LinearFilter;
  texture.magFilter = THREE.LinearFilter;

  const material = new THREE.ShaderMaterial({
    transparent: true,
    depthTest: false,
    depthWrite: false,
    blending: THREE.NormalBlending,
    uniforms: {
      pointTexture: { value: texture }
    },
    vertexShader: `
      attribute vec3 color;
      attribute float size;
      attribute float alpha;
      varying vec3 vColor;
      varying float vAlpha;

      void main() {
        vColor = color;
        vAlpha = alpha;
        vec4 mvPosition = modelViewMatrix * vec4(position, 1.0);
        gl_PointSize = clamp(size * (110.0 / max(0.1, -mvPosition.z)), 0.4, 44.0);
        gl_Position = projectionMatrix * mvPosition;
      }
    `,
    fragmentShader: `
      uniform sampler2D pointTexture;
      varying vec3 vColor;
      varying float vAlpha;

      void main() {
        vec4 tex = texture2D(pointTexture, gl_PointCoord);
        float alpha = tex.a * vAlpha;
        if (alpha <= 0.001) discard;
        gl_FragColor = vec4(vColor, alpha);
      }
    `
  });

  const points = new THREE.Points(new THREE.BufferGeometry(), material);
  points.renderOrder = 15;
  points.frustumCulled = false;
  return points;
}

export function createLineTrailRenderable() {
  const material = new THREE.LineBasicMaterial({
    vertexColors: true,
    transparent: true,
    opacity: 0.6,
    depthTest: false,
    depthWrite: false
  });
  const line = new THREE.Line(new THREE.BufferGeometry(), material);
  line.renderOrder = 15;
  line.frustumCulled = false;
  return line;
}

export function createTrailRenderable() {
  return trailRenderMode === 'line'
    ? createLineTrailRenderable()
    : createDiffuseTrailRenderable();
}

// ── Position / colour helpers ─────────────────────────────────────────

export function mapTrailRawToScene(raw) {
  if (raw.directSpeakerIndex !== null && raw.directSpeakerIndex !== undefined) {
    const speakerMesh = speakerMeshes[raw.directSpeakerIndex];
    if (speakerMesh) {
      return speakerMesh.position.clone();
    }
  }
  const hydrated = hydrateObjectCoordinateState({ ...raw });
  const scene = normalizedOmniphonyToScenePosition(hydrated);
  return new THREE.Vector3(scene.x, scene.y, scene.z);
}

export function trailPointColorFromRaw(raw, fallbackColor) {
  const rgb = Array.isArray(raw?.trailColor) ? raw.trailColor : null;
  if (rgb && rgb.length >= 3) {
    return new THREE.Color(
      Math.min(1, Math.max(0, Number(rgb[0]) || 0)),
      Math.min(1, Math.max(0, Number(rgb[1]) || 0)),
      Math.min(1, Math.max(0, Number(rgb[2]) || 0))
    );
  }
  return fallbackColor.clone();
}

export function captureTrailPointColor(mesh) {
  const color = mesh?.material?.color;
  if (!color) {
    return [SOURCE_MATERIAL_COLOR.r, SOURCE_MATERIAL_COLOR.g, SOURCE_MATERIAL_COLOR.b];
  }
  return [color.r, color.g, color.b];
}

// ── Geometry rebuilders ───────────────────────────────────────────────

export function rebuildLineTrailGeometry(trail, mappedPositions, pointColors) {
  const positions = new Float32Array(mappedPositions.length * 3);
  const colors = new Float32Array(mappedPositions.length * 3);
  for (let i = 0; i < mappedPositions.length; i++) {
    const point = mappedPositions[i];
    const t = mappedPositions.length > 1 ? i / (mappedPositions.length - 1) : 1;
    const color = pointColors[i];
    positions[i * 3] = point.x;
    positions[i * 3 + 1] = point.y;
    positions[i * 3 + 2] = point.z;
    colors[i * 3] = color.r * (0.2 + 0.8 * t);
    colors[i * 3 + 1] = color.g * (0.2 + 0.8 * t);
    colors[i * 3 + 2] = color.b * (0.2 + 0.8 * t);
  }
  trail.line.geometry.dispose();
  const geometry = new THREE.BufferGeometry();
  geometry.setAttribute('position', new THREE.BufferAttribute(positions, 3));
  geometry.setAttribute('color', new THREE.BufferAttribute(colors, 3));
  trail.line.geometry = geometry;
}

export function rebuildDiffuseTrailGeometry(trail, mappedPositions, pointColors, sourceScale) {
  const count = mappedPositions.length;
  const loudnessFactor = Math.pow(sourceScale, 1.8);

  const expanded = [];
  for (let i = 0; i < mappedPositions.length; i++) {
    const current = mappedPositions[i];
    const currentColor = pointColors[i];
    const baseT = count > 1 ? i / (count - 1) : 1;
    expanded.push({ position: current, color: currentColor, t: baseT });
    if (i >= mappedPositions.length - 1) {
      continue;
    }
    const next = mappedPositions[i + 1];
    const nextColor = pointColors[i + 1];
    const distance = current.distanceTo(next);
    const subdivisions = Math.max(2, Math.min(10, Math.ceil(distance / 0.06)));
    for (let step = 1; step < subdivisions; step += 1) {
      const localT = step / subdivisions;
      expanded.push({
        position: current.clone().lerp(next, localT),
        color: currentColor.clone().lerp(nextColor, localT),
        t: (i + localT) / (count - 1)
      });
    }
  }

  const positions = new Float32Array(expanded.length * 3);
  const colors = new Float32Array(expanded.length * 3);
  const sizes = new Float32Array(expanded.length);
  const alphas = new Float32Array(expanded.length);
  for (let i = 0; i < expanded.length; i++) {
    const point = expanded[i];
    const color = point.color;
    positions[i * 3] = point.position.x;
    positions[i * 3 + 1] = point.position.y;
    positions[i * 3 + 2] = point.position.z;
    const t = point.t;
    const glow = 0.22 + (0.78 * t);
    colors[i * 3] = (color.r * 0.35 + 0.18) * glow;
    colors[i * 3 + 1] = (color.g * 0.65 + 0.45) * glow;
    colors[i * 3 + 2] = (color.b * 0.85 + 0.95) * glow;
    sizes[i] = (6 + (20 * t)) * loudnessFactor;
    alphas[i] = 0.05 + (0.2 * t * t);
  }
  trail.line.geometry.dispose();
  const geometry = new THREE.BufferGeometry();
  geometry.setAttribute('position', new THREE.BufferAttribute(positions, 3));
  geometry.setAttribute('color', new THREE.BufferAttribute(colors, 3));
  geometry.setAttribute('size', new THREE.BufferAttribute(sizes, 1));
  geometry.setAttribute('alpha', new THREE.BufferAttribute(alphas, 1));
  trail.line.geometry = geometry;
}

export function rebuildTrailGeometry(id) {
  const trail = sourceTrails.get(id);
  if (!trail) return;
  const count = trail.positions.length;
  if (count < 2) {
    trail.line.geometry.dispose();
    trail.line.geometry = new THREE.BufferGeometry();
    return;
  }
  const mesh = sourceMeshes.get(id);
  const fallbackColor = mesh ? mesh.material.color.clone() : new THREE.Color(0xcc6640);
  const sourceScale = Math.max(0.0, Number(mesh?.scale.x) || 0.0);
  const mappedPositions = trail.positions.map((raw) => mapTrailRawToScene(raw));
  const pointColors = trail.positions.map((raw) => trailPointColorFromRaw(raw, fallbackColor));
  if (trailRenderMode === 'line') {
    rebuildLineTrailGeometry(trail, mappedPositions, pointColors);
    return;
  }
  rebuildDiffuseTrailGeometry(trail, mappedPositions, pointColors, sourceScale);
}

// ── Trail decay ───────────────────────────────────────────────────────

export function decayTrails(nowMs) {
  // Decay trails a few times per second; no need to run every frame.
  if (nowMs - lastTrailDecayAt < 120) return;
  lastTrailDecayAt = nowMs;

  const cutoff = nowMs - trailPointTtlMs;
  sourceTrails.forEach((trail, id) => {
    const before = trail.positions.length;
    if (before === 0) return;

    // Keep points with recent timestamps. Legacy points without timestamp are
    // treated as stale and dropped on first decay pass.
    trail.positions = trail.positions.filter((p) => typeof p.t === 'number' && p.t >= cutoff);
    if (trail.positions.length !== before) {
      rebuildTrailGeometry(id);
      const entry = objectItems.get(String(id));
      if (entry) {
        entry.root.classList.toggle('has-active-trail', trail.positions.length > 0);
      }
    }
  });
}

// ── Accessors for module-level trail settings ─────────────────────────

export function getTrailRenderMode() {
  return trailRenderMode;
}

export function setTrailRenderMode(mode) {
  trailRenderMode = mode;
}

export function getTrailPointTtlMs() {
  return trailPointTtlMs;
}

export function setTrailPointTtlMs(ms) {
  trailPointTtlMs = ms;
}
