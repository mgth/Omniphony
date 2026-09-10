//! Room box, faces, screen plane, axes triad and selection face shadows
//! (`scene/setup.js`, `controls/room-geometry.js`, `scene/axes.js`,
//! `scene/gizmos.js`, `speakers.js updateRoomFaceVisibility`).

use glam::{Mat4, Quat, Vec3};

use crate::model::app_state::RoomRatio;
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
                size: (0.052 * ppu).clamp(6.0, 40.0),
                depth,
            });
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
