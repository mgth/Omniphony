// Headphone L/R channel rows for the binaural modes — same rendering as the
// speaker rows (same DOM classes, same meter pipeline). The ears carry
// DEDICATED engine params (they used to ride the first two per-speaker
// slots, which now belong to the virtual FL/FR rows in cascaded mode):
// meters come from /omniphony/meter/ear/0|1 and M/S drive the
// control_ear_mute command, with solo composed client-side from the two
// mutes.
//
// Position icon: a top-view headphone glyph with the active cup filled
// (left cup for L, right for R). No crossover/filter glyph — there is no
// per-ear crossover.

import { invoke } from '@tauri-apps/api/core';
import { updateMeterUI } from '../mute-solo.js';
import { updateItemClasses } from '../flush.js';

const entries = new Map(); // '0' | '1' → row entry
// Engine-confirmed mute state per ear (reflected from the state broadcast).
const earMuted = new Set();
// Client-side solo: the ear whose S button is lit, or null. Solo is sugar
// over the two mutes — engaging it mutes the other ear, releasing restores.
let earSolo = null;

function sendEarMute(id, muted) {
  invoke('control_ear_mute', { ear: Number(id), muted }).catch((e) =>
    console.error('[ears] control_ear_mute', e),
  );
}

function toggleEarMute(id) {
  // Manual mute edits drop the solo interpretation (same feel as the
  // speaker rows): the buttons then just mirror the two raw mutes.
  earSolo = null;
  sendEarMute(id, !earMuted.has(id));
}

function toggleEarSolo(id) {
  const other = id === '0' ? '1' : '0';
  if (earSolo === id) {
    earSolo = null;
    sendEarMute(other, false);
  } else {
    earSolo = id;
    sendEarMute(id, false);
    sendEarMute(other, true);
  }
}

function headphoneSvg(side) {
  const fillL = side === 'L' ? '#8cd6ff' : 'none';
  const fillR = side === 'R' ? '#8cd6ff' : 'none';
  return `<svg viewBox="0 0 20 20" width="16" height="16" aria-hidden="true">
    <path d="M3 12 a7 7 0 0 1 14 0" fill="none" stroke="#9eb4c8" stroke-width="1.6"/>
    <rect x="2" y="11" width="4" height="6" rx="1.2" fill="${fillL}" stroke="#9eb4c8" stroke-width="1.2"/>
    <rect x="14" y="11" width="4" height="6" rx="1.2" fill="${fillR}" stroke="#9eb4c8" stroke-width="1.2"/>
  </svg>`;
}

function createChannelRow(side, id) {
  const root = document.createElement('div');
  root.className = 'info-item speaker-item';

  const idStrip = document.createElement('div');
  idStrip.className = 'id-strip flip';
  const idText = document.createElement('span');
  idText.textContent = side;
  idStrip.appendChild(idText);

  const content = document.createElement('div');
  content.className = 'speaker-content';

  const level = document.createElement('div');
  // hp variant: same grid minus the crossover-glyph column (4 columns).
  level.className = 'meter-row speaker-meter-row hp-meter-row';

  const positionIcon = document.createElement('span');
  positionIcon.className = 'speaker-position-icon';
  positionIcon.innerHTML = headphoneSvg(side);
  level.appendChild(positionIcon);

  const levelText = document.createElement('div');
  levelText.className = 'fixed-metric';
  level.appendChild(levelText);

  const meterBar = document.createElement('div');
  meterBar.className = 'meter-bar level-meter';
  const meterFill = document.createElement('div');
  meterFill.className = 'meter-fill';
  const peakCursor = document.createElement('div');
  peakCursor.className = 'meter-peak';
  meterBar.appendChild(meterFill);
  meterBar.appendChild(peakCursor);

  const controlsRow = document.createElement('div');
  controlsRow.className = 'speaker-meter-actions';
  const muteBtn = document.createElement('button');
  muteBtn.type = 'button';
  muteBtn.className = 'toggle-btn';
  muteBtn.textContent = 'M';
  muteBtn.addEventListener('click', (e) => {
    e.preventDefault();
    toggleEarMute(id);
  });
  const soloBtn = document.createElement('button');
  soloBtn.type = 'button';
  soloBtn.className = 'toggle-btn';
  soloBtn.textContent = 'S';
  soloBtn.addEventListener('click', (e) => {
    e.preventDefault();
    toggleEarSolo(id);
  });
  controlsRow.append(muteBtn, soloBtn);

  level.appendChild(meterBar);
  level.appendChild(controlsRow);
  content.appendChild(level);
  root.append(idStrip, content);

  return { root, levelText, meterFill, peakCursor, muteBtn, soloBtn };
}

/** Build the two rows into #hpChannelsList (idempotent). */
export function initHeadphoneChannels() {
  const list = document.getElementById('hpChannelsList');
  if (!list || entries.size) return;
  for (const [side, id] of [['L', '0'], ['R', '1']]) {
    const entry = createChannelRow(side, id);
    entries.set(id, entry);
    list.appendChild(entry.root);
  }
}

/** Meter update for ear `index` (0|1); same pipeline as the speaker rows. */
export function updateHeadphoneMeter(index, level) {
  const entry = entries.get(String(index));
  if (!entry) return;
  updateMeterUI(entry, level, 'ear', `hp${index}`);
}

/** Mirror the mute/solo state onto the L/R buttons (same classes as rows). */
export function updateHeadphoneControlsUI() {
  entries.forEach((entry, id) => {
    const muted = earMuted.has(id);
    entry.muteBtn.classList.toggle('active', muted);
    entry.soloBtn.classList.toggle('active', earSolo === id);
    updateItemClasses(entry, muted, Boolean(earSolo && earSolo !== id));
  });
}

/** Reflect the engine's ear params (from the state broadcast: b.ears). */
export function applyEarState(ears) {
  if (!Array.isArray(ears)) return;
  ears.forEach((e, i) => {
    const id = String(i);
    if (e && typeof e.muted === 'boolean') {
      if (e.muted) earMuted.add(id);
      else earMuted.delete(id);
    }
  });
  // A mute pattern that no longer matches the solo interpretation drops it.
  if (earSolo !== null) {
    const other = earSolo === '0' ? '1' : '0';
    if (earMuted.has(earSolo) || !earMuted.has(other)) earSolo = null;
  }
  updateHeadphoneControlsUI();
}
