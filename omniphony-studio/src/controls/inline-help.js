/**
 * Inline parameter help: clicking a parameter's name shows its help text in a
 * small highlighted panel directly below its row (no "i" button). Only one panel
 * is open at a time across the whole UI, and it closes on any click outside it.
 *
 * Two entry points share the single open-panel state so the "one at a time" rule
 * holds across both the schema-generated plugin params (see `buildParamControl`
 * in `plugin-params.js`) and the hand-written renderer params (wired declaratively from
 * markup via `wireInlineHelpFromMarkup`).
 */

import { t } from '../i18n.js';

let openHelp = null;
let outsideCloseBound = false;

/** Hide the currently open help panel, if any. */
export function closeInlineHelp() {
  if (openHelp) {
    openHelp.style.display = 'none';
    openHelp = null;
  }
}

function ensureOutsideClose() {
  if (outsideCloseBound) return;
  outsideCloseBound = true;
  // The toggle stops propagation, so this only fires for clicks elsewhere.
  document.addEventListener('click', (event) => {
    if (!openHelp || openHelp.contains(event.target)) return;
    closeInlineHelp();
  });
}

/** Build a styled, initially-hidden help panel containing `text`. */
export function makeHelpPanel(text) {
  const help = document.createElement('div');
  help.className = 'generated-param-help';
  help.textContent = text;
  help.style.display = 'none';
  help.style.marginTop = '0.15rem';
  help.style.padding = '0.3rem 0.45rem';
  help.style.fontSize = '11px';
  help.style.lineHeight = '1.35';
  help.style.color = '#d9ecff';
  help.style.background = 'rgba(120,200,255,0.10)';
  help.style.border = '1px solid rgba(120,200,255,0.30)';
  help.style.borderRadius = '6px';
  return help;
}

/**
 * Make `triggerEl` (a parameter's name) toggle `panelEl`. A faint dotted
 * underline hints it is clickable; opening one panel closes any other and the
 * panel dismisses on an outside click.
 */
export function attachInlineHelp(triggerEl, panelEl) {
  triggerEl.setAttribute('role', 'button');
  triggerEl.style.cursor = 'pointer';
  triggerEl.style.textDecoration = 'underline dotted rgba(217,236,255,0.4)';
  triggerEl.style.textUnderlineOffset = '2px';
  triggerEl.addEventListener('click', (event) => {
    // Stay put: don't activate an associated control or trip the outside-close.
    event.preventDefault();
    event.stopPropagation();
    const wasOpen = panelEl.style.display !== 'none';
    closeInlineHelp();
    if (!wasOpen) {
      panelEl.style.display = '';
      openHelp = panelEl;
      ensureOutsideClose();
    }
  });
}

/**
 * Scan `root` for elements carrying a `data-help-i18n` key, inserting a help
 * panel just below each one's row and wiring its name as the toggle. The row is
 * the nearest `.control-row` / `.inline-toggle` by default; an element can
 * override it with `data-help-anchor="<css selector>"` (matched with `closest`)
 * so the panel lands below a whole group instead of inside a dense grid cell.
 * Idempotent — wired elements are marked with `data-help-wired`, so it is safe to
 * call again after re-rendering markup.
 */
export function wireInlineHelpFromMarkup(root = document) {
  root.querySelectorAll('[data-help-i18n]:not([data-help-wired])').forEach((el) => {
    const key = el.getAttribute('data-help-i18n');
    const text = t(key);
    // Skip until a translation exists, so a raw key never leaks into the UI.
    if (!text || text === key) return;
    const anchorSelector = el.getAttribute('data-help-anchor');
    const row =
      (anchorSelector && el.closest(anchorSelector))
      || el.closest('.control-row, .inline-toggle')
      || el.parentElement;
    if (!row || !row.parentElement) return;
    const panel = makeHelpPanel(text);
    row.after(panel);
    attachInlineHelp(el, panel);
    el.setAttribute('data-help-wired', '1');
  });
}
