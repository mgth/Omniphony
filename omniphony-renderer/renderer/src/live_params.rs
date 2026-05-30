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
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use crate::backend_registry::{TopologyBuildPlan, prepare_topology_build_plan};
use crate::render_backend::backend_descriptor_by_id;
use crate::render_backend::{
    EvaluationBuildConfig, GainModelKind, PreparedRenderEngine, RenderBackendKind, RenderRequest,
};
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
}

impl RampMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Frame => "frame",
            Self::Sample => "sample",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "frame" | "per_frame" => Some(Self::Frame),
            "sample" | "per_sample" => Some(Self::Sample),
            _ => None,
        }
    }
}

/// Live-tunable parameters for a single input object (bed or audio object).
#[derive(Clone)]
pub struct ObjectLiveParams {
    /// Linear gain override (default 1.0 = unity).
    pub gain: f32,
    /// Mute flag; when true the object is silenced.
    pub muted: bool,
}

impl Default for ObjectLiveParams {
    fn default() -> Self {
        Self {
            gain: 1.0,
            muted: false,
        }
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

    /// Per-object live parameters (gain, mute).
    /// Absent entries use `ObjectLiveParams::default()` (gain=1.0, muted=false).
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

    /// Enable distance-based antipodal diffuse blending.
    ///
    /// When active, each object's VBAP gains are blended with the gains of the
    /// antipodal point `(-x, -y, z)` (same elevation, opposite horizontal direction).
    /// The mix is controlled by the ADM distance (pre-room_ratio):
    ///   - dist = 0  →  50 % direct + 50 % mirror  (iso-energy weights: √0.5 each)
    ///   - dist ≥ `distance_diffuse_threshold`  →  100 % direct
    pub use_distance_diffuse: bool,

    /// ADM distance at which the blend reaches 100 % direct.  Default: 1.0.
    pub distance_diffuse_threshold: f32,

    /// Curve exponent applied to the normalised distance before computing the
    /// blend weight.  1.0 = linear, < 1 = fast-near, > 1 = slow-near.  Default: 1.0.
    pub distance_diffuse_curve: f32,

    /// Runtime tuning parameters for the experimental distance backend.
    pub experimental_distance: ExperimentalDistanceLiveParams,

    /// Runtime tuning parameters for the barycenter backend.
    pub barycenter: BarycenterLiveParams,

    /// Runtime tuning parameters for the hybrid backend.
    pub hybrid: HybridLiveParams,

    /// Selected Dynamic Range Control mode (as string).
    pub drc_mode: String,
    /// DRC weighting in [0.0, 1.0]. 1.0 applies the full bridge-decoded DRC gain;
    /// 0.0 bypasses it entirely. Intermediate values scale the dB reduction
    /// linearly (effective_gain = bridge_gain.powf(drc_weight)).
    pub drc_weight: f32,
}

impl LiveParams {
    pub fn set_evaluation_mode(&mut self, mode: LiveEvaluationMode) {
        self.evaluation.mode = mode;
    }

    pub fn backend_id(&self) -> &str {
        self.backend_id.as_str()
    }

    pub fn backend_kind(&self) -> Option<RenderBackendKind> {
        RenderBackendKind::from_str(self.backend_id())
    }

    pub fn gain_model_kind(&self) -> GainModelKind {
        backend_descriptor_by_id(self.backend_id())
            .map(|descriptor| descriptor.gain_model_kind)
            .unwrap_or(GainModelKind::Vbap)
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
    pub table_mode: VbapTableMode,
    pub cartesian_default_x_size: usize,
    pub cartesian_default_y_size: usize,
    pub cartesian_default_z_size: usize,
    pub cartesian_default_z_neg_size: usize,
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

    pub fn gain_model_kind(&self) -> GainModelKind {
        backend_descriptor_by_id(self.backend_id)
            .map(|descriptor| descriptor.gain_model_kind)
            .unwrap_or(GainModelKind::Vbap)
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
            size_to_spread_mode: live.size_to_spread_mode,
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
            distance_model: live.distance_model,
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
    pub bed_to_speaker_mapping: HashMap<usize, usize>,
    pub num_speakers: usize,
    pub num_spatializable: usize,
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
            bed_to_speaker_mapping: speaker_layout.bed_to_speaker_mapping(),
            num_speakers,
            num_spatializable,
            speaker_layout,
            backend,
            backend_to_speaker_mapping,
        })
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

    /// Path of the active config file, used by the save-config handler.
    /// Set after construction via `set_config_path()`.
    pub config_path: Mutex<Option<PathBuf>>,

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
            config_path: Mutex::new(None),
            input_path: Mutex::new(None),
            bridge_path: Mutex::new(None),
            bridge_supported_drc_modes: Mutex::new(Vec::new()),
            requested_ramp_mode: Mutex::new(RampMode::Frame),
            // Seeded from config (or a host default) after construction.
            meter_rate_hz_bits: Arc::new(std::sync::atomic::AtomicU32::new(50.0_f32.to_bits())),
            diag_rate_hz_bits: Arc::new(std::sync::atomic::AtomicU32::new(50.0_f32.to_bits())),
        })
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
        *self.config_path.lock().unwrap() = Some(path);
    }

    pub fn active_topology(&self) -> Arc<RenderTopology> {
        self.topology.load_full()
    }

    pub fn active_layout(&self) -> SpeakerLayout {
        self.active_topology().speaker_layout.clone()
    }

    pub fn editable_layout(&self) -> SpeakerLayout {
        self.editable_layout.lock().unwrap().clone()
    }

    pub fn with_editable_layout<R>(&self, f: impl FnOnce(&mut SpeakerLayout) -> R) -> R {
        let mut layout = self.editable_layout.lock().unwrap();
        f(&mut layout)
    }

    pub fn publish_topology(&self, topology: RenderTopology) {
        self.topology.store(Arc::new(topology));
    }

    pub fn backend_rebuild_params(&self) -> Option<BackendRebuildParams> {
        *self.backend_rebuild_params.read().unwrap()
    }

    pub fn set_backend_rebuild_params(&self, params: Option<BackendRebuildParams>) {
        *self.backend_rebuild_params.write().unwrap() = params;
    }

    pub fn mark_object_params_dirty(&self) {
        self.object_params_generation
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn mark_speaker_params_dirty(&self) {
        self.speaker_params_generation
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn prepare_topology_rebuild(&self) -> Option<TopologyBuildPlan> {
        let layout = self.editable_layout();
        self.prepare_topology_rebuild_for_layout(layout)
    }

    pub fn prepare_topology_rebuild_for_layout(
        &self,
        layout: SpeakerLayout,
    ) -> Option<TopologyBuildPlan> {
        let live = self.live.read().unwrap();
        let backend_rebuild_params = self.backend_rebuild_params();
        let evaluation_build_config = evaluation_build_config_from_live(
            &live,
            rebuild_params_allow_negative_z(backend_rebuild_params),
        );
        prepare_topology_build_plan(
            layout,
            &live,
            backend_rebuild_params,
            evaluation_build_config,
        )
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
        *self.input_path.lock().unwrap() = input_path;
    }

    pub fn input_path(&self) -> Option<String> {
        self.input_path.lock().unwrap().clone()
    }

    pub fn set_bridge_path(&self, bridge_path: Option<PathBuf>) {
        *self.bridge_path.lock().unwrap() = bridge_path;
    }

    pub fn bridge_path(&self) -> Option<PathBuf> {
        self.bridge_path.lock().unwrap().clone()
    }

    pub fn set_bridge_supported_drc_modes(&self, modes: Vec<String>) {
        *self.bridge_supported_drc_modes.lock().unwrap() = modes;
    }

    pub fn bridge_supported_drc_modes(&self) -> Vec<String> {
        self.bridge_supported_drc_modes.lock().unwrap().clone()
    }

    pub fn set_requested_ramp_mode(&self, mode: RampMode) {
        *self.requested_ramp_mode.lock().unwrap() = mode;
    }

    pub fn requested_ramp_mode(&self) -> RampMode {
        *self.requested_ramp_mode.lock().unwrap()
    }
}
