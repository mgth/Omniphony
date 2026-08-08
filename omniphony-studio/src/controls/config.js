/**
 * Config saved indicator.
 */

import { app, dirty } from '../state.js';
import { scheduleUIFlush, flushCallbacks } from '../flush.js';
import { inAudioPanel, inSaveFooter } from '../ui/panel-roots.js';
import { t } from '../i18n.js';

function getConfigSavedIndicatorEl() { return inAudioPanel('configSavedIndicator'); }
function getSaveConfigBtnEl() { return inSaveFooter('saveConfigBtn'); }
function getReloadConfigBtnEl() { return inSaveFooter('reloadConfigBtn'); }

export function renderConfigSavedUI() {
  const configSavedIndicatorEl = getConfigSavedIndicatorEl();
  const saveConfigBtnEl = getSaveConfigBtnEl();
  const reloadConfigBtnEl = getReloadConfigBtnEl();
  const runtimeConnected = app.oscSnapshotReady === true;
  if (configSavedIndicatorEl) {
    if (typeof app.saveError === 'string' && app.saveError.length > 0) {
      configSavedIndicatorEl.textContent = app.saveError;
      configSavedIndicatorEl.title = app.saveError;
      configSavedIndicatorEl.style.color = '#ff7676';
    } else {
      configSavedIndicatorEl.textContent = '';
      configSavedIndicatorEl.removeAttribute('title');
      configSavedIndicatorEl.style.color = '';
    }
  }
  if (saveConfigBtnEl) {
    const alreadySaved = app.configSaved === true;
    const pending = app.saveRequested === true;
    const enabled = runtimeConnected && !alreadySaved && !pending;
    saveConfigBtnEl.disabled = !enabled;
    saveConfigBtnEl.style.opacity = enabled ? '1' : '0.5';
    saveConfigBtnEl.style.cursor = enabled ? 'pointer' : 'default';
  }
  if (reloadConfigBtnEl) {
    reloadConfigBtnEl.disabled = !runtimeConnected;
    reloadConfigBtnEl.style.opacity = runtimeConnected ? '1' : '0.5';
    reloadConfigBtnEl.style.cursor = runtimeConnected ? 'pointer' : 'default';
  }
}

export function updateConfigSavedUI() {
  dirty.configSaved = true;
  scheduleUIFlush();
}

// Reflect the connected renderer's effective config path + load status in the
// About modal. A path that didn't actually load (missing / parse_error) is the
// smoking gun for a CLI-vs-host (e.g. mpv) mismatch — the renderer silently
// runs on built-in defaults — so we call it out in red rather than showing a
// reassuring-looking path.
export function updateAboutConfigPath() {
  const el = document.getElementById('aboutConfigPath');
  if (!el) return;
  const path = typeof app.renderConfigPath === 'string' ? app.renderConfigPath.trim() : '';
  const status = typeof app.renderConfigStatus === 'string' ? app.renderConfigStatus.trim() : '';
  if (path) {
    if (status === 'missing' || status === 'parse_error') {
      const label = status === 'missing' ? t('about.configMissing') : t('about.configParseError');
      const text = `${path} — ${label}`;
      el.textContent = text;
      el.title = text;
      el.style.color = '#ff7676';
    } else {
      el.textContent = path;
      el.title = path;
      el.style.color = '';
    }
  } else if (app.oscStatusState === 'connected') {
    el.textContent = t('about.configDefaults');
    el.title = t('about.configDefaults');
    el.style.color = '#ffb347';
  } else {
    el.textContent = '—';
    el.removeAttribute('title');
    el.style.color = '';
  }
}

// Reflect the connected renderer's build fingerprint in About. Compare CLI vs
// mpv here to spot a stale liborender (the version skew that makes one host load
// the config while the other silently falls back to defaults).
export function updateAboutRendererVersion() {
  const el = document.getElementById('aboutRendererVersion');
  if (!el) return;
  const version = typeof app.renderVersion === 'string' ? app.renderVersion.trim() : '';
  // The liborender host also reports its C-ABI pair; the CLI reports none.
  const abi = typeof app.renderAbi === 'string' ? app.renderAbi.trim() : '';
  const text = version && abi ? `${version} · ABI ${abi}` : version;
  if (text) {
    el.textContent = text;
    el.title = withExecutablePath(text);
  } else {
    el.textContent = '—';
    el.removeAttribute('title');
  }
}

// Two checkouts of the same commit report the same fingerprint, so the path is
// the only thing telling them apart. It goes on the tooltip rather than the
// line, which is already long.
function withExecutablePath(text) {
  const executable = typeof app.renderExecutable === 'string' ? app.renderExecutable.trim() : '';
  return executable ? `${text}\n${executable}` : text;
}

/**
 * True when the connected renderer is not the binary this Studio would launch.
 *
 * Studio sends its controls to whatever answers on the OSC port, and a renderer
 * left running by another environment answers just as readily as its own. The
 * connection then looks healthy while every control the other build does not
 * implement is silently dropped. `null` while either path is still unknown, so
 * the check cannot cry wolf before the first snapshot lands.
 */
export function rendererIsForeign() {
  const running = typeof app.renderExecutable === 'string' ? app.renderExecutable.trim() : '';
  const expected = typeof app.expectedOrenderPath === 'string' ? app.expectedOrenderPath.trim() : '';
  if (!running || !expected) return null;
  return running !== expected;
}

// Wire render function into the flush callback registry.
flushCallbacks.renderConfigSavedUI = renderConfigSavedUI;
