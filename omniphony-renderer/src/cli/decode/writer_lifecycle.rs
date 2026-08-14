use super::output::AudioWriter;
use super::state::{
    DecodeSessionState, OutputState, RuntimeOutputState, SpatialState, TelemetryState,
};
use crate::cli::command::{OutputBackend, OutputFileFormatArg};
use anyhow::{Result, anyhow};
use audio_input::InputControl;
use audio_output::AudioControl;
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::time::Instant;

pub struct WriterLifecycleCoordinator<'a> {
    output: &'a mut OutputState,
    runtime: &'a RuntimeOutputState,
    telemetry: &'a mut TelemetryState,
    spatial: &'a SpatialState,
    session: &'a DecodeSessionState,
    spatial_renderer: Option<&'a renderer::spatial_renderer::SpatialRenderer>,
    audio_control: Option<&'a Arc<AudioControl>>,
    input_control: Option<&'a Arc<InputControl>>,
}

impl<'a> WriterLifecycleCoordinator<'a> {
    pub fn new(
        output: &'a mut OutputState,
        runtime: &'a RuntimeOutputState,
        telemetry: &'a mut TelemetryState,
        spatial: &'a SpatialState,
        session: &'a DecodeSessionState,
        spatial_renderer: Option<&'a renderer::spatial_renderer::SpatialRenderer>,
        audio_control: Option<&'a Arc<AudioControl>>,
        input_control: Option<&'a Arc<InputControl>>,
    ) -> Self {
        Self {
            output,
            runtime,
            telemetry,
            spatial,
            session,
            spatial_renderer,
            audio_control,
            input_control,
        }
    }

    pub fn create_audio_writer_if_needed(
        &mut self,
        output_backend: OutputBackend,
        sample_rate: u32,
        channel_count: usize,
    ) -> Result<()> {
        #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
        let _ = (output_backend, sample_rate, channel_count);

        if self.output.audio_writer.is_none() && !self.output.output_init_failed {
            // File/FIFO/stdout sink: cross-platform, no device clock, so no
            // bootstrap-settling gate and no pacer — create it on the first
            // frame. This block must exist or the writer is never created and
            // the renderer silently produces no output.
            if output_backend == OutputBackend::File {
                match self.build_audio_writer(output_backend, sample_rate, channel_count, None) {
                    Ok(writer) => {
                        log::info!(
                            "Writing rendered audio to '{}' ({} Hz, {} channels, format={:?})",
                            self.runtime.output_file,
                            self.runtime.output_sample_rate.unwrap_or(sample_rate),
                            channel_count,
                            self.runtime.output_file_format,
                        );
                        self.output.audio_writer = Some(writer);
                        if let Some(control) = self.audio_control {
                            control.set_audio_error(None);
                        }
                    }
                    Err(e) => {
                        if let Some(control) = self.audio_control {
                            control.set_audio_error(Some(e.to_string()));
                        }
                        log::warn!("File output initialization failed: {}", e);
                        self.output.output_init_failed = true;
                    }
                }
                return Ok(());
            }

            #[cfg(target_os = "linux")]
            if output_backend == OutputBackend::Pipewire {
                if self.spatial_renderer.is_some() {
                    self.output
                        .bootstrap_started_at
                        .get_or_insert_with(Instant::now);
                    if !self.spatial.has_objects {
                        self.output.bootstrap_frames_seen =
                            self.output.bootstrap_frames_seen.saturating_add(1);
                        if self.output.bootstrap_frames_seen < 8 {
                            return Ok(());
                        }
                    } else {
                        if self.spatial.bed_indices.is_none() {
                            self.output.bootstrap_frames_seen = 0;
                            return Ok(());
                        }
                        self.output.bootstrap_frames_seen =
                            self.output.bootstrap_frames_seen.saturating_add(1);
                        if self.output.bootstrap_frames_seen < 3 {
                            return Ok(());
                        }
                    }
                }

                log::info!(
                    "Creating PipeWire audio stream: {} Hz, {} channels (bootstrap_frames={}, has_objects={}, bed_indices={:?}, decoded_frames={}, decoded_samples={}, bootstrap_elapsed_ms={:.0})",
                    sample_rate,
                    channel_count,
                    self.output.bootstrap_frames_seen,
                    self.spatial.has_objects,
                    self.spatial.bed_indices,
                    self.session.decoded_frames,
                    self.session.decoded_samples,
                    self.output
                        .bootstrap_started_at
                        .map(|t| t.elapsed().as_secs_f64() * 1000.0)
                        .unwrap_or(0.0)
                );

                // PipeWire channel naming follows the output channel mapping:
                // `by_name` tags each channel with its speaker position (FC, …) so
                // PipeWire routes positionally; `by_index` uses positionless AUX
                // names so the link is 1:1 by index (port N → layout speaker N).
                let mapping = self
                    .spatial_renderer
                    .map(|r| r.renderer_control().live.read().output_channel_mapping)
                    .unwrap_or_default();
                let channel_names = match mapping {
                    renderer::live_params::OutputChannelMapping::ByName => {
                        self.spatial_renderer.map(|renderer| {
                            renderer
                                .speaker_names()
                                .iter()
                                .map(|s| s.to_string())
                                .collect::<Vec<_>>()
                        })
                    }
                    renderer::live_params::OutputChannelMapping::ByIndex => Some(
                        (0..channel_count)
                            .map(|i| format!("AUX{i}"))
                            .collect::<Vec<_>>(),
                    ),
                };
                match self.build_audio_writer(
                    output_backend,
                    sample_rate,
                    channel_count,
                    channel_names,
                ) {
                    Ok(writer) => {
                        // Wire the post-rendering pacer into the input
                        // control before stashing the writer, so the input
                        // PwStream callback can drain the FIFO on its next
                        // tick.
                        if let (Some(ic), Some(handle)) =
                            (self.input_control, writer.pacer_handle())
                        {
                            ic.install_output_pacer(handle);
                        }
                        self.output.audio_writer = Some(writer);
                        self.output.bootstrap_frames_seen = 0;
                        self.output.bootstrap_started_at = None;
                        if let Some(control) = self.audio_control {
                            control.set_audio_error(None);
                        }
                    }
                    Err(e) => {
                        if let Some(control) = self.audio_control {
                            control.set_audio_error(Some(e.to_string()));
                        }
                        log::warn!(
                            "Output backend initialization failed, waiting for a valid config: {}",
                            e
                        );
                        self.output.output_init_failed = true;
                    }
                }
                return Ok(());
            }

            #[cfg(any(target_os = "windows", target_os = "macos"))]
            if output_backend.is_local_cpal() {
                if let Some(output_rate) = self.runtime.output_sample_rate {
                    log::info!(
                        "Creating realtime audio stream with upsampling: {} Hz -> {} Hz, {} channels",
                        sample_rate,
                        output_rate,
                        channel_count
                    );
                } else {
                    log::info!(
                        "Creating realtime audio stream: {} Hz, {} channels",
                        sample_rate,
                        channel_count
                    );
                }

                match self.build_audio_writer(output_backend, sample_rate, channel_count, None) {
                    Ok(writer) => {
                        self.output.audio_writer = Some(writer);
                        if let Some(control) = self.audio_control {
                            control.set_audio_error(None);
                        }
                    }
                    Err(e) => {
                        if let Some(control) = self.audio_control {
                            control.set_audio_error(Some(e.to_string()));
                        }
                        log::warn!(
                            "Output backend initialization failed, waiting for a valid config: {}",
                            e
                        );
                        self.output.output_init_failed = true;
                    }
                }
                return Ok(());
            }
        }
        Ok(())
    }

    pub fn publish_audio_state_if_changed(
        &mut self,
        output_backend: OutputBackend,
        input_sample_rate: u32,
    ) {
        let effective_output_device = self.runtime.output_device.clone();
        let (effective_rate, sample_format) = effective_audio_state(
            output_backend,
            input_sample_rate,
            self.runtime.output_sample_rate,
        );

        if self.output.last_audio_sample_rate_hz == Some(effective_rate)
            && self.output.last_audio_sample_format.as_deref() == Some(sample_format)
            && self.output.last_audio_output_device == effective_output_device
        {
            return;
        }

        self.output.last_audio_sample_rate_hz = Some(effective_rate);
        self.output.last_audio_sample_format = Some(sample_format.to_string());
        self.output.last_audio_output_device = effective_output_device.clone();

        if let Some(control) = self.audio_control {
            control.set_effective_output_device(effective_output_device);
            control.set_audio_state(effective_rate, sample_format);
        }
        if self
            .telemetry
            .osc_sender
            .as_ref()
            .is_some_and(|sender| sender.has_osc_clients())
        {
            let osc_sender = self
                .telemetry
                .osc_sender
                .as_ref()
                .expect("osc_sender present");
            // The audio-state broadcast lives in host_audio's extend_snapshot;
            // re-emit the full live-state bundle to refresh it after the
            // output stream is (re)configured.
            let _ = (effective_rate, sample_format);
            if let Err(e) = osc_sender.send_live_state_bundle() {
                log::warn!("Failed to send OSC state bundle: {}", e);
            }
        }
    }

    pub fn build_audio_writer(
        &self,
        output_backend: OutputBackend,
        sample_rate: u32,
        channel_count: usize,
        #[cfg(target_os = "linux")] pipewire_channel_names: Option<Vec<String>>,
        #[cfg(not(target_os = "linux"))] _pipewire_channel_names: Option<Vec<String>>,
    ) -> Result<AudioWriter> {
        #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
        let _ = (output_backend, sample_rate, channel_count);

        match output_backend {
            #[cfg(target_os = "linux")]
            OutputBackend::Pipewire => {
                // Pre-bridge clock atomic: when no live input is wired up
                // (file decode path) the writer gets a zero-initialised
                // atomic that never increments, so `use_pre_bridge_clock`
                // simply leaves the PI frozen on a zero source-clock
                // signal (bootstrap-freeze branch), which is exactly the
                // desired safe behaviour.
                let input_clock_us = self
                    .input_control
                    .map(|ic| ic.input_clock_us_atomic())
                    .unwrap_or_else(|| Arc::new(std::sync::atomic::AtomicU64::new(0)));
                if let Some(names) = pipewire_channel_names {
                    Ok(AudioWriter::create_pipewire_with_channel_names(
                        sample_rate,
                        channel_count as u32,
                        self.runtime.output_device.clone(),
                        names,
                        self.runtime.enable_adaptive_resampling,
                        self.runtime.output_sample_rate,
                        self.runtime.pw_buffer_config.clone(),
                        self.runtime.adaptive_resampling_config.clone(),
                        input_clock_us,
                    )?)
                } else {
                    Ok(AudioWriter::create_pipewire(
                        sample_rate,
                        channel_count as u32,
                        self.runtime.output_device.clone(),
                        self.runtime.enable_adaptive_resampling,
                        self.runtime.output_sample_rate,
                        self.runtime.pw_buffer_config.clone(),
                        self.runtime.adaptive_resampling_config.clone(),
                        input_clock_us,
                    )?)
                }
            }
            #[cfg(target_os = "windows")]
            OutputBackend::Asio => self.build_cpal_audio_writer(sample_rate, channel_count),
            #[cfg(target_os = "macos")]
            OutputBackend::Coreaudio => self.build_cpal_audio_writer(sample_rate, channel_count),
            OutputBackend::File => {
                let format = match self.runtime.output_file_format {
                    OutputFileFormatArg::RawF32 => audio_output::FileSinkFormat::RawF32,
                    OutputFileFormatArg::Caf => audio_output::FileSinkFormat::Caf,
                };
                // Embed the speaker geometry in the CAF `chan` chunk so the
                // capture is self-describing for arbitrary spatial layouts.
                let channel_descs = self.spatial_renderer.map(|renderer| {
                    renderer
                        .speaker_layout()
                        .speakers
                        .iter()
                        .map(|s| {
                            audio_output::CafChannelDesc::spherical(
                                s.azimuth,
                                s.elevation,
                                s.distance,
                            )
                        })
                        .collect::<Vec<_>>()
                });
                AudioWriter::create_file(
                    &self.runtime.output_file,
                    format,
                    self.runtime.output_sample_rate.unwrap_or(sample_rate),
                    channel_count as u32,
                    channel_descs,
                )
            }
            OutputBackend::Unsupported => Err(anyhow!("No supported realtime output backend")),
        }
    }

    /// Build the cpal-backed realtime writer — ASIO on Windows, CoreAudio on
    /// macOS. Both platforms feed the same `CpalWriter` via `create_cpal`.
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    fn build_cpal_audio_writer(
        &self,
        sample_rate: u32,
        channel_count: usize,
    ) -> Result<AudioWriter> {
        let effective_sample_rate = self.runtime.output_sample_rate.unwrap_or(sample_rate);
        AudioWriter::create_cpal(
            sample_rate,
            effective_sample_rate,
            channel_count as u32,
            self.runtime.output_device.clone(),
            self.runtime.latency_target_ms,
            self.runtime.enable_adaptive_resampling,
            self.runtime.adaptive_resampling_config.clone(),
        )
    }
}

fn effective_audio_state(
    output_backend: OutputBackend,
    input_sample_rate: u32,
    output_rate: Option<u32>,
) -> (u32, &'static str) {
    match output_backend {
        #[cfg(target_os = "linux")]
        OutputBackend::Pipewire => (output_rate.unwrap_or(input_sample_rate), "f32le"),
        #[cfg(target_os = "windows")]
        OutputBackend::Asio => (output_rate.unwrap_or(input_sample_rate), "f32le"),
        #[cfg(target_os = "macos")]
        OutputBackend::Coreaudio => (output_rate.unwrap_or(input_sample_rate), "f32le"),
        OutputBackend::File => (output_rate.unwrap_or(input_sample_rate), "f32le"),
        _ => (input_sample_rate, "s24le"),
    }
}
