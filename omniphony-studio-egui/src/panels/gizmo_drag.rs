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
//! would otherwise snap the channel back between updates.

use std::time::{Duration, Instant};

use egui::{Pos2, Rect};
use glam::Vec3;

use crate::app::StudioSpike;
use crate::view::gizmos::{
    self, EditMode, GizmoTarget, project_ray_onto_axis, ray_plane, snap_drag_angle, spherical,
};

/// How close to the ring or the arc a press has to be to grab it, as a
/// fraction of the radius. A line is one pixel wide and nobody can hit that.
const GRAB: f32 = 0.12;
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
        match gizmo.mode {
            EditMode::Polar if gizmo.polar_armed => {
                // The ring lies in the horizontal plane; the arc stands in the
                // speaker's own azimuth plane. Whichever the press landed on
                // is the angle being dragged.
                if let Some(hit) = ray_plane(origin, dir, Vec3::Y) {
                    let radial = (hit.x * hit.x + hit.z * hit.z).sqrt();
                    if (radial - distance).abs() <= GRAB * distance {
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
                    && (hit.length() - distance).abs() <= GRAB * distance
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
                    // The handle is a sphere; a press inside it takes its axis.
                    let to_handle = handle - origin;
                    let along = to_handle.dot(dir);
                    if along <= 0.0 {
                        continue;
                    }
                    if (to_handle - dir * along).length() <= radius * 1.6 {
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
    pub(crate) fn update_gizmo_drag(&mut self, pointer: Pos2, rect: Rect, aspect: f32) {
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
                start_pos + axis * (t - start_t)
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
        // The frame's own copy moves at once, so the gizmo tracks the pointer
        // rather than the next state broadcast.
        self.gizmo_target = Some((target.clone(), scene));
        let room = self.host.read().app.room_ratio.clone();
        let adm = gizmos::scene_to_normalized(scene, &room);
        match target {
            GizmoTarget::Speaker(index) => {
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
