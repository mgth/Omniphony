//! Speaker cubes (`speakers.js renderLayout`, `sources.js
//! updateSpeakerColorsFromSelection`, `applySpeakerLevel`,
//! `scene/speaker-band-bars.js bandColor`).

use std::time::Instant;

use glam::{Mat4, Quat, Vec3};

use crate::model::app_state::RoomRatio;
use crate::model::layouts::crossover_cutoffs;
use crate::osc::dispatch::Live;
use crate::render::{
    FrameData, MeshInstance, MeshItem, MeshKind, hex_linear, lerp_rgb, with_alpha,
};

use super::objects::hsl_to_rgb;
use super::{ViewSettings, dbfs_to_scale, decayed_level, scene_position};

/// `SPEAKER_BASE_SIZE` (materials.js).
pub const SPEAKER_BASE_SIZE: f32 = 0.08;
const SPEAKER_COLOR: u32 = 0x8ec8ff;
const SPEAKER_EMISSIVE: u32 = 0x10253a;
const SPEAKER_HOT: u32 = 0xff3030;
const SPEAKER_SELECTED: u32 = 0x4dff88;

pub struct SpeakerVisual {
    pub index: usize,
    pub name: String,
    pub scene_pos: Vec3,
    pub spatialize: bool,
    pub muted: bool,
    pub selected: bool,
    /// Uniform mesh scale (level × size slider).
    pub scale: f32,
    pub color: [f32; 3],
    pub opacity: f32,
    /// The crossover band this speaker belongs to, and how many there are:
    /// the gauge's lit segment takes its colour from the pair.
    pub band: (usize, usize),
    /// The pass-band in hertz, zero where the layout does not cut.
    pub pass_band: (f32, f32),
}

/// `bandColor(i, n)`: `#8ec8ff` for a single band, else an HSL ramp from red
/// (low) to blue (high). Returns linear RGB.
pub fn band_color(index: usize, count: usize) -> [f32; 3] {
    if count <= 1 {
        return hex_linear(SPEAKER_COLOR);
    }
    let hue = (8.0 + 248.0 * index as f32 / (count - 1) as f32).round();
    let srgb = hsl_to_rgb(hue / 360.0, 0.68, 0.56);
    [srgb[0].powf(2.2), srgb[1].powf(2.2), srgb[2].powf(2.2)]
}

/// `speakerBandIndex`: first band edge within 0.1 Hz of the speaker's low cut.
fn speaker_band_index(freq_low: Option<f32>, edges: &[f64]) -> usize {
    let lo = freq_low.filter(|f| *f > 0.0).map(f64::from).unwrap_or(0.0);
    edges.iter().position(|e| (e - lo).abs() < 0.1).unwrap_or(0)
}

pub fn collect(
    live: &Live,
    settings: &ViewSettings,
    room: &RoomRatio,
    selected_object: Option<&str>,
    selected_speaker: Option<usize>,
    now: Instant,
) -> Vec<SpeakerVisual> {
    let speakers = live.selected_speakers();
    let cutoffs = crossover_cutoffs(speakers);
    let mut edges: Vec<f64> = Vec::with_capacity(cutoffs.len() + 2);
    edges.push(0.0);
    edges.extend(cutoffs.iter().copied());
    edges.push(f64::INFINITY);
    let band_count = edges.len() - 1;
    let selected_gains = selected_object.and_then(|id| live.app.object_speaker_gains.get(id));
    let size_scale = settings.speaker_size.clamp(0.04, 0.2) / SPEAKER_BASE_SIZE;

    speakers
        .iter()
        .enumerate()
        .map(|(index, s)| {
            let key = index.to_string();
            let spatialize = s.spatialize != 0;
            let base_color = band_color(speaker_band_index(s.freq_low, &edges), band_count);
            let base_opacity: f32 = if spatialize { 0.65 } else { 0.3 };
            let rms = live
                .app
                .speaker_levels
                .get(&key)
                .map(|m| decayed_level(m.rms_dbfs, live.speaker_level_seen.get(&key).copied(), now))
                .unwrap_or(-100.0);
            let scale = dbfs_to_scale(rms, 0.65, 2.2) * size_scale;
            let selected = selected_speaker == Some(index);

            let (color, opacity) = match selected_gains {
                Some(gains) => {
                    let mix = gains.get(index).copied().unwrap_or(0.0).clamp(0.0, 1.0) as f32;
                    let color = lerp_rgb(base_color, hex_linear(SPEAKER_HOT), mix);
                    let opacity = if mix <= 1e-6 {
                        base_opacity.min(0.08)
                    } else {
                        base_opacity
                    };
                    (color, opacity)
                }
                None => (base_color, base_opacity),
            };
            let color = if selected {
                hex_linear(SPEAKER_SELECTED)
            } else {
                color
            };

            SpeakerVisual {
                index,
                name: s.id.clone(),
                scene_pos: scene_position([s.x, s.y, s.z], room),
                spatialize,
                muted: live.app.speaker_mutes.get(&key).is_some_and(|m| *m != 0),
                selected,
                scale,
                color,
                opacity,
                band: (speaker_band_index(s.freq_low, &edges), band_count),
                pass_band: (s.freq_low.unwrap_or(0.0), s.freq_high.unwrap_or(0.0)),
            }
        })
        .collect()
}

/// Cube with `MeshStandardMaterial` look: depth-written even though blended
/// (three.js keeps `depthWrite` on for the speaker material).
pub fn emit(sp: &SpeakerVisual, frame: &mut FrameData) {
    let model = Mat4::from_scale_rotation_translation(
        Vec3::splat(SPEAKER_BASE_SIZE * sp.scale),
        Quat::IDENTITY,
        sp.scene_pos,
    );
    let e = hex_linear(SPEAKER_EMISSIVE);
    frame.meshes.push(MeshItem {
        kind: MeshKind::Cube,
        instance: MeshInstance::new(
            model,
            with_alpha(sp.color, sp.opacity),
            [e[0], e[1], e[2], 0.15],
        ),
        blend: false,
        depth_test: true,
        order: 0,
    });
}
