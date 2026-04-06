//! Spatial audio renderer using VBAP
//!
//! This module handles rendering spatial object audio to speaker channels
//! using Vector-Based Amplitude Panning (VBAP).
//!
//! # Architecture
//!
//! 1. **Initialization**: Create `SpatialRenderer` with speaker layout
//! 2. **Per-Frame Rendering**: For each decoded audio frame with spatial metadata:
//!    - Extract object positions from metadata
//!    - Convert ADM coordinates to spherical (az/el)
//!    - Get VBAP gains for each object
//!    - Mix object audio into speaker channels
//! 3. **Output**: Return speaker-rendered audio samples
//!
//! # Example
//!
//! ```no_run
//! use omniphony_renderer::spatial_renderer::SpatialRenderer;
//! use omniphony_renderer::speaker_layout::SpeakerLayout;
//! use omniphony_renderer::spatial_vbap::{DistanceModel, VbapTableMode};
//!
//! // Load speaker layout
//! let layout = SpeakerLayout::preset("7.1.4")?;
//!
//! // Create renderer with VBAP configuration
//! let renderer = SpatialRenderer::new(
//!     layout,
//!     48000,                 // sample rate (Hz)
//!     1,                     // azimuth resolution
//!     1,                     // elevation resolution
//!     0.25,                  // spread resolution (0.0 = single table, >0 = dynamic spread)
//!     2.0,                   // polar distance max
//!     VbapTableMode::Polar,  // precomputed table mode
//!     true,                  // allow_negative_z
//!     DistanceModel::Linear, // distance attenuation model
//!     false,                 // spread_from_distance (false = use spread_min/spread_max)
//!     1.0,                   // spread_distance_range (distance where spread reaches 0)
//!     1.0,                   // spread_distance_curve (1.0 = linear, 2.0 = quadratic)
//!     0.0,                   // spread_min
//!     1.0,                   // spread_max
//!     false,                 // log_object_positions
//!     [1.0, 2.0, 0.5],       // room_ratio [width, length, height]
//!     2.0,                   // room_ratio_rear
//!     0.5,                   // room_ratio_center_blend
//!     0.0,                   // master_gain_db
//!     false,                 // auto_gain
//!     false,                 // use_loudness
//!     false,                 // distance_diffuse
//!     1.0,                   // distance_diffuse_threshold
//!     1.0,                   // distance_diffuse_curve
//!     omniphony_renderer::live_params::PreferredEvaluationMode::PrecomputedPolar, // bridge preferred mode
//!     omniphony_renderer::live_params::LiveEvaluationMode::PrecomputedPolar,      // initial live selection
//!     31,                    // cartesian default x size
//!     31,                    // cartesian default y size
//!     15,                    // cartesian default z size
//! )?;
//!
//! // Render objects for a frame (in decode loop)
//! let speaker_samples = renderer.render_frame(
//!     &decoded_access_unit,
//!     &spatial_metadata,
//!     bed_channel_count,
//! )?;
//! ```

use crate::live_params::{
    CartesianEvaluationParams, EvaluationLiveParams, LiveEvaluationMode, LiveParams,
    LiveVbapTableMode, PolarEvaluationParams, PreferredEvaluationMode, RampMode, RenderTopology,
    RendererControl,
};
use crate::render_backend::{
    EffectiveEvaluationMode, GainModelInstance, RenderBackendKind, RenderRequest, VbapBackend,
    build_prepared_render_engine,
};
use crate::spatial_vbap::VbapTableMode;
use crate::spatial_vbap::{DistanceModel, Gains, VbapPanner, adm_to_spherical};
use crate::speaker_layout::SpeakerLayout;
use anyhow::Result;
use std::sync::Arc;

/// Output of a single rendered frame.
pub struct RenderedFrame {
    /// Interleaved speaker audio: `[sample0_spk0, sample0_spk1, ..., sample1_spk0, ...]`.
    pub samples: Vec<f32>,
    /// VBAP gains at the final sample for each rendered object channel.
    /// `(channel_idx, gains)` — `gains[speaker_idx]` is the gain applied to that speaker.
    /// Ordered by `channel_idx`. Empty if no objects were spatialized this frame.
    pub object_gains: Vec<(usize, Gains)>,
}

/// Format-agnostic spatial metadata for one audio channel (bed or object).
///
/// The renderer accepts this type instead of any format-specific event type
/// (e.g. `damf::Event`), so that any spatial audio source (ADM, …)
/// can feed the renderer without a format dependency.
///
/// All fields except `channel_idx` and `is_bed` are `Option` — `None` means
/// "keep the previously cached value for this channel".
#[derive(Debug, Clone)]
pub struct SpatialChannelEvent {
    /// PCM channel index in the interleaved input buffer.
    pub channel_idx: usize,
    /// `true` → direct speaker routing (bed); `false` → VBAP spatialization (object).
    pub is_bed: bool,
    /// Channel gain in dB (`None` = unchanged).
    pub gain_db: Option<i8>,
    /// Ramp duration in audio frames (`None` = unchanged).
    pub ramp_length: Option<u32>,
    /// Object spread metadata in [0.0, 1.0] (`None` = unchanged).
    pub spread: Option<f32>,
    /// Target position in ADM Cartesian coordinates [x, y, z] (`None` = unchanged).
    /// x ∈ [-1, 1] left/right · y ∈ [-1, 1] back/front · z ∈ [-1, 1] floor/ceiling.
    pub position: Option<[f64; 3]>,
    /// Sample index within the access unit where this event takes effect.
    pub sample_pos: Option<u64>,
}

/// Grouping of spatial properties (position and spread)
#[derive(Debug, Clone, Copy, PartialEq)]
struct SpatialAttributes {
    pub position: [f64; 3],
    pub spread: f32,
}

impl Default for SpatialAttributes {
    fn default() -> Self {
        Self {
            position: [0.0; 3],
            spread: 0.0,
        }
    }
}

impl SpatialAttributes {
    fn interpolate(&self, target: &Self, fraction: f64) -> Self {
        let ts = 1.0 - fraction;
        let tt = fraction;
        Self {
            position: [
                self.position[0] * ts + target.position[0] * tt,
                self.position[1] * ts + target.position[1] * tt,
                self.position[2] * ts + target.position[2] * tt,
            ],
            spread: (self.spread as f64 * ts + target.spread as f64 * tt) as f32,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct RenderGainCache {
    topology_identity: usize,
    position_bits: [u64; 3],
    room_ratio_bits: [u32; 3],
    room_ratio_rear_bits: u32,
    room_ratio_lower_bits: u32,
    room_ratio_center_blend_bits: u32,
    spread_min_bits: u32,
    spread_max_bits: u32,
    spread_distance_range_bits: u32,
    spread_distance_curve_bits: u32,
    distance_diffuse_threshold_bits: u32,
    distance_diffuse_curve_bits: u32,
    spread_from_distance: bool,
    use_distance_diffuse: bool,
    distance_model: crate::spatial_vbap::DistanceModel,
    valid: bool,
    gains: Vec<f32>,
}

impl RenderGainCache {
    fn matches(
        &self,
        topology_identity: usize,
        rendering_position: [f64; 3],
        live: &LiveSnapshot<'_>,
        distance_model: crate::spatial_vbap::DistanceModel,
    ) -> bool {
        self.valid
            && self.topology_identity == topology_identity
            && self.position_bits == rendering_position.map(f64::to_bits)
            && self.room_ratio_bits == live.room_ratio.map(f32::to_bits)
            && self.room_ratio_rear_bits == live.room_ratio_rear.to_bits()
            && self.room_ratio_lower_bits == live.room_ratio_lower.to_bits()
            && self.room_ratio_center_blend_bits == live.room_ratio_center_blend.to_bits()
            && self.spread_min_bits == live.spread_min.to_bits()
            && self.spread_max_bits == live.spread_max.to_bits()
            && self.spread_distance_range_bits == live.spread_distance_range.to_bits()
            && self.spread_distance_curve_bits == live.spread_distance_curve.to_bits()
            && self.distance_diffuse_threshold_bits == live.distance_diffuse_threshold.to_bits()
            && self.distance_diffuse_curve_bits == live.distance_diffuse_curve.to_bits()
            && self.spread_from_distance == live.spread_from_distance
            && self.use_distance_diffuse == live.use_distance_diffuse
            && self.distance_model == distance_model
    }

    fn update_signature(
        &mut self,
        topology_identity: usize,
        rendering_position: [f64; 3],
        live: &LiveSnapshot<'_>,
        distance_model: crate::spatial_vbap::DistanceModel,
    ) {
        self.topology_identity = topology_identity;
        self.position_bits = rendering_position.map(f64::to_bits);
        self.room_ratio_bits = live.room_ratio.map(f32::to_bits);
        self.room_ratio_rear_bits = live.room_ratio_rear.to_bits();
        self.room_ratio_lower_bits = live.room_ratio_lower.to_bits();
        self.room_ratio_center_blend_bits = live.room_ratio_center_blend.to_bits();
        self.spread_min_bits = live.spread_min.to_bits();
        self.spread_max_bits = live.spread_max.to_bits();
        self.spread_distance_range_bits = live.spread_distance_range.to_bits();
        self.spread_distance_curve_bits = live.spread_distance_curve.to_bits();
        self.distance_diffuse_threshold_bits = live.distance_diffuse_threshold.to_bits();
        self.distance_diffuse_curve_bits = live.distance_diffuse_curve.to_bits();
        self.spread_from_distance = live.spread_from_distance;
        self.use_distance_diffuse = live.use_distance_diffuse;
        self.distance_model = distance_model;
        self.valid = true;
    }
}

/// Per-channel state for movement detection and gain ramping
#[derive(Debug, Clone)]
struct ChannelState {
    /// Gain in dB
    gain_db: i8,

    /// Current spatial attributes (position, spread)
    current: SpatialAttributes,

    /// Target spatial attributes for ramping
    target: SpatialAttributes,

    /// Ramp duration in samples from metadata.
    ramp_length: u64,
    /// Remaining ramp units. Interpreted according to the active ramp mode.
    remaining_ramp_units: Option<u64>,

    target_sample_index: Option<u64>,
    render_gain_cache: RenderGainCache,
}

impl Default for ChannelState {
    fn default() -> Self {
        Self {
            gain_db: -128, // -inf dB (muted)
            current: SpatialAttributes::default(),
            target: SpatialAttributes::default(),
            ramp_length: 0,
            remaining_ramp_units: None,
            target_sample_index: None,
            render_gain_cache: RenderGainCache::default(),
        }
    }
}

trait RampProcessor {
    fn process_ramp(&mut self, step: usize) -> RampStatus;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RampStatus {
    Ramping,
    Finished,
    Idle,
}

impl RampProcessor for ChannelState {
    fn process_ramp(&mut self, step: usize) -> RampStatus {
        if let Some(remaining_units) = self.remaining_ramp_units {
            let step_u64 = step as u64;

            if remaining_units > step_u64 {
                let fraction = step_u64 as f64 / remaining_units as f64;

                // Interpolate current state towards target
                self.current = self.current.interpolate(&self.target, fraction);

                self.remaining_ramp_units = Some(remaining_units - step_u64);
                RampStatus::Ramping
            } else {
                self.current = self.target;
                self.remaining_ramp_units = None;
                RampStatus::Finished
            }
        } else {
            RampStatus::Idle
        }
    }
}

#[inline]
fn map_depth_with_room_ratios(
    depth: f32,
    front_ratio: f32,
    rear_ratio: f32,
    center_blend: f32,
) -> f32 {
    let d = depth.clamp(-1.0, 1.0);
    let blend = center_blend.clamp(0.0, 1.0);
    let center_ratio = rear_ratio + (front_ratio - rear_ratio) * blend;
    if d >= 0.0 {
        let t = d;
        let a = center_ratio - front_ratio;
        let b = 2.0 * (front_ratio - center_ratio);
        a * t * t * t + b * t * t + center_ratio * t
    } else {
        let t = -d;
        let a = center_ratio - rear_ratio;
        let b = 2.0 * (rear_ratio - center_ratio);
        -(a * t * t * t + b * t * t + center_ratio * t)
    }
}

/// Snapshot of `LiveParams` taken at the start of each render frame.
///
/// Holding this snapshot (rather than keeping the `RwLock` locked) allows the
/// OSC listener to write new values at any time without blocking the render
/// thread between samples.
struct LiveSnapshot<'a> {
    master_gain: f32,
    object_params: &'a [crate::live_params::ObjectLiveParams],
    spread_min: f32,
    spread_max: f32,
    spread_from_distance: bool,
    spread_distance_range: f32,
    spread_distance_curve: f32,
    ramp_mode: RampMode,
    use_loudness: bool,
    speaker_params: &'a [crate::live_params::SpeakerLiveParams],
    room_ratio: [f32; 3],
    room_ratio_rear: f32,
    room_ratio_lower: f32,
    room_ratio_center_blend: f32,
    use_distance_diffuse: bool,
    distance_diffuse_threshold: f32,
    distance_diffuse_curve: f32,
}

/// Spatial audio renderer using VBAP
pub struct SpatialRenderer {
    /// Number of output speakers (total, including non-spatialized like LFE)
    num_speakers: usize,

    /// Spread resolution for multi-table VBAP (0.0 = single table)
    spread_resolution: f32,

    /// Bed channel IDs in PCM order (e.g. [3, 0, 1, 2, ...]).
    /// Updated when format metadata changes and read lock-free in the audio thread.
    bed_indices: arc_swap::ArcSwap<Vec<usize>>,

    /// Flag for first render (for detailed logging)
    first_render: std::sync::atomic::AtomicBool,

    /// Frame counter for periodic logging
    frame_counter: std::sync::atomic::AtomicU64,

    /// Per-channel state (movement detection + gain ramping)
    channel_states: std::sync::Mutex<std::collections::HashMap<usize, ChannelState>>,

    /// Sample rate for ramp time calculations
    sample_rate: u32,

    /// Distance attenuation model
    distance_model: DistanceModel,

    /// Enable detailed logging of object positions (ramping and movement)
    log_object_positions: bool,

    /// Dialog normalization gain in linear (1.0 = no normalization)
    /// Set dynamically when dialogue_level is received from the stream
    loudness_gain: std::sync::atomic::AtomicU32,

    /// Enable automatic gain adjustment to prevent clipping
    auto_gain: bool,

    /// Current auto-gain multiplier (adjusted dynamically when clipping detected)
    /// Stored as atomic for thread-safe updates
    current_auto_gain: std::sync::atomic::AtomicU32,

    /// Shared live parameters + speaker layout + pending VBAP swap.
    control: Arc<RendererControl>,

    /// Per-speaker gain scratch buffer — pre-allocated once, reused every frame.
    speaker_gains_buf: Vec<f32>,

    /// Scratch snapshot of live per-object params, indexed by input channel.
    object_params_buf: Vec<crate::live_params::ObjectLiveParams>,

    /// Scratch snapshot of live per-speaker params, indexed by output speaker.
    speaker_params_buf: Vec<crate::live_params::SpeakerLiveParams>,

    /// Last integrated generation for per-object live params.
    object_params_generation_seen: u64,

    /// Last integrated generation for per-speaker live params.
    speaker_params_generation_seen: u64,

    /// Scratch routing gains for bed channels.
    ///
    /// Keep this as a reusable full speaker-domain buffer instead of collapsing beds
    /// back to a hardcoded one-speaker fast path. Bed routing is expected to evolve
    /// beyond strict 1:1 mapping so we can simulate missing or non-standard speakers
    /// without changing the downstream mix model again.
    bed_routing_gains_buf: Vec<f32>,

    /// Per-speaker delay lines — one per speaker, fixed 100 ms capacity.
    /// Owned exclusively by the render thread; no locking required.
    delay_lines: Vec<crate::delay_line::DelayLine>,
}

impl SpatialRenderer {
    /// Create a new spatial renderer
    ///
    /// # Arguments
    ///
    /// * `speaker_layout` - Speaker configuration
    /// * `sample_rate` - Sample rate in Hz (for ramp timing)
    /// * `az_res_deg` - Azimuth resolution in degrees (1-10)
    /// * `el_res_deg` - Elevation resolution in degrees (1-10)
    /// * `spread_resolution` - Spread table resolution (0.0 = single table with spread=0, >0 = dynamic spread)
    /// * `distance_model` - Distance attenuation model
    /// * `spread_from_distance` - Calculate spread from distance instead of object spread metadata
    /// * `spread_distance_range` - Distance at which spread reaches 0.0
    /// * `spread_distance_curve` - Curve exponent for distance-based spread
    /// * `spread_min` - Minimum effective spread
    /// * `spread_max` - Maximum effective spread
    /// * `log_object_positions` - Enable detailed logging of object positions
    /// * `room_ratio` - Room proportions [width, length, height] for scaling ADM coordinates
    /// * `master_gain_db` - Master gain in dB (applied to final output)
    /// * `auto_gain` - Enable automatic gain reduction to prevent clipping
    /// * `use_loudness` - Apply loudness metadata correction gain from stream metadata
    /// * `distance_diffuse` - Enable distance-based antipodal diffuse blending
    /// * `distance_diffuse_threshold` - ADM distance at which blend reaches 100% direct
    /// * `distance_diffuse_curve` - Curve exponent for the blend weight
    ///
    /// **Note:** This method requires the `saf_vbap` feature to generate VBAP tables.
    /// Without saf_vbap, use `from_vbap()` to load pre-generated tables.
    #[cfg(feature = "saf_vbap")]
    pub fn new(
        speaker_layout: SpeakerLayout,
        sample_rate: u32,
        az_res_deg: i32,
        el_res_deg: i32,
        spread_resolution: f32,
        distance_max: f32,
        table_mode: VbapTableMode,
        allow_negative_z: bool,
        vbap_position_interpolation: bool,
        distance_model: DistanceModel,
        spread_from_distance: bool,
        spread_distance_range: f32,
        spread_distance_curve: f32,
        spread_min: f32,
        spread_max: f32,
        log_object_positions: bool,
        room_ratio: [f32; 3],
        room_ratio_rear: f32,
        room_ratio_lower: f32,
        room_ratio_center_blend: f32,
        master_gain_db: f32,
        auto_gain: bool,
        use_loudness: bool,
        distance_diffuse: bool,
        distance_diffuse_threshold: f32,
        distance_diffuse_curve: f32,
        preferred_evaluation_mode: PreferredEvaluationMode,
        initial_evaluation_mode: LiveEvaluationMode,
        cartesian_default_x_size: usize,
        cartesian_default_y_size: usize,
        cartesian_default_z_size: usize,
        cartesian_default_z_neg_size: usize,
    ) -> Result<Self> {
        let num_speakers = speaker_layout.num_speakers();
        let spatializable_positions = speaker_layout
            .spatializable_positions_for_room(
                room_ratio,
                room_ratio_rear,
                room_ratio_lower,
                room_ratio_center_blend,
            )
            .0;
        let num_vbap_speakers = spatializable_positions.len();

        let vbap = VbapPanner::new_with_mode(
            &spatializable_positions,
            az_res_deg,
            el_res_deg,
            0.0,
            table_mode,
        )
        .map_err(|e| anyhow::anyhow!("Failed to create VBAP panner: {}", e))?
        .with_negative_z(allow_negative_z)
        .with_position_interpolation(vbap_position_interpolation);
        let distance_step = if spread_resolution > 0.0 {
            spread_resolution
        } else {
            0.25
        };
        let vbap = vbap
            .precompute_effect_tables(
                distance_step,
                distance_max,
                spread_min.clamp(0.0, 1.0),
                spread_max.clamp(0.0, 1.0),
                distance_model,
                spread_from_distance,
                spread_distance_range,
                spread_distance_curve,
                distance_diffuse,
                distance_diffuse_threshold,
                distance_diffuse_curve,
                room_ratio,
                room_ratio_rear,
                room_ratio_lower,
                room_ratio_center_blend,
            )
            .map_err(|e| anyhow::anyhow!("Failed to precompute VBAP effect tables: {}", e))?;
        if !vbap.has_precomputed_effects() {
            return Err(anyhow::anyhow!(
                "VBAP panner created without precomputed effect tables"
            ));
        }
        let vbap_triangles = vbap.num_triangles();
        let topology = RenderTopology::new(
            Arc::new(build_prepared_render_engine(
                GainModelInstance::Vbap(VbapBackend::new(vbap)),
                match table_mode {
                    VbapTableMode::Polar => EffectiveEvaluationMode::PrecomputedPolar,
                    VbapTableMode::Cartesian { .. } => {
                        EffectiveEvaluationMode::PrecomputedCartesian
                    }
                },
            )?),
            speaker_layout,
        )?;

        log::info!(
            "Created spatial renderer: {} total speakers, {} spatializable, {} triangles, spread_res={}, table_mode={:?}, distance_model={}",
            num_speakers,
            num_vbap_speakers,
            vbap_triangles,
            spread_resolution,
            table_mode,
            distance_model
        );

        let excluded: Vec<&str> = topology
            .speaker_layout
            .speakers
            .iter()
            .filter(|s| !s.spatialize)
            .map(|s| s.name.as_str())
            .collect();
        let live_params = Self::build_live_params_and_log(
            &topology.speaker_layout,
            initial_evaluation_mode,
            az_res_deg,
            el_res_deg,
            distance_step,
            distance_max,
            allow_negative_z,
            vbap_position_interpolation,
            cartesian_default_x_size,
            cartesian_default_y_size,
            cartesian_default_z_size,
            cartesian_default_z_neg_size,
            master_gain_db,
            spread_min,
            spread_max,
            spread_from_distance,
            spread_distance_range,
            spread_distance_curve,
            RampMode::Sample,
            use_loudness,
            distance_model,
            room_ratio,
            room_ratio_rear,
            room_ratio_lower,
            room_ratio_center_blend,
            distance_diffuse,
            distance_diffuse_threshold,
            distance_diffuse_curve,
            auto_gain,
            &excluded,
            &topology.bed_to_speaker_mapping,
        );
        let editable_layout = topology.speaker_layout.clone();
        let control = RendererControl::new(
            live_params,
            topology,
            editable_layout,
            Some(crate::live_params::BackendRebuildParams {
                gain_model_kind: RenderBackendKind::Vbap.as_gain_model_kind(),
                preferred_evaluation_mode,
                allow_negative_z,
                vbap: Some(crate::live_params::VbapModelRebuildParams {
                    az_res_deg,
                    el_res_deg,
                    spread_resolution,
                    distance_max,
                    position_interpolation: vbap_position_interpolation,
                    table_mode,
                    cartesian_default_x_size,
                    cartesian_default_y_size,
                    cartesian_default_z_size,
                    cartesian_default_z_neg_size,
                    distance_model,
                    allow_negative_z,
                }),
            }),
        );

        Ok(Self::finish_construction(
            num_speakers,
            spread_resolution,
            sample_rate,
            distance_model,
            log_object_positions,
            auto_gain,
            control,
        ))
    }

    /// Create a new spatial renderer from a pre-loaded VBAP panner
    ///
    /// This allows using pre-computed VBAP gain tables loaded from disk,
    /// which is much faster than computing them at runtime.
    ///
    /// # Arguments
    ///
    /// * `vbap` - Pre-loaded VBAP panner (from VbapPanner::load_from_file)
    /// * `speaker_layout` - Speaker configuration (must match the VBAP table)
    /// * `sample_rate` - Sample rate in Hz (for ramp timing)
    /// * `distance_model` - Distance attenuation model
    /// * `spread_from_distance` - Calculate spread from distance instead of object spread metadata
    /// * `spread_distance_range` - Distance at which spread reaches 0.0
    /// * `spread_distance_curve` - Curve exponent for distance-based spread
    /// * `spread_min` - Minimum effective spread
    /// * `spread_max` - Maximum effective spread
    /// * `log_object_positions` - Enable detailed logging
    /// * `room_ratio` - Room proportions [width, length, height]
    /// * `room_ratio_lower` - Lower height ratio used for negative Z coordinates
    /// * `master_gain_db` - Master gain in dB
    /// * `auto_gain` - Enable automatic gain reduction to prevent clipping
    /// * `use_loudness` - Apply loudness metadata correction gain from stream metadata
    /// * `distance_diffuse` - Enable distance-based antipodal diffuse blending
    /// * `distance_diffuse_threshold` - ADM distance at which blend reaches 100% direct
    /// * `distance_diffuse_curve` - Curve exponent for the blend weight
    pub fn from_vbap(
        vbap: VbapPanner,
        speaker_layout: SpeakerLayout,
        sample_rate: u32,
        allow_negative_z: bool,
        vbap_position_interpolation: bool,
        distance_model: DistanceModel,
        distance_max: f32,
        spread_from_distance: bool,
        spread_distance_range: f32,
        spread_distance_curve: f32,
        spread_min: f32,
        spread_max: f32,
        log_object_positions: bool,
        room_ratio: [f32; 3],
        room_ratio_rear: f32,
        room_ratio_lower: f32,
        room_ratio_center_blend: f32,
        master_gain_db: f32,
        auto_gain: bool,
        use_loudness: bool,
        distance_diffuse: bool,
        distance_diffuse_threshold: f32,
        distance_diffuse_curve: f32,
    ) -> Result<Self> {
        let num_speakers = speaker_layout.num_speakers();
        let spread_resolution = vbap.spread_resolution();
        let distance_step = if spread_resolution > 0.0 {
            spread_resolution
        } else {
            0.25
        };
        let vbap = vbap
            .with_negative_z(allow_negative_z)
            .with_position_interpolation(vbap_position_interpolation)
            .precompute_effect_tables(
                distance_step,
                distance_max,
                spread_min.clamp(0.0, 1.0),
                spread_max.clamp(0.0, 1.0),
                distance_model,
                spread_from_distance,
                spread_distance_range,
                spread_distance_curve,
                distance_diffuse,
                distance_diffuse_threshold,
                distance_diffuse_curve,
                room_ratio,
                room_ratio_rear,
                room_ratio_lower,
                room_ratio_center_blend,
            )
            .map_err(|e| anyhow::anyhow!("Failed to precompute VBAP effect tables: {}", e))?;
        if !vbap.has_precomputed_effects() {
            return Err(anyhow::anyhow!(
                "Loaded VBAP panner without precomputed effect tables"
            ));
        }
        let vbap_num_speakers = vbap.num_speakers();
        let vbap_num_triangles = vbap.num_triangles();
        let vbap_table_mode = vbap.table_mode();
        let vbap_azimuth_resolution = vbap.azimuth_resolution();
        let vbap_elevation_resolution = vbap.elevation_resolution();
        let vbap_position_interpolation = vbap.position_interpolation();

        log::info!(
            "Created spatial renderer from pre-loaded VBAP table: {} total speakers, {} in VBAP table, {} triangles, spread_res={}, distance_model={}",
            num_speakers,
            vbap_num_speakers,
            vbap_num_triangles,
            spread_resolution,
            distance_model
        );
        let topology = RenderTopology::new(
            Arc::new(build_prepared_render_engine(
                GainModelInstance::Vbap(VbapBackend::new(vbap)),
                match vbap_table_mode {
                    VbapTableMode::Polar => EffectiveEvaluationMode::PrecomputedPolar,
                    VbapTableMode::Cartesian { .. } => {
                        EffectiveEvaluationMode::PrecomputedCartesian
                    }
                },
            )?),
            speaker_layout,
        )?;

        let excluded: Vec<&str> = topology
            .speaker_layout
            .speakers
            .iter()
            .filter(|s| !s.spatialize)
            .map(|s| s.name.as_str())
            .collect();
        let live_params = Self::build_live_params_and_log(
            &topology.speaker_layout,
            match vbap_table_mode {
                VbapTableMode::Polar => LiveEvaluationMode::PrecomputedPolar,
                VbapTableMode::Cartesian { .. } => LiveEvaluationMode::PrecomputedCartesian,
            },
            vbap_azimuth_resolution,
            vbap_elevation_resolution,
            distance_step,
            distance_max,
            allow_negative_z,
            vbap_position_interpolation,
            match vbap_table_mode {
                VbapTableMode::Cartesian { x_size, .. } => x_size.saturating_sub(1),
                VbapTableMode::Polar => 1,
            },
            match vbap_table_mode {
                VbapTableMode::Cartesian { y_size, .. } => y_size.saturating_sub(1),
                VbapTableMode::Polar => 1,
            },
            match vbap_table_mode {
                VbapTableMode::Cartesian { z_size, .. } => z_size.saturating_sub(1),
                VbapTableMode::Polar => 1,
            },
            match vbap_table_mode {
                VbapTableMode::Cartesian { z_neg_size, .. } => z_neg_size,
                VbapTableMode::Polar => 0,
            },
            master_gain_db,
            spread_min,
            spread_max,
            spread_from_distance,
            spread_distance_range,
            spread_distance_curve,
            RampMode::Sample,
            use_loudness,
            distance_model,
            room_ratio,
            room_ratio_rear,
            room_ratio_lower,
            room_ratio_center_blend,
            distance_diffuse,
            distance_diffuse_threshold,
            distance_diffuse_curve,
            auto_gain,
            &excluded,
            &topology.bed_to_speaker_mapping,
        );
        let rebuild_params =
            crate::live_params::BackendRebuildParams {
                gain_model_kind: RenderBackendKind::Vbap.as_gain_model_kind(),
                preferred_evaluation_mode: PreferredEvaluationMode::from_vbap_table_mode(
                    vbap_table_mode,
                ),
                allow_negative_z,
                vbap: Some(crate::live_params::VbapModelRebuildParams {
                az_res_deg: vbap_azimuth_resolution,
                el_res_deg: vbap_elevation_resolution,
                spread_resolution,
                distance_max,
                position_interpolation: vbap_position_interpolation,
                table_mode: vbap_table_mode,
                cartesian_default_x_size: match vbap_table_mode {
                    VbapTableMode::Cartesian { x_size, .. } => x_size.saturating_sub(1),
                    VbapTableMode::Polar => 1,
                },
                cartesian_default_y_size: match vbap_table_mode {
                    VbapTableMode::Cartesian { y_size, .. } => y_size.saturating_sub(1),
                    VbapTableMode::Polar => 1,
                },
                cartesian_default_z_size: match vbap_table_mode {
                    VbapTableMode::Cartesian { z_size, .. } => z_size.saturating_sub(1),
                    VbapTableMode::Polar => 1,
                },
                cartesian_default_z_neg_size: match vbap_table_mode {
                    VbapTableMode::Cartesian { z_neg_size, .. } => z_neg_size,
                    VbapTableMode::Polar => 0,
                },
                distance_model,
                allow_negative_z,
            }),
            };
        let editable_layout = topology.speaker_layout.clone();
        let control =
            RendererControl::new(live_params, topology, editable_layout, Some(rebuild_params));

        Ok(Self::finish_construction(
            num_speakers,
            spread_resolution,
            sample_rate,
            distance_model,
            log_object_positions,
            auto_gain,
            control,
        ))
    }

    /// Build `LiveParams` from common constructor arguments and emit the shared log lines.
    ///
    /// Called by both `new` and `from_vbap` after each constructor has logged its own
    /// format-specific header (VBAP table size, triangle count, …).
    #[allow(clippy::too_many_arguments)]
    fn build_live_params_and_log(
        speaker_layout: &SpeakerLayout,
        initial_evaluation_mode: LiveEvaluationMode,
        az_res_deg: i32,
        el_res_deg: i32,
        distance_res: f32,
        distance_max: f32,
        allow_negative_z: bool,
        vbap_position_interpolation: bool,
        cartesian_default_x_size: usize,
        cartesian_default_y_size: usize,
        cartesian_default_z_size: usize,
        cartesian_default_z_neg_size: usize,
        master_gain_db: f32,
        spread_min: f32,
        spread_max: f32,
        spread_from_distance: bool,
        spread_distance_range: f32,
        spread_distance_curve: f32,
        ramp_mode: RampMode,
        use_loudness: bool,
        distance_model: DistanceModel,
        room_ratio: [f32; 3],
        room_ratio_rear: f32,
        room_ratio_lower: f32,
        room_ratio_center_blend: f32,
        distance_diffuse: bool,
        distance_diffuse_threshold: f32,
        distance_diffuse_curve: f32,
        auto_gain: bool,
        excluded: &[&str],
        bed_to_speaker_mapping: &std::collections::HashMap<usize, usize>,
    ) -> LiveParams {
        if !excluded.is_empty() {
            log::info!("Excluded from VBAP spatialization: {}", excluded.join(", "));
        }
        if spread_from_distance {
            log::warn!(
                "spread-from-distance enabled: object spread metadata will be OVERRIDDEN by \
                 distance-based spread (formula: spread = (1.0 - dist/{})^{}, clamped to [0,1])",
                spread_distance_range,
                spread_distance_curve
            );
        }
        log::info!("Spread range: [{:.2}, {:.2}]", spread_min, spread_max);
        log::info!(
            "Room ratio: width={}, length={}, height+={}",
            room_ratio[0],
            room_ratio[1],
            room_ratio[2]
        );
        log::info!("Room ratio rear (depth<0): {}", room_ratio_rear);
        log::info!("Room ratio lower (z<0): {}", room_ratio_lower);
        log::info!("Room ratio center blend: {}", room_ratio_center_blend);
        log::info!("Ramp mode: {}", ramp_mode.as_str());
        log::info!(
            "VBAP position interpolation: {}",
            if vbap_position_interpolation {
                "enabled"
            } else {
                "disabled (nearest-cell lookup)"
            }
        );
        let master_gain = 10.0_f32.powf(master_gain_db / 20.0);
        log::info!(
            "Master gain: {:.1} dB (linear: {:.4}), auto-gain: {}",
            master_gain_db,
            master_gain,
            auto_gain
        );
        log::info!(
            "Bed to speaker mapping (by name): {:?}",
            bed_to_speaker_mapping
        );

        let mut speaker_live = std::collections::HashMap::new();
        for (idx, spk) in speaker_layout.speakers.iter().enumerate() {
            if spk.delay_ms != 0.0 {
                speaker_live.insert(
                    idx,
                    crate::live_params::SpeakerLiveParams {
                        delay_ms: spk.delay_ms.max(0.0),
                        ..Default::default()
                    },
                );
            }
        }

        LiveParams {
            master_gain,
            objects: std::collections::HashMap::new(),
            spread_min,
            spread_max,
            spread_from_distance,
            spread_distance_range,
            spread_distance_curve,
            ramp_mode,
            backend_kind: RenderBackendKind::Vbap,
            evaluation: EvaluationLiveParams {
                mode: initial_evaluation_mode,
                cartesian: CartesianEvaluationParams {
                    x_size: cartesian_default_x_size.max(1),
                    y_size: cartesian_default_y_size.max(1),
                    z_size: cartesian_default_z_size.max(1),
                    z_neg_size: cartesian_default_z_neg_size,
                },
                polar: PolarEvaluationParams {
                    azimuth_values: (360.0 / az_res_deg.max(1) as f32).round() as i32,
                    elevation_values: (((if allow_negative_z { 180.0 } else { 90.0 })
                        / el_res_deg.max(1) as f32)
                        .round() as i32),
                    distance_res: (distance_max / distance_res.max(0.01)).round() as i32,
                    distance_max: distance_max.max(0.01),
                },
            },
            vbap_table_mode: match initial_evaluation_mode {
                LiveEvaluationMode::Auto => LiveVbapTableMode::Auto,
                LiveEvaluationMode::PrecomputedPolar => LiveVbapTableMode::Polar,
                LiveEvaluationMode::PrecomputedCartesian => LiveVbapTableMode::Cartesian,
                LiveEvaluationMode::Realtime => LiveVbapTableMode::Auto,
            },
            vbap_position_interpolation,
            use_loudness,
            distance_model,
            speakers: speaker_live,
            room_ratio,
            room_ratio_rear,
            room_ratio_lower,
            room_ratio_center_blend,
            dialogue_level: None,
            use_distance_diffuse: distance_diffuse,
            distance_diffuse_threshold,
            distance_diffuse_curve,
        }
    }

    /// Assemble the `SpatialRenderer` struct from fully resolved components.
    ///
    /// Called by both `new` and `from_vbap` after each constructor has built its
    /// VBAP panner and `RendererControl`.
    #[allow(clippy::too_many_arguments)]
    fn finish_construction(
        num_speakers: usize,
        spread_resolution: f32,
        sample_rate: u32,
        distance_model: DistanceModel,
        log_object_positions: bool,
        auto_gain: bool,
        control: Arc<RendererControl>,
    ) -> Self {
        Self {
            num_speakers,
            spread_resolution,
            bed_indices: arc_swap::ArcSwap::new(std::sync::Arc::new(Vec::new())),
            first_render: std::sync::atomic::AtomicBool::new(true),
            frame_counter: std::sync::atomic::AtomicU64::new(0),
            channel_states: std::sync::Mutex::new(std::collections::HashMap::new()),
            sample_rate,
            distance_model,
            log_object_positions,
            loudness_gain: std::sync::atomic::AtomicU32::new(1.0_f32.to_bits()),
            auto_gain,
            current_auto_gain: std::sync::atomic::AtomicU32::new(1.0_f32.to_bits()),
            control,
            speaker_gains_buf: vec![0.0f32; num_speakers],
            object_params_buf: Vec::new(),
            speaker_params_buf: vec![
                crate::live_params::SpeakerLiveParams::default();
                num_speakers
            ],
            object_params_generation_seen: 0,
            speaker_params_generation_seen: 0,
            bed_routing_gains_buf: vec![0.0f32; num_speakers],
            delay_lines: {
                let max_delay = (0.1 * sample_rate as f32) as usize; // 100 ms
                (0..num_speakers)
                    .map(|_| crate::delay_line::DelayLine::new(max_delay))
                    .collect()
            },
        }
    }

    /// Get the current auto-gain attenuation in dB.
    /// Returns 0.0 if no clipping occurred (auto-gain = 1.0).
    /// Returns negative values indicating the attenuation applied (e.g., -3.0 means -3 dB).
    pub fn get_auto_gain_db(&self) -> f32 {
        let current = f32::from_bits(
            self.current_auto_gain
                .load(std::sync::atomic::Ordering::Relaxed),
        );
        if current >= 1.0 {
            0.0
        } else {
            20.0 * current.log10()
        }
    }

    /// Set loudness metadata correction gain based on `dialogue_level` from the stream.
    ///
    /// The reference level is -31 dBFS. The gain is calculated as:
    /// gain_db = -31 - dialogue_level
    ///
    /// For example:
    /// - dialogue_level = -27 dBFS → gain = -4 dB
    /// - dialogue_level = -31 dBFS → gain = 0 dB (reference)
    /// - dialogue_level = -24 dBFS → gain = -7 dB
    pub fn set_loudness(&self, dialogue_level: i8) {
        const REFERENCE_LEVEL: i32 = -31;
        let gain_db = REFERENCE_LEVEL - (dialogue_level as i32);
        let gain_linear = 10.0_f32.powf(gain_db as f32 / 20.0);
        self.loudness_gain
            .store(gain_linear.to_bits(), std::sync::atomic::Ordering::Relaxed);
        self.control.live.write().unwrap().dialogue_level = Some(dialogue_level);
        log::info!(
            "Dialog normalization: dialogue_level={} dBFS → gain={} dB (linear: {:.4})",
            dialogue_level,
            gain_db,
            gain_linear
        );
    }

    /// Set the bed channel IDs in PCM channel order.
    ///
    /// Must be called once when the first metadata arrives, before any call to `render_frame`.
    /// The mapping is stable for the lifetime of the stream.
    pub fn configure_beds(&self, bed_indices: &[usize]) {
        self.bed_indices
            .store(std::sync::Arc::new(bed_indices.to_vec()));
        log::debug!("Renderer bed_indices configured: {:?}", bed_indices);
    }

    /// Return the shared `RendererControl` Arc so that `OscSender` can hold it.
    pub fn renderer_control(&self) -> Arc<RendererControl> {
        Arc::clone(&self.control)
    }

    /// Clear cached per-channel spatial/ramp state after a decoder reset or
    /// stream restart so stale object positions cannot leak into subsequent
    /// rendering.
    pub fn reset_runtime_state(&self) {
        self.channel_states.lock().unwrap().clear();
        self.first_render
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Update channel states from format-agnostic spatial events.
    ///
    /// Called internally from `render_frame` when pending events are present.
    /// The `channel_idx` and `is_bed` fields of each event must already be
    /// resolved by the caller (see `SpatialChannelEvent`).
    fn update_metadata(&self, events: &[SpatialChannelEvent]) -> Result<()> {
        let mut channel_states = self.channel_states.lock().unwrap();

        for event in events {
            let state = channel_states
                .entry(event.channel_idx)
                .or_insert_with(ChannelState::default);

            if let Some(gain) = event.gain_db {
                state.gain_db = gain;
            }
            if let Some(ramp_length) = event.ramp_length {
                state.ramp_length = ramp_length as u64;
            }

            // Beds are routed directly to speakers — no position state needed.
            if event.is_bed {
                continue;
            }

            // Per-event spread is intentionally ignored.
            let spread_changed = false;

            if let Some(target_position) = event.position {
                if state.target.position != target_position || spread_changed {
                    if self.log_object_positions {
                        let remaining_units = state.remaining_ramp_units.unwrap_or(0);
                        let sample_pos = event.sample_pos.unwrap_or(0);
                        if state.target.position != target_position {
                            log::info!(
                                "  Obj ch{:2}: sample_pos {} remaining {} - Starting ramp over {} samples (~{}ms)",
                                event.channel_idx,
                                sample_pos,
                                remaining_units,
                                state.ramp_length,
                                state.ramp_length as f32 / self.sample_rate as f32 * 1000.0
                            );
                        }
                    }
                    state.target.position = target_position;
                    state.remaining_ramp_units = Some(state.ramp_length);
                    state.target_sample_index = event.sample_pos;
                }
            } else if spread_changed {
                if state.remaining_ramp_units.is_none() {
                    state.current.spread = state.target.spread;
                }
            }
        }

        Ok(())
    }

    /// Render audio objects to speaker channels for a single frame
    ///
    /// This function takes the raw PCM data from the decoder (bed + objects),
    /// separates the object channels based on bed_indices, applies VBAP panning
    /// to object channels, and routes bed channels directly to speakers.
    ///
    /// # Arguments
    ///
    /// * `pcm_data` - Decoded PCM samples [sample_idx][channel_idx]
    /// * `metadata` - Spatial object metadata (positions, gains, etc.)
    /// * `total_channels` - Total number of channels in pcm_data (bed + objects)
    /// * `bed_indices` - Indices of channels that are bed channels (e.g., [3] for LFE only)
    ///
    /// # Returns
    ///
    /// Interleaved speaker samples: [sample_idx][speaker_idx]
    ///
    /// # Notes
    ///
    /// - Channels in `bed_indices` are copied directly to corresponding speakers
    /// - All other channels are treated as objects and spatialized with VBAP
    /// - Output has self.num_speakers channels
    /// - MAX_CHANNELS is 16 (decoder maximum)
    /// Render a frame of spatial audio into a pre-allocated output buffer.
    ///
    /// The caller provides `samples_buf` — a `Vec<f32>` that will be cleared,
    /// resized to `sample_length × num_speakers`, and filled with interleaved
    /// speaker audio.  Passing back the `RenderedFrame::samples` from the
    /// *previous* call eliminates the per-frame heap allocation after warm-up:
    ///
    /// ```ignore
    /// let mut buf = Vec::new();
    /// loop {
    ///     let frame = renderer.render_frame(pcm, channels, events, buf)?;
    ///     // … consume frame.samples …
    ///     buf = frame.samples; // donate back for next iteration
    /// }
    /// ```
    pub fn render_frame(
        &mut self,
        input_pcm: &[f32],
        input_channel_count: usize,
        pending_events: &[SpatialChannelEvent],
        samples_buf: Vec<f32>,
    ) -> Result<RenderedFrame> {
        if !pending_events.is_empty() {
            self.update_metadata(pending_events)?;
        }

        // ── 1. Snapshot live params so we hold the read lock for as short a time as possible ──
        let live = {
            let g = self.control.live.read().unwrap();
            let object_params_generation = self
                .control
                .object_params_generation
                .load(std::sync::atomic::Ordering::Relaxed);
            let speaker_params_generation = self
                .control
                .speaker_params_generation
                .load(std::sync::atomic::Ordering::Relaxed);

            if self.object_params_generation_seen != object_params_generation {
                if self.object_params_buf.len() < input_channel_count {
                    self.object_params_buf.resize(
                        input_channel_count,
                        crate::live_params::ObjectLiveParams::default(),
                    );
                }
                for params in self.object_params_buf.iter_mut().take(input_channel_count) {
                    *params = crate::live_params::ObjectLiveParams::default();
                }
                for (&idx, params) in &g.objects {
                    if idx >= self.object_params_buf.len() {
                        self.object_params_buf
                            .resize(idx + 1, crate::live_params::ObjectLiveParams::default());
                    }
                    self.object_params_buf[idx] = params.clone();
                }
                self.object_params_generation_seen = object_params_generation;
            } else if self.object_params_buf.len() < input_channel_count {
                self.object_params_buf.resize(
                    input_channel_count,
                    crate::live_params::ObjectLiveParams::default(),
                );
            }

            if self.speaker_params_generation_seen != speaker_params_generation {
                if self.speaker_params_buf.len() < self.num_speakers {
                    self.speaker_params_buf.resize(
                        self.num_speakers,
                        crate::live_params::SpeakerLiveParams::default(),
                    );
                }
                for params in self.speaker_params_buf.iter_mut().take(self.num_speakers) {
                    *params = crate::live_params::SpeakerLiveParams::default();
                }
                for (&idx, params) in &g.speakers {
                    if idx < self.speaker_params_buf.len() {
                        self.speaker_params_buf[idx] = params.clone();
                    }
                }
                self.speaker_params_generation_seen = speaker_params_generation;
            }
            LiveSnapshot {
                master_gain: g.master_gain,
                object_params: &self.object_params_buf[..input_channel_count],
                spread_min: g.spread_min,
                spread_max: g.spread_max,
                spread_from_distance: g.spread_from_distance,
                spread_distance_range: g.spread_distance_range,
                spread_distance_curve: g.spread_distance_curve,
                ramp_mode: g.ramp_mode,
                use_loudness: g.use_loudness,
                speaker_params: &self.speaker_params_buf[..self.num_speakers],
                room_ratio: g.room_ratio,
                room_ratio_rear: g.room_ratio_rear,
                room_ratio_lower: g.room_ratio_lower,
                room_ratio_center_blend: g.room_ratio_center_blend,
                use_distance_diffuse: g.use_distance_diffuse,
                distance_diffuse_threshold: g.distance_diffuse_threshold,
                distance_diffuse_curve: g.distance_diffuse_curve,
            }
        };

        // ── 2. Load the current immutable render topology (lock-free ArcSwap snapshot) ──
        let topology_guard = self.control.active_topology();
        let topology = &*topology_guard;
        let topology_identity = std::sync::Arc::as_ptr(&topology_guard) as usize;

        let start_time = std::time::Instant::now();

        // Derive sample count from slice length and channel count.
        let sample_length = if input_channel_count > 0 {
            input_pcm.len() / input_channel_count
        } else {
            0
        };

        // Snapshot bed_indices once for this frame via ArcSwap: no mutex and no Vec clone.
        let bed_indices = self.bed_indices.load_full();
        let active_layout = &topology.speaker_layout;
        let active_bed_to_speaker_mapping = &topology.bed_to_speaker_mapping;
        let active_backend_to_speaker_mapping = &topology.backend_to_speaker_mapping;

        // Reuse the donated buffer — resize (no alloc if capacity suffices) and zero it.
        let mut output = samples_buf;
        let required = sample_length * self.num_speakers;
        output.clear();
        output.resize(required, 0.0);

        // Collect VBAP gains at the final sample for each object channel (for monitoring).
        let mut object_gains_out: Vec<(usize, Gains)> = Vec::with_capacity(input_channel_count);

        // Beds always come FIRST in PCM data, then objects.
        // bed_indices contains bed channel IDs (e.g., [3] for LFE), NOT PCM channel indices.
        let num_beds = bed_indices.len();

        // Check if this is the first render for detailed logging
        let is_first = self
            .first_render
            .swap(false, std::sync::atomic::Ordering::Relaxed);
        let active_speaker_names = if is_first || self.log_object_positions {
            Some(active_layout.speaker_names())
        } else {
            None
        };

        if is_first {
            log::info!(
                "VBAP render: {} total PCM channels, {} bed channels (PCM 0..{}), {} object channels (PCM {}..{})",
                input_channel_count,
                num_beds,
                num_beds.saturating_sub(1),
                input_channel_count - num_beds,
                num_beds,
                input_channel_count - 1
            );
            log::info!("  Bed IDs (spatial metadata): {:?}", bed_indices);
            log::info!("  Mapping: channel_idx -> bed_id");
            for (ch_idx, &bed_id) in bed_indices.iter().enumerate() {
                log::info!("    PCM channel {} -> bed channel ID {}", ch_idx, bed_id);
            }
        }

        // Hold channel metadata state lock once for the whole render pass.
        // This avoids lock/unlock churn in the channel loop.
        let mut channel_states = self.channel_states.lock().unwrap();

        // Process each channel
        for input_channel_idx in 0..input_channel_count {
            // Per-channel live gain + mute (applies to beds and objects).
            // Mute is independent of gain: unmuting restores the stored gain.
            let obj_params = live.object_params.get(input_channel_idx);
            let obj_gain = match obj_params {
                Some(o) if o.muted => 0.0,
                Some(o) => o.gain,
                None => 1.0,
            };

            // Get gain from cached metadata (common for ALL channels - beds and objects)
            let gain_db = channel_states
                .get(&input_channel_idx)
                .map(|s| s.gain_db)
                .unwrap_or(-128);

            // Convert gain from dB to linear
            let gain_linear = if gain_db == -128 {
                0.0 // -inf dB
            } else {
                10.0_f32.powf(gain_db as f32 / 20.0)
            };

            if input_channel_idx < num_beds {
                // BED CHANNEL: Route to speaker based on bed_to_speaker_mapping (by name)
                let bed_id = bed_indices[input_channel_idx];

                // Look up speaker index from bed ID using name-based mapping
                let speaker_idx = match active_bed_to_speaker_mapping.get(&bed_id) {
                    Some(&idx) => idx,
                    None => {
                        // Bed ID not found in speaker layout - skip this channel
                        if is_first {
                            log::warn!(
                                "  Bed ch{} (ID={}) has no matching speaker in layout, skipping",
                                input_channel_idx,
                                bed_id
                            );
                        }
                        continue;
                    }
                };

                self.bed_routing_gains_buf.fill(0.0);
                self.bed_routing_gains_buf[speaker_idx] = 1.0;

                // Mix bed samples through the same per-speaker gain accumulation model
                // used for objects, but with a one-hot routing table.
                for sample_idx in 0..sample_length {
                    let sample = input_pcm[sample_idx * input_channel_count + input_channel_idx]
                        * gain_linear
                        * obj_gain;
                    let out_base = sample_idx * self.num_speakers;
                    for (speaker_idx, &gain) in self.bed_routing_gains_buf.iter().enumerate() {
                        output[out_base + speaker_idx] += sample * gain;
                    }
                }

                let mut gains = Gains::zeroed(self.num_speakers);
                for (speaker_idx, &gain) in self.bed_routing_gains_buf.iter().enumerate() {
                    gains.set(speaker_idx, gain);
                }
                object_gains_out.push((input_channel_idx, gains));

                if is_first {
                    let speaker_name = active_layout.speakers[speaker_idx].name.as_str();
                    log::info!(
                        "  Bed ch{} (ID={}) → Speaker {} ({}) gain={}dB",
                        input_channel_idx,
                        bed_id,
                        speaker_idx,
                        speaker_name,
                        gain_db
                    );
                }
            } else {
                let state_mut = channel_states.get_mut(&input_channel_idx);
                let state = match state_mut {
                    // Skip if no metadata available
                    Some(s) => s,
                    None => {
                        if self.log_object_positions {
                            log::warn!(
                                "Channel {} missing cached metadata, skipping",
                                input_channel_idx
                            );
                        }
                        continue;
                    }
                };

                let frame_rendering_position = match live.ramp_mode {
                    RampMode::Off => {
                        state.current = state.target;
                        state.remaining_ramp_units = None;
                        Some((state.current.position, RampStatus::Finished))
                    }
                    RampMode::Frame => {
                        let rendering_position = state.current.position;
                        let ramping = state.process_ramp(sample_length);
                        Some((rendering_position, ramping))
                    }
                    RampMode::Sample => {
                        if state.remaining_ramp_units.is_none() {
                            Some((state.current.position, RampStatus::Idle))
                        } else {
                            None
                        }
                    }
                };

                let compute_object_gains = |rendering_position: [f64; 3]| {
                    let scaled_x = rendering_position[0] as f32 * live.room_ratio[0];
                    let scaled_y = map_depth_with_room_ratios(
                        rendering_position[1] as f32,
                        live.room_ratio[1],
                        live.room_ratio_rear,
                        live.room_ratio_center_blend,
                    );
                    let scaled_z = if rendering_position[2] >= 0.0 {
                        rendering_position[2] as f32 * live.room_ratio[2]
                    } else {
                        rendering_position[2] as f32 * live.room_ratio_lower
                    };
                    let final_gains = topology
                        .backend
                        .compute_gains(&RenderRequest {
                            adm_position: rendering_position,
                            spread_min: live.spread_min,
                            spread_max: live.spread_max,
                            spread_from_distance: live.spread_from_distance,
                            spread_distance_range: live.spread_distance_range,
                            spread_distance_curve: live.spread_distance_curve,
                            room_ratio: live.room_ratio,
                            room_ratio_rear: live.room_ratio_rear,
                            room_ratio_lower: live.room_ratio_lower,
                            room_ratio_center_blend: live.room_ratio_center_blend,
                            use_distance_diffuse: live.use_distance_diffuse,
                            distance_diffuse_threshold: live.distance_diffuse_threshold,
                            distance_diffuse_curve: live.distance_diffuse_curve,
                            distance_model: self.distance_model,
                        })
                        .gains;
                    (scaled_x, scaled_y, scaled_z, final_gains)
                };

                // Fast path: with per-frame or disabled ramping, the position is constant
                // across the whole frame so the speaker gains only need to be computed once.
                if let Some((rendering_position, ramping)) = frame_rendering_position {
                    let final_gains = if state.render_gain_cache.matches(
                        topology_identity,
                        rendering_position,
                        &live,
                        self.distance_model,
                    ) {
                        &state.render_gain_cache.gains
                    } else {
                        let (_, _, _, final_gains) = compute_object_gains(rendering_position);
                        state.render_gain_cache.gains.clear();
                        state
                            .render_gain_cache
                            .gains
                            .extend(final_gains.iter().copied());
                        state.render_gain_cache.update_signature(
                            topology_identity,
                            rendering_position,
                            &live,
                            self.distance_model,
                        );
                        &state.render_gain_cache.gains
                    };

                    if self.log_object_positions {
                        match ramping {
                            RampStatus::Ramping => {
                                //log::info!(
                                //    "  Obj ch{:2} RAMPED: ADM({:+.5},{:+.5},{:+.5}) ",
                                //    input_channel_idx,
                                //    state.current.position[0], state.current.position[1], state.current.position[2],
                                //);
                            }
                            RampStatus::Finished => {
                                // Log the movement
                                // Map VBAP gains to actual speaker names using the mapping (if any)
                                let gains_with_names: Vec<String> = final_gains
                                    .iter()
                                    .enumerate()
                                    .map(|(idx, g)| {
                                        let speaker_idx = if let Some(mapping) =
                                            active_backend_to_speaker_mapping
                                        {
                                            mapping[idx]
                                        } else {
                                            idx
                                        };
                                        format!(
                                            "{}={:.3}",
                                            active_speaker_names
                                                .as_ref()
                                                .expect("speaker names available for logging")
                                                [speaker_idx],
                                            g
                                        )
                                    })
                                    .collect();
                                let active_speakers =
                                    final_gains.iter().filter(|&&g| g > 0.01).count();

                                //                       let distance_db = 20.0 * final_distance_attenuation.log10();
                                let spread_indicator =
                                    if live.spread_from_distance { "d" } else { "" };

                                // Log the SCALED spherical coordinates (what VBAP actually sees)
                                let scaled_x = rendering_position[0] as f32 * live.room_ratio[0];
                                let scaled_y = map_depth_with_room_ratios(
                                    rendering_position[1] as f32,
                                    live.room_ratio[1],
                                    live.room_ratio_rear,
                                    live.room_ratio_center_blend,
                                );
                                let scaled_z = if rendering_position[2] >= 0.0 {
                                    rendering_position[2] as f32 * live.room_ratio[2]
                                } else {
                                    rendering_position[2] as f32 * live.room_ratio_lower
                                };
                                let (azimuth, elevation, distance) =
                                    adm_to_spherical(scaled_x, scaled_y, scaled_z);

                                log::info!(
                                    "  Obj ch{:2}: ADM({:+.2},{:+.2},{:+.2}) (az:{:+.2},el:{:+.2},d:{:+.2}) ({:+.1}dB) spread[min={:.2},max={:.2}]{} → [{}] ({} spk)",
                                    input_channel_idx,
                                    rendering_position[0],
                                    rendering_position[1],
                                    rendering_position[2],
                                    azimuth,
                                    elevation,
                                    distance,
                                    gain_db,
                                    live.spread_min,
                                    live.spread_max,
                                    spread_indicator,
                                    gains_with_names.join(", "),
                                    active_speakers
                                );
                            }
                            RampStatus::Idle => {}
                        }
                    }

                    for sample_idx in 0..sample_length {
                        let object_sample = input_pcm
                            [sample_idx * input_channel_count + input_channel_idx]
                            * gain_linear
                            * obj_gain;

                        let out_base = sample_idx * self.num_speakers;
                        if let Some(mapping) = active_backend_to_speaker_mapping {
                            for (vbap_idx, &gain) in final_gains.iter().enumerate() {
                                let speaker_idx = mapping[vbap_idx];
                                output[out_base + speaker_idx] += object_sample * gain;
                            }
                        } else {
                            for (speaker_idx, &gain) in final_gains.iter().enumerate() {
                                output[out_base + speaker_idx] += object_sample * gain;
                            }
                        }
                    }

                    if let Some(mapping) = active_backend_to_speaker_mapping {
                        let mut gains = Gains::zeroed(self.num_speakers);
                        for (vbap_idx, &gain) in final_gains.iter().enumerate() {
                            gains.set(mapping[vbap_idx], gain);
                        }
                        object_gains_out.push((input_channel_idx, gains));
                    } else {
                        let mut gains = Gains::zeroed(self.num_speakers);
                        for (speaker_idx, &gain) in final_gains.iter().enumerate() {
                            gains.set(speaker_idx, gain);
                        }
                        object_gains_out.push((input_channel_idx, gains));
                    }
                    continue;
                }

                // Mix object into speaker channels with interpolated gains (ramped if moving)
                for sample_idx in 0..sample_length {
                    // OBJECT CHANNEL: VBAP spatialization
                    // Get channel state from cached metadata
                    let rendering_position = state.current.position;
                    let _ramping = state.process_ramp(1);
                    state.render_gain_cache.valid = false;
                    let (_, _, _, final_gains) = compute_object_gains(rendering_position);

                    // Mix object into speaker channels with interpolated gains (ramped if moving)
                    // Apply metadata gain + per-object live gain.
                    let object_sample = input_pcm
                        [sample_idx * input_channel_count + input_channel_idx]
                        * gain_linear
                        * obj_gain;

                    // Apply gains to speakers
                    // v4 format: direct iteration (SIMD friendly), gains array has all speakers
                    // v3 format: use mapping to scatter gains to correct speaker indices
                    let out_base = sample_idx * self.num_speakers;
                    if let Some(mapping) = active_backend_to_speaker_mapping {
                        // v3/runtime: use mapping
                        for (vbap_idx, &gain) in final_gains.iter().enumerate() {
                            let speaker_idx = mapping[vbap_idx];
                            output[out_base + speaker_idx] += object_sample * gain;
                        }
                        // At the last sample, collect gains for the caller (monitoring/OSC).
                        if sample_idx == sample_length - 1 {
                            let mut gains = Gains::zeroed(self.num_speakers);
                            for (vbap_idx, &gain) in final_gains.iter().enumerate() {
                                gains.set(mapping[vbap_idx], gain);
                            }
                            object_gains_out.push((input_channel_idx, gains));
                        }
                    } else {
                        // v4 expanded: direct iteration (gains[i] corresponds to speaker[i])
                        for (speaker_idx, &gain) in final_gains.iter().enumerate() {
                            output[out_base + speaker_idx] += object_sample * gain;
                        }
                        // At the last sample, collect gains for the caller (monitoring/OSC).
                        if sample_idx == sample_length - 1 {
                            object_gains_out.push((input_channel_idx, final_gains.clone()));
                        }
                    }
                }
            }
        }
        drop(channel_states);

        // topology_guard is an ArcSwap Guard (no lock held); drop it here to make the
        // intent explicit before the gain/auto-gain section.
        drop(topology_guard);

        // Increment frame counter
        let _frame_num = self
            .frame_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // Get current auto-gain value.
        let current_auto_gain = f32::from_bits(
            self.current_auto_gain
                .load(std::sync::atomic::Ordering::Relaxed),
        );

        // Dialog norm: only apply if the live flag is set.
        let loudness = if live.use_loudness {
            f32::from_bits(
                self.loudness_gain
                    .load(std::sync::atomic::Ordering::Relaxed),
            )
        } else {
            1.0
        };

        let total_gain = live.master_gain * loudness * current_auto_gain;

        // Pre-compute per-speaker total gains and update delay-line targets in a
        // single pass over the speaker list — one HashMap lookup per speaker.
        // Mute overrides gain to 0.0 without touching the stored gain value.
        self.speaker_gains_buf
            .iter_mut()
            .enumerate()
            .for_each(|(idx, g)| {
                let sp = live.speaker_params.get(idx);
                *g = if sp.is_some_and(|s| s.muted) {
                    0.0
                } else {
                    total_gain * sp.map_or(1.0, |s| s.gain)
                };
            });
        for (idx, dl) in self.delay_lines.iter_mut().enumerate() {
            dl.set_target_ms(
                live.speaker_params.get(idx).map_or(0.0, |s| s.delay_ms),
                self.sample_rate,
            );
        }
        let speaker_total_gains = &self.speaker_gains_buf;

        // Apply per-speaker gains and delay lines, and detect peak.
        let mut peak_sample: f32 = 0.0;
        for sample_idx in 0..sample_length {
            for speaker_idx in 0..self.num_speakers {
                let s = &mut output[sample_idx * self.num_speakers + speaker_idx];
                *s *= speaker_total_gains[speaker_idx];
                *s = self.delay_lines[speaker_idx].process(*s);
                peak_sample = peak_sample.max(s.abs());
            }
        }

        // Auto-gain adjustment: if clipping detected, reduce gain permanently (no recovery)
        // This acts as a "peak hold" - we keep the minimum gain needed to avoid clipping
        if self.auto_gain && peak_sample > 1.0 {
            // Calculate the gain needed to bring this peak to exactly 1.0
            let required_gain = 1.0 / peak_sample;
            // New auto-gain = current * required (reduces further if needed)
            let new_auto_gain = current_auto_gain * required_gain;
            self.current_auto_gain.store(
                new_auto_gain.to_bits(),
                std::sync::atomic::Ordering::Relaxed,
            );

            let total_reduction_db = 20.0 * new_auto_gain.log10();
            log::warn!(
                "Clipping detected (peak={:.3})! Auto-gain reduced to {:.4} ({:.1} dB)",
                peak_sample,
                new_auto_gain,
                total_reduction_db
            );
        }

        // Warn only if rendering is unusually slow
        let elapsed = start_time.elapsed();
        if elapsed.as_micros() > 5000 {
            let num_objects = input_channel_count - num_beds;
            log::warn!(
                "VBAP render_frame took {}μs ({} objects, {} samples) - VERY SLOW!",
                elapsed.as_micros(),
                num_objects,
                sample_length
            );
        }

        object_gains_out.sort_by_key(|(idx, _)| *idx);
        Ok(RenderedFrame {
            samples: output,
            object_gains: object_gains_out,
        })
    }

    /// Get the number of output speakers
    pub fn num_speakers(&self) -> usize {
        self.num_speakers
    }

    pub fn speaker_layout(&self) -> crate::speaker_layout::SpeakerLayout {
        self.control.active_layout()
    }

    /// Get speaker names
    pub fn speaker_names(&self) -> Vec<String> {
        self.control
            .topology
            .load()
            .speaker_layout
            .speaker_names()
            .into_iter()
            .map(|s| s.to_string())
            .collect()
    }

    /// Get spread resolution
    pub fn spread_resolution(&self) -> f32 {
        self.spread_resolution
    }

    /// Save VBAP gain table to binary file (includes speaker layout)
    ///
    /// # Arguments
    ///
    /// * `path` - Output file path
    ///
    /// # Example
    ///
    /// ```no_run
    /// renderer.save_vbap_table("vbap_7.1.4.bin")?;
    /// ```
    pub fn save_vbap_table(&self, path: &std::path::Path) -> Result<()> {
        let topology = self.control.active_topology();
        topology
            .backend
            .save_to_file(path, &topology.speaker_layout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_renderer_creation() {
        let layout = SpeakerLayout::preset("7.1.4").unwrap();
        let renderer = SpatialRenderer::new(
            layout,
            48000,
            1,
            1,
            0.0,
            2.0,
            VbapTableMode::Polar,
            false,
            DistanceModel::Linear,
            false,
            1.0,
            1.0,
            0.0,
            1.0,
            false,
            [1.0, 2.0, 0.5],
            2.0,
            0.5,
            0.0,
            false,
            false,
            false,
            1.0,
            1.0,
            PreferredEvaluationMode::PrecomputedPolar,
            LiveEvaluationMode::PrecomputedPolar,
            31,
            31,
            15,
        );

        assert!(renderer.is_ok());

        let renderer = renderer.unwrap();
        assert_eq!(renderer.num_speakers(), 12);
    }

    // TODO: Add integration test with real spatial metadata
    // For now, testing is done via real spatial audio content decoding
}
