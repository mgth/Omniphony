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
  headPoseGroup,
  BRASSEMPOUY_TARGET_MAX_DIMENSION, brassempouyAssetUrl
} from './scene/setup.js';
import { updateHeadPose } from './scene/head-pose.js';
import './scene/axes.js';
import { refreshObjectEnergyVolume } from './scene/object-energy-volume.js';
import { refreshSpeakerSoloVolume } from './scene/speaker-solo-volume.js';
import { refreshGlobalEnergyVolume } from './scene/global-energy-volume.js';
import { refreshDiscontinuityVolume } from './scene/discontinuity-volume.js';

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
  loadTrailPrefs, loadEffectiveRenderPrefs, refreshRoomGeometryInputState, setRoomGeometryExpanded,
  renderRoomRatioDisplay, refreshEffectiveRenderVisibility, updateRoomDimensionGuides, applyRoomRatio
} from './controls/room-geometry.js';

// ── Modals ──────────────────────────────────────────────────────────────────
import {
  setTelemetryGaugesOpen,
  setAudioOutputSectionOpen,
  setInputSectionOpen,
  setRendererSectionOpen,
  setDisplaySectionOpen,
  setDrcSectionOpen,
  setTwoDSourcesSectionOpen,
  setAutoGainSectionOpen
} from './modals.js';

// ── Initialization & wiring ─────────────────────────────────────────────────
import { applyInitState } from './init.js';
import { initMpvOverlay } from './mpvOverlay.js';
import { initSceneEffectsBar } from './controls/scene-effects-bar.js';
import { setupTauriBridge } from './tauri-bridge.js';
import { setupUIListeners } from './setup-listeners.js';
import { setupPointerListeners } from './picking.js';
import { setupNumericWheelEditing } from './input.js';
import { flushUI, flushCallbacks } from './flush.js';
import { setupVisualRecovery, teardownVisualRecovery } from './visual-recovery.js';
import { initRenderSurfaceController } from './core/render/render-surface-controller.js';

// ── Flush callback wiring ──────────────────────────────────────────────────
import {
  renderVbapStatus,
  renderEvaluationMode,
  renderHybridOptions,
  renderRenderBackend,
  renderVbapCartesian,
  renderVbapPolar
} from './controls/vbap.js';
import { renderLoudnessDisplay, renderDistanceModelUI, renderMasterGainUI, renderAutoGainUI, renderAutoGainCeilingUI, updateMasterMeterUI } from './controls/master.js';
import { renderAdaptiveResamplingUI } from './controls/adaptive.js';
import { renderDistanceDiffuseUI } from './controls/distance-diffuse.js';
import { renderConfigSavedUI } from './controls/config.js';
import { renderLatencyDisplay, renderLatencyMeterUI, renderRenderTimeUI, renderResampleRatioDisplay } from './controls/latency.js';
import { renderAudioFormatDisplay, applyAudioSampleRateNow } from './controls/audio.js';
import { bindDrcListeners, renderDrcUI } from './controls/drc.js';
import { initBinauralPanel } from './controls/binaural.js';
import { initUpdateCheck, maybeCheck } from './controls/updates.js';
import {
  updateObjectContributionUI,
  updateSpeakerContributionUI,
  getObjectDisplayName,
  refreshEffectiveRenderDecorations,
  sourceCallbacks,
  setSelectedSource,
  enforceObjectsVisibilityIfHidden
} from './sources.js';
import { updateVbapCartesianFaceGrid, renderVbapCartesianGridToggle } from './scene/gizmos.js';
import { updateObjectMeterUI, updateObjectPositionUI, updateObjectSizeUI, updateObjectLabelUI } from './flush.js';
import {
  renderObjectsList, updateSpeakerControlsUI, updateObjectControlsUI, updateObjectDominantSpeakerUI,
  objectHasActiveTrail, getObjectIds, updateSectionProportions, updateAllSpeakerBandBars,
  updateSpeakerGizmo, applyObjectPositionIcon, applyObjectIdentity
} from './speakers.js';
import { rebuildTrailGeometry, captureTrailPointColor } from './trails.js';
import { muteSoloCallbacks } from './mute-solo.js';
import { installMemoryDiagnostics } from './debug-memory.js';

// Memory diagnostics: registers window.omniphonyDebug.memory and samples only
// when opted in via localStorage 'spatialviz.memory_sampler' (see debug-memory.js).
installMemoryDiagnostics();

flushCallbacks.renderRoomRatioDisplay = renderRoomRatioDisplay;
flushCallbacks.renderEvaluationMode = renderEvaluationMode;
flushCallbacks.renderRenderBackend = renderRenderBackend;
flushCallbacks.renderHybridOptions = renderHybridOptions;
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
flushCallbacks.renderDrcUI = renderDrcUI;
flushCallbacks.renderMasterGainUI = renderMasterGainUI;
flushCallbacks.renderAutoGainUI = renderAutoGainUI;
flushCallbacks.renderAutoGainCeilingUI = renderAutoGainCeilingUI;
flushCallbacks.updateMasterMeterUI = updateMasterMeterUI;
flushCallbacks.updateObjectContributionUI = updateObjectContributionUI;
flushCallbacks.updateObjectPositionIcon = applyObjectPositionIcon;
flushCallbacks.updateSpeakerContributionUI = updateSpeakerContributionUI;
flushCallbacks.getObjectDisplayName = getObjectDisplayName;
flushCallbacks.applyObjectIdentity = applyObjectIdentity;
flushCallbacks.applyAudioSampleRateNow = applyAudioSampleRateNow;
flushCallbacks.refreshEffectiveRenderVisibility = refreshEffectiveRenderDecorations;
flushCallbacks.updateVbapCartesianFaceGrid = updateVbapCartesianFaceGrid;
flushCallbacks.renderVbapCartesianGridToggle = renderVbapCartesianGridToggle;
flushCallbacks.applyRoomRatio = applyRoomRatio;
flushCallbacks.updateRoomDimensionGuides = updateRoomDimensionGuides;

// ── Source callbacks wiring ─────────────────────────────────────────────────
sourceCallbacks.renderObjectsList = renderObjectsList;
sourceCallbacks.updateObjectPositionUI = updateObjectPositionUI;
sourceCallbacks.updateObjectSizeUI = updateObjectSizeUI;
sourceCallbacks.updateObjectLabelUI = updateObjectLabelUI;
sourceCallbacks.updateObjectMeterUI = updateObjectMeterUI;
sourceCallbacks.updateObjectDominantSpeakerUI = updateObjectDominantSpeakerUI;
sourceCallbacks.updateObjectControlsUI = updateObjectControlsUI;
sourceCallbacks.updateSectionProportions = updateSectionProportions;
sourceCallbacks.rebuildTrailGeometry = rebuildTrailGeometry;
sourceCallbacks.captureTrailPointColor = captureTrailPointColor;
sourceCallbacks.objectHasActiveTrail = objectHasActiveTrail;
sourceCallbacks.getObjectIds = getObjectIds;
sourceCallbacks.updateAllSpeakerBandBars = updateAllSpeakerBandBars;
sourceCallbacks.refreshEditGizmo = updateSpeakerGizmo;

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
  renderAutoGainUI();
  renderAutoGainCeilingUI();
  updateMasterMeterUI();
  renderSpeakersList();
  renderObjectsList();
  renderConfigSavedUI();
});

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
    headPoseGroup.add(model);
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

loadTrailPrefs();
loadEffectiveRenderPrefs();
bindDrcListeners();
initBinauralPanel();
refreshRoomGeometryInputState();
setRoomGeometryExpanded(false);
setTelemetryGaugesOpen(false);
setAudioOutputSectionOpen(false);
setInputSectionOpen(false);
setRendererSectionOpen(false);
setDisplaySectionOpen(false);
setDrcSectionOpen(false);
setTwoDSourcesSectionOpen(false);
setAutoGainSectionOpen(false);

// Register UI event listeners
initRenderSurfaceController({
  getRenderer: () => renderer,
  getCamera: () => camera
});
setupUIListeners();
setupPointerListeners();
setupNumericWheelEditing();
setupVisualRecovery();

// Reconnect to mpv overlay if the user had it enabled in a previous session.
initMpvOverlay();

// Floating quick-toggle bar over the 3D view (mirrors the display checkboxes).
// After setupUIListeners() so the dispatched `change` reaches their handlers.
initSceneEffectsBar();

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

initUpdateCheck();

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
    maybeCheck(info.version);
  })
  .catch((e) => {
    console.error('[get_about_info]', e);
  });

// Which orender binary this Studio would launch. Compared against the path the
// connected renderer reports so a foreign one is called out instead of silently
// swallowing controls it does not implement.
invoke('expected_orender_path')
  .then((path) => {
    app.expectedOrenderPath = typeof path === 'string' ? path.trim() || null : null;
    renderOscStatus();
  })
  .catch((e) => {
    console.error('[expected_orender_path]', e);
  });

// ── Animation loop ──────────────────────────────────────────────────────────
let animationFrameId = 0;

function animate() {
  animationFrameId = requestAnimationFrame(animate);
  controls.update();
  updateHeadPose();
  updateRoomFaceVisibility();
  updateSelectedSpeakerFaceShadows();
  updateSelectedObjectFaceShadows();
  const now = performance.now();
  decayTrails(now);
  decayMeters(now);
  refreshObjectEnergyVolume(now);
  refreshSpeakerSoloVolume(now);
  refreshGlobalEnergyVolume(now);
  refreshDiscontinuityVolume(now);
  enforceObjectsVisibilityIfHidden();

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

if (import.meta.hot) {
  import.meta.hot.on('vite:beforeUpdate', ({ updates }) => {
    if (updates.some((update) => update.type === 'js-update')) {
      window.location.reload();
    }
  });

  import.meta.hot.dispose(() => {
    if (animationFrameId) {
      cancelAnimationFrame(animationFrameId);
      animationFrameId = 0;
    }
    teardownVisualRecovery();
  });
}
