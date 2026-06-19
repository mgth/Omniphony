// Floating quick-toggle bar pinned over the bottom of the 3D view. Each icon
// button mirrors one of the main display toggles in the renderer panel: it
// drives the source checkbox (dispatching `change`, so every existing handler
// and state update runs exactly as if the panel checkbox were clicked) and
// reflects that checkbox's state with an `.active` style. No effect logic lives
// here — this is purely a convenience surface over the existing controls.
//
// Two buttons (Objects, Trails) additionally carry a "nature" flyout: a small
// vertical menu, opened from a caret or a right-click, that drives the existing
// panel <select> (objectDisplayModeSelect / trailModeSelect) the same way — by
// setting its value and dispatching `change`. Still no rendering/persistence
// logic here; the panel handlers do all the work.

// Button id -> source checkbox id. Order defines left-to-right layout.
// `flyout` (optional) links a button to a nature menu defined in FX_FLYOUTS.
const SCENE_FX = [
  { btn: 'fxGridBtn', src: 'vbapCartesianGridToggleBtn' },
  { btn: 'fxObjectsBtn', src: 'showObjectsToggle', flyout: 'object' },
  { btn: 'fxLabelsBtn', src: 'objectLabelsToggle' },
  { btn: 'fxTrailsBtn', src: 'trailToggle', flyout: 'trail' },
  { btn: 'fxFieldBtn', src: 'objectEnergyHeatmapToggle' },
  { btn: 'fxHeatmapBtn', src: 'speakerHeatmapVolumeToggle' },
  { btn: 'fxMpvBtn', src: 'mpvOverlayToggle' },
];

// Nature flyouts. `menu` is the popup container, `select` the panel <select>
// the menu items map onto (item.dataset.value === one of its <option> values),
// and `visToggle` the visibility checkbox to enable when a nature is picked.
const FX_FLYOUTS = {
  object: { menu: 'fxObjectsMenu', select: 'objectDisplayModeSelect', visToggle: 'showObjectsToggle' },
  trail: { menu: 'fxTrailsMenu', select: 'trailModeSelect', visToggle: 'trailToggle' },
};

// The single currently-open flyout, or null. { menu, btn }.
let openFlyout = null;

function reflect(entry) {
  const btn = document.getElementById(entry.btn);
  const src = document.getElementById(entry.src);
  if (!btn || !src) return;
  const on = !!src.checked;
  btn.classList.toggle('active', on);
  btn.setAttribute('aria-pressed', on ? 'true' : 'false');
}

/** Mark the menu item matching the panel <select>'s current value. Read at open
 *  time, so the menu always shows the live mode regardless of where it changed. */
function reflectFlyoutSelection(cfg) {
  const menu = document.getElementById(cfg.menu);
  const select = document.getElementById(cfg.select);
  if (!menu || !select) return;
  const current = select.value;
  menu.querySelectorAll('.fx-flyout-item').forEach((item) => {
    const on = item.dataset.value === current;
    item.classList.toggle('selected', on);
    item.setAttribute('aria-checked', on ? 'true' : 'false');
  });
}

function closeFlyout() {
  if (!openFlyout) return;
  openFlyout.menu.hidden = true;
  openFlyout.btn.setAttribute('aria-expanded', 'false');
  openFlyout = null;
}

function showFlyout(cfg, btn) {
  const menu = document.getElementById(cfg.menu);
  if (!menu) return;
  closeFlyout();
  reflectFlyoutSelection(cfg);
  menu.hidden = false;
  btn.setAttribute('aria-expanded', 'true');
  openFlyout = { menu, btn };
}

function toggleFlyout(cfg, btn) {
  if (openFlyout && openFlyout.btn === btn) closeFlyout();
  else showFlyout(cfg, btn);
}

/** Apply a chosen nature by driving the panel <select> (and enabling the layer
 *  if it is currently hidden) — reusing every existing change handler. */
function chooseFlyoutValue(cfg, value) {
  const select = document.getElementById(cfg.select);
  if (!select) return;
  // Picking a nature implies you want to see the layer: enable it if hidden.
  const vis = document.getElementById(cfg.visToggle);
  if (vis && !vis.checked) {
    vis.checked = true;
    vis.dispatchEvent(new Event('change', { bubbles: true }));
  }
  if (select.value !== value) {
    select.value = value;
    select.dispatchEvent(new Event('change', { bubbles: true }));
  }
}

/** Re-read every source checkbox and update its button (and any flyout's checked
 *  item). Safe to call any time (e.g. after the display panel is re-rendered
 *  from persisted state). */
export function syncSceneEffectsBar() {
  for (const e of SCENE_FX) {
    reflect(e);
    if (e.flyout) reflectFlyoutSelection(FX_FLYOUTS[e.flyout]);
  }
}

/** Wire the bar's buttons to their source checkboxes. Call after the renderer/
 *  display panel listeners are installed so the dispatched `change` reaches the
 *  existing handlers. */
export function initSceneEffectsBar() {
  for (const entry of SCENE_FX) {
    const btn = document.getElementById(entry.btn);
    const src = document.getElementById(entry.src);
    if (!btn || !src) continue;
    const cfg = entry.flyout ? FX_FLYOUTS[entry.flyout] : null;
    btn.addEventListener('click', (e) => {
      // A click on the caret opens the nature menu instead of toggling.
      if (cfg && e.target.closest('.fx-caret')) {
        toggleFlyout(cfg, btn);
        return;
      }
      src.checked = !src.checked;
      src.dispatchEvent(new Event('change', { bubbles: true }));
      reflect(entry);
    });
    // Keep the button in sync when the panel checkbox is toggled directly (the
    // dispatched event above also lands here, which is harmless/idempotent).
    src.addEventListener('change', () => reflect(entry));
    reflect(entry);

    if (cfg) {
      // Right-click anywhere on the button is an alias for opening the menu.
      btn.addEventListener('contextmenu', (e) => {
        e.preventDefault();
        toggleFlyout(cfg, btn);
      });
      const menu = document.getElementById(cfg.menu);
      if (menu) {
        menu.addEventListener('click', (e) => {
          const item = e.target.closest('.fx-flyout-item');
          if (!item) return;
          chooseFlyoutValue(cfg, item.dataset.value);
          closeFlyout();
        });
      }
    }
  }

  // Dismiss an open flyout on outside click or Escape. A click on the owning
  // button (caret/toggle) is handled there, so ignore it here.
  document.addEventListener('click', (e) => {
    if (!openFlyout) return;
    if (openFlyout.menu.contains(e.target) || openFlyout.btn.contains(e.target)) return;
    closeFlyout();
  });
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') closeFlyout();
  });
}
