import { app } from '../state.js';

const inputModeSelectEl = document.getElementById('inputModeSelect');
const inputBackendSelectEl = document.getElementById('inputBackendSelect');
const inputNodeInputEl = document.getElementById('inputNodeInput');
const inputDescriptionInputEl = document.getElementById('inputDescriptionInput');
const inputLayoutInputEl = document.getElementById('inputLayoutInput');
const inputChannelsInputEl = document.getElementById('inputChannelsInput');
const inputSampleRateInputEl = document.getElementById('inputSampleRateInput');
const inputFormatSelectEl = document.getElementById('inputFormatSelect');
const inputMapSelectEl = document.getElementById('inputMapSelect');
const inputLfeModeSelectEl = document.getElementById('inputLfeModeSelect');
const inputStatusInfoEl = document.getElementById('inputStatusInfo');
const inputSummaryEl = document.getElementById('inputSummary');
const inputApplyBtnEl = document.getElementById('inputApplyBtn');
const inputLiveFieldsEl = document.getElementById('inputLiveFields');

function stringOrEmpty(value) {
  return typeof value === 'string' ? value : '';
}

export function updateInputControlUI() {
  if (inputModeSelectEl) {
    inputModeSelectEl.value = ['live', 'pipewire_bridge'].includes(app.inputMode)
      ? app.inputMode
      : 'bridge';
  }
  if (inputBackendSelectEl) {
    inputBackendSelectEl.value = app.liveInput.backend === 'asio' ? 'asio' : 'pipewire';
  }
  if (inputNodeInputEl) {
    inputNodeInputEl.value = stringOrEmpty(app.liveInput.node);
  }
  if (inputDescriptionInputEl) {
    inputDescriptionInputEl.value = stringOrEmpty(app.liveInput.description);
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

  const liveRequested = app.inputMode !== 'bridge';
  if (inputLiveFieldsEl) {
    inputLiveFieldsEl.style.opacity = liveRequested ? '1' : '0.55';
  }
  [
    inputBackendSelectEl,
    inputNodeInputEl,
    inputDescriptionInputEl,
    inputLayoutInputEl,
    inputChannelsInputEl,
    inputSampleRateInputEl,
    inputFormatSelectEl,
    inputMapSelectEl,
    inputLfeModeSelectEl
  ].forEach((el) => {
    if (el) {
      el.disabled = !liveRequested;
    }
  });

  if (inputStatusInfoEl) {
    const activeMode = app.inputActiveMode || 'bridge';
    const requestedMode = app.inputMode || 'bridge';
    const backend = app.inputBackend || app.liveInput.backend || '—';
    const channels = app.inputChannels || app.liveInput.channels || '—';
    const sampleRate = app.inputSampleRate || app.liveInput.sampleRate || '—';
    const format = app.inputStreamFormat || app.liveInput.format || '—';
    const pending = app.inputApplyPending ? 'pending apply' : 'synced';
    const error = app.inputError ? ` • error: ${app.inputError}` : '';
    inputStatusInfoEl.textContent =
      `requested ${requestedMode} • active ${activeMode} • ${backend} • ${channels} ch • ${sampleRate} Hz • ${format} • ${pending}${error}`;
  }

  if (inputSummaryEl) {
    const requestedMode = app.inputMode || 'bridge';
    const activeMode = app.inputActiveMode || 'bridge';
    const backend = requestedMode === 'bridge' ? 'bridge' : (app.liveInput.backend || 'pipewire');
    inputSummaryEl.textContent = `${requestedMode} • active ${activeMode} • ${backend}`;
  }

  if (inputApplyBtnEl) {
    inputApplyBtnEl.textContent = app.inputApplyPending ? 'Apply pending…' : 'Apply';
  }
}
