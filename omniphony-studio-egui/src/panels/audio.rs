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
use crate::ui::group::Group;
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
        // The meter sits in the header, as `.master-header` has it: the bar
        // between the title and the readout, in view whether the section is
        // open or folded. The bar follows the peak, the cursor is the held
        // peak, the readout is the RMS. Bar and readout are one widget rather
        // than a header widget and the section's summary, so the readout gets
        // the fixed box the web gives it instead of a summary's own metrics.
        let peak = meter.as_ref().map_or(METER_DB_MIN, |m| m.peak_dbfs);
        let hold = hold.unwrap_or(peak);
        let readout = format_level(meter.as_ref());
        Section::new("masterSection", "master.title")
            .default_open(true)
            .help("help.master.gain")
            .header_widget(move |ui| master_meter(ui, peak, hold, &readout))
            .show(ui, |ui| {
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
                                widgets::stepped(egui::Slider::new(&mut value, 0.0..=2.0), 0.01)
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

                // `#autoGainSection`: the switch in the bar with the clip dot
                // beside it — the web draws the dot in the label — and the
                // ceiling in the inset.
                let mut on = auto_gain;
                let mut db = ceiling as f32;
                let ceiling_changed = Group::new(t("autoGain.title"))
                    .help("help.master.autoGain")
                    .actions(|ui| {
                        widgets::switch(ui, &mut on);
                        clip_indicator(ui, clipping);
                    })
                    .show(ui, |ui| {
                        ui.add_enabled_ui(ready, |ui| {
                            widgets::value_slider_help(
                                ui,
                                t("autoGain.ceiling"),
                                "help.master.ceiling",
                                &mut db,
                                -12.0..=0.0,
                                0.1,
                                |v| format!("{v:.1} dB"),
                            )
                        })
                        .inner
                    });
                if on != auto_gain && ready {
                    gain::control_auto_gain(&self.host, i32::from(on));
                }
                if ceiling_changed {
                    gain::control_auto_gain_ceiling(&self.host, db);
                }
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

/// The readout's box, in monospace advances. It is fixed on purpose — the
/// web's `.fixed-metric` pins it at `8ch` with `tabular-nums` — because a box
/// that fits the current reading would drag the bar a few points left and
/// right every time the number gained or lost a digit. Nine holds the widest
/// reading there can be, `-100.0 dB`: the parser clamps `rms_dbfs` to
/// -100…0 dBFS, and the `— dB` placeholder is shorter still.
const READOUT_ADVANCES: f32 = 9.0;

/// The master meter drawn in the section header, laid out as `.master-header`
/// is: the readout takes its fixed box at the right end, and the bar takes
/// everything else (`flex: 1 1 auto; min-width: 0`). `level_meter_sized`
/// floors the bar at its own height, so an overlay dragged to its narrowest
/// shortens the bar rather than pushing the readout out of the panel.
fn master_meter(ui: &mut Ui, peak: f64, hold: f64, readout: &str) {
    crate::ui::meter::row_with_readout(ui, READOUT_ADVANCES, readout, |ui, width| {
        crate::ui::meter::level_meter_sized(
            ui,
            width,
            meter_fraction(peak),
            (hold > METER_DB_MIN).then(|| meter_fraction(hold)),
            hold >= 0.0,
            None,
        );
    });
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

#[cfg(test)]
mod tests {
    use super::{READOUT_ADVANCES, format_level};
    use crate::model::app_state::Meter;

    /// The readout's box is a fixed number of advances so the bar beside it
    /// never moves, which only holds while every reading fits in it. The
    /// parser clamps `rms_dbfs` to -100…0 dBFS, so the widest is the floor.
    #[test]
    fn every_reading_fits_the_fixed_readout_box() {
        let box_chars = READOUT_ADVANCES as usize;
        let reading = |db| {
            format_level(Some(&Meter {
                peak_dbfs: db,
                rms_dbfs: db,
            }))
            .chars()
            .count()
        };
        assert_eq!(reading(-100.0), box_chars, "the floor is what the box fits");
        for db in [0.0, -0.1, -9.9, -12.3, -60.0, -99.9] {
            assert!(reading(db) <= box_chars, "{db} dBFS overflows the box");
        }
        // The "— dB" placeholder is shorter still.
        assert!(format_level(None).chars().count() <= box_chars);
    }
}
