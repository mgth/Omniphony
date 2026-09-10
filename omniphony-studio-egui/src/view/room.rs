//! Room box, faces, screen plane, axes triad and selection face shadows
//! (`scene/setup.js`, `controls/room-geometry.js`, `scene/axes.js`,
//! `scene/gizmos.js`, `speakers.js updateRoomFaceVisibility`).

use glam::{Mat4, Quat, Vec3};

use crate::model::app_state::{RoomRatio, VbapCartesian};
use crate::render::{
    FrameData, LineVertex, MeshInstance, MeshItem, MeshKind, hex_linear, with_alpha,
};

use super::Label;

/// Warped room bounds in scene units.
#[derive(Clone, Copy, Debug)]
pub struct RoomBounds {
    pub x_min: f32,
    pub x_max: f32,
    pub y_min: f32,
    pub y_max: f32,
    pub z_min: f32,
    pub z_max: f32,
}

impl RoomBounds {
    pub fn from_ratio(room: &RoomRatio) -> Self {
        let lower = if room.lower > 0.0 { room.lower } else { 0.5 };
        Self {
            x_min: -(room.rear.max(0.001)) as f32,
            x_max: room.length.max(0.001) as f32,
            y_min: -(lower.max(0.001)) as f32,
            y_max: room.height.max(0.001) as f32,
            z_min: -(room.width.max(0.001)) as f32,
            z_max: room.width.max(0.001) as f32,
        }
    }

    pub fn center(&self) -> Vec3 {
        Vec3::new(
            (self.x_min + self.x_max) * 0.5,
            (self.y_min + self.y_max) * 0.5,
            (self.z_min + self.z_max) * 0.5,
        )
    }

    pub fn size(&self) -> Vec3 {
        Vec3::new(
            self.x_max - self.x_min,
            self.y_max - self.y_min,
            self.z_max - self.z_min,
        )
    }

    pub fn clamp(&self, p: Vec3) -> Vec3 {
        Vec3::new(
            p.x.clamp(self.x_min, self.x_max),
            p.y.clamp(self.y_min, self.y_max),
            p.z.clamp(self.z_min, self.z_max),
        )
    }
}

/// The six room faces: key, inward normal, centre, and the quad model matrix.
fn faces(b: &RoomBounds) -> [(Vec3, Vec3, Mat4); 6] {
    let c = b.center();
    let s = b.size();
    let quad = |pos: Vec3, rot: Quat, w: f32, h: f32| {
        Mat4::from_scale_rotation_translation(Vec3::new(w, h, 1.0), rot, pos)
    };
    [
        // posX (front wall): PlaneGeometry rotated y = -π/2
        (
            Vec3::new(-1.0, 0.0, 0.0),
            Vec3::new(b.x_max, c.y, c.z),
            quad(
                Vec3::new(b.x_max, c.y, c.z),
                Quat::from_rotation_y(-std::f32::consts::FRAC_PI_2),
                s.z,
                s.y,
            ),
        ),
        (
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(b.x_min, c.y, c.z),
            quad(
                Vec3::new(b.x_min, c.y, c.z),
                Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
                s.z,
                s.y,
            ),
        ),
        (
            Vec3::new(0.0, -1.0, 0.0),
            Vec3::new(c.x, b.y_max, c.z),
            quad(
                Vec3::new(c.x, b.y_max, c.z),
                Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2),
                s.x,
                s.z,
            ),
        ),
        (
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(c.x, b.y_min, c.z),
            quad(
                Vec3::new(c.x, b.y_min, c.z),
                Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                s.x,
                s.z,
            ),
        ),
        (
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::new(c.x, c.y, b.z_max),
            quad(Vec3::new(c.x, c.y, b.z_max), Quat::IDENTITY, s.x, s.y),
        ),
        (
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(c.x, c.y, b.z_min),
            quad(
                Vec3::new(c.x, c.y, b.z_min),
                Quat::from_rotation_y(std::f32::consts::PI),
                s.x,
                s.y,
            ),
        ),
    ]
}

/// Box fill, edges, far-side faces and the screen plane.
pub fn emit_room(bounds: &RoomBounds, cam_pos: Vec3, frame: &mut FrameData) {
    // Fill: MeshBasicMaterial #4d6eff α 0.08, depth test on, write off.
    frame.meshes.push(MeshItem {
        kind: MeshKind::Cube,
        instance: MeshInstance::unlit(
            Mat4::from_scale_rotation_translation(bounds.size(), Quat::IDENTITY, bounds.center()),
            with_alpha(hex_linear(0x4d6eff), 0.08),
        ),
        blend: true,
        depth_test: true,
        order: 0,
    });

    // Edges: #6f8dff α 0.45, no depth test.
    let edge = with_alpha(hex_linear(0x6f8dff), 0.45);
    let (x0, x1, y0, y1, z0, z1) = (
        bounds.x_min,
        bounds.x_max,
        bounds.y_min,
        bounds.y_max,
        bounds.z_min,
        bounds.z_max,
    );
    let mut seg = |a: [f32; 3], b: [f32; 3]| {
        frame.overlay_lines.push(LineVertex {
            pos: a,
            color: edge,
        });
        frame.overlay_lines.push(LineVertex {
            pos: b,
            color: edge,
        });
    };
    for &x in &[x0, x1] {
        for &z in &[z0, z1] {
            seg([x, y0, z], [x, y1, z]);
        }
    }
    for &y in &[y0, y1] {
        for &x in &[x0, x1] {
            seg([x, y, z0], [x, y, z1]);
        }
        for &z in &[z0, z1] {
            seg([x0, y, z], [x1, y, z]);
        }
    }

    // Faces: #233047 α 0.18, double-sided, no depth; only the far side.
    let face_color = with_alpha(hex_linear(0x233047), 0.18);
    for (inward, pos, model) in faces(bounds) {
        if inward.dot(cam_pos - pos) > 0.0 {
            frame.meshes.push(MeshItem {
                kind: MeshKind::Quad,
                instance: MeshInstance::unlit(model, face_color),
                blend: true,
                depth_test: false,
                order: 1,
            });
        }
    }

    // Screen: 16:9 white α 0.18 on the front wall (`fitScreenToUpperHalf`).
    let avail_w = (z1 - z0).max(0.01);
    let avail_h = (y1 - y0).max(0.01);
    let mut h = 1.0f32;
    let mut w = h * 16.0 / 9.0;
    if h > avail_h {
        h = avail_h;
        w = h * 16.0 / 9.0;
    }
    if w > 2.0 {
        w = 2.0;
        h = w * 9.0 / 16.0;
    }
    if w > avail_w {
        w = avail_w;
        h = w * 9.0 / 16.0;
    }
    frame.meshes.push(MeshItem {
        kind: MeshKind::Quad,
        instance: MeshInstance::unlit(
            Mat4::from_scale_rotation_translation(
                Vec3::new(w, h, 1.0),
                Quat::from_rotation_y(-std::f32::consts::FRAC_PI_2),
                Vec3::new(x1 - 0.005, y0 + avail_h * 0.5, (z0 + z1) * 0.5),
            ),
            with_alpha(hex_linear(0xffffff), 0.18),
        ),
        blend: true,
        depth_test: false,
        order: 5,
    });
}

/// Axis triad (`scene/axes.js`): lines with a gap around the head, a cone on
/// the positive end, and a label beyond it. Labels are returned for egui.
pub fn emit_axes(
    frame: &mut FrameData,
    project: &dyn Fn(Vec3) -> Option<(egui::Pos2, f32)>,
    points_per_unit: &dyn Fn(f32) -> f32,
    labels: &mut Vec<Label>,
) {
    const GAP: f32 = 0.3;
    const EXTENT: f32 = 0.58;
    const ARROW_R: f32 = 0.02;
    const ARROW_H: f32 = 0.075;
    const LABEL_OFFSET: f32 = 0.12;
    let specs: [(&str, u32, Vec3); 3] = [
        ("Y", 0xff6b6b, Vec3::X),
        ("Z", 0x7fff7f, Vec3::Y),
        ("X", 0x6bb8ff, Vec3::Z),
    ];
    for (label, hex, dir) in specs {
        let c = hex_linear(hex);
        let line = with_alpha(c, 0.85);
        for (a, b) in [(GAP, EXTENT), (-GAP, -EXTENT)] {
            frame.overlay_lines.push(LineVertex {
                pos: (dir * a).to_array(),
                color: line,
            });
            frame.overlay_lines.push(LineVertex {
                pos: (dir * b).to_array(),
                color: line,
            });
        }
        let cone_pos = dir * (EXTENT + ARROW_H * 0.45);
        frame.meshes.push(MeshItem {
            kind: MeshKind::Cone,
            instance: MeshInstance::unlit(
                Mat4::from_scale_rotation_translation(
                    Vec3::new(ARROW_R, ARROW_H, ARROW_R),
                    Quat::from_rotation_arc(Vec3::Y, dir),
                    cone_pos,
                ),
                with_alpha(c, 0.92),
            ),
            blend: true,
            depth_test: false,
            order: 31,
        });
        let label_pos = dir * (EXTENT + ARROW_H + LABEL_OFFSET);
        if let Some((p, depth)) = project(label_pos) {
            let ppu = points_per_unit(depth);
            labels.push(Label {
                pos: p + egui::vec2(0.0, 0.03 * ppu),
                text: label.to_owned(),
                color: egui::Color32::from_rgb(
                    ((hex >> 16) & 0xff) as u8,
                    ((hex >> 8) & 0xff) as u8,
                    (hex & 0xff) as u8,
                ),
                size: (0.052 * ppu).clamp(6.0, 40.0).round(),
                depth,
            });
        }
    }
}

/// The seven room dimension guides (`room-geometry.js`).
///
/// Each is a line with a tick at both ends and a label in metres, laid just
/// outside the room so the numbers can be read against the box they describe.
/// They are shown only while the room panel is open, which is the moment those
/// numbers are being edited.
pub fn emit_dimension_guides(
    b: &RoomBounds,
    room: &RoomRatio,
    frame: &mut FrameData,
    project: &dyn Fn(Vec3) -> Option<(egui::Pos2, f32)>,
    points_per_unit: &dyn Fn(f32) -> f32,
    labels: &mut Vec<Label>,
) {
    const OFF: f32 = 0.08;
    const TICK: f32 = 0.04;
    /// The label sits a little past the tick, clear of the line.
    const LABEL_AT: f32 = 2.2;
    let mpu = room.scale_m.max(0.001) as f32;
    let y_top = b.y_max + 0.06;
    let guides: [(u32, Vec3, Vec3, Vec3, f32); 7] = [
        (
            0x88c7ff,
            Vec3::new(b.x_max + OFF, y_top, b.z_min),
            Vec3::new(b.x_max + OFF, y_top, b.z_max),
            Vec3::X,
            room.width as f32 * mpu * 2.0,
        ),
        (
            0xa0ffd1,
            Vec3::new(0.0, y_top, b.z_max + OFF),
            Vec3::new(b.x_max, y_top, b.z_max + OFF),
            Vec3::Z,
            room.length as f32 * mpu,
        ),
        (
            0xffd08a,
            Vec3::new(b.x_min, y_top, b.z_max + OFF),
            Vec3::new(0.0, y_top, b.z_max + OFF),
            Vec3::Z,
            room.rear as f32 * mpu,
        ),
        (
            0xb8b8ff,
            Vec3::new(b.x_min, y_top, b.z_min - OFF),
            Vec3::new(b.x_max, y_top, b.z_min - OFF),
            Vec3::Z,
            (room.length + room.rear) as f32 * mpu,
        ),
        (
            0xff9ed8,
            Vec3::new(b.x_max + OFF, 0.0, b.z_max + OFF),
            Vec3::new(b.x_max + OFF, b.y_max, b.z_max + OFF),
            Vec3::X,
            room.height as f32 * mpu,
        ),
        (
            0xff7a7a,
            Vec3::new(b.x_max + OFF, b.y_min, b.z_max + OFF),
            Vec3::new(b.x_max + OFF, 0.0, b.z_max + OFF),
            Vec3::X,
            room.lower as f32 * mpu,
        ),
        (
            0xffb3e6,
            Vec3::new(b.x_max + OFF, b.y_min, b.z_min - OFF),
            Vec3::new(b.x_max + OFF, b.y_max, b.z_min - OFF),
            Vec3::X,
            (room.height + room.lower) as f32 * mpu,
        ),
    ];
    for (hex, start, end, tick_dir, metres) in guides {
        let colour = with_alpha(hex_linear(hex), 0.85);
        let tick = tick_dir.normalize_or_zero() * TICK;
        let mut segment = |a: Vec3, z: Vec3| {
            frame.overlay_lines.push(LineVertex {
                pos: a.to_array(),
                color: colour,
            });
            frame.overlay_lines.push(LineVertex {
                pos: z.to_array(),
                color: colour,
            });
        };
        segment(start, end);
        segment(start - tick, start + tick);
        segment(end - tick, end + tick);
        if let Some((p, depth)) = project((start + end) * 0.5 + tick * LABEL_AT) {
            labels.push(Label {
                pos: p,
                text: format!("{metres:.2} m"),
                color: egui::Color32::from_rgb(
                    ((hex >> 16) & 0xff) as u8,
                    ((hex >> 8) & 0xff) as u8,
                    (hex & 0xff) as u8,
                ),
                size: (0.052 * points_per_unit(depth)).clamp(6.0, 40.0).round(),
                depth,
            });
        }
    }
}

/// The hybrid backend's iso-distance shape (`scene/hybrid-distance.js`).
///
/// A hybrid backend switches models at a distance, and its curve says where.
/// Selecting a point on that curve draws the surface that distance stands for,
/// so it is a place in the room rather than a number on an axis.
///
/// The web draws a translucent solid; this draws the same surface as a warped
/// wire grid. The renderer here takes instanced unit shapes, and the room's
/// depth warp is not a scale — it is a curve, and the whole point of the shape
/// is that it follows it. Every vertex is warped exactly, the way a position
/// is, which is what makes the shape line up with the speakers.
pub fn emit_hybrid_distance(
    radius_adm: f32,
    spherical: bool,
    room: &RoomRatio,
    frame: &mut FrameData,
) {
    // A zero radius is a point, not a surface; the web hides it too.
    if radius_adm <= 1e-4 {
        return;
    }
    let colour = with_alpha(hex_linear(0xffd166), 0.5);
    let point = |adm: Vec3| -> Vec3 {
        use omniphony_geometry::f64 as g;
        let o = adm * radius_adm;
        let scaled = g::room_scaled_position(
            [o.x as f64, o.y as f64, o.z as f64],
            [room.width, room.length, room.height],
            room.rear,
            room.lower,
            room.center_blend,
        );
        let s = g::adm_to_scene(scaled);
        Vec3::new(s[0] as f32, s[1] as f32, s[2] as f32)
    };
    let mut segment = |a: Vec3, b: Vec3| {
        frame.lines.push(LineVertex {
            pos: a.to_array(),
            color: colour,
        });
        frame.lines.push(LineVertex {
            pos: b.to_array(),
            color: colour,
        });
    };
    if spherical {
        // Latitudes and longitudes: enough to read as a surface, few enough to
        // stay a hint rather than a model.
        const RINGS: usize = 7;
        const SEGMENTS: usize = 24;
        let at = |t: f32, p: f32| {
            let (theta, phi) = (t * std::f32::consts::PI, p * std::f32::consts::TAU);
            Vec3::new(
                theta.sin() * phi.cos(),
                theta.sin() * phi.sin(),
                theta.cos(),
            )
        };
        for i in 1..RINGS {
            let t = i as f32 / RINGS as f32;
            for j in 0..SEGMENTS {
                let (p0, p1) = (j as f32 / SEGMENTS as f32, (j + 1) as f32 / SEGMENTS as f32);
                segment(point(at(t, p0)), point(at(t, p1)));
            }
        }
        for j in 0..SEGMENTS {
            let p = j as f32 / SEGMENTS as f32;
            for i in 0..RINGS {
                let (t0, t1) = (i as f32 / RINGS as f32, (i + 1) as f32 / RINGS as f32);
                segment(point(at(t0, p)), point(at(t1, p)));
            }
        }
    } else {
        // The cube's twelve edges, each subdivided so the depth warp shows.
        const STEPS: usize = 8;
        let corner = |i: usize| {
            Vec3::new(
                if i & 1 == 0 { -1.0 } else { 1.0 },
                if i & 2 == 0 { -1.0 } else { 1.0 },
                if i & 4 == 0 { -1.0 } else { 1.0 },
            )
        };
        for a in 0..8usize {
            for bit in [1usize, 2, 4] {
                let b = a | bit;
                if b == a {
                    continue;
                }
                let (from, to) = (corner(a), corner(b));
                for step in 0..STEPS {
                    let (t0, t1) = (step as f32 / STEPS as f32, (step + 1) as f32 / STEPS as f32);
                    segment(point(from.lerp(to, t0)), point(from.lerp(to, t1)));
                }
            }
        }
    }
}

/// Six black discs projected on the walls for a selected speaker/object
/// (`updateSelected*FaceShadows`).
pub fn emit_face_shadows(p: Vec3, b: &RoomBounds, frame: &mut FrameData) {
    const EPS: f32 = 0.01;
    const BASE: f32 = 0.08;
    let c = b.clamp(p);
    let span = b.size().max(Vec3::splat(1e-6));
    let shadows: [(Vec3, f32, f32, Quat); 6] = [
        (
            Vec3::new(b.x_max - EPS, c.y, c.z),
            (b.x_max - p.x).abs(),
            span.x,
            Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
        ),
        (
            Vec3::new(b.x_min + EPS, c.y, c.z),
            (b.x_min - p.x).abs(),
            span.x,
            Quat::from_rotation_y(-std::f32::consts::FRAC_PI_2),
        ),
        (
            Vec3::new(c.x, b.y_max - EPS, c.z),
            (b.y_max - p.y).abs(),
            span.y,
            Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2),
        ),
        (
            Vec3::new(c.x, b.y_min + EPS, c.z),
            (b.y_min - p.y).abs(),
            span.y,
            Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
        ),
        (
            Vec3::new(c.x, c.y, b.z_max - EPS),
            (b.z_max - p.z).abs(),
            span.z,
            Quat::from_rotation_y(std::f32::consts::PI),
        ),
        (
            Vec3::new(c.x, c.y, b.z_min + EPS),
            (b.z_min - p.z).abs(),
            span.z,
            Quat::IDENTITY,
        ),
    ];
    for (pos, dist, max_dist, rot) in shadows {
        let t = if max_dist > 1e-6 {
            (1.0 - dist / max_dist).clamp(0.08, 1.0)
        } else {
            1.0
        };
        let scale = BASE * (0.7 + 0.6 * t);
        frame.meshes.push(MeshItem {
            kind: MeshKind::Disc,
            instance: MeshInstance::unlit(
                Mat4::from_scale_rotation_translation(Vec3::splat(scale), rot, pos),
                [0.0, 0.0, 0.0, 0.06 + 0.18 * t],
            ),
            blend: true,
            depth_test: false,
            order: 3,
        });
    }
}

/// VBAP cartesian face grids (`scene/gizmos.js`): the evaluation grid's
/// nodes drawn on the visible room faces, `#66d8ff` α 0.42, no depth test.
pub fn emit_vbap_grids(
    b: &RoomBounds,
    room: &RoomRatio,
    cam_pos: Vec3,
    sizes: &VbapCartesian,
    frame: &mut FrameData,
) {
    use omniphony_geometry::f64 as geometry;
    let (Some(xs_n), Some(ys_n), Some(zs_n)) = (sizes.x_size, sizes.y_size, sizes.z_size) else {
        return;
    };
    let z_neg = sizes.z_neg_size.unwrap_or(0);
    if xs_n < 2 || ys_n < 2 || zs_n < 2 {
        return;
    }
    let axis = |min: f32, max: f32, n: u32| -> Vec<f32> {
        (0..n)
            .map(|i| min + (max - min) * i as f32 / (n - 1).max(1) as f32)
            .collect()
    };
    // Scene x (depth) from ADM y nodes, through the depth warp.
    let xs: Vec<f32> = axis(-1.0, 1.0, ys_n + 1)
        .into_iter()
        .map(|v| {
            geometry::map_depth(f64::from(v), room.length, room.rear, room.center_blend) as f32
        })
        .collect();
    // Scene y (height): lower half without its last node, then the upper half.
    let mut ys: Vec<f32> = if z_neg >= 1 {
        let mut lower = axis(b.y_min, 0.0, z_neg + 1);
        lower.pop();
        lower
    } else {
        Vec::new()
    };
    ys.extend(axis(0.0, b.y_max, zs_n + 1));
    // Scene z (width) from ADM x nodes.
    let zs = axis(b.z_min, b.z_max, xs_n + 1);

    let color = with_alpha(hex_linear(0x66d8ff), 0.42);
    let mut seg = |a: [f32; 3], c: [f32; 3]| {
        frame.overlay_lines.push(LineVertex { pos: a, color });
        frame.overlay_lines.push(LineVertex { pos: c, color });
    };
    for (index, (inward, pos, _)) in faces(b).into_iter().enumerate() {
        if inward.dot(cam_pos - pos) <= 0.0 {
            continue;
        }
        match index {
            0 | 1 => {
                let x = if index == 0 { b.x_max } else { b.x_min };
                for &y in &ys {
                    seg([x, y, b.z_min], [x, y, b.z_max]);
                }
                for &z in &zs {
                    seg([x, b.y_min, z], [x, b.y_max, z]);
                }
            }
            2 | 3 => {
                let y = if index == 2 { b.y_max } else { b.y_min };
                for &x in &xs {
                    seg([x, y, b.z_min], [x, y, b.z_max]);
                }
                for &z in &zs {
                    seg([b.x_min, y, z], [b.x_max, y, z]);
                }
            }
            _ => {
                let z = if index == 4 { b.z_max } else { b.z_min };
                for &x in &xs {
                    seg([x, b.y_min, z], [x, b.y_max, z]);
                }
                for &y in &ys {
                    seg([b.x_min, y, z], [b.x_max, y, z]);
                }
            }
        }
    }
}
