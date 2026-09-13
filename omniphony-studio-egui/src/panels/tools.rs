//! Sections that belong to the native host rather than the web Studio: the
//! frame/OSC stats and the camera help.

use std::sync::atomic::Ordering;

use crate::app::StudioSpike;
use crate::ui::section::Section;

impl StudioSpike {
    pub(crate) fn tool_sections(&mut self, ui: &mut egui::Ui) {
        // Closed by default: numbers for whoever is measuring the host, not
        // something to read while listening.
        Section::titled("statsSection", "Stats").show(ui, |ui| {
            let (objects, epoch) = {
                let live = self.host.read();
                (live.app.sources.len(), live.snapshot_epoch)
            };
            egui::Grid::new("stats-grid")
                .num_columns(2)
                .spacing([16.0, 2.0])
                .show(ui, |ui| {
                    let row = |ui: &mut egui::Ui, k: &str, v: String| {
                        ui.label(k);
                        ui.monospace(v);
                        ui.end_row();
                    };
                    row(ui, "frames/s", format!("{:.1}", self.frame_stats.fps));
                    row(
                        ui,
                        "frame interval",
                        format!("{:.2} ms", self.frame_stats.frame_ms),
                    );
                    row(
                        ui,
                        "OSC packets/s",
                        format!("{:.0}", self.proc_stats.packets_per_s),
                    );
                    row(ui, "objects", objects.to_string());
                    row(ui, "snapshot epoch", epoch.to_string());
                    row(ui, "RSS", format!("{:.0} MB", self.proc_stats.rss_mb));
                    row(
                        ui,
                        "process CPU",
                        format!("{:.1} %", self.proc_stats.cpu_percent),
                    );
                    row(
                        ui,
                        "OSC ignored",
                        self.osc_stats.ignored.load(Ordering::Relaxed).to_string(),
                    );
                });
        });

        Section::titled("cameraSection", "Camera").show(ui, |ui| {
                ui.small("Drag: orbit · middle drag / wheel: dolly · right drag: pan · click: select · Esc: clear");
                if ui.button("Reset view").clicked() {
                    self.camera.reset();
                }
            });
    }
}
