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
use crate::ui::{theme, widgets};

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
        Section::new("roomGeometrySection", "room.title")
            .summary(summary)
            .show(ui, |ui| {
                ui.add_enabled_ui(!frozen, |ui| {
                    let mut changed = false;
                    changed |= metre_row(ui, t("room.axis.width"), &mut edit.width);
                    ui.horizontal(|ui| {
                        widgets::note(ui, t("room.mpu"));
                        ui.label(
                            RichText::new(format!("{:.2}", edit.meters_per_unit()))
                                .monospace()
                                .size(theme::FONT_SIZE_SMALL)
                                .color(theme::TEXT),
                        );
                    });
                    changed |= metre_row(ui, t("room.axis.length"), &mut edit.front);
                    changed |= metre_row(ui, t("room.axis.rear"), &mut edit.rear);
                    changed |= metre_row(ui, t("room.axis.height"), &mut edit.height);
                    changed |= metre_row(ui, t("room.axis.lower"), &mut edit.lower);

                    // The blend only means something with different front and
                    // rear depths.
                    if (edit.front - edit.rear).abs() >= 1e-6 {
                        let mut percent = (edit.center_blend * 100.0) as f32;
                        let response = ui.horizontal(|ui| {
                            ui.label(t("room.centerBlend"));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(
                                    RichText::new(format!("{:.0}/{:.0}", percent, 100.0 - percent))
                                        .monospace()
                                        .color(theme::TEXT_STRONG),
                                );
                                ui.add(
                                    egui::Slider::new(&mut percent, 0.0..=100.0)
                                        .step_by(1.0)
                                        .show_value(false),
                                )
                            })
                            .inner
                        });
                        let slider = response.inner;
                        if slider.double_clicked() {
                            edit.center_blend = 0.5;
                            changed = true;
                        } else if slider.changed() {
                            edit.center_blend = (percent / 100.0) as f64;
                            changed = true;
                        }
                        slider.on_hover_text(t("room.centerBlend.resetTitle"));
                    }

                    self.room_edit = Some(edit);
                    if changed {
                        self.room_editing = true;
                        self.apply_room_geometry(edit);
                    }
                });
            });
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

/// One metre field: two decimals, never below a centimetre.
fn metre_row(ui: &mut Ui, label: &str, value: &mut f64) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let mut metres = *value as f32;
            if ui
                .add_sized(
                    egui::vec2(72.0, ui.spacing().interact_size.y),
                    egui::DragValue::new(&mut metres)
                        .speed(0.01)
                        .range(0.01..=f32::MAX)
                        .fixed_decimals(2)
                        .suffix(" m"),
                )
                .changed()
            {
                *value = (metres as f64).max(0.01);
                changed = true;
            }
        });
    });
    changed
}
