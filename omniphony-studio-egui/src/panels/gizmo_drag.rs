//! Dragging the edit gizmos (`picking.js beginSpeakerDrag` and friends).
//!
//! The gizmo is the handle; this is what happens when it is pulled. Three
//! drags: the azimuth ring turns the speaker around the listener, the
//! elevation arc raises it in its own vertical plane, and a cartesian handle
//! slides it along one axis.
//!
//! Two rules from the web are what make it usable. The camera does not move
//! while a gizmo is being dragged — an orbit under a drag would move the thing
//! you are aiming with — and a speaker's edit is sent once, on release, while
//! a virtual bed channel's is sent continuously, because the renderer's bed
//! would otherwise snap the channel back between updates. Either way the
//! frame draws the target from the renderer's state, so the drag pins the
//! target at the pointer for as long as it lasts, and a little beyond.
//!
//! A cartesian drag lands on the VBAP cartesian grid, the one the room faces
//! can show, unless the command modifier (ctrl) is held; the polar drags
//! snap by angle, and are freed by pulling inside the ring.

use std::time::{Duration, Instant};

use egui::{Pos2, Rect};
use glam::Vec3;

use crate::app::StudioSpike;
use crate::model::app_state::RoomRatio;
use crate::model::layouts::Speaker;
use crate::view::gizmos::{
    self, EditMode, GizmoTarget, project_ray_onto_axis, ray_plane, snap_drag_angle, spherical,
};

/// How close to the ring or the arc a press has to be to grab it, as a
/// fraction of the radius. A line is one pixel wide and nobody can hit that.
const GRAB: f32 = 0.12;
/// The least a grab may demand on screen, in points. The cartesian handles
/// are drawn a few points across once the camera is at any distance; a
/// target that small is not one.
const MIN_GRAB_PX: f32 = 10.0;
/// The wheel's distance steps, coarse and fine.
const WHEEL_STEP: f32 = 0.05;
const WHEEL_STEP_FINE: f32 = 0.01;

#[derive(Clone, Copy, Debug)]
pub enum DragKind {
    Azimuth,
    Elevation,
    Cartesian {
        axis: Vec3,
        origin: Vec3,
        start_t: f32,
        start_pos: Vec3,
    },
}

/// A drag in progress.
#[derive(Clone, Debug)]
pub struct GizmoDrag {
    pub kind: DragKind,
    /// The spherical position being edited, kept across the drag so the two
    /// angles and the distance do not fight each other.
    pub az_deg: f32,
    pub el_deg: f32,
    pub distance: f32,
}

impl StudioSpike {
    /// Try to grab a gizmo. Returns true when one was taken, which is also
    /// what tells the viewport not to orbit.
    pub(crate) fn begin_gizmo_drag(&mut self, pointer: Pos2, rect: Rect, aspect: f32) -> bool {
        let Some((_, at)) = self.gizmo_target.clone() else {
            return false;
        };
        let gizmo = self.settings.gizmo;
        let (origin, dir) = self.viewport_ray(pointer, rect, aspect);
        let (az, el, dist) = spherical(at);
        let distance = dist.max(0.01);
        let eye = self.camera.eye();
        let fov_y = self.camera.fov_y;
        let height = rect.height();
        match gizmo.mode {
            EditMode::Polar if gizmo.polar_armed => {
                // The ring lies in the horizontal plane; the arc stands in the
                // speaker's own azimuth plane. Whichever the press landed on
                // is the angle being dragged. The slack is the web's fraction
                // of the radius, but never under the on-screen floor.
                let slack = |hit: Vec3| {
                    (GRAB * distance).max(
                        MIN_GRAB_PX * scene_units_per_point((hit - eye).length(), fov_y, height),
                    )
                };
                if let Some(hit) = ray_plane(origin, dir, Vec3::Y) {
                    let radial = (hit.x * hit.x + hit.z * hit.z).sqrt();
                    if (radial - distance).abs() <= slack(hit) {
                        self.gizmo_drag = Some(GizmoDrag {
                            kind: DragKind::Azimuth,
                            az_deg: az,
                            el_deg: el,
                            distance,
                        });
                        return true;
                    }
                }
                let normal = arc_normal(az);
                if let Some(hit) = ray_plane(origin, dir, normal)
                    && (hit.length() - distance).abs() <= slack(hit)
                {
                    self.gizmo_drag = Some(GizmoDrag {
                        kind: DragKind::Elevation,
                        az_deg: az,
                        el_deg: el,
                        distance,
                    });
                    return true;
                }
                false
            }
            EditMode::Cartesian if gizmo.cartesian_armed => {
                let scale = ((self.camera.eye() - at).length() * 0.08).max(0.2);
                let radius = 0.045 * scale;
                for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
                    let handle = at + axis * 0.45 * scale;
                    // The handle is a sphere; a press inside it, or within
                    // the on-screen floor of it, takes its axis.
                    let to_handle = handle - origin;
                    let along = to_handle.dot(dir);
                    if along <= 0.0 {
                        continue;
                    }
                    let grab = grab_radius(radius, (handle - eye).length(), fov_y, height);
                    if (to_handle - dir * along).length() <= grab {
                        self.gizmo_drag = Some(GizmoDrag {
                            kind: DragKind::Cartesian {
                                axis,
                                origin: at,
                                start_t: project_ray_onto_axis(origin, dir, at, axis),
                                start_pos: at,
                            },
                            az_deg: az,
                            el_deg: el,
                            distance,
                        });
                        return true;
                    }
                }
                false
            }
            _ => false,
        }
    }

    /// Follow the pointer. `send` is false here: a speaker commits on release.
    /// `free` lifts the cartesian grid snap for this step.
    pub(crate) fn update_gizmo_drag(&mut self, pointer: Pos2, rect: Rect, aspect: f32, free: bool) {
        let Some(drag) = self.gizmo_drag.clone() else {
            return;
        };
        let (origin, dir) = self.viewport_ray(pointer, rect, aspect);
        let mut next = drag.clone();
        let position = match drag.kind {
            DragKind::Azimuth => {
                let Some(hit) = ray_plane(origin, dir, Vec3::Y) else {
                    return;
                };
                let radial = (hit.x * hit.x + hit.z * hit.z).sqrt();
                let raw = hit.z.atan2(hit.x).to_degrees();
                next.az_deg = snap_drag_angle(
                    omniphony_geometry::f32::normalize_deg(raw),
                    (radial - drag.distance) / drag.distance,
                );
                from_spherical(next.az_deg, next.el_deg, next.distance)
            }
            DragKind::Elevation => {
                let Some(hit) = ray_plane(origin, dir, arc_normal(drag.az_deg)) else {
                    return;
                };
                let planar = (hit.x * hit.x + hit.z * hit.z).sqrt();
                let raw = hit.y.atan2(planar).to_degrees().clamp(-90.0, 90.0);
                next.el_deg = snap_drag_angle(raw, (hit.length() - drag.distance) / drag.distance);
                from_spherical(next.az_deg, next.el_deg, next.distance)
            }
            DragKind::Cartesian {
                axis,
                origin: axis_origin,
                start_t,
                start_pos,
            } => {
                let t = project_ray_onto_axis(origin, dir, axis_origin, axis);
                let pulled = start_pos + axis * (t - start_t);
                // On the grid unless freed: the moved coordinate alone, so
                // the other two stay wherever the layout has them.
                if free {
                    pulled
                } else {
                    self.snapped_along(pulled, axis)
                }
            }
        };
        self.gizmo_drag = Some(next);
        self.commit_gizmo_position(position, false);
    }

    /// Let go: the position is committed for good, and sent.
    pub(crate) fn end_gizmo_drag(&mut self) {
        let Some(drag) = self.gizmo_drag.take() else {
            return;
        };
        let Some((_, at)) = self.gizmo_target.clone() else {
            return;
        };
        let _ = drag;
        self.commit_gizmo_position(at, true);
    }

    /// The wheel, held with a modifier, moves the target closer or further.
    /// Without one it is the camera's, which is what a wheel over a 3D view
    /// normally is.
    pub(crate) fn gizmo_wheel(&mut self, scroll: f32, fine: bool) -> bool {
        let gizmo = self.settings.gizmo;
        if gizmo.mode != EditMode::Polar || !gizmo.polar_armed {
            return false;
        }
        let Some((target, at)) = self.gizmo_target.clone() else {
            return false;
        };
        let (az, el, dist) = spherical(at);
        let step = if fine { WHEEL_STEP_FINE } else { WHEEL_STEP };
        let next = (dist + scroll.signum() * step).clamp(0.2, 2.0);
        if (next - dist).abs() < 1e-6 {
            return true;
        }
        // A channel is sent per tick: its position lives in the renderer's bed,
        // which would put it back where it was between ticks otherwise.
        let send = matches!(target, GizmoTarget::Channel(_));
        self.commit_gizmo_position(from_spherical(az, el, next), send);
        true
    }

    /// `scene` with its coordinate along `axis` moved to the nearest node of
    /// the VBAP cartesian grid, the object test's cached one; unchanged while
    /// the renderer has published none.
    fn snapped_along(&mut self, scene: Vec3, axis: Vec3) -> Vec3 {
        if !self.ensure_vbap_grid() {
            return scene;
        }
        let Some((_, axes)) = self.vbap_grid_cache.as_ref() else {
            return scene;
        };
        let adm_axis = gizmos::adm_axis_of(axis);
        let room = self.host.read().app.room_ratio.clone();
        let mut adm = gizmos::scene_to_normalized(scene, &room);
        adm[adm_axis] = gizmos::snap_to_nodes(adm[adm_axis], &axes[adm_axis]);
        crate::view::scene_position(adm, &room)
    }

    /// The speaker as the editor should show it while the gizmo holds it:
    /// at the pin, with the polar readout the renderer will derive from the
    /// cartesian edit. `None` when the state is the right source.
    pub(crate) fn speaker_at_edit_pin(&self, index: usize, speaker: &Speaker) -> Option<Speaker> {
        let (pinned, scene, _) = self.speaker_edit_pin.as_ref()?;
        if *pinned != index {
            return None;
        }
        let room = self.host.read().app.room_ratio.clone();
        Some(speaker_at(
            speaker,
            gizmos::scene_to_normalized(*scene, &room),
        ))
    }

    fn viewport_ray(&self, pointer: Pos2, rect: Rect, aspect: f32) -> (Vec3, Vec3) {
        let ndc_x = (pointer.x - rect.min.x) / rect.width() * 2.0 - 1.0;
        let ndc_y = 1.0 - (pointer.y - rect.min.y) / rect.height() * 2.0;
        self.camera
            .ray(ndc_x, ndc_y, aspect, [rect.width(), rect.height()])
    }

    /// Put the target at a scene position: locally always, on the wire when
    /// asked.
    fn commit_gizmo_position(&mut self, scene: Vec3, send: bool) {
        let Some((target, _)) = self.gizmo_target.clone() else {
            return;
        };
        let room = self.host.read().app.room_ratio.clone();
        let adm = gizmos::scene_to_normalized(scene, &room);
        // Both targets live in the layout's cube: the conversion above clamps
        // to it, and so does the bed's `polar_to_adm` for a channel. Anchoring
        // and pinning the clamped position holds the target at the wall while
        // the pointer is beyond it, instead of letting it out and snapping it
        // back on release.
        let scene = clamped_to_layout(scene, &room);
        // The frame's own copy moves at once, so the gizmo tracks the pointer
        // rather than the next state broadcast.
        self.gizmo_target = Some((target.clone(), scene));
        match target {
            GizmoTarget::Speaker(index) => {
                // Hold the cube, and so its gizmo, at the pointer: the frame
                // draws speakers from the renderer's state, which only learns
                // of the move on release. 600 ms past that, as for a channel,
                // covers the echo; no expiry while the pointer is down.
                self.speaker_edit_pin = Some((
                    index,
                    scene,
                    send.then(|| Instant::now() + Duration::from_millis(600)),
                ));
                if send {
                    self.edit_speaker_position(index as i32, adm);
                }
            }
            GizmoTarget::Channel(name) => {
                // Hold the object here until the renderer has had time to echo
                // the new bed back: 600 ms, as in the web, and no expiry at all
                // while the pointer is still down.
                self.channel_edit_pin = Some((
                    name.clone(),
                    scene,
                    send.then(|| Instant::now() + Duration::from_millis(600)),
                ));
                let (az, el, dist) = spherical(scene);
                self.set_channel_polar_from_drag(
                    &name,
                    f64::from(az),
                    f64::from(el),
                    f64::from(dist.max(0.01)),
                    send,
                );
            }
        }
    }
}

/// `speaker` moved to the normalised `adm` position, its polar readout
/// derived the way the renderer derives it from a cartesian edit.
pub(crate) fn speaker_at(speaker: &Speaker, adm: [f64; 3]) -> Speaker {
    let (azimuth_deg, elevation_deg, distance_m) =
        omniphony_geometry::f64::hydrate_from_cartesian(adm[0], adm[1], adm[2]);
    Speaker {
        x: adm[0],
        y: adm[1],
        z: adm[2],
        azimuth_deg,
        elevation_deg,
        distance_m,
        ..speaker.clone()
    }
}

/// The scene position a speaker can actually take: the layout is written in
/// the normalised cube, so the round trip through it stops at the walls.
pub(crate) fn clamped_to_layout(scene: Vec3, room: &RoomRatio) -> Vec3 {
    crate::view::scene_position(gizmos::scene_to_normalized(scene, room), room)
}

/// The scene-space size of one point of the viewport at `depth` from the eye,
/// for a vertical field of view `fov_y` over a viewport `height` points tall.
pub(crate) fn scene_units_per_point(depth: f32, fov_y: f32, height: f32) -> f32 {
    2.0 * depth * (fov_y * 0.5).tan() / height.max(1.0)
}

/// How far from a handle a press may land and still take it: the handle's own
/// radius with the web's slack, but never less than `MIN_GRAB_PX` on screen.
pub(crate) fn grab_radius(handle_radius: f32, depth: f32, fov_y: f32, height: f32) -> f32 {
    (handle_radius * 1.6).max(MIN_GRAB_PX * scene_units_per_point(depth, fov_y, height))
}

/// The normal of the vertical plane the elevation arc stands in.
fn arc_normal(az_deg: f32) -> Vec3 {
    let az = az_deg.to_radians();
    Vec3::new(az.cos(), 0.0, az.sin())
        .cross(Vec3::Y)
        .normalize_or_zero()
}

/// `sphericalToCartesianDeg` in scene axes.
fn from_spherical(az_deg: f32, el_deg: f32, distance: f32) -> Vec3 {
    let (az, el) = (az_deg.to_radians(), el_deg.to_radians());
    Vec3::new(
        distance * el.cos() * az.cos(),
        distance * el.sin(),
        distance * el.cos() * az.sin(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A held speaker reads at the pin, in both coordinate systems, and keeps
    /// everything that is not a position.
    #[test]
    fn the_readout_follows_the_pin_in_both_coordinate_systems() {
        let speaker: Speaker = serde_json::from_value(serde_json::json!({
            "id": "Ltf", "x": -0.5, "y": 0.5, "z": 0.0, "delay_ms": 3.5
        }))
        .unwrap();
        let held = speaker_at(&speaker, [0.2, 0.9, 0.3]);
        assert_eq!((held.x, held.y, held.z), (0.2, 0.9, 0.3));
        let (az, el, dist) = omniphony_geometry::f64::hydrate_from_cartesian(0.2, 0.9, 0.3);
        assert_eq!(
            (held.azimuth_deg, held.elevation_deg, held.distance_m),
            (az, el, dist)
        );
        assert!(az > 0.0 && el > 0.0 && dist > 0.9, "{az} {el} {dist}");
        assert_eq!(held.id, "Ltf");
        assert_eq!(held.delay_ms, 3.5);
    }

    /// The layout's cube is the limit a speaker stops at: a point inside
    /// comes back where it was, a point beyond a wall comes back on it.
    #[test]
    fn a_speaker_stops_at_the_layout_cube() {
        let room = RoomRatio::default();
        let inside = Vec3::new(0.3, 0.2, -0.4);
        let back = clamped_to_layout(inside, &room);
        assert!((back - inside).length() < 1e-4, "{back:?}");
        let beyond = inside * 40.0;
        let wall = clamped_to_layout(beyond, &room);
        let adm = gizmos::scene_to_normalized(wall, &room);
        assert!(adm.iter().all(|c| c.abs() <= 1.0 + 1e-6), "{adm:?}");
        assert!(
            adm.iter().any(|c| (c.abs() - 1.0).abs() < 1e-6),
            "not on a wall: {adm:?}"
        );
        assert!(wall.length() < beyond.length());
        // And the wall is where it stays: the clamp is idempotent.
        assert!((clamped_to_layout(wall, &room) - wall).length() < 1e-4);
    }

    /// A handle the camera has shrunk to a few points still takes a press
    /// within the on-screen floor; a big one keeps its own radius and the
    /// web's slack.
    #[test]
    fn a_grab_is_never_smaller_than_the_on_screen_floor() {
        let fov_y = 65f32.to_radians();
        let unit = scene_units_per_point(10.0, fov_y, 1000.0);
        assert!((unit - 2.0 * 10.0 * (fov_y / 2.0).tan() / 1000.0).abs() < 1e-7);
        // The cartesian handle at that depth: 0.045 * 0.08 * 10 = 0.036 scene
        // units, under three points on screen.
        let small = grab_radius(0.036, 10.0, fov_y, 1000.0);
        assert!((small - MIN_GRAB_PX * unit).abs() < 1e-6, "{small}");
        assert!(small > 0.036 * 1.6);
        // A handle already wider than the floor keeps the web's rule.
        let large = grab_radius(1.0, 10.0, fov_y, 1000.0);
        assert!((large - 1.6).abs() < 1e-6, "{large}");
    }

    /// The arc stands in the plane that contains the speaker and the vertical,
    /// so its normal is horizontal and square to the speaker's direction.
    #[test]
    fn the_elevation_plane_contains_the_speaker_and_the_vertical() {
        for az in [0.0, 37.0, -90.0, 179.0] {
            let normal = arc_normal(az);
            assert!(normal.y.abs() < 1e-6, "the plane is not vertical at {az}");
            let direction = from_spherical(az, 0.0, 1.0);
            assert!(
                normal.dot(direction).abs() < 1e-5,
                "the speaker is not in its own plane at {az}"
            );
            assert!(normal.dot(Vec3::Y).abs() < 1e-6);
        }
    }

    /// The round trip through spherical is what a polar drag writes back, so
    /// it has to land where it started.
    #[test]
    fn the_spherical_round_trip_is_stable() {
        for p in [
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(-0.5, 0.7, 1.2),
            Vec3::new(0.0, -2.0, 0.0),
        ] {
            let (az, el, d) = spherical(p);
            let back = from_spherical(az, el, d);
            assert!((back - p).length() < 1e-5, "{p:?} came back as {back:?}");
        }
    }
}
