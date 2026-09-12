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
use crate::host::commands::SharedState;
use crate::host::commands::binaural as cmd;
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
        widgets::label_row_help(
            ui,
            RichText::new("HRTF")
                .size(theme::FONT_SIZE)
                .color(theme::TEXT_STRONG),
            "help.binaural.hrtf",
            |ui| {
                let mut chosen = source.clone();
                widgets::bounded_combo(ui, 160.0, |ui, w| {
                    egui::ComboBox::from_id_salt("hrir-source")
                        .selected_text(t(HRIR_SOURCES
                            .iter()
                            .find(|(id, _)| *id == source)
                            .map(|(_, key)| *key)
                            .unwrap_or("binaural.hrtfSource.kemar")))
                        .width(w)
                        .truncate()
                        .show_ui(ui, |ui| {
                            for (id, key) in HRIR_SOURCES {
                                ui.selectable_value(&mut chosen, (*id).to_owned(), t(key));
                            }
                        })
                });
                if chosen != source {
                    self.send_hrir_source(&chosen);
                }
                // The browser is only reachable while a SOFA file is what the
                // renderer would use — picking one otherwise would download a
                // file nothing plays.
                if source == "sofa"
                    && ui
                        .button(t("backend.file.browse"))
                        .on_hover_text(t("binaural.sofaBrowseTitle"))
                        .clicked()
                {
                    self.open_sofa_browser();
                }
            },
        );

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
            // The path is its own field: `hrirSource` is the bare word "sofa"
            // once the renderer has parsed the `sofa:<path>` control, so the
            // file name has to come from `hrtfSofaPath`.
            let path = text(doc, &["hrtfSofaPath"]);
            match path {
                Some(path) => {
                    let name = path.rsplit(['/', '\\']).next().unwrap_or(&path).to_owned();
                    ui.label(
                        RichText::new(format!("File: {name}"))
                            .size(theme::FONT_SIZE_SMALL)
                            .color(theme::TEXT_MUTED),
                    )
                    .on_hover_text(path);
                }
                None => {
                    ui.label(
                        RichText::new(
                            "No SOFA file selected — using the embedded KEMAR until you pick \
                             one (Browse…).",
                        )
                        .size(theme::FONT_SIZE_SMALL)
                        .color(theme::WARN),
                    );
                }
            }
        }

        let mut eq = flag(doc, &["diffuseFieldEq"], false);
        if widgets::switch_row_help(
            ui,
            t("binaural.diffuseFieldEq"),
            "help.binaural.diffuseFieldEq",
            &mut eq,
        ) {
            cmd::control_binaural_diffuse_field_eq(&self.host, i32::from(eq));
        }

        // The head radius travels in metres; the slider is in centimetres.
        let mut radius_cm = (number(doc, &["headRadiusM"], 0.0875) * 100.0) as f32;
        if widgets::value_slider_help(
            ui,
            t("binaural.headRadius"),
            "help.binaural.headRadius",
            &mut radius_cm,
            5.0..=15.0,
            0.1,
            |v| format!("{v:.1}"),
        ) {
            cmd::control_binaural_head_radius(&self.host, radius_cm / 100.0);
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
        widgets::label_row_help(
            ui,
            t("binaural.hrirUpdateLatticeLabel"),
            "help.hrirUpdateLattice",
            |ui| {
                widgets::bounded_combo(ui, 140.0, |ui, w| {
                    egui::ComboBox::from_id_salt("hrir-lattice")
                        .selected_text(t(lattices
                            .iter()
                            .find(|(id, _)| *id == lattice)
                            .map(|(_, key)| *key)
                            .unwrap_or("binaural.hrirLattice.balanced")))
                        .width(w)
                        .truncate()
                        .show_ui(ui, |ui| {
                            for (id, key) in lattices {
                                ui.selectable_value(&mut chosen, id.to_owned(), t(key));
                            }
                        })
                });
            },
        );
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
            widgets::label_row_help(
                ui,
                t("binaural.pinnaPreset"),
                "help.binaural.pinnaPreset",
                |ui| {
                    widgets::bounded_combo(ui, 120.0, |ui, w| {
                        egui::ComboBox::from_id_salt("pinna-preset")
                            .selected_text(t(presets
                                .iter()
                                .find(|(id, _)| *id == current)
                                .map(|(_, key)| *key)
                                .unwrap_or("binaural.pinnaPreset.pbnh")))
                            .width(w)
                            .truncate()
                            .show_ui(ui, |ui| {
                                for (id, key) in presets {
                                    ui.selectable_value(&mut chosen, id.to_owned(), t(key));
                                }
                            })
                    });
                },
            );
            let mut changed = chosen != current;
            self.pinna_preset = chosen;
            let mut d_scale = self.pinna_d_scale;
            changed |= widgets::value_slider_help(
                ui,
                t("binaural.pinnaDScale"),
                "help.binaural.pinnaDScale",
                &mut d_scale,
                50.0..=150.0,
                5.0,
                |v| format!("{v:.0}"),
            );
            self.pinna_d_scale = d_scale;
            let mut depth = self.pinna_depth;
            changed |= widgets::value_slider_help(
                ui,
                t("binaural.pinnaDepth"),
                "help.binaural.pinnaDepth",
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
            let mut changed = widgets::value_slider_help(
                ui,
                t("binaural.prtfDepth"),
                "help.binaural.prtfDepth",
                &mut depth,
                0.0..=100.0,
                5.0,
                |v| format!("{v:.0}"),
            );
            self.prtf_depth = depth;
            let mut freq_scale = self.prtf_freq_scale;
            changed |= widgets::value_slider_help(
                ui,
                t("binaural.prtfFreqScale"),
                "help.binaural.prtfFreqScale",
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
        cmd::control_hrir_source(&self.host, value);
    }

    fn distance_block(&mut self, ui: &mut Ui, doc: Option<&serde_json::Value>) {
        ui.add_space(4.0);
        ui.label(
            RichText::new(t("binaural.distanceTitle"))
                .size(theme::FONT_SIZE)
                .color(theme::TEXT_STRONG),
        );
        let mut scale = number(doc, &["unitScaleM"], 1.0) as f32;
        if widgets::value_slider_help(
            ui,
            t("binaural.distanceScale"),
            "help.binaural.distanceScale",
            &mut scale,
            0.1..=10.0,
            0.1,
            |v| format!("{v:.1}"),
        ) {
            cmd::control_binaural_unit_scale(&self.host, scale);
        }
        let mut air = flag(doc, &["airAbsorption"], true);
        if widgets::switch_row_help(
            ui,
            t("binaural.airAbsorption"),
            "help.binaural.airAbsorption",
            &mut air,
        ) {
            cmd::control_binaural_air_absorption(&self.host, i32::from(air));
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
        if widgets::switch_row_help(
            ui,
            t("binaural.earlyReflections"),
            "help.binaural.earlyReflections",
            &mut reflections,
        ) {
            cmd::control_binaural_reflections_enabled(&self.host, i32::from(reflections));
        }
        if reflections {
            let mut level = number(doc, &["reflections", "level"], 0.5) as f32;
            if widgets::value_slider_help(
                ui,
                t("binaural.reflectionLevel"),
                "help.binaural.reflectionLevel",
                &mut level,
                0.0..=1.0,
                0.01,
                |v| format!("{v:.2}"),
            ) {
                cmd::control_binaural_reflections_level(&self.host, level);
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
            for (index, (axis, label)) in [("width", "W"), ("depth", "D"), ("height", "H")]
                .into_iter()
                .enumerate()
            {
                let mut value = room[index] as f32;
                // One help for the three sliders, as the web has one label for
                // them; each slider opens its own card, under itself.
                let help = widgets::Help::text(
                    ("binaural-room", axis),
                    crate::i18n::lookup("help.binaural.room").unwrap_or(""),
                );
                if widgets::value_slider_help(
                    ui,
                    &format!("{} {label}", t("binaural.roomDims")),
                    help,
                    &mut value,
                    1.0..=20.0,
                    0.1,
                    |v| format!("{v:.1}"),
                ) {
                    cmd::control_binaural_reflections_room(&self.host, axis.to_owned(), value);
                }
            }
            // The cutoff travels in hertz; the slider is in kilohertz.
            let mut cutoff_khz =
                (number(doc, &["reflections", "wallCutoffHz"], 6000.0) / 1000.0) as f32;
            if widgets::value_slider_help(
                ui,
                t("binaural.wallDamping"),
                "help.binaural.wallDamping",
                &mut cutoff_khz,
                1.0..=20.0,
                0.5,
                |v| format!("{v:.1}"),
            ) {
                cmd::control_binaural_reflections_wall_cutoff(&self.host, cutoff_khz * 1000.0);
            }
        }

        ui.separator();
        let mut reverb = flag(doc, &["reverb", "enabled"], false);
        if widgets::switch_row_help(
            ui,
            t("binaural.lateReverb"),
            "help.binaural.lateReverb",
            &mut reverb,
        ) {
            cmd::control_binaural_reverb_enabled(&self.host, i32::from(reverb));
        }
        if !reverb {
            return;
        }
        let mut level = number(doc, &["reverb", "level"], 0.25) as f32;
        if widgets::value_slider_help(
            ui,
            t("binaural.reverbLevel"),
            "help.binaural.reverbLevel",
            &mut level,
            0.0..=1.0,
            0.01,
            |v| format!("{v:.2}"),
        ) {
            cmd::control_binaural_reverb_level(&self.host, level);
        }
        let mut rt60 = number(doc, &["reverb", "rt60S"], 0.35) as f32;
        if widgets::value_slider_help(
            ui,
            t("binaural.rt60"),
            "help.binaural.rt60",
            &mut rt60,
            0.1..=1.5,
            0.05,
            |v| format!("{v:.2}"),
        ) {
            cmd::control_binaural_reverb_rt60(&self.host, rt60);
        }
        let mut size = number(doc, &["reverb", "size"], 1.0) as f32;
        if widgets::value_slider_help(
            ui,
            t("binaural.reverbSize"),
            "help.binaural.reverbSize",
            &mut size,
            0.5..=2.0,
            0.05,
            |v| format!("{v:.2}"),
        ) {
            cmd::control_binaural_reverb_size(&self.host, size);
        }
        // The decay ratios are edited in octaves so unity sits mid-slider.
        self.decay_ratio_row(
            ui,
            t("binaural.reverbBassDecay"),
            "help.binaural.reverbBassDecay",
            cmd::control_binaural_reverb_rt60_low_ratio,
            number(doc, &["reverb", "rt60LowRatio"], 1.0),
        );
        self.decay_ratio_row(
            ui,
            t("binaural.reverbTrebleDecay"),
            "help.binaural.reverbTrebleDecay",
            cmd::control_binaural_reverb_rt60_high_ratio,
            number(doc, &["reverb", "rt60HighRatio"], 1.0),
        );
    }

    /// A ratio slider whose scale is log2, so 1.0 is the centre. `send` is the
    /// command the new ratio goes to, one per band.
    fn decay_ratio_row(
        &mut self,
        ui: &mut Ui,
        label: &str,
        help: &str,
        send: fn(&SharedState, f32),
        ratio: f64,
    ) {
        let mut octaves = if ratio > 0.0 {
            ratio.log2() as f32
        } else {
            0.0
        };
        if widgets::value_slider_help(ui, label, help, &mut octaves, -2.0..=2.0, 0.1, |v| {
            format!("{:.2}", 2f32.powf(v))
        }) {
            send(&self.host, 2f32.powf(octaves));
        }
    }

    fn tracking_block(&mut self, ui: &mut Ui, doc: Option<&serde_json::Value>) {
        ui.add_space(4.0);
        let step = number(doc, &["tracking", "calibrationStep"], 0.0) as usize;
        let calibrated = flag(doc, &["tracking", "axesCalibrated"], false);
        widgets::label_row_help(
            ui,
            RichText::new(t("binaural.headTrackingTitle"))
                .size(theme::FONT_SIZE)
                .color(theme::TEXT_STRONG),
            "help.binaural.headTracking",
            |ui| {
                if ui
                    .button(t("binaural.calibrateAxes"))
                    .on_hover_text(t("help.binaural.calibrateAxes"))
                    .clicked()
                {
                    let axis = CALIBRATION_STEPS.get(step).copied().unwrap_or("front");
                    cmd::control_head_calibrate(&self.host, axis.to_owned());
                }
                if ui.button(t("binaural.recenter")).clicked() {
                    cmd::control_head_recenter(&self.host);
                }
            },
        );
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
        widgets::label_row_help(
            ui,
            t("binaural.oscAddressLabel"),
            "help.binaural.oscAddress",
            |ui| {
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut edited)
                            .desired_width(170.0)
                            .hint_text("/android/rotationvector"),
                    )
                    .lost_focus()
                    && edited.trim() != address
                {
                    cmd::control_head_tracking_address(&self.host, edited.trim().to_owned());
                }
            },
        );

        let format = text(doc, &["tracking", "format"]).unwrap_or_else(|| "auto".to_owned());
        let mut chosen = format.clone();
        widgets::label_row_help(
            ui,
            t("binaural.trackFormatLabel"),
            "help.binaural.trackFormat",
            |ui| {
                widgets::bounded_combo(ui, 140.0, |ui, w| {
                    egui::ComboBox::from_id_salt("track-format")
                        .selected_text(t(TRACK_FORMATS
                            .iter()
                            .find(|(id, _)| *id == format)
                            .map(|(_, key)| *key)
                            .unwrap_or("common.auto")))
                        .width(w)
                        .truncate()
                        .show_ui(ui, |ui| {
                            for (id, key) in TRACK_FORMATS {
                                ui.selectable_value(&mut chosen, (*id).to_owned(), t(key));
                            }
                        })
                });
            },
        );
        if chosen != format {
            cmd::control_head_tracking_format(&self.host, chosen.clone());
        }

        let mut smoothing = number(doc, &["tracking", "smoothing"], 0.2) as f32;
        if widgets::value_slider_help(
            ui,
            t("binaural.trackSmoothing"),
            "help.binaural.trackSmoothing",
            &mut smoothing,
            0.0..=0.99,
            0.01,
            |v| format!("{v:.2}"),
        ) {
            cmd::control_head_tracking_smoothing(&self.host, smoothing);
        }
        let mut invert = flag(doc, &["tracking", "invert"], false);
        if widgets::switch_row_help(
            ui,
            t("binaural.invertRotation"),
            "help.binaural.invertRotation",
            &mut invert,
        ) {
            cmd::control_head_tracking_invert(&self.host, i32::from(invert));
        }

        // The live pose, as the tracker reports it.
        let pose = self.live.lock().unwrap().head_pose;
        if let Some([w, x, y, z]) = pose {
            let (yaw, pitch, roll) = euler_degrees(w, x, y, z);
            ui.horizontal(|ui| {
                crate::ui::help::label(ui, t("binaural.pose"), "help.binaural.pose");
                ui.label(
                    RichText::new(format!("yaw {yaw:.0}°  pitch {pitch:.0}°  roll {roll:.0}°"))
                        .monospace()
                        .size(theme::FONT_SIZE_SMALL)
                        .color(theme::TEXT_MUTED),
                );
            });
            crate::ui::help::card(ui, "help.binaural.pose");
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
