use std::path::Path;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use renderer::config::RenderConfig;
use renderer::live_params::{
    LiveEvaluationMode, LiveParams, PreferredEvaluationMode, RendererControl,
};

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

    let mut config = renderer::config::Config::load_or_default(&path);
    store_live_into_config(control, host, &mut config);
    // A deliberate save supersedes any pending live-handoff overlay.
    commit_config(&path, &config)?;
    control.mark_clean();

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
    let mut config = renderer::config::Config::load_or_default(base_path);
    store_live_into_config(control, host, &mut config);
    config.save(out_path)?;

    Ok(())
}

/// Serialize the current live state into `config.render` (creating the render
/// section if needed) without touching disk. The write half of
/// [`save_live_config_to_path`], shared with the OSC profile operations, which
/// commit the live state into the outgoing profile before switching
/// (docs/config-profiles.md).
pub fn store_live_into_config(
    control: &Arc<RendererControl>,
    host: Option<&dyn HostControlHandler>,
    config: &mut renderer::config::Config,
) {
    let live = control.live.read();
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
            spk.gain_db = renderer::live_params::speaker_gain_db(lp.gain);
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
    // Declared live options (registry rows: auto-gain, loudness, ramp mode,
    // DRC, the fixed-channel family, …) + their param bags and the virtual
    // bed: one call covers what the OSC targeted persists cover, so the full
    // save and the per-option writes cannot drift.
    renderer::options::store_live_to_config(render, &live);
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
    // restart — output mode, HRIR source (+ SOFA path), isotropic scale, the
    // head-tracking input and the room (reflections, reverb).
    let (hrir_source, hrtf_sofa_path, brir_sofa_path) = match &live.binaural.hrir_source {
        renderer::binaural::HrirSource::Sofa(p) if !p.is_empty() => {
            ("sofa".to_string(), Some(std::path::PathBuf::from(p)), None)
        }
        renderer::binaural::HrirSource::Brir(p) if !p.is_empty() => {
            ("brir".to_string(), None, Some(std::path::PathBuf::from(p)))
        }
        renderer::binaural::HrirSource::Pinna {
            preset,
            d_scale_pct,
            depth_pct,
        } => (
            format!("pinna:{}:{d_scale_pct}:{depth_pct}", preset.as_str()),
            None,
            None,
        ),
        renderer::binaural::HrirSource::Prtf {
            freq_scale_pct,
            depth_pct,
        } => (format!("prtf:{freq_scale_pct}:{depth_pct}"), None, None),
        other => (other.as_str().to_string(), None, None),
    };
    // Updated field by field in the loaded section, never rebuilt: keys this
    // version does not know (`extra`) and anything `store_live_to_config`
    // wrote under `binaural` above (e.g. `hrir_update_lattice`) survive.
    let bin = render.binaural.get_or_insert_with(Default::default);
    bin.output_mode = Some(live.binaural.output_mode.as_str().to_string());
    bin.mode = Some(live.binaural.mode.as_str().to_string());
    bin.ear_gains = Some([live.binaural.ears[0].gain, live.binaural.ears[1].gain]);
    bin.ear_mutes = Some([live.binaural.ears[0].muted, live.binaural.ears[1].muted]);
    bin.unit_scale_m = Some(live.binaural.unit_scale_m);
    bin.head_radius_m = Some(live.binaural.head_radius_m);
    bin.hrir_source = Some(hrir_source);
    bin.hrtf_sofa_path = hrtf_sofa_path;
    bin.brir_sofa_path = brir_sofa_path;
    bin.brir_head_tracking = live.binaural.brir.head_tracking;
    bin.brir_max_length_s = Some(live.binaural.brir.max_length_s);
    bin.brir_tail_floor_db = Some(live.binaural.brir.tail_floor_db);
    bin.air_absorption = Some(live.binaural.air_absorption);
    bin.diffuse_field_eq = Some(live.binaural.diffuse_field_eq);

    let tracking = &live.binaural.tracking;
    let ht = bin.head_tracking.get_or_insert_with(Default::default);
    ht.osc_address = tracking.address.clone();
    ht.format = Some(tracking.format.as_str().to_string());
    // Carry the recenter reference through an explicit Save too (the targeted
    // write-back already persists it on recenter); omit when back at identity
    // to keep the YAML clean.
    let identity = renderer::binaural::HeadPose::identity();
    ht.reference_quat =
        (tracking.reference != identity).then(|| tracking.reference.to_quat_array());
    ht.axes_quat = (tracking.axes != identity).then(|| tracking.axes.to_quat_array());
    ht.smoothing = Some(tracking.smoothing);
    ht.invert = Some(tracking.invert);

    let reflections = &live.binaural.reflections;
    let refl = bin.reflections.get_or_insert_with(Default::default);
    refl.enabled = Some(reflections.enabled);
    refl.room_width_m = Some(reflections.room_size_m[0]);
    refl.room_depth_m = Some(reflections.room_size_m[1]);
    refl.room_height_m = Some(reflections.room_size_m[2]);
    refl.level = Some(reflections.level);
    refl.wall_cutoff_hz = Some(reflections.wall_cutoff_hz);

    let reverb = &live.binaural.reverb;
    let rev = bin.reverb.get_or_insert_with(Default::default);
    rev.enabled = Some(reverb.enabled);
    rev.level = Some(reverb.level);
    rev.rt60_s = Some(reverb.rt60_s);
    rev.predelay_ms = Some(reverb.predelay_ms);
    rev.size = Some(reverb.size);
    rev.rt60_low_ratio = Some(reverb.rt60_low_ratio);
    rev.rt60_high_ratio = Some(reverb.rt60_high_ratio);
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

    drop(live);

    // Audio output, live input, adaptive resampling, latency target — written
    // by the host's `host_audio::HostAudio` (via the trait). The audio-free
    // core never references those fields directly.
    if let Some(h) = host {
        h.amend_saved_config(render);
    }
}

/// Write `config` to `path` as the new persistent config, then drop the
/// live-handoff sidecar and overlay cache next to it.
///
/// Every write of the *whole* live state to `config.yaml` goes through here —
/// the full save and a profile operation — because each one supersedes
/// whatever a previous instance left in the sidecar when it fell back and tore
/// down: a stale sidecar must not override the file on the next boot. A
/// targeted per-field persist does not: it writes one field and amends the
/// overlay instead ([`persist_render_fields_to_path`]). (The shutdown handoff,
/// which *writes* the sidecar, is the other writer that does not.)
pub fn commit_config(path: &Path, config: &renderer::config::Config) -> Result<()> {
    config.save(path)?;
    renderer::config::discard_live_sidecar(path);
    Ok(())
}

/// One targeted write-back: the config field(s) a live change must reach the
/// file right away, instead of waiting for an explicit Save.
///
/// `store` reads the live value and writes it into the render section; like
/// the registry's `OptionSpec::config_store`, a skip-if-default writer keeps a
/// default value out of the file entirely. Carried in
/// [`crate::osc::ControlEffects::persist`] by the handlers and performed by the
/// engine, which owns the I/O.
#[derive(Debug, Clone, Copy)]
pub struct PersistOp {
    /// What is written, for the log.
    pub what: &'static str,
    pub store: PersistStore,
}

/// Where a [`PersistOp`] reads the value it writes.
#[derive(Debug, Clone, Copy)]
pub enum PersistStore {
    /// A field of the live parameters.
    Live(fn(&mut RenderConfig, &LiveParams)),
    /// A value `RendererControl` holds outside them (the cadence atomics).
    Control(fn(&mut RenderConfig, &RendererControl)),
}

impl PersistOp {
    /// A declared live option (`renderer::options` registry row).
    pub fn option(spec: &'static renderer::options::OptionSpec) -> Self {
        Self {
            what: spec.key,
            store: PersistStore::Live(spec.config_store),
        }
    }

    /// The head-tracking recenter reference, so the chosen "forward" survives
    /// an engine rebuild (mpv track change) and a restart.
    pub const HEAD_CENTER: Self = Self {
        what: "head recenter",
        store: PersistStore::Live(|render, live| {
            let ht = head_tracking_config(render);
            ht.reference_quat = non_identity_quat(live.binaural.tracking.reference);
        }),
    };

    /// The sensor-to-head axis calibration, next to the recenter reference.
    pub const HEAD_AXES: Self = Self {
        what: "head axes",
        store: PersistStore::Live(|render, live| {
            let ht = head_tracking_config(render);
            ht.axes_quat = non_identity_quat(live.binaural.tracking.axes);
        }),
    };

    /// The meter publication cadence: view state, it never waits for a Save.
    pub const METER_RATE: Self = Self {
        what: "meter rate",
        store: PersistStore::Control(|render, control| {
            render.meter_rate = Some(round6(control.meter_rate_hz()));
        }),
    };

    /// The diagnostics publication cadence: view state, like the meter's.
    pub const DIAG_RATE: Self = Self {
        what: "diag rate",
        store: PersistStore::Control(|render, control| {
            render.diag_rate = Some(round6(control.diag_rate_hz()));
        }),
    };
}

fn head_tracking_config(render: &mut RenderConfig) -> &mut renderer::config::HeadTrackingConfig {
    render
        .binaural
        .get_or_insert_with(Default::default)
        .head_tracking
        .get_or_insert_with(Default::default)
}

/// `None` at identity, so an "uncentered" / uncalibrated tracker leaves a
/// clean config rather than persisting a no-op quaternion.
fn non_identity_quat(pose: renderer::binaural::HeadPose) -> Option<[f32; 4]> {
    (pose != renderer::binaural::HeadPose::identity()).then(|| pose.to_quat_array())
}

/// Perform targeted write-backs against the control's config file, if it has
/// one. Best-effort: a failure is logged, never raised — the live change has
/// already been applied, and the explicit Save still covers it.
pub fn persist_ops(control: &RendererControl, ops: &[PersistOp]) {
    if ops.is_empty() {
        return;
    }
    let Some(path) = control.config_path() else {
        return;
    };
    persist_render_fields_to_path(&path, |render| {
        let live = control.live.read();
        for op in ops {
            match op.store {
                PersistStore::Live(store) => store(render, &live),
                PersistStore::Control(store) => store(render, control),
            }
        }
    });
    let what: Vec<&str> = ops.iter().map(|op| op.what).collect();
    log::debug!("persisted {} to {}", what.join(", "), path.display());
}

/// Targeted config write: load the existing config, let `store` set *only*
/// its fields (every other key survives, unknown ones included via the
/// config's flattened `extra`) and save it. Best-effort; logs on error.
///
/// The same fields are written into a pending live-handoff overlay, if there
/// is one, rather than discarding it: the overlay holds the *other* edits the
/// user has not saved yet, which a one-field write must neither commit nor
/// throw away, and amending it keeps its stale copy of this field from
/// reverting the write on the next boot.
pub fn persist_render_fields_to_path(path: &Path, store: impl Fn(&mut RenderConfig)) {
    let mut config = renderer::config::Config::load_or_default(path);
    store(config.render.get_or_insert_with(Default::default));
    if let Err(e) = config.save(path) {
        log::warn!("failed to persist a live change to {}: {e}", path.display());
    }
    renderer::config::amend_live_overlay(path, |overlay| {
        store(overlay.render.get_or_insert_with(Default::default));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use renderer::live_params::ChannelRenderMode;
    use std::path::PathBuf;

    /// A targeted persist amends a pending handoff sidecar in place: it now
    /// holds every `present` line and none of the `absent` keys.
    fn assert_sidecar(sidecar: &Path, present: &[&str], absent: &[&str]) {
        let text = std::fs::read_to_string(sidecar).expect("pending sidecar kept");
        for line in present {
            assert!(text.contains(line), "sidecar lacks {line:?}: {text}");
        }
        for key in absent {
            assert!(!text.contains(key), "sidecar still has {key:?}: {text}");
        }
    }

    /// The realtime speaker gain lights the Save button, so the Save writes
    /// it — as the layout's `gain_db` — and the next boot seeds it back.
    #[test]
    fn a_save_keeps_the_speaker_output_gains() {
        let control = crate::test_support::fixture_control();
        control.live.write().speakers.entry(2).or_default().gain = 0.5;
        control.live.write().speakers.entry(3).or_default().gain = 0.0;
        let mut config = renderer::config::Config::default();
        store_live_into_config(&control, None, &mut config);
        let layout = config
            .render
            .as_ref()
            .and_then(|r| r.current_layout.as_ref())
            .expect("layout stored");
        assert_eq!(layout.speakers[2].gain_db, -6.0);
        assert_eq!(
            layout.speakers[3].gain_db,
            renderer::live_params::SPEAKER_GAIN_FLOOR_DB
        );

        let seeded = renderer::live_params::speaker_live_from_layout(layout);
        assert!((seeded[&2].gain - 0.501).abs() < 1e-3);
        assert_eq!(seeded[&3].gain, 0.0);
        assert!(!seeded.contains_key(&0), "unity speakers need no entry");
    }

    fn temp_config_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "orender-crm-persist-{}-{}",
            std::process::id(),
            tag
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("config.yaml")
    }

    #[test]
    fn persist_channel_render_mode_writes_host_and_amends_sidecar() {
        let path = temp_config_path("host");
        // A config with an unknown render key and a known one, both must survive.
        std::fs::write(
            &path,
            "render:\n  bridge_path: /tmp/libbridge.so\n  some_future_key: 42\n",
        )
        .unwrap();
        // A pending handoff holding an unrelated unsaved edit: the persist
        // must add its field there and keep the edit.
        let sidecar = renderer::config::live_sidecar_path(&path);
        std::fs::write(&sidecar, "render:\n  surround_placement: back\n").unwrap();

        persist_render_fields_to_path(&path, |render| {
            renderer::config_fields::channel_render_mode::store(render, ChannelRenderMode::Host)
        });

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            written.contains("channel_render_mode: host"),
            "host not written: {written}"
        );
        assert!(
            written.contains("bridge_path: /tmp/libbridge.so"),
            "known key lost: {written}"
        );
        assert!(
            written.contains("some_future_key: 42"),
            "unknown key lost: {written}"
        );
        assert_sidecar(
            &sidecar,
            &["channel_render_mode: host", "surround_placement: back"],
            &[],
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn persist_channel_render_mode_spatial_omits_key_and_amends_sidecar() {
        let path = temp_config_path("spatial");
        std::fs::write(
            &path,
            "render:\n  bridge_path: /tmp/libbridge.so\n  channel_render_mode: host\n",
        )
        .unwrap();
        let sidecar = renderer::config::live_sidecar_path(&path);
        std::fs::write(
            &sidecar,
            "render:\n  channel_render_mode: host\n  surround_placement: back\n",
        )
        .unwrap();

        persist_render_fields_to_path(&path, |render| {
            renderer::config_fields::channel_render_mode::store(render, ChannelRenderMode::Spatial)
        });

        let written = std::fs::read_to_string(&path).unwrap();
        // Spatial is the default → skip-if-default omits the key entirely.
        assert!(
            !written.contains("channel_render_mode"),
            "default spatial should omit the key: {written}"
        );
        assert!(
            written.contains("bridge_path: /tmp/libbridge.so"),
            "known key lost: {written}"
        );
        assert_sidecar(
            &sidecar,
            &["surround_placement: back"],
            &["channel_render_mode"],
        );

        // Reloading yields the default (Spatial).
        let cfg = renderer::config::Config::load_or_default(&path);
        let mode = cfg
            .render
            .as_ref()
            .and_then(renderer::config_fields::channel_render_mode::get)
            .unwrap_or(ChannelRenderMode::Spatial);
        assert_eq!(mode, ChannelRenderMode::Spatial);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn persist_surround_placement_writes_back_and_amends_sidecar() {
        use renderer::live_params::SurroundPlacement;
        let path = temp_config_path("surround-back");
        std::fs::write(
            &path,
            "render:\n  bridge_path: /tmp/libbridge.so\n  some_future_key: 42\n",
        )
        .unwrap();
        let sidecar = renderer::config::live_sidecar_path(&path);
        std::fs::write(&sidecar, "render:\n  channel_render_mode: host\n").unwrap();

        persist_render_fields_to_path(&path, |render| {
            renderer::config_fields::surround_placement::store(render, SurroundPlacement::Back)
        });

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            written.contains("surround_placement: back"),
            "back not written: {written}"
        );
        assert!(
            written.contains("bridge_path: /tmp/libbridge.so"),
            "known key lost: {written}"
        );
        assert!(
            written.contains("some_future_key: 42"),
            "unknown key lost: {written}"
        );
        assert_sidecar(
            &sidecar,
            &["surround_placement: back", "channel_render_mode: host"],
            &[],
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn persist_head_center_writes_reference_and_amends_sidecar() {
        let path = temp_config_path("head-center");
        std::fs::write(
            &path,
            "render:\n  bridge_path: /tmp/libbridge.so\n  binaural:\n    head_tracking:\n      osc_address: /android/rotationvector\n",
        )
        .unwrap();
        let sidecar = renderer::config::live_sidecar_path(&path);
        std::fs::write(&sidecar, "render:\n  surround_placement: back\n").unwrap();

        let control = crate::test_support::fixture_control();
        let reference = [0.5, 0.5, 0.5, 0.5];
        control.live.write().binaural.tracking.reference =
            renderer::binaural::HeadPose::from_quat_array(reference);
        control.set_config_path(path.clone());
        persist_ops(&control, &[PersistOp::HEAD_CENTER]);

        // Written under binaural.head_tracking, the existing osc_address kept,
        // bridge_path preserved, and the pending sidecar amended.
        let cfg = renderer::config::Config::load_or_default(&path);
        let ht = cfg
            .render
            .as_ref()
            .and_then(|r| r.binaural.as_ref())
            .and_then(|b| b.head_tracking.as_ref())
            .expect("head_tracking present");
        assert_eq!(ht.reference_quat, Some(reference));
        assert_eq!(ht.osc_address.as_deref(), Some("/android/rotationvector"));
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("bridge_path: /tmp/libbridge.so"),
            "known key lost"
        );
        assert_sidecar(
            &sidecar,
            &["reference_quat", "surround_placement: back"],
            &[],
        );

        // Recentering back to identity drops the key entirely.
        control.live.write().binaural.tracking.reference = renderer::binaural::HeadPose::identity();
        persist_ops(&control, &[PersistOp::HEAD_CENTER]);
        let cfg = renderer::config::Config::load_or_default(&path);
        let ht = cfg
            .render
            .as_ref()
            .and_then(|r| r.binaural.as_ref())
            .and_then(|b| b.head_tracking.as_ref())
            .expect("head_tracking present");
        assert_eq!(ht.reference_quat, None, "identity should omit the key");

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn persist_surround_placement_side_omits_key_and_amends_sidecar() {
        use renderer::live_params::SurroundPlacement;
        let path = temp_config_path("surround-side");
        std::fs::write(
            &path,
            "render:\n  bridge_path: /tmp/libbridge.so\n  surround_placement: back\n",
        )
        .unwrap();
        let sidecar = renderer::config::live_sidecar_path(&path);
        std::fs::write(
            &sidecar,
            "render:\n  surround_placement: back\n  channel_render_mode: host\n",
        )
        .unwrap();

        persist_render_fields_to_path(&path, |render| {
            renderer::config_fields::surround_placement::store(render, SurroundPlacement::Side)
        });

        let written = std::fs::read_to_string(&path).unwrap();
        // Side is the default → skip-if-default omits the key entirely.
        assert!(
            !written.contains("surround_placement"),
            "default side should omit the key: {written}"
        );
        assert!(
            written.contains("bridge_path: /tmp/libbridge.so"),
            "known key lost: {written}"
        );
        assert_sidecar(
            &sidecar,
            &["channel_render_mode: host"],
            &["surround_placement"],
        );

        let cfg = renderer::config::Config::load_or_default(&path);
        let placement = cfg
            .render
            .as_ref()
            .and_then(renderer::config_fields::surround_placement::get)
            .unwrap_or(SurroundPlacement::Side);
        assert_eq!(placement, SurroundPlacement::Side);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
