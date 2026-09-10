//! The object and speaker lists of the right overlay (`#objectsList`,
//! `#speakersList`).

use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::app::StudioSpike;
use crate::ui::{section::Section, theme, widgets};
use crate::view;
use crate::view::Selection;

impl StudioSpike {
    pub(crate) fn object_and_speaker_lists(&mut self, ui: &mut egui::Ui) {
        let max_h = ui.ctx().content_rect().height();
        let (layout_name, speakers, objects): (
            String,
            Vec<String>,
            Vec<(String, String, bool, bool)>,
        ) = {
            let live = self.live.lock().unwrap();
            let name = live
                .app
                .layouts
                .iter()
                .find(|l| Some(&l.key) == live.app.selected_layout_key.as_ref())
                .map(|l| l.name.clone())
                .unwrap_or_else(|| "(none)".to_owned());
            let mut objects: Vec<(String, String, bool, bool)> = live
                .app
                .sources
                .iter()
                .map(|(id, src)| {
                    (
                        id.clone(),
                        view::objects::badge_code(id, src.name.as_deref()),
                        src.fixed.unwrap_or(false),
                        live.app.object_mutes.get(id).is_some_and(|m| *m != 0),
                    )
                })
                .collect();
            objects.sort_by(|a, b| match (a.0.parse::<u32>(), b.0.parse::<u32>()) {
                (Ok(x), Ok(y)) => x.cmp(&y),
                _ => a.0.cmp(&b.0),
            });
            (
                name,
                live.selected_speakers()
                    .iter()
                    .map(|s| s.id.clone())
                    .collect(),
                objects,
            )
        };
        ui.heading(format!(
            "{} ({})",
            crate::i18n::t("section.objects"),
            objects.len()
        ));
        egui::ScrollArea::vertical()
            .id_salt("objects-list")
            .max_height(max_h * 0.45)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for (id, code, fixed, muted) in &objects {
                    let selected = self.selection.object.as_deref() == Some(id.as_str());
                    let mut text = format!("{id:>3}  {code}");
                    if *fixed {
                        text.push_str("  (bed)");
                    }
                    if *muted {
                        text.push_str("  M");
                    }
                    if ui.selectable_label(selected, text).clicked() {
                        self.selection = Selection {
                            object: (!selected).then(|| id.clone()),
                            speaker: None,
                        };
                    }
                }
                if objects.is_empty() {
                    ui.small("No objects yet. Feed OSC or run with --synthetic 64.");
                }
            });
        ui.separator();
        ui.heading(format!(
            "{} · {layout_name}",
            crate::i18n::t("section.speakers")
        ));
        egui::ScrollArea::vertical()
            .id_salt("speakers-list")
            .max_height(max_h * 0.3)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for (index, name) in speakers.iter().enumerate() {
                    let selected = self.selection.speaker == Some(index);
                    if ui
                        .selectable_label(selected, format!("{index:>2}  {name}"))
                        .clicked()
                    {
                        self.selection = Selection {
                            object: None,
                            speaker: (!selected).then_some(index),
                        };
                    }
                }
            });
    }
}
