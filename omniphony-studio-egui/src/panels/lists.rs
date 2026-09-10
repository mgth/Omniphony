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

use crate::panels::object_test::OBJECT_TEST_SOURCE_ID;

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
    /// Normalised position, for the plan thumbnail.
    position: Option<[f64; 3]>,
    /// False for a direct feed, which sits outside the room model.
    spatialize: bool,
    /// Band limits, for the crossover glyph. Only a speaker has one: an
    /// object is not filtered, and a "full band" glyph on every object row
    /// would be a column of noise.
    speaker: bool,
    freq_low: Option<f32>,
    freq_high: Option<f32>,
    /// How much of the selected object this entry carries, 0..1, and the same
    /// split per crossover band. Both empty unless an object is selected.
    contribution: Option<f64>,
    band_gains: Vec<f64>,
}

impl StudioSpike {
    pub(crate) fn objects_section(&mut self, ui: &mut Ui) {
        let rows = self.object_rows();
        let cutoffs = self.crossover_cutoffs();
        Section::new("objectsSection", "section.objects")
            .default_open(true)
            .summary(format!("{}", rows.len()))
            .max_height(crate::ui::section::open_max_height_large(
                ui.ctx().content_rect().height(),
            ))
            .show(ui, |ui| {
                self.object_test_feature_row(ui);
                if rows.is_empty() {
                    widgets::note(ui, t("objects.none"));
                }
                for row in &rows {
                    let selected = self.selection.object.as_deref() == Some(row.id.as_str());
                    let action = list_row(ui, row, selected, &cutoffs);
                    self.apply_row_action(action, row, false);
                }
            });
    }

    pub(crate) fn speakers_section(&mut self, ui: &mut Ui) {
        let rows = self.speaker_rows();
        let cutoffs = self.crossover_cutoffs();
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
                self.layout_actions(ui);
                if rows.is_empty() {
                    widgets::note(ui, t("speakers.none"));
                }
                for row in &rows {
                    let index: Option<usize> = row.id.parse().ok();
                    let selected = index.is_some() && self.selection.speaker == index;
                    let action = list_row(ui, row, selected, &cutoffs);
                    self.apply_row_action(action, row, true);
                }
            });
    }

    /// The band edges the layout's spatialized speakers imply. Derived, never
    /// stored: a stored copy goes stale the moment a band limit is edited.
    fn crossover_cutoffs(&self) -> Vec<f64> {
        let live = self.live.lock().unwrap();
        crate::model::layouts::crossover_cutoffs(&live.selected_speakers())
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
                    position: Some([src.x, src.y, src.z]),
                    spatialize: true,
                    speaker: false,
                    freq_low: None,
                    freq_high: None,
                    contribution: None,
                    band_gains: Vec::new(),
                }
            })
            .collect();
        // Numbered objects keep the stream's order; bed channels take the
        // classic 5.1/7.1 channel order rather than an alphabetical one, and
        // the injected test source sorts last because it is neither.
        let rank = |row: &Row| self.channel_catalog.rank(&live.app, &row.label);
        rows.sort_by(|a, b| match (a.id.parse::<u32>(), b.id.parse::<u32>()) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            (Ok(_), Err(_)) => std::cmp::Ordering::Less,
            (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
            _ => match (rank(a), rank(b)) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                _ => a.id.cmp(&b.id),
            },
        });
        rows
    }

    fn speaker_rows(&self) -> Vec<Row> {
        let live = self.live.lock().unwrap();
        // The contribution overlay answers "where does *this* object go", so it
        // exists only while one is selected.
        let selected = self.selection.object.as_deref();
        let speaker_gains = selected.and_then(|id| live.app.object_speaker_gains.get(id));
        let band_gains = selected.and_then(|id| live.app.object_band_gains.get(id));
        let source_rms = selected
            .and_then(|id| live.app.source_levels.get(id))
            .map(|m| m.rms_dbfs);
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
                    position: Some([speaker.x, speaker.y, speaker.z]),
                    spatialize: speaker.spatialize != 0,
                    speaker: true,
                    freq_low: speaker.freq_low,
                    freq_high: speaker.freq_high,
                    // The object's own RMS through this speaker's panning gain
                    // — what it actually contributes, not what it was asked for.
                    contribution: speaker_gains
                        .and_then(|gains| gains.get(index).copied())
                        .filter(|g| *g > 0.0)
                        .zip(source_rms)
                        .map(|(g, rms)| meter_fraction(rms + 20.0 * g.log10()) as f64),
                    band_gains: band_gains
                        .and_then(|bands| bands.get(index).cloned())
                        .unwrap_or_default(),
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
            // The injected source owns its own muting: it is not addressable
            // by index, so `control_object_mute` would read `NaN`. Stopping
            // the signal while remembering that it was playing is what makes
            // unmuting resume rather than need the transport again.
            self.live
                .lock()
                .unwrap()
                .app
                .object_mutes
                .insert(id.to_owned(), u8::from(muted));
            self.set_object_test_muted(muted);
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
fn list_row(ui: &mut Ui, row: &Row, selected: bool, cutoffs: &[f64]) -> RowAction {
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
                if let Some(position) = row.position {
                    crate::panels::row_glyphs::position_icon(ui, position, row.spatialize);
                }
                if row.speaker {
                    crate::panels::row_glyphs::filter_icon(ui, row.freq_low, row.freq_high);
                }
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
                    let response = widgets::meter(
                        ui,
                        meter_fraction(peak),
                        (hold > METER_DB_MIN).then(|| meter_fraction(hold)),
                        hold >= 0.0,
                    );
                    // The selected object's own share of this speaker, painted
                    // over the level so the two are read against one scale.
                    if let Some(contribution) = row.contribution {
                        let rect = response.rect;
                        let mut fill = rect;
                        fill.set_width(rect.width() * contribution.clamp(0.0, 1.0) as f32);
                        ui.painter().rect_filled(
                            fill,
                            3.0,
                            Color32::from_rgba_unmultiplied(138, 240, 255, 235),
                        );
                    }
                });
            });
            if !row.band_gains.is_empty() {
                crate::panels::row_glyphs::band_bars(ui, cutoffs, &row.band_gains);
            }
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
