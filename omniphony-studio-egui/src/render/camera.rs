//! Orbit camera in the layout frame (x right, y front, z up).

use glam::{Mat4, Vec3};

pub struct OrbitCamera {
    pub target: Vec3,
    /// Rotation around +z, radians. 0 looks along +y from behind the listener.
    pub yaw: f32,
    /// Elevation above the horizon, radians.
    pub pitch: f32,
    pub distance: f32,
    pub fov_y: f32,
}

impl OrbitCamera {
    pub const NEAR: f32 = 0.05;
    pub const FAR: f32 = 100.0;

    pub fn new() -> Self {
        Self {
            target: Vec3::new(0.0, 0.0, 0.1),
            yaw: 0.55,
            pitch: 0.42,
            distance: 4.4,
            fov_y: 45f32.to_radians(),
        }
    }

    pub fn eye(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        self.target + self.distance * Vec3::new(cp * sy, -cp * cy, sp)
    }

    pub fn view(&self) -> Mat4 {
        Mat4::look_at_rh(self.eye(), self.target, Vec3::Z)
    }

    /// wgpu clip space: depth in `[0, 1]`.
    pub fn proj(&self, aspect: f32) -> Mat4 {
        Mat4::perspective_rh(self.fov_y, aspect.max(1e-3), Self::NEAR, Self::FAR)
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        self.proj(aspect) * self.view()
    }

    pub fn orbit(&mut self, dx_points: f32, dy_points: f32) {
        self.yaw += dx_points * 0.008;
        self.pitch = (self.pitch + dy_points * 0.008).clamp(-1.45, 1.45);
    }

    pub fn zoom(&mut self, scroll_points: f32) {
        let factor = (1.0 - scroll_points * 0.002).clamp(0.5, 2.0);
        self.distance = (self.distance * factor).clamp(1.2, 25.0);
    }

    /// Slide the target in the view plane.
    pub fn pan(&mut self, dx_points: f32, dy_points: f32) {
        let view = self.view();
        let right = Vec3::new(view.x_axis.x, view.y_axis.x, view.z_axis.x);
        let up = Vec3::new(view.x_axis.y, view.y_axis.y, view.z_axis.y);
        let scale = self.distance * 0.0015;
        self.target += (-dx_points * right + dy_points * up) * scale;
    }

    /// World-space ray through a normalized device coordinate (y up).
    pub fn ray(&self, ndc_x: f32, ndc_y: f32, aspect: f32) -> (Vec3, Vec3) {
        let inv = self.view_proj(aspect).inverse();
        let near = inv.project_point3(Vec3::new(ndc_x, ndc_y, 0.0));
        let far = inv.project_point3(Vec3::new(ndc_x, ndc_y, 1.0));
        (near, (far - near).normalize_or_zero())
    }
}
