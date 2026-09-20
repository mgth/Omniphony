//! Telling the core what Studio is showing, so the mpv overlay can be kept in
//! step with it (`mpvOverlay.js`).
//!
//! The overlay draws the same scene on top of the video. Which settings are
//! mirrored is the view's business and lives here; pushing them, and pushing
//! them again on a fresh connection, is the core's (`services::overlay`).

use crate::app::StudioSpike;
use crate::host::services::overlay::{OverlayPrefs, set_overlay_prefs};
use crate::view::trails::TrailMode;
use crate::view::volumes::GradientStop;

/// The custom stops as the renderer takes them: `[pos, r, g, b, …]`.
fn stops_flat(stops: &[GradientStop]) -> Vec<f32> {
    stops
        .iter()
        .flat_map(|stop| [stop.pos, stop.rgb[0], stop.rgb[1], stop.rgb[2]])
        .collect()
}

impl StudioSpike {
    /// What Studio is showing, in the overlay's terms.
    pub(crate) fn declare_overlay_prefs(&mut self) {
        let trails = &self.settings.trails;
        set_overlay_prefs(
            &self.host,
            OverlayPrefs {
                objects: self.settings.objects_visible,
                labels: self.settings.object_labels_enabled,
                heatmap: self.volume_settings.object_field_enabled,
                // The web's own default, and what the overlay's heatmap is
                // built around; the renderer clamps it to 1..12 regardless.
                bands: 12,
                colormap: self.volume_settings.object_colormap as i32,
                trails: trails.enabled,
                // The renderer's own floor, applied here so a Studio value
                // below it does not silently become something else over the
                // video.
                trail_ttl_ms: (trails.ttl.as_millis() as u32).max(500),
                trail_line: trails.mode == TrailMode::Line,
                teleport_threshold: trails.teleport_threshold.clamp(0.05, 2.0),
                stops: stops_flat(&self.volume_settings.object_stops),
            },
        );
    }
}
