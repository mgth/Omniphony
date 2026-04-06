/**
 * UI event listener registrations.
 *
 * Extracted from app.js lines 6772-8028 (plus locale/layout select listeners).
 * Every `if (xxxEl) { xxxEl.addEventListener(...) }` block and every
 * `document.addEventListener(...)` call lives here.
 */

import { invoke } from '@tauri-apps/api/core';
import { app, sourceNames } from './state.js';
import { t, tf, i18nState, normalizeLocalePreference, applyStaticTranslations, LOCALE_STORAGE_KEY } from './i18n.js';
import {
  pushLog, logState, renderLogPanel, renderLogLevelControl,
  normalizeLogLevel, normalizeLogError, setLogExpanded, copyLogsToClipboard
} from './log.js';
import { setOscStatus } from './controls/osc.js';
import { setupAudioPanelListeners } from './listeners/audio-panel-listeners.js';
import { setupInputPanelListeners } from './listeners/input-panel-listeners.js';
import { setupRendererPanelListeners } from './listeners/renderer-panel-listeners.js';
import { setupModalAndToggleListeners } from './listeners/modal-and-toggle-listeners.js';
import { setupRoomGeometryListeners } from './listeners/room-geometry-listeners.js';
import { setupSpeakerEditorListeners } from './listeners/speaker-editor-listeners.js';
import { setupLayoutListeners } from './listeners/layout-listeners.js';
import { setupTrailsAndDisplayListeners } from './listeners/trails-and-display-listeners.js';

export function setupUIListeners() {
  setupAudioPanelListeners();
  setupInputPanelListeners();
  setupRendererPanelListeners();
  setupModalAndToggleListeners();
  setupRoomGeometryListeners();
  setupSpeakerEditorListeners();
  setupLayoutListeners();
  setupTrailsAndDisplayListeners();

  // ── DOM element queries ─────────────────────────────────────────────────

  const saveConfigBtnEl = document.getElementById('saveConfigBtn');
  const reloadConfigBtnEl = document.getElementById('reloadConfigBtn');
  const logToggleBtnEl = document.getElementById('logToggleBtn');
  const logClearBtnEl = document.getElementById('logClearBtn');
  const logCopyBtnEl = document.getElementById('logCopyBtn');
  const logLevelSelectEl = document.getElementById('logLevelSelect');
  // ── Save / reload config ────────────────────────────────────────────────

  if (saveConfigBtnEl) {
    saveConfigBtnEl.addEventListener('click', () => {
      pushLog('info', t('log.saveRequested'));
      invoke('control_save_config');
    });
  }

  if (reloadConfigBtnEl) {
    reloadConfigBtnEl.addEventListener('click', () => {
      pushLog('info', t('log.reloadRequested'));
      invoke('control_reload_config');
    });
  }

  // ── Log panel ───────────────────────────────────────────────────────────

  if (logToggleBtnEl) {
    logToggleBtnEl.addEventListener('click', () => {
      setLogExpanded(!logState.expanded);
    });
  }

  if (logClearBtnEl) {
    logClearBtnEl.addEventListener('click', () => {
      logState.entries = [];
      renderLogPanel();
    });
  }

  if (logCopyBtnEl) {
    logCopyBtnEl.addEventListener('click', () => {
      copyLogsToClipboard();
    });
  }

  if (logLevelSelectEl) {
    logLevelSelectEl.addEventListener('change', () => {
      const value = normalizeLogLevel(logLevelSelectEl.value);
      logState.backendLogLevel = value;
      renderLogLevelControl();
      pushLog('info', tf('log.levelChanged', { value }));
      invoke('control_log_level', { value }).catch((e) => {
        pushLog('error', tf('log.oscConfigFailed', { error: normalizeLogError(e) }));
      });
    });
  }

  // ── Boot-time calls ─────────────────────────────────────────────────────

  applyStaticTranslations();
  setOscStatus('initializing');
  pushLog('info', t('log.boot'));
}
