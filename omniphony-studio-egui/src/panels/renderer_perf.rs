//! The renderer's performance gauges (`#rendererPerfWrap`,
//! `latency.js:415–576`).
//!
//! One bar for the whole frame, split into the four stages end to end, plus a
//! readout per stage. It answers one question — is the renderer keeping up, and
//! which stage is spending the time — and it answers it against the frame
//! budget rather than in the abstract, which is why the bar's scale is the
//! frame duration whenever the renderer reports one.
//!
//! Shown only while metering is on: with metering off the renderer stops
//! sending the timings, and a gauge frozen on its last values is worse than no
//! gauge.

use egui::{Color32, RichText, Ui, vec2};

use crate::app::StudioSpike;
use crate::i18n::tf;
use crate::osc::dispatch::Stage;
use crate::ui::theme;

/// The four segment colours, in the order the bar draws them.
const SEGMENT: [Color32; 4] = [
    Color32::from_rgb(0x8c, 0xd6, 0xff),
    Color32::from_rgb(0xff, 0xd6, 0x78),
    Color32::from_rgb(0x70, 0xaa, 0xff),
    Color32::from_rgb(0xb4, 0xff, 0xb8),
];
/// The cumulative worst-case markers.
const MARKER: [Color32; 4] = [
    Color32::from_rgb(0xff, 0xd5, 0x4a),
    Color32::from_rgb(0xff, 0xeb, 0x8a),
    Color32::from_rgb(0xff, 0xb8, 0x4a),
    Color32::from_rgb(0xff, 0x8b, 0x4a),
];
const BAR_HEIGHT: f32 = 8.0;

/// What one stage contributes, now and over the two windows.
#[derive(Clone, Copy, Default)]
struct StageTime {
    now: f64,
    mean: Option<f64>,
    max: Option<f64>,
}

/// The whole gauge's numbers, already untangled. Taken once per frame by
/// [`StudioSpike::perf_snapshot`] and drawn twice: the bar in the section's
/// header, the readouts at the top of its body.
#[derive(Default, Clone, Copy)]
pub(crate) struct Perf {
    stages: [StageTime; 4],
    frame: Option<f64>,
}

impl Perf {
    /// The bar's full-scale value: the frame budget when the renderer reports
    /// one, otherwise whatever the numbers themselves need.
    fn scale_ms(&self) -> f64 {
        if let Some(frame) = self.frame.filter(|f| f.is_finite() && *f > 0.0) {
            return frame;
        }
        let now: f64 = self.stages.iter().map(|s| s.now).sum();
        let max: f64 = self.stages.iter().map(|s| s.max.unwrap_or(0.0)).sum();
        now.max(max).max(0.01)
    }
}

impl StudioSpike {
    /// The gauge's numbers, or `None` while metering is off: with it off the
    /// renderer stops sending the timings, and a gauge frozen on its last
    /// values is worse than no gauge.
    pub(crate) fn perf_snapshot(&self) -> Option<Perf> {
        let metering = {
            let live = self.host.read();
            live.app.osc_metering_enabled.unwrap_or(0) != 0
        };
        metering.then(|| self.collect_perf())
    }

    /// Crossover time is *contained* in render time, so it is carved out of it
    /// rather than added: the four segments must sum to the frame's real cost.
    fn collect_perf(&self) -> Perf {
        let live = self.host.read();
        let positive = |v: Option<f64>| v.unwrap_or(0.0).max(0.0);
        let decode = positive(live.app.decode_time_ms);
        let render_total = positive(live.app.render_time_ms);
        let crossover = positive(live.app.crossover_time_ms).min(render_total);
        let write = positive(live.app.write_time_ms);

        let stat = |stage: Stage| {
            let (avg, max) = live.stage_stats(stage);
            (avg.map(|s| s.mean), max.map(|s| s.max))
        };
        let (decode_mean, decode_max) = stat(Stage::Decode);
        let (render_mean_total, render_max_total) = stat(Stage::Render);
        let (crossover_mean_raw, crossover_max_raw) = stat(Stage::Crossover);
        let (write_mean, write_max) = stat(Stage::Write);
        // The same containment rule applies to both windows.
        let carve = |total: Option<f64>, part: Option<f64>| match (total, part) {
            (Some(total), Some(part)) => {
                let part = part.min(total);
                (Some(part), Some(total - part))
            }
            (total, part) => (part, total),
        };
        let (crossover_mean, render_mean) = carve(render_mean_total, crossover_mean_raw);
        let (crossover_max, render_max) = carve(render_max_total, crossover_max_raw);

        Perf {
            stages: [
                StageTime {
                    now: decode,
                    mean: decode_mean,
                    max: decode_max,
                },
                StageTime {
                    now: crossover,
                    mean: crossover_mean,
                    max: crossover_max,
                },
                StageTime {
                    now: render_total - crossover,
                    mean: render_mean,
                    max: render_max,
                },
                StageTime {
                    now: write,
                    mean: write_mean,
                    max: write_max,
                },
            ],
            frame: live.app.frame_duration_ms,
        }
    }
}

/// The bar, in the section's header (`#rendererPerfWrap`'s `.meter-bar`): the
/// four stages end to end against the frame budget, with the cumulative
/// worst cases as markers, so a spike that has already passed is still
/// visible where it would have landed. The readouts are its tooltip, so the
/// numbers are one hover away with the section folded.
pub(crate) fn perf_bar(ui: &mut Ui, perf: &Perf) {
    let width = (ui.available_width() * 0.45).clamp(60.0, 180.0);
    let (rect, response) = ui.allocate_exact_size(vec2(width, BAR_HEIGHT), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, theme::FILL);
    let scale = perf.scale_ms();
    let x_for = |ms: f64| rect.left() + ((ms / scale).clamp(0.0, 1.0) as f32) * rect.width();
    let mut start = 0.0;
    for (index, stage) in perf.stages.iter().enumerate() {
        let end = start + stage.now;
        if stage.now > 0.0 {
            let segment = egui::Rect::from_x_y_ranges(x_for(start)..=x_for(end), rect.y_range());
            painter.rect_filled(segment, 0.0, SEGMENT[index]);
        }
        start = end;
    }
    let mut cumulative = 0.0;
    for (index, stage) in perf.stages.iter().enumerate() {
        let Some(max) = stage.max else { continue };
        cumulative += max;
        painter.vline(
            x_for(cumulative),
            rect.y_range(),
            egui::Stroke::new(1.5, MARKER[index]),
        );
    }
    response.on_hover_text(readout_lines(perf).join("\n"));
}

/// The readouts under the bar: the one-second average of each stage, not the
/// instantaneous value — at frame rate the latter is unreadable, and the
/// question is what the stage costs, not what it cost once — then the worst
/// cases.
pub(crate) fn perf_readouts(ui: &mut Ui, perf: &Perf) {
    let [means, maxes] = readout_lines(perf);
    ui.label(
        RichText::new(means)
            .size(theme::FONT_SIZE_SMALL)
            .color(theme::TEXT),
    );
    ui.label(
        RichText::new(maxes)
            .size(theme::FONT_SIZE_SMALL)
            .color(theme::TEXT_DIM),
    );
}

/// The two readout lines: the averages with the frame budget, then the maxima.
fn readout_lines(perf: &Perf) -> [String; 2] {
    let scale = perf.frame.filter(|f| f.is_finite() && *f > 0.0);
    let means = [
        "renderer.perf.decode",
        "renderer.perf.crossover",
        "renderer.perf.render",
        "renderer.perf.write",
    ]
    .into_iter()
    .enumerate()
    .map(|(index, key)| {
        tf(
            key,
            &[("value", &ms_with_pct(perf.stages[index].mean, scale))],
        )
    })
    .chain(std::iter::once(tf(
        "renderer.perf.frame",
        &[("value", &ms_or_dash(perf.frame))],
    )))
    .collect::<Vec<_>>()
    .join(" · ");
    let maxes = (0..4)
        .map(|index| {
            tf(
                "renderer.perf.max",
                &[("value", &ms_with_pct(perf.stages[index].max, scale))],
            )
        })
        .collect::<Vec<_>>()
        .join(" · ");
    [means, maxes]
}

/// `msWithPct`: milliseconds, and what fraction of the frame budget that is.
fn ms_with_pct(ms: Option<f64>, frame: Option<f64>) -> String {
    let Some(ms) = ms.filter(|v| v.is_finite()) else {
        return "—".to_owned();
    };
    let base = format!("{ms:.3} ms");
    let Some(frame) = frame.filter(|f| *f > 0.0) else {
        return base;
    };
    let pct = ms / frame * 100.0;
    if pct >= 10.0 {
        format!("{base} ({pct:.0}%)")
    } else {
        format!("{base} ({pct:.1}%)")
    }
}

fn ms_or_dash(ms: Option<f64>) -> String {
    match ms.filter(|v| v.is_finite()) {
        Some(ms) => format!("{ms:.3} ms"),
        None => "—".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bar_falls_back_to_its_own_numbers_when_no_frame_budget_is_known() {
        let mut perf = Perf::default();
        perf.stages[0].now = 2.0;
        perf.stages[2].now = 3.0;
        assert_eq!(perf.scale_ms(), 5.0);
        // A worst case larger than the current frame still has to fit.
        perf.stages[2].max = Some(20.0);
        assert_eq!(perf.scale_ms(), 20.0);
        // A reported budget wins over both: the question is what fraction of
        // the frame is spent, not what fraction of the peak.
        perf.frame = Some(10.0);
        assert_eq!(perf.scale_ms(), 10.0);
        // Nothing at all still gives a usable scale rather than a division by
        // zero.
        assert_eq!(Perf::default().scale_ms(), 0.01);
    }

    #[test]
    fn a_stage_is_shown_against_the_frame_budget_when_there_is_one() {
        assert_eq!(ms_with_pct(Some(1.25), Some(10.0)), "1.250 ms (12%)");
        assert_eq!(ms_with_pct(Some(0.5), Some(10.0)), "0.500 ms (5.0%)");
        assert_eq!(ms_with_pct(Some(0.5), None), "0.500 ms");
        assert_eq!(ms_with_pct(None, Some(10.0)), "—");
        assert_eq!(ms_with_pct(Some(f64::NAN), None), "—");
        assert_eq!(ms_or_dash(None), "—");
    }
}
