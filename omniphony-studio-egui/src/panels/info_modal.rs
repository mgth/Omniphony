//! The `*.infoModal` dialogs (`modals.js`).
//!
//! A dozen sections carry a long-form explanation — what a backend is, what the
//! adaptive controller does, how the heatmaps are computed. In the web the
//! section's own title is the trigger: it gets a dotted underline and opens the
//! modal, so the thing you click is the thing you are asking about. The same
//! here, with the request travelling from the header through egui's temporary
//! data rather than through a return value, so a section stays a pure widget.

use crate::app::StudioSpike;
use crate::i18n::t;
use crate::ui::{markup, theme, widgets};

/// Where a header leaves the id of the modal it wants opened.
pub fn request_id() -> egui::Id {
    egui::Id::new("info-modal")
}

impl StudioSpike {
    pub(crate) fn info_modal(&mut self, ctx: &egui::Context) {
        let requested: Option<String> = ctx.data_mut(|d| d.remove_temp(request_id()));
        if let Some(key) = requested {
            self.info_modal_open = Some(key);
        }
        let Some(key) = self.info_modal_open.clone() else {
            return;
        };
        let modal = egui::Modal::new(egui::Id::new("info-modal-window"))
            .frame(widgets::modal_frame())
            .show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.label(
                    egui::RichText::new(t(&format!("{key}.infoTitle")))
                        .size(theme::FONT_SIZE_TITLE)
                        .color(theme::TEXT_STRONG),
                );
                ui.add_space(theme::ROW_GAP);
                // Long bodies scroll rather than growing the dialog past the
                // window: the adaptive controller's runs to eighteen hundred
                // characters.
                egui::ScrollArea::vertical()
                    .max_height(ui.ctx().content_rect().height() * 0.6)
                    .show(ui, |ui| {
                        markup::info_body(ui, t(&format!("{key}.infoBody")));
                    });
                ui.add_space(theme::PANEL_GAP);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(t("common.close")).clicked() {
                        self.info_modal_open = None;
                    }
                });
            });
        if modal.should_close() {
            self.info_modal_open = None;
        }
    }
}
