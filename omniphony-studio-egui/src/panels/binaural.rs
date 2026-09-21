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
use crate::ui::group::Group;
use crate::ui::{theme, widgets};

/// HRTF sources, in the select's order.
const HRIR_SOURCES: &[(&str, &str)] = &[
    ("saf", "binaural.hrtfSource.kemar"),
    ("synthetic", "binaural.hrtfSource.synthetic"),
    ("pinna", "binaural.hrtfSource.pinna"),
    ("prtf", "binaural.hrtfSource.prtf"),
    ("sofa", "binaural.hrtfSource.sofa"),
    ("brir", "binaural.hrtfSource.brir"),
];

/// Which measured head orientations of a BRIR stay resident, as the
/// renderer reports it (`auto` follows the head-tracking address).
const BRIR_TRACKING: &[(&str, &str)] = &[
    ("auto", "binaural.brirHeadTracking.auto"),
    ("on", "binaural.brirHeadTracking.on"),
    ("off", "binaural.brirHeadTracking.off"),
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

/// Which binaural path the renderer runs, as the state describes it. Each
/// path consumes a different set of the tab's fields (see `binaural_tab`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum BinauralPath {
    /// Every object through its own HRTF pair.
    Direct,
    /// The virtual speaker layout, each speaker through an HRTF pair.
    Cascaded,
    /// The virtual speaker layout through a measured room (a `brir` source,
    /// which forces the virtual-speaker path).
    Brir,
}

impl BinauralPath {
    fn of(doc: Option<&serde_json::Value>) -> Self {
        if text(doc, &["hrirSource"]).as_deref() == Some("brir") {
            return Self::Brir;
        }
        match text(doc, &["modeEffective"])
            .or_else(|| text(doc, &["mode"]))
            .as_deref()
        {
            Some("cascaded") => Self::Cascaded,
            _ => Self::Direct,
        }
    }
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
            let live = self.host.read();
            live.app.binaural.clone()
        };
        let doc = doc.as_ref();
        // What each group feeds, from the renderer's binaural stage:
        // - Direct and Cascaded run the HRTF stage, which reads every group
        //   (the source and its shaping, the distance cues, the synthetic
        //   room, the head pose);
        // - a measured room (BRIR) reads its own file options and the head
        //   pose only — the measurement is the distance, the reflections and
        //   the tail, so those groups are not drawn;
        // - with the output on the speakers nothing here renders: the tab
        //   stays editable, with a note saying so.
        let path = BinauralPath::of(doc);
        if text(doc, &["outputMode"]).as_deref() != Some("binaural") {
            widgets::note(ui, t("binaural.speakerOutputNote"));
        }
        self.hrtf_block(ui, doc, path);
        if path != BinauralPath::Brir {
            self.distance_block(ui, doc);
            self.room_block(ui, doc);
        }
        self.tracking_block(ui, doc);
    }

    fn hrtf_block(&mut self, ui: &mut Ui, doc: Option<&serde_json::Value>, path: BinauralPath) {
        let source = text(doc, &["hrirSource"]).unwrap_or_else(|| "saf".to_owned());
        let effective = text(doc, &["hrirEffective"]);
        // The source select and, for a SOFA file, its Browse button: only
        // reachable while a SOFA file is what the renderer would use — picking
        // one otherwise would download a file nothing plays.
        let mut chosen = source.clone();
        let mut browse = false;
        let mut browse_brir = false;
        Group::new("HRTF")
            .help("help.binaural.hrtf")
            .actions(|ui| {
                if source == "sofa" {
                    browse = ui
                        .button(t("backend.file.browse"))
                        .on_hover_text(t("binaural.sofaBrowseTitle"))
                        .clicked();
                }
                if source == "brir" {
                    // A room response is a local file of the renderer's:
                    // the native picker, not the HRTF database browser.
                    browse_brir = ui
                        .button(t("backend.file.browse"))
                        .on_hover_text(t("binaural.brirBrowseTitle"))
                        .clicked();
                }
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
            })
            .show(ui, |ui| {
                self.hrtf_rows(ui, doc, path, &source, effective.as_deref())
            });
        if chosen != source {
            self.send_hrir_source(&chosen);
        }
        if browse {
            self.open_sofa_browser();
        }
        if browse_brir {
            self.pick_files(
                ui.ctx(),
                crate::ui::file_dialogs::Purpose::Brir,
                &["sofa".to_owned()],
            );
        }
    }

    /// The HRTF group's inset: what was loaded, the EQ, the head, the update
    /// lattice, and the parametric sources' own settings — or, for a measured
    /// room, its file, status and options alone: the EQ, the head radius and
    /// the lattice shape the HRTF stage, which a room response bypasses.
    fn hrtf_rows(
        &mut self,
        ui: &mut Ui,
        doc: Option<&serde_json::Value>,
        path: BinauralPath,
        source: &str,
        effective: Option<&str>,
    ) {
        if path == BinauralPath::Brir {
            // A room response has its own status: the HRIR grid's effective
            // source is the direct path's KEMAR meanwhile, not a fallback.
            self.brir_rows(ui, doc);
            widgets::note(ui, t("binaural.brirMeasuredNote"));
            return;
        }
        // The renderer says what it actually loaded; a fallback means the
        // requested source did not work.
        if let Some(effective) = effective
            && effective != source
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
            let live = self.host.read();
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

    /// The BRIR source's inset: the file, what the renderer holds of it (or
    /// why it holds nothing), and the load options.
    fn brir_rows(&mut self, ui: &mut Ui, doc: Option<&serde_json::Value>) {
        let small = |line: String, color: egui::Color32| {
            RichText::new(line)
                .size(theme::FONT_SIZE_SMALL)
                .color(color)
        };
        let path = text(doc, &["brirSofaPath"]);
        match &path {
            Some(path) => {
                let name = path.rsplit(['/', '\\']).next().unwrap_or(path).to_owned();
                ui.label(small(format!("File: {name}"), theme::TEXT_MUTED))
                    .on_hover_text(path);
            }
            None => {
                ui.label(small(t("binaural.brirNoFile").to_owned(), theme::WARN));
            }
        }
        let status = doc.and_then(|d| d.get("brir"));
        let loaded = status
            .and_then(|b| b.get("loaded"))
            .filter(|v| !v.is_null());
        if let Some(error) = text(doc, &["brir", "error"]) {
            ui.label(small(t("binaural.brirError").to_owned(), theme::WARN))
                .on_hover_text(error);
        } else if let Some(loaded) = loaded {
            let count = |key: &str| loaded.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
            let conventions = loaded
                .get("conventions")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("BRIR")
                .to_owned();
            let (taps, rate) = (count("maxTaps"), count("sampleRate"));
            let seconds = if rate > 0 {
                taps as f64 / rate as f64
            } else {
                0.0
            };
            let line = tf(
                "binaural.brirLoaded",
                &[
                    ("conventions", conventions.as_str()),
                    ("emitters", &count("emitters").to_string()),
                    ("orientations", &count("orientations").to_string()),
                    ("seconds", &format!("{seconds:.2}")),
                    (
                        "mb",
                        &format!("{:.0}", count("bytes") as f64 / (1024.0 * 1024.0)),
                    ),
                ],
            );
            ui.label(small(line, theme::TEXT_MUTED));
        } else if path.is_some() {
            ui.label(small(
                t("binaural.brirLoading").to_owned(),
                theme::TEXT_MUTED,
            ));
        }

        // Load options: a change reloads the set on the renderer.
        let tracking = match status.and_then(|b| b.get("headTracking")) {
            Some(serde_json::Value::Bool(true)) => "on",
            Some(serde_json::Value::Bool(false)) => "off",
            _ => "auto",
        };
        let mut chosen = tracking;
        widgets::label_row_help(
            ui,
            t("binaural.brirHeadTracking"),
            "help.binaural.brirHeadTracking",
            |ui| {
                widgets::bounded_combo(ui, 160.0, |ui, w| {
                    egui::ComboBox::from_id_salt("brir-head-tracking")
                        .selected_text(t(BRIR_TRACKING
                            .iter()
                            .find(|(id, _)| *id == tracking)
                            .map(|(_, key)| *key)
                            .unwrap_or("binaural.brirHeadTracking.auto")))
                        .width(w)
                        .truncate()
                        .show_ui(ui, |ui| {
                            for (id, key) in BRIR_TRACKING {
                                ui.selectable_value(&mut chosen, *id, t(key));
                            }
                        })
                });
            },
        );
        if chosen != tracking {
            cmd::control_brir_head_tracking(
                &self.host,
                match chosen {
                    "on" => Some(true),
                    "off" => Some(false),
                    _ => None,
                },
            );
        }
        let mut max_length = number(doc, &["brir", "maxLengthS"], 2.0) as f32;
        if widgets::value_slider_help(
            ui,
            t("binaural.brirMaxLength"),
            "help.binaural.brirMaxLength",
            &mut max_length,
            0.0..=5.0,
            0.1,
            |v| {
                if v <= 0.0 {
                    t("binaural.brirWhole").to_owned()
                } else {
                    format!("{v:.1} s")
                }
            },
        ) {
            cmd::control_brir_max_length(&self.host, max_length);
        }
        let mut floor = number(doc, &["brir", "tailFloorDb"], 60.0) as f32;
        if widgets::value_slider_help(
            ui,
            t("binaural.brirTailFloor"),
            "help.binaural.brirTailFloor",
            &mut floor,
            30.0..=90.0,
            1.0,
            |v| format!("{v:.0} dB"),
        ) {
            cmd::control_brir_tail_floor(&self.host, floor);
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
        Group::new(t("binaural.distanceTitle")).show(ui, |ui| self.distance_rows(ui, doc));
    }

    fn distance_rows(&mut self, ui: &mut Ui, doc: Option<&serde_json::Value>) {
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
        Group::new(t("binaural.roomTitle")).show(ui, |ui| self.room_rows(ui, doc));
    }

    /// Early reflections and their parameters while they are on, then the
    /// late reverb and its own.
    fn room_rows(&mut self, ui: &mut Ui, doc: Option<&serde_json::Value>) {
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
        let step = number(doc, &["tracking", "calibrationStep"], 0.0) as usize;
        let (mut calibrate, mut recenter) = (false, false);
        Group::new(t("binaural.headTrackingTitle"))
            .help("help.binaural.headTracking")
            .actions(|ui| {
                calibrate = ui
                    .button(t("binaural.calibrateAxes"))
                    .on_hover_text(t("help.binaural.calibrateAxes"))
                    .clicked();
                recenter = ui.button(t("binaural.recenter")).clicked();
            })
            .show(ui, |ui| self.tracking_rows(ui, doc, step));
        if calibrate {
            let axis = CALIBRATION_STEPS.get(step).copied().unwrap_or("front");
            cmd::control_head_calibrate(&self.host, axis.to_owned());
        }
        if recenter {
            cmd::control_head_recenter(&self.host);
        }
    }

    fn tracking_rows(&mut self, ui: &mut Ui, doc: Option<&serde_json::Value>, step: usize) {
        let calibrated = flag(doc, &["tracking", "axesCalibrated"], false);
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
        let edited = widgets::label_row_help(
            ui,
            t("binaural.oscAddressLabel"),
            "help.binaural.oscAddress",
            |ui| {
                self.head_address_edit.show(
                    ui,
                    "tracking-address",
                    &address,
                    "/android/rotationvector",
                    170.0,
                    true,
                )
            },
        );
        if let Some(address) = edited {
            cmd::control_head_tracking_address(&self.host, address);
        }

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
        let pose = self.host.read().head_pose;
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
