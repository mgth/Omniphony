import { app, sourceMeshes, sourceTrails, speakerLabels, speakerBandBars, speakerLevels, speakerMeshes } from '../state.js';
import { setLocale } from '../i18n.js';
import { persistTrailPrefs, persistEffectiveRenderPrefs, refreshEffectiveRenderVisibility } from '../controls/room-geometry.js';
import { rebuildTrailGeometry, createTrailRenderable } from '../trails.js';
import { scene } from '../scene/setup.js';
import { applySpeakerLevel, updateSourceDecorations, updateSourceSelectionStyles, applyObjectsVisibility } from '../sources.js';
import { refreshOverlayLists, updateSpeakerVisualsFromState, refreshSpeakerOrientations } from '../speakers.js';
import { syncCrossoverBandSelects } from '../scene/speaker-band-select.js';
import { acquireGainTable, releaseGainTable, refreshGaintableSubscription } from '../scene/speaker-gaintable.js';
import { refreshObjectEnergyVolume } from '../scene/object-energy-volume.js';
import { clampVolumeGamma, colormapIndex } from '../scene/object-energy-shared.js';
import {
  onOverlayState,
  getMpvOverlayStatus,
  setMpvOverlayEnabled,
  setMpvOverlayHeatmapEnabled,
  setMpvOverlayHeatmapCustomStops,
  pushMpvOverlayTrailPrefs
} from '../mpvOverlay.js';
import { registerGradientEditor, setGradientEditorOnChange } from '../scene/gradient-editor.js';
import { invoke } from '@tauri-apps/api/core';

const GAINTABLE_CONSUMER_SOLO = 'speakerSoloVolume';
const GAINTABLE_CONSUMER_GLOBAL = 'globalEnergyVolume';

export function setupTrailsAndDisplayListeners() {
  const trailToggleEl = document.getElementById('trailToggle');
  const effectiveRenderToggleEl = document.getElementById('effectiveRenderToggle');
  const showObjectsToggleEl = document.getElementById('showObjectsToggle');
  const objectColorsToggleEl = document.getElementById('objectColorsToggle');
  const objectDisplayModeSelectEl = document.getElementById('objectDisplayModeSelect');
  const objectSphereSizeSliderEl = document.getElementById('objectSphereSizeSlider');
  const objectSphereSizeValEl = document.getElementById('objectSphereSizeVal');
  const objectLabelsToggleEl = document.getElementById('objectLabelsToggle');
  const showObjectDetailsToggleEl = document.getElementById('showObjectDetailsToggle');
  const objectDetailsToggleBtnEl = document.getElementById('objectDetailsToggleBtn');
  const speakerLabelsToggleEl = document.getElementById('speakerLabelsToggle');
  const speakerBandBarsToggleEl = document.getElementById('speakerBandBarsToggle');
  const speakerFaceListenerToggleEl = document.getElementById('speakerFaceListenerToggle');
  const speakerSizeSliderEl = document.getElementById('speakerSizeSlider');
  const speakerSizeValEl = document.getElementById('speakerSizeVal');
  const trailModeSelectEl = document.getElementById('trailModeSelect');
  const trailTtlSliderEl = document.getElementById('trailTtlSlider');
  const trailTtlValEl = document.getElementById('trailTtlVal');
  const trailTeleportSliderEl = document.getElementById('trailTeleportSlider');
  const trailTeleportValEl = document.getElementById('trailTeleportVal');
  const localeSelectEl = document.getElementById('localeSelect');
  const speakerHeatmapVolumeToggleEl = document.getElementById('speakerHeatmapVolumeToggle');
  const speakerHeatmapVolumeColormapEl = document.getElementById('speakerHeatmapVolumeColormap');
  const speakerHeatmapBandSelectEl = document.getElementById('speakerHeatmapBandSelect');
  const objectEnergyHeatmapToggleEl = document.getElementById('objectEnergyHeatmapToggle');
  const objectEnergyColormapEl = document.getElementById('objectEnergyColormap');
  const objectEnergyVolumeMixSliderEl = document.getElementById('objectEnergyVolumeMixSlider');
  const objectEnergyVolumeMixValEl = document.getElementById('objectEnergyVolumeMixVal');
  const objectEnergyVolumeGammaAccumulateSliderEl = document.getElementById('objectEnergyVolumeGammaAccumulateSlider');
  const objectEnergyVolumeGammaAccumulateValEl = document.getElementById('objectEnergyVolumeGammaAccumulateVal');
  const objectEnergyVolumeGammaMipSliderEl = document.getElementById('objectEnergyVolumeGammaMipSlider');
  const objectEnergyVolumeGammaMipValEl = document.getElementById('objectEnergyVolumeGammaMipVal');
  const objectEnergyHeatmapResolutionSliderEl = document.getElementById('objectEnergyHeatmapResolutionSlider');
  const objectEnergyHeatmapResolutionValEl = document.getElementById('objectEnergyHeatmapResolutionVal');
  const volumeRefreshSliderEl = document.getElementById('volumeRefreshSlider');
  const volumeRefreshValEl = document.getElementById('volumeRefreshVal');
  const objectEnergyHeatmapRadiusSliderEl = document.getElementById('objectEnergyHeatmapRadiusSlider');
  const objectEnergyHeatmapRadiusValEl = document.getElementById('objectEnergyHeatmapRadiusVal');
  const objectEnergyHeatmapOpacitySliderEl = document.getElementById('objectEnergyHeatmapOpacitySlider');
  const objectEnergyHeatmapOpacityValEl = document.getElementById('objectEnergyHeatmapOpacityVal');
  const volumeSmoothToggleEl = document.getElementById('volumeSmoothToggle');
  const mpvOverlayToggleEl = document.getElementById('mpvOverlayToggle');

  if (mpvOverlayToggleEl) {
    mpvOverlayToggleEl.checked = getMpvOverlayStatus().enabled;
    mpvOverlayToggleEl.addEventListener('change', async () => {
      await setMpvOverlayEnabled(mpvOverlayToggleEl.checked);
    });
  }

  function applyObjectDetailsVisibility(enabled) {
    app.showObjectDetails = Boolean(enabled);
    if (showObjectDetailsToggleEl) {
      showObjectDetailsToggleEl.checked = app.showObjectDetails;
    }
    if (objectDetailsToggleBtnEl) {
      objectDetailsToggleBtnEl.classList.toggle('active', app.showObjectDetails);
      objectDetailsToggleBtnEl.setAttribute('aria-pressed', app.showObjectDetails ? 'true' : 'false');
      objectDetailsToggleBtnEl.textContent = app.showObjectDetails ? '▾' : '▸';
    }
    document.body.classList.toggle('hide-object-details', !app.showObjectDetails);
  }

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
      pushMpvOverlayTrailPrefs(
        app.trailsEnabled, app.trailPointTtlMs,
        app.trailRenderMode, app.trailTeleportThreshold
      );
    });
  }

  if (effectiveRenderToggleEl) {
    effectiveRenderToggleEl.addEventListener('change', () => {
      app.effectiveRenderEnabled = effectiveRenderToggleEl.checked;
      refreshEffectiveRenderVisibility();
      persistEffectiveRenderPrefs();
    });
  }

  if (showObjectsToggleEl) {
    showObjectsToggleEl.checked = app.objectsVisible !== false;
    showObjectsToggleEl.addEventListener('change', () => {
      app.objectsVisible = showObjectsToggleEl.checked;
      applyObjectsVisibility();
      invoke('mpv_overlay_set_objects', { visible: app.objectsVisible }).catch(() => {});
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
      // Mirror the toggle to the in-process mpv overlay (OSC control).
      invoke('mpv_overlay_set_labels', { enabled: app.objectLabelsEnabled }).catch(() => {});
      persistEffectiveRenderPrefs();
    });
  }

  if (showObjectDetailsToggleEl) {
    applyObjectDetailsVisibility(app.showObjectDetails);
    showObjectDetailsToggleEl.addEventListener('change', () => {
      applyObjectDetailsVisibility(showObjectDetailsToggleEl.checked);
      persistEffectiveRenderPrefs();
    });
  }

  if (objectDetailsToggleBtnEl) {
    applyObjectDetailsVisibility(app.showObjectDetails);
    objectDetailsToggleBtnEl.addEventListener('click', () => {
      applyObjectDetailsVisibility(!app.showObjectDetails);
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

  if (speakerBandBarsToggleEl) {
    speakerBandBarsToggleEl.checked = app.speakerBandBarsEnabled;
    speakerBandBarsToggleEl.addEventListener('change', () => {
      app.speakerBandBarsEnabled = speakerBandBarsToggleEl.checked;
      speakerBandBars.forEach((bar) => {
        if (bar) {
          bar.visible = app.speakerBandBarsEnabled;
        }
      });
      persistEffectiveRenderPrefs();
    });
  }

  if (speakerFaceListenerToggleEl) {
    speakerFaceListenerToggleEl.checked = app.speakerFaceListenerEnabled;
    speakerFaceListenerToggleEl.addEventListener('change', () => {
      app.speakerFaceListenerEnabled = speakerFaceListenerToggleEl.checked;
      refreshSpeakerOrientations();
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
      pushMpvOverlayTrailPrefs(
        app.trailsEnabled, app.trailPointTtlMs,
        app.trailRenderMode, app.trailTeleportThreshold
      );
    });
  }

  if (trailTtlSliderEl) {
    trailTtlSliderEl.addEventListener('input', () => {
      const seconds = Number(trailTtlSliderEl.value);
      app.trailPointTtlMs = Math.max(500, seconds * 1000);
      if (trailTtlValEl) trailTtlValEl.textContent = `${seconds.toFixed(1)}s`;
      persistTrailPrefs();
      pushMpvOverlayTrailPrefs(
        app.trailsEnabled, app.trailPointTtlMs,
        app.trailRenderMode, app.trailTeleportThreshold
      );
    });
  }

  if (trailTeleportSliderEl) {
    trailTeleportSliderEl.addEventListener('input', () => {
      const value = Number(trailTeleportSliderEl.value);
      app.trailTeleportThreshold = Math.max(0.05, Math.min(2.0, Number.isFinite(value) ? value : 0.5));
      if (trailTeleportValEl) {
        trailTeleportValEl.textContent = app.trailTeleportThreshold.toFixed(2);
      }
      // Rebuild every visible trail so the new threshold takes effect on
      // already-buffered points (we evaluate breaks at rebuild time from
      // the raw consecutive positions).
      sourceTrails.forEach((_trail, id) => {
        rebuildTrailGeometry(id);
      });
      persistTrailPrefs();
      pushMpvOverlayTrailPrefs(
        app.trailsEnabled, app.trailPointTtlMs,
        app.trailRenderMode, app.trailTeleportThreshold
      );
    });
  }

  if (localeSelectEl) {
    localeSelectEl.addEventListener('change', () => {
      setLocale(localeSelectEl.value || 'auto');
    });
  }

  if (speakerHeatmapVolumeToggleEl) {
    speakerHeatmapVolumeToggleEl.checked = app.speakerHeatmapVolumeEnabled;
    // Honour a persisted-on state at startup: register as a gain-table consumer.
    if (app.speakerHeatmapVolumeEnabled) acquireGainTable(GAINTABLE_CONSUMER_SOLO);
    speakerHeatmapVolumeToggleEl.addEventListener('change', () => {
      app.speakerHeatmapVolumeEnabled = speakerHeatmapVolumeToggleEl.checked;
      app.lastSpeakerSoloVolumeAt = 0; // rebuild on the next tick
      // The volume renders locally from the gain table: subscribe while shown,
      // release (unsubscribe when last) when hidden.
      if (app.speakerHeatmapVolumeEnabled) {
        acquireGainTable(GAINTABLE_CONSUMER_SOLO);
      } else {
        releaseGainTable(GAINTABLE_CONSUMER_SOLO);
      }
      persistEffectiveRenderPrefs();
    });
  }

  // Show the gradient editor under whichever selector is set to 'custom'.
  const updateGradientEditorVisibility = () => {
    const objEd = document.getElementById('objectGradientEditor');
    const spkEd = document.getElementById('speakerGradientEditor');
    if (objEd) objEd.style.display = app.objectEnergyColormap === 'custom' ? '' : 'none';
    if (spkEd) spkEd.style.display = app.speakerHeatmapVolumeColormap === 'custom' ? '' : 'none';
  };

  // Speaker heatmap volume's own gradient (Studio 3D only — no mpv overlay).
  if (speakerHeatmapVolumeColormapEl) {
    speakerHeatmapVolumeColormapEl.value = app.speakerHeatmapVolumeColormap;
    speakerHeatmapVolumeColormapEl.addEventListener('change', () => {
      app.speakerHeatmapVolumeColormap = speakerHeatmapVolumeColormapEl.value;
      app.lastSpeakerSoloVolumeAt = 0; // rebuild on the next tick
      updateGradientEditorVisibility();
      persistEffectiveRenderPrefs();
    });
  }

  const globalEnergyHeatmapToggleEl = document.getElementById('globalEnergyHeatmapToggle');
  // The engine owns the overlay display prefs and republishes them on every
  // change, including an mpv keybind Studio never sees. Re-render the switches
  // from that state so the UI cannot claim something the overlay isn't doing.
  onOverlayState(() => {
    const trailEl = document.getElementById('trailToggle');
    if (trailEl) trailEl.checked = app.trailsEnabled;
    const showObjectsEl = document.getElementById('showObjectsToggle');
    if (showObjectsEl) showObjectsEl.checked = app.objectsVisible !== false;
    const labelsEl = document.getElementById('objectLabelsToggle');
    if (labelsEl) labelsEl.checked = app.objectLabelsEnabled;
    const heatmapEl = document.getElementById('objectEnergyHeatmapToggle');
    if (heatmapEl) heatmapEl.checked = app.objectEnergyHeatmapEnabled;
    const trailModeEl = document.getElementById('trailModeSelect');
    if (trailModeEl) trailModeEl.value = app.trailRenderMode;
    applyObjectsVisibility();
    refreshObjectEnergyHeatmapNow();
  });

  if (globalEnergyHeatmapToggleEl) {
    globalEnergyHeatmapToggleEl.checked = app.globalEnergyHeatmapEnabled;
    // Honour a persisted-on state at startup, like the per-speaker heatmap.
    if (app.globalEnergyHeatmapEnabled) acquireGainTable(GAINTABLE_CONSUMER_GLOBAL);
    globalEnergyHeatmapToggleEl.addEventListener('change', () => {
      app.globalEnergyHeatmapEnabled = globalEnergyHeatmapToggleEl.checked;
      app.lastGlobalEnergyVolumeAt = 0; // rebuild on the next tick
      if (app.globalEnergyHeatmapEnabled) {
        acquireGainTable(GAINTABLE_CONSUMER_GLOBAL);
      } else {
        releaseGainTable(GAINTABLE_CONSUMER_GLOBAL);
      }
      // The subscription target changes with the toggle (one target per client),
      // so re-subscribe even when another consumer keeps the table alive.
      refreshGaintableSubscription();
      persistEffectiveRenderPrefs();
    });
  }

  const globalEnergyHeatmapScaleEl = document.getElementById('globalEnergyHeatmapScale');
  if (globalEnergyHeatmapScaleEl) {
    globalEnergyHeatmapScaleEl.value = String(app.globalEnergyHeatmapScaleDb);
    globalEnergyHeatmapScaleEl.addEventListener('change', () => {
      const raw = Number(globalEnergyHeatmapScaleEl.value);
      const next = Number.isFinite(raw) ? Math.max(1, Math.min(40, Math.round(raw))) : 6;
      app.globalEnergyHeatmapScaleDb = next;
      globalEnergyHeatmapScaleEl.value = String(next);
      app.lastGlobalEnergyVolumeAt = 0;
      persistEffectiveRenderPrefs();
    });
  }

  const globalEnergyHeatmapBandSelectEl = document.getElementById('globalEnergyHeatmapBandSelect');
  if (globalEnergyHeatmapBandSelectEl) {
    syncCrossoverBandSelects();
    globalEnergyHeatmapBandSelectEl.addEventListener('change', () => {
      const next = Number(globalEnergyHeatmapBandSelectEl.value);
      app.globalEnergyHeatmapBandIndex = Math.max(0, Math.round(Number.isFinite(next) ? next : 0));
      app.lastGlobalEnergyVolumeAt = 0;
      syncCrossoverBandSelects();
      persistEffectiveRenderPrefs();
    });
  }

  if (speakerHeatmapBandSelectEl) {
    syncCrossoverBandSelects();
    speakerHeatmapBandSelectEl.addEventListener('change', () => {
      const value = speakerHeatmapBandSelectEl.value;
      if (value === 'all') {
        app.speakerHeatmapAllBands = true; // heatmap composite; band index unchanged
      } else {
        app.speakerHeatmapAllBands = false;
        const nextBandIndex = Number(value);
        app.speakerHeatmapBandIndex = Math.max(0, Math.round(Number.isFinite(nextBandIndex) ? nextBandIndex : 0));
      }
      app.lastSpeakerSoloVolumeAt = 0; // rebuild the heatmap on the next tick
      syncCrossoverBandSelects();
      refreshOverlayLists();
      refreshEffectiveRenderVisibility();
      persistEffectiveRenderPrefs();
    });
  }

  // Force the next refresh tick to rebuild immediately rather than wait out the
  // throttle window, so UI changes feel instant.
  const refreshObjectEnergyHeatmapNow = () => {
    app.lastObjectEnergyHeatmapAt = 0;
    refreshObjectEnergyVolume(performance.now());
  };

  // The mix/γ/resolution/opacity params are shared by both volumes: rebuild the
  // object field now and let the per-speaker volume pick it up next frame.
  const refreshSharedVolumesNow = () => {
    app.lastSpeakerSoloVolumeAt = 0;
    refreshObjectEnergyHeatmapNow();
  };

  // Crisp cells vs trilinear gradient between each cell's 8 corner texels.
  if (volumeSmoothToggleEl) {
    volumeSmoothToggleEl.checked = app.volumeSmoothInterpolation;
    volumeSmoothToggleEl.addEventListener('change', () => {
      app.volumeSmoothInterpolation = volumeSmoothToggleEl.checked;
      refreshSharedVolumesNow();
      persistEffectiveRenderPrefs();
    });
  }

  if (objectEnergyHeatmapToggleEl) {
    objectEnergyHeatmapToggleEl.checked = app.objectEnergyHeatmapEnabled;
    objectEnergyHeatmapToggleEl.addEventListener('change', () => {
      app.objectEnergyHeatmapEnabled = objectEnergyHeatmapToggleEl.checked;
      refreshObjectEnergyHeatmapNow();
      setMpvOverlayHeatmapEnabled(app.objectEnergyHeatmapEnabled);
      persistEffectiveRenderPrefs();
    });
  }

  // Object-field colour gradient — applies to the Studio 3D volume and the mpv overlay.
  if (objectEnergyColormapEl) {
    objectEnergyColormapEl.value = app.objectEnergyColormap;
    objectEnergyColormapEl.addEventListener('change', () => {
      app.objectEnergyColormap = objectEnergyColormapEl.value;
      refreshObjectEnergyHeatmapNow();
      invoke('mpv_overlay_set_heatmap_colormap', { colormap: colormapIndex(app.objectEnergyColormap) }).catch(() => {});
      if (app.objectEnergyColormap === 'custom') {
        setMpvOverlayHeatmapCustomStops(app.objectCustomGradientStops);
      }
      updateGradientEditorVisibility();
      persistEffectiveRenderPrefs();
    });
  }

  // Mount the two independent custom-gradient editors. An edit rebuilds only the
  // matching volume; the object gradient also pushes to the mpv overlay (when the
  // object field uses the custom map). Both persist.
  setGradientEditorOnChange((target) => {
    if (target === 'speaker') {
      app.lastSpeakerSoloVolumeAt = 0; // rebuild on the next tick (sig already bumped)
    } else {
      refreshObjectEnergyHeatmapNow();
      if (app.objectEnergyColormap === 'custom') {
        setMpvOverlayHeatmapCustomStops(app.objectCustomGradientStops);
      }
    }
    persistEffectiveRenderPrefs();
  });
  const objectGradientEditorEl = document.getElementById('objectGradientEditor');
  const speakerGradientEditorEl = document.getElementById('speakerGradientEditor');
  if (objectGradientEditorEl) registerGradientEditor(objectGradientEditorEl, 'object');
  if (speakerGradientEditorEl) registerGradientEditor(speakerGradientEditorEl, 'speaker');
  updateGradientEditorVisibility();

  // Mix slider: 0 = pure accumulate … 1 = pure peak. Both components render at
  // once and are blended in the shader.
  if (objectEnergyVolumeMixSliderEl) {
    objectEnergyVolumeMixSliderEl.value = String(app.objectEnergyVolumeMix);
    if (objectEnergyVolumeMixValEl) {
      objectEnergyVolumeMixValEl.textContent = app.objectEnergyVolumeMix.toFixed(2);
    }
    objectEnergyVolumeMixSliderEl.addEventListener('input', () => {
      const next = Number(objectEnergyVolumeMixSliderEl.value);
      app.objectEnergyVolumeMix = Math.max(0, Math.min(1, Number.isFinite(next) ? next : 0));
      if (objectEnergyVolumeMixValEl) {
        objectEnergyVolumeMixValEl.textContent = app.objectEnergyVolumeMix.toFixed(2);
      }
      refreshSharedVolumesNow();
      persistEffectiveRenderPrefs();
    });
  }

  // Each component keeps its own γ (own range): peak weighs one sample, accumulate
  // integrates the whole ray, so a shared value isn't comparable.
  const bindVolumeGamma = (sliderEl, valEl, field, projection, digits) => {
    if (!sliderEl) return;
    sliderEl.value = String(app[field]);
    if (valEl) valEl.textContent = app[field].toFixed(digits);
    sliderEl.addEventListener('input', () => {
      app[field] = clampVolumeGamma(projection, sliderEl.value);
      if (valEl) valEl.textContent = app[field].toFixed(digits);
      refreshSharedVolumesNow();
      persistEffectiveRenderPrefs();
    });
  };
  bindVolumeGamma(objectEnergyVolumeGammaAccumulateSliderEl, objectEnergyVolumeGammaAccumulateValEl,
    'objectEnergyVolumeGammaAccumulate', 'accumulate', 1);
  bindVolumeGamma(objectEnergyVolumeGammaMipSliderEl, objectEnergyVolumeGammaMipValEl,
    'objectEnergyVolumeGammaMip', 'mip', 2);

  if (objectEnergyHeatmapResolutionSliderEl) {
    objectEnergyHeatmapResolutionSliderEl.value = String(app.objectEnergyHeatmapResolution);
    if (objectEnergyHeatmapResolutionValEl) {
      objectEnergyHeatmapResolutionValEl.textContent = String(app.objectEnergyHeatmapResolution);
    }
    objectEnergyHeatmapResolutionSliderEl.addEventListener('input', () => {
      const next = Number(objectEnergyHeatmapResolutionSliderEl.value);
      app.objectEnergyHeatmapResolution = Math.max(8, Math.min(64, Math.round(Number.isFinite(next) ? next : 24)));
      if (objectEnergyHeatmapResolutionValEl) {
        objectEnergyHeatmapResolutionValEl.textContent = String(app.objectEnergyHeatmapResolution);
      }
      refreshSharedVolumesNow();
      persistEffectiveRenderPrefs();
    });
  }

  // Energy-volume refresh interval (ms): how often the 3D texture is rebuilt and
  // re-uploaded. Lower = more fluid, more GPU upload churn; higher = lighter.
  if (volumeRefreshSliderEl) {
    volumeRefreshSliderEl.value = String(app.volumeRefreshMs);
    if (volumeRefreshValEl) {
      volumeRefreshValEl.textContent = `${app.volumeRefreshMs} ms`;
    }
    volumeRefreshSliderEl.addEventListener('input', () => {
      const next = Number(volumeRefreshSliderEl.value);
      app.volumeRefreshMs = Math.max(40, Math.min(500, Math.round(Number.isFinite(next) ? next : 160)));
      if (volumeRefreshValEl) {
        volumeRefreshValEl.textContent = `${app.volumeRefreshMs} ms`;
      }
      persistEffectiveRenderPrefs();
    });
  }

  if (objectEnergyHeatmapRadiusSliderEl) {
    objectEnergyHeatmapRadiusSliderEl.value = String(app.objectEnergyHeatmapFalloffRadius);
    if (objectEnergyHeatmapRadiusValEl) {
      objectEnergyHeatmapRadiusValEl.textContent = app.objectEnergyHeatmapFalloffRadius.toFixed(2);
    }
    objectEnergyHeatmapRadiusSliderEl.addEventListener('input', () => {
      const next = Number(objectEnergyHeatmapRadiusSliderEl.value);
      app.objectEnergyHeatmapFalloffRadius = Math.max(0.02, Math.min(0.5, Number.isFinite(next) ? next : 0.12));
      if (objectEnergyHeatmapRadiusValEl) {
        objectEnergyHeatmapRadiusValEl.textContent = app.objectEnergyHeatmapFalloffRadius.toFixed(2);
      }
      refreshObjectEnergyHeatmapNow();
      persistEffectiveRenderPrefs();
    });
  }

  if (objectEnergyHeatmapOpacitySliderEl) {
    objectEnergyHeatmapOpacitySliderEl.value = String(app.objectEnergyHeatmapOpacity);
    if (objectEnergyHeatmapOpacityValEl) {
      objectEnergyHeatmapOpacityValEl.textContent = app.objectEnergyHeatmapOpacity.toFixed(2);
    }
    objectEnergyHeatmapOpacitySliderEl.addEventListener('input', () => {
      const next = Number(objectEnergyHeatmapOpacitySliderEl.value);
      app.objectEnergyHeatmapOpacity = Math.max(0.05, Math.min(1.0, Number.isFinite(next) ? next : 0.55));
      if (objectEnergyHeatmapOpacityValEl) {
        objectEnergyHeatmapOpacityValEl.textContent = app.objectEnergyHeatmapOpacity.toFixed(2);
      }
      refreshSharedVolumesNow();
      persistEffectiveRenderPrefs();
    });
  }

  // No startup seed: the mpv overlay display prefs (enable / labels / trails)
  // are owned and persisted by orender now and loaded at its startup. Studio
  // only drives them live via the on-change handlers above; seeding here would
  // override orender's persisted state at connect.
}
