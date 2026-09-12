//! The speakers header's layout actions (`.speakers-layout-actions`,
//! `listeners/layout-listeners.js`, `speakers.js:303–367`, host
//! `commands/layout_io.rs`).
//!
//! Presets and Import read a layout file into Studio's own list *and* push it
//! to the renderer, because a layout that only exists on this side desyncs the
//! two: the renderer keeps rendering the old one and a save persists the wrong
//! thing. Export writes the live layout back out. Add appends a speaker at the
//! selected one's pose, which is almost always where the next one goes.

use egui::Ui;

use crate::app::StudioSpike;
use crate::host::commands::HostPaths;
use crate::host::commands::speakers;
use crate::i18n::{t, tf};
use crate::model::layouts::{Layout, Speaker};

/// The renderer's own mirror of the live layout. Pushing it back would echo
/// what the renderer just said.
const LIVE_LAYOUT_KEY: &str = "omniphony-live";

/// Which picker an import starts from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pick {
    /// The bundled presets folder.
    Presets,
    /// Wherever the user last imported from.
    Import,
}

impl StudioSpike {
    /// Presets, Import, Export, Add — right of the Speakers header.
    ///
    /// All four are refused while the backend has the speakers frozen: the
    /// layout is what it is precomputing against, and changing it underneath
    /// would invalidate the table it is using.
    pub(crate) fn layout_actions(&mut self, ui: &mut Ui) {
        let frozen = {
            let live = self.live.lock().unwrap();
            live.app.render_backend_state.frozen_speakers
        };
        // Four buttons need ~350 points on one line, more than the panel's
        // minimum width. Laid out on a plain row they overflowed it, and the
        // panel's frame — anchored to the right edge — grew leftward to hold
        // them: every other section then looked narrower than the panel, and
        // anything placed from the configured width landed inside it. They
        // wrap instead, as the web's flex row does.
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(!frozen, |ui| {
                if ui
                    .button(t("config.presets"))
                    .on_hover_text(t("config.presetsHint"))
                    .clicked()
                {
                    self.import_layout(Pick::Presets);
                }
                if ui.button(t("config.import")).clicked() {
                    self.import_layout(Pick::Import);
                }
                if ui.button(t("config.export")).clicked() {
                    self.export_layout();
                }
                if ui.button(format!("+ {}", t("speaker.add"))).clicked() {
                    self.add_speaker();
                }
            });
        });
    }

    fn import_layout(&mut self, pick: Pick) {
        let paths = HostPaths::default();
        let state = self.host.clone();
        let path = match pick {
            Pick::Presets => crate::ui::file_dialogs::pick_preset_layout_path(&paths),
            Pick::Import => crate::ui::file_dialogs::pick_import_layout_path(&paths, &state),
        };
        // An empty answer is a cancelled dialog, which is not a failure.
        let Some(path) = path.filter(|p| !p.trim().is_empty()) else {
            return;
        };
        self.log(
            "info",
            "layout",
            tf("log.layoutImportRequested", &[("path", &path)]),
        );
        match crate::host::commands::layout_io::import_layout_from_path(&state, path.clone()) {
            Ok(payload) => {
                let key = payload
                    .get("selectedLayoutKey")
                    .and_then(|k| k.as_str())
                    .map(str::to_owned);
                if let Some(key) = key {
                    self.apply_layout_to_renderer(&key);
                }
                self.selection.speaker = None;
                self.log(
                    "info",
                    "layout",
                    tf("log.layoutImported", &[("path", &path)]),
                );
            }
            Err(error) => self.log(
                "error",
                "layout",
                tf("log.layoutImportFailed", &[("error", &error)]),
            ),
        }
    }

    /// Push a whole layout to the renderer and commit it. Without this an
    /// import would change only Studio's picture of the room.
    fn apply_layout_to_renderer(&mut self, key: &str) {
        if key.is_empty() || key == LIVE_LAYOUT_KEY {
            return;
        }
        let payload = {
            let live = self.live.lock().unwrap();
            let Some(layout) = live.app.layouts.iter().find(|l| l.key == key) else {
                return;
            };
            replace_layout_payload(layout)
        };
        speakers::apply_layout_document(&self.host, payload);
    }

    fn export_layout(&mut self) {
        let layout = {
            let live = self.live.lock().unwrap();
            let key = live.app.selected_layout_key.clone();
            live.app
                .layouts
                .iter()
                .find(|l| Some(&l.key) == key.as_ref())
                .cloned()
        };
        let Some(layout) = layout else { return };
        let suggested =
            crate::host::commands::layout_io::default_layout_export_name(layout.clone());
        let Some(path) = crate::ui::file_dialogs::pick_export_layout_path(Some(suggested))
            .filter(|p| !p.trim().is_empty())
        else {
            return;
        };
        let exported = path.clone();
        match crate::host::commands::layout_io::export_layout_to_path(path, layout) {
            Ok(()) => self.log(
                "info",
                "layout",
                tf("log.layoutExported", &[("path", &exported)]),
            ),
            Err(error) => self.log(
                "error",
                "layout",
                tf("log.layoutExportFailed", &[("error", &error)]),
            ),
        }
    }

    /// Append a speaker at the selected one's pose. A new speaker is nearly
    /// always a sibling of the one being looked at, and an origin default would
    /// put it inside the listener's head.
    fn add_speaker(&mut self) {
        let (name, speaker) = {
            let live = self.live.lock().unwrap();
            let speakers = live.selected_speakers();
            let base = self
                .selection
                .speaker
                .and_then(|i| speakers.get(i))
                .cloned();
            let name = format!("spk-{}", speakers.len());
            (name, base)
        };
        let base = speaker.unwrap_or_else(|| Speaker {
            id: String::new(),
            x: 0.0,
            y: 0.0,
            z: 0.0,
            azimuth_deg: 0.0,
            elevation_deg: 0.0,
            distance_m: 1.0,
            coord_mode: "polar".to_owned(),
            spatialize: 1,
            delay_ms: 0.0,
            freq_low: None,
            freq_high: None,
        });
        speakers::apply_layout_document(
            &self.host,
            serde_json::json!({ "addSpeaker": {
                "name": name,
                "azimuth": base.azimuth_deg,
                "elevation": base.elevation_deg,
                "distance": base.distance_m.max(0.01),
                "spatialize": base.spatialize != 0,
                "delayMs": base.delay_ms.max(0.0),
            }}),
        );
    }

    /// One line into the log overlay.
    pub(crate) fn log(&self, level: &str, target: &str, message: impl Into<String>) {
        crate::host::commands::app::push_log(&self.host, level, target, message.into());
    }
}

/// `buildReplaceLayoutPayload`: the whole layout as one patch, with the
/// frontend's clamps — which are the ones that matter, since the web never uses
/// the host's single-field helpers.
fn replace_layout_payload(layout: &Layout) -> serde_json::Value {
    let speakers: Vec<serde_json::Value> = layout
        .speakers
        .iter()
        .enumerate()
        .map(|(index, speaker)| {
            let name = if speaker.id.is_empty() {
                format!("spk-{index}")
            } else {
                speaker.id.clone()
            };
            serde_json::json!({
                "name": name,
                "coordMode": speaker.coord_mode,
                "x": speaker.x.clamp(-1.0, 1.0),
                "y": speaker.y.clamp(-1.0, 1.0),
                "z": speaker.z.clamp(-1.0, 1.0),
                "azimuth": finite_or(speaker.azimuth_deg, 0.0),
                "elevation": finite_or(speaker.elevation_deg, 0.0),
                "distance": number_or(speaker.distance_m, 1.0).max(0.01),
                "spatialize": speaker.spatialize != 0,
                "delayMs": speaker.delay_ms.max(0.0),
                // A band edge of zero means "no edge", which is null on the
                // wire and not a 0 Hz corner.
                "freqLow": positive(speaker.freq_low),
                "freqHigh": positive(speaker.freq_high),
            })
        })
        .collect();
    serde_json::json!({ "replaceLayout": {
        "radiusM": number_or(layout.radius_m, 1.0).max(0.01),
        "speakers": speakers,
    }})
}

/// The web's `Number.isFinite(v) ? v : fallback`: an honest zero survives.
fn finite_or(value: f64, fallback: f64) -> f64 {
    if value.is_finite() { value } else { fallback }
}

/// The web's `Number(v) || fallback`: a zero is not a radius or a distance, so
/// it falls back rather than collapsing the room.
fn number_or(value: f64, fallback: f64) -> f64 {
    if value.is_finite() && value != 0.0 {
        value
    } else {
        fallback
    }
}

fn positive(value: Option<f32>) -> Option<f32> {
    value.filter(|v| v.is_finite() && *v > 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speaker(id: &str) -> Speaker {
        Speaker {
            id: id.to_owned(),
            x: 2.0,
            y: -3.0,
            z: 0.5,
            azimuth_deg: 30.0,
            elevation_deg: 0.0,
            distance_m: 0.0,
            coord_mode: "cartesian".to_owned(),
            spatialize: 0,
            delay_ms: -5.0,
            freq_low: Some(0.0),
            freq_high: Some(120.0),
        }
    }

    #[test]
    fn the_replace_patch_carries_the_frontends_own_clamps() {
        let layout = Layout {
            key: "k".to_owned(),
            name: "n".to_owned(),
            speakers: vec![
                speaker("L"),
                Speaker {
                    id: String::new(),
                    ..speaker("")
                },
            ],
            radius_m: 0.0,
        };
        let payload = replace_layout_payload(&layout);
        let patch = &payload["replaceLayout"];
        // A zero radius is not a room: it falls back rather than collapsing.
        assert_eq!(patch["radiusM"], 1.0);
        let first = &patch["speakers"][0];
        assert_eq!(first["x"], 1.0, "out-of-cube coordinates are clamped");
        assert_eq!(first["y"], -1.0);
        // A zero distance is not a distance: it falls back to one metre
        // rather than collapsing the speaker onto the listener.
        assert_eq!(first["distance"], 1.0);
        assert_eq!(first["delayMs"], 0.0, "a negative delay is not a delay");
        assert_eq!(first["spatialize"], false);
        // An empty band edge is null, not a 0 Hz corner.
        assert!(first["freqLow"].is_null());
        assert_eq!(first["freqHigh"], 120.0);
        // An unnamed speaker gets its index, so the renderer can address it.
        assert_eq!(patch["speakers"][1]["name"], "spk-1");
    }
}
