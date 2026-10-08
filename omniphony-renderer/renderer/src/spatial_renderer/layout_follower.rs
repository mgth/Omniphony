//! Rebuilds the render topology on the layout a headphone render pans onto
//! (a BRIR set's loudspeakers, or the editable layout), and on the grid a new
//! stream's bridge hints, when no host does it.
//!
//! [`RendererControl::render_layout_outdated`] says when that layout changed:
//! a set landed or went away, or the output switched between speakers and
//! headphones with one selected; [`RendererControl::bridge_grid_pending`],
//! when a stream's bridge hinted another grid (`crate::evaluation_grid`).
//! The OSC listener follows both and tells its clients
//! ([`RendererControl::relayout_by_host`]); a real-time host without one (the
//! CLI with OSC off, an embedded host) relies on this worker. An offline
//! render does it on the render thread instead (`settle_brir_layout`).
//!
//! The render thread only compares a fingerprint per frame and, when it
//! moved, asks; the build — a triangulation — runs here, on a background
//! thread.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;

use crate::evaluation_grid::GridDecision;
use crate::live_params::RendererControl;

/// How long the worker waits for a host's running rebuild before it looks
/// again.
const BUSY_RETRY: Duration = Duration::from_millis(50);

/// What a frame looks at: the layout fingerprint
/// ([`RendererControl::render_layout_fingerprint`]), the count of grid hints
/// offered, whether the output is binaural and whether the HRIR source is a
/// BRIR set.
type Seen = ((u64, u64, bool), u64, bool, bool);

pub(super) struct LayoutFollower {
    request_tx: mpsc::SyncSender<()>,
    /// What the last frame looked at.
    last_seen: Option<Seen>,
}

impl LayoutFollower {
    pub(super) fn spawn(control: Arc<RendererControl>) -> Self {
        // One pending request is enough: the worker rebuilds on whatever is
        // wanted when it runs.
        let (request_tx, request_rx) = mpsc::sync_channel::<()>(1);
        std::thread::Builder::new()
            .name("render-layout-follower".into())
            .spawn(move || {
                crate::background_pool::enter_background();
                // Ends when the renderer, and its sender, is dropped.
                while request_rx.recv().is_ok() {
                    follow(&control);
                }
            })
            .expect("spawn render layout follower");
        Self {
            request_tx,
            last_seen: None,
        }
    }

    /// Once per frame, from the render thread: ask the worker for a rebuild
    /// when what the layout depends on moved and the published topology is
    /// no longer for it, or a stream's bridge hinted another grid. Steady
    /// state is four atomic loads and a compare.
    pub(super) fn poll(&mut self, control: &RendererControl, binaural: bool, brir_source: bool) {
        let seen = (
            control.render_layout_fingerprint(),
            control.bridge_grids_offered(),
            binaural,
            brir_source,
        );
        if self.last_seen == Some(seen) {
            return;
        }
        self.last_seen = Some(seen);
        if !control.relayout_by_host()
            && (control.render_layout_outdated() || control.bridge_grid_pending())
        {
            // Full: a request is already pending, which covers this one.
            let _ = self.request_tx.try_send(());
        }
    }
}

/// Rebuild and publish the topology while it is outdated, or its grid no
/// longer the bridge's, and no host owns the rebuilds. Claims `recomputing`
/// like a host's recompute, so the two never publish over each other.
fn follow(control: &RendererControl) {
    // A rebuild a host queued behind this worker's (it found `recomputing`
    // held) is owed whatever it was for: run once more, unconditionally.
    let mut owed = false;
    loop {
        if !owed
            && (control.relayout_by_host()
                || (!control.render_layout_outdated() && !control.bridge_grid_pending()))
        {
            return;
        }
        if control
            .recomputing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            std::thread::sleep(BUSY_RETRY);
            continue;
        }
        let mut rebuild = owed || control.render_layout_outdated();
        // A new stream's grid: no rebuild when a topology on it is at hand
        // (`RendererControl::request_live_grid`).
        let mut grid_only = false;
        if control.bridge_grid_pending()
            && control.take_bridge_grid()
            && control.request_live_grid() == GridDecision::Rebuild
        {
            grid_only = !rebuild;
            rebuild = true;
        }
        if rebuild && let Some(plan) = control.prepare_topology_rebuild() {
            let current = control.active_topology();
            match plan.build_topology_reusing(Some(&current)) {
                Ok(topology) => {
                    if control.publish_topology_if_current(topology, grid_only.then_some(current)) {
                        log::info!(
                            "Render topology rebuilt on the {} layout",
                            if plan.brir_layout {
                                "BRIR set's"
                            } else {
                                "editable"
                            }
                        );
                    } else {
                        // A grid request overtook it: build again for that.
                        owed = true;
                    }
                }
                Err(e) => {
                    control.grid_rebuild_failed();
                    log::warn!("Render topology rebuild failed: {e}");
                }
            }
        }
        control.recomputing.store(false, Ordering::Release);
        control.bump_live_state();
        owed = control.recompute_pending.swap(false, Ordering::AcqRel) || owed;
        if !owed && !control.render_layout_outdated() && !control.bridge_grid_pending() {
            return;
        }
    }
}
