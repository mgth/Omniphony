import { mountAudioPanel } from './ui/audio-panel.js';
import { mountInputPanel } from './ui/input-panel.js';
import { mountRendererPanel } from './ui/renderer-panel.js';
import { initSidePanels } from './ui/side-panels.js';
import { wireInlineHelpFromMarkup } from './controls/inline-help.js';

mountAudioPanel();
mountInputPanel();
mountRendererPanel();
initSidePanels();
// Wire the inline parameter help across every panel (static index.html markup
// plus the just-mounted panels). Idempotent, so the renderer panel's own
// mount-time pass is harmless to repeat here.
wireInlineHelpFromMarkup(document);

// These fixed-position roots must be direct children of <body>: an ancestor
// with a backdrop-filter (like #speakersOverlay) becomes their containing
// block, so `left: 50%` centers them inside the panel instead of the window.
// An unbalanced </div> in the overlay markup has swallowed them twice already —
// self-heal and complain loudly rather than let the Save/Reload cluster render
// on top of the right panel again.
for (const id of ['bandCursor', 'saveFooterRoot', 'rendererInfoModalsRoot']) {
  const el = document.getElementById(id);
  if (el && el.parentElement !== document.body) {
    console.error(
      `#${id} is nested under #${el.parentElement.id || el.parentElement.tagName} — `
      + 'unbalanced overlay markup in index.html; reparenting to <body>'
    );
    document.body.appendChild(el);
  }
}
