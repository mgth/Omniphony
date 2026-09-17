//! The audio level meter (`.meter-bar`, `.meter-fill`, `.meter-peak` and the
//! `.level-meter::after` clipping zone of `app.css`).
//!
//! The web draws it with one CSS gradient painted across the **whole** track
//! and then clipped: the colour at a point is decided by where that point sits
//! on the scale, never by how long the bar happens to be. A level of -6 dBFS is
//! amber at its tip whether it is rising or falling. Reproducing that is the
//! whole reason this is a component rather than a filled rectangle: egui has no
//! gradient primitive, so the track is built as a `Mesh` whose vertex colours
//! are sampled from the same stops, and the fill, the peak slice and the
//! clipping zone are the same mesh under different clip rects.
//!
//! The silhouette is a capsule (`border-radius: 999px` on a 6 px bar), so the
//! mesh follows a rounded profile instead of a plain rectangle — otherwise the
//! gradient would spill square corners over the rounded track underneath.

use egui::epaint::{Mesh, Vertex, WHITE_UV};
use egui::{Align2, Color32, Painter, Rect, Response, Sense, Ui, pos2, vec2};

use crate::host::peak_hold::{METER_DB_MAX, METER_DB_MIN};
use crate::ui::theme;

/// The stops of `linear-gradient(90deg, #4dd7ff 0%, #7bff6a 60%, #ffd13a 82%,
/// #ff5d5d 100%)`.
const STOPS: [(f32, [u8; 3]); 4] = [
    (0.00, [0x4d, 0xd7, 0xff]),
    (0.60, [0x7b, 0xff, 0x6a]),
    (0.82, [0xff, 0xd1, 0x3a]),
    (1.00, [0xff, 0x5d, 0x5d]),
];

/// The track is the same gradient at 18 % — the scale stays readable where the
/// level has not reached yet.
const TRACK_ALPHA: u8 = 46; // 0.18 × 255
/// `rgba(255, 80, 80, 0.32)` over the over-0 dBFS headroom.
const ZONE: Color32 = Color32::from_rgba_premultiplied(82, 26, 26, 82);
/// The 1 px `border-left` that opens the zone.
const ZONE_EDGE: Color32 = Color32::from_rgba_premultiplied(191, 173, 173, 191);
/// A held peak past 0 dBFS stops being a slice of the scale and turns solid.
const PEAK_OVER: Color32 = Color32::from_rgb(0xff, 0x3b, 0x3b);
/// Half-width of the peak cursor, in points.
const PEAK_HALF: f32 = 1.0;
/// The bar's own height, `height: 6px`.
pub const HEIGHT: f32 = 6.0;

/// Where 0 dBFS sits on the scale — the start of the clipping zone (90.9 %).
pub fn clip_start() -> f32 {
    ((0.0 - METER_DB_MIN) / (METER_DB_MAX - METER_DB_MIN)) as f32
}

/// The colour at `t` on a stop list, with the alpha the caller wants. CSS
/// interpolates its stops in sRGB, so this does too.
fn stop_colour(stops: &[(f32, [u8; 3])], t: f32, alpha: u8) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let mut span = (stops[0], stops[stops.len() - 1]);
    for pair in stops.windows(2) {
        if t >= pair[0].0 {
            span = (pair[0], pair[1]);
        }
    }
    let ((t0, a), (t1, b)) = span;
    let k = if t1 > t0 { (t - t0) / (t1 - t0) } else { 0.0 };
    let k = k.clamp(0.0, 1.0);
    let lerp = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * k).round() as u8;
    Color32::from_rgba_unmultiplied(lerp(a[0], b[0]), lerp(a[1], b[1]), lerp(a[2], b[2]), alpha)
}

/// Half the capsule's height at `x`: the full radius along the straight middle,
/// and the circle's own profile inside either cap.
fn half_height(rect: &Rect, x: f32) -> f32 {
    let r = rect.height() * 0.5;
    let dx = if x < rect.left() + r {
        rect.left() + r - x
    } else if x > rect.right() - r {
        x - (rect.right() - r)
    } else {
        0.0
    };
    (r * r - dx * dx).max(0.0).sqrt()
}

/// The x positions the mesh needs vertices at: both caps sampled finely enough
/// to read as round, plus every gradient stop so no stop is crossed by
/// interpolation.
fn sample_xs(rect: &Rect, stops: &[(f32, [u8; 3])]) -> Vec<f32> {
    let r = rect.height() * 0.5;
    let mut xs: Vec<f32> = Vec::with_capacity(24);
    let cap_steps = 6;
    for i in 0..=cap_steps {
        let k = i as f32 / cap_steps as f32;
        xs.push(rect.left() + r * k);
        xs.push(rect.right() - r * (1.0 - k));
    }
    for (t, _) in stops {
        xs.push(rect.left() + rect.width() * t);
    }
    xs.retain(|x| *x >= rect.left() && *x <= rect.right());
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    xs.dedup_by(|a, b| (*a - *b).abs() < 0.01);
    xs
}

/// The capsule as a strip of quads. `tint` paints one flat colour (the clipping
/// zone, a clipped peak); without it every vertex takes the gradient.
fn capsule(rect: &Rect, alpha: u8, tint: Option<Color32>) -> Mesh {
    strip_with(rect, &STOPS, alpha, tint)
}

/// The same capsule painted from another stop list (the contribution overlay).
fn strip(rect: &Rect, stops: &[(f32, [u8; 3])], alpha: u8) -> Mesh {
    strip_with(rect, stops, alpha, None)
}

fn strip_with(rect: &Rect, stops: &[(f32, [u8; 3])], alpha: u8, tint: Option<Color32>) -> Mesh {
    let mut mesh = Mesh::default();
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return mesh;
    }
    let xs = sample_xs(rect, stops);
    let colour = |x: f32| match tint {
        Some(c) => c,
        None => stop_colour(stops, (x - rect.left()) / rect.width(), alpha),
    };
    for x in &xs {
        let h = half_height(rect, *x);
        let c = colour(*x);
        let base = mesh.vertices.len() as u32;
        mesh.vertices.push(Vertex {
            pos: pos2(*x, rect.center().y - h),
            uv: WHITE_UV,
            color: c,
        });
        mesh.vertices.push(Vertex {
            pos: pos2(*x, rect.center().y + h),
            uv: WHITE_UV,
            color: c,
        });
        if base >= 2 {
            mesh.add_triangle(base - 2, base - 1, base);
            mesh.add_triangle(base - 1, base, base + 1);
        }
    }
    mesh
}

/// Paint the capsule clipped to `window` — the CSS `clip-path: inset(...)`, so
/// the rounded ends survive while the cut itself is a straight vertical edge.
fn clipped(painter: &Painter, rect: &Rect, window: Rect, alpha: u8, tint: Option<Color32>) {
    if window.width() <= 0.0 {
        return;
    }
    painter
        .with_clip_rect(window.intersect(painter.clip_rect()))
        .add(capsule(rect, alpha, tint));
}

/// The contribution overlay's own gradient, `.meter-fill.contribution`.
const CONTRIB: [(f32, [u8; 3]); 2] = [(0.0, [0x8a, 0xf0, 0xff]), (1.0, [0xff, 0xe2, 0x7a])];
/// What the level drops to while a contribution is painted over it, so the two
/// are read as foreground and background rather than as one bar.
const UNDER_CONTRIB: u8 = 97; // 0.38 × 255

/// A meter that takes its row with the reading in a fixed box at the right
/// end — the shape `.master-header` gives the master meter, and the latency
/// section's grid its own. `bar` draws the meter at the width handed to it.
///
/// The box is fixed on purpose: one sized to the reading would drag the bar a
/// few points left and right every time the number gained or lost a digit,
/// which is the one thing a meter must not do. `advances` is that width in
/// monospace advances — pick it from the widest reading the caller can
/// produce. A longer one is clipped to the box rather than let over the bar.
pub fn row_with_readout(ui: &mut Ui, advances: f32, reading: &str, bar: impl FnOnce(&mut Ui, f32)) {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let (advance, row_height) = ui.fonts_mut(|f| (f.glyph_width(&font, '0'), f.row_height(&font)));
    let box_width = advance * advances;
    bar(
        ui,
        ui.available_width() - box_width - ui.spacing().item_spacing.x,
    );
    let (rect, _) = ui.allocate_exact_size(vec2(box_width, row_height), Sense::hover());
    ui.painter()
        .with_clip_rect(rect.intersect(ui.clip_rect()))
        .text(
            rect.right_center(),
            Align2::RIGHT_CENTER,
            reading,
            font,
            theme::TEXT_STRONG,
        );
}

/// `.meter-bar.level-meter` at a width the caller decides: `level` and `peak`
/// are already mapped to 0..1 by the caller (`meter_fraction`), `clipping`
/// says the held peak crossed 0 dBFS, and `contribution` is the selected
/// object's share of this row, on the same scale, painted over the level.
///
/// Every caller shares its row with something — a list row's grid column, a
/// section header's readout — so what is left of the row is the caller's to
/// work out. A width under [`HEIGHT`] is floored there, so a panel dragged to
/// its narrowest shortens the bar instead of inverting it.
pub fn level_meter_sized(
    ui: &mut Ui,
    width: f32,
    level: f32,
    peak: Option<f32>,
    clipping: bool,
    contribution: Option<f32>,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(width.max(HEIGHT), HEIGHT), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter();
    let x_at = |t: f32| rect.left() + rect.width() * t.clamp(0.0, 1.0);

    painter.add(capsule(&rect, TRACK_ALPHA, None));

    let level = level.clamp(0.0, 1.0);
    if level > 0.0 {
        let fill = Rect::from_min_max(rect.left_top(), pos2(x_at(level), rect.bottom()));
        let alpha = if contribution.is_some() {
            UNDER_CONTRIB
        } else {
            255
        };
        clipped(&painter, &rect, fill, alpha, None);
    }
    if let Some(share) = contribution {
        let share = share.clamp(0.0, 1.0);
        if share > 0.0 {
            let over = Rect::from_min_max(rect.left_top(), pos2(x_at(share), rect.bottom()));
            painter
                .with_clip_rect(over.intersect(painter.clip_rect()))
                .add(strip(&rect, &CONTRIB, 235));
        }
    }

    // The headroom above 0 dBFS, drawn over the level: a peak that reaches it
    // is clipping, and the web makes that unmissable rather than letting the
    // bar saturate at the top of the scale.
    let clip_x = x_at(clip_start());
    let zone = Rect::from_min_max(pos2(clip_x, rect.top()), rect.right_bottom());
    clipped(&painter, &rect, zone, 255, Some(ZONE));
    painter.vline(clip_x, rect.y_range(), egui::Stroke::new(1.0, ZONE_EDGE));

    if let Some(peak) = peak {
        let x = x_at(peak);
        let slice = Rect::from_min_max(
            pos2(x - PEAK_HALF, rect.top()),
            pos2(x + PEAK_HALF, rect.bottom()),
        );
        if clipping {
            // `box-shadow: 0 0 6px rgba(255, 59, 59, 0.9)`, as close as a
            // painter without shadows gets: one faint wider pass underneath.
            let glow = slice.expand2(vec2(2.0, 0.0));
            clipped(
                &painter,
                &rect,
                glow,
                255,
                Some(Color32::from_rgba_unmultiplied(255, 59, 59, 90)),
            );
            clipped(&painter, &rect, slice, 255, Some(PEAK_OVER));
        } else {
            clipped(&painter, &rect, slice, 255, None);
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The level scale's own colour at `t`, which is what the tests are about.
    fn track_colour(t: f32, alpha: u8) -> Color32 {
        stop_colour(&STOPS, t, alpha)
    }

    /// The stops are the anchors of the scale: read them back exactly, or the
    /// meter no longer says the same thing as the web's.
    #[test]
    fn the_gradient_returns_its_own_stops() {
        for (t, rgb) in STOPS {
            let c = track_colour(t, 255);
            assert_eq!([c.r(), c.g(), c.b()], rgb, "stop at {t}");
        }
    }

    /// Between two stops the colour has to move monotonically, and a value past
    /// either end clamps instead of wrapping.
    #[test]
    fn the_gradient_interpolates_and_clamps() {
        let mid = track_colour(0.30, 255);
        assert!(mid.r() > 0x4d && mid.r() < 0x7b, "red climbs: {}", mid.r());
        assert_eq!(track_colour(-1.0, 255), track_colour(0.0, 255));
        assert_eq!(track_colour(2.0, 255), track_colour(1.0, 255));
    }

    /// 0 dBFS on a -60..+6 scale, which is where the headroom zone opens.
    #[test]
    fn the_clipping_zone_starts_at_0_dbfs() {
        assert!((clip_start() - 0.909_09).abs() < 1e-4, "{}", clip_start());
    }

    /// The capsule is as tall as the bar along the middle and closes to nothing
    /// at both ends — that is what makes the ends read as round.
    #[test]
    fn the_capsule_closes_at_both_ends() {
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 6.0));
        assert!((half_height(&rect, 50.0) - 3.0).abs() < 1e-6);
        assert!(half_height(&rect, 0.0) < 1e-6);
        assert!(half_height(&rect, 100.0) < 1e-6);
        // Halfway into the left cap the profile is the circle's, not a ramp.
        let expected = (9.0f32 - 2.25).sqrt();
        assert!((half_height(&rect, 1.5) - expected).abs() < 1e-5);
    }

    /// Every sampled column has to produce a quad, and the stops must be in
    /// there so no stop is crossed by a single interpolated span.
    #[test]
    fn the_mesh_covers_the_track() {
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(120.0, 6.0));
        let xs = sample_xs(&rect, &STOPS);
        for (t, _) in STOPS {
            let want = rect.left() + rect.width() * t;
            assert!(
                xs.iter().any(|x| (x - want).abs() < 0.05),
                "no vertex at stop {t}"
            );
        }
        let mesh = capsule(&rect, 255, None);
        assert_eq!(mesh.vertices.len(), xs.len() * 2);
        assert_eq!(mesh.indices.len(), (xs.len() - 1) * 6);
    }
}
