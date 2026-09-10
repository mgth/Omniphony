//! The Binaural tab of the renderer panel (`controls/binaural.js`): the HRTF
//! source and its parametric variants, distance, the simulated listening
//! room, and head tracking.
//!
//! The renderer's `/omniphony/state/binaural` document is passed through as
//! JSON, so the panel reads it by key rather than through a typed mirror, the
//! way the web does. Nothing here is written optimistically: a slider moves
//! its own readout and sends, and the renderer's echo is what sticks.

use egui::{RichText, Ui};

use crate::app::StudioSpike;
use crate::i18n::{t, tf};
use crate::ui::{theme, widgets};

/// HRTF sources, in the select's order.
const HRIR_SOURCES: &[(&str, &str)] = &[
    ("saf", "binaural.hrtfSource.kemar"),
    ("synthetic", "binaural.hrtfSource.synthetic"),
    ("pinna", "binaural.hrtfSource.pinna"),
    ("prtf", "binaural.hrtfSource.prtf"),
    ("sofa", "binaural.hrtfSource.sofa"),
];

const TRACK_FORMATS: &[(&str, &str)] = &[
    ("auto", "common.auto"),
    ("quat", "binaural.trackFormat.quat"),
    ("rotvec", "binaural.trackFormat.rotvec"),
    ("euler", "binaural.trackFormat.euler"),
];

/// The three axes the calibration walks through.
const CALIBRATION_STEPS: &[&str] = &["front", "left", "up"];

/// Read a number out of the binaural document.
fn number(doc: Option<&serde_json::Value>, path: &[&str], fallback: f64) -> f64 {
    let mut cursor = doc;
    for key in path {
        cursor = cursor.and_then(|v| v.get(key));
    }
    cursor.and_then(|v| v.as_f64()).unwrap_or(fallback)
}

fn flag(doc: Option<&serde_json::Value>, path: &[&str], fallback: bool) -> bool {
    let mut cursor = doc;
    for key in path {
        cursor = cursor.and_then(|v| v.get(key));
    }
    cursor.and_then(|v| v.as_bool()).unwrap_or(fallback)
}

fn text(doc: Option<&serde_json::Value>, path: &[&str]) -> Option<String> {
    let mut cursor = doc;
    for key in path {
        cursor = cursor.and_then(|v| v.get(key));
    }
    cursor
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
        .filter(|s| !s.is_empty())
}

impl StudioSpike {
    pub(crate) fn binaural_tab(&mut self, ui: &mut Ui) {
        let doc = {
            let live = self.live.lock().unwrap();
            live.app.binaural.clone()
        };
        let doc = doc.as_ref();
        self.hrtf_block(ui, doc);
        self.distance_block(ui, doc);
        self.room_block(ui, doc);
        self.tracking_block(ui, doc);
    }

    fn hrtf_block(&mut self, ui: &mut Ui, doc: Option<&serde_json::Value>) {
        let source = text(doc, &["hrirSource"]).unwrap_or_else(|| "saf".to_owned());
        let effective = text(doc, &["hrirEffective"]);
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("HRTF")
                    .size(theme::FONT_SIZE)
                    .color(theme::TEXT_STRONG),
            );
            widgets::help(ui, "help.binaural.hrtf");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut chosen = source.clone();
                egui::ComboBox::from_id_salt("hrir-source")
                    .selected_text(t(HRIR_SOURCES
                        .iter()
                        .find(|(id, _)| *id == source)
                        .map(|(_, key)| *key)
                        .unwrap_or("binaural.hrtfSource.kemar")))
                    .width(160.0)
                    .show_ui(ui, |ui| {
                        for (id, key) in HRIR_SOURCES {
                            ui.selectable_value(&mut chosen, (*id).to_owned(), t(key));
                        }
                    });
                if chosen != source {
                    self.send_hrir_source(&chosen);
                }
            });
        });

        // The renderer says what it actually loaded; a fallback means the
        // requested source did not work.
        if let Some(effective) = &effective
            && *effective != source
        {
            let line = tf("binaural.hrtfFallback", &[("effective", effective)]);
            let mut label = ui.label(
                RichText::new(line)
                    .size(theme::FONT_SIZE_SMALL)
                    .color(theme::WARN),
            );
            if let Some(error) = text(doc, &["hrirError"]) {
                label = label.on_hover_text(error);
            }
            let _ = label;
        } else if source == "sofa" {
            let file = text(doc, &["hrirSource"]).and_then(|s| {
                s.strip_prefix("sofa:")
                    .map(|path| path.rsplit('/').next().unwrap_or(path).to_owned())
            });
            match file {
                Some(file) => widgets::note(ui, &format!("File: {file}")),
                None => {
                    ui.label(
                        RichText::new(
                            "No SOFA file selected — using the embedded KEMAR until you pick \
                             one. The SOFA browser is not ported yet.",
                        )
                        .size(theme::FONT_SIZE_SMALL)
                        .color(theme::WARN),
                    );
                }
            }
        }

        let mut eq = flag(doc, &["diffuseFieldEq"], false);
        if widgets::switch_row(ui, t("binaural.diffuseFieldEq"), &mut eq) {
            self.ctl.send_int(
                "/omniphony/control/binaural/diffuse_field_eq",
                i32::from(eq),
            );
        }

        // The head radius travels in metres; the slider is in centimetres.
        let mut radius_cm = (number(doc, &["headRadiusM"], 0.0875) * 100.0) as f32;
        if widgets::value_slider(
            ui,
            t("binaural.headRadius"),
            &mut radius_cm,
            5.0..=15.0,
            0.1,
            |v| format!("{v:.1}"),
        ) {
            self.ctl
                .send_float("/omniphony/control/binaural/head_radius", radius_cm / 100.0);
        }

        let lattice = {
            let live = self.live.lock().unwrap();
            live.option_str("hrir_update_lattice")
                .unwrap_or_else(|| "balanced".to_owned())
        };
        let lattices = [
            ("exact", "binaural.hrirLattice.exact"),
            ("fine", "binaural.hrirLattice.fine"),
            ("balanced", "binaural.hrirLattice.balanced"),
            ("coarse", "binaural.hrirLattice.coarse"),
        ];
        let mut chosen = lattice.clone();
        ui.horizontal(|ui| {
            ui.label(t("binaural.hrirUpdateLatticeLabel"));
            widgets::help(ui, "help.hrirUpdateLattice");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::ComboBox::from_id_salt("hrir-lattice")
                    .selected_text(t(lattices
                        .iter()
                        .find(|(id, _)| *id == lattice)
                        .map(|(_, key)| *key)
                        .unwrap_or("binaural.hrirLattice.balanced")))
                    .width(140.0)
                    .show_ui(ui, |ui| {
                        for (id, key) in lattices {
                            ui.selectable_value(&mut chosen, id.to_owned(), t(key));
                        }
                    });
            });
        });
        if chosen != lattice {
            self.set_option("hrir_update_lattice", serde_json::json!(chosen));
        }

        // The parametric sources carry their settings inside the source
        // string, so the renderer never echoes them back: this panel owns
        // them, like the web does.
        if source == "pinna" {
            let presets = [
                ("pbnh", "binaural.pinnaPreset.pbnh"),
                ("rd", "binaural.pinnaPreset.rd"),
            ];
            let current = self.pinna_preset.clone();
            let mut chosen = current.clone();
            ui.horizontal(|ui| {
                ui.label(t("binaural.pinnaPreset"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    egui::ComboBox::from_id_salt("pinna-preset")
                        .selected_text(t(presets
                            .iter()
                            .find(|(id, _)| *id == current)
                            .map(|(_, key)| *key)
                            .unwrap_or("binaural.pinnaPreset.pbnh")))
                        .width(120.0)
                        .show_ui(ui, |ui| {
                            for (id, key) in presets {
                                ui.selectable_value(&mut chosen, id.to_owned(), t(key));
                            }
                        });
                });
            });
            let mut changed = chosen != current;
            self.pinna_preset = chosen;
            let mut d_scale = self.pinna_d_scale;
            changed |= widgets::value_slider(
                ui,
                t("binaural.pinnaDScale"),
                &mut d_scale,
                50.0..=150.0,
                5.0,
                |v| format!("{v:.0}"),
            );
            self.pinna_d_scale = d_scale;
            let mut depth = self.pinna_depth;
            changed |= widgets::value_slider(
                ui,
                t("binaural.pinnaDepth"),
                &mut depth,
                0.0..=100.0,
                5.0,
                |v| format!("{v:.0}"),
            );
            self.pinna_depth = depth;
            if changed {
                self.send_hrir_source("pinna");
            }
        } else if source == "prtf" {
            let mut depth = self.prtf_depth;
            let mut changed = widgets::value_slider(
                ui,
                t("binaural.prtfDepth"),
                &mut depth,
                0.0..=100.0,
                5.0,
                |v| format!("{v:.0}"),
            );
            self.prtf_depth = depth;
            let mut freq_scale = self.prtf_freq_scale;
            changed |= widgets::value_slider(
                ui,
                t("binaural.prtfFreqScale"),
                &mut freq_scale,
                50.0..=150.0,
                5.0,
                |v| format!("{v:.0}"),
            );
            self.prtf_freq_scale = freq_scale;
            if changed {
                self.send_hrir_source("prtf");
            }
        }
    }

    /// The parametric sources encode their parameters in the source string.
    fn send_hrir_source(&mut self, source: &str) {
        let value = match source {
            "pinna" => format!(
                "pinna:{}:{}:{}",
                self.pinna_preset,
                self.pinna_d_scale.round() as i64,
                self.pinna_depth.round() as i64
            ),
            "prtf" => format!(
                "prtf:{}:{}",
                self.prtf_freq_scale.round() as i64,
                self.prtf_depth.round() as i64
            ),
            other => other.to_owned(),
        };
        self.ctl
            .send_string("/omniphony/control/binaural/hrir_source", &value);
    }

    fn distance_block(&mut self, ui: &mut Ui, doc: Option<&serde_json::Value>) {
        ui.add_space(4.0);
        ui.label(
            RichText::new(t("binaural.distanceTitle"))
                .size(theme::FONT_SIZE)
                .color(theme::TEXT_STRONG),
        );
        let mut scale = number(doc, &["unitScaleM"], 1.0) as f32;
        if widgets::value_slider(
            ui,
            t("binaural.distanceScale"),
            &mut scale,
            0.1..=10.0,
            0.1,
            |v| format!("{v:.1}"),
        ) {
            self.ctl
                .send_float("/omniphony/control/binaural/unit_scale", scale);
        }
        let mut air = flag(doc, &["airAbsorption"], true);
        if widgets::switch_row(ui, t("binaural.airAbsorption"), &mut air) {
            self.ctl
                .send_int("/omniphony/control/binaural/air_absorption", i32::from(air));
        }
    }

    fn room_block(&mut self, ui: &mut Ui, doc: Option<&serde_json::Value>) {
        ui.add_space(4.0);
        ui.label(
            RichText::new(t("binaural.roomTitle"))
                .size(theme::FONT_SIZE)
                .color(theme::TEXT_STRONG),
        );

        // Early reflections, then their parameters while they are on.
        let mut reflections = flag(doc, &["reflections", "enabled"], false);
        if widgets::switch_row(ui, t("binaural.earlyReflections"), &mut reflections) {
            self.ctl.send_int(
                "/omniphony/control/binaural/reflections/enabled",
                i32::from(reflections),
            );
        }
        if reflections {
            let mut level = number(doc, &["reflections", "level"], 0.5) as f32;
            if widgets::value_slider(
                ui,
                t("binaural.reflectionLevel"),
                &mut level,
                0.0..=1.0,
                0.01,
                |v| format!("{v:.2}"),
            ) {
                self.ctl
                    .send_float("/omniphony/control/binaural/reflections/level", level);
            }
            let room = doc
                .and_then(|d| d.get("reflections"))
                .and_then(|r| r.get("roomM"))
                .and_then(|v| v.as_array())
                .map(|a| {
                    let get = |i: usize, fallback: f64| {
                        a.get(i).and_then(|v| v.as_f64()).unwrap_or(fallback)
                    };
                    [get(0, 4.0), get(1, 5.0), get(2, 2.7)]
                })
                .unwrap_or([4.0, 5.0, 2.7]);
            for (index, (axis, label)) in [
                ("room_width", "W"),
                ("room_depth", "D"),
                ("room_height", "H"),
            ]
            .into_iter()
            .enumerate()
            {
                let mut value = room[index] as f32;
                if widgets::value_slider(
                    ui,
                    &format!("{} {label}", t("binaural.roomDims")),
                    &mut value,
                    1.0..=20.0,
                    0.1,
                    |v| format!("{v:.1}"),
                ) {
                    self.ctl.send_float(
                        &format!("/omniphony/control/binaural/reflections/{axis}"),
                        value,
                    );
                }
            }
            // The cutoff travels in hertz; the slider is in kilohertz.
            let mut cutoff_khz =
                (number(doc, &["reflections", "wallCutoffHz"], 6000.0) / 1000.0) as f32;
            if widgets::value_slider(
                ui,
                t("binaural.wallDamping"),
                &mut cutoff_khz,
                1.0..=20.0,
                0.5,
                |v| format!("{v:.1}"),
            ) {
                self.ctl.send_float(
                    "/omniphony/control/binaural/reflections/wall_cutoff",
                    cutoff_khz * 1000.0,
                );
            }
        }

        ui.separator();
        let mut reverb = flag(doc, &["reverb", "enabled"], false);
        if widgets::switch_row(ui, t("binaural.lateReverb"), &mut reverb) {
            self.ctl.send_int(
                "/omniphony/control/binaural/reverb/enabled",
                i32::from(reverb),
            );
        }
        if !reverb {
            return;
        }
        let mut level = number(doc, &["reverb", "level"], 0.25) as f32;
        if widgets::value_slider(
            ui,
            t("binaural.reverbLevel"),
            &mut level,
            0.0..=1.0,
            0.01,
            |v| format!("{v:.2}"),
        ) {
            self.ctl
                .send_float("/omniphony/control/binaural/reverb/level", level);
        }
        let mut rt60 = number(doc, &["reverb", "rt60S"], 0.35) as f32;
        if widgets::value_slider(ui, t("binaural.rt60"), &mut rt60, 0.1..=1.5, 0.05, |v| {
            format!("{v:.2}")
        }) {
            self.ctl
                .send_float("/omniphony/control/binaural/reverb/rt60", rt60);
        }
        let mut size = number(doc, &["reverb", "size"], 1.0) as f32;
        if widgets::value_slider(
            ui,
            t("binaural.reverbSize"),
            &mut size,
            0.5..=2.0,
            0.05,
            |v| format!("{v:.2}"),
        ) {
            self.ctl
                .send_float("/omniphony/control/binaural/reverb/size", size);
        }
        // The decay ratios are edited in octaves so unity sits mid-slider.
        self.decay_ratio_row(
            ui,
            t("binaural.reverbBassDecay"),
            "rt60_low_ratio",
            number(doc, &["reverb", "rt60LowRatio"], 1.0),
        );
        self.decay_ratio_row(
            ui,
            t("binaural.reverbTrebleDecay"),
            "rt60_high_ratio",
            number(doc, &["reverb", "rt60HighRatio"], 1.0),
        );
    }

    /// A ratio slider whose scale is log2, so 1.0 is the centre.
    fn decay_ratio_row(&mut self, ui: &mut Ui, label: &str, address: &str, ratio: f64) {
        let mut octaves = if ratio > 0.0 {
            ratio.log2() as f32
        } else {
            0.0
        };
        if widgets::value_slider(ui, label, &mut octaves, -2.0..=2.0, 0.1, |v| {
            format!("{:.2}", 2f32.powf(v))
        }) {
            self.ctl.send_float(
                &format!("/omniphony/control/binaural/reverb/{address}"),
                2f32.powf(octaves),
            );
        }
    }

    fn tracking_block(&mut self, ui: &mut Ui, doc: Option<&serde_json::Value>) {
        ui.add_space(4.0);
        let step = number(doc, &["tracking", "calibrationStep"], 0.0) as usize;
        let calibrated = flag(doc, &["tracking", "axesCalibrated"], false);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(t("binaural.headTrackingTitle"))
                    .size(theme::FONT_SIZE)
                    .color(theme::TEXT_STRONG),
            );
            widgets::help(ui, "help.binaural.headTracking");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button(t("binaural.calibrateAxes"))
                    .on_hover_text(t("help.binaural.calibrateAxes"))
                    .clicked()
                {
                    let axis = CALIBRATION_STEPS.get(step).copied().unwrap_or("front");
                    self.ctl
                        .send_string("/omniphony/control/head/calibrate", axis);
                }
                if ui.button(t("binaural.recenter")).clicked() {
                    self.ctl.send_int("/omniphony/control/head/recenter", 1);
                }
            });
        });
        // Where the calibration is up to, in the user's terms.
        if calibrated {
            widgets::note(ui, t("binaural.calibrateDone"));
        } else if step == 1 {
            ui.label(
                RichText::new(t("binaural.calibratePromptLeft"))
                    .size(theme::FONT_SIZE_SMALL)
                    .color(theme::WARN),
            );
        } else if step == 2 {
            ui.label(
                RichText::new(t("binaural.calibratePromptUp"))
                    .size(theme::FONT_SIZE_SMALL)
                    .color(theme::WARN),
            );
        }

        let address = text(doc, &["tracking", "address"]).unwrap_or_default();
        let mut edited = address.clone();
        ui.horizontal(|ui| {
            ui.label(t("binaural.oscAddressLabel"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut edited)
                            .desired_width(170.0)
                            .hint_text("/android/rotationvector"),
                    )
                    .lost_focus()
                    && edited.trim() != address
                {
                    self.ctl
                        .send_string("/omniphony/control/head/tracking/address", edited.trim());
                }
            });
        });

        let format = text(doc, &["tracking", "format"]).unwrap_or_else(|| "auto".to_owned());
        let mut chosen = format.clone();
        ui.horizontal(|ui| {
            ui.label(t("binaural.trackFormatLabel"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::ComboBox::from_id_salt("track-format")
                    .selected_text(t(TRACK_FORMATS
                        .iter()
                        .find(|(id, _)| *id == format)
                        .map(|(_, key)| *key)
                        .unwrap_or("common.auto")))
                    .width(140.0)
                    .show_ui(ui, |ui| {
                        for (id, key) in TRACK_FORMATS {
                            ui.selectable_value(&mut chosen, (*id).to_owned(), t(key));
                        }
                    });
            });
        });
        if chosen != format {
            self.ctl
                .send_string("/omniphony/control/head/tracking/format", &chosen);
        }

        let mut smoothing = number(doc, &["tracking", "smoothing"], 0.2) as f32;
        if widgets::value_slider(
            ui,
            t("binaural.trackSmoothing"),
            &mut smoothing,
            0.0..=0.99,
            0.01,
            |v| format!("{v:.2}"),
        ) {
            self.ctl
                .send_float("/omniphony/control/head/tracking/smoothing", smoothing);
        }
        let mut invert = flag(doc, &["tracking", "invert"], false);
        if widgets::switch_row(ui, t("binaural.invertRotation"), &mut invert) {
            self.ctl
                .send_int("/omniphony/control/head/tracking/invert", i32::from(invert));
        }

        // The live pose, as the tracker reports it.
        let pose = self.live.lock().unwrap().head_pose;
        if let Some([w, x, y, z]) = pose {
            let (yaw, pitch, roll) = euler_degrees(w, x, y, z);
            ui.horizontal(|ui| {
                ui.label(t("binaural.pose"));
                ui.label(
                    RichText::new(format!("yaw {yaw:.0}°  pitch {pitch:.0}°  roll {roll:.0}°"))
                        .monospace()
                        .size(theme::FONT_SIZE_SMALL)
                        .color(theme::TEXT_MUTED),
                );
            });
        }
    }
}

/// The readout's angles, from the tracker's quaternion.
fn euler_degrees(w: f32, x: f32, y: f32, z: f32) -> (f32, f32, f32) {
    let yaw = (2.0 * (w * z + x * y)).atan2(1.0 - 2.0 * (y * y + z * z));
    let pitch = (2.0 * (w * x - z * y)).clamp(-1.0, 1.0).asin();
    let roll = (2.0 * (w * y + z * x)).atan2(1.0 - 2.0 * (x * x + y * y));
    (yaw.to_degrees(), pitch.to_degrees(), roll.to_degrees())
}
