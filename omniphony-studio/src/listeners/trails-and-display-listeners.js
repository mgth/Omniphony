import { app, sourceMeshes, sourceTrails, speakerLabels, speakerLevels, speakerMeshes } from '../state.js';
import { setLocale } from '../i18n.js';
import { persistTrailPrefs, persistEffectiveRenderPrefs, refreshEffectiveRenderVisibility } from '../controls/room-geometry.js';
import { rebuildTrailGeometry, createTrailRenderable } from '../trails.js';
import { scene } from '../scene/setup.js';
import { applySpeakerLevel, updateSourceDecorations, updateSourceSelectionStyles } from '../sources.js';
import { refreshOverlayLists, updateSpeakerVisualsFromState } from '../speakers.js';
import { requestSpeakerHeatmapIfNeeded } from '../scene/speaker-heatmap.js';

export function setupTrailsAndDisplayListeners() {
  const trailToggleEl = document.getElementById('trailToggle');
  const effectiveRenderToggleEl = document.getElementById('effectiveRenderToggle');
  const objectColorsToggleEl = document.getElementById('objectColorsToggle');
  const objectDisplayModeSelectEl = document.getElementById('objectDisplayModeSelect');
  const objectSphereSizeSliderEl = document.getElementById('objectSphereSizeSlider');
  const objectSphereSizeValEl = document.getElementById('objectSphereSizeVal');
  const objectLabelsToggleEl = document.getElementById('objectLabelsToggle');
  const speakerLabelsToggleEl = document.getElementById('speakerLabelsToggle');
  const speakerSizeSliderEl = document.getElementById('speakerSizeSlider');
  const speakerSizeValEl = document.getElementById('speakerSizeVal');
  const trailModeSelectEl = document.getElementById('trailModeSelect');
  const trailTtlSliderEl = document.getElementById('trailTtlSlider');
  const trailTtlValEl = document.getElementById('trailTtlVal');
  const localeSelectEl = document.getElementById('localeSelect');
  const speakerHeatmapSlicesToggleEl = document.getElementById('speakerHeatmapSlicesToggle');
  const speakerHeatmapVolumeToggleEl = document.getElementById('speakerHeatmapVolumeToggle');
  const speakerHeatmapSampleCountInputEl = document.getElementById('speakerHeatmapSampleCountInput');
  const speakerHeatmapMaxSphereSizeSliderEl = document.getElementById('speakerHeatmapMaxSphereSizeSlider');
  const speakerHeatmapMaxSphereSizeValEl = document.getElementById('speakerHeatmapMaxSphereSizeVal');

  if (trailToggleEl) {
    trailToggleEl.addEventListener('change', () => {
      app.trailsEnabled = trailToggleEl.checked;
      sourceTrails.forEach((trail, id) => {
        trail.line.visible = app.trailsEnabled;
        if (app.trailsEnabled) {
          rebuildTrailGeometry(id);
        }
      });
      persistTrailPrefs();
    });
  }

  if (effectiveRenderToggleEl) {
    effectiveRenderToggleEl.addEventListener('change', () => {
      app.effectiveRenderEnabled = effectiveRenderToggleEl.checked;
      refreshEffectiveRenderVisibility();
      persistEffectiveRenderPrefs();
    });
  }

  if (objectColorsToggleEl) {
    objectColorsToggleEl.checked = app.objectColorsEnabled;
    objectColorsToggleEl.addEventListener('change', () => {
      app.objectColorsEnabled = objectColorsToggleEl.checked;
      updateSourceSelectionStyles();
      sourceTrails.forEach((_trail, id) => {
        rebuildTrailGeometry(id);
      });
      refreshOverlayLists();
      persistEffectiveRenderPrefs();
    });
  }

  if (objectDisplayModeSelectEl) {
    objectDisplayModeSelectEl.value = app.objectDisplayMode;
    objectDisplayModeSelectEl.addEventListener('change', () => {
      const nextMode = objectDisplayModeSelectEl.value;
      app.objectDisplayMode = nextMode === 'transparent-sphere' || nextMode === 'diffuse-sphere'
        ? nextMode
        : 'circle';
      updateSourceSelectionStyles();
      persistEffectiveRenderPrefs();
    });
  }

  if (objectSphereSizeSliderEl) {
    objectSphereSizeSliderEl.value = String(app.objectSphereSize);
    if (objectSphereSizeValEl) {
      objectSphereSizeValEl.textContent = app.objectSphereSize.toFixed(3);
    }
    objectSphereSizeSliderEl.addEventListener('input', () => {
      const nextSize = Number(objectSphereSizeSliderEl.value);
      app.objectSphereSize = Math.max(0.03, Math.min(0.2, Number.isFinite(nextSize) ? nextSize : 0.07));
      if (objectSphereSizeValEl) {
        objectSphereSizeValEl.textContent = app.objectSphereSize.toFixed(3);
      }
      updateSourceSelectionStyles();
      sourceMeshes.forEach((_mesh, id) => {
        updateSourceDecorations(id);
      });
      persistEffectiveRenderPrefs();
    });
  }

  if (objectLabelsToggleEl) {
    objectLabelsToggleEl.checked = app.objectLabelsEnabled;
    objectLabelsToggleEl.addEventListener('change', () => {
      app.objectLabelsEnabled = objectLabelsToggleEl.checked;
      sourceMeshes.forEach((_mesh, id) => {
        updateSourceDecorations(id);
      });
      persistEffectiveRenderPrefs();
    });
  }

  if (speakerLabelsToggleEl) {
    speakerLabelsToggleEl.checked = app.speakerLabelsEnabled;
    speakerLabelsToggleEl.addEventListener('change', () => {
      app.speakerLabelsEnabled = speakerLabelsToggleEl.checked;
      speakerLabels.forEach((label) => {
        if (label) {
          label.visible = app.speakerLabelsEnabled;
        }
      });
      persistEffectiveRenderPrefs();
    });
  }

  if (speakerSizeSliderEl) {
    speakerSizeSliderEl.value = String(app.speakerSize);
    if (speakerSizeValEl) {
      speakerSizeValEl.textContent = app.speakerSize.toFixed(3);
    }
    speakerSizeSliderEl.addEventListener('input', () => {
      const nextSize = Number(speakerSizeSliderEl.value);
      app.speakerSize = Math.max(0.04, Math.min(0.2, Number.isFinite(nextSize) ? nextSize : 0.08));
      if (speakerSizeValEl) {
        speakerSizeValEl.textContent = app.speakerSize.toFixed(3);
      }
      speakerMeshes.forEach((mesh, index) => {
        applySpeakerLevel(mesh, speakerLevels.get(String(index)));
        updateSpeakerVisualsFromState(index);
      });
      persistEffectiveRenderPrefs();
    });
  }

  if (trailModeSelectEl) {
    trailModeSelectEl.value = app.trailRenderMode;
    trailModeSelectEl.addEventListener('change', () => {
      app.trailRenderMode = trailModeSelectEl.value === 'line' ? 'line' : 'diffuse';
      sourceTrails.forEach((trail, id) => {
        const wasVisible = trail.line.visible;
        scene.remove(trail.line);
        trail.line.geometry.dispose();
        trail.line.material.dispose();
        trail.line = createTrailRenderable();
        trail.line.visible = wasVisible;
        scene.add(trail.line);
        if (app.trailsEnabled) {
          rebuildTrailGeometry(id);
        }
      });
      persistTrailPrefs();
    });
  }

  if (trailTtlSliderEl) {
    trailTtlSliderEl.addEventListener('input', () => {
      const seconds = Number(trailTtlSliderEl.value);
      app.trailPointTtlMs = Math.max(500, seconds * 1000);
      if (trailTtlValEl) trailTtlValEl.textContent = `${seconds.toFixed(1)}s`;
      persistTrailPrefs();
    });
  }

  if (localeSelectEl) {
    localeSelectEl.addEventListener('change', () => {
      setLocale(localeSelectEl.value || 'auto');
    });
  }

  if (speakerHeatmapSlicesToggleEl) {
    speakerHeatmapSlicesToggleEl.checked = app.speakerHeatmapSlicesEnabled;
    speakerHeatmapSlicesToggleEl.addEventListener('change', () => {
      app.speakerHeatmapSlicesEnabled = speakerHeatmapSlicesToggleEl.checked;
      requestSpeakerHeatmapIfNeeded();
      persistEffectiveRenderPrefs();
    });
  }

  if (speakerHeatmapVolumeToggleEl) {
    speakerHeatmapVolumeToggleEl.checked = app.speakerHeatmapVolumeEnabled;
    speakerHeatmapVolumeToggleEl.addEventListener('change', () => {
      app.speakerHeatmapVolumeEnabled = speakerHeatmapVolumeToggleEl.checked;
      requestSpeakerHeatmapIfNeeded();
      persistEffectiveRenderPrefs();
    });
  }

  if (speakerHeatmapSampleCountInputEl) {
    speakerHeatmapSampleCountInputEl.value = String(app.speakerHeatmapSampleCount);
    speakerHeatmapSampleCountInputEl.addEventListener('change', () => {
      const nextCount = Number(speakerHeatmapSampleCountInputEl.value);
      app.speakerHeatmapSampleCount = Math.max(128, Math.min(20000, Math.round(Number.isFinite(nextCount) ? nextCount : 3072)));
      speakerHeatmapSampleCountInputEl.value = String(app.speakerHeatmapSampleCount);
      requestSpeakerHeatmapIfNeeded();
      persistEffectiveRenderPrefs();
    });
  }

  if (speakerHeatmapMaxSphereSizeSliderEl) {
    speakerHeatmapMaxSphereSizeSliderEl.value = String(app.speakerHeatmapMaxSphereSize);
    if (speakerHeatmapMaxSphereSizeValEl) {
      speakerHeatmapMaxSphereSizeValEl.textContent = app.speakerHeatmapMaxSphereSize.toFixed(3);
    }
    speakerHeatmapMaxSphereSizeSliderEl.addEventListener('input', () => {
      const nextSize = Number(speakerHeatmapMaxSphereSizeSliderEl.value);
      app.speakerHeatmapMaxSphereSize = Math.max(0.01, Math.min(0.2, Number.isFinite(nextSize) ? nextSize : 0.062));
      if (speakerHeatmapMaxSphereSizeValEl) {
        speakerHeatmapMaxSphereSizeValEl.textContent = app.speakerHeatmapMaxSphereSize.toFixed(3);
      }
      requestSpeakerHeatmapIfNeeded();
      persistEffectiveRenderPrefs();
    });
  }
}
