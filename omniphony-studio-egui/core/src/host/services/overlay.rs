//! Keeping the mpv overlay showing what Studio shows.
//!
//! The overlay draws the same scene on top of the video, and the renderer owns
//! and persists its settings. Studio's job is to keep them in step with what
//! it is showing itself, so the two pictures do not disagree — a trail visible
//! in Studio and absent over the film is a bug the user reports as "the
//! overlay is broken".
//!
//! Two moments matter, and neither is a frame: a mirrored control changing,
//! and a fresh connection. The renderer comes up on its own persisted values,
//! so the whole set is pushed again whenever it has just told us its state.

use std::sync::atomic::Ordering;
use std::time::Instant;

use super::Tick;
use crate::host::commands::{SharedState, mpv_overlay};

/// Everything Studio mirrors, in the overlay's own terms. The view converts
/// its settings into this; the service decides what to send and when.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct OverlayPrefs {
    pub objects: bool,
    pub labels: bool,
    pub heatmap: bool,
    pub bands: i32,
    pub colormap: i32,
    pub trails: bool,
    pub trail_ttl_ms: u32,
    pub trail_line: bool,
    pub teleport_threshold: f32,
    /// The custom gradient as the renderer takes it: `[pos, r, g, b, …]`.
    pub stops: Vec<f32>,
}

/// What the view is showing. Called whenever it draws; the service only acts
/// on a change.
pub fn set_overlay_prefs(state: &SharedState, prefs: OverlayPrefs) {
    let mut live = state.inner.lock().unwrap();
    if live.overlay_prefs.as_ref() != Some(&prefs) {
        live.overlay_prefs = Some(prefs);
        drop(live);
        (state.waker)();
    }
}

#[derive(Default)]
pub struct MpvOverlay {
    pushed: Option<OverlayPrefs>,
    pushed_epoch: Option<u64>,
}

impl MpvOverlay {
    pub fn tick(&mut self, state: &SharedState, _now: Instant) -> Tick {
        if state.stats.connection_state() != crate::osc::ConnectionState::Connected {
            return Tick::idle();
        }
        let epoch = state.stats.connection_epoch.load(Ordering::Relaxed);
        let wanted = state.inner.lock().unwrap().overlay_prefs.clone();
        let Some(wanted) = wanted else {
            return Tick::idle();
        };
        let previous = if self.pushed_epoch == Some(epoch) {
            self.pushed.as_ref()
        } else {
            None
        };
        if previous.is_none_or(|old| old.objects != wanted.objects) {
            mpv_overlay::mpv_overlay_set_objects(state, wanted.objects);
        }
        if previous.is_none_or(|old| old.labels != wanted.labels) {
            mpv_overlay::mpv_overlay_set_labels(state, wanted.labels);
        }
        if previous.is_none_or(|old| old.heatmap != wanted.heatmap) {
            mpv_overlay::mpv_overlay_set_heatmap_enabled(state, wanted.heatmap);
        }
        if previous.is_none_or(|old| old.bands != wanted.bands) {
            mpv_overlay::mpv_overlay_set_heatmap_bands(state, wanted.bands);
        }
        if previous.is_none_or(|old| old.colormap != wanted.colormap) {
            mpv_overlay::mpv_overlay_set_heatmap_colormap(state, wanted.colormap);
        }
        if previous.is_none_or(|old| old.stops != wanted.stops) {
            mpv_overlay::mpv_overlay_set_heatmap_custom_stops(state, wanted.stops.clone());
        }
        if previous.is_none_or(|old| {
            (
                old.trails,
                old.trail_ttl_ms,
                old.trail_line,
                old.teleport_threshold,
            ) != (
                wanted.trails,
                wanted.trail_ttl_ms,
                wanted.trail_line,
                wanted.teleport_threshold,
            )
        }) {
            mpv_overlay::mpv_overlay_set_trail_prefs(
                state,
                wanted.trails,
                wanted.trail_ttl_ms,
                if wanted.trail_line { "line" } else { "diffuse" }.to_owned(),
                wanted.teleport_threshold,
            );
        }
        self.pushed = Some(wanted);
        self.pushed_epoch = Some(epoch);
        Tick::idle()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unrelated_snapshots_send_nothing_and_reconnection_sends_one_full_set() {
        let mut state = crate::host::commands::tests::state();
        let (tx, rx) = std::sync::mpsc::channel();
        state.osc_tx = tx;
        let mut service = MpvOverlay::default();
        set_overlay_prefs(&state, OverlayPrefs::default());
        service.tick(&state, Instant::now());
        assert_eq!(rx.try_iter().count(), 0, "offline changes stay pending");
        state.stats.registered.store(true, Ordering::Relaxed);
        service.tick(&state, Instant::now());
        assert_eq!(rx.try_iter().count(), 7);
        state.inner.lock().unwrap().snapshot_epoch += 1;
        service.tick(&state, Instant::now());
        assert_eq!(rx.try_iter().count(), 0);
        set_overlay_prefs(
            &state,
            OverlayPrefs {
                labels: true,
                ..Default::default()
            },
        );
        service.tick(&state, Instant::now());
        let sent: Vec<_> = rx.try_iter().collect();
        assert_eq!(sent.len(), 1);
        assert!(
            matches!(&sent[0], crate::osc::Control::Send { address, .. } if address == crate::osc_contract::CONTROL_OVERLAY_LABELS)
        );
        state.stats.connection_epoch.fetch_add(1, Ordering::Relaxed);
        service.tick(&state, Instant::now());
        assert_eq!(rx.try_iter().count(), 7);
        service.tick(&state, Instant::now());
        assert_eq!(rx.try_iter().count(), 0);
    }
}
