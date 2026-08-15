// Generic diagnostic-metrics plot. Reads the schema published by the renderer
// (app.diagSchema = { items: [{name, label, group, unit}, ...] }) and lets the
// user pick which metrics to trace via a multi-select chip row. Each selected
// metric is rendered in its own stacked panel with an independent y-scale.
//
// The renderer's DiagRegistry is the source of truth: adding a new metric on
// the Rust side instantly makes it available here without any change.
// User selections persist across reloads via localStorage.

import { app } from '../state.js';
import { invoke } from '@tauri-apps/api/core';

const POLL_INTERVAL_MS = 20;
const WINDOW_OPTIONS_MS = [5000, 10000, 30000, 60000];
const DEFAULT_WINDOW_MS = 10000;
const DIAG_RATE_OPTIONS_HZ = [10, 20, 50, 100, 200];
const DEFAULT_DIAG_RATE_HZ = 50;
const STORAGE_KEY = 'diagPlot.selectedMetrics.v1';
const WINDOW_STORAGE_KEY = 'diagPlot.windowMs.v1';
// Diag rate persisted separately from the audio-meter rate. Reads the older
// `diagPlot.meterRateHz.v1` key as a migration fallback so users who set a
// non-default value before the decoupling don't lose it on first run.
const DIAG_RATE_STORAGE_KEY = 'diagPlot.diagRateHz.v1';
const DIAG_RATE_LEGACY_STORAGE_KEY = 'diagPlot.meterRateHz.v1';
const FFT_STORAGE_KEY = 'diagPlot.fftMode.v1';
const DIFF_STORAGE_KEY = 'diagPlot.diffMode.v1';
// Active metric tab. The renderer tags each metric with a `tier`
// ("base" | "advanced"); the chip row shows only the metrics of the active
// tab. Defaults to "base" for the clear, obvious signals.
const TIER_STORAGE_KEY = 'diagPlot.tier.v1';
const DEFAULT_TIER = 'base';
const TIERS = ['base', 'advanced'];
// FFT panel: sample rate equals the plot poll rate (buffer is filled once per
// tick at POLL_INTERVAL_MS). Nyquist = 1000/(2·POLL_INTERVAL_MS). At 20 ms
// poll this gives 25 Hz, sufficient for the 3.1 Hz sawtooth investigation.
const FFT_SAMPLE_RATE_HZ = 1000 / POLL_INTERVAL_MS;
// Visible-range size below the peak. The Y axis is dynamic per-panel: top
// snaps a few dB above the peak, bottom = top − FFT_DB_SPAN. Bin amplitudes
// are absolute (dB rel. 1 unit of the metric) so two captures with different
// EMA settings are directly comparable.
const FFT_DB_SPAN = 60;
const FFT_DB_CLIP_FLOOR = -120;
// Hanning coherent gain (mean of the window). Used to compensate the per-bin
// magnitude so it matches the amplitude of an equivalent pure sinusoid.
const HANNING_COHERENT_GAIN = 0.5;
const PALETTE = [
  '#7ad7ff', '#9cffa3', '#ffb86b', '#d6a0ff',
  '#ff8da6', '#ffd166', '#a0e5ff', '#b6ff8c',
  '#ffa07a', '#c099ff', '#ff9bba', '#ffe88a',
];

let canvasEl = null;
let containerEl = null;
let controlsEl = null;
let toggleBtnEl = null;
let intervalId = null;
let visible = false;
let paused = false;
let pauseBtnEl = null;
let fftBtnEl = null;
let fftMode = loadFftMode();
let diffBtnEl = null;
let diffMode = loadDiffMode();
let activeTier = loadTier();
const buffer = [];
let selected = loadSelection();
let windowMs = loadWindowMs();
let diagRateHz = loadDiagRateHz();
let diagRateSelectEl = null;
let lastDiagEnabledKeepaliveAt = 0;
const DIAG_ENABLED_KEEPALIVE_MS = 1000;
let renderedSchemaSignature = null;
// Selection-rectangle measurement state. Active only while paused; the
// rectangle is anchored on the first panel it was started in (so the Y
// axis interpretation is unambiguous). lastRenderState carries the panel
// geometry/value range from the most recent render so overlay redraws
// can compute t and v from canvas pixel coordinates.
let selection = null;
let dragging = false;
let lastRenderState = null;
let selectionAttached = false;

function loadSelection() {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return new Set();
    const arr = JSON.parse(raw);
    return new Set(Array.isArray(arr) ? arr : []);
  } catch (_) {
    return new Set();
  }
}

function saveSelection() {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify([...selected]));
  } catch (_) { /* ignore */ }
}

function loadWindowMs() {
  try {
    const raw = localStorage.getItem(WINDOW_STORAGE_KEY);
    const n = raw === null ? NaN : Number.parseInt(raw, 10);
    if (Number.isFinite(n) && WINDOW_OPTIONS_MS.includes(n)) return n;
  } catch (_) { /* ignore */ }
  return DEFAULT_WINDOW_MS;
}

function saveWindowMs() {
  try {
    localStorage.setItem(WINDOW_STORAGE_KEY, String(windowMs));
  } catch (_) { /* ignore */ }
}

function loadDiagRateHz() {
  try {
    let raw = localStorage.getItem(DIAG_RATE_STORAGE_KEY);
    if (raw === null) raw = localStorage.getItem(DIAG_RATE_LEGACY_STORAGE_KEY);
    const n = raw === null ? NaN : Number.parseInt(raw, 10);
    if (Number.isFinite(n) && DIAG_RATE_OPTIONS_HZ.includes(n)) return n;
  } catch (_) { /* ignore */ }
  return DEFAULT_DIAG_RATE_HZ;
}

function saveDiagRateHz() {
  try {
    localStorage.setItem(DIAG_RATE_STORAGE_KEY, String(diagRateHz));
  } catch (_) { /* ignore */ }
}

function loadTier() {
  try {
    const raw = localStorage.getItem(TIER_STORAGE_KEY);
    if (TIERS.includes(raw)) return raw;
  } catch (_) { /* ignore */ }
  return DEFAULT_TIER;
}

function saveTier() {
  try {
    localStorage.setItem(TIER_STORAGE_KEY, activeTier);
  } catch (_) { /* ignore */ }
}

function loadFftMode() {
  try {
    return localStorage.getItem(FFT_STORAGE_KEY) === '1';
  } catch (_) {
    return false;
  }
}

function saveFftMode() {
  try {
    localStorage.setItem(FFT_STORAGE_KEY, fftMode ? '1' : '0');
  } catch (_) { /* ignore */ }
}

function loadDiffMode() {
  try {
    return localStorage.getItem(DIFF_STORAGE_KEY) === '1';
  } catch (_) {
    return false;
  }
}

function saveDiffMode() {
  try {
    localStorage.setItem(DIFF_STORAGE_KEY, diffMode ? '1' : '0');
  } catch (_) { /* ignore */ }
}

function getElements() {
  if (!canvasEl) canvasEl = document.getElementById('diagPlotCanvas');
  if (!containerEl) containerEl = document.getElementById('diagPlotContainer');
  if (!controlsEl) controlsEl = document.getElementById('diagPlotControls');
  if (!toggleBtnEl) toggleBtnEl = document.getElementById('diagPlotToggleBtn');
  return { canvasEl, containerEl, controlsEl, toggleBtnEl };
}

function schemaItems() {
  const schema = app.diagSchema;
  if (!schema || !Array.isArray(schema.items)) return [];
  // Hide internal sentinels (e.g. `_diag_alive`): underscore-prefixed metrics
  // are liveness/plumbing signals, not user-selectable traces.
  return schema.items.filter((it) => it && typeof it.name === 'string' && !it.name.startsWith('_'));
}

function colorFor(name) {
  let hash = 0;
  for (let i = 0; i < name.length; i += 1) hash = (hash * 31 + name.charCodeAt(i)) | 0;
  return PALETTE[Math.abs(hash) % PALETTE.length];
}

function tierOf(item) {
  return item.tier === 'base' ? 'base' : 'advanced';
}

function rebuildControlsIfSchemaChanged() {
  const { controlsEl: ctrl } = getElements();
  if (!ctrl) return;
  const items = schemaItems();
  // Signature includes the active tab and each metric's tier so the chip row
  // rebuilds when the tab changes or the renderer (re)classifies a metric.
  const signature =
    activeTier + '||' + items.map((it) => `${it.name}:${tierOf(it)}`).join('|');
  if (signature === renderedSchemaSignature) return;
  renderedSchemaSignature = signature;

  ctrl.innerHTML = '';
  ctrl.appendChild(buildTierTabs());
  ctrl.appendChild(buildWindowSelector());
  ctrl.appendChild(buildDiagRateSelector());
  ctrl.appendChild(buildPauseButton());
  ctrl.appendChild(buildFftButton());
  ctrl.appendChild(buildDiffButton());
  if (items.length === 0) {
    const empty = document.createElement('span');
    empty.textContent = 'No diag metrics registered yet.';
    empty.style.opacity = '0.7';
    ctrl.appendChild(empty);
    return;
  }
  // Group by `group` field, preserve registration order within group.
  // Only the metrics of the active tier are shown as chips; selected metrics
  // from the other tab still plot, they're just not listed here.
  const visibleItems = items.filter((it) => tierOf(it) === activeTier);
  if (visibleItems.length === 0) {
    const empty = document.createElement('span');
    empty.textContent = `No ${activeTier} metrics.`;
    empty.style.opacity = '0.7';
    ctrl.appendChild(empty);
    return;
  }
  const groups = new Map();
  for (const item of visibleItems) {
    const g = item.group || 'misc';
    if (!groups.has(g)) groups.set(g, []);
    groups.get(g).push(item);
  }
  for (const [groupName, members] of groups) {
    const groupEl = document.createElement('div');
    groupEl.style.display = 'flex';
    groupEl.style.flexWrap = 'wrap';
    groupEl.style.alignItems = 'center';
    groupEl.style.gap = '0.25rem';
    groupEl.style.padding = '0.15rem 0.35rem';
    groupEl.style.border = '1px solid rgba(255,255,255,0.08)';
    groupEl.style.borderRadius = '6px';
    const lbl = document.createElement('span');
    lbl.textContent = groupName;
    lbl.style.opacity = '0.7';
    lbl.style.marginRight = '0.15rem';
    groupEl.appendChild(lbl);
    for (const item of members) {
      const chip = document.createElement('button');
      chip.type = 'button';
      chip.textContent = item.label || item.name;
      chip.title = `${item.name}${item.unit ? ' (' + item.unit + ')' : ''}`;
      chip.dataset.name = item.name;
      chip.style.cssText = chipStyle(selected.has(item.name), colorFor(item.name));
      chip.addEventListener('click', () => {
        if (selected.has(item.name)) selected.delete(item.name);
        else selected.add(item.name);
        chip.style.cssText = chipStyle(selected.has(item.name), colorFor(item.name));
        saveSelection();
      });
      groupEl.appendChild(chip);
    }
    ctrl.appendChild(groupEl);
  }
}

function tierTabStyle(on) {
  const color = on ? '#7ad7ff' : '#d9ecff';
  const bg = on ? 'rgba(122, 215, 255, 0.18)' : 'rgba(255,255,255,0.06)';
  const border = on ? '#7ad7ff' : 'rgba(255,255,255,0.15)';
  return `font-size:10px;padding:0.15rem 0.55rem;border-radius:6px;border:1px solid ${border};background:${bg};color:${color};cursor:pointer;text-transform:uppercase;letter-spacing:0.04em;`;
}

function buildTierTabs() {
  const wrap = document.createElement('div');
  wrap.style.display = 'flex';
  wrap.style.alignItems = 'center';
  wrap.style.gap = '0.25rem';
  for (const tier of TIERS) {
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.textContent = tier;
    btn.style.cssText = tierTabStyle(activeTier === tier);
    btn.addEventListener('click', () => {
      if (activeTier === tier) return;
      activeTier = tier;
      saveTier();
      rebuildControlsIfSchemaChanged();
    });
    wrap.appendChild(btn);
  }
  return wrap;
}

function buildWindowSelector() {
  const wrap = document.createElement('div');
  wrap.style.display = 'flex';
  wrap.style.alignItems = 'center';
  wrap.style.gap = '0.35rem';
  wrap.style.padding = '0.15rem 0.35rem';
  wrap.style.border = '1px solid rgba(255,255,255,0.08)';
  wrap.style.borderRadius = '6px';
  const lbl = document.createElement('span');
  lbl.textContent = 'window';
  lbl.style.opacity = '0.7';
  lbl.style.fontSize = '10px';
  wrap.appendChild(lbl);
  const sel = document.createElement('select');
  sel.className = 'micro-select';
  for (const ms of WINDOW_OPTIONS_MS) {
    const opt = document.createElement('option');
    opt.value = String(ms);
    opt.textContent = ms >= 1000 ? `${ms / 1000} s` : `${ms} ms`;
    if (ms === windowMs) opt.selected = true;
    sel.appendChild(opt);
  }
  sel.addEventListener('change', () => {
    const v = Number.parseInt(sel.value, 10);
    if (Number.isFinite(v) && WINDOW_OPTIONS_MS.includes(v)) {
      windowMs = v;
      saveWindowMs();
    }
  });
  wrap.appendChild(sel);
  return wrap;
}

function buildDiagRateSelector() {
  const wrap = document.createElement('div');
  wrap.style.display = 'flex';
  wrap.style.alignItems = 'center';
  wrap.style.gap = '0.35rem';
  wrap.style.padding = '0.15rem 0.35rem';
  wrap.style.border = '1px solid rgba(255,255,255,0.08)';
  wrap.style.borderRadius = '6px';
  const lbl = document.createElement('span');
  lbl.textContent = 'diag';
  lbl.style.opacity = '0.7';
  lbl.style.fontSize = '10px';
  wrap.appendChild(lbl);
  const sel = document.createElement('select');
  sel.title = 'Diag publish rate (Hz) — independent from the audio meter rate. Higher = more plot resolution, more network/CPU.';
  sel.className = 'micro-select';
  for (const hz of DIAG_RATE_OPTIONS_HZ) {
    const opt = document.createElement('option');
    opt.value = String(hz);
    opt.textContent = `${hz} Hz`;
    if (hz === diagRateHz) opt.selected = true;
    sel.appendChild(opt);
  }
  sel.addEventListener('change', () => {
    const v = Number.parseInt(sel.value, 10);
    if (Number.isFinite(v) && DIAG_RATE_OPTIONS_HZ.includes(v)) {
      diagRateHz = v;
      saveDiagRateHz();
      pushDiagRate();
    }
  });
  // Renderer is the source of truth: no boot push. The select syncs from
  // /state/monitoring (syncDiagRateFromRenderer); user changes still push.
  diagRateSelectEl = sel;
  wrap.appendChild(sel);
  return wrap;
}

// Sync the diag-rate select from the renderer's authoritative value.
export function syncDiagRateFromRenderer(hz) {
  const rounded = Math.round(Number(hz));
  if (!Number.isFinite(rounded) || !DIAG_RATE_OPTIONS_HZ.includes(rounded)) return;
  diagRateHz = rounded;
  saveDiagRateHz();
  if (diagRateSelectEl) diagRateSelectEl.value = String(rounded);
}

function pushDiagRate() {
  try {
    invoke('control_diag_rate_hz', { value: diagRateHz });
  } catch (e) {
    // Tauri may not be available in some build configs — fail silently.
  }
}

function pushDiagEnabled(enabled) {
  try {
    invoke('control_diag_publication_enabled', { enable: enabled ? 1 : 0 });
  } catch (e) {
    // Tauri may not be available in some build configs — fail silently.
  }
}

function buildPauseButton() {
  const btn = document.createElement('button');
  btn.type = 'button';
  pauseBtnEl = btn;
  applyPauseButtonStyle();
  btn.addEventListener('click', () => {
    paused = !paused;
    applyPauseButtonStyle();
    if (paused) {
      if (intervalId) {
        clearInterval(intervalId);
        intervalId = null;
      }
    } else {
      // Leaving pause: drop the selection rectangle (its coordinates
      // become meaningless once tMax starts moving again).
      selection = null;
      dragging = false;
      if (visible && !intervalId) {
        intervalId = setInterval(tick, POLL_INTERVAL_MS);
        tick();
      }
    }
  });
  return btn;
}

function applyPauseButtonStyle() {
  if (!pauseBtnEl) return;
  pauseBtnEl.textContent = paused ? 'Resume' : 'Pause';
  const color = paused ? '#ffd166' : '#d9ecff';
  const bg = paused ? 'rgba(255, 209, 102, 0.18)' : 'rgba(255,255,255,0.06)';
  const border = paused ? '#ffd166' : 'rgba(255,255,255,0.15)';
  pauseBtnEl.style.cssText = `font-size:10px;padding:0.15rem 0.55rem;border-radius:6px;border:1px solid ${border};background:${bg};color:${color};cursor:pointer;`;
}

function buildFftButton() {
  const btn = document.createElement('button');
  btn.type = 'button';
  btn.title = 'Toggle FFT magnitude-spectrum view. Each selected metric is resampled to a uniform grid, Hanning-windowed, and plotted as |X(f)| in dB up to the Nyquist of the plot poll rate.';
  fftBtnEl = btn;
  applyFftButtonStyle();
  btn.addEventListener('click', () => {
    fftMode = !fftMode;
    saveFftMode();
    applyFftButtonStyle();
    // Reset the selection rectangle on mode flip: its anchor is in the
    // previous coordinate system (time vs frequency) and would otherwise
    // produce misleading readings.
    selection = null;
    dragging = false;
    render();
  });
  return btn;
}

function applyFftButtonStyle() {
  if (!fftBtnEl) return;
  fftBtnEl.textContent = fftMode ? 'FFT on' : 'FFT';
  const color = fftMode ? '#7ad7ff' : '#d9ecff';
  const bg = fftMode ? 'rgba(122, 215, 255, 0.18)' : 'rgba(255,255,255,0.06)';
  const border = fftMode ? '#7ad7ff' : 'rgba(255,255,255,0.15)';
  fftBtnEl.style.cssText = `font-size:10px;padding:0.15rem 0.55rem;border-radius:6px;border:1px solid ${border};background:${bg};color:${color};cursor:pointer;`;
}

function buildDiffButton() {
  const btn = document.createElement('button');
  btn.type = 'button';
  btn.title = 'Toggle first-derivative view (d/dt). Cumulative or trending metrics show their rate per second instead of the raw value. Combine with FFT to read frequency content cleanly.';
  diffBtnEl = btn;
  applyDiffButtonStyle();
  btn.addEventListener('click', () => {
    diffMode = !diffMode;
    saveDiffMode();
    applyDiffButtonStyle();
    selection = null;
    dragging = false;
    render();
  });
  return btn;
}

function applyDiffButtonStyle() {
  if (!diffBtnEl) return;
  diffBtnEl.textContent = diffMode ? 'd/dt on' : 'd/dt';
  const color = diffMode ? '#9cffa3' : '#d9ecff';
  const bg = diffMode ? 'rgba(156, 255, 163, 0.18)' : 'rgba(255,255,255,0.06)';
  const border = diffMode ? '#9cffa3' : 'rgba(255,255,255,0.15)';
  diffBtnEl.style.cssText = `font-size:10px;padding:0.15rem 0.55rem;border-radius:6px;border:1px solid ${border};background:${bg};color:${color};cursor:pointer;`;
}

// Iterative radix-2 Cooley-Tukey FFT, in-place. Re and Im are Float64Array of
// equal length N where N is a power of 2. Forward transform only; we never
// need the inverse here.
function fftInPlace(re, im) {
  const n = re.length;
  // Bit-reversal permutation.
  for (let i = 1, j = 0; i < n; i += 1) {
    let bit = n >> 1;
    for (; j & bit; bit >>= 1) {
      j ^= bit;
    }
    j ^= bit;
    if (i < j) {
      const tr = re[i]; re[i] = re[j]; re[j] = tr;
      const ti = im[i]; im[i] = im[j]; im[j] = ti;
    }
  }
  // Butterflies.
  for (let len = 2; len <= n; len <<= 1) {
    const half = len >> 1;
    const theta = -2 * Math.PI / len;
    const wRe = Math.cos(theta);
    const wIm = Math.sin(theta);
    for (let i = 0; i < n; i += len) {
      let curRe = 1.0;
      let curIm = 0.0;
      for (let k = 0; k < half; k += 1) {
        const a = i + k;
        const b = a + half;
        const tRe = curRe * re[b] - curIm * im[b];
        const tIm = curRe * im[b] + curIm * re[b];
        re[b] = re[a] - tRe;
        im[b] = im[a] - tIm;
        re[a] += tRe;
        im[a] += tIm;
        const nextRe = curRe * wRe - curIm * wIm;
        curIm = curRe * wIm + curIm * wRe;
        curRe = nextRe;
      }
    }
  }
}

// Largest power-of-2 ≤ n.
function floorPow2(n) {
  let p = 1;
  while (p * 2 <= n) p *= 2;
  return p;
}

// Build a uniformly-sampled vector of length N for the given metric, ending at
// the most recent buffer sample. Walks the buffer backwards in time; missing
// samples are linearly interpolated between the surrounding finite values.
// Returns null if too few finite samples are available.
function uniformResample(metricName, N) {
  // Read from the transformed series so diffMode propagates into the FFT
  // pipeline without any extra branching here.
  const series = transformedSeries(metricName).filter((p) => Number.isFinite(p.v));
  if (series.length < 2) return null;
  const tEnd = series[series.length - 1].t;
  const dtMs = POLL_INTERVAL_MS;
  const tStart = tEnd - (N - 1) * dtMs;
  if (tStart < series[0].t) return null; // not enough history.
  const out = new Float64Array(N);
  let bIdx = 0;
  for (let i = 0; i < N; i += 1) {
    const t = tStart + i * dtMs;
    while (bIdx + 1 < series.length && series[bIdx + 1].t <= t) bIdx += 1;
    const left = series[bIdx];
    const right = series[bIdx + 1] || left;
    if (right.t > left.t) {
      const a = (t - left.t) / (right.t - left.t);
      out[i] = left.v + a * (right.v - left.v);
    } else {
      out[i] = left.v;
    }
  }
  return out;
}

// Compute one-sided magnitude spectrum for the metric. Returns the half-
// spectrum in dB (relative to its own peak, so peak = 0 dB) plus the peak
// metadata. N is the FFT length (chosen by the caller). Sample rate is
// FFT_SAMPLE_RATE_HZ (the plot poll rate).
function computeSpectrum(metricName, N) {
  const signal = uniformResample(metricName, N);
  if (!signal) return null;
  // Remove DC offset; massive low-frequency drift would otherwise dominate.
  let mean = 0;
  for (let i = 0; i < N; i += 1) mean += signal[i];
  mean /= N;
  const re = new Float64Array(N);
  const im = new Float64Array(N);
  // Apply a Hanning window to reduce spectral leakage and improve the visible
  // separation between adjacent tones (relevant at 0.05 Hz resolution).
  for (let i = 0; i < N; i += 1) {
    const w = 0.5 * (1 - Math.cos((2 * Math.PI * i) / (N - 1)));
    re[i] = (signal[i] - mean) * w;
  }
  fftInPlace(re, im);
  const half = (N >> 1) + 1;
  // Convert per-bin |X[k]| to amplitude of an equivalent sinusoid in the
  // metric's natural unit. With a Hanning window the coherent gain is 0.5
  // (mean of the window); the one-sided amplitude for a non-DC bin is
  // |X[k]| × 2 / (N × coherent_gain) = 4 |X[k]| / N.
  const amp = new Float64Array(half);
  const db = new Float64Array(half);
  let peakAmp = 0;
  let peakIdx = 0;
  const scaleNonDc = 2.0 / (N * HANNING_COHERENT_GAIN);
  const scaleDc = 1.0 / (N * HANNING_COHERENT_GAIN);
  for (let i = 0; i < half; i += 1) {
    const m = Math.hypot(re[i], im[i]);
    amp[i] = m * (i === 0 || i === half - 1 ? scaleDc : scaleNonDc);
    db[i] = amp[i] > 0 ? 20 * Math.log10(amp[i]) : FFT_DB_CLIP_FLOOR;
    if (db[i] < FFT_DB_CLIP_FLOOR) db[i] = FFT_DB_CLIP_FLOOR;
    // Skip bin 0 (DC residual) when searching for the peak — Hanning + mean
    // removal already attenuate DC heavily but a residual can survive when
    // the signal contains a linear trend within the window.
    if (i > 0 && amp[i] > peakAmp) {
      peakAmp = amp[i];
      peakIdx = i;
    }
  }
  const binHz = FFT_SAMPLE_RATE_HZ / N;
  return {
    db,
    amp,
    peakIdx,
    peakHz: peakIdx * binHz,
    peakAmp,
    peakDb: db[peakIdx],
    binHz,
    half,
    N,
  };
}

function chipStyle(on, color) {
  const baseBg = on ? `${color}33` : 'rgba(255,255,255,0.06)';
  const borderColor = on ? color : 'rgba(255,255,255,0.15)';
  const txt = on ? color : '#d9ecff';
  return `font-size:10px;padding:0.15rem 0.45rem;border-radius:999px;border:1px solid ${borderColor};background:${baseBg};color:${txt};cursor:pointer;`;
}

// Build the (t, v) series consumed by both the time-domain renderer and the
// FFT resampler. When `diffMode` is on, returns the first derivative
// (value per second of wall time) — DC-free and trend-free, which is what
// makes a cumulative ramp like `input_clock_us` legible.
//
// The derivative is taken between consecutive **value changes**, not between
// consecutive polls: the studio polls at 50 Hz but the renderer republishes
// the diag bundle at its own cadence (default 50 Hz), and the underlying
// atomic only updates when its producing thread fires. So adjacent polls
// frequently carry an identical snapshot — naively diffing them yields a
// stream of zeros punctuated by spikes (a beat artefact, not the signal).
// Skipping unchanged polls collapses that aliasing and recovers the true
// rate between actual updates.
function transformedSeries(metricName) {
  if (!diffMode) {
    const out = new Array(buffer.length);
    for (let i = 0; i < buffer.length; i += 1) {
      out[i] = { t: buffer[i].t, v: buffer[i][metricName] };
    }
    return out;
  }
  const out = [];
  let lastChanged = null;
  for (const s of buffer) {
    const v = s[metricName];
    if (!Number.isFinite(v)) continue;
    if (lastChanged === null) {
      lastChanged = { t: s.t, v };
      continue;
    }
    if (v === lastChanged.v) continue; // no update since last change.
    const dt = (s.t - lastChanged.t) / 1000;
    if (dt > 0) out.push({ t: s.t, v: (v - lastChanged.v) / dt });
    lastChanged = { t: s.t, v };
  }
  return out;
}

function displayUnit(item) {
  if (!item) return '';
  if (!item.unit) return diffMode ? '/s' : '';
  return diffMode ? `${item.unit}/s` : item.unit;
}

function sampleValues() {
  const values = app.diagValues || {};
  const sample = { t: Date.now() };
  for (const name of selected) {
    const v = values[name];
    sample[name] = typeof v === 'number' && Number.isFinite(v) ? v : null;
  }
  return sample;
}

function drawTimeGrid(ctx, x0, y0, panelW, panelH, tMin, tMax) {
  // Light 250 ms ticks under the bold 1 s ticks so the latter remain
  // visually dominant. Skip 250 ms ticks that fall on a 1 s boundary.
  const xFor = (t) => x0 + ((t - tMin) / (tMax - tMin)) * panelW;
  ctx.lineWidth = 1;
  ctx.strokeStyle = 'rgba(255, 255, 255, 0.04)';
  ctx.beginPath();
  let t = Math.ceil(tMin / 250) * 250;
  while (t <= tMax) {
    if (t % 1000 !== 0) {
      const x = Math.round(xFor(t)) + 0.5;
      ctx.moveTo(x, y0);
      ctx.lineTo(x, y0 + panelH);
    }
    t += 250;
  }
  ctx.stroke();
  ctx.strokeStyle = 'rgba(255, 255, 255, 0.15)';
  ctx.beginPath();
  t = Math.ceil(tMin / 1000) * 1000;
  while (t <= tMax) {
    const x = Math.round(xFor(t)) + 0.5;
    ctx.moveTo(x, y0);
    ctx.lineTo(x, y0 + panelH);
    t += 1000;
  }
  ctx.stroke();
}

function renderPanel(ctx, item, x0, y0, panelW, panelH, tMin, tMax) {
  drawTimeGrid(ctx, x0, y0, panelW, panelH, tMin, tMax);
  ctx.strokeStyle = 'rgba(255, 255, 255, 0.07)';
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.moveTo(x0, y0 + panelH + 0.5);
  ctx.lineTo(x0 + panelW, y0 + panelH + 0.5);
  ctx.stroke();

  const series = transformedSeries(item.name);
  let vMin = Infinity;
  let vMax = -Infinity;
  let vSum = 0;
  let vCount = 0;
  for (const point of series) {
    if (point.t < tMin) continue; // off-screen to the left
    if (Number.isFinite(point.v)) {
      if (point.v < vMin) vMin = point.v;
      if (point.v > vMax) vMax = point.v;
      vSum += point.v;
      vCount += 1;
    }
  }
  const vMean = vCount > 0 ? vSum / vCount : null;
  const color = colorFor(item.name);
  const label = item.label || item.name;
  const unitTxt = displayUnit(item);
  const unit = unitTxt ? ` ${unitTxt}` : '';
  if (vMin === Infinity) {
    ctx.fillStyle = 'rgba(217, 236, 255, 0.55)';
    ctx.font = '10px sans-serif';
    ctx.textBaseline = 'top';
    ctx.fillText(`${label}: no data`, x0 + 4, y0 + 2);
    return { item, x0, y0, panelW, panelH, vMin: null, vMax: null };
  }
  const vMinRaw = vMin;
  const vMaxRaw = vMax;
  if (vMax - vMin < 1e-9) vMax = vMin + 1;
  const pad = (vMax - vMin) * 0.1;
  vMin -= pad;
  vMax += pad;
  const xFor = (t) => x0 + ((t - tMin) / (tMax - tMin)) * panelW;
  const yFor = (v) => y0 + ((vMax - v) / (vMax - vMin)) * (panelH - 4) + 2;

  ctx.strokeStyle = color;
  ctx.lineWidth = 1.5;
  ctx.beginPath();
  let started = false;
  for (const point of series) {
    if (!Number.isFinite(point.v)) continue;
    const xx = xFor(point.t);
    const yy = yFor(point.v);
    if (!started) { ctx.moveTo(xx, yy); started = true; }
    else { ctx.lineTo(xx, yy); }
  }
  if (started) ctx.stroke();

  // Mean reference line: dashed horizontal so the baseline is readable at
  // a glance even when the trace itself is noisy or oscillating around it.
  // Skipped when the visible-window range collapses to a point (no info).
  if (vMean !== null && vMaxRaw - vMinRaw > 1e-9) {
    const meanY = yFor(vMean);
    ctx.save();
    ctx.setLineDash([4, 3]);
    ctx.strokeStyle = '#ffd166';
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(x0, meanY + 0.5);
    ctx.lineTo(x0 + panelW, meanY + 0.5);
    ctx.stroke();
    ctx.restore();
    ctx.font = '10px sans-serif';
    ctx.fillStyle = '#ffd166';
    const meanLabel = `μ ${formatMean(vMean)}${unit}`;
    const lblW = ctx.measureText(meanLabel).width;
    const lblPad = 3;
    const boxW = lblW + lblPad * 2;
    const boxH = 12;
    let boxX = x0 + panelW - boxW - 2;
    let boxY = meanY - boxH / 2;
    if (boxY < y0 + 12) boxY = y0 + 12; // don't overlap header label
    if (boxY + boxH > y0 + panelH) boxY = y0 + panelH - boxH;
    ctx.fillStyle = 'rgba(15, 23, 36, 0.85)';
    ctx.fillRect(boxX, boxY, boxW, boxH);
    ctx.fillStyle = '#ffd166';
    ctx.textBaseline = 'middle';
    ctx.fillText(meanLabel, boxX + lblPad, boxY + boxH / 2);
  }

  ctx.fillStyle = 'rgba(217, 236, 255, 0.85)';
  ctx.font = '10px sans-serif';
  ctx.textBaseline = 'top';
  ctx.fillText(
    `${label} ${vMinRaw.toFixed(2)}…${vMaxRaw.toFixed(2)}${unit} (Δ ${(vMaxRaw - vMinRaw).toFixed(2)})`,
    x0 + 4,
    y0 + 2
  );
  return { item, x0, y0, panelW, panelH, vMin, vMax };
}

// Format a mean value compactly. Large magnitudes get exponential notation so
// a 1.9M baseline doesn't blow up the label width; small ones get fixed.
function formatMean(v) {
  const abs = Math.abs(v);
  if (abs >= 1e6 || (abs > 0 && abs < 0.01)) return v.toExponential(3);
  if (abs >= 100) return v.toFixed(1);
  return v.toFixed(3);
}

function drawFrequencyGrid(ctx, x0, y0, panelW, panelH, fMin, fMax) {
  // 1 Hz major grid + 0.25 Hz minor under it. Skip minors that coincide with
  // a major. Matches the cosmetics of the time grid (light minor + bold
  // major) so the eye reads the axis the same way.
  const xFor = (f) => x0 + ((f - fMin) / (fMax - fMin)) * panelW;
  ctx.lineWidth = 1;
  ctx.strokeStyle = 'rgba(255, 255, 255, 0.04)';
  ctx.beginPath();
  let f = Math.ceil(fMin / 0.25) * 0.25;
  while (f <= fMax) {
    const onMajor = Math.abs(f - Math.round(f)) < 1e-9;
    if (!onMajor) {
      const x = Math.round(xFor(f)) + 0.5;
      ctx.moveTo(x, y0);
      ctx.lineTo(x, y0 + panelH);
    }
    f += 0.25;
  }
  ctx.stroke();
  ctx.strokeStyle = 'rgba(255, 255, 255, 0.15)';
  ctx.beginPath();
  f = Math.ceil(fMin);
  while (f <= fMax) {
    const x = Math.round(xFor(f)) + 0.5;
    ctx.moveTo(x, y0);
    ctx.lineTo(x, y0 + panelH);
    f += 1;
  }
  ctx.stroke();
}

function renderFftPanel(ctx, item, x0, y0, panelW, panelH) {
  // Choose FFT length: largest power-of-2 ≤ buffer fill, capped to whatever
  // fits in the configured windowMs at POLL_INTERVAL_MS. Below 64 the
  // spectrum is too noisy to be useful — bail.
  const samplesInWindow = Math.floor(windowMs / POLL_INTERVAL_MS);
  const N = Math.min(floorPow2(buffer.length), floorPow2(samplesInWindow));
  ctx.strokeStyle = 'rgba(255, 255, 255, 0.07)';
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.moveTo(x0, y0 + panelH + 0.5);
  ctx.lineTo(x0 + panelW, y0 + panelH + 0.5);
  ctx.stroke();

  const color = colorFor(item.name);
  const label = item.label || item.name;
  if (N < 64) {
    ctx.fillStyle = 'rgba(217, 236, 255, 0.55)';
    ctx.font = '10px sans-serif';
    ctx.textBaseline = 'top';
    ctx.fillText(`${label}: building FFT… (need ≥ ${64 * POLL_INTERVAL_MS / 1000}s)`, x0 + 4, y0 + 2);
    return { item, x0, y0, panelW, panelH, vMin: null, vMax: null, isFft: true };
  }
  const spec = computeSpectrum(item.name, N);
  if (!spec) {
    ctx.fillStyle = 'rgba(217, 236, 255, 0.55)';
    ctx.font = '10px sans-serif';
    ctx.textBaseline = 'top';
    ctx.fillText(`${label}: no data`, x0 + 4, y0 + 2);
    return { item, x0, y0, panelW, panelH, vMin: null, vMax: null, isFft: true };
  }
  const fMin = 0;
  const fMax = FFT_SAMPLE_RATE_HZ / 2;
  drawFrequencyGrid(ctx, x0, y0, panelW, panelH, fMin, fMax);
  // Snap the top a few dB above the visible peak so the dB axis stays stable
  // across small spectrum updates (auto-scale to the absolute peak makes the
  // curve jitter vertically).
  const dbMax = Math.ceil((spec.peakDb + 3) / 5) * 5;
  const dbMin = dbMax - FFT_DB_SPAN;
  const xFor = (f) => x0 + ((f - fMin) / (fMax - fMin)) * panelW;
  const yFor = (db) => {
    const clamped = db < dbMin ? dbMin : db;
    return y0 + ((dbMax - clamped) / (dbMax - dbMin)) * (panelH - 4) + 2;
  };

  ctx.strokeStyle = color;
  ctx.lineWidth = 1.25;
  ctx.beginPath();
  for (let i = 0; i < spec.half; i += 1) {
    const f = i * spec.binHz;
    const xx = xFor(f);
    const yy = yFor(spec.db[i]);
    if (i === 0) ctx.moveTo(xx, yy);
    else ctx.lineTo(xx, yy);
  }
  ctx.stroke();
  // Peak marker + label.
  const px = xFor(spec.peakHz);
  const py = yFor(spec.peakDb);
  ctx.fillStyle = '#ffd166';
  ctx.beginPath();
  ctx.arc(px, py, 3, 0, Math.PI * 2);
  ctx.fill();
  ctx.font = '10px sans-serif';
  ctx.textBaseline = 'bottom';
  const peakLabel = `${spec.peakHz.toFixed(2)} Hz  ${spec.peakDb.toFixed(1)} dB`;
  const peakTextW = ctx.measureText(peakLabel).width;
  let peakLabelX = px + 5;
  if (peakLabelX + peakTextW > x0 + panelW - 2) peakLabelX = px - 5 - peakTextW;
  ctx.fillStyle = '#ffd166';
  ctx.fillText(peakLabel, peakLabelX, py - 4);

  ctx.fillStyle = 'rgba(217, 236, 255, 0.85)';
  ctx.textBaseline = 'top';
  const windowSec = (N * POLL_INTERVAL_MS / 1000).toFixed(2);
  const unitTxt = displayUnit(item);
  const unit = unitTxt ? ` ${unitTxt}` : '';
  ctx.fillText(
    `${label}  peak ${spec.peakHz.toFixed(2)} Hz @ ${spec.peakDb.toFixed(1)} dB (${spec.peakAmp.toExponential(2)}${unit})  (res ${spec.binHz.toFixed(3)} Hz, win ${windowSec}s, N=${N})`,
    x0 + 4,
    y0 + 2
  );
  return {
    item,
    x0,
    y0,
    panelW,
    panelH,
    vMin: dbMin,
    vMax: dbMax,
    isFft: true,
    fMin,
    fMax,
    spec,
  };
}

function pickPanelAt(y) {
  if (!lastRenderState) return -1;
  return lastRenderState.panels.findIndex((p) => y >= p.y0 && y < p.y0 + p.panelH);
}

function eventToCanvasCoords(canvas, evt) {
  const rect = canvas.getBoundingClientRect();
  return {
    x: (evt.clientX - rect.left) * (canvas.width / rect.width),
    y: (evt.clientY - rect.top) * (canvas.height / rect.height),
  };
}

function attachSelectionHandlers(canvas) {
  if (!canvas || selectionAttached) return;
  selectionAttached = true;
  canvas.style.cursor = 'crosshair';
  canvas.addEventListener('mousedown', (evt) => {
    // Time-domain panels: only meaningful while paused (rectangle anchors a
    // wall-clock instant). FFT panels: meaningful live since the X axis is
    // frequency, not time.
    if (!paused && !fftMode) return;
    if (evt.button !== 0) return;
    const p = eventToCanvasCoords(canvas, evt);
    const idx = pickPanelAt(p.y);
    if (idx < 0) return;
    selection = { startX: p.x, startY: p.y, endX: p.x, endY: p.y, panelIdx: idx };
    dragging = true;
    evt.preventDefault();
    render();
  });
  window.addEventListener('mousemove', (evt) => {
    if (!dragging) return;
    if (!paused && !fftMode) return;
    const p = eventToCanvasCoords(canvas, evt);
    selection.endX = p.x;
    selection.endY = p.y;
    render();
  });
  window.addEventListener('mouseup', () => {
    if (!dragging) return;
    dragging = false;
    render();
  });
}

function renderSelectionOverlay(ctx, w, h) {
  if (!selection || !lastRenderState) return;
  const panel = lastRenderState.panels[selection.panelIdx];
  if (!panel || panel.vMin === null) return;
  const { startX, startY, endX, endY } = selection;
  const xL = Math.min(startX, endX);
  const xR = Math.max(startX, endX);
  const yT = Math.min(startY, endY);
  const yB = Math.max(startY, endY);
  const dx = xR - xL;
  const dy = yB - yT;
  // Rectangle.
  ctx.fillStyle = 'rgba(255, 213, 74, 0.10)';
  ctx.strokeStyle = 'rgba(255, 213, 74, 0.85)';
  ctx.lineWidth = 1;
  ctx.fillRect(xL, yT, dx, dy);
  ctx.strokeRect(xL + 0.5, yT + 0.5, Math.max(0, dx - 1), Math.max(0, dy - 1));
  // Measurements. Coordinate system depends on panel mode (time-domain vs
  // FFT). FFT panels treat X as Hz and Y as dB (relative to peak).
  const yToV = (y) => {
    const norm = (y - panel.y0 - 2) / (panel.panelH - 4);
    return panel.vMax - norm * (panel.vMax - panel.vMin);
  };
  let lines;
  if (panel.isFft) {
    const fMin = panel.fMin ?? 0;
    const fMax = panel.fMax ?? (FFT_SAMPLE_RATE_HZ / 2);
    const xToF = (x) => fMin + (x / w) * (fMax - fMin);
    const fStart = Math.max(0, xToF(startX));
    const fEnd = Math.max(0, xToF(endX));
    // Look up the actual spectral value at the pointer (and at the anchor)
    // from the cached spectrum so the dB reading is the spectrum's value at
    // that frequency — not the meaningless dB derived from the cursor's Y
    // position.
    const lookup = (f) => {
      if (!panel.spec) return null;
      const { db, binHz, half } = panel.spec;
      const idx = Math.max(0, Math.min(half - 1, Math.round(f / binHz)));
      return { idx, dbVal: db[idx], f: idx * binHz };
    };
    const a = lookup(fStart);
    const b = lookup(fEnd);
    const deltaF = Math.abs(fEnd - fStart);
    const isPoint = deltaF < 0.05; // < 50 mHz drag → treat as single-point probe.
    const unitTxt = displayUnit(panel.item);
    const unit = unitTxt ? ` ${unitTxt}` : '';
    if (isPoint) {
      lines = [
        `f   ${b ? b.f.toFixed(3) : fEnd.toFixed(3)} Hz`,
        `|X| ${b ? b.dbVal.toFixed(1) : '—'} dB`,
      ];
      if (b && panel.spec) {
        const ampUnit = panel.spec.amp[b.idx];
        lines.push(`amp ${Number.isFinite(ampUnit) ? ampUnit.toExponential(2) : '—'}${unit}`);
      }
    } else {
      // Band probe: report the actual spectral peak in the band (not the rect
      // Y geometry), the endpoint dB values, and their spectral delta.
      let bandPeakHz = null;
      let bandPeakDb = null;
      let bandPeakAmp = null;
      if (panel.spec) {
        const { db, amp, binHz, half } = panel.spec;
        const fLo = Math.min(fStart, fEnd);
        const fHi = Math.max(fStart, fEnd);
        const iLo = Math.max(1, Math.floor(fLo / binHz));
        const iHi = Math.min(half - 1, Math.ceil(fHi / binHz));
        let bestDb = -Infinity;
        let bestIdx = -1;
        for (let i = iLo; i <= iHi; i += 1) {
          if (db[i] > bestDb) { bestDb = db[i]; bestIdx = i; }
        }
        if (bestIdx >= 0) {
          bandPeakHz = bestIdx * binHz;
          bandPeakDb = bestDb;
          bandPeakAmp = amp[bestIdx];
        }
      }
      const deltaDbSpectral = (a && b) ? (b.dbVal - a.dbVal) : null;
      lines = [
        `Δf   ${deltaF.toFixed(deltaF >= 10 ? 2 : 3)} Hz`,
        `${a ? a.f.toFixed(2) : fStart.toFixed(2)} Hz ${a ? a.dbVal.toFixed(1) : '—'} dB`,
        `${b ? b.f.toFixed(2) : fEnd.toFixed(2)} Hz ${b ? b.dbVal.toFixed(1) : '—'} dB`,
      ];
      if (deltaDbSpectral !== null) {
        lines.push(`ΔdB  ${deltaDbSpectral >= 0 ? '+' : ''}${deltaDbSpectral.toFixed(1)}`);
      }
      if (bandPeakHz !== null) {
        lines.push(`peak ${bandPeakHz.toFixed(2)} Hz @ ${bandPeakDb.toFixed(1)} dB (${bandPeakAmp.toExponential(2)}${unit})`);
      }
    }
  } else {
    const { tMin, tMax } = lastRenderState;
    const xToT = (x) => tMin + (x / w) * (tMax - tMin);
    const periodMs = Math.abs(xToT(endX) - xToT(startX));
    const freqHz = periodMs > 0 ? 1000 / periodMs : 0;
    const vStart = yToV(startY);
    const vEnd = yToV(endY);
    const amplitude = Math.abs(vEnd - vStart);
    const unitTxt = displayUnit(panel.item);
    const unit = unitTxt ? ` ${unitTxt}` : '';
    lines = [
      `Δt ${periodMs.toFixed(periodMs >= 100 ? 1 : 2)} ms`,
      `f  ${freqHz < 100 ? freqHz.toFixed(2) : freqHz.toFixed(0)} Hz`,
      `Δv ${amplitude.toFixed(amplitude >= 100 ? 1 : 3)}${unit}`,
    ];
  }
  ctx.font = '10px sans-serif';
  let boxW = 0;
  for (const line of lines) boxW = Math.max(boxW, ctx.measureText(line).width);
  boxW += 12;
  const lineHeight = 12;
  const padding = 4;
  const boxH = lines.length * lineHeight + 2 * padding;
  let boxX = xR + 6;
  if (boxX + boxW > w) boxX = xL - boxW - 6;
  if (boxX < 0) boxX = Math.max(0, w - boxW - 2);
  let boxY = yT;
  if (boxY + boxH > h) boxY = h - boxH;
  if (boxY < 0) boxY = 0;
  ctx.fillStyle = 'rgba(15, 23, 36, 0.94)';
  ctx.fillRect(boxX, boxY, boxW, boxH);
  ctx.strokeStyle = 'rgba(255, 213, 74, 0.7)';
  ctx.strokeRect(boxX + 0.5, boxY + 0.5, boxW - 1, boxH - 1);
  ctx.fillStyle = '#ffd166';
  ctx.textBaseline = 'top';
  for (let i = 0; i < lines.length; i += 1) {
    ctx.fillText(lines[i], boxX + padding, boxY + padding + i * lineHeight);
  }
}

function render() {
  const { canvasEl: canvas } = getElements();
  if (!canvas) return;
  attachSelectionHandlers(canvas);
  const ctx = canvas.getContext('2d');
  const w = canvas.width;
  const h = canvas.height;
  ctx.fillStyle = 'rgba(15, 23, 36, 0.9)';
  ctx.fillRect(0, 0, w, h);

  const items = schemaItems().filter((it) => selected.has(it.name));
  if (items.length === 0) {
    ctx.fillStyle = 'rgba(217, 236, 255, 0.85)';
    ctx.font = '10px sans-serif';
    ctx.fillText('Select one or more metrics above.', 6, h / 2);
    lastRenderState = null;
    return;
  }
  if (buffer.length < 2) {
    ctx.fillStyle = 'rgba(217, 236, 255, 0.85)';
    ctx.font = '10px sans-serif';
    ctx.fillText('Waiting for diag telemetry…', 6, h / 2);
    lastRenderState = null;
    return;
  }
  const tMax = buffer[buffer.length - 1].t;
  const tMin = tMax - windowMs;
  const panelH = Math.floor(h / items.length);
  const panels = [];
  for (let i = 0; i < items.length; i += 1) {
    const y0 = i * panelH;
    const ph = i === items.length - 1 ? h - y0 : panelH;
    const meta = fftMode
      ? renderFftPanel(ctx, items[i], 0, y0, w, ph)
      : renderPanel(ctx, items[i], 0, y0, w, ph, tMin, tMax);
    if (meta) panels.push(meta);
  }
  lastRenderState = { tMin, tMax, w, h, panels, fftMode };
  renderSelectionOverlay(ctx, w, h);
}

function resizeCanvasForSelection() {
  const { canvasEl: canvas } = getElements();
  if (!canvas) return;
  const count = Math.max(1, [...selected].length);
  const target = Math.min(640, Math.max(120, count * 80));
  if (canvas.height !== target) {
    canvas.height = target;
  }
}

function tick() {
  rebuildControlsIfSchemaChanged();
  resizeCanvasForSelection();
  // Re-assert the diag-publication subscription periodically while the
  // panel is open. Cheap (one OSC int per second) and self-heals after a
  // renderer restart or a missed initial enable (e.g. the panel was
  // already open at page reload, so `setDiagPlotVisible` was never called
  // and never sent its edge-triggered enable).
  const sample = sampleValues();
  if (sample.t - lastDiagEnabledKeepaliveAt >= DIAG_ENABLED_KEEPALIVE_MS) {
    pushDiagEnabled(true);
    lastDiagEnabledKeepaliveAt = sample.t;
  }
  buffer.push(sample);
  // Retain up to the largest window option so the user can zoom out
  // without losing recently-collected history.
  const maxWindow = WINDOW_OPTIONS_MS[WINDOW_OPTIONS_MS.length - 1];
  const cutoff = sample.t - maxWindow;
  while (buffer.length && buffer[0].t < cutoff) buffer.shift();
  render();
}

export function setDiagPlotVisible(open) {
  const { containerEl: c, toggleBtnEl: btn } = getElements();
  if (!c) return;
  const wasVisible = visible;
  visible = Boolean(open);
  c.style.display = visible ? 'block' : 'none';
  if (btn) {
    btn.classList.toggle('is-active', visible);
    btn.setAttribute('aria-pressed', visible ? 'true' : 'false');
  }
  if (visible) {
    if (!paused && !intervalId) intervalId = setInterval(tick, POLL_INTERVAL_MS);
    tick();
  } else {
    if (intervalId) {
      clearInterval(intervalId);
      intervalId = null;
    }
    buffer.length = 0;
    render();
  }
  // Tell the renderer whether anyone is listening to diag traffic. The
  // renderer-side per-client `diag_enabled` flag gates `send_diag_bundle`
  // entirely so closing the panel stops the JSON serialisation + UDP
  // traffic. Always fire on visibility changes — and when becoming visible
  // again, reset the keepalive timer so `tick` immediately re-asserts
  // (this catches the case where the renderer restarted while we were
  // hidden).
  if (visible) {
    lastDiagEnabledKeepaliveAt = 0;
    pushDiagEnabled(true);
  } else if (wasVisible) {
    pushDiagEnabled(false);
  }
}

export function isDiagPlotVisible() {
  return visible;
}

export function toggleDiagPlot() {
  setDiagPlotVisible(!visible);
}

/// Returns the currently-selected diag-plot time window in milliseconds.
/// Other plots (components-plot, resample-plot) read this so they share the
/// same horizontal scale and the user only has one knob to turn.
export function getPlotWindowMs() {
  return windowMs;
}
