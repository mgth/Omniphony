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

use renderer::live_params::{ProfilesInfo, RendererControl, SpeakerLiveParams};
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

    // Commit the current live state into `render:` first, so the operation
    // acts on — and the outgoing/copied profile captures — what the user
    // actually hears, including unsaved tweaks (same spirit as the handoff
    // sidecar: a deliberate profile action must not lose them).
    let mut config = renderer::config::Config::load_or_default(&path);
    let host_ref: Option<&dyn HostControlHandler> = host.map(|h| h.as_ref());
    runtime_control::persist::store_live_into_config(control, host_ref, &mut config);

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
    let _ = std::fs::remove_file(renderer::config::live_sidecar_path(&path));
    renderer::config::clear_live_overlay_cache();

    control.set_profiles_info(ProfilesInfo {
        active: config.active_profile_name().to_string(),
        names: config.profile_names(),
    });

    if is_switch {
        apply_switched_profile(&config, control, socket, clients, gaintable_cache);
    }

    // Everything the user heard is now in the file (the commit above), so the
    // dirty indicator clears like it does after an explicit save.
    control.mark_clean();
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

    // Embedded layout wins; a path reference is resolved best-effort; a
    // profile without a layout keeps the current one.
    let new_layout = render.current_layout.clone().or_else(|| {
        render.speaker_layout.as_ref().and_then(|p| {
            renderer::speaker_layout::SpeakerLayout::from_file(p)
                .map_err(|e| {
                    log::warn!(
                        "profile switch: layout '{}' failed to load: {e}",
                        p.display()
                    )
                })
                .ok()
        })
    });
    if let Some(layout) = new_layout {
        // Per-speaker live params follow the layout: re-seed the delays the
        // same way construction does, dropping the previous profile's
        // runtime gains/mutes (they belong to the speakers we just left).
        {
            let mut live = control.live.write();
            live.speakers.clear();
            for (idx, spk) in layout.speakers.iter().enumerate() {
                if spk.delay_ms != 0.0 {
                    live.speakers.insert(
                        idx,
                        SpeakerLiveParams {
                            delay_ms: spk.delay_ms.max(0.0),
                            ..Default::default()
                        },
                    );
                }
            }
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
