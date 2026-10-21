//! Opt-in, versioned measurement messages carried as one OSC string over TCP.
//! A reply confirms control/render progress, never DAC or acoustic timing.
use serde::{Deserialize, Serialize};

pub const REQUEST_ADDRESS: &str = crate::CONTROL_MEASUREMENT;
pub const REPLY_ADDRESS: &str = crate::STATE_MEASUREMENT;
/// The version this schema writes. Version 5 adds output positions, version 6
/// level noise that skips the output's gain, version 7 [`Command::UseSpeakerOutput`],
/// version 8 room correction filters ([`FILTERS_VERSION`]).
pub const VERSION: u32 = 8;
/// The oldest request version a renderer still answers, in that version's
/// shape ([`Response::for_version`]); older clients reject unknown fields.
pub const MIN_VERSION: u32 = 4;
/// First version whose snapshots carry [`Output`] positions.
pub const POSITIONS_VERSION: u32 = 5;
/// First version whose renderers accept [`LevelNoise::bypass_output_gain`].
pub const NOISE_GAINS_VERSION: u32 = 6;
/// First version whose renderers accept [`Command::UseSpeakerOutput`].
pub const SPEAKER_OUTPUT_VERSION: u32 = 7;
/// First version whose renderers accept [`Command::UploadFilter`],
/// [`Command::SetOutputFilters`], `bypass_output_filters` on
/// [`Command::Begin`] and `bass_managed` on [`Command::Start`], and whose
/// snapshots report [`Output::filter`] and [`Snapshot::output_filters_bypassed`].
pub const FILTERS_VERSION: u32 = 8;
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
        /// Version 8: play every output unfiltered while the session holds
        /// (the room correction filters switched to identity, at unchanged
        /// latency). Restored when the session ends, however it ends.
        /// Omitted when false, the version-7 wire form.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        bypass_output_filters: bool,
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
        /// Version 8: play each target's sweep as a source at that output's
        /// layout position, through the crossover and bass management, so a
        /// subwoofer plays its low band as in programme playback. The
        /// reference sweeps stay direct. Omitted when false.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        bass_managed: bool,
    },
    /// Version 8: one chunk of a room correction filter, an impulse response
    /// at `sample_rate`. The first chunk has no `upload_id` and `offset` 0;
    /// the reply's [`FilterUpload::upload_id`] names the upload in the next
    /// chunks, which continue at [`FilterUpload::received_taps`]. Accepted in
    /// and out of a session: it only stores the taps, which
    /// [`Command::SetOutputFilters`] applies.
    UploadFilter {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        upload_id: Option<String>,
        sample_rate: u32,
        total_taps: u32,
        offset: u32,
        /// [`encode_taps`]: base64 of little-endian `f32`, at most
        /// [`MAX_UPLOAD_CHUNK_TAPS`] taps.
        taps: String,
    },
    /// Version 8: compare and replace the filters of some outputs as one
    /// live-state publication, like `set_outputs`: outside a session only,
    /// never written to the saved configuration (the Studio's Save writes
    /// the filters next to it).
    SetOutputFilters {
        expected_revision: String,
        outputs: Vec<OutputFilterSetting>,
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
    /// [`POSITIONS_VERSION`] every snapshot drops its output positions, before
    /// [`FILTERS_VERSION`] its filters and bypass state.
    pub fn for_version(version: u32, request_id: u64, mut reply: Reply) -> Self {
        let snapshot = match &mut reply {
            Reply::Snapshot { snapshot }
            | Reply::Session { snapshot, .. }
            | Reply::NoiseSession { snapshot, .. } => Some(snapshot),
            Reply::FilterUpload { .. } | Reply::Ended {} | Reply::Error { .. } => None,
        };
        if let Some(snapshot) = snapshot {
            if version < FILTERS_VERSION {
                snapshot.output_filters_bypassed = false;
                for output in &mut snapshot.outputs {
                    output.filter = None;
                }
            }
            if version < POSITIONS_VERSION {
                for output in &mut snapshot.outputs {
                    output.azimuth_deg = None;
                    output.elevation_deg = None;
                    output.distance_m = None;
                }
            }
        }
        if version < FILTERS_VERSION
            && let Reply::Session {
                signal: Some(progress),
                ..
            } = &mut reply
        {
            progress.bass_managed = false;
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
    /// Version 8: where an [`Command::UploadFilter`] stands.
    FilterUpload {
        upload: FilterUpload,
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
    /// Version 8: a session holds the outputs unfiltered
    /// (`bypass_output_filters`). Omitted when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub output_filters_bypassed: bool,
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
    /// Version 8: the room correction filter the output plays through, when
    /// it has one. Absent in replies to older versions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<OutputFilter>,
}

/// Where an output's filter comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterOrigin {
    /// A file the layout names (`correction_filter`), saved in the
    /// configuration.
    File,
    /// Taps a client uploaded ([`Command::SetOutputFilters`]): live, not
    /// saved until the Studio's Save writes them to a file.
    Live,
}

/// How far the renderer got with an output's filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterLoadState {
    /// Being read and prepared.
    Pending,
    /// In use (unless [`Snapshot::output_filters_bypassed`]).
    Loaded,
    /// Unusable: the output plays unfiltered, [`OutputFilter::error`] says why.
    Error,
}

/// Version 8: one output's room correction filter in a snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputFilter {
    pub origin: FilterOrigin,
    /// The file, for [`FilterOrigin::File`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    pub state: FilterLoadState,
    /// Rate the filter was designed at. Known for a live filter, and for a
    /// file once loaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    /// Length at that rate, as uploaded or read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taps: Option<u32>,
    /// [`taps_hash_hex`] of the taps at that rate, as uploaded or read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Version 8: one output's filter in [`Command::SetOutputFilters`]: an
/// uploaded filter's [`FilterUpload::upload_id`], or `null` for no filter
/// (which also drops a file the layout names, until a Reload).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputFilterSetting {
    pub output_id: String,
    pub filter: Option<String>,
}

/// Version 8: the state of one [`Command::UploadFilter`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilterUpload {
    pub upload_id: String,
    pub sample_rate: u32,
    pub total_taps: u32,
    pub received_taps: u32,
    /// [`taps_hash_hex`] of the whole filter, once every tap arrived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}

/// Most taps one [`Command::UploadFilter`] carries: 32 KiB of `f32`, about
/// 43 KiB of base64, inside [`MAX_PACKET_BYTES`] with the rest of the JSON.
/// The default 8192-tap filter travels in one chunk.
pub const MAX_UPLOAD_CHUNK_TAPS: usize = 8192;

/// Longest filter accepted, in seconds at its own rate.
pub const MAX_FILTER_SECONDS: f64 = 1.5;

/// Rates a filter may be designed at besides the renderer's own.
pub const FILTER_SOURCE_RATES: std::ops::RangeInclusive<u32> = 32_000..=384_000;

/// Most taps a filter at `sample_rate` may have ([`MAX_FILTER_SECONDS`]).
pub fn max_filter_taps(sample_rate: u32) -> usize {
    (MAX_FILTER_SECONDS * f64::from(sample_rate)) as usize
}

/// The content hash of a filter: 64-bit FNV-1a over the little-endian bytes
/// of its `f32` taps, as 16 lowercase hex digits.
pub fn taps_hash_hex(taps: &[f32]) -> String {
    format!("{:016x}", taps_hash(taps))
}

/// [`taps_hash_hex`] as a number.
pub fn taps_hash(taps: &[f32]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for tap in taps {
        for byte in tap.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// `taps` as [`Command::UploadFilter`] carries them: standard base64 (with
/// padding) of their little-endian `f32` bytes.
pub fn encode_taps(taps: &[f32]) -> String {
    let bytes: Vec<u8> = taps.iter().flat_map(|t| t.to_le_bytes()).collect();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(BASE64[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The taps [`encode_taps`] wrote. Refuses anything else: another alphabet,
/// missing padding, or a length that is not whole `f32`s.
pub fn decode_taps(text: &str) -> Result<Vec<f32>, &'static str> {
    const INVALID: &str = "taps must be base64 of little-endian 32-bit floats";
    let text = text.as_bytes();
    if !text.len().is_multiple_of(4) {
        return Err(INVALID);
    }
    let mut bytes = Vec::with_capacity(text.len() / 4 * 3);
    for (index, quad) in text.chunks(4).enumerate() {
        let last = index + 1 == text.len() / 4;
        let padding = quad.iter().rev().take_while(|&&c| c == b'=').count();
        if padding > 2 || (padding > 0 && !last) {
            return Err(INVALID);
        }
        let mut n = 0u32;
        for &c in &quad[..4 - padding] {
            let value = match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => return Err(INVALID),
            };
            n = (n << 6) | u32::from(value);
        }
        n <<= 6 * padding as u32;
        let decoded = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        bytes.extend_from_slice(&decoded[..3 - padding]);
    }
    if !bytes.len().is_multiple_of(4) {
        return Err(INVALID);
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
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
    /// Version 8: the targets play as positioned sources (`bass_managed` on
    /// Start). Omitted when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bass_managed: bool,
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
