import { invoke } from '@tauri-apps/api/core';
import { app } from '../state.js';
import { tf } from '../i18n.js';
import { pushLog, normalizeLogError } from '../log.js';
import { updateInputControlUI, persistInputPipeNow, sendInputConfig } from '../controls/input.js';

export function setupInputPanelListeners() {
  const inputModeSelectEl = document.getElementById('inputModeSelect');
  const inputPipeInputEl = document.getElementById('pipeStatus');
  const oscBridgePathInputEl = document.getElementById('oscBridgePathInput');
  const oscBridgeBrowseBtnEl = document.getElementById('oscBridgeBrowseBtn');
  const inputBackendSelectEl = document.getElementById('inputBackendSelect');
  const inputNodeInputEl = document.getElementById('inputNodeInput');
  const inputDescriptionInputEl = document.getElementById('inputDescriptionInput');
  const inputClockModeSelectEl = document.getElementById('inputClockModeSelect');
  const inputLayoutBrowseBtnEl = document.getElementById('inputLayoutBrowseBtn');
  const inputChannelsInputEl = document.getElementById('inputChannelsInput');
  const inputSampleRateInputEl = document.getElementById('inputSampleRateInput');
  const inputMapSelectEl = document.getElementById('inputMapSelect');
  const inputLfeModeSelectEl = document.getElementById('inputLfeModeSelect');
  const inputApplyBtnEl = document.getElementById('inputApplyBtn');

  if (inputModeSelectEl) {
    inputModeSelectEl.addEventListener('change', () => {
      const previousMode = app.inputMode;
      const value = ['pipewire', 'pipe_bridge'].includes(inputModeSelectEl.value)
        ? inputModeSelectEl.value
        : 'pipe_bridge';
      app.inputMode = value;
      app.inputModeDirty = true;
      if (value === 'pipewire' && previousMode !== 'pipewire') {
        app.liveInput.channels = 2;
        app.liveInput.sampleRate = 192000;
      }
      updateInputControlUI();
      sendInputConfig();
    });
  }

  if (inputPipeInputEl) {
    inputPipeInputEl.addEventListener('change', () => {
      persistInputPipeNow()
        .catch((e) => {
          pushLog('error', `Failed to set input pipe: ${normalizeLogError(e)}`);
        })
        .finally(() => {
          updateInputControlUI();
        });
    });
  }

  if (oscBridgeBrowseBtnEl) {
    oscBridgeBrowseBtnEl.addEventListener('click', () => {
      invoke('pick_bridge_path')
        .then((selectedPath) => {
          const trimmed = String(selectedPath || '').trim();
          if (trimmed && oscBridgePathInputEl) {
            oscBridgePathInputEl.value = trimmed;
            oscBridgePathInputEl.dispatchEvent(new Event('change', { bubbles: true }));
          }
        })
        .catch((e) => {
          pushLog('error', `Failed to select bridge: ${normalizeLogError(e)}`);
        });
    });
  }

  if (oscBridgePathInputEl) {
    oscBridgePathInputEl.addEventListener('change', () => {
      const value = String(oscBridgePathInputEl.value || '').trim();
      app.renderBridgePath = value || null;
      updateInputControlUI();
      invoke('control_render_bridge_path', { value });
    });
  }

  if (inputBackendSelectEl) {
    inputBackendSelectEl.addEventListener('change', () => {
      const value = inputBackendSelectEl.value === 'asio' ? 'asio' : 'pipewire';
      app.liveInput.backend = value;
      updateInputControlUI();
      sendInputConfig();
    });
  }

  if (inputNodeInputEl) {
    inputNodeInputEl.addEventListener('change', () => {
      const value = String(inputNodeInputEl.value || '');
      app.liveInput.node = value;
      updateInputControlUI();
      sendInputConfig();
    });
  }

  if (inputDescriptionInputEl) {
    inputDescriptionInputEl.addEventListener('change', () => {
      const value = String(inputDescriptionInputEl.value || '');
      app.liveInput.description = value;
      updateInputControlUI();
      sendInputConfig();
    });
  }

  if (inputClockModeSelectEl) {
    inputClockModeSelectEl.addEventListener('change', () => {
      const value = ['dac', 'pipewire', 'upstream'].includes(inputClockModeSelectEl.value)
        ? inputClockModeSelectEl.value
        : 'dac';
      app.liveInput.clockMode = value;
      app.liveInputClockModeDirty = true;
      updateInputControlUI();
    });
  }

  if (inputLayoutBrowseBtnEl) {
    inputLayoutBrowseBtnEl.addEventListener('click', () => {
      invoke('pick_import_layout_path')
        .then((path) => {
          const trimmed = typeof path === 'string' ? path.trim() : '';
          if (!trimmed) return;
          pushLog('info', tf('log.layoutImportRequested', { path: trimmed }));
          return invoke('import_input_layout_from_path', { path: trimmed })
            .then((payload) => {
              const importedPath = String(payload?.path || trimmed);
              app.liveInput.layout = importedPath;
              updateInputControlUI();
              sendInputConfig();
              pushLog('info', tf('log.layoutImported', { path: importedPath }));
            });
        })
        .catch((e) => {
          console.error('[input layout import]', e);
          pushLog('error', tf('log.layoutImportFailed', { error: normalizeLogError(e) }));
        });
    });
  }

  if (inputChannelsInputEl) {
    inputChannelsInputEl.addEventListener('change', () => {
      const fallback = 2;
      const value = Math.max(1, Math.round(Number(inputChannelsInputEl.value) || fallback));
      app.liveInput.channels = value;
      updateInputControlUI();
      sendInputConfig();
    });
  }

  if (inputSampleRateInputEl) {
    inputSampleRateInputEl.addEventListener('change', () => {
      const fallback = 192000;
      const value = Math.max(1, Math.round(Number(inputSampleRateInputEl.value) || fallback));
      app.liveInput.sampleRate = value;
      updateInputControlUI();
      sendInputConfig();
    });
  }

  if (inputMapSelectEl) {
    inputMapSelectEl.addEventListener('change', () => {
      const value = '7.1-fixed';
      app.liveInput.map = value;
      updateInputControlUI();
      sendInputConfig();
    });
  }

  if (inputLfeModeSelectEl) {
    inputLfeModeSelectEl.addEventListener('change', () => {
      const raw = String(inputLfeModeSelectEl.value || '').trim().toLowerCase();
      const value = ['object', 'direct', 'drop'].includes(raw) ? raw : 'object';
      app.liveInput.lfeMode = value;
      updateInputControlUI();
      sendInputConfig();
    });
  }

  if (inputApplyBtnEl) {
    inputApplyBtnEl.addEventListener('click', () => {
      const requestedMode = app.inputMode || 'pipe_bridge';
      const activeMode = app.inputActiveMode || 'pipe_bridge';
      const needsBridgeBootstrap =
        requestedMode === 'pipe_bridge'
        || (requestedMode === 'pipewire' && activeMode !== 'pipewire');
      if (needsBridgeBootstrap) {
        const value = String(oscBridgePathInputEl?.value || '').trim();
        const clockMode = app.liveInput.clockMode || 'dac';
        app.inputApplyPending = false;
        app.inputApplyAwaitingAck = false;
        updateInputControlUI();
        invoke('control_render_bridge_path', { value })
          .then(() => invoke('control_input_live_clock_mode', { value: clockMode }))
          .then(() => invoke('control_save_config'))
          .then(() => invoke('control_reload_config'))
          .catch((e) => {
            pushLog('error', `Failed to apply bridge path: ${normalizeLogError(e)}`);
          });
        return;
      }
      app.inputApplyPending = true;
      app.inputApplyAwaitingAck = true;
      updateInputControlUI();
      invoke('control_input_live_clock_mode', { value: app.liveInput.clockMode || 'dac' })
        .then(() => sendInputConfig({ apply: true }))
        .catch((e) => {
          pushLog('error', `Failed to apply input clock mode: ${normalizeLogError(e)}`);
          app.inputApplyPending = false;
          app.inputApplyAwaitingAck = false;
          updateInputControlUI();
        });
    });
  }
}
