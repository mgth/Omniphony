//! The Audio Input panel (`ui/input-panel.js`, `controls/input.js`): how the
//! renderer is fed — through the decoder bridge's pipe, or from a PipeWire
//! node — and the Apply that makes a change take.
//!
//! The rows the web keeps as dead markup (backend, imported layout, channel
//! count, sample rate, map, LFE mode) are not ported; they belong to the
//! legacy PCM mode.

use egui::Ui;

use crate::app::StudioSpike;
use crate::host::commands::input;
use crate::i18n::{t, tf};
use crate::ui::group::Group;
use crate::ui::section::Section;
use crate::ui::text_draft::TextDraft;
use crate::ui::widgets;

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
        let (mode, active, bridge, pipe, clock, error, node, description, pending) = {
            let live = self.host.read();
            (
                live.app
                    .input_mode
                    .clone()
                    .unwrap_or_else(|| "pipe_bridge".to_owned()),
                live.app.input_active_mode.clone(),
                live.app.render_bridge_path.clone().unwrap_or_default(),
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

                // The bridge path is exempt from the connection lock: it is
                // how a missing bridge gets fixed.
                if let Some(path) = text_row(
                    ui,
                    "bridge-path",
                    t("input.bridgeBinary"),
                    "help.input.bridge",
                    &mut self.input_edits.bridge,
                    &bridge,
                    t("input.autoDetect"),
                ) {
                    input::set_render_bridge_path(&self.host, path);
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
