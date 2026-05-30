use anyhow::Result;

use super::{BackendCapabilities, GainModel, GainModelKind, RenderRequest, RenderResponse};
use crate::spatial_vbap::{DistanceMetric, Gains};
use crate::speaker_layout::SpeakerLayout;

/// Decorator that applies distance-based antipodal diffuse blending to the gains
/// produced by any inner gain model.
///
/// The source's gains are blended with the gains of its horizontal antipode
/// `(-x, -y, z)` (same elevation, opposite azimuth); the mix is driven by the
/// ADM distance, pulling toward a diffuse field as the source approaches the
/// centre. Like the distance model, this used to live inside the VBAP backend;
/// extracting it into a decorator makes it available to every backend.
///
/// Applied *under* the distance-model decorator so the energy renormalization
/// here does not cancel the distance attenuation. When `use_distance_diffuse`
/// is false it is a no-op (and skips the second backend evaluation).
pub struct DistanceDiffuseModel {
    inner: Box<dyn GainModel>,
    metric: DistanceMetric,
}

impl DistanceDiffuseModel {
    pub fn new(inner: Box<dyn GainModel>, metric: DistanceMetric) -> Self {
        Self { inner, metric }
    }
}

impl GainModel for DistanceDiffuseModel {
    fn kind(&self) -> GainModelKind {
        self.inner.kind()
    }

    fn backend_id(&self) -> &'static str {
        self.inner.backend_id()
    }

    fn backend_label(&self) -> &'static str {
        self.inner.backend_label()
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            supports_distance_diffuse: true,
            ..self.inner.capabilities()
        }
    }

    fn speaker_count(&self) -> usize {
        self.inner.speaker_count()
    }

    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        if !req.use_distance_diffuse {
            return self.inner.compute_gains(req);
        }

        let direct = self.inner.compute_gains(req).gains;

        // Horizontal antipode in ADM space: flip azimuth, keep elevation. The
        // backend applies its own room scaling, which reproduces the mirrored
        // scaled coordinates (the depth warp is an odd function).
        let mut mirror_req = *req;
        mirror_req.adm_position = [
            -req.adm_position[0],
            -req.adm_position[1],
            req.adm_position[2],
        ];
        let mirror = self.inner.compute_gains(&mirror_req).gains;

        // Blend weight from the (raw) ADM distance, under the selected metric.
        let adm_dist = self.metric.measure(req.adm_position.map(|v| v as f32));
        let t = (adm_dist / req.distance_diffuse_threshold.max(1e-6))
            .min(1.0)
            .powf(req.distance_diffuse_curve);
        let alpha = 0.5 + 0.5 * t;
        let w_direct = alpha.sqrt();
        let w_mirror = (1.0 - alpha).sqrt();

        let n = direct.len().min(mirror.len());
        let mut blended = Gains::zeroed(self.inner.speaker_count());
        let mut energy_direct = 0.0f32;
        let mut energy_blended = 0.0f32;
        for i in 0..n {
            let g = w_direct * direct[i] + w_mirror * mirror[i];
            blended.set(i, g);
            energy_direct += direct[i] * direct[i];
            energy_blended += g * g;
        }

        if energy_blended > 1e-12 {
            let scale = (energy_direct / energy_blended).sqrt();
            for g in blended.iter_mut() {
                *g *= scale;
            }
        }

        RenderResponse { gains: blended }
    }

    fn save_to_file(&self, path: &std::path::Path, speaker_layout: &SpeakerLayout) -> Result<()> {
        self.inner.save_to_file(path, speaker_layout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render_backend::BarycenterBackend;

    fn request(position: [f64; 3], diffuse: bool) -> RenderRequest {
        RenderRequest {
            adm_position: position,
            event_size: [0.0, 0.0, 0.0],
            size_to_spread_mode: Default::default(),
            spread_min: 0.0,
            spread_max: 0.0,
            spread_from_distance: false,
            spread_distance_range: 1.0,
            spread_distance_curve: 1.0,
            room_ratio: [1.0, 1.0, 1.0],
            room_ratio_rear: 1.0,
            room_ratio_lower: 1.0,
            room_ratio_center_blend: 0.0,
            use_distance_diffuse: diffuse,
            distance_diffuse_threshold: 1.0,
            distance_diffuse_curve: 1.0,
            distance_model: crate::spatial_vbap::DistanceModel::None,
            barycenter_localize: 0.0,
            experimental_distance_distance_floor: 0.0,
            experimental_distance_min_active_speakers: 1,
            experimental_distance_max_active_speakers: 2,
            experimental_distance_position_error_floor: 0.0,
            experimental_distance_position_error_nearest_scale: 0.0,
            experimental_distance_position_error_span_scale: 0.0,
        }
    }

    fn speakers() -> Vec<[f32; 3]> {
        vec![[-1.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, -1.0, 0.0]]
    }

    fn wrapped() -> DistanceDiffuseModel {
        DistanceDiffuseModel::new(
            Box::new(BarycenterBackend::new(speakers())),
            DistanceMetric::Spherical,
        )
    }

    #[test]
    fn disabled_is_a_noop() {
        let position = [0.4, 0.2, 0.1];
        let decorated = wrapped().compute_gains(&request(position, false)).gains;
        let bare = BarycenterBackend::new(speakers())
            .compute_gains(&request(position, false))
            .gains;
        for (a, b) in decorated.iter().zip(bare.iter()) {
            assert!((a - b).abs() < 1e-6, "{a} vs {b}");
        }
    }

    #[test]
    fn enabled_preserves_direct_energy_on_a_non_vbap_backend() {
        // The renorm targets the direct energy, so the blended vector keeps the
        // direct backend's energy regardless of the mirror contribution.
        let position = [0.3, 0.1, 0.2];
        let blended = wrapped().compute_gains(&request(position, true)).gains;
        let direct = BarycenterBackend::new(speakers())
            .compute_gains(&request(position, true))
            .gains;
        let energy_blended: f32 = blended.iter().map(|g| g * g).sum();
        let energy_direct: f32 = direct.iter().map(|g| g * g).sum();
        assert!(
            (energy_blended - energy_direct).abs() < 1e-4,
            "blended={energy_blended} direct={energy_direct}"
        );
    }

    #[test]
    fn capabilities_advertise_distance_diffuse() {
        assert!(wrapped().capabilities().supports_distance_diffuse);
    }
}
