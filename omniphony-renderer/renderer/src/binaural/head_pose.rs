//! Head orientation as a unit quaternion.
//!
//! The pose rotates *world* (ADM) coordinates into *head-relative* coordinates,
//! so a binaural object's direction tracks head movement: `head_relative =
//! pose.rotate(world_position)`. Identity = listener facing ADM front (+Y),
//! up = +Z, right = +X.
//!
//! Quaternion is stored normalised as `(w, x, y, z)`. The math here is
//! hand-rolled (no external dependency) and gimbal-lock-free, consistent with
//! the minimal vector helpers in `spatial_vbap::vbap_native`.

/// Unit quaternion describing the listener's head orientation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeadPose {
    pub w: f32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Default for HeadPose {
    fn default() -> Self {
        Self::identity()
    }
}

impl HeadPose {
    /// No rotation (listener facing ADM front).
    pub const fn identity() -> Self {
        Self {
            w: 1.0,
            x: 0.0,
            y: 0.0,
            z: 0.0,
        }
    }

    /// Build from raw quaternion components, normalising. Falls back to identity
    /// if the input is degenerate (near-zero norm), so the audio path can never
    /// produce NaNs from a bad sensor packet.
    pub fn from_quat(w: f32, x: f32, y: f32, z: f32) -> Self {
        let n = (w * w + x * x + y * y + z * z).sqrt();
        if !n.is_finite() || n < 1e-9 {
            return Self::identity();
        }
        let inv = 1.0 / n;
        Self {
            w: w * inv,
            x: x * inv,
            y: y * inv,
            z: z * inv,
        }
    }

    /// Quaternion components as `[w, x, y, z]`, for compact config storage.
    pub fn to_quat_array(self) -> [f32; 4] {
        [self.w, self.x, self.y, self.z]
    }

    /// Rebuild from a `[w, x, y, z]` array (normalising, see [`Self::from_quat`]).
    pub fn from_quat_array(a: [f32; 4]) -> Self {
        Self::from_quat(a[0], a[1], a[2], a[3])
    }

    /// Build from intrinsic yaw→pitch→roll Euler angles in degrees.
    ///
    /// Axis convention (ADM): yaw about +Z (turn left/right), pitch about +X
    /// (nod up/down), roll about +Y (tilt). The exact mapping is refined when
    /// SensorsOSC axis wiring is finalised (M2); the rotation itself is correct
    /// for any consistent choice.
    pub fn from_euler_deg(yaw_deg: f32, pitch_deg: f32, roll_deg: f32) -> Self {
        let (sy, cy) = (yaw_deg.to_radians() * 0.5).sin_cos();
        let (sp, cp) = (pitch_deg.to_radians() * 0.5).sin_cos();
        let (sr, cr) = (roll_deg.to_radians() * 0.5).sin_cos();
        // q = qz(yaw) * qx(pitch) * qy(roll)
        let qz = HeadPose {
            w: cy,
            x: 0.0,
            y: 0.0,
            z: sy,
        };
        let qx = HeadPose {
            w: cp,
            x: sp,
            y: 0.0,
            z: 0.0,
        };
        let qy = HeadPose {
            w: cr,
            x: 0.0,
            y: sr,
            z: 0.0,
        };
        qz.mul(qx).mul(qy).normalized()
    }

    /// Hamilton product `self * rhs`.
    pub fn mul(self, r: HeadPose) -> HeadPose {
        HeadPose {
            w: self.w * r.w - self.x * r.x - self.y * r.y - self.z * r.z,
            x: self.w * r.x + self.x * r.w + self.y * r.z - self.z * r.y,
            y: self.w * r.y - self.x * r.z + self.y * r.w + self.z * r.x,
            z: self.w * r.z + self.x * r.y - self.y * r.x + self.z * r.w,
        }
    }

    /// Unit-normalise, returning identity on a degenerate quaternion.
    pub fn normalized(self) -> HeadPose {
        Self::from_quat(self.w, self.x, self.y, self.z)
    }

    /// Conjugate (inverse rotation for a unit quaternion).
    pub fn conjugate(self) -> HeadPose {
        HeadPose {
            w: self.w,
            x: -self.x,
            y: -self.y,
            z: -self.z,
        }
    }

    /// Rotate a 3-D vector by this quaternion: `v' = q * v * q⁻¹`, using the
    /// branch-free `v + 2w(u×v) + 2(u×(u×v))` form (`u` = quaternion vector part).
    pub fn rotate(self, v: [f64; 3]) -> [f64; 3] {
        let ux = self.x as f64;
        let uy = self.y as f64;
        let uz = self.z as f64;
        let w = self.w as f64;
        // t = 2 * (u × v)
        let tx = 2.0 * (uy * v[2] - uz * v[1]);
        let ty = 2.0 * (uz * v[0] - ux * v[2]);
        let tz = 2.0 * (ux * v[1] - uy * v[0]);
        // v' = v + w*t + (u × t)
        [
            v[0] + w * tx + (uy * tz - uz * ty),
            v[1] + w * ty + (uz * tx - ux * tz),
            v[2] + w * tz + (ux * ty - uy * tx),
        ]
    }

    /// Normalised linear interpolation toward `target` by `t` ∈ [0, 1].
    ///
    /// Cheaper than slerp and indistinguishable for the small per-update steps of
    /// head-tracking smoothing. The shorter arc is taken (sign-corrected), and the
    /// result is renormalised. `t = 1` returns `target`; `t = 0` returns `self`.
    pub fn nlerp(self, target: HeadPose, t: f32) -> HeadPose {
        // Take the shorter arc: if the quaternions are more than 90° apart,
        // negate one (q and −q are the same rotation).
        let dot = self.w * target.w + self.x * target.x + self.y * target.y + self.z * target.z;
        let s = if dot < 0.0 { -1.0 } else { 1.0 };
        let it = 1.0 - t;
        HeadPose::from_quat(
            it * self.w + t * s * target.w,
            it * self.x + t * s * target.x,
            it * self.y + t * s * target.y,
            it * self.z + t * s * target.z,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: [f64; 3], b: [f64; 3]) {
        for i in 0..3 {
            assert!((a[i] - b[i]).abs() < 1e-6, "{a:?} != {b:?}");
        }
    }

    #[test]
    fn identity_is_no_rotation() {
        approx(
            HeadPose::identity().rotate([0.0, 1.0, 0.0]),
            [0.0, 1.0, 0.0],
        );
    }

    #[test]
    fn yaw_90_turns_front_to_left() {
        // +90° yaw about +Z maps world-front (+Y) to head-left (−X, ADM left).
        let p = HeadPose::from_euler_deg(90.0, 0.0, 0.0);
        approx(p.rotate([0.0, 1.0, 0.0]), [-1.0, 0.0, 0.0]);
    }

    #[test]
    fn conjugate_undoes_rotation() {
        let p = HeadPose::from_euler_deg(37.0, -12.0, 5.0);
        let v = [0.3, -0.7, 0.5];
        approx(p.conjugate().rotate(p.rotate(v)), v);
    }

    #[test]
    fn degenerate_quat_falls_back_to_identity() {
        let p = HeadPose::from_quat(0.0, 0.0, 0.0, 0.0);
        assert_eq!(p, HeadPose::identity());
    }
}
