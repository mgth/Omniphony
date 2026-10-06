//! The Essentials view: what a listener needs, with the full control board
//! behind the Advanced switch (#679).
//!
//! Essentials keeps the connection, the output device, the speaker layout or
//! the headphones, the master level, and the output mode with the HRTF. The
//! sections that stay are the same sections, with their tuning rows left out
//! (`self.advanced` in their bodies); the renderer section, which is mostly
//! tuning, gives way to the Listening section below. The switch is display
//! state: kept the moment it changes (docs/persistence-policy.md).

use egui::Ui;

use crate::app::StudioSpike;
use crate::i18n::t;
use crate::panels::renderer::OutputMode;
use crate::ui::section::Section;
use crate::ui::widgets;

impl StudioSpike {
    /// The Advanced switch, with the application's other settings at the
    /// head of the left overlay: above every section it shows or hides, so
    /// flipping it never moves the switch itself.
    pub(crate) fn view_mode_row(&mut self, ui: &mut Ui) {
        let mut on = self.advanced;
        if widgets::switch_row_help(
            ui,
            t("essentials.advanced"),
            "help.essentials.advanced",
            &mut on,
        ) {
            self.advanced = on;
        }
    }

    /// Speakers or headphones, and on headphones which HRTF: the part of the
    /// renderer section a listener chooses.
    pub(crate) fn listening_section(&mut self, ui: &mut Ui) {
        let (mode, embedded) = {
            let live = self.host.read();
            (
                OutputMode::from_state(live.app.binaural.as_ref()),
                live.app
                    .producer_capabilities
                    .as_ref()
                    .and_then(|c| c.get("variant"))
                    .and_then(|v| v.as_str())
                    == Some("embedded"),
            )
        };
        Section::new("listeningSection", "essentials.listening")
            .icon(&crate::ui::icons::SECTION_RENDERER)
            .default_open(true)
            .summary(mode.label())
            .show(ui, |ui| {
                self.output_mode_row(ui);
                if embedded {
                    widgets::note(ui, t("outputMode.mpvNote"));
                }
                if mode != OutputMode::Speaker {
                    self.essentials_hrtf(ui);
                }
            });
    }
}
