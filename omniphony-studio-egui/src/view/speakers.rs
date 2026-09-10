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

/// The driver disc's radius as a fraction of the cube's side (`materials.js`:
/// `CircleGeometry(0.08 × 0.36)` against a 0.08 cube).
const DRIVER_RADIUS: f32 = 0.36;
/// How far the disc sits off the cube's +Z face, in cube sides: half the cube
/// plus a hair, so it never z-fights with the face it is on.
const DRIVER_OFFSET: f32 = 0.5 + 0.01;
const DRIVER_COLOR: u32 = 0x0c1118;

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

/// `applySpeakerOrientation`: the rotation that aims the cube's +Z face — the
/// one the driver disc is on — at the listener, with no roll on elevated
/// speakers.
///
/// three.js `Matrix4.lookAt(eye = origin, target = p, up = +Y)`: the local +Z
/// axis lands on `normalize(eye − target)`, which points from the speaker back
/// to the origin. A speaker straight overhead has no unique answer, so the
/// same nudge three.js applies is applied here rather than leaving a
/// degenerate basis.
pub fn face_listener_rotation(p: Vec3) -> Quat {
    if p.length_squared() < 1e-8 {
        return Quat::IDENTITY;
    }
    let mut z = -p.normalize();
    let up = Vec3::Y;
    if up.cross(z).length_squared() < 1e-12 {
        z.x += 1e-4;
        z = z.normalize();
    }
    let x = up.cross(z).normalize();
    let y = z.cross(x);
    Quat::from_mat3(&glam::Mat3::from_cols(x, y, z))
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
pub fn emit(sp: &SpeakerVisual, settings: &ViewSettings, frame: &mut FrameData) {
    let side = SPEAKER_BASE_SIZE * sp.scale;
    let rotation = if settings.speaker_face_listener_enabled {
        face_listener_rotation(sp.scene_pos)
    } else {
        Quat::IDENTITY
    };
    let model = Mat4::from_scale_rotation_translation(Vec3::splat(side), rotation, sp.scene_pos);
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
    // The driver: a dark disc on the face that is aimed at the listener, so
    // which way a speaker points is visible rather than inferred. It only
    // means anything when the cubes are oriented, and the web shows it only
    // then.
    if !settings.speaker_face_listener_enabled {
        return;
    }
    let disc = Mat4::from_scale_rotation_translation(Vec3::splat(side), rotation, sp.scene_pos)
        * Mat4::from_translation(Vec3::new(0.0, 0.0, DRIVER_OFFSET))
        * Mat4::from_scale(Vec3::splat(DRIVER_RADIUS));
    let colour = hex_linear(DRIVER_COLOR);
    frame.meshes.push(MeshItem {
        kind: MeshKind::Disc,
        instance: MeshInstance::new(disc, with_alpha(colour, 0.85), [0.0, 0.0, 0.0, 0.0]),
        blend: true,
        depth_test: true,
        order: 1,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The face the driver is on ends up pointing at the listener, wherever
    /// the speaker is, and the cube keeps no roll: its local +Y stays in the
    /// vertical plane through it.
    #[test]
    fn the_driver_face_aims_at_the_listener() {
        for p in [
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(-0.7, 0.5, 0.7),
            Vec3::new(0.0, 1.0, 0.0001),
        ] {
            let rotation = face_listener_rotation(p);
            let forward = rotation * Vec3::Z;
            let to_listener = -p.normalize();
            assert!(
                forward.dot(to_listener) > 0.999,
                "the driver face does not aim at the listener from {p:?}"
            );
            // No roll: the local X axis stays horizontal.
            let right = rotation * Vec3::X;
            assert!(right.y.abs() < 1e-4, "the cube is rolled at {p:?}");
        }
    }

    /// A speaker straight overhead has no unique answer; three.js nudges the
    /// basis rather than producing a degenerate one, and so does this.
    #[test]
    fn a_speaker_overhead_still_gets_a_rotation() {
        let rotation = face_listener_rotation(Vec3::Y);
        assert!(rotation.is_finite() && rotation.is_normalized());
        assert!((rotation * Vec3::Z).dot(-Vec3::Y) > 0.999);
        // And one on the listener itself is not rotated at all.
        assert_eq!(face_listener_rotation(Vec3::ZERO), Quat::IDENTITY);
    }
}
