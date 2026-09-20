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

use std::collections::{BTreeSet, VecDeque};

use egui::{Color32, Pos2, RichText, Stroke, Ui, vec2};
use serde::{Deserialize, Serialize};

use crate::app::StudioSpike;
use crate::host::commands::diag;
use crate::host::services::interests;
use crate::ui::section::Section;
use crate::ui::{theme, widgets};

const WINDOW_OPTIONS_MS: &[u64] = &[5_000, 10_000, 30_000, 60_000];
const RATE_OPTIONS_HZ: &[u32] = &[10, 20, 50, 100, 200];
const TIERS: &[&str] = &["base", "advanced"];
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
/// 50 Hz, Nyquist 25 Hz — the web's `POLL_INTERVAL_MS`. Reception timestamps
/// are resampled onto this grid, independently of the display frame rate.
const SAMPLE_INTERVAL_MS: f64 = 20.0;
const MAX_GAP_MS: f64 = 1_000.0;
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

/// What a drawn panel leaves behind for the measurement overlay: where it is,
/// how to read a position in it, and what unit to say.
struct PanelInfo {
    rect: egui::Rect,
    unit: String,
    /// `None` when the panel drew a note instead of a trace: there is nothing
    /// to measure on it.
    scale: Option<PanelScale>,
}

enum PanelScale {
    Time { v_min: f64, v_max: f64 },
    Freq(Box<Spectrum>),
}

impl PanelInfo {
    fn empty(rect: egui::Rect, unit: String) -> Self {
        Self {
            rect,
            unit,
            scale: None,
        }
    }
}

/// The rectangle a drag is describing, in canvas coordinates.
#[derive(Clone, Copy, Debug)]
pub struct DiagSelection {
    pub panel: usize,
    pub start: Pos2,
    pub end: Pos2,
}

/// The series as the plot reads it: values, or their rate of change.
///
/// The derivative is taken between *changes*, not between samples: a metric
/// the renderer republishes unchanged would otherwise read as a stretch of
/// zeroes broken by a spike, which says something about the publication rate
/// rather than about the metric.
fn transformed<'a>(
    series: impl IntoIterator<Item = &'a (f64, f64)>,
    diff: bool,
) -> Vec<(f64, f64)> {
    if !diff {
        return series
            .into_iter()
            .filter(|(_, v)| v.is_finite())
            .copied()
            .collect();
    }
    let mut out = Vec::new();
    let mut last: Option<(f64, f64)> = None;
    for (t, v) in series.into_iter().filter(|(_, v)| v.is_finite()) {
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

/// Split on reception timestamps BEFORE deriving changes. Constant values are
/// still receipts; slow-changing metrics must not look disconnected.
fn transformed_segments(series: &VecDeque<(f64, f64)>, diff: bool) -> Vec<Vec<(f64, f64)>> {
    let mut result = Vec::new();
    let mut start = 0;
    for i in 1..series.len() {
        if series[i].0 - series[i - 1].0 > MAX_GAP_MS {
            result.push(transformed(series.range(start..i), diff));
            start = i;
        }
    }
    result.push(transformed(series.range(start..), diff));
    result
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
pub struct Spectrum {
    /// Half spectrum in dB relative to one unit of the metric, floored.
    db: Vec<f64>,
    /// The same bins in the metric's own unit.
    amp: Vec<f64>,
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
    let mut amps = Vec::with_capacity(half);
    let mut peak = (0usize, 0.0f64);
    for i in 0..half {
        let magnitude = re[i].hypot(im[i]);
        let amp = magnitude
            * if i == 0 || i == half - 1 {
                scale_dc
            } else {
                scale_non_dc
            };
        amps.push(amp);
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
        amp: amps,
    })
}

/// What the selection reports in a time-domain panel: the interval it spans,
/// the frequency a cycle of that length would have, and the value it crosses.
/// A rectangle drawn around one period of a ripple is the measurement this
/// exists for.
fn time_lines(dt_ms: f64, dv: f64, unit: &str) -> Vec<String> {
    let hz = if dt_ms > 0.0 { 1000.0 / dt_ms } else { 0.0 };
    vec![
        format!("Δt {:.*} ms", if dt_ms >= 100.0 { 1 } else { 2 }, dt_ms),
        format!("f  {:.*} Hz", if hz < 100.0 { 2 } else { 0 }, hz),
        format!("Δv {:.*}{unit}", if dv >= 100.0 { 1 } else { 3 }, dv),
    ]
}

/// What it reports in a spectrum panel. The dB values are read out of the
/// spectrum at those frequencies, never from where the pointer happens to be:
/// the vertical position of a drag says nothing about the signal.
fn freq_lines(spectrum: &Spectrum, f_start: f64, f_end: f64, unit: &str) -> Vec<String> {
    let half = spectrum.db.len();
    let bin = |f: f64| ((f / spectrum.bin_hz).round() as usize).min(half.saturating_sub(1));
    let delta = (f_end - f_start).abs();
    // Under fifty millihertz nobody was drawing a band; they were pointing at
    // a bin.
    if delta < 0.05 {
        let i = bin(f_end);
        return vec![
            format!("f   {:.3} Hz", i as f64 * spectrum.bin_hz),
            format!("|X| {:.1} dB", spectrum.db[i]),
            format!("amp {:.2e}{unit}", spectrum.amp[i]),
        ];
    }
    let (a, b) = (bin(f_start.min(f_end)), bin(f_start.max(f_end)));
    let mut lines = vec![
        format!("Δf   {:.*} Hz", if delta >= 10.0 { 2 } else { 3 }, delta),
        format!(
            "{:.2} Hz {:.1} dB",
            a as f64 * spectrum.bin_hz,
            spectrum.db[a]
        ),
        format!(
            "{:.2} Hz {:.1} dB",
            b as f64 * spectrum.bin_hz,
            spectrum.db[b]
        ),
        format!("ΔdB  {:+.1}", spectrum.db[b] - spectrum.db[a]),
    ];
    // The peak of the band as the spectrum has it, not as the rectangle was
    // drawn: bin zero is excluded for the same reason it is excluded globally.
    let lo = a.max(1);
    if lo <= b
        && let Some((i, _)) = spectrum.db[lo..=b]
            .iter()
            .enumerate()
            .max_by(|x, y| x.1.total_cmp(y.1))
    {
        let i = lo + i;
        lines.push(format!(
            "peak {:.2} Hz @ {:.1} dB ({:.2e}{unit})",
            i as f64 * spectrum.bin_hz,
            spectrum.db[i],
            spectrum.amp[i]
        ));
    }
    lines
}

impl StudioSpike {
    /// The section. Its disclosure *is* the web's plot toggle: the web needs a
    /// separate button because its header is not one, and here opening the
    /// section is exactly the gesture that says "I am looking at this".
    pub(crate) fn diagnostics_section(&mut self, ui: &mut Ui) {
        let schema = {
            let live = self.host.read();
            live.app.latency.diag_schema.clone()
        };
        let metrics = metrics(schema.as_ref());
        let summary = if self.prefs.diag_plot.selected.is_empty() {
            String::new()
        } else {
            format!("{}", self.prefs.diag_plot.selected.len())
        };
        let open = Section::new("diagSection", "section.diagnostics")
            .icon(&crate::ui::icons::SECTION_DIAGNOSTICS)
            .info("telemetry")
            .summary(summary)
            .show(ui, |ui| {
                self.diag_controls(ui, &metrics);
                self.sample_diag_trace(true);
                self.diag_canvas(ui, &metrics);
            })
            .is_some();
        if !open {
            self.sample_diag_trace(false);
        }
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
        // The window and rate dropdowns get a row of their own: a ComboBox is
        // a nested layout, not an atomic widget, so a wrapped row cannot carry
        // it to the next line — squeezed in with the toggles, it pushed the
        // whole section past the panel.
        ui.horizontal(|ui| {
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
                            diag::control_diag_rate_hz(&self.host, *option as f32);
                        }
                    }
                });
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
                    // A wrapped row only breaks *between* chips; a chip whose
                    // name alone is wider than the panel is cut to it instead,
                    // and says its whole name on hover.
                    if ui
                        .add_enabled(
                            on || self.prefs.diag_plot.selected.len()
                                < crate::host::diagnostics::MAX_METRICS,
                            egui::Button::selectable(on, text).truncate(),
                        )
                        .on_hover_text(&metric.label)
                        .clicked()
                    {
                        if on {
                            self.prefs.diag_plot.selected.remove(&metric.name);
                            self.diag_trace.series.remove(&metric.name);
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
        // The plot is a measuring instrument while it is paused, or whenever
        // the axis is frequency: a rectangle over a time-domain trace that is
        // still scrolling would be measuring a moment that has already left.
        let measurable = self.diag_paused || self.prefs.diag_plot.fft;
        let (rect, response) = ui.allocate_exact_size(
            vec2(ui.available_width(), CANVAS_HEIGHT),
            if measurable {
                egui::Sense::click_and_drag()
            } else {
                egui::Sense::hover()
            },
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
        let t_max = self.diag_trace.end_ms;
        let t_min = t_max - self.prefs.diag_plot.window_ms as f64;
        let panel_h = rect.height() / showing.len() as f32;
        let mut panels = Vec::with_capacity(showing.len());
        for (index, metric) in showing.iter().enumerate() {
            let top = rect.top() + panel_h * index as f32;
            let panel = egui::Rect::from_min_size(
                egui::pos2(rect.left(), top),
                vec2(rect.width(), panel_h),
            );
            panels.push(if self.prefs.diag_plot.fft {
                self.diag_fft_panel(&painter, panel, metric, &font)
            } else {
                self.diag_panel(&painter, panel, metric, t_min, t_max, &font)
            });
        }
        if !measurable {
            // A selection made while paused would otherwise reappear, anchored
            // to instants the window has scrolled past.
            self.diag_selection = None;
            return;
        }
        if response.drag_started()
            && let Some(at) = response.interact_pointer_pos()
        {
            let panel = ((at.y - rect.top()) / panel_h) as usize;
            self.diag_selection = (panel < panels.len()).then_some(DiagSelection {
                panel,
                start: at,
                end: at,
            });
        }
        if response.dragged()
            && let Some(at) = response.interact_pointer_pos()
            && let Some(selection) = &mut self.diag_selection
        {
            selection.end = at;
        }
        if let Some(selection) = self.diag_selection {
            self.diag_measurement(&painter, rect, &panels, selection, t_min, t_max, &font);
        }
    }

    /// The measuring rectangle and its readout.
    #[allow(clippy::too_many_arguments)]
    fn diag_measurement(
        &self,
        painter: &egui::Painter,
        canvas: egui::Rect,
        panels: &[PanelInfo],
        selection: DiagSelection,
        t_min: f64,
        t_max: f64,
        font: &egui::FontId,
    ) {
        let Some(panel) = panels.get(selection.panel) else {
            return;
        };
        let Some(scale) = &panel.scale else { return };
        let box_rect = egui::Rect::from_two_pos(selection.start, selection.end);
        painter.rect_filled(
            box_rect,
            0.0,
            Color32::from_rgba_unmultiplied(26, 21, 7, 26),
        );
        painter.rect_stroke(
            box_rect,
            0.0,
            Stroke::new(1.0, MEAN_COLOUR.gamma_multiply(0.85)),
            egui::StrokeKind::Inside,
        );
        let lines = match scale {
            PanelScale::Time { v_min, v_max } => {
                let to_t = |x: f32| {
                    t_min + ((x - canvas.left()) / canvas.width()) as f64 * (t_max - t_min)
                };
                let to_v = |y: f32| {
                    let norm = ((y - panel.rect.top() - 2.0) / (panel.rect.height() - 4.0)) as f64;
                    v_max - norm * (v_max - v_min)
                };
                time_lines(
                    (to_t(selection.end.x) - to_t(selection.start.x)).abs(),
                    (to_v(selection.end.y) - to_v(selection.start.y)).abs(),
                    &panel.unit,
                )
            }
            PanelScale::Freq(spectrum) => {
                let f_max = FFT_SAMPLE_RATE_HZ / 2.0;
                let to_f = |x: f32| {
                    (((x - canvas.left()) / canvas.width()) as f64 * f_max).clamp(0.0, f_max)
                };
                freq_lines(
                    spectrum,
                    to_f(selection.start.x),
                    to_f(selection.end.x),
                    &panel.unit,
                )
            }
        };
        // The box goes to the right of the rectangle, or to its left when
        // there is no room, and never off the canvas.
        let width = lines
            .iter()
            .map(|line| line.chars().count() as f32 * 5.4)
            .fold(0.0f32, f32::max)
            + 12.0;
        let height = lines.len() as f32 * 12.0 + 8.0;
        let x = if box_rect.right() + 6.0 + width <= canvas.right() {
            box_rect.right() + 6.0
        } else {
            (box_rect.left() - width - 6.0).max(canvas.left())
        };
        let y = box_rect
            .top()
            .min(canvas.bottom() - height)
            .max(canvas.top());
        let readout = egui::Rect::from_min_size(egui::pos2(x, y), vec2(width, height));
        painter.rect_filled(readout, 2.0, PLOT_BG.gamma_multiply(0.94));
        painter.rect_stroke(
            readout,
            2.0,
            Stroke::new(1.0, MEAN_COLOUR.gamma_multiply(0.7)),
            egui::StrokeKind::Inside,
        );
        for (index, line) in lines.iter().enumerate() {
            painter.text(
                readout.left_top() + vec2(6.0, 4.0 + index as f32 * 12.0),
                egui::Align2::LEFT_TOP,
                line,
                font.clone(),
                MEAN_COLOUR,
            );
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
    ) -> PanelInfo {
        time_grid(painter, rect, t_min, t_max);
        painter.hline(
            rect.x_range(),
            rect.bottom(),
            Stroke::new(1.0, Color32::from_white_alpha(18)),
        );
        let unit = self.display_unit(metric);
        let Some(series) = self.diag_trace.series.get(&metric.name) else {
            painter.text(
                rect.left_top() + vec2(4.0, 2.0),
                egui::Align2::LEFT_TOP,
                format!("{}: no data", metric.label),
                font.clone(),
                theme::TEXT_MUTED,
            );
            return PanelInfo::empty(rect, unit);
        };
        let segments = transformed_segments(series, self.prefs.diag_plot.diff);
        let visible: Vec<(f64, f64)> = segments
            .iter()
            .flatten()
            .filter(|(t, _)| *t >= t_min)
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
            return PanelInfo::empty(rect, unit);
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
        // A silence in reception is a gap, not a measured straight line.
        for segment in &segments {
            let points: Vec<Pos2> = segment
                .iter()
                .filter(|(t, _)| *t >= t_min)
                .map(|(t, v)| egui::pos2(x_for(*t), y_for(*v)))
                .collect();
            painter.add(egui::Shape::line(
                points,
                Stroke::new(1.5, colour_for(&metric.name)),
            ));
        }

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
        PanelInfo {
            rect,
            unit,
            scale: Some(PanelScale::Time { v_min, v_max }),
        }
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
    ) -> PanelInfo {
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
        let unit = self.display_unit(metric);
        let Some(series) = self.diag_trace.series.get(&metric.name) else {
            note(format!("{}: no data", metric.label));
            return PanelInfo::empty(rect, unit);
        };
        let series = transformed_segments(series, self.prefs.diag_plot.diff)
            .pop()
            .unwrap_or_default();
        // The transform is bounded by both what has been collected and the
        // window the user chose, so a shorter window is also a coarser
        // spectrum — which is the trade the window control is making.
        let in_window = (self.prefs.diag_plot.window_ms as f64 / SAMPLE_INTERVAL_MS) as usize;
        let duration_samples = series
            .first()
            .zip(series.last())
            .map_or(0, |(first, last)| {
                ((last.0 - first.0) / SAMPLE_INTERVAL_MS) as usize + 1
            });
        let n = floor_pow2(duration_samples).min(floor_pow2(in_window));
        if n < FFT_MIN_N {
            note(format!(
                "{}: building FFT… (need ≥ {:.1}s)",
                metric.label,
                FFT_MIN_N as f64 * SAMPLE_INTERVAL_MS / 1000.0
            ));
            return PanelInfo::empty(rect, unit);
        }
        let Some(spectrum) = spectrum(&series, n) else {
            note(format!("{}: no data", metric.label));
            return PanelInfo::empty(rect, unit);
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
                unit,
                spectrum.bin_hz,
                n as f64 * SAMPLE_INTERVAL_MS / 1000.0,
            ),
            font.clone(),
            theme::TEXT,
        );
        PanelInfo {
            rect,
            unit,
            scale: Some(PanelScale::Freq(Box::new(spectrum))),
        }
    }

    /// The UI declares interest and copies new arrivals. Pause freezes only
    /// this view; the core continues to collect telemetry while minimized.
    fn sample_diag_trace(&mut self, open: bool) {
        interests::set_diagnostics_wanted(
            &self.host,
            open.then_some(self.prefs.diag_plot.rate_hz as f32),
        );
        let empty = BTreeSet::new();
        crate::host::diagnostics::select(
            &self.host,
            if open {
                &self.prefs.diag_plot.selected
            } else {
                &empty
            },
        );
        if !open {
            self.diag_trace = Default::default();
            return;
        }
        if !self.diag_paused {
            self.host.read().diagnostics.copy_to(&mut self.diag_trace);
        }
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

    #[test]
    fn reception_gaps_are_distinct_from_slow_changes() {
        let series: VecDeque<_> = (0..=400)
            .map(|i| (i as f64 * 20.0, (i / 100) as f64))
            .collect();
        let segments = transformed_segments(&series, true);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].len(), 4);
        assert!(uniform_resample(&segments[0], 64).is_some());
        let mut broken = series;
        broken.push_back((12_000.0, 9.0));
        let segments = transformed_segments(&broken, true);
        assert_eq!(segments.len(), 2);
        assert!(segments[1].is_empty()); // no derivative across the gap
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

    /// A rectangle drawn around one cycle of a ripple reads back as its period
    /// and its frequency; the height it spans is the value it crosses.
    #[test]
    fn a_time_selection_reports_a_period_and_an_amplitude() {
        let lines = time_lines(320.0, 1.25, " ms");
        assert_eq!(lines[0], "Δt 320.0 ms");
        assert_eq!(lines[1], "f  3.12 Hz");
        assert_eq!(lines[2], "Δv 1.250 ms");
        // A short interval keeps a digit more, and a large value one less.
        assert_eq!(time_lines(2.5, 250.0, "")[0], "Δt 2.50 ms");
        assert_eq!(time_lines(2.5, 250.0, "")[2], "Δv 250.0");
    }

    #[test]
    fn a_spectrum_selection_probes_a_bin_or_a_band() {
        let spectrum = Spectrum {
            db: vec![-90.0, -40.0, -6.0, -50.0, -30.0],
            amp: vec![1e-5, 1e-2, 0.5, 3e-3, 3e-2],
            peak_hz: 2.0,
            peak_db: -6.0,
            peak_amp: 0.5,
            bin_hz: 1.0,
        };
        // A drag of nothing is a probe of one bin.
        let point = freq_lines(&spectrum, 2.0, 2.0, " ms");
        assert_eq!(point[0], "f   2.000 Hz");
        assert_eq!(point[1], "|X| -6.0 dB");
        assert!(point[2].starts_with("amp 5.00e-1 ms"));
        // A band reports its ends, their difference, and the peak between
        // them — which is the spectrum's, not the rectangle's.
        let band = freq_lines(&spectrum, 1.0, 4.0, "");
        assert_eq!(band[0], "Δf   3.000 Hz");
        assert_eq!(band[1], "1.00 Hz -40.0 dB");
        assert_eq!(band[2], "4.00 Hz -30.0 dB");
        assert_eq!(band[3], "ΔdB  +10.0");
        assert!(band[4].starts_with("peak 2.00 Hz @ -6.0 dB"));
        // Bin zero is not a tone, so a band starting at it reports the peak
        // above it.
        let from_dc = freq_lines(&spectrum, 0.0, 1.0, "");
        assert!(from_dc[4].starts_with("peak 1.00 Hz"));
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
