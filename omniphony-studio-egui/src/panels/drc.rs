//! DRC and loudness (`#drcSection`, `controls/drc.js`): the compression mode
//! and its weight, the loudness switch, and the gain gauge the renderer
//! reports while metering is on.

use egui::{Color32, RichText, Ui};

use crate::app::StudioSpike;
use crate::host::commands::{engine, gain};
use crate::i18n::t;
use crate::ui::group::Group;
use crate::ui::section::Section;
use crate::ui::{theme, widgets};

/// Gauge colour by correction, from `controls/drc.js`.
fn gauge_colour(db: f64) -> Color32 {
    if db > 1.0 {
        Color32::from_rgb(0x33, 0xb5, 0xe5)
    } else if db < -12.0 {
        Color32::from_rgb(0xff, 0x44, 0x44)
    } else if db < -6.0 {
        Color32::from_rgb(0xff, 0xbb, 0x33)
    } else {
        Color32::from_rgb(0x00, 0xc8, 0x51)
    }
}

impl StudioSpike {
    pub(crate) fn drc_section(&mut self, ui: &mut Ui) {
        let (mode, modes, weight, loudness, metering, gain, source) = {
            let live = self.host.read();
            (
                live.app
                    .drc_mode
                    .clone()
                    .unwrap_or_else(|| "Off".to_owned()),
                live.app.supported_drc_modes.clone(),
                live.app.drc_weight.unwrap_or(1.0),
                live.app.loudness.unwrap_or(0) != 0,
                live.app.osc_metering_enabled.unwrap_or(0) != 0,
                live.drc_gain,
                live.app.loudness_source,
            )
        };
        let summary = format!(
            "{mode} ({}%) | Loudness {}",
            (weight * 100.0).round(),
            if loudness { "ON" } else { "OFF" }
        );
        let mut section = Section::new("drcSection", "section.drc")
            .info("drc")
            .summary(summary);
        // The gauge sits in the header, as `#drcGaugeRow` does, so it stays
        // in view with the section folded. It only means something while the
        // renderer is metering, so it follows that switch like the web does.
        if metering {
            section = section.header_widget(move |ui| drc_gauge(ui, gain));
        }
        section.show(ui, |ui| {
            // The mode list is the renderer's, plus whatever it currently
            // reports, so an unknown mode is never silently dropped.
            let mut options = modes.clone();
            if !options.iter().any(|m| *m == mode) {
                options.push(mode.clone());
            }
            if options.is_empty() {
                options.push("Off".to_owned());
            }
            // DRC: the mode in the bar, its weight in the inset.
            let mut chosen = mode.clone();
            let mut percent = (weight * 100.0).round();
            let weight_changed = Group::new(t("input.drc"))
                .help("help.drc.mode")
                .actions(|ui| {
                    widgets::bounded_combo(ui, 120.0, |ui, w| {
                        egui::ComboBox::from_id_salt("drc-mode")
                            .selected_text(&mode)
                            .width(w)
                            .truncate()
                            .show_ui(ui, |ui| {
                                for option in &options {
                                    ui.selectable_value(&mut chosen, option.clone(), option);
                                }
                            })
                    });
                })
                .show(ui, |ui| {
                    widgets::value_slider_help(
                        ui,
                        t("input.drc_weight"),
                        "help.drc.weight",
                        &mut percent,
                        0.0..=100.0,
                        1.0,
                        |v| format!("{v:.0}%"),
                    )
                });
            if chosen != mode {
                engine::control_drc_mode(&self.host, chosen);
            }
            if weight_changed {
                engine::control_drc_weight(&self.host, (percent / 100.0) as f32);
            }

            // Loudness: the switch in the bar, what it measures in the inset.
            let mut on = loudness;
            Group::new(t("section.loudness"))
                .help("help.drc.loudness")
                .actions(|ui| {
                    widgets::switch(ui, &mut on);
                })
                .show(ui, |ui| {
                    for line in loudness_lines(source, gain) {
                        widgets::note(ui, &line);
                    }
                });
            if on != loudness {
                gain::control_loudness(&self.host, i32::from(on));
            }
        });
    }
}

/// The three lines under the loudness switch: what the source declared, what
/// it becomes, and the correction applied.
fn loudness_lines(source: Option<f64>, gain: Option<f64>) -> Vec<String> {
    let em_dash = "—".to_owned();
    let source_text = source.map(|v| format!("{v:.0}")).unwrap_or(em_dash.clone());
    let target = match (source, gain) {
        (Some(s), Some(g)) if g > 0.0 => format!("{:.0}", s + 20.0 * g.log10()),
        _ => em_dash.clone(),
    };
    let correction = match gain {
        Some(g) if g > 0.0 => format!("{g:.2} ({:.1} dB)", 20.0 * g.log10()),
        _ => em_dash,
    };
    vec![
        format!("source loudness: {source_text} dBFS"),
        format!("target loudness: {target} dBFS"),
        format!("correction: {correction}"),
    ]
}

/// `linearToDb` with the web's floor: the reported gain in dB, -100 for
/// silence or nonsense. `None` is a gauge nothing has been reported to yet,
/// which the web leaves empty at "0.0 dB" rather than full at the floor.
fn reading_db(gain: Option<f64>) -> Option<f64> {
    gain.map(|g| {
        let db = if g > 0.0 { 20.0 * g.log10() } else { -100.0 };
        if db.is_finite() { db } else { -100.0 }
    })
}

/// A right-anchored bar: the correction grows leftwards from unity. It shares
/// the header with the summary (`flex: 1 1 auto; min-width: 60px`), so it
/// takes a share of what is left of the row rather than all of it.
fn drc_gauge(ui: &mut Ui, gain: Option<f64>) {
    let db = reading_db(gain);
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2((ui.available_width() * 0.35).clamp(60.0, 120.0), 6.0),
            egui::Sense::hover(),
        );
        let painter = ui.painter();
        painter.rect_filled(rect, 3.0, Color32::from_rgb(0x22, 0x22, 0x22));
        // `maxDelta`: the bar is full at 20 dB either way.
        if let Some(db) = db {
            let fraction = ((db.abs() / 20.0).min(1.0)) as f32;
            if fraction > 0.0 {
                let mut fill = rect;
                fill.set_left(rect.right() - rect.width() * fraction);
                painter.rect_filled(fill, 3.0, gauge_colour(db));
            }
        }
        let text = match db {
            None => "0.0 dB".to_owned(),
            Some(db) if db >= 0.0 => format!("+{db:.1} dB"),
            Some(db) => format!("{db:.1} dB"),
        };
        ui.label(
            RichText::new(text)
                .monospace()
                .size(theme::FONT_SIZE_SMALL)
                .color(theme::TEXT_MUTED),
        );
    });
}

#[cfg(test)]
mod tests {
    use super::reading_db;

    /// `linearToDb`: unity is 0 dB, half is -6 dB, silence and nonsense are
    /// the -100 dB floor, and a gauge nothing was reported to has no reading.
    #[test]
    fn the_gauge_reads_the_gain_in_db_with_the_webs_floor() {
        assert_eq!(reading_db(None), None);
        assert_eq!(reading_db(Some(1.0)), Some(0.0));
        assert!((reading_db(Some(0.5)).unwrap() + 6.02).abs() < 0.01);
        assert_eq!(reading_db(Some(0.0)), Some(-100.0));
        assert_eq!(reading_db(Some(f64::NAN)), Some(-100.0));
    }
}
