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

// DOM refs
const latencyInfoEl = document.getElementById('latencyInfo');
const latencyRawInfoEl = document.getElementById('latencyRawInfo');
const latencyCtrlInfoEl = document.getElementById('latencyCtrlInfo');
const latencyTargetInputEl = document.getElementById('latencyTargetInput');
const latencyTargetApplyBtnEl = document.getElementById('latencyTargetApplyBtn');
const resampleRatioInfoEl = document.getElementById('resampleRatioInfo');
const resampleMeterRowEl = document.getElementById('resampleMeterRow');
const resampleNegMeterFillEl = document.getElementById('resampleNegMeterFill');
const resamplePosMeterFillEl = document.getElementById('resamplePosMeterFill');
const resampleNegFarMarkerEl = document.getElementById('resampleNegFarMarker');
const resamplePosFarMarkerEl = document.getElementById('resamplePosFarMarker');
const resampleNegNearMarkerEl = document.getElementById('resampleNegNearMarker');
const resamplePosNearMarkerEl = document.getElementById('resamplePosNearMarker');
const latencyMeterFillEl = document.getElementById('latencyMeterFill');
const latencyRawMinMaskEl = document.getElementById('latencyRawMinMask');
const latencyRawMaxMaskEl = document.getElementById('latencyRawMaxMask');
const latencyRawMinMarkerEl = document.getElementById('latencyRawMinMarker');
const latencyCtrlMarkerEl = document.getElementById('latencyCtrlMarker');
const latencyRawMaxMarkerEl = document.getElementById('latencyRawMaxMarker');
const latencyTargetMarkerEl = document.getElementById('latencyTargetMarker');
const latencyNearLowMarkerEl = document.getElementById('latencyNearLowMarker');
const latencyNearHighMarkerEl = document.getElementById('latencyNearHighMarker');
const latencyRawMinValueEl = document.getElementById('latencyRawMinValue');
const latencyRawMaxValueEl = document.getElementById('latencyRawMaxValue');
const rendererPerfWrapEl = document.getElementById('rendererPerfWrap');
const rendererPerfDecodeFillEl = document.getElementById('rendererPerfDecodeFill');
const rendererPerfRenderFillEl = document.getElementById('rendererPerfRenderFill');
const rendererPerfWriteFillEl = document.getElementById('rendererPerfWriteFill');
const rendererPerfDecodeMaxMarkerEl = document.getElementById('rendererPerfDecodeMaxMarker');
const rendererPerfRenderMaxMarkerEl = document.getElementById('rendererPerfRenderMaxMarker');
const rendererPerfWriteMaxMarkerEl = document.getElementById('rendererPerfWriteMaxMarker');
const rendererPerfDecodeValueEl = document.getElementById('rendererPerfDecodeValue');
const rendererPerfRenderValueEl = document.getElementById('rendererPerfRenderValue');
const rendererPerfWriteValueEl = document.getElementById('rendererPerfWriteValue');
const rendererPerfFrameValueEl = document.getElementById('rendererPerfFrameValue');

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

export function setFrameDurationMs(value) {
  const next = Number(value);
  if (!Number.isFinite(next) || next <= 0) {
    return;
  }
  app.frameDurationMs = next;
}

// ── Latency display ───────────────────────────────────────────────────────

export function renderLatencyDisplay() {
  if (!latencyRawInfoEl && !latencyCtrlInfoEl && !latencyInfoEl) return;
  const instantText = app.latencyInstantMs === null ? '—' : `${formatNumber(app.latencyInstantMs, 0)} ms`;
  const controlText = app.latencyControlMs === null ? '—' : `${formatNumber(app.latencyControlMs, 0)} ms`;
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
    const targetValue = app.latencyTargetMs ?? app.latencyMs;
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
  if (!resampleRatioInfoEl) return;
  if (app.adaptiveResamplingEnabled !== true) {
    resampleRatioInfoEl.style.display = 'none';
    if (resampleMeterRowEl) resampleMeterRowEl.style.display = 'none';
    return;
  }
  resampleRatioInfoEl.style.display = '';
  if (resampleMeterRowEl) resampleMeterRowEl.style.display = '';
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
  const farBound = Math.max(0.000001, Number(app.adaptiveResamplingMaxAdjustFar) || 0.000001);
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
  const maxMs = 2000;
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
  const visible = app.oscMeteringEnabled === true;
  if (rendererPerfWrapEl) {
    rendererPerfWrapEl.style.display = visible ? 'block' : 'none';
  }
  if (!visible) {
    return;
  }
  const dec = Math.max(0, Number(app.decodeTimeMs) || 0);
  const rnd = Math.max(0, Number(app.renderTimeMs) || 0);
  const wri = Math.max(0, Number(app.writeTimeMs) || 0);
  const decMax = app.decodeTimeWindow.length > 0 ? Math.max(...app.decodeTimeWindow.map((entry) => entry.v)) : null;
  const rndMax = app.renderTimeWindow.length > 0 ? Math.max(...app.renderTimeWindow.map((entry) => entry.v)) : null;
  const wriMax = app.writeTimeWindow.length > 0 ? Math.max(...app.writeTimeWindow.map((entry) => entry.v)) : null;
  const frameBudgetMs = Number(app.frameDurationMs);
  const scaleMs = Number.isFinite(frameBudgetMs) && frameBudgetMs > 0
    ? frameBudgetMs
    : Math.max(0.01, dec + rnd + wri, (decMax ?? 0) + (rndMax ?? 0) + (wriMax ?? 0));
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
  setSegment(rendererPerfRenderFillEl, dec, dec + rnd);
  setSegment(rendererPerfWriteFillEl, dec + rnd, dec + rnd + wri);

  setMarker(rendererPerfDecodeMaxMarkerEl, decMax);
  setMarker(rendererPerfRenderMaxMarkerEl, decMax === null && rndMax === null ? null : (decMax ?? 0) + (rndMax ?? 0));
  setMarker(rendererPerfWriteMaxMarkerEl, decMax === null && rndMax === null && wriMax === null ? null : (decMax ?? 0) + (rndMax ?? 0) + (wriMax ?? 0));

  if (rendererPerfDecodeValueEl) rendererPerfDecodeValueEl.textContent = `decode ${app.decodeTimeMs === null ? '—' : `${formatNumber(dec, 3)} ms`}`;
  if (rendererPerfRenderValueEl) rendererPerfRenderValueEl.textContent = `render ${app.renderTimeMs === null ? '—' : `${formatNumber(rnd, 3)} ms`}`;
  if (rendererPerfWriteValueEl) rendererPerfWriteValueEl.textContent = `write ${app.writeTimeMs === null ? '—' : `${formatNumber(wri, 3)} ms`}`;
  if (rendererPerfFrameValueEl) rendererPerfFrameValueEl.textContent = `frame ${app.frameDurationMs === null ? '—' : `${formatNumber(frameBudgetMs, 3)} ms`}`;
}

export function updateRenderTimeUI() {
  dirty.renderTime = true;
  scheduleUIFlush();
}

export function applyLatencyTargetNow() {
  const requested = Math.max(1, Math.round(Number(latencyTargetInputEl?.value) || 0));
  app.latencyTargetMs = requested;
  app.latencyMs = requested;
  app.latencyTargetDirty = false;
  app.latencyTargetEditing = false;
  updateLatencyDisplay();
  updateLatencyMeterUI();
  invoke('control_latency_target', { value: requested });
}
