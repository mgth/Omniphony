//! Display, Trails and Heatmaps sections of the left overlay
//! (`#displaySection`, `#trailSection`, `#heatmapsSection`).

use std::time::Duration;

use crate::app::StudioSpike;
use crate::i18n::t;
use crate::ui::section::{Section, open_max_height};
use crate::ui::widgets;
use crate::view::objects::ObjectDisplayMode;
use crate::view::trails::TrailMode;
use crate::view::volumes::{Colormap, DiscontinuityMode};

impl StudioSpike {
    pub(crate) fn display_sections(&mut self, ui: &mut egui::Ui) {
        let max_height = open_max_height(ui.ctx().content_rect().height());
        let mut locale_choice: Option<String> = None;
        let locale = self
            .prefs
            .locale
            .clone()
            .unwrap_or_else(|| "auto".to_owned());
        {
            let s = &mut self.settings;
            Section::new("displaySection", "section.display")
                .default_open(true)
                .max_height(max_height)
                .show(ui, |ui| {
                    // The language row heads the Display section, as in the web.
                    ui.horizontal(|ui| {
                        ui.label(t("app.language"));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            egui::ComboBox::from_id_salt("locale")
                                .selected_text(
                                    crate::i18n::LOCALE_OPTIONS
                                        .iter()
                                        .find(|(id, _)| *id == locale)
                                        .map(|(_, label)| *label)
                                        .unwrap_or("Auto"),
                                )
                                .width(150.0)
                                .show_ui(ui, |ui| {
                                    for (id, label) in crate::i18n::LOCALE_OPTIONS {
                                        if ui.selectable_label(*id == locale, *label).clicked()
                                            && *id != locale
                                        {
                                            locale_choice = Some((*id).to_owned());
                                        }
                                    }
                                });
                            // "Auto" does not say which language it picked, and
                            // that is exactly what a reader checks when the UI
                            // is not in the language they expected.
                            if locale == "auto" {
                                ui.label(
                                    egui::RichText::new(crate::i18n::active_locale())
                                        .size(crate::ui::theme::FONT_SIZE_SMALL)
                                        .color(crate::ui::theme::TEXT_MUTED),
                                );
                            }
                        });
                    });
                    widgets::switch_row(ui, t("display.showObjects"), &mut s.objects_visible);
                    ui.horizontal(|ui| {
                        ui.label(t("display.objectDisplayMode"));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            egui::ComboBox::from_id_salt("object-display-mode")
                                .selected_text(s.object_display_mode.label())
                                .width(150.0)
                                .show_ui(ui, |ui| {
                                    for mode in ObjectDisplayMode::ALL {
                                        ui.selectable_value(
                                            &mut s.object_display_mode,
                                            mode,
                                            mode.label(),
                                        );
                                    }
                                });
                        });
                    });
                    ui.add(
                        egui::Slider::new(&mut s.object_sphere_size, 0.03..=0.2)
                            .step_by(0.002)
                            .text(t("display.objectSphereSize")),
                    );
                    widgets::switch_row(
                        ui,
                        t("display.objectColors"),
                        &mut s.object_colors_enabled,
                    );
                    widgets::switch_row(
                        ui,
                        t("display.objectLabels"),
                        &mut s.object_labels_enabled,
                    );
                    widgets::switch_row(ui, "Effective render", &mut s.effective_render_enabled);
                    widgets::switch_row(ui, t("display.grid"), &mut s.vbap_grid);
                    ui.separator();
                    widgets::switch_row(ui, t("display.speakers"), &mut s.speakers_visible);
                    widgets::switch_row(
                        ui,
                        t("display.speakerLabels"),
                        &mut s.speaker_labels_enabled,
                    );
                    ui.add(
                        egui::Slider::new(&mut s.speaker_size, 0.04..=0.2)
                            .step_by(0.002)
                            .text(t("display.speakerSize")),
                    );
                });
        }

        if let Some(choice) = locale_choice {
            // The renderer is not told: the language is this host's own, and
            // nothing on the wire carries a string the user reads.
            crate::i18n::set_locale(&choice);
            self.prefs.locale = Some(choice);
            self.mark_prefs_dirty();
        }

        let s = &mut self.settings;
        Section::new("trailSection", "trail.title")
            .info("trail")
            .default_open(true)
            .max_height(max_height)
            .show(ui, |ui| {
                widgets::switch_row(ui, t("trail.show"), &mut s.trails.enabled);
                ui.horizontal(|ui| {
                    ui.label(t("trail.mode"));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        egui::ComboBox::from_id_salt("trail-mode")
                            .selected_text(s.trails.mode.label())
                            .width(120.0)
                            .show_ui(ui, |ui| {
                                for mode in [TrailMode::Diffuse, TrailMode::Line] {
                                    ui.selectable_value(&mut s.trails.mode, mode, mode.label());
                                }
                            });
                    });
                });
                let mut ttl_s = s.trails.ttl.as_secs_f32();
                if ui
                    .add(
                        egui::Slider::new(&mut ttl_s, 1.0..=20.0)
                            .step_by(0.5)
                            .text(t("trail.duration")),
                    )
                    .changed()
                {
                    s.trails.ttl = Duration::from_secs_f32(ttl_s.max(0.5));
                }
                ui.add(
                    egui::Slider::new(&mut s.trails.teleport_threshold, 0.05..=2.0)
                        .step_by(0.05)
                        .text(t("trail.teleport")),
                );
            });

        let v = &mut self.volume_settings;
        Section::new("heatmapsSection", "display.heatmaps")
            .info("heatmap")
            .help("help.heatmaps")
            .max_height(max_height)
            .show(ui, |ui| {
                let combo = |ui: &mut egui::Ui, id: &str, label: &str, cm: &mut Colormap| {
                    ui.horizontal(|ui| {
                        ui.label(label);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            egui::ComboBox::from_id_salt(id)
                                .selected_text(cm.label())
                                .width(130.0)
                                .show_ui(ui, |ui| {
                                    for c in Colormap::ALL {
                                        ui.selectable_value(cm, c, c.label());
                                    }
                                });
                        });
                    });
                };
                widgets::switch_row(ui, t("heatmap.objectEnergy"), &mut v.object_field_enabled);
                combo(
                    ui,
                    "object-colormap",
                    t("heatmap.objectEnergy.colormap"),
                    &mut v.object_colormap,
                );
                ui.add(
                    egui::Slider::new(&mut v.object_radius, 0.02..=0.5)
                        .step_by(0.01)
                        .text(t("heatmap.objectEnergy.radius")),
                );
                ui.separator();
                widgets::switch_row(ui, t("heatmap.globalEnergy"), &mut v.global_enabled);
                ui.add(
                    egui::Slider::new(&mut v.global_scale_db, 1.0..=40.0)
                        .step_by(1.0)
                        .text(t("heatmap.globalEnergy.scale")),
                );
                ui.separator();
                widgets::switch_row(ui, t("heatmap.speakers"), &mut v.speaker_enabled);
                combo(
                    ui,
                    "speaker-colormap",
                    t("heatmap.objectEnergy.colormap"),
                    &mut v.speaker_colormap,
                );
                ui.separator();
                widgets::switch_row(
                    ui,
                    t("heatmap.discontinuity.toggle"),
                    &mut v.discontinuity_enabled,
                );
                ui.horizontal(|ui| {
                    ui.label(t("heatmap.discontinuity.mode"));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        egui::ComboBox::from_id_salt("discontinuity-mode")
                            .selected_text(match v.discontinuity_mode {
                                DiscontinuityMode::Gain => "Gain",
                                DiscontinuityMode::Centroid => "Centroid",
                            })
                            .width(130.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut v.discontinuity_mode,
                                    DiscontinuityMode::Gain,
                                    t("heatmap.discontinuity.modeGain"),
                                );
                                ui.selectable_value(
                                    &mut v.discontinuity_mode,
                                    DiscontinuityMode::Centroid,
                                    t("heatmap.discontinuity.modeCentroid"),
                                );
                            });
                    });
                });
                ui.add(
                    egui::Slider::new(&mut v.discontinuity_scale, 0.05..=2.0)
                        .step_by(0.05)
                        .text(t("heatmap.discontinuity.scale")),
                );
                ui.separator();
                widgets::note(ui, t("heatmap.common"));
                let mut res = v.resolution as f32;
                if ui
                    .add(
                        egui::Slider::new(&mut res, 8.0..=64.0)
                            .step_by(2.0)
                            .text(t("heatmap.objectEnergy.resolution")),
                    )
                    .changed()
                {
                    v.resolution = res.round() as u32;
                }
                ui.add(
                    egui::Slider::new(&mut v.opacity, 0.05..=1.0)
                        .step_by(0.05)
                        .text(t("heatmap.objectEnergy.opacity")),
                );
                ui.add(
                    egui::Slider::new(&mut v.mix, 0.0..=1.0)
                        .step_by(0.01)
                        .text(t("heatmap.objectEnergy.mix")),
                );
                ui.add(
                    egui::Slider::new(&mut v.gamma_accumulate, 1.0..=10.0)
                        .step_by(0.1)
                        .text(t("heatmap.objectEnergy.gammaAccumulate")),
                );
                ui.add(
                    egui::Slider::new(&mut v.gamma_mip, 0.2..=3.0)
                        .step_by(0.05)
                        .text(t("heatmap.objectEnergy.gammaMip")),
                );
                let mut refresh = v.refresh_ms as f32;
                if ui
                    .add(
                        egui::Slider::new(&mut refresh, 40.0..=500.0)
                            .step_by(10.0)
                            .text(t("heatmap.objectEnergy.refresh")),
                    )
                    .changed()
                {
                    v.refresh_ms = refresh.round() as u32;
                }
                widgets::switch_row(ui, t("heatmap.smooth"), &mut v.smooth);
                widgets::switch_row(ui, t("heatmap.bandAll"), &mut v.all_bands);
                if !v.all_bands {
                    let mut band = v.band_index as f32;
                    if ui
                        .add(
                            egui::Slider::new(&mut band, 0.0..=7.0)
                                .step_by(1.0)
                                .text("band"),
                        )
                        .changed()
                    {
                        v.band_index = band.round() as usize;
                    }
                }
            });
    }
}
