/**
 * Audio format display controls.
 *
 * Extracted from app.js (lines 4295-4378).
 */

import { invoke } from '@tauri-apps/api/core';
import {
  app,
  dirty,
  getLiveOption,
  AUDIO_SAMPLE_RATE_PRESETS,
  hasProducerDomain
} from '../state.js';
import { reflectBoundOptions } from '../options-binder.js';
import { t, tf } from '../i18n.js';
import { scheduleUIFlush } from '../flush.js';
import { inAudioPanel, inRendererPanel } from '../ui/panel-roots.js';
import { syncVirtualBedObjects, renderChannelEditor } from './virtual-bed.js';

function getAudioFormatInfoEl() { return inAudioPanel('audioFormatInfo'); }
function getAudioOutputDeviceSelectEl() { return inAudioPanel('audioOutputDeviceSelect'); }
function getRampModeSelectEl() { return inRendererPanel('rampModeSelect'); }
function getAudioSampleRateInputEl() { return inAudioPanel('audioSampleRateInput'); }
function getAudioSampleRateMenuEl() { return inAudioPanel('audioSampleRateMenu'); }
function getAudioOutputSummaryEl() { return inAudioPanel('audioOutputSummary'); }

export function buildAudioConfigPayload() {
  return {
    outputDevice: app.audioOutputDevice || null,
    sampleRate: app.audioSampleRate || null,
    latencyTargetMs: app.latencyRequestedMs || app.latencyTargetMs || null,
    adaptiveResampling: {
      enabled: app.adaptiveResamplingEnabled === true,
      enableFarMode: app.adaptiveResamplingEnableFarMode === true,
      forceSilenceInFarMode: app.adaptiveResamplingForceSilenceInFarMode === true,
      hardRecoverHighInFarMode: app.adaptiveResamplingHardRecoverHighInFarMode === true,
      hardRecoverLowInFarMode: app.adaptiveResamplingHardRecoverLowInFarMode === true,
      farModeReturnFadeInMs: Math.max(0, Math.round(app.adaptiveResamplingFarModeReturnFadeInMs ?? 0)),
      kpNear: Number(app.adaptiveResamplingKpNear ?? 1),
      ki: Number(app.adaptiveResamplingKi ?? 1),
      integralDischargeRatio: Number(app.adaptiveResamplingIntegralDischargeRatio ?? 0.25),
      maxAdjust: Number(app.adaptiveResamplingMaxAdjust ?? 0.01),
      highRecoverEntryMarginMs: Math.max(1, Math.round(app.adaptiveResamplingHighRecoverEntryMarginMs ?? 1000)),
      updateIntervalCallbacks: Math.max(1, Math.round(app.adaptiveResamplingUpdateIntervalCallbacks ?? 1)),
      lowRecoverSettleStableMs: Math.max(0, Number(app.adaptiveResamplingLowRecoverSettleStableMs ?? 200)),
      lowRecoverEntryMarginMs: Math.max(0, Number(app.adaptiveResamplingLowRecoverEntryMarginMs ?? 18)),
      lowRecoverExitMarginMs: Math.max(0, Number(app.adaptiveResamplingLowRecoverExitMarginMs ?? 6)),
      lowRecoverSettleMarginMs: Math.max(0, Number(app.adaptiveResamplingLowRecoverSettleMarginMs ?? 6)),
      lowRecoverRefillDeltaAlpha: Math.min(1, Math.max(0, Number(app.adaptiveResamplingLowRecoverRefillDeltaAlpha ?? 0.5))),
      controlSmoothingCutoffHz: Math.max(0.001, Number(app.adaptiveResamplingControlSmoothingCutoffHz ?? 0.5)),
      controlSmoothingOrder: Math.min(2, Math.max(1, Math.round(Number(app.adaptiveResamplingControlSmoothingOrder ?? 1)))),
      paused: app.adaptiveResamplingPaused === true,
      usePreBridgeClock: app.adaptiveResamplingUsePreBridgeClock === true,
      useOutputPacing: app.adaptiveResamplingUseOutputPacing === true,
      disableBackpressure: app.adaptiveResamplingDisableBackpressure === true
    }
  };
}

export function sendAudioConfig({ apply = true } = {}) {
  const payload = buildAudioConfigPayload();
  return invoke('control_audio_config', { payload }).then(() => {
    if (!apply) return null;
    return invoke('control_audio_config_apply');
  });
}

export function renderAudioFormatDisplay() {
  const audioFormatInfoEl = getAudioFormatInfoEl();
  const audioOutputDeviceSelectEl = getAudioOutputDeviceSelectEl();
  const rampModeSelectEl = getRampModeSelectEl();
  const audioSampleRateInputEl = getAudioSampleRateInputEl();
  const audioOutputSummaryEl = getAudioOutputSummaryEl();
  const hasAudioDomain = hasProducerDomain('audio');
  if (audioFormatInfoEl) {
    const rateText = app.audioSampleRate ? `${app.audioSampleRate} Hz` : '—';
    const fmtText = app.audioSampleFormat || '—';
    const baseText = tf('status.audioFormat', { rate: rateText, format: fmtText });
    audioFormatInfoEl.textContent = app.audioError ? `${baseText} • Error: ${app.audioError}` : baseText;
  }
  if (audioOutputDeviceSelectEl) {
    const defaultLabel = app.oscSnapshotReady ? t('status.defaultOutputDevice') : '—';
    const options = [{ value: '', label: defaultLabel }, ...app.audioOutputDevices];
    if (app.audioOutputDevice && !options.some((entry) => entry.value === app.audioOutputDevice)) {
      options.push({ value: app.audioOutputDevice, label: app.audioOutputDevice });
    }
    const selectedValue = app.audioOutputDeviceEditing
      ? String(audioOutputDeviceSelectEl.value || '')
      : (app.audioOutputDevice || '');
    audioOutputDeviceSelectEl.innerHTML = '';
    options.forEach((entry) => {
      const optionEl = document.createElement('option');
      optionEl.value = entry.value;
      optionEl.textContent = entry.label || entry.value || t('status.defaultOutputDevice');
      audioOutputDeviceSelectEl.appendChild(optionEl);
    });
    audioOutputDeviceSelectEl.value = options.some((entry) => entry.value === selectedValue)
      ? selectedValue
      : '';
    audioOutputDeviceSelectEl.disabled = !app.oscSnapshotReady || !hasAudioDomain;
  }
  // Output backend selector + device/file rows. When the file backend is
  // active, a "Named pipe" switch chooses stdout (off) vs a FIFO/file path
  // (on) — the path field only shows in pipe mode, so there's no magic "-".
  const isFileBackend = app.audioOutputBackend === 'file';
  const useNamedPipe = isFileBackend && app.audioOutputFile !== '-';
  const audioOutputBackendSelectEl = inAudioPanel('audioOutputBackendSelect');
  const audioOutputDeviceRowEl = inAudioPanel('audioOutputDeviceRow');
  const audioOutputPipeRowEl = inAudioPanel('audioOutputPipeRow');
  const audioOutputPipeToggleEl = inAudioPanel('audioOutputPipeToggle');
  const audioOutputFileRowEl = inAudioPanel('audioOutputFileRow');
  const audioOutputFileFormatRowEl = inAudioPanel('audioOutputFileFormatRow');
  const audioOutputFileInputEl = inAudioPanel('audioOutputFileInput');
  const audioOutputFileFormatSelectEl = inAudioPanel('audioOutputFileFormatSelect');
  if (audioOutputBackendSelectEl) {
    audioOutputBackendSelectEl.value = isFileBackend ? 'file' : 'device';
    audioOutputBackendSelectEl.disabled = !app.oscSnapshotReady || !hasAudioDomain;
  }
  if (audioOutputDeviceRowEl) audioOutputDeviceRowEl.style.display = isFileBackend ? 'none' : '';
  if (audioOutputPipeRowEl) audioOutputPipeRowEl.style.display = isFileBackend ? '' : 'none';
  if (audioOutputPipeToggleEl) {
    audioOutputPipeToggleEl.checked = useNamedPipe;
    audioOutputPipeToggleEl.disabled = !app.oscSnapshotReady || !hasAudioDomain;
  }
  if (audioOutputFileRowEl) audioOutputFileRowEl.style.display = useNamedPipe ? '' : 'none';
  if (audioOutputFileFormatRowEl) audioOutputFileFormatRowEl.style.display = isFileBackend ? '' : 'none';
  if (audioOutputFileInputEl && !app.audioOutputFileEditing) {
    audioOutputFileInputEl.value = app.audioOutputFile === '-' ? '' : app.audioOutputFile || '';
    audioOutputFileInputEl.disabled = !app.oscSnapshotReady || !hasAudioDomain;
  }
  if (audioOutputFileFormatSelectEl) {
    audioOutputFileFormatSelectEl.value = ['raw_f32', 'caf'].includes(app.audioOutputFileFormat)
      ? app.audioOutputFileFormat
      : 'raw_f32';
    audioOutputFileFormatSelectEl.disabled = !app.oscSnapshotReady || !hasAudioDomain;
  }
  if (rampModeSelectEl) {
    rampModeSelectEl.value = ['off', 'frame', 'sample', 'interp'].includes(app.rampMode) ? app.rampMode : 'frame';
  }
  {
    // The registry binder reflects the bound controls. Applicability only
    // changes status text: every setting remains available for offline setup.
    reflectBoundOptions();
    const virtualBedActions = document.getElementById('virtualBedActions');
    if (virtualBedActions) virtualBedActions.style.display = 'flex';
    const surroundRow = document.getElementById('surroundPlacementRow');
    if (surroundRow) surroundRow.style.display = 'flex';
    updateTwoDSourcesSummary();
    const objectGeneratorRow = document.getElementById('objectGeneratorRow');
    if (objectGeneratorRow) objectGeneratorRow.style.display = 'flex';
    updateObjectGeneratorUI();
    const phantomRow = document.getElementById('phantomExtractRow');
    if (phantomRow) phantomRow.style.display = 'flex';
    updatePhantomUI();
    updateFixedChannelProcessingUI();
    syncVirtualBedObjects();
    renderChannelEditor();
  }
  if (audioSampleRateInputEl && !app.audioSampleRateEditing) {
    audioSampleRateInputEl.value = String(app.audioSampleRate || 0);
    audioSampleRateInputEl.disabled = !app.oscSnapshotReady || !hasAudioDomain;
  }
  if (audioOutputSummaryEl) {
    if (!hasAudioDomain) {
      // Host/mpv mode: the renderer doesn't own the output device, so this panel
      // shows only the channel mapping — summarise that, not a (stale) device.
      const mKey = getLiveOption('output_channel_mapping') === 'by_name'
        ? 'audio.channelMapping.byName'
        : 'audio.channelMapping.byIndex';
      audioOutputSummaryEl.textContent = `${t('audio.channelMapping')}: ${t(mKey)}`;
    } else {
      const requestedValue = (app.audioOutputDevice || '').trim();
      const effectiveValue = (app.audioOutputDeviceEffective || requestedValue).trim();
      const deviceEntry = app.audioOutputDevices.find((entry) => entry.value === effectiveValue);
      const deviceText = effectiveValue
        ? (deviceEntry?.label || effectiveValue)
        : (app.oscSnapshotReady ? t('status.defaultOutputDevice') : '—');
      const rateText = app.audioSampleRate ? `${app.audioSampleRate} Hz` : '—';
      const fmtText = app.audioSampleFormat || '—';
      const summary = tf('audio.summary', {
        device: deviceText,
        rate: rateText,
        format: fmtText
      });
      audioOutputSummaryEl.textContent = app.audioError ? `${summary} • Error: ${app.audioError}` : summary;
    }
  }
  updateOutputChannelMappingUI();
}

export function closeAudioSampleRateMenu() {
  const audioSampleRateMenuEl = getAudioSampleRateMenuEl();
  if (!audioSampleRateMenuEl) return;
  audioSampleRateMenuEl.style.display = 'none';
}

export function openAudioSampleRateMenu() {
  const audioSampleRateMenuEl = getAudioSampleRateMenuEl();
  const audioSampleRateInputEl = getAudioSampleRateInputEl();
  if (!audioSampleRateMenuEl) return;
  app.audioSampleRateEditing = true;
  audioSampleRateMenuEl.innerHTML = '';
  AUDIO_SAMPLE_RATE_PRESETS.forEach((rate) => {
    const item = document.createElement('button');
    item.type = 'button';
    item.style.cssText = 'display:block;width:100%;text-align:left;background:none;border:none;color:#d9ecff;padding:0.25rem 0.35rem;border-radius:6px;cursor:pointer;font-size:12px';
    item.textContent = rate === 0 ? t('status.nativeRate') : `${rate} Hz`;
    item.addEventListener('click', () => {
      if (audioSampleRateInputEl) {
        audioSampleRateInputEl.value = String(rate);
      }
      applyAudioSampleRateNow();
      closeAudioSampleRateMenu();
    });
    item.addEventListener('mouseenter', () => {
      item.style.background = 'rgba(255,255,255,0.12)';
    });
    item.addEventListener('mouseleave', () => {
      item.style.background = 'transparent';
    });
    audioSampleRateMenuEl.appendChild(item);
  });
  audioSampleRateMenuEl.style.display = 'block';
}

export function updateAudioFormatDisplay() {
  dirty.audioFormat = true;
  scheduleUIFlush();
}

export function applyAudioSampleRateNow() {
  const audioSampleRateInputEl = getAudioSampleRateInputEl();
  const requested = Math.max(0, Math.round(Number(audioSampleRateInputEl?.value) || 0));
  app.audioSampleRate = requested > 0 ? requested : null;
  updateAudioFormatDisplay();
  sendAudioConfig();
  app.audioSampleRateEditing = false;
  closeAudioSampleRateMenu();
}

export function applyAudioOutputDeviceNow() {
  const audioOutputDeviceSelectEl = getAudioOutputDeviceSelectEl();
  const requested = String(audioOutputDeviceSelectEl?.value || '').trim();
  app.audioOutputDevice = requested || null;
  updateAudioFormatDisplay();
  sendAudioConfig();
  app.audioOutputDeviceEditing = false;
}

export function applyAudioOutputBackendNow() {
  const el = inAudioPanel('audioOutputBackendSelect');
  const requested = String(el?.value || 'device').trim() === 'file' ? 'file' : 'device';
  app.audioOutputBackend = requested;
  // Explicit backend switch goes through its own control (not the batch audio
  // config), so unrelated config applies never flip the backend.
  invoke('control_audio_output_backend', { backend: requested });
  updateAudioFormatDisplay();
}

// Stdout ↔ named-pipe switch. Off = stdout (destination "-"); on = a FIFO/file
// path entered below (restored from the remembered path when toggling back).
export function applyAudioOutputNamedPipeNow() {
  const el = inAudioPanel('audioOutputPipeToggle');
  const usePipe = !!el?.checked;
  if (usePipe) {
    const path = (app.audioOutputPipePath || '').trim();
    app.audioOutputFile = path; // empty until the user types a path
    if (path) {
      invoke('control_audio_output_file', { path });
    }
  } else {
    if (app.audioOutputFile && app.audioOutputFile !== '-') {
      app.audioOutputPipePath = app.audioOutputFile;
    }
    app.audioOutputFile = '-';
    invoke('control_audio_output_file', { path: '-' });
  }
  updateAudioFormatDisplay();
}

export function applyAudioOutputFileNow() {
  const el = inAudioPanel('audioOutputFileInput');
  const requested = String(el?.value ?? '').trim();
  app.audioOutputFileEditing = false;
  if (!requested) {
    // Empty path: nothing valid to send yet; keep the field open.
    app.audioOutputFile = '';
    updateAudioFormatDisplay();
    return;
  }
  app.audioOutputFile = requested;
  app.audioOutputPipePath = requested;
  invoke('control_audio_output_file', { path: requested });
  updateAudioFormatDisplay();
}

export function applyAudioOutputFileFormatNow() {
  const el = inAudioPanel('audioOutputFileFormatSelect');
  const requested = String(el?.value || 'raw_f32').trim();
  app.audioOutputFileFormat = requested;
  invoke('control_audio_output_file_format', { format: requested });
  updateAudioFormatDisplay();
}

export function applyRampModeNow() {
  const rampModeSelectEl = getRampModeSelectEl();
  const requested = String(rampModeSelectEl?.value || 'frame').trim().toLowerCase();
  if (!['off', 'frame', 'sample'].includes(requested)) {
    return;
  }
  app.rampMode = requested;
  updateAudioFormatDisplay();
  invoke('control_ramp_mode', { value: requested });
}

// Header summary shown while the fixed-channel-source panel is collapsed.
export function updateTwoDSourcesSummary() {
  const summaryEl = document.getElementById('twoDSourcesSummary');
  if (!summaryEl) return;
  const placement = getLiveOption('surround_placement') === 'back'
    ? t('twoDSources.surroundBack')
    : t('twoDSources.surroundSide');
  const synthesis = getLiveOption('synthetic_objects_enabled')
    ? t('twoDSources.summary.syntheticEnabled')
    : t('twoDSources.summary.fixedOnly');
  summaryEl.textContent = `${placement} · ${synthesis}`;
}

const PROCESSING_REASON_KEYS = {
  active: 'twoDSources.status.active',
  off: 'twoDSources.status.off',
  master_off: 'twoDSources.status.masterOff',
  no_stream: 'twoDSources.status.noStream',
  object_stream: 'twoDSources.status.objectStream',
  input_has_height: 'twoDSources.status.inputHasHeight',
  output_has_no_height: 'twoDSources.status.outputHasNoHeight',
  insufficient_channels: 'twoDSources.status.insufficientChannels'
};

function processingReason(value) {
  return t(PROCESSING_REASON_KEYS[value] || 'twoDSources.status.noStream');
}

function effectivePhantomReason() {
  if ((getLiveOption('phantom_extract_mode') || 'off') === 'off') return 'off';
  if (!getLiveOption('synthetic_objects_enabled')) return 'master_off';
  return app.fixedChannelProcessing?.phantom || 'no_stream';
}

function effectiveHeightReason() {
  const generator = getLiveOption('object_generator_id') || 'none';
  if (!generator || generator === 'none') return 'off';
  if (!getLiveOption('synthetic_objects_enabled')) return 'master_off';
  return app.fixedChannelProcessing?.height || 'no_stream';
}

export function updateFixedChannelProcessingUI() {
  const state = app.fixedChannelProcessing || {};
  const activity = document.getElementById('fixedChannelActivity');
  if (activity) {
    const key = state.stream === 'fixed'
      ? 'twoDSources.stream.fixed'
      : state.stream === 'objects'
        ? 'twoDSources.stream.objects'
        : 'twoDSources.stream.idle';
    activity.textContent = t(key);
  }
  const masterStatus = document.getElementById('syntheticObjectsStatus');
  if (masterStatus) {
    masterStatus.textContent = getLiveOption('synthetic_objects_enabled')
      ? t('twoDSources.syntheticConfigured')
      : t('twoDSources.fixedOnly');
  }
  const phantomStatus = document.getElementById('phantomStatus');
  if (phantomStatus) phantomStatus.textContent = processingReason(effectivePhantomReason());
}

// The generator id whose param sliders are currently built into the DOM.
let builtParamGenId = null;

// Format a parameter value for display: decimals derived from the schema step,
// plus an optional unit suffix.
function fmtParamValue(spec, v) {
  const n = Number(v);
  const step = Number(spec.step) || 0;
  let s;
  if (step >= 1) s = String(Math.round(n));
  else if (step >= 0.1) s = n.toFixed(1);
  else s = n.toFixed(2);
  return spec.unit ? `${s} ${spec.unit}` : s;
}

// Localized label from an i18n key when it resolves, else the English label the
// schema carries (so out-of-tree generators still get a readable label).
function schemaLabel(i18nKey, fallback) {
  if (i18nKey) {
    const localized = t(i18nKey);
    if (localized && localized !== i18nKey) return localized;
  }
  return fallback || '';
}

function activeGeneratorSchema() {
  const id = getLiveOption('object_generator_id') || 'none';
  return (app.objectGenerators || []).find((g) => g && g.id === id) || null;
}

// (Re)build the generator selector from the declared schema. The hardcoded HTML
// options stay as a fallback until the schema arrives / for older renderers.
export function rebuildObjectGeneratorControls() {
  const sel = document.getElementById('objectGeneratorSelect');
  const list = app.objectGenerators || [];
  if (sel && list.length) {
    const current = getLiveOption('object_generator_id') || 'none';
    sel.innerHTML = '';
    const off = document.createElement('option');
    off.value = 'none';
    off.textContent = t('twoDSources.objectGenNone') || 'Off';
    sel.appendChild(off);
    for (const gen of list) {
      const opt = document.createElement('option');
      opt.value = gen.id;
      opt.textContent = schemaLabel(gen.i18nKey, gen.label);
      sel.appendChild(opt);
    }
    sel.value = current;
  }
  builtParamGenId = null; // force the param sliders to rebuild
  updateObjectGeneratorUI();
}

function buildParamSliders(schema) {
  const row = document.getElementById('objectGenParamsRow');
  if (!row) return;
  row.innerHTML = '';
  const params = (schema && schema.params) || [];
  for (const spec of params) {
    const label = document.createElement('label');
    label.style.cssText = 'display:flex;align-items:center;gap:0.5rem;font-size:11px';
    const name = document.createElement('span');
    name.style.cssText = 'flex:0 0 auto;min-width:96px';
    name.textContent = schemaLabel(spec.i18nKey, spec.label);
    const slider = document.createElement('input');
    slider.type = 'range';
    slider.min = String(spec.min);
    slider.max = String(spec.max);
    slider.step = String(spec.step);
    slider.style.cssText = 'flex:1;min-width:0';
    slider.dataset.paramKey = spec.key;
    const valEl = document.createElement('span');
    valEl.style.cssText =
      'flex:0 0 auto;width:48px;text-align:right;font-variant-numeric:tabular-nums';
    const stored = app.objectGeneratorParams && app.objectGeneratorParams[spec.key];
    const initial = stored != null ? stored : spec.default;
    slider.value = String(initial);
    valEl.textContent = fmtParamValue(spec, initial);
    slider.addEventListener('input', () => {
      valEl.textContent = fmtParamValue(spec, slider.value);
      applyObjectGeneratorParamNow(spec.key, slider.value);
    });
    label.appendChild(name);
    label.appendChild(slider);
    label.appendChild(valEl);
    row.appendChild(label);
  }
  builtParamGenId = schema ? schema.id : null;
}

// Reflect the active generator + its parameter sliders. Rebuilds the sliders
// when the active generator (or its schema) changes; otherwise just refreshes
// values without disturbing an in-progress drag.
export function updateObjectGeneratorUI() {
  const sel = document.getElementById('objectGeneratorSelect');
  // (Re)set after a schema-driven rebuild; the binder reflects the same value.
  if (sel && document.activeElement !== sel) sel.value = getLiveOption('object_generator_id') || 'none';
  // Applicability never disables configuration. The renderer reports why a
  // selected generator is currently inactive while offline edits remain valid.
  const hasHeight = app.objectGeneratorLayoutHasHeight !== false;
  if (sel) sel.disabled = false;
  const note = document.getElementById('objectGeneratorNoHeightNote');
  if (note) {
    const reason = effectiveHeightReason();
    note.style.display = reason && reason !== 'active' && reason !== 'off' ? 'inline' : 'none';
    note.textContent = processingReason(reason || (hasHeight ? 'off' : 'output_has_no_height'));
  }
  const schema = activeGeneratorSchema();
  const row = document.getElementById('objectGenParamsRow');
  const show = !!(schema && (schema.params || []).length);
  if (row) row.style.display = show ? 'flex' : 'none';
  const wantId = schema ? schema.id : null;
  if (wantId !== builtParamGenId) {
    buildParamSliders(schema);
  } else if (row) {
    for (const slider of row.querySelectorAll('input[type=range]')) {
      const key = slider.dataset.paramKey;
      const spec = (schema.params || []).find((p) => p.key === key);
      if (!spec) continue;
      const stored = app.objectGeneratorParams && app.objectGeneratorParams[key];
      const v = stored != null ? stored : spec.default;
      if (document.activeElement !== slider) slider.value = String(v);
      const valEl = slider.nextElementSibling;
      if (valEl) valEl.textContent = fmtParamValue(spec, v);
    }
  }
}

// Commit a generator parameter change live (slider drag): store the override and
// push it to the engine, which validates/clamps and applies it by key without
// resetting DSP state.
export function applyObjectGeneratorParamNow(key, value) {
  const v = Number(value);
  if (!Number.isFinite(v)) return;
  if (!app.objectGeneratorParams || typeof app.objectGeneratorParams !== 'object') {
    app.objectGeneratorParams = {};
  }
  app.objectGeneratorParams[key] = v;
  invoke('control_object_generator_param', { key, value: v });
}

// ── Phantom-source extraction pre-stage ──
// Runs before the height lift: extracts the correlated/primary content of channel
// pairs as discrete objects at their real panned position and reduces the bed.

let builtPhantomParams = false;

// Whether a declared param is a binary (on/off) toggle — rendered as a switch
// rather than a slider, per the switches-not-checkboxes rule.
function isBinaryParam(spec) {
  return Number(spec.min) === 0 && Number(spec.max) === 1 && Number(spec.step) === 1;
}

// Params that only shape the BROADBAND plan: the engine deliberately ignores
// their changes while spectral mode is active (see phantom_extract.rs sync —
// no pointless re-prime). Keep them editable for offline configuration, but
// visually identify that they do not affect the current mode.
const PHANTOM_BROADBAND_ONLY = new Set(['passes', 'center', 'sides']);

function phantomMethodIsSpectral() {
  return getLiveOption('phantom_extract_mode') === 'spectral';
}

// Reflect a param row's broadband-only gating on its container + control.
function applyPhantomParamGate(container, control, key, spectral) {
  const gated = spectral && PHANTOM_BROADBAND_ONLY.has(key);
  control.disabled = false;
  container.style.opacity = gated ? '0.7' : '';
  if (gated) {
    container.title = t('twoDSources.phantomBroadbandOnly');
  } else {
    container.removeAttribute('title');
  }
}

// Build the phantom param controls from the declared schema (app.phantomSchema is
// the param-spec array directly). Continuous params render as sliders; binary
// params (min 0, max 1, step 1) render as a switch. Mirrors buildParamSliders.
function buildPhantomParamSliders() {
  const row = document.getElementById('phantomParamsRow');
  if (!row) return;
  row.innerHTML = '';
  const params = app.phantomSchema || [];
  const spectral = phantomMethodIsSpectral();
  for (const spec of params) {
    const stored = app.phantomParams && app.phantomParams[spec.key];
    const initial = stored != null ? stored : spec.default;
    if (isBinaryParam(spec)) {
      const label = document.createElement('label');
      label.className = 'switch-row';
      label.style.cssText = 'font-size:11px;cursor:pointer;margin-top:0';
      const name = document.createElement('span');
      name.textContent = schemaLabel(spec.i18nKey, spec.label);
      const toggle = document.createElement('input');
      toggle.type = 'checkbox';
      toggle.dataset.paramKey = spec.key;
      toggle.checked = Number(initial) >= 0.5;
      toggle.addEventListener('change', () => {
        applyPhantomParamNow(spec.key, toggle.checked ? 1 : 0);
      });
      applyPhantomParamGate(label, toggle, spec.key, spectral);
      label.appendChild(name);
      label.appendChild(toggle);
      row.appendChild(label);
      continue;
    }
    const label = document.createElement('label');
    label.style.cssText = 'display:flex;align-items:center;gap:0.5rem;font-size:11px';
    const name = document.createElement('span');
    name.style.cssText = 'flex:0 0 auto;min-width:96px';
    name.textContent = schemaLabel(spec.i18nKey, spec.label);
    const slider = document.createElement('input');
    slider.type = 'range';
    slider.min = String(spec.min);
    slider.max = String(spec.max);
    slider.step = String(spec.step);
    slider.style.cssText = 'flex:1;min-width:0';
    slider.dataset.paramKey = spec.key;
    const valEl = document.createElement('span');
    valEl.style.cssText =
      'flex:0 0 auto;width:48px;text-align:right;font-variant-numeric:tabular-nums';
    slider.value = String(initial);
    valEl.textContent = fmtParamValue(spec, initial);
    slider.addEventListener('input', () => {
      valEl.textContent = fmtParamValue(spec, slider.value);
      applyPhantomParamNow(spec.key, slider.value);
    });
    applyPhantomParamGate(label, slider, spec.key, spectral);
    label.appendChild(name);
    label.appendChild(slider);
    label.appendChild(valEl);
    row.appendChild(label);
  }
  builtPhantomParams = (app.phantomSchema || []).length > 0;
}

// (Re)build the phantom controls when the schema arrives.
export function rebuildPhantomControls() {
  builtPhantomParams = false;
  updatePhantomUI();
}

// Show/hide + refresh the phantom parameter sliders.
export function updatePhantomUI() {
  const params = app.phantomSchema || [];
  const row = document.getElementById('phantomParamsRow');
  const show =
    getLiveOption('phantom_extract_mode') !== 'off' &&
    params.length > 0;
  if (row) row.style.display = show ? 'flex' : 'none';
  if (!builtPhantomParams && params.length > 0) {
    buildPhantomParamSliders();
  } else if (row) {
    const spectral = phantomMethodIsSpectral();
    for (const slider of row.querySelectorAll('input[type=range]')) {
      const key = slider.dataset.paramKey;
      const spec = params.find((p) => p.key === key);
      if (!spec) continue;
      const stored = app.phantomParams && app.phantomParams[key];
      const v = stored != null ? stored : spec.default;
      if (document.activeElement !== slider) slider.value = String(v);
      const valEl = slider.nextElementSibling;
      if (valEl) valEl.textContent = fmtParamValue(spec, v);
      applyPhantomParamGate(slider.parentElement, slider, key, spectral);
    }
    for (const toggle of row.querySelectorAll('input[type=checkbox][data-param-key]')) {
      const key = toggle.dataset.paramKey;
      const spec = params.find((p) => p.key === key);
      if (!spec) continue;
      const stored = app.phantomParams && app.phantomParams[key];
      const v = stored != null ? stored : spec.default;
      toggle.checked = Number(v) >= 0.5;
      applyPhantomParamGate(toggle.closest('label') || toggle.parentElement, toggle, key, spectral);
    }
  }
}

// Commit a phantom parameter change live (slider drag).
export function applyPhantomParamNow(key, value) {
  const v = Number(value);
  if (!Number.isFinite(v)) return;
  if (!app.phantomParams || typeof app.phantomParams !== 'object') {
    app.phantomParams = {};
  }
  app.phantomParams[key] = v;
  invoke('control_phantom_extract_param', { key, value: v });
}

// Show a warning when by-name mapping can't route some speakers (non-standard
// names for the active backend, reported by the renderer). The by-index/by-name
// buttons themselves are binder-reflected.
export function updateOutputChannelMappingUI() {
  const mapping = getLiveOption('output_channel_mapping') === 'by_name' ? 'by_name' : 'by_index';
  const warnEl = document.getElementById('outputChannelMappingWarning');
  if (warnEl) {
    const names = Array.isArray(app.outputChannelMappingUnroutable)
      ? app.outputChannelMappingUnroutable
      : [];
    if (mapping === 'by_name' && names.length > 0) {
      warnEl.textContent = `${t('audio.channelMapping.warning')} ${names.join(', ')}`;
      warnEl.style.display = 'block';
    } else {
      warnEl.style.display = 'none';
    }
  }
}
