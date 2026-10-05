//! OSC handlers for the named config-profile operations
//! (`/omniphony/control/profile/*`, see docs/config-profiles.md).
//!
//! A profile is a whole-config transaction — file I/O, live re-seed, layout
//! staging and a forced topology rebuild — so it is a dedicated handler
//! rather than a `renderer::options` registry row; it still follows the
//! registry's conventions (contract constants, snapshot block, state
//! re-broadcast after every mutation).

use std::net::UdpSocket;
use std::sync::Arc;

use renderer::config::ConfigLoadStatus;
use renderer::live_params::RendererControl;
use rosc::{OscMessage, OscType};
use runtime_control::HostControlHandler;
use runtime_control::osc_contract;

use super::client_registry::OscClientRegistry;
use super::export::build_live_state;
use super::gaintable::GaintableCache;
use super::recompute::trigger_layout_recompute;
use super::transport::{broadcast_int, broadcast_string};

pub(crate) fn broadcast_profiles_state(
    control: &Arc<RendererControl>,
    socket: &Arc<UdpSocket>,
    clients: &Arc<OscClientRegistry>,
) {
    broadcast_string(
        socket,
        clients,
        osc_contract::STATE_PROFILES,
        &runtime_control::snapshot::profiles_state_json(control),
    );
}

/// Handle a `/omniphony/control/profile/*` message. Returns `true` when the
/// address was one of the profile operations (even if it failed — errors are
/// logged and the unchanged state is re-broadcast so optimistic clients
/// resync).
pub(crate) fn handle_profile_message(
    msg: &OscMessage,
    control: &Arc<RendererControl>,
    host: Option<&Arc<dyn HostControlHandler>>,
    socket: &Arc<UdpSocket>,
    clients: &Arc<OscClientRegistry>,
    gaintable_cache: &Arc<GaintableCache>,
) -> bool {
    let addr = msg.addr.as_str();
    let is_switch = addr == osc_contract::CONTROL_PROFILE_SWITCH;
    if !is_switch
        && addr != osc_contract::CONTROL_PROFILE_CREATE
        && addr != osc_contract::CONTROL_PROFILE_DELETE
        && addr != osc_contract::CONTROL_PROFILE_RENAME
    {
        return false;
    }

    let Some(OscType::String(name)) = msg.args.first() else {
        log::warn!("OSC {addr}: missing profile name");
        return true;
    };
    let name = name.trim().to_string();
    let Some(path) = control.config_path() else {
        log::warn!("OSC {addr}: no config path available");
        return true;
    };

    // A file that fails to parse is refused outright: every operation below
    // ends in a write, which would replace it with defaults.
    let refuse = |e: anyhow::Error| {
        let message = format!("profile operation '{name}' refused: {e}");
        log::warn!("OSC {addr}: {message}");
        broadcast_string(
            socket,
            clients,
            osc_contract::STATE_CONFIG_SAVE_ERROR,
            &message,
        );
        broadcast_profiles_state(control, socket, clients);
    };
    let mut config = match renderer::config::Config::load_for_update(&path) {
        Ok(config) => config,
        Err(e) => {
            refuse(e);
            return true;
        }
    };

    if is_switch {
        // Switching to the already-active profile must be a true no-op: the
        // full switch path would wipe runtime speaker gains/mutes and force a
        // gratuitous rebuild. Re-broadcast so an optimistic client resyncs.
        if name == config.active_profile_name() {
            broadcast_profiles_state(control, socket, clients);
            return true;
        }
        // Preflight the target's layout BEFORE committing anything: a profile
        // whose layout file went missing must fail the switch outright, not
        // half-apply its params on the previous layout while reporting
        // success.
        if let Err(e) = resolve_profile_layout(config.profiles.get(&name)) {
            let message = format!("profile switch '{name}' refused: {e}");
            log::warn!("OSC {addr}: {message}");
            broadcast_string(
                socket,
                clients,
                osc_contract::STATE_CONFIG_SAVE_ERROR,
                &message,
            );
            broadcast_profiles_state(control, socket, clients);
            return true;
        }
    }

    // A profile operation never commits the user's unsaved edits behind their
    // back (docs/persistence-policy.md):
    // - switch discards them — unless the client asked to save first
    //   (`[name, "save"]`): the full Save, host fields included, into the
    //   outgoing profile, and no switch at all when it fails;
    // - create makes the new profile from what the user hears ("from the
    //   current state") and leaves the active one as its file says;
    // - rename and delete are bookkeeping.
    let host_ref: Option<&dyn HostControlHandler> = host.map(|h| h.as_ref());
    if is_switch && matches!(msg.args.get(1), Some(OscType::String(mode)) if mode == "save") {
        broadcast_string(socket, clients, osc_contract::STATE_CONFIG_SAVE_ERROR, "");
        if let Err(e) = runtime_control::persist::save_live_config(control, host_ref) {
            let message = format!("profile switch '{name}' not done: save failed: {e}");
            log::error!("OSC {addr}: {message}");
            broadcast_string(
                socket,
                clients,
                osc_contract::STATE_CONFIG_SAVE_ERROR,
                &message,
            );
            broadcast_profiles_state(control, socket, clients);
            return true;
        }
        config = match renderer::config::Config::load_for_update(&path) {
            Ok(config) => config,
            Err(e) => {
                refuse(e);
                return true;
            }
        };
    }
    let created_from_live = (addr == osc_contract::CONTROL_PROFILE_CREATE).then(|| {
        let mut live = config.clone();
        // Without the host amend, as a switch-in re-seeds: host-owned fields
        // (output device, live input, resampling, latency) only take effect
        // at engine start, so the new profile keeps the file's.
        runtime_control::persist::store_live_into_config(control, None, &mut live);
        live.render.unwrap_or_default()
    });

    let rename_to = match msg.args.get(1) {
        Some(OscType::String(new)) => Some(new.clone()),
        _ => None,
    };
    let operation = |config: &mut renderer::config::Config| -> anyhow::Result<()> {
        if is_switch {
            config.switch_profile(&name)
        } else if let Some(render) = &created_from_live {
            config.create_profile(&name)?;
            config.profiles.insert(name.clone(), render.clone());
            Ok(())
        } else if addr == osc_contract::CONTROL_PROFILE_DELETE {
            config.delete_profile(&name)
        } else {
            match &rename_to {
                Some(new) => config.rename_profile(&name, new),
                None => anyhow::bail!("rename needs [old, new]"),
            }
        }
    };
    if let Err(e) = operation(&mut config) {
        log::warn!("OSC {addr} '{name}': {e}");
        broadcast_profiles_state(control, socket, clients);
        return true;
    }

    // A switch replaces the live state with the file's, so a pending handoff
    // overlay (the outgoing profile's unsaved edits) goes with it. The others
    // leave those edits pending: the overlay gets the same bookkeeping, or a
    // handoff would bring the old profile list back.
    let written = if is_switch {
        runtime_control::persist::commit_config(&path, &config)
    } else {
        config.save(&path).map(|()| {
            renderer::config::amend_live_overlay(&path, |overlay| {
                if let Err(e) = operation(overlay) {
                    log::warn!("OSC {addr} '{name}': not applied to the handoff overlay: {e}");
                }
            });
        })
    };
    if let Err(e) = written {
        let message = format!("profile operation failed to save config: {e}");
        log::error!("OSC {addr} '{name}': {message}");
        broadcast_string(
            socket,
            clients,
            osc_contract::STATE_CONFIG_SAVE_ERROR,
            &message,
        );
        return true;
    }

    control.set_profiles_info(config.profiles_info());

    if is_switch {
        apply_switched_profile(&config, control, socket, clients, gaintable_cache);
        // The live state is now the switched-in profile, as its file says.
        control.mark_clean();
        // That file parsed, so the live state is no longer the parse-error
        // fallback, and a Save may write it again.
        control.set_config_status(Some(ConfigLoadStatus::Loaded.as_str().into()));
        broadcast_string(socket, clients, osc_contract::STATE_CONFIG_SAVE_ERROR, "");
        broadcast_int(socket, clients, osc_contract::STATE_CONFIG_SAVED, 1);
    }
    broadcast_profiles_state(control, socket, clients);
    // Full state refresh so every client view (options, layout, binaural,
    // gains…) re-syncs to the post-operation state.
    build_live_state(control, host).broadcast(socket, clients);
    log::info!(
        "OSC {addr}: '{name}' done (active profile '{}')",
        config.active_profile_name()
    );
    true
}

/// Resolve the speaker layout a profile's render section describes: the
/// embedded layout wins, a `speaker_layout` path reference must load, and no
/// layout at all means "keep the current one" (`Ok(None)`). Errors instead of
/// falling back so the switch handler can refuse a profile whose layout file
/// is gone rather than half-applying its params on the previous layout.
fn resolve_profile_layout(
    render: Option<&renderer::config::RenderConfig>,
) -> anyhow::Result<Option<renderer::speaker_layout::SpeakerLayout>> {
    let Some(render) = render else {
        return Ok(None);
    };
    if let Some(layout) = render.current_layout.clone() {
        return Ok(Some(layout));
    }
    match render.speaker_layout.as_ref() {
        Some(path) => renderer::speaker_layout::SpeakerLayout::from_file(path)
            .map(Some)
            .map_err(|e| anyhow::anyhow!("layout '{}' failed to load: {e}", path.display())),
        None => Ok(None),
    }
}

/// Adopt the live state a departing host handed off, when resuming from standby.
///
/// `enter_standby` writes this instance's unsaved live state to the sidecar so
/// the successor (the mpv-embedded renderer taking the RX port) starts from it.
/// The return leg had no counterpart: this process stays alive across the yield,
/// so nothing ever re-read the config, and `resume` only re-bound the listener.
/// Everything changed in Studio while mpv held the port was therefore dropped
/// the moment we took the port back — and, worse, silently overwritten by our
/// stale pre-yield state on the next save.
///
/// The handoff is symmetric: whichever side departs leaves its state behind,
/// and whichever side takes the port over reads it.
///
/// The read is unconditional — the saved config every time, plus the sidecar if
/// one is there. The departing host may have left unsaved edits (sidecar) or
/// saved over `config.yaml`, which clears the dirty flag and so writes no
/// sidecar at all; and it may have done neither. Rather than detect which, this
/// simply re-reads: after standing by, the file is authoritative and our
/// in-memory state is not evidence of anything.
///
/// An earlier revision gated the config leg on the file's mtime having moved.
/// That worked, but it made correctness depend on filesystem timestamp
/// granularity and on nothing else writing within the same tick — a whole class
/// of "did we notice?" bugs, for the sake of skipping a rebuild at the one
/// moment the instance is already re-acquiring its audio output. Not worth the
/// ambiguity.
///
/// `load_or_default_with_live` is consume-once, so this both reads and clears
/// the sidecar. Reuses the profile-switch application path: a config arriving
/// from outside the process is the same problem as a profile being switched in
/// — re-seed the live params, stage the layout, rebuild the topology in the
/// background while audio keeps playing on the previous one.
pub(crate) fn adopt_handoff_live_state(
    control: &Arc<RendererControl>,
    socket: &Arc<UdpSocket>,
    clients: &Arc<OscClientRegistry>,
    gaintable_cache: &Arc<GaintableCache>,
) {
    let Some(path) = control.config_path.lock().as_ref().cloned() else {
        return;
    };
    let (config, restored) = renderer::config::Config::load_or_default_with_live(&path);
    // The adopted state may be another instance's parse-error fallback, or
    // the file this instance once failed to parse may since load: the status
    // the Save refusal keys on follows the state, as at boot.
    let status = renderer::config::live_load_status(&path, &config, restored);
    apply_switched_profile(&config, control, socket, clients, gaintable_cache);
    control.set_config_status(Some(status.as_str().into()));
    if restored {
        // Sidecar state only ever lived in that file, so it is unsaved by
        // definition — the save indicator must show it as pending, exactly as
        // the engine does when it restores a sidecar at startup.
        control.mark_dirty();
    } else {
        // Adopted straight from config.yaml, which the departing host saved.
        // Marking it dirty here would invent a phantom unsaved diff against the
        // very file it came from.
        control.mark_clean();
    }
    // The save flag alone changes nothing on the wire: the state bundle is
    // re-broadcast off the live-state generation. Without this the adoption
    // would be applied to the audio but never reach Studio, which would keep
    // displaying — and then save — the values we just replaced.
    control.bump_live_state();
    log::info!(
        "standby resume: adopted the live state handed off by the previous host ({})",
        if restored { "sidecar" } else { "saved config" }
    );
}

/// `reload_config` for a host that cannot restart its pipeline (the embedded
/// FFI renderer: mpv owns its lifecycle, so the CLI's restart-from-config loop
/// has no counterpart there and the request used to be dropped on the floor).
///
/// Same contract as the CLI restart — discard live state, re-read the config —
/// through the profile-switch application path: forget any handoff overlay,
/// load `config.yaml`, re-seed the live params, stage its layout and rebuild
/// the topology while audio keeps playing. Host-owned fields (output device,
/// live input, bridge path) only take effect at engine start, as with a
/// profile switch.
pub(crate) fn reload_config_in_place(
    control: &Arc<RendererControl>,
    host: Option<&Arc<dyn HostControlHandler>>,
    socket: &Arc<UdpSocket>,
    clients: &Arc<OscClientRegistry>,
    gaintable_cache: &Arc<GaintableCache>,
) {
    let Some(path) = control.config_path() else {
        log::warn!("OSC reload_config: no config path available");
        return;
    };
    renderer::config::discard_live_sidecar(&path);
    let (config, status) = renderer::config::Config::load_or_default_with_status(&path);
    control.set_profiles_info(config.profiles_info());
    apply_switched_profile(&config, control, socket, clients, gaintable_cache);
    // The live state now is the file: nothing left to save. A file the user
    // fixed lifts the parse-error refusal on Save; one still broken keeps it.
    control.mark_clean();
    control.set_config_status(Some(status.as_str().into()));
    broadcast_string(socket, clients, osc_contract::STATE_CONFIG_SAVE_ERROR, "");
    broadcast_int(socket, clients, osc_contract::STATE_CONFIG_SAVED, 1);
    broadcast_profiles_state(control, socket, clients);
    build_live_state(control, host).broadcast(socket, clients);
    log::info!(
        "OSC reload_config: reloaded {} in place (active profile '{}')",
        path.display(),
        config.active_profile_name()
    );
}

/// Apply the freshly switched-in `render:` section to the running engine:
/// stage the profile's layout, re-seed the live params through the shared
/// construction seeds, and kick the background topology rebuild. Audio keeps
/// playing on the previous topology until the rebuilt one is published; a
/// rebuild failure surfaces through the standard recompute_error broadcast.
fn apply_switched_profile(
    config: &renderer::config::Config,
    control: &Arc<RendererControl>,
    socket: &Arc<UdpSocket>,
    clients: &Arc<OscClientRegistry>,
    gaintable_cache: &Arc<GaintableCache>,
) {
    let Some(render) = config.render.as_ref() else {
        return;
    };

    // Preflighted by the handler, so a load failure here is unexpected — but
    // still keep the current layout rather than half-applying.
    let new_layout = match resolve_profile_layout(config.render.as_ref()) {
        Ok(layout) => layout,
        Err(e) => {
            log::warn!("profile switch: layout resolution failed after preflight: {e}");
            None
        }
    };
    if let Some(layout) = new_layout {
        // Per-speaker live params follow the layout: re-seed the delays the
        // same way construction does (shared helper), dropping the previous
        // profile's runtime gains/mutes (they belong to the speakers we just
        // left).
        {
            let mut live = control.live.write();
            live.speakers = renderer::live_params::speaker_live_from_layout(&layout);
        }
        control.with_editable_layout(|l| *l = layout);
    }

    if let Err(e) = crate::renderer_build::apply_render_config_live(control, render) {
        log::warn!("profile switch: live re-seed failed: {e}");
    }

    // Synthesized-object plans key on the options epoch; the wholesale
    // re-seed above may have changed any of them without going through
    // `options::apply_to_control`, so invalidate once explicitly.
    control.bump_options_epoch();
    control.bump_geometry_generation();
    trigger_layout_recompute(control, socket, clients, gaintable_cache);
}

#[cfg(test)]
mod tests {
    use super::*;
    use renderer::config::{Config, RenderConfig};
    use renderer::live_params::{LiveEvaluationMode, PreferredEvaluationMode};
    use renderer::spatial_renderer::{RendererSpec, SpatialRenderer};
    use renderer::spatial_vbap::{DistanceModel, VbapTableMode};
    use renderer::speaker_layout::SpeakerLayout;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    /// A real `RendererControl` on a 7.1.4 layout, small grid so the table
    /// build stays trivial (same fixture as the live-options conformance net).
    fn fixture_control() -> Arc<RendererControl> {
        let layout = SpeakerLayout::preset("7.1.4").expect("7.1.4 preset");
        SpatialRenderer::new(RendererSpec {
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
        .expect("fixture renderer")
        .renderer_control()
    }

    fn config_with_layout(preset: &str) -> Config {
        Config {
            render: Some(RenderConfig {
                current_layout: Some(SpeakerLayout::preset(preset).unwrap()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// An embedded host cannot restart, so `reload_config` must re-read the
    /// saved config in place: the stale handoff sidecar (here a 7.1.4 live
    /// state, as carried from instance to instance) is discarded, the saved
    /// layout is staged, and the state is clean.
    #[test]
    fn reload_in_place_restores_saved_layout_and_drops_sidecar() {
        let dir =
            std::env::temp_dir().join(format!("orender-reload-in-place-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yaml");
        config_with_layout("9.1.6").save(&path).unwrap();
        let sidecar = renderer::config::live_sidecar_path(&path);
        config_with_layout("7.1.4").save(&sidecar).unwrap();

        let control = fixture_control();
        control.set_config_path(path.clone());
        control.mark_dirty();
        // The engine came up on a file that failed to parse, since fixed.
        control.set_config_status(Some("parse_error".into()));
        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").unwrap());
        let clients = Arc::new(OscClientRegistry::new(Duration::from_secs(5)));
        let gaintable_cache = Arc::new(GaintableCache::new());

        reload_config_in_place(&control, None, &socket, &clients, &gaintable_cache);

        let expected = SpeakerLayout::preset("9.1.6").unwrap();
        assert_eq!(
            control.editable_layout().speaker_names(),
            expected.speaker_names()
        );
        assert!(!sidecar.exists(), "stale sidecar not discarded");
        assert!(!control.config_dirty.load(Ordering::Relaxed));
        assert_eq!(control.config_status().as_deref(), Some("loaded"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Codex's standby sequence: this instance loaded the file and went to
    /// standby; the file broke, the incoming instance ran on the defaults,
    /// the file was fixed, and that instance handed its fallback over. On
    /// resume the adopted state is still the fallback, so the Save must be
    /// refused although this instance loaded the file and it parses again.
    /// Adopting a state that is not a fallback lifts an old parse_error.
    #[test]
    fn a_standby_resume_adopts_the_parse_error_status_of_the_handed_over_state() {
        let dir =
            std::env::temp_dir().join(format!("orender-standby-resume-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yaml");
        config_with_layout("9.1.6").save(&path).unwrap();
        let fixed = std::fs::read(&path).unwrap();
        let sidecar = renderer::config::live_sidecar_path(&path);
        let mut fallback = config_with_layout("7.1.4");
        fallback.live_from_parse_error = true;
        fallback.save_without_backup(&sidecar).unwrap();

        let control = fixture_control();
        control.set_config_path(path.clone());
        control.set_config_status(Some("loaded".into()));
        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").unwrap());
        let clients = Arc::new(OscClientRegistry::new(Duration::from_secs(5)));
        let gaintable_cache = Arc::new(GaintableCache::new());

        adopt_handoff_live_state(&control, &socket, &clients, &gaintable_cache);
        assert_eq!(control.config_status().as_deref(), Some("parse_error"));
        assert!(runtime_control::persist::save_live_config(&control, None).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), fixed);

        // A handover from an instance that loaded the file: no fallback left.
        config_with_layout("7.1.4")
            .save_without_backup(&sidecar)
            .unwrap();
        adopt_handoff_live_state(&control, &socket, &clients, &gaintable_cache);
        assert_eq!(control.config_status().as_deref(), Some("loaded"));

        renderer::config::discard_live_sidecar(&path);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A config holding two profiles, "a" (active) and "b", both 7.1.4 with
    /// every option at its default, and a control running "a" with one
    /// unsaved edit: surround placement set to `back`.
    fn two_profiles_with_an_unsaved_edit(tag: &str) -> (std::path::PathBuf, Arc<RendererControl>) {
        let dir =
            std::env::temp_dir().join(format!("orender-profile-ops-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yaml");
        let mut config = config_with_layout("7.1.4");
        config.active_profile = Some("a".into());
        config.create_profile("b").unwrap();
        config.save(&path).unwrap();

        let control = fixture_control();
        control.set_config_path(path.clone());
        control.live.write().surround_placement = renderer::live_params::SurroundPlacement::Back;
        control.mark_dirty();
        (path, control)
    }

    /// Handle one profile message and return the `save_error` strings a
    /// connected client received for it.
    fn run(control: &Arc<RendererControl>, addr: &str, args: &[&str]) -> Vec<String> {
        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").unwrap());
        let clients = Arc::new(OscClientRegistry::new(Duration::from_secs(5)));
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        clients.insert_permanent(client.local_addr().unwrap());
        let msg = OscMessage {
            addr: addr.to_string(),
            args: args
                .iter()
                .map(|a| OscType::String(a.to_string()))
                .collect(),
        };
        assert!(handle_profile_message(
            &msg,
            control,
            None,
            &socket,
            &clients,
            &Arc::new(GaintableCache::new()),
        ));
        save_errors(&client)
    }

    /// The `save_error` strings queued on `client`: everything was sent
    /// before the handler returned, so draining without waiting is enough.
    fn save_errors(client: &UdpSocket) -> Vec<String> {
        fn collect(packet: rosc::OscPacket, out: &mut Vec<String>) {
            match packet {
                rosc::OscPacket::Message(msg) => {
                    if msg.addr == osc_contract::STATE_CONFIG_SAVE_ERROR {
                        if let Some(OscType::String(text)) = msg.args.into_iter().next() {
                            out.push(text);
                        }
                    }
                }
                rosc::OscPacket::Bundle(bundle) => {
                    for inner in bundle.content {
                        collect(inner, out);
                    }
                }
            }
        }
        client.set_nonblocking(true).unwrap();
        let mut out = Vec::new();
        let mut buf = vec![0u8; 70_000];
        while let Ok(len) = client.recv(&mut buf) {
            let (_, packet) = rosc::decoder::decode_udp(&buf[..len]).expect("valid OSC");
            collect(packet, &mut out);
        }
        out
    }

    /// Every profile operation ends in a write, so on a file that fails to
    /// parse each one is refused and the file left byte-identical.
    #[test]
    fn profile_operations_leave_a_file_that_fails_to_parse_untouched() {
        let (path, control) = two_profiles_with_an_unsaved_edit("parse-error");
        let corrupt = "profiles: [ unterminated\n";
        std::fs::write(&path, corrupt).unwrap();
        for (addr, args) in [
            (osc_contract::CONTROL_PROFILE_CREATE, &["c"][..]),
            (osc_contract::CONTROL_PROFILE_SWITCH, &["b", "save"][..]),
            (osc_contract::CONTROL_PROFILE_RENAME, &["a", "z"][..]),
            (osc_contract::CONTROL_PROFILE_DELETE, &["b"][..]),
        ] {
            let errors = run(&control, addr, args);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), corrupt, "{addr}");
            // The refusal is the only thing the user sees: it must reach them.
            assert!(
                errors.iter().any(|e| e.contains("left untouched")),
                "{addr}: {errors:?}"
            );
        }
    }

    /// The engine came up on defaults because the file failed to parse, and
    /// the user has since fixed it. Save-and-switch must not write those
    /// defaults into the outgoing profile; a plain switch reads the file into
    /// the live state, after which saving is allowed again.
    #[test]
    fn a_switch_lifts_the_parse_error_refusal_a_save_and_switch_hits() {
        let (path, control) = two_profiles_with_an_unsaved_edit("parse-error-fixed");
        control.set_config_status(Some("parse_error".into()));
        let before = std::fs::read_to_string(&path).unwrap();

        let errors = run(
            &control,
            osc_contract::CONTROL_PROFILE_SWITCH,
            &["b", "save"],
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        assert!(
            errors.iter().any(|e| e.contains("press Reload")),
            "{errors:?}"
        );
        assert_eq!(Config::load_or_default(&path).active_profile_name(), "a");

        run(&control, osc_contract::CONTROL_PROFILE_SWITCH, &["b"]);
        assert_eq!(Config::load_or_default(&path).active_profile_name(), "b");
        assert_eq!(control.config_status().as_deref(), Some("loaded"));
        runtime_control::persist::save_live_config(&control, None).expect("save after switch");
    }

    /// Whether profile `name`, as the file says, sets surround placement to
    /// `back` (the unsaved edit of the fixture).
    fn saved_back(path: &std::path::Path, name: &str) -> bool {
        let config = Config::load_or_default(path);
        let render = if config.active_profile_name() == name {
            config.render.clone()
        } else {
            config.profiles.get(name).cloned()
        };
        render.expect("profile present").surround_placement
            == Some(renderer::live_params::SurroundPlacement::Back)
    }

    /// A switch without "save" discards the unsaved edit: the outgoing
    /// profile's file is left as it was, and the live state is the incoming
    /// profile's.
    #[test]
    fn a_plain_switch_never_saves_the_unsaved_edits() {
        let (path, control) = two_profiles_with_an_unsaved_edit("switch");
        run(&control, osc_contract::CONTROL_PROFILE_SWITCH, &["b"]);
        assert!(!saved_back(&path, "a"));
        assert_eq!(Config::load_or_default(&path).active_profile_name(), "b");
        assert!(!control.config_dirty.load(Ordering::Relaxed));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// `[name, "save"]` is the Save button, then the switch.
    #[test]
    fn save_and_switch_keeps_the_edits_in_the_outgoing_profile() {
        let (path, control) = two_profiles_with_an_unsaved_edit("save-switch");
        run(
            &control,
            osc_contract::CONTROL_PROFILE_SWITCH,
            &["b", "save"],
        );
        assert!(saved_back(&path, "a"));
        assert!(!saved_back(&path, "b"));
        assert_eq!(Config::load_or_default(&path).active_profile_name(), "b");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// Create copies what the user hears into the new profile; the active one
    /// keeps its file, and its edit stays unsaved.
    #[test]
    fn create_copies_the_live_state_and_leaves_the_active_profile_alone() {
        let (path, control) = two_profiles_with_an_unsaved_edit("create");
        run(&control, osc_contract::CONTROL_PROFILE_CREATE, &["c"]);
        assert!(saved_back(&path, "c"));
        assert!(!saved_back(&path, "a"));
        assert!(control.config_dirty.load(Ordering::Relaxed));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// Rename and delete are bookkeeping: the edit stays unsaved.
    #[test]
    fn rename_and_delete_leave_the_unsaved_edits_pending() {
        let (path, control) = two_profiles_with_an_unsaved_edit("rename");
        run(
            &control,
            osc_contract::CONTROL_PROFILE_RENAME,
            &["a", "main"],
        );
        run(&control, osc_contract::CONTROL_PROFILE_DELETE, &["b"]);
        let config = Config::load_or_default(&path);
        assert_eq!(config.active_profile_name(), "main");
        assert_eq!(config.profile_names(), vec!["main".to_string()]);
        assert!(!saved_back(&path, "main"));
        assert!(control.config_dirty.load(Ordering::Relaxed));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
