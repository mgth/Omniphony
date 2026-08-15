/**
 * mpv overlay bridge — UI side.
 *
 * The overlay is generated in-process by orender (liborender.so) and pulled by
 * a small mpv Lua shim; Studio no longer talks to mpv over a JSON IPC socket.
 * This module only owns the enable toggle, which travels to the renderer as OSC
 * control. orender owns and persists the overlay display prefs (enable / labels
 * / trails), so there is nothing to store on the Studio side.
 *
 * The overlay is a process-global singleton in the engine with TWO writers:
 * Studio over OSC, and mpv through the FFI toggles bound to keybinds. Studio
 * therefore must not treat its own last write as the truth — `applyOverlayState`
 * adopts what the engine republishes (`/omniphony/state/overlay`), so pressing
 * the mpv keybind moves the Studio switch instead of silently diverging from it.
 */

import { invoke } from '@tauri-apps/api/core';
import { app } from './state.js';
import { pushLog } from './log.js';
import { colormapIndex } from './scene/object-energy-shared.js';

// Last state the ENGINE reported. Written only by `applyOverlayState`; a local
// toggle updates it optimistically and is corrected by the next broadcast.
const overlay = {
  enabled: true
};

/** Callbacks invoked after the engine's state lands, so the UI can re-render. */
const stateListeners = new Set();

/** Register a UI refresher for engine-driven overlay changes. */
export function onOverlayState(fn) {
  stateListeners.add(fn);
  return () => stateListeners.delete(fn);
}

/**
 * Adopt the overlay state published by the engine.
 *
 * This is the authoritative direction: whatever flipped the value — a Studio
 * switch, an mpv keybind, or the prefs restored at orender startup — Studio
 * ends up showing what is actually drawn.
 */
export function applyOverlayState(payload) {
  if (!payload || typeof payload !== 'object') return;
  if (typeof payload.enabled === 'boolean') overlay.enabled = payload.enabled;
  if (typeof payload.objectsVisible === 'boolean') app.objectsVisible = payload.objectsVisible;
  if (typeof payload.labelsEnabled === 'boolean') app.objectLabelsEnabled = payload.labelsEnabled;
  if (typeof payload.heatmapEnabled === 'boolean') {
    app.objectEnergyHeatmapEnabled = payload.heatmapEnabled;
  }
  if (Number.isFinite(Number(payload.heatmapBands))) {
    app.objectEnergyHeatmapBandCount = Math.max(1, Math.min(12, Math.round(Number(payload.heatmapBands))));
  }
  if (typeof payload.trailsEnabled === 'boolean') app.trailsEnabled = payload.trailsEnabled;
  if (Number.isFinite(Number(payload.trailTtlMs))) {
    app.trailPointTtlMs = Math.max(500, Math.round(Number(payload.trailTtlMs)));
  }
  if (payload.trailMode === 'diffuse' || payload.trailMode === 'line') {
    app.trailRenderMode = payload.trailMode;
  }
  if (Number.isFinite(Number(payload.trailTeleportThreshold))) {
    app.trailTeleportThreshold = Number(payload.trailTeleportThreshold);
  }
  for (const fn of stateListeners) {
    try {
      fn(payload);
    } catch (e) {
      pushLog('warn', `overlay state listener failed: ${e}`);
    }
  }
}

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

/** The engine's last reported overlay state (not a write-only local guess). */
export function getMpvOverlayStatus() {
  return { enabled: overlay.enabled };
}

/** Mirror Studio's "Object energy field" toggle onto the mpv overlay heatmap. */
export function setMpvOverlayHeatmapEnabled(enabled) {
  invoke('mpv_overlay_set_heatmap_enabled', { enabled: Boolean(enabled) }).catch(() => {});
}

/** Push the shared custom gradient stops to the overlay as a flat [pos,r,g,b,…]. */
export function setMpvOverlayHeatmapCustomStops(stops) {
  const flat = [];
  if (Array.isArray(stops)) {
    for (const s of stops) {
      flat.push(Number(s?.pos) || 0, Number(s?.r) || 0, Number(s?.g) || 0, Number(s?.b) || 0);
    }
  }
  invoke('mpv_overlay_set_heatmap_custom_stops', { stops: flat }).catch(() => {});
}

// Push Studio's current overlay display prefs to the renderer. The overlay
// controls are otherwise only sent on manual toggle, so without this the
// renderer keeps its defaults after a (re)connect or orender restart — the
// persisted Studio values would not apply until the user touched each control.
// Call on connection (snapshot ready). All sends are best-effort.
export function syncMpvOverlayPrefs() {
  invoke('mpv_overlay_set_objects', { visible: app.objectsVisible !== false }).catch(() => {});
  invoke('mpv_overlay_set_heatmap_enabled', { enabled: Boolean(app.objectEnergyHeatmapEnabled) }).catch(() => {});
  invoke('mpv_overlay_set_heatmap_bands', { count: app.objectEnergyHeatmapBandCount }).catch(() => {});
  setMpvOverlayHeatmapCustomStops(app.objectCustomGradientStops);
  invoke('mpv_overlay_set_heatmap_colormap', { colormap: colormapIndex(app.objectEnergyColormap) }).catch(() => {});
  invoke('mpv_overlay_set_labels', { enabled: app.objectLabelsEnabled }).catch(() => {});
  pushMpvOverlayTrailPrefs(
    app.trailsEnabled,
    app.trailPointTtlMs,
    app.trailRenderMode,
    app.trailTeleportThreshold
  );
}
