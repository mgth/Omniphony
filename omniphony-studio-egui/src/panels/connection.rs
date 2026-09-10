//! OSC connection status (`#oscPanelRoot`): the dot, the target and the port,
//! plus the reconnect form of `controls/osc.js`.

use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::app::StudioSpike;
use crate::host::config::{OscConfig, save_config};
use crate::i18n::t;
use crate::ui::section::Section;
use crate::ui::{theme, widgets};

impl StudioSpike {
    /// The status line: a coloured dot and one line of text, like the web
    /// `#oscStatus` row.
    pub(crate) fn connection_line(&mut self, ui: &mut egui::Ui) {
        let port = self.osc_stats.listen_port.load(Ordering::Relaxed);
        let target = *self.osc_stats.target.lock().unwrap();
        let (dot, text) = match (
            target,
            self.osc_stats.registered.load(Ordering::Relaxed),
            self.osc_stats.since_last_packet(),
        ) {
            (Some(addr), true, Some(d)) if d < Duration::from_secs(7) => {
                (theme::OK, format!("registered with {addr} · udp/{port}"))
            }
            (Some(addr), _, _) => (theme::WARN, format!("waiting for {addr} · udp/{port}")),
            (None, _, Some(d)) if d < Duration::from_secs(2) => {
                (theme::OK, format!("receiving on udp/{port}"))
            }
            (None, _, _) => (theme::TEXT_FAINT, format!("idle · udp/{port}")),
        };
        ui.horizontal(|ui| {
            widgets::status_dot(ui, dot, text);
            // `#aboutBtn`: the box's second entry point, beside the status it
            // reports on.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(egui::Button::new("?").small())
                    .on_hover_text(t("about.open"))
                    .clicked()
                {
                    self.about_open = true;
                }
            });
        });
    }
}

impl StudioSpike {
    /// The OSC configuration form (`#oscConfigForm`, `controls/osc.js`): the
    /// renderer's host and port, this client's listen port and the metering
    /// switch, with a Connect button that re-registers.
    pub(crate) fn osc_section(&mut self, ui: &mut egui::Ui) {
        let listen_port = self.osc_stats.listen_port.load(Ordering::Relaxed);
        let metering_on = {
            let live = self.live.lock().unwrap();
            live.app.osc_metering_enabled.unwrap_or(0) != 0
        };
        Section::new("oscSection", "osc.configTitle")
            .summary(format!("{}:{}", self.osc_host, self.osc_port))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(t("osc.host"));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.osc_host).desired_width(140.0));
                    });
                });
                ui.horizontal(|ui| {
                    ui.label(t("osc.omniphonyPort"));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add(egui::DragValue::new(&mut self.osc_port).range(1..=65535));
                    });
                });
                ui.horizontal(|ui| {
                    ui.label(t("osc.listenPort"));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(listen_port.to_string())
                                .monospace()
                                .color(theme::TEXT_MUTED),
                        );
                    });
                });
                let mut metering = metering_on;
                if widgets::switch_row(ui, t("osc.metering"), &mut metering) {
                    self.live.lock().unwrap().app.osc_metering_enabled = Some(u8::from(metering));
                    self.ctl.set_metering(metering);
                }
                if ui.button(t("osc.connect")).clicked() {
                    self.connect();
                }
            });
    }

    /// Point the client at the configured renderer and save the choice, like
    /// `save_osc_config` + the host's `Reconnect`.
    fn connect(&mut self) {
        let target = format!("{}:{}", self.osc_host.trim(), self.osc_port);
        match target.parse::<std::net::SocketAddr>() {
            Ok(addr) => {
                self.ctl.reconnect(addr);
                let mut live = self.live.lock().unwrap();
                live.push_log("info", "osc", format!("connecting to {addr}"));
            }
            Err(_) => {
                // A hostname needs a lookup; do it here rather than in the
                // listener thread so the error can be shown.
                use std::net::ToSocketAddrs;
                match target.to_socket_addrs().ok().and_then(|mut a| a.next()) {
                    Some(addr) => {
                        self.ctl.reconnect(addr);
                        self.live.lock().unwrap().push_log(
                            "info",
                            "osc",
                            format!("connecting to {addr}"),
                        );
                    }
                    None => {
                        self.live.lock().unwrap().push_log(
                            "error",
                            "osc",
                            format!("cannot resolve {target}"),
                        );
                        return;
                    }
                }
            }
        }
        let config = OscConfig {
            host: self.osc_host.trim().to_owned(),
            osc_rx_port: self.osc_port,
            osc_metering_enabled: self
                .live
                .lock()
                .unwrap()
                .app
                .osc_metering_enabled
                .unwrap_or(0)
                != 0,
            ..OscConfig::default()
        };
        if let Err(e) = save_config(&self.config_dir, &config) {
            log::warn!("[osc] could not save the configuration: {e}");
        }
    }
}
