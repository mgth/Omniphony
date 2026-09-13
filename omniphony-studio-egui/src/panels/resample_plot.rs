//! The resample sparkline (`#resamplePlotContainer`, `controls/resample-plot.js`
//! over `auto-tune/sparkline.js`).
//!
//! Two traces stacked in one canvas: the smoothed latency against its target,
//! and the rate adjustment the resampler is applying. They belong together
//! because they are cause and effect — the controller pulls the rate to move
//! the latency — and reading one without the other says nothing about whether
//! the loop is behaving.

use std::collections::VecDeque;

use egui::{Color32, Pos2, RichText, Stroke, Ui, vec2};

use crate::app::StudioSpike;
use crate::ui::theme;

/// `#resamplePlotCanvas` is 600×140; the height is what matters here.
const HEIGHT: f32 = 140.0;
/// The longest window the picker offers, so switching to it does not start
/// from an empty buffer.
const RETAIN_MS: f64 = 60_000.0;

const BG: Color32 = Color32::from_rgb(0x0f, 0x17, 0x24);
const LATENCY: Color32 = Color32::from_rgb(0x7a, 0xd7, 0xff);
const TARGET: Color32 = Color32::from_rgba_premultiplied(153, 128, 44, 153);
const RATE: Color32 = Color32::from_rgb(0xd6, 0xff, 0x8c);
const GRID: Color32 = Color32::from_rgba_premultiplied(18, 18, 18, 18);

/// One polled sample. Any of the three can be missing, and a gap in a trace is
/// the truth rather than a straight line across it.
#[derive(Clone, Copy)]
pub struct Sample {
    pub t: f64,
    pub latency: Option<f64>,
    pub target: Option<f64>,
    /// `(ratio - 1) × 1e6`: parts per million, which is the unit the
    /// controller's own limits are written in.
    pub ppm: Option<f64>,
}

pub type ResampleSeries = VecDeque<Sample>;

impl StudioSpike {
    /// The toggle, then the plot when it is on.
    pub(crate) fn resample_plot(&mut self, ui: &mut Ui) {
        let open = self.resample_plot_open;
        if ui
            .selectable_label(open, RichText::new("~").size(theme::FONT_SIZE))
            .on_hover_text(crate::i18n::t("telemetry.plotToggle"))
            .clicked()
        {
            self.resample_plot_open = !open;
            if open {
                // Closing drops the history: a plot reopened ten minutes later
                // showing a stale window would be read as current.
                self.resample_series.clear();
            }
        }
        if !self.resample_plot_open {
            return;
        }
        self.poll_resample_sample(ui.ctx());
        let window_ms = self.prefs.diag_plot.window_ms as f64;
        let (rect, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), HEIGHT), egui::Sense::hover());
        let painter = ui.painter().with_clip_rect(rect);
        self.resample_traces(&painter, rect, window_ms);
    }

    /// The two traces on one rectangle. The auto-tune wizard draws the same
    /// picture beside its own controls: the run is the controller being
    /// pulled, and this is what that looks like.
    pub(crate) fn resample_traces(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        window_ms: f64,
    ) {
        painter.rect_filled(rect, 2.0, BG);
        let font = egui::FontId::proportional(theme::FONT_SIZE_SMALL);
        if self.resample_series.len() < 2 {
            painter.text(
                rect.left_center() + vec2(6.0, 0.0),
                egui::Align2::LEFT_CENTER,
                "Waiting for telemetry…",
                font,
                theme::TEXT_MUTED,
            );
            return;
        }
        let t_max = self.resample_series.back().map_or(0.0, |s| s.t);
        let t_min = t_max - window_ms;
        let half = rect.height() / 2.0;
        let top = egui::Rect::from_min_size(rect.min, vec2(rect.width(), half));
        let bottom =
            egui::Rect::from_min_size(rect.min + vec2(0.0, half), vec2(rect.width(), half));
        painter.hline(rect.x_range(), bottom.top(), Stroke::new(1.0, GRID));

        // The latency half, with the target it is being held at.
        self.trace(
            &painter,
            top,
            t_min,
            t_max,
            LATENCY,
            |s| s.latency,
            Some(("ms", TARGET, |s: &Sample| s.target)),
            &font,
        );
        // The rate half. Zero is the line worth seeing: the controller at rest.
        self.trace(
            &painter,
            bottom,
            t_min,
            t_max,
            RATE,
            |s| s.ppm,
            Some(("ppm", GRID, |_: &Sample| Some(0.0))),
            &font,
        );
    }

    /// One trace, autoscaled over the visible window, with an optional
    /// reference line drawn on the same scale.
    #[allow(clippy::too_many_arguments)]
    fn trace(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        t_min: f64,
        t_max: f64,
        colour: Color32,
        value: impl Fn(&Sample) -> Option<f64>,
        reference: Option<(&str, Color32, fn(&Sample) -> Option<f64>)>,
        font: &egui::FontId,
    ) {
        let visible: Vec<&Sample> = self
            .resample_series
            .iter()
            .filter(|s| s.t >= t_min)
            .collect();
        let mut values: Vec<f64> = visible.iter().filter_map(|s| value(s)).collect();
        if let Some((_, _, reference)) = reference {
            values.extend(visible.iter().filter_map(|s| reference(s)));
        }
        if values.is_empty() {
            return;
        }
        let (mut lo, mut hi) = values
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
                (lo.min(*v), hi.max(*v))
            });
        if hi - lo < 1e-9 {
            hi = lo + 1.0;
        }
        let pad = (hi - lo) * 0.1;
        lo -= pad;
        hi += pad;
        let point = |t: f64, v: f64| {
            Pos2::new(
                rect.left() + (((t - t_min) / (t_max - t_min)) as f32) * rect.width(),
                rect.top() + (((hi - v) / (hi - lo)) as f32) * (rect.height() - 4.0) + 2.0,
            )
        };
        if let Some((unit, reference_colour, reference)) = reference {
            let line: Vec<Pos2> = visible
                .iter()
                .filter_map(|s| reference(s).map(|v| point(s.t, v)))
                .collect();
            if line.len() > 1 {
                painter.add(egui::Shape::dashed_line(
                    &line,
                    Stroke::new(1.0, reference_colour),
                    4.0,
                    3.0,
                ));
            }
            painter.text(
                rect.right_top() + vec2(-4.0, 2.0),
                egui::Align2::RIGHT_TOP,
                format!("{lo:.2}…{hi:.2} {unit}"),
                font.clone(),
                theme::TEXT_MUTED,
            );
        }
        // A missing sample breaks the line rather than being bridged: the gap
        // is what happened.
        let mut run: Vec<Pos2> = Vec::new();
        for sample in &visible {
            match value(sample) {
                Some(v) => run.push(point(sample.t, v)),
                None => {
                    if run.len() > 1 {
                        painter.add(egui::Shape::line(
                            std::mem::take(&mut run),
                            Stroke::new(1.2, colour),
                        ));
                    } else {
                        run.clear();
                    }
                }
            }
        }
        if run.len() > 1 {
            painter.add(egui::Shape::line(run, Stroke::new(1.2, colour)));
        }
    }

    /// Poll the three values the plot draws, and keep the window repainting.
    pub(crate) fn poll_resample_sample(&mut self, ctx: &egui::Context) {
        ctx.request_repaint();
        let t = self.diag_started.elapsed().as_secs_f64() * 1000.0;
        let sample = {
            let live = self.host.read();
            Sample {
                t,
                latency: live
                    .app
                    .latency
                    .latency_smoothed_ms
                    .filter(|v| v.is_finite()),
                target: live.app.latency.latency_target_ms.map(|v| v as f64),
                ppm: live
                    .app
                    .resample_ratio
                    .filter(|v| v.is_finite())
                    .map(|ratio| (ratio - 1.0) * 1e6),
            }
        };
        self.resample_series.push_back(sample);
        while self
            .resample_series
            .front()
            .is_some_and(|s| s.t < t - RETAIN_MS)
        {
            self.resample_series.pop_front();
        }
    }
}
