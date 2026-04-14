import { app } from '../state.js';
import { t, tf } from '../i18n.js';
import { inInputPanel } from '../ui/panel-roots.js';

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
  if (inputModeSelectEl) {
    inputModeSelectEl.value = ['pipewire', 'pipewire_bridge', 'pipe_bridge'].includes(app.inputMode)
      ? app.inputMode
      : 'pipe_bridge';
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
    inputChannelsInputEl.value = String(app.liveInput.channels || 8);
  }
  if (inputSampleRateInputEl) {
    inputSampleRateInputEl.value = String(app.liveInput.sampleRate || 48000);
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
    inputBridgeFieldsEl.style.display = bridgeRequested ? '' : 'none';
  }
  if (oscBridgePathStatusEl) {
    oscBridgePathStatusEl.textContent = bridgePathMissing;
    oscBridgePathStatusEl.style.display = bridgePathMissing ? 'block' : 'none';
  }
  if (oscBridgePathInputEl) {
    oscBridgePathInputEl.classList.toggle('input-panel-danger', Boolean(bridgePathMissing));
  }
  if (inputLiveFieldsEl) {
    inputLiveFieldsEl.style.display = endpointRequested ? '' : 'none';
    inputLiveFieldsEl.style.opacity = endpointRequested ? '1' : '0.55';
  }
  if (inputPipeRowEl) inputPipeRowEl.style.display = requestedMode === 'pipe_bridge' ? '' : 'none';
  if (inputBackendRowEl) inputBackendRowEl.style.display = liveRequested ? '' : 'none';
  if (inputNodeRowEl) inputNodeRowEl.style.display = endpointRequested ? '' : 'none';
  if (inputDescriptionRowEl) inputDescriptionRowEl.style.display = endpointRequested ? '' : 'none';
  if (inputClockModeRowEl) inputClockModeRowEl.style.display = pipewireBridgeRequested ? '' : 'none';
  if (inputLayoutRowEl) inputLayoutRowEl.style.display = liveRequested ? '' : 'none';
  if (inputChannelsRowEl) inputChannelsRowEl.style.display = liveRequested ? '' : 'none';
  if (inputSampleRateRowEl) inputSampleRateRowEl.style.display = liveRequested ? '' : 'none';
  if (inputFormatRowEl) inputFormatRowEl.style.display = liveRequested ? '' : 'none';
  if (inputMapRowEl) inputMapRowEl.style.display = liveRequested ? '' : 'none';
  if (inputLfeModeRowEl) inputLfeModeRowEl.style.display = liveRequested ? '' : 'none';
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
    inputLayoutInputEl.disabled = !liveRequested;
  }
  if (inputLayoutBrowseBtnEl) {
    inputLayoutBrowseBtnEl.disabled = !liveRequested;
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
    if (liveRequested) {
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
  const value = String(inputPipeInputEl?.value || '').trim();
  app.orenderInputPipe = value || null;
  return Promise.resolve();
}
