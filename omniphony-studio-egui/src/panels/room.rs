//! Room geometry (`#roomGeometryPanelRoot`, `controls/room-geometry.js`): the
//! five metre dimensions, the derived scale, and the front/rear blend.
//!
//! The renderer works in ratios with the half-width as the unit, so width is
//! the reference: everything else is that measurement divided by the scale.

use egui::{RichText, Ui};

use crate::app::StudioSpike;
use crate::i18n::t;
use crate::model::app_state::RoomRatio;
use crate::ui::section::Section;
use crate::ui::{help, theme, widgets};

/// The dimensions the form edits, in metres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoomDimensions {
    pub width: f64,
    pub front: f64,
    pub rear: f64,
    pub height: f64,
    pub lower: f64,
    /// Front/rear weighting, 0..1 (the slider shows it as `b/100−b`).
    pub center_blend: f64,
}

impl RoomDimensions {
    /// `computeRoomGeometryFromInputs` in reverse: the renderer's ratios and
    /// scale read back as metres.
    pub fn from_ratio(ratio: &RoomRatio) -> Self {
        let mpu = ratio.scale_m.max(0.01);
        Self {
            width: ratio.width * mpu * 2.0,
            front: ratio.length * mpu,
            rear: ratio.rear * mpu,
            height: ratio.height * mpu,
            lower: ratio.lower * mpu,
            center_blend: ratio.center_blend,
        }
    }

    /// Metres per scene unit: the half-width.
    pub fn meters_per_unit(&self) -> f64 {
        (self.width / 2.0).max(0.01)
    }

    /// The ratios the renderer is sent.
    pub fn to_ratio(self) -> RoomRatio {
        let mpu = self.meters_per_unit();
        RoomRatio {
            width: 1.0,
            length: (self.front.max(0.01) / mpu).max(0.01),
            height: (self.height.max(0.01) / mpu).max(0.01),
            rear: (self.rear.max(0.01) / mpu).max(0.01),
            lower: (self.lower.max(0.01) / mpu).max(0.01),
            center_blend: self.center_blend.clamp(0.0, 1.0),
            scale_m: mpu,
        }
    }
}

impl StudioSpike {
    pub(crate) fn room_geometry_section(&mut self, ui: &mut Ui) {
        let (ratio, frozen) = {
            let live = self.live.lock().unwrap();
            (
                live.app.room_ratio.clone(),
                live.app.render_backend_state.frozen_room_ratio,
            )
        };
        let current = RoomDimensions::from_ratio(&ratio);
        // The renderer's value wins whenever it differs from what this panel
        // last sent, so an edit elsewhere is not silently overwritten.
        if self.room_edit != Some(current) && !self.room_editing {
            self.room_edit = Some(current);
        }
        let mut edit = self.room_edit.unwrap_or(current);
        let summary = format!(
            "m/u {:.2} • X {:.2}m • Y {:.2}m • Z {:.2}m",
            edit.meters_per_unit(),
            edit.width,
            edit.front + edit.rear,
            edit.height + edit.lower
        );
        // The guides exist to be read while these numbers are being edited,
        // so they follow the panel rather than a switch of their own.
        let open = Section::new("roomGeometrySection", "room.title")
            .info("room")
            .summary(summary)
            .show(ui, |ui| {
                ui.add_enabled_ui(!frozen, |ui| {
                    let mut step = Step::default();
                    // `#roomGeometryForm`: three columns, one per axis — the
                    // width and the scale it sets under X, the two depths
                    // under Y, the two heights under Z.
                    ui.columns(3, |columns| {
                        axis_header(&mut columns[0], "X");
                        axis_header(&mut columns[1], "Y");
                        axis_header(&mut columns[2], "Z");
                        step |= metre_field(
                            &mut columns[0],
                            t("room.axis.width"),
                            "help.room.width",
                            &mut edit.width,
                        );
                        field_label(&mut columns[0], "m/u", None);
                        columns[0].with_layout(
                            egui::Layout::right_to_left(egui::Align::Min),
                            |ui| {
                                ui.label(
                                    RichText::new(format!("{:.2}", edit.meters_per_unit()))
                                        .size(11.0)
                                        .color(theme::TEXT),
                                );
                            },
                        );
                        step |= metre_field(
                            &mut columns[1],
                            t("room.axis.length"),
                            "help.room.front",
                            &mut edit.front,
                        );
                        step |= metre_field(
                            &mut columns[1],
                            t("room.axis.rear"),
                            "help.room.rear",
                            &mut edit.rear,
                        );
                        step |= metre_field(
                            &mut columns[2],
                            t("room.axis.height"),
                            "help.room.height",
                            &mut edit.height,
                        );
                        step |= metre_field(
                            &mut columns[2],
                            t("room.axis.lower"),
                            "help.room.lower",
                            &mut edit.lower,
                        );
                    });
                    // The fields' help opens under the whole grid, as the
                    // web anchors it to the form: a column is too narrow for
                    // a paragraph.
                    for key in [
                        "help.room.width",
                        "help.room.front",
                        "help.room.rear",
                        "help.room.height",
                        "help.room.lower",
                    ] {
                        help::card(ui, key);
                    }

                    // The blend only means something with different front and
                    // rear depths.
                    if (edit.front - edit.rear).abs() >= 1e-6 {
                        step |= center_blend_row(ui, &mut edit.center_blend);
                    }

                    self.room_edit = Some(edit);
                    if step.commit {
                        self.apply_room_geometry(edit);
                    } else if step.preview {
                        // Mid-drag: the scene follows, the renderer waits for
                        // the release, as the web previews while typing and
                        // commits on Enter or blur. Sent every frame, a drag
                        // re-planned the layout at the frame rate.
                        self.room_editing = true;
                        self.live.lock().unwrap().app.room_ratio = edit.to_ratio();
                    }
                });
            })
            .is_some();
        self.settings.room_guides_visible = open;
    }

    /// The five messages `applyRoomGeometryNow` sends, in its order: the
    /// scale, the blend, the box, then the two extra depths.
    fn apply_room_geometry(&mut self, edit: RoomDimensions) {
        let ratio = edit.to_ratio();
        {
            let mut live = self.live.lock().unwrap();
            live.app.room_ratio = ratio.clone();
        }
        self.ctl.send_json(
            "/omniphony/control/config/layout",
            &serde_json::json!({ "radiusM": ratio.scale_m }),
        );
        self.ctl.send_float(
            "/omniphony/control/room_ratio_center_blend",
            ratio.center_blend as f32,
        );
        self.ctl.send_floats3(
            "/omniphony/control/room_ratio",
            ratio.width as f32,
            ratio.length as f32,
            ratio.height as f32,
        );
        self.ctl
            .send_float("/omniphony/control/room_ratio_rear", ratio.rear as f32);
        self.ctl
            .send_float("/omniphony/control/room_ratio_lower", ratio.lower as f32);
        self.room_editing = false;
    }
}

/// What a control did this frame: moved mid-drag (the scene previews it),
/// or settled (the renderer is sent it).
#[derive(Clone, Copy, Default)]
struct Step {
    preview: bool,
    commit: bool,
}

impl std::ops::BitOrAssign for Step {
    fn bitor_assign(&mut self, other: Self) {
        self.preview |= other.preview;
        self.commit |= other.commit;
    }
}

impl Step {
    /// A drag commits when it is let go; any other change — a typed value on
    /// Enter, an arrow key — commits at once.
    fn of(response: &egui::Response) -> Self {
        let changed = response.changed();
        Self {
            preview: changed,
            commit: response.drag_stopped() || (changed && !response.dragged()),
        }
    }
}

/// An axis letter heading its column.
fn axis_header(ui: &mut Ui, axis: &str) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
        ui.label(
            RichText::new(axis)
                .size(theme::FONT_SIZE)
                .color(theme::TEXT_STRONG),
        );
    });
}

/// A field's name, small and right-aligned over it; its help, when it has
/// one, opens from it.
fn field_label(ui: &mut Ui, label: &str, help: Option<&str>) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
        let text = RichText::new(label)
            .size(theme::FONT_SIZE_SMALL)
            .color(theme::TEXT_DIM);
        match help {
            Some(key) => {
                help::label(ui, text, key);
            }
            None => {
                ui.label(text);
            }
        }
    });
}

/// One metre field in its column: its name over it, two decimals, never
/// below a centimetre, as wide as the column.
fn metre_field(ui: &mut Ui, label: &str, help: &str, value: &mut f64) -> Step {
    field_label(ui, label, Some(help));
    let mut metres = *value as f32;
    let response = ui.add_sized(
        egui::vec2(ui.available_width(), ui.spacing().interact_size.y),
        egui::DragValue::new(&mut metres)
            .speed(0.01)
            .range(0.01..=f32::MAX)
            .fixed_decimals(2)
            .suffix(" m"),
    );
    let step = Step::of(&response);
    if step.preview {
        *value = f64::from(metres).max(0.01);
    }
    step
}

/// `#roomCenterBlendRow`: the name, the slider taking the room between, and
/// the split as `front/rear`. A double click on either puts it back to
/// 50/50, as the web's `dblclick` on both does.
fn center_blend_row(ui: &mut Ui, blend: &mut f64) -> Step {
    let mut percent = (*blend * 100.0) as f32;
    let (slider, value) =
        widgets::label_row_help(ui, t("room.centerBlend"), "help.room.centerBlend", |ui| {
            let value = ui
                .add(
                    egui::Label::new(
                        RichText::new(format!("{:.0}/{:.0}", percent, 100.0 - percent))
                            .size(11.0)
                            .color(theme::TEXT),
                    )
                    .sense(egui::Sense::click())
                    .selectable(false),
                )
                .on_hover_text(t("room.centerBlend.resetTitle"));
            let slider = ui.add(
                egui::Slider::new(&mut percent, 0.0..=100.0)
                    .step_by(1.0)
                    .show_value(false),
            );
            (slider, value)
        });
    if slider.double_clicked() || value.double_clicked() {
        *blend = 0.5;
        return Step {
            preview: true,
            commit: true,
        };
    }
    let step = Step::of(&slider);
    if step.preview {
        *blend = f64::from(percent / 100.0);
    }
    step
}
