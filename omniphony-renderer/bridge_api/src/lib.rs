#![allow(non_local_definitions)]

pub mod labels;

use abi_stable::{
    StableAbi, declare_root_module_statics,
    library::RootModule,
    package_version_strings, sabi_trait,
    sabi_types::VersionStrings,
    std_types::{RBox, ROption, RSlice, RStr, RString, RVec},
};

/// Transport wrapper used for the incoming bytes passed to a bridge.
#[repr(u8)]
#[derive(StableAbi, Clone, Copy, Debug, PartialEq, Eq)]
pub enum RInputTransport {
    Raw = 0,
    Iec61937 = 1,
}

/// ABI-stable log level used by bridges to forward diagnostics to the host.
#[repr(u8)]
#[derive(StableAbi, Clone, Copy, Debug, PartialEq, Eq)]
pub enum RLogLevel {
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
    Trace = 5,
}

/// Host-installed callback used by bridges to inject logs into the renderer pipeline.
pub type BridgeHostLogSink = extern "C" fn(level: RLogLevel, target: RStr<'_>, message: RStr<'_>);

/// ABI-stable spatial event (single object update for one frame).
#[repr(C)]
#[derive(StableAbi, Clone, Debug)]
pub struct REvent {
    pub id: u32,
    pub sample_pos: u64,
    /// True when `pos` contains valid 3-D coordinates. A `false` event is a
    /// gain/ramp-only update for its object.
    pub has_pos: bool,
    /// Position payload, interpreted according to [`FormatBridge::coordinate_format`]:
    /// - Cartesian: `[x, y, z]` (ADM convention)
    /// - Polar: `[azimuth_deg, elevation_deg, distance]` with:
    ///   - `azimuth_deg`: 0°=front, -90°=left, +90°=right (wrapped in [-180°, +180°])
    ///   - `elevation_deg`: -90°=down, +90°=up
    ///   - `distance`: non-negative
    pub pos: [f64; 3],
    pub gain_db: i8,
    /// Object spatial extent per axis (width, depth, height), each normalised to
    /// `[0.0, 1.0]` per ETSI TS 103 420 §5.2.2 `object_size`.
    /// `[0.0, 0.0, 0.0]` denotes a point source. The renderer is responsible
    /// for reducing this triplet to a scalar spread according to its policy.
    pub size: [f64; 3],
    pub ramp_duration: u32,
}

/// Sparse object-name update keyed by object id (same id space as `REvent.id`
/// and `RObjectChannel.id`). Fixed channels are named by their label instead.
#[repr(C)]
#[derive(StableAbi, Clone)]
pub struct RNameUpdate {
    pub id: u32,
    pub name: RString,
}

/// ABI-stable channel label (speaker position), encoded as u8.
#[repr(u8)]
#[derive(StableAbi, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RChannelLabel {
    L = 0,
    R = 1,
    C = 2,
    LFE = 3,
    Ls = 4,
    Rs = 5,
    Tfl = 6,
    Tfr = 7,
    Tsl = 8,
    Tsr = 9,
    Tbl = 10,
    Tbr = 11,
    Lsc = 12,
    Rsc = 13,
    Lb = 14,
    Rb = 15,
    Cb = 16,
    Tc = 17,
    Lsd = 18,
    Rsd = 19,
    Lw = 20,
    Rw = 21,
    Tfc = 22,
    LFE2 = 23,
    /// The channel carries dynamic-object audio; its position comes from the
    /// metadata events of the object bound to it via `RObjectChannel`.
    Object = 24,
    Unknown = 255,
}

/// Sparse declaration binding a dynamic object to the PCM channel that
/// carries its audio. Object ids are opaque, bridge-chosen and stable for the
/// lifetime of the object; this mapping is the only link between an id and
/// its channel. See `docs/channel-object-contract.md` ("ABI").
#[repr(C)]
#[derive(StableAbi, Clone, Copy, Debug, PartialEq, Eq)]
pub struct RObjectChannel {
    pub id: u32,
    pub channel: u32,
}

/// Metadata-driven gain automation for one fixed channel (e.g. OAMD bed
/// gains). Applied with the frame's `ramp_duration`.
#[repr(C)]
#[derive(StableAbi, Clone, Copy, Debug, PartialEq, Eq)]
pub struct RChannelGain {
    pub channel: u32,
    pub gain_db: i8,
}

/// Spatial metadata for one payload within a decoded frame.
///
/// Describes dynamic objects only; fixed channels are fully described by
/// [`RDecodedFrame::channel_labels`] (channels carrying object audio are
/// labeled [`RChannelLabel::Object`]). Emitted only by formats that carry
/// dynamic objects — a fixed-only presentation sends no metadata frames.
#[repr(C)]
#[derive(StableAbi)]
pub struct RMetadataFrame {
    /// Position/gain/size events, keyed by object id.
    pub events: RVec<REvent>,
    /// Sparse object↔channel declaration: emitted on the first metadata
    /// frame, on any change, and after [`FormatBridge::reset`]. Consumers
    /// cache it unconditionally.
    pub object_channels: RVec<RObjectChannel>,
    /// Gain automation for fixed channels (empty when the format has none).
    pub channel_gains: RVec<RChannelGain>,
    /// Sparse object-name updates for this frame (same emission rules as
    /// `object_channels`).
    pub name_updates: RVec<RNameUpdate>,
    /// Base sample position for this metadata (= decoded_samples at frame start,
    /// without evo_sample_offset). Used for OSC timestamping.
    pub sample_pos: u64,
    /// Ramp duration in samples.
    pub ramp_duration: u32,
}

/// A fully decoded audio frame: interleaved PCM + metadata.
#[repr(C)]
#[derive(StableAbi)]
pub struct RDecodedFrame {
    pub sampling_frequency: u32,
    pub sample_count: u32,
    pub channel_count: u32,
    /// PCM samples, interleaved: `[s0c0, s0c1, …, s0c(N-1), s1c0, …]`.
    pub pcm: RVec<i32>,
    /// One label per channel (length == channel_count).
    pub channel_labels: RVec<RChannelLabel>,
    /// One entry per metadata payload found in this access unit.
    pub metadata: RVec<RMetadataFrame>,
    /// Target Dynamic Range Control gain (linear, 1.0 = 0dB).
    pub drc_gain: f32,
    /// DRC ramp duration in samples.
    pub drc_ramp_duration: u32,
    /// Dialogue normalisation level in dBFS (updated from major sync).
    pub dialogue_level: ROption<i8>,
    /// True when the stream format changed mid-stream (new segment boundary).
    pub is_new_segment: bool,
}

/// Result returned by [`FormatBridge::push_packet`].
#[repr(C)]
#[derive(StableAbi)]
pub struct RPushResult {
    /// Decoded frames produced from this chunk (may be empty).
    pub frames: RVec<RDecodedFrame>,
    /// Non-empty if a fatal error occurred (strict mode only).
    pub error_message: RString,
    /// True when the internal pipeline was reset (seek/sync loss recovery).
    pub did_reset: bool,
}

/// Coordinate representation used in [`REvent::pos`].
#[repr(u8)]
#[derive(StableAbi, Clone, Copy, Debug, PartialEq, Eq)]
pub enum RCoordinateFormat {
    Cartesian = 0,
    Polar = 1,
}

/// Default Cartesian VBAP grid dimensions suggested by the loaded bridge.
#[repr(C)]
#[derive(StableAbi, Clone, Copy, Debug, PartialEq, Eq)]
pub struct RVbapCartesianDefaults {
    pub x_size: u32,
    pub y_size: u32,
    pub z_size: u32,
    pub allow_negative_z: bool,
}

/// Preferred VBAP table mode suggested by the loaded bridge.
#[repr(u8)]
#[derive(StableAbi, Clone, Copy, Debug, PartialEq, Eq)]
pub enum RVbapTableMode {
    Polar = 0,
    Cartesian = 1,
}

/// Format bridge trait — implemented by each plugin `.so`.
///
/// The bridge owns the full decode pipeline internally.
/// Call [`push_packet`] for each incoming chunk or packet; the bridge handles
/// format-specific validation, parsing, and metadata extraction.
#[sabi_trait]
pub trait FormatBridge: Send + Sync + 'static {
    /// Push one input unit and get back any fully decoded frames that became
    /// available.
    ///
    /// For [`RInputTransport::Raw`], `data_type` must be zero.
    /// For [`RInputTransport::Iec61937`], `data` is the extracted IEC 61937
    /// payload and `data_type` is the transport data-type byte from the packet
    /// header. The bridge is responsible for validating whether it supports
    /// that payload type.
    fn push_packet(
        &mut self,
        data: RSlice<'_, u8>,
        transport: RInputTransport,
        data_type: u8,
    ) -> RPushResult;

    /// Reset the internal pipeline (call after a seek or stream discontinuity).
    fn reset(&mut self);

    /// `true` once at least one frame has been successfully decoded.
    fn is_ready(&self) -> bool;

    /// `true` while the current presentation carries dynamic objects.
    ///
    /// A live, observable fact about the stream: it may flip in either
    /// direction mid-stream (e.g. an extension substream appearing after a
    /// few frames, or disappearing) and must not be latched by callers.
    /// Before the first decoded frame it reports the best container-level
    /// guess. See `docs/channel-object-contract.md`.
    fn has_objects(&self) -> bool;

    /// Set a bridge-specific configuration option.
    ///
    /// Must be called before the first [`push_packet`].
    /// Returns `true` if the key was recognised, `false` otherwise.
    /// Keys and their semantics are defined by each bridge implementation.
    fn configure(&mut self, key: RStr<'_>, value: RStr<'_>) -> bool;

    /// Declares how this bridge encodes [`REvent::pos`].
    ///
    /// Bridges should return a stable value for the lifetime of the instance.
    fn coordinate_format(&self) -> RCoordinateFormat;

    /// Default Cartesian VBAP grid dimensions for this bridge.
    ///
    /// These defaults are consumed by hosts when Cartesian VBAP table mode is enabled
    /// and explicit CLI/config values are not provided.
    fn vbap_cartesian_defaults(&self) -> RVbapCartesianDefaults;

    /// Preferred VBAP table mode for this bridge when the host did not receive an
    /// explicit CLI/config override.
    fn preferred_vbap_table_mode(&self) -> RVbapTableMode;

    /// Returns a list of supported DRC modes for this bridge.
    fn supported_drc_modes(&self) -> RVec<RString>;

    /// Selects a DRC mode for this bridge.
    ///
    /// Returns `true` if the mode was successfully applied.
    fn set_drc_mode(&mut self, mode: RStr<'_>) -> bool;
}

/// Owned, heap-allocated bridge trait object.
pub type FormatBridgeBox = FormatBridge_TO<RBox<()>>;

/// Root module exported by each plugin `.so`.
#[repr(C)]
#[derive(StableAbi)]
#[sabi(kind(Prefix(prefix_ref = BridgeLibRef)))]
pub struct BridgeLib {
    /// Create a fresh bridge instance.
    ///
    /// - `strict`: when true, parse/decode errors set `error_message` instead of
    ///   silently resetting.
    ///
    /// Format-specific options (e.g. substream selection) are set afterwards
    /// via [`FormatBridge::configure`] before the first [`FormatBridge::push_packet`].
    #[sabi(last_prefix_field)]
    pub new_bridge: extern "C" fn(strict: bool) -> FormatBridgeBox,
    /// Install a host log sink for bridge diagnostics.
    ///
    /// New hosts should register this immediately after loading the bridge.
    /// Older bridges may not expose it; in that case bridge diagnostics fall
    /// back to stderr.
    pub set_host_log_sink: extern "C" fn(usize),
}

impl RootModule for BridgeLibRef {
    declare_root_module_statics! {BridgeLibRef}
    const BASE_NAME: &'static str = "format_bridge";
    const NAME: &'static str = "format_bridge";
    const VERSION_STRINGS: VersionStrings = package_version_strings!();
}
