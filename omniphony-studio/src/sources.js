/**
 * Source (object) mesh management, levels, gains, selection, and contribution UI.
 *
 * Extracted from app.js — createSourceOutline, createEffectiveRenderMarker/Line,
 * computeEffectiveRenderPosition, updateEffectiveRenderDecoration,
 * updateSourceDecorations, dbfsToScale, gainToMix, applySourceLevel,
 * applySpeakerLevel, getSelectedSourceGains, updateSourceColorsFromSelection,
 * updateSourceSelectionStyles, updateSpeakerColorsFromSelection,
 * setSelectedSource, getSourceMesh, updateSource, decayTrails,
 * updateSourceLevel, normalizeGainsPayload, updateSourceGains, removeSource,
 * clearSpeakers, getSelectedSourceContribution, updateSpeakerContributionUI,
 * getSelectedSpeakerContributionForObject, updateObjectContributionUI,
 * getObjectDisplayName, formatObjectLabel.
 */

import * as THREE from 'three';
import {
  sourceMeshes,
  sourceLabels,
  sourceOutlines,
  sourceLevels,
  sourceLevelLastSeen,
  sourceGains,
  sourceTrails,
  sourceEffectiveMarkers,
  sourceEffectiveLines,
  sourceNames,
  sourcePositionsRaw,
  sourceDirectSpeakerIndices,
  speakerMeshes,
  speakerLabels,
  speakerItems,
  objectItems,
  sourceBaseColors,
  objectMuted,
  objectManualMuted,
  objectBaseGains,
  objectGainCache,
  dirtyObjectMeters,
  dirtyObjectPositions,
  dirtyObjectLabels,
  app
} from './state.js';
import {
  hydrateObjectCoordinateState,
  normalizedOmniphonyToScenePosition
} from './coordinates.js';
import { scene } from './scene/setup.js';
import {
  sourceMaterial,
  sourceGeometry,
  speakerBaseColor,
  speakerHotColor,
  speakerSelectedColor,
  sourceHotColor,
  sourceDefaultEmissive,
  sourceNeutralEmissive,
  sourceContributionEmissive,
  sourceSelectedEmissive,
  sourceOutlineColor,
  sourceOutlineSelectedColor
} from './scene/materials.js';
import {
  createLabelSprite,
  setLabelSpriteText,
  updateSpeakerLabelsFromSelection
} from './scene/labels.js';
import { createTrailRenderable } from './trails.js';
import {
  linearToDb,
  meterToPercent,
  getBaseGain,
  getSoloTarget,
  sendObjectMute,
  applyGroupGains
} from './mute-solo.js';
import { formatNumber } from './coordinates.js';

// ---------------------------------------------------------------------------
// Callbacks that other modules populate to avoid circular imports.
// E.g. app.js sets these after importing this module.
// ---------------------------------------------------------------------------

export const sourceCallbacks = {
  renderObjectsList: null,
  updateObjectPositionUI: null,
  updateObjectLabelUI: null,
  updateObjectMeterUI: null,
  updateObjectDominantSpeakerUI: null,
  updateObjectControlsUI: null,
  updateSectionProportions: null,
  rebuildTrailGeometry: null,
  captureTrailPointColor: null,
  objectHasActiveTrail: null,
  getObjectIds: null
};

// ---------------------------------------------------------------------------
// Display name / label helpers
// ---------------------------------------------------------------------------

export function getObjectDisplayName(id) {
  const raw = sourceNames.get(id);
  if (raw && typeof raw === 'string' && raw.trim()) {
    return raw.trim();
  }
  return String(id);
}

export function formatObjectLabel(id) {
  const raw = sourceNames.get(id);
  if (raw && typeof raw === 'string') {
    const trimmed = raw.trim();
    const underscoreIndex = trimmed.indexOf('_');
    const cleaned = underscoreIndex >= 0 ? trimmed.slice(underscoreIndex + 1) : trimmed;
    if (cleaned) {
      return cleaned;
    }
  }
  return String(id);
}

const OBJECT_COLOR_PALETTE = [
  '#ff6b6b',
  '#4ecdc4',
  '#ffe66d',
  '#5dade2',
  '#af7ac5',
  '#f5b041',
  '#58d68d',
  '#ec7063',
  '#48c9b0',
  '#f4d03f',
  '#5499c7',
  '#a569bd',
  '#eb984e',
  '#45b39d',
  '#7fb3d5',
  '#f1948a'
];

function hashObjectId(id) {
  let hash = 2166136261;
  const text = String(id);
  for (let i = 0; i < text.length; i += 1) {
    hash ^= text.charCodeAt(i);
    hash = Math.imul(hash, 16777619);
  }
  return hash >>> 0;
}

export function getObjectBaseColor(id) {
  const key = String(id);
  let color = sourceBaseColors.get(key);
  if (!color) {
    const numericId = Number(key);
    const paletteIndex = Number.isInteger(numericId)
      ? Math.abs(numericId) % OBJECT_COLOR_PALETTE.length
      : hashObjectId(key) % OBJECT_COLOR_PALETTE.length;
    color = new THREE.Color(OBJECT_COLOR_PALETTE[paletteIndex]);
    sourceBaseColors.set(key, color.clone());
  }
  return color.clone();
}

export function getObjectTrailColor(id) {
  return getObjectBaseColor(id).offsetHSL(0, 0.04, 0.08);
}

export function getObjectUiAccent(id) {
  const color = getObjectBaseColor(id);
  return `rgb(${Math.round(color.r * 255)}, ${Math.round(color.g * 255)}, ${Math.round(color.b * 255)})`;
}

export function applyObjectItemColor(entry, id) {
  if (!entry?.root || !entry.idStrip) {
    return;
  }
  if (!app.objectColorsEnabled) {
    entry.root.classList.remove('object-colorized');
    entry.root.style.removeProperty('--object-accent');
    entry.idStrip.style.removeProperty('color');
    return;
  }
  entry.root.classList.add('object-colorized');
  entry.root.style.setProperty('--object-accent', getObjectUiAccent(id));
  entry.idStrip.style.color = '#edf5ff';
}

// ---------------------------------------------------------------------------
// Source outline / effective-render helpers
// ---------------------------------------------------------------------------

export function createSourceOutline() {
  const points = [];
  const segments = 64;
  for (let i = 0; i < segments; i += 1) {
    const a = (i / segments) * Math.PI * 2;
    points.push(new THREE.Vector3(Math.cos(a), Math.sin(a), 0));
  }

  const geometry = new THREE.BufferGeometry().setFromPoints(points);
  const material = new THREE.LineBasicMaterial({
    color: sourceOutlineColor.clone(),
    transparent: true,
    opacity: 0.98,
    depthTest: false,
    depthWrite: false
  });

  const outline = new THREE.LineLoop(geometry, material);
  outline.renderOrder = 20;
  return outline;
}

export function createEffectiveRenderMarker() {
  const geometry = new THREE.SphereGeometry(0.04, 18, 18);
  const material = new THREE.MeshStandardMaterial({
    color: 0x7ce7ff,
    emissive: 0x0a2834,
    transparent: true,
    opacity: 0.34,
    depthWrite: false
  });
  const marker = new THREE.Mesh(geometry, material);
  marker.renderOrder = 12;
  return marker;
}

export function createEffectiveRenderLine() {
  const geometry = new THREE.BufferGeometry();
  const material = new THREE.LineBasicMaterial({
    color: 0x7ce7ff,
    transparent: true,
    opacity: 0.22,
    depthWrite: false
  });
  const line = new THREE.Line(geometry, material);
  line.renderOrder = 11;
  return line;
}

// ---------------------------------------------------------------------------
// Effective render position computation
// ---------------------------------------------------------------------------

export function computeEffectiveRenderPosition(id) {
  const gains = sourceGains.get(String(id));
  if (!Array.isArray(gains) || gains.length === 0) {
    return null;
  }

  const weighted = new THREE.Vector3();
  let weightSum = 0;

  gains.forEach((rawGain, index) => {
    const gain = Number(rawGain) || 0;
    if (gain <= 0) {
      return;
    }
    const speakerMesh = speakerMeshes[index];
    if (!speakerMesh) {
      return;
    }
    const weight = gain * gain;
    weighted.addScaledVector(speakerMesh.position, weight);
    weightSum += weight;
  });

  if (weightSum <= 1e-9) {
    return null;
  }

  return weighted.multiplyScalar(1 / weightSum);
}

export function updateEffectiveRenderDecoration(id) {
  const mesh = sourceMeshes.get(id);
  const marker = sourceEffectiveMarkers.get(id);
  const line = sourceEffectiveLines.get(id);
  if (!mesh || !marker || !line) {
    return;
  }

  if (!app.effectiveRenderEnabled) {
    marker.visible = false;
    line.visible = false;
    line.geometry.setFromPoints([]);
    return;
  }

  const effectivePosition = computeEffectiveRenderPosition(id);
  if (!effectivePosition) {
    marker.visible = false;
    line.visible = false;
    line.geometry.setFromPoints([]);
    return;
  }

  marker.visible = true;
  marker.position.copy(effectivePosition);
  const markerScale = Math.max(0.035, (Number(mesh.scale.x) || 1) * 0.12);
  marker.scale.setScalar(markerScale);

  const isSelected = id === app.selectedSourceId;
  marker.material.opacity = isSelected ? 0.68 : 0.34;
  marker.material.emissive.setHex(isSelected ? 0x10566c : 0x0a2834);

  const offset = new THREE.Vector3().subVectors(effectivePosition, mesh.position);
  const distance = offset.length();
  if (distance <= 0.01) {
    line.visible = false;
    line.geometry.setFromPoints([]);
    return;
  }

  line.visible = true;
  line.material.opacity = isSelected ? 0.44 : 0.22;
  line.geometry.setFromPoints([mesh.position.clone(), effectivePosition.clone()]);
}

// ---------------------------------------------------------------------------
// Source decorations (label + outline positioning)
// ---------------------------------------------------------------------------

export function updateSourceDecorations(id) {
  const mesh = sourceMeshes.get(id);
  const label = sourceLabels.get(id);
  const outline = sourceOutlines.get(id);

  if (!mesh) {
    return;
  }

  if (label) {
    label.position.set(mesh.position.x, mesh.position.y, mesh.position.z);
  }

  if (outline) {
    const radius = 0.07 * mesh.scale.x * 1.08;
    outline.position.set(mesh.position.x, mesh.position.y, mesh.position.z);
    outline.scale.setScalar(radius);
  }

  updateEffectiveRenderDecoration(id);
}

// ---------------------------------------------------------------------------
// Level / gain helpers
// ---------------------------------------------------------------------------

export function dbfsToScale(dbfs, minScale, maxScale) {
  const clamped = Math.min(0, Math.max(-100, Number(dbfs ?? -100)));
  const normalized = (clamped + 100) / 100;
  return minScale + normalized * (maxScale - minScale);
}

export function gainToMix(gain) {
  return Math.min(1, Math.max(0, Number(gain ?? 0)));
}

export function applySourceLevel(id, mesh, meter) {
  const scale = dbfsToScale(meter?.rmsDbfs, 0.5, 2.4);
  mesh.userData.levelScale = scale;
  if (app.selectedSpeakerIndex === null) {
    mesh.scale.setScalar(scale);
  }
  updateSourceDecorations(id);
}

export function applySpeakerLevel(mesh, meter) {
  const scale = dbfsToScale(meter?.rmsDbfs, 0.65, 2.2);
  mesh.scale.setScalar(scale);
}

export function getSelectedSourceGains() {
  if (!app.selectedSourceId) {
    return null;
  }
  return sourceGains.get(app.selectedSourceId) || null;
}

// ---------------------------------------------------------------------------
// Contribution helpers (speaker / object)
// ---------------------------------------------------------------------------

export function getSelectedSourceContribution(index) {
  if (!app.selectedSourceId) {
    return null;
  }
  const gains = getSelectedSourceGains();
  if (!Array.isArray(gains)) {
    return null;
  }
  const rawGain = Number(gains[index]);
  if (!Number.isFinite(rawGain)) {
    return {
      gain: 0,
      gainDb: '-\u221E dB',
      resultDbfs: null,
      resultText: '\u2014 dBFS',
      percent: 0
    };
  }
  const resultDbfs = (() => {
    const sourceMeter = sourceLevels.get(app.selectedSourceId);
    const sourceRms = Number(sourceMeter?.rmsDbfs);
    if (!Number.isFinite(sourceRms) || rawGain <= 0) {
      return null;
    }
    return sourceRms + (20 * Math.log10(rawGain));
  })();
  return {
    gain: rawGain,
    gainDb: linearToDb(rawGain),
    resultDbfs,
    resultText: resultDbfs === null ? '\u2014 dBFS' : `${formatNumber(resultDbfs, 1)} dBFS`,
    percent: resultDbfs === null ? 0 : meterToPercent({ rmsDbfs: resultDbfs })
  };
}

export function updateSpeakerContributionUI(entry, id) {
  if (!entry?.contributionFill || !entry?.contributionSlider || !entry?.contributionValue) {
    return;
  }
  const contribution = getSelectedSourceContribution(Number(id));
  if (!app.selectedSourceId || !contribution) {
    entry.contributionFill.style.setProperty('--level', '0%');
    entry.meterFill.style.opacity = '1';
    entry.contributionSlider.style.visibility = 'hidden';
    entry.contributionValue.style.visibility = 'hidden';
    return;
  }

  entry.meterFill.style.opacity = '0.38';
  entry.contributionSlider.style.visibility = 'visible';
  entry.contributionValue.style.visibility = 'visible';
  entry.contributionFill.style.setProperty('--level', `${contribution.percent.toFixed(1)}%`);
  entry.contributionSlider.value = String(Math.max(0, Math.min(1, contribution.gain)));
  entry.contributionValue.textContent = `${contribution.gainDb} | ${contribution.resultText}`;
}

export function getSelectedSpeakerContributionForObject(id) {
  if (app.selectedSpeakerIndex === null) {
    return null;
  }
  const gains = sourceGains.get(String(id));
  if (!Array.isArray(gains)) {
    return null;
  }
  const rawGain = Number(gains[app.selectedSpeakerIndex]);
  if (!Number.isFinite(rawGain)) {
    return {
      gain: 0,
      gainDb: '-\u221E dB',
      resultDbfs: null,
      resultText: '\u2014 dBFS',
      percent: 0
    };
  }
  const sourceMeter = sourceLevels.get(String(id));
  const sourceRms = Number(sourceMeter?.rmsDbfs);
  const resultDbfs = (!Number.isFinite(sourceRms) || rawGain <= 0)
    ? null
    : sourceRms + (20 * Math.log10(rawGain));
  return {
    gain: rawGain,
    gainDb: linearToDb(rawGain),
    resultDbfs,
    resultText: resultDbfs === null ? '\u2014 dBFS' : `${formatNumber(resultDbfs, 1)} dBFS`,
    percent: resultDbfs === null ? 0 : meterToPercent({ rmsDbfs: resultDbfs })
  };
}

export function updateObjectContributionUI(entry, id) {
  if (!entry?.contributionFill || !entry?.gainSlider || !entry?.gainBox) {
    return;
  }
  const contribution = getSelectedSpeakerContributionForObject(id);
  if (app.selectedSpeakerIndex === null || !contribution) {
    entry.contributionFill.style.setProperty('--level', '0%');
    entry.meterFill.style.opacity = '1';
    entry.gainSlider.disabled = false;
    entry.gainSlider.style.visibility = 'visible';
    entry.gainBox.style.visibility = 'visible';
    const gainValue = getBaseGain(objectBaseGains, objectGainCache, id);
    entry.gainSlider.value = String(gainValue);
    entry.gainBox.textContent = linearToDb(gainValue);
    return;
  }

  entry.meterFill.style.opacity = '0.38';
  entry.contributionFill.style.setProperty('--level', `${contribution.percent.toFixed(1)}%`);
  entry.gainSlider.disabled = true;
  entry.gainSlider.style.visibility = 'visible';
  entry.gainBox.style.visibility = 'visible';
  entry.gainSlider.value = String(Math.max(0, Math.min(1, contribution.gain)));
  entry.gainBox.textContent = `${contribution.gainDb} | ${contribution.resultText}`;
}

// ---------------------------------------------------------------------------
// Selection styling
// ---------------------------------------------------------------------------

export function updateSourceColorsFromSelection() {
  sourceMeshes.forEach((mesh, id) => {
    const baseOpacity = Number(mesh.userData.baseOpacity ?? 0.7);
    const baseScale = Math.max(0.5, Number(mesh.userData.levelScale) || 1);
    const gains = app.selectedSpeakerIndex !== null ? (sourceGains.get(id) || null) : null;
    const mix = gainToMix(gains?.[app.selectedSpeakerIndex]);
    const hasContribution = mix > 1e-6;
    const contributionColor = speakerSelectedColor;
    const objectColor = app.objectColorsEnabled ? getObjectBaseColor(id) : sourceMaterial.color.clone();

    if (app.selectedSpeakerIndex !== null) {
      mesh.visible = true;
      mesh.material.color.copy(objectColor);
      if (hasContribution) {
        mesh.material.color.lerp(contributionColor, Math.min(0.68, 0.22 + (0.42 * mix)));
      }
      mesh.material.opacity = hasContribution
        ? Math.max(baseOpacity * (0.35 + (0.55 * mix)), 0.24)
        : 0.0;
      mesh.scale.setScalar(baseScale);
    } else {
      mesh.visible = true;
      mesh.material.color.copy(objectColor);
      mesh.material.opacity = 0.0;
      mesh.scale.setScalar(baseScale);
    }

    const outline = sourceOutlines.get(id);
    if (outline) {
      outline.visible = true;
      outline.material.opacity = app.selectedSpeakerIndex !== null
        ? (mix <= 1e-6 ? 0.15 : 0.25 + (0.73 * mix))
        : 0.98;
      if (app.selectedSpeakerIndex !== null) {
        outline.material.color.copy(app.objectColorsEnabled ? objectColor : sourceOutlineColor)
          .lerp(contributionColor, hasContribution ? mix * 0.65 : 0);
      } else {
        outline.material.color.copy(app.objectColorsEnabled ? objectColor : sourceOutlineColor);
      }
    }
  });
}

export function updateSourceSelectionStyles() {
  updateSourceColorsFromSelection();

  sourceMeshes.forEach((mesh, id) => {
    const isSelected = id === app.selectedSourceId;
    const gains = app.selectedSpeakerIndex !== null ? (sourceGains.get(id) || null) : null;
    const mix = gainToMix(gains?.[app.selectedSpeakerIndex]);
    const hasContribution = mix > 1e-6;
    if (isSelected) {
      mesh.material.emissive.copy(sourceSelectedEmissive);
    } else if (app.selectedSpeakerIndex !== null) {
      mesh.material.emissive.copy(hasContribution ? sourceContributionEmissive : sourceNeutralEmissive);
    } else {
      mesh.material.emissive.copy(sourceDefaultEmissive);
    }

    const outline = sourceOutlines.get(id);
    if (outline) {
      outline.material.color.copy(app.objectColorsEnabled ? getObjectBaseColor(id) : sourceOutlineColor);
      const selectedColor = app.selectedSpeakerIndex !== null
        ? sourceHotColor.clone().lerp(sourceOutlineSelectedColor, 0.55)
        : sourceOutlineSelectedColor;
      if (isSelected) {
        outline.material.color.copy(selectedColor);
      }
      if (isSelected) {
        outline.material.opacity = 1;
      }
    }

    updateEffectiveRenderDecoration(id);
    if ((sourceTrails.get(id)?.positions.length || 0) > 0) {
      sourceCallbacks.rebuildTrailGeometry?.(id);
    }

    const entry = objectItems.get(String(id));
    if (entry) {
      applyObjectItemColor(entry, id);
    }
  });
}

export function updateSpeakerColorsFromSelection() {
  const gains = getSelectedSourceGains();

  speakerMeshes.forEach((mesh, index) => {
    const mix = gainToMix(gains?.[index]);
    mesh.material.color.copy(speakerBaseColor).lerp(speakerHotColor, mix);
    if (app.selectedSpeakerIndex !== null && index === app.selectedSpeakerIndex) {
      mesh.material.color.copy(speakerSelectedColor);
    }

    const baseOpacity = Number(mesh.userData.baseOpacity ?? 0.65);
    if (!app.selectedSourceId) {
      mesh.material.opacity = baseOpacity;
      return;
    }

    mesh.material.opacity = mix <= 1e-6 ? Math.min(baseOpacity, 0.08) : baseOpacity;
  });

  updateSpeakerLabelsFromSelection();
}

export function setSelectedSource(id) {
  const nextId = id === null || id === undefined ? null : String(id);
  const currentSolo = getSoloTarget('object');
  if (nextId !== null && currentSolo && currentSolo !== nextId) {
    const ids = sourceCallbacks.getObjectIds?.() ?? [];
    ids.forEach((objId) => {
      const shouldMute = objId !== nextId;
      if (shouldMute) {
        objectMuted.add(objId);
      } else {
        objectMuted.delete(objId);
        objectManualMuted.delete(objId);
      }
      sendObjectMute(objId, shouldMute);
    });
    applyGroupGains('object');
  }
  app.selectedSourceId = nextId;
  updateSourceSelectionStyles();
  updateSpeakerColorsFromSelection();
  sourceCallbacks.updateObjectControlsUI?.();
}

// ---------------------------------------------------------------------------
// Source mesh lifecycle
// ---------------------------------------------------------------------------

export function getSourceMesh(id) {
  if (!sourceMeshes.has(id)) {
    const mesh = new THREE.Mesh(sourceGeometry, sourceMaterial.clone());
    const objectColor = getObjectBaseColor(id);
    const trailColor = getObjectTrailColor(id);
    mesh.material.color.copy(objectColor);
    mesh.material.emissive.copy(sourceDefaultEmissive);
    mesh.material.opacity = 0.0;
    mesh.material.depthWrite = false;
    mesh.userData.sourceId = id;
    mesh.userData.baseOpacity = sourceMaterial.opacity;
    mesh.userData.objectBaseColor = objectColor.clone();
    mesh.userData.objectTrailColor = trailColor.clone();

    const outline = createSourceOutline();
    const trailLine = createTrailRenderable();
    const effectiveMarker = createEffectiveRenderMarker();
    const effectiveLine = createEffectiveRenderLine();
    trailLine.visible = app.trailsEnabled;
    effectiveMarker.visible = false;
    effectiveLine.visible = false;
    scene.add(mesh);
    scene.add(outline);
    scene.add(trailLine);
    scene.add(effectiveLine);
    scene.add(effectiveMarker);

    const label = createLabelSprite(formatObjectLabel(id));
    label.userData.sourceId = id;
    scene.add(label);

    sourceMeshes.set(id, mesh);
    sourceLabels.set(id, label);
    sourceOutlines.set(id, outline);
    sourceTrails.set(id, { positions: [], line: trailLine });
    sourceEffectiveMarkers.set(id, effectiveMarker);
    sourceEffectiveLines.set(id, effectiveLine);
    applySourceLevel(id, mesh, sourceLevels.get(id));
    updateSourceSelectionStyles();
  }
  return sourceMeshes.get(id);
}

export function updateSource(id, position) {
  const mesh = getSourceMesh(id);
  const skipTrail = Boolean(position && position._noTrail);
  const now = performance.now();
  const directSpeakerIndex = Number.isInteger(position?.directSpeakerIndex)
    ? position.directSpeakerIndex
    : null;
  const raw = hydrateObjectCoordinateState({
    x: Number(position.x) || 0,
    y: Number(position.y) || 0,
    z: Number(position.z) || 0,
    coordMode: position?.coordMode,
    azimuthDeg: Number.isFinite(Number(position?.azimuthDeg)) ? Number(position.azimuthDeg) : undefined,
    elevationDeg: Number.isFinite(Number(position?.elevationDeg)) ? Number(position.elevationDeg) : undefined,
    distanceM: Number.isFinite(Number(position?.distanceM)) ? Number(position.distanceM) : undefined,
    directSpeakerIndex,
    t: now
  });
  sourcePositionsRaw.set(String(id), raw);
  if (directSpeakerIndex !== null) {
    sourceDirectSpeakerIndices.set(String(id), directSpeakerIndex);
    const speakerMesh = speakerMeshes[directSpeakerIndex];
    if (speakerMesh) {
      mesh.position.copy(speakerMesh.position);
    } else {
      const scenePos = normalizedOmniphonyToScenePosition(raw);
      mesh.position.set(scenePos.x, scenePos.y, scenePos.z);
    }
  } else {
    sourceDirectSpeakerIndices.delete(String(id));
    const scenePos = normalizedOmniphonyToScenePosition(raw);
    mesh.position.set(scenePos.x, scenePos.y, scenePos.z);
  }

  const trail = sourceTrails.get(id);
  if (trail && !skipTrail) {
    trail.positions.push({
      ...raw,
      trailColor: sourceCallbacks.captureTrailPointColor?.(mesh)
    });
    if (app.trailsEnabled) {
      sourceCallbacks.rebuildTrailGeometry?.(id);
    }
  }

  updateSourceDecorations(id);
  if (position && typeof position.name === 'string' && position.name.trim()) {
    sourceNames.set(String(id), position.name.trim());
  }
  const label = sourceLabels.get(id);
  if (label) {
    setLabelSpriteText(label, formatObjectLabel(String(id)));
  }
  const key = String(id);
  if (!objectItems.has(key)) {
    sourceCallbacks.renderObjectsList?.();
  } else {
    sourceCallbacks.updateObjectPositionUI?.(key, raw);
    sourceCallbacks.updateObjectLabelUI?.(key);
  }
  const entry = objectItems.get(key);
  if (entry) {
    entry.root.classList.toggle('has-active-trail', sourceCallbacks.objectHasActiveTrail?.(key) ?? false);
    applyObjectItemColor(entry, key);
  }
}

export function decayTrails(nowMs) {
  // Decay trails a few times per second; no need to run every frame.
  if (nowMs - app.lastTrailDecayAt < 120) return;
  app.lastTrailDecayAt = nowMs;

  const cutoff = nowMs - app.trailPointTtlMs;
  sourceTrails.forEach((trail, id) => {
    const before = trail.positions.length;
    if (before === 0) return;

    // Keep points with recent timestamps. Legacy points without timestamp are
    // treated as stale and dropped on first decay pass.
    trail.positions = trail.positions.filter((p) => typeof p.t === 'number' && p.t >= cutoff);
    if (trail.positions.length !== before) {
      sourceCallbacks.rebuildTrailGeometry?.(id);
      const entry = objectItems.get(String(id));
      if (entry) {
        entry.root.classList.toggle('has-active-trail', trail.positions.length > 0);
      }
    }
  });
}

export function updateSourceLevel(id, meter) {
  const key = String(id);
  sourceLevels.set(key, {
    peakDbfs: Number(meter?.peakDbfs ?? -100),
    rmsDbfs: Number(meter?.rmsDbfs ?? -100)
  });
  sourceLevelLastSeen.set(key, performance.now());
  const mesh = sourceMeshes.get(id);
  if (mesh) {
    applySourceLevel(id, mesh, sourceLevels.get(key));
  }
  if (app.selectedSourceId === key) {
    speakerItems.forEach((entry, speakerId) => {
      updateSpeakerContributionUI(entry, speakerId);
    });
  }
  if (app.selectedSpeakerIndex !== null) {
    const entry = objectItems.get(key);
    if (entry) {
      updateObjectContributionUI(entry, key);
    }
  }
  sourceCallbacks.updateObjectMeterUI?.(key);
}

export function normalizeGainsPayload(payload) {
  if (Array.isArray(payload)) {
    return payload;
  }
  if (payload && Array.isArray(payload.gains)) {
    return payload.gains;
  }
  return [];
}

export function updateSourceGains(id, gainsPayload) {
  sourceGains.set(id, normalizeGainsPayload(gainsPayload));
  sourceCallbacks.updateObjectDominantSpeakerUI?.(String(id));
  if (app.selectedSourceId === String(id)) {
    speakerItems.forEach((entry, speakerId) => {
      updateSpeakerContributionUI(entry, speakerId);
    });
  }
  if (app.selectedSpeakerIndex !== null) {
    const entry = objectItems.get(String(id));
    if (entry) {
      updateObjectContributionUI(entry, String(id));
    }
  }
  updateEffectiveRenderDecoration(id);
  if (app.selectedSourceId === id) {
    updateSpeakerColorsFromSelection();
  }
  if (app.selectedSpeakerIndex !== null) {
    updateSourceSelectionStyles();
  }
}

export function removeSource(id) {
  const mesh = sourceMeshes.get(id);
  if (!mesh) return;
  const label = sourceLabels.get(id);
  scene.remove(mesh);
  if (label) {
    scene.remove(label);
    label.material.map.dispose();
    label.material.dispose();
  }
  const outline = sourceOutlines.get(id);
  if (outline) {
    scene.remove(outline);
    outline.geometry.dispose();
    outline.material.dispose();
  }
  const trail = sourceTrails.get(id);
  if (trail) {
    scene.remove(trail.line);
    trail.line.geometry.dispose();
    trail.line.material.dispose();
    sourceTrails.delete(id);
  }
  const effectiveMarker = sourceEffectiveMarkers.get(id);
  if (effectiveMarker) {
    scene.remove(effectiveMarker);
    effectiveMarker.geometry.dispose();
    effectiveMarker.material.dispose();
    sourceEffectiveMarkers.delete(id);
  }
  const effectiveLine = sourceEffectiveLines.get(id);
  if (effectiveLine) {
    scene.remove(effectiveLine);
    effectiveLine.geometry.dispose();
    effectiveLine.material.dispose();
    sourceEffectiveLines.delete(id);
  }
  mesh.geometry.dispose();
  mesh.material.dispose();
  sourceMeshes.delete(id);
  sourceLabels.delete(id);
  sourceLevels.delete(id);
  sourceLevelLastSeen.delete(String(id));
  sourceGains.delete(id);
  sourceOutlines.delete(id);
  sourceBaseColors.delete(String(id));
  sourceNames.delete(String(id));
  sourcePositionsRaw.delete(String(id));
  sourceDirectSpeakerIndices.delete(String(id));
  dirtyObjectMeters.delete(String(id));
  dirtyObjectPositions.delete(String(id));
  dirtyObjectLabels.delete(String(id));

  if (app.selectedSourceId === id) {
    setSelectedSource(null);
  }
  objectMuted.delete(String(id));
  objectManualMuted.delete(String(id));
  objectBaseGains.delete(String(id));
  const entry = objectItems.get(String(id));
  if (entry) {
    entry.root.remove();
    objectItems.delete(String(id));
  }
  sourceCallbacks.updateObjectControlsUI?.();
  sourceCallbacks.updateSectionProportions?.();
}

export function clearSpeakers() {
  speakerMeshes.forEach((mesh) => {
    scene.remove(mesh);
    mesh.geometry.dispose();
    mesh.material.dispose();
  });
  speakerLabels.forEach((label) => {
    scene.remove(label);
    label.material.map.dispose();
    label.material.dispose();
  });
  speakerMeshes.length = 0;
  speakerLabels.length = 0;
}
