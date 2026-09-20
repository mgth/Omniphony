//! One floating side overlay: the frame, the collapse button and the drag
//! handle of `src/ui/side-panels.js`.
//!
//! The overlay floats above the viewport and never takes space from it, so
//! resizing or collapsing a panel cannot move the scene.

use egui::{Align2, Color32, Context, CornerRadius, Id, Sense, Stroke, Ui, vec2};

use super::layout::{OverlayLayout, Side};
use super::theme;

/// Height of the visual grip inside the drag handle, and its hover height.
const GRIP_HEIGHT: f32 = 36.0;
const GRIP_HEIGHT_HOVER: f32 = 56.0;
const HANDLE_WIDTH: f32 = 10.0;
const COLLAPSE_BUTTON: f32 = 28.8;

/// Draw one overlay. `body` fills the panel below its header row.
pub fn show(ctx: &Context, side: Side, layout: &mut OverlayLayout, body: impl FnOnce(&mut Ui)) {
    let screen = ctx.content_rect();
    let margin = theme::PANEL_EDGE_MARGIN;
    let collapsed = layout.side(side).collapsed;
    let anchor = match side {
        Side::Left => Align2::LEFT_TOP,
        Side::Right => Align2::RIGHT_TOP,
    };
    let offset = match side {
        Side::Left => [margin, margin],
        Side::Right => [-margin, margin],
    };
    let id = match side {
        Side::Left => "overlay-left",
        Side::Right => "overlay-right",
    };

    let area = egui::Area::new(Id::new(id))
        .anchor(anchor, offset)
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            if collapsed {
                collapse_button(ui, side, layout, screen.width());
                return;
            }
            let width = layout.side(side).width;
            let height = screen.height() - 2.0 * margin;
            // Fixed extents. The overlay takes exactly its configured size, and
            // its content is laid out in a child placed on that rect — a child
            // does not allocate in the area — clipped to it. A row wider than
            // the panel is then cut inside the panel instead of growing its
            // frame; grown, the frame made every section that did respect the
            // width look narrower than the panel, and dragging the edge past
            // the minimum made the content keep shrinking while the panel had
            // stopped. The project's rule for overlays: fixed extents, internal
            // overflow.
            let (rect, _) = ui.allocate_exact_size(
                vec2(width + super::layout::PANEL_CHROME, height),
                egui::Sense::hover(),
            );
            let mut panel = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(rect)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            panel.set_clip_rect(rect.intersect(ui.clip_rect()));
            let frame = theme::panel_frame();
            // The frame's height is its content's plus its padding *and* its
            // border: leaving the border out made the frame two points taller
            // than the rect it is clipped to, and cut its bottom edge off.
            // (The width already counts it, in `PANEL_CHROME`.)
            let chrome = frame.total_margin().sum();
            frame.show(&mut panel, |ui| {
                ui.set_width(width);
                ui.set_height(height - chrome.y);
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        if side == Side::Right {
                            collapse_button(ui, side, layout, screen.width());
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if side == Side::Left {
                                collapse_button(ui, side, layout, screen.width());
                            }
                        });
                    });
                    body(ui);
                });
            });
        });

    if !collapsed {
        drag_handle(ctx, side, layout, area.response.rect, screen.width());
    }
}

/// The hamburger / speaker button that folds the panel to a strip.
fn collapse_button(ui: &mut Ui, side: Side, layout: &mut OverlayLayout, viewport_width: f32) {
    let (rect, response) =
        ui.allocate_exact_size(vec2(COLLAPSE_BUTTON, COLLAPSE_BUTTON), Sense::click());
    let hovered = response.hovered();
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(theme::CONTROL_RADIUS),
        if hovered {
            theme::FILL_HOVER
        } else {
            theme::FILL
        },
        Stroke::new(1.0, theme::CONTROL_BORDER),
        egui::StrokeKind::Inside,
    );
    let colour = if hovered {
        theme::TEXT_STRONG
    } else {
        theme::TEXT
    };
    match side {
        Side::Left => paint_hamburger(painter, rect, colour),
        Side::Right => paint_speaker(painter, rect, colour),
    }
    if response.clicked() {
        layout.toggle_collapsed(side, viewport_width);
    }
    let tooltip = match side {
        Side::Left => "Toggle controls",
        Side::Right => "Toggle speakers",
    };
    response.on_hover_text(tooltip);
}

fn paint_hamburger(painter: &egui::Painter, rect: egui::Rect, colour: Color32) {
    let c = rect.center();
    let half = 7.0;
    for dy in [-5.0, 0.0, 5.0] {
        painter.hline(
            (c.x - half)..=(c.x + half),
            c.y + dy,
            Stroke::new(1.8, colour),
        );
    }
}

fn paint_speaker(painter: &egui::Painter, rect: egui::Rect, colour: Color32) {
    let c = rect.center();
    let body = egui::Rect::from_center_size(egui::pos2(c.x - 4.0, c.y), vec2(4.0, 7.0));
    painter.rect_filled(body, 1.0, colour);
    painter.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(c.x - 2.0, c.y - 3.5),
            egui::pos2(c.x + 2.0, c.y - 7.0),
            egui::pos2(c.x + 2.0, c.y + 7.0),
            egui::pos2(c.x - 2.0, c.y + 3.5),
        ],
        colour,
        Stroke::NONE,
    ));
    for (r, w) in [(5.0, 1.0), (8.0, 1.0)] {
        painter.circle_stroke(
            egui::pos2(c.x + 2.0, c.y),
            r,
            Stroke::new(w, colour.gamma_multiply(0.8)),
        );
    }
}

/// The 10 px strip on the panel's inner edge: drag to resize, double-click to
/// go back to the default width.
fn drag_handle(
    ctx: &Context,
    side: Side,
    layout: &mut OverlayLayout,
    panel: egui::Rect,
    viewport_width: f32,
) {
    let x = match side {
        Side::Left => panel.right(),
        Side::Right => panel.left(),
    };
    let id = match side {
        Side::Left => "overlay-handle-left",
        Side::Right => "overlay-handle-right",
    };
    egui::Area::new(Id::new(id))
        .order(egui::Order::Foreground)
        .fixed_pos(egui::pos2(x - HANDLE_WIDTH * 0.5, panel.top()))
        .show(ctx, |ui| {
            let (rect, response) =
                ui.allocate_exact_size(vec2(HANDLE_WIDTH, panel.height()), Sense::click_and_drag());
            let active = response.dragged();
            if active {
                let direction = match side {
                    Side::Left => 1.0,
                    Side::Right => -1.0,
                };
                let width = layout.side(side).width + direction * response.drag_delta().x;
                layout.set_width(side, width, viewport_width);
            }
            if response.double_clicked() {
                layout.set_width(side, super::layout::DEFAULT_WIDTH, viewport_width);
            }
            let hot = response.hovered() || active;
            if hot {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
            let (height, colour) = if hot {
                (
                    GRIP_HEIGHT_HOVER,
                    Color32::from_rgba_unmultiplied(120, 200, 255, 179),
                )
            } else {
                (
                    GRIP_HEIGHT,
                    Color32::from_rgba_unmultiplied(255, 255, 255, 46),
                )
            };
            let grip = egui::Rect::from_center_size(rect.center(), vec2(2.0, height));
            ui.painter().rect_filled(grip, 1.0, colour);
        });
}

/// The web's pinned editor slot (`#speakerEditSection`, `#channelEditSection`,
/// `#objectTestEditSection`): a temporary editing panel at the foot of an
/// overlay, below its scroll, so it stays in view while the list above it
/// keeps scrolling. At most half the window high, scrolling inside itself
/// past that so a tall editor never starves the list. The web capped it at
/// `max-height: 45vh`; the speaker editor's Edit tab, laid out in groups,
/// stands just about that tall on a 1080-line screen, and an editor that
/// scrolls inside its slot loses its own header first.
///
/// It is carved out of the overlay with a bottom panel, and the list's
/// `CentralPanel` — called after this — takes what it leaves, so opening an
/// editor shortens the list rather than pushing the editor off the foot of a
/// fixed-height overlay.
///
/// The panel is sized from the height egui measured for the editor's content
/// on the previous frame. Left to size itself it never grows: the editor sits
/// in a scroll area, a scroll area is bounded by the room it is given, and the
/// room it is given is the panel's own initial sliver — so the content never
/// overflows and the panel never learns it should be taller. The first frame
/// opens at the full half and settles on the content from the next.
pub fn pinned_slot(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    let max = ui.ctx().content_rect().height() * 0.5;
    let key = Id::new((id, "content-height"));
    let wanted: f32 = ui.data(|d| d.get_temp(key)).unwrap_or(max);
    egui::Panel::bottom(Id::new(id))
        .frame(egui::Frame::NONE)
        .show_separator_line(false)
        .exact_size(wanted.clamp(0.0, max))
        .show_inside(ui, |ui| {
            let measured = panel_scroll(ui, (id, "scroll"), None, add).y;
            if (measured - wanted).abs() > 0.5 {
                ui.data_mut(|d| d.insert_temp(key, measured));
                ui.ctx().request_repaint();
            }
        });
}

/// How far into the panel's right padding a panel's scroll area reaches: its
/// scroll bar lives there, beside the content rather than over it.
const SCROLL_GUTTER: f32 = 12.0;

/// A panel's vertical scroll, with its bar in the panel's right padding.
///
/// The bar floats — hidden until the pointer nears it, as the web's overlay
/// scrollbars are — but egui draws a floating bar over the right edge of the
/// content, which here is where the switches are: reaching for one woke the
/// bar, and the bar sat on top of the switch. So the scroll area is laid out
/// `SCROLL_GUTTER` wider than the content, into the padding, and its content
/// is held to the original width: the bar and the band that wakes it are
/// both in the padding, the content keeps every point of its width, and the
/// panel does not grow. The clip is widened with it, or egui would pull the
/// bar back inside the clip, onto the content.
///
/// Returns the size of the content, for a caller sizing itself on it.
pub fn panel_scroll(
    ui: &mut Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    max_height: Option<f32>,
    add: impl FnOnce(&mut Ui),
) -> egui::Vec2 {
    let rect = ui.available_rect_before_wrap();
    let content_width = rect.width();
    let outer = rect.with_max_x(rect.max.x + SCROLL_GUTTER);
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(outer)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    let clip = ui.clip_rect();
    child.set_clip_rect(clip.with_max_x(clip.max.x.max(outer.max.x)));
    let mut area = egui::ScrollArea::vertical()
        .id_salt(id_salt)
        .auto_shrink([false, false]);
    if let Some(height) = max_height {
        area = area.max_height(height);
    }
    let out = area.show(&mut child, |ui| {
        ui.set_max_width(content_width);
        add(ui);
    });
    // The child took no room in the parent; the scroll area's footprint
    // within the content width does.
    ui.allocate_rect(
        rect.intersect(out.inner_rect.with_max_x(rect.max.x)),
        Sense::hover(),
    );
    out.content_size
}
