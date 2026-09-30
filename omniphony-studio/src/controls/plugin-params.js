/**
 * Schema-generated parameter controls, one builder for every plugin.
 *
 * Render backends, object generators and the phantom-extraction stage all
 * publish their parameters in one format (the renderer's `ParamSpec`:
 * `{ key, label, i18nKey?, unit?, kind: { type, … }, default, requires?, help? }`),
 * so one builder draws them all: a switch for a bool, a select for an enum, a
 * slider for a number, a file field for a backend's file.
 */

import { invoke } from '@tauri-apps/api/core';
import { t } from '../i18n.js';
import { makeHelpPanel, attachInlineHelp } from './inline-help.js';
import { openBackendFileEditor } from './script-editor.js';

// The renderer publishes its param schema (labels, help, enum option labels) in
// English over OSC. Localize it client-side — the `i18nKey` a parameter
// declares, else the Studio's own `backendParam.<key>` — falling back to the
// renderer-provided string so contributor plugins and unknown params still
// render. These re-resolve on locale change because the panels rebuild on it.
function trFallback(key, fallback) {
  const v = t(key);
  return (!v || v === key) ? fallback : v;
}
function localizeParamLabel(spec) {
  const fallback = trFallback(`backendParam.${spec.key}`, spec.label || spec.key);
  return spec.i18nKey ? trFallback(spec.i18nKey, fallback) : fallback;
}
function localizeParamHelp(spec) {
  return spec.help ? trFallback(`backendParamHelp.${spec.key}`, spec.help) : spec.help;
}
function localizeParamOption(paramKey, value, fallback) {
  return trFallback(`backendParamOption.${paramKey}.${value}`, fallback);
}

// A numeric value's readout: as many decimals as the step means (0.01 → 2,
// 0.5 → 1, 10 → 0, at most 3), then the unit the parameter declares.
function formatParamValue(spec, v, isInt) {
  const kind = spec.kind || {};
  const step = isInt ? 1 : Number(kind.step) || 0;
  const decimals = step > 0 ? Math.min(3, Math.max(0, -Math.floor(Math.log10(step)))) : 2;
  const text = Number(v).toFixed(decimals);
  return spec.unit ? `${text} ${spec.unit}` : text;
}

// Build one control field for a param spec, seeded with `current`. Returns a
// wrapper holding the control row (with a `_setValue` hook used to refresh it
// without a rebuild) and, when the spec has help, a collapsible help panel below
// it. Edits go to `send(key, value)`. Options:
// - `live`: send numbers while dragging, not only on release (a synthesizing
//   stage applies them in place; a backend rebuilds, so it waits for release);
// - `backend` / `rendererIsLocal`: the backend a file parameter belongs to — a
//   file lives on the renderer and moves over the backend file controls, so
//   only a backend can declare one.
function buildParamControl(spec, current, send, opts = {}) {
  const field = document.createElement('div');
  field.className = 'generated-param-field';
  const row = document.createElement('div');
  row.className = 'control-row generated-param-row';
  row.dataset.paramKey = spec.key;
  const label = document.createElement('label');
  label.textContent = localizeParamLabel(spec);
  let help = null;
  if (spec.help) {
    // The param name itself is the toggle for a help panel shown below the row.
    help = makeHelpPanel(localizeParamHelp(spec));
    attachInlineHelp(label, help);
  }
  row.appendChild(label);
  const kind = spec.kind || {};
  if (kind.type === 'bool') {
    const input = document.createElement('input');
    input.type = 'checkbox';
    input.checked = current === true;
    input.addEventListener('change', () => send(spec.key, input.checked));
    row.appendChild(input);
    row._setValue = (v) => { input.checked = v === true; };
  } else if (kind.type === 'enum') {
    const select = document.createElement('select');
    for (const opt of (kind.options || [])) {
      const o = document.createElement('option');
      o.value = String(opt.value);
      o.textContent = localizeParamOption(spec.key, opt.value, String(opt.label || opt.value));
      select.appendChild(o);
    }
    select.value = String(current);
    select.addEventListener('change', () => send(spec.key, select.value));
    row.appendChild(select);
    row._setValue = (v) => { select.value = String(v); };
  } else if (kind.type === 'path' && opts.backend) {
    const input = document.createElement('input');
    input.type = 'text';
    input.className = 'delay-input';
    input.style.minWidth = '12rem';
    input.placeholder = '/path/to/backend.lua';
    input.value = current == null ? '' : String(current);
    input.addEventListener('change', () => send(spec.key, input.value.trim()));
    row.appendChild(input);
    // Don't clobber the field while the user is typing in it.
    row._setValue = (v) => {
      if (document.activeElement !== input) input.value = v == null ? '' : String(v);
    };
  } else if (kind.type === 'file' && opts.backend) {
    // A renderer-owned file handle: a text field for the handle, a Browse button
    // (native dialog, shown only when the renderer is local) and, when editable,
    // an Edit button opening the local editor that loads/saves over the renderer.
    const exts = Array.isArray(kind.extensions) ? kind.extensions.map(String) : [];
    const wrap = document.createElement('div');
    wrap.style.display = 'flex';
    wrap.style.gap = '0.3rem';
    wrap.style.alignItems = 'center';
    wrap.style.flexWrap = 'wrap';

    const input = document.createElement('input');
    input.type = 'text';
    input.className = 'delay-input';
    input.style.minWidth = '10rem';
    input.placeholder = exts.length ? `name.${exts[0]}` : '/path/to/file';
    input.value = current == null ? '' : String(current);
    input.addEventListener('change', () => send(spec.key, input.value.trim()));
    wrap.appendChild(input);

    const browse = document.createElement('button');
    browse.type = 'button';
    browse.className = 'mini-btn';
    browse.textContent = t('backend.file.browse');
    browse.style.display = opts.rendererIsLocal ? '' : 'none';
    browse.addEventListener('click', async () => {
      const picked = await invoke('pick_backend_file_path', { extensions: exts }).catch(() => null);
      if (picked) {
        input.value = picked;
        send(spec.key, picked);
      }
    });
    wrap.appendChild(browse);

    if (kind.editable) {
      const edit = document.createElement('button');
      edit.type = 'button';
      edit.className = 'mini-btn';
      edit.textContent = t('backend.file.edit');
      edit.addEventListener('click', () => openBackendFileEditor({
        backend: opts.backend,
        key: spec.key,
        language: kind.language || null,
        extensions: exts,
        rendererIsLocal: opts.rendererIsLocal,
      }));
      wrap.appendChild(edit);
    }

    row.appendChild(wrap);
    row._setValue = (v) => {
      if (document.activeElement !== input) input.value = v == null ? '' : String(v);
    };
  } else if (kind.type === 'path' || kind.type === 'file') {
    return null;
  } else {
    const isInt = kind.type === 'int';
    const input = document.createElement('input');
    input.type = 'range';
    input.min = String(kind.min ?? 0);
    input.max = String(kind.max ?? 1);
    input.step = String(isInt ? 1 : (kind.step ?? 0.01));
    input.value = String(current);
    const valEl = document.createElement('span');
    valEl.className = 'val';
    valEl.textContent = formatParamValue(spec, current, isInt);
    // Fixed-width, right-aligned readout so the value's changing digit count
    // does not resize its grid column and shift the slider as you drag.
    valEl.style.display = 'inline-block';
    valEl.style.minWidth = '3.5em';
    valEl.style.textAlign = 'right';
    const parse = (v) => (isInt ? Math.round(Number(v)) : Number(v));
    input.addEventListener('input', () => {
      valEl.textContent = formatParamValue(spec, input.value, isInt);
      if (opts.live) send(spec.key, parse(input.value));
    });
    if (!opts.live) {
      input.addEventListener('change', () => send(spec.key, parse(input.value)));
    }
    row.appendChild(input);
    row.appendChild(valEl);
    row._setValue = (v) => {
      input.value = String(v);
      valEl.textContent = formatParamValue(spec, v, isInt);
    };
  }
  field.appendChild(row);
  if (help) field.appendChild(help);
  return field;
}

// Fill `container` with one generated control per declared parameter, seeded
// from `values` (the plugin's stored `{ key: value }`) or the declared
// default. Rebuilt only when `buildKey` changes (another plugin, another
// locale); otherwise the values are refreshed in place, leaving a control
// that has the focus alone. `gate(requires)` returns the note to show,
// dimmed, while a parameter's requirement is not met — the control stays
// editable, the configuration being kept for when it is.
export function renderParamForm(container, params, values, send, opts = {}) {
  const valueFor = (spec) => (values && spec.key in values ? values[spec.key] : spec.default);
  if (container.dataset.buildKey !== String(opts.buildKey)) {
    container.replaceChildren();
    container.dataset.buildKey = String(opts.buildKey);
    for (const spec of params) {
      const field = buildParamControl(spec, valueFor(spec), send, opts);
      if (field) container.appendChild(field);
    }
  } else {
    for (const row of container.querySelectorAll('.generated-param-row')) {
      const spec = params.find((p) => p.key === row.dataset.paramKey);
      if (spec && typeof row._setValue === 'function'
        && document.activeElement !== row.querySelector('input, select')) {
        row._setValue(valueFor(spec));
      }
    }
  }
  const gate = opts.gate || (() => null);
  for (const row of container.querySelectorAll('.generated-param-row')) {
    const spec = params.find((p) => p.key === row.dataset.paramKey);
    const note = spec && spec.requires ? gate(spec.requires) : null;
    row.style.opacity = note ? '0.7' : '';
    if (note) row.title = note;
    else row.removeAttribute('title');
  }
}
