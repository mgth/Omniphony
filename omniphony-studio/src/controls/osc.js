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

import { app, dirty, isLinux, producerHost, producerVariant, isEmbeddedProducer, sourceNames } from '../state.js';
import { t, tf } from '../i18n.js';
import { scheduleUIFlush } from '../flush.js';
import { pushLog, normalizeLogError, normalizeLogLevel, logState } from '../log.js';
import { invoke } from '@tauri-apps/api/core';
import { syncRuntimeConnectionLock } from '../runtime-connection.js';
import { inObjectsPanel, inOscPanel } from '../ui/panel-roots.js';
import { updateConfigSavedUI } from './config.js';
import { applyProducerCapabilityVisibility } from '../init.js';

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
function getAutoStartRendererToggleEl() { return inOscPanel('autoStartRendererToggle'); }
function getKeepRendererAliveToggleEl() { return inOscPanel('keepRendererAliveToggle'); }
function getOscMeteringRateSelectEl() { return inObjectsPanel('oscMeteringRateSelect'); }
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
  // The connect/launch/service buttons are hidden only while connected to an
  // embedded (mpv) host; on disconnect they must come back. Capability changes
  // arrive via the handshake, but a disconnect arrives only as a status change,
  // so re-evaluate the cap-embedded gate here too.
  applyProducerCapabilityVisibility();
  if (statusEl) {
    let statusText = t(`status.${app.oscStatusState}`);
    // Label the connected renderer flavour so the user can tell what they are
    // talking to: "mpv" for the embedded player, "service" for the OS-managed
    // instance, or "cli" for a standalone/Studio-launched one.
    if (app.oscStatusState === 'connected' && app.producerCapabilities) {
      let flavour;
      if (isEmbeddedProducer()) {
        flavour = producerHost() || producerVariant(); // "mpv"
      } else if (app.orenderServiceRunning) {
        flavour = 'service';
      } else {
        flavour = producerHost() || producerVariant(); // "cli"
      }
      if (flavour) statusText += ` · ${flavour}`;
    }
    statusEl.textContent = statusText;
  }
  const oscConnectingHintEl = inOscPanel('oscConnectingHint');
  const bridgeError = typeof app.renderBridgeError === 'string' ? app.renderBridgeError.trim() : '';
  if (oscConnectingHintEl) {
    // While no renderer is connected, tell the user how to bring one up — but
    // not when we already have a more specific bridge-error banner to show.
    const connecting = app.oscStatusState === 'reconnecting'
      || app.oscStatusState === 'initializing';
    oscConnectingHintEl.style.display = (connecting && !bridgeError) ? '' : 'none';
  }
  // Decoder bridge missing → the renderer came up degraded (no spatial). Show a
  // prominent red banner with the underlying error (from the degraded reporter's
  // /state/render/bridge_error). Mirrors the "no orender server" hint styling.
  const bridgeErrorBannerEl = inOscPanel('bridgeErrorBanner');
  if (bridgeErrorBannerEl) {
    bridgeErrorBannerEl.style.display = bridgeError ? '' : 'none';
    const detailEl = inOscPanel('bridgeErrorDetail');
    if (detailEl) detailEl.textContent = bridgeError;
  }
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

// Clear the "launching" state + its safety timer, so the connection buttons
// (launch / service / config) become usable again.
function clearOscLaunchPending() {
  if (app.oscLaunchPendingTimer !== null) {
    clearTimeout(app.oscLaunchPendingTimer);
    app.oscLaunchPendingTimer = null;
  }
  app.oscLaunchPending = false;
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
    const autoStartToggleEl = getAutoStartRendererToggleEl();
    const keepAliveToggleEl = getKeepRendererAliveToggleEl();
    if (oscHostInputEl) oscHostInputEl.value = cfg.host;
    if (oscRxPortInputEl) oscRxPortInputEl.value = String(cfg.osc_rx_port);
    if (oscListenPortInputEl) oscListenPortInputEl.value = String(cfg.osc_port);
    if (oscMeteringToggleEl) oscMeteringToggleEl.checked = Boolean(cfg.osc_metering_enabled);
    if (autoStartToggleEl) autoStartToggleEl.checked = Boolean(cfg.auto_start_renderer);
    if (keepAliveToggleEl) keepAliveToggleEl.checked = Boolean(cfg.keep_renderer_alive_on_quit);
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
  const autoStartToggleEl = getAutoStartRendererToggleEl();
  const keepAliveToggleEl = getKeepRendererAliveToggleEl();
  return {
    host: oscHostInputEl?.value.trim() || '127.0.0.1',
    osc_rx_port: Math.max(1, Math.min(65535, parseInt(oscRxPortInputEl?.value || '9000', 10))),
    osc_port: Math.max(0, Math.min(65535, parseInt(oscListenPortInputEl?.value || '0', 10))),
    osc_metering_enabled: Boolean(oscMeteringToggleEl?.checked),
    auto_start_renderer: autoStartToggleEl ? Boolean(autoStartToggleEl.checked) : true,
    keep_renderer_alive_on_quit: Boolean(keepAliveToggleEl?.checked)
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
    // Re-arm the Audio Input auto-open for the next connection (a fresh
    // instance with a broken bridge should surface the panel once again).
    app.lastAutoOpenedInputError = null;
  }
  // Leaving 'connected' (a disconnect) ends any launch: clear the pending flag +
  // safety timer before re-rendering so the connection buttons come back.
  const disconnected = previous === 'connected' && next !== 'connected';
  if (disconnected) {
    clearOscLaunchPending();
    // Drop cached object identities on a drop/producer swap so a renderer that
    // takes over the port (CLI⇄mpv) can't inherit stale names: the re-handshake
    // re-sends every object's name via the renderer's forced full frame. Without
    // this, an id the new stream reuses with an empty name would keep showing the
    // previous producer's label.
    sourceNames.clear();
  }
  updateConfigSavedUI();
  renderOscStatus();
  if (next === 'connected') {
    clearOscConfigAutoOpenTimer();
    if (app.oscLaunchPending) {
      clearOscLaunchPending();
      closeOscConfigPanel();
    }
    // Embedded (mpv) host owns the renderer lifecycle, so the OSC host/port
    // form has nothing actionable (its connect/launch/service buttons are
    // hidden). Close it once the link is up so we don't leave a dead config
    // panel hanging open.
    if (isEmbeddedProducer()) {
      closeOscConfigPanel();
    }
  } else if (next === 'initializing') {
    clearOscConfigAutoOpenTimer();
    openOscConfigPanel();
  } else if (next === 'reconnecting') {
    // Auto-surface the config panel again on a drop. On the embedded (mpv)
    // host it was closed while connected, so reopening it is what brings the
    // (now un-hidden) connect buttons back without the user hunting the gear.
    if (previous === 'initializing' || app.oscLaunchPending || disconnected) {
      scheduleOscConfigAutoOpen();
    }
  } else if (next === 'error') {
    clearOscConfigAutoOpenTimer();
    openOscConfigPanel();
    clearOscLaunchPending();
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
  // Safety net: if orender never reaches 'connected', re-enable the buttons.
  if (app.oscLaunchPendingTimer !== null) {
    clearTimeout(app.oscLaunchPendingTimer);
  }
  app.oscLaunchPendingTimer = window.setTimeout(() => {
    app.oscLaunchPendingTimer = null;
    if (app.oscLaunchPending) {
      app.oscLaunchPending = false;
      renderOscStatus();
    }
  }, 12000);
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
      clearOscLaunchPending();
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
      // Installing the service disables auto-start backend-side; reload the
      // panel so the toggle reflects it.
      loadOscConfigIntoPanel();
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

[getOscHostInputEl(), getOscRxPortInputEl(), getOscListenPortInputEl(), getOscMeteringToggleEl(),
  getAutoStartRendererToggleEl(), getKeepRendererAliveToggleEl()]
  .filter(Boolean)
  .forEach((el) => {
    el.addEventListener(el.type === 'checkbox' ? 'change' : 'input', () => {
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

// Audio meter publish rate. Controls only the AudioMeter cadence (peak/RMS
// level updates); the diag publication rate is independent and lives in
// `controls/diag-plot.js`.
const METER_RATE_STORAGE_KEY = 'audioMetering.rateHz.v1';
const METER_RATE_LEGACY_STORAGE_KEY = 'diagPlot.meterRateHz.v1';
const METER_RATE_OPTIONS_HZ = [10, 20, 50, 100, 200];
const METER_RATE_DEFAULT_HZ = 50;

function loadMeterRateHzFromStorage() {
  try {
    let raw = localStorage.getItem(METER_RATE_STORAGE_KEY);
    if (raw === null) raw = localStorage.getItem(METER_RATE_LEGACY_STORAGE_KEY);
    const n = raw === null ? NaN : Number.parseInt(raw, 10);
    if (Number.isFinite(n) && METER_RATE_OPTIONS_HZ.includes(n)) return n;
  } catch (_) { /* ignore */ }
  return METER_RATE_DEFAULT_HZ;
}

function applyMeterRateToSelect(value) {
  const sel = getOscMeteringRateSelectEl();
  if (!sel) return;
  if (sel.value !== String(value)) sel.value = String(value);
}

const initialOscMeteringRateSelectEl = getOscMeteringRateSelectEl();
if (initialOscMeteringRateSelectEl) {
  const initialRate = loadMeterRateHzFromStorage();
  // Pre-connect default for the UI only. The renderer is the source of truth:
  // its persisted value arrives via /state/monitoring (syncMeterRateFromRenderer)
  // and overrides this. We no longer push localStorage to the renderer on boot.
  applyMeterRateToSelect(initialRate);
  initialOscMeteringRateSelectEl.addEventListener('change', () => {
    const sel = getOscMeteringRateSelectEl();
    if (!sel) return;
    const v = Number.parseInt(sel.value, 10);
    if (!Number.isFinite(v) || !METER_RATE_OPTIONS_HZ.includes(v)) return;
    try { localStorage.setItem(METER_RATE_STORAGE_KEY, String(v)); } catch (_) { /* ignore */ }
    invoke('control_metering_rate_hz', { value: v }).catch((e) => {
      console.error('[meter rate]', e);
    });
  });
}

// Sync the meter-rate select from the renderer's authoritative value
// (/omniphony/state/monitoring). Mirrors it into localStorage so the UI memory
// tracks the renderer rather than overriding it.
export function syncMeterRateFromRenderer(hz) {
  const rounded = Math.round(Number(hz));
  if (!Number.isFinite(rounded) || !METER_RATE_OPTIONS_HZ.includes(rounded)) return;
  applyMeterRateToSelect(rounded);
  try { localStorage.setItem(METER_RATE_STORAGE_KEY, String(rounded)); } catch (_) { /* ignore */ }
}
