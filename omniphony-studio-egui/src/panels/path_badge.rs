//! The viewport's badge: what renders now — the path and the set in force —
//! and whether that is a fallback. The words come from the core
//! (`model::binaural::path_badge`); a click opens the listening settings.
//! It sits at the bottom left of the viewport, level with the scene-fx bar,
//! so the Essentials view reads the path without the Advanced board.

use egui::{Align2, RichText, Sense};

use crate::app::StudioSpike;
use crate::i18n::t;
use crate::model::binaural::path_badge;
use crate::panels::renderer::RendererTab;
use crate::ui::layout::{OverlayLayout, Side};
use crate::ui::{section, theme};

/// Room between the left overlay and the badge.
const SIDE_GAP: f32 = 16.0;

impl StudioSpike {
    /// `layout` is the frame's working copy of the side panels, written
    /// back by the caller: the click uncollapses the right overlay through
    /// it, since a request to open a section goes nowhere while the panel
    /// holding it is not drawn.
    pub(crate) fn path_badge(&mut self, ctx: &egui::Context, layout: &mut OverlayLayout) {
        let badge = {
            let live = self.host.read();
            path_badge(&live.app)
        };
        let left = layout.effective_width(crate::ui::layout::Side::Left) + SIDE_GAP;
        let clicked = egui::Area::new(egui::Id::new("path-badge"))
            .anchor(Align2::LEFT_BOTTOM, [left, super::scene_fx::BOTTOM_OFFSET])
            .order(egui::Order::Middle)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(super::scene_fx::BAR_BG)
                    .stroke(egui::Stroke::new(1.0, super::scene_fx::BAR_EDGE))
                    .corner_radius(10)
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.horizontal(|ui| {
                            let mut response = ui.add(
                                egui::Label::new(
                                    RichText::new(&badge.text)
                                        .size(theme::FONT_SIZE_SMALL)
                                        .color(theme::TEXT),
                                )
                                .sense(Sense::click()),
                            );
                            // A fallback is said in the warn colour; what
                            // went wrong is on hover.
                            match &badge.warning {
                                Some(warning) => {
                                    let warn = ui.add(
                                        egui::Label::new(
                                            RichText::new(t("badge.fallback"))
                                                .size(theme::FONT_SIZE_SMALL)
                                                .color(theme::WARN),
                                        )
                                        .sense(Sense::click()),
                                    );
                                    response = (response | warn).on_hover_text(warning);
                                }
                                None => response = response.on_hover_text(t("badge.hint")),
                            }
                            response
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .clicked()
                        })
                        .inner
                    })
                    .inner
            })
            .inner;
        if clicked {
            // The listening settings: the Essentials section, and the
            // Renderer section on its Binaural tab for the Advanced board —
            // in the right overlay, opened first when it is collapsed.
            if layout.right.collapsed {
                layout.toggle_collapsed(Side::Right, ctx.content_rect().width());
            }
            section::request_open(ctx, "listeningSection");
            section::request_open(ctx, "rendererSection");
            self.renderer_tab = RendererTab::Binaural;
        }
    }
}
