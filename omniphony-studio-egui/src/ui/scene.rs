//! The adapter between the 3D view and egui.
//!
//! `view/` works in its own screen types — a point, a rectangle, unmultiplied
//! sRGB — and composes its overlay as flat shapes. This is where those become
//! egui, and the only place in the app that knows both vocabularies. Swapping
//! the toolkit rewrites this file and leaves the projection alone.

use crate::view::screen::{Color, ScreenPos, ScreenRect, Shape};

pub fn to_pos2(p: ScreenPos) -> egui::Pos2 {
    egui::pos2(p.x, p.y)
}

pub fn to_screen_pos(p: egui::Pos2) -> ScreenPos {
    ScreenPos::new(p.x, p.y)
}

pub fn to_rect(r: ScreenRect) -> egui::Rect {
    egui::Rect::from_min_max(to_pos2(r.min), to_pos2(r.max))
}

pub fn to_screen_rect(r: egui::Rect) -> ScreenRect {
    ScreenRect::from_min_max(to_screen_pos(r.min), to_screen_pos(r.max))
}

/// The view's colours are unmultiplied, which is the constructor egui wants:
/// `from_rgba_premultiplied` on the same bytes would darken every translucent
/// overlay.
pub fn to_color32(c: Color) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])
}

/// Put one of the view's shapes on screen.
pub fn paint_shape(painter: &egui::Painter, shape: Shape) {
    match shape {
        Shape::Rect {
            rect,
            radius,
            fill,
            stroke,
        } => {
            let rect = to_rect(rect);
            // egui takes the corner radius as a byte, so it is rounded and
            // clamped here rather than constraining what the view may ask for.
            let radius = egui::CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8);
            if let Some(fill) = fill {
                painter.rect_filled(rect, radius, to_color32(fill));
            }
            if let Some((width, color)) = stroke {
                // Inside: the stroke belongs to the rectangle's own extent, so
                // a gauge's outline cannot grow it past the hit area.
                painter.rect_stroke(
                    rect,
                    radius,
                    egui::Stroke::new(width, to_color32(color)),
                    egui::StrokeKind::Inside,
                );
            }
        }
        Shape::HLine { x, y, width, color } => {
            painter.hline(x.0..=x.1, y, egui::Stroke::new(width, to_color32(color)));
        }
    }
}
