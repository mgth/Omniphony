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
use crate::host::commands::engine;
use crate::i18n::t;
use crate::ui::group::Group;
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
            let live = self.host.read();
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

                widgets::label_row_help(
                    ui,
                    t("twoDSources.surroundLabel"),
                    "help.twoDSources.surroundPlacement",
                    |ui| {
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
                    },
                );

                let mut on = synthetic;
                if widgets::switch_row_help(
                    ui,
                    t("twoDSources.syntheticObjectsLabel"),
                    "help.syntheticObjects",
                    &mut on,
                ) {
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

                // Height generator, then phantom extraction: each a group
                // with its choice in the bar and, in the inset, why it is or
                // is not running and the parameters it declares.
                let height_reason = effective_reason(
                    &generator != "none" && !generator.is_empty(),
                    synthetic,
                    processing.as_ref(),
                    "height",
                );
                self.generator_group(ui, &generator, generators.as_ref(), &height_reason);
                let phantom_reason =
                    effective_reason(phantom != "off", synthetic, processing.as_ref(), "phantom");
                self.phantom_group(ui, &phantom, phantom_schema.as_ref(), &phantom_reason);

                // The channel layout every fixed channel is placed by. The
                // editor for one channel opens from the objects list. Aligned
                // to the top of what is left, not its middle: centred, the
                // button hung halfway down the empty panel.
                ui.add_space(4.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    if ui.button(t("virtualBed.reset")).clicked() {
                        self.virtual_bed_reset_confirm = true;
                    }
                });
            });
    }

    fn generator_group(
        &mut self,
        ui: &mut Ui,
        current: &str,
        schema: Option<&serde_json::Value>,
        reason: &str,
    ) {
        let options = generator_options(schema);
        let current = if current.is_empty() {
            "none".to_owned()
        } else {
            current.to_owned()
        };
        let mut chosen = current.clone();
        Group::new(t("twoDSources.objectGeneratorLabel"))
            .help("help.objectGenerator")
            .actions(|ui| {
                widgets::bounded_combo(ui, 160.0, |ui, w| {
                    egui::ComboBox::from_id_salt("object-generator")
                        .selected_text(
                            options
                                .iter()
                                .find(|(id, _)| *id == current)
                                .map(|(_, label)| label.clone())
                                .unwrap_or_else(|| current.clone()),
                        )
                        .width(w)
                        .truncate()
                        .show_ui(ui, |ui| {
                            for (id, label) in &options {
                                ui.selectable_value(&mut chosen, id.clone(), label);
                            }
                        })
                });
            })
            .show(ui, |ui| {
                // `objectGeneratorNoHeightNote`: only while the generator is
                // chosen and not running. "Off" would repeat the select, and
                // "active" is what choosing it meant.
                if reason != "off" && reason != "active" {
                    widgets::note(ui, reason_text(reason));
                }
                if let Some(schema) = schema {
                    self.generator_params(ui, &current, schema);
                }
            });
        if chosen != current {
            let id = if chosen == "none" { "" } else { &chosen };
            crate::host::commands::engine::set_object_generator(&self.host, id);
        }
    }

    fn phantom_group(
        &mut self,
        ui: &mut Ui,
        current: &str,
        schema: Option<&serde_json::Value>,
        reason: &str,
    ) {
        let options = [
            ("off", "twoDSources.phantomOff"),
            ("broadband", "twoDSources.phantomBroadband"),
            ("spectral", "twoDSources.phantomSpectral"),
        ];
        let mut chosen = current.to_owned();
        Group::new(t("twoDSources.phantomLabel"))
            .help("help.phantomExtract")
            .actions(|ui| {
                widgets::bounded_combo(ui, 160.0, |ui, w| {
                    egui::ComboBox::from_id_salt("phantom-extract")
                        .selected_text(t(options
                            .iter()
                            .find(|(id, _)| *id == current)
                            .map(|(_, key)| *key)
                            .unwrap_or("twoDSources.phantomOff")))
                        .width(w)
                        .truncate()
                        .show_ui(ui, |ui| {
                            for (id, key) in options {
                                ui.selectable_value(&mut chosen, id.to_owned(), t(key));
                            }
                        })
                });
            })
            .show(ui, |ui| {
                widgets::note(ui, reason_text(reason));
                if current != "off"
                    && let Some(schema) = schema
                {
                    self.phantom_params(ui, schema, current == "spectral");
                }
            });
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
            let live = self.host.read();
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
                engine::set_object_generator_param(&self.host, key, sent);
            }
        }
    }

    /// The phantom extractor's declared parameters (`buildPhantomParamSliders`):
    /// a switch for an on/off parameter, a slider for the rest. A parameter
    /// only the other method reads stays editable — the configuration is
    /// kept for when that method is picked — but is dimmed and says so.
    fn phantom_params(&mut self, ui: &mut Ui, schema: &serde_json::Value, spectral: bool) {
        let Some(params) = schema.as_array() else {
            return;
        };
        let stored = {
            let live = self.host.read();
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
            let gate = phantom_gate(key, spectral);
            let row = ui.scope(|ui| {
                if gate.is_some() {
                    ui.multiply_opacity(0.7);
                }
                if is_binary(spec) {
                    let mut on = value >= 0.5;
                    widgets::switch_row(ui, &param_label(key, spec), &mut on).then_some(if on {
                        1.0
                    } else {
                        0.0
                    })
                } else {
                    param_slider(ui, key, spec, value)
                }
            });
            if let Some(hover) = gate {
                row.response.on_hover_text(t(hover));
            }
            if let Some(sent) = row.inner {
                // Kept at once, as the generator's are: the slider would
                // otherwise snap back until the renderer's echo arrives.
                engine::set_phantom_extract_param(&self.host, key, sent);
            }
        }
    }

    /// The web's `confirm('confirm.resetVirtualBed')`: the reset moves every
    /// channel back to its default place.
    pub(crate) fn virtual_bed_reset_modal(&mut self, ctx: &egui::Context) {
        if !self.virtual_bed_reset_confirm {
            return;
        }
        let mut run = false;
        let mut cancel = false;
        let modal = egui::Modal::new(egui::Id::new("virtual-bed-reset-confirm"))
            .frame(widgets::modal_frame())
            .show(ctx, |ui| {
                ui.set_max_width(340.0);
                for line in t("confirm.resetVirtualBed").split('\n') {
                    if line.is_empty() {
                        ui.add_space(crate::ui::theme::ROW_GAP);
                    } else {
                        ui.label(line);
                    }
                }
                ui.add_space(crate::ui::theme::PANEL_GAP);
                ui.horizontal(|ui| {
                    cancel = ui.button(t("common.cancel")).clicked();
                    run = ui
                        .button(
                            egui::RichText::new(t("virtualBed.reset"))
                                .color(crate::ui::theme::WARN),
                        )
                        .clicked();
                });
            });
        if run {
            crate::host::commands::engine::reset_virtual_bed(&self.host);
        }
        if run || cancel || modal.should_close() {
            self.virtual_bed_reset_confirm = false;
        }
    }
}

/// "Off" plus one entry per declared generator.
fn generator_options(schema: Option<&serde_json::Value>) -> Vec<(String, String)> {
    let mut options = vec![("none".to_owned(), t("twoDSources.objectGenNone").to_owned())];
    let declared = schema
        .and_then(|s| s.as_array())
        .filter(|list| !list.is_empty());
    let Some(list) = declared else {
        // The select's built-in options, kept by the web "as a fallback
        // until the schema arrives / for older renderers" — and for a
        // renderer that publishes an empty list, which left "Off" the only
        // choice here.
        for (id, key) in [
            ("copy_up", "twoDSources.objectGenCopyUp"),
            ("pad", "twoDSources.objectGenPad"),
            ("dirac", "twoDSources.objectGenDirac"),
        ] {
            options.push((id.to_owned(), t(key).to_owned()));
        }
        return options;
    };
    {
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

/// `schemaLabel`: the translation when the key has one, else the English
/// label the schema carries, else the key.
fn param_label(key: &str, spec: &serde_json::Value) -> String {
    spec.get("i18nKey")
        .and_then(|v| v.as_str())
        .and_then(crate::i18n::lookup)
        .map(str::to_owned)
        .or_else(|| {
            spec.get("label")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| key.to_owned())
}

/// `isBinaryParam`: an on/off parameter (0..1 in steps of 1), shown as a
/// switch rather than a two-position slider.
fn is_binary(spec: &serde_json::Value) -> bool {
    let number = |k: &str| spec.get(k).and_then(|v| v.as_f64());
    number("min") == Some(0.0) && number("max") == Some(1.0) && number("step") == Some(1.0)
}

/// `applyPhantomParamGate`: which method alone reads `key`, when it is not
/// the one running — the i18n key of the note saying so.
fn phantom_gate(key: &str, spectral: bool) -> Option<&'static str> {
    const BROADBAND_ONLY: [&str; 3] = ["passes", "center", "sides"];
    const SPECTRAL_ONLY: [&str; 2] = ["heights", "height_split"];
    if spectral && BROADBAND_ONLY.contains(&key) {
        Some("twoDSources.phantomBroadbandOnly")
    } else if !spectral && SPECTRAL_ONLY.contains(&key) {
        Some("twoDSources.phantomSpectralOnly")
    } else {
        None
    }
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
    let label = param_label(key, spec);
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

#[cfg(test)]
mod tests {
    use super::{generator_options, is_binary, phantom_gate};

    #[test]
    fn an_empty_generator_schema_falls_back_to_the_built_in_generators() {
        let ids = |schema: Option<&serde_json::Value>| {
            generator_options(schema)
                .into_iter()
                .map(|(id, _)| id)
                .collect::<Vec<_>>()
        };
        let built_in = ["none", "copy_up", "pad", "dirac"];
        assert_eq!(ids(None), built_in);
        assert_eq!(ids(Some(&serde_json::json!([]))), built_in);
        // A declared list replaces them.
        assert_eq!(
            ids(Some(
                &serde_json::json!([{ "id": "custom", "label": "Custom" }])
            )),
            ["none", "custom"]
        );
    }

    #[test]
    fn on_off_parameters_and_method_only_parameters_are_told_apart() {
        assert!(is_binary(
            &serde_json::json!({ "min": 0, "max": 1, "step": 1 })
        ));
        assert!(!is_binary(
            &serde_json::json!({ "min": 0, "max": 1, "step": 0.01 })
        ));
        assert_eq!(
            phantom_gate("passes", true),
            Some("twoDSources.phantomBroadbandOnly")
        );
        assert_eq!(phantom_gate("passes", false), None);
        assert_eq!(
            phantom_gate("heights", false),
            Some("twoDSources.phantomSpectralOnly")
        );
        assert_eq!(phantom_gate("strength", true), None);
    }
}
