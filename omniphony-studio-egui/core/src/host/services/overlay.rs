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
    }
}

#[derive(Default)]
pub struct MpvOverlay {
    pushed: Option<OverlayPrefs>,
    pushed_epoch: Option<u64>,
}

impl MpvOverlay {
    pub fn tick(&mut self, state: &SharedState, _now: Instant) -> Tick {
        let (wanted, epoch) = {
            let live = state.inner.lock().unwrap();
            (live.overlay_prefs.clone(), live.snapshot_epoch)
        };
        let Some(wanted) = wanted else {
            return Tick::idle();
        };
        // A new snapshot epoch is a renderer that has just told us its whole
        // state — which is exactly when its overlay is back on its own
        // defaults.
        let reconnected = self.pushed_epoch != Some(epoch);
        if !reconnected && self.pushed.as_ref() == Some(&wanted) {
            return Tick::idle();
        }
        mpv_overlay::mpv_overlay_set_objects(state, wanted.objects);
        mpv_overlay::mpv_overlay_set_labels(state, wanted.labels);
        mpv_overlay::mpv_overlay_set_heatmap_enabled(state, wanted.heatmap);
        mpv_overlay::mpv_overlay_set_heatmap_bands(state, wanted.bands);
        mpv_overlay::mpv_overlay_set_heatmap_colormap(state, wanted.colormap);
        // The custom stops go with the colormap that uses them: pushing the
        // colormap without them would show the overlay's own gradient under
        // Studio's choice of "Custom".
        mpv_overlay::mpv_overlay_set_heatmap_custom_stops(state, wanted.stops.clone());
        mpv_overlay::mpv_overlay_set_trail_prefs(
            state,
            wanted.trails,
            wanted.trail_ttl_ms,
            if wanted.trail_line { "line" } else { "diffuse" }.to_owned(),
            wanted.teleport_threshold,
        );
        self.pushed = Some(wanted);
        self.pushed_epoch = Some(epoch);
        Tick::idle()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn everything_is_pushed_again_on_a_fresh_connection() {
        let state = crate::host::commands::tests::state();
        let mut service = MpvOverlay::default();
        set_overlay_prefs(&state, OverlayPrefs::default());
        service.tick(&state, Instant::now());
        let pushed = service.pushed.clone();
        assert!(pushed.is_some());

        // Nothing changed: nothing to do.
        service.tick(&state, Instant::now());
        assert_eq!(service.pushed_epoch, Some(0));

        // A renderer that has just restated its whole state gets the set back,
        // because its overlay came up on its own values.
        state.inner.lock().unwrap().snapshot_epoch = 1;
        service.tick(&state, Instant::now());
        assert_eq!(service.pushed_epoch, Some(1));
    }
}
