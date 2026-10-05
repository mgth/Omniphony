use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use renderer::backend_files;
use renderer::backend_params::ParamValue;
use renderer::live_params::RendererControl;
use rosc::{OscMessage, OscType};
use runtime_control::HostControlHandler;
use runtime_control::command::{RuntimeCommand, parse_process_command};
use runtime_control::command_table::{self, Command};
use runtime_control::context::RuntimeControlContext;
use runtime_control::osc::{
    BroadcastUpdate, BroadcastValue, ControlEffects, Notify, apply_simple_osc_control,
    gaintable_chunk_broadcasts,
};
use runtime_control::osc::{
    parse_bool_arg, parse_f32_arg, parse_nonnegative_u32_arg, parse_positive_u32_arg,
};
use runtime_control::osc_contract;

use super::client_registry::OscClientRegistry;
use super::export::{build_live_state, export_current_layout, save_live_config};
use super::gaintable::GaintableCache;
use super::recompute::trigger_layout_recompute;
use super::transport::{
    broadcast_blob, broadcast_fff, broadcast_float, broadcast_int, broadcast_string,
    resolve_register_addr, send_diag_state, send_message_to_client, send_metering_state,
    send_update_to_client,
};

#[derive(Default)]
pub(crate) struct RealtimeSeqState {
    pub master_gain: Option<i32>,
    pub speaker_gain: HashMap<usize, i32>,
}

/// What an engine handler of [`ENGINE_COMMANDS`] reaches.
pub(crate) struct Dispatch<'a> {
    msg: &'a OscMessage,
    /// The sender, for the handlers that reply point-to-point.
    src: SocketAddr,
    control: &'a Arc<RendererControl>,
    host: Option<&'a Arc<dyn HostControlHandler>>,
    realtime_seq: &'a mut RealtimeSeqState,
    socket: &'a Arc<UdpSocket>,
    clients: &'a Arc<OscClientRegistry>,
    gaintable_cache: &'a Arc<GaintableCache>,
}

type EngineHandler = fn(&mut Dispatch);

/// The engine's control addresses (see `runtime_control::command_table`):
/// the mpv overlay, the per-client subscriptions, the realtime gains, the
/// bridge and input paths, the profiles, the backend files and the layout
/// export. Tried after the live options, before the process commands, the
/// core's table and the host.
pub(crate) static ENGINE_COMMANDS: &[Command<EngineHandler>] = &[
    Command::exact(osc_contract::CONTROL_OVERLAY_ENABLED, overlay_enabled),
    Command::exact(osc_contract::CONTROL_OVERLAY_LABELS, overlay_labels),
    Command::exact(osc_contract::CONTROL_OVERLAY_OBJECTS, overlay_objects),
    Command::exact(
        osc_contract::CONTROL_OVERLAY_HEATMAP_ENABLED,
        overlay_heatmap_enabled,
    ),
    Command::exact(
        osc_contract::CONTROL_OVERLAY_HEATMAP_CUSTOM_STOPS,
        overlay_heatmap_custom_stops,
    ),
    Command::exact(
        osc_contract::CONTROL_OVERLAY_HEATMAP_BANDS,
        overlay_heatmap_bands,
    ),
    Command::exact(
        osc_contract::CONTROL_OVERLAY_HEATMAP_COLORMAP,
        overlay_heatmap_colormap,
    ),
    Command::exact(osc_contract::CONTROL_OVERLAY_TRAILS, overlay_trails),
    Command::exact(osc_contract::CONTROL_OVERLAY_TAG, overlay_tag),
    Command::exact(
        osc_contract::CONTROL_DEBUG_SPEAKER_GAINTABLE_SUBSCRIBE,
        debug_speaker_gaintable_subscribe,
    ),
    Command::exact(
        osc_contract::CONTROL_DEBUG_SPEAKER_GAINTABLE_UNSUBSCRIBE,
        debug_speaker_gaintable_unsubscribe,
    ),
    Command::exact(
        osc_contract::CONTROL_DEBUG_SPEAKER_GAINTABLE_NACK,
        debug_speaker_gaintable_nack,
    ),
    Command::exact(osc_contract::CONTROL_METERING, metering),
    Command::exact(osc_contract::CONTROL_DIAG_ENABLED, diag_enabled),
    Command::exact(osc_contract::CONTROL_INPUT_REFRESH, input_refresh),
    Command::exact(
        osc_contract::CONTROL_REALTIME_MASTER_GAIN,
        realtime_master_gain,
    ),
    Command::exact(
        osc_contract::CONTROL_REALTIME_SPEAKER_GAIN,
        realtime_speaker_gain,
    ),
    Command::exact(osc_contract::CONTROL_RENDER_BRIDGE_PATH, render_bridge_path),
    Command::exact(osc_contract::CONTROL_RENDER_INPUT_PIPE, render_input_pipe),
    Command::exact(osc_contract::CONTROL_BACKEND_FILE_GET, backend_file_get),
    Command::exact(osc_contract::CONTROL_BACKEND_FILE_LIST, backend_file_list),
    Command::exact(osc_contract::CONTROL_BACKEND_FILE_PUT, backend_file_put),
    Command::exact(osc_contract::CONTROL_LAYOUT_EXPORT, layout_export),
    Command::any(
        &[
            osc_contract::CONTROL_PROFILE_SWITCH,
            osc_contract::CONTROL_PROFILE_CREATE,
            osc_contract::CONTROL_PROFILE_DELETE,
            osc_contract::CONTROL_PROFILE_RENAME,
        ],
        profile,
    ),
];

/// Named config profiles: switch / create / delete / rename
/// (docs/config-profiles.md). Handled before the process commands so the
/// profile addresses never fall through to the host.
fn profile(d: &mut Dispatch) {
    super::profiles::handle_profile_message(
        d.msg,
        d.control,
        d.host,
        d.socket,
        d.clients,
        d.gaintable_cache,
    );
}

pub(crate) fn handle_control_message(
    msg: &OscMessage,
    src: SocketAddr,
    control: &Arc<RendererControl>,
    host: Option<&Arc<dyn HostControlHandler>>,
    realtime_seq: &mut RealtimeSeqState,
    socket: &Arc<UdpSocket>,
    clients: &Arc<OscClientRegistry>,
    gaintable_cache: &Arc<GaintableCache>,
) {
    let addr = msg.addr.as_str();
    let runtime_ctx = RuntimeControlContext::new(Arc::clone(control));

    // Pure live-state writes (declared live options, monitoring cadences,
    // generator/phantom params, placement): validated and applied by the core;
    // notified and persisted here.
    if let Some(effects) = runtime_control::live_control::apply_live_control(
        msg,
        &runtime_ctx,
        host.map(|h| h.as_ref()),
    ) {
        apply_control_effects(effects, control, host, socket, clients, gaintable_cache);
        return;
    }

    if let Some(run) = command_table::find(ENGINE_COMMANDS, addr) {
        run(&mut Dispatch {
            msg,
            src,
            control,
            host,
            realtime_seq,
            socket,
            clients,
            gaintable_cache,
        });
        return;
    }

    if let Some(command) = parse_process_command(msg) {
        match command {
            RuntimeCommand::SaveConfig => save_live_config(control, host, socket, clients),
            RuntimeCommand::ReloadConfig => {
                log::info!("OSC reload_config requested");
                if sys::shutdown::is_restartable() {
                    sys::shutdown::request_restart_from_config();
                } else {
                    super::profiles::reload_config_in_place(
                        control,
                        host,
                        socket,
                        clients,
                        gaintable_cache,
                    );
                }
            }
            RuntimeCommand::Restart => {
                if sys::shutdown::is_restartable() {
                    log::info!("OSC restart requested (live state kept)");
                    sys::shutdown::request_restart_keeping_live();
                } else {
                    // An embedded host owns the pipeline's lifecycle: a new
                    // bridge takes effect when it restarts the renderer.
                    log::info!("OSC restart ignored (embedded host)");
                }
            }
            RuntimeCommand::Quit => {
                log::info!("OSC quit requested");
                sys::shutdown::request_shutdown();
            }
            RuntimeCommand::YieldPort => {
                if sys::shutdown::is_yieldable() {
                    // Instead of shutting down, allocate a dynamic resume port,
                    // tell the requester (mpv) about it, and enter standby: the
                    // render loop releases the RX port + audio output and idles
                    // until a `resume` arrives on that port (mpv exit).
                    match crate::osc::prepare_standby_resume_port() {
                        Some(resume_port) => {
                            let reply = OscMessage {
                                addr: crate::osc::STANDBY_RESUME_REPLY.to_string(),
                                args: vec![OscType::Int(resume_port as i32)],
                            };
                            if let Ok(bytes) =
                                rosc::encoder::encode(&rosc::OscPacket::Message(reply))
                            {
                                let _ = socket.send_to(&bytes, src);
                            }
                            log::info!(
                                "OSC yield_port: entering standby; resume port {resume_port}"
                            );
                            sys::shutdown::request_standby();
                        }
                        None => {
                            log::warn!(
                                "OSC yield_port: could not allocate a resume port; shutting down"
                            );
                            sys::shutdown::request_shutdown();
                        }
                    }
                } else {
                    log::info!("OSC yield_port ignored (instance not yieldable)");
                }
            }
            RuntimeCommand::Resume => {
                log::info!("OSC resume requested");
                sys::shutdown::request_resume();
            }
            RuntimeCommand::SetLogLevel(requested) => {
                live_log::set_runtime_level(requested);
                broadcast_string(
                    socket,
                    clients,
                    osc_contract::STATE_LOG_LEVEL,
                    live_log::current_runtime_level_name(),
                );
                log::info!(
                    "OSC: log_level → {}",
                    live_log::current_runtime_level_name()
                );
            }
        }
        return;
    }

    if let Some(effects) = apply_simple_osc_control(msg, &runtime_ctx) {
        apply_control_effects(effects, control, host, socket, clients, gaintable_cache);
        return;
    }

    // Core didn't handle it — delegate to the host (audio output/input).
    if let Some(effects) = host.and_then(|h| h.handle(addr, msg)) {
        apply_control_effects(effects, control, host, socket, clients, gaintable_cache);
        return;
    }
}

/// The one notification path for a control write that changed config-backed
/// live state: mark the config dirty, light every client's Save button
/// (`/state/config/saved = 0`), then publish the new value to every client the
/// way `notify` says (see [`Notify`]). Registry options, the core handlers'
/// `ControlEffects`, the host handler's, and the engine-side writes below all
/// land here, so no write can reach one client and leave the others stale.
fn notify_changed(
    control: &Arc<RendererControl>,
    host: Option<&Arc<dyn HostControlHandler>>,
    socket: &Arc<UdpSocket>,
    clients: &Arc<OscClientRegistry>,
    notify: Notify,
) {
    control.mark_dirty();
    broadcast_int(socket, clients, osc_contract::STATE_CONFIG_SAVED, 0);
    publish_changed(control, host, socket, clients, notify);
}

/// Publish a changed live value to every client the way `notify` says. The
/// tail of [`notify_changed`], and the whole of the announcement for a change
/// no Save is for (view or transient state), which leaves the config clean.
fn publish_changed(
    control: &Arc<RendererControl>,
    host: Option<&Arc<dyn HostControlHandler>>,
    socket: &Arc<UdpSocket>,
    clients: &Arc<OscClientRegistry>,
    notify: Notify,
) {
    match notify {
        Notify::Snapshot => build_live_state(control, host).broadcast(socket, clients),
        // Picked up by the OSC loop's live-state generation poll.
        Notify::CoalescedSnapshot => control.bump_live_state(),
        Notify::DirtyOnly => {}
    }
}

fn apply_control_effects(
    effects: ControlEffects,
    control: &Arc<RendererControl>,
    host: Option<&Arc<dyn HostControlHandler>>,
    socket: &Arc<UdpSocket>,
    clients: &Arc<OscClientRegistry>,
    gaintable_cache: &Arc<GaintableCache>,
) {
    // Persist before notifying, so a client reacting to the notification by
    // reading the config finds the change already there.
    runtime_control::persist::persist_ops(control, &effects.persist);
    if effects.mark_dirty {
        notify_changed(control, host, socket, clients, effects.notify);
    } else if effects.publish_only {
        publish_changed(control, host, socket, clients, effects.notify);
    }
    for update in effects.broadcasts {
        match update.value {
            BroadcastValue::Int(value) => broadcast_int(socket, clients, &update.addr, value),
            BroadcastValue::Float(value) => broadcast_float(socket, clients, &update.addr, value),
            BroadcastValue::Fff(a, b, c) => broadcast_fff(socket, clients, &update.addr, a, b, c),
            BroadcastValue::String(value) => {
                broadcast_string(socket, clients, &update.addr, &value)
            }
            BroadcastValue::Blob(bytes) => broadcast_blob(socket, clients, &update.addr, &bytes),
        }
    }
    if let Some(message) = effects.log_message {
        log::info!("{message}");
    }
    if effects.trigger_layout_recompute {
        // A change that affects the backend geometry (triangulation / decorator
        // metrics) bumps the geometry generation so the upcoming recompute rebuilds
        // the gain models. Evaluation-only changes (mode / grid resolution) leave it
        // untouched, letting the recompute reuse the existing models and rebuild
        // only the evaluation wrapper. Bump BEFORE triggering so the plan captures
        // the new generation.
        if !effects.evaluation_only {
            control.bump_geometry_generation();
        }
        trigger_layout_recompute(control, socket, clients, gaintable_cache);
    }
}

/// Max bytes for an editable backend file carried in one OSC datagram. Scripts
/// are tiny, so a save/load stays a single all-or-nothing message (no chunk
/// reassembly), well under the UDP datagram limit.
const BACKEND_FILE_MAX_BYTES: usize = 60_000;

fn str_arg(msg: &OscMessage, index: usize) -> Option<String> {
    match msg.args.get(index) {
        Some(OscType::String(s)) => Some(s.clone()),
        _ => None,
    }
}

/// The directory holding the YAML config, used to root the managed file store.
fn backend_file_config_dir(control: &RendererControl) -> Option<PathBuf> {
    control
        .config_path()
        .and_then(|path| path.parent().map(|dir| dir.to_path_buf()))
}

/// Optional opaque request tag. Older clients omit it; malformed tags never
/// grow a reply unboundedly and are treated as absent.
fn backend_file_request_id(msg: &OscMessage, index: usize) -> Option<String> {
    str_arg(msg, index).filter(|id| !id.is_empty() && id.len() <= 64)
}
fn backend_file_reply(mut args: Vec<OscType>, request_id: Option<&str>) -> Vec<OscType> {
    if let Some(id) = request_id {
        args.push(OscType::String(id.to_owned()));
    }
    args
}

fn send_backend_file_error(
    socket: &UdpSocket,
    src: SocketAddr,
    backend_id: &str,
    key: &str,
    request_id: Option<&str>,
    message: impl Into<String>,
) {
    let message = message.into();
    log::warn!("backend file {backend_id}.{key}: {message}");
    send_message_to_client(
        socket,
        src,
        osc_contract::STATE_BACKEND_FILE_ERROR,
        backend_file_reply(
            vec![
                OscType::String(backend_id.to_string()),
                OscType::String(key.to_string()),
                OscType::String(message),
            ],
            request_id,
        ),
    );
}

/// `get [backend_id, key, name?]` → read a file's content on the renderer and
/// reply STATE_BACKEND_FILE_CONTENT to the requester. With an explicit `name` the
/// editor previews any managed-store file; without it, the param's current handle
/// is read. An absolute handle is only honoured for a loopback caller (see
/// [`backend_files::resolve`]).
fn handle_backend_file_get(
    msg: &OscMessage,
    src: SocketAddr,
    control: &Arc<RendererControl>,
    socket: &UdpSocket,
) {
    let (Some(backend_id), Some(key)) = (str_arg(msg, 0), str_arg(msg, 1)) else {
        return;
    };
    let request_id = backend_file_request_id(msg, 3);
    let request_id = request_id.as_deref();
    let handle = match str_arg(msg, 2) {
        Some(name) if !name.trim().is_empty() => name,
        _ => control
            .with_plugin_params(|params| {
                params
                    .get(renderer::plugin::PluginKind::Backend, &backend_id, &key)
                    .and_then(|value| value.as_str().map(str::to_string))
            })
            .unwrap_or_default(),
    };
    let config_dir = backend_file_config_dir(control);
    let allow_absolute = src.ip().is_loopback();
    let Some(path) =
        backend_files::resolve(config_dir.as_deref(), &backend_id, &handle, allow_absolute)
    else {
        send_backend_file_error(
            socket,
            src,
            &backend_id,
            &key,
            request_id,
            "no file selected",
        );
        return;
    };
    match std::fs::read_to_string(&path) {
        Ok(content) => send_message_to_client(
            socket,
            src,
            osc_contract::STATE_BACKEND_FILE_CONTENT,
            backend_file_reply(
                vec![
                    OscType::String(backend_id),
                    OscType::String(key),
                    OscType::String(handle),
                    OscType::String(content),
                ],
                request_id,
            ),
        ),
        Err(e) => send_backend_file_error(
            socket,
            src,
            &backend_id,
            &key,
            request_id,
            format!("read failed: {e}"),
        ),
    }
}

/// `list [backend_id]` → reply STATE_BACKEND_FILE_LIST with the managed store's
/// file names as a JSON array, so the editor can offer them when the renderer is
/// remote (no native Browse).
fn handle_backend_file_list(
    msg: &OscMessage,
    src: SocketAddr,
    control: &Arc<RendererControl>,
    socket: &UdpSocket,
) {
    let Some(backend_id) = str_arg(msg, 0) else {
        return;
    };
    let config_dir = backend_file_config_dir(control);
    let names = backend_files::list(config_dir.as_deref(), &backend_id);
    let json = serde_json::to_string(&names).unwrap_or_else(|_| "[]".to_string());
    send_message_to_client(
        socket,
        src,
        osc_contract::STATE_BACKEND_FILE_LIST,
        vec![OscType::String(backend_id), OscType::String(json)],
    );
}

/// `put [backend_id, key, name, content]` → write the content into the managed
/// store (or, for a loopback caller, an absolute path), persist the handle, and
/// rebuild the backend. Replies STATE_BACKEND_FILE_CONTENT as a save ack; build
/// errors surface through the usual recompute-error banner.
fn handle_backend_file_put(
    msg: &OscMessage,
    src: SocketAddr,
    control: &Arc<RendererControl>,
    host: Option<&Arc<dyn HostControlHandler>>,
    socket: &Arc<UdpSocket>,
    clients: &Arc<OscClientRegistry>,
    gaintable_cache: &Arc<GaintableCache>,
) {
    let (Some(backend_id), Some(key), Some(name)) =
        (str_arg(msg, 0), str_arg(msg, 1), str_arg(msg, 2))
    else {
        return;
    };
    let request_id = backend_file_request_id(msg, 4);
    let request_id = request_id.as_deref();
    let content = str_arg(msg, 3).unwrap_or_default();
    if content.len() > BACKEND_FILE_MAX_BYTES {
        send_backend_file_error(
            socket,
            src,
            &backend_id,
            &key,
            request_id,
            format!(
                "file too large ({} bytes, max {BACKEND_FILE_MAX_BYTES})",
                content.len()
            ),
        );
        return;
    }
    let config_dir = backend_file_config_dir(control);
    let allow_absolute = src.ip().is_loopback();
    let Some(path) =
        backend_files::resolve(config_dir.as_deref(), &backend_id, &name, allow_absolute)
    else {
        send_backend_file_error(
            socket,
            src,
            &backend_id,
            &key,
            request_id,
            "invalid file name",
        );
        return;
    };
    // The handle we persist must resolve back to `path` at build time (which
    // always allows absolute paths): keep an allowed absolute name as-is,
    // otherwise the safe store basename.
    let stored_handle = if allow_absolute && Path::new(name.trim()).is_absolute() {
        name.trim().to_string()
    } else {
        match backend_files::sanitize_name(&name) {
            Some(basename) => basename,
            None => {
                send_backend_file_error(
                    socket,
                    src,
                    &backend_id,
                    &key,
                    request_id,
                    "invalid file name",
                );
                return;
            }
        }
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            send_backend_file_error(
                socket,
                src,
                &backend_id,
                &key,
                request_id,
                format!("cannot create store dir: {e}"),
            );
            return;
        }
    }
    if let Err(e) = std::fs::write(&path, content.as_bytes()) {
        send_backend_file_error(
            socket,
            src,
            &backend_id,
            &key,
            request_id,
            format!("write failed: {e}"),
        );
        return;
    }
    control.set_backend_param(&backend_id, &key, ParamValue::Text(stored_handle.clone()));
    // Ack the save back to the editor.
    send_message_to_client(
        socket,
        src,
        osc_contract::STATE_BACKEND_FILE_CONTENT,
        backend_file_reply(
            vec![
                OscType::String(backend_id.clone()),
                OscType::String(key.clone()),
                OscType::String(stored_handle),
                OscType::String(content),
            ],
            request_id,
        ),
    );
    // Republish state and rebuild the backend with the new content; a bad script
    // surfaces via the recompute-error path like any other build failure.
    apply_control_effects(
        ControlEffects {
            mark_dirty: true,
            trigger_layout_recompute: true,
            log_message: Some(format!("OSC: backend file {backend_id}.{key} saved")),
            ..Default::default()
        },
        control,
        host,
        socket,
        clients,
        gaintable_cache,
    );
}

/// Reply to a gain-table subscribe: push the full chunked table if the client's
/// cached `have_version` is stale (or absent), ack `uptodate` if it already has
/// the current version, or `unavailable` if the active backend has no table.
fn push_gaintable_subscribe(
    socket: &UdpSocket,
    clients: &OscClientRegistry,
    gaintable_cache: &GaintableCache,
    ctx: &RuntimeControlContext,
    client: SocketAddr,
    speaker: i64,
    have_version: Option<u32>,
) {
    match gaintable_cache.bytes_for_target(ctx, speaker) {
        Some((version, bytes)) => {
            if have_version == Some(version) {
                send_update_to_client(
                    socket,
                    client,
                    &BroadcastUpdate {
                        addr: osc_contract::STATE_DEBUG_SPEAKER_GAINTABLE_UPTODATE.to_string(),
                        value: BroadcastValue::Int(version as i32),
                    },
                );
            } else {
                for update in gaintable_chunk_broadcasts(&bytes, None) {
                    send_update_to_client(socket, client, &update);
                }
                clients.set_gaintable_version(client, speaker, version);
            }
        }
        None => send_update_to_client(
            socket,
            client,
            &BroadcastUpdate {
                addr: osc_contract::STATE_DEBUG_SPEAKER_GAINTABLE_UNAVAILABLE.to_string(),
                value: BroadcastValue::String(
                    "{\"reason\":\"no precomputed gain table for the active backend\"}".to_string(),
                ),
            },
        ),
    }
}

#[cfg(test)]
mod command_table_tests {
    use super::*;

    #[test]
    fn the_engine_table_is_declared_in_the_contract_and_claimed_once() {
        let core = runtime_control::command_table::core_addresses;
        let found = runtime_control::command_table::problems(ENGINE_COMMANDS, &[&core]);
        assert!(found.is_empty(), "{found:#?}");
    }
}

#[cfg(test)]
mod backend_file_request_tests {
    use super::*;
    #[test]
    fn request_ids_are_optional_bounded_and_echoed_on_error() {
        let msg = OscMessage {
            addr: String::new(),
            args: vec![OscType::String("id-a".into())],
        };
        assert_eq!(backend_file_request_id(&msg, 0).as_deref(), Some("id-a"));
        assert!(backend_file_request_id(&msg, 1).is_none());
        let too_long = OscMessage {
            addr: String::new(),
            args: vec![OscType::String("x".repeat(65))],
        };
        assert!(backend_file_request_id(&too_long, 0).is_none());
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        for id in [None, Some("id-a")] {
            send_backend_file_error(
                &socket,
                receiver.local_addr().unwrap(),
                "script",
                "file",
                id,
                "test failure",
            );
            let mut bytes = [0; 1024];
            let count = receiver.recv(&mut bytes).unwrap();
            let (_, rosc::OscPacket::Message(reply)) =
                rosc::decoder::decode_udp(&bytes[..count]).unwrap()
            else {
                panic!("expected message");
            };
            assert_eq!(reply.addr, osc_contract::STATE_BACKEND_FILE_ERROR);
            assert_eq!(reply.args.len(), if id.is_some() { 4 } else { 3 });
            if let Some(id) = id {
                assert_eq!(reply.args[3], OscType::String(id.into()));
            }
        }
        let content = vec![
            OscType::String("script".into()),
            OscType::String("file".into()),
            OscType::String("file.lua".into()),
            OscType::String("return 1".into()),
        ];
        assert_eq!(backend_file_reply(content.clone(), None), content);
        assert_eq!(
            backend_file_reply(content, Some("id-b"))[4],
            OscType::String("id-b".into())
        );
    }
}

#[cfg(test)]
mod notify_tests {
    use super::*;
    use renderer::live_params::{LiveEvaluationMode, PreferredEvaluationMode};
    use renderer::spatial_renderer::{RendererSpec, SpatialRenderer};
    use renderer::spatial_vbap::{DistanceModel, VbapTableMode};
    use renderer::speaker_layout::SpeakerLayout;
    use std::time::Duration;

    /// A real `RendererControl` on 7.1.4 with a trivial cartesian grid (the
    /// live-options conformance fixture).
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

    /// The engine socket, a registry with two clients (the one that writes
    /// and a bystander), and the bystander's socket.
    struct Wire {
        engine: Arc<UdpSocket>,
        clients: Arc<OscClientRegistry>,
        writer: SocketAddr,
        bystander: UdpSocket,
        gaintable_cache: Arc<GaintableCache>,
    }

    fn wire() -> Wire {
        let engine = Arc::new(UdpSocket::bind("127.0.0.1:0").unwrap());
        let writer = UdpSocket::bind("127.0.0.1:0").unwrap();
        let bystander = UdpSocket::bind("127.0.0.1:0").unwrap();
        bystander
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let clients = Arc::new(OscClientRegistry::new(Duration::from_secs(60)));
        clients.register(writer.local_addr().unwrap());
        clients.register(bystander.local_addr().unwrap());
        Wire {
            engine,
            clients,
            writer: writer.local_addr().unwrap(),
            bystander,
            gaintable_cache: Arc::new(GaintableCache::new()),
        }
    }

    fn send(wire: &Wire, control: &Arc<RendererControl>, addr: &str, args: Vec<OscType>) {
        handle_control_message(
            &OscMessage {
                addr: addr.to_string(),
                args,
            },
            wire.writer,
            control,
            None,
            &mut RealtimeSeqState::default(),
            &wire.engine,
            &wire.clients,
            &wire.gaintable_cache,
        );
    }

    /// A maximum-size `backend/file/put`, sent through the control listener's
    /// real UDP socket, is received whole, written and acknowledged: the
    /// datagram is well over the 4 KiB the listener used to read.
    #[test]
    fn a_maximum_size_backend_file_put_crosses_the_socket() {
        use crate::osc::test_support::{SERIAL, listening_sender};
        // The listener registers in the process-wide port registry and its
        // drop consumes the resume target, like the yield tests.
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("orender-osc-big-put-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let control = fixture_control();
        control.set_config_path(dir.join("config.yaml"));
        let (sender, port) = listening_sender(&control);

        let content = "x".repeat(BACKEND_FILE_MAX_BYTES);
        let put = rosc::encoder::encode(&rosc::OscPacket::Message(OscMessage {
            addr: osc_contract::CONTROL_BACKEND_FILE_PUT.to_string(),
            args: vec![
                OscType::String("test".into()),
                OscType::String("script".into()),
                OscType::String("big.lua".into()),
                OscType::String(content.clone()),
            ],
        }))
        .unwrap();
        assert!(put.len() > BACKEND_FILE_MAX_BYTES);
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        // A client has the same send limit to lift as the engine.
        crate::osc::transport::ensure_send_buffer(&client);
        client.send_to(&put, ("127.0.0.1", port)).unwrap();

        let ack = awaited(&client, osc_contract::STATE_BACKEND_FILE_CONTENT)
            .expect("the put is acknowledged");
        assert_eq!(ack.args.get(3), Some(&OscType::String(content.clone())));
        let path = backend_files::resolve(Some(&dir), "test", "big.lua", false).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), content);

        drop(sender);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The first message on `addr` the socket receives. A reply produced on
    /// the listener thread arrives when that thread gets to it, so this waits
    /// for it rather than for the socket to go quiet like [`received`].
    fn awaited(socket: &UdpSocket, addr: &str) -> Option<OscMessage> {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut buf = vec![0u8; 70_000];
        socket
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        while std::time::Instant::now() < deadline {
            let Ok(len) = socket.recv(&mut buf) else {
                continue;
            };
            if let Ok((_, rosc::OscPacket::Message(msg))) = rosc::decoder::decode_udp(&buf[..len])
                && msg.addr == addr
            {
                return Some(msg);
            }
        }
        None
    }

    /// Every message the bystander receives until the socket goes quiet.
    fn received(socket: &UdpSocket) -> Vec<OscMessage> {
        fn flatten(packet: rosc::OscPacket, out: &mut Vec<OscMessage>) {
            match packet {
                rosc::OscPacket::Message(msg) => out.push(msg),
                rosc::OscPacket::Bundle(bundle) => {
                    for inner in bundle.content {
                        flatten(inner, out);
                    }
                }
            }
        }
        let mut out = Vec::new();
        let mut buf = vec![0u8; 70_000];
        socket
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        while let Ok(len) = socket.recv(&mut buf) {
            let (_, packet) = rosc::decoder::decode_udp(&buf[..len]).expect("valid OSC");
            flatten(packet, &mut out);
        }
        out
    }

    fn state_json(messages: &[OscMessage], addr: &str) -> Option<serde_json::Value> {
        messages.iter().rev().find(|m| m.addr == addr).map(|m| {
            let Some(OscType::String(json)) = m.args.first() else {
                panic!("{addr} carries no JSON");
            };
            serde_json::from_str(json).expect("valid JSON")
        })
    }

    fn saw_dirty(messages: &[OscMessage]) -> bool {
        messages
            .iter()
            .any(|m| m.addr == osc_contract::STATE_CONFIG_SAVED && m.args == [OscType::Int(0)])
    }

    #[test]
    fn a_generator_param_write_reaches_the_other_clients() {
        let control = fixture_control();
        control.live.write().options.object_generator_id = "pad".to_string();
        let wire = wire();
        let generation = control.live_state_generation();
        send(
            &wire,
            &control,
            osc_contract::CONTROL_OBJECT_GENERATOR_PARAM,
            vec![OscType::String("strength".into()), OscType::Float(0.25)],
        );
        // The Save button lights on every client right away …
        assert!(saw_dirty(&received(&wire.bystander)));
        // … and the value is queued for the OSC loop's next live-state
        // bundle, which is what the loop broadcasts on a generation change.
        assert_ne!(control.live_state_generation(), generation);
        build_live_state(&control, None).broadcast(&wire.engine, &wire.clients);
        let renderer = state_json(&received(&wire.bystander), osc_contract::STATE_RENDERER)
            .expect("bundle carries /state/renderer");
        assert_eq!(
            renderer["objectGeneratorParamValuesById"]["pad"]["strength"],
            0.25
        );
    }

    #[test]
    fn a_monitoring_rate_write_broadcasts_the_bundle_to_the_other_clients() {
        let control = fixture_control();
        let wire = wire();
        send(
            &wire,
            &control,
            osc_contract::CONTROL_METERING_RATE_HZ,
            vec![OscType::Float(12.0)],
        );
        let messages = received(&wire.bystander);
        // A cadence is view state: it never lights the Save button.
        assert!(!saw_dirty(&messages));
        assert!(
            !control
                .config_dirty
                .load(std::sync::atomic::Ordering::Relaxed)
        );
        let monitoring = state_json(&messages, osc_contract::STATE_MONITORING)
            .expect("the bundle went out with the write");
        assert_eq!(monitoring["meterRateHz"], 12.0);
    }

    #[test]
    fn a_registry_option_write_waits_for_save_and_reaches_the_other_clients() {
        let control = fixture_control();
        let wire = wire();
        let dir = std::env::temp_dir().join(format!(
            "orender-dispatch-option-persist-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yaml");
        std::fs::write(&path, "render:\n  some_future_key: 42\n").unwrap();
        control.set_config_path(path.clone());

        // The legacy dedicated address is an alias of `/control/option`.
        send(
            &wire,
            &control,
            osc_contract::CONTROL_SURROUND_PLACEMENT,
            vec![OscType::String("back".into())],
        );
        let messages = received(&wire.bystander);
        assert!(saw_dirty(&messages));
        let renderer = state_json(&messages, osc_contract::STATE_RENDERER)
            .expect("the bundle went out with the write");
        assert_eq!(renderer["options"]["surround_placement"], "back");
        // An option changes what is heard: it waits for the Save button.
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written, "render:\n  some_future_key: 42\n");

        // The same value again changes nothing, so it lights nothing.
        control.mark_clean();
        send(
            &wire,
            &control,
            osc_contract::CONTROL_SURROUND_PLACEMENT,
            vec![OscType::String("back".into())],
        );
        assert!(!saw_dirty(&received(&wire.bystander)));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn both_master_gain_addresses_share_one_validation() {
        let control = fixture_control();
        let wire = wire();
        control.live.write().master_gain = 0.5;
        send(
            &wire,
            &control,
            osc_contract::CONTROL_REALTIME_MASTER_GAIN,
            vec![OscType::Float(f32::NAN), OscType::Int(1)],
        );
        send(
            &wire,
            &control,
            osc_contract::CONTROL_GAIN,
            vec![OscType::Float(-1.0)],
        );
        assert_eq!(control.live.read().master_gain, 0.5);
        assert!(!saw_dirty(&received(&wire.bystander)));

        let generation = control.live_state_generation();
        send(
            &wire,
            &control,
            osc_contract::CONTROL_REALTIME_MASTER_GAIN,
            vec![OscType::Float(0.75), OscType::Int(2)],
        );
        assert_eq!(control.live.read().master_gain, 0.75);
        let messages = received(&wire.bystander);
        assert!(saw_dirty(&messages));
        assert!(
            messages
                .iter()
                .any(|m| m.addr == osc_contract::STATE_REALTIME_MASTER_GAIN)
        );
        assert_ne!(control.live_state_generation(), generation);
    }

    /// Datagrams nested thousands of levels deep, in bundles and in arrays,
    /// are dropped and the listener goes on answering. Decoded, either one
    /// overflows the listener thread's stack, which aborts the whole process;
    /// only the larger receive buffer lets a datagram hold that many levels.
    #[test]
    fn a_deeply_nested_datagram_does_not_take_the_listener_down() {
        use crate::osc::test_support::{SERIAL, listening_sender};
        use osc_contract::nesting::{nested_arrays, nested_bundles};
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let control = fixture_control();
        let (sender, port) = listening_sender(&control);

        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        crate::osc::transport::ensure_send_buffer(&client);
        for nested in [nested_bundles(3_000), nested_arrays(30_000)] {
            assert_eq!(nested.len(), 60_008);
            client.send_to(&nested, ("127.0.0.1", port)).unwrap();
        }

        let heartbeat = rosc::encoder::encode(&rosc::OscPacket::Message(OscMessage {
            addr: osc_contract::HEARTBEAT.to_string(),
            args: vec![],
        }))
        .unwrap();
        client.send_to(&heartbeat, ("127.0.0.1", port)).unwrap();
        assert!(
            awaited(&client, osc_contract::HEARTBEAT_UNKNOWN).is_some(),
            "the listener still answers"
        );

        drop(sender);
    }
}

/// mpv overlay configuration. The overlay itself is generated in-process by
/// the `overlay` module and pulled over FFI; Studio only configures it here
/// (it no longer transports overlay frames). These are view state
/// (docs/persistence-policy.md): the enabled, labels and trails switches
/// are written to `overlay-prefs.conf` as they change, the rest is
/// transient, and none of them ever marks the config dirty.
fn overlay_enabled(d: &mut Dispatch) {
    let msg = d.msg;
    let enabled = match parse_bool_arg(msg.args.first()) {
        Some(v) => v,
        None => return,
    };
    crate::overlay::set_enabled(enabled);
}

fn overlay_labels(d: &mut Dispatch) {
    let msg = d.msg;
    let enabled = match parse_bool_arg(msg.args.first()) {
        Some(v) => v,
        None => return,
    };
    crate::overlay::set_labels_enabled(enabled);
}

fn overlay_objects(d: &mut Dispatch) {
    let msg = d.msg;
    let visible = match parse_bool_arg(msg.args.first()) {
        Some(v) => v,
        None => return,
    };
    crate::overlay::set_objects_visible(visible);
}

fn overlay_heatmap_enabled(d: &mut Dispatch) {
    let msg = d.msg;
    let enabled = match parse_bool_arg(msg.args.first()) {
        Some(v) => v,
        None => return,
    };
    crate::overlay::set_heatmap_enabled(enabled);
}

fn overlay_heatmap_custom_stops(d: &mut Dispatch) {
    let msg = d.msg;
    // Flat [pos, r, g, b, …] floats → grouped stops for the custom gradient.
    let flat: Vec<f32> = msg
        .args
        .iter()
        .filter_map(|a| match a {
            OscType::Float(f) => Some(*f),
            OscType::Int(i) => Some(*i as f32),
            _ => None,
        })
        .collect();
    let stops: Vec<[f32; 4]> = flat
        .chunks_exact(4)
        .map(|c| [c[0], c[1], c[2], c[3]])
        .collect();
    crate::overlay::set_heatmap_custom_stops(stops);
}

fn overlay_heatmap_bands(d: &mut Dispatch) {
    let msg = d.msg;
    let count = match parse_positive_u32_arg(msg.args.first()) {
        Some(v) => v as usize,
        None => return,
    };
    crate::overlay::set_heatmap_bands(count);
}

fn overlay_heatmap_colormap(d: &mut Dispatch) {
    let msg = d.msg;
    let idx = match parse_nonnegative_u32_arg(msg.args.first()) {
        Some(v) => v as usize,
        None => return,
    };
    crate::overlay::set_heatmap_colormap(idx);
}

fn overlay_trails(d: &mut Dispatch) {
    let msg = d.msg;
    // Args mirror Studio's former wire fields: enabled, ttl_ms, mode, teleport.
    let enabled = match parse_bool_arg(msg.args.first()) {
        Some(v) => v,
        None => return,
    };
    let ttl_ms = parse_nonnegative_u32_arg(msg.args.get(1)).unwrap_or(7000);
    let diffuse = matches!(
        msg.args.get(2),
        Some(OscType::String(s)) if s.eq_ignore_ascii_case("diffuse")
    );
    let teleport = parse_f32_arg(msg.args.get(3)).unwrap_or(0.0) as f64;
    crate::overlay::set_trail_config(enabled, ttl_ms, diffuse, teleport);
}

fn overlay_tag(d: &mut Dispatch) {
    let msg = d.msg;
    // [id, tag]: tag "A"/"B" sets an override colour, anything else clears it.
    let Some(id) = msg.args.first().and_then(|a| match a {
        OscType::Int(v) if *v >= 0 => Some(*v as u32),
        OscType::Float(v) if *v >= 0.0 => Some(*v as u32),
        OscType::String(s) => s.parse::<u32>().ok(),
        _ => None,
    }) else {
        return;
    };
    let tag = match msg.args.get(1) {
        Some(OscType::String(s)) => s
            .chars()
            .next()
            .filter(|c| matches!(c, 'A' | 'a' | 'B' | 'b')),
        _ => None,
    };
    crate::overlay::set_tag(id, tag);
}

/// Speaker gain-table pub/sub. A client subscribes for one speaker (the heatmap
/// shows one), carrying the version it has cached; the renderer pushes that
/// speaker's per-band field only if the version differs, and keeps pushing on
/// every topology rebuild while subscribed (see `recompute.rs`). Args:
/// [Int have_version, Int speaker_index].
fn debug_speaker_gaintable_subscribe(d: &mut Dispatch) {
    let runtime_ctx = RuntimeControlContext::new(Arc::clone(d.control));
    let msg = d.msg;
    let src = d.src;
    let socket = d.socket;
    let clients = d.clients;
    let gaintable_cache = d.gaintable_cache;
    let have_version = parse_nonnegative_u32_arg(msg.args.first());
    // A negative index selects an all-speaker derived field, not an error:
    // the global heatmaps subscribe through the same path as a per-speaker
    // one. Known sentinels pass through; an unknown negative falls back to
    // the energy field so an older client never gets a field it can't read.
    let speaker = match msg.args.get(1) {
        Some(OscType::Int(i)) if *i >= 0 => *i as i64,
        Some(OscType::Int(i))
            if matches!(
                *i as i64,
                renderer::band_gaintable::GAIN_DISCONTINUITY_INDEX
                    | renderer::band_gaintable::CENTROID_JUMP_INDEX
            ) =>
        {
            *i as i64
        }
        Some(OscType::Int(_)) => renderer::band_gaintable::GLOBAL_ENERGY_INDEX,
        _ => 0,
    };
    let client = resolve_register_addr(src, &[]);
    // Ensure the client exists in the registry (refreshes liveness) so the
    // subscribe flag sticks and the 5 s heartbeat keeps it alive.
    clients.register(client);
    clients.set_gaintable(client, true);
    // Additive: a client showing several heatmaps subscribes once per
    // target, and each must keep receiving pushes.
    clients.add_gaintable_target(client, speaker);
    push_gaintable_subscribe(
        socket,
        clients,
        gaintable_cache,
        &runtime_ctx,
        client,
        speaker,
        have_version,
    );
}

fn debug_speaker_gaintable_unsubscribe(d: &mut Dispatch) {
    let src = d.src;
    let clients = d.clients;
    let client = resolve_register_addr(src, &[]);
    clients.set_gaintable(client, false);
    // Drop the targets too: the next subscribe declares what it wants, and
    // keeping them would push fields nobody is displaying any more.
    clients.clear_gaintable_targets(client);
}

fn debug_speaker_gaintable_nack(d: &mut Dispatch) {
    let runtime_ctx = RuntimeControlContext::new(Arc::clone(d.control));
    let msg = d.msg;
    let src = d.src;
    let socket = d.socket;
    let clients = d.clients;
    let gaintable_cache = d.gaintable_cache;
    // Args: Int version, Int missing_index… — resend just the lost chunks for
    // the client's subscribed speaker.
    let mut ints = msg.args.iter().filter_map(|a| match a {
        OscType::Int(i) if *i >= 0 => Some(*i as u32),
        _ => None,
    });
    if let Some(version) = ints.next() {
        let missing: Vec<u32> = ints.collect();
        if !missing.is_empty() {
            let client = resolve_register_addr(src, &[]);
            // Resolve the target from the version the client is missing
            // chunks for, so a NACK is answered with the right field even
            // when several transfers are in flight.
            let target = clients
                .gaintable_target_for_version(client, version)
                .unwrap_or(0);
            if let Some((_v, bytes)) = gaintable_cache.bytes_for_target(&runtime_ctx, target) {
                for update in gaintable_chunk_broadcasts(&bytes, Some((version, missing))) {
                    send_update_to_client(socket, client, &update);
                }
            }
        }
    }
}

fn metering(d: &mut Dispatch) {
    let msg = d.msg;
    let src = d.src;
    let socket = d.socket;
    let clients = d.clients;
    let enabled = match parse_bool_arg(msg.args.first()) {
        Some(v) => v,
        None => return,
    };
    let client = resolve_register_addr(src, &[]);
    if clients.set_metering(client, enabled) {
        send_metering_state(socket, client, enabled);
    }
}

fn diag_enabled(d: &mut Dispatch) {
    let msg = d.msg;
    let src = d.src;
    let socket = d.socket;
    let clients = d.clients;
    let enabled = match parse_bool_arg(msg.args.first()) {
        Some(v) => v,
        None => return,
    };
    let client = resolve_register_addr(src, &[]);
    if clients.set_diag(client, enabled) {
        send_diag_state(socket, client, enabled);
    }
}

fn input_refresh(d: &mut Dispatch) {
    let control = d.control;
    let host = d.host;
    let socket = d.socket;
    let clients = d.clients;
    build_live_state(control, host).broadcast(socket, clients);
    log::info!("OSC: input state refresh requested");
}

fn realtime_master_gain(d: &mut Dispatch) {
    let msg = d.msg;
    let control = d.control;
    let host = d.host;
    let realtime_seq = &mut *d.realtime_seq;
    let socket = d.socket;
    let clients = d.clients;
    let Some(value) = msg.args.first().and_then(|arg| match arg {
        OscType::Float(v) => Some(*v),
        OscType::Int(v) => Some(*v as f32),
        _ => None,
    }) else {
        return;
    };
    let Some(seq) = msg.args.get(1).and_then(|arg| match arg {
        OscType::Int(v) => Some(*v),
        _ => None,
    }) else {
        return;
    };
    if realtime_seq.master_gain.is_some_and(|last| seq < last) {
        return;
    }
    // Same setter as `/control/gain`: one validation, one field.
    let Some(value) = runtime_control::osc::set_master_gain(control, value) else {
        return;
    };
    realtime_seq.master_gain = Some(seq);
    // The realtime echo below is for the sender's own sequencing; the
    // other clients read the gain from the live-state bundle, coalesced
    // because a gain slider drag is a burst of writes.
    notify_changed(control, host, socket, clients, Notify::CoalescedSnapshot);
    if let Ok(bytes) = rosc::encoder::encode(&rosc::OscPacket::Message(rosc::OscMessage {
        addr: osc_contract::STATE_REALTIME_MASTER_GAIN.to_string(),
        args: vec![OscType::Float(value), OscType::Int(seq)],
    })) {
        super::transport::send_raw(socket, clients, &bytes);
    }
}

fn realtime_speaker_gain(d: &mut Dispatch) {
    let msg = d.msg;
    let control = d.control;
    let host = d.host;
    let realtime_seq = &mut *d.realtime_seq;
    let socket = d.socket;
    let clients = d.clients;
    let Some(idx) = msg.args.first().and_then(|arg| match arg {
        OscType::Int(v) if *v >= 0 => Some(*v as usize),
        OscType::Float(v) if *v >= 0.0 => Some(*v as usize),
        _ => None,
    }) else {
        return;
    };
    let Some(value) = msg.args.get(1).and_then(|arg| match arg {
        OscType::Float(v) => Some(*v),
        OscType::Int(v) => Some(*v as f32),
        _ => None,
    }) else {
        return;
    };
    let Some(seq) = msg.args.get(2).and_then(|arg| match arg {
        OscType::Int(v) => Some(*v),
        _ => None,
    }) else {
        return;
    };
    if realtime_seq
        .speaker_gain
        .get(&idx)
        .copied()
        .is_some_and(|last| seq < last)
    {
        return;
    }
    if !value.is_finite() || value < 0.0 {
        log::warn!("OSC speaker gain: rejected value {value}");
        return;
    }
    realtime_seq.speaker_gain.insert(idx, seq);
    control.live.write().speakers.entry(idx).or_default().gain = value;
    control.mark_speaker_params_dirty();
    notify_changed(control, host, socket, clients, Notify::CoalescedSnapshot);
    if let Ok(bytes) = rosc::encoder::encode(&rosc::OscPacket::Message(rosc::OscMessage {
        addr: osc_contract::STATE_REALTIME_SPEAKER_GAIN.to_string(),
        args: vec![
            OscType::Int(idx as i32),
            OscType::Float(value),
            OscType::Int(seq),
        ],
    })) {
        super::transport::send_raw(socket, clients, &bytes);
    }
}

fn render_bridge_path(d: &mut Dispatch) {
    let msg = d.msg;
    let control = d.control;
    let host = d.host;
    let socket = d.socket;
    let clients = d.clients;
    let value = match msg.args.first() {
        Some(OscType::String(s)) => s.trim(),
        _ => return,
    };
    let next = if value.is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(value))
    };
    if control.bridge_path() != next {
        control.set_bridge_path(next.clone());
        notify_changed(control, host, socket, clients, Notify::DirtyOnly);
        let state_value = next
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        broadcast_string(
            socket,
            clients,
            osc_contract::STATE_RENDER_BRIDGE_PATH,
            &state_value,
        );
        log::info!(
            "OSC: render.bridge_path → {}",
            next.as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "<auto>".to_string())
        );
    }
}

fn render_input_pipe(d: &mut Dispatch) {
    let msg = d.msg;
    let control = d.control;
    let host = d.host;
    let socket = d.socket;
    let clients = d.clients;
    let value = match msg.args.first() {
        Some(OscType::String(s)) => s.trim(),
        _ => return,
    };
    let next = if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    };
    if control.input_path() != next {
        control.set_input_path(next.clone());
        notify_changed(control, host, socket, clients, Notify::DirtyOnly);
        broadcast_string(
            socket,
            clients,
            osc_contract::STATE_INPUT_PIPE,
            &next.clone().unwrap_or_default(),
        );
        log::info!(
            "OSC: render.input_pipe → {}",
            next.as_deref().unwrap_or("<default>")
        );
    }
}

/// Editable backend files (e.g. the scriptable backend's `.lua`). The content
/// is owned by the renderer, so the editor reads/writes it here over OSC; these
/// reply point-to-point to the requester (`src`) rather than broadcasting.
fn backend_file_get(d: &mut Dispatch) {
    let msg = d.msg;
    let src = d.src;
    let control = d.control;
    let socket = d.socket;
    handle_backend_file_get(msg, src, control, socket);
}

fn backend_file_list(d: &mut Dispatch) {
    let msg = d.msg;
    let src = d.src;
    let control = d.control;
    let socket = d.socket;
    handle_backend_file_list(msg, src, control, socket);
}

fn backend_file_put(d: &mut Dispatch) {
    let msg = d.msg;
    let src = d.src;
    let control = d.control;
    let host = d.host;
    let socket = d.socket;
    let clients = d.clients;
    let gaintable_cache = d.gaintable_cache;
    handle_backend_file_put(msg, src, control, host, socket, clients, gaintable_cache);
}

fn layout_export(d: &mut Dispatch) {
    let msg = d.msg;
    let control = d.control;
    let requested_name = match msg.args.first() {
        Some(OscType::String(s)) if !s.trim().is_empty() => Some(s.trim()),
        _ => None,
    };
    export_current_layout(control, requested_name);
}
