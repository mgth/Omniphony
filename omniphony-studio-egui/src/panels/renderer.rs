//! The renderer panel (`#rendererSection`, `ui/renderer-panel.js`): the
//! output mode, the Renderer/Binaural tab pair, and the groups of each tab —
//! backend, evaluation, distance model, distance diffuse and ramp on the
//! Renderer tab, the four binaural groups on the other, the crossover on
//! both. Laid out as `PANELS.md` says: one group per concern, its key control
//! in the bar, its rows in the inset.

use egui::{RichText, Ui};

use crate::app::StudioSpike;
use crate::host::commands::SharedState;
use crate::host::commands::{binaural, engine, render};
use crate::i18n::{t, tf};
use crate::ui::group::Group;
use crate::ui::help::{self, Help};
use crate::ui::section::Section;
use crate::ui::{theme, widgets};

use super::renderer_perf;

/// Drafts for the few file/path parameters in a generated backend form. Keys
/// include backend identity; storage is bounded to fields visible this frame
/// or the preceding frame, and a new renderer session drops every old draft.
#[derive(Default)]
pub struct BackendPathDrafts {
    entries: Vec<PathDraft>,
    frame: Option<u64>,
    epoch: Option<u64>,
    context: Option<crate::host::commands::layout_io::SessionToken>,
}
struct PathDraft {
    id: egui::Id,
    seen: u64,
    draft: crate::ui::text_draft::TextDraft,
}
impl BackendPathDrafts {
    fn discard_context(&mut self) {
        self.entries.clear();
    }
    fn sync_context(&mut self, host: &SharedState) -> bool {
        if self
            .context
            .as_ref()
            .is_none_or(|context| !context.is_current(host))
        {
            self.discard_context();
            self.context = Some(crate::host::commands::layout_io::SessionToken::new(host));
        }
        self.context
            .as_ref()
            .is_some_and(|context| context.is_current(host))
    }

    fn field(
        &mut self,
        id: egui::Id,
        epoch: u64,
        frame: u64,
    ) -> &mut crate::ui::text_draft::TextDraft {
        if self.epoch != Some(epoch) {
            self.entries.clear();
            self.epoch = Some(epoch);
        }
        if self.frame != Some(frame) {
            self.entries
                .retain(|entry| entry.seen >= frame.saturating_sub(1));
            self.frame = Some(frame);
        }
        let index = self
            .entries
            .iter()
            .position(|entry| entry.id == id)
            .unwrap_or_else(|| {
                self.entries.push(PathDraft {
                    id,
                    seen: frame,
                    draft: Default::default(),
                });
                self.entries.len() - 1
            });
        self.entries[index].seen = frame;
        &mut self.entries[index].draft
    }
}

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
        let (summary, embedded) = {
            let live = self.host.read();
            let mode = live
                .app
                .render_evaluation_mode_state
                .effective
                .clone()
                .or_else(|| live.app.render_evaluation_mode_state.selection.clone())
                .unwrap_or_else(|| "auto".to_owned());
            // `renderEvaluationMode`: the backend the engine runs, then the
            // mode it evaluates it in.
            let b = &live.app.render_backend_state;
            let backend_id = b
                .effective
                .clone()
                .or_else(|| b.selection.clone())
                .unwrap_or_else(|| "vbap".to_owned());
            let backend = b
                .effective_label
                .clone()
                .filter(|_| b.effective.as_deref() == Some(backend_id.as_str()))
                .unwrap_or_else(|| backend_label(&backend_id));
            let embedded = live
                .app
                .producer_capabilities
                .as_ref()
                .and_then(|c| c.get("variant"))
                .and_then(|v| v.as_str())
                == Some("embedded");
            (
                format!(
                    "{backend} / {}",
                    tf("renderer.summary", &[("mode", evaluation_label(&mode))])
                ),
                embedded,
            )
        };
        // The gauge's bar sits in the header, as `#rendererPerfWrap` does, so
        // it stays in view with the section folded; its numbers open the body.
        let perf = self.perf_snapshot();
        let mut section = Section::new("rendererSection", "section.renderer")
            .icon(&crate::ui::icons::SECTION_RENDERER)
            .summary(summary);
        if let Some(perf) = perf {
            section = section.header_widget(move |ui| renderer_perf::perf_bar(ui, &perf));
        }
        section.show(ui, |ui| {
            if let Some(perf) = &perf {
                renderer_perf::perf_readouts(ui, perf);
            }
            self.output_mode_row(ui);
            // The embedded host applies the output mode at player start.
            if embedded {
                widgets::note(ui, t("outputMode.mpvNote"));
            }
            ui.add_space(2.0);
            if let Some(tab) = widgets::tab_bar(
                ui,
                &self.renderer_tab,
                &[
                    (RendererTab::Renderer, t("rendererTabs.renderer")),
                    (RendererTab::Binaural, t("rendererTabs.binaural")),
                ],
            ) {
                self.renderer_tab = tab;
            }
            // Groups in the order their choices constrain one another: the
            // backend first, since it says which evaluation modes exist; the
            // two distance treatments it applies; how gains move between
            // frames; and last the crossover, which both tabs share.
            match self.renderer_tab {
                RendererTab::Renderer => {
                    self.backend_group(ui);
                    self.evaluation_group(ui);
                    self.distance_model_group(ui);
                    self.distance_diffuse_group(ui);
                    self.ramp_group(ui);
                }
                RendererTab::Binaural => self.binaural_tab(ui),
            }
            self.crossover_group(ui);
        });
    }

    fn output_mode_row(&mut self, ui: &mut Ui) {
        let current = {
            let live = self.host.read();
            OutputMode::from_state(live.app.binaural.as_ref())
        };
        let mut chosen = current;
        widgets::label_row_help(ui, t("outputMode.selectTitle"), "help.outputMode", |ui| {
            widgets::bounded_combo(ui, 160.0, |ui, w| {
                egui::ComboBox::from_id_salt("output-mode")
                    .selected_text(current.label())
                    .width(w)
                    .truncate()
                    .show_ui(ui, |ui| {
                        for mode in [
                            OutputMode::Speaker,
                            OutputMode::BinauralDirect,
                            OutputMode::BinauralCascaded,
                        ] {
                            ui.selectable_value(&mut chosen, mode, mode.label());
                        }
                    })
            });
        });
        if chosen == current {
            return;
        }
        // Not optimistic: the renderer's echo is what moves the select, so a
        // rejected change does not leave the UI lying.
        match chosen {
            OutputMode::Speaker => binaural::control_output_mode(&self.host, "speaker".into()),
            OutputMode::BinauralDirect => {
                binaural::control_output_mode(&self.host, "binaural".into());
                binaural::control_binaural_mode(&self.host, "direct".into());
            }
            OutputMode::BinauralCascaded => {
                binaural::control_output_mode(&self.host, "binaural".into());
                binaural::control_binaural_mode(&self.host, "cascaded".into());
            }
        }
    }

    // ── backend and its schema-generated parameters ──────────────────────

    /// Bar: the title (its help names the chosen backend), the recompute
    /// status, the backend select and — when it differs — what the engine
    /// actually runs. Inset: the backend's own parameters.
    fn backend_group(&mut self, ui: &mut Ui) {
        let (selection, effective, effective_label, available, values, frozen, status) = {
            let live = self.host.read();
            let b = &live.app.render_backend_state;
            (
                b.selection.clone().unwrap_or_else(|| "vbap".to_owned()),
                b.effective.clone(),
                b.effective_label.clone(),
                b.available_backends.clone(),
                b.backend_param_values_by_id.clone(),
                b.frozen_speakers,
                vbap_status(
                    &live.app.recompute_error,
                    live.app.vbap_recomputing,
                    live.recompute_timed_out,
                ),
            )
        };
        let backends = backend_list(&available);
        let selected_label = backends
            .iter()
            .find(|(id, _)| *id == selection)
            .map(|(_, label)| label.clone())
            .unwrap_or_else(|| selection.clone());
        // What the engine runs, when that is not what was asked for: a build
        // that failed, or a backend the engine substituted.
        let running = effective
            .as_ref()
            .filter(|id| **id != selection)
            .map(|id| effective_label.clone().unwrap_or_else(|| backend_label(id)));
        // The script backend follows the selection so its file field stays
        // reachable while its build fails; everything else follows what the
        // engine actually runs.
        let visible = if selection == "script" {
            selection.clone()
        } else {
            effective.clone().unwrap_or_else(|| selection.clone())
        };
        let mut chosen = selection.clone();
        let mut group =
            Group::new(t("backend.title")).overlay(|| backend_overlay(&selection, &backends));
        // Only while there is something to say: at rest the web shows an em
        // dash, which next to a title reads as punctuation.
        if let Some((text, colour)) = status {
            group = group.status(text, colour);
        }
        group
            .actions(|ui| {
                if let Some(running) = &running {
                    ui.add(
                        egui::Label::new(
                            RichText::new(running)
                                .size(theme::FONT_SIZE_SMALL)
                                .color(theme::TEXT_MUTED),
                        )
                        .truncate(),
                    );
                }
                ui.add_enabled_ui(!frozen, |ui| {
                    widgets::bounded_combo(ui, 150.0, |ui, w| {
                        egui::ComboBox::from_id_salt("render-backend")
                            .selected_text(&selected_label)
                            .width(w)
                            .truncate()
                            .show_ui(ui, |ui| {
                                for (id, label) in &backends {
                                    ui.selectable_value(&mut chosen, id.clone(), label);
                                }
                            })
                    });
                });
            })
            .show(ui, |ui| {
                // `hybrid` has a bespoke panel of its own, not a generated one.
                if visible == "hybrid" {
                    self.hybrid_block(ui, &available, &values);
                } else {
                    self.backend_params_for(ui, &visible, &available, &values);
                }
            });
        if chosen != selection && !chosen.is_empty() {
            render::control_render_backend(&self.host, chosen);
        }
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
        let context_current = self.backend_path_edits.sync_context(&self.host);
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
            // The backend's own description, or the Studio's translation of
            // it; opened from the parameter's name, not always on show.
            let help_text = param_help(key, spec);
            let help = Help::text(
                ("backend-param", backend, key),
                help_text.as_deref().unwrap_or(""),
            );
            let kind = spec.get("kind");
            let kind_type = kind
                .and_then(|k| k.get("type"))
                .and_then(|v| v.as_str())
                .unwrap_or("float");
            let sent = match kind_type {
                "bool" => {
                    let mut on = value.as_bool().unwrap_or(false);
                    widgets::switch_row_help(ui, &label, help, &mut on)
                        .then(|| serde_json::json!(on))
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
                    widgets::label_row_help(ui, &label, help, |ui| {
                        widgets::bounded_combo(ui, 150.0, |ui, w| {
                            egui::ComboBox::from_id_salt(("backend-param", key))
                                .selected_text(
                                    options
                                        .iter()
                                        .find(|(v, _)| *v == current)
                                        .map(|(_, l)| l.clone())
                                        .unwrap_or_else(|| current.clone()),
                                )
                                .width(w)
                                .truncate()
                                .show_ui(ui, |ui| {
                                    for (v, l) in &options {
                                        ui.selectable_value(&mut chosen, v.clone(), l);
                                    }
                                })
                        });
                    });
                    (chosen != current).then(|| serde_json::json!(chosen))
                }
                "path" | "file" => {
                    let source = value.as_str().unwrap_or("");
                    let mut committed = None;
                    let extensions: Vec<String> = kind
                        .and_then(|k| k.get("extensions"))
                        .and_then(|v| v.as_array())
                        .map(|list| {
                            list.iter()
                                .filter_map(|e| e.as_str().map(str::to_owned))
                                .collect()
                        })
                        .unwrap_or_default();
                    let editable = kind
                        .and_then(|k| k.get("editable"))
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    let language = kind
                        .and_then(|k| k.get("language"))
                        .and_then(|v| v.as_str())
                        .map(str::to_owned);
                    // A path only means something to the renderer when it is
                    // the renderer's own filesystem, so Browse is offered only
                    // then; the editor works either way, because it moves the
                    // bytes rather than the path.
                    let local = crate::host::commands::app::renderer_is_local(&self.host);
                    let mut browse = false;
                    let mut edit = false;
                    let epoch = self
                        .osc_stats
                        .connection_epoch
                        .load(std::sync::atomic::Ordering::Relaxed);
                    ui.add_enabled_ui(context_current, |ui| {
                        widgets::label_row_help(ui, &label, help, |ui| {
                            if editable {
                                edit = ui.button(t("backend.file.edit")).clicked();
                            }
                            if local {
                                browse = ui.button(t("backend.file.browse")).clicked();
                            }
                            let draft = self.backend_path_edits.field(
                                ui.make_persistent_id(("backend-file", backend, key, epoch)),
                                epoch,
                                ui.ctx().cumulative_frame_nr(),
                            );
                            if edit || browse {
                                draft.discard();
                            }
                            let hint = match extensions.first() {
                                Some(ext) => format!("name.{ext}"),
                                None if kind_type == "path" => "/path/to/backend.lua".to_owned(),
                                None => "name.ext".to_owned(),
                            };
                            committed = draft.show(
                                ui,
                                ("backend-file", backend, key, epoch),
                                source,
                                &hint,
                                120.0,
                                true,
                            );
                        });
                    });
                    if edit {
                        self.open_script_editor(backend, key, language, extensions.clone());
                    }
                    if browse {
                        self.pick_files(
                            ui.ctx(),
                            crate::ui::file_dialogs::Purpose::Backend {
                                backend: backend.to_owned(),
                                key: key.to_owned(),
                            },
                            &extensions,
                        );
                    }
                    committed.map(serde_json::Value::String)
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
                    widgets::value_slider_help(
                        ui,
                        &label,
                        help,
                        &mut number,
                        min..=max,
                        step,
                        move |v| {
                            if is_int {
                                format!("{}", v.round() as i64)
                            } else {
                                format!("{v:.3}")
                            }
                        },
                    )
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
                if matches!(kind_type, "path" | "file") {
                    if let Some(context) = &self.backend_path_edits.context {
                        context.with_current(&self.host, || {
                            self.send_backend_param(backend, key, value)
                        });
                    }
                } else {
                    self.send_backend_param(backend, key, value);
                }
            }
        }
    }

    /// `sendBackendParam`: no optimistic write, the renderer echoes the value.
    fn send_backend_param(&self, backend: &str, key: &str, value: serde_json::Value) {
        render::control_backend_param(&self.host, key.to_owned(), value, Some(backend.to_owned()));
    }

    // ── evaluation ───────────────────────────────────────────────────────

    /// Bar: the mode select and, while the choice is `auto`, the mode it
    /// resolved to. Inset: the grid of the precomputed mode in force, then
    /// the interpolation switch and the size intervals — nothing at all in
    /// realtime with a backend that cannot size events.
    fn evaluation_group(&mut self, ui: &mut Ui) {
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
            let live = self.host.read();
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
        // What the engine resolved the choice to, when that says more than
        // the choice itself.
        let resolved = effective
            .as_ref()
            .filter(|mode| **mode != selection)
            .map(|mode| evaluation_label(mode));
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
        // Hidden only when the backend says it cannot size events.
        let supports_size = caps.as_ref().is_none_or(|c| c.supports_spread);

        let mut chosen = selection.clone();
        Group::new(t("evaluation.title"))
            .info("evaluation")
            .actions(|ui| {
                if let Some(resolved) = resolved {
                    ui.label(
                        RichText::new(resolved)
                            .size(theme::FONT_SIZE_SMALL)
                            .color(theme::TEXT_MUTED),
                    );
                }
                widgets::bounded_combo(ui, 150.0, |ui, w| {
                    egui::ComboBox::from_id_salt("evaluation-mode")
                        .selected_text(evaluation_label(&selection))
                        .width(w)
                        .truncate()
                        .show_ui(ui, |ui| {
                            for mode in &allowed {
                                ui.selectable_value(
                                    &mut chosen,
                                    mode.clone(),
                                    evaluation_label(mode),
                                );
                            }
                        })
                });
            })
            .show(ui, |ui| {
                if show_cartesian {
                    self.cartesian_grid(ui, &cartesian, allow_neg_z, meters_per_unit);
                }
                if show_polar {
                    self.polar_grid(ui, &polar, allow_neg_z);
                }
                if show_cartesian || show_polar {
                    let mut on = interpolation;
                    if widgets::switch_row_help(
                        ui,
                        t("vbap.positionInterpolation"),
                        "help.vbap.positionInterpolation",
                        &mut on,
                    ) {
                        render::control_render_evaluation_position_interpolation(
                            &self.host,
                            i32::from(on),
                        );
                    }
                }
                if supports_size {
                    let mut value = intervals;
                    widgets::label_row_help(
                        ui,
                        t("evaluation.objectSizeIntervals"),
                        "help.eval.objectSizeIntervals",
                        |ui| {
                            if ui
                                .add(egui::DragValue::new(&mut value).range(0..=u32::MAX))
                                .changed()
                            {
                                render::control_render_evaluation_object_size_intervals(
                                    &self.host,
                                    value as i32,
                                );
                            }
                        },
                    );
                }
            });
        if chosen != selection && allowed.contains(&chosen) {
            render::control_render_evaluation_mode(&self.host, chosen);
        }
    }

    /// The cartesian grid's four counts and the step each makes.
    fn cartesian_grid(
        &mut self,
        ui: &mut Ui,
        cartesian: &crate::model::app_state::VbapCartesian,
        allow_neg_z: Option<bool>,
        meters_per_unit: f64,
    ) {
        help::label(
            ui,
            RichText::new(t("eval.cartesianGrid"))
                .size(theme::FONT_SIZE_SMALL)
                .color(theme::TEXT_MUTED),
            "help.eval.cartesianGrid",
        );
        help::card(ui, "help.eval.cartesianGrid");
        ui.horizontal(|ui| {
            // Steps are the room extent over the count: 2 units across X
            // and Y, 1 unit up.
            self.grid_field(
                ui,
                "X",
                cartesian.x_size,
                1,
                render::control_render_evaluation_cartesian_x_size,
            );
            self.grid_field(
                ui,
                "Y",
                cartesian.y_size,
                1,
                render::control_render_evaluation_cartesian_y_size,
            );
            self.grid_field(
                ui,
                "Z+",
                cartesian.z_size,
                1,
                render::control_render_evaluation_cartesian_z_size,
            );
            self.grid_field(
                ui,
                "Z-",
                cartesian.z_neg_size,
                0,
                render::control_render_evaluation_cartesian_z_neg_size,
            );
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

    /// The polar grid's three resolutions, the step each makes, and the
    /// distance the grid reaches.
    fn polar_grid(
        &mut self,
        ui: &mut Ui,
        polar: &crate::model::app_state::VbapPolar,
        allow_neg_z: Option<bool>,
    ) {
        help::label(
            ui,
            RichText::new(t("eval.polarGrid"))
                .size(theme::FONT_SIZE_SMALL)
                .color(theme::TEXT_MUTED),
            "help.eval.polarGrid",
        );
        help::card(ui, "help.eval.polarGrid");
        ui.horizontal(|ui| {
            self.grid_field(
                ui,
                "az",
                polar.azimuth_resolution,
                1,
                render::control_render_evaluation_polar_azimuth_resolution,
            );
            self.grid_field(
                ui,
                "el",
                polar.elevation_resolution,
                1,
                render::control_render_evaluation_polar_elevation_resolution,
            );
            self.grid_field(
                ui,
                "d",
                polar.distance_res,
                1,
                render::control_render_evaluation_polar_distance_res,
            );
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
                render::control_render_evaluation_polar_distance_max(&self.host, distance_max);
            }
        });
    }

    /// One integer field of an evaluation grid. `floor` is the smallest value
    /// the renderer accepts (1 everywhere but the negative-Z count).
    fn grid_field(
        &mut self,
        ui: &mut Ui,
        placeholder: &str,
        current: Option<u32>,
        floor: u32,
        send: fn(&SharedState, i32),
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
                send(&self.host, value.max(floor) as i32);
            }
        });
    }

    // ── distance, ramp, crossover ────────────────────────────────────────

    /// Bar: the model select. Inset: its metric, once there is a model.
    fn distance_model_group(&mut self, ui: &mut Ui) {
        let (value, metric) = {
            let live = self.host.read();
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
        let mut chosen = value.clone();
        let picked_metric = Group::new(t("distance.model"))
            .overlay(|| help::Overlay::keys("distance.modelInfoTitle", "distance.modelInfoBody"))
            .actions(|ui| {
                widgets::bounded_combo(ui, 150.0, |ui, w| {
                    egui::ComboBox::from_id_salt("distance-model")
                        .selected_text(t(DISTANCE_MODELS
                            .iter()
                            .find(|(id, _)| *id == value)
                            .map(|(_, key)| *key)
                            .unwrap_or("distance.model.none")))
                        .width(w)
                        .truncate()
                        .show_ui(ui, |ui| {
                            for (id, key) in DISTANCE_MODELS {
                                ui.selectable_value(&mut chosen, (*id).to_owned(), t(key));
                            }
                        })
                });
            })
            .show(ui, |ui| {
                // The metric only means something once a model is applied.
                (value != "none")
                    .then(|| {
                        self.metric_row(
                            ui,
                            "distance-model-metric",
                            "help.distanceModel.metric",
                            &metric,
                        )
                    })
                    .flatten()
            });
        if chosen != value {
            render::control_distance_model(&self.host, chosen);
        }
        if let Some(metric) = picked_metric {
            render::control_distance_model_metric(&self.host, metric);
        }
    }

    /// Bar: the effect's switch. Inset: its parameters, while it is on.
    fn distance_diffuse_group(&mut self, ui: &mut Ui) {
        let state = {
            let live = self.host.read();
            live.app.distance_diffuse.clone()
        };
        let enabled = state.enabled.unwrap_or(false);
        let mut on = enabled;
        Group::new(t("distance.title"))
            .info("distance")
            .actions(|ui| {
                widgets::switch(ui, &mut on, t("distance.title"));
            })
            .show(ui, |ui| {
                // The parameters collapse with the effect, as in the web panel.
                if !enabled {
                    return;
                }
                let metric = state.metric.clone().unwrap_or_else(|| "spherical".into());
                if let Some(chosen) = self.metric_row(
                    ui,
                    "distance-diffuse-metric",
                    "help.distanceDiffuse.metric",
                    &metric,
                ) {
                    render::control_distance_diffuse_metric(&self.host, chosen);
                }
                self.mirror_axes_rows(ui, state.mirror_axes.unwrap_or_default());
                let mut threshold = state.threshold.unwrap_or(1.0) as f32;
                if widgets::value_slider_help(
                    ui,
                    t("distance.threshold"),
                    "help.distanceDiffuse.threshold",
                    &mut threshold,
                    0.1..=2.0,
                    0.01,
                    |v| format!("{v:.2}"),
                ) {
                    render::control_distance_diffuse_threshold(&self.host, threshold);
                }
                let mut curve = state.curve.unwrap_or(1.0) as f32;
                if widgets::value_slider_help(
                    ui,
                    t("distance.curve"),
                    "help.distanceDiffuse.curve",
                    &mut curve,
                    0.5..=2.0,
                    0.05,
                    |v| format!("{v:.2}"),
                ) {
                    render::control_distance_diffuse_curve(&self.host, curve);
                }
            });
        if on != enabled {
            render::control_distance_diffuse_enabled(&self.host, i32::from(on));
        }
    }

    /// "Mirror axes": the symmetry the flips make, named, and one switch per
    /// axis under it in the web's quieter sub-row style.
    fn mirror_axes_rows(&mut self, ui: &mut Ui, axes: crate::model::app_state::MirrorAxes) {
        widgets::label_row_help(
            ui,
            t("distance.mirrorAxes"),
            "help.distanceDiffuse.mirrorAxes",
            |ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(t(axes.symmetry_key()))
                            .size(theme::FONT_SIZE_SMALL)
                            .color(theme::TEXT_FAINT),
                    )
                    .truncate(),
                );
            },
        );
        let mut next = axes;
        for (key, on) in [
            ("distance.mirrorAxis.x", &mut next.x),
            ("distance.mirrorAxis.y", &mut next.y),
            ("distance.mirrorAxis.z", &mut next.z),
        ] {
            // `.switch-row` at 0.7rem in `#8fa6bd`.
            widgets::label_row(
                ui,
                RichText::new(t(key))
                    .size(theme::FONT_SIZE_SMALL)
                    .color(theme::TEXT_FAINT),
                |ui| widgets::switch(ui, on, t(key)),
            );
        }
        if next != axes {
            render::control_distance_diffuse_mirror_axes(&self.host, next);
        }
    }

    /// A group that is its select: how gains move between frames.
    fn ramp_group(&mut self, ui: &mut Ui) {
        let current = {
            let live = self.host.read();
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
        Group::new(t("renderer.rampTitle"))
            .info("rampMode")
            .actions(|ui| {
                widgets::bounded_combo(ui, 140.0, |ui, w| {
                    egui::ComboBox::from_id_salt("ramp-mode")
                        .selected_text(t(RAMP_MODES
                            .iter()
                            .find(|(id, _)| *id == current)
                            .map(|(_, key)| *key)
                            .unwrap_or("audio.rampModeFrame")))
                        .width(w)
                        .truncate()
                        .show_ui(ui, |ui| {
                            for (id, key) in RAMP_MODES {
                                ui.selectable_value(&mut chosen, (*id).to_owned(), t(key));
                            }
                        })
                });
            })
            .bar(ui);
        if chosen != current {
            engine::control_ramp_mode(&self.host, chosen);
        }
    }

    /// Shown on both tabs. Bar: the filter. Inset: the FIR's transition
    /// width while that is the filter, and what the renderer actually built.
    fn crossover_group(&mut self, ui: &mut Ui) {
        let (crossover, kind, transition) = {
            let live = self.host.read();
            (
                live.app.live_options.crossover.clone(),
                live.option_str("crossover_type")
                    .unwrap_or_else(|| "lr4".into()),
                live.option_f64("crossover_fir_transition_ratio")
                    .unwrap_or(0.5),
            )
        };
        let mut chosen = kind.clone();
        let mut ratio = transition as f32;
        let ratio_changed = Group::new(t("renderer.crossoverTitle"))
            .help("help.renderer.crossoverType")
            .actions(|ui| {
                widgets::bounded_combo(ui, 160.0, |ui, w| {
                    egui::ComboBox::from_id_salt("crossover-type")
                        .selected_text(t(if kind == "fir" {
                            "renderer.crossoverType.fir"
                        } else {
                            "renderer.crossoverType.lr4"
                        }))
                        .width(w)
                        .truncate()
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
                        })
                });
            })
            .show(ui, |ui| {
                let changed = kind == "fir"
                    && widgets::value_slider_help(
                        ui,
                        t("renderer.crossoverTransitionLabel"),
                        "help.renderer.crossoverTransition",
                        &mut ratio,
                        0.05..=2.0,
                        0.05,
                        |v| format!("{v:.2}"),
                    );
                widgets::note(ui, &crossover_info(crossover.as_ref()));
                changed
            });
        if chosen != kind {
            self.set_option("crossover_type", serde_json::json!(chosen));
        }
        if ratio_changed {
            self.set_option(
                "crossover_fir_transition_ratio",
                serde_json::json!(ratio as f64),
            );
        }
    }

    /// The spherical/Chebyshev select shared by both distance groups.
    fn metric_row(&mut self, ui: &mut Ui, id: &str, help: &str, current: &str) -> Option<String> {
        let mut chosen = current.to_owned();
        widgets::label_row_help(ui, t("distance.metric"), help, |ui| {
            widgets::bounded_combo(ui, 130.0, |ui, w| {
                egui::ComboBox::from_id_salt(id)
                    .selected_text(t(METRICS
                        .iter()
                        .find(|(v, _)| *v == current)
                        .map(|(_, key)| *key)
                        .unwrap_or("distance.metric.spherical")))
                    .width(w)
                    .truncate()
                    .show_ui(ui, |ui| {
                        for (v, key) in METRICS {
                            ui.selectable_value(&mut chosen, (*v).to_owned(), t(key));
                        }
                    })
            });
        });
        (chosen != current).then_some(chosen)
    }

    /// Ask the core whether an unanswered recompute has run out of time, and
    /// come back when it would: the deadline must not wait for a frame that
    /// something else happens to draw.
    pub(crate) fn check_recompute_ack(&mut self, ctx: &egui::Context) {
        let now = std::time::Instant::now();
        if let Some(deadline) = crate::host::commands::render::tick_recompute(&self.host, now) {
            ctx.request_repaint_after(deadline.saturating_duration_since(now));
        }
    }

    /// `control_option`: the registry message, which applies the value too.
    pub(crate) fn set_option(&mut self, key: &str, value: serde_json::Value) {
        engine::control_option(&self.host, key.to_owned(), value);
    }
}

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

/// `backendLabel`: the built-in names, or the raw id.
fn backend_label(id: &str) -> String {
    match id {
        "vbap" => "VBAP",
        "barycenter" => "Barycenter",
        "experimental_distance" => "Distance",
        "hybrid" => "Hybrid",
        other => other,
    }
    .to_owned()
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

/// `setBackendInfoModalOpen`: the generic intro, then a paragraph on the
/// selected backend when there is one to say (`help.backend.<id>`).
fn backend_overlay(selection: &str, backends: &[(String, String)]) -> help::Overlay {
    let mut overlay = help::Overlay::info("backend");
    let id = selection.trim();
    if let Some(specific) = crate::i18n::lookup(&format!("help.backend.{id}")) {
        let label = backends
            .iter()
            .find(|(value, _)| value == id)
            .map_or(id, |(_, label)| label.as_str());
        overlay.body = format!(
            "{}<br><br><strong>{label}</strong><br>{specific}",
            overlay.body
        );
    }
    overlay
}

fn param_help(key: &str, spec: &serde_json::Value) -> Option<String> {
    if let Some(translated) = crate::i18n::lookup(&format!("backendParamHelp.{key}")) {
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

/// `renderVbapStatus`: the engine's recompute state, the error it reported, or
/// our own silence timeout — a flag in the model, said here in the user's
/// language. `None` while nothing has happened yet.
fn vbap_status(
    error: &Option<String>,
    recomputing: Option<bool>,
    timed_out: bool,
) -> Option<(String, egui::Color32)> {
    match (error, recomputing, timed_out) {
        (Some(message), _, _) => Some((message.clone(), theme::ERROR)),
        (None, _, true) => Some((t("vbap.status.noAck").to_owned(), theme::ERROR)),
        (None, Some(true), _) => Some((t("vbap.status.computing").to_owned(), theme::WARN)),
        (None, Some(false), _) => Some((t("vbap.status.ready").to_owned(), theme::OK)),
        (None, None, _) => None,
    }
}

#[cfg(test)]
mod path_draft_tests {
    use super::*;
    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        }
    }
    fn frame(
        ctx: &egui::Context,
        drafts: &mut BackendPathDrafts,
        backend: &str,
        epoch: u64,
        source: &str,
        focus: bool,
        events: Vec<egui::Event>,
    ) -> Vec<(String, String)> {
        let mut sent = Vec::new();
        let mut output = ctx.run_ui(
            egui::RawInput {
                events,
                ..Default::default()
            },
            |ui| {
                for name in ["first", "second"] {
                    let id = ("backend-file", backend, name, epoch);
                    if focus && name == "first" {
                        ui.memory_mut(|memory| memory.request_focus(ui.make_persistent_id(id)));
                    }
                    if let Some(value) = drafts
                        .field(ui.make_persistent_id(id), epoch, ctx.cumulative_frame_nr())
                        .show(ui, id, source, "", 160.0, true)
                    {
                        sent.push((name.to_owned(), value));
                    }
                }
            },
        );
        output.textures_delta.clear();
        sent
    }
    #[test]
    fn two_file_fields_keep_typing_across_echoes_and_only_commit_the_edited_field() {
        let ctx = egui::Context::default();
        let mut drafts = BackendPathDrafts::default();
        frame(&ctx, &mut drafts, "backend", 0, "old.lua", true, vec![]);
        assert!(
            frame(
                &ctx,
                &mut drafts,
                "backend",
                0,
                "old.lua",
                false,
                vec![egui::Event::Paste("中é".into())]
            )
            .is_empty()
        );
        assert!(frame(&ctx, &mut drafts, "backend", 0, "server.lua", false, vec![]).is_empty());
        let sent = frame(
            &ctx,
            &mut drafts,
            "backend",
            0,
            "server.lua",
            false,
            vec![key(egui::Key::Enter)],
        );
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "first");
        assert!(sent[0].1.contains("中é"));
        assert!(frame(&ctx, &mut drafts, "backend", 0, &sent[0].1, false, vec![]).is_empty());
    }
    #[test]
    fn invalidated_profile_context_discards_typing_even_with_the_same_field_and_epoch() {
        let ctx = egui::Context::default();
        let mut drafts = BackendPathDrafts::default();
        frame(
            &ctx,
            &mut drafts,
            "backend",
            0,
            "profile-a.lua",
            true,
            vec![],
        );
        frame(
            &ctx,
            &mut drafts,
            "backend",
            0,
            "profile-a.lua",
            false,
            vec![egui::Event::Paste("old draft".into())],
        );
        // The adapter invalidates on a changed core SessionToken. The renderer
        // may keep exactly the same backend/key/transport epoch in profile B.
        drafts.discard_context();
        assert!(
            frame(
                &ctx,
                &mut drafts,
                "backend",
                0,
                "profile-b.lua",
                false,
                vec![key(egui::Key::Enter)]
            )
            .is_empty()
        );
    }
    #[test]
    fn changed_backend_or_session_never_commits_an_old_path_and_storage_is_bounded() {
        let ctx = egui::Context::default();
        let mut drafts = BackendPathDrafts::default();
        for change_session in [false, true] {
            frame(&ctx, &mut drafts, "before", 0, "old.lua", true, vec![]);
            frame(
                &ctx,
                &mut drafts,
                "before",
                0,
                "old.lua",
                false,
                vec![egui::Event::Text("X".into())],
            );
            let (backend, epoch) = if change_session {
                ("before", 1)
            } else {
                ("after", 0)
            };
            assert!(
                frame(
                    &ctx,
                    &mut drafts,
                    backend,
                    epoch,
                    "new.lua",
                    false,
                    vec![key(egui::Key::Enter)]
                )
                .is_empty()
            );
        }
        for index in 0..30 {
            frame(
                &ctx,
                &mut drafts,
                &format!("backend-{index}"),
                1,
                "",
                false,
                vec![],
            );
            assert!(drafts.entries.len() <= 4);
        }
    }
}
