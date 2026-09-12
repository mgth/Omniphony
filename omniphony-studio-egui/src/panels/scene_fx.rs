//! The scene-effects bar (`#sceneEffectBar`, `controls/scene-effects-bar.js`).
//!
//! Seven quick toggles pinned over the bottom of the viewport, each mirroring a
//! switch that already exists in a panel. No effect logic lives here: the bar
//! drives the same state the panel does, so a toggle made from either place is
//! the same toggle. It exists because the display switches are the ones reached
//! for most often while looking at the scene, and reaching for them should not
//! mean opening a panel over the thing being looked at.
//!
//! Two of them carry a "nature" flyout — which kind of object marker, which
//! kind of trail — for the same reason: the choice belongs with the switch that
//! turns it on.
//!
//! The last button opens the display panel: every setting the bar summarises
//! (Display, Trails, Heatmaps), in a panel of its own floating over the bar.
//! Those settings are about the scene, not about the audio chain the side
//! panels hold, so they open from the scene's own controls.

use egui::{Align2, Color32, Pos2, Rect, Sense, Ui, pos2, vec2};

use crate::app::StudioSpike;
use crate::i18n::t;
use crate::ui::icons::{self, Icon};
use crate::view::objects::ObjectDisplayMode;
use crate::view::trails::TrailMode;

/// `#sceneEffectBar`: `rgba(18,22,28,0.72)`, a hairline of white at 8 %.
const BAR_BG: Color32 = Color32::from_rgba_premultiplied(13, 16, 20, 184);
const BAR_EDGE: Color32 = Color32::from_rgba_premultiplied(20, 20, 20, 20);
/// `.scene-fx-flyout`: the same glass, denser, with a 10 % edge.
const FLY_BG: Color32 = Color32::from_rgba_premultiplied(17, 20, 26, 235);
const FLY_EDGE: Color32 = Color32::from_rgba_premultiplied(26, 26, 26, 26);
/// The accent green of an active tool, and its text tones.
const GREEN: [u8; 3] = [82, 226, 162];
const GREEN_TEXT: Color32 = Color32::from_rgb(0x7a, 0xf0, 0xc0);
const GREEN_TEXT_HOT: Color32 = Color32::from_rgb(0xae, 0xf5, 0xd8);
/// `rgba(223, 232, 243, a)`, the idle icon tone at the alphas the stylesheet uses.
fn ink(alpha: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(223, 232, 243, (alpha * 255.0).round() as u8)
}
fn green(alpha: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(GREEN[0], GREEN[1], GREEN[2], (alpha * 255.0).round() as u8)
}
fn white(alpha: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(255, 255, 255, (alpha * 255.0).round() as u8)
}

/// `.scene-fx-btn` and `.fx-flyout-item` boxes, and the icon inside them.
const BUTTON: egui::Vec2 = vec2(40.0, 36.0);
const ITEM: egui::Vec2 = vec2(40.0, 34.0);
const ICON: f32 = 20.0;
/// `.fx-caret`: 12 × 10 in the button's top-right corner, 2 down and 3 in.
const CARET: egui::Vec2 = vec2(12.0, 10.0);
/// The bar sits above the save footer, which owns the bottom margin.
const BOTTOM_OFFSET: f32 = -56.0;
/// The flyout floats 9 points above its button, a 6-point tail between them.
const FLYOUT_GAP: f32 = 9.0;
const TAIL: f32 = 6.0;

/// The display panel's area, for the backdrop blur behind it.
pub const DISPLAY_PANEL_ID: &str = "display-panel";
/// The display panel's width, the side panels' default.
const DISPLAY_PANEL_WIDTH: f32 = 380.0;
/// Its tallest: enough for the three sections half open, and short enough to
/// leave most of the scene above it in view.
const DISPLAY_PANEL_MAX_HEIGHT: f32 = 640.0;

/// Which flyout is open, if any.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Flyout {
    Objects,
    Trails,
}

/// What a press on a bar button asked for.
#[derive(PartialEq, Eq)]
enum Press {
    None,
    /// The body: toggle the layer.
    Toggle,
    /// The caret, or a right-click: open or close the nature menu.
    Menu,
}

impl StudioSpike {
    pub(crate) fn scene_fx_bar(&mut self, ctx: &egui::Context) {
        let mut anchors = (Rect::NOTHING, Rect::NOTHING);
        let bar = egui::Area::new(egui::Id::new("scene-fx-bar"))
            .anchor(Align2::CENTER_BOTTOM, vec2(0.0, BOTTOM_OFFSET))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(BAR_BG)
                    .stroke(egui::Stroke::new(1.0, BAR_EDGE))
                    .corner_radius(14)
                    .inner_margin(egui::Margin::symmetric(8, 6))
                    .shadow(egui::Shadow {
                        offset: [0, 6],
                        blur: 24,
                        spread: 0,
                        color: Color32::from_black_alpha(115),
                    })
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.horizontal(|ui| anchors = self.scene_fx_buttons(ui));
                    });
            });
        self.scene_fx_flyout(ctx, anchors);
        self.display_panel(ctx, bar.response.rect);
    }

    /// Display, Trails and Heatmaps, in a panel floating over the bar.
    ///
    /// Fixed extents, as the side panels have: its height is set by the room
    /// above the bar, not by what is open inside it, and the sections scroll
    /// within. A panel sized by its content would grow upward as a section
    /// opened, and sections open under the pointer.
    fn display_panel(&mut self, ctx: &egui::Context, bar: Rect) {
        if !self.display_panel_open || !bar.is_finite() {
            return;
        }
        let screen = ctx.content_rect();
        let margin = crate::ui::theme::PANEL_EDGE_MARGIN;
        // Between the side panels when it can be: narrowed to the room
        // either side of the bar, but never below a usable width.
        let side_edge = |id: &str| ctx.memory(|m| m.area_rect(egui::Id::new(id)));
        let left = side_edge("overlay-left").map_or(screen.left(), |r| r.right());
        let right = side_edge("overlay-right").map_or(screen.right(), |r| r.left());
        let room = 2.0 * (bar.center().x - left).min(right - bar.center().x) - 2.0 * margin;
        let width = DISPLAY_PANEL_WIDTH.min(room).max(260.0);
        let height = (bar.top() - FLYOUT_GAP - screen.top() - margin)
            .min(DISPLAY_PANEL_MAX_HEIGHT)
            .max(120.0);
        let mut close = false;
        egui::Area::new(egui::Id::new(DISPLAY_PANEL_ID))
            .order(egui::Order::Middle)
            .pivot(Align2::CENTER_BOTTOM)
            .fixed_pos(pos2(bar.center().x, bar.top() - FLYOUT_GAP))
            .show(ctx, |ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
                let mut panel = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(rect)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                panel.set_clip_rect(rect.intersect(ui.clip_rect()));
                let frame = crate::ui::theme::panel_frame();
                let chrome = frame.total_margin().sum();
                frame.show(&mut panel, |ui| {
                    ui.set_width(width - chrome.x);
                    ui.set_height(height - chrome.y);
                    // No title: the first section is "Display" and says it.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                        close = close_button(ui);
                    });
                    crate::ui::overlay::panel_scroll(ui, "display-panel-scroll", None, |ui| {
                        self.display_sections(ui);
                    });
                });
            });
        if close {
            self.display_panel_open = false;
        }
    }

    /// The seven toggles. Returns where the two flyout buttons were drawn, so
    /// their menus can open above them.
    fn scene_fx_buttons(&mut self, ui: &mut Ui) -> (Rect, Rect) {
        let open = self.scene_fx_flyout_open;
        let mut grid = self.settings.vbap_grid;
        if fx_button(ui, &icons::GRID, &mut grid, t("sceneFx.grid"), None).0 == Press::Toggle {
            self.settings.vbap_grid = grid;
        }
        let mut objects = self.settings.objects_visible;
        let (press, objects_rect) = fx_button(
            ui,
            &icons::OBJECTS,
            &mut objects,
            t("sceneFx.objects"),
            Some(open == Some(Flyout::Objects)),
        );
        match press {
            Press::Toggle => self.settings.objects_visible = objects,
            Press::Menu => {
                self.scene_fx_flyout_open = toggle(open, Flyout::Objects);
                self.display_panel_open = false;
            }
            Press::None => {}
        }
        let mut labels = self.settings.object_labels_enabled;
        if fx_button(ui, &icons::LABELS, &mut labels, t("sceneFx.labels"), None).0 == Press::Toggle
        {
            self.settings.object_labels_enabled = labels;
        }
        let mut trails = self.settings.trails.enabled;
        let (press, trails_rect) = fx_button(
            ui,
            &icons::TRAILS,
            &mut trails,
            t("sceneFx.trails"),
            Some(open == Some(Flyout::Trails)),
        );
        match press {
            Press::Toggle => self.settings.trails.enabled = trails,
            Press::Menu => {
                self.scene_fx_flyout_open = toggle(open, Flyout::Trails);
                self.display_panel_open = false;
            }
            Press::None => {}
        }
        let mut field = self.volume_settings.object_field_enabled;
        if fx_button(
            ui,
            &icons::FIELD,
            &mut field,
            t("sceneFx.energyField"),
            None,
        )
        .0 == Press::Toggle
        {
            self.volume_settings.object_field_enabled = field;
        }
        let mut heatmap = self.volume_settings.speaker_enabled;
        if fx_button(
            ui,
            &icons::HEATMAP,
            &mut heatmap,
            t("sceneFx.heatmap"),
            None,
        )
        .0 == Press::Toggle
        {
            self.volume_settings.speaker_enabled = heatmap;
        }
        // The overlay's state is the engine's, not this host's: it can be
        // toggled from an mpv keybind Studio never sees, so the button reflects
        // what the engine last published rather than a local flag.
        let mut overlay = self
            .live
            .lock()
            .unwrap()
            .overlay
            .as_ref()
            .and_then(|o| o.get("enabled"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if fx_button(ui, &icons::MPV, &mut overlay, t("sceneFx.mpvOverlay"), None).0
            == Press::Toggle
        {
            crate::host::commands::mpv_overlay::mpv_overlay_set_active(&self.host, overlay);
        }
        // The toggles end here; the last key opens their settings.
        let (divider, _) = ui.allocate_exact_size(vec2(1.0, BUTTON.y - 12.0), Sense::hover());
        ui.painter().rect_filled(divider, 0.0, white(0.12));
        let mut panel = self.display_panel_open;
        if fx_button(ui, &icons::SETTINGS, &mut panel, t("section.display"), None).0
            == Press::Toggle
        {
            self.display_panel_open = panel;
            // Both float over the bar: one at a time.
            if panel {
                self.scene_fx_flyout_open = None;
            }
        }
        (objects_rect, trails_rect)
    }

    /// The nature menu, floating above its button with a tail pointing down at
    /// it. Picking a nature also turns its layer on: choosing how something
    /// should look is asking to see it (`chooseFlyoutValue`).
    fn scene_fx_flyout(&mut self, ctx: &egui::Context, anchors: (Rect, Rect)) {
        let Some(open) = self.scene_fx_flyout_open else {
            return;
        };
        let owner = match open {
            Flyout::Objects => anchors.0,
            Flyout::Trails => anchors.1,
        };
        if !owner.is_finite() {
            return;
        }
        let mut close = false;
        let area = egui::Area::new(egui::Id::new("scene-fx-flyout"))
            .order(egui::Order::Foreground)
            .pivot(Align2::CENTER_BOTTOM)
            .fixed_pos(pos2(owner.center().x, owner.top() - FLYOUT_GAP))
            .show(ctx, |ui| {
                let frame = egui::Frame::new()
                    .fill(FLY_BG)
                    .stroke(egui::Stroke::new(1.0, FLY_EDGE))
                    .corner_radius(11)
                    .inner_margin(egui::Margin::same(5))
                    .shadow(egui::Shadow {
                        offset: [0, 8],
                        blur: 28,
                        spread: 0,
                        color: Color32::from_black_alpha(128),
                    })
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.vertical(|ui| match open {
                            Flyout::Objects => {
                                let current = self.settings.object_display_mode;
                                for (mode, icon, key) in [
                                    (
                                        ObjectDisplayMode::Circle,
                                        &icons::MODE_CIRCLE,
                                        "display.objectDisplayMode.circle",
                                    ),
                                    (
                                        ObjectDisplayMode::TransparentSphere,
                                        &icons::MODE_TRANSPARENT,
                                        "display.objectDisplayMode.transparentSphere",
                                    ),
                                    (
                                        ObjectDisplayMode::DiffuseSphere,
                                        &icons::MODE_DIFFUSE,
                                        "display.objectDisplayMode.diffuseSphere",
                                    ),
                                ] {
                                    if flyout_item(ui, icon, mode == current, t(key)) {
                                        self.settings.object_display_mode = mode;
                                        self.settings.objects_visible = true;
                                        close = true;
                                    }
                                }
                            }
                            Flyout::Trails => {
                                let current = self.settings.trails.mode;
                                for (mode, icon, key) in [
                                    (
                                        TrailMode::Diffuse,
                                        &icons::TRAIL_DIFFUSE,
                                        "trail.mode.diffuse",
                                    ),
                                    (TrailMode::Line, &icons::TRAIL_LINE, "trail.mode.line"),
                                ] {
                                    if flyout_item(ui, icon, mode == current, t(key)) {
                                        self.settings.trails.mode = mode;
                                        self.settings.trails.enabled = true;
                                        close = true;
                                    }
                                }
                            }
                        });
                    });
                // `::after`: a small triangle bridging the menu down to its
                // button, in the menu's own glass. Painted on the layer so the
                // area's clip does not cut it off below the frame.
                let c = frame.response.rect.center_bottom();
                ui.ctx()
                    .layer_painter(ui.layer_id())
                    .add(egui::Shape::convex_polygon(
                        vec![
                            c + vec2(-TAIL, -0.5),
                            c + vec2(TAIL, -0.5),
                            c + vec2(0.0, TAIL),
                        ],
                        FLY_BG,
                        egui::Stroke::NONE,
                    ));
                frame.response.rect
            });
        // Dismissed by a click anywhere but the menu or its own button (which
        // handles the click itself), and by Escape.
        let menu = area.inner;
        let outside = ctx.input(|i| {
            i.pointer.any_click()
                && i.pointer
                    .interact_pos()
                    .is_some_and(|p: Pos2| !menu.contains(p) && !owner.contains(p))
        });
        if close || outside || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.scene_fx_flyout_open = None;
        }
    }
}

fn toggle(current: Option<Flyout>, wanted: Flyout) -> Option<Flyout> {
    if current == Some(wanted) {
        None
    } else {
        Some(wanted)
    }
}

/// `.scene-fx-btn`: an icon in a 40 × 36 key, lit green while its layer is on.
/// `menu` is `Some(open)` for the two buttons that carry a nature caret.
fn fx_button(
    ui: &mut Ui,
    icon: &Icon,
    on: &mut bool,
    title: &str,
    menu: Option<bool>,
) -> (Press, Rect) {
    let (rect, response) = ui.allocate_exact_size(BUTTON, Sense::click());
    let caret = Rect::from_min_size(pos2(rect.right() - 3.0 - CARET.x, rect.top() + 2.0), CARET);
    let pointer = ui.ctx().pointer_hover_pos();
    let hovered = response.hovered();
    let caret_hovered = menu.is_some() && hovered && pointer.is_some_and(|p| caret.contains(p));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let (bg, border, ink_colour) = if *on {
            (
                green(if hovered { 0.24 } else { 0.16 }),
                Some(green(0.55)),
                if hovered { GREEN_TEXT_HOT } else { GREEN_TEXT },
            )
        } else if hovered {
            (white(0.07), None, ink(0.92))
        } else {
            (Color32::TRANSPARENT, None, ink(0.55))
        };
        painter.rect_filled(rect, 9.0, bg);
        if let Some(border) = border {
            painter.rect_stroke(
                rect,
                9.0,
                egui::Stroke::new(1.0, border),
                egui::StrokeKind::Inside,
            );
            // `box-shadow: inset 0 0 0 1px rgba(82,226,162,0.18)`.
            painter.rect_stroke(
                rect.shrink(1.0),
                8.0,
                egui::Stroke::new(1.0, green(0.18)),
                egui::StrokeKind::Inside,
            );
        }
        icons::paint(
            painter,
            Rect::from_center_size(rect.center(), vec2(ICON, ICON)),
            icon,
            ink_colour,
        );
        if menu.is_some() {
            let caret_ink = if caret_hovered {
                painter.rect_filled(caret, 3.0, white(0.14));
                Color32::WHITE
            } else if *on {
                Color32::from_rgba_unmultiplied(122, 240, 192, 204)
            } else if hovered {
                ink(0.75)
            } else {
                ink(0.38)
            };
            icons::paint(
                painter,
                Rect::from_center_size(caret.center(), vec2(10.0, 7.0)),
                &icons::CARET,
                caret_ink,
            );
        }
    }
    let response = response.on_hover_text(title);
    let press = if menu.is_some() && response.secondary_clicked() {
        Press::Menu
    } else if response.clicked() {
        if menu.is_some()
            && response
                .interact_pointer_pos()
                .is_some_and(|p| caret.contains(p))
        {
            Press::Menu
        } else {
            *on = !*on;
            Press::Toggle
        }
    } else {
        Press::None
    };
    (press, rect)
}

/// `.fx-flyout-item`: one nature, as an icon; green when it is the current one.
fn flyout_item(ui: &mut Ui, icon: &Icon, selected: bool, title: &str) -> bool {
    let (rect, response) = ui.allocate_exact_size(ITEM, Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let hovered = response.hovered();
        let ink_colour = if selected {
            painter.rect_filled(rect, 8.0, green(0.16));
            painter.rect_stroke(
                rect,
                8.0,
                egui::Stroke::new(1.0, green(0.5)),
                egui::StrokeKind::Inside,
            );
            GREEN_TEXT_HOT
        } else if hovered {
            painter.rect_filled(rect, 8.0, white(0.08));
            Color32::WHITE
        } else {
            ink(0.6)
        };
        icons::paint(
            painter,
            Rect::from_center_size(rect.center(), vec2(ICON, ICON)),
            icon,
            ink_colour,
        );
    }
    response.on_hover_text(title).clicked()
}

/// A small painted cross, so it does not depend on a glyph the bundled
/// fonts may lack.
fn close_button(ui: &mut Ui) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
    let hovered = response.hovered();
    if hovered {
        ui.painter().rect_filled(rect, 5.0, white(0.08));
    }
    let r = Rect::from_center_size(rect.center(), vec2(8.0, 8.0));
    let stroke = egui::Stroke::new(1.5, if hovered { Color32::WHITE } else { ink(0.6) });
    ui.painter()
        .line_segment([r.left_top(), r.right_bottom()], stroke);
    ui.painter()
        .line_segment([r.right_top(), r.left_bottom()], stroke);
    response.on_hover_text(t("common.close")).clicked()
}
