//! The speaker editor (`#speakerEditSection`, `speakers.js`,
//! `listeners/speaker-editor-listeners.js`, `controls/speaker-test.js`): the
//! Edit and Test tabs of the selected speaker.
//!
//! Every edit goes into the layout document as a `speakerEdits` entry and is
//! then applied, exactly like the Tauri commands; the delay is the one field
//! that belongs to the speakers document instead.

use egui::{RichText, Ui};

use crate::app::StudioSpike;
use crate::host::commands::{gain, speakers};
use crate::host::services::speaker_test;
use crate::i18n::t;
use crate::model::layouts::Speaker;
use crate::ui::{help, theme, widgets};
use crate::view::gizmos::EditMode;

/// Which tab of the editor is showing (`body.speaker-tab-test`).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum SpeakerTab {
    #[default]
    Edit,
    Test,
}

/// Coordinate mode of the editor's two tables.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CoordMode {
    Cartesian,
    Polar,
}

/// A burst stops itself after two seconds; a toggle stops after a minute so a
/// test the user walked away from does not keep the room busy
/// (`speaker-test.js`).
/// The renderer expires the idle-feed arm after a keepalive window.

const TEST_MODES: &[(&str, &str)] = &[
    ("toggle", "speaker.testMode.toggle"),
    ("burst", "speaker.testMode.burst"),
    ("hold", "speaker.testMode.hold"),
];

const TEST_ISOLATIONS: &[(&str, &str)] = &[
    ("test_only", "speaker.testIsolation.testOnly"),
    ("with_programme", "speaker.testIsolation.withProgramme"),
    ("test_only_solo", "speaker.testIsolation.solo"),
];

/// `SPEED_OF_SOUND_M_S` of the web's delay tools.
const SPEED_OF_SOUND_M_S: f64 = 343.0;
/// `DEFAULT_SAMPLE_RATE_HZ`: the web converts delays to samples at 48 kHz,
/// whatever the output runs at, and the two readouts have to agree.
const DELAY_SAMPLE_RATE_HZ: f64 = 48_000.0;

/// The editor's two bulk delay tools (`speakerEditAutoDelayBtn`,
/// `speakerEditDelayToDistanceBtn`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DelayTool {
    /// Delay every speaker so all arrive with the farthest one.
    CalcDelays,
    /// Move every speaker along its direction until its distance matches its
    /// delay, the farthest speaker being the reference.
    DelayToDistance,
}

impl DelayTool {
    fn confirm_key(self) -> &'static str {
        match self {
            Self::CalcDelays => "confirm.calcDelays",
            Self::DelayToDistance => "confirm.delayToDist",
        }
    }

    fn label_key(self) -> &'static str {
        match self {
            Self::CalcDelays => "speaker.calcDelays",
            Self::DelayToDistance => "speaker.delayToDist",
        }
    }
}

/// `computeAndApplySpeakerDelays`: the delay, in ms rounded to the µs, that
/// aligns each speaker at `distances_m` with the farthest one.
fn aligned_delays_ms(distances_m: &[f64]) -> Vec<f64> {
    let farthest = distances_m.iter().copied().fold(0.0, f64::max);
    distances_m
        .iter()
        .map(|d| {
            let ms = ((farthest - d) / SPEED_OF_SOUND_M_S * 1000.0).max(0.0);
            (ms * 1000.0).round() / 1000.0
        })
        .collect()
}

/// `adjustSpeakerDistancesFromDelays`: the distance, in room units, each
/// speaker should sit at so that its delay is the path it is short of the
/// farthest speaker. Never closer than 0.01.
fn distances_from_delays(distances_m: &[f64], delays_ms: &[f64], scale_m: f64) -> Vec<f64> {
    let reference = distances_m.iter().copied().fold(0.01, f64::max);
    delays_ms
        .iter()
        .map(|delay| {
            let shortfall = delay.max(0.0) / 1000.0 * SPEED_OF_SOUND_M_S;
            ((reference - shortfall) / scale_m).max(0.01)
        })
        .collect()
}

impl StudioSpike {
    /// Shown only while a speaker is selected, like the web editor.
    pub(crate) fn speaker_editor(&mut self, ui: &mut Ui) {
        let Some(index) = self.selection.speaker else {
            return;
        };
        let (speaker, count, frozen, scale_m) = {
            let live = self.host.read();
            let speakers = live.selected_speakers();
            let Some(speaker) = speakers.get(index).cloned() else {
                return;
            };
            (
                speaker,
                speakers.len(),
                live.app.render_backend_state.frozen_speakers,
                live.app.room_ratio.scale_m.max(0.001),
            )
        };
        ui.add_space(theme::PANEL_GAP);
        ui.separator();
        ui.label(
            RichText::new(t("section.speakerEditor"))
                .size(theme::FONT_SIZE_SECTION)
                .color(theme::TEXT_STRONG),
        );
        self.speaker_tabs(ui);
        self.layout_row(ui, index, count, frozen);
        match self.speaker_tab {
            SpeakerTab::Edit => self.speaker_edit_tab(ui, index, &speaker, scale_m, frozen),
            SpeakerTab::Test => self.speaker_test_tab(ui, index),
        }
    }

    fn speaker_tabs(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            for (tab, key) in [
                (SpeakerTab::Edit, "speakerTabs.edit"),
                (SpeakerTab::Test, "speakerTabs.test"),
            ] {
                let active = self.speaker_tab == tab;
                if ui.selectable_label(active, t(key)).clicked() {
                    self.speaker_tab = tab;
                    if tab == SpeakerTab::Edit {
                        // Leaving the test pane stops whatever it started.
                        self.stop_speaker_test();
                    }
                }
            }
        });
    }

    /// Reorder and delete. Both rewrite the layout, so both are refused while
    /// the backend has the speakers frozen.
    fn layout_row(&mut self, ui: &mut Ui, index: usize, count: usize, frozen: bool) {
        let clicked = widgets::label_buttons_help(
            ui,
            t("speaker.layout"),
            "help.speaker.layout",
            &[
                (t("speaker.up"), !frozen && index > 0),
                (t("speaker.down"), !frozen && index + 1 < count),
                (t("speaker.delete"), !frozen),
            ],
        );
        match clicked {
            Some(0) => self.move_speaker(index, index - 1),
            Some(1) => self.move_speaker(index, index + 1),
            Some(2) => {
                speakers::control_speakers_remove(&self.host, index as i32);
                self.selection.speaker = None;
            }
            _ => {}
        }
    }

    fn speaker_edit_tab(
        &mut self,
        ui: &mut Ui,
        index: usize,
        speaker: &Speaker,
        scale_m: f64,
        frozen: bool,
    ) {
        let id = index as i32;
        // While the gizmo holds this speaker, the readouts follow the pointer
        // rather than the state, as the web's editor did during a drag.
        let held = self.speaker_at_edit_pin(index, speaker);
        let speaker = held.as_ref().unwrap_or(speaker);
        ui.add_enabled_ui(!frozen, |ui| {
            // Name.
            let mut name = speaker.id.clone();
            widgets::label_row_help(ui, t("common.name"), "help.speaker.name", |ui| {
                if ui
                    .add(egui::TextEdit::singleline(&mut name).desired_width(150.0))
                    .lost_focus()
                    && name.trim() != speaker.id
                    && !name.trim().is_empty()
                {
                    self.edit_speaker(id, "name", serde_json::json!(name.trim()));
                }
            });

            // Coordinates: the two tables of the web editor, normalised on one
            // row and metres on the next. Metres are the normalised value
            // times the room scale, so only one of the pair is ever sent.
            let mode = if speaker.coord_mode == "polar" {
                CoordMode::Polar
            } else {
                CoordMode::Cartesian
            };
            widgets::label_row_help(
                ui,
                t("speaker.coordinates"),
                "help.speaker.position",
                |ui| {
                    let mut chosen = mode;
                    ui.selectable_value(&mut chosen, CoordMode::Polar, t("common.polarShort"));
                    ui.selectable_value(
                        &mut chosen,
                        CoordMode::Cartesian,
                        t("common.cartesianShort"),
                    );
                    if chosen != mode {
                        let value = match chosen {
                            CoordMode::Cartesian => "cartesian",
                            CoordMode::Polar => "polar",
                        };
                        self.edit_speaker(id, "coordMode", serde_json::json!(value));
                    }
                },
            );
            match mode {
                CoordMode::Cartesian => {
                    self.cartesian_table(ui, id, speaker, scale_m);
                }
                CoordMode::Polar => {
                    self.polar_table(ui, id, speaker, scale_m);
                }
            }
            self.gizmo_button(ui, mode, frozen);

            // Gain is realtime, like the master and the list rows.
            let gain = {
                let live = self.host.read();
                live.app
                    .speaker_gains
                    .get(&index.to_string())
                    .copied()
                    .unwrap_or(1.0)
            };
            let mut value = gain as f32;
            if widgets::value_slider_help(
                ui,
                t("speaker.gain"),
                "help.speaker.gain",
                &mut value,
                0.0..=2.0,
                0.01,
                |v| crate::panels::audio::format_linear_as_db(Some(v as f64)),
            ) {
                self.set_speaker_gain(index, value);
            }

            // Delay belongs to the speakers document, not the layout.
            let mut delay = speaker.delay_ms as f32;
            widgets::label_row_help(ui, t("speaker.delayMs"), "help.speaker.delayMs", |ui| {
                if ui
                    .add(
                        egui::DragValue::new(&mut delay)
                            .speed(0.1)
                            .range(0.0..=f32::MAX),
                    )
                    .changed()
                {
                    self.set_speaker_delay(id, delay as f64);
                }
            });
            // The same delay in samples, both readouts kept in step.
            let mut samples = (speaker.delay_ms.max(0.0) / 1000.0 * DELAY_SAMPLE_RATE_HZ).round();
            widgets::label_row_help(
                ui,
                t("speaker.delaySamples"),
                "help.speaker.delaySamples",
                |ui| {
                    if ui
                        .add(
                            egui::DragValue::new(&mut samples)
                                .speed(1.0)
                                .range(0.0..=f64::MAX)
                                .fixed_decimals(0),
                        )
                        .changed()
                    {
                        self.set_speaker_delay(id, samples.round() * 1000.0 / DELAY_SAMPLE_RATE_HZ);
                    }
                },
            );
            self.delay_tools_row(ui);

            let mut spatialize = speaker.spatialize != 0;
            if widgets::switch_row_help(
                ui,
                t("speaker.spatialize"),
                "help.speaker.spatialize",
                &mut spatialize,
            ) {
                self.edit_speaker(id, "spatialize", serde_json::json!(spatialize));
            }

            // Band limits: empty means full range, which is why they are
            // optional in the model and sent as zero to clear.
            self.frequency_row(
                ui,
                id,
                t("speaker.freqLow"),
                "help.speaker.freqLow",
                "freqLow",
                speaker.freq_low,
            );
            self.frequency_row(
                ui,
                id,
                "Freq. max (Hz)",
                "help.speaker.freqHigh",
                "freqHigh",
                speaker.freq_high,
            );
        });
    }

    /// The editor's "3D Edit" toggle: it arms the gizmo for the mode being
    /// edited, and only one mode is ever armed — two sets of handles on one
    /// speaker would be two answers to the same question.
    fn gizmo_button(&mut self, ui: &mut Ui, mode: CoordMode, frozen: bool) {
        let wanted = match mode {
            CoordMode::Cartesian => EditMode::Cartesian,
            CoordMode::Polar => EditMode::Polar,
        };
        let gizmo = self.settings.gizmo;
        let armed = gizmo.mode == wanted
            && match wanted {
                EditMode::Cartesian => gizmo.cartesian_armed,
                EditMode::Polar => gizmo.polar_armed,
            };
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        !frozen,
                        egui::Button::selectable(armed, t("speaker.edit3d")),
                    )
                    .clicked()
                {
                    let gizmo = &mut self.settings.gizmo;
                    gizmo.mode = wanted;
                    match wanted {
                        EditMode::Cartesian => {
                            gizmo.cartesian_armed = !armed;
                            gizmo.polar_armed = false;
                        }
                        EditMode::Polar => {
                            gizmo.polar_armed = !armed;
                            gizmo.cartesian_armed = false;
                        }
                    }
                }
            });
        });
    }

    fn cartesian_table(&mut self, ui: &mut Ui, id: i32, speaker: &Speaker, scale_m: f64) {
        let axes = [
            ("X", speaker.x, "x"),
            ("Y", speaker.y, "y"),
            ("Z", speaker.z, "z"),
        ];
        ui.horizontal(|ui| {
            let width = widgets::fitted_field_width(
                ui,
                &[t("speaker.normalizedCoords"), "X", "Y", "Z"],
                3,
                30.0,
                56.0,
            );
            ui.label(
                RichText::new(t("speaker.normalizedCoords"))
                    .size(theme::FONT_SIZE_SMALL)
                    .color(theme::TEXT_MUTED),
            );
            for (label, value, key) in axes {
                let mut v = value as f32;
                ui.label(
                    RichText::new(label)
                        .size(theme::FONT_SIZE_SMALL)
                        .color(theme::TEXT_DIM),
                );
                if ui
                    .add_sized(
                        egui::vec2(width, ui.spacing().interact_size.y),
                        egui::DragValue::new(&mut v).speed(0.001).range(-1.0..=1.0),
                    )
                    .changed()
                {
                    self.edit_speaker(id, key, serde_json::json!(v.clamp(-1.0, 1.0)));
                }
            }
        });
        ui.horizontal(|ui| {
            help::label(
                ui,
                RichText::new(t("speaker.metersCoords"))
                    .size(theme::FONT_SIZE_SMALL)
                    .color(theme::TEXT_MUTED),
                "help.speaker.positionMeters",
            );
            let width = widgets::fitted_field_width(ui, &["X", "Y", "Z"], 3, 30.0, 56.0);
            for (label, value, key) in axes {
                let mut metres = (value * scale_m) as f32;
                ui.label(
                    RichText::new(label)
                        .size(theme::FONT_SIZE_SMALL)
                        .color(theme::TEXT_DIM),
                );
                if ui
                    .add_sized(
                        egui::vec2(width, ui.spacing().interact_size.y),
                        egui::DragValue::new(&mut metres).speed(0.01),
                    )
                    .changed()
                {
                    let normalised = (metres as f64 / scale_m).clamp(-1.0, 1.0);
                    self.edit_speaker(id, key, serde_json::json!(normalised));
                }
            }
        });
        help::card(ui, "help.speaker.positionMeters");
    }

    fn polar_table(&mut self, ui: &mut Ui, id: i32, speaker: &Speaker, scale_m: f64) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(t("speaker.normalizedCoords"))
                    .size(theme::FONT_SIZE_SMALL)
                    .color(theme::TEXT_MUTED),
            );
            let width = widgets::fitted_field_width(ui, &["Az°", "El°", "Dist"], 3, 30.0, 56.0);
            let mut az = speaker.azimuth_deg as f32;
            let mut el = speaker.elevation_deg as f32;
            let mut distance = speaker.distance_m as f32;
            ui.label(
                RichText::new("Az°")
                    .size(theme::FONT_SIZE_SMALL)
                    .color(theme::TEXT_DIM),
            );
            if ui
                .add_sized(
                    egui::vec2(width, ui.spacing().interact_size.y),
                    egui::DragValue::new(&mut az).speed(0.1),
                )
                .changed()
            {
                self.edit_speaker(id, "azimuth", serde_json::json!(az));
            }
            ui.label(
                RichText::new("El°")
                    .size(theme::FONT_SIZE_SMALL)
                    .color(theme::TEXT_DIM),
            );
            if ui
                .add_sized(
                    egui::vec2(width, ui.spacing().interact_size.y),
                    egui::DragValue::new(&mut el).speed(0.1),
                )
                .changed()
            {
                self.edit_speaker(id, "elevation", serde_json::json!(el));
            }
            ui.label(
                RichText::new("Dist")
                    .size(theme::FONT_SIZE_SMALL)
                    .color(theme::TEXT_DIM),
            );
            if ui
                .add_sized(
                    egui::vec2(width, ui.spacing().interact_size.y),
                    egui::DragValue::new(&mut distance)
                        .speed(0.001)
                        .range(0.01..=f32::MAX),
                )
                .changed()
            {
                self.edit_speaker(id, "distance", serde_json::json!(distance.max(0.01)));
            }
        });
        ui.horizontal(|ui| {
            help::label(
                ui,
                RichText::new(t("speaker.metersCoords"))
                    .size(theme::FONT_SIZE_SMALL)
                    .color(theme::TEXT_MUTED),
                "help.speaker.positionMeters",
            );
            let mut metres = (speaker.distance_m * scale_m) as f32;
            if ui
                .add_sized(
                    egui::vec2(56.0, ui.spacing().interact_size.y),
                    egui::DragValue::new(&mut metres)
                        .speed(0.01)
                        .range(0.01..=f32::MAX),
                )
                .changed()
            {
                let normalised = (metres as f64 / scale_m).max(0.01);
                self.edit_speaker(id, "distance", serde_json::json!(normalised));
            }
        });
        help::card(ui, "help.speaker.positionMeters");
    }

    /// A band limit: empty means full range, so zero clears it.
    fn frequency_row(
        &mut self,
        ui: &mut Ui,
        id: i32,
        label: &str,
        help: &str,
        key: &'static str,
        current: Option<f32>,
    ) {
        let mut value = current.unwrap_or(0.0);
        widgets::label_row_help(ui, label, help, |ui| {
            let response = ui.add(
                egui::DragValue::new(&mut value)
                    .speed(10.0)
                    .range(0.0..=f32::MAX)
                    .suffix(" Hz"),
            );
            if response.changed() {
                // Zero is "no limit", not a 0 Hz corner.
                let sent = if value > 0.0 {
                    serde_json::json!(value)
                } else {
                    serde_json::Value::Null
                };
                self.edit_speaker(id, key, sent);
            }
            if current.is_none() {
                response.on_hover_text("full range");
            }
        });
    }

    fn speaker_test_tab(&mut self, ui: &mut Ui, index: usize) {
        let running = self.host.read().speaker_test.running == Some(index);
        widgets::label_row_help(ui, t("speaker.test"), "help.speaker.test", |ui| {
            let label = if running {
                t("speaker.testStop")
            } else {
                t("speaker.testPlay")
            };
            let button = ui.add(egui::Button::new(label).fill(if running {
                theme::FILL_ACTIVE
            } else {
                theme::FILL
            }));
            if self.speaker_test_mode == "hold" {
                // Hold: the test lasts exactly as long as the press.
                if button.is_pointer_button_down_on() && !running {
                    self.start_speaker_test(index);
                } else if running && !button.is_pointer_button_down_on() {
                    self.stop_speaker_test();
                }
            } else if button.clicked() {
                if running {
                    self.stop_speaker_test();
                } else {
                    self.start_speaker_test(index);
                }
            }
        });
        let mode = self.speaker_test_mode.clone();
        if let Some(chosen) = select_row(
            ui,
            t("speaker.testMode"),
            "help.speaker.testMode",
            "speaker-test-mode",
            &mode,
            TEST_MODES,
        ) {
            self.speaker_test_mode = chosen;
        }
        let isolation = self.speaker_test_isolation.clone();
        if let Some(chosen) = select_row(
            ui,
            t("speaker.testIsolation"),
            "help.speaker.testIsolation",
            "speaker-test-isolation",
            &isolation,
            TEST_ISOLATIONS,
        ) {
            self.speaker_test_isolation = chosen;
        }
        let mut level = self.speaker_test_level_db;
        if widgets::value_slider_help(
            ui,
            t("speaker.testLevel"),
            "help.speaker.testLevel",
            &mut level,
            -60.0..=0.0,
            1.0,
            |v| format!("{v:.0} dBFS"),
        ) {
            self.speaker_test_level_db = level;
        }
    }

    /// Start the test on a speaker. The core arms the safety window and stops
    /// it on its own clock, whatever is on screen.
    fn start_speaker_test(&mut self, index: usize) {
        speaker_test::start(
            &self.host,
            index,
            self.speaker_test_level_db,
            self.speaker_test_isolation.clone(),
            &self.speaker_test_mode,
        );
    }

    pub(crate) fn stop_speaker_test(&mut self) {
        speaker_test::stop(&self.host);
    }

    /// A test running on another speaker follows a new selection in toggle
    /// mode, and stops in the others (`onSpeakerSelectionChanged`).
    pub(crate) fn follow_speaker_selection(&mut self) {
        let Some(running) = self.host.read().speaker_test.running else {
            return;
        };
        match self.selection.speaker {
            Some(index) if index == running => {}
            Some(index) if self.speaker_test_mode == "toggle" => {
                self.start_speaker_test(index);
            }
            _ => self.stop_speaker_test(),
        }
    }

    /// The renderer's input-to-output chain is kept warm while the Test pane
    /// is open on a speaker, so a test is audible with nothing playing. The
    /// arm expires renderer-side, hence the re-arm.
    pub(crate) fn declare_idle_feed_interest(&mut self) {
        // Two things ask for it (`test-idle-feed.js`), and both are keyed on
        // the editor being *open* rather than merely configured, so a restored
        // preference cannot make the host talk to a live renderer at startup.
        // The core holds the arm and renews it.
        use crate::host::services::interests::{FeedClient, set_idle_feed_wanted};
        set_idle_feed_wanted(
            &self.host,
            FeedClient::SpeakerTest,
            self.speaker_tab == SpeakerTab::Test && self.selection.speaker.is_some(),
        );
        set_idle_feed_wanted(
            &self.host,
            FeedClient::ObjectTest,
            self.object_test_editor_open(),
        );
    }

    /// "Delay tools": the two bulk tools, each asking before it runs.
    fn delay_tools_row(&mut self, ui: &mut Ui) {
        let tools = [DelayTool::CalcDelays, DelayTool::DelayToDistance];
        let clicked = widgets::label_buttons_help(
            ui,
            t("speaker.delayTools"),
            "help.speaker.delayTools",
            &tools.map(|tool| (t(tool.label_key()), true)),
        );
        if let Some(index) = clicked {
            self.delay_tool_confirm = Some(tools[index]);
        }
    }

    /// Delay belongs to the speakers document, not the layout.
    fn set_speaker_delay(&mut self, id: i32, delay_ms: f64) {
        speakers::control_speaker_delay(&self.host, id, delay_ms as f32);
    }

    /// The web asks with `window.confirm`; here a modal with the same text.
    pub(crate) fn delay_tool_modal(&mut self, ctx: &egui::Context) {
        let Some(tool) = self.delay_tool_confirm else {
            return;
        };
        let mut run = false;
        let mut cancel = false;
        let modal = egui::Modal::new(egui::Id::new("delay-tool-confirm"))
            .frame(widgets::modal_frame())
            .show(ctx, |ui| {
                ui.set_max_width(340.0);
                for line in t(tool.confirm_key()).split('\n') {
                    if line.is_empty() {
                        ui.add_space(theme::ROW_GAP);
                    } else {
                        ui.label(line);
                    }
                }
                ui.add_space(theme::PANEL_GAP);
                ui.horizontal(|ui| {
                    cancel = ui.button(t("common.cancel")).clicked();
                    run = ui
                        .button(RichText::new(t(tool.label_key())).color(theme::WARN))
                        .clicked();
                });
            });
        if run {
            self.run_delay_tool(tool);
        }
        if run || cancel || modal.should_close() {
            self.delay_tool_confirm = None;
        }
    }

    fn run_delay_tool(&mut self, tool: DelayTool) {
        let (speakers, frozen, scale_m) = {
            let live = self.host.read();
            (
                live.selected_speakers().to_vec(),
                live.app.render_backend_state.frozen_speakers,
                live.app.room_ratio.scale_m.max(0.01),
            )
        };
        if frozen || speakers.is_empty() {
            return;
        }
        let distances_m: Vec<f64> = speakers
            .iter()
            .map(|s| s.distance_m.max(0.0) * scale_m)
            .collect();
        match tool {
            DelayTool::CalcDelays => {
                let edits: Vec<serde_json::Value> = aligned_delays_ms(&distances_m)
                    .into_iter()
                    .enumerate()
                    .map(|(id, delay_ms)| serde_json::json!({ "id": id, "delayMs": delay_ms }))
                    .collect();
                speakers::control_speakers_config(
                    &self.host,
                    serde_json::json!({ "speakerEdits": edits }),
                );
            }
            DelayTool::DelayToDistance => {
                let delays: Vec<f64> = speakers.iter().map(|s| s.delay_ms).collect();
                let targets = distances_from_delays(&distances_m, &delays, scale_m);
                // Along each speaker's own direction: its position scaled to
                // the new distance, one layout edit for the lot.
                let edits: Vec<serde_json::Value> = speakers
                    .iter()
                    .zip(targets)
                    .enumerate()
                    .map(|(id, (s, target))| {
                        let norm = (s.x * s.x + s.y * s.y + s.z * s.z).sqrt();
                        let dir = if norm > 1e-6 {
                            [s.x / norm, s.y / norm, s.z / norm]
                        } else {
                            [1.0, 0.0, 0.0]
                        };
                        serde_json::json!({
                            "id": id,
                            "coordMode": "cartesian",
                            "x": dir[0] * target,
                            "y": dir[1] * target,
                            "z": dir[2] * target,
                        })
                    })
                    .collect();
                speakers::apply_layout_document(
                    &self.host,
                    serde_json::json!({ "speakerEdits": edits }),
                );
            }
        }
    }

    /// One field of one speaker in the layout document, then apply.
    /// The three coordinates in one edit, which is what a drag produces: three
    /// separate edits would be three layout applies for one move.
    pub(crate) fn edit_speaker_position(&mut self, id: i32, adm: [f64; 3]) {
        speakers::apply_layout_document(
            &self.host,
            serde_json::json!({
                "speakerEdits": [{
                    "id": id.max(0),
                    "coordMode": "cartesian",
                    "x": adm[0],
                    "y": adm[1],
                    "z": adm[2],
                }]
            }),
        );
    }

    fn edit_speaker(&mut self, id: i32, key: &str, value: serde_json::Value) {
        speakers::apply_layout_document(
            &self.host,
            serde_json::json!({ "speakerEdits": [{ "id": id.max(0), key: value }] }),
        );
    }

    pub(crate) fn move_speaker(&mut self, from: usize, to: usize) {
        speakers::control_speakers_move(&self.host, from as i32, to as i32);
        self.selection.speaker = Some(to);
    }

    /// `control_speaker_gain`: realtime, stamped so the renderer can drop a
    /// stale update.
    pub(crate) fn set_speaker_gain(&mut self, index: usize, gain: f32) {
        gain::control_speaker_gain(&self.host, index as i32, gain);
    }
}

/// Label on the left, a select on the right. Returns the new value.
fn select_row(
    ui: &mut Ui,
    label: &str,
    help: &str,
    id: &str,
    current: &str,
    options: &[(&str, &str)],
) -> Option<String> {
    let mut chosen = current.to_owned();
    widgets::label_row_help(ui, label, help, |ui| {
        widgets::bounded_combo(ui, 160.0, |ui, w| {
            egui::ComboBox::from_id_salt(id)
                .selected_text(t(options
                    .iter()
                    .find(|(v, _)| *v == current)
                    .map(|(_, key)| *key)
                    .unwrap_or(options[0].1)))
                .width(w)
                .truncate()
                .show_ui(ui, |ui| {
                    for (value, key) in options {
                        ui.selectable_value(&mut chosen, (*value).to_owned(), t(key));
                    }
                })
        });
    });
    (chosen != current).then_some(chosen)
}

#[cfg(test)]
mod tests {
    use super::{aligned_delays_ms, distances_from_delays};

    #[test]
    fn nearer_speakers_wait_for_the_farthest() {
        // 3.43 m of path is 10 ms of sound.
        let delays = aligned_delays_ms(&[3.43, 0.0, 1.715]);
        assert_eq!(delays, vec![0.0, 10.0, 5.0]);
    }

    #[test]
    fn delays_move_speakers_back_to_the_distance_they_stand_for() {
        let distances_m = [3.43, 3.43];
        // Scale 2 m per unit: the reference 3.43 m is 1.715 units.
        let targets = distances_from_delays(&distances_m, &[0.0, 5.0], 2.0);
        assert!((targets[0] - 1.715).abs() < 1e-9);
        assert!((targets[1] - 0.8575).abs() < 1e-9);
        // Round trip: the delays computed from the new distances are the
        // delays that were asked for.
        let back = aligned_delays_ms(&targets.iter().map(|t| t * 2.0).collect::<Vec<_>>());
        assert_eq!(back, vec![0.0, 5.0]);
        // A delay longer than the room is clamped, not negative.
        assert_eq!(distances_from_delays(&[1.0], &[1000.0], 1.0), vec![0.01]);
    }
}
