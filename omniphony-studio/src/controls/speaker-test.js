/**
 * Speaker test signal: band-limited pink noise on one speaker, for identifying
 * it and checking its crossover assignment by ear.
 *
 * The renderer contract is deliberately small — "play on speaker N at level L
 * with isolation I", or "stop". Everything about *when* a test runs is policy
 * and lives here: the trigger modes, the safety timeout, and stopping when the
 * selection changes or the link drops. That keeps the renderer free of UI
 * timing and means a new trigger mode never touches the audio path.
 */

import { invoke } from '@tauri-apps/api/core';
import { app } from '../state.js';
import { t } from '../i18n.js';

const MODE_KEY = 'speakerTest.mode.v1';
const ISOLATION_KEY = 'speakerTest.isolation.v1';
const LEVEL_KEY = 'speakerTest.levelDb.v1';

const BURST_MS = 2000;
/** Toggle mode cannot run forever: an unattended test is a room full of noise. */
const TOGGLE_SAFETY_MS = 60_000;

const DEFAULT_LEVEL_DB = -20;

let stopTimer = null;
/** Speaker index currently under test, or null. */
let running = null;

function el(id) { return document.getElementById(id); }

function load(key, fallback) {
  try {
    const v = localStorage.getItem(key);
    return v === null ? fallback : v;
  } catch (_) { return fallback; }
}

function save(key, value) {
  try { localStorage.setItem(key, String(value)); } catch (_) { /* ignore */ }
}

function levelDb() {
  const n = Number(load(LEVEL_KEY, DEFAULT_LEVEL_DB));
  return Number.isFinite(n) ? Math.min(-6, Math.max(-60, n)) : DEFAULT_LEVEL_DB;
}

/** dBFS → linear amplitude against the renderer's unit-RMS generator. */
function levelLinear() {
  return 10 ** (levelDb() / 20);
}

function clearTimer() {
  if (stopTimer !== null) {
    clearTimeout(stopTimer);
    stopTimer = null;
  }
}

function send(id) {
  invoke('control_speaker_test', {
    id,
    level: levelLinear(),
    isolation: load(ISOLATION_KEY, 'test_only')
  }).catch(() => { /* renderer gone: the test is moot */ });
}

/**
 * Stop any running test. Safe to call unconditionally — the stop is sent even
 * when this module thinks nothing is running, because "nothing is running" is
 * our belief and the renderer's state is the one that matters.
 */
export function stopSpeakerTest({ force = false } = {}) {
  clearTimer();
  if (running === null && !force) return;
  running = null;
  send(-1);
  renderSpeakerTestUI();
}

function startSpeakerTest(index) {
  if (index === null || index === undefined) return;
  clearTimer();
  running = index;
  send(index);
  const mode = load(MODE_KEY, 'toggle');
  if (mode === 'burst') {
    stopTimer = setTimeout(() => stopSpeakerTest(), BURST_MS);
  } else if (mode === 'toggle') {
    // Not a feature, a guard: if the user walks away mid-test the room should
    // fall quiet on its own.
    stopTimer = setTimeout(() => stopSpeakerTest(), TOGGLE_SAFETY_MS);
  }
  renderSpeakerTestUI();
}

/** Reflect state on the button and the level readout. */
export function renderSpeakerTestUI() {
  const btn = el('speakerTestBtn');
  if (btn) {
    const active = running !== null && running === app.selectedSpeakerIndex;
    btn.classList.toggle('active', active);
    btn.textContent = active ? t('speaker.testStop') : t('speaker.testPlay');
    btn.disabled = app.selectedSpeakerIndex === null;
  }
  const box = el('speakerTestLevelBox');
  if (box) box.textContent = `${levelDb()} dBFS`;
  const slider = el('speakerTestLevelSlider');
  if (slider && document.activeElement !== slider) slider.value = String(levelDb());
  const modeSel = el('speakerTestModeSelect');
  if (modeSel) modeSel.value = load(MODE_KEY, 'toggle');
  const isoSel = el('speakerTestIsolationSelect');
  if (isoSel) isoSel.value = load(ISOLATION_KEY, 'test_only');
}

export function setupSpeakerTestListeners() {
  const btn = el('speakerTestBtn');
  if (btn) {
    // Hold mode needs press/release; the other two act on a plain click. Both
    // are bound, and each ignores the events belonging to the other mode.
    btn.addEventListener('click', (event) => {
      event.preventDefault();
      if (load(MODE_KEY, 'toggle') === 'hold') return;
      if (running !== null) stopSpeakerTest();
      else startSpeakerTest(app.selectedSpeakerIndex);
    });
    btn.addEventListener('pointerdown', (event) => {
      if (load(MODE_KEY, 'toggle') !== 'hold') return;
      event.preventDefault();
      startSpeakerTest(app.selectedSpeakerIndex);
    });
    // pointerup on the window, not the button: releasing after dragging off it
    // must still stop the noise.
    window.addEventListener('pointerup', () => {
      if (load(MODE_KEY, 'toggle') !== 'hold') return;
      stopSpeakerTest();
    });
  }

  const modeSel = el('speakerTestModeSelect');
  if (modeSel) {
    modeSel.addEventListener('change', () => {
      save(MODE_KEY, modeSel.value);
      // The running test was started under the previous policy, whose stop
      // condition may no longer exist — end it rather than orphan it.
      stopSpeakerTest();
    });
  }

  const isoSel = el('speakerTestIsolationSelect');
  if (isoSel) {
    isoSel.addEventListener('change', () => {
      save(ISOLATION_KEY, isoSel.value);
      // Isolation is carried in the start message, so re-send to apply it live.
      if (running !== null) send(running);
    });
  }

  const slider = el('speakerTestLevelSlider');
  if (slider) {
    slider.addEventListener('input', () => {
      save(LEVEL_KEY, slider.value);
      renderSpeakerTestUI();
      if (running !== null) send(running);
    });
  }

  // A test belongs to the speaker it was started on: selecting another one, or
  // closing the editor, must not leave it playing.
  window.addEventListener('beforeunload', () => stopSpeakerTest({ force: true }));
}

/** Called when the speaker selection changes. */
export function onSpeakerSelectionChanged() {
  if (running !== null && running !== app.selectedSpeakerIndex) stopSpeakerTest();
  else renderSpeakerTestUI();
}
