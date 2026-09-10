//! Small widgets the spike needs that egui does not ship: a toggle switch
//! (Studio never shows native checkboxes) and a schema-driven option row that
//! stands in for the live-options registry binder.

use egui::{Color32, Response, Ui};

/// A pill switch. Behaves like a checkbox for accessibility (AccessKit gets a
/// checkbox role and the on/off state).
pub fn toggle_switch(ui: &mut Ui, on: &mut bool) -> Response {
    let desired = ui.spacing().interact_size.y * egui::vec2(1.9, 1.0);
    let (rect, mut response) = ui.allocate_exact_size(desired, egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *on, "")
    });
    if ui.is_rect_visible(rect) {
        let how_on = ui.ctx().animate_bool_responsive(response.id, *on);
        let visuals = ui.style().interact_selectable(&response, *on);
        let rect = rect.expand(visuals.expansion);
        let radius = 0.5 * rect.height();
        let track = if *on {
            Color32::from_rgb(20, 132, 134)
        } else {
            visuals.bg_fill
        };
        ui.painter().rect(
            rect,
            radius,
            track,
            visuals.bg_stroke,
            egui::StrokeKind::Inside,
        );
        let knob_x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), how_on);
        ui.painter().circle(
            egui::pos2(knob_x, rect.center().y),
            0.72 * radius,
            Color32::from_gray(235),
            visuals.fg_stroke,
        );
    }
    response
}

/// Label on the left, switch on the right. Returns true when toggled.
pub fn switch_row(ui: &mut Ui, label: &str, on: &mut bool) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            changed = toggle_switch(ui, on).changed();
        });
    });
    changed
}

// ---------------------------------------------------------------------------
// Schema-driven options (mirror of the renderer's `OptionSpec`)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub enum OptionKind {
    Bool,
    Enum(&'static [&'static str]),
    Str,
}

#[derive(Clone, Copy, Debug)]
pub enum OptionDefault {
    Bool(bool),
    Str(&'static str),
}

#[derive(Clone, Debug, PartialEq)]
pub enum OptionValue {
    Bool(bool),
    Str(String),
}

impl OptionDefault {
    pub fn to_value(self) -> OptionValue {
        match self {
            OptionDefault::Bool(b) => OptionValue::Bool(b),
            OptionDefault::Str(s) => OptionValue::Str(s.to_owned()),
        }
    }
}

pub struct OptionSpec {
    /// Canonical registry key (`/omniphony/control/option [key, value]`).
    pub key: &'static str,
    /// English label; the real Studio resolves `i18n_key` instead.
    pub label: &'static str,
    pub help: Option<&'static str>,
    pub kind: OptionKind,
    pub default: OptionDefault,
}

/// The first five options declared in `renderer/src/options.rs`, keys, kinds
/// and defaults copied verbatim. In the port this table comes from the
/// renderer's `/state/options_schema` instead of being typed here.
pub const OPTION_SCHEMA: &[OptionSpec] = &[
    OptionSpec {
        key: "surround_placement",
        label: "Surround placement",
        help: None,
        kind: OptionKind::Enum(&["side", "back"]),
        default: OptionDefault::Str("side"),
    },
    OptionSpec {
        key: "synthetic_objects_enabled",
        label: "Synthetic objects",
        help: Some("Synthesize height/phantom objects from 2D sources."),
        kind: OptionKind::Bool,
        default: OptionDefault::Bool(false),
    },
    OptionSpec {
        key: "output_channel_mapping",
        label: "Output channel mapping",
        help: None,
        kind: OptionKind::Enum(&["by_index", "by_name"]),
        default: OptionDefault::Str("by_index"),
    },
    OptionSpec {
        key: "object_generator_id",
        label: "Object generator",
        help: Some("Registry id of the object generator backend."),
        kind: OptionKind::Str,
        default: OptionDefault::Str(""),
    },
    OptionSpec {
        key: "phantom_extract_mode",
        label: "Phantom extraction",
        help: Some("Extract phantom centre content from stereo pairs."),
        kind: OptionKind::Enum(&["off", "broadband", "spectral"]),
        default: OptionDefault::Str("off"),
    },
];

/// Render one option from its spec. Returns true when the value changed.
pub fn option_row(ui: &mut Ui, spec: &OptionSpec, value: &mut OptionValue) -> bool {
    let mut changed = false;
    match (spec.kind, value) {
        (OptionKind::Bool, OptionValue::Bool(b)) => {
            changed = switch_row(ui, spec.label, b);
        }
        (OptionKind::Enum(values), OptionValue::Str(s)) => {
            ui.horizontal(|ui| {
                ui.label(spec.label);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    egui::ComboBox::from_id_salt(spec.key)
                        .selected_text(s.as_str())
                        .width(120.0)
                        .show_ui(ui, |ui| {
                            for v in values {
                                if ui.selectable_label(s == v, *v).clicked() && s != v {
                                    *s = (*v).to_owned();
                                    changed = true;
                                }
                            }
                        });
                });
            });
        }
        (OptionKind::Str, OptionValue::Str(s)) => {
            ui.horizontal(|ui| {
                ui.label(spec.label);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    changed = ui
                        .add(
                            egui::TextEdit::singleline(s)
                                .desired_width(120.0)
                                .hint_text("registry id"),
                        )
                        .changed();
                });
            });
        }
        _ => {
            ui.colored_label(Color32::RED, format!("{}: kind/value mismatch", spec.key));
        }
    }
    if let Some(help) = spec.help {
        ui.small(help);
    }
    changed
}
