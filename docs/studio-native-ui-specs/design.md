# Omniphony Studio — Visual Design System Specification (for the egui port)

Scope: the *look* of the web Studio — theme tokens, layout geometry, generic widget
appearance and states, animation, responsiveness, and the i18n application rules.
Panel-by-panel control inventories are covered by other Phase-2 specs; this document
is what an engineer needs to build the egui `Visuals` / `Style` / reusable widget set
that those panels then compose.

All sources are under
`omniphony-studio/`.
Cited paths are relative to that directory. Line numbers refer to the state of the
tree at the time of writing.

**Root font size is 16 px.** There is no `html { font-size: … }` rule anywhere in
`src/styles/app.css` (verified by grep), so every `rem` in the sheet converts at
`1rem = 16px`. All px values below are already converted.

**Font families are declared but not bundled.** `body` asks for
`Inter, system-ui, sans-serif` (app.css:16) and `.info-list` asks for
`"JetBrains Mono", "SFMono-Regular", Menlo, monospace` (app.css:1475). There is no
`@font-face` and no font asset in the repo, so on a stock Linux box the UI actually
renders in the system UI font and the system monospace. For egui: ship/register one
sans (Inter if you want to match the designer's intent) and one monospace with
tabular figures; several readouts rely on `font-variant-numeric: tabular-nums`.

---

## 0. Token table (compact)

### 0.1 Surfaces

| Token | Value | Where |
|---|---|---|
| `page.bg` | `#0a0b10` | `body` (app.css:17). Only visible where the 3D canvas isn't. |
| `overlay.bg` | `rgba(0,0,0,0.65)` + backdrop blur 8 px | `#overlay`, `#speakersOverlay` (app.css:42, 570) |
| `overlay.border` | `1px solid rgba(255,255,255,0.20)` | app.css:45, 573 |
| `overlay.radius` | `12px` | app.css:46 |
| `log.bg.expanded` | `rgba(8,11,18,0.58)` + blur 10 px | app.css:614 |
| `log.border.expanded` | `1px solid rgba(255,255,255,0.14)`, radius `14px` | app.css:617-618 |
| `modal.backdrop` | `rgba(0,0,0,0.45)` | `.info-modal` (app.css:1347) |
| `modal.card.bg` | `rgba(14,16,22,0.96)` | `.info-modal-card` (app.css:1355) |
| `floatbar.bg` | `rgba(18,22,28,0.72)` + blur 10 px | `#bandCursor`, `#sceneEffectBar` (app.css:2752, 2801) |
| `flyout.bg` | `rgba(18,22,28,0.92)` + blur 12 px | `.scene-fx-flyout` (app.css:3040) |
| `section.card.bg` | `rgba(255,255,255,0.03)` | subpanels, `.input-panel-shell` (app.css:1846) |
| `section.card.bg2` | `rgba(255,255,255,0.04)` | `.adaptive-subpanel`, `.info-item`, `.log-entry` (app.css:1148, 1506, 1005) |
| `section.card.bg3` | `rgba(255,255,255,0.05)` | `.telemetry-gauges-form` (app.css:1166) |
| `section.card.bg4` | `rgba(255,255,255,0.06)` | `.room-geometry-summary` (app.css:876) |
| `control.bg` | `rgba(255,255,255,0.08)` | inputs, selects, `.ui-btn`, `.toggle-btn`, `.gain-box`, `.delay-input` |
| `control.bg.hover` | `rgba(255,255,255,0.12)` … `0.16` | select hover `.12`; `.info-icon-btn`/`.panel-toggle-btn` hover `.16` |
| `control.bg.active` | `rgba(255,255,255,0.20)` | `.toggle-btn.active` (app.css:2058) |
| `idstrip.bg` | `rgba(0,0,0,0.55)` | `.id-strip` (app.css:2196) |

### 0.2 Borders / strokes

| Token | Value | Where |
|---|---|---|
| `border.hairline` | `1px rgba(255,255,255,0.08)` | subpanel outlines, `.log-entry` |
| `border.divider` | `1px rgba(255,255,255,0.12)` | `.info-section` top rule (app.css:1088), `.channel-direct-target` |
| `border.subtle` | `1px rgba(255,255,255,0.14)` | `.room-geometry-summary`, expanded log |
| `border.control` | `1px rgba(255,255,255,0.20)` | every text/number input, select, `.ui-btn`, `.toggle-btn`, switch track |
| `border.control.hover` | `1px rgba(255,255,255,0.28)` … `0.32` | `.panel-toggle-btn` hover `.28`; select hover `.32` |
| `border.icon` | `1px rgba(255,255,255,0.30)` | `.info-icon-btn`, `.axis-driver` switch |
| `border.focus` | `1px rgba(255,255,255,0.45)` | `:focus` on `.delay-input`, `.coord-grid input`, `select:focus-visible` |
| `border.focus.blue` | `rgba(100,180,255,0.5)` | `.osc-config-row input:focus` (app.css:1247) |

Widths are **always 1 px**, except `.dual-range` track (4 px tall) and painted markers.

### 0.3 Text colours

| Token | Hex / rgba | Use |
|---|---|---|
| `text.primary` | `#d9ecff` | default panel text, input text, labels, values |
| `text.bright` | `#edf5ff` | log messages, About `<dd>`, colorised id-strip text |
| `text.white` | `#ffffff` | hover text on `.panel-toggle-btn` / `.toggle-btn.active`, subpanel titles |
| `text.field` | `#dfe8f3` | `.delay-input`, `.coord-grid input`, `.name-input`, `.toggle-btn` |
| `text.button` | `#cfd9e8` | `.ui-btn` base |
| `text.secondary` | `#b9c7d8` | `.input-panel-status`, `.object-topright`, `.meter-subvalues` |
| `text.muted` | `#9eb4c8` | `.panel-summary`, `.vbap-polar-meta` |
| `text.muted2` | `#9fb6cf` | `.cart-coord-head`, `.filter-freq` |
| `text.muted3` | `#8fa6bd` | `.input-panel-subtitle`, `.output-mode-mpv-note` |
| `text.muted4` | `#8fa3b7` | `.channel-direct-target small` |
| `text.dim` | `#8a9aac` | `.object-coords`, `.object-size-label` |
| `text.dim2` | `#8a9ab0` | `.band-label`, `.band-db` |
| `text.dim3` | `#86a7c3` | `.vbap-step` |
| `text.icon.dim` | `#5d6b7d` | position icon, full-band filter glyph |
| `text.icon` | `#8fb0d0` | active filter glyph |
| alpha ramp on `#d9ecff` | `.92 / .82 / .75 / .70 / .62 / .56 / .45` | headers, labels, log meta, placeholders |
| alpha ramp on `#dfe8f3` | `.95 / .92 / .72 / .60 / .55 / .38` | scene-fx and transport buttons |
| `text.unknown` | `#7f8a99` | OSC status dot fallback |

### 0.4 Semantic / accent colours

| Token | Value | Where |
|---|---|---|
| `accent.green` | `#52e2a2` | OSC connected dot, `.info-icon-btn.is-active`, scene-fx active, focus ring |
| `accent.green.bright` | `#5cff9a` | `.toggle-btn.effective` dot, object-test marker/orbit |
| `accent.green.text` | `#7af0c0` (idle) / `#aef5d8` (hover) / `#eaffe4` | scene-fx / transport / info-icon active text |
| `accent.green.ready` | `#78e08f` | `.vbap-status.ready` |
| `switch.on` | `rgba(80,200,120,0.45)` track | checked switch (app.css:1073) |
| `selected.row` | bg `rgba(46,110,64,0.45)`, border `rgba(90,200,120,0.35)` | `.info-item.is-selected` (app.css:2085) |
| `dragging.row` | bg `rgba(72,140,92,0.55)`, border `rgba(120,225,150,0.65)` | `.speaker-item.is-dragging` |
| `accent.blue` | `rgba(88,160,255,0.18)` bg / `rgba(88,160,255,0.38)` border | `.ui-btn-accent`, update banner |
| `accent.blue.tab` | bg `rgba(86,156,255,0.18)`, border `rgba(86,156,255,0.80)`, text `#cfe4ff` | `.renderer-tab-btn.active` (app.css:2717) |
| `accent.blue.link` | `#9cc6ff` | update-available link |
| `accent.cyan.glow` | `rgba(100,210,255,0.20 / 0.25)` | logo glows |
| `accent.help` | bg `rgba(120,200,255,0.10)`, border `rgba(120,200,255,0.30)` | inline help panel (`inline-help.js:39-43`) |
| `warn` | `#ffbf66` | `.vbap-status.computing`, `.mpv-orender-note.is-conflict` |
| `warn.dot` | `#ffb347` | OSC "reconnecting" |
| `warn.log` | text `#ffd891`, bg `rgba(255,177,71,0.16)` | log `warn` chip |
| `error` | `#ff5d5d` | OSC "error" dot, meter top of gradient |
| `error.text` | `#ff6b6b` (connecting hint), `#ff7d7d` (`.input-panel-inline-status`) | |
| `error.log` | text `#ffb0b0`, bg `rgba(255,92,92,0.16)` | log `error` chip |
| `clip` | `#ff3b30` (indicator), `#ff3b3b` (`.meter-peak.over`) | |
| `info.dot` | `#89a3ff` | OSC "initializing" |
| `info.log` | text `#b8deff`, bg `rgba(86,158,255,0.14)` | log `info` chip |
| `debug.log` | text `#d2c2ff`, bg `rgba(158,118,255,0.15)` | log `debug` chip |
| `trace.log` | text `#b8c1cf`, bg `rgba(168,177,191,0.12)` | log `trace` chip |
| `drc.boost` | `#33b5e5` | DRC gauge, gain > +1 dB (`controls/drc.js:128`) |
| `drc.ok` | `#00c851` | −6…+1 dB |
| `drc.warn` | `#ffbb33` | −12…−6 dB |
| `drc.cut` | `#ff4444` | < −12 dB |
| `banner.error` | bg `rgba(220,38,38,0.92)`, border `#ff8080`, text `#fff` | bridge-error banner (index.html:80) |
| `banner.warn` | bg `rgba(245,158,11,0.92)`, border `#ffd27f`, text `#1a1200` | foreign-renderer banner (index.html:84) |
| `banner.info` | bg `rgba(88,160,255,0.15)`, border `rgba(88,160,255,0.38)`, text `#d9ecff` | update banner (index.html:71) |
| `banner.danger.soft` | bg `rgba(255,107,107,0.10)`, border `rgba(255,107,107,0.42)`, text `#ff6b6b` | `#oscConnectingHint` (index.html:79) |

### 0.5 Radii

| Radius | Applies to |
|---|---|
| `6px` | inputs, selects, `.ui-btn`, `.toggle-btn`, `.gain-box`, `.panel-toggle-btn`, `.info-item`, `.id-strip`, `.channel-direct-target`, inline help panel, `.panel-side-collapse-btn`, subpanel *body* |
| `7px` | banners; `.band-cursor-seg` |
| `8px` | subpanel *shells*, `.input-panel-shell`, `.adaptive-subpanel`, `.telemetry-gauges-form`, `.room-geometry-summary`, `.save-footer-button`, `.band-cursor-all` |
| `9px` | `.log-entry`; `.scene-fx-btn` |
| `10px` | `.info-modal-card` |
| `11px` | `.scene-fx-flyout` |
| `12px` | `#overlay`, `#speakersOverlay` |
| `14px` | `#logOverlay.expanded`, `#bandCursor`, `#sceneEffectBar` |
| `999px` (pill) | `.log-panel-btn`, `.log-panel-filter input`, `.log-entry-level`, `.meter-bar`, `.meter-marker`, scrollbar thumb, switch track |
| `50%` (circle) | `.info-icon-btn`, `.osc-status-dot`, `.clip-indicator`, switch thumb, `.transport-btn` |
| `0.45rem` / `0.8rem` (7.2 / 12.8 px) | app-brand logo / about logo |

egui: `CornerRadius::same(6)` is the workhorse; pill = `CornerRadius::same(h/2)`.

### 0.6 Shadows & blur

| Element | Shadow | Backdrop blur |
|---|---|---|
| `#overlay` / `#speakersOverlay` | none | 8 px |
| `#logOverlay.expanded` | `0 12px 32px rgba(0,0,0,0.20)` | 10 px |
| `.info-modal-card` | `0 14px 36px rgba(0,0,0,0.35)` | none |
| `#bandCursor`, `#sceneEffectBar` | `0 6px 24px rgba(0,0,0,0.45)` | 10 px |
| `.scene-fx-flyout` | `0 8px 28px rgba(0,0,0,0.50)` | 12 px |
| `.app-brand-logo` | `0 0 10px rgba(100,210,255,0.20)` | — |
| `.about-logo` | `0 0 16px rgba(100,210,255,0.25)` | — |
| `.meter-fill` | `0 0 8px rgba(255,140,80,0.35)` glow | — |
| `.meter-peak` | `0 0 4px rgba(255,255,255,0.30)`; `.over`: `0 0 6px rgba(255,59,59,0.9)` | — |
| `.clip-indicator.clip-active` | `0 0 6px rgba(255,59,48,0.9)` | — |

egui has no backdrop blur. Substitute: paint the overlay frame with a slightly more
opaque fill (e.g. `rgba(0,0,0,0.78)` instead of `0.65`) so text stays legible over a
busy 3D scene. Shadows map to `egui::epaint::Shadow { offset, blur, spread: 0, color }`.

### 0.7 Type scale

Sizes are absolute px unless noted. `line-height` 1.45 is the panel default
(`#overlay`, app.css:49-50).

| Size | Weight | Letter-spacing | Use |
|---|---|---|---|
| 16 px (1rem) | 400 | `0.01em` | `.app-brand-title strong` (app title) |
| 16.8 px (1.05rem) | 700 | — | `.about-heading strong` |
| 14 px | 400 | — | overlay base font; banners; `.save-footer-button` (600) |
| 13 px | 400 | — | `.switch-row` labels (inline `font-size:13px`); `#logOverlay.collapsed .log-panel-summary` |
| 13 px | 700 | — | `.info-modal-title` |
| 12.5 px | 400 | — | `.info-list` (right panel) |
| 12 px | 400 | — | the workhorse: `select`, `.ui-btn`, `.osc-config-row`, `.editor-row label`, `.log-*`, `.name-input`, `#spreadInfo`, `.coord-mode-label`, `.input-panel-row label` |
| 12 px | 700, `0.02em`, UPPERCASE | | `.log-panel-title` |
| 11.5 px | 400 | — | `#overlay .info-list` |
| 11 px | 400 | — | `.delay-input`, `.coord-grid input`, `.cart-coord-table input`, `.toggle-btn`, `.gain-box`, `.channel-direct-target`, `.meter-mini-label`, `.cart-coord-head`, `.band-cursor-seg::after` label, inline help panel (`line-height 1.35`) |
| 11 px | 600, `0.02em` | | `.id-strip span` (vertical) |
| 11 px | 400 | — | `.info-icon-btn` glyph; `.panel-toggle-btn` glyph |
| 11.2 px (0.7rem) | 400 | `0.12em`, UPPERCASE | `.app-brand-subtitle` |
| 11 px | 400 | — | `#overlay .panel-title` (**11 px**, `line-height 1.15`, `0.01em`) |
| 10.4 px (0.65rem) | 400 | — | `.output-mode-mpv-note`, `#binauralSofaInfo`, DRC gain readout |
| 10 px | 400 | — | `#overlay .panel-summary`, `.vbap-polar-meta`, `.vbap-step`, `.delay-label`, `.channel-direct-target small`, `.object-topright`, `.meter-subvalues`, `.option-status-note` (inline), `#overlay .info-icon-btn`, `#overlay .panel-toggle-btn` |
| 10 px | 400 | `0.08em`, UPPERCASE | `.input-panel-subtitle` |
| 10 px | 700 | `0.05em`, UPPERCASE | `.log-entry-level` chip |
| 9 px | 400 | — | `.object-coords`, `.band-label`, `.band-db` |
| 7 px | 400 | `-0.02em` | `.filter-freq` cutoff labels |
| 7 px | 600 | — | `.object-size-label` |

Weights actually used: **400** (default), **600** (`.info-title`, `.id-strip span`,
`.room-geometry-summary-label`, `.info-item strong`, subpanel titles, `.save-footer-button`),
**700** (`#status`, `.log-panel-title`, `.log-entry-level`, `.info-modal-title`, banners).

**Note for egui:** the web sheet uses fractional sizes (12.5 px, 11.5 px, 11.2 px,
10.4 px). Per the project memory note "arrondir les tailles de police egui", **round
to integers** when building `TextStyle`s. Suggested egui `TextStyle` set:

| TextStyle | px | family |
|---|---|---|
| `Heading` | 16 | sans, 600 |
| `Body` | 12 | sans |
| `Button` | 12 | sans |
| `Small` | 10 | sans |
| custom `Tiny` | 9 | sans |
| custom `Micro` | 7 | sans |
| `Monospace` | 11 | mono, tabular |
| custom `MonoSmall` | 9 | mono, tabular |

### 0.8 Spacing scale (rem → px)

The sheet uses a dense, hand-tuned scale. Rounded to the nearest sensible px:

| rem | px | typical use |
|---|---|---|
| 0.1 | 1.6 → **2** | micro padding |
| 0.15 | 2.4 → **2** | inline help margin |
| 0.18–0.2 | 2.9–3.2 → **3** | `#overlay .info-list` gap, `.conditional-params.open` gap |
| 0.25 | **4** | `.room-geometry-form` gap, `.info-list` gap |
| 0.3 | **5** | subpanel body gap |
| 0.35 | **6** | most inter-row gaps, `.info-icon-btn` group gap |
| 0.4 | **6.4 → 6** | `.overlay` flex gap, `.panel-header` gap, form gaps |
| 0.45 | **7** | `.object-item` column gap, subpanel h-padding |
| 0.5 | **8** | `.inline-toggle` gap, subpanel h-padding |
| 0.55 | **9** | banner padding-y |
| 0.6 | **10** | banner padding-y (bridge) |
| 0.65–0.7 | 10.4–11.2 → **11** | log header gap, banner padding-x |
| 0.75 | **12** | overlay padding-y |
| 1 | **16** | overlay padding-x, overlay screen margin |

egui: `Spacing { item_spacing: vec2(6.0, 6.0), button_padding: vec2(7.0, 2.5),
window_margin: Margin { left: 16, right: 16, top: 12, bottom: 12 },
indent: 16.0, interact_size: vec2(0.0, 20.0), slider_width: …, scroll: … }`.

The single most important derived constant: **a standard control is 20 px tall**
(a 12 px input with `0.125rem` = 2 px padding + 1 px border each side; the
comment at app.css:1757 says exactly this: "1.2 + the padding below lands the box
on the inputs' 20px"). The compact `.delay-input` variant lands at **16.8 px**.

---

## 1. Layout

### 1.1 Two side overlays

`#overlay` (left, app.css:39-57) and `#speakersOverlay` (right, app.css:569-587) are
identical except for the edge:

```
position: fixed;  top: 1rem (16px);  left|right: 1rem (16px);
height: calc(100vh - 2rem)              → viewport height − 32 px
width: var(--panel-width-left|right)    → content width, default 440 px
background: rgba(0,0,0,0.65) + blur(8px)
border: 1px solid rgba(255,255,255,0.20);  border-radius: 12px
padding: 0.75rem 1rem                   → 12 px top/bottom, 16 px left/right
font-size: 14px;  line-height: 1.45
display: flex; flex-direction: column; gap: 0.4rem (6.4px)
z-index: 2
```

They are **content-box**, so the on-screen footprint of an expanded panel is
`width + 2 × 16 px padding + 2 × 1 px border = width + 34 px`
(`overlay-layout-state.js:8-12`: `PANEL_PADDING_REM = 2`, `PANEL_BORDER_PX = 2`).

**Width state** (`src/ui/layout/overlay-layout-state.js`):

| Constant | Value | Line |
|---|---|---|
| `MIN_WIDTH` | **220 px** | 4 |
| `DEFAULT_WIDTH` | **440 px** | 5 |
| `COLLAPSED_WIDTH_REM` | 1.8 rem → **28.8 px** | 6 |
| `PANEL_EDGE_MARGIN_REM` | 1 rem (16 px) per side | 7 |
| `PANEL_SAFETY_GAP_PX` | 4 px | 12 |
| storage key | `spatialviz.side_panels` (localStorage, JSON `{left:{width,collapsed},right:{…}}`) | 3 |

Max width is dynamic (`clampWidth`, line 76):
`max = max(220, floor(viewportWidth − effectiveWidth(otherSide) − 34 − (32 + 4)))`
where `effectiveWidth(side) = collapsed ? 28.8 : width + 34`.
So the two panels may meet in the middle but never overlap; there is no fixed
upper bound. On every window resize both widths are re-clamped
(`subscribeWindowViewport` → `clampAllWidths`, line 122).

The applied width is published as a CSS variable on `:root` in **px**
(`side-panels.js:52`): `--panel-width-left` / `--panel-width-right`. The
`min(440px, 92vw)` in `:root` (app.css:5-6) is only the pre-JS default and is
overwritten on first layout pass. **A collapsed panel publishes 28.8 px**, not its
stored width — the log overlay and band cursor position off these variables.

**Collapsed strip** (`.panel-collapsed`, app.css:59-72):
padding 0, gap 0, `background: transparent`, no border, no blur, `height: auto`;
every child except the collapse button and the resize handle is `display:none`.
So a collapsed panel is *just the 28.8 px button* floating at `top:16px`,
`left|right:16px`. Clicking anywhere on the collapsed strip re-expands
(`side-panels.js:139-148`).

**Resize handle** (`.panel-resize-handle`, app.css:74-108, built in
`side-panels.js:63-121`):
- 10 px wide, full panel height, absolutely positioned, `right:-5px` (left panel) /
  `left:-5px` (right panel) — i.e. straddling the inner edge. `cursor: ew-resize`.
- Visual: a `::before` pill, **2 × 36 px**, centred, `rgba(255,255,255,0.18)`,
  radius 2 px. On hover / while dragging: `rgba(120,200,255,0.70)` and **height 56 px**.
  Transition `background .12s, height .12s`.
- Drag = pointer capture; width follows `startWidth + direction * dx`
  (direction `+1` left, `−1` right). **Double-click resets to 440 px** (line 119).
- While dragging, `body.overlay-layout-resizing` disables all backdrop filters and
  transitions on the overlays (app.css:143-163) — a performance guard.
- Hidden entirely when the panel is collapsed (app.css:105).

**Collapse button** (`.panel-side-collapse-btn`, app.css:110-141, built in
`side-panels.js:123-137`):
- 28.8 × 28.8 px (1.8 rem), radius 6 px, `rgba(0,0,0,0.6)` bg,
  `1px rgba(255,255,255,0.28)` border, colour `#d9ecff`, font-size 12 px.
- Absolutely positioned at `top: 0.55rem (8.8px)`; left panel → `right: 0.5rem (8px)`;
  right panel → `left: 0.5rem (8px)`. So it sits **inside the panel, on its inner edge**.
- When collapsed it becomes `position: static` and is the only visible child.
- Hover: bg `rgba(255,255,255,0.18)`, border `rgba(255,255,255,0.32)`.
- Icon: **left = hamburger**, three 10 px horizontal lines in a 16×16 viewBox,
  `stroke-width 1.8`, rendered 14×14 (`side-panels.js:21`).
  **Right = a loudspeaker glyph** (filled cone + two arcs), 14×14 (`side-panels.js:22`).
- `title`: `"Toggle controls"` / `"Toggle speakers"` — **hard-coded English, not
  i18n'd** (`side-panels.js:132`). Worth fixing in the port.

### 1.2 Overlay scroll area

`#overlayScroll` / `#speakersOverlayScroll` (app.css:189-206, 588-604):

```
flex: 1 1 auto; min-height: 0;
display: flex; flex-direction: column; gap: 0.4rem (6.4px);
overflow-y: auto; overflow-x: hidden;
scrollbar-gutter: stable both-edges;  scrollbar-width: thin;
padding-right: var(--overlay-scroll-inset)   → 1.35rem = 21.6 px
```

`--overlay-scroll-inset: 1.35rem` (app.css:36) is a *measured* value — the comment
says the previous value left the right-most slider 1 px from the content edge. In
egui: reserve **~22 px** on the right of every scrolling overlay column so the
scrollbar clears the per-section `i` / chevron buttons.

Custom scrollbar (app.css:2623-2660): width **10 px**; track
`rgba(255,255,255,0.04)` pill; thumb `rgba(217,236,255,0.22)` pill with a 2 px
transparent inset (`border: 2px solid transparent; background-clip: padding-box`) —
so the visible thumb is ~6 px wide.

Above the scroll, pinned and never scrolling:
`#aboutOpenArea` (brand), `#profileRow`, `#profileNameRow` (app.css:165-187),
each `flex: 0 0 auto`.

Below the scroll, three mutually-exclusive **pinned editors** (app.css:210-275),
all with the same recipe:
`#speakerEditSection` (right panel), `#channelEditSection`, `#objectTestEditSection`
(left panel) — `flex: 0 0 auto`, `max-height: 45vh`, own `overflow-y:auto`,
same `padding-right: 21.6px` gutter. They stay visible while the list above scrolls.

### 1.3 The `panel-open-max-height` rule (CLAUDE.md constraint)

CLAUDE.md requires that expanding a panel never changes the 3D viewport size. The
whole mechanism rests on: **the overlays are `position: fixed` with a fixed
`height: calc(100vh - 2rem)`**, so no inner growth can reflow the canvas. Inside
them, expandable bodies are bounded:

| Variable / rule | Value | Line |
|---|---|---|
| `--panel-open-max-height` | `min(44vh, 420px)` | app.css:3 |
| `--panel-open-max-height-large` | `min(52vh, 520px)` | app.css:4 |
| `.osc-config-form.open` | `max-height: var(--panel-open-max-height)` + `overflow-y:auto`, `scrollbar-gutter: stable` | app.css:1108-1115 |
| `.room-geometry-form.open` | `max-height: var(--panel-open-max-height-large)` + internal scroll | app.css:1128-1135 |
| `#twoDSourcesBody` | `max-height: min(52vh, 34rem)` (34 rem = 544 px) + internal scroll | app.css:406-412 |
| `.log-panel-body` | `max-height: min(28vh, 260px)` | app.css:783 |
| pinned editors | `max-height: 45vh` | app.css:242, 257, 273 |
| `.telemetry-gauges-form.open` | `max-height: 2000px` (effectively unbounded, but the overlay clips) | app.css:1174 |

**Critical implementation note (app.css:762-767):** collapsible content above the
WebGL canvas is *never* toggled with `display:none ↔ block`; it is hidden with
`max-height: 0; opacity: 0; pointer-events: none` and revealed by restoring them.
The comment states that `display` toggling repeatedly triggered a WebView texture
corruption bug in the 3D view. **This constraint disappears in egui** (immediate
mode, no DOM, no compositor) — the port can and should simply not draw hidden
content. But the *max-height + internal scroll* discipline must be kept: in egui,
wrap an expanded section body in `ScrollArea::vertical().max_height(min(0.44*h, 420.0))`.

Closed state of the two "form" classes: `max-height: 0; opacity: 0; overflow: hidden;
pointer-events: none; margin-top: 0`. Open adds `margin-top: 0.4rem` (osc) /
`0.2rem` (room) / `0.35rem` (telemetry).

`.conditional-params` (app.css:1401-1414) is the generic accordion body:
closed `display:grid; max-height:0; opacity:0; overflow:hidden; pointer-events:none`;
open `.conditional-params.open` → `gap: 0.2rem (3.2px); max-height: none; opacity: 1;
overflow: visible`. Note this one is **unbounded** — it relies on the overlay's own
scroll. `.renderer-panel-root .conditional-params.open` adds `overflow-x: hidden`.

### 1.4 Log overlay

`#logOverlay` (app.css:606-640) is `position: fixed`, **top-anchored and horizontally
sandwiched between the two panels** (not bottom):

```
top: 0.9rem (14.4px);
left:  calc(var(--panel-width-left)  + 4rem + 2px)   → panel width + 66 px
right: calc(var(--panel-width-right) + 4rem + 2px)
z-index: 3;  width: auto;  colour #d9ecff;  overflow: hidden;
display: flex; flex-direction: column; gap: 0.4rem
```

(`4rem + 2px` = 2 × 16 px panel padding + 2 × 1 px border + 2 × 16 px screen margin
+ 2 px slack.)

- **Collapsed** (`.collapsed`, app.css:625): fully transparent — no bg, no border, no
  radius, no shadow, no padding. Only the summary line and the toggle button show:
  header grid becomes `minmax(0,1fr) auto`, gap 7.2 px, `min-height: 0`; the title,
  level select, filter and Copy/Clear buttons are `display:none` (app.css:689-695).
  The summary is bumped to **13 px**, colour `rgba(237,245,255,0.92)`, with
  `text-shadow: 0 1px 8px rgba(0,0,0,0.45)` so it reads over the 3D scene.
  The toggle button loses its chrome: 25.6 × 25.6 px (1.6 rem), no border,
  transparent bg, colour `rgba(237,245,255,0.9)`; hover `rgba(255,255,255,0.08)`.
  Body height 0.
- **Expanded** (`.expanded`, app.css:613-621): bg `rgba(8,11,18,0.58)` + blur 10 px,
  border `1px rgba(255,255,255,0.14)`, radius 14 px, shadow `0 12px 32px rgba(0,0,0,0.2)`,
  padding `0.55rem 0.75rem 0.7rem` (8.8 / 12 / 11.2 px).
  Body: `max-height: min(28vh, 260px)`, `overflow-y:auto`, `padding-right: 0.15rem`.
- Header grid (app.css:642-648): `auto | minmax(0,1fr) | auto | minmax(10rem,18rem) | auto`
  = title, summary, level label+select, filter, actions. `gap: 0.65rem (10.4px)`,
  `min-height: 1.5rem (24px)`. The filter column is **160–288 px**.

Log entry rows (`.log-entry`, app.css:800-810): grid `auto auto 1fr`, gap 8.8 px,
`align-items: start`, bg `rgba(255,255,255,0.04)`, border `1px rgba(255,255,255,0.08)`,
radius 9 px, padding `0.42rem 0.52rem` (6.7 / 8.3 px). List gap `0.32rem (5.1px)`.
Newest first (`log.js:159` reverses).
- `.log-entry-time`: 11 px, `rgba(217,236,255,0.56)`, tabular-nums.
- `.log-entry-level`: 10 px / 700 / `0.05em` / uppercase, pill radius, padding
  `0.08rem 0.42rem`, **`min-width: 3.3rem (52.8px)`**, centred; colours per level as
  in §0.4.
- `.log-entry-message`: 12 px, `line-height 1.4`, `#edf5ff`, `word-break: break-word`.
- Empty state `.log-panel-empty`: 12 px, `rgba(217,236,255,0.62)`.
- Entry cap: **120** (`log.js:16`), oldest spliced off.
- Toggle glyph: `▴` expanded / `▾` collapsed (`log.js:151`).
- Filter input: pill (`radius 999px`), `rgba(255,255,255,0.08)` on
  `1px rgba(255,255,255,0.18)`, padding `0.24rem 0.7rem`, 12 px; placeholder
  `rgba(217,236,255,0.45)`.
- `.log-panel-btn`: pill, `min-width 1.9rem (30.4px)`, `height 1.9rem`, padding
  `0 0.65rem`, 12 px; icon-only variant is a 30.4 px square. Hover bg
  `rgba(255,255,255,0.14)`.
- `.log-panel-level select { min-width: 6.8rem (108.8px) }` — otherwise the shared
  `select` shell.

### 1.5 Scene-effects bar and save footer

`.save-footer` (app.css:1188-1197): `position: fixed; left: 50%; bottom: 1rem;
transform: translateX(-50%); z-index: 2;` a **column** with
`align-items: center; gap: 0.35rem (5.6px)`. It contains, top to bottom:
`#sceneEffectBar`, then `.save-footer-actions` (a row, gap 8 px).

`#sceneEffectBar` (app.css:2799-2809): flex row, `gap: 6px`, `padding: 6px 8px`,
`margin-bottom: 0.2rem`, bg `rgba(18,22,28,0.72)`, border `1px rgba(255,255,255,0.08)`,
radius 14 px, shadow `0 6px 24px rgba(0,0,0,0.45)`, blur 10 px.

`.save-footer-button` (app.css:1204-1214): `min-width: 8.5rem (136px)`, bg
`rgba(255,255,255,0.12)`, border `1px rgba(255,255,255,0.28)`, colour `#d9ecff`,
radius 8 px, **14 px / 600**, padding `0.45rem 1rem` (7.2 / 16 px).

Both roots must be **direct children of `<body>`** — an ancestor with
`backdrop-filter` becomes their containing block and `left:50%` would then centre
inside the panel (`bootstrap-ui.js:16-31` self-heals this). In egui this is a
non-issue: paint them in screen coordinates.

### 1.6 Band cursor

`#bandCursor` (app.css:2743-2760): `position: fixed; top: 50%;
right: calc(var(--panel-width-right) + 2.6rem)` (= right panel width + 41.6 px);
`transform: translateY(-50%)`. Column, `gap: 5px`, `padding: 8px 7px`, same
glass recipe as the scene bar (bg `rgba(18,22,28,0.72)`, border
`1px rgba(255,255,255,0.08)`, radius 14 px, shadow `0 6px 24px rgba(0,0,0,0.45)`,
blur 10 px). Hidden (`[hidden]`) when the layout has fewer than 2 crossover bands
(`band-cursor.js:49-54`).

Segments (`.band-cursor-seg`, app.css:2766-2782): **14 × 30 px**, radius 7 px,
border `1px rgba(255,255,255,0.10)`, background = the band colour, **opacity 0.45**.
Hover: opacity 0.85 + `transform: scaleX(1.18)`. Selected: opacity 1, border
`rgba(255,255,255,0.70)`, `box-shadow: 0 0 10px 1px <seg-color>`.
The "All bands" cap (`.band-cursor-all`) is **14 × 16 px**, radius 8 px,
`margin-bottom: 3px`, and its background is a `linear-gradient(to top, …)` of every
band colour bottom-up; its `--seg-color` (used for the selected glow) is
`rgba(223,232,243,0.8)`.

Order top→bottom: **All**, then band `n−1` … band `0` (highest frequency at the top,
`band-cursor.js:73`).

Frequency-range chip: a `::after` pseudo-element floating **12 px to the left** of the
segment, vertically centred: padding `2px 8px`, radius 8 px, bg `rgba(18,22,28,0.9)`,
border `1px rgba(255,255,255,0.12)`, colour `#d9ecff`, 11 px, `line-height 1.5`,
`white-space: nowrap`. Opacity 0 → 1 on hover **and while selected**
(app.css:2790-2797). In egui: draw a tooltip-styled label at the same offset; keep
it permanently visible for the selected segment.

**Band palette** (`scene/speaker-band-bars.js:39-43`, shared with the per-speaker and
per-object band bars, and with the speaker cubes in the 3D scene):

```
bandColor(index, count):
  if count <= 1 -> "#8ec8ff"                    // FULL_BAND_COLOR
  hue = 8 + 248 * index / (count - 1)           // ~red (low) -> ~blue (high)
  return hsl(hue, 68%, 56%)
```

### 1.7 Modals

`.info-modal` (app.css:1344-1352): `position: fixed; inset: 0;` a flex centring box,
`background: rgba(0,0,0,0.45)`, `z-index: 70`. Closed = `display:none`; open =
`display:flex` via a `.open` class (`modals.js`, one `set*ModalOpen(bool)` per modal).
No open/close animation.

`.info-modal-card` (app.css:1354-1362): `width: min(460px, calc(100vw - 2rem))`,
bg `rgba(14,16,22,0.96)`, border `1px rgba(255,255,255,0.20)`, radius 10 px,
padding `0.8rem 0.9rem` (12.8 / 14.4 px), colour `#d9ecff`,
shadow `0 14px 36px rgba(0,0,0,0.35)`.
The SOFA browser overrides to `max-width: 560px; width: 90vw` (index.html:1012).

- `.info-modal-title`: 13 px / 700, `margin-bottom: 0.45rem (7.2px)`.
- `.info-modal-text`: 12 px, `line-height 1.45`, `rgba(217,236,255,0.92)`.
- `.info-modal-actions`: `margin-top: 0.7rem (11.2px)`, right-aligned flex; the close
  button is a plain `.toggle-btn`.

There are **21 modals** in the tree (`modals.js` getters + index.html), all sharing
this card. Two are out of scope for a "panels" phase but must exist as entry points:
`#sofaBrowserModal` (SOFA browser) and `#autoTunePiWizardModal` (an empty div filled
by the wizard module). `#backendInfoModal` is the only one with **composed content**:
`modals.js:130-149` builds `t('backend.infoBody')` plus, when a
`help.backend.<id>` key exists, `"<br><br><strong>{label}</strong><br>{specific}"`.

About modal extras: `.about-logo-row` (flex, gap 13.6 px, `margin-bottom 12.8 px`),
`.about-logo` 48 × 48 px radius 12.8 px with cyan glow, `.about-heading strong`
16.8 px `#edf5ff`, `.about-heading span` 11.8 px `rgba(217,236,255,0.7)` uppercase
`0.08em`; `.about-info-grid` is a `auto 1fr` dl, gap `0.45rem 0.8rem` (7.2 / 12.8 px),
12 px, `dt` `rgba(217,236,255,0.62)`, `dd` `#edf5ff` with `word-break: break-word`.

---

## 2. Generic widgets

### 2.1 `.info-section` and `.panel-header`

`.info-section` (app.css:1082-1089):
```
margin-top: 0.75rem (12px);  padding-top: 0.5rem (8px);
border-top: 1px solid rgba(255,255,255,0.12);
display: flex; flex-direction: column; min-height: 0;
```
The first section inside `#audioPanelRoot` drops the rule and the margins entirely,
and its header gets `padding-left: 2.2rem (35.2px)` to clear the right panel's
collapse button (app.css:1091-1098).

**The left panel runs a denser variant** (app.css:296-402). Both must be supported:

| Property | Right panel (base) | Left panel (`#overlay …`) |
|---|---|---|
| `.info-section` margin-top / padding-top | 12 / 8 px | **8.8 / 5.6 px** |
| collapsed section margin-top / padding-top | — | **7.2 / 4.5 px** |
| `.panel-header` min-height | (natural) | **27.2 px**, collapsed **24.8 px** |
| `.panel-header` gap | 6.4 px | **4.8 px** |
| `.panel-header-main` gap | 8 px | **5.6 px** |
| `.panel-title` font | 600 (inherits 14 px) | **11 px**, `line-height 1.15`, `0.01em` |
| `.panel-summary` font | 11 px | **10 px**, `line-height 1.15` |
| `.info-icon-btn` | 18 × 18 px, 11 px glyph | **17 × 17 px, 10 px glyph** |
| `.panel-toggle-btn` | min-w 28.8 px, h 20.8 px, 11 px | **min-w 26.4 px, h 19.2 px, padding 1.3/5.4 px, 10 px** |

`#roomGeometryPanelRoot` opts into the left-panel density explicitly (same selectors).

Header structure (`ui/ui-primitives.js:1-19`, `panelHeader()`):
```
.panel-header                       flex, space-between, align-center, gap 6.4px
  .panel-header-main                flex 1, gap 8px, min-width 0
    .panel-title-wrap               flex 1, gap 8px, min-width 0
      .info-title.panel-title       [data-i18n=titleKey]      600 weight
      .panel-summary  (optional)    id=summaryId, initially display:none
    .info-icon-btn   (optional)     "i", [data-i18n-title=…]
  .panel-toggle-btn                 "▸"
```
`.info-title` on its own (app.css:1284): `font-weight: 600; margin-bottom: 0.35rem`.
`.panel-title` resets that margin to 0.

`.panel-summary` (app.css:288-295): `flex: 1 1 auto; min-width: 0; font-size: 11px;
color: #9eb4c8; white-space: nowrap; overflow: hidden; text-overflow: ellipsis`.
Visibility is inverted with the body: **shown only while the section is collapsed**
(`modals.js` sets `summaryEl.style.display = open ? 'none' : 'block'`). In egui: draw
the summary as an ellipsised single line in the header row when collapsed.

**Chevron / collapsed state.** `.panel-toggle-btn` text is `▸` (closed) / `▾` (open),
flipped by every `set*SectionOpen()` in `modals.js`. In parallel the section root gets
`.section-collapsed` toggled (`!open`), which only tightens the spacing (table above).
There is **no rotate animation** — the glyph is swapped.

`.panel-toggle-btn` (app.css:1325-1341):
```
min-width 1.8rem (28.8px); height 1.3rem (20.8px); inline-flex centred;
bg rgba(255,255,255,0.08); border 1px rgba(255,255,255,0.20); radius 6px;
color #d9ecff; font-size 11px; line-height 1; padding 0.1rem 0.4rem (1.6/6.4px);
transition background/border-color/color .12s ease
hover: bg rgba(255,255,255,0.16); border rgba(255,255,255,0.28); color #ffffff
```

`.info-icon-btn` (app.css:1295-1323):
```
18 × 18 px; border-radius 50%; border 1px rgba(255,255,255,0.30);
bg rgba(255,255,255,0.08); color #d9ecff; font-size 11px; line-height 1; padding 0
hover: bg rgba(255,255,255,0.16)
.is-active / [aria-pressed=true]: bg rgba(82,226,162,0.28),
    border rgba(82,226,162,0.60), color #eaffe4
```
Glyph is the literal letter `i` (or `?` for the About button, index.html:75).

**Help affordance — two distinct mechanisms:**
1. **`i` button → modal.** `.info-icon-btn` + a `.info-modal` (§1.7).
2. **Inline help** (`src/controls/inline-help.js`). Any element carrying
   `data-help-i18n="<key>"` becomes a *clickable parameter name*: it gets
   `role="button"`, `cursor: pointer`, and
   `text-decoration: underline dotted rgba(217,236,255,0.4)` with
   `text-underline-offset: 2px` (lines 65-68). Clicking it toggles a panel inserted
   **after the row** (`.control-row`/`.inline-toggle` by default, or the element
   matched by `data-help-anchor="<selector>"` via `closest()`).
   Panel style (`makeHelpPanel`, lines 34-48):
   ```
   margin-top 0.15rem (2.4px); padding 0.3rem 0.45rem (4.8/7.2px);
   font-size 11px; line-height 1.35; color #d9ecff;
   background rgba(120,200,255,0.10); border 1px rgba(120,200,255,0.30);
   border-radius 6px
   ```
   **Exactly one help panel is open at a time, globally** (module-level `openHelp`),
   and any click outside it closes it. A key whose translation is missing
   (`t(key) === key`) is skipped so a raw key never leaks (line 92).
   There are **170 `help.*` keys** in `en.json`.

### 2.2 `.conditional-params` / `.inline-toggle`

`.conditional-params` — see §1.3.

`.inline-toggle` (app.css:978-985): `display:flex; align-items:center; gap: 0.5rem
(8px); margin-top: 0.2rem (3.2px); justify-content: space-between; min-width: 0`.
Its first child (and `.title-with-info > :first-child`, `.control-row > label`,
`.room-geometry-summary-*`) gets `min-width: 0; white-space: normal;
overflow-wrap: anywhere` (app.css:987-994) — **labels wrap rather than overflow**.

`.switch-row` (app.css:962-968): same idea, `margin-top: 0.25rem (4px)`, gap 8 px.
Used with `font-size: 13px; cursor: pointer` inline on the wrapping `<label>`.

`.title-with-info` (app.css:1286-1293): flex, space-between, gap 6.4 px, min-width 0 —
a label plus its `i` button.

`.control-row` (app.css:1712-1719): `display:grid;
grid-template-columns: 1fr auto auto auto; gap: 0.35rem (5.6px);
align-items: center; margin-top: 0.2rem; min-width: 0`. Variants:
- `.speaker-item .control-row`: `minmax(0,1fr) minmax(0,1fr) auto auto`, `padding-right: 5.6px`
- `.object-item .control-row`: `minmax(0,1fr) auto`, `padding-right: 5.6px`
- `#masterSection .control-row`: `1fr auto`

### 2.3 Switches (`toggle-switch` look)

There is **no `.toggle-switch` class**; the switch is a restyled
`input[type=checkbox]` under four selectors (app.css:1051-1080):
`.switch-row`, `.inline-toggle`, `.control-row`, `.renderer-subpanel-actions`.

```
track:  34 × 18 px, radius 999px,
        bg  rgba(255,255,255,0.18)   (off)
            rgba(80,200,120,0.45)    (on)
        border 1px rgba(255,255,255,0.20)
thumb:  ::after, 12 × 12 px circle, background #d9ecff,
        top 2px left 2px, transform translateX(16px) when checked,
        transition transform 0.2s ease
```
There is **no explicit disabled style** for this switch. Disabled parameter groups are
dimmed at the container level instead: `.adaptive-param-disabled { opacity: 0.42 }`
and `.adaptive-param-disabled input { opacity: 0.72 }` (app.css:1156-1162).

A **second, smaller switch** exists for the VBAP axis drivers
(`.axis-driver input[type=checkbox]`, app.css:2001-2033):
```
track 1.8rem × 1rem (28.8 × 16 px), radius 999px,
      bg rgba(255,255,255,0.12), border 1px rgba(255,255,255,0.30)
thumb 0.75rem (12 px) circle, #dfe8f3, top/left 1px,
      checked: translateX(0.8rem = 12.8px)
checked track/border are UNCHANGED (deliberate: it marks an axis, not an on/off state)
disabled: opacity 0.45; cursor default
transition: border-color .12s, transform .12s
```

### 2.4 Toggle buttons and tab pairs

`.toggle-btn` (app.css:2046-2060):
```
bg rgba(255,255,255,0.08); border 1px rgba(255,255,255,0.20);
color #dfe8f3; radius 6px; font-size 11px;
padding 0.1rem 0.35rem (1.6 / 5.6 px); cursor pointer
.active     -> bg rgba(255,255,255,0.20); color #ffffff
```
Shared with `.ui-btn` (app.css:471-486): `align-self: stretch; display: inline-flex;
align-items: center; justify-content: center; text-align: center; user-select: none`.
The `align-self: stretch` is load-bearing — it makes every button in a row agree on
height even when one label wraps to two lines (see the long comment at app.css:461-470).

`.toggle-btn.effective` (app.css:2062-2075) — "this option is the one actually in
force":
```
box-shadow: inset 0 0 0 1px rgba(96,255,154,0.75), 0 0 0 1px rgba(42,163,87,0.25);
::after  -> an 8 × 8 px circle, margin-left 5.6px, background #5cff9a,
            box-shadow 0 0 8px rgba(92,255,154,0.7), vertical-align middle
```

**Tab pairs** reuse `.toggle-btn` plus `.renderer-tab-btn` (app.css:2712-2719):
```
.renderer-tab-btn        { opacity: 0.55; flex: 1 1 0; }
.renderer-tab-btn.active { opacity: 1;
                           border-color: rgba(86,156,255,0.80);
                           background:  rgba(86,156,255,0.18);
                           color:       #cfe4ff; }
```
Two instances:
- **Renderer / Binaural** (`ui/renderer-panel.js:57-60`) — `#rendererTabsBar`,
  a flex row, `gap: 0.25rem (4px)`, `padding: 0 0.1rem`. Which pane shows is driven
  by a body class `studio-tab-binaural` (app.css:2699-2711); the buttons only mirror
  it, so a tab change re-renders nothing.
- **Speaker Edit / Test** (`index.html:808-811`) — `#speakerTabsBar`, same flex row
  plus `margin-bottom: 0.3rem`; body class `speaker-tab-test` (app.css:2723-2726).

`M` / `S` (mute / solo) buttons on every list row are plain `.toggle-btn`s carrying
the literal characters `M` and `S` (`speakers.js:715, 725, 1399, 1409`;
`controls/headphone-meter.js:98-107`); `.active` marks muted / soloed.

### 2.5 Buttons

| Class | Style | Line |
|---|---|---|
| `.ui-btn` (secondary, default) | bg `rgba(255,255,255,0.08)`, border `1px rgba(255,255,255,0.20)`, colour `#cfd9e8`, radius 6 px, **12 px**, padding `0.15rem 0.6rem` (2.4 / 9.6 px) | app.css:452-460 |
| `.ui-btn.ui-btn-primary` | bg `rgba(255,255,255,0.10)`, border `rgba(255,255,255,0.25)`, colour `#d9ecff` | 488-492 |
| `.ui-btn.ui-btn-success` | bg `rgba(80,200,120,0.20)`, border `rgba(80,200,120,0.40)`, colour `#d9ecff` | 494-498 |
| `.ui-btn.ui-btn-accent` | bg `rgba(88,160,255,0.18)`, border `rgba(88,160,255,0.38)`, colour `#d9ecff` | 500-504 |
| `.ui-btn-compact` | padding `0.1rem 0.45rem` (1.6 / 7.2 px) | 506-508 |
| `.save-footer-button` | see §1.5 | 1204 |
| `.log-panel-btn` | pill, 30.4 px tall | 700 |
| `.panel-toggle-btn` | icon button, see §2.1 | 1325 |
| `.info-icon-btn` | 18 px round icon button | 1295 |
| `.scene-fx-btn` | 40 × 36 px icon button, see §2.11 | 2811 |
| `.transport-btn` | 34 px round, see below | 2939 |
| `.mini-btn` | **UNSTYLED** — no CSS rule exists. Used by `controls/vbap.js:508,523` (Browse / Edit on file params) and `controls/script-editor.js:144`. Renders as a native platform button. Treat as a bug; in egui, style it as `.ui-btn-compact`. | — |

There is **no dedicated "danger" button class.** Destructive intent is conveyed
per-instance with inline styles: `controls/osc.js:158-170` recolours
`#oscLaunchRendererBtn` to `background rgba(255,96,96,0.18)`,
`border-color rgba(255,96,96,0.38)`, `color #ffe2e2` when the renderer is running
(i.e. the button will stop it), and back to the blue accent otherwise. Adopt that
as the danger token: **bg `rgba(255,96,96,0.18)`, border `rgba(255,96,96,0.38)`,
text `#ffe2e2`**.

Generic disabled convention (there is no `:disabled` rule on `.ui-btn`; it is applied
per-site): `opacity: 0.45; cursor: default` (app.css:180-183 for profile buttons;
`osc.js:142-144` sets the same inline). Selects use `opacity: 0.5; cursor: not-allowed`
(app.css:1789-1792). `.transport-btn:disabled` uses `opacity: 0.4; cursor: not-allowed`.

`.transport-btn` (object-injection play/stop, app.css:2933-2985):
```
34 × 34 px circle; border 1px rgba(255,255,255,0.22); bg rgba(255,255,255,0.07);
color rgba(223,232,243,0.72); svg fill currentColor
hover:            bg rgba(255,255,255,0.12); color rgba(223,232,243,0.95)
focus-visible:    border rgba(82,226,162,0.60), no outline
[aria-pressed=true] (playing): bg rgba(82,226,162,0.18),
    border rgba(82,226,162,0.60), color #7af0c0,
    animation transport-pulse 2s ease-in-out infinite
      (box-shadow 0 0 0 0 rgba(82,226,162,0.34)  ->  0 0 0 5px rgba(82,226,162,0))
    hover: bg rgba(82,226,162,0.26); color #aef5d8
```
`@media (prefers-reduced-motion: reduce)` disables the pulse (app.css:2991).

**First-click reliability (WebKitGTK) — irrelevant to egui but explains the markup.**
`user-select: none` is sprinkled over every button, `.switch-row`, `.panel-title`, and
inner `<span>`s get `pointer-events: none` (app.css:174-186, 477-484, 1794-1797,
3162-3166). None of this needs porting.

### 2.6 Selects

The `select` element is fully restyled (app.css:1745-1800) — read the comment there;
it explains the height-matching intent.
```
appearance: none; font-family: inherit; font-size: 12px; line-height: 1.2;
color #d9ecff; background-color rgba(255,255,255,0.08);
background-image: --select-chevron, no-repeat, right 0.45rem (7.2px) center;
border 1px rgba(255,255,255,0.20); border-radius 6px;
padding 0.125rem 1.35rem 0.125rem 0.4rem   (2 / 21.6 / 2 / 6.4 px)
cursor: pointer
hover (:not(:disabled)): bg rgba(255,255,255,0.12); border rgba(255,255,255,0.32)
focus-visible:           border rgba(255,255,255,0.45); no outline
disabled:                opacity 0.5; cursor not-allowed
option:                  background-color #12141c; color #d9ecff
```
Resulting box height: **20 px** (matches text inputs).

The chevron is a data-URI SVG held once in `:root` (app.css:9):
`10 × 6 viewBox`, path `M1 1l4 4 4-4`, stroke `#d9ecff`, `stroke-width 1.4`, round
caps/joins. It is re-declared in the two variants below because they set the
`background` shorthand, which resets `background-image`.

Two density variants:
- `select.delay-input` (app.css:1802-1817): `padding-top/bottom: 1px` (→ 16.8 px tall
  to match the 11 px `.delay-input`), chevron at `right 0.4rem (6.4px)`,
  `text-align: left`.
- `select.micro-select` (app.css:1819-1829): `font-size: 10px`,
  `padding: 1px 1.05rem 1px 0.3rem` (1 / 16.8 / 1 / 4.8 px), chevron at
  `right 0.32rem (5.1px)`. Used in the diag-plot strip.

`.form-select` appears in markup (`#profileSelect`, `#binauralHrirSource`) but
**has no CSS rule** — it is inert; those selects get the base shell plus inline
overrides.

### 2.7 Text and number inputs

Three near-identical recipes; unify them in egui as one `TextEdit` frame with two
sizes.

| Class | Size | Padding | Colour | Align | Line |
|---|---|---|---|---|---|
| `.delay-input` | **width 52 px**, 11 px | `0.1rem 0.3rem` (1.6 / 4.8 px) | `#dfe8f3` | right | app.css:1723-1732 |
| `.osc-config-row input` | width 100 %, 12 px | `0.15rem 0.4rem` (2.4 / 6.4 px) | `#d9ecff` | left | 1226-1237 |
| `.coord-grid input`, `.cart-coord-table input` | width 100 %, 11 px | `0.15rem 0.35rem` (2.4 / 5.6 px) | `#dfe8f3` | right | 2537-2547, 2578-2588 |
| `.name-input` | width 100 %, 12 px | `0.2rem 0.4rem` (3.2 / 6.4 px) | `#dfe8f3` | left | 2601-2610 |
| `#profileNameRow input` | 12 px (0.75rem), padding `0.1rem 0.2rem` | | | | 172-176 |

All share: `background rgba(255,255,255,0.08)`, `border 1px rgba(255,255,255,0.20)`,
`border-radius 6px`, `box-sizing: border-box`, `outline: none`.
Focus border: `rgba(255,255,255,0.45)` — except `.osc-config-row input:focus` which
uses `rgba(100,180,255,0.5)`.

`.derived-field` (app.css:1969-1977) hides the native number spinners
(`appearance: textfield`, `::-webkit-*-spin-button { display: none }`) — in egui,
`DragValue`/`TextEdit` have no spinners anyway.

`.input-panel-danger` (app.css:445-448): `border-color: rgba(255,90,90,0.95)
!important; box-shadow: 0 0 0 1px rgba(255,90,90,0.2)` — the invalid-field marker.
Paired with `.input-panel-inline-status` (app.css:438-444): `display:none` by default,
`margin: -0.2rem 0 0.15rem`, `padding-left: 7.2rem (115.2px)` (aligns under the field
column), 12.8 px, colour `#ff7d7d`.

### 2.8 Range sliders

**Range inputs are almost entirely unstyled.** `.gain-slider { width: 100% }`
(app.css:2042) is the only rule; everything else is the platform's native slider
under `color-scheme: dark`. Two exceptions:

1. `.dual-range` (app.css:1002-1046) — a two-thumb overlay used by the spread control:
   ```
   container: position relative; flex 1 1 auto; height 18px
   each input: absolutely stacked, width 100%, height 18px,
               background transparent, pointer-events: none
   thumb:      10 × 10 px circle, background #d9ecff,
               border 1px rgba(0,0,0,0.4), pointer-events: auto
   track:      height 4px, background rgba(255,255,255,0.12), radius 999px
   ```
   **Use this as the canonical slider look in egui**: 4 px track
   `rgba(255,255,255,0.12)`, 10 px round thumb `#d9ecff` with a
   `rgba(0,0,0,0.4)` hairline. It is the only place the designer actually specified
   a slider, and it is consistent with the rest of the palette.

2. `.object-test-radius-slider` (app.css:3244-3281) — a native slider drawn over a
   tick layer. `.object-test-radius-ticks` is an absolutely positioned 9 px tall strip
   painting 1 px `rgba(217,236,255,0.5)` marks at **35.355 %, 43.301 %, 70.711 %,
   86.603 %** of the width (= √2/4, √3/4, 2√2/4, 2√3/4 on a 0…4 range). In egui,
   paint these as ticks on the slider track.

**Value readout.** Two conventions:
- `.gain-box` (app.css:2077-2084): `min-width: 54px; text-align: right;
  padding: 0.1rem 0.3rem (1.6/4.8px); border-radius: 6px;
  background: rgba(255,255,255,0.08); font-size: 11px; color: #d9ecff`.
  Sits in `.editor-row.gain-row` (`grid-template-columns: auto 1fr auto`,
  app.css:2515) — i.e. **label | slider | boxed value**.
- Generated backend params (`controls/vbap.js:551-557`): a bare
  `<span class="val">` with `display:inline-block; min-width: 3.5em;
  text-align: right` — fixed-width so a changing digit count never shifts the slider.

Generated slider param rules (`vbap.js:537-559`): `min = kind.min ?? 0`,
`max = kind.max ?? 1`, `step = isInt ? 1 : (kind.step ?? 0.01)`; the readout updates
on `input`, the value is sent on `change` (`Math.round` for ints).

### 2.9 Meters

`.meter-row` (app.css:1521-1526): grid `8ch 1fr`, gap 6.4 px, centred. Variants:
- `.speaker-meter-row`: `auto auto 8ch 1fr auto`
  (position icon | filter glyph | dB | bar | M/S)
- `.speaker-meter-row.hp-meter-row`: `auto 8ch 1fr auto` (no filter glyph)
- `.object-item .meter-row`: `auto 8ch 1fr 32px auto`
  (position icon | dB | bar | size gauges | M/S)

`.fixed-metric` (app.css:1541-1549): `width: 8ch; min-width: 8ch;` monospace stack,
`font-variant-numeric: tabular-nums; text-align: right; white-space: pre`.

`.meter-bar` (app.css:1556-1563):
```
position: relative; height: 6px; border-radius: 999px; overflow: hidden;
background: linear-gradient(90deg,
    rgba(77,215,255,0.18)  0%,
    rgba(123,255,106,0.18) 60%,
    rgba(255,209,58,0.18)  82%,
    rgba(255,93,93,0.18)   100%)
```
i.e. the *unlit* track is the lit gradient at 18 % alpha.

`.meter-fill` (app.css:1675-1682):
```
position: absolute; inset: 0; width: 100%;
background: linear-gradient(90deg, #4dd7ff 0%, #7bff6a 60%, #ffd13a 82%, #ff5d5d 100%);
box-shadow: 0 0 8px rgba(255,140,80,0.35);
clip-path: inset(0 calc(100% - var(--level, 0%)) 0 0);
```
**The gradient is fixed to the full bar**, and the fill is *revealed* by clipping —
so the colour at a given x is always the same regardless of level. In egui: paint the
full gradient into the bar rect and clip to `level%`.

`.meter-peak` (peak-hold marker, app.css:1684-1691): the same gradient clipped to a
**2 px slice** at the hold position —
`clip-path: inset(0 calc(100% - var(--level) - 1px) 0 var(--level))`,
`box-shadow: 0 0 4px rgba(255,255,255,0.3)`, `z-index: 2` (3 inside `.level-meter`),
`pointer-events: none`, `transition: clip-path 0.1s linear, opacity 0.2s ease`.
Hidden (`opacity: 0`) when the hold is ≤ 0.1 % (`mute-solo.js:104`).
`.meter-peak.over` → solid `#ff3b3b` with `box-shadow: 0 0 6px rgba(255,59,59,0.9)`,
applied when `peakHoldDbfs >= 0` (`mute-solo.js:105`).

**dB scale mapping** (`src/mute-solo.js:50-59`) — the single source of truth:
```
METER_DB_MIN = -60 ; METER_DB_MAX = +6
dbToMeterPercent(db) = clamp(((db - (-60)) / 66) * 100, 0, 100)
  ->  0 dBFS lands at 90.909…%
```
The bar and the cursor both plot **peak** (`peakDbfs` / `peakHoldDbfs`); the numeric
readout is **RMS** (`rmsDbfs`), formatted `toFixed(1)` + `" dB"`
(`mute-solo.js:42-46, 88-106`). Missing meter → the literal `"— dB"`.

The over-0 headroom zone is painted by `.meter-bar.level-meter::after`
(app.css:1699-1710): a band from `left: 90.9%` to the right edge, filled
`rgba(255,80,80,0.32)` with `border-left: 1px solid rgba(255,230,230,0.75)`,
`z-index: 2`.

Layering inside `.level-meter`: fill `z-index 1`, headroom band `2`, peak cursor `3`.

Specialised fills:
| Class | Gradient | Glow |
|---|---|---|
| `.meter-fill.contribution` | `rgba(138,240,255,0.92) → rgba(255,226,122,0.92)` | `0 0 8px rgba(138,240,255,0.24)` |
| `.meter-fill.latency` | `#52e2a2 0% → #ffd56a 60% → #ff8a5c 82% → #ff4d4d 100%` | `0 0 6px rgba(255,160,90,0.35)` |
| `.meter-fill.latency-ctrl` | `#7aa8ff 0% → #8af0ff 52% → #7bffb8 100%` | `0 0 6px rgba(122,168,255,0.35)` |
| `.meter-fill.resample-pos` | `#8af0ff → #7bffb8` | `0 0 6px rgba(123,255,184,0.3)` |
| `.meter-fill.resample-neg` | `#ffd56a → #ff8a5c` | `0 0 6px rgba(255,138,92,0.3)` |

`.meter-bar.resample-meter-shell` (app.css:1571-1582) is a symmetric 7-stop gradient
(red edges → cyan centre) with `.resample-meter-center`: a 2 px
`rgba(217,236,255,0.6)` pill at `left: calc(50% - 1px)`, extending 1 px above and
below the bar.

`.meter-marker` (app.css:1620-1637): 2 px wide pill, `top:-1px; bottom:-1px`,
`opacity: 0.95`. `.min` → `rgba(255,160,90,0.95)` + `0 0 4px` glow;
`.max` → `rgba(255,213,106,0.95)` + `0 0 4px` glow.
`.meter-range-mask` (app.css:1565-1570): an absolutely positioned
`rgba(6,9,14,0.42)` veil over an out-of-range span.

`.meter-stack` / `.meter-mini-row` / `.meter-subvalues` (app.css:1584-1618):
stack gap `0.22rem (3.5px)`; mini rows are `4.2rem (67.2px) | 1fr` with gap 5.6 px;
mini label 11 px monospace tabular; subvalues a centred flex row, gap `0.7rem (11.2px)`,
10 px `#b9c7d8` monospace, last child right-aligned; `.placeholder` →
`visibility: hidden` (reserves height).

`.clip-indicator` (app.css:965-976): 9 × 9 px circle, `background
rgba(255,255,255,0.18)`, `border 1px rgba(255,255,255,0.25)`, `flex: 0 0 auto`,
`transition: background .15s, box-shadow .15s`. `.clip-active` → `#ff3b30` fill and
border, `box-shadow: 0 0 6px rgba(255,59,48,0.9)`.

Speaker clip flash (app.css:2166-2178): a 1 s `ease-out` animation on the row's
`.id-strip` only, from `background-color rgba(255,59,48,0.85)` +
`inset 0 0 0 1px rgba(255,59,48,0.9)` back to `rgba(0,0,0,0.55)` / transparent ring.

### 2.10 `.info-list` and its rows

`.info-list` (app.css:1472-1484):
```
font-family: "JetBrains Mono", "SFMono-Regular", Menlo, monospace;
font-size: 12.5px;  (left panel: 11.5px)
display: grid; gap: 0.25rem (4px);  (left panel: 0.18rem = 2.9px)
align-content: start; grid-auto-rows: max-content;
max-height: none; overflow: visible;   <- the list does NOT scroll itself
```
The list never scrolls; the overlay's scroll area does. `#objectsList` adds
`padding-right: 0.6rem (9.6px)` (`0.2rem` in the left panel) — a leftover gutter
(see the comment at app.css:1486-1493 explaining why `#speakersList` no longer has it).

`.info-item` (app.css:1502-1509): `display: grid; grid-template-columns: 1fr;
gap: 0.1rem (1.6px); padding: 0.25rem 0.35rem (4/5.6px)`
(left panel `0.18rem 0.28rem` = 2.9/4.5 px), `border-radius: 6px`,
`background: rgba(255,255,255,0.04)`. `.info-item strong { font-weight:600; color:#d9ecff }`.

Row states (app.css:2077-2094, `flush.js:240-243`):
| State | Style | Set when |
|---|---|---|
| `.is-dimmed` | `opacity: 0.45` | another row is soloed, or (objects) the metadata says silent |
| `.is-muted` | `opacity: 0.35` | this row is muted |
| `.is-selected` | `background: rgba(46,110,64,0.45); border: 1px solid rgba(90,200,120,0.35)` | this row is the selected speaker/source |
| `.is-dragging` | `opacity: 1; background: rgba(72,140,92,0.55); border: 1px solid rgba(120,225,150,0.65)` | speaker being dragged |

**`.speaker-item` / `.object-item`** (app.css:2096-2163): `display: grid;
grid-template-columns: 18px 1fr; gap: 0.45rem (7.2px); align-items: stretch`.
The 18 px column is the vertical name badge.

**`.id-strip`** (badge / colour swatch, app.css:2196-2225):
```
background rgba(0,0,0,0.55); border-radius 6px; flex centred;
padding 0.2rem 0.1rem (3.2 / 1.6 px);
transition background/box-shadow/border-color 120ms ease
span: writing-mode vertical-rl; text-orientation mixed;
      font-weight 600; font-size 11px; letter-spacing 0.02em; color #d9ecff
.flip span -> transform rotate(180deg)   (reads bottom-to-top)
```
Variants:
- `.object-item.has-active-trail .id-strip` → bg `rgba(124,231,255,0.28)`,
  `inset 0 0 0 1px rgba(124,231,255,0.34)`.
- `.object-item.object-colorized .id-strip` → bg
  `color-mix(in srgb, var(--object-accent) 34%, rgba(0,0,0,0.55))`, ring
  `color-mix(… 52%, rgba(255,255,255,0.12))`. With an active trail: 55 % / 64 % mixes
  toward the cyan values. `--object-accent` is set per row from the object's 3D
  colour (`sources.js:273-291`, `rgb(r,g,b)` from `getObjectBaseColor`), and the
  badge text is forced to `#edf5ff`. **This is the colour-swatch mechanism.**
  In egui: `mix = a*0.34 + b*0.66` in linear sRGB (or plain sRGB — the difference is
  imperceptible at these alphas).
- `.object-type-icon` inside the badge (app.css:2280-2288): forced back to
  `horizontal-tb`, no rotation, **12 px**, `line-height 1`. Glyphs: `▲` = height
  upmix, `◇` = phantom; `.type-phantom` colours it `#ffe66d`.
- Speaker badges are **drag handles**: `cursor: grab` / `:active { cursor: grabbing }`.

**Name truncation** — `.object-head > :first-child` (app.css:2352-2358): `min-width: 0;
font-size: 11px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis`.
`.object-head` is `minmax(0,1fr) 95px`, gap 8 px. `.object-topright` is a fixed
**95 px** right-aligned 10 px `#b9c7d8` ellipsised cell.

`.object-coords` (app.css:2237-2247): grid `repeat(3, 6.8ch) 1.5ch repeat(3, 9.5ch)`,
gap 0, monospace tabular, **9 px**, `#8a9aac`. A `::after` `"|"` at `opacity 0.3`
occupies column 4 as the XYZ / AED separator.

`.object-size-gauges` (app.css:2302-2348): a 32 px wide stack of three rows
(`8px 1fr`, gap 2 px). Label 7 px / 600 / `#8a9aac`. Bar **2 px tall**, radius 1 px,
`rgba(255,255,255,0.06)`. Fills, `transition: width 80ms ease-out`:
- W: `rgba(255,168,122,0.85) → rgba(255,226,122,0.95)`
- D: `rgba(122,200,255,0.85) → rgba(138,240,255,0.95)`
- H: `rgba(160,255,168,0.85) → rgba(218,255,138,0.95)`

`body.hide-object-details` hides `.object-head` and tightens `.object-item`
vertical padding to `0.2rem` (app.css:2290-2297).

**Band contribution bars** (app.css:2438-2503): `.speaker-contrib-row` /
`.object-contrib-row` are `flex; align-items: flex-start; gap: 0.4rem;
margin-top: 4px`. `.band-contrib-bars` is a column, `gap: 3px`.
`.band-row` is `flex; gap: 5px`. `.band-label` 9 px `#8a9ab0`, `min-width: 52px`,
tabular. `.band-bar` `flex: 1; height: 6px; radius: 3px;
background rgba(255,255,255,0.08)`, with a `::after` clipped to `--level` and filled
with `--band-color` (default `#8ec8ff`) — set per band from `bandColor()`.
`.band-db` 9 px `#8a9ab0`, `min-width: 40px`, right-aligned, tabular.

**Position thumbnail** (`speakers.js:842-868`): a 16 × 16 SVG — an outlined square
(`rect 0.6,0.6,14.8,14.8 rx 1.2`, `stroke-width 0.9`, `currentColor`, or **`#000`**
when `speaker.spatialize === 0`) with a 3.2 × 3.2 rounded marker at the normalised
(x, y). Marker colour encodes height: `heightToColor(z) = hsl(240*(1−clamp(z,0,1)),
75%, 52%)` — blue at 0, green at 0.5, red at 1. Container `.speaker-position-icon` /
`.object-position-icon` is `inline-flex` centred, colour `#5d6b7d`.

**Crossover filter glyph** (app.css:2404-2436): `.speaker-filter-icon` is a centred
column, `line-height 1`, colour `#8fb0d0`; `[data-filter='full']` de-emphasises to
`#5d6b7d`. Cutoff labels `.filter-freq` are **7 px**, `#9fb6cf`, tabular,
`letter-spacing -0.02em`; empty ones `display:none` so the group stays centred.
Top label = `freqHigh`, bottom = `freqLow`.

**Pointer-events discipline** (app.css:2118-2163) — readouts inside a row are
`pointer-events: none` so a click anywhere selects the row (the text nodes churn
continuously and were swallowing clicks). Only `.object-meter-actions` /
`.speaker-meter-actions` and the `.id-strip` drag handle keep their own events.
In egui: make the whole row one `Response`, and only the M/S buttons separate.

### 2.11 `.option-status-note`, `.panel-summary`, banners, status dots

**`.option-status-note` has no CSS rule.** It is a naming convention; every use site
carries inline `font-size:10px` plus `opacity: 0.75 / 0.8 / 0.85`, sometimes
`padding-left: 0.5rem (8px)` and `font-variant-numeric: tabular-nums`
(index.html:142, 154, 181, 621, 634, 636, 641). **Token: 10 px, inherited colour at
80 % opacity, optional 8 px indent.**

`.panel-summary` — §2.1.

**Banners** are all inline-styled in `index.html`; there is no banner class. Common
shape: `margin-top: 0.4rem; border-radius: 7px; line-height: 1.5`.

| Banner | Padding | Font | Colours | Line |
|---|---|---|---|---|
| `#updateAvailableBanner` (info) | `0.55rem 0.65rem` (8.8/10.4) | 14 px / 600 | text `#d9ecff`, bg `rgba(88,160,255,0.15)`, border `1px rgba(88,160,255,0.38)`; link `#9cc6ff`, underlined, `margin-left 8px` | 71-74 |
| `#oscConnectingHint` (danger, soft) | `0.55rem 0.65rem` | 14 px / 600 | text `#ff6b6b`, bg `rgba(255,107,107,0.10)`, border `1px rgba(255,107,107,0.42)` | 79 |
| `#bridgeErrorBanner` (error, solid) | `0.6rem 0.7rem` (9.6/11.2) | 14 px / **700** | text `#fff`, bg `rgba(220,38,38,0.92)`, border `1px #ff8080` | 80-83 |
| `#foreignRendererBanner` (warning, solid) | `0.6rem 0.7rem` | 14 px / **700** | text `#1a1200`, bg `rgba(245,158,11,0.92)`, border `1px #ffd27f` | 84-87 |

The two solid banners have a **detail line**: `font-weight: 500; font-size: 12px;
opacity: 0.95; word-break: break-word; white-space: pre-wrap;` monospace, under a
title with `margin-bottom: 0.25rem`.

**Status dot** — `.osc-status-dot` (app.css:1271-1279): `display:inline-block;
width: 7px; height: 7px; border-radius: 50%; margin-right: 0.3rem (4.8px);
flex-shrink: 0`. Default fill `#52e2a2`; recoloured by `controls/osc.js:149-157`:

| `app.oscStatusState` | Colour |
|---|---|
| `initializing` | `#89a3ff` |
| `connected` | `#52e2a2` |
| `reconnecting` | `#ffb347` |
| `error` | `#ff5d5d` |
| (anything else) | `#7f8a99` |

`#status` (the text next to it) is `font-weight: 700` (app.css:854).

`.vbap-status` (app.css:2612-2621): `margin-top: 0.25rem; font-size: 12px;
color: #d9ecff`; `.computing → #ffbf66`; `.ready → #78e08f`.

`.mpv-orender-note` (app.css:971-981): 12 px, `line-height 1.45`,
`margin: -0.1rem 0 0.35rem`, `rgba(217,236,255,0.62)`, `overflow-wrap: anywhere`;
`.is-conflict → #ffbf66`.

### 2.12 Tables, editors, forms

`.editor-body` (app.css:2504-2507): `display: grid; gap: 0.35rem (5.6px)`.
`.editor-meta` (2509-2514): grid, gap `0.2rem (3.2px)`, 12 px, `#d9ecff`.
`.editor-row` (2516-2521): grid `1fr auto`, gap 6.4 px, centred; `label` 12 px `#d9ecff`.
`.gain-row` (2523-2525) overrides the columns to `auto 1fr auto`.

`.coord-table-row` (app.css:2549-2558): `flex; align-items: center; gap: 0.4rem`;
the table takes `flex: 1 1 auto; min-width: 0`, the trailing `.toggle-btn`
("3D Edit") is `flex: 0 0 auto`.

`.cart-coord-table` (app.css:2560-2565): `display: grid;
grid-template-columns: auto repeat(3, minmax(0,1fr)); gap: 0.2rem 0.35rem
(3.2 / 5.6 px); align-items: center`. Cells:
- `.cart-coord-corner` — an empty spacer (`aria-hidden`), top-left.
- `.cart-coord-head` — 11 px, `#9fb6cf`, centred. Labels: `X` `Y` `Z` (cartesian) or
  `Az°` `El°` `Dist` (polar).
- `.cart-coord-rowlabel` — 11 px, `#d9ecff`, `white-space: nowrap`. Two rows:
  "Norm." (`speaker.normalizedCoords`) and "Real (m)" (`speaker.metersCoords`).
- inputs — see §2.7.
In the polar table the "Real (m)" row has two `aria-hidden` empty spans before the
distance field (only distance has a metres equivalent).

`.coord-mode-row` (app.css:2590-2595): grid `auto 1fr auto`, gap `0.45rem (7.2px)`.
`.coord-mode-label` (2597-2603): `inline-flex; gap: 0.35rem; font-size: 12px;
color: #d9ecff` — wraps a native `radio` plus a `<strong>` label
("Cartesian:" / "Polar:"). The radios are **not restyled** — native platform radios.

`.coord-grid` (app.css:2527-2531): `repeat(3, minmax(0,1fr))`, gap 5.6 px.

`.room-geometry-form` — see §1.3 for the open/closed mechanics. Its body
(index.html:208-243) is a `1fr 1fr 1fr` grid, gap `0.4rem 0.8rem` (6.4 / 12.8 px),
`align-items: start`; three column headers `X` `Y` `Z` at 12 px / 600 / `opacity 0.8`,
right-aligned; each column is a sub-grid (gap `0.3rem`) of
`label (10 px, opacity 0.7, right) + .delay-input (100 % wide, right-aligned)`.
The Y column carries `front` + `rear`, Z carries `height` + `lower`, X carries
`width` + a `m/u` label + `#roomMpuValue` (11 px, `#d9ecff`, `opacity 0.85`,
right-aligned, padding `0.2rem 0`).
`#roomCenterBlendRow` is a flex row, gap `0.45rem`: label (12 px, nowrap, opacity 0.9)
| `.gain-slider` (flex 1) | value span (`min-width: 3.5rem = 56px`, right-aligned,
11 px, `#d9ecff`, `cursor: default`, double-click resets to `50/50`).

`.room-geometry-summary` (app.css:874-882, plus the denser
`#roomGeometryPanelRoot` variant at 884-891):
```
margin-top 0.3rem; background rgba(255,255,255,0.06);
border 1px rgba(255,255,255,0.14); border-radius 8px;
padding 0.35rem 0.45rem (5.6/7.2px); display grid; gap 0.22rem; font-size 11px
   dense variant: margin-top 3.5px, padding 4.5/6.1px, gap 2.6px,
                  font-size 10px, line-height 1.2
row: grid auto 1fr, gap 6.4px, centred
label: rgba(217,236,255,0.75), font-weight 600      value: #d9ecff
```

`.channel-direct-target` (app.css:418-436): grid `minmax(0,1fr) auto`,
gap `0.15rem 0.5rem`, `align-items: baseline`, `margin-bottom: 0.4rem`,
`padding: 0.35rem 0.45rem`, `border 1px rgba(255,255,255,0.12)`, radius 6 px,
`background rgba(0,0,0,0.22)`, 11 px. `strong` → `#d9ecff`, right-aligned;
`small` spans the full width, `#8fa3b7`, 10 px.

`.input-panel-*` family (app.css:1831-1924):
- `.input-panel-shell`: `margin-top 0.35rem; padding 0.45rem 0.5rem;
  border 1px rgba(255,255,255,0.08); radius 8px; background rgba(255,255,255,0.03);
  display grid; gap 0.4rem`
- `.input-panel-row`: grid `6.4rem (102.4px) | minmax(0,1fr)`, gap `0.45rem`, centred
- `.input-panel-row label`: 12 px, `rgba(217,236,255,0.92)`, wraps
  (`overflow-wrap: anywhere`)
- `.input-panel-grid` / `.input-panel-stack`: grid, gap `0.32rem (5.1px)`
- `.input-panel-inline-grid`: `repeat(2, minmax(0,1fr))`, gap `0.35rem`
- `.input-panel-triple-grid`: `repeat(3, minmax(0,1fr))`, gap `0.35rem`
- `.input-panel-field`: grid, gap `0.16rem (2.6px)`
- `.input-panel-subtitle`: 10 px, `0.08em`, uppercase, `#8fa6bd`, `margin-top 0.08rem`
- `.input-panel-status`: 11 px, `#b9c7d8`, `line-height 1.35`, `word-break: break-word`
- `.input-panel-actions`: flex, right-aligned, gap `0.35rem`

`.vbap-polar-grid` / `.vbap-grid-3` (app.css:1938-1980): `repeat(3, minmax(0,1fr))`,
**fixed `width: 13.1rem (209.6px)`**, `column-gap: 0.25rem`, `row-gap: 0.15rem`
(polar only, two rows). Their `.delay-input`s go to `width: 100%`.
`.vbap-polar-meta` 10 px `#9eb4c8`, centred, `line-height 1`.
`.vbap-step` 10 px `#86a7c3`, centred, `line-height 1`.
`.delay-label` 10 px `rgba(255,255,255,0.45)`, nowrap.

`.osc-config-row` (app.css:1218-1224): grid `6rem (96px) | 1fr`, gap `0.4rem`,
centred, 12 px. `.osc-config-actions`: flex, right-aligned, gap `0.4rem`,
`margin-top 0.2rem`.

**Renderer subpanels** — no CSS class; an inline recipe repeated ~10 times in
`ui/renderer-panel.js` (61, 138, 156, 233, 273, …):
```
shell (.info-section.renderer-subpanel):
    margin: 0; padding: 0.4rem 0.5rem (6.4/8px);
    border: 1px solid rgba(255,255,255,0.08); border-radius: 8px;
    background: rgba(255,255,255,0.03)
bar   (.renderer-subpanel-bar):
    flex, space-between, align-center, gap 0.4rem
    title: margin 0; font-size 12px; font-weight 600; color #ffffff
    actions (.renderer-subpanel-actions): flex, align-center, gap 0.35rem
body  (.renderer-subpanel-body):
    margin-top 0.25rem; padding 0.3rem 0.4rem (4.8/6.4px);
    background rgba(255,255,255,0.03); border-radius 6px; display grid; gap 0.3rem
```
The generated backend-param container reuses the body recipe plus
`margin-left: 1rem` and `font-size: 11px`, `gap: 0.18rem` (`vbap.js:583-593`).
Inside `.renderer-panel-root`, everything is forced to
`min-width: 0; max-width: 100%; box-sizing: border-box` and the bars/actions
`flex-wrap: wrap` (app.css:1400-1445) — the panel is expected to survive a 220 px
width.

`.adaptive-subpanel` (app.css:1143-1153): `display: flex; flex-direction: column;
gap: 0.25rem; padding: 0.45rem 0.5rem; background rgba(255,255,255,0.04);
border 1px rgba(255,255,255,0.10); border-radius 8px`.
`.adaptive-advanced-form`: column, gap `0.45rem`, `margin-top 0.35rem`.
`.telemetry-gauges-form` (1164-1179): gap `0.3rem`, padding `0.45rem 0.5rem`,
bg `rgba(255,255,255,0.05)`, border `1px rgba(255,255,255,0.12)`, radius 8 px.

### 2.13 DRC gauge

There is **no `.drc-gauge` class**. It is inline markup in the DRC panel header
(index.html:505-510) plus `controls/drc.js:110-135`:
```
#drcGaugeRow  flex:1 1 auto; margin: 0 0.4rem; align-items:center; gap:0.4rem;
              min-width:60px;  display:none unless app.oscMeteringEnabled
  track       flex:1; height:6px; background:#222; border-radius:3px; overflow:hidden
  #drcGaugeFill  absolutely positioned, RIGHT-anchored (right:0; top:0; bottom:0),
                 width: 0%..100%, transition: width 0.1s ease-out
  #drcGainValue  font-size 0.65rem (10.4px); color #888; nowrap; monospace
```
The fill grows **right-to-left** (it is anchored `right: 0`). Mapping
(`drc.js:117-135`):
```
gainDb   = linearToDb(gain)   (or -100 when not finite)
maxDelta = 20
percent  = min(100, |gainDb| / 20 * 100)          -> width, toFixed(1) + "%"
label    = (gainDb >= 0 ? "+" : "") + gainDb.toFixed(1) + " dB"
colour   = gainDb >  +1  -> #33b5e5   (boost)
           gainDb <  -12 -> #ff4444
           gainDb <   -6 -> #ffbb33
           else          -> #00c851
```
Note `#222` and `#888` are the only two greys in the whole UI that do not belong to
the palette — flag as an inconsistency; in egui use `section.card.bg2` for the track
and `text.dim` for the label.

### 2.14 Scene-effects bar widgets

`.scene-fx-btn` (app.css:2811-2843):
```
inline-flex centred; 40 × 36 px; padding 0; border 1px solid transparent;
border-radius 9px; background transparent; color rgba(223,232,243,0.55);
transition background/color/border-color/box-shadow 0.13s ease
hover:          bg rgba(255,255,255,0.07); color rgba(223,232,243,0.92)
focus-visible:  border rgba(82,226,162,0.50); no outline
.active:        bg rgba(82,226,162,0.16); border rgba(82,226,162,0.55);
                color #7af0c0; box-shadow inset 0 0 0 1px rgba(82,226,162,0.18)
.active:hover:  bg rgba(82,226,162,0.24); color #aef5d8
```
Icons are inline SVGs, `viewBox 0 0 24 24`, rendered **20 × 20**, `fill: none;
stroke: currentColor; stroke-width: 1.8; stroke-linecap/linejoin: round` — except the
heatmap icon (four filled rounded rects at opacities .95/.4/.55/.85) and the diffuse
trail icon (four filled circles at .4/.62/.82/1). Buttons in bar order
(`controls/scene-effects-bar.js:17-25`): Grid, Objects (flyout), Labels,
Trails (flyout), Object energy field, Speaker heatmap, mpv overlay.
Labels come from `data-i18n-title` → `sceneFx.grid`, `sceneFx.objects`,
`sceneFx.labels`, `sceneFx.trails`, `sceneFx.energyField`, `sceneFx.heatmap`,
`sceneFx.mpvOverlay` — **the i18n text lives in the `title` attribute (tooltip)**, per
the project convention `feedback_studio_scene_fx_icons`.

`.fx-caret` (app.css:3009-3029): absolutely positioned at `top: 2px; right: 3px`,
**12 × 10 px**, radius 3 px, colour `rgba(223,232,243,0.38)`; the parent button's
hover lifts it to `0.75`; its own hover gives it `background rgba(255,255,255,0.14)`
and `#ffffff`; on an `.active` button it is `rgba(122,240,192,0.8)`. Its glyph is a
`12 × 8` chevron-up, rendered 10 × 7, `stroke-width 2`.
`.scene-fx-slot` (app.css:2999-3002) is a `position: relative; inline-flex` wrapper so
the popup anchors to the button without affecting bar layout.

`.scene-fx-flyout` (app.css:3031-3057): absolutely positioned
`bottom: calc(100% + 9px); left: 50%; translateX(-50%)`; column, `gap: 2px`,
`padding: 5px`, bg `rgba(18,22,28,0.92)`, border `1px rgba(255,255,255,0.10)`,
radius 11 px, shadow `0 8px 28px rgba(0,0,0,0.5)`, blur 12 px, `z-index: 80`.
A `::after` pointer tail: a 6 px transparent border box with
`border-top-color: rgba(18,22,28,0.92)`, centred under the menu.

`.fx-flyout-item` (app.css:3070-3094): **40 × 34 px**, radius 8 px, transparent border
and background, colour `rgba(223,232,243,0.6)`; hover
`bg rgba(255,255,255,0.08); color #ffffff`; `.selected`
`bg rgba(82,226,162,0.16); border rgba(82,226,162,0.5); color #aef5d8`.

Behaviour worth preserving (`scene-effects-bar.js`): a left-click on the caret opens
the menu, a left-click elsewhere toggles the effect, a **right-click anywhere on the
button** is an alias for opening the menu, Escape and any outside click dismiss it,
and picking a nature **implicitly enables the layer** if it was off (lines 88-99).

`.object-test-transport` (app.css:2915-2931): flex row, gap 8 px,
`margin: 0.15rem 0 0.1rem`. `.transport-label` 12 px `rgba(223,232,243,0.72)`,
ellipsised nowrap. `.transport-btn` — §2.5. The play/pause glyphs are two SVGs
toggled by `aria-pressed` (app.css:2995-2997).

---

## 3. Animations and transitions

| Animation | Duration / easing | Keep? |
|---|---|---|
| `.panel-resize-handle::before` background + height (36 → 56 px) | .12s ease | **Keep** — the only affordance that the edge is draggable. |
| `.panel-side-collapse-btn` bg + border | .12s ease | Keep (cheap hover feedback). |
| `.panel-toggle-btn` / `.info-icon-btn` / `select` hover | .12s ease | Keep — egui gives this for free via `Visuals::widgets::hovered`. |
| Switch thumb `transform` (16 px slide) | .2s ease | **Keep** — reads as an on/off gesture. Easy in egui with an `AnimationManager` lerp. |
| `.axis-driver` thumb + border | .12s ease | Keep. |
| `.clip-indicator` bg + shadow | .15s ease | Keep. |
| `.meter-peak` `clip-path` + `opacity` | .1s linear / .2s ease | **Keep the opacity fade**; drop the clip-path transition — in egui the meter is repainted every frame anyway and a 100 ms lag on the peak cursor is a *feature* only because CSS repaints are coarse. Repaint directly. |
| `.object-size-fill` width | 80 ms ease-out | Optional; direct is fine. |
| `.id-strip` bg/shadow/border | 120 ms ease | Keep. |
| `speaker-clip-flash` keyframes (red → transparent) | 1 s ease-out | **Keep** — it is the only clipping notification on the row. |
| `transport-pulse` keyframes (expanding ring) | 2 s ease-in-out infinite | **Keep** — deliberately loud ("a signal that can be left on by accident"). Honour reduced-motion. |
| `.band-cursor-seg` opacity + `scaleX(1.18)` + shadow | .15s ease | Keep the opacity/glow; `scaleX` is optional. |
| `.band-cursor-seg::after` chip opacity | .15s ease | Keep. |
| `.scene-fx-btn` / `.fx-flyout-item` / `.transport-btn` | .12–.13s ease | Keep. |
| Speaker-list reorder FLIP (`speakers.js:2113-2122`) | 120 ms `cubic-bezier(0.2,0.8,0.2,1)`, `translateY` | **Keep if drag-reorder is ported**; otherwise drop. |
| Accordion expand/collapse (`max-height` + `opacity`) | **none — there is no transition on these properties** | Nothing to port. Sections snap open/closed. Adding an egui height animation would be a *change*, not parity; if you want it, keep it under 120 ms. |
| Modal open/close | none | Nothing to port. |
| `body.overlay-layout-resizing` | disables **all** overlay transitions and blurs while dragging | In egui: skip any easing while a resize drag is active. |

Droppable outright: the `-webkit-backdrop-filter` duplicates, the
`user-select`/`pointer-events` WebKitGTK guards, `scrollbar-gutter`, and every
`display:none` avoidance hack (§1.3).

---

## 4. Responsive and high-DPI

- **There are no width breakpoints.** The only `@media` query in the sheet is
  `prefers-reduced-motion: reduce` (app.css:2991). Narrow-window behaviour comes
  entirely from:
  - `--panel-width-*: min(440px, 92vw)` — the pre-JS default only.
  - `clampWidth()` (§1.1): each panel is clamped so the pair cannot overlap; the
    floor is 220 px, so on a window narrower than ~2×(220+34)+36 ≈ 544 px both panels
    sit at 220 px and *will* overlap the safety gap. No further handling exists.
  - `.info-modal-card { width: min(460px, calc(100vw - 2rem)) }`.
  - Everywhere a grid could overflow, `minmax(0, 1fr)` + `min-width: 0` +
    `overflow-wrap: anywhere` is used instead of a breakpoint
    (app.css:987-994, 1400-1445, 1918-1924).
  - `.renderer-panel-root` bars use `flex-wrap: wrap` so action rows reflow at
    narrow widths (app.css:1429-1434).
  - Fixed-width islands that will **not** shrink and must be watched at 220 px:
    `.vbap-polar-grid` / `.vbap-grid-3` (209.6 px), `.log-panel-filter` (min 160 px),
    `.fixed-metric` (8ch), `.object-topright` (95 px), `.band-label` (52 px),
    `.gain-box` (54 px), `.delay-input` (52 px), `.save-footer-button` (136 px).
- **Height** is handled with `vh`-based caps (§1.3): 44 vh / 52 vh / 45 vh / 28 vh.
  Port these as fractions of the available window height, not as constants.
- **High-DPI.** The web UI has no DPI-specific code: CSS px are logical px and the
  browser scales. In egui, everything above is in **points**; set
  `ctx.set_pixels_per_point()` from the window scale factor and all the values
  transcribe 1:1. Two things need care:
  - 1 px hairlines (borders, `.meter-marker`, `.object-size-bar` at 2 px,
    `.band-bar` at 6 px, `.object-size-bar` at 2 px) will land on half-pixels at
    fractional scale factors. Round stroke rects to the physical pixel grid
    (`painter.rect_stroke` with a 0.5-point offset) or accept the blur.
  - The 7 px `.filter-freq` and 9 px `.object-coords` text is at the edge of
    legibility; do **not** let it round down further after DPI scaling.
- `color-scheme: dark` on `:root` (app.css:2) is what makes the unstyled native
  controls (range sliders, radio buttons, `select` popup lists, scrollbars) render
  dark. In egui they are all custom-painted, so this disappears.

---

## 5. i18n application rules

Source: `src/i18n.js` (164 lines), `src/i18n/*.json`, `src/controls/inline-help.js`.

### 5.1 Catalogue and key resolution

- Locales: **`en, fr, de, ja, es, it, pt-BR, zh-CN`** (8), each a **flat**
  `{ "key.path": "text" }` JSON object. `en.json` has **940 keys**.
- Non-English catalogues are pre-merged at module load
  (`i18n.js:16-26`): `fr: { ...en, ...fr }` — so a missing translation is already
  the English string before lookup.
- `t(key)` (line 106): `TRANSLATIONS[locale]?.[key] ?? TRANSLATIONS.en[key] ?? key`.
  **A missing key returns the key itself** — never throws, never blanks.
- Dot-separated key names are **not** a nested structure; they are opaque strings.
  In Rust, a `HashMap<&'static str, &'static str>` per locale (or a `phf` map) is a
  faithful port. Consider a build-time `include_str!` + `serde_json` parse into a
  `OnceLock<HashMap>`; there is no hot-path cost since lookups happen per frame only
  for visible labels — cache resolved `&str` in widget structs if it shows up in a
  profile.

### 5.2 Placeholder substitution

`tf(key, values)` (i18n.js:110-117):
```js
t(key).replace(/\{(\w+)\}/g, (_, name) =>
    values[name] === undefined || values[name] === null ? '' : String(values[name]))
```
- Syntax is **`{name}`** with `\w+` only (ASCII letters, digits, underscore).
- An unmatched/undefined placeholder becomes the **empty string**, not the literal.
- No escaping, no formatting specifiers, no nesting.
- The 37 placeholder names in use across `en.json`: `active, bands, clock, count,
  ctrl, device, effective, enabled, error, expected, format, gain, index, iter, keep,
  kp, latency, line, low, max, min, mode, name, path, pipe, rate, raw, requested,
  running, sec, source, status, sync, taps, target, text, value, version`.

### 5.3 Plurals

**There is none.** No plural keys exist (grep for `plural|_one|_other|\.one$|\.other$`
across all 940 keys returns nothing), and `tf` has no plural machinery. Counts are
interpolated as `{count}` into a single invariant string
(e.g. `log.copySuccess`). Port as-is; do not introduce ICU plurals or the
translations will not match.

### 5.4 DOM attribute application → egui equivalents

`applyStaticTranslations()` (i18n.js:125-164) walks the document once per locale
change and applies five attributes:

| Attribute | Effect | egui equivalent |
|---|---|---|
| `data-i18n` | `el.textContent = t(key)` | label text |
| `data-i18n-title` | `el.setAttribute('title', t(key))` | `.on_hover_text(t(key))` |
| `data-i18n-html` | `el.innerHTML = t(key)` | **see below** |
| `data-i18n-placeholder` | input placeholder | `TextEdit::hint_text` |
| `data-i18n-aria-label` | accessibility label | `Response::widget_info` / AccessKit label |

It also sets `document.documentElement.lang = locale` and repopulates the locale
`<select>` labels as `` `${english} / ${native}` `` (or just `english` when they are
equal — i.e. `Auto` and `English`), then invokes the log-panel re-render callbacks.
In an immediate-mode UI none of this walking is needed: call `t()` at draw time.

**`data-i18n-html` — the 17 keys and their markup** (all in `en.json`; the
translations use the same tags):

| Key | Tags used | Length (en) |
|---|---|---|
| `adaptive.infoBody` | `<strong>`, `<br>` | 1809 |
| `heatmap.infoBody` | `<strong>`, `<br>` | 1012 |
| `rampMode.infoBody` | `<strong>`, `<br />` | 753 |
| `input.clockInfoBody` | `<strong>`, `<br>` | 595 |
| `telemetry.infoBody` | `<strong>`, `<br>` | 567 |
| `spread.distanceInfoBody` | `<strong>`, `<br>` | 560 |
| `room.infoBody` | `<strong>`, `<br>` | 421 |
| `drc.infoBody` | `<strong>`, `<br>` | 411 |
| `input.lfeInfoBody` | `<strong>`, `<br>` | 384 |
| `evaluation.infoBody` | `<strong>`, `<br>` | 383 |
| `distance.infoBody` | `<strong>`, `<br>` | 365 |
| `distance.modelInfoBody` | `<strong>`, `<br>` | 363 |
| `effectiveRender.infoBody` | `<br>` only | 350 |
| `input.infoBody` | `<strong>`, `<br>` | 313 |
| `status.connectingHint` | `<strong>`, **`<a href>`** | 304 |
| `osc.infoBody` | `<strong>`, `<br>` | 290 |
| `trail.infoBody` | `<strong>`, `<br>` | 238 |

Plus `backend.infoBody` (339 chars, **no tags**) which is composed at runtime with
`<br /><br /><strong>{label}</strong><br />{specific}` (`modals.js:143-146`).

So the **entire HTML vocabulary is `<strong>`, `<br>` / `<br />`, and one `<a href>`**:
```html
<a href="https://github.com/mgth/mpv-omniphony/releases" target="_blank"
   rel="noreferrer" style="color:inherit;text-decoration:underline">
```
(inside `status.connectingHint`).

**Recommended egui port:** write a tiny renderer that splits on `<br>`/`<br />` into
paragraphs and builds a `LayoutJob` where text between `<strong>` and `</strong>` is
`FontId` bold (or the same size with `Color32::WHITE` if you ship no bold face), and
the single `<a>` becomes a `Hyperlink`. Do **not** pull in an HTML engine, and do not
strip the tags — the bold runs carry meaning (they label the paragraphs).

### 5.5 Help keys

- `data-help-i18n="<key>"` marks a parameter *name* as an inline-help trigger
  (`inline-help.js:88-101`). `data-help-anchor="<css selector>"` optionally moves the
  panel below a whole group instead of into a dense grid cell.
- **170 `help.*` keys** exist in `en.json`.
- A key whose translation is missing (`t(key) === key`) is **silently skipped** —
  no trigger is created and no raw key is shown (line 92).
- Backend-specific modal blurbs use `help.backend.<backendId>` and follow the same
  "skip when `t(key) === key`" rule (`modals.js:141-147`).
- Generated backend params carry their label/help through
  `localizeParamLabel` / `localizeParamHelp` (`vbap.js:445-449`), and the whole
  generated container is **rebuilt on a locale change** because those strings are
  resolved at build time (`vbap.js:619`, keyed on `container.dataset.locale`). In
  egui this is moot — resolve every frame.

### 5.6 Locale choice and persistence

```
LOCALE_STORAGE_KEY = 'spatialviz.locale'          (i18n.js:14)
stored value ∈ { 'auto', 'en', 'fr', 'de', 'ja', 'es', 'it', 'pt-BR', 'zh-CN' }
normalizeLocalePreference(v): 'auto' passes through; otherwise
    v ∈ {fr,de,ja,es,it,pt-BR,zh-CN} ? v : 'en'          (lines 48-55)
detectLocale():  stored ? (stored === 'auto' ? detectSystemLocale() : stored)
                        : detectSystemLocale()            (lines 76-83)
```
`detectSystemLocale()` (lines 57-74) walks `navigator.languages` (falling back to
`[navigator.language]`), lowercases each candidate and returns the **first** match,
tested in this exact order:
`fr* → de* → ja* → es* → it* → pt-br* → zh-cn* → en*`, default **`en`**.
(Note the ordering means a candidate list like `["pt-PT","fr"]` resolves to `fr`,
because `pt-PT` matches nothing — `pt` alone is not accepted, only `pt-br`.)

`setLocale(newLocale)` (lines 87-99) persists the **preference** (so `'auto'` is
stored as `'auto'`, not as the resolved locale), resolves `i18nState.locale`,
re-applies translations, then notifies every `onLocaleChange` listener
(errors in a listener are caught and logged, never propagated).

**Rust/egui port:** store the preference string in the same key so an existing
install keeps its choice; resolve `auto` from `std::env` (`LC_ALL` → `LC_MESSAGES` →
`LANG`) or `sys-locale`, applying the same prefix-match order. Keep `pt-BR` and
`zh-CN` case-sensitive as stored values.

**Locale-dependent formatting elsewhere:** the log timestamp uses
`Intl.DateTimeFormat(locale, { hour: '2-digit', minute: '2-digit', second: '2-digit' })`
(`log.js:26-33`). Everything else — dB values, coordinates, percentages — is
formatted with plain `toFixed()` and is therefore **always dot-decimal, locale
independent**. Preserve that: do not localise numbers.

---

## 6. Out-of-scope entry points (present as chrome, deferred as features)

These have visual entry points in the panels and must be drawn, even if the feature
behind them lands later:

| Entry point | Where | Look |
|---|---|---|
| SOFA browser | `#sofaBrowseBtn` `.toggle-btn` in the binaural HRTF subpanel bar; opens `#sofaBrowserModal` (`.info-modal-card` widened to `max-width:560px; width:90vw`, index.html:1011-1033) | standard toggle-btn + wide modal |
| Backend script editor | `.mini-btn` "Edit" beside a `file`-kind backend param (`vbap.js:520-533`) — **unstyled**, treat as `.ui-btn-compact` | |
| Auto-tune PI wizard | `#autoTunePiWizardModal` — an empty `.info-modal` div filled by its module (index.html:1152) | |
| Diagnostics plot | `#diagSection` with `select.micro-select` (10 px) controls | |
| Resample plot | `.meter-bar.resample-meter-shell` + `.resample-meter-center` | |
| Gradient / heatmap editor | driven from `#heatmapBandSelect` + `#bandCursor` | |
| Presets / import / export layout | three `.ui-btn.ui-btn-primary` in the Speakers header (index.html:793-796) | |

---

## 7. Known inconsistencies to fix (or deliberately reproduce)

1. `.mini-btn` has **no stylesheet rule** — three call sites render a raw platform
   button. Style it as `.ui-btn-compact`.
2. `.form-select` has no rule (inert class on `#profileSelect`, `#binauralHrirSource`).
3. `.option-status-note` has no rule; every site re-declares 10 px + an opacity.
4. `.drc-gauge` colours `#222` / `#888` are outside the palette (§2.13).
5. The side-panel collapse buttons' tooltips are **hard-coded English**
   (`side-panels.js:132`) — no `data-i18n-title`.
6. `.adaptive-ratio-reset-btn { display: none }` (app.css:511) — a dead control.
7. `<html lang="fr">` in `index.html:2` is a leftover; `applyStaticTranslations`
   overwrites it at runtime.
8. `.info-modal` is toggled with `display: none ↔ flex`, which contradicts the
   "never toggle display over the canvas" rule stated at app.css:762-767 (modals are
   full-screen overlays, so in practice they are the exception).
9. Banners are four ad-hoc inline styles; unify them into one `Banner{severity}`
   widget in egui (§2.11 has the four token sets).
