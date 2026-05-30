use anyhow::Result;

use super::{BackendCapabilities, GainModel, GainModelKind, RenderRequest, RenderResponse};
use crate::spatial_vbap::{DistanceMetric, Gains};
use crate::speaker_layout::SpeakerLayout;

/// Largest distance from the cube centre under each metric, used to normalise the
/// blend curve's X axis to `[0, 1]`. Chebyshev reaches 1 on the cube surface;
/// spherical (Euclidean) reaches the cube diagonal √3 at a corner.
pub fn hybrid_max_distance(metric: DistanceMetric) -> f32 {
    match metric {
        DistanceMetric::Chebyshev => 1.0,
        DistanceMetric::Spherical => 3.0_f32.sqrt(),
    }
}

/// Backend that blends two other gain models ("external" and "internal") as a
/// function of the source's normalised distance from the centre of the cube.
///
/// For each query both inner models are evaluated, the blend ratio `r` is read
/// from [`BlendCurve`] at the normalised distance, and the per-speaker gains are
/// mixed as `gain[i] = (1 - r) * internal[i] + r * external[i]` before being
/// renormalised to unit energy. `r = 1` means 100 % external, `r = 0` means
/// 100 % internal.
pub struct HybridBackend {
    external: Box<dyn GainModel>,
    internal: Box<dyn GainModel>,
    curve: BlendCurve,
    metric: DistanceMetric,
    max_distance: f32,
}

impl HybridBackend {
    pub fn new(
        external: Box<dyn GainModel>,
        internal: Box<dyn GainModel>,
        curve: BlendCurve,
        metric: DistanceMetric,
    ) -> Self {
        debug_assert_eq!(
            external.speaker_count(),
            internal.speaker_count(),
            "hybrid inner backends must share the same speaker count"
        );
        Self {
            external,
            internal,
            curve,
            metric,
            max_distance: hybrid_max_distance(metric),
        }
    }

    pub fn speaker_count(&self) -> usize {
        self.internal.speaker_count()
    }

    pub fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        let external = self.external.compute_gains(req).gains;
        let internal = self.internal.compute_gains(req).gains;

        // Distance on the raw ADM position, normalised by the metric's maximum
        // (Chebyshev: 1 on the cube surface; spherical: √3 at a corner), so the
        // blend curve's X axis stays in [0, 1] regardless of metric.
        let distance = self.metric.measure(req.adm_position.map(|v| v as f32));
        let normalized = if self.max_distance > 0.0 {
            (distance / self.max_distance).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let ratio = self.curve.eval(normalized);

        let count = internal.len().min(external.len());
        let mut gains = Gains::zeroed(self.speaker_count());
        let mut energy = 0.0f32;
        for index in 0..count {
            let mixed = (1.0 - ratio) * internal[index] + ratio * external[index];
            gains.set(index, mixed);
            energy += mixed * mixed;
        }

        // A weighted average of two unit-energy vectors is generally not
        // unit-energy (it dips toward the middle of the crossfade), so we
        // renormalise to keep loudness stable across the blend.
        if energy > 1e-12 {
            let norm = energy.sqrt();
            for gain in gains.iter_mut() {
                *gain /= norm;
            }
        }

        RenderResponse { gains }
    }

    pub fn save_to_file(
        &self,
        _path: &std::path::Path,
        _speaker_layout: &SpeakerLayout,
    ) -> Result<()> {
        Err(anyhow::anyhow!(
            "Saving a precomputed table is only supported for the VBAP backend"
        ))
    }
}

impl GainModel for HybridBackend {
    fn kind(&self) -> GainModelKind {
        GainModelKind::Hybrid
    }

    fn backend_id(&self) -> &'static str {
        "hybrid"
    }

    fn backend_label(&self) -> &'static str {
        "Hybrid"
    }

    fn capabilities(&self) -> BackendCapabilities {
        // The hybrid backend itself only composes two inner models; the inner
        // models consume the per-request content fields (distance model, spread,
        // …) directly, so we don't advertise those capabilities here.
        BackendCapabilities {
            supports_realtime: true,
            supports_precomputed_polar: true,
            supports_precomputed_cartesian: true,
            supports_position_interpolation: true,
            supports_distance_model: false,
            supports_spread: false,
            supports_spread_from_distance: false,
            supports_event_size: false,
            supports_distance_diffuse: false,
            supports_heatmap_cartesian: true,
            supports_table_export: false,
        }
    }

    fn speaker_count(&self) -> usize {
        HybridBackend::speaker_count(self)
    }

    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        HybridBackend::compute_gains(self, req)
    }

    fn save_to_file(&self, path: &std::path::Path, speaker_layout: &SpeakerLayout) -> Result<()> {
        HybridBackend::save_to_file(self, path, speaker_layout)
    }
}

/// Piecewise-linear blend curve mapping a normalised distance `x ∈ [0, 1]` to a
/// blend ratio `y ∈ [0, 1]`. Points are kept sorted by `x`; evaluation clamps to
/// the first / last point outside the defined range.
#[derive(Debug, Clone)]
pub struct BlendCurve {
    points: Vec<[f32; 2]>,
}

impl BlendCurve {
    /// Build a curve from `(x, y)` control points. Points are sorted by `x` and
    /// both axes are clamped to `[0, 1]`. An empty input falls back to the
    /// default linear ramp.
    pub fn new(mut points: Vec<[f32; 2]>) -> Self {
        for point in &mut points {
            point[0] = point[0].clamp(0.0, 1.0);
            point[1] = point[1].clamp(0.0, 1.0);
        }
        points.sort_by(|a, b| a[0].total_cmp(&b[0]));
        if points.is_empty() {
            return Self::default();
        }
        Self { points }
    }

    /// Evaluate the curve at the given normalised distance.
    #[inline]
    pub fn eval(&self, x: f32) -> f32 {
        let points = &self.points;
        // `new()` guarantees a non-empty point set.
        if x <= points[0][0] {
            return points[0][1];
        }
        let last = points.len() - 1;
        if x >= points[last][0] {
            return points[last][1];
        }
        // Linear scan: curves hold only a handful of points, so this is cheaper
        // than a binary search and branch-predicts well in the hot path.
        for window in points.windows(2) {
            let [x0, y0] = window[0];
            let [x1, y1] = window[1];
            if x <= x1 {
                let span = (x1 - x0).max(1e-6);
                let t = ((x - x0) / span).clamp(0.0, 1.0);
                return y0 + (y1 - y0) * t;
            }
        }
        points[last][1]
    }

    pub fn points(&self) -> &[[f32; 2]] {
        &self.points
    }
}

impl Default for BlendCurve {
    fn default() -> Self {
        Self {
            points: vec![[0.0, 0.0], [1.0, 1.0]],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render_backend::{BarycenterBackend, ExperimentalDistanceBackend};

    fn request(position: [f64; 3]) -> RenderRequest {
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
        vec![
            [-1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0],
        ]
    }

    fn hybrid(curve: BlendCurve) -> HybridBackend {
        HybridBackend::new(
            Box::new(ExperimentalDistanceBackend::new(speakers())),
            Box::new(BarycenterBackend::new(speakers())),
            curve,
            crate::spatial_vbap::DistanceMetric::Chebyshev,
        )
    }

    #[test]
    fn curve_eval_clamps_and_interpolates() {
        let curve = BlendCurve::new(vec![[0.0, 0.2], [0.5, 0.8], [1.0, 0.4]]);
        assert!((curve.eval(-1.0) - 0.2).abs() < 1e-6);
        assert!((curve.eval(0.0) - 0.2).abs() < 1e-6);
        assert!((curve.eval(0.25) - 0.5).abs() < 1e-6);
        assert!((curve.eval(0.5) - 0.8).abs() < 1e-6);
        assert!((curve.eval(2.0) - 0.4).abs() < 1e-6);
    }

    #[test]
    fn constant_zero_curve_matches_internal() {
        let position = [0.3, 0.1, 0.2];
        let blended = hybrid(BlendCurve::new(vec![[0.0, 0.0], [1.0, 0.0]]))
            .compute_gains(&request(position))
            .gains;
        let internal = BarycenterBackend::new(speakers())
            .compute_gains(&request(position))
            .gains;
        for (a, b) in blended.iter().zip(internal.iter()) {
            assert!((a - b).abs() < 1e-5, "{a} vs {b}");
        }
    }

    #[test]
    fn constant_one_curve_matches_external() {
        let position = [0.3, 0.1, 0.2];
        let blended = hybrid(BlendCurve::new(vec![[0.0, 1.0], [1.0, 1.0]]))
            .compute_gains(&request(position))
            .gains;
        let external = ExperimentalDistanceBackend::new(speakers())
            .compute_gains(&request(position))
            .gains;
        for (a, b) in blended.iter().zip(external.iter()) {
            assert!((a - b).abs() < 1e-5, "{a} vs {b}");
        }
    }

    #[test]
    fn build_precomputed_cartesian_with_vbap_inner_completes() {
        use crate::render_backend::{
            EffectiveEvaluationMode, EvaluationBuildConfig, VbapBackend, build_prepared_render_engine,
        };
        use crate::spatial_vbap::{VbapPanner, VbapTableMode};

        let positions = [[-30.0, 0.0], [30.0, 0.0], [-110.0, 0.0], [110.0, 0.0]];
        let panner = VbapPanner::new_with_mode(
            &positions,
            5,
            5,
            0.0,
            VbapTableMode::Cartesian {
                x_size: 33,
                y_size: 33,
                z_size: 17,
                z_neg_size: 8,
            },
        )
        .expect("vbap panner")
        .with_negative_z(true)
        .with_position_interpolation(true);
        let external: Box<dyn GainModel> = Box::new(VbapBackend::new(panner));
        let internal: Box<dyn GainModel> = Box::new(BarycenterBackend::new(speakers()));
        let model: Box<dyn GainModel> =
            Box::new(HybridBackend::new(
                external,
                internal,
                BlendCurve::default(),
                crate::spatial_vbap::DistanceMetric::Chebyshev,
            ));

        let config = EvaluationBuildConfig {
            request_template: request([0.0, 0.0, 0.0]),
            position_interpolation: true,
            cartesian: crate::render_backend::CartesianEvaluationConfig {
                x_size: 9,
                y_size: 9,
                z_size: 5,
                z_neg_size: 0,
            },
            polar: crate::render_backend::PolarEvaluationConfig {
                azimuth_values: 8,
                elevation_values: 5,
                distance_values: 4,
                distance_max: 1.0,
                allow_negative_z: false,
            },
            distance_model_metric: crate::spatial_vbap::DistanceMetric::default(),
            distance_diffuse_metric: crate::spatial_vbap::DistanceMetric::default(),
        };

        let engine = build_prepared_render_engine(
            model,
            EffectiveEvaluationMode::PrecomputedCartesian,
            &config,
        )
        .expect("build hybrid engine");
        assert_eq!(engine.speaker_count(), 4);
    }

    #[test]
    fn blend_renormalises_energy() {
        // Halfway blend between two distinct backends still yields unit energy.
        let blended = hybrid(BlendCurve::new(vec![[0.0, 0.5], [1.0, 0.5]]))
            .compute_gains(&request([0.3, 0.1, 0.2]))
            .gains;
        let energy: f32 = blended.iter().map(|gain| gain * gain).sum();
        assert!((energy - 1.0).abs() < 1e-4, "energy={energy}");
    }
}
