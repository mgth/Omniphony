//! Fixtures shared by this crate's unit tests.

use std::sync::Arc;

use renderer::live_params::{LiveEvaluationMode, PreferredEvaluationMode, RendererControl};
use renderer::spatial_renderer::{RendererSpec, SpatialRenderer};
use renderer::spatial_vbap::{DistanceModel, VbapTableMode};
use renderer::speaker_layout::SpeakerLayout;

/// A real `RendererControl` on a 7.1.4 layout, with a small cartesian grid so
/// the table build stays trivial. Same fixture as the live-options
/// conformance net.
pub(crate) fn fixture_control() -> Arc<RendererControl> {
    let layout = SpeakerLayout::preset("7.1.4").expect("7.1.4 preset");
    let renderer = SpatialRenderer::new(RendererSpec {
        speaker_layout: layout,
        sample_rate: 48_000,
        az_res_deg: 1,
        el_res_deg: 1,
        spread_resolution: 0.0,
        distance_max: 2.0,
        table_mode: VbapTableMode::Cartesian {
            x_size: 5,
            y_size: 5,
            z_size: 3,
            z_neg_size: 3,
        },
        allow_negative_z: false,
        vbap_position_interpolation: true,
        distance_model: DistanceModel::Linear,
        spread_from_distance: false,
        spread_distance_range: 1.0,
        spread_distance_curve: 1.0,
        spread_min: 0.0,
        spread_max: 1.0,
        log_object_positions: false,
        room_ratio: [1.0, 1.0, 1.0],
        room_ratio_rear: 1.0,
        room_ratio_lower: 1.0,
        room_ratio_center_blend: 0.0,
        master_gain_db: 0.0,
        auto_gain: false,
        use_loudness: false,
        distance_diffuse: false,
        distance_diffuse_threshold: 1.0,
        distance_diffuse_curve: 1.0,
        preferred_evaluation_mode: PreferredEvaluationMode::PrecomputedCartesian,
        initial_evaluation_mode: LiveEvaluationMode::PrecomputedCartesian,
        cartesian_default_x_size: 5,
        cartesian_default_y_size: 5,
        cartesian_default_z_size: 3,
        cartesian_default_z_neg_size: 3,
    })
    .expect("fixture renderer");
    renderer.renderer_control()
}
