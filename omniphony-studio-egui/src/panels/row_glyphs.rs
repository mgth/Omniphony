//! The three graphics a speaker row carries besides its meter: the position
//! thumbnail, the crossover filter glyph and the band contribution bars
//! (`speakers.js:849–977`, `app.css:2370–2435`).
//!
//! They are small on purpose. A list of twenty speakers has to say *where* each
//! one is and *what band* it carries without becoming a table, so each answer is
//! a glyph you read at a glance and hover for the number.

use egui::{Color32, Rect, RichText, Stroke, Ui, vec2};

use crate::i18n::{t, tf};
use crate::ui::theme;

/// The frame and the marker of the position thumbnail, in glyph units.
const ICON: f32 = 16.0;
const MARKER: f32 = 3.2;
/// The filter glyph's own box.
const GLYPH_W: f32 = 16.0;
const GLYPH_H: f32 = 11.0;

const ICON_STROKE: Color32 = Color32::from_rgb(0x5d, 0x6b, 0x7d);
const GLYPH_ON: Color32 = Color32::from_rgb(0x8f, 0xb0, 0xd0);
const CUTOFF_LABEL: Color32 = Color32::from_rgb(0x9f, 0xb6, 0xcf);
const BAND_LABEL: Color32 = Color32::from_rgb(0x8a, 0x9a, 0xb0);

/// Which crossover shape a speaker's band limits describe.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Filter {
    Full,
    Low,
    High,
    Band,
}

impl Filter {
    pub fn of(freq_low: Option<f32>, freq_high: Option<f32>) -> Self {
        let low = freq_low.is_some_and(|f| f.is_finite() && f > 0.0);
        let high = freq_high.is_some_and(|f| f.is_finite() && f > 0.0);
        match (low, high) {
            (false, false) => Filter::Full,
            (false, true) => Filter::Low,
            (true, false) => Filter::High,
            (true, true) => Filter::Band,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Filter::Full => "speaker.filter.full",
            Filter::Low => "speaker.filter.low",
            Filter::High => "speaker.filter.high",
            Filter::Band => "speaker.filter.band",
        }
    }

    /// The polyline, in the glyph's own 16×11 box.
    fn path(self) -> &'static [(f32, f32)] {
        match self {
            Filter::Full => &[(1.0, 5.5), (15.0, 5.5)],
            Filter::Low => &[(1.0, 4.0), (8.5, 4.0), (14.0, 9.5)],
            Filter::High => &[(2.0, 9.5), (7.5, 4.0), (15.0, 4.0)],
            Filter::Band => &[(1.0, 9.5), (5.0, 4.0), (11.0, 4.0), (15.0, 9.5)],
        }
    }
}

/// `heightToColor`: blue at floor level and below, green half way up, red at
/// the ceiling. The hue is the only thing carrying height in a flat thumbnail.
pub fn height_colour(z: f64) -> Color32 {
    let hue = 240.0 * (1.0 - z.clamp(0.0, 1.0));
    hsl(hue as f32, 0.75, 0.52)
}

/// `bandColor(index, count)`: one blue for a single band, else red (lowest)
/// through blue (highest) — the same ramp as the 3D gauges.
pub fn band_colour(index: usize, count: usize) -> Color32 {
    if count <= 1 {
        return Color32::from_rgb(0x8e, 0xc8, 0xff);
    }
    let hue = 8.0 + 248.0 * index as f32 / (count - 1) as f32;
    hsl(hue.round(), 0.68, 0.56)
}

fn hsl(h_deg: f32, s: f32, l: f32) -> Color32 {
    let h = h_deg / 360.0;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h * 6.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h * 6.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    Color32::from_rgb(
        ((r + m) * 255.0).round() as u8,
        ((g + m) * 255.0).round() as u8,
        ((b + m) * 255.0).round() as u8,
    )
}

/// `formatCutoffHz`: 80 → "80", 1500 → "1.5k", 2000 → "2k".
pub fn format_cutoff_hz(hz: f64) -> String {
    if hz >= 1000.0 {
        let k = hz / 1000.0;
        if (k.fract()).abs() < 1e-9 {
            format!("{k:.0}k")
        } else {
            format!("{k:.1}k")
        }
    } else {
        format!("{}", hz.round() as i64)
    }
}

/// The plan view of one speaker: X left to right, Y rear to front with front
/// *up*, and the height in the marker's colour.
///
/// A non-spatialized feed is framed in black rather than grey: it sits outside
/// the room model altogether, and a thumbnail that looked like the others would
/// claim it is placed somewhere it is not.
pub fn position_icon(ui: &mut Ui, position: [f64; 3], spatialize: bool) {
    let (rect, response) = ui.allocate_exact_size(vec2(ICON, ICON), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter();
    let unit = rect.width() / ICON;
    let frame = Rect::from_min_max(
        rect.min + vec2(0.6 * unit, 0.6 * unit),
        rect.min + vec2(15.4 * unit, 15.4 * unit),
    );
    painter.rect_stroke(
        frame,
        1.2 * unit,
        Stroke::new(
            0.9 * unit,
            if spatialize {
                ICON_STROKE
            } else {
                Color32::BLACK
            },
        ),
        egui::StrokeKind::Inside,
    );
    let cx = 2.0 + ((position[0].clamp(-1.0, 1.0) + 1.0) / 2.0) * 12.0;
    let cy = 2.0 + ((1.0 - position[1].clamp(-1.0, 1.0)) / 2.0) * 12.0;
    let centre = rect.min + vec2(cx as f32 * unit, cy as f32 * unit);
    painter.rect_filled(
        Rect::from_center_size(centre, vec2(MARKER * unit, MARKER * unit)),
        0.5 * unit,
        height_colour(position[2]),
    );
    response.on_hover_text(format!(
        "X {:.2}  Y {:.2}  Z {:.2}",
        position[0], position[1], position[2]
    ));
}

/// The crossover shape, with its two cutoffs above and below it.
///
/// The labels are inverted relative to the editor's field order on purpose: the
/// top one is the *low-pass* edge, which is where the band stops, and the bottom
/// one is where it starts — reading the glyph top to bottom then matches
/// reading the frequency axis.
pub fn filter_icon(ui: &mut Ui, freq_low: Option<f32>, freq_high: Option<f32>) {
    let filter = Filter::of(freq_low, freq_high);
    let colour = if filter == Filter::Full {
        ICON_STROKE
    } else {
        GLYPH_ON
    };
    let response = ui
        .vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            cutoff_label(ui, freq_high);
            let (rect, _) = ui.allocate_exact_size(vec2(GLYPH_W, GLYPH_H), egui::Sense::hover());
            if ui.is_rect_visible(rect) {
                let points: Vec<egui::Pos2> = filter
                    .path()
                    .iter()
                    .map(|(x, y)| rect.min + vec2(*x, *y))
                    .collect();
                ui.painter()
                    .add(egui::Shape::line(points, Stroke::new(1.4, colour)));
            }
            cutoff_label(ui, freq_low);
        })
        .response;
    response.on_hover_text(t(filter.title()));
}

/// An absent cutoff reserves no height, so the glyph stays vertically centred
/// whatever is or is not configured.
fn cutoff_label(ui: &mut Ui, hz: Option<f32>) {
    let Some(hz) = hz.filter(|f| f.is_finite() && *f > 0.0) else {
        return;
    };
    ui.label(
        RichText::new(format_cutoff_hz(f64::from(hz)))
            .size(7.0)
            .color(CUTOFF_LABEL),
    );
}

/// `crossoverBandLabels`: "< 100 Hz", "1k–4k Hz", "≥ 4k Hz".
pub fn band_labels(cutoffs: &[f64]) -> Vec<String> {
    if cutoffs.is_empty() {
        return vec![t("heatmap.bandFull").to_owned()];
    }
    let mut labels = Vec::with_capacity(cutoffs.len() + 1);
    labels.push(format!("< {} Hz", format_cutoff_hz(cutoffs[0])));
    for pair in cutoffs.windows(2) {
        labels.push(format!(
            "{}–{} Hz",
            format_cutoff_hz(pair[0]),
            format_cutoff_hz(pair[1])
        ));
    }
    labels.push(format!(
        "≥ {} Hz",
        format_cutoff_hz(cutoffs[cutoffs.len() - 1])
    ));
    labels
}

/// One row per band: its name, how much of the selected object it carries, and
/// that as decibels.
///
/// Only drawn while an object is selected — the bars answer "where does *this*
/// object go", which is not a question a speaker has on its own.
pub fn band_bars(ui: &mut Ui, cutoffs: &[f64], gains: &[f64]) {
    if gains.is_empty() {
        return;
    }
    let labels = band_labels(cutoffs);
    let count = gains.len();
    for (index, gain) in gains.iter().enumerate() {
        ui.horizontal(|ui| {
            let label = labels
                .get(index)
                .cloned()
                .unwrap_or_else(|| tf("heatmap.bandIndex", &[("index", &index.to_string())]));
            ui.add_sized(
                vec2(52.0, 10.0),
                egui::Label::new(RichText::new(label).size(9.0).color(BAND_LABEL)).truncate(),
            );
            let (rect, _) = ui.allocate_exact_size(
                vec2((ui.available_width() - 44.0).max(24.0), 6.0),
                egui::Sense::hover(),
            );
            let painter = ui.painter();
            painter.rect_filled(rect, 3.0, theme::FILL);
            let level = (gain * 100.0).min(100.0).max(0.0) as f32 / 100.0;
            if level > 0.0 {
                let mut fill = rect;
                fill.set_width(rect.width() * level);
                painter.rect_filled(fill, 3.0, band_colour(index, count));
            }
            ui.add_sized(
                vec2(40.0, 10.0),
                egui::Label::new(
                    RichText::new(crate::panels::audio::format_linear_as_db(Some(*gain)))
                        .size(9.0)
                        .monospace()
                        .color(BAND_LABEL),
                ),
            );
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_glyph_says_which_of_the_four_shapes_the_band_limits_describe() {
        assert_eq!(Filter::of(None, None), Filter::Full);
        assert_eq!(Filter::of(None, Some(120.0)), Filter::Low);
        assert_eq!(Filter::of(Some(80.0), None), Filter::High);
        assert_eq!(Filter::of(Some(80.0), Some(4000.0)), Filter::Band);
        // A zero or a NaN is not a band edge, it is an empty field.
        assert_eq!(Filter::of(Some(0.0), Some(f32::NAN)), Filter::Full);
    }

    #[test]
    fn cutoffs_are_written_the_way_a_crossover_chart_writes_them() {
        assert_eq!(format_cutoff_hz(80.0), "80");
        assert_eq!(format_cutoff_hz(1500.0), "1.5k");
        assert_eq!(format_cutoff_hz(2000.0), "2k");
        assert_eq!(format_cutoff_hz(119.6), "120");
    }

    #[test]
    fn the_band_labels_bound_every_band_including_the_open_ended_ones() {
        assert_eq!(band_labels(&[]), vec![t("heatmap.bandFull").to_owned()]);
        assert_eq!(
            band_labels(&[100.0, 4000.0]),
            vec!["< 100 Hz", "100–4k Hz", "≥ 4k Hz"]
        );
    }

    #[test]
    fn height_and_band_colours_run_the_way_the_scene_draws_them() {
        // Floor is blue, ceiling is red — the thumbnail's only height cue.
        assert!(height_colour(0.0).b() > height_colour(0.0).r());
        assert!(height_colour(1.0).r() > height_colour(1.0).b());
        // A single band gets the flat blue rather than an end of the ramp.
        assert_eq!(band_colour(0, 1), Color32::from_rgb(0x8e, 0xc8, 0xff));
        assert!(band_colour(0, 3).r() > band_colour(2, 3).r());
    }
}
