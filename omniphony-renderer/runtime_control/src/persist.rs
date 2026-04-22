use std::sync::Arc;

use anyhow::{Result, anyhow};
use audio_input::{
    InputBackend, InputClockMode, InputControl, InputLfeMode, InputMapMode, InputMode,
    InputSampleFormat,
};
use audio_output::AudioControl;
use renderer::live_params::{
    LiveEvaluationMode, PreferredEvaluationMode, RampMode, RendererControl,
};

use crate::snapshot::build_live_state_bundle;

pub struct SaveLiveConfigResult {
    pub path: std::path::PathBuf,
    pub state_bundle: Vec<u8>,
    pub restart_required: bool,
}

#[inline]
fn round6(v: f32) -> f32 {
    (v * 1_000_000.0).round() / 1_000_000.0
}

pub fn save_live_config(
    control: &Arc<RendererControl>,
    audio_control: Option<&Arc<AudioControl>>,
    input_control: Option<&Arc<InputControl>>,
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
    render.master_gain = if master_gain_db.abs() > 0.01 {
        Some(master_gain_db)
    } else {
        None
    };

    render.vbap_spread_min = if live.spread_min != 0.0 {
        Some(live.spread_min)
    } else {
        None
    };
    render.vbap_spread_max = if live.spread_max != 1.0 {
        Some(live.spread_max)
    } else {
        None
    };
    render.vbap_azimuth_resolution = if live.evaluation.polar.azimuth_values != 360 {
        Some(live.evaluation.polar.azimuth_values.max(1))
    } else {
        None
    };
    render.vbap_elevation_resolution = if live.evaluation.polar.elevation_values != 180 {
        Some(live.evaluation.polar.elevation_values.max(1))
    } else {
        None
    };
    render.vbap_distance_res = if live.evaluation.polar.distance_res != 8 {
        Some(live.evaluation.polar.distance_res.max(1))
    } else {
        None
    };
    render.vbap_distance_max = if (live.evaluation.polar.distance_max - 2.0).abs() > 1e-4 {
        Some(live.evaluation.polar.distance_max.max(0.01))
    } else {
        None
    };
    render.render_evaluation_position_interpolation = Some(live.evaluation.position_interpolation);
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
    render.spread_from_distance = if live.spread_from_distance {
        Some(true)
    } else {
        None
    };
    render.spread_distance_range = if (live.spread_distance_range - 1.0).abs() > 1e-4 {
        Some(live.spread_distance_range)
    } else {
        None
    };
    render.spread_distance_curve = if (live.spread_distance_curve - 1.0).abs() > 1e-4 {
        Some(live.spread_distance_curve)
    } else {
        None
    };
    render.use_loudness = if live.use_loudness { Some(true) } else { None };
    render.vbap_distance_model =
        if live.distance_model != renderer::spatial_vbap::DistanceModel::None {
            Some(live.distance_model.to_string())
        } else {
            None
        };
    let [w, l, h] = live.room_ratio;
    let w = round6(w);
    let l = round6(l);
    let h = round6(h);
    let r = round6(live.room_ratio_rear);
    let lower = round6(live.room_ratio_lower);
    let cb = round6(live.room_ratio_center_blend);
    render.room_ratio = Some(format!("{w:.6},{l:.6},{h:.6}"));
    render.room_ratio_rear = Some(r);
    render.room_ratio_lower = Some(lower);
    render.room_ratio_center_blend = Some(cb);
    render.distance_diffuse = if live.use_distance_diffuse {
        Some(true)
    } else {
        None
    };
    render.distance_diffuse_threshold = if (live.distance_diffuse_threshold - 1.0).abs() > 1e-4 {
        Some(live.distance_diffuse_threshold)
    } else {
        None
    };
    render.distance_diffuse_curve = if (live.distance_diffuse_curve - 1.0).abs() > 1e-4 {
        Some(live.distance_diffuse_curve)
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
    render.ramp_mode = match control.requested_ramp_mode() {
        RampMode::Frame => None,
        mode => Some(mode.as_str().to_string()),
    };

    if let Some(audio_control) = audio_control {
        let requested = audio_control.requested_snapshot();
        render.output_device = requested.output_device;
        render.output_sample_rate = requested.output_sample_rate_hz;
        render.enable_adaptive_resampling = Some(requested.adaptive_enabled);
        render.adaptive_resampling_enable_far_mode = Some(requested.adaptive.enable_far_mode);
        render.adaptive_resampling_force_silence_in_far_mode =
            Some(requested.adaptive.force_silence_in_far_mode);
        render.adaptive_resampling_hard_recover_high_in_far_mode =
            Some(requested.adaptive.hard_recover_high_in_far_mode);
        render.adaptive_resampling_hard_recover_low_in_far_mode =
            Some(requested.adaptive.hard_recover_low_in_far_mode);
        render.adaptive_resampling_far_mode_return_fade_in_ms =
            Some(requested.adaptive.far_mode_return_fade_in_ms);
        render.latency_target = requested.latency_target_ms;
        render.adaptive_resampling_kp_near = Some(requested.adaptive.kp_near as f32);
        render.adaptive_resampling_ki = Some(requested.adaptive.ki as f32);
        render.adaptive_resampling_integral_discharge_ratio =
            Some(requested.adaptive.integral_discharge_ratio as f32);
        render.adaptive_resampling_max_adjust = Some(requested.adaptive.max_adjust as f32);
        render.adaptive_resampling_update_interval_callbacks =
            Some(requested.adaptive.update_interval_callbacks);
        render.adaptive_resampling_near_far_threshold_ms =
            Some(requested.adaptive.near_far_threshold_ms);
    }

    if let Some(input_control) = input_control {
        let requested = input_control.requested_snapshot();
        render.input_mode = Some(match requested.mode {
            InputMode::Bridge => renderer::config::InputModeConfig::Bridge,
            InputMode::Live => renderer::config::InputModeConfig::Live,
            InputMode::PipewireBridge => renderer::config::InputModeConfig::PipewireBridge,
        });
        render.live_input = Some(renderer::config::LiveInputConfig {
            backend: requested.backend.map(|backend| match backend {
                InputBackend::Pipewire => renderer::config::InputBackendConfig::Pipewire,
                InputBackend::Asio => renderer::config::InputBackendConfig::Asio,
            }),
            node: requested.node_name,
            description: requested.node_description,
            layout: requested.layout_path,
            current_layout: requested.current_layout,
            clock_mode: Some(match requested.clock_mode {
                InputClockMode::Dac => renderer::config::InputClockModeConfig::Dac,
                InputClockMode::Pipewire => renderer::config::InputClockModeConfig::Pipewire,
                InputClockMode::Upstream => renderer::config::InputClockModeConfig::Upstream,
            }),
            channels: requested.channels,
            sample_rate: requested.sample_rate_hz,
            sample_format: requested.sample_format.map(|format| match format {
                InputSampleFormat::F32 => "f32".to_string(),
                InputSampleFormat::S16 => "s16".to_string(),
            }),
            map: Some(match requested.map_mode {
                InputMapMode::SevenOneFixed => renderer::config::InputMapModeConfig::SevenOneFixed,
            }),
            lfe_mode: Some(match requested.lfe_mode {
                InputLfeMode::Object => renderer::config::InputLfeModeConfig::Object,
                InputLfeMode::Direct => renderer::config::InputLfeModeConfig::Direct,
                InputLfeMode::Drop => renderer::config::InputLfeModeConfig::Drop,
            }),
        });
    }

    drop(live);
    config.save(&path)?;
    control.mark_clean();

    Ok(SaveLiveConfigResult {
        path,
        state_bundle: build_live_state_bundle(control, audio_control, input_control),
        restart_required: false,
    })
}
