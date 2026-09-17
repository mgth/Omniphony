//! The two frames inside a section — the *group* and its *inset* — as
//! `PANELS.md` lays them out.
//!
//! A group is the web's `.renderer-subpanel` (also `.adaptive-subpanel` and
//! `.input-panel-shell`): a hairline card whose title bar carries the group's
//! name, its status if it has one, and the one control that decides what the
//! rest shows. The rows go in the inset, the web's `.renderer-subpanel-body`:
//! the same wash again, no border, indented so the rows read as belonging to
//! the title above them. An inset that would hold nothing is not drawn, so a
//! group whose whole meaning is in its bar stays a single line.

use egui::{Color32, CornerRadius, Margin, RichText, Sense, Stroke, Ui};

use super::{help, theme};

pub struct Group<'a> {
    title: String,
    /// What the title opens in the centred overlay, built only when clicked.
    overlay: Option<Box<dyn Fn() -> help::Overlay + 'a>>,
    /// A short readout beside the title (`#vbapStatus`): what the group is
    /// doing, in its own colour.
    status: Option<(String, Color32)>,
    /// The bar's right end (`.renderer-subpanel-actions`): the group's key
    /// control, drawn right to left.
    actions: Option<Box<dyn FnOnce(&mut Ui) + 'a>>,
}

impl<'a> Group<'a> {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            overlay: None,
            status: None,
            actions: None,
        }
    }

    /// The title opens `<prefix>.infoTitle` / `<prefix>.infoBody`.
    pub fn info(self, prefix: &'a str) -> Self {
        self.overlay(move || help::Overlay::info(prefix))
    }

    /// The title opens a `help.*` string under the group's own name.
    pub fn help(self, key: &'a str) -> Self {
        let title = self.title.clone();
        self.overlay(move || help::Overlay::titled(title.clone(), key))
    }

    /// The title opens whatever `build` makes: for a group whose help
    /// depends on its state, as the backend's does on the backend chosen.
    pub fn overlay(mut self, build: impl Fn() -> help::Overlay + 'a) -> Self {
        self.overlay = Some(Box::new(build));
        self
    }

    pub fn status(mut self, text: impl Into<String>, colour: Color32) -> Self {
        self.status = Some((text.into(), colour));
        self
    }

    pub fn actions(mut self, draw: impl FnOnce(&mut Ui) + 'a) -> Self {
        self.actions = Some(Box::new(draw));
        self
    }

    /// The card: its bar, then the inset with `body` in it — unless `body`
    /// drew nothing, in which case the card ends with its bar.
    pub fn show<R>(self, ui: &mut Ui, body: impl FnOnce(&mut Ui) -> R) -> R {
        card(ui, |ui| {
            self.bar_in(ui);
            inset(ui, body)
        })
    }

    /// The card with its bar alone, for a group that is its key control.
    pub fn bar(self, ui: &mut Ui) {
        card(ui, |ui| self.bar_in(ui));
    }

    /// `.renderer-subpanel-bar`: the actions placed first, from the right,
    /// then the title and the status in what they leave, the title truncated
    /// before the status is — as `widgets::label_row` lays out a row, and for
    /// the same reason: laid out the other way, a title longer than its room
    /// sits under the actions instead of pushing them.
    fn bar_in(self, ui: &mut Ui) {
        let Group {
            title,
            overlay,
            status,
            actions,
        } = self;
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(draw) = actions {
                    draw(ui);
                }
                let room = ui.available_width();
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    let spacing = ui.spacing().item_spacing.x;
                    let status_width = status
                        .as_ref()
                        .map(|(text, _)| {
                            egui::WidgetText::from(RichText::new(text).size(theme::FONT_SIZE_SMALL))
                                .into_galley(
                                    ui,
                                    Some(egui::TextWrapMode::Extend),
                                    f32::INFINITY,
                                    egui::TextStyle::Body,
                                )
                                .size()
                                .x
                        })
                        .unwrap_or(0.0);
                    // The title keeps at least half the room; a long status
                    // truncates before the title does.
                    let title_max = (room - status_width - spacing).max(room * 0.5);
                    let text = RichText::new(&title)
                        .size(theme::FONT_SIZE)
                        .color(theme::TEXT_WHITE);
                    let response = ui
                        .scope(|ui| {
                            ui.set_max_width(title_max);
                            ui.add(egui::Label::new(text).truncate().selectable(false).sense(
                                if overlay.is_some() {
                                    Sense::click()
                                } else {
                                    Sense::hover()
                                },
                            ))
                        })
                        .inner;
                    if let Some(build) = overlay {
                        help::overlay_trigger(ui, &response, || build());
                    }
                    if let Some((text, colour)) = status {
                        ui.add(
                            egui::Label::new(
                                RichText::new(text)
                                    .size(theme::FONT_SIZE_SMALL)
                                    .color(colour),
                            )
                            .truncate()
                            .selectable(false),
                        );
                    }
                });
            });
        });
    }
}

/// `.renderer-subpanel`: the hairline card, full width so every group of a
/// section lines up whatever it holds.
fn card<R>(ui: &mut Ui, content: impl FnOnce(&mut Ui) -> R) -> R {
    // `gap: .35rem` between groups, on top of the row gap already paid.
    ui.add_space(theme::GROUP_GAP - ui.spacing().item_spacing.y);
    egui::Frame::new()
        .fill(theme::GROUP_FILL)
        .stroke(Stroke::new(1.0, theme::HAIRLINE))
        .corner_radius(CornerRadius::same(theme::GROUP_RADIUS))
        .inner_margin(Margin::symmetric(
            theme::GROUP_PADDING_X as i8,
            theme::GROUP_PADDING_Y as i8,
        ))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            content(ui)
        })
        .inner
}

/// `.renderer-subpanel-body`: the indented, borderless box a group's rows sit
/// in. Nothing is drawn — and no height taken — when `body` adds nothing, so
/// a body that is conditional from end to end costs no empty box.
pub fn inset<R>(ui: &mut Ui, body: impl FnOnce(&mut Ui) -> R) -> R {
    let frame = egui::Frame::new()
        .fill(theme::GROUP_FILL)
        .corner_radius(CornerRadius::same(theme::CONTROL_RADIUS))
        .inner_margin(Margin::symmetric(
            theme::INSET_PADDING_X as i8,
            theme::INSET_PADDING_Y as i8,
        ))
        .outer_margin(Margin {
            left: theme::INSET_INDENT as i8,
            right: 0,
            // `margin-top: .25rem`, less the row gap the layout already adds.
            top: (theme::GROUP_PADDING_Y - theme::ROW_GAP) as i8,
            bottom: 0,
        });
    let mut prepared = frame.begin(ui);
    prepared
        .content_ui
        .set_min_width(prepared.content_ui.available_width());
    let out = body(&mut prepared.content_ui);
    // Ending the frame paints it and takes its space; a frame that was never
    // ended does neither.
    if prepared.content_ui.min_size().y > 0.0 {
        prepared.end(ui);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A group whose body draws nothing ends with its bar: no empty box.
    #[test]
    fn an_empty_body_takes_no_room() {
        let ctx = egui::Context::default();
        let (mut with_bar, mut with_empty_body) = (0.0, 0.0);
        let mut output = ctx.run_ui(Default::default(), |ui| {
            ui.vertical(|ui| {
                let top = ui.cursor().top();
                Group::new("A").bar(ui);
                with_bar = ui.cursor().top() - top;
                let top = ui.cursor().top();
                Group::new("B").show(ui, |_| {});
                with_empty_body = ui.cursor().top() - top;
            });
        });
        // Nothing paints here; egui still wants its font atlas taken.
        output.textures_delta.clear();
        assert!(with_bar > 0.0);
        assert_eq!(with_empty_body, with_bar, "an empty inset took room");
    }

    /// A body with a row in it gets its inset under the bar.
    #[test]
    fn a_body_with_rows_stands_under_the_bar() {
        let ctx = egui::Context::default();
        let (mut with_bar, mut with_body) = (0.0, 0.0);
        let mut output = ctx.run_ui(Default::default(), |ui| {
            ui.vertical(|ui| {
                let top = ui.cursor().top();
                Group::new("A").bar(ui);
                with_bar = ui.cursor().top() - top;
                let top = ui.cursor().top();
                Group::new("B").show(ui, |ui| {
                    ui.label("row");
                });
                with_body = ui.cursor().top() - top;
            });
        });
        output.textures_delta.clear();
        assert!(with_body > with_bar);
    }
}
