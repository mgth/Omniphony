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
import { t, tf, i18nState } from '../i18n.js';
import { scheduleUIFlush } from '../flush.js';
import { inAudioPanel, inRendererPanel } from '../ui/panel-roots.js';
import { syncVirtualBedObjects, renderChannelEditor, renderPlacementPanel } from './virtual-bed.js';
import { renderParamForm } from './plugin-params.js';

function getAudioFormatInfoEl() { return inAudioPanel('audioFormatInfo'); }
function getAudioOutputDeviceSelectEl() { return inAudioPanel('audioOutputDeviceSelect'); }
function getRampModeSelectEl() { return inRendererPanel('rampModeSelect'); }
function getAudioSampleRateInputEl() { return inAudioPanel('audioSampleRateInput'); }
function getAudioSampleRateMenuEl() { return inAudioPanel('audioSampleRateMenu'); }
function getAudioOutputSummaryEl() { return inAudioPanel('audioOutputSummary'); }

// What the user typed, unmodified. The bounds, the defaults and the coercions
// live in `src-tauri/src/audio_config.rs`, which is also what forwards the
// configuration to the renderer — so there is one definition of a valid audio
// config, on the side that owns it. `control_audio_config` hands back the
// effective values, and `applyEffectiveAudioConfig` below renders those, so a
// field pulled into range shows its corrected value instead of looking
// accepted as typed.
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
      farModeReturnFadeInMs: app.adaptiveResamplingFarModeReturnFadeInMs,
      kpNear: app.adaptiveResamplingKpNear,
      ki: app.adaptiveResamplingKi,
      integralDischargeRatio: app.adaptiveResamplingIntegralDischargeRatio,
      maxAdjust: app.adaptiveResamplingMaxAdjust,
      highRecoverEntryMarginMs: app.adaptiveResamplingHighRecoverEntryMarginMs,
      updateIntervalCallbacks: app.adaptiveResamplingUpdateIntervalCallbacks,
      lowRecoverSettleStableMs: app.adaptiveResamplingLowRecoverSettleStableMs,
      lowRecoverEntryMarginMs: app.adaptiveResamplingLowRecoverEntryMarginMs,
      lowRecoverExitMarginMs: app.adaptiveResamplingLowRecoverExitMarginMs,
      lowRecoverSettleMarginMs: app.adaptiveResamplingLowRecoverSettleMarginMs,
      lowRecoverRefillDeltaAlpha: app.adaptiveResamplingLowRecoverRefillDeltaAlpha,
      controlSmoothingCutoffHz: app.adaptiveResamplingControlSmoothingCutoffHz,
      controlSmoothingOrder: app.adaptiveResamplingControlSmoothingOrder,
      paused: app.adaptiveResamplingPaused === true,
      usePreBridgeClock: app.adaptiveResamplingUsePreBridgeClock === true,
      useOutputPacing: app.adaptiveResamplingUseOutputPacing === true,
      disableBackpressure: app.adaptiveResamplingDisableBackpressure === true
    }
  };
}

/** Adopt the values the backend actually applied, so the form shows them. */
function applyEffectiveAudioConfig(effective) {
  const r = effective?.adaptiveResampling;
  if (!r || typeof r !== 'object') return;
  const numeric = {
    farModeReturnFadeInMs: 'adaptiveResamplingFarModeReturnFadeInMs',
    kpNear: 'adaptiveResamplingKpNear',
    ki: 'adaptiveResamplingKi',
    integralDischargeRatio: 'adaptiveResamplingIntegralDischargeRatio',
    maxAdjust: 'adaptiveResamplingMaxAdjust',
    highRecoverEntryMarginMs: 'adaptiveResamplingHighRecoverEntryMarginMs',
    updateIntervalCallbacks: 'adaptiveResamplingUpdateIntervalCallbacks',
    lowRecoverSettleStableMs: 'adaptiveResamplingLowRecoverSettleStableMs',
    lowRecoverEntryMarginMs: 'adaptiveResamplingLowRecoverEntryMarginMs',
    lowRecoverExitMarginMs: 'adaptiveResamplingLowRecoverExitMarginMs',
    lowRecoverSettleMarginMs: 'adaptiveResamplingLowRecoverSettleMarginMs',
    lowRecoverRefillDeltaAlpha: 'adaptiveResamplingLowRecoverRefillDeltaAlpha',
    controlSmoothingCutoffHz: 'adaptiveResamplingControlSmoothingCutoffHz',
    controlSmoothingOrder: 'adaptiveResamplingControlSmoothingOrder'
  };
  for (const [field, key] of Object.entries(numeric)) {
    if (Number.isFinite(r[field])) app[key] = r[field];
  }
  dirty.adaptiveResampling = true;
  scheduleUIFlush();
}

export function sendAudioConfig({ apply = true } = {}) {
  const payload = buildAudioConfigPayload();
  return invoke('control_audio_config', { payload }).then((effective) => {
    applyEffectiveAudioConfig(effective);
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
    renderPlacementPanel();
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
  renderCrossoverInfoDisplay();
}

function getCrossoverInfoEl() { return inRendererPanel('crossoverInfo'); }

// Annotate the crossover control with the bank the renderer actually built
// ({engine, bands, cutoffsHz, taps, latencyMs} from the snapshot; null until
// the first render). Piggybacks on the audio-format flush so it re-renders on
// every state push and language re-render without its own dirty flag.
function renderCrossoverInfoDisplay() {
  const el = getCrossoverInfoEl();
  if (!el) return;
  // The FIR transition tuning only means something for the FIR engine.
  const transitionRow = inRendererPanel('crossoverTransitionRow');
  if (transitionRow) {
    transitionRow.style.display = getLiveOption('crossover_type') === 'fir' ? '' : 'none';
  }
  const info = app.crossover;
  if (!info || !(info.bands > 1)) {
    el.textContent = t('renderer.crossoverInfoNone');
    return;
  }
  const low = Array.isArray(info.cutoffsHz) && info.cutoffsHz.length
    ? Math.round(info.cutoffsHz[0])
    : '—';
  const values = {
    bands: info.bands,
    low,
    taps: Number.isFinite(info.taps) ? info.taps.toLocaleString() : '—',
    latency: Number.isFinite(info.latencyMs) ? info.latencyMs.toFixed(1) : '0.0'
  };
  el.textContent = info.engine === 'fir'
    ? tf('renderer.crossoverInfoFir', values)
    : tf('renderer.crossoverInfoIir', values);
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

// Localized label from an i18n key when it resolves, else the English label the
// listing carries (so out-of-tree generators still get a readable label).
function schemaLabel(i18nKey, fallback) {
  if (i18nKey) {
    const localized = t(i18nKey);
    if (localized && localized !== i18nKey) return localized;
  }
  return fallback || '';
}

function activeGeneratorListing() {
  const id = getLiveOption('object_generator_id') || 'none';
  return (app.objectGenerators || []).find((g) => g && g.id === id) || null;
}

// (Re)build the generator selector from the published listings. The hardcoded
// HTML options stay as a fallback until the listings arrive / for older
// renderers.
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
  const row = document.getElementById('objectGenParamsRow');
  if (row) row.dataset.buildKey = ''; // force the param controls to rebuild
  updateObjectGeneratorUI();
}

// Reflect the active generator + its parameter controls, generated from its
// listing the way a backend's are. Rebuilt when the active generator (or its
// listing, or the locale) changes; otherwise the values are refreshed without
// disturbing an in-progress drag.
export function updateObjectGeneratorUI() {
  const sel = document.getElementById('objectGeneratorSelect');
  // (Re)set after a listing-driven rebuild; the binder reflects the same value.
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
  const listing = activeGeneratorListing();
  const row = document.getElementById('objectGenParamsRow');
  if (!row) return;
  const params = (listing && listing.params) || [];
  row.style.display = params.length ? 'flex' : 'none';
  if (!listing) {
    row.dataset.buildKey = '';
    row.replaceChildren();
    return;
  }
  const values = (app.objectGeneratorParamValuesById || {})[listing.id] || {};
  renderParamForm(
    row,
    params,
    values,
    (key, value) => applyObjectGeneratorParamNow(listing.id, key, value),
    { buildKey: `${listing.id}|${i18nState.locale}`, live: true },
  );
}

// Commit a generator parameter change live (slider drag): store it for the
// generator the form shows and push it to the engine, addressed by id, which
// reads it in its declared type and applies it without resetting DSP state.
function applyObjectGeneratorParamNow(generator, key, value) {
  const byId = app.objectGeneratorParamValuesById || (app.objectGeneratorParamValuesById = {});
  byId[generator] = { ...(byId[generator] || {}), [key]: value };
  invoke('control_object_generator_param', { generator, key, value });
}

// ── Phantom-source extraction pre-stage ──
// Runs before the height lift: extracts the correlated/primary content of channel
// pairs as discrete objects at their real panned position and reduces the bed.

// A parameter only one method reads declares it (`requires`): the engine
// ignores the other method's changes while it is active (see
// phantom_extract.rs sync — no pointless re-prime). Keep them editable for
// offline configuration, but visually identify that they do not affect the
// current mode.
function phantomGate(requires) {
  const spectral = getLiveOption('phantom_extract_mode') === 'spectral';
  if (requires === 'broadband' && spectral) return t('twoDSources.phantomBroadbandOnly');
  if (requires === 'spectral' && !spectral) return t('twoDSources.phantomSpectralOnly');
  return null;
}

// (Re)build the phantom controls when the listing arrives.
export function rebuildPhantomControls() {
  const row = document.getElementById('phantomParamsRow');
  if (row) row.dataset.buildKey = '';
  updatePhantomUI();
}

// Show/hide + refresh the phantom parameter controls, generated from the
// stage's listing the way a backend's are.
export function updatePhantomUI() {
  const params = (app.phantomListing && app.phantomListing.params) || [];
  const row = document.getElementById('phantomParamsRow');
  if (!row) return;
  const show = getLiveOption('phantom_extract_mode') !== 'off' && params.length > 0;
  row.style.display = show ? 'flex' : 'none';
  if (!params.length) return;
  renderParamForm(
    row,
    params,
    app.phantomParamValues || {},
    applyPhantomParamNow,
    { buildKey: i18nState.locale, live: true, gate: phantomGate },
  );
}

// Commit a phantom parameter change live (slider drag, switch).
function applyPhantomParamNow(key, value) {
  app.phantomParamValues = { ...(app.phantomParamValues || {}), [key]: value };
  invoke('control_phantom_extract_param', { key, value });
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
