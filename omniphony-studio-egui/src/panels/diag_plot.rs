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

/// The uniform grid the spectrum is computed on, and so its sample rate:
/// 50 Hz, Nyquist 25 Hz — the web's `POLL_INTERVAL_MS`. Samples arrive on the
/// frame loop rather than on a timer, so this is a grid the series is
/// resampled onto and not a claim about when they were taken.
const SAMPLE_INTERVAL_MS: f64 = 20.0;
const FFT_SAMPLE_RATE_HZ: f64 = 1000.0 / SAMPLE_INTERVAL_MS;
/// Visible range below the peak, and the floor a bin is clamped to.
const FFT_DB_SPAN: f64 = 60.0;
const FFT_DB_CLIP_FLOOR: f64 = -120.0;
/// Mean of the Hanning window, which the per-bin magnitude is divided by so a
/// bin reads as the amplitude of the equivalent sinusoid.
const HANNING_COHERENT_GAIN: f64 = 0.5;
/// Below this the spectrum is too noisy to tell anyone anything.
const FFT_MIN_N: usize = 64;

/// The plot's persisted settings (`diagPlot.*` in the web's localStorage).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct DiagPlotPrefs {
    /// Which metric tab the chip row is showing.
    pub tier: String,
    pub window_ms: u64,
    pub rate_hz: u32,
    pub selected: BTreeSet<String>,
    /// `diagPlot.diffMode.v1`: plot the rate of change instead of the value.
    pub diff: bool,
    /// `diagPlot.fftMode.v1`: plot the spectrum instead of the time series.
    pub fft: bool,
}

impl Default for DiagPlotPrefs {
    fn default() -> Self {
        Self {
            tier: "base".to_owned(),
            window_ms: 10_000,
            rate_hz: 50,
            selected: BTreeSet::new(),
            diff: false,
            fft: false,
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

/// The series as the plot reads it: values, or their rate of change.
///
/// The derivative is taken between *changes*, not between samples: a metric
/// the renderer republishes unchanged would otherwise read as a stretch of
/// zeroes broken by a spike, which says something about the publication rate
/// rather than about the metric.
fn transformed(series: &VecDeque<(f64, f64)>, diff: bool) -> Vec<(f64, f64)> {
    if !diff {
        return series
            .iter()
            .filter(|(_, v)| v.is_finite())
            .copied()
            .collect();
    }
    let mut out = Vec::new();
    let mut last: Option<(f64, f64)> = None;
    for (t, v) in series.iter().filter(|(_, v)| v.is_finite()) {
        let Some((t0, v0)) = last else {
            last = Some((*t, *v));
            continue;
        };
        if *v == v0 {
            continue;
        }
        let dt = (*t - t0) / 1000.0;
        if dt > 0.0 {
            out.push((*t, (*v - v0) / dt));
        }
        last = Some((*t, *v));
    }
    out
}

/// Largest power of two not greater than `n`.
fn floor_pow2(n: usize) -> usize {
    let mut p = 1;
    while p * 2 <= n {
        p *= 2;
    }
    p
}

/// `n` samples on a uniform grid ending at the newest one, missing samples
/// interpolated between the values around them. `None` when the series does
/// not reach back far enough — a spectrum of a partly-invented signal would
/// be a picture of the invention.
fn uniform_resample(series: &[(f64, f64)], n: usize) -> Option<Vec<f64>> {
    if series.len() < 2 || n < 2 {
        return None;
    }
    let t_end = series[series.len() - 1].0;
    let t_start = t_end - (n - 1) as f64 * SAMPLE_INTERVAL_MS;
    if t_start < series[0].0 {
        return None;
    }
    let mut out = Vec::with_capacity(n);
    let mut index = 0;
    for i in 0..n {
        let t = t_start + i as f64 * SAMPLE_INTERVAL_MS;
        while index + 1 < series.len() && series[index + 1].0 <= t {
            index += 1;
        }
        let (t0, v0) = series[index];
        let (t1, v1) = series.get(index + 1).copied().unwrap_or((t0, v0));
        out.push(if t1 > t0 {
            v0 + (t - t0) / (t1 - t0) * (v1 - v0)
        } else {
            v0
        });
    }
    Some(out)
}

/// Iterative radix-2 Cooley-Tukey, in place. Forward only: nothing here needs
/// the inverse. `re` and `im` are the same power-of-two length.
fn fft_in_place(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    debug_assert_eq!(n, im.len());
    debug_assert!(n.is_power_of_two());
    // Bit-reversal permutation.
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    // Butterflies.
    let mut len = 2;
    while len <= n {
        let half = len >> 1;
        let theta = -2.0 * std::f64::consts::PI / len as f64;
        let (w_im, w_re) = theta.sin_cos();
        for start in (0..n).step_by(len) {
            let (mut cur_re, mut cur_im) = (1.0f64, 0.0f64);
            for k in 0..half {
                let a = start + k;
                let b = a + half;
                let t_re = cur_re * re[b] - cur_im * im[b];
                let t_im = cur_re * im[b] + cur_im * re[b];
                re[b] = re[a] - t_re;
                im[b] = im[a] - t_im;
                re[a] += t_re;
                im[a] += t_im;
                let next_re = cur_re * w_re - cur_im * w_im;
                cur_im = cur_re * w_im + cur_im * w_re;
                cur_re = next_re;
            }
        }
        len <<= 1;
    }
}

/// One-sided magnitude spectrum of a metric.
struct Spectrum {
    /// Half spectrum in dB relative to one unit of the metric, floored.
    db: Vec<f64>,
    peak_hz: f64,
    peak_db: f64,
    peak_amp: f64,
    bin_hz: f64,
}

/// The spectrum of `series` over `n` samples, or `None` when there is not
/// enough history.
fn spectrum(series: &[(f64, f64)], n: usize) -> Option<Spectrum> {
    let signal = uniform_resample(series, n)?;
    // The mean goes first: a metric with a large offset or a slow drift would
    // otherwise put everything under one enormous bin at zero.
    let mean = signal.iter().sum::<f64>() / n as f64;
    let mut re = vec![0.0; n];
    let mut im = vec![0.0; n];
    for (i, value) in signal.iter().enumerate() {
        let w = 0.5 * (1.0 - (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos());
        re[i] = (value - mean) * w;
    }
    fft_in_place(&mut re, &mut im);
    let half = (n >> 1) + 1;
    let scale_non_dc = 2.0 / (n as f64 * HANNING_COHERENT_GAIN);
    let scale_dc = 1.0 / (n as f64 * HANNING_COHERENT_GAIN);
    let mut db = Vec::with_capacity(half);
    let mut peak = (0usize, 0.0f64);
    for i in 0..half {
        let magnitude = re[i].hypot(im[i]);
        let amp = magnitude
            * if i == 0 || i == half - 1 {
                scale_dc
            } else {
                scale_non_dc
            };
        db.push(if amp > 0.0 {
            (20.0 * amp.log10()).max(FFT_DB_CLIP_FLOOR)
        } else {
            FFT_DB_CLIP_FLOOR
        });
        // Bin zero is the residue the window and the mean removal left; it is
        // not a tone, and it would win every time a trend survives the window.
        if i > 0 && amp > peak.1 {
            peak = (i, amp);
        }
    }
    let bin_hz = FFT_SAMPLE_RATE_HZ / n as f64;
    Some(Spectrum {
        peak_hz: peak.0 as f64 * bin_hz,
        peak_db: db[peak.0],
        peak_amp: peak.1,
        bin_hz,
        db,
    })
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
            .info("telemetry")
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
            let diff = self.prefs.diag_plot.diff;
            if ui
                .selectable_label(
                    diff,
                    RichText::new(if diff { "d/dt on" } else { "d/dt" }).color(if diff {
                        PALETTE[1]
                    } else {
                        theme::TEXT
                    }),
                )
                .on_hover_text("Plot the rate of change instead of the value")
                .clicked()
            {
                self.prefs.diag_plot.diff = !diff;
                self.mark_prefs_dirty();
            }
            let fft = self.prefs.diag_plot.fft;
            if ui
                .selectable_label(
                    fft,
                    RichText::new(if fft { "FFT on" } else { "FFT" }).color(if fft {
                        PALETTE[0]
                    } else {
                        theme::TEXT
                    }),
                )
                .on_hover_text("Plot the spectrum instead of the time series")
                .clicked()
            {
                self.prefs.diag_plot.fft = !fft;
                self.mark_prefs_dirty();
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
            if self.prefs.diag_plot.fft {
                self.diag_fft_panel(&painter, panel, metric, &font);
            } else {
                self.diag_panel(&painter, panel, metric, t_min, t_max, &font);
            }
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
        let unit = self.display_unit(metric);
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
        let visible: Vec<(f64, f64)> = transformed(series, self.prefs.diag_plot.diff)
            .into_iter()
            .filter(|(t, _)| *t >= t_min)
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

    /// A metric's unit as the panels label it: a rate of change is per second.
    fn display_unit(&self, metric: &Metric) -> String {
        match (metric.unit.is_empty(), self.prefs.diag_plot.diff) {
            (true, true) => " /s".to_owned(),
            (true, false) => String::new(),
            (false, true) => format!(" {}/s", metric.unit),
            (false, false) => format!(" {}", metric.unit),
        }
    }

    /// One metric's spectrum: magnitude in dB against frequency, on a scale
    /// snapped a few dB above the peak so the curve does not jump vertically
    /// every time the spectrum is recomputed.
    fn diag_fft_panel(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        metric: &Metric,
        font: &egui::FontId,
    ) {
        painter.hline(
            rect.x_range(),
            rect.bottom(),
            Stroke::new(1.0, Color32::from_white_alpha(18)),
        );
        let note = |text: String| {
            painter.text(
                rect.left_top() + vec2(4.0, 2.0),
                egui::Align2::LEFT_TOP,
                text,
                font.clone(),
                theme::TEXT_MUTED,
            );
        };
        let Some(series) = self.diag_series.get(&metric.name) else {
            note(format!("{}: no data", metric.label));
            return;
        };
        let series = transformed(series, self.prefs.diag_plot.diff);
        // The transform is bounded by both what has been collected and the
        // window the user chose, so a shorter window is also a coarser
        // spectrum — which is the trade the window control is making.
        let in_window = (self.prefs.diag_plot.window_ms as f64 / SAMPLE_INTERVAL_MS) as usize;
        let n = floor_pow2(series.len()).min(floor_pow2(in_window));
        if n < FFT_MIN_N {
            note(format!(
                "{}: building FFT… (need ≥ {:.1}s)",
                metric.label,
                FFT_MIN_N as f64 * SAMPLE_INTERVAL_MS / 1000.0
            ));
            return;
        }
        let Some(spectrum) = spectrum(&series, n) else {
            note(format!("{}: no data", metric.label));
            return;
        };
        let f_max = FFT_SAMPLE_RATE_HZ / 2.0;
        frequency_grid(painter, rect, f_max);
        let db_max = ((spectrum.peak_db + 3.0) / 5.0).ceil() * 5.0;
        let db_min = db_max - FFT_DB_SPAN;
        let x_for = |f: f64| rect.left() + ((f / f_max) as f32) * rect.width();
        let y_for = |db: f64| {
            rect.top()
                + (((db_max - db.max(db_min)) / FFT_DB_SPAN) as f32) * (rect.height() - 4.0)
                + 2.0
        };
        let points: Vec<Pos2> = spectrum
            .db
            .iter()
            .enumerate()
            .map(|(i, db)| egui::pos2(x_for(i as f64 * spectrum.bin_hz), y_for(*db)))
            .collect();
        painter.add(egui::Shape::line(
            points,
            Stroke::new(1.25, colour_for(&metric.name)),
        ));
        let peak = egui::pos2(x_for(spectrum.peak_hz), y_for(spectrum.peak_db));
        painter.circle_filled(peak, 3.0, MEAN_COLOUR);
        let label = format!("{:.2} Hz  {:.1} dB", spectrum.peak_hz, spectrum.peak_db);
        // The label goes left of the marker when it would run off the panel,
        // and under it when the peak is high enough that the label would sit
        // on the panel's own header line.
        let right = peak.x > rect.right() - 90.0;
        let under = peak.y < rect.top() + 26.0;
        let align = match (right, under) {
            (true, true) => egui::Align2::RIGHT_TOP,
            (true, false) => egui::Align2::RIGHT_BOTTOM,
            (false, true) => egui::Align2::LEFT_TOP,
            (false, false) => egui::Align2::LEFT_BOTTOM,
        };
        painter.text(
            peak + vec2(
                if right { -5.0 } else { 5.0 },
                if under { 5.0 } else { -4.0 },
            ),
            align,
            label,
            font.clone(),
            MEAN_COLOUR,
        );
        painter.text(
            rect.left_top() + vec2(4.0, 2.0),
            egui::Align2::LEFT_TOP,
            format!(
                "{}  peak {:.2} Hz @ {:.1} dB ({:.2e}{})  (res {:.3} Hz, win {:.2}s, N={n})",
                metric.label,
                spectrum.peak_hz,
                spectrum.peak_db,
                spectrum.peak_amp,
                self.display_unit(metric),
                spectrum.bin_hz,
                n as f64 * SAMPLE_INTERVAL_MS / 1000.0,
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

/// Quarter-hertz ticks under bold one-hertz ticks, the frequency counterpart
/// of the time grid.
fn frequency_grid(painter: &egui::Painter, rect: egui::Rect, f_max: f64) {
    let x_for = |f: f64| rect.left() + ((f / f_max) as f32) * rect.width();
    let minor = Stroke::new(1.0, Color32::from_white_alpha(18));
    let major = Stroke::new(1.0, Color32::from_white_alpha(38));
    let mut f = 0.25;
    while f <= f_max {
        let on_hertz = f.fract().abs() < 1e-9;
        painter.vline(
            x_for(f),
            rect.y_range(),
            if on_hertz { major } else { minor },
        );
        f += 0.25;
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

    /// A pure tone has to land in the bin it belongs to, at the amplitude it
    /// was generated with: that is what makes a peak readable as "3.1 Hz at
    /// 0.5 ms" rather than as a shape.
    #[test]
    fn a_tone_lands_in_its_own_bin_at_its_own_amplitude() {
        let n = 512;
        let hz = 3.125; // an exact bin at N=512, so no leakage to argue about
        let amplitude = 0.5;
        let series: Vec<(f64, f64)> = (0..n)
            .map(|i| {
                let t = i as f64 * SAMPLE_INTERVAL_MS;
                (
                    t,
                    2.0 + amplitude * (2.0 * std::f64::consts::PI * hz * t / 1000.0).sin(),
                )
            })
            .collect();
        let spectrum = spectrum(&series, n).expect("a spectrum");
        assert!(
            (spectrum.peak_hz - hz).abs() < spectrum.bin_hz,
            "peak at {} Hz, expected {hz}",
            spectrum.peak_hz
        );
        // Within a tenth of a dB of the amplitude it was built with — the
        // offset of 2.0 is removed, not measured.
        assert!(
            (spectrum.peak_amp - amplitude).abs() < 0.01,
            "peak amplitude {}",
            spectrum.peak_amp
        );
        assert!((spectrum.peak_db - 20.0 * amplitude.log10()).abs() < 0.2);
    }

    #[test]
    fn the_transform_lengths_are_powers_of_two_and_history_is_required() {
        assert_eq!(floor_pow2(63), 32);
        assert_eq!(floor_pow2(64), 64);
        assert_eq!(floor_pow2(1), 1);
        // Half a window of history cannot answer for a whole one.
        let series: Vec<(f64, f64)> = (0..10)
            .map(|i| (i as f64 * SAMPLE_INTERVAL_MS, 1.0))
            .collect();
        assert!(uniform_resample(&series, 64).is_none());
        assert_eq!(uniform_resample(&series, 4).map(|v| v.len()), Some(4));
    }

    /// The grid is uniform whatever the frame rate was: a sample missing from
    /// the middle is interpolated, not dropped, or every gap would shift the
    /// spectrum.
    #[test]
    fn a_gap_is_interpolated_onto_the_grid() {
        let series = [(0.0, 0.0), (40.0, 2.0)];
        assert_eq!(uniform_resample(&series, 3), Some(vec![0.0, 1.0, 2.0]));
    }

    /// The derivative is taken between changes: a republished identical value
    /// is not a moment where the metric stopped moving.
    #[test]
    fn the_rate_of_change_ignores_repeats() {
        let series: VecDeque<(f64, f64)> = [(0.0, 1.0), (1000.0, 1.0), (2000.0, 3.0)]
            .into_iter()
            .collect();
        assert_eq!(transformed(&series, true), vec![(2000.0, 1.0)]);
        // Without the transform every finite sample is kept as it is.
        assert_eq!(transformed(&series, false).len(), 3);
        // A non-finite sample is never a value.
        let holed: VecDeque<(f64, f64)> = [(0.0, f64::NAN), (1000.0, 2.0)].into_iter().collect();
        assert_eq!(transformed(&holed, false), vec![(1000.0, 2.0)]);
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
