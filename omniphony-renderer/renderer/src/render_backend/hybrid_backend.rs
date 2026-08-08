use anyhow::Result;

use super::{BackendCapabilities, GainModel, RenderRequest, RenderResponse};
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

/// Blend curve mapping a normalised distance `x ∈ [0, 1]` to a blend ratio
/// `y ∈ [0, 1]`. Points are kept sorted by `x` and act as anchors; `smoothing`
/// (0 = piecewise-linear, 1 = full Catmull-Rom spline through the points) blends
/// the linear interpolation with a cubic spline. Evaluation clamps to the first
/// / last point outside the defined range.
#[derive(Debug, Clone)]
pub struct BlendCurve {
    points: Vec<[f32; 2]>,
    smoothing: f32,
}

impl BlendCurve {
    /// Build a curve from `(x, y)` control points and a `smoothing` amount in
    /// `[0, 1]`. Points are sorted by `x` and both axes are clamped to `[0, 1]`.
    /// An empty input falls back to the default linear ramp.
    pub fn new(mut points: Vec<[f32; 2]>, smoothing: f32) -> Self {
        for point in &mut points {
            point[0] = point[0].clamp(0.0, 1.0);
            point[1] = point[1].clamp(0.0, 1.0);
        }
        points.sort_by(|a, b| a[0].total_cmp(&b[0]));
        if points.is_empty() {
            return Self {
                smoothing: smoothing.clamp(0.0, 1.0),
                ..Self::default()
            };
        }
        Self {
            points,
            smoothing: smoothing.clamp(0.0, 1.0),
        }
    }

    /// Evaluate the curve at the given normalised distance.
    ///
    /// `smoothing` blends the piecewise-linear curve (passes through the control
    /// points) with an approximating quadratic B-spline (corner cutting): the
    /// control points become handles, the smooth curve is tangent to each
    /// segment at its midpoint. A segment adjacent to a point therefore yields a
    /// horizontal tangent there when that segment is horizontal.
    #[inline]
    pub fn eval(&self, x: f32) -> f32 {
        let points = &self.points;
        // `new()` guarantees a non-empty point set; endpoints are interpolated.
        if x <= points[0][0] {
            return points[0][1];
        }
        let last = points.len() - 1;
        if x >= points[last][0] {
            return points[last][1];
        }
        let linear = linear_y(points, x);
        if self.smoothing <= 0.0 {
            return linear;
        }
        let spline = bspline_y(points, x);
        (linear + (spline - linear) * self.smoothing).clamp(0.0, 1.0)
    }

    pub fn points(&self) -> &[[f32; 2]] {
        &self.points
    }
}

/// Piecewise-linear interpolation through the (sorted, in-range) control points.
fn linear_y(points: &[[f32; 2]], x: f32) -> f32 {
    let last = points.len() - 1;
    for i in 0..last {
        let [x0, y0] = points[i];
        let [x1, y1] = points[i + 1];
        if x <= x1 {
            let span = (x1 - x0).max(1e-6);
            let t = ((x - x0) / span).clamp(0.0, 1.0);
            return y0 + (y1 - y0) * t;
        }
    }
    points[last][1]
}

#[inline]
fn midpoint(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [0.5 * (a[0] + b[0]), 0.5 * (a[1] + b[1])]
}

/// Approximating uniform quadratic B-spline as a function `y(x)`: straight from
/// the first point to the first segment midpoint, a quadratic Bézier around each
/// interior point (control = the point, endpoints = the adjacent segment
/// midpoints), then straight from the last midpoint to the last point. The curve
/// passes through the endpoints and the segment midpoints, and is tangent to
/// every segment at its midpoint.
fn bspline_y(points: &[[f32; 2]], x: f32) -> f32 {
    let last = points.len() - 1;
    if last <= 1 {
        // Fewer than three points: no corner to cut, so it is just the line.
        return linear_y(points, x);
    }
    let m_first = midpoint(points[0], points[1]);
    if x <= m_first[0] {
        let span = (m_first[0] - points[0][0]).max(1e-6);
        let t = ((x - points[0][0]) / span).clamp(0.0, 1.0);
        return points[0][1] + (m_first[1] - points[0][1]) * t;
    }
    let m_last = midpoint(points[last - 1], points[last]);
    if x >= m_last[0] {
        let span = (points[last][0] - m_last[0]).max(1e-6);
        let t = ((x - m_last[0]) / span).clamp(0.0, 1.0);
        return m_last[1] + (points[last][1] - m_last[1]) * t;
    }
    for i in 1..last {
        let end = midpoint(points[i], points[i + 1]);
        if x <= end[0] {
            let start = midpoint(points[i - 1], points[i]);
            return quadratic_bezier_y_at_x(start, points[i], end, x);
        }
    }
    points[last][1]
}

/// Evaluate the quadratic Bézier (`start`, `control`, `end`) as `y` at the given
/// `x` by solving `Bx(u) = x` for `u ∈ [0, 1]`.
fn quadratic_bezier_y_at_x(start: [f32; 2], control: [f32; 2], end: [f32; 2], x: f32) -> f32 {
    let a = start[0] - 2.0 * control[0] + end[0];
    let b = 2.0 * (control[0] - start[0]);
    let c = start[0] - x;
    let u = if a.abs() < 1e-6 {
        if b.abs() < 1e-9 { 0.0 } else { -c / b }
    } else {
        let disc = (b * b - 4.0 * a * c).max(0.0).sqrt();
        let u1 = (-b + disc) / (2.0 * a);
        if (0.0..=1.0).contains(&u1) {
            u1
        } else {
            (-b - disc) / (2.0 * a)
        }
    }
    .clamp(0.0, 1.0);
    let omu = 1.0 - u;
    omu * omu * start[1] + 2.0 * omu * u * control[1] + u * u * end[1]
}

impl Default for BlendCurve {
    fn default() -> Self {
        Self {
            points: vec![[0.0, 0.0], [1.0, 1.0]],
            smoothing: 0.0,
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
            room_ratio: [1.0, 1.0, 1.0],
            room_ratio_rear: 1.0,
            room_ratio_lower: 1.0,
            room_ratio_center_blend: 0.0,
            use_distance_diffuse: false,
            diffuse_mirror_axes: crate::spatial_vbap::MirrorAxes::default(),
            distance_diffuse_threshold: 1.0,
            distance_diffuse_curve: 1.0,
            distance_model: crate::spatial_vbap::DistanceModel::None,
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
            Box::new(ExperimentalDistanceBackend::new(
                speakers(),
                crate::live_params::ExperimentalDistanceLiveParams::default(),
            )),
            Box::new(BarycenterBackend::new(speakers(), 0.0)),
            curve,
            crate::spatial_vbap::DistanceMetric::Chebyshev,
        )
    }

    #[test]
    fn curve_eval_clamps_and_interpolates() {
        let curve = BlendCurve::new(vec![[0.0, 0.2], [0.5, 0.8], [1.0, 0.4]], 0.0);
        assert!((curve.eval(-1.0) - 0.2).abs() < 1e-6);
        assert!((curve.eval(0.0) - 0.2).abs() < 1e-6);
        assert!((curve.eval(0.25) - 0.5).abs() < 1e-6);
        assert!((curve.eval(0.5) - 0.8).abs() < 1e-6);
        assert!((curve.eval(2.0) - 0.4).abs() < 1e-6);
    }

    #[test]
    fn curve_smoothing_cuts_corners_and_keeps_endpoints() {
        let points = vec![[0.0, 0.2], [0.5, 0.8], [1.0, 0.4]];
        let smooth = BlendCurve::new(points, 1.0);
        // Endpoints are still interpolated.
        assert!((smooth.eval(0.0) - 0.2).abs() < 1e-6);
        assert!((smooth.eval(1.0) - 0.4).abs() < 1e-6);
        // The interior point is now a handle: the corner-cutting curve does NOT
        // pass through it (it is pulled toward, but short of, 0.8).
        let mid = smooth.eval(0.5);
        assert!(mid < 0.8 - 1e-2 && mid > 0.5, "mid={mid}");
        // Output stays within [0, 1] across the whole range.
        for i in 0..=100 {
            let y = smooth.eval(i as f32 / 100.0);
            assert!((0.0..=1.0).contains(&y), "y={y} out of range");
        }
    }

    #[test]
    fn horizontal_segment_yields_horizontal_tangent_when_smoothed() {
        // Flat then ramp: at full smoothing the midpoint of the flat segment
        // stays flat (≈ its endpoints' height) rather than bending early.
        let smooth = BlendCurve::new(vec![[0.0, 0.0], [0.5, 0.0], [1.0, 1.0]], 1.0);
        // Quarter of the way along the horizontal segment the curve is still ~0.
        assert!(smooth.eval(0.125).abs() < 1e-3, "{}", smooth.eval(0.125));
    }

    #[test]
    fn constant_zero_curve_matches_internal() {
        let position = [0.3, 0.1, 0.2];
        let blended = hybrid(BlendCurve::new(vec![[0.0, 0.0], [1.0, 0.0]], 0.0))
            .compute_gains(&request(position))
            .gains;
        let internal = BarycenterBackend::new(speakers(), 0.0)
            .compute_gains(&request(position))
            .gains;
        for (a, b) in blended.iter().zip(internal.iter()) {
            assert!((a - b).abs() < 1e-5, "{a} vs {b}");
        }
    }

    #[test]
    fn constant_one_curve_matches_external() {
        let position = [0.3, 0.1, 0.2];
        let blended = hybrid(BlendCurve::new(vec![[0.0, 1.0], [1.0, 1.0]], 0.0))
            .compute_gains(&request(position))
            .gains;
        let external = ExperimentalDistanceBackend::new(
            speakers(),
            crate::live_params::ExperimentalDistanceLiveParams::default(),
        )
        .compute_gains(&request(position))
        .gains;
        for (a, b) in blended.iter().zip(external.iter()) {
            assert!((a - b).abs() < 1e-5, "{a} vs {b}");
        }
    }

    #[test]
    fn build_precomputed_cartesian_with_vbap_inner_completes() {
        use crate::render_backend::{
            EffectiveEvaluationMode, EvaluationBuildConfig, VbapBackend,
            build_prepared_render_engine,
        };
        use crate::spatial_vbap::VbapPanner;

        let positions = [[-30.0, 0.0], [30.0, 0.0], [-110.0, 0.0], [110.0, 0.0]];
        let panner = VbapPanner::new(&positions, 5, 5, 0.0, Default::default())
            .expect("vbap panner")
            .with_negative_z(true);
        let external: Box<dyn GainModel> = Box::new(VbapBackend::new(
            panner,
            crate::render_backend::VbapSpreadParams::default(),
        ));
        let internal: Box<dyn GainModel> = Box::new(BarycenterBackend::new(speakers(), 0.0));
        let model: Box<dyn GainModel> = Box::new(HybridBackend::new(
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
            object_size_intervals: 0,
            object_size_mode: crate::render_backend::SizeToSpreadMode::default(),
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
        let blended = hybrid(BlendCurve::new(vec![[0.0, 0.5], [1.0, 0.5]], 0.0))
            .compute_gains(&request([0.3, 0.1, 0.2]))
            .gains;
        let energy: f32 = blended.iter().map(|gain| gain * gain).sum();
        assert!((energy - 1.0).abs() < 1e-4, "energy={energy}");
    }
}
