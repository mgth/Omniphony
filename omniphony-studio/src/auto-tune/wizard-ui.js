// PI auto-tune wizard modal UI.
//
// Renders all wizard screens (preparation → kp sweep → ki tuning →
// disturbance breakpoint → long run → tightening → summary) inside the
// #autoTunePiWizardModal placeholder declared in index.html. Owns the
// sparkline render loop while the modal is open.

import { app } from '../state.js';
import { t, tf } from '../i18n.js';
import { createAutoTuneRunner } from './runner.js';
import { createSparkline } from './sparkline.js';
import { attachQuitGuard, detachQuitGuard } from './quit-guard.js';

let modalEl = null;
let bodyEl = null;
let actionsEl = null;
let canvasEl = null;
let sparkline = null;
let renderInterval = null;
let runner = null;
let runnerOff = null;
let lastProgress = {};

function ensureMarkup() {
  if (modalEl) return;
  modalEl = document.getElementById('autoTunePiWizardModal');
  if (!modalEl) return;
  modalEl.innerHTML = `
    <div class="info-modal-card" style="max-width:520px;width:min(96vw,520px);display:flex;flex-direction:column;gap:0.6rem">
      <div class="info-modal-title" data-i18n="autoTune.title">PI Auto-tune</div>
      <div id="autoTuneWizardBody" class="info-modal-text" style="display:flex;flex-direction:column;gap:0.45rem"></div>
      <canvas id="autoTuneSparkline" width="480" height="140" style="display:none;border-radius:8px;width:100%;max-width:480px;height:auto"></canvas>
      <div id="autoTuneWizardActions" class="info-modal-actions" style="display:flex;justify-content:flex-end;gap:0.4rem"></div>
    </div>
  `;
  bodyEl = modalEl.querySelector('#autoTuneWizardBody');
  actionsEl = modalEl.querySelector('#autoTuneWizardActions');
  canvasEl = modalEl.querySelector('#autoTuneSparkline');
  sparkline = createSparkline(canvasEl);
}

function setOpen(open) {
  ensureMarkup();
  if (!modalEl) return;
  modalEl.classList.toggle('open', Boolean(open));
  modalEl.setAttribute('aria-hidden', open ? 'false' : 'true');
}

function setBody(html) {
  if (bodyEl) bodyEl.innerHTML = html;
}

function setActions(buttons) {
  if (!actionsEl) return;
  actionsEl.innerHTML = '';
  for (const { id, textKey, fallback, kind = 'secondary', onClick } of buttons) {
    const btn = document.createElement('button');
    btn.id = id;
    btn.type = 'button';
    btn.className = kind === 'primary' ? 'ui-btn ui-btn-primary' : 'toggle-btn';
    if (textKey) btn.setAttribute('data-i18n', textKey);
    btn.textContent = (textKey && t(textKey)) || fallback;
    if (onClick) btn.addEventListener('click', onClick);
    actionsEl.appendChild(btn);
  }
}

function showSparkline(show) {
  if (canvasEl) canvasEl.style.display = show ? 'block' : 'none';
}

function startSparklineRender() {
  if (renderInterval) return;
  renderInterval = setInterval(() => {
    if (!sparkline) return;
    sparkline.push({
      t: Date.now(),
      latencySmoothedMs: typeof app.latencySmoothedMs === 'number' ? app.latencySmoothedMs : null,
      latencyTargetMs: typeof app.latencyTargetMs === 'number' ? app.latencyTargetMs : null,
      resampleRatio: typeof app.resampleRatio === 'number' ? app.resampleRatio : null,
    });
    sparkline.render();
  }, 100);
}

function stopSparklineRender() {
  if (renderInterval) {
    clearInterval(renderInterval);
    renderInterval = null;
  }
  if (sparkline) sparkline.clear();
}

function fmt(value, digits = 2) {
  if (value === null || value === undefined || !isFinite(value)) return '—';
  return Number(value).toFixed(digits);
}

function renderPreparation() {
  showSparkline(false);
  stopSparklineRender();
  setBody(`
    <p><strong>${t('autoTune.intro') || 'Auto-tune the local resampler PI controller.'}</strong></p>
    <p>${t('autoTune.prep') || 'Play a long, stable audio source before starting. The tuning lasts roughly 12-15 minutes and modifies kp / ki / max_adjust / update_interval_callbacks live. Initial values will be restored on Cancel.'}</p>
    <p style="opacity:0.8;font-size:0.9em">${t('autoTune.persistReminder') || 'Values are NOT persisted automatically. Click the main Save button after Accept to write them to config.yaml.'}</p>
  `);
  setActions([
    { id: 'autoTuneCancelBtn', textKey: 'common.cancel', fallback: 'Cancel', kind: 'secondary', onClick: closeWizard },
    { id: 'autoTuneStartBtn', textKey: 'autoTune.start', fallback: 'Start', kind: 'primary', onClick: handleStart },
  ]);
}

function renderHoldKp(payload) {
  showSparkline(true);
  startSparklineRender();
  const kp = payload.currentKp ?? lastProgress.currentKp;
  const pp = payload.palierStats?.peakToPeakPpm;
  const cr = payload.palierStats?.crossingRate;
  const ppJump = payload.verdict?.peakToPeakJump;
  const reason = payload.verdict?.reason;
  let statsLine = '';
  if (pp !== undefined && pp !== null) {
    const jumpFrag = ppJump && isFinite(ppJump) ? ` · jump ×${fmt(ppJump, 1)}` : '';
    const reasonFrag = reason ? ` · ${reason}` : '';
    statsLine = `<div style="opacity:0.7;font-size:0.85em">p-p ${fmt(pp, 0)} ppm · ${fmt(cr, 2)} cross/s${jumpFrag}${reasonFrag}</div>`;
  }
  setBody(`
    <div><strong>${t('autoTune.step1Label') || 'Step 1 — Finding Kp'}</strong></div>
    <div>Kp = ${fmt(kp, kp >= 100 ? 0 : 2)}</div>
    ${statsLine}
    <div style="opacity:0.7;font-size:0.9em">${t('autoTune.holdHint') || 'Doubling Kp every 30 s until oscillation is detected on rate_adjust_ppm.'}</div>
    ${payload.saturated ? `<div style="opacity:0.7;font-size:0.85em;color:#ffd54a">${t('autoTune.saturated') || 'Saturated — continuing.'}</div>` : ''}
  `);
  setActions([
    { id: 'autoTuneCancelBtn', textKey: 'common.cancel', fallback: 'Cancel', kind: 'secondary', onClick: handleCancel },
  ]);
}

function renderTuningKi(payload) {
  showSparkline(true);
  startSparklineRender();
  const kpFinal = payload.kpFinal ?? lastProgress.kpFinal;
  const ki = payload.currentKi ?? lastProgress.currentKi;
  const iter = payload.kiIteration ?? lastProgress.kiIteration ?? 0;
  setBody(`
    <div><strong>${t('autoTune.step2Label') || 'Step 2 — Finding Ki'}</strong></div>
    <div>Kp = ${fmt(kpFinal)} · Ki = ${fmt(ki, 4)}</div>
    <div style="opacity:0.7;font-size:0.9em">${tf('autoTune.kiIteration', { iter }) || `Iteration ${iter}`}</div>
    ${payload.reason ? `<div style="opacity:0.7;font-size:0.85em">${payload.reason}</div>` : ''}
  `);
  setActions([
    { id: 'autoTuneCancelBtn', textKey: 'common.cancel', fallback: 'Cancel', kind: 'secondary', onClick: handleCancel },
  ]);
}

function renderAwaitPerturbation(payload) {
  showSparkline(true);
  setBody(`
    <div><strong>${t('autoTune.step3Label') || 'Step 3 — Disturbance test'}</strong></div>
    <p>${t('autoTune.perturbationPrompt') || 'Pause your audio source for 2-3 seconds, then resume it. When done, click Continue.'}</p>
    <div style="opacity:0.7;font-size:0.9em">Kp = ${fmt(payload.kpFinal ?? lastProgress.kpFinal)} · Ki = ${fmt(payload.kiFinal ?? lastProgress.kiFinal, 4)}</div>
    ${payload.hitIterationCap ? `<div style="opacity:0.7;font-size:0.85em;color:#ffd54a">${t('autoTune.kiBestKept') || 'Ki iteration cap reached; using best value seen.'}</div>` : ''}
  `);
  setActions([
    { id: 'autoTuneCancelBtn', textKey: 'common.cancel', fallback: 'Cancel', kind: 'secondary', onClick: handleCancel },
    { id: 'autoTuneSkipBtn', textKey: 'autoTune.skip', fallback: 'Skip', kind: 'secondary', onClick: () => runner && runner.userAck('skipPerturbation') },
    { id: 'autoTuneContinueBtn', textKey: 'autoTune.continue', fallback: 'Continue', kind: 'primary', onClick: () => runner && runner.userAck('perturbation') },
  ]);
}

function renderPerturbationRecovering() {
  showSparkline(true);
  setBody(`
    <div><strong>${t('autoTune.step3Recovering') || 'Step 3 — Recovering'}</strong></div>
    <div style="opacity:0.7;font-size:0.9em">${t('autoTune.recoveringHint') || 'Watching rate_adjust to settle (~15 s)…'}</div>
  `);
  setActions([
    { id: 'autoTuneCancelBtn', textKey: 'common.cancel', fallback: 'Cancel', kind: 'secondary', onClick: handleCancel },
  ]);
}

function renderLongRun(payload) {
  showSparkline(true);
  startSparklineRender();
  const elapsedSec = Math.round((payload.elapsedMs || 0) / 1000);
  const canAbbreviate = payload.canAbbreviate || lastProgress.canAbbreviate;
  setBody(`
    <div><strong>${t('autoTune.step4Label') || 'Step 4 — Long-run stability'}</strong></div>
    <div>${tf('autoTune.elapsed', { sec: elapsedSec }) || `Elapsed: ${elapsedSec} s`}</div>
    <div style="opacity:0.7;font-size:0.9em">${t('autoTune.longRunHint') || 'Watching the system over 10 minutes to size max_adjust.'}</div>
  `);
  const actions = [
    { id: 'autoTuneCancelBtn', textKey: 'common.cancel', fallback: 'Cancel', kind: 'secondary', onClick: handleCancel },
  ];
  if (canAbbreviate) {
    actions.push({
      id: 'autoTuneAbbreviateBtn',
      textKey: 'autoTune.abbreviate',
      fallback: 'Shorten',
      kind: 'primary',
      onClick: () => runner && runner.abbreviate(),
    });
  }
  setActions(actions);
}

function renderTightening(payload) {
  showSparkline(true);
  setBody(`
    <div><strong>${t('autoTune.step5Label') || 'Step 5 — Tightening protections'}</strong></div>
    <div>max_adjust = ${fmt((payload.maxAdjustFinal ?? lastProgress.maxAdjustFinal) * 100, 2)} %</div>
    <div>update_interval_callbacks = ${payload.updateIntervalFinal ?? lastProgress.updateIntervalFinal ?? '—'}</div>
    ${payload.maxAdjustWarn ? `<div style="color:#ff9b6c">${t('autoTune.maxAdjustWarn') || 'High drift detected — max_adjust exceeds 15 %.'}</div>` : ''}
    <div style="opacity:0.7;font-size:0.85em">${t('autoTune.tighteningHint') || 'Verifying the system stays stable for 30 s…'}</div>
  `);
  setActions([
    { id: 'autoTuneCancelBtn', textKey: 'common.cancel', fallback: 'Cancel', kind: 'secondary', onClick: handleCancel },
  ]);
}

function renderSummary(result) {
  showSparkline(false);
  stopSparklineRender();
  setBody(`
    <div><strong>${t('autoTune.summaryTitle') || 'Tuning complete'}</strong></div>
    <table style="border-collapse:collapse;font-size:0.9em">
      <tr><td style="padding:0.15rem 0.5rem 0.15rem 0;opacity:0.75">kp_crit</td><td>≈ ${fmt(result.kpCrit)}</td></tr>
      <tr><td style="padding:0.15rem 0.5rem 0.15rem 0;opacity:0.75">kp_near</td><td>${fmt(result.kpFinal)}</td></tr>
      <tr><td style="padding:0.15rem 0.5rem 0.15rem 0;opacity:0.75">ki</td><td>${fmt(result.kiFinal, 4)}</td></tr>
      <tr><td style="padding:0.15rem 0.5rem 0.15rem 0;opacity:0.75">max_adjust</td><td>${fmt(result.maxAdjustFinal * 100, 2)} %</td></tr>
      <tr><td style="padding:0.15rem 0.5rem 0.15rem 0;opacity:0.75">update_interval_callbacks</td><td>${fmt(result.updateIntervalFinal, 0)}</td></tr>
    </table>
    <div style="opacity:0.8;font-size:0.9em">${t('autoTune.persistReminder') || 'Values are applied live but NOT persisted. Click the main Save button to write them to config.yaml.'}</div>
  `);
  setActions([
    { id: 'autoTuneRevertBtn', textKey: 'autoTune.revert', fallback: 'Revert', kind: 'secondary', onClick: handleCancel },
    { id: 'autoTuneAcceptBtn', textKey: 'autoTune.accept', fallback: 'Accept', kind: 'primary', onClick: handleAccept },
  ]);
}

function renderError(payload) {
  showSparkline(false);
  stopSparklineRender();
  const msg = payload.kind === 'no-oscillation'
    ? (tf('autoTune.errorNoOsc', { kp: fmt(payload.kpReached) })
      || `No oscillation reached at Kp=${fmt(payload.kpReached)}. The system might be heavily damped or the source unstable.`)
    : (t('autoTune.errorGeneric') || `Error: ${payload.kind || 'unknown'}`);
  setBody(`<div style="color:#ff9b6c">${msg}</div>`);
  setActions([
    { id: 'autoTuneCloseBtn', textKey: 'common.close', fallback: 'Close', kind: 'primary', onClick: handleCancel },
  ]);
}

function renderRefused(payload) {
  showSparkline(false);
  stopSparklineRender();
  const msg = payload.reason === 'not-enabled'
    ? (t('autoTune.refusedNotEnabled') || 'Adaptive resampling is disabled. Enable it before running the auto-tuner.')
    : payload.reason === 'paused'
      ? (t('autoTune.refusedPaused') || 'The resampling controller is paused. Resume it before running the auto-tuner.')
      : (t('autoTune.refusedGeneric') || `Cannot start: ${payload.reason}`);
  setBody(`<div style="color:#ff9b6c">${msg}</div>`);
  setActions([
    { id: 'autoTuneCloseBtn', textKey: 'common.close', fallback: 'Close', kind: 'primary', onClick: closeWizard },
  ]);
}

function renderSuspended() {
  showSparkline(true);
  setBody(`
    <div style="color:#ff9b6c"><strong>${t('autoTune.suspended') || 'Source lost'}</strong></div>
    <div>${t('autoTune.sourceLostHint') || 'Audio source went unstable. Restart your source then click Resume.'}</div>
  `);
  setActions([
    { id: 'autoTuneCancelBtn', textKey: 'common.cancel', fallback: 'Cancel', kind: 'secondary', onClick: handleCancel },
    { id: 'autoTuneResumeBtn', textKey: 'autoTune.resume', fallback: 'Resume', kind: 'primary', onClick: () => runner && runner.userAck('resumeAfterSourceLoss') },
  ]);
}

async function handleStart() {
  if (!runner) return;
  const result = await runner.start();
  // Only guard the close button once a run is actually under way: a refused
  // start leaves nothing to lose.
  if (!result || result.started !== false) {
    attachQuitGuard(handleCancel).catch((err) => {
      // eslint-disable-next-line no-console
      console.error('[auto-tune wizard] could not guard the close button', err);
    });
  }
}

async function handleCancel() {
  if (runner) {
    try {
      await runner.cancel();
    } catch (err) {
      // eslint-disable-next-line no-console
      console.error('[auto-tune wizard] cancel failed', err);
    }
  }
  closeWizard();
}

async function handleAccept() {
  if (runner) {
    try {
      await runner.accept();
    } catch (err) {
      // eslint-disable-next-line no-console
      console.error('[auto-tune wizard] accept failed', err);
    }
  }
  closeWizard();
}

function closeWizard() {
  detachQuitGuard();
  stopSparklineRender();
  if (runnerOff) {
    runnerOff();
    runnerOff = null;
  }
  setOpen(false);
  runner = null;
  lastProgress = {};
}

function onRunnerEvent(event, payload) {
  switch (event) {
    case 'progress': {
      lastProgress = { ...lastProgress, ...payload };
      const step = payload.step;
      if (step === 'holdKp') renderHoldKp(payload);
      else if (step === 'tuningKi') renderTuningKi(payload);
      else if (step === 'awaitPerturbation') renderAwaitPerturbation(payload);
      else if (step === 'perturbationRecovering') renderPerturbationRecovering();
      else if (step === 'longRun') renderLongRun(payload);
      else if (step === 'tightening') renderTightening(payload);
      break;
    }
    case 'awaitUserAction':
      if (payload.kind === 'perturbation') renderAwaitPerturbation(payload);
      break;
    case 'complete':
      renderSummary(payload);
      break;
    case 'cancelled':
      break;
    case 'error':
      renderError(payload);
      break;
    case 'refused':
      renderRefused(payload);
      break;
    case 'sourceLost':
      renderSuspended();
      break;
    case 'sourceRecovered':
      break;
    default:
      break;
  }
}

export function openAutoTuneWizard() {
  ensureMarkup();
  if (!modalEl) return;
  if (!runner) {
    runner = createAutoTuneRunner();
    runnerOff = runner.on(onRunnerEvent);
  }
  renderPreparation();
  setOpen(true);
}

export async function closeAutoTuneWizardOnEscape() {
  if (!modalEl || !modalEl.classList.contains('open')) return;
  await handleCancel();
}
