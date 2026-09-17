//! The speakers header's layout actions (`.speakers-layout-actions`,
//! `listeners/layout-listeners.js`, `speakers.js:303–367`, host
//! `commands/layout_io.rs`).
//!
//! Presets and Import read a layout file into Studio's own list *and* push it
//! to the renderer, because a layout that only exists on this side desyncs the
//! two: the renderer keeps rendering the old one and a save persists the wrong
//! thing. Export writes the live layout back out. Add appends a speaker at the
//! selected one's pose, which is almost always where the next one goes.

use egui::Ui;

use crate::app::StudioSpike;
use crate::host::commands::speakers;
use crate::i18n::t;
use crate::model::layouts::Speaker;

impl StudioSpike {
    /// Presets, Import, Export, Add — right of the Speakers header.
    ///
    /// All four are refused while the backend has the speakers frozen: the
    /// layout is what it is precomputing against, and changing it underneath
    /// would invalidate the table it is using.
    pub(crate) fn layout_actions(&mut self, ui: &mut Ui) {
        let frozen = {
            let live = self.host.read();
            live.app.render_backend_state.frozen_speakers
        };
        // Four buttons need ~350 points on one line, more than the panel's
        // minimum width. Laid out on a plain row they overflowed it, and the
        // panel's frame — anchored to the right edge — grew leftward to hold
        // them: every other section then looked narrower than the panel, and
        // anything placed from the configured width landed inside it. They
        // wrap instead, as the web's flex row does.
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(!frozen && !self.layout_transfer.busy(), |ui| {
                if ui
                    .button(t("config.presets"))
                    .on_hover_text(t("config.presetsHint"))
                    .clicked()
                {
                    self.layout_transfer.import(&self.host, true);
                }
                if ui.button(t("config.import")).clicked() {
                    self.layout_transfer.import(&self.host, false);
                }
                if ui.button(t("config.export")).clicked() {
                    self.layout_transfer.export(&self.host);
                }
                if ui.button(format!("+ {}", t("speaker.add"))).clicked() {
                    self.add_speaker();
                }
            });
        });
        self.layout_transfer.show_status(ui);
    }

    /// Append a speaker at the selected one's pose. A new speaker is nearly
    /// always a sibling of the one being looked at, and an origin default would
    /// put it inside the listener's head.
    fn add_speaker(&mut self) {
        let (name, speaker) = {
            let live = self.host.read();
            let speakers = live.selected_speakers();
            let base = self
                .selection
                .speaker
                .and_then(|i| speakers.get(i))
                .cloned();
            let name = format!("spk-{}", speakers.len());
            (name, base)
        };
        let base = speaker.unwrap_or_else(|| Speaker {
            id: String::new(),
            x: 0.0,
            y: 0.0,
            z: 0.0,
            azimuth_deg: 0.0,
            elevation_deg: 0.0,
            distance_m: 1.0,
            coord_mode: "polar".to_owned(),
            spatialize: 1,
            delay_ms: 0.0,
            freq_low: None,
            freq_high: None,
        });
        speakers::apply_layout_document(
            &self.host,
            serde_json::json!({ "addSpeaker": {
                "name": name,
                "azimuth": base.azimuth_deg,
                "elevation": base.elevation_deg,
                "distance": base.distance_m.max(0.01),
                "spatialize": base.spatialize != 0,
                "delayMs": base.delay_ms.max(0.0),
            }}),
        );
    }

    /// One line into the log overlay.
    pub(crate) fn log(&self, level: &str, target: &str, message: impl Into<String>) {
        crate::host::commands::app::push_log(&self.host, level, target, message.into());
    }
}
