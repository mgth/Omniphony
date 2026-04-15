use std::sync::Arc;

use renderer::live_params::RendererControl;
use runtime_control::snapshot::build_render_backend_state_json;

use super::client_registry::OscClientRegistry;
use super::transport::{broadcast_fff, broadcast_int, broadcast_string};

pub(crate) fn trigger_layout_recompute(
    control: &Arc<RendererControl>,
    socket: &Arc<std::net::UdpSocket>,
    clients: &Arc<OscClientRegistry>,
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

        let control_clone = Arc::clone(control);
        let socket_clone = Arc::clone(socket);
        let clients_clone = Arc::clone(clients);
        let rebuild_plan_for_thread = rebuild_plan.clone();

        std::thread::Builder::new()
            .name("render-backend-recompute".into())
            .spawn(move || {
                log::info!(
                    "Render backend recompute started ({})",
                    rebuild_plan_for_thread.log_summary()
                );
                match rebuild_plan_for_thread.build_topology() {
                    Ok(new_topology) => {
                        control_clone.publish_topology(new_topology);
                        control_clone
                            .recomputing
                            .store(false, std::sync::atomic::Ordering::Relaxed);
                        log::info!(
                            "Render backend {} updated with new speaker layout",
                            rebuild_plan_for_thread.backend_id()
                        );
                        let effective_backend =
                            control_clone.active_topology().backend.kind().as_str();
                        let effective_evaluation_mode = control_clone
                            .active_topology()
                            .backend
                            .evaluation_mode()
                            .as_str();
                        let render_backend_state_json = {
                            let live = control_clone.live.read().unwrap();
                            let topology = control_clone.active_topology();
                            build_render_backend_state_json(&live, &topology)
                        };
                        broadcast_string(
                            &socket_clone,
                            &clients_clone,
                            "/omniphony/state/render_backend/effective",
                            effective_backend,
                        );
                        broadcast_string(
                            &socket_clone,
                            &clients_clone,
                            "/omniphony/state/render_backend/state",
                            &render_backend_state_json,
                        );
                        broadcast_string(
                            &socket_clone,
                            &clients_clone,
                            "/omniphony/state/render_evaluation_mode/effective",
                            effective_evaluation_mode,
                        );
                        broadcast_int(
                            &socket_clone,
                            &clients_clone,
                            "/omniphony/state/speakers/recomputing",
                            0,
                        );
                        for (idx, speaker) in
                            rebuild_plan_for_thread.layout().speakers.iter().enumerate()
                        {
                            broadcast_fff(
                                &socket_clone,
                                &clients_clone,
                                &format!("/omniphony/state/speaker/{}", idx),
                                speaker.azimuth,
                                speaker.elevation,
                                speaker.distance,
                            );
                            broadcast_int(
                                &socket_clone,
                                &clients_clone,
                                &format!("/omniphony/state/speaker/{}/spatialize", idx),
                                if speaker.spatialize { 1 } else { 0 },
                            );
                            broadcast_string(
                                &socket_clone,
                                &clients_clone,
                                &format!("/omniphony/state/speaker/{}/name", idx),
                                &speaker.name,
                            );
                        }
                        log::info!("Render backend recompute completed");
                    }
                    Err(e) => {
                        log::error!("Render backend recompute failed: {}", e);
                        control_clone
                            .recomputing
                            .store(false, std::sync::atomic::Ordering::Relaxed);
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
