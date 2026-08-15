/**
 * VBAP status, mode, cartesian, polar, and position interpolation controls.
 *
 * Extracted from app.js (lines 3711-3852).
 */

import { invoke } from '@tauri-apps/api/core';
import { app, dirty } from '../state.js';
import { t, tf, i18nState } from '../i18n.js';
import { formatNumber } from '../coordinates.js';
import { scheduleUIFlush } from '../flush.js';
import { inRendererPanel } from '../ui/panel-roots.js';
import { renderHybridCurve } from './hybrid-curve.js';
import { makeHelpPanel, attachInlineHelp, closeInlineHelp } from './inline-help.js';
import { openBackendFileEditor } from './script-editor.js';

// Whether the renderer shares this machine's filesystem (its OSC host is
// loopback). The native Browse dialog returns a path in *this* machine's
// namespace, which is only meaningful to the renderer when they are the same
// host — so Browse is hidden when the renderer is remote. Editing still works
// remotely through the editor's renderer-side content channel. Cached here and
// refreshed from the Tauri host; defaults to local (the common 127.0.0.1 case).
let rendererIsLocal = true;
export function refreshRendererLocality() {
  return invoke('renderer_is_local')
    .then((local) => { rendererIsLocal = !!local; })
    .catch(() => {});
}
refreshRendererLocality();

function getVbapStatusEl() { return inRendererPanel('vbapStatus'); }
function getRenderBackendSelectEl() { return inRendererPanel('renderBackendSelect'); }
function getRestoreBackendBtnEl() { return inRendererPanel('restoreBackendBtn'); }
function getRenderBackendEffectiveEl() { return inRendererPanel('renderBackendEffective'); }
function getRenderEvaluationModeSelectEl() { return inRendererPanel('renderEvaluationModeSelect'); }
function getRenderEvaluationModeEffectiveEl() { return inRendererPanel('renderEvaluationModeEffective'); }
function getRendererSummaryEl() { return inRendererPanel('rendererSummary'); }
function getBackendParametersSectionEl() { return inRendererPanel('backendParametersSection'); }
function getBackendSpecificParamsSectionEl() { return inRendererPanel('backendSpecificParamsSection'); }
function getEvaluationSectionEl() { return inRendererPanel('evaluationSection'); }
function getHybridSectionEl() { return inRendererPanel('hybridSection'); }
function getHybridExternalBackendSelectEl() { return inRendererPanel('hybridExternalBackendSelect'); }
function getHybridInternalBackendSelectEl() { return inRendererPanel('hybridInternalBackendSelect'); }
function getHybridParamTabsEl() { return inRendererPanel('hybridParamTabs'); }
function getHybridConfigPanelEl() { return inRendererPanel('hybridConfigPanel'); }
function getVbapCartXSizeInputEl() { return inRendererPanel('vbapCartXSizeInput'); }
function getVbapCartYSizeInputEl() { return inRendererPanel('vbapCartYSizeInput'); }
function getVbapCartZSizeInputEl() { return inRendererPanel('vbapCartZSizeInput'); }
function getVbapCartZNegSizeInputEl() { return inRendererPanel('vbapCartZNegSizeInput'); }
function getVbapCartXStepInfoEl() { return inRendererPanel('vbapCartXStepInfo'); }
function getVbapCartYStepInfoEl() { return inRendererPanel('vbapCartYStepInfo'); }
function getVbapCartZStepInfoEl() { return inRendererPanel('vbapCartZStepInfo'); }
function getVbapCartZNegStepInfoEl() { return inRendererPanel('vbapCartZNegStepInfo'); }
function getVbapPolarAzimuthResolutionInputEl() { return inRendererPanel('vbapPolarAzimuthResolutionInput'); }
function getVbapPolarElevationResolutionInputEl() { return inRendererPanel('vbapPolarElevationResolutionInput'); }
function getVbapPolarDistanceResInputEl() { return inRendererPanel('vbapPolarDistanceResInput'); }
function getVbapPolarDistanceMaxInputEl() { return inRendererPanel('vbapPolarDistanceMaxInput'); }
function getVbapElevationRangeInfoEl() { return inRendererPanel('vbapElevationRangeInfo'); }
function getVbapAzimuthRangeInfoEl() { return inRendererPanel('vbapAzimuthRangeInfo'); }
function getVbapPolarAzStepInfoEl() { return inRendererPanel('vbapPolarAzStepInfo'); }
function getVbapPolarElStepInfoEl() { return inRendererPanel('vbapPolarElStepInfo'); }
function getVbapPolarDistStepInfoEl() { return inRendererPanel('vbapPolarDistStepInfo'); }
function getVbapPositionInterpolationToggleEl() { return inRendererPanel('vbapPositionInterpolationToggleEl'); }

// These are called from renderVbapCartesian but defined elsewhere in app.js.
// They must be provided via the callback registry or imported separately.
// For now we reference them as imported stubs that the wiring code will supply.
import { flushCallbacks } from '../flush.js';

function updateVbapCartesianFaceGrid() {
  if (typeof flushCallbacks.updateVbapCartesianFaceGrid === 'function') {
    flushCallbacks.updateVbapCartesianFaceGrid();
  }
}

function renderVbapCartesianGridToggle() {
  if (typeof flushCallbacks.renderVbapCartesianGridToggle === 'function') {
    flushCallbacks.renderVbapCartesianGridToggle();
  }
}

function backendCapabilities() {
  return app.renderBackendState.capabilities || null;
}


function backendLabel(backend) {
  if (backend === (app.renderBackendState.effective || '')) {
    return app.renderBackendState.effectiveLabel || backend || '—';
  }
  if (backend === 'vbap') return 'VBAP';
  if (backend === 'barycenter') return 'Barycenter';
  if (backend === 'experimental_distance') return 'Distance';
  if (backend === 'hybrid') return 'Hybrid';
  return backend || '—';
}

function applyRendererBackendVisibility(backend) {
  const backendParametersSectionEl = getBackendParametersSectionEl();
  const evaluationSectionEl = getEvaluationSectionEl();
  const backendSpecificParamsSectionEl = getBackendSpecificParamsSectionEl();
  const hybridSectionEl = getHybridSectionEl();
  const hybridConfigPanelEl = getHybridConfigPanelEl();
  const capabilities = backendCapabilities();
  const isHybrid = backend === 'hybrid';

  // The base backend whose individual parameters are currently displayed.
  // In hybrid mode that's the active inner-backend tab; otherwise the active
  // backend itself. VBAP-specific sections (spread, …) are gated by
  // capabilities for the active backend, but in hybrid mode VBAP is an inner
  // model that consumes those same shared params, so we show them when the VBAP
  // tab is active. (Distance model is now a standalone top-level block.)
  let paramBackend = backend;
  let hybridTab = null;
  if (isHybrid) {
    hybridTab = resolveHybridParamTab();
    // The 'hybrid' tab shows the mix config (selects + curve); the other tabs
    // each show one inner backend's parameters.
    paramBackend = hybridTab === 'hybrid' ? null : hybridTab;
  }

  // Backends without a hand-written section (VBAP, barycenter, distance,
  // contributor backends) render their controls from the schema. `paramBackend`
  // is null on the hybrid mix-config tab, where only the config panel shows.
  const showsGeneric =
    !!paramBackend
    && !BESPOKE_BACKENDS.includes(paramBackend)
    && backendSchemaParams(paramBackend).length > 0;
  const showsHybrid = isHybrid;
  if (backendParametersSectionEl) {
    backendParametersSectionEl.style.display = '';
  }
  if (evaluationSectionEl) {
    evaluationSectionEl.style.display = '';
  }
  if (backendSpecificParamsSectionEl) {
    // Keep `flex` (not '') so the inline `flex-direction:column` + `order:-1` on
    // the hybrid section stay in effect — the hybrid tab bar must render above
    // all tab content, including the inner-backend param sections that follow it
    // in the DOM.
    backendSpecificParamsSectionEl.style.display =
      showsGeneric || showsHybrid ? 'flex' : 'none';
  }
  if (hybridSectionEl) {
    hybridSectionEl.style.display = showsHybrid ? '' : 'none';
  }
  if (hybridConfigPanelEl) {
    hybridConfigPanelEl.style.display = isHybrid && hybridTab === 'hybrid' ? '' : 'none';
  }
  // Schema-generated controls for the active (possibly inner) backend.
  renderGenericBackendParams(paramBackend);
}

/** Distinct inner backends currently selected for the hybrid backend. */
function hybridInnerBackends() {
  const state = app.renderBackendState.hybrid || {};
  const external = typeof state.externalBackend === 'string' ? state.externalBackend : 'vbap';
  const internal = typeof state.internalBackend === 'string' ? state.internalBackend : 'barycenter';
  return external === internal ? [external] : [external, internal];
}

/** Tab list for the hybrid backend: the mix config tab plus each inner backend. */
function hybridTabList() {
  return ['hybrid', ...hybridInnerBackends()];
}

/** Active hybrid tab ('hybrid' for the mix config, otherwise an inner backend). */
function resolveHybridParamTab() {
  const tabs = hybridTabList();
  let active = app.hybridParamTab;
  if (!tabs.includes(active)) {
    active = tabs[0];
  }
  app.hybridParamTab = active;
  return active;
}

export function selectHybridParamTab(backend) {
  app.hybridParamTab = backend;
  renderRenderBackend();
}

function applyEvaluationModeVisibility(mode) {
  const capabilities = backendCapabilities();
  const showsCartesian =
    mode === 'precomputed_cartesian' && capabilities?.supportsPrecomputedCartesian === true;
  const showsPolar =
    mode === 'precomputed_polar' && capabilities?.supportsPrecomputedPolar === true;
  const showsInterpolation = showsCartesian || showsPolar;
  const renderEvaluationCartesianBlockEl = inRendererPanel('renderEvaluationCartesianBlock');
  const renderEvaluationPolarBlockEl = inRendererPanel('renderEvaluationPolarBlock');
  const renderEvaluationPositionInterpolationRowEl = inRendererPanel('renderEvaluationPositionInterpolationRow');
  if (renderEvaluationCartesianBlockEl) {
    renderEvaluationCartesianBlockEl.hidden = !showsCartesian;
    renderEvaluationCartesianBlockEl.style.setProperty('display', showsCartesian ? '' : 'none', 'important');
  }
  if (renderEvaluationPolarBlockEl) {
    renderEvaluationPolarBlockEl.hidden = !showsPolar;
    renderEvaluationPolarBlockEl.style.setProperty('display', showsPolar ? '' : 'none', 'important');
  }
  if (renderEvaluationPositionInterpolationRowEl) {
    renderEvaluationPositionInterpolationRowEl.hidden = !showsInterpolation;
    renderEvaluationPositionInterpolationRowEl.style.setProperty('display', showsInterpolation ? '' : 'none', 'important');
  }
}

function formatEvaluationModeLabel(mode) {
  switch (mode) {
    case 'auto': return t('common.auto');
    case 'realtime': return t('eval.mode.realtime');
    case 'precomputed_polar': return t('common.polarShort');
    case 'precomputed_cartesian': return t('common.cartesianShort');
    default: return '—';
  }
}

// How long a control waits for the engine to acknowledge a recompute before
// declaring it unreachable. Generous: a large cartesian table on a slow machine
// can take seconds to even report that it started.
const RECOMPUTE_ACK_TIMEOUT_MS = 8000;

let recomputeAckWatchdog = null;

/**
 * Mark a recompute as pending after sending a control to the engine.
 *
 * The "computing" state is optimistic — set locally the moment we send, and
 * only the engine's own `vbap:recomputing` broadcast clears it. So a control
 * the engine does not understand (an older build, or a renderer left running by
 * another environment that happens to hold the port) pins the status on
 * "computing" forever, with no error and nothing to retry. Arming a watchdog on
 * every optimistic transition turns that silence into a message.
 *
 * Use this instead of assigning `app.vbapRecomputing` directly.
 */
export function markRecomputePending() {
  app.vbapRecomputing = true;
  app.recomputeError = null;
  clearRecomputeAckWatchdog();
  recomputeAckWatchdog = setTimeout(() => {
    recomputeAckWatchdog = null;
    // A late acknowledgement may have landed while the timer sat queued.
    if (app.vbapRecomputing !== true) return;
    app.vbapRecomputing = false;
    app.recomputeError = t('vbap.status.noAck');
    renderVbapStatus();
  }, RECOMPUTE_ACK_TIMEOUT_MS);
  renderVbapStatus();
}

/**
 * Disarm the watchdog: the engine answered, so it is alive and understood us.
 *
 * Any `vbap:recomputing` broadcast counts, including the one announcing that
 * the recompute started. A recompute that starts and never finishes is a
 * different failure, and leaving that one on "computing" is at least truthful.
 */
export function clearRecomputeAckWatchdog() {
  if (recomputeAckWatchdog === null) return;
  clearTimeout(recomputeAckWatchdog);
  recomputeAckWatchdog = null;
}

export function renderVbapStatus() {
  const vbapStatusEl = getVbapStatusEl();
  if (!vbapStatusEl) return;
  vbapStatusEl.classList.remove('computing', 'ready', 'error');
  vbapStatusEl.removeAttribute('title');
  if (typeof app.recomputeError === 'string' && app.recomputeError.length > 0) {
    vbapStatusEl.textContent = app.recomputeError;
    vbapStatusEl.classList.add('error');
    vbapStatusEl.title = app.recomputeError;
    vbapStatusEl.style.color = '#ff7676';
  } else if (app.vbapRecomputing === true) {
    vbapStatusEl.textContent = t('vbap.status.computing');
    vbapStatusEl.classList.add('computing');
    vbapStatusEl.style.color = '';
  } else if (app.vbapRecomputing === false) {
    vbapStatusEl.textContent = t('vbap.status.ready');
    vbapStatusEl.classList.add('ready');
    vbapStatusEl.style.color = '';
  } else {
    vbapStatusEl.textContent = t('vbap.status.idle');
    vbapStatusEl.style.color = '';
  }
}

export function renderEvaluationMode() {
  const renderEvaluationModeSelectEl = getRenderEvaluationModeSelectEl();
  const renderEvaluationModeEffectiveEl = getRenderEvaluationModeEffectiveEl();
  const rendererSummaryEl = getRendererSummaryEl();
  const backend = app.renderBackendState.effective || app.renderBackendState.selection || 'vbap';
  const allowedModes = Array.isArray(app.renderBackendState.allowedEvaluationModes)
    && app.renderBackendState.allowedEvaluationModes.length > 0
    ? app.renderBackendState.allowedEvaluationModes
    : ['auto', 'realtime', 'precomputed_polar', 'precomputed_cartesian'];
  const selection = typeof app.evaluationModeState.selection === 'string' ? app.evaluationModeState.selection : null;
  const effectiveMode = typeof app.evaluationModeState.effective === 'string' ? app.evaluationModeState.effective : null;
  const visibleModes = [...allowedModes];
  if (selection && !visibleModes.includes(selection)) {
    visibleModes.push(selection);
  }
  if (effectiveMode && !visibleModes.includes(effectiveMode)) {
    visibleModes.push(effectiveMode);
  }
  const nextValue = visibleModes.includes(selection) ? selection : visibleModes[0];
  if (renderEvaluationModeSelectEl) {
    const currentOptions = Array.from(renderEvaluationModeSelectEl.options).map((option) => option.value);
    if (
      currentOptions.length !== visibleModes.length
      || currentOptions.some((value, index) => value !== visibleModes[index])
    ) {
      renderEvaluationModeSelectEl.innerHTML = '';
      visibleModes.forEach((mode) => {
        const option = document.createElement('option');
        option.value = mode;
        option.textContent = formatEvaluationModeLabel(mode);
        renderEvaluationModeSelectEl.append(option);
      });
    }
    renderEvaluationModeSelectEl.value = nextValue;
    renderEvaluationModeSelectEl.disabled = allowedModes.length === 0;
  }
  if (renderEvaluationModeEffectiveEl) {
    renderEvaluationModeEffectiveEl.textContent = formatEvaluationModeLabel(effectiveMode);
  }
  if (rendererSummaryEl) {
    const mode = effectiveMode || selection;
    const modeText = formatEvaluationModeLabel(mode);
    const backendText = backendLabel(backend);
    rendererSummaryEl.textContent = `${backendText} / ${tf('renderer.summary', { mode: modeText })}`;
  }
  const visibleMode = nextValue === 'auto'
    ? (effectiveMode || nextValue)
    : nextValue;
  applyEvaluationModeVisibility(visibleMode);
  renderVbapPositionInterpolation();
  renderObjectSizeIntervals();
}

// Sync the object-size interval count input, and hide it for backends that do
// not consume object size (the precomputed size tables would have no effect).
function renderObjectSizeIntervals() {
  const rowEl = inRendererPanel('objectSizeIntervalsRow');
  const inputEl = inRendererPanel('objectSizeIntervalsInput');
  if (inputEl && document.activeElement !== inputEl) {
    inputEl.value = String(app.objectSizeIntervals ?? 0);
  }
  if (rowEl) {
    const caps = backendCapabilities();
    // Show unless the active backend explicitly reports no event-size support.
    const supportsSize = !caps
      || (caps.supports_event_size !== false && caps.supportsEventSize !== false);
    rowEl.style.display = supportsSize ? '' : 'none';
  }
}

export function updateEvaluationMode() {
  dirty.vbapMode = true;
  scheduleUIFlush();
}

// Populate the backend dropdown from the engine-published list of selectable
// backends (built-in + contributor-registered). Falls back to the static HTML
// options when the engine doesn't send a list (older builds).
function syncBackendOptions(selectEl, excludeIds) {
  const available = app.renderBackendState && Array.isArray(app.renderBackendState.availableBackends)
    ? app.renderBackendState.availableBackends
    : null;
  if (!selectEl || !available || available.length === 0) return;
  const exclude = excludeIds instanceof Set ? excludeIds : new Set(excludeIds || []);
  const desired = available
    .filter((b) => !exclude.has(String(b.id)))
    .map((b) => ({
      value: String(b.id),
      label: String(b.label || b.id),
    }));
  if (desired.length === 0) return;
  const current = Array.from(selectEl.options).map((o) => `${o.value}=${o.textContent}`).join('|');
  const next = desired.map((d) => `${d.value}=${d.label}`).join('|');
  if (current === next) return;
  selectEl.replaceChildren();
  for (const d of desired) {
    const opt = document.createElement('option');
    opt.value = d.value;
    opt.textContent = d.label;
    selectEl.appendChild(opt);
  }
}

// Backends that keep hand-written control sections instead of schema-generated
// ones. Only Hybrid remains bespoke (the mix-config tab). VBAP's spread tuning
// is now published as generic backend params and rendered from the schema like
// barycenter/distance/contributor backends; its evaluation grids, recompute
// status and 3-D gizmo stay as separate (non-param) affordances.
const BESPOKE_BACKENDS = ['hybrid'];

// Param schema (`[{ key, label, kind, default, help? }]`) published for a
// backend, or `[]` if it has none / is unknown.
function backendSchemaParams(backendId) {
  const available = app.renderBackendState && Array.isArray(app.renderBackendState.availableBackends)
    ? app.renderBackendState.availableBackends
    : [];
  const entry = available.find((b) => String(b.id) === String(backendId));
  return entry && Array.isArray(entry.params) ? entry.params : [];
}

// Set a param on a specific backend. `backend` is passed so an inner hybrid
// backend (not the active selection) is addressed correctly; omitted, the engine
// applies it to the currently selected backend.
function sendBackendParam(key, value, backend) {
  invoke('control_backend_param', backend ? { key, value, backend } : { key, value });
}

// The renderer publishes its param schema (labels, help, enum option labels) in
// English over OSC. Localize it client-side by stable key, falling back to the
// renderer-provided string so contributor backends and unknown params still
// render. These re-resolve on locale change because app.js re-runs
// renderRenderBackend() from its onLocaleChange handler.
function trFallback(key, fallback) {
  const v = t(key);
  return (!v || v === key) ? fallback : v;
}
function localizeParamLabel(spec) {
  return trFallback(`backendParam.${spec.key}`, spec.label || spec.key);
}
function localizeParamHelp(spec) {
  return spec.help ? trFallback(`backendParamHelp.${spec.key}`, spec.help) : spec.help;
}
function localizeParamOption(paramKey, value, fallback) {
  return trFallback(`backendParamOption.${paramKey}.${value}`, fallback);
}

// Build one control field for a param spec, seeded with `current`. Returns a
// wrapper holding the control row (with a `_setValue` hook used to refresh it
// without a rebuild) and, when the spec has help, a collapsible help panel below
// it. Edits are sent to `backend`.
function buildParamControl(spec, current, backend) {
  const field = document.createElement('div');
  field.className = 'generated-param-field';
  const row = document.createElement('div');
  row.className = 'control-row generated-param-row';
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
    input.addEventListener('change', () => sendBackendParam(spec.key, input.checked, backend));
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
    select.addEventListener('change', () => sendBackendParam(spec.key, select.value, backend));
    row.appendChild(select);
    row._setValue = (v) => { select.value = String(v); };
  } else if (kind.type === 'path') {
    const input = document.createElement('input');
    input.type = 'text';
    input.className = 'delay-input';
    input.style.minWidth = '12rem';
    input.placeholder = '/path/to/backend.lua';
    input.value = current == null ? '' : String(current);
    input.addEventListener('change', () => sendBackendParam(spec.key, input.value.trim(), backend));
    row.appendChild(input);
    // Don't clobber the field while the user is typing in it.
    row._setValue = (v) => {
      if (document.activeElement !== input) input.value = v == null ? '' : String(v);
    };
  } else if (kind.type === 'file') {
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
    input.addEventListener('change', () => sendBackendParam(spec.key, input.value.trim(), backend));
    wrap.appendChild(input);

    const browse = document.createElement('button');
    browse.type = 'button';
    browse.className = 'mini-btn';
    browse.textContent = t('backend.file.browse');
    browse.style.display = rendererIsLocal ? '' : 'none';
    browse.addEventListener('click', async () => {
      const picked = await invoke('pick_backend_file_path', { extensions: exts }).catch(() => null);
      if (picked) {
        input.value = picked;
        sendBackendParam(spec.key, picked, backend);
      }
    });
    wrap.appendChild(browse);

    if (kind.editable) {
      const edit = document.createElement('button');
      edit.type = 'button';
      edit.className = 'mini-btn';
      edit.textContent = t('backend.file.edit');
      edit.addEventListener('click', () => openBackendFileEditor({
        backend,
        key: spec.key,
        language: kind.language || null,
        extensions: exts,
        rendererIsLocal,
      }));
      wrap.appendChild(edit);
    }

    row.appendChild(wrap);
    row._setValue = (v) => {
      if (document.activeElement !== input) input.value = v == null ? '' : String(v);
    };
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
    valEl.textContent = String(current);
    // Fixed-width, right-aligned readout so the value's changing digit count
    // does not resize its grid column and shift the slider as you drag.
    valEl.style.display = 'inline-block';
    valEl.style.minWidth = '3.5em';
    valEl.style.textAlign = 'right';
    const parse = (v) => (isInt ? Math.round(Number(v)) : Number(v));
    input.addEventListener('input', () => { valEl.textContent = input.value; });
    input.addEventListener('change', () => sendBackendParam(spec.key, parse(input.value), backend));
    row.appendChild(input);
    row.appendChild(valEl);
    row._setValue = (v) => { input.value = String(v); valEl.textContent = String(v); };
  }
  field.appendChild(row);
  if (help) field.appendChild(help);
  return field;
}

// Generate controls for `targetBackend` from its published param schema. Used
// for every backend without a hand-written section — the built-in barycenter and
// distance backends as well as contributor backends — both when selected on their
// own and as the active hybrid inner-backend tab. Values are read per backend id
// (`backendParamValuesById`), and edits are addressed to `targetBackend`, so an
// inner hybrid backend is tuned independently of the active selection. The
// container lives in `backendSpecificParamsSection` so it sits below the hybrid
// tab bar when one is shown.
function renderGenericBackendParams(targetBackend) {
  const host = getBackendSpecificParamsSectionEl();
  if (!host) return;
  let container = document.getElementById('backendGenericParamsSection');
  if (!container) {
    container = document.createElement('div');
    container.id = 'backendGenericParamsSection';
    // Indented sub-block with smaller text, mirroring the Distance Diffuse
    // body (`renderer-subpanel-body`): a lighter inset panel so the per-backend
    // params read as a child of the Backend section rather than top-level rows.
    container.className = 'control-section renderer-subpanel-body';
    container.style.marginTop = '0.25rem';
    container.style.marginLeft = '1rem';
    container.style.padding = '0.3rem 0.4rem';
    container.style.background = 'rgba(255,255,255,0.03)';
    container.style.borderRadius = '6px';
    container.style.display = 'grid';
    container.style.gap = '0.18rem';
    container.style.fontSize = '11px';
    host.appendChild(container);
  }

  const schema = targetBackend && !BESPOKE_BACKENDS.includes(targetBackend)
    ? backendSchemaParams(targetBackend)
    : [];

  if (schema.length === 0) {
    closeInlineHelp();
    container.style.display = 'none';
    container.dataset.backend = '';
    return;
  }
  container.style.display = 'grid';

  const byId = (app.renderBackendState && app.renderBackendState.backendParamValuesById) || {};
  const values = byId[targetBackend] || {};
  const valueFor = (spec) => (spec.key in values ? values[spec.key] : spec.default);

  // Rebuild controls only when the target backend changed; otherwise refresh
  // values so an external change (OSC) is reflected without dropping focus.
  // Rebuild on a backend change or a locale change (param labels/help are
  // localized at build time); otherwise just refresh values so an external
  // change (OSC) is reflected without dropping focus.
  if (container.dataset.backend !== String(targetBackend) || container.dataset.locale !== i18nState.locale) {
    // Rebuilding detaches any open help panel; drop the stale reference.
    closeInlineHelp();
    container.replaceChildren();
    container.dataset.backend = String(targetBackend);
    container.dataset.locale = i18nState.locale;
    for (const spec of schema) {
      container.appendChild(buildParamControl(spec, valueFor(spec), targetBackend));
    }
  } else {
    const rows = container.querySelectorAll('.generated-param-row');
    schema.forEach((spec, i) => {
      const row = rows[i];
      if (row && typeof row._setValue === 'function' && document.activeElement !== row.querySelector('input, select')) {
        row._setValue(valueFor(spec));
      }
    });
  }
}

export function renderRenderBackend() {
  const renderBackendSelectEl = getRenderBackendSelectEl();
  const restoreBackendBtnEl = getRestoreBackendBtnEl();
  const renderBackendEffectiveEl = getRenderBackendEffectiveEl();
  const selection = typeof app.renderBackendState.selection === 'string' ? app.renderBackendState.selection : 'vbap';
  const effective = typeof app.renderBackendState.effective === 'string' ? app.renderBackendState.effective : null;
  // Normally show the active (effective) backend's sections. The script backend
  // is the exception: until a valid .lua is set its build fails, so `effective`
  // lags on the previous backend — follow the selection instead so its config
  // (the file path field) is reachable to fix the error.
  const visibleBackend = selection === 'script' ? selection : (effective || selection);
  const frozen = app.renderBackendState.frozenSpeakers === true;
  if (renderBackendSelectEl) {
    syncBackendOptions(renderBackendSelectEl);
    renderBackendSelectEl.value = selection;
    renderBackendSelectEl.disabled = frozen;
  }
  if (restoreBackendBtnEl) {
    restoreBackendBtnEl.style.display = 'none';
    restoreBackendBtnEl.disabled = true;
  }
  if (renderBackendEffectiveEl) {
    renderBackendEffectiveEl.textContent = backendLabel(effective);
  }
  const layoutSelectEl = document.getElementById('layoutSelect');
  const importLayoutBtnEl = document.getElementById('importLayoutBtn');
  const exportLayoutBtnEl = document.getElementById('exportLayoutBtn');
  const layoutFrozen = frozen;
  if (layoutSelectEl) {
    layoutSelectEl.disabled = layoutFrozen || layoutSelectEl.options.length === 0;
  }
  if (importLayoutBtnEl) importLayoutBtnEl.disabled = layoutFrozen;
  if (exportLayoutBtnEl) exportLayoutBtnEl.disabled = layoutFrozen;
  applyRendererBackendVisibility(visibleBackend);
  renderHybridOptions();
  renderEvaluationMode();
}

export function updateRenderBackend() {
  dirty.renderBackend = true;
  dirty.hybrid = true;
  dirty.vbapMode = true;
  scheduleUIFlush();
}

export function renderHybridOptions() {
  const hybridExternalBackendSelectEl = getHybridExternalBackendSelectEl();
  const hybridInternalBackendSelectEl = getHybridInternalBackendSelectEl();
  const state = app.renderBackendState.hybrid || {};
  // Inner models can be any registered backend except a nested hybrid.
  const innerExclude = new Set(['hybrid']);
  syncBackendOptions(hybridExternalBackendSelectEl, innerExclude);
  syncBackendOptions(hybridInternalBackendSelectEl, innerExclude);
  if (hybridExternalBackendSelectEl && typeof state.externalBackend === 'string') {
    hybridExternalBackendSelectEl.value = state.externalBackend;
  }
  if (hybridInternalBackendSelectEl && typeof state.internalBackend === 'string') {
    hybridInternalBackendSelectEl.value = state.internalBackend;
  }
  const hybridMetricSelectEl = inRendererPanel('hybridMetricSelect');
  if (hybridMetricSelectEl) {
    hybridMetricSelectEl.value = ['spherical', 'chebyshev'].includes(state.metric)
      ? state.metric
      : 'chebyshev';
  }
  const smoothing = Math.min(1, Math.max(0, Number(state.curveSmoothing) || 0));
  const hybridSmoothingSliderEl = inRendererPanel('hybridCurveSmoothingSlider');
  if (hybridSmoothingSliderEl) {
    hybridSmoothingSliderEl.value = String(smoothing);
  }
  const hybridSmoothingValEl = inRendererPanel('hybridCurveSmoothingVal');
  if (hybridSmoothingValEl) {
    hybridSmoothingValEl.textContent = formatNumber(smoothing, 2);
  }
  renderHybridParamTabs();
  renderHybridCurve();
}

function renderHybridParamTabs() {
  const tabsEl = getHybridParamTabsEl();
  if (!tabsEl) return;
  if (app.renderBackendState.selection !== 'hybrid') {
    tabsEl.style.display = 'none';
    tabsEl.innerHTML = '';
    return;
  }
  const tabs = hybridTabList();
  const active = resolveHybridParamTab();
  tabsEl.style.display = '';
  tabsEl.innerHTML = '';
  tabs.forEach((backend) => {
    const button = document.createElement('button');
    button.type = 'button';
    button.textContent = backend === 'hybrid' ? t('hybrid.tabMix') : backendLabel(backend);
    const isActive = backend === active;
    button.style.cssText = [
      'padding:0.2rem 0.6rem',
      'font-size:11px',
      'border-radius:6px 6px 0 0',
      'border:1px solid rgba(255,255,255,0.14)',
      'border-bottom:none',
      'cursor:pointer',
      'color:#ffffff',
      `background:${isActive ? 'rgba(255,255,255,0.12)' : 'transparent'}`,
      `font-weight:${isActive ? '600' : '400'}`
    ].join(';');
    button.addEventListener('click', () => selectHybridParamTab(backend));
    tabsEl.append(button);
  });
}

export function updateHybridOptions() {
  dirty.hybrid = true;
  scheduleUIFlush();
}

export function renderVbapCartesian() {
  const vbapCartXSizeInputEl = getVbapCartXSizeInputEl();
  const vbapCartYSizeInputEl = getVbapCartYSizeInputEl();
  const vbapCartZSizeInputEl = getVbapCartZSizeInputEl();
  const vbapCartZNegSizeInputEl = getVbapCartZNegSizeInputEl();
  const vbapCartXStepInfoEl = getVbapCartXStepInfoEl();
  const vbapCartYStepInfoEl = getVbapCartYStepInfoEl();
  const vbapCartZStepInfoEl = getVbapCartZStepInfoEl();
  const vbapCartZNegStepInfoEl = getVbapCartZNegStepInfoEl();
  if (vbapCartXSizeInputEl) {
    vbapCartXSizeInputEl.value = app.vbapCartesianState.xSize === null ? '' : String(app.vbapCartesianState.xSize);
  }
  if (vbapCartYSizeInputEl) {
    vbapCartYSizeInputEl.value = app.vbapCartesianState.ySize === null ? '' : String(app.vbapCartesianState.ySize);
  }
  if (vbapCartZSizeInputEl) {
    vbapCartZSizeInputEl.value = app.vbapCartesianState.zSize === null ? '' : String(app.vbapCartesianState.zSize);
  }
  if (vbapCartZNegSizeInputEl) {
    vbapCartZNegSizeInputEl.value = String(Math.max(0, Math.round(Number(app.vbapCartesianState.zNegSize) || 0)));
  }
  const metersPerUnit = app.metersPerUnit ?? 1;
  const xStep = app.vbapCartesianState.xSize && app.vbapCartesianState.xSize > 0
    ? 2.0 / app.vbapCartesianState.xSize
    : null;
  const yStep = app.vbapCartesianState.ySize && app.vbapCartesianState.ySize > 0
    ? 2.0 / app.vbapCartesianState.ySize
    : null;
  const zStep = app.vbapCartesianState.zSize && app.vbapCartesianState.zSize > 0
    ? 1.0 / app.vbapCartesianState.zSize
    : null;
  const zNegStep = app.vbapAllowNegativeZ === false
    ? null
    : (Number(app.vbapCartesianState.zNegSize) || 0) > 0
      ? 1.0 / Number(app.vbapCartesianState.zNegSize)
    : null;
  const xStepMm = xStep === null ? null : xStep * metersPerUnit * 1000.0;
  const yStepMm = yStep === null ? null : yStep * metersPerUnit * 1000.0;
  const zStepMm = zStep === null ? null : zStep * metersPerUnit * 1000.0;
  const zNegStepMm = zNegStep === null ? null : zNegStep * metersPerUnit * 1000.0;
  if (vbapCartXStepInfoEl) vbapCartXStepInfoEl.textContent = xStepMm === null ? '—' : `${formatNumber(xStepMm, 1)}mm`;
  if (vbapCartYStepInfoEl) vbapCartYStepInfoEl.textContent = yStepMm === null ? '—' : `${formatNumber(yStepMm, 1)}mm`;
  if (vbapCartZStepInfoEl) vbapCartZStepInfoEl.textContent = zStepMm === null ? '—' : `${formatNumber(zStepMm, 1)}mm`;
  if (vbapCartZNegStepInfoEl) vbapCartZNegStepInfoEl.textContent = zNegStepMm === null ? '—' : `${formatNumber(zNegStepMm, 1)}mm`;
  updateVbapCartesianFaceGrid();
  renderVbapCartesianGridToggle();
  renderVbapPositionInterpolation();
}

export function updateVbapCartesian() {
  dirty.vbapCartesian = true;
  scheduleUIFlush();
}

export function renderVbapPolar() {
  const vbapPolarAzimuthResolutionInputEl = getVbapPolarAzimuthResolutionInputEl();
  const vbapPolarElevationResolutionInputEl = getVbapPolarElevationResolutionInputEl();
  const vbapPolarDistanceResInputEl = getVbapPolarDistanceResInputEl();
  const vbapPolarDistanceMaxInputEl = getVbapPolarDistanceMaxInputEl();
  const vbapElevationRangeInfoEl = getVbapElevationRangeInfoEl();
  const vbapAzimuthRangeInfoEl = getVbapAzimuthRangeInfoEl();
  const vbapPolarAzStepInfoEl = getVbapPolarAzStepInfoEl();
  const vbapPolarElStepInfoEl = getVbapPolarElStepInfoEl();
  const vbapPolarDistStepInfoEl = getVbapPolarDistStepInfoEl();
  if (vbapPolarAzimuthResolutionInputEl) {
    vbapPolarAzimuthResolutionInputEl.value =
      app.vbapPolarState.azimuthResolution === null ? '' : String(app.vbapPolarState.azimuthResolution);
  }
  if (vbapPolarElevationResolutionInputEl) {
    vbapPolarElevationResolutionInputEl.value =
      app.vbapPolarState.elevationResolution === null ? '' : String(app.vbapPolarState.elevationResolution);
  }
  if (vbapPolarDistanceResInputEl) {
    vbapPolarDistanceResInputEl.value =
      app.vbapPolarState.distanceRes === null ? '' : String(app.vbapPolarState.distanceRes);
  }
  if (vbapPolarDistanceMaxInputEl) {
    vbapPolarDistanceMaxInputEl.value =
      app.vbapPolarState.distanceMax === null ? '' : String(app.vbapPolarState.distanceMax);
  }
  if (vbapElevationRangeInfoEl) {
    const txt = app.vbapAllowNegativeZ === null
      ? '—'
      : app.vbapAllowNegativeZ
        ? '-90..90'
        : '0..90';
    vbapElevationRangeInfoEl.textContent = txt;
  }
  if (vbapAzimuthRangeInfoEl) {
    vbapAzimuthRangeInfoEl.textContent = '-180..180';
  }
  const azStep = app.vbapPolarState.azimuthResolution && app.vbapPolarState.azimuthResolution > 0
    ? 360.0 / app.vbapPolarState.azimuthResolution
    : null;
  const elRange = app.vbapAllowNegativeZ === false ? 90.0 : 180.0;
  const elStep = app.vbapPolarState.elevationResolution && app.vbapPolarState.elevationResolution > 0
    ? elRange / app.vbapPolarState.elevationResolution
    : null;
  const dStep = (app.vbapPolarState.distanceRes && app.vbapPolarState.distanceRes > 0 && app.vbapPolarState.distanceMax && app.vbapPolarState.distanceMax > 0)
    ? app.vbapPolarState.distanceMax / app.vbapPolarState.distanceRes
    : null;
  if (vbapPolarAzStepInfoEl) vbapPolarAzStepInfoEl.textContent = azStep === null ? '—' : `${formatNumber(azStep, 2)}°`;
  if (vbapPolarElStepInfoEl) vbapPolarElStepInfoEl.textContent = elStep === null ? '—' : `${formatNumber(elStep, 2)}°`;
  if (vbapPolarDistStepInfoEl) vbapPolarDistStepInfoEl.textContent = dStep === null ? '—' : `${formatNumber(dStep, 3)}`;
  renderVbapPositionInterpolation();
}

export function updateVbapPolar() {
  dirty.vbapPolar = true;
  scheduleUIFlush();
}

export function renderVbapPositionInterpolation() {
  const vbapPositionInterpolationToggleEl = getVbapPositionInterpolationToggleEl();
  if (!vbapPositionInterpolationToggleEl) return;
  vbapPositionInterpolationToggleEl.checked = app.vbapPositionInterpolation !== false;
}

export function updateVbapPositionInterpolation() {
  dirty.vbapPolar = true;
  scheduleUIFlush();
}
