//! DRC and loudness (`#drcSection`, `controls/drc.js`): the compression mode
//! and its weight, the loudness switch, and the gain gauge the renderer
//! reports while metering is on.

use egui::{Color32, RichText, Ui};

use crate::app::StudioSpike;
use crate::i18n::t;
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
            let live = self.live.lock().unwrap();
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
        Section::new("drcSection", "section.drc")
            .info("drc")
            .summary(summary)
            .show(ui, |ui| {
                // The gauge only means something while the renderer is
                // metering, so it follows that switch like the web does.
                if metering {
                    drc_gauge(ui, gain);
                }

                // The mode list is the renderer's, plus whatever it currently
                // reports, so an unknown mode is never silently dropped.
                let mut options = modes.clone();
                if !options.iter().any(|m| *m == mode) {
                    options.push(mode.clone());
                }
                if options.is_empty() {
                    options.push("Off".to_owned());
                }
                let mut chosen = mode.clone();
                ui.horizontal(|ui| {
                    ui.label(t("input.drc"));
                    widgets::help(ui, "help.drc.mode");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        egui::ComboBox::from_id_salt("drc-mode")
                            .selected_text(&mode)
                            .width(120.0)
                            .show_ui(ui, |ui| {
                                for option in &options {
                                    ui.selectable_value(&mut chosen, option.clone(), option);
                                }
                            });
                    });
                });
                if chosen != mode {
                    self.live.lock().unwrap().app.drc_mode = Some(chosen.clone());
                    self.ctl
                        .send_string("/omniphony/control/input/drc_mode", &chosen);
                }

                let mut percent = (weight * 100.0).round();
                if widgets::value_slider(
                    ui,
                    t("input.drc_weight"),
                    &mut percent,
                    0.0..=100.0,
                    1.0,
                    |v| format!("{v:.0}%"),
                ) {
                    let value = (percent / 100.0).clamp(0.0, 1.0);
                    self.live.lock().unwrap().app.drc_weight = Some(value);
                    self.ctl
                        .send_float("/omniphony/control/input/drc_weight", value);
                }

                ui.separator();
                let mut on = loudness;
                if widgets::switch_row(ui, t("section.loudness"), &mut on) {
                    self.live.lock().unwrap().app.loudness = Some(u8::from(on));
                    self.ctl
                        .send_int("/omniphony/control/loudness", i32::from(on));
                }
                for line in loudness_lines(source, gain) {
                    widgets::note(ui, &line);
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

/// A right-anchored bar: the correction grows leftwards from unity.
fn drc_gauge(ui: &mut Ui, gain: Option<f64>) {
    let db = match gain {
        Some(g) if g > 0.0 => 20.0 * g.log10(),
        _ => -100.0,
    };
    let db = if db.is_finite() { db } else { -100.0 };
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width().min(120.0), 6.0),
            egui::Sense::hover(),
        );
        let painter = ui.painter();
        painter.rect_filled(rect, 3.0, Color32::from_rgb(0x22, 0x22, 0x22));
        let fraction = ((db.abs() / 20.0).min(1.0)) as f32;
        if fraction > 0.0 {
            let mut fill = rect;
            fill.set_left(rect.right() - rect.width() * fraction);
            painter.rect_filled(fill, 3.0, gauge_colour(db));
        }
        let text = if db <= -100.0 {
            "0.0 dB".to_owned()
        } else if db >= 0.0 {
            format!("+{db:.1} dB")
        } else {
            format!("{db:.1} dB")
        };
        ui.label(
            RichText::new(text)
                .monospace()
                .size(theme::FONT_SIZE_SMALL)
                .color(theme::TEXT_MUTED),
        );
    });
}
