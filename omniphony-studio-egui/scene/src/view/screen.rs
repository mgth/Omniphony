//! Screen space, in the view's own terms.
//!
//! The view works out where a thing lands on screen and what colour it is. It
//! does not know what puts it there. These are the types it says that in — a
//! point in logical pixels with y downwards, a rectangle of them, and
//! unmultiplied sRGB bytes — so swapping the toolkit means converting at the
//! boundary rather than rewriting the projection.
//!
//! `[u8; 4]` rather than a colour type of our own: every toolkit takes it, and
//! a wrapper here would be one more thing to unwrap on the way out.

use glam::Vec2;

/// A point on screen, in logical pixels, y downwards.
pub type ScreenPos = Vec2;

/// Unmultiplied sRGB with alpha.
pub type Color = [u8; 4];

pub const WHITE: Color = [255, 255, 255, 255];

/// Opaque, from sRGB bytes.
pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
    [r, g, b, 255]
}

/// White at an alpha — the overlay's usual way of dimming a line or a label.
pub const fn white_alpha(a: u8) -> Color {
    [255, 255, 255, a]
}

/// An axis-aligned rectangle in screen points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenRect {
    pub min: ScreenPos,
    pub max: ScreenPos,
}

impl ScreenRect {
    pub const fn from_min_max(min: ScreenPos, max: ScreenPos) -> Self {
        Self { min, max }
    }

    pub fn from_min_size(min: ScreenPos, size: Vec2) -> Self {
        Self {
            min,
            max: min + size,
        }
    }

    pub fn from_center_size(center: ScreenPos, size: Vec2) -> Self {
        let half = size * 0.5;
        Self {
            min: center - half,
            max: center + half,
        }
    }

    pub fn left(&self) -> f32 {
        self.min.x
    }
    pub fn right(&self) -> f32 {
        self.max.x
    }
    pub fn top(&self) -> f32 {
        self.min.y
    }
    pub fn width(&self) -> f32 {
        self.max.x - self.min.x
    }
    pub fn height(&self) -> f32 {
        self.max.y - self.min.y
    }

    pub fn contains(&self, p: ScreenPos) -> bool {
        p.x >= self.min.x && p.x <= self.max.x && p.y >= self.min.y && p.y <= self.max.y
    }

    /// The overlap, which may be empty (`min` past `max` on an axis). Callers
    /// here only use it to keep a drawn segment inside its track.
    pub fn intersect(&self, other: Self) -> Self {
        Self {
            min: self.min.max(other.min),
            max: self.max.min(other.max),
        }
    }
}

/// One drawable primitive of an overlay the view composes but does not paint.
///
/// Deliberately small: the band gauge is the only thing in `view/` that draws
/// rather than projects, and two shapes cover it. Anything needing more than
/// this is a sign it belongs in a panel.
#[derive(Clone, Copy, Debug)]
pub enum Shape {
    /// Filled first, then stroked inside its own edge when `stroke` is set.
    Rect {
        rect: ScreenRect,
        /// Corner radius in points; 0 is square.
        radius: f32,
        fill: Option<Color>,
        /// Width and colour.
        stroke: Option<(f32, Color)>,
    },
    /// A horizontal rule from `x.0` to `x.1`.
    HLine {
        x: (f32, f32),
        y: f32,
        width: f32,
        color: Color,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rect_knows_its_edges_and_what_it_holds() {
        let r = ScreenRect::from_min_size(Vec2::new(10.0, 20.0), Vec2::new(30.0, 40.0));
        assert_eq!((r.left(), r.top(), r.right()), (10.0, 20.0, 40.0));
        assert_eq!((r.width(), r.height()), (30.0, 40.0));
        assert!(r.contains(Vec2::new(25.0, 40.0)));
        // The edges count: a gauge hit on its own border still picks it.
        assert!(r.contains(Vec2::new(10.0, 20.0)));
        assert!(!r.contains(Vec2::new(9.0, 40.0)));
    }

    #[test]
    fn from_center_size_and_intersect_agree_with_the_edges() {
        let r = ScreenRect::from_center_size(Vec2::ZERO, Vec2::new(10.0, 10.0));
        assert_eq!(r.min, Vec2::new(-5.0, -5.0));
        assert_eq!(r.max, Vec2::new(5.0, 5.0));
        let clipped = r.intersect(ScreenRect::from_min_max(
            Vec2::new(0.0, -100.0),
            Vec2::new(100.0, 100.0),
        ));
        assert_eq!(clipped.min, Vec2::new(0.0, -5.0));
        assert_eq!(clipped.max, Vec2::new(5.0, 5.0));
    }
}
