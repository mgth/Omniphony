//! Persistent view-owned drafts for fields that commit on blur or Enter.
//!
//! A temporary clone of the model loses every keystroke on the next frame.
//! Keep the draft and its original value until the edit finishes; renderer
//! echoes may update the model underneath it but never replace typing.

use egui::{Id, Key, TextEdit, Ui};

#[derive(Default)]
pub struct TextDraft {
    target: Option<Id>,
    text: String,
    original: String,
    active: bool,
    invalid: bool,
}

impl TextDraft {
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
