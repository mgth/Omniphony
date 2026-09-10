//! The speaker editor (`#speakerEditSection`, `speakers.js`,
//! `listeners/speaker-editor-listeners.js`, `controls/speaker-test.js`): the
//! Edit and Test tabs of the selected speaker.
//!
//! Every edit goes into the layout document as a `speakerEdits` entry and is
//! then applied, exactly like the Tauri commands; the delay is the one field
//! that belongs to the speakers document instead.

use egui::{RichText, Ui};

use crate::app::StudioSpike;
use crate::i18n::t;
use crate::model::layouts::Speaker;
use crate::ui::{theme, widgets};
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
const BURST: std::time::Duration = std::time::Duration::from_secs(2);
const TOGGLE_SAFETY: std::time::Duration = std::time::Duration::from_secs(60);
/// The renderer expires the idle-feed arm after a keepalive window.
const IDLE_FEED_REARM: std::time::Duration = std::time::Duration::from_secs(120);

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

impl StudioSpike {
    /// Shown only while a speaker is selected, like the web editor.
    pub(crate) fn speaker_editor(&mut self, ui: &mut Ui) {
        let Some(index) = self.selection.speaker else {
            return;
        };
        let (speaker, count, frozen, scale_m) = {
            let live = self.live.lock().unwrap();
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
        ui.horizontal(|ui| {
            ui.label(t("speaker.layout"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_enabled_ui(!frozen, |ui| {
                    if ui.button(t("speaker.delete")).clicked() {
                        self.ctl.send_json(
                            "/omniphony/control/config/layout",
                            &serde_json::json!({ "removeSpeaker": index }),
                        );
                        self.apply_layout();
                        self.selection.speaker = None;
                    }
                    if ui
                        .add_enabled(index + 1 < count, egui::Button::new(t("speaker.down")))
                        .clicked()
                    {
                        self.move_speaker(index, index + 1);
                    }
                    if ui
                        .add_enabled(index > 0, egui::Button::new(t("speaker.up")))
                        .clicked()
                    {
                        self.move_speaker(index, index - 1);
                    }
                });
            });
        });
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
        ui.add_enabled_ui(!frozen, |ui| {
            // Name.
            let mut name = speaker.id.clone();
            ui.horizontal(|ui| {
                ui.label(t("common.name"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(egui::TextEdit::singleline(&mut name).desired_width(150.0))
                        .lost_focus()
                        && name.trim() != speaker.id
                        && !name.trim().is_empty()
                    {
                        self.edit_speaker(id, "name", serde_json::json!(name.trim()));
                    }
                });
            });

            // Coordinates: the two tables of the web editor, normalised on one
            // row and metres on the next. Metres are the normalised value
            // times the room scale, so only one of the pair is ever sent.
            let mode = if speaker.coord_mode == "polar" {
                CoordMode::Polar
            } else {
                CoordMode::Cartesian
            };
            ui.horizontal(|ui| {
                ui.label(t("speaker.coordinates"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
                });
            });
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
                let live = self.live.lock().unwrap();
                live.app
                    .speaker_gains
                    .get(&index.to_string())
                    .copied()
                    .unwrap_or(1.0)
            };
            let mut value = gain as f32;
            if widgets::value_slider(ui, t("speaker.gain"), &mut value, 0.0..=2.0, 0.01, |v| {
                crate::panels::audio::format_linear_as_db(Some(v as f64))
            }) {
                self.set_speaker_gain(index, value);
            }

            // Delay belongs to the speakers document, not the layout.
            let mut delay = speaker.delay_ms as f32;
            ui.horizontal(|ui| {
                ui.label(t("speaker.delayMs"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(
                            egui::DragValue::new(&mut delay)
                                .speed(0.1)
                                .range(0.0..=f32::MAX),
                        )
                        .changed()
                    {
                        self.ctl.send_json(
                            "/omniphony/control/config/speakers",
                            &serde_json::json!({
                                "speakerEdits": [{ "id": id.max(0), "delayMs": delay.max(0.0) }]
                            }),
                        );
                    }
                });
            });

            let mut spatialize = speaker.spatialize != 0;
            if widgets::switch_row(ui, t("speaker.spatialize"), &mut spatialize) {
                self.edit_speaker(id, "spatialize", serde_json::json!(spatialize));
            }

            // Band limits: empty means full range, which is why they are
            // optional in the model and sent as zero to clear.
            self.frequency_row(ui, id, t("speaker.freqLow"), "freqLow", speaker.freq_low);
            self.frequency_row(ui, id, "Freq. max (Hz)", "freqHigh", speaker.freq_high);
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
                        egui::vec2(56.0, ui.spacing().interact_size.y),
                        egui::DragValue::new(&mut v).speed(0.001).range(-1.0..=1.0),
                    )
                    .changed()
                {
                    self.edit_speaker(id, key, serde_json::json!(v.clamp(-1.0, 1.0)));
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(t("speaker.metersCoords"))
                    .size(theme::FONT_SIZE_SMALL)
                    .color(theme::TEXT_MUTED),
            );
            for (label, value, key) in axes {
                let mut metres = (value * scale_m) as f32;
                ui.label(
                    RichText::new(label)
                        .size(theme::FONT_SIZE_SMALL)
                        .color(theme::TEXT_DIM),
                );
                if ui
                    .add_sized(
                        egui::vec2(56.0, ui.spacing().interact_size.y),
                        egui::DragValue::new(&mut metres).speed(0.01),
                    )
                    .changed()
                {
                    let normalised = (metres as f64 / scale_m).clamp(-1.0, 1.0);
                    self.edit_speaker(id, key, serde_json::json!(normalised));
                }
            }
        });
    }

    fn polar_table(&mut self, ui: &mut Ui, id: i32, speaker: &Speaker, scale_m: f64) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(t("speaker.normalizedCoords"))
                    .size(theme::FONT_SIZE_SMALL)
                    .color(theme::TEXT_MUTED),
            );
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
                    egui::vec2(56.0, ui.spacing().interact_size.y),
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
                    egui::vec2(56.0, ui.spacing().interact_size.y),
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
                    egui::vec2(56.0, ui.spacing().interact_size.y),
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
            ui.label(
                RichText::new(t("speaker.metersCoords"))
                    .size(theme::FONT_SIZE_SMALL)
                    .color(theme::TEXT_MUTED),
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
    }

    /// A band limit: empty means full range, so zero clears it.
    fn frequency_row(
        &mut self,
        ui: &mut Ui,
        id: i32,
        label: &str,
        key: &'static str,
        current: Option<f32>,
    ) {
        let mut value = current.unwrap_or(0.0);
        ui.horizontal(|ui| {
            ui.label(label);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
        });
    }

    fn speaker_test_tab(&mut self, ui: &mut Ui, index: usize) {
        let running = self.speaker_test_running == Some(index);
        ui.horizontal(|ui| {
            ui.label(t("speaker.test"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
        });
        let mode = self.speaker_test_mode.clone();
        if let Some(chosen) = select_row(
            ui,
            t("speaker.testMode"),
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
            "speaker-test-isolation",
            &isolation,
            TEST_ISOLATIONS,
        ) {
            self.speaker_test_isolation = chosen;
        }
        let mut level = self.speaker_test_level_db;
        if widgets::value_slider(
            ui,
            t("speaker.testLevel"),
            &mut level,
            -60.0..=0.0,
            1.0,
            |v| format!("{v:.0} dBFS"),
        ) {
            self.speaker_test_level_db = level;
        }
        // A running test whose safety window elapsed stops itself.
        if let Some(deadline) = self.speaker_test_deadline
            && std::time::Instant::now() >= deadline
        {
            self.stop_speaker_test();
        }
    }

    /// The start message. Sent again whenever something it carries changes.
    fn send_speaker_test(&mut self, index: usize) {
        // Peak dBFS to the peak linear amplitude the renderer clamps to.
        let level = 10f32.powf(self.speaker_test_level_db / 20.0);
        self.ctl.send(
            "/omniphony/control/speaker_test",
            vec![
                rosc::OscType::Int(index as i32),
                rosc::OscType::Float(level.clamp(0.0, 1.0)),
                rosc::OscType::String(self.speaker_test_isolation.clone()),
            ],
        );
    }

    fn start_speaker_test(&mut self, index: usize) {
        self.send_speaker_test(index);
        self.speaker_test_running = Some(index);
        self.speaker_test_deadline = match self.speaker_test_mode.as_str() {
            "burst" => Some(std::time::Instant::now() + BURST),
            "toggle" => Some(std::time::Instant::now() + TOGGLE_SAFETY),
            _ => None,
        };
    }

    /// Sent unconditionally: "nothing is running" is this side's belief, and
    /// the renderer's state is the one that matters.
    pub(crate) fn stop_speaker_test(&mut self) {
        if self.speaker_test_running.is_none() {
            return;
        }
        self.speaker_test_running = None;
        self.speaker_test_deadline = None;
        self.ctl.send(
            "/omniphony/control/speaker_test",
            vec![
                rosc::OscType::Int(-1),
                rosc::OscType::Float(0.0),
                rosc::OscType::String(self.speaker_test_isolation.clone()),
            ],
        );
    }

    /// A test running on another speaker follows a new selection in toggle
    /// mode, and stops in the others (`onSpeakerSelectionChanged`).
    pub(crate) fn follow_speaker_selection(&mut self) {
        let Some(running) = self.speaker_test_running else {
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
    pub(crate) fn maintain_test_idle_feed(&mut self) {
        // Ref-counted across the two things that need it (`test-idle-feed.js`):
        // the speaker Test pane and the object injection. Both are keyed on the
        // editor being *open* rather than merely configured, so a restored
        // preference cannot make the host talk to a live renderer at startup.
        let wanted = (self.speaker_tab == SpeakerTab::Test && self.selection.speaker.is_some())
            || self.object_test_editor_open();
        let now = std::time::Instant::now();
        match (wanted, self.idle_feed_armed_at) {
            (true, None) => {
                self.ctl
                    .send_int("/omniphony/control/speaker_test/idle_feed", 1);
                self.idle_feed_armed_at = Some(now);
            }
            (true, Some(at)) if now.duration_since(at) >= IDLE_FEED_REARM => {
                self.ctl
                    .send_int("/omniphony/control/speaker_test/idle_feed", 1);
                self.idle_feed_armed_at = Some(now);
            }
            (false, Some(_)) => {
                self.ctl
                    .send_int("/omniphony/control/speaker_test/idle_feed", 0);
                self.idle_feed_armed_at = None;
            }
            _ => {}
        }
    }

    /// One field of one speaker in the layout document, then apply.
    fn edit_speaker(&mut self, id: i32, key: &str, value: serde_json::Value) {
        self.ctl.send_json(
            "/omniphony/control/config/layout",
            &serde_json::json!({
                "speakerEdits": [{ "id": id.max(0), key: value }]
            }),
        );
        self.apply_layout();
    }

    pub(crate) fn move_speaker(&mut self, from: usize, to: usize) {
        self.ctl.send_json(
            "/omniphony/control/config/layout",
            &serde_json::json!({ "moveSpeaker": { "from": from, "to": to } }),
        );
        self.apply_layout();
        self.selection.speaker = Some(to);
    }

    fn apply_layout(&mut self) {
        self.mark_recompute_pending();
        self.ctl
            .send_no_args("/omniphony/control/config/layout/apply");
    }

    /// `control_speaker_gain`: realtime, stamped so the renderer can drop a
    /// stale update.
    pub(crate) fn set_speaker_gain(&mut self, index: usize, gain: f32) {
        let clamped = gain.clamp(0.0, 2.0);
        self.live
            .lock()
            .unwrap()
            .app
            .speaker_gains
            .insert(index.to_string(), clamped as f64);
        let seq = self.next_realtime_seq();
        self.ctl.send(
            "/omniphony/control/realtime/speaker_gain",
            vec![
                rosc::OscType::Int(index as i32),
                rosc::OscType::Float(clamped),
                rosc::OscType::Int(seq),
            ],
        );
    }
}

/// Label on the left, a select on the right. Returns the new value.
fn select_row(
    ui: &mut Ui,
    label: &str,
    id: &str,
    current: &str,
    options: &[(&str, &str)],
) -> Option<String> {
    let mut chosen = current.to_owned();
    ui.horizontal(|ui| {
        ui.label(label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            egui::ComboBox::from_id_salt(id)
                .selected_text(t(options
                    .iter()
                    .find(|(v, _)| *v == current)
                    .map(|(_, key)| *key)
                    .unwrap_or(options[0].1)))
                .width(160.0)
                .show_ui(ui, |ui| {
                    for (value, key) in options {
                        ui.selectable_value(&mut chosen, (*value).to_owned(), t(key));
                    }
                });
        });
    });
    (chosen != current).then_some(chosen)
}
