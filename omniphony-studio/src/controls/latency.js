/**
 * Latency, render-time, and resample-ratio controls.
 *
 * Extracted from app.js:
 *   - lines 1092-1154: setLatencyInstantMs, setRenderTimeMs, setDecodeTimeMs,
 *     setWriteTimeMs, setFrameDurationMs
 *   - lines 4201-4294: renderLatencyDisplay, updateLatencyDisplay,
 *     renderResampleRatioDisplay, updateResampleRatioDisplay
 *   - lines 4379-4545: renderLatencyMeterUI, updateLatencyMeterUI,
 *     renderRenderTimeUI, updateRenderTimeUI
 */

import { invoke } from '@tauri-apps/api/core';
import { app, dirty, LATENCY_RAW_WINDOW_MS, RENDER_TIME_WINDOW_MS } from '../state.js';
import { t, tf } from '../i18n.js';
import { formatNumber } from '../coordinates.js';
import { scheduleUIFlush } from '../flush.js';
import { inAudioPanel, inRendererPanel } from '../ui/panel-roots.js';

const RENDER_TIME_AVERAGE_WINDOW_MS = 1000;
const RENDER_TIME_DISPLAY_HOLD_MS = 350;

const rendererPerfDisplayState = {
  decode: { value: null, updatedAt: 0 },
  render: { value: null, updatedAt: 0 },
  crossover: { value: null, updatedAt: 0 },
  write: { value: null, updatedAt: 0 },
};

function getLatencyInfoEl() { return inAudioPanel('latencyInfo'); }
function getLatencyRawInfoEl() { return inAudioPanel('latencyRawInfo'); }
function getLatencyCtrlInfoEl() { return inAudioPanel('latencyCtrlInfo'); }
function getLatencyTargetInputEl() { return inAudioPanel('latencyTargetInput'); }
function getLatencyTargetApplyBtnEl() { return inAudioPanel('latencyTargetApplyBtn'); }
function getResampleRatioInfoEl() { return inAudioPanel('resampleRatioInfo'); }
function getResampleMeterLabelEl() { return inAudioPanel('resampleMeterLabel'); }
function getResampleMeterBodyEl() { return inAudioPanel('resampleMeterBody'); }
function getResampleNegMeterFillEl() { return inAudioPanel('resampleNegMeterFill'); }
function getResamplePosMeterFillEl() { return inAudioPanel('resamplePosMeterFill'); }
function getResampleNegFarMarkerEl() { return inAudioPanel('resampleNegFarMarker'); }
function getResamplePosFarMarkerEl() { return inAudioPanel('resamplePosFarMarker'); }
function getResampleNegNearMarkerEl() { return inAudioPanel('resampleNegNearMarker'); }
function getResamplePosNearMarkerEl() { return inAudioPanel('resamplePosNearMarker'); }
function getLatencyMeterFillEl() { return inAudioPanel('latencyMeterFill'); }
function getLatencyRawMinMaskEl() { return inAudioPanel('latencyRawMinMask'); }
function getLatencyRawMaxMaskEl() { return inAudioPanel('latencyRawMaxMask'); }
function getLatencyRawMinMarkerEl() { return inAudioPanel('latencyRawMinMarker'); }
function getLatencyCtrlMarkerEl() { return inAudioPanel('latencyCtrlMarker'); }
function getLatencyRawMaxMarkerEl() { return inAudioPanel('latencyRawMaxMarker'); }
function getLatencyTargetMarkerEl() { return inAudioPanel('latencyTargetMarker'); }
function getLatencyNearLowMarkerEl() { return inAudioPanel('latencyNearLowMarker'); }
function getLatencyNearHighMarkerEl() { return inAudioPanel('latencyNearHighMarker'); }
function getLatencyRawMinValueEl() { return inAudioPanel('latencyRawMinValue'); }
function getLatencyRawMaxValueEl() { return inAudioPanel('latencyRawMaxValue'); }
function getRendererPerfWrapEl() { return inRendererPanel('rendererPerfWrap'); }
function getRendererPerfDecodeFillEl() { return inRendererPanel('rendererPerfDecodeFill'); }
function getRendererPerfRenderFillEl() { return inRendererPanel('rendererPerfRenderFill'); }
function getRendererPerfCrossoverFillEl() { return inRendererPanel('rendererPerfCrossoverFill'); }
function getRendererPerfWriteFillEl() { return inRendererPanel('rendererPerfWriteFill'); }
function getRendererPerfDecodeMaxMarkerEl() { return inRendererPanel('rendererPerfDecodeMaxMarker'); }
function getRendererPerfRenderMaxMarkerEl() { return inRendererPanel('rendererPerfRenderMaxMarker'); }
function getRendererPerfCrossoverMaxMarkerEl() { return inRendererPanel('rendererPerfCrossoverMaxMarker'); }
function getRendererPerfWriteMaxMarkerEl() { return inRendererPanel('rendererPerfWriteMaxMarker'); }
function getRendererPerfDecodeValueEl() { return inRendererPanel('rendererPerfDecodeValue'); }
function getRendererPerfRenderValueEl() { return inRendererPanel('rendererPerfRenderValue'); }
function getRendererPerfCrossoverValueEl() { return inRendererPanel('rendererPerfCrossoverValue'); }
function getRendererPerfWriteValueEl() { return inRendererPanel('rendererPerfWriteValue'); }
function getRendererPerfDecodeMaxValueEl() { return inRendererPanel('rendererPerfDecodeMaxValue'); }
function getRendererPerfRenderMaxValueEl() { return inRendererPanel('rendererPerfRenderMaxValue'); }
function getRendererPerfCrossoverMaxValueEl() { return inRendererPanel('rendererPerfCrossoverMaxValue'); }
function getRendererPerfWriteMaxValueEl() { return inRendererPanel('rendererPerfWriteMaxValue'); }
function getRendererPerfFrameValueEl() { return inRendererPanel('rendererPerfFrameValue'); }

// ── Latency / timing setters ──────────────────────────────────────────────

export function setLatencyInstantMs(value) {
  const next = Number(value);
  if (!Number.isFinite(next)) {
    return;
  }
  app.latencyInstantMs = next;
  const now = performance.now();
  app.latencyRawWindow.push({ t: now, v: next });
  const cutoff = now - LATENCY_RAW_WINDOW_MS;
  while (app.latencyRawWindow.length > 0 && app.latencyRawWindow[0].t < cutoff) {
    app.latencyRawWindow.shift();
  }
}

export function setRenderTimeMs(value) {
  const next = Number(value);
  if (!Number.isFinite(next)) {
    return;
  }
  app.renderTimeMs = next;
  const now = performance.now();
  app.renderTimeWindow.push({ t: now, v: next });
  const cutoff = now - RENDER_TIME_WINDOW_MS;
  while (app.renderTimeWindow.length > 0 && app.renderTimeWindow[0].t < cutoff) {
    app.renderTimeWindow.shift();
  }
}

export function setDecodeTimeMs(value) {
  const next = Number(value);
  if (!Number.isFinite(next)) {
    return;
  }
  app.decodeTimeMs = next;
  const now = performance.now();
  app.decodeTimeWindow.push({ t: now, v: next });
  const cutoff = now - RENDER_TIME_WINDOW_MS;
  while (app.decodeTimeWindow.length > 0 && app.decodeTimeWindow[0].t < cutoff) {
    app.decodeTimeWindow.shift();
  }
}

export function setWriteTimeMs(value) {
  const next = Number(value);
  if (!Number.isFinite(next)) {
    return;
  }
  app.writeTimeMs = next;
  const now = performance.now();
  app.writeTimeWindow.push({ t: now, v: next });
  const cutoff = now - RENDER_TIME_WINDOW_MS;
  while (app.writeTimeWindow.length > 0 && app.writeTimeWindow[0].t < cutoff) {
    app.writeTimeWindow.shift();
  }
}

export function setCrossoverTimeMs(value) {
  const next = Number(value);
  if (!Number.isFinite(next)) {
    return;
  }
  app.crossoverTimeMs = next;
  const now = performance.now();
  app.crossoverTimeWindow.push({ t: now, v: next });
  const cutoff = now - RENDER_TIME_WINDOW_MS;
  while (app.crossoverTimeWindow.length > 0 && app.crossoverTimeWindow[0].t < cutoff) {
    app.crossoverTimeWindow.shift();
  }
}

export function setFrameDurationMs(value) {
  const next = Number(value);
  if (!Number.isFinite(next) || next <= 0) {
    return;
  }
  app.frameDurationMs = next;
}

function averageRecent(window, now, spanMs) {
  const entries = window.filter((entry) => now - entry.t <= spanMs);
  if (entries.length === 0) {
    return null;
  }
  const sum = entries.reduce((acc, entry) => acc + entry.v, 0);
  return sum / entries.length;
}

function getStableRenderPerfValue(key, nextValue, now) {
  const state = rendererPerfDisplayState[key];
  if (!Number.isFinite(nextValue)) {
    state.value = null;
    state.updatedAt = now;
    return null;
  }
  if (state.value === null || now - state.updatedAt >= RENDER_TIME_DISPLAY_HOLD_MS) {
    state.value = nextValue;
    state.updatedAt = now;
  }
  return state.value;
}

// ── Latency display ───────────────────────────────────────────────────────

export function renderLatencyDisplay() {
  const latencyInfoEl = getLatencyInfoEl();
  const latencyRawInfoEl = getLatencyRawInfoEl();
  const latencyCtrlInfoEl = getLatencyCtrlInfoEl();
  const latencyTargetInputEl = getLatencyTargetInputEl();
  const latencyTargetApplyBtnEl = getLatencyTargetApplyBtnEl();
  if (!latencyRawInfoEl && !latencyCtrlInfoEl && !latencyInfoEl) return;
  const instantText = app.latencyInstantMs === null ? '—' : `${formatNumber(app.latencyInstantMs, 0)} ms`;
  const controlText = app.latencyControlMs === null ? '—' : `${formatNumber(app.latencyControlMs, 0)} ms`;
  const targetValue = app.latencyRequestedMs ?? app.latencyTargetMs ?? app.latencyMs;
  if (latencyRawInfoEl) {
    latencyRawInfoEl.textContent = instantText;
  }
  if (latencyCtrlInfoEl) {
    latencyCtrlInfoEl.textContent = controlText;
  }
  if (!latencyRawInfoEl && latencyInfoEl) {
    latencyInfoEl.textContent = tf('status.latencyFallback', { raw: instantText, ctrl: controlText });
  }
  if (latencyTargetInputEl && !app.latencyTargetEditing && !app.latencyTargetDirty) {
    latencyTargetInputEl.value = targetValue === null ? '' : String(Math.max(1, Math.round(targetValue)));
  }
  if (latencyTargetApplyBtnEl) {
    const enabled = app.latencyTargetDirty;
    latencyTargetApplyBtnEl.disabled = !enabled;
    latencyTargetApplyBtnEl.style.opacity = enabled ? '1' : '0.4';
    latencyTargetApplyBtnEl.style.cursor = enabled ? 'pointer' : 'default';
  }
}

export function updateLatencyDisplay() {
  dirty.latency = true;
  scheduleUIFlush();
}

// ── Resample ratio display ────────────────────────────────────────────────

export function renderResampleRatioDisplay() {
  const resampleRatioInfoEl = getResampleRatioInfoEl();
  const resampleMeterLabelEl = getResampleMeterLabelEl();
  const resampleMeterBodyEl = getResampleMeterBodyEl();
  const resampleNegMeterFillEl = getResampleNegMeterFillEl();
  const resamplePosMeterFillEl = getResamplePosMeterFillEl();
  const resampleNegFarMarkerEl = getResampleNegFarMarkerEl();
  const resamplePosFarMarkerEl = getResamplePosFarMarkerEl();
  const resampleNegNearMarkerEl = getResampleNegNearMarkerEl();
  const resamplePosNearMarkerEl = getResamplePosNearMarkerEl();
  if (!resampleRatioInfoEl) return;
  if (app.adaptiveResamplingEnabled !== true) {
    resampleRatioInfoEl.style.display = 'none';
    if (resampleMeterLabelEl) resampleMeterLabelEl.style.display = 'none';
    if (resampleMeterBodyEl) resampleMeterBodyEl.style.display = 'none';
    return;
  }
  resampleRatioInfoEl.style.display = '';
  if (resampleMeterLabelEl) resampleMeterLabelEl.style.display = '';
  if (resampleMeterBodyEl) resampleMeterBodyEl.style.display = 'grid';
  const farModeEnabled = app.adaptiveResamplingEnableFarMode === true;
  if (app.resampleRatio === null) {
    resampleRatioInfoEl.textContent = '—';
    if (resampleNegMeterFillEl) resampleNegMeterFillEl.style.clipPath = 'inset(0 50% 0 50%)';
    if (resamplePosMeterFillEl) resamplePosMeterFillEl.style.clipPath = 'inset(0 50% 0 50%)';
    if (resampleNegFarMarkerEl) resampleNegFarMarkerEl.style.display = 'none';
    if (resamplePosFarMarkerEl) resamplePosFarMarkerEl.style.display = 'none';
    if (resampleNegNearMarkerEl) resampleNegNearMarkerEl.style.display = 'none';
    if (resamplePosNearMarkerEl) resamplePosNearMarkerEl.style.display = 'none';
    return;
  }

  const ppm = Math.round((app.resampleRatio - 1.0) * 1e6);
  const sign = ppm >= 0 ? '+' : '';
  resampleRatioInfoEl.textContent = `${sign}${ppm} ppm`;
  const farBound = Math.max(0.000001, Number(app.adaptiveResamplingMaxAdjust) || 0.000001);
  const nearBound = Math.max(0, Math.min(farBound, Number(app.adaptiveResamplingMaxAdjust) || 0));
  const deviation = Number(app.resampleRatio) - 1.0;
  const normalizedMagnitude = Math.min(1, Math.abs(deviation) / farBound);
  const magnitude = normalizedMagnitude * 50;
  if (resampleNegMeterFillEl) {
    if (deviation < 0) {
      resampleNegMeterFillEl.style.clipPath = `inset(0 50% 0 ${Number((50 - magnitude).toFixed(1))}%)`;
    } else {
      resampleNegMeterFillEl.style.clipPath = 'inset(0 50% 0 50%)';
    }
  }
  if (resamplePosMeterFillEl) {
    if (deviation > 0) {
      resamplePosMeterFillEl.style.clipPath = `inset(0 ${Number((50 - magnitude).toFixed(1))}% 0 50%)`;
    } else {
      resamplePosMeterFillEl.style.clipPath = 'inset(0 50% 0 50%)';
    }
  }
  const nearPercent = 50 + (nearBound / farBound) * 50;
  const negNearPercent = 50 - (nearBound / farBound) * 50;
  if (resampleNegFarMarkerEl) {
    resampleNegFarMarkerEl.style.display = '';
    resampleNegFarMarkerEl.style.left = 'calc(0% - 1px)';
    resampleNegFarMarkerEl.style.opacity = farModeEnabled ? '1' : '0.28';
  }
  if (resamplePosFarMarkerEl) {
    resamplePosFarMarkerEl.style.display = '';
    resamplePosFarMarkerEl.style.left = 'calc(100% - 1px)';
    resamplePosFarMarkerEl.style.opacity = farModeEnabled ? '1' : '0.28';
  }
  if (resampleNegNearMarkerEl) {
    resampleNegNearMarkerEl.style.display = '';
    resampleNegNearMarkerEl.style.left = `calc(${negNearPercent.toFixed(1)}% - 1px)`;
  }
  if (resamplePosNearMarkerEl) {
    resamplePosNearMarkerEl.style.display = '';
    resamplePosNearMarkerEl.style.left = `calc(${nearPercent.toFixed(1)}% - 1px)`;
  }
}

export function updateResampleRatioDisplay() {
  dirty.resample = true;
  scheduleUIFlush();
}

// ── Latency meter UI ──────────────────────────────────────────────────────

export function renderLatencyMeterUI() {
  const latencyMeterFillEl = getLatencyMeterFillEl();
  const latencyRawMinMaskEl = getLatencyRawMinMaskEl();
  const latencyRawMaxMaskEl = getLatencyRawMaxMaskEl();
  const latencyRawMinMarkerEl = getLatencyRawMinMarkerEl();
  const latencyCtrlMarkerEl = getLatencyCtrlMarkerEl();
  const latencyRawMaxMarkerEl = getLatencyRawMaxMarkerEl();
  const latencyTargetMarkerEl = getLatencyTargetMarkerEl();
  const latencyNearLowMarkerEl = getLatencyNearLowMarkerEl();
  const latencyNearHighMarkerEl = getLatencyNearHighMarkerEl();
  const latencyRawMinValueEl = getLatencyRawMinValueEl();
  const latencyRawMaxValueEl = getLatencyRawMaxValueEl();
  const targetForScale = app.latencyRequestedMs ?? app.latencyTargetMs ?? app.latencyMs;
  const maxMs = targetForScale === null
    ? 2000
    : Math.max(100, Number(targetForScale) * 2);
  const farModeEnabled = app.adaptiveResamplingEnableFarMode === true;
  const setThresholdDot = (el, valueMs) => {
    if (!el) return;
    if (valueMs === null || !Number.isFinite(valueMs)) {
      el.style.display = 'none';
      return;
    }
    const clamped = Math.min(maxMs, Math.max(0, Number(valueMs)));
    const percent = (clamped / maxMs) * 100;
    el.style.display = '';
    el.style.left = `calc(${percent.toFixed(1)}% - 2px)`;
  };
  if (latencyMeterFillEl) {
    const raw = app.latencyInstantMs ?? app.latencyTargetMs ?? app.latencyMs;
    if (raw === null) {
      latencyMeterFillEl.style.setProperty('--level', '0%');
    } else {
      const percent = Math.min(100, (Math.max(0, Number(raw)) / maxMs) * 100);
      latencyMeterFillEl.style.setProperty('--level', `${percent.toFixed(1)}%`);
    }
  }
  const rawMin =
    app.latencyRawWindow.length > 0 ? Math.min(...app.latencyRawWindow.map((entry) => entry.v)) : null;
  const rawMax =
    app.latencyRawWindow.length > 0 ? Math.max(...app.latencyRawWindow.map((entry) => entry.v)) : null;
  if (latencyRawMinMaskEl) {
    if (rawMin === null || rawMax === null || rawMax < rawMin) {
      latencyRawMinMaskEl.style.display = 'none';
    } else {
      const percent = Math.min(100, (Math.max(0, rawMin) / maxMs) * 100);
      latencyRawMinMaskEl.style.display = '';
      latencyRawMinMaskEl.style.width = `${percent.toFixed(1)}%`;
    }
  }
  if (latencyRawMaxMaskEl) {
    if (rawMin === null || rawMax === null || rawMax < rawMin) {
      latencyRawMaxMaskEl.style.display = 'none';
    } else {
      const percent = Math.min(100, (Math.max(0, rawMax) / maxMs) * 100);
      latencyRawMaxMaskEl.style.display = '';
      latencyRawMaxMaskEl.style.width = `${Math.max(0, 100 - percent).toFixed(1)}%`;
    }
  }
  if (latencyRawMinMarkerEl) {
    if (rawMin === null) {
      latencyRawMinMarkerEl.style.display = 'none';
    } else {
      const percent = Math.min(100, (Math.max(0, rawMin) / maxMs) * 100);
      latencyRawMinMarkerEl.style.display = '';
      latencyRawMinMarkerEl.style.left = `calc(${percent.toFixed(1)}% - 1px)`;
    }
  }
  if (latencyRawMinValueEl) {
    latencyRawMinValueEl.textContent = tf('status.minValue', { value: rawMin === null ? '—' : formatNumber(rawMin, 0) });
  }
  if (latencyRawMaxMarkerEl) {
    if (rawMax === null) {
      latencyRawMaxMarkerEl.style.display = 'none';
    } else {
      const percent = Math.min(100, (Math.max(0, rawMax) / maxMs) * 100);
      latencyRawMaxMarkerEl.style.display = '';
      latencyRawMaxMarkerEl.style.left = `calc(${percent.toFixed(1)}% - 1px)`;
    }
  }
  if (latencyRawMaxValueEl) {
    latencyRawMaxValueEl.textContent = tf('status.maxValue', { value: rawMax === null ? '—' : formatNumber(rawMax, 0) });
  }
  if (latencyCtrlMarkerEl) {
    const ctrl = app.latencyControlMs ?? app.latencyTargetMs ?? app.latencyMs;
    if (ctrl === null) {
      latencyCtrlMarkerEl.style.display = 'none';
    } else {
      const percent = Math.min(100, (Math.max(0, Number(ctrl)) / maxMs) * 100);
      latencyCtrlMarkerEl.style.display = '';
      latencyCtrlMarkerEl.style.left = `calc(${percent.toFixed(1)}% - 1px)`;
    }
  }
  if (latencyTargetMarkerEl) {
    const target = app.latencyTargetMs ?? app.latencyMs;
    if (target === null) {
      latencyTargetMarkerEl.style.display = 'none';
    } else {
      const percent = Math.min(100, (Math.max(0, Number(target)) / maxMs) * 100);
      latencyTargetMarkerEl.style.display = '';
      latencyTargetMarkerEl.style.left = `calc(${percent.toFixed(1)}% - 2px)`;
    }
  }
  const target = app.latencyTargetMs ?? app.latencyMs;
  const nearThreshold = Number(app.adaptiveResamplingNearFarThresholdMs);
  if (target === null || !Number.isFinite(Number(target))) {
    setThresholdDot(latencyNearLowMarkerEl, null);
    setThresholdDot(latencyNearHighMarkerEl, null);
  } else {
    const targetMs = Number(target);
    setThresholdDot(
      latencyNearLowMarkerEl,
      Number.isFinite(nearThreshold) ? targetMs - nearThreshold : null
    );
    setThresholdDot(
      latencyNearHighMarkerEl,
      Number.isFinite(nearThreshold) ? targetMs + nearThreshold : null
    );
  }
  if (latencyNearLowMarkerEl) latencyNearLowMarkerEl.style.opacity = farModeEnabled ? '1' : '0.28';
  if (latencyNearHighMarkerEl) latencyNearHighMarkerEl.style.opacity = farModeEnabled ? '1' : '0.28';
}

export function updateLatencyMeterUI() {
  dirty.latency = true;
  scheduleUIFlush();
}

// ── Render-time UI ────────────────────────────────────────────────────────

export function renderRenderTimeUI() {
  const rendererPerfWrapEl = getRendererPerfWrapEl();
  const rendererPerfDecodeFillEl = getRendererPerfDecodeFillEl();
  const rendererPerfRenderFillEl = getRendererPerfRenderFillEl();
  const rendererPerfCrossoverFillEl = getRendererPerfCrossoverFillEl();
  const rendererPerfWriteFillEl = getRendererPerfWriteFillEl();
  const rendererPerfDecodeMaxMarkerEl = getRendererPerfDecodeMaxMarkerEl();
  const rendererPerfRenderMaxMarkerEl = getRendererPerfRenderMaxMarkerEl();
  const rendererPerfCrossoverMaxMarkerEl = getRendererPerfCrossoverMaxMarkerEl();
  const rendererPerfWriteMaxMarkerEl = getRendererPerfWriteMaxMarkerEl();
  const rendererPerfDecodeValueEl = getRendererPerfDecodeValueEl();
  const rendererPerfRenderValueEl = getRendererPerfRenderValueEl();
  const rendererPerfCrossoverValueEl = getRendererPerfCrossoverValueEl();
  const rendererPerfWriteValueEl = getRendererPerfWriteValueEl();
  const rendererPerfDecodeMaxValueEl = getRendererPerfDecodeMaxValueEl();
  const rendererPerfRenderMaxValueEl = getRendererPerfRenderMaxValueEl();
  const rendererPerfCrossoverMaxValueEl = getRendererPerfCrossoverMaxValueEl();
  const rendererPerfWriteMaxValueEl = getRendererPerfWriteMaxValueEl();
  const rendererPerfFrameValueEl = getRendererPerfFrameValueEl();
  const visible = app.oscMeteringEnabled === true;
  if (rendererPerfWrapEl) {
    rendererPerfWrapEl.style.display = visible ? 'block' : 'none';
  }
  if (!visible) {
    return;
  }
  const dec = Math.max(0, Number(app.decodeTimeMs) || 0);
  const rndTotal = Math.max(0, Number(app.renderTimeMs) || 0);
  const cro = Math.min(rndTotal, Math.max(0, Number(app.crossoverTimeMs) || 0));
  const rnd = Math.max(0, rndTotal - cro);
  const wri = Math.max(0, Number(app.writeTimeMs) || 0);
  const decMax = app.decodeTimeWindow.length > 0 ? Math.max(...app.decodeTimeWindow.map((entry) => entry.v)) : null;
  const rndTotalMax = app.renderTimeWindow.length > 0 ? Math.max(...app.renderTimeWindow.map((entry) => entry.v)) : null;
  const croMax = app.crossoverTimeWindow.length > 0
    ? Math.min(rndTotalMax ?? Number.POSITIVE_INFINITY, Math.max(...app.crossoverTimeWindow.map((entry) => entry.v)))
    : null;
  const rndMax = rndTotalMax === null ? null : Math.max(0, rndTotalMax - (croMax ?? 0));
  const wriMax = app.writeTimeWindow.length > 0 ? Math.max(...app.writeTimeWindow.map((entry) => entry.v)) : null;
  const frameBudgetMs = Number(app.frameDurationMs);
  const scaleMs = Number.isFinite(frameBudgetMs) && frameBudgetMs > 0
    ? frameBudgetMs
    : Math.max(
      0.01,
      dec + cro + rnd + wri,
      (decMax ?? 0) + (croMax ?? 0) + (rndMax ?? 0) + (wriMax ?? 0)
    );
  const now = performance.now();
  const decAvg = averageRecent(app.decodeTimeWindow, now, RENDER_TIME_AVERAGE_WINDOW_MS);
  const rndTotalAvg = averageRecent(app.renderTimeWindow, now, RENDER_TIME_AVERAGE_WINDOW_MS);
  const croAvgRaw = averageRecent(app.crossoverTimeWindow, now, RENDER_TIME_AVERAGE_WINDOW_MS);
  const wriAvg = averageRecent(app.writeTimeWindow, now, RENDER_TIME_AVERAGE_WINDOW_MS);
  const croAvg = croAvgRaw === null
    ? null
    : Math.min(rndTotalAvg ?? Number.POSITIVE_INFINITY, croAvgRaw);
  const rndAvg = rndTotalAvg === null ? null : Math.max(0, rndTotalAvg - (croAvg ?? 0));
  const decShown = getStableRenderPerfValue('decode', decAvg, now);
  const rndShown = getStableRenderPerfValue('render', rndAvg, now);
  const croShown = getStableRenderPerfValue('crossover', croAvg, now);
  const wriShown = getStableRenderPerfValue('write', wriAvg, now);
  const setSegment = (el, startMs, endMs) => {
    if (!el) return;
    const left = Math.min(100, Math.max(0, (startMs / scaleMs) * 100));
    const right = Math.min(100, Math.max(0, 100 - ((endMs / scaleMs) * 100)));
    el.style.clipPath = `inset(0 ${right.toFixed(1)}% 0 ${left.toFixed(1)}%)`;
  };
  const setMarker = (el, valueMs) => {
    if (!el) return;
    if (valueMs === null || !Number.isFinite(valueMs)) {
      el.style.display = 'none';
      return;
    }
    const percent = Math.min(100, Math.max(0, (valueMs / scaleMs) * 100));
    el.style.display = '';
    el.style.left = `calc(${percent.toFixed(1)}% - 1px)`;
  };

  setSegment(rendererPerfDecodeFillEl, 0, dec);
  setSegment(rendererPerfCrossoverFillEl, dec, dec + cro);
  setSegment(rendererPerfRenderFillEl, dec + cro, dec + cro + rnd);
  setSegment(rendererPerfWriteFillEl, dec + cro + rnd, dec + cro + rnd + wri);

  setMarker(rendererPerfDecodeMaxMarkerEl, decMax);
  setMarker(
    rendererPerfCrossoverMaxMarkerEl,
    decMax === null && croMax === null ? null : (decMax ?? 0) + (croMax ?? 0)
  );
  setMarker(
    rendererPerfRenderMaxMarkerEl,
    decMax === null && croMax === null && rndMax === null
      ? null
      : (decMax ?? 0) + (croMax ?? 0) + (rndMax ?? 0)
  );
  setMarker(
    rendererPerfWriteMaxMarkerEl,
    decMax === null && croMax === null && rndMax === null && wriMax === null
      ? null
      : (decMax ?? 0) + (croMax ?? 0) + (rndMax ?? 0) + (wriMax ?? 0)
  );

  if (rendererPerfDecodeValueEl) {
    rendererPerfDecodeValueEl.textContent = tf('renderer.perf.decode', {
      value: decShown === null ? '—' : `${formatNumber(decShown, 3)} ms`
    });
  }
  if (rendererPerfRenderValueEl) {
    rendererPerfRenderValueEl.textContent = tf('renderer.perf.render', {
      value: rndShown === null ? '—' : `${formatNumber(rndShown, 3)} ms`
    });
  }
  if (rendererPerfCrossoverValueEl) {
    rendererPerfCrossoverValueEl.textContent = tf('renderer.perf.crossover', {
      value: croShown === null ? '—' : `${formatNumber(croShown, 3)} ms`
    });
  }
  if (rendererPerfWriteValueEl) {
    rendererPerfWriteValueEl.textContent = tf('renderer.perf.write', {
      value: wriShown === null ? '—' : `${formatNumber(wriShown, 3)} ms`
    });
  }
  if (rendererPerfDecodeMaxValueEl) {
    rendererPerfDecodeMaxValueEl.textContent = tf('renderer.perf.max', {
      value: decMax === null ? '—' : `${formatNumber(decMax, 3)} ms`
    });
  }
  if (rendererPerfRenderMaxValueEl) {
    rendererPerfRenderMaxValueEl.textContent = tf('renderer.perf.max', {
      value: rndMax === null ? '—' : `${formatNumber(rndMax, 3)} ms`
    });
  }
  if (rendererPerfCrossoverMaxValueEl) {
    rendererPerfCrossoverMaxValueEl.textContent = tf('renderer.perf.max', {
      value: croMax === null ? '—' : `${formatNumber(croMax, 3)} ms`
    });
  }
  if (rendererPerfWriteMaxValueEl) {
    rendererPerfWriteMaxValueEl.textContent = tf('renderer.perf.max', {
      value: wriMax === null ? '—' : `${formatNumber(wriMax, 3)} ms`
    });
  }
  if (rendererPerfFrameValueEl) {
    rendererPerfFrameValueEl.textContent = tf('renderer.perf.frame', {
      value: app.frameDurationMs === null ? '—' : `${formatNumber(frameBudgetMs, 3)} ms`
    });
  }
}

export function updateRenderTimeUI() {
  dirty.renderTime = true;
  scheduleUIFlush();
}

export function applyLatencyTargetNow() {
  const latencyTargetInputEl = getLatencyTargetInputEl();
  const requested = Math.max(1, Math.round(Number(latencyTargetInputEl?.value) || 0));
  app.latencyRequestedMs = requested;
  app.latencyTargetMs = requested;
  app.latencyMs = requested;
  app.latencyTargetDirty = false;
  app.latencyTargetEditing = false;
  updateLatencyDisplay();
  updateLatencyMeterUI();
  invoke('control_latency_target', { value: requested });
}
