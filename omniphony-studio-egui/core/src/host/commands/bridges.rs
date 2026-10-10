//! The decoder bridge list (`render.bridge_paths`, docs/multi-bridge.md):
//! what the Input panel shows and every edit it makes.
//!
//! The engine publishes `/omniphony/state/render/bridges` — the paths asked
//! for, then each bridge loaded (with its source families) or failed (with
//! why) — and takes `/omniphony/control/render/bridge_paths`, the whole list
//! in load order. The list here is what the panel draws: the requested
//! entries in order with their status, then what is loaded but no longer
//! asked for (until the restart that drops it), or, with nothing asked for,
//! what auto-discovery loaded.
//!
//! The bridge decoding the stream now is derived here, not published: the
//! playing stream's source family (`fixedChannelProcessing`, what the
//! placement readouts use) is matched against the families each loaded bridge
//! declares, and the first one in load order that lists it wins — the rule the
//! engine routes by when two bridges cover one family (an override is listed
//! first). Display only: nothing is written or saved.
//!
//! An edit is unsaved engine state: the engine marks its config dirty and
//! the list reaches `config.yaml` only through Save
//! (docs/persistence-policy.md). It takes effect at the next restart, which
//! the panel's Apply sends.

use rosc::OscType;

use super::OscControlMsg;
use super::{SharedState, send_control};
use crate::host::channels::{Family, playing_family};
use crate::model::app_state::RenderBridges;
use crate::osc_contract;

/// What the Input panel draws of the decoder bridges.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    /// `None` while the engine has not published its list: one predating
    /// contract revision 4, or none connected. The panel then keeps the
    /// single path field.
    pub list: Option<BridgeList>,
    /// The single path (`render/bridge_path`), for that fallback.
    pub single_path: String,
    /// Whether the renderer runs on this machine, where a file picked here
    /// names the same file for it.
    pub local: bool,
}

impl Snapshot {
    pub fn read(state: &SharedState) -> Self {
        let local = state
            .stats
            .target
            .lock()
            .unwrap()
            .is_some_and(|target| target.ip().is_loopback());
        let live = state.read();
        Self {
            list: live.app.render_bridges.as_ref().map(|bridges| {
                BridgeList::of(
                    bridges,
                    live.app.render_bridges_edited,
                    playing_family(&live.app),
                )
            }),
            single_path: live.app.render_bridge_path.clone().unwrap_or_default(),
            local,
        }
    }
}

/// The bridge list, ready to draw.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BridgeList {
    /// The paths asked for, in load order. Empty: auto-discovery.
    pub requested: Vec<String>,
    /// The requested entries in order, then the bridges the engine reports
    /// that are not among them.
    pub rows: Vec<Row>,
    /// The list differs from what the engine loaded: the next restart (the
    /// panel's Apply) loads it.
    pub restart_pending: bool,
}

/// One bridge of the list.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub path: String,
    /// Its index in the requested list; `None` for a bridge loaded without
    /// being asked for — by auto-discovery, or one removed from the list
    /// that stays loaded until the restart.
    pub position: Option<usize>,
    pub status: RowStatus,
    /// The bridge decoding the playing stream (see the module doc): at most
    /// one row, and only a loaded one.
    pub decoding: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RowStatus {
    /// Loaded, with the source families it declares.
    Loaded { families: Vec<String> },
    /// Asked for or found, and refused: why.
    Failed { error: String },
    /// Asked for, and not loaded yet: added since the last restart.
    NotLoaded,
}

impl Row {
    /// The file name: the full path is long, and the panel narrow. Split on
    /// both separators, since a remote renderer may run on another system.
    pub fn file_name(&self) -> &str {
        self.path
            .rsplit(['/', '\\'])
            .find(|part| !part.is_empty())
            .unwrap_or(&self.path)
    }
}

impl BridgeList {
    /// Join what was asked for with what the engine reports. `edited`: the
    /// list was changed from this Studio since the last restart, which is
    /// the only sign of a pending change when the list was emptied (the
    /// bridges reported are then indistinguishable from discovered ones).
    /// `playing`: the family of the stream playing, `None` while idle; it
    /// marks the row of the bridge that decodes it.
    pub fn of(bridges: &RenderBridges, edited: bool, playing: Option<Family>) -> Self {
        let mut rows: Vec<Row> = bridges
            .requested
            .iter()
            .enumerate()
            .map(|(index, path)| Row {
                path: path.clone(),
                position: Some(index),
                status: bridges
                    .bridges
                    .iter()
                    .find(|bridge| &bridge.path == path)
                    .map(status_of)
                    .unwrap_or(RowStatus::NotLoaded),
                decoding: false,
            })
            .collect();
        for bridge in &bridges.bridges {
            if !bridges.requested.contains(&bridge.path) {
                rows.push(Row {
                    path: bridge.path.clone(),
                    position: None,
                    status: status_of(bridge),
                    decoding: false,
                });
            }
        }
        if let Some(path) = playing.and_then(|family| decoding_bridge(bridges, family))
            && let Some(row) = rows
                .iter_mut()
                .find(|row| row.path == path && matches!(row.status, RowStatus::Loaded { .. }))
        {
            row.decoding = true;
        }
        let explicit = !bridges.requested.is_empty();
        let mismatch = explicit
            && rows.iter().any(|row| match (row.position, &row.status) {
                (Some(_), RowStatus::NotLoaded) => true,
                (None, RowStatus::Loaded { .. }) => true,
                _ => false,
            });
        Self {
            requested: bridges.requested.clone(),
            rows,
            restart_pending: edited || mismatch,
        }
    }

    /// Nothing asked for: the engine loads what it discovers.
    pub fn is_auto(&self) -> bool {
        self.requested.is_empty()
    }
}

/// The path of the bridge that decodes a stream of `family`: the first loaded
/// bridge, in load order, that declares it (compared as the renderer's family
/// table does, trimmed and case-insensitive). `None` for the generic family —
/// a stream that names none, or one no bridge declared, which no bridge can
/// be told apart by — and when no loaded bridge declares it (e.g. live PCM).
///
/// Exact while the loaded bridges declare disjoint families, as harletty's
/// family plugins do. With two declaring one family, the first is the one
/// the engine picks when both claim a stream (load order breaks the tie,
/// docs/multi-bridge.md); a stream only the later one claims, or one a
/// forced `input_codec` sends to it, is shown on the first.
pub fn decoding_bridge(bridges: &RenderBridges, family: Family) -> Option<&str> {
    if family.is_generic() {
        return None;
    }
    bridges
        .bridges
        .iter()
        .filter(|bridge| bridge.error.is_none())
        .find(|bridge| {
            bridge
                .families
                .iter()
                .any(|declared| declared.trim().eq_ignore_ascii_case(family.as_str()))
        })
        .map(|bridge| bridge.path.as_str())
}

fn status_of(bridge: &crate::model::app_state::RenderBridge) -> RowStatus {
    match &bridge.error {
        Some(error) => RowStatus::Failed {
            error: error.clone(),
        },
        None => RowStatus::Loaded {
            families: bridge.families.clone(),
        },
    }
}

/// An edit of the requested list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Append a path (picked, or typed for a renderer on another machine).
    Add(String),
    /// Remove the entry at this index.
    Remove(usize),
    /// Move the entry at `from` to `to`: the earlier bridge wins when two
    /// accept the same stream.
    Move { from: usize, to: usize },
}

/// The list `action` makes of `list`, or `None` when it changes nothing (an
/// empty or duplicate path, an index out of range).
pub fn edited(list: &[String], action: &Action) -> Option<Vec<String>> {
    let mut next = list.to_vec();
    match action {
        Action::Add(path) => {
            let path = path.trim();
            if path.is_empty() || list.iter().any(|entry| entry == path) {
                return None;
            }
            next.push(path.to_owned());
        }
        Action::Remove(index) => {
            if *index >= next.len() {
                return None;
            }
            next.remove(*index);
        }
        Action::Move { from, to } => {
            if from == to || *from >= next.len() || *to >= next.len() {
                return None;
            }
            let entry = next.remove(*from);
            next.insert(*to, entry);
        }
    }
    Some(next)
}

/// Apply an edit to the current list and send the result.
pub fn apply(state: &SharedState, action: Action) {
    let current = {
        let live = state.read();
        match &live.app.render_bridges {
            Some(bridges) => bridges.requested.clone(),
            // No list published: an engine that only knows the single path.
            None => live.app.render_bridge_path.iter().cloned().collect(),
        }
    };
    if let Some(next) = edited(&current, &action) {
        set_render_bridge_paths(state, next);
    }
}

/// Ask for these bridges, in load order; empty for auto-discovery. Applied
/// to the model at once, so the panel shows the list being edited before
/// the engine's echo, and sent whole.
pub fn set_render_bridge_paths(state: &SharedState, paths: Vec<String>) {
    let paths: Vec<String> = paths
        .into_iter()
        .map(|path| path.trim().to_owned())
        .filter(|path| !path.is_empty())
        .collect();
    let list_known = {
        let mut live = state.inner.lock().unwrap();
        live.app.render_bridge_path = paths.first().cloned();
        live.app.render_bridges_edited = true;
        match live.app.render_bridges.as_mut() {
            Some(bridges) => {
                bridges.requested = paths.clone();
                true
            }
            None => false,
        }
    };
    if list_known {
        control_render_bridge_paths(state, &paths);
    } else {
        // An engine without the list control refuses it; the single path is
        // what it understands.
        super::render::control_render_bridge_path(
            state,
            paths.first().cloned().unwrap_or_default(),
        );
    }
}

/// Resend the requested list as the model holds it, before the restart that
/// loads it (the input panel's Apply).
pub fn resend_for_restart(state: &SharedState) {
    let (list, single) = {
        let mut live = state.inner.lock().unwrap();
        live.app.render_bridges_edited = false;
        (
            live.app
                .render_bridges
                .as_ref()
                .map(|bridges| bridges.requested.clone()),
            live.app.render_bridge_path.clone().unwrap_or_default(),
        )
    };
    match list {
        Some(paths) => control_render_bridge_paths(state, &paths),
        None => super::render::control_render_bridge_path(state, single),
    }
}

/// `/omniphony/control/render/bridge_paths`: one string per bridge, none for
/// auto-discovery.
pub fn control_render_bridge_paths(state: &SharedState, paths: &[String]) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: osc_contract::CONTROL_RENDER_BRIDGE_PATHS.to_string(),
            args: paths
                .iter()
                .map(|path| OscType::String(path.clone()))
                .collect(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::commands::tests::state_with_outbox;
    use crate::model::app_state::RenderBridge;

    fn loaded(path: &str, families: &[&str]) -> RenderBridge {
        RenderBridge {
            path: path.to_owned(),
            families: families.iter().map(|f| (*f).to_owned()).collect(),
            error: None,
        }
    }

    fn failed(path: &str, error: &str) -> RenderBridge {
        RenderBridge {
            path: path.to_owned(),
            families: Vec::new(),
            error: Some(error.to_owned()),
        }
    }

    fn bridges(requested: &[&str], status: Vec<RenderBridge>) -> RenderBridges {
        RenderBridges {
            requested: requested.iter().map(|p| (*p).to_owned()).collect(),
            bridges: status,
        }
    }

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|p| (*p).to_owned()).collect()
    }

    /// Everything sent so far, as address and string arguments.
    fn sent(rx: &std::sync::mpsc::Receiver<crate::osc::Control>) -> Vec<(String, Vec<String>)> {
        rx.try_iter()
            .filter_map(|control| match control {
                crate::osc::Control::Send { address, args } => Some((
                    address,
                    args.into_iter()
                        .map(|arg| match arg {
                            OscType::String(s) => s,
                            other => panic!("not a string: {other:?}"),
                        })
                        .collect(),
                )),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn requested_entries_keep_their_order_and_take_their_status() {
        let list = BridgeList::of(
            &bridges(
                &["/b/dts.so", "/b/dolby.so", "/b/gone.so"],
                vec![
                    loaded("/b/dolby.so", &["truehd", "eac3"]),
                    loaded("/b/dts.so", &["dts"]),
                    failed("/b/gone.so", "does not exist"),
                ],
            ),
            false,
            None,
        );
        assert_eq!(
            list.rows,
            vec![
                Row {
                    path: "/b/dts.so".into(),
                    position: Some(0),
                    status: RowStatus::Loaded {
                        families: strings(&["dts"])
                    },
                    decoding: false,
                },
                Row {
                    path: "/b/dolby.so".into(),
                    position: Some(1),
                    status: RowStatus::Loaded {
                        families: strings(&["truehd", "eac3"])
                    },
                    decoding: false,
                },
                Row {
                    path: "/b/gone.so".into(),
                    position: Some(2),
                    status: RowStatus::Failed {
                        error: "does not exist".into()
                    },
                    decoding: false,
                },
            ]
        );
        assert!(!list.is_auto());
        assert!(!list.restart_pending);
    }

    /// Nothing asked for: the rows are what discovery loaded and refused,
    /// none of them editable.
    #[test]
    fn auto_discovery_shows_what_it_loaded() {
        let list = BridgeList::of(
            &bridges(
                &[],
                vec![
                    loaded("/usr/lib/a_bridge.so", &["iamf"]),
                    failed("/usr/lib/libold_bridge.so", "bridge_api 0.5"),
                ],
            ),
            false,
            None,
        );
        assert!(list.is_auto());
        assert!(list.rows.iter().all(|row| row.position.is_none()));
        assert_eq!(list.rows.len(), 2);
        assert!(!list.restart_pending);
    }

    /// An entry added since the restart is not loaded yet, and one removed
    /// stays loaded until then: both say a restart is due.
    #[test]
    fn a_list_that_differs_from_what_is_loaded_awaits_the_restart() {
        let added = BridgeList::of(
            &bridges(&["/a.so", "/new.so"], vec![loaded("/a.so", &["dts"])]),
            false,
            None,
        );
        assert_eq!(added.rows[1].status, RowStatus::NotLoaded);
        assert!(added.restart_pending);

        let removed = BridgeList::of(
            &bridges(
                &["/a.so"],
                vec![loaded("/a.so", &["dts"]), loaded("/old.so", &["iamf"])],
            ),
            false,
            None,
        );
        assert_eq!(removed.rows[1].position, None);
        assert!(removed.restart_pending);

        // Emptied: what is loaded cannot be told from discovery's, so only
        // the edit says so.
        let emptied = bridges(&[], vec![loaded("/a.so", &["dts"])]);
        assert!(!BridgeList::of(&emptied, false, None).restart_pending);
        assert!(BridgeList::of(&emptied, true, None).restart_pending);
    }

    /// The rows marked decoding, by path.
    fn decoding(list: &BridgeList) -> Vec<&str> {
        list.rows
            .iter()
            .filter(|row| row.decoding)
            .map(|row| row.path.as_str())
            .collect()
    }

    fn family(name: &str) -> Option<Family> {
        Some(Family::named(name))
    }

    /// One family per bridge, as harletty's plugins: the stream's family
    /// names its bridge; idle, nothing is marked.
    #[test]
    fn the_playing_family_marks_the_bridge_that_declares_it() {
        let status = bridges(
            &["/b/dolby.so", "/b/dts.so"],
            vec![
                loaded("/b/dolby.so", &["dolby"]),
                loaded("/b/dts.so", &["dts", "auro"]),
            ],
        );
        assert!(decoding(&BridgeList::of(&status, false, None)).is_empty());
        assert_eq!(
            decoding(&BridgeList::of(&status, false, family("dolby"))),
            ["/b/dolby.so"]
        );
        // Compared as the renderer's family table does.
        let shouted = bridges(&[], vec![loaded("/b/dts.so", &[" DTS "])]);
        assert_eq!(
            decoding(&BridgeList::of(&shouted, false, family("dts"))),
            ["/b/dts.so"]
        );
        assert_eq!(
            decoding(&BridgeList::of(&status, false, family("auro"))),
            ["/b/dts.so"]
        );
        // A family no bridge declares (the reference bridge's `pcm`), and
        // the generic family, mark nothing.
        assert!(decoding(&BridgeList::of(&status, false, family("pcm"))).is_empty());
        assert!(decoding(&BridgeList::of(&status, false, Some(Family::GENERIC))).is_empty());
    }

    /// Two bridges declaring one family: the first in load order, the one
    /// the engine picks when both claim a stream — whatever order the list
    /// asked for, which only takes effect at the restart.
    #[test]
    fn with_one_family_twice_the_first_loaded_bridge_decodes() {
        let status = bridges(
            &["/override.so", "/b/dolby.so"],
            vec![
                loaded("/override.so", &["dolby"]),
                loaded("/b/dolby.so", &["dolby"]),
            ],
        );
        assert_eq!(
            decoding(&BridgeList::of(&status, false, family("dolby"))),
            ["/override.so"]
        );
        // Reordered, not restarted yet: the engine still has the override
        // first.
        let reordered = bridges(&["/b/dolby.so", "/override.so"], status.bridges.clone());
        assert_eq!(
            decoding(&BridgeList::of(&reordered, true, family("dolby"))),
            ["/override.so"]
        );
    }

    /// Discovered bridges are marked like requested ones; a bridge that
    /// failed to load never is, even when another entry of the same path
    /// lists the family.
    #[test]
    fn discovered_rows_are_marked_and_failed_ones_never() {
        let discovered = bridges(
            &[],
            vec![
                loaded("/usr/lib/harletty_dolby_bridge.so", &["dolby"]),
                loaded("/usr/lib/harletty_iamf_bridge.so", &["iamf"]),
            ],
        );
        let list = BridgeList::of(&discovered, false, family("iamf"));
        assert_eq!(decoding(&list), ["/usr/lib/harletty_iamf_bridge.so"]);
        assert!(list.rows.iter().all(|row| row.position.is_none()));

        // Removed from the list, still loaded until the restart: it decodes.
        let removed = bridges(
            &["/a.so"],
            vec![loaded("/a.so", &["dts"]), loaded("/old.so", &["iamf"])],
        );
        assert_eq!(
            decoding(&BridgeList::of(&removed, false, family("iamf"))),
            ["/old.so"]
        );

        let mut failing = failed("/b/dolby.so", "bridge_api 0.5");
        failing.families = strings(&["dolby"]);
        let refused = bridges(&["/b/dolby.so"], vec![failing]);
        assert!(decoding(&BridgeList::of(&refused, false, family("dolby"))).is_empty());
        // Asked for, not loaded yet: nothing decodes on it.
        let pending = bridges(&["/b/new.so"], Vec::new());
        assert!(decoding(&BridgeList::of(&pending, true, family("dolby"))).is_empty());
    }

    /// The snapshot the panel reads follows the stream: a family played, the
    /// stream gone idle, and the bridge list replaced.
    #[test]
    fn the_snapshot_follows_the_stream_and_the_list() {
        let (state, _rx) = state_with_outbox(std::sync::Arc::new(|| {}));
        let set_stream = |stream: &str, family: &str| {
            state
                .inner
                .lock()
                .unwrap()
                .app
                .live_options
                .fixed_channel_processing =
                Some(serde_json::json!({ "stream": stream, "family": family }));
        };
        state.inner.lock().unwrap().app.render_bridges = Some(bridges(
            &[],
            vec![loaded("/d.so", &["dolby"]), loaded("/i.so", &["iamf"])],
        ));
        let marked = |state: &SharedState| {
            Snapshot::read(state)
                .list
                .map(|list| decoding(&list).into_iter().map(str::to_owned).collect())
                .unwrap_or_else(Vec::<String>::new)
        };
        assert!(marked(&state).is_empty());
        set_stream("objects", "dolby");
        assert_eq!(marked(&state), ["/d.so"]);
        set_stream("fixed", "iamf");
        assert_eq!(marked(&state), ["/i.so"]);
        set_stream("idle", "iamf");
        assert!(marked(&state).is_empty());
        set_stream("fixed", "iamf");
        state.inner.lock().unwrap().app.render_bridges =
            Some(bridges(&[], vec![loaded("/d.so", &["dolby"])]));
        assert!(marked(&state).is_empty());
    }

    #[test]
    fn the_file_name_is_split_on_either_separator() {
        let row = |path: &str| Row {
            path: path.into(),
            position: None,
            status: RowStatus::NotLoaded,
            decoding: false,
        };
        assert_eq!(row("/usr/lib/liba_bridge.so").file_name(), "liba_bridge.so");
        assert_eq!(
            row(r"C:\Omniphony\a_bridge.dll").file_name(),
            "a_bridge.dll"
        );
        assert_eq!(row("plain.so").file_name(), "plain.so");
        assert_eq!(row("/dir/").file_name(), "dir");
    }

    #[test]
    fn add_appends_a_new_trimmed_path_once() {
        let list = strings(&["/a.so"]);
        assert_eq!(
            edited(&list, &Action::Add("  /b.so ".into())),
            Some(strings(&["/a.so", "/b.so"]))
        );
        assert_eq!(edited(&list, &Action::Add("/a.so".into())), None);
        assert_eq!(edited(&list, &Action::Add("   ".into())), None);
        assert_eq!(
            edited(&[], &Action::Add("/a.so".into())),
            Some(strings(&["/a.so"]))
        );
    }

    #[test]
    fn remove_and_move_edit_by_index() {
        let list = strings(&["/a.so", "/b.so", "/c.so"]);
        assert_eq!(
            edited(&list, &Action::Remove(1)),
            Some(strings(&["/a.so", "/c.so"]))
        );
        assert_eq!(edited(&list, &Action::Remove(3)), None);
        assert_eq!(
            edited(&list, &Action::Move { from: 2, to: 1 }),
            Some(strings(&["/a.so", "/c.so", "/b.so"]))
        );
        assert_eq!(
            edited(&list, &Action::Move { from: 0, to: 2 }),
            Some(strings(&["/b.so", "/c.so", "/a.so"]))
        );
        assert_eq!(edited(&list, &Action::Move { from: 1, to: 1 }), None);
        assert_eq!(edited(&list, &Action::Move { from: 0, to: 3 }), None);
    }

    /// An edit sends the whole list, one string per bridge, and shows it at
    /// once; the single path follows the first entry.
    #[test]
    fn an_edit_sends_the_whole_list_and_applies_it() {
        let (state, rx) = state_with_outbox(std::sync::Arc::new(|| {}));
        state.inner.lock().unwrap().app.render_bridges = Some(bridges(
            &["/a.so", "/b.so"],
            vec![loaded("/a.so", &["dts"]), loaded("/b.so", &["iamf"])],
        ));
        apply(&state, Action::Move { from: 1, to: 0 });
        assert_eq!(
            sent(&rx),
            vec![(
                osc_contract::CONTROL_RENDER_BRIDGE_PATHS.to_owned(),
                strings(&["/b.so", "/a.so"])
            )]
        );
        let snapshot = Snapshot::read(&state);
        let list = snapshot.list.unwrap();
        assert_eq!(list.requested, strings(&["/b.so", "/a.so"]));
        assert_eq!(snapshot.single_path, "/b.so");
        // Loaded in the old order, asked for in the new: the restart is due.
        assert!(list.restart_pending);

        // Removing every entry asks for auto-discovery: no argument at all.
        apply(&state, Action::Remove(0));
        apply(&state, Action::Remove(0));
        let messages = sent(&rx);
        assert_eq!(
            messages.last(),
            Some(&(
                osc_contract::CONTROL_RENDER_BRIDGE_PATHS.to_owned(),
                Vec::new()
            ))
        );
        assert_eq!(state.read().app.render_bridge_path, None);
    }

    /// An edit that changes nothing sends nothing.
    #[test]
    fn a_no_op_edit_sends_nothing() {
        let (state, rx) = state_with_outbox(std::sync::Arc::new(|| {}));
        state.inner.lock().unwrap().app.render_bridges = Some(bridges(&["/a.so"], Vec::new()));
        apply(&state, Action::Add("/a.so".into()));
        apply(&state, Action::Remove(4));
        assert!(sent(&rx).is_empty());
        assert!(!state.read().app.render_bridges_edited);
    }

    /// An engine that publishes no list only knows the single path: the
    /// edit goes out as that, and is never lost into a refused control.
    #[test]
    fn an_engine_without_the_list_gets_the_single_path() {
        let (state, rx) = state_with_outbox(std::sync::Arc::new(|| {}));
        state.inner.lock().unwrap().app.render_bridge_path = Some("/a.so".into());
        apply(&state, Action::Remove(0));
        assert_eq!(
            sent(&rx),
            vec![(
                osc_contract::CONTROL_RENDER_BRIDGE_PATH.to_owned(),
                vec![String::new()]
            )]
        );
    }

    /// The restart resends the list, not just its first entry, and clears
    /// the edit mark.
    #[test]
    fn the_restart_resends_the_whole_list() {
        let (state, rx) = state_with_outbox(std::sync::Arc::new(|| {}));
        {
            let mut live = state.inner.lock().unwrap();
            live.app.render_bridges = Some(bridges(&["/a.so", "/b.so"], Vec::new()));
            live.app.render_bridges_edited = true;
        }
        resend_for_restart(&state);
        assert_eq!(
            sent(&rx),
            vec![(
                osc_contract::CONTROL_RENDER_BRIDGE_PATHS.to_owned(),
                strings(&["/a.so", "/b.so"])
            )]
        );
        assert!(!state.read().app.render_bridges_edited);
    }
}
