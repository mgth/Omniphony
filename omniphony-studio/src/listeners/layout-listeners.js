import { invoke } from '@tauri-apps/api/core';
import { app, isSpeakerLayoutFrozen } from '../state.js';
import { tf } from '../i18n.js';
import { pushLog, normalizeLogError } from '../log.js';
import { updateConfigSavedUI } from '../controls/config.js';
import {
  renderSpeakerEditor,
  serializeCurrentLayoutForExport,
  refreshOverlayLists, hydrateLayoutSelect, applyLayoutToRenderer
} from '../speakers.js';

// Shared import flow for the "Import layout" and "Presets" buttons. They differ
// only in which picker command they call: the generic one (remembers the last
// dir) vs. the presets one (always opens the bundled presets folder).
function runLayoutImport(pickCommand) {
  if (isSpeakerLayoutFrozen()) return;
  invoke(pickCommand)
    .then((path) => {
      const trimmed = typeof path === 'string' ? path.trim() : '';
      if (!trimmed) return;
      pushLog('info', tf('log.layoutImportRequested', { path: trimmed }));
      return invoke('import_layout_from_path', { path: trimmed })
        .then((payload) => {
          hydrateLayoutSelect(payload.layouts || [], payload.selectedLayoutKey);
          // Push the imported layout to the renderer so it actually takes
          // effect (and gets persisted on the next save). hydrateLayoutSelect
          // has already made it the current layout.
          applyLayoutToRenderer(payload.selectedLayoutKey);
          app.configSaved = false;
          updateConfigSavedUI();
          refreshOverlayLists();
          renderSpeakerEditor();
          pushLog('info', tf('log.layoutImported', { path: trimmed }));
        });
    })
    .catch((e) => {
      console.error('[layout import]', e);
      pushLog('error', tf('log.layoutImportFailed', { error: normalizeLogError(e) }));
    });
}

export function setupLayoutListeners() {
  const exportLayoutBtnEl = document.getElementById('exportLayoutBtn');
  const importLayoutBtnEl = document.getElementById('importLayoutBtn');
  const presetsBtnEl = document.getElementById('presetsBtn');

  if (exportLayoutBtnEl) {
    exportLayoutBtnEl.addEventListener('click', () => {
      if (isSpeakerLayoutFrozen()) return;
      const layout = serializeCurrentLayoutForExport();
      if (!layout) return;
      // The backend names the file from the speaker set ("7.1.4") and
      // sanitizes it; it also normalizes the speakers on the way out.
      invoke('default_layout_export_name', { layout })
        .then((fallbackName) => invoke('pick_export_layout_path', { suggestedName: fallbackName }))
        .then((path) => {
          const trimmed = typeof path === 'string' ? path.trim() : '';
          if (!trimmed) return;
          return invoke('export_layout_to_path', { path: trimmed, layout })
            .then(() => {
              pushLog('info', tf('log.layoutExported', { path: trimmed }));
            });
        })
        .catch((e) => {
          console.error('[layout export]', e);
          pushLog('error', tf('log.layoutExportFailed', { error: normalizeLogError(e) }));
        });
    });
  }

  if (importLayoutBtnEl) {
    importLayoutBtnEl.addEventListener('click', () => runLayoutImport('pick_import_layout_path'));
  }

  if (presetsBtnEl) {
    presetsBtnEl.addEventListener('click', () => runLayoutImport('pick_preset_layout_path'));
  }
}
