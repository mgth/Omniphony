//! The log overlay (`#logOverlay`, `src/log.js`): a collapsed summary line
//! floating between the two side panels, expanding into the newest-first list
//! of the 120-entry ring with a level filter and a copy/clear pair.

use egui::{Align2, Color32, Id, RichText, Ui};

use crate::app::StudioSpike;
use crate::i18n::t;
use crate::osc::dispatch::{LogLevel, LogLine};
use crate::ui::{layout::OverlayLayout, theme};

/// `top: 0.9rem`.
const TOP_MARGIN: f32 = 14.4;
/// `left/right: calc(panel width + 4rem + 2px)`.
const SIDE_GAP: f32 = 66.0;
/// `.log-panel-body { max-height: min(28vh, 260px) }`.
fn body_max_height(viewport_height: f32) -> f32 {
    (0.28 * viewport_height).min(260.0)
}

/// Chip colours of `.log-entry-level`.
fn level_colours(level: LogLevel) -> (Color32, Color32) {
    match level {
        LogLevel::Error => (
            Color32::from_rgb(0xff, 0xb0, 0xb0),
            Color32::from_rgba_unmultiplied(255, 92, 92, 41),
        ),
        LogLevel::Warn => (
            Color32::from_rgb(0xff, 0xd8, 0x91),
            Color32::from_rgba_unmultiplied(255, 177, 71, 41),
        ),
        LogLevel::Info => (
            Color32::from_rgb(0xb8, 0xde, 0xff),
            Color32::from_rgba_unmultiplied(86, 158, 255, 36),
        ),
        LogLevel::Debug => (
            Color32::from_rgb(0xd2, 0xc2, 0xff),
            Color32::from_rgba_unmultiplied(158, 118, 255, 38),
        ),
        LogLevel::Trace => (
            Color32::from_rgb(0xb8, 0xc1, 0xcf),
            Color32::from_rgba_unmultiplied(168, 177, 191, 31),
        ),
    }
}

/// Levels `control_log_level` accepts, in the select's order.
pub const LOG_LEVELS: &[&str] = &["off", "error", "warn", "info", "debug", "trace"];

impl StudioSpike {
    pub(crate) fn log_overlay(&mut self, ctx: &egui::Context, layout: &OverlayLayout) {
        let screen = ctx.content_rect();
        let left = layout.effective_width(crate::ui::layout::Side::Left) + SIDE_GAP;
        let right = layout.effective_width(crate::ui::layout::Side::Right) + SIDE_GAP;
        let width = (screen.width() - left - right).max(220.0);
        let entries: Vec<LogLine> = {
            let live = self.live.lock().unwrap();
            live.log.iter().rev().cloned().collect()
        };
        let filter = self.log_filter.to_lowercase();
        let filtered: Vec<&LogLine> = entries
            .iter()
            .filter(|e| {
                filter.is_empty() || {
                    let searchable =
                        format!("{} {} {}", e.rendered(), e.target, e.level.label()).to_lowercase();
                    searchable.contains(&filter)
                }
            })
            .collect();

        let expanded = self.log_expanded;
        egui::Area::new(Id::new("log-overlay"))
            .anchor(Align2::LEFT_TOP, [left, TOP_MARGIN])
            .order(egui::Order::Middle)
            .show(ctx, |ui| {
                ui.set_width(width);
                let frame = if expanded {
                    egui::Frame::new()
                        .fill(Color32::from_rgba_unmultiplied(8, 11, 18, 148))
                        .stroke(egui::Stroke::new(
                            1.0,
                            Color32::from_rgba_unmultiplied(255, 255, 255, 36),
                        ))
                        .corner_radius(14.0)
                        .inner_margin(egui::Margin {
                            left: 12,
                            right: 12,
                            top: 9,
                            bottom: 11,
                        })
                } else {
                    egui::Frame::NONE
                };
                frame.show(ui, |ui| {
                    self.log_header(ui, &filtered, expanded);
                    if expanded {
                        let max_height = body_max_height(screen.height());
                        egui::ScrollArea::vertical()
                            .id_salt("log-entries")
                            .max_height(max_height)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                if filtered.is_empty() {
                                    ui.label(
                                        RichText::new(t("log.empty"))
                                            .size(theme::FONT_SIZE_SMALL)
                                            .color(theme::TEXT_MUTED),
                                    );
                                }
                                for entry in &filtered {
                                    log_row(ui, entry);
                                }
                            });
                    }
                });
            });
    }

    fn log_header(&mut self, ui: &mut Ui, filtered: &[&LogLine], expanded: bool) {
        ui.horizontal(|ui| {
            let toggle = ui.add(
                egui::Button::new(if expanded { "▾" } else { "▸" })
                    .frame(expanded)
                    .min_size(egui::vec2(25.6, 25.6)),
            );
            if toggle.clicked() {
                self.log_expanded = !self.log_expanded;
            }
            if expanded {
                ui.label(
                    RichText::new(t("log.title"))
                        .size(theme::FONT_SIZE)
                        .color(theme::TEXT_STRONG),
                );
            }
            // The summary is the newest line of the filtered list; over the 3D
            // scene it is the only thing a collapsed panel shows.
            let summary = filtered
                .first()
                .map(|e| e.rendered())
                .unwrap_or_else(|| t("log.empty").to_owned());
            let size = if expanded {
                theme::FONT_SIZE_SMALL
            } else {
                13.0
            };
            ui.add(
                egui::Label::new(RichText::new(summary).size(size).color(if expanded {
                    theme::TEXT_MUTED
                } else {
                    theme::TEXT_STRONG
                }))
                .truncate(),
            );
            if !expanded {
                return;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(t("log.clear")).clicked() {
                    self.live.lock().unwrap().log.clear();
                }
                if ui.button(t("log.copy")).clicked() {
                    let text = filtered
                        .iter()
                        .rev()
                        .map(|e| format!("[{}] {}", e.level.label(), e.rendered()))
                        .collect::<Vec<_>>()
                        .join("\n");
                    ui.ctx().copy_text(text);
                }
                ui.add(
                    egui::TextEdit::singleline(&mut self.log_filter)
                        .hint_text(t("log.filterPlaceholder"))
                        .desired_width(180.0),
                );
                let current = {
                    let live = self.live.lock().unwrap();
                    live.app.log_level.clone().unwrap_or_else(|| "info".into())
                };
                let mut chosen = current.clone();
                egui::ComboBox::from_id_salt("log-level")
                    .selected_text(t(&format!("log.levelOption.{current}")))
                    .width(90.0)
                    .show_ui(ui, |ui| {
                        for level in LOG_LEVELS {
                            ui.selectable_value(
                                &mut chosen,
                                (*level).to_owned(),
                                t(&format!("log.levelOption.{level}")),
                            );
                        }
                    });
                if chosen != current {
                    // `control_log_level` → `/omniphony/control/log_level`.
                    self.ctl
                        .send_string("/omniphony/control/log_level", &chosen);
                    self.live.lock().unwrap().app.log_level = Some(chosen);
                }
                crate::ui::help::label(
                    ui,
                    RichText::new(t("log.levelLabel"))
                        .size(theme::FONT_SIZE_SMALL)
                        .color(theme::TEXT_MUTED),
                    "help.log.level",
                );
            });
        });
        crate::ui::help::card(ui, "help.log.level");
    }
}

/// One `.log-entry`: time, level chip, message.
fn log_row(ui: &mut Ui, entry: &LogLine) {
    let (text_colour, chip) = level_colours(entry.level);
    egui::Frame::new()
        .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 10))
        .stroke(egui::Stroke::new(
            1.0,
            Color32::from_rgba_unmultiplied(255, 255, 255, 20),
        ))
        .corner_radius(9.0)
        .inner_margin(egui::Margin {
            left: 8,
            right: 8,
            top: 7,
            bottom: 7,
        })
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.label(
                    RichText::new(format!("{:>5.1}s", entry.at.elapsed().as_secs_f32()))
                        .monospace()
                        .size(theme::FONT_SIZE_SMALL)
                        .color(Color32::from_rgba_unmultiplied(217, 236, 255, 143)),
                );
                egui::Frame::new()
                    .fill(chip)
                    .corner_radius(999.0)
                    .inner_margin(egui::Margin::symmetric(7, 1))
                    .show(ui, |ui| {
                        ui.set_min_width(52.8);
                        ui.label(
                            RichText::new(entry.level.label().to_uppercase())
                                .size(theme::FONT_SIZE_SMALL)
                                .color(text_colour),
                        );
                    });
                ui.label(
                    RichText::new(entry.rendered())
                        .size(theme::FONT_SIZE)
                        .color(theme::TEXT_STRONG),
                );
            });
        });
}
