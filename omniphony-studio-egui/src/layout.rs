//! Speaker layout loader for the Studio `layouts/*.yaml` format.

use std::path::Path;

use serde::Deserialize;

use crate::scene::Speaker;

#[derive(Deserialize)]
struct LayoutFile {
    #[serde(default)]
    name: String,
    #[serde(default)]
    speakers: Vec<SpeakerEntry>,
}

#[derive(Deserialize)]
struct SpeakerEntry {
    name: String,
    #[serde(default)]
    coord_mode: String,
    #[serde(default)]
    x: f32,
    #[serde(default)]
    y: f32,
    #[serde(default)]
    z: f32,
    #[serde(default)]
    azimuth: f32,
    #[serde(default)]
    elevation: f32,
    #[serde(default = "one")]
    distance: f32,
    #[serde(default = "yes")]
    spatialize: bool,
}

fn one() -> f32 {
    1.0
}
fn yes() -> bool {
    true
}

pub fn load(path: &Path) -> Result<(String, Vec<Speaker>), String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let file: LayoutFile = serde_yaml_ng::from_str(&text)
        .map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
    let speakers = file
        .speakers
        .into_iter()
        .map(|s| {
            let pos = if s.coord_mode.eq_ignore_ascii_case("spherical") {
                let (x, y, z) =
                    omniphony_geometry::f32::from_spherical(s.azimuth, s.elevation, s.distance);
                [x, y, z]
            } else {
                [s.x, s.y, s.z]
            };
            Speaker {
                name: s.name,
                pos,
                spatialize: s.spatialize,
            }
        })
        .collect();
    let name = if file.name.is_empty() {
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        file.name
    };
    Ok((name, speakers))
}
