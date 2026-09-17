//! The Display and Heatmaps sections (`#displaySection` with its
//! `#trailSection` sub-card, `#heatmapsSection`).

use std::time::Duration;

use crate::app::StudioSpike;
use crate::i18n::t;
use crate::ui::group::Group;
use crate::ui::section::Section;
use crate::ui::widgets;
use crate::view::objects::ObjectDisplayMode;
use crate::view::trails::TrailMode;
use crate::view::volumes::{Colormap, DiscontinuityMode};

impl StudioSpike {
    /// The language select. The web heads its Display section with it; here
    /// the Display settings moved to the scene's own panel, and the language
    /// is not a setting of the scene, so it stays in the main panel with the
    /// other settings of the application.
    pub(crate) fn language_row(&mut self, ui: &mut egui::Ui) {
        let mut locale_choice: Option<String> = None;
        let locale = self
            .prefs
            .locale
            .clone()
            .unwrap_or_else(|| "auto".to_owned());
        widgets::label_row_help(ui, t("app.language"), "help.display.language", |ui| {
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
                            if ui.selectable_label(*id == locale, *label).clicked() && *id != locale
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
        if let Some(choice) = locale_choice {
            // The renderer is not told: the language is this host's own, and
            // nothing on the wire carries a string the user reads.
            crate::i18n::set_locale(&choice);
            self.prefs.locale = Some(choice);
            self.mark_prefs_dirty();
        }
    }

    pub(crate) fn display_sections(&mut self, ui: &mut egui::Ui) {
        {
            let s = &mut self.settings;
            let host = &self.host;
            // The overlay's state is the engine's (an mpv keybind can flip
            // it), so the row shows what the engine last published, as the
            // scene-effects button does.
            let mut overlay = host
                .read()
                .overlay
                .as_ref()
                .and_then(|o| o.get("enabled"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            // The web draws one card with a sub-card per subject: the rows
            // that apply to the whole scene first, then a group each for the
            // objects, the trails and the speakers, its "show" switch in
            // the bar and how it looks in the inset.
            Section::new("displaySection", "section.display")
                .default_open(true)
                .show(ui, |ui| {
                    if widgets::switch_row_help(
                        ui,
                        t("mpvOverlay.title"),
                        "help.display.mpvOverlay",
                        &mut overlay,
                    ) {
                        crate::host::commands::mpv_overlay::mpv_overlay_set_active(host, overlay);
                    }
                    widgets::switch_row_help(
                        ui,
                        t("display.grid"),
                        "help.display.grid",
                        &mut s.vbap_grid,
                    );
                    Group::new(t("display.objectAppearance"))
                        .help("help.display.showObjects")
                        .actions(|ui| {
                            widgets::switch(ui, &mut s.objects_visible);
                        })
                        .show(ui, |ui| {
                            widgets::label_row_help(
                                ui,
                                t("display.objectDisplayMode"),
                                "help.display.objectDisplayMode",
                                |ui| {
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
                                },
                            );
                            widgets::slider_line_help(
                                ui,
                                t("display.objectSphereSize"),
                                "help.display.objectSphereSize",
                                &mut s.object_sphere_size,
                                0.03..=0.2,
                                0.002,
                            );
                            widgets::switch_row_help(
                                ui,
                                t("display.objectColors"),
                                "help.display.objectColors",
                                &mut s.object_colors_enabled,
                            );
                            widgets::switch_row_help(
                                ui,
                                t("display.objectLabels"),
                                "help.display.objectLabels",
                                &mut s.object_labels_enabled,
                            );
                            widgets::switch_row_help(
                                ui,
                                t("display.showObjectDetails"),
                                "help.display.showObjectDetails",
                                &mut s.show_object_details,
                            );
                            widgets::label_row_info(
                                ui,
                                t("effectiveRender.title"),
                                "effectiveRender",
                                |ui| widgets::switch(ui, &mut s.effective_render_enabled),
                            );
                        });
                    Group::new(t("trail.title"))
                        .info("trail")
                        .actions(|ui| {
                            widgets::switch(ui, &mut s.trails.enabled);
                        })
                        .show(ui, |ui| {
                            widgets::label_row_help(ui, t("trail.mode"), "help.trail.mode", |ui| {
                                widgets::bounded_combo(ui, 120.0, |ui, w| {
                                    egui::ComboBox::from_id_salt("trail-mode")
                                        .selected_text(s.trails.mode.label())
                                        .width(w)
                                        .truncate()
                                        .show_ui(ui, |ui| {
                                            for mode in [TrailMode::Diffuse, TrailMode::Line] {
                                                ui.selectable_value(
                                                    &mut s.trails.mode,
                                                    mode,
                                                    mode.label(),
                                                );
                                            }
                                        })
                                });
                            });
                            let mut ttl_s = s.trails.ttl.as_secs_f32();
                            if widgets::slider_line_help(
                                ui,
                                t("trail.duration"),
                                "help.trail.duration",
                                &mut ttl_s,
                                1.0..=20.0,
                                0.5,
                            )
                            .changed()
                            {
                                s.trails.ttl = Duration::from_secs_f32(ttl_s.max(0.5));
                            }
                            widgets::slider_line_help(
                                ui,
                                t("trail.teleport"),
                                "help.trail.teleport",
                                &mut s.trails.teleport_threshold,
                                0.05..=2.0,
                                0.05,
                            );
                        });
                    Group::new(t("display.speakers"))
                        .help("help.display.showSpeakers")
                        .actions(|ui| {
                            widgets::switch(ui, &mut s.speakers_visible);
                        })
                        .show(ui, |ui| {
                            widgets::switch_row_help(
                                ui,
                                t("display.speakerLabels"),
                                "help.display.speakerLabels",
                                &mut s.speaker_labels_enabled,
                            );
                            widgets::switch_row_help(
                                ui,
                                t("display.speakerBands"),
                                "help.display.speakerBands",
                                &mut s.speaker_band_bars_enabled,
                            );
                            widgets::switch_row_help(
                                ui,
                                t("display.speakerFaceListener"),
                                "help.display.speakerFaceListener",
                                &mut s.speaker_face_listener_enabled,
                            );
                            widgets::slider_line_help(
                                ui,
                                t("display.speakerSize"),
                                "help.display.speakerSize",
                                &mut s.speaker_size,
                                0.04..=0.2,
                                0.002,
                            );
                        });
                });
        }

        // Which stop each editor has selected. Held on the app rather than in
        // the settings: it is a pointer into the list, not part of the
        // gradient, and a saved selection would go stale the moment a stop is
        // added elsewhere.
        let mut object_stop = self.object_stop_selected;
        let mut speaker_stop = self.speaker_stop_selected;
        let band_labels = {
            let live = self.host.read();
            crate::panels::row_glyphs::band_labels(&crate::model::layouts::crossover_cutoffs(
                &live.selected_speakers(),
            ))
        };
        let band = &mut self.settings.heatmap_band_index;
        let v = &mut self.volume_settings;
        // One group per heatmap (the web's sub-cards), its switch in the bar
        // and its own parameters in the inset, then the parameters they share.
        Section::new("heatmapsSection", "display.heatmaps")
            .info("heatmap")
            .show(ui, |ui| {
                // The one crossover-band selector: every heatmap below, the
                // effective render and the dominant-speaker readouts follow
                // it, as does the cursor floating over the scene.
                crossover_band_row(ui, band, &mut v.all_bands, &band_labels);
                // Both colormap rows share one help; each opens its own card.
                let combo = |ui: &mut egui::Ui, id: &str, label: &str, cm: &mut Colormap| {
                    let help = widgets::Help::text(
                        ("colormap", id),
                        crate::i18n::lookup("help.heatmap.colormap").unwrap_or(""),
                    );
                    widgets::label_row_help(ui, label, help, |ui| {
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
                Group::new(t("heatmap.objects"))
                    .help("help.heatmap.objectEnergy")
                    .actions(|ui| {
                        widgets::switch(ui, &mut v.object_field_enabled);
                    })
                    .show(ui, |ui| {
                        combo(
                            ui,
                            "object-colormap",
                            t("heatmap.objectEnergy.colormap"),
                            &mut v.object_colormap,
                        );
                        // The editor appears with the colormap it edits: a
                        // gradient bar sitting under a preset nobody picked is
                        // noise.
                        if v.object_colormap == Colormap::Custom {
                            crate::panels::gradient::gradient_editor(
                                ui,
                                &mut v.object_stops,
                                &mut object_stop,
                            );
                        }
                        widgets::slider_line_help(
                            ui,
                            t("heatmap.objectEnergy.radius"),
                            "help.heatmap.radius",
                            &mut v.object_radius,
                            0.02..=0.5,
                            0.01,
                        );
                    });
                Group::new(t("heatmap.global"))
                    .help("help.heatmap.globalEnergy")
                    .actions(|ui| {
                        widgets::switch(ui, &mut v.global_enabled);
                    })
                    .show(ui, |ui| {
                        widgets::slider_line_help(
                            ui,
                            t("heatmap.globalEnergy.scale"),
                            "help.heatmap.globalEnergyScale",
                            &mut v.global_scale_db,
                            1.0..=40.0,
                            1.0,
                        );
                    });
                Group::new(t("heatmap.speakers"))
                    .help("help.heatmap.speakerVolume")
                    .actions(|ui| {
                        widgets::switch(ui, &mut v.speaker_enabled);
                    })
                    .show(ui, |ui| {
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
                    });
                Group::new(t("heatmap.discontinuity"))
                    .help("help.heatmap.discontinuity")
                    .actions(|ui| {
                        widgets::switch(ui, &mut v.discontinuity_enabled);
                    })
                    .show(ui, |ui| {
                        widgets::label_row_help(
                            ui,
                            t("heatmap.discontinuity.mode"),
                            "help.heatmap.discontinuityMode",
                            |ui| {
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
                            },
                        );
                        widgets::slider_line_help(
                            ui,
                            t("heatmap.discontinuity.scale"),
                            "help.heatmap.discontinuityScale",
                            &mut v.discontinuity_scale,
                            0.05..=2.0,
                            0.05,
                        );
                    });
                Group::new(t("heatmap.common")).show(ui, |ui| {
                    let mut res = v.resolution as f32;
                    if widgets::slider_line_help(
                        ui,
                        t("heatmap.objectEnergy.resolution"),
                        "help.heatmap.resolution",
                        &mut res,
                        8.0..=64.0,
                        2.0,
                    )
                    .changed()
                    {
                        v.resolution = res.round() as u32;
                    }
                    widgets::slider_line_help(
                        ui,
                        t("heatmap.objectEnergy.opacity"),
                        "help.heatmap.opacity",
                        &mut v.opacity,
                        0.05..=1.0,
                        0.05,
                    );
                    widgets::slider_line_help(
                        ui,
                        t("heatmap.objectEnergy.mix"),
                        "help.heatmap.mix",
                        &mut v.mix,
                        0.0..=1.0,
                        0.01,
                    );
                    widgets::slider_line_help(
                        ui,
                        t("heatmap.objectEnergy.gammaAccumulate"),
                        "help.heatmap.gammaAccumulate",
                        &mut v.gamma_accumulate,
                        1.0..=10.0,
                        0.1,
                    );
                    widgets::slider_line_help(
                        ui,
                        t("heatmap.objectEnergy.gammaMip"),
                        "help.heatmap.gammaMip",
                        &mut v.gamma_mip,
                        0.2..=3.0,
                        0.05,
                    );
                    let mut refresh = v.refresh_ms as f32;
                    if widgets::slider_line_help(
                        ui,
                        t("heatmap.objectEnergy.refresh"),
                        "help.heatmap.refresh",
                        &mut refresh,
                        40.0..=500.0,
                        10.0,
                    )
                    .changed()
                    {
                        v.refresh_ms = refresh.round() as u32;
                    }
                    widgets::switch_row_help(
                        ui,
                        t("heatmap.smooth"),
                        "help.heatmap.smooth",
                        &mut v.smooth,
                    );
                });
            });
        self.object_stop_selected = object_stop;
        self.speaker_stop_selected = speaker_stop;
    }
}

/// `#heatmapBandSelect`: one entry per band, and "All bands" after the last
/// when there is more than one. Picking a band clears "all"; picking "all"
/// keeps the band, as the web does, for the readouts that follow one band.
fn crossover_band_row(ui: &mut egui::Ui, band: &mut usize, all: &mut bool, labels: &[String]) {
    let count = labels.len();
    let showing_all = count > 1 && *all;
    let shown = if showing_all {
        t("heatmap.bandAll").to_owned()
    } else {
        labels
            .get((*band).min(count.saturating_sub(1)))
            .cloned()
            .unwrap_or_default()
    };
    widgets::label_row_help(
        ui,
        t("heatmap.crossoverBand"),
        "help.heatmap.crossoverBand",
        |ui| {
            widgets::bounded_combo(ui, 150.0, |ui, w| {
                egui::ComboBox::from_id_salt("heatmap-band")
                    .selected_text(shown)
                    .width(w)
                    .truncate()
                    .show_ui(ui, |ui| {
                        for (index, label) in labels.iter().enumerate() {
                            if ui
                                .selectable_label(!showing_all && *band == index, label)
                                .clicked()
                            {
                                *band = index;
                                *all = false;
                            }
                        }
                        if count > 1
                            && ui
                                .selectable_label(showing_all, t("heatmap.bandAll"))
                                .clicked()
                        {
                            *all = true;
                        }
                    })
            });
        },
    );
}
