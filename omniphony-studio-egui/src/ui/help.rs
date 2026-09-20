//! Help, one at a time (`controls/inline-help.js`, `modals.js`).
//!
//! Two kinds. A parameter's help opens in place, in a small framed card under
//! its row, and the thing you click is the name of what you are asking about,
//! marked by a dotted underline: no "?" beside every label. A panel's help —
//! what a whole section is for — opens centred over everything, in the help
//! overlay, from a small "i" beside the section's title that only shows while
//! the pointer is over the header (always, on a touch screen): a title is for
//! opening the section, so it cannot also be the help's trigger.
//!
//! Only one help is ever open. Opening a card closes the one before it,
//! opening the overlay closes any card, and a click anywhere but on the open
//! card or a help trigger closes it, as the web's outside-click listener does.

use egui::{Color32, Id, Response, Sense, Ui};

use super::theme;
use crate::i18n::t;

/// The one open card, by its help's id.
fn open_id() -> Id {
    Id::new("inline-help-open")
}

/// Set for the frame when a click landed on a trigger or on the open card, so
/// the end-of-frame check leaves the card alone.
fn claimed_id() -> Id {
    Id::new("inline-help-claimed")
}

/// Where a trigger leaves the overlay it wants opened; the app shows it.
pub fn overlay_request_id() -> Id {
    Id::new("help-overlay-request")
}

/// A panel-level help: the overlay's title and its body. Bodies may carry the
/// `*.infoBody` strings' small markup (`<br>`, `<b>`, `<code>`).
#[derive(Clone, Debug, PartialEq)]
pub struct Overlay {
    pub title: String,
    pub body: String,
}

impl Overlay {
    /// A `<prefix>.infoTitle` / `<prefix>.infoBody` pair.
    pub fn info(prefix: &str) -> Self {
        Self {
            title: t(&format!("{prefix}.infoTitle")).to_owned(),
            body: t(&format!("{prefix}.infoBody")).to_owned(),
        }
    }

    /// A title key and a body key that do not share a prefix
    /// (`input.clockInfoTitle` / `input.clockInfoBody`).
    pub fn keys(title_key: &str, body_key: &str) -> Self {
        Self {
            title: t(title_key).to_owned(),
            body: t(body_key).to_owned(),
        }
    }

    /// A section's own title over a `help.*` string.
    pub fn titled(title: impl Into<String>, body_key: &str) -> Self {
        Self {
            title: title.into(),
            body: t(body_key).to_owned(),
        }
    }
}

/// A parameter's help: who it belongs to and what it says.
#[derive(Clone, Copy)]
pub struct Help<'a> {
    id: Id,
    text: &'a str,
}

impl<'a> Help<'a> {
    /// Help whose text comes from somewhere other than a `help.*` key — a
    /// backend's own parameter description, say. `source` identifies it; two
    /// rows sharing one would open together.
    pub fn text(source: impl std::hash::Hash + std::fmt::Debug, text: &'a str) -> Self {
        Self {
            id: Id::new(("inline-help", source)),
            text,
        }
    }

    /// Nothing to say: no translation, or an empty description. The row then
    /// shows a plain label rather than a trigger that opens an empty card.
    fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }
}

impl From<&str> for Help<'static> {
    /// A `help.*` key. A key with no translation yields no help, so the raw key
    /// never reaches the screen.
    fn from(key: &str) -> Self {
        Help {
            id: Id::new(("inline-help", key)),
            text: crate::i18n::lookup(key).unwrap_or(""),
        }
    }
}

fn is_open(ctx: &egui::Context, id: Id) -> bool {
    ctx.data(|d| d.get_temp::<Id>(open_id())) == Some(id)
}

fn claim(ctx: &egui::Context) {
    ctx.data_mut(|d| d.insert_temp(claimed_id(), true));
}

/// Close whatever card is open.
pub fn close(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<Id>(open_id()));
}

/// Ask for the centred overlay. Any open card closes: one help at a time.
pub fn open_overlay(ctx: &egui::Context, overlay: Overlay) {
    close(ctx);
    ctx.data_mut(|d| d.insert_temp(overlay_request_id(), overlay));
}

/// The trigger's look: a pointer, and a dotted underline under `text_rect`
/// that turns to the accent under the pointer and stays so while its help is
/// open. Returns whether the trigger was clicked.
pub fn decorate(ui: &Ui, text_rect: egui::Rect, response: &Response, active: bool) -> bool {
    let hovered = response.hovered();
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let y = text_rect.bottom() + 0.5;
    let from = egui::pos2(text_rect.left(), y);
    let to = egui::pos2(text_rect.right(), y);
    let painter = ui.painter().with_clip_rect(ui.clip_rect());
    if active {
        painter.line_segment([from, to], egui::Stroke::new(1.0, theme::ACCENT));
    } else {
        let colour = if hovered {
            theme::ACCENT
        } else {
            // `underline dotted rgba(217,236,255,.4)`.
            Color32::from_rgba_unmultiplied(217, 236, 255, 102)
        };
        painter.extend(egui::Shape::dotted_line(&[from, to], colour, 3.0, 0.6));
    }
    let clicked = response.clicked();
    if clicked {
        claim(ui.ctx());
    }
    clicked
}

/// Make `text_rect` — a label already painted — the trigger of `help`'s card.
pub fn trigger(ui: &Ui, text_rect: egui::Rect, help: Help<'_>) {
    if help.is_empty() {
        return;
    }
    let response = ui.interact(text_rect, help.id.with("trigger"), Sense::click());
    let open = is_open(ui.ctx(), help.id);
    if decorate(ui, text_rect, &response, open) {
        ui.ctx().data_mut(|d| {
            if open {
                d.remove::<Id>(open_id());
            } else {
                d.insert_temp(open_id(), help.id);
            }
        });
    }
}

/// What a row's label opens.
#[derive(Clone, Copy)]
pub enum Trigger<'a> {
    /// A card under the row: the help of one parameter.
    Card(Help<'a>),
    /// The centred overlay, for a `<prefix>.infoTitle` / `.infoBody` pair: the
    /// label heads a whole block, and its help is about the block.
    Info(&'a str),
    /// The centred overlay, for a title key and a body key.
    InfoKeys(&'a str, &'a str),
}

/// Make `text_rect` — a label already painted — open what `what` names.
pub fn trigger_any(ui: &Ui, text_rect: egui::Rect, what: Trigger<'_>) {
    match what {
        Trigger::Card(help) => trigger(ui, text_rect, help),
        Trigger::Info(prefix) => {
            let response = ui.interact(text_rect, Id::new(("help-info", prefix)), Sense::click());
            overlay_trigger(ui, &response, || Overlay::info(prefix));
        }
        Trigger::InfoKeys(title, body) => {
            let response = ui.interact(text_rect, Id::new(("help-info", title)), Sense::click());
            overlay_trigger(ui, &response, || Overlay::keys(title, body));
        }
    }
}

/// A label that toggles `help`'s card, for rows that are not a `label_row`.
/// The card is the caller's to place, with [`card`], under whatever the help
/// is about.
pub fn label<'h>(
    ui: &mut Ui,
    text: impl Into<egui::WidgetText>,
    help: impl Into<Help<'h>>,
) -> Response {
    let response = ui.add(egui::Label::new(text).selectable(false));
    trigger(ui, response.rect, help.into());
    response
}

/// A section-style title that opens `overlay` in the centre.
pub fn overlay_title(
    ui: &mut Ui,
    text: impl Into<egui::WidgetText>,
    overlay: impl FnOnce() -> Overlay,
) -> Response {
    let response = ui.add(
        egui::Label::new(text)
            .sense(Sense::click())
            .selectable(false),
    );
    overlay_trigger(ui, &response, overlay);
    response
}

/// How long an excerpt may run before it is cut.
const EXCERPT_MAX: usize = 160;

/// A break the markup asked for, kept apart from the words until the cut.
const BREAK: char = '\u{1}';

/// The first sentence of a help body, as plain text, for a tooltip: the tags
/// dropped (`<br>`, `<p>` and `<li>` end the excerpt as a sentence would),
/// whitespace collapsed, and the rest cut at the first sentence end or at
/// `EXCERPT_MAX` characters, with an ellipsis when something was left out.
pub fn excerpt(body: &str) -> String {
    let mut text = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(open) = rest.find('<') {
        text.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('>') else {
            rest = &rest[open..];
            break;
        };
        let tag = rest[open + 1..open + close].trim().to_ascii_lowercase();
        if ["br", "p", "/p", "li"].iter().any(|t| tag.starts_with(t)) {
            text.push(' ');
            text.push(BREAK);
        }
        rest = &rest[open + close + 1..];
    }
    text.push_str(rest);
    let mut out = String::new();
    let mut cut = false;
    for word in text.split_whitespace() {
        if word.starts_with(BREAK) {
            cut = !out.is_empty();
            break;
        }
        let next_len = out.chars().count() + word.chars().count() + usize::from(!out.is_empty());
        if next_len > EXCERPT_MAX {
            cut = true;
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
        if word.ends_with(['.', '!', '?']) {
            break;
        }
    }
    if cut {
        out.push('…');
    }
    out
}

/// A small "i" that opens `overlay`: a ring with the letter in it, drawn at
/// `visibility` (0 = not there, 1 = fully drawn — the caller animates it),
/// accent-coloured under the pointer, with the help's first sentence as its
/// tooltip. The space is taken whatever the visibility, so a header does not
/// shift when the glyph fades in. Returns the glyph's response; a click opens
/// the overlay here.
pub fn info_glyph(ui: &mut Ui, visibility: f32, overlay: impl Fn() -> Overlay) -> Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), Sense::click());
    if visibility <= 0.0 {
        return response;
    }
    let hovered = response.hovered();
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let colour = if hovered {
        theme::ACCENT
    } else {
        theme::TEXT_DIM
    }
    .gamma_multiply(visibility);
    let painter = ui.painter();
    let centre = rect.center();
    painter.circle_stroke(centre, 5.5, egui::Stroke::new(1.0, colour));
    painter.text(
        centre,
        egui::Align2::CENTER_CENTER,
        "i",
        egui::FontId::proportional(9.0),
        colour,
    );
    if response.clicked() {
        claim(ui.ctx());
        open_overlay(ui.ctx(), overlay());
    }
    response.on_hover_text(excerpt(&overlay().body))
}

/// Make an existing, clickable response the trigger of `overlay`, which is
/// only built once clicked.
pub fn overlay_trigger(ui: &Ui, response: &Response, overlay: impl FnOnce() -> Overlay) {
    if decorate(ui, response.rect, response, false) {
        open_overlay(ui.ctx(), overlay());
    }
}

/// The card, under the row that owns it, when its help is the open one.
///
/// Laid out at the width it is given and no wider, the text wrapping inside:
/// a card sized by its own text would widen the panel it sits in (see
/// `widgets::label_row`).
pub fn card<'h>(ui: &mut Ui, help: impl Into<Help<'h>>) {
    let help = help.into();
    if help.is_empty() || !is_open(ui.ctx(), help.id) {
        return;
    }
    // `padding: .3rem .45rem; font-size: 11px; line-height: 1.35`.
    let padding = egui::vec2(7.0, 5.0);
    let width = ui.available_width();
    let galley = egui::WidgetText::from(
        egui::RichText::new(help.text)
            .size(theme::FONT_SIZE_SECTION)
            .color(theme::TEXT),
    )
    .into_galley(
        ui,
        Some(egui::TextWrapMode::Wrap),
        (width - 2.0 * padding.x).max(0.0),
        egui::TextStyle::Body,
    );
    ui.add_space(2.0);
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(width, galley.size().y + 2.0 * padding.y),
        Sense::click(),
    );
    if response.clicked() {
        claim(ui.ctx());
    }
    let radius = egui::CornerRadius::same(theme::CONTROL_RADIUS);
    let painter = ui.painter();
    painter.add(
        egui::Shadow {
            offset: [0, 2],
            blur: 8,
            spread: 0,
            color: Color32::from_black_alpha(110),
        }
        .as_shape(rect, radius),
    );
    // The card sits on the panel's translucent ground, so it gets a dense
    // fill of its own first and the web's accent wash over it: at the wash
    // alone the shadow would show through as a dark smudge.
    painter.rect_filled(rect, radius, theme::POPUP_BG);
    painter.rect(
        rect,
        radius,
        Color32::from_rgba_unmultiplied(120, 200, 255, 26),
        egui::Stroke::new(1.0, Color32::from_rgba_unmultiplied(120, 200, 255, 77)),
        egui::StrokeKind::Inside,
    );
    painter.galley(rect.min + padding, galley, theme::TEXT);
    ui.add_space(2.0);
}

/// End of frame: a click that no trigger and no card took closes the card.
pub fn end_frame(ctx: &egui::Context) {
    let claimed = ctx.data_mut(|d| d.remove_temp::<bool>(claimed_id()).unwrap_or(false));
    if !claimed && ctx.input(|i| i.pointer.any_click()) {
        close(ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An open card is a paragraph across the row's width. It was a tall
    /// sliver of nothing once: the auto-gain row sat in a `ui.horizontal` of
    /// its own, the row took every point that horizontal had, and the card
    /// drawn after it was allocated a width of zero — so its text wrapped one
    /// glyph per line, out of sight, and opening the help looked like opening
    /// a large empty panel.
    #[test]
    fn an_open_card_is_a_paragraph_and_not_a_sliver() {
        let ctx = egui::Context::default();
        let help = Help::from("help.master.autoGain");
        assert!(!help.is_empty(), "the catalogue lost the auto-gain help");
        ctx.data_mut(|d| d.insert_temp(open_id(), help.id));
        let mut block = egui::Rect::NOTHING;
        let mut output = ctx.run_ui(Default::default(), |ui| {
            ui.vertical(|ui| {
                // A panel's width, so the card has to wrap to a few lines.
                ui.set_max_width(400.0);
                let mut on = false;
                crate::ui::widgets::switch_row_help_leading(
                    ui,
                    |ui| {
                        ui.allocate_exact_size(egui::vec2(9.0, 9.0), Sense::hover());
                    },
                    "Auto-gain (anti-clip)",
                    help,
                    &mut on,
                );
                block = ui.min_rect();
            });
        });
        // Nothing paints here; egui still wants its font atlas taken.
        output.textures_delta.clear();
        assert!(
            block.width() > 300.0,
            "row and card took {} of the 400 offered",
            block.width()
        );
        assert!(
            block.height() < 200.0,
            "the card wrapped into a sliver {} tall",
            block.height()
        );
    }

    /// A tooltip gets the first sentence, as plain text, and knows when it
    /// has left something out.
    #[test]
    fn the_excerpt_is_the_first_sentence_without_its_markup() {
        assert_eq!(
            excerpt("The <strong>room</strong> in metres. Then the rest.<br>And more."),
            "The room in metres."
        );
        assert_eq!(
            excerpt("<b>Term:</b> what it does<br>Next term: more"),
            "Term: what it does…"
        );
        assert_eq!(excerpt("  spaced   out\n words "), "spaced out words");
        assert_eq!(excerpt(""), "");
        let long = "word ".repeat(80);
        let cut = excerpt(&long);
        assert!(cut.ends_with('…'), "{cut}");
        assert!(
            cut.chars().count() <= EXCERPT_MAX + 1,
            "{}",
            cut.chars().count()
        );
    }

    #[test]
    fn a_key_without_a_translation_is_no_help() {
        assert!(Help::from("help.no.such.key").is_empty());
        assert!(!Help::from("help.updates.check").is_empty());
    }
}
