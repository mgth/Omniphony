//! The custom-gradient editor (`scene/gradient-editor.js`).
//!
//! The heatmaps' "Custom" colormap is a short list of colour stops, and this is
//! how it is edited: a bar showing the gradient itself, a handle per stop, and a
//! colour well for the selected one. The bar *is* the control — reading a table
//! of positions and hex triplets tells you nothing about what the volume will
//! look like, and that is the only question being asked.

use egui::{Color32, Rect, Sense, Ui, vec2};

use crate::ui::theme;
use crate::view::volumes::GradientStop;

/// The shader carries eight stops; more would be silently dropped.
pub const MAX_STOPS: usize = 8;
/// Two stops are a gradient; one is a colour, so the last pair is kept.
const MIN_STOPS: usize = 2;
const BAR_HEIGHT: f32 = 18.0;
const HANDLE: f32 = 9.0;

/// Draw the editor. Returns true when the stops changed.
pub fn gradient_editor(
    ui: &mut Ui,
    stops: &mut Vec<GradientStop>,
    selected: &mut Option<usize>,
) -> bool {
    if stops.len() < MIN_STOPS {
        *stops = crate::view::volumes::default_stops();
    }
    let mut changed = false;
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width().min(240.0), BAR_HEIGHT + HANDLE),
        Sense::click_and_drag(),
    );
    let bar = Rect::from_min_size(rect.min, vec2(rect.width(), BAR_HEIGHT));
    paint_bar(ui, bar, stops);

    let pos_of = |x: f32| ((x - bar.left()) / bar.width().max(1.0)).clamp(0.0, 1.0);
    let x_of = |pos: f32| bar.left() + pos * bar.width();

    // Grab the nearest handle, or add a stop where there is none.
    if response.drag_started() || response.clicked() {
        if let Some(p) = response.interact_pointer_pos() {
            let near = stops
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    (x_of(a.pos) - p.x)
                        .abs()
                        .total_cmp(&(x_of(b.pos) - p.x).abs())
                })
                .filter(|(_, s)| (x_of(s.pos) - p.x).abs() <= HANDLE)
                .map(|(i, _)| i);
            match near {
                Some(index) => *selected = Some(index),
                None if response.clicked() && stops.len() < MAX_STOPS => {
                    // A new stop takes the colour already there, so adding one
                    // changes the shape and not the picture.
                    let pos = pos_of(p.x);
                    let rgb = sample(stops, pos);
                    stops.push(GradientStop { pos, rgb });
                    sort(stops, selected);
                    *selected = stops.iter().position(|s| s.pos == pos);
                    changed = true;
                }
                None => {}
            }
        }
    }
    if response.dragged()
        && let Some(index) = *selected
        && let Some(p) = response.interact_pointer_pos()
        && index < stops.len()
    {
        stops[index].pos = pos_of(p.x);
        sort(stops, selected);
        changed = true;
    }

    for (index, stop) in stops.iter().enumerate() {
        let centre = egui::pos2(x_of(stop.pos), bar.bottom() + HANDLE / 2.0);
        let on = *selected == Some(index);
        ui.painter().add(egui::Shape::convex_polygon(
            vec![
                centre + vec2(0.0, -HANDLE / 2.0),
                centre + vec2(HANDLE / 2.0, HANDLE / 2.0),
                centre + vec2(-HANDLE / 2.0, HANDLE / 2.0),
            ],
            colour_of(stop),
            egui::Stroke::new(
                1.0,
                if on {
                    theme::TEXT_STRONG
                } else {
                    theme::CONTROL_BORDER
                },
            ),
        ));
    }

    ui.horizontal(|ui| {
        let Some(index) = (*selected).filter(|i| *i < stops.len()) else {
            crate::ui::widgets::note(ui, "Click the bar to add a stop.");
            return;
        };
        let mut rgb = stops[index].rgb;
        // The theme sets `interact_size.x` to zero, which is right for rows
        // that size themselves from their content and starves any widget that
        // uses it as its own size. The colour well is one: without this it is
        // drawn zero pixels wide and there is nothing to click.
        ui.spacing_mut().interact_size.x = 24.0;
        if egui::color_picker::color_edit_button_rgb(ui, &mut rgb).changed() {
            stops[index].rgb = rgb;
            changed = true;
        }
        ui.label(
            egui::RichText::new(format!("{:.0}%", stops[index].pos * 100.0))
                .size(theme::FONT_SIZE_SMALL)
                .monospace()
                .color(theme::TEXT_MUTED),
        );
        // Removing the last pair would leave a colour rather than a gradient.
        let can_remove = stops.len() > MIN_STOPS;
        if ui
            .add_enabled(can_remove, egui::Button::new("✕").small())
            .on_hover_text(crate::i18n::t("common.close"))
            .clicked()
        {
            stops.remove(index);
            *selected = None;
            changed = true;
        }
        if ui.button("↺").on_hover_text("Reset").clicked() {
            *stops = crate::view::volumes::default_stops();
            *selected = None;
            changed = true;
        }
    });
    changed
}

/// The gradient itself, as one vertex-coloured strip per segment: the bar has
/// to show the interpolation the shader will do, not an approximation of it.
fn paint_bar(ui: &Ui, bar: Rect, stops: &[GradientStop]) {
    let mut mesh = egui::Mesh::default();
    let mut push = |x: f32, colour: Color32| {
        let base = mesh.vertices.len() as u32;
        mesh.colored_vertex(egui::pos2(x, bar.top()), colour);
        mesh.colored_vertex(egui::pos2(x, bar.bottom()), colour);
        if base >= 2 {
            mesh.add_triangle(base - 2, base - 1, base);
            mesh.add_triangle(base - 1, base, base + 1);
        }
    };
    // The ends are flat: the shader clamps outside the first and last stop.
    push(bar.left(), colour_of(&stops[0]));
    for stop in stops {
        push(bar.left() + stop.pos * bar.width(), colour_of(stop));
    }
    push(bar.right(), colour_of(&stops[stops.len() - 1]));
    ui.painter().add(egui::Shape::mesh(mesh));
    ui.painter().rect_stroke(
        bar,
        2.0,
        egui::Stroke::new(1.0, theme::CONTROL_BORDER),
        egui::StrokeKind::Inside,
    );
}

fn colour_of(stop: &GradientStop) -> Color32 {
    Color32::from_rgb(
        (stop.rgb[0].clamp(0.0, 1.0) * 255.0) as u8,
        (stop.rgb[1].clamp(0.0, 1.0) * 255.0) as u8,
        (stop.rgb[2].clamp(0.0, 1.0) * 255.0) as u8,
    )
}

/// The colour already at `pos`, so a new stop does not move the gradient.
fn sample(stops: &[GradientStop], pos: f32) -> [f32; 3] {
    if pos <= stops[0].pos {
        return stops[0].rgb;
    }
    for pair in stops.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if pos <= b.pos {
            let f = if b.pos > a.pos {
                (pos - a.pos) / (b.pos - a.pos)
            } else {
                0.0
            };
            return [
                a.rgb[0] + (b.rgb[0] - a.rgb[0]) * f,
                a.rgb[1] + (b.rgb[1] - a.rgb[1]) * f,
                a.rgb[2] + (b.rgb[2] - a.rgb[2]) * f,
            ];
        }
    }
    stops[stops.len() - 1].rgb
}

/// Keep the stops in position order, following the selection through the move.
fn sort(stops: &mut [GradientStop], selected: &mut Option<usize>) {
    let picked = selected.and_then(|i| stops.get(i).copied());
    stops.sort_by(|a, b| a.pos.total_cmp(&b.pos));
    if let Some(picked) = picked {
        *selected = stops.iter().position(|s| *s == picked);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stops() -> Vec<GradientStop> {
        crate::view::volumes::default_stops()
    }

    #[test]
    fn a_new_stop_takes_the_colour_already_there() {
        let s = stops();
        // Half way between blue and green.
        let mid = sample(&s, 0.25);
        assert!((mid[2] - 0.5).abs() < 1e-6 && (mid[1] - 0.5).abs() < 1e-6);
        // Outside the ends the gradient is flat, as the shader clamps it.
        assert_eq!(sample(&s, -1.0), s[0].rgb);
        assert_eq!(sample(&s, 2.0), s[2].rgb);
    }

    #[test]
    fn dragging_a_stop_past_another_keeps_the_selection_on_it() {
        let mut s = stops();
        let mut selected = Some(0);
        // Drag the first stop past the middle one.
        s[0].pos = 0.9;
        sort(&mut s, &mut selected);
        assert_eq!(selected, Some(1), "the selection followed the stop");
        assert_eq!(s[1].rgb, [0.0, 0.0, 1.0], "and it is still the blue one");
    }
}
