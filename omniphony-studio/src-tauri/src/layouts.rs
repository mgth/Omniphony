use serde::{Deserialize, Serialize};
use std::path::Path;

// ── types ─────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Speaker {
    pub id: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    #[serde(rename = "azimuthDeg", default)]
    pub azimuth_deg: f64,
    #[serde(rename = "elevationDeg", default)]
    pub elevation_deg: f64,
    #[serde(rename = "distanceM", default = "default_distance_m")]
    pub distance_m: f64,
    #[serde(rename = "coordMode", default = "default_coord_mode")]
    pub coord_mode: String,
    #[serde(default = "default_spatialize")]
    pub spatialize: u8,
    #[serde(default)]
    pub delay_ms: f64,
    #[serde(rename = "freqLow", default, skip_serializing_if = "Option::is_none")]
    pub freq_low: Option<f32>,
    #[serde(rename = "freqHigh", default, skip_serializing_if = "Option::is_none")]
    pub freq_high: Option<f32>,
}

fn default_radius_m() -> f64 {
    1.0
}
fn default_spatialize() -> u8 {
    1
}

fn default_distance_m() -> f64 {
    1.0
}

fn default_coord_mode() -> String {
    "polar".to_string()
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Layout {
    pub key: String,
    pub name: String,
    pub speakers: Vec<Speaker>,
    /// Physical radius of the speaker array in metres.
    /// Used by the visualizer to convert normalised distances to real delays.
    /// Defaults to 1.0 when absent from the layout file.
    #[serde(default = "default_radius_m")]
    pub radius_m: f64,
}

#[derive(Serialize)]
struct ExportSpeaker<'a> {
    name: &'a str,
    coord_mode: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    x: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    y: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    z: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    azimuth: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    elevation: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    distance: Option<f64>,
    spatialize: bool,
    delay_ms: f64,
    #[serde(rename = "freqLow", skip_serializing_if = "Option::is_none")]
    freq_low: Option<f32>,
    #[serde(rename = "freqHigh", skip_serializing_if = "Option::is_none")]
    freq_high: Option<f32>,
}

#[derive(Serialize)]
struct ExportLayout<'a> {
    name: &'a str,
    radius_m: f64,
    speakers: Vec<ExportSpeaker<'a>>,
}

fn yaml_quote(value: &str) -> String {
    format!("{:?}", value)
}

fn format_layout_as_yaml(layout: &ExportLayout<'_>) -> String {
    let mut text = String::new();
    text.push_str(&format!("name: {}\n", yaml_quote(layout.name)));
    text.push_str(&format!("radius_m: {}\n", layout.radius_m));
    text.push_str("speakers:\n");
    for speaker in &layout.speakers {
        text.push_str(&format!("  - name: {}\n", yaml_quote(speaker.name)));
        text.push_str(&format!("    coord_mode: {}\n", speaker.coord_mode));
        if let Some(x) = speaker.x {
            text.push_str(&format!("    x: {}\n", x));
        }
        if let Some(y) = speaker.y {
            text.push_str(&format!("    y: {}\n", y));
        }
        if let Some(z) = speaker.z {
            text.push_str(&format!("    z: {}\n", z));
        }
        if let Some(azimuth) = speaker.azimuth {
            text.push_str(&format!("    azimuth: {}\n", azimuth));
        }
        if let Some(elevation) = speaker.elevation {
            text.push_str(&format!("    elevation: {}\n", elevation));
        }
        if let Some(distance) = speaker.distance {
            text.push_str(&format!("    distance: {}\n", distance));
        }
        text.push_str(&format!(
            "    spatialize: {}\n",
            if speaker.spatialize { "true" } else { "false" }
        ));
        text.push_str(&format!("    delay_ms: {}\n", speaker.delay_ms));
        if let Some(freq_low) = speaker.freq_low {
            text.push_str(&format!("    freq_low: {}\n", freq_low));
        }
        if let Some(freq_high) = speaker.freq_high {
            text.push_str(&format!("    freq_high: {}\n", freq_high));
        }
    }
    text
}

// ── helpers ───────────────────────────────────────────────────────────────

fn clamp(v: f64, min: f64, max: f64) -> f64 {
    v.max(min).min(max)
}

fn spherical_to_cartesian(az_deg: f64, el_deg: f64, dist: f64) -> (f64, f64, f64) {
    let az = az_deg.to_radians();
    let el = el_deg.to_radians();
    (
        dist * el.cos() * az.cos(),
        dist * el.sin(),
        dist * el.cos() * az.sin(),
    )
}

fn cartesian_to_spherical(x: f64, y: f64, z: f64) -> (f64, f64, f64) {
    let dist = (x * x + y * y + z * z).sqrt();
    let az = z.atan2(x).to_degrees();
    let el = if dist > 0.0 {
        y.atan2((x * x + z * z).sqrt()).to_degrees()
    } else {
        0.0
    };
    (az, el, dist)
}

// ── raw deserialization types ─────────────────────────────────────────────

#[derive(Deserialize, Debug, Default)]
struct RawSpeaker {
    #[serde(default)]
    id: Option<serde_json::Value>,
    #[serde(default)]
    name: Option<serde_json::Value>,
    #[serde(default)]
    x: Option<f64>,
    #[serde(default)]
    y: Option<f64>,
    #[serde(default)]
    z: Option<f64>,
    #[serde(default)]
    azimuth: Option<f64>,
    #[serde(default)]
    az: Option<f64>,
    #[serde(default)]
    elevation: Option<f64>,
    #[serde(default)]
    el: Option<f64>,
    #[serde(default)]
    distance: Option<f64>,
    #[serde(default)]
    dist: Option<f64>,
    #[serde(default)]
    coord_mode: Option<String>,
    #[serde(default)]
    coordinate_mode: Option<String>,
    #[serde(default, rename = "coordMode")]
    coord_mode_camel: Option<String>,
    #[serde(default)]
    delay_ms: Option<f64>,
    #[serde(default)]
    delay: Option<f64>,
    #[serde(default)]
    spatialize: Option<serde_json::Value>,
    #[serde(default, rename = "freqLow")]
    freq_low: Option<f64>,
    #[serde(default, rename = "freq_low")]
    freq_low_snake: Option<f64>,
    #[serde(default, rename = "freqHigh")]
    freq_high: Option<f64>,
    #[serde(default, rename = "freq_high")]
    freq_high_snake: Option<f64>,
}

#[derive(Deserialize, Debug, Default)]
struct RawLayout {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    radius_m: Option<f64>,
    #[serde(default)]
    speakers: Vec<RawSpeaker>,
}

fn normalize_speaker(raw: RawSpeaker) -> Speaker {
    let id = {
        let v = raw.id.or(raw.name);
        match v {
            Some(serde_json::Value::String(s)) => s,
            Some(serde_json::Value::Number(n)) => n.to_string(),
            _ => "spk".to_string(),
        }
    };

    let delay_ms = raw.delay_ms.or(raw.delay).unwrap_or(0.0).max(0.0);
    let coord_mode = raw
        .coord_mode
        .or(raw.coordinate_mode)
        .or(raw.coord_mode_camel)
        .unwrap_or_else(|| {
            if raw.x.is_some() && raw.y.is_some() && raw.z.is_some() {
                "cartesian".to_string()
            } else {
                "polar".to_string()
            }
        })
        .to_ascii_lowercase();
    let coord_mode = if coord_mode == "cartesian" {
        "cartesian".to_string()
    } else {
        "polar".to_string()
    };
    let spatialize = match raw.spatialize {
        Some(serde_json::Value::Bool(v)) => {
            if v {
                1
            } else {
                0
            }
        }
        Some(serde_json::Value::Number(v)) => {
            if v.as_f64().unwrap_or(1.0) != 0.0 {
                1
            } else {
                0
            }
        }
        Some(serde_json::Value::String(v)) => {
            if v == "0" || v.eq_ignore_ascii_case("false") {
                0
            } else {
                1
            }
        }
        _ => 1,
    };

    let freq_low = raw
        .freq_low
        .or(raw.freq_low_snake)
        .map(|v| v.max(0.0) as f32)
        .filter(|&v| v > 0.0);
    let freq_high = raw
        .freq_high
        .or(raw.freq_high_snake)
        .map(|v| v.max(0.0) as f32)
        .filter(|&v| v > 0.0);

    if let (Some(x), Some(y), Some(z)) = (raw.x, raw.y, raw.z) {
        let x = clamp(x, -1.0, 1.0);
        let y = clamp(y, -1.0, 1.0);
        let z = clamp(z, -1.0, 1.0);
        let (derived_az, derived_el, derived_dist) = cartesian_to_spherical(x, y, z);
        return Speaker {
            id,
            x,
            y,
            z,
            azimuth_deg: raw.azimuth.or(raw.az).unwrap_or(derived_az),
            elevation_deg: raw.elevation.or(raw.el).unwrap_or(derived_el),
            distance_m: raw.distance.or(raw.dist).unwrap_or(derived_dist).max(0.01),
            coord_mode,
            spatialize,
            delay_ms,
            freq_low,
            freq_high,
        };
    }

    let az = raw.azimuth.or(raw.az).unwrap_or(0.0);
    let el = raw.elevation.or(raw.el).unwrap_or(0.0);
    let dist = raw.distance.or(raw.dist).unwrap_or(1.0).max(0.01);
    let (x, y, z) = spherical_to_cartesian(az, el, dist);

    Speaker {
        id,
        x: clamp(x, -1.0, 1.0),
        y: clamp(y, -1.0, 1.0),
        z: clamp(z, -1.0, 1.0),
        azimuth_deg: az,
        elevation_deg: el,
        distance_m: dist,
        coord_mode,
        spatialize,
        delay_ms,
        freq_low,
        freq_high,
    }
}

// ── YAML parser (minimal, mirrors layouts.js implementation) ──────────────

fn parse_yaml_value(raw: &str) -> serde_json::Value {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return serde_json::Value::String(String::new());
    }
    if (trimmed.starts_with('"') && trimmed.ends_with('"'))
        || (trimmed.starts_with('\'') && trimmed.ends_with('\''))
    {
        return serde_json::Value::String(trimmed[1..trimmed.len() - 1].to_string());
    }
    if trimmed == "true" {
        return serde_json::Value::Bool(true);
    }
    if trimmed == "false" {
        return serde_json::Value::Bool(false);
    }
    if let Ok(n) = trimmed.parse::<f64>() {
        if let Some(v) = serde_json::Number::from_f64(n) {
            return serde_json::Value::Number(v);
        }
    }
    serde_json::Value::String(trimmed.to_string())
}

fn parse_yaml_layout(text: &str) -> RawLayout {
    let mut speakers: Vec<serde_json::Map<String, serde_json::Value>> = Vec::new();
    let mut current: Option<serde_json::Map<String, serde_json::Value>> = None;
    let mut in_speakers_block = false;
    let mut top_level: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();

    for line in text.lines() {
        // strip inline comments
        let without_comment = {
            // find whitespace followed by '#'
            let mut result = line;
            let bytes = line.as_bytes();
            for i in 0..bytes.len() {
                if bytes[i] == b'#' && i > 0 && (bytes[i - 1] == b' ' || bytes[i - 1] == b'\t') {
                    result = &line[..i];
                    break;
                }
            }
            result
        };
        let trimmed = without_comment.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "speakers:" {
            in_speakers_block = true;
            if let Some(c) = current.take() {
                speakers.push(c);
            }
            continue;
        }
        if !in_speakers_block {
            // Capture top-level scalars (name, radius_m, …).
            if let Some(sep) = trimmed.find(':') {
                let key = trimmed[..sep].trim().to_string();
                let val = parse_yaml_value(&trimmed[sep + 1..]);
                top_level.insert(key, val);
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("- ") {
            if let Some(c) = current.take() {
                speakers.push(c);
            }
            let mut map = serde_json::Map::new();
            let rest = rest.trim();
            if !rest.is_empty() {
                if let Some(sep) = rest.find(':') {
                    let key = rest[..sep].trim().to_string();
                    let val = parse_yaml_value(&rest[sep + 1..]);
                    map.insert(key, val);
                }
            }
            current = Some(map);
            continue;
        }
        if let Some(map) = current.as_mut() {
            if let Some(sep) = trimmed.find(':') {
                let key = trimmed[..sep].trim().to_string();
                let val = parse_yaml_value(&trimmed[sep + 1..]);
                map.insert(key, val);
            }
        }
    }
    if let Some(c) = current.take() {
        speakers.push(c);
    }

    let raw_speakers = speakers
        .into_iter()
        .map(|map| serde_json::from_value(serde_json::Value::Object(map)).unwrap_or_default())
        .collect();

    let name = top_level
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let radius_m = top_level.get("radius_m").and_then(|v| v.as_f64());

    RawLayout {
        name,
        radius_m,
        speakers: raw_speakers,
    }
}

// ── public API ────────────────────────────────────────────────────────────

pub fn load_layouts(layouts_dir: &Path) -> Vec<Layout> {
    if !layouts_dir.exists() {
        return Vec::new();
    }

    let entries = match std::fs::read_dir(layouts_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut files: Vec<std::path::PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            matches!(
                p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_lowercase())
                    .as_deref(),
                Some("json") | Some("yaml") | Some("yml")
            )
        })
        .collect();

    files.sort_by_key(|p| p.file_name().map(|n| n.to_os_string()));

    // detect duplicate stems
    let mut stem_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for path in &files {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let stem_key = format!("{stem}.{ext}");
        *stem_counts.entry(stem.clone()).or_insert(0) += 1;
        let _ = stem_key;
    }

    let mut layouts: Vec<Layout> = files
        .iter()
        .filter_map(|path| {
            let ext = path.extension().and_then(|e| e.to_str())?.to_lowercase();
            let stem = path.file_stem().and_then(|s| s.to_str())?.to_string();
            let text = std::fs::read_to_string(path).ok()?;

            let raw: RawLayout = if ext == "json" {
                serde_json::from_str(&text).unwrap_or_default()
            } else {
                parse_yaml_layout(&text)
            };

            let has_dup = *stem_counts.get(&stem).unwrap_or(&0) > 1;
            let key = if has_dup {
                format!("{stem}-{ext}")
            } else {
                stem.clone()
            };
            let name = raw.name.clone().unwrap_or_else(|| {
                if has_dup {
                    format!("{stem} ({ext})")
                } else {
                    stem.clone()
                }
            });

            let speakers = raw.speakers.into_iter().map(normalize_speaker).collect();
            let radius_m = raw.radius_m.unwrap_or(1.0).max(0.01);
            Some(Layout {
                key,
                name,
                speakers,
                radius_m,
            })
        })
        .collect();

    layouts.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    layouts
}

pub fn load_layout_file(path: &Path) -> Option<Layout> {
    let ext = path.extension().and_then(|e| e.to_str())?.to_lowercase();
    if ext != "json" && ext != "yaml" && ext != "yml" {
        return None;
    }
    let stem = path.file_stem().and_then(|s| s.to_str())?.to_string();
    let text = std::fs::read_to_string(path).ok()?;
    let raw: RawLayout = if ext == "json" {
        serde_json::from_str(&text).unwrap_or_default()
    } else {
        parse_yaml_layout(&text)
    };
    let name = raw.name.clone().unwrap_or_else(|| stem.clone());
    let speakers = raw.speakers.into_iter().map(normalize_speaker).collect();
    let radius_m = raw.radius_m.unwrap_or(1.0).max(0.01);
    Some(Layout {
        key: stem,
        name,
        speakers,
        radius_m,
    })
}

pub fn save_layout_file(path: &Path, layout: &Layout) -> Result<(), String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let export = ExportLayout {
        name: &layout.name,
        radius_m: layout.radius_m.max(0.01),
        speakers: layout
            .speakers
            .iter()
            .map(|speaker| {
                let cartesian = speaker.coord_mode.eq_ignore_ascii_case("cartesian");
                ExportSpeaker {
                    name: &speaker.id,
                    coord_mode: if cartesian { "cartesian" } else { "polar" },
                    x: if cartesian {
                        Some(clamp(speaker.x, -1.0, 1.0))
                    } else {
                        None
                    },
                    y: if cartesian {
                        Some(clamp(speaker.y, -1.0, 1.0))
                    } else {
                        None
                    },
                    z: if cartesian {
                        Some(clamp(speaker.z, -1.0, 1.0))
                    } else {
                        None
                    },
                    azimuth: if cartesian {
                        None
                    } else {
                        Some(speaker.azimuth_deg)
                    },
                    elevation: if cartesian {
                        None
                    } else {
                        Some(speaker.elevation_deg)
                    },
                    distance: if cartesian {
                        None
                    } else {
                        Some(speaker.distance_m.max(0.01))
                    },
                    spatialize: speaker.spatialize != 0,
                    delay_ms: speaker.delay_ms.max(0.0),
                    freq_low: speaker.freq_low,
                    freq_high: speaker.freq_high,
                }
            })
            .collect(),
    };

    let text = match ext.as_str() {
        "json" => serde_json::to_string_pretty(&export)
            .map_err(|e| format!("failed to serialize layout: {e}"))?,
        "yaml" | "yml" => format_layout_as_yaml(&export),
        _ => return Err("supported export formats are .yaml, .yml and .json".to_string()),
    };
    std::fs::write(path, text).map_err(|e| format!("failed to write layout file: {e}"))?;
    Ok(())
}

pub fn build_live_layout_from_cache(
    speakers: &std::collections::BTreeMap<u32, crate::app_state::LiveSpeakerConfig>,
    expected_count: Option<u32>,
) -> Option<Layout> {
    if speakers.is_empty() {
        return None;
    }

    if let Some(count) = expected_count {
        if count == 0 {
            return None;
        }
        for index in 0..count {
            if !speakers.contains_key(&index) {
                return None;
            }
        }
    }

    let max_index = expected_count
        .map(|count| count.saturating_sub(1))
        .or_else(|| speakers.keys().next_back().copied())
        .unwrap_or(0);

    let spk_list = (0..=max_index)
        .filter_map(|index| {
            speakers.get(&index).map(|speaker| Speaker {
                id: speaker.name.clone(),
                x: clamp(speaker.x, -1.0, 1.0),
                y: clamp(speaker.y, -1.0, 1.0),
                z: clamp(speaker.z, -1.0, 1.0),
                azimuth_deg: speaker.azimuth_deg,
                elevation_deg: speaker.elevation_deg,
                distance_m: speaker.distance_m.max(0.01),
                coord_mode: if speaker.coord_mode.eq_ignore_ascii_case("cartesian") {
                    "cartesian".to_string()
                } else {
                    "polar".to_string()
                },
                spatialize: speaker.spatialize,
                delay_ms: speaker.delay_ms,
                freq_low: speaker.freq_low,
                freq_high: speaker.freq_high,
            })
        })
        .collect::<Vec<_>>();

    if spk_list.is_empty() {
        return None;
    }

    Some(Layout {
        key: "omniphony-live".to_string(),
        name: "omniphony (live)".to_string(),
        speakers: spk_list,
        radius_m: 1.0,
    })
}

#[cfg(test)]
mod tests {
    use super::{normalize_speaker, parse_yaml_layout, RawSpeaker};

    #[test]
    fn normalizes_freq_fields_from_json_variants() {
        let speaker = normalize_speaker(RawSpeaker {
            name: Some(serde_json::json!("L")),
            freq_low: Some(80.0),
            freq_high_snake: Some(18000.0),
            ..RawSpeaker::default()
        });
        assert_eq!(speaker.freq_low, Some(80.0));
        assert_eq!(speaker.freq_high, Some(18000.0));
    }

    #[test]
    fn normalizes_non_positive_freq_fields_to_none() {
        let speaker = normalize_speaker(RawSpeaker {
            name: Some(serde_json::json!("L")),
            freq_low: Some(0.0),
            freq_high: Some(-10.0),
            ..RawSpeaker::default()
        });
        assert_eq!(speaker.freq_low, None);
        assert_eq!(speaker.freq_high, None);
    }

    #[test]
    fn parses_yaml_freq_fields() {
        let raw = parse_yaml_layout(
            r#"
name: "test"
speakers:
  - name: "L"
    azimuth: 30
    elevation: 0
    distance: 1
    spatialize: true
    delay_ms: 0
    freq_low: 80
    freq_high: 12000
"#,
        );
        let speaker = normalize_speaker(raw.speakers.into_iter().next().unwrap());
        assert_eq!(speaker.freq_low, Some(80.0));
        assert_eq!(speaker.freq_high, Some(12000.0));
    }
}
