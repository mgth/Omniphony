//! OSC connection status (`#oscPanelRoot`): the status line, the banners that
//! explain a connection that is missing, degraded or not ours, and the
//! reconnect form of `controls/osc.js`.

use std::sync::atomic::Ordering;
use std::time::Duration;

use egui::Color32;

use crate::app::StudioSpike;
use crate::host::commands::app as app_cmd;
use crate::host::commands::mpv_config::MpvOrenderState;
use crate::host::config::{load_config, save_config};
use crate::i18n::{t, tf};
use crate::ui::section::Section;
use crate::ui::{theme, widgets};

/// A packet this recent means the renderer is still talking to us.
const ALIVE: Duration = Duration::from_secs(7);
/// `status.connectingHint`'s link, unwrapped from the HTML the web renders.
const MPV_RELEASES: &str = "https://github.com/mgth/mpv-omniphony/releases";

/// The four states the web's status line reports (`app.oscStatusState`).
///
/// The web is told which one it is in by its own connection machinery; this
/// host derives it from what its listener actually knows, which is the same
/// information one layer down. `Error` is reserved for the auto-start watchdog
/// of a later pass — nothing here can distinguish a failure from a renderer
/// that has simply not come up yet.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OscState {
    Initializing,
    Connected,
    Reconnecting,
}

impl OscState {
    fn key(self) -> &'static str {
        match self {
            OscState::Initializing => "status.initializing",
            OscState::Connected => "status.connected",
            OscState::Reconnecting => "status.reconnecting",
        }
    }

    fn colour(self) -> Color32 {
        match self {
            OscState::Initializing => theme::INFO,
            OscState::Connected => theme::OK,
            OscState::Reconnecting => theme::WARN,
        }
    }
}

impl StudioSpike {
    pub(crate) fn osc_state(&self) -> OscState {
        let registered = self.osc_stats.registered.load(Ordering::Relaxed);
        let recent = self
            .osc_stats
            .since_last_packet()
            .is_some_and(|d| d < ALIVE);
        match (*self.osc_stats.target.lock().unwrap(), registered && recent) {
            (_, true) => OscState::Connected,
            (Some(_), false) => OscState::Reconnecting,
            (None, false) => OscState::Initializing,
        }
    }

    /// The status line: a coloured dot and one line of text, like the web
    /// `#oscStatus` row.
    pub(crate) fn connection_line(&mut self, ui: &mut egui::Ui) {
        let port = self.osc_stats.listen_port.load(Ordering::Relaxed);
        let state = self.osc_state();
        let mut text = t(state.key()).to_owned();
        // Name the flavour of the connected renderer, so the user can tell
        // what they are talking to: "mpv" for the embedded player, "cli" for a
        // standalone one.
        if state == OscState::Connected
            && let Some(flavour) = self.producer_flavour()
        {
            text.push_str(" · ");
            text.push_str(&flavour);
        }
        text.push_str(&format!(" · udp/{port}"));
        ui.horizontal(|ui| {
            widgets::status_dot(ui, state.colour(), text);
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
        self.connection_banners(ui);
    }

    /// `producerHost() || producerVariant()`: what the renderer says it is.
    ///
    /// The web has a third answer, "service", for the OS-managed instance. It
    /// reads a flag refreshed by a host command that shells out to the service
    /// manager; that belongs with the service controls themselves, so this
    /// reports the handshake's own answer until then.
    fn producer_flavour(&self) -> Option<String> {
        let live = self.live.lock().unwrap();
        let caps = live.app.producer_capabilities.as_ref()?;
        caps.get("host")
            .and_then(|h| h.as_str())
            .or_else(|| caps.get("variant").and_then(|v| v.as_str()))
            .map(str::to_owned)
    }

    /// Whether the renderer answering is one this Studio did not start
    /// (`rendererIsForeign`): `None` while either path is unknown, and never
    /// true for an embedded producer, which was never ours to start.
    fn renderer_is_foreign(&self) -> Option<(String, String)> {
        let live = self.live.lock().unwrap();
        let embedded = live
            .app
            .producer_capabilities
            .as_ref()
            .and_then(|c| c.get("variant"))
            .and_then(|v| v.as_str())
            == Some("embedded");
        foreign_renderer(
            embedded,
            live.app.render_executable.as_deref(),
            self.expected_orender_path.as_deref(),
        )
    }

    /// The three things that can be wrong with a connection, in the order the
    /// web shows them.
    fn connection_banners(&mut self, ui: &mut egui::Ui) {
        let bridge_error = {
            let live = self.live.lock().unwrap();
            live.app
                .render_bridge_error
                .as_deref()
                .map(str::trim)
                .filter(|e| !e.is_empty())
                .map(str::to_owned)
        };
        // While no renderer is connected, say how to bring one up — but not
        // when there is a more specific banner to show.
        if bridge_error.is_none() && self.osc_state() != OscState::Connected {
            widgets::banner_with(
                ui,
                widgets::Severity::Error,
                "No renderer connected.",
                |ui| {
                    ui.label(
                        egui::RichText::new("Run orender to create a SPDIF audio input, or launch")
                            .size(theme::FONT_SIZE_SMALL)
                            .color(theme::TEXT_MUTED),
                    );
                    // The web says this with an anchor inside the sentence;
                    // egui has no inline links, so the link is its own line.
                    ui.hyperlink_to(
                        egui::RichText::new("mpv-omniphony")
                            .size(theme::FONT_SIZE_SMALL)
                            .color(theme::ACCENT),
                        MPV_RELEASES,
                    );
                    ui.label(
                        egui::RichText::new("which embeds its own renderer.")
                            .size(theme::FONT_SIZE_SMALL)
                            .color(theme::TEXT_MUTED),
                    );
                },
            );
        }
        // The renderer came up without its decoder bridge: it is running, and
        // it has no spatial audio. The underlying error is the useful part.
        if let Some(error) = bridge_error {
            widgets::banner(
                ui,
                widgets::Severity::Error,
                t("status.bridgeErrorTitle"),
                Some(&error),
            );
        }
        // Attached to someone else's renderer: the connection looks perfectly
        // healthy, so this has to be said out loud or every control it does not
        // implement just vanishes.
        if let Some((running, expected)) = self.renderer_is_foreign() {
            widgets::banner(
                ui,
                widgets::Severity::Warning,
                t("status.foreignRendererTitle"),
                Some(&tf(
                    "status.foreignRendererDetail",
                    &[("running", &running), ("expected", &expected)],
                )),
            );
        }
    }
}

impl StudioSpike {
    /// The OSC configuration form (`#oscConfigForm`, `controls/osc.js`): the
    /// renderer's host and port, this client's listen port and the metering
    /// switch, with a Connect button that re-registers.
    pub(crate) fn osc_section(&mut self, ui: &mut egui::Ui) {
        let listen_port = self.osc_stats.listen_port.load(Ordering::Relaxed);
        let shown = Section::new("oscSection", "osc.configTitle")
            .info("osc")
            .summary(format!("{}:{}", self.osc_host, self.osc_port))
            .show(ui, |ui| {
                widgets::label_row_help(ui, t("osc.host"), "help.osc.host", |ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.osc_host).desired_width(140.0));
                });
                widgets::label_row_help(
                    ui,
                    t("osc.omniphonyPort"),
                    "help.osc.omniphonyPort",
                    |ui| {
                        ui.add(egui::DragValue::new(&mut self.osc_port).range(1..=65535));
                    },
                );
                widgets::label_row_help(ui, t("osc.listenPort"), "help.osc.listenPort", |ui| {
                    ui.label(
                        egui::RichText::new(listen_port.to_string())
                            .monospace()
                            .color(theme::TEXT_MUTED),
                    );
                });
                self.host_switches(ui);
                if ui.button(t("osc.connect")).clicked() {
                    self.connect();
                }
                ui.separator();
                self.renderer_controls(ui);
            });
        // mpv.conf can change outside Studio (a hand edit, another machine),
        // so it is re-read every time the section opens rather than cached.
        if shown.is_none() {
            self.mpv_orender = None;
        }
    }

    /// `#oscMeteringToggle` and `#oscMeteringRateSelect`, which the web puts
    /// at the head of the Objects section, over the meters they feed: whether
    /// this client receives levels at all, and how often the renderer
    /// publishes them. The switch is saved to `osc_config.json`, as the
    /// Tauri host's `control_osc_metering` does, so it holds across
    /// restarts; the rate is the renderer's own, echoed in its snapshot.
    pub(crate) fn metering_row(&mut self, ui: &mut egui::Ui) {
        const RATES_HZ: [u32; 5] = [10, 20, 50, 100, 200];
        let (metering_on, rate) = {
            let live = self.live.lock().unwrap();
            (
                live.app.osc_metering_enabled.unwrap_or(0) != 0,
                live.app.meter_rate_hz.map_or(50, |hz| hz.round() as u32),
            )
        };
        let mut metering = metering_on;
        let mut chosen = rate;
        let toggled = widgets::label_row_help(ui, t("osc.metering"), "help.osc.metering", |ui| {
            widgets::bounded_combo(ui, 80.0, |ui, w| {
                egui::ComboBox::from_id_salt("meter-rate")
                    .selected_text(format!("{rate} Hz"))
                    .width(w)
                    .truncate()
                    .show_ui(ui, |ui| {
                        for hz in RATES_HZ {
                            ui.selectable_value(&mut chosen, hz, format!("{hz} Hz"));
                        }
                    })
                    .response
                    .on_hover_text(
                        "Audio meter publish rate (Hz) — drives the level meters only. \
                         Diag publication has its own rate in the diag plot controls.",
                    );
            });
            widgets::switch(ui, &mut metering).changed()
        });
        if toggled {
            app_cmd::set_metering_enabled(&self.host, metering);
            self.log(
                "info",
                "osc",
                t(if metering {
                    "log.oscMeteringEnabled"
                } else {
                    "log.oscMeteringDisabled"
                }),
            );
        }
        if chosen != rate {
            app_cmd::set_meter_rate_hz(&self.host, chosen as f32);
        }
    }

    /// The switches under the form: auto-start and keep-alive for a local
    /// renderer, and the mpv.conf opt-in. Each takes effect when flipped —
    /// the first two are read by the watchdog and at quit, the third edits
    /// mpv.conf — so none waits for Connect.
    fn host_switches(&mut self, ui: &mut egui::Ui) {
        let mut auto_start = self.osc_auto_start;
        if widgets::switch_row_help(
            ui,
            t("osc.autoStartRenderer"),
            "help.osc.autoStartRenderer",
            &mut auto_start,
        ) {
            self.osc_auto_start = auto_start;
            self.save_host_switches();
            if auto_start {
                // Turning it back on after installing the service lifts the
                // suppression the install set; the watchdog decides the rest.
                app_cmd::resume_watchdog(&self.host);
            }
        }
        let mut keep_alive = self.osc_keep_alive;
        if widgets::switch_row_help(
            ui,
            t("osc.keepRendererAlive"),
            "help.osc.keepRendererAlive",
            &mut keep_alive,
        ) {
            self.osc_keep_alive = keep_alive;
            self.save_host_switches();
        }

        let status = self
            .mpv_orender
            .get_or_insert_with(crate::host::commands::mpv_config::mpv_orender_status)
            .clone();
        let (mut enabled, conflict) = match &status {
            Ok(status) => (
                status.state == MpvOrenderState::Enabled,
                status.state == MpvOrenderState::Conflict,
            ),
            Err(_) => (false, false),
        };
        // A hand-written `ad=` wins: Studio never edits a line it did not
        // write, so the switch is inert until the user resolves it.
        let flipped = ui
            .add_enabled_ui(!conflict && status.is_ok(), |ui| {
                widgets::switch_row_help(
                    ui,
                    t("osc.mpvOrender"),
                    "help.osc.mpvOrender",
                    &mut enabled,
                )
            })
            .inner;
        if flipped {
            match crate::host::commands::mpv_config::mpv_orender_set(enabled) {
                Ok(status) => {
                    let key = if enabled {
                        "log.mpvOrenderEnabled"
                    } else {
                        "log.mpvOrenderDisabled"
                    };
                    self.log("info", "mpv", tf(key, &[("path", &status.path)]));
                    self.mpv_orender = Some(Ok(status));
                }
                Err(error) => {
                    // The file was not changed: re-read it, which puts the
                    // switch back and shows a refused conflict as such.
                    self.log(
                        "error",
                        "mpv",
                        tf("log.mpvOrenderFailed", &[("error", &error)]),
                    );
                    self.mpv_orender = None;
                }
            }
        }
        let (note, colour) = match &status {
            Ok(status) if conflict => (
                tf(
                    "osc.mpvOrenderConflict",
                    &[
                        (
                            "line",
                            &status
                                .conflict_line
                                .map_or_else(|| "?".to_owned(), |line| line.to_string()),
                        ),
                        ("text", status.conflict_text.as_deref().unwrap_or("").trim()),
                    ],
                ),
                theme::WARN,
            ),
            Ok(status) => (
                tf("osc.mpvOrenderPath", &[("path", &status.path)]),
                theme::TEXT_MUTED,
            ),
            Err(error) => (error.clone(), theme::WARN),
        };
        ui.add(
            egui::Label::new(
                egui::RichText::new(note)
                    .size(theme::FONT_SIZE_SMALL)
                    .color(colour),
            )
            .wrap(),
        );
    }

    /// Write the two host switches into `osc_config.json`, leaving every
    /// other field as the file has it.
    fn save_host_switches(&self) {
        let mut config = load_config(&self.config_dir);
        config.auto_start_renderer = self.osc_auto_start;
        config.keep_renderer_alive_on_quit = self.osc_keep_alive;
        if let Err(e) = save_config(&self.config_dir, &config) {
            log::warn!("[osc] could not save the configuration: {e}");
        }
    }

    /// At quit, take a renderer this Studio launched down with it: the core
    /// owns the child and the two-second grace.
    pub(crate) fn stop_launched_renderer(&mut self) {
        crate::host::commands::orender::stop_launched_renderer(&self.host);
    }

    /// Point the client at the configured renderer and save the choice, like
    /// `save_osc_config` + the host's `Reconnect`.
    fn connect(&mut self) {
        if app_cmd::connect_to(&self.host, &self.osc_host, self.osc_port).is_err() {
            return;
        }
        // Only the form's own fields: starting from the defaults instead
        // turned auto-start back on and forgot the import directory on every
        // Connect.
        let mut config = load_config(&self.config_dir);
        config.host = self.osc_host.trim().to_owned();
        config.osc_rx_port = self.osc_port;
        config.osc_metering_enabled = self
            .live
            .lock()
            .unwrap()
            .app
            .osc_metering_enabled
            .unwrap_or(0)
            != 0;
        if let Err(e) = save_config(&self.config_dir, &config) {
            log::warn!("[osc] could not save the configuration: {e}");
        }
    }
}

/// `rendererIsForeign`, as a rule on its own: unknown while either path is
/// missing, never true for an embedded producer — which was never ours to
/// start — and true only when the two paths disagree.
fn foreign_renderer(
    embedded: bool,
    running: Option<&str>,
    expected: Option<&str>,
) -> Option<(String, String)> {
    if embedded {
        return None;
    }
    let running = running?.trim();
    let expected = expected?.trim();
    if running.is_empty() || expected.is_empty() || running == expected {
        return None;
    }
    Some((running.to_owned(), expected.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_foreign_renderer_is_only_claimed_when_both_paths_are_known_and_differ() {
        let ours = Some("/usr/bin/orender");
        let theirs = Some("/opt/other/orender");
        assert!(foreign_renderer(false, theirs, ours).is_some());
        assert!(foreign_renderer(false, ours, ours).is_none());
        // Half the answer is no answer: an unknown path must not be reported
        // as a mismatch.
        assert!(foreign_renderer(false, None, ours).is_none());
        assert!(foreign_renderer(false, theirs, None).is_none());
        assert!(foreign_renderer(false, Some("  "), ours).is_none());
        // An embedded producer is never one this Studio started.
        assert!(foreign_renderer(true, theirs, ours).is_none());
    }
}
