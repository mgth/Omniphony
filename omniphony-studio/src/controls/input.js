import { app, hasProducerDomain, isEmbeddedProducer } from '../state.js';
import { t, tf } from '../i18n.js';
import { inInputPanel } from '../ui/panel-roots.js';
import { invoke } from '@tauri-apps/api/core';

function getInputModeSelectEl() { return inInputPanel('inputModeSelect'); }
function getInputPipeInputEl() { return inInputPanel('pipeStatus'); }
function getOscBridgePathInputEl() { return inInputPanel('oscBridgePathInput'); }
function getOscBridgePathStatusEl() { return inInputPanel('oscBridgePathStatus'); }
function getInputBackendSelectEl() { return inInputPanel('inputBackendSelect'); }
function getInputNodeInputEl() { return inInputPanel('inputNodeInput'); }
function getInputDescriptionInputEl() { return inInputPanel('inputDescriptionInput'); }
function getInputClockModeSelectEl() { return inInputPanel('inputClockModeSelect'); }
function getInputLayoutInputEl() { return inInputPanel('inputLayoutInput'); }
function getInputLayoutBrowseBtnEl() { return inInputPanel('inputLayoutBrowseBtn'); }
function getInputChannelsInputEl() { return inInputPanel('inputChannelsInput'); }
function getInputSampleRateInputEl() { return inInputPanel('inputSampleRateInput'); }
function getInputFormatSelectEl() { return inInputPanel('inputFormatSelect'); }
function getInputMapSelectEl() { return inInputPanel('inputMapSelect'); }
function getInputLfeModeSelectEl() { return inInputPanel('inputLfeModeSelect'); }
function getInputStatusInfoEl() { return inInputPanel('inputStatusInfo'); }
function getInputSummaryEl() { return inInputPanel('inputSummary'); }
function getInputApplyBtnEl() { return inInputPanel('inputApplyBtn'); }
function getInputBridgeFieldsEl() { return inInputPanel('inputBridgeFields'); }
function getInputLiveFieldsEl() { return inInputPanel('inputLiveFields'); }

function stringOrEmpty(value) {
  return typeof value === 'string' ? value : '';
}

function formatInputModeLabel(value) {
  switch (value) {
    case 'bridge':
    case 'pipe_bridge':
      return t('input.mode.pipe_bridge');
    case 'pipewire_bridge':
      return t('input.mode.pipewire_bridge');
    case 'live':
    case 'pipewire':
      return t('input.mode.pipewire');
    default:
      return value || '—';
  }
}

function formatInputBackendLabel(value) {
  if (value === 'asio') return t('input.backend.asio');
  if (value === 'pipewire') return t('input.backend.pipewire');
  return value || '—';
}

function formatClockModeLabel(value) {
  if (value === 'dac') return t('input.clock.dac');
  if (value === 'pipewire') return t('input.clock.pipewire');
  if (value === 'upstream') return t('input.clock.upstream');
  return value || '—';
}

function defaultNodePlaceholder(requestedMode) {
  return requestedMode === 'pipewire_bridge' ? 'omniphony' : 'omniphony_input_7_1';
}

function defaultDescriptionPlaceholder(requestedMode) {
  return requestedMode === 'pipewire_bridge'
    ? 'Omniphony Bridge Input'
    : 'Omniphony Input 7.1';
}

function defaultInputChannels(requestedMode) {
  return requestedMode === 'pipewire_bridge' ? 2 : 8;
}

function defaultInputSampleRate(requestedMode) {
  return requestedMode === 'pipewire_bridge' ? 192000 : 48000;
}

function bridgePathMissingMessage(requestedMode) {
  if (requestedMode !== 'pipe_bridge' && requestedMode !== 'pipewire_bridge') {
    return '';
  }
  const error = String(app.inputError || '').trim();
  if (!/bridge path missing|no bridge plugin found|render\.bridge_path/i.test(error)) {
    return '';
  }
  if (app.renderBridgePath) {
    return '';
  }
  return 'Bridge path missing';
}

export function buildInputConfigPayload() {
  const requestedMode = app.inputMode || 'pipe_bridge';
  return {
    mode: requestedMode,
    liveInput: {
      backend: app.liveInput.backend || null,
      node: app.liveInput.node || null,
      description: app.liveInput.description || null,
      layout: app.liveInput.layout || null,
      clockMode: app.liveInput.clockMode || 'dac',
      channels: app.liveInput.channels || defaultInputChannels(requestedMode),
      sampleRate: app.liveInput.sampleRate || defaultInputSampleRate(requestedMode),
      format: app.liveInput.format || null,
      map: app.liveInput.map || '7.1-fixed',
      lfeMode: app.liveInput.lfeMode || 'object'
    }
  };
}

export function sendInputConfig({ apply = false } = {}) {
  const payload = buildInputConfigPayload();
  return invoke('control_input_config', { payload }).then(() => {
    if (!apply) return null;
    return invoke('control_input_config_apply');
  });
}

export function updateInputControlUI() {
  const inputModeSelectEl = getInputModeSelectEl();
  const inputPipeInputEl = getInputPipeInputEl();
  const oscBridgePathInputEl = getOscBridgePathInputEl();
  const oscBridgePathStatusEl = getOscBridgePathStatusEl();
  const inputBackendSelectEl = getInputBackendSelectEl();
  const inputNodeInputEl = getInputNodeInputEl();
  const inputDescriptionInputEl = getInputDescriptionInputEl();
  const inputClockModeSelectEl = getInputClockModeSelectEl();
  const inputLayoutInputEl = getInputLayoutInputEl();
  const inputLayoutBrowseBtnEl = getInputLayoutBrowseBtnEl();
  const inputChannelsInputEl = getInputChannelsInputEl();
  const inputSampleRateInputEl = getInputSampleRateInputEl();
  const inputFormatSelectEl = getInputFormatSelectEl();
  const inputMapSelectEl = getInputMapSelectEl();
  const inputLfeModeSelectEl = getInputLfeModeSelectEl();
  const inputStatusInfoEl = getInputStatusInfoEl();
  const inputSummaryEl = getInputSummaryEl();
  const inputApplyBtnEl = getInputApplyBtnEl();
  const inputBridgeFieldsEl = getInputBridgeFieldsEl();
  const inputLiveFieldsEl = getInputLiveFieldsEl();
  const inputBackendRowEl = inputBackendSelectEl?.closest('.input-panel-row') || null;
  const inputPipeRowEl = inputPipeInputEl?.closest('.input-panel-row') || null;
  const inputNodeRowEl = inputNodeInputEl?.closest('.input-panel-row') || null;
  const inputDescriptionRowEl = inputDescriptionInputEl?.closest('.input-panel-row') || null;
  const inputClockModeRowEl = inputClockModeSelectEl?.closest('.input-panel-row') || null;
  const inputLayoutRowEl = inputLayoutInputEl?.closest('.input-panel-row') || null;
  const inputChannelsRowEl = inputChannelsInputEl?.closest('.input-panel-field') || null;
  const inputSampleRateRowEl = inputSampleRateInputEl?.closest('.input-panel-field') || null;
  const inputFormatRowEl = inputFormatSelectEl?.closest('.input-panel-field') || null;
  const inputMapRowEl = inputMapSelectEl?.closest('.input-panel-field') || null;
  const inputLfeModeRowEl = inputLfeModeSelectEl?.closest('.input-panel-field') || null;
  const requestedMode = app.inputMode || 'pipe_bridge';
  const hasInputDomain = hasProducerDomain('input');
  // Embedded (mpv) host: mpv owns the input stage, so the only relevant field
  // is the decoder bridge path. Show just that and hide the rest of the panel.
  const embedded = isEmbeddedProducer();
  if (inputModeSelectEl) {
    inputModeSelectEl.value = ['pipewire', 'pipewire_bridge', 'pipe_bridge'].includes(app.inputMode)
      ? app.inputMode
      : 'pipe_bridge';
    inputModeSelectEl.disabled = !hasInputDomain;
  }
  if (inputBackendSelectEl) {
    inputBackendSelectEl.value = app.liveInput.backend === 'asio' ? 'asio' : 'pipewire';
  }
  if (inputPipeInputEl && document.activeElement !== inputPipeInputEl) {
    inputPipeInputEl.value = stringOrEmpty(app.orenderInputPipe);
  }
  if (oscBridgePathInputEl && document.activeElement !== oscBridgePathInputEl) {
    oscBridgePathInputEl.value = stringOrEmpty(app.renderBridgePath);
  }
  if (inputNodeInputEl && document.activeElement !== inputNodeInputEl) {
    inputNodeInputEl.value = stringOrEmpty(app.liveInput.node || app.inputNode);
    inputNodeInputEl.placeholder = defaultNodePlaceholder(requestedMode);
  }
  if (inputDescriptionInputEl && document.activeElement !== inputDescriptionInputEl) {
    inputDescriptionInputEl.value = stringOrEmpty(app.liveInput.description || app.inputDescription);
    inputDescriptionInputEl.placeholder = defaultDescriptionPlaceholder(requestedMode);
  }
  if (inputClockModeSelectEl) {
    inputClockModeSelectEl.value = ['dac', 'pipewire', 'upstream'].includes(app.liveInput.clockMode)
      ? app.liveInput.clockMode
      : 'dac';
  }
  if (inputLayoutInputEl) {
    inputLayoutInputEl.value = stringOrEmpty(app.liveInput.layout);
  }
  if (inputChannelsInputEl) {
    inputChannelsInputEl.value = String(app.liveInput.channels || defaultInputChannels(requestedMode));
  }
  if (inputSampleRateInputEl) {
    inputSampleRateInputEl.value = String(app.liveInput.sampleRate || defaultInputSampleRate(requestedMode));
  }
  if (inputFormatSelectEl) {
    inputFormatSelectEl.value = app.liveInput.format === 's16' ? 's16' : 'f32';
  }
  if (inputMapSelectEl) {
    inputMapSelectEl.value = app.liveInput.map === '7.1-fixed' ? '7.1-fixed' : '7.1-fixed';
  }
  if (inputLfeModeSelectEl) {
    const value = app.liveInput.lfeMode;
    inputLfeModeSelectEl.value = ['object', 'direct', 'drop'].includes(value) ? value : 'object';
  }

  const showApplyPending = requestedMode !== 'pipe_bridge' && app.inputApplyPending;
  const bridgePathMissing = bridgePathMissingMessage(requestedMode);
  const bridgeRequested = requestedMode !== 'live';
  const liveRequested = requestedMode === 'pipewire';
  const pipewireBridgeRequested = requestedMode === 'pipewire_bridge';
  const endpointRequested = liveRequested || pipewireBridgeRequested;

  if (inputBridgeFieldsEl) {
    inputBridgeFieldsEl.style.display =
      embedded || (hasInputDomain && bridgeRequested) ? '' : 'none';
  }
  // In embedded mode the panel becomes the "Decoder bridge" section: relabel
  // the header and keep only the bridge path — hide the mode selector, the
  // status line, the redundant "Bridge Input" subtitle and the Apply action
  // (live fields are already hidden with no input domain, and the Pipe row is
  // hidden by `inputPipeRowEl` below).
  const titleEl = document.querySelector('#audioInputSection .panel-title');
  if (titleEl) {
    const key = embedded ? 'section.decoderBridge' : 'section.audioInput';
    titleEl.setAttribute('data-i18n', key);
    titleEl.textContent = t(key);
  }
  if (embedded) {
    const modeGroupEl =
      inputModeSelectEl?.closest('.input-panel-grid') ||
      inputModeSelectEl?.closest('.input-panel-row') ||
      null;
    if (modeGroupEl) modeGroupEl.style.display = 'none';
    const statusWrapEl = inputStatusInfoEl?.parentElement || null;
    if (statusWrapEl) statusWrapEl.style.display = 'none';
    const applyActionsEl = inputApplyBtnEl?.closest('.input-panel-actions') || null;
    if (applyActionsEl) applyActionsEl.style.display = 'none';
    const bridgeSubtitleEl = inputBridgeFieldsEl?.querySelector('.input-panel-subtitle') || null;
    if (bridgeSubtitleEl) bridgeSubtitleEl.style.display = 'none';
  }
  if (oscBridgePathStatusEl) {
    oscBridgePathStatusEl.textContent = bridgePathMissing;
    oscBridgePathStatusEl.style.display = bridgePathMissing ? 'block' : 'none';
  }
  if (oscBridgePathInputEl) {
    oscBridgePathInputEl.classList.toggle('input-panel-danger', Boolean(bridgePathMissing));
  }
  if (inputLiveFieldsEl) {
    inputLiveFieldsEl.style.display = hasInputDomain && endpointRequested ? '' : 'none';
    inputLiveFieldsEl.style.opacity = hasInputDomain && endpointRequested ? '1' : '0.55';
  }
  if (inputPipeRowEl) inputPipeRowEl.style.display = hasInputDomain && requestedMode === 'pipe_bridge' ? '' : 'none';
  if (inputBackendRowEl) inputBackendRowEl.style.display = hasInputDomain && liveRequested ? '' : 'none';
  if (inputNodeRowEl) inputNodeRowEl.style.display = hasInputDomain && endpointRequested ? '' : 'none';
  if (inputDescriptionRowEl) inputDescriptionRowEl.style.display = hasInputDomain && endpointRequested ? '' : 'none';
  if (inputClockModeRowEl) inputClockModeRowEl.style.display = hasInputDomain && pipewireBridgeRequested ? '' : 'none';
  if (inputLayoutRowEl) inputLayoutRowEl.style.display = hasInputDomain && liveRequested ? '' : 'none';
  if (inputChannelsRowEl) inputChannelsRowEl.style.display = hasInputDomain && liveRequested ? '' : 'none';
  if (inputSampleRateRowEl) inputSampleRateRowEl.style.display = hasInputDomain && liveRequested ? '' : 'none';
  if (inputFormatRowEl) inputFormatRowEl.style.display = hasInputDomain && liveRequested ? '' : 'none';
  if (inputMapRowEl) inputMapRowEl.style.display = hasInputDomain && liveRequested ? '' : 'none';
  if (inputLfeModeRowEl) inputLfeModeRowEl.style.display = hasInputDomain && liveRequested ? '' : 'none';
  [
    inputBackendSelectEl,
    inputNodeInputEl,
    inputDescriptionInputEl,
    inputClockModeSelectEl,
    inputChannelsInputEl,
    inputSampleRateInputEl,
    inputFormatSelectEl,
    inputMapSelectEl,
    inputLfeModeSelectEl
  ].forEach((el) => {
    if (el) {
      if (!hasInputDomain) {
        el.disabled = true;
        return;
      }
      if (el === inputNodeInputEl || el === inputDescriptionInputEl) {
        el.disabled = !endpointRequested;
      } else if (el === inputClockModeSelectEl) {
        el.disabled = !pipewireBridgeRequested;
      } else {
        el.disabled = !liveRequested;
      }
    }
  });
  if (inputLayoutInputEl) {
    inputLayoutInputEl.disabled = !hasInputDomain || !liveRequested;
  }
  if (inputLayoutBrowseBtnEl) {
    inputLayoutBrowseBtnEl.disabled = !hasInputDomain || !liveRequested;
  }

  if (inputStatusInfoEl) {
    const activeMode = app.inputActiveMode || 'pipe_bridge';
    const requestedModeLabel = formatInputModeLabel(requestedMode);
    const activeModeLabel = formatInputModeLabel(activeMode);
    const sync = showApplyPending ? t('input.sync.pending') : t('input.sync.synced');
    const error = app.inputError ? tf('input.status.error', { error: app.inputError }) : '';
    if (liveRequested) {
      const backend = formatInputBackendLabel(app.inputBackend || app.liveInput.backend || '');
      const channels = app.inputChannels || app.liveInput.channels || '—';
      const sampleRate = app.inputSampleRate || app.liveInput.sampleRate || '—';
      const format = app.inputStreamFormat || app.liveInput.format || '—';
      inputStatusInfoEl.textContent = tf('input.status.live', {
        requested: requestedModeLabel,
        active: activeModeLabel,
        backend,
        channels,
        sampleRate,
        format,
        sync
      }) + error;
    } else {
      const pipe = app.orenderInputPipe || '—';
      const clock = pipewireBridgeRequested
        ? tf('input.status.clock', { clock: formatClockModeLabel(app.liveInput.clockMode || 'dac') })
        : '';
      inputStatusInfoEl.textContent = tf('input.status.bridge', {
        requested: requestedModeLabel,
        active: activeModeLabel,
        pipe,
        sync
      }) + clock + error;
    }
  }

  if (inputSummaryEl) {
    const activeMode = app.inputActiveMode || 'pipe_bridge';
    const requestedModeLabel = formatInputModeLabel(requestedMode);
    const activeModeLabel = formatInputModeLabel(activeMode);
    if (embedded) {
      // In mpv mode the panel is just the decoder bridge path; the requested/
      // active mode summary is meaningless. Show the bridge path instead.
      const bridgePath = String(app.renderBridgePath || '').trim();
      inputSummaryEl.textContent = bridgePath || t('input.autoDetect');
    } else if (liveRequested) {
      const backend = formatInputBackendLabel(app.liveInput.backend || 'pipewire');
      const layoutSuffix = app.liveInput.layout ? t('input.summary.liveLayout') : '';
      inputSummaryEl.textContent = tf('input.summary.live', {
        requested: requestedModeLabel,
        active: activeModeLabel,
        backend
      }) + layoutSuffix;
    } else if (pipewireBridgeRequested) {
      inputSummaryEl.textContent = tf('input.summary.pipewireBridge', {
        requested: requestedModeLabel,
        active: activeModeLabel,
        clock: formatClockModeLabel(app.liveInput.clockMode || 'dac')
      });
    } else {
      inputSummaryEl.textContent = tf('input.summary.bridge', {
        requested: requestedModeLabel,
        active: activeModeLabel
      });
    }
  }

  if (inputApplyBtnEl) {
    inputApplyBtnEl.textContent = showApplyPending ? t('input.applyPending') : t('input.apply');
  }
}

export function persistInputPipeNow() {
  const inputPipeInputEl = getInputPipeInputEl();
  const value = String(inputPipeInputEl?.value || '').trim();
  app.orenderInputPipe = value || null;
  return invoke('control_render_input_pipe', { value });
}
