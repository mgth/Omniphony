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

// DOM refs
const statusEl = document.getElementById('status');
const pipeStatusEl = document.getElementById('pipeStatus');
const oscStatusDotEl = document.getElementById('oscStatusDot');
const oscConfigToggleBtnEl = document.getElementById('oscConfigToggleBtn');
const oscConfigFormEl = document.getElementById('oscConfigForm');
const oscHostInputEl = document.getElementById('oscHostInput');
const oscRxPortInputEl = document.getElementById('oscRxPortInput');
const oscListenPortInputEl = document.getElementById('oscListenPortInput');
const oscBridgePathInputEl = document.getElementById('oscBridgePathInput');
const oscBridgeBrowseBtnEl = document.getElementById('oscBridgeBrowseBtn');
const oscMeteringToggleEl = document.getElementById('oscMeteringToggle');
const oscConfigApplyBtnEl = document.getElementById('oscConfigApplyBtn');
const oscServiceBtnEl = document.getElementById('oscServiceBtn');
const oscRestartServiceBtnEl = document.getElementById('oscRestartServiceBtn');
const oscRestartPipewireBtnEl = document.getElementById('oscRestartPipewireBtn');
const oscLaunchRendererBtnEl = document.getElementById('oscLaunchRendererBtn');

// ---------------------------------------------------------------------------
// OSC status rendering
// ---------------------------------------------------------------------------

export function renderOscStatus() {
  if (statusEl) statusEl.textContent = t(`status.${app.oscStatusState}`);
  if (pipeStatusEl) pipeStatusEl.textContent = ` • Pipe: ${app.orenderInputPipe || '—'}`;
  if (oscServiceBtnEl) {
    oscServiceBtnEl.textContent = app.orenderServiceInstalled ? 'Uninstall service' : 'Install service';
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
    oscServiceBtnEl.title = `${app.orenderServiceInstalled ? 'Uninstall' : 'Install'} service${manager}`;
  }
  if (oscRestartServiceBtnEl) {
    const enabled = app.orenderServiceInstalled && !app.orenderServicePending && !app.oscLaunchPending;
    oscRestartServiceBtnEl.disabled = !enabled;
    oscRestartServiceBtnEl.style.opacity = enabled ? '1' : '0.45';
    oscRestartServiceBtnEl.style.cursor = enabled ? 'pointer' : 'default';
    oscRestartServiceBtnEl.title = app.orenderServiceInstalled
      ? 'Restart service'
      : 'Install service first';
  }
  if (oscRestartPipewireBtnEl) {
    const enabled = isLinux && !app.orenderServicePending && !app.oscLaunchPending;
    oscRestartPipewireBtnEl.style.display = isLinux ? '' : 'none';
    oscRestartPipewireBtnEl.disabled = !enabled;
    oscRestartPipewireBtnEl.style.opacity = enabled ? '1' : '0.45';
    oscRestartPipewireBtnEl.style.cursor = enabled ? 'pointer' : 'default';
    oscRestartPipewireBtnEl.title = isLinux
      ? 'Restart PipeWire and WirePlumber'
      : 'Only available on Linux';
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
      ? (running ? 'Stop service' : 'Start service')
      : (running ? 'Stop orender' : 'Launch orender');
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
  if (!oscConfigFormEl) return;
  oscConfigFormEl.classList.add('open');
  if (oscConfigToggleBtnEl) oscConfigToggleBtnEl.textContent = '\u2715';
}

export function closeOscConfigPanel() {
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
    if (oscHostInputEl) oscHostInputEl.value = cfg.host;
    if (oscRxPortInputEl) oscRxPortInputEl.value = String(cfg.osc_rx_port);
    if (oscListenPortInputEl) oscListenPortInputEl.value = String(cfg.osc_port);
    if (oscBridgePathInputEl) oscBridgePathInputEl.value = String(cfg.bridge_path || '');
    if (oscMeteringToggleEl) oscMeteringToggleEl.checked = Boolean(cfg.osc_metering_enabled);
    app.oscConfiguredOrenderPath = String(cfg.orender_path || '').trim();
    app.oscConfigBaselineKey = oscConfigStateKey();
    renderOscConfigApplyButton();
    return refreshOrenderServiceStatus().catch(() => null).then(() => cfg);
  }).catch(() => null);
}

export function readOscConfigForm() {
  return {
    host: oscHostInputEl?.value.trim() || '127.0.0.1',
    osc_rx_port: Math.max(1, Math.min(65535, parseInt(oscRxPortInputEl?.value || '9000', 10))),
    osc_port: Math.max(0, Math.min(65535, parseInt(oscListenPortInputEl?.value || '0', 10))),
    osc_metering_enabled: Boolean(oscMeteringToggleEl?.checked),
    bridge_path: (oscBridgePathInputEl?.value || '').trim() || null,
    orender_path: app.oscConfiguredOrenderPath || null
  };
}

export function oscConfigStateKey() {
  return JSON.stringify(readOscConfigForm());
}

export function renderOscConfigApplyButton() {
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
  renderOscStatus();
  if (next === 'connected') {
    clearOscConfigAutoOpenTimer();
    if (app.oscLaunchPending) {
      app.oscLaunchPending = false;
      closeOscConfigPanel();
    }
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
    bridgePath: config.bridge_path,
    orenderPath: orenderPathOverride || config.orender_path,
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
    bridgePath: config.bridge_path,
    orenderPath: app.oscConfiguredOrenderPath || config.orender_path,
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

if (oscConfigToggleBtnEl && oscConfigFormEl) {
  oscConfigToggleBtnEl.addEventListener('click', () => {
    const isOpen = oscConfigFormEl.classList.toggle('open');
    oscConfigToggleBtnEl.textContent = isOpen ? '\u2715' : '\u2699';
    if (isOpen) {
      loadOscConfigIntoPanel();
    }
  });
}

if (oscConfigApplyBtnEl) {
  oscConfigApplyBtnEl.addEventListener('click', () => {
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

[oscHostInputEl, oscRxPortInputEl, oscListenPortInputEl, oscBridgePathInputEl, oscMeteringToggleEl]
  .filter(Boolean)
  .forEach((el) => {
    el.addEventListener(el === oscMeteringToggleEl ? 'change' : 'input', () => {
      renderOscConfigApplyButton();
    });
  });

if (oscLaunchRendererBtnEl) {
  oscLaunchRendererBtnEl.addEventListener('click', () => {
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

if (oscServiceBtnEl) {
  oscServiceBtnEl.addEventListener('click', () => {
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

if (oscRestartServiceBtnEl) {
  oscRestartServiceBtnEl.addEventListener('click', () => {
    if (app.oscLaunchPending || app.orenderServicePending || !app.orenderServiceInstalled) {
      return;
    }
    restartOrenderServiceFromPanel().catch((e) => {
      pushLog('error', `Failed to restart orender service: ${normalizeLogError(e)}`);
    });
  });
}

if (oscRestartPipewireBtnEl) {
  oscRestartPipewireBtnEl.addEventListener('click', () => {
    if (!isLinux || app.oscLaunchPending || app.orenderServicePending) {
      return;
    }
    restartPipewireFromPanel().catch((e) => {
      pushLog('error', `Failed to restart PipeWire: ${normalizeLogError(e)}`);
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
        }
      })
      .catch((e) => {
        pushLog('error', `Failed to select bridge: ${normalizeLogError(e)}`);
      });
  });
}

if (oscMeteringToggleEl) {
  oscMeteringToggleEl.addEventListener('change', () => {
    const enabled = Boolean(oscMeteringToggleEl.checked);
    app.oscMeteringEnabled = enabled;
    pushLog('info', t(enabled ? 'log.oscMeteringEnabled' : 'log.oscMeteringDisabled'));
    invoke('control_osc_metering', { enable: enabled ? 1 : 0 }).catch((e) => {
      console.error('[osc metering]', e);
      pushLog('error', tf('log.oscMeteringFailed', { error: normalizeLogError(e) }));
    });
  });
}
