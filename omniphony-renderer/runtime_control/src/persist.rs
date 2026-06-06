use std::sync::Arc;

use anyhow::{Result, anyhow};
use renderer::live_params::{LiveEvaluationMode, PreferredEvaluationMode, RendererControl};

use crate::HostControlHandler;

pub struct SaveLiveConfigResult {
    pub path: std::path::PathBuf,
    pub restart_required: bool,
}

#[inline]
fn round6(v: f32) -> f32 {
    (v * 1_000_000.0).round() / 1_000_000.0
}

/// Save the live config to disk. The audio-free core writes core fields
/// (renderer/layout/speakers/loudness/DRC/monitoring); the optional host
/// handler (e.g. `host_audio::HostAudio`) appends its own fields
/// (output device, live input, adaptive resampling, latency target) via
/// [`HostControlHandler::amend_saved_config`] before the file is written.
pub fn save_live_config(
    control: &Arc<RendererControl>,
    host: Option<&dyn HostControlHandler>,
) -> Result<SaveLiveConfigResult> {
    let path = {
        let guard = control.config_path.lock().unwrap();
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow!("no config path available"))?
    };

    let live = control.live.read().unwrap();
    let mut config = renderer::config::Config::load_or_default(&path);
    let render = config.render.get_or_insert_with(Default::default);
    let requested_bridge_path = control.bridge_path();
    render.bridge_path = requested_bridge_path;
    render.input_pipe = control
        .input_path()
        .map(|value| std::path::PathBuf::from(value.trim()))
        .filter(|path| !path.as_os_str().is_empty());

    let mut layout_snapshot = control.editable_layout();
    for (idx, spk) in layout_snapshot.speakers.iter_mut().enumerate() {
        if let Some(lp) = live.speakers.get(&idx) {
            spk.delay_ms = lp.delay_ms.max(0.0);
        }
    }
    layout_snapshot.radius_m = round6(layout_snapshot.radius_m);
    render.current_layout = Some(layout_snapshot);
    render.speaker_layout = None;

    let master_gain_db = 20.0_f32 * live.master_gain.log10();
    renderer::config_fields::master_gain::store(render, master_gain_db);

    renderer::config_fields::vbap_spread_min::store(render, live.spread_min);
    renderer::config_fields::vbap_spread_max::store(render, live.spread_max);
    renderer::config_fields::vbap_azimuth_resolution::store(
        render,
        live.evaluation.polar.azimuth_values.max(1),
    );
    renderer::config_fields::vbap_elevation_resolution::store(
        render,
        live.evaluation.polar.elevation_values.max(1),
    );
    renderer::config_fields::vbap_distance_res::store(
        render,
        live.evaluation.polar.distance_res.max(1),
    );
    renderer::config_fields::vbap_distance_max::store(
        render,
        live.evaluation.polar.distance_max.max(0.01),
    );
    renderer::config_fields::render_evaluation_position_interpolation::store(
        render,
        live.evaluation.position_interpolation,
    );
    render.render_backend = match live.backend_id() {
        "vbap" => None,
        other => Some(other.to_string()),
    };
    render.render_evaluation_mode = match live.requested_evaluation_mode() {
        LiveEvaluationMode::Auto => None,
        other => Some(other.as_str().to_string()),
    };
    let effective_cartesian = match live.requested_evaluation_mode() {
        LiveEvaluationMode::PrecomputedCartesian => true,
        LiveEvaluationMode::PrecomputedPolar => false,
        LiveEvaluationMode::Realtime => false,
        LiveEvaluationMode::Auto => matches!(
            control
                .backend_rebuild_params()
                .map(|p| p.preferred_evaluation_mode),
            Some(PreferredEvaluationMode::PrecomputedCartesian)
        ),
    };
    if effective_cartesian {
        render.evaluation_cartesian_x_size = Some(live.evaluation.cartesian.x_size.max(1));
        render.evaluation_cartesian_y_size = Some(live.evaluation.cartesian.y_size.max(1));
        render.evaluation_cartesian_z_size = Some(live.evaluation.cartesian.z_size.max(1));
        render.evaluation_cartesian_z_neg_size = Some(live.evaluation.cartesian.z_neg_size);
    } else {
        render.evaluation_cartesian_x_size = None;
        render.evaluation_cartesian_y_size = None;
        render.evaluation_cartesian_z_size = None;
        render.evaluation_cartesian_z_neg_size = None;
    }
    renderer::config_fields::spread_from_distance::store(render, live.spread_from_distance);
    renderer::config_fields::spread_distance_range::store(render, live.spread_distance_range);
    renderer::config_fields::spread_distance_curve::store(render, live.spread_distance_curve);
    render.size_to_spread_mode =
        if live.size_to_spread_mode != renderer::render_backend::SizeToSpreadMode::default() {
            Some(live.size_to_spread_mode)
        } else {
            None
        };
    renderer::config_fields::use_loudness::store(render, live.use_loudness);
    renderer::config_fields::auto_gain::store(render, live.auto_gain);
    renderer::config_fields::auto_gain_ceiling_db::store(render, live.auto_gain_ceiling_db);
    renderer::config_fields::vbap_distance_model::store(render, live.distance_model.to_string());
    // Room geometry is persisted in metres. Width is the reference and the room
    // scale is Width/2 = the layout radius, so metres = ratio × radius × factor
    // (factor 2 for width). The legacy `room_ratio*` are dropped — `Config::load`
    // re-derives the runtime ratios from these metres.
    let [w, l, h] = live.room_ratio;
    let radius = render
        .current_layout
        .as_ref()
        .map(|layout| layout.radius_m)
        .unwrap_or(1.0);
    render.room_width_m = Some(round6(w * radius * 2.0));
    render.room_front_m = Some(round6(l * radius));
    render.room_rear_m = Some(round6(live.room_ratio_rear * radius));
    render.room_height_m = Some(round6(h * radius));
    render.room_lower_m = Some(round6(live.room_ratio_lower * radius));
    render.room_ratio_center_blend = Some(round6(live.room_ratio_center_blend));
    render.room_ratio = None;
    render.room_ratio_rear = None;
    render.room_ratio_lower = None;
    render.drc_weight = if (live.drc_weight - 1.0).abs() > 1e-4 {
        Some(round6(live.drc_weight))
    } else {
        None
    };
    render.drc_mode = if live.drc_mode != "Off" {
        Some(live.drc_mode.clone())
    } else {
        None
    };
    // Monitoring cadences: the renderer is the source of truth, so always
    // persist the current values (read lock-free from RendererControl).
    render.meter_rate = Some(round6(control.meter_rate_hz()));
    render.diag_rate = Some(round6(control.diag_rate_hz()));
    renderer::config_fields::distance_diffuse::store(render, live.use_distance_diffuse);
    renderer::config_fields::distance_diffuse_threshold::store(
        render,
        live.distance_diffuse_threshold,
    );
    renderer::config_fields::distance_diffuse_curve::store(render, live.distance_diffuse_curve);
    let default_metric = renderer::spatial_vbap::DistanceMetric::default();
    render.distance_model_metric = if live.distance_model_metric != default_metric {
        Some(live.distance_model_metric.to_string())
    } else {
        None
    };
    render.distance_diffuse_metric = if live.distance_diffuse_metric != default_metric {
        Some(live.distance_diffuse_metric.to_string())
    } else {
        None
    };
    let experimental_defaults = renderer::live_params::ExperimentalDistanceLiveParams::default();
    render.experimental_distance_distance_floor =
        if (live.experimental_distance.distance_floor - experimental_defaults.distance_floor).abs()
            > 1e-4
        {
            Some(live.experimental_distance.distance_floor)
        } else {
            None
        };
    render.experimental_distance_min_active_speakers =
        if live.experimental_distance.min_active_speakers
            != experimental_defaults.min_active_speakers
        {
            Some(live.experimental_distance.min_active_speakers)
        } else {
            None
        };
    render.experimental_distance_max_active_speakers =
        if live.experimental_distance.max_active_speakers
            != experimental_defaults.max_active_speakers
        {
            Some(live.experimental_distance.max_active_speakers)
        } else {
            None
        };
    render.experimental_distance_position_error_floor =
        if (live.experimental_distance.position_error_floor
            - experimental_defaults.position_error_floor)
            .abs()
            > 1e-4
        {
            Some(live.experimental_distance.position_error_floor)
        } else {
            None
        };
    render.experimental_distance_position_error_nearest_scale =
        if (live.experimental_distance.position_error_nearest_scale
            - experimental_defaults.position_error_nearest_scale)
            .abs()
            > 1e-4
        {
            Some(live.experimental_distance.position_error_nearest_scale)
        } else {
            None
        };
    render.experimental_distance_position_error_span_scale =
        if (live.experimental_distance.position_error_span_scale
            - experimental_defaults.position_error_span_scale)
            .abs()
            > 1e-4
        {
            Some(live.experimental_distance.position_error_span_scale)
        } else {
            None
        };
    let hybrid_defaults = renderer::live_params::HybridLiveParams::default();
    render.hybrid_external_backend =
        if live.hybrid.external_backend_id != hybrid_defaults.external_backend_id {
            Some(live.hybrid.external_backend_id.clone())
        } else {
            None
        };
    render.hybrid_internal_backend =
        if live.hybrid.internal_backend_id != hybrid_defaults.internal_backend_id {
            Some(live.hybrid.internal_backend_id.clone())
        } else {
            None
        };
    render.hybrid_curve = if live.hybrid.curve != hybrid_defaults.curve {
        Some(live.hybrid.curve.clone())
    } else {
        None
    };
    render.hybrid_curve_smoothing =
        if (live.hybrid.curve_smoothing - hybrid_defaults.curve_smoothing).abs() > 1e-4 {
            Some(live.hybrid.curve_smoothing)
        } else {
            None
        };
    render.hybrid_metric = if live.hybrid.metric != hybrid_defaults.metric {
        Some(live.hybrid.metric.to_string())
    } else {
        None
    };
    let barycenter_defaults = renderer::live_params::BarycenterLiveParams::default();
    render.barycenter_localize =
        if (live.barycenter.localize - barycenter_defaults.localize).abs() > 1e-4 {
            Some(live.barycenter.localize)
        } else {
            None
        };
    // Scriptable backend: persist the script path and its numeric params (the
    // loaded source is re-read from the path on load, never serialized).
    render.script_backend_path = if live.script.path.is_empty() {
        None
    } else {
        Some(live.script.path.clone())
    };
    render.script_backend_params = renderer::config::script_params_to_mapping(&live.script.params);
    renderer::config_fields::ramp_mode::store(render, control.requested_ramp_mode().as_str());

    drop(live);

    // Audio output, live input, adaptive resampling, latency target — written
    // by the host's `host_audio::HostAudio` (via the trait). The audio-free
    // core never references those fields directly.
    if let Some(h) = host {
        h.amend_saved_config(render);
    }

    config.save(&path)?;
    control.mark_clean();

    Ok(SaveLiveConfigResult {
        path,
        restart_required: false,
    })
}
