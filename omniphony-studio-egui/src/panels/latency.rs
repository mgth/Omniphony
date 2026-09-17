//! The Latency section (`#latencySection`, `ui/audio-panel.js`,
//! `controls/latency.js`, `controls/adaptive.js`): the latency meter with its
//! markers, the resampling deviation meter, the target latency, and the
//! adaptive resampling controller's parameters.
//!
//! The numeric parameters share one dirty/apply cycle: they are edited into a
//! buffer and sent together, because half a controller's settings applied on
//! their own is a worse state than the one before the edit.

use egui::{Color32, RichText, Ui};

use crate::app::StudioSpike;
use crate::host::commands::adaptive::{self, Param, Switch};
use crate::host::commands::resampling;
use crate::i18n::{t, tf};
use crate::model::app_state::AppState;
use crate::ui::section::Section;
use crate::ui::{theme, widgets};

/// When each numeric field can be edited.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// Whenever the producer has an audio domain.
    Always,
    /// Only while the far-mode actions can fire.
    FarMode,
    /// Only while adaptive resampling is on.
    Adaptive,
    /// Only while the far mode silences the output.
    Silence,
}

/// How one numeric parameter is drawn. What it *is* — the model field, the
/// default, the range — is `Param`, in the core; this is the form around it.
pub struct Row {
    pub param: Param,
    pub label: &'static str,
    pub help: &'static str,
    pub unit: &'static str,
    pub step: f64,
    pub decimals: usize,
    pub gate: Gate,
}

/// The three subpanels of the web form, in its order.
pub const SUBPANELS: &[(&str, &[Row])] = &[
    ("adaptive.globalActions", GLOBAL_ROWS),
    ("adaptive.resamplingController", CONTROLLER_ROWS),
    ("adaptive.stabilizationPhases", STABILISATION_ROWS),
];

const GLOBAL_ROWS: &[Row] = &[
    Row {
        param: Param::HighRecoverEntryMarginMs,
        label: "adaptive.threshold",
        help: "help.adaptive.threshold",
        unit: "ms",
        step: 1.0,
        decimals: 0,
        gate: Gate::FarMode,
    },
    Row {
        param: Param::LowRecoverEntryMarginMs,
        label: "adaptive.lowRecoverEntryMargin",
        help: "help.adaptive.lowRecoverEntryMargin",
        unit: "ms",
        step: 0.1,
        decimals: 1,
        gate: Gate::Always,
    },
    Row {
        param: Param::LowRecoverExitMarginMs,
        label: "adaptive.lowRecoverExitMargin",
        help: "help.adaptive.lowRecoverExitMargin",
        unit: "ms",
        step: 0.1,
        decimals: 1,
        gate: Gate::Always,
    },
    Row {
        param: Param::FarModeReturnFadeInMs,
        label: "adaptive.fadeNearReturn",
        help: "help.adaptive.fadeNearReturn",
        unit: "ms",
        step: 1.0,
        decimals: 0,
        gate: Gate::Silence,
    },
];

const CONTROLLER_ROWS: &[Row] = &[
    Row {
        param: Param::UpdateIntervalCallbacks,
        label: "adaptive.updateInterval",
        help: "help.adaptive.updateInterval",
        unit: "",
        step: 1.0,
        decimals: 0,
        gate: Gate::Adaptive,
    },
    Row {
        param: Param::MaxAdjustPpm,
        label: "adaptive.max",
        help: "help.adaptive.max",
        unit: "ppm",
        step: 1.0,
        decimals: 0,
        gate: Gate::Adaptive,
    },
    Row {
        param: Param::KpNear,
        label: "adaptive.kpNear",
        help: "help.adaptive.kpNear",
        unit: "",
        step: 0.001,
        decimals: 3,
        gate: Gate::Adaptive,
    },
    Row {
        param: Param::Ki,
        label: "adaptive.ki",
        help: "help.adaptive.ki",
        unit: "",
        step: 0.001,
        decimals: 3,
        gate: Gate::Adaptive,
    },
    Row {
        param: Param::IntegralDischargeRatio,
        label: "adaptive.integralDischarge",
        help: "help.adaptive.integralDischarge",
        unit: "",
        step: 0.001,
        decimals: 3,
        gate: Gate::Adaptive,
    },
];

const STABILISATION_ROWS: &[Row] = &[
    Row {
        param: Param::LowRecoverSettleStableMs,
        label: "adaptive.lowRecoverSettleStable",
        help: "help.adaptive.lowRecoverSettleStable",
        unit: "ms",
        step: 1.0,
        decimals: 0,
        gate: Gate::Always,
    },
    Row {
        param: Param::LowRecoverSettleMarginMs,
        label: "adaptive.lowRecoverSettleMargin",
        help: "help.adaptive.lowRecoverSettleMargin",
        unit: "ms",
        step: 0.1,
        decimals: 1,
        gate: Gate::Always,
    },
    Row {
        param: Param::LowRecoverRefillDeltaAlpha,
        label: "adaptive.lowRecoverRefillDeltaAlpha",
        help: "help.adaptive.lowRecoverRefillDeltaAlpha",
        unit: "",
        step: 0.01,
        decimals: 2,
        gate: Gate::Always,
    },
    Row {
        param: Param::ControlSmoothingCutoffHz,
        label: "adaptive.controlSmoothingCutoffHz",
        help: "help.adaptive.controlSmoothingCutoffHz",
        unit: "Hz",
        step: 0.05,
        decimals: 3,
        gate: Gate::Always,
    },
    Row {
        param: Param::ControlSmoothingOrder,
        label: "adaptive.controlSmoothingOrder",
        help: "help.adaptive.controlSmoothingOrder",
        unit: "",
        step: 1.0,
        decimals: 0,
        gate: Gate::Always,
    },
];

/// The switches, in the web's order: the three far-mode actions first, the
/// three diagnostics last.
const SWITCHES: &[(Switch, &str, &str)] = &[
    (
        Switch::HardRecoverHigh,
        "adaptive.hardRecoverHigh",
        "help.adaptive.hardRecoverHigh",
    ),
    (
        Switch::HardRecoverLow,
        "adaptive.hardRecoverLow",
        "help.adaptive.hardRecoverLow",
    ),
    (
        Switch::SilenceFar,
        "adaptive.silenceFar",
        "help.adaptive.silenceFar",
    ),
    (
        Switch::UsePreBridgeClock,
        "adaptive.usePreBridgeClock",
        "help.adaptive.usePreBridgeClock",
    ),
    (
        Switch::UseOutputPacing,
        "adaptive.useOutputPacing",
        "help.adaptive.useOutputPacing",
    ),
    (
        Switch::DisableBackpressure,
        "adaptive.disableBackpressure",
        "help.adaptive.disableBackpressure",
    ),
];

impl StudioSpike {
    pub(crate) fn latency_section(&mut self, ui: &mut Ui) {
        let (state, stats, adaptive_on, paused, band, runtime_state) = {
            let live = self.host.read();
            (
                LatencyView::of(&live.app),
                live.latency_stats(),
                live.app.adaptive_resampling.unwrap_or(0) != 0,
                live.app.adaptive_resampling_paused.unwrap_or(0) != 0,
                live.app.adaptive_resampling_band.clone(),
                live.app.adaptive_resampling_state.clone(),
            )
        };
        let target = state.requested.or(state.target).or(state.latency);
        // The meter sits in the header, beside the title, as the web's grid
        // puts it (`#latencySection`, column 2 of row 1): in view whether the
        // section is open or folded, with the numbers it summarises below it.
        // Bar and reading are one widget rather than a header widget and the
        // section's summary, so the reading gets a fixed box and cannot move
        // the bar from one frame to the next.
        let reading = reading(state.instant);
        Section::new("latencySection", "section.latency")
            .info("adaptive")
            .header_widget(|ui| {
                crate::ui::meter::row_with_readout(ui, READOUT_ADVANCES, &reading, |ui, width| {
                    latency_meter(ui, width, &state, stats.as_ref(), target);
                })
            })
            .show(ui, |ui| {
                readouts(ui, &state, stats.as_ref());
                if adaptive_on {
                    self.resample_meter(ui);
                }
                // The sparkline sits between the gauge and the target, as in
                // the web: the gauge says where the rate is now, the plot says
                // how it got there.
                self.resample_plot(ui);
                self.target_latency_row(ui, target);
                band_indicator(ui, runtime_state.as_deref(), band.as_deref());
                self.adaptive_form(ui, adaptive_on, paused);
            });
    }

    /// The deviation meter: the resampler's correction against the range it
    /// is allowed, centred on no correction at all.
    fn resample_meter(&mut self, ui: &mut Ui) {
        let (ratio, max_adjust) = {
            let live = self.host.read();
            (
                live.app.resample_ratio,
                live.app.adaptive_resampling_max_adjust.unwrap_or(0.01),
            )
        };
        ui.horizontal(|ui| {
            crate::ui::help::label(
                ui,
                RichText::new(t("telemetry.resample"))
                    .size(theme::FONT_SIZE_SMALL)
                    .color(theme::TEXT_MUTED),
                "help.telemetry.resample",
            );
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(ui.available_width().min(140.0), 6.0),
                egui::Sense::hover(),
            );
            let painter = ui.painter();
            painter.rect_filled(rect, 3.0, theme::FILL);
            let centre = rect.center().x;
            painter.vline(
                centre,
                rect.y_range(),
                egui::Stroke::new(2.0, Color32::from_rgba_unmultiplied(217, 236, 255, 153)),
            );
            let text = match ratio {
                Some(ratio) => {
                    let bound = max_adjust.max(1e-6);
                    let magnitude =
                        ((ratio - 1.0).abs() / bound).min(1.0) as f32 * rect.width() * 0.5;
                    if magnitude > 0.0 {
                        let mut fill = rect;
                        if ratio >= 1.0 {
                            fill.set_left(centre);
                            fill.set_right(centre + magnitude);
                            painter.rect_filled(fill, 3.0, Color32::from_rgb(0x7b, 0xff, 0xb8));
                        } else {
                            fill.set_left(centre - magnitude);
                            fill.set_right(centre);
                            painter.rect_filled(fill, 3.0, Color32::from_rgb(0xff, 0x8a, 0x5c));
                        }
                    }
                    let ppm = ((ratio - 1.0) * 1e6).round() as i64;
                    format!("{}{ppm} ppm", if ppm >= 0 { "+" } else { "" })
                }
                None => "—".to_owned(),
            };
            ui.label(
                RichText::new(text)
                    .monospace()
                    .size(theme::FONT_SIZE_SMALL)
                    .color(theme::TEXT_MUTED),
            );
        });
        crate::ui::help::card(ui, "help.telemetry.resample");
    }

    fn target_latency_row(&mut self, ui: &mut Ui, target: Option<i64>) {
        let mut value = self
            .latency_target_edit
            .unwrap_or_else(|| target.unwrap_or(500).max(1) as f64);
        widgets::label_row_help(
            ui,
            t("audio.targetLatency"),
            "help.audio.targetLatency",
            |ui| {
                let dirty = self.latency_target_edit.is_some();
                if ui
                    .add_enabled(dirty, egui::Button::new(t("adaptive.apply")))
                    .clicked()
                {
                    resampling::set_latency_target(&self.host, value.round() as i64);
                    self.latency_target_edit = None;
                }
                if ui
                    .add_sized(
                        egui::vec2(72.0, ui.spacing().interact_size.y),
                        egui::DragValue::new(&mut value)
                            .speed(1.0)
                            .range(1.0..=f64::MAX)
                            .suffix(" ms"),
                    )
                    .changed()
                {
                    self.latency_target_edit = Some(value);
                }
            },
        );
    }

    /// The adaptive controller: its switches, its three groups of numbers,
    /// and the pause control.
    fn adaptive_form(&mut self, ui: &mut Ui, adaptive_on: bool, paused: bool) {
        ui.separator();
        let mut on = adaptive_on;
        if widgets::switch_row_help(ui, t("adaptive.title"), "help.adaptive.title", &mut on) {
            resampling::set_adaptive_resampling_enabled(&self.host, on);
        }
        ui.horizontal_wrapped(|ui| {
            let label = if paused {
                format!("▶ {}", t("adaptive.resume"))
            } else {
                format!("⏸ {}", t("adaptive.pause"))
            };
            if ui
                .add_enabled(adaptive_on, egui::Button::new(label))
                .clicked()
            {
                resampling::set_adaptive_resampling_paused(&self.host, !paused);
            }
            // Only reachable while paused: it is a diagnostic, not a control.
            if adaptive_on && paused && ui.button(t("adaptive.resetRatio")).clicked() {
                resampling::control_adaptive_resampling_reset_ratio(&self.host);
            }
            // The wizard patches this controller live, so it only makes sense
            // while there is one running to patch.
            if adaptive_on && !paused {
                self.auto_tune_button(ui);
            }
        });

        // The far mode fires when any of its three actions is armed.
        let far_mode = {
            let live = self.host.read();
            live.app
                .adaptive_resampling_hard_recover_high_in_far_mode
                .unwrap_or(1)
                != 0
                || live
                    .app
                    .adaptive_resampling_hard_recover_low_in_far_mode
                    .unwrap_or(0)
                    != 0
                || live
                    .app
                    .adaptive_resampling_force_silence_in_far_mode
                    .unwrap_or(1)
                    != 0
        };
        let silence = Switch::SilenceFar.get(&self.host.read().app);

        for (index, (caption, rows)) in SUBPANELS.iter().enumerate() {
            ui.add_space(4.0);
            ui.label(
                RichText::new(t(caption))
                    .size(theme::FONT_SIZE_SMALL)
                    .color(theme::TEXT_MUTED),
            );
            // The switches of the first and last subpanels, in the web's
            // places: far actions first, diagnostics last.
            let switches: &[usize] = match index {
                0 => &[0, 1, 2],
                2 => &[3, 4, 5],
                _ => &[],
            };
            for i in switches {
                let (switch, label, help) = &SWITCHES[*i];
                let mut value = switch.get(&self.host.read().app);
                // Label, help mark and switch on one line, the switch placed
                // first: it used to fall to a line of its own below its label.
                if widgets::label_row_help(ui, t(label), *help, |ui| {
                    widgets::switch(ui, &mut value).changed()
                }) {
                    adaptive::set_switch(&self.host, *switch, value);
                }
            }
            for row in *rows {
                let enabled = match row.gate {
                    Gate::Always => true,
                    Gate::FarMode => far_mode,
                    Gate::Adaptive => adaptive_on,
                    Gate::Silence => silence,
                };
                let stored = row.param.get(&self.host.read().app);
                let mut value = *self.adaptive_edits.get(&row.param).unwrap_or(&stored);
                let (min, max) = row.param.range();
                ui.add_enabled_ui(enabled, |ui| {
                    widgets::label_row_help(ui, t(row.label), row.help, |ui| {
                        let mut drag = egui::DragValue::new(&mut value)
                            .speed(row.step)
                            .range(min..=max)
                            .fixed_decimals(row.decimals);
                        if !row.unit.is_empty() {
                            drag = drag.suffix(format!(" {}", row.unit));
                        }
                        if ui
                            .add_sized(egui::vec2(84.0, ui.spacing().interact_size.y), drag)
                            .changed()
                        {
                            self.adaptive_edits.insert(row.param, value);
                        }
                    });
                });
            }
        }

        let dirty = !self.adaptive_edits.is_empty();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(dirty, egui::Button::new(t("adaptive.apply")))
                .clicked()
            {
                self.apply_adaptive_edits();
            }
            if ui
                .add_enabled(dirty, egui::Button::new(t("common.cancel")))
                .clicked()
            {
                self.adaptive_edits.clear();
            }
        });
    }

    /// Send the edits as one batch: the core clamps them, keeps the hysteresis
    /// well formed and pushes the whole controller.
    fn apply_adaptive_edits(&mut self) {
        let edits = std::mem::take(&mut self.adaptive_edits);
        adaptive::apply_params(&self.host, &edits);
    }
}

/// The header's reading: the instant latency, or the placeholder when the
/// renderer has reported none.
fn reading(instant: Option<i64>) -> String {
    match instant {
        Some(v) => format!("{v} ms"),
        None => "—".to_owned(),
    }
}

/// The readout's box, in monospace advances, as `panels::audio` has one for
/// the master meter. Eight hold `99999 ms`, well past the latency the
/// controller would still call a latency, and the `—` placeholder is one
/// character. Nothing clamps the figure the renderer reports — unlike the
/// master's `rms_dbfs` — so a wider one is clipped to the box by
/// `row_with_readout` instead of running over the bar.
const READOUT_ADVANCES: f32 = 8.0;

/// The bar's own height, and the floor its width takes: an overlay dragged to
/// its narrowest shortens the bar rather than inverting it.
const BAR_HEIGHT: f32 = 8.0;

/// The instant latency against its target, with the spread over the window,
/// at the width the header leaves it.
fn latency_meter(
    ui: &mut Ui,
    width: f32,
    state: &LatencyView,
    stats: Option<&crate::host::timing_stats::WindowStats>,
    target: Option<i64>,
) {
    // Full scale is twice the target, so the target sits mid-bar.
    let max_ms = match target {
        Some(t) => (2.0 * t as f64).max(100.0),
        None => 2000.0,
    };
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(width.max(BAR_HEIGHT), BAR_HEIGHT),
        egui::Sense::hover(),
    );
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, theme::FILL);
    let at = |ms: f64| rect.left() + rect.width() * (ms / max_ms).clamp(0.0, 1.0) as f32;

    let raw = state
        .instant
        .or(state.target)
        .or(state.latency)
        .unwrap_or(0) as f64;
    let mut fill = rect;
    fill.set_right(at(raw));
    // Green up to the target, amber past it, red at twice.
    let ratio = (raw / max_ms).clamp(0.0, 1.0);
    let colour = if ratio < 0.5 {
        Color32::from_rgb(0x52, 0xe2, 0xa2)
    } else if ratio < 0.75 {
        Color32::from_rgb(0xff, 0xd5, 0x6a)
    } else {
        Color32::from_rgb(0xff, 0x4d, 0x4d)
    };
    painter.rect_filled(fill, 4.0, colour);

    if let Some(stats) = stats {
        for value in [stats.min, stats.max] {
            painter.vline(
                at(value),
                rect.y_range(),
                egui::Stroke::new(1.0, theme::TEXT_MUTED),
            );
        }
    }
    if let Some(control) = state.control {
        painter.vline(
            at(control as f64),
            rect.y_range(),
            egui::Stroke::new(1.5, Color32::from_rgb(0x58, 0xa0, 0xff)),
        );
    }
    if let Some(smoothed) = state.smoothed {
        painter.vline(
            at(smoothed),
            rect.y_range(),
            egui::Stroke::new(1.5, Color32::from_rgb(0xc8, 0x79, 0xff)),
        );
    }
    if let Some(target) = target {
        painter.circle_filled(
            egui::pos2(at(target as f64), rect.top() - 3.0),
            2.5,
            Color32::from_rgb(0x52, 0xe2, 0xa2),
        );
    }
}

/// The two lines of numbers under the meter.
fn readouts(
    ui: &mut Ui,
    state: &LatencyView,
    stats: Option<&crate::host::timing_stats::WindowStats>,
) {
    let ms = |v: Option<i64>| match v {
        Some(v) => format!("{v} ms"),
        None => "—".to_owned(),
    };
    ui.horizontal(|ui| {
        let min = stats
            .map(|s| format!("{:.0}", s.min))
            .unwrap_or_else(|| "—".to_owned());
        let max = stats
            .map(|s| format!("{:.0}", s.max))
            .unwrap_or_else(|| "—".to_owned());
        widgets::note(ui, &tf("status.minValue", &[("value", &min)]));
        widgets::note(ui, &ms(state.instant));
        widgets::note(ui, &tf("status.maxValue", &[("value", &max)]));
    });
    ui.horizontal(|ui| {
        widgets::note(ui, &format!("ctrl {}", ms(state.control)));
        let smoothed = state
            .smoothed
            .map(|v| format!("{v:.2} ms"))
            .unwrap_or_else(|| "—".to_owned());
        widgets::note(ui, &format!("smoothed {smoothed}"));
        widgets::note(ui, &format!("path {}", ms(state.downstream)));
    });
}

/// The controller's phase and the band it is working in.
fn band_indicator(ui: &mut Ui, runtime_state: Option<&str>, band: Option<&str>) {
    let colour = match band {
        Some("hard") => Color32::from_rgb(0xff, 0x4d, 0x4d),
        Some("far") => Color32::from_rgb(0xff, 0x9a, 0x5c),
        Some("near") => theme::OK,
        _ => Color32::from_rgba_unmultiplied(255, 255, 255, 64),
    };
    ui.horizontal(|ui| {
        widgets::note(ui, &runtime_state.unwrap_or("—").to_uppercase());
        let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.0, colour);
        ui.label(
            RichText::new(band.unwrap_or("—"))
                .size(theme::FONT_SIZE)
                .color(theme::TEXT),
        );
    });
}

/// The latency numbers this panel reads, copied out under the lock.
pub struct LatencyView {
    pub latency: Option<i64>,
    pub instant: Option<i64>,
    pub control: Option<i64>,
    pub smoothed: Option<f64>,
    pub downstream: Option<i64>,
    pub target: Option<i64>,
    pub requested: Option<i64>,
}

impl LatencyView {
    /// Snapshot of the model's latency block, so the panel does not hold the
    /// lock while it draws. A constructor on the view's own type rather than a
    /// method on `AppState`: the model is the core's, and the UI does not
    /// extend it.
    pub fn of(app: &AppState) -> Self {
        Self {
            latency: app.latency.latency_ms,
            instant: app.latency.latency_instant_ms,
            control: app.latency.latency_control_ms,
            smoothed: app.latency.latency_smoothed_ms,
            downstream: app.latency.latency_downstream_ms,
            target: app.latency.latency_target_ms,
            requested: app.latency.latency_requested_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{READOUT_ADVANCES, reading};

    /// The reading's box is a fixed number of advances so the bar beside it
    /// never moves. Every latency worth calling one fits in it — and past
    /// that the box does not grow, `row_with_readout` clips.
    #[test]
    fn a_plausible_reading_fits_the_fixed_readout_box() {
        let box_chars = READOUT_ADVANCES as usize;
        for ms in [0, 1, 40, 500, 2_000, 99_999] {
            assert!(
                reading(Some(ms)).chars().count() <= box_chars,
                "{ms} ms overflows the box"
            );
        }
        assert!(reading(None).chars().count() <= box_chars);
        assert!(reading(Some(1_000_000)).chars().count() > box_chars);
    }
}
