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
use crate::ui::group::Group;
use crate::ui::widgets::{CoordCell, CoordRow};
use crate::ui::{section, theme, widgets};
use crate::view::gizmos::EditMode;

use super::row_glyphs::Filter;

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

/// The coordinates group's title: the web's label without the colon it
/// carries as a row label (`Coordinates:`, `Coordonnées :`, `座標：`).
pub(crate) fn coordinates_title() -> &'static str {
    t("speaker.coordinates").trim_end_matches([':', '：', ' ', '\u{a0}'])
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
        // A section's header, with the row this editor is on at its right
        // end, so Up and Down have something to be read against.
        let position = format!("{} / {}", index + 1, count);
        section::pinned_header(ui, t("section.speakerEditor"), Some(&position));
        self.speaker_tabs(ui);
        if self.layout_row(ui, index, count, frozen) {
            self.speaker_name_edit.discard();
            return; // the captured index no longer identifies this speaker
        }
        match self.speaker_tab {
            SpeakerTab::Edit => self.speaker_edit_tab(ui, index, &speaker, scale_m, frozen),
            SpeakerTab::Test => self.speaker_test_tab(ui, index),
        }
    }

    fn speaker_tabs(&mut self, ui: &mut Ui) {
        if let Some(tab) = widgets::tab_bar(
            ui,
            &self.speaker_tab,
            &[
                (SpeakerTab::Edit, t("speakerTabs.edit")),
                (SpeakerTab::Test, t("speakerTabs.test")),
            ],
        ) {
            self.speaker_tab = tab;
            if tab == SpeakerTab::Edit {
                // Leaving the test pane stops whatever it started.
                self.stop_speaker_test();
            }
        }
    }

    /// Reorder and delete. Both rewrite the layout, so both are refused while
    /// the backend has the speakers frozen.
    fn layout_row(&mut self, ui: &mut Ui, index: usize, count: usize, frozen: bool) -> bool {
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
        clicked.is_some()
    }

    /// The Edit tab: what the speaker is — its name, whether it takes part in
    /// panning — in rows, then a group for where it stands and one for what
    /// it puts out.
    fn speaker_edit_tab(
        &mut self,
        ui: &mut Ui,
        index: usize,
        speaker: &Speaker,
        scale_m: f64,
        frozen: bool,
    ) {
        let id = index as i32;
        let layout_key = self.host.read().app.selected_layout_key.clone();
        // While the gizmo holds this speaker, the readouts follow the pointer
        // rather than the state, as the web's editor did during a drag.
        let held = self.speaker_at_edit_pin(index, speaker);
        let speaker = held.as_ref().unwrap_or(speaker);
        ui.add_enabled_ui(!frozen, |ui| {
            let name = widgets::label_row_help(ui, t("common.name"), "help.speaker.name", |ui| {
                self.speaker_name_edit.show(
                    ui,
                    ("speaker-name", &layout_key, index),
                    &speaker.id,
                    "",
                    150.0,
                    false,
                )
            });
            if let Some(name) = name {
                self.edit_speaker(id, "name", serde_json::json!(name));
            }
            let mut spatialize = speaker.spatialize != 0;
            if widgets::switch_row_help(
                ui,
                t("speaker.spatialize"),
                "help.speaker.spatialize",
                &mut spatialize,
            ) {
                self.edit_speaker(id, "spatialize", serde_json::json!(spatialize));
            }

            // Coordinates: the mode in the bar; in the inset the table — the
            // web editor's two rows, normalised and metres, metres being the
            // normalised value times the room scale, so only one of the pair
            // is ever sent — and the 3D edit toggle.
            let mode = if speaker.coord_mode == "polar" {
                CoordMode::Polar
            } else {
                CoordMode::Cartesian
            };
            let mut chosen = mode;
            Group::new(coordinates_title())
                .help("help.speaker.position")
                .actions(|ui| {
                    if let Some(picked) = widgets::toggle_buttons(
                        ui,
                        &mode,
                        &[
                            (CoordMode::Cartesian, t("common.cartesianShort")),
                            (CoordMode::Polar, t("common.polarShort")),
                        ],
                    ) {
                        chosen = picked;
                    }
                })
                .show(ui, |ui| {
                    let edit_mode = match mode {
                        CoordMode::Cartesian => EditMode::Cartesian,
                        CoordMode::Polar => EditMode::Polar,
                    };
                    self.coord_table_with_gizmo(ui, edit_mode, frozen, |this, ui| match mode {
                        CoordMode::Cartesian => this.cartesian_table(ui, id, speaker, scale_m),
                        CoordMode::Polar => this.polar_table(ui, id, speaker, scale_m),
                    });
                });
            if chosen != mode {
                let value = match chosen {
                    CoordMode::Cartesian => "cartesian",
                    CoordMode::Polar => "polar",
                };
                self.edit_speaker(id, "coordMode", serde_json::json!(value));
            }

            // Output: the gain — realtime, like the master and the list rows —
            // then the delay, which belongs to the speakers document rather
            // than the layout, in milliseconds and in samples with the two
            // kept in step, and the bulk tools under them.
            Group::new(t("speaker.output")).show(ui, |ui| {
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
                let mut delay = speaker.delay_ms as f32;
                widgets::label_row_help(ui, t("speaker.delayMs"), "help.speaker.delayMs", |ui| {
                    let field = egui::DragValue::new(&mut delay)
                        .speed(0.1)
                        .range(0.0..=f32::MAX);
                    if widgets::number_field(ui, widgets::FIELD_WIDTH, field).changed() {
                        self.set_speaker_delay(id, delay as f64);
                    }
                });
                let mut samples =
                    (speaker.delay_ms.max(0.0) / 1000.0 * DELAY_SAMPLE_RATE_HZ).round();
                widgets::label_row_help(
                    ui,
                    t("speaker.delaySamples"),
                    "help.speaker.delaySamples",
                    |ui| {
                        let field = egui::DragValue::new(&mut samples)
                            .speed(1.0)
                            .range(0.0..=f64::MAX)
                            .fixed_decimals(0);
                        if widgets::number_field(ui, widgets::FIELD_WIDTH, field).changed() {
                            self.set_speaker_delay(
                                id,
                                samples.round() * 1000.0 / DELAY_SAMPLE_RATE_HZ,
                            );
                        }
                    },
                );
                self.delay_tools_row(ui);
            });

            // Band: the two limits the crossover gives this speaker. Empty
            // limits mean full range, which is why they are optional in the
            // model and sent as zero to clear. The bar names the shape the two
            // make — full band included, so the line always reads — and draws
            // it as the glyph the speaker list carries, without the list's
            // cutoff labels: the numbers are in the two fields right below,
            // and stacked on the glyph they pushed it out of the bar and over
            // the first field's label.
            let (freq_low, freq_high) = (speaker.freq_low, speaker.freq_high);
            let filter = Filter::of(freq_low, freq_high);
            let band = Group::new(t("speaker.band"))
                .status(t(filter.title()), theme::TEXT_MUTED)
                .actions(move |ui| {
                    super::row_glyphs::filter_glyph(ui, filter);
                });
            band.show(ui, |ui| {
                self.frequency_row(
                    ui,
                    id,
                    t("speaker.freqLow"),
                    "help.speaker.freqLow",
                    "freqLow",
                    freq_low,
                );
                self.frequency_row(
                    ui,
                    id,
                    t("speaker.freqHigh"),
                    "help.speaker.freqHigh",
                    "freqHigh",
                    freq_high,
                );
            });
        });
    }

    /// The coordinate table with the "3D Edit" toggle at its right, as the
    /// web's `.coord-table-row` lays them out — the toggle centred on the
    /// table's height, a handle to the whole of it; under the table,
    /// right-aligned, on a panel too narrow for both. Shared with the channel
    /// editor.
    pub(crate) fn coord_table_with_gizmo(
        &mut self,
        ui: &mut Ui,
        wanted: EditMode,
        frozen: bool,
        table: impl FnOnce(&mut Self, &mut Ui),
    ) {
        let spacing = ui.spacing().item_spacing.x;
        let button_width = egui::WidgetText::from(t("speaker.edit3d"))
            .into_galley(
                ui,
                Some(egui::TextWrapMode::Extend),
                f32::INFINITY,
                egui::TextStyle::Button,
            )
            .size()
            .x
            + 2.0 * ui.spacing().button_padding.x;
        let table_min = widgets::coord_table_min_width(
            ui,
            &[t("speaker.normalizedCoords"), t("speaker.metersCoords")],
        );
        if ui.available_width() - button_width - spacing >= table_min {
            ui.horizontal(|ui| {
                let table_width = ui.available_width() - button_width - spacing;
                ui.vertical(|ui| {
                    ui.set_max_width(table_width);
                    table(self, ui);
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    self.gizmo_toggle(ui, wanted, frozen);
                });
            });
        } else {
            table(self, ui);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    self.gizmo_toggle(ui, wanted, frozen);
                });
            });
        }
    }

    /// The "3D Edit" toggle of the speaker and channel editors: it arms the
    /// gizmo for the mode being edited, and only one mode is ever armed — two
    /// sets of handles on one target would be two answers to the same
    /// question.
    fn gizmo_toggle(&mut self, ui: &mut Ui, wanted: EditMode, frozen: bool) {
        let gizmo = self.settings.gizmo;
        let armed = gizmo.mode == wanted
            && match wanted {
                EditMode::Cartesian => gizmo.cartesian_armed,
                EditMode::Polar => gizmo.polar_armed,
            };
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
    }

    fn cartesian_table(&mut self, ui: &mut Ui, id: i32, speaker: &Speaker, scale_m: f64) {
        const KEYS: [&str; 3] = ["x", "y", "z"];
        let adm = [speaker.x, speaker.y, speaker.z];
        let mut rows = [
            CoordRow {
                label: t("speaker.normalizedCoords"),
                help: None,
                cells: adm.map(|v| CoordCell::field(v as f32, 0.001, 3).in_range(-1.0..=1.0)),
            },
            CoordRow {
                label: t("speaker.metersCoords"),
                help: Some("help.speaker.positionMeters".into()),
                cells: adm.map(|v| CoordCell::field((v * scale_m) as f32, 0.01, 2)),
            },
        ];
        match widgets::coord_table(ui, ("speaker-cartesian", id), ["X", "Y", "Z"], &mut rows) {
            Some((0, axis, value)) => {
                self.edit_speaker(id, KEYS[axis], serde_json::json!(value.clamp(-1.0, 1.0)));
            }
            Some((_, axis, metres)) => {
                let normalised = (f64::from(metres) / scale_m).clamp(-1.0, 1.0);
                self.edit_speaker(id, KEYS[axis], serde_json::json!(normalised));
            }
            None => {}
        }
    }

    fn polar_table(&mut self, ui: &mut Ui, id: i32, speaker: &Speaker, scale_m: f64) {
        let mut rows = [
            CoordRow {
                label: t("speaker.normalizedCoords"),
                help: None,
                cells: [
                    CoordCell::field(speaker.azimuth_deg as f32, 0.1, 1),
                    CoordCell::field(speaker.elevation_deg as f32, 0.1, 1),
                    CoordCell::field(speaker.distance_m as f32, 0.001, 3).in_range(0.01..=f32::MAX),
                ],
            },
            // Only the distance has a length: the angles are what they are.
            CoordRow {
                label: t("speaker.metersCoords"),
                help: Some("help.speaker.positionMeters".into()),
                cells: [
                    CoordCell::Empty,
                    CoordCell::Empty,
                    CoordCell::field((speaker.distance_m * scale_m) as f32, 0.01, 2)
                        .in_range(0.01..=f32::MAX),
                ],
            },
        ];
        match widgets::coord_table(ui, ("speaker-polar", id), ["Az°", "El°", "Dist"], &mut rows) {
            Some((0, 0, azimuth)) => self.edit_speaker(id, "azimuth", serde_json::json!(azimuth)),
            Some((0, 1, elevation)) => {
                self.edit_speaker(id, "elevation", serde_json::json!(elevation));
            }
            Some((0, _, distance)) => {
                self.edit_speaker(id, "distance", serde_json::json!(distance.max(0.01)));
            }
            Some((_, _, metres)) => {
                let normalised = (f64::from(metres) / scale_m).max(0.01);
                self.edit_speaker(id, "distance", serde_json::json!(normalised));
            }
            None => {}
        }
    }

    /// A band limit: empty means full range, so zero clears it — and shows as
    /// a dash rather than a 0 Hz corner.
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
            let field = egui::DragValue::new(&mut value)
                .speed(10.0)
                .range(0.0..=f32::MAX)
                .custom_formatter(|v, _| {
                    if v > 0.0 {
                        format!("{v:.0} Hz")
                    } else {
                        "—".to_owned()
                    }
                })
                .custom_parser(|text| {
                    let text = text.trim().trim_end_matches("Hz").trim();
                    if text.is_empty() || text == "—" {
                        Some(0.0)
                    } else {
                        text.parse().ok()
                    }
                });
            let response = widgets::number_field(ui, widgets::FIELD_WIDTH, field);
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

    /// The Test tab: one group, the pink-noise button in its bar and, in its
    /// inset, the trigger, the isolation and the level — global settings
    /// rather than this speaker's, so they sit beside the button instead of
    /// repeating for every speaker.
    fn speaker_test_tab(&mut self, ui: &mut Ui, index: usize) {
        let running = self.host.read().speaker_test.running == Some(index);
        let hold = self.speaker_test_mode == "hold";
        // What the button asked: `Some(true)` to start, `Some(false)` to stop.
        // Read after the group, whose bar borrows nothing of the editor.
        let mut wanted = None;
        Group::new(t("speaker.test"))
            .help("help.speaker.test")
            .actions(|ui| {
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
                if hold {
                    // Hold: the test lasts exactly as long as the press.
                    let down = button.is_pointer_button_down_on();
                    if down && !running {
                        wanted = Some(true);
                    } else if running && !down {
                        wanted = Some(false);
                    }
                } else if button.clicked() {
                    wanted = Some(!running);
                }
            })
            .show(ui, |ui| {
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
            });
        match wanted {
            Some(true) => self.start_speaker_test(index),
            Some(false) => self.stop_speaker_test(),
            None => {}
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
