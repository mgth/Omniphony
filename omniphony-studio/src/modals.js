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

function getTrailInfoModalEl() { return document.getElementById('trailInfoModal'); }
function getEffectiveRenderInfoModalEl() { return document.getElementById('effectiveRenderInfoModal'); }
function getOscInfoModalEl() { return document.getElementById('oscInfoModal'); }
function getAboutModalEl() { return document.getElementById('aboutModal'); }
function getRoomGeometryInfoModalEl() { return document.getElementById('roomGeometryInfoModal'); }
function getAdaptiveResamplingInfoModalEl() { return document.getElementById('adaptiveResamplingInfoModal'); }
function getTelemetryGaugesInfoModalEl() { return document.getElementById('telemetryGaugesInfoModal'); }
function getRampModeInfoModalEl() { return document.getElementById('rampModeInfoModal'); }
function getVbapPositionInterpolationInfoModalEl() { return document.getElementById('vbapPositionInterpolationInfoModal'); }
function getTelemetryGaugesFormEl() { return document.getElementById('telemetryGaugesForm'); }
function getTelemetryGaugesToggleBtnEl() { return document.getElementById('telemetryGaugesToggleBtn'); }
function getDisplaySectionContentEl() { return document.getElementById('displaySectionContent'); }
function getDisplaySectionToggleBtnEl() { return document.getElementById('displaySectionToggleBtn'); }
function getAudioOutputSectionContentEl() { return document.getElementById('audioOutputSectionContent'); }
function getAudioOutputSummaryEl() { return document.getElementById('audioOutputSummary'); }
function getAudioOutputSectionToggleBtnEl() { return document.getElementById('audioOutputSectionToggleBtn'); }
function getInputSectionContentEl() { return document.getElementById('inputSectionContent'); }
function getInputSummaryEl() { return document.getElementById('inputSummary'); }
function getInputSectionToggleBtnEl() { return document.getElementById('inputSectionToggleBtn'); }
function getRendererSectionContentEl() { return document.getElementById('rendererSectionContent'); }
function getRendererSummaryEl() { return document.getElementById('rendererSummary'); }
function getRendererSectionToggleBtnEl() { return document.getElementById('rendererSectionToggleBtn'); }
function getSpreadFromDistanceInfoModalEl() { return document.getElementById('spreadFromDistanceInfoModal'); }
function getDistanceDiffuseInfoModalEl() { return document.getElementById('distanceDiffuseInfoModal'); }

// ---------------------------------------------------------------------------
// Simple info modals
// ---------------------------------------------------------------------------

export function setTrailInfoModalOpen(open) {
  const trailInfoModalEl = getTrailInfoModalEl();
  if (!trailInfoModalEl) return;
  trailInfoModalEl.classList.toggle('open', Boolean(open));
}

export function setEffectiveRenderInfoModalOpen(open) {
  const effectiveRenderInfoModalEl = getEffectiveRenderInfoModalEl();
  if (!effectiveRenderInfoModalEl) return;
  effectiveRenderInfoModalEl.classList.toggle('open', Boolean(open));
}

export function setOscInfoModalOpen(open) {
  const oscInfoModalEl = getOscInfoModalEl();
  if (!oscInfoModalEl) return;
  oscInfoModalEl.classList.toggle('open', Boolean(open));
}

export function setAboutModalOpen(open) {
  const aboutModalEl = getAboutModalEl();
  if (!aboutModalEl) return;
  aboutModalEl.classList.toggle('open', Boolean(open));
}

export function setRoomGeometryInfoModalOpen(open) {
  const roomGeometryInfoModalEl = getRoomGeometryInfoModalEl();
  if (!roomGeometryInfoModalEl) return;
  roomGeometryInfoModalEl.classList.toggle('open', Boolean(open));
}

export function setAdaptiveResamplingInfoModalOpen(open) {
  const adaptiveResamplingInfoModalEl = getAdaptiveResamplingInfoModalEl();
  if (!adaptiveResamplingInfoModalEl) return;
  adaptiveResamplingInfoModalEl.classList.toggle('open', Boolean(open));
}

export function setTelemetryGaugesInfoModalOpen(open) {
  const telemetryGaugesInfoModalEl = getTelemetryGaugesInfoModalEl();
  if (!telemetryGaugesInfoModalEl) return;
  telemetryGaugesInfoModalEl.classList.toggle('open', Boolean(open));
}

export function setRampModeInfoModalOpen(open) {
  const rampModeInfoModalEl = getRampModeInfoModalEl();
  if (!rampModeInfoModalEl) return;
  rampModeInfoModalEl.classList.toggle('open', Boolean(open));
}

export function setVbapPositionInterpolationInfoModalOpen(open) {
  const vbapPositionInterpolationInfoModalEl = getVbapPositionInterpolationInfoModalEl();
  if (!vbapPositionInterpolationInfoModalEl) return;
  vbapPositionInterpolationInfoModalEl.classList.toggle('open', Boolean(open));
}

export function setSpreadFromDistanceInfoModalOpen(open) {
  const spreadFromDistanceInfoModalEl = getSpreadFromDistanceInfoModalEl();
  if (!spreadFromDistanceInfoModalEl) return;
  spreadFromDistanceInfoModalEl.classList.toggle('open', Boolean(open));
}

export function setDistanceDiffuseInfoModalOpen(open) {
  const distanceDiffuseInfoModalEl = getDistanceDiffuseInfoModalEl();
  if (!distanceDiffuseInfoModalEl) return;
  distanceDiffuseInfoModalEl.classList.toggle('open', Boolean(open));
}

// ---------------------------------------------------------------------------
// Accordion sections (update shared state)
// ---------------------------------------------------------------------------

export function setTelemetryGaugesOpen(open) {
  const telemetryGaugesFormEl = getTelemetryGaugesFormEl();
  const telemetryGaugesToggleBtnEl = getTelemetryGaugesToggleBtnEl();
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
  const displaySectionContentEl = getDisplaySectionContentEl();
  const displaySectionToggleBtnEl = getDisplaySectionToggleBtnEl();
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
  const audioOutputSectionContentEl = getAudioOutputSectionContentEl();
  const audioOutputSummaryEl = getAudioOutputSummaryEl();
  const audioOutputSectionToggleBtnEl = getAudioOutputSectionToggleBtnEl();
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
  const inputSectionContentEl = getInputSectionContentEl();
  const inputSummaryEl = getInputSummaryEl();
  const inputSectionToggleBtnEl = getInputSectionToggleBtnEl();
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
  const rendererSectionContentEl = getRendererSectionContentEl();
  const rendererSummaryEl = getRendererSummaryEl();
  const rendererSectionToggleBtnEl = getRendererSectionToggleBtnEl();
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
