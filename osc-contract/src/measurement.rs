//! Opt-in, versioned measurement messages carried as one OSC string over TCP.
//! A reply confirms control/render progress, never DAC or acoustic timing.
use serde::{Deserialize, Serialize};

pub const REQUEST_ADDRESS: &str = crate::CONTROL_MEASUREMENT;
pub const REPLY_ADDRESS: &str = crate::STATE_MEASUREMENT;
pub const VERSION: u32 = 1;
pub const MAX_PACKET_BYTES: usize = 64 * 1024;
pub const LEASE_MS: u64 = 3_000;
pub const MAX_DURATION_MS: u32 = 5_000;
pub const MAX_DELAY_MS: f32 = 100.0;
/// Silence before/after the sweep flushes output delay lines between bursts.
pub const SETTLE_MS: u32 = 250;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub request_id: u64,
    pub command: Command,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Discover {},
    Begin {
        expected_revision: String,
    },
    Start {
        session_id: String,
        signal_id: u64,
        output_id: String,
        signal: Sweep,
    },
    Status {
        session_id: String,
    },
    Stop {
        session_id: String,
    },
    End {
        session_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub version: u32,
    pub request_id: u64,
    pub reply: Reply,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Snapshot {
        snapshot: Snapshot,
    },
    Session {
        session_id: String,
        snapshot: Snapshot,
        lease_ms: u64,
        signal: Option<Progress>,
    },
    Ended {},
    Error {
        code: String,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    /// Identity of this listener lifetime, not a permanent installation ID.
    pub instance_id: String,
    /// Opaque revision: invalid across restarts or configuration changes.
    pub revision: String,
    pub sample_rate: u32,
    pub master_gain_db: f32,
    pub max_delay_ms: f32,
    pub outputs: Vec<Output>,
    /// Discovery is still usable when calibration cannot start.
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Output {
    /// Layout name, required to be unique; stable across index reordering.
    pub id: String,
    pub index: u32,
    pub name: String,
    pub gain_db: f32,
    pub delay_ms: f32,
    pub muted: bool,
    pub freq_low_hz: Option<f32>,
    pub freq_high_hz: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sweep {
    pub duration_ms: u32,
    /// Peak at the injection point, before the existing master/output gains.
    pub peak_dbfs: f32,
    pub start_hz: f32,
    pub end_hz: f32,
}

impl Sweep {
    pub fn validate(&self, sample_rate: u32) -> Result<(), &'static str> {
        if !(8_000..=192_000).contains(&sample_rate) {
            return Err("measurement requires a sample rate between 8000 and 192000 Hz");
        }
        if !(100..=MAX_DURATION_MS).contains(&self.duration_ms) {
            return Err("duration must be between 100 and 5000 ms");
        }
        if !self.peak_dbfs.is_finite() || !(-60.0..=-12.0).contains(&self.peak_dbfs) {
            return Err("peak must be between -60 and -12 dBFS");
        }
        if !self.start_hz.is_finite()
            || !self.end_hz.is_finite()
            || self.start_hz < 20.0
            || self.start_hz >= self.end_hz
            || self.end_hz > sample_rate as f32 * 0.45
        {
            return Err(
                "sweep frequencies must increase from at least 20 Hz to below 0.45 times the sample rate",
            );
        }
        Ok(())
    }

    pub fn frames(&self, sample_rate: u32) -> u64 {
        u64::from(sample_rate) * u64::from(self.duration_ms) / 1_000
            + 2 * (u64::from(sample_rate) * u64::from(SETTLE_MS) / 1_000)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalState {
    Queued,
    Rendering,
    Completed,
    Cancelled,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Progress {
    pub signal_id: u64,
    pub output_id: String,
    pub signal: Sweep,
    pub state: SignalState,
    /// Frames submitted to the renderer output stage, not DAC timestamps.
    pub rendered_frames: u64,
    pub total_frames: u64,
}
