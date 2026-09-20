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
        let room = ctx.content_rect();
        let modal = egui::Modal::new(egui::Id::new("info-modal-window"))
            .frame(widgets::modal_frame())
            .show(ctx, |ui| {
                ui.set_max_width(body_width(room));
                let title = ui.label(
                    egui::RichText::new(&overlay.title)
                        .size(theme::FONT_SIZE_TITLE)
                        .color(theme::TEXT_STRONG),
                );
                ui.add_space(theme::ROW_GAP);
                // A long body scrolls only once it truly cannot fit: what it
                // may take is the window's height, less what the dialog spends
                // around it. The cap used to be a flat 60 % of the window, and
                // the width a flat 460 pt whatever there was — between them,
                // the longest help in the catalogue got a scrollbar with room
                // to spare on either side of the dialog.
                egui::ScrollArea::vertical()
                    .max_height(body_height(room, title.rect.height()))
                    .show(ui, |ui| {
                        markup::info_body(ui, &overlay.body);
                    });
            });
        if modal.should_close() {
            self.info_modal_open = None;
        }
    }
}

/// The widest the body is laid out, whatever the window has to offer. A line
/// much past ninety characters — 560 pt at `FONT_SIZE` — is one the eye loses
/// its way back from, so the dialog stops there and the room beyond it stays
/// margin.
const MAX_MEASURE: f32 = 560.0;

/// What the dialog keeps between itself and the window's edges.
const EDGE_MARGIN: f32 = 24.0;

/// A body shorter than this scrolls rather than squeezing the dialog out of a
/// window barely taller than its own chrome.
const MIN_BODY: f32 = 120.0;

/// The body's measure in a window of `room`: what is there, up to what is
/// readable.
fn body_width(room: egui::Rect) -> f32 {
    (room.width() - 2.0 * (EDGE_MARGIN + theme::PANEL_PADDING_X))
        .min(MAX_MEASURE)
        .max(MIN_BODY)
}

/// The height the body may take before it has to scroll: the window's, less
/// the frame's own padding, the title above it and the margin the dialog keeps
/// from the window's edges.
fn body_height(room: egui::Rect, title_height: f32) -> f32 {
    let chrome = 2.0 * (EDGE_MARGIN + theme::PANEL_PADDING_Y) + title_height + theme::ROW_GAP;
    (room.height() - chrome).max(MIN_BODY)
}

#[cfg(test)]
mod tests {
    use super::{body_height, body_width};
    use crate::ui::{markup, theme};

    /// The app's own default window (`main.rs`), and an ordinary smaller one.
    const DEFAULT_WINDOW: egui::Vec2 = egui::vec2(1400.0, 900.0);
    const SMALL_WINDOW: egui::Vec2 = egui::vec2(1000.0, 700.0);

    /// Lay `body` out at `width` and say how tall it comes out.
    fn body_extent(width: f32, body: &str) -> f32 {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut height = 0.0;
        let mut output = ctx.run_ui(Default::default(), |ui| {
            ui.vertical(|ui| {
                ui.set_max_width(width);
                markup::info_body(ui, body);
                height = ui.min_rect().height();
            });
        });
        // Nothing paints here; egui still wants its font atlas taken.
        output.textures_delta.clear();
        height
    }

    /// The tallest help in the catalogue, and its title, so the dialog is
    /// measured against what it actually has to show.
    fn longest_help() -> (String, String) {
        let json: serde_json::Value =
            serde_json::from_str(include_str!("../../../omniphony-studio/src/i18n/fr.json"))
                .expect("the French catalogue parses");
        let mut best = (0.0f32, String::new());
        for (key, value) in json.as_object().expect("a flat catalogue") {
            if !(key.ends_with("infoBody") || key.starts_with("help.")) {
                continue;
            }
            let Some(text) = value.as_str() else { continue };
            let height = body_extent(body_width(rect(DEFAULT_WINDOW)), text);
            if height > best.0 {
                best = (height, text.to_owned());
            }
        }
        (best.1, format!("{:.0}", best.0))
    }

    fn rect(size: egui::Vec2) -> egui::Rect {
        egui::Rect::from_min_size(egui::Pos2::ZERO, size)
    }

    /// No help in the catalogue scrolls while the window can hold it. The
    /// dialog was a flat 460 pt wide with its body capped at 60 % of the
    /// window height, and the longest one — the adaptive controller's, two
    /// thousand characters — came to 510 pt: against the 420 pt those caps
    /// allowed in a 1000×700 window, a scrollbar with half the window empty
    /// around the dialog. Laid out at the measure it can now have, it is
    /// 440 pt and the window allows 601.
    #[test]
    fn the_longest_help_does_not_scroll_in_a_window_that_can_hold_it() {
        let (body, _) = longest_help();
        // A one-line title, which every one of them is.
        let title = theme::FONT_SIZE_TITLE * 1.5;
        for window in [DEFAULT_WINDOW, SMALL_WINDOW] {
            let room = rect(window);
            let height = body_extent(body_width(room), &body);
            let allowed = body_height(room, title);
            assert!(
                height <= allowed,
                "in a {:.0}×{:.0} window the longest help is {height:.0} pt tall at {:.0} pt \
                 wide, past the {allowed:.0} pt the dialog allows: widen the measure, or \
                 shorten the help",
                window.x,
                window.y,
                body_width(room),
            );
        }
    }

    /// A window too small for the body still gives it a workable height rather
    /// than a sliver, and a narrow one shrinks the measure instead of running
    /// the dialog off the sides.
    #[test]
    fn a_small_window_bounds_the_dialog_rather_than_the_other_way_round() {
        let tiny = rect(egui::vec2(320.0, 200.0));
        assert!(
            body_width(tiny) <= 320.0,
            "the dialog is wider than the window"
        );
        assert!(body_height(tiny, 24.0) >= 120.0, "the body is a sliver");
        let huge = rect(egui::vec2(3840.0, 2160.0));
        assert_eq!(body_width(huge), 560.0, "the measure is unbounded");
    }
}
