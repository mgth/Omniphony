//! Object (source) visuals: `sources.js` `updateSourceColorsFromSelection`,
//! `updateSourceSelectionStyles`, `updateSourceDecorations`, `objectBadge`,
//! `getObjectBaseColor`, and the halo/outline/perceived-image renderables.

use glam::Vec3;

use crate::model::app_state::{ChannelTag, RoomRatio};
use crate::model::layouts::crossover_bands;
use crate::model::perceived::{self, Band, Head};
use crate::osc::dispatch::Live;
use glam::{Mat4, Quat};

use crate::render::{
    FrameData, LineVertex, MeshInstance, MeshItem, MeshKind, SpriteInstance, hex_linear, lerp_rgb,
    scale_rgb, with_alpha,
};

use super::{ViewSettings, billboard_ring, dbfs_to_scale, scene_position};

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

/// The injected test source, named where the service that publishes it lives.
pub use crate::host::services::object_test::SOURCE_ID as OBJECT_TEST_SOURCE_ID;

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
    /// The perceived image (`model::perceived`) when the toggle is on and
    /// the stage reports gains.
    pub perceived: Option<Perceived>,
}

/// Where an object is heard, and how sharply.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Perceived {
    /// The image's point in the scene, on the loudspeakers carrying it.
    pub pos: Vec3,
    /// `|rE|`: 1 for one loudspeaker, towards 0 as the image spreads.
    pub focus: f32,
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
    if id == OBJECT_TEST_SOURCE_ID {
        // Studio's own marker, and the only source whose name is ours to
        // choose. The model holds codes, so the translation happens here.
        return crate::i18n::t("objectTest.markerLabel").to_owned();
    }
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

/// What a channel tag is called: the stream's own name for the channels,
/// else the kind's (a kind this Studio does not know stays as it is sent),
/// with the language when the stream states one — `Dialogue`, `Dialogue VF
/// (fr)`.
pub fn tag_name(tag: &ChannelTag) -> String {
    let name = match (tag.label.trim(), tag.kind.as_str()) {
        ("", ChannelTag::DIALOGUE) => crate::i18n::t("input.dialogue"),
        ("", kind) => kind,
        (label, _) => label,
    };
    if tag.language.is_empty() {
        name.to_owned()
    } else {
        format!("{name} ({})", tag.language)
    }
}

/// A source's badge code, led by its channel tag's name when the stream tags
/// the channel: a dialogue element coded apart from the bed has an `L`, an
/// `R` and a `C` of its own, which nothing else tells from the bed's.
pub fn tagged_code(tag: Option<&ChannelTag>, code: String) -> String {
    match tag {
        Some(tag) => format!("{} · {code}", tag_name(tag)),
        None => code,
    }
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
    head: &Head,
    selected_object: Option<&str>,
    selected_speaker: Option<usize>,
) -> Vec<ObjectVisual> {
    let mut out = Vec::with_capacity(live.app.sources.len());
    // What the perceived image reads, once per frame: the loudspeakers as
    // drawn, in the frame the objects are, and the bands the stage splits
    // the objects into.
    let (positions, bands_hz): (Vec<[f32; 3]>, Vec<(f64, f64)>) =
        if settings.effective_render_enabled {
            (
                speakers.iter().map(|s| s.scene_pos.to_array()).collect(),
                crossover_bands(live.selected_speakers()),
            )
        } else {
            (Vec::new(), Vec::new())
        };
    let band_hz = |band: usize| bands_hz.get(band).copied().unwrap_or((0.0, f64::INFINITY));
    let mut band_scratch: Vec<Band<'_>> = Vec::new();
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

        // Already decayed on the model (`maintain_meters`), the same number
        // the object list shows.
        let rms = live
            .app
            .source_levels
            .get(id)
            .map_or(-100.0, |m| m.rms_dbfs);
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

        // The perceived image: the selected band's, or every band's mix
        // while all bands are shown (`model::perceived`).
        let perceived = if settings.effective_render_enabled {
            let band_gains = live
                .app
                .object_band_gains
                .get(id)
                .filter(|bands| !bands.is_empty());
            let image =
                match band_gains {
                    Some(bands) if !settings.heatmap_all_bands => {
                        let band = settings.heatmap_band_index;
                        bands
                            .get(band)
                            .filter(|g| !g.is_empty())
                            .and_then(|g| perceived::band_image(g, &positions, head, band_hz(band)))
                    }
                    Some(bands) => {
                        let band_rms = live.object_band_rms.get(id);
                        band_scratch.clear();
                        band_scratch.extend(bands.iter().enumerate().map(|(band, gains)| Band {
                            gains,
                            hz: band_hz(band),
                            rms_dbfs: band_rms.and_then(|rms| rms.get(band).copied()),
                        }));
                        perceived::object_image(&band_scratch, &positions, head)
                    }
                    // An engine without band gains: its summed gains are the one
                    // band there is.
                    None => live.app.object_speaker_gains.get(id).and_then(|g| {
                        perceived::band_image(g, &positions, head, (0.0, f64::INFINITY))
                    }),
                };
            image.map(|image| Perceived {
                pos: Vec3::from_array(image.point()),
                focus: image.focus,
            })
        } else {
            None
        };

        out.push(ObjectVisual {
            label: tagged_code(
                live.app.channel_tag_of(id),
                badge_code(id, src.name.as_deref()),
            ),
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
            perceived,
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

    if let Some(image) = obj.perceived {
        let p = image.pos;
        // The focus shows in the marker itself: sharp and solid for one
        // loudspeaker, larger and fainter as the image spreads over the
        // array. (A ring of the loudspeakers' angular spread was tried and
        // said nothing: VBAP spreads 30–60° by nature.)
        let spread = 1.0 - image.focus.clamp(0.0, 1.0);
        let marker_scale = (mesh_scale * 0.12).max(0.035) * (1.0 + spread);
        let (opacity, emissive) = if obj.selected {
            (0.68, EFFECTIVE_EMISSIVE_SELECTED)
        } else {
            (0.34, EFFECTIVE_EMISSIVE)
        };
        let opacity = opacity * (1.0 - 0.6 * spread);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A tagged channel is named after its tag: the stream's own name for
    /// it and its language, else the kind's name.
    #[test]
    fn a_tagged_channel_is_led_by_its_tags_name() {
        let mut tag = ChannelTag {
            kind: ChannelTag::DIALOGUE.to_owned(),
            ..ChannelTag::default()
        };
        assert_eq!(tag_name(&tag), crate::i18n::t("input.dialogue"));
        tag.label = "Dialogue VF".to_owned();
        tag.language = "fr".to_owned();
        assert_eq!(tag_name(&tag), "Dialogue VF (fr)");
        assert_eq!(
            tagged_code(Some(&tag), "L".to_owned()),
            "Dialogue VF (fr) · L"
        );
        assert_eq!(tagged_code(None, "L".to_owned()), "L");
        // A kind this Studio does not know is shown as the stream sends it.
        let unknown = ChannelTag {
            kind: "commentary".to_owned(),
            ..ChannelTag::default()
        };
        assert_eq!(tag_name(&unknown), "commentary");
    }
}
