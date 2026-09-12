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
use crate::host::commands::mpv_overlay as cmd;
use crate::view::trails::TrailMode;
use crate::view::volumes::GradientStop;

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
    /// The custom gradient, as one number. The stops themselves are a list, so
    /// they cannot sit in a `Copy` value the whole struct is compared by; what
    /// matters here is only whether they changed since the last push.
    stops_signature: u64,
}

/// The custom stops as the renderer takes them: `[pos, r, g, b, …]`.
fn stops_flat(stops: &[GradientStop]) -> Vec<f32> {
    stops
        .iter()
        .flat_map(|stop| [stop.pos, stop.rgb[0], stop.rgb[1], stop.rgb[2]])
        .collect()
}

/// A signature of the flattened stops. Bit patterns rather than floats so the
/// value is hashable, and so a stop moved by a pixel is a change.
fn stops_signature(flat: &[f32]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    flat.len().hash(&mut hasher);
    for value in flat {
        value.to_bits().hash(&mut hasher);
    }
    hasher.finish()
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
        cmd::mpv_overlay_set_objects(&self.host, wanted.objects);
        cmd::mpv_overlay_set_labels(&self.host, wanted.labels);
        cmd::mpv_overlay_set_heatmap_enabled(&self.host, wanted.heatmap);
        cmd::mpv_overlay_set_heatmap_bands(&self.host, wanted.bands);
        cmd::mpv_overlay_set_heatmap_colormap(&self.host, wanted.colormap);
        // The custom stops go with the colormap that uses them: pushing the
        // colormap without them would show the overlay's own gradient under
        // Studio's choice of "Custom".
        cmd::mpv_overlay_set_heatmap_custom_stops(
            &self.host,
            stops_flat(&self.volume_settings.object_stops),
        );
        cmd::mpv_overlay_set_trail_prefs(
            &self.host,
            wanted.trails,
            wanted.trail_ttl_ms as u32,
            if wanted.trail_line { "line" } else { "diffuse" }.to_owned(),
            wanted.teleport_threshold,
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
            stops_signature: stops_signature(&stops_flat(&self.volume_settings.object_stops)),
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
            stops_signature: 0,
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

    /// The gradient is mirrored too, so moving a stop has to reach the push.
    #[test]
    fn a_moved_stop_is_a_change() {
        let stops = vec![
            GradientStop {
                pos: 0.0,
                rgb: [0.0, 0.0, 1.0],
            },
            GradientStop {
                pos: 1.0,
                rgb: [1.0, 0.0, 0.0],
            },
        ];
        let flat = stops_flat(&stops);
        assert_eq!(flat, vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0, 0.0]);
        let mut moved = stops.clone();
        moved[0].pos = 0.01;
        assert_ne!(stops_signature(&flat), stops_signature(&stops_flat(&moved)));
        // Dropping a stop is a change even when the remaining numbers repeat.
        assert_ne!(
            stops_signature(&flat),
            stops_signature(&stops_flat(&stops[..1]))
        );
    }
}
