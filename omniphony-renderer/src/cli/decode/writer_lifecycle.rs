use super::state::{
    DecodeSessionState, OutputState, RuntimeOutputState, SpatialState, TelemetryState,
};
use super::output::AudioWriter;
use crate::cli::command::OutputBackend;
use anyhow::{anyhow, Result};
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
    ) -> Self {
        Self {
            output,
            runtime,
            telemetry,
            spatial,
            session,
            spatial_renderer,
            audio_control,
        }
    }

    pub fn create_audio_writer_if_needed(
        &mut self,
        output_backend: OutputBackend,
        sample_rate: u32,
        channel_count: usize,
    ) -> Result<()> {
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        let _ = (output_backend, sample_rate, channel_count);

        if self.output.audio_writer.is_none() && !self.output.output_init_failed {
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

                let speaker_names = self.spatial_renderer.map(|renderer| {
                    renderer
                        .speaker_names()
                        .iter()
                        .map(|s| s.to_string())
                        .collect::<Vec<_>>()
                });
                match self.build_audio_writer(
                    output_backend,
                    sample_rate,
                    channel_count,
                    speaker_names,
                ) {
                    Ok(writer) => {
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

            #[cfg(target_os = "windows")]
            if output_backend == OutputBackend::Asio {
                if let Some(output_rate) = self.runtime.output_sample_rate {
                    log::info!(
                        "Creating ASIO audio stream with upsampling: {} Hz -> {} Hz, {} channels",
                        sample_rate,
                        output_rate,
                        channel_count
                    );
                } else {
                    log::info!(
                        "Creating ASIO audio stream: {} Hz, {} channels",
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
        let (effective_rate, sample_format) =
            effective_audio_state(output_backend, input_sample_rate, self.runtime.output_sample_rate);

        if self.output.last_audio_sample_rate_hz == Some(effective_rate)
            && self.output.last_audio_sample_format.as_deref() == Some(sample_format)
        {
            return;
        }

        self.output.last_audio_sample_rate_hz = Some(effective_rate);
        self.output.last_audio_sample_format = Some(sample_format.to_string());

        if let Some(control) = self.audio_control {
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
            if let Err(e) = osc_sender.send_audio_state(effective_rate, sample_format) {
                log::warn!("Failed to send OSC audio state: {}", e);
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
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        let _ = (output_backend, sample_rate, channel_count);

        match output_backend {
            #[cfg(target_os = "linux")]
            OutputBackend::Pipewire => {
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
                    )?)
                }
            }
            #[cfg(target_os = "windows")]
            OutputBackend::Asio => {
                let effective_sample_rate = self.runtime.output_sample_rate.unwrap_or(sample_rate);
                Ok(AudioWriter::create_asio(
                    sample_rate,
                    effective_sample_rate,
                    channel_count as u32,
                    self.runtime.output_device.clone(),
                    self.runtime.latency_target_ms,
                    self.runtime.enable_adaptive_resampling,
                    self.runtime.adaptive_resampling_config.clone(),
                )?)
            }
            OutputBackend::Unsupported => Err(anyhow!("No supported realtime output backend")),
        }
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
        _ => (input_sample_rate, "s24le"),
    }
}
