/**
 * Master gain and master meter controls.
 *
 * Extracted from app.js (lines 4547-4590).
 */

import {
  app,
  dirty,
  speakerLevels,
  supportsRealtimeKey,
  masterPeak
} from '../state.js';
import { t, tf } from '../i18n.js';
import { formatNumber } from '../coordinates.js';
import { linearToDb, dbToLinear } from '../mute-solo.js';
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
    masterGainBoxEl.textContent = hasValue ? linearToDb(app.masterGain) : '—';
  }
}

export function updateMasterGainUI() {
  dirty.masterGain = true;
  scheduleUIFlush();
}

export function getAverageSpeakerRmsDb() {
  const valid = Array.from(speakerLevels.values()).filter(
    (meter) => meter && typeof meter.rmsDbfs === 'number'
  );
  if (valid.length === 0) {
    return null;
  }
  const sumLinear = valid.reduce((acc, meter) => acc + dbToLinear(meter.rmsDbfs), 0);
  const avgLinear = sumLinear / valid.length;
  const avgDb = 20 * Math.log10(Math.max(avgLinear, 1e-6));
  return Math.max(-100, Math.min(0, avgDb));
}

export function updateMasterMeterUI() {
  const masterMeterTextEl = getMasterMeterTextEl();
  const masterMeterFillEl = getMasterMeterFillEl();
  const masterMeterPeakEl = inAudioPanel('masterMeterPeak');

  if (!masterMeterTextEl || !masterMeterFillEl) return;
  const avgDb = getAverageSpeakerRmsDb();
  if (avgDb === null) {
    masterMeterTextEl.textContent = t('status.masterMeter');
    masterMeterFillEl.style.setProperty('--level', '0%');
    if (masterMeterPeakEl) masterMeterPeakEl.style.opacity = '0';
    return;
  }
  const levelPercent = ((avgDb + 100) / 100) * 100;
  masterMeterFillEl.style.setProperty('--level', `${levelPercent.toFixed(1)}%`);

  const now = Date.now();
  if (avgDb >= (masterPeak.db ?? -100) || now > masterPeak.expires) {
    if (avgDb >= (masterPeak.db ?? -100)) {
      masterPeak.db = avgDb;
      masterPeak.value = levelPercent;
      masterPeak.expires = now + 1000;
    } else {
      masterPeak.db = Math.max(avgDb, masterPeak.db - 2.0);
      masterPeak.value = ((Math.max(-100, masterPeak.db) + 100) / 100) * 100;
      if (masterPeak.value <= levelPercent + 0.1) masterPeak.expires = now + 1000;
    }
  }

  masterMeterTextEl.textContent = `${formatNumber(masterPeak.db ?? avgDb, 1)} dB`;

  if (masterMeterPeakEl) {
    masterMeterPeakEl.style.setProperty('--level', `${masterPeak.value.toFixed(1)}%`);
    masterMeterPeakEl.style.opacity = masterPeak.value > 0.1 ? '1' : '0';
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
      : 20 * Math.log10(Number(app.loudnessGain));
  const targetValue =
    app.loudnessSource !== null && correctionDbValue !== null
      ? app.loudnessSource + correctionDbValue
      : null;
  const targetText = targetValue === null ? '—' : `${formatNumber(targetValue, 0)} dBFS`;
  const gainText =
    app.loudnessGain === null
      ? '—'
      : `${formatNumber(app.loudnessGain, 2)} (${linearToDb(app.loudnessGain)})`;
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
