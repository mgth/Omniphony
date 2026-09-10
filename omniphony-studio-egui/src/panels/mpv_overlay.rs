//! Mirroring Studio's display choices onto the mpv overlay (`mpvOverlay.js`).
//!
//! The overlay draws the same scene on top of the video, and the renderer owns
//! and persists its settings. Studio's job is to keep them in step with what it
//! is showing itself, so the two pictures do not disagree — a trail visible in
//! Studio and absent over the film is a bug the user reports as "the overlay is
//! broken".
//!
//! Two moments matter. Whenever a mirrored control changes, the change is
//! pushed. And on every fresh connection the whole set is pushed again: the
//! renderer comes up on its own persisted values, and without this Studio's
//! would not apply until the user touched each control in turn.

use crate::app::StudioSpike;
use crate::view::trails::TrailMode;

/// Everything Studio mirrors, as one comparable value. A struct rather than a
/// pile of fields so "did anything change" is one comparison.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct OverlayPrefs {
    objects: bool,
    labels: bool,
    heatmap: bool,
    bands: i32,
    colormap: i32,
    trails: bool,
    trail_ttl_ms: i64,
    trail_line: bool,
    teleport_threshold: f32,
}

impl StudioSpike {
    /// Push what changed, and everything on a fresh connection.
    pub(crate) fn maintain_mpv_overlay(&mut self) {
        let wanted = self.overlay_prefs();
        let epoch = self.live.lock().unwrap().snapshot_epoch;
        // A new snapshot epoch is a renderer that has just told us its whole
        // state — which is exactly when its overlay is back on its own
        // defaults.
        let reconnected = self.overlay_pushed_epoch != Some(epoch);
        if !reconnected && self.overlay_pushed == Some(wanted) {
            return;
        }
        self.overlay_pushed = Some(wanted);
        self.overlay_pushed_epoch = Some(epoch);
        self.ctl.send_int(
            "/omniphony/control/overlay/objects",
            i32::from(wanted.objects),
        );
        self.ctl.send_int(
            "/omniphony/control/overlay/labels",
            i32::from(wanted.labels),
        );
        self.ctl.send_int(
            "/omniphony/control/overlay/heatmap_enabled",
            i32::from(wanted.heatmap),
        );
        self.ctl
            .send_int("/omniphony/control/overlay/heatmap_bands", wanted.bands);
        self.ctl.send_int(
            "/omniphony/control/overlay/heatmap_colormap",
            wanted.colormap,
        );
        self.ctl.send(
            "/omniphony/control/overlay/trails",
            vec![
                rosc::OscType::Int(i32::from(wanted.trails)),
                rosc::OscType::Int(wanted.trail_ttl_ms as i32),
                rosc::OscType::String(
                    if wanted.trail_line { "line" } else { "diffuse" }.to_owned(),
                ),
                rosc::OscType::Float(wanted.teleport_threshold),
            ],
        );
    }

    fn overlay_prefs(&self) -> OverlayPrefs {
        let trails = &self.settings.trails;
        OverlayPrefs {
            objects: self.settings.objects_visible,
            labels: self.settings.object_labels_enabled,
            heatmap: self.volume_settings.object_field_enabled,
            // The web's own default, and what the overlay's heatmap is built
            // around; the renderer clamps it to 1..12 regardless.
            bands: 12,
            colormap: self.volume_settings.object_colormap as i32,
            trails: trails.enabled,
            // The renderer's own floor, applied here so a Studio value below it
            // does not silently become something else over the video.
            trail_ttl_ms: (trails.ttl.as_millis() as i64).max(500),
            trail_line: trails.mode == TrailMode::Line,
            teleport_threshold: trails.teleport_threshold.clamp(0.05, 2.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefs() -> OverlayPrefs {
        OverlayPrefs {
            objects: true,
            labels: true,
            heatmap: false,
            bands: 0,
            colormap: 0,
            trails: true,
            trail_ttl_ms: 7000,
            trail_line: false,
            teleport_threshold: 0.5,
        }
    }

    #[test]
    fn one_comparison_decides_whether_anything_needs_pushing() {
        let a = prefs();
        assert_eq!(a, prefs());
        let mut b = prefs();
        b.labels = false;
        assert_ne!(a, b);
        let mut c = prefs();
        c.teleport_threshold = 0.6;
        assert_ne!(a, c);
    }
}
