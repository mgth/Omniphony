use anyhow::Result;

use super::room_transform::room_scaled_position;
use super::{
    BackendCapabilities, GainModel, RenderRequest, RenderResponse, SizeToSpreadMode,
    reduce_size_to_spread,
};
use crate::spatial_vbap::{VbapPanner, adm_to_spherical};
use crate::speaker_layout::SpeakerLayout;

/// VBAP spread tuning, baked into the backend at build time (read from the
/// generic backend-param bag, see `backend_registry::vbap_spread_params`). These
/// are *not* per-request inputs: changing one triggers a topology rebuild, so the
/// hot-path `compute_gains` reads them from `self` rather than from the request.
///
/// The only genuinely per-event spread input is `RenderRequest::event_size`,
/// which carries per-object metadata and stays on the request.
#[derive(Debug, Clone, Copy)]
pub struct VbapSpreadParams {
    pub spread_min: f32,
    pub spread_max: f32,
    pub spread_from_distance: bool,
    pub spread_distance_range: f32,
    pub spread_distance_curve: f32,
    pub size_to_spread_mode: SizeToSpreadMode,
}

impl Default for VbapSpreadParams {
    fn default() -> Self {
        Self {
            spread_min: 0.0,
            spread_max: 1.0,
            spread_from_distance: false,
            spread_distance_range: 1.0,
            spread_distance_curve: 1.0,
            size_to_spread_mode: SizeToSpreadMode::default(),
        }
    }
}

pub struct VbapBackend {
    panner: VbapPanner,
    spread: VbapSpreadParams,
}

impl VbapBackend {
    pub fn new(panner: VbapPanner, spread: VbapSpreadParams) -> Self {
        Self { panner, spread }
    }

    pub fn speaker_count(&self) -> usize {
        self.panner.num_speakers()
    }

    /// The panner, for a model built on this one (the volumetric backend
    /// reads its triangulation).
    pub(crate) fn panner(&self) -> &VbapPanner {
        &self.panner
    }

    pub fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        let [scaled_x, scaled_y, scaled_z] = room_scaled_position(
            req.adm_position.map(|v| v as f32),
            req.room_ratio,
            req.room_ratio_rear,
            req.room_ratio_lower,
            req.room_ratio_center_blend,
        );

        // Per-event 3-D size → scalar policy. `[0; 3]` yields 0, preserving the
        // legacy behaviour for streams that don't carry size metadata.
        // `event_size` is the only per-request spread input; the policy and the
        // output range are baked tuning (see `VbapSpreadParams`).
        let intrinsic = reduce_size_to_spread(
            req.event_size,
            [scaled_x, scaled_y, scaled_z],
            self.spread.size_to_spread_mode,
        );

        let effective_spread = if self.spread.spread_from_distance {
            let (_, _, dist) = adm_to_spherical(scaled_x, scaled_y, scaled_z);
            let t = (1.0 - dist / self.spread.spread_distance_range)
                .clamp(0.0, 1.0)
                .powf(self.spread.spread_distance_curve);
            (self.spread.spread_min + t * (self.spread.spread_max - self.spread.spread_min))
                .clamp(0.0, 1.0)
        } else {
            // `[spread_min, spread_max]` is now used as the output range that
            // bounds the per-event intrinsic. This also fixes the latent bug
            // where `spread_max` was ignored when `spread_from_distance` was
            // off: an `event_size = [0;3]` still yields `spread_min` (legacy
            // compatibility), while `intrinsic = 1.0` reaches `spread_max`.
            (self.spread.spread_min + intrinsic * (self.spread.spread_max - self.spread.spread_min))
                .clamp(0.0, 1.0)
        };

        // Distance diffuse blending is applied by the shared DistanceDiffuseModel
        // decorator; VBAP returns pure panning gains.
        let gains = self
            .panner
            .get_gains_cartesian(scaled_x, scaled_y, scaled_z, effective_spread);

        RenderResponse { gains }
    }

    pub fn save_to_file(
        &self,
        path: &std::path::Path,
        speaker_layout: &SpeakerLayout,
    ) -> Result<()> {
        let _ = (path, speaker_layout);
        // The VBAP backend holds no precomputed table to serialize (the panner is
        // geometry-only). Precomputed tables are exported from the evaluation
        // layer (`render_backend::evaluation_artifact`), not from the backend.
        anyhow::bail!(
            "VBAP backend is geometry-only; export the precomputed table via the evaluator"
        )
    }
}

impl GainModel for VbapBackend {
    fn backend_id(&self) -> &'static str {
        "vbap"
    }

    fn backend_label(&self) -> &'static str {
        "VBAP"
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            supports_realtime: true,
            supports_precomputed_polar: true,
            supports_precomputed_cartesian: true,
            supports_position_interpolation: true,
            supports_distance_model: true,
            supports_spread: true,
            supports_spread_from_distance: true,
            supports_event_size: true,
            supports_distance_diffuse: true,
            supports_table_export: true,
        }
    }

    fn speaker_count(&self) -> usize {
        VbapBackend::speaker_count(self)
    }

    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        VbapBackend::compute_gains(self, req)
    }

    fn save_to_file(&self, path: &std::path::Path, speaker_layout: &SpeakerLayout) -> Result<()> {
        VbapBackend::save_to_file(self, path, speaker_layout)
    }
}
