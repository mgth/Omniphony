/**
 * Speaker management — serialization, layout editing, delay computation,
 * speaker/object list UI, gizmo updates, room face visibility, and level metering.
 *
 * Extracted from app.js.
 */

import { invoke } from '@tauri-apps/api/core';
import * as THREE from 'three';

import {
  app,
  speakerMeshes,
  speakerLabels,
  speakerItems,
  objectItems,
  speakerLevels,
  speakerLevelLastSeen,
  sourceLevels,
  sourceLevelLastSeen,
  sourceMeshes,
  sourceGains,
  sourceBandGains,
  sourceNames,
  sourcePositionsRaw,
  sourceTrails,
  speakerGainCache,
  speakerBaseGains,
  speakerDelays,
  speakerMuted,
  objectMuted,
  speakerManualMuted,
  objectManualMuted,
  layoutsByKey,
  speakerReorderAnimations,
  dirty,
  dirtySpeakerMeters,
  dirtyObjectMeters,
  METER_DECAY_START_MS,
  METER_DECAY_DB_PER_SEC,
  DEFAULT_SAMPLE_RATE_HZ
} from './state.js';
import { isSpeakerLayoutFrozen } from './state.js';

import {
  hydrateSpeakerCoordinateState,
  normalizedOmniphonyToScenePosition,
  scenePositionToNormalizedOmniphony,
  cartesianToSpherical,
  sphericalToCartesianDeg,
  clampNumber,
  normalizeAngleDeg,
  formatNumber,
  formatPosition,
  getSpeakerCoordMode,
  getSpeakerSpatializeValue,
  getSpeakerBaseOpacity
} from './coordinates.js';

import {
  scene,
  camera,
  controls,
  roomGroup,
  roomFaceDefs,
  roomBounds,
  sceneState,
  tempCameraLocal,
  tempToCamera,
  tempToCenter,
  screenMaterial
} from './scene/setup.js';

import {
  speakerGeometry,
  speakerMaterial,
  speakerBaseColor,
  speakerHotColor,
  speakerSelectedColor,
  sourceMaterial,
  sourceOutlineColor,
  sourceHotColor,
  sourceSelectedEmissive,
  sourceContributionEmissive,
  sourceNeutralEmissive,
  sourceDefaultEmissive
} from './scene/materials.js';

import { createLabelSprite, setLabelSpriteText, updateSpeakerLabelsFromSelection } from './scene/labels.js';
import { requestSpeakerHeatmapIfNeeded, refreshSpeakerHeatmapScene } from './scene/speaker-heatmap.js';

import {
  speakerGizmo,
  distanceGizmo,
  cartesianGizmo,
  selectedSpeakerShadows,
  selectedObjectShadows,
  syncVbapCartesianFaceGridVisibility,
  ringLabelAngles,
  arcLabelAngles
} from './scene/gizmos.js';

import { t, tf } from './i18n.js';
import { pushLog } from './log.js';
import { scheduleUIFlush } from './flush.js';
import { updateItemClasses, updateSpeakerMeterUI, updateObjectMeterUI } from './flush.js';
import { computeCrossoverBandLabels } from './crossover-bands.js';

import {
  linearToDb,
  meterToPercent,
  formatLevel,
  getBaseGain,
  getSoloTarget,
  toggleMute,
  toggleSolo,
  sendObjectMute,
  sendSpeakerMute,
  updateMeterUI
} from './mute-solo.js';

import {
  applySpeakerLevel,
  applySourceLevel,
  clearSpeakers,
  updateSpeakerColorsFromSelection,
  updateSourceSelectionStyles,
  setSelectedSource,
  getSelectedSourceGains,
  getSelectedSourceContribution,
  getSelectedSpeakerContributionForObject,
  updateSpeakerContributionUI as updateSpeakerContributionUI_src,
  updateObjectContributionUI as updateObjectContributionUI_src,
  updateEffectiveRenderDecoration,
  getObjectDisplayName,
  applyObjectItemColor,
  dbfsToScale,
  gainToMix,
  getSelectedSourceBandContributions
} from './sources.js';

// ---------------------------------------------------------------------------
// DOM references
// ---------------------------------------------------------------------------

function getSpeakersListEl() { return document.getElementById('speakersList'); }
function getObjectsListEl() { return document.getElementById('objectsList'); }
function getSpeakersSectionEl() { return document.getElementById('speakersSection'); }
function getSpeakerEditSectionEl() { return document.getElementById('speakerEditSection'); }
function getSpeakerEditBodyEl() { return document.getElementById('speakerEditBody'); }
function getSpeakerEditTitleEl() { return document.getElementById('speakerEditTitle'); }
function getSpeakerEditNameInputEl() { return document.getElementById('speakerEditNameInput'); }
function getSpeakerEditXInputEl() { return document.getElementById('speakerEditXInput'); }
function getSpeakerEditYInputEl() { return document.getElementById('speakerEditYInput'); }
function getSpeakerEditZInputEl() { return document.getElementById('speakerEditZInput'); }
function getSpeakerEditCartesianModeEl() { return document.getElementById('speakerEditCartesianMode'); }
function getSpeakerEditAzInputEl() { return document.getElementById('speakerEditAzInput'); }
function getSpeakerEditElInputEl() { return document.getElementById('speakerEditElInput'); }
function getSpeakerEditRInputEl() { return document.getElementById('speakerEditRInput'); }
function getSpeakerEditPolarModeEl() { return document.getElementById('speakerEditPolarMode'); }
function getSpeakerEditCartesianGizmoBtnEl() { return document.getElementById('speakerEditCartesianGizmoBtn'); }
function getSpeakerEditPolarGizmoBtnEl() { return document.getElementById('speakerEditPolarGizmoBtn'); }
function getSpeakerEditGainSliderEl() { return document.getElementById('speakerEditGainSlider'); }
function getSpeakerEditGainBoxEl() { return document.getElementById('speakerEditGainBox'); }
function getSpeakerEditDelayMsInputEl() { return document.getElementById('speakerEditDelayMsInput'); }
function getSpeakerEditDelaySamplesInputEl() { return document.getElementById('speakerEditDelaySamplesInput'); }
function getSpeakerEditAutoDelayBtnEl() { return document.getElementById('speakerEditAutoDelayBtn'); }
function getSpeakerEditDelayToDistanceBtnEl() { return document.getElementById('speakerEditDelayToDistanceBtn'); }
function getSpeakerEditSpatializeToggleEl() { return document.getElementById('speakerEditSpatializeToggle'); }
function getSpeakerEditFreqLowInputEl() { return document.getElementById('speakerEditFreqLowInput'); }
function getSpeakerEditFreqHighInputEl() { return document.getElementById('speakerEditFreqHighInput'); }
function getSpeakerAddBtnEl() { return document.getElementById('speakerAddBtn'); }
function getSpeakerMoveUpBtnEl() { return document.getElementById('speakerMoveUpBtn'); }
function getSpeakerMoveDownBtnEl() { return document.getElementById('speakerMoveDownBtn'); }
function getSpeakerRemoveBtnEl() { return document.getElementById('speakerRemoveBtn'); }
function getObjectsSectionEl() { return document.getElementById('objectsSection'); }

// ---------------------------------------------------------------------------
// Local aliases for app state
// ---------------------------------------------------------------------------

function get_selectedSourceId() { return app.selectedSourceId; }
function get_selectedSpeakerIndex() { return app.selectedSpeakerIndex; }
function set_selectedSpeakerIndex(v) { app.selectedSpeakerIndex = v; }
function get_currentLayoutKey() { return app.currentLayoutKey; }
function set_currentLayoutKey(v) { app.currentLayoutKey = v; }
function get_currentLayoutSpeakers() { return app.currentLayoutSpeakers; }
function set_currentLayoutSpeakers(v) { app.currentLayoutSpeakers = v; }

function syncInputValueUnlessEditing(inputEl, nextValue) {
  if (!inputEl) return;
  if (document.activeElement === inputEl) return;
  if (inputEl.value !== nextValue) {
    inputEl.value = nextValue;
  }
}

// ---------------------------------------------------------------------------
// Speaker serialization / export
// ---------------------------------------------------------------------------

export { getSpeakerSpatializeValue, getSpeakerBaseOpacity };

export function defaultLayoutExportNameFromSpeakers(speakers) {
  let a = 0;
  let b = 0;
  let c = 0;
  for (const speaker of speakers || []) {
    const spatialized = getSpeakerSpatializeValue(speaker) !== 0;
    if (!spatialized) {
      b += 1;
      continue;
    }
    const y = Number(speaker?.y);
    if (Number.isFinite(y) && y > 0.5) {
      c += 1;
    } else {
      a += 1;
    }
  }
  return `${a}.${b}.${c}`;
}

export function sanitizeLayoutExportName(name) {
  const sanitized = String(name ?? '')
    .trim()
    .split('')
    .map((ch) => (/^[A-Za-z0-9._-]$/.test(ch) ? ch : '_'))
    .join('');
  const trimmed = sanitized.replace(/^\.+|\.+$/g, '');
  return trimmed || 'layout';
}

export function serializeSpeakerForExport(speaker, index) {
  hydrateSpeakerCoordinateState(speaker);
  return {
    id: String(speaker?.id ?? `spk-${index}`),
    x: clampNumber(Number(speaker?.x) || 0, -1, 1),
    y: clampNumber(Number(speaker?.y) || 0, -1, 1),
    z: clampNumber(Number(speaker?.z) || 0, -1, 1),
    azimuthDeg: Number.isFinite(Number(speaker?.azimuthDeg)) ? Number(speaker.azimuthDeg) : 0,
    elevationDeg: Number.isFinite(Number(speaker?.elevationDeg)) ? Number(speaker.elevationDeg) : 0,
    distanceM: Math.max(0.01, Number(speaker?.distanceM) || 1),
    coordMode: getSpeakerCoordMode(speaker),
    spatialize: getSpeakerSpatializeValue(speaker),
    delay_ms: Math.max(0, Number(speaker?.delay_ms) || 0),
    freqLow: Number.isFinite(Number(speaker?.freqLow)) && Number(speaker.freqLow) > 0 ? Number(speaker.freqLow) : null,
    freqHigh: Number.isFinite(Number(speaker?.freqHigh)) && Number(speaker.freqHigh) > 0 ? Number(speaker.freqHigh) : null
  };
}

export function serializeCurrentLayoutForExport() {
  const layout = currentLayoutRef();
  if (!layout) return null;
  const currentLayoutSpeakers = get_currentLayoutSpeakers();
  return {
    key: String(layout.key || 'layout'),
    name: String(layout.name || layout.key || 'layout'),
    radius_m: Math.max(0.01, Number(layout.radius_m) || Number(sceneState.metersPerUnit) || 1),
    speakers: currentLayoutSpeakers.map((speaker, index) => serializeSpeakerForExport(speaker, index))
  };
}

// ---------------------------------------------------------------------------
// Delay / distance utilities
// ---------------------------------------------------------------------------

export function delayMsToSamples(ms, sampleRateHz = DEFAULT_SAMPLE_RATE_HZ) {
  const msValue = Number(ms);
  if (!Number.isFinite(msValue) || msValue < 0) {
    return 0;
  }
  return Math.round((msValue / 1000) * sampleRateHz);
}

export function samplesToDelayMs(samples, sampleRateHz = DEFAULT_SAMPLE_RATE_HZ) {
  const sampleValue = Number(samples);
  if (!Number.isFinite(sampleValue) || sampleValue < 0) {
    return 0;
  }
  return (sampleValue * 1000) / sampleRateHz;
}

export function distanceMetersFromSpeaker(speaker) {
  if (!speaker) return 0;
  const distance = Number(speaker.distanceM);
  if (Number.isFinite(distance)) return Math.max(0, distance);
  return 0;
}

export function computeAndApplySpeakerDelays() {
  const currentLayoutSpeakers = get_currentLayoutSpeakers();
  if (!currentLayoutSpeakers.length) return;
  const SPEED_OF_SOUND_M_S = 343.0;
  const scale = Math.max(0.01, Number(sceneState.metersPerUnit) || 1.0);
  const distances = currentLayoutSpeakers.map((speaker) => distanceMetersFromSpeaker(speaker) * scale);
  const maxDistance = distances.reduce((acc, d) => Math.max(acc, d), 0);

  distances.forEach((distance, index) => {
    const delayMs = Math.max(0, ((maxDistance - distance) / SPEED_OF_SOUND_M_S) * 1000);
    const rounded = Math.round(delayMs * 1000) / 1000;
    const id = String(index);
    speakerDelays.set(id, rounded);
    invoke('control_speaker_delay', { id: index, delayMs: rounded });
  });

  renderSpeakerEditor();
}

export function adjustSpeakerDistancesFromDelays() {
  const currentLayoutSpeakers = get_currentLayoutSpeakers();
  if (!currentLayoutSpeakers.length) return;
  const SPEED_OF_SOUND_M_S = 343.0;
  const scale = Math.max(0.01, Number(sceneState.metersPerUnit) || 1.0);
  const currentDistancesM = currentLayoutSpeakers.map((speaker) => distanceMetersFromSpeaker(speaker) * scale);
  const referenceMaxM = currentDistancesM.reduce((acc, d) => Math.max(acc, d), 0.01);

  currentLayoutSpeakers.forEach((speaker, index) => {
    const id = String(index);
    const delayMs = Math.max(0, Number(speakerDelays.get(id) ?? speaker.delay_ms ?? 0));
    const deltaM = (delayMs / 1000) * SPEED_OF_SOUND_M_S;
    const targetDistanceUnits = Math.max(0.01, (referenceMaxM - deltaM) / scale);

    const x = Number(speaker.x) || 0;
    const y = Number(speaker.y) || 0;
    const z = Number(speaker.z) || 0;
    const norm = Math.sqrt(x * x + y * y + z * z);
    const dirX = norm > 1e-6 ? x / norm : 1;
    const dirY = norm > 1e-6 ? y / norm : 0;
    const dirZ = norm > 1e-6 ? z / norm : 0;

    applySpeakerCartesianEdit(
      index,
      dirX * targetDistanceUnits,
      dirY * targetDistanceUnits,
      dirZ * targetDistanceUnits,
      false
    );
  });

  currentLayoutSpeakers.forEach((speaker, index) => {
    invoke('control_speaker_az', { id: index, value: Number(speaker.azimuthDeg) || 0 });
    invoke('control_speaker_el', { id: index, value: Number(speaker.elevationDeg) || 0 });
    invoke('control_speaker_distance', { id: index, value: Number(speaker.distanceM) || 1 });
  });
  invoke('control_speakers_apply');
  renderSpeakerEditor();
}

// ---------------------------------------------------------------------------
// Coord mode
// ---------------------------------------------------------------------------

export function setSpeakerCoordMode(index, mode) {
  if (isSpeakerLayoutFrozen()) return;
  const currentLayoutSpeakers = get_currentLayoutSpeakers();
  const speaker = currentLayoutSpeakers[index];
  if (!speaker) return;
  speaker.coordMode = mode === 'cartesian' ? 'cartesian' : 'polar';
  hydrateSpeakerCoordinateState(speaker);
  invoke('control_speaker_coord_mode', { id: index, value: speaker.coordMode });
  invoke('control_speaker_x', { id: index, value: speaker.x });
  invoke('control_speaker_y', { id: index, value: speaker.y });
  invoke('control_speaker_z', { id: index, value: speaker.z });
  invoke('control_speaker_az', { id: index, value: speaker.azimuthDeg });
  invoke('control_speaker_el', { id: index, value: speaker.elevationDeg });
  invoke('control_speaker_distance', { id: index, value: speaker.distanceM });
  invoke('control_speakers_apply');
  updateSpeakerVisualsFromState(index);
  renderSpeakerEditor();
}

// ---------------------------------------------------------------------------
// Controls UI update
// ---------------------------------------------------------------------------

export function updateSpeakerControlsUI() {
  const selectedSpeakerIndex = get_selectedSpeakerIndex();
  const soloTarget = getSoloTarget('speaker');
  speakerItems.forEach((entry, id) => {
    entry.muteBtn.classList.toggle('active', speakerMuted.has(id));
    entry.soloBtn.classList.toggle('active', soloTarget === id);
    updateItemClasses(entry, speakerMuted.has(id), soloTarget && soloTarget !== id);
    entry.root.classList.toggle('is-selected', selectedSpeakerIndex !== null && Number(id) === selectedSpeakerIndex);
    updateSpeakerContributionUI_src(entry, id);
  });
  renderSpeakerEditor();
}

export function updateObjectControlsUI() {
  const selectedSourceId = get_selectedSourceId();
  const soloTarget = getSoloTarget('object');
  objectItems.forEach((entry, id) => {
    const metadataSilent = objectHasSilentMetadataGain(id);
    entry.muteBtn.classList.toggle('active', objectMuted.has(id));
    entry.soloBtn.classList.toggle('active', soloTarget === id);
    updateItemClasses(entry, objectMuted.has(id), Boolean((soloTarget && soloTarget !== id) || metadataSilent));
    entry.root.classList.toggle('is-selected', selectedSourceId === id);
    entry.root.classList.toggle('has-active-trail', objectHasActiveTrail(id));
    if (entry.topRight) {
      entry.topRight.textContent = getObjectDominantSpeakerText(id);
    }
    updateObjectContributionUI_src(entry, id);
  });
  speakerItems.forEach((entry, id) => {
    updateSpeakerContributionUI_src(entry, id);
    updateSpeakerBandBars(entry, Number(id));
  });
}

export function updateObjectDominantSpeakerUI(id) {
  const entry = objectItems.get(String(id));
  if (entry?.topRight) {
    entry.topRight.textContent = getObjectDominantSpeakerText(id);
  }
}

// ---------------------------------------------------------------------------
// Object helpers (dominant speaker, trail detection)
// ---------------------------------------------------------------------------

export function getObjectDominantSpeakerText(id) {
  const currentLayoutSpeakers = get_currentLayoutSpeakers();
  const key = String(id);
  const selectedBandIndex = Math.max(0, Math.round(Number(app.speakerHeatmapBandIndex) || 0));
  const bandGains = sourceBandGains.get(key);
  const gains = Array.isArray(bandGains?.[selectedBandIndex]) && bandGains[selectedBandIndex].length > 0
    ? bandGains[selectedBandIndex]
    : sourceGains.get(key);
  if (!Array.isArray(gains) || gains.length === 0) {
    return '\u2014';
  }
  let bestIndex = -1;
  let bestGain = -Infinity;
  gains.forEach((rawGain, index) => {
    const gain = Number(rawGain);
    if (!Number.isFinite(gain) || gain <= bestGain) {
      return;
    }
    bestGain = gain;
    bestIndex = index;
  });
  if (bestIndex < 0 || bestGain <= 0) {
    return '\u2014';
  }
  const speaker = currentLayoutSpeakers[bestIndex];
  const name = String(speaker?.id ?? bestIndex);
  return `${name} ${linearToDb(bestGain)}`;
}

export function objectHasActiveTrail(id) {
  const trail = sourceTrails.get(String(id));
  return Boolean(trail && trail.positions.length > 0);
}

function objectHasSilentMetadataGain(id) {
  const raw = sourcePositionsRaw.get(String(id));
  const metadataGainDb = Number(raw?.metadataGainDb);
  return Number.isFinite(metadataGainDb) && metadataGainDb <= -128;
}

// ---------------------------------------------------------------------------
// Speaker list item creation / update
// ---------------------------------------------------------------------------

export function createSpeakerItem(id, speaker) {
  const root = document.createElement('div');
  root.className = 'info-item speaker-item';
  root.addEventListener('click', () => {
    setSelectedSource(null);
    setSelectedSpeaker(Number(id));
  });
  root.addEventListener('dragover', (event) => {
    const speakersListEl = getSpeakersListEl();
    if (app.draggedSpeakerIndex === null || !app.draggedSpeakerRoot || !speakersListEl) return;
    event.preventDefault();
    if (event.dataTransfer) {
      event.dataTransfer.dropEffect = 'move';
    }
    const targetIndex = Number(id);
    if (!Number.isInteger(targetIndex) || targetIndex === app.draggedSpeakerIndex) return;
    const rect = root.getBoundingClientRect();
    const insertAfter = event.clientY >= (rect.top + rect.height * 0.5);
    if (insertAfter) {
      const afterNode = root.nextSibling;
      if (afterNode !== app.draggedSpeakerRoot) {
        animateSpeakerListReorder(() => {
          speakersListEl.insertBefore(app.draggedSpeakerRoot, afterNode);
        });
      }
    } else if (root !== app.draggedSpeakerRoot) {
      animateSpeakerListReorder(() => {
        speakersListEl.insertBefore(app.draggedSpeakerRoot, root);
      });
    }
    app.draggedSpeakerIndex = Array.from(speakersListEl.querySelectorAll('.speaker-item')).indexOf(app.draggedSpeakerRoot);
    markDraggedSpeakerItem();
  });
  root.addEventListener('drop', (event) => {
    event.preventDefault();
    app.draggedSpeakerDidDrop = true;
  });

  const idStrip = document.createElement('div');
  idStrip.className = 'id-strip flip';
  idStrip.title = 'Drag to reorder';
  idStrip.draggable = true;
  idStrip.addEventListener('dragstart', (event) => {
    const idx = Number(id);
    if (!Number.isInteger(idx)) return;
    app.draggedSpeakerIndex = idx;
    app.draggedSpeakerInitialIndex = idx;
    app.draggedSpeakerDidDrop = false;
    app.draggedSpeakerRoot = root;
    markDraggedSpeakerItem();
    if (event.dataTransfer) {
      event.dataTransfer.effectAllowed = 'move';
      event.dataTransfer.setData('text/plain', String(idx));
    }
  });
  idStrip.addEventListener('dragend', () => {
    if (app.draggedSpeakerInitialIndex !== null && app.draggedSpeakerIndex !== null) {
      if (app.draggedSpeakerDidDrop) {
        if (app.draggedSpeakerInitialIndex !== app.draggedSpeakerIndex) {
          invoke('control_speakers_move', { from: app.draggedSpeakerInitialIndex, to: app.draggedSpeakerIndex });
          requestMoveSpeakerTo(app.draggedSpeakerInitialIndex, app.draggedSpeakerIndex, false);
        }
      } else {
        // Drag cancelled: restore current logical order.
        renderSpeakersList();
      }
    }
    app.draggedSpeakerIndex = null;
    app.draggedSpeakerInitialIndex = null;
    app.draggedSpeakerDidDrop = false;
    app.draggedSpeakerRoot = null;
    speakerItems.forEach((item) => item.root.classList.remove('is-dragging'));
  });

  const idText = document.createElement('span');
  idStrip.appendChild(idText);

  const content = document.createElement('div');
  content.className = 'speaker-content';

  const level = document.createElement('div');
  level.className = 'meter-row';
  level.classList.add('speaker-meter-row');

  const levelText = document.createElement('div');
  level.appendChild(levelText);

  const meterBar = document.createElement('div');
  meterBar.className = 'meter-bar';
  const meterFill = document.createElement('div');
  meterFill.className = 'meter-fill';
  const contributionFill = document.createElement('div');
  contributionFill.className = 'meter-fill contribution';
  meterBar.appendChild(meterFill);
  meterBar.appendChild(contributionFill);
  const controlsRow = document.createElement('div');
  controlsRow.className = 'speaker-meter-actions';

  const muteBtn = document.createElement('button');
  muteBtn.type = 'button';
  muteBtn.className = 'toggle-btn';
  muteBtn.textContent = 'M';
  muteBtn.addEventListener('click', (event) => {
    event.preventDefault();
    toggleMute('speaker', id);
  });
  controlsRow.appendChild(muteBtn);

  const soloBtn = document.createElement('button');
  soloBtn.type = 'button';
  soloBtn.className = 'toggle-btn';
  soloBtn.textContent = 'S';
  soloBtn.addEventListener('click', (event) => {
    event.preventDefault();
    toggleSolo('speaker', id);
  });
  controlsRow.appendChild(soloBtn);

  level.appendChild(meterBar);
  level.appendChild(controlsRow);
  content.appendChild(level);

  const contributionRow = document.createElement('div');
  contributionRow.className = 'speaker-contrib-row';

  const bandBarsContainer = document.createElement('div');
  bandBarsContainer.className = 'band-contrib-bars';
  bandBarsContainer.style.display = 'none';
  contributionRow.appendChild(bandBarsContainer);

  content.appendChild(contributionRow);
  root.appendChild(idStrip);
  root.appendChild(content);

  return {
    root,
    label: idText,
    levelText,
    meterFill,
    contributionFill,
    contributionRow,
    bandBarsContainer,
    muteBtn,
    soloBtn
  };
}

export function updateSpeakerItem(entry, id, speaker) {
  const selectedSpeakerIndex = get_selectedSpeakerIndex();
  const soloTarget = getSoloTarget('speaker');
  entry.label.textContent = String(speaker.id ?? id);
  entry.muteBtn.classList.toggle('active', speakerMuted.has(id));
  entry.soloBtn.classList.toggle('active', soloTarget === id);
  updateItemClasses(entry, speakerMuted.has(id), soloTarget && soloTarget !== id);
  entry.root.classList.toggle('is-selected', selectedSpeakerIndex !== null && Number(id) === selectedSpeakerIndex);
  updateMeterUI(entry, speakerLevels.get(id));
  updateSpeakerContributionUI_src(entry, id);
  updateSpeakerBandBars(entry, Number(id));
}

function getCrossoverBandLabels() {
  return computeCrossoverBandLabels(app.currentLayoutSpeakers, {
    useUnicodeGte: true,
    useUnicodeDash: true,
  });
}

export function updateSpeakerBandBars(entry, speakerIndex) {
  if (!entry?.bandBarsContainer || !entry?.contributionRow) return;
  const contributions = getSelectedSourceBandContributions(speakerIndex);
  if (!app.selectedSourceId || !contributions || contributions.length === 0) {
    entry.contributionRow.style.display = 'none';
    entry.bandBarsContainer.style.display = 'none';
    return;
  }
  entry.contributionRow.style.display = '';
  entry.bandBarsContainer.style.display = '';
  const labels = getCrossoverBandLabels();

  while (entry.bandBarsContainer.children.length < contributions.length) {
    const b = entry.bandBarsContainer.children.length;
    const row = document.createElement('div');
    row.className = 'band-row';

    const labelEl = document.createElement('span');
    labelEl.className = 'band-label';
    row.appendChild(labelEl);

    const bar = document.createElement('div');
    bar.className = 'band-bar';
    bar.dataset.band = String(b);
    row.appendChild(bar);

    const dbEl = document.createElement('span');
    dbEl.className = 'band-db';
    row.appendChild(dbEl);

    entry.bandBarsContainer.appendChild(row);
  }

  contributions.forEach((gain, b) => {
    const row = entry.bandBarsContainer.children[b];
    if (!row) return;
    const labelEl = row.querySelector('.band-label');
    const bar = row.querySelector('.band-bar');
    const dbEl = row.querySelector('.band-db');
    if (labelEl) {
      labelEl.textContent = labels?.[b] ?? (contributions.length === 1 ? 'Full band' : `Band ${b}`);
    }
    if (bar) bar.style.setProperty('--level', `${Math.min(100, gain * 100).toFixed(1)}%`);
    if (dbEl) dbEl.textContent = linearToDb(gain);
  });

  for (let b = 0; b < entry.bandBarsContainer.children.length; b += 1) {
    const row = entry.bandBarsContainer.children[b];
    if (row) row.style.display = b < contributions.length ? '' : 'none';
  }
}

export function updateAllSpeakerBandBars() {
  speakerItems.forEach((entry, speakerId) => {
    updateSpeakerBandBars(entry, Number(speakerId));
  });
}

// ---------------------------------------------------------------------------
// Speaker spatialize / visuals / edit
// ---------------------------------------------------------------------------

export function setSpeakerSpatializeLocal(index, spatialize) {
  const currentLayoutSpeakers = get_currentLayoutSpeakers();
  const speaker = currentLayoutSpeakers[index];
  if (!speaker) {
    return;
  }
  speaker.spatialize = spatialize === 0 ? 0 : 1;
  const mesh = speakerMeshes[index];
  if (mesh) {
    const baseOpacity = getSpeakerBaseOpacity(speaker);
    mesh.userData.baseOpacity = baseOpacity;
    mesh.material.opacity = baseOpacity;
  }
  updateSpeakerColorsFromSelection();
  renderSpeakerEditor();
}

export function updateSpeakerVisualsFromState(index) {
  const currentLayoutSpeakers = get_currentLayoutSpeakers();
  const selectedSpeakerIndex = get_selectedSpeakerIndex();
  const speaker = currentLayoutSpeakers[index];
  if (!speaker) return;
  hydrateSpeakerCoordinateState(speaker);
  const scenePosition = normalizedOmniphonyToScenePosition(speaker);

  const mesh = speakerMeshes[index];
  if (mesh) {
    mesh.position.set(scenePosition.x, scenePosition.y, scenePosition.z);
  }

  const label = speakerLabels[index];
  if (label) {
    label.visible = app.speakerLabelsEnabled;
    label.position.set(scenePosition.x, scenePosition.y + 0.12, scenePosition.z);
    setLabelSpriteText(label, String(speaker.id ?? index));
  }

  const entry = speakerItems.get(String(index));
  if (entry) {
    entry.label.textContent = String(speaker.id ?? index);
  }

  if (selectedSpeakerIndex === index) {
    updateSpeakerGizmo();
    refreshSpeakerHeatmapScene();
  }
}

export function applySpeakerSceneCartesianEdit(index, x, y, z, sendOsc = true) {
  if (isSpeakerLayoutFrozen()) return;
  const currentLayoutSpeakers = get_currentLayoutSpeakers();
  const speaker = currentLayoutSpeakers[index];
  if (!speaker) return;
  if (![x, y, z].every((v) => Number.isFinite(v))) return;

  const normalized = scenePositionToNormalizedOmniphony({ x, y, z });
  speaker.x = normalized.x;
  speaker.y = normalized.y;
  speaker.z = normalized.z;
  const sph = cartesianToSpherical({ x, y, z });
  speaker.azimuthDeg = sph.az;
  speaker.elevationDeg = sph.el;
  speaker.distanceM = Math.max(0.01, sph.dist);
  updateSpeakerVisualsFromState(index);

  if (sendOsc) {
    invoke('control_speaker_coord_mode', { id: index, value: getSpeakerCoordMode(speaker) });
    invoke('control_speaker_x', { id: index, value: speaker.x });
    invoke('control_speaker_y', { id: index, value: speaker.y });
    invoke('control_speaker_z', { id: index, value: speaker.z });
    invoke('control_speaker_az', { id: index, value: speaker.azimuthDeg });
    invoke('control_speaker_el', { id: index, value: speaker.elevationDeg });
    invoke('control_speaker_distance', { id: index, value: speaker.distanceM });
    invoke('control_speakers_apply');
  }

  renderSpeakerEditor();
}

export function applySpeakerCartesianEdit(index, x, y, z, sendOsc = true) {
  const scn = normalizedOmniphonyToScenePosition({ x, y, z });
  applySpeakerSceneCartesianEdit(index, scn.x, scn.y, scn.z, sendOsc);
}

export function applySpeakerPolarEdit(index, az, el, r, sendOsc = true) {
  if (![az, el, r].every((v) => Number.isFinite(v))) return;
  const currentLayoutSpeakers = get_currentLayoutSpeakers();
  const radius = Math.max(0.01, r);
  const cart = sphericalToCartesianDeg(az, el, radius);
  const speaker = currentLayoutSpeakers[index];
  if (speaker) {
    speaker.azimuthDeg = az;
    speaker.elevationDeg = el;
    speaker.distanceM = radius;
  }
  applySpeakerSceneCartesianEdit(index, cart.x, cart.y, cart.z, sendOsc);
}

// ---------------------------------------------------------------------------
// Speaker editor panel
// ---------------------------------------------------------------------------

export function renderSpeakerEditor() {
  const speakerEditSectionEl = getSpeakerEditSectionEl();
  const speakerEditBodyEl = getSpeakerEditBodyEl();
  const speakerAddBtnEl = getSpeakerAddBtnEl();
  const speakerMoveUpBtnEl = getSpeakerMoveUpBtnEl();
  const speakerMoveDownBtnEl = getSpeakerMoveDownBtnEl();
  const speakerRemoveBtnEl = getSpeakerRemoveBtnEl();
  const speakerEditTitleEl = getSpeakerEditTitleEl();
  const speakerEditNameInputEl = getSpeakerEditNameInputEl();
  const speakerEditXInputEl = getSpeakerEditXInputEl();
  const speakerEditYInputEl = getSpeakerEditYInputEl();
  const speakerEditZInputEl = getSpeakerEditZInputEl();
  const speakerEditCartesianModeEl = getSpeakerEditCartesianModeEl();
  const speakerEditPolarModeEl = getSpeakerEditPolarModeEl();
  const speakerEditAzInputEl = getSpeakerEditAzInputEl();
  const speakerEditElInputEl = getSpeakerEditElInputEl();
  const speakerEditRInputEl = getSpeakerEditRInputEl();
  const speakerEditGainSliderEl = getSpeakerEditGainSliderEl();
  const speakerEditGainBoxEl = getSpeakerEditGainBoxEl();
  const speakerEditDelayMsInputEl = getSpeakerEditDelayMsInputEl();
  const speakerEditDelaySamplesInputEl = getSpeakerEditDelaySamplesInputEl();
  const speakerEditSpatializeToggleEl = getSpeakerEditSpatializeToggleEl();
  const speakerEditFreqHighInputEl = getSpeakerEditFreqHighInputEl();
  const speakerEditAutoDelayBtnEl = getSpeakerEditAutoDelayBtnEl();
  const speakerEditDelayToDistanceBtnEl = getSpeakerEditDelayToDistanceBtnEl();
  const speakerEditCartesianGizmoBtnEl = getSpeakerEditCartesianGizmoBtnEl();
  const speakerEditPolarGizmoBtnEl = getSpeakerEditPolarGizmoBtnEl();
  if (!speakerEditSectionEl || !speakerEditBodyEl) {
    return;
  }

  const selectedSpeakerIndex = get_selectedSpeakerIndex();
  const currentLayoutSpeakers = get_currentLayoutSpeakers();
  const frozen = isSpeakerLayoutFrozen();

  if (speakerAddBtnEl) speakerAddBtnEl.disabled = frozen;

  if (selectedSpeakerIndex === null || !currentLayoutSpeakers[selectedSpeakerIndex]) {
    if (speakerMoveUpBtnEl) speakerMoveUpBtnEl.disabled = true;
    if (speakerMoveDownBtnEl) speakerMoveDownBtnEl.disabled = true;
    if (speakerRemoveBtnEl) speakerRemoveBtnEl.disabled = true;
    speakerEditSectionEl.style.display = 'none';
    speakerEditBodyEl.style.display = 'none';
    return;
  }

  const idx = selectedSpeakerIndex;
  const id = String(idx);
  const speaker = currentLayoutSpeakers[idx];
  if (speakerMoveUpBtnEl) speakerMoveUpBtnEl.disabled = frozen || idx <= 0;
  if (speakerMoveDownBtnEl) speakerMoveDownBtnEl.disabled = frozen || idx >= currentLayoutSpeakers.length - 1;
  if (speakerRemoveBtnEl) speakerRemoveBtnEl.disabled = frozen || currentLayoutSpeakers.length === 0;
  const gain = getBaseGain(speakerBaseGains, speakerGainCache, id);
  const delayMs = Number(speakerDelays.get(id) ?? speaker.delay_ms ?? 0);
  const spherical = cartesianToSpherical(normalizedOmniphonyToScenePosition(speaker));
  const az = Number.isFinite(Number(speaker.azimuthDeg)) ? Number(speaker.azimuthDeg) : spherical.az;
  const el = Number.isFinite(Number(speaker.elevationDeg)) ? Number(speaker.elevationDeg) : spherical.el;
  const r = Number.isFinite(Number(speaker.distanceM)) ? Number(speaker.distanceM) : spherical.dist;

  speakerEditSectionEl.style.display = '';
  speakerEditBodyEl.style.display = '';

  if (speakerEditTitleEl) speakerEditTitleEl.textContent = `Speaker ${idx}`;
  if (speakerEditNameInputEl) speakerEditNameInputEl.value = String(speaker.id ?? idx);
  if (speakerEditXInputEl) speakerEditXInputEl.value = formatNumber(Number(speaker.x), 3);
  if (speakerEditYInputEl) speakerEditYInputEl.value = formatNumber(Number(speaker.y), 3);
  if (speakerEditZInputEl) speakerEditZInputEl.value = formatNumber(Number(speaker.z), 3);
  if (speakerEditCartesianModeEl) speakerEditCartesianModeEl.checked = getSpeakerCoordMode(speaker) === 'cartesian';
  if (speakerEditPolarModeEl) speakerEditPolarModeEl.checked = getSpeakerCoordMode(speaker) === 'polar';
  if (speakerEditAzInputEl) speakerEditAzInputEl.value = formatNumber(az, 1);
  if (speakerEditElInputEl) speakerEditElInputEl.value = formatNumber(el, 1);
  if (speakerEditRInputEl) speakerEditRInputEl.value = formatNumber(r, 3);
  if (speakerEditGainSliderEl) speakerEditGainSliderEl.value = String(gain);
  if (speakerEditGainBoxEl) speakerEditGainBoxEl.textContent = linearToDb(gain);
  if (speakerEditDelayMsInputEl) speakerEditDelayMsInputEl.value = String(Math.max(0, delayMs));
  if (speakerEditDelaySamplesInputEl) speakerEditDelaySamplesInputEl.value = String(delayMsToSamples(delayMs));
  if (speakerEditSpatializeToggleEl) speakerEditSpatializeToggleEl.checked = getSpeakerSpatializeValue(speaker) !== 0;
  const speakerEditFreqLowInputEl = getSpeakerEditFreqLowInputEl();
  if (speakerEditFreqLowInputEl) {
    syncInputValueUnlessEditing(
      speakerEditFreqLowInputEl,
      speaker.freqLow != null && speaker.freqLow > 0 ? String(speaker.freqLow) : ''
    );
  }
  if (speakerEditFreqHighInputEl) {
    syncInputValueUnlessEditing(
      speakerEditFreqHighInputEl,
      speaker.freqHigh != null && speaker.freqHigh > 0 ? String(speaker.freqHigh) : ''
    );
  }
  [
    speakerEditNameInputEl,
    speakerEditXInputEl,
    speakerEditYInputEl,
    speakerEditZInputEl,
    speakerEditAzInputEl,
    speakerEditElInputEl,
    speakerEditRInputEl,
    speakerEditGainSliderEl,
    speakerEditDelayMsInputEl,
    speakerEditDelaySamplesInputEl,
    speakerEditAutoDelayBtnEl,
    speakerEditDelayToDistanceBtnEl,
    speakerEditSpatializeToggleEl,
    speakerEditFreqLowInputEl,
    speakerEditFreqHighInputEl,
    speakerEditCartesianModeEl,
    speakerEditPolarModeEl,
    speakerEditCartesianGizmoBtnEl,
    speakerEditPolarGizmoBtnEl
  ].forEach((el) => {
    if (el) el.disabled = frozen;
  });
  if (speakerEditCartesianGizmoBtnEl) {
    speakerEditCartesianGizmoBtnEl.classList.toggle('active', app.cartesianEditArmed && app.activeEditMode === 'cartesian');
  }
  if (speakerEditPolarGizmoBtnEl) {
    speakerEditPolarGizmoBtnEl.classList.toggle('active', app.polarEditArmed && app.activeEditMode === 'polar');
  }
}

// ---------------------------------------------------------------------------
// Object list item creation / update
// ---------------------------------------------------------------------------

export function createObjectItem(id) {
  const root = document.createElement('div');
  root.className = 'info-item object-item';
  root.addEventListener('click', () => {
    setSelectedSource(id);
  });

  const idStrip = document.createElement('div');
  idStrip.className = 'id-strip flip';
  const idText = document.createElement('span');
  idText.textContent = String(id);
  idStrip.appendChild(idText);
  root.appendChild(idStrip);

  const content = document.createElement('div');
  content.className = 'object-content';

  const head = document.createElement('div');
  head.className = 'object-head';

  const position = document.createElement('div');
  head.appendChild(position);

  const topRight = document.createElement('div');
  topRight.className = 'object-topright';
  topRight.textContent = '\u2014';
  head.appendChild(topRight);

  content.appendChild(head);

  const level = document.createElement('div');
  level.className = 'meter-row';

  const levelText = document.createElement('div');
  level.appendChild(levelText);

  const meterBar = document.createElement('div');
  meterBar.className = 'meter-bar';
  const meterFill = document.createElement('div');
  meterFill.className = 'meter-fill';
  const contributionFill = document.createElement('div');
  contributionFill.className = 'meter-fill contribution';
  meterBar.appendChild(meterFill);
  meterBar.appendChild(contributionFill);
  const actionsRow = document.createElement('div');
  actionsRow.className = 'object-meter-actions';

  const muteBtn = document.createElement('button');
  muteBtn.type = 'button';
  muteBtn.className = 'toggle-btn';
  muteBtn.textContent = 'M';
  muteBtn.addEventListener('click', (event) => {
    event.preventDefault();
    toggleMute('object', id);
  });
  actionsRow.appendChild(muteBtn);

  const soloBtn = document.createElement('button');
  soloBtn.type = 'button';
  soloBtn.className = 'toggle-btn';
  soloBtn.textContent = 'S';
  soloBtn.addEventListener('click', (event) => {
    event.preventDefault();
    toggleSolo('object', id);
  });
  actionsRow.appendChild(soloBtn);

  level.appendChild(meterBar);
  level.appendChild(actionsRow);
  content.appendChild(level);

  const contributionRow = document.createElement('div');
  contributionRow.className = 'object-contrib-row';

  const bandBarsContainer = document.createElement('div');
  bandBarsContainer.className = 'band-contrib-bars';
  bandBarsContainer.style.display = 'none';
  contributionRow.style.display = 'none';
  contributionRow.appendChild(bandBarsContainer);

  content.appendChild(contributionRow);
  root.appendChild(content);

  return {
    root,
    idStrip,
    label: idText,
    position,
    topRight,
    levelText,
    meterFill,
    contributionFill,
    contributionRow,
    bandBarsContainer,
    muteBtn,
    soloBtn
  };
}

export function updateObjectItem(entry, id, position, name) {
  const selectedSourceId = get_selectedSourceId();
  const soloTarget = getSoloTarget('object');
  const metadataSilent = objectHasSilentMetadataGain(id);
  if (name) {
    sourceNames.set(id, name);
  }
  entry.label.textContent = getObjectDisplayName(id);
  entry.position.textContent = formatPosition(position);
  entry.topRight.textContent = getObjectDominantSpeakerText(id);
  entry.root.classList.toggle('has-active-trail', objectHasActiveTrail(id));
  entry.muteBtn.classList.toggle('active', objectMuted.has(id));
  entry.soloBtn.classList.toggle('active', soloTarget === id);
  updateItemClasses(entry, objectMuted.has(id), Boolean((soloTarget && soloTarget !== id) || metadataSilent));
  entry.root.classList.toggle('is-selected', selectedSourceId === id);
  updateMeterUI(entry, sourceLevels.get(id));
  updateObjectContributionUI_src(entry, id);
  applyObjectItemColor(entry, id);
}

// ---------------------------------------------------------------------------
// Speakers / Objects lists rendering
// ---------------------------------------------------------------------------

export function renderSpeakersList() {
  const speakersListEl = getSpeakersListEl();
  if (!speakersListEl) return;

  const currentLayoutSpeakers = get_currentLayoutSpeakers();

  if (!currentLayoutSpeakers.length) {
    speakersListEl.textContent = t('speakers.none');
    speakerItems.clear();
    updateSectionProportions();
    return;
  }

  speakersListEl.textContent = '';
  const activeIds = new Set();
  currentLayoutSpeakers.forEach((speaker, index) => {
    const id = String(index);
    activeIds.add(id);
    let entry = speakerItems.get(id);
    if (!entry) {
      entry = createSpeakerItem(id, speaker);
      speakerItems.set(id, entry);
    }
    updateSpeakerItem(entry, id, speaker);
    speakersListEl.appendChild(entry.root);
  });
  speakerItems.forEach((entry, id) => {
    if (!activeIds.has(id)) {
      entry.root.remove();
      speakerItems.delete(id);
    }
  });
  updateSectionProportions();
}

export function renderObjectsList() {
  const objectsListEl = getObjectsListEl();
  if (!objectsListEl) return;

  const ids = [...sourceMeshes.keys()].sort((a, b) => {
    const aNum = Number(a);
    const bNum = Number(b);
    const aIsNum = Number.isFinite(aNum);
    const bIsNum = Number.isFinite(bNum);
    if (aIsNum && bIsNum) {
      return aNum - bNum;
    }
    if (aIsNum) {
      return -1;
    }
    if (bIsNum) {
      return 1;
    }
    return String(a).localeCompare(String(b));
  });
  if (!ids.length) {
    objectsListEl.textContent = t('objects.none');
    objectItems.clear();
    updateSectionProportions();
    return;
  }

  objectsListEl.textContent = '';
  const activeIds = new Set();
  ids.forEach((id) => {
    const mesh = sourceMeshes.get(id);
    if (!mesh) return;
    const key = String(id);
    activeIds.add(key);
    let entry = objectItems.get(key);
    if (!entry) {
      entry = createObjectItem(key);
      objectItems.set(key, entry);
    }
    const raw = sourcePositionsRaw.get(key) || mesh.position;
    updateObjectItem(entry, key, raw, sourceNames.get(key));
    objectsListEl.appendChild(entry.root);
  });
  objectItems.forEach((entry, id) => {
    if (!activeIds.has(id)) {
      entry.root.remove();
      objectItems.delete(id);
    }
  });
  updateSectionProportions();
}

export function refreshOverlayLists() {
  renderSpeakersList();
  renderObjectsList();
  updateSectionProportions();
}

export function getSpeakerIds() {
  const currentLayoutSpeakers = get_currentLayoutSpeakers();
  return currentLayoutSpeakers.map((_, index) => String(index));
}

export function getObjectIds() {
  return [...sourceMeshes.keys()].map((id) => String(id));
}

// ---------------------------------------------------------------------------
// Gizmo / selection
// ---------------------------------------------------------------------------

export function updateSpeakerGizmo() {
  const selectedSpeakerIndex = get_selectedSpeakerIndex();
  const polarActive = app.activeEditMode === 'polar' && selectedSpeakerIndex !== null && app.polarEditArmed;
  const cartesianActive = app.activeEditMode === 'cartesian' && selectedSpeakerIndex !== null && app.cartesianEditArmed;

  cartesianGizmo.group.visible = false;

  if (!polarActive) {
    speakerGizmo.ring.visible = false;
    speakerGizmo.ringTicks.visible = false;
    speakerGizmo.ringMinorTicks.visible = false;
    speakerGizmo.arc.visible = false;
    speakerGizmo.arcTicks.visible = false;
    speakerGizmo.arcMinorTicks.visible = false;
    speakerGizmo.ringLabels.visible = false;
    speakerGizmo.arcLabels.visible = false;
    speakerGizmo.ringCurrent.visible = false;
    speakerGizmo.arcCurrent.visible = false;
    distanceGizmo.group.visible = false;
  } else {
    const mesh = speakerMeshes[selectedSpeakerIndex];
    if (!mesh) {
      speakerGizmo.ring.visible = false;
      speakerGizmo.ringTicks.visible = false;
      speakerGizmo.ringMinorTicks.visible = false;
      speakerGizmo.arc.visible = false;
      speakerGizmo.arcTicks.visible = false;
      speakerGizmo.arcMinorTicks.visible = false;
      speakerGizmo.ringLabels.visible = false;
      speakerGizmo.arcLabels.visible = false;
      speakerGizmo.ringCurrent.visible = false;
      speakerGizmo.arcCurrent.visible = false;
      distanceGizmo.group.visible = false;
    } else {
      const { az, el, dist } = cartesianToSpherical(mesh.position);
      app.dragAzimuthDeg = az;
      app.dragElevationDeg = el;
      app.dragDistance = Math.max(0.01, dist);

      speakerGizmo.ring.visible = true;
      speakerGizmo.ringTicks.visible = !app.isDraggingSpeaker || app.dragAzimuthDelta > 0.1;
      speakerGizmo.ringMinorTicks.visible = app.isDraggingSpeaker && app.dragAzimuthDelta >= 0 && app.dragAzimuthDelta <= 0.1;
      speakerGizmo.arc.visible = true;
      speakerGizmo.arcTicks.visible = !app.isDraggingSpeaker || app.dragElevationDelta > 0.1;
      speakerGizmo.arcMinorTicks.visible = app.isDraggingSpeaker && app.dragElevationDelta >= 0 && app.dragElevationDelta <= 0.1;
      speakerGizmo.ringLabels.visible = true;
      speakerGizmo.arcLabels.visible = true;
      speakerGizmo.ringCurrent.visible = true;
      speakerGizmo.arcCurrent.visible = true;
      distanceGizmo.group.visible = true;

      speakerGizmo.ring.position.set(0, 0, 0);
      speakerGizmo.ring.scale.set(app.dragDistance, 1, app.dragDistance);
      speakerGizmo.ringTicks.position.set(0, 0, 0);
      speakerGizmo.ringTicks.scale.set(app.dragDistance, 1, app.dragDistance);
      speakerGizmo.ringMinorTicks.position.set(0, 0, 0);
      speakerGizmo.ringMinorTicks.scale.set(app.dragDistance, 1, app.dragDistance);
      speakerGizmo.ringLabels.position.set(0, 0, 0);
      speakerGizmo.ringLabels.scale.set(app.dragDistance, 1, app.dragDistance);
      speakerGizmo.ringCurrent.position.set(0, 0, 0);
      speakerGizmo.ringCurrent.scale.set(app.dragDistance, 1, app.dragDistance);

      const azRad = (az * Math.PI) / 180;
      speakerGizmo.arc.position.set(0, 0, 0);
      speakerGizmo.arc.scale.set(app.dragDistance, app.dragDistance, app.dragDistance);
      speakerGizmo.arc.rotation.set(0, -azRad, 0);
      speakerGizmo.arcTicks.position.set(0, 0, 0);
      speakerGizmo.arcTicks.scale.set(app.dragDistance, app.dragDistance, app.dragDistance);
      speakerGizmo.arcTicks.rotation.set(0, -azRad, 0);
      speakerGizmo.arcMinorTicks.position.set(0, 0, 0);
      speakerGizmo.arcMinorTicks.scale.set(app.dragDistance, app.dragDistance, app.dragDistance);
      speakerGizmo.arcMinorTicks.rotation.set(0, -azRad, 0);
      speakerGizmo.arcLabels.position.set(0, 0, 0);
      speakerGizmo.arcLabels.scale.set(app.dragDistance, app.dragDistance, app.dragDistance);
      speakerGizmo.arcLabels.rotation.set(0, -azRad, 0);
      speakerGizmo.arcCurrent.position.set(0, 0, 0);
      speakerGizmo.arcCurrent.scale.set(app.dragDistance, app.dragDistance, app.dragDistance);
      speakerGizmo.arcCurrent.rotation.set(0, -azRad, 0);

      ringLabelAngles.forEach((angle, idx) => {
        const sprite = speakerGizmo.ringLabels.children[idx];
        const rad = (angle * Math.PI) / 180;
        const r = 1.1;
        sprite.position.set(Math.cos(rad) * r, 0.02, Math.sin(rad) * r);
      });

      arcLabelAngles.forEach((angle, idx) => {
        const sprite = speakerGizmo.arcLabels.children[idx];
        const rad = (angle * Math.PI) / 180;
        const r = 1.1;
        sprite.position.set(Math.cos(rad) * r, Math.sin(rad) * r, 0);
      });

      const ringAngle = normalizeAngleDeg(app.dragAzimuthDeg);
      const ringRad = (ringAngle * Math.PI) / 180;
      speakerGizmo.ringCurrentLabel.position.set(Math.cos(ringRad) * 1.24, 0.04, Math.sin(ringRad) * 1.24);
      setLabelSpriteText(speakerGizmo.ringCurrentLabel, `${ringAngle.toFixed(1)}`);

      const arcAngle = app.dragElevationDeg;
      const arcRad = (arcAngle * Math.PI) / 180;
      speakerGizmo.arcCurrentLabel.position.set(Math.cos(arcRad) * 1.24, Math.sin(arcRad) * 1.24, 0);
      setLabelSpriteText(speakerGizmo.arcCurrentLabel, `${arcAngle.toFixed(1)}`);

      const speakerPos = mesh.position.clone();
      const dir = speakerPos.length() > 1e-6 ? speakerPos.clone().normalize() : new THREE.Vector3(1, 0, 0);
      const lineGeom = distanceGizmo.line.geometry;
      lineGeom.setFromPoints([new THREE.Vector3(0, 0, 0), speakerPos.clone()]);
      lineGeom.attributes.position.needsUpdate = true;

      const arrowOffset = 0.1;
      distanceGizmo.arrowA.position.copy(dir.clone().multiplyScalar(arrowOffset));
      distanceGizmo.arrowB.position.copy(speakerPos.clone().add(dir.clone().multiplyScalar(-arrowOffset)));

      const up = new THREE.Vector3(0, 1, 0);
      const quat = new THREE.Quaternion().setFromUnitVectors(up, dir);
      distanceGizmo.arrowA.quaternion.copy(quat);
      const quatB = new THREE.Quaternion().setFromUnitVectors(up, dir.clone().negate());
      distanceGizmo.arrowB.quaternion.copy(quatB);

      const mid = speakerPos.clone().multiplyScalar(0.5);
      distanceGizmo.label.position.set(mid.x, mid.y + 0.08, mid.z);
      setLabelSpriteText(distanceGizmo.label, `${speakerPos.length().toFixed(2)}`);
    }
  }

  if (cartesianActive) {
    const mesh = speakerMeshes[selectedSpeakerIndex];
    if (!mesh) {
      cartesianGizmo.group.visible = false;
    } else {
      cartesianGizmo.group.visible = true;
      cartesianGizmo.group.position.copy(mesh.position);
      const scale = Math.max(0.2, camera.position.distanceTo(mesh.position) * 0.08);
      cartesianGizmo.group.scale.setScalar(scale);
    }
  }
}

export function setSelectedSpeaker(index) {
  if (index === null) {
    app.polarEditArmed = false;
    app.cartesianEditArmed = false;
  }
  app.selectedSpeakerIndex = index;
  updateSourceSelectionStyles();
  updateSpeakerColorsFromSelection();
  updateSpeakerGizmo();
  updateSpeakerControlsUI();
  updateControlsForEditMode();
  requestSpeakerHeatmapIfNeeded();
}

export function updateControlsForEditMode() {
  controls.enableZoom = true;
}

// ---------------------------------------------------------------------------
// Room face visibility / face shadows
// ---------------------------------------------------------------------------

export function updateRoomFaceVisibility() {
  tempCameraLocal.copy(camera.position);
  roomGroup.worldToLocal(tempCameraLocal);
  roomFaceDefs.forEach((entry) => {
    const facePos = entry.mesh.position;
    tempToCamera.set(
      tempCameraLocal.x - facePos.x,
      tempCameraLocal.y - facePos.y,
      tempCameraLocal.z - facePos.z
    );
    tempToCenter.set(-facePos.x, -facePos.y, -facePos.z);
    const camSide = entry.inward.dot(tempToCamera);
    entry.mesh.visible = camSide > 0;
  });
  syncVbapCartesianFaceGridVisibility();

  const screenFace = roomFaceDefs.find((entry) => entry.key === 'posX');
  if (screenFace) {
    const facePos = screenFace.mesh.position;
    tempToCamera.set(
      tempCameraLocal.x - facePos.x,
      tempCameraLocal.y - facePos.y,
      tempCameraLocal.z - facePos.z
    );
    const camSide = screenFace.inward.dot(tempToCamera);
    const isInside = camSide > 0;
    screenMaterial.opacity = isInside ? 0.18 : 0.18;
  }
}

export function updateSelectedSpeakerFaceShadows() {
  const selectedSpeakerIndex = get_selectedSpeakerIndex();
  const index = selectedSpeakerIndex;
  const mesh = index !== null ? speakerMeshes[index] : null;
  if (!mesh) {
    Object.values(selectedSpeakerShadows).forEach((shadow) => {
      shadow.visible = false;
    });
    return;
  }

  const xMin = roomBounds.xMin;
  const xMax = roomBounds.xMax;
  const yMin = roomBounds.yMin;
  const yMax = roomBounds.yMax;
  const zMin = roomBounds.zMin;
  const zMax = roomBounds.zMax;
  const spanX = Math.max(1e-6, xMax - xMin);
  const spanY = Math.max(1e-6, yMax - yMin);
  const spanZ = Math.max(1e-6, zMax - zMin);
  const p = mesh.position;
  const eps = 0.01;
  const baseRadius = 0.08;

  const clampedX = clampNumber(p.x, xMin, xMax);
  const clampedY = clampNumber(p.y, yMin, yMax);
  const clampedZ = clampNumber(p.z, zMin, zMax);

  const setShadow = (shadow, x, y, z, dist, maxDist) => {
    const t = maxDist > 1e-6 ? clampNumber(1 - (dist / maxDist), 0.08, 1) : 1;
    shadow.visible = true;
    shadow.position.set(x, y, z);
    shadow.scale.setScalar(baseRadius * (0.7 + 0.6 * t));
    shadow.material.opacity = 0.06 + 0.18 * t;
  };

  setShadow(selectedSpeakerShadows.posX, xMax - eps, clampedY, clampedZ, Math.abs(xMax - p.x), spanX);
  setShadow(selectedSpeakerShadows.negX, xMin + eps, clampedY, clampedZ, Math.abs(xMin - p.x), spanX);
  setShadow(selectedSpeakerShadows.posY, clampedX, yMax - eps, clampedZ, Math.abs(yMax - p.y), spanY);
  setShadow(selectedSpeakerShadows.negY, clampedX, yMin + eps, clampedZ, Math.abs(yMin - p.y), spanY);
  setShadow(selectedSpeakerShadows.posZ, clampedX, clampedY, zMax - eps, Math.abs(zMax - p.z), spanZ);
  setShadow(selectedSpeakerShadows.negZ, clampedX, clampedY, zMin + eps, Math.abs(zMin - p.z), spanZ);
}

export function updateSelectedObjectFaceShadows() {
  const selectedSourceId = get_selectedSourceId();
  const mesh = selectedSourceId ? sourceMeshes.get(selectedSourceId) : null;
  if (!mesh) {
    Object.values(selectedObjectShadows).forEach((shadow) => {
      shadow.visible = false;
    });
    return;
  }

  const xMin = roomBounds.xMin;
  const xMax = roomBounds.xMax;
  const yMin = roomBounds.yMin;
  const yMax = roomBounds.yMax;
  const zMin = roomBounds.zMin;
  const zMax = roomBounds.zMax;
  const spanX = Math.max(1e-6, xMax - xMin);
  const spanY = Math.max(1e-6, yMax - yMin);
  const spanZ = Math.max(1e-6, zMax - zMin);
  const p = mesh.position;
  const eps = 0.01;
  const baseRadius = 0.08;

  const clampedX = clampNumber(p.x, xMin, xMax);
  const clampedY = clampNumber(p.y, yMin, yMax);
  const clampedZ = clampNumber(p.z, zMin, zMax);

  const setShadow = (shadow, x, y, z, dist, maxDist) => {
    const t = maxDist > 1e-6 ? clampNumber(1 - (dist / maxDist), 0.08, 1) : 1;
    shadow.visible = true;
    shadow.position.set(x, y, z);
    shadow.scale.setScalar(baseRadius * (0.7 + 0.6 * t));
    shadow.material.opacity = 0.06 + 0.18 * t;
  };

  setShadow(selectedObjectShadows.posX, xMax - eps, clampedY, clampedZ, Math.abs(xMax - p.x), spanX);
  setShadow(selectedObjectShadows.negX, xMin + eps, clampedY, clampedZ, Math.abs(xMin - p.x), spanX);
  setShadow(selectedObjectShadows.posY, clampedX, yMax - eps, clampedZ, Math.abs(yMax - p.y), spanY);
  setShadow(selectedObjectShadows.negY, clampedX, yMin + eps, clampedZ, Math.abs(yMin - p.y), spanY);
  setShadow(selectedObjectShadows.posZ, clampedX, clampedY, zMax - eps, Math.abs(zMax - p.z), spanZ);
  setShadow(selectedObjectShadows.negZ, clampedX, clampedY, zMin + eps, Math.abs(zMin - p.z), spanZ);
}

export function updateSectionProportions() {
  const speakersSectionEl = getSpeakersSectionEl();
  const objectsSectionEl = getObjectsSectionEl();
  if (speakersSectionEl) {
    speakersSectionEl.style.flex = '1 1 0%';
  }
  if (objectsSectionEl) {
    objectsSectionEl.style.flex = '1 1 0%';
  }
}

// ---------------------------------------------------------------------------
// Layout / speaker management
// ---------------------------------------------------------------------------

export function currentLayoutRef() {
  const currentLayoutKey = get_currentLayoutKey();
  return currentLayoutKey ? layoutsByKey.get(currentLayoutKey) : null;
}

export function requestAddSpeaker() {
  if (isSpeakerLayoutFrozen()) return;
  const layout = currentLayoutRef();
  if (!layout) return;
  const selectedSpeakerIndex = get_selectedSpeakerIndex();
  const base = selectedSpeakerIndex !== null ? layout.speakers[selectedSpeakerIndex] : null;
  const nextIndex = layout.speakers.length;
  const speaker = {
    id: `spk-${nextIndex}`,
    x: Number(base?.x) || 0,
    y: Number(base?.y) || 0,
    z: Number(base?.z) || 0,
    azimuthDeg: Number(base?.azimuthDeg) || 0,
    elevationDeg: Number(base?.elevationDeg) || 0,
    distanceM: Math.max(0.01, Number(base?.distanceM) || 1),
    coordMode: getSpeakerCoordMode(base),
    spatialize: Number(base?.spatialize ?? 1) ? 1 : 0,
    delay_ms: Math.max(0, Number(base?.delay_ms) || 0)
  };
  layout.speakers.push(speaker);
  renderLayout(get_currentLayoutKey());
  setSelectedSpeaker(layout.speakers.length - 1);
  invoke('control_speakers_add', {
    name: speaker.id,
    azimuth: Number(speaker.azimuthDeg) || 0,
    elevation: Number(speaker.elevationDeg) || 0,
    distance: Math.max(0.01, Number(speaker.distanceM) || 1),
    spatialize: Number(speaker.spatialize) ? 1 : 0,
    delayMs: Math.max(0, Number(speaker.delay_ms) || 0)
  });
}

export function requestRemoveSpeaker() {
  if (isSpeakerLayoutFrozen()) return;
  const layout = currentLayoutRef();
  const selectedSpeakerIndex = get_selectedSpeakerIndex();
  if (!layout || selectedSpeakerIndex === null) return;
  const idx = selectedSpeakerIndex;
  if (idx < 0 || idx >= layout.speakers.length) return;
  layout.speakers.splice(idx, 1);
  renderLayout(get_currentLayoutKey());
  const next = layout.speakers.length ? Math.max(0, idx - 1) : null;
  setSelectedSpeaker(next);
  invoke('control_speakers_remove', { index: idx });
}

export function requestMoveSpeaker(delta) {
  if (isSpeakerLayoutFrozen()) return;
  const layout = currentLayoutRef();
  const selectedSpeakerIndex = get_selectedSpeakerIndex();
  if (!layout || selectedSpeakerIndex === null) return;
  const from = selectedSpeakerIndex;
  const to = Math.max(0, Math.min(layout.speakers.length - 1, from + delta));
  requestMoveSpeakerTo(from, to, true);
}

export function markDraggedSpeakerItem() {
  speakerItems.forEach((item) => {
    item.root.classList.toggle('is-dragging', app.draggedSpeakerRoot !== null && item.root === app.draggedSpeakerRoot);
  });
}

export function animateSpeakerListReorder(mutate) {
  const speakersListEl = getSpeakersListEl();
  if (!speakersListEl) {
    mutate();
    return;
  }
  const items = Array.from(speakersListEl.querySelectorAll('.speaker-item'));
  const beforeTop = new Map();
  items.forEach((el) => {
    beforeTop.set(el, el.getBoundingClientRect().top);
  });

  mutate();

  const afterItems = Array.from(speakersListEl.querySelectorAll('.speaker-item'));
  afterItems.forEach((el) => {
    if (app.draggedSpeakerRoot && el === app.draggedSpeakerRoot) return;
    const prev = beforeTop.get(el);
    if (prev === undefined) return;
    const next = el.getBoundingClientRect().top;
    const dy = prev - next;
    if (Math.abs(dy) < 0.5) return;
    const prevAnim = speakerReorderAnimations.get(el);
    if (prevAnim) {
      prevAnim.cancel();
    }
    const anim = el.animate(
      [
        { transform: `translateY(${dy}px)` },
        { transform: 'translateY(0px)' }
      ],
      {
        duration: 120,
        easing: 'cubic-bezier(0.2, 0.8, 0.2, 1)',
        fill: 'none'
      }
    );
    speakerReorderAnimations.set(el, anim);
    anim.onfinish = () => {
      if (speakerReorderAnimations.get(el) === anim) {
        speakerReorderAnimations.delete(el);
      }
    };
  });
}

export function requestMoveSpeakerTo(from, to, sendOsc = true) {
  if (isSpeakerLayoutFrozen()) return;
  const layout = currentLayoutRef();
  if (!layout) return;
  if (!Number.isInteger(from) || !Number.isInteger(to)) return;
  if (from < 0 || to < 0 || from >= layout.speakers.length || to >= layout.speakers.length) return;
  if (from === to) return;

  const moved = layout.speakers.splice(from, 1)[0];
  layout.speakers.splice(to, 0, moved);

  let nextSelected = get_selectedSpeakerIndex();
  if (nextSelected === from) {
    nextSelected = to;
  } else if (nextSelected !== null) {
    if (from < to && nextSelected > from && nextSelected <= to) {
      nextSelected -= 1;
    } else if (to < from && nextSelected >= to && nextSelected < from) {
      nextSelected += 1;
    }
  }

  renderLayout(get_currentLayoutKey());
  setSelectedSpeaker(nextSelected);
  if (sendOsc) {
    invoke('control_speakers_move', { from, to });
  }
  markDraggedSpeakerItem();
}

// ---------------------------------------------------------------------------
// Speakers list drag-and-drop event listeners
// ---------------------------------------------------------------------------

const initialSpeakersListEl = getSpeakersListEl();
if (initialSpeakersListEl) {
  initialSpeakersListEl.addEventListener('dragenter', (event) => {
    if (app.draggedSpeakerRoot === null) return;
    event.preventDefault();
    if (event.dataTransfer) {
      event.dataTransfer.dropEffect = 'move';
    }
  });

  initialSpeakersListEl.addEventListener('dragover', (event) => {
    const speakersListEl = getSpeakersListEl();
    if (!speakersListEl) return;
    if (app.draggedSpeakerIndex === null || !app.draggedSpeakerRoot) return;
    event.preventDefault();
    if (event.dataTransfer) {
      event.dataTransfer.dropEffect = 'move';
    }
    // Let per-item handlers manage direct item hover. This path handles gaps.
    const target = event.target;
    if (target instanceof Element && target.closest('.speaker-item')) return;
    const items = Array.from(speakersListEl.querySelectorAll('.speaker-item'));
    let insertBefore = null;
    for (const item of items) {
      if (item === app.draggedSpeakerRoot) continue;
      const rect = item.getBoundingClientRect();
      if (event.clientY < rect.top + rect.height * 0.5) {
        insertBefore = item;
        break;
      }
    }
    animateSpeakerListReorder(() => {
      speakersListEl.insertBefore(app.draggedSpeakerRoot, insertBefore);
    });
    app.draggedSpeakerIndex = Array.from(speakersListEl.querySelectorAll('.speaker-item')).indexOf(app.draggedSpeakerRoot);
    markDraggedSpeakerItem();
  });

  initialSpeakersListEl.addEventListener('drop', (event) => {
    if (app.draggedSpeakerIndex === null) return;
    event.preventDefault();
    app.draggedSpeakerDidDrop = true;
  });
}

// Ensure the browser keeps "drop allowed" cursor over any child node inside the speakers list.
document.addEventListener('dragover', (event) => {
  const speakersListEl = getSpeakersListEl();
  if (!app.draggedSpeakerRoot || !speakersListEl) return;
  const target = event.target;
  if (!(target instanceof Node) || !speakersListEl.contains(target)) return;
  event.preventDefault();
  if (event.dataTransfer) {
    event.dataTransfer.dropEffect = 'move';
  }
});

// ---------------------------------------------------------------------------
// Render layout (rebuild speaker meshes from layout data)
// ---------------------------------------------------------------------------

export function renderLayout(key) {
  const previousLayoutKey = get_currentLayoutKey();
  const previousSelectedIndex = get_selectedSpeakerIndex();
  const currentLayoutSpeakers = get_currentLayoutSpeakers();
  const previousSelectedSpeaker = previousSelectedIndex !== null ? currentLayoutSpeakers[previousSelectedIndex] : null;
  const previousSelectedSpeakerId = previousSelectedSpeaker ? String(previousSelectedSpeaker.id ?? previousSelectedIndex) : null;
  const preserveSelection = previousLayoutKey !== null && previousLayoutKey === key;
  const previousSpeakersById = new Map(
    currentLayoutSpeakers.map((speaker, index) => [String(speaker?.id ?? index), speaker])
  );

  clearSpeakers();
  const layout = layoutsByKey.get(key);
  if (!layout) {
    set_currentLayoutKey(null);
    set_currentLayoutSpeakers([]);
    renderSpeakersList();
    set_selectedSpeakerIndex(null);
    app.polarEditArmed = false;
    app.cartesianEditArmed = false;
    updateSpeakerGizmo();
    updateControlsForEditMode();
    renderSpeakerEditor();
    return;
  }

  set_currentLayoutKey(key);
  const newSpeakers = Array.isArray(layout.speakers) ? layout.speakers : [];
  set_currentLayoutSpeakers(newSpeakers);
  sceneState.metersPerUnit = Math.max(0.01, Number(layout.radius_m) || 1.0);
  speakerDelays.clear();
  newSpeakers.forEach((speaker, index) => {
    const speakerId = String(speaker?.id ?? index);
    const previousSpeaker = preserveSelection ? previousSpeakersById.get(speakerId) : null;
    if (previousSpeaker) {
      speaker.coordMode = getSpeakerCoordMode(previousSpeaker);
      speaker.x = Number.isFinite(Number(previousSpeaker.x)) ? Number(previousSpeaker.x) : speaker.x;
      speaker.y = Number.isFinite(Number(previousSpeaker.y)) ? Number(previousSpeaker.y) : speaker.y;
      speaker.z = Number.isFinite(Number(previousSpeaker.z)) ? Number(previousSpeaker.z) : speaker.z;
      speaker.azimuthDeg = Number.isFinite(Number(previousSpeaker.azimuthDeg))
        ? Number(previousSpeaker.azimuthDeg)
        : speaker.azimuthDeg;
      speaker.elevationDeg = Number.isFinite(Number(previousSpeaker.elevationDeg))
        ? Number(previousSpeaker.elevationDeg)
        : speaker.elevationDeg;
      speaker.distanceM = Number.isFinite(Number(previousSpeaker.distanceM))
        ? Number(previousSpeaker.distanceM)
        : speaker.distanceM;
    }
    hydrateSpeakerCoordinateState(speaker);
    speakerDelays.set(String(index), speaker.delay_ms ?? 0);
  });
  if (preserveSelection) {
    let nextSelectedIndex = null;
    if (previousSelectedSpeakerId !== null) {
      const matchedIndex = newSpeakers.findIndex(
        (speaker, index) => String(speaker?.id ?? index) === previousSelectedSpeakerId
      );
      if (matchedIndex >= 0) {
        nextSelectedIndex = matchedIndex;
      }
    }
    if (nextSelectedIndex === null
      && previousSelectedIndex !== null
      && previousSelectedIndex >= 0
      && previousSelectedIndex < newSpeakers.length) {
      nextSelectedIndex = previousSelectedIndex;
    }
    set_selectedSpeakerIndex(nextSelectedIndex);
    if (get_selectedSpeakerIndex() === null) {
      app.polarEditArmed = false;
      app.cartesianEditArmed = false;
    }
  } else {
    set_selectedSpeakerIndex(null);
    app.polarEditArmed = false;
    app.cartesianEditArmed = false;
  }
  updateSpeakerGizmo();
  updateControlsForEditMode();
  const speakerIds = getSpeakerIds();
  speakerMuted.forEach((id) => {
    if (!speakerIds.includes(id)) {
      speakerMuted.delete(id);
    }
  });
  speakerManualMuted.forEach((id) => {
    if (!speakerIds.includes(id)) {
      speakerManualMuted.delete(id);
    }
  });
  speakerBaseGains.forEach((_, id) => {
    if (!speakerIds.includes(id)) {
      speakerBaseGains.delete(id);
    }
  });

  layout.speakers.forEach((speaker, index) => {
    const mesh = new THREE.Mesh(speakerGeometry.clone(), speakerMaterial.clone());
    const scenePosition = normalizedOmniphonyToScenePosition(speaker);
    mesh.position.set(scenePosition.x, scenePosition.y, scenePosition.z);
    const baseOpacity = getSpeakerBaseOpacity(speaker);
    mesh.userData.baseOpacity = baseOpacity;
    mesh.material.opacity = baseOpacity;
    scene.add(mesh);
    speakerMeshes.push(mesh);

    const label = createLabelSprite(String(speaker.id || index));
    label.userData.speakerIndex = index;
    label.visible = app.speakerLabelsEnabled;
    label.position.set(scenePosition.x, scenePosition.y + 0.12, scenePosition.z);
    scene.add(label);
    speakerLabels.push(label);

    applySpeakerLevel(mesh, speakerLevels.get(String(index)));
  });

  sourceMeshes.forEach((_, id) => {
    updateEffectiveRenderDecoration(id);
  });

  updateSpeakerColorsFromSelection();
  refreshOverlayLists();
  renderSpeakerEditor();
}

// ---------------------------------------------------------------------------
// Speaker level / meter decay
// ---------------------------------------------------------------------------

export function updateSpeakerLevel(index, meter) {
  const key = String(index);
  speakerLevels.set(key, {
    peakDbfs: Number(meter?.peakDbfs ?? -100),
    rmsDbfs: Number(meter?.rmsDbfs ?? -100)
  });
  speakerLevelLastSeen.set(key, performance.now());
  const mesh = speakerMeshes[index];
  if (mesh) {
    applySpeakerLevel(mesh, speakerLevels.get(key));
  }
  updateSpeakerMeterUI(key);
  dirty.masterMeter = true;
  scheduleUIFlush();
}

export function decayMeters(nowMs) {
  if (app.lastMeterDecayAt === 0) {
    app.lastMeterDecayAt = nowMs;
    return;
  }
  const dtSec = Math.max(0, (nowMs - app.lastMeterDecayAt) / 1000);
  app.lastMeterDecayAt = nowMs;
  if (dtSec <= 0) return;

  const decayDb = METER_DECAY_DB_PER_SEC * dtSec;
  let anySpeakerChanged = false;

  sourceLevels.forEach((meter, id) => {
    const lastSeen = sourceLevelLastSeen.get(id) ?? nowMs;
    if (nowMs - lastSeen < METER_DECAY_START_MS) return;
    const prevPeak = Number(meter?.peakDbfs ?? -100);
    const prevRms = Number(meter?.rmsDbfs ?? -100);
    const nextPeak = Math.max(-100, prevPeak - decayDb);
    const nextRms = Math.max(-100, prevRms - decayDb);
    if (nextPeak === prevPeak && nextRms === prevRms) return;
    meter.peakDbfs = nextPeak;
    meter.rmsDbfs = nextRms;
    const mesh = sourceMeshes.get(id);
    if (mesh) {
      applySourceLevel(id, mesh, meter);
    }
    updateObjectMeterUI(id);
  });

  speakerLevels.forEach((meter, id) => {
    const lastSeen = speakerLevelLastSeen.get(id) ?? nowMs;
    if (nowMs - lastSeen < METER_DECAY_START_MS) return;
    const prevPeak = Number(meter?.peakDbfs ?? -100);
    const prevRms = Number(meter?.rmsDbfs ?? -100);
    const nextPeak = Math.max(-100, prevPeak - decayDb);
    const nextRms = Math.max(-100, prevRms - decayDb);
    if (nextPeak === prevPeak && nextRms === prevRms) return;
    meter.peakDbfs = nextPeak;
    meter.rmsDbfs = nextRms;
    const idx = Number(id);
    if (Number.isInteger(idx) && speakerMeshes[idx]) {
      applySpeakerLevel(speakerMeshes[idx], meter);
    }
    updateSpeakerMeterUI(id);
    anySpeakerChanged = true;
  });

  if (anySpeakerChanged) {
    dirty.masterMeter = true;
    scheduleUIFlush();
  }
}

// ---------------------------------------------------------------------------
// Hydrate layout <select> dropdown from layout list
// ---------------------------------------------------------------------------

function canPatchCurrentLayout(key, layout) {
  if (!layout || get_currentLayoutKey() !== key) {
    return false;
  }
  const currentSpeakers = get_currentLayoutSpeakers();
  const nextSpeakers = Array.isArray(layout.speakers) ? layout.speakers : [];
  if (currentSpeakers.length !== nextSpeakers.length) {
    return false;
  }
  for (let index = 0; index < nextSpeakers.length; index += 1) {
    const currentId = String(currentSpeakers[index]?.id ?? index);
    const nextId = String(nextSpeakers[index]?.id ?? index);
    if (currentId !== nextId) {
      return false;
    }
  }
  return true;
}

function patchCurrentLayout(key) {
  const layout = layoutsByKey.get(key);
  if (!layout) {
    return false;
  }
  const nextSpeakers = Array.isArray(layout.speakers) ? layout.speakers : [];
  set_currentLayoutKey(key);
  set_currentLayoutSpeakers(nextSpeakers);
  sceneState.metersPerUnit = Math.max(0.01, Number(layout.radius_m) || 1.0);
  speakerDelays.clear();
  nextSpeakers.forEach((speaker, index) => {
    hydrateSpeakerCoordinateState(speaker);
    speakerDelays.set(String(index), speaker.delay_ms ?? 0);
    updateSpeakerVisualsFromState(index);
  });
  sourceMeshes.forEach((_, id) => {
    updateEffectiveRenderDecoration(id);
  });
  updateSpeakerColorsFromSelection();
  refreshOverlayLists();
  renderSpeakersList();
  renderSpeakerEditor();
  return true;
}

export function hydrateLayoutSelect(layouts, selectedLayoutKey) {
  const layoutSelectEl = document.getElementById('layoutSelect');

  layoutsByKey.clear();
  if (layoutSelectEl) {
    layoutSelectEl.innerHTML = '';
  }

  layouts.forEach((layout) => {
    layoutsByKey.set(layout.key, layout);
    if (layoutSelectEl) {
      const option = document.createElement('option');
      option.value = layout.key;
      option.textContent = layout.name;
      layoutSelectEl.appendChild(option);
    }
  });

  if (selectedLayoutKey && layoutsByKey.has(selectedLayoutKey)) {
    if (layoutSelectEl) layoutSelectEl.value = selectedLayoutKey;
    if (!canPatchCurrentLayout(selectedLayoutKey, layoutsByKey.get(selectedLayoutKey))) {
      renderLayout(selectedLayoutKey);
    } else {
      patchCurrentLayout(selectedLayoutKey);
    }
  } else if (layouts.length > 0) {
    const firstKey = layouts[0].key;
    if (layoutSelectEl) layoutSelectEl.value = firstKey;
    if (!canPatchCurrentLayout(firstKey, layoutsByKey.get(firstKey))) {
      renderLayout(firstKey);
    } else {
      patchCurrentLayout(firstKey);
    }
  } else {
    set_currentLayoutKey(null);
    set_currentLayoutSpeakers([]);
    renderSpeakersList();
    renderSpeakerEditor();
  }

  if (layoutSelectEl) {
    layoutSelectEl.disabled = layouts.length === 0 || isSpeakerLayoutFrozen();
  }
}
