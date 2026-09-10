//! The Studio's recurring controls. Each one mirrors a CSS class of
//! `styles/app.css`: the switch of `.inline-toggle input[type=checkbox]`, the
//! `.toggle-btn` pair, `.gain-box`, `.meter`, `.info-row`, the status dot and
//! the banners. They are plain functions over `Ui`, so a panel reads like the
//! markup it replaces.

#![allow(dead_code)] // the toolkit is complete before every panel using it
use egui::{Color32, Response, Sense, Ui, Widget, vec2};

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

/// Label left, switch right (`.inline-toggle`). Returns true when toggled.
pub fn switch_row(ui: &mut Ui, label: &str, on: &mut bool) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            changed = switch(ui, on).changed();
        });
    });
    changed
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
pub fn value_slider(
    ui: &mut Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    step: f64,
    format: impl Fn(f32) -> String,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_sized(
                vec2(64.0, ui.spacing().interact_size.y),
                egui::Label::new(
                    egui::RichText::new(format(*value))
                        .monospace()
                        .color(theme::TEXT_STRONG),
                ),
            );
            let slider = egui::Slider::new(value, range)
                .show_value(false)
                .step_by(step);
            changed = slider.ui(ui).changed();
        });
    });
    changed
}

/// `.meter`: a level bar with an optional peak-hold tick, both already mapped
/// to 0..1 by the caller (the web maps dBFS with `METER_DB_MIN = -60`).
pub fn meter(ui: &mut Ui, level: f32, peak: Option<f32>, clipping: bool) -> Response {
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width().min(120.0), 6.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect_filled(rect, 3.0, theme::FILL);
        let level = level.clamp(0.0, 1.0);
        if level > 0.0 {
            let mut fill = rect;
            fill.set_width(rect.width() * level);
            let colour = if clipping { theme::ERROR } else { theme::OK };
            painter.rect_filled(fill, 3.0, colour);
        }
        if let Some(peak) = peak {
            let x = rect.left() + rect.width() * peak.clamp(0.0, 1.0);
            painter.vline(
                x,
                rect.y_range(),
                egui::Stroke::new(1.5, theme::TEXT_STRONG),
            );
        }
    }
    response
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
    let colour = severity.colour();
    egui::Frame::new()
        .fill(colour.gamma_multiply(0.12))
        .stroke(egui::Stroke::new(1.0, colour.gamma_multiply(0.5)))
        .corner_radius(theme::CONTROL_RADIUS)
        .inner_margin(egui::Margin::symmetric(8, 5))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(title).color(colour));
                if let Some(detail) = detail {
                    ui.label(
                        egui::RichText::new(detail)
                            .size(theme::FONT_SIZE_SMALL)
                            .color(theme::TEXT_MUTED),
                    );
                }
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

/// The `?` affordance of `controls/inline-help.js`: hovering shows the help
/// string for the key.
pub fn help(ui: &mut Ui, help_key: &str) {
    let text = crate::i18n::t(help_key);
    ui.label(
        egui::RichText::new("?")
            .size(theme::FONT_SIZE_SMALL)
            .color(theme::TEXT_FAINT),
    )
    .on_hover_ui(|ui| {
        ui.set_max_width(260.0);
        ui.label(text);
    });
}
