//! Speaker layout configuration parser
//!
//! This module handles parsing speaker layout YAML files for VBAP spatial rendering.
//! Speaker layouts define the physical positions of speakers in a listening environment
//! using azimuth and elevation angles.
//!
//! # YAML Format
//!
//! ```yaml
//! # 7.1.4 spatial audio layout
//! speakers:
//!   - name: "FL"
//!     azimuth: -30.0
//!     elevation: 0.0
//!   - name: "FR"
//!     azimuth: 30.0
//!     elevation: 0.0
//!   # ... more speakers
//! ```
//!
//! # Coordinate System
//!
//! - **Azimuth**: -180° to +180° (0° = front, -90° = left, 90° = right, ±180° = rear)
//! - **Elevation**: -90° to +90° (0° = horizontal, +90° = zenith, -90° = nadir)
//!
//! # Example
//!
//! ```ignore
//! use omniphony_renderer::speaker_layout::SpeakerLayout;
//!
//! let layout = SpeakerLayout::from_file("../layouts/7.1.4.yaml")?;
//! println!("Loaded {} speakers", layout.num_speakers());
//!
//! // Get positions for VBAP
//! let positions = layout.positions();
//! ```

use anyhow::{Context, Result};
use omniphony_geometry::f32 as geometry;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

/// Legacy 0-9 bed id for a channel label. The renderer no longer routes by
/// bed id — the only remaining consumer is the CLI file-export bed
/// conformance, which keeps the fixed 10-slot export order
/// (`docs/channel-object-contract.md`).
pub fn legacy_bed_id(label: bridge_api::RChannelLabel) -> Option<usize> {
    use bridge_api::RChannelLabel as Label;
    match label {
        Label::L => Some(0),
        Label::R => Some(1),
        Label::C => Some(2),
        Label::LFE => Some(3),
        Label::Ls => Some(4),
        Label::Rs => Some(5),
        Label::Lb => Some(6),
        Label::Rb => Some(7),
        Label::Tfl => Some(8),
        Label::Tfr => Some(9),
        // The height tier's front pair fills the same two height slots: the
        // export shape has room for one front-height pair, whichever tier
        // the presentation names it from.
        Label::Lh => Some(8),
        Label::Rh => Some(9),
        _ => None,
    }
}

/// A single speaker in the layout
#[derive(Debug, Clone, PartialEq)]
pub struct Speaker {
    /// Speaker name (e.g., "FL", "FR", "C", "TFL")
    pub name: String,

    /// Azimuth in degrees (-180 to +180)
    /// 0° = front, -90° = left, 90° = right, ±180° = rear
    pub azimuth: f32,

    /// Elevation in degrees (-90 to +90)
    /// 0° = horizontal, +90° = zenith, -90° = nadir
    pub elevation: f32,

    /// Distance from the listening position in metres (default: 1.0).
    /// Not used for rendering but transmitted via OSC for visualisation.
    pub distance: f32,

    /// Public coordinate source of truth for persistence and UI round-trips.
    pub coord_mode: String,

    /// Normalized Omniphony Cartesian coordinates in [-1, 1].
    pub x: f32,
    pub y: f32,
    pub z: f32,

    /// Whether this speaker participates in VBAP spatialization
    /// Set to false for LFE/subwoofers (default: true)
    pub spatialize: bool,

    /// Per-entry gain in dB (default: 0 = unity, 0.1 dB resolution). In the
    /// virtual bed, the per-input-channel trim; in an output layout, the saved
    /// per-speaker output gain, which seeds the live one
    /// (`live_params::speaker_live_from_layout`) and is written back from it
    /// on Save.
    pub gain_db: f32,

    /// Per-speaker output delay in milliseconds (default: 0.0).
    pub delay_ms: f32,

    /// Lowest frequency this speaker can reproduce, in Hz (default: None = 0 Hz).
    pub freq_low: Option<f32>,

    /// Highest frequency this speaker can reproduce, in Hz (default: None = +∞ Hz).
    pub freq_high: Option<f32>,
}

/// How far a BRIR emitter may sit from a standard position and still take its
/// name ([`SpeakerLayout::from_brir_emitters`]), degrees.
pub const BRIR_NAME_MATCH_DEG: f32 = 20.0;

/// Standard positions a BRIR emitter is named after: `(name, azimuth,
/// elevation)` in the layout convention (negative azimuth to the left).
const BRIR_STANDARD_POSITIONS: [(&str, f32, f32); 16] = [
    ("C", 0.0, 0.0),
    ("FL", -30.0, 0.0),
    ("FR", 30.0, 0.0),
    ("FWL", -60.0, 0.0),
    ("FWR", 60.0, 0.0),
    ("SL", -90.0, 0.0),
    ("SR", 90.0, 0.0),
    ("BL", -135.0, 0.0),
    ("BR", 135.0, 0.0),
    ("BC", 180.0, 0.0),
    ("TFL", -45.0, 35.0),
    ("TFR", 45.0, 35.0),
    ("TSL", -90.0, 45.0),
    ("TSR", 90.0, 45.0),
    ("TBL", -135.0, 35.0),
    ("TBR", 135.0, 35.0),
];

/// Great-circle angle between two `(azimuth, elevation)` directions, degrees.
fn angle_between_deg(az_a: f32, el_a: f32, az_b: f32, el_b: f32) -> f32 {
    let (a, b) = (
        geometry::from_spherical(az_a, el_a, 1.0),
        geometry::from_spherical(az_b, el_b, 1.0),
    );
    let dot = a.0 * b.0 + a.1 * b.1 + a.2 * b.2;
    dot.clamp(-1.0, 1.0).acos().to_degrees()
}

fn default_coord_mode() -> String {
    "polar".to_string()
}

fn default_spatialize() -> bool {
    true
}

fn default_delay_ms() -> f32 {
    0.0
}

fn default_radius_m() -> f32 {
    1.0
}

fn speaker_with_distance(
    name: impl Into<String>,
    azimuth: f32,
    elevation: f32,
    distance: f32,
) -> Speaker {
    Speaker::from_polar(name, azimuth, elevation, distance, true, 0.0)
}

#[derive(Deserialize)]
struct RawSpeaker {
    name: String,
    azimuth: Option<f32>,
    elevation: Option<f32>,
    distance: Option<f32>,
    #[serde(default = "default_coord_mode")]
    coord_mode: String,
    x: Option<f32>,
    y: Option<f32>,
    z: Option<f32>,
    #[serde(default = "default_spatialize")]
    spatialize: bool,
    #[serde(default)]
    gain_db: f32,
    #[serde(default = "default_delay_ms")]
    delay_ms: f32,
    #[serde(default)]
    freq_low: Option<f32>,
    #[serde(default)]
    freq_high: Option<f32>,
}

impl<'de> Deserialize<'de> for Speaker {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawSpeaker::deserialize(deserializer)?;
        let coord_mode = if raw.coord_mode.eq_ignore_ascii_case("cartesian") {
            "cartesian".to_string()
        } else {
            "polar".to_string()
        };
        let (azimuth, elevation, distance, x, y, z) =
            if let (Some(x), Some(y), Some(z)) = (raw.x, raw.y, raw.z) {
                let x = x.clamp(-1.0, 1.0);
                let y = y.clamp(-1.0, 1.0);
                let z = z.clamp(-1.0, 1.0);
                let (az, el, dist) = geometry::to_spherical(x, y, z);
                (
                    raw.azimuth.unwrap_or(az),
                    raw.elevation.unwrap_or(el),
                    raw.distance.unwrap_or(dist).max(0.01),
                    x,
                    y,
                    z,
                )
            } else {
                let az = raw.azimuth.unwrap_or(0.0);
                let el = raw.elevation.unwrap_or(0.0);
                let dist = raw.distance.unwrap_or(1.0).max(0.01);
                let (x, y, z) = geometry::hydrate_from_spherical(az, el, dist);
                (az, el, dist, x, y, z)
            };
        Ok(Self {
            name: raw.name,
            azimuth,
            elevation,
            distance,
            coord_mode,
            x,
            y,
            z,
            spatialize: raw.spatialize,
            gain_db: raw.gain_db,
            delay_ms: raw.delay_ms,
            freq_low: raw.freq_low.filter(|value| *value > 0.0),
            freq_high: raw.freq_high.filter(|value| *value > 0.0),
        })
    }
}

impl Serialize for Speaker {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let cartesian = self.coord_mode.eq_ignore_ascii_case("cartesian");
        let field_count = 9;
        let mut state = serializer.serialize_struct("Speaker", field_count)?;
        state.serialize_field("name", &self.name)?;
        state.serialize_field("coord_mode", if cartesian { "cartesian" } else { "polar" })?;
        if cartesian {
            state.serialize_field("x", &self.x)?;
            state.serialize_field("y", &self.y)?;
            state.serialize_field("z", &self.z)?;
        } else {
            state.serialize_field("azimuth", &self.azimuth)?;
            state.serialize_field("elevation", &self.elevation)?;
            state.serialize_field("distance", &self.distance)?;
        }
        state.serialize_field("spatialize", &self.spatialize)?;
        // Same 0.01 dB write tolerance as the render-config gains: below that
        // is inaudible and must not re-add a key the user never set.
        if self.gain_db.abs() > 0.01 {
            state.serialize_field("gain_db", &self.gain_db)?;
        }
        state.serialize_field("delay_ms", &self.delay_ms)?;
        if self.freq_low.is_some() {
            state.serialize_field("freq_low", &self.freq_low)?;
        }
        if self.freq_high.is_some() {
            state.serialize_field("freq_high", &self.freq_high)?;
        }
        state.end()
    }
}

impl Speaker {
    pub fn from_polar(
        name: impl Into<String>,
        azimuth: f32,
        elevation: f32,
        distance: f32,
        spatialize: bool,
        delay_ms: f32,
    ) -> Self {
        let distance = distance.max(0.01);
        let (x, y, z) = geometry::hydrate_from_spherical(azimuth, elevation, distance);
        Self {
            name: name.into(),
            azimuth,
            elevation,
            distance,
            coord_mode: "polar".to_string(),
            x,
            y,
            z,
            spatialize,
            gain_db: 0.0,
            delay_ms: delay_ms.max(0.0),
            freq_low: None,
            freq_high: None,
        }
    }

    /// Create a speaker from normalised cartesian coordinates in `[-1, 1]`,
    /// deriving the polar representation — the same conversion the YAML
    /// deserializer applies to a `coord_mode: cartesian` entry.
    pub fn from_cartesian(
        name: impl Into<String>,
        x: f32,
        y: f32,
        z: f32,
        spatialize: bool,
        delay_ms: f32,
    ) -> Self {
        let x = x.clamp(-1.0, 1.0);
        let y = y.clamp(-1.0, 1.0);
        let z = z.clamp(-1.0, 1.0);
        let (azimuth, elevation, distance) = geometry::to_spherical(x, y, z);
        Self {
            name: name.into(),
            azimuth,
            elevation,
            distance: distance.max(0.01),
            coord_mode: "cartesian".to_string(),
            x,
            y,
            z,
            spatialize,
            gain_db: 0.0,
            delay_ms: delay_ms.max(0.0),
            freq_low: None,
            freq_high: None,
        }
    }

    pub fn with_freq_low(mut self, freq_low: f32) -> Self {
        self.freq_low = Some(freq_low.max(0.0));
        self
    }

    pub fn with_freq_high(mut self, freq_high: f32) -> Self {
        self.freq_high = Some(freq_high.max(0.0));
        self
    }

    /// Create a new speaker (spatialize defaults to true)
    pub fn new(name: impl Into<String>, azimuth: f32, elevation: f32) -> Self {
        Self::from_polar(name, azimuth, elevation, 1.0, true, 0.0)
    }

    /// Create a new speaker with explicit spatialize flag
    pub fn new_with_spatialize(
        name: impl Into<String>,
        azimuth: f32,
        elevation: f32,
        spatialize: bool,
    ) -> Self {
        Self::from_polar(name, azimuth, elevation, 1.0, spatialize, 0.0)
    }

    /// Get position as [azimuth, elevation] array for VBAP
    pub fn position(&self) -> [f32; 2] {
        [self.azimuth, self.elevation]
    }

    /// Validate speaker angles are in valid range
    pub fn validate(&self) -> Result<()> {
        if self.azimuth < -180.0 || self.azimuth > 180.0 {
            anyhow::bail!(
                "Speaker '{}': azimuth {:.1}° out of range [-180, 180]",
                self.name,
                self.azimuth
            );
        }

        if self.elevation < -90.0 || self.elevation > 90.0 {
            anyhow::bail!(
                "Speaker '{}': elevation {:.1}° out of range [-90, 90]",
                self.name,
                self.elevation
            );
        }

        Ok(())
    }
}

/// Speaker layout configuration
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct SpeakerLayout {
    /// Physical metres-per-unit scale for UI distance/delay conversion.
    #[serde(default = "default_radius_m")]
    pub radius_m: f32,
    /// List of speakers in the layout
    pub speakers: Vec<Speaker>,
}

impl SpeakerLayout {
    /// Load speaker layout from YAML file
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let file = File::open(path)
            .with_context(|| format!("Failed to open speaker layout file: {}", path.display()))?;

        let reader = BufReader::new(file);
        let layout: SpeakerLayout = serde_yaml_ng::from_reader(reader)
            .with_context(|| format!("Failed to parse speaker layout YAML: {}", path.display()))?;

        layout.validate()?;

        Ok(layout)
    }

    /// Parse a speaker layout from a YAML string (same schema as
    /// [`from_file`](Self::from_file)). Used to apply a layout received over OSC
    /// (e.g. the virtual bed) without touching the filesystem.
    pub fn from_yaml_str(yaml: &str) -> Result<Self> {
        let layout: SpeakerLayout =
            serde_yaml_ng::from_str(yaml).context("Failed to parse speaker layout YAML")?;
        layout.validate()?;
        Ok(layout)
    }

    /// Parse a set of placement entries from a YAML string: the same schema
    /// as a layout, but a family's entries are a partial set — one channel
    /// is a legitimate list, and so is none — so only each entry and the
    /// names' uniqueness are validated, not the VBAP minimum an *output*
    /// layout needs.
    pub fn entries_from_yaml_str(yaml: &str) -> Result<Self> {
        let layout: SpeakerLayout =
            serde_yaml_ng::from_str(yaml).context("Failed to parse channel entries YAML")?;
        for speaker in &layout.speakers {
            speaker.validate()?;
        }
        let mut names = std::collections::HashSet::new();
        for speaker in &layout.speakers {
            if !names.insert(speaker.name.as_str()) {
                anyhow::bail!("Duplicate channel entry: '{}'", speaker.name);
            }
        }
        Ok(layout)
    }

    /// Create a speaker layout from a vector of speakers
    pub fn from_speakers(speakers: Vec<Speaker>) -> Result<Self> {
        let layout = Self {
            radius_m: 1.0,
            speakers,
        };
        layout.validate()?;
        Ok(layout)
    }

    /// The virtual loudspeakers of a measured room (a BRIR set): one
    /// spatialized speaker per emitter, in the set's order, so bus `n` is
    /// emitter `n`, plus a non-spatialized `LFE` last — the set measures no
    /// subwoofer, and the cascade's direct-bus policy feeds it to both ears.
    ///
    /// `positions` are the emitters relative to the listener in the
    /// renderer's frame (`x` right, `y` front, `z` up, metres). Each is
    /// placed as a fraction of the measured room they stand in — `room`,
    /// `radius_m` metres to its unit ([`crate::binaural::brir::MeasuredRoom`])
    /// — by the inverse of the stage's warp, so that warping the layout
    /// with that same room returns every speaker to its measured position
    /// (up to scale) and an object is panned among them in the room's own
    /// metric. No delay, gain or crossover band: the measurement carries the
    /// room's own. An emitter within [`BRIR_NAME_MATCH_DEG`] of a standard
    /// position takes that position's name (each name once, nearest
    /// first), so beds placed by channel name still find their speaker; the
    /// others are `E<n>`, `n` counted from 1.
    pub fn from_brir_emitters(
        positions: &[[f32; 3]],
        room: &crate::live_params::RoomRatios,
        radius_m: f32,
    ) -> Result<Self> {
        let radius_m = radius_m.max(0.01);
        let directions: Vec<(f32, f32)> = positions
            .iter()
            .map(|&[x, y, z]| {
                let (azimuth, elevation, _) = geometry::to_spherical(x, y, z);
                (azimuth, elevation)
            })
            .collect();
        let mut names: Vec<Option<&str>> = vec![None; positions.len()];
        let mut candidates: Vec<(f32, usize, usize)> = Vec::new();
        for (e, &(azimuth, elevation)) in directions.iter().enumerate() {
            for (n, &(_, std_azimuth, std_elevation)) in BRIR_STANDARD_POSITIONS.iter().enumerate()
            {
                let angle = angle_between_deg(azimuth, elevation, std_azimuth, std_elevation);
                if angle <= BRIR_NAME_MATCH_DEG {
                    candidates.push((angle, e, n));
                }
            }
        }
        candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut taken = [false; BRIR_STANDARD_POSITIONS.len()];
        for (_, e, n) in candidates {
            if names[e].is_none() && !taken[n] {
                names[e] = Some(BRIR_STANDARD_POSITIONS[n].0);
                taken[n] = true;
            }
        }
        let mut speakers: Vec<Speaker> = positions
            .iter()
            .zip(&names)
            .enumerate()
            .map(|(e, (&[x, y, z], name))| {
                let [x, y, z] = room.inverse([x / radius_m, y / radius_m, z / radius_m]);
                let name = name.map_or_else(|| format!("E{}", e + 1), str::to_string);
                Speaker::from_cartesian(name, x, y, z, true, 0.0)
            })
            .collect();
        speakers.push(Speaker::new_with_spatialize("LFE", 0.0, -30.0, false));
        Self::from_speakers(speakers)
    }

    /// Get number of speakers in the layout
    pub fn num_speakers(&self) -> usize {
        self.speakers.len()
    }

    /// Get speaker positions as [[az, el], ...] for VBAP
    pub fn positions(&self) -> Vec<[f32; 2]> {
        self.speakers.iter().map(|s| s.position()).collect()
    }

    /// Get positions for speakers that participate in spatialization (spatialize=true)
    /// Returns (positions, vbap_to_speaker_mapping)
    /// - positions: Vec of [az, el] for VBAP
    /// - mapping: Vec mapping VBAP index → speaker index
    pub fn spatializable_positions(&self) -> (Vec<[f32; 2]>, Vec<usize>) {
        let mut positions = Vec::new();
        let mut mapping = Vec::new();

        for (speaker_idx, speaker) in self.speakers.iter().enumerate() {
            if speaker.spatialize {
                positions.push(speaker.position());
                mapping.push(speaker_idx);
            }
        }

        (positions, mapping)
    }

    /// Get positions for speakers that participate in spatialization, with
    /// cartesian speakers converted to directions in the same room-ratio space
    /// as rendered objects.
    pub fn spatializable_positions_for_room(
        &self,
        room_ratio: [f32; 3],
        room_ratio_rear: f32,
        room_ratio_lower: f32,
        room_ratio_center_blend: f32,
    ) -> (Vec<[f32; 2]>, Vec<usize>) {
        let mut positions = Vec::new();
        let mut mapping = Vec::new();

        for (speaker_idx, speaker) in self.speakers.iter().enumerate() {
            if !speaker.spatialize {
                continue;
            }
            let pos = if speaker.coord_mode.eq_ignore_ascii_case("cartesian") {
                let scaled_x = speaker.x * room_ratio[0];
                let scaled_y = geometry::map_depth(
                    speaker.y,
                    room_ratio[1],
                    room_ratio_rear,
                    room_ratio_center_blend,
                );
                let scaled_z = if speaker.z >= 0.0 {
                    speaker.z * room_ratio[2]
                } else {
                    speaker.z * room_ratio_lower
                };
                let (az, el, _) =
                    crate::spatial_vbap::adm_to_spherical(scaled_x, scaled_y, scaled_z);
                [az, el]
            } else {
                speaker.position()
            };
            positions.push(pos);
            mapping.push(speaker_idx);
        }

        (positions, mapping)
    }

    /// Get speaker names
    pub fn speaker_names(&self) -> Vec<&str> {
        self.speakers.iter().map(|s| s.name.as_str()).collect()
    }

    /// Per-label speaker lookup: each recognised speaker name (shared alias
    /// table) maps its channel label to the speaker index; the first speaker
    /// matching a label wins. This is the layout-independent routing language
    /// of `docs/channel-object-contract.md` — a stored `RChannelLabel` stays
    /// valid across layout swaps, the topology re-resolves it here.
    pub fn label_to_speaker_mapping(
        &self,
    ) -> std::collections::HashMap<bridge_api::RChannelLabel, usize> {
        let mut mapping = std::collections::HashMap::new();
        for (speaker_idx, speaker) in self.speakers.iter().enumerate() {
            let label = bridge_api::labels::label_for_name(&speaker.name);
            if label != bridge_api::RChannelLabel::Unknown {
                mapping.entry(label).or_insert(speaker_idx);
            }
        }
        mapping
    }

    /// Validate the layout
    pub fn validate(&self) -> Result<()> {
        if self.speakers.is_empty() {
            anyhow::bail!("Speaker layout must contain at least one speaker");
        }

        if self.speakers.len() < 3 {
            anyhow::bail!(
                "VBAP requires at least 3 speakers, found {}",
                self.speakers.len()
            );
        }

        // Validate each speaker
        for speaker in &self.speakers {
            speaker.validate()?;
        }

        // Check for duplicate names
        let mut names = std::collections::HashSet::new();
        for speaker in &self.speakers {
            if !names.insert(speaker.name.as_str()) {
                anyhow::bail!("Duplicate speaker name: '{}'", speaker.name);
            }
        }

        Ok(())
    }

    /// Get a preset layout by name
    pub fn preset(name: &str) -> Result<Self> {
        match name {
            "stereo" => Self::preset_stereo(),
            "5.1" => Self::preset_5_1(),
            "7.1" => Self::preset_7_1(),
            "7.1.4" => Self::preset_7_1_4(),
            "9.1.6" => Self::preset_9_1_6(),
            "cascade-12" => Self::preset_cascade_12(),
            _ => anyhow::bail!(
                "Unknown preset layout: '{}'. Available: stereo, 5.1, 7.1, 7.1.4, 9.1.6, cascade-12",
                name
            ),
        }
    }

    /// Stereo pair on the front corners of the default 1:2 room — ±26.57°,
    /// not the ITU-R BS.775 ±30°.
    pub fn preset_stereo() -> Result<Self> {
        Self::from_speakers(vec![
            speaker_with_distance("L", -26.565052, 0.0, 2.236068),
            speaker_with_distance("R", 26.565052, 0.0, 2.236068),
            Speaker::new("Top", 0.0, 90.0), // Dummy for 3D triangulation
        ])
    }

    /// 5.1 with the ITU-R BS.775 channel set, placed on the corners of the
    /// default 1:2 room: fronts at ±26.57°, backs at ±153.4° (not the
    /// recommendation's ±30° / ±110°).
    pub fn preset_5_1() -> Result<Self> {
        Self::from_speakers(vec![
            speaker_with_distance("FL", -26.565052, 0.0, 2.236068),
            speaker_with_distance("FR", 26.565052, 0.0, 2.236068),
            speaker_with_distance("C", 0.0, 0.0, 2.0),
            speaker_with_distance("LFE", 26.565052, -12.6043825, 2.291288),
            speaker_with_distance("BL", -153.43495, 0.0, 2.236068),
            speaker_with_distance("BR", 153.43495, 0.0, 2.236068),
        ])
    }

    /// 7.1: the [`Self::preset_5_1`] room-corner placement (fronts ±26.57°,
    /// backs ±153.4°) plus sides at ±90° — not the ITU-R BS.775 angles.
    pub fn preset_7_1() -> Result<Self> {
        Self::from_speakers(vec![
            speaker_with_distance("FL", -26.565052, 0.0, 2.236068),
            speaker_with_distance("FR", 26.565052, 0.0, 2.236068),
            speaker_with_distance("C", 0.0, 0.0, 2.0),
            speaker_with_distance("LFE", 26.565052, -12.6043825, 2.291288),
            speaker_with_distance("BL", -153.43495, 0.0, 2.236068),
            speaker_with_distance("BR", 153.43495, 0.0, 2.236068),
            speaker_with_distance("SL", -90.0, 0.0, 1.0),
            speaker_with_distance("SR", 90.0, 0.0, 1.0),
        ])
    }

    /// 7.1.4 spatial audio layout, the renderer's default when no layout is
    /// configured. Kept byte-for-byte in sync with `layouts/7.1.4.yaml` (the
    /// "omniphony (live)" default) in normalised cartesian coordinates — see
    /// `preset_7_1_4_matches_bundled_yaml`.
    pub fn preset_7_1_4() -> Result<Self> {
        Self::from_speakers(vec![
            // Bed layer (7.1)
            Speaker::from_cartesian("FL", -1.0, 1.0, 0.0, true, 0.0),
            Speaker::from_cartesian("FR", 1.0, 1.0, 0.0, true, 0.0),
            Speaker::from_cartesian("C", 0.0, 1.0, 0.0, true, 0.0),
            Speaker::from_cartesian("LFE", 1.0, 1.0, -1.0, false, 0.0),
            Speaker::from_cartesian("BL", -1.0, -1.0, 0.0, true, 0.0),
            Speaker::from_cartesian("BR", 1.0, -1.0, 0.0, true, 0.0),
            Speaker::from_cartesian("SL", -1.0, 0.0, 0.0, true, 0.0),
            Speaker::from_cartesian("SR", 1.0, 0.0, 0.0, true, 0.0),
            // Height layer
            Speaker::from_cartesian("TFL", -1.0, 1.0, 1.0, true, 0.0),
            Speaker::from_cartesian("TFR", 1.0, 1.0, 1.0, true, 0.0),
            Speaker::from_cartesian("TBL", -1.0, -1.0, 1.0, true, 0.0),
            Speaker::from_cartesian("TBR", 1.0, -1.0, 1.0, true, 0.0),
        ])
    }

    /// Virtual layout for the cascaded binaural mode (issue #220): a closed 3D
    /// shell of 12 spatialized speakers around the listener — 8 on the ear
    /// plane (45° spacing, where most content lives) and 4 at 45° elevation.
    /// The zenith is covered by the panner's virtual-pole downmix onto the
    /// height ring. The LFE entry (`spatialize: false`) receives one-hot
    /// LFE-routed channels exactly like a physical room; the binaural stage
    /// then feeds it to both ears dry (its direct-channel policy).
    pub fn preset_cascade_12() -> Result<Self> {
        Self::from_speakers(vec![
            // Ear-plane ring (8), 45° spacing.
            Speaker::new("C", 0.0, 0.0),
            Speaker::new_with_spatialize("LFE", 45.0, -10.0, false),
            Speaker::new("FL", -45.0, 0.0),
            Speaker::new("FR", 45.0, 0.0),
            Speaker::new("SL", -90.0, 0.0),
            Speaker::new("SR", 90.0, 0.0),
            Speaker::new("BL", -135.0, 0.0),
            Speaker::new("BR", 135.0, 0.0),
            Speaker::new("B", 180.0, 0.0),
            // Height ring (4) at 45° elevation.
            Speaker::new("TFL", -45.0, 45.0),
            Speaker::new("TFR", 45.0, 45.0),
            Speaker::new("TBL", -135.0, 45.0),
            Speaker::new("TBR", 135.0, 45.0),
        ])
    }

    /// 9.1.6 with the channel set of ITU-R BS.2051, placed on the walls and
    /// corners of the default 1:2 room (fronts ±26.57°, wides ±63.4°, backs
    /// ±153.4°, heights on the ceiling corners) rather than at the
    /// recommendation's angles.
    // The TSL/TSR distance `1.4142136` parses to the f32 one ULP above
    // `std::f32::consts::SQRT_2` (0x3fb504f4 against 0x3fb504f3). Swapping in
    // the constant would move those two speakers and so change this preset's
    // render, so the literal stays and the lint is silenced here only.
    #[allow(clippy::approx_constant)]
    pub fn preset_9_1_6() -> Result<Self> {
        Self::from_speakers(vec![
            // Bed layer (9.1)
            speaker_with_distance("FL", -26.565052, 0.0, 2.236068),
            speaker_with_distance("FR", 26.565052, 0.0, 2.236068),
            speaker_with_distance("C", 0.0, 0.0, 2.0),
            speaker_with_distance("LFE", 26.565052, -12.6043825, 2.291288),
            speaker_with_distance("BL", -153.43495, 0.0, 2.236068),
            speaker_with_distance("BR", 153.43495, 0.0, 2.236068),
            speaker_with_distance("SL", -90.0, 0.0, 1.0),
            speaker_with_distance("SR", 90.0, 0.0, 1.0),
            speaker_with_distance("FWL", -63.43495, 0.0, 1.118034),
            speaker_with_distance("FWR", 63.43495, 0.0, 1.118034),
            // Height layer (6 speakers)
            speaker_with_distance("TFL", -45.0, 35.26439, 1.7320508),
            speaker_with_distance("TFR", 45.0, 35.26439, 1.7320508),
            speaker_with_distance("TSL", -90.0, 45.0, 1.4142136),
            speaker_with_distance("TSR", 90.0, 45.0, 1.4142136),
            speaker_with_distance("TBL", -135.0, 35.26439, 1.7320508),
            speaker_with_distance("TBR", 135.0, 35.26439, 1.7320508),
        ])
    }

    /// Save layout to YAML file
    pub fn save_to_file(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let file = File::create(path)
            .with_context(|| format!("Failed to create file: {}", path.display()))?;

        serde_yaml_ng::to_writer(file, self)
            .with_context(|| format!("Failed to write YAML: {}", path.display()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binaural::brir::MeasuredRoom;
    use crate::live_params::RoomRatios;

    /// The 13 loudspeakers of a generic 9+4 measured room, as the BRIR loader
    /// reports them: renderer frame, metres.
    fn nine_plus_four_room_emitters() -> Vec<[f32; 3]> {
        [
            (0.0, 0.0, 1.99),
            (-45.0, 0.0, 3.01),
            (45.0, 0.0, 3.01),
            (-30.0, 0.0, 2.37),
            (30.0, 0.0, 2.37),
            (-90.0, 0.0, 2.28),
            (90.0, 0.0, 2.28),
            (-135.0, 0.0, 3.01),
            (135.0, 0.0, 3.01),
            (-45.0, 40.0, 1.91),
            (45.0, 40.0, 1.91),
            (-110.0, 40.0, 1.91),
            (110.0, 40.0, 1.91),
        ]
        .iter()
        .map(|&(az, el, r)| {
            let (x, y, z) = geometry::from_spherical(az, el, r);
            [x, y, z]
        })
        .collect()
    }

    /// The layout of a set whose loudspeakers stand at `emitters` (metres,
    /// renderer frame), in the room estimated from them with the default
    /// front/rear blend: what `RendererControl::brir_layout` builds.
    fn brir_layout_of(emitters: &[[f32; 3]]) -> (SpeakerLayout, RoomRatios) {
        let measured = MeasuredRoom::of(emitters, None);
        let room = measured.ratios(0.5);
        let layout =
            SpeakerLayout::from_brir_emitters(emitters, &room, measured.radius_m()).unwrap();
        (layout, room)
    }

    #[test]
    fn a_brir_layout_has_one_speaker_per_emitter_pointing_at_it() {
        let emitters = nine_plus_four_room_emitters();
        let (layout, room) = brir_layout_of(&emitters);
        assert_eq!(layout.num_speakers(), emitters.len() + 1);
        // Placed as fractions of the measured room, the speakers point at
        // their emitters once the stage warps them with that room — not
        // before: the cube reading is the room-fraction, which an
        // elongated room moves off the direction.
        let (positions, mapping) = layout.spatializable_positions_for_room(
            room.ratio,
            room.rear,
            room.lower,
            room.center_blend,
        );
        assert_eq!(mapping.len(), emitters.len());
        for ((speaker, e), [az, el]) in layout.speakers.iter().zip(&emitters).zip(&positions) {
            let (want_az, want_el, _) = geometry::to_spherical(e[0], e[1], e[2]);
            assert!(
                // f32 `acos` near 1 resolves a few hundredths of a degree.
                angle_between_deg(*az, *el, want_az, want_el) < 0.1,
                "{} points at its emitter in the room: {az} {el} vs {want_az} {want_el}",
                speaker.name
            );
            assert!(speaker.spatialize);
            assert_eq!(
                (
                    speaker.gain_db,
                    speaker.delay_ms,
                    speaker.freq_low,
                    speaker.freq_high
                ),
                (0.0, 0.0, None, None),
                "no trim, delay or band of its own"
            );
        }
        let lfe = layout.speakers.last().unwrap();
        assert_eq!(lfe.name, "LFE");
        assert!(!lfe.spatialize, "the LFE is a direct bus");
    }

    /// Warping a speaker's room fraction with the room it was placed in
    /// returns its measured position, to the metre scale of the room: the
    /// topology pans an object onto the loudspeakers where they stand.
    #[test]
    fn a_brir_speaker_warps_back_to_its_measured_position() {
        // A room the listener is not centred in: fronts far, sides near,
        // a back wall closer than the front one.
        let emitters: Vec<[f32; 3]> = vec![
            [0.0, 3.0, 0.0],
            [-1.5, 3.0, 0.0],
            [1.5, 3.0, 0.0],
            [-2.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [-1.5, -2.0, 0.0],
            [1.5, -2.0, 0.0],
            [-1.5, 3.0, 1.5],
            [1.5, 3.0, 1.5],
        ];
        let measured = MeasuredRoom::of(&emitters, None);
        let room = measured.ratios(0.5);
        let radius = measured.radius_m();
        let layout = SpeakerLayout::from_brir_emitters(&emitters, &room, radius).unwrap();
        assert_ne!(room.ratio[1], room.rear, "the room is deeper to the front");
        for (speaker, e) in layout.speakers.iter().zip(&emitters) {
            let back = room.scale([speaker.x, speaker.y, speaker.z]);
            for axis in 0..3 {
                assert!(
                    (back[axis] * radius - e[axis]).abs() < 1e-3,
                    "{}: axis {axis} warps back to {} m, measured {} m",
                    speaker.name,
                    back[axis] * radius,
                    e[axis]
                );
            }
        }
    }

    #[test]
    fn brir_emitters_take_the_nearest_free_standard_name() {
        let (layout, _) = brir_layout_of(&nine_plus_four_room_emitters());
        assert_eq!(
            layout.speaker_names(),
            [
                // ±45° lose FL/FR to the exact ±30° pair and are the wides.
                "C", "FWL", "FWR", "FL", "FR", "SL", "SR", "BL", "BR", "TFL", "TFR",
                // ±110° at 40°: nearer the top sides than the top backs.
                "TSL", "TSR", "LFE",
            ]
        );
        // Nothing standard near it: numbered from 1 in the set's order.
        let (odd, _) = brir_layout_of(&[
            [0.0, 1.0, 0.0],
            [-1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 0.3, -1.0],
        ]);
        assert_eq!(odd.speaker_names(), ["C", "SL", "SR", "E4", "LFE"]);
    }

    #[test]
    fn placement_entries_accept_a_partial_set_and_reject_duplicates() {
        // One channel is a legitimate set of entries: only its trim and
        // routing are meant, the rest of the channels keep their defaults.
        let one = SpeakerLayout::entries_from_yaml_str(
            "speakers:\n  - { name: LFE, coord_mode: cartesian, x: 0, y: 1, z: 0, spatialize: false, gain_db: -6 }\n",
        )
        .expect("one entry parses");
        assert_eq!(one.speakers.len(), 1);
        assert!(!one.speakers[0].spatialize);
        assert!(
            SpeakerLayout::from_yaml_str(
                "speakers:\n  - { name: LFE, coord_mode: cartesian, x: 0, y: 1, z: 0 }\n"
            )
            .is_err(),
            "an output layout still needs its VBAP minimum"
        );
        assert!(SpeakerLayout::entries_from_yaml_str("speakers: []\n").is_ok());
        assert!(
            SpeakerLayout::entries_from_yaml_str(
                "speakers:\n  - { name: Ls, coord_mode: polar, azimuth: -110, elevation: 0, distance: 1 }\n  - { name: Ls, coord_mode: polar, azimuth: -90, elevation: 0, distance: 1 }\n"
            )
            .is_err(),
            "the same channel twice is a mistake, not a choice"
        );
    }

    #[test]
    fn label_mapping_accepts_every_legacy_alias() {
        // Parity net kept from the bed-id era: every spelling the historical
        // alias table accepted must still resolve, now to its channel label.
        use bridge_api::RChannelLabel as L;
        let legacy: [(L, &[&str]); 10] = [
            (L::L, &["L", "FL", "FrontLeft", "LeftFront"]),
            (L::R, &["R", "FR", "FrontRight", "RightFront"]),
            (L::C, &["C", "FC", "Center", "Centre"]),
            (L::LFE, &["LFE", "Sub", "Subwoofer", "SW"]),
            (L::Ls, &["Ls", "SL", "LeftSurround", "SurroundLeft"]),
            (L::Rs, &["Rs", "SR", "RightSurround", "SurroundRight"]),
            (
                L::Lb,
                &[
                    "Lb", "BL", "Lrs", "BackLeft", "LeftBack", "RearLeft", "LeftRear",
                ],
            ),
            (
                L::Rb,
                &[
                    "Rb",
                    "BR",
                    "Rrs",
                    "BackRight",
                    "RightBack",
                    "RearRight",
                    "RightRear",
                ],
            ),
            (
                L::Tfl,
                &[
                    "Ltf",
                    "TFL",
                    "TopFrontLeft",
                    "LeftTopFront",
                    "HeightLeft",
                    "HL",
                ],
            ),
            (
                L::Tfr,
                &[
                    "Rtf",
                    "TFR",
                    "TopFrontRight",
                    "RightTopFront",
                    "HeightRight",
                    "HR",
                ],
            ),
        ];
        for (label, aliases) in legacy {
            for alias in aliases {
                let layout = SpeakerLayout::from_speakers(vec![
                    Speaker::new("A", -30.0, 0.0),
                    Speaker::new(*alias, 30.0, 0.0),
                    Speaker::new("B", 110.0, 0.0),
                ])
                .expect("parity layout");
                let mapping = layout.label_to_speaker_mapping();
                assert_eq!(
                    mapping.get(&label),
                    Some(&1),
                    "legacy alias {alias:?} no longer maps to {label:?}"
                );
            }
        }
    }

    #[test]
    fn label_mapping_first_matching_speaker_wins() {
        let layout = SpeakerLayout::from_speakers(vec![
            Speaker::new("FL", -30.0, 0.0),
            Speaker::new("FrontLeft", -31.0, 0.0),
            Speaker::new("FR", 30.0, 0.0),
        ])
        .expect("dup layout");
        assert_eq!(
            layout
                .label_to_speaker_mapping()
                .get(&bridge_api::RChannelLabel::L),
            Some(&0)
        );
    }

    #[test]
    fn test_speaker_validation() {
        // Valid speaker
        assert!(Speaker::new("FL", -30.0, 0.0).validate().is_ok());

        // Invalid azimuth
        assert!(Speaker::new("FL", -200.0, 0.0).validate().is_err());
        assert!(Speaker::new("FL", 200.0, 0.0).validate().is_err());

        // Invalid elevation
        assert!(Speaker::new("FL", 0.0, -100.0).validate().is_err());
        assert!(Speaker::new("FL", 0.0, 100.0).validate().is_err());
    }

    #[test]
    fn test_layout_validation() {
        // Valid layout
        let layout = SpeakerLayout::from_speakers(vec![
            Speaker::new("FL", -30.0, 0.0),
            Speaker::new("FR", 30.0, 0.0),
            Speaker::new("C", 0.0, 0.0),
        ]);
        assert!(layout.is_ok());

        // Too few speakers
        let layout = SpeakerLayout::from_speakers(vec![
            Speaker::new("FL", -30.0, 0.0),
            Speaker::new("FR", 30.0, 0.0),
        ]);
        assert!(layout.is_err());

        // Duplicate names
        let layout = SpeakerLayout::from_speakers(vec![
            Speaker::new("FL", -30.0, 0.0),
            Speaker::new("FL", 30.0, 0.0),
            Speaker::new("C", 0.0, 0.0),
        ]);
        assert!(layout.is_err());
    }

    #[test]
    fn test_preset_layouts() {
        // Test all presets load successfully
        assert!(SpeakerLayout::preset("stereo").is_ok());
        assert!(SpeakerLayout::preset("5.1").is_ok());
        assert!(SpeakerLayout::preset("7.1").is_ok());
        assert!(SpeakerLayout::preset("7.1.4").is_ok());
        assert!(SpeakerLayout::preset("9.1.6").is_ok());

        // Test invalid preset
        assert!(SpeakerLayout::preset("invalid").is_err());
    }

    #[test]
    fn test_7_1_4_layout() {
        let layout = SpeakerLayout::preset("7.1.4").unwrap();
        assert_eq!(layout.num_speakers(), 12);

        // The preset mirrors layouts/7.1.4.yaml (normalised cartesian).
        let fl = &layout.speakers[0];
        assert_eq!(fl.name, "FL");
        assert_eq!(fl.coord_mode, "cartesian");
        assert_eq!([fl.x, fl.y, fl.z], [-1.0, 1.0, 0.0]);

        let tfl = &layout.speakers[8];
        assert_eq!(tfl.name, "TFL");
        assert_eq!([tfl.x, tfl.y, tfl.z], [-1.0, 1.0, 1.0]);

        // LFE stays non-spatialized.
        assert!(!layout.speakers[3].spatialize);
    }

    #[test]
    fn test_positions_extraction() {
        let layout = SpeakerLayout::preset("5.1").unwrap();
        let positions = layout.positions();

        assert_eq!(positions.len(), 6);
        assert_eq!(positions[0], [-26.565052, 0.0]); // FL
        assert_eq!(positions[1], [26.565052, 0.0]); // FR
        assert_eq!(positions[2], [0.0, 0.0]); // C
    }

    #[test]
    fn test_speaker_names() {
        let layout = SpeakerLayout::preset("stereo").unwrap();
        let names = layout.speaker_names();

        assert_eq!(names.len(), 3);
        assert_eq!(names[0], "L");
        assert_eq!(names[1], "R");
        assert_eq!(names[2], "Top");
    }
}
#[cfg(test)]
mod integration_tests {
    use crate::speaker_layout::SpeakerLayout;
    use std::path::PathBuf;

    fn layout_path(name: &str) -> PathBuf {
        // CARGO_MANIFEST_DIR is the `renderer` crate dir
        // (`<repo>/omniphony-renderer/renderer`); the shipped layouts live at
        // the repo root (`<repo>/layouts`), so climb two levels up.
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("layouts")
            .join(name)
    }

    /// The shipped layouts load, with the speakers their names promise. The
    /// height-less ones live under `layouts/legacy/`.
    #[test]
    fn bundled_layouts_load_with_their_speaker_counts() {
        for (file, speakers) in [
            ("legacy/5.1.yaml", 6),
            ("7.1.4.yaml", 12),
            ("9.1.6.yaml", 16),
        ] {
            let layout = SpeakerLayout::from_file(layout_path(file))
                .unwrap_or_else(|e| panic!("{file}: {e:?}"));
            assert_eq!(layout.num_speakers(), speakers, "{file}");
        }
    }

    #[test]
    fn preset_7_1_4_matches_bundled_yaml() {
        // The default fallback layout (`SpeakerLayout::preset("7.1.4")`, used by
        // bootstrap/degraded/engine when no layout is configured) must stay in
        // sync with the shipped `layouts/7.1.4.yaml` ("omniphony (live)").
        let preset = SpeakerLayout::preset("7.1.4").expect("7.1.4 preset");
        let yaml = SpeakerLayout::from_file(layout_path("7.1.4.yaml")).expect("load 7.1.4.yaml");

        assert_eq!(preset.speakers.len(), yaml.speakers.len());
        for (p, y) in preset.speakers.iter().zip(&yaml.speakers) {
            assert_eq!(p.name, y.name, "speaker order/name mismatch");
            assert_eq!(p.coord_mode, y.coord_mode, "{} coord_mode mismatch", p.name);
            assert_eq!(p.spatialize, y.spatialize, "{} spatialize mismatch", p.name);
            assert!(
                (p.x - y.x).abs() < 1e-6 && (p.y - y.y).abs() < 1e-6 && (p.z - y.z).abs() < 1e-6,
                "{} cartesian mismatch: preset {:?} vs yaml {:?}",
                p.name,
                (p.x, p.y, p.z),
                (y.x, y.y, y.z)
            );
        }
    }
}
