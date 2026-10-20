//! Opt-in, versioned measurement messages carried as one OSC string over TCP.
//! A reply confirms control/render progress, never DAC or acoustic timing.
use serde::{Deserialize, Serialize};

pub const REQUEST_ADDRESS: &str = crate::CONTROL_MEASUREMENT;
pub const REPLY_ADDRESS: &str = crate::STATE_MEASUREMENT;
/// The version this schema writes. Version 5 adds output positions, version 6
/// level noise that skips the output's gain, version 7 [`Command::UseSpeakerOutput`].
pub const VERSION: u32 = 7;
/// The oldest request version a renderer still answers, in that version's
/// shape ([`Response::for_version`]); older clients reject unknown fields.
pub const MIN_VERSION: u32 = 4;
/// First version whose snapshots carry [`Output`] positions.
pub const POSITIONS_VERSION: u32 = 5;
/// First version whose renderers accept [`LevelNoise::bypass_output_gain`].
pub const NOISE_GAINS_VERSION: u32 = 6;
/// First version whose renderers accept [`Command::UseSpeakerOutput`].
pub const SPEAKER_OUTPUT_VERSION: u32 = 7;
/// The snapshot's `unavailable_reason` while the renderer plays to headphones
/// (binaural output): the one reason [`Command::UseSpeakerOutput`] resolves.
pub const SPEAKER_OUTPUT_REQUIRED: &str = "measurement requires speaker output mode";

/// Whether a renderer built from this schema answers `version` requests.
pub fn supported(version: u32) -> bool {
    (MIN_VERSION..=VERSION).contains(&version)
}

/// The version a reply to a `requested` request carries: the request's own
/// when supported, so an older client can match it, otherwise [`VERSION`].
pub fn reply_version(requested: u32) -> u32 {
    if supported(requested) {
        requested
    } else {
        VERSION
    }
}
pub const MAX_SEQUENCE_OUTPUTS: usize = 32;
pub const MAX_SEQUENCE_MS: u64 = 120_000;
pub const MAX_PACKET_BYTES: usize = 64 * 1024;
pub const LEASE_MS: u64 = 3_000;
pub const MAX_DURATION_MS: u32 = 5_000;
/// Physical output delay capacity; bounded by the unchanged sweep settling time.
pub const MAX_DELAY_MS: f32 = 250.0;
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
    /// Compare and replace absolute output settings as one live-state publication.
    /// This never writes the persistent configuration.
    SetOutputs {
        expected_revision: String,
        settings: Vec<OutputSetting>,
    },
    Begin {
        expected_revision: String,
    },
    Start {
        session_id: String,
        signal_id: u64,
        output_id: String,
        /// When set, play reference, target, reference at fixed sample offsets.
        #[serde(default)]
        reference_output_id: Option<String>,
        /// Ordered targets; nonempty means reference, target, reference, ... .
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        sequence_outputs: Vec<String>,
        signal: Sweep,
    },
    /// Version 7: switch the live output to the speakers, when the renderer plays
    /// to headphones ([`SPEAKER_OUTPUT_REQUIRED`]). Outside a session only; replies
    /// with the new snapshot. Live only, like `set_outputs`: never the saved config.
    UseSpeakerOutput {},
    /// Optional v4 extension: finite, band-limited pink noise for physical level matching.
    StartNoise {
        session_id: String,
        signal_id: u64,
        output_id: String,
        signal: LevelNoise,
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

impl Response {
    /// A reply in the shape `version` clients parse: before
    /// [`POSITIONS_VERSION`] every snapshot drops its output positions.
    pub fn for_version(version: u32, request_id: u64, mut reply: Reply) -> Self {
        if version < POSITIONS_VERSION {
            match &mut reply {
                Reply::Snapshot { snapshot }
                | Reply::Session { snapshot, .. }
                | Reply::NoiseSession { snapshot, .. } => {
                    for output in &mut snapshot.outputs {
                        output.azimuth_deg = None;
                        output.elevation_deg = None;
                        output.distance_m = None;
                    }
                }
                Reply::Ended {} | Reply::Error { .. } => {}
            }
        }
        Self {
            version,
            request_id,
            reply,
        }
    }
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
    NoiseSession {
        session_id: String,
        snapshot: Snapshot,
        lease_ms: u64,
        signal: NoiseProgress,
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
pub struct OutputSetting {
    pub output_id: String,
    pub gain_db: f32,
    pub delay_ms: f32,
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
    /// Version 5: where the active layout places the output, seen from the
    /// listening position (0° front, negative to the left, positive up).
    /// Absent when unknown and in replies to older versions. Declared
    /// geometry for display, not a measured or acoustic position.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub azimuth_deg: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elevation_deg: Option<f32>,
    /// Distance in metres at the layout's room scale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance_m: Option<f32>,
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

    /// Prepare a mono segment off the audio thread. Both playback and analysis
    /// use this definition, including the 5 ms raised-cosine fades.
    pub fn samples(&self, sample_rate: u32) -> Result<Vec<f32>, &'static str> {
        self.validate(sample_rate)?;
        let frames = self.frames(sample_rate) as usize;
        let padding = sample_rate as usize * SETTLE_MS as usize / 1_000;
        let sweep_frames = frames - 2 * padding;
        let duration = sweep_frames as f64 / sample_rate as f64;
        let log_ratio = (self.end_hz as f64 / self.start_hz as f64).ln();
        let phase_scale = std::f64::consts::TAU * self.start_hz as f64 * duration / log_ratio;
        let peak = 10_f64.powf(self.peak_dbfs as f64 / 20.0);
        let fade = (sample_rate as f64 * 0.005).round() as usize;
        Ok((0..frames)
            .map(|i| {
                if i < padding || i >= padding + sweep_frames {
                    return 0.0;
                }
                let i = i - padding;
                let t = i as f64 / sample_rate as f64;
                let edge = i.min(sweep_frames - 1 - i).min(fade) as f64 / fade as f64;
                let envelope = 0.5 - 0.5 * (std::f64::consts::PI * edge).cos();
                (peak * envelope * (phase_scale * (log_ratio * t / duration).exp_m1()).sin()) as f32
            })
            .collect())
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
    pub reference_output_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sequence_outputs: Vec<String>,
    pub signal: Sweep,
    pub state: SignalState,
    /// Frames submitted to the renderer output stage, not DAC timestamps.
    pub rendered_frames: u64,
    pub total_frames: u64,
}

/// Validate a finite shared-reference sequence before allocating or playing it.
pub fn sequence_frames(signal: Sweep, rate: u32, targets: usize) -> Result<u64, &'static str> {
    signal.validate(rate)?;
    if !(1..=MAX_SEQUENCE_OUTPUTS).contains(&targets) {
        return Err("sequence needs 1..32 targets");
    }
    let frames = signal
        .frames(rate)
        .checked_mul(2 * targets as u64 + 1)
        .ok_or("sequence is too long")?;
    if frames > u64::from(rate) * MAX_SEQUENCE_MS / 1000 {
        return Err("sequence exceeds 120 seconds; select fewer speakers or a shorter sweep");
    }
    Ok(frames)
}

/// A fixed common midrange band intentionally excludes subwoofer matching.
/// Peak is before the existing renderer master gain, and before the output's
/// gain unless [`Self::bypass_output_gain`] is set.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LevelNoise {
    pub duration_seconds: u32,
    pub peak_dbfs: f32,
    /// Version 6: play the noise without the output's own gain (its level
    /// correction); the master gain still applies. Omitted when false, so a
    /// request without it keeps the version-4 wire form older renderers accept.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bypass_output_gain: bool,
}
impl LevelNoise {
    pub const LOW_HZ: f32 = 500.0;
    pub const HIGH_HZ: f32 = 2000.0;
    pub fn validate(self, rate: u32) -> Result<(), &'static str> {
        if !(8000..=192000).contains(&rate)
            || !(5..=60).contains(&self.duration_seconds)
            || !self.peak_dbfs.is_finite()
            || !(-60.0..=-12.0).contains(&self.peak_dbfs)
        {
            return Err("noise requires 5..60 seconds, -60..-12 dBFS, and a supported sample rate");
        }
        Ok(())
    }
    pub fn frames(self, rate: u32) -> u64 {
        u64::from(rate) * u64::from(self.duration_seconds)
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoiseProgress {
    pub signal_id: u64,
    pub output_id: String,
    pub signal: LevelNoise,
    pub state: SignalState,
    pub rendered_frames: u64,
    pub total_frames: u64,
}
