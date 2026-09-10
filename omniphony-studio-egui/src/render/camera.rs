//! Orbit camera reproducing the Studio's three.js `OrbitControls` setup
//! (`scene/setup.js`): vertical FOV 65°, near 0.1, far 100, pivot
//! `HEAD_PIVOT = (0, 0.25, 0)`, initial eye `(-3.8, 1.1, 0)`, damping 0.06,
//! rotate speed relative to the viewport height, dolly by `0.95^x`, and the
//! right-drag "pan" implemented as a lens shift rather than a target move.
//!
//! Frame: three.js scene axes, `x` = depth (front +), `y` = up, `z` = right.
//! Spherical convention is three's: `x = r sinφ sinθ`, `y = r cosφ`,
//! `z = r sinφ cosθ`.

use glam::{Mat4, Vec3};

const DAMPING: f32 = 0.06;
const PHI_EPS: f32 = 1e-6;

pub struct OrbitCamera {
    pub target: Vec3,
    pub theta: f32,
    pub phi: f32,
    pub radius: f32,
    pub fov_y: f32,
    /// Lens-shift pan in points (`panX`, `panY` of `setup.js`).
    pub pan: [f32; 2],
    delta_theta: f32,
    delta_phi: f32,
    scale: f32,
}

impl OrbitCamera {
    pub const NEAR: f32 = 0.1;
    pub const FAR: f32 = 100.0;

    pub fn new() -> Self {
        let target = Vec3::new(0.0, 0.25, 0.0);
        let offset = Vec3::new(-3.8, 1.1, 0.0) - target;
        let radius = offset.length();
        Self {
            target,
            theta: offset.x.atan2(offset.z),
            phi: (offset.y / radius).clamp(-1.0, 1.0).acos(),
            radius,
            fov_y: 65f32.to_radians(),
            pan: [0.0, 0.0],
            delta_theta: 0.0,
            delta_phi: 0.0,
            scale: 1.0,
        }
    }

    pub fn eye(&self) -> Vec3 {
        let (st, ct) = self.theta.sin_cos();
        let (sp, cp) = self.phi.sin_cos();
        self.target + self.radius * Vec3::new(sp * st, cp, sp * ct)
    }

    pub fn view(&self) -> Mat4 {
        Mat4::look_at_rh(self.eye(), self.target, Vec3::Y)
    }

    /// Projection with the lens shift applied (`camera.setViewOffset`):
    /// `x_ndc -= 2·panX/W`, `y_ndc += 2·panY/H`.
    pub fn proj(&self, aspect: f32, viewport_points: [f32; 2]) -> Mat4 {
        let p = Mat4::perspective_rh(self.fov_y, aspect.max(1e-3), Self::NEAR, Self::FAR);
        let shift = Mat4::from_translation(Vec3::new(
            -2.0 * self.pan[0] / viewport_points[0].max(1.0),
            2.0 * self.pan[1] / viewport_points[1].max(1.0),
            0.0,
        ));
        shift * p
    }

    pub fn view_proj(&self, aspect: f32, viewport_points: [f32; 2]) -> Mat4 {
        self.proj(aspect, viewport_points) * self.view()
    }

    /// Camera-space right and up axes in world space (for billboards).
    pub fn basis(&self) -> (Vec3, Vec3) {
        let view = self.view();
        let right = Vec3::new(view.x_axis.x, view.y_axis.x, view.z_axis.x);
        let up = Vec3::new(view.x_axis.y, view.y_axis.y, view.z_axis.y);
        (right, up)
    }

    /// Left drag: OrbitControls `rotateLeft/Up` with both axes scaled by the
    /// viewport height.
    pub fn rotate(&mut self, dx_points: f32, dy_points: f32, viewport_height: f32) {
        let h = viewport_height.max(1.0);
        self.delta_theta -= std::f32::consts::TAU * dx_points / h;
        self.delta_phi -= std::f32::consts::TAU * dy_points / h;
    }

    /// Mouse wheel: `deltaY` in points; up = closer.
    pub fn dolly_wheel(&mut self, delta_y: f32) {
        let f = 0.95f32.powf((delta_y * 0.01).abs());
        if delta_y < 0.0 {
            self.scale *= f;
        } else if delta_y > 0.0 {
            self.scale /= f;
        }
    }

    /// Middle drag: `dy > 0` moves away.
    pub fn dolly_drag(&mut self, dy_points: f32) {
        let f = 0.95f32.powf((dy_points * 0.01).abs());
        if dy_points > 0.0 {
            self.scale /= f;
        } else if dy_points < 0.0 {
            self.scale *= f;
        }
    }

    /// Right drag: lens shift, image follows the cursor 1:1.
    pub fn pan(&mut self, dx_points: f32, dy_points: f32) {
        self.pan[0] -= dx_points;
        self.pan[1] -= dy_points;
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// One `controls.update()`: apply damped deltas. Returns true while the
    /// motion is still decaying, so the caller keeps repainting.
    pub fn update(&mut self) -> bool {
        self.theta += self.delta_theta * DAMPING;
        self.phi =
            (self.phi + self.delta_phi * DAMPING).clamp(PHI_EPS, std::f32::consts::PI - PHI_EPS);
        self.radius = (self.radius * self.scale).clamp(0.05, 200.0);
        self.scale = 1.0;
        self.delta_theta *= 1.0 - DAMPING;
        self.delta_phi *= 1.0 - DAMPING;
        self.delta_theta.abs() > 1e-5 || self.delta_phi.abs() > 1e-5
    }

    /// World-space ray through a normalized device coordinate (y up).
    pub fn ray(
        &self,
        ndc_x: f32,
        ndc_y: f32,
        aspect: f32,
        viewport_points: [f32; 2],
    ) -> (Vec3, Vec3) {
        let inv = self.view_proj(aspect, viewport_points).inverse();
        let near = inv.project_point3(Vec3::new(ndc_x, ndc_y, 0.0));
        let far = inv.project_point3(Vec3::new(ndc_x, ndc_y, 1.0));
        (near, (far - near).normalize_or_zero())
    }
}
