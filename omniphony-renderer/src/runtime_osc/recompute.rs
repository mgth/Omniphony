use std::sync::Arc;

use renderer::live_params::RendererControl;

use super::client_registry::OscClientRegistry;
use super::transport::{broadcast_fff, broadcast_int, broadcast_string};

pub(crate) fn trigger_layout_recompute(
    control: &Arc<RendererControl>,
    socket: &Arc<std::net::UdpSocket>,
    clients: &Arc<OscClientRegistry>,
) {
    #[cfg(not(feature = "saf_vbap"))]
    {
        let _ = control;
        log::warn!("OSC apply: VBAP recompute requires a build with the 'saf_vbap' feature");
        broadcast_int(socket, clients, "/omniphony/state/speakers/recomputing", 0);
        return;
    }

    #[cfg(feature = "saf_vbap")]
    {
        if control.vbap_rebuild_params.is_none() {
            log::warn!(
                "OSC apply: VBAP speaker positions cannot be updated — pre-loaded table does not support recompute"
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
                log::warn!("OSC apply: failed to prepare VBAP recompute plan");
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
            .name("vbap-recompute".into())
            .spawn(move || {
                log::info!(
                    "VBAP recompute started (azimuth_resolution={}, elevation_resolution={}, distance_res={}, distance_max={}, mode={:?})",
                    rebuild_plan_for_thread.azimuth_resolution,
                    rebuild_plan_for_thread.elevation_resolution,
                    rebuild_plan_for_thread.distance_res,
                    rebuild_plan_for_thread.distance_max,
                    rebuild_plan_for_thread.table_mode
                );
                match rebuild_plan_for_thread.build_topology() {
                    Ok(new_topology) => {
                        control_clone.publish_topology(new_topology);
                        control_clone
                            .recomputing
                            .store(false, std::sync::atomic::Ordering::Relaxed);
                        log::info!("VBAP updated with new speaker layout");
                        let effective_mode = match control_clone.active_topology().vbap.table_mode()
                        {
                            renderer::spatial_vbap::VbapTableMode::Polar => "polar",
                            renderer::spatial_vbap::VbapTableMode::Cartesian { .. } => "cartesian",
                        };
                        broadcast_string(
                            &socket_clone,
                            &clients_clone,
                            "/omniphony/state/vbap/effective_mode",
                            effective_mode,
                        );
                        broadcast_int(
                            &socket_clone,
                            &clients_clone,
                            "/omniphony/state/speakers/recomputing",
                            0,
                        );
                        for (idx, speaker) in rebuild_plan_for_thread.layout.speakers.iter().enumerate() {
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
                        log::info!("VBAP recompute completed");
                    }
                    Err(e) => {
                        log::error!("VBAP recompute failed: {}", e);
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
}
