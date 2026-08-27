use omniphony_geometry::f64 as geometry;
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

#[derive(Deserialize, Debug, Clone)]
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

// `crossoverCutoffs` is derived, not stored, so it cannot go stale. Every path
// that ships a layout — the state bundle, a layout change, a speaker's
// frequency being edited — serializes through here and gets the current
// answer. Storing it as a field would mean remembering to refresh it at each
// of those, and forgetting one is invisible until a band edge is wrong.
impl Serialize for Layout {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("Layout", 5)?;
        state.serialize_field("key", &self.key)?;
        state.serialize_field("name", &self.name)?;
        state.serialize_field("speakers", &self.speakers)?;
        state.serialize_field("radius_m", &self.radius_m)?;
        state.serialize_field("crossoverCutoffs", &crossover_cutoffs(&self.speakers))?;
        state.end()
    }
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

// Coordinate conversions come from `omniphony-geometry`, which the renderer
// shares. They used to live here, and applied the Three.js scene-space formula
// (`az = atan2(z, x)`, elevation off +Y) to layout files written in the ADM
// frame the renderer uses (`az = atan2(x, y)`, elevation off +Z). The axes were
// never swizzled, so the two disagreed: `FR` in layouts/7.1.4.yaml — x=1, y=1,
// z=0, front-right at ear level — parsed as azimuth 0°, elevation 45°.

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
        let (derived_az, derived_el, derived_dist) = geometry::hydrate_from_cartesian(x, y, z);
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
    let (x, y, z) = geometry::hydrate_from_spherical(az, el, dist);

    Speaker {
        id,
        x,
        y,
        z,
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

/// Interior crossover band edges for a speaker set, in Hz: every distinct
/// cutoff a spatialized speaker declares, sorted.
///
/// The bands themselves are `[0, ...cutoffs, +inf]`. Only the interior edges
/// are published because JSON has no infinity — the frontend caps the ends.
///
/// Non-spatialized speakers are skipped: an LFE's low-pass is not a band
/// boundary for the objects being panned, and counting it would split the
/// heatmap on an edge nothing is rendered across.
pub fn crossover_cutoffs(speakers: &[Speaker]) -> Vec<f64> {
    let mut cutoffs: Vec<f64> = speakers
        .iter()
        .filter(|speaker| speaker.spatialize != 0)
        .flat_map(|speaker| [speaker.freq_low, speaker.freq_high])
        .flatten()
        .map(f64::from)
        .filter(|value| *value > 0.0)
        .collect();
    cutoffs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    // Two speakers meeting at "the same" cutoff rarely agree to the last
    // decimal; anything closer than 0.1 Hz is one edge, not two.
    cutoffs.dedup_by(|a, b| (*a - *b).abs() < 0.1);
    cutoffs
}

/// Default export file name for a speaker set, as `spatialized.non.height`.
///
/// The Studio's naming convention: how many spatialized speakers sit at or
/// below ear level, how many are excluded from spatialization (LFE), and how
/// many are overhead. A 7.1.4 layout exports as `7.1.4`.
///
/// The height test is on the ADM z axis, so it is the same "is this a height
/// speaker" question the renderer asks — not a name match.
pub fn default_export_name(speakers: &[Speaker]) -> String {
    let mut ear_level = 0;
    let mut non_spatialized = 0;
    let mut height = 0;
    for speaker in speakers {
        if speaker.spatialize == 0 {
            non_spatialized += 1;
        } else if speaker.z > HEIGHT_SPEAKER_Z {
            height += 1;
        } else {
            ear_level += 1;
        }
    }
    format!("{ear_level}.{non_spatialized}.{height}")
}

/// Above this normalised height a spatialized speaker counts as overhead for
/// the export name.
const HEIGHT_SPEAKER_Z: f64 = 0.5;

/// Reduce a name to something safe to use as a file name.
///
/// Everything outside `[A-Za-z0-9._-]` becomes `_`, and leading/trailing dots
/// are stripped so the result cannot be a hidden file or trip an extension
/// check. An empty result falls back to `layout` rather than producing a file
/// with no name.
pub fn sanitize_export_name(name: &str) -> String {
    let sanitized: String = name
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = sanitized.trim_matches('.');
    if trimmed.is_empty() {
        "layout".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Bring a speaker coming from the editor into the shape the layout format
/// expects, before it is written out.
///
/// The frontend used to do this on its way to the export command, which meant
/// the rules for a valid stored speaker lived in two places — and the one that
/// mattered on *import* (`normalize_speaker`) was not the one applied on
/// export. Both now clamp the same way and derive the missing coordinate
/// representation through `omniphony-geometry`.
pub fn normalize_for_export(speaker: &mut Speaker) {
    speaker.x = clamp(speaker.x, -1.0, 1.0);
    speaker.y = clamp(speaker.y, -1.0, 1.0);
    speaker.z = clamp(speaker.z, -1.0, 1.0);
    speaker.delay_ms = speaker.delay_ms.max(0.0);
    speaker.freq_low = speaker.freq_low.filter(|v| *v > 0.0);
    speaker.freq_high = speaker.freq_high.filter(|v| *v > 0.0);
    speaker.coord_mode = if speaker.coord_mode.eq_ignore_ascii_case("cartesian") {
        "cartesian".to_string()
    } else {
        "polar".to_string()
    };

    // Derive whichever representation is not authoritative, so the two never
    // disagree in the written file.
    if speaker.coord_mode == "cartesian" {
        let (az, el, dist) = geometry::hydrate_from_cartesian(speaker.x, speaker.y, speaker.z);
        speaker.azimuth_deg = az;
        speaker.elevation_deg = el;
        speaker.distance_m = dist;
    } else {
        if !speaker.azimuth_deg.is_finite() {
            speaker.azimuth_deg = 0.0;
        }
        if !speaker.elevation_deg.is_finite() {
            speaker.elevation_deg = 0.0;
        }
        speaker.distance_m = if speaker.distance_m.is_finite() {
            speaker.distance_m.max(0.01)
        } else {
            1.0
        };
        let (x, y, z) = geometry::hydrate_from_spherical(
            speaker.azimuth_deg,
            speaker.elevation_deg,
            speaker.distance_m,
        );
        speaker.x = x;
        speaker.y = y;
        speaker.z = z;
    }
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

#[cfg(test)]
mod tests {
    use super::{
        crossover_cutoffs, default_export_name, load_layout_file, normalize_for_export,
        normalize_speaker, parse_yaml_layout, sanitize_export_name, save_layout_file, Layout,
        RawSpeaker, Speaker,
    };

    /// A cartesian speaker must derive its angles in the ADM frame the layout
    /// file is written in — the same one the renderer reads it with.
    ///
    /// `FR` from layouts/7.1.4.yaml is x=1, y=1, z=0: front-right, ear level.
    /// The scene-space formula this code used to carry read it as azimuth 0°
    /// (straight ahead) and elevation 45° (up), because it never swizzled the
    /// axes. Angles are only derived when the file omits them, and cartesian
    /// export drops them again, so the corruption surfaced on round-trip:
    /// `coord_mode` defaults to polar when the key is absent, and a layout
    /// carrying x/y/z but no `coord_mode` exported these derived angles.
    fn spk(id: &str, x: f64, y: f64, z: f64, spatialize: u8) -> Speaker {
        Speaker {
            id: id.to_string(),
            x,
            y,
            z,
            azimuth_deg: 0.0,
            elevation_deg: 0.0,
            distance_m: 1.0,
            coord_mode: "cartesian".to_string(),
            spatialize,
            delay_ms: 0.0,
            freq_low: None,
            freq_high: None,
        }
    }

    // ── crossover bands ─────────────────────────────────────────────────────

    fn banded(id: &str, low: Option<f32>, high: Option<f32>, spatialize: u8) -> Speaker {
        let mut speaker = spk(id, 0.0, 1.0, 0.0, spatialize);
        speaker.freq_low = low;
        speaker.freq_high = high;
        speaker
    }

    #[test]
    fn a_layout_with_no_crossover_has_no_edges() {
        let speakers = vec![spk("L", -1.0, 1.0, 0.0, 1), spk("R", 1.0, 1.0, 0.0, 1)];
        assert!(crossover_cutoffs(&speakers).is_empty());
    }

    #[test]
    fn edges_are_sorted_and_deduplicated() {
        let speakers = vec![
            banded("sub", None, Some(120.0), 1),
            banded("mid", Some(120.0), Some(4000.0), 1),
            banded("hi", Some(4000.0), None, 1),
        ];
        assert_eq!(crossover_cutoffs(&speakers), vec![120.0, 4000.0]);
    }

    /// Two speakers meeting at "the same" cutoff rarely agree to the last
    /// decimal. Within 0.1 Hz is one edge; beyond it is two.
    #[test]
    fn near_identical_cutoffs_collapse_to_one_edge() {
        let speakers = vec![
            banded("a", None, Some(120.0), 1),
            banded("b", Some(120.05), None, 1),
        ];
        assert_eq!(crossover_cutoffs(&speakers), vec![120.0]);

        let speakers = vec![
            banded("a", None, Some(120.0), 1),
            banded("b", Some(120.5), None, 1),
        ];
        assert_eq!(crossover_cutoffs(&speakers).len(), 2);
    }

    /// An LFE's low-pass is not a band boundary for the objects being panned.
    #[test]
    fn a_non_spatialized_speaker_contributes_no_edge() {
        let speakers = vec![
            banded("LFE", None, Some(120.0), 0),
            spk("L", -1.0, 1.0, 0.0, 1),
        ];
        assert!(crossover_cutoffs(&speakers).is_empty());
    }

    #[test]
    fn non_positive_cutoffs_are_ignored() {
        let speakers = vec![
            banded("a", Some(0.0), Some(-5.0), 1),
            banded("b", Some(80.0), None, 1),
        ];
        assert_eq!(crossover_cutoffs(&speakers), vec![80.0]);
    }

    /// A mixed layout: some speakers banded, some full-range. The full-range
    /// ones must not add edges, but must not suppress the others' either.
    #[test]
    fn a_mixed_layout_keeps_only_the_declared_edges() {
        let speakers = vec![
            spk("L", -1.0, 1.0, 0.0, 1),
            banded("sub", None, Some(80.0), 1),
            spk("R", 1.0, 1.0, 0.0, 1),
        ];
        assert_eq!(crossover_cutoffs(&speakers), vec![80.0]);
    }

    /// The cutoffs are derived at serialization, so a layout whose speakers
    /// changed since it was loaded still ships the right edges.
    #[test]
    fn serialization_reflects_an_edited_speaker() {
        let mut layout = Layout {
            key: "k".to_string(),
            name: "k".to_string(),
            radius_m: 1.0,
            speakers: vec![banded("a", None, Some(80.0), 1)],
        };
        let before = serde_json::to_value(&layout).unwrap();
        assert_eq!(before["crossoverCutoffs"], serde_json::json!([80.0]));

        layout.speakers[0].freq_high = Some(200.0);
        let after = serde_json::to_value(&layout).unwrap();
        assert_eq!(after["crossoverCutoffs"], serde_json::json!([200.0]));
    }

    // ── export name ─────────────────────────────────────────────────────────

    #[test]
    fn the_export_name_counts_ear_level_lfe_and_height() {
        // A 7.1.4: seven at ear level, one non-spatialized, four overhead.
        let mut speakers: Vec<Speaker> = (0..7)
            .map(|i| spk(&i.to_string(), 1.0, 0.0, 0.0, 1))
            .collect();
        speakers.push(spk("lfe", 0.0, 1.0, 0.0, 0));
        speakers.extend((0..4).map(|i| spk(&format!("t{i}"), 1.0, 0.0, 1.0, 1)));
        assert_eq!(default_export_name(&speakers), "7.1.4");
    }

    /// Height is decided on the z axis, not on the speaker's name — the same
    /// question the renderer asks.
    #[test]
    fn height_is_decided_by_position_not_name() {
        let speakers = vec![
            spk("TFL", 1.0, 1.0, 0.0, 1), // named like a top speaker, at ear level
            spk("plain", 0.0, 0.0, 0.9, 1),
        ];
        assert_eq!(default_export_name(&speakers), "1.0.1");
    }

    #[test]
    fn a_non_spatialized_speaker_never_counts_as_height() {
        let speakers = vec![spk("lfe", 0.0, 0.0, 1.0, 0)];
        assert_eq!(default_export_name(&speakers), "0.1.0");
    }

    #[test]
    fn an_empty_layout_still_names_something() {
        assert_eq!(default_export_name(&[]), "0.0.0");
    }

    // ── file-name sanitizing ────────────────────────────────────────────────

    #[test]
    fn sanitizing_keeps_safe_characters_and_replaces_the_rest() {
        assert_eq!(sanitize_export_name("7.1.4"), "7.1.4");
        assert_eq!(sanitize_export_name("my layout"), "my_layout");
        assert_eq!(sanitize_export_name("a/b\\c"), "a_b_c");
    }

    /// Leading and trailing dots would make a hidden file or confuse an
    /// extension check.
    #[test]
    fn sanitizing_strips_edge_dots_and_never_returns_empty() {
        assert_eq!(sanitize_export_name("..hidden.."), "hidden");
        assert_eq!(sanitize_export_name("..."), "layout");
        assert_eq!(sanitize_export_name("   "), "layout");
        // Unsafe characters become underscores, which is a usable name — the
        // fallback is only for a name that ends up genuinely empty. Matches the
        // frontend this replaces.
        assert_eq!(sanitize_export_name("///"), "___");
    }

    // ── export normalization ────────────────────────────────────────────────

    #[test]
    fn export_clamps_out_of_range_values() {
        let mut speaker = spk("s", 5.0, -9.0, 0.25, 1);
        speaker.delay_ms = -3.0;
        speaker.freq_low = Some(0.0);
        normalize_for_export(&mut speaker);
        assert_eq!(speaker.x, 1.0);
        assert_eq!(speaker.y, -1.0);
        assert_eq!(speaker.delay_ms, 0.0);
        assert_eq!(speaker.freq_low, None);
    }

    /// A cartesian speaker must get angles in the ADM frame, so the written
    /// file agrees with what the renderer reads back.
    #[test]
    fn export_derives_angles_for_a_cartesian_speaker() {
        let mut speaker = spk("FR", 1.0, 1.0, 0.0, 1);
        normalize_for_export(&mut speaker);
        assert!((speaker.azimuth_deg - 45.0).abs() < 1e-6);
        assert!(speaker.elevation_deg.abs() < 1e-6);
    }

    #[test]
    fn export_derives_cartesian_for_a_polar_speaker() {
        let mut speaker = spk("R", 0.0, 0.0, 0.0, 1);
        speaker.coord_mode = "polar".to_string();
        speaker.azimuth_deg = 90.0;
        speaker.distance_m = 1.0;
        normalize_for_export(&mut speaker);
        assert!((speaker.x - 1.0).abs() < 1e-6, "x was {}", speaker.x);
        assert!(speaker.y.abs() < 1e-6);
    }

    #[test]
    fn export_repairs_a_non_finite_polar_speaker() {
        let mut speaker = spk("s", 0.0, 0.0, 0.0, 1);
        speaker.coord_mode = "polar".to_string();
        speaker.azimuth_deg = f64::NAN;
        speaker.distance_m = f64::NAN;
        normalize_for_export(&mut speaker);
        assert_eq!(speaker.azimuth_deg, 0.0);
        assert_eq!(speaker.distance_m, 1.0);
        assert!(speaker.x.is_finite() && speaker.y.is_finite() && speaker.z.is_finite());
    }

    /// The acceptance criterion for this change: what the export writes can be
    /// read back as the same speaker set. Goes through the real file path, so
    /// the YAML formatter and the parser are both exercised.
    #[test]
    fn an_exported_layout_round_trips_through_the_parser() {
        let mut layout = Layout {
            key: "roundtrip".to_string(),
            name: "round trip".to_string(),
            radius_m: 1.5,
            speakers: vec![
                spk("FL", -1.0, 1.0, 0.0, 1),
                spk("FR", 1.0, 1.0, 0.0, 1),
                spk("LFE", 0.0, 1.0, -1.0, 0),
            ],
        };
        layout.speakers[0].freq_low = Some(80.0);
        layout.speakers[0].delay_ms = 2.5;
        for speaker in &mut layout.speakers {
            normalize_for_export(speaker);
        }

        let path = std::env::temp_dir().join("omniphony-export-roundtrip.yaml");
        save_layout_file(&path, &layout).expect("export must succeed");
        let reparsed = load_layout_file(&path).expect("exported YAML must parse");
        let _ = std::fs::remove_file(&path);

        assert_eq!(reparsed.speakers.len(), 3);
        assert!((reparsed.radius_m - 1.5).abs() < 1e-9);
        for (before, after) in layout.speakers.iter().zip(reparsed.speakers.iter()) {
            assert_eq!(before.id, after.id, "name did not survive");
            assert_eq!(before.spatialize, after.spatialize, "spatialize flipped");
            assert!((before.x - after.x).abs() < 1e-6, "x drifted");
            assert!((before.y - after.y).abs() < 1e-6, "y drifted");
            assert!((before.z - after.z).abs() < 1e-6, "z drifted");
            assert_eq!(before.freq_low, after.freq_low, "crossover edge lost");
            assert!(
                (before.delay_ms - after.delay_ms).abs() < 1e-9,
                "delay lost"
            );
        }
    }

    #[test]
    fn derives_speaker_angles_in_the_adm_frame() {
        let speaker = normalize_speaker(RawSpeaker {
            name: Some(serde_json::json!("FR")),
            x: Some(1.0),
            y: Some(1.0),
            z: Some(0.0),
            ..RawSpeaker::default()
        });
        assert!(
            (speaker.azimuth_deg - 45.0).abs() < 1e-6,
            "azimuth {} should be 45° (front-right), not 0°",
            speaker.azimuth_deg
        );
        assert!(
            speaker.elevation_deg.abs() < 1e-6,
            "elevation {} should be 0° (ear level), not 45°",
            speaker.elevation_deg
        );
    }

    /// Overhead must read as +90° elevation, and the polar->cartesian
    /// direction has to land back on the same axis.
    #[test]
    fn derives_overhead_and_lateral_speakers_consistently() {
        let top = normalize_speaker(RawSpeaker {
            name: Some(serde_json::json!("TOP")),
            x: Some(0.0),
            y: Some(0.0),
            z: Some(1.0),
            ..RawSpeaker::default()
        });
        assert!((top.elevation_deg - 90.0).abs() < 1e-6);

        // Hard right in polar must come back as +X, not +Z.
        let right = normalize_speaker(RawSpeaker {
            name: Some(serde_json::json!("R")),
            azimuth: Some(90.0),
            elevation: Some(0.0),
            distance: Some(1.0),
            ..RawSpeaker::default()
        });
        assert!((right.x - 1.0).abs() < 1e-6, "x was {}", right.x);
        assert!(right.y.abs() < 1e-6, "y was {}", right.y);
        assert!(right.z.abs() < 1e-6, "z was {}", right.z);
    }

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
