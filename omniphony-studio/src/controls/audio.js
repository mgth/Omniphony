/**
 * Audio format display controls.
 *
 * Extracted from app.js (lines 4295-4378).
 */

import { invoke } from '@tauri-apps/api/core';
import { app, dirty, AUDIO_SAMPLE_RATE_PRESETS } from '../state.js';
import { t, tf } from '../i18n.js';
import { scheduleUIFlush } from '../flush.js';
import { inAudioPanel } from '../ui/panel-roots.js';

// DOM refs
const audioFormatInfoEl = inAudioPanel('audioFormatInfo');
const audioOutputDeviceSelectEl = inAudioPanel('audioOutputDeviceSelect');
const rampModeSelectEl = inAudioPanel('rampModeSelect');
const audioSampleRateInputEl = inAudioPanel('audioSampleRateInput');
const audioSampleRateMenuEl = inAudioPanel('audioSampleRateMenu');
const audioOutputSummaryEl = inAudioPanel('audioOutputSummary');

export function renderAudioFormatDisplay() {
  if (audioFormatInfoEl) {
    const rateText = app.audioSampleRate ? `${app.audioSampleRate} Hz` : '—';
    const fmtText = app.audioSampleFormat || '—';
    const baseText = tf('status.audioFormat', { rate: rateText, format: fmtText });
    audioFormatInfoEl.textContent = app.audioError ? `${baseText} • Error: ${app.audioError}` : baseText;
  }
  if (audioOutputDeviceSelectEl) {
    const defaultLabel = app.oscSnapshotReady ? t('status.defaultOutputDevice') : '—';
    const options = [{ value: '', label: defaultLabel }, ...app.audioOutputDevices];
    if (app.audioOutputDevice && !options.some((entry) => entry.value === app.audioOutputDevice)) {
      options.push({ value: app.audioOutputDevice, label: app.audioOutputDevice });
    }
    const selectedValue = app.audioOutputDeviceEditing
      ? String(audioOutputDeviceSelectEl.value || '')
      : (app.audioOutputDevice || '');
    audioOutputDeviceSelectEl.innerHTML = '';
    options.forEach((entry) => {
      const optionEl = document.createElement('option');
      optionEl.value = entry.value;
      optionEl.textContent = entry.label || entry.value || t('status.defaultOutputDevice');
      audioOutputDeviceSelectEl.appendChild(optionEl);
    });
    audioOutputDeviceSelectEl.value = options.some((entry) => entry.value === selectedValue)
      ? selectedValue
      : '';
  }
  if (rampModeSelectEl) {
    rampModeSelectEl.value = ['off', 'frame', 'sample'].includes(app.rampMode) ? app.rampMode : 'sample';
  }
  if (audioSampleRateInputEl && !app.audioSampleRateEditing) {
    audioSampleRateInputEl.value = String(app.audioSampleRate || 0);
  }
  if (audioOutputSummaryEl) {
    const requestedValue = (app.audioOutputDevice || '').trim();
    const effectiveValue = (app.audioOutputDeviceEffective || requestedValue).trim();
    const deviceEntry = app.audioOutputDevices.find((entry) => entry.value === effectiveValue);
    const deviceText = effectiveValue
      ? (deviceEntry?.label || effectiveValue)
      : (app.oscSnapshotReady ? t('status.defaultOutputDevice') : '—');
    const rateText = app.audioSampleRate ? `${app.audioSampleRate} Hz` : '—';
    const fmtText = app.audioSampleFormat || '—';
    const summary = tf('audio.summary', {
      device: deviceText,
      rate: rateText,
      format: fmtText
    });
    audioOutputSummaryEl.textContent = app.audioError ? `${summary} • Error: ${app.audioError}` : summary;
  }
}

export function closeAudioSampleRateMenu() {
  if (!audioSampleRateMenuEl) return;
  audioSampleRateMenuEl.style.display = 'none';
}

export function openAudioSampleRateMenu() {
  if (!audioSampleRateMenuEl) return;
  app.audioSampleRateEditing = true;
  audioSampleRateMenuEl.innerHTML = '';
  AUDIO_SAMPLE_RATE_PRESETS.forEach((rate) => {
    const item = document.createElement('button');
    item.type = 'button';
    item.style.cssText = 'display:block;width:100%;text-align:left;background:none;border:none;color:#d9ecff;padding:0.25rem 0.35rem;border-radius:6px;cursor:pointer;font-size:12px';
    item.textContent = rate === 0 ? t('status.nativeRate') : `${rate} Hz`;
    item.addEventListener('click', () => {
      if (audioSampleRateInputEl) {
        audioSampleRateInputEl.value = String(rate);
      }
      applyAudioSampleRateNow();
      closeAudioSampleRateMenu();
    });
    item.addEventListener('mouseenter', () => {
      item.style.background = 'rgba(255,255,255,0.12)';
    });
    item.addEventListener('mouseleave', () => {
      item.style.background = 'transparent';
    });
    audioSampleRateMenuEl.appendChild(item);
  });
  audioSampleRateMenuEl.style.display = 'block';
}

export function updateAudioFormatDisplay() {
  dirty.audioFormat = true;
  scheduleUIFlush();
}

export function applyAudioSampleRateNow() {
  const requested = Math.max(0, Math.round(Number(audioSampleRateInputEl?.value) || 0));
  app.audioSampleRate = requested > 0 ? requested : null;
  updateAudioFormatDisplay();
  invoke('control_audio_sample_rate', { sampleRate: requested });
  app.audioSampleRateEditing = false;
  closeAudioSampleRateMenu();
}

export function applyAudioOutputDeviceNow() {
  const requested = String(audioOutputDeviceSelectEl?.value || '').trim();
  app.audioOutputDevice = requested || null;
  updateAudioFormatDisplay();
  invoke('control_audio_output_device', { outputDevice: requested });
  app.audioOutputDeviceEditing = false;
}

export function applyRampModeNow() {
  const requested = String(rampModeSelectEl?.value || 'sample').trim().toLowerCase();
  if (!['off', 'frame', 'sample'].includes(requested)) {
    return;
  }
  app.rampMode = requested;
  updateAudioFormatDisplay();
  invoke('control_ramp_mode', { value: requested });
}
