//! Fixtures shared by this crate's unit tests.

use std::sync::Arc;

use renderer::live_params::{LiveEvaluationMode, PreferredEvaluationMode, RendererControl};
use renderer::spatial_renderer::SpatialRenderer;
use renderer::spatial_vbap::{DistanceModel, VbapTableMode};
use renderer::speaker_layout::SpeakerLayout;

/// A real `RendererControl` on a 7.1.4 layout, with a small cartesian grid so
/// the table build stays trivial. Same fixture as the live-options
/// conformance net.
pub(crate) fn fixture_control() -> Arc<RendererControl> {
    let layout = SpeakerLayout::preset("7.1.4").expect("7.1.4 preset");
    let renderer = SpatialRenderer::new(
        layout,
        48_000,
        1,
        1,
        0.0,
        2.0,
        VbapTableMode::Cartesian {
            x_size: 5,
            y_size: 5,
            z_size: 3,
            z_neg_size: 3,
        },
        false,
        true,
        DistanceModel::Linear,
        false,
        1.0,
        1.0,
        0.0,
        1.0,
        false,
        [1.0, 1.0, 1.0],
        1.0,
        1.0,
        0.0,
        0.0,
        false,
        false,
        false,
        1.0,
        1.0,
        PreferredEvaluationMode::PrecomputedCartesian,
        LiveEvaluationMode::PrecomputedCartesian,
        5,
        5,
        3,
        3,
    )
    .expect("fixture renderer");
    renderer.renderer_control()
}
