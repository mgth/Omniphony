use anyhow::Result;

use super::{BackendCapabilities, GainModel, GainModelKind, RenderRequest, RenderResponse};
use crate::spatial_vbap::{Gains, VbapPanner, adm_to_spherical};
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

        let effective_spread = if req.spread_from_distance {
            let (_, _, dist) = adm_to_spherical(scaled_x, scaled_y, scaled_z);
            let t = (1.0 - dist / req.spread_distance_range)
                .clamp(0.0, 1.0)
                .powf(req.spread_distance_curve);
            (req.spread_min + t * (req.spread_max - req.spread_min)).clamp(0.0, 1.0)
        } else {
            req.spread_min.clamp(0.0, 1.0)
        };

        let direct_gains = self.panner.get_gains_cartesian_raw(
            scaled_x,
            scaled_y,
            scaled_z,
            effective_spread,
            req.distance_model,
        );

        let gains = if req.use_distance_diffuse {
            let [rx, ry, rz] = rendering_position;
            let adm_dist = ((rx * rx + ry * ry + rz * rz) as f32).sqrt();
            let t = (adm_dist / req.distance_diffuse_threshold.max(1e-6))
                .min(1.0)
                .powf(req.distance_diffuse_curve);
            let alpha = 0.5 + 0.5 * t;
            let w_direct = alpha.sqrt();
            let w_mirror = (1.0 - alpha).sqrt();
            let mirror_gains = self.panner.get_gains_cartesian_raw(
                -scaled_x,
                -scaled_y,
                scaled_z,
                effective_spread,
                req.distance_model,
            );

            let n = direct_gains.len();
            let mut blended = Gains::zeroed(n);
            let mut energy_direct = 0.0f32;
            let mut energy_blended = 0.0f32;
            for i in 0..n {
                let g = w_direct * direct_gains[i] + w_mirror * mirror_gains[i];
                blended.set(i, g);
                energy_direct += direct_gains[i] * direct_gains[i];
                energy_blended += g * g;
            }

            if energy_blended > 1e-12 {
                let scale = (energy_direct / energy_blended).sqrt();
                for g in blended.iter_mut() {
                    *g *= scale;
                }
            }

            blended
        } else {
            direct_gains
        };

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
