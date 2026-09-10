//! The Diagnostics section and its metrics plot (`#diagSection`,
//! `controls/diag-plot.js`, host `commands/diag.rs`).
//!
//! The renderer's `DiagRegistry` is the source of truth: it publishes a schema
//! of every metric it exposes and a flat map of their current values, so adding
//! a metric on the Rust side makes it plottable here without a line of UI code.
//!
//! Telemetry is not free, so the renderer only publishes it while someone is
//! looking: opening the section enables publication and closing it stops it.
//! The enable is re-asserted once a second — cheap, and it self-heals after a
//! renderer restart.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::time::{Duration, Instant};

use egui::{Color32, Pos2, RichText, Stroke, Ui, vec2};
use serde::{Deserialize, Serialize};

use crate::app::StudioSpike;
use crate::ui::section::Section;
use crate::ui::{theme, widgets};

const WINDOW_OPTIONS_MS: &[u64] = &[5_000, 10_000, 30_000, 60_000];
const RATE_OPTIONS_HZ: &[u32] = &[10, 20, 50, 100, 200];
const TIERS: &[&str] = &["base", "advanced"];
/// One OSC int a second keeps the renderer publishing.
const KEEPALIVE: Duration = Duration::from_secs(1);
/// The plot's own canvas, as the web sizes it.
const CANVAS_HEIGHT: f32 = 240.0;

/// Trace colours, in the web's order.
const PALETTE: &[Color32] = &[
    Color32::from_rgb(0x7a, 0xd7, 0xff),
    Color32::from_rgb(0x9c, 0xff, 0xa3),
    Color32::from_rgb(0xff, 0xb8, 0x6b),
    Color32::from_rgb(0xd6, 0xa0, 0xff),
    Color32::from_rgb(0xff, 0x8d, 0xa6),
    Color32::from_rgb(0xff, 0xd1, 0x66),
    Color32::from_rgb(0xa0, 0xe5, 0xff),
    Color32::from_rgb(0xb6, 0xff, 0x8c),
    Color32::from_rgb(0xff, 0xa0, 0x7a),
    Color32::from_rgb(0xc0, 0x99, 0xff),
    Color32::from_rgb(0xff, 0x9b, 0xba),
    Color32::from_rgb(0xff, 0xe8, 0x8a),
];
const MEAN_COLOUR: Color32 = Color32::from_rgb(0xff, 0xd1, 0x66);
const PLOT_BG: Color32 = Color32::from_rgb(0x0f, 0x17, 0x24);

/// The plot's persisted settings (`diagPlot.*` in the web's localStorage).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct DiagPlotPrefs {
    /// Which metric tab the chip row is showing.
    pub tier: String,
    pub window_ms: u64,
    pub rate_hz: u32,
    pub selected: BTreeSet<String>,
}

impl Default for DiagPlotPrefs {
    fn default() -> Self {
        Self {
            tier: "base".to_owned(),
            window_ms: 10_000,
            rate_hz: 50,
            selected: BTreeSet::new(),
        }
    }
}

/// One metric of the published schema.
struct Metric {
    name: String,
    label: String,
    group: String,
    unit: String,
    tier: String,
}

fn colour_for(name: &str) -> Color32 {
    let hash = name
        .bytes()
        .fold(0i32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as i32));
    PALETTE[hash.unsigned_abs() as usize % PALETTE.len()]
}

/// The schema's metrics, minus the internal sentinels: an underscore-prefixed
/// name is a liveness or plumbing signal, not something to trace.
fn metrics(schema: Option<&serde_json::Value>) -> Vec<Metric> {
    let Some(items) = schema
        .and_then(|s| s.get("items"))
        .and_then(|i| i.as_array())
    else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let name = item.get("name")?.as_str()?;
            if name.starts_with('_') {
                return None;
            }
            let text = |key: &str| {
                item.get(key)
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned()
            };
            let label = match text("label") {
                l if l.is_empty() => name.to_owned(),
                l => l,
            };
            Some(Metric {
                name: name.to_owned(),
                label,
                group: text("group"),
                unit: text("unit"),
                // Anything the renderer does not call "base" is advanced.
                tier: if text("tier") == "base" {
                    "base".to_owned()
                } else {
                    "advanced".to_owned()
                },
            })
        })
        .collect()
}

/// A mean, written compactly: a 1.9M baseline must not blow up the label.
fn format_mean(v: f64) -> String {
    let abs = v.abs();
    if abs >= 1e6 || (abs > 0.0 && abs < 0.01) {
        format!("{v:.3e}")
    } else if abs >= 100.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.3}")
    }
}

impl StudioSpike {
    /// The section. Its disclosure *is* the web's plot toggle: the web needs a
    /// separate button because its header is not one, and here opening the
    /// section is exactly the gesture that says "I am looking at this".
    pub(crate) fn diagnostics_section(&mut self, ui: &mut Ui) {
        let schema = {
            let live = self.live.lock().unwrap();
            live.app.latency.diag_schema.clone()
        };
        let metrics = metrics(schema.as_ref());
        let summary = if self.prefs.diag_plot.selected.is_empty() {
            String::new()
        } else {
            format!("{}", self.prefs.diag_plot.selected.len())
        };
        let open = Section::new("diagSection", "section.diagnostics")
            .summary(summary)
            .max_height(CANVAS_HEIGHT + 160.0)
            .show(ui, |ui| {
                self.diag_controls(ui, &metrics);
                self.diag_canvas(ui, &metrics);
            })
            .is_some();
        self.maintain_diag_publication(open, ui.ctx());
    }

    fn diag_controls(&mut self, ui: &mut Ui, metrics: &[Metric]) {
        ui.horizontal_wrapped(|ui| {
            let tier = self.prefs.diag_plot.tier.clone();
            for name in TIERS {
                if ui.selectable_label(tier == *name, *name).clicked() {
                    self.prefs.diag_plot.tier = (*name).to_owned();
                    self.mark_prefs_dirty();
                }
            }
            ui.separator();
            let window = self.prefs.diag_plot.window_ms;
            egui::ComboBox::from_id_salt("diag-window")
                .selected_text(format!("{} s", window / 1000))
                .width(64.0)
                .show_ui(ui, |ui| {
                    for option in WINDOW_OPTIONS_MS {
                        if ui
                            .selectable_label(window == *option, format!("{} s", option / 1000))
                            .clicked()
                        {
                            self.prefs.diag_plot.window_ms = *option;
                            self.mark_prefs_dirty();
                        }
                    }
                });
            let rate = self.prefs.diag_plot.rate_hz;
            egui::ComboBox::from_id_salt("diag-rate")
                .selected_text(format!("{rate} Hz"))
                .width(72.0)
                .show_ui(ui, |ui| {
                    for option in RATE_OPTIONS_HZ {
                        if ui
                            .selectable_label(rate == *option, format!("{option} Hz"))
                            .clicked()
                        {
                            self.prefs.diag_plot.rate_hz = *option;
                            self.mark_prefs_dirty();
                            self.ctl
                                .send_float("/omniphony/control/diag/rate_hz", *option as f32);
                        }
                    }
                });
            if ui
                .selectable_label(self.diag_paused, if self.diag_paused { "▶" } else { "❚❚" })
                .clicked()
            {
                self.diag_paused = !self.diag_paused;
            }
        });
        if metrics.is_empty() {
            widgets::note(ui, "No diag metrics registered yet.");
            return;
        }
        // Chips, grouped the way the renderer registered them. Metrics of the
        // other tab keep plotting; they are simply not listed here.
        let tier = self.prefs.diag_plot.tier.clone();
        let mut groups: Vec<&str> = Vec::new();
        for metric in metrics.iter().filter(|m| m.tier == tier) {
            if !groups.contains(&metric.group.as_str()) {
                groups.push(&metric.group);
            }
        }
        if groups.is_empty() {
            widgets::note(ui, &format!("No {tier} metrics."));
            return;
        }
        for group in groups {
            ui.horizontal_wrapped(|ui| {
                if !group.is_empty() {
                    ui.label(
                        RichText::new(group)
                            .size(theme::FONT_SIZE_SMALL)
                            .color(theme::TEXT_DIM),
                    );
                }
                for metric in metrics
                    .iter()
                    .filter(|m| m.tier == tier && m.group == group)
                {
                    let on = self.prefs.diag_plot.selected.contains(&metric.name);
                    let text = RichText::new(&metric.label).color(if on {
                        colour_for(&metric.name)
                    } else {
                        theme::TEXT_MUTED
                    });
                    if ui.selectable_label(on, text).clicked() {
                        if on {
                            self.prefs.diag_plot.selected.remove(&metric.name);
                            self.diag_series.remove(&metric.name);
                        } else {
                            self.prefs.diag_plot.selected.insert(metric.name.clone());
                        }
                        self.mark_prefs_dirty();
                    }
                }
            });
        }
    }

    /// The stacked panels, one per selected metric, each on its own y scale:
    /// metrics of wildly different magnitudes are the normal case, and one
    /// shared scale would flatten all but the largest into a line.
    fn diag_canvas(&mut self, ui: &mut Ui, metrics: &[Metric]) {
        let (rect, _) = ui.allocate_exact_size(
            vec2(ui.available_width(), CANVAS_HEIGHT),
            egui::Sense::hover(),
        );
        let painter = ui.painter().with_clip_rect(rect);
        painter.rect_filled(rect, 2.0, PLOT_BG);
        let showing: Vec<&Metric> = metrics
            .iter()
            .filter(|m| self.prefs.diag_plot.selected.contains(&m.name))
            .collect();
        let font = egui::FontId::proportional(theme::FONT_SIZE_SMALL);
        if showing.is_empty() {
            painter.text(
                rect.left_center() + vec2(6.0, 0.0),
                egui::Align2::LEFT_CENTER,
                "Select one or more metrics above.",
                font,
                theme::TEXT_MUTED,
            );
            return;
        }
        let now_ms = self.diag_started.elapsed().as_secs_f64() * 1000.0;
        let t_max = now_ms;
        let t_min = t_max - self.prefs.diag_plot.window_ms as f64;
        let panel_h = rect.height() / showing.len() as f32;
        for (index, metric) in showing.iter().enumerate() {
            let top = rect.top() + panel_h * index as f32;
            let panel = egui::Rect::from_min_size(
                egui::pos2(rect.left(), top),
                vec2(rect.width(), panel_h),
            );
            self.diag_panel(&painter, panel, metric, t_min, t_max, &font);
        }
    }

    fn diag_panel(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        metric: &Metric,
        t_min: f64,
        t_max: f64,
        font: &egui::FontId,
    ) {
        time_grid(painter, rect, t_min, t_max);
        painter.hline(
            rect.x_range(),
            rect.bottom(),
            Stroke::new(1.0, Color32::from_white_alpha(18)),
        );
        let unit = if metric.unit.is_empty() {
            String::new()
        } else {
            format!(" {}", metric.unit)
        };
        let Some(series) = self.diag_series.get(&metric.name) else {
            painter.text(
                rect.left_top() + vec2(4.0, 2.0),
                egui::Align2::LEFT_TOP,
                format!("{}: no data", metric.label),
                font.clone(),
                theme::TEXT_MUTED,
            );
            return;
        };
        let visible: Vec<(f64, f64)> = series
            .iter()
            .filter(|(t, v)| *t >= t_min && v.is_finite())
            .copied()
            .collect();
        if visible.is_empty() {
            painter.text(
                rect.left_top() + vec2(4.0, 2.0),
                egui::Align2::LEFT_TOP,
                format!("{}: no data", metric.label),
                font.clone(),
                theme::TEXT_MUTED,
            );
            return;
        }
        let raw_min = visible
            .iter()
            .map(|(_, v)| *v)
            .fold(f64::INFINITY, f64::min);
        let raw_max = visible
            .iter()
            .map(|(_, v)| *v)
            .fold(f64::NEG_INFINITY, f64::max);
        let mean = visible.iter().map(|(_, v)| *v).sum::<f64>() / visible.len() as f64;
        // A flat trace still needs a scale, and one with no headroom would
        // draw on the panel's own edges.
        let (mut v_min, mut v_max) = (raw_min, raw_max);
        if v_max - v_min < 1e-9 {
            v_max = v_min + 1.0;
        }
        let pad = (v_max - v_min) * 0.1;
        v_min -= pad;
        v_max += pad;
        let x_for = |t: f64| rect.left() + (((t - t_min) / (t_max - t_min)) as f32) * rect.width();
        let y_for = |v: f64| {
            rect.top() + (((v_max - v) / (v_max - v_min)) as f32) * (rect.height() - 4.0) + 2.0
        };
        let points: Vec<Pos2> = visible
            .iter()
            .map(|(t, v)| egui::pos2(x_for(*t), y_for(*v)))
            .collect();
        painter.add(egui::Shape::line(
            points,
            Stroke::new(1.5, colour_for(&metric.name)),
        ));

        // The mean as a dashed reference, so the baseline stays readable even
        // when the trace is noisy around it. Meaningless when the window's
        // range collapses to a point.
        if raw_max - raw_min > 1e-9 {
            let y = y_for(mean);
            painter.add(egui::Shape::dashed_line(
                &[egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
                Stroke::new(1.0, MEAN_COLOUR),
                4.0,
                3.0,
            ));
            painter.text(
                egui::pos2(
                    rect.right() - 3.0,
                    y.clamp(rect.top() + 14.0, rect.bottom() - 6.0),
                ),
                egui::Align2::RIGHT_CENTER,
                format!("μ {}{unit}", format_mean(mean)),
                font.clone(),
                MEAN_COLOUR,
            );
        }
        painter.text(
            rect.left_top() + vec2(4.0, 2.0),
            egui::Align2::LEFT_TOP,
            format!(
                "{} {raw_min:.2}…{raw_max:.2}{unit} (Δ {:.2})",
                metric.label,
                raw_max - raw_min
            ),
            font.clone(),
            theme::TEXT,
        );
    }

    /// Sample the published values, hold the publication open, and keep the
    /// window repainting while the plot is on screen.
    fn maintain_diag_publication(&mut self, open: bool, ctx: &egui::Context) {
        if !open {
            if self.diag_keepalive_at.take().is_some() {
                self.ctl.send_int("/omniphony/control/diag/enabled", 0);
                self.diag_series.clear();
            }
            return;
        }
        let now = Instant::now();
        if self
            .diag_keepalive_at
            .is_none_or(|at| now.duration_since(at) >= KEEPALIVE)
        {
            self.ctl.send_int("/omniphony/control/diag/enabled", 1);
            // The rate is restated with the enable: a renderer that restarted
            // came back on its own default.
            self.ctl.send_float(
                "/omniphony/control/diag/rate_hz",
                self.prefs.diag_plot.rate_hz as f32,
            );
            self.diag_keepalive_at = Some(now);
        }
        // A plot only redraws when something asks it to, and telemetry arrives
        // without any input event.
        ctx.request_repaint();
        if self.diag_paused {
            return;
        }
        let t = self.diag_started.elapsed().as_secs_f64() * 1000.0;
        let cutoff = t - WINDOW_OPTIONS_MS[WINDOW_OPTIONS_MS.len() - 1] as f64;
        let values = {
            let live = self.live.lock().unwrap();
            live.app.latency.diag_values.clone()
        };
        let Some(values) = values else { return };
        for name in &self.prefs.diag_plot.selected {
            let Some(v) = values.get(name).and_then(serde_json::Value::as_f64) else {
                continue;
            };
            let series = self
                .diag_series
                .entry(name.clone())
                .or_insert_with(VecDeque::new);
            series.push_back((t, v));
            while series.front().is_some_and(|(t0, _)| *t0 < cutoff) {
                series.pop_front();
            }
        }
        // Series of metrics that are no longer selected are dropped on
        // deselection; this catches the ones a schema change removed.
        self.diag_series
            .retain(|name, _| self.prefs.diag_plot.selected.contains(name));
    }
}

/// 250 ms ticks under bold 1 s ticks, so the seconds stay visually dominant.
fn time_grid(painter: &egui::Painter, rect: egui::Rect, t_min: f64, t_max: f64) {
    let x_for = |t: f64| rect.left() + (((t - t_min) / (t_max - t_min)) as f32) * rect.width();
    let minor = Stroke::new(1.0, Color32::from_white_alpha(10));
    let major = Stroke::new(1.0, Color32::from_white_alpha(38));
    let mut t = (t_min / 250.0).ceil() * 250.0;
    while t <= t_max {
        let on_second = (t / 1000.0).fract().abs() < 1e-9;
        painter.vline(
            x_for(t),
            rect.y_range(),
            if on_second { major } else { minor },
        );
        t += 250.0;
    }
}

/// Series held per metric, keyed by name: the plot is the only reader, and a
/// map per sample would allocate at the poll rate.
pub type DiagSeries = HashMap<String, VecDeque<(f64, f64)>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schema_hides_the_renderers_own_plumbing_signals() {
        let schema = serde_json::json!({ "items": [
            { "name": "_diag_alive", "label": "alive", "tier": "base" },
            { "name": "latency_ms", "label": "Latency", "group": "clock",
              "unit": "ms", "tier": "base" },
            { "name": "ratio", "group": "clock" },
        ]});
        let items = metrics(Some(&schema));
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "latency_ms");
        assert_eq!(items[0].unit, "ms");
        // No label falls back to the name, and anything not called "base" is
        // advanced.
        assert_eq!(items[1].label, "ratio");
        assert_eq!(items[1].tier, "advanced");
        assert!(metrics(None).is_empty());
    }

    #[test]
    fn a_metric_always_gets_the_same_colour() {
        assert_eq!(colour_for("latency_ms"), colour_for("latency_ms"));
        assert!(PALETTE.contains(&colour_for("anything at all")));
    }

    #[test]
    fn means_stay_short_whatever_their_magnitude() {
        assert_eq!(format_mean(1.5), "1.500");
        assert_eq!(format_mean(150.25), "150.2");
        assert_eq!(format_mean(0.0), "0.000");
        assert!(format_mean(1.9e6).contains('e'));
        assert!(format_mean(0.0001).contains('e'));
    }
}
