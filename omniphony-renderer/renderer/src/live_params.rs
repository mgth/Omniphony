//! Live-tunable renderer parameters shared between the render thread and the OSC listener.
//!
//! # Design
//!
//! `RendererControl` is wrapped in an `Arc` and held by both the `SpatialRenderer`
//! (reads) and the `OscSender` listener thread (writes).  The render thread takes a
//! snapshot at the beginning of each frame so that the `RwLock` on `LiveParams` is
//! held for the shortest possible time.
//!
//! Speaker position updates (via `/omniphony/control/speaker/{idx}/{az|el|distance}` +
//! `/omniphony/control/speakers/apply`) trigger a background recompute of the VBAP
//! panner.  The finished panner is stored directly via `RendererControl.vbap`
//! (an `ArcSwap`), so the render thread picks it up lock-free at the next frame.

use anyhow::Result;
use arc_swap::ArcSwap;
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use crate::backend_registry::{BackendRegistry, TopologyBuildPlan, prepare_topology_build_plan};
use crate::render_backend::{EvaluationBuildConfig, PreparedRenderEngine, RenderRequest};
use crate::spatial_vbap::VbapTableMode;
use crate::speaker_layout::SpeakerLayout;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveEvaluationMode {
    Auto,
    Realtime,
    PrecomputedPolar,
    PrecomputedCartesian,
}

impl LiveEvaluationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Realtime => "realtime",
            Self::PrecomputedPolar => "precomputed_polar",
            Self::PrecomputedCartesian => "precomputed_cartesian",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "realtime" | "direct" => Some(Self::Realtime),
            "precomputed_polar" | "polar" => Some(Self::PrecomputedPolar),
            "precomputed_cartesian" | "cartesian" => Some(Self::PrecomputedCartesian),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreferredEvaluationMode {
    PrecomputedPolar,
    PrecomputedCartesian,
}

impl PreferredEvaluationMode {
    pub fn from_vbap_table_mode(mode: VbapTableMode) -> Self {
        match mode {
            VbapTableMode::Polar => Self::PrecomputedPolar,
            VbapTableMode::Cartesian { .. } => Self::PrecomputedCartesian,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RampMode {
    Off,
    Frame,
    Sample,
    /// One VBAP evaluation per object per frame (the destination gains), then a
    /// per-sample linear interpolation of the gains from the previous block's
    /// end to this block's end. Cheaper than `Sample` (no per-sample VBAP) while
    /// keeping per-sample smoothness.
    Interp,
}

impl RampMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Frame => "frame",
            Self::Sample => "sample",
            Self::Interp => "interp",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "frame" | "per_frame" => Some(Self::Frame),
            "sample" | "per_sample" => Some(Self::Sample),
            "interp" | "sample_interp" => Some(Self::Interp),
            _ => None,
        }
    }
}

/// How channel-based (non-object) content is rendered. Applies only to streams
/// that carry no spatial objects (plain EAC3 / TrueHD beds, AC3, multichannel
/// PCM); object streams are unaffected. Shared by the CLI/spdif decode path and
/// the embedded mpv host so both behave identically.
///
/// The placement of each input channel (direct to a speaker, or virtualized as
/// an object at a position) is decided per channel by the parametrable virtual
/// bed (`LiveParams::virtual_bed`); this enum only selects the global policy:
/// let the host decode it (`Host`), or render it through the virtual bed
/// (`Spatial`). The legacy `direct`/`virtual` values deserialise to `Spatial`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelRenderMode {
    /// Let the host deal with it: the embedded mpv decoder declines so mpv falls
    /// back to its native decoder (`ad_lavc`); the CLI outputs the decoded
    /// channels straight to the sink (no spatialization).
    Host,
    /// Render through the virtual bed: each channel is either routed direct to
    /// its matching speaker (`spatialize:false` in the virtual bed, e.g. LFE) or
    /// virtualized as an object at the bed's configured position and VBAP-panned
    /// (`spatialize:true`). The default. Accepts the old `direct`/`virtual`
    /// config values as aliases.
    #[default]
    #[serde(alias = "virtual", alias = "direct")]
    Spatial,
}

impl ChannelRenderMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Spatial => "spatial",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "host" | "native" | "passthrough" => Some(Self::Host),
            // `direct`/`virtual` are legacy aliases: placement is now per-channel
            // in the virtual bed, so both collapse to the single `Spatial` mode.
            "spatial" | "virtual" | "virtual_objects" | "objects" | "direct"
            | "direct_speakers" => Some(Self::Spatial),
            _ => None,
        }
    }
}

/// Phantom-source extraction algorithm selected for synthesized objects.
///
/// This is deliberately separate from the global synthesized-object master:
/// the user can prepare/tune a method while synthesis is disabled, then restore
/// it without losing the choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhantomExtractMode {
    /// Do not run the phantom extraction stage.
    #[default]
    Off,
    /// Pairwise time-domain extraction.
    Broadband,
    /// Per-band STFT extraction.
    Spectral,
}

impl PhantomExtractMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Broadband => "broadband",
            Self::Spectral => "spectral",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" | "disabled" | "none" => Some(Self::Off),
            "broadband" | "wideband" => Some(Self::Broadband),
            "spectral" | "per_band" | "per-band" => Some(Self::Spectral),
            _ => None,
        }
    }
}

/// Where the surround pair (`Ls`/`Rs`) of a channel-based source WITHOUT
/// dedicated back channels (4.x / 5.x) is placed when rendered through the
/// virtual bed. Sources that already carry back channels (7.x: `Lb`/`Rb`/`Cb`)
/// ignore this — their surrounds are unambiguous. Live-tunable via
/// `/omniphony/control/surround_placement`, persisted to config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurroundPlacement {
    /// Side surrounds (the historical placement): `Ls`/`Rs` at the side corner
    /// (≈±90°).
    #[default]
    Side,
    /// Rear/back surrounds: `Ls`/`Rs` at the back corner (≈±135°); a surround
    /// routed direct (not spatialized) goes to a back output speaker when the
    /// layout has one.
    Back,
}

impl SurroundPlacement {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Side => "side",
            Self::Back => "back",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "side" | "sides" | "side_surround" => Some(Self::Side),
            "back" | "rear" | "back_surround" | "rear_surround" => Some(Self::Back),
            _ => None,
        }
    }
}

/// How the renderer's output channels map to the physical device ports. The
/// output is always the user's speaker layout in order; this selects whether each
/// output channel is tagged with its spatial position (so a position-aware
/// host/sink routes by position) or left positionless so port N carries layout
/// speaker N. Live-tunable via `/omniphony/control/output_channel_mapping`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputChannelMapping {
    /// Positionless: output port N = layout speaker N, in order, no position tags.
    /// Matches a custom DAC wired to the layout order (and what ASIO/CoreAudio
    /// already do). The default.
    #[default]
    ByIndex,
    /// Positional: tag each output channel with its speaker position (FC, …) so a
    /// position-aware host/sink routes by position. For standard layouts feeding a
    /// standard sink/AVR.
    ByName,
}

impl OutputChannelMapping {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ByIndex => "by_index",
            Self::ByName => "by_name",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "by_index" | "index" | "raw" | "positionless" | "aux" => Some(Self::ByIndex),
            "by_name" | "name" | "positional" | "position" => Some(Self::ByName),
            _ => None,
        }
    }

    /// Small code for the C FFI: 0 = by_index (default), 1 = by_name.
    pub fn code(self) -> i32 {
        match self {
            Self::ByIndex => 0,
            Self::ByName => 1,
        }
    }

    /// Inverse of [`Self::code`]; any other value is ignored by the caller.
    pub fn from_code(code: i32) -> Option<Self> {
        match code {
            0 => Some(Self::ByIndex),
            1 => Some(Self::ByName),
            _ => None,
        }
    }
}

/// Output rendering path: a multichannel speaker array (VBAP) or an independent
/// 2-channel headphone (binaural) stage. See [`crate::binaural`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputMode {
    /// Classic VBAP render to the configured speaker layout.
    #[default]
    SpeakerArray,
    /// Independent binaural render to stereo (ITD/ILD/HRTF) for headphones.
    Binaural,
}

impl OutputMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SpeakerArray => "speaker",
            Self::Binaural => "binaural",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "speaker" | "speakers" | "speaker_array" | "vbap" => Some(Self::SpeakerArray),
            "binaural" | "headphone" | "headphones" => Some(Self::Binaural),
            _ => None,
        }
    }
}

/// Early-reflection (shoebox) settings for the binaural stage. World-fixed
/// room, listener at the centre; six first-order image sources per channel.
/// The direct/reflected ratio falling with distance is the main
/// externalization / distance cue an anechoic HRTF render lacks.
#[derive(Debug, Clone, PartialEq)]
pub struct BinauralReflections {
    /// Master enable for the reflection bank.
    pub enabled: bool,
    /// Room extents in metres: [width (x), depth (y), height (z)].
    pub room_size_m: [f32; 3],
    /// Per-reflection wall gain (0..1) applied on top of the 1/d law.
    pub level: f32,
}

impl Default for BinauralReflections {
    fn default() -> Self {
        Self {
            // Off by default (dry headphone output); opt-in like the late
            // reverb. Room size / level below apply once enabled.
            enabled: false,
            room_size_m: [4.0, 5.0, 2.7],
            level: 0.5,
        }
    }
}

/// Late-reverb (FDN) settings for the binaural stage. Models the LISTENING
/// room — a small, dry, constant space like the room around a loudspeaker
/// setup — not the scene's acoustics (those are in the content and pass
/// through). The reverberant field level is distance-independent while the
/// direct falls as 1/d, so the direct/reverb ratio carries distance.
#[derive(Debug, Clone, PartialEq)]
pub struct BinauralReverb {
    /// Master enable for the late tail.
    pub enabled: bool,
    /// Return level (0..1) of the reverb bus.
    pub level: f32,
    /// Broadband decay time (s). Living-room-ish by default; cinema halls
    /// are deliberately NOT the target.
    pub rt60_s: f32,
    /// Pre-delay (ms) between the direct sound and the start of the tail.
    pub predelay_ms: f32,
}

impl Default for BinauralReverb {
    fn default() -> Self {
        Self {
            // Off by default: the late-reverb tail isn't convincing enough yet,
            // so headphone output is dry unless the user opts in. The level/
            // rt60/predelay below are the values used once it's enabled.
            enabled: false,
            level: 0.25,
            rt60_s: 0.35,
            predelay_ms: 20.0,
        }
    }
}

/// Live-tunable parameters for the binaural (headphone) output stage.
///
/// `unit_scale_m` is an **isotropic** metres-per-ADM-unit factor for distance
/// cues only — `room_ratio` is intentionally not reused here (it is anisotropic
/// and would distort directions / HRTF localisation).
#[derive(Debug, Clone)]
pub struct BinauralLiveParams {
    /// Selected output path. `SpeakerArray` keeps the classic VBAP renderer.
    pub output_mode: OutputMode,
    /// Metres represented by one ADM unit; scales physical distance for the
    /// 1/d gain and ITD/ILD without altering object directions.
    pub unit_scale_m: f32,
    /// Effective head radius (m) for the Woodworth ITD model — half the
    /// inter-ear distance. Per-listener fit; default is KEMAR-ish.
    pub head_radius_m: f32,
    /// Current (smoothed) head orientation applied to world positions. Updated by
    /// the head-tracking input or set directly via the `head/*` OSC controls.
    pub head_pose: crate::binaural::HeadPose,
    /// Live head-tracking input config + recenter/smoothing state (SensorsOSC).
    pub tracking: crate::binaural::HeadTracking,
    /// HRIR data set to convolve with (synthetic / embedded KEMAR / SOFA).
    pub hrir_source: crate::binaural::HrirSource,
    /// Shoebox early-reflection settings (externalization).
    pub reflections: BinauralReflections,
    /// Late-reverb tail settings (distance / externalization).
    pub reverb: BinauralReverb,
    /// Distance low-pass on the direct path (air absorption): physically
    /// true indoors and outdoors, the natural "far sounds dull" cue.
    pub air_absorption: bool,
}

impl Default for BinauralLiveParams {
    fn default() -> Self {
        Self {
            output_mode: OutputMode::default(),
            unit_scale_m: 1.0,
            head_radius_m: crate::binaural::itd::DEFAULT_HEAD_RADIUS_M,
            head_pose: crate::binaural::HeadPose::identity(),
            tracking: crate::binaural::HeadTracking::default(),
            hrir_source: crate::binaural::HrirSource::default(),
            reflections: BinauralReflections::default(),
            reverb: BinauralReverb::default(),
            air_absorption: true,
        }
    }
}

/// Live-tunable parameters for a single input object (bed or audio object).
#[derive(Clone)]
pub struct ObjectLiveParams {
    /// Mute flag; when true the object is silenced.
    pub muted: bool,
}

impl Default for ObjectLiveParams {
    fn default() -> Self {
        Self { muted: false }
    }
}

/// Live-tunable parameters for a single output speaker.
#[derive(Clone)]
pub struct SpeakerLiveParams {
    /// Linear gain override (default 1.0 = unity).
    pub gain: f32,
    /// Mute flag — independent of `gain`; unmuting restores the stored value.
    pub muted: bool,
    /// Delay in milliseconds applied via a fractional delay line (default 0.0).
    pub delay_ms: f32,
}

impl Default for SpeakerLiveParams {
    fn default() -> Self {
        Self {
            gain: 1.0,
            muted: false,
            delay_ms: 0.0,
        }
    }
}

#[derive(Clone, Copy)]
pub struct CartesianEvaluationParams {
    pub x_size: usize,
    pub y_size: usize,
    pub z_size: usize,
    pub z_neg_size: usize,
}

#[derive(Clone, Copy)]
pub struct PolarEvaluationParams {
    pub azimuth_values: i32,
    pub elevation_values: i32,
    pub distance_res: i32,
    pub distance_max: f32,
}

#[derive(Clone, Copy)]
pub struct EvaluationLiveParams {
    pub mode: LiveEvaluationMode,
    pub position_interpolation: bool,
    pub cartesian: CartesianEvaluationParams,
    pub polar: PolarEvaluationParams,
    /// Number of object-size intervals to precompute (0 = single table, the
    /// default; `N` ⇒ `N + 1` size tables interpolated at read time). Applies to
    /// both precomputed modes; ignored for backends without `supports_event_size`.
    pub object_size_intervals: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct ExperimentalDistanceLiveParams {
    pub distance_floor: f32,
    pub min_active_speakers: usize,
    pub max_active_speakers: usize,
    pub position_error_floor: f32,
    pub position_error_nearest_scale: f32,
    pub position_error_span_scale: f32,
}

impl Default for ExperimentalDistanceLiveParams {
    fn default() -> Self {
        Self {
            distance_floor: 0.05,
            min_active_speakers: 2,
            max_active_speakers: 8,
            position_error_floor: 0.08,
            position_error_nearest_scale: 0.75,
            position_error_span_scale: 0.3,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct BarycenterLiveParams {
    pub localize: f32,
}

impl Default for BarycenterLiveParams {
    fn default() -> Self {
        Self { localize: 0.0 }
    }
}

/// Runtime tuning parameters for the hybrid backend, which blends two concrete
/// backends ("external"/"internal") as a function of normalised distance.
#[derive(Debug, Clone)]
pub struct HybridLiveParams {
    /// Backend id mixed in at ratio = 1 (cube surface end of the curve).
    pub external_backend_id: String,
    /// Backend id mixed in at ratio = 0 (centre end of the curve).
    pub internal_backend_id: String,
    /// Editable blend curve: `(normalised_distance, ratio)` control points,
    /// ratio = weight of the external backend.
    pub curve: Vec<[f32; 2]>,
    /// Curve smoothing in `[0, 1]`: 0 = piecewise-linear, 1 = full spline.
    pub curve_smoothing: f32,
    /// Metric used to reduce a position to the (normalised) blend distance.
    /// Chebyshev (default) reaches 1 on the cube surface; spherical reaches √3
    /// at a corner.
    pub metric: crate::spatial_vbap::DistanceMetric,
}

impl Default for HybridLiveParams {
    fn default() -> Self {
        Self {
            external_backend_id: "vbap".to_string(),
            internal_backend_id: "barycenter".to_string(),
            curve: vec![[0.0, 0.0], [1.0, 1.0]],
            curve_smoothing: 0.0,
            metric: crate::spatial_vbap::DistanceMetric::Chebyshev,
        }
    }
}

/// Live-tunable rendering parameters.
///
/// Written (exclusively) by the OSC listener thread, read via snapshot by the
/// render thread.
pub struct LiveParams {
    /// Master output gain, linear scale (1.0 = unity, 0.5 ≈ −6 dB).
    pub master_gain: f32,

    /// Per-object live parameters (mute).
    /// Absent entries use `ObjectLiveParams::default()` (muted=false).
    pub objects: HashMap<usize, ObjectLiveParams>,

    /// Minimum spread applied when the object spread value is 0.0.
    pub spread_min: f32,

    /// Maximum spread applied when the object spread value is 1.0.
    pub spread_max: f32,

    /// Derive spread from distance rather than from object spread metadata.
    pub spread_from_distance: bool,

    /// Distance (normalised) at which spread reaches 0.0.
    pub spread_distance_range: f32,

    /// Curve exponent for the distance-based spread formula.
    pub spread_distance_curve: f32,

    /// Reduction policy applied to the per-event 3-D object size triplet
    /// (w, d, h) to derive a scalar spread for backends that consume it.
    pub size_to_spread_mode: crate::render_backend::SizeToSpreadMode,

    /// Ramp processing mode for object moves and gain transitions.
    pub ramp_mode: RampMode,

    /// Requested spatial render backend identifier.
    pub backend_id: String,

    /// Requested evaluation parameters for the current gain model.
    pub evaluation: EvaluationLiveParams,

    /// Apply dialogue normalisation gain stored in the renderer.
    pub use_loudness: bool,

    /// Automatic gain reduction: when set, the gain stage permanently lowers
    /// output gain on detected clipping (peak hold, no recovery). Live-tunable
    /// via `/omniphony/control/auto_gain`.
    pub auto_gain: bool,

    /// Target ceiling (dBFS) that auto-gain corrects detected peaks down to.
    /// Clipping is detected at 0 dBFS (peak > 1.0); when it fires, the master
    /// gain is lowered so the peak lands at this level instead of exactly 0 dBFS,
    /// leaving headroom so corrections fire less often. Default −1 dBFS.
    pub auto_gain_ceiling_db: f32,

    /// Distance attenuation model currently applied by the renderer.
    pub distance_model: crate::spatial_vbap::DistanceModel,

    /// Metric (spherical / chebyshev) used to reduce a position to a scalar
    /// distance for the distance model stage.
    pub distance_model_metric: crate::spatial_vbap::DistanceMetric,

    /// Metric (spherical / chebyshev) used by the distance diffuse stage.
    pub distance_diffuse_metric: crate::spatial_vbap::DistanceMetric,

    /// Per-speaker live parameters: gain, mute, delay.
    /// Absent entries use `SpeakerLiveParams::default()` (gain=1.0, muted=false, delay=0 ms).
    pub speakers: HashMap<usize, SpeakerLiveParams>,

    /// Room proportions `[width, length, height]` used to scale ADM coordinates
    /// before VBAP panning.  Updated live via `/omniphony/control/room_ratio`.
    pub room_ratio: [f32; 3],

    /// Rear depth ratio used by the non-linear depth warp (`depth < 0`) for object rendering.
    /// Updated live via `/omniphony/control/room_ratio_rear`.
    pub room_ratio_rear: f32,

    /// Lower height ratio used for negative Z coordinates.
    /// Updated live via `/omniphony/control/room_ratio_lower`.
    pub room_ratio_lower: f32,

    /// Blend position for depth warp center ratio (0.0 = rear, 1.0 = front).
    /// Updated live via `/omniphony/control/room_ratio_center_blend`.
    pub room_ratio_center_blend: f32,

    /// Raw dialogue_level value extracted from the bitstream (dBFS, e.g. −27).
    /// `None` until the first major_sync is decoded.
    /// Written by `SpatialRenderer::set_loudness`; read by the OSC sender
    /// to compute and broadcast the applied gain.
    pub dialogue_level: Option<i8>,

    /// Enable distance-based mirrored diffuse blending.
    ///
    /// When active, each object's VBAP gains are blended with the gains of a
    /// mirror image of its position, selected by `distance_diffuse_mirror_axes`.
    /// The mix is controlled by the ADM distance (pre-room_ratio):
    ///   - dist = 0  →  50 % direct + 50 % mirror  (iso-energy weights: √0.5 each)
    ///   - dist ≥ `distance_diffuse_threshold`  →  100 % direct
    pub use_distance_diffuse: bool,

    /// ADM distance at which the blend reaches 100 % direct.  Default: 1.0.
    pub distance_diffuse_threshold: f32,

    /// Curve exponent applied to the normalised distance before computing the
    /// blend weight.  1.0 = linear, < 1 = fast-near, > 1 = slow-near.  Default: 1.0.
    pub distance_diffuse_curve: f32,

    /// ADM axes negated to build the diffuse mirror image.  Default `xy`, the
    /// half-turn about the vertical axis the stage has always used; `y` alone
    /// mirrors front/back, `xyz` inverts through the origin.  Updated live via
    /// `/omniphony/control/distance_diffuse/mirror_axes`.
    pub distance_diffuse_mirror_axes: crate::spatial_vbap::MirrorAxes,

    /// Runtime tuning parameters for the hybrid backend.
    pub hybrid: HybridLiveParams,

    /// Selected Dynamic Range Control mode (as string).
    pub drc_mode: String,
    /// DRC weighting in [0.0, 1.0]. 1.0 applies the full bridge-decoded DRC gain;
    /// 0.0 bypasses it entirely. Intermediate values scale the dB reduction
    /// linearly (effective_gain = bridge_gain.powf(drc_weight)).
    pub drc_weight: f32,

    /// Binaural (headphone) output stage parameters. When
    /// `binaural.output_mode == OutputMode::Binaural`, the renderer bypasses the
    /// speaker/VBAP path and emits a 2-channel frame instead.
    pub binaural: BinauralLiveParams,

    /// How channel-based (non-object) content is rendered. Only consulted for
    /// streams that carry no spatial objects; object streams ignore it. Read
    /// identically by the CLI/spdif decode path and the embedded mpv host. This
    /// is an internal/host override, not a Studio or persistent live option.
    pub channel_render_mode: ChannelRenderMode,

    /// Where the 4.x/5.x surround pair (`Ls`/`Rs`) is placed: side vs back.
    /// Consulted only for channel content without dedicated back channels;
    /// 7.x sources ignore it. Live-tunable via
    /// `/omniphony/control/surround_placement`.
    pub surround_placement: SurroundPlacement,

    /// How output channels map to device ports: positionless `ByIndex` (default,
    /// port N = layout speaker N) or positional `ByName`. Consulted when the
    /// output stream is (re)configured. Live-tunable via
    /// `/omniphony/control/output_channel_mapping`.
    pub output_channel_mapping: OutputChannelMapping,

    /// Parametrable virtual bed for channel-based content (consulted only when
    /// `channel_render_mode == Spatial`). One entry per input-channel label
    /// (`L`, `R`, `C`, `LFE`, `Ls`, `Rs`, `Lb`, `Rb`, …): `spatialize:true`
    /// virtualizes the channel as an object at the entry's position, `false`
    /// routes it direct to the matching output speaker (e.g. LFE → sub). `None`
    /// falls back to the built-in canonical poses (LFE direct, the rest
    /// virtualized). Live-tunable via the `virtual_bed` layout OSC controls.
    pub virtual_bed: Option<SpeakerLayout>,

    /// Selects the bed→height object generator (2D upmix): synthesizes height
    /// objects from channel-based content so a height-capable layout (7.1.4, …)
    /// is exercised when the source has no height. Empty / `"none"` = disabled
    /// (the default). Consulted only for channel content without spatial objects;
    /// object streams ignore it. Live-tunable via
    /// `/omniphony/control/object_generator`.
    pub object_generator_id: String,

    /// Live-tunable parameter overrides for the active object generator, keyed by
    /// the param `key` the generator declares in its schema. Sparse: an absent key
    /// uses the generator's built-in default. Cleared when the active generator
    /// changes. Set via `/omniphony/control/object_generator/param`.
    pub object_generator_params: std::collections::HashMap<String, f32>,

    /// Global permission for renderer-synthesized objects. When false, both the
    /// phantom extractor and height generator are bypassed without clearing
    /// their configured selections or parameters.
    pub synthetic_objects_enabled: bool,

    /// Phantom-source extraction algorithm. `Off` disables only this stage;
    /// the global synthesized-object master may independently suppress it.
    pub phantom_extract_mode: PhantomExtractMode,

    /// Live-tunable parameter overrides for the phantom-extraction stage, keyed by
    /// the param `key` it declares (`strength` / `passes` / `lift`). Sparse: an
    /// absent key uses the stage's default. Set via
    /// `/omniphony/control/phantom_extract/param`.
    pub phantom_params: std::collections::HashMap<String, f32>,
}

impl LiveParams {
    pub fn set_evaluation_mode(&mut self, mode: LiveEvaluationMode) {
        self.evaluation.mode = mode;
    }

    pub fn backend_id(&self) -> &str {
        self.backend_id.as_str()
    }

    pub fn requested_evaluation_mode(&self) -> LiveEvaluationMode {
        self.evaluation.mode
    }
}

/// Parse a `"width,length,height"` string into `[f32; 3]`.
/// Returns `[1.0, 1.0, 1.0]` on any parse error.
pub fn parse_room_ratio(s: &str) -> [f32; 3] {
    let parts: Vec<f32> = s.split(',').filter_map(|p| p.trim().parse().ok()).collect();
    if parts.len() == 3 {
        [parts[0], parts[1], parts[2]]
    } else {
        [1.0, 1.0, 1.0]
    }
}

#[derive(Debug, Clone, Copy)]
pub struct VbapModelRebuildParams {
    pub az_res_deg: i32,
    pub el_res_deg: i32,
    pub spread_resolution: f32,
    pub distance_max: f32,
    pub allow_negative_z: bool,
    pub distance_model: crate::spatial_vbap::DistanceModel,
}

#[derive(Debug, Clone, Copy)]
pub struct BackendRebuildParams {
    pub backend_id: &'static str,
    pub preferred_evaluation_mode: PreferredEvaluationMode,
    pub allow_negative_z: bool,
    pub vbap: Option<VbapModelRebuildParams>,
}

impl BackendRebuildParams {
    pub fn preferred_evaluation_mode(&self) -> PreferredEvaluationMode {
        self.preferred_evaluation_mode
    }
}

fn rebuild_params_allow_negative_z(params: Option<BackendRebuildParams>) -> bool {
    params.map(|value| value.allow_negative_z).unwrap_or(false)
}

fn evaluation_build_config_from_live(
    live: &LiveParams,
    allow_negative_z: bool,
) -> EvaluationBuildConfig {
    EvaluationBuildConfig {
        request_template: RenderRequest {
            adm_position: [0.0, 0.0, 0.0],
            event_size: [0.0, 0.0, 0.0],
            room_ratio: live.room_ratio,
            room_ratio_rear: live.room_ratio_rear,
            room_ratio_lower: live.room_ratio_lower,
            room_ratio_center_blend: live.room_ratio_center_blend,
            use_distance_diffuse: live.use_distance_diffuse,
            distance_diffuse_threshold: live.distance_diffuse_threshold,
            distance_diffuse_curve: live.distance_diffuse_curve,
            diffuse_mirror_axes: live.distance_diffuse_mirror_axes,
            distance_model: live.distance_model,
        },
        position_interpolation: live.evaluation.position_interpolation,
        cartesian: crate::render_backend::CartesianEvaluationConfig {
            x_size: live.evaluation.cartesian.x_size.max(1) + 1,
            y_size: live.evaluation.cartesian.y_size.max(1) + 1,
            z_size: live.evaluation.cartesian.z_size.max(1) + 1,
            z_neg_size: live.evaluation.cartesian.z_neg_size,
        },
        polar: crate::render_backend::PolarEvaluationConfig {
            azimuth_values: live.evaluation.polar.azimuth_values.max(2) as usize,
            elevation_values: live.evaluation.polar.elevation_values.max(2) as usize,
            distance_values: live.evaluation.polar.distance_res.max(1) as usize + 1,
            distance_max: live.evaluation.polar.distance_max.max(0.01),
            allow_negative_z,
        },
        distance_model_metric: live.distance_model_metric,
        distance_diffuse_metric: live.distance_diffuse_metric,
        object_size_intervals: live.evaluation.object_size_intervals,
        object_size_mode: live.size_to_spread_mode,
    }
}

/// Immutable render-time snapshot published atomically to the audio thread.
///
/// This is the only topology state the renderer should consume during a frame:
/// the speaker layout, the VBAP panner built for that layout, and the derived
/// mappings that tie both together.
pub struct RenderTopology {
    pub speaker_layout: SpeakerLayout,
    pub backend: Arc<PreparedRenderEngine>,
    pub backend_to_speaker_mapping: Option<Vec<usize>>,
    /// Per-label speaker lookup for the channel-routing table (re-resolved on
    /// every topology rebuild, so stored labels survive layout swaps).
    pub label_to_speaker: HashMap<bridge_api::RChannelLabel, usize>,
    pub num_speakers: usize,
    pub num_spatializable: usize,
    /// The `RendererControl::geometry_generation` this topology's gain models were
    /// built at. A recompute whose generation matches can reuse `backend`'s
    /// decorated model instead of re-triangulating. Defaults to 0 (initial build).
    pub geometry_generation: u64,
}

impl RenderTopology {
    pub fn new(backend: Arc<PreparedRenderEngine>, speaker_layout: SpeakerLayout) -> Result<Self> {
        let num_speakers = speaker_layout.num_speakers();
        let (_, spatializable_mapping) = speaker_layout.spatializable_positions();
        let num_spatializable = spatializable_mapping.len();
        let backend_speakers = backend.speaker_count();

        let backend_to_speaker_mapping = if backend_speakers == num_speakers {
            log::info!(
                "Render backend uses expanded speaker-domain format ({} speakers)",
                num_speakers
            );
            None
        } else if backend_speakers == num_spatializable {
            log::info!(
                "Render backend uses spatializable-domain format ({} spatializable of {} total) - using mapping",
                num_spatializable,
                num_speakers
            );
            Some(spatializable_mapping)
        } else {
            return Err(anyhow::anyhow!(
                "Render backend speaker mismatch: backend has {} speakers but layout has {} total ({} spatializable)",
                backend_speakers,
                num_speakers,
                num_spatializable
            ));
        };

        Ok(Self {
            label_to_speaker: speaker_layout.label_to_speaker_mapping(),
            num_speakers,
            num_spatializable,
            speaker_layout,
            backend,
            backend_to_speaker_mapping,
            geometry_generation: 0,
        })
    }

    /// Set the geometry generation this topology was built at (chaining helper).
    pub fn with_geometry_generation(mut self, generation: u64) -> Self {
        self.geometry_generation = generation;
        self
    }

    pub fn backend_speaker_index_for_layout_speaker(&self, speaker_index: usize) -> Option<usize> {
        match self.backend_to_speaker_mapping.as_ref() {
            None => {
                if speaker_index < self.num_speakers {
                    Some(speaker_index)
                } else {
                    None
                }
            }
            Some(mapping) => mapping.iter().position(|&mapped| mapped == speaker_index),
        }
    }
}

/// Shared control object held by both `SpatialRenderer` and `OscSender`.
///
/// The renderer reads `live` via a snapshot and loads the current immutable
/// `RenderTopology` lock-free at the start of each frame. The OSC listener writes
/// `live`, edits the staging layout, rebuilds a new `RenderTopology` in the
/// background, then publishes it atomically.
pub struct RendererControl {
    /// Live-tunable parameters (protected by a readers-writer lock).
    pub live: RwLock<LiveParams>,

    /// Current render topology, shared between render thread (reads) and OSC
    /// listener (writes on recompute).  Lock-free: the render thread loads an
    /// `Arc` snapshot at the start of each frame; the OSC thread stores a new
    /// `Arc` when a recompute finishes.
    pub topology: ArcSwap<RenderTopology>,

    /// Editable speaker layout staged before publication into `topology`.
    pub editable_layout: Mutex<SpeakerLayout>,

    /// Parameters needed to recompute the VBAP table when speaker positions change.
    ///
    /// `None` when the renderer was constructed from a pre-loaded table (`from_vbap`),
    /// because recomputation is not supported in that case.
    pub backend_rebuild_params: RwLock<Option<BackendRebuildParams>>,

    /// `true` while a VBAP recompute is running in the background.
    pub recomputing: AtomicBool,

    /// `true` when live params have been changed via OSC since the last save.
    /// Reset to `false` by a successful `/omniphony/control/save_config`.
    pub config_dirty: AtomicBool,

    /// Bumped whenever per-object live params change.
    pub object_params_generation: std::sync::atomic::AtomicU64,

    /// Bumped whenever per-speaker live params change.
    pub speaker_params_generation: std::sync::atomic::AtomicU64,

    /// Bumped whenever live state changes in a way clients should see without an
    /// explicit request (e.g. auto-gain lowering the master gain on the audio
    /// thread). The engine's OSC listener polls this and re-broadcasts the
    /// live-state bundle when it changes, coalesced to the listener's poll cadence.
    pub live_state_generation: std::sync::atomic::AtomicU64,

    /// Monotonic counter bumped whenever a change affects the backend *geometry*
    /// (speaker positions / triangulation or the decorator metrics) — as opposed
    /// to evaluation-only changes (mode, grid resolution). A topology records the
    /// generation it was built at; a recompute whose generation matches the active
    /// topology's reuses the existing gain models and rebuilds only the evaluation
    /// wrapper, avoiding re-triangulation. See `build_topology_reusing`.
    pub geometry_generation: std::sync::atomic::AtomicU64,

    /// Monotonic counter bumped whenever a live option flagged `REPLAN` in the
    /// declared registry ([`crate::options`]) changes. Plan signatures compare
    /// this single epoch instead of enumerating options field by field, so a
    /// new re-planning option cannot be forgotten in a signature.
    pub options_epoch: std::sync::atomic::AtomicU64,

    /// Set by the gain stage whenever output clipping is detected (peak > 0 dBFS),
    /// independently of whether auto-gain is enabled. Holds the index of the speaker
    /// channel that held the peak, or `-1` when no clip is pending. The OSC listener
    /// polls and clears it to emit a one-shot `/omniphony/state/clip <speaker_idx>`
    /// so clients can flash clip indicators. Coalesced: many clipping frames between
    /// polls collapse to one event carrying the most recent offending speaker.
    pub clip_pending: AtomicI32,

    /// Path of the active config file, used by the save-config handler.
    /// Set after construction via `set_config_path()`.
    pub config_path: Mutex<Option<PathBuf>>,

    /// Diagnostic: did the active config path actually load, or did the host
    /// silently fall back to defaults? One of "loaded"/"missing"/"parse_error",
    /// or `None` when no config path was provided (defaults by design). Set at
    /// construction; broadcast to Studio's About panel.
    pub config_status: Mutex<Option<String>>,

    /// Non-empty when the renderer is running in the degraded "no decoder" mode
    /// because the bridge could not be resolved/loaded. Broadcast over OSC so
    /// Studio can surface a red banner. `None`/empty in normal operation.
    pub bridge_error: Mutex<Option<String>>,

    /// C-ABI version pair of the FFI shim hosting this engine (liborender),
    /// set by the shim at session start. `None` for hosts that link the engine
    /// as a Rust crate (the orender CLI). Broadcast to Studio's About panel
    /// next to the build fingerprint.
    pub host_abi: Mutex<Option<(u32, u32)>>,

    /// Actual renderer input path used for this process.
    pub input_path: Mutex<Option<String>>,
    /// Requested format bridge path to be persisted into render.bridge_path.
    pub bridge_path: Mutex<Option<PathBuf>>,
    /// Supported DRC modes reported by the bridge.
    pub bridge_supported_drc_modes: Mutex<Vec<String>>,
    /// Requested ramp mode from OSC control.
    pub requested_ramp_mode: Mutex<RampMode>,

    /// OSC meter cadence in Hz (`f32::to_bits`). Read lock-free by `AudioMeter`
    /// each poll; OSC-adjustable and persisted to config. The renderer is the
    /// source of truth (not the studio client).
    pub meter_rate_hz_bits: Arc<std::sync::atomic::AtomicU32>,
    /// OSC diag-publication cadence in Hz (`f32::to_bits`). Read lock-free by
    /// the diag publisher; OSC-adjustable and persisted to config.
    pub diag_rate_hz_bits: Arc<std::sync::atomic::AtomicU32>,

    /// Available render backends, queried by `prepare_topology_rebuild_for_layout`
    /// to build the active backend by id. Defaults to the built-ins; a host
    /// registers extra backends at startup via [`RendererControl::register_backend`].
    /// Behind a lock because registration happens after construction (the control
    /// is already shared); only read off the audio hot path (topology rebuild).
    backend_registry: RwLock<BackendRegistry>,

    /// Host-set backend parameter values, keyed by `backend_id` then param key
    /// (see [`crate::backend_params`]). Generic so a backend's params need no
    /// typed field here. Read at topology-build time, never on the audio hot path.
    backend_params: RwLock<HashMap<String, HashMap<String, crate::backend_params::ParamValue>>>,

    /// JSON schema (`[{id,label,i18nKey,params:[…]}]`) of the available bed→height
    /// object generators, set by the engine from its registry (which lives in
    /// `orender_engine` and so can't be held here as a typed registry). Published
    /// to Studio so host-registered (out-of-tree) generators appear too. `"[]"`
    /// until the engine sets it.
    object_generators_schema: RwLock<String>,

    /// JSON schema (`[{key,label,i18nKey,…}]`) of the phantom-extraction stage's
    /// declared params, set by the engine so Studio builds its sliders. `"[]"`
    /// until the engine sets it.
    phantom_schema: RwLock<String>,

    /// Canonical fixed-channel editor catalogue supplied by the engine. Kept as
    /// JSON because the canonical poses live in `orender_engine`, above this
    /// crate in the dependency graph.
    fixed_channel_catalog: RwLock<String>,

    /// Current fixed-channel/synthesized-object applicability state supplied by
    /// the engine on declaration/topology/option changes (never per sample).
    fixed_channel_processing: RwLock<String>,
}

impl RendererControl {
    /// Create a new `RendererControl` and wrap it in an `Arc`.
    ///
    /// * `live`                – initial live parameters.
    /// * `initial_topology`    – the initial coherent render topology.
    /// * `layout`              – editable speaker layout staging area for OSC mutations.
    /// * `vbap_rebuild_params` – see field docs; `None` for pre-loaded tables.
    pub fn new(
        live: LiveParams,
        initial_topology: RenderTopology,
        editable_layout: SpeakerLayout,
        backend_rebuild_params: Option<BackendRebuildParams>,
    ) -> Arc<Self> {
        Arc::new(Self {
            live: RwLock::new(live),
            topology: ArcSwap::new(Arc::new(initial_topology)),
            editable_layout: Mutex::new(editable_layout),
            backend_rebuild_params: RwLock::new(backend_rebuild_params),
            recomputing: AtomicBool::new(false),
            config_dirty: AtomicBool::new(false),
            object_params_generation: std::sync::atomic::AtomicU64::new(1),
            speaker_params_generation: std::sync::atomic::AtomicU64::new(1),
            live_state_generation: std::sync::atomic::AtomicU64::new(0),
            geometry_generation: std::sync::atomic::AtomicU64::new(0),
            options_epoch: std::sync::atomic::AtomicU64::new(0),
            clip_pending: AtomicI32::new(-1),
            config_path: Mutex::new(None),
            config_status: Mutex::new(None),
            bridge_error: Mutex::new(None),
            host_abi: Mutex::new(None),
            input_path: Mutex::new(None),
            bridge_path: Mutex::new(None),
            bridge_supported_drc_modes: Mutex::new(Vec::new()),
            requested_ramp_mode: Mutex::new(RampMode::Frame),
            // Seeded from config (or a host default) after construction.
            meter_rate_hz_bits: Arc::new(std::sync::atomic::AtomicU32::new(50.0_f32.to_bits())),
            diag_rate_hz_bits: Arc::new(std::sync::atomic::AtomicU32::new(50.0_f32.to_bits())),
            backend_registry: RwLock::new(BackendRegistry::builtin()),
            backend_params: RwLock::new(HashMap::new()),
            object_generators_schema: RwLock::new("[]".to_string()),
            phantom_schema: RwLock::new("[]".to_string()),
            fixed_channel_catalog: RwLock::new("[]".to_string()),
            fixed_channel_processing: RwLock::new(
                r#"{"stream":"idle","labels":[],"phantom":"no_stream","height":"no_stream"}"#
                    .to_string(),
            ),
        })
    }

    /// Register an additional render backend. Call at startup, before audio runs;
    /// a later registration with the same id overrides the earlier one. Selecting
    /// the backend by its id (`LiveParams::backend_id`) then routes a topology
    /// rebuild through it.
    pub fn register_backend(&self, factory: Box<dyn crate::backend_registry::BackendFactory>) {
        self.backend_registry.write().register(factory);
    }

    /// Set the published object-generator schema JSON (called by the engine from
    /// its registry, so any host-registered out-of-tree generators are included).
    pub fn set_object_generators_schema(&self, json: String) {
        *self.object_generators_schema.write() = json;
    }

    /// The published object-generator schema JSON (`"[]"` until the engine sets it).
    pub fn object_generators_schema(&self) -> String {
        self.object_generators_schema.read().clone()
    }

    /// Set the published phantom-extraction param schema JSON (called by the engine).
    pub fn set_phantom_schema(&self, json: String) {
        *self.phantom_schema.write() = json;
    }

    /// The published phantom-extraction schema JSON (`"[]"` until the engine sets it).
    pub fn phantom_schema(&self) -> String {
        self.phantom_schema.read().clone()
    }

    pub fn set_fixed_channel_catalog(&self, json: String) {
        *self.fixed_channel_catalog.write() = json;
    }

    pub fn fixed_channel_catalog(&self) -> String {
        self.fixed_channel_catalog.read().clone()
    }

    /// Publish a new applicability snapshot only when it actually changed.
    pub fn set_fixed_channel_processing(&self, json: String) {
        let mut current = self.fixed_channel_processing.write();
        if *current != json {
            *current = json;
            drop(current);
            self.bump_live_state();
        }
    }

    pub fn fixed_channel_processing(&self) -> String {
        self.fixed_channel_processing.read().clone()
    }

    /// Whether a backend with this id is registered (built-in or host-registered).
    pub fn has_backend(&self, id: &str) -> bool {
        self.backend_registry.read().get(id).is_some()
    }

    /// Id + label of every registered backend, for the host to publish so the UI
    /// can list the selectable backends (built-in and contributor-registered).
    pub fn available_backends(&self) -> Vec<crate::backend_registry::BackendListing> {
        // Resolve dynamic schemas (e.g. the scriptable backend's, which depends
        // on its selected file) against the current param store, with File-kind
        // handles resolved to absolute renderer paths so the schema reader can open
        // the file. Hybrid composes the other backends, so it is forced to the end
        // of the selection combo regardless of registration order (see `hybrid_last`).
        let registry = self.backend_registry.read();
        let resolved = self.resolved_backend_params(&registry);
        let listings = registry.available_with(&resolved);
        crate::backend_registry::hybrid_last(listings)
    }

    /// Resolve File-kind param handles to absolute renderer paths so backend
    /// factories read a real path (see [`crate::backend_files`]). Takes the
    /// already-held registry guard to avoid re-locking it.
    fn resolved_backend_params(
        &self,
        registry: &BackendRegistry,
    ) -> HashMap<String, HashMap<String, crate::backend_params::ParamValue>> {
        let config_dir = self
            .config_path()
            .and_then(|path| path.parent().map(|dir| dir.to_path_buf()));
        let raw = self.backend_params.read();
        crate::backend_files::resolve_file_params(&raw, config_dir.as_deref(), |backend_id, key| {
            registry
                .get(backend_id)
                .map(|factory| {
                    factory.param_schema().iter().any(|spec| {
                        spec.key == key
                            && matches!(spec.kind, crate::backend_params::ParamKind::File { .. })
                    })
                })
                .unwrap_or(false)
        })
    }

    /// Set one backend parameter value (host/OSC). Stored generically and applied
    /// at the next topology rebuild.
    pub fn set_backend_param(
        &self,
        backend_id: &str,
        key: &str,
        value: crate::backend_params::ParamValue,
    ) {
        self.backend_params
            .write()
            .entry(backend_id.to_string())
            .or_default()
            .insert(key.to_string(), value);
    }

    /// A clone of the stored param values for `backend_id` (empty if none set),
    /// for the host to publish alongside the backend's schema.
    pub fn backend_params_for(
        &self,
        backend_id: &str,
    ) -> HashMap<String, crate::backend_params::ParamValue> {
        self.backend_params
            .read()
            .get(backend_id)
            .cloned()
            .unwrap_or_default()
    }

    /// A clone of the entire backend-param store (`backend_id -> key -> value`),
    /// for the host to persist to config.
    pub fn all_backend_params(
        &self,
    ) -> HashMap<String, HashMap<String, crate::backend_params::ParamValue>> {
        self.backend_params.read().clone()
    }

    /// Shared meter-cadence atomic (Hz bits) for `AudioMeter::new_with_rate_atomic`.
    pub fn meter_rate_atomic(&self) -> Arc<std::sync::atomic::AtomicU32> {
        Arc::clone(&self.meter_rate_hz_bits)
    }
    /// Current meter cadence in Hz.
    pub fn meter_rate_hz(&self) -> f32 {
        f32::from_bits(self.meter_rate_hz_bits.load(Ordering::Relaxed))
    }
    /// Set the meter cadence (Hz), clamped to `[1, 1000]`.
    pub fn set_meter_rate_hz(&self, hz: f32) {
        self.meter_rate_hz_bits
            .store(hz.clamp(1.0, 1000.0).to_bits(), Ordering::Relaxed);
    }
    /// Shared diag-cadence atomic (Hz bits) for the diag publisher.
    pub fn diag_rate_atomic(&self) -> Arc<std::sync::atomic::AtomicU32> {
        Arc::clone(&self.diag_rate_hz_bits)
    }
    /// Current diag-publication cadence in Hz.
    pub fn diag_rate_hz(&self) -> f32 {
        f32::from_bits(self.diag_rate_hz_bits.load(Ordering::Relaxed))
    }
    /// Set the diag-publication cadence (Hz), clamped to `[1, 1000]`.
    pub fn set_diag_rate_hz(&self, hz: f32) {
        self.diag_rate_hz_bits
            .store(hz.clamp(1.0, 1000.0).to_bits(), Ordering::Relaxed);
    }

    /// Store the active config file path so the save-config OSC handler can use it.
    pub fn set_config_path(&self, path: PathBuf) {
        *self.config_path.lock() = Some(path);
    }

    /// The active config file path, if one was resolved at construction. `None`
    /// means the renderer is running on built-in defaults (no config loaded) —
    /// the very condition Studio surfaces in About to diagnose CLI-vs-host
    /// config mismatches.
    pub fn config_path(&self) -> Option<PathBuf> {
        self.config_path.lock().clone()
    }

    /// Record whether the active config path actually loaded (see field docs).
    pub fn set_config_status(&self, status: Option<String>) {
        *self.config_status.lock() = status;
    }

    pub fn config_status(&self) -> Option<String> {
        self.config_status.lock().clone()
    }

    /// Record the degraded "no decoder" bridge error (see field docs).
    pub fn set_bridge_error(&self, message: Option<String>) {
        *self.bridge_error.lock() = message;
    }

    pub fn bridge_error(&self) -> Option<String> {
        self.bridge_error.lock().clone()
    }

    /// Record the hosting FFI shim's C-ABI version (liborender only; Rust-linked
    /// hosts never call this).
    pub fn set_host_abi(&self, major: u32, minor: u32) {
        *self.host_abi.lock() = Some((major, minor));
    }

    pub fn host_abi(&self) -> Option<(u32, u32)> {
        *self.host_abi.lock()
    }

    pub fn active_topology(&self) -> Arc<RenderTopology> {
        self.topology.load_full()
    }

    pub fn active_layout(&self) -> SpeakerLayout {
        self.active_topology().speaker_layout.clone()
    }

    pub fn editable_layout(&self) -> SpeakerLayout {
        self.editable_layout.lock().clone()
    }

    pub fn with_editable_layout<R>(&self, f: impl FnOnce(&mut SpeakerLayout) -> R) -> R {
        let mut layout = self.editable_layout.lock();
        f(&mut layout)
    }

    pub fn publish_topology(&self, topology: RenderTopology) {
        self.topology.store(Arc::new(topology));
    }

    pub fn backend_rebuild_params(&self) -> Option<BackendRebuildParams> {
        *self.backend_rebuild_params.read()
    }

    pub fn set_backend_rebuild_params(&self, params: Option<BackendRebuildParams>) {
        *self.backend_rebuild_params.write() = params;
    }

    pub fn mark_object_params_dirty(&self) {
        self.object_params_generation
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn mark_speaker_params_dirty(&self) {
        self.speaker_params_generation
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Signal that live state changed and should be re-broadcast to clients.
    pub fn bump_live_state(&self) {
        self.live_state_generation.fetch_add(1, Ordering::Relaxed);
    }

    pub fn live_state_generation(&self) -> u64 {
        self.live_state_generation.load(Ordering::Relaxed)
    }

    /// Bump the geometry generation: the next recompute will rebuild the gain
    /// models from scratch (used for changes that alter triangulation / metrics).
    pub fn bump_geometry_generation(&self) {
        self.geometry_generation.fetch_add(1, Ordering::Relaxed);
    }

    pub fn geometry_generation(&self) -> u64 {
        self.geometry_generation.load(Ordering::Relaxed)
    }

    /// Bump the options epoch: a `REPLAN`-flagged live option changed, so the
    /// synthesized-object plan signatures must invalidate (see [`crate::options`]).
    pub fn bump_options_epoch(&self) {
        self.options_epoch.fetch_add(1, Ordering::Relaxed);
    }

    pub fn options_epoch(&self) -> u64 {
        self.options_epoch.load(Ordering::Relaxed)
    }

    /// Flag that output clipping was detected this frame on `speaker_idx`
    /// (lock-free, audio-thread safe).
    pub fn note_clip(&self, speaker_idx: usize) {
        self.clip_pending
            .store(speaker_idx as i32, Ordering::Relaxed);
    }

    /// Atomically read and clear the clip flag. Returns `Some(speaker_idx)` if
    /// clipping was flagged since the last call, else `None`.
    pub fn take_clip_pending(&self) -> Option<usize> {
        let idx = self.clip_pending.swap(-1, Ordering::Relaxed);
        (idx >= 0).then_some(idx as usize)
    }

    pub fn prepare_topology_rebuild(&self) -> Option<TopologyBuildPlan> {
        let layout = self.editable_layout();
        self.prepare_topology_rebuild_for_layout(layout)
    }

    pub fn prepare_topology_rebuild_for_layout(
        &self,
        layout: SpeakerLayout,
    ) -> Option<TopologyBuildPlan> {
        let live = self.live.read();
        let backend_rebuild_params = self.backend_rebuild_params();
        let evaluation_build_config = evaluation_build_config_from_live(
            &live,
            rebuild_params_allow_negative_z(backend_rebuild_params),
        );
        let geometry_generation = self.geometry_generation();
        let registry = self.backend_registry.read();
        // File-kind handles are resolved to absolute renderer paths here too, so
        // the backend's `build_plan` opens a real path (mirrors `available_backends`).
        let backend_params = self.resolved_backend_params(&registry);
        prepare_topology_build_plan(
            &registry,
            layout,
            &live,
            backend_rebuild_params,
            &backend_params,
            evaluation_build_config,
        )
        .map(|mut plan| {
            plan.geometry_generation = geometry_generation;
            plan
        })
    }

    /// Build a band-aware speaker gain table and serialize it for transfer.
    ///
    /// For each crossover band (`compute_bands`), builds the band-restricted VBAP
    /// topology and samples it over the SAME cartesian grid as the full table,
    /// scattering band-local gains into full speaker-index order. Layouts with no
    /// crossover yield a single band (= the full layout). The result is cached raw
    /// and serialized per speaker for transfer (see [`crate::band_gaintable`]).
    pub fn build_band_gaintable_full(
        &self,
    ) -> anyhow::Result<crate::band_gaintable::BandGaintableFull> {
        use rayon::prelude::*;

        let topology = self.active_topology();
        let layout = topology.speaker_layout.clone();
        let speaker_count = layout.speakers.len();

        // Same cartesian grid the full gain table uses.
        let (x_positions, y_positions, z_positions, template) = {
            let live = self.live.read();
            let rebuild_params = self.backend_rebuild_params();
            let config = evaluation_build_config_from_live(
                &live,
                rebuild_params_allow_negative_z(rebuild_params),
            );
            (
                crate::render_backend::evenly_spaced_axis(
                    config.cartesian.x_size.max(2),
                    -1.0,
                    1.0,
                ),
                crate::render_backend::evenly_spaced_axis(
                    config.cartesian.y_size.max(2),
                    -1.0,
                    1.0,
                ),
                crate::render_backend::cartesian_z_axis(
                    config.cartesian.z_size.max(2),
                    config.cartesian.z_neg_size,
                ),
                config.request_template,
            )
        };
        let (nx, ny, nz) = (x_positions.len(), y_positions.len(), z_positions.len());
        let cell_count = nx * ny * nz;

        let bands = crate::crossover::compute_bands(&layout);

        let mut band_meta: Vec<(f32, f32)> = Vec::with_capacity(bands.len());
        let mut band_gains_all: Vec<Vec<f32>> = Vec::with_capacity(bands.len());
        for band in &bands {
            band_meta.push((band.low_hz, band.high_hz));
            let indices = band.speaker_indices.clone();
            let n = indices.len();
            let mut gains = vec![0.0f32; cell_count * speaker_count];
            if n >= 3 {
                let band_layout = crate::speaker_layout::SpeakerLayout {
                    radius_m: layout.radius_m,
                    speakers: indices
                        .iter()
                        .map(|&i| layout.speakers[i].clone())
                        .collect(),
                };
                let band_topology = self
                    .prepare_topology_rebuild_for_layout(band_layout)
                    .ok_or_else(|| anyhow::anyhow!("failed to prepare band topology"))?
                    .build_topology()?;
                let per_cell: Vec<crate::spatial_vbap::Gains> = (0..cell_count)
                    .into_par_iter()
                    .map(|idx| {
                        let xi = idx % nx;
                        let yi = (idx / nx) % ny;
                        let zi = idx / (nx * ny);
                        let mut req = template;
                        req.adm_position = [
                            x_positions[xi] as f64,
                            y_positions[yi] as f64,
                            z_positions[zi] as f64,
                        ];
                        band_topology.backend.compute_gains(&req).gains
                    })
                    .collect();
                for (idx, cell) in per_cell.iter().enumerate() {
                    let base = idx * speaker_count;
                    for (gi, &g) in cell.iter().enumerate() {
                        gains[base + indices[gi]] = g;
                    }
                }
            } else if n > 0 {
                // <3 speakers: no VBAP solution — uniform fill for the band's speakers.
                let v = 1.0 / (n as f32).sqrt();
                for idx in 0..cell_count {
                    let base = idx * speaker_count;
                    for &gi in &indices {
                        gains[base + gi] = v;
                    }
                }
            }
            band_gains_all.push(gains);
        }

        let band_fields = band_meta
            .into_iter()
            .zip(band_gains_all)
            .map(
                |((low_hz, high_hz), gains)| crate::band_gaintable::BandField {
                    low_hz,
                    high_hz,
                    gains,
                },
            )
            .collect();
        // Speaker positions ride along for the centroid-jump derived field —
        // already in the same normalised [-1, 1] cube as the grid.
        let speaker_positions = layout.speakers.iter().map(|s| [s.x, s.y, s.z]).collect();
        Ok(crate::band_gaintable::BandGaintableFull {
            x_positions,
            y_positions,
            z_positions,
            speaker_count,
            speaker_positions,
            bands: band_fields,
        })
    }

    /// Mark live params as dirty (changed since last save) and return the new state.
    pub fn mark_dirty(&self) {
        self.config_dirty.store(true, Ordering::Relaxed);
    }

    /// Mark live params as clean (just saved) and return the new state.
    pub fn mark_clean(&self) {
        self.config_dirty.store(false, Ordering::Relaxed);
    }

    pub fn set_input_path(&self, input_path: Option<String>) {
        *self.input_path.lock() = input_path;
    }

    pub fn input_path(&self) -> Option<String> {
        self.input_path.lock().clone()
    }

    pub fn set_bridge_path(&self, bridge_path: Option<PathBuf>) {
        *self.bridge_path.lock() = bridge_path;
    }

    pub fn bridge_path(&self) -> Option<PathBuf> {
        self.bridge_path.lock().clone()
    }

    pub fn set_bridge_supported_drc_modes(&self, modes: Vec<String>) {
        *self.bridge_supported_drc_modes.lock() = modes;
    }

    pub fn bridge_supported_drc_modes(&self) -> Vec<String> {
        self.bridge_supported_drc_modes.lock().clone()
    }

    pub fn set_requested_ramp_mode(&self, mode: RampMode) {
        *self.requested_ramp_mode.lock() = mode;
    }

    pub fn requested_ramp_mode(&self) -> RampMode {
        *self.requested_ramp_mode.lock()
    }
}
