// Binaural (headphone) output panel: output-mode toggle, HRIR source selection,
// and Sensors2OSC head-tracking controls (address, format, smoothing, invert,
// recenter) plus a live head-pose readout.
//
// Self-contained: caches its own DOM elements and applies incoming renderer
// state via `applyBinauralState(payload.binaural)`. All element lookups are
// guarded so a missing node never throws.

import { invoke } from '@tauri-apps/api/core';
import { initSofaBrowser, setActiveSofaPath } from './sofa-browser.js';
import { setSpeakersGhosted } from '../speakers.js';
import { initHeadphoneChannels } from './headphone-meter.js';

const el = (id) => document.getElementById(id);

// Guard against re-binding listeners if the panel is initialised twice.
let bound = false;
// While we are pushing renderer state into the controls, ignore the change
// events that programmatic value updates would otherwise fire back as commands.
let applying = false;

function send(cmd, args) {
  invoke(cmd, args).catch((e) => console.error('[binaural]', cmd, e));
}

// Last output mode seen from the renderer state, so the editing tab only
// auto-switches when the MODE actually changes (never on the ~10 Hz echo).
let lastSeenOutputMode = null;

// Select the Renderer or Binaural editing tab (pure UI — see the CSS block
// "Renderer / Binaural editing tabs" in app.css).
function setStudioTab(binauralTab) {
  document.body.classList.toggle('studio-tab-binaural', binauralTab);
  const tr = el('rendererTabRendererBtn');
  const tb = el('rendererTabBinauralBtn');
  if (tr) tr.classList.toggle('active', !binauralTab);
  if (tb) tb.classList.toggle('active', binauralTab);
}

// Show the reflections / reverb parameter blocks only when their master switch
// is on, so a dry (default) headphone setup isn't cluttered with inactive
// controls. Driven from the switch change handlers and applyBinauralState.
function syncBinauralParamVisibility() {
  const refl = el('binauralReflEnabled');
  const reflParams = el('binauralReflParams');
  if (reflParams) reflParams.style.display = refl && refl.checked ? 'grid' : 'none';
  const rev = el('binauralRevEnabled');
  const revParams = el('binauralRevParams');
  if (revParams) revParams.style.display = rev && rev.checked ? 'grid' : 'none';
}

export function initBinauralPanel() {
  if (bound) return;
  bound = true;

  initSofaBrowser();
  initHeadphoneChannels();

  // Output mode: one combo, three targets. The selection is NOT applied
  // optimistically — the renderer's state broadcast is the source of truth
  // (applyBinauralState sets the value back). "Headphones (virtual room)" is
  // the cascaded mode: the speaker pipeline rendered on a virtual layout,
  // then binauralised.
  const outputSel = el('outputModeSelect');
  if (outputSel) {
    outputSel.addEventListener('change', (e) => {
      if (applying) return;
      switch (e.target.value) {
        case 'speaker':
          send('control_output_mode', { value: 'speaker' });
          break;
        case 'binaural-direct':
          send('control_output_mode', { value: 'binaural' });
          send('control_binaural_mode', { value: 'direct' });
          break;
        case 'binaural-cascaded':
          send('control_output_mode', { value: 'binaural' });
          send('control_binaural_mode', { value: 'cascaded' });
          break;
        default:
          break;
      }
    });
  }

  // Renderer / Binaural editing tabs: pure UI, decoupled from the output
  // mode (the virtual-room cascade keeps the renderer options meaningful on
  // headphones). applyBinauralState re-aims the tab when the MODE changes;
  // clicking is always allowed.
  const tabRenderer = el('rendererTabRendererBtn');
  if (tabRenderer) {
    tabRenderer.addEventListener('click', () => setStudioTab(false));
  }
  const tabBinaural = el('rendererTabBinauralBtn');
  if (tabBinaural) {
    tabBinaural.addEventListener('click', () => setStudioTab(true));
  }
  // Reflect the initial tab (Renderer) on the buttons.
  setStudioTab(document.body.classList.contains('studio-tab-binaural'));

  const cascadeLayout = el('binauralCascadeLayout');
  if (cascadeLayout) {
    cascadeLayout.addEventListener('change', (e) => {
      if (applying) return;
      send('control_cascade_layout', { value: e.target.value });
    });
  }

  // The SOFA browser only makes sense for the 'sofa' source (KEMAR is
  // embedded, synthetic is analytic) — show the button accordingly.
  const syncSofaBrowseVisibility = (value) => {
    const btn = el('sofaBrowseBtn');
    if (btn) btn.style.display = value === 'sofa' ? '' : 'none';
  };

  // The parametric-pinna model exposes two tuning sliders; show them only for
  // the 'pinna' source. The renderer carries the size/depth in the source
  // string itself ("pinna:<size>:<depth>"), so both sliders just re-send it.
  const syncPinnaVisibility = (value) => {
    const box = el('binauralPinnaControls');
    if (box) box.style.display = value === 'pinna' ? 'grid' : 'none';
  };
  const sendPinna = () => {
    const preset = el('binauralPinnaPreset')?.value === 'rd' ? 'rd' : 'pbnh';
    const dScale = Math.round(Number(el('binauralPinnaDScale')?.value));
    const depth = Math.round(Number(el('binauralPinnaDepth')?.value));
    const ds = Number.isFinite(dScale) ? dScale : 100;
    const dp = Number.isFinite(depth) ? depth : 100;
    send('control_hrir_source', { value: `pinna:${preset}:${ds}:${dp}` });
  };

  // Spagnol PRTF model carries its two knobs in the source string too
  // ("prtf:<freqScale>:<depth>").
  const syncPrtfVisibility = (value) => {
    const box = el('binauralPrtfControls');
    if (box) box.style.display = value === 'prtf' ? 'grid' : 'none';
  };
  const sendPrtf = () => {
    const freq = Math.round(Number(el('binauralPrtfFreqScale')?.value));
    const depth = Math.round(Number(el('binauralPrtfDepth')?.value));
    const fs = Number.isFinite(freq) ? freq : 100;
    const dp = Number.isFinite(depth) ? depth : 100;
    send('control_hrir_source', { value: `prtf:${fs}:${dp}` });
  };

  const src = el('binauralHrirSource');
  if (src) {
    src.addEventListener('change', (e) => {
      syncSofaBrowseVisibility(e.target.value);
      syncPinnaVisibility(e.target.value);
      syncPrtfVisibility(e.target.value);
      if (applying) return;
      if (e.target.value === 'pinna') sendPinna();
      else if (e.target.value === 'prtf') sendPrtf();
      else send('control_hrir_source', { value: e.target.value });
    });
  }

  const unitScale = el('binauralUnitScale');
  if (unitScale) {
    unitScale.addEventListener('input', (e) => {
      if (applying) return;
      const v = Number(e.target.value);
      const out = el('binauralUnitScaleVal');
      if (out) out.textContent = v.toFixed(1);
      send('control_binaural_unit_scale', { value: v });
    });
  }

  const pinnaPreset = el('binauralPinnaPreset');
  if (pinnaPreset) {
    pinnaPreset.addEventListener('change', () => {
      if (applying) return;
      sendPinna();
    });
  }
  const pinnaDScale = el('binauralPinnaDScale');
  if (pinnaDScale) {
    pinnaDScale.addEventListener('input', (e) => {
      const out = el('binauralPinnaDScaleVal');
      if (out) out.textContent = String(Math.round(Number(e.target.value)));
      if (applying) return;
      sendPinna();
    });
  }
  const pinnaDepth = el('binauralPinnaDepth');
  if (pinnaDepth) {
    pinnaDepth.addEventListener('input', (e) => {
      const out = el('binauralPinnaDepthVal');
      if (out) out.textContent = String(Math.round(Number(e.target.value)));
      if (applying) return;
      sendPinna();
    });
  }

  const prtfDepth = el('binauralPrtfDepth');
  if (prtfDepth) {
    prtfDepth.addEventListener('input', (e) => {
      const out = el('binauralPrtfDepthVal');
      if (out) out.textContent = String(Math.round(Number(e.target.value)));
      if (applying) return;
      sendPrtf();
    });
  }
  const prtfFreqScale = el('binauralPrtfFreqScale');
  if (prtfFreqScale) {
    prtfFreqScale.addEventListener('input', (e) => {
      const out = el('binauralPrtfFreqScaleVal');
      if (out) out.textContent = String(Math.round(Number(e.target.value)));
      if (applying) return;
      sendPrtf();
    });
  }

  const headRadius = el('binauralHeadRadius');
  if (headRadius) {
    headRadius.addEventListener('input', (e) => {
      if (applying) return;
      const cm = Number(e.target.value);
      const out = el('binauralHeadRadiusVal');
      if (out) out.textContent = cm.toFixed(1);
      send('control_binaural_head_radius', { value: cm / 100 });
    });
  }

  const reflEnabled = el('binauralReflEnabled');
  if (reflEnabled) {
    reflEnabled.addEventListener('change', (e) => {
      syncBinauralParamVisibility();
      if (applying) return;
      send('control_binaural_reflections_enabled', { enable: e.target.checked ? 1 : 0 });
    });
  }

  const reflLevel = el('binauralReflLevel');
  if (reflLevel) {
    reflLevel.addEventListener('input', (e) => {
      if (applying) return;
      const v = Number(e.target.value);
      const out = el('binauralReflLevelVal');
      if (out) out.textContent = v.toFixed(2);
      send('control_binaural_reflections_level', { value: v });
    });
  }

  const updateRoomReadout = () => {
    const out = el('binauralReflRoomVal');
    if (!out) return;
    const w = Number(el('binauralReflRoomW')?.value ?? 0);
    const d = Number(el('binauralReflRoomD')?.value ?? 0);
    const h = Number(el('binauralReflRoomH')?.value ?? 0);
    out.textContent = `${w.toFixed(1)} × ${d.toFixed(1)} × ${h.toFixed(1)}`;
  };
  for (const [id, axis] of [
    ['binauralReflRoomW', 'width'],
    ['binauralReflRoomD', 'depth'],
    ['binauralReflRoomH', 'height'],
  ]) {
    const slider = el(id);
    if (!slider) continue;
    slider.addEventListener('input', (e) => {
      if (applying) return;
      updateRoomReadout();
      send('control_binaural_reflections_room', { axis, value: Number(e.target.value) });
    });
  }

  const revEnabled = el('binauralRevEnabled');
  if (revEnabled) {
    revEnabled.addEventListener('change', (e) => {
      syncBinauralParamVisibility();
      if (applying) return;
      send('control_binaural_reverb_enabled', { enable: e.target.checked ? 1 : 0 });
    });
  }

  const revLevel = el('binauralRevLevel');
  if (revLevel) {
    revLevel.addEventListener('input', (e) => {
      if (applying) return;
      const v = Number(e.target.value);
      const out = el('binauralRevLevelVal');
      if (out) out.textContent = v.toFixed(2);
      send('control_binaural_reverb_level', { value: v });
    });
  }

  const revRt60 = el('binauralRevRt60');
  if (revRt60) {
    revRt60.addEventListener('input', (e) => {
      if (applying) return;
      const v = Number(e.target.value);
      const out = el('binauralRevRt60Val');
      if (out) out.textContent = v.toFixed(2);
      send('control_binaural_reverb_rt60', { value: v });
    });
  }

  const airAbs = el('binauralAirAbsorption');
  if (airAbs) {
    airAbs.addEventListener('change', (e) => {
      if (applying) return;
      send('control_binaural_air_absorption', { enable: e.target.checked ? 1 : 0 });
    });
  }

  const recenter = el('binauralRecenter');
  if (recenter) {
    recenter.addEventListener('click', () => send('control_head_recenter', {}));
  }

  const addr = el('binauralTrackAddress');
  if (addr) {
    const commit = () => {
      if (applying) return;
      send('control_head_tracking_address', { value: addr.value });
    };
    addr.addEventListener('change', commit);
  }

  const fmt = el('binauralTrackFormat');
  if (fmt) {
    fmt.addEventListener('change', (e) => {
      if (applying) return;
      send('control_head_tracking_format', { value: e.target.value });
    });
  }

  const smooth = el('binauralTrackSmoothing');
  if (smooth) {
    smooth.addEventListener('input', (e) => {
      if (applying) return;
      const v = Number(e.target.value);
      const out = el('binauralTrackSmoothingVal');
      if (out) out.textContent = v.toFixed(2);
      send('control_head_tracking_smoothing', { value: v });
    });
  }

  const invert = el('binauralTrackInvert');
  if (invert) {
    invert.addEventListener('change', (e) => {
      if (applying) return;
      send('control_head_tracking_invert', { enable: e.target.checked ? 1 : 0 });
    });
  }

  // Reflect the initial (default-off) switch state.
  syncBinauralParamVisibility();
}

// Apply the `binaural` object from the renderer state bundle to the controls.
export function applyBinauralState(b) {
  if (!b || typeof b !== 'object') return;
  applying = true;
  try {
    // Never overwrite a control the user is interacting with: while a text
    // field is being typed into (or a slider dragged), the ~10 Hz state echo
    // would keep resetting it to the previous value, making edits impossible.
    const setVal = (id, v) => {
      const n = el(id);
      if (!n || v == null) return;
      if (document.activeElement === n) return;
      n.value = String(v);
    };

    if (typeof b.outputMode === 'string') {
      const binaural = b.outputMode === 'binaural';
      // `output-binaural` now gates only the reality-bound pieces (speaker
      // rows vs headphone meters, 3D ghosting); the editable parameter groups
      // are free tabs (`studio-tab-binaural`, below).
      document.body.classList.toggle('output-binaural', binaural);
      setSpeakersGhosted(binaural);
      const cascaded = b.mode === 'cascaded';
      setVal(
        'outputModeSelect',
        binaural ? (cascaded ? 'binaural-cascaded' : 'binaural-direct') : 'speaker',
      );
      // Auto-aim the editing tab when the MODE changes (state echoes at
      // ~10 Hz — only a real change may steal the user's tab choice).
      const modeKey = `${b.outputMode}/${b.mode}`;
      if (lastSeenOutputMode !== modeKey) {
        lastSeenOutputMode = modeKey;
        setStudioTab(binaural);
      }
    }
    if (typeof b.mode === 'string') {
      const cascaded = b.mode === 'cascaded';
      // The virtual-layout row only means something in cascaded mode.
      const layoutSection = el('binauralVirtualLayoutSection');
      if (layoutSection) layoutSection.style.display = cascaded ? '' : 'none';
      const layoutSel = el('binauralCascadeLayout');
      if (layoutSel && typeof b.cascadeLayout === 'string' && b.cascadeLayout) {
        // A custom YAML path from the config may not be one of the preset
        // options — surface it as an extra entry instead of mis-selecting.
        if (
          document.activeElement !== layoutSel &&
          ![...layoutSel.options].some((o) => o.value === b.cascadeLayout)
        ) {
          const opt = document.createElement('option');
          opt.value = b.cascadeLayout;
          opt.textContent = b.cascadeLayout.split('/').pop();
          layoutSel.appendChild(opt);
        }
        setVal('binauralCascadeLayout', b.cascadeLayout);
      }
    }
    if (typeof b.hrirSource === 'string') {
      setVal('binauralHrirSource', b.hrirSource);
      const btn = el('sofaBrowseBtn');
      if (btn) btn.style.display = b.hrirSource === 'sofa' ? '' : 'none';
      const pinnaBox = el('binauralPinnaControls');
      if (pinnaBox) pinnaBox.style.display = b.hrirSource === 'pinna' ? 'grid' : 'none';
      const prtfBox = el('binauralPrtfControls');
      if (prtfBox) prtfBox.style.display = b.hrirSource === 'prtf' ? 'grid' : 'none';
      // Info line under the source zone: chosen file name, or the
      // KEMAR-fallback notice while no file is selected yet.
      setActiveSofaPath(typeof b.hrtfSofaPath === 'string' ? b.hrtfSofaPath : '');
      const info = el('binauralSofaInfo');
      if (info) {
        if (b.hrirSource !== 'sofa') {
          info.style.display = 'none';
        } else {
          const path = typeof b.hrtfSofaPath === 'string' ? b.hrtfSofaPath : '';
          info.style.display = '';
          if (path) {
            const name = path.split('/').pop();
            info.textContent = `File: ${name}`;
            info.title = path;
            info.style.color = '#8fa6bd';
          } else {
            info.textContent =
              'No SOFA file selected — using the embedded KEMAR until you pick one (Browse…).';
            info.title = '';
            info.style.color = '#e8c46a';
          }
        }
      }
    }

    if (typeof b.unitScaleM === 'number') {
      setVal('binauralUnitScale', b.unitScaleM);
      const out = el('binauralUnitScaleVal');
      if (out) out.textContent = Number(b.unitScaleM).toFixed(1);
    }
    if (typeof b.headRadiusM === 'number') {
      setVal('binauralHeadRadius', b.headRadiusM * 100);
      const out = el('binauralHeadRadiusVal');
      if (out) out.textContent = (b.headRadiusM * 100).toFixed(1);
    }

    const refl = b.reflections;
    if (refl && typeof refl === 'object') {
      const en = el('binauralReflEnabled');
      if (en && typeof refl.enabled === 'boolean') en.checked = refl.enabled;
      if (typeof refl.level === 'number') {
        setVal('binauralReflLevel', refl.level);
        const out = el('binauralReflLevelVal');
        if (out) out.textContent = Number(refl.level).toFixed(2);
      }
      if (Array.isArray(refl.roomM) && refl.roomM.length === 3) {
        setVal('binauralReflRoomW', refl.roomM[0]);
        setVal('binauralReflRoomD', refl.roomM[1]);
        setVal('binauralReflRoomH', refl.roomM[2]);
        const out = el('binauralReflRoomVal');
        if (out) {
          out.textContent = refl.roomM.map((v) => Number(v).toFixed(1)).join(' × ');
        }
      }
    }

    const rev = b.reverb;
    if (rev && typeof rev === 'object') {
      const en = el('binauralRevEnabled');
      if (en && typeof rev.enabled === 'boolean') en.checked = rev.enabled;
      if (typeof rev.level === 'number') {
        setVal('binauralRevLevel', rev.level);
        const out = el('binauralRevLevelVal');
        if (out) out.textContent = Number(rev.level).toFixed(2);
      }
      if (typeof rev.rt60S === 'number') {
        setVal('binauralRevRt60', rev.rt60S);
        const out = el('binauralRevRt60Val');
        if (out) out.textContent = Number(rev.rt60S).toFixed(2);
      }
    }
    const air = el('binauralAirAbsorption');
    if (air && typeof b.airAbsorption === 'boolean') air.checked = b.airAbsorption;

    // The renderer's enabled state drives the param blocks' visibility.
    syncBinauralParamVisibility();

    const t = b.tracking || {};
    if (typeof t.address === 'string') setVal('binauralTrackAddress', t.address);
    if (typeof t.format === 'string') setVal('binauralTrackFormat', t.format);
    if (typeof t.smoothing === 'number') {
      setVal('binauralTrackSmoothing', t.smoothing);
      const out = el('binauralTrackSmoothingVal');
      if (out) out.textContent = Number(t.smoothing).toFixed(2);
    }
    const inv = el('binauralTrackInvert');
    if (inv && typeof t.invert === 'boolean') inv.checked = t.invert;

    // Head-pose readout as yaw/pitch/roll degrees, derived from the quaternion.
    const pose = b.headPose;
    const out = el('binauralPoseReadout');
    if (out && pose && typeof pose.w === 'number') {
      const { w, x, y, z } = pose;
      const yaw = Math.atan2(2 * (w * z + x * y), 1 - 2 * (y * y + z * z));
      const pitch = Math.asin(Math.max(-1, Math.min(1, 2 * (w * x - z * y))));
      const roll = Math.atan2(2 * (w * y + z * x), 1 - 2 * (x * x + y * y));
      const deg = (r) => (r * 180 / Math.PI).toFixed(0);
      out.textContent = `yaw ${deg(yaw)}°  pitch ${deg(pitch)}°  roll ${deg(roll)}°`;
    }
  } finally {
    applying = false;
  }
}
