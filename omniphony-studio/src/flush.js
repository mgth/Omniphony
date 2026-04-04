/**
 * UI flush batching — scheduleUIFlush, flushUI, and per-item dirty helpers.
 *
 * Extracted from app.js.  Render functions that live in other modules are
 * wired up via the `flushCallbacks` registry — each module sets its own
 * callback at init time.
 */

import { formatPosition } from './coordinates.js';
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
  renderSpreadDisplay: null,
  renderVbapMode: null,
  renderRenderBackend: null,
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
  renderMasterGainUI: null,
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
    updateMeterUI(entry, sourceLevels.get(id));
    flushCallbacks.updateObjectContributionUI?.(entry, id);
  });
  dirtyObjectMeters.clear();

  dirtySpeakerMeters.forEach((id) => {
    const entry = speakerItems.get(id);
    if (!entry) return;
    updateMeterUI(entry, speakerLevels.get(id));
    flushCallbacks.updateSpeakerContributionUI?.(entry, id);
  });
  dirtySpeakerMeters.clear();

  dirtyObjectPositions.forEach((id) => {
    const entry = objectItems.get(id);
    if (!entry) return;
    const pos = sourcePositionsRaw.get(id);
    entry.position.textContent = formatPosition(pos);
  });
  dirtyObjectPositions.clear();

  dirtyObjectLabels.forEach((id) => {
    const entry = objectItems.get(id);
    if (!entry) return;
    const displayName = flushCallbacks.getObjectDisplayName
      ? flushCallbacks.getObjectDisplayName(id)
      : (() => {
          const raw = sourceNames.get(id);
          return (raw && typeof raw === 'string' && raw.trim()) ? raw.trim() : String(id);
        })();
    entry.label.textContent = displayName;
  });
  dirtyObjectLabels.clear();

  if (dirty.masterMeter) {
    flushCallbacks.updateMasterMeterUI?.();
    dirty.masterMeter = false;
  }

  if (dirty.roomRatio) {
    flushCallbacks.renderRoomRatioDisplay?.();
    dirty.roomRatio = false;
  }

  if (dirty.spread) {
    flushCallbacks.renderSpreadDisplay?.();
    dirty.spread = false;
  }

  if (dirty.vbapMode) {
    flushCallbacks.renderVbapMode?.();
    dirty.vbapMode = false;
  }

  if (dirty.renderBackend) {
    flushCallbacks.renderRenderBackend?.();
    dirty.renderBackend = false;
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

export function updateObjectLabelUI(id) {
  const key = String(id);
  dirtyObjectLabels.add(key);
  scheduleUIFlush();
}
