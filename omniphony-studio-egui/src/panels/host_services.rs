//! Host operation controls. Slow work and its pending/error state are core-owned.
use crate::app::StudioSpike;
use crate::host::services::operations::Action;
use crate::i18n::t;
use crate::ui::{theme, widgets};
use egui::Ui;

impl StudioSpike {
    pub(crate) fn renderer_controls(&mut self, ui: &mut Ui) {
        if !crate::host::capabilities::ActionPolicy::of(&self.host).manage_process {
            return;
        }
        self.host_operations.poll();
        if self.host_operations.status.is_none()
            && !self.host_operations.pending()
            && self.host_operations.error.is_none()
        {
            self.host_operations.request(&self.host, Action::Refresh);
        }
        let installed = self
            .host_operations
            .status
            .as_ref()
            .and_then(|s| s.as_ref().ok())
            .map(|s| s.installed);
        let mut action = None;
        ui.add_enabled_ui(!self.host_operations.pending(), |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button(t("osc.orender.launch")).clicked() {
                    action = Some(Action::Launch);
                }
                if ui.button(t("osc.orender.stop")).clicked() {
                    action = Some(Action::Stop);
                }
                match installed {
                    Some(true) => {
                        if ui.button(t("osc.service.restart")).clicked() {
                            action = Some(Action::RestartService);
                        }
                        if ui.button(t("osc.service.uninstall")).clicked() {
                            action = Some(Action::UninstallService);
                        }
                    }
                    Some(false) => {
                        if ui.button(t("osc.service.install")).clicked() {
                            action = Some(Action::InstallService);
                        }
                    }
                    None => {}
                }
                if ui.button(t("osc.pipewire.restartTitle")).clicked() {
                    action = Some(Action::RestartPipewire);
                }
                if ui.button(t("common.refresh")).clicked() {
                    action = Some(Action::Refresh);
                }
            });
        });
        if let Some(action) = action {
            self.host_operations.request(&self.host, action);
        }
        if self.host_operations.pending() {
            ui.spinner();
        }
        if let Some(status) = &self.host_operations.status {
            match status {
                Ok(status) => widgets::note(
                    ui,
                    &format!(
                        "{} · {}",
                        status.manager,
                        t(if status.running {
                            "osc.service.running"
                        } else if status.installed {
                            "osc.service.installed"
                        } else {
                            "osc.service.notInstalled"
                        })
                    ),
                ),
                Err(error) => {
                    ui.colored_label(theme::ERROR, error);
                }
            }
        }
        if let Some(error) = &self.host_operations.error {
            ui.colored_label(theme::ERROR, error);
        }
    }
}
