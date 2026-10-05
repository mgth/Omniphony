use std::path::Path;
use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use renderer::config::{ConfigLoadStatus, RenderConfig};
use renderer::live_params::{LiveParams, RendererControl};

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

    refuse_live_state_from_defaults(control, &path)?;
    let mut config = renderer::config::Config::load_for_update(&path)?;
    store_live_into_config(control, host, &mut config);
    // A deliberate save supersedes any pending live-handoff overlay.
    commit_config(&path, &config)?;
    control.mark_clean();

    Ok(SaveLiveConfigResult {
        path,
        restart_required: false,
    })
}

/// Refuse to write the live state over `path` while it is the defaults the
/// engine fell back to because the file failed to parse when it was loaded
/// (`config_status = parse_error`). Parsing the file again at write time is
/// not enough: once the user has fixed it, it parses, and the Save would then
/// replace their layout with those defaults. Only re-reading the file into the
/// live state lifts this: a reload (a restart on the CLI) or a profile switch,
/// which both set the status again.
fn refuse_live_state_from_defaults(control: &RendererControl, path: &Path) -> Result<()> {
    if control.config_status().as_deref() == Some(ConfigLoadStatus::ParseError.as_str()) {
        bail!(
            "{} failed to parse when it was loaded, so the engine is running on built-in \
             defaults and a save would replace the file with them; fix or remove the file, \
             press Reload, then save",
            path.display()
        );
    }
    Ok(())
}

/// Serialize the current live state into a complete config file at `out_path`,
/// amending a base config loaded from `base_path`. Does NOT mark the live
/// state clean, does NOT notify clients and keeps no `.bak` — used by the
/// shutdown handoff, which writes the live-state sidecar next to the
/// persistent config. A base that fails to parse is read as defaults here:
/// the sidecar then carries what the engine is actually running on, and
/// `out_path` is never the base itself.
pub fn save_live_config_to_path(
    control: &Arc<RendererControl>,
    host: Option<&dyn HostControlHandler>,
    base_path: &std::path::Path,
    out_path: &std::path::Path,
) -> Result<()> {
    let mut config = renderer::config::Config::load_or_default(base_path);
    store_live_into_config(control, host, &mut config);
    config.save_without_backup(out_path)?;

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

    // Every plugin's param values — backends, object generators, the phantom
    // stage — persisted verbatim (an empty map is skipped); the legacy keys
    // they were migrated from are dropped.
    control.plugin_params().store_to_config(render);
    // VBAP spread tuning (min/max, from_distance, distance range/curve, size
    // policy) now lives in the generic param bag (`render.backend_params`,
    // written above). Drop the legacy dedicated keys on save; an old config
    // carrying them is still migrated into the bag on load.
    render.vbap_spread_min = None;
    render.vbap_spread_max = None;
    render.spread_from_distance = None;
    render.spread_distance_range = None;
    render.spread_distance_curve = None;
    render.size_to_spread_mode = None;
    // Declared live options (registry rows: auto-gain, loudness, ramp mode,
    // DRC, the fixed-channel family, the room, …) + their param bags and the
    // virtual bed: one call covers what the OSC targeted persists cover, so
    // the full save and the per-option writes cannot drift. After the layout:
    // the room is written in metres against its radius.
    renderer::options::store_live_to_config(
        render,
        &live,
        // A host with audio I/O leaves the embedded engine's options as
        // the file has them.
        &renderer::options::OptionEnv::of(control).with_host_io(host.is_some()),
    );
    // Monitoring cadences: the renderer is the source of truth, so always
    // persist the current values (read lock-free from RendererControl).
    render.meter_rate = Some(round6(control.meter_rate_hz()));
    render.diag_rate = Some(round6(control.diag_rate_hz()));
    // Binaural: the options are registry rows (stored above). Kept here: the
    // ear mutes, and the head-tracking recenter reference and axis
    // calibration, which the recenter writes at once (the persistence
    // policy's exception) and an explicit Save carries through; both are
    // omitted when back at identity to keep the YAML clean.
    let bin = render.binaural.get_or_insert_with(Default::default);
    bin.ear_mutes = Some([live.binaural.ears[0].muted, live.binaural.ears[1].muted]);
    let tracking = &live.binaural.tracking;
    let ht = bin.head_tracking.get_or_insert_with(Default::default);
    let identity = renderer::binaural::HeadPose::identity();
    ht.reference_quat =
        (tracking.reference != identity).then(|| tracking.reference.to_quat_array());
    ht.axes_quat = (tracking.axes != identity).then(|| tracking.axes.to_quat_array());

    // barycenter / experimental_distance params now live in the generic param bag
    // (`render.backend_params`, written below), so drop the legacy dedicated keys
    // on save. Reading an old config still migrates them into the bag on load.
    render.experimental_distance_distance_floor = None;
    render.experimental_distance_min_active_speakers = None;
    render.experimental_distance_max_active_speakers = None;
    render.experimental_distance_position_error_floor = None;
    render.experimental_distance_position_error_nearest_scale = None;
    render.experimental_distance_position_error_span_scale = None;
    // The hybrid curve is a point list, kept out of the registry; the legs,
    // smoothing and metric are registry rows (stored above).
    let default_curve = renderer::live_params::HybridLiveParams::default().curve;
    render.hybrid_curve = (live.hybrid.curve != default_curve).then(|| live.hybrid.curve.clone());
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
/// `store` reads the live value and writes it into the render section; a
/// skip-if-default writer keeps a default value out of the file entirely.
/// Only view state is written this way (docs/persistence-policy.md). Carried in
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
/// config's flattened `extra`) and save it. Best-effort; logs on error, and
/// leaves a file that fails to parse untouched.
///
/// The same fields are written into a pending live-handoff overlay, if there
/// is one, rather than discarding it: the overlay holds the *other* edits the
/// user has not saved yet, which a one-field write must neither commit nor
/// throw away, and amending it keeps its stale copy of this field from
/// reverting the write on the next boot.
pub fn persist_render_fields_to_path(path: &Path, store: impl Fn(&mut RenderConfig)) {
    let written = renderer::config::Config::load_for_update(path).and_then(|mut config| {
        store(config.render.get_or_insert_with(Default::default));
        config.save(path)
    });
    if let Err(e) = written {
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

    /// A config.yaml that fails to parse runs the engine on defaults; neither
    /// the Save nor a targeted write may then replace it with them.
    #[test]
    fn a_file_that_fails_to_parse_survives_save_and_targeted_writes() {
        let path = temp_config_path("parse-error");
        let corrupt = "render:\n  current_layout: [ unterminated\n";
        std::fs::write(&path, corrupt).unwrap();
        let control = crate::test_support::fixture_control();
        *control.config_path.lock() = Some(path.clone());

        let err = save_live_config(&control, None)
            .err()
            .expect("save refused");
        assert!(err.to_string().contains("left untouched"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), corrupt);

        persist_render_fields_to_path(&path, |render| render.output_file = Some("x".into()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), corrupt);
        assert!(!renderer::config::backup_path(&path).exists());
    }

    /// The engine came up on defaults because the file failed to parse; the
    /// user then fixes the file. A Save must still be refused, or it would
    /// write those defaults over the fixed file. Once the file has been read
    /// back into the live state (a reload sets the status again), it goes
    /// through.
    #[test]
    fn save_is_refused_while_the_live_state_is_the_parse_error_fallback() {
        let path = temp_config_path("parse-error-fixed");
        let fixed = "render:\n  output_file: kept\n";
        std::fs::write(&path, fixed).unwrap();
        let control = crate::test_support::fixture_control();
        *control.config_path.lock() = Some(path.clone());
        control.set_config_status(Some(ConfigLoadStatus::ParseError.as_str().into()));

        let err = save_live_config(&control, None)
            .err()
            .expect("save refused");
        assert!(err.to_string().contains("press Reload"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), fixed);

        control.set_config_status(Some(ConfigLoadStatus::Loaded.as_str().into()));
        save_live_config(&control, None).expect("save after reload");
        assert_ne!(std::fs::read_to_string(&path).unwrap(), fixed);
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
