//! The web's inline SVG icons, drawn with the egui painter.
//!
//! The scene-effects bar is seven icons from the same stroke family (a 24-unit
//! box, 1.8-unit round strokes, `currentColor`). Pulling in an SVG rasteriser
//! for them would be a dependency heavier than the feature; drawing them with
//! glyphs from the bundled fonts is what made the bar look like a row of
//! typing mistakes. So the icons keep their SVG source — the same `d`
//! strings, rects and circles as `index.html` — and this module flattens that
//! geometry once into polylines and strokes or fills it at any size.
//!
//! Only what the icons use is supported: `rect` (with `rx`), `circle`,
//! `ellipse`, and paths made of `M L H V C Q T A Z` in both absolute and
//! relative form, with fill, stroke, per-shape opacity and a dash pattern.

use std::f32::consts::{PI, TAU};
use std::sync::OnceLock;

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, pos2, vec2};

/// One SVG element of an icon, in the icon's own view box.
#[derive(Clone, Copy)]
pub enum Prim {
    Path(&'static str),
    Circle {
        cx: f32,
        cy: f32,
        r: f32,
    },
    Ellipse {
        cx: f32,
        cy: f32,
        rx: f32,
        ry: f32,
    },
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        rx: f32,
    },
}

/// How an element is painted.
#[derive(Clone, Copy)]
pub struct Paint {
    pub prim: Prim,
    pub stroke: bool,
    pub fill: bool,
    pub opacity: f32,
    /// `stroke-dasharray`: dash and gap lengths, in view-box units.
    pub dash: Option<(f32, f32)>,
}

const fn stroke(prim: Prim) -> Paint {
    Paint {
        prim,
        stroke: true,
        fill: false,
        opacity: 1.0,
        dash: None,
    }
}

const fn fill(prim: Prim) -> Paint {
    Paint {
        prim,
        stroke: false,
        fill: true,
        opacity: 1.0,
        dash: None,
    }
}

const fn fill_at(prim: Prim, opacity: f32) -> Paint {
    Paint {
        prim,
        stroke: false,
        fill: true,
        opacity,
        dash: None,
    }
}

/// An icon: its view box size, stroke width, and elements.
pub struct Icon {
    pub view: f32,
    pub stroke_width: f32,
    pub paints: &'static [Paint],
}

macro_rules! icon {
    ($name:ident, $view:expr, $sw:expr, [$($p:expr),* $(,)?]) => {
        pub const $name: Icon = Icon { view: $view, stroke_width: $sw, paints: &[$($p),*] };
    };
}

use Prim::{Circle, Ellipse, Path, Rect as R};

// The display settings panel's button: lucide's `sliders-horizontal`, in
// the same 24-unit, 1.8-stroke family as the bar's other icons.
icon!(
    SETTINGS,
    24.0,
    1.8,
    [stroke(Path(
        "M21 4h-7M10 4H3M21 12h-9M8 12H3M21 20h-5M12 20H3M14 2v4M8 10v4M16 18v4"
    ))]
);
// `#fxGridBtn`
icon!(
    GRID,
    24.0,
    1.8,
    [
        stroke(R {
            x: 3.0,
            y: 3.0,
            w: 18.0,
            h: 18.0,
            rx: 2.0
        }),
        stroke(Path("M3 9h18M3 15h18M9 3v18M15 3v18")),
    ]
);
// `#fxObjectsBtn`
icon!(
    OBJECTS,
    24.0,
    1.8,
    [
        stroke(Circle {
            cx: 12.0,
            cy: 12.0,
            r: 9.0
        }),
        fill(Circle {
            cx: 12.0,
            cy: 12.0,
            r: 2.6
        }),
    ]
);
// `#fxLabelsBtn`
icon!(
    LABELS,
    24.0,
    1.8,
    [
        stroke(Path(
            "M12.586 2.586A2 2 0 0 0 11.172 2H4a2 2 0 0 0-2 2v7.172a2 2 0 0 0 .586 1.414l8.704 8.704a2.426 2.426 0 0 0 3.42 0l6.58-6.58a2.426 2.426 0 0 0 0-3.42z"
        )),
        fill(Circle {
            cx: 7.5,
            cy: 7.5,
            r: 1.5
        }),
    ]
);
// `#fxTrailsBtn`
icon!(
    TRAILS,
    24.0,
    1.8,
    [
        stroke(Circle {
            cx: 6.0,
            cy: 19.0,
            r: 3.0
        }),
        stroke(Path("M9 19h8.5a3.5 3.5 0 0 0 0-7h-11a3.5 3.5 0 0 1 0-7H15")),
        stroke(Circle {
            cx: 18.0,
            cy: 5.0,
            r: 3.0
        }),
    ]
);
// `#fxFieldBtn`
icon!(
    FIELD,
    24.0,
    1.8,
    [
        stroke(Path("M4.9 19.1C1 15.2 1 8.8 4.9 4.9")),
        stroke(Path("M7.8 16.2c-2.3-2.3-2.3-6.1 0-8.5")),
        fill(Circle {
            cx: 12.0,
            cy: 12.0,
            r: 2.0
        }),
        stroke(Path("M16.2 7.8c2.3 2.3 2.3 6.1 0 8.5")),
        stroke(Path("M19.1 4.9C23 8.8 23 15.1 19.1 19")),
    ]
);
// `#fxHeatmapBtn`
icon!(
    HEATMAP,
    24.0,
    1.8,
    [
        fill_at(
            R {
                x: 3.0,
                y: 3.0,
                w: 7.5,
                h: 7.5,
                rx: 1.2
            },
            0.95
        ),
        fill_at(
            R {
                x: 13.5,
                y: 3.0,
                w: 7.5,
                h: 7.5,
                rx: 1.2
            },
            0.4
        ),
        fill_at(
            R {
                x: 3.0,
                y: 13.5,
                w: 7.5,
                h: 7.5,
                rx: 1.2
            },
            0.55
        ),
        fill_at(
            R {
                x: 13.5,
                y: 13.5,
                w: 7.5,
                h: 7.5,
                rx: 1.2
            },
            0.85
        ),
    ]
);
// `#fxMpvBtn`
icon!(
    MPV,
    24.0,
    1.8,
    [
        stroke(R {
            x: 2.0,
            y: 3.0,
            w: 20.0,
            h: 14.0,
            rx: 2.0
        }),
        stroke(Path("M8 21h8M12 17v4")),
        fill(Path("M10 8.4v4l3.6-2z")),
    ]
);
// `.fx-caret`
icon!(CARET, 12.0, 2.0, [stroke(Path("M2 6l4-4 4 4"))]);
// Object display modes (`#fxObjectsMenu`).
icon!(
    MODE_CIRCLE,
    24.0,
    1.8,
    [stroke(Circle {
        cx: 12.0,
        cy: 12.0,
        r: 8.0
    })]
);
icon!(
    MODE_TRANSPARENT,
    24.0,
    1.8,
    [
        stroke(Circle {
            cx: 12.0,
            cy: 12.0,
            r: 8.0
        }),
        stroke(Ellipse {
            cx: 12.0,
            cy: 12.0,
            rx: 3.4,
            ry: 8.0
        }),
        stroke(Ellipse {
            cx: 12.0,
            cy: 12.0,
            rx: 8.0,
            ry: 3.4
        }),
    ]
);
icon!(
    MODE_DIFFUSE,
    24.0,
    1.8,
    [
        fill(Circle {
            cx: 12.0,
            cy: 12.0,
            r: 4.0
        }),
        Paint {
            prim: Circle {
                cx: 12.0,
                cy: 12.0,
                r: 8.0
            },
            stroke: true,
            fill: false,
            opacity: 1.0,
            dash: Some((1.6, 2.6)),
        },
    ]
);
// Trail modes (`#fxTrailsMenu`).
icon!(
    TRAIL_DIFFUSE,
    24.0,
    1.8,
    [
        fill_at(
            Circle {
                cx: 5.0,
                cy: 15.0,
                r: 1.2
            },
            0.4
        ),
        fill_at(
            Circle {
                cx: 10.0,
                cy: 13.0,
                r: 1.8
            },
            0.62
        ),
        fill_at(
            Circle {
                cx: 15.0,
                cy: 11.0,
                r: 2.4
            },
            0.82
        ),
        fill(Circle {
            cx: 20.0,
            cy: 9.0,
            r: 3.0
        }),
    ]
);
icon!(
    TRAIL_LINE,
    24.0,
    1.8,
    [stroke(Path("M3 18Q9 4 12 11T21 6"))]
);

/// A flattened element: its sub-paths, each a polyline and whether it closes.
type Flat = Vec<(Vec<Pos2>, bool)>;

/// How finely curves are cut, in view-box units. At a 20-point icon a unit is
/// under a point, so a quarter of one is well below what the eye resolves.
const TOLERANCE: f32 = 0.25;

fn flatten(prim: Prim) -> Flat {
    match prim {
        Prim::Path(d) => flatten_path(d),
        Prim::Circle { cx, cy, r } => vec![(ellipse_points(cx, cy, r, r), true)],
        Prim::Ellipse { cx, cy, rx, ry } => vec![(ellipse_points(cx, cy, rx, ry), true)],
        Prim::Rect { x, y, w, h, rx } => vec![(rounded_rect_points(x, y, w, h, rx), true)],
    }
}

fn segments_for(radius: f32, sweep: f32) -> usize {
    // Enough segments that the chord deviates from the arc by < TOLERANCE.
    let r = radius.max(0.01);
    let step = 2.0 * (1.0 - TOLERANCE.min(r) / r).clamp(-1.0, 1.0).acos();
    ((sweep.abs() / step.max(1e-3)).ceil() as usize).clamp(4, 256)
}

fn ellipse_points(cx: f32, cy: f32, rx: f32, ry: f32) -> Vec<Pos2> {
    let n = segments_for(rx.max(ry), TAU);
    (0..n)
        .map(|i| {
            let a = TAU * i as f32 / n as f32;
            pos2(cx + rx * a.cos(), cy + ry * a.sin())
        })
        .collect()
}

fn rounded_rect_points(x: f32, y: f32, w: f32, h: f32, rx: f32) -> Vec<Pos2> {
    let r = rx.clamp(0.0, w.min(h) / 2.0);
    if r <= 0.0 {
        return vec![
            pos2(x, y),
            pos2(x + w, y),
            pos2(x + w, y + h),
            pos2(x, y + h),
        ];
    }
    let mut pts = Vec::new();
    let n = segments_for(r, PI / 2.0);
    // Corners clockwise from the top-right, each a quarter arc.
    let corners = [
        (x + w - r, y + r, -PI / 2.0),
        (x + w - r, y + h - r, 0.0),
        (x + r, y + h - r, PI / 2.0),
        (x + r, y + r, PI),
    ];
    for (ccx, ccy, start) in corners {
        for i in 0..=n {
            let a = start + (PI / 2.0) * i as f32 / n as f32;
            pts.push(pos2(ccx + r * a.cos(), ccy + r * a.sin()));
        }
    }
    pts
}

/// Tokenise a path's numbers: signs and a second decimal point both start a
/// new number, as the SVG grammar allows (`-2.3-2.3`, `.586`, `1.2.5`).
fn numbers(s: &str) -> Vec<f32> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut Vec<f32>| {
        if !cur.is_empty() {
            if let Ok(v) = cur.parse() {
                out.push(v);
            }
            cur.clear();
        }
    };
    for c in s.chars() {
        match c {
            '0'..='9' => cur.push(c),
            '.' => {
                if cur.contains('.') {
                    flush(&mut cur, &mut out);
                }
                cur.push(c);
            }
            '-' | '+' => {
                if !cur.is_empty() && !cur.ends_with('e') && !cur.ends_with('E') {
                    flush(&mut cur, &mut out);
                }
                cur.push(c);
            }
            'e' | 'E' => cur.push(c),
            _ => flush(&mut cur, &mut out),
        }
    }
    flush(&mut cur, &mut out);
    out
}

fn cubic(out: &mut Vec<Pos2>, p0: Pos2, p1: Pos2, p2: Pos2, p3: Pos2) {
    let len = (p1 - p0).length() + (p2 - p1).length() + (p3 - p2).length();
    let n = ((len / TOLERANCE).sqrt().ceil() as usize).clamp(2, 64);
    for i in 1..=n {
        let t = i as f32 / n as f32;
        let u = 1.0 - t;
        let p = p0.to_vec2() * (u * u * u)
            + p1.to_vec2() * (3.0 * u * u * t)
            + p2.to_vec2() * (3.0 * u * t * t)
            + p3.to_vec2() * (t * t * t);
        out.push(p.to_pos2());
    }
}

fn quad(out: &mut Vec<Pos2>, p0: Pos2, p1: Pos2, p2: Pos2) {
    let len = (p1 - p0).length() + (p2 - p1).length();
    let n = ((len / TOLERANCE).sqrt().ceil() as usize).clamp(2, 64);
    for i in 1..=n {
        let t = i as f32 / n as f32;
        let u = 1.0 - t;
        let p = p0.to_vec2() * (u * u) + p1.to_vec2() * (2.0 * u * t) + p2.to_vec2() * (t * t);
        out.push(p.to_pos2());
    }
}

/// SVG 1.1 F.6.5: an endpoint arc to its centre form, then flattened.
#[allow(clippy::too_many_arguments)]
fn arc(
    out: &mut Vec<Pos2>,
    p0: Pos2,
    rx: f32,
    ry: f32,
    phi_deg: f32,
    large: bool,
    sweep: bool,
    p1: Pos2,
) {
    if (p1 - p0).length() < 1e-6 {
        return;
    }
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if rx < 1e-6 || ry < 1e-6 {
        out.push(p1);
        return;
    }
    let phi = phi_deg.to_radians();
    let (cp, sp) = (phi.cos(), phi.sin());
    let dx = (p0.x - p1.x) / 2.0;
    let dy = (p0.y - p1.y) / 2.0;
    let x1 = cp * dx + sp * dy;
    let y1 = -sp * dx + cp * dy;
    // Radii too small for the endpoints are scaled up, as the spec says.
    let lambda = (x1 * x1) / (rx * rx) + (y1 * y1) / (ry * ry);
    if lambda > 1.0 {
        let s = lambda.sqrt();
        rx *= s;
        ry *= s;
    }
    let num = rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1;
    let den = rx * rx * y1 * y1 + ry * ry * x1 * x1;
    let mut coef = (num / den).max(0.0).sqrt();
    if large == sweep {
        coef = -coef;
    }
    let cx1 = coef * (rx * y1 / ry);
    let cy1 = coef * -(ry * x1 / rx);
    let cx = cp * cx1 - sp * cy1 + (p0.x + p1.x) / 2.0;
    let cy = sp * cx1 + cp * cy1 + (p0.y + p1.y) / 2.0;
    let angle = |ux: f32, uy: f32, vx: f32, vy: f32| {
        let a = (ux * vy - uy * vx).atan2(ux * vx + uy * vy);
        a
    };
    let theta1 = angle(1.0, 0.0, (x1 - cx1) / rx, (y1 - cy1) / ry);
    let mut delta = angle(
        (x1 - cx1) / rx,
        (y1 - cy1) / ry,
        (-x1 - cx1) / rx,
        (-y1 - cy1) / ry,
    );
    if !sweep && delta > 0.0 {
        delta -= TAU;
    } else if sweep && delta < 0.0 {
        delta += TAU;
    }
    let n = segments_for(rx.max(ry), delta);
    for i in 1..=n {
        let t = theta1 + delta * i as f32 / n as f32;
        let (x, y) = (rx * t.cos(), ry * t.sin());
        out.push(pos2(cp * x - sp * y + cx, sp * x + cp * y + cy));
    }
    // Land exactly on the endpoint the path names.
    if let Some(last) = out.last_mut() {
        *last = p1;
    }
}

fn flatten_path(d: &str) -> Flat {
    let mut subpaths: Flat = Vec::new();
    let mut cur: Vec<Pos2> = Vec::new();
    let mut pen = pos2(0.0, 0.0);
    let mut start = pen;
    let mut last_quad_ctrl: Option<Pos2> = None;
    // Split into (command, arguments) runs.
    let mut runs: Vec<(char, Vec<f32>)> = Vec::new();
    let mut i = 0;
    let bytes: Vec<char> = d.chars().collect();
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_alphabetic() && c != 'e' && c != 'E' {
            let mut j = i + 1;
            while j < bytes.len()
                && !(bytes[j].is_ascii_alphabetic() && bytes[j] != 'e' && bytes[j] != 'E')
            {
                j += 1;
            }
            let args: String = bytes[i + 1..j].iter().collect();
            runs.push((c, numbers(&args)));
            i = j;
        } else {
            i += 1;
        }
    }
    let finish = |cur: &mut Vec<Pos2>, subpaths: &mut Flat, closed: bool| {
        if cur.len() > 1 {
            subpaths.push((std::mem::take(cur), closed));
        } else {
            cur.clear();
        }
    };
    for (cmd, args) in runs {
        let rel = cmd.is_ascii_lowercase();
        let at = |pen: Pos2, x: f32, y: f32| if rel { pen + vec2(x, y) } else { pos2(x, y) };
        match cmd.to_ascii_uppercase() {
            'M' => {
                finish(&mut cur, &mut subpaths, false);
                for (k, pair) in args.chunks_exact(2).enumerate() {
                    pen = at(pen, pair[0], pair[1]);
                    if k == 0 {
                        start = pen;
                    }
                    cur.push(pen);
                }
                last_quad_ctrl = None;
            }
            'L' => {
                for pair in args.chunks_exact(2) {
                    pen = at(pen, pair[0], pair[1]);
                    cur.push(pen);
                }
                last_quad_ctrl = None;
            }
            'H' => {
                for x in &args {
                    pen = if rel {
                        pos2(pen.x + x, pen.y)
                    } else {
                        pos2(*x, pen.y)
                    };
                    cur.push(pen);
                }
                last_quad_ctrl = None;
            }
            'V' => {
                for y in &args {
                    pen = if rel {
                        pos2(pen.x, pen.y + y)
                    } else {
                        pos2(pen.x, *y)
                    };
                    cur.push(pen);
                }
                last_quad_ctrl = None;
            }
            'C' => {
                for c in args.chunks_exact(6) {
                    let (p1, p2, p3) = (
                        at(pen, c[0], c[1]),
                        at(pen, c[2], c[3]),
                        at(pen, c[4], c[5]),
                    );
                    cubic(&mut cur, pen, p1, p2, p3);
                    pen = p3;
                }
                last_quad_ctrl = None;
            }
            'Q' => {
                for c in args.chunks_exact(4) {
                    let (p1, p2) = (at(pen, c[0], c[1]), at(pen, c[2], c[3]));
                    quad(&mut cur, pen, p1, p2);
                    last_quad_ctrl = Some(p1);
                    pen = p2;
                }
            }
            'T' => {
                for c in args.chunks_exact(2) {
                    // The control point reflects the previous one through the pen.
                    let ctrl = last_quad_ctrl.map_or(pen, |q| pen + (pen - q));
                    let p2 = at(pen, c[0], c[1]);
                    quad(&mut cur, pen, ctrl, p2);
                    last_quad_ctrl = Some(ctrl);
                    pen = p2;
                }
            }
            'A' => {
                for c in args.chunks_exact(7) {
                    let p1 = at(pen, c[5], c[6]);
                    arc(
                        &mut cur,
                        pen,
                        c[0],
                        c[1],
                        c[2],
                        c[3] != 0.0,
                        c[4] != 0.0,
                        p1,
                    );
                    pen = p1;
                }
                last_quad_ctrl = None;
            }
            'Z' => {
                pen = start;
                finish(&mut cur, &mut subpaths, true);
                cur.push(pen);
                last_quad_ctrl = None;
            }
            _ => {}
        }
    }
    finish(&mut cur, &mut subpaths, false);
    subpaths
}

/// Cut a closed or open polyline into dashes of `on` length every `on + off`.
///
/// The pattern is walked with an explicit state — drawing or not, and how much
/// of the current dash or gap is left — rather than as the running length
/// modulo the period. The modulo version stalled: once the running length is
/// large next to a step, adding the step no longer changes it in `f32`, the
/// phase stops moving, and the loop never ends. Here every pass consumes either
/// the rest of the segment or the rest of the dash, and the dash is reset to a
/// positive length when it runs out, so every pass makes progress.
fn dashes(points: &[Pos2], closed: bool, on: f32, off: f32) -> Vec<Vec<Pos2>> {
    if on <= 0.0 || off < 0.0 {
        return vec![points.to_vec()];
    }
    let mut pts = points.to_vec();
    if closed && let Some(first) = pts.first().copied() {
        pts.push(first);
    }
    let mut out = Vec::new();
    let mut dash: Vec<Pos2> = Vec::new();
    let mut drawing = true;
    let mut left = on;
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let seg = (b - a).length();
        if seg <= f32::EPSILON {
            continue;
        }
        let mut s = 0.0f32;
        while seg - s > 1e-6 {
            let step = left.min(seg - s);
            let p0 = a + (b - a) * (s / seg);
            let p1 = a + (b - a) * ((s + step) / seg);
            if drawing {
                if dash.is_empty() {
                    dash.push(p0);
                }
                dash.push(p1);
            }
            s += step;
            left -= step;
            if left <= 1e-6 {
                if drawing && dash.len() > 1 {
                    out.push(std::mem::take(&mut dash));
                }
                dash.clear();
                drawing = !drawing;
                left = if drawing { on } else { off.max(1e-3) };
            }
        }
    }
    if dash.len() > 1 {
        out.push(dash);
    }
    out
}

/// Every icon's flattened geometry, built on first use.
fn geometry(icon: &Icon) -> &'static [Flat] {
    // One cache per icon, keyed by the icon's address: the icons are
    // `const`s, so each has a single static location for the program's life.
    static CACHE: OnceLock<std::sync::Mutex<Vec<(usize, &'static [Flat])>>> = OnceLock::new();
    let key = icon.paints.as_ptr() as usize;
    let cache = CACHE.get_or_init(Default::default);
    let mut cache = cache.lock().unwrap();
    if let Some((_, flat)) = cache.iter().find(|(k, _)| *k == key) {
        return flat;
    }
    let flat: Vec<Flat> = icon.paints.iter().map(|p| flatten(p.prim)).collect();
    let flat: &'static [Flat] = Box::leak(flat.into_boxed_slice());
    cache.push((key, flat));
    flat
}

/// Paint `icon` into `rect` in `colour`, as the browser would at that size.
pub fn paint(painter: &Painter, rect: Rect, icon: &Icon, colour: Color32) {
    let scale = rect.width().min(rect.height()) / icon.view;
    let origin = rect.center() - vec2(icon.view, icon.view) * scale / 2.0;
    let map = |p: &Pos2| origin + p.to_vec2() * scale;
    let width = icon.stroke_width * scale;
    for (paint, flat) in icon.paints.iter().zip(geometry(icon)) {
        let c = if paint.opacity < 1.0 {
            colour.gamma_multiply(paint.opacity)
        } else {
            colour
        };
        if paint.fill {
            match paint.prim {
                // Circles and rounded rects fill exactly with the painter's own
                // shapes; the one filled path is a convex triangle.
                Prim::Circle { cx, cy, r } => {
                    painter.circle_filled(map(&pos2(cx, cy)), r * scale, c);
                }
                Prim::Rect { x, y, w, h, rx } => {
                    painter.rect_filled(
                        Rect::from_min_size(map(&pos2(x, y)), vec2(w, h) * scale),
                        rx * scale,
                        c,
                    );
                }
                _ => {
                    for (pts, _) in flat {
                        painter.add(Shape::convex_polygon(
                            pts.iter().map(map).collect(),
                            c,
                            Stroke::NONE,
                        ));
                    }
                }
            }
        }
        if paint.stroke {
            let stroke = Stroke::new(width, c);
            for (pts, closed) in flat {
                let mapped: Vec<Pos2> = pts.iter().map(map).collect();
                match paint.dash {
                    Some((on, off)) => {
                        for dash in dashes(&mapped, *closed, on * scale, off * scale) {
                            painter.add(Shape::line(dash, stroke));
                        }
                    }
                    None if *closed => {
                        painter.add(Shape::closed_line(mapped, stroke));
                    }
                    None => {
                        // Round caps, as `stroke-linecap: round`: a dot of the
                        // stroke's width at each open end.
                        if let (Some(a), Some(b)) = (mapped.first(), mapped.last()) {
                            painter.circle_filled(*a, width / 2.0, c);
                            painter.circle_filled(*b, width / 2.0, c);
                        }
                        painter.add(Shape::line(mapped, stroke));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_split_on_signs_and_second_points() {
        assert_eq!(
            numbers("-2.3-2.3-2.3-6.1 0-8.5"),
            vec![-2.3, -2.3, -2.3, -6.1, 0.0, -8.5]
        );
        assert_eq!(
            numbers("0 0 0 .586 1.414"),
            vec![0.0, 0.0, 0.0, 0.586, 1.414]
        );
        assert_eq!(numbers("1.2.5"), vec![1.2, 0.5]);
    }

    /// `M3 9h18M3 15h18M9 3v18M15 3v18` is four separate strokes of the grid.
    #[test]
    fn a_path_splits_into_its_subpaths() {
        let flat = flatten_path("M3 9h18M3 15h18M9 3v18M15 3v18");
        assert_eq!(flat.len(), 4);
        assert_eq!(flat[0].0, vec![pos2(3.0, 9.0), pos2(21.0, 9.0)]);
        assert_eq!(flat[3].0, vec![pos2(15.0, 3.0), pos2(15.0, 21.0)]);
    }

    /// Relative commands move from the pen, and `z` closes the sub-path.
    #[test]
    fn relative_commands_and_close() {
        let flat = flatten_path("M10 8.4v4l3.6-2z");
        assert_eq!(flat.len(), 1);
        let (pts, closed) = &flat[0];
        assert!(*closed);
        assert_eq!(pts[0], pos2(10.0, 8.4));
        assert_eq!(pts[1], pos2(10.0, 12.4));
        assert!((pts[2] - pos2(13.6, 10.4)).length() < 1e-5);
    }

    /// A half-circle arc from (0,0) to (10,0) with radius 5 passes through the
    /// point 5 away from the chord, on the side the sweep flag picks, and lands
    /// on the named endpoint.
    #[test]
    fn an_arc_follows_its_circle() {
        let mut out = vec![pos2(0.0, 0.0)];
        arc(
            &mut out,
            pos2(0.0, 0.0),
            5.0,
            5.0,
            0.0,
            false,
            true,
            pos2(10.0, 0.0),
        );
        assert_eq!(*out.last().unwrap(), pos2(10.0, 0.0));
        let extreme = out
            .iter()
            .map(|p| p.y)
            .fold(0.0f32, |a, y| if y.abs() > a.abs() { y } else { a });
        // The apex need not be a vertex: flattening only promises the chords
        // stay within TOLERANCE of the curve.
        assert!(
            (extreme.abs() - 5.0).abs() <= TOLERANCE + 1e-3,
            "the arc bulges {extreme}"
        );
        for p in &out {
            assert!(
                ((*p - pos2(5.0, 0.0)).length() - 5.0).abs() < 0.3,
                "{p:?} is off the circle"
            );
        }
    }

    /// `T` reflects the previous control point: the smooth curve of the "line"
    /// trail icon must not kink where its two halves meet.
    #[test]
    fn a_smooth_quadratic_continues_the_tangent() {
        let flat = flatten_path("M3 18Q9 4 12 11T21 6");
        let pts = &flat[0].0;
        let join = pts
            .iter()
            .position(|p| (*p - pos2(12.0, 11.0)).length() < 1e-4)
            .unwrap();
        let before = pts[join] - pts[join - 1];
        let after = pts[join + 1] - pts[join];
        let cos = before.normalized().dot(after.normalized());
        assert!(cos > 0.95, "the curve kinks at the join: cos {cos}");
    }

    /// Every icon the bar uses flattens to something drawable.
    #[test]
    fn every_icon_has_geometry() {
        for icon in [
            &GRID,
            &OBJECTS,
            &LABELS,
            &TRAILS,
            &FIELD,
            &HEATMAP,
            &MPV,
            &CARET,
            &MODE_CIRCLE,
            &MODE_TRANSPARENT,
            &MODE_DIFFUSE,
            &TRAIL_DIFFUSE,
            &TRAIL_LINE,
        ] {
            for flat in geometry(icon) {
                assert!(!flat.is_empty());
                for (pts, _) in flat {
                    assert!(pts.len() >= 2);
                    assert!(pts.iter().all(|p| p.x.is_finite() && p.y.is_finite()));
                }
            }
        }
    }

    /// A dashed circle is broken into dashes, none longer than the dash.
    #[test]
    fn dashes_respect_the_pattern() {
        let circle = ellipse_points(12.0, 12.0, 8.0, 8.0);
        let pieces = dashes(&circle, true, 1.6, 2.6);
        let circumference = TAU * 8.0;
        let expected = (circumference / 4.2).floor() as usize;
        assert!(
            pieces.len() >= expected - 1 && pieces.len() <= expected + 1,
            "{} dashes",
            pieces.len()
        );
        for d in &pieces {
            let len: f32 = d.windows(2).map(|w| (w[1] - w[0]).length()).sum();
            assert!(len <= 1.6 + 1e-3, "a dash of {len}");
        }
    }
}
