//! The Latency section: the latency meter with its markers, the trace of the
//! latency and of the rate that holds it, the target latency, and the
//! settings of the loop (`docs/resampling-rework-plan.md`).
//!
//! Each number is typed and applied on its own: a new target or loop setting
//! reopens the output, which is not something a drag should do on the way.

use egui::{Color32, Ui};

use crate::app::StudioSpike;
use crate::host::commands::resampling::{self, SyncSetting};
use crate::i18n::{t, tf};
use crate::model::app_state::AppState;
use crate::ui::section::Section;
use crate::ui::{theme, widgets};

impl StudioSpike {
    pub(crate) fn latency_section(&mut self, ui: &mut Ui) {
        let policy = crate::host::capabilities::ActionPolicy::of(&self.host);
        if !policy.audio_output {
            return;
        }
        let (state, stats) = {
            let live = self.host.read();
            (LatencyView::of(&live.app), live.latency_stats())
        };
        let target = state.requested.or(state.target).or(state.latency);
        // What the meter is read against: the latency the renderer holds,
        // which is the target unless the stream needed more.
        let meter_target = state.held.or(target);
        // The meter sits in the header, beside the title, as the web's grid
        // puts it (`#latencySection`, column 2 of row 1): in view whether the
        // section is open or folded, with the numbers it summarises below it.
        // Bar and reading are one widget rather than a header widget and the
        // section's summary, so the reading gets a fixed box and cannot move
        // the bar from one frame to the next.
        let reading = reading(state.instant);
        Section::new("latencySection", "section.latency")
            .icon(&crate::ui::icons::SECTION_LATENCY)
            .header_widget(|ui| {
                crate::ui::meter::row_with_readout(ui, READOUT_ADVANCES, &reading, |ui, width| {
                    latency_meter(ui, width, &state, stats.as_ref(), meter_target);
                })
            })
            .show(ui, |ui| {
                readouts(ui, &state, stats.as_ref());
                // The trace sits between the gauge and the target: the gauge
                // says where the latency is now, the plot how it got there.
                self.resample_plot(ui);
                ui.add_enabled_ui(policy.renderer_ready, |ui| {
                    self.target_latency_row(ui, target)
                });
                raised_target_banner(ui, &state);
                if self.advanced {
                    ui.add_enabled_ui(policy.renderer_ready, |ui| self.sync_rows(ui));
                }
            });
    }

    /// The latency to hold, from the source's samples to the device's
    /// output: typed, then applied.
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
                    .add_enabled(dirty, egui::Button::new(t("common.apply")))
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

    /// The loop's two settings, each typed and applied like the target
    /// above: a new value reopens the output.
    fn sync_rows(&mut self, ui: &mut Ui) {
        const ROWS: [(SyncSetting, &str, &str, f64, usize, &str); 2] = [
            (
                SyncSetting::LoopBandwidthHz,
                "sync.loopBandwidth",
                "help.sync.loopBandwidth",
                0.001,
                3,
                " Hz",
            ),
            (
                SyncSetting::RealignThresholdMs,
                "sync.realignThreshold",
                "help.sync.realignThreshold",
                1.0,
                0,
                " ms",
            ),
        ];
        // The clock the source runs on: taken at once, like a choice.
        let clock = resampling::source_clock(&self.host);
        let mut chosen = clock.clone();
        let label_of = |name: &str| t(&format!("sync.sourceClock.{name}"));
        widgets::label_row_help(ui, t("sync.sourceClock"), "help.sync.sourceClock", |ui| {
            widgets::bounded_combo(ui, 200.0, |ui, width| {
                egui::ComboBox::from_id_salt("sync-source-clock")
                    .selected_text(label_of(&clock))
                    .width(width)
                    .truncate()
                    .show_ui(ui, |ui| {
                        for name in resampling::SOURCE_CLOCKS {
                            ui.selectable_value(&mut chosen, name.to_owned(), label_of(name));
                        }
                    })
            });
        });
        if chosen != clock {
            resampling::set_source_clock(&self.host, &chosen);
        }
        for (index, (setting, label, help, speed, decimals, suffix)) in ROWS.into_iter().enumerate()
        {
            let (min, max) = setting.range();
            let mut value = self.sync_edits[index].unwrap_or_else(|| setting.get(&self.host));
            widgets::label_row_help(ui, t(label), help, |ui| {
                let dirty = self.sync_edits[index].is_some();
                if ui
                    .add_enabled(dirty, egui::Button::new(t("common.apply")))
                    .clicked()
                {
                    resampling::set_sync_setting(&self.host, setting, value);
                    self.sync_edits[index] = None;
                }
                if ui
                    .add_sized(
                        egui::vec2(72.0, ui.spacing().interact_size.y),
                        egui::DragValue::new(&mut value)
                            .speed(speed)
                            .range(min..=max)
                            .fixed_decimals(decimals)
                            .suffix(suffix),
                    )
                    .changed()
                {
                    self.sync_edits[index] = Some(value);
                }
            });
        }
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
    if let Some(target) = target {
        painter.circle_filled(
            egui::pos2(at(target as f64), rect.top() - 3.0),
            2.5,
            Color32::from_rgb(0x52, 0xe2, 0xa2),
        );
    }
}

/// The numbers under the meter: the latency now, between the least and the
/// most of the window.
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
}

/// The stream needs more latency than the target asks for, and the renderer
/// holds that instead: said under the target, with the value to set.
fn raised_target_banner(ui: &mut Ui, state: &LatencyView) {
    let (Some(held), Some(requested)) = (state.held, state.requested) else {
        return;
    };
    if held <= requested {
        return;
    }
    let (held, target) = (held.to_string(), requested.to_string());
    let values = [("held", held.as_str()), ("target", target.as_str())];
    widgets::banner(
        ui,
        widgets::Severity::Warning,
        &tf("latency.raisedTitle", &values),
        Some(&tf("latency.raisedBody", &values)),
    );
}

/// The latency numbers this panel reads, copied out under the lock.
pub struct LatencyView {
    pub latency: Option<i64>,
    pub instant: Option<i64>,
    pub target: Option<i64>,
    pub requested: Option<i64>,
    /// The latency held in place of the requested target, when the stream
    /// needs more.
    pub held: Option<i64>,
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
            target: app.latency.latency_target_ms,
            requested: app.latency.latency_requested_ms,
            held: app.latency.latency_held_ms,
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
