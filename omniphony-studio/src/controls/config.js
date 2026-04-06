/**
 * Config saved indicator.
 */

import { app, dirty } from '../state.js';
import { scheduleUIFlush, flushCallbacks } from '../flush.js';
import { inAudioPanel, inSaveFooter } from '../ui/panel-roots.js';

const configSavedIndicatorEl = inAudioPanel('configSavedIndicator');
const saveConfigBtnEl = inSaveFooter('saveConfigBtn');

export function renderConfigSavedUI() {
  if (!configSavedIndicatorEl) return;
  configSavedIndicatorEl.textContent = '';
  if (saveConfigBtnEl) {
    const alreadySaved = app.configSaved === true;
    saveConfigBtnEl.disabled = alreadySaved;
    saveConfigBtnEl.style.opacity = alreadySaved ? '0.5' : '1';
    saveConfigBtnEl.style.cursor = alreadySaved ? 'default' : 'pointer';
  }
}

export function updateConfigSavedUI() {
  dirty.configSaved = true;
  scheduleUIFlush();
}

// Wire render function into the flush callback registry.
flushCallbacks.renderConfigSavedUI = renderConfigSavedUI;
