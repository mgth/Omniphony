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

/// Save the live config to disk at the control's config path. The audio-free
/// core writes core fields (renderer/layout/speakers/loudness/DRC/monitoring);
/// the optional host handler (e.g. `host_audio::HostAudio`) appends its own
/// fields (output device, live input, adaptive resampling, latency target) via
/// [`HostControlHandler::amend_saved_config`] before the file is written.
pub fn save_live_config(
    control: &Arc<RendererControl>,
    host: Option<&dyn HostControlHandler>,
) -> Result<SaveLiveConfigResult> {
    let path = {
        let guard = control.config_path.lock();
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow!("no config path available"))?
    };

    save_live_config_to_path(control, host, &path, &path)?;
    control.mark_clean();
    // A deliberate save supersedes any pending live-handoff overlay.
    let _ = std::fs::remove_file(renderer::config::live_sidecar_path(&path));
    renderer::config::clear_live_overlay_cache();

    Ok(SaveLiveConfigResult {
        path,
        restart_required: false,
    })
}

/// Serialize the current live state into a complete config file at `out_path`,
/// amending a base config loaded from `base_path`. Does NOT mark the live
/// state clean and does NOT notify clients — used by [`save_live_config`]
/// (with `out_path == base_path`) and by the shutdown handoff, which writes
/// the live-state sidecar next to the persistent config.
pub fn save_live_config_to_path(
    control: &Arc<RendererControl>,
    host: Option<&dyn HostControlHandler>,
    base_path: &std::path::Path,
    out_path: &std::path::Path,
) -> Result<()> {
    let live = control.live.read();
    let mut config = renderer::config::Config::load_or_default(base_path);
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
    // Generic per-backend param values, persisted verbatim (empty map is skipped).
    render.backend_params = control.all_backend_params();
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
    // Object-size interval count applies to both precomputed modes; persist it
    // only when enabled (0 is the default and stays out of the file).
    render.evaluation_object_size_intervals = (live.evaluation.object_size_intervals > 0)
        .then_some(live.evaluation.object_size_intervals);
    // VBAP spread tuning (min/max, from_distance, distance range/curve, size
    // policy) now lives in the generic param bag (`render.backend_params`,
    // written above via `all_backend_params`). Drop the legacy dedicated keys on
    // save; an old config carrying them is still migrated into the bag on load.
    render.vbap_spread_min = None;
    render.vbap_spread_max = None;
    render.spread_from_distance = None;
    render.spread_distance_range = None;
    render.spread_distance_curve = None;
    render.size_to_spread_mode = None;
    renderer::config_fields::use_loudness::store(render, live.use_loudness);
    // Declared live options (registry rows) + their param bags and the virtual
    // bed: one call covers what the OSC targeted persists cover, so the full
    // save and the per-option writes cannot drift.
    renderer::options::store_live_to_config(render, &live);
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
    let default_mirror_axes = renderer::spatial_vbap::MirrorAxes::default();
    render.distance_diffuse_mirror_axes =
        if live.distance_diffuse_mirror_axes != default_mirror_axes {
            Some(live.distance_diffuse_mirror_axes.to_string())
        } else {
            None
        };
    // Binaural (headphone) stage: persist the live selection so it survives a
    // restart — output mode, HRIR source (+ SOFA path), isotropic scale, and the
    // head-tracking OSC address/format.
    let (hrir_source, hrtf_sofa_path) = match &live.binaural.hrir_source {
        renderer::binaural::HrirSource::Sofa(p) if !p.is_empty() => {
            ("sofa".to_string(), Some(std::path::PathBuf::from(p)))
        }
        renderer::binaural::HrirSource::Pinna {
            preset,
            d_scale_pct,
            depth_pct,
        } => (
            format!("pinna:{}:{d_scale_pct}:{depth_pct}", preset.as_str()),
            None,
        ),
        renderer::binaural::HrirSource::Prtf {
            freq_scale_pct,
            depth_pct,
        } => (format!("prtf:{freq_scale_pct}:{depth_pct}"), None),
        other => (other.as_str().to_string(), None),
    };
    render.binaural = Some(renderer::config::BinauralConfig {
        output_mode: Some(live.binaural.output_mode.as_str().to_string()),
        mode: Some(live.binaural.mode.as_str().to_string()),
        ear_gains: Some([live.binaural.ears[0].gain, live.binaural.ears[1].gain]),
        ear_mutes: Some([live.binaural.ears[0].muted, live.binaural.ears[1].muted]),
        unit_scale_m: Some(live.binaural.unit_scale_m),
        head_radius_m: Some(live.binaural.head_radius_m),
        hrir_source: Some(hrir_source),
        hrtf_sofa_path,
        head_tracking: Some(renderer::config::HeadTrackingConfig {
            osc_address: live.binaural.tracking.address.clone(),
            format: Some(live.binaural.tracking.format.as_str().to_string()),
            reference_quat: {
                // Carry the recenter reference through an explicit Save too (the
                // targeted write-back already persists it on recenter); omit when
                // back at identity to keep the YAML clean.
                let r = live.binaural.tracking.reference;
                (r != renderer::binaural::HeadPose::identity()).then(|| r.to_quat_array())
            },
            extra: Default::default(),
        }),
        reflections: Some(renderer::config::ReflectionsConfig {
            enabled: Some(live.binaural.reflections.enabled),
            room_width_m: Some(live.binaural.reflections.room_size_m[0]),
            room_depth_m: Some(live.binaural.reflections.room_size_m[1]),
            room_height_m: Some(live.binaural.reflections.room_size_m[2]),
            level: Some(live.binaural.reflections.level),
            extra: Default::default(),
        }),
        reverb: Some(renderer::config::ReverbConfig {
            enabled: Some(live.binaural.reverb.enabled),
            level: Some(live.binaural.reverb.level),
            rt60_s: Some(live.binaural.reverb.rt60_s),
            predelay_ms: Some(live.binaural.reverb.predelay_ms),
            extra: Default::default(),
        }),
        air_absorption: Some(live.binaural.air_absorption),
        extra: Default::default(),
    });
    // barycenter / experimental_distance params now live in the generic param bag
    // (`render.backend_params`, written below), so drop the legacy dedicated keys
    // on save. Reading an old config still migrates them into the bag on load.
    render.experimental_distance_distance_floor = None;
    render.experimental_distance_min_active_speakers = None;
    render.experimental_distance_max_active_speakers = None;
    render.experimental_distance_position_error_floor = None;
    render.experimental_distance_position_error_nearest_scale = None;
    render.experimental_distance_position_error_span_scale = None;
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
    render.barycenter_localize = None;
    renderer::config_fields::ramp_mode::store(render, control.requested_ramp_mode().as_str());

    drop(live);

    // Audio output, live input, adaptive resampling, latency target — written
    // by the host's `host_audio::HostAudio` (via the trait). The audio-free
    // core never references those fields directly.
    if let Some(h) = host {
        h.amend_saved_config(render);
    }

    config.save(out_path)?;

    Ok(())
}
