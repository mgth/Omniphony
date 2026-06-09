/**
 * Room geometry panel controls.
 *
 * Extracted from app.js (lines 3113-3660).
 */

import * as THREE from 'three';
import { app, dirty, isRoomRatioFrozen, layoutsByKey, sourceMeshes, sourcePositionsRaw, speakerMeshes, speakerLabels } from '../state.js';
import { formatNumber, normalizedOmniphonyToScenePosition, hydrateSpeakerCoordinateState } from '../coordinates.js';
import { scheduleUIFlush } from '../flush.js';
import { flushCallbacks } from '../flush.js';
import { invoke } from '@tauri-apps/api/core';
import { roomDimensionGroup, roomBounds, roomGroup, room, roomEdges, roomFaces, fitScreenToUpperHalf } from '../scene/setup.js';
import { updateVbapCartesianFaceGrid } from '../scene/gizmos.js';
import { redrawHybridDistanceShape } from '../scene/hybrid-distance.js';
import { updateSourceDecorations } from '../sources.js';
import { clampVolumeGamma, OBJECT_ENERGY_COLORMAPS, MAX_CUSTOM_STOPS } from '../scene/object-energy-shared.js';
import { rebuildTrailGeometry } from '../trails.js';
import { renderSpeakerEditor } from '../speakers.js';
import { emitOverlayLayoutChanged } from '../ui/layout/overlay-layout-state.js';
import { inDisplayPanel, inRoomGeometryPanel } from '../ui/panel-roots.js';
import { syncSpeakerHeatmapBandSelect } from '../scene/speaker-band-select.js';
import { createSmallLabelSprite, setLabelSpriteText } from '../scene/labels.js';

const TRAIL_PREFS_STORAGE_KEY = 'spatialviz.trail_prefs';
const EFFECTIVE_RENDER_PREFS_STORAGE_KEY = 'spatialviz.effective_render_prefs';

function getRoomGeometrySummaryEl() { return inRoomGeometryPanel('roomGeometrySummary'); }
function getRoomGeometryHeaderSummaryEl() { return inRoomGeometryPanel('roomGeometryHeaderSummary'); }
function getRoomGeometrySummaryScaleEl() { return inRoomGeometryPanel('roomGeometrySummaryScale'); }
function getRoomGeometrySummarySizeEl() { return inRoomGeometryPanel('roomGeometrySummarySize'); }
function getRoomGeometrySummaryRatioEl() { return inRoomGeometryPanel('roomGeometrySummaryRatio'); }
function getRoomDimWidthInputEl() { return inRoomGeometryPanel('roomDimWidthInput'); }
function getRoomDimLengthInputEl() { return inRoomGeometryPanel('roomDimLengthInput'); }
function getRoomDimHeightInputEl() { return inRoomGeometryPanel('roomDimHeightInput'); }
function getRoomDimRearInputEl() { return inRoomGeometryPanel('roomDimRearInput'); }
function getRoomDimLowerInputEl() { return inRoomGeometryPanel('roomDimLowerInput'); }
function getRoomRatioCenterBlendSliderEl() { return inRoomGeometryPanel('roomRatioCenterBlendSlider'); }
function getRoomRatioCenterBlendValueEl() { return inRoomGeometryPanel('roomRatioCenterBlendValue'); }
function getRoomGeometryCancelBtnEl() { return inRoomGeometryPanel('roomGeometryCancelBtn'); }
function getTrailToggleEl() { return inDisplayPanel('trailToggle'); }
function getTrailModeSelectEl() { return inDisplayPanel('trailModeSelect'); }
function getTrailTtlSliderEl() { return inDisplayPanel('trailTtlSlider'); }
function getTrailTtlValEl() { return inDisplayPanel('trailTtlVal'); }
function getTrailTeleportSliderEl() { return inDisplayPanel('trailTeleportSlider'); }
function getTrailTeleportValEl() { return inDisplayPanel('trailTeleportVal'); }
function getEffectiveRenderToggleEl() { return inDisplayPanel('effectiveRenderToggle'); }
function getShowObjectsToggleEl() { return inDisplayPanel('showObjectsToggle'); }
function getObjectColorsToggleEl() { return inDisplayPanel('objectColorsToggle'); }
function getObjectDisplayModeSelectEl() { return inDisplayPanel('objectDisplayModeSelect'); }
function getObjectSphereSizeSliderEl() { return inDisplayPanel('objectSphereSizeSlider'); }
function getObjectSphereSizeValEl() { return inDisplayPanel('objectSphereSizeVal'); }
function getObjectLabelsToggleEl() { return inDisplayPanel('objectLabelsToggle'); }
function getShowObjectDetailsToggleEl() { return inDisplayPanel('showObjectDetailsToggle'); }
function getSpeakerLabelsToggleEl() { return inDisplayPanel('speakerLabelsToggle'); }
function getSpeakerBandBarsToggleEl() { return inDisplayPanel('speakerBandBarsToggle'); }
function getSpeakerFaceListenerToggleEl() { return inDisplayPanel('speakerFaceListenerToggle'); }
function getSpeakerSizeSliderEl() { return inDisplayPanel('speakerSizeSlider'); }
function getSpeakerSizeValEl() { return inDisplayPanel('speakerSizeVal'); }
function getSpeakerHeatmapVolumeToggleEl() { return inDisplayPanel('speakerHeatmapVolumeToggle'); }
function getSpeakerHeatmapVolumeColormapEl() { return inDisplayPanel('speakerHeatmapVolumeColormap'); }
function getSpeakerHeatmapBandSelectEl() { return inDisplayPanel('speakerHeatmapBandSelect'); }
function getObjectEnergyHeatmapToggleEl() { return inDisplayPanel('objectEnergyHeatmapToggle'); }
function getObjectEnergyColormapEl() { return inDisplayPanel('objectEnergyColormap'); }
function getObjectEnergyVolumeMixSliderEl() { return inDisplayPanel('objectEnergyVolumeMixSlider'); }
function getObjectEnergyVolumeMixValEl() { return inDisplayPanel('objectEnergyVolumeMixVal'); }
function getObjectEnergyVolumeGammaAccumulateSliderEl() { return inDisplayPanel('objectEnergyVolumeGammaAccumulateSlider'); }
function getObjectEnergyVolumeGammaAccumulateValEl() { return inDisplayPanel('objectEnergyVolumeGammaAccumulateVal'); }
function getObjectEnergyVolumeGammaMipSliderEl() { return inDisplayPanel('objectEnergyVolumeGammaMipSlider'); }
function getObjectEnergyVolumeGammaMipValEl() { return inDisplayPanel('objectEnergyVolumeGammaMipVal'); }
function getObjectEnergyHeatmapResolutionSliderEl() { return inDisplayPanel('objectEnergyHeatmapResolutionSlider'); }
function getObjectEnergyHeatmapResolutionValEl() { return inDisplayPanel('objectEnergyHeatmapResolutionVal'); }
function getVolumeRefreshSliderEl() { return inDisplayPanel('volumeRefreshSlider'); }
function getVolumeRefreshValEl() { return inDisplayPanel('volumeRefreshVal'); }
function getObjectEnergyHeatmapRadiusSliderEl() { return inDisplayPanel('objectEnergyHeatmapRadiusSlider'); }
function getObjectEnergyHeatmapRadiusValEl() { return inDisplayPanel('objectEnergyHeatmapRadiusVal'); }
function getObjectEnergyHeatmapOpacitySliderEl() { return inDisplayPanel('objectEnergyHeatmapOpacitySlider'); }
function getObjectEnergyHeatmapOpacityValEl() { return inDisplayPanel('objectEnergyHeatmapOpacityVal'); }
function getVolumeSmoothToggleEl() { return inDisplayPanel('volumeSmoothToggle'); }

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

export function persistTrailPrefs() {
  try {
    const payload = {
      enabled: app.trailsEnabled,
      mode: app.trailRenderMode === 'line' ? 'line' : 'diffuse',
      duration_ms: app.trailPointTtlMs,
      teleport_threshold: app.trailTeleportThreshold
    };
    localStorage.setItem(TRAIL_PREFS_STORAGE_KEY, JSON.stringify(payload));
  } catch (_e) {
    // Ignore storage errors (private mode, quota, etc.).
  }
}

export function persistEffectiveRenderPrefs() {
  try {
    localStorage.setItem(EFFECTIVE_RENDER_PREFS_STORAGE_KEY, JSON.stringify({
      enabled: app.effectiveRenderEnabled,
      objectsVisible: app.objectsVisible,
      objectColors: app.objectColorsEnabled,
      objectDisplayMode: app.objectDisplayMode,
      objectSphereSize: app.objectSphereSize,
      objectLabels: app.objectLabelsEnabled,
      showObjectDetails: app.showObjectDetails,
      speakerLabels: app.speakerLabelsEnabled,
      speakerBands: app.speakerBandBarsEnabled,
      speakerFaceListener: app.speakerFaceListenerEnabled,
      speakerSize: app.speakerSize,
      speakerHeatmapVolumeEnabled: app.speakerHeatmapVolumeEnabled,
      speakerHeatmapVolumeColormap: app.speakerHeatmapVolumeColormap,
      speakerHeatmapBandIndex: app.speakerHeatmapBandIndex,
      speakerHeatmapAllBands: app.speakerHeatmapAllBands,
      objectEnergyHeatmapEnabled: app.objectEnergyHeatmapEnabled,
      objectEnergyColormap: app.objectEnergyColormap,
      objectEnergyVolumeMix: app.objectEnergyVolumeMix,
      objectEnergyVolumeGammaAccumulate: app.objectEnergyVolumeGammaAccumulate,
      objectEnergyVolumeGammaMip: app.objectEnergyVolumeGammaMip,
      objectEnergyHeatmapBandCount: app.objectEnergyHeatmapBandCount,
      objectEnergyHeatmapResolution: app.objectEnergyHeatmapResolution,
      objectEnergyHeatmapFalloffRadius: app.objectEnergyHeatmapFalloffRadius,
      objectEnergyHeatmapOpacity: app.objectEnergyHeatmapOpacity,
      volumeRefreshMs: app.volumeRefreshMs,
      volumeSmoothInterpolation: app.volumeSmoothInterpolation,
      objectCustomGradientStops: app.objectCustomGradientStops,
      speakerCustomGradientStops: app.speakerCustomGradientStops
    }));
  } catch (_e) {
    // Ignore storage errors (private mode, quota, etc.).
  }
}

export function applyTrailPrefsToUi() {
  const trailToggleEl = getTrailToggleEl();
  const trailModeSelectEl = getTrailModeSelectEl();
  const trailTtlSliderEl = getTrailTtlSliderEl();
  const trailTtlValEl = getTrailTtlValEl();
  const trailTeleportSliderEl = getTrailTeleportSliderEl();
  const trailTeleportValEl = getTrailTeleportValEl();
  if (trailToggleEl) {
    trailToggleEl.checked = app.trailsEnabled;
  }
  if (trailModeSelectEl) {
    trailModeSelectEl.value = app.trailRenderMode;
  }
  if (trailTtlSliderEl) {
    trailTtlSliderEl.value = (app.trailPointTtlMs / 1000).toFixed(1);
  }
  if (trailTtlValEl) {
    trailTtlValEl.textContent = `${(app.trailPointTtlMs / 1000).toFixed(1)}s`;
  }
  if (trailTeleportSliderEl) {
    trailTeleportSliderEl.value = app.trailTeleportThreshold.toFixed(2);
  }
  if (trailTeleportValEl) {
    trailTeleportValEl.textContent = app.trailTeleportThreshold.toFixed(2);
  }
}

export function applyEffectiveRenderPrefsToUi() {
  const effectiveRenderToggleEl = getEffectiveRenderToggleEl();
  const showObjectsToggleEl = getShowObjectsToggleEl();
  const objectColorsToggleEl = getObjectColorsToggleEl();
  const objectDisplayModeSelectEl = getObjectDisplayModeSelectEl();
  const objectSphereSizeSliderEl = getObjectSphereSizeSliderEl();
  const objectSphereSizeValEl = getObjectSphereSizeValEl();
  const objectLabelsToggleEl = getObjectLabelsToggleEl();
  const showObjectDetailsToggleEl = getShowObjectDetailsToggleEl();
  const speakerLabelsToggleEl = getSpeakerLabelsToggleEl();
  const speakerBandBarsToggleEl = getSpeakerBandBarsToggleEl();
  const speakerFaceListenerToggleEl = getSpeakerFaceListenerToggleEl();
  const speakerSizeSliderEl = getSpeakerSizeSliderEl();
  const speakerSizeValEl = getSpeakerSizeValEl();
  const speakerHeatmapVolumeToggleEl = getSpeakerHeatmapVolumeToggleEl();
  const speakerHeatmapBandSelectEl = getSpeakerHeatmapBandSelectEl();
  if (effectiveRenderToggleEl) {
    effectiveRenderToggleEl.checked = app.effectiveRenderEnabled;
  }
  if (showObjectsToggleEl) {
    showObjectsToggleEl.checked = app.objectsVisible !== false;
  }
  if (objectColorsToggleEl) {
    objectColorsToggleEl.checked = app.objectColorsEnabled;
  }
  if (objectDisplayModeSelectEl) {
    objectDisplayModeSelectEl.value = app.objectDisplayMode;
  }
  if (objectSphereSizeSliderEl) {
    objectSphereSizeSliderEl.value = String(app.objectSphereSize);
  }
  if (objectSphereSizeValEl) {
    objectSphereSizeValEl.textContent = app.objectSphereSize.toFixed(3);
  }
  if (objectLabelsToggleEl) {
    objectLabelsToggleEl.checked = app.objectLabelsEnabled;
  }
  if (showObjectDetailsToggleEl) {
    showObjectDetailsToggleEl.checked = app.showObjectDetails;
    document.body.classList.toggle('hide-object-details', !app.showObjectDetails);
  }
  if (speakerLabelsToggleEl) {
    speakerLabelsToggleEl.checked = app.speakerLabelsEnabled;
  }
  if (speakerBandBarsToggleEl) {
    speakerBandBarsToggleEl.checked = app.speakerBandBarsEnabled;
  }
  if (speakerFaceListenerToggleEl) {
    speakerFaceListenerToggleEl.checked = app.speakerFaceListenerEnabled;
  }
  if (speakerSizeSliderEl) {
    speakerSizeSliderEl.value = String(app.speakerSize);
  }
  if (speakerSizeValEl) {
    speakerSizeValEl.textContent = app.speakerSize.toFixed(3);
  }
  if (speakerHeatmapVolumeToggleEl) {
    speakerHeatmapVolumeToggleEl.checked = app.speakerHeatmapVolumeEnabled;
  }
  const speakerHeatmapVolumeColormapEl = getSpeakerHeatmapVolumeColormapEl();
  if (speakerHeatmapVolumeColormapEl) {
    speakerHeatmapVolumeColormapEl.value = app.speakerHeatmapVolumeColormap;
  }
  syncSpeakerHeatmapBandSelect();
  if (speakerHeatmapBandSelectEl) {
    speakerHeatmapBandSelectEl.value = app.speakerHeatmapAllBands
      ? 'all'
      : String(app.speakerHeatmapBandIndex);
  }
  const objectEnergyHeatmapToggleEl = getObjectEnergyHeatmapToggleEl();
  const objectEnergyColormapEl = getObjectEnergyColormapEl();
  const objectEnergyVolumeMixSliderEl = getObjectEnergyVolumeMixSliderEl();
  const objectEnergyVolumeMixValEl = getObjectEnergyVolumeMixValEl();
  const objectEnergyVolumeGammaAccumulateSliderEl = getObjectEnergyVolumeGammaAccumulateSliderEl();
  const objectEnergyVolumeGammaAccumulateValEl = getObjectEnergyVolumeGammaAccumulateValEl();
  const objectEnergyVolumeGammaMipSliderEl = getObjectEnergyVolumeGammaMipSliderEl();
  const objectEnergyVolumeGammaMipValEl = getObjectEnergyVolumeGammaMipValEl();
  const objectEnergyHeatmapResolutionSliderEl = getObjectEnergyHeatmapResolutionSliderEl();
  const objectEnergyHeatmapResolutionValEl = getObjectEnergyHeatmapResolutionValEl();
  const volumeRefreshSliderEl = getVolumeRefreshSliderEl();
  const volumeRefreshValEl = getVolumeRefreshValEl();
  const objectEnergyHeatmapRadiusSliderEl = getObjectEnergyHeatmapRadiusSliderEl();
  const objectEnergyHeatmapRadiusValEl = getObjectEnergyHeatmapRadiusValEl();
  const objectEnergyHeatmapOpacitySliderEl = getObjectEnergyHeatmapOpacitySliderEl();
  const objectEnergyHeatmapOpacityValEl = getObjectEnergyHeatmapOpacityValEl();
  if (objectEnergyHeatmapToggleEl) {
    objectEnergyHeatmapToggleEl.checked = app.objectEnergyHeatmapEnabled;
  }
  if (objectEnergyColormapEl) {
    objectEnergyColormapEl.value = app.objectEnergyColormap;
  }
  if (objectEnergyVolumeMixSliderEl) {
    objectEnergyVolumeMixSliderEl.value = String(app.objectEnergyVolumeMix);
  }
  if (objectEnergyVolumeMixValEl) {
    objectEnergyVolumeMixValEl.textContent = app.objectEnergyVolumeMix.toFixed(2);
  }
  if (objectEnergyVolumeGammaAccumulateSliderEl) {
    objectEnergyVolumeGammaAccumulateSliderEl.value = String(app.objectEnergyVolumeGammaAccumulate);
  }
  if (objectEnergyVolumeGammaAccumulateValEl) {
    objectEnergyVolumeGammaAccumulateValEl.textContent = app.objectEnergyVolumeGammaAccumulate.toFixed(1);
  }
  if (objectEnergyVolumeGammaMipSliderEl) {
    objectEnergyVolumeGammaMipSliderEl.value = String(app.objectEnergyVolumeGammaMip);
  }
  if (objectEnergyVolumeGammaMipValEl) {
    objectEnergyVolumeGammaMipValEl.textContent = app.objectEnergyVolumeGammaMip.toFixed(2);
  }
  if (objectEnergyHeatmapResolutionSliderEl) {
    objectEnergyHeatmapResolutionSliderEl.value = String(app.objectEnergyHeatmapResolution);
  }
  if (objectEnergyHeatmapResolutionValEl) {
    objectEnergyHeatmapResolutionValEl.textContent = String(app.objectEnergyHeatmapResolution);
  }
  if (volumeRefreshSliderEl) {
    volumeRefreshSliderEl.value = String(app.volumeRefreshMs);
  }
  if (volumeRefreshValEl) {
    volumeRefreshValEl.textContent = `${app.volumeRefreshMs} ms`;
  }
  if (objectEnergyHeatmapRadiusSliderEl) {
    objectEnergyHeatmapRadiusSliderEl.value = String(app.objectEnergyHeatmapFalloffRadius);
  }
  if (objectEnergyHeatmapRadiusValEl) {
    objectEnergyHeatmapRadiusValEl.textContent = app.objectEnergyHeatmapFalloffRadius.toFixed(2);
  }
  if (objectEnergyHeatmapOpacitySliderEl) {
    objectEnergyHeatmapOpacitySliderEl.value = String(app.objectEnergyHeatmapOpacity);
  }
  if (objectEnergyHeatmapOpacityValEl) {
    objectEnergyHeatmapOpacityValEl.textContent = app.objectEnergyHeatmapOpacity.toFixed(2);
  }
  const volumeSmoothToggleEl = getVolumeSmoothToggleEl();
  if (volumeSmoothToggleEl) {
    volumeSmoothToggleEl.checked = app.volumeSmoothInterpolation;
  }
}

export function loadTrailPrefs() {
  try {
    const raw = localStorage.getItem(TRAIL_PREFS_STORAGE_KEY);
    if (!raw) {
      applyTrailPrefsToUi();
      return;
    }
    const parsed = JSON.parse(raw);
    app.trailsEnabled = Boolean(parsed?.enabled);
    app.trailRenderMode = parsed?.mode === 'line' ? 'line' : 'diffuse';
    const durationMs = Number(parsed?.duration_ms);
    if (Number.isFinite(durationMs)) {
      app.trailPointTtlMs = Math.max(500, durationMs);
    }
    const teleport = Number(parsed?.teleport_threshold);
    if (Number.isFinite(teleport)) {
      app.trailTeleportThreshold = Math.max(0.05, Math.min(2.0, teleport));
    }
  } catch (_e) {
    // Ignore malformed payloads.
  }
  applyTrailPrefsToUi();
}

export function loadEffectiveRenderPrefs() {
  try {
    const raw = localStorage.getItem(EFFECTIVE_RENDER_PREFS_STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw);
      app.effectiveRenderEnabled = Boolean(parsed?.enabled);
      if (typeof parsed?.objectsVisible === 'boolean') {
        app.objectsVisible = parsed.objectsVisible;
      }
      app.objectColorsEnabled = Boolean(parsed?.objectColors);
      if (parsed?.objectDisplayMode === 'transparent-sphere' || parsed?.objectDisplayMode === 'diffuse-sphere') {
        app.objectDisplayMode = parsed.objectDisplayMode;
      } else {
        app.objectDisplayMode = 'circle';
      }
      const objectSphereSize = Number(parsed?.objectSphereSize);
      if (Number.isFinite(objectSphereSize)) {
        app.objectSphereSize = Math.max(0.03, Math.min(0.2, objectSphereSize));
      }
      if (typeof parsed?.objectLabels === 'boolean') {
        app.objectLabelsEnabled = parsed.objectLabels;
      }
      if (typeof parsed?.showObjectDetails === 'boolean') {
        app.showObjectDetails = parsed.showObjectDetails;
      }
      if (typeof parsed?.speakerLabels === 'boolean') {
        app.speakerLabelsEnabled = parsed.speakerLabels;
      }
      if (typeof parsed?.speakerBands === 'boolean') {
        app.speakerBandBarsEnabled = parsed.speakerBands;
      }
      if (typeof parsed?.speakerFaceListener === 'boolean') {
        app.speakerFaceListenerEnabled = parsed.speakerFaceListener;
      }
      const speakerSize = Number(parsed?.speakerSize);
      if (Number.isFinite(speakerSize)) {
        app.speakerSize = Math.max(0.04, Math.min(0.2, speakerSize));
      }
      if (typeof parsed?.speakerHeatmapVolumeEnabled === 'boolean') {
        app.speakerHeatmapVolumeEnabled = parsed.speakerHeatmapVolumeEnabled;
      }
      if (OBJECT_ENERGY_COLORMAPS.includes(parsed?.speakerHeatmapVolumeColormap)) {
        app.speakerHeatmapVolumeColormap = parsed.speakerHeatmapVolumeColormap;
      }
      const bandIndex = Number(parsed?.speakerHeatmapBandIndex);
      if (Number.isFinite(bandIndex)) {
        app.speakerHeatmapBandIndex = Math.max(0, Math.round(bandIndex));
      }
      if (typeof parsed?.speakerHeatmapAllBands === 'boolean') {
        app.speakerHeatmapAllBands = parsed.speakerHeatmapAllBands;
      }
      if (typeof parsed?.objectEnergyHeatmapEnabled === 'boolean') {
        app.objectEnergyHeatmapEnabled = parsed.objectEnergyHeatmapEnabled;
      }
      if (OBJECT_ENERGY_COLORMAPS.includes(parsed?.objectEnergyColormap)) {
        app.objectEnergyColormap = parsed.objectEnergyColormap;
      }
      const objectEnergyVolumeMix = Number(parsed?.objectEnergyVolumeMix);
      if (Number.isFinite(objectEnergyVolumeMix)) {
        app.objectEnergyVolumeMix = Math.max(0, Math.min(1, objectEnergyVolumeMix));
      }
      if (Number.isFinite(Number(parsed?.objectEnergyVolumeGammaAccumulate))) {
        app.objectEnergyVolumeGammaAccumulate = clampVolumeGamma('accumulate', parsed.objectEnergyVolumeGammaAccumulate);
      }
      if (Number.isFinite(Number(parsed?.objectEnergyVolumeGammaMip))) {
        app.objectEnergyVolumeGammaMip = clampVolumeGamma('mip', parsed.objectEnergyVolumeGammaMip);
      }
      const objectEnergyBandCount = Number(parsed?.objectEnergyHeatmapBandCount);
      if (Number.isFinite(objectEnergyBandCount)) {
        app.objectEnergyHeatmapBandCount = Math.max(1, Math.min(12, Math.round(objectEnergyBandCount)));
      }
      const objectEnergyResolution = Number(parsed?.objectEnergyHeatmapResolution);
      if (Number.isFinite(objectEnergyResolution)) {
        app.objectEnergyHeatmapResolution = Math.max(8, Math.min(64, Math.round(objectEnergyResolution)));
      }
      const objectEnergyRadius = Number(parsed?.objectEnergyHeatmapFalloffRadius);
      if (Number.isFinite(objectEnergyRadius)) {
        app.objectEnergyHeatmapFalloffRadius = Math.max(0.02, Math.min(0.5, objectEnergyRadius));
      }
      const objectEnergyOpacity = Number(parsed?.objectEnergyHeatmapOpacity);
      if (Number.isFinite(objectEnergyOpacity)) {
        app.objectEnergyHeatmapOpacity = Math.max(0.05, Math.min(1.0, objectEnergyOpacity));
      }
      const volumeRefreshMs = Number(parsed?.volumeRefreshMs);
      if (Number.isFinite(volumeRefreshMs)) {
        app.volumeRefreshMs = Math.max(40, Math.min(500, Math.round(volumeRefreshMs)));
      }
      if (typeof parsed?.volumeSmoothInterpolation === 'boolean') {
        app.volumeSmoothInterpolation = parsed.volumeSmoothInterpolation;
      }
      const objStops = sanitizeGradientStops(parsed?.objectCustomGradientStops);
      if (objStops) {
        app.objectCustomGradientStops = objStops;
      }
      const spkStops = sanitizeGradientStops(parsed?.speakerCustomGradientStops);
      if (spkStops) {
        app.speakerCustomGradientStops = spkStops;
      }
    }
  } catch (_e) {
    // Ignore malformed payloads.
  }
  applyEffectiveRenderPrefsToUi();
}

// Validate a persisted custom-gradient: 2..MAX_CUSTOM_STOPS stops with numeric
// pos/r/g/b clamped to [0,1] and sorted; returns null if unusable (keep default).
function sanitizeGradientStops(raw) {
  if (!Array.isArray(raw) || raw.length < 2) return null;
  const clamp01 = (v) => Math.max(0, Math.min(1, Number(v)));
  const out = [];
  for (const s of raw.slice(0, MAX_CUSTOM_STOPS)) {
    if (!s || ![s.pos, s.r, s.g, s.b].every((v) => Number.isFinite(Number(v)))) return null;
    out.push({ pos: clamp01(s.pos), r: clamp01(s.r), g: clamp01(s.g), b: clamp01(s.b) });
  }
  out.sort((a, b) => a.pos - b.pos);
  return out;
}

export function refreshEffectiveRenderVisibility() {
  if (typeof flushCallbacks.refreshEffectiveRenderVisibility === 'function') {
    flushCallbacks.refreshEffectiveRenderVisibility();
  }
}

export function getRoomSizeInputEl(axis) {
  if (axis === 'width') return getRoomDimWidthInputEl();
  if (axis === 'length') return getRoomDimLengthInputEl();
  if (axis === 'height') return getRoomDimHeightInputEl();
  if (axis === 'rear') return getRoomDimRearInputEl();
  if (axis === 'lower') return getRoomDimLowerInputEl();
  return null;
}

function roundRoomGeom(value) {
  const n = Number(value);
  if (!Number.isFinite(n)) return 0;
  return Math.round(n * 1e6) / 1e6;
}

export function getRoomCenterBlendFromInput(fallback = app.roomRatio.centerBlend) {
  const roomRatioCenterBlendSliderEl = getRoomRatioCenterBlendSliderEl();
  const n = Number(roomRatioCenterBlendSliderEl?.value);
  const fallbackNum = Number(fallback);
  if (!Number.isFinite(n)) return Number.isFinite(fallbackNum) ? fallbackNum : 0.5;
  return Math.max(0, Math.min(1, n / 100));
}

export function renderRoomCenterBlendControl(value = app.roomRatio.centerBlend) {
  const roomRatioCenterBlendSliderEl = getRoomRatioCenterBlendSliderEl();
  const roomRatioCenterBlendValueEl = getRoomRatioCenterBlendValueEl();
  const parsed = Number(value);
  const blend = Math.max(0, Math.min(1, Number.isFinite(parsed) ? parsed : 0.5));
  if (roomRatioCenterBlendSliderEl) {
    roomRatioCenterBlendSliderEl.value = String(Math.round(blend * 100));
  }
  if (roomRatioCenterBlendValueEl) {
    roomRatioCenterBlendValueEl.textContent = `${Math.round(blend * 100)}/${Math.round((1 - blend) * 100)}`;
  }
}

// The Y center blend only matters when Front and Rear differ — when they are
// equal the blend has no effect on the renderer (room_transform: center_ratio
// collapses to front and the cubic terms vanish), so we hide the control.
function updateCenterBlendVisibility(frontRatio = app.roomRatio.length, rearRatio = app.roomRatio.rear) {
  const row = inRoomGeometryPanel('roomCenterBlendRow');
  if (!row) return;
  const symmetric = Math.abs((Number(frontRatio) || 0) - (Number(rearRatio) || 0)) < 1e-6;
  row.style.display = symmetric ? 'none' : '';
}

// m/u (metres per unit) = the world scale = Width / 2, shown under the X column.
function renderRoomMpu(mpuValue = app.metersPerUnit ?? 1) {
  const el = inRoomGeometryPanel('roomMpuValue');
  if (el) el.textContent = formatNumber(Number(mpuValue) || 1, 2);
}

export function roomGeometryStateFromInputs() {
  const axes = ['width', 'length', 'height', 'rear', 'lower'];
  const preview = computeRoomGeometryFromInputs();
  const state = {
    mpu: roundRoomGeom(preview.mpu),
    centerBlend: roundRoomGeom(getRoomCenterBlendFromInput()),
    size: {}
  };
  axes.forEach((axis) => {
    state.size[axis] = roundRoomGeom(getRoomSizeInputEl(axis)?.value);
  });
  return state;
}

export function normalizeRoomGeometryInputDisplays() {
  [
    getRoomDimWidthInputEl(),
    getRoomDimLengthInputEl(),
    getRoomDimHeightInputEl(),
    getRoomDimRearInputEl(),
    getRoomDimLowerInputEl()
  ].forEach((el) => {
    if (!el) return;
    const n = Number(el.value);
    if (!Number.isFinite(n)) return;
    el.value = formatNumber(n, 2);
  });
}

export function roomGeometryStateKey(state) {
  const s = state || roomGeometryStateFromInputs();
  return JSON.stringify({
    mpu: s.mpu,
    centerBlend: s.centerBlend,
    size: s.size
  });
}

export function updateRoomGeometryButtonsState() {
  const roomGeometryCancelBtnEl = getRoomGeometryCancelBtnEl();
  if (isRoomRatioFrozen()) {
    if (roomGeometryCancelBtnEl) {
      roomGeometryCancelBtnEl.disabled = true;
      roomGeometryCancelBtnEl.style.opacity = '0.55';
      roomGeometryCancelBtnEl.style.cursor = 'default';
    }
    return;
  }
  const currentKey = roomGeometryStateKey();
  const unchanged = app.roomGeometryBaselineKey !== '' && currentKey === app.roomGeometryBaselineKey;
  if (roomGeometryCancelBtnEl) {
    roomGeometryCancelBtnEl.disabled = unchanged;
    roomGeometryCancelBtnEl.style.opacity = unchanged ? '0.55' : '1';
    roomGeometryCancelBtnEl.style.cursor = unchanged ? 'default' : 'pointer';
  }
}

export function applyRoomGeometryNow() {
  if (isRoomRatioFrozen()) {
    return;
  }
  const preview = computeRoomGeometryFromInputs();
  const width = preview.ratio.width;
  const length = preview.ratio.length;
  const height = preview.ratio.height;
  const rear = preview.ratio.rear;
  const lower = preview.ratio.lower;
  const centerBlend = getRoomCenterBlendFromInput();
  const mpu = preview.mpu;

  app.metersPerUnit = mpu;
  const layout = app.currentLayoutKey ? layoutsByKey.get(app.currentLayoutKey) : null;
  if (layout) {
    layout.radius_m = mpu;
  }

  applyRoomRatio({ width, length, height, rear, lower, centerBlend });
  invoke('control_layout_radius_m', { value: mpu });
  invoke('control_room_ratio_center_blend', { value: centerBlend });
  invoke('control_room_ratio', { width, length, height });
  invoke('control_room_ratio_rear', { value: rear });
  invoke('control_room_ratio_lower', { value: lower });
  renderSpeakerEditor();
  normalizeRoomGeometryInputDisplays();
  setRoomGeometryBaselineFromInputs();
}

export function scheduleRoomGeometryApply(delayMs = 120) {
  if (app.roomGeometryApplyTimer !== null) {
    clearTimeout(app.roomGeometryApplyTimer);
  }
  app.roomGeometryApplyTimer = window.setTimeout(() => {
    app.roomGeometryApplyTimer = null;
    applyRoomGeometryNow();
  }, delayMs);
}

export function setRoomGeometryBaselineFromInputs() {
  app.roomGeometryBaselineKey = roomGeometryStateKey();
  updateRoomGeometryButtonsState();
}

export function renderRoomGeometrySummary(preview = null) {
  const roomGeometrySummaryEl = getRoomGeometrySummaryEl();
  const roomGeometryHeaderSummaryEl = getRoomGeometryHeaderSummaryEl();
  const roomGeometrySummaryScaleEl = getRoomGeometrySummaryScaleEl();
  const roomGeometrySummarySizeEl = getRoomGeometrySummarySizeEl();
  const roomGeometrySummaryRatioEl = getRoomGeometrySummaryRatioEl();
  if (!roomGeometrySummaryEl && !roomGeometryHeaderSummaryEl) return;
  const metersPerUnit = app.metersPerUnit ?? 1;
  const ratioWidth = Number(preview?.ratio?.width ?? app.roomRatio.width) || 1;
  const ratioLength = Number(preview?.ratio?.length ?? app.roomRatio.length) || 1;
  const ratioRear = Number(preview?.ratio?.rear ?? app.roomRatio.rear) || 1;
  const ratioHeight = Number(preview?.ratio?.height ?? app.roomRatio.height) || 1;
  const ratioLower = Number(preview?.ratio?.lower ?? app.roomRatio.lower) || 0.5;
  const mpuValue = Number(preview?.mpu ?? metersPerUnit) || 1;
  const sizeWidth = ratioWidth * mpuValue * 2;
  const sizeFront = ratioLength * mpuValue;
  const sizeRear = ratioRear * mpuValue;
  const sizeHeight = ratioHeight * mpuValue;
  const sizeLower = ratioLower * mpuValue;

  if (roomGeometryHeaderSummaryEl) {
    roomGeometryHeaderSummaryEl.textContent =
      `m/u ${formatNumber(mpuValue, 2)} • X ${formatNumber(sizeWidth, 2)}m • Y ${formatNumber(sizeFront + sizeRear, 2)}m • Z ${formatNumber(sizeHeight + sizeLower, 2)}m`;
  }

  if (roomGeometrySummaryScaleEl) {
    roomGeometrySummaryScaleEl.textContent = `m/u: ${formatNumber(mpuValue, 2)}`;
  }
  if (roomGeometrySummarySizeEl) {
    roomGeometrySummarySizeEl.textContent =
      `X: ${formatNumber(sizeWidth, 2)}m | Y+: ${formatNumber(sizeFront, 2)}m | Y-: ${formatNumber(sizeRear, 2)}m | Z+: ${formatNumber(sizeHeight, 2)}m | Z-: ${formatNumber(sizeLower, 2)}m`;
  }
  if (roomGeometrySummaryRatioEl) {
    roomGeometrySummaryRatioEl.textContent =
      `X: ${formatNumber(ratioWidth, 2)} | Y+: ${formatNumber(ratioLength, 2)} | Y-: ${formatNumber(ratioRear, 2)} | Z+: ${formatNumber(ratioHeight, 2)} | Z-: ${formatNumber(ratioLower, 2)}`;
  }
}

export function applyRoomGeometryStateToInputs(state) {
  if (!state) return;
  const axes = ['width', 'length', 'height', 'rear', 'lower'];
  axes.forEach((axis) => {
    const sizeEl = getRoomSizeInputEl(axis);
    if (sizeEl && Number.isFinite(state.size?.[axis])) sizeEl.value = String(state.size[axis]);
  });
  const centerBlend = Number.isFinite(state.centerBlend) ? state.centerBlend : app.roomRatio.centerBlend;
  renderRoomCenterBlendControl(centerBlend);
  normalizeRoomGeometryInputDisplays();
  refreshRoomGeometryInputState();
  updateRoomGeometryButtonsState();
}

/**
 * Room geometry model — dimensions in metres, Width is the implicit reference.
 *
 * The room is entered as five plain metre dimensions: Width (full left↔right
 * span), Front (Y+, `length`), Rear (Y−), Height (Z+), Lower (Z−). There is no
 * master selector and no ratio field anymore — Width pins the scale and the
 * renderer's ratios are derived on the fly:
 *
 *     mpu = radius_m = Width / 2          (so ratio_width is always 1)
 *     ratio_front  = Front  / mpu         ratio_height = Height / mpu
 *     ratio_rear   = Rear   / mpu         ratio_lower  = Lower  / mpu
 *
 * (Width carries factor 2 — the normalised cube spans [-1,+1] across width but
 * only [0,+1] on each front/back/up/down half-axis.) Only the ratios + radius_m
 * are pushed to / persisted by the renderer (unchanged wire format); Studio
 * keeps no ratio of its own. Returns the legacy {master,mpu,ratio,size} shape so
 * the downstream apply/preview/summary code stays untouched.
 */
export function computeRoomGeometryFromInputs() {
  const mpuNow = app.metersPerUnit ?? 1;
  const safeNumber = (value, fallback, min = 0.01) => {
    const n = Number(value);
    if (!Number.isFinite(n)) return fallback;
    return Math.max(min, n);
  };

  const widthM = safeNumber(getRoomSizeInputEl('width')?.value, app.roomRatio.width * mpuNow * 2);
  const frontM = safeNumber(getRoomSizeInputEl('length')?.value, app.roomRatio.length * mpuNow);
  const rearM = safeNumber(getRoomSizeInputEl('rear')?.value, app.roomRatio.rear * mpuNow);
  const heightM = safeNumber(getRoomSizeInputEl('height')?.value, app.roomRatio.height * mpuNow);
  const lowerM = safeNumber(getRoomSizeInputEl('lower')?.value, app.roomRatio.lower * mpuNow);

  const mpu = Math.max(0.01, widthM / 2);
  const ratio = {
    width: 1,
    length: safeNumber(frontM / mpu, 1),
    height: safeNumber(heightM / mpu, 1),
    rear: safeNumber(rearM / mpu, 1),
    lower: safeNumber(lowerM / mpu, 0.5)
  };

  return {
    mpu,
    ratio,
    size: { width: widthM, length: frontM, height: heightM, rear: rearM, lower: lowerM }
  };
}

export function updateRoomGeometryLivePreview() {
  const preview = computeRoomGeometryFromInputs();
  // Metres-only model: every field is a direct, independent input — nothing is
  // derived, so there is no field to overwrite here.
  renderRoomGeometrySummary(preview);
  updateRoomDimensionGuides(preview);
  updateCenterBlendVisibility(preview.ratio.length, preview.ratio.rear);
  renderRoomMpu(preview.mpu);
}

function setRoomFieldEditable(inputEl, editable) {
  if (!inputEl) return;
  inputEl.readOnly = !editable;
  inputEl.tabIndex = editable ? 0 : -1;
  inputEl.style.pointerEvents = editable ? 'auto' : 'none';
  inputEl.classList.toggle('derived-field', !editable);
  inputEl.style.background = editable ? 'rgba(255,255,255,0.08)' : 'transparent';
  inputEl.style.border = editable ? '1px solid rgba(255,255,255,0.2)' : '1px solid transparent';
  inputEl.style.color = editable ? '#dfe8f3' : 'rgba(223,232,243,0.88)';
  inputEl.style.boxShadow = 'none';
}

export function refreshRoomGeometryInputState() {
  const roomRatioCenterBlendSliderEl = getRoomRatioCenterBlendSliderEl();
  const roomGeometryCancelBtnEl = getRoomGeometryCancelBtnEl();
  const axes = ['width', 'length', 'height', 'rear', 'lower'];
  const frozen = isRoomRatioFrozen();

  // Metres-only model: every dimension field is directly editable (no derived
  // size/ratio split anymore), unless the renderer has frozen the room ratio.
  axes.forEach((axis) => setRoomFieldEditable(getRoomSizeInputEl(axis), !frozen));
  if (roomRatioCenterBlendSliderEl) {
    roomRatioCenterBlendSliderEl.disabled = frozen;
  }
  if (roomGeometryCancelBtnEl) {
    roomGeometryCancelBtnEl.disabled = frozen || roomGeometryCancelBtnEl.disabled;
  }
  updateRoomGeometryLivePreview();
  updateRoomGeometryButtonsState();
}

export function renderRoomRatioDisplay() {
  const roomDimWidthInputEl = getRoomDimWidthInputEl();
  const roomDimLengthInputEl = getRoomDimLengthInputEl();
  const roomDimHeightInputEl = getRoomDimHeightInputEl();
  const roomDimRearInputEl = getRoomDimRearInputEl();
  const roomDimLowerInputEl = getRoomDimLowerInputEl();
  const metersPerUnit = app.metersPerUnit ?? 1;
  const dimW = app.roomRatio.width * metersPerUnit * 2;
  const dimL = app.roomRatio.length * metersPerUnit;
  const dimH = app.roomRatio.height * metersPerUnit;
  const dimRear = app.roomRatio.rear * metersPerUnit;
  const dimLower = app.roomRatio.lower * metersPerUnit;
  if (roomDimWidthInputEl) roomDimWidthInputEl.value = formatNumber(dimW, 2);
  if (roomDimLengthInputEl) roomDimLengthInputEl.value = formatNumber(dimL, 2);
  if (roomDimHeightInputEl) roomDimHeightInputEl.value = formatNumber(dimH, 2);
  if (roomDimRearInputEl) roomDimRearInputEl.value = formatNumber(dimRear, 2);
  if (roomDimLowerInputEl) roomDimLowerInputEl.value = formatNumber(dimLower, 2);
  renderRoomCenterBlendControl(app.roomRatio.centerBlend);
  updateCenterBlendVisibility();
  renderRoomMpu();
  renderRoomGeometrySummary();
  normalizeRoomGeometryInputDisplays();
  refreshRoomGeometryInputState();
  setRoomGeometryBaselineFromInputs();
}

export function updateRoomRatioDisplay() {
  dirty.roomRatio = true;
  scheduleUIFlush();
}

// ---------------------------------------------------------------------------
// Room geometry expansion toggle
// ---------------------------------------------------------------------------

export function setRoomGeometryExpanded(expanded) {
  app.roomGeometryExpanded = Boolean(expanded);
  const roomGeometryPanelRootEl = inRoomGeometryPanel('roomGeometryPanelRoot');
  const roomGeometryFormEl = inRoomGeometryPanel('roomGeometryForm');
  const roomGeometryHeaderSummaryEl = inRoomGeometryPanel('roomGeometryHeaderSummary');
  const roomGeometrySummaryEl = inRoomGeometryPanel('roomGeometrySummary');
  const roomGeometryToggleBtnEl = inRoomGeometryPanel('roomGeometryToggleBtn');
  if (roomGeometryPanelRootEl) {
    roomGeometryPanelRootEl.classList.toggle('section-collapsed', !app.roomGeometryExpanded);
  }
  if (roomGeometryFormEl) {
    roomGeometryFormEl.classList.toggle('open', app.roomGeometryExpanded);
  }
  if (roomGeometryHeaderSummaryEl) {
    roomGeometryHeaderSummaryEl.style.display = app.roomGeometryExpanded ? 'none' : 'block';
  }
  if (roomGeometrySummaryEl) {
    roomGeometrySummaryEl.style.display = 'none';
  }
  if (roomGeometryToggleBtnEl) {
    roomGeometryToggleBtnEl.textContent = app.roomGeometryExpanded ? '\u25be' : '\u25b8';
  }
  roomDimensionGroup.visible = app.roomGeometryExpanded;
  emitOverlayLayoutChanged('room-geometry-toggle');
}

// ---------------------------------------------------------------------------
// Room dimension guides (3D measurement overlays)
// ---------------------------------------------------------------------------

function createRoomDimensionGuide(color = 0x9dd3ff) {
  const line = new THREE.LineSegments(
    new THREE.BufferGeometry(),
    new THREE.LineBasicMaterial({ color, transparent: true, opacity: 0.85, depthTest: false })
  );
  line.renderOrder = 30;
  const label = createSmallLabelSprite('');
  label.renderOrder = 31;
  const group = new THREE.Group();
  group.add(line);
  group.add(label);
  roomDimensionGroup.add(group);
  return { group, line, label };
}

const roomDimensionGuides = {
  width: createRoomDimensionGuide(0x88c7ff),
  front: createRoomDimensionGuide(0xa0ffd1),
  rear: createRoomDimensionGuide(0xffd08a),
  total: createRoomDimensionGuide(0xb8b8ff),
  height: createRoomDimensionGuide(0xff9ed8),
  lower: createRoomDimensionGuide(0xff7a7a),
  totalHeight: createRoomDimensionGuide(0xffb3e6)
};

export function rebuildRoomDimensionGuideResources() {
  Object.values(roomDimensionGuides).forEach((guide) => {
    if (guide?.line?.material) {
      guide.line.material.needsUpdate = true;
    }
  });
}

function updateRoomDimensionGuide(guide, start, end, tickDir, labelText) {
  const tick = tickDir.clone().normalize().multiplyScalar(0.04);
  const points = [
    start, end,
    start.clone().sub(tick), start.clone().add(tick),
    end.clone().sub(tick), end.clone().add(tick)
  ];
  guide.line.geometry.dispose();
  guide.line.geometry = new THREE.BufferGeometry().setFromPoints(points);
  const mid = start.clone().add(end).multiplyScalar(0.5).add(tick.clone().multiplyScalar(2.2));
  guide.label.position.copy(mid);
  setLabelSpriteText(guide.label, labelText);
}

export function updateRoomDimensionGuides(preview = null) {
  const ratioWidth = Number(preview?.ratio?.width ?? app.roomRatio.width) || 1;
  const ratioLength = Number(preview?.ratio?.length ?? app.roomRatio.length) || 1;
  const ratioHeight = Number(preview?.ratio?.height ?? app.roomRatio.height) || 1;
  const ratioRear = Number(preview?.ratio?.rear ?? app.roomRatio.rear) || 1;
  const ratioLower = Number(preview?.ratio?.lower ?? app.roomRatio.lower) || 0.5;
  const mpuValue = Number(preview?.mpu ?? app.metersPerUnit) || 1;
  const xMin = roomBounds.xMin;
  const xMax = roomBounds.xMax;
  const yMin = roomBounds.yMin;
  const yMax = roomBounds.yMax;
  const zMin = roomBounds.zMin;
  const zMax = roomBounds.zMax;
  const yTop = yMax + 0.06;
  const off = 0.08;

  updateRoomDimensionGuide(
    roomDimensionGuides.width,
    new THREE.Vector3(xMax + off, yTop, zMin),
    new THREE.Vector3(xMax + off, yTop, zMax),
    new THREE.Vector3(1, 0, 0),
    `${formatNumber(ratioWidth * mpuValue * 2, 2)}m`
  );
  updateRoomDimensionGuide(
    roomDimensionGuides.front,
    new THREE.Vector3(0, yTop, zMax + off),
    new THREE.Vector3(xMax, yTop, zMax + off),
    new THREE.Vector3(0, 0, 1),
    `${formatNumber(ratioLength * mpuValue, 2)}m`
  );
  updateRoomDimensionGuide(
    roomDimensionGuides.rear,
    new THREE.Vector3(xMin, yTop, zMax + off),
    new THREE.Vector3(0, yTop, zMax + off),
    new THREE.Vector3(0, 0, 1),
    `${formatNumber(ratioRear * mpuValue, 2)}m`
  );
  updateRoomDimensionGuide(
    roomDimensionGuides.total,
    new THREE.Vector3(xMin, yTop, zMin - off),
    new THREE.Vector3(xMax, yTop, zMin - off),
    new THREE.Vector3(0, 0, 1),
    `${formatNumber((ratioLength + ratioRear) * mpuValue, 2)}m`
  );
  updateRoomDimensionGuide(
    roomDimensionGuides.height,
    new THREE.Vector3(xMax + off, 0, zMax + off),
    new THREE.Vector3(xMax + off, yMax, zMax + off),
    new THREE.Vector3(1, 0, 0),
    `${formatNumber(ratioHeight * mpuValue, 2)}m`
  );
  updateRoomDimensionGuide(
    roomDimensionGuides.lower,
    new THREE.Vector3(xMax + off, yMin, zMax + off),
    new THREE.Vector3(xMax + off, 0, zMax + off),
    new THREE.Vector3(1, 0, 0),
    `${formatNumber(ratioLower * mpuValue, 2)}m`
  );
  updateRoomDimensionGuide(
    roomDimensionGuides.totalHeight,
    new THREE.Vector3(xMax + off, yMin, zMin - off),
    new THREE.Vector3(xMax + off, yMax, zMin - off),
    new THREE.Vector3(1, 0, 0),
    `${formatNumber((ratioHeight + ratioLower) * mpuValue, 2)}m`
  );

  roomDimensionGroup.visible = app.roomGeometryExpanded;
}

// ---------------------------------------------------------------------------
// Apply room ratio to 3D scene objects
// ---------------------------------------------------------------------------

export function applyRoomRatioToScene(preview = null, { refit = true } = {}) {
  // `preview` (the result of computeRoomGeometryFromInputs) drives the box and
  // guides from typed, uncommitted values for a live preview while editing; with
  // no preview it renders the committed ratios. `refit` is skipped during the
  // live preview so the camera doesn't jump on every keystroke.
  const r = preview?.ratio ?? app.roomRatio;
  const xMax = Math.max(0.001, Number(r.length) || 1);
  const xMin = -Math.max(0.001, Number(r.rear) || 1);
  const yMax = Math.max(0.001, Number(r.height) || 1);
  const yMin = -Math.max(0.001, Number(r.lower) || 0.5);
  const halfZ = Math.max(0.001, Number(r.width) || 1);
  const depthHalfX = Math.max(0.001, (xMax - xMin) * 0.5);
  const xCenter = (xMin + xMax) * 0.5;
  const yCenter = (yMin + yMax) * 0.5;
  const totalHeight = yMax - yMin;

  roomBounds.xMin = xMin;
  roomBounds.xMax = xMax;
  roomBounds.yMin = yMin;
  roomBounds.yMax = yMax;
  roomBounds.zMin = -halfZ;
  roomBounds.zMax = halfZ;

  roomGroup.scale.set(1, 1, 1);

  room.scale.set(depthHalfX, totalHeight, halfZ);
  room.position.set(xCenter, yCenter, 0);
  roomEdges.scale.set(depthHalfX, totalHeight, halfZ);
  roomEdges.position.set(xCenter, yCenter, 0);

  roomFaces.posX.position.set(xMax, yCenter, 0);
  roomFaces.posX.scale.set(halfZ, totalHeight, 1);
  roomFaces.negX.position.set(xMin, yCenter, 0);
  roomFaces.negX.scale.set(halfZ, totalHeight, 1);
  roomFaces.posY.position.set(xCenter, yMax, 0);
  roomFaces.posY.scale.set(depthHalfX, halfZ, 1);
  roomFaces.negY.position.set(xCenter, yMin, 0);
  roomFaces.negY.scale.set(depthHalfX, halfZ, 1);
  roomFaces.posZ.position.set(xCenter, yCenter, halfZ);
  roomFaces.posZ.scale.set(depthHalfX, totalHeight, 1);
  roomFaces.negZ.position.set(xCenter, yCenter, -halfZ);
  roomFaces.negZ.scale.set(depthHalfX, totalHeight, 1);

  if (refit) fitScreenToUpperHalf();
  updateRoomDimensionGuides(preview);
  updateVbapCartesianFaceGrid();
  redrawHybridDistanceShape();
}

/**
 * Live scene preview while editing room inputs — refreshes the 3D box, guides,
 * master m/u readout and summary from the typed (uncommitted) values WITHOUT
 * rewriting the input fields or pushing to orender. The value is committed on
 * blur / Enter (the input listeners' 'change' path). This is what lets a partial
 * keystroke stand instead of being reset mid-edit.
 */
export function previewRoomGeometryScene() {
  const preview = computeRoomGeometryFromInputs();
  applyRoomRatioToScene(preview, { refit: false });
  renderRoomGeometrySummary(preview);
  updateCenterBlendVisibility(preview.ratio.length, preview.ratio.rear);
  renderRoomMpu(preview.mpu);
}

// ---------------------------------------------------------------------------
// Apply room ratio (reposition all sources and speakers)
// ---------------------------------------------------------------------------

export function applyRoomRatio(nextRatio) {
  // Adopt the room scale (m/u = radius_m) straight from the renderer's room
  // domain when present (the snapshot/echo carries scaleM). This is the reliable
  // restore path; a local commit calls applyRoomRatio without scaleM, so it
  // keeps the metersPerUnit the edit just set.
  const scaleM = Number(nextRatio.scaleM);
  if (Number.isFinite(scaleM) && scaleM > 0) {
    app.metersPerUnit = scaleM;
  }
  app.roomRatio.width = Number(nextRatio.width) || 1;
  app.roomRatio.length = Number(nextRatio.length) || 1;
  app.roomRatio.height = Number(nextRatio.height) || 1;
  const rearValue = Number(nextRatio.rear);
  const lowerValue = Number(nextRatio.lower);
  app.roomRatio.rear = Number.isFinite(rearValue) && rearValue > 0 ? rearValue : app.roomRatio.rear;
  app.roomRatio.lower = Number.isFinite(lowerValue) && lowerValue > 0 ? lowerValue : app.roomRatio.lower;
  const centerBlendValue = Number(nextRatio.centerBlend);
  app.roomRatio.centerBlend = Number.isFinite(centerBlendValue)
    ? Math.max(0, Math.min(1, centerBlendValue))
    : app.roomRatio.centerBlend;
  updateRoomRatioDisplay();
  applyRoomRatioToScene();

  sourceMeshes.forEach((mesh, id) => {
    const raw = sourcePositionsRaw.get(String(id));
    if (!raw) return;
    if (raw.directSpeakerIndex !== null && raw.directSpeakerIndex !== undefined) {
      const speakerMesh = speakerMeshes[raw.directSpeakerIndex];
      if (speakerMesh) {
        mesh.position.copy(speakerMesh.position);
      } else {
        const pos = normalizedOmniphonyToScenePosition(raw);
        mesh.position.set(pos.x, pos.y, pos.z);
      }
    } else {
      const pos = normalizedOmniphonyToScenePosition(raw);
      mesh.position.set(pos.x, pos.y, pos.z);
    }
    updateSourceDecorations(id);
    rebuildTrailGeometry(id);
  });

  speakerMeshes.forEach((mesh, index) => {
    const speaker = app.currentLayoutSpeakers[index];
    if (!speaker) return;
    hydrateSpeakerCoordinateState(speaker);
    const scenePosition = normalizedOmniphonyToScenePosition(speaker);
    mesh.position.set(scenePosition.x, scenePosition.y, scenePosition.z);
    const label = speakerLabels[index];
    if (label) {
      label.position.set(scenePosition.x, scenePosition.y + 0.12, scenePosition.z);
    }
  });

  renderSpeakerEditor();
}
