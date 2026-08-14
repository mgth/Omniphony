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

use renderer::live_params::RendererControl;
use rosc::{OscMessage, OscType};
use runtime_control::HostControlHandler;
use runtime_control::osc_contract;

use super::client_registry::OscClientRegistry;
use super::export::build_live_state_bundle;
use super::gaintable::GaintableCache;
use super::recompute::trigger_layout_recompute;
use super::transport::{broadcast_int, broadcast_string, send_raw};

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

    let mut config = renderer::config::Config::load_or_default(&path);

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

    // Commit the current live state into `render:` first, so the operation
    // acts on — and the outgoing/copied profile captures — what the user
    // actually hears, including unsaved tweaks (same spirit as the handoff
    // sidecar: a deliberate profile action must not lose them). Deliberately
    // WITHOUT the host amend: host-owned fields (output device, live input,
    // resampling, latency) only take effect at engine start, so after a
    // switch the running host state describes the PREVIOUS profile — amending
    // here would overwrite the new profile's saved output settings with it.
    // The on-disk values (already in `config`) stay as persisted.
    runtime_control::persist::store_live_into_config(control, None, &mut config);

    let result = if is_switch {
        config.switch_profile(&name)
    } else if addr == osc_contract::CONTROL_PROFILE_CREATE {
        config.create_profile(&name)
    } else if addr == osc_contract::CONTROL_PROFILE_DELETE {
        config.delete_profile(&name)
    } else {
        match msg.args.get(1) {
            Some(OscType::String(new)) => config.rename_profile(&name, new),
            _ => {
                log::warn!("OSC {addr}: rename needs [old, new]");
                return true;
            }
        }
    };
    if let Err(e) = result {
        log::warn!("OSC {addr} '{name}': {e}");
        broadcast_profiles_state(control, socket, clients);
        return true;
    }

    if let Err(e) = config.save(&path) {
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
    // A deliberate profile mutation supersedes any pending live-handoff overlay.
    renderer::config::discard_live_sidecar(&path);

    control.set_profiles_info(config.profiles_info());

    if is_switch {
        apply_switched_profile(&config, control, socket, clients, gaintable_cache);
    }

    // Everything the user heard is now in the file (the commit above), so the
    // dirty indicator clears like it does after an explicit save, and any
    // stale save-error banner from an earlier attempt clears with it.
    control.mark_clean();
    broadcast_string(socket, clients, osc_contract::STATE_CONFIG_SAVE_ERROR, "");
    broadcast_int(socket, clients, osc_contract::STATE_CONFIG_SAVED, 1);
    broadcast_profiles_state(control, socket, clients);
    // Full state refresh so every client view (options, layout, binaural,
    // gains…) re-syncs to the post-operation state.
    let state_bytes = build_live_state_bundle(control, host);
    send_raw(socket, clients, &state_bytes);
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
/// The handoff is now symmetric: whichever side departs writes the sidecar, and
/// whichever side takes the port over consumes it. `load_or_default_with_live`
/// is consume-once, so this both reads and clears it.
///
/// Two ways the truth can have moved while we stood by, and both must be picked
/// up for the switchover to be invisible:
///
/// * a **sidecar** — the departing host had unsaved changes and handed them over;
/// * a **newer config.yaml** — the departing host *saved* instead, which cleared
///   its dirty flag and so wrote no sidecar at all.
///
/// Neither means our in-memory state is still valid. Only when both are absent
/// is it left alone.
///
/// Reuses the profile-switch application path: a config arriving from outside
/// the process is the same problem as a profile being switched in — re-seed the
/// live params, stage the layout, rebuild the topology in the background while
/// audio keeps playing on the previous one.
pub(crate) fn adopt_handoff_live_state(
    control: &Arc<RendererControl>,
    socket: &Arc<UdpSocket>,
    clients: &Arc<OscClientRegistry>,
    gaintable_cache: &Arc<GaintableCache>,
    config_mtime_at_standby: Option<std::time::SystemTime>,
) {
    let Some(path) = control.config_path.lock().as_ref().cloned() else {
        return;
    };
    let (config, restored) = renderer::config::Config::load_or_default_with_live(&path);
    // The sidecar only carries *unsaved* state. If the departing host saved
    // instead, it cleared the dirty flag, so it wrote no sidecar at all — and
    // the truth moved to config.yaml while we stood by. Adopting only on a
    // restored sidecar would leave us running our pre-yield state against a
    // config file that now says something else, and reporting it as saved.
    // That is the case where the user was most explicit about the change.
    if !restored && !config_changed_since(&path, config_mtime_at_standby) {
        return;
    }
    apply_switched_profile(&config, control, socket, clients, gaintable_cache);
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

/// Whether `path` was modified after we entered standby. Conservative: an
/// unreadable mtime, or no recorded baseline, reports no change — a spurious
/// adopt would rebuild the topology on every mpv quit for nothing.
fn config_changed_since(path: &std::path::Path, since: Option<std::time::SystemTime>) -> bool {
    let Some(since) = since else {
        return false;
    };
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .map(|modified| modified > since)
        .unwrap_or(false)
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
mod handoff_tests {
    use super::config_changed_since;
    use std::time::{Duration, SystemTime};

    fn temp_config(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("omniphony-handoff-{name}.yaml"));
        std::fs::write(&path, "render: {}\n").unwrap();
        path
    }

    /// The second trigger of the adoption: the departing host saved over
    /// config.yaml, which writes no sidecar because saving clears the dirty
    /// flag. A newer mtime is then the only evidence the truth moved.
    #[test]
    fn a_config_saved_during_standby_is_detected() {
        let path = temp_config("saved");
        let before_save = SystemTime::now() - Duration::from_secs(60);
        assert!(
            config_changed_since(&path, Some(before_save)),
            "a config written after we stood by must trigger the adoption"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// An untouched config must NOT trigger it: a spurious adopt would rebuild
    /// the topology on every single mpv quit for no reason.
    #[test]
    fn an_untouched_config_does_not_trigger_adoption() {
        let path = temp_config("untouched");
        let after_write = SystemTime::now() + Duration::from_secs(60);
        assert!(!config_changed_since(&path, Some(after_write)));
        let _ = std::fs::remove_file(&path);
    }

    /// No baseline (never stood by) and an unreadable path both stay quiet,
    /// rather than adopting on a guess.
    #[test]
    fn missing_baseline_or_file_stays_quiet() {
        let path = temp_config("baseline");
        assert!(!config_changed_since(&path, None));
        let _ = std::fs::remove_file(&path);
        assert!(!config_changed_since(&path, Some(SystemTime::UNIX_EPOCH)));
    }
}
