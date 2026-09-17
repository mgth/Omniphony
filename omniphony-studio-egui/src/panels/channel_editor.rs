//! The channel editor (`#channelEditSection`, `controls/virtual-bed.js`,
//! `listeners/channel-editor-listeners.js`).
//!
//! The table only. What a channel *is* — the catalogue, the two coordinate
//! forms, the bed the renderer is sent — is `host::channels`, and the scene
//! markers that stand in for the channels at rest are published by
//! `host::services::virtual_bed`.
//!
//! Editing reuses the speaker editor's mechanic: the channels appear in the
//! objects list, and selecting one opens this editor under it. Every edit
//! pushes the whole bed, because the renderer takes a layout and not a diff.

use egui::{RichText, Ui};

use crate::app::StudioSpike;
use crate::host::channels::{
    Channel, CoordMode, adm_to_meters, adm_to_polar, build_layout_payload, effective_channels,
    meters_to_adm, polar_to_adm,
};
use crate::host::commands::engine;
use crate::i18n::t;
use crate::model::app_state::RoomRatio;
use crate::ui::{help, theme, widgets};
use crate::view::gizmos::EditMode;

// ---------------------------------------------------------------------------
// The panel
// ---------------------------------------------------------------------------

impl StudioSpike {
    /// The canonical channel the selection names, if it names one.
    pub(crate) fn selected_channel(&self) -> Option<String> {
        let id = self.selection.object.as_deref()?;
        let live = self.host.read();
        let name = live
            .app
            .sources
            .get(id)
            .and_then(|s| s.name.clone())
            .unwrap_or_else(|| id.to_owned());
        live.channels.canonical(&live.app, &name)
    }

    pub(crate) fn channel_editor(&mut self, ui: &mut Ui) {
        let Some(name) = self.selected_channel() else {
            return;
        };
        let (channel, room, scale_m, direct) = {
            let live = self.host.read();
            let channels = effective_channels(&live.channels, &live.app);
            let Some(channel) = channels.into_iter().find(|c| c.name == name) else {
                return;
            };
            let direct = (!channel.spatialize).then(|| self.direct_target(&live, &name));
            (
                channel,
                live.app.room_ratio.clone(),
                live.app.room_ratio.scale_m.max(0.001),
                direct,
            )
        };
        ui.add_space(theme::PANEL_GAP);
        ui.separator();
        ui.label(
            RichText::new(format!("{} — {}", t("channelEdit.title"), name))
                .size(theme::FONT_SIZE_SECTION)
                .color(theme::TEXT_STRONG),
        );

        let mut gain = channel.gain_db as f32;
        widgets::label_row_help(ui, t("channelEdit.gain"), "help.channelEdit.gain", |ui| {
            let readout = if gain > 0.0 {
                format!("+{gain:.1} dB")
            } else {
                format!("{gain:.1} dB")
            };
            // Double-clicking the readout is the web's reset to unity.
            if ui
                .add_sized(
                    egui::vec2(58.0, ui.spacing().interact_size.y),
                    egui::Label::new(RichText::new(readout).monospace().color(theme::TEXT_STRONG))
                        .sense(egui::Sense::click()),
                )
                .double_clicked()
            {
                self.set_channel_gain(&name, 0.0);
            }
            if ui
                .add(
                    widgets::stepped(egui::Slider::new(&mut gain, -24.0..=12.0), 0.1)
                        .show_value(false),
                )
                .changed()
            {
                self.set_channel_gain(&name, f64::from(gain));
            }
        });

        // Virtual or direct. The web puts this on a switch whose *label* is
        // the current state, so the control reads "Direct" when it is direct
        // and "Virtual" when it is virtual — and nothing says which way the
        // switch would move you. Both choices are shown here instead, one
        // highlighted: a deliberate divergence, asked for because the web's
        // version is read backwards as often as forwards.
        let spatialize = channel.spatialize;
        widgets::label_row_help(
            ui,
            t("channelEdit.routing"),
            "help.channelEdit.routing",
            |ui| {
                if let Some(picked) = widgets::toggle_buttons(
                    ui,
                    &spatialize,
                    &[
                        (false, t("virtualBed.direct")),
                        (true, t("virtualBed.virtual")),
                    ],
                ) {
                    self.commit_channel(&name, |c| c.spatialize = picked);
                }
            },
        );

        // Direct: the coordinates below are the speaker's, and nothing about
        // the position is editable.
        if let Some(direct) = &direct {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(t("channelEdit.destinationSpeaker"))
                        .size(theme::FONT_SIZE_SMALL)
                        .color(theme::TEXT_MUTED),
                );
                ui.label(RichText::new(match direct {
                    Some((_, label)) => label.clone(),
                    None => t("channelEdit.noMatchingSpeaker").to_owned(),
                }));
            });
            widgets::note(ui, t("channelEdit.destinationPosition"));
        }

        // A direct channel with a resolved speaker shows that speaker's own
        // coordinate mode; otherwise the editor's own choice decides.
        let speaker = direct.clone().flatten().and_then(|(index, _)| {
            let live = self.host.read();
            live.selected_speakers().get(index).cloned()
        });
        let editable = channel.spatialize;
        let mode = match &speaker {
            Some(s) if s.coord_mode == "polar" => CoordMode::Polar,
            Some(_) => CoordMode::Cartesian,
            None => self.channel_coord_mode,
        };
        let position = match &speaker {
            Some(s) => Some([s.x, s.y, s.z]),
            None if direct.is_some() => None,
            None => Some([channel.x, channel.y, channel.z]),
        };

        ui.add_enabled_ui(editable, |ui| {
            ui.horizontal(|ui| {
                let mut chosen = mode;
                ui.selectable_value(&mut chosen, CoordMode::Cartesian, t("common.cartesian"));
                ui.selectable_value(&mut chosen, CoordMode::Polar, t("common.polar"));
                if chosen != mode {
                    self.channel_coord_mode = chosen;
                }
            });
        });
        match mode {
            CoordMode::Cartesian => {
                self.channel_cartesian_table(ui, &name, position, &room, scale_m, editable)
            }
            CoordMode::Polar => {
                self.channel_polar_table(ui, &name, position, &room, scale_m, editable)
            }
        }
        // The speaker editor's "3D Edit" toggle, which the web had on this
        // editor too: a virtual channel is dragged with the same gizmo. A
        // direct channel sits where its speaker is and has nothing to arm.
        self.gizmo_button(
            ui,
            match mode {
                CoordMode::Cartesian => EditMode::Cartesian,
                CoordMode::Polar => EditMode::Polar,
            },
            !editable,
        );
    }

    /// The output speaker a direct channel actually reaches: the renderer's
    /// reported index first (it includes routing choices such as a 5.1 surround
    /// mapped to the back row), then a label match so the editor stays
    /// informative while offline.
    fn direct_target(
        &self,
        live: &crate::osc::dispatch::Live,
        name: &str,
    ) -> Option<(usize, String)> {
        let reported = self
            .selection
            .object
            .as_deref()
            .and_then(|id| live.app.sources.get(id))
            .and_then(|s| s.direct_speaker_index)
            .map(|i| i as usize);
        let speakers = live.selected_speakers();
        let index = reported.or_else(|| {
            speakers
                .iter()
                .position(|s| live.channels.canonical(&live.app, &s.id).as_deref() == Some(name))
        })?;
        let speaker = speakers.get(index)?;
        Some((index, speaker.id.clone()))
    }

    #[allow(clippy::too_many_arguments)]
    fn channel_cartesian_table(
        &mut self,
        ui: &mut Ui,
        name: &str,
        position: Option<[f64; 3]>,
        room: &RoomRatio,
        scale_m: f64,
        editable: bool,
    ) {
        let adm = position.unwrap_or([f64::NAN; 3]);
        let meters = adm_to_meters(room, adm, scale_m);
        let mut edit: Option<[f64; 3]> = None;
        ui.add_enabled_ui(editable, |ui| {
            ui.horizontal(|ui| {
                coord_label(ui, t("speaker.normalizedCoords"));
                for (index, axis) in ["X", "Y", "Z"].into_iter().enumerate() {
                    let mut value = adm[index] as f32;
                    axis_label(ui, axis);
                    if coord_field(ui, &mut value, 0.001, Some(-1.0..=1.0), adm[index].is_nan()) {
                        let mut next = adm;
                        next[index] = f64::from(value).clamp(-1.0, 1.0);
                        edit = Some(next);
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
                for (index, axis) in ["X", "Y", "Z"].into_iter().enumerate() {
                    let mut value = meters[index] as f32;
                    axis_label(ui, axis);
                    if coord_field(ui, &mut value, 0.01, None, adm[index].is_nan()) {
                        // The edited axis in metres, the other two canonical:
                        // converting all three back would round-trip values the
                        // user did not touch.
                        let mut next_m = meters;
                        next_m[index] = f64::from(value);
                        edit = Some(meters_to_adm(room, next_m, scale_m));
                    }
                }
            });
            help::card(ui, "help.speaker.positionMeters");
        });
        if let Some(next) = edit {
            self.set_channel_cartesian(name, next);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn channel_polar_table(
        &mut self,
        ui: &mut Ui,
        name: &str,
        position: Option<[f64; 3]>,
        room: &RoomRatio,
        scale_m: f64,
        editable: bool,
    ) {
        let blank = position.is_none();
        let adm = position.unwrap_or([0.0; 3]);
        let (az, el, dist) = adm_to_polar(room, adm);
        let meters = adm_to_meters(room, adm, scale_m);
        let dist_m = (meters[0] * meters[0] + meters[1] * meters[1] + meters[2] * meters[2]).sqrt();
        let mut edit: Option<(f64, f64, f64)> = None;
        ui.add_enabled_ui(editable, |ui| {
            ui.horizontal(|ui| {
                coord_label(ui, t("speaker.normalizedCoords"));
                let mut a = az as f32;
                axis_label(ui, "Az°");
                if coord_field(ui, &mut a, 0.1, None, blank) {
                    edit = Some((f64::from(a), el, dist));
                }
                let mut e = el as f32;
                axis_label(ui, "El°");
                if coord_field(ui, &mut e, 0.1, None, blank) {
                    edit = Some((az, f64::from(e), dist));
                }
                let mut d = dist as f32;
                axis_label(ui, "Dist");
                if coord_field(ui, &mut d, 0.001, Some(0.01..=f32::MAX), blank) {
                    edit = Some((az, el, f64::from(d)));
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
                let mut d = dist_m as f32;
                axis_label(ui, "Dist");
                if coord_field(ui, &mut d, 0.01, Some(0.01..=f32::MAX), blank) {
                    edit = Some((az, el, f64::from(d) / scale_m));
                }
            });
            help::card(ui, "help.speaker.positionMeters");
        });
        if let Some((az, el, dist)) = edit {
            self.set_channel_polar(name, az, el, dist);
        }
    }

    // -----------------------------------------------------------------------
    // Commits
    // -----------------------------------------------------------------------

    fn set_channel_gain(&mut self, name: &str, gain_db: f64) {
        // 0.1 dB resolution, matching the per-speaker output gain.
        let rounded = (gain_db * 10.0).round() / 10.0;
        self.commit_channel(name, |c| c.gain_db = rounded);
    }

    /// Commit a cartesian placement from the normalised fields: the wire payload
    /// ships the cartesian block and the renderer derives the pose, the same
    /// path the output speakers use, so the channel lands exactly where it was
    /// put.
    fn set_channel_cartesian(&mut self, name: &str, adm: [f64; 3]) {
        let room = self.host.read().app.room_ratio.clone();
        let adm = [
            adm[0].clamp(-1.0, 1.0),
            adm[1].clamp(-1.0, 1.0),
            adm[2].clamp(-1.0, 1.0),
        ];
        let (azimuth, elevation, distance) = adm_to_polar(&room, adm);
        self.commit_channel(name, |c| {
            c.coord_mode = CoordMode::Cartesian;
            c.x = adm[0];
            c.y = adm[1];
            c.z = adm[2];
            c.azimuth = azimuth;
            c.elevation = elevation;
            c.distance = distance;
        });
    }

    /// The gizmo's version: the same commit, except that a drag in flight
    /// only moves the local copy. The bed is a whole layout, and pushing one
    /// per pointer move would be a stream of layouts.
    pub(crate) fn set_channel_polar_from_drag(
        &mut self,
        name: &str,
        azimuth: f64,
        elevation: f64,
        distance: f64,
        send: bool,
    ) {
        if send {
            self.set_channel_polar(name, azimuth, elevation, distance);
            return;
        }
        let room = self.host.read().app.room_ratio.clone();
        let distance = distance.max(0.01);
        let adm = polar_to_adm(&room, azimuth, elevation, distance);
        let live = self.host.read();
        let mut channels = effective_channels(&live.channels, &live.app);
        let Some(target) = channels.iter_mut().find(|c| c.name == name) else {
            return;
        };
        target.coord_mode = CoordMode::Polar;
        target.azimuth = azimuth;
        target.elevation = elevation;
        target.distance = distance;
        target.x = adm[0];
        target.y = adm[1];
        target.z = adm[2];
        let payload = build_layout_payload(&live.app, &channels);
        drop(live);
        engine::preview_virtual_bed(&self.host, payload);
    }

    fn set_channel_polar(&mut self, name: &str, azimuth: f64, elevation: f64, distance: f64) {
        let room = self.host.read().app.room_ratio.clone();
        let distance = if distance > 0.0 { distance } else { 0.01 };
        let adm = polar_to_adm(&room, azimuth, elevation, distance);
        self.commit_channel(name, |c| {
            c.coord_mode = CoordMode::Polar;
            c.azimuth = azimuth;
            c.elevation = elevation;
            c.distance = distance;
            c.x = adm[0];
            c.y = adm[1];
            c.z = adm[2];
        });
    }

    /// Mutate one channel, then push the whole bed: the renderer takes a layout,
    /// not a diff. Optimistic, like the web — the renderer echoes `virtualBed`
    /// back in the snapshot.
    fn commit_channel(&mut self, name: &str, mutate: impl FnOnce(&mut Channel)) {
        let payload = {
            let live = self.host.read();
            let mut channels = effective_channels(&live.channels, &live.app);
            let Some(target) = channels.iter_mut().find(|c| c.name == name) else {
                return;
            };
            mutate(target);
            build_layout_payload(&live.app, &channels)
        };
        self.send_virtual_bed(payload);
    }

    fn send_virtual_bed(&mut self, payload: serde_json::Value) {
        engine::set_virtual_bed(&self.host, payload);
    }
}

fn coord_label(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .size(theme::FONT_SIZE_SMALL)
            .color(theme::TEXT_MUTED),
    );
}

fn axis_label(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .size(theme::FONT_SIZE_SMALL)
            .color(theme::TEXT_DIM),
    );
}

/// One coordinate cell. A direct channel with no matching speaker has nothing
/// to show, so the cell is blanked rather than filled with a made-up zero.
fn coord_field(
    ui: &mut Ui,
    value: &mut f32,
    speed: f64,
    range: Option<std::ops::RangeInclusive<f32>>,
    blank: bool,
) -> bool {
    let size = egui::vec2(56.0, ui.spacing().interact_size.y);
    if blank {
        ui.add_sized(size, egui::Label::new("—"));
        return false;
    }
    let mut drag = egui::DragValue::new(value).speed(speed);
    if let Some(range) = range {
        drag = drag.range(range);
    }
    ui.add_sized(size, drag).changed()
}
