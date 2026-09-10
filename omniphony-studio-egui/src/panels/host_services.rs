//! Host services: the local-renderer auto-start watchdog and the OS-service
//! controls (`src-tauri/src/main.rs`, `osc_listener.rs:1101–1242`,
//! `commands/orender.rs`, spec `host_contract.md` §3).
//!
//! The watchdog is what makes "open Studio and it works" true on a machine
//! where the renderer is a separate process: when the link has been down long
//! enough, the configured host is this machine, and nothing else is already
//! holding the port, it starts one. Everything else in it is a rule against
//! doing that when it would be wrong or futile.

use std::time::{Duration, Instant};

use egui::Ui;

use crate::app::StudioSpike;
use crate::host::commands::{HostPaths, orender};
use crate::host::config::load_config;
use crate::i18n::t;
use crate::panels::connection::OscState;
use crate::ui::widgets;

/// `WATCHDOG_INTERVAL`: how often the rules below are re-checked.
const TICK: Duration = Duration::from_secs(1);
/// `WATCHDOG_DISCONNECT_DEBOUNCE`: how long the link must be down first. A
/// renderer restarting on its own must be allowed to come back.
const DEBOUNCE: Duration = Duration::from_secs(6);
/// `WATCHDOG_GOODBYE_GRACE`: a renderer that said goodbye is not coming back,
/// so its port is free almost at once and the debounce is short-circuited.
const GOODBYE_GRACE: Duration = Duration::from_millis(500);
/// `WATCHDOG_FAST_FAIL_WINDOW`: a child that dies this soon after its spawn
/// did not start — it failed.
const FAST_FAIL: Duration = Duration::from_secs(5);
/// `WATCHDOG_COOLDOWN` and `WATCHDOG_MAX_ATTEMPTS`: back off, then give up
/// until something re-arms. Three failures in a row is a broken installation,
/// not bad luck, and a spawn loop would bury the reason in the log.
const COOLDOWN: Duration = Duration::from_secs(5);
const MAX_ATTEMPTS: u8 = 3;

impl StudioSpike {
    /// One watchdog tick, at most once a second.
    pub(crate) fn maintain_renderer_watchdog(&mut self) {
        let now = Instant::now();
        if self.watchdog_tick.is_some_and(|at| now - at < TICK) {
            return;
        }
        self.watchdog_tick = Some(now);
        self.reap_renderer_child();
        if self.osc_state() == OscState::Connected {
            self.disconnected_since = None;
            self.host.watchdog.lock().unwrap().check_requested_at = None;
            return;
        }
        let since = *self.disconnected_since.get_or_insert(now);
        let goodbye_ready = self
            .host
            .watchdog
            .lock()
            .unwrap()
            .check_requested_at
            .is_some_and(|at| at.elapsed() >= GOODBYE_GRACE);
        if !goodbye_ready && since.elapsed() < DEBOUNCE {
            return;
        }
        // Re-read the configuration at check time, so a panel edit applies
        // without a restart.
        let cfg = load_config(&self.host.config_dir);
        if !cfg.auto_start_renderer || !crate::host::commands::app::host_is_local(&cfg.host) {
            return;
        }
        {
            let wd = self.host.watchdog.lock().unwrap();
            if wd.suppressed || wd.attempts >= MAX_ATTEMPTS {
                return;
            }
            if wd.cooldown_until.is_some_and(|at| at > now) {
                return;
            }
        }
        // A tracked child that is still running is still starting up.
        if self
            .host
            .renderer_child
            .lock()
            .unwrap()
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(None)))
        {
            return;
        }
        // A service-managed renderer is someone else's responsibility.
        if orender::orender_service_running() {
            return;
        }
        // Something already holds the port — an mpv-embedded renderer we lost
        // contact with, most likely. Starting a second one would fight it.
        if std::net::UdpSocket::bind(("0.0.0.0", cfg.osc_rx_port)).is_err() {
            self.host.watchdog.lock().unwrap().check_requested_at = None;
            return;
        }
        match orender::autostart_orender(&HostPaths::default(), &self.host) {
            Ok(info) => {
                let command = info
                    .get("command")
                    .and_then(|c| c.as_str())
                    .unwrap_or_default()
                    .to_owned();
                self.log(
                    "info",
                    "orender",
                    format!("{} {command}", t("log.orenderAutostartLaunched")),
                );
            }
            Err(error) => {
                let mut wd = self.host.watchdog.lock().unwrap();
                wd.attempts += 1;
                wd.cooldown_until = Some(now + COOLDOWN);
                let attempts = wd.attempts;
                drop(wd);
                self.log("error", "orender", format!("orender: {error}"));
                // Only the streak is worth an alarm: one failed spawn on a busy
                // machine is not a broken installation.
                if attempts >= MAX_ATTEMPTS {
                    self.log("error", "orender", t("log.orenderAutostartFailed"));
                }
            }
        }
    }

    /// Reap the tracked child whatever the connection state, so a renderer that
    /// died is noticed even while another one answers.
    fn reap_renderer_child(&mut self) {
        let exited = {
            let mut slot = self.host.renderer_child.lock().unwrap();
            match slot.as_mut().map(|child| child.try_wait()) {
                Some(Ok(Some(status))) => {
                    *slot = None;
                    Some(status)
                }
                _ => None,
            }
        };
        let Some(status) = exited else { return };
        let fast_fail = {
            let wd = self.host.watchdog.lock().unwrap();
            wd.last_spawn_at.is_some_and(|at| at.elapsed() < FAST_FAIL)
        };
        if !fast_fail {
            // Yielded to an mpv-embedded renderer, or stopped on purpose.
            self.host.watchdog.lock().unwrap().attempts = 0;
            self.log("info", "orender", format!("orender exited: {status}"));
            return;
        }
        let attempts = {
            let mut wd = self.host.watchdog.lock().unwrap();
            wd.attempts += 1;
            wd.cooldown_until = Some(Instant::now() + COOLDOWN);
            wd.attempts
        };
        self.log(
            "warn",
            "orender",
            format!(
                "orender exited within {}s of starting: {status}",
                FAST_FAIL.as_secs()
            ),
        );
        if attempts >= MAX_ATTEMPTS {
            self.log("error", "orender", t("log.orenderAutostartFailed"));
        }
    }

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
