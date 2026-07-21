use super::handler::{BedChannelMapper, ChannelCountCalculator};
use super::output::AudioSamples;
use super::state::{DecodeSessionState, OutputState, SpatialState, TelemetryState};
use anyhow::Result;
use audio_input::InputControl;
use bridge_api::RChannelLabel;
use bridge_api::RDecodedFrame;
use orender_engine::render::fill_pcm_f32_drc;
use orender_engine::virtual_bed::{
    ChannelRenderPlan, build_virtual_bed_objects, plan_channel_render,
};
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

        let latency_snapshot = self
            .output
            .audio_writer
            .as_ref()
            .and_then(|w| w.latency_snapshot());
        let current_latency_instant_ms = latency_snapshot.map(|snapshot| snapshot.final_latency_ms);
        let current_latency_control_ms =
            latency_snapshot.and_then(|snapshot| snapshot.control_latency_ms);
        let current_latency_smoothed_ms =
            latency_snapshot.and_then(|snapshot| snapshot.smoothed_control_latency_ms);
        let current_latency_target_ms =
            latency_snapshot.and_then(|snapshot| snapshot.target_control_latency_ms);
        let current_latency_downstream_ms =
            latency_snapshot.and_then(|snapshot| snapshot.downstream_latency_ms);
        let current_latency_avail_input_ms =
            latency_snapshot.and_then(|snapshot| snapshot.avail_input_latency_ms);
        let current_latency_output_fifo_ms =
            latency_snapshot.and_then(|snapshot| snapshot.output_fifo_latency_ms);
        let current_latency_resampler_pending_ms =
            latency_snapshot.and_then(|snapshot| snapshot.resampler_pending_latency_ms);
        // Diag publication runs on its own cadence and per-client enable
        // flag, independent of the audio meter bundle. Gated by
        // `has_diag_clients` so we skip the JSON serialisation entirely
        // when no Studio plot is open.
        let now = Instant::now();
        let want_diag = self
            .telemetry
            .osc_sender
            .as_ref()
            .map(|s| s.has_diag_clients())
            .unwrap_or(false)
            && self
                .telemetry
                .diag_cadence
                .as_mut()
                .map(|c| c.should_send(now))
                .unwrap_or(false);
        if want_diag {
            if let Some(ic) = self.input_control {
                let registry = ic.diag_registry();
                let schema_json = registry.schema_json();
                let values_json = registry.values_json();
                if let Some(osc_sender) = &self.telemetry.osc_sender {
                    if let Err(e) = osc_sender.send_diag_bundle(schema_json, values_json) {
                        log::warn!("Failed to send diag OSC bundle: {}", e);
                    }
                }
                if let Some(cadence) = self.telemetry.diag_cadence.as_mut() {
                    cadence.mark_sent(now);
                }
            }
        }
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

        // DIAG output: wire the backend's pre-allocated diag atomics into
        // the registry. register_external is idempotent: second call (and
        // later) is a no-op so the per-call cost is a single mutex check
        // per handle. Adding a new metric to the backend's
        // `diag_atomic_handles()` list surfaces it in the diag plot
        // automatically — no other code change required.
        if let (Some(ic), Some(writer)) = (self.input_control, self.output.audio_writer.as_ref()) {
            let diag = ic.diag_registry();
            for handle in writer.diag_atomic_handles() {
                diag.register_external(
                    handle.name,
                    handle.label,
                    handle.group,
                    handle.unit,
                    handle.atomic,
                );
            }
            // Target latency setpoint: published here (not from the audio
            // backend) because the target lives in the decode runtime. Same
            // value that feeds the latency-gauge target marker, so the diag
            // plot can show the controller's setpoint alongside the measured
            // latency traces.
            if let Some(target_ms) = current_latency_target_ms {
                diag.register("latency_target_ms", "Target latency", "latency", "ms")
                    .store(
                        (target_ms as f64).to_bits(),
                        std::sync::atomic::Ordering::Relaxed,
                    );
            }
            // Tier classification (renderer-declared): the obvious,
            // clearly-meaningful signals go in the diag plot's "base" tab;
            // everything else stays "advanced" (registration default).
            for base_metric in [
                "latency_smoothed_ms",
                "latency_control_ms",
                "latency_target_ms",
                "rate_adjust_ppm",
            ] {
                diag.set_tier(base_metric, "base");
            }
        }

        let freeze_delay_sync = current_latency_control_ms
            .zip(current_latency_target_ms)
            .map(|(control_ms, target_ms)| control_ms + 40.0 < target_ms)
            .unwrap_or(false)
            || current_resample_ratio
                .map(|ratio| (ratio - 1.0).abs() >= 0.03)
                .unwrap_or(false)
            || matches!(current_adaptive_band, Some("hard"));
        if let Some(total_ms) = self.output.audio_writer.as_ref().and_then(|w| {
            w.measured_audio_delay_ms()
                .or_else(|| w.target_audio_delay_ms())
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

                let drc_weight = renderer
                    .renderer_control()
                    .live
                    .read()
                    .drc_weight
                    .clamp(0.0, 1.0);
                self.output.drc_target_gain = if drc_weight >= 1.0 {
                    frame.drc_gain
                } else if drc_weight <= 0.0 {
                    1.0
                } else {
                    frame.drc_gain.powf(drc_weight)
                };
                self.output.drc_ramp_samples_remaining = frame.drc_ramp_duration;

                if self.spatial.has_objects {
                    log::trace!(
                        "Using VBAP spatial rendering (metadata source: {})",
                        if frame.metadata.is_empty() {
                            "cached"
                        } else {
                            "current frame"
                        }
                    );

                    fill_pcm_f32_drc(
                        &mut pcm_f32_scratch,
                        &frame.pcm,
                        channel_count,
                        &mut self.output.drc_gain,
                        self.output.drc_target_gain,
                        &mut self.output.drc_ramp_samples_remaining,
                    );
                    let pcm_data_f32 = &pcm_f32_scratch;

                    let has_metering_clients = self
                        .telemetry
                        .osc_sender
                        .as_ref()
                        .is_some_and(|sender| sender.has_metering_clients());
                    if has_metering_clients {
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
                        has_metering_clients,
                    )?;

                    let num_speakers = renderer.num_speakers();

                    let meter_snapshot = if has_metering_clients {
                        self.telemetry.audio_meter.as_mut().and_then(|m| {
                            m.process_speakers(&rendered.samples, num_speakers);
                            m.poll()
                        })
                    } else {
                        None
                    };
                    let render_time_ms = render_started_at.elapsed().as_secs_f32() * 1000.0;
                    let sent_meter_bundle = if let (Some(snapshot), Some(osc_sender)) =
                        (meter_snapshot, &self.telemetry.osc_sender)
                    {
                        if let Err(e) = osc_sender.send_meter_bundle(
                            &snapshot,
                            &rendered.object_gains,
                            &rendered.object_band_gains,
                            Some(decode_time_ms),
                            Some(rendered.crossover_time_ms),
                            Some(render_time_ms),
                            None,
                            Some(frame_duration_ms),
                            current_latency_instant_ms,
                            current_latency_control_ms,
                            current_latency_smoothed_ms,
                            current_latency_target_ms,
                            current_latency_downstream_ms,
                            current_latency_avail_input_ms,
                            current_latency_output_fifo_ms,
                            current_latency_resampler_pending_ms,
                            current_resample_ratio,
                            current_adaptive_band,
                            current_adaptive_state,
                            Some(self.output.drc_gain),
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
                    let (
                        mode,
                        virtual_bed_layout,
                        surround_placement,
                        room_ratio,
                        room_ratio_rear,
                        room_ratio_lower,
                        room_ratio_center_blend,
                    ) = {
                        let control = renderer.renderer_control();
                        let live = control.live.read();
                        (
                            live.channel_render_mode,
                            live.virtual_bed.clone(),
                            live.surround_placement,
                            live.room_ratio,
                            live.room_ratio_rear,
                            live.room_ratio_lower,
                            live.room_ratio_center_blend,
                        )
                    };
                    let output_layout = renderer.speaker_layout();
                    let virtual_events = match plan_channel_render(
                        mode,
                        &labels,
                        virtual_bed_layout.as_ref(),
                        Some(&output_layout),
                        room_ratio,
                        room_ratio_rear,
                        room_ratio_lower,
                        room_ratio_center_blend,
                        surround_placement,
                    ) {
                        ChannelRenderPlan::Events { events, routes } => {
                            // Spatial mode mixes per channel: direct channels
                            // route one-hot by label, virtual channels render
                            // as VBAP objects. Reconfigure only on change.
                            self.spatial.fixed_planner.apply_routes(renderer, routes);
                            events
                        }
                        ChannelRenderPlan::HostPassthrough => {
                            // No spatialization: write the decoded channels
                            // straight to the sink (let the host/sink handle
                            // them), mirroring mpv falling back to ad_lavc.
                            self.output
                                .audio_writer
                                .as_mut()
                                .expect("audio_writer present")
                                .write_pcm_samples(
                                    &AudioSamples::I32(frame.pcm.to_vec()),
                                    channel_count,
                                )?;
                            self.output.pcm_f32_buf = pcm_f32_scratch;
                            return Ok(());
                        }
                        ChannelRenderPlan::Silence => {
                            log::warn!(
                                "No channel render mapping for labels {:?} - outputting silence",
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

                    fill_pcm_f32_drc(
                        &mut pcm_f32_scratch,
                        &frame.pcm,
                        channel_count,
                        &mut self.output.drc_gain,
                        self.output.drc_target_gain,
                        &mut self.output.drc_ramp_samples_remaining,
                    );
                    let pcm_data_f32 = &pcm_f32_scratch;

                    let has_metering_clients = self
                        .telemetry
                        .osc_sender
                        .as_ref()
                        .is_some_and(|sender| sender.has_metering_clients());
                    if has_metering_clients {
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
                        has_metering_clients,
                    )?;
                    let num_speakers = renderer.num_speakers();

                    let meter_snapshot = if has_metering_clients {
                        self.telemetry.audio_meter.as_mut().and_then(|m| {
                            m.process_speakers(&rendered.samples, num_speakers);
                            m.poll()
                        })
                    } else {
                        None
                    };
                    let render_time_ms = render_started_at.elapsed().as_secs_f32() * 1000.0;
                    let sent_meter_bundle = if let (Some(snapshot), Some(osc_sender)) =
                        (meter_snapshot, &self.telemetry.osc_sender)
                    {
                        if let Err(e) = osc_sender.send_meter_bundle(
                            &snapshot,
                            &rendered.object_gains,
                            &rendered.object_band_gains,
                            Some(decode_time_ms),
                            Some(rendered.crossover_time_ms),
                            Some(render_time_ms),
                            None,
                            Some(frame_duration_ms),
                            current_latency_instant_ms,
                            current_latency_control_ms,
                            current_latency_smoothed_ms,
                            current_latency_target_ms,
                            current_latency_downstream_ms,
                            current_latency_avail_input_ms,
                            current_latency_output_fifo_ms,
                            current_latency_resampler_pending_ms,
                            current_resample_ratio,
                            current_adaptive_band,
                            current_adaptive_state,
                            Some(self.output.drc_gain),
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
                                virtual_bed_layout.as_ref(),
                                Some(&renderer.speaker_layout()),
                                room_ratio,
                                room_ratio_rear,
                                room_ratio_lower,
                                room_ratio_center_blend,
                                surround_placement,
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
