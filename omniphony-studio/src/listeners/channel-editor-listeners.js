/**
 * Listeners for the channel editor panel (virtual-bed channels edited from the
 * Objects list). Mirrors the speaker editor: numeric cartesian/polar (normalized
 * + real metres), a Direct/Virtual switch, and a per-channel gain. Commits go
 * through the virtual-bed `applyChannel*` helpers, which push the whole bed via
 * `control_virtual_bed`.
 */

import { app, sourceNames } from '../state.js';
import { metersPerUnit, metersToSceneUnits } from '../coordinates.js';
import { updateSpeakerGizmo } from '../speakers.js';
import {
  canonicalChannelName,
  getChannelPosition,
  applyChannelCartesian,
  applyChannelSceneCartesian,
  applyChannelPolar,
  applyChannelGain,
  applyChannelPlacement,
  renderChannelEditor
} from '../controls/virtual-bed.js';

function el(id) { return document.getElementById(id); }
function num(id) { return Number(el(id)?.value); }
function bind(id, event, handler) {
  const node = el(id);
  if (node) node.addEventListener(event, handler);
}

function selectedChannel() {
  if (app.selectedSourceId === null || app.selectedSourceId === undefined) return null;
  return canonicalChannelName(sourceNames.get(String(app.selectedSourceId)));
}

export function setupChannelEditorListeners() {
  const spatializeToggle = el('channelEditSpatializeToggle');
  if (spatializeToggle) {
    spatializeToggle.addEventListener('change', () => {
      const name = selectedChannel();
      if (name) applyChannelPlacement(name, spatializeToggle.checked);
    });
  }

  // Read only the field that fired; pull the other axes from the canonical
  // channel state so the rounded display values can't drift the untouched axes
  // (same rule as the speaker editor).
  function onCoordChange(handler) {
    return () => {
      const name = selectedChannel();
      if (!name) return;
      const pos = getChannelPosition(name);
      if (!pos) return;
      handler(name, pos);
    };
  }

  bind('channelEditXInput', 'change', onCoordChange((name, pos) => {
    const x = num('channelEditXInput');
    if (Number.isFinite(x)) applyChannelCartesian(name, x, pos.y, pos.z);
  }));
  bind('channelEditYInput', 'change', onCoordChange((name, pos) => {
    const y = num('channelEditYInput');
    if (Number.isFinite(y)) applyChannelCartesian(name, pos.x, y, pos.z);
  }));
  bind('channelEditZInput', 'change', onCoordChange((name, pos) => {
    const z = num('channelEditZInput');
    if (Number.isFinite(z)) applyChannelCartesian(name, pos.x, pos.y, z);
  }));

  // Real metres mirror the speaker editor: take the edited axis from the field,
  // the other two from canonical state (in metres), convert metres → scene units,
  // then commit. Never re-read the rounded sibling metre fields.
  function commitMeters(axis, inputId) {
    return onCoordChange((name, pos) => {
      const value = num(inputId);
      if (!Number.isFinite(value)) return;
      const meters = { x: pos.mx, y: pos.my, z: pos.mz };
      meters[axis] = value;
      const scn = metersToSceneUnits(meters);
      applyChannelSceneCartesian(name, scn.x, scn.y, scn.z);
    });
  }
  bind('channelEditXMetersInput', 'change', commitMeters('x', 'channelEditXMetersInput'));
  bind('channelEditYMetersInput', 'change', commitMeters('y', 'channelEditYMetersInput'));
  bind('channelEditZMetersInput', 'change', commitMeters('z', 'channelEditZMetersInput'));

  bind('channelEditAzInput', 'change', onCoordChange((name, pos) => {
    const az = num('channelEditAzInput');
    if (Number.isFinite(az)) applyChannelPolar(name, az, pos.elevation, pos.distance);
  }));
  bind('channelEditElInput', 'change', onCoordChange((name, pos) => {
    const elv = num('channelEditElInput');
    if (Number.isFinite(elv)) applyChannelPolar(name, pos.azimuth, elv, pos.distance);
  }));
  bind('channelEditRInput', 'change', onCoordChange((name, pos) => {
    const r = num('channelEditRInput');
    if (Number.isFinite(r)) applyChannelPolar(name, pos.azimuth, pos.elevation, r);
  }));
  bind('channelEditRMetersInput', 'change', onCoordChange((name, pos) => {
    const rM = num('channelEditRMetersInput');
    if (Number.isFinite(rM)) applyChannelPolar(name, pos.azimuth, pos.elevation, rM / metersPerUnit());
  }));

  // The coord-mode radios only choose which table the user edits; the bed is
  // always stored as polar, so switching mode never moves the channel.
  bind('channelEditCartesianMode', 'change', () => {
    if (el('channelEditCartesianMode')?.checked) {
      app.channelEditCoordMode = 'cartesian';
      renderChannelEditor(true);
    }
  });
  bind('channelEditPolarMode', 'change', () => {
    if (el('channelEditPolarMode')?.checked) {
      app.channelEditCoordMode = 'polar';
      renderChannelEditor(true);
    }
  });

  const gainSlider = el('channelEditGainSlider');
  if (gainSlider) {
    const gainBox = el('channelEditGainBox');
    gainSlider.addEventListener('input', () => {
      const v = Math.round((Number(gainSlider.value) || 0) * 10) / 10;
      if (gainBox) gainBox.textContent = `${v > 0 ? '+' : ''}${v.toFixed(1)} dB`;
    });
    gainSlider.addEventListener('change', () => {
      const name = selectedChannel();
      if (name) applyChannelGain(name, Number(gainSlider.value) || 0);
    });
    // Double-click resets to unity (0 dB), mirroring the speaker gain slider.
    gainSlider.addEventListener('dblclick', () => {
      const name = selectedChannel();
      if (!name) return;
      gainSlider.value = '0';
      if (gainBox) gainBox.textContent = '0 dB';
      applyChannelGain(name, 0);
    });
  }

  // 3D Edit buttons arm the shared gizmo (same one the speakers use) on the
  // selected channel object: cartesian handles or the polar ring/arc.
  const cartGizmoBtn = el('channelEditCartesianGizmoBtn');
  if (cartGizmoBtn) {
    cartGizmoBtn.addEventListener('click', () => {
      if (!selectedChannel()) return;
      app.activeEditMode = 'cartesian';
      app.cartesianEditArmed = !app.cartesianEditArmed;
      if (app.cartesianEditArmed) app.polarEditArmed = false;
      renderChannelEditor(true);
      updateSpeakerGizmo();
    });
  }
  const polarGizmoBtn = el('channelEditPolarGizmoBtn');
  if (polarGizmoBtn) {
    polarGizmoBtn.addEventListener('click', () => {
      if (!selectedChannel()) return;
      app.activeEditMode = 'polar';
      app.polarEditArmed = !app.polarEditArmed;
      if (app.polarEditArmed) app.cartesianEditArmed = false;
      renderChannelEditor(true);
      updateSpeakerGizmo();
    });
  }
}
