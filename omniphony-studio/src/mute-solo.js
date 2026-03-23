/**
 * Audio level utilities and mute/solo/gain logic.
 *
 * Extracted from app.js — formatLevel, meterToPercent, linearToDb, dbToLinear,
 * updateMeterUI, send*Gain, send*Mute, solo helpers, applyGroupGains,
 * getBaseGain, toggleMute, toggleSolo.
 */

import { invoke } from '@tauri-apps/api/core';
import { formatNumber } from './coordinates.js';
import {
  speakerMuted,
  objectMuted,
  speakerManualMuted,
  objectManualMuted,
  speakerGainCache,
  objectGainCache,
  speakerBaseGains,
  objectBaseGains,
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

export function meterToPercent(meter) {
  const db = typeof meter?.rmsDbfs === 'number' ? meter.rmsDbfs : -100;
  const clamped = Math.min(0, Math.max(-100, db));
  return ((clamped + 100) / 100) * 100;
}

export function linearToDb(value) {
  const v = Number(value);
  if (!Number.isFinite(v) || v <= 0) {
    return '-∞ dB';
  }
  return `${(20 * Math.log10(v)).toFixed(1)} dB`;
}

export function dbToLinear(db) {
  const v = Number(db);
  if (!Number.isFinite(v)) {
    return 0;
  }
  return Math.pow(10, v / 20);
}

// ---------------------------------------------------------------------------
// Meter UI
// ---------------------------------------------------------------------------

export function updateMeterUI(entry, meter) {
  if (!entry) return;
  entry.levelText.textContent = formatLevel(meter);
  entry.meterFill.style.setProperty('--level', `${meterToPercent(meter).toFixed(1)}%`);
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

export function sendObjectGain(id, gain) {
  invoke('control_object_gain', { id: Number(id), gain: Number(gain) });
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
  invoke('control_object_mute', { id: Number(id), muted: muted ? 1 : 0 });
}

export function sendSpeakerMute(id, muted) {
  invoke('control_speaker_mute', { id: Number(id), muted: muted ? 1 : 0 });
}

// ---------------------------------------------------------------------------
// Apply group gains
// ---------------------------------------------------------------------------

export function applyGroupGains(group) {
  const isSpeaker = group === 'speaker';
  const ids = isSpeaker ? getSpeakerIds() : getObjectIds();
  const baseMap = isSpeaker ? speakerBaseGains : objectBaseGains;
  const cache = isSpeaker ? speakerGainCache : objectGainCache;

  ids.forEach((id) => {
    const baseGain = getBaseGain(baseMap, cache, id);
    if (isSpeaker) {
      sendSpeakerGain(id, baseGain);
    } else {
      sendObjectGain(id, baseGain);
    }
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
