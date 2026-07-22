use std::sync::Arc;

use renderer::live_params::RendererControl;
use runtime_control::context::RuntimeControlContext;
use runtime_control::osc::gaintable_chunk_broadcasts;
use runtime_control::snapshot::{build_renderer_state_json, build_speakers_state_json};

use super::client_registry::OscClientRegistry;
use super::gaintable::GaintableCache;
use super::transport::{broadcast_int, broadcast_string, send_update_to_client};

pub(crate) fn trigger_layout_recompute(
    control: &Arc<RendererControl>,
    socket: &Arc<std::net::UdpSocket>,
    clients: &Arc<OscClientRegistry>,
    gaintable_cache: &Arc<GaintableCache>,
) {
    if control.prepare_topology_rebuild().is_none() {
        log::warn!(
            "OSC apply: speaker positions cannot be updated — requested backend rebuild could not be prepared"
        );
        broadcast_int(socket, clients, "/omniphony/state/speakers/recomputing", 0);
        return;
    }

    if control
        .recomputing
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        log::warn!("OSC apply: VBAP recompute already in progress, ignoring");
        broadcast_int(socket, clients, "/omniphony/state/speakers/recomputing", 1);
        return;
    }

    let rebuild_plan = match control.prepare_topology_rebuild() {
        Some(plan) => plan,
        None => {
            log::warn!("OSC apply: failed to prepare render backend recompute plan");
            broadcast_int(socket, clients, "/omniphony/state/speakers/recomputing", 0);
            return;
        }
    };

    control
        .recomputing
        .store(true, std::sync::atomic::Ordering::Relaxed);
    broadcast_int(socket, clients, "/omniphony/state/speakers/recomputing", 1);
    broadcast_string(
        socket,
        clients,
        "/omniphony/state/speakers/recompute_error",
        "",
    );

    let control_clone = Arc::clone(control);
    let socket_clone = Arc::clone(socket);
    let clients_clone = Arc::clone(clients);
    let gaintable_cache_clone = Arc::clone(gaintable_cache);
    let rebuild_plan_for_thread = rebuild_plan.clone();

    std::thread::Builder::new()
        .name("render-backend-recompute".into())
        .spawn(move || {
            log::info!(
                "Render backend recompute started ({})",
                rebuild_plan_for_thread.log_summary()
            );
            let current_topology = control_clone.active_topology();
            let build_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                rebuild_plan_for_thread.build_topology_reusing(Some(&current_topology))
            }))
            .unwrap_or_else(|payload| {
                let detail = if let Some(msg) = payload.downcast_ref::<&'static str>() {
                    (*msg).to_string()
                } else if let Some(msg) = payload.downcast_ref::<String>() {
                    msg.clone()
                } else {
                    "panic with non-string payload".to_string()
                };
                Err(anyhow::anyhow!(
                    "render backend panicked during build_topology: {detail}"
                ))
            });
            match build_result {
                Ok(new_topology) => {
                    control_clone.publish_topology(new_topology);
                    control_clone
                        .recomputing
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                    log::info!(
                        "Render backend {} updated with new speaker layout",
                        rebuild_plan_for_thread.backend_id()
                    );
                    let renderer_state_json = {
                        let live = control_clone.live.read();
                        let topology = control_clone.active_topology();
                        let scale_m = control_clone.editable_layout().radius_m;
                        // Speaker names that don't resolve to a known channel
                        // label — can't be routed by position in by_name mode.
                        let unroutable: Vec<String> = topology
                            .speaker_layout
                            .speakers
                            .iter()
                            .filter(|s| {
                                crate::channel_layout::label_for_speaker_name(&s.name)
                                    == bridge_api::RChannelLabel::Unknown
                            })
                            .map(|s| s.name.clone())
                            .collect();
                        build_renderer_state_json(
                            &live,
                            &topology,
                            scale_m,
                            control_clone.available_backends(),
                            control_clone.all_backend_params(),
                            &unroutable,
                            &control_clone.fixed_channel_catalog(),
                            &control_clone.fixed_channel_processing(),
                        )
                    };
                    let layout_json = {
                        let layout = control_clone.editable_layout();
                        serde_json::to_string(&layout).unwrap_or_else(|_| "{}".to_string())
                    };
                    let speakers_state_json = {
                        let live = control_clone.live.read();
                        let layout = control_clone.editable_layout();
                        build_speakers_state_json(&live, &layout)
                    };
                    broadcast_string(
                        &socket_clone,
                        &clients_clone,
                        "/omniphony/state/renderer",
                        &renderer_state_json,
                    );
                    broadcast_string(
                        &socket_clone,
                        &clients_clone,
                        "/omniphony/state/layout",
                        &layout_json,
                    );
                    broadcast_string(
                        &socket_clone,
                        &clients_clone,
                        "/omniphony/state/speakers",
                        &speakers_state_json,
                    );
                    broadcast_int(
                        &socket_clone,
                        &clients_clone,
                        "/omniphony/state/speakers/recomputing",
                        0,
                    );
                    // The precomputed gain table changed with the new topology.
                    // Push each live subscriber its own speaker's per-band field
                    // (targeted unicast), only when its cached version differs. The
                    // full table is rebuilt once into the cache; per-speaker bytes
                    // are serialized cheaply. Skipped when nobody is subscribed.
                    gaintable_cache_clone.invalidate();
                    let subscribers = clients_clone.gaintable_subscribers();
                    if !subscribers.is_empty() {
                        let ctx = RuntimeControlContext::new(Arc::clone(&control_clone));
                        for (addr, client_version, speaker) in subscribers {
                            let speaker = speaker.unwrap_or(0);
                            if let Some((version, bytes)) =
                                gaintable_cache_clone.bytes_for_speaker(&ctx, speaker)
                            {
                                if client_version != Some(version) {
                                    for update in gaintable_chunk_broadcasts(&bytes, None) {
                                        send_update_to_client(&socket_clone, addr, &update);
                                    }
                                    clients_clone.set_gaintable_version(addr, version);
                                }
                            }
                        }
                    }
                    log::info!("Render backend recompute completed");
                }
                Err(e) => {
                    let message = format!(
                        "Render backend {} recompute failed: {}",
                        rebuild_plan_for_thread.backend_id(),
                        e
                    );
                    log::error!("{message}");
                    control_clone
                        .recomputing
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                    broadcast_string(
                        &socket_clone,
                        &clients_clone,
                        "/omniphony/state/speakers/recompute_error",
                        &message,
                    );
                    broadcast_int(
                        &socket_clone,
                        &clients_clone,
                        "/omniphony/state/speakers/recomputing",
                        0,
                    );
                }
            }
        })
        .expect("failed to spawn vbap-recompute thread");
}
