//! The brand row and the About modal (`#aboutOpenArea`, `#aboutModal`,
//! `app.js:300–319`, `controls/config.js:55–100`).
//!
//! Half of it is fixed at build time — the name, the version, the licence and
//! the repository come from the host's own `get_about_info` — and half of it is
//! whatever the renderer last said about itself. That second half is the reason
//! this box exists: it answers "which renderer am I actually driving, and which
//! configuration is it running on", which is the first question when something
//! sounds wrong.

use egui::{RichText, Ui};

use crate::app::StudioSpike;
use crate::i18n::t;
use crate::ui::{theme, widgets};

/// A configuration the renderer could not read is worth more than a warning
/// colour, so the web gives it its own red rather than the panel's error red.
const CONFIG_ERROR: egui::Color32 = egui::Color32::from_rgb(0xff, 0x76, 0x76);

/// What the renderer has told us about itself, read once per frame the modal is
/// open rather than held: all of it arrives on OSC and can change under us.
struct RendererFacts {
    version: Option<String>,
    abi: Option<String>,
    executable: Option<String>,
    config_path: Option<String>,
    config_status: Option<String>,
    connected: bool,
}

impl StudioSpike {
    /// The brand row. Clicking it — or the `?` beside the connection line —
    /// opens the About box, as in the web.
    pub(crate) fn brand_row(&mut self, ui: &mut Ui) {
        let area = ui
            .vertical(|ui| {
                let title = ui.add(
                    egui::Label::new(RichText::new(t("app.title")).size(theme::FONT_SIZE_TITLE))
                        .sense(egui::Sense::click())
                        .selectable(false),
                );
                let subtitle = ui.add(
                    egui::Label::new(
                        RichText::new(t("app.subtitle"))
                            .size(theme::FONT_SIZE_SMALL)
                            .color(theme::TEXT_MUTED),
                    )
                    .sense(egui::Sense::click())
                    .selectable(false),
                );
                title.union(subtitle)
            })
            .inner;
        if area.clicked() {
            self.about_open = true;
        }
        area.on_hover_text(t("about.open"));
    }

    pub(crate) fn about_modal(&mut self, ctx: &egui::Context) {
        if !self.about_open {
            return;
        }
        let info = serde_json::to_value(crate::host::commands::app::get_about_info())
            .unwrap_or(serde_json::Value::Null);
        let field = |key: &str| {
            info.get(key)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned()
        };
        let facts = {
            let live = self.live.lock().unwrap();
            RendererFacts {
                version: live.app.render_version.clone(),
                abi: live.app.render_abi.clone(),
                executable: live.app.render_executable.clone(),
                config_path: live.app.render_config_path.clone(),
                config_status: live.app.render_config_status.clone(),
                connected: live.app.osc_status.as_deref() == Some("connected"),
            }
        };
        let modal = egui::Modal::new(egui::Id::new("about-modal"))
            .frame(widgets::modal_frame())
            .show(ctx, |ui| {
                ui.set_max_width(420.0);
                ui.label(
                    RichText::new(t("about.title"))
                        .size(theme::FONT_SIZE_TITLE)
                        .color(theme::TEXT_STRONG),
                );
                ui.label(RichText::new(field("name")).color(theme::TEXT_STRONG));
                let description = match field("description") {
                    d if d.is_empty() => t("about.descriptionFallback").to_owned(),
                    d => d,
                };
                ui.label(RichText::new(description).color(theme::TEXT_MUTED));
                ui.add_space(theme::PANEL_GAP);

                row(ui, t("about.version"), |ui| {
                    ui.label(RichText::new(field("version")).monospace());
                });
                row(ui, t("about.license"), |ui| {
                    ui.label(RichText::new(field("license")).monospace());
                });
                row(ui, t("about.repository"), |ui| {
                    let url = field("repository_url");
                    ui.hyperlink_to(RichText::new(url.clone()).color(theme::ACCENT), url);
                });
                row(ui, t("about.rendererVersion"), |ui| {
                    renderer_version(ui, &facts);
                });
                row(ui, t("about.configPath"), |ui| {
                    config_path(ui, &facts);
                });

                ui.add_space(theme::PANEL_GAP);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(t("common.close")).clicked() {
                        self.about_open = false;
                    }
                });
            });
        if modal.should_close() {
            self.about_open = false;
        }
    }
}

/// Which renderer, and which ABI it speaks. The executable's path is the
/// tooltip rather than a line of its own: it is long, and it only matters once
/// the version raises a question.
fn renderer_version(ui: &mut Ui, facts: &RendererFacts) {
    let Some(version) = facts.version.as_deref().filter(|v| !v.is_empty()) else {
        ui.label(RichText::new("—").color(theme::TEXT_FAINT));
        return;
    };
    let mut text = version.to_owned();
    if let Some(abi) = facts.abi.as_deref().filter(|a| !a.is_empty()) {
        text.push_str(" · ABI ");
        text.push_str(abi);
    }
    let label = ui.label(RichText::new(&text).monospace());
    if let Some(executable) = facts.executable.as_deref().filter(|e| !e.is_empty()) {
        label.on_hover_text(format!("{text}\n{executable}"));
    }
}

/// The configuration the renderer is running on — and, when it could not read
/// it, that it is running on defaults instead. A renderer silently on defaults
/// is the explanation for a whole class of "why does it not sound like the
/// settings say" questions.
fn config_path(ui: &mut Ui, facts: &RendererFacts) {
    let (text, colour, monospace) = config_line(
        facts.config_path.as_deref().unwrap_or_default(),
        facts.config_status.as_deref().unwrap_or_default(),
        facts.connected,
    );
    let mut rich = RichText::new(text);
    if monospace {
        rich = rich.monospace();
    }
    ui.label(rich.color(colour));
}

/// The configuration line's text and colour: the path, the path plus why it
/// could not be used, the "running on built-in defaults" warning, or nothing
/// known at all.
fn config_line(path: &str, status: &str, connected: bool) -> (String, egui::Color32, bool) {
    let failure = match status {
        "missing" => Some(t("about.configMissing")),
        "parse_error" => Some(t("about.configParseError")),
        _ => None,
    };
    if !path.is_empty() {
        return match failure {
            Some(reason) => (format!("{path} — {reason}"), CONFIG_ERROR, true),
            None => (path.to_owned(), theme::TEXT, true),
        };
    }
    // Connected and still no path: the renderer read no file at all, which is
    // a different thing from "Studio has not been told yet".
    if connected {
        return (t("about.configDefaults").to_owned(), theme::WARN, false);
    }
    ("—".to_owned(), theme::TEXT_FAINT, false)
}

/// One label/value line of the box.
fn row(ui: &mut Ui, label: &str, value: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        // A fixed, left-aligned label column, so the values line up in one
        // readable stack instead of drifting with the label widths.
        let height = ui.spacing().interact_size.y;
        ui.allocate_ui_with_layout(
            egui::vec2(78.0, height),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.label(
                    RichText::new(label)
                        .size(theme::FONT_SIZE_SMALL)
                        .color(theme::TEXT_MUTED),
                );
            },
        );
        value(ui);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_config_line_says_when_the_renderer_fell_back_to_defaults() {
        // A path the renderer read: just the path.
        let (text, colour, _) = config_line("/etc/omniphony/config.yaml", "ok", true);
        assert_eq!(text, "/etc/omniphony/config.yaml");
        assert_eq!(colour, theme::TEXT);
        // A path it could not read: the path *and* what went wrong, in red.
        let (text, colour, _) = config_line("/etc/omniphony/config.yaml", "missing", true);
        assert!(text.starts_with("/etc/omniphony/config.yaml — "));
        assert!(text.ends_with(t("about.configMissing")));
        assert_eq!(colour, CONFIG_ERROR);
        let (text, _, _) = config_line("/x.yaml", "parse_error", true);
        assert!(text.ends_with(t("about.configParseError")));
        // Connected with no path at all: running on built-in defaults, which
        // is worth an amber warning.
        let (text, colour, _) = config_line("", "", true);
        assert_eq!(text, t("about.configDefaults"));
        assert_eq!(colour, theme::WARN);
        // Not connected: nothing is known, and nothing is claimed.
        let (text, colour, _) = config_line("", "", false);
        assert_eq!(text, "—");
        assert_eq!(colour, theme::TEXT_FAINT);
    }
}
