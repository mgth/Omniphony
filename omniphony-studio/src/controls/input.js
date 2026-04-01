import { app } from '../state.js';
import { invoke } from '@tauri-apps/api/core';

const inputModeSelectEl = document.getElementById('inputModeSelect');
const inputPipeInputEl = document.getElementById('pipeStatus');
const inputBackendSelectEl = document.getElementById('inputBackendSelect');
const inputNodeInputEl = document.getElementById('inputNodeInput');
const inputDescriptionInputEl = document.getElementById('inputDescriptionInput');
const inputClockModeSelectEl = document.getElementById('inputClockModeSelect');
const inputLayoutInputEl = document.getElementById('inputLayoutInput');
const inputLayoutBrowseBtnEl = document.getElementById('inputLayoutBrowseBtn');
const inputChannelsInputEl = document.getElementById('inputChannelsInput');
const inputSampleRateInputEl = document.getElementById('inputSampleRateInput');
const inputFormatSelectEl = document.getElementById('inputFormatSelect');
const inputMapSelectEl = document.getElementById('inputMapSelect');
const inputLfeModeSelectEl = document.getElementById('inputLfeModeSelect');
const inputStatusInfoEl = document.getElementById('inputStatusInfo');
const inputSummaryEl = document.getElementById('inputSummary');
const inputApplyBtnEl = document.getElementById('inputApplyBtn');
const inputBridgeFieldsEl = document.getElementById('inputBridgeFields');
const inputLiveFieldsEl = document.getElementById('inputLiveFields');
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

function stringOrEmpty(value) {
  return typeof value === 'string' ? value : '';
}

function formatInputModeLabel(value) {
  switch (value) {
    case 'bridge':
    case 'pipe_bridge':
      return 'Pipe bridge';
    case 'pipewire_bridge':
      return 'PipeWire bridge';
    case 'live':
    case 'pipewire':
      return 'PipeWire';
    default:
      return value || '—';
  }
}

export function updateInputControlUI() {
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
  if (inputNodeInputEl) {
    inputNodeInputEl.value = stringOrEmpty(app.liveInput.node);
  }
  if (inputDescriptionInputEl) {
    inputDescriptionInputEl.value = stringOrEmpty(app.liveInput.description);
  }
  if (inputClockModeSelectEl) {
    inputClockModeSelectEl.value = app.liveInput.clockMode === 'pipewire' ? 'pipewire' : 'dac';
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

  const requestedMode = app.inputMode || 'pipe_bridge';
  const bridgeRequested = requestedMode !== 'live';
  const liveRequested = requestedMode === 'pipewire';
  const pipewireBridgeRequested = requestedMode === 'pipewire_bridge';
  const endpointRequested = liveRequested || pipewireBridgeRequested;

  if (inputBridgeFieldsEl) {
    inputBridgeFieldsEl.style.display = bridgeRequested ? '' : 'none';
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
    const pending = app.inputApplyPending ? 'pending apply' : 'synced';
    const error = app.inputError ? ` • error: ${app.inputError}` : '';
    if (liveRequested) {
      const backend = app.inputBackend || app.liveInput.backend || '—';
      const channels = app.inputChannels || app.liveInput.channels || '—';
      const sampleRate = app.inputSampleRate || app.liveInput.sampleRate || '—';
      const format = app.inputStreamFormat || app.liveInput.format || '—';
      inputStatusInfoEl.textContent =
        `requested ${requestedModeLabel} • active ${activeModeLabel} • ${backend} • ${channels} ch • ${sampleRate} Hz • ${format} • ${pending}${error}`;
    } else {
      const pipe = app.orenderInputPipe || '—';
      const clockMode = pipewireBridgeRequested ? ` • clock ${app.liveInput.clockMode || 'dac'}` : '';
      inputStatusInfoEl.textContent =
        `requested ${requestedModeLabel} • active ${activeModeLabel} • pipe ${pipe}${clockMode} • ${pending}${error}`;
    }
  }

  if (inputSummaryEl) {
    const activeMode = app.inputActiveMode || 'pipe_bridge';
    const requestedModeLabel = formatInputModeLabel(requestedMode);
    const activeModeLabel = formatInputModeLabel(activeMode);
    if (liveRequested) {
      const backend = app.liveInput.backend || 'pipewire';
      const layout = app.liveInput.layout ? ' • imported layout' : '';
      inputSummaryEl.textContent = `${requestedModeLabel} • active ${activeModeLabel} • ${backend}${layout}`;
    } else if (pipewireBridgeRequested) {
      inputSummaryEl.textContent = `${requestedModeLabel} • active ${activeModeLabel} • ${app.liveInput.clockMode || 'dac'} clock`;
    } else {
      inputSummaryEl.textContent = `${requestedModeLabel} • active ${activeModeLabel} • pipe`;
    }
  }

  if (inputApplyBtnEl) {
    inputApplyBtnEl.textContent = app.inputApplyPending ? 'Apply pending…' : 'Apply';
  }
}

export function persistInputPipeNow() {
  const value = String(inputPipeInputEl?.value || '').trim();
  app.orenderInputPipe = value || null;
  return invoke('save_input_pipe_pref', { inputPipe: app.orenderInputPipe })
    .catch((e) => {
      console.error('[save_input_pipe_pref]', e);
    });
}
