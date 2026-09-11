//! Object (source) visuals: `sources.js` `updateSourceColorsFromSelection`,
//! `updateSourceSelectionStyles`, `updateSourceDecorations`, `objectBadge`,
//! `getObjectBaseColor`, and the halo/outline/effective-render renderables.

use std::time::Instant;

use glam::Vec3;

use crate::model::app_state::RoomRatio;
use crate::osc::dispatch::Live;
use glam::{Mat4, Quat};

use crate::render::{
    FrameData, LineVertex, MeshInstance, MeshItem, MeshKind, SpriteInstance, hex_linear, lerp_rgb,
    scale_rgb, with_alpha,
};

use super::{ViewSettings, billboard_ring, dbfs_to_scale, decayed_level, scene_position};

/// `app.objectDisplayMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ObjectDisplayMode {
    Circle,
    TransparentSphere,
    DiffuseSphere,
}

impl ObjectDisplayMode {
    pub const ALL: [ObjectDisplayMode; 3] = [
        ObjectDisplayMode::Circle,
        ObjectDisplayMode::TransparentSphere,
        ObjectDisplayMode::DiffuseSphere,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ObjectDisplayMode::Circle => "Circle",
            ObjectDisplayMode::TransparentSphere => "Transparent sphere",
            ObjectDisplayMode::DiffuseSphere => "Diffuse sphere",
        }
    }
}

/// Minimal speaker data the object rules need.
pub struct SpeakerRef {
    pub scene_pos: Vec3,
}

/// The injected test source (`object-test-id.js`).
pub const OBJECT_TEST_SOURCE_ID: &str = "injection";

/// `SOURCE_BASE_RADIUS` (materials.js).
pub const SOURCE_BASE_RADIUS: f32 = 0.07;

// materials.js colours (sRGB hex).
const SOURCE_COLOR: u32 = 0xff7c4d;
const SOURCE_DEFAULT_EMISSIVE: u32 = 0x64210c;
const SOURCE_NEUTRAL_EMISSIVE: u32 = 0x10161d;
const SOURCE_CONTRIBUTION_EMISSIVE: u32 = 0x10311a;
const SOURCE_SELECTED_EMISSIVE: u32 = 0x9b7f22;
const SOURCE_HOT: u32 = 0xff3030;
const OUTLINE_COLOR: u32 = 0xd9ecff;
const OUTLINE_SELECTED: u32 = 0xffde8a;
const SPEAKER_SELECTED: u32 = 0x4dff88;
const EFFECTIVE_COLOR: u32 = 0x7ce7ff;
const EFFECTIVE_EMISSIVE: u32 = 0x0a2834;
const EFFECTIVE_EMISSIVE_SELECTED: u32 = 0x10566c;
const TAG_A: u32 = 0xff8b6b;
const TAG_B: u32 = 0x62d7c7;
const BASE_OPACITY: f32 = 0.7;

const PALETTE: [u32; 16] = [
    0xff6b6b, 0x4ecdc4, 0xffe66d, 0x5dade2, 0xaf7ac5, 0xf5b041, 0x58d68d, 0xec7063, 0x48c9b0,
    0xf4d03f, 0x5499c7, 0xa569bd, 0xeb984e, 0x45b39d, 0x7fb3d5, 0xf1948a,
];

/// One object, fully resolved: position, colours, scales and decorations.
pub struct ObjectVisual {
    pub id: String,
    pub label: String,
    pub scene_pos: Vec3,
    pub selected: bool,
    pub muted: bool,
    /// `getObjectBaseColor` (linear), also the trail colour's base.
    pub base_color: [f32; 3],
    /// `userData.levelScale`, from the decayed RMS level (0.5..2.4).
    pub level_scale: f32,
    /// Sphere colour (linear) and opacity for the current display mode.
    pub sphere_color: [f32; 3],
    pub sphere_opacity: f32,
    pub emissive: [f32; 3],
    pub ring_color: [f32; 3],
    pub ring_opacity: f32,
    pub halo_color: [f32; 3],
    pub halo_opacity: f32,
    /// Effective-render centroid (scene) when the toggle is on and gains exist.
    pub effective_pos: Option<Vec3>,
}

/// `objectBadge(id).code`.
pub fn badge_code(id: &str, name: Option<&str>) -> String {
    if id == OBJECT_TEST_SOURCE_ID {
        return "INJ".to_owned();
    }
    let name = display_name(id, name);
    let lower = name.to_ascii_lowercase();
    let after =
        |prefix: &str| -> Option<&str> { lower.starts_with(prefix).then(|| &name[prefix.len()..]) };
    if let Some(rest) = after("ambience_")
        && !rest.is_empty()
    {
        return rest.to_owned();
    }
    if lower.starts_with("height_")
        && lower.ends_with("_synth")
        && name.len() > "height_".len() + "_synth".len()
    {
        return name["height_".len()..name.len() - "_synth".len()].to_owned();
    }
    if let Some(rest) = after("diffuse_")
        && !rest.is_empty()
    {
        return rest.to_owned();
    }
    if let Some(rest) = after("phantom_")
        && !rest.is_empty()
    {
        return match rest.split_once('_') {
            Some((a, b)) if !a.is_empty() && !b.is_empty() => format!("{a}·{b}"),
            _ => rest.to_owned(),
        };
    }
    if let Some(rest) = after("directh_")
        && !rest.is_empty()
    {
        return format!("{rest}↑");
    }
    if let Some(rest) = after("direct_")
        && !rest.is_empty()
    {
        return rest.to_owned();
    }
    match name.split_once('_') {
        Some((_, code)) if !code.is_empty() => code.to_owned(),
        _ => name,
    }
}

/// `getObjectDisplayName`: the raw name (or the id) without a leading
/// `a_`/`v_`/`obj_`-style technical prefix.
pub fn display_name(id: &str, name: Option<&str>) -> String {
    let raw = name.map(str::trim).filter(|s| !s.is_empty()).unwrap_or(id);
    let mut s = raw;
    if s.len() >= 2 {
        let b = s.as_bytes();
        if matches!(b[0].to_ascii_lowercase(), b'a' | b'v') && matches!(b[1], b'_' | b':' | b'-') {
            s = &s[2..];
        }
    }
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("obj") && s.len() >= 4 && matches!(s.as_bytes()[3], b'_' | b':' | b'-') {
        s = &s[4..];
    }
    s.to_owned()
}

/// `inferSourceTagFromId` + stored tag → `Some('A' | 'B')`.
fn source_tag(id: &str, stored: Option<&str>) -> Option<char> {
    let from_store = stored
        .and_then(|t| t.trim().chars().next())
        .map(|c| c.to_ascii_uppercase());
    if matches!(from_store, Some('A') | Some('B')) {
        return from_store;
    }
    let b = id.as_bytes();
    if b.len() >= 2 && matches!(b[1], b'_' | b':') {
        match b[0].to_ascii_lowercase() {
            b'a' => return Some('A'),
            b'b' => return Some('B'),
            _ => {}
        }
    }
    None
}

/// FNV-1a over UTF-16 code units, as `hashObjectId`.
fn hash_object_id(id: &str) -> u32 {
    let mut h: u32 = 2166136261;
    for unit in id.encode_utf16() {
        h ^= u32::from(unit);
        h = h.wrapping_mul(16777619);
    }
    h
}

/// `getObjectBaseColor` (linear RGB) and whether it is a semantic A/B colour.
pub fn base_color(id: &str, tag: Option<&str>) -> ([f32; 3], bool) {
    match source_tag(id, tag) {
        Some('A') => (hex_linear(TAG_A), true),
        Some('B') => (hex_linear(TAG_B), true),
        _ => {
            let idx = match id.parse::<i64>() {
                Ok(n) => (n.unsigned_abs() % PALETTE.len() as u64) as usize,
                Err(_) => (hash_object_id(id) % PALETTE.len() as u32) as usize,
            };
            (hex_linear(PALETTE[idx]), false)
        }
    }
}

/// `getObjectTrailColor`: base colour with `offsetHSL(0, +0.04, +0.08)`.
pub fn trail_color(base: [f32; 3]) -> [f32; 3] {
    // three.js offsets HSL of the sRGB-encoded colour.
    let srgb = [
        base[0].powf(1.0 / 2.2),
        base[1].powf(1.0 / 2.2),
        base[2].powf(1.0 / 2.2),
    ];
    let (h, s, l) = rgb_to_hsl(srgb);
    let out = hsl_to_rgb(h, (s + 0.04).clamp(0.0, 1.0), (l + 0.08).clamp(0.0, 1.0));
    [out[0].powf(2.2), out[1].powf(2.2), out[2].powf(2.2)]
}

fn rgb_to_hsl(c: [f32; 3]) -> (f32, f32, f32) {
    let max = c[0].max(c[1]).max(c[2]);
    let min = c[0].min(c[1]).min(c[2]);
    let l = (max + min) * 0.5;
    if (max - min).abs() < 1e-6 {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if max == c[0] {
        (c[1] - c[2]) / d + if c[1] < c[2] { 6.0 } else { 0.0 }
    } else if max == c[1] {
        (c[2] - c[0]) / d + 2.0
    } else {
        (c[0] - c[1]) / d + 4.0
    } / 6.0;
    (h, s, l)
}

pub fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [f32; 3] {
    if s <= 0.0 {
        return [l, l, l];
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let f = |mut t: f32| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    [f(h + 1.0 / 3.0), f(h), f(h - 1.0 / 3.0)]
}

/// Resolve every visible object from the model.
pub fn collect(
    live: &Live,
    settings: &ViewSettings,
    room: &RoomRatio,
    speakers: &[SpeakerRef],
    selected_object: Option<&str>,
    selected_speaker: Option<usize>,
    now: Instant,
) -> Vec<ObjectVisual> {
    let mut out = Vec::with_capacity(live.app.sources.len());
    for (id, src) in &live.app.sources {
        // Metadata-silent objects (gain ≤ −128 dB) draw nothing at all.
        if src.gain_db.is_some_and(|g| g <= -128) {
            continue;
        }
        // Position: the editor's pin if it holds this one, else snapped onto
        // its direct speaker, else room-warped. The pin exists because a
        // stream packet arriving mid-drag carries the position the object had
        // before the drag started, and would put it back there.
        let pinned = settings
            .channel_edit_pin
            .as_ref()
            .filter(|(pinned, _)| pinned == id)
            .map(|(_, at)| *at);
        let scene_pos = match (
            pinned,
            src.direct_speaker_index
                .and_then(|i| speakers.get(i as usize)),
        ) {
            (Some(at), _) => at,
            (None, Some(sp)) => sp.scene_pos,
            (None, None) => scene_position([src.x, src.y, src.z], room),
        };

        let rms = live
            .app
            .source_levels
            .get(id)
            .map(|m| decayed_level(m.rms_dbfs, live.source_level_seen.get(id).copied(), now))
            .unwrap_or(-100.0);
        let level_scale = dbfs_to_scale(rms, 0.5, 2.4);

        let selected = selected_object == Some(id.as_str());
        let muted = live.app.object_mutes.get(id).is_some_and(|m| *m != 0);
        let (palette_color, semantic) = base_color(id, src.source_tag.as_deref());
        let use_object_color = settings.object_colors_enabled || semantic;
        let object_color = if use_object_color {
            palette_color
        } else {
            hex_linear(SOURCE_COLOR)
        };

        // Contribution to the selected speaker.
        let (mix, has_contribution, speaker_selected) = match selected_speaker {
            Some(si) => {
                let g = live
                    .app
                    .object_speaker_gains
                    .get(id)
                    .and_then(|v| v.get(si))
                    .copied()
                    .unwrap_or(0.0)
                    .clamp(0.0, 1.0) as f32;
                (g, g > 1e-6, true)
            }
            None => (0.0, false, false),
        };
        let green = hex_linear(SPEAKER_SELECTED);
        let outline_base = if use_object_color {
            object_color
        } else {
            hex_linear(OUTLINE_COLOR)
        };

        let mode = settings.object_display_mode;
        // --- updateSourceColorsFromSelection ---
        let (
            mut sphere_color,
            mut sphere_opacity,
            mut ring_color,
            mut ring_opacity,
            mut halo_color,
            mut halo_opacity,
        );
        if !speaker_selected {
            sphere_color = object_color;
            sphere_opacity = match mode {
                ObjectDisplayMode::Circle => 0.0,
                ObjectDisplayMode::TransparentSphere => (BASE_OPACITY * 0.82).max(0.58),
                ObjectDisplayMode::DiffuseSphere => 0.06,
            };
            ring_color = outline_base;
            ring_opacity = 0.98;
            halo_color = object_color;
            halo_opacity = 0.4;
        } else if has_contribution {
            sphere_color = lerp_rgb(object_color, green, (0.22 + 0.42 * mix).min(0.68));
            sphere_opacity = match mode {
                ObjectDisplayMode::Circle => (BASE_OPACITY * (0.35 + 0.55 * mix)).max(0.24),
                ObjectDisplayMode::TransparentSphere => {
                    (BASE_OPACITY * (0.82 + 0.36 * mix)).max(0.68)
                }
                ObjectDisplayMode::DiffuseSphere => (0.07 + 0.08 * mix).max(0.07),
            };
            ring_color = lerp_rgb(outline_base, green, 0.65 * mix);
            ring_opacity = 0.25 + 0.73 * mix;
            halo_color = lerp_rgb(object_color, green, (0.2 + 0.4 * mix).min(0.55));
            halo_opacity = 0.34 + 0.34 * mix;
        } else {
            sphere_color = object_color;
            sphere_opacity = match mode {
                ObjectDisplayMode::Circle => 0.0,
                ObjectDisplayMode::TransparentSphere => 0.58,
                ObjectDisplayMode::DiffuseSphere => 0.05,
            };
            ring_color = outline_base;
            ring_opacity = 0.15;
            halo_color = object_color;
            halo_opacity = 0.2;
        }

        // --- updateSourceSelectionStyles ---
        let emissive = match mode {
            ObjectDisplayMode::Circle => {
                if selected {
                    hex_linear(SOURCE_SELECTED_EMISSIVE)
                } else if speaker_selected {
                    hex_linear(if has_contribution {
                        SOURCE_CONTRIBUTION_EMISSIVE
                    } else {
                        SOURCE_NEUTRAL_EMISSIVE
                    })
                } else {
                    hex_linear(SOURCE_DEFAULT_EMISSIVE)
                }
            }
            ObjectDisplayMode::TransparentSphere => {
                let k = if selected { 0.72 * 1.45 } else { 0.42 };
                scale_rgb(sphere_color, k)
            }
            ObjectDisplayMode::DiffuseSphere => {
                let k = if selected { 0.46 } else { 0.26 * 0.65 };
                scale_rgb(sphere_color, k)
            }
        };
        if selected {
            ring_color = if speaker_selected {
                lerp_rgb(hex_linear(SOURCE_HOT), hex_linear(OUTLINE_SELECTED), 0.55)
            } else {
                hex_linear(OUTLINE_SELECTED)
            };
            ring_opacity = 1.0;
            halo_opacity = halo_opacity.max(0.7);
        }
        let _ = &mut sphere_color;
        let _ = &mut sphere_opacity;
        let _ = &mut halo_color;

        // Effective-render centroid: gain²-weighted speaker positions.
        let effective_pos = if settings.effective_render_enabled {
            let band = live
                .app
                .object_band_gains
                .get(id)
                .and_then(|bands| bands.get(settings.heatmap_band_index))
                .filter(|g| !g.is_empty());
            let gains = band.or_else(|| live.app.object_speaker_gains.get(id));
            gains.and_then(|g| {
                let mut acc = Vec3::ZERO;
                let mut wsum = 0.0f32;
                for (i, &gain) in g.iter().enumerate() {
                    if gain <= 0.0 {
                        continue;
                    }
                    let Some(sp) = speakers.get(i) else { continue };
                    let w = (gain * gain) as f32;
                    acc += sp.scene_pos * w;
                    wsum += w;
                }
                (wsum > 1e-9).then(|| acc / wsum)
            })
        } else {
            None
        };

        out.push(ObjectVisual {
            label: badge_code(id, src.name.as_deref()),
            id: id.clone(),
            scene_pos,
            selected,
            muted,
            base_color: palette_color,
            level_scale,
            sphere_color,
            sphere_opacity,
            emissive,
            ring_color,
            ring_opacity,
            halo_color,
            halo_opacity,
            effective_pos,
        });
    }
    out.sort_by(|a, b| match (a.id.parse::<u32>(), b.id.parse::<u32>()) {
        (Ok(x), Ok(y)) => x.cmp(&y),
        _ => a.id.cmp(&b.id),
    });
    out
}

/// Push one object's renderables into the frame (`updateSourceDecorations`).
pub fn emit(
    obj: &ObjectVisual,
    settings: &ViewSettings,
    frame: &mut FrameData,
    right: Vec3,
    up: Vec3,
) {
    let sphere_size_scale = settings.object_sphere_size / SOURCE_BASE_RADIUS;
    let mesh_scale = obj.level_scale * sphere_size_scale;
    let gloss = match settings.object_display_mode {
        ObjectDisplayMode::DiffuseSphere => 0.3,
        _ => 0.95,
    };
    let sphere = |center: Vec3, radius: f32| {
        Mat4::from_scale_rotation_translation(Vec3::splat(radius), Quat::IDENTITY, center)
    };

    if obj.sphere_opacity > 0.001 {
        frame.meshes.push(MeshItem {
            kind: MeshKind::Sphere,
            instance: MeshInstance::new(
                sphere(obj.scene_pos, SOURCE_BASE_RADIUS * mesh_scale),
                with_alpha(obj.sphere_color, obj.sphere_opacity),
                [obj.emissive[0], obj.emissive[1], obj.emissive[2], gloss],
            ),
            blend: true,
            depth_test: true,
            order: 0,
        });
    }

    match settings.object_display_mode {
        ObjectDisplayMode::Circle => {
            let radius = SOURCE_BASE_RADIUS * obj.level_scale.max(0.5) * sphere_size_scale * 1.08;
            billboard_ring(
                obj.scene_pos,
                radius,
                right,
                up,
                with_alpha(obj.ring_color, obj.ring_opacity),
                &mut frame.overlay_lines,
            );
        }
        ObjectDisplayMode::DiffuseSphere => {
            frame.sprites_additive.push(SpriteInstance {
                center: obj.scene_pos.to_array(),
                size: 0.26 * mesh_scale * 2.15,
                color: with_alpha(obj.halo_color, obj.halo_opacity),
                params: [SpriteInstance::HALO, 0.0, 0.0, 0.0],
            });
        }
        ObjectDisplayMode::TransparentSphere => {}
    }

    if let Some(p) = obj.effective_pos {
        let marker_scale = (mesh_scale * 0.12).max(0.035);
        let (opacity, emissive) = if obj.selected {
            (0.68, EFFECTIVE_EMISSIVE_SELECTED)
        } else {
            (0.34, EFFECTIVE_EMISSIVE)
        };
        let e = hex_linear(emissive);
        frame.meshes.push(MeshItem {
            kind: MeshKind::Sphere,
            instance: MeshInstance::new(
                sphere(p, 0.04 * marker_scale),
                with_alpha(hex_linear(EFFECTIVE_COLOR), opacity),
                [e[0], e[1], e[2], 0.5],
            ),
            blend: true,
            depth_test: true,
            order: 12,
        });
        if (p - obj.scene_pos).length() > 0.01 {
            let c = with_alpha(
                hex_linear(EFFECTIVE_COLOR),
                if obj.selected { 0.44 } else { 0.22 },
            );
            frame.lines.push(LineVertex {
                pos: obj.scene_pos.to_array(),
                color: c,
            });
            frame.lines.push(LineVertex {
                pos: p.to_array(),
                color: c,
            });
        }
    }
}
