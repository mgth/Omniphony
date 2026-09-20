//! The config-profile picker (`#profileRow` / `#profileNameRow`,
//! `controls/profiles.js`, host `commands/profiles.rs`).
//!
//! Pinned above the left overlay's scroll, so opening the name editor never
//! moves the viewport. The renderer is the single source of truth: every
//! mutation goes out over OSC and the row is re-populated from the
//! `/omniphony/state/profiles` echo. Nothing is applied optimistically —
//! the select shows what the renderer last said, not what was asked for.

use egui::Ui;

use crate::app::StudioSpike;
use crate::host::commands::profiles as cmd;
use crate::i18n::{t, tf};
use crate::ui::{theme, widgets};

/// View state owned by this panel, independent of the application and renderer.
#[derive(Default)]
pub struct ProfilePanel {
    editor: Option<cmd::NameEditor>,
    name: String,
    focus_name: bool,
    delete_confirm: Option<String>,
}

impl StudioSpike {
    pub(crate) fn profiles_row(&mut self, ui: &mut Ui) {
        let snapshot = cmd::Snapshot::of(&self.host.read().app);
        if let Some(action) = self.profiles.show(ui, &snapshot) {
            cmd::apply(&self.host, action);
        }
    }
}

impl ProfilePanel {
    /// Draw from a snapshot and return at most one user intent. The core checks
    /// it against current state before sending commands; drawing never owns I/O.
    pub fn show(&mut self, ui: &mut Ui, snapshot: &cmd::Snapshot) -> Option<cmd::Action> {
        let cmd::Snapshot { active, names } = snapshot;
        let mut action = None;
        let has_active = active.as_deref().is_some_and(|a| !a.is_empty());
        // The three buttons are placed first, right to left, and the list takes
        // what they leave. Sizing the list as "available minus the buttons"
        // needs the buttons' width, and the hand-written guess was ten points
        // short: the row ran past the panel at its minimum width.
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // The renderer refuses to delete the last profile, so the button
                // says so rather than letting the message be rejected silently.
                let can_delete = has_active && names.len() > 1;
                if ui
                    .add_enabled(can_delete, egui::Button::new("✕"))
                    .on_hover_text(t("profiles.delete"))
                    .clicked()
                {
                    self.delete_confirm = active.clone();
                }
                if ui
                    .add_enabled(has_active, egui::Button::new("✎"))
                    .on_hover_text(t("profiles.rename"))
                    .clicked()
                    && let Some(name) = &active
                {
                    self.editor = Some(cmd::NameEditor::Rename(name.clone()));
                    self.name = name.clone();
                    self.focus_name = true;
                }
                if ui.button("+").on_hover_text(t("profiles.create")).clicked() {
                    self.editor = Some(cmd::NameEditor::Create);
                    self.name.clear();
                    self.focus_name = true;
                }
                let label = active.clone().unwrap_or_default();
                let combo = egui::ComboBox::from_id_salt("profile-select")
                    .selected_text(label)
                    .width(ui.available_width());
                let mut picked: Option<String> = None;
                ui.add_enabled_ui(!names.is_empty(), |ui| {
                    // The list has no label to click, so its help is its
                    // hover text, as the web's `title` is.
                    combo
                        .show_ui(ui, |ui| {
                            for name in names {
                                let selected = active.as_deref() == Some(name.as_str());
                                if ui.selectable_label(selected, name).clicked() && !selected {
                                    picked = Some(name.clone());
                                }
                            }
                        })
                        .response
                        .on_hover_text(t("help.profiles"));
                });
                if let Some(name) = picked {
                    action = Some(cmd::Action::Switch(name));
                }
            });
        });
        self.name_editor(ui, &mut action);
        self.delete_modal(ui, names, &mut action);
        action
    }

    /// `#profileNameRow`: hidden until create or rename opens it.
    fn name_editor(&mut self, ui: &mut Ui, intent: &mut Option<cmd::Action>) {
        let Some(action) = self.editor.clone() else {
            return;
        };
        let mut submit = false;
        let mut cancel = false;
        ui.horizontal(|ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.name)
                    .hint_text(t("profiles.namePlaceholder"))
                    .font(egui::FontId::proportional(theme::FONT_SIZE))
                    .desired_width(ui.available_width() - 56.0),
            );
            if std::mem::take(&mut self.focus_name) {
                edit.request_focus();
            }
            if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                submit = true;
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                cancel = true;
            }
            if ui
                .button("✓")
                .on_hover_text(t("profiles.confirmName"))
                .clicked()
            {
                submit = true;
            }
            if ui.button("✕").on_hover_text(t("common.cancel")).clicked() {
                cancel = true;
            }
        });
        if cancel {
            self.editor = None;
            return;
        }
        if !submit {
            return;
        }
        *intent = Some(cmd::Action::Submit {
            editor: action,
            name: self.name.clone(),
        });
        self.editor = None;
    }

    /// The web asks with `window.confirm`; a native window gets a modal that
    /// spells out the two messages the confirmation actually sends.
    fn delete_modal(&mut self, ui: &mut Ui, names: &[String], intent: &mut Option<cmd::Action>) {
        let Some(name) = self.delete_confirm.clone() else {
            return;
        };
        if !names.contains(&name) {
            self.delete_confirm = None;
            return;
        }
        let Some(keep) = names.iter().find(|n| *n != &name).cloned() else {
            self.delete_confirm = None;
            return;
        };
        let modal = egui::Modal::new(egui::Id::new("profile-delete-confirm"))
            .frame(widgets::modal_frame())
            .show(ui.ctx(), |ui| {
                ui.set_max_width(320.0);
                for line in tf(
                    "profiles.confirmDelete",
                    &[("name", &name), ("keep", &keep)],
                )
                .split('\n')
                {
                    if line.is_empty() {
                        ui.add_space(theme::ROW_GAP);
                    } else {
                        ui.label(line);
                    }
                }
                ui.add_space(theme::PANEL_GAP);
                ui.horizontal(|ui| {
                    if ui.button(t("common.cancel")).clicked() {
                        self.delete_confirm = None;
                    }
                    if ui
                        .button(egui::RichText::new(t("profiles.delete")).color(theme::ERROR))
                        .clicked()
                    {
                        // Switch first: the renderer will not delete the profile
                        // it is running on.
                        *intent = Some(cmd::Action::Delete(name.clone()));
                        self.delete_confirm = None;
                    }
                });
            });
        if modal.should_close() {
            self.delete_confirm = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(
        ctx: &egui::Context,
        panel: &mut ProfilePanel,
        snapshot: &cmd::Snapshot,
        events: Vec<egui::Event>,
    ) -> Option<cmd::Action> {
        let mut intent = None;
        let mut output = ctx.run_ui(
            egui::RawInput {
                events,
                ..Default::default()
            },
            |ui| {
                intent = panel.show(ui, snapshot).or(intent.take());
            },
        );
        output.textures_delta.clear();
        intent
    }
    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        }
    }

    #[test]
    fn an_isolated_panel_keeps_unicode_typing_across_echoes_and_submits_once() {
        let ctx = egui::Context::default();
        let mut panel = ProfilePanel {
            editor: Some(cmd::NameEditor::Create),
            focus_name: true,
            ..Default::default()
        };
        let mut snapshot = cmd::Snapshot::default();
        assert!(frame(&ctx, &mut panel, &snapshot, vec![]).is_none());
        assert!(
            frame(
                &ctx,
                &mut panel,
                &snapshot,
                vec![egui::Event::Text("Écoute".into())]
            )
            .is_none()
        );
        snapshot.active = Some("Existing".into());
        snapshot.names.push("Existing".into());
        assert!(
            frame(
                &ctx,
                &mut panel,
                &snapshot,
                vec![egui::Event::Paste(" 日本語".into())]
            )
            .is_none()
        );
        assert_eq!(panel.name, "Écoute 日本語");
        assert_eq!(
            frame(&ctx, &mut panel, &snapshot, vec![key(egui::Key::Enter)]),
            Some(cmd::Action::Submit {
                editor: cmd::NameEditor::Create,
                name: "Écoute 日本語".into()
            })
        );
        assert!(frame(&ctx, &mut panel, &snapshot, vec![]).is_none());
    }

    #[test]
    fn escape_cancels_without_intent_and_removed_profiles_close_confirmation() {
        let ctx = egui::Context::default();
        let mut panel = ProfilePanel {
            editor: Some(cmd::NameEditor::Create),
            name: "Draft".into(),
            focus_name: true,
            delete_confirm: Some("Removed".into()),
        };
        let snapshot = cmd::Snapshot {
            active: Some("A".into()),
            names: vec!["A".into(), "B".into()],
        };
        assert!(frame(&ctx, &mut panel, &snapshot, vec![]).is_none());
        assert!(panel.delete_confirm.is_none());
        assert!(frame(&ctx, &mut panel, &snapshot, vec![key(egui::Key::Escape)]).is_none());
        assert!(panel.editor.is_none());
    }
}
