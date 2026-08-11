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
  'oscMeteringToggle',
  // Edits the local mpv.conf, not the runtime: it must stay usable while
  // nothing is connected. That is in fact when it is needed — the switch is
  // what gets a first-time setup to the point where a renderer can connect.
  'mpvOrenderToggle'
]);

const PANEL_TOGGLE_IDS = [
  'inputSectionToggleBtn',
  'roomGeometryToggleBtn',
  'displaySectionToggleBtn',
  'audioOutputSectionToggleBtn',
  'telemetryGaugesToggleBtn',
  'rendererSectionToggleBtn'
];

const PANEL_COLLAPSE_IDS = [
  'leftPanelCollapseBtn',
  'rightPanelCollapseBtn'
];

// Pure navigation between the Renderer / Binaural parameter tabs: browsing
// must stay possible with no runtime connected (the controls inside the tabs
// are locked individually by the generic selector sweep).
const TAB_IDS = [
  'rendererTabRendererBtn',
  'rendererTabBinauralBtn'
];

const EXEMPT_CONTROL_IDS = new Set([
  ...OSC_CONTROL_IDS,
  ...PANEL_TOGGLE_IDS,
  ...PANEL_COLLAPSE_IDS,
  ...TAB_IDS
]);

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
