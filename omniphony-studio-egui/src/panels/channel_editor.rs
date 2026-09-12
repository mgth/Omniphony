//! The channel editor (`#channelEditSection`, `controls/virtual-bed.js`,
//! `listeners/channel-editor-listeners.js`).
//!
//! Every input channel of a channel-based stream is either routed straight to
//! its speaker (LFE → sub) or virtualised as an object at a position. The whole
//! set is a speaker layout of its own — one entry per channel label — pushed
//! live to the renderer as `control_virtual_bed`.
//!
//! Editing reuses the speaker editor's mechanic: the channels appear in the
//! objects list, and selecting one opens this editor under it. With no stream
//! playing, Studio materialises one scene marker per channel so the bed stays
//! visible and editable at rest; the live stream's objects take over as soon as
//! frames arrive.

use std::collections::HashMap;

use egui::{RichText, Ui};

use crate::app::StudioSpike;
use crate::host::commands::engine;
use crate::i18n::t;
use crate::model::app_state::{AppState, RoomRatio, SourcePosition};
use crate::ui::{help, theme, widgets};

/// Editable fixed-channel set with its default ADM cartesian poses (X
/// left/right, Y rear/front, Z down/up; ear level Z = 0), used until the
/// renderer publishes its catalogue. LFE channels default to direct because
/// they cannot be VBAP-panned.
const FALLBACK_BED: &[(&str, f64, f64, f64, bool)] = &[
    ("L", -1.0, 1.0, 0.0, true),
    ("R", 1.0, 1.0, 0.0, true),
    ("C", 0.0, 1.0, 0.0, true),
    ("LFE", 0.0, 1.0, 0.0, false),
    ("Ls", -1.0, 0.0, 0.0, true),
    ("Rs", 1.0, 0.0, 0.0, true),
    ("Lb", -1.0, -1.0, 0.0, true),
    ("Rb", 1.0, -1.0, 0.0, true),
    ("TFL", -1.0, 1.0, 1.0, true),
    ("TFR", 1.0, 1.0, 1.0, true),
    ("TBL", -1.0, -1.0, 1.0, true),
    ("TBR", 1.0, -1.0, 1.0, true),
    ("Lsc", -0.5, 1.0, 0.0, true),
    ("Rsc", 0.5, 1.0, 0.0, true),
    ("Cb", 0.0, -1.0, 0.0, true),
    ("Lsd", -1.0, -0.5, 0.0, true),
    ("Rsd", 1.0, -0.5, 0.0, true),
    ("Lw", -1.0, 0.5, 0.0, true),
    ("Rw", 1.0, 0.5, 0.0, true),
    ("LFE2", 0.0, 1.0, 0.0, false),
    ("TSL", -1.0, 0.0, 1.0, true),
    ("TSR", 1.0, 0.0, 1.0, true),
    ("TC", 0.0, 0.0, 1.0, true),
    ("TFC", 0.0, 1.0, 1.0, true),
];

/// Normalise a channel name exactly like `bridge_api::labels`: drop whitespace,
/// `_` and `-`, then uppercase. "Top Front Left", "top_front-left" and "TFL"
/// all become "TOPFRONTLEFT".
pub fn normalize_channel_name(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_whitespace() && *c != '_' && *c != '-')
        .flat_map(char::to_uppercase)
        .collect()
}

/// One channel of the catalogue, with the pose it defaults to.
#[derive(Clone, Debug)]
pub struct Base {
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub spatialize: bool,
}

/// The renderer-published fixed-channel catalogue, digested once.
///
/// The renderer publishes it at start-up and then keeps it static, so this is
/// rebuilt only when the array actually changes: the alias lookup runs once per
/// list row per frame, and rebuilding a hash map to answer it would be churn.
#[derive(Default)]
pub struct ChannelCatalog {
    /// What the digest was built from: entry count and first label.
    key: (usize, String),
    /// Normalised spelling → canonical label, from each entry's aliases.
    by_spelling: HashMap<String, String>,
    /// Canonical order, the common 7.1.4 set first.
    order: Vec<String>,
    /// The published bases, or the fallback bed when nothing is published.
    bases: Vec<Base>,
}

impl ChannelCatalog {
    /// Canonical channel key for any spelling the renderer accepts.
    pub fn canonical(&self, app: &AppState, name: &str) -> Option<String> {
        let norm = normalize_channel_name(name);
        if norm.is_empty() {
            return None;
        }
        if let Some(label) = self.by_spelling.get(&norm) {
            return Some(label.clone());
        }
        // A label the renderer knows about but left out of its alias table:
        // match the published lists directly before giving up.
        let published = app
            .live_options
            .fixed_channel_processing
            .as_ref()
            .and_then(|p| p.get("labels"))
            .and_then(|l| l.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str())
            .chain(
                bed_speakers(app)
                    .into_iter()
                    .flatten()
                    .filter_map(|s| s.get("name").and_then(|v| v.as_str())),
            )
            .find(|label| normalize_channel_name(label) == norm);
        if let Some(label) = published {
            return Some(label.trim().to_owned());
        }
        // Offline last resort: the canonical fallback names still resolve,
        // their aliases wait for the renderer.
        FALLBACK_BED
            .iter()
            .find(|(name, ..)| normalize_channel_name(name) == norm)
            .map(|(name, ..)| (*name).to_owned())
    }

    /// Rank in the canonical order, for sorting the objects list by the classic
    /// channel order instead of alphabetically.
    pub fn rank(&self, app: &AppState, name: &str) -> Option<usize> {
        let key = self.canonical(app, name)?;
        self.order.iter().position(|label| *label == key)
    }
}

fn bed_speakers(app: &AppState) -> Option<&Vec<serde_json::Value>> {
    app.live_options
        .virtual_bed
        .as_ref()?
        .get("speakers")?
        .as_array()
}

// ---------------------------------------------------------------------------
// The editable channel set
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum CoordMode {
    Cartesian,
    Polar,
}

impl CoordMode {
    fn as_str(self) -> &'static str {
        match self {
            CoordMode::Cartesian => "cartesian",
            CoordMode::Polar => "polar",
        }
    }
}

/// One channel as the editor holds it. Both representations are kept in step on
/// every edit, so changing one cartesian axis cannot drift the others through a
/// polar round-trip.
#[derive(Clone, Debug)]
pub struct Channel {
    pub name: String,
    pub coord_mode: CoordMode,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub azimuth: f64,
    pub elevation: f64,
    pub distance: f64,
    pub spatialize: bool,
    pub gain_db: f64,
}

/// Polar → ADM normalised cartesian, exactly like the speaker editor: the room
/// warp is inverted and the result clamped. "Norm" is the ADM position, not a
/// raw axis swizzle.
fn polar_to_adm(room: &RoomRatio, azimuth: f64, elevation: f64, distance: f64) -> [f64; 3] {
    use omniphony_geometry::f64 as g;
    let (x, y, z) = g::from_spherical(azimuth, elevation, distance);
    g::inverse_room_scaled_position(
        [x, y, z],
        [room.width, room.length, room.height],
        room.rear,
        room.lower,
        room.center_blend,
    )
}

/// ADM normalised cartesian → polar, through the same scene round-trip: the
/// room warp is re-applied, then the spherical form derived.
fn adm_to_polar(room: &RoomRatio, adm: [f64; 3]) -> (f64, f64, f64) {
    use omniphony_geometry::f64 as g;
    let scaled = g::room_scaled_position(
        adm,
        [room.width, room.length, room.height],
        room.rear,
        room.lower,
        room.center_blend,
    );
    let (az, el, dist) = g::to_spherical(scaled[0], scaled[1], scaled[2]);
    (az, el, dist.max(0.01))
}

/// Normalised ADM → Omniphony-axis metres, honouring the room geometry.
fn adm_to_meters(room: &RoomRatio, adm: [f64; 3], scale_m: f64) -> [f64; 3] {
    use omniphony_geometry::f64 as g;
    let scaled = g::room_scaled_position(
        adm,
        [room.width, room.length, room.height],
        room.rear,
        room.lower,
        room.center_blend,
    );
    [
        scaled[0] * scale_m,
        scaled[1] * scale_m,
        scaled[2] * scale_m,
    ]
}

/// The inverse of [`adm_to_meters`].
fn meters_to_adm(room: &RoomRatio, meters: [f64; 3], scale_m: f64) -> [f64; 3] {
    use omniphony_geometry::f64 as g;
    let scale = scale_m.max(0.001);
    g::inverse_room_scaled_position(
        [meters[0] / scale, meters[1] / scale, meters[2] / scale],
        [room.width, room.length, room.height],
        room.rear,
        room.lower,
        room.center_blend,
    )
}

fn default_entry(room: &RoomRatio, base: &Base) -> Channel {
    let (azimuth, elevation, distance) = adm_to_polar(room, [base.x, base.y, base.z]);
    Channel {
        name: base.name.clone(),
        coord_mode: CoordMode::Cartesian,
        x: base.x,
        y: base.y,
        z: base.z,
        azimuth,
        elevation,
        distance,
        spatialize: base.spatialize,
        gain_db: 0.0,
    }
}

/// Read a configured bed entry as a model entry, falling back to the canonical
/// default when it cannot be parsed.
fn read_entry(room: &RoomRatio, base: &Base, entry: Option<&serde_json::Value>) -> Channel {
    let Some(entry) = entry else {
        return default_entry(room, base);
    };
    let number = |key: &str| entry.get(key).and_then(serde_json::Value::as_f64);
    let gain_db = number("gain_db").map_or(0.0, |g| (g * 10.0).round() / 10.0);
    let spatialize = entry
        .get("spatialize")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let cartesian = entry
        .get("coord_mode")
        .and_then(|m| m.as_str())
        .is_some_and(|m| m.eq_ignore_ascii_case("cartesian"));
    if cartesian && let Some(x) = number("x") {
        let adm = [x, number("y").unwrap_or(0.0), number("z").unwrap_or(0.0)];
        let (azimuth, elevation, distance) = adm_to_polar(room, adm);
        return Channel {
            name: base.name.clone(),
            coord_mode: CoordMode::Cartesian,
            x: adm[0],
            y: adm[1],
            z: adm[2],
            azimuth,
            elevation,
            distance,
            spatialize,
            gain_db,
        };
    }
    if let Some(azimuth) = number("azimuth") {
        let elevation = number("elevation").unwrap_or(0.0);
        let distance = number("distance").filter(|d| *d > 0.0).unwrap_or(1.0);
        let adm = polar_to_adm(room, azimuth, elevation, distance);
        return Channel {
            name: base.name.clone(),
            coord_mode: CoordMode::Polar,
            x: adm[0],
            y: adm[1],
            z: adm[2],
            azimuth,
            elevation,
            distance,
            spatialize,
            gain_db,
        };
    }
    default_entry(room, base)
}

/// The full editable set: the catalogue's defaults, overridden by whatever the
/// live bed configures, plus any channel the bed or the stream mentions that the
/// catalogue does not.
pub fn effective_channels(catalog: &ChannelCatalog, app: &AppState) -> Vec<Channel> {
    let room = &app.room_ratio;
    let mut bases = catalog.bases.clone();
    let add_base = |name: &str, source: Option<&serde_json::Value>, bases: &mut Vec<Base>| {
        let key = catalog
            .canonical(app, name)
            .unwrap_or_else(|| name.trim().to_owned());
        if key.is_empty()
            || bases
                .iter()
                .any(|b| catalog.canonical(app, &b.name).as_deref() == Some(key.as_str()))
        {
            return;
        }
        let number = |k: &str| {
            source
                .and_then(|s| s.get(k))
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0)
        };
        bases.push(Base {
            name: key,
            x: number("x"),
            y: number("y"),
            z: number("z"),
            spatialize: source
                .and_then(|s| s.get("spatialize"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true),
        });
    };
    if let Some(speakers) = bed_speakers(app) {
        for entry in speakers {
            if let Some(name) = entry.get("name").and_then(|v| v.as_str()) {
                add_base(name, Some(entry), &mut bases);
            }
        }
    }
    if let Some(labels) = app
        .live_options
        .fixed_channel_processing
        .as_ref()
        .and_then(|p| p.get("labels"))
        .and_then(|l| l.as_array())
    {
        for label in labels.iter().filter_map(|v| v.as_str()) {
            add_base(label, None, &mut bases);
        }
    }
    let configured = bed_speakers(app);
    bases
        .iter()
        .map(|base| {
            let key = catalog
                .canonical(app, &base.name)
                .unwrap_or_else(|| base.name.clone());
            let match_entry = configured.and_then(|speakers| {
                speakers.iter().find(|s| {
                    s.get("name")
                        .and_then(|v| v.as_str())
                        .and_then(|n| catalog.canonical(app, n))
                        .as_deref()
                        == Some(key.as_str())
                })
            });
            read_entry(room, base, match_entry)
        })
        .collect()
}

/// The wire payload: each channel ships the block matching its own coord mode,
/// exactly like the speaker editor. Forcing polar here would replace a cartesian
/// edit with a Studio-side conversion the renderer does not make, and the
/// channel would land at the polar-derived spot instead.
pub fn build_layout_payload(app: &AppState, channels: &[Channel]) -> serde_json::Value {
    let radius = app
        .live_options
        .virtual_bed
        .as_ref()
        .and_then(|b| b.get("radius_m"))
        .and_then(serde_json::Value::as_f64)
        .filter(|r| *r > 0.0)
        .unwrap_or(1.0);
    let speakers: Vec<serde_json::Value> = channels
        .iter()
        .map(|c| {
            let mut entry = serde_json::json!({
                "name": c.name,
                "coord_mode": c.coord_mode.as_str(),
                "spatialize": c.spatialize,
            });
            let map = entry.as_object_mut().expect("object");
            match c.coord_mode {
                CoordMode::Cartesian => {
                    map.insert("x".into(), c.x.clamp(-1.0, 1.0).into());
                    map.insert("y".into(), c.y.clamp(-1.0, 1.0).into());
                    map.insert("z".into(), c.z.clamp(-1.0, 1.0).into());
                }
                CoordMode::Polar => {
                    map.insert("azimuth".into(), c.azimuth.into());
                    map.insert("elevation".into(), c.elevation.into());
                    map.insert("distance".into(), c.distance.max(0.01).into());
                }
            }
            let gain_db = (c.gain_db * 10.0).round() / 10.0;
            if gain_db != 0.0 {
                map.insert("gain_db".into(), gain_db.into());
            }
            entry
        })
        .collect();
    serde_json::json!({ "radius_m": radius, "speakers": speakers })
}

// ---------------------------------------------------------------------------
// The panel
// ---------------------------------------------------------------------------

impl StudioSpike {
    /// Rebuild the catalogue digest when the renderer's array has changed.
    pub(crate) fn refresh_channel_catalog(&mut self) {
        let key = {
            let live = self.live.lock().unwrap();
            let entries = live
                .app
                .live_options
                .fixed_channel_catalog
                .as_ref()
                .and_then(|c| c.as_array());
            (
                entries.map_or(0, Vec::len),
                entries
                    .and_then(|e| e.first())
                    .and_then(|e| e.get("label"))
                    .and_then(|l| l.as_str())
                    .unwrap_or_default()
                    .to_owned(),
            )
        };
        if self.channel_catalog.key == key && (key.0 > 0 || !self.channel_catalog.bases.is_empty())
        {
            return;
        }
        let live = self.live.lock().unwrap();
        let entries = live
            .app
            .live_options
            .fixed_channel_catalog
            .as_ref()
            .and_then(|c| c.as_array())
            .cloned()
            .unwrap_or_default();
        let mut by_spelling = HashMap::new();
        let mut order = Vec::new();
        let mut bases = Vec::new();
        for entry in &entries {
            let Some(label) = entry
                .get("label")
                .and_then(|l| l.as_str())
                .map(str::trim)
                .filter(|l| !l.is_empty())
            else {
                continue;
            };
            by_spelling.insert(normalize_channel_name(label), label.to_owned());
            for alias in entry
                .get("aliases")
                .and_then(|a| a.as_array())
                .into_iter()
                .flatten()
                .filter_map(|a| a.as_str())
            {
                let norm = normalize_channel_name(alias);
                if !norm.is_empty() {
                    by_spelling.insert(norm, label.to_owned());
                }
            }
            order.push(label.to_owned());
            let number = |k: &str| {
                entry
                    .get(k)
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.0)
            };
            bases.push(Base {
                name: label.to_owned(),
                x: number("x"),
                y: number("y"),
                z: number("z"),
                spatialize: entry
                    .get("spatialize")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true),
            });
        }
        if bases.is_empty() {
            bases = FALLBACK_BED
                .iter()
                .map(|(name, x, y, z, spatialize)| Base {
                    name: (*name).to_owned(),
                    x: *x,
                    y: *y,
                    z: *z,
                    spatialize: *spatialize,
                })
                .collect();
            order = bases.iter().map(|b| b.name.clone()).collect();
        }
        self.channel_catalog = ChannelCatalog {
            key,
            by_spelling,
            order,
            bases,
        };
    }

    /// The canonical channel the selection names, if it names one.
    pub(crate) fn selected_channel(&self) -> Option<String> {
        let id = self.selection.object.as_deref()?;
        let live = self.live.lock().unwrap();
        let name = live
            .app
            .sources
            .get(id)
            .and_then(|s| s.name.clone())
            .unwrap_or_else(|| id.to_owned());
        self.channel_catalog.canonical(&live.app, &name)
    }

    pub(crate) fn channel_editor(&mut self, ui: &mut Ui) {
        let Some(name) = self.selected_channel() else {
            return;
        };
        let (channel, room, scale_m, direct) = {
            let live = self.live.lock().unwrap();
            let channels = effective_channels(&self.channel_catalog, &live.app);
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
                    egui::Slider::new(&mut gain, -24.0..=12.0)
                        .show_value(false)
                        .step_by(0.1),
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
            let live = self.live.lock().unwrap();
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
            speakers.iter().position(|s| {
                self.channel_catalog.canonical(&live.app, &s.id).as_deref() == Some(name)
            })
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
        let room = self.live.lock().unwrap().app.room_ratio.clone();
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
        let room = self.live.lock().unwrap().app.room_ratio.clone();
        let distance = distance.max(0.01);
        let adm = polar_to_adm(&room, azimuth, elevation, distance);
        let mut live = self.live.lock().unwrap();
        let mut channels = effective_channels(&self.channel_catalog, &live.app);
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
        let room = self.live.lock().unwrap().app.room_ratio.clone();
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
            let mut live = self.live.lock().unwrap();
            let mut channels = effective_channels(&self.channel_catalog, &live.app);
            let Some(target) = channels.iter_mut().find(|c| c.name == name) else {
                return;
            };
            mutate(target);
            build_layout_payload(&live.app, &channels)
        };
        self.send_virtual_bed(payload);
    }

    /// Reset every channel to its catalogue corner, in cartesian mode.
    ///
    /// Sending an empty string would hand the renderer its built-in *polar*
    /// poses, which the editor would then display as cartesian corners: the
    /// polar form would change while the cartesian fields stayed stale even
    /// though the mode read "cartesian". Pushing the explicit cartesian bed
    /// keeps the editor, the 3D view and the audio in agreement.
    pub(crate) fn reset_virtual_bed(&mut self) {
        let payload = {
            let mut live = self.live.lock().unwrap();
            let room = live.app.room_ratio.clone();
            let channels: Vec<Channel> = effective_channels(&self.channel_catalog, &live.app)
                .iter()
                .map(|channel| {
                    let base = self
                        .channel_catalog
                        .bases
                        .iter()
                        .find(|b| b.name == channel.name)
                        .cloned()
                        .unwrap_or(Base {
                            name: channel.name.clone(),
                            x: 0.0,
                            y: 0.0,
                            z: 0.0,
                            spatialize: true,
                        });
                    default_entry(&room, &base)
                })
                .collect();
            build_layout_payload(&live.app, &channels)
        };
        self.send_virtual_bed(payload);
    }

    fn send_virtual_bed(&mut self, payload: serde_json::Value) {
        engine::set_virtual_bed(&self.host, payload);
        self.sync_virtual_bed_objects(true);
    }

    // -----------------------------------------------------------------------
    // Editor-only scene markers
    // -----------------------------------------------------------------------

    /// One marker per channel while no stream is playing, gone as soon as one
    /// is. These are Studio's own scene objects, not renderer metadata and not
    /// synthesised audio: they exist so the bed can be seen and edited at rest.
    pub(crate) fn sync_virtual_bed_objects(&mut self, force: bool) {
        // The live stream owns the scene while spatial frames are arriving,
        // which also covers the brief gap after a seek.
        let streaming = {
            let live = self.live.lock().unwrap();
            live.last_spatial_frame_at
                .is_some_and(|at| at.elapsed() < STREAM_IDLE)
        };
        if streaming {
            if !self.synthetic_bed_ids.is_empty() {
                let mut live = self.live.lock().unwrap();
                for id in self.synthetic_bed_ids.drain(..) {
                    live.app.sources.remove(&id);
                }
                self.synthetic_bed_signature = None;
            }
            return;
        }
        let channels = {
            let live = self.live.lock().unwrap();
            effective_channels(&self.channel_catalog, &live.app)
        };
        let signature = bed_signature(&channels);
        if !force
            && self.synthetic_bed_signature == Some(signature)
            && self.synthetic_bed_ids.len() == channels.len()
        {
            return;
        }
        self.synthetic_bed_signature = Some(signature);
        let mut live = self.live.lock().unwrap();
        // Live objects get no removal message when the engine simply stops
        // emitting them; left in place they would double the markers below.
        // The injected test source is neither live nor a bed channel — only its
        // own switch may remove it.
        let stale: Vec<String> = live
            .app
            .sources
            .keys()
            .filter(|id| {
                id.as_str() != crate::panels::object_test::OBJECT_TEST_SOURCE_ID
                    && !channels.iter().any(|c| &c.name == *id)
            })
            .cloned()
            .collect();
        for id in stale {
            live.app.sources.remove(&id);
        }
        self.synthetic_bed_ids.clear();
        for channel in &channels {
            let direct_speaker_index = (!channel.spatialize)
                .then(|| {
                    live.selected_speakers().iter().position(|s| {
                        self.channel_catalog.canonical(&live.app, &s.id).as_deref()
                            == Some(channel.name.as_str())
                    })
                })
                .flatten()
                .map(|i| i as u32);
            let source = SourcePosition {
                x: channel.x,
                y: channel.y,
                z: channel.z,
                coord_mode: Some(channel.coord_mode.as_str().to_owned()),
                azimuth_deg: Some(channel.azimuth),
                elevation_deg: Some(channel.elevation),
                distance_m: Some(channel.distance.max(0.01)),
                gain_db: Some(channel.gain_db.round() as i32),
                fixed: Some(true),
                label: Some(channel.name.clone()),
                name: Some(channel.name.clone()),
                direct_speaker_index,
                ..Default::default()
            };
            live.app.sources.insert(channel.name.clone(), source);
            self.synthetic_bed_ids.push(channel.name.clone());
        }
    }
}

/// How long after the last spatial frame the session still counts as playing.
const STREAM_IDLE: std::time::Duration = std::time::Duration::from_millis(800);

/// Cheap change detector for the marker set, so the sweep above runs only when
/// the bed actually moved.
fn bed_signature(channels: &[Channel]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for c in channels {
        c.name.hash(&mut hasher);
        c.spatialize.hash(&mut hasher);
        c.coord_mode.hash(&mut hasher);
        for v in [c.x, c.y, c.z, c.azimuth, c.elevation, c.distance, c.gain_db] {
            v.to_bits().hash(&mut hasher);
        }
    }
    hasher.finish()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn room() -> RoomRatio {
        RoomRatio {
            width: 1.0,
            length: 2.0,
            height: 1.0,
            rear: 1.0,
            lower: 0.5,
            center_blend: 0.5,
            scale_m: 1.5,
        }
    }

    #[test]
    fn spellings_normalise_the_way_the_renderers_label_table_does() {
        assert_eq!(normalize_channel_name("Top Front Left"), "TOPFRONTLEFT");
        assert_eq!(normalize_channel_name("top_front-left"), "TOPFRONTLEFT");
        assert_eq!(normalize_channel_name("tfl"), "TFL");
        assert_eq!(normalize_channel_name("  "), "");
    }

    #[test]
    fn the_catalogue_resolves_every_published_alias_to_its_canonical_label() {
        let app = AppState::new(Vec::new());
        let mut catalog = ChannelCatalog::default();
        catalog.by_spelling.insert("FL".to_owned(), "L".to_owned());
        catalog
            .by_spelling
            .insert("FRONTLEFT".to_owned(), "L".to_owned());
        catalog.order = vec!["L".to_owned(), "R".to_owned()];
        assert_eq!(catalog.canonical(&app, "front left").as_deref(), Some("L"));
        assert_eq!(catalog.canonical(&app, "FL").as_deref(), Some("L"));
        assert_eq!(catalog.rank(&app, "front-left"), Some(0));
        // Not a bed channel at all.
        assert_eq!(catalog.canonical(&app, "12"), None);
        // With no catalogue published, the canonical fallback names still
        // resolve; their aliases wait for the renderer.
        let empty = ChannelCatalog::default();
        assert_eq!(empty.canonical(&app, "lfe").as_deref(), Some("LFE"));
        assert_eq!(empty.canonical(&app, "FL"), None);
    }

    #[test]
    fn the_polar_and_cartesian_forms_are_each_others_inverse() {
        let r = room();
        for adm in [
            [0.0, 1.0, 0.0],
            [-1.0, 0.0, 0.0],
            [0.5, -0.25, 0.75],
            [0.0, 0.0, -1.0],
        ] {
            let (az, el, dist) = adm_to_polar(&r, adm);
            let back = polar_to_adm(&r, az, el, dist);
            for i in 0..3 {
                assert!((back[i] - adm[i]).abs() < 1e-6, "{adm:?} -> {back:?}");
            }
        }
    }

    #[test]
    fn metres_carry_the_room_warp_and_convert_back_unchanged() {
        let r = room();
        let adm = [0.5, -0.25, -0.5];
        let metres = adm_to_meters(&r, adm, r.scale_m);
        // The lower half is half as deep, so a normalised -0.5 in height is
        // -0.25 of the room's own unit before the metre scale.
        assert!((metres[2] - (-0.375)).abs() < 1e-9, "{metres:?}");
        let back = meters_to_adm(&r, metres, r.scale_m);
        for i in 0..3 {
            assert!((back[i] - adm[i]).abs() < 1e-6, "{adm:?} -> {back:?}");
        }
    }

    #[test]
    fn each_channel_ships_the_block_matching_its_own_coordinate_mode() {
        let app = AppState::new(Vec::new());
        let r = room();
        let mut cartesian = default_entry(
            &r,
            &Base {
                name: "L".to_owned(),
                x: -1.0,
                y: 1.0,
                z: 0.0,
                spatialize: true,
            },
        );
        cartesian.gain_db = 0.04; // rounds to 0.0 and is then omitted
        let mut polar = default_entry(
            &r,
            &Base {
                name: "C".to_owned(),
                x: 0.0,
                y: 1.0,
                z: 0.0,
                spatialize: true,
            },
        );
        polar.coord_mode = CoordMode::Polar;
        polar.gain_db = -3.25;
        let payload = build_layout_payload(&app, &[cartesian, polar]);
        let speakers = payload["speakers"].as_array().expect("speakers");
        assert_eq!(payload["radius_m"], 1.0);
        assert_eq!(speakers[0]["coord_mode"], "cartesian");
        assert_eq!(speakers[0]["x"], -1.0);
        assert!(speakers[0].get("azimuth").is_none());
        assert!(speakers[0].get("gain_db").is_none(), "0.0 dB is not sent");
        assert_eq!(speakers[1]["coord_mode"], "polar");
        assert!(speakers[1].get("x").is_none());
        assert_eq!(speakers[1]["gain_db"], -3.3);
    }

    #[test]
    fn a_configured_entry_wins_over_the_default_and_an_unreadable_one_does_not() {
        let r = room();
        let base = Base {
            name: "Ls".to_owned(),
            x: -1.0,
            y: 0.0,
            z: 0.0,
            spatialize: true,
        };
        let configured = serde_json::json!({
            "name": "Ls", "coord_mode": "cartesian",
            "x": -0.5, "y": -0.5, "z": 0.25, "spatialize": false, "gain_db": 2.25
        });
        let channel = read_entry(&r, &base, Some(&configured));
        assert_eq!(channel.coord_mode, CoordMode::Cartesian);
        assert_eq!(channel.x, -0.5);
        assert!(!channel.spatialize);
        assert_eq!(channel.gain_db, 2.3);
        // Neither block present: back to the catalogue corner.
        let broken = serde_json::json!({ "name": "Ls", "coord_mode": "cartesian" });
        let channel = read_entry(&r, &base, Some(&broken));
        assert_eq!(channel.x, -1.0);
        assert_eq!(channel.gain_db, 0.0);
        assert!(channel.spatialize);
    }
}
