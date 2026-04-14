/**
 * Master gain and master meter controls.
 *
 * Extracted from app.js (lines 4547-4590).
 */

import { app, dirty, speakerMeshes, speakerLevels } from '../state.js';
import { t, tf } from '../i18n.js';
import { formatNumber } from '../coordinates.js';
import { linearToDb, dbToLinear } from '../mute-solo.js';
import { scheduleUIFlush } from '../flush.js';
import { inAudioPanel, inRendererPanel } from '../ui/panel-roots.js';

function getMasterGainSliderEl() { return inAudioPanel('masterGainSlider'); }
function getMasterGainBoxEl() { return inAudioPanel('masterGainBox'); }
function getMasterMeterTextEl() { return inAudioPanel('masterMeterText'); }
function getMasterMeterFillEl() { return inAudioPanel('masterMeterFill'); }
function getLoudnessInfoEl() { return inAudioPanel('loudnessInfo'); }
function getLoudnessToggleEl() { return inAudioPanel('loudnessToggle'); }
function getDistanceModelSelectEl() { return inRendererPanel('distanceModelSelect'); }

export function renderMasterGainUI() {
  const masterGainSliderEl = getMasterGainSliderEl();
  const masterGainBoxEl = getMasterGainBoxEl();
  if (masterGainSliderEl) {
    const hasValue = Number.isFinite(app.masterGain) && app.masterGain > 0;
    masterGainSliderEl.disabled = !app.oscSnapshotReady || !hasValue;
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
  const levels = speakerMeshes.length
    ? speakerMeshes.map((_, index) => speakerLevels.get(String(index)))
    : [];
  const valid = levels.filter((meter) => meter && typeof meter.rmsDbfs === 'number');
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
  if (!masterMeterTextEl || !masterMeterFillEl) return;
  const avgDb = getAverageSpeakerRmsDb();
  if (avgDb === null) {
    masterMeterTextEl.textContent = t('status.masterMeter');
    masterMeterFillEl.style.setProperty('--level', '0%');
    return;
  }
  masterMeterTextEl.textContent = `${formatNumber(avgDb, 1)} dB`;
  const percent = ((avgDb + 100) / 100) * 100;
  masterMeterFillEl.style.setProperty('--level', `${percent.toFixed(1)}%`);
}

// ---------------------------------------------------------------------------
// Loudness display
// ---------------------------------------------------------------------------

export function renderLoudnessDisplay() {
  const loudnessInfoEl = getLoudnessInfoEl();
  const loudnessToggleEl = getLoudnessToggleEl();
  if (!loudnessInfoEl) return;
  const enabledText = app.loudnessEnabled === null ? '—' : app.loudnessEnabled ? t('loudness.on') : t('loudness.off');
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
  loudnessInfoEl.textContent = tf('loudness.template', {
    source: sourceText,
    target: targetText,
    gain: gainText,
    enabled: enabledText
  });
  if (loudnessToggleEl) {
    loudnessToggleEl.checked = app.loudnessEnabled === true;
  }
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
  if (!distanceModelSelectEl) return;
  distanceModelSelectEl.value = ['none', 'linear', 'quadratic', 'inverse-square'].includes(app.distanceModel)
    ? app.distanceModel
    : 'none';
}

export function updateDistanceModelUI() {
  dirty.distanceModel = true;
  scheduleUIFlush();
}
