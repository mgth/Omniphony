//! The source bounds section: a box the renderer hears sources in, edited on a
//! plan and a side view of the room. While it is on, every source outside the
//! box — an object at its position, a bed channel at its speaker's — is muted
//! by the renderer (`/control/source_bounds`). Transient, like a mute: the
//! renderer starts with it off and no Save is for it.
//!
//! The two views share one scale, as the object test's sheet does, so the depth
//! of the plan is the same length as the depth of the side view and the room
//! keeps its proportions. Each edge of the box is dragged on its own; inside the
//! box, a drag moves it whole.

use egui::{Color32, CursorIcon, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, vec2};

use crate::app::StudioSpike;
use crate::host::commands::gain;
use crate::i18n::{t, tf};
use crate::model::app_state::{RoomRatio, SourceBounds};
use crate::panels::object_test_sheet::{RoomExtent, RoomPoint, Space, Span};
use crate::ui::group::Group;
use crate::ui::section::Section;
use crate::ui::{theme, widgets};

/// The box's colour: the 3D scene's (`room::SOURCE_BOUNDS_COLOR`).
const BOX: Color32 = Color32::from_rgb(0xff, 0x6b, 0xd6);
const BOX_FILL: Color32 = Color32::from_rgba_premultiplied(40, 17, 34, 40);
const ROOM_FILL: Color32 = Color32::from_rgba_premultiplied(13, 13, 13, 13);
const ROOM_EDGE: Color32 = Color32::from_rgba_premultiplied(76, 83, 89, 89);
const AXIS: Color32 = Color32::from_rgba_premultiplied(33, 35, 38, 38);
/// What lies outside the box is shaded, so the part that is heard stands out.
const OUTSIDE: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 110);
const HEARD: Color32 = Color32::from_rgb(0xd9, 0xec, 0xff);
const SILENCED: Color32 = Color32::from_rgba_premultiplied(70, 76, 82, 110);
/// The drawing's height cap, in points.
const MAX_HEIGHT: f32 = 200.0;
/// The gap between the plan and the side view, as a fraction of the scale.
const GAP: f32 = 0.25;
/// How far from an edge, in points, still grabs it.
const GRAB: f32 = 6.0;
/// The source dots' radius, in points.
const DOT: f32 = 2.6;

/// The plan or the side view.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    /// Seen from above: left → right across, front at the top.
    Plan,
    /// Seen from the right: back → front across, ceiling at the top.
    Side,
}

impl View {
    /// The ADM axes drawn across and up.
    fn axes(self) -> (usize, usize) {
        match self {
            View::Plan => (0, 1),
            View::Side => (1, 2),
        }
    }
}

/// What a drag on the drawing holds.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Grab {
    /// One side of the box: an ADM axis, and its max end or its min end.
    Edge { axis: usize, max: bool },
    /// The whole box, in a view's two axes, from where the drag began.
    Move {
        view: View,
        from: [f64; 2],
        start: SourceBounds,
    },
}

/// Where a view sits on screen, and its mapping between room and points.
struct Frame {
    rect: Rect,
    view: View,
    across: Span,
    up: Span,
    scale: f32,
}

impl Frame {
    fn to_screen(&self, a: f64, b: f64) -> Pos2 {
        Pos2::new(
            self.rect.left() + ((a - self.across.min) as f32) * self.scale,
            self.rect.bottom() - ((b - self.up.min) as f32) * self.scale,
        )
    }

    fn to_room(&self, p: Pos2) -> (f64, f64) {
        (
            self.across.min + f64::from((p.x - self.rect.left()) / self.scale),
            self.up.min + f64::from((self.rect.bottom() - p.y) / self.scale),
        )
    }
}

fn span(extent: &RoomExtent, axis: usize) -> Span {
    match axis {
        0 => extent.lateral,
        1 => extent.depth,
        _ => extent.height,
    }
}

fn room_axis(room: &RoomPoint, axis: usize) -> f64 {
    match axis {
        0 => room.lateral,
        1 => room.depth,
        _ => room.height,
    }
}

/// One ADM coordinate along `axis`, in room units. The room warp is separable,
/// so an axis maps on its own.
fn to_room_axis(space: Space<'_>, axis: usize, value: f64) -> f64 {
    let mut adm = [0.0; 3];
    adm[axis] = value;
    room_axis(&space.to_room(adm), axis)
}

/// The inverse of [`to_room_axis`].
fn to_adm_axis(space: Space<'_>, axis: usize, value: f64) -> f64 {
    let mut room = RoomPoint::default();
    match axis {
        0 => room.lateral = value,
        1 => room.depth = value,
        _ => room.height = value,
    }
    space.to_adm(room)[axis]
}

/// The box with `axis`'s side moved to `value`, never past the other side.
fn with_side(mut bounds: SourceBounds, axis: usize, max: bool, value: f32) -> SourceBounds {
    let value = value.clamp(-1.0, 1.0);
    if max {
        bounds.max[axis] = value.max(bounds.min[axis]);
    } else {
        bounds.min[axis] = value.min(bounds.max[axis]);
    }
    bounds
}

/// `start` moved by `delta` along `axis`, keeping its size and staying in the
/// cube.
fn shifted(
    mut bounds: SourceBounds,
    start: &SourceBounds,
    axis: usize,
    delta: f32,
) -> SourceBounds {
    let delta = delta.max(-1.0 - start.min[axis]).min(1.0 - start.max[axis]);
    bounds.min[axis] = start.min[axis] + delta;
    bounds.max[axis] = start.max[axis] + delta;
    bounds
}

/// A preset box, from its sides; `None` for a side bounds nothing.
fn preset(enabled: bool, min: [f32; 3], max: [f32; 3]) -> SourceBounds {
    SourceBounds { enabled, min, max }
}

/// The section's one-line summary while folded.
fn summary(bounds: &SourceBounds, silenced: usize) -> String {
    if !bounds.enabled {
        return t("sourceBounds.off").to_owned();
    }
    if !bounds.is_active() {
        return t("sourceBounds.wholeRoom").to_owned();
    }
    tf("sourceBounds.silenced", &[("count", &silenced.to_string())])
}

/// A source the drawing shows: where it is in ADM, and whether it is heard.
struct Dot {
    adm: [f64; 3],
    heard: bool,
}

impl StudioSpike {
    pub(crate) fn source_bounds_section(&mut self, ui: &mut Ui) {
        let (bounds, room, dots, ready) = {
            let live = self.host.read();
            let bounds = live.app.source_bounds;
            let speakers = live.selected_speakers();
            let dots: Vec<Dot> = live
                .app
                .sources
                .values()
                // Metadata-silent objects are not drawn in the scene either.
                .filter(|src| !src.gain_db.is_some_and(|g| g <= -128))
                .map(|src| {
                    // A direct channel is judged by its speaker's place, as
                    // the renderer does.
                    let adm = src
                        .direct_speaker_index
                        .and_then(|i| speakers.get(i as usize))
                        .map_or([src.x, src.y, src.z], |s| [s.x, s.y, s.z]);
                    Dot {
                        adm,
                        heard: bounds.admits(adm),
                    }
                })
                .collect();
            (
                bounds,
                live.app.room_ratio.clone(),
                dots,
                live.app.osc_snapshot_ready,
            )
        };
        let silenced = dots.iter().filter(|d| !d.heard).count();
        let mut next = bounds;
        let mut switch_on = bounds.enabled;
        Section::new("sourceBoundsSection", "sourceBounds.title")
            .icon(&crate::ui::icons::SECTION_SOURCE_BOUNDS)
            .help("help.sourceBounds")
            .summary(summary(&bounds, silenced))
            .show(ui, |ui| {
                let status = bounds
                    .is_active()
                    .then(|| tf("sourceBounds.silenced", &[("count", &silenced.to_string())]));
                let mut group =
                    Group::new(t("sourceBounds.enabled")).help("help.sourceBounds.enabled");
                if let Some(status) = status {
                    group = group.status(status, theme::WARN);
                }
                group
                    .actions(|ui| {
                        ui.add_enabled_ui(ready, |ui| {
                            widgets::switch(ui, &mut switch_on, t("sourceBounds.enabled"));
                        });
                    })
                    .show(ui, |ui| {
                        ui.add_enabled_ui(ready, |ui| {
                            self.source_bounds_drawing(ui, &mut next, &room, &dots);
                            source_bounds_fields(ui, &mut next);
                            let buttons = [
                                (t("sourceBounds.heights"), true),
                                (t("sourceBounds.floor"), true),
                                (t("sourceBounds.front"), true),
                                (t("sourceBounds.rear"), true),
                                (t("sourceBounds.reset"), true),
                            ];
                            let buttons: Vec<(&str, bool)> =
                                buttons.iter().map(|(text, on)| (*text, *on)).collect();
                            if let Some(index) = widgets::label_buttons_help(
                                ui,
                                t("sourceBounds.presets"),
                                "help.sourceBounds.presets",
                                &buttons,
                            ) {
                                // A preset is asked for to be heard: it turns
                                // the box on, but for the whole room, which
                                // gives everything back.
                                next = match index {
                                    0 => preset(true, [-1.0, -1.0, 0.5], [1.0, 1.0, 1.0]),
                                    1 => preset(true, [-1.0, -1.0, -1.0], [1.0, 1.0, 0.5]),
                                    2 => preset(true, [-1.0, 0.0, -1.0], [1.0, 1.0, 1.0]),
                                    3 => preset(true, [-1.0, -1.0, -1.0], [1.0, 0.0, 1.0]),
                                    _ => SourceBounds::default(),
                                };
                            }
                            widgets::note(ui, t("sourceBounds.transient"));
                        });
                    });
            });
        if switch_on != bounds.enabled {
            next.enabled = switch_on;
        }
        if next != bounds && ready {
            gain::control_source_bounds(&self.host, next);
        }
    }

    /// The plan and the side view, side by side at one scale, with the box
    /// drawn on both and its edges and inside draggable.
    fn source_bounds_drawing(
        &mut self,
        ui: &mut Ui,
        bounds: &mut SourceBounds,
        room: &RoomRatio,
        dots: &[Dot],
    ) {
        let space = Space {
            adm_view: false,
            room,
        };
        let extent = space.extent();
        let width = |s: Span| (s.max - s.min) as f32;
        // Plan: lateral × depth. Side: depth × height.
        let across_total = width(extent.lateral) + width(extent.depth);
        let tallest = width(extent.depth).max(width(extent.height));
        let avail = ui.available_width();
        let scale = (avail / (across_total * (1.0 + GAP / 2.0)))
            .min(MAX_HEIGHT / tallest)
            .max(1.0);
        let gap = GAP * scale;
        let drawn = vec2(across_total * scale + gap, tallest * scale + 14.0);
        let (rect, response) =
            ui.allocate_exact_size(vec2(avail, drawn.y), Sense::click_and_drag());
        let left = rect.left() + (avail - drawn.x).max(0.0) / 2.0;
        let top = rect.top() + 14.0;
        let frame = |view: View, x: f32| {
            let (a, b) = view.axes();
            let (across, up) = (span(&extent, a), span(&extent, b));
            let size = vec2(width(across) * scale, width(up) * scale);
            Frame {
                rect: Rect::from_min_size(Pos2::new(x, top + tallest * scale - size.y), size),
                view,
                across,
                up,
                scale,
            }
        };
        let plan = frame(View::Plan, left);
        let side = frame(View::Side, left + width(extent.lateral) * scale + gap);
        let frames = [&plan, &side];

        // The box's sides in room units, per ADM axis.
        let sides = |b: &SourceBounds| -> [(f64, f64); 3] {
            std::array::from_fn(|axis| {
                (
                    to_room_axis(space, axis, f64::from(b.min[axis])),
                    to_room_axis(space, axis, f64::from(b.max[axis])),
                )
            })
        };
        let box_rect = |f: &Frame, b: &SourceBounds| {
            let s = sides(b);
            let (a, v) = f.view.axes();
            Rect::from_two_pos(f.to_screen(s[a].0, s[v].1), f.to_screen(s[a].1, s[v].0))
        };

        // What the pointer is over: an edge, nearest first, else the inside.
        let target_at = |p: Pos2, bounds: &SourceBounds| -> Option<Grab> {
            let f = frames
                .into_iter()
                .find(|f| f.rect.expand(GRAB).contains(p))?;
            let r = box_rect(f, bounds);
            let (a, v) = f.view.axes();
            let near_x = p.y >= r.top() - GRAB && p.y <= r.bottom() + GRAB;
            let near_y = p.x >= r.left() - GRAB && p.x <= r.right() + GRAB;
            let mut best: Option<(f32, Grab)> = None;
            let mut consider = |d: f32, grab: Grab| {
                if d <= GRAB && best.is_none_or(|(bd, _)| d < bd) {
                    best = Some((d, grab));
                }
            };
            if near_x {
                consider(
                    (p.x - r.left()).abs(),
                    Grab::Edge {
                        axis: a,
                        max: false,
                    },
                );
                consider((p.x - r.right()).abs(), Grab::Edge { axis: a, max: true });
            }
            if near_y {
                consider(
                    (p.y - r.bottom()).abs(),
                    Grab::Edge {
                        axis: v,
                        max: false,
                    },
                );
                consider((p.y - r.top()).abs(), Grab::Edge { axis: v, max: true });
            }
            if let Some((_, grab)) = best {
                return Some(grab);
            }
            r.contains(p).then(|| {
                let (ra, rb) = f.to_room(p);
                Grab::Move {
                    view: f.view,
                    from: [to_adm_axis(space, a, ra), to_adm_axis(space, v, rb)],
                    start: *bounds,
                }
            })
        };

        if response.drag_started() {
            self.source_bounds_grab = response
                .interact_pointer_pos()
                .and_then(|p| target_at(p, bounds));
        }
        if response.drag_stopped() {
            self.source_bounds_grab = None;
        }
        if let Some(grab) = self.source_bounds_grab
            && let Some(p) = response.interact_pointer_pos()
        {
            match grab {
                Grab::Edge { axis, max } => {
                    // Either view carrying the axis maps the pointer to it.
                    let f = frames
                        .into_iter()
                        .filter(|f| {
                            let (a, v) = f.view.axes();
                            a == axis || v == axis
                        })
                        .min_by(|x, y| {
                            x.rect
                                .distance_to_pos(p)
                                .total_cmp(&y.rect.distance_to_pos(p))
                        });
                    if let Some(f) = f {
                        let (ra, rb) = f.to_room(p);
                        let room_value = if f.view.axes().0 == axis { ra } else { rb };
                        let value = to_adm_axis(space, axis, room_value) as f32;
                        *bounds = with_side(*bounds, axis, max, value);
                        // Dragging an edge means wanting the box heard.
                        bounds.enabled = true;
                    }
                }
                Grab::Move { view, from, start } => {
                    let f = if view == View::Plan { &plan } else { &side };
                    let (a, v) = view.axes();
                    let (ra, rb) = f.to_room(p);
                    let now = [to_adm_axis(space, a, ra), to_adm_axis(space, v, rb)];
                    let moved = shifted(*bounds, &start, a, (now[0] - from[0]) as f32);
                    *bounds = shifted(moved, &start, v, (now[1] - from[1]) as f32);
                    bounds.enabled = true;
                }
            }
        }
        let hovered = self
            .source_bounds_grab
            .or_else(|| response.hover_pos().and_then(|p| target_at(p, bounds)));
        match hovered {
            Some(Grab::Edge { axis, .. }) => {
                let horizontal = match axis {
                    0 => true,
                    // Depth runs up the plan and across the side view.
                    1 => response
                        .hover_pos()
                        .or(response.interact_pointer_pos())
                        .is_some_and(|p| side.rect.expand(GRAB).contains(p)),
                    _ => false,
                };
                ui.ctx().set_cursor_icon(if horizontal {
                    CursorIcon::ResizeHorizontal
                } else {
                    CursorIcon::ResizeVertical
                });
            }
            Some(Grab::Move { .. }) => {
                ui.ctx()
                    .set_cursor_icon(if self.source_bounds_grab.is_some() {
                        CursorIcon::Grabbing
                    } else {
                        CursorIcon::Grab
                    })
            }
            None => {}
        }

        // Paint.
        let painter = ui.painter().with_clip_rect(rect.expand(2.0));
        let font = egui::FontId::proportional(theme::FONT_SIZE_SMALL);
        for f in frames {
            painter.rect_filled(f.rect, 2.0, ROOM_FILL);
            // The listener's axes through the origin.
            let o = f.to_screen(0.0, 0.0);
            painter.line_segment(
                [
                    Pos2::new(f.rect.left(), o.y),
                    Pos2::new(f.rect.right(), o.y),
                ],
                Stroke::new(1.0, AXIS),
            );
            painter.line_segment(
                [
                    Pos2::new(o.x, f.rect.top()),
                    Pos2::new(o.x, f.rect.bottom()),
                ],
                Stroke::new(1.0, AXIS),
            );
            let caption = match f.view {
                View::Plan => t("sourceBounds.plan"),
                View::Side => t("sourceBounds.side"),
            };
            painter.text(
                Pos2::new(f.rect.left(), f.rect.top() - 2.0),
                egui::Align2::LEFT_BOTTOM,
                caption,
                font.clone(),
                theme::TEXT_MUTED,
            );
            // Depth is the one direction a reader cannot guess.
            let (front, back) = (t("sourceBounds.frontEnd"), t("sourceBounds.backEnd"));
            match f.view {
                View::Plan => {
                    painter.text(
                        Pos2::new(f.rect.center().x, f.rect.top() + 1.0),
                        egui::Align2::CENTER_TOP,
                        front,
                        font.clone(),
                        theme::TEXT_FAINT,
                    );
                    painter.text(
                        Pos2::new(f.rect.center().x, f.rect.bottom() - 1.0),
                        egui::Align2::CENTER_BOTTOM,
                        back,
                        font.clone(),
                        theme::TEXT_FAINT,
                    );
                }
                View::Side => {
                    painter.text(
                        Pos2::new(f.rect.right() - 2.0, f.rect.center().y),
                        egui::Align2::RIGHT_CENTER,
                        front,
                        font.clone(),
                        theme::TEXT_FAINT,
                    );
                    painter.text(
                        Pos2::new(f.rect.left() + 2.0, f.rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        back,
                        font.clone(),
                        theme::TEXT_FAINT,
                    );
                }
            }
            let r = box_rect(f, bounds);
            if bounds.enabled {
                // Shade what lies outside the box: four bands around it.
                for band in [
                    Rect::from_min_max(f.rect.min, Pos2::new(f.rect.right(), r.top())),
                    Rect::from_min_max(Pos2::new(f.rect.left(), r.bottom()), f.rect.max),
                    Rect::from_min_max(
                        Pos2::new(f.rect.left(), r.top()),
                        Pos2::new(r.left(), r.bottom()),
                    ),
                    Rect::from_min_max(
                        Pos2::new(r.right(), r.top()),
                        Pos2::new(f.rect.right(), r.bottom()),
                    ),
                ] {
                    if band.is_positive() {
                        painter.rect_filled(band, 0.0, OUTSIDE);
                    }
                }
                painter.rect_filled(r, 0.0, BOX_FILL);
            }
            painter.rect_stroke(f.rect, 2.0, Stroke::new(1.0, ROOM_EDGE), StrokeKind::Inside);
            let (a, v) = f.view.axes();
            for dot in dots {
                let room_point = space.to_room(dot.adm);
                let at = f.to_screen(room_axis(&room_point, a), room_axis(&room_point, v));
                let colour = if dot.heard { HEARD } else { SILENCED };
                painter.circle_filled(at, DOT, colour);
            }
            let edge = if bounds.enabled {
                BOX
            } else {
                BOX.gamma_multiply(0.4)
            };
            painter.rect_stroke(r, 0.0, Stroke::new(1.5, edge), StrokeKind::Middle);
            // The grabbed or hovered edge, thicker.
            if let Some(Grab::Edge { axis, max }) = hovered {
                let line = if axis == a {
                    let x = if max { r.right() } else { r.left() };
                    Some([Pos2::new(x, r.top()), Pos2::new(x, r.bottom())])
                } else if axis == v {
                    let y = if max { r.top() } else { r.bottom() };
                    Some([Pos2::new(r.left(), y), Pos2::new(r.right(), y)])
                } else {
                    None
                };
                if let Some(line) = line {
                    painter.line_segment(line, Stroke::new(3.0, BOX));
                }
            }
        }
    }
}

/// One side of the box as a number field.
fn bound_field(value: &mut f32, range: std::ops::RangeInclusive<f32>) -> egui::DragValue<'_> {
    egui::DragValue::new(value)
        .speed(0.01)
        .range(range)
        .fixed_decimals(2)
}

/// The box's sides as numbers, an axis per row: min and max, in the cube's
/// `[-1, 1]`.
fn source_bounds_fields(ui: &mut Ui, bounds: &mut SourceBounds) {
    let labels = [
        t("sourceBounds.lateral"),
        t("sourceBounds.depth"),
        t("sourceBounds.height"),
    ];
    for (axis, label) in labels.into_iter().enumerate() {
        let (mut lo, mut hi) = (bounds.min[axis], bounds.max[axis]);
        let changed = widgets::label_row(ui, label, |ui| {
            // Right to left: the max is placed first.
            let hi_changed =
                widgets::number_field(ui, 48.0, bound_field(&mut hi, lo..=1.0)).changed();
            ui.label(egui::RichText::new("…").color(theme::TEXT_MUTED));
            let lo_changed =
                widgets::number_field(ui, 48.0, bound_field(&mut lo, -1.0..=hi)).changed();
            hi_changed || lo_changed
        });
        if changed {
            *bounds = with_side(with_side(*bounds, axis, false, lo), axis, true, hi);
            bounds.enabled = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_side_never_passes_the_other() {
        let b = preset(true, [-0.5, -1.0, 0.0], [0.5, 1.0, 1.0]);
        let moved = with_side(b, 0, true, -0.8);
        assert_eq!((moved.min[0], moved.max[0]), (-0.5, -0.5));
        let moved = with_side(b, 2, false, 2.0);
        assert_eq!((moved.min[2], moved.max[2]), (1.0, 1.0));
    }

    #[test]
    fn a_moved_box_keeps_its_size_inside_the_cube() {
        let start = preset(true, [-0.5, 0.0, 0.0], [0.5, 1.0, 1.0]);
        let moved = shifted(start, &start, 0, 0.9);
        assert_eq!((moved.min[0], moved.max[0]), (0.0, 1.0));
        let moved = shifted(start, &start, 1, -0.4);
        assert!((moved.min[1] + 0.4).abs() < 1e-6 && (moved.max[1] - 0.6).abs() < 1e-6);
    }

    #[test]
    fn the_summary_says_what_is_silenced() {
        assert_eq!(summary(&SourceBounds::default(), 0), t("sourceBounds.off"));
        let whole = SourceBounds {
            enabled: true,
            ..SourceBounds::default()
        };
        assert_eq!(summary(&whole, 0), t("sourceBounds.wholeRoom"));
        let heights = preset(true, [-1.0, -1.0, 0.5], [1.0, 1.0, 1.0]);
        assert_eq!(
            summary(&heights, 3),
            tf("sourceBounds.silenced", &[("count", "3")])
        );
    }
}
