import { app, sourceLabels, speakerLabels } from './state.js';
import { pushLog } from './log.js';
import { camera, renderer, scene, rebuildRendererOnFreshCanvas, teardownRenderer } from './scene/setup.js';
import { rebuildLabelSpriteTexture } from './scene/labels.js';
import { rebuildAllTrailRenderables } from './trails.js';
import { rebuildRoomDimensionGuideResources, updateRoomDimensionGuides } from './controls/room-geometry.js';
import { rebindPointerListeners } from './picking.js';

let recoveryCanvas = null;
let scheduledRecoveryTimer = null;
const visualRecoveryStats = {
  rebuilds: 0,
  reasons: {},
  recentReasons: []
};

function attachVisualRecoveryDebugHandle() {
  if (typeof window === 'undefined') {
    return;
  }
  const existing = window.omniphonyDebug && typeof window.omniphonyDebug === 'object'
    ? window.omniphonyDebug
    : {};
  window.omniphonyDebug = {
    ...existing,
    visualRecoveryStats
  };
}

attachVisualRecoveryDebugHandle();

function onContextLost(event) {
  event.preventDefault();
  app.webglContextLossCount = (Number(app.webglContextLossCount) || 0) + 1;
  pushLog('warn', `WebGL context lost (#${app.webglContextLossCount}).`);
}

function onContextRestored() {
  pushLog('warn', 'WebGL context restored. Rebuilding WebGL renderer and visual resources.');
  requestAnimationFrame(() => {
    rebuildRenderer('webglcontextrestored');
  });
}

function bindRecoveryListeners() {
  const canvas = renderer.domElement;
  if (recoveryCanvas === canvas) {
    return;
  }
  if (recoveryCanvas) {
    recoveryCanvas.removeEventListener('webglcontextlost', onContextLost);
    recoveryCanvas.removeEventListener('webglcontextrestored', onContextRestored);
  }
  canvas.addEventListener('webglcontextlost', onContextLost);
  canvas.addEventListener('webglcontextrestored', onContextRestored);
  recoveryCanvas = canvas;
}

function rendererContextIsLost() {
  try {
    const gl = renderer.getContext();
    return Boolean(gl?.isContextLost?.());
  } catch (_error) {
    return true;
  }
}

function flagMaterial(material) {
  if (!material) {
    return;
  }
  material.needsUpdate = true;
  if (material.map) {
    material.map.needsUpdate = true;
  }
}

function flagObjectResources(object) {
  if (object.geometry) {
    Object.values(object.geometry.attributes || {}).forEach((attribute) => {
      if (attribute) {
        attribute.needsUpdate = true;
      }
    });
  }
  if (Array.isArray(object.material)) {
    object.material.forEach(flagMaterial);
    return;
  }
  flagMaterial(object.material);
}

function onOverlayLayoutChanged(event) {
  const reason = typeof event?.detail?.reason === 'string' && event.detail.reason
    ? event.detail.reason
    : 'overlay-layout-change';
  scheduleVisualRecovery(reason);
}

export function scheduleVisualRecovery(reason = 'manual') {
  if (scheduledRecoveryTimer !== null) {
    window.clearTimeout(scheduledRecoveryTimer);
  }
  scheduledRecoveryTimer = window.setTimeout(() => {
    scheduledRecoveryTimer = null;
    rebuildVisualResources(reason);
  }, 120);
}

export function rebuildVisualResources(reason = 'manual') {
  visualRecoveryStats.rebuilds += 1;
  visualRecoveryStats.reasons[reason] = (visualRecoveryStats.reasons[reason] || 0) + 1;
  visualRecoveryStats.recentReasons.push({ t: Date.now(), reason });
  if (visualRecoveryStats.recentReasons.length > 40) {
    visualRecoveryStats.recentReasons.shift();
  }
  if (rendererContextIsLost()) {
    pushLog('warn', `Skipped visual rebuild (${reason}) because the WebGL context is currently lost.`);
    return;
  }
  sourceLabels.forEach((label) => {
    rebuildLabelSpriteTexture(label);
  });
  speakerLabels.forEach((label) => {
    if (label) {
      rebuildLabelSpriteTexture(label);
    }
  });
  rebuildRoomDimensionGuideResources();
  updateRoomDimensionGuides();
  rebuildAllTrailRenderables();
  scene.traverse(flagObjectResources);
  renderer.resetState();
  try {
    renderer.compile(scene, camera);
  } catch (error) {
    pushLog('warn', `Renderer compile skipped during visual rebuild (${reason}): ${error instanceof Error ? error.message : String(error)}`);
  }
  pushLog('warn', `Visual resources rebuilt (${reason}).`);
}

export function setupVisualRecovery() {
  bindRecoveryListeners();
  if (typeof window !== 'undefined') {
    window.addEventListener('omniphony:overlay-layout-changed', onOverlayLayoutChanged);
  }
  if (typeof window !== 'undefined') {
    const existing = window.omniphonyDebug && typeof window.omniphonyDebug === 'object'
      ? window.omniphonyDebug
      : {};
    window.omniphonyDebug = {
      ...existing,
      rebuildVisualResources,
      rebuildRenderer,
      scheduleVisualRecovery
    };
  }
}

export function rebuildRenderer(reason = 'renderer-rebuild') {
  try {
    rebuildRendererOnFreshCanvas();
    rebindPointerListeners();
    bindRecoveryListeners();
  } catch (error) {
    pushLog('error', `Renderer rebuild failed: ${error instanceof Error ? error.message : String(error)}`);
    return;
  }
  rebuildVisualResources(reason);
  pushLog('warn', 'WebGL renderer rebuilt.');
}

export function teardownVisualRecovery() {
  if (scheduledRecoveryTimer !== null) {
    window.clearTimeout(scheduledRecoveryTimer);
    scheduledRecoveryTimer = null;
  }
  if (typeof window !== 'undefined') {
    window.removeEventListener('omniphony:overlay-layout-changed', onOverlayLayoutChanged);
  }
  if (recoveryCanvas) {
    recoveryCanvas.removeEventListener('webglcontextlost', onContextLost);
    recoveryCanvas.removeEventListener('webglcontextrestored', onContextRestored);
    recoveryCanvas = null;
  }
  teardownRenderer(true);
}
