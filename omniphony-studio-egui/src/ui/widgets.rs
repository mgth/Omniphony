//! The Studio's recurring controls. Each one mirrors a CSS class of
//! `styles/app.css`: the switch of `.inline-toggle input[type=checkbox]`, the
//! `.toggle-btn` pair, `.gain-box`, `.meter`, `.info-row`, the status dot and
//! the banners. They are plain functions over `Ui`, so a panel reads like the
//! markup it replaces.

#![allow(dead_code)] // the toolkit is complete before every panel using it
use egui::{Color32, Response, Sense, Ui, Widget, vec2};

use super::help::{Help, Trigger};
use super::theme;

/// `.inline-toggle input[type="checkbox"]`: 34×18 track, 12 px thumb, green
/// when on.
pub fn switch(ui: &mut Ui, on: &mut bool) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(vec2(34.0, 18.0), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *on, "")
    });
    if ui.is_rect_visible(rect) {
        let how_on = ui.ctx().animate_bool_responsive(response.id, *on);
        let enabled = ui.is_enabled();
        let track = if *on {
            Color32::from_rgba_unmultiplied(82, 226, 162, 115)
        } else {
            theme::FILL_ACTIVE
        };
        let track = if enabled {
            track
        } else {
            track.gamma_multiply(0.5)
        };
        ui.painter().rect(
            rect,
            9.0,
            track,
            egui::Stroke::new(1.0, theme::CONTROL_BORDER),
            egui::StrokeKind::Inside,
        );
        let x = egui::lerp((rect.left() + 9.0)..=(rect.right() - 9.0), how_on);
        ui.painter().circle_filled(
            egui::pos2(x, rect.center().y),
            6.0,
            if enabled {
                theme::TEXT
            } else {
                theme::TEXT_FAINT
            },
        );
    }
    response
}

/// A label on the left and controls on the right, where the controls win.
///
/// They are placed first, from the right edge, and the label takes what they
/// leave — truncated if it must be, and egui shows it whole on hover. Laid out
/// the other way round, label first and then a right-to-left block, a label
/// longer than its room simply sits *under* the controls: a right-to-left
/// layout draws over what is already there instead of pushing it, and the
/// panel's measured width stays innocent while the row is unreadable.
pub fn label_row<R>(
    ui: &mut Ui,
    label: impl Into<egui::WidgetText>,
    add_right: impl FnOnce(&mut Ui) -> R,
) -> R {
    labelled(ui, label.into(), None, add_right)
}

/// `label_row` whose label opens `help` in a card under the row (see
/// [`super::help`]). `help` is a `help.*` key, or a [`Help`] built from text.
pub fn label_row_help<'h, R>(
    ui: &mut Ui,
    label: impl Into<egui::WidgetText>,
    help: impl Into<Help<'h>>,
    add_right: impl FnOnce(&mut Ui) -> R,
) -> R {
    let help = help.into();
    let out = labelled(ui, label.into(), Some(Trigger::Card(help)), add_right);
    super::help::card(ui, help);
    out
}

/// `label_row` whose label heads a whole block and opens the centred overlay
/// for its `<prefix>.infoTitle` / `.infoBody` pair.
pub fn label_row_info<R>(
    ui: &mut Ui,
    label: impl Into<egui::WidgetText>,
    info_prefix: &str,
    add_right: impl FnOnce(&mut Ui) -> R,
) -> R {
    labelled(
        ui,
        label.into(),
        Some(Trigger::Info(info_prefix)),
        add_right,
    )
}

fn labelled<R>(
    ui: &mut Ui,
    label: egui::WidgetText,
    help: Option<Trigger<'_>>,
    add_right: impl FnOnce(&mut Ui) -> R,
) -> R {
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let out = add_right(ui);
            // What the controls left, from the row's own left edge. The label
            // takes exactly that and no more: it is laid out at that width,
            // cut to it, and painted inside it. Left to size itself, a label
            // that could not quite fit made its row a few points wider than
            // the panel — and egui grows a layout's bounds to whatever its
            // children took, so every row after it started those few points
            // further left and lost its first letter to the panel's edge.
            let space = ui.available_rect_before_wrap();
            let text_width = space.width().max(0.0);
            let galley = label.clone().into_galley(
                ui,
                Some(egui::TextWrapMode::Truncate),
                text_width,
                egui::TextStyle::Body,
            );
            // One row high: the space's own height is whatever is left below.
            let row_height = ui.spacing().interact_size.y.max(galley.size().y);
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(space.width(), row_height), Sense::hover());
            let text_rect = egui::Rect::from_min_size(
                egui::pos2(rect.left(), rect.center().y - galley.size().y / 2.0),
                galley.size(),
            );
            if text_width > 0.0 {
                let truncated = galley.elided;
                let colour = ui.visuals().text_color();
                ui.painter()
                    .with_clip_rect(rect.intersect(ui.clip_rect()))
                    .galley(text_rect.min, galley, colour);
                if truncated {
                    response.on_hover_text(label.text());
                }
                // The label itself is the trigger, over the text only: the
                // blank between it and the controls stays inert.
                if let Some(help) = help {
                    super::help::trigger_any(ui, text_rect.intersect(rect), help);
                }
            }
            out
        })
        .inner
    })
    .inner
}

/// The width a dropdown in a `label_row` takes: its usual width, but never more
/// than 60 % of the row, so its label keeps room to be read when the panel is
/// narrow. Called inside the row's controls closure, where the row is still
/// wholly available.
pub fn combo_width(ui: &Ui, preferred: f32) -> f32 {
    (ui.available_width() * 0.6).min(preferred).max(40.0)
}

/// A dropdown held to `combo_width(preferred)`. egui takes `ComboBox::width`
/// as a minimum and, truncating, lays the selected text out against all the
/// width it can see — so a truncated combo grows to fill whatever is left of
/// its row, and a row of label and combo ran past the panel. The combo is
/// drawn inside a scope bounded to its width, so that is all it can see.
/// `add` gets the bounded ui and the width to pass to `ComboBox::width`.
pub fn bounded_combo<R>(ui: &mut Ui, preferred: f32, add: impl FnOnce(&mut Ui, f32) -> R) -> R {
    let width = combo_width(ui, preferred);
    ui.scope(|ui| {
        ui.set_max_width(width);
        add(ui, width)
    })
    .inner
}

/// Label left, switch right (`.inline-toggle`). Returns true when toggled.
pub fn switch_row(ui: &mut Ui, label: &str, on: &mut bool) -> bool {
    label_row(ui, label, |ui| switch(ui, on).changed())
}

/// `switch_row` whose label opens `help`.
pub fn switch_row_help<'h>(
    ui: &mut Ui,
    label: &str,
    help: impl Into<Help<'h>>,
    on: &mut bool,
) -> bool {
    label_row_help(ui, label, help, |ui| switch(ui, on).changed())
}

/// A `.toggle-btn` group: one active value out of a list of (value, label).
/// Returns the newly picked value.
pub fn toggle_buttons<'a, T: PartialEq + Clone>(
    ui: &mut Ui,
    current: &T,
    options: &[(T, &'a str)],
) -> Option<T> {
    let mut picked = None;
    ui.horizontal(|ui| {
        for (value, label) in options {
            let active = value == current;
            let mut text = egui::RichText::new(*label);
            if active {
                text = text.color(theme::TEXT_STRONG);
            }
            if ui.selectable_label(active, text).clicked() && !active {
                picked = Some(value.clone());
            }
        }
    });
    picked
}

/// `.gain-box`: a label, a slider, and the value with its unit. `format`
/// renders the readout so each caller keeps the web's exact formatting.
///
/// Narrowing the panel takes room from the track first, down to `MIN_TRACK`,
/// and only then from the label: a slider that is merely shorter still works,
/// a label cut to three letters says nothing.
pub fn value_slider(
    ui: &mut Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    step: f64,
    format: impl Fn(f32) -> String,
) -> bool {
    slider_row(ui, label, None, value, range, step, format)
}

/// `value_slider` whose label opens `help`.
pub fn value_slider_help<'h>(
    ui: &mut Ui,
    label: &str,
    help: impl Into<Help<'h>>,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    step: f64,
    format: impl Fn(f32) -> String,
) -> bool {
    let help = help.into();
    let changed = slider_row(
        ui,
        label,
        Some(Trigger::Card(help)),
        value,
        range,
        step,
        format,
    );
    super::help::card(ui, help);
    changed
}

fn slider_row(
    ui: &mut Ui,
    label: &str,
    help: Option<Trigger<'_>>,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    step: f64,
    format: impl Fn(f32) -> String,
) -> bool {
    let label_width = egui::WidgetText::from(label)
        .into_galley(
            ui,
            Some(egui::TextWrapMode::Extend),
            f32::INFINITY,
            egui::TextStyle::Body,
        )
        .size()
        .x;
    let full_track = ui.spacing().slider_width;
    labelled(ui, label.into(), help, |ui| {
        ui.add_sized(
            vec2(64.0, ui.spacing().interact_size.y),
            egui::Label::new(
                egui::RichText::new(format(*value))
                    .monospace()
                    .color(theme::TEXT_STRONG),
            ),
        );
        let room = ui.available_width() - label_width - ui.spacing().item_spacing.x;
        ui.spacing_mut().slider_width = room.clamp(MIN_TRACK, full_track.max(MIN_TRACK));
        egui::Slider::new(value, range)
            .show_value(false)
            .step_by(step)
            .ui(ui)
            .changed()
    })
}

/// The connection dot of the OSC panel: a filled circle plus a caption.
pub fn status_dot(ui: &mut Ui, colour: Color32, text: impl Into<egui::WidgetText>) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.0, colour);
        ui.add(egui::Label::new(text).truncate());
    });
}

/// Severity of a banner row (`#bridgeErrorBanner`, `#foreignRendererBanner`,
/// `#updateAvailableBanner`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl Severity {
    fn colour(self) -> Color32 {
        match self {
            Severity::Info => theme::ACCENT,
            Severity::Warning => theme::WARN,
            Severity::Error => theme::ERROR,
        }
    }
}

/// A tinted, outlined block of text.
pub fn banner(ui: &mut Ui, severity: Severity, title: &str, detail: Option<&str>) {
    banner_with(ui, severity, title, |ui| {
        if let Some(detail) = detail {
            ui.label(
                egui::RichText::new(detail)
                    .size(theme::FONT_SIZE_SMALL)
                    .color(theme::TEXT_MUTED),
            );
        }
    });
}

/// A banner whose body the caller draws — for the ones that carry a link or a
/// control rather than a line of prose.
pub fn banner_with(ui: &mut Ui, severity: Severity, title: &str, body: impl FnOnce(&mut Ui)) {
    let colour = severity.colour();
    egui::Frame::new()
        .fill(colour.gamma_multiply(0.12))
        .stroke(egui::Stroke::new(1.0, colour.gamma_multiply(0.5)))
        .corner_radius(theme::CONTROL_RADIUS)
        .inner_margin(egui::Margin::symmetric(8, 5))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(title).color(colour));
                body(ui);
            });
        });
}

/// The frame a modal sits in (`.modal-card`).
///
/// Opaque, unlike the side overlays: a dialog painted at the panels' 78 % lets
/// the scene and the controls behind it show through its own text, and a box
/// that has to be read is the one place the translucency costs more than it
/// buys.
pub fn modal_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(theme::PAGE_BG)
        .stroke(egui::Stroke::new(1.0, theme::PANEL_BORDER))
        .corner_radius(theme::PANEL_RADIUS)
        .inner_margin(egui::Margin::symmetric(
            theme::PANEL_PADDING_X as i8,
            theme::PANEL_PADDING_Y as i8,
        ))
}

/// `.option-status-note`: 11 px muted text under a control.
pub fn note(ui: &mut Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(theme::FONT_SIZE_SMALL)
            .color(theme::TEXT_MUTED),
    );
}

/// The shortest track a slider row keeps; its label truncates before the track
/// goes below it.
const MIN_TRACK: f32 = 48.0;

/// The web's Display-panel slider row (`.control-row`, `grid auto 1fr`): the
/// label with its current value on the left, the track taking what is left.
///
/// `Slider::text` cannot do this. It lays a track of the style's fixed width,
/// a value box and then the label on one line, and none of the three shrinks,
/// so every such row ran past a panel at its minimum width — "Object sphere
/// size" alone does. Here the track is the part that gives, as the web's `1fr`
/// column does, and past a short track the label truncates and shows itself
/// whole on hover.
pub fn slider_line(
    ui: &mut Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    step: f64,
) -> Response {
    let decimals = decimals_for(step);
    ui.horizontal(|ui| {
        let spacing = ui.spacing().item_spacing.x;
        let label_max = (ui.available_width() - MIN_TRACK - spacing).max(0.0);
        let text = format!("{label} {:.*}", decimals, *value);
        ui.scope(|ui| {
            ui.set_max_width(label_max);
            ui.add(egui::Label::new(egui::RichText::new(text).size(theme::FONT_SIZE)).truncate());
        });
        ui.spacing_mut().slider_width = ui.available_width().max(MIN_TRACK);
        ui.add(
            egui::Slider::new(value, range)
                .step_by(step)
                .show_value(false),
        )
    })
    .inner
}

/// As many decimals as the step can move the value by: a 0.002 step reads
/// `0.070`, a 0.5 step `7.0`, a whole step `12`.
fn decimals_for(step: f64) -> usize {
    if step <= 0.0 || !step.is_finite() {
        return 2;
    }
    (-step.log10()).ceil().max(0.0) as usize
}

#[cfg(test)]
mod slider_line_tests {
    use super::decimals_for;

    /// The readout keeps the digits the step can actually change, and no more:
    /// these are the steps the Display sliders use.
    #[test]
    fn the_readout_follows_the_step() {
        assert_eq!(decimals_for(0.002), 3);
        assert_eq!(decimals_for(0.01), 2);
        assert_eq!(decimals_for(0.05), 2);
        assert_eq!(decimals_for(0.1), 1);
        assert_eq!(decimals_for(0.5), 1);
        assert_eq!(decimals_for(1.0), 0);
        assert_eq!(decimals_for(10.0), 0);
    }
}
