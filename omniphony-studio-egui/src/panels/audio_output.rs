//! The Audio Output section (`#audioOutputSection`, `ui/audio-panel.js`,
//! `controls/audio.js`): the format line, the output backend, the device or
//! file destination, the channel mapping and the sample rate.
//!
//! Several of these controls share one batched configuration message
//! (`sendAudioConfig`), which the host resolves before sending; the resolver
//! is `host::audio_config`, copied verbatim from the Tauri host.

use egui::{RichText, Ui};

use crate::app::StudioSpike;
use crate::host::commands::audio;
use crate::i18n::{t, tf};
use crate::ui::section::Section;
use crate::ui::{theme, widgets};

/// `AUDIO_SAMPLE_RATE_PRESETS` (`state.js`).
const SAMPLE_RATE_PRESETS: &[u32] = &[0, 32000, 44100, 48000, 88200, 96000, 176400, 192000];

const FILE_FORMATS: &[(&str, &str)] = &[
    ("raw_f32", "audio.formatRawF32"),
    ("caf", "audio.formatCaf"),
];

impl StudioSpike {
    pub(crate) fn audio_output_section(&mut self, ui: &mut Ui) {
        let (audio, devices, ready, unroutable, mapping) = {
            let live = self.host.read();
            (
                live.app.audio.clone(),
                live.app.audio.audio_output_devices.clone(),
                live.app.osc_snapshot_ready,
                live.app
                    .live_options
                    .output_channel_mapping_unroutable
                    .clone()
                    .unwrap_or_default(),
                live.option_str("output_channel_mapping")
                    .unwrap_or_else(|| "by_index".to_owned()),
            )
        };
        let file_backend = audio.audio_output_backend.as_deref() == Some("file");
        Section::new("audioOutputSection", "section.audioOutput")
            .summary(summary(&audio, &devices, ready))
            .show(ui, |ui| {
                // The format line: what the engine actually opened.
                let rate = audio
                    .audio_sample_rate
                    .filter(|r| *r > 0)
                    .map(|r| format!("{r} Hz"))
                    .unwrap_or_else(|| "—".to_owned());
                let format = audio
                    .audio_sample_format
                    .clone()
                    .unwrap_or_else(|| "—".into());
                let mut line = tf(
                    "status.audioFormat",
                    &[("rate", &rate), ("format", &format)],
                );
                if let Some(error) = &audio.audio_error {
                    line.push_str(&format!(" • Error: {error}"));
                }
                widgets::note(ui, &line);
                ui.add_enabled_ui(ready, |ui| {
                    self.output_backend_row(ui, file_backend);
                    if file_backend {
                        self.file_rows(ui, &audio);
                    } else {
                        self.device_row(ui, &audio, &devices, ready);
                    }
                    self.channel_mapping_row(ui, &mapping, &unroutable);
                    self.sample_rate_row(ui, audio.audio_sample_rate.unwrap_or(0));
                });
            });
    }

    fn output_backend_row(&mut self, ui: &mut Ui, file_backend: bool) {
        let current = if file_backend { "file" } else { "device" };
        let mut chosen = current.to_owned();
        widgets::label_row_help(
            ui,
            t("audio.outputBackend"),
            "help.audio.outputBackend",
            |ui| {
                widgets::bounded_combo(ui, 150.0, |ui, w| {
                    egui::ComboBox::from_id_salt("audio-output-backend")
                        .selected_text(t(if file_backend {
                            "audio.backendFile"
                        } else {
                            "audio.backendDevice"
                        }))
                        .width(w)
                        .truncate()
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut chosen,
                                "device".to_owned(),
                                t("audio.backendDevice"),
                            );
                            ui.selectable_value(
                                &mut chosen,
                                "file".to_owned(),
                                t("audio.backendFile"),
                            );
                        })
                });
            },
        );
        if chosen != current {
            // Deliberately outside the batched config, like the web panel.
            audio::control_audio_output_backend(&self.host, chosen);
        }
    }

    fn device_row(
        &mut self,
        ui: &mut Ui,
        audio: &crate::model::app_state::RuntimeAudioState,
        devices: &[crate::model::app_state::OutputDeviceOption],
        ready: bool,
    ) {
        let current = audio.audio_output_device.clone().unwrap_or_default();
        let default_label = if ready {
            t("status.defaultOutputDevice")
        } else {
            "—"
        };
        let label_of = |value: &str| -> String {
            if value.is_empty() {
                return default_label.to_owned();
            }
            devices
                .iter()
                .find(|d| d.value == value)
                .map(|d| d.label.clone())
                .unwrap_or_else(|| value.to_owned())
        };
        let mut chosen = current.clone();
        widgets::label_row_help(
            ui,
            t("audio.outputDevice"),
            "help.audio.outputDevice",
            |ui| {
                if ui
                    .small_button("↺")
                    .on_hover_text(t("audio.refreshDevices"))
                    .clicked()
                {
                    audio::refresh_output_devices(&self.host);
                }
                // Device names are long (`alsa_output.usb-…iec958-stereo`): the
                // list is cut to its row and says the whole name on hover.
                widgets::bounded_combo(ui, 180.0, |ui, w| {
                    egui::ComboBox::from_id_salt("audio-output-device")
                        .selected_text(label_of(&current))
                        .width(w)
                        .truncate()
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut chosen, String::new(), default_label);
                            for device in devices {
                                ui.selectable_value(
                                    &mut chosen,
                                    device.value.clone(),
                                    &device.label,
                                );
                            }
                        })
                })
                .response
                .on_hover_text(label_of(&current));
            },
        );
        if chosen != current {
            audio::set_output_device(&self.host, chosen);
        }
    }

    fn file_rows(&mut self, ui: &mut Ui, audio: &crate::model::app_state::RuntimeAudioState) {
        let file = audio
            .audio_output_file
            .clone()
            .unwrap_or_else(|| "-".into());
        let mut named_pipe = file != "-";
        if widgets::switch_row_help(
            ui,
            t("audio.namedPipe"),
            "help.audio.namedPipe",
            &mut named_pipe,
        ) {
            if named_pipe {
                // Restore the path the switch remembered, if there is one.
                audio::set_output_file(&self.host, self.audio_pipe_path.clone());
            } else {
                self.audio_pipe_path = file.clone();
                audio::set_output_file(&self.host, "-".to_owned());
            }
        }
        if named_pipe {
            let path =
                widgets::label_row_help(ui, t("audio.outputFile"), "help.audio.outputFile", |ui| {
                    self.output_path_edit.show(
                        ui,
                        "output-file",
                        if file == "-" { "" } else { &file },
                        "/path/to/fifo",
                        180.0,
                        true,
                    )
                });
            if let Some(path) = path {
                if !path.is_empty() {
                    self.audio_pipe_path = path.clone();
                }
                audio::set_output_file(&self.host, path);
            }
        }
        let current = audio
            .audio_output_file_format
            .clone()
            .filter(|f| FILE_FORMATS.iter().any(|(id, _)| id == f))
            .unwrap_or_else(|| "raw_f32".to_owned());
        let mut chosen = current.clone();
        widgets::label_row_help(
            ui,
            t("audio.outputFileFormat"),
            "help.audio.outputFileFormat",
            |ui| {
                widgets::bounded_combo(ui, 150.0, |ui, w| {
                    egui::ComboBox::from_id_salt("audio-file-format")
                        .selected_text(t(FILE_FORMATS
                            .iter()
                            .find(|(id, _)| *id == current)
                            .map(|(_, key)| *key)
                            .unwrap_or("audio.formatRawF32")))
                        .width(w)
                        .truncate()
                        .show_ui(ui, |ui| {
                            for (id, key) in FILE_FORMATS {
                                ui.selectable_value(&mut chosen, (*id).to_owned(), t(key));
                            }
                        })
                });
            },
        );
        if chosen != current {
            audio::control_audio_output_file_format(&self.host, chosen);
        }
    }

    fn channel_mapping_row(&mut self, ui: &mut Ui, mapping: &str, unroutable: &[String]) {
        widgets::label_row_help(
            ui,
            t("audio.channelMapping"),
            "help.audio.channelMapping",
            |ui| {
                if let Some(picked) = widgets::toggle_buttons(
                    ui,
                    &mapping.to_owned(),
                    &[
                        ("by_index".to_owned(), t("audio.channelMapping.byIndex")),
                        ("by_name".to_owned(), t("audio.channelMapping.byName")),
                    ],
                ) {
                    self.set_option("output_channel_mapping", serde_json::json!(picked));
                }
            },
        );
        // Speakers the renderer could not place by name are a real routing
        // hole, so the warning stays visible while it lasts.
        if mapping == "by_name" && !unroutable.is_empty() {
            ui.label(
                RichText::new(format!(
                    "{} {}",
                    t("audio.channelMapping.warning"),
                    unroutable.join(", ")
                ))
                .size(theme::FONT_SIZE_SMALL)
                .color(theme::WARN),
            );
        }
    }

    /// The rate is a text field with a menu of presets beside it, not a plain
    /// select: a device may run at a rate nobody thought to list, and the
    /// renderer accepts any of them. The presets are a shortcut, not the set.
    fn sample_rate_row(&mut self, ui: &mut Ui, current: u32) {
        let mut apply: Option<u32> = None;
        widgets::label_row_help(ui, t("audio.sampleRate"), "help.audio.sampleRate", |ui| {
            // A preset picks a value *and* applies it, as the web's menu
            // does.
            egui::ComboBox::from_id_salt("audio-sample-rate")
                .selected_text("▾")
                .width(34.0)
                .show_ui(ui, |ui| {
                    for rate in SAMPLE_RATE_PRESETS {
                        if ui
                            .selectable_label(*rate == current, rate_label(*rate))
                            .clicked()
                        {
                            apply = Some(*rate);
                        }
                    }
                });
            // Not overwritten while it is being typed in: the field holds
            // what the user is writing, not what the renderer last said.
            if self.sample_rate_edit.is_none() {
                self.sample_rate_edit = Some(current.to_string());
            }
            let text = self.sample_rate_edit.get_or_insert_with(String::new);
            let response = ui.add(
                egui::TextEdit::singleline(text)
                    .desired_width(90.0)
                    .font(egui::FontId::proportional(theme::FONT_SIZE)),
            );
            if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                apply = Some(text.trim().parse::<u32>().unwrap_or(0));
            }
            if !response.has_focus() && apply.is_none() {
                // Back to the renderer's answer as soon as the field is
                // left, so an abandoned edit does not linger as a claim.
                *text = current.to_string();
            }
        });
        if let Some(rate) = apply {
            self.sample_rate_edit = Some(rate.to_string());
            audio::set_sample_rate(&self.host, rate);
        }
    }
}

/// `Native (0)` for the zero preset, `48000 Hz` otherwise.
fn rate_label(rate: u32) -> String {
    if rate == 0 {
        t("status.nativeRate").to_owned()
    } else {
        format!("{rate} Hz")
    }
}

/// The collapsed header's line: device, rate, format, plus any error.
fn summary(
    audio: &crate::model::app_state::RuntimeAudioState,
    devices: &[crate::model::app_state::OutputDeviceOption],
    ready: bool,
) -> String {
    let value = audio
        .audio_output_device_effective
        .clone()
        .or_else(|| audio.audio_output_device.clone())
        .unwrap_or_default();
    let device = if value.is_empty() {
        if ready {
            t("status.defaultOutputDevice").to_owned()
        } else {
            "—".to_owned()
        }
    } else {
        devices
            .iter()
            .find(|d| d.value == value)
            .map(|d| d.label.clone())
            .unwrap_or(value)
    };
    let rate = audio
        .audio_sample_rate
        .filter(|r| *r > 0)
        .map(|r| format!("{r} Hz"))
        .unwrap_or_else(|| "—".to_owned());
    let format = audio
        .audio_sample_format
        .clone()
        .unwrap_or_else(|| "—".to_owned());
    let mut summary = tf(
        "audio.summary",
        &[("device", &device), ("rate", &rate), ("format", &format)],
    );
    if let Some(error) = &audio.audio_error {
        summary.push_str(&format!(" • Error: {error}"));
    }
    summary
}
