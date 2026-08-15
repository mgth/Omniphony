/**
 * Modal dialog and accordion section toggle helpers.
 */

import { app } from './state.js';
import { t } from './i18n.js';
import { emitOverlayLayoutChanged } from './ui/layout/overlay-layout-state.js';

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
function getEvaluationInfoModalEl() { return document.getElementById('evaluationInfoModal'); }
function getBackendInfoModalEl() { return document.getElementById('backendInfoModal'); }
function getDistanceModelInfoModalEl() { return document.getElementById('distanceModelInfoModal'); }
function getInputInfoModalEl() { return document.getElementById('inputInfoModal'); }
function getInputClockInfoModalEl() { return document.getElementById('inputClockInfoModal'); }
function getInputLfeInfoModalEl() { return document.getElementById('inputLfeInfoModal'); }
function getDrcInfoModalEl() { return document.getElementById('drcInfoModal'); }
function getHeatmapInfoModalEl() { return document.getElementById('heatmapInfoModal'); }
function getTelemetryGaugesFormEl() { return document.getElementById('telemetryGaugesForm'); }
function getTelemetryGaugesToggleBtnEl() { return document.getElementById('telemetryGaugesToggleBtn'); }
function getDisplaySectionContentEl() { return document.getElementById('displaySectionContent'); }
function getDisplaySectionToggleBtnEl() { return document.getElementById('displaySectionToggleBtn'); }
function getDisplaySectionEl() { return document.getElementById('displaySection'); }
function getDrcSectionContentEl() { return document.getElementById('drcSectionContent'); }
function getDrcSummaryEl() { return document.getElementById('drcSummary'); }
function getDrcSectionToggleBtnEl() { return document.getElementById('drcSectionToggleBtn'); }
function getDrcSectionEl() { return document.getElementById('drcSection'); }
function getTwoDSourcesPanelRootEl() { return document.getElementById('twoDSourcesPanelRoot'); }
function getTwoDSourcesBodyEl() { return document.getElementById('twoDSourcesBody'); }
function getTwoDSourcesSummaryEl() { return document.getElementById('twoDSourcesSummary'); }
function getTwoDSourcesToggleBtnEl() { return document.getElementById('twoDSourcesToggleBtn'); }
function getAudioOutputSectionContentEl() { return document.getElementById('audioOutputSectionContent'); }
function getAudioOutputSummaryEl() { return document.getElementById('audioOutputSummary'); }
function getAudioOutputSectionToggleBtnEl() { return document.getElementById('audioOutputSectionToggleBtn'); }
function getAudioOutputSectionEl() { return document.getElementById('audioOutputSection'); }
function getInputSectionContentEl() { return document.getElementById('inputSectionContent'); }
function getInputSummaryEl() { return document.getElementById('inputSummary'); }
function getInputSectionToggleBtnEl() { return document.getElementById('inputSectionToggleBtn'); }
function getInputSectionEl() { return document.getElementById('audioInputSection'); }
function getRendererSectionContentEl() { return document.getElementById('rendererSectionContent'); }
function getRendererSummaryEl() { return document.getElementById('rendererSummary'); }
function getRendererSectionToggleBtnEl() { return document.getElementById('rendererSectionToggleBtn'); }
function getRendererSectionEl() { return document.getElementById('rendererSection'); }
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


export function setEvaluationInfoModalOpen(open) {
  const evaluationInfoModalEl = getEvaluationInfoModalEl();
  if (!evaluationInfoModalEl) return;
  evaluationInfoModalEl.classList.toggle('open', Boolean(open));
}

export function setBackendInfoModalOpen(open) {
  const backendInfoModalEl = getBackendInfoModalEl();
  if (!backendInfoModalEl) return;
  // Compose the body on open: a generic intro that doesn't go stale plus a blurb
  // for the currently-selected backend (keyed by its id). Backends without a
  // specific blurb (e.g. third-party ones) just show the intro.
  if (open) {
    const bodyEl = document.getElementById('backendInfoBody');
    if (bodyEl) {
      const state = app.renderBackendState || {};
      const id = String(state.selection || state.effective || '').trim();
      const list = Array.isArray(state.availableBackends) ? state.availableBackends : [];
      const entry = list.find((b) => b && b.id === id);
      const label = (entry && entry.label) || id;
      let html = t('backend.infoBody');
      const key = id ? `help.backend.${id}` : '';
      const specific = key ? t(key) : '';
      if (id && specific && specific !== key) {
        html += `<br /><br /><strong>${label}</strong><br />${specific}`;
      }
      bodyEl.innerHTML = html;
    }
  }
  backendInfoModalEl.classList.toggle('open', Boolean(open));
}

export function setDistanceModelInfoModalOpen(open) {
  const distanceModelInfoModalEl = getDistanceModelInfoModalEl();
  if (!distanceModelInfoModalEl) return;
  distanceModelInfoModalEl.classList.toggle('open', Boolean(open));
}

export function setInputInfoModalOpen(open) {
  const inputInfoModalEl = getInputInfoModalEl();
  if (!inputInfoModalEl) return;
  inputInfoModalEl.classList.toggle('open', Boolean(open));
}

export function setInputClockInfoModalOpen(open) {
  const inputClockInfoModalEl = getInputClockInfoModalEl();
  if (!inputClockInfoModalEl) return;
  inputClockInfoModalEl.classList.toggle('open', Boolean(open));
}

export function setInputLfeInfoModalOpen(open) {
  const inputLfeInfoModalEl = getInputLfeInfoModalEl();
  if (!inputLfeInfoModalEl) return;
  inputLfeInfoModalEl.classList.toggle('open', Boolean(open));
}

export function setDrcInfoModalOpen(open) {
  const drcInfoModalEl = getDrcInfoModalEl();
  if (!drcInfoModalEl) return;
  drcInfoModalEl.classList.toggle('open', Boolean(open));
}

export function setHeatmapInfoModalOpen(open) {
  const heatmapInfoModalEl = getHeatmapInfoModalEl();
  if (!heatmapInfoModalEl) return;
  heatmapInfoModalEl.classList.toggle('open', Boolean(open));
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
  emitOverlayLayoutChanged('telemetry-gauges-toggle');
}

export function setDisplaySectionOpen(open) {
  const displaySectionContentEl = getDisplaySectionContentEl();
  const displaySectionToggleBtnEl = getDisplaySectionToggleBtnEl();
  const displaySectionEl = getDisplaySectionEl();
  app.displaySectionOpen = Boolean(open);
  if (displaySectionContentEl) {
    displaySectionContentEl.classList.toggle('open', app.displaySectionOpen);
  }
  if (displaySectionToggleBtnEl) {
    displaySectionToggleBtnEl.textContent = app.displaySectionOpen ? '▾' : '▸';
  }
  if (displaySectionEl) {
    displaySectionEl.classList.toggle('section-collapsed', !app.displaySectionOpen);
  }
  emitOverlayLayoutChanged('display-section-toggle');
}

export function setDrcSectionOpen(open) {
  const drcSectionContentEl = getDrcSectionContentEl();
  const drcSummaryEl = getDrcSummaryEl();
  const drcSectionToggleBtnEl = getDrcSectionToggleBtnEl();
  const drcSectionEl = getDrcSectionEl();
  app.drcSectionOpen = Boolean(open);
  if (drcSectionContentEl) {
    drcSectionContentEl.classList.toggle('open', app.drcSectionOpen);
  }
  if (drcSummaryEl) {
    drcSummaryEl.style.display = app.drcSectionOpen ? 'none' : 'block';
  }
  if (drcSectionToggleBtnEl) {
    drcSectionToggleBtnEl.textContent = app.drcSectionOpen ? '▾' : '▸';
  }
  if (drcSectionEl) {
    drcSectionEl.classList.toggle('section-collapsed', !app.drcSectionOpen);
  }
  emitOverlayLayoutChanged('drc-section-toggle');
}

export function setTwoDSourcesSectionOpen(open) {
  const rootEl = getTwoDSourcesPanelRootEl();
  const bodyEl = getTwoDSourcesBodyEl();
  const summaryEl = getTwoDSourcesSummaryEl();
  const toggleBtnEl = getTwoDSourcesToggleBtnEl();
  app.twoDSourcesSectionOpen = Boolean(open);
  if (rootEl) {
    rootEl.classList.toggle('section-collapsed', !app.twoDSourcesSectionOpen);
  }
  if (bodyEl) {
    bodyEl.style.display = app.twoDSourcesSectionOpen ? 'grid' : 'none';
  }
  if (summaryEl) {
    summaryEl.style.display = app.twoDSourcesSectionOpen ? 'none' : 'block';
  }
  if (toggleBtnEl) {
    toggleBtnEl.textContent = app.twoDSourcesSectionOpen ? '▾' : '▸';
  }
  emitOverlayLayoutChanged('twoD-sources-toggle');
}

export function setAudioOutputSectionOpen(open) {
  const audioOutputSectionContentEl = getAudioOutputSectionContentEl();
  const audioOutputSummaryEl = getAudioOutputSummaryEl();
  const audioOutputSectionToggleBtnEl = getAudioOutputSectionToggleBtnEl();
  const audioOutputSectionEl = getAudioOutputSectionEl();
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
  if (audioOutputSectionEl) {
    audioOutputSectionEl.classList.toggle('section-collapsed', !app.audioOutputSectionOpen);
  }
  emitOverlayLayoutChanged('audio-output-section-toggle');
}

export function setAutoGainSectionOpen(open) {
  app.autoGainSectionOpen = Boolean(open);
  const content = document.getElementById('autoGainSection');
  const toggleBtn = document.getElementById('autoGainSectionToggleBtn');
  if (content) {
    content.classList.toggle('open', app.autoGainSectionOpen);
  }
  if (toggleBtn) {
    toggleBtn.textContent = app.autoGainSectionOpen ? '▾' : '▸';
  }
  emitOverlayLayoutChanged('auto-gain-section-toggle');
}

export function setInputSectionOpen(open) {
  const inputSectionContentEl = getInputSectionContentEl();
  const inputSummaryEl = getInputSummaryEl();
  const inputSectionToggleBtnEl = getInputSectionToggleBtnEl();
  const inputSectionEl = getInputSectionEl();
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
  if (inputSectionEl) {
    inputSectionEl.classList.toggle('section-collapsed', !app.inputSectionOpen);
  }
  emitOverlayLayoutChanged('input-section-toggle');
}

export function setRendererSectionOpen(open) {
  const rendererSectionContentEl = getRendererSectionContentEl();
  const rendererSummaryEl = getRendererSummaryEl();
  const rendererSectionToggleBtnEl = getRendererSectionToggleBtnEl();
  const rendererSectionEl = getRendererSectionEl();
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
  if (rendererSectionEl) {
    rendererSectionEl.classList.toggle('section-collapsed', !app.rendererSectionOpen);
  }
  emitOverlayLayoutChanged('renderer-section-toggle');
}

export function collapseRuntimeSections() {
  setTelemetryGaugesOpen(false);
  setDisplaySectionOpen(false);
  setDrcSectionOpen(false);
  setAudioOutputSectionOpen(false);
  setInputSectionOpen(false);
  setRendererSectionOpen(false);
}
