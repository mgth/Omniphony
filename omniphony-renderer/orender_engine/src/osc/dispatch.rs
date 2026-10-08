use std::collections::HashMap;
use std::net::UdpSocket;
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
use super::export::{broadcast_live_state, export_current_layout, save_live_config};
use super::gaintable::GaintableCache;
use super::peer::Peer;
use super::recompute::trigger_layout_recompute;
use super::transport::{
    broadcast_blob, broadcast_fff, broadcast_float, broadcast_int, broadcast_string,
    resolve_register_addr, send_diag_state, send_message_to_client, send_metering_state,
    send_update_to_client,
};

/// What became of a control message, for its sender
/// ([`osc_contract::STATE_CONTROL_ERROR`]).
#[derive(Debug, PartialEq)]
pub(crate) enum ControlOutcome {
    /// A handler took it. It may still have changed nothing (a value already
    /// in force, a stale realtime sequence number).
    Handled,
    /// Its handler refused it, for this reason.
    Invalid(String),
    /// No handler took it.
    Unhandled,
    /// Refused because of who sent it, for this reason
    /// ([`osc_contract::CONTROL_ERROR_NOT_ALLOWED`]).
    NotAllowed(String),
}

impl ControlOutcome {
    fn invalid(reason: impl Into<String>) -> Self {
        Self::Invalid(reason.into())
    }
}

#[derive(Default)]
pub(crate) struct RealtimeSeqState {
    pub master_gain: Option<i32>,
    pub speaker_gain: HashMap<usize, i32>,
}

/// What an engine handler of [`ENGINE_COMMANDS`] reaches.
pub(crate) struct Dispatch<'a> {
    msg: &'a OscMessage,
    /// The sender, for the handlers that reply point-to-point.
    src: &'a Peer,
    control: &'a Arc<RendererControl>,
    host: Option<&'a Arc<dyn HostControlHandler>>,
    realtime_seq: &'a mut RealtimeSeqState,
    socket: &'a Arc<UdpSocket>,
    clients: &'a Arc<OscClientRegistry>,
    gaintable_cache: &'a Arc<GaintableCache>,
}

type EngineHandler = fn(&mut Dispatch) -> ControlOutcome;

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
    Command::exact(
        osc_contract::CONTROL_RENDER_BRIDGE_PATHS,
        render_bridge_paths,
    ),
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
/// (docs/config-profiles.md). In the engine's table, so the profile
/// addresses never fall through to the host.
fn profile(d: &mut Dispatch) -> ControlOutcome {
    super::profiles::handle_profile_message(
        d.msg,
        d.control,
        d.host,
        d.socket,
        d.clients,
        d.gaintable_cache,
    );
    ControlOutcome::Handled
}

pub(crate) fn handle_control_message(
    msg: &OscMessage,
    src: &Peer,
    control: &Arc<RendererControl>,
    host: Option<&Arc<dyn HostControlHandler>>,
    realtime_seq: &mut RealtimeSeqState,
    socket: &Arc<UdpSocket>,
    clients: &Arc<OscClientRegistry>,
    gaintable_cache: &Arc<GaintableCache>,
) -> ControlOutcome {
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
        return apply_control_effects(effects, control, host, socket, clients, gaintable_cache);
    }

    if let Some(run) = command_table::find(ENGINE_COMMANDS, addr) {
        return run(&mut Dispatch {
            msg,
            src,
            control,
            host,
            realtime_seq,
            socket,
            clients,
            gaintable_cache,
        });
    }

    if let Some(command) = parse_process_command(msg) {
        // The OSC/UDP socket listens on the network (head tracking from a
        // phone, a remote Studio); stopping the engine or taking its port is
        // for a client on this machine only (#680).
        if matches!(
            command,
            RuntimeCommand::Quit | RuntimeCommand::YieldPort | RuntimeCommand::Resume
        ) && !src.is_loopback()
        {
            return ControlOutcome::NotAllowed(
                "only a client on this machine may stop the engine or take its port".into(),
            );
        }
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
                                let _ = src.send(socket, &bytes);
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
        return ControlOutcome::Handled;
    }

    if let Some(effects) = apply_simple_osc_control(msg, &runtime_ctx) {
        return apply_control_effects(effects, control, host, socket, clients, gaintable_cache);
    }

    // Core didn't handle it — delegate to the host (audio output/input).
    if let Some(effects) = host.and_then(|h| h.handle(addr, msg)) {
        return apply_control_effects(effects, control, host, socket, clients, gaintable_cache);
    }

    ControlOutcome::Unhandled
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
        Notify::Snapshot => broadcast_live_state(control, host, socket, clients),
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
) -> ControlOutcome {
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
    if effects.grid_request {
        super::recompute::request_grid(control, socket, clients, gaintable_cache);
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
    match effects.rejected {
        Some(reason) => ControlOutcome::Invalid(reason),
        None => ControlOutcome::Handled,
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
    src: &Peer,
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
    src: &Peer,
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
    let allow_absolute = src.is_loopback();
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
    src: &Peer,
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
    src: &Peer,
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
    let allow_absolute = src.is_loopback();
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
    client: &Peer,
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
                for update in
                    gaintable_chunk_broadcasts(&bytes, None, client.gaintable_chunk_bytes())
                {
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
                &Peer::Udp(receiver.local_addr().unwrap()),
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

/// Process-lifecycle controls from another machine (#680, step 4).
#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use renderer::test_support::fixture_control;

    fn outcome(addr: &str, from: &Peer) -> ControlOutcome {
        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").unwrap());
        handle_control_message(
            &OscMessage {
                addr: addr.to_string(),
                args: Vec::new(),
            },
            from,
            &fixture_control(),
            None,
            &mut RealtimeSeqState::default(),
            &socket,
            &Arc::new(OscClientRegistry::new(std::time::Duration::from_secs(60))),
            &Arc::new(GaintableCache::new()),
        )
    }

    /// Refused from another machine, before anything happens: the request
    /// flags (process-wide) are left as they were. The local path is the
    /// engine's existing behaviour and would stop the test process.
    #[test]
    fn lifecycle_controls_from_another_machine_are_not_allowed() {
        let remote = Peer::Udp("192.0.2.10:9000".parse().unwrap());
        let standby_before = sys::shutdown::is_standby_requested();
        for addr in [
            osc_contract::CONTROL_QUIT,
            osc_contract::CONTROL_YIELD_PORT,
            osc_contract::CONTROL_RESUME,
        ] {
            assert!(
                matches!(outcome(addr, &remote), ControlOutcome::NotAllowed(_)),
                "{addr} from another machine"
            );
        }
        assert_eq!(sys::shutdown::is_standby_requested(), standby_before);
    }

    /// Everything else stays open to the network: a remote control reaches
    /// its handler, which here refuses the missing value, not the sender.
    #[test]
    fn other_controls_from_another_machine_are_taken() {
        let remote = Peer::Udp("192.0.2.10:9000".parse().unwrap());
        assert!(matches!(
            outcome(osc_contract::CONTROL_GAIN, &remote),
            ControlOutcome::Invalid(_)
        ));
    }
}

#[cfg(test)]
mod notify_tests {
    use super::*;
    use renderer::test_support::fixture_control;
    use std::time::Duration;

    /// The engine socket, a registry with two clients (the one that writes
    /// and a bystander), and the bystander's socket.
    struct Wire {
        engine: Arc<UdpSocket>,
        clients: Arc<OscClientRegistry>,
        writer: std::net::SocketAddr,
        bystander: UdpSocket,
        gaintable_cache: Arc<GaintableCache>,
    }

    fn wire() -> Wire {
        let engine = UdpSocket::bind("127.0.0.1:0").unwrap();
        // As OscSender's socket: without it macOS refuses the live-state
        // bundle (EMSGSIZE above net.inet.udp.maxdgram) and nothing arrives.
        crate::osc::transport::ensure_send_buffer(&engine);
        let engine = Arc::new(engine);
        let writer = UdpSocket::bind("127.0.0.1:0").unwrap();
        let bystander = UdpSocket::bind("127.0.0.1:0").unwrap();
        bystander
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let clients = Arc::new(OscClientRegistry::new(Duration::from_secs(60)));
        clients.register(&Peer::Udp(writer.local_addr().unwrap()));
        clients.register(&Peer::Udp(bystander.local_addr().unwrap()));
        Wire {
            engine,
            clients,
            writer: writer.local_addr().unwrap(),
            bystander,
            gaintable_cache: Arc::new(GaintableCache::new()),
        }
    }

    fn send(
        wire: &Wire,
        control: &Arc<RendererControl>,
        addr: &str,
        args: Vec<OscType>,
    ) -> ControlOutcome {
        handle_control_message(
            &OscMessage {
                addr: addr.to_string(),
                args,
            },
            &Peer::Udp(wire.writer),
            control,
            None,
            &mut RealtimeSeqState::default(),
            &wire.engine,
            &wire.clients,
            &wire.gaintable_cache,
        )
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
        broadcast_live_state(&control, None, &wire.engine, &wire.clients);
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

    /// Argument lists: a few a control plausibly accepts, so that changes are
    /// applied, and the ways a sender gets them wrong — none at all, too few,
    /// the wrong type, a non-finite number, out of range, and many.
    fn argument_lists() -> Vec<Vec<OscType>> {
        vec![
            vec![OscType::Int(0)],
            vec![OscType::Float(0.5)],
            vec![OscType::String("1".into())],
            vec![],
            vec![OscType::Int(1)],
            vec![OscType::Int(-1)],
            vec![OscType::Int(i32::MAX)],
            vec![OscType::Float(f32::NAN)],
            vec![OscType::Float(f32::INFINITY)],
            vec![OscType::Double(-1e300)],
            vec![OscType::String(String::new())],
            vec![OscType::String("x".into())],
            vec![OscType::Nil],
            vec![OscType::Blob(vec![0; 3])],
            (0..32).map(OscType::Int).collect(),
            (0..32).map(|i| OscType::String(i.to_string())).collect(),
        ]
    }

    /// Process lifecycle commands act on the whole test process (shutdown,
    /// restart and standby flags) and read no argument but the log level's.
    const PROCESS_COMMANDS: &[&str] = &[
        osc_contract::CONTROL_RELOAD_CONFIG,
        osc_contract::CONTROL_RESTART,
        osc_contract::CONTROL_QUIT,
        osc_contract::CONTROL_YIELD_PORT,
        osc_contract::CONTROL_RESUME,
    ];

    /// Every control address a sender can reach: the contract's, minus the
    /// process lifecycle commands, plus each prefixed family's real fields —
    /// the registry's prefixed rows and the hand-wired ones — and a few
    /// malformed instances of each family. The contract lists the families by
    /// prefix only, so without the real fields their handlers go unswept.
    fn control_addresses() -> Vec<String> {
        let mut addresses: Vec<String> = osc_contract::ALL_CONTROL
            .iter()
            .filter(|address| !PROCESS_COMMANDS.contains(address))
            .map(|address| address.to_string())
            .collect();
        let registry_fields = renderer::options::LIVE_OPTIONS
            .iter()
            .filter_map(|spec| match spec.legacy_control_addr {
                renderer::options::LegacyAddr::Prefixed { prefix, tail } => {
                    Some(format!("{prefix}{tail}"))
                }
                _ => None,
            });
        addresses.extend(registry_fields);
        // Hand-wired fields, outside the registry.
        for (prefix, tail) in [
            (osc_contract::CONTROL_HYBRID_PREFIX, "curve"),
            (osc_contract::CONTROL_HYBRID_PREFIX, "external_backend"),
            (osc_contract::CONTROL_HYBRID_PREFIX, "internal_backend"),
            (osc_contract::CONTROL_DISTANCE_DIFFUSE_PREFIX, "threshold"),
            (osc_contract::CONTROL_OBJECT_PREFIX, "1/mute"),
            (osc_contract::CONTROL_OBJECT_PREFIX, "0/mute"),
            (osc_contract::CONTROL_OBJECT_PREFIX, "-1/mute"),
            (osc_contract::CONTROL_OBJECT_PREFIX, "4294967296/mute"),
        ] {
            addresses.push(format!("{prefix}{tail}"));
        }
        // And what a sender gets wrong under each family.
        for prefix in [
            osc_contract::CONTROL_OBJECT_PREFIX,
            osc_contract::CONTROL_DISTANCE_DIFFUSE_PREFIX,
            osc_contract::CONTROL_HYBRID_PREFIX,
            osc_contract::CONTROL_RENDER_EVALUATION_CARTESIAN_PREFIX,
            osc_contract::CONTROL_RENDER_EVALUATION_POLAR_PREFIX,
        ] {
            for suffix in ["", "1", "x", "x/y/z", "x/mute"] {
                addresses.push(format!("{prefix}{suffix}"));
            }
        }
        addresses.sort();
        addresses.dedup();
        addresses
    }

    /// The address list reaches the prefixed families' real fields, not only
    /// the contract's exact addresses: one per registry row with a prefixed
    /// address, and the hand-wired hybrid curve.
    #[test]
    fn the_sweeps_reach_every_prefixed_field() {
        let addresses = control_addresses();
        let prefixed = renderer::options::LIVE_OPTIONS
            .iter()
            .filter(|spec| {
                matches!(
                    spec.legacy_control_addr,
                    renderer::options::LegacyAddr::Prefixed { .. }
                )
            })
            .count();
        assert!(
            prefixed > 5,
            "the registry declares prefixed fields: {prefixed}"
        );
        let in_families = |prefix: &str| {
            addresses
                .iter()
                .filter(|a| a.starts_with(prefix) && a.len() > prefix.len() + 1)
                .count()
        };
        assert!(in_families(osc_contract::CONTROL_HYBRID_PREFIX) > 3);
        assert!(addresses.contains(&format!("{}curve", osc_contract::CONTROL_HYBRID_PREFIX)));
        assert!(in_families(osc_contract::CONTROL_RENDER_EVALUATION_CARTESIAN_PREFIX) >= 4);
    }

    /// A control datagram is untrusted: whatever its arguments, the handler
    /// ignores or applies it and never panics, which would end the control
    /// listener thread and leave the engine deaf to every client.
    #[test]
    fn no_control_address_panics_on_malformed_arguments() {
        // The sweep drives the process-global overlay and port registry too.
        let _overlay = crate::overlay::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _serial = crate::osc::test_support::SERIAL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile_dir("control-sweep");
        let control = fixture_control();
        // Save, profiles and backend files write beside the config: here.
        control.set_config_path(dir.join("config.yaml"));
        let wire = wire();

        let addresses = control_addresses();

        let mut sent = 0;
        for address in &addresses {
            for args in argument_lists() {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    send(&wire, &control, address, args.clone())
                }));
                assert!(
                    outcome.is_ok(),
                    "{address} with {args:?} panicked the control handler"
                );
                sent += 1;
            }
        }
        assert!(sent > 1000, "too few cases to mean anything: {sent}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What may write `config.yaml` without the Save button
    /// (docs/persistence-policy.md): the Save itself, the profile operations,
    /// and the view-state exceptions — head-tracker recenter and calibration,
    /// and the monitoring cadences.
    const WRITES_CONFIG: &[&str] = &[
        osc_contract::CONTROL_SAVE_CONFIG,
        osc_contract::CONTROL_PROFILE_SWITCH,
        osc_contract::CONTROL_PROFILE_CREATE,
        osc_contract::CONTROL_PROFILE_DELETE,
        osc_contract::CONTROL_PROFILE_RENAME,
        osc_contract::CONTROL_HEAD_RECENTER,
        osc_contract::CONTROL_HEAD_CALIBRATE,
        osc_contract::CONTROL_METERING_RATE_HZ,
        osc_contract::CONTROL_DIAG_RATE_HZ,
    ];

    /// The persistence policy, checked where it is enforced: no control
    /// message but the ones it names changes `config.yaml`, whatever its
    /// arguments. A render or engine change marks the config dirty and waits
    /// for the Save. The source tripwire (runtime_control's
    /// persistence_policy.rs) lists the writers; this watches the file.
    #[test]
    fn only_save_profiles_and_view_state_change_the_config_file() {
        let _overlay = crate::overlay::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _serial = crate::osc::test_support::SERIAL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile_dir("persistence-net");
        let config = dir.join("config.yaml");
        let original = "render:\n  ramp_mode: sample\n";
        std::fs::write(&config, original).unwrap();
        let control = fixture_control();
        control.set_config_path(config.clone());
        let wire = wire();

        let addresses = control_addresses();
        let mut writers_seen = Vec::new();
        let mut violations = Vec::new();
        for address in &addresses {
            let address = address.as_str();
            for args in argument_lists() {
                let _ = send(&wire, &control, address, args.clone());
                let now = std::fs::read_to_string(&config).unwrap_or_default();
                if now != original {
                    if WRITES_CONFIG.contains(&address) {
                        writers_seen.push(address);
                    } else {
                        violations.push(format!("{address} {args:?}"));
                    }
                    std::fs::write(&config, original).unwrap();
                }
                // A profile operation may move the control to another file.
                control.set_config_path(config.clone());
            }
        }
        assert!(
            violations.is_empty(),
            "these controls wrote config.yaml without the Save button; mark the config \
             dirty instead, or name the write in docs/persistence-policy.md's \
             exceptions with its reason:\n{}",
            violations.join("\n")
        );
        // The net must see a write when one happens, or it proves nothing.
        assert!(
            writers_seen.contains(&osc_contract::CONTROL_SAVE_CONFIG),
            "Save never changed the file: the check is not watching the right file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn tempfile_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("orender-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// What a control came to, as its sender is told: taken, refused with a
    /// reason, or taken by nobody.
    #[test]
    fn every_control_comes_back_with_an_outcome() {
        let control = fixture_control();
        let wire = wire();
        assert_eq!(
            send(&wire, &control, osc_contract::CONTROL_UNKNOWN, vec![]),
            ControlOutcome::Unhandled
        );
        assert!(matches!(
            send(
                &wire,
                &control,
                osc_contract::CONTROL_OVERLAY_ENABLED,
                vec![OscType::String("yes".into())],
            ),
            ControlOutcome::Invalid(_)
        ));
        let ControlOutcome::Invalid(reason) = send(
            &wire,
            &control,
            osc_contract::CONTROL_OPTION,
            vec![OscType::String("no_such_option".into()), OscType::Int(1)],
        ) else {
            panic!("an unknown option key is refused");
        };
        assert!(reason.contains("no_such_option"), "got: {reason}");
        assert_eq!(
            send(
                &wire,
                &control,
                osc_contract::CONTROL_OVERLAY_ENABLED,
                vec![OscType::Int(1)],
            ),
            ControlOutcome::Handled
        );
    }

    /// A grouped option write applies the pairs it can and reports the ones
    /// it dropped, rather than one silently masking the other.
    #[test]
    fn a_grouped_option_write_reports_the_pair_it_refused() {
        let control = fixture_control();
        let wire = wire();
        let ControlOutcome::Invalid(reason) = send(
            &wire,
            &control,
            osc_contract::CONTROL_OPTIONS,
            vec![
                OscType::String("ramp_mode".into()),
                OscType::String("frame".into()),
                OscType::String("ramp_mode".into()),
                OscType::String("no_such_mode".into()),
            ],
        ) else {
            panic!("the invalid pair is reported");
        };
        assert!(reason.contains("ramp_mode"), "got: {reason}");
    }

    /// Single state updates go out with the next generation, `full = 0`; a
    /// broadcast snapshot closes on the one after, `full = 1`; a snapshot sent
    /// to one client reports the current one and moves nothing.
    #[test]
    fn state_updates_and_snapshots_carry_the_generation_in_sequence() {
        let control = fixture_control();
        let wire = wire();
        let generations = |messages: &[OscMessage]| -> Vec<(i32, i32, i32, i32)> {
            messages
                .iter()
                .filter(|m| m.addr == osc_contract::STATE_GENERATION)
                .map(|m| match m.args[..] {
                    [
                        OscType::Int(g),
                        OscType::Int(full),
                        OscType::Int(part),
                        OscType::Int(parts),
                    ] => (g, full, part, parts),
                    _ => panic!("malformed generation: {:?}", m.args),
                })
                .collect()
        };
        let start = wire.clients.state_generation() as i32;
        broadcast_int(
            &wire.engine,
            &wire.clients,
            osc_contract::STATE_CONFIG_SAVED,
            0,
        );
        broadcast_string(
            &wire.engine,
            &wire.clients,
            osc_contract::STATE_LOG_LEVEL,
            "info",
        );
        broadcast_live_state(&control, None, &wire.engine, &wire.clients);
        assert_eq!(
            generations(&received(&wire.bystander)),
            [
                (start + 1, 0, 0, 1),
                (start + 2, 0, 0, 1),
                (start + 3, 1, 0, 1)
            ]
        );

        crate::osc::export::send_live_state_to(
            &control,
            None,
            &wire.engine,
            &wire.clients,
            &Peer::Udp(wire.bystander.local_addr().unwrap()),
        );
        let messages = received(&wire.bystander);
        assert_eq!(generations(&messages), [(start + 3, 1, 0, 1)]);
        assert_eq!(
            messages.last().map(|m| m.addr.as_str()),
            Some(osc_contract::STATE_SNAPSHOT_COMPLETE),
            "the snapshot still ends on its marker"
        );
    }

    /// Through the real listener: a refused control is answered to its sender
    /// with the address and a code, the heartbeat ack carries the generation,
    /// and a refresh brings the snapshot back.
    #[test]
    fn the_listener_answers_refusals_acks_with_the_generation_and_refreshes() {
        use crate::osc::test_support::{SERIAL, listening_sender};
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let control = fixture_control();
        let (sender, port) = listening_sender(&control);
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        let send_to_engine = |addr: &str, args: Vec<OscType>| {
            let bytes = rosc::encoder::encode(&rosc::OscPacket::Message(OscMessage {
                addr: addr.to_string(),
                args,
            }))
            .unwrap();
            client.send_to(&bytes, ("127.0.0.1", port)).unwrap();
        };
        let error = |code: &str| {
            let reply = awaited(&client, osc_contract::STATE_CONTROL_ERROR)
                .expect("a refused control is answered");
            assert_eq!(reply.args.get(1), Some(&OscType::String(code.to_string())));
            assert_conforms(&reply);
            reply
        };

        send_to_engine(osc_contract::CONTROL_UNKNOWN, vec![]);
        let reply = error(osc_contract::CONTROL_ERROR_UNKNOWN_ADDRESS);
        assert_eq!(
            reply.args.first(),
            Some(&OscType::String(osc_contract::CONTROL_UNKNOWN.into()))
        );
        send_to_engine(
            osc_contract::CONTROL_OVERLAY_ENABLED,
            vec![OscType::String("yes".into())],
        );
        error(osc_contract::CONTROL_ERROR_INVALID_ARGUMENTS);
        // Catalogued, but only a host with audio output takes it.
        send_to_engine(
            osc_contract::CONTROL_AUDIO_OUTPUT_DEVICE,
            vec![OscType::String("hw:0".into())],
        );
        error(osc_contract::CONTROL_ERROR_NOT_APPLIED);

        let local_port = i32::from(client.local_addr().unwrap().port());
        send_to_engine(osc_contract::REGISTER, vec![OscType::Int(local_port)]);
        send_to_engine(osc_contract::HEARTBEAT, vec![OscType::Int(local_port)]);
        let ack = awaited(&client, osc_contract::HEARTBEAT_ACK).expect("the heartbeat is acked");
        assert!(
            matches!(ack.args[..], [OscType::Int(_), OscType::Int(_)]),
            "epoch and generation: {:?}",
            ack.args
        );

        // Whatever the registration sent is read; the refresh is what follows.
        let _ = received(&client);
        send_to_engine(
            osc_contract::CONTROL_STATE_REFRESH,
            vec![OscType::Int(local_port)],
        );
        let messages = received(&client);
        assert!(
            messages
                .iter()
                .any(|m| m.addr == osc_contract::STATE_SNAPSHOT_COMPLETE),
            "the refresh brings the snapshot back"
        );
        assert!(
            !messages.iter().any(|m| m.addr == osc_contract::LOG),
            "and nothing else"
        );

        drop(sender);
    }

    /// `msg` carries the arguments the contract's shape table gives its
    /// address: the types in order, and a JSON argument that parses.
    fn assert_conforms(msg: &OscMessage) {
        use osc_contract::shapes::Arg;
        let shape = osc_contract::shapes::state(&msg.addr)
            .unwrap_or_else(|| panic!("{} has no shape in the contract", msg.addr));
        assert!(
            msg.args.len() >= shape.len(),
            "{}: {:?} is shorter than {shape:?}",
            msg.addr,
            msg.args
        );
        for (arg, kind) in msg.args.iter().zip(shape) {
            let conforms = match (kind, arg) {
                (Arg::Int, OscType::Int(_))
                | (Arg::Float, OscType::Float(_))
                | (Arg::String, OscType::String(_))
                | (Arg::Blob, OscType::Blob(_)) => true,
                (Arg::Json, OscType::String(json)) => {
                    serde_json::from_str::<serde_json::Value>(json).is_ok()
                }
                _ => false,
            };
            assert!(conforms, "{}: {arg:?} is not {kind:?}", msg.addr);
        }
    }

    /// What the engine sends is what the contract says it sends, so the
    /// shapes Studio's conformance test feeds its parser are the real ones.
    #[test]
    fn the_snapshot_and_state_updates_match_the_contract_shapes() {
        let control = fixture_control();
        let wire = wire();
        broadcast_live_state(&control, None, &wire.engine, &wire.clients);
        broadcast_int(
            &wire.engine,
            &wire.clients,
            osc_contract::STATE_CONFIG_SAVED,
            1,
        );
        let messages = received(&wire.bystander);
        assert!(
            messages
                .iter()
                .any(|m| m.addr == osc_contract::STATE_SNAPSHOT_COMPLETE)
        );
        for msg in &messages {
            // Per-object mutes are a family, not a catalogued address.
            if msg.addr.starts_with("/omniphony/state/object/") {
                continue;
            }
            assert_conforms(msg);
        }
    }

    /// The interleaving the publication lock is for: a snapshot asked for
    /// while another publication is under way is captured when its turn
    /// comes, not when it was asked for. Captured early, it would carry the
    /// state from before a change made meanwhile under a count higher than
    /// the change's own, and a client would take the older state for current.
    #[test]
    fn a_snapshot_captures_the_state_of_its_turn_under_the_lock() {
        use std::sync::mpsc;
        let control = fixture_control();
        control.live.write().master_gain = 1.0;
        let wire = wire();
        let (entered_tx, entered) = mpsc::channel();
        let (release, release_rx) = mpsc::channel::<()>();
        std::thread::scope(|scope| {
            // A publication in progress, holding the lock.
            let clients = &wire.clients;
            let held = scope.spawn(move || {
                clients.publish(|generation| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    generation.advance()
                })
            });
            entered.recv().unwrap();
            // A snapshot asked for now has to wait its turn …
            let snapshot =
                scope.spawn(|| broadcast_live_state(&control, None, &wire.engine, &wire.clients));
            std::thread::sleep(Duration::from_millis(100));
            // … so the change made while it waits is in it.
            control.live.write().master_gain = 0.5;
            release.send(()).unwrap();
            let held_generation = held.join().unwrap();
            snapshot.join().unwrap();
            assert_eq!(wire.clients.state_generation(), held_generation + 1);
        });
        let messages = received(&wire.bystander);
        let renderer = state_json(&messages, osc_contract::STATE_RENDERER).expect("a snapshot");
        assert_eq!(renderer["masterGain"], 0.5);
    }

    /// Several values captured together go out one datagram each, numbered
    /// in sequence: together, a recompute's renderer, layout and speakers
    /// could outgrow a datagram.
    #[test]
    fn values_published_together_go_out_one_numbered_datagram_each() {
        let wire = wire();
        let start = wire.clients.state_generation() as i32;
        crate::osc::transport::publish_state(&wire.engine, &wire.clients, || {
            [osc_contract::STATE_RENDERER, osc_contract::STATE_LAYOUT]
                .into_iter()
                .map(|addr| OscMessage {
                    addr: addr.to_string(),
                    args: vec![OscType::String("{}".into())],
                })
                .collect()
        });
        let mut buf = vec![0u8; 70_000];
        let mut numbered = Vec::new();
        while let Ok(len) = wire.bystander.recv(&mut buf) {
            let (_, rosc::OscPacket::Bundle(bundle)) =
                rosc::decoder::decode_udp(&buf[..len]).unwrap()
            else {
                panic!("a state update is a bundle");
            };
            let [
                rosc::OscPacket::Message(value),
                rosc::OscPacket::Message(generation),
            ] = &bundle.content[..]
            else {
                panic!("one value and its generation per datagram");
            };
            assert_conforms(generation);
            numbered.push((value.addr.clone(), generation.args[0].clone()));
            if numbered.len() == 2 {
                break;
            }
        }
        assert_eq!(
            numbered,
            [
                (
                    osc_contract::STATE_RENDERER.to_string(),
                    OscType::Int(start + 1)
                ),
                (
                    osc_contract::STATE_LAYOUT.to_string(),
                    OscType::Int(start + 2)
                ),
            ]
        );
    }
}

/// mpv overlay configuration. The overlay itself is generated in-process by
/// the `overlay` module and pulled over FFI; Studio only configures it here
/// (it no longer transports overlay frames). These are view state
/// (docs/persistence-policy.md): the enabled, labels and trails switches
/// are written to `overlay-prefs.conf` as they change, the rest is
/// transient, and none of them ever marks the config dirty.
fn overlay_enabled(d: &mut Dispatch) -> ControlOutcome {
    let msg = d.msg;
    let enabled = match parse_bool_arg(msg.args.first()) {
        Some(v) => v,
        None => return ControlOutcome::invalid("expected a boolean"),
    };
    crate::overlay::set_enabled(enabled);
    ControlOutcome::Handled
}

fn overlay_labels(d: &mut Dispatch) -> ControlOutcome {
    let msg = d.msg;
    let enabled = match parse_bool_arg(msg.args.first()) {
        Some(v) => v,
        None => return ControlOutcome::invalid("expected a boolean"),
    };
    crate::overlay::set_labels_enabled(enabled);
    ControlOutcome::Handled
}

fn overlay_objects(d: &mut Dispatch) -> ControlOutcome {
    let msg = d.msg;
    let visible = match parse_bool_arg(msg.args.first()) {
        Some(v) => v,
        None => return ControlOutcome::invalid("expected a boolean"),
    };
    crate::overlay::set_objects_visible(visible);
    ControlOutcome::Handled
}

fn overlay_heatmap_enabled(d: &mut Dispatch) -> ControlOutcome {
    let msg = d.msg;
    let enabled = match parse_bool_arg(msg.args.first()) {
        Some(v) => v,
        None => return ControlOutcome::invalid("expected a boolean"),
    };
    crate::overlay::set_heatmap_enabled(enabled);
    ControlOutcome::Handled
}

fn overlay_heatmap_custom_stops(d: &mut Dispatch) -> ControlOutcome {
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
    ControlOutcome::Handled
}

fn overlay_heatmap_bands(d: &mut Dispatch) -> ControlOutcome {
    let msg = d.msg;
    let count = match parse_positive_u32_arg(msg.args.first()) {
        Some(v) => v as usize,
        None => return ControlOutcome::invalid("expected a positive integer"),
    };
    crate::overlay::set_heatmap_bands(count);
    ControlOutcome::Handled
}

fn overlay_heatmap_colormap(d: &mut Dispatch) -> ControlOutcome {
    let msg = d.msg;
    let idx = match parse_nonnegative_u32_arg(msg.args.first()) {
        Some(v) => v as usize,
        None => return ControlOutcome::invalid("expected a non-negative integer"),
    };
    crate::overlay::set_heatmap_colormap(idx);
    ControlOutcome::Handled
}

fn overlay_trails(d: &mut Dispatch) -> ControlOutcome {
    let msg = d.msg;
    // Args mirror Studio's former wire fields: enabled, ttl_ms, mode, teleport.
    let enabled = match parse_bool_arg(msg.args.first()) {
        Some(v) => v,
        None => return ControlOutcome::invalid("expected a boolean"),
    };
    let ttl_ms = parse_nonnegative_u32_arg(msg.args.get(1)).unwrap_or(7000);
    let diffuse = matches!(
        msg.args.get(2),
        Some(OscType::String(s)) if s.eq_ignore_ascii_case("diffuse")
    );
    let teleport = parse_f32_arg(msg.args.get(3)).unwrap_or(0.0) as f64;
    crate::overlay::set_trail_config(enabled, ttl_ms, diffuse, teleport);
    ControlOutcome::Handled
}

fn overlay_tag(d: &mut Dispatch) -> ControlOutcome {
    let msg = d.msg;
    // [id, tag]: tag "A"/"B" sets an override colour, anything else clears it.
    let Some(id) = msg.args.first().and_then(|a| match a {
        OscType::Int(v) if *v >= 0 => Some(*v as u32),
        OscType::Float(v) if *v >= 0.0 => Some(*v as u32),
        OscType::String(s) => s.parse::<u32>().ok(),
        _ => None,
    }) else {
        return ControlOutcome::invalid("expected an object id");
    };
    let tag = match msg.args.get(1) {
        Some(OscType::String(s)) => s
            .chars()
            .next()
            .filter(|c| matches!(c, 'A' | 'a' | 'B' | 'b')),
        _ => None,
    };
    crate::overlay::set_tag(id, tag);
    ControlOutcome::Handled
}

/// Speaker gain-table pub/sub. A client subscribes for one speaker (the heatmap
/// shows one), carrying the version it has cached; the renderer pushes that
/// speaker's per-band field only if the version differs, and keeps pushing on
/// every topology rebuild while subscribed (see `recompute.rs`). Args:
/// [Int have_version, Int speaker_index].
fn debug_speaker_gaintable_subscribe(d: &mut Dispatch) -> ControlOutcome {
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
    let client = &resolve_register_addr(src, &[]);
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
    ControlOutcome::Handled
}

fn debug_speaker_gaintable_unsubscribe(d: &mut Dispatch) -> ControlOutcome {
    let src = d.src;
    let clients = d.clients;
    let client = &resolve_register_addr(src, &[]);
    clients.set_gaintable(client, false);
    // Drop the targets too: the next subscribe declares what it wants, and
    // keeping them would push fields nobody is displaying any more.
    clients.clear_gaintable_targets(client);
    ControlOutcome::Handled
}

fn debug_speaker_gaintable_nack(d: &mut Dispatch) -> ControlOutcome {
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
            let client = &resolve_register_addr(src, &[]);
            // Resolve the target from the version the client is missing
            // chunks for, so a NACK is answered with the right field even
            // when several transfers are in flight.
            let target = clients
                .gaintable_target_for_version(client, version)
                .unwrap_or(0);
            if let Some((_v, bytes)) = gaintable_cache.bytes_for_target(&runtime_ctx, target) {
                for update in gaintable_chunk_broadcasts(
                    &bytes,
                    Some((version, missing)),
                    client.gaintable_chunk_bytes(),
                ) {
                    send_update_to_client(socket, client, &update);
                }
            }
        }
    }
    ControlOutcome::Handled
}

fn metering(d: &mut Dispatch) -> ControlOutcome {
    let msg = d.msg;
    let src = d.src;
    let socket = d.socket;
    let clients = d.clients;
    let enabled = match parse_bool_arg(msg.args.first()) {
        Some(v) => v,
        None => return ControlOutcome::invalid("expected a boolean"),
    };
    let client = &resolve_register_addr(src, &[]);
    if clients.set_metering(client, enabled) {
        send_metering_state(socket, client, enabled);
    }
    ControlOutcome::Handled
}

fn diag_enabled(d: &mut Dispatch) -> ControlOutcome {
    let msg = d.msg;
    let src = d.src;
    let socket = d.socket;
    let clients = d.clients;
    let enabled = match parse_bool_arg(msg.args.first()) {
        Some(v) => v,
        None => return ControlOutcome::invalid("expected a boolean"),
    };
    let client = &resolve_register_addr(src, &[]);
    if clients.set_diag(client, enabled) {
        send_diag_state(socket, client, enabled);
    }
    ControlOutcome::Handled
}

fn input_refresh(d: &mut Dispatch) -> ControlOutcome {
    let control = d.control;
    let host = d.host;
    let socket = d.socket;
    let clients = d.clients;
    broadcast_live_state(control, host, socket, clients);
    log::info!("OSC: input state refresh requested");
    ControlOutcome::Handled
}

fn realtime_master_gain(d: &mut Dispatch) -> ControlOutcome {
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
        return ControlOutcome::invalid("expected a gain");
    };
    let Some(seq) = msg.args.get(1).and_then(|arg| match arg {
        OscType::Int(v) => Some(*v),
        _ => None,
    }) else {
        return ControlOutcome::invalid("expected a sequence number");
    };
    if realtime_seq.master_gain.is_some_and(|last| seq < last) {
        return ControlOutcome::Handled;
    }
    // Same setter as `/control/gain`: one validation, one field.
    let Some(value) = runtime_control::osc::set_master_gain(control, value) else {
        return ControlOutcome::invalid("the gain must be finite and non-negative");
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
    ControlOutcome::Handled
}

fn realtime_speaker_gain(d: &mut Dispatch) -> ControlOutcome {
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
        return ControlOutcome::invalid("expected a speaker index");
    };
    let Some(value) = msg.args.get(1).and_then(|arg| match arg {
        OscType::Float(v) => Some(*v),
        OscType::Int(v) => Some(*v as f32),
        _ => None,
    }) else {
        return ControlOutcome::invalid("expected a gain");
    };
    let Some(seq) = msg.args.get(2).and_then(|arg| match arg {
        OscType::Int(v) => Some(*v),
        _ => None,
    }) else {
        return ControlOutcome::invalid("expected a sequence number");
    };
    if realtime_seq
        .speaker_gain
        .get(&idx)
        .copied()
        .is_some_and(|last| seq < last)
    {
        return ControlOutcome::Handled;
    }
    if !value.is_finite() || value < 0.0 {
        log::warn!("OSC speaker gain: rejected value {value}");
        return ControlOutcome::invalid("the gain must be finite and non-negative");
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
    ControlOutcome::Handled
}

fn render_bridge_path(d: &mut Dispatch) -> ControlOutcome {
    let value = match d.msg.args.first() {
        Some(OscType::String(s)) => s.trim(),
        _ => return ControlOutcome::invalid("expected a string"),
    };
    let next = if value.is_empty() {
        Vec::new()
    } else {
        vec![std::path::PathBuf::from(value)]
    };
    set_requested_bridges(d, next)
}

fn render_bridge_paths(d: &mut Dispatch) -> ControlOutcome {
    let mut next = Vec::with_capacity(d.msg.args.len());
    for arg in &d.msg.args {
        match arg {
            OscType::String(s) if !s.trim().is_empty() => {
                next.push(std::path::PathBuf::from(s.trim()));
            }
            OscType::String(_) => {}
            _ => return ControlOutcome::invalid("expected one string per bridge path"),
        }
    }
    set_requested_bridges(d, next)
}

/// Record the bridges asked for: unsaved state, applied at the next restart
/// or config reload.
fn set_requested_bridges(d: &mut Dispatch, next: Vec<std::path::PathBuf>) -> ControlOutcome {
    let control = d.control;
    if control.bridge_paths() != next {
        control.set_bridge_paths(next.clone());
        notify_changed(control, d.host, d.socket, d.clients, Notify::DirtyOnly);
        let first = next
            .first()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        broadcast_string(
            d.socket,
            d.clients,
            osc_contract::STATE_RENDER_BRIDGE_PATH,
            &first,
        );
        broadcast_string(
            d.socket,
            d.clients,
            osc_contract::STATE_RENDER_BRIDGES,
            &runtime_control::snapshot::bridges_state_json(control),
        );
        log::info!(
            "OSC: render.bridge_path(s) → {}",
            if next.is_empty() {
                "<auto>".to_string()
            } else {
                next.iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        );
    }
    ControlOutcome::Handled
}

fn render_input_pipe(d: &mut Dispatch) -> ControlOutcome {
    let msg = d.msg;
    let control = d.control;
    let host = d.host;
    let socket = d.socket;
    let clients = d.clients;
    let value = match msg.args.first() {
        Some(OscType::String(s)) => s.trim(),
        _ => return ControlOutcome::invalid("expected a string"),
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
    ControlOutcome::Handled
}

/// Editable backend files (e.g. the scriptable backend's `.lua`). The content
/// is owned by the renderer, so the editor reads/writes it here over OSC; these
/// reply point-to-point to the requester (`src`) rather than broadcasting.
fn backend_file_get(d: &mut Dispatch) -> ControlOutcome {
    let msg = d.msg;
    let src = d.src;
    let control = d.control;
    let socket = d.socket;
    handle_backend_file_get(msg, src, control, socket);
    ControlOutcome::Handled
}

fn backend_file_list(d: &mut Dispatch) -> ControlOutcome {
    let msg = d.msg;
    let src = d.src;
    let control = d.control;
    let socket = d.socket;
    handle_backend_file_list(msg, src, control, socket);
    ControlOutcome::Handled
}

fn backend_file_put(d: &mut Dispatch) -> ControlOutcome {
    let msg = d.msg;
    let src = d.src;
    let control = d.control;
    let host = d.host;
    let socket = d.socket;
    let clients = d.clients;
    let gaintable_cache = d.gaintable_cache;
    handle_backend_file_put(msg, src, control, host, socket, clients, gaintable_cache);
    ControlOutcome::Handled
}

fn layout_export(d: &mut Dispatch) -> ControlOutcome {
    let msg = d.msg;
    let control = d.control;
    let requested_name = match msg.args.first() {
        Some(OscType::String(s)) if !s.trim().is_empty() => Some(s.trim()),
        _ => None,
    };
    export_current_layout(control, requested_name);
    ControlOutcome::Handled
}
