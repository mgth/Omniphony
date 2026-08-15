mod barycenter_backend;
mod degenerate_vbap_backend;
mod distance_attenuation;
mod distance_diffuse;
mod evaluation_artifact;
mod experimental_distance_backend;
mod hybrid_backend;
mod room_transform;
pub mod size_to_spread;
mod vbap_backend;

use crate::spatial_vbap::{DistanceModel, Gains, adm_to_spherical, spherical_to_adm};
use crate::speaker_layout::SpeakerLayout;
use anyhow::Result;
use rayon::prelude::*;
use serde::Serialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub use barycenter_backend::BarycenterBackend;
pub use degenerate_vbap_backend::DegenerateVbapBackend;
use distance_attenuation::DistanceAttenuatedModel;
use distance_diffuse::DistanceDiffuseModel;
pub use evaluation_artifact::{
    BackendRestoreSnapshot, SerializedEvaluationMode, build_backend_restore_snapshot,
};
pub use experimental_distance_backend::ExperimentalDistanceBackend;
pub use hybrid_backend::{BlendCurve, HybridBackend};
pub use room_transform::room_scaled_position;
pub use size_to_spread::{SizeToSpreadMode, reduce_size_to_spread};
pub use vbap_backend::{VbapBackend, VbapSpreadParams};

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct BackendCapabilities {
    pub supports_realtime: bool,
    pub supports_precomputed_polar: bool,
    pub supports_precomputed_cartesian: bool,
    pub supports_position_interpolation: bool,
    pub supports_distance_model: bool,
    pub supports_spread: bool,
    pub supports_spread_from_distance: bool,
    /// True when the backend consumes per-event object size (anisotropic
    /// w/d/h triplet) in addition to or instead of the global spread params.
    pub supports_event_size: bool,
    pub supports_distance_diffuse: bool,
    pub supports_table_export: bool,
}

/// Canonical id for a built-in backend, accepting the legacy aliases that older
/// configs and OSC clients may still send (`"barycentre"`, `"distance"`,
/// `"distance_based"`). Returns `None` for anything that is not a shipped
/// built-in; callers then fall back to the
/// [`BackendRegistry`](crate::backend_registry::BackendRegistry) to resolve
/// contributor-registered backend ids.
pub fn canonical_builtin_backend_id(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "vbap" => Some("vbap"),
        "barycenter" | "barycentre" => Some("barycenter"),
        "experimental_distance" | "distance" | "distance_based" => Some("experimental_distance"),
        "hybrid" => Some("hybrid"),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveEvaluationMode {
    Realtime,
    PrecomputedPolar,
    PrecomputedCartesian,
}

impl EffectiveEvaluationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Realtime => "realtime",
            Self::PrecomputedPolar => "precomputed_polar",
            Self::PrecomputedCartesian => "precomputed_cartesian",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RenderRequest {
    pub adm_position: [f64; 3],
    /// Per-event object size (w, d, h) ∈ [0, 1]³. `[0, 0, 0]` for a point
    /// source or when no size information is available.
    ///
    /// This is the only spread-related input that stays on the request: it is
    /// genuine per-object metadata that varies frame to frame. The spread
    /// *tuning* (min/max, distance ramp, size→scalar policy) is baked into the
    /// backend at build time from the generic param bag — see
    /// [`crate::render_backend::VbapSpreadParams`]. Because object-size spread is
    /// resolved live from this field, it is honoured only in realtime evaluation;
    /// the precomputed (sampled) modes index on position alone and therefore
    /// freeze it at the build-time `event_size`.
    pub event_size: [f32; 3],
    pub room_ratio: [f32; 3],
    pub room_ratio_rear: f32,
    pub room_ratio_lower: f32,
    pub room_ratio_center_blend: f32,
    pub use_distance_diffuse: bool,
    pub distance_diffuse_threshold: f32,
    pub distance_diffuse_curve: f32,
    /// ADM axes negated to build the diffuse mirror image. See
    /// [`crate::spatial_vbap::MirrorAxes`].
    pub diffuse_mirror_axes: crate::spatial_vbap::MirrorAxes,
    pub distance_model: DistanceModel,
}

pub struct RenderResponse {
    pub gains: Gains,
}

#[derive(Clone, Copy)]
pub struct CartesianEvaluationConfig {
    pub x_size: usize,
    pub y_size: usize,
    pub z_size: usize,
    pub z_neg_size: usize,
}

#[derive(Clone, Copy)]
pub struct PolarEvaluationConfig {
    pub azimuth_values: usize,
    pub elevation_values: usize,
    pub distance_values: usize,
    pub distance_max: f32,
    pub allow_negative_z: bool,
}

#[derive(Clone, Copy)]
pub struct EvaluationBuildConfig {
    pub request_template: RenderRequest,
    pub position_interpolation: bool,
    pub cartesian: CartesianEvaluationConfig,
    pub polar: PolarEvaluationConfig,
    /// Metric used to reduce a position to a scalar distance for the distance
    /// model and distance diffuse output stages (Spherical / Chebyshev).
    pub distance_model_metric: crate::spatial_vbap::DistanceMetric,
    pub distance_diffuse_metric: crate::spatial_vbap::DistanceMetric,
    /// Number of object-size *intervals* to precompute. `0` (default) bakes a
    /// single table at the build-time `event_size` (object size honoured only in
    /// realtime). `N >= 1` builds `N + 1` tables at isotropic sizes `s_i = i / N`
    /// and interpolates between them at read time, so the precomputed modes
    /// honour object size. Only meaningful for backends whose capabilities report
    /// `supports_event_size`. See [`SizeInterpolatingEvaluator`].
    pub object_size_intervals: usize,
    /// Policy used to reduce a `(w, d, h)` object-size triplet to the scalar that
    /// indexes the size tables at read time (see [`reduce_size_to_spread`]).
    pub object_size_mode: SizeToSpreadMode,
}

/// A gain model maps an object position (plus live render parameters) to a
/// per-speaker gain vector. Implement this trait to add a render backend.
///
/// # Hot-path contract for [`compute_gains`](GainModel::compute_gains)
///
/// `compute_gains` runs in the realtime audio thread, once per object per band
/// per frame. To keep that thread glitch-free, an implementation MUST:
///
/// - **not panic** — return a best-effort gain vector instead (e.g. zeroed);
/// - **not allocate** on the heap, lock, or block;
/// - return exactly [`speaker_count`](GainModel::speaker_count) finite gains.
///
/// Do any expensive setup (triangulation, lookup tables, caches) when the model
/// is built, not here. As a safety net the engine smoke-tests every freshly
/// built backend on a few reference positions on the build thread: a model that
/// panics or returns a malformed gain vector is rejected at topology build time
/// (surfaced to Studio as a recompute error) rather than crashing the audio
/// thread — but that guard only catches the build-time probe, so honouring the
/// contract above is still required for correct realtime behaviour.
pub trait GainModel: Send + Sync + 'static {
    fn backend_id(&self) -> &'static str;
    fn backend_label(&self) -> &'static str;
    fn capabilities(&self) -> BackendCapabilities;
    fn speaker_count(&self) -> usize;
    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse;
    fn save_to_file(&self, path: &std::path::Path, speaker_layout: &SpeakerLayout) -> Result<()>;
}

pub trait EvaluationStrategy {
    fn effective_mode(&self) -> EffectiveEvaluationMode;
    fn prepare(
        self,
        model: Arc<dyn GainModel>,
        config: &EvaluationBuildConfig,
    ) -> Result<Box<dyn PreparedEvaluator>>;
}

/// Borrowed view of a sampled cartesian evaluator's table + axes, used to merge
/// several per-band tables into one [`MultiBandCartesianTable`].
#[derive(Clone, Copy)]
pub(crate) struct CartesianParts<'a> {
    /// Flat gains grid, layout `[cell][speaker]` (band-local speakers).
    pub gains: &'a [f32],
    pub speaker_count: usize,
    pub x: &'a AxisLut,
    pub y: &'a AxisLut,
    pub z: &'a AxisLut,
    pub position_interpolation: bool,
}

/// Borrowed view of a sampled polar evaluator's table + axes, the polar
/// counterpart of [`CartesianParts`]. Flat gains layout `[dist][el][az][speaker]`
/// (cell order `az` fastest), so the unified table treats azimuth as the x axis.
#[derive(Clone, Copy)]
pub(crate) struct PolarParts<'a> {
    pub gains: &'a [f32],
    pub speaker_count: usize,
    pub azimuth: &'a AzimuthLut,
    pub elevation: &'a AxisLut,
    pub distance: &'a AxisLut,
    pub position_interpolation: bool,
}

pub trait PreparedEvaluator: Send + Sync {
    fn speaker_count(&self) -> usize;
    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse;
    /// The decorated gain model this evaluator wraps, when it holds one. Shared
    /// (`Arc`) so a geometry-unchanged recompute can reuse it and rebuild only the
    /// evaluation wrapper (no re-triangulation). Default `None` (e.g. a from-file
    /// artifact evaluator that owns a table, not a model). See
    /// `PreparedRenderEngine::decorated_model`.
    fn model_arc(&self) -> Option<Arc<dyn GainModel>> {
        None
    }
    /// Update the read-time `position_interpolation` flag (nearest cell vs
    /// trilinear). The precomputed table content is independent of this flag, so
    /// toggling it must NOT rebuild the table — only the sampled evaluators hold
    /// the flag, and they read it via interior mutability. Default: no-op
    /// (realtime evaluators recompute live and ignore it here).
    fn set_position_interpolation(&self, _interpolate: bool) {}
    fn save_to_file(&self, path: &std::path::Path, speaker_layout: &SpeakerLayout) -> Result<()>;
    /// Borrow the sampled cartesian table + axes, when this evaluator is a
    /// precomputed cartesian one. Default `None` (realtime/polar). Crate-internal
    /// view type, used only to merge bands into a `MultiBandCartesianTable`.
    #[allow(private_interfaces)]
    fn cartesian_parts(&self) -> Option<CartesianParts<'_>> {
        None
    }
    /// Borrow the sampled polar table + axes, when this evaluator is a precomputed
    /// polar one. Default `None`. Crate-internal view used to merge bands into a
    /// polar [`MultiBandTable`].
    #[allow(private_interfaces)]
    fn polar_parts(&self) -> Option<PolarParts<'_>> {
        None
    }
    /// Serialize the precomputed evaluation table (gains grid + metadata) to the
    /// portable artifact byte layout, so it can be shipped to clients (chunked) and
    /// rebuilt verbatim. Default: unsupported (realtime evaluators hold no table).
    fn artifact_bytes(&self, speaker_layout: &SpeakerLayout) -> Result<Vec<u8>> {
        let _ = speaker_layout;
        anyhow::bail!("evaluator has no precomputed table to serialize")
    }
}

pub struct RealtimeEvaluator {
    model: Arc<dyn GainModel>,
}

impl RealtimeEvaluator {
    pub fn new(model: Arc<dyn GainModel>) -> Self {
        Self { model }
    }
}

impl PreparedEvaluator for RealtimeEvaluator {
    fn speaker_count(&self) -> usize {
        self.model.speaker_count()
    }

    fn model_arc(&self) -> Option<Arc<dyn GainModel>> {
        Some(Arc::clone(&self.model))
    }

    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        self.model.compute_gains(req)
    }

    fn save_to_file(&self, path: &std::path::Path, speaker_layout: &SpeakerLayout) -> Result<()> {
        let _ = (path, speaker_layout);
        anyhow::bail!("only precomputed evaluators can be exported to a from-file artifact")
    }
}

pub struct SampledCartesianEvaluator {
    model: Arc<dyn GainModel>,
    x_positions: Vec<f32>,
    y_positions: Vec<f32>,
    z_positions: Vec<f32>,
    // Precomputed division-free lookups for the runtime table read. Kept in sync
    // with the *_positions arrays above (the source of truth for serialization).
    x_lut: AxisLut,
    y_lut: AxisLut,
    z_lut: AxisLut,
    gains: Vec<f32>,
    speaker_count: usize,
    /// Read-time only (nearest cell vs trilinear). Interior-mutable so the live
    /// toggle can update it without rebuilding the table (see `set_position_interpolation`).
    position_interpolation: AtomicBool,
    frozen_request: RenderRequest,
    backend_restore_snapshot: Option<BackendRestoreSnapshot>,
}

impl SampledCartesianEvaluator {
    pub fn new(model: Arc<dyn GainModel>, config: &EvaluationBuildConfig) -> Self {
        // Intentionally sample and query the precomputed cartesian evaluator in native
        // ADM coordinates. The backend remains responsible for any room/depth transforms,
        // so the runtime can read gains directly from object positions without converting
        // into a backend-specific "effect space" first.
        let x_positions = evenly_spaced_axis(config.cartesian.x_size.max(2), -1.0, 1.0);
        let y_positions = evenly_spaced_axis(config.cartesian.y_size.max(2), -1.0, 1.0);
        let z_positions =
            cartesian_z_axis(config.cartesian.z_size.max(2), config.cartesian.z_neg_size);
        let speaker_count = model.speaker_count();
        let (nx, ny, nz) = (x_positions.len(), y_positions.len(), z_positions.len());
        let template = config.request_template;
        // Sampling the gain model over the full x×y×z volume dominates engine
        // startup, and it runs once per render backend. Each cell is independent
        // and GainModel is Sync, so evaluate them in parallel. The flat index
        // decodes to the SAME z→y→x order the sequential build produced, which
        // the runtime table lookup relies on.
        let per_cell: Vec<Gains> = (0..nx * ny * nz)
            .into_par_iter()
            .map(|idx| {
                let xi = idx % nx;
                let yi = (idx / nx) % ny;
                let zi = idx / (nx * ny);
                let mut request = template;
                request.adm_position = [
                    x_positions[xi] as f64,
                    y_positions[yi] as f64,
                    z_positions[zi] as f64,
                ];
                model.compute_gains(&request).gains
            })
            .collect();
        let mut gains = Vec::with_capacity(nx * ny * nz * speaker_count);
        for cell in &per_cell {
            gains.extend_from_slice(&cell[..]);
        }
        let backend_restore_snapshot = build_backend_restore_snapshot(
            model.backend_id(),
            model.backend_label(),
            SerializedEvaluationMode::PrecomputedCartesian,
            config,
        );
        let x_lut = AxisLut::from_values(&x_positions);
        let y_lut = AxisLut::from_values(&y_positions);
        let z_lut = AxisLut::from_values(&z_positions);
        Self {
            model,
            x_positions,
            y_positions,
            z_positions,
            x_lut,
            y_lut,
            z_lut,
            gains,
            speaker_count,
            position_interpolation: AtomicBool::new(config.position_interpolation),
            frozen_request: config.request_template,
            backend_restore_snapshot,
        }
    }
}

impl PreparedEvaluator for SampledCartesianEvaluator {
    fn speaker_count(&self) -> usize {
        self.speaker_count
    }

    fn model_arc(&self) -> Option<Arc<dyn GainModel>> {
        Some(Arc::clone(&self.model))
    }

    fn set_position_interpolation(&self, interpolate: bool) {
        self.position_interpolation
            .store(interpolate, Ordering::Relaxed);
    }

    #[allow(private_interfaces)]
    fn cartesian_parts(&self) -> Option<CartesianParts<'_>> {
        Some(CartesianParts {
            gains: &self.gains,
            speaker_count: self.speaker_count,
            x: &self.x_lut,
            y: &self.y_lut,
            z: &self.z_lut,
            position_interpolation: self.position_interpolation.load(Ordering::Relaxed),
        })
    }

    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        // Read the table directly from native ADM coordinates. This avoids a render-time
        // round-trip through spherical/effect-space conversions for the cartesian path.
        let gains = sample_cartesian_table(
            &self.gains,
            self.speaker_count,
            &self.x_lut,
            &self.y_lut,
            &self.z_lut,
            req.adm_position.map(|value| value as f32),
            self.position_interpolation.load(Ordering::Relaxed),
        );
        RenderResponse { gains }
    }

    fn save_to_file(&self, path: &std::path::Path, speaker_layout: &SpeakerLayout) -> Result<()> {
        std::fs::write(path, self.artifact_bytes(speaker_layout)?)?;
        Ok(())
    }

    fn artifact_bytes(&self, speaker_layout: &SpeakerLayout) -> Result<Vec<u8>> {
        evaluation_artifact::LoadedEvaluationArtifact::from_sampled_cartesian(
            self.model.backend_id(),
            self.model.backend_label(),
            speaker_layout,
            self.frozen_request,
            self.position_interpolation.load(Ordering::Relaxed),
            self.backend_restore_snapshot.as_ref(),
            &self.x_positions,
            &self.y_positions,
            &self.z_positions,
            &self.gains,
            self.speaker_count,
        )?
        .to_serialized_bytes()
    }
}

pub struct SampledPolarEvaluator {
    model: Arc<dyn GainModel>,
    azimuth_positions: Vec<f32>,
    elevation_positions: Vec<f32>,
    distance_positions: Vec<f32>,
    // Division-free lookups rebuilt from the *_positions arrays (the source of
    // truth for serialization). Kept in sync with them.
    azimuth_lut: AzimuthLut,
    elevation_lut: AxisLut,
    distance_lut: AxisLut,
    gains: Vec<f32>,
    speaker_count: usize,
    /// Read-time only (nearest cell vs trilinear); interior-mutable, see
    /// `set_position_interpolation`.
    position_interpolation: AtomicBool,
    frozen_request: RenderRequest,
    backend_restore_snapshot: Option<BackendRestoreSnapshot>,
}

impl SampledPolarEvaluator {
    pub fn new(model: Arc<dyn GainModel>, config: &EvaluationBuildConfig) -> Self {
        let azimuth_positions = polar_azimuth_axis(config.polar.azimuth_values.max(2));
        let elevation_positions = polar_elevation_axis(
            config.polar.elevation_values.max(2),
            config.polar.allow_negative_z,
        );
        let distance_positions = evenly_spaced_axis(
            config.polar.distance_values.max(2),
            0.0,
            config.polar.distance_max.max(0.01),
        );
        let speaker_count = model.speaker_count();
        let mut gains = Vec::with_capacity(
            azimuth_positions.len()
                * elevation_positions.len()
                * distance_positions.len()
                * speaker_count,
        );
        let mut request = config.request_template;
        for &distance in &distance_positions {
            for &elevation in &elevation_positions {
                for &azimuth in &azimuth_positions {
                    let (x, y, z) = spherical_to_adm(azimuth, elevation, distance);
                    request.adm_position = [x as f64, y as f64, z as f64];
                    gains.extend_from_slice(&model.compute_gains(&request).gains);
                }
            }
        }
        let backend_restore_snapshot = build_backend_restore_snapshot(
            model.backend_id(),
            model.backend_label(),
            SerializedEvaluationMode::PrecomputedPolar,
            config,
        );
        let azimuth_lut = AzimuthLut::from_values(&azimuth_positions);
        let elevation_lut = AxisLut::from_values(&elevation_positions);
        let distance_lut = AxisLut::from_values(&distance_positions);
        Self {
            model,
            azimuth_positions,
            elevation_positions,
            distance_positions,
            azimuth_lut,
            elevation_lut,
            distance_lut,
            gains,
            speaker_count,
            position_interpolation: AtomicBool::new(config.position_interpolation),
            frozen_request: config.request_template,
            backend_restore_snapshot,
        }
    }
}

impl PreparedEvaluator for SampledPolarEvaluator {
    fn speaker_count(&self) -> usize {
        self.speaker_count
    }

    fn model_arc(&self) -> Option<Arc<dyn GainModel>> {
        Some(Arc::clone(&self.model))
    }

    fn set_position_interpolation(&self, interpolate: bool) {
        self.position_interpolation
            .store(interpolate, Ordering::Relaxed);
    }

    #[allow(private_interfaces)]
    fn polar_parts(&self) -> Option<PolarParts<'_>> {
        Some(PolarParts {
            gains: &self.gains,
            speaker_count: self.speaker_count,
            azimuth: &self.azimuth_lut,
            elevation: &self.elevation_lut,
            distance: &self.distance_lut,
            position_interpolation: self.position_interpolation.load(Ordering::Relaxed),
        })
    }

    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        let (azimuth, elevation, distance) = adm_to_spherical(
            req.adm_position[0] as f32,
            req.adm_position[1] as f32,
            req.adm_position[2] as f32,
        );
        let gains = sample_polar_table_lut(
            &self.gains,
            self.speaker_count,
            &self.azimuth_lut,
            &self.elevation_lut,
            &self.distance_lut,
            [azimuth, elevation, distance],
            self.position_interpolation.load(Ordering::Relaxed),
        );
        RenderResponse { gains }
    }

    fn save_to_file(&self, path: &std::path::Path, speaker_layout: &SpeakerLayout) -> Result<()> {
        std::fs::write(path, self.artifact_bytes(speaker_layout)?)?;
        Ok(())
    }

    fn artifact_bytes(&self, speaker_layout: &SpeakerLayout) -> Result<Vec<u8>> {
        evaluation_artifact::LoadedEvaluationArtifact::from_sampled_polar(
            self.model.backend_id(),
            self.model.backend_label(),
            speaker_layout,
            self.frozen_request,
            self.position_interpolation.load(Ordering::Relaxed),
            self.backend_restore_snapshot.as_ref(),
            &self.azimuth_positions,
            &self.elevation_positions,
            &self.distance_positions,
            &self.gains,
            self.speaker_count,
        )?
        .to_serialized_bytes()
    }
}

/// Wraps `N + 1` position tables, each baked at a fixed isotropic object size
/// `s_i = i / N`, and interpolates between the two bracketing tables at read time
/// on the object's reduced size scalar. This is how the precomputed modes honour
/// object size: a single table freezes it, so when `object_size_intervals > 0`
/// the strategy builds several tables and wraps them here.
///
/// `cartesian_parts`/`polar_parts` return `None`, so a crossover layout with this
/// evaluator does not build the unified multi-band table and falls back to the
/// per-band `compute_gains` path (see `build_unified_table`). Table export is
/// likewise unsupported here.
pub struct SizeInterpolatingEvaluator {
    model: Arc<dyn GainModel>,
    /// Sorted ascending in `[0, 1]`; `sizes[i] = i / N`. `len() == inners.len()`,
    /// always `>= 2` (built only when `intervals >= 1`).
    sizes: Vec<f32>,
    inners: Vec<Box<dyn PreparedEvaluator>>,
    /// Policy reducing the request's `(w, d, h)` to the scalar that indexes `sizes`.
    mode: SizeToSpreadMode,
    speaker_count: usize,
}

impl SizeInterpolatingEvaluator {
    /// Build `intervals + 1` inner evaluators at isotropic sizes `s_i = i /
    /// intervals`, each via `build_inner` with a per-size config (object size
    /// frozen to `[s_i; 3]`, its own interval count reset to 0 so the inner bakes
    /// a single table).
    fn new(
        model: Arc<dyn GainModel>,
        config: &EvaluationBuildConfig,
        intervals: usize,
        build_inner: impl Fn(Arc<dyn GainModel>, &EvaluationBuildConfig) -> Box<dyn PreparedEvaluator>,
    ) -> Self {
        let n = intervals.max(1);
        let speaker_count = model.speaker_count();
        let mut sizes = Vec::with_capacity(n + 1);
        let mut inners = Vec::with_capacity(n + 1);
        for i in 0..=n {
            let s = i as f32 / n as f32;
            let mut inner_config = *config;
            inner_config.object_size_intervals = 0;
            inner_config.request_template.event_size = [s, s, s];
            sizes.push(s);
            inners.push(build_inner(Arc::clone(&model), &inner_config));
        }
        Self {
            model,
            sizes,
            inners,
            mode: config.object_size_mode,
            speaker_count,
        }
    }
}

impl PreparedEvaluator for SizeInterpolatingEvaluator {
    fn speaker_count(&self) -> usize {
        self.speaker_count
    }

    fn model_arc(&self) -> Option<Arc<dyn GainModel>> {
        Some(Arc::clone(&self.model))
    }

    fn set_position_interpolation(&self, interpolate: bool) {
        for inner in &self.inners {
            inner.set_position_interpolation(interpolate);
        }
    }

    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        let pos = req.adm_position.map(|value| value as f32);
        let query = reduce_size_to_spread(req.event_size, pos, self.mode).clamp(0.0, 1.0);
        // Bracket the query size. `sizes` is sorted ascending and has >= 2 entries.
        let last = self.sizes.len() - 1;
        let mut hi = 1;
        while hi < last && self.sizes[hi] < query {
            hi += 1;
        }
        let lo = hi - 1;
        let span = (self.sizes[hi] - self.sizes[lo]).max(1e-6);
        let fraction = ((query - self.sizes[lo]) / span).clamp(0.0, 1.0);
        let gains_lo = self.inners[lo].compute_gains(req).gains;
        let gains_hi = self.inners[hi].compute_gains(req).gains;
        let mut gains = Gains::zeroed(self.speaker_count);
        for index in 0..self.speaker_count {
            gains.set(
                index,
                gains_lo[index] * (1.0 - fraction) + gains_hi[index] * fraction,
            );
        }
        RenderResponse { gains }
    }

    fn save_to_file(&self, path: &std::path::Path, speaker_layout: &SpeakerLayout) -> Result<()> {
        std::fs::write(path, self.artifact_bytes(speaker_layout)?)?;
        Ok(())
    }

    fn artifact_bytes(&self, _speaker_layout: &SpeakerLayout) -> Result<Vec<u8>> {
        anyhow::bail!("object-size interval tables are not serializable yet")
    }
}

pub struct RealtimeStrategy;

impl EvaluationStrategy for RealtimeStrategy {
    fn effective_mode(&self) -> EffectiveEvaluationMode {
        EffectiveEvaluationMode::Realtime
    }

    fn prepare(
        self,
        model: Arc<dyn GainModel>,
        _config: &EvaluationBuildConfig,
    ) -> Result<Box<dyn PreparedEvaluator>> {
        Ok(Box::new(RealtimeEvaluator::new(model)))
    }
}

pub struct PrecomputedCartesianStrategy;

impl EvaluationStrategy for PrecomputedCartesianStrategy {
    fn effective_mode(&self) -> EffectiveEvaluationMode {
        EffectiveEvaluationMode::PrecomputedCartesian
    }

    fn prepare(
        self,
        model: Arc<dyn GainModel>,
        config: &EvaluationBuildConfig,
    ) -> Result<Box<dyn PreparedEvaluator>> {
        if config.object_size_intervals > 0 && model.capabilities().supports_event_size {
            Ok(Box::new(SizeInterpolatingEvaluator::new(
                model,
                config,
                config.object_size_intervals,
                |inner_model, inner_config| {
                    Box::new(SampledCartesianEvaluator::new(inner_model, inner_config))
                },
            )))
        } else {
            Ok(Box::new(SampledCartesianEvaluator::new(model, config)))
        }
    }
}

pub struct PrecomputedPolarStrategy;

impl EvaluationStrategy for PrecomputedPolarStrategy {
    fn effective_mode(&self) -> EffectiveEvaluationMode {
        EffectiveEvaluationMode::PrecomputedPolar
    }

    fn prepare(
        self,
        model: Arc<dyn GainModel>,
        config: &EvaluationBuildConfig,
    ) -> Result<Box<dyn PreparedEvaluator>> {
        if config.object_size_intervals > 0 && model.capabilities().supports_event_size {
            Ok(Box::new(SizeInterpolatingEvaluator::new(
                model,
                config,
                config.object_size_intervals,
                |inner_model, inner_config| {
                    Box::new(SampledPolarEvaluator::new(inner_model, inner_config))
                },
            )))
        } else {
            Ok(Box::new(SampledPolarEvaluator::new(model, config)))
        }
    }
}

pub struct PreparedRenderEngine {
    backend_id: &'static str,
    backend_label: &'static str,
    capabilities: BackendCapabilities,
    evaluation_mode: EffectiveEvaluationMode,
    backend_restore_snapshot: Option<BackendRestoreSnapshot>,
    evaluator: Box<dyn PreparedEvaluator>,
}

impl PreparedRenderEngine {
    pub fn new(
        backend_id: &'static str,
        backend_label: &'static str,
        capabilities: BackendCapabilities,
        evaluation_mode: EffectiveEvaluationMode,
        backend_restore_snapshot: Option<BackendRestoreSnapshot>,
        evaluator: Box<dyn PreparedEvaluator>,
    ) -> Self {
        Self {
            backend_id,
            backend_label,
            capabilities,
            evaluation_mode,
            backend_restore_snapshot,
            evaluator,
        }
    }

    pub fn backend_id(&self) -> &'static str {
        self.backend_id
    }

    pub fn backend_label(&self) -> &'static str {
        self.backend_label
    }

    pub fn capabilities(&self) -> BackendCapabilities {
        self.capabilities
    }

    pub fn evaluation_mode(&self) -> EffectiveEvaluationMode {
        self.evaluation_mode
    }

    pub fn has_backend_restore_snapshot(&self) -> bool {
        self.backend_restore_snapshot.is_some()
    }

    pub fn backend_restore_snapshot(&self) -> Option<&BackendRestoreSnapshot> {
        self.backend_restore_snapshot.as_ref()
    }

    pub fn speaker_count(&self) -> usize {
        self.evaluator.speaker_count()
    }

    pub fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        self.evaluator.compute_gains(req)
    }

    /// Update the read-time `position_interpolation` flag on the underlying
    /// evaluator. Cheap and lock-free; does NOT rebuild the precomputed table.
    pub fn set_position_interpolation(&self, interpolate: bool) {
        self.evaluator.set_position_interpolation(interpolate);
    }

    /// The decorated gain model (geometry + output-stage decorators) this engine
    /// wraps. Shared (`Arc`) so a geometry-unchanged recompute can rebuild only the
    /// evaluation wrapper via [`wrap_prepared_engine`] instead of re-triangulating.
    pub(crate) fn decorated_model(&self) -> Option<Arc<dyn GainModel>> {
        self.evaluator.model_arc()
    }

    pub(crate) fn cartesian_parts(&self) -> Option<CartesianParts<'_>> {
        self.evaluator.cartesian_parts()
    }

    pub(crate) fn polar_parts(&self) -> Option<PolarParts<'_>> {
        self.evaluator.polar_parts()
    }

    pub fn save_to_file(
        &self,
        path: &std::path::Path,
        speaker_layout: &SpeakerLayout,
    ) -> Result<()> {
        self.evaluator.save_to_file(path, speaker_layout)
    }

    /// Serialize the precomputed evaluation table (gains grid) to portable artifact
    /// bytes for shipping to clients. Errors on non-precomputed (realtime) backends.
    pub fn artifact_bytes(&self, speaker_layout: &SpeakerLayout) -> Result<Vec<u8>> {
        self.evaluator.artifact_bytes(speaker_layout)
    }
}

/// Apply the shared output-stage decorators to a raw backend gain model and
/// return it as a shareable `Arc`. The result is geometry/output-stage state
/// only (no evaluation table), so it can be reused across an evaluation-mode
/// change (see [`wrap_prepared_engine`]).
///
/// Order matters: distance diffuse blends + renormalizes, so distance attenuation
/// must wrap it (be applied last) or the renorm would cancel the attenuation.
/// Identity/metadata still delegate to the inner backend; capabilities gain
/// `supports_distance_diffuse` / `_model`.
pub fn build_decorated_model(
    model: Box<dyn GainModel>,
    config: &EvaluationBuildConfig,
) -> Arc<dyn GainModel> {
    let model: Box<dyn GainModel> = Box::new(DistanceDiffuseModel::new(
        model,
        config.distance_diffuse_metric,
    ));
    let model: Box<dyn GainModel> = Box::new(DistanceAttenuatedModel::new(
        model,
        config.distance_model_metric,
    ));
    Arc::from(model)
}

/// Wrap an already-decorated gain model in the evaluation strategy for the given
/// mode. The model is shared (`Arc`): realtime just re-wraps it (no work);
/// precomputed samples it into a table. Reused on a geometry-unchanged recompute.
pub fn wrap_prepared_engine(
    model: Arc<dyn GainModel>,
    evaluation_mode: EffectiveEvaluationMode,
    config: &EvaluationBuildConfig,
) -> Result<PreparedRenderEngine> {
    let backend_id = model.backend_id();
    let backend_label = model.backend_label();
    let capabilities = model.capabilities();
    let evaluator = match evaluation_mode {
        EffectiveEvaluationMode::Realtime => RealtimeStrategy.prepare(model, config)?,
        EffectiveEvaluationMode::PrecomputedCartesian => {
            PrecomputedCartesianStrategy.prepare(model, config)?
        }
        EffectiveEvaluationMode::PrecomputedPolar => {
            PrecomputedPolarStrategy.prepare(model, config)?
        }
    };
    Ok(PreparedRenderEngine::new(
        backend_id,
        backend_label,
        capabilities,
        evaluation_mode,
        None,
        evaluator,
    ))
}

pub fn build_prepared_render_engine(
    model: Box<dyn GainModel>,
    evaluation_mode: EffectiveEvaluationMode,
    config: &EvaluationBuildConfig,
) -> Result<PreparedRenderEngine> {
    wrap_prepared_engine(
        build_decorated_model(model, config),
        evaluation_mode,
        config,
    )
}

#[derive(Clone, Copy, Debug)]
struct AxisSample {
    lower: usize,
    upper: usize,
    fraction: f32,
}

pub(crate) fn evenly_spaced_axis(count: usize, min: f32, max: f32) -> Vec<f32> {
    if count <= 1 {
        return vec![min];
    }
    let step = (max - min) / (count.saturating_sub(1) as f32);
    (0..count).map(|index| min + step * index as f32).collect()
}

pub(crate) fn cartesian_z_axis(z_size: usize, z_neg_size: usize) -> Vec<f32> {
    let mut values = Vec::with_capacity(z_neg_size + z_size);
    if z_neg_size > 0 {
        for index in 0..z_neg_size {
            let t = (index + 1) as f32 / z_neg_size as f32;
            values.push(-1.0 + (t - 1.0 / z_neg_size as f32));
        }
    }
    values.extend(evenly_spaced_axis(z_size.max(2), 0.0, 1.0));
    values
}

fn polar_azimuth_axis(count: usize) -> Vec<f32> {
    let count = count.max(2);
    let step = 360.0 / count as f32;
    (0..count)
        .map(|index| -180.0 + step * index as f32)
        .collect()
}

fn polar_elevation_axis(count: usize, allow_negative_z: bool) -> Vec<f32> {
    if allow_negative_z {
        evenly_spaced_axis(count.max(2), -90.0, 90.0)
    } else {
        evenly_spaced_axis(count.max(2), 0.0, 90.0)
    }
}

/// Precomputed per-axis lookup: turns a position into the `(lower, upper,
/// fraction)` bracket without a per-call division or binary search. Built once
/// when the table is created (`inv_step` = `1.0 / grid_step`), so the runtime
/// lookup is a multiply instead of the `partition_point` search + step/fraction
/// divisions that dominated the cartesian `compute_gains` cost.
#[derive(Clone)]
pub(crate) enum AxisLut {
    /// Evenly spaced grid: `values[k] == min + k / inv_step`.
    Uniform { min: f32, inv_step: f32, len: usize },
    /// Two evenly spaced regions joined at the value `0.0` (the cartesian z
    /// axis): indices `0..=split` span `[-1, 0]`, `split..len` span `[0, 1]`.
    SplitZero {
        split: usize,
        neg_inv_step: f32,
        pos_inv_step: f32,
        len: usize,
    },
    /// Arbitrary ascending values — falls back to the binary-search path.
    Irregular(Vec<f32>),
}

impl AxisLut {
    /// Classify an axis grid. Detects an evenly-spaced axis (x/y) or the two
    /// uniform-region cartesian z axis; anything else keeps the search path, so
    /// the result is always correct regardless of grid shape.
    pub(crate) fn from_values(values: &[f32]) -> Self {
        let n = values.len();
        if n < 2 {
            return Self::Irregular(values.to_vec());
        }
        // Returns inv_step iff values[lo..=hi] are evenly spaced.
        let uniform_inv_step = |lo: usize, hi: usize| -> Option<f32> {
            let step = (values[hi] - values[lo]) / (hi - lo) as f32;
            if step <= 0.0 {
                return None;
            }
            let tol = 1e-5 * step.max(1.0);
            for k in lo..=hi {
                let expected = values[lo] + (k - lo) as f32 * step;
                if (values[k] - expected).abs() > tol {
                    return None;
                }
            }
            Some(1.0 / step)
        };
        if let Some(inv_step) = uniform_inv_step(0, n - 1) {
            return Self::Uniform {
                min: values[0],
                inv_step,
                len: n,
            };
        }
        if let Some(split) = values.iter().position(|&v| v == 0.0) {
            if split > 0 && split < n - 1 {
                if let (Some(neg), Some(pos)) =
                    (uniform_inv_step(0, split), uniform_inv_step(split, n - 1))
                {
                    return Self::SplitZero {
                        split,
                        neg_inv_step: neg,
                        pos_inv_step: pos,
                        len: n,
                    };
                }
            }
        }
        Self::Irregular(values.to_vec())
    }

    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Uniform { len, .. } | Self::SplitZero { len, .. } => *len,
            Self::Irregular(values) => values.len(),
        }
    }

    /// Bracket within an evenly-spaced region of `len` points, given the
    /// position already expressed in cell units (`f = (pos - min) * inv_step`).
    fn bracket_uniform(f: f32, len: usize, interpolate: bool) -> AxisSample {
        let f = f.clamp(0.0, (len - 1) as f32);
        if !interpolate {
            let nearest = ((f + 0.5) as usize).min(len - 1);
            return AxisSample {
                lower: nearest,
                upper: nearest,
                fraction: 0.0,
            };
        }
        let lower = (f as usize).min(len - 2);
        AxisSample {
            lower,
            upper: lower + 1,
            fraction: f - lower as f32,
        }
    }

    fn sample(&self, position: f32, interpolate: bool) -> AxisSample {
        match self {
            Self::Uniform { min, inv_step, len } => {
                Self::bracket_uniform((position - min) * inv_step, *len, interpolate)
            }
            Self::SplitZero {
                split,
                neg_inv_step,
                pos_inv_step,
                len,
            } => {
                if position < 0.0 {
                    // Region [-1, 0] occupies indices 0..=split (split+1 points).
                    Self::bracket_uniform((position + 1.0) * neg_inv_step, split + 1, interpolate)
                } else {
                    // Region [0, 1] occupies indices split..len; offset back.
                    let mut s =
                        Self::bracket_uniform(position * pos_inv_step, len - split, interpolate);
                    s.lower += split;
                    s.upper += split;
                    s
                }
            }
            Self::Irregular(values) => sample_axis(values, position, interpolate),
        }
    }
}

/// Wrapped (circular) azimuth axis lookup — the polar counterpart of [`AxisLut`].
/// Azimuth grids are evenly spaced and periodic in degrees, so the bracket is
/// O(1): `f = (wrap_degrees(pos) - min) * inv_step`, with the `len-1 → 0` seam
/// handled by indexing modulo `len`. Replaces the per-lookup O(n) linear scan in
/// [`sample_wrapped_axis`]. A non-uniform restored grid falls back to that scan.
#[derive(Clone)]
pub(crate) enum AzimuthLut {
    /// Evenly spaced periodic grid: `values[k] == min + k / inv_step`, wrapping
    /// at `len` back to index 0 (which sits `360°` above `values[len-1]`).
    WrappedUniform { min: f32, inv_step: f32, len: usize },
    /// Arbitrary ascending grid — defers to the wrapped scan path.
    Irregular(Vec<f32>),
}

impl AzimuthLut {
    pub(crate) fn from_values(values: &[f32]) -> Self {
        let n = values.len();
        if n < 2 {
            return Self::Irregular(values.to_vec());
        }
        let step = (values[n - 1] - values[0]) / (n - 1) as f32;
        if step > 0.0 {
            let tol = 1e-5 * step.max(1.0);
            let uniform = (0..n).all(|k| (values[k] - (values[0] + k as f32 * step)).abs() <= tol);
            if uniform {
                return Self::WrappedUniform {
                    min: values[0],
                    inv_step: 1.0 / step,
                    len: n,
                };
            }
        }
        Self::Irregular(values.to_vec())
    }

    pub(crate) fn len(&self) -> usize {
        match self {
            Self::WrappedUniform { len, .. } => *len,
            Self::Irregular(values) => values.len(),
        }
    }

    fn sample(&self, position: f32, interpolate: bool) -> AxisSample {
        match self {
            Self::WrappedUniform { min, inv_step, len } => {
                let len = *len;
                // wrap_degrees maps into (-180, 180]; with min == values[0] the
                // cell coordinate f lands in (0, len], len being the seam point
                // that reads back into index 0 (== min + 360°).
                let f = (wrap_degrees(position) - min) * inv_step;
                if !interpolate {
                    let nearest = (f + 0.5).floor() as usize % len;
                    return AxisSample {
                        lower: nearest,
                        upper: nearest,
                        fraction: 0.0,
                    };
                }
                let base = f.floor();
                let lower = (base as usize) % len;
                let upper = (lower + 1) % len;
                AxisSample {
                    lower,
                    upper,
                    fraction: f - base,
                }
            }
            Self::Irregular(values) => {
                sample_wrapped_axis(values, wrap_degrees(position), interpolate)
            }
        }
    }
}

/// Division-free twin of [`sample_polar_table`]: same flat `[dist][el][az]` table
/// and trilinear/nearest accumulation, but the per-axis brackets come from the
/// precomputed [`AzimuthLut`]/[`AxisLut`] instead of per-call scans/divisions.
pub(crate) fn sample_polar_table_lut(
    table: &[f32],
    speaker_count: usize,
    azimuth: &AzimuthLut,
    elevation: &AxisLut,
    distance: &AxisLut,
    position: [f32; 3],
    interpolate: bool,
) -> Gains {
    let a = azimuth.sample(position[0], interpolate);
    let e = elevation.sample(position[1], interpolate);
    let d = distance.sample(position[2], interpolate);
    let az_len = azimuth.len();
    let el_len = elevation.len();
    let mut gains = Gains::zeroed(speaker_count);
    if !interpolate {
        write_flat_sample(
            table,
            speaker_count,
            az_len,
            el_len,
            a.lower,
            e.lower,
            d.lower,
            &mut gains,
        );
        return gains;
    }
    for (id, wd) in [(d.lower, 1.0 - d.fraction), (d.upper, d.fraction)] {
        for (ie, we) in [(e.lower, 1.0 - e.fraction), (e.upper, e.fraction)] {
            for (ia, wa) in [(a.lower, 1.0 - a.fraction), (a.upper, a.fraction)] {
                let weight = wa * we * wd;
                if weight <= 0.0 {
                    continue;
                }
                accumulate_flat_sample(
                    table,
                    speaker_count,
                    az_len,
                    el_len,
                    ia,
                    ie,
                    id,
                    weight,
                    &mut gains,
                );
            }
        }
    }
    gains
}

pub(crate) fn sample_cartesian_table(
    table: &[f32],
    speaker_count: usize,
    x_axis: &AxisLut,
    y_axis: &AxisLut,
    z_axis: &AxisLut,
    position: [f32; 3],
    interpolate: bool,
) -> Gains {
    let x = x_axis.sample(position[0].clamp(-1.0, 1.0), interpolate);
    let y = y_axis.sample(position[1].clamp(-1.0, 1.0), interpolate);
    let z = z_axis.sample(position[2].clamp(-1.0, 1.0), interpolate);
    let x_len = x_axis.len();
    let y_len = y_axis.len();
    let mut gains = Gains::zeroed(speaker_count);
    if !interpolate {
        write_flat_sample(
            table,
            speaker_count,
            x_len,
            y_len,
            x.lower,
            y.lower,
            z.lower,
            &mut gains,
        );
        return gains;
    }

    for (iz, wz) in [(z.lower, 1.0 - z.fraction), (z.upper, z.fraction)] {
        for (iy, wy) in [(y.lower, 1.0 - y.fraction), (y.upper, y.fraction)] {
            for (ix, wx) in [(x.lower, 1.0 - x.fraction), (x.upper, x.fraction)] {
                let weight = wx * wy * wz;
                if weight <= 0.0 {
                    continue;
                }
                accumulate_flat_sample(
                    table,
                    speaker_count,
                    x_len,
                    y_len,
                    ix,
                    iy,
                    iz,
                    weight,
                    &mut gains,
                );
            }
        }
    }
    gains
}

/// One grid axis of a [`MultiBandTable`], dispatching to the right precomputed
/// lookup so the trilinear core is coordinate-agnostic.
#[derive(Clone)]
pub(crate) enum GridAxis {
    Linear(AxisLut),
    Azimuth(AzimuthLut),
}

impl GridAxis {
    #[inline]
    fn sample(&self, position: f32, interpolate: bool) -> AxisSample {
        match self {
            Self::Linear(lut) => lut.sample(position, interpolate),
            Self::Azimuth(lut) => lut.sample(position, interpolate),
        }
    }

    #[inline]
    fn len(&self) -> usize {
        match self {
            Self::Linear(lut) => lut.len(),
            Self::Azimuth(lut) => lut.len(),
        }
    }
}

/// Coordinate space of a [`MultiBandTable`]: how an ADM position is turned into
/// grid coordinates before the per-axis lookups. Cartesian clamps the ADM cube;
/// polar converts to spherical (axes then self-clamp / wrap).
#[derive(Clone, Copy)]
pub(crate) enum CoordSpace {
    Cartesian,
    Polar,
}

impl CoordSpace {
    #[inline]
    fn to_grid(self, p: [f32; 3]) -> [f32; 3] {
        match self {
            Self::Cartesian => [
                p[0].clamp(-1.0, 1.0),
                p[1].clamp(-1.0, 1.0),
                p[2].clamp(-1.0, 1.0),
            ],
            Self::Polar => {
                let (az, el, dist) = adm_to_spherical(p[0], p[1], p[2]);
                [az, el, dist]
            }
        }
    }
}

/// One table covering several crossover bands at once, in either coordinate
/// space. The per-band gains for each grid cell are stored contiguously
/// (`[cell][band][speaker]`, each band full-size with the speaker scatter baked
/// in), so a lookup localises the cell ONCE and accumulates every band's gains in
/// a single pass — instead of one full lookup (localise + accumulate + scatter)
/// per band. The cost no longer scales with the band count beyond the
/// accumulation itself. Cell order is `axes[2]` (z/distance) slowest, `axes[0]`
/// (x/azimuth) fastest.
pub(crate) struct MultiBandTable {
    axes: [GridAxis; 3],
    coord: CoordSpace,
    /// `[cell][band][num_speakers]`, row-major.
    gains: Vec<f32>,
    n_bands: usize,
    num_speakers: usize,
    /// Read-time only (nearest cell vs trilinear); interior-mutable so the live
    /// toggle updates it without rebuilding the merged table.
    position_interpolation: AtomicBool,
}

impl MultiBandTable {
    /// Merge per-band cartesian tables into the unified layout.
    pub(crate) fn build_cartesian(
        bands: &[(CartesianParts<'_>, &[usize])],
        num_speakers: usize,
    ) -> Option<Self> {
        let (first, _) = bands.first()?;
        let axes = [
            GridAxis::Linear(first.x.clone()),
            GridAxis::Linear(first.y.clone()),
            GridAxis::Linear(first.z.clone()),
        ];
        let position_interpolation = first.position_interpolation;
        let cells: Vec<(&[f32], usize, &[usize])> = bands
            .iter()
            .map(|(p, idx)| (p.gains, p.speaker_count, *idx))
            .collect();
        Self::build_inner(
            axes,
            CoordSpace::Cartesian,
            &cells,
            num_speakers,
            position_interpolation,
        )
    }

    /// Merge per-band polar tables into the unified layout. Azimuth is `axes[0]`
    /// (x), elevation `axes[1]` (y), distance `axes[2]` (z), matching the polar
    /// evaluator's `[dist][el][az]` flat cell order.
    pub(crate) fn build_polar(
        bands: &[(PolarParts<'_>, &[usize])],
        num_speakers: usize,
    ) -> Option<Self> {
        let (first, _) = bands.first()?;
        let axes = [
            GridAxis::Azimuth(first.azimuth.clone()),
            GridAxis::Linear(first.elevation.clone()),
            GridAxis::Linear(first.distance.clone()),
        ];
        let position_interpolation = first.position_interpolation;
        let cells: Vec<(&[f32], usize, &[usize])> = bands
            .iter()
            .map(|(p, idx)| (p.gains, p.speaker_count, *idx))
            .collect();
        Self::build_inner(
            axes,
            CoordSpace::Polar,
            &cells,
            num_speakers,
            position_interpolation,
        )
    }

    /// Coordinate-agnostic merge: copies each band's per-cell gains into the
    /// `[cell][band][num_speakers]` grid, scattering band-local speakers to global
    /// indices. All bands must share the grid; returns `None` otherwise.
    fn build_inner(
        axes: [GridAxis; 3],
        coord: CoordSpace,
        bands: &[(&[f32], usize, &[usize])],
        num_speakers: usize,
        position_interpolation: bool,
    ) -> Option<Self> {
        let n_cells = axes[0].len() * axes[1].len() * axes[2].len();
        let n_bands = bands.len();
        let mut gains = vec![0.0f32; n_cells * n_bands * num_speakers];
        for (b, (band_gains, sc, indices)) in bands.iter().enumerate() {
            let sc = *sc;
            if band_gains.len() != n_cells * sc || indices.len() != sc {
                return None; // grid mismatch — fall back to the per-band path
            }
            for cell in 0..n_cells {
                let src = &band_gains[cell * sc..cell * sc + sc];
                let dst_base = (cell * n_bands + b) * num_speakers;
                for (i, &g) in src.iter().enumerate() {
                    gains[dst_base + indices[i]] = g;
                }
            }
        }
        Some(Self {
            axes,
            coord,
            gains,
            n_bands,
            num_speakers,
            position_interpolation: AtomicBool::new(position_interpolation),
        })
    }

    /// Update the read-time interpolation flag without rebuilding the table.
    pub(crate) fn set_position_interpolation(&self, interpolate: bool) {
        self.position_interpolation
            .store(interpolate, Ordering::Relaxed);
    }

    /// Trilinear lookup for all bands at `position`. Fills `out` with `n_bands`
    /// full-size `Gains` (one localisation, contiguous per-cell accumulation).
    pub(crate) fn sample_into(&self, position: [f32; 3], out: &mut Vec<Gains>) {
        let interp = self.position_interpolation.load(Ordering::Relaxed);
        let p = self.coord.to_grid(position);
        let x = self.axes[0].sample(p[0], interp);
        let y = self.axes[1].sample(p[1], interp);
        let z = self.axes[2].sample(p[2], interp);
        let x_len = self.axes[0].len();
        let y_len = self.axes[1].len();
        let band_stride = self.num_speakers;
        let cell_stride = self.n_bands * self.num_speakers;

        out.clear();
        out.resize(self.n_bands, Gains::zeroed(self.num_speakers));

        let table = &self.gains;
        let mut accumulate = |ix: usize, iy: usize, iz: usize, weight: f32| {
            let cell_base = (((iz * y_len) + iy) * x_len + ix) * cell_stride;
            for (b, g) in out.iter_mut().enumerate() {
                let base = cell_base + b * band_stride;
                let src = &table[base..base + band_stride];
                for (d, &s) in g[..band_stride].iter_mut().zip(src) {
                    *d += s * weight;
                }
            }
        };

        if !interp {
            accumulate(x.lower, y.lower, z.lower, 1.0);
            return;
        }
        for (iz, wz) in [(z.lower, 1.0 - z.fraction), (z.upper, z.fraction)] {
            for (iy, wy) in [(y.lower, 1.0 - y.fraction), (y.upper, y.fraction)] {
                for (ix, wx) in [(x.lower, 1.0 - x.fraction), (x.upper, x.fraction)] {
                    let weight = wx * wy * wz;
                    if weight <= 0.0 {
                        continue;
                    }
                    accumulate(ix, iy, iz, weight);
                }
            }
        }
    }
}

/// Reference polar lookup over the raw `*_positions` arrays (per-call wrapped
/// scan / binary search). Superseded at runtime by [`sample_polar_table_lut`];
/// retained as the parity oracle for the LUT path's tests.
#[cfg(test)]
pub(crate) fn sample_polar_table(
    table: &[f32],
    speaker_count: usize,
    azimuth_positions: &[f32],
    elevation_positions: &[f32],
    distance_positions: &[f32],
    position: [f32; 3],
    interpolate: bool,
) -> Gains {
    let azimuth = sample_wrapped_axis(azimuth_positions, wrap_degrees(position[0]), interpolate);
    let elevation = sample_axis(
        elevation_positions,
        position[1].clamp(
            *elevation_positions.first().unwrap_or(&-90.0),
            *elevation_positions.last().unwrap_or(&90.0),
        ),
        interpolate,
    );
    let distance = sample_axis(
        distance_positions,
        position[2].clamp(0.0, *distance_positions.last().unwrap_or(&0.0)),
        interpolate,
    );
    let mut gains = Gains::zeroed(speaker_count);
    if !interpolate {
        write_flat_sample(
            table,
            speaker_count,
            azimuth_positions.len(),
            elevation_positions.len(),
            azimuth.lower,
            elevation.lower,
            distance.lower,
            &mut gains,
        );
        return gains;
    }

    for (id, wd) in [
        (distance.lower, 1.0 - distance.fraction),
        (distance.upper, distance.fraction),
    ] {
        for (ie, we) in [
            (elevation.lower, 1.0 - elevation.fraction),
            (elevation.upper, elevation.fraction),
        ] {
            for (ia, wa) in [
                (azimuth.lower, 1.0 - azimuth.fraction),
                (azimuth.upper, azimuth.fraction),
            ] {
                let weight = wa * we * wd;
                if weight <= 0.0 {
                    continue;
                }
                accumulate_flat_sample(
                    table,
                    speaker_count,
                    azimuth_positions.len(),
                    elevation_positions.len(),
                    ia,
                    ie,
                    id,
                    weight,
                    &mut gains,
                );
            }
        }
    }
    gains
}

fn write_flat_sample(
    table: &[f32],
    speaker_count: usize,
    x_len: usize,
    y_len: usize,
    x_index: usize,
    y_index: usize,
    z_index: usize,
    gains: &mut Gains,
) {
    let offset = flat_sample_offset(speaker_count, x_len, y_len, x_index, y_index, z_index);
    // Slice both sides up front so the copy is bounds-check-free and vectorizable.
    gains[..speaker_count].copy_from_slice(&table[offset..offset + speaker_count]);
}

fn accumulate_flat_sample(
    table: &[f32],
    speaker_count: usize,
    x_len: usize,
    y_len: usize,
    x_index: usize,
    y_index: usize,
    z_index: usize,
    weight: f32,
    gains: &mut Gains,
) {
    let offset = flat_sample_offset(speaker_count, x_len, y_len, x_index, y_index, z_index);
    // Slice both sides so the weighted accumulation is bounds-check-free and the
    // compiler can vectorize the multiply-add over speakers.
    let row = &table[offset..offset + speaker_count];
    for (g, &t) in gains[..speaker_count].iter_mut().zip(row) {
        *g += t * weight;
    }
}

fn flat_sample_offset(
    speaker_count: usize,
    x_len: usize,
    y_len: usize,
    x_index: usize,
    y_index: usize,
    z_index: usize,
) -> usize {
    (((z_index * y_len) + y_index) * x_len + x_index) * speaker_count
}

fn sample_axis(values: &[f32], position: f32, interpolate: bool) -> AxisSample {
    if values.len() <= 1 {
        return AxisSample {
            lower: 0,
            upper: 0,
            fraction: 0.0,
        };
    }
    if !interpolate {
        let nearest = values
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| ((*a - position).abs()).total_cmp(&((*b - position).abs())))
            .map(|(index, _)| index)
            .unwrap_or(0);
        return AxisSample {
            lower: nearest,
            upper: nearest,
            fraction: 0.0,
        };
    }
    if position <= values[0] {
        return AxisSample {
            lower: 0,
            upper: 0,
            fraction: 0.0,
        };
    }
    // Fast path: assume an evenly-spaced axis (true for the cartesian x/y axes
    // and the polar elevation/distance axes) and jump straight to the bracket in
    // O(1). Verify the guess against its neighbours so a non-uniform axis (e.g.
    // the two-region cartesian z) correctly falls through to the binary search.
    // The fraction is computed from the stored grid values either way, so the
    // result is bit-identical to the search path.
    let last = values.len() - 1;
    let step = (values[last] - values[0]) / last as f32;
    if step > 0.0 {
        let guess = (((position - values[0]) / step) as usize).min(last - 1);
        if position >= values[guess] && position <= values[guess + 1] {
            let span = (values[guess + 1] - values[guess]).max(1e-6);
            return AxisSample {
                lower: guess,
                upper: guess + 1,
                fraction: ((position - values[guess]) / span).clamp(0.0, 1.0),
            };
        }
    }
    let upper = values.partition_point(|value| *value < position);
    if upper >= values.len() {
        let last = values.len() - 1;
        return AxisSample {
            lower: last,
            upper: last,
            fraction: 0.0,
        };
    }
    let lower = upper.saturating_sub(1);
    let span = (values[upper] - values[lower]).max(1e-6);
    AxisSample {
        lower,
        upper,
        fraction: ((position - values[lower]) / span).clamp(0.0, 1.0),
    }
}

fn sample_wrapped_axis(values: &[f32], position: f32, interpolate: bool) -> AxisSample {
    if values.len() <= 1 {
        return AxisSample {
            lower: 0,
            upper: 0,
            fraction: 0.0,
        };
    }
    if !interpolate {
        let nearest = values
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                wrapped_angle_distance(**a, position)
                    .total_cmp(&wrapped_angle_distance(**b, position))
            })
            .map(|(index, _)| index)
            .unwrap_or(0);
        return AxisSample {
            lower: nearest,
            upper: nearest,
            fraction: 0.0,
        };
    }
    let mut best = AxisSample {
        lower: 0,
        upper: 0,
        fraction: 0.0,
    };
    let mut best_distance = f32::MAX;
    for index in 0..values.len() {
        let next = (index + 1) % values.len();
        let start = values[index];
        let end = if next == 0 {
            values[0] + 360.0
        } else {
            values[next]
        };
        let value = if position < start {
            position + 360.0
        } else {
            position
        };
        if value < start || value > end {
            continue;
        }
        let span = (end - start).max(1e-6);
        return AxisSample {
            lower: index,
            upper: next,
            fraction: ((value - start) / span).clamp(0.0, 1.0),
        };
    }
    for (index, axis) in values.iter().enumerate() {
        let distance = wrapped_angle_distance(*axis, position);
        if distance < best_distance {
            best_distance = distance;
            best = AxisSample {
                lower: index,
                upper: index,
                fraction: 0.0,
            };
        }
    }
    best
}

#[inline]
fn wrap_degrees(value: f32) -> f32 {
    let wrapped = (value + 180.0).rem_euclid(360.0) - 180.0;
    if wrapped == -180.0 { 180.0 } else { wrapped }
}

#[inline]
fn wrapped_angle_distance(a: f32, b: f32) -> f32 {
    let delta = (a - b).abs().rem_euclid(360.0);
    delta.min(360.0 - delta)
}

#[cfg(test)]
mod cartesian_lookup_bench {
    //! Microbench of the cartesian table lookup, contrasting the production
    //! `AxisLut` (division-free `inv_step` index for x/y + split z) against the
    //! generic binary-search path (`AxisLut::Irregular`, the pre-optimisation
    //! behaviour). Both go through `sample_cartesian_table`, so this isolates the
    //! lookup-localisation cost the `inv_step` precompute removed.
    //!
    //! Run: cargo test -p renderer --release cartesian_lookup_bench -- --ignored --nocapture
    use super::*;
    use std::hint::black_box;
    use std::time::Instant;

    #[test]
    #[ignore = "microbenchmark, run explicitly: cargo test -p renderer --release \
                cartesian_lookup_bench -- --ignored --nocapture"]
    fn inv_step_vs_search() {
        let sc = 12usize;
        let xs = evenly_spaced_axis(31, -1.0, 1.0);
        let ys = evenly_spaced_axis(31, -1.0, 1.0);
        let zs = cartesian_z_axis(15, 15);
        let (nx, ny, nz) = (xs.len(), ys.len(), zs.len());
        let mut table = vec![0.0f32; nx * ny * nz * sc];
        for (i, v) in table.iter_mut().enumerate() {
            *v = ((i * 2654435761) % 1000) as f32 / 1000.0;
        }

        // Production luts (uniform x/y, split z) vs forced binary-search luts.
        let (fx, fy, fz) = (
            AxisLut::from_values(&xs),
            AxisLut::from_values(&ys),
            AxisLut::from_values(&zs),
        );
        let (sx, sy, sz) = (
            AxisLut::Irregular(xs.clone()),
            AxisLut::Irregular(ys.clone()),
            AxisLut::Irregular(zs.clone()),
        );

        let n = 40;
        let positions: Vec<[f32; 3]> = (0..n)
            .map(|s| {
                let t = s as f32 / (n - 1) as f32;
                [-0.3 + 0.6 * t, -0.2 + 0.4 * t, -0.1 + 0.3 * t]
            })
            .collect();

        let reps = 300_000;
        let run = |x: &AxisLut, y: &AxisLut, z: &AxisLut| {
            for p in &positions {
                black_box(sample_cartesian_table(&table, sc, x, y, z, *p, true));
            }
        };
        run(&fx, &fy, &fz); // warm up

        let t0 = Instant::now();
        for _ in 0..reps {
            run(&fx, &fy, &fz);
        }
        let inv = t0.elapsed();

        let t1 = Instant::now();
        for _ in 0..reps {
            run(&sx, &sy, &sz);
        }
        let search = t1.elapsed();

        let calls = (reps * n) as f64;
        let inv_ns = inv.as_secs_f64() * 1e9 / calls;
        let search_ns = search.as_secs_f64() * 1e9 / calls;
        eprintln!(
            "cartesian lookup: inv_step {inv_ns:.1} ns/call | binary-search {search_ns:.1} ns/call \
             | inv_step is {:.0}% faster",
            (search_ns - inv_ns) / search_ns * 100.0,
        );
    }
}

#[cfg(test)]
mod polar_lut_tests {
    //! Bit-equivalence guards for the division-free polar lookup
    //! (`sample_polar_table_lut` + `AzimuthLut`) against the reference scan path
    //! (`sample_polar_table`), which is the production behaviour it replaces.
    use super::*;

    /// Deterministic, reproducible "table" value for a flat index.
    fn synth(i: usize) -> f32 {
        let x = (i as u32).wrapping_mul(2_654_435_761);
        ((x >> 8) & 0xffff) as f32 / 65535.0 - 0.5
    }

    /// Sweep azimuth / elevation / distance over in-cell offsets (avoiding exact
    /// midpoints, where nearest-mode tie-breaking is allowed to differ) plus a few
    /// out-of-range values (to exercise clamping), and require the LUT lookup to
    /// match the reference within f32 rounding, in both interpolate and nearest
    /// modes.
    #[test]
    fn polar_lut_matches_reference() {
        let azimuth_positions = polar_azimuth_axis(24);
        let elevation_positions = polar_elevation_axis(9, true);
        let distance_positions = evenly_spaced_axis(6, 0.0, 2.0);
        let speaker_count = 7;
        let n = azimuth_positions.len() * elevation_positions.len() * distance_positions.len();
        let table: Vec<f32> = (0..n * speaker_count).map(synth).collect();

        let az_lut = AzimuthLut::from_values(&azimuth_positions);
        let el_lut = AxisLut::from_values(&elevation_positions);
        let dist_lut = AxisLut::from_values(&distance_positions);

        // Build sweep points: each grid point plus 0.3 / 0.7 of a step, plus a few
        // out-of-range probes.
        let sweep = |grid: &[f32], lo: f32, hi: f32| -> Vec<f32> {
            let step = (grid[grid.len() - 1] - grid[0]) / (grid.len() - 1) as f32;
            let mut v = Vec::new();
            for &g in grid {
                v.push(g);
                v.push(g + 0.3 * step);
                v.push(g + 0.7 * step);
            }
            v.push(lo - 5.0);
            v.push(hi + 5.0);
            v
        };
        let az_sweep = sweep(&azimuth_positions, -180.0, 180.0);
        let el_sweep = sweep(&elevation_positions, -90.0, 90.0);
        let dist_sweep = sweep(&distance_positions, 0.0, 2.0);

        for &interpolate in &[true, false] {
            let mut max_diff = 0.0f32;
            for &az in &az_sweep {
                for &el in &el_sweep {
                    for &dist in &dist_sweep {
                        let reference = sample_polar_table(
                            &table,
                            speaker_count,
                            &azimuth_positions,
                            &elevation_positions,
                            &distance_positions,
                            [az, el, dist],
                            interpolate,
                        );
                        let lut = sample_polar_table_lut(
                            &table,
                            speaker_count,
                            &az_lut,
                            &el_lut,
                            &dist_lut,
                            [az, el, dist],
                            interpolate,
                        );
                        for (a, b) in reference.iter().zip(lut.iter()) {
                            max_diff = max_diff.max((a - b).abs());
                        }
                    }
                }
            }
            assert!(
                max_diff < 1e-5,
                "polar LUT vs reference mismatch (interpolate={interpolate}): max diff {max_diff}"
            );
        }
    }

    /// The wrapped azimuth seam must read the same physical cell as the reference.
    /// At the `±180°` boundary index `len-1` and index `0` are `360°` apart; the
    /// two paths may name the bracket differently (e.g. `{15,0,1.0}` vs
    /// `{0,1,0.0}`) yet must yield the same interpolated value. Compare the actual
    /// weighted read, which is the invariant that matters.
    #[test]
    fn azimuth_seam_wraps_like_reference() {
        let values = polar_azimuth_axis(16);
        let lut = AzimuthLut::from_values(&values);
        assert!(matches!(lut, AzimuthLut::WrappedUniform { .. }));
        let tbl: Vec<f32> = (0..values.len()).map(synth).collect();
        let read = |s: AxisSample| tbl[s.lower] * (1.0 - s.fraction) + tbl[s.upper] * s.fraction;
        let step = 360.0 / values.len() as f32;
        for &pos in &[
            180.0 - 0.25 * step,
            180.0,
            -180.0,
            -180.0 + 0.25 * step,
            540.0,
        ] {
            let want = read(sample_wrapped_axis(&values, wrap_degrees(pos), true));
            let got = read(lut.sample(pos, true));
            assert!(
                (want - got).abs() < 1e-5,
                "azimuth seam mismatch at {pos}: want {want} got {got}"
            );
        }
    }
}

#[cfg(test)]
mod size_interval_tests {
    //! Object-size interval precompute: a `SizeInterpolatingEvaluator` built with
    //! `object_size_intervals = N` bakes `N + 1` position tables at isotropic
    //! sizes and interpolates between them at read time, so the precomputed modes
    //! honour object size (a single table freezes it).
    use super::*;
    use crate::spatial_vbap::{DistanceMetric, DistanceModel, VbapPanner};

    /// 4-speaker horizontal VBAP backend. `supports_event_size` is true and the
    /// default spread range is [0, 1], so a non-zero `event_size` widens the pan.
    fn make_model() -> Box<dyn GainModel> {
        let positions = [[-30.0, 0.0], [30.0, 0.0], [-110.0, 0.0], [110.0, 0.0]];
        let panner = VbapPanner::new(&positions, 5, 5, 0.0, Default::default())
            .expect("panner")
            .with_negative_z(true);
        Box::new(VbapBackend::new(panner, VbapSpreadParams::default()))
    }

    fn config(intervals: usize, event_size: [f32; 3]) -> EvaluationBuildConfig {
        EvaluationBuildConfig {
            request_template: RenderRequest {
                adm_position: [0.0, 0.0, 0.0],
                event_size,
                room_ratio: [1.0, 1.0, 1.0],
                room_ratio_rear: 1.0,
                room_ratio_lower: 1.0,
                room_ratio_center_blend: 0.0,
                use_distance_diffuse: false,
                diffuse_mirror_axes: crate::spatial_vbap::MirrorAxes::default(),
                distance_diffuse_threshold: 1.0,
                distance_diffuse_curve: 1.0,
                distance_model: DistanceModel::None,
            },
            position_interpolation: true,
            cartesian: CartesianEvaluationConfig {
                x_size: 9,
                y_size: 9,
                z_size: 5,
                z_neg_size: 0,
            },
            polar: PolarEvaluationConfig {
                azimuth_values: 16,
                elevation_values: 7,
                distance_values: 4,
                distance_max: 1.0,
                allow_negative_z: false,
            },
            distance_model_metric: DistanceMetric::default(),
            distance_diffuse_metric: DistanceMetric::default(),
            object_size_intervals: intervals,
            object_size_mode: SizeToSpreadMode::Max,
        }
    }

    fn request(pos: [f64; 3], size: [f32; 3]) -> RenderRequest {
        let mut req = config(0, [0.0; 3]).request_template;
        req.adm_position = pos;
        req.event_size = size;
        req
    }

    fn engine(
        mode: EffectiveEvaluationMode,
        intervals: usize,
        frozen_size: [f32; 3],
    ) -> PreparedRenderEngine {
        build_prepared_render_engine(make_model(), mode, &config(intervals, frozen_size))
            .expect("engine builds")
    }

    fn assert_close(a: &Gains, b: &Gains, eps: f32) {
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b.iter()) {
            assert!((x - y).abs() < eps, "{x} vs {y}");
        }
    }

    /// A bridge that advertises `allow_negative_z` does not imply the cartesian
    /// grid has negative-z cells (`z_neg_size` defaults to 0). Below-horizon
    /// requests against such a table must clamp onto the `z = 0` plane — finite
    /// gains, identical to the same position at `z = 0` — never an
    /// out-of-bounds lookup or an extrapolation. The realtime path has no grid
    /// and reaches the panner's out-of-hull handling directly.
    #[test]
    fn cartesian_without_neg_cells_clamps_below_horizon_requests() {
        let table = engine(EffectiveEvaluationMode::PrecomputedCartesian, 0, [0.0; 3]);
        for (below, at_plane) in [
            ([0.3, 0.1, -0.4], [0.3, 0.1, 0.0]),
            ([-0.6, 0.2, -1.0], [-0.6, 0.2, 0.0]),
            ([0.0, 0.0, -1.0], [0.0, 0.0, 0.0]),
        ] {
            let below_gains = table.compute_gains(&request(below, [0.0; 3])).gains;
            let plane_gains = table.compute_gains(&request(at_plane, [0.0; 3])).gains;
            for g in below_gains.iter() {
                assert!(g.is_finite(), "non-finite gain for {below:?}");
            }
            assert_close(&below_gains, &plane_gains, 1e-6);
        }

        let realtime = engine(EffectiveEvaluationMode::Realtime, 0, [0.0; 3]);
        let gains = realtime
            .compute_gains(&request([0.3, 0.1, -0.4], [0.0; 3]))
            .gains;
        let energy: f32 = gains.iter().map(|g| g * g).sum::<f32>().sqrt();
        for g in gains.iter() {
            assert!(g.is_finite(), "non-finite realtime gain below the horizon");
        }
        assert!(
            energy > 0.5,
            "below-horizon realtime request should render at level, got rms {energy}"
        );
    }

    /// At the sampled endpoints (size 0 and size 1) the interval engine reproduces
    /// the single tables frozen at those sizes, and object size visibly matters.
    fn endpoints_match_frozen_tables(mode: EffectiveEvaluationMode) {
        let pos = [0.3, 0.1, 0.2];
        let intervals = engine(mode, 1, [0.0; 3]);
        let small = engine(mode, 0, [0.0; 3]);
        let large = engine(mode, 0, [1.0; 3]);

        let at_small = intervals.compute_gains(&request(pos, [0.0; 3])).gains;
        let at_large = intervals.compute_gains(&request(pos, [1.0; 3])).gains;
        assert_close(
            &at_small,
            &small.compute_gains(&request(pos, [0.0; 3])).gains,
            1e-6,
        );
        assert_close(
            &at_large,
            &large.compute_gains(&request(pos, [1.0; 3])).gains,
            1e-6,
        );

        let max_diff = at_small
            .iter()
            .zip(at_large.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(max_diff > 1e-3, "object size had no effect: {max_diff}");
    }

    #[test]
    fn cartesian_endpoints_match_frozen_tables() {
        endpoints_match_frozen_tables(EffectiveEvaluationMode::PrecomputedCartesian);
    }

    #[test]
    fn polar_endpoints_match_frozen_tables() {
        endpoints_match_frozen_tables(EffectiveEvaluationMode::PrecomputedPolar);
    }

    /// At a size between two sampled sizes the result is their linear blend
    /// (N = 1 ⇒ sizes {0, 1}, query 0.5 ⇒ equal weights).
    #[test]
    fn cartesian_midpoint_is_blend_of_endpoints() {
        let pos = [0.3, 0.1, 0.2];
        let intervals = engine(EffectiveEvaluationMode::PrecomputedCartesian, 1, [0.0; 3]);
        let small = engine(EffectiveEvaluationMode::PrecomputedCartesian, 0, [0.0; 3]);
        let large = engine(EffectiveEvaluationMode::PrecomputedCartesian, 0, [1.0; 3]);

        let mid = intervals.compute_gains(&request(pos, [0.5; 3])).gains;
        let s = small.compute_gains(&request(pos, [0.0; 3])).gains;
        let l = large.compute_gains(&request(pos, [1.0; 3])).gains;
        for i in 0..mid.len() {
            let expected = 0.5 * s[i] + 0.5 * l[i];
            assert!(
                (mid[i] - expected).abs() < 1e-6,
                "i={i}: {} vs {expected}",
                mid[i]
            );
        }
    }

    /// With intervals = 0 (the default) the single table freezes object size: the
    /// request's `event_size` is ignored, matching the pre-feature behaviour.
    #[test]
    fn intervals_zero_freezes_object_size() {
        let pos = [0.3, 0.1, 0.2];
        let engine = engine(EffectiveEvaluationMode::PrecomputedCartesian, 0, [0.0; 3]);
        let g0 = engine.compute_gains(&request(pos, [0.0; 3])).gains;
        let g1 = engine.compute_gains(&request(pos, [1.0; 3])).gains;
        assert_close(&g0, &g1, 1e-9);
    }
}
