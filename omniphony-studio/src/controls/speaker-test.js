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
/**
 * Bumped to v2 when the level became peak-referenced instead of RMS-referenced:
 * the same stored number now means a signal ~13 dB quieter, so reusing v1 would
 * silently drop every existing user's test level. Dropping the old key re-seeds
 * the new default, which is the same loudness they had before.
 */
const LEVEL_KEY = 'speakerTest.levelDb.v2';

const BURST_MS = 2000;
/** Toggle mode cannot run forever: an unattended test is a room full of noise. */
const TOGGLE_SAFETY_MS = 60_000;

/**
 * The renderer expires an idle-feed arm after a keepalive window (10 min)
 * rather than trust a client to always say goodbye; re-arm well inside it.
 * The periodic re-send also re-covers a renderer restart mid-session.
 */
const IDLE_FEED_REARM_MS = 120_000;

/**
 * Peak dBFS. -8 rather than a rounder number because it reproduces the loudness
 * of the old -20 dBFS default: that figure set the RMS, and pink noise peaked a
 * crest factor (~13 dB) above it. Same signal, honest number.
 */
const DEFAULT_LEVEL_DB = -8;

let stopTimer = null;
/** Speaker index currently under test, or null. */
let running = null;

let idleFeedTimer = null;
let idleFeedArmed = false;

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

/**
 * Test level in **peak** dBFS. 0 is full scale and is allowed: the renderer
 * bounds the test to this peak, so the top of the slider is the loudest signal
 * that still cannot clip, rather than a value that must be kept clear of.
 */
function levelDb() {
  const n = Number(load(LEVEL_KEY, DEFAULT_LEVEL_DB));
  return Number.isFinite(n) ? Math.min(0, Math.max(-60, n)) : DEFAULT_LEVEL_DB;
}

/** Peak dBFS → peak linear amplitude, which is what the renderer clamps to. */
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

function sendIdleFeed(enable) {
  invoke('control_speaker_test_idle_feed', { enable })
    .catch(() => { /* renderer gone: nothing to keep warm */ });
}

/**
 * The idle feed keeps the renderer's input→output chain warm while the Test
 * pane is open, so a test makes sound immediately even with nothing playing.
 * Armed exactly while (Test pane visible AND a speaker is selected); re-sent
 * periodically because the renderer expires the arm on a keepalive rather
 * than trust a client to always say goodbye.
 */
function syncIdleFeed() {
  const hasSpeaker = app.selectedSpeakerIndex !== null && app.selectedSpeakerIndex !== undefined;
  const want = document.body.classList.contains('speaker-tab-test') && hasSpeaker;
  if (want === idleFeedArmed) return;
  idleFeedArmed = want;
  if (want) {
    sendIdleFeed(true);
    idleFeedTimer = setInterval(() => sendIdleFeed(true), IDLE_FEED_REARM_MS);
  } else {
    clearInterval(idleFeedTimer);
    idleFeedTimer = null;
    sendIdleFeed(false);
  }
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

/**
 * Reflect state on the test controls.
 *
 * Also owns their enabled state. The speaker editor starts with everything
 * disabled and each control re-enables itself here once a speaker is selected —
 * the same contract `renderSpeakerEditor` follows for the layout and coordinate
 * controls. A control with no owner stays disabled forever, which is exactly
 * what happened to the tabs before this.
 */
export function renderSpeakerTestUI() {
  const hasSpeaker = app.selectedSpeakerIndex !== null && app.selectedSpeakerIndex !== undefined;
  // The tabs are navigation, not editing: usable whenever the editor is open.
  for (const id of ['speakerTabEditBtn', 'speakerTabTestBtn']) {
    const tab = el(id);
    if (tab) tab.disabled = !hasSpeaker;
  }
  for (const id of ['speakerTestModeSelect', 'speakerTestIsolationSelect', 'speakerTestLevelSlider']) {
    const c = el(id);
    if (c) c.disabled = !hasSpeaker;
  }
  const btn = el('speakerTestBtn');
  if (btn) {
    const active = running !== null && running === app.selectedSpeakerIndex;
    btn.classList.toggle('active', active);
    btn.textContent = active ? t('speaker.testStop') : t('speaker.testPlay');
    btn.disabled = !hasSpeaker;
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

/**
 * Select the Edit or Test pane of the speaker editor. Pure UI: a body class
 * drives the CSS, so nothing re-renders on a tab change.
 */
function setSpeakerTab(testTab) {
  document.body.classList.toggle('speaker-tab-test', testTab);
  const edit = el('speakerTabEditBtn');
  const test = el('speakerTabTestBtn');
  if (edit) edit.classList.toggle('active', !testTab);
  if (test) test.classList.toggle('active', testTab);
  syncIdleFeed();
}

export function setupSpeakerTestListeners() {
  const tabEdit = el('speakerTabEditBtn');
  if (tabEdit) {
    tabEdit.addEventListener('click', () => {
      // Leaving the Test pane must not leave noise playing behind it.
      stopSpeakerTest();
      setSpeakerTab(false);
    });
  }
  const tabTest = el('speakerTabTestBtn');
  if (tabTest) tabTest.addEventListener('click', () => setSpeakerTab(true));
  setSpeakerTab(document.body.classList.contains('speaker-tab-test'));

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

  // Whatever the trigger mode, closing the window must not leave noise playing.
  window.addEventListener('beforeunload', () => {
    stopSpeakerTest({ force: true });
    if (idleFeedArmed) sendIdleFeed(false);
  });
}

/** Called when the speaker selection changes. */
export function onSpeakerSelectionChanged() {
  const selected = app.selectedSpeakerIndex;
  const hasSpeaker = selected !== null && selected !== undefined;
  if (running !== null && running !== selected) {
    // Toggle means "keep testing until I say stop", and the whole point of
    // testing several speakers is comparing them: walk the selection down the
    // layout and the noise walks with it. Hold and burst each own an end
    // condition of their own (release, timeout), so for those a selection
    // change still ends the test rather than silently re-arming it elsewhere.
    if (hasSpeaker && load(MODE_KEY, 'toggle') === 'toggle') startSpeakerTest(selected);
    else stopSpeakerTest();
  } else {
    renderSpeakerTestUI();
  }
  syncIdleFeed();
}
