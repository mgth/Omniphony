//! `.info-section`: a rule, a header made of a title, an optional one-line
//! summary and a chevron, and a body that expands in place.
//!
//! The body takes its full height. The web bounds an open section
//! (`--panel-open-max-height: min(44vh, 420px)`) and scrolls it inside; here
//! the overlays already have a fixed extent and one scroll each, so a bound
//! per section only nested a second scrollbar inside the first — a list of
//! twenty speakers in a box of its own, inside a panel that scrolls anyway.
//! An expanding section still cannot resize anything outside its overlay:
//! the overlay's own scroll absorbs it.

use egui::Ui;

use super::theme;

/// The header's disclosure triangle, rotated by `openness` (0 = closed).
fn paint_chevron(ui: &Ui, rect: egui::Rect, openness: f32) {
    let colour = theme::TEXT_MUTED;
    let rect = egui::Rect::from_center_size(rect.center(), egui::vec2(7.0, 7.0));
    let angle = openness * std::f32::consts::FRAC_PI_2;
    let (sin, cos) = angle.sin_cos();
    let points: Vec<egui::Pos2> = [
        egui::vec2(-0.5, -1.0),
        egui::vec2(-0.5, 1.0),
        egui::vec2(1.0, 0.0),
    ]
    .into_iter()
    .map(|v| {
        let v = egui::vec2(v.x * cos - v.y * sin, v.x * sin + v.y * cos);
        rect.center() + v * rect.width() * 0.6
    })
    .collect();
    ui.painter().add(egui::Shape::convex_polygon(
        points,
        colour,
        egui::Stroke::NONE,
    ));
}

/// What a section's title opens, built only when it is clicked.
#[derive(Clone, Copy)]
enum Explains<'a> {
    /// A `<prefix>.infoTitle` / `<prefix>.infoBody` pair.
    Info(&'a str),
    /// A `help.*` string under the section's own title.
    Help(&'a str),
}

pub struct Section<'a> {
    id: &'a str,
    title: String,
    summary: Option<String>,
    explains: Option<Explains<'a>>,
    default_open: bool,
}

impl<'a> Section<'a> {
    /// `id` is the DOM id of the web section, so state survives reordering;
    /// `title_key` is its `data-i18n` key.
    pub fn new(id: &'a str, title_key: &str) -> Self {
        Self {
            id,
            title: crate::i18n::t(title_key).to_owned(),
            summary: None,
            explains: None,
            default_open: false,
        }
    }

    /// The collapsed header's one-line summary (`.panel-summary`).
    pub fn summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = Some(summary.into());
        self
    }

    /// A `help.*` string explaining the whole section, opened in the centred
    /// overlay under the section's own title.
    pub fn help(mut self, key: &'a str) -> Self {
        self.explains = Some(Explains::Help(key));
        self
    }

    /// The prefix of an `<id>.infoTitle` / `<id>.infoBody` pair. The web
    /// promotes the title itself into the trigger — a dotted underline and a
    /// pointer — rather than hanging an "i" button beside it, so the thing you
    /// click is the thing you are asking about.
    pub fn info(mut self, key: &'a str) -> Self {
        self.explains = Some(Explains::Info(key));
        self
    }

    pub fn default_open(mut self, open: bool) -> Self {
        self.default_open = open;
        self
    }

    pub fn show<R>(self, ui: &mut Ui, body: impl FnOnce(&mut Ui) -> R) -> Option<R> {
        let id = ui.make_persistent_id(("section", self.id));
        let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
            ui.ctx(),
            id,
            self.default_open,
        );
        ui.add_space(theme::PANEL_GAP);
        ui.separator();
        let header = ui.horizontal(|ui| {
            let openness = state.openness(ui.ctx());
            let (rect, chevron) =
                ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::click());
            paint_chevron(ui, rect, openness);
            let text = egui::RichText::new(&self.title)
                .size(theme::FONT_SIZE_SECTION)
                .color(theme::TEXT_STRONG);
            let title = ui.add(
                egui::Label::new(text)
                    .sense(egui::Sense::click())
                    .selectable(false),
            );
            if let Some(explains) = self.explains {
                // The title is the trigger: the thing you click is the thing
                // you are asking about.
                super::help::overlay_trigger(ui, &title, || match explains {
                    Explains::Info(prefix) => super::help::Overlay::info(prefix),
                    Explains::Help(key) => super::help::Overlay::titled(&self.title, key),
                });
            }
            if let Some(summary) = &self.summary {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(summary)
                                .size(theme::FONT_SIZE_SMALL)
                                .color(theme::TEXT_MUTED),
                        )
                        .truncate()
                        .selectable(false),
                    );
                });
            }
            // A title that opens a modal is the modal's trigger, not the
            // section's: the chevron keeps the disclosure to itself, or one
            // click would both explain the section and close it.
            if self.explains.is_some() {
                chevron
            } else {
                chevron.union(title)
            }
        });
        if header.inner.clicked() {
            state.toggle(ui);
        }
        state.show_body_unindented(ui, body).map(|r| r.inner)
    }
}
