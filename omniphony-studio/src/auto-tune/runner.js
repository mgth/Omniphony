// Glue layer between the PI auto-tune FSM and the studio runtime.
//
// `createAutoTuneRunner()` picks an implementation at start: the local one
// below, or the backend port behind the `rust_auto_tune` config flag. The
// wizard drives whichever it gets through the same interface.
//
// The local runner owns the polling loop (50 ms), forwards app.* telemetry to the FSM,
// applies kpNear/ki/maxAdjust/updateIntervalCallbacks patches via
// sendAudioConfig(), and persists/restores the initial values via the
// Tauri auto_tune_snapshot_* commands so the wizard survives a WebView
// reload.
//
// Never touches adaptiveResamplingIntegralDischargeRatio (non-operative).

import { invoke } from '@tauri-apps/api/core';
import { app } from '../state.js';
import { sendAudioConfig, buildAudioConfigPayload } from '../controls/audio.js';
import { updateAdaptiveResamplingUI } from '../controls/adaptive.js';
import { createAutoTuneStateMachine } from './state-machine.js';
import { createBackendAutoTuneRunner } from './backend-runner.js';

const POLL_INTERVAL_MS = 50;

export function createLocalAutoTuneRunner() {
  let fsm = null;
  let intervalId = null;
  const listeners = new Set();

  function emit(event, payload) {
    for (const fn of listeners) {
      try {
        fn(event, payload);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.error('[auto-tune runner] listener error', err);
      }
    }
  }

  function on(fn) {
    listeners.add(fn);
    return () => listeners.delete(fn);
  }

  function preflight() {
    if (app.adaptiveResamplingEnabled !== true) {
      return { ok: false, reason: 'not-enabled' };
    }
    if (app.adaptiveResamplingPaused === true) {
      return { ok: false, reason: 'paused' };
    }
    return { ok: true };
  }

  async function applyPatch(partial) {
    if ('kpNear' in partial) app.adaptiveResamplingKpNear = Number(partial.kpNear);
    if ('ki' in partial) app.adaptiveResamplingKi = Number(partial.ki);
    if ('maxAdjust' in partial) app.adaptiveResamplingMaxAdjust = Number(partial.maxAdjust);
    if ('updateIntervalCallbacks' in partial) {
      app.adaptiveResamplingUpdateIntervalCallbacks = Math.max(1, Math.round(Number(partial.updateIntervalCallbacks)));
    }
    updateAdaptiveResamplingUI();
    await sendAudioConfig();
  }

  function pollOnce() {
    if (!fsm) return;
    const sample = {
      t: Date.now(),
      latencySmoothedMs: typeof app.latencySmoothedMs === 'number' ? app.latencySmoothedMs : null,
      latencyTargetMs: typeof app.latencyTargetMs === 'number' ? app.latencyTargetMs : null,
      resampleRatio: typeof app.resampleRatio === 'number' ? app.resampleRatio : null,
      phase: app.adaptiveResamplingState ?? null,
    };
    fsm.pushSample(sample);
  }

  function stopPolling() {
    if (intervalId !== null) {
      clearInterval(intervalId);
      intervalId = null;
    }
  }

  async function start(options = {}) {
    if (fsm) {
      return { started: false, reason: 'already-running' };
    }
    const check = preflight();
    if (!check.ok) {
      emit('refused', { reason: check.reason });
      return { started: false, reason: check.reason };
    }

    const snapshot = buildAudioConfigPayload().adaptiveResampling;
    await invoke('auto_tune_snapshot_save', { snapshot });

    fsm = createAutoTuneStateMachine(options);
    fsm.on((event, payload) => {
      if (event === 'applyParams') {
        applyPatch(payload).catch((err) => emit('error', { kind: 'apply-failed', err: String(err) }));
        return;
      }
      emit(event, payload);
    });

    fsm.start(Date.now());
    intervalId = setInterval(pollOnce, POLL_INTERVAL_MS);
    return { started: true };
  }

  async function restoreSnapshot() {
    const snap = await invoke('auto_tune_snapshot_take');
    if (!snap || typeof snap !== 'object') return false;
    const patch = {};
    if ('kpNear' in snap) patch.kpNear = snap.kpNear;
    if ('ki' in snap) patch.ki = snap.ki;
    if ('maxAdjust' in snap) patch.maxAdjust = snap.maxAdjust;
    if ('updateIntervalCallbacks' in snap) patch.updateIntervalCallbacks = snap.updateIntervalCallbacks;
    if (Object.keys(patch).length === 0) return false;
    await applyPatch(patch);
    return true;
  }

  async function cancel() {
    if (!fsm) return false;
    fsm.cancel();
    stopPolling();
    fsm = null;
    try {
      await restoreSnapshot();
    } catch (err) {
      // eslint-disable-next-line no-console
      console.error('[auto-tune runner] restore failed', err);
    }
    return true;
  }

  async function accept() {
    stopPolling();
    fsm = null;
    try {
      await invoke('auto_tune_snapshot_take');
    } catch (err) {
      // eslint-disable-next-line no-console
      console.error('[auto-tune runner] clear snapshot failed', err);
    }
    return true;
  }

  function userAck(kind) {
    return fsm ? fsm.userAck(kind) : false;
  }

  function abbreviate() {
    return fsm ? fsm.abbreviate() : false;
  }

  function getState() {
    return fsm ? fsm.getState() : 'idle';
  }

  function getContext() {
    return fsm ? fsm.getContext() : null;
  }

  async function hasPendingSnapshot() {
    try {
      const snap = await invoke('auto_tune_snapshot_peek');
      return snap && typeof snap === 'object';
    } catch (err) {
      return false;
    }
  }

  return {
    on,
    start,
    cancel,
    accept,
    userAck,
    abbreviate,
    getState,
    getContext,
    restoreSnapshot,
    hasPendingSnapshot,
  };
}

/**
 * The runner the wizard gets: the backend port when `rust_auto_tune` is on,
 * the local state machine otherwise.
 *
 * The choice is made at `start()`, not at construction, so `on()` can be
 * registered first — the backend emits its first events synchronously inside
 * `auto_tune_start`, and a listener attached afterwards would miss step 1.
 */
export function createAutoTuneRunner() {
  const listeners = new Set();
  let impl = null;

  function emit(event, payload) {
    for (const fn of listeners) {
      try {
        fn(event, payload);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.error('[auto-tune runner] listener error', err);
      }
    }
  }

  async function pick() {
    if (impl) return impl;
    let backend = false;
    try {
      backend = (await invoke('auto_tune_backend_enabled')) === true;
    } catch (err) {
      // An older backend has no such command; the local runner is the default
      // anyway, so this is the right way to fail.
      backend = false;
    }
    impl = backend ? createBackendAutoTuneRunner() : createLocalAutoTuneRunner();
    impl.on(emit);
    return impl;
  }

  return {
    on(fn) {
      listeners.add(fn);
      return () => listeners.delete(fn);
    },
    async start(options) {
      return (await pick()).start(options);
    },
    async cancel() {
      return impl ? impl.cancel() : false;
    },
    async accept() {
      return impl ? impl.accept() : false;
    },
    userAck(kind) {
      return impl ? impl.userAck(kind) : false;
    },
    abbreviate() {
      return impl ? impl.abbreviate() : false;
    },
    async getState() {
      return impl ? impl.getState() : 'idle';
    },
  };
}
