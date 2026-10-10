//! The Essentials view: what a listener needs, with the full control board
//! behind the Advanced switch (#679).
//!
//! Essentials keeps the connection, the output device, the speaker layout or
//! the headphones, the master level, and the output mode with the HRTF and
//! the channel placement it implies (read-only: the choice is Advanced). The
//! sections that stay are the same sections, with their tuning rows left out
//! (`self.advanced` in their bodies); the renderer section, which is mostly
//! tuning, gives way to the Listening section below. The switch is display
//! state: kept the moment it changes (docs/persistence-policy.md).

use egui::Ui;

use crate::app::StudioSpike;
use crate::host::channels::{Family, ModeSource, family_label, family_placement, playing_family};
use crate::i18n::t;
use crate::model::binaural::OutputMode;
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
            .summary(t(mode.i18n_key()))
            .show(ui, |ui| {
                self.output_mode_row(ui);
                if embedded {
                    widgets::note(ui, t("outputMode.mpvNote"));
                }
                if mode != OutputMode::Speaker {
                    self.essentials_hrtf(ui);
                }
                self.placement_readout(ui);
            });
    }

    /// Which placement the fixed channels get, and why: for the stream that
    /// is playing, else for the formats that have no choice of their own. A
    /// readout; the choice is made in the Advanced view's placement group.
    fn placement_readout(&mut self, ui: &mut Ui) {
        let (family, placement, family_name) = {
            let live = self.host.read();
            let family = playing_family(&live.app).unwrap_or(Family::GENERIC);
            (
                family,
                family_placement(&live.app, family),
                family_label(&live.app, family),
            )
        };
        let why = match placement.mode_source {
            // The generic family's own choice is every format's.
            ModeSource::Own if family.is_generic() => t("placement.why.generic"),
            ModeSource::Own => t("placement.why.own"),
            ModeSource::Generic => t("placement.why.generic"),
            ModeSource::Headphones => t("placement.why.headphones"),
            ModeSource::Family => t("placement.why.family"),
        }
        .replace("{family}", &family_name);
        let mode = t(placement.effective_mode.i18n_key());
        widgets::label_row_help(
            ui,
            t("essentials.placement"),
            "help.essentials.placement",
            |ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!("{mode} · {why}"))
                            .size(crate::ui::theme::FONT_SIZE_SMALL)
                            .color(crate::ui::theme::TEXT_MUTED),
                    )
                    .truncate(),
                );
            },
        );
    }
}
