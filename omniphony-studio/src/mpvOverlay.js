/**
 * mpv overlay bridge — UI side.
 *
 * Only owns the enable toggle, the IPC endpoint (Unix socket path on
 * Linux / macOS, named pipe path on Windows), and the connection
 * lifecycle. The per-frame payload is built and pushed to mpv directly
 * by the Rust OSC thread (`src-tauri/src/osc_listener.rs` →
 * `push_mpv_overlay_frame`) so that the frame stream keeps flowing when
 * the webview is in the background (WebKitGTK throttles JS timers
 * there, including event-driven callbacks).
 *
 * Studio remembers the endpoint and toggle in a file under the
 * config dir; nothing here touches the renderer YAML config.
 */

import { invoke } from '@tauri-apps/api/core';
import { pushLog } from './log.js';

const RECONNECT_PERIOD_MS = 2000;

const overlay = {
  enabled: false,
  socketPath: '',
  connected: false,
  reconnectTimer: null,
  lastError: null,
  ready: false
};

async function loadPrefs() {
  // Persisted to a file in Studio's config dir by Rust — survives restarts
  // even when the webview's localStorage is volatile in dev mode.
  try {
    const p = await invoke('mpv_overlay_load_prefs');
    overlay.enabled = Boolean(p?.enabled);
    overlay.socketPath = String(p?.socket_path ?? '').trim();
  } catch (e) {
    pushLog('warn', `mpv overlay: load prefs failed: ${e}`);
  }
  overlay.ready = true;
}

async function savePrefs() {
  try {
    await invoke('mpv_overlay_save_prefs', {
      prefs: {
        enabled: overlay.enabled,
        socket_path: overlay.socketPath
      }
    });
  } catch (e) {
    pushLog('warn', `mpv overlay: save prefs failed: ${e}`);
  }
}

function buildIpcLine(scriptCmd, ...args) {
  return JSON.stringify({
    command: ['script-message', 'omniphony-overlay', scriptCmd, ...args]
  });
}

async function sendDisable() {
  if (!overlay.connected) return;
  try {
    await invoke('mpv_overlay_send', { line: buildIpcLine('disable') });
  } catch (_) {
    // Disconnect is happening anyway; swallow.
  }
}

function scheduleReconnect() {
  if (!overlay.enabled || overlay.reconnectTimer) return;
  overlay.reconnectTimer = setTimeout(async () => {
    overlay.reconnectTimer = null;
    if (!overlay.enabled) return;
    await tryConnect(/*silent=*/ true);
  }, RECONNECT_PERIOD_MS);
}

async function tryConnect(silent = false) {
  if (!overlay.socketPath) {
    overlay.lastError = 'no socket path configured';
    if (!silent) {
      pushLog('warn', 'mpv overlay: no socket path configured');
    }
    return false;
  }
  try {
    await invoke('mpv_overlay_connect', { path: overlay.socketPath });
    overlay.connected = true;
    overlay.lastError = null;
    if (!silent) {
      pushLog('info', `mpv overlay connected (${overlay.socketPath})`);
    }
    return true;
  } catch (e) {
    overlay.connected = false;
    overlay.lastError = String(e);
    if (!silent) {
      pushLog('warn', `mpv overlay connect failed: ${e}`);
    }
    scheduleReconnect();
    return false;
  }
}

async function tearDown() {
  if (overlay.reconnectTimer) {
    clearTimeout(overlay.reconnectTimer);
    overlay.reconnectTimer = null;
  }
  await sendDisable();
  try {
    await invoke('mpv_overlay_disconnect');
  } catch (_) {}
  overlay.connected = false;
}

// ── public API ──────────────────────────────────────────────────────────

export async function initMpvOverlay() {
  await loadPrefs();
  if (overlay.enabled) {
    await tryConnect(/*silent=*/ true);
  }
}

export async function setMpvOverlayEnabled(enabled) {
  const next = Boolean(enabled);
  if (next === overlay.enabled) return;
  overlay.enabled = next;
  await savePrefs();
  // The overlay is drawn in-process by orender now; show/hide travels as OSC
  // control to the renderer (works regardless of the legacy mpv IPC socket).
  try {
    await invoke('mpv_overlay_set_active', { enabled: next });
  } catch (e) {
    pushLog('warn', `mpv overlay: set active failed: ${e}`);
  }
  // Legacy socket path: harmless when not connected (e.g. an atmos-ranker mpv).
  if (next) {
    await tryConnect();
  } else {
    await tearDown();
  }
}

export async function setMpvOverlaySocketPath(path) {
  const next = String(path || '').trim();
  if (next === overlay.socketPath) return;
  overlay.socketPath = next;
  await savePrefs();
  if (overlay.enabled) {
    await tearDown();
    await tryConnect();
  }
}

export async function pushMpvOverlayTrailPrefs(enabled, ttlMs, mode, teleportThreshold) {
  try {
    const thresholdNum = Number(teleportThreshold);
    await invoke('mpv_overlay_set_trail_prefs', {
      enabled: Boolean(enabled),
      ttlMs: Math.max(500, Math.round(Number(ttlMs) || 7000)),
      mode: mode === 'diffuse' ? 'diffuse' : 'line',
      teleportThreshold: Number.isFinite(thresholdNum)
        ? Math.max(0.05, Math.min(2.0, thresholdNum))
        : 0.5
    });
  } catch (_) {
    // Best-effort; if mpv isn't connected the Rust side just stashes
    // them for the next reconnect.
  }
}

export function getMpvOverlayStatus() {
  return {
    enabled: overlay.enabled,
    socketPath: overlay.socketPath,
    connected: overlay.connected,
    lastError: overlay.lastError
  };
}
