//! Speaker cubes (`speakers.js renderLayout`, `sources.js
//! updateSpeakerColorsFromSelection`, `applySpeakerLevel`,
//! `scene/speaker-band-bars.js bandColor`).

use glam::{Mat4, Quat, Vec3};

use crate::model::app_state::RoomRatio;
use crate::model::binaural::RenderPath;
use crate::model::layouts::crossover_cutoffs;
use crate::osc::dispatch::Live;
use crate::render::{
    FrameData, LineVertex, MeshInstance, MeshItem, MeshKind, hex_linear, lerp_rgb, with_alpha,
};

use super::objects::hsl_to_rgb;
use super::room::MEASURED_ROOM_COLOR;
use super::{ViewSettings, dbfs_to_scale, scene_point, scene_position};

/// `setSpeakersGhosted`: how much of a speaker is left when the renderer is
/// not feeding speakers at all.
const GHOST: f32 = 0.18;

/// A wireframe's edges read thin: the alpha a solid cube would have, lifted.
const WIRE_ALPHA_GAIN: f32 = 1.4;

/// What kind of thing a speaker is on the path in force, said by its shape:
/// colour, opacity and size already carry other meanings (the crossover
/// band, the selection and the selected object's feed, the level).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SpeakerLook {
    /// A loudspeaker the speaker stage feeds: a solid cube.
    Real,
    /// A virtual speaker of the headphone room, what the cascade convolves:
    /// a wireframe cube in the band colour.
    Virtual,
    /// A loudspeaker of a measured room (BRIR): a wireframe cube in the
    /// measured room's colour, the set carrying no crossover band.
    Measured,
    /// The layout kept in view on the direct path, where nothing feeds it:
    /// ghosted, a reference for where the objects are.
    Reference,
}

impl SpeakerLook {
    pub fn of(path: RenderPath) -> Self {
        match path {
            RenderPath::Speakers => SpeakerLook::Real,
            RenderPath::VirtualRoom => SpeakerLook::Virtual,
            RenderPath::MeasuredRoom => SpeakerLook::Measured,
            RenderPath::Direct => SpeakerLook::Reference,
        }
    }

    /// Drawn as edges rather than a solid.
    pub fn is_wire(self) -> bool {
        matches!(self, SpeakerLook::Virtual | SpeakerLook::Measured)
    }
}

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
    /// What kind of speaker this is on the path in force.
    pub look: SpeakerLook,
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
) -> Vec<SpeakerVisual> {
    let speakers = live.selected_speakers();
    let cutoffs = crossover_cutoffs(speakers);
    let mut edges: Vec<f64> = Vec::with_capacity(cutoffs.len() + 2);
    edges.push(0.0);
    edges.extend(cutoffs.iter().copied());
    edges.push(f64::INFINITY);
    let band_count = edges.len() - 1;
    let selected_gains = selected_object.and_then(|id| live.app.object_speaker_gains.get(id));
    let look = SpeakerLook::of(live.app.render_path());
    // A measured room's loudspeakers stand where they were measured, in
    // metres at the room's reach; the LFE bus the layout appends has no
    // measurement and keeps the layout's place.
    let measured: Option<Vec<Vec3>> = (look == SpeakerLook::Measured)
        .then(|| live.app.brir_geometry())
        .flatten()
        .map(|g| {
            let reach = g.reach_m();
            g.emitters_m
                .iter()
                .map(|e| scene_point([e[0] / reach, e[1] / reach, e[2] / reach]))
                .collect()
        });
    let size_scale = settings.speaker_size.clamp(0.04, 0.2) / SPEAKER_BASE_SIZE;

    speakers
        .iter()
        .enumerate()
        .map(|(index, s)| {
            let key = index.to_string();
            let spatialize = s.spatialize != 0;
            let base_color = if look == SpeakerLook::Measured {
                hex_linear(MEASURED_ROOM_COLOR)
            } else {
                band_color(speaker_band_index(s.freq_low, &edges), band_count)
            };
            let base_opacity: f32 = if spatialize { 0.65 } else { 0.3 };
            // Already decayed on the model (`maintain_meters`), the same
            // number the speaker list shows.
            let rms = live
                .app
                .speaker_levels
                .get(&key)
                .map_or(-100.0, |m| m.rms_dbfs);
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
            // The ghost factor is applied after everything else. The web
            // writes the base opacity back on the next selection or gains
            // update and so loses the ghosting until the next mode change;
            // taking it last is the same rule stated once, and it holds.
            let opacity = if look == SpeakerLook::Reference {
                opacity * GHOST
            } else {
                opacity
            };

            SpeakerVisual {
                index,
                name: s.id.clone(),
                scene_pos: pinned_position(
                    settings.speaker_edit_pin,
                    index,
                    measured
                        .as_ref()
                        .and_then(|m| m.get(index).copied())
                        .unwrap_or_else(|| scene_position([s.x, s.y, s.z], room)),
                ),
                spatialize,
                muted: live.app.speaker_mutes.get(&key).is_some_and(|m| *m != 0),
                selected,
                scale,
                color,
                opacity,
                look,
                band: (speaker_band_index(s.freq_low, &edges), band_count),
                pass_band: (s.freq_low.unwrap_or(0.0), s.freq_high.unwrap_or(0.0)),
            }
        })
        .collect()
}

/// The editor's pin wins over the state for the one speaker it holds: a
/// state bundle arriving mid-drag carries the position the speaker had before
/// the drag started, and would put it back there.
pub fn pinned_position(pin: Option<(usize, Vec3)>, index: usize, from_state: Vec3) -> Vec3 {
    match pin {
        Some((pinned, at)) if pinned == index => at,
        _ => from_state,
    }
}

impl SpeakerVisual {
    /// Only a reference: its label dims with it.
    pub fn ghosted(&self) -> bool {
        self.look == SpeakerLook::Reference
    }
}

/// The twelve edges of the unit cube `[-0.5, 0.5]³` under `model`, as a
/// depth-tested line list: the wireframe a virtual or measured speaker is
/// drawn as.
pub fn wire_cube(model: Mat4, color: [f32; 4], out: &mut Vec<LineVertex>) {
    let corner = |i: usize| {
        model.transform_point3(Vec3::new(
            if i & 1 == 0 { -0.5 } else { 0.5 },
            if i & 2 == 0 { -0.5 } else { 0.5 },
            if i & 4 == 0 { -0.5 } else { 0.5 },
        ))
    };
    for a in 0..8usize {
        for bit in [1usize, 2, 4] {
            let b = a | bit;
            if b == a {
                continue;
            }
            out.push(LineVertex {
                pos: corner(a).to_array(),
                color,
            });
            out.push(LineVertex {
                pos: corner(b).to_array(),
                color,
            });
        }
    }
}

/// A solid cube with the `MeshStandardMaterial` look, depth-written even
/// though blended (three.js keeps `depthWrite` on for the speaker material);
/// a wireframe for the virtual and measured kinds (`SpeakerLook`).
pub fn emit(sp: &SpeakerVisual, settings: &ViewSettings, frame: &mut FrameData) {
    let side = SPEAKER_BASE_SIZE * sp.scale;
    let rotation = if settings.speaker_face_listener_enabled {
        face_listener_rotation(sp.scene_pos)
    } else {
        Quat::IDENTITY
    };
    let model = Mat4::from_scale_rotation_translation(Vec3::splat(side), rotation, sp.scene_pos);
    if sp.look.is_wire() {
        wire_cube(
            model,
            with_alpha(sp.color, (sp.opacity * WIRE_ALPHA_GAIN).min(1.0)),
            &mut frame.lines,
        );
    } else {
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

    /// The pin replaces the state's position for the speaker it names and
    /// leaves every other one alone.
    #[test]
    fn the_edit_pin_holds_only_its_own_speaker() {
        let pin = Some((2, Vec3::new(0.5, 0.25, -0.5)));
        let from_state = Vec3::new(-1.0, 0.0, 1.0);
        assert_eq!(
            pinned_position(pin, 2, from_state),
            Vec3::new(0.5, 0.25, -0.5)
        );
        assert_eq!(pinned_position(pin, 1, from_state), from_state);
        assert_eq!(pinned_position(None, 2, from_state), from_state);
    }

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

    /// Each kind of speaker has its shape, from the path in force: solid
    /// on the loudspeakers, wire on the two headphone rooms, ghosted as a
    /// reference on the direct path.
    #[test]
    fn the_look_follows_the_render_path() {
        assert_eq!(SpeakerLook::of(RenderPath::Speakers), SpeakerLook::Real);
        assert_eq!(
            SpeakerLook::of(RenderPath::VirtualRoom),
            SpeakerLook::Virtual
        );
        assert_eq!(
            SpeakerLook::of(RenderPath::MeasuredRoom),
            SpeakerLook::Measured
        );
        assert_eq!(SpeakerLook::of(RenderPath::Direct), SpeakerLook::Reference);
        assert!(SpeakerLook::Virtual.is_wire() && SpeakerLook::Measured.is_wire());
        assert!(!SpeakerLook::Real.is_wire() && !SpeakerLook::Reference.is_wire());
    }

    /// A wire cube is twelve edges, each joining two corners of the cube
    /// the model places, one unit apart along one axis.
    #[test]
    fn a_wire_cube_is_twelve_edges_of_the_placed_cube() {
        let mut out = Vec::new();
        let model = Mat4::from_scale_rotation_translation(
            Vec3::splat(0.2),
            Quat::IDENTITY,
            Vec3::new(1.0, 2.0, 3.0),
        );
        wire_cube(model, [1.0; 4], &mut out);
        assert_eq!(out.len(), 24);
        for pair in out.chunks(2) {
            let (a, b) = (Vec3::from_array(pair[0].pos), Vec3::from_array(pair[1].pos));
            let d = (b - a).abs();
            let axes = [d.x, d.y, d.z].iter().filter(|v| **v > 1e-6).count();
            assert_eq!(axes, 1, "an edge runs along one axis: {a:?} → {b:?}");
            assert!(
                (d.max_element() - 0.2).abs() < 1e-6,
                "an edge is one side long"
            );
            for p in [a, b] {
                assert!((p - Vec3::new(1.0, 2.0, 3.0)).abs().max_element() <= 0.1 + 1e-6);
            }
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
