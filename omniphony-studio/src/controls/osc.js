/**
 * OSC config panel and service management.
 *
 * Extracted from app.js:
 *   - lines 1156-1322: renderOscStatus, refreshOrenderServiceStatus,
 *     openOscConfigPanel, closeOscConfigPanel, clearOscConfigAutoOpenTimer,
 *     scheduleOscConfigAutoOpen, loadOscConfigIntoPanel, readOscConfigForm,
 *     oscConfigStateKey, renderOscConfigApplyButton, setOscStatus
 *   - lines 8029-8281: OSC config panel event listeners and service management
 *     functions (launchOrenderFromPanel, installOrenderServiceFromPanel, etc.)
 */

import { app, dirty, isLinux } from '../state.js';
import { t, tf } from '../i18n.js';
import { scheduleUIFlush } from '../flush.js';
import { pushLog, normalizeLogError, normalizeLogLevel, logState } from '../log.js';
import { invoke } from '@tauri-apps/api/core';
import { syncRuntimeConnectionLock } from '../runtime-connection.js';
import { collapseRuntimeSections } from '../modals.js';
import { inObjectsPanel, inOscPanel } from '../ui/panel-roots.js';

// DOM refs
function getStatusEl() { return inOscPanel('status'); }
function getPipeStatusEl() { return inOscPanel('pipeStatus'); }
function getOscStatusDotEl() { return inOscPanel('oscStatusDot'); }
function getOscConfigToggleBtnEl() { return inOscPanel('oscConfigToggleBtn'); }
function getOscConfigFormEl() { return inOscPanel('oscConfigForm'); }
function getOscHostInputEl() { return inOscPanel('oscHostInput'); }
function getOscRxPortInputEl() { return inOscPanel('oscRxPortInput'); }
function getOscListenPortInputEl() { return inOscPanel('oscListenPortInput'); }
function getOscMeteringToggleEl() { return inObjectsPanel('oscMeteringToggle'); }
function getOscConfigApplyBtnEl() { return inOscPanel('oscConfigApplyBtn'); }
function getOscServiceBtnEl() { return inOscPanel('oscServiceBtn'); }
function getOscRestartServiceBtnEl() { return inOscPanel('oscRestartServiceBtn'); }
function getOscRestartPipewireBtnEl() { return inOscPanel('oscRestartPipewireBtn'); }
function getOscLaunchRendererBtnEl() { return inOscPanel('oscLaunchRendererBtn'); }

// ---------------------------------------------------------------------------
// OSC status rendering
// ---------------------------------------------------------------------------

export function renderOscStatus() {
  const statusEl = getStatusEl();
  const pipeStatusEl = getPipeStatusEl();
  const oscStatusDotEl = getOscStatusDotEl();
  const oscServiceBtnEl = getOscServiceBtnEl();
  const oscRestartServiceBtnEl = getOscRestartServiceBtnEl();
  const oscRestartPipewireBtnEl = getOscRestartPipewireBtnEl();
  const oscLaunchRendererBtnEl = getOscLaunchRendererBtnEl();
  syncRuntimeConnectionLock();
  if (statusEl) statusEl.textContent = t(`status.${app.oscStatusState}`);
  if (pipeStatusEl && document.activeElement !== pipeStatusEl) {
    pipeStatusEl.value = app.orenderInputPipe || '';
  }
  if (oscServiceBtnEl) {
    oscServiceBtnEl.textContent = app.orenderServiceInstalled ? t('osc.service.uninstall') : t('osc.service.install');
    oscServiceBtnEl.style.background = app.orenderServiceInstalled
      ? 'rgba(255,96,96,0.18)'
      : 'rgba(255,255,255,0.08)';
    oscServiceBtnEl.style.borderColor = app.orenderServiceInstalled
      ? 'rgba(255,96,96,0.38)'
      : 'rgba(255,255,255,0.18)';
    oscServiceBtnEl.style.color = '#d9ecff';
    oscServiceBtnEl.disabled = app.orenderServicePending || app.oscLaunchPending;
    oscServiceBtnEl.style.opacity = (app.orenderServicePending || app.oscLaunchPending) ? '0.6' : '1';
    oscServiceBtnEl.style.cursor = (app.orenderServicePending || app.oscLaunchPending) ? 'default' : 'pointer';
    const manager = app.orenderServiceManager ? ` (${app.orenderServiceManager})` : '';
    oscServiceBtnEl.title = `${app.orenderServiceInstalled ? t('osc.service.uninstallShort') : t('osc.service.installShort')} ${t('osc.service.serviceNoun')}${manager}`;
  }
  if (oscRestartServiceBtnEl) {
    const enabled = app.orenderServiceInstalled && !app.orenderServicePending && !app.oscLaunchPending;
    oscRestartServiceBtnEl.disabled = !enabled;
    oscRestartServiceBtnEl.style.opacity = enabled ? '1' : '0.45';
    oscRestartServiceBtnEl.style.cursor = enabled ? 'pointer' : 'default';
    oscRestartServiceBtnEl.title = app.orenderServiceInstalled
      ? t('osc.service.restart')
      : t('osc.service.installFirst');
  }
  if (oscRestartPipewireBtnEl) {
    const enabled = isLinux && !app.orenderServicePending && !app.oscLaunchPending;
    oscRestartPipewireBtnEl.style.display = isLinux ? '' : 'none';
    oscRestartPipewireBtnEl.disabled = !enabled;
    oscRestartPipewireBtnEl.style.opacity = enabled ? '1' : '0.45';
    oscRestartPipewireBtnEl.style.cursor = enabled ? 'pointer' : 'default';
    oscRestartPipewireBtnEl.title = isLinux
      ? t('osc.pipewire.restartTitle')
      : t('osc.pipewire.linuxOnly');
  }
  if (oscStatusDotEl) {
    const colors = {
      initializing: '#89a3ff',
      connected: '#52e2a2',
      reconnecting: '#ffb347',
      error: '#ff5d5d'
    };
    oscStatusDotEl.style.background = colors[app.oscStatusState] || '#7f8a99';
  }
  if (oscLaunchRendererBtnEl) {
    const running = app.orenderServiceInstalled ? app.orenderServiceRunning : app.oscStatusState === 'connected';
    oscLaunchRendererBtnEl.textContent = app.orenderServiceInstalled
      ? (running ? t('osc.service.stop') : t('osc.service.start'))
      : (running ? t('osc.orender.stop') : t('osc.orender.launch'));
    oscLaunchRendererBtnEl.style.background = running
      ? 'rgba(255,96,96,0.18)'
      : 'rgba(88,160,255,0.18)';
    oscLaunchRendererBtnEl.style.borderColor = running
      ? 'rgba(255,96,96,0.38)'
      : 'rgba(88,160,255,0.38)';
    oscLaunchRendererBtnEl.style.color = running ? '#ffe2e2' : '#d9ecff';
    oscLaunchRendererBtnEl.disabled = app.oscLaunchPending || app.orenderServicePending;
    oscLaunchRendererBtnEl.style.opacity = (app.oscLaunchPending || app.orenderServicePending) ? '0.6' : '1';
    oscLaunchRendererBtnEl.style.cursor = (app.oscLaunchPending || app.orenderServicePending) ? 'default' : 'pointer';
  }
  renderOscConfigApplyButton();
}

export function refreshOrenderServiceStatus() {
  return invoke('get_orender_service_status')
    .then((status) => {
      app.orenderServiceInstalled = Boolean(status?.installed);
      app.orenderServiceRunning = Boolean(status?.running);
      app.orenderServiceManager = typeof status?.manager === 'string' ? status.manager : null;
      renderOscStatus();
      return status;
    });
}

export function openOscConfigPanel() {
  const oscConfigFormEl = getOscConfigFormEl();
  const oscConfigToggleBtnEl = getOscConfigToggleBtnEl();
  if (!oscConfigFormEl) return;
  oscConfigFormEl.classList.add('open');
  if (oscConfigToggleBtnEl) oscConfigToggleBtnEl.textContent = '\u2715';
}

export function closeOscConfigPanel() {
  const oscConfigFormEl = getOscConfigFormEl();
  const oscConfigToggleBtnEl = getOscConfigToggleBtnEl();
  if (!oscConfigFormEl) return;
  oscConfigFormEl.classList.remove('open');
  if (oscConfigToggleBtnEl) oscConfigToggleBtnEl.textContent = '\u2699';
}

export function clearOscConfigAutoOpenTimer() {
  if (app.oscConfigAutoOpenTimer !== null) {
    clearTimeout(app.oscConfigAutoOpenTimer);
    app.oscConfigAutoOpenTimer = null;
  }
}

export function scheduleOscConfigAutoOpen() {
  clearOscConfigAutoOpenTimer();
  app.oscConfigAutoOpenTimer = setTimeout(() => {
    app.oscConfigAutoOpenTimer = null;
    if (app.oscStatusState !== 'connected') {
      openOscConfigPanel();
    }
  }, 3000);
}

export function loadOscConfigIntoPanel() {
  return invoke('get_osc_config').then((cfg) => {
    const oscHostInputEl = getOscHostInputEl();
    const oscRxPortInputEl = getOscRxPortInputEl();
    const oscListenPortInputEl = getOscListenPortInputEl();
    const oscMeteringToggleEl = getOscMeteringToggleEl();
    if (oscHostInputEl) oscHostInputEl.value = cfg.host;
    if (oscRxPortInputEl) oscRxPortInputEl.value = String(cfg.osc_rx_port);
    if (oscListenPortInputEl) oscListenPortInputEl.value = String(cfg.osc_port);
    if (oscMeteringToggleEl) oscMeteringToggleEl.checked = Boolean(cfg.osc_metering_enabled);
    app.oscConfigBaselineKey = oscConfigStateKey();
    dirty.audioFormat = true;
    scheduleUIFlush();
    renderOscConfigApplyButton();
    return refreshOrenderServiceStatus().catch(() => null).then(() => cfg);
  }).catch(() => null);
}

export function readOscConfigForm() {
  const oscHostInputEl = getOscHostInputEl();
  const oscRxPortInputEl = getOscRxPortInputEl();
  const oscListenPortInputEl = getOscListenPortInputEl();
  const oscMeteringToggleEl = getOscMeteringToggleEl();
  return {
    host: oscHostInputEl?.value.trim() || '127.0.0.1',
    osc_rx_port: Math.max(1, Math.min(65535, parseInt(oscRxPortInputEl?.value || '9000', 10))),
    osc_port: Math.max(0, Math.min(65535, parseInt(oscListenPortInputEl?.value || '0', 10))),
    osc_metering_enabled: Boolean(oscMeteringToggleEl?.checked)
  };
}

export function oscConfigStateKey() {
  return JSON.stringify(readOscConfigForm());
}

export function renderOscConfigApplyButton() {
  const oscConfigApplyBtnEl = getOscConfigApplyBtnEl();
  if (!oscConfigApplyBtnEl) return;
  const isDirty = oscConfigStateKey() !== app.oscConfigBaselineKey;
  const enabled = isDirty && !app.oscLaunchPending && !app.orenderServicePending;
  oscConfigApplyBtnEl.disabled = !enabled;
  oscConfigApplyBtnEl.style.opacity = enabled ? '1' : '0.45';
  oscConfigApplyBtnEl.style.cursor = enabled ? 'pointer' : 'default';
}

export function setOscStatus(next) {
  const changed = app.oscStatusState !== next;
  const previous = app.oscStatusState;
  app.oscStatusState = next;
  if (next !== 'connected') {
    app.oscSnapshotReady = false;
    collapseRuntimeSections();
  }
  renderOscStatus();
  if (next === 'connected') {
    clearOscConfigAutoOpenTimer();
    if (app.oscLaunchPending) {
      app.oscLaunchPending = false;
      closeOscConfigPanel();
    }
  } else if (next === 'initializing') {
    clearOscConfigAutoOpenTimer();
    openOscConfigPanel();
  } else if (next === 'reconnecting') {
    if (previous === 'initializing' || app.oscLaunchPending) {
      scheduleOscConfigAutoOpen();
    }
  } else if (next === 'error') {
    clearOscConfigAutoOpenTimer();
    openOscConfigPanel();
    app.oscLaunchPending = false;
  }
  if (changed) {
    pushLog('info', tf('log.oscStatus', { status: t(`status.${next}`) }));
  }
}

// ---------------------------------------------------------------------------
// Service management functions
// ---------------------------------------------------------------------------

export function launchOrenderFromPanel(orenderPathOverride = null) {
  const config = readOscConfigForm();
  const payload = {
    host: config.host,
    oscRxPort: config.osc_rx_port,
    oscPort: config.osc_port,
    oscMeteringEnabled: config.osc_metering_enabled,
    orenderPath: orenderPathOverride || app.oscConfiguredOrenderPath || null,
    logLevel: normalizeLogLevel(logState.backendLogLevel)
  };
  app.oscLaunchPending = true;
  return invoke('launch_orender', payload)
    .then((result) => {
      app.oscConfiguredOrenderPath = String(payload.orenderPath || app.oscConfiguredOrenderPath || '').trim();
      if (result?.command) {
        pushLog('info', `orender launched: ${result.command}`);
      } else {
        pushLog('info', 'orender launched.');
      }
    })
    .catch((e) => {
      app.oscLaunchPending = false;
      const message = normalizeLogError(e);
      if (message.includes('orender binary not found')) {
        openOscConfigPanel();
        return invoke('pick_orender_path')
          .then((selectedPath) => {
            const trimmed = String(selectedPath || '').trim();
            if (!trimmed) {
              return;
            }
            app.oscConfiguredOrenderPath = trimmed;
            return launchOrenderFromPanel(trimmed);
          });
      }
      throw e;
    });
}

export function installOrenderServiceFromPanel() {
  const config = readOscConfigForm();
  const payload = {
    host: config.host,
    oscRxPort: config.osc_rx_port,
    oscPort: config.osc_port,
    oscMeteringEnabled: config.osc_metering_enabled,
    orenderPath: app.oscConfiguredOrenderPath || null,
    logLevel: normalizeLogLevel(logState.backendLogLevel)
  };
  app.orenderServicePending = true;
  renderOscStatus();
  return invoke('install_orender_service', payload)
    .then((result) => {
      if (result?.command) {
        pushLog('info', `orender service installed: ${result.command}`);
      } else {
        pushLog('info', 'orender service installed.');
      }
      return refreshOrenderServiceStatus();
    })
    .finally(() => {
      app.orenderServicePending = false;
      renderOscStatus();
    });
}

export function uninstallOrenderServiceFromPanel() {
  app.orenderServicePending = true;
  renderOscStatus();
  return invoke('uninstall_orender_service')
    .then(() => {
      pushLog('info', 'orender service uninstalled.');
      return refreshOrenderServiceStatus();
    })
    .finally(() => {
      app.orenderServicePending = false;
      renderOscStatus();
    });
}

export function restartOrenderServiceFromPanel() {
  app.orenderServicePending = true;
  renderOscStatus();
  return invoke('restart_orender_service')
    .then(() => {
      pushLog('info', 'orender service restart requested.');
      return refreshOrenderServiceStatus();
    })
    .finally(() => {
      app.orenderServicePending = false;
      renderOscStatus();
    });
}

export function restartPipewireFromPanel() {
  app.orenderServicePending = true;
  renderOscStatus();
  return invoke('restart_pipewire_services')
    .then(() => {
      pushLog('info', 'PipeWire restart requested.');
      return refreshOrenderServiceStatus().catch(() => {});
    })
    .finally(() => {
      app.orenderServicePending = false;
      renderOscStatus();
    });
}

// ---------------------------------------------------------------------------
// Event listener wiring (runs at module load time)
// ---------------------------------------------------------------------------

const initialOscConfigToggleBtnEl = getOscConfigToggleBtnEl();
const initialOscConfigFormEl = getOscConfigFormEl();
if (initialOscConfigToggleBtnEl && initialOscConfigFormEl) {
  initialOscConfigToggleBtnEl.addEventListener('click', () => {
    const oscConfigFormEl = getOscConfigFormEl();
    const oscConfigToggleBtnEl = getOscConfigToggleBtnEl();
    if (!oscConfigFormEl || !oscConfigToggleBtnEl) return;
    const isOpen = oscConfigFormEl.classList.toggle('open');
    oscConfigToggleBtnEl.textContent = isOpen ? '\u2715' : '\u2699';
    if (isOpen) {
      loadOscConfigIntoPanel();
    }
  });
}

const initialOscConfigApplyBtnEl = getOscConfigApplyBtnEl();
if (initialOscConfigApplyBtnEl) {
  initialOscConfigApplyBtnEl.addEventListener('click', () => {
    const oscConfigApplyBtnEl = getOscConfigApplyBtnEl();
    if (oscConfigApplyBtnEl.disabled) return;
    const config = readOscConfigForm();
    invoke('save_osc_config', { config })
      .then(() => {
        app.oscMeteringEnabled = config.osc_metering_enabled;
        app.oscConfigBaselineKey = oscConfigStateKey();
        renderOscConfigApplyButton();
        pushLog('info', t('log.oscConfigSaved'));
        setOscStatus('reconnecting');
        closeOscConfigPanel();
      })
      .catch((e) => {
        console.error('[osc config]', e);
        pushLog('error', tf('log.oscConfigFailed', { error: normalizeLogError(e) }));
      });
  });
}

[getOscHostInputEl(), getOscRxPortInputEl(), getOscListenPortInputEl(), getOscMeteringToggleEl()]
  .filter(Boolean)
  .forEach((el) => {
    el.addEventListener(el === getOscMeteringToggleEl() ? 'change' : 'input', () => {
      renderOscConfigApplyButton();
    });
  });

const initialOscLaunchRendererBtnEl = getOscLaunchRendererBtnEl();
if (initialOscLaunchRendererBtnEl) {
  initialOscLaunchRendererBtnEl.addEventListener('click', () => {
    if (app.oscLaunchPending || app.orenderServicePending) {
      return;
    }
    if (app.orenderServiceInstalled) {
      const command = app.orenderServiceRunning ? 'stop_orender_service' : 'start_orender_service';
      const label = app.orenderServiceRunning ? 'stop orender service' : 'start orender service';
      const success = app.orenderServiceRunning ? 'orender service stop requested.' : 'orender service start requested.';
      app.orenderServicePending = true;
      renderOscStatus();
      invoke(command)
        .then(() => {
          pushLog('info', success);
          return refreshOrenderServiceStatus();
        })
        .catch((e) => {
          pushLog('error', `Failed to ${label}: ${normalizeLogError(e)}`);
        })
        .finally(() => {
          app.orenderServicePending = false;
          renderOscStatus();
        });
      return;
    }
    if (app.oscStatusState === 'connected') {
      invoke('stop_orender')
        .then(() => {
          pushLog('info', 'orender stop requested.');
        })
        .catch((e) => {
          pushLog('error', `Failed to stop orender: ${normalizeLogError(e)}`);
        });
      return;
    }
    launchOrenderFromPanel()
      .catch((e) => {
        pushLog('error', `Failed to launch orender: ${normalizeLogError(e)}`);
      });
  });
}

const initialOscServiceBtnEl = getOscServiceBtnEl();
if (initialOscServiceBtnEl) {
  initialOscServiceBtnEl.addEventListener('click', () => {
    if (app.oscLaunchPending || app.orenderServicePending) {
      return;
    }
    const task = app.orenderServiceInstalled
      ? uninstallOrenderServiceFromPanel()
      : installOrenderServiceFromPanel();
    task.catch((e) => {
      const label = app.orenderServiceInstalled ? 'uninstall orender service' : 'install orender service';
      pushLog('error', `Failed to ${label}: ${normalizeLogError(e)}`);
    });
  });
}

const initialOscRestartServiceBtnEl = getOscRestartServiceBtnEl();
if (initialOscRestartServiceBtnEl) {
  initialOscRestartServiceBtnEl.addEventListener('click', () => {
    if (app.oscLaunchPending || app.orenderServicePending || !app.orenderServiceInstalled) {
      return;
    }
    restartOrenderServiceFromPanel().catch((e) => {
      pushLog('error', `Failed to restart orender service: ${normalizeLogError(e)}`);
    });
  });
}

const initialOscRestartPipewireBtnEl = getOscRestartPipewireBtnEl();
if (initialOscRestartPipewireBtnEl) {
  initialOscRestartPipewireBtnEl.addEventListener('click', () => {
    if (!isLinux || app.oscLaunchPending || app.orenderServicePending) {
      return;
    }
    restartPipewireFromPanel().catch((e) => {
      pushLog('error', `Failed to restart PipeWire: ${normalizeLogError(e)}`);
    });
  });
}

const initialOscMeteringToggleEl = getOscMeteringToggleEl();
if (initialOscMeteringToggleEl) {
  initialOscMeteringToggleEl.addEventListener('change', () => {
    const oscMeteringToggleEl = getOscMeteringToggleEl();
    if (!oscMeteringToggleEl) return;
    const enabled = Boolean(oscMeteringToggleEl.checked);
    app.oscMeteringEnabled = enabled;
    pushLog('info', t(enabled ? 'log.oscMeteringEnabled' : 'log.oscMeteringDisabled'));
    invoke('control_osc_metering', { enable: enabled ? 1 : 0 }).catch((e) => {
      console.error('[osc metering]', e);
      pushLog('error', tf('log.oscMeteringFailed', { error: normalizeLogError(e) }));
    });
  });
}
