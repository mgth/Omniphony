use std::sync::Arc;

use renderer::evaluation_grid::GridDecision;
use renderer::live_params::RendererControl;
use runtime_control::context::RuntimeControlContext;
use runtime_control::osc::gaintable_chunk_broadcasts;
use runtime_control::snapshot::{build_renderer_state_json, build_speakers_state_json};

use super::client_registry::OscClientRegistry;
use super::gaintable::GaintableCache;
use super::transport::{broadcast_int, broadcast_string, publish_state, send_update_to_client};
use rosc::{OscMessage, OscType};
use runtime_control::osc_contract;

/// Rebuild the topology from the live state, off the listener's thread, and
/// publish it with the state that goes with it. A rebuild already running
/// queues one more.
pub(crate) fn trigger_layout_recompute(
    control: &Arc<RendererControl>,
    socket: &Arc<std::net::UdpSocket>,
    clients: &Arc<OscClientRegistry>,
    gaintable_cache: &Arc<GaintableCache>,
) {
    start_recompute(control, socket, clients, gaintable_cache, false);
}

/// A stream's bridge hinted another grid: take it, and while the grid
/// follows the bridge, request it ([`request_grid`]). Either way the clients
/// learn the bridge's grid with the next state bundle.
pub(crate) fn follow_bridge_grid(
    control: &Arc<RendererControl>,
    socket: &Arc<std::net::UdpSocket>,
    clients: &Arc<OscClientRegistry>,
    gaintable_cache: &Arc<GaintableCache>,
) {
    if control.take_bridge_grid() {
        request_grid(control, socket, clients, gaintable_cache);
    }
}

/// The grid alone changed (its source switched, the bridge's hint moved):
/// request the live grid (`renderer::evaluation_grid`). The rebuild it may
/// start is an evaluation-only one at idle priority; until it lands, the
/// installed table keeps rendering. When a topology on that grid is at hand
/// it is put back in force and nothing is built; a rebuild running for an
/// older request is then dropped when it finishes, and one that covered
/// more than the grid is run again from the live state.
pub(crate) fn request_grid(
    control: &Arc<RendererControl>,
    socket: &Arc<std::net::UdpSocket>,
    clients: &Arc<OscClientRegistry>,
    gaintable_cache: &Arc<GaintableCache>,
) {
    match control.request_live_grid() {
        GridDecision::Unchanged => {}
        GridDecision::Adopted => {
            log::info!("Evaluation grid: a topology on the requested grid is in force, no rebuild");
            if control
                .recomputing
                .load(std::sync::atomic::Ordering::Relaxed)
                && !control.grid_only_rebuild_in_flight()
            {
                control
                    .recompute_pending
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
            control.bump_live_state();
        }
        GridDecision::Rebuild => start_recompute(control, socket, clients, gaintable_cache, true),
    }
}

fn start_recompute(
    control: &Arc<RendererControl>,
    socket: &Arc<std::net::UdpSocket>,
    clients: &Arc<OscClientRegistry>,
    gaintable_cache: &Arc<GaintableCache>,
    grid_only: bool,
) {
    if control.prepare_topology_rebuild().is_none() {
        log::warn!(
            "OSC apply: speaker positions cannot be updated — requested backend rebuild could not be prepared"
        );
        broadcast_int(socket, clients, osc_contract::STATE_SPEAKERS_RECOMPUTING, 0);
        return;
    }

    if control
        .recomputing
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        // Don't drop the request: the running rebuild snapshotted the live
        // state BEFORE this change (a profile switch or layout edit), so it
        // won't cover it. Flag it and let the finishing recompute re-trigger.
        control
            .recompute_pending
            .store(true, std::sync::atomic::Ordering::Relaxed);
        log::info!("OSC apply: recompute already in progress, queueing a follow-up rebuild");
        broadcast_int(socket, clients, osc_contract::STATE_SPEAKERS_RECOMPUTING, 1);
        return;
    }

    // A rebuild that covers more than the grid answers the live grid: a
    // grid-only one still running for another is out of date.
    if !grid_only {
        control.record_live_grid();
    }
    let rebuild_plan = match control.prepare_topology_rebuild() {
        Some(plan) => plan,
        None => {
            log::warn!("OSC apply: failed to prepare render backend recompute plan");
            control.grid_rebuild_failed();
            broadcast_int(socket, clients, osc_contract::STATE_SPEAKERS_RECOMPUTING, 0);
            return;
        }
    };

    control
        .recomputing
        .store(true, std::sync::atomic::Ordering::Relaxed);
    control.set_grid_only_rebuild_in_flight(grid_only);
    broadcast_int(socket, clients, osc_contract::STATE_SPEAKERS_RECOMPUTING, 1);
    broadcast_string(
        socket,
        clients,
        osc_contract::STATE_SPEAKERS_RECOMPUTE_ERROR,
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
            #[cfg(test)]
            hold::wait_while_held(&control_clone);
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
            // Published unless a later grid request overtook it, whatever its
            // identity.
            let published = build_result.map(|new_topology| {
                control_clone.publish_topology_if_current(
                    new_topology,
                    grid_only.then(|| Arc::clone(&current_topology)),
                )
            });
            match published {
                Ok(false) => {
                    // The topology in force stays, and the request that
                    // overtook it has its own rebuild or none.
                    log::info!(
                        "Render backend recompute discarded: a later evaluation grid request \
                         overtook it"
                    );
                    control_clone
                        .recomputing
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                    broadcast_int(
                        &socket_clone,
                        &clients_clone,
                        osc_contract::STATE_SPEAKERS_RECOMPUTING,
                        0,
                    );
                    if control_clone
                        .recompute_pending
                        .swap(false, std::sync::atomic::Ordering::Relaxed)
                    {
                        trigger_layout_recompute(
                            &control_clone,
                            &socket_clone,
                            &clients_clone,
                            &gaintable_cache_clone,
                        );
                    }
                }
                Ok(true) => {
                    control_clone
                        .recomputing
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                    log::info!(
                        "Render backend {} updated with new speaker layout",
                        rebuild_plan_for_thread.backend_id()
                    );
                    // Read under the publication lock: a snapshot the
                    // listener publishes meanwhile is either all before
                    // these or all after, never newer under a lower count.
                    publish_state(&socket_clone, &clients_clone, || {
                        let renderer_state_json = {
                            // Before the live lock: it reads the live params.
                            let brir_layout_error = control_clone.brir_layout().err();
                            let live = control_clone.live.read();
                            let topology = control_clone.active_topology();
                            let scale_m = control_clone.editable_layout().radius_m;
                            // Speaker names that don't resolve to a known channel
                            // label — can't be routed by position in by_name mode.
                            // A BRIR set's loudspeakers are named by the renderer,
                            // not the user: nothing to warn about there.
                            let unroutable: Vec<String> = topology
                                .speaker_layout
                                .speakers
                                .iter()
                                .filter(|_| !topology.brir_layout)
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
                                control_clone.plugin_params(),
                                &unroutable,
                                &control_clone.fixed_channel_catalog(),
                                &control_clone.fixed_channel_processing(),
                                control_clone.crossover_info(),
                                &control_clone.binaural_hrir_status(),
                                &control_clone.binaural_brir_status(),
                                brir_layout_error,
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
                        let message = |addr: &str, json: String| OscMessage {
                            addr: addr.to_string(),
                            args: vec![OscType::String(json)],
                        };
                        vec![
                            message(osc_contract::STATE_RENDERER, renderer_state_json),
                            message(osc_contract::STATE_LAYOUT, layout_json),
                            message(osc_contract::STATE_SPEAKERS, speakers_state_json),
                        ]
                    });
                    broadcast_int(
                        &socket_clone,
                        &clients_clone,
                        osc_contract::STATE_SPEAKERS_RECOMPUTING,
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
                        for (addr, targets) in subscribers {
                            // Every target the client holds, not just one: a
                            // second display must not starve on a stale cache.
                            for (target, client_version) in targets {
                                if let Some((version, bytes)) =
                                    gaintable_cache_clone.bytes_for_target(&ctx, target)
                                {
                                    if client_version != Some(version) {
                                        for update in gaintable_chunk_broadcasts(
                                            &bytes,
                                            None,
                                            addr.gaintable_chunk_bytes(),
                                        ) {
                                            send_update_to_client(&socket_clone, &addr, &update);
                                        }
                                        clients_clone.set_gaintable_version(&addr, target, version);
                                    }
                                }
                            }
                        }
                    }
                    log::info!("Render backend recompute completed");
                    // A rebuild request arrived while this one was running:
                    // it saw pre-change state, so run once more from the
                    // current live state (spawns a fresh recompute thread).
                    if control_clone
                        .recompute_pending
                        .swap(false, std::sync::atomic::Ordering::Relaxed)
                    {
                        trigger_layout_recompute(
                            &control_clone,
                            &socket_clone,
                            &clients_clone,
                            &gaintable_cache_clone,
                        );
                    }
                }
                Err(e) => {
                    let message = format!(
                        "Render backend {} recompute failed: {}",
                        rebuild_plan_for_thread.backend_id(),
                        e
                    );
                    log::error!("{message}");
                    control_clone.grid_rebuild_failed();
                    control_clone
                        .recomputing
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                    broadcast_string(
                        &socket_clone,
                        &clients_clone,
                        osc_contract::STATE_SPEAKERS_RECOMPUTE_ERROR,
                        &message,
                    );
                    broadcast_int(
                        &socket_clone,
                        &clients_clone,
                        osc_contract::STATE_SPEAKERS_RECOMPUTING,
                        0,
                    );
                    // Same follow-up as the success path: a request queued
                    // behind this failed rebuild still deserves its run (it
                    // may be exactly the change that fixes the failure, e.g.
                    // switching away from a broken profile).
                    if control_clone
                        .recompute_pending
                        .swap(false, std::sync::atomic::Ordering::Relaxed)
                    {
                        trigger_layout_recompute(
                            &control_clone,
                            &socket_clone,
                            &clients_clone,
                            &gaintable_cache_clone,
                        );
                    }
                }
            }
        })
        .expect("failed to spawn vbap-recompute thread");
}

/// Tests that need a recompute still running at a given moment hold the
/// worker of one engine (one `RendererControl`) at its start until the guard
/// is dropped; every other engine's workers run as usual.
#[cfg(test)]
pub(crate) mod hold {
    use std::sync::{Arc, Condvar, Mutex};

    use renderer::live_params::RendererControl;

    static HELD: Mutex<Vec<usize>> = Mutex::new(Vec::new());
    static RELEASED: Condvar = Condvar::new();

    fn key(control: &Arc<RendererControl>) -> usize {
        Arc::as_ptr(control) as usize
    }

    pub(crate) struct Held(usize);

    impl Drop for Held {
        fn drop(&mut self) {
            HELD.lock().unwrap().retain(|&k| k != self.0);
            RELEASED.notify_all();
        }
    }

    /// Hold `control`'s recompute workers until the returned guard drops.
    pub(crate) fn hold(control: &Arc<RendererControl>) -> Held {
        HELD.lock().unwrap().push(key(control));
        Held(key(control))
    }

    pub(super) fn wait_while_held(control: &Arc<RendererControl>) {
        let key = key(control);
        let mut held = HELD.lock().unwrap();
        while held.contains(&key) {
            held = RELEASED.wait(held).unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use renderer::speaker_layout::{Speaker, SpeakerLayout};
    use renderer::test_support::fixture_control;
    use rosc::{OscPacket, OscType};
    use std::net::UdpSocket;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    /// The string a client receives on `addr`, waiting for the first
    /// non-empty one (a recompute first clears the previous error).
    fn next_non_empty_string(client: &UdpSocket, addr: &str) -> Option<String> {
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut buf = vec![0u8; 65536];
        while Instant::now() < deadline {
            let Ok(len) = client.recv(&mut buf) else {
                continue;
            };
            // A state update travels in a bundle with its generation.
            let messages = match rosc::decoder::decode_udp(&buf[..len]) {
                Ok((_, OscPacket::Message(msg))) => vec![msg],
                Ok((_, OscPacket::Bundle(bundle))) => bundle
                    .content
                    .into_iter()
                    .filter_map(|packet| match packet {
                        OscPacket::Message(msg) => Some(msg),
                        OscPacket::Bundle(_) => None,
                    })
                    .collect(),
                Err(_) => continue,
            };
            for msg in messages {
                if msg.addr == addr
                    && let Some(OscType::String(s)) = msg.args.first()
                    && !s.is_empty()
                {
                    return Some(s.clone());
                }
            }
        }
        None
    }

    /// A Cartesian hint of `x` × 5 × 3 cells.
    fn hint(x: u32) -> renderer::evaluation_grid::EvaluationGrid {
        renderer::evaluation_grid::EvaluationGrid::from_hint(
            bridge_api::RVbapCartesianDefaults {
                x_size: x,
                y_size: 5,
                z_size: 3,
                z_neg_size: 0,
                allow_negative_z: false,
            },
            bridge_api::RVbapTableMode::Cartesian,
        )
    }

    /// A renderer a host built on bridge A's hint (`hint(5)`), the grid
    /// following the bridge, and what its listener holds.
    struct Listener {
        renderer: renderer::spatial_renderer::SpatialRenderer,
        control: Arc<RendererControl>,
        socket: Arc<UdpSocket>,
        clients: Arc<OscClientRegistry>,
        cache: Arc<GaintableCache>,
    }

    impl Listener {
        fn new() -> Self {
            let a = hint(5);
            let renderer = crate::renderer_build::build_spatial_renderer(
                &crate::renderer_build::SpatialRendererParams::from_render_config(None),
                SpeakerLayout::preset("7.1.4").expect("preset"),
                48_000,
                bridge_api::RVbapCartesianDefaults {
                    x_size: a.cartesian.x_size as u32,
                    y_size: 5,
                    z_size: 3,
                    z_neg_size: 0,
                    allow_negative_z: false,
                },
                bridge_api::RVbapTableMode::Cartesian,
                None,
            )
            .expect("renderer");
            let control = renderer.renderer_control();
            control.set_relayout_by_host(true);
            assert_eq!(control.live_grid(), a);
            Self {
                renderer,
                control,
                socket: Arc::new(UdpSocket::bind("127.0.0.1:0").unwrap()),
                clients: Arc::new(OscClientRegistry::new(Duration::from_secs(5))),
                cache: Arc::new(GaintableCache::new()),
            }
        }

        /// A stream's bridge hints `grid`; the listener's poll follows it.
        fn stream(&self, grid: renderer::evaluation_grid::EvaluationGrid) {
            assert!(self.control.offer_bridge_grid(grid));
            assert!(self.control.bridge_grid_pending());
            follow_bridge_grid(&self.control, &self.socket, &self.clients, &self.cache);
        }

        /// The grid's source switched to `source` over OSC, alone.
        fn switch_to(&self, source: &str) {
            let spec = renderer::options::find("evaluation_grid").expect("row");
            let applied = renderer::options::apply_to_control(
                &self.control,
                spec,
                &renderer::options::RawOptionValue::Str(source),
            )
            .expect("accepted");
            assert!(applied.changed);
            request_grid(&self.control, &self.socket, &self.clients, &self.cache);
        }

        fn wait_idle(&self) {
            let deadline = Instant::now() + Duration::from_secs(60);
            while self.control.recomputing.load(Ordering::Relaxed) {
                assert!(Instant::now() < deadline, "the recompute never ended");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    /// While the grid follows the bridge, a stream with other hints rebuilds
    /// the topology on them; one with the same hints rebuilds nothing.
    #[test]
    fn a_stream_with_other_hints_rebuilds_once() {
        let listener = Listener::new();
        let control = &listener.control;
        let a = control.active_topology();
        listener.stream(hint(7));
        listener.wait_idle();
        let b = control.active_topology();
        assert!(!Arc::ptr_eq(&a, &b));
        assert_eq!(b.grid, Some(hint(7)));
        assert!(control.topology_grid_is_current(&b));

        // Same hints: not even offered as new.
        assert!(control.offer_bridge_grid(hint(7)));
        assert!(!control.bridge_grid_pending());
        assert!(!control.recomputing.load(Ordering::Relaxed));
        assert!(Arc::ptr_eq(&b, &control.active_topology()));

        // Back to bridge A's stream: the topology it replaced is in force
        // again, nothing is built.
        listener.stream(hint(5));
        assert!(!control.recomputing.load(Ordering::Relaxed));
        assert!(Arc::ptr_eq(&a, &control.active_topology()));
        drop(listener.renderer);
    }

    /// Forcing the grid while the bridge's new grid is being built keeps the
    /// grid in force: the rebuild is discarded when it finishes, and nothing
    /// is rebuilt.
    #[test]
    fn forcing_the_grid_while_the_bridges_one_builds_discards_it() {
        let listener = Listener::new();
        let control = &listener.control;
        let a = control.active_topology();
        let held = hold::hold(control);
        listener.stream(hint(7));
        assert!(control.recomputing.load(Ordering::Relaxed));

        listener.switch_to("custom");
        assert_eq!(control.live_grid(), hint(5), "the grid in force, not B's");
        assert!(!control.recompute_pending.load(Ordering::Relaxed));
        drop(held);
        listener.wait_idle();
        assert!(Arc::ptr_eq(&a, &control.active_topology()), "B discarded");
        assert!(control.topology_grid_is_current(&a));
        assert!(!control.recompute_pending.load(Ordering::Relaxed));
        drop(listener.renderer);
    }

    /// A → B → A while B is being built ends on A, with no rebuild.
    #[test]
    fn a_hint_that_comes_back_during_the_build_ends_on_it() {
        let listener = Listener::new();
        let control = &listener.control;
        let a = control.active_topology();
        let held = hold::hold(control);
        listener.stream(hint(7));
        listener.stream(hint(5));
        drop(held);
        listener.wait_idle();
        assert!(Arc::ptr_eq(&a, &control.active_topology()));
        assert!(control.topology_grid_is_current(&a));
        assert_eq!(control.live_grid(), hint(5));
        drop(listener.renderer);
    }

    /// A rebuild that covers more than the grid (a speaker edit) and that a
    /// grid request overtakes is run again from the live state: the edit is
    /// not lost with the discarded result.
    #[test]
    fn an_overtaken_rebuild_of_more_than_the_grid_runs_again() {
        let listener = Listener::new();
        let control = &listener.control;
        let held = hold::hold(control);
        control.with_editable_layout(|layout| layout.speakers[0].name = "Edited".to_string());
        control.bump_geometry_generation();
        trigger_layout_recompute(
            control,
            &listener.socket,
            &listener.clients,
            &listener.cache,
        );
        listener.stream(hint(7));
        assert!(control.recompute_pending.load(Ordering::Relaxed));
        drop(held);
        let deadline = Instant::now() + Duration::from_secs(60);
        while control.active_topology().grid != Some(hint(7))
            || control.recomputing.load(Ordering::Relaxed)
        {
            assert!(Instant::now() < deadline, "never rebuilt on B");
            std::thread::sleep(Duration::from_millis(5));
        }
        let topology = control.active_topology();
        assert_eq!(topology.speaker_layout.speakers[0].name, "Edited");
        assert!(control.topology_grid_is_current(&topology));
        drop(listener.renderer);
    }

    /// A backend that cannot be built for the edited layout.
    struct RefusingFactory;

    impl renderer::plugin::PluginFactory for RefusingFactory {
        fn id(&self) -> &'static str {
            "refusing"
        }
    }

    impl renderer::backend_registry::BackendFactory for RefusingFactory {
        fn build_plan(
            &self,
            _ctx: &renderer::backend_registry::BackendBuildCtx<'_>,
        ) -> Option<renderer::backend_registry::BackendBuildPlan> {
            Some(renderer::backend_registry::BackendBuildPlan::Dynamic(
                renderer::backend_registry::DynamicBackendPlan::new("refusing", || {
                    Err(anyhow::anyhow!("no hull for this layout"))
                }),
            ))
        }
    }

    /// A layout edit the selected backend cannot be built for: the recompute
    /// fails with the backend's reason on the recompute-error broadcast, and
    /// the engine keeps rendering the previous topology.
    #[test]
    fn a_failed_layout_recompute_reports_its_reason_and_keeps_the_topology() {
        let control = fixture_control();
        control.register_backend(Box::new(RefusingFactory));
        let before = control.active_topology();
        control.live.write().backend_id = "refusing".to_string();
        control.with_editable_layout(|layout| layout.speakers[0].name = "Edited".to_string());
        control.bump_geometry_generation();

        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").unwrap());
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let clients = Arc::new(OscClientRegistry::new(Duration::from_secs(5)));
        clients.insert_permanent(&crate::osc::peer::Peer::Udp(client.local_addr().unwrap()));

        trigger_layout_recompute(
            &control,
            &socket,
            &clients,
            &Arc::new(GaintableCache::new()),
        );

        let error = next_non_empty_string(&client, osc_contract::STATE_SPEAKERS_RECOMPUTE_ERROR)
            .expect("a recompute error is broadcast");
        assert!(
            error.contains("no hull for this layout") && !error.contains("panicked"),
            "{error}"
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while control.recomputing.load(Ordering::Relaxed) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!control.recomputing.load(Ordering::Relaxed));
        assert!(
            Arc::ptr_eq(&before, &control.active_topology()),
            "the previous topology stays active"
        );
    }

    /// Studio grows the layout past the 24 speakers the renderer's gain sets
    /// used to hold (#745), whichever backend is selected: the recompute
    /// builds the wider topology and publishes it.
    #[test]
    fn a_layout_wider_than_24_speakers_recomputes_with_every_backend() {
        for backend in [
            "vbap",
            "volumetric",
            "barycenter",
            "experimental_distance",
            "hybrid",
        ] {
            a_wider_layout_recomputes(backend);
        }
    }

    fn a_wider_layout_recomputes(backend: &str) {
        let control = fixture_control();
        let before = control.active_topology();
        control.live.write().backend_id = backend.to_string();
        let n: usize = 40;
        let layout = SpeakerLayout::from_speakers(
            (0..n)
                .map(|i| {
                    Speaker::new(
                        format!("S{i}"),
                        -180.0 + 360.0 * (i / 2) as f32 / n.div_ceil(2) as f32,
                        if i % 2 == 0 { 0.0 } else { 40.0 },
                    )
                })
                .collect(),
        )
        .expect("ring layout");
        control.with_editable_layout(|l| *l = layout);
        control.bump_geometry_generation();

        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").unwrap());
        let clients = Arc::new(OscClientRegistry::new(Duration::from_secs(5)));
        trigger_layout_recompute(
            &control,
            &socket,
            &clients,
            &Arc::new(GaintableCache::new()),
        );

        let deadline = Instant::now() + Duration::from_secs(60);
        while Arc::ptr_eq(&before, &control.active_topology())
            || control.recomputing.load(Ordering::Relaxed)
        {
            assert!(
                Instant::now() < deadline,
                "{backend}: the wider topology was never published"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let topology = control.active_topology();
        assert_eq!(topology.num_speakers, n, "{backend}");
        assert_eq!(topology.backend.backend_id(), backend);
        assert_eq!(topology.backend.speaker_count(), n, "{backend}");
    }
}
