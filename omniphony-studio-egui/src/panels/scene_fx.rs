//! The scene-effects bar (`#sceneEffectBar`, `controls/scene-effects-bar.js`).
//!
//! Seven quick toggles pinned over the bottom of the viewport, each mirroring a
//! switch that already exists in a panel. No effect logic lives here: the bar
//! drives the same state the panel does, so a toggle made from either place is
//! the same toggle. It exists because the display switches are the ones reached
//! for most often while looking at the scene, and reaching for them should not
//! mean opening a panel over the thing being looked at.
//!
//! Two of them carry a "nature" flyout — which kind of object marker, which
//! kind of trail — for the same reason: the choice belongs with the switch that
//! turns it on.

use egui::{Align2, Color32, Ui, vec2};

use crate::app::StudioSpike;
use crate::i18n::t;
use crate::ui::theme;
use crate::view::objects::ObjectDisplayMode;
use crate::view::trails::TrailMode;

/// The glass the bar is made of, shared with the save footer.
const GLASS: Color32 = Color32::from_rgba_premultiplied(13, 16, 20, 184);
const GLASS_EDGE: Color32 = Color32::from_rgba_premultiplied(20, 20, 20, 20);
/// The bar sits above the save footer, which owns the bottom margin.
const BOTTOM_OFFSET: f32 = -56.0;

/// Which flyout is open, if any.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Flyout {
    Objects,
    Trails,
}

impl StudioSpike {
    pub(crate) fn scene_fx_bar(&mut self, ctx: &egui::Context) {
        egui::Area::new(egui::Id::new("scene-fx-bar"))
            .anchor(Align2::CENTER_BOTTOM, vec2(0.0, BOTTOM_OFFSET))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(GLASS)
                    .stroke(egui::Stroke::new(1.0, GLASS_EDGE))
                    .corner_radius(14)
                    .inner_margin(egui::Margin::symmetric(8, 6))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| self.scene_fx_buttons(ui));
                    });
            });
        self.scene_fx_flyout(ctx);
    }

    fn scene_fx_buttons(&mut self, ui: &mut Ui) {
        // A glyph rather than the web's inline SVG: rasterising SVG would mean
        // a dependency for seven icons, and the title says what each one is.
        let mut grid = self.settings.vbap_grid;
        if fx(ui, "#", &mut grid, t("sceneFx.grid")) {
            self.settings.vbap_grid = grid;
        }
        let mut objects = self.settings.objects_visible;
        if fx(ui, "●", &mut objects, t("sceneFx.objects")) {
            self.settings.objects_visible = objects;
        }
        if caret(ui, self.scene_fx_flyout_open == Some(Flyout::Objects)) {
            self.scene_fx_flyout_open = toggle(self.scene_fx_flyout_open, Flyout::Objects);
        }
        let mut labels = self.settings.object_labels_enabled;
        if fx(ui, "A", &mut labels, t("sceneFx.labels")) {
            self.settings.object_labels_enabled = labels;
        }
        let mut trails = self.settings.trails.enabled;
        if fx(ui, "~", &mut trails, t("sceneFx.trails")) {
            self.settings.trails.enabled = trails;
        }
        if caret(ui, self.scene_fx_flyout_open == Some(Flyout::Trails)) {
            self.scene_fx_flyout_open = toggle(self.scene_fx_flyout_open, Flyout::Trails);
        }
        let mut field = self.volume_settings.object_field_enabled;
        if fx(ui, "◆", &mut field, t("sceneFx.energyField")) {
            self.volume_settings.object_field_enabled = field;
        }
        let mut heatmap = self.volume_settings.speaker_enabled;
        if fx(ui, "▪", &mut heatmap, t("sceneFx.heatmap")) {
            self.volume_settings.speaker_enabled = heatmap;
        }
        // The overlay's state is the engine's, not this host's: it can be
        // toggled from an mpv keybind Studio never sees, so the button reflects
        // what the engine last published rather than a local flag.
        let mut overlay = self
            .live
            .lock()
            .unwrap()
            .overlay
            .as_ref()
            .and_then(|o| o.get("enabled"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if fx(ui, "▶", &mut overlay, t("sceneFx.mpvOverlay")) {
            self.ctl
                .send_int("/omniphony/control/overlay/enabled", i32::from(overlay));
        }
    }

    /// The nature menu. Picking a nature also turns its layer on: choosing how
    /// something should look is asking to see it.
    fn scene_fx_flyout(&mut self, ctx: &egui::Context) {
        let Some(open) = self.scene_fx_flyout_open else {
            return;
        };
        let mut close = false;
        egui::Area::new(egui::Id::new("scene-fx-flyout"))
            .anchor(Align2::CENTER_BOTTOM, vec2(0.0, BOTTOM_OFFSET - 34.0))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(GLASS)
                    .stroke(egui::Stroke::new(1.0, GLASS_EDGE))
                    .corner_radius(10)
                    .inner_margin(egui::Margin::symmetric(6, 4))
                    .show(ui, |ui| match open {
                        Flyout::Objects => {
                            let current = self.settings.object_display_mode;
                            for mode in ObjectDisplayMode::ALL {
                                if ui.selectable_label(mode == current, mode.label()).clicked() {
                                    self.settings.object_display_mode = mode;
                                    self.settings.objects_visible = true;
                                    close = true;
                                }
                            }
                        }
                        Flyout::Trails => {
                            let current = self.settings.trails.mode;
                            for mode in TrailMode::ALL {
                                if ui.selectable_label(mode == current, mode.label()).clicked() {
                                    self.settings.trails.mode = mode;
                                    self.settings.trails.enabled = true;
                                    close = true;
                                }
                            }
                        }
                    });
            });
        if close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.scene_fx_flyout_open = None;
        }
    }
}

fn toggle(current: Option<Flyout>, wanted: Flyout) -> Option<Flyout> {
    if current == Some(wanted) {
        None
    } else {
        Some(wanted)
    }
}

/// One quick toggle: lit when its layer is on.
fn fx(ui: &mut Ui, glyph: &str, on: &mut bool, title: &str) -> bool {
    let response = ui
        .selectable_label(*on, egui::RichText::new(glyph).size(theme::FONT_SIZE))
        .on_hover_text(title);
    if response.clicked() {
        *on = !*on;
        return true;
    }
    false
}

/// The caret that opens a nature menu.
///
/// Painted rather than set as a glyph: the bundled faces have no dependable
/// small triangle, and a missing glyph draws as a hollow box that reads as
/// another toggle.
fn caret(ui: &mut Ui, open: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(10.0, 18.0), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let c = rect.center();
        let colour = if open || response.hovered() {
            theme::TEXT_STRONG
        } else {
            theme::TEXT_MUTED
        };
        ui.painter().add(egui::Shape::convex_polygon(
            vec![
                c + vec2(-3.5, -1.5),
                c + vec2(3.5, -1.5),
                c + vec2(0.0, 2.5),
            ],
            colour,
            egui::Stroke::NONE,
        ));
    }
    response.clicked()
}
