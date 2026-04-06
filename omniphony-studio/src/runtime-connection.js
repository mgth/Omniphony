import { app } from './state.js';

const RUNTIME_LOCK_SELECTORS = [
  '#overlay button',
  '#overlay input',
  '#overlay select',
  '#overlay textarea',
  '#speakersOverlay button',
  '#speakersOverlay input',
  '#speakersOverlay select',
  '#speakersOverlay textarea'
].join(', ');

const OSC_CONTROL_IDS = new Set([
  'oscConfigToggleBtn',
  'oscHostInput',
  'oscRxPortInput',
  'oscListenPortInput',
  'oscBridgePathInput',
  'oscBridgeBrowseBtn',
  'oscConfigApplyBtn',
  'oscServiceBtn',
  'oscRestartServiceBtn',
  'oscRestartPipewireBtn',
  'oscLaunchRendererBtn',
  'oscInfoBtn',
  'oscMeteringToggle'
]);

const PANEL_TOGGLE_IDS = [
  'inputSectionToggleBtn',
  'roomGeometryToggleBtn',
  'displaySectionToggleBtn',
  'audioOutputSectionToggleBtn',
  'telemetryGaugesToggleBtn',
  'rendererSectionToggleBtn'
];

const EXEMPT_CONTROL_IDS = new Set([...OSC_CONTROL_IDS, ...PANEL_TOGGLE_IDS]);

function runtimeConnected() {
  return app.oscStatusState === 'connected';
}

export function syncRuntimeConnectionLock() {
  const connected = runtimeConnected();
  PANEL_TOGGLE_IDS.forEach((id) => {
    const el = document.getElementById(id);
    if (el instanceof HTMLButtonElement) {
      el.disabled = !connected;
    }
  });
  document.querySelectorAll(RUNTIME_LOCK_SELECTORS).forEach((el) => {
    if (!(el instanceof HTMLElement)) {
      return;
    }
    if (EXEMPT_CONTROL_IDS.has(el.id)) {
      return;
    }
    if (!connected) {
      if (el.dataset.runtimeLockApplied !== '1') {
        el.dataset.runtimeLockApplied = '1';
        el.dataset.runtimeLockPrevDisabled = el.disabled ? '1' : '0';
      }
      el.disabled = true;
      return;
    }
    if (el.dataset.runtimeLockApplied === '1') {
      el.disabled = el.dataset.runtimeLockPrevDisabled === '1';
      delete el.dataset.runtimeLockApplied;
      delete el.dataset.runtimeLockPrevDisabled;
    }
  });
}
