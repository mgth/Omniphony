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

use crate::render_backend::{GainModelKind, PreparedRenderEngine, RenderBackendKind};
#[cfg(feature = "saf_vbap")]
use crate::render_backend::{GainModelInstance, build_prepared_render_engine};
use crate::spatial_vbap::VbapTableMode;
use crate::speaker_layout::SpeakerLayout;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveVbapTableMode {
    Auto,
    Polar,
    Cartesian,
}

impl LiveVbapTableMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Polar => "polar",
            Self::Cartesian => "cartesian",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "polar" => Some(Self::Polar),
            "cartesian" => Some(Self::Cartesian),
            _ => None,
        }
    }
}

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

    pub fn from_vbap_table_mode(mode: LiveVbapTableMode) -> Self {
        match mode {
            LiveVbapTableMode::Auto => Self::Auto,
            LiveVbapTableMode::Polar => Self::PrecomputedPolar,
            LiveVbapTableMode::Cartesian => Self::PrecomputedCartesian,
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
    /// Mute flag — independent of `gain`; unmuting restores the stored value.
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

/// Live-tunable rendering parameters.
///
/// Written (exclusively) by the OSC listener thread, read via snapshot by the
/// render thread.
pub struct LiveParams {
    /// Master output gain, linear scale (1.0 = unity, 0.5 ≈ −6 dB).
    pub master_gain: f32,

    /// Per-object live parameters: gain and mute.
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

    /// Ramp processing mode for object moves and gain transitions.
    pub ramp_mode: RampMode,

    /// Requested spatial render backend.
    pub backend_kind: RenderBackendKind,

    /// Cartesian VBAP table size on X axis (live-editable via OSC).
    pub vbap_cart_x_size: usize,

    /// Cartesian VBAP table size on Y axis (live-editable via OSC).
    pub vbap_cart_y_size: usize,

    /// Cartesian VBAP table size on Z axis (live-editable via OSC).
    pub vbap_cart_z_size: usize,

    /// Cartesian VBAP table size on negative Z axis (live-editable via OSC).
    pub vbap_cart_z_neg_size: usize,

    /// Requested evaluation mode for the current gain model.
    pub evaluation_mode: LiveEvaluationMode,

    /// Legacy compatibility mirror for the old VBAP table-mode UI/OSC surface.
    /// Keep in sync with `evaluation_mode` while the old protocol still exists.
    pub vbap_table_mode: LiveVbapTableMode,

    /// Polar VBAP azimuth granularity in degrees.
    pub vbap_polar_azimuth_values: i32,

    /// Polar VBAP elevation granularity in degrees.
    pub vbap_polar_elevation_values: i32,

    /// VBAP distance-table granularity as number of values across full distance range.
    pub vbap_polar_distance_res: i32,

    /// Maximum distance covered by polar VBAP precomputed table.
    pub vbap_polar_distance_max: f32,

    /// Interpolate between neighbouring VBAP table positions during lookup.
    pub vbap_position_interpolation: bool,

    /// Apply dialogue normalisation gain stored in the renderer.
    pub use_loudness: bool,

    /// Distance attenuation model currently applied by the renderer.
    pub distance_model: crate::spatial_vbap::DistanceModel,

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
}

impl LiveParams {
    pub fn set_evaluation_mode(&mut self, mode: LiveEvaluationMode) {
        self.evaluation_mode = mode;
        self.vbap_table_mode = match mode {
            LiveEvaluationMode::Auto => LiveVbapTableMode::Auto,
            LiveEvaluationMode::PrecomputedPolar => LiveVbapTableMode::Polar,
            LiveEvaluationMode::PrecomputedCartesian => LiveVbapTableMode::Cartesian,
            LiveEvaluationMode::Realtime => self.vbap_table_mode,
        };
    }

    pub fn gain_model_kind(&self) -> GainModelKind {
        self.backend_kind.as_gain_model_kind()
    }

    pub fn requested_evaluation_mode(&self) -> LiveEvaluationMode {
        match self.gain_model_kind() {
            GainModelKind::Vbap => self.evaluation_mode,
            GainModelKind::ExperimentalDistance => LiveEvaluationMode::Realtime,
        }
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
    pub position_interpolation: bool,
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
    pub gain_model_kind: GainModelKind,
    pub preferred_evaluation_mode: PreferredEvaluationMode,
    pub allow_negative_z: bool,
    pub vbap: Option<VbapModelRebuildParams>,
}

impl BackendRebuildParams {
    pub fn preferred_evaluation_mode(&self) -> PreferredEvaluationMode {
        self.preferred_evaluation_mode
    }
}

#[cfg(feature = "saf_vbap")]
#[derive(Clone)]
pub enum GainModelBuildPlan {
    Vbap(VbapTopologyBuildPlan),
    ExperimentalDistance { speaker_positions: Vec<[f32; 3]> },
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
}

#[cfg(feature = "saf_vbap")]
#[derive(Clone)]
pub struct VbapTopologyBuildPlan {
    pub layout: SpeakerLayout,
    pub positions: Vec<[f32; 2]>,
    pub azimuth_resolution: i32,
    pub elevation_resolution: i32,
    pub distance_res: f32,
    pub distance_max: f32,
    pub position_interpolation: bool,
    pub table_mode: VbapTableMode,
    pub allow_negative_z: bool,
    pub distance_model: crate::spatial_vbap::DistanceModel,
    pub spread_min: f32,
    pub spread_max: f32,
    pub spread_from_distance: bool,
    pub spread_distance_range: f32,
    pub spread_distance_curve: f32,
    pub room_ratio: [f32; 3],
    pub room_ratio_rear: f32,
    pub room_ratio_lower: f32,
    pub room_ratio_center_blend: f32,
    pub diffuse: bool,
    pub diffuse_thr: f32,
    pub diffuse_curve: f32,
}

#[cfg(feature = "saf_vbap")]
impl VbapTopologyBuildPlan {
    pub fn build_topology(&self) -> Result<RenderTopology> {
        let vbap = crate::spatial_vbap::VbapPanner::new_with_mode(
            &self.positions,
            self.azimuth_resolution,
            self.elevation_resolution,
            0.0,
            self.table_mode,
        )
        .map_err(|e| anyhow::anyhow!("Failed to create VBAP panner: {}", e))?
        .with_negative_z(self.allow_negative_z)
        .with_position_interpolation(self.position_interpolation)
        .precompute_effect_tables(
            self.distance_res,
            self.distance_max,
            self.spread_min,
            self.spread_max,
            self.distance_model,
            self.spread_from_distance,
            self.spread_distance_range,
            self.spread_distance_curve,
            self.diffuse,
            self.diffuse_thr,
            self.diffuse_curve,
            self.room_ratio,
            self.room_ratio_rear,
            self.room_ratio_lower,
            self.room_ratio_center_blend,
        )
        .map_err(|e| anyhow::anyhow!("Failed to precompute VBAP effect tables: {}", e))?;

        RenderTopology::new(
            Arc::new(build_prepared_render_engine(
                GainModelInstance::Vbap(crate::render_backend::VbapBackend::new(vbap)),
                match self.table_mode {
                    VbapTableMode::Polar => crate::render_backend::EffectiveEvaluationMode::PrecomputedPolar,
                    VbapTableMode::Cartesian { .. } => {
                        crate::render_backend::EffectiveEvaluationMode::PrecomputedCartesian
                    }
                },
            )?),
            self.layout.clone(),
        )
    }
}

#[cfg(feature = "saf_vbap")]
#[derive(Clone)]
pub struct TopologyBuildPlan {
    pub layout: SpeakerLayout,
    pub gain_model: GainModelBuildPlan,
    pub evaluation_mode: LiveEvaluationMode,
}

#[cfg(feature = "saf_vbap")]
impl TopologyBuildPlan {
    pub fn build_topology(&self) -> Result<RenderTopology> {
        match &self.gain_model {
            GainModelBuildPlan::Vbap(plan) => plan.build_topology(),
            GainModelBuildPlan::ExperimentalDistance { speaker_positions } => RenderTopology::new(
                Arc::new(build_prepared_render_engine(
                    GainModelInstance::ExperimentalDistance(
                        crate::render_backend::ExperimentalDistanceBackend::new(
                            speaker_positions.clone(),
                        ),
                    ),
                    crate::render_backend::EffectiveEvaluationMode::Realtime,
                )?),
                self.layout.clone(),
            ),
        }
    }

    pub fn backend_kind(&self) -> RenderBackendKind {
        match self.gain_model {
            GainModelBuildPlan::Vbap(_) => RenderBackendKind::Vbap,
            GainModelBuildPlan::ExperimentalDistance { .. } => RenderBackendKind::ExperimentalDistance,
        }
    }

    pub fn gain_model_kind(&self) -> GainModelKind {
        self.backend_kind().as_gain_model_kind()
    }

    pub fn evaluation_mode(&self) -> LiveEvaluationMode {
        self.evaluation_mode
    }

    pub fn layout(&self) -> &SpeakerLayout {
        &self.layout
    }

    pub fn log_summary(&self) -> String {
        match &self.gain_model {
            GainModelBuildPlan::Vbap(plan) => format!(
                "gain_model=vbap evaluation_mode={} azimuth_resolution={} elevation_resolution={} distance_res={} distance_max={} mode={:?}",
                self.evaluation_mode().as_str(),
                plan.azimuth_resolution,
                plan.elevation_resolution,
                plan.distance_res,
                plan.distance_max,
                plan.table_mode
            ),
            GainModelBuildPlan::ExperimentalDistance { speaker_positions } => format!(
                "gain_model=experimental_distance evaluation_mode={} speakers={}",
                self.evaluation_mode().as_str(),
                speaker_positions.len()
            ),
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
    pub backend_rebuild_params: Option<BackendRebuildParams>,

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
    /// Requested ramp mode from OSC control.
    pub requested_ramp_mode: Mutex<RampMode>,
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
            backend_rebuild_params,
            recomputing: AtomicBool::new(false),
            config_dirty: AtomicBool::new(false),
            object_params_generation: std::sync::atomic::AtomicU64::new(1),
            speaker_params_generation: std::sync::atomic::AtomicU64::new(1),
            config_path: Mutex::new(None),
            input_path: Mutex::new(None),
            requested_ramp_mode: Mutex::new(RampMode::Sample),
        })
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

    pub fn mark_object_params_dirty(&self) {
        self.object_params_generation
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn mark_speaker_params_dirty(&self) {
        self.speaker_params_generation
            .fetch_add(1, Ordering::Relaxed);
    }

    #[cfg(feature = "saf_vbap")]
    pub fn prepare_topology_rebuild(&self) -> Option<TopologyBuildPlan> {
        let layout = self.editable_layout();
        let live = self.live.read().unwrap();
        if live.gain_model_kind() == GainModelKind::ExperimentalDistance {
            let speaker_positions = layout
                .speakers
                .iter()
                .filter(|speaker| speaker.spatialize)
                .map(|speaker| [speaker.x, speaker.y, speaker.z])
                .collect();
            return Some(TopologyBuildPlan {
                layout,
                gain_model: GainModelBuildPlan::ExperimentalDistance { speaker_positions },
                evaluation_mode: LiveEvaluationMode::Realtime,
            });
        }

        let rebuild_params = self.backend_rebuild_params?;
        let preferred_evaluation_mode = rebuild_params.preferred_evaluation_mode();
        let rebuild = rebuild_params.vbap?;
        let positions = layout
            .spatializable_positions_for_room(
                live.room_ratio,
                live.room_ratio_rear,
                live.room_ratio_lower,
                live.room_ratio_center_blend,
            )
            .0;

        let table_mode = match live.evaluation_mode {
            LiveEvaluationMode::Realtime => return None,
            LiveEvaluationMode::Auto => match preferred_evaluation_mode {
                PreferredEvaluationMode::PrecomputedPolar => crate::spatial_vbap::VbapTableMode::Polar,
                PreferredEvaluationMode::PrecomputedCartesian => crate::spatial_vbap::VbapTableMode::Cartesian {
                    x_size: live
                        .vbap_cart_x_size
                        .max(rebuild.cartesian_default_x_size)
                        .max(1)
                        + 1,
                    y_size: live
                        .vbap_cart_y_size
                        .max(rebuild.cartesian_default_y_size)
                        .max(1)
                        + 1,
                    z_size: live
                        .vbap_cart_z_size
                        .max(rebuild.cartesian_default_z_size)
                        .max(1)
                        + 1,
                    z_neg_size: live
                        .vbap_cart_z_neg_size
                        .max(rebuild.cartesian_default_z_neg_size),
                },
            },
            LiveEvaluationMode::PrecomputedPolar => crate::spatial_vbap::VbapTableMode::Polar,
            LiveEvaluationMode::PrecomputedCartesian => crate::spatial_vbap::VbapTableMode::Cartesian {
                x_size: live
                    .vbap_cart_x_size
                    .max(rebuild.cartesian_default_x_size)
                    .max(1)
                    + 1,
                y_size: live
                    .vbap_cart_y_size
                    .max(rebuild.cartesian_default_y_size)
                    .max(1)
                    + 1,
                z_size: live
                    .vbap_cart_z_size
                    .max(rebuild.cartesian_default_z_size)
                    .max(1)
                    + 1,
                z_neg_size: live
                    .vbap_cart_z_neg_size
                    .max(rebuild.cartesian_default_z_neg_size),
            },
        };
        let azimuth_resolution = if live.vbap_polar_azimuth_values > 0 {
            ((360.0f32 / (live.vbap_polar_azimuth_values as f32)).round() as i32).clamp(1, 360)
        } else {
            rebuild.az_res_deg.clamp(1, 360)
        };
        let elevation_resolution = if live.vbap_polar_elevation_values > 0 {
            (((if rebuild.allow_negative_z {
                180.0
            } else {
                90.0
            }) / (live.vbap_polar_elevation_values as f32))
                .round() as i32)
                .clamp(1, if rebuild.allow_negative_z { 180 } else { 90 })
        } else {
            rebuild
                .el_res_deg
                .clamp(1, if rebuild.allow_negative_z { 180 } else { 90 })
        };
        let distance_max = if live.vbap_polar_distance_max > 0.0 {
            live.vbap_polar_distance_max
        } else {
            rebuild.distance_max.max(0.01)
        };
        let distance_res = if live.vbap_polar_distance_res > 0 {
            distance_max / (live.vbap_polar_distance_res as f32)
        } else if rebuild.spread_resolution > 0.0 {
            rebuild.spread_resolution
        } else {
            0.25
        };

        let evaluation_mode = match table_mode {
            VbapTableMode::Polar => LiveEvaluationMode::PrecomputedPolar,
            VbapTableMode::Cartesian { .. } => LiveEvaluationMode::PrecomputedCartesian,
        };

        Some(TopologyBuildPlan {
            layout: layout.clone(),
            gain_model: GainModelBuildPlan::Vbap(VbapTopologyBuildPlan {
                layout,
                positions,
                azimuth_resolution,
                elevation_resolution,
                distance_res,
                distance_max,
                position_interpolation: live.vbap_position_interpolation,
                table_mode,
                allow_negative_z: rebuild.allow_negative_z,
                distance_model: live.distance_model,
                spread_min: live.spread_min,
                spread_max: live.spread_max,
                spread_from_distance: live.spread_from_distance,
                spread_distance_range: live.spread_distance_range,
                spread_distance_curve: live.spread_distance_curve,
                room_ratio: live.room_ratio,
                room_ratio_rear: live.room_ratio_rear,
                room_ratio_lower: live.room_ratio_lower,
                room_ratio_center_blend: live.room_ratio_center_blend,
                diffuse: live.use_distance_diffuse,
                diffuse_thr: live.distance_diffuse_threshold,
                diffuse_curve: live.distance_diffuse_curve,
            }),
            evaluation_mode,
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
        *self.input_path.lock().unwrap() = input_path;
    }

    pub fn input_path(&self) -> Option<String> {
        self.input_path.lock().unwrap().clone()
    }

    pub fn set_requested_ramp_mode(&self, mode: RampMode) {
        *self.requested_ramp_mode.lock().unwrap() = mode;
    }

    pub fn requested_ramp_mode(&self) -> RampMode {
        *self.requested_ramp_mode.lock().unwrap()
    }
}
