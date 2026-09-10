//! The save footer and the band cursor (`.save-footer`, `#bandCursor`,
//! `controls/config.js`, `controls/band-cursor.js`, `design.md` §1.5–1.6).
//!
//! Both float over the viewport in screen coordinates rather than inside a
//! panel: the footer is centred at the bottom because saving is about the whole
//! session and not about whichever panel is open, and the band cursor sits
//! against the right overlay because it filters what the scene draws.

use egui::{Align2, Color32, Ui, vec2};

use crate::app::StudioSpike;
use crate::i18n::t;
use crate::panels::row_glyphs::{band_colour, band_labels};
use crate::ui::layout::{COLLAPSED_WIDTH, OverlayLayout};
use crate::ui::theme;

/// The glass both pieces are made of (`rgba(18,22,28,0.72)` over a blur egui
/// cannot do, so the fill is opaque enough to read against the scene).
const GLASS: Color32 = Color32::from_rgba_premultiplied(13, 16, 20, 184);
const GLASS_EDGE: Color32 = Color32::from_rgba_premultiplied(20, 20, 20, 20);
/// `#bandCursor` segments.
const SEG: egui::Vec2 = egui::vec2(14.0, 30.0);
const SEG_ALL: egui::Vec2 = egui::vec2(14.0, 16.0);
/// `right: calc(var(--panel-width-right) + 2.6rem)`.
const CURSOR_GAP: f32 = 41.6;

impl StudioSpike {
    /// Save and Reload, with what the renderer last said about the file.
    ///
    /// The indicator is the point of the footer: a renderer whose live state
    /// has drifted from its configuration file will come back as the file after
    /// a restart, and nothing else on screen says so.
    pub(crate) fn save_footer(&mut self, ctx: &egui::Context) {
        let (saved, error, pending) = {
            let live = self.live.lock().unwrap();
            (
                live.app.config_saved.unwrap_or(0) != 0,
                live.app.save_error.clone(),
                live.save_requested,
            )
        };
        egui::Area::new(egui::Id::new("save-footer"))
            .anchor(Align2::CENTER_BOTTOM, vec2(0.0, -theme::PANEL_EDGE_MARGIN))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(GLASS)
                    .stroke(egui::Stroke::new(1.0, GLASS_EDGE))
                    .corner_radius(14)
                    .inner_margin(egui::Margin::symmetric(8, 6))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            self.save_indicator(ui, saved, error.as_deref(), pending);
                            if ui.button(t("config.save")).clicked() {
                                self.live.lock().unwrap().save_requested = true;
                                self.ctl.send_no_args("/omniphony/control/save_config");
                            }
                            if ui.button(t("config.reload")).clicked() {
                                self.ctl.send_no_args("/omniphony/control/reload_config");
                            }
                        });
                    });
            });
    }

    fn save_indicator(&self, ui: &mut Ui, saved: bool, error: Option<&str>, pending: bool) {
        // An error outranks everything: it means the renderer tried and could
        // not, which is the one state a "Modified" label would hide.
        if let Some(error) = error.filter(|e| !e.trim().is_empty()) {
            ui.label(
                egui::RichText::new(error)
                    .size(theme::FONT_SIZE_SMALL)
                    .color(theme::ERROR),
            )
            .on_hover_text(error);
            return;
        }
        let (text, colour) = match (pending, saved) {
            (true, _) => ("…", theme::TEXT_MUTED),
            (false, true) => (t("config.saved"), theme::OK),
            (false, false) => (t("config.modified"), theme::WARN),
        };
        ui.label(
            egui::RichText::new(text)
                .size(theme::FONT_SIZE_SMALL)
                .color(colour),
        );
    }

    /// The band cursor: pick which crossover band the scene's heatmaps and
    /// effective-render centroid are drawn for.
    ///
    /// Hidden below two bands, where there is nothing to choose between — a
    /// one-segment cursor would be a control that cannot do anything.
    pub(crate) fn band_cursor(&mut self, ctx: &egui::Context, layout: &OverlayLayout) {
        let cutoffs = {
            let live = self.live.lock().unwrap();
            crate::model::layouts::crossover_cutoffs(&live.selected_speakers())
        };
        let count = cutoffs.len() + 1;
        if count < 2 {
            return;
        }
        let labels = band_labels(&cutoffs);
        let right = if layout.right.collapsed {
            COLLAPSED_WIDTH
        } else {
            layout.right.width
        };
        let selected = self.settings.heatmap_band_index;
        egui::Area::new(egui::Id::new("band-cursor"))
            .anchor(Align2::RIGHT_CENTER, vec2(-(right + CURSOR_GAP), 0.0))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(GLASS)
                    .stroke(egui::Stroke::new(1.0, GLASS_EDGE))
                    .corner_radius(14)
                    .inner_margin(egui::Margin::symmetric(7, 8))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 5.0;
                        ui.vertical_centered(|ui| {
                            // "All" caps the stack, then the bands run highest
                            // frequency first: the cursor reads like a spectrum
                            // stood on end.
                            if segment(ui, SEG_ALL, all_bands_colour(), selected >= count)
                                .on_hover_text(t("heatmap.bandAll"))
                                .clicked()
                            {
                                self.settings.heatmap_band_index = count;
                            }
                            for band in (0..count).rev() {
                                let label = labels
                                    .get(band)
                                    .cloned()
                                    .unwrap_or_else(|| t("heatmap.bandFull").to_owned());
                                if segment(ui, SEG, band_colour(band, count), selected == band)
                                    .on_hover_text(label)
                                    .clicked()
                                {
                                    self.settings.heatmap_band_index = band;
                                }
                            }
                        });
                    });
            });
    }
}

/// The "All bands" cap: every band's colour bottom-up, so it reads as the
/// whole spectrum rather than as another band.
fn all_bands_colour() -> Color32 {
    Color32::from_rgba_unmultiplied(223, 232, 243, 204)
}

/// One segment. Unselected segments sit at 45 % so the chosen one is the only
/// thing the eye lands on.
fn segment(ui: &mut Ui, size: egui::Vec2, colour: Color32, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let alpha = if selected {
            1.0
        } else if response.hovered() {
            0.85
        } else {
            0.45
        };
        let radius = if size.y > 20.0 { 7 } else { 8 };
        ui.painter().rect(
            rect,
            radius,
            colour.gamma_multiply(alpha),
            egui::Stroke::new(
                1.0,
                if selected {
                    Color32::from_white_alpha(178)
                } else {
                    Color32::from_white_alpha(26)
                },
            ),
            egui::StrokeKind::Inside,
        );
    }
    response
}
