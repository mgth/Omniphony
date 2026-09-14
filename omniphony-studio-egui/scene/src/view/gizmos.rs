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

use super::screen;
use glam::{Mat4, Quat, Vec3};

use crate::render::{
    FrameData, LineVertex, MeshInstance, MeshItem, MeshKind, hex_linear, with_alpha,
};

use crate::model::app_state::{AppState, RoomRatio};

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

/// What the gizmo is pointed at, so a drag knows what to commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GizmoTarget {
    /// A speaker, by its index in the layout.
    Speaker(usize),
    /// A virtual bed channel, by name.
    Channel(String),
}

/// A scene position back to the normalised ADM triple the layout is written
/// in: the room warp inverted, the axes swizzled, the result clamped.
pub fn scene_to_normalized(scene: Vec3, room: &RoomRatio) -> [f64; 3] {
    use omniphony_geometry::f64 as g;
    g::inverse_room_scaled_position(
        g::scene_to_adm([scene.x as f64, scene.y as f64, scene.z as f64]),
        [room.width, room.length, room.height],
        room.rear,
        room.lower,
        room.center_blend,
    )
}

/// Where a ray crosses a plane through the origin, or `None` when it runs
/// along it.
pub fn ray_plane(origin: Vec3, dir: Vec3, normal: Vec3) -> Option<Vec3> {
    let denominator = normal.dot(dir);
    if denominator.abs() < 1e-6 {
        return None;
    }
    let t = -normal.dot(origin) / denominator;
    (t > 0.0).then(|| origin + dir * t)
}

/// How far along an axis the closest approach of a ray lies — the scalar an
/// axis drag moves the speaker by.
pub fn project_ray_onto_axis(origin: Vec3, dir: Vec3, axis_origin: Vec3, axis_dir: Vec3) -> f32 {
    let w = axis_origin - origin;
    let a = axis_dir.dot(axis_dir);
    let b = axis_dir.dot(dir);
    let c = dir.dot(dir);
    let d = axis_dir.dot(w);
    let e = dir.dot(w);
    let denominator = a * c - b * b;
    // A ray along the axis has no unique closest point; the axis's own
    // projection of the origin is the honest answer.
    if denominator.abs() < 1e-6 {
        return d / a.max(1e-6);
    }
    (b * e - c * d) / denominator
}

/// The angle snapping a polar drag applies.
///
/// The radial distance of the pointer from the ring is the precision control:
/// on the ring the drag snaps to whole degrees, and pulling outward past a
/// tenth of the radius coarsens it to five. Nothing is snapped while the
/// pointer is inside the ring, which is where fine work happens.
pub fn snap_drag_angle(deg: f32, radial_delta: f32) -> f32 {
    use omniphony_geometry::f32 as g;
    if (0.0..=0.1).contains(&radial_delta) {
        g::snap_deg(deg, 1.0, 0.5)
    } else if radial_delta > 0.1 {
        g::snap_deg(deg, 5.0, 2.5)
    } else {
        deg
    }
}

/// Which ADM axis a scene axis is: the swizzle `scene_to_adm` applied to the
/// axis vector, read back as the index it lands on.
pub fn adm_axis_of(scene_axis: Vec3) -> usize {
    use omniphony_geometry::f32 as g;
    let adm = g::scene_to_adm([scene_axis.x, scene_axis.y, scene_axis.z]);
    (0..3)
        .max_by(|&a, &b| adm[a].abs().total_cmp(&adm[b].abs()))
        .unwrap_or(0)
}

/// `value` moved to the nearest of `nodes`; unchanged when there are none.
pub fn snap_to_nodes(value: f64, nodes: &[f64]) -> f64 {
    nodes
        .iter()
        .copied()
        .min_by(|a, b| (a - value).abs().total_cmp(&(b - value).abs()))
        .unwrap_or(value)
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
    project: &dyn Fn(Vec3) -> Option<(screen::ScreenPos, f32)>,
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
                color: screen::rgb(
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

    /// Scene depth is ADM y, scene up is ADM z and scene right is ADM x, and
    /// a negative axis lands on the same index as its positive.
    #[test]
    fn a_scene_axis_names_its_adm_axis() {
        assert_eq!(adm_axis_of(Vec3::X), 1);
        assert_eq!(adm_axis_of(Vec3::Y), 2);
        assert_eq!(adm_axis_of(Vec3::Z), 0);
        assert_eq!(adm_axis_of(-Vec3::Z), 0);
    }

    /// The nearest node wins, and a value with nothing to snap to is left
    /// where it is.
    #[test]
    fn snapping_takes_the_nearest_node() {
        let nodes = [-1.0, -0.5, 0.0, 0.5, 1.0];
        assert_eq!(snap_to_nodes(0.2, &nodes), 0.0);
        assert_eq!(snap_to_nodes(0.3, &nodes), 0.5);
        assert_eq!(snap_to_nodes(-0.76, &nodes), -1.0);
        assert_eq!(snap_to_nodes(7.0, &nodes), 1.0);
        assert_eq!(snap_to_nodes(0.37, &[]), 0.37);
    }

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
    fn a_ray_meets_the_plane_it_points_at() {
        let hit = ray_plane(Vec3::new(0.0, 2.0, 0.0), Vec3::new(0.0, -1.0, 0.0), Vec3::Y);
        assert_eq!(hit, Some(Vec3::ZERO));
        // Parallel to the plane, and pointing away from it: no crossing.
        assert!(ray_plane(Vec3::new(0.0, 2.0, 0.0), Vec3::X, Vec3::Y).is_none());
        assert!(ray_plane(Vec3::new(0.0, 2.0, 0.0), Vec3::Y, Vec3::Y).is_none());
    }

    /// An axis drag moves the speaker by how far the pointer travelled along
    /// that axis, whatever angle the camera is at.
    #[test]
    fn the_axis_projection_follows_the_pointer_along_the_axis() {
        // A ray straight down onto the X axis at x = 3.
        let t = project_ray_onto_axis(
            Vec3::new(3.0, 5.0, 0.0),
            Vec3::new(0.0, -1.0, 0.0),
            Vec3::ZERO,
            Vec3::X,
        );
        assert!((t - 3.0).abs() < 1e-4, "t = {t}");
        // An axis that starts elsewhere is measured from where it starts.
        let t = project_ray_onto_axis(
            Vec3::new(3.0, 5.0, 0.0),
            Vec3::new(0.0, -1.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::X,
        );
        assert!((t - 2.0).abs() < 1e-4, "t = {t}");
    }

    /// The distance from the ring is the precision control, and inside it
    /// nothing is snapped at all.
    #[test]
    fn pulling_away_from_the_ring_coarsens_the_snap() {
        assert!((snap_drag_angle(44.7, 0.02) - 45.0).abs() < 1e-4);
        // On the ring, five degrees away from a multiple of five: untouched.
        assert!((snap_drag_angle(42.0, 0.02) - 42.0).abs() < 1e-4);
        // Pulled out: the same angle snaps to the five-degree grid.
        assert!((snap_drag_angle(44.0, 0.5) - 45.0).abs() < 1e-4);
        // Inside the ring: free.
        assert!((snap_drag_angle(44.7, -0.3) - 44.7).abs() < 1e-4);
    }

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
