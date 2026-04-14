/**
 * Modal dialog and accordion section toggle helpers.
 */

import { app } from './state.js';

function notifyOverlayLayoutChanged(reason) {
  if (typeof window === 'undefined') {
    return;
  }
  window.dispatchEvent(new CustomEvent('omniphony:overlay-layout-changed', {
    detail: { reason }
  }));
}

// ---------------------------------------------------------------------------
// DOM refs (queried once at module load)
// ---------------------------------------------------------------------------

const trailInfoModalEl = document.getElementById('trailInfoModal');
const effectiveRenderInfoModalEl = document.getElementById('effectiveRenderInfoModal');
const oscInfoModalEl = document.getElementById('oscInfoModal');
const aboutModalEl = document.getElementById('aboutModal');
const roomGeometryInfoModalEl = document.getElementById('roomGeometryInfoModal');
const adaptiveResamplingInfoModalEl = document.getElementById('adaptiveResamplingInfoModal');
const telemetryGaugesInfoModalEl = document.getElementById('telemetryGaugesInfoModal');
const rampModeInfoModalEl = document.getElementById('rampModeInfoModal');
const vbapPositionInterpolationInfoModalEl = document.getElementById('vbapPositionInterpolationInfoModal');
const telemetryGaugesFormEl = document.getElementById('telemetryGaugesForm');
const telemetryGaugesToggleBtnEl = document.getElementById('telemetryGaugesToggleBtn');
const displaySectionContentEl = document.getElementById('displaySectionContent');
const displaySectionToggleBtnEl = document.getElementById('displaySectionToggleBtn');
const audioOutputSectionContentEl = document.getElementById('audioOutputSectionContent');
const audioOutputSummaryEl = document.getElementById('audioOutputSummary');
const audioOutputSectionToggleBtnEl = document.getElementById('audioOutputSectionToggleBtn');
const inputSectionContentEl = document.getElementById('inputSectionContent');
const inputSummaryEl = document.getElementById('inputSummary');
const inputSectionToggleBtnEl = document.getElementById('inputSectionToggleBtn');
const rendererSectionContentEl = document.getElementById('rendererSectionContent');
const rendererSummaryEl = document.getElementById('rendererSummary');
const rendererSectionToggleBtnEl = document.getElementById('rendererSectionToggleBtn');
const spreadFromDistanceInfoModalEl = document.getElementById('spreadFromDistanceInfoModal');
const distanceDiffuseInfoModalEl = document.getElementById('distanceDiffuseInfoModal');

// ---------------------------------------------------------------------------
// Simple info modals
// ---------------------------------------------------------------------------

export function setTrailInfoModalOpen(open) {
  if (!trailInfoModalEl) return;
  trailInfoModalEl.classList.toggle('open', Boolean(open));
}

export function setEffectiveRenderInfoModalOpen(open) {
  if (!effectiveRenderInfoModalEl) return;
  effectiveRenderInfoModalEl.classList.toggle('open', Boolean(open));
}

export function setOscInfoModalOpen(open) {
  if (!oscInfoModalEl) return;
  oscInfoModalEl.classList.toggle('open', Boolean(open));
}

export function setAboutModalOpen(open) {
  if (!aboutModalEl) return;
  aboutModalEl.classList.toggle('open', Boolean(open));
}

export function setRoomGeometryInfoModalOpen(open) {
  if (!roomGeometryInfoModalEl) return;
  roomGeometryInfoModalEl.classList.toggle('open', Boolean(open));
}

export function setAdaptiveResamplingInfoModalOpen(open) {
  if (!adaptiveResamplingInfoModalEl) return;
  adaptiveResamplingInfoModalEl.classList.toggle('open', Boolean(open));
}

export function setTelemetryGaugesInfoModalOpen(open) {
  if (!telemetryGaugesInfoModalEl) return;
  telemetryGaugesInfoModalEl.classList.toggle('open', Boolean(open));
}

export function setRampModeInfoModalOpen(open) {
  if (!rampModeInfoModalEl) return;
  rampModeInfoModalEl.classList.toggle('open', Boolean(open));
}

export function setVbapPositionInterpolationInfoModalOpen(open) {
  if (!vbapPositionInterpolationInfoModalEl) return;
  vbapPositionInterpolationInfoModalEl.classList.toggle('open', Boolean(open));
}

export function setSpreadFromDistanceInfoModalOpen(open) {
  if (!spreadFromDistanceInfoModalEl) return;
  spreadFromDistanceInfoModalEl.classList.toggle('open', Boolean(open));
}

export function setDistanceDiffuseInfoModalOpen(open) {
  if (!distanceDiffuseInfoModalEl) return;
  distanceDiffuseInfoModalEl.classList.toggle('open', Boolean(open));
}

// ---------------------------------------------------------------------------
// Accordion sections (update shared state)
// ---------------------------------------------------------------------------

export function setTelemetryGaugesOpen(open) {
  app.telemetryGaugesOpen = Boolean(open);
  if (telemetryGaugesFormEl) {
    telemetryGaugesFormEl.classList.toggle('open', app.telemetryGaugesOpen);
  }
  if (telemetryGaugesToggleBtnEl) {
    telemetryGaugesToggleBtnEl.textContent = app.telemetryGaugesOpen ? '▾' : '▸';
  }
  notifyOverlayLayoutChanged('telemetry-gauges-toggle');
}

export function setDisplaySectionOpen(open) {
  app.displaySectionOpen = Boolean(open);
  if (displaySectionContentEl) {
    displaySectionContentEl.classList.toggle('open', app.displaySectionOpen);
  }
  if (displaySectionToggleBtnEl) {
    displaySectionToggleBtnEl.textContent = app.displaySectionOpen ? '▾' : '▸';
  }
  notifyOverlayLayoutChanged('display-section-toggle');
}

export function setAudioOutputSectionOpen(open) {
  app.audioOutputSectionOpen = Boolean(open);
  if (audioOutputSectionContentEl) {
    audioOutputSectionContentEl.classList.toggle('open', app.audioOutputSectionOpen);
  }
  if (audioOutputSummaryEl) {
    audioOutputSummaryEl.style.display = app.audioOutputSectionOpen ? 'none' : 'block';
  }
  if (audioOutputSectionToggleBtnEl) {
    audioOutputSectionToggleBtnEl.textContent = app.audioOutputSectionOpen ? '▾' : '▸';
  }
  notifyOverlayLayoutChanged('audio-output-section-toggle');
}

export function setInputSectionOpen(open) {
  app.inputSectionOpen = Boolean(open);
  if (inputSectionContentEl) {
    inputSectionContentEl.classList.toggle('open', app.inputSectionOpen);
  }
  if (inputSummaryEl) {
    inputSummaryEl.style.display = app.inputSectionOpen ? 'none' : 'block';
  }
  if (inputSectionToggleBtnEl) {
    inputSectionToggleBtnEl.textContent = app.inputSectionOpen ? '▾' : '▸';
  }
  notifyOverlayLayoutChanged('input-section-toggle');
}

export function setRendererSectionOpen(open) {
  app.rendererSectionOpen = Boolean(open);
  if (rendererSectionContentEl) {
    rendererSectionContentEl.classList.toggle('open', app.rendererSectionOpen);
  }
  if (rendererSummaryEl) {
    rendererSummaryEl.style.display = app.rendererSectionOpen ? 'none' : 'block';
  }
  if (rendererSectionToggleBtnEl) {
    rendererSectionToggleBtnEl.textContent = app.rendererSectionOpen ? '▾' : '▸';
  }
  notifyOverlayLayoutChanged('renderer-section-toggle');
}

export function collapseRuntimeSections() {
  setTelemetryGaugesOpen(false);
  setDisplaySectionOpen(false);
  setAudioOutputSectionOpen(false);
  setInputSectionOpen(false);
  setRendererSectionOpen(false);
}
