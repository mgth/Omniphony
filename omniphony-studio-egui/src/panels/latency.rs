//! The Latency section (`#latencySection`, `ui/audio-panel.js`,
//! `controls/latency.js`, `controls/adaptive.js`): the latency meter with its
//! markers, the resampling deviation meter, the target latency, and the
//! adaptive resampling controller's parameters.
//!
//! The numeric parameters share one dirty/apply cycle: they are edited into a
//! buffer and sent together, because half a controller's settings applied on
//! their own is a worse state than the one before the edit.

use std::collections::BTreeMap;

use egui::{Color32, RichText, Ui};

use crate::app::StudioSpike;
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

/// One numeric parameter of the adaptive controller.
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub unit: &'static str,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub decimals: usize,
    pub gate: Gate,
    pub get: fn(&AppState) -> f64,
    pub set: fn(&mut AppState, f64),
}

/// The three subpanels of the web form, in its order.
pub const SUBPANELS: &[(&str, &[Field])] = &[
    ("adaptive.globalActions", GLOBAL_FIELDS),
    ("adaptive.resamplingController", CONTROLLER_FIELDS),
    ("adaptive.stabilizationPhases", STABILISATION_FIELDS),
];

const GLOBAL_FIELDS: &[Field] = &[
    Field {
        key: "high_recover_entry_margin_ms",
        label: "adaptive.threshold",
        help: "help.adaptive.threshold",
        unit: "ms",
        min: 1.0,
        max: 10_000.0,
        step: 1.0,
        decimals: 0,
        gate: Gate::FarMode,
        get: |a| {
            a.adaptive_resampling_high_recover_entry_margin_ms
                .unwrap_or(1000) as f64
        },
        set: |a, v| a.adaptive_resampling_high_recover_entry_margin_ms = Some(v.round() as i64),
    },
    Field {
        key: "low_recover_entry_margin_ms",
        label: "adaptive.lowRecoverEntryMargin",
        help: "help.adaptive.lowRecoverEntryMargin",
        unit: "ms",
        min: 0.0,
        max: 1000.0,
        step: 0.1,
        decimals: 1,
        gate: Gate::Always,
        get: |a| {
            a.adaptive_resampling_low_recover_entry_margin_ms
                .unwrap_or(18.0)
        },
        set: |a, v| a.adaptive_resampling_low_recover_entry_margin_ms = Some(v),
    },
    Field {
        key: "low_recover_exit_margin_ms",
        label: "adaptive.lowRecoverExitMargin",
        help: "help.adaptive.lowRecoverExitMargin",
        unit: "ms",
        min: 0.0,
        max: 1000.0,
        step: 0.1,
        decimals: 1,
        gate: Gate::Always,
        get: |a| {
            a.adaptive_resampling_low_recover_exit_margin_ms
                .unwrap_or(6.0)
        },
        set: |a, v| a.adaptive_resampling_low_recover_exit_margin_ms = Some(v),
    },
    Field {
        key: "far_mode_return_fade_in_ms",
        label: "adaptive.fadeNearReturn",
        help: "help.adaptive.fadeNearReturn",
        unit: "ms",
        min: 0.0,
        max: 10_000.0,
        step: 1.0,
        decimals: 0,
        gate: Gate::Silence,
        get: |a| {
            a.adaptive_resampling_far_mode_return_fade_in_ms
                .unwrap_or(0) as f64
        },
        set: |a, v| a.adaptive_resampling_far_mode_return_fade_in_ms = Some(v.round() as i64),
    },
];

const CONTROLLER_FIELDS: &[Field] = &[
    Field {
        key: "update_interval_callbacks",
        label: "adaptive.updateInterval",
        help: "help.adaptive.updateInterval",
        unit: "",
        min: 1.0,
        max: 1000.0,
        step: 1.0,
        decimals: 0,
        gate: Gate::Adaptive,
        get: |a| a.adaptive_resampling_update_interval_callbacks.unwrap_or(1) as f64,
        set: |a, v| a.adaptive_resampling_update_interval_callbacks = Some(v.round() as i64),
    },
    // Edited in parts per million, stored as a ratio.
    Field {
        key: "max_adjust_ppm",
        label: "adaptive.max",
        help: "help.adaptive.max",
        unit: "ppm",
        min: 1.0,
        max: 100_000.0,
        step: 1.0,
        decimals: 0,
        gate: Gate::Adaptive,
        get: |a| (a.adaptive_resampling_max_adjust.unwrap_or(0.01) * 1e6).round(),
        set: |a, v| a.adaptive_resampling_max_adjust = Some((v / 1e6).max(1e-6)),
    },
    Field {
        key: "kp_near",
        label: "adaptive.kpNear",
        help: "help.adaptive.kpNear",
        unit: "",
        min: 0.01,
        max: 100.0,
        step: 0.001,
        decimals: 3,
        gate: Gate::Adaptive,
        get: |a| a.adaptive_resampling_kp_near.unwrap_or(1.0),
        set: |a, v| a.adaptive_resampling_kp_near = Some(v),
    },
    Field {
        key: "ki",
        label: "adaptive.ki",
        help: "help.adaptive.ki",
        unit: "",
        min: 0.0,
        max: 100.0,
        step: 0.001,
        decimals: 3,
        gate: Gate::Adaptive,
        get: |a| a.adaptive_resampling_ki.unwrap_or(1.0),
        set: |a, v| a.adaptive_resampling_ki = Some(v),
    },
    // Measured to be inert; kept so the panel matches the renderer's schema.
    Field {
        key: "integral_discharge_ratio",
        label: "adaptive.integralDischarge",
        help: "help.adaptive.integralDischarge",
        unit: "",
        min: 0.0,
        max: 1.0,
        step: 0.001,
        decimals: 3,
        gate: Gate::Adaptive,
        get: |a| {
            a.adaptive_resampling_integral_discharge_ratio
                .unwrap_or(0.25)
        },
        set: |a, v| a.adaptive_resampling_integral_discharge_ratio = Some(v),
    },
];

const STABILISATION_FIELDS: &[Field] = &[
    Field {
        key: "low_recover_settle_stable_ms",
        label: "adaptive.lowRecoverSettleStable",
        help: "help.adaptive.lowRecoverSettleStable",
        unit: "ms",
        min: 0.0,
        max: 10_000.0,
        step: 1.0,
        decimals: 0,
        gate: Gate::Always,
        get: |a| {
            a.adaptive_resampling_low_recover_settle_stable_ms
                .unwrap_or(200.0)
        },
        set: |a, v| a.adaptive_resampling_low_recover_settle_stable_ms = Some(v.round()),
    },
    Field {
        key: "low_recover_settle_margin_ms",
        label: "adaptive.lowRecoverSettleMargin",
        help: "help.adaptive.lowRecoverSettleMargin",
        unit: "ms",
        min: 0.0,
        max: 1000.0,
        step: 0.1,
        decimals: 1,
        gate: Gate::Always,
        get: |a| {
            a.adaptive_resampling_low_recover_settle_margin_ms
                .unwrap_or(6.0)
        },
        set: |a, v| a.adaptive_resampling_low_recover_settle_margin_ms = Some(v),
    },
    Field {
        key: "low_recover_refill_delta_alpha",
        label: "adaptive.lowRecoverRefillDeltaAlpha",
        help: "help.adaptive.lowRecoverRefillDeltaAlpha",
        unit: "",
        min: 0.0,
        max: 1.0,
        step: 0.01,
        decimals: 2,
        gate: Gate::Always,
        get: |a| {
            a.adaptive_resampling_low_recover_refill_delta_alpha
                .unwrap_or(0.5)
        },
        set: |a, v| a.adaptive_resampling_low_recover_refill_delta_alpha = Some(v),
    },
    Field {
        key: "control_smoothing_cutoff_hz",
        label: "adaptive.controlSmoothingCutoffHz",
        help: "help.adaptive.controlSmoothingCutoffHz",
        unit: "Hz",
        min: 0.001,
        max: 20.0,
        step: 0.05,
        decimals: 3,
        gate: Gate::Always,
        get: |a| {
            a.adaptive_resampling_control_smoothing_cutoff_hz
                .unwrap_or(0.5)
        },
        set: |a, v| a.adaptive_resampling_control_smoothing_cutoff_hz = Some(v),
    },
    Field {
        key: "control_smoothing_order",
        label: "adaptive.controlSmoothingOrder",
        help: "help.adaptive.controlSmoothingOrder",
        unit: "",
        min: 1.0,
        max: 2.0,
        step: 1.0,
        decimals: 0,
        gate: Gate::Always,
        get: |a| a.adaptive_resampling_control_smoothing_order.unwrap_or(1) as f64,
        set: |a, v| a.adaptive_resampling_control_smoothing_order = Some(v.round() as u32),
    },
];

/// The switches, which are outside the dirty cycle: each sends at once.
const SWITCHES: &[(&str, &str, fn(&AppState) -> bool, fn(&mut AppState, bool))] = &[
    (
        "adaptive.hardRecoverHigh",
        "help.adaptive.hardRecoverHigh",
        |a| {
            a.adaptive_resampling_hard_recover_high_in_far_mode
                .unwrap_or(1)
                != 0
        },
        |a, v| a.adaptive_resampling_hard_recover_high_in_far_mode = Some(u8::from(v)),
    ),
    (
        "adaptive.hardRecoverLow",
        "help.adaptive.hardRecoverLow",
        |a| {
            a.adaptive_resampling_hard_recover_low_in_far_mode
                .unwrap_or(0)
                != 0
        },
        |a, v| a.adaptive_resampling_hard_recover_low_in_far_mode = Some(u8::from(v)),
    ),
    (
        "adaptive.silenceFar",
        "help.adaptive.silenceFar",
        |a| a.adaptive_resampling_force_silence_in_far_mode.unwrap_or(1) != 0,
        |a, v| a.adaptive_resampling_force_silence_in_far_mode = Some(u8::from(v)),
    ),
    (
        "adaptive.usePreBridgeClock",
        "help.adaptive.usePreBridgeClock",
        |a| a.adaptive_resampling_use_pre_bridge_clock.unwrap_or(0) != 0,
        |a, v| a.adaptive_resampling_use_pre_bridge_clock = Some(u8::from(v)),
    ),
    (
        "adaptive.useOutputPacing",
        "help.adaptive.useOutputPacing",
        |a| a.adaptive_resampling_use_output_pacing.unwrap_or(0) != 0,
        |a, v| a.adaptive_resampling_use_output_pacing = Some(u8::from(v)),
    ),
    (
        "adaptive.disableBackpressure",
        "help.adaptive.disableBackpressure",
        |a| a.adaptive_resampling_disable_backpressure.unwrap_or(0) != 0,
        |a, v| a.adaptive_resampling_disable_backpressure = Some(u8::from(v)),
    ),
];

impl StudioSpike {
    pub(crate) fn latency_section(&mut self, ui: &mut Ui) {
        let (state, stats, adaptive_on, paused, band, runtime_state) = {
            let live = self.live.lock().unwrap();
            (
                live.app.clone_latency_view(),
                live.latency_stats(),
                live.app.adaptive_resampling.unwrap_or(0) != 0,
                live.app.adaptive_resampling_paused.unwrap_or(0) != 0,
                live.app.adaptive_resampling_band.clone(),
                live.app.adaptive_resampling_state.clone(),
            )
        };
        let target = state.requested.or(state.target).or(state.latency);
        let summary = match state.instant {
            Some(v) => format!("{v} ms"),
            None => "—".to_owned(),
        };
        Section::new("latencySection", "section.latency")
            .info("adaptive")
            .summary(summary)
            .show(ui, |ui| {
                latency_meter(ui, &state, stats.as_ref(), target);
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
            let live = self.live.lock().unwrap();
            (
                live.app.resample_ratio,
                live.app.adaptive_resampling_max_adjust.unwrap_or(0.01),
            )
        };
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(t("telemetry.resample"))
                    .size(theme::FONT_SIZE_SMALL)
                    .color(theme::TEXT_MUTED),
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
    }

    fn target_latency_row(&mut self, ui: &mut Ui, target: Option<i64>) {
        let mut value = self
            .latency_target_edit
            .unwrap_or_else(|| target.unwrap_or(500).max(1) as f64);
        ui.horizontal(|ui| {
            ui.label(t("audio.targetLatency"));
            widgets::help(ui, "help.audio.targetLatency");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let dirty = self.latency_target_edit.is_some();
                if ui
                    .add_enabled(dirty, egui::Button::new(t("adaptive.apply")))
                    .clicked()
                {
                    let requested = (value.round() as i64).max(1);
                    {
                        let mut live = self.live.lock().unwrap();
                        live.app.latency.latency_requested_ms = Some(requested);
                        live.app.latency.latency_target_ms = Some(requested);
                    }
                    self.ctl
                        .send_int("/omniphony/control/latency_target", requested as i32);
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
            });
        });
    }

    /// The adaptive controller: its switches, its three groups of numbers,
    /// and the pause control.
    fn adaptive_form(&mut self, ui: &mut Ui, adaptive_on: bool, paused: bool) {
        ui.separator();
        let mut on = adaptive_on;
        if widgets::switch_row(ui, t("adaptive.title"), &mut on) {
            self.live.lock().unwrap().app.adaptive_resampling = Some(u8::from(on));
            self.send_audio_config();
        }
        ui.horizontal(|ui| {
            let label = if paused {
                format!("▶ {}", t("adaptive.resume"))
            } else {
                format!("⏸ {}", t("adaptive.pause"))
            };
            if ui
                .add_enabled(adaptive_on, egui::Button::new(label))
                .clicked()
            {
                self.live.lock().unwrap().app.adaptive_resampling_paused = Some(u8::from(!paused));
                self.send_audio_config();
            }
            // Only reachable while paused: it is a diagnostic, not a control.
            if adaptive_on && paused && ui.button(t("adaptive.resetRatio")).clicked() {
                self.ctl
                    .send_int("/omniphony/control/adaptive_resampling/reset_ratio", 1);
            }
            // The wizard patches this controller live, so it only makes sense
            // while there is one running to patch.
            if adaptive_on && !paused {
                self.auto_tune_button(ui);
            }
        });

        // The far mode fires when any of its three actions is armed.
        let far_mode = {
            let live = self.live.lock().unwrap();
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
        let silence = {
            let live = self.live.lock().unwrap();
            live.app
                .adaptive_resampling_force_silence_in_far_mode
                .unwrap_or(1)
                != 0
        };

        for (index, (caption, fields)) in SUBPANELS.iter().enumerate() {
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
                let (label, help, get, set) = &SWITCHES[*i];
                let mut value = {
                    let live = self.live.lock().unwrap();
                    get(&live.app)
                };
                ui.horizontal(|ui| {
                    ui.label(t(label));
                    widgets::help(ui, help);
                });
                if widgets::switch_row(ui, "", &mut value) {
                    {
                        let mut live = self.live.lock().unwrap();
                        set(&mut live.app, value);
                        // The far mode is not a switch of its own: it is on
                        // when any of its three actions is armed.
                        let derived = live
                            .app
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
                                != 0;
                        live.app.adaptive_resampling_enable_far_mode = Some(u8::from(derived));
                    }
                    self.send_audio_config();
                }
            }
            for field in *fields {
                let enabled = match field.gate {
                    Gate::Always => true,
                    Gate::FarMode => far_mode,
                    Gate::Adaptive => adaptive_on,
                    Gate::Silence => silence,
                };
                let stored = {
                    let live = self.live.lock().unwrap();
                    (field.get)(&live.app)
                };
                let mut value = *self.adaptive_edits.get(field.key).unwrap_or(&stored);
                ui.add_enabled_ui(enabled, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(t(field.label));
                        widgets::help(ui, field.help);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let mut drag = egui::DragValue::new(&mut value)
                                .speed(field.step)
                                .range(field.min..=field.max)
                                .fixed_decimals(field.decimals);
                            if !field.unit.is_empty() {
                                drag = drag.suffix(format!(" {}", field.unit));
                            }
                            if ui
                                .add_sized(egui::vec2(84.0, ui.spacing().interact_size.y), drag)
                                .changed()
                            {
                                self.adaptive_edits.insert(field.key, value);
                            }
                        });
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

    /// Write every edited field into the model and send the batch. The exit
    /// margin is corrected against the entry margin first: the hysteresis has
    /// to stay well formed whatever order the two were typed in.
    fn apply_adaptive_edits(&mut self) {
        let edits: BTreeMap<&'static str, f64> = std::mem::take(&mut self.adaptive_edits);
        {
            let mut live = self.live.lock().unwrap();
            for (_, fields) in SUBPANELS {
                for field in *fields {
                    if let Some(value) = edits.get(field.key) {
                        (field.set)(&mut live.app, value.clamp(field.min, field.max));
                    }
                }
            }
            let entry = live
                .app
                .adaptive_resampling_low_recover_entry_margin_ms
                .unwrap_or(18.0);
            let exit = live
                .app
                .adaptive_resampling_low_recover_exit_margin_ms
                .unwrap_or(6.0);
            live.app.adaptive_resampling_low_recover_exit_margin_ms =
                Some(exit.min((entry - 0.1).max(0.0)));
        }
        self.send_audio_config();
    }
}

/// The instant latency against its target, with the spread over the window.
fn latency_meter(
    ui: &mut Ui,
    state: &LatencyView,
    stats: Option<&crate::host::timing_stats::WindowStats>,
    target: Option<i64>,
) {
    // Full scale is twice the target, so the target sits mid-bar.
    let max_ms = match target {
        Some(t) => (2.0 * t as f64).max(100.0),
        None => 2000.0,
    };
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 8.0), egui::Sense::hover());
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

impl AppState {
    /// Snapshot of the latency block, so the panel does not hold the lock
    /// while it draws.
    pub fn clone_latency_view(&self) -> LatencyView {
        LatencyView {
            latency: self.latency.latency_ms,
            instant: self.latency.latency_instant_ms,
            control: self.latency.latency_control_ms,
            smoothed: self.latency.latency_smoothed_ms,
            downstream: self.latency.latency_downstream_ms,
            target: self.latency.latency_target_ms,
            requested: self.latency.latency_requested_ms,
        }
    }
}
