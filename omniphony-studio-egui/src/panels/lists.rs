//! The object and speaker lists of the right overlay (`#objectsList`,
//! `#speakersList`, `speakers.js`): one `.info-item` per entry with its name
//! chip, level meter with peak-hold cursor, dB readout and the M/S pair.

use egui::{Color32, RichText, Sense, Ui, vec2};

use crate::app::StudioSpike;
use crate::host::peak_hold::METER_DB_MIN;
use crate::i18n::t;
use crate::model::app_state::Meter;
use crate::panels::audio::meter_fraction;
use crate::ui::{section::Section, theme, widgets};
use crate::view::{self, Selection};

/// The injected test source owns its own muting: `control_object_mute` takes a
/// number and this id is a name (`object-test-id.js`).
const OBJECT_TEST_SOURCE_ID: &str = "injection";

/// One row of a list, already read out of the model.
struct Row {
    id: String,
    label: String,
    meter: Option<Meter>,
    hold: Option<f64>,
    muted: bool,
    colour: Color32,
    /// Extra note shown after the label (bed channels, gain).
    detail: Option<String>,
}

impl StudioSpike {
    pub(crate) fn objects_section(&mut self, ui: &mut Ui) {
        let rows = self.object_rows();
        Section::new("objectsSection", "section.objects")
            .default_open(true)
            .summary(format!("{}", rows.len()))
            .max_height(crate::ui::section::open_max_height_large(
                ui.ctx().content_rect().height(),
            ))
            .show(ui, |ui| {
                if rows.is_empty() {
                    widgets::note(ui, t("objects.none"));
                }
                for row in &rows {
                    let selected = self.selection.object.as_deref() == Some(row.id.as_str());
                    let action = list_row(ui, row, selected);
                    self.apply_row_action(action, row, false);
                }
            });
    }

    pub(crate) fn speakers_section(&mut self, ui: &mut Ui) {
        let rows = self.speaker_rows();
        let layout_name = {
            let live = self.live.lock().unwrap();
            live.app
                .layouts
                .iter()
                .find(|l| Some(&l.key) == live.app.selected_layout_key.as_ref())
                .map(|l| l.name.clone())
                .unwrap_or_default()
        };
        Section::new("speakersSection", "section.speakers")
            .default_open(true)
            .summary(layout_name)
            .max_height(crate::ui::section::open_max_height_large(
                ui.ctx().content_rect().height(),
            ))
            .show(ui, |ui| {
                if rows.is_empty() {
                    widgets::note(ui, t("speakers.none"));
                }
                for row in &rows {
                    let index: Option<usize> = row.id.parse().ok();
                    let selected = index.is_some() && self.selection.speaker == index;
                    let action = list_row(ui, row, selected);
                    self.apply_row_action(action, row, true);
                }
            });
    }

    fn object_rows(&self) -> Vec<Row> {
        let live = self.live.lock().unwrap();
        let mut rows: Vec<Row> = live
            .app
            .sources
            .iter()
            .map(|(id, src)| {
                let (base, _semantic) = view::objects::base_color(id, src.name.as_deref());
                Row {
                    id: id.clone(),
                    label: view::objects::display_name(id, src.name.as_deref()),
                    meter: live.app.source_levels.get(id).cloned(),
                    hold: live.peak_hold(&format!("src:{id}")),
                    muted: live.app.object_mutes.get(id).is_some_and(|m| *m != 0),
                    colour: Color32::from_rgb(
                        (base[0].powf(1.0 / 2.2) * 255.0) as u8,
                        (base[1].powf(1.0 / 2.2) * 255.0) as u8,
                        (base[2].powf(1.0 / 2.2) * 255.0) as u8,
                    ),
                    detail: src.fixed.unwrap_or(false).then(|| "bed".to_owned()),
                }
            })
            .collect();
        rows.sort_by(|a, b| match (a.id.parse::<u32>(), b.id.parse::<u32>()) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            _ => a.id.cmp(&b.id),
        });
        rows
    }

    fn speaker_rows(&self) -> Vec<Row> {
        let live = self.live.lock().unwrap();
        live.selected_speakers()
            .iter()
            .enumerate()
            .map(|(index, speaker)| {
                let key = index.to_string();
                let gain = live.app.speaker_gains.get(&key).copied();
                Row {
                    id: key.clone(),
                    label: speaker.id.clone(),
                    meter: live.app.speaker_levels.get(&key).cloned(),
                    hold: live.peak_hold(&format!("spk:{key}")),
                    muted: live.app.speaker_mutes.get(&key).is_some_and(|m| *m != 0),
                    colour: theme::TEXT,
                    detail: gain
                        .filter(|g| (*g - 1.0).abs() > 1e-3)
                        .map(|g| crate::panels::audio::format_linear_as_db(Some(g))),
                }
            })
            .collect()
    }

    /// Apply what the row's controls asked for: selection, mute, solo.
    fn apply_row_action(&mut self, action: RowAction, row: &Row, speaker: bool) {
        match action {
            RowAction::None => {}
            RowAction::Select => {
                let index: Option<usize> = row.id.parse().ok();
                let already = if speaker {
                    self.selection.speaker == index
                } else {
                    self.selection.object.as_deref() == Some(row.id.as_str())
                };
                self.selection = if speaker {
                    Selection {
                        object: None,
                        speaker: (!already).then_some(index).flatten(),
                    }
                } else {
                    Selection {
                        object: (!already).then(|| row.id.clone()),
                        speaker: None,
                    }
                };
            }
            RowAction::Mute => self.set_muted(&row.id, !row.muted, speaker),
            RowAction::Solo => self.solo(&row.id, speaker),
        }
    }

    /// `sendObjectMute` / `sendSpeakerMute` plus the optimistic local write.
    fn set_muted(&mut self, id: &str, muted: bool, speaker: bool) {
        if !speaker && id == OBJECT_TEST_SOURCE_ID {
            // The injected source owns its own muting; it is not addressable
            // by index, so the renderer would read `NaN`.
            return;
        }
        let Ok(index) = id.parse::<i32>() else {
            return;
        };
        {
            let mut live = self.live.lock().unwrap();
            let map = if speaker {
                &mut live.app.speaker_mutes
            } else {
                &mut live.app.object_mutes
            };
            map.insert(id.to_owned(), u8::from(muted));
        }
        if speaker {
            // `control_speaker_mute` goes through the speakers config document.
            self.ctl.send_json(
                "/omniphony/control/config/speakers",
                &serde_json::json!({
                    "speakerEdits": [{ "id": index.max(0), "muted": muted }]
                }),
            );
        } else {
            self.ctl.send_int(
                &format!("/omniphony/control/object/{index}/mute"),
                i32::from(muted),
            );
        }
    }

    /// `toggleSolo`: mute everything else, or unmute everything when this
    /// entry is already the only one playing.
    fn solo(&mut self, id: &str, speaker: bool) {
        let ids: Vec<String> = {
            let live = self.live.lock().unwrap();
            if speaker {
                (0..live.selected_speakers().len())
                    .map(|i| i.to_string())
                    .collect()
            } else {
                live.app.sources.keys().cloned().collect()
            }
        };
        if ids.len() <= 1 {
            return;
        }
        let muted_now: Vec<bool> = {
            let live = self.live.lock().unwrap();
            ids.iter()
                .map(|other| {
                    let map = if speaker {
                        &live.app.speaker_mutes
                    } else {
                        &live.app.object_mutes
                    };
                    map.get(other).is_some_and(|m| *m != 0)
                })
                .collect()
        };
        let unmuted: Vec<&String> = ids
            .iter()
            .zip(&muted_now)
            .filter(|(_, muted)| !**muted)
            .map(|(id, _)| id)
            .collect();
        let solo_target = (unmuted.len() == 1).then(|| unmuted[0].clone());

        if solo_target.as_deref() == Some(id) {
            // Already soloed: lift the mutes.
            for other in &ids {
                if other != id {
                    self.set_muted(other, false, speaker);
                }
            }
            return;
        }
        for (other, muted) in ids.iter().zip(&muted_now) {
            if other == id {
                if *muted {
                    self.set_muted(other, false, speaker);
                }
            } else if !*muted {
                self.set_muted(other, true, speaker);
            }
        }
        if !speaker {
            self.selection = Selection {
                object: Some(id.to_owned()),
                speaker: None,
            };
        }
    }
}

/// What a row's controls asked for.
enum RowAction {
    None,
    Select,
    Mute,
    Solo,
}

/// One `.info-item`: name chip, meter, readout, M and S.
fn list_row(ui: &mut Ui, row: &Row, selected: bool) -> RowAction {
    let mut action = RowAction::None;
    let fill = if selected {
        Color32::from_rgba_unmultiplied(46, 110, 64, 115)
    } else {
        Color32::from_rgba_unmultiplied(255, 255, 255, 10)
    };
    let stroke = if selected {
        egui::Stroke::new(1.0, Color32::from_rgba_unmultiplied(90, 200, 120, 89))
    } else {
        egui::Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 20))
    };
    let response = egui::Frame::new()
        .fill(fill)
        .stroke(stroke)
        .corner_radius(theme::CONTROL_RADIUS)
        .inner_margin(egui::Margin::symmetric(7, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::Label::new(RichText::new(&row.label).size(theme::FONT_SIZE).color(
                        if row.muted {
                            theme::TEXT_DIM
                        } else {
                            row.colour
                        },
                    ))
                    .truncate()
                    .selectable(false),
                );
                if let Some(detail) = &row.detail {
                    ui.label(
                        RichText::new(detail)
                            .size(theme::FONT_SIZE_SMALL)
                            .color(theme::TEXT_MUTED),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if toggle_letter(ui, "S", false).clicked() {
                        action = RowAction::Solo;
                    }
                    if toggle_letter(ui, "M", row.muted).clicked() {
                        action = RowAction::Mute;
                    }
                    let rms = row.meter.as_ref().map_or(METER_DB_MIN, |m| m.rms_dbfs);
                    ui.add_sized(
                        vec2(48.0, ui.spacing().interact_size.y),
                        egui::Label::new(
                            RichText::new(format!("{rms:.1} dB"))
                                .monospace()
                                .size(theme::FONT_SIZE_SMALL)
                                .color(theme::TEXT_MUTED),
                        ),
                    );
                    let peak = row.meter.as_ref().map_or(METER_DB_MIN, |m| m.peak_dbfs);
                    let hold = row.hold.unwrap_or(peak);
                    widgets::meter(
                        ui,
                        meter_fraction(peak),
                        (hold > METER_DB_MIN).then(|| meter_fraction(hold)),
                        hold >= 0.0,
                    );
                });
            });
        })
        .response
        .interact(Sense::click());
    if response.clicked() && matches!(action, RowAction::None) {
        action = RowAction::Select;
    }
    action
}

/// The `.toggle-btn` M / S squares of a row.
fn toggle_letter(ui: &mut Ui, letter: &str, active: bool) -> egui::Response {
    let text = RichText::new(letter)
        .size(theme::FONT_SIZE_SMALL)
        .color(if active {
            Color32::WHITE
        } else {
            theme::TEXT_MUTED
        });
    ui.add(
        egui::Button::new(text)
            .min_size(vec2(20.0, 18.0))
            .fill(if active {
                Color32::from_rgba_unmultiplied(255, 255, 255, 51)
            } else {
                theme::FILL
            }),
    )
}
