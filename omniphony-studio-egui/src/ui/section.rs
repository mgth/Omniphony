//! `.info-section`: a rule, a header made of a title, an optional one-line
//! summary and a chevron, and a body that expands in place.
//!
//! The body is height-bounded and scrolls internally
//! (`--panel-open-max-height: min(44vh, 420px)`), which is what keeps an
//! expanding section from resizing anything outside the overlay.

use egui::Ui;

use super::theme;

/// `min(44vh, 420px)`.
pub fn open_max_height(viewport_height: f32) -> f32 {
    (0.44 * viewport_height).min(420.0)
}

/// `min(52vh, 520px)`, for the sections the stylesheet marks as large.
pub fn open_max_height_large(viewport_height: f32) -> f32 {
    (0.52 * viewport_height).min(520.0)
}

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

pub struct Section<'a> {
    id: &'a str,
    title: String,
    summary: Option<String>,
    help_key: Option<&'a str>,
    default_open: bool,
    max_height: f32,
}

impl<'a> Section<'a> {
    /// `id` is the DOM id of the web section, so state survives reordering;
    /// `title_key` is its `data-i18n` key.
    pub fn new(id: &'a str, title_key: &str) -> Self {
        Self {
            id,
            title: crate::i18n::t(title_key).to_owned(),
            summary: None,
            help_key: None,
            default_open: false,
            max_height: 420.0,
        }
    }

    /// The collapsed header's one-line summary (`.panel-summary`).
    pub fn summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = Some(summary.into());
        self
    }

    pub fn help(mut self, key: &'a str) -> Self {
        self.help_key = Some(key);
        self
    }

    pub fn default_open(mut self, open: bool) -> Self {
        self.default_open = open;
        self
    }

    pub fn max_height(mut self, height: f32) -> Self {
        self.max_height = height;
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
            let title = ui.add(
                egui::Label::new(
                    egui::RichText::new(&self.title)
                        .size(theme::FONT_SIZE_SECTION)
                        .color(theme::TEXT_STRONG),
                )
                .sense(egui::Sense::click())
                .selectable(false),
            );
            if let Some(key) = self.help_key {
                super::widgets::help(ui, key);
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
            chevron.union(title)
        });
        if header.inner.clicked() {
            state.toggle(ui);
        }
        let max_height = self.max_height;
        let id_salt = self.id.to_owned();
        state
            .show_body_unindented(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt(id_salt)
                    .max_height(max_height)
                    .auto_shrink([false, true])
                    .show(ui, body)
                    .inner
            })
            .map(|r| r.inner)
    }
}
