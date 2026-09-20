//! The fixed-channel set: the catalogue the renderer publishes, and the
//! per-family placement built from it.
//!
//! Every input channel of a channel-based stream is either routed straight to
//! its speaker (LFE → sub) or virtualised as an object at a position. Where a
//! virtualised channel goes is the placement policy of the stream's source
//! family (`renderer::placement`, docs/placement.md): a direction on the
//! listener's sphere, a corner of the room model, or the family's own entry
//! (manual). The family's entries are a speaker layout of their own — one
//! entry per channel label — pushed live to the renderer as
//! `control/placement/layout`; `spatialize` and `gain_db` apply in every
//! mode, the pose in manual mode only.
//!
//! None of this draws. The editor above it is a table of the same channels, and
//! the markers that stand in for them at rest are published by
//! `services::virtual_bed`; both read the bed from here so they cannot disagree
//! about what it is.

use std::collections::HashMap;

use crate::model::app_state::{AppState, RoomRatio};

/// Editable fixed-channel set with its default room corner (ADM cartesian: X
/// left/right, Y rear/front, Z down/up; ear level Z = 0) and its nominal
/// direction on the sphere (azimuth, elevation), used until the renderer
/// publishes its catalogue. LFE channels default to direct because they cannot
/// be VBAP-panned. The height tier's corner is on the wall above its floor
/// speaker, 30° up in a cube; its direction is 30° over the same speaker.
/// Mirrors the renderer's catalogue (`virtual_bed::fixed_channel_catalog_json`).
const FALLBACK_BED: &[(&str, f64, f64, f64, bool, (f64, f64))] = &[
    ("L", -1.0, 1.0, 0.0, true, (-30.0, 0.0)),
    ("R", 1.0, 1.0, 0.0, true, (30.0, 0.0)),
    ("C", 0.0, 1.0, 0.0, true, (0.0, 0.0)),
    ("LFE", 0.0, 1.0, 0.0, false, (0.0, 0.0)),
    ("Ls", -1.0, 0.0, 0.0, true, (-110.0, 0.0)),
    ("Rs", 1.0, 0.0, 0.0, true, (110.0, 0.0)),
    ("Lb", -1.0, -1.0, 0.0, true, (-135.0, 0.0)),
    ("Rb", 1.0, -1.0, 0.0, true, (135.0, 0.0)),
    ("TFL", -1.0, 1.0, 1.0, true, (-45.0, 45.0)),
    ("TFR", 1.0, 1.0, 1.0, true, (45.0, 45.0)),
    ("TBL", -1.0, -1.0, 1.0, true, (-135.0, 45.0)),
    ("TBR", 1.0, -1.0, 1.0, true, (135.0, 45.0)),
    ("Lsc", -0.5, 1.0, 0.0, true, (-15.0, 0.0)),
    ("Rsc", 0.5, 1.0, 0.0, true, (15.0, 0.0)),
    ("Cb", 0.0, -1.0, 0.0, true, (180.0, 0.0)),
    ("Lsd", -1.0, -0.5, 0.0, true, (-120.0, 0.0)),
    ("Rsd", 1.0, -0.5, 0.0, true, (120.0, 0.0)),
    ("Lw", -1.0, 0.5, 0.0, true, (-60.0, 0.0)),
    ("Rw", 1.0, 0.5, 0.0, true, (60.0, 0.0)),
    ("LFE2", 0.0, 1.0, 0.0, false, (0.0, 0.0)),
    ("TSL", -1.0, 0.0, 1.0, true, (-90.0, 45.0)),
    ("TSR", 1.0, 0.0, 1.0, true, (90.0, 45.0)),
    ("TC", 0.0, 0.0, 1.0, true, (0.0, 90.0)),
    ("TFC", 0.0, 1.0, 1.0, true, (0.0, 45.0)),
    ("Lh", -1.0, 1.0, 0.8165, true, (-30.0, 30.0)),
    ("Rh", 1.0, 1.0, 0.8165, true, (30.0, 30.0)),
    ("Ch", 0.0, 1.0, 0.5774, true, (0.0, 30.0)),
    ("Lhs", -1.0, 0.0, 0.5774, true, (-110.0, 30.0)),
    ("Rhs", 1.0, 0.0, 0.5774, true, (110.0, 30.0)),
];

// ---------------------------------------------------------------------------
// Families and modes
// ---------------------------------------------------------------------------

/// A source family, as the renderer's placement policy knows it
/// (`renderer::placement::SourceFamily`): the format a stream comes from.
/// `Generic` is the base the others inherit from, and what an undeclared
/// format gets.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Default)]
pub enum Family {
    #[default]
    Generic,
    Dolby,
    Dts,
    Auro,
    Pcm,
}

impl Family {
    pub const ALL: [Family; 5] = [Self::Generic, Self::Dolby, Self::Dts, Self::Auro, Self::Pcm];

    /// The wire name, as the renderer's controls and snapshot spell it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::Dolby => "dolby",
            Self::Dts => "dts",
            Self::Auro => "auro",
            Self::Pcm => "pcm",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        Self::ALL
            .into_iter()
            .find(|f| f.as_str().eq_ignore_ascii_case(s))
    }

    /// The Studio string naming the family.
    pub fn i18n_key(self) -> &'static str {
        match self {
            Self::Generic => "placement.family.generic",
            Self::Dolby => "placement.family.dolby",
            Self::Dts => "placement.family.dts",
            Self::Auro => "placement.family.auro",
            Self::Pcm => "placement.family.pcm",
        }
    }

    /// The renderer's built-in default when neither the family nor the
    /// generic one sets a mode: Auro-3D is a sphere, the rest a room.
    fn builtin_mode(self) -> PlacementMode {
        match self {
            Self::Auro => PlacementMode::Sphere,
            _ => PlacementMode::Room,
        }
    }
}

/// How a family's fixed channels are placed (`renderer::placement::PlacementMode`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum PlacementMode {
    Sphere,
    Room,
    Manual,
}

impl PlacementMode {
    pub const ALL: [PlacementMode; 3] = [Self::Sphere, Self::Room, Self::Manual];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sphere => "sphere",
            Self::Room => "room",
            Self::Manual => "manual",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        Self::ALL
            .into_iter()
            .find(|m| m.as_str().eq_ignore_ascii_case(s))
    }

    pub fn i18n_key(self) -> &'static str {
        match self {
            Self::Sphere => "placement.mode.sphere",
            Self::Room => "placement.mode.room",
            Self::Manual => "placement.mode.manual",
        }
    }
}

/// Whose entries a family uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LayoutSource {
    /// Its own.
    Own,
    /// The generic family's, inherited.
    Generic,
    /// Nobody's: the defaults (LFE direct, unity trims).
    None,
}

/// One family's placement, as the renderer reports it, with the inheritance
/// resolved here by the renderer's own rule — so an offline Studio, or one
/// whose edit has not been echoed yet, shows the same answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FamilyPlacement {
    /// The family's own mode, `None` when it inherits.
    pub own_mode: Option<PlacementMode>,
    pub effective_mode: PlacementMode,
    pub layout_source: LayoutSource,
}

fn placement_block<'a>(app: &'a AppState, family: Family) -> Option<&'a serde_json::Value> {
    app.live_options
        .placement
        .as_ref()?
        .get(family.as_str())
        .filter(|v| v.is_object())
}

fn own_mode(app: &AppState, family: Family) -> Option<PlacementMode> {
    placement_block(app, family)?
        .get("mode")
        .and_then(|m| m.as_str())
        .and_then(PlacementMode::parse)
}

fn own_speakers(app: &AppState, family: Family) -> Option<&Vec<serde_json::Value>> {
    placement_block(app, family)?
        .get("layout")?
        .get("speakers")?
        .as_array()
}

pub fn family_placement(app: &AppState, family: Family) -> FamilyPlacement {
    let own = own_mode(app, family);
    let effective_mode = own
        .or_else(|| own_mode(app, Family::Generic))
        .unwrap_or_else(|| family.builtin_mode());
    let layout_source = if own_speakers(app, family).is_some() {
        LayoutSource::Own
    } else if own_speakers(app, Family::Generic).is_some() || legacy_bed_speakers(app).is_some() {
        LayoutSource::Generic
    } else {
        LayoutSource::None
    };
    FamilyPlacement {
        own_mode: own,
        effective_mode,
        layout_source,
    }
}

/// The entries a family uses: its own, else the generic family's, else the
/// legacy single bed a renderer from before placement reports.
pub fn family_speakers(app: &AppState, family: Family) -> Option<&Vec<serde_json::Value>> {
    own_speakers(app, family)
        .or_else(|| own_speakers(app, Family::Generic))
        .or_else(|| legacy_bed_speakers(app))
}

fn legacy_bed_speakers(app: &AppState) -> Option<&Vec<serde_json::Value>> {
    app.live_options
        .virtual_bed
        .as_ref()?
        .get("speakers")?
        .as_array()
}

/// The family of the stream the renderer is rendering, if a fixed-channel
/// stream is playing (`fixedChannelProcessing.family`).
pub fn playing_family(app: &AppState) -> Option<Family> {
    let processing = app.live_options.fixed_channel_processing.as_ref()?;
    let stream = processing.get("stream").and_then(|s| s.as_str())?;
    if stream == "idle" {
        return None;
    }
    processing
        .get("family")
        .and_then(|f| f.as_str())
        .and_then(Family::parse)
}

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
    /// The nominal direction of the channel on the listener's sphere,
    /// `(azimuth, elevation)`: where sphere mode puts it when the format
    /// declares nothing. `None` for a channel the catalogue does not know.
    pub sphere: Option<(f64, f64)>,
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
                Family::ALL
                    .iter()
                    .flat_map(|&family| own_speakers(app, family).into_iter().flatten())
                    .chain(legacy_bed_speakers(app).into_iter().flatten())
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

    /// The catalogue's own pose for a channel, if it publishes one.
    pub fn base(&self, name: &str) -> Option<&Base> {
        self.bases.iter().find(|b| b.name == name)
    }

    /// Rank in the canonical order, for sorting the objects list by the classic
    /// channel order instead of alphabetically.
    pub fn rank(&self, app: &AppState, name: &str) -> Option<usize> {
        let key = self.canonical(app, name)?;
        self.order.iter().position(|label| *label == key)
    }
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
    pub fn as_str(self) -> &'static str {
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
pub fn polar_to_adm(room: &RoomRatio, azimuth: f64, elevation: f64, distance: f64) -> [f64; 3] {
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
pub fn adm_to_polar(room: &RoomRatio, adm: [f64; 3]) -> (f64, f64, f64) {
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
pub fn adm_to_meters(room: &RoomRatio, adm: [f64; 3], scale_m: f64) -> [f64; 3] {
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
pub fn meters_to_adm(room: &RoomRatio, meters: [f64; 3], scale_m: f64) -> [f64; 3] {
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

/// The room model's channel: the catalogue corner, cartesian.
pub fn default_entry(room: &RoomRatio, base: &Base) -> Channel {
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

/// The sphere model's channel: the nominal direction, polar, at unit
/// distance — or the room corner when the catalogue gives no direction.
fn sphere_entry(room: &RoomRatio, base: &Base) -> Channel {
    let Some((azimuth, elevation)) = base.sphere else {
        return default_entry(room, base);
    };
    let [x, y, z] = polar_to_adm(room, azimuth, elevation, 1.0);
    Channel {
        name: base.name.clone(),
        coord_mode: CoordMode::Polar,
        x,
        y,
        z,
        azimuth,
        elevation,
        distance: 1.0,
        spatialize: base.spatialize,
        gain_db: 0.0,
    }
}

/// The channel as the family's `mode` renders it: manual reads the entry's
/// pose ([`read_entry`]); room and sphere take the model's pose and only the
/// entry's routing and trim.
fn channel_in_mode(
    room: &RoomRatio,
    base: &Base,
    entry: Option<&serde_json::Value>,
    mode: PlacementMode,
) -> Channel {
    match mode {
        PlacementMode::Manual => read_entry(room, base, entry),
        PlacementMode::Room | PlacementMode::Sphere => {
            let mut channel = match mode {
                PlacementMode::Sphere => sphere_entry(room, base),
                _ => default_entry(room, base),
            };
            if let Some(entry) = entry {
                if let Some(spatialize) =
                    entry.get("spatialize").and_then(serde_json::Value::as_bool)
                {
                    channel.spatialize = spatialize;
                }
                if let Some(gain) = entry.get("gain_db").and_then(serde_json::Value::as_f64) {
                    channel.gain_db = (gain * 10.0).round() / 10.0;
                }
            }
            channel
        }
    }
}

/// Read a configured entry as a manual-mode channel, falling back to the room
/// corner when it cannot be parsed.
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

/// The full editable set of one family, as its effective mode renders it: the
/// catalogue's channels, with the family's entries applied (routing and trim
/// in every mode, the pose in manual mode), plus any channel the entries or
/// the stream mention that the catalogue does not.
pub fn effective_channels_for(
    catalog: &ChannelCatalog,
    app: &AppState,
    family: Family,
) -> Vec<Channel> {
    let room = &app.room_ratio;
    let mode = family_placement(app, family).effective_mode;
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
            sphere: None,
        });
    };
    if let Some(speakers) = family_speakers(app, family) {
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
    let configured = family_speakers(app, family);
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
            channel_in_mode(room, base, match_entry, mode)
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
impl ChannelCatalog {
    /// Rebuild the digest when the renderer's array has changed.
    ///
    /// The renderer publishes the catalogue at start-up and then keeps it
    /// static, so this returns early on the common pass: the alias lookup runs
    /// once per list row per frame, and rebuilding a hash map to answer it
    /// would be churn.
    pub fn refresh(&mut self, app: &AppState) {
        let entries = app
            .live_options
            .fixed_channel_catalog
            .as_ref()
            .and_then(|c| c.as_array());
        let key = (
            entries.map_or(0, Vec::len),
            entries
                .and_then(|e| e.first())
                .and_then(|e| e.get("label"))
                .and_then(|l| l.as_str())
                .unwrap_or_default()
                .to_owned(),
        );
        if self.key == key && (key.0 > 0 || !self.bases.is_empty()) {
            return;
        }
        let entries = entries.cloned().unwrap_or_default();
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
            let sphere = entry
                .get("azimuth")
                .and_then(serde_json::Value::as_f64)
                .map(|azimuth| {
                    let elevation = entry
                        .get("elevation")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(0.0);
                    (azimuth, elevation)
                });
            bases.push(Base {
                name: label.to_owned(),
                x: number("x"),
                y: number("y"),
                z: number("z"),
                spatialize: entry
                    .get("spatialize")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true),
                sphere,
            });
        }
        if bases.is_empty() {
            bases = FALLBACK_BED
                .iter()
                .map(|(name, x, y, z, spatialize, sphere)| Base {
                    name: (*name).to_owned(),
                    x: *x,
                    y: *y,
                    z: *z,
                    spatialize: *spatialize,
                    sphere: Some(*sphere),
                })
                .collect();
            order = bases.iter().map(|b| b.name.clone()).collect();
        }
        *self = ChannelCatalog {
            key,
            by_spelling,
            order,
            bases,
        };
    }
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
                sphere: None,
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
                sphere: None,
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
            sphere: None,
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

    fn app_with_placement(placement: serde_json::Value) -> AppState {
        let mut app = AppState::new(Vec::new());
        app.live_options.placement = Some(placement);
        app
    }

    #[test]
    fn families_inherit_the_generic_mode_and_entries() {
        let app = app_with_placement(serde_json::json!({
            "generic": {
                "mode": "manual",
                "layout": { "speakers": [
                    { "name": "Ls", "coord_mode": "polar", "azimuth": -110.0, "elevation": 0.0, "distance": 1.0 }
                ] }
            },
            "auro": { "mode": "sphere" }
        }));
        let dts = family_placement(&app, Family::Dts);
        assert_eq!(dts.own_mode, None);
        assert_eq!(dts.effective_mode, PlacementMode::Manual);
        assert_eq!(dts.layout_source, LayoutSource::Generic);
        let auro = family_placement(&app, Family::Auro);
        assert_eq!(auro.own_mode, Some(PlacementMode::Sphere));
        assert_eq!(auro.effective_mode, PlacementMode::Sphere);

        let mut catalog = ChannelCatalog::default();
        catalog.refresh(&app);
        let ls = effective_channels_for(&catalog, &app, Family::Dts)
            .into_iter()
            .find(|c| c.name == "Ls")
            .expect("Ls");
        assert_eq!(ls.coord_mode, CoordMode::Polar);
        assert_eq!(ls.azimuth, -110.0);
    }

    #[test]
    fn room_and_sphere_modes_take_the_model_pose_and_only_the_entry_routing() {
        let entries = serde_json::json!({ "speakers": [
            { "name": "LFE", "coord_mode": "cartesian", "x": 0.0, "y": 1.0, "z": 0.0, "spatialize": false, "gain_db": -3.0 },
            { "name": "Ls", "coord_mode": "polar", "azimuth": -135.0, "elevation": 0.0, "distance": 1.0 }
        ] });
        // No mode anywhere: the built-in default, room.
        let app = app_with_placement(serde_json::json!({ "generic": { "layout": entries } }));
        let mut catalog = ChannelCatalog::default();
        catalog.refresh(&app);
        assert_eq!(
            family_placement(&app, Family::Dolby).effective_mode,
            PlacementMode::Room
        );
        let channels = effective_channels_for(&catalog, &app, Family::Dolby);
        let ls = channels.iter().find(|c| c.name == "Ls").expect("Ls");
        assert_eq!(ls.coord_mode, CoordMode::Cartesian);
        assert_eq!(
            (ls.x, ls.y, ls.z),
            (-1.0, 0.0, 0.0),
            "the corner, not the entry"
        );
        let lfe = channels.iter().find(|c| c.name == "LFE").expect("LFE");
        assert!(!lfe.spatialize, "routing comes from the entry");
        assert_eq!(lfe.gain_db, -3.0, "so does the trim");

        // Sphere: the nominal direction, polar, entry pose ignored.
        let app = app_with_placement(serde_json::json!({
            "generic": { "layout": entries },
            "dolby": { "mode": "sphere" }
        }));
        let channels = effective_channels_for(&catalog, &app, Family::Dolby);
        let ls = channels.iter().find(|c| c.name == "Ls").expect("Ls");
        assert_eq!(ls.coord_mode, CoordMode::Polar);
        assert_eq!((ls.azimuth, ls.elevation), (-110.0, 0.0));
        assert!(
            ls.x < -0.9 && ls.y < 0.0,
            "left and behind: {} {}",
            ls.x,
            ls.y
        );
        assert_eq!(
            family_placement(&app, Family::Dolby).layout_source,
            LayoutSource::Generic
        );
    }

    #[test]
    fn the_playing_family_comes_from_the_processing_facts() {
        let mut app = AppState::new(Vec::new());
        assert_eq!(playing_family(&app), None);
        app.live_options.fixed_channel_processing =
            Some(serde_json::json!({ "stream": "fixed", "family": "auro" }));
        assert_eq!(playing_family(&app), Some(Family::Auro));
        app.live_options.fixed_channel_processing =
            Some(serde_json::json!({ "stream": "idle", "family": "auro" }));
        assert_eq!(playing_family(&app), None);
        app.live_options.fixed_channel_processing =
            Some(serde_json::json!({ "stream": "fixed", "family": "mpeg-h" }));
        assert_eq!(playing_family(&app), None, "an unknown family is not one");
    }

    #[test]
    fn a_legacy_renderer_bed_still_feeds_every_family() {
        let mut app = AppState::new(Vec::new());
        app.live_options.virtual_bed = Some(serde_json::json!({ "speakers": [
            { "name": "LFE", "coord_mode": "cartesian", "x": 0.0, "y": 1.0, "z": 0.0, "spatialize": false, "gain_db": -6.0 }
        ] }));
        assert_eq!(
            family_placement(&app, Family::Dts).layout_source,
            LayoutSource::Generic
        );
        let mut catalog = ChannelCatalog::default();
        catalog.refresh(&app);
        let lfe = effective_channels_for(&catalog, &app, Family::Dts)
            .into_iter()
            .find(|c| c.name == "LFE")
            .expect("LFE");
        assert_eq!(lfe.gain_db, -6.0);
    }
}
