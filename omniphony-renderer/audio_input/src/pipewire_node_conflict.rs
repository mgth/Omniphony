//! One-shot check for another PipeWire client that already publishes a sink
//! carrying the live-input node name.
//!
//! WirePlumber resolves a default-sink *name* to one node. When two renderers
//! publish the same name — an `orender` from another workflow, an orphan left
//! over from a previous session — the resolution can land on the older node,
//! whose output may never cycle: every client targeting that name then hangs
//! on a driver-less sink while the healthy renderer reports nothing wrong.
//!
//! The scan runs once on the backend's main loop, before the node is
//! published, so it never touches the realtime path. It warns and reports; it
//! does not rename the node.

use crate::InputControl;
use anyhow::{Result, anyhow};
use pipewire as pw;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Bound on the registry round trip. A wedged daemon must not keep the sink
/// from being published: on expiry the check is skipped, not failed.
pub const DUPLICATE_NODE_SCAN_TIMEOUT: Duration = Duration::from_secs(2);

/// An `Audio/Sink` node in the registry carrying the name we are about to
/// publish. The registry exposes only a handful of node properties, so the
/// owner is reached through `client.id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SinkNodeEntry {
    pub id: u32,
    pub object_serial: Option<u64>,
    pub client_id: Option<u32>,
}

/// A client in the registry. The pid prefers `pipewire.sec.pid`, which the
/// daemon fills from the socket credentials, over the client-declared
/// `application.process.id`; the binary prefers the declared
/// `application.process.binary` over `application.name`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientEntry {
    pub id: u32,
    pub pid: Option<u32>,
    pub binary: Option<String>,
}

/// A node with our name that belongs to someone else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DuplicateLiveInputNode {
    pub node_id: u32,
    pub object_serial: Option<u64>,
    pub client_id: Option<u32>,
    pub pid: Option<u32>,
    pub binary: Option<String>,
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

/// Reads a registry `Node` global: `Some` only for an `Audio/Sink` named
/// `node_name`.
pub fn sink_node_from_props<'a>(
    id: u32,
    node_name: &str,
    get: impl Fn(&str) -> Option<&'a str>,
) -> Option<SinkNodeEntry> {
    if non_empty(get(*pw::keys::MEDIA_CLASS)) != Some("Audio/Sink") {
        return None;
    }
    if non_empty(get(*pw::keys::NODE_NAME)) != Some(node_name) {
        return None;
    }
    Some(SinkNodeEntry {
        id,
        object_serial: non_empty(get(*pw::keys::OBJECT_SERIAL)).and_then(|v| v.parse().ok()),
        client_id: non_empty(get(*pw::keys::CLIENT_ID)).and_then(|v| v.parse().ok()),
    })
}

/// Reads a registry `Client` global.
pub fn client_from_props<'a>(id: u32, get: impl Fn(&str) -> Option<&'a str>) -> ClientEntry {
    let pid = non_empty(get(*pw::keys::SEC_PID))
        .or_else(|| non_empty(get(*pw::keys::APP_PROCESS_ID)))
        .and_then(|v| v.parse().ok());
    let binary = non_empty(get(*pw::keys::APP_PROCESS_BINARY))
        .or_else(|| non_empty(get(*pw::keys::APP_NAME)))
        .map(str::to_owned);
    ClientEntry { id, pid, binary }
}

/// Joins the nodes to their owning clients and keeps those held by another
/// process. A node owned by this process is a sibling connection, or the
/// previous incarnation of this backend still being torn down — not a
/// conflict. A node with no resolvable owner is reported: it may be a sink
/// the daemon itself loaded under our name.
pub fn resolve_duplicates(
    nodes: &[SinkNodeEntry],
    clients: &[ClientEntry],
    own_pid: u32,
) -> Vec<DuplicateLiveInputNode> {
    nodes
        .iter()
        .filter_map(|node| {
            let client = node
                .client_id
                .and_then(|client_id| clients.iter().find(|client| client.id == client_id));
            let pid = client.and_then(|client| client.pid);
            if pid == Some(own_pid) {
                return None;
            }
            Some(DuplicateLiveInputNode {
                node_id: node.id,
                object_serial: node.object_serial,
                client_id: node.client_id,
                pid,
                binary: client.and_then(|client| client.binary.clone()),
            })
        })
        .collect()
}

/// The message logged and shown in Studio's input status. It opens with a
/// stable phrase Studio matches on to surface the Audio Input section.
pub fn duplicate_node_warning(node_name: &str, duplicates: &[DuplicateLiveInputNode]) -> String {
    let owners: Vec<String> = duplicates
        .iter()
        .map(|dup| {
            let owner = match (dup.pid, dup.binary.as_deref(), dup.client_id) {
                (Some(pid), Some(binary), _) => format!("pid {pid} ({binary})"),
                (Some(pid), None, _) => format!("pid {pid}"),
                (None, Some(binary), _) => format!("{binary} (pid unknown)"),
                (None, None, Some(client_id)) => format!("client {client_id} (pid unknown)"),
                (None, None, None) => "an unknown owner".to_string(),
            };
            format!("{owner} as node {}", dup.node_id)
        })
        .collect();
    format!(
        "duplicate PipeWire sink \"{node_name}\": already published by {}; clients targeting \"{node_name}\" may attach to that node instead of this renderer",
        owners.join(" and by ")
    )
}

/// Takes a registry snapshot with one core round trip on the calling
/// thread's main loop and returns the foreign nodes carrying `node_name`.
/// `Ok(None)` when the daemon did not answer within `timeout`.
pub fn scan_duplicate_live_input_nodes(
    mainloop: &pw::main_loop::MainLoopRc,
    core: &pw::core::CoreRc,
    node_name: &str,
    timeout: Duration,
) -> Result<Option<Vec<DuplicateLiveInputNode>>> {
    let registry = core
        .get_registry()
        .map_err(|e| anyhow!("Failed to get PipeWire registry: {e:?}"))?;
    // Queued after the registry bind, so its `done` lands after every
    // existing global has been announced.
    let pending = core
        .sync(0)
        .map_err(|e| anyhow!("PipeWire sync failed: {e:?}"))?;

    let done = Rc::new(Cell::new(false));
    let nodes = Rc::new(RefCell::new(Vec::<SinkNodeEntry>::new()));
    let clients = Rc::new(RefCell::new(Vec::<ClientEntry>::new()));

    let done_for_core = Rc::clone(&done);
    let _core_listener = core
        .add_listener_local()
        .done(move |id, seq| {
            if id == pw::core::PW_ID_CORE && seq == pending {
                done_for_core.set(true);
            }
        })
        .register();

    let node_name_for_registry = node_name.to_owned();
    let nodes_for_registry = Rc::clone(&nodes);
    let clients_for_registry = Rc::clone(&clients);
    let _registry_listener = registry
        .add_listener_local()
        .global(move |global| {
            let Some(props) = global.props.as_ref() else {
                return;
            };
            match global.type_ {
                pw::types::ObjectType::Node => {
                    if let Some(node) =
                        sink_node_from_props(global.id, &node_name_for_registry, |key| {
                            props.get(key)
                        })
                    {
                        nodes_for_registry.borrow_mut().push(node);
                    }
                }
                pw::types::ObjectType::Client => {
                    clients_for_registry
                        .borrow_mut()
                        .push(client_from_props(global.id, |key| props.get(key)));
                }
                _ => {}
            }
        })
        .register();

    let deadline = Instant::now() + timeout;
    while !done.get() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        let _ = mainloop
            .loop_()
            .iterate(remaining.min(Duration::from_millis(50)));
    }

    // `pipewire.sec.pid` is the peer pid as the daemon sees it; across a pid
    // namespace boundary (a sandboxed renderer) it would differ from ours and
    // a sibling connection of this process would show up as foreign.
    let duplicates = resolve_duplicates(&nodes.borrow(), &clients.borrow(), std::process::id());
    Ok(Some(duplicates))
}

/// Runs the scan and, when another owner already holds `node_name`, logs the
/// warning and posts it to the input status that Studio displays. A scan
/// that fails or times out is logged and skipped: it must never keep the
/// sink from being published.
pub fn warn_on_duplicate_live_input_node(
    mainloop: &pw::main_loop::MainLoopRc,
    core: &pw::core::CoreRc,
    input_control: &InputControl,
    node_name: &str,
) {
    match scan_duplicate_live_input_nodes(mainloop, core, node_name, DUPLICATE_NODE_SCAN_TIMEOUT) {
        Ok(Some(duplicates)) if !duplicates.is_empty() => {
            let warning = duplicate_node_warning(node_name, &duplicates);
            log::warn!("{warning}");
            input_control.set_input_error(Some(warning));
        }
        Ok(Some(_)) => {
            log::debug!("No other PipeWire sink named \"{node_name}\" in the registry");
        }
        Ok(None) => {
            log::warn!(
                "Registry scan for a duplicate PipeWire sink named \"{node_name}\" timed out after {} ms; check skipped",
                DUPLICATE_NODE_SCAN_TIMEOUT.as_millis()
            );
        }
        Err(err) => {
            log::warn!(
                "Registry scan for a duplicate PipeWire sink named \"{node_name}\" failed: {err:#}; check skipped"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props<'a>(items: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<&'a str> {
        move |key| items.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
    }

    #[test]
    fn sink_node_needs_the_class_and_the_exact_name() {
        let ours = [
            ("media.class", "Audio/Sink"),
            ("node.name", "omniphony"),
            ("object.serial", "44705"),
            ("client.id", "535"),
        ];
        assert_eq!(
            sink_node_from_props(477, "omniphony", props(&ours)),
            Some(SinkNodeEntry {
                id: 477,
                object_serial: Some(44705),
                client_id: Some(535),
            })
        );

        let other_name = [("media.class", "Audio/Sink"), ("node.name", "omniphony2")];
        assert_eq!(
            sink_node_from_props(1, "omniphony", props(&other_name)),
            None
        );

        let source = [("media.class", "Audio/Source"), ("node.name", "omniphony")];
        assert_eq!(sink_node_from_props(2, "omniphony", props(&source)), None);

        let stream = [
            ("media.class", "Stream/Output/Audio"),
            ("node.name", "omniphony"),
        ];
        assert_eq!(sink_node_from_props(3, "omniphony", props(&stream)), None);
    }

    #[test]
    fn sink_node_tolerates_missing_serial_and_client() {
        let bare = [("media.class", "Audio/Sink"), ("node.name", "omniphony")];
        assert_eq!(
            sink_node_from_props(9, "omniphony", props(&bare)),
            Some(SinkNodeEntry {
                id: 9,
                object_serial: None,
                client_id: None,
            })
        );
    }

    #[test]
    fn client_prefers_the_daemon_verified_pid_and_the_binary() {
        let full = [
            ("pipewire.sec.pid", "1491576"),
            ("application.process.id", "42"),
            ("application.process.binary", "orender"),
            ("application.name", "Omniphony"),
        ];
        assert_eq!(
            client_from_props(535, props(&full)),
            ClientEntry {
                id: 535,
                pid: Some(1491576),
                binary: Some("orender".to_string()),
            }
        );

        // What `orender` actually registers: no declared pid or binary.
        let minimal = [
            ("pipewire.sec.pid", "1491576"),
            ("application.name", "orender"),
        ];
        assert_eq!(
            client_from_props(253, props(&minimal)),
            ClientEntry {
                id: 253,
                pid: Some(1491576),
                binary: Some("orender".to_string()),
            }
        );

        let declared_only = [("application.process.id", "42")];
        assert_eq!(
            client_from_props(7, props(&declared_only)),
            ClientEntry {
                id: 7,
                pid: Some(42),
                binary: None,
            }
        );
    }

    fn node(id: u32, client_id: Option<u32>) -> SinkNodeEntry {
        SinkNodeEntry {
            id,
            object_serial: Some(u64::from(id) * 100),
            client_id,
        }
    }

    fn client(id: u32, pid: Option<u32>, binary: &str) -> ClientEntry {
        ClientEntry {
            id,
            pid,
            binary: Some(binary.to_string()),
        }
    }

    #[test]
    fn own_process_nodes_are_not_duplicates() {
        // Two connections of this process (output + input) plus the sink of
        // a renderer from another workflow.
        let nodes = [node(477, Some(535)), node(612, Some(700))];
        let clients = [
            client(253, Some(1000), "orender"),
            client(535, Some(1000), "orender"),
            client(700, Some(2000), "orender"),
        ];
        let duplicates = resolve_duplicates(&nodes, &clients, 1000);
        assert_eq!(
            duplicates,
            vec![DuplicateLiveInputNode {
                node_id: 612,
                object_serial: Some(61200),
                client_id: Some(700),
                pid: Some(2000),
                binary: Some("orender".to_string()),
            }]
        );
    }

    #[test]
    fn nodes_without_a_resolvable_owner_are_reported() {
        let nodes = [node(10, None), node(11, Some(999))];
        let clients = [client(535, Some(1000), "orender")];
        let duplicates = resolve_duplicates(&nodes, &clients, 1000);
        assert_eq!(duplicates.len(), 2);
        assert!(duplicates.iter().all(|dup| dup.pid.is_none()));
        assert_eq!(duplicates[1].client_id, Some(999));
    }

    #[test]
    fn warning_names_the_owner_and_opens_with_the_studio_marker() {
        let duplicates = [DuplicateLiveInputNode {
            node_id: 477,
            object_serial: Some(44705),
            client_id: Some(535),
            pid: Some(1491576),
            binary: Some("orender".to_string()),
        }];
        let warning = duplicate_node_warning("omniphony", &duplicates);
        assert!(warning.starts_with("duplicate PipeWire sink \"omniphony\""));
        assert!(warning.contains("pid 1491576 (orender) as node 477"));

        let unknown = [DuplicateLiveInputNode {
            node_id: 5,
            object_serial: None,
            client_id: None,
            pid: None,
            binary: None,
        }];
        assert!(duplicate_node_warning("x", &unknown).contains("an unknown owner as node 5"));

        let several = [duplicates[0].clone(), unknown[0].clone()];
        let warning = duplicate_node_warning("x", &several);
        assert!(warning.contains("as node 477 and by an unknown owner as node 5"));
    }

    /// Talks to the session's PipeWire daemon: run with `--ignored` while a
    /// renderer publishes `omniphony` from another process to see it listed.
    #[test]
    #[ignore = "needs a running PipeWire session"]
    fn live_registry_scan_completes_and_never_reports_this_process() {
        pw::init();
        let mainloop = pw::main_loop::MainLoopRc::new(None).expect("main loop");
        let context = pw::context::ContextRc::new(&mainloop, None).expect("context");
        let core = context.connect_rc(None).expect("core");
        let scanned = scan_duplicate_live_input_nodes(
            &mainloop,
            &core,
            "omniphony",
            DUPLICATE_NODE_SCAN_TIMEOUT,
        )
        .expect("registry scan");
        let duplicates = scanned.expect("the daemon answered the sync within the timeout");
        for dup in &duplicates {
            eprintln!("foreign omniphony sink: {dup:?}");
            assert_ne!(dup.pid, Some(std::process::id()));
        }
    }
}
