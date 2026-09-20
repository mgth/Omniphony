//! Live head-tracking input (e.g. SensorsOSC on Android).
//!
//! The orientation arrives on an arbitrary OSC address as a few floats. This
//! module owns the *pure* parts — value-format decoding, recenter reference and
//! exponential smoothing — so they are unit-testable without any OSC plumbing.
//! The transport glue (address match, locking) lives in the engine's OSC loop.

use super::HeadPose;

/// How to interpret the float payload of a head-tracking OSC message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HeadTrackingFormat {
    /// Best-effort: 4 floats → `[x, y, z, w]` quaternion (Android order);
    /// 3 floats → rotation vector (`[x, y, z]`, `w` derived).
    #[default]
    Auto,
    /// Explicit quaternion `[x, y, z, w]`.
    Quat,
    /// Rotation vector `[x, y, z]`; `w = √(1 − x² − y² − z²)`.
    RotVec,
    /// Euler degrees `[yaw, pitch, roll]`.
    Euler,
}

impl HeadTrackingFormat {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "quat" | "quaternion" => Some(Self::Quat),
            "rotvec" | "rotation_vector" | "rotationvector" => Some(Self::RotVec),
            "euler" | "orientation" | "ypr" => Some(Self::Euler),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Quat => "quat",
            Self::RotVec => "rotvec",
            Self::Euler => "euler",
        }
    }

    /// Decode a raw orientation from the message's float arguments.
    pub fn parse(self, args: &[f32]) -> Option<HeadPose> {
        match self {
            Self::Quat => {
                let [x, y, z, w] = first4(args)?;
                Some(HeadPose::from_quat(w, x, y, z))
            }
            Self::RotVec => {
                let [x, y, z] = first3(args)?;
                Some(rotvec_to_pose(x, y, z))
            }
            Self::Euler => {
                let [yaw, pitch, roll] = first3(args)?;
                Some(HeadPose::from_euler_deg(yaw, pitch, roll))
            }
            Self::Auto => {
                if args.len() >= 4 {
                    let [x, y, z, w] = first4(args)?;
                    Some(HeadPose::from_quat(w, x, y, z))
                } else if args.len() >= 3 {
                    let [x, y, z] = first3(args)?;
                    Some(rotvec_to_pose(x, y, z))
                } else {
                    None
                }
            }
        }
    }
}

fn first3(a: &[f32]) -> Option<[f32; 3]> {
    match a {
        [x, y, z, ..] => Some([*x, *y, *z]),
        _ => None,
    }
}

fn first4(a: &[f32]) -> Option<[f32; 4]> {
    match a {
        [x, y, z, w, ..] => Some([*x, *y, *z, *w]),
        _ => None,
    }
}

/// Android rotation-vector → quaternion: the three components are the vector part
/// `(x, y, z)`; the scalar `w` is recovered (clamped) from unit-norm.
fn rotvec_to_pose(x: f32, y: f32, z: f32) -> HeadPose {
    let w = (1.0 - (x * x + y * y + z * z)).max(0.0).sqrt();
    HeadPose::from_quat(w, x, y, z)
}

/// Head-tracking configuration *and* live recenter/smoothing state. Written by
/// the OSC listener thread; the smoothed result is mirrored into
/// `BinauralLiveParams::head_pose`, which the render thread reads.
#[derive(Debug, Clone)]
pub struct HeadTracking {
    /// OSC address carrying the orientation. `None`/empty → tracking disabled.
    pub address: Option<String>,
    /// Value format of the payload.
    pub format: HeadTrackingFormat,
    /// Exponential smoothing in [0, 1): 0 = instant, higher = smoother/laggier.
    pub smoothing: f32,
    /// Flip the applied rotation, for sensors whose motion comes out mirrored.
    pub invert: bool,
    /// Orientation captured at the last recenter — defines "looking forward".
    pub reference: HeadPose,
    /// Last raw orientation received (captured as the reference on recenter).
    pub last_raw: HeadPose,
    /// Arrival time of the last packet, for the time-based smoothing step.
    /// `None` until the first packet.
    pub last_packet: Option<std::time::Instant>,
    /// Sensor-to-head axis calibration: the rotation taking a vector's
    /// coordinates in the sensor's frame to the head's (right, front, up).
    /// Identity until calibrated — the sensor's axes are then assumed to be
    /// the head's, which is only true for one way of strapping it on.
    pub axes: HeadPose,
    /// Poses captured so far by an in-progress calibration.
    pub calibration: Calibration,
}

/// The three poses of an axis calibration, captured in order: looking
/// ahead (which also recenters), then with the head turned to the **left**,
/// then looking **up**. The turn gives the head's up axis in sensor
/// coordinates, the nod its right axis; front is their cross product. Three
/// poses because a turn alone cannot tell which horizontal direction is
/// ahead, and no assumption about how the sensor is mounted is needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalibrationStep {
    Front,
    Left,
    Up,
    /// Forget the calibration: sensor axes = head axes again.
    Reset,
}

impl CalibrationStep {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "front" | "ahead" | "forward" => Some(Self::Front),
            "left" => Some(Self::Left),
            "up" => Some(Self::Up),
            "reset" | "clear" => Some(Self::Reset),
            _ => None,
        }
    }
}

/// In-progress calibration poses.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Calibration {
    /// Raw orientation captured looking ahead.
    pub front: Option<HeadPose>,
    /// Raw orientation captured with the head turned left.
    pub left: Option<HeadPose>,
}

impl Calibration {
    /// 0 = nothing captured (idle), 1 = front captured (turn left next),
    /// 2 = left captured (look up next).
    pub fn step(&self) -> u8 {
        match (self.front, self.left) {
            (None, _) => 0,
            (Some(_), None) => 1,
            (Some(_), Some(_)) => 2,
        }
    }
}

impl Default for HeadTracking {
    fn default() -> Self {
        Self {
            address: None,
            format: HeadTrackingFormat::Auto,
            smoothing: 0.2,
            invert: false,
            reference: HeadPose::identity(),
            last_raw: HeadPose::identity(),
            last_packet: None,
            axes: HeadPose::identity(),
            calibration: Calibration::default(),
        }
    }
}

/// Packet rate the `smoothing` setting is defined at: the value keeps the
/// meaning it always had for a 30 Hz source (Sensors2OSC), and other rates
/// get the same time constant instead of the same per-packet step.
pub const SMOOTHING_REFERENCE_HZ: f32 = 30.0;

/// Longest interval a single step may account for. A source that paused
/// (app in the background, Bluetooth drop) then resumes should converge on
/// its first packets, not teleport to the target in one.
const MAX_STEP_S: f32 = 0.25;

impl HeadTracking {
    /// Whether `addr` is the configured tracking address (non-empty match).
    pub fn matches(&self, addr: &str) -> bool {
        self.address
            .as_deref()
            .is_some_and(|a| !a.is_empty() && a == addr)
    }

    /// Convert a raw device orientation, relative to the recenter reference, into
    /// the world→head rotation the renderer applies to source positions. At the
    /// instant of recenter (`raw == reference`) this is the identity.
    fn world_to_head(&self, raw: HeadPose) -> HeadPose {
        // The relative rotation since the recenter, in the sensor's frame…
        let rel = if self.invert {
            self.reference.conjugate().mul(raw)
        } else {
            raw.conjugate().mul(self.reference)
        };
        // …expressed in the head's frame through the axis calibration
        // (a similarity: identity axes leave it untouched).
        self.axes.mul(rel).mul(self.axes.conjugate()).normalized()
    }

    /// Advance the axis calibration with the current raw orientation.
    /// Returns `Ok(true)` when the calibration just completed (the axes were
    /// updated), `Ok(false)` when a pose was captured or the calibration
    /// reset, `Err` with the reason when the step is out of order or the
    /// captured poses do not describe a turn / a nod.
    pub fn calibrate(&mut self, step: CalibrationStep) -> Result<bool, String> {
        match step {
            CalibrationStep::Reset => {
                self.axes = HeadPose::identity();
                self.calibration = Calibration::default();
                Ok(false)
            }
            CalibrationStep::Front => {
                // Looking ahead is also the recenter.
                self.reference = self.last_raw;
                self.calibration = Calibration {
                    front: Some(self.last_raw),
                    left: None,
                };
                Ok(false)
            }
            CalibrationStep::Left => {
                let Some(front) = self.calibration.front else {
                    return Err("look ahead first (step 1) before turning left".into());
                };
                let turn = front.conjugate().mul(self.last_raw);
                if turn.axis().is_none() {
                    return Err("the head did not turn: no rotation between the two poses".into());
                }
                self.calibration.left = Some(self.last_raw);
                Ok(false)
            }
            CalibrationStep::Up => {
                let (Some(front), Some(left)) = (self.calibration.front, self.calibration.left)
                else {
                    return Err("look ahead, then turn left, before looking up".into());
                };
                // Rotation axes of the two moves, in sensor coordinates: the
                // turn's is the head's up, the nod's is the head's right.
                let up = front
                    .conjugate()
                    .mul(left)
                    .axis()
                    .ok_or_else(|| "the turn had no rotation".to_string())?;
                let nod = front.conjugate().mul(self.last_raw).axis().ok_or_else(|| {
                    "the head did not nod: no rotation between the poses".to_string()
                })?;
                // Orthonormalise: right ⟂ up, front = up × right.
                let dot = nod[0] * up[0] + nod[1] * up[1] + nod[2] * up[2];
                let mut right = [
                    nod[0] - dot * up[0],
                    nod[1] - dot * up[1],
                    nod[2] - dot * up[2],
                ];
                let n = (right[0] * right[0] + right[1] * right[1] + right[2] * right[2]).sqrt();
                if n < 0.2 {
                    return Err("the nod was along the turn's axis: look up, not sideways".into());
                }
                for v in right.iter_mut() {
                    *v /= n;
                }
                let front_axis = [
                    up[1] * right[2] - up[2] * right[1],
                    up[2] * right[0] - up[0] * right[2],
                    up[0] * right[1] - up[1] * right[0],
                ];
                self.axes = HeadPose::from_rows(right, front_axis, up);
                self.calibration = Calibration::default();
                Ok(true)
            }
        }
    }

    /// Ingest a raw orientation received at `now` and return the new smoothed
    /// pose to render with. `current` is the pose currently in use.
    ///
    /// The smoothing is exponential in **time**, not per packet: the step is
    /// `1 − exp(−Δt/τ)` with `τ` derived from `smoothing` at
    /// [`SMOOTHING_REFERENCE_HZ`], so a 100 Hz tracker and a 30 Hz one settle
    /// in the same milliseconds for the same setting. (Per packet, the same
    /// setting used to make the fast tracker three times as sluggish.)
    pub fn ingest(
        &mut self,
        raw: HeadPose,
        current: HeadPose,
        now: std::time::Instant,
    ) -> HeadPose {
        self.last_raw = raw;
        let target = self.world_to_head(raw);
        let dt = match self.last_packet.replace(now) {
            Some(prev) => now
                .saturating_duration_since(prev)
                .as_secs_f32()
                .min(MAX_STEP_S),
            // First packet: behave as one reference interval.
            None => 1.0 / SMOOTHING_REFERENCE_HZ,
        };
        current.nlerp(target, Self::step(self.smoothing, dt))
    }

    /// Fraction of the way to the target to move after `dt` seconds:
    /// `1 − smoothing` at the reference interval, exactly as before, and the
    /// same time constant elsewhere. `smoothing` 0 is instant.
    fn step(smoothing: f32, dt: f32) -> f32 {
        let s = smoothing.clamp(0.0, 0.999);
        if s <= 0.0 {
            return 1.0;
        }
        // τ = −1 / (f_ref · ln s), so that 1 − exp(−(1/f_ref)/τ) = 1 − s.
        let tau = -1.0 / (SMOOTHING_REFERENCE_HZ * s.ln());
        (1.0 - (-dt / tau).exp()).clamp(0.0, 1.0)
    }

    /// Capture the current raw orientation as the new "forward" reference.
    pub fn recenter(&mut self) {
        self.reference = self.last_raw;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(p: HeadPose, q: HeadPose) {
        let d = (p.w - q.w).abs() + (p.x - q.x).abs() + (p.y - q.y).abs() + (p.z - q.z).abs();
        // q and −q are the same rotation; accept either sign.
        let d2 = (p.w + q.w).abs() + (p.x + q.x).abs() + (p.y + q.y).abs() + (p.z + q.z).abs();
        assert!(d.min(d2) < 1e-5, "{p:?} != {q:?}");
    }

    #[test]
    fn parse_euler_and_quat() {
        let e = HeadTrackingFormat::Euler.parse(&[90.0, 0.0, 0.0]).unwrap();
        approx(e, HeadPose::from_euler_deg(90.0, 0.0, 0.0));
        let q = HeadTrackingFormat::Quat
            .parse(&[0.0, 0.0, 0.7071, 0.7071])
            .unwrap();
        approx(q, HeadPose::from_quat(0.7071, 0.0, 0.0, 0.7071));
    }

    #[test]
    fn auto_picks_quat_for_four_floats_rotvec_for_three() {
        assert!(
            HeadTrackingFormat::Auto
                .parse(&[0.0, 0.0, 0.0, 1.0])
                .is_some()
        );
        assert!(HeadTrackingFormat::Auto.parse(&[0.0, 0.0, 0.0]).is_some());
        assert!(HeadTrackingFormat::Auto.parse(&[0.0]).is_none());
    }

    #[test]
    fn rotvec_recovers_unit_quaternion() {
        let p = rotvec_to_pose(0.0, 0.0, 0.0);
        approx(p, HeadPose::identity());
    }

    #[test]
    fn recenter_makes_current_orientation_forward() {
        let mut ht = HeadTracking {
            smoothing: 0.0,
            ..Default::default()
        };
        // Look 40° right, recenter: the rendered pose must become identity.
        let raw = HeadPose::from_euler_deg(40.0, 0.0, 0.0);
        ht.ingest(raw, HeadPose::identity(), std::time::Instant::now());
        ht.recenter();
        let pose = ht.ingest(raw, HeadPose::identity(), std::time::Instant::now());
        approx(pose, HeadPose::identity());
    }

    #[test]
    fn smoothing_moves_only_partway() {
        let mut ht = HeadTracking {
            smoothing: 0.5,
            ..Default::default()
        };
        let raw = HeadPose::from_euler_deg(90.0, 0.0, 0.0);
        let p1 = ht.ingest(raw, HeadPose::identity(), std::time::Instant::now());
        // Halfway: not yet at the target, not still at identity.
        let target = ht.world_to_head(raw);
        approx_between(p1, HeadPose::identity(), target);
    }

    /// A sensor strapped on in some arbitrary orientation: after the three
    /// calibration poses, a real head yaw renders as that yaw — with no
    /// roll and no pitch leaking in — where the uncalibrated path renders
    /// a rotation about the wrong axis.
    #[test]
    fn three_pose_calibration_recovers_the_head_axes() {
        use std::time::Instant;
        // Mounting: head → sensor axes. v_sensor = mount · v_head.
        let mount =
            HeadPose::from_euler_deg(90.0, 0.0, 0.0).mul(HeadPose::from_euler_deg(0.0, 35.0, 20.0));
        // The sensor reports its own orientation: raw = head · mount⁻¹.
        let raw_for = |head: HeadPose| head.mul(mount.conjugate()).normalized();
        let mut ht = HeadTracking {
            smoothing: 0.0,
            ..Default::default()
        };
        let feed = |ht: &mut HeadTracking, head: HeadPose| {
            ht.ingest(raw_for(head), HeadPose::identity(), Instant::now())
        };
        feed(&mut ht, HeadPose::identity());
        assert_eq!(ht.calibrate(CalibrationStep::Front), Ok(false));
        feed(&mut ht, HeadPose::from_euler_deg(60.0, 0.0, 0.0)); // turn left
        assert_eq!(ht.calibrate(CalibrationStep::Left), Ok(false));
        assert_eq!(ht.calibration.step(), 2);
        feed(&mut ht, HeadPose::from_euler_deg(0.0, 40.0, 0.0)); // look up
        assert_eq!(ht.calibrate(CalibrationStep::Up), Ok(true));
        assert_eq!(ht.calibration.step(), 0);
        // Now a 35° yaw of the head must render as the inverse 35° yaw.
        let head = HeadPose::from_euler_deg(35.0, 0.0, 0.0);
        let pose = feed(&mut ht, head);
        approx(pose, head.conjugate());
        // A source dead ahead lands 35° to the right, level.
        let p = pose.rotate([0.0, 1.0, 0.0]);
        assert!(p[0] > 0.55 && p[0] < 0.6 && p[2].abs() < 1e-3, "{p:?}");
        // Without the calibration the same yaw is a rotation about the
        // mount's axis: not a pure yaw of the rendered pose.
        let mut plain = HeadTracking {
            smoothing: 0.0,
            ..Default::default()
        };
        feed(&mut plain, HeadPose::identity());
        plain.recenter();
        let wrong = feed(&mut plain, head).rotate([0.0, 1.0, 0.0]);
        assert!(
            wrong[2].abs() > 0.1,
            "uncalibrated pose should leak out of the horizon: {wrong:?}"
        );
        // Reset forgets it.
        assert_eq!(ht.calibrate(CalibrationStep::Reset), Ok(false));
        assert_eq!(ht.axes, HeadPose::identity());
    }

    /// Steps out of order, or poses without movement, are refused with a
    /// reason and leave the axes alone.
    #[test]
    fn calibration_refuses_bad_steps() {
        let mut ht = HeadTracking::default();
        assert!(ht.calibrate(CalibrationStep::Left).is_err());
        assert!(ht.calibrate(CalibrationStep::Up).is_err());
        assert_eq!(ht.calibrate(CalibrationStep::Front), Ok(false));
        // No movement between front and left: refused.
        assert!(ht.calibrate(CalibrationStep::Left).is_err());
        assert_eq!(ht.axes, HeadPose::identity());
        assert_eq!(CalibrationStep::from_str("Up"), Some(CalibrationStep::Up));
        assert_eq!(CalibrationStep::from_str("sideways"), None);
    }

    /// Same setting, same milliseconds, same pose — whatever the packet rate.
    #[test]
    fn smoothing_is_a_time_constant_not_a_packet_count() {
        use std::time::{Duration, Instant};
        let raw = HeadPose::from_euler_deg(60.0, 0.0, 0.0);
        let run = |hz: u32| -> HeadPose {
            let mut ht = HeadTracking {
                smoothing: 0.8,
                ..Default::default()
            };
            let t0 = Instant::now();
            let mut pose = HeadPose::identity();
            let n = hz / 5; // 200 ms of packets
            for k in 0..=n {
                let at = t0 + Duration::from_secs_f32(k as f32 / hz as f32);
                pose = ht.ingest(raw, pose, at);
            }
            pose
        };
        let (slow, fast) = (run(30), run(100));
        let d = (slow.w - fast.w).abs()
            + (slow.x - fast.x).abs()
            + (slow.y - fast.y).abs()
            + (slow.z - fast.z).abs();
        assert!(d < 0.02, "30 Hz and 100 Hz diverged after 200 ms: {d}");
        // And at the reference rate the per-packet step is what it always was.
        assert!((HeadTracking::step(0.8, 1.0 / 30.0) - 0.2).abs() < 1e-6);
        assert!((HeadTracking::step(0.0, 0.01) - 1.0).abs() < 1e-6);
    }

    /// A source that resumes after a long pause converges instead of
    /// teleporting: the step is capped at a quarter second's worth.
    #[test]
    fn a_long_pause_does_not_teleport() {
        use std::time::{Duration, Instant};
        let mut ht = HeadTracking {
            smoothing: 0.9,
            ..Default::default()
        };
        let raw = HeadPose::from_euler_deg(90.0, 0.0, 0.0);
        let t0 = Instant::now();
        let p1 = ht.ingest(raw, HeadPose::identity(), t0);
        let p2 = ht.ingest(raw, p1, t0 + Duration::from_secs(5));
        let target = ht.world_to_head(raw);
        approx_between(p2, HeadPose::identity(), target);
        let expected = HeadTracking::step(0.9, MAX_STEP_S);
        assert!(
            expected < 1.0,
            "the capped step must still be partial: {expected}"
        );
    }

    fn approx_between(p: HeadPose, a: HeadPose, b: HeadPose) {
        let da = (p.w - a.w).abs() + (p.x - a.x).abs() + (p.y - a.y).abs() + (p.z - a.z).abs();
        let db = (p.w - b.w).abs() + (p.x - b.x).abs() + (p.y - b.y).abs() + (p.z - b.z).abs();
        assert!(
            da > 1e-3 && db > 1e-3,
            "expected strictly between endpoints"
        );
    }
}
