// Guards the window close button while a tuning run is in progress.
//
// A run patches kp / ki / max_adjust / update_interval on the live renderer
// and only puts them back when it is stopped properly. Closing Studio in the
// middle used to just take the run with it — so this asks first, and says what
// leaving actually costs.
//
// Both buttons name what they do. Neither is "Cancel": in a dialog about
// stopping something, "Cancel" can mean either side and the user has to guess.

import { getCurrentWindow } from '@tauri-apps/api/window';

import { t } from '../i18n.js';

let unlisten = null;
let asking = false;

/**
 * Ask what to do about the run. Resolves true when the user chose to leave.
 *
 * Exported so it can be shown — and looked at — without a live tuning run.
 *
 * Its own modal rather than the wizard's: the wizard is mid-render on a live
 * run, and borrowing its body would mean rebuilding whatever screen it was
 * showing if the user decides to stay.
 */
export function askAboutTheRun() {
  return new Promise((resolve) => {
    const modal = document.createElement('div');
    modal.className = 'info-modal open';
    modal.id = 'autoTuneQuitModal';
    modal.setAttribute('aria-hidden', 'false');
    modal.setAttribute('role', 'dialog');
    modal.setAttribute('aria-modal', 'true');
    modal.innerHTML = `
      <div class="info-modal-card" style="max-width:460px;width:min(96vw,460px);display:flex;flex-direction:column;gap:0.6rem">
        <div class="info-modal-title"></div>
        <div class="info-modal-text"></div>
        <div class="info-modal-actions" style="display:flex;justify-content:flex-end;gap:0.4rem"></div>
      </div>
    `;
    modal.querySelector('.info-modal-title').textContent = t('autoTune.quitTitle')
      || 'A tuning run is in progress';
    modal.querySelector('.info-modal-text').textContent = t('autoTune.quitBody')
      || 'Closing Studio ends the run. The resampler goes back to the values it had before it started.';

    const actions = modal.querySelector('.info-modal-actions');
    const button = (labelKey, fallback, kind, choice) => {
      const el = document.createElement('button');
      el.type = 'button';
      el.className = kind === 'primary' ? 'ui-btn ui-btn-primary' : 'toggle-btn';
      el.setAttribute('data-i18n', labelKey);
      el.textContent = t(labelKey) || fallback;
      el.addEventListener('click', () => {
        close();
        resolve(choice);
      });
      return el;
    };

    function close() {
      document.removeEventListener('keydown', onKey);
      modal.remove();
    }

    // Escape means "I did not mean to close the window", which is the
    // conservative side: keep tuning.
    function onKey(event) {
      if (event.key === 'Escape') {
        close();
        resolve(false);
      }
    }

    actions.appendChild(button('autoTune.quitLeave', 'Quit and stop', 'secondary', true));
    const stay = button('autoTune.quitStay', 'Keep tuning', 'primary', false);
    actions.appendChild(stay);

    document.addEventListener('keydown', onKey);
    document.body.appendChild(modal);
    stay.focus();
  });
}

/**
 * Intercept the close button until the run ends.
 *
 * `stop` must stop the run and restore the parameters — it is the wizard's own
 * cancel path, which also detaches this guard, so the `close()` that follows
 * goes through the normal shutdown (renderer handoff included) rather than
 * being intercepted a second time.
 */
export async function attachQuitGuard(stop) {
  if (unlisten) return;
  const win = getCurrentWindow();
  unlisten = await win.onCloseRequested(async (event) => {
    event.preventDefault();
    // A second click on the X while the question is up must not stack a
    // second copy of it.
    if (asking) return;
    asking = true;
    let leaving = false;
    try {
      leaving = await askAboutTheRun();
    } finally {
      asking = false;
    }
    if (!leaving) return;
    await stop();
    await getCurrentWindow().close();
  });
}

export function detachQuitGuard() {
  if (unlisten) {
    unlisten();
    unlisten = null;
  }
}
