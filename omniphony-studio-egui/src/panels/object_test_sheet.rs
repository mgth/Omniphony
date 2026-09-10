//! The object-test placement widget (`#objectTestFaces`, `object-test.js`):
//! three orthographic views of the room laid out as a CAD sheet, with the
//! single-axis sliders living in the gutters between them.
//!
//! The whole drawing shares one scale, so a unit of room is the same number of
//! sheet units in every view: the side view's depth is visibly the same length
//! as the plan's depth, and a tall room looks tall next to its own plan. The
//! sheet is normalised to 100 on its larger side, which is why every size here
//! is a bare number — they are sheet units, not pixels.

use egui::{Color32, Pos2, Rect, Stroke, vec2};

use crate::model::app_state::RoomRatio;

/// Gutter between views, as a fraction of the room's largest extent. It is not
/// empty space: it carries the single-axis sliders.
const GUTTER: f64 = 0.24;
/// Marker radius, in sheet units.
pub const MARKER_R: f64 = 2.4;
/// How far off a slider's track still counts as grabbing it.
const SLIDER_GRAB: f64 = 3.2;
/// Sheet height cap, in points (`max-height:300px`).
pub const MAX_HEIGHT: f32 = 300.0;

const FACE_FILL: Color32 = Color32::from_rgba_premultiplied(13, 13, 13, 13);
const FACE_EDGE: Color32 = Color32::from_rgba_premultiplied(76, 83, 89, 89);
const FACE_AXIS: Color32 = Color32::from_rgba_premultiplied(33, 35, 38, 38);
const MITRE: Color32 = Color32::from_rgba_premultiplied(48, 52, 56, 56);
const CAPTION: Color32 = Color32::from_rgba_premultiplied(163, 177, 191, 191);
const END_LABEL: Color32 = Color32::from_rgba_premultiplied(98, 106, 115, 115);
const TRACK: Color32 = Color32::from_rgba_premultiplied(61, 66, 71, 71);
const TICK: Color32 = Color32::from_rgba_premultiplied(76, 83, 89, 89);
const GRID: Color32 = Color32::from_rgba_premultiplied(76, 83, 89, 89);
const THUMB: Color32 = Color32::from_rgb(0x5c, 0xff, 0x9a);
const ORBIT: Color32 = Color32::from_rgba_premultiplied(78, 217, 131, 217);

// ---------------------------------------------------------------------------
// Room space
// ---------------------------------------------------------------------------

/// The three axes of the space the sheet is drawn in.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Lateral,
    Depth,
    Height,
}

impl Axis {
    /// The ADM component this room axis corresponds to (`ADM_OF_ROOM_AXIS`).
    fn adm_index(self) -> usize {
        match self {
            Axis::Lateral => 0,
            Axis::Depth => 1,
            Axis::Height => 2,
        }
    }
}

/// A room point in `{lateral, depth, height}`, as the faces read it.
#[derive(Clone, Copy, Default)]
pub struct RoomPoint {
    pub lateral: f64,
    pub depth: f64,
    pub height: f64,
}

impl RoomPoint {
    fn get(&self, axis: Axis) -> f64 {
        match axis {
            Axis::Lateral => self.lateral,
            Axis::Depth => self.depth,
            Axis::Height => self.height,
        }
    }

    fn set(&mut self, axis: Axis, value: f64) {
        match axis {
            Axis::Lateral => self.lateral = value,
            Axis::Depth => self.depth = value,
            Axis::Height => self.height = value,
        }
    }
}

fn clamp1(v: f64) -> f64 {
    v.clamp(-1.0, 1.0)
}

#[derive(Clone, Copy)]
pub struct Span {
    pub min: f64,
    pub max: f64,
}

/// The room's reach along each scene axis.
///
/// A room is neither a cube nor symmetric: it usually reaches further in front
/// than behind and further up than down, and the depth axis is warped on top of
/// that. Drawing the faces from `width`/`length`/`height` alone would draw a
/// room nobody configured.
#[derive(Clone, Copy)]
pub struct RoomExtent {
    pub lateral: Span,
    pub depth: Span,
    pub height: Span,
}

impl RoomExtent {
    fn span(&self, axis: Axis) -> Span {
        match axis {
            Axis::Lateral => self.lateral,
            Axis::Depth => self.depth,
            Axis::Height => self.height,
        }
    }
}

/// The view the sheet is drawn in: the room, or the renderer's unit cube.
#[derive(Clone, Copy)]
pub struct Space<'a> {
    pub adm_view: bool,
    pub room: &'a RoomRatio,
}

impl Space<'_> {
    pub fn extent(&self) -> RoomExtent {
        // ADM space is the unit cube by definition, so every face is square
        // and the origin sits at the middle of each.
        if self.adm_view {
            let unit = Span {
                min: -1.0,
                max: 1.0,
            };
            return RoomExtent {
                lateral: unit,
                depth: unit,
                height: unit,
            };
        }
        let r = self.room;
        let num = |v: f64, d: f64| if v.is_finite() && v > 0.0 { v } else { d };
        RoomExtent {
            lateral: Span {
                min: -num(r.width, 1.0),
                max: num(r.width, 1.0),
            },
            depth: Span {
                min: -num(r.rear, 1.0),
                max: num(r.length, 1.0),
            },
            height: Span {
                min: -num(r.lower, 0.5),
                max: num(r.height, 1.0),
            },
        }
    }

    /// ADM → the space the sheet is drawn in. In ADM view this is only the
    /// axis renaming, so the drawing is a plain view of the unit cube.
    pub fn to_room(&self, pos: [f64; 3]) -> RoomPoint {
        if self.adm_view {
            return RoomPoint {
                lateral: pos[0],
                depth: pos[1],
                height: pos[2],
            };
        }
        use omniphony_geometry::f64 as g;
        let r = self.room;
        let scaled = g::room_scaled_position(
            [clamp1(pos[0]), clamp1(pos[1]), clamp1(pos[2])],
            [r.width, r.length, r.height],
            r.rear,
            r.lower,
            r.center_blend,
        );
        let s = g::adm_to_scene(scaled);
        RoomPoint {
            depth: s[0],
            height: s[1],
            lateral: s[2],
        }
    }

    /// The exact inverse of [`Space::to_room`], in whichever view is current.
    pub fn to_adm(&self, room: RoomPoint) -> [f64; 3] {
        if self.adm_view {
            return [
                clamp1(room.lateral),
                clamp1(room.depth),
                clamp1(room.height),
            ];
        }
        use omniphony_geometry::f64 as g;
        let r = self.room;
        let scaled = g::scene_to_adm([room.depth, room.height, room.lateral]);
        g::inverse_room_scaled_position(
            scaled,
            [r.width, r.length, r.height],
            r.rear,
            r.lower,
            r.center_blend,
        )
    }
}

// ---------------------------------------------------------------------------
// The sheet
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FaceId {
    Floor,
    Front,
    Side,
}

#[derive(Clone, Copy)]
pub struct AxisRef {
    pub axis: Axis,
    pub invert: bool,
}

/// Which of a face's two axes carries depth, when one of them does.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Which {
    H,
    V,
}

pub struct Face {
    pub id: FaceId,
    pub label_key: &'static str,
    pub h: AxisRef,
    pub v: AxisRef,
    /// Depth is the only direction a reader cannot guess — left/right and
    /// floor/ceiling speak for themselves — so it is the only one labelled.
    pub depth_ends: Option<Which>,
}

pub const FACES: &[Face] = &[
    Face {
        id: FaceId::Floor,
        label_key: "objectTest.facePlan",
        h: AxisRef {
            axis: Axis::Lateral,
            invert: false,
        },
        v: AxisRef {
            axis: Axis::Depth,
            invert: true,
        },
        depth_ends: Some(Which::V),
    },
    Face {
        id: FaceId::Front,
        label_key: "objectTest.faceFront",
        h: AxisRef {
            axis: Axis::Lateral,
            invert: false,
        },
        v: AxisRef {
            axis: Axis::Height,
            invert: true,
        },
        depth_ends: None,
    },
    Face {
        id: FaceId::Side,
        label_key: "objectTest.faceSide",
        h: AxisRef {
            axis: Axis::Depth,
            invert: false,
        },
        v: AxisRef {
            axis: Axis::Height,
            invert: true,
        },
        depth_ends: Some(Which::H),
    },
];

impl Face {
    fn axis(&self, which: Which) -> AxisRef {
        match which {
            Which::H => self.h,
            Which::V => self.v,
        }
    }
}

/// A rectangle in sheet units.
#[derive(Clone, Copy)]
pub struct Box64 {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Box64 {
    fn contains(&self, px: f64, py: f64) -> bool {
        px >= self.x && px <= self.x + self.w && py >= self.y && py <= self.y + self.h
    }
}

pub struct Sheet {
    pub w: f64,
    pub h: f64,
    pub gutter: f64,
    pub mitre: Box64,
    pub extent: RoomExtent,
    floor: Box64,
    front: Box64,
    side: Box64,
}

impl Sheet {
    pub fn rect(&self, id: FaceId) -> Box64 {
        match id {
            FaceId::Floor => self.floor,
            FaceId::Front => self.front,
            FaceId::Side => self.side,
        }
    }
}

/// Lay the three views out and return every rectangle in one coordinate system.
///
/// ```text
///     side        front
///      ·          floor      the empty corner carries the 45° mitre
/// ```
///
/// Column widths are (depth, width) and the lower row is depth tall, so the
/// empty bottom-left cell is depth × depth — exactly square, which is what
/// lets the mitre run at a true 45°.
pub fn layout(extent: RoomExtent) -> Sheet {
    let w = extent.lateral.max - extent.lateral.min;
    let d = extent.depth.max - extent.depth.min;
    let h = extent.height.max - extent.height.min;
    let g = GUTTER * w.max(d).max(h);
    let raw_w = d + g + w;
    let raw_h = h + g + d;
    // Normalise the larger dimension to 100 so stroke widths, marker size and
    // type size read the same whatever the room's proportions.
    let s = 100.0 / raw_w.max(raw_h);
    let col2 = (d + g) * s;
    let row2 = (h + g) * s;
    Sheet {
        w: raw_w * s,
        h: raw_h * s,
        gutter: g * s,
        mitre: Box64 {
            x: 0.0,
            y: row2,
            w: d * s,
            h: d * s,
        },
        extent,
        side: Box64 {
            x: 0.0,
            y: 0.0,
            w: d * s,
            h: h * s,
        },
        front: Box64 {
            x: col2,
            y: 0.0,
            w: w * s,
            h: h * s,
        },
        floor: Box64 {
            x: col2,
            y: row2,
            w: w * s,
            h: d * s,
        },
    }
}

/// ADM position → a point inside `rect`, for one face.
///
/// Goes through room space rather than mapping ADM linearly onto the rectangle:
/// the depth is warped and its halves are unequal, so a source at ADM y = 0.5
/// is not half way to the front wall. Projecting the same way the 3D scene does
/// is what makes the two views agree.
fn face_to_sheet(
    face: &Face,
    rect: Box64,
    pos: [f64; 3],
    sheet: &Sheet,
    space: Space,
) -> (f64, f64) {
    let room = space.to_room(pos);
    let along = |a: AxisRef, size: f64| {
        let span = sheet.extent.span(a.axis);
        let t = (room.get(a.axis) - span.min) / (span.max - span.min).max(1e-9);
        (if a.invert { 1.0 - t } else { t }) * size
    };
    (
        rect.x + along(face.h, rect.w),
        rect.y + along(face.v, rect.h),
    )
}

/// A point inside `rect` → the two ADM axes that face drives. The third is
/// preserved exactly rather than round-tripped through the warp.
fn sheet_to_face(
    face: &Face,
    rect: Box64,
    px: f64,
    py: f64,
    sheet: &Sheet,
    current: [f64; 3],
    space: Space,
) -> [f64; 3] {
    let mut room = space.to_room(current);
    let back = |a: AxisRef, offset: f64, size: f64| {
        let span = sheet.extent.span(a.axis);
        let mut t = (offset / size.max(1e-9)).clamp(0.0, 1.0);
        if a.invert {
            t = 1.0 - t;
        }
        span.min + t * (span.max - span.min)
    };
    room.set(face.h.axis, back(face.h, px - rect.x, rect.w));
    room.set(face.v.axis, back(face.v, py - rect.y, rect.h));
    space.to_adm(room)
}

// ---------------------------------------------------------------------------
// Gutter sliders
// ---------------------------------------------------------------------------

/// Where a slider lies relative to its view.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lane {
    Below,
    Left,
}

pub struct Slider {
    face: FaceId,
    use_axis: Which,
    lane: Lane,
    pub label_key: &'static str,
}

/// One slider per gutter.
///
/// ```text
///     side       front         [x] below the front view
///     [y]        [x]           [z] left of the front view
///      ·    [y]  floor
/// ```
///
/// Each lies beside the view whose axis it drives and runs parallel to that
/// axis *in that view*, so a slider and the marker it moves always travel the
/// same direction. Depth gets two, because two views show it; they run
/// visibly opposite ways because each matches its own neighbour, and they need
/// no synchronising code since both read and write the same coordinate.
pub const SLIDERS: &[Slider] = &[
    Slider {
        face: FaceId::Front,
        use_axis: Which::H,
        lane: Lane::Below,
        label_key: "objectTest.sliderX",
    },
    Slider {
        face: FaceId::Front,
        use_axis: Which::V,
        lane: Lane::Left,
        label_key: "objectTest.sliderZ",
    },
    Slider {
        face: FaceId::Side,
        use_axis: Which::H,
        lane: Lane::Below,
        label_key: "objectTest.sliderY",
    },
    Slider {
        face: FaceId::Floor,
        use_axis: Which::V,
        lane: Lane::Left,
        label_key: "objectTest.sliderY",
    },
];

struct Track {
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
    horizontal: bool,
}

impl Slider {
    fn axis(&self) -> AxisRef {
        let face = FACES.iter().find(|f| f.id == self.face).expect("face");
        face.axis(self.use_axis)
    }

    fn track(&self, sheet: &Sheet) -> Track {
        let rect = sheet.rect(self.face);
        let half = sheet.gutter / 2.0;
        match self.lane {
            // Horizontal, centred in the gutter below its view, exactly as
            // long as it.
            Lane::Below => Track {
                x1: rect.x,
                y1: rect.y + rect.h + half,
                x2: rect.x + rect.w,
                y2: rect.y + rect.h + half,
                horizontal: true,
            },
            Lane::Left => Track {
                x1: rect.x - half,
                y1: rect.y,
                x2: rect.x - half,
                y2: rect.y + rect.h,
                horizontal: false,
            },
        }
    }

    fn thumb(&self, sheet: &Sheet, pos: [f64; 3], space: Space) -> (f64, f64) {
        let track = self.track(sheet);
        let a = self.axis();
        let room = space.to_room(pos);
        let span = sheet.extent.span(a.axis);
        let mut t = (room.get(a.axis) - span.min) / (span.max - span.min).max(1e-9);
        if a.invert {
            t = 1.0 - t;
        }
        let t = t.clamp(0.0, 1.0);
        (
            track.x1 + (track.x2 - track.x1) * t,
            track.y1 + (track.y2 - track.y1) * t,
        )
    }

    /// A point on (or near) the track → the position it means, with the other
    /// two coordinates untouched.
    fn value_at(
        &self,
        sheet: &Sheet,
        px: f64,
        py: f64,
        current: [f64; 3],
        space: Space,
    ) -> [f64; 3] {
        let track = self.track(sheet);
        let mut t = if track.horizontal {
            (px - track.x1) / (track.x2 - track.x1).max(1e-9)
        } else {
            (py - track.y1) / (track.y2 - track.y1).max(1e-9)
        };
        t = t.clamp(0.0, 1.0);
        let a = self.axis();
        if a.invert {
            t = 1.0 - t;
        }
        let span = sheet.extent.span(a.axis);
        let mut room = space.to_room(current);
        room.set(a.axis, span.min + t * (span.max - span.min));
        space.to_adm(room)
    }

    /// Nudge one ADM coordinate, for the keyboard — a step in the room's
    /// warped space would grow and shrink as the source moved.
    pub fn nudge(&self, current: [f64; 3], delta: f64, absolute: Option<f64>) -> [f64; 3] {
        let index = self.axis().axis.adm_index();
        let mut next = current;
        next[index] = clamp1(absolute.unwrap_or(next[index] + delta));
        next
    }

    /// Pad along the track as well as across it, so the two extremes are as
    /// grabbable as the middle — and the extremes are exactly the positions
    /// this tool exists to try.
    fn grabbed(&self, sheet: &Sheet, px: f64, py: f64) -> bool {
        let track = self.track(sheet);
        let (lo_x, hi_x) = (
            track.x1.min(track.x2) - SLIDER_GRAB,
            track.x1.max(track.x2) + SLIDER_GRAB,
        );
        let (lo_y, hi_y) = (
            track.y1.min(track.y2) - SLIDER_GRAB,
            track.y1.max(track.y2) + SLIDER_GRAB,
        );
        if track.horizontal {
            px >= lo_x && px <= hi_x && (py - track.y1).abs() <= SLIDER_GRAB
        } else {
            py >= lo_y && py <= hi_y && (px - track.x1).abs() <= SLIDER_GRAB
        }
    }
}

/// What a gesture is locked to for its whole duration: the views and sliders
/// are adjacent, and a pointer that strays across a gutter mid-drag must not
/// silently start driving something else.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Face(FaceId),
    /// Index into [`SLIDERS`].
    Slider(usize),
}

/// Sliders win over the views: their grab areas sit in the gutters, but a
/// generous one can overlap a view's edge, and the narrower intent wins.
pub fn target_at(sheet: &Sheet, px: f64, py: f64) -> Option<Target> {
    if let Some(index) = SLIDERS.iter().position(|s| s.grabbed(sheet, px, py)) {
        return Some(Target::Slider(index));
    }
    FACES
        .iter()
        .find(|f| sheet.rect(f.id).contains(px, py))
        .map(|f| Target::Face(f.id))
}

/// The position a pointer at `(px, py)` asks for, given what it is locked to.
pub fn position_at(
    sheet: &Sheet,
    target: Target,
    px: f64,
    py: f64,
    current: [f64; 3],
    space: Space,
) -> [f64; 3] {
    match target {
        Target::Slider(index) => SLIDERS[index].value_at(sheet, px, py, current, space),
        Target::Face(id) => {
            let face = FACES.iter().find(|f| f.id == id).expect("face");
            sheet_to_face(face, sheet.rect(id), px, py, sheet, current, space)
        }
    }
}

// ---------------------------------------------------------------------------
// Painting
// ---------------------------------------------------------------------------

/// Sheet units → points on screen, honouring the letterboxing.
pub struct Transform {
    origin: Pos2,
    scale: f32,
    pad: f64,
}

impl Transform {
    pub fn to_screen(&self, x: f64, y: f64) -> Pos2 {
        self.origin
            + vec2(
                ((x + self.pad) as f32) * self.scale,
                ((y + self.pad) as f32) * self.scale,
            )
    }

    pub fn to_sheet(&self, p: Pos2) -> (f64, f64) {
        (
            f64::from((p.x - self.origin.x) / self.scale) - self.pad,
            f64::from((p.y - self.origin.y) / self.scale) - self.pad,
        )
    }

    fn len(&self, units: f64) -> f32 {
        (units as f32) * self.scale
    }
}

/// Fit the sheet into `avail` and return the drawing's screen box.
///
/// A source against a wall sits exactly on a box edge, where half the marker
/// would fall outside the sheet — precisely the positions (hard left, ceiling,
/// back wall) this tool exists to try — so the view box is padded by one.
pub fn fit(sheet: &Sheet, avail: Rect) -> (Rect, Transform) {
    let pad = MARKER_R + 1.0;
    let vb_w = sheet.w + 2.0 * pad;
    let vb_h = sheet.h + 2.0 * pad;
    let mut w = avail.width();
    let mut h = w * (vb_h / vb_w) as f32;
    if h > MAX_HEIGHT {
        h = MAX_HEIGHT;
        w = h * (vb_w / vb_h) as f32;
    }
    let rect = Rect::from_center_size(
        egui::pos2(avail.center().x, avail.min.y + h / 2.0),
        vec2(w, h),
    );
    let transform = Transform {
        origin: rect.min,
        scale: w / vb_w as f32,
        pad,
    };
    (rect, transform)
}

/// What the sheet draws on top of the room: where the source is, where it will
/// travel, and which nodes a snap would land on.
pub struct Overlay<'a> {
    pub position: [f64; 3],
    /// The orbit sampled as a closed path, or empty when the radius is zero.
    pub orbit: &'a [[f64; 3]],
    /// Snap nodes per ADM axis, or `None` when snapping is off or ungridded.
    pub grid: Option<&'a [Vec<f64>; 3]>,
    /// The slider a keyboard focus is on, drawn with a brighter track.
    pub focused: Option<usize>,
}

pub fn paint(
    painter: &egui::Painter,
    sheet: &Sheet,
    t: &Transform,
    space: Space,
    overlay: &Overlay<'_>,
) {
    // The mitre carries depth between the plan and the side view: a point's
    // distance from the plan's front edge and from the side view's front edge
    // are the same distance, which is what makes these two views one drawing.
    dashed_line(
        painter,
        t.to_screen(sheet.mitre.x, sheet.mitre.y + sheet.mitre.h),
        t.to_screen(sheet.mitre.x + sheet.mitre.w, sheet.mitre.y),
        Stroke::new(t.len(0.5).max(0.5), MITRE),
        t.len(2.0),
    );
    for face in FACES {
        paint_face(painter, face, sheet, t, space, overlay);
    }
    for (index, slider) in SLIDERS.iter().enumerate() {
        paint_slider(
            painter,
            slider,
            sheet,
            t,
            space,
            overlay,
            overlay.focused == Some(index),
        );
    }
}

fn paint_face(
    painter: &egui::Painter,
    face: &Face,
    sheet: &Sheet,
    t: &Transform,
    space: Space,
    overlay: &Overlay<'_>,
) {
    let r = sheet.rect(face.id);
    let screen = Rect::from_min_max(t.to_screen(r.x, r.y), t.to_screen(r.x + r.w, r.y + r.h));
    painter.rect(
        screen,
        0.0,
        FACE_FILL,
        Stroke::new(t.len(1.0).max(1.0), FACE_EDGE),
        egui::StrokeKind::Inside,
    );

    // The room's axes through the ORIGIN, not the middle of the rectangle: a
    // room reaching twice as far forward as back has its origin a third of the
    // way up the plan. Projected with the same function as the marker, so
    // placing the source at 0, 0, 0 always lands on the cross.
    let (ox, oy) = face_to_sheet(face, r, [0.0; 3], sheet, space);
    let axis_stroke = Stroke::new(t.len(1.0).max(1.0), FACE_AXIS);
    let dash = t.len(3.0);
    dashed_line(
        painter,
        t.to_screen(ox, r.y),
        t.to_screen(ox, r.y + r.h),
        axis_stroke,
        dash,
    );
    dashed_line(
        painter,
        t.to_screen(r.x, oy),
        t.to_screen(r.x + r.w, oy),
        axis_stroke,
        dash,
    );

    // The grid the snap lands on, under everything else: the nodes are not
    // evenly spaced on screen once the depth is warped, and without the ticks
    // a drag appears to stick at irregular intervals for no visible reason.
    if let Some(grid) = overlay.grid {
        paint_grid(painter, face, r, sheet, t, space, overlay.position, grid);
    }

    // A circle in 3D projects to an ellipse on a face — or, once the clamp
    // bites, to something with flats on it. Sampling the same function the
    // renderer uses draws whichever it really is.
    if overlay.orbit.len() > 1 {
        let points: Vec<Pos2> = overlay
            .orbit
            .iter()
            .map(|p| {
                let (x, y) = face_to_sheet(face, r, *p, sheet, space);
                t.to_screen(x, y)
            })
            .collect();
        painter.add(egui::Shape::line(
            points,
            Stroke::new(t.len(1.1).max(1.0), ORBIT),
        ));
    }

    let (mx, my) = face_to_sheet(face, r, overlay.position, sheet, space);
    let centre = t.to_screen(mx, my);
    painter.circle(
        centre,
        t.len(MARKER_R),
        THUMB,
        Stroke::new(t.len(1.0).max(1.0), Color32::from_black_alpha(140)),
    );

    painter.text(
        t.to_screen(r.x + 1.0, r.y + r.h - 1.6),
        egui::Align2::LEFT_BOTTOM,
        crate::i18n::t(face.label_key),
        egui::FontId::proportional(t.len(3.4).max(6.0)),
        CAPTION,
    );

    // Which end is which is read off the axis rather than written down: an
    // inverted axis puts the front at the start of its travel.
    if let Some(which) = face.depth_ends {
        let a = face.axis(which);
        let (start, end) = if a.invert {
            ("objectTest.axisFront", "objectTest.axisBack")
        } else {
            ("objectTest.axisBack", "objectTest.axisFront")
        };
        let font = egui::FontId::proportional(t.len(2.8).max(6.0));
        let ends: [(&str, f64, f64, egui::Align2); 2] = if which == Which::V {
            // Vertical: both labels on the right, clear of the markers.
            [
                (
                    start,
                    r.x + r.w - 1.0,
                    r.y + 3.4,
                    egui::Align2::RIGHT_BOTTOM,
                ),
                (
                    end,
                    r.x + r.w - 1.0,
                    r.y + r.h - 1.6,
                    egui::Align2::RIGHT_BOTTOM,
                ),
            ]
        } else {
            [
                (start, r.x + 1.0, r.y + 3.4, egui::Align2::LEFT_BOTTOM),
                (end, r.x + r.w - 1.0, r.y + 3.4, egui::Align2::RIGHT_BOTTOM),
            ]
        };
        for (key, x, y, anchor) in ends {
            painter.text(
                t.to_screen(x, y),
                anchor,
                crate::i18n::t(key),
                font.clone(),
                END_LABEL,
            );
        }
    }
}

/// Ticks along each edge, thinned so they stay countable: at sixty-odd
/// intervals a ruled grid on a 100-unit face is a grey wash.
fn paint_grid(
    painter: &egui::Painter,
    face: &Face,
    rect: Box64,
    sheet: &Sheet,
    t: &Transform,
    space: Space,
    position: [f64; 3],
    axes: &[Vec<f64>; 3],
) {
    const TICK: f64 = 1.6;
    const MIN_GAP: f64 = 1.2;
    let stroke = Stroke::new(t.len(0.45).max(0.5), GRID);
    for which in [Which::H, Which::V] {
        let a = face.axis(which);
        let nodes = &axes[a.axis.adm_index()];
        if nodes.len() < 2 {
            continue;
        }
        let mut pts: Vec<f64> = nodes
            .iter()
            .map(|n| {
                let mut probe = position;
                probe[a.axis.adm_index()] = *n;
                let (x, y) = face_to_sheet(face, rect, probe, sheet, space);
                if which == Which::H { x } else { y }
            })
            .collect();
        pts.sort_by(f64::total_cmp);
        let tightest = pts
            .windows(2)
            .map(|w| w[1] - w[0])
            .fold(f64::INFINITY, f64::min);
        let step = (MIN_GAP / tightest.max(1e-6)).ceil().max(1.0) as usize;
        for v in pts.iter().step_by(step) {
            if which == Which::H {
                painter.line_segment(
                    [t.to_screen(*v, rect.y), t.to_screen(*v, rect.y + TICK)],
                    stroke,
                );
                painter.line_segment(
                    [
                        t.to_screen(*v, rect.y + rect.h),
                        t.to_screen(*v, rect.y + rect.h - TICK),
                    ],
                    stroke,
                );
            } else {
                painter.line_segment(
                    [t.to_screen(rect.x, *v), t.to_screen(rect.x + TICK, *v)],
                    stroke,
                );
                painter.line_segment(
                    [
                        t.to_screen(rect.x + rect.w, *v),
                        t.to_screen(rect.x + rect.w - TICK, *v),
                    ],
                    stroke,
                );
            }
        }
    }
}

fn paint_slider(
    painter: &egui::Painter,
    slider: &Slider,
    sheet: &Sheet,
    t: &Transform,
    space: Space,
    overlay: &Overlay<'_>,
    focused: bool,
) {
    let track = slider.track(sheet);
    let a = t.to_screen(track.x1, track.y1);
    let b = t.to_screen(track.x2, track.y2);
    let colour = if focused {
        Color32::from_rgba_premultiplied(120, 130, 140, 140)
    } else {
        TRACK
    };
    painter.line_segment([a, b], Stroke::new(t.len(0.7).max(0.7), colour));

    // Centre tick: the room's midpoint on this axis, so zero is findable.
    let mid_x = (track.x1 + track.x2) / 2.0;
    let mid_y = (track.y1 + track.y2) / 2.0;
    let (t1, t2) = if track.horizontal {
        ((mid_x, mid_y - 1.4), (mid_x, mid_y + 1.4))
    } else {
        ((mid_x - 1.4, mid_y), (mid_x + 1.4, mid_y))
    };
    painter.line_segment(
        [t.to_screen(t1.0, t1.1), t.to_screen(t2.0, t2.1)],
        Stroke::new(t.len(0.6).max(0.6), TICK),
    );

    let (cx, cy) = slider.thumb(sheet, overlay.position, space);
    painter.circle(
        t.to_screen(cx, cy),
        t.len(2.1),
        THUMB,
        Stroke::new(
            t.len(if focused { 0.9 } else { 0.5 }).max(0.5),
            if focused {
                crate::ui::theme::TEXT
            } else {
                Color32::from_rgba_premultiplied(8, 9, 13, 204)
            },
        ),
    );
}

/// egui has no dash array, so the segments are stepped by hand.
fn dashed_line(painter: &egui::Painter, from: Pos2, to: Pos2, stroke: Stroke, dash: f32) {
    let dash = dash.max(1.0);
    let delta = to - from;
    let length = delta.length();
    if length <= 0.0 {
        return;
    }
    let dir = delta / length;
    let mut at = 0.0;
    while at < length {
        let end = (at + dash).min(length);
        painter.line_segment([from + dir * at, from + dir * end], stroke);
        at = end + dash;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room() -> RoomRatio {
        RoomRatio {
            width: 1.0,
            length: 2.0,
            height: 1.0,
            rear: 1.0,
            lower: 0.5,
            center_blend: 0.5,
            scale_m: 1.0,
        }
    }

    #[test]
    fn the_room_projection_is_invertible_so_a_marker_lands_where_it_was_placed() {
        let r = room();
        let space = Space {
            adm_view: false,
            room: &r,
        };
        for pos in [
            [0.0, 0.0, 0.0],
            [-0.4, 0.35, 0.2],
            [1.0, -1.0, 0.75],
            [-1.0, 1.0, -1.0],
        ] {
            let back = space.to_adm(space.to_room(pos));
            for i in 0..3 {
                assert!((back[i] - pos[i]).abs() < 1e-6, "{pos:?} -> {back:?}");
            }
        }
    }

    #[test]
    fn the_mitre_cell_is_square_on_the_depth_span_so_the_45_degrees_carries_depth() {
        let r = room();
        let space = Space {
            adm_view: false,
            room: &r,
        };
        let sheet = layout(space.extent());
        assert!((sheet.mitre.w - sheet.mitre.h).abs() < 1e-9);
        assert!((sheet.mitre.w - sheet.rect(FaceId::Side).w).abs() < 1e-9);
        // The plan and the side view show the same depth at the same scale.
        assert!((sheet.rect(FaceId::Floor).h - sheet.rect(FaceId::Side).w).abs() < 1e-9);
        // The two elevations share their height, and the sheet is normalised.
        assert!((sheet.rect(FaceId::Front).h - sheet.rect(FaceId::Side).h).abs() < 1e-9);
        assert!((sheet.w.max(sheet.h) - 100.0).abs() < 1e-9);
    }

    #[test]
    fn the_adm_view_makes_every_face_square() {
        let r = room();
        let sheet = layout(
            Space {
                adm_view: true,
                room: &r,
            }
            .extent(),
        );
        for id in [FaceId::Floor, FaceId::Front, FaceId::Side] {
            let rect = sheet.rect(id);
            assert!((rect.w - rect.h).abs() < 1e-9, "face is not square");
        }
    }

    #[test]
    fn a_slider_writes_its_own_axis_and_leaves_the_other_two_alone() {
        let r = room();
        let space = Space {
            adm_view: true,
            room: &r,
        };
        let sheet = layout(space.extent());
        let start = [0.25, -0.5, 0.75];
        for slider in SLIDERS {
            let track = slider.track(&sheet);
            let moved = slider.value_at(&sheet, track.x2, track.y2, start, space);
            let index = slider.axis().axis.adm_index();
            for i in 0..3 {
                if i == index {
                    continue;
                }
                assert!((moved[i] - start[i]).abs() < 1e-9, "{moved:?}");
            }
        }
    }
}
