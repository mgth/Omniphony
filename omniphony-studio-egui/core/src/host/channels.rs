//! The fixed-channel set: the catalogue the renderer publishes, and the bed
//! built from it.
//!
//! Every input channel of a channel-based stream is either routed straight to
//! its speaker (LFE → sub) or virtualised as an object at a position. The whole
//! set is a speaker layout of its own — one entry per channel label — pushed
//! live to the renderer as `control_virtual_bed`.
//!
//! None of this draws. The editor above it is a table of the same channels, and
//! the markers that stand in for them at rest are published by
//! `services::virtual_bed`; both read the bed from here so they cannot disagree
//! about what it is.

use std::collections::HashMap;

use crate::model::app_state::{AppState, RoomRatio};

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
