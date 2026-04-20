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

use crate::crossover::{BiquadState, FreqBand, LR4CrossoverBank, compute_bands};
use crate::live_params::{
    CartesianEvaluationParams, EvaluationLiveParams, LiveEvaluationMode, LiveParams,
    PolarEvaluationParams, RampMode, RenderTopology, RendererControl,
};
use crate::ramp_strategy::{
    ChannelRampState, GainTableRampStrategy, PositionRampStrategy, RampContext, RampProgress,
    RampRenderParams, RampStatus, RampStrategy, RampTarget,
};
use crate::live_params::PreferredEvaluationMode;
use crate::render_backend::RenderRequest;
use crate::render_backend::{
    CartesianEvaluationConfig, EffectiveEvaluationMode, EvaluationBuildConfig,
    LoadedEvaluationArtifact, LoadedVbapFile, PolarEvaluationConfig, RenderBackendKind,
    SerializedEvaluationMode, VbapBackend, build_from_artifact_render_engine,
    build_from_file_render_engine, build_prepared_render_engine,
};
use crate::spatial_vbap::VbapPanner;
use crate::spatial_vbap::VbapTableMode;
use crate::spatial_vbap::{DistanceModel, Gains, adm_to_spherical};
use crate::speaker_layout::SpeakerLayout;
use anyhow::Result;
use std::str::FromStr;
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

fn evaluation_build_config(
    request_template: RenderRequest,
    position_interpolation: bool,
    table_mode: VbapTableMode,
    azimuth_resolution: i32,
    elevation_resolution: i32,
    distance_step: f32,
    distance_max: f32,
    allow_negative_z: bool,
) -> EvaluationBuildConfig {
    let cartesian = match table_mode {
        VbapTableMode::Cartesian {
            x_size,
            y_size,
            z_size,
            z_neg_size,
        } => CartesianEvaluationConfig {
            x_size,
            y_size,
            z_size,
            z_neg_size,
        },
        VbapTableMode::Polar => CartesianEvaluationConfig {
            x_size: 2,
            y_size: 2,
            z_size: 2,
            z_neg_size: usize::from(allow_negative_z),
        },
    };
    let azimuth_values = (360 / azimuth_resolution.max(1)).max(2) as usize;
    let elevation_span = if allow_negative_z { 180 } else { 90 };
    let elevation_values = (elevation_span / elevation_resolution.max(1)).max(2) as usize;
    let distance_values =
        ((distance_max.max(0.01) / distance_step.max(0.01)).round() as usize).max(1) + 1;
    EvaluationBuildConfig {
        request_template,
        position_interpolation,
        cartesian,
        polar: PolarEvaluationConfig {
            azimuth_values,
            elevation_values,
            distance_values,
            distance_max,
            allow_negative_z,
        },
    }
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

/// Per-channel state for movement detection and gain ramping
#[derive(Clone)]
struct ChannelState {
    /// Gain in dB
    gain_db: i8,

    ramp: ChannelRampState,
}

impl Default for ChannelState {
    fn default() -> Self {
        Self {
            gain_db: -128, // -inf dB (muted)
            ramp: ChannelRampState::default(),
        }
    }
}

/// VBAP engine for one crossover frequency band.
struct CrossoverBandEngine {
    /// Indices into the full speaker layout for the speakers in this band.
    speaker_indices: Vec<usize>,
    /// VBAP backend restricted to this band's speakers.
    /// `None` when the band has fewer than 3 speakers (uses uniform gain instead).
    vbap: Option<VbapBackend>,
}

impl CrossoverBandEngine {
    fn from_band(
        band: &FreqBand,
        layout: &crate::speaker_layout::SpeakerLayout,
        az_res_deg: i32,
        el_res_deg: i32,
    ) -> Self {
        let speaker_indices = band.speaker_indices.clone();
        let positions: Vec<[f32; 2]> = speaker_indices
            .iter()
            .map(|&i| layout.speakers[i].position())
            .collect();

        let vbap = if positions.len() >= 3 {
            VbapPanner::new(&positions, az_res_deg, el_res_deg, 0.0)
                .ok()
                .map(|p| VbapBackend::new(p))
        } else {
            None
        };

        Self { speaker_indices, vbap }
    }

    /// Compute gains for this band at `position` using the given render params.
    fn compute_gains(
        &self,
        render_params: crate::ramp_strategy::RampRenderParams,
        position: [f64; 3],
    ) -> crate::spatial_vbap::Gains {
        let req = render_params.render_request(position);
        match &self.vbap {
            Some(backend) => backend.compute_gains(&req).gains,
            None => {
                let n = self.speaker_indices.len();
                let mut gains = crate::spatial_vbap::Gains::zeroed(n);
                if n > 0 {
                    let g = 1.0 / (n as f32).sqrt();
                    for i in 0..n {
                        gains.set(i, g);
                    }
                }
                gains
            }
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
    position_interpolation: bool,
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
    barycenter: crate::live_params::BarycenterLiveParams,
    experimental_distance: crate::live_params::ExperimentalDistanceLiveParams,
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

    /// Optional contributor-provided ramp strategy override.
    ramp_strategy_override: Option<Arc<dyn RampStrategy>>,

    /// Per-band VBAP engines, derived from speaker `freq_low` values at construction.
    /// Empty when no speaker defines `freq_low` (standard single-band rendering).
    crossover_bands: Vec<CrossoverBandEngine>,

    /// Crossover filter bank for splitting objects into frequency bands.
    /// `None` when `crossover_bands` has ≤ 1 entry.
    crossover_filter_bank: Option<LR4CrossoverBank>,

    /// Per-object filter states for the crossover bank, keyed by channel index.
    crossover_filter_states: std::collections::HashMap<usize, Vec<BiquadState>>,
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
    /// Without saf_vbap, use `from_vbap_file()` to load pre-generated tables.
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
        let vbap_triangles = vbap.num_triangles();
        let topology = RenderTopology::new(
            Arc::new(build_prepared_render_engine(
                Box::new(VbapBackend::new(vbap)),
                match table_mode {
                    VbapTableMode::Polar => EffectiveEvaluationMode::PrecomputedPolar,
                    VbapTableMode::Cartesian { .. } => {
                        EffectiveEvaluationMode::PrecomputedCartesian
                    }
                },
                &evaluation_build_config(
                    RenderRequest {
                        adm_position: [0.0, 0.0, 0.0],
                        spread_min,
                        spread_max,
                        spread_from_distance,
                        spread_distance_range,
                        spread_distance_curve,
                        room_ratio,
                        room_ratio_rear,
                        room_ratio_lower,
                        room_ratio_center_blend,
                        use_distance_diffuse: distance_diffuse,
                        distance_diffuse_threshold,
                        distance_diffuse_curve,
                        distance_model,
                        barycenter_localize: 0.0,
                        experimental_distance_distance_floor:
                            crate::live_params::ExperimentalDistanceLiveParams::default()
                                .distance_floor,
                        experimental_distance_min_active_speakers:
                            crate::live_params::ExperimentalDistanceLiveParams::default()
                                .min_active_speakers,
                        experimental_distance_max_active_speakers:
                            crate::live_params::ExperimentalDistanceLiveParams::default()
                                .max_active_speakers,
                        experimental_distance_position_error_floor:
                            crate::live_params::ExperimentalDistanceLiveParams::default()
                                .position_error_floor,
                        experimental_distance_position_error_nearest_scale:
                            crate::live_params::ExperimentalDistanceLiveParams::default()
                                .position_error_nearest_scale,
                        experimental_distance_position_error_span_scale:
                            crate::live_params::ExperimentalDistanceLiveParams::default()
                                .position_error_span_scale,
                    },
                    vbap_position_interpolation,
                    table_mode,
                    az_res_deg,
                    el_res_deg,
                    distance_step,
                    distance_max,
                    allow_negative_z,
                ),
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
        let (crossover_bands, crossover_filter_bank) =
            Self::build_crossover(&editable_layout, az_res_deg, el_res_deg, sample_rate);
        let control = RendererControl::new(
            live_params,
            topology,
            editable_layout,
            Some(crate::live_params::BackendRebuildParams {
                backend_id: RenderBackendKind::Vbap.as_str(),
                preferred_evaluation_mode,
                allow_negative_z,
                vbap: Some(crate::live_params::VbapModelRebuildParams {
                    az_res_deg,
                    el_res_deg,
                    spread_resolution,
                    distance_max,
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
            crossover_bands,
            crossover_filter_bank,
        ))
    }

    /// Create a new spatial renderer from a pre-loaded VBAP evaluation file
    ///
    /// This uses a serialized evaluation table directly, without constructing a VBAP backend.
    /// The loaded file becomes the active evaluator, which preserves the original lookup data
    /// and keeps the file-loading path independent from backend implementations.
    ///
    /// # Arguments
    ///
    /// * `loaded_file` - Pre-loaded VBAP evaluation file
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
    pub fn from_evaluation_artifact(
        artifact: LoadedEvaluationArtifact,
        sample_rate: u32,
        log_object_positions: bool,
        master_gain_db: f32,
        auto_gain: bool,
        use_loudness: bool,
    ) -> Result<Self> {
        let speaker_layout = artifact.speaker_layout().clone();
        let frozen = artifact.frozen_request().clone();
        let distance_model = DistanceModel::from_str(&frozen.distance_model)
            .map_err(|e| anyhow::anyhow!("Invalid frozen distance model in artifact: {}", e))?;
        let initial_evaluation_mode = match artifact.mode() {
            SerializedEvaluationMode::PrecomputedCartesian => {
                LiveEvaluationMode::PrecomputedCartesian
            }
            SerializedEvaluationMode::PrecomputedPolar => LiveEvaluationMode::PrecomputedPolar,
        };
        let (
            az_res_deg,
            el_res_deg,
            distance_res,
            distance_max,
            allow_negative_z,
            cartesian_x,
            cartesian_y,
            cartesian_z,
            cartesian_z_neg,
        ) = match artifact.mode() {
            SerializedEvaluationMode::PrecomputedCartesian => {
                let (x_count, y_count, z_count) =
                    artifact.cartesian_dimensions().unwrap_or((2, 2, 2));
                (
                    1,
                    1,
                    0.25,
                    2.0,
                    true,
                    x_count.max(1),
                    y_count.max(1),
                    z_count.max(1),
                    0,
                )
            }
            SerializedEvaluationMode::PrecomputedPolar => {
                let (az_count, el_count, distance_count) =
                    artifact.polar_dimensions().unwrap_or((2, 2, 2));
                let allow_negative_z = el_count > 2;
                let az_res_deg = (360.0 / az_count.max(1) as f32).round().max(1.0) as i32;
                let elevation_span = if allow_negative_z { 180.0 } else { 90.0 };
                let el_res_deg = (elevation_span / el_count.max(1) as f32).round().max(1.0) as i32;
                let distance_max = 2.0;
                let distance_res = distance_max / distance_count.max(1) as f32;
                (
                    az_res_deg,
                    el_res_deg,
                    distance_res,
                    distance_max,
                    allow_negative_z,
                    1,
                    1,
                    1,
                    0,
                )
            }
        };
        let spread_resolution = 0.0;
        let topology = RenderTopology::new(
            Arc::new(build_from_artifact_render_engine(artifact)),
            speaker_layout,
        )?;
        let excluded: Vec<&str> = topology
            .speaker_layout
            .speakers
            .iter()
            .filter(|s| !s.spatialize)
            .map(|s| s.name.as_str())
            .collect();
        let mut live_params = Self::build_live_params_and_log(
            &topology.speaker_layout,
            initial_evaluation_mode,
            az_res_deg,
            el_res_deg,
            distance_res,
            distance_max,
            allow_negative_z,
            topology
                .backend
                .capabilities()
                .supports_position_interpolation,
            cartesian_x,
            cartesian_y,
            cartesian_z,
            cartesian_z_neg,
            master_gain_db,
            frozen.spread_min,
            frozen.spread_max,
            frozen.spread_from_distance,
            frozen.spread_distance_range,
            frozen.spread_distance_curve,
            RampMode::Sample,
            use_loudness,
            distance_model,
            frozen.room_ratio,
            frozen.room_ratio_rear,
            frozen.room_ratio_lower,
            frozen.room_ratio_center_blend,
            frozen.use_distance_diffuse,
            frozen.distance_diffuse_threshold,
            frozen.distance_diffuse_curve,
            auto_gain,
            &excluded,
            &topology.bed_to_speaker_mapping,
        );
        live_params.backend_id = RenderBackendKind::FromFile.as_str().to_string();
        let editable_layout = topology.speaker_layout.clone();
        let control = RendererControl::new(live_params, topology, editable_layout, None);

        let layout = control.active_topology().speaker_layout.clone();
        let (crossover_bands, crossover_filter_bank) =
            Self::build_crossover(&layout, az_res_deg, el_res_deg, sample_rate);

        Ok(Self::finish_construction(
            layout.num_speakers(),
            spread_resolution,
            sample_rate,
            distance_model,
            log_object_positions,
            auto_gain,
            control,
            crossover_bands,
            crossover_filter_bank,
        ))
    }

    pub fn from_vbap_file(
        loaded_file: LoadedVbapFile,
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
        let spread_resolution = loaded_file.spread_resolution();
        let distance_step = if spread_resolution > 0.0 {
            spread_resolution
        } else {
            0.25
        };
        let vbap_num_speakers = loaded_file.num_speakers();
        let vbap_num_triangles = loaded_file.num_triangles();
        let vbap_table_mode = VbapTableMode::Polar;
        let vbap_azimuth_resolution = loaded_file.azimuth_resolution();
        let vbap_elevation_resolution = loaded_file.elevation_resolution();

        log::info!(
            "Created spatial renderer from pre-loaded VBAP table: {} total speakers, {} in VBAP table, {} triangles, spread_res={}, distance_model={}",
            num_speakers,
            vbap_num_speakers,
            vbap_num_triangles,
            spread_resolution,
            distance_model
        );
        let topology = RenderTopology::new(
            Arc::new(build_from_file_render_engine(
                loaded_file,
                allow_negative_z,
                vbap_position_interpolation,
            )),
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
        let editable_layout = topology.speaker_layout.clone();
        let control = RendererControl::new(live_params, topology, editable_layout, None);

        let layout = control.active_topology().speaker_layout.clone();
        let (crossover_bands, crossover_filter_bank) = Self::build_crossover(
            &layout,
            vbap_azimuth_resolution,
            vbap_elevation_resolution,
            sample_rate,
        );

        Ok(Self::finish_construction(
            num_speakers,
            spread_resolution,
            sample_rate,
            distance_model,
            log_object_positions,
            auto_gain,
            control,
            crossover_bands,
            crossover_filter_bank,
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
            backend_id: RenderBackendKind::Vbap.as_str().to_string(),
            evaluation: EvaluationLiveParams {
                mode: initial_evaluation_mode,
                position_interpolation: vbap_position_interpolation,
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
            experimental_distance: crate::live_params::ExperimentalDistanceLiveParams::default(),
            barycenter: crate::live_params::BarycenterLiveParams::default(),
        }
    }

    /// Build crossover band engines from a speaker layout.
    ///
    /// Returns `(bands, Some(filter_bank))` when the layout defines `freq_low` on
    /// at least one speaker (producing ≥ 2 bands), or `(single_band, None)` when
    /// no crossover is needed.
    fn build_crossover(
        layout: &crate::speaker_layout::SpeakerLayout,
        az_res_deg: i32,
        el_res_deg: i32,
        sample_rate: u32,
    ) -> (Vec<CrossoverBandEngine>, Option<LR4CrossoverBank>) {
        let bands = compute_bands(layout);
        if bands.len() <= 1 {
            let engines = bands
                .iter()
                .map(|b| CrossoverBandEngine::from_band(b, layout, az_res_deg, el_res_deg))
                .collect();
            return (engines, None);
        }

        let cutoffs: Vec<f32> = bands
            .windows(2)
            .map(|w| w[0].high_hz)
            .filter(|f| f.is_finite())
            .collect();

        let filter_bank = LR4CrossoverBank::new(&cutoffs, sample_rate);
        let engines = bands
            .iter()
            .map(|b| CrossoverBandEngine::from_band(b, layout, az_res_deg, el_res_deg))
            .collect();

        log::info!(
            "Crossover enabled: {} bands, cutoffs = {:?} Hz",
            bands.len(),
            cutoffs
        );

        (engines, Some(filter_bank))
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
        crossover_bands: Vec<CrossoverBandEngine>,
        crossover_filter_bank: Option<LR4CrossoverBank>,
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
            ramp_strategy_override: None,
            crossover_bands,
            crossover_filter_bank,
            crossover_filter_states: std::collections::HashMap::new(),
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

    pub fn set_ramp_strategy(&mut self, strategy: Arc<dyn RampStrategy>) {
        self.ramp_strategy_override = Some(strategy);
        self.reset_runtime_state();
    }

    pub fn clear_ramp_strategy(&mut self) {
        self.ramp_strategy_override = None;
        self.reset_runtime_state();
    }

    fn ramp_context<'a>(
        &self,
        topology_identity: usize,
        topology: &'a RenderTopology,
        live: &LiveSnapshot<'_>,
    ) -> RampContext<'a> {
        RampContext::new(
            topology.backend.as_ref(),
            topology_identity,
            RampRenderParams {
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
                barycenter_localize: live.barycenter.localize,
                experimental_distance_distance_floor: live.experimental_distance.distance_floor,
                experimental_distance_min_active_speakers: live
                    .experimental_distance
                    .min_active_speakers,
                experimental_distance_max_active_speakers: live
                    .experimental_distance
                    .max_active_speakers,
                experimental_distance_position_error_floor: live
                    .experimental_distance
                    .position_error_floor,
                experimental_distance_position_error_nearest_scale: live
                    .experimental_distance
                    .position_error_nearest_scale,
                experimental_distance_position_error_span_scale: live
                    .experimental_distance
                    .position_error_span_scale,
            },
        )
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
    fn update_metadata(
        &self,
        events: &[SpatialChannelEvent],
        strategy: &dyn RampStrategy,
        ctx: &RampContext<'_>,
    ) -> Result<()> {
        let mut channel_states = self.channel_states.lock().unwrap();

        for event in events {
            let state = channel_states
                .entry(event.channel_idx)
                .or_insert_with(ChannelState::default);
            state.ramp.ensure_speaker_count(ctx.speaker_count());

            if let Some(gain) = event.gain_db {
                state.gain_db = gain;
            }
            if let Some(ramp_length) = event.ramp_length {
                state.ramp.ramp_length = ramp_length as u64;
            }

            // Beds are routed directly to speakers — no position state needed.
            if event.is_bed {
                continue;
            }

            // Per-event spread is intentionally ignored.
            let spread_changed = false;

            if let Some(target_position) = event.position {
                if state.ramp.target_position != target_position || spread_changed {
                    let current_target_spread = state.ramp.target_spread;
                    let current_ramp_length = state.ramp.ramp_length;
                    if self.log_object_positions {
                        let remaining_units = state.ramp.remaining_ramp_units.unwrap_or(0);
                        let sample_pos = event.sample_pos.unwrap_or(0);
                        if state.ramp.target_position != target_position {
                            log::info!(
                                "  Obj ch{:2}: sample_pos {} remaining {} - Starting ramp over {} samples (~{}ms)",
                                event.channel_idx,
                                sample_pos,
                                remaining_units,
                                state.ramp.ramp_length,
                                state.ramp.ramp_length as f32 / self.sample_rate as f32 * 1000.0
                            );
                        }
                    }
                    strategy.update_target(
                        &mut state.ramp,
                        RampTarget {
                            position: target_position,
                            spread: current_target_spread,
                            ramp_length: current_ramp_length,
                        },
                        event.sample_pos,
                        ctx,
                    );
                }
            } else if spread_changed {
                if state.ramp.remaining_ramp_units.is_none() {
                    state.ramp.current_spread = state.ramp.target_spread;
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
                position_interpolation: g.evaluation.position_interpolation,
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
                barycenter: g.barycenter,
                experimental_distance: g.experimental_distance,
            }
        };

        // ── 2. Load the current immutable render topology (lock-free ArcSwap snapshot) ──
        let topology_guard = self.control.active_topology();
        let topology = &*topology_guard;
        let topology_identity = std::sync::Arc::as_ptr(&topology_guard) as usize;
        let ramp_context = self.ramp_context(topology_identity, topology, &live);
        let ramp_strategy_override = self.ramp_strategy_override.clone();
        static POSITION_STRATEGY: PositionRampStrategy = PositionRampStrategy;
        static GAIN_TABLE_STRATEGY: GainTableRampStrategy = GainTableRampStrategy;
        let ramp_strategy: &dyn RampStrategy = if let Some(ref strategy) = ramp_strategy_override {
            strategy.as_ref()
        } else if live.position_interpolation {
            &POSITION_STRATEGY
        } else {
            &GAIN_TABLE_STRATEGY
        };

        if !pending_events.is_empty() {
            self.update_metadata(pending_events, ramp_strategy, &ramp_context)?;
        }

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
                state
                    .ramp
                    .ensure_speaker_count(ramp_context.speaker_count());

                // ── Crossover path ──────────────────────────────────────────────────────
                // When freq_low is defined on speakers, split each object into frequency
                // bands and pan each band through its own VBAP topology.
                if self.crossover_filter_bank.is_some() {
                    let render_params = ramp_context.render_params();

                    // Ensure per-object filter state is allocated
                    let state_count = self.crossover_filter_bank.as_ref().unwrap().state_count();
                    let obj_filter_states = self.crossover_filter_states
                        .entry(input_channel_idx)
                        .or_insert_with(|| vec![BiquadState::default(); state_count]);

                    // Borrow filter bank (different field from crossover_filter_states)
                    let filter_bank = self.crossover_filter_bank.as_ref().unwrap();

                    match live.ramp_mode {
                        RampMode::Off => {
                            state.ramp.remaining_ramp_units = None;
                            state.ramp.start_position = state.ramp.target_position;
                            state.ramp.current_position = state.ramp.target_position;
                            state.ramp.current_spread = state.ramp.target_spread;
                            state.ramp.output_position = state.ramp.target_position;

                            let position = state.ramp.output_position;
                            let band_gains: Vec<Gains> = self.crossover_bands.iter()
                                .map(|b| b.compute_gains(render_params, position))
                                .collect();

                            for sample_idx in 0..sample_length {
                                let raw = input_pcm
                                    [sample_idx * input_channel_count + input_channel_idx]
                                    * gain_linear
                                    * obj_gain;
                                let bands = filter_bank.process_sample(raw, obj_filter_states);
                                let out_base = sample_idx * self.num_speakers;
                                for (b, band) in self.crossover_bands.iter().enumerate() {
                                    for (gi, &g) in band_gains[b].iter().enumerate() {
                                        output[out_base + band.speaker_indices[gi]] +=
                                            bands.get(b) * g;
                                    }
                                }
                            }
                        }
                        RampMode::Frame => {
                            let progress =
                                state.ramp.current_progress().unwrap_or(RampProgress {
                                    completed_units: 0,
                                    total_units: 0,
                                });
                            ramp_strategy.evaluate(&mut state.ramp, progress, &ramp_context);
                            let position = state.ramp.output_position;
                            let band_gains: Vec<Gains> = self.crossover_bands.iter()
                                .map(|b| b.compute_gains(render_params, position))
                                .collect();

                            for sample_idx in 0..sample_length {
                                let raw = input_pcm
                                    [sample_idx * input_channel_count + input_channel_idx]
                                    * gain_linear
                                    * obj_gain;
                                let bands = filter_bank.process_sample(raw, obj_filter_states);
                                let out_base = sample_idx * self.num_speakers;
                                for (b, band) in self.crossover_bands.iter().enumerate() {
                                    for (gi, &g) in band_gains[b].iter().enumerate() {
                                        output[out_base + band.speaker_indices[gi]] +=
                                            bands.get(b) * g;
                                    }
                                }
                            }

                            state.ramp.commit_output_position();
                            state.ramp.advance_ramp(sample_length as u64);
                        }
                        RampMode::Sample => {
                            for sample_idx in 0..sample_length {
                                let progress =
                                    state.ramp.current_progress().unwrap_or(RampProgress {
                                        completed_units: 0,
                                        total_units: 0,
                                    });
                                ramp_strategy.evaluate(&mut state.ramp, progress, &ramp_context);
                                let position = state.ramp.output_position;
                                let band_gains: Vec<Gains> = self.crossover_bands.iter()
                                    .map(|b| b.compute_gains(render_params, position))
                                    .collect();

                                let raw = input_pcm
                                    [sample_idx * input_channel_count + input_channel_idx]
                                    * gain_linear
                                    * obj_gain;
                                let bands = filter_bank.process_sample(raw, obj_filter_states);
                                let out_base = sample_idx * self.num_speakers;
                                for (b, band) in self.crossover_bands.iter().enumerate() {
                                    for (gi, &g) in band_gains[b].iter().enumerate() {
                                        output[out_base + band.speaker_indices[gi]] +=
                                            bands.get(b) * g;
                                    }
                                }

                                state.ramp.commit_output_position();
                                state.ramp.advance_ramp(1);
                            }
                        }
                    }

                    object_gains_out.push((input_channel_idx, Gains::zeroed(self.num_speakers)));
                    continue;
                }
                // ── End crossover path ───────────────────────────────────────────────────

                let log_object_snapshot = |rendering_position: [f64; 3], final_gains: &Gains| {
                    if !self.log_object_positions {
                        return;
                    }

                    let gains_with_names: Vec<String> = final_gains
                        .iter()
                        .enumerate()
                        .map(|(idx, g)| {
                            let speaker_idx =
                                if let Some(mapping) = active_backend_to_speaker_mapping {
                                    mapping[idx]
                                } else {
                                    idx
                                };
                            format!(
                                "{}={:.3}",
                                active_speaker_names
                                    .as_ref()
                                    .expect("speaker names available for logging")[speaker_idx],
                                g
                            )
                        })
                        .collect();
                    let active_speakers = final_gains.iter().filter(|&&g| g > 0.01).count();
                    let spread_indicator = if live.spread_from_distance { "d" } else { "" };
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
                };

                let push_monitor_gains = |out: &mut Vec<(usize, Gains)>, final_gains: &Gains| {
                    if let Some(mapping) = active_backend_to_speaker_mapping {
                        let mut gains = Gains::zeroed(self.num_speakers);
                        for (backend_idx, &gain) in final_gains.iter().enumerate() {
                            gains.set(mapping[backend_idx], gain);
                        }
                        out.push((input_channel_idx, gains));
                    } else {
                        let mut gains = Gains::zeroed(self.num_speakers);
                        for (speaker_idx, &gain) in final_gains.iter().enumerate() {
                            gains.set(speaker_idx, gain);
                        }
                        out.push((input_channel_idx, gains));
                    }
                };

                if matches!(live.ramp_mode, RampMode::Off) {
                    state.ramp.remaining_ramp_units = None;
                    state.ramp.start_position = state.ramp.target_position;
                    state.ramp.current_position = state.ramp.target_position;
                    state.ramp.current_spread = state.ramp.target_spread;
                    state.ramp.output_position = state.ramp.target_position;
                    state.ramp.output_gains =
                        ramp_context.compute_gains(state.ramp.target_position);
                    state.ramp.start_gains = state.ramp.output_gains.clone();
                    state.ramp.target_gains = state.ramp.output_gains.clone();

                    let final_gains = state.ramp.output_gains();
                    log_object_snapshot(state.ramp.output_position, final_gains);
                    for sample_idx in 0..sample_length {
                        let object_sample = input_pcm
                            [sample_idx * input_channel_count + input_channel_idx]
                            * gain_linear
                            * obj_gain;
                        let out_base = sample_idx * self.num_speakers;
                        if let Some(mapping) = active_backend_to_speaker_mapping {
                            for (backend_idx, &gain) in final_gains.iter().enumerate() {
                                let speaker_idx = mapping[backend_idx];
                                output[out_base + speaker_idx] += object_sample * gain;
                            }
                        } else {
                            for (speaker_idx, &gain) in final_gains.iter().enumerate() {
                                output[out_base + speaker_idx] += object_sample * gain;
                            }
                        }
                    }
                    push_monitor_gains(&mut object_gains_out, final_gains);
                    continue;
                }

                if let Some(progress) = state.ramp.current_progress() {
                    match live.ramp_mode {
                        RampMode::Frame => {
                            let eval_status =
                                ramp_strategy.evaluate(&mut state.ramp, progress, &ramp_context);
                            let rendering_position = state.ramp.output_position;
                            let final_gains = state.ramp.output_gains().clone();

                            if self.log_object_positions
                                && matches!(eval_status, RampStatus::Finished)
                            {
                                log_object_snapshot(rendering_position, &final_gains);
                            }

                            for sample_idx in 0..sample_length {
                                let object_sample = input_pcm
                                    [sample_idx * input_channel_count + input_channel_idx]
                                    * gain_linear
                                    * obj_gain;
                                let out_base = sample_idx * self.num_speakers;
                                if let Some(mapping) = active_backend_to_speaker_mapping {
                                    for (backend_idx, &gain) in final_gains.iter().enumerate() {
                                        let speaker_idx = mapping[backend_idx];
                                        output[out_base + speaker_idx] += object_sample * gain;
                                    }
                                } else {
                                    for (speaker_idx, &gain) in final_gains.iter().enumerate() {
                                        output[out_base + speaker_idx] += object_sample * gain;
                                    }
                                }
                            }

                            state.ramp.commit_output_position();
                            let final_status = state.ramp.advance_ramp(sample_length as u64);
                            if self.log_object_positions
                                && !matches!(eval_status, RampStatus::Finished)
                                && matches!(final_status, RampStatus::Finished)
                            {
                                log_object_snapshot(state.ramp.target_position, &final_gains);
                            }
                            push_monitor_gains(&mut object_gains_out, &final_gains);
                        }
                        RampMode::Sample => {
                            for sample_idx in 0..sample_length {
                                let progress =
                                    state.ramp.current_progress().unwrap_or(RampProgress {
                                        completed_units: 0,
                                        total_units: 0,
                                    });
                                let eval_status = ramp_strategy.evaluate(
                                    &mut state.ramp,
                                    progress,
                                    &ramp_context,
                                );
                                let final_gains = state.ramp.output_gains().clone();
                                let object_sample = input_pcm
                                    [sample_idx * input_channel_count + input_channel_idx]
                                    * gain_linear
                                    * obj_gain;
                                let out_base = sample_idx * self.num_speakers;

                                if let Some(mapping) = active_backend_to_speaker_mapping {
                                    for (backend_idx, &gain) in final_gains.iter().enumerate() {
                                        let speaker_idx = mapping[backend_idx];
                                        output[out_base + speaker_idx] += object_sample * gain;
                                    }
                                } else {
                                    for (speaker_idx, &gain) in final_gains.iter().enumerate() {
                                        output[out_base + speaker_idx] += object_sample * gain;
                                    }
                                }

                                state.ramp.commit_output_position();
                                let final_status = state.ramp.advance_ramp(1);

                                if self.log_object_positions
                                    && sample_idx == sample_length - 1
                                    && (matches!(eval_status, RampStatus::Finished)
                                        || matches!(final_status, RampStatus::Finished))
                                {
                                    log_object_snapshot(state.ramp.output_position, &final_gains);
                                }

                                if sample_idx == sample_length - 1 {
                                    push_monitor_gains(&mut object_gains_out, &final_gains);
                                }
                            }
                        }
                        RampMode::Off => unreachable!(),
                    }
                } else {
                    let eval_status = ramp_strategy.evaluate(
                        &mut state.ramp,
                        RampProgress {
                            completed_units: 0,
                            total_units: 0,
                        },
                        &ramp_context,
                    );
                    let rendering_position = state.ramp.output_position;
                    let final_gains = state.ramp.output_gains();

                    if self.log_object_positions && matches!(eval_status, RampStatus::Finished) {
                        log_object_snapshot(rendering_position, final_gains);
                    }

                    for sample_idx in 0..sample_length {
                        let object_sample = input_pcm
                            [sample_idx * input_channel_count + input_channel_idx]
                            * gain_linear
                            * obj_gain;
                        let out_base = sample_idx * self.num_speakers;
                        if let Some(mapping) = active_backend_to_speaker_mapping {
                            for (backend_idx, &gain) in final_gains.iter().enumerate() {
                                let speaker_idx = mapping[backend_idx];
                                output[out_base + speaker_idx] += object_sample * gain;
                            }
                        } else {
                            for (speaker_idx, &gain) in final_gains.iter().enumerate() {
                                output[out_base + speaker_idx] += object_sample * gain;
                            }
                        }
                    }

                    push_monitor_gains(&mut object_gains_out, final_gains);
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
            15,
        );

        assert!(renderer.is_ok());

        let renderer = renderer.unwrap();
        assert_eq!(renderer.num_speakers(), 12);
    }

    // TODO: Add integration test with real spatial metadata
    // For now, testing is done via real spatial audio content decoding
}
