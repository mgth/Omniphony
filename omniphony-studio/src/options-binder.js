// Two-way binder for the declared live options (registry RFC phase 2,
// docs/live-options-registry.md).
//
// A control declares its option in markup and this module wires BOTH
// directions generically — no per-option apply function, listener block or
// Tauri command:
//
//   toggle-btn pair:  <button data-option="surround_placement" data-option-value="side">
//   switch (bool):    <input type="checkbox" data-option="synthetic_objects_enabled">
//   enum select:      <select data-option="phantom_extract_mode">…</select>
//   select:           <select data-option="object_generator_id" data-option-empty="none">
//
// Sending: the generic `control_option` Tauri command → OSC
// `/omniphony/control/option [key, value]`; the renderer validates the value
// against its registry and drops bad input. Reflection: reflectBoundOptions()
// applies `app.options` (canonical snake_case keys from the renderer
// snapshot; the published schema provides pre-snapshot defaults) onto every
// bound control. While neither has arrived, the control keeps its baked HTML
// default rather than flashing an empty state.

import { invoke } from '@tauri-apps/api/core';
import { app, dirty, getLiveOption } from './state.js';
import { scheduleUIFlush } from './flush.js';

// Option-specific UI side effects on a local set. Everything the audio-format
// render repaints (row visibility, summaries, param sliders) is covered by the
// generic flush below — only state the render can't derive belongs here.
const AFTER_SET = {
  // Switching generators drops the previous one's overrides (the renderer does
  // the same) so the new generator shows its declared defaults.
  object_generator_id: () => {
    app.objectGeneratorParams = {};
  },
};

function setOption(key, value) {
  // Optimistic local write: the renderer echoes the canonical value on the
  // next snapshot, but the UI must not lag the click.
  app.options[key] = value;
  if (AFTER_SET[key]) AFTER_SET[key]();
  invoke('control_option', { key, value }).catch((err) => {
    // An optimistically-reflected control must never hide a dead send chain.
    console.error(`control_option ${key} failed:`, err);
  });
  reflectBoundOptions();
  dirty.audioFormat = true;
  scheduleUIFlush();
}

// Wire every `[data-option]` control found in the DOM. Idempotent (re-running
// skips already-bound elements), so late-mounted panels can call it again.
export function bindOptionControls() {
  for (const el of document.querySelectorAll('[data-option]')) {
    const key = el.dataset.option;
    if (!key || el.dataset.optionBound) continue;
    el.dataset.optionBound = '1';
    if (el.tagName === 'BUTTON') {
      el.addEventListener('click', () => setOption(key, el.dataset.optionValue));
    } else if (el.tagName === 'SELECT') {
      el.addEventListener('change', () => setOption(key, el.value));
    } else if (el.type === 'checkbox') {
      el.addEventListener('change', () => {
        const enumOn = el.dataset.optionOn;
        setOption(
          key,
          enumOn !== undefined ? (el.checked ? enumOn : el.dataset.optionOff) : el.checked
        );
      });
    }
  }
  reflectBoundOptions();
}

// Reflect the current option values onto every bound control. Unknown value
// (no snapshot AND no schema yet) leaves the control's baked HTML default.
export function reflectBoundOptions() {
  for (const el of document.querySelectorAll('[data-option]')) {
    const v = getLiveOption(el.dataset.option);
    if (v === undefined) continue;
    if (el.tagName === 'BUTTON') {
      el.classList.toggle('active', String(v) === el.dataset.optionValue);
    } else if (el.tagName === 'SELECT') {
      if (document.activeElement === el) continue;
      const empty = el.dataset.optionEmpty;
      el.value = (v === '' || v === null) && empty !== undefined ? empty : String(v);
    } else if (el.type === 'checkbox') {
      const enumOn = el.dataset.optionOn;
      el.checked = enumOn !== undefined ? String(v) === enumOn : !!v;
    }
  }
}
