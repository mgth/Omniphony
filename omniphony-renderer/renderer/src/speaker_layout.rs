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
//! let layout = SpeakerLayout::from_file("layouts/7.1.4.yaml")?;
//! println!("Loaded {} speakers", layout.num_speakers());
//!
//! // Get positions for VBAP
//! let positions = layout.positions();
//! ```

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

/// A single speaker in the layout
#[derive(Debug, Clone, Deserialize, Serialize)]
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
    #[serde(default = "default_distance")]
    pub distance: f32,

    /// Whether this speaker participates in VBAP spatialization
    /// Set to false for LFE/subwoofers (default: true)
    #[serde(default = "default_spatialize")]
    pub spatialize: bool,

    /// Per-speaker output delay in milliseconds (default: 0.0).
    #[serde(default = "default_delay_ms")]
    pub delay_ms: f32,
}

fn default_distance() -> f32 {
    1.0
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

impl Speaker {
    /// Create a new speaker (spatialize defaults to true)
    pub fn new(name: impl Into<String>, azimuth: f32, elevation: f32) -> Self {
        Self {
            name: name.into(),
            azimuth,
            elevation,
            distance: 1.0,
            spatialize: true,
            delay_ms: 0.0,
        }
    }

    /// Create a new speaker with explicit spatialize flag
    pub fn new_with_spatialize(
        name: impl Into<String>,
        azimuth: f32,
        elevation: f32,
        spatialize: bool,
    ) -> Self {
        Self {
            name: name.into(),
            azimuth,
            elevation,
            distance: 1.0,
            spatialize,
            delay_ms: 0.0,
        }
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
            Speaker::new("L", -30.0, 0.0),
            Speaker::new("R", 30.0, 0.0),
            Speaker::new("Top", 0.0, 90.0), // Dummy for 3D triangulation
        ])
    }

    /// ITU-R BS.775 5.1 layout
    pub fn preset_5_1() -> Result<Self> {
        Self::from_speakers(vec![
            Speaker::new("FL", -30.0, 0.0),
            Speaker::new("FR", 30.0, 0.0),
            Speaker::new("C", 0.0, 0.0),
            Speaker::new("LFE", 0.0, 0.0), // Same as center for VBAP
            Speaker::new("BL", -110.0, 0.0),
            Speaker::new("BR", 110.0, 0.0),
        ])
    }

    /// ITU-R BS.775 7.1 layout
    pub fn preset_7_1() -> Result<Self> {
        Self::from_speakers(vec![
            Speaker::new("FL", -30.0, 0.0),
            Speaker::new("FR", 30.0, 0.0),
            Speaker::new("C", 0.0, 0.0),
            Speaker::new("LFE", 0.0, 0.0),
            Speaker::new("BL", -145.0, 0.0),
            Speaker::new("BR", 145.0, 0.0),
            Speaker::new("SL", -90.0, 0.0),
            Speaker::new("SR", 90.0, 0.0),
        ])
    }

    /// 7.1.4 spatial audio layout (ITU-R BS.2051-3 Config 4+5+0)
    pub fn preset_7_1_4() -> Result<Self> {
        Self::from_speakers(vec![
            // Bed layer (7.1)
            Speaker::new("FL", -30.0, 0.0),
            Speaker::new("FR", 30.0, 0.0),
            Speaker::new("C", 0.0, 0.0),
            Speaker::new("LFE", 0.0, 0.0),
            Speaker::new("BL", -145.0, 0.0),
            Speaker::new("BR", 145.0, 0.0),
            Speaker::new("SL", -90.0, 0.0),
            Speaker::new("SR", 90.0, 0.0),
            // Height layer (4 speakers at 45° elevation)
            Speaker::new("TFL", -30.0, 45.0),
            Speaker::new("TFR", 30.0, 45.0),
            Speaker::new("TBL", -135.0, 45.0),
            Speaker::new("TBR", 135.0, 45.0),
        ])
    }

    /// 9.1.6 spatial audio layout (ITU-R BS.2051-3 Config 6+4+0)
    pub fn preset_9_1_6() -> Result<Self> {
        Self::from_speakers(vec![
            // Bed layer (9.1)
            Speaker::new("FL", -30.0, 0.0),
            Speaker::new("FR", 30.0, 0.0),
            Speaker::new("C", 0.0, 0.0),
            Speaker::new("LFE", 0.0, 0.0),
            Speaker::new("BL", -135.0, 0.0),
            Speaker::new("BR", 135.0, 0.0),
            Speaker::new("SL", -90.0, 0.0),
            Speaker::new("SR", 90.0, 0.0),
            Speaker::new("FWL", -60.0, 0.0),
            Speaker::new("FWR", 60.0, 0.0),
            // Height layer (6 speakers)
            Speaker::new("TFL", -30.0, 45.0),
            Speaker::new("TFR", 30.0, 45.0),
            Speaker::new("TSL", -90.0, 45.0),
            Speaker::new("TSR", 90.0, 45.0),
            Speaker::new("TBL", -135.0, 45.0),
            Speaker::new("TBR", 135.0, 45.0),
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
