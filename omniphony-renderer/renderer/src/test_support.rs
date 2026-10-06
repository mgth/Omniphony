//! Renderer fixtures for tests, in this crate and downstream (feature
//! `test-support`, enabled from `[dev-dependencies]` only).
//!
//! [`RendererSpec`] deliberately declares no default: a host takes every
//! value from its config. A test only cares about the few values it is about,
//! so it starts from [`spec`] and overrides those:
//!
//! ```ignore
//! let r = SpatialRenderer::new(RendererSpec {
//!     distance_model: DistanceModel::None,
//!     ..test_support::spec(layout)
//! })?;
//! ```

use std::sync::Arc;

use crate::live_params::{LiveEvaluationMode, PreferredEvaluationMode, RendererControl};
use crate::spatial_renderer::{RendererSpec, SpatialRenderer};
use crate::spatial_vbap::{DistanceModel, VbapTableMode};
use crate::speaker_layout::SpeakerLayout;

/// The spec most tests render with: 48 kHz, a 21 × 21 × 9 (+ 9 below)
/// cartesian table with position interpolation, linear distance model, the
/// default room ratios, no gain automation, no distance diffusion.
pub fn spec(speaker_layout: SpeakerLayout) -> RendererSpec {
    RendererSpec {
        speaker_layout,
        sample_rate: 48_000,
        az_res_deg: 1,
        el_res_deg: 1,
        spread_resolution: 0.0,
        distance_max: 2.0,
        table_mode: VbapTableMode::Cartesian {
            x_size: 21,
            y_size: 21,
            z_size: 9,
            z_neg_size: 9,
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
        room_ratio: [1.0, 2.0, 0.5],
        room_ratio_rear: 2.0,
        room_ratio_lower: 0.5,
        room_ratio_center_blend: 0.0,
        master_gain_db: 0.0,
        auto_gain: false,
        use_loudness: false,
        distance_diffuse: false,
        distance_diffuse_threshold: 1.0,
        distance_diffuse_curve: 1.0,
        preferred_evaluation_mode: PreferredEvaluationMode::PrecomputedCartesian,
        initial_evaluation_mode: LiveEvaluationMode::PrecomputedCartesian,
        cartesian_default_x_size: 21,
        cartesian_default_y_size: 21,
        cartesian_default_z_size: 9,
        cartesian_default_z_neg_size: 9,
    }
}

/// [`spec`] with a 5 × 5 × 3 (+ 3) table and a unit room, for tests about
/// control state rather than gains: the table build stays trivial.
pub fn small_grid_spec(speaker_layout: SpeakerLayout) -> RendererSpec {
    RendererSpec {
        table_mode: VbapTableMode::Cartesian {
            x_size: 5,
            y_size: 5,
            z_size: 3,
            z_neg_size: 3,
        },
        room_ratio: [1.0, 1.0, 1.0],
        room_ratio_rear: 1.0,
        room_ratio_lower: 1.0,
        cartesian_default_x_size: 5,
        cartesian_default_y_size: 5,
        cartesian_default_z_size: 3,
        cartesian_default_z_neg_size: 3,
        ..spec(speaker_layout)
    }
}

/// A real [`RendererControl`] on 7.1.4 built from [`small_grid_spec`]: the
/// only way live options exist at runtime.
pub fn fixture_control() -> Arc<RendererControl> {
    let layout = SpeakerLayout::preset("7.1.4").expect("7.1.4 preset");
    SpatialRenderer::new(small_grid_spec(layout))
        .expect("fixture renderer")
        .renderer_control()
}
