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
  normalizeLogLevel, normalizeLogError, setLogExpanded, copyLogsToClipboard, setLogFilterText
} from './log.js';
import { setOscStatus } from './controls/osc.js';
import { updateConfigSavedUI } from './controls/config.js';
import { setupAudioPanelListeners } from './listeners/audio-panel-listeners.js';
import { setupInputPanelListeners } from './listeners/input-panel-listeners.js';
import { setupRendererPanelListeners } from './listeners/renderer-panel-listeners.js';
import { setupModalAndToggleListeners } from './listeners/modal-and-toggle-listeners.js';
import { setupRoomGeometryListeners } from './listeners/room-geometry-listeners.js';
import { setupSpeakerEditorListeners } from './listeners/speaker-editor-listeners.js';
import { setupSpeakerTestListeners } from './controls/speaker-test.js';
import { setupObjectTestListeners } from './controls/object-test.js';
import { setupChannelEditorListeners } from './listeners/channel-editor-listeners.js';
import { setupLayoutListeners } from './listeners/layout-listeners.js';
import { setupTrailsAndDisplayListeners } from './listeners/trails-and-display-listeners.js';

export function setupUIListeners() {
  setupAudioPanelListeners();
  setupInputPanelListeners();
  setupRendererPanelListeners();
  setupModalAndToggleListeners();
  setupRoomGeometryListeners();
  setupSpeakerEditorListeners();
  setupSpeakerTestListeners();
  setupObjectTestListeners();
  setupChannelEditorListeners();
  setupLayoutListeners();
  setupTrailsAndDisplayListeners();

  // ── DOM element queries ─────────────────────────────────────────────────

  const saveConfigBtnEl = document.getElementById('saveConfigBtn');
  const reloadConfigBtnEl = document.getElementById('reloadConfigBtn');
  const logToggleBtnEl = document.getElementById('logToggleBtn');
  const logClearBtnEl = document.getElementById('logClearBtn');
  const logCopyBtnEl = document.getElementById('logCopyBtn');
  const logLevelSelectEl = document.getElementById('logLevelSelect');
  const logFilterInputEl = document.getElementById('logFilterInput');
  // ── Save / reload config ────────────────────────────────────────────────

  if (saveConfigBtnEl) {
    saveConfigBtnEl.addEventListener('click', () => {
      if (!app.oscSnapshotReady) return;
      pushLog('info', t('log.saveRequested'));
      app.saveRequested = true;
      app.saveError = null;
      invoke('control_save_config');
      updateConfigSavedUI();
    });
  }

  if (reloadConfigBtnEl) {
    reloadConfigBtnEl.addEventListener('click', () => {
      if (!app.oscSnapshotReady) return;
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

  if (logFilterInputEl) {
    logFilterInputEl.addEventListener('input', () => {
      setLogFilterText(logFilterInputEl.value);
    });
  }

  // ── Boot-time calls ─────────────────────────────────────────────────────

  applyStaticTranslations();
  setOscStatus('initializing');
  pushLog('info', t('log.boot'));
}
