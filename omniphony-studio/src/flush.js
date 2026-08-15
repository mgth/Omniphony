/**
 * UI flush batching — scheduleUIFlush, flushUI, and per-item dirty helpers.
 *
 * Extracted from app.js.  Render functions that live in other modules are
 * wired up via the `flushCallbacks` registry — each module sets its own
 * callback at init time.
 */

import { decomposePosition, formatPosition } from './coordinates.js';
import { onRoomRatioChanged } from './controls/object-test.js';
import {
  dirtyObjectMeters,
  dirtySpeakerMeters,
  dirtyObjectPositions,
  dirtyObjectLabels,
  dirty,
  objectItems,
  speakerItems,
  sourceLevels,
  speakerLevels,
  sourcePositionsRaw,
  sourceSizes,
  sourceNames,
  app
} from './state.js';
import { updateMeterUI } from './mute-solo.js';

// ---------------------------------------------------------------------------
// Flush callback registry
//
// Other modules populate these at init time so flushUI can call them without
// circular-import issues.
// ---------------------------------------------------------------------------

export const flushCallbacks = {
  renderRoomRatioDisplay: null,
  renderEvaluationMode: null,
  renderRenderBackend: null,
  renderHybridOptions: null,
  renderVbapCartesian: null,
  renderVbapPolar: null,
  renderLoudnessDisplay: null,
  renderAdaptiveResamplingUI: null,
  renderDistanceDiffuseUI: null,
  renderDistanceModelUI: null,
  renderConfigSavedUI: null,
  renderLatencyDisplay: null,
  renderLatencyMeterUI: null,
  renderRenderTimeUI: null,
  renderResampleRatioDisplay: null,
  renderAudioFormatDisplay: null,
  renderDrcUI: null,
  renderMasterGainUI: null,
  renderAutoGainUI: null,
  renderAutoGainCeilingUI: null,
  updateMasterMeterUI: null,
  updateObjectContributionUI: null,
  updateSpeakerContributionUI: null,
  getObjectDisplayName: null,
  applyAudioSampleRateNow: null,
  refreshEffectiveRenderVisibility: null,
  updateVbapCartesianFaceGrid: null,
  renderVbapCartesianGridToggle: null,
  applyRoomRatio: null,
  updateRoomDimensionGuides: null
};

// ---------------------------------------------------------------------------
// Schedule / flush
// ---------------------------------------------------------------------------

export function scheduleUIFlush() {
  if (app.uiFlushScheduled) {
    return;
  }
  app.uiFlushScheduled = true;
  requestAnimationFrame(flushUI);
}

export function flushUI() {
  app.uiFlushScheduled = false;

  dirtyObjectMeters.forEach((id) => {
    const entry = objectItems.get(id);
    if (!entry) return;
    updateMeterUI(entry, sourceLevels.get(id), 'source', id);
    flushCallbacks.updateObjectContributionUI?.(entry, id);
  });
  dirtyObjectMeters.clear();

  dirtySpeakerMeters.forEach((id) => {
    const entry = speakerItems.get(id);
    if (!entry) return;
    updateMeterUI(entry, speakerLevels.get(id), 'speaker', id);
    flushCallbacks.updateSpeakerContributionUI?.(entry, id);
  });
  dirtySpeakerMeters.clear();

  dirtyObjectPositions.forEach((id) => {
    const entry = objectItems.get(id);
    if (!entry) return;
    const pos = sourcePositionsRaw.get(id);
    if (entry.axisElems) {
      const coords = decomposePosition(pos);
      Object.keys(entry.axisElems).forEach((axis) => {
        entry.axisElems[axis].textContent = `${axis}:${coords[axis]}`;
      });
    } else if (entry.position) {
      entry.position.textContent = formatPosition(pos);
    }
    // Live-refresh the row's position thumbnail as the object moves.
    flushCallbacks.updateObjectPositionIcon?.(entry, pos);
  });
  dirtyObjectPositions.clear();

  dirtyObjectLabels.forEach((id) => {
    const entry = objectItems.get(id);
    if (!entry) return;
    // Refresh the fixed type icon + short position code from the (possibly
    // changed) name. Falls back to writing the raw name onto the strip only if
    // the identity callback isn't wired yet.
    if (flushCallbacks.applyObjectIdentity) {
      flushCallbacks.applyObjectIdentity(entry, id);
    } else {
      const raw = sourceNames.get(String(id));
      entry.label.textContent =
        (raw && typeof raw === 'string' && raw.trim()) ? raw.trim() : String(id);
    }
  });
  dirtyObjectLabels.clear();

  if (dirty.masterMeter) {
    flushCallbacks.updateMasterMeterUI?.();
    dirty.masterMeter = false;
  }

  if (dirty.roomRatio) {
    flushCallbacks.renderRoomRatioDisplay?.();
    // The object-injection faces are drawn at the room's proportions, so they
    // are stale the moment those change.
    onRoomRatioChanged();
    dirty.roomRatio = false;
  }


  if (dirty.vbapMode) {
    flushCallbacks.renderEvaluationMode?.();
    dirty.vbapMode = false;
  }

  if (dirty.renderBackend) {
    flushCallbacks.renderRenderBackend?.();
    dirty.renderBackend = false;
  }

  if (dirty.hybrid) {
    flushCallbacks.renderHybridOptions?.();
    dirty.hybrid = false;
  }

  if (dirty.vbapCartesian) {
    flushCallbacks.renderVbapCartesian?.();
    dirty.vbapCartesian = false;
  }

  if (dirty.vbapPolar) {
    flushCallbacks.renderVbapPolar?.();
    dirty.vbapPolar = false;
  }

  if (dirty.loudness) {
    flushCallbacks.renderLoudnessDisplay?.();
    dirty.loudness = false;
  }

  if (dirty.adaptiveResampling) {
    flushCallbacks.renderAdaptiveResamplingUI?.();
    dirty.adaptiveResampling = false;
  }

  if (dirty.distanceDiffuse) {
    flushCallbacks.renderDistanceDiffuseUI?.();
    dirty.distanceDiffuse = false;
  }

  if (dirty.distanceModel) {
    flushCallbacks.renderDistanceModelUI?.();
    dirty.distanceModel = false;
  }

  if (dirty.configSaved) {
    flushCallbacks.renderConfigSavedUI?.();
    dirty.configSaved = false;
  }

  if (dirty.latency) {
    flushCallbacks.renderLatencyDisplay?.();
    flushCallbacks.renderLatencyMeterUI?.();
    dirty.latency = false;
  }

  if (dirty.renderTime) {
    flushCallbacks.renderRenderTimeUI?.();
    dirty.renderTime = false;
  }

  if (dirty.resample) {
    flushCallbacks.renderResampleRatioDisplay?.();
    dirty.resample = false;
  }

  if (dirty.audioFormat) {
    flushCallbacks.renderAudioFormatDisplay?.();
    dirty.audioFormat = false;
  }

  if (dirty.drcUI) {
    flushCallbacks.renderDrcUI?.();
    dirty.drcUI = false;
  }

  if (dirty.autoGain) {
    flushCallbacks.renderAutoGainUI?.();
    dirty.autoGain = false;
  }
  if (dirty.autoGainCeiling) {
    flushCallbacks.renderAutoGainCeilingUI?.();
    dirty.autoGainCeiling = false;
  }
  if (dirty.masterGain) {
    flushCallbacks.renderMasterGainUI?.();
    dirty.masterGain = false;
  }
}

// ---------------------------------------------------------------------------
// Item class helpers
// ---------------------------------------------------------------------------

export function updateItemClasses(entry, isMuted, isDimmed) {
  entry.root.classList.toggle('is-muted', isMuted);
  entry.root.classList.toggle('is-dimmed', isDimmed);
}

// ---------------------------------------------------------------------------
// Per-item dirty markers (convenience wrappers around scheduleUIFlush)
// ---------------------------------------------------------------------------

export function updateSpeakerMeterUI(id) {
  const key = String(id);
  dirtySpeakerMeters.add(key);
  scheduleUIFlush();
}

export function updateObjectMeterUI(id) {
  const key = String(id);
  dirtyObjectMeters.add(key);
  scheduleUIFlush();
}

export function updateObjectPositionUI(id, position) {
  const key = String(id);
  if (position) {
    sourcePositionsRaw.set(key, position);
  }
  dirtyObjectPositions.add(key);
  scheduleUIFlush();
}

/** Render the per-object size gauges from `sourceSizes`. Called directly (no
 *  batching) because three CSS-width writes are cheap. */
export function updateObjectSizeUI(id) {
  const key = String(id);
  const entry = objectItems.get(key);
  if (!entry || !entry.sizeFills) return;
  const s = sourceSizes.get(key);
  const w = s?.w ?? 0;
  const d = s?.d ?? 0;
  const h = s?.h ?? 0;
  entry.sizeFills.w.style.width = `${(Math.max(0, Math.min(1, w)) * 100).toFixed(1)}%`;
  entry.sizeFills.d.style.width = `${(Math.max(0, Math.min(1, d)) * 100).toFixed(1)}%`;
  entry.sizeFills.h.style.width = `${(Math.max(0, Math.min(1, h)) * 100).toFixed(1)}%`;
}

export function updateObjectLabelUI(id) {
  const key = String(id);
  dirtyObjectLabels.add(key);
  scheduleUIFlush();
}
