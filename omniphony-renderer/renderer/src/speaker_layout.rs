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
//! ```no_run
//! use omniphony_renderer::speaker_layout::SpeakerLayout;
//!
//! let layout = SpeakerLayout::from_file("../layouts/7.1.4.yaml")?;
//! println!("Loaded {} speakers", layout.num_speakers());
//!
//! // Get positions for VBAP
//! let positions = layout.positions();
//! ```

use anyhow::{Context, Result};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

fn map_depth_with_room_ratios(
    depth: f32,
    front_ratio: f32,
    rear_ratio: f32,
    center_blend: f32,
) -> f32 {
    let d = depth.clamp(-1.0, 1.0);
    let blend = center_blend.clamp(0.0, 1.0);
    let center_ratio = rear_ratio + (front_ratio - rear_ratio) * blend;
    if d >= 0.0 {
        let t = d;
        let a = center_ratio - front_ratio;
        let b = 2.0 * (front_ratio - center_ratio);
        a * t * t * t + b * t * t + center_ratio * t
    } else {
        let t = -d;
        let a = center_ratio - rear_ratio;
        let b = 2.0 * (rear_ratio - center_ratio);
        -(a * t * t * t + b * t * t + center_ratio * t)
    }
}

/// A single speaker in the layout
#[derive(Debug, Clone)]
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

    /// Per-speaker output delay in milliseconds (default: 0.0).
    pub delay_ms: f32,

    /// Lowest frequency this speaker can reproduce, in Hz (default: None = 0 Hz).
    pub freq_low: Option<f32>,

    /// Highest frequency this speaker can reproduce, in Hz (default: None = +∞ Hz).
    pub freq_high: Option<f32>,
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

fn spherical_to_cartesian(azimuth: f32, elevation: f32, distance: f32) -> (f32, f32, f32) {
    let az = azimuth.to_radians();
    let el = elevation.to_radians();
    // Keep speaker cartesian persistence aligned with the renderer ADM convention:
    // x = right, y = front, z = up.
    let horizontal = distance * el.cos();
    let x = horizontal * az.sin();
    let y = horizontal * az.cos();
    let z = distance * el.sin();
    (x.clamp(-1.0, 1.0), y.clamp(-1.0, 1.0), z.clamp(-1.0, 1.0))
}

fn cartesian_to_spherical(x: f32, y: f32, z: f32) -> (f32, f32, f32) {
    let dist = (x * x + y * y + z * z).sqrt();
    let az = x.atan2(y).to_degrees();
    let el = if dist > 0.0 {
        z.atan2((x * x + y * y).sqrt()).to_degrees()
    } else {
        0.0
    };
    (az, el, dist.max(0.01))
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
                let (az, el, dist) = cartesian_to_spherical(x, y, z);
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
                let (x, y, z) = spherical_to_cartesian(az, el, dist);
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
        let (x, y, z) = spherical_to_cartesian(azimuth, elevation, distance);
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
#[derive(Debug, Clone, Deserialize, Serialize)]
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

    /// Create a speaker layout from a vector of speakers
    pub fn from_speakers(speakers: Vec<Speaker>) -> Result<Self> {
        let layout = Self {
            radius_m: 1.0,
            speakers,
        };
        layout.validate()?;
        Ok(layout)
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
                let scaled_y = map_depth_with_room_ratios(
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

    /// Create mapping from bed channel ID to speaker index based on speaker names
    ///
    /// Bed channel IDs (0-9) are mapped to speakers by matching names:
    /// - 0: L, FL, FrontLeft (Left Front)
    /// - 1: R, FR, FrontRight (Right Front)
    /// - 2: C, FC, Center (Center)
    /// - 3: LFE, Sub (Low Frequency Effects)
    /// - 4: Ls, SL, LeftSurround (Left Surround)
    /// - 5: Rs, SR, RightSurround (Right Surround)
    /// - 6: Lb, BL, Lrs, BackLeft (Left Back)
    /// - 7: Rb, BR, Rrs, BackRight (Right Back)
    /// - 8: Ltf, TFL, TopFrontLeft (Left Top Front)
    /// - 9: Rtf, TFR, TopFrontRight (Right Top Front)
    ///
    /// Returns a HashMap<bed_id, speaker_idx> for beds found in the layout.
    /// Beds not found are not included in the map.
    pub fn bed_to_speaker_mapping(&self) -> std::collections::HashMap<usize, usize> {
        // Bed channel ID → list of possible speaker name aliases (case-insensitive)
        let bed_aliases: [(usize, &[&str]); 10] = [
            (0, &["L", "FL", "FrontLeft", "LeftFront"]),
            (1, &["R", "FR", "FrontRight", "RightFront"]),
            (2, &["C", "FC", "Center", "Centre"]),
            (3, &["LFE", "Sub", "Subwoofer", "SW"]),
            (4, &["Ls", "SL", "LeftSurround", "SurroundLeft"]),
            (5, &["Rs", "SR", "RightSurround", "SurroundRight"]),
            (
                6,
                &[
                    "Lb", "BL", "Lrs", "BackLeft", "LeftBack", "RearLeft", "LeftRear",
                ],
            ),
            (
                7,
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
                8,
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
                9,
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

        let mut mapping = std::collections::HashMap::new();

        for (bed_id, aliases) in &bed_aliases {
            // Find speaker matching any alias (case-insensitive)
            for (speaker_idx, speaker) in self.speakers.iter().enumerate() {
                let speaker_name_lower = speaker.name.to_lowercase();
                for alias in *aliases {
                    if speaker_name_lower == alias.to_lowercase() {
                        mapping.insert(*bed_id, speaker_idx);
                        break;
                    }
                }
                if mapping.contains_key(bed_id) {
                    break;
                }
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
            _ => anyhow::bail!(
                "Unknown preset layout: '{}'. Available: stereo, 5.1, 7.1, 7.1.4, 9.1.6",
                name
            ),
        }
    }

    /// ITU-R BS.775 stereo layout (±30°)
    pub fn preset_stereo() -> Result<Self> {
        Self::from_speakers(vec![
            speaker_with_distance("L", -26.565052, 0.0, 2.236068),
            speaker_with_distance("R", 26.565052, 0.0, 2.236068),
            Speaker::new("Top", 0.0, 90.0), // Dummy for 3D triangulation
        ])
    }

    /// ITU-R BS.775 5.1 layout
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

    /// ITU-R BS.775 7.1 layout
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

    /// 7.1.4 spatial audio layout (ITU-R BS.2051-3 Config 4+5+0)
    pub fn preset_7_1_4() -> Result<Self> {
        Self::from_speakers(vec![
            // Bed layer (7.1)
            speaker_with_distance("FL", -26.565052, 0.0, 2.236068),
            speaker_with_distance("FR", 26.565052, 0.0, 2.236068),
            speaker_with_distance("C", 0.0, 0.0, 2.0),
            speaker_with_distance("LFE", 26.565052, -12.6043825, 2.291288),
            speaker_with_distance("BL", -153.43495, 0.0, 2.236068),
            speaker_with_distance("BR", 153.43495, 0.0, 2.236068),
            speaker_with_distance("SL", -90.0, 0.0, 1.0),
            speaker_with_distance("SR", 90.0, 0.0, 1.0),
            // Height layer (4 speakers at 45° elevation)
            speaker_with_distance("TFL", -45.0, 35.26439, 1.7320508),
            speaker_with_distance("TFR", 45.0, 35.26439, 1.7320508),
            speaker_with_distance("TBL", -135.0, 35.26439, 1.7320508),
            speaker_with_distance("TBR", 135.0, 35.26439, 1.7320508),
        ])
    }

    /// 9.1.6 spatial audio layout (ITU-R BS.2051-3 Config 6+4+0)
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

    #[test]
    fn test_speaker_creation() {
        let speaker = Speaker::new("FL", -30.0, 0.0);
        assert_eq!(speaker.name, "FL");
        assert_eq!(speaker.azimuth, -30.0);
        assert_eq!(speaker.elevation, 0.0);
        assert!(speaker.validate().is_ok());
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

        let positions = layout.positions();
        assert_eq!(positions.len(), 12);

        // Check first speaker (FL)
        assert_eq!(positions[0], [-30.0, 0.0]);

        // Check height speaker (TFL)
        assert_eq!(positions[8], [-30.0, 45.0]);
    }

    #[test]
    fn test_positions_extraction() {
        let layout = SpeakerLayout::preset("5.1").unwrap();
        let positions = layout.positions();

        assert_eq!(positions.len(), 6);
        assert_eq!(positions[0], [-30.0, 0.0]); // FL
        assert_eq!(positions[1], [30.0, 0.0]); // FR
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
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("layouts")
            .join(name)
    }

    #[test]
    fn test_load_5_1_yaml() {
        let layout = SpeakerLayout::from_file(layout_path("5.1.yaml"));
        assert!(
            layout.is_ok(),
            "Failed to load 5.1.yaml: {:?}",
            layout.err()
        );

        let layout = layout.unwrap();
        assert_eq!(layout.num_speakers(), 6);
    }

    #[test]
    fn test_load_7_1_4_yaml() {
        let layout = SpeakerLayout::from_file(layout_path("7.1.4.yaml"));
        assert!(
            layout.is_ok(),
            "Failed to load 7.1.4.yaml: {:?}",
            layout.err()
        );

        let layout = layout.unwrap();
        assert_eq!(layout.num_speakers(), 12);
    }

    #[test]
    fn test_load_9_1_6_yaml() {
        let layout = SpeakerLayout::from_file(layout_path("9.1.6.yaml"));
        assert!(
            layout.is_ok(),
            "Failed to load 9.1.6.yaml: {:?}",
            layout.err()
        );

        let layout = layout.unwrap();
        assert_eq!(layout.num_speakers(), 16);
    }
}
