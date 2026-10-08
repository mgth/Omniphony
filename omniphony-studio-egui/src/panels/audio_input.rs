//! The Audio Input panel (`ui/input-panel.js`, `controls/input.js`): how the
//! renderer is fed — through the decoder bridge's pipe, or from a PipeWire
//! node — and the Apply that makes a change take.
//!
//! The rows the web keeps as dead markup (backend, imported layout, channel
//! count, sample rate, map, LFE mode) are not ported; they belong to the
//! legacy PCM mode.

use egui::Ui;

use crate::app::StudioSpike;
use crate::host::commands::bridges::{self, Action, RowStatus};
use crate::host::commands::input;
use crate::i18n::{t, tf};
use crate::ui::group::Group;
use crate::ui::section::Section;
use crate::ui::text_draft::TextDraft;
use crate::ui::{theme, widgets};

#[derive(Default)]
pub(crate) struct InputEdits {
    bridge: TextDraft,
    pipe: TextDraft,
    node: TextDraft,
    description: TextDraft,
}

const MODES: &[(&str, &str)] = &[
    ("pipe_bridge", "input.mode.pipe_bridge"),
    ("pipewire", "input.mode.pipewire"),
];

const CLOCK_MODES: &[(&str, &str)] = &[
    ("dac", "input.clock.dac"),
    ("pipewire", "input.clock.pipewire"),
    ("upstream", "input.clock.upstream"),
];

impl StudioSpike {
    pub(crate) fn audio_input_section(&mut self, ui: &mut Ui) {
        let policy = crate::host::capabilities::ActionPolicy::of(&self.host);
        let bridges = bridges::Snapshot::read(&self.host);
        let (mode, active, pipe, clock, error, node, description, pending) = {
            let live = self.host.read();
            (
                live.app
                    .input_mode
                    .clone()
                    .unwrap_or_else(|| "pipe_bridge".to_owned()),
                live.app.input_active_mode.clone(),
                live.app.orender_input_pipe.clone().unwrap_or_default(),
                live.app
                    .live_input
                    .clock_mode
                    .clone()
                    .unwrap_or_else(|| "dac".to_owned()),
                live.app.input_error.clone(),
                live.app.live_input.node.clone().unwrap_or_default(),
                live.app.live_input.description.clone().unwrap_or_default(),
                live.app.input_apply_pending.unwrap_or(0) != 0,
            )
        };
        let pipewire = mode == "pipewire";
        let summary = if pipewire {
            tf(
                "input.summary.pipewire",
                &[
                    ("requested", mode_label(&mode)),
                    ("active", mode_label(active.as_deref().unwrap_or(""))),
                    ("clock", &clock),
                ],
            )
        } else {
            tf(
                "input.summary.bridge",
                &[
                    ("requested", mode_label(&mode)),
                    ("active", mode_label(active.as_deref().unwrap_or(""))),
                ],
            )
        };
        Section::new("audioInputSection", "section.audioInput")
            .icon(&crate::ui::icons::SECTION_AUDIO_INPUT)
            .info("input")
            .summary(summary)
            .show(ui, |ui| {
                // Status: what was asked for, what is running, and whether a
                // change is still waiting for its Apply.
                let mut status = tf(
                    "input.status.bridge",
                    &[
                        ("requested", mode_label(&mode)),
                        ("active", mode_label(active.as_deref().unwrap_or(""))),
                        (
                            "pipe",
                            if pipe.is_empty() {
                                "—"
                            } else {
                                pipe.as_str()
                            },
                        ),
                        (
                            "sync",
                            if pending {
                                t("input.sync.pending")
                            } else {
                                t("input.sync.synced")
                            },
                        ),
                    ],
                );
                if let Some(error) = &error {
                    status.push_str(&tf("input.status.error", &[("error", error)]));
                }
                widgets::note(ui, &status);

                // The bridges are exempt from the connection lock: they are
                // how a missing bridge gets fixed.
                match &bridges.list {
                    Some(list) => self.bridge_list_group(ui, list, bridges.local),
                    // An engine that publishes no list: its single path.
                    None => {
                        if let Some(path) = text_row(
                            ui,
                            "bridge-path",
                            t("input.bridgeBinary"),
                            "help.input.bridge",
                            &mut self.input_edits.bridge,
                            &bridges.single_path,
                            t("input.autoDetect"),
                        ) {
                            input::set_render_bridge_path(&self.host, path);
                        }
                    }
                }

                if !policy.input {
                    return;
                }
                ui.add_enabled_ui(policy.renderer_ready, |ui| {
                    let mut chosen = mode.clone();
                    widgets::label_row_help(ui, t("input.mode"), "help.input.mode", |ui| {
                        widgets::bounded_combo(ui, 150.0, |ui, w| {
                            egui::ComboBox::from_id_salt("input-mode")
                                .selected_text(mode_label(&mode))
                                .width(w)
                                .truncate()
                                .show_ui(ui, |ui| {
                                    for (id, key) in MODES {
                                        ui.selectable_value(&mut chosen, (*id).to_owned(), t(key));
                                    }
                                })
                        });
                    });
                    if chosen != mode {
                        input::set_input_mode(&self.host, chosen);
                    }

                    // The fields of the mode chosen, in a group of their own
                    // (`#inputLiveFields` / `#inputBridgeFields`).
                    if pipewire {
                        Group::new(t("input.liveSource")).show(ui, |ui| {
                            if let Some(node) = text_row(
                                ui,
                                "input-node",
                                t("input.node"),
                                "help.input.node",
                                &mut self.input_edits.node,
                                &node,
                                "omniphony",
                            ) {
                                input::set_live_input_node(&self.host, node);
                            }
                            if let Some(description) = text_row(
                                ui,
                                "input-description",
                                t("input.description"),
                                "help.input.description",
                                &mut self.input_edits.description,
                                &description,
                                "Omniphony Bridge Input",
                            ) {
                                input::set_live_input_description(&self.host, description);
                            }
                            let mut chosen_clock = clock.clone();
                            widgets::label_row_info_keys(
                                ui,
                                t("input.clock"),
                                "input.clockInfoTitle",
                                "input.clockInfoBody",
                                |ui| {
                                    widgets::bounded_combo(ui, 150.0, |ui, w| {
                                        egui::ComboBox::from_id_salt("input-clock")
                                            .selected_text(t(CLOCK_MODES
                                                .iter()
                                                .find(|(id, _)| *id == clock)
                                                .map(|(_, key)| *key)
                                                .unwrap_or("input.clock.dac")))
                                            .width(w)
                                            .truncate()
                                            .show_ui(ui, |ui| {
                                                for (id, key) in CLOCK_MODES {
                                                    ui.selectable_value(
                                                        &mut chosen_clock,
                                                        (*id).to_owned(),
                                                        t(key),
                                                    );
                                                }
                                            })
                                    });
                                },
                            );
                            if chosen_clock != clock {
                                // Held until Apply: the clock cannot change under a
                                // running bridge.
                                input::set_live_input_clock_mode(&self.host, chosen_clock);
                            }
                        });
                    } else {
                        Group::new(t("input.bridgeInput")).show(ui, |ui| {
                            if let Some(path) = text_row(
                                ui,
                                "input-pipe",
                                t("input.pipe"),
                                "help.input.pipe",
                                &mut self.input_edits.pipe,
                                &pipe,
                                t("input.autoDetect"),
                            ) {
                                input::set_orender_input_pipe(&self.host, path);
                            }
                        });
                    }

                    let label = if pending {
                        t("input.applyPending")
                    } else {
                        t("input.apply")
                    };
                    if ui.button(label).clicked() {
                        input::apply_input(&self.host, &mode, active.as_deref());
                    }
                });
                // Values staged some other way (another client, a script)
                // and still waiting: the schema-driven Apply of the group.
                self.staged_apply_row(ui, "live_input");
            });
    }
}

impl StudioSpike {
    /// The decoder bridges, in load order: each with its families or why it
    /// failed, moved, removed, or added with the file picker (a renderer on
    /// this machine) or by its path (one on another). With none asked for,
    /// what auto-discovery loaded, read-only. An edit is unsaved: Apply
    /// restarts on it, Save keeps it (docs/persistence-policy.md).
    fn bridge_list_group(&mut self, ui: &mut Ui, list: &bridges::BridgeList, local: bool) {
        let mut action = None;
        let mut browse = false;
        let mut group = Group::new(t("input.bridges")).help("help.input.bridges");
        if list.restart_pending {
            group = group.status(t("input.bridges.pending"), theme::WARN);
        }
        group
            .actions(|ui| {
                browse = ui
                    .add_enabled(local, egui::Button::new(t("input.bridges.add")))
                    .on_hover_text(t("input.bridges.addTitle"))
                    .on_disabled_hover_text(t("input.bridges.addRemote"))
                    .clicked();
            })
            .show(ui, |ui| {
                if list.is_auto() {
                    widgets::note(
                        ui,
                        if list.rows.is_empty() {
                            t("input.bridges.autoNone")
                        } else {
                            t("input.bridges.auto")
                        },
                    );
                }
                let last = list.requested.len().saturating_sub(1);
                for row in &list.rows {
                    let (detail, colour) = row_detail(row, list.is_auto());
                    widgets::list_entry(ui, row.file_name(), &row.path, &detail, colour, |ui| {
                        let Some(index) = row.position else {
                            return;
                        };
                        // Right to left: remove, then down, then up.
                        if ui
                            .button("✕")
                            .on_hover_text(t("input.bridges.remove"))
                            .clicked()
                        {
                            action = Some(Action::Remove(index));
                        }
                        if ui
                            .add_enabled(index < last, egui::Button::new("⏷"))
                            .on_hover_text(t("input.bridges.down"))
                            .clicked()
                        {
                            action = Some(Action::Move {
                                from: index,
                                to: index + 1,
                            });
                        }
                        if ui
                            .add_enabled(index > 0, egui::Button::new("⏶"))
                            .on_hover_text(t("input.bridges.up"))
                            .clicked()
                        {
                            action = Some(Action::Move {
                                from: index,
                                to: index - 1,
                            });
                        }
                    });
                }
                // A typed path: the picker's file is the Studio machine's,
                // which is not the renderer's when it runs elsewhere.
                if let Some(path) = text_row(
                    ui,
                    "bridge-add",
                    t("input.bridges.addPath"),
                    "help.input.bridgesAddPath",
                    &mut self.input_edits.bridge,
                    "",
                    t("input.bridges.pathHint"),
                ) {
                    action = Some(Action::Add(path));
                }
                widgets::note(ui, t("input.bridges.applyNote"));
            });
        if let Some(action) = action {
            bridges::apply(&self.host, action);
        }
        if browse {
            self.pick_files(
                ui.ctx(),
                crate::ui::file_dialogs::Purpose::Bridge,
                &[std::env::consts::DLL_EXTENSION.to_owned()],
            );
        }
    }
}

/// What a row says under its name, and in which colour.
fn row_detail(row: &bridges::Row, auto: bool) -> (String, egui::Color32) {
    match &row.status {
        RowStatus::Loaded { families } => {
            let families = if families.is_empty() {
                "—".to_owned()
            } else {
                families.join(", ")
            };
            if row.position.is_none() && !auto {
                // Taken out of the list, still loaded until the restart.
                (
                    tf("input.bridges.removed", &[("families", &families)]),
                    theme::TEXT_MUTED,
                )
            } else {
                (
                    tf("input.bridges.loaded", &[("families", &families)]),
                    theme::OK,
                )
            }
        }
        RowStatus::Failed { error } => (
            // One line: the row keeps its height, the hover has it whole.
            tf(
                "input.bridges.failed",
                &[("error", &error.trim().replace('\n', " "))],
            ),
            theme::ERROR,
        ),
        RowStatus::NotLoaded => (t("input.bridges.notLoaded").to_owned(), theme::WARN),
    }
}

fn mode_label(mode: &str) -> &'static str {
    match mode {
        "pipewire" => t("input.mode.pipewire"),
        "pipe_bridge" => t("input.mode.pipe_bridge"),
        _ => "—",
    }
}

/// Label plus a persistent text draft; paths can be empty to select auto.
fn text_row(
    ui: &mut Ui,
    key: &str,
    label: &str,
    help: &str,
    draft: &mut TextDraft,
    source: &str,
    hint: &str,
) -> Option<String> {
    widgets::label_row_help(ui, label, help, |ui| {
        draft.show(ui, key, source, hint, 170.0, true)
    })
}
