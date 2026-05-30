use anyhow::Result;

use super::room_transform::room_scaled_position;
use super::{BackendCapabilities, GainModel, GainModelKind, RenderRequest, RenderResponse};
use crate::spatial_vbap::{DistanceMetric, calculate_distance_attenuation};
use crate::speaker_layout::SpeakerLayout;

/// Decorator that applies the distance attenuation model to the gains produced
/// by any inner gain model.
///
/// Distance falloff is orthogonal to panning, so it is applied once on the final
/// gain vector rather than baked into a single backend. This makes the distance
/// model available to every backend — and, crucially, to the hybrid backend
/// *after* its blend + energy renormalization, so the renorm no longer cancels
/// the attenuation. With `DistanceModel::None` the attenuation is `1.0`, i.e. a
/// no-op, preserving prior behaviour.
pub struct DistanceAttenuatedModel {
    inner: Box<dyn GainModel>,
    metric: DistanceMetric,
}

impl DistanceAttenuatedModel {
    pub fn new(inner: Box<dyn GainModel>, metric: DistanceMetric) -> Self {
        Self { inner, metric }
    }
}

impl GainModel for DistanceAttenuatedModel {
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
        // The distance model is now provided by this decorator for every
        // backend, regardless of what the inner backend advertises.
        BackendCapabilities {
            supports_distance_model: true,
            ..self.inner.capabilities()
        }
    }

    fn speaker_count(&self) -> usize {
        self.inner.speaker_count()
    }

    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        let mut response = self.inner.compute_gains(req);
        if req.distance_model == crate::spatial_vbap::DistanceModel::None {
            return response;
        }
        let scaled = room_scaled_position(
            req.adm_position.map(|value| value as f32),
            req.room_ratio,
            req.room_ratio_rear,
            req.room_ratio_lower,
            req.room_ratio_center_blend,
        );
        let distance = self.metric.measure(scaled);
        let attenuation = calculate_distance_attenuation(distance, req.distance_model);
        for gain in response.gains.iter_mut() {
            *gain *= attenuation;
        }
        response
    }

    fn save_to_file(&self, path: &std::path::Path, speaker_layout: &SpeakerLayout) -> Result<()> {
        self.inner.save_to_file(path, speaker_layout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render_backend::BarycenterBackend;
    use crate::spatial_vbap::DistanceModel;

    fn request(position: [f64; 3], distance_model: DistanceModel) -> RenderRequest {
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
            use_distance_diffuse: false,
            distance_diffuse_threshold: 1.0,
            distance_diffuse_curve: 1.0,
            distance_model,
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

    fn wrapped() -> DistanceAttenuatedModel {
        DistanceAttenuatedModel::new(
            Box::new(BarycenterBackend::new(speakers())),
            DistanceMetric::Spherical,
        )
    }

    #[test]
    fn none_model_is_a_noop() {
        let position = [0.4, 0.2, 0.1];
        let decorated = wrapped().compute_gains(&request(position, DistanceModel::None)).gains;
        let bare = BarycenterBackend::new(speakers())
            .compute_gains(&request(position, DistanceModel::None))
            .gains;
        for (a, b) in decorated.iter().zip(bare.iter()) {
            assert!((a - b).abs() < 1e-6, "{a} vs {b}");
        }
    }

    #[test]
    fn attenuation_applies_to_a_non_vbap_backend() {
        // The distance model now works for any backend, not just VBAP.
        let position = [0.6, 0.0, 0.0];
        let model = wrapped();
        let gains = model.compute_gains(&request(position, DistanceModel::Linear)).gains;
        let energy: f32 = gains.iter().map(|g| g * g).sum();
        // Barycenter alone is unit energy; Linear attenuation at distance 0.6 is
        // 1/(1+0.6) ≈ 0.625, so energy ≈ 0.625² ≈ 0.39 < 1.
        let expected = (1.0f32 / 1.6).powi(2);
        assert!((energy - expected).abs() < 1e-3, "energy={energy}, expected≈{expected}");
    }

    #[test]
    fn chebyshev_metric_uses_max_norm() {
        // At (0.6, 0.5, 0.0): spherical ≈ 0.781, chebyshev = 0.6 → different
        // attenuation, confirming the metric is honoured.
        let position = [0.6, 0.5, 0.0];
        let chebyshev = DistanceAttenuatedModel::new(
            Box::new(BarycenterBackend::new(speakers())),
            DistanceMetric::Chebyshev,
        );
        let gains = chebyshev.compute_gains(&request(position, DistanceModel::Linear)).gains;
        let energy: f32 = gains.iter().map(|g| g * g).sum();
        let expected = (1.0f32 / (1.0 + 0.6)).powi(2);
        assert!((energy - expected).abs() < 1e-3, "energy={energy}, expected≈{expected}");
    }

    #[test]
    fn capabilities_advertise_distance_model() {
        assert!(wrapped().capabilities().supports_distance_model);
    }
}
