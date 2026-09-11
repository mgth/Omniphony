//! Fixed-channel sources (`#twoDSourcesPanelRoot`, `controls/audio.js`): what
//! Omniphony does with a channel-based stream — where the rear channels go,
//! whether height objects are synthesised, and whether a phantom centre is
//! extracted.
//!
//! Every control here is a declared live option, so the panel sends
//! `/omniphony/control/option` and the renderer validates against its own
//! registry. The generator and phantom parameters come from the schemas the
//! renderer publishes.

use egui::Ui;

use crate::app::StudioSpike;
use crate::i18n::t;
use crate::ui::section::Section;
use crate::ui::widgets;

/// `PROCESSING_REASON_KEYS`: why a synthesis stage is or is not running.
fn reason_text(reason: &str) -> &'static str {
    match reason {
        "active" => t("twoDSources.status.active"),
        "off" => t("twoDSources.status.off"),
        "master_off" => t("twoDSources.status.masterOff"),
        "object_stream" => t("twoDSources.status.objectStream"),
        "input_has_height" => t("twoDSources.status.inputHasHeight"),
        "output_has_no_height" => t("twoDSources.status.outputHasNoHeight"),
        "insufficient_channels" => t("twoDSources.status.insufficientChannels"),
        _ => t("twoDSources.status.noStream"),
    }
}

impl StudioSpike {
    pub(crate) fn sources_2d_section(&mut self, ui: &mut Ui) {
        let (placement, synthetic, generator, phantom, processing, generators, phantom_schema) = {
            let live = self.live.lock().unwrap();
            (
                live.option_str("surround_placement")
                    .unwrap_or_else(|| "side".to_owned()),
                live.option_bool("synthetic_objects_enabled")
                    .unwrap_or(false),
                live.option_str("object_generator_id").unwrap_or_default(),
                live.option_str("phantom_extract_mode")
                    .unwrap_or_else(|| "off".to_owned()),
                live.app.live_options.fixed_channel_processing.clone(),
                live.object_generators_schema.clone(),
                live.phantom_schema.clone(),
            )
        };
        let summary = format!(
            "{} · {}",
            if placement == "back" {
                t("twoDSources.surroundBack")
            } else {
                t("twoDSources.surroundSide")
            },
            if synthetic {
                t("twoDSources.summary.syntheticEnabled")
            } else {
                t("twoDSources.summary.fixedOnly")
            }
        );
        Section::new("twoDSourcesSection", "section.twoDSources")
            .help("help.twoDSources")
            .summary(summary)
            .show(ui, |ui| {
                // What the renderer is doing with the current stream.
                let stream = processing
                    .as_ref()
                    .and_then(|p| p.get("stream"))
                    .and_then(|v| v.as_str());
                widgets::note(
                    ui,
                    match stream {
                        Some("fixed") => t("twoDSources.stream.fixed"),
                        Some("objects") => t("twoDSources.stream.objects"),
                        _ => t("twoDSources.stream.idle"),
                    },
                );

                widgets::label_row(ui, t("twoDSources.surroundLabel"), |ui| {
                    if let Some(picked) = widgets::toggle_buttons(
                        ui,
                        &placement,
                        &[
                            ("side".to_owned(), t("twoDSources.surroundSide")),
                            ("back".to_owned(), t("twoDSources.surroundBack")),
                        ],
                    ) {
                        self.set_option("surround_placement", serde_json::json!(picked));
                    }
                });

                let mut on = synthetic;
                if widgets::switch_row(ui, t("twoDSources.syntheticObjectsLabel"), &mut on) {
                    self.set_option("synthetic_objects_enabled", serde_json::json!(on));
                }
                widgets::note(
                    ui,
                    if synthetic {
                        t("twoDSources.syntheticConfigured")
                    } else {
                        t("twoDSources.fixedOnly")
                    },
                );

                // Height generator, then its declared parameters.
                self.generator_row(ui, &generator, generators.as_ref());
                let height_reason = effective_reason(
                    &generator != "none" && !generator.is_empty(),
                    synthetic,
                    processing.as_ref(),
                    "height",
                );
                widgets::note(ui, reason_text(&height_reason));
                if let Some(schema) = generators.as_ref() {
                    self.generator_params(ui, &generator, schema);
                }

                // Phantom extraction, then its own parameters.
                self.phantom_row(ui, &phantom);
                let phantom_reason =
                    effective_reason(phantom != "off", synthetic, processing.as_ref(), "phantom");
                widgets::note(ui, reason_text(&phantom_reason));
                if phantom != "off"
                    && let Some(schema) = phantom_schema.as_ref()
                {
                    self.phantom_params(ui, schema);
                }

                // The channel layout every fixed channel is placed by. The
                // editor for one channel opens from the objects list.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(t("virtualBed.reset")).clicked() {
                        self.reset_virtual_bed();
                    }
                });
            });
    }

    fn generator_row(&mut self, ui: &mut Ui, current: &str, schema: Option<&serde_json::Value>) {
        let options = generator_options(schema);
        let current = if current.is_empty() {
            "none".to_owned()
        } else {
            current.to_owned()
        };
        let mut chosen = current.clone();
        widgets::label_row_help(
            ui,
            t("twoDSources.objectGeneratorLabel"),
            "help.objectGenerator",
            |ui| {
                egui::ComboBox::from_id_salt("object-generator")
                    .selected_text(
                        options
                            .iter()
                            .find(|(id, _)| *id == current)
                            .map(|(_, label)| label.clone())
                            .unwrap_or_else(|| current.clone()),
                    )
                    .width(160.0)
                    .show_ui(ui, |ui| {
                        for (id, label) in &options {
                            ui.selectable_value(&mut chosen, id.clone(), label);
                        }
                    });
            },
        );
        if chosen != current {
            // The renderer drops the previous generator's overrides, so the
            // new one shows its declared defaults.
            self.live
                .lock()
                .unwrap()
                .app
                .live_options
                .object_generator_params = None;
            let value = if chosen == "none" {
                String::new()
            } else {
                chosen
            };
            self.set_option("object_generator_id", serde_json::json!(value));
        }
    }

    fn phantom_row(&mut self, ui: &mut Ui, current: &str) {
        let options = [
            ("off", "twoDSources.phantomOff"),
            ("broadband", "twoDSources.phantomBroadband"),
            ("spectral", "twoDSources.phantomSpectral"),
        ];
        let mut chosen = current.to_owned();
        widgets::label_row_help(
            ui,
            t("twoDSources.phantomLabel"),
            "help.phantomExtract",
            |ui| {
                egui::ComboBox::from_id_salt("phantom-extract")
                    .selected_text(t(options
                        .iter()
                        .find(|(id, _)| *id == current)
                        .map(|(_, key)| *key)
                        .unwrap_or("twoDSources.phantomOff")))
                    .width(160.0)
                    .show_ui(ui, |ui| {
                        for (id, key) in options {
                            ui.selectable_value(&mut chosen, id.to_owned(), t(key));
                        }
                    });
            },
        );
        if chosen != current {
            self.set_option("phantom_extract_mode", serde_json::json!(chosen));
        }
    }

    /// The active generator's declared parameters, one slider each.
    fn generator_params(&mut self, ui: &mut Ui, generator: &str, schema: &serde_json::Value) {
        let Some(params) = schema
            .as_array()
            .and_then(|list| {
                list.iter()
                    .find(|g| g.get("id").and_then(|v| v.as_str()) == Some(generator))
            })
            .and_then(|g| g.get("params"))
            .and_then(|p| p.as_array())
        else {
            return;
        };
        let stored = {
            let live = self.live.lock().unwrap();
            live.app.live_options.object_generator_params.clone()
        };
        for spec in params {
            let Some(key) = spec.get("key").and_then(|v| v.as_str()) else {
                continue;
            };
            let value = stored
                .as_ref()
                .and_then(|v| v.get(key))
                .and_then(|v| v.as_f64())
                .or_else(|| spec.get("default").and_then(|v| v.as_f64()))
                .unwrap_or(0.0);
            if let Some(sent) = param_slider(ui, key, spec, value) {
                {
                    let mut live = self.live.lock().unwrap();
                    let params = live
                        .app
                        .live_options
                        .object_generator_params
                        .get_or_insert_with(|| serde_json::Value::Object(Default::default()));
                    if let Some(map) = params.as_object_mut() {
                        map.insert(key.to_owned(), serde_json::json!(sent));
                    }
                }
                self.ctl.send(
                    "/omniphony/control/object_generator/param",
                    vec![
                        rosc::OscType::String(key.to_owned()),
                        rosc::OscType::Float(sent as f32),
                    ],
                );
            }
        }
    }

    /// The phantom extractor's declared parameters.
    fn phantom_params(&mut self, ui: &mut Ui, schema: &serde_json::Value) {
        let Some(params) = schema.as_array() else {
            return;
        };
        let stored = {
            let live = self.live.lock().unwrap();
            live.app.live_options.phantom_params.clone()
        };
        for spec in params {
            let Some(key) = spec.get("key").and_then(|v| v.as_str()) else {
                continue;
            };
            let value = stored
                .as_ref()
                .and_then(|v| v.get(key))
                .and_then(|v| v.as_f64())
                .or_else(|| spec.get("default").and_then(|v| v.as_f64()))
                .unwrap_or(0.0);
            if let Some(sent) = param_slider(ui, key, spec, value) {
                self.ctl.send(
                    "/omniphony/control/phantom_extract/param",
                    vec![
                        rosc::OscType::String(key.to_owned()),
                        rosc::OscType::Float(sent as f32),
                    ],
                );
            }
        }
    }
}

/// "Off" plus one entry per declared generator.
fn generator_options(schema: Option<&serde_json::Value>) -> Vec<(String, String)> {
    let mut options = vec![("none".to_owned(), t("twoDSources.objectGenNone").to_owned())];
    if let Some(list) = schema.and_then(|s| s.as_array()) {
        for generator in list {
            let Some(id) = generator.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            let label = generator
                .get("i18nKey")
                .and_then(|v| v.as_str())
                .map(|key| t(key))
                .filter(|label| !label.contains('.'))
                .map(|l| l.to_owned())
                .or_else(|| {
                    generator
                        .get("label")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_owned())
                })
                .unwrap_or_else(|| id.to_owned());
            options.push((id.to_owned(), label));
        }
    }
    options
}

/// One declared parameter: a slider with the schema's range, step and unit.
fn param_slider(ui: &mut Ui, key: &str, spec: &serde_json::Value, value: f64) -> Option<f64> {
    let min = spec.get("min").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
    let max = spec.get("max").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
    let step = spec.get("step").and_then(|v| v.as_f64()).unwrap_or(0.01);
    let unit = spec
        .get("unit")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    let label = spec
        .get("i18nKey")
        .and_then(|v| v.as_str())
        .map(|k| t(k))
        .filter(|l| !l.contains('.'))
        .map(|l| l.to_owned())
        .or_else(|| {
            spec.get("label")
                .and_then(|v| v.as_str())
                .map(|s| s.to_owned())
        })
        .unwrap_or_else(|| key.to_owned());
    let mut current = value as f32;
    // `fmtParamValue`: the step decides how many decimals are meaningful.
    let changed = widgets::value_slider(ui, &label, &mut current, min..=max, step, move |v| {
        let text = if step >= 1.0 {
            format!("{}", v.round() as i64)
        } else if step >= 0.1 {
            format!("{v:.1}")
        } else {
            format!("{v:.2}")
        };
        if unit.is_empty() {
            text
        } else {
            format!("{text} {unit}")
        }
    });
    changed.then_some(current as f64)
}

/// `effectiveHeightReason` / `effectivePhantomReason`: the stage's own state
/// first, then the master switch, then what the renderer reports.
fn effective_reason(
    configured: bool,
    master: bool,
    processing: Option<&serde_json::Value>,
    key: &str,
) -> String {
    if !configured {
        return "off".to_owned();
    }
    if !master {
        return "master_off".to_owned();
    }
    processing
        .and_then(|p| p.get(key))
        .and_then(|v| v.as_str())
        .unwrap_or("no_stream")
        .to_owned()
}
