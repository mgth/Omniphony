import { app, sourceTrails } from '../state.js';
import { setLocale } from '../i18n.js';
import { persistTrailPrefs, persistEffectiveRenderPrefs, refreshEffectiveRenderVisibility } from '../controls/room-geometry.js';
import { rebuildTrailGeometry, createTrailRenderable } from '../trails.js';
import { scene } from '../scene/setup.js';
import { updateSourceSelectionStyles } from '../sources.js';
import { refreshOverlayLists } from '../speakers.js';

export function setupTrailsAndDisplayListeners() {
  const trailToggleEl = document.getElementById('trailToggle');
  const effectiveRenderToggleEl = document.getElementById('effectiveRenderToggle');
  const objectColorsToggleEl = document.getElementById('objectColorsToggle');
  const trailModeSelectEl = document.getElementById('trailModeSelect');
  const trailTtlSliderEl = document.getElementById('trailTtlSlider');
  const trailTtlValEl = document.getElementById('trailTtlVal');
  const localeSelectEl = document.getElementById('localeSelect');

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
}
