use super::output_runtime_sync::OutputRuntimeCoordinator;
use super::sample_write::SampleWriteCoordinator;
use super::spatial_metadata::SpatialMetadataCoordinator;
use super::state::{
    DecodeSessionState, FrameHandlerContext, OutputState, RuntimeOutputState, SpatialState,
    TelemetryState,
};
use super::writer_lifecycle::WriterLifecycleCoordinator;
use crate::cli::command::OutputBackend;
use audio_output::AudioControl;
use bridge_api::RDecodedFrame;

use anyhow::Result;
use std::sync::Arc;
use std::time::Instant;

pub(crate) struct BedChannelMapper;

pub(crate) struct ChannelCountCalculator;

impl ChannelCountCalculator {
    const TARGET_BED_CHANNELS: usize = 10; // 7.1.2 layout

    /// Calculate the effective channel count for bed conformance
    /// Returns (num_bed_channels, num_object_channels, conformed_channel_count)
    pub(crate) fn calculate_bed_conform_counts(
        original_channel_count: usize,
        bed_indices: &[usize],
    ) -> (usize, usize, usize) {
        let num_bed_channels = bed_indices.len();
        let num_object_channels = original_channel_count.saturating_sub(num_bed_channels);
        let conformed_channel_count = Self::TARGET_BED_CHANNELS + num_object_channels;
        (
            num_bed_channels,
            num_object_channels,
            conformed_channel_count,
        )
    }

    /// Calculate conformed channel count only (shorthand for common case)
    pub(crate) fn calculate_conformed_channel_count(
        original_channel_count: usize,
        bed_indices: &[usize],
    ) -> usize {
        let (_, _, conformed_count) =
            Self::calculate_bed_conform_counts(original_channel_count, bed_indices);
        conformed_count
    }
}

impl BedChannelMapper {
    pub(crate) fn apply_bed_conformance(
        original_samples: Vec<i32>,
        original_channel_count: usize,
        bed_indices: &[usize],
    ) -> Vec<i32> {
        let (num_bed_channels, num_object_channels, conformed_channel_count) =
            ChannelCountCalculator::calculate_bed_conform_counts(
                original_channel_count,
                bed_indices,
            );
        let samples_per_frame = original_samples.len() / original_channel_count;

        let mut conformed_samples = Vec::with_capacity(samples_per_frame * conformed_channel_count);

        for sample_idx in 0..samples_per_frame {
            // Handle bed channels (0-9)
            for target_bed_ch in 0..ChannelCountCalculator::TARGET_BED_CHANNELS {
                if let Some(source_ch_pos) =
                    bed_indices.iter().position(|&idx| idx == target_bed_ch)
                {
                    let sample =
                        original_samples[sample_idx * original_channel_count + source_ch_pos];
                    conformed_samples.push(sample);
                } else {
                    conformed_samples.push(0i32);
                }
            }

            // Handle object channels
            for obj_ch in 0..num_object_channels {
                let source_ch = num_bed_channels + obj_ch;
                let sample = original_samples[sample_idx * original_channel_count + source_ch];
                conformed_samples.push(sample);
            }
        }

        conformed_samples
    }

    pub(crate) fn apply_bed_conformance_to_frame(
        pcm: &[i32],
        sample_count: usize,
        channel_count: usize,
        bed_indices: &[usize],
    ) -> Vec<i32> {
        let (num_bed_channels, num_object_channels, conformed_channel_count) =
            ChannelCountCalculator::calculate_bed_conform_counts(channel_count, bed_indices);

        let mut samples = Vec::with_capacity(sample_count * conformed_channel_count);

        for sample_idx in 0..sample_count {
            // Handle bed channels (0-9)
            for target_bed_ch in 0..ChannelCountCalculator::TARGET_BED_CHANNELS {
                if let Some(source_ch_pos) =
                    bed_indices.iter().position(|&idx| idx == target_bed_ch)
                {
                    let sample = pcm[sample_idx * channel_count + source_ch_pos];
                    samples.push(sample);
                } else {
                    samples.push(0i32);
                }
            }

            // Handle object channels
            for obj_ch in 0..num_object_channels {
                let source_ch = num_bed_channels + obj_ch;
                let sample = pcm[sample_idx * channel_count + source_ch];
                samples.push(sample);
            }
        }

        samples
    }
}

pub struct DecodeHandler {
    pub output: OutputState,
    pub telemetry: TelemetryState,
    pub runtime: RuntimeOutputState,
    pub spatial: SpatialState,
    pub session: DecodeSessionState,
    pub spatial_renderer: Option<renderer::spatial_renderer::SpatialRenderer>,
    pub audio_control: Option<Arc<AudioControl>>,
}

impl Default for DecodeHandler {
    fn default() -> Self {
        Self {
            output: OutputState::default(),
            telemetry: TelemetryState::default(),
            runtime: RuntimeOutputState::default(),
            spatial: SpatialState::default(),
            session: DecodeSessionState::default(),
            spatial_renderer: None,
            audio_control: None,
        }
    }
}

impl DecodeHandler {
    pub fn handle_decoded_frame(
        &mut self,
        frame: RDecodedFrame,
        ctx: &FrameHandlerContext,
    ) -> Result<()> {
        let now = Instant::now();
        let sample_rate = frame.sampling_frequency;
        let channel_count = frame.channel_count as usize;
        let sample_count = frame.sample_count as usize;
        let sample_count_u32 = frame.sample_count;
        let metadata_count = frame.metadata.len();

        if let Some(prev_at) = self.session.last_frame_received_at {
            let wall_gap_ms = now.saturating_duration_since(prev_at).as_secs_f64() * 1000.0;
            let frame_duration_ms = sample_count as f64 / sample_rate.max(1) as f64 * 1000.0;
            let sample_count_changed = self
                .session
                .last_frame_sample_count
                .is_some_and(|prev| prev != sample_count_u32);
            let severe_gap_ms = (frame_duration_ms * 20.0).max(100.0);
            let suspicious_metadata_change = metadata_count > 0 && sample_count_changed;
            if wall_gap_ms > severe_gap_ms || suspicious_metadata_change {
                log::warn!(
                    "Decoded frame cadence anomaly: samples={} ch={} sr={} metadata={} decode_ms={:.3} queue_ms={:.3} wall_gap_ms={:.3} frame_ms={:.3} prev_samples={:?}",
                    sample_count,
                    channel_count,
                    sample_rate,
                    metadata_count,
                    ctx.decode_time_ms,
                    ctx.queue_delay_ms,
                    wall_gap_ms,
                    frame_duration_ms,
                    self.session.last_frame_sample_count
                );
            } else if wall_gap_ms > (frame_duration_ms * 1.5).max(15.0) {
                log::debug!(
                    "Decoded frame burst cadence: samples={} ch={} sr={} metadata={} decode_ms={:.3} queue_ms={:.3} wall_gap_ms={:.3} frame_ms={:.3} prev_samples={:?}",
                    sample_count,
                    channel_count,
                    sample_rate,
                    metadata_count,
                    ctx.decode_time_ms,
                    ctx.queue_delay_ms,
                    wall_gap_ms,
                    frame_duration_ms,
                    self.session.last_frame_sample_count
                );
            }
        }
        self.session.last_frame_received_at = Some(now);
        self.session.last_frame_sample_count = Some(sample_count_u32);

        self.session.decoded_frames += 1u64;
        self.session.final_sample_rate = sample_rate;
        self.spatial.au_index += 1;

        // Apply dialogue normalisation from bridge (updated on major sync frames).
        // The level is always stored so OSC clients receive loudness/source
        // and loudness/gain regardless of whether --use-loudness is set.
        if !self.spatial.loudness_applied {
            if let Some(dialogue_level) = frame.dialogue_level.into_option() {
                if let Some(ref renderer) = self.spatial_renderer {
                    renderer.set_loudness(dialogue_level);
                    self.spatial.loudness_applied = true;
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
                        osc_sender.send_loudness_state();
                    }
                }
            }
        }

        SpatialMetadataCoordinator::new(
            &mut self.spatial,
            self.spatial_renderer.as_ref(),
            self.telemetry.osc_sender.as_mut(),
        )
        .handle_spatial_metadata(&frame, frame.sampling_frequency)?;

        self.session.decoded_samples += sample_count as u64;

        let effective_channel_count = if ctx.bed_conform && self.spatial.has_objects {
            let empty_vec = Vec::new();
            let bed_indices = self.spatial.bed_indices.as_ref().unwrap_or(&empty_vec);
            ChannelCountCalculator::calculate_conformed_channel_count(channel_count, bed_indices)
        } else if let Some(ref renderer) = self.spatial_renderer {
            // When VBAP is active, output channel count is the number of speakers
            renderer.num_speakers()
        } else {
            channel_count
        };

        OutputRuntimeCoordinator::new(
            &mut self.output,
            &mut self.runtime,
            self.audio_control.as_deref(),
        )
        .sync_all(ctx.output_backend)?;
        WriterLifecycleCoordinator::new(
            &mut self.output,
            &self.runtime,
            &mut self.telemetry,
            &self.spatial,
            &self.session,
            self.spatial_renderer.as_ref(),
            self.audio_control.as_ref(),
        )
        .create_audio_writer_if_needed(ctx.output_backend, sample_rate, effective_channel_count)?;
        WriterLifecycleCoordinator::new(
            &mut self.output,
            &self.runtime,
            &mut self.telemetry,
            &self.spatial,
            &self.session,
            self.spatial_renderer.as_ref(),
            self.audio_control.as_ref(),
        )
        .publish_audio_state_if_changed(ctx.output_backend, sample_rate);

        let mut sample_write = SampleWriteCoordinator::new(
            &mut self.output,
            &mut self.telemetry,
            &mut self.spatial,
            &self.session,
            self.spatial_renderer.as_mut(),
        );
        if ctx.bed_conform && sample_write.spatial_has_objects() {
            sample_write.write_audio_samples_bed_conform(&frame, ctx.decode_time_ms)?;
        } else {
            sample_write.write_audio_samples(&frame, ctx.decode_time_ms)?;
        }

        Ok(())
    }

    pub fn finalize(&mut self) -> Result<()> {
        if let Some(ref mut writer) = self.output.audio_writer {
            writer.finish()?;
        }

        Ok(())
    }

    pub fn handle_stream_restart(
        &mut self,
        output_backend: OutputBackend,
        sample_rate: u32,
        channel_count: usize,
        bed_conform: bool,
    ) -> Result<()> {
        log::info!(
            "Stream restart detected at AU {}, resetting realtime output state",
            self.spatial.au_index
        );

        if let Some(mut writer) = self.output.invalidate_writer() {
            writer.flush()?;
        }
        self.output.bootstrap_frames_seen = 0;
        self.output.bootstrap_started_at = None;

        let effective_channel_count = if bed_conform && self.spatial.has_objects {
            let empty_vec = Vec::new();
            let bed_indices = self.spatial.bed_indices.as_ref().unwrap_or(&empty_vec);
            ChannelCountCalculator::calculate_conformed_channel_count(channel_count, bed_indices)
        } else {
            channel_count
        };
        self.output.audio_writer = Some(
            WriterLifecycleCoordinator::new(
                &mut self.output,
                &self.runtime,
                &mut self.telemetry,
                &self.spatial,
                &self.session,
                self.spatial_renderer.as_ref(),
                self.audio_control.as_ref(),
            )
            .build_audio_writer(output_backend, sample_rate, effective_channel_count, None)?,
        );
        self.reset_spatial_state_for_segment();
        Ok(())
    }

    fn reset_spatial_state_for_segment(&mut self) {
        SpatialMetadataCoordinator::new(
            &mut self.spatial,
            self.spatial_renderer.as_ref(),
            None,
        )
        .reset_for_segment();
    }

    pub fn handle_decoder_flush_request(&mut self) {
        log::info!("Received flush request after decoder reset");
        self.reset_spatial_state_for_segment();
    }
}
