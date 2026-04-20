use super::handler::{BedChannelMapper, ChannelCountCalculator};
use super::output::AudioSamples;
use super::state::{DecodeSessionState, OutputState, SpatialState, TelemetryState};
use super::virtual_bed::{build_virtual_bed_events, build_virtual_bed_objects};
use anyhow::Result;
use audio_input::InputControl;
use bridge_api::RChannelLabel;
use bridge_api::RDecodedFrame;
use std::time::Instant;

pub struct SampleWriteCoordinator<'a> {
    output: &'a mut OutputState,
    telemetry: &'a mut TelemetryState,
    spatial: &'a mut SpatialState,
    session: &'a DecodeSessionState,
    input_control: Option<&'a InputControl>,
    spatial_renderer: Option<&'a mut renderer::spatial_renderer::SpatialRenderer>,
}

impl<'a> SampleWriteCoordinator<'a> {
    pub fn spatial_has_objects(&self) -> bool {
        self.spatial.has_objects
    }

    pub fn new(
        output: &'a mut OutputState,
        telemetry: &'a mut TelemetryState,
        spatial: &'a mut SpatialState,
        session: &'a DecodeSessionState,
        input_control: Option<&'a InputControl>,
        spatial_renderer: Option<&'a mut renderer::spatial_renderer::SpatialRenderer>,
    ) -> Self {
        Self {
            output,
            telemetry,
            spatial,
            session,
            input_control,
            spatial_renderer,
        }
    }

    pub fn write_audio_samples(
        &mut self,
        frame: &RDecodedFrame,
        decode_time_ms: f32,
    ) -> Result<()> {
        let channel_count = frame.channel_count as usize;
        let sample_count = frame.sample_count as usize;
        let frame_duration_ms =
            sample_count as f32 / frame.sampling_frequency.max(1) as f32 * 1000.0;

        let current_latency_instant_ms: Option<f32> = self
            .output
            .audio_writer
            .as_ref()
            .and_then(|w| w.measured_audio_delay_ms());
        let current_latency_control_ms: Option<f32> = self
            .output
            .audio_writer
            .as_ref()
            .and_then(|w| w.control_audio_delay_ms());
        let current_latency_target_ms: Option<f32> = self
            .output
            .audio_writer
            .as_ref()
            .and_then(|w| w.total_audio_delay_ms());
        let current_resample_ratio: Option<f32> = self
            .output
            .audio_writer
            .as_ref()
            .and_then(|w| w.resample_ratio());
        let current_adaptive_band: Option<&'static str> = self
            .output
            .audio_writer
            .as_ref()
            .and_then(|w| w.adaptive_band());
        let current_adaptive_state: Option<&'static str> = self
            .output
            .audio_writer
            .as_ref()
            .and_then(|w| w.adaptive_runtime_state());

        let freeze_delay_sync = current_latency_control_ms
            .zip(current_latency_target_ms)
            .map(|(control_ms, target_ms)| control_ms + 40.0 < target_ms)
            .unwrap_or(false)
            || current_resample_ratio
                .map(|ratio| (ratio - 1.0).abs() >= 0.03)
                .unwrap_or(false)
            || matches!(current_adaptive_band, Some("hard"));
        if let Some(total_ms) = self.output.audio_writer.as_ref().and_then(|w| {
            w.total_audio_delay_ms()
                .or_else(|| w.measured_audio_delay_ms())
        }) {
            let should_write = !freeze_delay_sync
                && self
                    .output
                    .last_audio_delay_attempted_ms
                    .map(|prev| (total_ms - prev).abs() >= 20.0)
                    .unwrap_or(true);
            if should_write {
                let delay_s = -(total_ms / 1000.0);
                let delay_path = std::env::temp_dir().join("omniphony_delay");
                self.output.last_audio_delay_attempted_ms = Some(total_ms);
                if let Err(e) = std::fs::write(&delay_path, format!("{:.4}\n", delay_s)) {
                    let now = Instant::now();
                    let should_log = self
                        .output
                        .last_audio_delay_write_error_at
                        .map(|prev| now.saturating_duration_since(prev).as_secs_f32() >= 5.0)
                        .unwrap_or(true);
                    if should_log {
                        log::warn!("Could not write {}: {}", delay_path.display(), e);
                        self.output.last_audio_delay_write_error_at = Some(now);
                    }
                } else {
                    self.output.last_audio_delay_written_ms = Some(total_ms);
                    self.output.last_audio_delay_write_error_at = None;
                }
            } else if freeze_delay_sync {
                log::trace!(
                    "Freezing omniphony_delay update during audio recovery: control_ms={:?} target_ms={:?} ratio={:?} band={:?}",
                    current_latency_control_ms,
                    current_latency_target_ms,
                    current_resample_ratio,
                    current_adaptive_band
                );
            }
        }

        if self.output.audio_writer.is_some() {
            let mut pcm_f32_scratch = std::mem::take(&mut self.output.pcm_f32_buf);
            if let Some(ref mut renderer) = self.spatial_renderer {
                log::trace!(
                    "VBAP check: has_objects={}, metadata.len()={}, channel_count={}",
                    self.spatial.has_objects,
                    frame.metadata.len(),
                    channel_count
                );

                if !frame.metadata.is_empty() {
                    log::trace!(
                        "Processed {} metadata payload(s) via bridge",
                        frame.metadata.len()
                    );
                }

                if self.spatial.has_objects {
                    log::trace!(
                        "Using VBAP spatial rendering (metadata source: {})",
                        if frame.metadata.is_empty() {
                            "cached"
                        } else {
                            "current frame"
                        }
                    );

                    fill_pcm_f32_reuse(&mut pcm_f32_scratch, &frame.pcm);
                    let pcm_data_f32 = &pcm_f32_scratch;

                    if self
                        .telemetry
                        .osc_sender
                        .as_ref()
                        .is_some_and(|sender| sender.has_metering_clients())
                    {
                        if let Some(ref mut meter) = self.telemetry.audio_meter {
                            meter.update_channel_count(channel_count);
                            for chunk in pcm_data_f32.chunks_exact(channel_count) {
                                meter.process_objects(chunk, channel_count);
                            }
                        }
                    }

                    let pending_events = std::mem::take(&mut self.spatial.frame_events);
                    let donated_buf = std::mem::take(&mut self.output.render_buf);
                    let render_started_at = Instant::now();
                    let rendered = renderer.render_frame(
                        &pcm_data_f32,
                        channel_count,
                        &pending_events,
                        donated_buf,
                    )?;
                    let render_time_ms = render_started_at.elapsed().as_secs_f32() * 1000.0;

                    let num_speakers = renderer.num_speakers();

                    let meter_snapshot = if self
                        .telemetry
                        .osc_sender
                        .as_ref()
                        .is_some_and(|sender| sender.has_metering_clients())
                    {
                        self.telemetry.audio_meter.as_mut().and_then(|m| {
                            m.process_speakers(&rendered.samples, num_speakers);
                            m.poll()
                        })
                    } else {
                        None
                    };
                    let sent_meter_bundle = if let (Some(snapshot), Some(osc_sender)) =
                        (meter_snapshot, &self.telemetry.osc_sender)
                    {
                        if let Err(e) = osc_sender.send_meter_bundle(
                            &snapshot,
                            &rendered.object_gains,
                            &rendered.object_band_gains,
                            Some(decode_time_ms),
                            Some(render_time_ms),
                            None,
                            Some(frame_duration_ms),
                            current_latency_instant_ms,
                            current_latency_control_ms,
                            current_latency_target_ms,
                            current_resample_ratio,
                            current_adaptive_band,
                            current_adaptive_state,
                        ) {
                            log::warn!("Failed to send meter OSC bundle: {}", e);
                            false
                        } else {
                            true
                        }
                    } else {
                        false
                    };

                    log::trace!(
                        "Writing {} samples ({} sample_count × {} speakers) to streaming output",
                        rendered.samples.len(),
                        sample_count,
                        num_speakers
                    );

                    let samples_audio = AudioSamples::F32(rendered.samples);
                    let write_started_at = Instant::now();
                    self.output
                        .audio_writer
                        .as_mut()
                        .expect("audio_writer present")
                        .write_pcm_samples(&samples_audio, num_speakers)?;
                    let write_time_ms = write_started_at.elapsed().as_secs_f32() * 1000.0;
                    if sent_meter_bundle {
                        if let Some(osc_sender) = &self.telemetry.osc_sender {
                            if let Err(e) =
                                osc_sender.send_timing_update(None, None, Some(write_time_ms))
                            {
                                log::warn!("Failed to send write timing OSC update: {}", e);
                            }
                        }
                    }
                    self.output.render_buf = match samples_audio {
                        AudioSamples::F32(v) => v,
                        _ => unreachable!(),
                    };
                    self.output.pcm_f32_buf = pcm_f32_scratch;
                    return Ok(());
                } else {
                    let labels: Vec<RChannelLabel> = frame.channel_labels.iter().copied().collect();
                    let (room_ratio, room_ratio_rear, room_ratio_lower, room_ratio_center_blend) = {
                        let control = renderer.renderer_control();
                        let live = control.live.read().unwrap();
                        (
                            live.room_ratio,
                            live.room_ratio_rear,
                            live.room_ratio_lower,
                            live.room_ratio_center_blend,
                        )
                    };
                    let input_layout = self
                        .input_control
                        .and_then(|control| control.requested_snapshot().current_layout);
                    let virtual_events = match build_virtual_bed_events(
                        &labels,
                        input_layout.as_ref(),
                        room_ratio,
                        room_ratio_rear,
                        room_ratio_lower,
                        room_ratio_center_blend,
                    ) {
                        Some(v) => v,
                        None => {
                            log::warn!(
                                "No virtual bed VBAP map for channel labels {:?} - outputting silence",
                                labels
                            );
                            let num_speakers = renderer.num_speakers();
                            self.output
                                .audio_writer
                                .as_mut()
                                .expect("audio_writer present")
                                .write_pcm_samples(
                                    &AudioSamples::I32(vec![0i32; sample_count * num_speakers]),
                                    num_speakers,
                                )?;
                            self.output.pcm_f32_buf = pcm_f32_scratch;
                            return Ok(());
                        }
                    };

                    fill_pcm_f32_reuse(&mut pcm_f32_scratch, &frame.pcm);
                    let pcm_data_f32 = &pcm_f32_scratch;

                    if self
                        .telemetry
                        .osc_sender
                        .as_ref()
                        .is_some_and(|sender| sender.has_metering_clients())
                    {
                        if let Some(ref mut meter) = self.telemetry.audio_meter {
                            meter.update_channel_count(channel_count);
                            for chunk in pcm_data_f32.chunks_exact(channel_count) {
                                meter.process_objects(chunk, channel_count);
                            }
                        }
                    }

                    let donated_buf = std::mem::take(&mut self.output.render_buf);
                    let render_started_at = Instant::now();
                    let rendered = renderer.render_frame(
                        &pcm_data_f32,
                        channel_count,
                        &virtual_events,
                        donated_buf,
                    )?;
                    let render_time_ms = render_started_at.elapsed().as_secs_f32() * 1000.0;
                    let num_speakers = renderer.num_speakers();

                    let meter_snapshot = if self
                        .telemetry
                        .osc_sender
                        .as_ref()
                        .is_some_and(|sender| sender.has_metering_clients())
                    {
                        self.telemetry.audio_meter.as_mut().and_then(|m| {
                            m.process_speakers(&rendered.samples, num_speakers);
                            m.poll()
                        })
                    } else {
                        None
                    };
                    let sent_meter_bundle = if let (Some(snapshot), Some(osc_sender)) =
                        (meter_snapshot, &self.telemetry.osc_sender)
                    {
                        if let Err(e) = osc_sender.send_meter_bundle(
                            &snapshot,
                            &rendered.object_gains,
                            &rendered.object_band_gains,
                            Some(decode_time_ms),
                            Some(render_time_ms),
                            None,
                            Some(frame_duration_ms),
                            current_latency_instant_ms,
                            current_latency_control_ms,
                            current_latency_target_ms,
                            current_resample_ratio,
                            current_adaptive_band,
                            current_adaptive_state,
                        ) {
                            log::warn!("Failed to send meter OSC bundle: {}", e);
                            false
                        } else {
                            true
                        }
                    } else {
                        false
                    };

                    let samples_audio = AudioSamples::F32(rendered.samples);
                    let write_started_at = Instant::now();
                    self.output
                        .audio_writer
                        .as_mut()
                        .expect("audio_writer present")
                        .write_pcm_samples(&samples_audio, num_speakers)?;
                    let write_time_ms = write_started_at.elapsed().as_secs_f32() * 1000.0;
                    if sent_meter_bundle {
                        if let Some(osc_sender) = &self.telemetry.osc_sender {
                            if let Err(e) =
                                osc_sender.send_timing_update(None, None, Some(write_time_ms))
                            {
                                log::warn!("Failed to send write timing OSC update: {}", e);
                            }
                        }
                    }
                    self.output.render_buf = match samples_audio {
                        AudioSamples::F32(v) => v,
                        _ => unreachable!(),
                    };
                    self.output.pcm_f32_buf = pcm_f32_scratch;

                    if self
                        .telemetry
                        .osc_sender
                        .as_ref()
                        .is_some_and(|sender| sender.has_osc_clients())
                    {
                        if let (Some(ref mut osc_sender), Some(objects)) = (
                            self.telemetry.osc_sender.as_mut(),
                            build_virtual_bed_objects(
                                &labels,
                                input_layout.as_ref(),
                                room_ratio,
                                room_ratio_rear,
                                room_ratio_lower,
                                room_ratio_center_blend,
                            ),
                        ) {
                            let sample_pos = self
                                .session
                                .decoded_samples
                                .saturating_sub(sample_count as u64);
                            if let Err(e) = osc_sender.send_object_frame(sample_pos, 0, 0, &objects)
                            {
                                log::warn!("Failed to send OSC virtual bed frame: {}", e);
                            }
                        }
                    }
                    return Ok(());
                }
            } else {
                log::trace!("Skipping VBAP: spatial_renderer is None");
            }

            log::trace!(
                "Writing {} samples (NO VBAP: {} sample_count × {} channels) to streaming output",
                frame.pcm.len(),
                sample_count,
                channel_count
            );

            self.output
                .audio_writer
                .as_mut()
                .expect("audio_writer present")
                .write_pcm_samples(&AudioSamples::I32(frame.pcm.to_vec()), channel_count)?;
            self.output.pcm_f32_buf = pcm_f32_scratch;
        }
        Ok(())
    }

    pub fn write_audio_samples_bed_conform(
        &mut self,
        frame: &RDecodedFrame,
        _decode_time_ms: f32,
    ) -> Result<()> {
        let channel_count = frame.channel_count as usize;
        let sample_count = frame.sample_count as usize;

        if let Some(ref mut writer) = self.output.audio_writer {
            let empty_vec = Vec::new();
            let bed_indices = self.spatial.bed_indices.as_ref().unwrap_or(&empty_vec);
            let conformed_channel_count = ChannelCountCalculator::calculate_conformed_channel_count(
                channel_count,
                bed_indices,
            );

            let samples = BedChannelMapper::apply_bed_conformance_to_frame(
                &frame.pcm,
                sample_count,
                channel_count,
                bed_indices,
            );

            writer.write_pcm_samples(&AudioSamples::I32(samples), conformed_channel_count)?;
        }
        Ok(())
    }
}

#[inline]
fn fill_pcm_f32_reuse(out: &mut Vec<f32>, pcm: &[i32]) {
    const SCALE: f32 = 8_388_608.0;
    out.clear();
    out.reserve(pcm.len().saturating_sub(out.capacity()));
    for &s in pcm {
        out.push(s as f32 / SCALE);
    }
}
