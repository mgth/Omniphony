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
//! pushes the whole layout of the family being edited, because the renderer
//! takes a layout and not a diff. Positions are editable in manual mode
//! only: in sphere and room mode the family's model places the channel, and
//! the editor offers the switch.

use egui::{RichText, Ui};

use crate::app::StudioSpike;
use crate::host::channels::{
    Channel, CoordMode, PlacementMode, adm_to_meters, adm_to_polar, build_layout_payload,
    effective_channels_for, family_placement, meters_to_adm, polar_to_adm,
};
use crate::host::commands::engine;
use crate::i18n::t;
use crate::model::app_state::RoomRatio;
use crate::ui::group::Group;
use crate::ui::widgets::{CoordCell, CoordRow};
use crate::ui::{section, theme, widgets};
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
        let (channel, room, scale_m, direct, family, mode) = {
            let live = self.host.read();
            let family = live.editing_family;
            let channels = effective_channels_for(&live.channels, &live.app, family);
            let Some(channel) = channels.into_iter().find(|c| c.name == name) else {
                return;
            };
            let direct = (!channel.spatialize).then(|| self.direct_target(&live, &name));
            (
                channel,
                live.app.room_ratio.clone(),
                live.app.room_ratio.scale_m.max(0.001),
                direct,
                family,
                family_placement(&live.app, family).effective_mode,
            )
        };
        let trailing = format!("{name} · {}", t(family.i18n_key()));
        section::pinned_header(ui, t("channelEdit.title"), Some(&trailing));

        // The gain first, before the routing: an input trim that applies in
        // both placements, so it must not read as a setting of one of them.
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

        // Routing: virtual or direct, the choice in the bar. The web puts this
        // on a switch whose *label* is the current state, so the control reads
        // "Direct" when it is direct and "Virtual" when it is virtual — and
        // nothing says which way the switch would move you. Both choices are
        // shown here instead, one highlighted: a deliberate divergence, asked
        // for because the web's version is read backwards as often as
        // forwards. A direct channel says which speaker it reaches in the
        // inset; a virtual one has nothing to add, and the inset goes.
        let spatialize = channel.spatialize;
        let mut routing = spatialize;
        Group::new(t("channelEdit.routing"))
            .help("help.channelEdit.routing")
            .actions(|ui| {
                if let Some(picked) = widgets::toggle_buttons(
                    ui,
                    &spatialize,
                    &[
                        (false, t("virtualBed.direct")),
                        (true, t("virtualBed.virtual")),
                    ],
                ) {
                    routing = picked;
                }
            })
            .show(ui, |ui| {
                if let Some(direct) = &direct {
                    widgets::label_row(ui, t("channelEdit.destinationSpeaker"), |ui| {
                        let target = match direct {
                            Some((_, label)) => label.as_str(),
                            None => t("channelEdit.noMatchingSpeaker"),
                        };
                        ui.label(RichText::new(target).color(theme::TEXT_STRONG));
                    });
                    widgets::note(ui, t("channelEdit.destinationPosition"));
                }
            });
        if routing != spatialize {
            self.commit_channel(&name, |c| c.spatialize = routing);
        }

        // A direct channel with a resolved speaker shows that speaker's own
        // coordinate mode; otherwise the editor's own choice decides. Direct,
        // the coordinates are the speaker's and nothing about them is editable.
        let speaker = direct.clone().flatten().and_then(|(index, _)| {
            let live = self.host.read();
            live.selected_speakers().get(index).cloned()
        });
        // A virtual channel is placed by hand in manual mode only; in the
        // sphere and room modes its family's model places it, and the
        // coordinates below are what that model gives.
        let manual = mode == PlacementMode::Manual;
        if channel.spatialize && !manual {
            widgets::note(
                ui,
                &t("placement.positionsFollow").replace("{mode}", t(mode.i18n_key())),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button(t("placement.editManually")).clicked() {
                    engine::switch_placement_to_manual(&self.host, family);
                }
            });
        }
        let editable = channel.spatialize && manual;
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

        // Coordinates: the mode in the bar, the table and the 3D edit toggle
        // in the inset, as the speaker editor lays them out.
        let mut chosen = mode;
        Group::new(super::speaker_editor::coordinates_title())
            .help("help.speaker.position")
            .actions(|ui| {
                ui.add_enabled_ui(editable, |ui| {
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
                });
            })
            .show(ui, |ui| {
                // The speaker editor's "3D Edit" toggle, which the web had on
                // this editor too: a virtual channel is dragged with the same
                // gizmo. A direct channel sits where its speaker is and has
                // nothing to arm.
                let edit_mode = match mode {
                    CoordMode::Cartesian => EditMode::Cartesian,
                    CoordMode::Polar => EditMode::Polar,
                };
                self.coord_table_with_gizmo(ui, edit_mode, !editable, |this, ui| match mode {
                    CoordMode::Cartesian => {
                        this.channel_cartesian_table(ui, &name, position, &room, scale_m, editable)
                    }
                    CoordMode::Polar => {
                        this.channel_polar_table(ui, &name, position, &room, scale_m, editable)
                    }
                });
            });
        if chosen != mode {
            self.channel_coord_mode = chosen;
        }
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
        let adm = position.unwrap_or([0.0; 3]);
        let meters = adm_to_meters(room, adm, scale_m);
        // A direct channel with no matching speaker has nothing to show, so
        // its cells are blanked rather than filled with a made-up zero.
        let cell = |value: f64, speed: f64, decimals: usize| {
            if position.is_some() {
                CoordCell::field(value as f32, speed, decimals)
            } else {
                CoordCell::Blank
            }
        };
        let mut rows = [
            CoordRow {
                label: t("speaker.normalizedCoords"),
                help: None,
                cells: adm.map(|v| cell(v, 0.001, 3).in_range(-1.0..=1.0)),
            },
            CoordRow {
                label: t("speaker.metersCoords"),
                help: Some("help.speaker.positionMeters".into()),
                cells: meters.map(|v| cell(v, 0.01, 2)),
            },
        ];
        let edited = ui
            .add_enabled_ui(editable, |ui| {
                widgets::coord_table(ui, ("channel-cartesian", name), ["X", "Y", "Z"], &mut rows)
            })
            .inner;
        let next = match edited {
            Some((0, axis, value)) => {
                let mut next = adm;
                next[axis] = f64::from(value).clamp(-1.0, 1.0);
                next
            }
            // The edited axis in metres, the other two canonical: converting
            // all three back would round-trip values the user did not touch.
            Some((_, axis, value)) => {
                let mut next_m = meters;
                next_m[axis] = f64::from(value);
                meters_to_adm(room, next_m, scale_m)
            }
            None => return,
        };
        self.set_channel_cartesian(name, next);
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
        let adm = position.unwrap_or([0.0; 3]);
        let (az, el, dist) = adm_to_polar(room, adm);
        let meters = adm_to_meters(room, adm, scale_m);
        let dist_m = (meters[0] * meters[0] + meters[1] * meters[1] + meters[2] * meters[2]).sqrt();
        let cell = |value: f64, speed: f64, decimals: usize| {
            if position.is_some() {
                CoordCell::field(value as f32, speed, decimals)
            } else {
                CoordCell::Blank
            }
        };
        let mut rows = [
            CoordRow {
                label: t("speaker.normalizedCoords"),
                help: None,
                cells: [
                    cell(az, 0.1, 1),
                    cell(el, 0.1, 1),
                    cell(dist, 0.001, 3).in_range(0.01..=f32::MAX),
                ],
            },
            CoordRow {
                label: t("speaker.metersCoords"),
                help: Some("help.speaker.positionMeters".into()),
                cells: [
                    CoordCell::Empty,
                    CoordCell::Empty,
                    cell(dist_m, 0.01, 2).in_range(0.01..=f32::MAX),
                ],
            },
        ];
        let edited = ui
            .add_enabled_ui(editable, |ui| {
                widgets::coord_table(
                    ui,
                    ("channel-polar", name),
                    ["Az°", "El°", "Dist"],
                    &mut rows,
                )
            })
            .inner;
        let (az, el, dist) = match edited {
            Some((0, 0, azimuth)) => (f64::from(azimuth), el, dist),
            Some((0, 1, elevation)) => (az, f64::from(elevation), dist),
            Some((0, _, distance)) => (az, el, f64::from(distance)),
            Some((_, _, metres)) => (az, el, f64::from(metres) / scale_m),
            None => return,
        };
        self.set_channel_polar(name, az, el, dist);
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
        let family = live.editing_family;
        let mut channels = effective_channels_for(&live.channels, &live.app, family);
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
        engine::preview_placement_layout(&self.host, family, payload);
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

    /// Mutate one channel, then push the family's whole layout: the renderer
    /// takes a layout, not a diff. Optimistic, like the web — the renderer
    /// echoes `placement` back in the snapshot.
    fn commit_channel(&mut self, name: &str, mutate: impl FnOnce(&mut Channel)) {
        let (family, payload) = {
            let live = self.host.read();
            let family = live.editing_family;
            let mut channels = effective_channels_for(&live.channels, &live.app, family);
            let Some(target) = channels.iter_mut().find(|c| c.name == name) else {
                return;
            };
            mutate(target);
            (family, build_layout_payload(&live.app, &channels))
        };
        engine::set_placement_layout(&self.host, family, payload);
    }
}
