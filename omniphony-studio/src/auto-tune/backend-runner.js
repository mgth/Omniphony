// Frontend face of the backend auto-tuner.
//
// Same interface as the local runner, so the wizard cannot tell which one it
// is driving. The state machine, the 50 ms tick and the OSC patches all live
// in Rust (`src-tauri/src/auto_tune/runner.rs`); this only relays.
//
// Two things it still does itself:
//
// - **Mirrors the applied values into `app.*`.** The backend patches the
//   renderer directly, and while the renderer does echo the new values back
//   over OSC, nothing on this side listens for those echoes at runtime —
//   `applyInitState` reads them once, at startup. Without this the panel would
//   sit on stale numbers for the whole run.
// - **Subscribes before starting.** `auto_tune_start` emits the first
//   `applyParams` and `progress` synchronously, so a listener attached after
//   the invoke would miss step 1 and the wizard would show nothing.

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

import { app } from '../state.js';
import { updateAdaptiveResamplingUI } from '../controls/adaptive.js';

export function createBackendAutoTuneRunner() {
  const listeners = new Set();
  let unlisten = null;

  function emit(event, payload) {
    for (const fn of listeners) {
      try {
        fn(event, payload);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.error('[auto-tune backend] listener error', err);
      }
    }
  }

  function on(fn) {
    listeners.add(fn);
    return () => listeners.delete(fn);
  }

  function mirror(patch) {
    if (typeof patch.kpNear === 'number') app.adaptiveResamplingKpNear = patch.kpNear;
    if (typeof patch.ki === 'number') app.adaptiveResamplingKi = patch.ki;
    if (typeof patch.maxAdjust === 'number') app.adaptiveResamplingMaxAdjust = patch.maxAdjust;
    if (typeof patch.updateIntervalCallbacks === 'number') {
      app.adaptiveResamplingUpdateIntervalCallbacks = Math.max(1, Math.round(patch.updateIntervalCallbacks));
    }
    updateAdaptiveResamplingUI();
  }

  async function subscribe() {
    if (unlisten) return;
    unlisten = await listen('auto_tune:event', ({ payload: wrapped }) => {
      const { event, payload } = wrapped || {};
      if (!event) return;
      if (event === 'applyParams') {
        mirror(payload || {});
        return;
      }
      emit(event, payload || {});
    });
  }

  function unsubscribe() {
    if (unlisten) {
      unlisten();
      unlisten = null;
    }
  }

  async function start() {
    await subscribe();
    try {
      await invoke('auto_tune_start');
      return { started: true };
    } catch (err) {
      // The command's error *is* the refusal reason the wizard has strings for.
      const reason = String(err);
      emit('refused', { reason });
      return { started: false, reason };
    }
  }

  async function cancel() {
    try {
      return await invoke('auto_tune_cancel');
    } finally {
      unsubscribe();
    }
  }

  async function accept() {
    try {
      return await invoke('auto_tune_accept');
    } finally {
      unsubscribe();
    }
  }

  function userAck(kind) {
    return invoke('auto_tune_ack', { kind });
  }

  function abbreviate() {
    return invoke('auto_tune_abbreviate');
  }

  function getState() {
    return invoke('auto_tune_state');
  }

  return { on, start, cancel, accept, userAck, abbreviate, getState };
}
