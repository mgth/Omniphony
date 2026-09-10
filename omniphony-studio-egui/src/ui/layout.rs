//! Side-panel geometry: `src/ui/layout/overlay-layout-state.js` and
//! `src/ui/side-panels.js`. Both overlays can be resized by dragging their
//! inner edge and collapsed to a strip; the widths are clamped so the two can
//! meet in the middle but never overlap, and they persist between sessions.
//! The viewport is drawn behind them at full size, so no panel change can move
//! the scene (the rule `CLAUDE.md` states for the web Studio).

use serde::{Deserialize, Serialize};

pub const MIN_WIDTH: f32 = 220.0;
pub const DEFAULT_WIDTH: f32 = 440.0;
/// `COLLAPSED_WIDTH_REM = 1.8` at a 16 px root.
pub const COLLAPSED_WIDTH: f32 = 28.8;
/// The web panel is content-box: its footprint adds the padding and border.
pub const PANEL_CHROME: f32 = 2.0 * super::theme::PANEL_PADDING_X + 2.0;
const SAFETY_GAP: f32 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct PanelState {
    pub width: f32,
    pub collapsed: bool,
}

impl Default for PanelState {
    fn default() -> Self {
        Self {
            width: DEFAULT_WIDTH,
            collapsed: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct OverlayLayout {
    pub left: PanelState,
    pub right: PanelState,
}

impl OverlayLayout {
    pub fn side(&self, side: Side) -> &PanelState {
        match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        }
    }

    pub fn side_mut(&mut self, side: Side) -> &mut PanelState {
        match side {
            Side::Left => &mut self.left,
            Side::Right => &mut self.right,
        }
    }

    /// On-screen footprint: a collapsed panel drops its padding and border.
    pub fn effective_width(&self, side: Side) -> f32 {
        let s = self.side(side);
        if s.collapsed {
            COLLAPSED_WIDTH
        } else {
            s.width + PANEL_CHROME
        }
    }

    /// `clampWidth`: at least `MIN_WIDTH`, at most what the other panel and
    /// the outer margins leave.
    pub fn clamp_width(&self, width: f32, side: Side, viewport_width: f32) -> f32 {
        let other = match side {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        };
        let reserve = 2.0 * super::theme::PANEL_EDGE_MARGIN + SAFETY_GAP;
        let max = (viewport_width - self.effective_width(other) - PANEL_CHROME - reserve)
            .floor()
            .max(MIN_WIDTH);
        width.clamp(MIN_WIDTH, max)
    }

    pub fn clamp_all(&mut self, viewport_width: f32) {
        self.left.width = self.clamp_width(self.left.width, Side::Left, viewport_width);
        self.right.width = self.clamp_width(self.right.width, Side::Right, viewport_width);
    }

    pub fn set_width(&mut self, side: Side, width: f32, viewport_width: f32) {
        let clamped = self.clamp_width(width, side, viewport_width);
        self.side_mut(side).width = clamped;
    }

    pub fn toggle_collapsed(&mut self, side: Side, viewport_width: f32) {
        let s = self.side_mut(side);
        s.collapsed = !s.collapsed;
        self.clamp_all(viewport_width);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_clamp_so_the_panels_cannot_overlap() {
        let mut layout = OverlayLayout::default();
        layout.clamp_all(1400.0);
        assert!(layout.left.width >= MIN_WIDTH);
        // Both footprints plus the margins fit in the window.
        let total = layout.effective_width(Side::Left) + layout.effective_width(Side::Right);
        assert!(total <= 1400.0, "{total} > 1400");
    }

    #[test]
    fn a_narrow_window_falls_back_to_the_minimum() {
        let mut layout = OverlayLayout::default();
        layout.clamp_all(500.0);
        assert_eq!(layout.left.width, MIN_WIDTH);
    }

    #[test]
    fn collapsing_one_side_frees_width_for_the_other() {
        let mut layout = OverlayLayout::default();
        layout.clamp_all(1400.0);
        let before = layout.clamp_width(f32::MAX, Side::Left, 1400.0);
        layout.toggle_collapsed(Side::Right, 1400.0);
        let after = layout.clamp_width(f32::MAX, Side::Left, 1400.0);
        assert!(after > before);
    }
}
