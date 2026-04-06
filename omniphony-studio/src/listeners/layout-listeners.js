import { invoke } from '@tauri-apps/api/core';
import { app } from '../state.js';
import { tf } from '../i18n.js';
import { pushLog, normalizeLogError } from '../log.js';
import { updateConfigSavedUI } from '../controls/config.js';
import {
  renderSpeakerEditor,
  sanitizeLayoutExportName, defaultLayoutExportNameFromSpeakers, serializeCurrentLayoutForExport,
  refreshOverlayLists, hydrateLayoutSelect
} from '../speakers.js';

export function setupLayoutListeners() {
  const exportLayoutBtnEl = document.getElementById('exportLayoutBtn');
  const importLayoutBtnEl = document.getElementById('importLayoutBtn');
  const layoutSelectEl = document.getElementById('layoutSelect');

  if (exportLayoutBtnEl) {
    exportLayoutBtnEl.addEventListener('click', () => {
      const fallbackName = sanitizeLayoutExportName(defaultLayoutExportNameFromSpeakers(app.currentLayoutSpeakers));
      invoke('pick_export_layout_path', { suggestedName: fallbackName })
        .then((path) => {
          const trimmed = typeof path === 'string' ? path.trim() : '';
          if (!trimmed) return;
          const layout = serializeCurrentLayoutForExport();
          if (!layout) return;
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
    importLayoutBtnEl.addEventListener('click', () => {
      invoke('pick_import_layout_path')
        .then((path) => {
          const trimmed = typeof path === 'string' ? path.trim() : '';
          if (!trimmed) return;
          pushLog('info', tf('log.layoutImportRequested', { path: trimmed }));
          return invoke('import_layout_from_path', { path: trimmed })
            .then((payload) => {
              hydrateLayoutSelect(payload.layouts || [], payload.selectedLayoutKey);
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
    });
  }

  if (layoutSelectEl) {
    layoutSelectEl.addEventListener('change', () => {
      invoke('select_layout', { key: layoutSelectEl.value });
    });
  }
}
