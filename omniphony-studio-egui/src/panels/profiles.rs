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

/// What a submitted name in the inline editor should do.
#[derive(Clone, PartialEq, Eq)]
pub enum NameEditor {
    Create,
    /// Rename this profile, if the new name differs and is free.
    Rename(String),
}

impl StudioSpike {
    pub(crate) fn profiles_row(&mut self, ui: &mut Ui) {
        let (active, names) = {
            let live = self.live.lock().unwrap();
            (
                live.app.active_profile.clone(),
                live.app.profile_names.clone(),
            )
        };
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
                    self.profile_delete_confirm = active.clone();
                }
                if ui
                    .add_enabled(has_active, egui::Button::new("✎"))
                    .on_hover_text(t("profiles.rename"))
                    .clicked()
                    && let Some(name) = &active
                {
                    self.profile_editor = Some(NameEditor::Rename(name.clone()));
                    self.profile_name_edit = name.clone();
                    self.profile_name_focus = true;
                }
                if ui.button("+").on_hover_text(t("profiles.create")).clicked() {
                    self.profile_editor = Some(NameEditor::Create);
                    self.profile_name_edit.clear();
                    self.profile_name_focus = true;
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
                            for name in &names {
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
                    cmd::control_profile_switch(&self.host, name);
                }
            });
        });
        self.profile_name_editor(ui, &names, active.as_deref());
        self.profile_delete_modal(ui, &names);
    }

    /// `#profileNameRow`: hidden until create or rename opens it.
    fn profile_name_editor(&mut self, ui: &mut Ui, names: &[String], active: Option<&str>) {
        let Some(action) = self.profile_editor.clone() else {
            return;
        };
        let mut submit = false;
        let mut cancel = false;
        ui.horizontal(|ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.profile_name_edit)
                    .hint_text(t("profiles.namePlaceholder"))
                    .font(egui::FontId::proportional(theme::FONT_SIZE))
                    .desired_width(ui.available_width() - 56.0),
            );
            if std::mem::take(&mut self.profile_name_focus) {
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
            self.profile_editor = None;
            return;
        }
        if !submit {
            return;
        }
        let name = self.profile_name_edit.trim().to_owned();
        self.profile_editor = None;
        if name.is_empty() {
            return;
        }
        match action {
            // An existing name is a switch, not a second profile of that name.
            NameEditor::Create if names.iter().any(|n| n == &name) => {
                cmd::control_profile_switch(&self.host, name);
            }
            NameEditor::Create => {
                cmd::control_profile_create(&self.host, name.clone());
                cmd::control_profile_switch(&self.host, name);
            }
            NameEditor::Rename(old) => {
                let unchanged = old == name || active != Some(old.as_str());
                if unchanged || names.iter().any(|n| n == &name) {
                    return;
                }
                cmd::control_profile_rename(&self.host, old, name);
            }
        }
    }

    /// The web asks with `window.confirm`; a native window gets a modal that
    /// spells out the two messages the confirmation actually sends.
    fn profile_delete_modal(&mut self, ui: &mut Ui, names: &[String]) {
        let Some(name) = self.profile_delete_confirm.clone() else {
            return;
        };
        let Some(keep) = names.iter().find(|n| *n != &name).cloned() else {
            self.profile_delete_confirm = None;
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
                        self.profile_delete_confirm = None;
                    }
                    if ui
                        .button(egui::RichText::new(t("profiles.delete")).color(theme::ERROR))
                        .clicked()
                    {
                        // Switch first: the renderer will not delete the profile
                        // it is running on.
                        cmd::control_profile_switch(&self.host, keep.clone());
                        cmd::control_profile_delete(&self.host, name.clone());
                        self.profile_delete_confirm = None;
                    }
                });
            });
        if modal.should_close() {
            self.profile_delete_confirm = None;
        }
    }
}
