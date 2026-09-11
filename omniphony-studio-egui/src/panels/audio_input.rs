//! The Audio Input panel (`ui/input-panel.js`, `controls/input.js`): how the
//! renderer is fed — through the decoder bridge's pipe, or from a PipeWire
//! node — and the Apply that makes a change take.
//!
//! The rows the web keeps as dead markup (backend, imported layout, channel
//! count, sample rate, map, LFE mode) are not ported; they belong to the
//! legacy PCM mode.

use egui::Ui;

use crate::app::StudioSpike;
use crate::i18n::{t, tf};
use crate::ui::section::Section;
use crate::ui::widgets;

const MODES: &[(&str, &str)] = &[
    ("pipe_bridge", "input.mode.pipe_bridge"),
    ("pipewire_bridge", "input.mode.pipewire_bridge"),
];

const CLOCK_MODES: &[(&str, &str)] = &[
    ("dac", "input.clock.dac"),
    ("pipewire", "input.clock.pipewire"),
    ("upstream", "input.clock.upstream"),
];

impl StudioSpike {
    pub(crate) fn audio_input_section(&mut self, ui: &mut Ui) {
        let (mode, active, bridge, pipe, clock, error, node, description, pending) = {
            let live = self.live.lock().unwrap();
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
        let pipewire = mode == "pipewire_bridge";
        let summary = if pipewire {
            tf(
                "input.summary.pipewireBridge",
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
                    egui::ComboBox::from_id_salt("input-mode")
                        .selected_text(mode_label(&mode))
                        .width(150.0)
                        .show_ui(ui, |ui| {
                            for (id, key) in MODES {
                                ui.selectable_value(&mut chosen, (*id).to_owned(), t(key));
                            }
                        });
                });
                if chosen != mode {
                    {
                        let mut live = self.live.lock().unwrap();
                        live.app.input_mode = Some(chosen.clone());
                        if chosen == "pipewire_bridge" {
                            // The PipeWire path has its own defaults.
                            live.app.live_input.channels = Some(2);
                            live.app.live_input.sample_rate = Some(192_000);
                        }
                    }
                    self.send_input_config(false);
                }

                // The bridge path is exempt from the connection lock: it is
                // how a missing bridge gets fixed.
                let mut path = bridge.clone();
                widgets::label_row(ui, t("input.bridgeBinary"), |ui| {
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut path)
                                .desired_width(170.0)
                                .hint_text(t("input.autoDetect")),
                        )
                        .lost_focus()
                        && path != bridge
                    {
                        let value = path.trim().to_owned();
                        self.live.lock().unwrap().app.render_bridge_path =
                            (!value.is_empty()).then(|| value.clone());
                        self.ctl
                            .send_string("/omniphony/control/render/bridge_path", &value);
                    }
                });

                if pipewire {
                    let mut node = node.clone();
                    if text_row(ui, t("input.node"), &mut node, "omniphony") {
                        self.live.lock().unwrap().app.live_input.node =
                            (!node.trim().is_empty()).then(|| node.trim().to_owned());
                        self.send_input_config(false);
                    }
                    let mut description = description.clone();
                    if text_row(
                        ui,
                        t("input.description"),
                        &mut description,
                        "Omniphony Bridge Input",
                    ) {
                        self.live.lock().unwrap().app.live_input.description =
                            (!description.trim().is_empty()).then(|| description.trim().to_owned());
                        self.send_input_config(false);
                    }
                    let mut chosen_clock = clock.clone();
                    widgets::label_row(ui, t("input.clock"), |ui| {
                        egui::ComboBox::from_id_salt("input-clock")
                            .selected_text(t(CLOCK_MODES
                                .iter()
                                .find(|(id, _)| *id == clock)
                                .map(|(_, key)| *key)
                                .unwrap_or("input.clock.dac")))
                            .width(150.0)
                            .show_ui(ui, |ui| {
                                for (id, key) in CLOCK_MODES {
                                    ui.selectable_value(
                                        &mut chosen_clock,
                                        (*id).to_owned(),
                                        t(key),
                                    );
                                }
                            });
                    });
                    if chosen_clock != clock {
                        // Held until Apply: the clock cannot change under a
                        // running bridge.
                        self.live.lock().unwrap().app.live_input.clock_mode = Some(chosen_clock);
                    }
                } else {
                    let mut pipe_path = pipe.clone();
                    if text_row(ui, t("input.pipe"), &mut pipe_path, t("input.autoDetect")) {
                        let value = pipe_path.trim().to_owned();
                        self.live.lock().unwrap().app.orender_input_pipe =
                            (!value.is_empty()).then(|| value.clone());
                        self.ctl
                            .send_string("/omniphony/control/render/input_pipe", &value);
                    }
                }

                let label = if pending {
                    t("input.applyPending")
                } else {
                    t("input.apply")
                };
                if ui.button(label).clicked() {
                    self.apply_input(&mode, active.as_deref());
                }
            });
    }

    /// `sendInputConfig`: the whole input document, optionally applied.
    fn send_input_config(&mut self, apply: bool) {
        let payload = {
            let live = self.live.lock().unwrap();
            let input = &live.app.live_input;
            serde_json::json!({
                "mode": live.app.input_mode,
                "liveInput": {
                    "backend": input.backend,
                    "node": input.node,
                    "description": input.description,
                    "layout": input.layout,
                    "clockMode": input.clock_mode.clone().unwrap_or_else(|| "dac".into()),
                    "channels": input.channels.unwrap_or(2),
                    "sampleRate": input.sample_rate.unwrap_or(192_000),
                    "map": input.map.clone().unwrap_or_else(|| "7.1-fixed".into()),
                    "lfeMode": input.lfe_mode.clone().unwrap_or_else(|| "object".into()),
                }
            })
        };
        self.ctl
            .send_json("/omniphony/control/config/input", &payload);
        if apply {
            self.ctl
                .send_no_args("/omniphony/control/config/input/apply");
        }
    }

    /// Apply: a bridge that has to be (re)started needs its path and clock
    /// saved and the configuration reloaded; otherwise the input document is
    /// enough.
    fn apply_input(&mut self, mode: &str, active: Option<&str>) {
        let clock = {
            let live = self.live.lock().unwrap();
            live.app
                .live_input
                .clock_mode
                .clone()
                .unwrap_or_else(|| "dac".to_owned())
        };
        let needs_bootstrap = mode == "pipe_bridge"
            || (mode == "pipewire_bridge" && active != Some("pipewire_bridge"));
        if needs_bootstrap {
            let bridge = {
                let live = self.live.lock().unwrap();
                live.app.render_bridge_path.clone().unwrap_or_default()
            };
            self.ctl
                .send_string("/omniphony/control/render/bridge_path", &bridge);
            self.ctl
                .send_string("/omniphony/control/input/live/clock_mode", &clock);
            self.ctl.send_no_args("/omniphony/control/save_config");
            self.ctl.send_no_args("/omniphony/control/reload_config");
        } else {
            self.live.lock().unwrap().app.input_apply_pending = Some(1);
            self.ctl
                .send_string("/omniphony/control/input/live/clock_mode", &clock);
            self.send_input_config(true);
        }
    }
}

fn mode_label(mode: &str) -> &'static str {
    match mode {
        "pipewire_bridge" => t("input.mode.pipewire_bridge"),
        "pipe_bridge" => t("input.mode.pipe_bridge"),
        _ => "—",
    }
}

/// Label plus a text field that commits when it loses focus.
fn text_row(ui: &mut Ui, label: &str, value: &mut String, hint: &str) -> bool {
    let mut committed = false;
    let before = value.clone();
    widgets::label_row(ui, label, |ui| {
        committed = ui
            .add(
                egui::TextEdit::singleline(value)
                    .desired_width(170.0)
                    .hint_text(hint),
            )
            .lost_focus();
    });
    committed && *value != before
}
