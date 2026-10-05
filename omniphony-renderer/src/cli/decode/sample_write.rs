use super::decoder_thread::DecodedSource;
use super::handler::{BedChannelMapper, ChannelCountCalculator};
use super::output::AudioSamples;
use super::state::{DecodeSessionState, OutputState, SpatialState, TelemetryState};
use anyhow::Result;
use audio_input::InputControl;
use bridge_api::RDecodedFrame;
use orender_engine::frame_pipeline::{FrameOutput, OutputStageFigures};
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
    /// Whether this frame carries objects (see [`SpatialState::frame_has_objects`]).
    pub fn frame_has_objects(&self, source: DecodedSource) -> bool {
        self.spatial.frame_has_objects(source)
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
        source: DecodedSource,
    ) -> Result<()> {
        // Resolved before the renderer is borrowed for the render itself, and
        // kept for the whole frame so the branch cannot change under us.
        let frame_has_objects = self.frame_has_objects(source);
        let channel_count = frame.channel_count as usize;
        let sample_count = frame.sample_count as usize;

        let latency_snapshot = self
            .output
            .audio_writer
            .as_ref()
            .and_then(|w| w.latency_snapshot());
        let current_latency_control_ms =
            latency_snapshot.and_then(|snapshot| snapshot.control_latency_ms);
        let current_latency_target_ms =
            latency_snapshot.and_then(|snapshot| snapshot.target_control_latency_ms);
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
        let output_figures = OutputStageFigures {
            latency_instant_ms: latency_snapshot.map(|l| l.final_latency_ms),
            latency_control_ms: current_latency_control_ms,
            latency_smoothed_ms: latency_snapshot.and_then(|l| l.smoothed_control_latency_ms),
            latency_target_ms: current_latency_target_ms,
            latency_downstream_ms: latency_snapshot.and_then(|l| l.downstream_latency_ms),
            latency_avail_input_ms: latency_snapshot.and_then(|l| l.avail_input_latency_ms),
            latency_output_fifo_ms: latency_snapshot.and_then(|l| l.output_fifo_latency_ms),
            latency_resampler_pending_ms: latency_snapshot
                .and_then(|l| l.resampler_pending_latency_ms),
            resample_ratio: current_resample_ratio,
            adaptive_band: current_adaptive_band,
            adaptive_state: current_adaptive_state,
        };

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
                .unwrap_or(false);
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

        if self.output.audio_writer.is_none() {
            return Ok(());
        }
        let Some(renderer) = self.spatial_renderer.as_deref_mut() else {
            // No renderer: the decoded channels go out as they are.
            return write_decoded_pcm(self.output, &frame.pcm, channel_count);
        };
        // The block this frame starts (the session clock has already moved
        // past it).
        let sample_pos = self
            .session
            .decoded_samples
            .saturating_sub(sample_count as u64);
        let donated = std::mem::take(&mut self.output.render_buf);
        let render = self.spatial.pipeline.render(
            frame,
            sample_pos,
            frame_has_objects,
            renderer,
            self.telemetry.osc_sender.as_mut(),
            &mut self.telemetry.audio_meter,
            donated,
            decode_time_ms,
            &output_figures,
        )?;
        match render.output {
            FrameOutput::Passthrough { unused } => {
                // The host channel render mode: no spatialization, the decoded
                // channels go straight to the sink (let the host/sink handle
                // them), mirroring mpv falling back to ad_lavc. The sink is
                // sized for them (`output_shape`).
                self.output.render_buf = unused;
                write_decoded_pcm(self.output, &frame.pcm, channel_count)
            }
            FrameOutput::Silence { samples, channels } => {
                write_rendered(self.output, samples, channels).map(|_| ())
            }
            FrameOutput::Rendered { samples, channels } => {
                // Feed the PipeWire bridge sink's advertised latency: render
                // DSP latency (constant, e.g. the linear-phase FIR crossover)
                // plus the measured output-chain latency (ring + pacer FIFO +
                // graph delay to the DAC). The client-node backend republishes
                // the sink's Latency/ProcessLatency params when this moves, so
                // upstream players stay in A/V sync.
                if let Some(ic) = self.input_control {
                    let rate = frame.sampling_frequency.max(1) as u64;
                    let dsp_ns = renderer.output_latency_samples() as u64 * 1_000_000_000 / rate;
                    let out_ns = latency_snapshot
                        .map(|l| l.final_latency_ms)
                        .map_or(0, |ms| (ms.max(0.0) as f64 * 1e6) as u64);
                    ic.set_downstream_latency_ns(dsp_ns + out_ns);
                }
                let write_time_ms = write_rendered(self.output, samples, channels)?;
                if render.meter_bundle_sent
                    && let Some(osc_sender) = &self.telemetry.osc_sender
                    && let Err(e) = osc_sender.send_timing_update(None, None, Some(write_time_ms))
                {
                    log::warn!("Failed to send write timing OSC update: {}", e);
                }
                Ok(())
            }
        }
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

/// Write rendered (or silent) `f32` audio, `channels` wide, to the sink;
/// the buffer goes back to `output.render_buf` for the next frame. Returns
/// how long the write took, in ms.
fn write_rendered(output: &mut OutputState, samples: Vec<f32>, channels: usize) -> Result<f32> {
    log::trace!(
        "Writing {} samples ({} channels) to streaming output",
        samples.len(),
        channels
    );
    let samples_audio = AudioSamples::F32(samples);
    let write_started_at = Instant::now();
    let result = output
        .audio_writer
        .as_mut()
        .expect("audio_writer present")
        .write_pcm_samples(&samples_audio, channels);
    let write_time_ms = write_started_at.elapsed().as_secs_f32() * 1000.0;
    output.render_buf = match samples_audio {
        AudioSamples::F32(v) => v,
        _ => unreachable!(),
    };
    result.map(|()| write_time_ms)
}

/// Write decoded PCM to the sink as it is (host passthrough, no renderer),
/// through a buffer kept across frames rather than a fresh copy per frame.
fn write_decoded_pcm(output: &mut OutputState, pcm: &[i32], channels: usize) -> Result<()> {
    let mut buf = std::mem::take(&mut output.pcm_i32_buf);
    buf.clear();
    buf.extend_from_slice(pcm);
    let samples = AudioSamples::I32(buf);
    let result = output
        .audio_writer
        .as_mut()
        .expect("audio_writer present")
        .write_pcm_samples(&samples, channels);
    output.pcm_i32_buf = match samples {
        AudioSamples::I32(v) => v,
        _ => unreachable!(),
    };
    result
}
