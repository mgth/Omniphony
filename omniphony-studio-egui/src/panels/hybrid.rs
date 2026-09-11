//! The hybrid backend's own controls (`#hybridSection`,
//! `renderer-panel.js:407–451`, `controls/hybrid-curve.js`).
//!
//! A hybrid renders the same object twice — once through an "external" backend
//! and once through an "internal" one — and crossfades between them by
//! distance. The curve *is* the backend: it says, for every distance from the
//! listener, how much of each model is heard. Everything else on this panel
//! exists to make that curve editable.

use egui::{Color32, Pos2, RichText, Stroke, Ui, vec2};

use crate::app::StudioSpike;
use crate::i18n::t;
use crate::ui::{help, theme, widgets};

/// The preview canvas, as the web sizes it.
const CURVE_HEIGHT: f32 = 180.0;
/// How many samples the host takes of the curve for the preview. The renderer
/// evaluates the same function, so the drawing cannot disagree with the audio.
const CURVE_SAMPLES: usize = 96;
/// How close to a point a click has to land to grab it, in canvas fractions.
const GRAB: f32 = 0.05;
/// Interior points stay strictly between their neighbours by this much, so an
/// edit can never produce a vertical segment the evaluator cannot invert.
const MIN_GAP: f64 = 1e-3;

const CURVE_BG: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 64);
const CURVE_EDGE: Color32 = Color32::from_rgba_premultiplied(31, 31, 31, 31);

/// Which tab of the hybrid panel is showing: the mix itself, or one of the two
/// inner backends' own parameters.
pub type HybridTab = String;

impl StudioSpike {
    /// The tab row, then either the mix panel or an inner backend's params.
    ///
    /// The inner backends are tuned through the same schema-generated controls
    /// as any other backend, addressed with the `backend` argument — so a
    /// hybrid's VBAP half can be sharpened without touching a plain VBAP.
    pub(crate) fn hybrid_block(
        &mut self,
        ui: &mut Ui,
        available: &serde_json::Value,
        values: &serde_json::Value,
    ) {
        let hybrid = {
            let live = self.live.lock().unwrap();
            live.app.render_backend_state.hybrid.clone()
        };
        let external = hybrid
            .external_backend
            .clone()
            .unwrap_or_else(|| "vbap".to_owned());
        let internal = hybrid
            .internal_backend
            .clone()
            .unwrap_or_else(|| "barycenter".to_owned());
        let mut tabs: Vec<String> = vec!["hybrid".to_owned(), external.clone()];
        if internal != external {
            tabs.push(internal.clone());
        }
        if !tabs.contains(&self.hybrid_tab) {
            self.hybrid_tab = tabs[0].clone();
        }
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(t("hybrid.title"))
                    .size(theme::FONT_SIZE)
                    .color(theme::TEXT_STRONG),
            );
            for id in &tabs {
                let label = if id == "hybrid" {
                    t("hybrid.tabMix").to_owned()
                } else {
                    super::renderer::backend_list(available)
                        .into_iter()
                        .find(|(value, _)| value == id)
                        .map(|(_, label)| label)
                        .unwrap_or_else(|| id.clone())
                };
                if ui.selectable_label(&self.hybrid_tab == id, label).clicked() {
                    self.hybrid_tab = id.clone();
                }
            }
        });
        if self.hybrid_tab != "hybrid" {
            let backend = self.hybrid_tab.clone();
            self.backend_params_for(ui, &backend, available, values);
            return;
        }
        self.hybrid_mix(ui, available, &external, &internal, &hybrid);
    }

    fn hybrid_mix(
        &mut self,
        ui: &mut Ui,
        available: &serde_json::Value,
        external: &str,
        internal: &str,
        hybrid: &crate::model::app_state::HybridState,
    ) {
        // The inner backends are every registered backend except another
        // hybrid: nesting one inside itself has no meaning.
        let inner: Vec<(String, String)> = super::renderer::backend_list(available)
            .into_iter()
            .filter(|(id, _)| id != "hybrid")
            .collect();
        if let Some(chosen) = backend_row(
            ui,
            t("hybrid.external"),
            "help.hybrid.external",
            "hybrid-external",
            external,
            &inner,
        ) {
            self.live
                .lock()
                .unwrap()
                .app
                .render_backend_state
                .hybrid
                .external_backend = Some(chosen.clone());
            self.mark_recompute_pending();
            self.ctl
                .send_string("/omniphony/control/hybrid/external_backend", &chosen);
        }
        if let Some(chosen) = backend_row(
            ui,
            t("hybrid.internal"),
            "help.hybrid.internal",
            "hybrid-internal",
            internal,
            &inner,
        ) {
            self.live
                .lock()
                .unwrap()
                .app
                .render_backend_state
                .hybrid
                .internal_backend = Some(chosen.clone());
            self.mark_recompute_pending();
            self.ctl
                .send_string("/omniphony/control/hybrid/internal_backend", &chosen);
        }
        let metric = hybrid
            .metric
            .clone()
            .unwrap_or_else(|| "chebyshev".to_owned());
        if let Some(chosen) = backend_row(
            ui,
            t("distance.metric"),
            "help.hybrid.metric",
            "hybrid-metric",
            &metric,
            &[
                (
                    "chebyshev".to_owned(),
                    t("distance.metric.chebyshev").to_owned(),
                ),
                (
                    "spherical".to_owned(),
                    t("distance.metric.spherical").to_owned(),
                ),
            ],
        ) {
            self.live
                .lock()
                .unwrap()
                .app
                .render_backend_state
                .hybrid
                .metric = Some(chosen.clone());
            self.mark_recompute_pending();
            self.ctl
                .send_string("/omniphony/control/hybrid/metric", &chosen);
        }
        let mut smoothing = hybrid.curve_smoothing.unwrap_or(0.0) as f32;
        if widgets::value_slider_help(
            ui,
            t("hybrid.smoothing"),
            "help.hybrid.smoothing",
            &mut smoothing,
            0.0..=1.0,
            0.01,
            |v| format!("{v:.2}"),
        ) {
            self.live
                .lock()
                .unwrap()
                .app
                .render_backend_state
                .hybrid
                .curve_smoothing = Some(f64::from(smoothing));
            self.mark_recompute_pending();
            self.ctl
                .send_float("/omniphony/control/hybrid/curve_smoothing", smoothing);
        }
        widgets::note(ui, t("hybrid.curveHint"));
        self.hybrid_curve(ui, hybrid, &metric);
    }

    /// The curve editor. Drag a point, double-click empty space to add one,
    /// double-click a point to remove it.
    ///
    /// The endpoints are locked to x = 0 and x = 1 because the curve has to
    /// answer for every distance; an interior point is kept strictly between
    /// its neighbours because the evaluator inverts x, and two points at the
    /// same distance would ask it which of two ratios is the answer.
    fn hybrid_curve(
        &mut self,
        ui: &mut Ui,
        hybrid: &crate::model::app_state::HybridState,
        metric: &str,
    ) {
        let (rect, response) = ui.allocate_exact_size(
            vec2(ui.available_width(), CURVE_HEIGHT),
            egui::Sense::click_and_drag(),
        );
        let painter = ui.painter().with_clip_rect(rect);
        painter.rect(
            rect,
            theme::CONTROL_RADIUS,
            CURVE_BG,
            Stroke::new(1.0, CURVE_EDGE),
            egui::StrokeKind::Inside,
        );
        let mut points = hybrid.curve.clone();
        if points.len() < 2 {
            points = vec![[0.0, 0.0], [1.0, 1.0]];
        }
        let to_screen = |p: [f64; 2]| {
            egui::pos2(
                rect.left() + p[0] as f32 * rect.width(),
                rect.bottom() - p[1] as f32 * rect.height(),
            )
        };
        let to_curve = |p: Pos2| {
            [
                ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64,
                ((rect.bottom() - p.y) / rect.height()).clamp(0.0, 1.0) as f64,
            ]
        };

        // The preview is the host's own sampling of the curve, so what is drawn
        // is what the renderer evaluates rather than a second implementation.
        let sampled = crate::host::commands::render::sample_hybrid_curve(
            points.clone(),
            hybrid.curve_smoothing.unwrap_or(0.0),
            CURVE_SAMPLES,
        );
        let line: Vec<Pos2> = sampled
            .iter()
            .enumerate()
            .map(|(i, y)| to_screen([i as f64 / CURVE_SAMPLES as f64, *y]))
            .collect();
        painter.add(egui::Shape::line(line, Stroke::new(1.5, theme::ACCENT)));

        let mut changed = false;
        if let Some(pos) = response.interact_pointer_pos() {
            let target = to_curve(pos);
            let nearest = points
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| distance(a, target).total_cmp(&distance(b, target)))
                .filter(|(_, p)| distance(p, target) <= f64::from(GRAB))
                .map(|(i, _)| i);
            if response.double_clicked() {
                match nearest {
                    // Never the endpoints: the curve must answer for every
                    // distance, so it cannot lose its ends.
                    Some(index) if index > 0 && index + 1 < points.len() => {
                        points.remove(index);
                        self.hybrid_point = None;
                        changed = true;
                    }
                    Some(_) => {}
                    None => {
                        let at = points.iter().position(|p| p[0] > target[0]).unwrap_or(1);
                        points.insert(at, target);
                        self.hybrid_point = Some(at);
                        changed = true;
                    }
                }
            } else if response.drag_started() {
                self.hybrid_point = nearest;
            } else if response.clicked() {
                self.hybrid_point = nearest;
            }
            if response.dragged()
                && let Some(index) = self.hybrid_point
                && index < points.len()
            {
                let last = points.len() - 1;
                let x = if index == 0 {
                    0.0
                } else if index == last {
                    1.0
                } else {
                    target[0].clamp(
                        points[index - 1][0] + MIN_GAP,
                        points[index + 1][0] - MIN_GAP,
                    )
                };
                points[index] = [x, target[1]];
                changed = true;
            }
        }

        for (index, point) in points.iter().enumerate() {
            let selected = self.hybrid_point == Some(index);
            painter.circle(
                to_screen(*point),
                if selected { 5.0 } else { 3.5 },
                if selected {
                    theme::OK_BRIGHT
                } else {
                    theme::ACCENT
                },
                Stroke::new(1.0, Color32::from_black_alpha(140)),
            );
        }
        if changed {
            self.commit_hybrid_curve(points.clone());
        }
        self.hybrid_point_editor(ui, &points, metric);
    }

    /// The numeric editor for the selected point. Distance is shown in the
    /// metric's own units, because "0.58" means nothing without knowing what
    /// the far corner of the room is.
    fn hybrid_point_editor(&mut self, ui: &mut Ui, points: &[[f64; 2]], metric: &str) {
        let Some(index) = self.hybrid_point.filter(|i| *i < points.len()) else {
            return;
        };
        let max_distance = if metric == "spherical" {
            3f64.sqrt()
        } else {
            1.0
        };
        let mut point = points[index];
        let endpoint = index == 0 || index + 1 == points.len();
        let mut changed = false;
        ui.horizontal(|ui| {
            help::label(
                ui,
                RichText::new(t("hybrid.selectedPoint"))
                    .size(theme::FONT_SIZE_SMALL)
                    .color(theme::TEXT_MUTED),
                "help.hybrid.selectedPoint",
            );
            ui.label(t("hybrid.pointDistance"));
            let mut distance = point[0] * max_distance;
            ui.add_enabled_ui(!endpoint, |ui| {
                if ui
                    .add(
                        egui::DragValue::new(&mut distance)
                            .speed(0.01)
                            .range(0.0..=max_distance),
                    )
                    .changed()
                {
                    point[0] = (distance / max_distance).clamp(0.0, 1.0);
                    changed = true;
                }
            });
            ui.label(t("hybrid.pointRatio"));
            if ui
                .add(
                    egui::DragValue::new(&mut point[1])
                        .speed(0.01)
                        .range(0.0..=1.0),
                )
                .changed()
            {
                changed = true;
            }
        });
        help::card(ui, "help.hybrid.selectedPoint");
        if changed {
            let mut next = points.to_vec();
            next[index] = point;
            next.sort_by(|a, b| a[0].total_cmp(&b[0]));
            self.commit_hybrid_curve(next);
        }
    }

    fn commit_hybrid_curve(&mut self, points: Vec<[f64; 2]>) {
        self.live
            .lock()
            .unwrap()
            .app
            .render_backend_state
            .hybrid
            .curve = points.clone();
        self.mark_recompute_pending();
        let flat: Vec<rosc::OscType> = points
            .iter()
            .flat_map(|p| {
                [
                    rosc::OscType::Float(p[0].clamp(0.0, 1.0) as f32),
                    rosc::OscType::Float(p[1].clamp(0.0, 1.0) as f32),
                ]
            })
            .collect();
        self.ctl.send("/omniphony/control/hybrid/curve", flat);
    }
}

fn distance(a: &[f64; 2], b: [f64; 2]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// A labelled select over backend ids.
fn backend_row(
    ui: &mut Ui,
    label: &str,
    help_key: &str,
    id: &str,
    current: &str,
    options: &[(String, String)],
) -> Option<String> {
    widgets::label_row_help(ui, label, help_key, |ui| {
        widgets::bounded_combo(ui, 150.0, |ui, w| {
            let mut chosen = None;
            egui::ComboBox::from_id_salt(id)
                .selected_text(
                    options
                        .iter()
                        .find(|(value, _)| value == current)
                        .map(|(_, label)| label.clone())
                        .unwrap_or_else(|| current.to_owned()),
                )
                .width(w)
                .truncate()
                .show_ui(ui, |ui| {
                    for (value, label) in options {
                        if ui.selectable_label(value == current, label).clicked()
                            && value != current
                        {
                            chosen = Some(value.clone());
                        }
                    }
                });
            chosen
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dragged_interior_point_stays_between_its_neighbours() {
        let points = [[0.0, 0.0], [0.5, 0.5], [1.0, 1.0]];
        // Dragged far past the right neighbour: it stops just short of it,
        // because two points at one distance would ask the evaluator which of
        // two ratios is the answer.
        let clamped = 2.0f64.clamp(points[0][0] + MIN_GAP, points[2][0] - MIN_GAP);
        assert!(clamped < 1.0 && clamped > 0.998);
        let clamped = (-1.0f64).clamp(points[0][0] + MIN_GAP, points[2][0] - MIN_GAP);
        assert!(clamped > 0.0);
    }

    #[test]
    fn the_grab_radius_picks_the_nearest_point_and_only_when_close() {
        let points = [[0.0, 0.0], [0.5, 0.5], [1.0, 1.0]];
        assert!(distance(&points[1], [0.52, 0.52]) < f64::from(GRAB));
        assert!(distance(&points[1], [0.8, 0.2]) > f64::from(GRAB));
    }
}
