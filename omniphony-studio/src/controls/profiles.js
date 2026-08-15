// Config-profile picker (top of the left overlay): switch between named
// renderer config profiles, create / rename / delete them.
//
// The renderer is the single source of truth: every mutation goes out over
// OSC and the UI is re-populated from the `/omniphony/state/profiles` echo,
// mirrored into the state snapshot as `activeProfile` / `profileNames`.
// Nothing is applied optimistically.

import { invoke } from '@tauri-apps/api/core';
import { tf } from '../i18n.js';

const el = (id) => document.getElementById(id);

// Guard against re-binding listeners if the panel is initialised twice.
let bound = false;
// While we are pushing renderer state into the select, ignore the change
// events that programmatic value updates would otherwise fire back as commands.
let applying = false;

// Mirror of the last renderer echo — refreshed on every state snapshot.
let activeProfile = null;
let profileNames = [];

// Pending inline name editor submit action (create or rename).
let nameSubmitAction = null;

function send(cmd, args) {
  invoke(cmd, args).catch((e) => console.error('[profiles]', cmd, e));
}

function closeNameEditor() {
  nameSubmitAction = null;
  const row = el('profileNameRow');
  if (row) row.style.display = 'none';
}

// Show the inline name editor (create / rename share it — no window.prompt:
// it is unavailable on WebView2 and blocking dialogs are best avoided).
function openNameEditor(initialValue, onSubmit) {
  const row = el('profileNameRow');
  const input = el('profileNameInput');
  if (!row || !input) return;
  nameSubmitAction = onSubmit;
  input.value = initialValue;
  row.style.display = 'flex';
  input.focus();
  input.select();
}

function submitNameEditor() {
  const input = el('profileNameInput');
  const action = nameSubmitAction;
  const name = input ? input.value.trim() : '';
  closeNameEditor();
  if (action && name) action(name);
}

function syncButtons() {
  const createBtn = el('profileCreateBtn');
  const renameBtn = el('profileRenameBtn');
  const deleteBtn = el('profileDeleteBtn');
  const hasActive = typeof activeProfile === 'string' && activeProfile !== '';
  if (renameBtn) renameBtn.disabled = !hasActive;
  // Deleting the last profile is refused renderer-side; grey the button.
  if (deleteBtn) deleteBtn.disabled = !hasActive || profileNames.length <= 1;
  if (createBtn) createBtn.disabled = false;
}

export function initProfilesPanel() {
  if (bound) return;
  bound = true;

  const select = el('profileSelect');
  if (select) {
    select.addEventListener('change', (e) => {
      if (applying) return;
      const name = String(e.target.value || '').trim();
      if (!name || name === activeProfile) return;
      // No optimistic apply: the renderer echoes the new profiles state (and
      // the full state bundle) after the switch; applyProfilesState re-aims
      // the select from that echo.
      send('control_profile_switch', { value: name });
    });
  }

  const createBtn = el('profileCreateBtn');
  if (createBtn) {
    createBtn.addEventListener('click', () => {
      openNameEditor('', (name) => {
        if (profileNames.includes(name)) {
          // Name already taken: just activate it instead of overwriting.
          send('control_profile_switch', { value: name });
          return;
        }
        // Create snapshots the current live state under the new name, then
        // switch makes it the active profile.
        send('control_profile_create', { value: name });
        send('control_profile_switch', { value: name });
      });
    });
  }

  const renameBtn = el('profileRenameBtn');
  if (renameBtn) {
    renameBtn.addEventListener('click', () => {
      if (typeof activeProfile !== 'string' || activeProfile === '') return;
      const oldName = activeProfile;
      openNameEditor(oldName, (name) => {
        if (name === oldName || profileNames.includes(name)) return;
        send('control_profile_rename', { old: oldName, new: name });
      });
    });
  }

  const deleteBtn = el('profileDeleteBtn');
  if (deleteBtn) {
    deleteBtn.addEventListener('click', () => {
      // The renderer refuses to delete the ACTIVE profile, so switch away to
      // the first other profile, then delete the previously active one. The
      // two messages travel in order on the same OSC socket.
      const name = activeProfile;
      const keep = profileNames.find((n) => n !== name);
      if (typeof name !== 'string' || name === '' || !keep) return;
      if (!window.confirm(tf('profiles.confirmDelete', { name, keep }))) return;
      send('control_profile_switch', { value: keep });
      send('control_profile_delete', { value: name });
    });
  }

  const okBtn = el('profileNameOkBtn');
  if (okBtn) okBtn.addEventListener('click', submitNameEditor);
  const cancelBtn = el('profileNameCancelBtn');
  if (cancelBtn) cancelBtn.addEventListener('click', closeNameEditor);
  const input = el('profileNameInput');
  if (input) {
    input.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') {
        e.preventDefault();
        submitNameEditor();
      } else if (e.key === 'Escape') {
        e.preventDefault();
        closeNameEditor();
      }
    });
  }

  syncButtons();
}

// Apply the profiles part of the renderer state snapshot to the picker.
export function applyProfilesState(payload) {
  if (!payload || typeof payload !== 'object') return;
  const names = Array.isArray(payload.profileNames)
    ? payload.profileNames.filter((n) => typeof n === 'string' && n !== '')
    : [];
  const active = typeof payload.activeProfile === 'string' ? payload.activeProfile : null;

  activeProfile = active;
  profileNames = names;

  const select = el('profileSelect');
  if (select) {
    applying = true;
    try {
      const wanted = names.length > 0 ? names : active ? [active] : [];
      // Rebuild the options only when the list actually changed (the state
      // echoes at ~10 Hz; a rebuild would close an open dropdown).
      const current = Array.from(select.options).map((o) => o.value);
      if (current.length !== wanted.length || current.some((v, i) => v !== wanted[i])) {
        select.textContent = '';
        for (const name of wanted) {
          const option = document.createElement('option');
          option.value = name;
          option.textContent = name;
          select.appendChild(option);
        }
      }
      if (active !== null && select.value !== active) select.value = active;
      select.disabled = wanted.length === 0;
    } finally {
      applying = false;
    }
  }

  syncButtons();
}
