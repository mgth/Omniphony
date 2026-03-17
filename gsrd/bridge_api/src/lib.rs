#![allow(non_local_definitions)]

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

/// ABI-stable spatial event (single object update for one frame).
#[repr(C)]
#[derive(StableAbi, Clone, Debug)]
pub struct REvent {
    pub id: u32,
    pub sample_pos: u64,
    /// True when `pos` contains valid 3-D coordinates (bed channels have no position).
    pub has_pos: bool,
    /// Position payload, interpreted according to [`FormatBridge::coordinate_format`]:
    /// - Cartesian: `[x, y, z]` (ADM convention)
    /// - Polar: `[azimuth_deg, elevation_deg, distance]` with:
    ///   - `azimuth_deg`: 0°=front, -90°=left, +90°=right (wrapped in [-180°, +180°])
    ///   - `elevation_deg`: -90°=down, +90°=up
    ///   - `distance`: non-negative
    pub pos: [f64; 3],
    pub gain_db: i8,
    /// Source spread in degrees.
    pub spread: f64,
    pub ramp_duration: u32,
}

/// Sparse object-name update keyed by object ID (same ID space as `REvent.id`).
#[repr(C)]
#[derive(StableAbi, Clone)]
pub struct RNameUpdate {
    pub id: u32,
    pub name: RString,
}

/// ABI-stable channel label (speaker position), encoded as u8.
#[repr(u8)]
#[derive(StableAbi, Clone, Copy, Debug, PartialEq)]
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
    Unknown = 255,
}

/// Spatial metadata for one payload within a decoded frame.
#[repr(C)]
#[derive(StableAbi)]
pub struct RMetadataFrame {
    /// Spatial events for the renderer (one per object).
    pub events: RVec<REvent>,
    /// Format-provided bed channel IDs (in the same ID space as `REvent.id`).
    pub bed_indices: RVec<usize>,
    /// Sparse object-name updates for this frame.
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

    /// `true` if the current presentation may contain spatial objects.
    ///
    /// Must be called after [`configure`] and before [`push_packet`].
    /// Drives output-format decisions in the host (e.g. whether to force CAF).
    fn is_spatial(&self) -> bool;

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
    pub new_bridge: extern "C" fn(strict: bool) -> FormatBridgeBox,
}

impl RootModule for BridgeLibRef {
    declare_root_module_statics! {BridgeLibRef}
    const BASE_NAME: &'static str = "format_bridge";
    const NAME: &'static str = "format_bridge";
    const VERSION_STRINGS: VersionStrings = package_version_strings!();
}
