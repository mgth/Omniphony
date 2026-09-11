//! Display, Trails and Heatmaps sections of the left overlay
//! (`#displaySection`, `#trailSection`, `#heatmapsSection`).

use std::time::Duration;

use crate::app::StudioSpike;
use crate::i18n::t;
use crate::ui::section::Section;
use crate::ui::widgets;
use crate::view::objects::ObjectDisplayMode;
use crate::view::trails::TrailMode;
use crate::view::volumes::{Colormap, DiscontinuityMode};

impl StudioSpike {
    pub(crate) fn display_sections(&mut self, ui: &mut egui::Ui) {
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
                .show(ui, |ui| {
                    // The language row heads the Display section, as in the web.
                    widgets::label_row(ui, t("app.language"), |ui| {
                        widgets::bounded_combo(ui, 150.0, |ui, w| {
                            egui::ComboBox::from_id_salt("locale")
                                .selected_text(
                                    crate::i18n::LOCALE_OPTIONS
                                        .iter()
                                        .find(|(id, _)| *id == locale)
                                        .map(|(_, label)| *label)
                                        .unwrap_or("Auto"),
                                )
                                .width(w)
                                .truncate()
                                .show_ui(ui, |ui| {
                                    for (id, label) in crate::i18n::LOCALE_OPTIONS {
                                        if ui.selectable_label(*id == locale, *label).clicked()
                                            && *id != locale
                                        {
                                            locale_choice = Some((*id).to_owned());
                                        }
                                    }
                                })
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
                    widgets::switch_row(ui, t("display.showObjects"), &mut s.objects_visible);
                    widgets::label_row(ui, t("display.objectDisplayMode"), |ui| {
                        widgets::bounded_combo(ui, 150.0, |ui, w| {
                            egui::ComboBox::from_id_salt("object-display-mode")
                                .selected_text(s.object_display_mode.label())
                                .width(w)
                                .truncate()
                                .show_ui(ui, |ui| {
                                    for mode in ObjectDisplayMode::ALL {
                                        ui.selectable_value(
                                            &mut s.object_display_mode,
                                            mode,
                                            mode.label(),
                                        );
                                    }
                                })
                        });
                    });
                    widgets::slider_line(
                        ui,
                        t("display.objectSphereSize"),
                        &mut s.object_sphere_size,
                        0.03..=0.2,
                        0.002,
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
                    widgets::switch_row(
                        ui,
                        t("display.speakerBands"),
                        &mut s.speaker_band_bars_enabled,
                    );
                    widgets::switch_row(
                        ui,
                        t("display.speakerFaceListener"),
                        &mut s.speaker_face_listener_enabled,
                    );
                    widgets::slider_line(
                        ui,
                        t("display.speakerSize"),
                        &mut s.speaker_size,
                        0.04..=0.2,
                        0.002,
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
            .show(ui, |ui| {
                widgets::switch_row(ui, t("trail.show"), &mut s.trails.enabled);
                widgets::label_row(ui, t("trail.mode"), |ui| {
                    widgets::bounded_combo(ui, 120.0, |ui, w| {
                        egui::ComboBox::from_id_salt("trail-mode")
                            .selected_text(s.trails.mode.label())
                            .width(w)
                            .truncate()
                            .show_ui(ui, |ui| {
                                for mode in [TrailMode::Diffuse, TrailMode::Line] {
                                    ui.selectable_value(&mut s.trails.mode, mode, mode.label());
                                }
                            })
                    });
                });
                let mut ttl_s = s.trails.ttl.as_secs_f32();
                if widgets::slider_line(ui, t("trail.duration"), &mut ttl_s, 1.0..=20.0, 0.5)
                    .changed()
                {
                    s.trails.ttl = Duration::from_secs_f32(ttl_s.max(0.5));
                }
                widgets::slider_line(
                    ui,
                    t("trail.teleport"),
                    &mut s.trails.teleport_threshold,
                    0.05..=2.0,
                    0.05,
                );
            });

        // Which stop each editor has selected. Held on the app rather than in
        // the settings: it is a pointer into the list, not part of the
        // gradient, and a saved selection would go stale the moment a stop is
        // added elsewhere.
        let mut object_stop = self.object_stop_selected;
        let mut speaker_stop = self.speaker_stop_selected;
        let v = &mut self.volume_settings;
        Section::new("heatmapsSection", "display.heatmaps")
            .info("heatmap")
            .help("help.heatmaps")
            .show(ui, |ui| {
                let combo = |ui: &mut egui::Ui, id: &str, label: &str, cm: &mut Colormap| {
                    widgets::label_row(ui, label, |ui| {
                        widgets::bounded_combo(ui, 130.0, |ui, w| {
                            egui::ComboBox::from_id_salt(id)
                                .selected_text(cm.label())
                                .width(w)
                                .truncate()
                                .show_ui(ui, |ui| {
                                    for c in Colormap::ALL {
                                        ui.selectable_value(cm, c, c.label());
                                    }
                                })
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
                // The editor appears with the colormap it edits: a gradient bar
                // sitting under a preset nobody picked is noise.
                if v.object_colormap == Colormap::Custom {
                    crate::panels::gradient::gradient_editor(
                        ui,
                        &mut v.object_stops,
                        &mut object_stop,
                    );
                }
                widgets::slider_line(
                    ui,
                    t("heatmap.objectEnergy.radius"),
                    &mut v.object_radius,
                    0.02..=0.5,
                    0.01,
                );
                ui.separator();
                widgets::switch_row(ui, t("heatmap.globalEnergy"), &mut v.global_enabled);
                widgets::slider_line(
                    ui,
                    t("heatmap.globalEnergy.scale"),
                    &mut v.global_scale_db,
                    1.0..=40.0,
                    1.0,
                );
                ui.separator();
                widgets::switch_row(ui, t("heatmap.speakers"), &mut v.speaker_enabled);
                combo(
                    ui,
                    "speaker-colormap",
                    t("heatmap.objectEnergy.colormap"),
                    &mut v.speaker_colormap,
                );
                if v.speaker_colormap == Colormap::Custom {
                    crate::panels::gradient::gradient_editor(
                        ui,
                        &mut v.speaker_stops,
                        &mut speaker_stop,
                    );
                }
                ui.separator();
                widgets::switch_row(
                    ui,
                    t("heatmap.discontinuity.toggle"),
                    &mut v.discontinuity_enabled,
                );
                widgets::label_row(ui, t("heatmap.discontinuity.mode"), |ui| {
                    widgets::bounded_combo(ui, 130.0, |ui, w| {
                        egui::ComboBox::from_id_salt("discontinuity-mode")
                            .selected_text(match v.discontinuity_mode {
                                DiscontinuityMode::Gain => "Gain",
                                DiscontinuityMode::Centroid => "Centroid",
                            })
                            .width(w)
                            .truncate()
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
                            })
                    });
                });
                widgets::slider_line(
                    ui,
                    t("heatmap.discontinuity.scale"),
                    &mut v.discontinuity_scale,
                    0.05..=2.0,
                    0.05,
                );
                ui.separator();
                widgets::note(ui, t("heatmap.common"));
                let mut res = v.resolution as f32;
                if widgets::slider_line(
                    ui,
                    t("heatmap.objectEnergy.resolution"),
                    &mut res,
                    8.0..=64.0,
                    2.0,
                )
                .changed()
                {
                    v.resolution = res.round() as u32;
                }
                widgets::slider_line(
                    ui,
                    t("heatmap.objectEnergy.opacity"),
                    &mut v.opacity,
                    0.05..=1.0,
                    0.05,
                );
                widgets::slider_line(
                    ui,
                    t("heatmap.objectEnergy.mix"),
                    &mut v.mix,
                    0.0..=1.0,
                    0.01,
                );
                widgets::slider_line(
                    ui,
                    t("heatmap.objectEnergy.gammaAccumulate"),
                    &mut v.gamma_accumulate,
                    1.0..=10.0,
                    0.1,
                );
                widgets::slider_line(
                    ui,
                    t("heatmap.objectEnergy.gammaMip"),
                    &mut v.gamma_mip,
                    0.2..=3.0,
                    0.05,
                );
                let mut refresh = v.refresh_ms as f32;
                if widgets::slider_line(
                    ui,
                    t("heatmap.objectEnergy.refresh"),
                    &mut refresh,
                    40.0..=500.0,
                    10.0,
                )
                .changed()
                {
                    v.refresh_ms = refresh.round() as u32;
                }
                widgets::switch_row(ui, t("heatmap.smooth"), &mut v.smooth);
                widgets::switch_row(ui, t("heatmap.bandAll"), &mut v.all_bands);
                if !v.all_bands {
                    let mut band = v.band_index as f32;
                    if widgets::slider_line(ui, "band", &mut band, 0.0..=7.0, 1.0).changed() {
                        v.band_index = band.round() as usize;
                    }
                }
            });
        self.object_stop_selected = object_stop;
        self.speaker_stop_selected = speaker_stop;
    }
}
