//! Studio look, ported from `omniphony-studio/src/styles/app.css`.
//!
//! egui has no cascade, so the CSS variables and the recurring rules become
//! named constants here and one `Style` the app installs at startup. Hex
//! values are the stylesheet's; alpha-on-dark rules (`rgba(255,255,255,.12)`
//! and friends) are kept as alpha so the same colour works over the panel and
//! over the viewport.

use egui::{Color32, CornerRadius, Margin, Stroke};

/// `body { background: #0a0b10 }` — behind the viewport, never painted over it.
pub const PAGE_BG: Color32 = Color32::from_rgb(0x0a, 0x0b, 0x10);
/// `#overlay { background: rgba(0,0,0,.65) }` plus an 8 px backdrop blur egui
/// cannot do; the fill is darkened to keep text contrast without it.
pub const PANEL_BG: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 199);
/// `border: 1px solid rgba(255,255,255,.2)`.
pub const PANEL_BORDER: Color32 = Color32::from_rgba_premultiplied(255, 255, 255, 51);
/// `border-top: 1px solid rgba(255,255,255,.12)` between sections.
pub const SECTION_RULE: Color32 = Color32::from_rgba_premultiplied(255, 255, 255, 31);

/// Body text (`color: #d9ecff`).
pub const TEXT: Color32 = Color32::from_rgb(0xd9, 0xec, 0xff);
/// Brighter text for titles and values (`#edf5ff`).
pub const TEXT_STRONG: Color32 = Color32::from_rgb(0xed, 0xf5, 0xff);
/// Secondary text (`#9eb4c8`), used by summaries and notes.
pub const TEXT_MUTED: Color32 = Color32::from_rgb(0x9e, 0xb4, 0xc8);
/// `.object-coords`, `.band-label` (`#8a9aac`).
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x8a, 0x9a, 0xac);
/// Disabled/very quiet text (`#8fa3b7`).
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x8f, 0xa3, 0xb7);

/// Interactive accent (`rgba(124,231,255,…)`, the focus and hover colour).
pub const ACCENT: Color32 = Color32::from_rgb(0x78, 0xc8, 0xff);
/// "On"/ok green (`#5cff9a`, `rgba(82,226,162,…)`).
pub const OK: Color32 = Color32::from_rgb(0x52, 0xe2, 0xa2);
/// Brighter green for markers (`#5cff9a`).
pub const OK_BRIGHT: Color32 = Color32::from_rgb(0x5c, 0xff, 0x9a);
/// Warning amber (`rgba(255,213,106,…)`, `rgba(255,160,90,…)`).
pub const WARN: Color32 = Color32::from_rgb(0xff, 0xb3, 0x47);
/// Error red (`#ff7d7d`, `rgba(255,59,48,.9)`).
pub const ERROR: Color32 = Color32::from_rgb(0xff, 0x5d, 0x5d);
/// Clip indicator (`#ff3b30`).
pub const CLIP: Color32 = Color32::from_rgb(0xff, 0x3b, 0x30);
/// "Initializing" status dot (`#89a3ff`).
pub const INFO: Color32 = Color32::from_rgb(0x89, 0xa3, 0xff);

/// Control fills: rest, hover, active (`rgba(255,255,255,.08/.12/.18)`).
pub const FILL: Color32 = Color32::from_rgba_premultiplied(255, 255, 255, 20);
pub const FILL_HOVER: Color32 = Color32::from_rgba_premultiplied(255, 255, 255, 31);
pub const FILL_ACTIVE: Color32 = Color32::from_rgba_premultiplied(255, 255, 255, 46);
/// Control outline (`rgba(255,255,255,.2)`).
pub const CONTROL_BORDER: Color32 = Color32::from_rgba_premultiplied(255, 255, 255, 51);

/// The stylesheet's workhorse size: selects, buttons, editor labels, list
/// rows. `#overlay` itself is 14 px, but almost every control inside is 12.
pub const FONT_SIZE: f32 = 12.0;
/// `.panel-summary`, `.option-status-note`, `.meter-subvalues`: 10 px.
pub const FONT_SIZE_SMALL: f32 = 10.0;
/// `.app-brand-title`: 16 px. Weight 600 in CSS; egui has one weight per
/// family, so titles are set apart by size and colour instead.
pub const FONT_SIZE_TITLE: f32 = 16.0;
/// `#overlay .panel-title`: 11 px, the section headers of the left overlay.
pub const FONT_SIZE_SECTION: f32 = 11.0;

/// `#overlay { border-radius: 12px }`.
pub const PANEL_RADIUS: u8 = 12;
/// Controls and chips.
pub const CONTROL_RADIUS: u8 = 6;

/// `#overlay { padding: .75rem 1rem }` at a 16 px root.
pub const PANEL_PADDING_X: f32 = 16.0;
pub const PANEL_PADDING_Y: f32 = 12.0;
/// `top/left: 1rem`.
pub const PANEL_EDGE_MARGIN: f32 = 16.0;
/// Row gap inside a section (`.conditional-params.open { gap: .2rem }`).
pub const ROW_GAP: f32 = 3.0;
/// Gap between the overlay's direct children (`gap: .4rem`).
pub const PANEL_GAP: f32 = 6.0;

/// Frame of one floating overlay.
pub fn panel_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(PANEL_BG)
        .stroke(Stroke::new(1.0, PANEL_BORDER))
        .corner_radius(CornerRadius::same(PANEL_RADIUS))
        .inner_margin(Margin::symmetric(
            PANEL_PADDING_X as i8,
            PANEL_PADDING_Y as i8,
        ))
}

/// Install the Studio style: dark visuals, the stylesheet's text colours, and
/// text styles at the CSS sizes.
pub fn install(ctx: &egui::Context) {
    use egui::{FontFamily::Proportional, FontId, TextStyle};
    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    style.visuals = egui::Visuals::dark();
    let v = &mut style.visuals;
    v.panel_fill = PANEL_BG;
    v.window_fill = PANEL_BG;
    v.extreme_bg_color = Color32::from_rgba_premultiplied(255, 255, 255, 10);
    v.faint_bg_color = Color32::from_rgba_premultiplied(255, 255, 255, 10);
    v.override_text_color = Some(TEXT);
    v.hyperlink_color = ACCENT;
    v.selection.bg_fill = Color32::from_rgba_unmultiplied(0x7c, 0xe7, 0xff, 60);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.widgets.noninteractive.bg_fill = FILL;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, SECTION_RULE);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.inactive.bg_fill = FILL;
    v.widgets.inactive.weak_bg_fill = FILL;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, CONTROL_BORDER);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.hovered.bg_fill = FILL_HOVER;
    v.widgets.hovered.weak_bg_fill = FILL_HOVER;
    v.widgets.hovered.bg_stroke =
        Stroke::new(1.0, Color32::from_rgba_premultiplied(255, 255, 255, 72));
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT_STRONG);
    v.widgets.active.bg_fill = FILL_ACTIVE;
    v.widgets.active.weak_bg_fill = FILL_ACTIVE;
    v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    v.widgets.active.fg_stroke = Stroke::new(1.0, TEXT_STRONG);
    v.widgets.open.bg_fill = FILL_HOVER;
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = CornerRadius::same(CONTROL_RADIUS);
    }
    style.text_styles = [
        (TextStyle::Small, FontId::new(FONT_SIZE_SMALL, Proportional)),
        (TextStyle::Body, FontId::new(FONT_SIZE, Proportional)),
        (TextStyle::Button, FontId::new(FONT_SIZE, Proportional)),
        (
            TextStyle::Heading,
            FontId::new(FONT_SIZE_TITLE, Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(FONT_SIZE_SMALL, egui::FontFamily::Monospace),
        ),
    ]
    .into();
    // A standard control is 20 px tall (a 12 px input with 2 px padding and a
    // 1 px border, app.css:1757).
    style.spacing.item_spacing = egui::vec2(6.0, ROW_GAP);
    style.spacing.interact_size = egui::vec2(0.0, 20.0);
    style.spacing.slider_width = 120.0;
    style.spacing.button_padding = egui::vec2(7.0, 2.5);
    style.spacing.window_margin = Margin {
        left: PANEL_PADDING_X as i8,
        right: PANEL_PADDING_X as i8,
        top: PANEL_PADDING_Y as i8,
        bottom: PANEL_PADDING_Y as i8,
    };
    style.spacing.indent = 16.0;
    let style = std::sync::Arc::new(style);
    ctx.set_style_of(egui::Theme::Dark, style.clone());
    // Studio is dark-only (`color-scheme: dark`), so a viewer whose system
    // prefers light gets the same look rather than an unstyled one.
    ctx.set_style_of(egui::Theme::Light, style);
    ctx.set_theme(egui::Theme::Dark);
}
