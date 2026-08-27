/**
 * Audio level utilities and mute/solo/gain logic.
 *
 * Extracted from app.js — formatLevel, meterToPercent, formatLinearAsDb,
 * updateMeterUI, send*Gain, send*Mute, solo helpers, applyGroupGains,
 * getBaseGain, toggleMute, toggleSolo.
 */

import { OBJECT_TEST_SOURCE_ID } from './object-test-id.js';
import { setObjectTestMuted } from './controls/object-test.js';
import { invoke } from '@tauri-apps/api/core';
import { formatNumber } from './coordinates.js';
import { linearToDb } from './audio-math.js';
import {
  speakerMuted,
  objectMuted,
  speakerManualMuted,
  objectManualMuted,
  speakerGainCache,
  speakerBaseGains,
  speakerItems,
  objectItems,
  sourceMeshes,
  app
} from './state.js';

// ---------------------------------------------------------------------------
// Callbacks that other modules populate (e.g. updateSpeakerControlsUI,
// updateObjectControlsUI, setSelectedSource).
// ---------------------------------------------------------------------------

export const muteSoloCallbacks = {
  updateSpeakerControlsUI: null,
  updateObjectControlsUI: null,
  setSelectedSource: null
};

// ---------------------------------------------------------------------------
// Level formatting / conversion
// ---------------------------------------------------------------------------

export function formatLevel(meter) {
  if (!meter) {
    return '— dB';
  }
  return `${formatNumber(meter.rmsDbfs, 1)} dB`;
}

// Meter scale: -60 dBFS at the bottom, +6 dBFS at the top. 0 dBFS sits at
// dbToMeterPercent(0) ≈ 90.9%, leaving a headroom (over-0) zone above it so
// true clipping peaks are visible instead of being flattened at the top.
export const METER_DB_MIN = -60;
export const METER_DB_MAX = 6;

export function dbToMeterPercent(db) {
  const v = Number.isFinite(db) ? db : METER_DB_MIN;
  const pct = ((v - METER_DB_MIN) / (METER_DB_MAX - METER_DB_MIN)) * 100;
  return Math.min(100, Math.max(0, pct));
}

export function meterToPercent(meter) {
  const db = typeof meter?.rmsDbfs === 'number' ? meter.rmsDbfs : METER_DB_MIN;
  return dbToMeterPercent(db);
}

/**
 * Amplitude ratio → display string, e.g. `"-12.3 dB"`.
 *
 * Named for what it returns: a label, not a number. For the value, use
 * `linearToDb` in `audio-math.js`.
 */
export function formatLinearAsDb(value) {
  const v = Number(value);
  if (!Number.isFinite(v) || v <= 0) {
    return '-∞ dB';
  }
  return `${linearToDb(v).toFixed(1)} dB`;
}

// ---------------------------------------------------------------------------
// Meter UI
// ---------------------------------------------------------------------------

// The peak cursor holds the engine-reported true sample peak so a transient
// stays readable after it has passed. The hold and its decay are computed in
// the backend (`src-tauri/src/peak_hold.rs`) and arrive as `peakHoldDbfs` on
// every meter payload; the cursor turns red (`.over`) once the held peak
// crosses 0 dBFS, into the headroom zone.

export function updateMeterUI(entry, meter, type = null, id = null) {
  if (!entry) return;
  const rmsDb = typeof meter?.rmsDbfs === 'number' ? meter.rmsDbfs : METER_DB_MIN;
  const peakDb = typeof meter?.peakDbfs === 'number' ? meter.peakDbfs : rmsDb;
  // Bar and cursor are the same quantity (peak) so the fill rises to the hold
  // marker on transients instead of leaving a permanent crest-factor gap; the
  // RMS stays as the numeric readout.
  const levelPercent = dbToMeterPercent(peakDb);

  if (entry.peakCursor) {
    const holdDb = typeof meter?.peakHoldDbfs === 'number' ? meter.peakHoldDbfs : peakDb;
    const holdPercent = dbToMeterPercent(holdDb);

    entry.levelText.textContent = `${formatNumber(rmsDb, 1)} dB`;
    entry.meterFill.style.setProperty('--level', `${levelPercent.toFixed(1)}%`);
    entry.peakCursor.style.setProperty('--level', `${holdPercent.toFixed(1)}%`);
    entry.peakCursor.style.opacity = holdPercent > 0.1 ? '1' : '0';
    entry.peakCursor.classList.toggle('over', holdDb >= 0);
  } else {
    entry.levelText.textContent = formatLevel(meter);
    entry.meterFill.style.setProperty('--level', `${levelPercent.toFixed(1)}%`);
  }
}

// ---------------------------------------------------------------------------
// Gain helpers
// ---------------------------------------------------------------------------

export function getBaseGain(map, cache, id) {
  if (map.has(id)) {
    return map.get(id);
  }
  if (cache.has(id)) {
    return cache.get(id);
  }
  return 1;
}

export function sendSpeakerGain(id, gain) {
  invoke('control_speaker_gain', { id: Number(id), gain: Number(gain) });
}

// ---------------------------------------------------------------------------
// ID helpers (private)
// ---------------------------------------------------------------------------

function getSpeakerIds() {
  return app.currentLayoutSpeakers.map((_, index) => String(index));
}

function getObjectIds() {
  return [...sourceMeshes.keys()].map((id) => String(id));
}

// ---------------------------------------------------------------------------
// Solo helpers
// ---------------------------------------------------------------------------

export function getSoloTarget(group) {
  const ids = group === 'speaker' ? getSpeakerIds() : getObjectIds();
  const mutedSet = group === 'speaker' ? speakerMuted : objectMuted;
  if (ids.length <= 1) {
    return null;
  }

  const unmuted = ids.filter((id) => !mutedSet.has(id));
  if (unmuted.length !== 1) {
    return null;
  }

  const target = unmuted[0];
  const othersMuted = ids.every((id) => id === target || mutedSet.has(id));
  return othersMuted ? target : null;
}

export function areAllOthersMuted(group, id) {
  const ids = group === 'speaker' ? getSpeakerIds() : getObjectIds();
  const mutedSet = group === 'speaker' ? speakerMuted : objectMuted;
  return ids.every((other) => other === id || mutedSet.has(other));
}

// ---------------------------------------------------------------------------
// Mute send
// ---------------------------------------------------------------------------

export function sendObjectMute(id, muted) {
  // The injected test source is addressed by name, and this command takes a
  // number — `Number('injection')` is NaN, which is why its M and S buttons
  // did nothing. It owns its own muting instead.
  if (String(id) === OBJECT_TEST_SOURCE_ID) {
    setObjectTestMuted(muted);
    return;
  }
  invoke('control_object_mute', { id: Number(id), muted: muted ? 1 : 0 });
}

export function sendSpeakerMute(id, muted) {
  invoke('control_speaker_mute', { id: Number(id), muted: muted ? 1 : 0 });
}

// ---------------------------------------------------------------------------
// Apply group gains
// ---------------------------------------------------------------------------

export function applySpeakerGroupGains() {
  getSpeakerIds().forEach((id) => {
    const baseGain = getBaseGain(speakerBaseGains, speakerGainCache, id);
    sendSpeakerGain(id, baseGain);
  });
}

// ---------------------------------------------------------------------------
// Toggle mute / solo
// ---------------------------------------------------------------------------

export function toggleMute(group, id) {
  const mutedSet = group === 'speaker' ? speakerMuted : objectMuted;
  const manualMutedSet = group === 'speaker' ? speakerManualMuted : objectManualMuted;
  if (mutedSet.has(id)) {
    mutedSet.delete(id);
    manualMutedSet.delete(id);
  } else {
    mutedSet.add(id);
    manualMutedSet.add(id);
  }
  if (group === 'speaker') {
    sendSpeakerMute(id, speakerMuted.has(id));
    muteSoloCallbacks.updateSpeakerControlsUI?.();
  } else {
    sendObjectMute(id, objectMuted.has(id));
    muteSoloCallbacks.updateObjectControlsUI?.();
  }
}

export function toggleSolo(group, id) {
  const isSpeaker = group === 'speaker';
  const ids = isSpeaker ? getSpeakerIds() : getObjectIds();
  const mutedSet = isSpeaker ? speakerMuted : objectMuted;
  const manualMutedSet = isSpeaker ? speakerManualMuted : objectManualMuted;
  const currentSolo = getSoloTarget(group);

  if (currentSolo && currentSolo !== id) {
    mutedSet.add(currentSolo);
    manualMutedSet.add(currentSolo);
    mutedSet.delete(id);
    manualMutedSet.delete(id);
    if (isSpeaker) {
      sendSpeakerMute(currentSolo, true);
      sendSpeakerMute(id, false);
      muteSoloCallbacks.updateSpeakerControlsUI?.();
    } else {
      sendObjectMute(currentSolo, true);
      sendObjectMute(id, false);
      muteSoloCallbacks.updateObjectControlsUI?.();
      muteSoloCallbacks.setSelectedSource?.(id);
    }
    return;
  }

  if (currentSolo === id) {
    ids.forEach((other) => {
      if (other === id) {
        return;
      }
      mutedSet.delete(other);
      manualMutedSet.delete(other);
      if (isSpeaker) {
        sendSpeakerMute(other, false);
      } else {
        sendObjectMute(other, false);
      }
    });
    if (isSpeaker) {
      muteSoloCallbacks.updateSpeakerControlsUI?.();
    } else {
      muteSoloCallbacks.updateObjectControlsUI?.();
    }
    return;
  }

  ids.forEach((other) => {
    if (other === id) {
      return;
    }
    if (!mutedSet.has(other)) {
      mutedSet.add(other);
      if (isSpeaker) {
        sendSpeakerMute(other, true);
      } else {
        sendObjectMute(other, true);
      }
    }
  });

  if (!isSpeaker) {
    muteSoloCallbacks.setSelectedSource?.(id);
  }

  if (isSpeaker) {
    muteSoloCallbacks.updateSpeakerControlsUI?.();
  } else {
    muteSoloCallbacks.updateObjectControlsUI?.();
  }
}
