/**
 * Master gain and master meter controls.
 *
 * Extracted from app.js (lines 4547-4590).
 */

import { linearToDb } from '../audio-math.js';
import {
  app,
  dirty,
  supportsRealtimeKey,
  masterLevel
} from '../state.js';
import { t, tf } from '../i18n.js';
import { formatNumber } from '../coordinates.js';
import { formatLinearAsDb, dbToMeterPercent, METER_DB_MIN } from '../mute-solo.js';
import { scheduleUIFlush } from '../flush.js';
import { inAudioPanel, inRendererPanel, inDrcPanel } from '../ui/panel-roots.js';

function getMasterGainSliderEl() { return inAudioPanel('masterGainSlider'); }
function getMasterGainBoxEl() { return inAudioPanel('masterGainBox'); }
function getMasterMeterTextEl() { return inAudioPanel('masterMeterText'); }
function getMasterMeterFillEl() { return inAudioPanel('masterMeterFill'); }
function getLoudnessInfoEl() { return inDrcPanel('loudnessInfo'); }
function getLoudnessToggleEl() { return inDrcPanel('loudnessToggle'); }
function getDistanceModelSelectEl() { return inRendererPanel('distanceModelSelect'); }

export function renderMasterGainUI() {
  const masterGainSliderEl = getMasterGainSliderEl();
  const masterGainBoxEl = getMasterGainBoxEl();
  const canControlMasterGain = supportsRealtimeKey('master_gain');
  if (masterGainSliderEl) {
    const hasValue = Number.isFinite(app.masterGain) && app.masterGain > 0;
    masterGainSliderEl.disabled = !app.oscSnapshotReady || !hasValue || !canControlMasterGain;
    masterGainSliderEl.value = String(hasValue ? app.masterGain : 1);
  }
  if (masterGainBoxEl) {
    const hasValue = Number.isFinite(app.masterGain) && app.masterGain > 0;
    masterGainBoxEl.textContent = hasValue ? formatLinearAsDb(app.masterGain) : '—';
  }
}

export function updateMasterGainUI() {
  dirty.masterGain = true;
  scheduleUIFlush();
}

function getAutoGainToggleEl() { return inAudioPanel('autoGainToggle'); }
function getAutoGainCeilingSliderEl() { return inAudioPanel('autoGainCeilingSlider'); }
function getAutoGainCeilingValEl() { return inAudioPanel('autoGainCeilingVal'); }
function getClipIndicatorEl() { return inAudioPanel('clipIndicator'); }

export function renderAutoGainUI() {
  const autoGainToggleEl = getAutoGainToggleEl();
  if (autoGainToggleEl) {
    autoGainToggleEl.checked = app.autoGain === true;
    autoGainToggleEl.disabled = !app.oscSnapshotReady;
  }
}

export function updateAutoGainUI() {
  dirty.autoGain = true;
  scheduleUIFlush();
}

export function renderAutoGainCeilingUI() {
  const sliderEl = getAutoGainCeilingSliderEl();
  const valEl = getAutoGainCeilingValEl();
  const db = Number.isFinite(app.autoGainCeilingDb) ? app.autoGainCeilingDb : -1.0;
  if (sliderEl) {
    sliderEl.value = String(db);
    sliderEl.disabled = !app.oscSnapshotReady;
  }
  if (valEl) {
    valEl.textContent = `${db.toFixed(1)} dB`;
  }
}

export function updateAutoGainCeilingUI() {
  dirty.autoGainCeiling = true;
  scheduleUIFlush();
}

/**
 * Flash the clip indicator red with a 1 s remanence. Driven by the renderer's
 * `/omniphony/state/clip` event and works regardless of the auto-gain toggle.
 */
let clipFadeTimer = null;
export function flashClipIndicator() {
  const el = getClipIndicatorEl();
  if (!el) {
    return;
  }
  el.classList.add('clip-active');
  if (clipFadeTimer !== null) {
    clearTimeout(clipFadeTimer);
  }
  clipFadeTimer = setTimeout(() => {
    el.classList.remove('clip-active');
    clipFadeTimer = null;
  }, 1000);
}

// Master output meter. The backend is the single source of truth: it forwards
// the engine's post-master-gain /omniphony/meter/master when there is one, and
// reconstructs it from the speaker meters when there is not (older engine, or a
// backend that has not been rebuilt) — see `derived_master_meter` in
// src-tauri/src/osc_listener.rs. Either way `masterLevel` is populated, so
// there is nothing to fall back to here.
//
// Returns { peakDb, rmsDb }, or null before the first meter arrives.
function getMasterMeter() {
  if (!masterLevel || typeof masterLevel.rmsDbfs !== 'number') {
    return null;
  }
  const rmsDb = masterLevel.rmsDbfs;
  const peakDb = typeof masterLevel.peakDbfs === 'number' ? masterLevel.peakDbfs : rmsDb;
  return {
    rmsDb,
    peakDb,
    holdDb: typeof masterLevel.peakHoldDbfs === 'number' ? masterLevel.peakHoldDbfs : peakDb
  };
}

export function updateMasterMeterUI() {
  const masterMeterTextEl = getMasterMeterTextEl();
  const masterMeterFillEl = getMasterMeterFillEl();
  const masterMeterPeakEl = inAudioPanel('masterMeterPeak');

  if (!masterMeterTextEl || !masterMeterFillEl) return;

  const level = getMasterMeter();
  if (!level) {
    masterMeterTextEl.textContent = t('status.masterMeter');
    masterMeterFillEl.style.setProperty('--level', '0%');
    if (masterMeterPeakEl) {
      masterMeterPeakEl.style.opacity = '0';
      masterMeterPeakEl.classList.remove('over');
    }
    return;
  }

  const rmsDb = level.rmsDb;
  const peakDb = level.peakDb;
  // Bar follows the peak (same quantity as the hold cursor) so they meet on
  // transients; RMS stays as the numeric readout.
  const levelPercent = dbToMeterPercent(peakDb);
  masterMeterFillEl.style.setProperty('--level', `${levelPercent.toFixed(1)}%`);

  masterMeterTextEl.textContent = `${formatNumber(rmsDb, 1)} dB`;

  if (masterMeterPeakEl) {
    // Hold and decay come from the backend, same as every other meter.
    const holdPercent = dbToMeterPercent(level.holdDb);
    masterMeterPeakEl.style.setProperty('--level', `${holdPercent.toFixed(1)}%`);
    masterMeterPeakEl.style.opacity = holdPercent > 0.1 ? '1' : '0';
    masterMeterPeakEl.classList.toggle('over', level.holdDb >= 0);
  }
}

// ---------------------------------------------------------------------------
// Loudness display
// ---------------------------------------------------------------------------

import { updateDrcSummary } from './drc.js';

export function renderLoudnessDisplay() {
  const loudnessInfoEl = getLoudnessInfoEl();
  const loudnessToggleEl = getLoudnessToggleEl();
  if (!loudnessInfoEl) return;
  const sourceText = app.loudnessSource === null ? '—' : `${formatNumber(app.loudnessSource, 0)} dBFS`;
  const correctionDbValue =
    app.loudnessGain === null || Number(app.loudnessGain) <= 0
      ? null
      : linearToDb(app.loudnessGain);
  const targetValue =
    app.loudnessSource !== null && correctionDbValue !== null
      ? app.loudnessSource + correctionDbValue
      : null;
  const targetText = targetValue === null ? '—' : `${formatNumber(targetValue, 0)} dBFS`;
  const gainText =
    app.loudnessGain === null
      ? '—'
      : `${formatNumber(app.loudnessGain, 2)} (${formatLinearAsDb(app.loudnessGain)})`;
  loudnessInfoEl.innerHTML = [
    `source loudness: ${sourceText}`,
    `target loudness: ${targetText}`,
    `correction: ${gainText}`
  ].join('<br>');
  if (loudnessToggleEl) {
    loudnessToggleEl.checked = app.loudnessEnabled === true;
  }
  updateDrcSummary();
}

export function updateLoudnessDisplay() {
  dirty.loudness = true;
  scheduleUIFlush();
}

// ---------------------------------------------------------------------------
// Distance model display
// ---------------------------------------------------------------------------

export function renderDistanceModelUI() {
  const distanceModelSelectEl = getDistanceModelSelectEl();
  if (distanceModelSelectEl) {
    distanceModelSelectEl.value = ['none', 'linear', 'quadratic', 'inverse-square'].includes(app.distanceModel)
      ? app.distanceModel
      : 'none';
  }
  const distanceModelMetricSelectEl = inRendererPanel('distanceModelMetricSelect');
  if (distanceModelMetricSelectEl) {
    distanceModelMetricSelectEl.value = ['spherical', 'chebyshev'].includes(app.distanceModelMetric)
      ? app.distanceModelMetric
      : 'spherical';
  }
  // The metric is irrelevant with no attenuation, so hide it when model is none.
  const distanceModelMetricRowEl = inRendererPanel('distanceModelMetricRow');
  if (distanceModelMetricRowEl) {
    distanceModelMetricRowEl.style.display = app.distanceModel === 'none' ? 'none' : '';
  }
}

export function updateDistanceModelUI() {
  dirty.distanceModel = true;
  scheduleUIFlush();
}
