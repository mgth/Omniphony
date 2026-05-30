use anyhow::Result;

use super::{
    BackendCapabilities, GainModel, GainModelKind, RenderRequest, RenderResponse,
    reduce_size_to_spread,
};
use super::room_transform::map_depth_with_room_ratios;
use crate::spatial_vbap::{VbapPanner, adm_to_spherical};
use crate::speaker_layout::SpeakerLayout;

pub struct VbapBackend {
    panner: VbapPanner,
}

impl VbapBackend {
    pub fn new(panner: VbapPanner) -> Self {
        Self { panner }
    }

    pub fn speaker_count(&self) -> usize {
        self.panner.num_speakers()
    }

    pub fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        let rendering_position = req.adm_position;
        let scaled_x = rendering_position[0] as f32 * req.room_ratio[0];
        let scaled_y = map_depth_with_room_ratios(
            rendering_position[1] as f32,
            req.room_ratio[1],
            req.room_ratio_rear,
            req.room_ratio_center_blend,
        );
        let scaled_z = if rendering_position[2] >= 0.0 {
            rendering_position[2] as f32 * req.room_ratio[2]
        } else {
            rendering_position[2] as f32 * req.room_ratio_lower
        };

        // Per-event 3-D size → scalar policy. `[0; 3]` yields 0, preserving the
        // legacy behaviour for streams that don't carry size metadata.
        let intrinsic = reduce_size_to_spread(
            req.event_size,
            [scaled_x, scaled_y, scaled_z],
            req.size_to_spread_mode,
        );

        let effective_spread = if req.spread_from_distance {
            let (_, _, dist) = adm_to_spherical(scaled_x, scaled_y, scaled_z);
            let t = (1.0 - dist / req.spread_distance_range)
                .clamp(0.0, 1.0)
                .powf(req.spread_distance_curve);
            (req.spread_min + t * (req.spread_max - req.spread_min)).clamp(0.0, 1.0)
        } else {
            // `[spread_min, spread_max]` is now used as the output range that
            // bounds the per-event intrinsic. This also fixes the latent bug
            // where `spread_max` was ignored when `spread_from_distance` was
            // off: an `event_size = [0;3]` still yields `spread_min` (legacy
            // compatibility), while `intrinsic = 1.0` reaches `spread_max`.
            (req.spread_min + intrinsic * (req.spread_max - req.spread_min)).clamp(0.0, 1.0)
        };

        // Distance diffuse blending is applied by the shared DistanceDiffuseModel
        // decorator; VBAP returns pure panning gains.
        let gains =
            self.panner
                .get_gains_cartesian(scaled_x, scaled_y, scaled_z, effective_spread);

        RenderResponse { gains }
    }

    pub fn save_to_file(
        &self,
        path: &std::path::Path,
        speaker_layout: &SpeakerLayout,
    ) -> Result<()> {
        self.panner
            .save_to_file(path, speaker_layout)
            .map_err(|e| anyhow::anyhow!("Failed to save VBAP table: {}", e))
    }
}

impl GainModel for VbapBackend {
    fn kind(&self) -> GainModelKind {
        GainModelKind::Vbap
    }

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
            supports_heatmap_cartesian: true,
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
