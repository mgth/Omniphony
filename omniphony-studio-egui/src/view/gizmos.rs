//! The speaker edit gizmos (`gizmos.js`, `speakers.js updateSpeakerGizmo`).
//!
//! Two of them, one per coordinate mode, and only while the editor has armed
//! the mode being shown. The polar one draws the two angles and the distance
//! as things you can read off the scene — a ring at the speaker's distance
//! with a scale in degrees, an elevation arc turned to the speaker's azimuth,
//! and a measured line from the listener. The cartesian one is three axes and
//! three handles at the speaker.
//!
//! Both are overlays: they are drawn without the depth test so a gizmo is
//! never hidden by the room or by a speaker in front of it, which is what
//! makes it usable from any camera angle.

use glam::{Mat4, Quat, Vec3};

use crate::render::{
    FrameData, LineVertex, MeshInstance, MeshItem, MeshKind, hex_linear, with_alpha,
};

use crate::model::app_state::AppState;

use super::Label;

const RING_COLOUR: u32 = 0x9ef7ff;
const ARC_COLOUR: u32 = 0xffd27a;
const DISTANCE_COLOUR: u32 = 0xa8ffbf;
const DISTANCE_LABEL: u32 = 0x7bff6a;
const LABEL_COLOUR: u32 = 0xd9ecff;
const AXIS_COLOURS: [u32; 3] = [0xff6b6b, 0x7fff7f, 0x6bb8ff];

/// Which coordinate mode the editor is in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EditMode {
    #[default]
    Polar,
    Cartesian,
}

/// The gizmo's state, as the editor sets it.
#[derive(Clone, Copy, Debug, Default)]
pub struct GizmoState {
    pub mode: EditMode,
    /// Armed by the editor's "3D Edit" button, one mode at a time.
    pub polar_armed: bool,
    pub cartesian_armed: bool,
}

/// `channelPlacement(name) === 'virtual'`: the channel is spatialised into an
/// object rather than sent straight to a speaker, which is what makes its
/// position something the editor owns.
pub fn is_virtual_channel(app: &AppState, name: &str) -> bool {
    let Some(speakers) = app
        .live_options
        .virtual_bed
        .as_ref()
        .and_then(|bed| bed.get("speakers"))
        .and_then(serde_json::Value::as_array)
    else {
        return false;
    };
    speakers.iter().any(|entry| {
        entry.get("name").and_then(serde_json::Value::as_str) == Some(name)
            && entry
                .get("spatialize")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true)
    })
}

/// `cartesianToSpherical`: azimuth and elevation in degrees, and the distance.
pub fn spherical(p: Vec3) -> (f32, f32, f32) {
    let dist = p.length();
    let horiz = (p.x * p.x + p.z * p.z).sqrt();
    let az = p.z.atan2(p.x).to_degrees();
    let el = if horiz < 1e-6 {
        90.0 * p.y.signum()
    } else {
        p.y.atan2(horiz).to_degrees()
    };
    (az, el, dist)
}

/// `normalizeAngleDeg`: into [−180, 180].
fn normalize_deg(mut deg: f32) -> f32 {
    while deg > 180.0 {
        deg -= 360.0;
    }
    while deg < -180.0 {
        deg += 360.0;
    }
    deg
}

fn push_line(frame: &mut FrameData, a: Vec3, b: Vec3, colour: [f32; 4]) {
    frame.overlay_lines.push(LineVertex {
        pos: a.to_array(),
        color: colour,
    });
    frame.overlay_lines.push(LineVertex {
        pos: b.to_array(),
        color: colour,
    });
}

/// A closed loop through `points`.
fn push_loop(frame: &mut FrameData, points: &[Vec3], colour: [f32; 4]) {
    for i in 0..points.len() {
        push_line(frame, points[i], points[(i + 1) % points.len()], colour);
    }
}

/// The polar gizmo: the ring at the speaker's distance, the elevation arc in
/// its azimuth plane, and the distance line.
#[allow(clippy::too_many_arguments)]
pub fn emit_polar(
    p: Vec3,
    frame: &mut FrameData,
    project: &dyn Fn(Vec3) -> Option<(egui::Pos2, f32)>,
    points_per_unit: &dyn Fn(f32) -> f32,
    labels: &mut Vec<Label>,
) {
    let (az, el, dist) = spherical(p);
    // A speaker on the listener has no direction to draw; the ring would
    // collapse to a point.
    let d = dist.max(0.01);
    let az_rad = az.to_radians();
    let ring = with_alpha(hex_linear(RING_COLOUR), 0.6);
    let ring_ticks = with_alpha(hex_linear(RING_COLOUR), 0.5);
    let arc_colour = with_alpha(hex_linear(ARC_COLOUR), 0.75);
    let arc_ticks = with_alpha(hex_linear(ARC_COLOUR), 0.55);

    // The azimuth ring, in the horizontal plane at the speaker's distance.
    let ring_point = |angle: f32, r: f32| Vec3::new(angle.cos() * r * d, 0.0, angle.sin() * r * d);
    let circle: Vec<Vec3> = (0..64)
        .map(|i| ring_point(i as f32 / 64.0 * std::f32::consts::TAU, 1.0))
        .collect();
    push_loop(frame, &circle, ring);
    // A tick every five degrees: the scale is what makes the ring a readout
    // rather than a decoration.
    for i in 0..72 {
        let angle = (i as f32 * 5.0).to_radians();
        push_line(
            frame,
            ring_point(angle, 1.0),
            ring_point(angle, 1.08),
            ring_ticks,
        );
    }

    // The elevation arc, turned into the speaker's azimuth plane.
    let arc_point = |angle: f32, r: f32| {
        let local = Vec3::new(angle.cos() * r * d, angle.sin() * r * d, 0.0);
        Quat::from_rotation_y(-az_rad) * local
    };
    let arc: Vec<Vec3> = (0..48)
        .map(|i| {
            arc_point(
                i as f32 / 47.0 * std::f32::consts::PI - std::f32::consts::FRAC_PI_2,
                1.0,
            )
        })
        .collect();
    push_loop(frame, &arc, arc_colour);
    for i in 0..=36 {
        let angle = (-90.0 + i as f32 * 5.0f32).to_radians();
        push_line(
            frame,
            arc_point(angle, 1.0),
            arc_point(angle, 1.08),
            arc_ticks,
        );
    }

    // The distance: a line from the listener with an arrow at each end, so it
    // reads as a measurement and not as a ray.
    let line = with_alpha(hex_linear(DISTANCE_COLOUR), 0.7);
    push_line(frame, Vec3::ZERO, p, line);
    let dir = if p.length_squared() > 1e-12 {
        p.normalize()
    } else {
        Vec3::X
    };
    for (at, along) in [(dir * 0.1, dir), (p - dir * 0.1, -dir)] {
        frame.meshes.push(MeshItem {
            kind: MeshKind::Cone,
            instance: MeshInstance::unlit(
                Mat4::from_scale_rotation_translation(
                    Vec3::new(0.02, 0.06, 0.02),
                    Quat::from_rotation_arc(Vec3::Y, along),
                    at,
                ),
                line,
            ),
            blend: true,
            depth_test: false,
            order: 5,
        });
    }

    // The scales' labels, and the two readouts of where the speaker is.
    let mut label = |at: Vec3, text: String, hex: u32| {
        if let Some((pos, depth)) = project(at) {
            labels.push(Label {
                pos,
                text,
                color: egui::Color32::from_rgb(
                    ((hex >> 16) & 0xff) as u8,
                    ((hex >> 8) & 0xff) as u8,
                    (hex & 0xff) as u8,
                ),
                size: (0.052 * points_per_unit(depth)).clamp(6.0, 40.0).round(),
                depth,
            });
        }
    };
    for i in 0..24 {
        let deg = -180.0 + i as f32 * 15.0;
        let angle = deg.to_radians();
        label(
            ring_point(angle, 1.1) + Vec3::new(0.0, 0.02, 0.0),
            format!("{deg:.0}"),
            LABEL_COLOUR,
        );
    }
    label(
        ring_point(normalize_deg(az).to_radians(), 1.24) + Vec3::new(0.0, 0.04, 0.0),
        format!("{az:.1}"),
        RING_COLOUR,
    );
    for i in 0..13 {
        let deg = -90.0 + i as f32 * 15.0;
        label(
            arc_point(deg.to_radians(), 1.1),
            format!("{deg:.0}"),
            LABEL_COLOUR,
        );
    }
    label(
        arc_point(el.to_radians(), 1.24),
        format!("{el:.1}"),
        ARC_COLOUR,
    );
    label(
        p * 0.5 + Vec3::new(0.0, 0.08, 0.0),
        format!("{dist:.2}"),
        DISTANCE_LABEL,
    );
}

/// The cartesian gizmo: three axes at the speaker with a handle on each.
///
/// Its scale follows the camera distance so the handles stay the same size on
/// screen whether the camera is inside the room or well outside it.
pub fn emit_cartesian(p: Vec3, cam_pos: Vec3, frame: &mut FrameData) {
    const ARM: f32 = 0.45;
    const HANDLE: f32 = 0.045;
    let scale = ((cam_pos - p).length() * 0.08).max(0.2);
    for (axis, hex) in [Vec3::X, Vec3::Y, Vec3::Z].into_iter().zip(AXIS_COLOURS) {
        let colour = hex_linear(hex);
        let tip = p + axis * ARM * scale;
        push_line(frame, p, tip, with_alpha(colour, 0.85));
        frame.meshes.push(MeshItem {
            kind: MeshKind::Sphere,
            instance: MeshInstance::unlit(
                Mat4::from_scale_rotation_translation(
                    Vec3::splat(HANDLE * scale),
                    Quat::IDENTITY,
                    tip,
                ),
                with_alpha(colour, 0.95),
            ),
            blend: true,
            depth_test: false,
            order: 5,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The angles the ring and the arc are scaled by are the scene's own, and
    /// a speaker straight overhead is at ninety degrees rather than at an
    /// angle nobody can compute.
    #[test]
    fn the_spherical_readout_matches_the_scene_axes() {
        // Scene +X is the front; azimuth is measured from it toward +Z.
        let (az, el, d) = spherical(Vec3::new(1.0, 0.0, 0.0));
        assert!((az - 0.0).abs() < 1e-4 && el.abs() < 1e-4 && (d - 1.0).abs() < 1e-6);
        let (az, _, _) = spherical(Vec3::new(0.0, 0.0, 2.0));
        assert!((az - 90.0).abs() < 1e-4);
        let (_, el, d) = spherical(Vec3::new(0.0, 3.0, 0.0));
        assert!((el - 90.0).abs() < 1e-4 && (d - 3.0).abs() < 1e-6);
        let (_, el, _) = spherical(Vec3::new(0.0, -1.0, 0.0));
        assert!((el + 90.0).abs() < 1e-4);
        let (_, el, _) = spherical(Vec3::new(1.0, 1.0, 0.0));
        assert!((el - 45.0).abs() < 1e-4);
    }

    #[test]
    fn angles_are_normalised_into_the_ring_scale() {
        assert!((normalize_deg(190.0) + 170.0).abs() < 1e-4);
        assert!((normalize_deg(-190.0) - 170.0).abs() < 1e-4);
        assert!((normalize_deg(45.0) - 45.0).abs() < 1e-4);
    }

    /// The cartesian gizmo grows with the camera distance so its handles keep
    /// their size on screen, and never collapses when the camera is close.
    #[test]
    fn the_cartesian_gizmo_scales_with_the_camera() {
        let mut near = FrameData::new(
            glam::Mat4::IDENTITY,
            Vec3::ZERO,
            Vec3::X,
            Vec3::Y,
            [100, 100],
        );
        emit_cartesian(Vec3::ZERO, Vec3::new(0.0, 0.0, 0.5), &mut near);
        let mut far = FrameData::new(
            glam::Mat4::IDENTITY,
            Vec3::ZERO,
            Vec3::X,
            Vec3::Y,
            [100, 100],
        );
        emit_cartesian(Vec3::ZERO, Vec3::new(0.0, 0.0, 20.0), &mut far);
        let arm = |frame: &FrameData| Vec3::from_array(frame.overlay_lines[1].pos).length();
        // At half a unit the floor applies; at twenty it does not.
        assert!((arm(&near) - 0.45 * 0.2).abs() < 1e-5);
        assert!((arm(&far) - 0.45 * 1.6).abs() < 1e-5);
        assert_eq!(far.meshes.len(), 3);
    }
}
