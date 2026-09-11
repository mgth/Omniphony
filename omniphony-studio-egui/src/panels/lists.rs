//! The object and speaker lists of the right overlay (`#objectsList`,
//! `#speakersList`, `speakers.js`): one `.info-item` per entry with its name
//! chip, level meter with peak-hold cursor, dB readout and the M/S pair.

use egui::{Color32, RichText, Sense, Ui, vec2};

use crate::app::StudioSpike;
use crate::host::peak_hold::METER_DB_MIN;
use crate::i18n::t;
use crate::model::app_state::Meter;
use crate::panels::audio::meter_fraction;
use crate::panels::row_glyphs;
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
    /// The object's extents, `[w, d, h]` in 0..1 — only objects have them.
    size: Option<[f32; 3]>,
    /// Trail points are still alive for this object, i.e. it is moving.
    moving: bool,
    /// What the vertical badge carries: a short code, or an icon for a
    /// synthesized object (whose full name then shows on hover).
    strip: String,
    strip_icon: Option<row_glyphs::BadgeIcon>,
    /// The badge carries the object's own colour, as `.object-colorized` does
    /// when the colour switch is on.
    colorized: bool,
    /// The object's coordinate line (`.object-head`), when details are on.
    details: Option<RowDetails>,
}

/// `.object-head`: where the object is, in both coordinate systems, and the
/// speaker it lands on most — or the one it is routed to.
struct RowDetails {
    coords: String,
    /// `.object-topright`.
    target: String,
    /// Says what `target` is when it is a routing rather than a gain.
    target_hover: Option<String>,
}

impl StudioSpike {
    pub(crate) fn objects_section(&mut self, ui: &mut Ui) {
        let rows = self.object_rows();
        let cutoffs = self.crossover_cutoffs();
        Section::new("objectsSection", "section.objects")
            .default_open(true)
            .summary(format!("{}", rows.len()))
            .header_toggle(
                self.settings.show_object_details,
                t("display.showObjectDetails"),
            )
            .show(ui, |ui| {
                self.object_test_feature_row(ui);
                if rows.is_empty() {
                    widgets::note(ui, t("objects.none"));
                }
                for row in &rows {
                    let selected = self.selection.object.as_deref() == Some(row.id.as_str());
                    let (action, rect) = list_row(
                        ui,
                        "objects",
                        row,
                        RowState {
                            selected,
                            dragging: false,
                            flash: false,
                        },
                        &cutoffs,
                    );
                    if selected {
                        self.reveal_selected_row(ui, rect);
                    }
                    self.apply_row_action(action, row, false);
                }
            });
        if Section::header_toggled(ui, "objectsSection") {
            self.settings.show_object_details = !self.settings.show_object_details;
        }
    }

    /// The two ear rows (`#hpChannelsList`). They are the output in binaural
    /// mode, so they carry the same meter and mute a speaker row does — but
    /// they are addressed by ear, not by layout index.
    pub(crate) fn headphones_section(&mut self, ui: &mut Ui) {
        let mode = {
            let live = self.live.lock().unwrap();
            crate::panels::renderer::OutputMode::from_state(live.app.binaural.as_ref())
        };
        if mode == crate::panels::renderer::OutputMode::Speaker {
            return;
        }
        let rows = self.ear_rows();
        ui.add_space(theme::PANEL_GAP);
        ui.separator();
        // Hardcoded English in the web too: this header has no i18n key.
        ui.label(
            RichText::new("Headphones")
                .size(theme::FONT_SIZE_SECTION)
                .color(theme::TEXT_STRONG),
        );
        for (ear, row) in rows.iter().enumerate() {
            let (action, _) = list_row(ui, "ears", row, RowState::default(), &[]);
            if matches!(action, RowAction::Mute) {
                self.set_ear_muted(ear, !row.muted);
            }
        }
    }

    fn ear_rows(&self) -> Vec<Row> {
        let live = self.live.lock().unwrap();
        let muted = |ear: usize| {
            live.app
                .binaural
                .as_ref()
                .and_then(|b| b.get("ears"))
                .and_then(|e| e.get(ear))
                .and_then(|e| e.get("muted"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        };
        ["L", "R"]
            .into_iter()
            .enumerate()
            .map(|(ear, label)| {
                let key = ear.to_string();
                Row {
                    id: key.clone(),
                    label: label.to_owned(),
                    strip: label.to_owned(),
                    strip_icon: None,
                    meter: live.ear_levels.get(&key).cloned(),
                    hold: live.peak_hold(&format!("ear:{key}")),
                    muted: muted(ear),
                    colour: theme::TEXT,
                    detail: None,
                    position: None,
                    spatialize: true,
                    speaker: false,
                    freq_low: None,
                    freq_high: None,
                    contribution: None,
                    band_gains: Vec::new(),
                    size: None,
                    moving: false,
                    colorized: false,
                    details: None,
                }
            })
            .collect()
    }

    /// `control_ear_mute`: the ear is the argument, not an index into a layout.
    fn set_ear_muted(&mut self, ear: usize, muted: bool) {
        self.ctl.send(
            "/omniphony/control/binaural/ear_mute",
            vec![
                rosc::OscType::Int(ear as i32),
                rosc::OscType::Int(i32::from(muted)),
            ],
        );
    }

    pub(crate) fn speakers_section(&mut self, ui: &mut Ui) {
        // In binaural-direct mode the speakers are not the output, so the list
        // stands down; the virtual-room mode shows both because it renders
        // through the speakers into the ears.
        let mode = {
            let live = self.live.lock().unwrap();
            crate::panels::renderer::OutputMode::from_state(live.app.binaural.as_ref())
        };
        if mode == crate::panels::renderer::OutputMode::BinauralDirect {
            return;
        }
        let rows = self.speaker_rows();
        let cutoffs = self.crossover_cutoffs();
        // A clip lights the speaker's own chip for a second, restarting rather
        // than stacking when clips repeat.
        let clip = {
            let live = self.live.lock().unwrap();
            live.clip
                .filter(|(_, at)| at.elapsed() < CLIP_FLASH)
                .map(|(index, _)| index)
        };
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
            .show(ui, |ui| {
                self.layout_actions(ui);
                if rows.is_empty() {
                    widgets::note(ui, t("speakers.none"));
                }
                let pointer = ui.ctx().pointer_interact_pos();
                let mut drop_on: Option<usize> = None;
                for row in &rows {
                    let index: Option<usize> = row.id.parse().ok();
                    let selected = index.is_some() && self.selection.speaker == index;
                    let dragging = self.speaker_drag == index && index.is_some();
                    let flash = index.is_some_and(|i| clip == Some(i as i32));
                    let (action, rect) = list_row(
                        ui,
                        "speakers",
                        row,
                        RowState {
                            selected,
                            dragging,
                            flash,
                        },
                        &cutoffs,
                    );
                    if matches!(action, RowAction::DragStart) {
                        self.speaker_drag = index;
                    }
                    if selected {
                        self.reveal_selected_row(ui, rect);
                    }
                    // The row under the pointer is the one a release lands on.
                    if pointer.is_some_and(|p| rect.contains(p)) {
                        drop_on = index;
                    }
                    self.apply_row_action(action, row, true);
                }
                if ui.input(|i| i.pointer.any_released())
                    && let (Some(from), Some(to)) = (self.speaker_drag.take(), drop_on)
                    && from != to
                {
                    self.move_speaker(from, to);
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
        // Trail points are filtered by age at draw time rather than pruned from
        // the store, so "is it moving" has to apply the same time-to-live —
        // otherwise a badge stays lit forever once its object has moved once.
        let now = std::time::Instant::now();
        let ttl = self.settings.trails.ttl;
        let speakers = live.selected_speakers();
        // The mirror of the speaker list's overlay: with a speaker selected,
        // each object row says what that object puts through it.
        let selected_speaker = self.selection.speaker;
        let show_details = self.settings.show_object_details;
        let band = self.settings.heatmap_band_index;
        let mut rows: Vec<Row> = live
            .app
            .sources
            .iter()
            .map(|(id, src)| {
                let (base, _semantic) = view::objects::base_color(id, src.name.as_deref());
                let name = view::objects::display_name(id, src.name.as_deref());
                let (strip_icon, strip) = object_badge(id, &name, src.kind.as_deref());
                let direct = direct_speaker(src.fixed, src.direct_speaker_index, speakers.len())
                    .and_then(|index| speakers.get(index));
                Row {
                    id: id.clone(),
                    label: name,
                    strip,
                    strip_icon,
                    meter: live.app.source_levels.get(id).cloned(),
                    hold: live.peak_hold(&format!("src:{id}")),
                    muted: live.app.object_mutes.get(id).is_some_and(|m| *m != 0),
                    colour: Color32::from_rgb(
                        (base[0].powf(1.0 / 2.2) * 255.0) as u8,
                        (base[1].powf(1.0 / 2.2) * 255.0) as u8,
                        (base[2].powf(1.0 / 2.2) * 255.0) as u8,
                    ),
                    // The web carries no "bed" marker in the meter line: a
                    // channel routed straight out is said by the position
                    // thumbnail, which is drawn at the destination speaker
                    // and framed in black.
                    detail: None,
                    position: Some(direct.map_or([src.x, src.y, src.z], |s| [s.x, s.y, s.z])),
                    spatialize: direct.is_none(),
                    speaker: false,
                    freq_low: None,
                    freq_high: None,
                    contribution: selected_speaker.and_then(|spk| {
                        contribution_fraction(
                            live.app
                                .object_speaker_gains
                                .get(id)
                                .and_then(|gains| gains.get(spk).copied()),
                            live.app.source_levels.get(id).map(|m| m.rms_dbfs),
                        )
                    }),
                    band_gains: selected_speaker
                        .and_then(|spk| {
                            live.app
                                .object_band_gains
                                .get(id)
                                .map(|bands| band_contributions(bands, spk))
                        })
                        .unwrap_or_default(),
                    colorized: self.settings.object_colors_enabled,
                    details: show_details.then(|| {
                        let (coords, target, target_hover) = match direct {
                            Some(speaker) => (
                                coordinates(
                                    [speaker.x, speaker.y, speaker.z],
                                    Some([
                                        speaker.azimuth_deg,
                                        speaker.elevation_deg,
                                        speaker.distance_m,
                                    ]),
                                ),
                                format!("→ {}", speaker.id),
                                Some(format!(
                                    "{}: {}",
                                    t("channelEdit.destinationSpeaker"),
                                    speaker.id
                                )),
                            ),
                            None => (
                                coordinates(
                                    [src.x, src.y, src.z],
                                    src.azimuth_deg.map(|az| {
                                        [
                                            az,
                                            src.elevation_deg.unwrap_or(0.0),
                                            src.distance_m.unwrap_or(0.0),
                                        ]
                                    }),
                                ),
                                dominant_speaker(
                                    live.app
                                        .object_band_gains
                                        .get(id)
                                        .and_then(|bands| bands.get(band))
                                        .filter(|gains| !gains.is_empty())
                                        .or_else(|| live.app.object_speaker_gains.get(id)),
                                    |index| speakers.get(index).map(|s| s.id.clone()),
                                ),
                                None,
                            ),
                        };
                        RowDetails {
                            coords,
                            target,
                            target_hover,
                        }
                    }),
                    size: live.object_sizes.get(id).copied(),
                    moving: live
                        .trails
                        .get(id)
                        .is_some_and(|t| t.points.iter().any(|p| now.duration_since(p.t) < ttl)),
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
                    strip: speaker.id.clone(),
                    strip_icon: None,
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
                    contribution: contribution_fraction(
                        speaker_gains.and_then(|gains| gains.get(index).copied()),
                        source_rms,
                    ),
                    size: None,
                    moving: false,
                    colorized: false,
                    details: None,
                    band_gains: band_gains
                        .map(|bands| band_contributions(bands, index))
                        .unwrap_or_default(),
                }
            })
            .collect()
    }

    /// Apply what the row's controls asked for: selection, mute, solo.
    /// Keep the selected row in view for a moment after the selection changed.
    /// It scrolls only when the row is not wholly visible and then only as far
    /// as needed — `block: 'nearest'` — so a row that is already in view does
    /// not move, and scrolling away from it afterwards is not undone.
    fn reveal_selected_row(&self, ui: &Ui, rect: egui::Rect) {
        if self.reveal_until.is_none() {
            return;
        }
        if !ui.clip_rect().contains_rect(rect) {
            ui.scroll_to_rect(rect, None);
        }
    }

    fn apply_row_action(&mut self, action: RowAction, row: &Row, speaker: bool) {
        match action {
            // The drag is bookkept by the caller, which knows the drop target.
            RowAction::None | RowAction::DragStart => {}
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

/// One object's per-band gains *through one speaker*, lowest band first —
/// `getSelectedSourceBandContributions` / `…ForObject` in the web.
///
/// The renderer sends one message per band, each carrying a gain for every
/// speaker (`/meter/object/{id}/band/{b}/gains`), so the table is band-major:
/// `bands[b][speaker]`. Reading it speaker-major — `bands[speaker]` — handed
/// the first few speakers a whole band's worth of gains each, one bar per
/// speaker under generic "band n" labels, and gave every other speaker none.
fn band_contributions(bands: &[Vec<f64>], speaker: usize) -> Vec<f64> {
    bands
        .iter()
        .map(|per_speaker| per_speaker.get(speaker).copied().unwrap_or(0.0))
        .collect()
}

/// What an object puts through a speaker, on the meter's own scale: its RMS
/// through the panning gain, not the gain it was asked for. `None` when the
/// object does not reach the speaker at all.
fn contribution_fraction(gain: Option<f64>, source_rms: Option<f64>) -> Option<f64> {
    let gain = gain.filter(|g| *g > 0.0)?;
    let rms = source_rms?;
    Some(meter_fraction(rms + 20.0 * gain.log10()) as f64)
}

/// `objectBadge` + `applyObjectIdentity`: what an object's vertical badge
/// carries. The strip is sized for codes like FL or TBR, so an object's name
/// is reduced to one: the injected test source is `INJ`, the synthesized
/// objects lose their technical prefix (`Ambience_FL` → FL, `Phantom_L_C` →
/// L·C, `DirectH_FL` → FL↑), and anything else loses a single prefix word.
/// Height and phantom objects show their kind as an icon instead.
fn object_badge(
    id: &str,
    name: &str,
    kind: Option<&str>,
) -> (Option<row_glyphs::BadgeIcon>, String) {
    if id == OBJECT_TEST_SOURCE_ID {
        return (None, "INJ".to_owned());
    }
    let icon = match kind {
        Some("height") => Some(row_glyphs::BadgeIcon::Height),
        Some("phantom") => Some(row_glyphs::BadgeIcon::Phantom),
        _ => None,
    };
    // `^Prefix(.+)$`, case-insensitive: the rest, if there is any.
    let after = |prefix: &str| -> Option<&str> {
        let head = name.get(..prefix.len())?;
        let rest = &name[prefix.len()..];
        (head.eq_ignore_ascii_case(prefix) && !rest.is_empty()).then_some(rest)
    };
    if let Some(rest) = after("Ambience_") {
        return (icon, rest.to_owned());
    }
    if let Some(rest) = after("Height_")
        && let Some(cut) = rest.len().checked_sub("_synth".len())
        && cut > 0
        && rest.is_char_boundary(cut)
        && rest[cut..].eq_ignore_ascii_case("_synth")
    {
        return (icon, rest[..cut].to_owned());
    }
    if let Some(rest) = after("Diffuse_") {
        return (icon, rest.to_owned());
    }
    if let Some(rest) = after("Phantom_") {
        // `Phantom_L_C`: a source localized between two channels.
        return match rest.split_once('_') {
            Some((a, b)) if !a.is_empty() && !b.is_empty() => (icon, format!("{a}·{b}")),
            _ => (icon, rest.to_owned()),
        };
    }
    // The high ring is marked ↑ so its codes stay distinct from the floor's.
    if let Some(rest) = after("DirectH_") {
        return (icon, format!("{rest}↑"));
    }
    if let Some(rest) = after("Direct_") {
        return (icon, rest.to_owned());
    }
    let code = name.split_once('_').map_or(name, |(_, rest)| rest);
    (icon, if code.is_empty() { name } else { code }.to_owned())
}

/// `directFixedSpeakerTarget`: the speaker a fixed channel is routed straight
/// to, if there is one. Both halves are required — `fixed` on its own is a bed
/// channel that is still panned, and an index no layout resolves is not a
/// destination. A row that has one is drawn at *that* speaker and framed in
/// black, because its own coordinates say nothing about where it lands.
fn direct_speaker(fixed: Option<bool>, index: Option<u32>, speakers: usize) -> Option<usize> {
    if fixed != Some(true) {
        return None;
    }
    let index = index? as usize;
    (index < speakers).then_some(index)
}

/// What a row's controls asked for.
enum RowAction {
    None,
    Select,
    Mute,
    Solo,
    /// The id strip was picked up: this row is now the one being reordered.
    DragStart,
}

/// How a row should draw itself.
#[derive(Clone, Copy, Default)]
struct RowState {
    selected: bool,
    /// Being dragged to a new position in the layout.
    dragging: bool,
    /// The renderer reported a clip on this speaker within the last second.
    flash: bool,
}

/// The level readout's column, the web's `8ch` of tabular monospace.
const READOUT_W: f32 = 48.0;
/// One `.toggle-btn` square.
const TOGGLE_W: f32 = 20.0;

/// How long a clip lights a row's id strip. Repeat clips restart it rather
/// than stacking, so a run of them reads as one continuous warning.
const CLIP_FLASH: std::time::Duration = std::time::Duration::from_millis(1000);

/// One `.info-item`: id strip, glyphs, meter, readout, M and S. Returns what
/// the row asked for and where it was drawn, so the caller can use it as a
/// drop target.
fn list_row(
    ui: &mut Ui,
    list: &str,
    row: &Row,
    state: RowState,
    cutoffs: &[f64],
) -> (RowAction, egui::Rect) {
    let mut action = RowAction::None;
    let fill = if state.dragging {
        Color32::from_rgba_unmultiplied(72, 140, 92, 140)
    } else if state.selected {
        Color32::from_rgba_unmultiplied(46, 110, 64, 115)
    } else {
        Color32::from_rgba_unmultiplied(255, 255, 255, 10)
    };
    let stroke = if state.dragging {
        egui::Stroke::new(1.0, Color32::from_rgba_unmultiplied(120, 225, 150, 166))
    } else if state.selected {
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
            let mut strip_rect = egui::Rect::NOTHING;
            ui.horizontal(|ui| {
                // The badge spans the whole row, band bars included, so its
                // shapes are reserved here and filled in once the content
                // below has been laid out and its height is known.
                let slot = ui.painter().add(egui::Shape::Noop);
                let (reserved, _) =
                    ui.allocate_exact_size(vec2(row_glyphs::STRIP_W, 0.0), Sense::hover());
                let content = ui
                    .vertical(|ui| {
                        if let Some(details) = &row.details {
                            details_line(ui, details);
                        }
                        row_line(ui, row, &mut action);
                        if !row.band_gains.is_empty() {
                            row_glyphs::band_bars(ui, cutoffs, &row.band_gains);
                        }
                    })
                    .response
                    .rect;
                strip_rect = egui::Rect::from_min_max(
                    egui::pos2(reserved.left(), content.top()),
                    egui::pos2(reserved.right(), content.bottom()),
                );
                let state = if state.flash {
                    row_glyphs::StripState::Clipping
                } else if row.moving {
                    row_glyphs::StripState::Moving
                } else {
                    row_glyphs::StripState::Rest
                };
                ui.painter().set(
                    slot,
                    egui::Shape::Vec(row_glyphs::id_strip(
                        ui,
                        strip_rect,
                        &row.strip,
                        row.strip_icon,
                        row.colorized.then_some(row.colour),
                        state,
                    )),
                );
            });
            // The badge is the drag handle, as in the web: the row itself stays
            // a click target for selection.
            // The id is spelled out rather than derived from the ui, because
            // the three lists number their rows from zero independently and
            // would otherwise ask egui for the same widget twice in one frame.
            let strip = ui.interact(
                strip_rect,
                egui::Id::new(("row-strip", list, row.id.as_str())),
                if row.speaker {
                    Sense::click_and_drag()
                } else {
                    Sense::click()
                },
            );
            if strip.drag_started() {
                action = RowAction::DragStart;
            }
            if row.speaker {
                strip.on_hover_text("Drag to reorder");
            } else if row.strip_icon.is_some() {
                // The name hides behind the icon, so it shows on hover.
                strip.on_hover_text(&row.label);
            }
        })
        .response
        .interact(Sense::click());
    if response.clicked() && matches!(action, RowAction::None) {
        action = RowAction::Select;
    }
    (action, response.rect)
}

/// `.object-head`: the coordinates on the left, cut short when the row is
/// narrow, and the target speaker kept whole on the right.
fn details_line(ui: &mut Ui, details: &RowDetails) {
    ui.scope(|ui| {
        // The line is 9–10 px text, not a control: it takes a text line's
        // height rather than a button's.
        ui.spacing_mut().interact_size.y = 12.0;
        widgets::label_row(
            ui,
            RichText::new(&details.coords)
                .monospace()
                .size(9.0)
                .color(theme::TEXT_DIM),
            |ui| {
                let target = ui.add(
                    egui::Label::new(
                        RichText::new(&details.target)
                            .size(theme::FONT_SIZE_SMALL)
                            .color(Color32::from_rgb(0xb9, 0xc7, 0xd8)),
                    )
                    .selectable(false),
                );
                if let Some(hover) = &details.target_hover {
                    target.on_hover_text(hover);
                }
            },
        );
    });
}

/// `decomposePosition`, laid out as `.object-coords`' grid: three cartesian
/// columns, a bar, three polar ones. `polar` is the renderer's own reading
/// when it sent one; otherwise it is derived from the cartesian position.
fn coordinates(xyz: [f64; 3], polar: Option<[f64; 3]>) -> String {
    let [x, y, z] = xyz;
    if !(x.is_finite() && y.is_finite() && z.is_finite()) {
        return "x:— y:— z:— | az:— el:— r:—".to_owned();
    }
    let [az, el, r] = polar.unwrap_or_else(|| {
        let planar = (x * x + y * y).sqrt();
        [
            x.atan2(y).to_degrees(),
            z.atan2(planar).to_degrees(),
            (x * x + y * y + z * z).sqrt(),
        ]
    });
    format!(
        "{:<7}{:<7}{:<7}| {:<10}{:<10}{}",
        format!("x:{x:.1}"),
        format!("y:{y:.1}"),
        format!("z:{z:.1}"),
        format!("az:{az:.1}"),
        format!("el:{el:.1}"),
        format!("r:{r:.2}"),
    )
}

/// `getObjectDominantSpeakerText`: the speaker taking the largest gain, and
/// that gain in dB — or a dash when nothing reaches any speaker.
fn dominant_speaker(gains: Option<&Vec<f64>>, name_of: impl Fn(usize) -> Option<String>) -> String {
    let best = gains.and_then(|gains| {
        gains
            .iter()
            .enumerate()
            .filter(|(_, g)| g.is_finite())
            .max_by(|a, b| a.1.total_cmp(b.1))
    });
    match best {
        Some((index, &gain)) if gain > 0.0 => {
            let name = name_of(index).unwrap_or_else(|| index.to_string());
            format!(
                "{name} {}",
                crate::panels::audio::format_linear_as_db(Some(gain))
            )
        }
        _ => "—".to_owned(),
    }
}

/// `.meter-row`: the web's grid `auto [auto] 8ch 1fr [32px] auto`, i.e. the
/// position thumbnail, the crossover glyph on speakers only, the level
/// readout, the meter taking the slack, the object's size gauges, then M and S.
fn row_line(ui: &mut Ui, row: &Row, action: &mut RowAction) {
    ui.horizontal(|ui| {
        if let Some(position) = row.position {
            row_glyphs::position_icon(ui, position, row.spatialize);
        }
        if row.speaker {
            row_glyphs::filter_icon(ui, row.freq_low, row.freq_high);
        }
        let rms = row.meter.as_ref().map_or(METER_DB_MIN, |m| m.rms_dbfs);
        ui.add_sized(
            vec2(READOUT_W, ui.spacing().interact_size.y),
            egui::Label::new(
                RichText::new(format!("{rms:.1} dB"))
                    .monospace()
                    .size(theme::FONT_SIZE_SMALL)
                    .color(theme::TEXT_MUTED),
            ),
        );
        // The web's `1fr`: the meter takes whatever the fixed columns to its
        // right leave. Those are placed first, right to left, and the meter is
        // handed the remainder — measuring them by hand instead would make the
        // row overflow whenever the guess came up short, which widens the
        // scroll area, which widens the next row's slack, and the list fans out
        // as it goes down.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if toggle_letter(ui, "S", false).clicked() {
                *action = RowAction::Solo;
            }
            if toggle_letter(ui, "M", row.muted).clicked() {
                *action = RowAction::Mute;
            }
            if let Some(size) = row.size {
                row_glyphs::size_gauges(ui, size);
            }
            if let Some(detail) = &row.detail {
                ui.label(
                    RichText::new(detail)
                        .size(theme::FONT_SIZE_SMALL)
                        .color(theme::TEXT_MUTED),
                );
            }
            let peak = row.meter.as_ref().map_or(METER_DB_MIN, |m| m.peak_dbfs);
            let hold = row.hold.unwrap_or(peak);
            crate::ui::meter::level_meter_sized(
                ui,
                ui.available_width(),
                meter_fraction(peak),
                (hold > METER_DB_MIN).then(|| meter_fraction(hold)),
                hold >= 0.0,
                row.contribution.map(|c| c as f32),
            );
        });
    });
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

#[cfg(test)]
mod tests {
    use super::{
        band_contributions, contribution_fraction, coordinates, direct_speaker, dominant_speaker,
        object_badge,
    };
    use crate::panels::row_glyphs::BadgeIcon;

    #[test]
    fn coordinates_read_as_the_web_grid_and_derive_the_polar_side() {
        let line = coordinates([0.0, 1.0, 0.0], None);
        assert_eq!(line, "x:0.0  y:1.0  z:0.0  | az:0.0    el:0.0    r:1.00");
        // The renderer's own polar reading wins over a derived one.
        let line = coordinates([1.0, 0.0, 0.0], Some([-30.0, 15.0, 2.5]));
        assert!(line.ends_with("| az:-30.0  el:15.0   r:2.50"), "{line}");
        assert!(coordinates([f64::NAN, 0.0, 0.0], None).starts_with("x:—"));
    }

    #[test]
    fn the_dominant_speaker_is_the_largest_gain_or_a_dash() {
        let names = ["L", "R"];
        let name_of = |i: usize| names.get(i).map(|n| n.to_string());
        assert_eq!(
            dominant_speaker(Some(&vec![0.1, 0.5]), name_of),
            "R -6.0 dB"
        );
        assert_eq!(dominant_speaker(Some(&vec![0.0, 0.0]), name_of), "—");
        assert_eq!(dominant_speaker(None, name_of), "—");
        // A gain past the layout's end still names its index.
        assert_eq!(
            dominant_speaker(Some(&vec![0.0, 0.0, 1.0]), name_of),
            "2 0.0 dB"
        );
    }

    /// The web's own examples, from the comments of `objectBadge`.
    #[test]
    fn a_badge_carries_a_short_code() {
        let code = |name: &str| object_badge("7", name, None).1;
        assert_eq!(code("Ambience_FL"), "FL");
        assert_eq!(code("Height_Ls_synth"), "Ls");
        assert_eq!(code("Diffuse_TFL"), "TFL");
        assert_eq!(code("Phantom_L_C"), "L·C");
        assert_eq!(code("Phantom_C"), "C");
        assert_eq!(code("DirectH_FL"), "FL↑");
        assert_eq!(code("Direct_FL"), "FL");
        assert_eq!(code("Obj_12"), "12");
        assert_eq!(code("LFE"), "LFE");
        // Case-insensitive, as the web's `/i`.
        assert_eq!(code("ambience_fl"), "fl");
    }

    /// The injected test source's name is a sentence; its badge is a code.
    #[test]
    fn the_test_source_is_inj() {
        let id = crate::panels::object_test::OBJECT_TEST_SOURCE_ID;
        assert_eq!(
            object_badge(id, "Objet de test", None),
            (None, "INJ".to_owned())
        );
    }

    /// Height and phantom objects show their kind as an icon.
    #[test]
    fn synthesized_kinds_get_an_icon() {
        assert_eq!(
            object_badge("3", "Height_Ls_synth", Some("height")).0,
            Some(BadgeIcon::Height)
        );
        assert_eq!(
            object_badge("4", "Phantom_L_C", Some("phantom")).0,
            Some(BadgeIcon::Phantom)
        );
        assert_eq!(object_badge("5", "Obj_5", Some("object")).0, None);
    }

    /// The table is band-major: one entry per band, each a gain per speaker.
    /// A speaker gets one bar per *band*, however many speakers there are.
    #[test]
    fn band_gains_are_read_through_one_speaker() {
        let bands = vec![
            vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6],
            vec![0.7, 0.8, 0.9, 1.0, 0.0, 0.25],
        ];
        assert_eq!(band_contributions(&bands, 1), vec![0.2, 0.8]);
        // Past the first few speakers too — the old read gave them nothing.
        assert_eq!(band_contributions(&bands, 5), vec![0.6, 0.25]);
        // A band that did not report this speaker reads as silence, not a gap.
        let ragged = vec![vec![0.5, 0.5], vec![0.5]];
        assert_eq!(band_contributions(&ragged, 1), vec![0.5, 0.0]);
    }

    /// A gain of zero, or no level to scale, is no contribution at all.
    #[test]
    fn an_object_that_does_not_reach_the_speaker_contributes_nothing() {
        assert_eq!(contribution_fraction(Some(0.0), Some(-20.0)), None);
        assert_eq!(contribution_fraction(Some(0.5), None), None);
        assert_eq!(contribution_fraction(None, Some(-20.0)), None);
        // Unity gain leaves the object's own level where it is.
        let unity = contribution_fraction(Some(1.0), Some(-6.0)).unwrap();
        assert!((unity - f64::from(super::meter_fraction(-6.0))).abs() < 1e-9);
    }

    /// The web takes the destination only when the channel is fixed *and*
    /// carries an index a layout can resolve.
    #[test]
    fn a_direct_channel_needs_both_halves() {
        assert_eq!(direct_speaker(Some(true), Some(3), 8), Some(3));
        assert_eq!(direct_speaker(Some(true), None, 8), None, "no index");
        assert_eq!(direct_speaker(None, Some(3), 8), None, "not fixed");
        assert_eq!(direct_speaker(Some(false), Some(3), 8), None, "panned");
    }

    /// A layout change can leave an index pointing past the end; the row then
    /// falls back to the object's own position rather than drawing nothing.
    #[test]
    fn an_index_past_the_layout_is_not_a_destination() {
        assert_eq!(direct_speaker(Some(true), Some(8), 8), None);
        assert_eq!(direct_speaker(Some(true), Some(7), 8), Some(7));
        assert_eq!(direct_speaker(Some(true), Some(0), 0), None);
    }
}
