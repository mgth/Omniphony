import { app } from '../state.js';
import {
  setTrailInfoModalOpen, setEffectiveRenderInfoModalOpen,
  setOscInfoModalOpen, setAboutModalOpen, setRoomGeometryInfoModalOpen,
  setAdaptiveResamplingInfoModalOpen, setTelemetryGaugesInfoModalOpen,
  setRampModeInfoModalOpen, setVbapPositionInterpolationInfoModalOpen,
  setSpreadFromDistanceInfoModalOpen, setDistanceDiffuseInfoModalOpen,
  setTelemetryGaugesOpen,
  setDisplaySectionOpen, setAudioOutputSectionOpen, setInputSectionOpen, setRendererSectionOpen
} from '../modals.js';

function bindModalOpenClose({ buttonId, closeButtonId, modalId, open, close }) {
  const buttonEl = document.getElementById(buttonId);
  const closeButtonEl = document.getElementById(closeButtonId);
  const modalEl = document.getElementById(modalId);

  if (buttonEl) {
    buttonEl.addEventListener('click', open);
  }

  if (closeButtonEl) {
    closeButtonEl.addEventListener('click', close);
  }

  if (modalEl) {
    modalEl.addEventListener('click', (event) => {
      if (event.target === modalEl) {
        close();
      }
    });
  }
}

export function setupModalAndToggleListeners() {
  bindModalOpenClose({
    buttonId: 'spreadFromDistanceInfoBtn',
    closeButtonId: 'spreadFromDistanceInfoCloseBtn',
    modalId: 'spreadFromDistanceInfoModal',
    open: () => setSpreadFromDistanceInfoModalOpen(true),
    close: () => setSpreadFromDistanceInfoModalOpen(false)
  });

  bindModalOpenClose({
    buttonId: 'distanceDiffuseInfoBtn',
    closeButtonId: 'distanceDiffuseInfoCloseBtn',
    modalId: 'distanceDiffuseInfoModal',
    open: () => setDistanceDiffuseInfoModalOpen(true),
    close: () => setDistanceDiffuseInfoModalOpen(false)
  });

  bindModalOpenClose({
    buttonId: 'trailInfoBtn',
    closeButtonId: 'trailInfoCloseBtn',
    modalId: 'trailInfoModal',
    open: () => setTrailInfoModalOpen(true),
    close: () => setTrailInfoModalOpen(false)
  });

  bindModalOpenClose({
    buttonId: 'effectiveRenderInfoBtn',
    closeButtonId: 'effectiveRenderInfoCloseBtn',
    modalId: 'effectiveRenderInfoModal',
    open: () => setEffectiveRenderInfoModalOpen(true),
    close: () => setEffectiveRenderInfoModalOpen(false)
  });

  bindModalOpenClose({
    buttonId: 'oscInfoBtn',
    closeButtonId: 'oscInfoCloseBtn',
    modalId: 'oscInfoModal',
    open: () => setOscInfoModalOpen(true),
    close: () => setOscInfoModalOpen(false)
  });

  bindModalOpenClose({
    buttonId: 'roomGeometryInfoBtn',
    closeButtonId: 'roomGeometryInfoCloseBtn',
    modalId: 'roomGeometryInfoModal',
    open: () => setRoomGeometryInfoModalOpen(true),
    close: () => setRoomGeometryInfoModalOpen(false)
  });

  bindModalOpenClose({
    buttonId: 'adaptiveResamplingInfoBtn',
    closeButtonId: 'adaptiveResamplingInfoCloseBtn',
    modalId: 'adaptiveResamplingInfoModal',
    open: () => setAdaptiveResamplingInfoModalOpen(true),
    close: () => setAdaptiveResamplingInfoModalOpen(false)
  });

  bindModalOpenClose({
    buttonId: 'telemetryGaugesInfoBtn',
    closeButtonId: 'telemetryGaugesInfoCloseBtn',
    modalId: 'telemetryGaugesInfoModal',
    open: () => setTelemetryGaugesInfoModalOpen(true),
    close: () => setTelemetryGaugesInfoModalOpen(false)
  });

  bindModalOpenClose({
    buttonId: 'rampModeInfoBtn',
    closeButtonId: 'rampModeInfoCloseBtn',
    modalId: 'rampModeInfoModal',
    open: () => setRampModeInfoModalOpen(true),
    close: () => setRampModeInfoModalOpen(false)
  });

  bindModalOpenClose({
    buttonId: 'vbapPositionInterpolationInfoBtn',
    closeButtonId: 'vbapPositionInterpolationInfoCloseBtn',
    modalId: 'vbapPositionInterpolationInfoModal',
    open: () => setVbapPositionInterpolationInfoModalOpen(true),
    close: () => setVbapPositionInterpolationInfoModalOpen(false)
  });

  const aboutBtnEl = document.getElementById('aboutBtn');
  const aboutOpenAreaEl = document.getElementById('aboutOpenArea');
  const aboutCloseBtnEl = document.getElementById('aboutCloseBtn');
  const aboutModalEl = document.getElementById('aboutModal');

  if (aboutBtnEl) {
    aboutBtnEl.addEventListener('click', () => {
      setAboutModalOpen(true);
    });
  }

  if (aboutOpenAreaEl) {
    aboutOpenAreaEl.addEventListener('click', () => {
      setAboutModalOpen(true);
    });
    aboutOpenAreaEl.addEventListener('keydown', (event) => {
      if (event.key === 'Enter' || event.key === ' ') {
        event.preventDefault();
        setAboutModalOpen(true);
      }
    });
  }

  if (aboutCloseBtnEl) {
    aboutCloseBtnEl.addEventListener('click', () => {
      setAboutModalOpen(false);
    });
  }

  if (aboutModalEl) {
    aboutModalEl.addEventListener('click', (event) => {
      if (event.target === aboutModalEl) {
        setAboutModalOpen(false);
      }
    });
  }

  const telemetryGaugesToggleBtnEl = document.getElementById('telemetryGaugesToggleBtn');
  const displaySectionToggleBtnEl = document.getElementById('displaySectionToggleBtn');
  const audioOutputSectionToggleBtnEl = document.getElementById('audioOutputSectionToggleBtn');
  const inputSectionToggleBtnEl = document.getElementById('inputSectionToggleBtn');
  const rendererSectionToggleBtnEl = document.getElementById('rendererSectionToggleBtn');

  if (telemetryGaugesToggleBtnEl) {
    telemetryGaugesToggleBtnEl.addEventListener('click', () => {
      setTelemetryGaugesOpen(!app.telemetryGaugesOpen);
    });
  }

  if (displaySectionToggleBtnEl) {
    displaySectionToggleBtnEl.addEventListener('click', () => {
      setDisplaySectionOpen(!app.displaySectionOpen);
    });
  }

  if (audioOutputSectionToggleBtnEl) {
    audioOutputSectionToggleBtnEl.addEventListener('click', () => {
      setAudioOutputSectionOpen(!app.audioOutputSectionOpen);
    });
  }

  if (inputSectionToggleBtnEl) {
    inputSectionToggleBtnEl.addEventListener('click', () => {
      setInputSectionOpen(!app.inputSectionOpen);
    });
  }

  if (rendererSectionToggleBtnEl) {
    rendererSectionToggleBtnEl.addEventListener('click', () => {
      setRendererSectionOpen(!app.rendererSectionOpen);
    });
  }

  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') {
      setDistanceDiffuseInfoModalOpen(false);
      setAdaptiveResamplingInfoModalOpen(false);
      setTrailInfoModalOpen(false);
      setOscInfoModalOpen(false);
      setRoomGeometryInfoModalOpen(false);
      setTelemetryGaugesInfoModalOpen(false);
      setRampModeInfoModalOpen(false);
    }
  });
}
