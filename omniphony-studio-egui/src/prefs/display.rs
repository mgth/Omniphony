//! The Display panel's settings, remembered across launches.
//!
//! The web keeps them in two `localStorage` entries — the effective-render
//! prefs (`persistEffectiveRenderPrefs`) and the trail prefs
//! (`persistTrailPrefs`) — and restores them at start-up. The native Studio
//! remembered none of it, so every launch came up on the defaults: a user who
//! had turned on "aim the speakers at the listener" in the web found the
//! speakers square to the room and without their driver face here, because the
//! driver is only drawn while the speakers face the listener.
//!
//! The keys and the value spellings are the web's, so the file reads the same
//! as what the web stores. Every field is optional: a missing one leaves the
//! default alone, and clamps follow the web's loader.

use serde::{Deserialize, Serialize};

use crate::view::ViewSettings;
use crate::view::objects::ObjectDisplayMode;
use crate::view::trails::TrailMode;
use crate::view::volumes::{Colormap, DiscontinuityMode, GradientStop, VolumeSettings};

/// `{ pos, r, g, b }`, the web's gradient stop.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct StopPref {
    pub pos: f32,
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

impl From<&GradientStop> for StopPref {
    fn from(s: &GradientStop) -> Self {
        Self {
            pos: s.pos,
            r: s.rgb[0],
            g: s.rgb[1],
            b: s.rgb[2],
        }
    }
}

impl From<&StopPref> for GradientStop {
    fn from(s: &StopPref) -> Self {
        Self {
            pos: s.pos.clamp(0.0, 1.0),
            rgb: [
                s.r.clamp(0.0, 1.0),
                s.g.clamp(0.0, 1.0),
                s.b.clamp(0.0, 1.0),
            ],
        }
    }
}

/// `persistTrailPrefs`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrailPrefs {
    pub enabled: Option<bool>,
    pub mode: Option<TrailMode>,
    pub duration_ms: Option<u64>,
    pub teleport_threshold: Option<f32>,
}

/// `persistEffectiveRenderPrefs`, as far as the native Studio has the
/// setting. (`showObjectDetails` and `objectEnergyHeatmapBandCount` have no
/// counterpart here and are not written.)
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DisplayPrefs {
    pub enabled: Option<bool>,
    pub objects_visible: Option<bool>,
    pub object_colors: Option<bool>,
    pub object_display_mode: Option<ObjectDisplayMode>,
    pub object_sphere_size: Option<f32>,
    pub object_labels: Option<bool>,
    pub show_object_details: Option<bool>,
    pub speaker_labels: Option<bool>,
    pub speaker_bands: Option<bool>,
    pub speaker_face_listener: Option<bool>,
    pub speaker_size: Option<f32>,
    pub speaker_heatmap_volume_enabled: Option<bool>,
    pub speaker_heatmap_volume_colormap: Option<Colormap>,
    pub heatmap_band_index: Option<usize>,
    pub heatmap_all_bands: Option<bool>,
    pub global_energy_heatmap_enabled: Option<bool>,
    pub global_energy_heatmap_scale_db: Option<f32>,
    pub discontinuity_heatmap_enabled: Option<bool>,
    pub discontinuity_heatmap_mode: Option<DiscontinuityMode>,
    pub discontinuity_heatmap_scale: Option<f32>,
    pub object_energy_heatmap_enabled: Option<bool>,
    pub object_energy_colormap: Option<Colormap>,
    pub object_energy_volume_mix: Option<f32>,
    pub object_energy_volume_gamma_accumulate: Option<f32>,
    pub object_energy_volume_gamma_mip: Option<f32>,
    pub object_energy_heatmap_resolution: Option<u32>,
    pub object_energy_heatmap_falloff_radius: Option<f32>,
    pub object_energy_heatmap_opacity: Option<f32>,
    pub volume_refresh_ms: Option<u32>,
    pub volume_smooth_interpolation: Option<bool>,
    pub object_custom_gradient_stops: Option<Vec<StopPref>>,
    pub speaker_custom_gradient_stops: Option<Vec<StopPref>>,
    pub trails: TrailPrefs,
}

fn stops_eq(prefs: &Option<Vec<StopPref>>, stops: &[GradientStop]) -> bool {
    prefs.as_ref().is_some_and(|p| {
        p.len() == stops.len() && p.iter().zip(stops).all(|(a, b)| *a == StopPref::from(b))
    })
}

/// A readable gradient needs at least two stops; the web keeps 2..8.
fn stops_from(prefs: &[StopPref]) -> Option<Vec<GradientStop>> {
    (2..=8)
        .contains(&prefs.len())
        .then(|| prefs.iter().map(GradientStop::from).collect())
}

impl DisplayPrefs {
    /// What the settings are now, in the file's shape.
    pub fn capture(settings: &ViewSettings, volume: &VolumeSettings) -> Self {
        Self {
            enabled: Some(settings.effective_render_enabled),
            objects_visible: Some(settings.objects_visible),
            object_colors: Some(settings.object_colors_enabled),
            object_display_mode: Some(settings.object_display_mode),
            object_sphere_size: Some(settings.object_sphere_size),
            object_labels: Some(settings.object_labels_enabled),
            show_object_details: Some(settings.show_object_details),
            speaker_labels: Some(settings.speaker_labels_enabled),
            speaker_bands: Some(settings.speaker_band_bars_enabled),
            speaker_face_listener: Some(settings.speaker_face_listener_enabled),
            speaker_size: Some(settings.speaker_size),
            speaker_heatmap_volume_enabled: Some(volume.speaker_enabled),
            speaker_heatmap_volume_colormap: Some(volume.speaker_colormap),
            heatmap_band_index: Some(settings.heatmap_band_index),
            heatmap_all_bands: Some(volume.all_bands),
            global_energy_heatmap_enabled: Some(volume.global_enabled),
            global_energy_heatmap_scale_db: Some(volume.global_scale_db),
            discontinuity_heatmap_enabled: Some(volume.discontinuity_enabled),
            discontinuity_heatmap_mode: Some(volume.discontinuity_mode),
            discontinuity_heatmap_scale: Some(volume.discontinuity_scale),
            object_energy_heatmap_enabled: Some(volume.object_field_enabled),
            object_energy_colormap: Some(volume.object_colormap),
            object_energy_volume_mix: Some(volume.mix),
            object_energy_volume_gamma_accumulate: Some(volume.gamma_accumulate),
            object_energy_volume_gamma_mip: Some(volume.gamma_mip),
            object_energy_heatmap_resolution: Some(volume.resolution),
            object_energy_heatmap_falloff_radius: Some(volume.object_radius),
            object_energy_heatmap_opacity: Some(volume.opacity),
            volume_refresh_ms: Some(volume.refresh_ms),
            volume_smooth_interpolation: Some(volume.smooth),
            object_custom_gradient_stops: Some(
                volume.object_stops.iter().map(StopPref::from).collect(),
            ),
            speaker_custom_gradient_stops: Some(
                volume.speaker_stops.iter().map(StopPref::from).collect(),
            ),
            trails: TrailPrefs {
                enabled: Some(settings.trails.enabled),
                mode: Some(settings.trails.mode),
                duration_ms: Some(settings.trails.ttl.as_millis() as u64),
                teleport_threshold: Some(settings.trails.teleport_threshold),
            },
        }
    }

    /// True when the file already says what the settings are. Compared field
    /// by field so the per-frame check allocates nothing; `capture` only runs
    /// when this says a setting moved.
    pub fn matches(&self, settings: &ViewSettings, volume: &VolumeSettings) -> bool {
        self.enabled == Some(settings.effective_render_enabled)
            && self.objects_visible == Some(settings.objects_visible)
            && self.object_colors == Some(settings.object_colors_enabled)
            && self.object_display_mode == Some(settings.object_display_mode)
            && self.object_sphere_size == Some(settings.object_sphere_size)
            && self.object_labels == Some(settings.object_labels_enabled)
            && self.show_object_details == Some(settings.show_object_details)
            && self.speaker_labels == Some(settings.speaker_labels_enabled)
            && self.speaker_bands == Some(settings.speaker_band_bars_enabled)
            && self.speaker_face_listener == Some(settings.speaker_face_listener_enabled)
            && self.speaker_size == Some(settings.speaker_size)
            && self.speaker_heatmap_volume_enabled == Some(volume.speaker_enabled)
            && self.speaker_heatmap_volume_colormap == Some(volume.speaker_colormap)
            && self.heatmap_band_index == Some(settings.heatmap_band_index)
            && self.heatmap_all_bands == Some(volume.all_bands)
            && self.global_energy_heatmap_enabled == Some(volume.global_enabled)
            && self.global_energy_heatmap_scale_db == Some(volume.global_scale_db)
            && self.discontinuity_heatmap_enabled == Some(volume.discontinuity_enabled)
            && self.discontinuity_heatmap_mode == Some(volume.discontinuity_mode)
            && self.discontinuity_heatmap_scale == Some(volume.discontinuity_scale)
            && self.object_energy_heatmap_enabled == Some(volume.object_field_enabled)
            && self.object_energy_colormap == Some(volume.object_colormap)
            && self.object_energy_volume_mix == Some(volume.mix)
            && self.object_energy_volume_gamma_accumulate == Some(volume.gamma_accumulate)
            && self.object_energy_volume_gamma_mip == Some(volume.gamma_mip)
            && self.object_energy_heatmap_resolution == Some(volume.resolution)
            && self.object_energy_heatmap_falloff_radius == Some(volume.object_radius)
            && self.object_energy_heatmap_opacity == Some(volume.opacity)
            && self.volume_refresh_ms == Some(volume.refresh_ms)
            && self.volume_smooth_interpolation == Some(volume.smooth)
            && stops_eq(&self.object_custom_gradient_stops, &volume.object_stops)
            && stops_eq(&self.speaker_custom_gradient_stops, &volume.speaker_stops)
            && self.trails.enabled == Some(settings.trails.enabled)
            && self.trails.mode == Some(settings.trails.mode)
            && self.trails.duration_ms == Some(settings.trails.ttl.as_millis() as u64)
            && self.trails.teleport_threshold == Some(settings.trails.teleport_threshold)
    }

    /// Restore what the file holds, with the web loader's clamps.
    pub fn apply(&self, settings: &mut ViewSettings, volume: &mut VolumeSettings) {
        macro_rules! set {
            ($field:expr, $value:expr) => {
                if let Some(v) = $value {
                    $field = v;
                }
            };
        }
        set!(settings.effective_render_enabled, self.enabled);
        set!(settings.objects_visible, self.objects_visible);
        set!(settings.object_colors_enabled, self.object_colors);
        set!(settings.object_display_mode, self.object_display_mode);
        set!(
            settings.object_sphere_size,
            self.object_sphere_size
                .filter(|v| v.is_finite())
                .map(|v| v.clamp(0.03, 0.2))
        );
        set!(settings.object_labels_enabled, self.object_labels);
        set!(settings.show_object_details, self.show_object_details);
        set!(settings.speaker_labels_enabled, self.speaker_labels);
        set!(settings.speaker_band_bars_enabled, self.speaker_bands);
        set!(
            settings.speaker_face_listener_enabled,
            self.speaker_face_listener
        );
        set!(
            settings.speaker_size,
            self.speaker_size
                .filter(|v| v.is_finite())
                .map(|v| v.clamp(0.04, 0.2))
        );
        set!(volume.speaker_enabled, self.speaker_heatmap_volume_enabled);
        set!(
            volume.speaker_colormap,
            self.speaker_heatmap_volume_colormap
        );
        if let Some(band) = self.heatmap_band_index {
            settings.heatmap_band_index = band;
            volume.band_index = band;
        }
        set!(volume.all_bands, self.heatmap_all_bands);
        set!(volume.global_enabled, self.global_energy_heatmap_enabled);
        set!(
            volume.global_scale_db,
            self.global_energy_heatmap_scale_db
                .filter(|v| v.is_finite())
                .map(|v| v.clamp(1.0, 40.0))
        );
        set!(
            volume.discontinuity_enabled,
            self.discontinuity_heatmap_enabled
        );
        set!(volume.discontinuity_mode, self.discontinuity_heatmap_mode);
        set!(
            volume.discontinuity_scale,
            self.discontinuity_heatmap_scale
                .filter(|v| v.is_finite())
                .map(|v| v.clamp(0.05, 2.0))
        );
        set!(
            volume.object_field_enabled,
            self.object_energy_heatmap_enabled
        );
        set!(volume.object_colormap, self.object_energy_colormap);
        set!(
            volume.mix,
            self.object_energy_volume_mix
                .filter(|v| v.is_finite())
                .map(|v| v.clamp(0.0, 1.0))
        );
        set!(
            volume.gamma_accumulate,
            self.object_energy_volume_gamma_accumulate
                .filter(|v| v.is_finite())
                .map(|v| v.clamp(1.0, 10.0))
        );
        set!(
            volume.gamma_mip,
            self.object_energy_volume_gamma_mip
                .filter(|v| v.is_finite())
                .map(|v| v.clamp(0.2, 3.0))
        );
        set!(
            volume.resolution,
            self.object_energy_heatmap_resolution
                .map(|v| v.clamp(8, 64))
        );
        set!(
            volume.object_radius,
            self.object_energy_heatmap_falloff_radius
                .filter(|v| v.is_finite())
                .map(|v| v.clamp(0.02, 0.5))
        );
        set!(
            volume.opacity,
            self.object_energy_heatmap_opacity
                .filter(|v| v.is_finite())
                .map(|v| v.clamp(0.05, 1.0))
        );
        set!(
            volume.refresh_ms,
            self.volume_refresh_ms.map(|v| v.clamp(40, 500))
        );
        set!(volume.smooth, self.volume_smooth_interpolation);
        if let Some(stops) = self
            .object_custom_gradient_stops
            .as_deref()
            .and_then(stops_from)
        {
            volume.object_stops = stops;
        }
        if let Some(stops) = self
            .speaker_custom_gradient_stops
            .as_deref()
            .and_then(stops_from)
        {
            volume.speaker_stops = stops;
        }
        set!(settings.trails.enabled, self.trails.enabled);
        set!(settings.trails.mode, self.trails.mode);
        if let Some(ms) = self.trails.duration_ms {
            // `trailPointTtlMs`, clamped ≥ 500 ms as the trail code does.
            settings.trails.ttl = std::time::Duration::from_millis(ms.max(500));
        }
        set!(
            settings.trails.teleport_threshold,
            self.trails
                .teleport_threshold
                .filter(|v| v.is_finite() && *v >= 0.0)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What goes out comes back: capture, write, read, apply, and the settings
    /// are the ones that were captured.
    #[test]
    fn settings_survive_the_round_trip() {
        let mut settings = ViewSettings::default();
        let mut volume = VolumeSettings::default();
        settings.speaker_face_listener_enabled = true;
        settings.speaker_size = 0.12;
        settings.show_object_details = false;
        settings.object_display_mode = ObjectDisplayMode::DiffuseSphere;
        settings.trails.mode = TrailMode::Line;
        volume.object_colormap = Colormap::WhiteRed;
        volume.discontinuity_mode = DiscontinuityMode::Centroid;
        let json = serde_json::to_string(&DisplayPrefs::capture(&settings, &volume)).unwrap();
        let back: DisplayPrefs = serde_json::from_str(&json).unwrap();
        let mut s2 = ViewSettings::default();
        let mut v2 = VolumeSettings::default();
        back.apply(&mut s2, &mut v2);
        assert!(back.matches(&s2, &v2));
        assert!(s2.speaker_face_listener_enabled);
        assert_eq!(s2.speaker_size, 0.12);
        assert!(!s2.show_object_details);
        assert_eq!(s2.object_display_mode, ObjectDisplayMode::DiffuseSphere);
        assert_eq!(s2.trails.mode, TrailMode::Line);
        assert_eq!(v2.object_colormap, Colormap::WhiteRed);
        assert_eq!(v2.discontinuity_mode, DiscontinuityMode::Centroid);
    }

    /// The file speaks the web's language: its keys and its value spellings.
    #[test]
    fn the_file_uses_the_web_keys_and_values() {
        let mut settings = ViewSettings::default();
        settings.object_display_mode = ObjectDisplayMode::TransparentSphere;
        settings.speaker_face_listener_enabled = true;
        let mut volume = VolumeSettings::default();
        volume.object_colormap = Colormap::BlueWhite;
        let json = serde_json::to_string(&DisplayPrefs::capture(&settings, &volume)).unwrap();
        for needle in [
            "\"speakerFaceListener\":true",
            "\"objectDisplayMode\":\"transparent-sphere\"",
            "\"objectEnergyColormap\":\"blueWhite\"",
            "\"duration_ms\"",
        ] {
            assert!(json.contains(needle), "{needle} missing from {json}");
        }
    }

    /// A partial or older file only touches what it names, and a value out of
    /// range is clamped the way the web's loader clamps it.
    #[test]
    fn a_partial_file_leaves_the_rest_alone() {
        let prefs: DisplayPrefs =
            serde_json::from_str(r#"{"speakerFaceListener":true,"speakerSize":9.0}"#).unwrap();
        let mut settings = ViewSettings::default();
        let mut volume = VolumeSettings::default();
        let before = settings.object_sphere_size;
        prefs.apply(&mut settings, &mut volume);
        assert!(settings.speaker_face_listener_enabled);
        assert_eq!(settings.speaker_size, 0.2);
        assert_eq!(settings.object_sphere_size, before);
    }
}
