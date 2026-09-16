//! The help overlay: the `*.infoModal` dialogs (`modals.js`) and every other
//! panel-level help.
//!
//! A dozen sections carry a long-form explanation — what a backend is, what the
//! adaptive controller does, how the heatmaps are computed — and a few a short
//! `help.*` string about the whole section. Either opens here, centred, from
//! the "i" beside the section's title (see `ui::help`). The request travels
//! from the header through egui's temporary data rather than through a return
//! value, so a section stays a pure widget. There is no close button: a click
//! anywhere outside, or Escape, closes it, and the absence of a button is what
//! says so.

use crate::app::StudioSpike;
use crate::ui::{help, markup, theme, widgets};

impl StudioSpike {
    pub(crate) fn info_modal(&mut self, ctx: &egui::Context) {
        let requested = ctx.data_mut(|d| {
            let overlay = d.get_temp::<help::Overlay>(help::overlay_request_id());
            d.remove::<help::Overlay>(help::overlay_request_id());
            overlay
        });
        if let Some(overlay) = requested {
            self.info_modal_open = Some(overlay);
        }
        let Some(overlay) = &self.info_modal_open else {
            return;
        };
        let modal = egui::Modal::new(egui::Id::new("info-modal-window"))
            .frame(widgets::modal_frame())
            .show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.label(
                    egui::RichText::new(&overlay.title)
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
                        markup::info_body(ui, &overlay.body);
                    });
            });
        if modal.should_close() {
            self.info_modal_open = None;
        }
    }
}
