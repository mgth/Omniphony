//! Object trails (`trails.js`): the diffuse point cloud (default) and the
//! line mode, built from the ring recorded by the OSC thread.

use std::time::{Duration, Instant};

use glam::Vec3;

use crate::model::app_state::RoomRatio;
use crate::osc::dispatch::Trail;
use crate::render::{FrameData, LineVertex, PointInstance};

use super::objects::SpeakerRef;
use super::scene_position;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrailMode {
    Diffuse,
    Line,
}

impl TrailMode {
    pub const ALL: [TrailMode; 2] = [TrailMode::Diffuse, TrailMode::Line];

    pub fn label(self) -> &'static str {
        match self {
            TrailMode::Diffuse => "Diffuse",
            TrailMode::Line => "Line",
        }
    }
}

#[derive(Clone, Debug)]
pub struct TrailSettings {
    pub enabled: bool,
    pub mode: TrailMode,
    /// `trailPointTtlMs`, clamped ≥ 500 ms.
    pub ttl: Duration,
    /// `trailTeleportThreshold` on raw ADM coordinates (0 disables).
    pub teleport_threshold: f32,
}

impl Default for TrailSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: TrailMode::Diffuse,
            ttl: Duration::from_secs(7),
            teleport_threshold: 0.5,
        }
    }
}

const SILENT_RMS_DBFS: f64 = -100.0;
const SILENT_GAIN_DB: i32 = -128;

fn is_teleport(a: [f64; 3], b: [f64; 3], threshold: f32) -> bool {
    if threshold <= 0.0 {
        return false;
    }
    let d = (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2);
    d > (threshold as f64).powi(2)
}

/// Emit one object's trail. `color` is the object's trail colour (linear),
/// `level_scale` its current `levelScale` (diffuse mode loudness).
pub fn emit(
    trail: &Trail,
    settings: &TrailSettings,
    room: &RoomRatio,
    speakers: &[SpeakerRef],
    color: [f32; 3],
    level_scale: f32,
    now: Instant,
    frame: &mut FrameData,
) {
    let alive: Vec<&crate::osc::dispatch::TrailPoint> = trail
        .points
        .iter()
        .filter(|p| now.duration_since(p.t) < settings.ttl)
        .collect();
    let map = |p: &crate::osc::dispatch::TrailPoint| -> Vec3 {
        match p
            .direct_speaker_index
            .and_then(|i| speakers.get(i as usize))
        {
            Some(sp) => sp.scene_pos,
            None => scene_position(p.adm, room),
        }
    };
    match settings.mode {
        TrailMode::Diffuse => {
            let audible: Vec<_> = alive
                .into_iter()
                .filter(|p| {
                    !p.rms_dbfs.is_some_and(|r| r <= SILENT_RMS_DBFS)
                        && !p.gain_db.is_some_and(|g| g <= SILENT_GAIN_DB)
                })
                .collect();
            let count = audible.len();
            if count < 2 {
                return;
            }
            let loudness = level_scale.max(0.0).powf(1.8);
            let denom = (count - 1) as f32;
            let positions: Vec<Vec3> = audible.iter().map(|p| map(p)).collect();
            let mut push = |pos: Vec3, t: f32| {
                let glow = 0.18 + 0.82 * t;
                frame.points.push(PointInstance {
                    pos: pos.to_array(),
                    size: (6.0 + 20.0 * t) * loudness,
                    color: [
                        color[0] * glow,
                        color[1] * glow,
                        color[2] * glow,
                        0.05 + 0.2 * t * t,
                    ],
                });
            };
            for i in 0..count {
                push(positions[i], i as f32 / denom);
                if i + 1 == count
                    || is_teleport(
                        audible[i].adm,
                        audible[i + 1].adm,
                        settings.teleport_threshold,
                    )
                {
                    continue;
                }
                let dist = (positions[i + 1] - positions[i]).length();
                let sub = ((dist / 0.06).ceil() as usize).clamp(2, 10);
                for step in 1..sub {
                    let f = step as f32 / sub as f32;
                    push(
                        positions[i].lerp(positions[i + 1], f),
                        (i as f32 + f) / denom,
                    );
                }
            }
        }
        TrailMode::Line => {
            let count = alive.len();
            if count < 2 {
                return;
            }
            let denom = (count - 1) as f32;
            let positions: Vec<Vec3> = alive.iter().map(|p| map(p)).collect();
            for i in 0..count - 1 {
                if is_teleport(alive[i].adm, alive[i + 1].adm, settings.teleport_threshold) {
                    continue;
                }
                let t1 = i as f32 / denom;
                let t2 = (i + 1) as f32 / denom;
                let k1 = 0.2 + 0.8 * t1;
                let k2 = 0.2 + 0.8 * t2;
                frame.overlay_lines.push(LineVertex {
                    pos: positions[i].to_array(),
                    color: [color[0] * k1, color[1] * k1, color[2] * k1, 0.6],
                });
                frame.overlay_lines.push(LineVertex {
                    pos: positions[i + 1].to_array(),
                    color: [color[0] * k2, color[1] * k2, color[2] * k2, 0.6],
                });
            }
        }
    }
}
