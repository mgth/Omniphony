//! Persistent view-owned drafts for fields that commit on blur or Enter.
//!
//! A temporary clone of the model loses every keystroke on the next frame.
//! Keep the draft and its original value until the edit finishes; renderer
//! echoes may update the model underneath it but never replace typing.

use egui::{Id, Key, TextEdit, Ui};

#[derive(Default)]
pub struct TextDraft {
    target: Option<Id>,
    last_frame: Option<u64>,
    text: String,
    original: String,
    active: bool,
    invalid: bool,
}

impl TextDraft {
    /// A structural action invalidates the entity this text belonged to.
    pub fn discard(&mut self) {
        self.target = None;
        self.active = false;
        self.invalid = false;
    }

    /// `key` identifies the edited entity, not its current text. Changing it
    /// discards the previous entity's draft rather than committing it to the
    /// newly selected one. Empty text is valid for paths that mean "auto".
    pub fn show(
        &mut self,
        ui: &mut Ui,
        key: impl std::hash::Hash + std::fmt::Debug,
        source: &str,
        hint: &str,
        width: f32,
        allow_empty: bool,
    ) -> Option<String> {
        let frame = ui.ctx().cumulative_frame_nr();
        if self
            .last_frame
            .is_some_and(|last| frame > last.saturating_add(1))
        {
            self.discard();
        }
        self.last_frame = Some(frame);
        let id = ui.make_persistent_id(key);
        if self.target != Some(id) {
            self.target = Some(id);
            self.reset(source);
        } else if !self.active {
            self.reset(source);
        }
        let response = ui.add(
            TextEdit::singleline(&mut self.text)
                .id(id)
                .desired_width(width)
                .hint_text(hint),
        );
        if (response.has_focus() || response.lost_focus())
            && ui.input(|i| i.key_pressed(Key::Escape))
        {
            response.surrender_focus();
            self.reset(source);
            return None;
        }
        self.active |= response.has_focus() || response.changed();
        if response.changed() {
            self.invalid = false;
        }
        let mut committed = None;
        if response.lost_focus() && self.active {
            let value = self.text.trim();
            if !allow_empty && value.is_empty() {
                self.invalid = true;
            } else {
                // An untouched field must not overwrite an external update.
                // Nor should an echo of this same value send it a second time.
                if value != self.original.trim() && value != source.trim() {
                    committed = Some(value.to_owned());
                }
                self.active = false;
                self.invalid = false;
            }
        }
        if self.invalid {
            ui.colored_label(super::theme::ERROR, crate::i18n::t("common.valueRequired"));
        }
        committed
    }

    fn reset(&mut self, source: &str) {
        if self.text != source {
            self.text.clear();
            self.text.push_str(source);
        }
        if self.original != source {
            self.original.clear();
            self.original.push_str(source);
        }
        self.active = false;
        self.invalid = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        }
    }

    fn frame(
        ctx: &egui::Context,
        draft: &mut TextDraft,
        target: usize,
        source: &str,
        focus: bool,
        events: Vec<egui::Event>,
    ) -> Option<String> {
        let mut commit = None;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 100.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                if focus {
                    let id = ui.make_persistent_id(target);
                    ui.memory_mut(|m| m.request_focus(id));
                }
                commit = draft.show(ui, target, source, "", 170.0, false);
            },
        );
        output.textures_delta.clear();
        commit
    }

    #[test]
    fn structural_click_discards_blur_before_the_new_entity_draws() {
        let ctx = egui::Context::default();
        let mut draft = TextDraft::default();
        let mut button = egui::Rect::NOTHING;
        let mut emitted = Vec::new();
        for frame in 0..4 {
            let events = match frame {
                1 => vec![egui::Event::Text("X".into())],
                2 => vec![
                    egui::Event::PointerMoved(button.center()),
                    egui::Event::PointerButton {
                        pos: button.center(),
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: Default::default(),
                    },
                    egui::Event::PointerButton {
                        pos: button.center(),
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: Default::default(),
                    },
                ],
                _ => vec![],
            };
            let mut output = ctx.run_ui(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ui| {
                    let response = ui.button("Move up");
                    button = response.rect;
                    if response.clicked() {
                        emitted.push("move");
                        draft.discard();
                        return;
                    }
                    if frame == 0 {
                        let id = ui.make_persistent_id("speaker");
                        ui.memory_mut(|m| m.request_focus(id));
                    }
                    if draft
                        .show(ui, "speaker", "Speaker", "", 170.0, false)
                        .is_some()
                    {
                        emitted.push("rename");
                    }
                },
            );
        }
        assert_eq!(emitted, ["move"]);
    }

    #[test]
    fn hiding_then_reopening_a_field_discards_an_uncommitted_draft() {
        let ctx = egui::Context::default();
        let mut draft = TextDraft::default();
        frame(&ctx, &mut draft, 1, "first", true, vec![]);
        frame(
            &ctx,
            &mut draft,
            1,
            "first",
            false,
            vec![egui::Event::Text("X".into())],
        );
        let mut output = ctx.run_ui(Default::default(), |_ui| {});
        output.textures_delta.clear();
        assert_eq!(frame(&ctx, &mut draft, 1, "second", false, vec![]), None);
        assert_eq!(draft.text, "second");
    }

    #[test]
    fn typing_survives_frames_and_echoes_and_commits_once() {
        let ctx = egui::Context::default();
        let mut draft = TextDraft::default();
        frame(&ctx, &mut draft, 1, "speaker", true, vec![]);
        frame(
            &ctx,
            &mut draft,
            1,
            "speaker",
            false,
            vec![egui::Event::Text("X".into())],
        );
        let typed = draft.text.clone();
        assert!(typed.contains('X'));
        frame(&ctx, &mut draft, 1, "external", false, vec![]);
        assert_eq!(draft.text, typed);
        assert_eq!(
            frame(
                &ctx,
                &mut draft,
                1,
                "external",
                false,
                vec![key(Key::Enter)]
            ),
            Some(typed.clone())
        );
        assert_eq!(frame(&ctx, &mut draft, 1, &typed, false, vec![]), None);
    }

    #[test]
    fn escape_discards_typing_and_adopts_the_current_model() {
        let ctx = egui::Context::default();
        let mut draft = TextDraft::default();
        frame(&ctx, &mut draft, 1, "before", true, vec![]);
        frame(
            &ctx,
            &mut draft,
            1,
            "before",
            false,
            vec![egui::Event::Paste("typed".into())],
        );
        assert_eq!(
            frame(
                &ctx,
                &mut draft,
                1,
                "external",
                false,
                vec![key(Key::Escape)]
            ),
            None
        );
        assert_eq!(draft.text, "external");
    }

    #[test]
    fn switching_entities_never_commits_the_previous_draft() {
        let ctx = egui::Context::default();
        let mut draft = TextDraft::default();
        frame(&ctx, &mut draft, 1, "first", true, vec![]);
        frame(
            &ctx,
            &mut draft,
            1,
            "first",
            false,
            vec![egui::Event::Text("X".into())],
        );
        assert_eq!(frame(&ctx, &mut draft, 2, "second", false, vec![]), None);
        assert_eq!(draft.text, "second");
    }

    #[test]
    fn blur_does_not_restore_an_untouched_old_value() {
        let ctx = egui::Context::default();
        let mut draft = TextDraft::default();
        frame(&ctx, &mut draft, 1, "before", true, vec![]);
        assert_eq!(
            frame(&ctx, &mut draft, 1, "external", false, vec![key(Key::Tab)]),
            None
        );
        frame(&ctx, &mut draft, 1, "external", false, vec![]);
        assert_eq!(draft.text, "external");
    }
}
