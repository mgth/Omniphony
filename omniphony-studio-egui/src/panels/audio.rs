//! The Master section of the audio panel (`#masterSection`,
//! `ui/audio-panel.js`, `controls/master.js`): the master meter with its
//! peak-hold cursor, the gain slider, the clip indicator and the auto-gain
//! block.

use egui::{Color32, RichText, Ui};

use crate::app::StudioSpike;
use crate::host::commands::gain;
use crate::host::peak_hold::{METER_DB_MIN, db_to_meter_percent};
use crate::i18n::t;
use crate::model::app_state::Meter;
use crate::ui::section::Section;
use crate::ui::{theme, widgets};

/// `formatLevel`: one decimal, or the "— dB" placeholder.
pub fn format_level(meter: Option<&Meter>) -> String {
    match meter {
        Some(m) => format!("{:.1} dB", m.rms_dbfs),
        None => t("status.masterMeter").to_owned(),
    }
}

/// `formatLinearAsDb`: an amplitude ratio as a dB label.
pub fn format_linear_as_db(value: Option<f64>) -> String {
    match value {
        Some(v) if v.is_finite() && v > 0.0 => format!("{:.1} dB", 20.0 * v.log10()),
        Some(_) => "-∞ dB".to_owned(),
        None => "—".to_owned(),
    }
}

/// Map a dBFS value to the meter's 0..1 fill, `-60 … +6` with 0 dBFS at 90.9 %.
pub fn meter_fraction(db: f64) -> f32 {
    (db_to_meter_percent(db) / 100.0) as f32
}

impl StudioSpike {
    pub(crate) fn master_section(&mut self, ui: &mut egui::Ui) {
        let (meter, hold, gain, auto_gain, ceiling, ready, realtime, clipping) = {
            let live = self.host.read();
            let meter = live.app.master_level.clone();
            (
                meter,
                live.peak_hold("master"),
                live.app.master_gain,
                live.app.auto_gain.unwrap_or(false),
                live.app.auto_gain_ceiling_db.unwrap_or(-1.0),
                live.app.osc_snapshot_ready,
                supports_realtime(&live.app.producer_capabilities, "master_gain"),
                live.clip
                    .is_some_and(|(_, at)| at.elapsed() < std::time::Duration::from_secs(1)),
            )
        };
        Section::new("masterSection", "master.title")
            .default_open(true)
            .help("help.master.gain")
            .summary(format_level(meter.as_ref()))
            .show(ui, |ui| {
                // Meter: the bar follows the peak, the readout is the RMS, the
                // cursor is the held peak.
                ui.horizontal(|ui| {
                    let peak = meter.as_ref().map_or(METER_DB_MIN, |m| m.peak_dbfs);
                    let hold = hold.unwrap_or(peak);
                    crate::ui::meter::level_meter(
                        ui,
                        meter_fraction(peak),
                        (hold > METER_DB_MIN).then(|| meter_fraction(hold)),
                        hold >= 0.0,
                        None,
                    );
                    ui.label(
                        RichText::new(format_level(meter.as_ref()))
                            .monospace()
                            .color(theme::TEXT_STRONG),
                    );
                });

                // Gain: 0..2 linear, sent as a realtime message with a
                // sequence number so the renderer can drop stale updates.
                let enabled = ready && gain.is_some() && realtime;
                let mut value = gain.unwrap_or(1.0) as f32;
                ui.add_enabled_ui(enabled, |ui| {
                    ui.horizontal(|ui| {
                        // The web row is slider + value only, no label.
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                RichText::new(format_linear_as_db(gain))
                                    .monospace()
                                    .color(theme::TEXT_STRONG),
                            );
                            let response = ui.add(
                                egui::Slider::new(&mut value, 0.0..=2.0)
                                    .step_by(0.01)
                                    .show_value(false),
                            );
                            if response.double_clicked() {
                                value = 1.0;
                                self.set_master_gain(1.0);
                            } else if response.changed() {
                                self.set_master_gain(value);
                            }
                        });
                    });
                });

                ui.horizontal(|ui| {
                    clip_indicator(ui, clipping);
                    let mut on = auto_gain;
                    if widgets::switch_row_help(
                        ui,
                        t("autoGain.title"),
                        "help.master.autoGain",
                        &mut on,
                    ) && ready
                    {
                        gain::control_auto_gain(&self.host, i32::from(on));
                    }
                });
                let mut db = ceiling as f32;
                ui.add_enabled_ui(ready, |ui| {
                    if widgets::value_slider_help(
                        ui,
                        t("autoGain.ceiling"),
                        "help.master.ceiling",
                        &mut db,
                        -12.0..=0.0,
                        0.1,
                        |v| format!("{v:.1} dB"),
                    ) {
                        gain::control_auto_gain_ceiling(&self.host, db);
                    }
                });
            });
    }

    /// The realtime master gain: clamped, applied and stamped by the core's
    /// command, which owns the sequence counter.
    fn set_master_gain(&mut self, gain: f32) {
        gain::control_master_gain(&self.host, gain);
    }
}

/// `supportsRealtimeKey`: the renderer lists the realtime controls it honours.
pub fn supports_realtime(capabilities: &Option<serde_json::Value>, key: &str) -> bool {
    capabilities
        .as_ref()
        .and_then(|c| c.get("realtime"))
        .and_then(|r| r.as_array())
        .is_some_and(|values| values.iter().any(|v| v.as_str() == Some(key)))
}

/// `.clip-indicator`: a 9 px dot that turns red for a second on every clip.
fn clip_indicator(ui: &mut Ui, active: bool) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(9.0, 9.0), egui::Sense::hover());
    let painter = ui.painter();
    painter.circle_filled(
        rect.center(),
        4.5,
        if active {
            theme::CLIP
        } else {
            Color32::from_rgba_unmultiplied(255, 255, 255, 46)
        },
    );
    if active {
        painter.circle_stroke(
            rect.center(),
            6.0,
            egui::Stroke::new(1.0, theme::CLIP.gamma_multiply(0.6)),
        );
    }
    response.on_hover_text("Clip");
}
