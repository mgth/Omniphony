/**
 * Omniphony Studio — application entry point.
 *
 * This module orchestrates the boot sequence and animation loop.
 * All domain logic lives in dedicated modules.
 */

import * as THREE from 'three';
import { GLTFLoader } from 'three/examples/jsm/loaders/GLTFLoader.js';
import { invoke } from '@tauri-apps/api/core';

// ── Shared state ────────────────────────────────────────────────────────────
import { app, sourceOutlines } from './state.js';

// ── i18n & logging ──────────────────────────────────────────────────────────
import { t, tf, applyStaticTranslations, onLocaleChange } from './i18n.js';
import { pushLog, renderLogLevelControl, renderLogPanel, normalizeLogError } from './log.js';

// ── Scene ───────────────────────────────────────────────────────────────────
import {
  scene, camera, renderer, controls,
  brassempouyAnchor,
  BRASSEMPOUY_TARGET_MAX_DIMENSION, brassempouyAssetUrl
} from './scene/setup.js';
import './scene/axes.js';

// ── Domain modules (imported for side-effects & to register into state) ─────
import {
  updateRoomFaceVisibility,
  updateSelectedSpeakerFaceShadows,
  updateSelectedObjectFaceShadows
} from './speakers.js';
import { decayTrails } from './trails.js';
import { decayMeters } from './speakers.js';

// ── Controls ────────────────────────────────────────────────────────────────
import { setOscStatus, loadOscConfigIntoPanel, renderOscStatus } from './controls/osc.js';
import {
  loadRoomGeometryPrefs, refreshRoomGeometryInputState, setRoomGeometryExpanded,
  renderRoomRatioDisplay, refreshEffectiveRenderVisibility, updateRoomDimensionGuides, applyRoomRatio
} from './controls/room-geometry.js';

// ── Modals ──────────────────────────────────────────────────────────────────
import {
  setTelemetryGaugesOpen,
  setAudioOutputSectionOpen,
  setInputSectionOpen,
  setRendererSectionOpen,
  setDisplaySectionOpen
} from './modals.js';

// ── Initialization & wiring ─────────────────────────────────────────────────
import { applyInitState } from './init.js';
import { setupTauriBridge } from './tauri-bridge.js';
import { setupUIListeners } from './setup-listeners.js';
import { setupPointerListeners } from './picking.js';
import { setupNumericWheelEditing } from './input.js';
import { flushUI, flushCallbacks } from './flush.js';
import { setupVisualRecovery } from './visual-recovery.js';

// ── Flush callback wiring ──────────────────────────────────────────────────
import { renderSpreadDisplay } from './controls/spread.js';
import {
  renderVbapStatus,
  renderEvaluationMode,
  renderRenderBackend,
  renderVbapCartesian,
  renderVbapPolar
} from './controls/vbap.js';
import { renderLoudnessDisplay, renderDistanceModelUI, renderMasterGainUI, updateMasterMeterUI } from './controls/master.js';
import { renderAdaptiveResamplingUI } from './controls/adaptive.js';
import { renderDistanceDiffuseUI } from './controls/distance-diffuse.js';
import { renderConfigSavedUI } from './controls/config.js';
import { renderLatencyDisplay, renderLatencyMeterUI, renderRenderTimeUI, renderResampleRatioDisplay } from './controls/latency.js';
import { renderAudioFormatDisplay, applyAudioSampleRateNow } from './controls/audio.js';
import {
  updateObjectContributionUI,
  updateSpeakerContributionUI,
  getObjectDisplayName,
  refreshEffectiveRenderDecorations,
  sourceCallbacks,
  setSelectedSource
} from './sources.js';
import { updateVbapCartesianFaceGrid, renderVbapCartesianGridToggle } from './scene/gizmos.js';
import { updateObjectMeterUI, updateObjectPositionUI, updateObjectLabelUI } from './flush.js';
import {
  renderObjectsList, updateSpeakerControlsUI, updateObjectControlsUI, updateObjectDominantSpeakerUI,
  objectHasActiveTrail, getObjectIds, updateSectionProportions
} from './speakers.js';
import { rebuildTrailGeometry, captureTrailPointColor } from './trails.js';
import { muteSoloCallbacks } from './mute-solo.js';

flushCallbacks.renderRoomRatioDisplay = renderRoomRatioDisplay;
flushCallbacks.renderSpreadDisplay = renderSpreadDisplay;
flushCallbacks.renderEvaluationMode = renderEvaluationMode;
flushCallbacks.renderRenderBackend = renderRenderBackend;
flushCallbacks.renderVbapCartesian = renderVbapCartesian;
flushCallbacks.renderVbapPolar = renderVbapPolar;
flushCallbacks.renderLoudnessDisplay = renderLoudnessDisplay;
flushCallbacks.renderAdaptiveResamplingUI = renderAdaptiveResamplingUI;
flushCallbacks.renderDistanceDiffuseUI = renderDistanceDiffuseUI;
flushCallbacks.renderDistanceModelUI = renderDistanceModelUI;
flushCallbacks.renderConfigSavedUI = renderConfigSavedUI;
flushCallbacks.renderLatencyDisplay = renderLatencyDisplay;
flushCallbacks.renderLatencyMeterUI = renderLatencyMeterUI;
flushCallbacks.renderRenderTimeUI = renderRenderTimeUI;
flushCallbacks.renderResampleRatioDisplay = renderResampleRatioDisplay;
flushCallbacks.renderAudioFormatDisplay = renderAudioFormatDisplay;
flushCallbacks.renderMasterGainUI = renderMasterGainUI;
flushCallbacks.updateMasterMeterUI = updateMasterMeterUI;
flushCallbacks.updateObjectContributionUI = updateObjectContributionUI;
flushCallbacks.updateSpeakerContributionUI = updateSpeakerContributionUI;
flushCallbacks.getObjectDisplayName = getObjectDisplayName;
flushCallbacks.applyAudioSampleRateNow = applyAudioSampleRateNow;
flushCallbacks.refreshEffectiveRenderVisibility = refreshEffectiveRenderDecorations;
flushCallbacks.updateVbapCartesianFaceGrid = updateVbapCartesianFaceGrid;
flushCallbacks.renderVbapCartesianGridToggle = renderVbapCartesianGridToggle;
flushCallbacks.applyRoomRatio = applyRoomRatio;
flushCallbacks.updateRoomDimensionGuides = updateRoomDimensionGuides;

// ── Source callbacks wiring ─────────────────────────────────────────────────
sourceCallbacks.renderObjectsList = renderObjectsList;
sourceCallbacks.updateObjectPositionUI = updateObjectPositionUI;
sourceCallbacks.updateObjectLabelUI = updateObjectLabelUI;
sourceCallbacks.updateObjectMeterUI = updateObjectMeterUI;
sourceCallbacks.updateObjectDominantSpeakerUI = updateObjectDominantSpeakerUI;
sourceCallbacks.updateObjectControlsUI = updateObjectControlsUI;
sourceCallbacks.updateSectionProportions = updateSectionProportions;
sourceCallbacks.rebuildTrailGeometry = rebuildTrailGeometry;
sourceCallbacks.captureTrailPointColor = captureTrailPointColor;
sourceCallbacks.objectHasActiveTrail = objectHasActiveTrail;
sourceCallbacks.getObjectIds = getObjectIds;

// ── Mute/solo callbacks wiring ──────────────────────────────────────────────
muteSoloCallbacks.updateSpeakerControlsUI = updateSpeakerControlsUI;
muteSoloCallbacks.updateObjectControlsUI = updateObjectControlsUI;
muteSoloCallbacks.setSelectedSource = setSelectedSource;

onLocaleChange(() => {
  renderOscStatus();
  renderRoomRatioDisplay();
  renderVbapStatus();
  renderEvaluationMode();
  renderRenderBackend();
  renderLoudnessDisplay();
  renderAdaptiveResamplingUI();
  renderDistanceDiffuseUI();
  renderLatencyDisplay();
  renderResampleRatioDisplay();
  renderAudioFormatDisplay();
  renderLatencyMeterUI();
  renderMasterGainUI();
  updateMasterMeterUI();
  renderSpeakersList();
  renderObjectsList();
  renderConfigSavedUI();
});

// ── Preferences ─────────────────────────────────────────────────────────────
const TRAIL_PREFS_STORAGE_KEY = 'spatialviz.trail_prefs';
const EFFECTIVE_RENDER_PREFS_STORAGE_KEY = 'spatialviz.effective_render_prefs';

function loadTrailPrefs() {
  try {
    const raw = localStorage.getItem(TRAIL_PREFS_STORAGE_KEY);
    if (!raw) return;
    const prefs = JSON.parse(raw);
    if (typeof prefs.enabled === 'boolean') app.trailsEnabled = prefs.enabled;
    if (prefs.mode === 'line' || prefs.mode === 'diffuse') app.trailRenderMode = prefs.mode;
    if (typeof prefs.ttlMs === 'number' && prefs.ttlMs >= 500) app.trailPointTtlMs = prefs.ttlMs;
  } catch (_e) { /* ignore */ }
}

function loadEffectiveRenderPrefs() {
  try {
    const raw = localStorage.getItem(EFFECTIVE_RENDER_PREFS_STORAGE_KEY);
    if (!raw) return;
    const prefs = JSON.parse(raw);
    if (typeof prefs.enabled === 'boolean') app.effectiveRenderEnabled = prefs.enabled;
    if (typeof prefs.objectColors === 'boolean') app.objectColorsEnabled = prefs.objectColors;
  } catch (_e) { /* ignore */ }
}

// ── GLTF model loading ──────────────────────────────────────────────────────
const gltfLoader = new GLTFLoader();
const brassempouyBounds = new THREE.Box3();
const brassempouySize = new THREE.Vector3();

gltfLoader.load(
  brassempouyAssetUrl.href,
  (gltf) => {
    const model = gltf.scene;
    model.traverse((node) => {
      if (!node.isMesh) return;
      node.castShadow = false;
      node.receiveShadow = false;
      node.frustumCulled = false;
      if (node.material && 'roughness' in node.material) {
        node.material.roughness = Math.min(0.92, Number(node.material.roughness) || 0.92);
      }
      if (node.material && 'metalness' in node.material) {
        node.material.metalness = 0.0;
      }
    });

    brassempouyBounds.setFromObject(model);
    brassempouyBounds.getSize(brassempouySize);
    const maxDimension = Math.max(brassempouySize.x, brassempouySize.y, brassempouySize.z);
    if (maxDimension > 0) {
      const scale = BRASSEMPOUY_TARGET_MAX_DIMENSION / maxDimension;
      model.scale.setScalar(scale);
      model.updateMatrixWorld(true);
      brassempouyBounds.setFromObject(model);
    }

    model.rotation.y = -Math.PI / 2;
    model.updateMatrixWorld(true);
    brassempouyAnchor.add(model);
  },
  undefined,
  (error) => {
    console.error('Failed to load la_dame_de_brassempouy.glb', error);
    pushLog('error', tf('log.modelLoadFailed', { error: normalizeLogError(error) }));
  }
);

// ── Boot sequence ───────────────────────────────────────────────────────────
applyStaticTranslations(renderLogLevelControl, renderLogPanel);
setOscStatus('initializing');
pushLog('info', t('log.boot'));

loadRoomGeometryPrefs();
loadTrailPrefs();
loadEffectiveRenderPrefs();
refreshRoomGeometryInputState();
setRoomGeometryExpanded(false);
setTelemetryGaugesOpen(false);
setAudioOutputSectionOpen(false);
setInputSectionOpen(false);
setRendererSectionOpen(false);
setDisplaySectionOpen(false);

// Register UI event listeners
setupUIListeners();
setupPointerListeners();
setupNumericWheelEditing();
setupVisualRecovery();

// Register Tauri backend event listeners
setupTauriBridge();

// Load persisted launch preferences before the live backend state arrives.
loadOscConfigIntoPanel();

// Fetch initial state from backend
invoke('get_state')
  .then((payload) => {
    if (app.oscSnapshotReady && payload && payload.oscSnapshotReady === false) {
      pushLog('debug', 'Ignoring stale initial state after live OSC snapshot');
      return;
    }
    applyInitState(payload);
    pushLog('info', t('log.stateLoaded'));
  })
  .catch((e) => {
    console.error('[get_state]', e);
    pushLog('error', tf('log.stateLoadFailed', { error: normalizeLogError(e) }));
  });

invoke('get_about_info')
  .then((info) => {
    const aboutNameEl = document.getElementById('aboutName');
    const aboutDescriptionEl = document.getElementById('aboutDescription');
    const aboutVersionEl = document.getElementById('aboutVersion');
    const aboutLicenseEl = document.getElementById('aboutLicense');
    const aboutRepositoryLinkEl = document.getElementById('aboutRepositoryLink');
    if (aboutNameEl) aboutNameEl.textContent = info.name || '';
    if (aboutDescriptionEl) aboutDescriptionEl.textContent = info.description || '';
    if (aboutVersionEl) aboutVersionEl.textContent = info.version || '';
    if (aboutLicenseEl) aboutLicenseEl.textContent = info.license || '';
    if (aboutRepositoryLinkEl && info.repository) {
      aboutRepositoryLinkEl.href = info.repository;
      aboutRepositoryLinkEl.textContent = info.repository;
    }
  })
  .catch((e) => {
    console.error('[get_about_info]', e);
  });

// ── Animation loop ──────────────────────────────────────────────────────────
function animate() {
  requestAnimationFrame(animate);
  controls.update();
  updateRoomFaceVisibility();
  updateSelectedSpeakerFaceShadows();
  updateSelectedObjectFaceShadows();
  const now = performance.now();
  decayTrails(now);
  decayMeters(now);

  sourceOutlines.forEach((outline) => {
    outline.quaternion.copy(camera.quaternion);
  });

  try {
    const gl = renderer.getContext?.();
    if (gl?.isContextLost?.()) {
      return;
    }
    renderer.render(scene, camera);
  } catch (error) {
    console.error('[renderer.render]', error);
  }
}

animate();

window.addEventListener('resize', () => {
  camera.aspect = window.innerWidth / window.innerHeight;
  camera.updateProjectionMatrix();
  renderer.setSize(window.innerWidth, window.innerHeight);
});
