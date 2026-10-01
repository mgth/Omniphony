//! What Studio asks before it lets the renderer's unsaved edits go: the quit
//! prompt and the Reload confirmation (docs/persistence-policy.md).
//!
//! The renderer holds render and engine edits unsaved until the Save button
//! writes them. Closing Studio or pressing Reload is where they are silently
//! lost, so both ask first — and only when there is something to lose: a
//! connected renderer that last said "unsaved".

use egui::RichText;

use crate::app::StudioSpike;
use crate::host::commands::engine::{self, SaveOutcome};
use crate::i18n::t;
use crate::ui::{theme, widgets};

/// Where the quit prompt stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum UnsavedQuit {
    /// No quit under way.
    #[default]
    Idle,
    /// A close was held back: the prompt is on screen.
    Asking,
    /// "Save and quit": waiting for the renderer to confirm the save.
    Saving,
    /// The user chose; the next close goes through.
    Confirmed,
}

/// What a "save and quit" does next.
#[derive(Debug, PartialEq, Eq)]
enum AfterSave {
    /// The renderer has not answered yet.
    Wait,
    /// Saved: close.
    Close,
    /// Not saved: back to the prompt, with the renderer's reason if it gave
    /// one.
    Ask(Option<String>),
}

fn after_save(outcome: SaveOutcome, connected: bool) -> AfterSave {
    match outcome {
        // Waiting on the connection, not on "unsaved": a save asked again
        // over a file an earlier attempt already wrote is just as pending.
        SaveOutcome::Pending if connected => AfterSave::Wait,
        SaveOutcome::Saved => AfterSave::Close,
        SaveOutcome::Failed(error) => AfterSave::Ask(Some(error)),
        // Another write landed after the save, or the renderer went away
        // mid-save and nothing will answer: the choice goes back to the user.
        SaveOutcome::Pending | SaveOutcome::Unsaved => AfterSave::Ask(None),
    }
}

impl StudioSpike {
    /// Logic-pass half of the prompt: finish a "save and quit" once the
    /// renderer answers. It runs from `App::logic`, which eframe calls even
    /// while the window is hidden, so a save confirmed behind a minimised
    /// window still closes it.
    pub(crate) fn advance_unsaved_quit(&mut self, ctx: &egui::Context) {
        if self.unsaved_quit != UnsavedQuit::Saving {
            return;
        }
        match after_save(
            engine::save_outcome(&self.host),
            engine::renderer_connected(&self.host),
        ) {
            AfterSave::Wait => {}
            AfterSave::Close => {
                self.unsaved_quit = UnsavedQuit::Confirmed;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            AfterSave::Ask(error) => {
                self.unsaved_quit_error = error;
                self.unsaved_quit = UnsavedQuit::Asking;
            }
        }
    }

    /// Whether a close has to be held back for the renderer's unsaved edits.
    pub(crate) fn quit_would_lose_edits(&self) -> bool {
        self.unsaved_quit != UnsavedQuit::Confirmed && engine::has_unsaved_edits(&self.host)
    }

    /// The prompt: save and quit, quit without saving, or stay.
    pub(crate) fn unsaved_quit_modal(&mut self, ctx: &egui::Context) {
        if !matches!(self.unsaved_quit, UnsavedQuit::Asking | UnsavedQuit::Saving) {
            return;
        }
        let saving = self.unsaved_quit == UnsavedQuit::Saving;
        let stops = crate::host::commands::orender::quitting_stops_renderer(&self.host);
        let modal = egui::Modal::new(egui::Id::new("unsaved-quit"))
            .frame(widgets::modal_frame())
            .show(ctx, |ui| {
                ui.set_max_width(400.0);
                ui.label(
                    RichText::new(t("unsaved.quitTitle"))
                        .size(theme::FONT_SIZE)
                        .color(theme::TEXT_STRONG),
                );
                ui.label(RichText::new(t("unsaved.quitBody")).size(theme::FONT_SIZE));
                ui.label(
                    RichText::new(t(if stops {
                        "unsaved.quitStops"
                    } else {
                        "unsaved.quitKeeps"
                    }))
                    .size(theme::FONT_SIZE_SMALL)
                    .color(theme::TEXT_MUTED),
                );
                if let Some(error) = &self.unsaved_quit_error {
                    ui.label(
                        RichText::new(error)
                            .size(theme::FONT_SIZE_SMALL)
                            .color(theme::ERROR),
                    );
                }
                ui.add_space(theme::PANEL_GAP);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let save = ui.add_enabled(
                        !saving,
                        egui::Button::new(if saving {
                            t("unsaved.saving")
                        } else {
                            t("unsaved.saveAndQuit")
                        }),
                    );
                    if save.clicked() {
                        self.unsaved_quit_error = None;
                        engine::request_save_config(&self.host);
                        self.unsaved_quit = UnsavedQuit::Saving;
                    }
                    if ui.button(t("unsaved.quitWithoutSaving")).clicked() {
                        self.unsaved_quit = UnsavedQuit::Confirmed;
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if ui.button(t("common.cancel")).clicked() {
                        self.unsaved_quit = UnsavedQuit::Idle;
                        self.unsaved_quit_error = None;
                    }
                });
            });
        if modal.should_close() && self.unsaved_quit != UnsavedQuit::Confirmed {
            self.unsaved_quit = UnsavedQuit::Idle;
            self.unsaved_quit_error = None;
        }
    }

    /// Reload throws the unsaved edits away: ask first when there are some.
    pub(crate) fn request_reload(&mut self) {
        if engine::has_unsaved_edits(&self.host) {
            self.reload_confirm_open = true;
        } else {
            engine::control_reload_config(&self.host);
        }
    }

    pub(crate) fn reload_confirm_modal(&mut self, ctx: &egui::Context) {
        if !self.reload_confirm_open {
            return;
        }
        let modal = egui::Modal::new(egui::Id::new("reload-confirm"))
            .frame(widgets::modal_frame())
            .show(ctx, |ui| {
                ui.set_max_width(380.0);
                ui.label(
                    RichText::new(t("unsaved.reloadTitle"))
                        .size(theme::FONT_SIZE)
                        .color(theme::TEXT_STRONG),
                );
                ui.label(RichText::new(t("unsaved.reloadBody")).size(theme::FONT_SIZE));
                ui.add_space(theme::PANEL_GAP);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(t("unsaved.reloadDiscard")).clicked() {
                        engine::control_reload_config(&self.host);
                        self.reload_confirm_open = false;
                    }
                    if ui.button(t("common.cancel")).clicked() {
                        self.reload_confirm_open = false;
                    }
                });
            });
        if modal.should_close() {
            self.reload_confirm_open = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_and_quit_closes_only_on_a_confirmed_save() {
        assert_eq!(after_save(SaveOutcome::Pending, true), AfterSave::Wait);
        assert_eq!(after_save(SaveOutcome::Saved, false), AfterSave::Close);
        assert_eq!(
            after_save(SaveOutcome::Failed("disk full".into()), true),
            AfterSave::Ask(Some("disk full".into()))
        );
        assert_eq!(after_save(SaveOutcome::Unsaved, true), AfterSave::Ask(None));
        // The renderer is gone: waiting would never end.
        assert_eq!(
            after_save(SaveOutcome::Pending, false),
            AfterSave::Ask(None)
        );
    }
}
