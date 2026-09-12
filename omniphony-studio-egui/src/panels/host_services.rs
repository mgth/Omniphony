//! Host services: the local-renderer auto-start watchdog and the OS-service
//! controls (`src-tauri/src/main.rs`, `osc_listener.rs:1101–1242`,
//! `commands/orender.rs`, spec `host_contract.md` §3).
//!
//! The watchdog itself runs on the core's clock (`services::watchdog`): it has
//! to work when nothing is drawn, which is exactly when a renderer is missing.
//! What is left here is the panel that reports and commands it.

use std::time::{Duration, Instant};

use egui::Ui;

use crate::app::StudioSpike;
use crate::host::commands::{HostPaths, orender};
use crate::host::config::load_config;
use crate::i18n::t;
use crate::ui::widgets;

impl StudioSpike {
    /// Launch, Stop, and the OS service. They live under the OSC section,
    /// where the connection they are about is reported.
    pub(crate) fn renderer_controls(&mut self, ui: &mut Ui) {
        let (installed, manager) = self.service_status();
        ui.horizontal_wrapped(|ui| {
            if ui.button(t("osc.orender.launch")).clicked() {
                self.launch_renderer();
            }
            if ui.button(t("osc.orender.stop")).clicked() {
                orender::stop_orender(&self.host);
            }
            if installed {
                if ui.button(t("osc.service.restart")).clicked() {
                    self.report("service", orender::restart_orender_service());
                }
                if ui.button(t("osc.service.uninstall")).clicked() {
                    self.report("service", orender::uninstall_orender_service());
                }
            } else if ui.button(t("osc.service.install")).clicked() {
                let cfg = load_config(&self.host.config_dir);
                self.report_value(
                    "service",
                    orender::install_orender_service(
                        &HostPaths::default(),
                        &self.host,
                        cfg.host,
                        cfg.osc_rx_port,
                        cfg.osc_port,
                        cfg.osc_metering_enabled,
                        None,
                        None,
                    ),
                );
            }
            if ui.button(t("osc.pipewire.restartTitle")).clicked() {
                self.report("pipewire", orender::restart_pipewire_services());
            }
        });
        widgets::note(
            ui,
            &format!(
                "{}: {} · {manager}",
                t("osc.service.serviceNoun"),
                if installed {
                    "installed"
                } else {
                    "not installed"
                },
            ),
        );
    }

    /// Whether the service is installed, and which manager runs it.
    ///
    /// Answering means spawning a process, so it is asked every few seconds
    /// rather than every frame — and re-asked at once after a change, since the
    /// button that made it is the reason anyone is looking.
    fn service_status(&mut self) -> (bool, String) {
        const REFRESH: Duration = Duration::from_secs(5);
        if let Some((at, installed, manager)) = &self.service_status
            && at.elapsed() < REFRESH
        {
            return (*installed, manager.clone());
        }
        let value = orender::get_orender_service_status()
            .ok()
            .and_then(|s| serde_json::to_value(s).ok());
        let installed = value
            .as_ref()
            .and_then(|v| v.get("installed").and_then(serde_json::Value::as_bool))
            .unwrap_or(false);
        let manager = value
            .as_ref()
            .and_then(|v| v.get("manager").and_then(|m| m.as_str()))
            .unwrap_or("none")
            .to_owned();
        self.service_status = Some((Instant::now(), installed, manager.clone()));
        (installed, manager)
    }

    fn launch_renderer(&mut self) {
        let cfg = load_config(&self.host.config_dir);
        let result = orender::launch_orender(
            &HostPaths::default(),
            &self.host,
            cfg.host,
            cfg.osc_rx_port,
            cfg.osc_port,
            cfg.osc_metering_enabled,
            None,
            None,
        );
        match result {
            Ok(info) => self.log(
                "info",
                "orender",
                info.get("command")
                    .and_then(|c| c.as_str())
                    .unwrap_or("orender launched")
                    .to_owned(),
            ),
            Err(error) => self.log("error", "orender", error),
        }
    }

    fn report(&mut self, target: &str, result: Result<(), String>) {
        self.service_status = None;
        match result {
            Ok(()) => self.log("info", target, "ok"),
            Err(error) => self.log("error", target, error),
        }
    }

    fn report_value(&mut self, target: &str, result: Result<serde_json::Value, String>) {
        self.service_status = None;
        match result {
            Ok(value) => self.log("info", target, value.to_string()),
            Err(error) => self.log("error", target, error),
        }
    }
}
