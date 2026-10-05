//! The banner for a configuration file the renderer will not write: one that
//! failed to parse (it runs on built-in defaults) or one a newer build wrote
//! (it runs on what it understands of it).
//!
//! Without it, a renderer on defaults shows a default layout and the reason
//! only in About, or in the footer once a Save is refused. It floats below
//! the log line, between the side overlays, so the viewport keeps its size.

use egui::{Align2, Id, RichText};

use crate::app::StudioSpike;
use crate::i18n::t;
use crate::model::app_state::ConfigRefusal;
use crate::ui::layout::{OverlayLayout, Side};
use crate::ui::{theme, widgets};

/// The clear space between the log line and the banner.
const GAP: f32 = 8.0;
/// Where the banner sits before the log line has a rect: its first frame.
const TOP_FALLBACK: f32 = 52.0;
/// The space kept clear of each side overlay, as the log line does.
const SIDE_GAP: f32 = 66.0;
const MAX_WIDTH: f32 = 560.0;

fn severity(refusal: ConfigRefusal) -> widgets::Severity {
    match refusal {
        ConfigRefusal::ParseError => widgets::Severity::Error,
        ConfigRefusal::NewerSchema => widgets::Severity::Warning,
    }
}

fn title(refusal: ConfigRefusal) -> &'static str {
    match refusal {
        ConfigRefusal::ParseError => t("config.banner.parseErrorTitle"),
        ConfigRefusal::NewerSchema => t("config.banner.newerSchemaTitle"),
    }
}

fn detail(refusal: ConfigRefusal) -> &'static str {
    match refusal {
        ConfigRefusal::ParseError => t("config.banner.parseErrorDetail"),
        ConfigRefusal::NewerSchema => t("config.banner.newerSchemaDetail"),
    }
}

impl StudioSpike {
    pub(crate) fn config_banner(&mut self, ctx: &egui::Context, layout: &OverlayLayout) {
        // What a renderer said stops meaning anything once it is gone.
        let connected = self.osc_stats.connection_state() == crate::osc::ConnectionState::Connected;
        if !connected {
            return;
        }
        let (refusal, path) = {
            let live = self.host.read();
            (
                live.app.config_refusal(),
                live.app.render_config_path.clone().unwrap_or_default(),
            )
        };
        let Some(refusal) = refusal else {
            return;
        };
        let screen = ctx.content_rect();
        let left = layout.effective_width(Side::Left) + SIDE_GAP;
        let right = layout.effective_width(Side::Right) + SIDE_GAP;
        let between = screen.width() - left - right;
        let width = between.clamp(220.0, MAX_WIDTH);
        let top = ctx
            .memory(|m| m.area_rect(Id::new("log-overlay")))
            .map(|log| log.bottom() + GAP)
            .unwrap_or(TOP_FALLBACK);
        egui::Area::new(Id::new("config-banner"))
            .pivot(Align2::CENTER_TOP)
            .fixed_pos([screen.left() + left + between / 2.0, top])
            .order(egui::Order::Middle)
            .show(ctx, |ui| {
                ui.set_width(width);
                // Opaque under the tint: the banner is read over the scene.
                egui::Frame::new()
                    .fill(theme::PAGE_BG)
                    .corner_radius(theme::CONTROL_RADIUS)
                    .show(ui, |ui| {
                        widgets::banner_with(ui, severity(refusal), title(refusal), |ui| {
                            ui.label(
                                RichText::new(detail(refusal))
                                    .size(theme::FONT_SIZE_SMALL)
                                    .color(theme::TEXT_MUTED),
                            );
                            ui.horizontal(|ui| {
                                if ui.button(t("config.reload")).clicked() {
                                    self.request_reload();
                                }
                                if !path.is_empty() {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(&path)
                                                .monospace()
                                                .size(theme::FONT_SIZE_SMALL),
                                        )
                                        .truncate(),
                                    )
                                    .on_hover_text(&path);
                                }
                            });
                        });
                    });
            });
    }
}
