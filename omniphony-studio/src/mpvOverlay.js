/**
 * mpv overlay bridge — UI side.
 *
 * The overlay is generated in-process by orender (liborender.so) and pulled by
 * a small mpv Lua shim; Studio no longer talks to mpv over a JSON IPC socket.
 * This module only owns the enable toggle, which travels to the renderer as OSC
 * control. orender owns and persists the overlay display prefs (enable / labels
 * / trails), so there is nothing to store on the Studio side.
 */

import { invoke } from '@tauri-apps/api/core';
import { pushLog } from './log.js';

const overlay = {
  enabled: true
};

// ── public API ──────────────────────────────────────────────────────────

export async function initMpvOverlay() {
  // No-op: orender owns + persists the overlay state and loads it at its own
  // startup. The toggle below only drives live changes.
}

export async function setMpvOverlayEnabled(enabled) {
  const next = Boolean(enabled);
  overlay.enabled = next;
  try {
    await invoke('mpv_overlay_set_active', { enabled: next });
  } catch (e) {
    pushLog('warn', `mpv overlay: set active failed: ${e}`);
  }
}

export async function pushMpvOverlayTrailPrefs(enabled, ttlMs, mode, teleportThreshold) {
  const thresholdNum = Number(teleportThreshold);
  try {
    await invoke('mpv_overlay_set_trail_prefs', {
      enabled: Boolean(enabled),
      ttlMs: Math.max(500, Math.round(Number(ttlMs) || 7000)),
      mode: mode === 'diffuse' ? 'diffuse' : 'line',
      teleportThreshold: Number.isFinite(thresholdNum)
        ? Math.max(0.05, Math.min(2.0, thresholdNum))
        : 0.5
    });
  } catch (_) {
    // Best-effort; the renderer owns + persists the trail prefs.
  }
}

export function getMpvOverlayStatus() {
  return { enabled: overlay.enabled };
}
