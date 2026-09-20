//! Reception-timed latency and rate traces, shared by the plot and tuning wizard.
use crate::app::StudioSpike;
use crate::ui::theme;
use egui::{Color32, Pos2, RichText, Stroke, Ui, vec2};
use std::collections::VecDeque;
const HEIGHT: f32 = 140.0;
const BG: Color32 = Color32::from_rgb(0x0f, 0x17, 0x24);
const LATENCY: Color32 = Color32::from_rgb(0x7a, 0xd7, 0xff);
const TARGET: Color32 = Color32::from_rgba_premultiplied(153, 128, 44, 153);
const RATE: Color32 = Color32::from_rgb(0xd6, 0xff, 0x8c);
const GRID: Color32 = Color32::from_rgba_premultiplied(18, 18, 18, 18);

impl StudioSpike {
    pub(crate) fn resample_plot(&mut self, ui: &mut Ui) {
        if ui
            .selectable_label(
                self.resample_plot_open,
                RichText::new("~").size(theme::FONT_SIZE),
            )
            .on_hover_text(crate::i18n::t("telemetry.plotToggle"))
            .clicked()
        {
            self.resample_plot_open = !self.resample_plot_open;
        }
        if !self.resample_plot_open {
            return;
        }
        self.poll_resample_sample(ui.ctx());
        let (rect, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), HEIGHT), egui::Sense::hover());
        self.resample_traces(
            &ui.painter().with_clip_rect(rect),
            rect,
            self.prefs.diag_plot.window_ms as f64,
        );
    }

    pub(crate) fn resample_traces(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        window_ms: f64,
    ) {
        painter.rect_filled(rect, 2.0, BG);
        let font = egui::FontId::proportional(theme::FONT_SIZE_SMALL);
        if self.resample_series.series.values().all(|s| s.len() < 2) {
            painter.text(
                rect.left_center() + vec2(6.0, 0.0),
                egui::Align2::LEFT_CENTER,
                crate::i18n::t("telemetry.waiting"),
                font,
                theme::TEXT_MUTED,
            );
            return;
        }
        let half = rect.height() / 2.0;
        let top = egui::Rect::from_min_size(rect.min, vec2(rect.width(), half));
        let bottom =
            egui::Rect::from_min_size(rect.min + vec2(0.0, half), vec2(rect.width(), half));
        painter.hline(rect.x_range(), bottom.top(), Stroke::new(1.0, GRID));
        // Target is a current configuration guide, never a fabricated sample.
        let target = self
            .host
            .read()
            .app
            .latency
            .latency_target_ms
            .map(|v| v as f64);
        for (key, area, colour, reference, unit) in [
            ("latency", top, LATENCY, target.map(|v| (v, TARGET)), "ms"),
            ("ppm", bottom, RATE, Some((0.0, GRID)), "ppm"),
        ] {
            if let Some(samples) = self.resample_series.series.get(key) {
                trace(
                    painter,
                    area,
                    samples,
                    self.resample_series.end_ms,
                    window_ms,
                    colour,
                    reference,
                    unit,
                    &font,
                );
            }
        }
    }

    /// Copy only new arrivals. Two consumers in one frame cannot duplicate data.
    pub(crate) fn poll_resample_sample(&mut self, _ctx: &egui::Context) {
        crate::host::diagnostics::select_resample(&self.host, self.resample_plot_open);
        self.host
            .read()
            .resampling
            .copy_to(&mut self.resample_series);
    }
}

#[allow(clippy::too_many_arguments)]
fn trace(
    painter: &egui::Painter,
    rect: egui::Rect,
    samples: &VecDeque<(f64, f64)>,
    end: f64,
    window: f64,
    colour: Color32,
    reference: Option<(f64, Color32)>,
    unit: &str,
    font: &egui::FontId,
) {
    let start = end - window;
    let visible = || samples.iter().filter(|(t, _)| *t >= start);
    let (mut lo, mut hi) = visible()
        .map(|(_, v)| *v)
        .chain(reference.map(|(v, _)| v))
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
            (lo.min(v), hi.max(v))
        });
    if !lo.is_finite() || !hi.is_finite() {
        return;
    }
    if hi - lo < 1e-9 {
        hi = lo + 1.0;
    }
    let pad = (hi - lo) * 0.1;
    lo -= pad;
    hi += pad;
    let point = |t: f64, v: f64| {
        Pos2::new(
            rect.left() + ((t - start) / window) as f32 * rect.width(),
            rect.top() + ((hi - v) / (hi - lo)) as f32 * (rect.height() - 4.0) + 2.0,
        )
    };
    if let Some((value, color)) = reference {
        painter.add(egui::Shape::dashed_line(
            &[point(start, value), point(end, value)],
            Stroke::new(1.0, color),
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
    let mut run = Vec::new();
    let mut previous = None;
    for &(t, v) in visible() {
        if previous.is_some_and(|last| separated(last, t)) {
            paint_run(painter, &mut run, colour);
        }
        run.push(point(t, v));
        previous = Some(t);
    }
    paint_run(painter, &mut run, colour);
}
fn separated(previous_ms: f64, at_ms: f64) -> bool {
    at_ms - previous_ms > 1000.0
}
fn paint_run(painter: &egui::Painter, points: &mut Vec<Pos2>, colour: Color32) {
    if points.len() > 1 {
        painter.add(egui::Shape::line(
            std::mem::take(points),
            Stroke::new(1.2, colour),
        ));
    } else {
        points.clear();
    }
}
