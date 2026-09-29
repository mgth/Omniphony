//! Where the Studio was left: the camera, the window, the open sections, the
//! tabs, the speaker-test settings. View state is kept the moment it changes
//! (docs/persistence-policy.md), so a restart — or a crash — comes back to
//! the same view rather than to the defaults.
//!
//! The file holds what the user chose; every field is optional and a missing
//! one leaves the default alone, so an older file still loads.

use serde::{Deserialize, Serialize};

use crate::app::StudioSpike;
use crate::host::channels::CoordMode;
use crate::panels::renderer::RendererTab;
use crate::panels::speaker_editor::SpeakerTab;
use crate::render::camera::OrbitCamera;
use crate::ui::section::OpenStates;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewPrefs {
    pub camera: Option<CameraPrefs>,
    pub window: Option<WindowPrefs>,
    /// Open (`true`) or folded, by section id.
    pub sections: OpenStates,
    pub renderer_tab: Option<RendererTab>,
    pub speaker_tab: Option<SpeakerTab>,
    pub hybrid_tab: Option<String>,
    /// The channel editor's coordinate view (`cartesian` / `polar`).
    pub channel_coord_mode: Option<String>,
    pub display_panel_open: Option<bool>,
    pub log_expanded: Option<bool>,
    pub resample_plot_open: Option<bool>,
    pub speaker_test: SpeakerTestPrefs,
}

/// The orbit camera at rest.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CameraPrefs {
    pub target: [f32; 3],
    pub theta: f32,
    pub phi: f32,
    pub radius: f32,
    pub pan: [f32; 2],
}

impl CameraPrefs {
    fn of(camera: &OrbitCamera) -> Self {
        Self {
            target: camera.target.to_array(),
            theta: camera.theta,
            phi: camera.phi,
            radius: camera.radius,
            pan: camera.pan,
        }
    }

    fn apply(&self, camera: &mut OrbitCamera) {
        let finite = self.target.iter().chain(&self.pan).all(|v| v.is_finite())
            && [self.theta, self.phi, self.radius]
                .iter()
                .all(|v| v.is_finite());
        if !finite {
            return;
        }
        camera.target = glam::Vec3::from_array(self.target);
        camera.theta = self.theta;
        camera.phi = self.phi.clamp(0.01, std::f32::consts::PI - 0.01);
        camera.radius = self.radius.clamp(0.05, 200.0);
        camera.pan = self.pan;
    }
}

/// The window, in points. The position is absent where the platform does not
/// tell it (Wayland).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowPrefs {
    pub size: [f32; 2],
    pub position: Option<[f32; 2]>,
    pub maximized: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SpeakerTestPrefs {
    pub mode: Option<String>,
    pub isolation: Option<String>,
    pub level_db: Option<f32>,
}

/// Smallest window a saved size may restore, so a bad file cannot open an
/// unusable sliver.
const MIN_WINDOW: [f32; 2] = [640.0, 400.0];

impl StudioSpike {
    /// Put the saved view back, before the first frame.
    pub(crate) fn restore_view(&mut self, ctx: &egui::Context) {
        let view = self.prefs.view.clone();
        if let Some(camera) = &view.camera {
            camera.apply(&mut self.camera);
        }
        if let Some(window) = &view.window {
            if !window.maximized
                && window.size.iter().all(|v| v.is_finite())
                && window.size[0] >= MIN_WINDOW[0]
                && window.size[1] >= MIN_WINDOW[1]
            {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                    window.size[0],
                    window.size[1],
                )));
            }
            if let Some([x, y]) = window.position.filter(|p| p.iter().all(|v| v.is_finite())) {
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(x, y)));
            }
            if window.maximized {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
            }
        }
        crate::ui::section::seed_open_states(ctx, view.sections);
        if let Some(tab) = view.renderer_tab {
            self.renderer_tab = tab;
        }
        if let Some(tab) = view.speaker_tab {
            self.speaker_tab = tab;
        }
        if let Some(tab) = view.hybrid_tab {
            self.hybrid_tab = tab;
        }
        match view.channel_coord_mode.as_deref() {
            Some("polar") => self.channel_coord_mode = CoordMode::Polar,
            Some("cartesian") => self.channel_coord_mode = CoordMode::Cartesian,
            _ => {}
        }
        if let Some(open) = view.display_panel_open {
            self.display_panel_open = open;
        }
        if let Some(open) = view.log_expanded {
            self.log_expanded = open;
        }
        if let Some(open) = view.resample_plot_open {
            self.resample_plot_open = open;
        }
        if let Some(mode) = view.speaker_test.mode {
            self.speaker_test_mode = mode;
        }
        if let Some(isolation) = view.speaker_test.isolation {
            self.speaker_test_isolation = isolation;
        }
        if let Some(level) = view.speaker_test.level_db.filter(|v| v.is_finite()) {
            self.speaker_test_level_db = level.clamp(-60.0, 0.0);
        }
    }

    /// Note what moved since the last frame, and mark the preferences for
    /// their debounced write if anything did. Nothing is allocated unless a
    /// setting actually changed. The camera is taken at rest, not on every
    /// frame of a glide.
    pub(crate) fn remember_view(&mut self, ctx: &egui::Context, camera_moving: bool) {
        let mut changed = false;
        let view = &mut self.prefs.view;
        macro_rules! keep {
            ($field:expr, $value:expr) => {
                if $field.as_ref() != Some(&$value) {
                    $field = Some($value);
                    changed = true;
                }
            };
        }
        if !camera_moving {
            keep!(view.camera, CameraPrefs::of(&self.camera));
        }
        if let Some(window) = ctx.input(|i| {
            let info = i.viewport();
            let maximized = info.maximized.unwrap_or(false);
            let size = info.inner_rect.map(|r| [r.width(), r.height()]);
            let position = info.outer_rect.map(|r| [r.min.x, r.min.y]);
            if info.minimized == Some(true) || info.fullscreen == Some(true) {
                return None;
            }
            size.map(|size| (size, position, maximized))
        }) {
            let (size, position, maximized) = window;
            // A maximised window keeps the size it will return to.
            let size = match (&view.window, maximized) {
                (Some(saved), true) => saved.size,
                _ => size,
            };
            keep!(
                view.window,
                WindowPrefs {
                    size,
                    position,
                    maximized,
                }
            );
        }
        if let Some(sections) = crate::ui::section::take_changed_open_states(ctx) {
            view.sections = sections;
            changed = true;
        }
        keep!(view.renderer_tab, self.renderer_tab);
        keep!(view.speaker_tab, self.speaker_tab);
        if view.hybrid_tab.as_deref() != Some(self.hybrid_tab.as_str()) {
            view.hybrid_tab = Some(self.hybrid_tab.clone());
            changed = true;
        }
        let coord = self.channel_coord_mode.as_str();
        if view.channel_coord_mode.as_deref() != Some(coord) {
            view.channel_coord_mode = Some(coord.to_owned());
            changed = true;
        }
        keep!(view.display_panel_open, self.display_panel_open);
        keep!(view.log_expanded, self.log_expanded);
        keep!(view.resample_plot_open, self.resample_plot_open);
        let test = &mut view.speaker_test;
        if test.mode.as_deref() != Some(self.speaker_test_mode.as_str()) {
            test.mode = Some(self.speaker_test_mode.clone());
            changed = true;
        }
        if test.isolation.as_deref() != Some(self.speaker_test_isolation.as_str()) {
            test.isolation = Some(self.speaker_test_isolation.clone());
            changed = true;
        }
        keep!(test.level_db, self.speaker_test_level_db);
        if changed {
            self.mark_prefs_dirty();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_camera_survives_the_round_trip() {
        let mut camera = OrbitCamera::new();
        camera.theta = 1.2;
        camera.phi = 0.8;
        camera.radius = 7.5;
        camera.pan = [12.0, -4.0];
        let json = serde_json::to_string(&CameraPrefs::of(&camera)).unwrap();
        let back: CameraPrefs = serde_json::from_str(&json).unwrap();
        let mut restored = OrbitCamera::new();
        back.apply(&mut restored);
        assert_eq!(CameraPrefs::of(&restored), CameraPrefs::of(&camera));
    }

    #[test]
    fn a_broken_camera_leaves_the_default() {
        let mut camera = OrbitCamera::new();
        let before = CameraPrefs::of(&camera);
        CameraPrefs {
            radius: f32::NAN,
            ..before
        }
        .apply(&mut camera);
        assert_eq!(CameraPrefs::of(&camera), before);
    }

    /// An older file, or one from before a field existed, still loads.
    #[test]
    fn a_partial_file_loads() {
        let view: ViewPrefs =
            serde_json::from_str(r#"{"renderer_tab":"binaural","sections":{"diagSection":true}}"#)
                .unwrap();
        assert_eq!(view.renderer_tab, Some(RendererTab::Binaural));
        assert_eq!(view.sections.get("diagSection"), Some(&true));
        assert!(view.camera.is_none());
    }
}
