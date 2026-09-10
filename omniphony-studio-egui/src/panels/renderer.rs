//! The renderer panel (`#rendererSection`, `ui/renderer-panel.js`): output
//! mode, the Renderer/Binaural tab pair, the evaluation grid, ramp mode, the
//! backend with its schema-generated parameters, distance diffuse, the
//! distance model and the crossover.

use egui::{RichText, Ui};

use crate::app::StudioSpike;
use crate::i18n::{t, tf};
use crate::ui::section::Section;
use crate::ui::{theme, widgets};

/// Which half of the panel is showing (`body.studio-tab-binaural`). UI state,
/// not persisted, Renderer first.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum RendererTab {
    #[default]
    Renderer,
    Binaural,
}

/// Output-mode select: the pair `(outputMode, mode)` of the binaural state
/// flattened into one choice, as `binaural.js` does.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Speaker,
    BinauralDirect,
    BinauralCascaded,
}

impl OutputMode {
    fn label(self) -> &'static str {
        match self {
            OutputMode::Speaker => t("outputMode.speakers"),
            OutputMode::BinauralDirect => t("outputMode.headphones"),
            OutputMode::BinauralCascaded => t("outputMode.headphonesVirtual"),
        }
    }

    /// `applyBinauralState`: read the flattened value out of the binaural
    /// document.
    pub(crate) fn from_state(binaural: Option<&serde_json::Value>) -> Self {
        let output = binaural
            .and_then(|b| b.get("outputMode"))
            .and_then(|v| v.as_str());
        if output != Some("binaural") {
            return OutputMode::Speaker;
        }
        match binaural
            .and_then(|b| b.get("mode"))
            .and_then(|v| v.as_str())
        {
            Some("cascaded") => OutputMode::BinauralCascaded,
            _ => OutputMode::BinauralDirect,
        }
    }
}

/// Evaluation modes, in the select's order.
const EVALUATION_MODES: &[&str] = &[
    "auto",
    "realtime",
    "precomputed_polar",
    "precomputed_cartesian",
];

/// `formatEvaluationModeLabel`.
fn evaluation_label(mode: &str) -> &'static str {
    match mode {
        "auto" => t("common.auto"),
        "realtime" => t("eval.mode.realtime"),
        "precomputed_polar" => t("common.polarShort"),
        "precomputed_cartesian" => t("common.cartesianShort"),
        _ => t("vbap.status.idle"),
    }
}

/// Ramp modes the host accepts. `interp` is deliberately absent: the web
/// select offers it but both the frontend and `control_ramp_mode` drop it.
const RAMP_MODES: &[(&str, &str)] = &[
    ("off", "audio.rampModeOff"),
    ("frame", "audio.rampModeFrame"),
    ("sample", "audio.rampModeSample"),
];

const DISTANCE_MODELS: &[(&str, &str)] = &[
    ("none", "distance.model.none"),
    ("linear", "distance.model.linear"),
    ("quadratic", "distance.model.quadratic"),
    ("inverse-square", "distance.model.inverseSquare"),
];

const METRICS: &[(&str, &str)] = &[
    ("spherical", "distance.metric.spherical"),
    ("chebyshev", "distance.metric.chebyshev"),
];

impl StudioSpike {
    pub(crate) fn renderer_section(&mut self, ui: &mut Ui) {
        let summary = {
            let live = self.live.lock().unwrap();
            let mode = live
                .app
                .render_evaluation_mode_state
                .effective
                .clone()
                .or_else(|| live.app.render_evaluation_mode_state.selection.clone())
                .unwrap_or_else(|| "auto".to_owned());
            tf("renderer.summary", &[("mode", evaluation_label(&mode))])
        };
        Section::new("rendererSection", "section.renderer")
            .summary(summary)
            .max_height(crate::ui::section::open_max_height_large(
                ui.ctx().content_rect().height(),
            ))
            .show(ui, |ui| {
                self.renderer_perf(ui);
                self.output_mode_row(ui);
                self.renderer_tabs(ui);
                match self.renderer_tab {
                    RendererTab::Renderer => {
                        self.evaluation_block(ui);
                        self.ramp_row(ui);
                        self.backend_block(ui);
                        self.distance_diffuse_block(ui);
                        self.distance_model_block(ui);
                    }
                    RendererTab::Binaural => self.binaural_tab(ui),
                }
                // Shown on both tabs.
                self.crossover_block(ui);
            });
    }

    fn output_mode_row(&mut self, ui: &mut Ui) {
        let current = {
            let live = self.live.lock().unwrap();
            OutputMode::from_state(live.app.binaural.as_ref())
        };
        let mut chosen = current;
        ui.horizontal(|ui| {
            ui.label(t("outputMode.selectTitle"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::ComboBox::from_id_salt("output-mode")
                    .selected_text(current.label())
                    .width(160.0)
                    .show_ui(ui, |ui| {
                        for mode in [
                            OutputMode::Speaker,
                            OutputMode::BinauralDirect,
                            OutputMode::BinauralCascaded,
                        ] {
                            ui.selectable_value(&mut chosen, mode, mode.label());
                        }
                    });
            });
        });
        if chosen == current {
            return;
        }
        // Not optimistic: the renderer's echo is what moves the select, so a
        // rejected change does not leave the UI lying.
        match chosen {
            OutputMode::Speaker => self
                .ctl
                .send_string("/omniphony/control/output_mode", "speaker"),
            OutputMode::BinauralDirect => {
                self.ctl
                    .send_string("/omniphony/control/output_mode", "binaural");
                self.ctl
                    .send_string("/omniphony/control/binaural_mode", "direct");
            }
            OutputMode::BinauralCascaded => {
                self.ctl
                    .send_string("/omniphony/control/output_mode", "binaural");
                self.ctl
                    .send_string("/omniphony/control/binaural_mode", "cascaded");
            }
        }
    }

    fn renderer_tabs(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            for (tab, key) in [
                (RendererTab::Renderer, "rendererTabs.renderer"),
                (RendererTab::Binaural, "rendererTabs.binaural"),
            ] {
                let active = self.renderer_tab == tab;
                if ui.selectable_label(active, t(key)).clicked() {
                    self.renderer_tab = tab;
                }
            }
        });
    }

    // ── evaluation ───────────────────────────────────────────────────────

    fn evaluation_block(&mut self, ui: &mut Ui) {
        let (
            selection,
            effective,
            allowed,
            caps,
            cartesian,
            polar,
            allow_neg_z,
            interpolation,
            intervals,
            meters_per_unit,
        ) = {
            let live = self.live.lock().unwrap();
            let s = &live.app.render_evaluation_mode_state;
            (
                s.selection.clone().unwrap_or_else(|| "auto".to_owned()),
                s.effective.clone(),
                if live
                    .app
                    .render_backend_state
                    .allowed_evaluation_modes
                    .is_empty()
                {
                    EVALUATION_MODES.iter().map(|m| (*m).to_owned()).collect()
                } else {
                    live.app
                        .render_backend_state
                        .allowed_evaluation_modes
                        .clone()
                },
                live.app.render_backend_state.capabilities.clone(),
                live.app.vbap_cartesian.clone(),
                live.app.vbap_polar.clone(),
                live.app.vbap_allow_negative_z,
                live.app.vbap_polar.position_interpolation.unwrap_or(true),
                live.app.object_size_intervals,
                // Room scale: metres per scene unit, from the renderer's room domain.
                live.app.room_ratio.scale_m.max(0.001),
            )
        };
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(t("evaluation.title"))
                    .size(theme::FONT_SIZE)
                    .color(theme::TEXT_STRONG),
            );
            widgets::help(ui, "evaluation.infoBody");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(
                        effective
                            .as_deref()
                            .map(evaluation_label)
                            .unwrap_or_else(|| t("vbap.status.idle")),
                    )
                    .size(theme::FONT_SIZE_SMALL)
                    .color(theme::TEXT_MUTED),
                );
                let mut chosen = selection.clone();
                egui::ComboBox::from_id_salt("evaluation-mode")
                    .selected_text(evaluation_label(&selection))
                    .width(150.0)
                    .show_ui(ui, |ui| {
                        for mode in &allowed {
                            ui.selectable_value(&mut chosen, mode.clone(), evaluation_label(mode));
                        }
                    });
                if chosen != selection && allowed.contains(&chosen) {
                    self.live
                        .lock()
                        .unwrap()
                        .app
                        .render_evaluation_mode_state
                        .selection = Some(chosen.clone());
                    self.mark_recompute_pending();
                    self.ctl
                        .send_string("/omniphony/control/render_evaluation_mode", &chosen);
                }
            });
        });

        // Which grid block applies: `auto` follows the effective mode.
        let visible_mode = if selection == "auto" {
            effective.clone().unwrap_or_else(|| "auto".to_owned())
        } else {
            selection.clone()
        };
        let supports_cartesian = caps
            .as_ref()
            .is_none_or(|c| c.supports_precomputed_cartesian);
        let supports_polar = caps.as_ref().is_none_or(|c| c.supports_precomputed_polar);
        let show_cartesian = visible_mode == "precomputed_cartesian" && supports_cartesian;
        let show_polar = visible_mode == "precomputed_polar" && supports_polar;

        if show_cartesian {
            ui.label(
                RichText::new(t("eval.cartesianGrid"))
                    .size(theme::FONT_SIZE_SMALL)
                    .color(theme::TEXT_MUTED),
            );
            ui.horizontal(|ui| {
                // Steps are the room extent over the count: 2 units across X
                // and Y, 1 unit up.
                self.grid_field(ui, "X", cartesian.x_size, 1, |v| ("cartesian/x_size", v));
                self.grid_field(ui, "Y", cartesian.y_size, 1, |v| ("cartesian/y_size", v));
                self.grid_field(ui, "Z+", cartesian.z_size, 1, |v| ("cartesian/z_size", v));
                self.grid_field(ui, "Z-", cartesian.z_neg_size, 0, |v| {
                    ("cartesian/z_neg_size", v)
                });
            });
            ui.horizontal(|ui| {
                step_label(
                    ui,
                    cartesian
                        .x_size
                        .map(|n| 2.0 / n as f64 * meters_per_unit * 1000.0),
                    "mm",
                );
                step_label(
                    ui,
                    cartesian
                        .y_size
                        .map(|n| 2.0 / n as f64 * meters_per_unit * 1000.0),
                    "mm",
                );
                step_label(
                    ui,
                    cartesian
                        .z_size
                        .map(|n| 1.0 / n as f64 * meters_per_unit * 1000.0),
                    "mm",
                );
                let z_neg = cartesian
                    .z_neg_size
                    .filter(|n| *n > 0 && allow_neg_z != Some(false))
                    .map(|n| 1.0 / n as f64 * meters_per_unit * 1000.0);
                step_label(ui, z_neg, "mm");
            });
        }

        if show_polar {
            ui.label(
                RichText::new(t("eval.polarGrid"))
                    .size(theme::FONT_SIZE_SMALL)
                    .color(theme::TEXT_MUTED),
            );
            ui.horizontal(|ui| {
                self.grid_field(ui, "az", polar.azimuth_resolution, 1, |v| {
                    ("polar/azimuth_resolution", v)
                });
                self.grid_field(ui, "el", polar.elevation_resolution, 1, |v| {
                    ("polar/elevation_resolution", v)
                });
                self.grid_field(ui, "d", polar.distance_res, 1, |v| {
                    ("polar/distance_res", v)
                });
            });
            ui.horizontal(|ui| {
                step_label(ui, polar.azimuth_resolution.map(|n| 360.0 / n as f64), "°");
                let elevation_span = if allow_neg_z == Some(false) {
                    90.0
                } else {
                    180.0
                };
                step_label(
                    ui,
                    polar
                        .elevation_resolution
                        .map(|n| elevation_span / n as f64),
                    "°",
                );
                let distance_step = match (polar.distance_max, polar.distance_res) {
                    (Some(max), Some(res)) if res > 0 => Some(max / res as f64),
                    _ => None,
                };
                step_label(ui, distance_step, "");
            });
            let mut distance_max = polar.distance_max.unwrap_or(2.0) as f32;
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("d max")
                        .size(theme::FONT_SIZE_SMALL)
                        .color(theme::TEXT_MUTED),
                );
                if ui
                    .add(
                        egui::DragValue::new(&mut distance_max)
                            .speed(0.01)
                            .range(0.01..=f32::MAX),
                    )
                    .changed()
                {
                    self.live.lock().unwrap().app.vbap_polar.distance_max =
                        Some(distance_max as f64);
                    self.mark_recompute_pending();
                    self.ctl.send_float(
                        "/omniphony/control/render_evaluation/polar/distance_max",
                        distance_max.max(0.01),
                    );
                }
            });
        }

        if show_cartesian || show_polar {
            let mut on = interpolation;
            if widgets::switch_row(ui, t("vbap.positionInterpolation"), &mut on) {
                self.live
                    .lock()
                    .unwrap()
                    .app
                    .vbap_polar
                    .position_interpolation = Some(on);
                self.mark_recompute_pending();
                self.ctl.send_int(
                    "/omniphony/control/render_evaluation/position_interpolation",
                    i32::from(on),
                );
            }
        }

        // Hidden only when the backend says it cannot size events.
        let supports_size = caps.as_ref().is_none_or(|c| c.supports_spread);
        if supports_size {
            let mut value = intervals;
            ui.horizontal(|ui| {
                ui.label(t("evaluation.objectSizeIntervals"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(egui::DragValue::new(&mut value).range(0..=u32::MAX))
                        .changed()
                    {
                        self.live.lock().unwrap().app.object_size_intervals = value;
                        self.mark_recompute_pending();
                        self.ctl.send_int(
                            "/omniphony/control/render_evaluation/object_size_intervals",
                            value as i32,
                        );
                    }
                });
            });
        }
    }

    /// One integer field of an evaluation grid. `floor` is the smallest value
    /// the renderer accepts (1 everywhere but the negative-Z count).
    fn grid_field(
        &mut self,
        ui: &mut Ui,
        placeholder: &str,
        current: Option<u32>,
        floor: u32,
        address: impl Fn(u32) -> (&'static str, u32),
    ) {
        let mut value = current.unwrap_or(floor);
        ui.vertical(|ui| {
            ui.label(
                RichText::new(placeholder)
                    .size(theme::FONT_SIZE_SMALL)
                    .color(theme::TEXT_MUTED),
            );
            if ui
                .add_sized(
                    egui::vec2(48.0, ui.spacing().interact_size.y),
                    egui::DragValue::new(&mut value).range(floor..=u32::MAX),
                )
                .changed()
            {
                let (suffix, value) = address(value.max(floor));
                self.mark_recompute_pending();
                self.ctl.send_int(
                    &format!("/omniphony/control/render_evaluation/{suffix}"),
                    value as i32,
                );
            }
        });
    }

    // ── backend and its schema-generated parameters ──────────────────────

    fn backend_block(&mut self, ui: &mut Ui) {
        let (selection, effective, effective_label, available, values, frozen, status) = {
            let live = self.live.lock().unwrap();
            let b = &live.app.render_backend_state;
            (
                b.selection.clone().unwrap_or_else(|| "vbap".to_owned()),
                b.effective.clone(),
                b.effective_label.clone(),
                b.available_backends.clone(),
                b.backend_param_values_by_id.clone(),
                b.frozen_speakers,
                vbap_status(&live.app.recompute_error, live.app.vbap_recomputing),
            )
        };
        let backends = backend_list(&available);
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(t("backend.title"))
                    .size(theme::FONT_SIZE)
                    .color(theme::TEXT_STRONG),
            );
            widgets::help(ui, "backend.infoBody");
            ui.label(
                RichText::new(status.0)
                    .size(theme::FONT_SIZE_SMALL)
                    .color(status.1),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(
                        effective_label
                            .clone()
                            .or_else(|| effective.clone())
                            .unwrap_or_else(|| "—".to_owned()),
                    )
                    .size(theme::FONT_SIZE_SMALL)
                    .color(theme::TEXT_MUTED),
                );
                let mut chosen = selection.clone();
                ui.add_enabled_ui(!frozen, |ui| {
                    egui::ComboBox::from_id_salt("render-backend")
                        .selected_text(
                            backends
                                .iter()
                                .find(|(id, _)| *id == selection)
                                .map(|(_, label)| label.clone())
                                .unwrap_or_else(|| selection.clone()),
                        )
                        .width(150.0)
                        .show_ui(ui, |ui| {
                            for (id, label) in &backends {
                                ui.selectable_value(&mut chosen, id.clone(), label);
                            }
                        });
                });
                if chosen != selection && !chosen.is_empty() {
                    self.live.lock().unwrap().app.render_backend_state.selection =
                        Some(chosen.clone());
                    self.mark_recompute_pending();
                    self.ctl
                        .send_string("/omniphony/control/render_backend", &chosen);
                }
            });
        });

        // The script backend follows the selection so its file field stays
        // reachable while its build fails; everything else follows what the
        // engine actually runs.
        let visible = if selection == "script" {
            selection.clone()
        } else {
            effective.clone().unwrap_or(selection)
        };
        // `hybrid` has a bespoke panel of its own, not a generated one.
        if visible == "hybrid" {
            self.hybrid_block(ui, &available, &values);
            return;
        }
        self.backend_params_for(ui, &visible, &available, &values);
    }

    /// One control per declared parameter of the visible backend
    /// (`renderGenericBackendParams`). The schema is the renderer's; nothing
    /// here is hard-coded per backend.
    pub(crate) fn backend_params_for(
        &mut self,
        ui: &mut Ui,
        backend: &str,
        available: &serde_json::Value,
        values: &serde_json::Value,
    ) {
        let Some(params) = available
            .as_array()
            .and_then(|list| {
                list.iter()
                    .find(|b| b.get("id").and_then(|v| v.as_str()) == Some(backend))
            })
            .and_then(|b| b.get("params"))
            .and_then(|p| p.as_array())
        else {
            return;
        };
        let stored = values.get(backend);
        for spec in params {
            let Some(key) = spec.get("key").and_then(|v| v.as_str()) else {
                continue;
            };
            let value = stored
                .and_then(|v| v.get(key))
                .cloned()
                .or_else(|| spec.get("default").cloned())
                .unwrap_or(serde_json::Value::Null);
            let label = param_label(key, spec);
            let kind = spec.get("kind");
            let kind_type = kind
                .and_then(|k| k.get("type"))
                .and_then(|v| v.as_str())
                .unwrap_or("float");
            let sent = match kind_type {
                "bool" => {
                    let mut on = value.as_bool().unwrap_or(false);
                    widgets::switch_row(ui, &label, &mut on).then(|| serde_json::json!(on))
                }
                "enum" => {
                    let current = value.as_str().unwrap_or("").to_owned();
                    let options: Vec<(String, String)> = kind
                        .and_then(|k| k.get("options"))
                        .and_then(|o| o.as_array())
                        .map(|list| {
                            list.iter()
                                .map(|o| {
                                    let v = o
                                        .get("value")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_owned();
                                    let l = option_label(key, &v, o);
                                    (v, l)
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let mut chosen = current.clone();
                    ui.horizontal(|ui| {
                        ui.label(&label);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            egui::ComboBox::from_id_salt(("backend-param", key))
                                .selected_text(
                                    options
                                        .iter()
                                        .find(|(v, _)| *v == current)
                                        .map(|(_, l)| l.clone())
                                        .unwrap_or_else(|| current.clone()),
                                )
                                .width(150.0)
                                .show_ui(ui, |ui| {
                                    for (v, l) in &options {
                                        ui.selectable_value(&mut chosen, v.clone(), l);
                                    }
                                });
                        });
                    });
                    (chosen != current).then(|| serde_json::json!(chosen))
                }
                "path" | "file" => {
                    let mut text = value.as_str().unwrap_or("").to_owned();
                    let mut changed = false;
                    ui.horizontal(|ui| {
                        ui.label(&label);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            changed = ui
                                .add(
                                    egui::TextEdit::singleline(&mut text)
                                        .desired_width(170.0)
                                        .hint_text(if kind_type == "path" {
                                            "/path/to/backend.lua"
                                        } else {
                                            "name.ext"
                                        }),
                                )
                                .lost_focus();
                        });
                    });
                    changed.then(|| serde_json::json!(text.trim()))
                }
                _ => {
                    let is_int = kind_type == "int";
                    let min = kind
                        .and_then(|k| k.get("min"))
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0) as f32;
                    let max = kind
                        .and_then(|k| k.get("max"))
                        .and_then(|v| v.as_f64())
                        .unwrap_or(1.0) as f32;
                    let step = if is_int {
                        1.0
                    } else {
                        kind.and_then(|k| k.get("step"))
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.01)
                    };
                    let mut number = value.as_f64().unwrap_or(min as f64) as f32;
                    widgets::value_slider(ui, &label, &mut number, min..=max, step, move |v| {
                        if is_int {
                            format!("{}", v.round() as i64)
                        } else {
                            format!("{v:.3}")
                        }
                    })
                    .then(|| {
                        if is_int {
                            serde_json::json!(number.round() as i64)
                        } else {
                            serde_json::json!(number as f64)
                        }
                    })
                }
            };
            if let Some(value) = sent {
                self.send_backend_param(backend, key, value);
            }
            if let Some(help) = param_help(key, spec) {
                widgets::note(ui, &help);
            }
        }
    }

    /// `sendBackendParam`: no optimistic write, the renderer echoes the value.
    fn send_backend_param(&mut self, backend: &str, key: &str, value: serde_json::Value) {
        let arg = match value {
            serde_json::Value::Bool(b) => rosc::OscType::Bool(b),
            serde_json::Value::String(s) => rosc::OscType::String(s),
            serde_json::Value::Number(n) => rosc::OscType::Float(n.as_f64().unwrap_or(0.0) as f32),
            other => rosc::OscType::String(other.to_string()),
        };
        self.ctl.send(
            "/omniphony/control/backend/param",
            vec![
                rosc::OscType::String(backend.to_owned()),
                rosc::OscType::String(key.to_owned()),
                arg,
            ],
        );
    }

    // ── ramp, crossover, distance ────────────────────────────────────────

    fn ramp_row(&mut self, ui: &mut Ui) {
        let current = {
            let live = self.live.lock().unwrap();
            live.app
                .audio
                .ramp_mode
                .clone()
                .unwrap_or_else(|| "frame".into())
        };
        let current = if RAMP_MODES.iter().any(|(id, _)| *id == current) {
            current
        } else {
            "frame".to_owned()
        };
        let mut chosen = current.clone();
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(t("renderer.rampTitle"))
                    .size(theme::FONT_SIZE)
                    .color(theme::TEXT_STRONG),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::ComboBox::from_id_salt("ramp-mode")
                    .selected_text(t(RAMP_MODES
                        .iter()
                        .find(|(id, _)| *id == current)
                        .map(|(_, key)| *key)
                        .unwrap_or("audio.rampModeFrame")))
                    .width(140.0)
                    .show_ui(ui, |ui| {
                        for (id, key) in RAMP_MODES {
                            ui.selectable_value(&mut chosen, (*id).to_owned(), t(key));
                        }
                    });
            });
        });
        if chosen != current {
            self.live.lock().unwrap().app.audio.ramp_mode = Some(chosen.clone());
            self.ctl
                .send_string("/omniphony/control/ramp_mode", &chosen);
        }
    }

    fn crossover_block(&mut self, ui: &mut Ui) {
        let (crossover, kind, transition) = {
            let live = self.live.lock().unwrap();
            (
                live.app.live_options.crossover.clone(),
                live.option_str("crossover_type")
                    .unwrap_or_else(|| "lr4".into()),
                live.option_f64("crossover_fir_transition_ratio")
                    .unwrap_or(0.5),
            )
        };
        ui.add_space(4.0);
        ui.label(
            RichText::new(t("renderer.crossoverTitle"))
                .size(theme::FONT_SIZE)
                .color(theme::TEXT_STRONG),
        );
        let mut chosen = kind.clone();
        ui.horizontal(|ui| {
            ui.label(t("renderer.crossoverTypeLabel"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::ComboBox::from_id_salt("crossover-type")
                    .selected_text(t(if kind == "fir" {
                        "renderer.crossoverType.fir"
                    } else {
                        "renderer.crossoverType.lr4"
                    }))
                    .width(160.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut chosen,
                            "lr4".to_owned(),
                            t("renderer.crossoverType.lr4"),
                        );
                        ui.selectable_value(
                            &mut chosen,
                            "fir".to_owned(),
                            t("renderer.crossoverType.fir"),
                        );
                    });
            });
        });
        if chosen != kind {
            self.set_option("crossover_type", serde_json::json!(chosen));
        }
        if kind == "fir" {
            let mut ratio = transition as f32;
            if widgets::value_slider(
                ui,
                t("renderer.crossoverTransitionLabel"),
                &mut ratio,
                0.05..=2.0,
                0.05,
                |v| format!("{v:.2}"),
            ) {
                self.set_option(
                    "crossover_fir_transition_ratio",
                    serde_json::json!(ratio as f64),
                );
            }
        }
        widgets::note(ui, &crossover_info(crossover.as_ref()));
    }

    fn distance_diffuse_block(&mut self, ui: &mut Ui) {
        let state = {
            let live = self.live.lock().unwrap();
            live.app.distance_diffuse.clone()
        };
        ui.add_space(4.0);
        let mut enabled = state.enabled.unwrap_or(false);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(t("distance.title"))
                    .size(theme::FONT_SIZE)
                    .color(theme::TEXT_STRONG),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::switch(ui, &mut enabled).changed() {
                    self.live.lock().unwrap().app.distance_diffuse.enabled = Some(enabled);
                    self.ctl.send_int(
                        "/omniphony/control/distance_diffuse/enabled",
                        i32::from(enabled),
                    );
                }
            });
        });
        // The parameters collapse with the effect, as in the web panel.
        if !enabled {
            return;
        }
        let metric = state.metric.clone().unwrap_or_else(|| "spherical".into());
        if let Some(chosen) = self.metric_row(ui, "distance-diffuse-metric", &metric) {
            self.live.lock().unwrap().app.distance_diffuse.metric = Some(chosen.clone());
            self.mark_recompute_pending();
            self.ctl
                .send_string("/omniphony/control/distance_diffuse/metric", &chosen);
        }
        let mut threshold = state.threshold.unwrap_or(1.0) as f32;
        if widgets::value_slider(
            ui,
            t("distance.threshold"),
            &mut threshold,
            0.1..=2.0,
            0.01,
            |v| format!("{v:.2}"),
        ) {
            self.live.lock().unwrap().app.distance_diffuse.threshold = Some(threshold as f64);
            self.ctl.send_float(
                "/omniphony/control/distance_diffuse/threshold",
                threshold.max(0.01),
            );
        }
        let mut curve = state.curve.unwrap_or(1.0) as f32;
        if widgets::value_slider(ui, t("distance.curve"), &mut curve, 0.5..=2.0, 0.05, |v| {
            format!("{v:.2}")
        }) {
            self.live.lock().unwrap().app.distance_diffuse.curve = Some(curve as f64);
            self.ctl
                .send_float("/omniphony/control/distance_diffuse/curve", curve.max(0.0));
        }
    }

    fn distance_model_block(&mut self, ui: &mut Ui) {
        let (value, metric) = {
            let live = self.live.lock().unwrap();
            (
                live.app
                    .distance_model
                    .value
                    .clone()
                    .unwrap_or_else(|| "none".into()),
                live.app
                    .distance_model
                    .metric
                    .clone()
                    .unwrap_or_else(|| "spherical".into()),
            )
        };
        let value = if DISTANCE_MODELS.iter().any(|(id, _)| *id == value) {
            value
        } else {
            "none".to_owned()
        };
        ui.add_space(4.0);
        let mut chosen = value.clone();
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(t("distance.model"))
                    .size(theme::FONT_SIZE)
                    .color(theme::TEXT_STRONG),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::ComboBox::from_id_salt("distance-model")
                    .selected_text(t(DISTANCE_MODELS
                        .iter()
                        .find(|(id, _)| *id == value)
                        .map(|(_, key)| *key)
                        .unwrap_or("distance.model.none")))
                    .width(150.0)
                    .show_ui(ui, |ui| {
                        for (id, key) in DISTANCE_MODELS {
                            ui.selectable_value(&mut chosen, (*id).to_owned(), t(key));
                        }
                    });
            });
        });
        if chosen != value {
            self.live.lock().unwrap().app.distance_model.value = Some(chosen.clone());
            self.mark_recompute_pending();
            self.ctl
                .send_string("/omniphony/control/distance_model", &chosen);
        }
        // The metric only means something once a model is applied.
        if value != "none"
            && let Some(chosen) = self.metric_row(ui, "distance-model-metric", &metric)
        {
            self.live.lock().unwrap().app.distance_model.metric = Some(chosen.clone());
            self.mark_recompute_pending();
            self.ctl
                .send_string("/omniphony/control/distance_model_metric", &chosen);
        }
    }

    /// The spherical/Chebyshev select shared by both distance blocks.
    fn metric_row(&mut self, ui: &mut Ui, id: &str, current: &str) -> Option<String> {
        let mut chosen = current.to_owned();
        ui.horizontal(|ui| {
            ui.label(t("distance.metric"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::ComboBox::from_id_salt(id)
                    .selected_text(t(METRICS
                        .iter()
                        .find(|(v, _)| *v == current)
                        .map(|(_, key)| *key)
                        .unwrap_or("distance.metric.spherical")))
                    .width(130.0)
                    .show_ui(ui, |ui| {
                        for (v, key) in METRICS {
                            ui.selectable_value(&mut chosen, (*v).to_owned(), t(key));
                        }
                    });
            });
        });
        (chosen != current).then_some(chosen)
    }

    /// `markRecomputePending`: assume the engine will recompute, and complain
    /// after eight seconds if it never says it did.
    pub(crate) fn mark_recompute_pending(&mut self) {
        let mut live = self.live.lock().unwrap();
        live.app.vbap_recomputing = Some(true);
        live.app.recompute_error = None;
        drop(live);
        self.recompute_deadline = Some(std::time::Instant::now() + RECOMPUTE_ACK_TIMEOUT);
    }

    /// Called every frame: raise the no-answer error once the deadline passes.
    pub(crate) fn check_recompute_ack(&mut self) {
        let Some(deadline) = self.recompute_deadline else {
            return;
        };
        let mut live = self.live.lock().unwrap();
        if live.app.vbap_recomputing != Some(true) {
            drop(live);
            self.recompute_deadline = None;
            return;
        }
        if std::time::Instant::now() >= deadline {
            live.app.vbap_recomputing = Some(false);
            live.app.recompute_error = Some(t("vbap.status.noAck").to_owned());
            drop(live);
            self.recompute_deadline = None;
        }
    }

    /// `control_option`: optimistic local write plus the registry message.
    pub(crate) fn set_option(&mut self, key: &str, value: serde_json::Value) {
        {
            let mut live = self.live.lock().unwrap();
            live.set_option(key, value.clone());
        }
        let encoded = match &value {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        self.ctl.send(
            "/omniphony/control/option",
            vec![
                rosc::OscType::String(key.to_owned()),
                rosc::OscType::String(encoded),
            ],
        );
    }
}

/// Eight seconds without a `vbap:recomputing` broadcast is an unanswered
/// request (`markRecomputePending`).
const RECOMPUTE_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// `.vbap-step`: the grid's step, or an em dash when it is unknown.
fn step_label(ui: &mut Ui, value: Option<f64>, unit: &str) {
    let text = match value {
        Some(v) if unit == "mm" => format!("{v:.1}mm"),
        Some(v) if unit == "°" => format!("{v:.2}°"),
        Some(v) => format!("{v:.3}"),
        None => "—".to_owned(),
    };
    ui.add_sized(
        egui::vec2(48.0, ui.spacing().interact_size.y),
        egui::Label::new(
            RichText::new(text)
                .size(theme::FONT_SIZE_SMALL)
                .color(theme::TEXT_DIM),
        ),
    );
}

/// `#crossoverInfo`: what the renderer actually built.
fn crossover_info(crossover: Option<&serde_json::Value>) -> String {
    let Some(c) = crossover else {
        return t("renderer.crossoverInfoNone").to_owned();
    };
    let bands = c.get("bands").and_then(|v| v.as_u64()).unwrap_or(0);
    if bands <= 1 {
        return t("renderer.crossoverInfoNone").to_owned();
    }
    let low = c
        .get("cutoffsHz")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_f64())
        .map(|v| format!("{}", v.round()))
        .unwrap_or_else(|| "—".to_owned());
    let bands = bands.to_string();
    if c.get("engine").and_then(|v| v.as_str()) == Some("fir") {
        let taps = c
            .get("taps")
            .and_then(|v| v.as_u64())
            .map(|v| v.to_string())
            .unwrap_or_else(|| "—".to_owned());
        let latency = c.get("latencyMs").and_then(|v| v.as_f64()).unwrap_or(0.0);
        tf(
            "renderer.crossoverInfoFir",
            &[
                ("bands", &bands),
                ("taps", &taps),
                ("latency", &format!("{latency:.1}")),
                ("low", &low),
            ],
        )
    } else {
        tf(
            "renderer.crossoverInfoIir",
            &[("bands", &bands), ("low", &low)],
        )
    }
}

/// The backend list the engine published, or the four built-in ids.
pub(crate) fn backend_list(available: &serde_json::Value) -> Vec<(String, String)> {
    if let Some(list) = available.as_array().filter(|l| !l.is_empty()) {
        return list
            .iter()
            .filter_map(|b| {
                let id = b.get("id").and_then(|v| v.as_str())?.to_owned();
                let label = b
                    .get("label")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&id)
                    .to_owned();
                Some((id, label))
            })
            .collect();
    }
    [
        ("vbap", "VBAP"),
        ("barycenter", "Barycenter"),
        ("experimental_distance", "Distance"),
        ("hybrid", "Hybrid"),
    ]
    .into_iter()
    .map(|(id, label)| (id.to_owned(), label.to_owned()))
    .collect()
}

/// A translated parameter label wins over the schema's own.
fn param_label(key: &str, spec: &serde_json::Value) -> String {
    let translated = t(&format!("backendParam.{key}"));
    if translated != format!("backendParam.{key}") {
        return translated.to_owned();
    }
    spec.get("label")
        .and_then(|v| v.as_str())
        .unwrap_or(key)
        .to_owned()
}

fn param_help(key: &str, spec: &serde_json::Value) -> Option<String> {
    let translated = t(&format!("backendParamHelp.{key}"));
    if translated != format!("backendParamHelp.{key}") {
        return Some(translated.to_owned());
    }
    spec.get("help")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
}

fn option_label(key: &str, value: &str, option: &serde_json::Value) -> String {
    let translated = t(&format!("backendParamOption.{key}.{value}"));
    if translated != format!("backendParamOption.{key}.{value}") {
        return translated.to_owned();
    }
    option
        .get("label")
        .and_then(|v| v.as_str())
        .unwrap_or(value)
        .to_owned()
}

/// `renderVbapStatus`: the engine's recompute state, or the error it reported.
fn vbap_status(error: &Option<String>, recomputing: Option<bool>) -> (String, egui::Color32) {
    match (error, recomputing) {
        (Some(message), _) => (message.clone(), theme::ERROR),
        (None, Some(true)) => (t("vbap.status.computing").to_owned(), theme::WARN),
        (None, Some(false)) => (t("vbap.status.ready").to_owned(), theme::OK),
        (None, None) => (t("vbap.status.idle").to_owned(), theme::TEXT_MUTED),
    }
}
