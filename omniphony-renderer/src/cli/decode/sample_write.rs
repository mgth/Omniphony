use super::decoder_thread::DecodedSource;
use super::handler::{BedChannelMapper, ChannelCountCalculator};
use super::output::AudioLatencySnapshot;
use super::output::AudioSamples;
use super::state::{DecodeSessionState, OutputState, SpatialState, TelemetryState};
use anyhow::Result;
use audio_input::InputControl;
use bridge_api::RChannelLabel;
use bridge_api::RDecodedFrame;
use orender_engine::render_metering::{meter_render_input, meter_render_output};
use orender_engine::virtual_bed::BedPlanKind;
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
        let frame_duration_ms =
            sample_count as f32 / frame.sampling_frequency.max(1) as f32 * 1000.0;

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
        let output_figures = OutputFigures {
            latency: latency_snapshot,
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

        if self.output.audio_writer.is_some() {
            let mut pcm_f32_scratch = std::mem::take(&mut self.output.pcm_f32_buf);
            if let Some(ref mut renderer) = self.spatial_renderer {
                log::trace!(
                    "VBAP check: has_objects={}, metadata.len()={}, channel_count={}",
                    self.spatial.stream.has_objects,
                    frame.metadata.len(),
                    channel_count
                );

                if !frame.metadata.is_empty() {
                    log::trace!(
                        "Processed {} metadata payload(s) via bridge",
                        frame.metadata.len()
                    );
                }

                if frame_has_objects {
                    log::trace!(
                        "Using VBAP spatial rendering (metadata source: {})",
                        if frame.metadata.is_empty() {
                            "cached"
                        } else {
                            "current frame"
                        }
                    );
                    let control = renderer.renderer_control();
                    let stream = &mut self.spatial.stream;
                    stream.publish_object_stream_processing(&control, &frame.channel_labels);
                    stream.fill_pcm_f32(&mut pcm_f32_scratch, frame, &control);
                    let pcm_data_f32 = &pcm_f32_scratch;

                    let has_metering_clients = self
                        .telemetry
                        .osc_sender
                        .as_ref()
                        .is_some_and(|sender| sender.has_metering_clients());
                    if has_metering_clients {
                        if let Some(ref mut meter) = self.telemetry.audio_meter {
                            meter_render_input(meter, pcm_data_f32, channel_count);
                        }
                    }

                    let donated_buf = std::mem::take(&mut self.output.render_buf);
                    let render_started_at = Instant::now();
                    let rendered = renderer.render_frame(
                        pcm_data_f32,
                        channel_count,
                        &stream.frame_events,
                        donated_buf,
                        has_metering_clients,
                    )?;
                    let render_time_ms = render_started_at.elapsed().as_secs_f32() * 1000.0;
                    // Emptied, so the next frame's events land in the same
                    // allocation.
                    stream.frame_events.clear();
                    let drc_gain = stream.drc.gain;

                    emit_rendered(
                        self.output,
                        self.telemetry,
                        self.input_control,
                        renderer,
                        rendered,
                        FrameTimings {
                            decode_ms: decode_time_ms,
                            render_ms: render_time_ms,
                            frame_ms: frame_duration_ms,
                            sample_rate: frame.sampling_frequency,
                            drc_gain,
                        },
                        has_metering_clients,
                        &output_figures,
                    )?;
                    self.output.pcm_f32_buf = pcm_f32_scratch;
                    return Ok(());
                } else {
                    let labels: &[RChannelLabel] = &frame.channel_labels;
                    // The plan depends only on the labels and a few live
                    // params, so the planner reuses it until one of them
                    // actually changes, instead of rebuilding a label→speaker
                    // map and re-solving the depth warp on every frame.
                    match self.spatial.stream.plan_bed(renderer, labels) {
                        BedPlanKind::Events => {}
                        BedPlanKind::HostPassthrough => {
                            // No spatialization: write the decoded channels
                            // straight to the sink (let the host/sink handle
                            // them), mirroring mpv falling back to ad_lavc. The
                            // sink is sized for them (`output_shape`).
                            write_decoded_pcm(self.output, &frame.pcm, channel_count)?;
                            self.output.pcm_f32_buf = pcm_f32_scratch;
                            return Ok(());
                        }
                        BedPlanKind::Silence => {
                            log::warn!(
                                "No channel render mapping for labels {:?} - outputting silence",
                                labels
                            );
                            // As wide as the sink: what the renderer would have
                            // emitted (the binaural pair in headphone mode, not
                            // the speaker count).
                            let width = renderer.output_channel_count();
                            let mut silence = std::mem::take(&mut self.output.render_buf);
                            silence.clear();
                            silence.resize(sample_count * width, 0.0);
                            let samples_audio = AudioSamples::F32(silence);
                            self.output
                                .audio_writer
                                .as_mut()
                                .expect("audio_writer present")
                                .write_pcm_samples(&samples_audio, width)?;
                            self.output.render_buf = match samples_audio {
                                AudioSamples::F32(v) => v,
                                _ => unreachable!(),
                            };
                            self.output.pcm_f32_buf = pcm_f32_scratch;
                            return Ok(());
                        }
                    };

                    // Synthesize objects from the bed, as the embedded engine
                    // does: plan first so each object gets its channel event,
                    // then extend the PCM below with its audio. The layout is
                    // borrowed from the live topology rather than
                    // `speaker_layout()`, which hands back a deep copy of it.
                    let control = renderer.renderer_control();
                    let topology = control.active_topology();
                    let output_layout = &topology.speaker_layout;
                    let stream = &mut self.spatial.stream;
                    let stage_counts = stream.sync_channel_objects(
                        &control,
                        labels,
                        channel_count,
                        output_layout,
                        frame.sampling_frequency,
                    );

                    stream.fill_pcm_f32(&mut pcm_f32_scratch, frame, &control);
                    let (pcm_data_f32, render_channel_count) =
                        stream.channel_objects.process_and_extend(
                            &mut pcm_f32_scratch,
                            channel_count,
                            sample_count,
                            frame.sampling_frequency,
                            stage_counts,
                        );

                    let has_metering_clients = self
                        .telemetry
                        .osc_sender
                        .as_ref()
                        .is_some_and(|sender| sender.has_metering_clients());
                    // Meter and render the extended width: the synthesized
                    // object channels sit past the bed, and striding by the bed
                    // width alone would walk through them wrongly.
                    if has_metering_clients {
                        if let Some(ref mut meter) = self.telemetry.audio_meter {
                            meter_render_input(meter, pcm_data_f32, render_channel_count);
                        }
                    }

                    let donated_buf = std::mem::take(&mut self.output.render_buf);
                    let render_started_at = Instant::now();
                    let rendered = renderer.render_frame(
                        pcm_data_f32,
                        render_channel_count,
                        &stream.bed_events,
                        donated_buf,
                        has_metering_clients,
                    )?;
                    let render_time_ms = render_started_at.elapsed().as_secs_f32() * 1000.0;
                    let drc_gain = stream.drc.gain;

                    emit_rendered(
                        self.output,
                        self.telemetry,
                        self.input_control,
                        renderer,
                        rendered,
                        FrameTimings {
                            decode_ms: decode_time_ms,
                            render_ms: render_time_ms,
                            frame_ms: frame_duration_ms,
                            sample_rate: frame.sampling_frequency,
                            drc_gain,
                        },
                        has_metering_clients,
                        &output_figures,
                    )?;
                    self.output.pcm_f32_buf = pcm_f32_scratch;

                    // The virtual bed and the synthesized objects: emitted
                    // here they appear in Studio's 3D view, omitted they are
                    // rendered but never shown.
                    if let Some(osc_sender) = self
                        .telemetry
                        .osc_sender
                        .as_mut()
                        .filter(|sender| sender.has_osc_clients())
                    {
                        let objects =
                            self.spatial
                                .stream
                                .bed_frame_metas(&control, labels, output_layout);
                        if !objects.is_empty() {
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

            write_decoded_pcm(self.output, &frame.pcm, channel_count)?;
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

/// This frame's output-stage figures, read once before the render, for the
/// meter bundle (the embedded host has no output stage: it sends none).
struct OutputFigures {
    latency: Option<AudioLatencySnapshot>,
    resample_ratio: Option<f32>,
    adaptive_band: Option<&'static str>,
    adaptive_state: Option<&'static str>,
}

/// Timings of the frame being written, for the meter bundle.
struct FrameTimings {
    decode_ms: f32,
    /// The render call alone (metering excluded), smoothed below.
    render_ms: f32,
    frame_ms: f32,
    sample_rate: u32,
    /// The DRC gain the frame ended on.
    drc_gain: f32,
}

/// Everything after a render, for both render paths (objects, channel
/// content): the sink latency feed, the output metering and meter bundle, and
/// the write. The metering itself is the embedded engine's
/// ([`meter_render_output`]); the reported render time is smoothed the same
/// way ([`renderer::metering::DutyEma`]) — raw per-frame figures alias with
/// the 40-sample TrueHD access units.
#[allow(clippy::too_many_arguments)]
fn emit_rendered(
    output: &mut OutputState,
    telemetry: &mut TelemetryState,
    input_control: Option<&InputControl>,
    renderer: &mut renderer::spatial_renderer::SpatialRenderer,
    rendered: renderer::spatial_renderer::RenderedFrame,
    timings: FrameTimings,
    has_metering_clients: bool,
    figures: &OutputFigures,
) -> Result<()> {
    let latency = figures.latency;
    // Feed the PipeWire bridge sink's advertised latency: render DSP latency
    // (constant, e.g. the linear-phase FIR crossover) plus the measured
    // output-chain latency (ring + pacer FIFO + graph delay to the DAC). The
    // client-node backend republishes the sink's Latency/ProcessLatency params
    // when this moves, so upstream players stay in A/V sync.
    if let Some(ic) = input_control {
        let rate = timings.sample_rate.max(1) as u64;
        let dsp_ns = renderer.output_latency_samples() as u64 * 1_000_000_000 / rate;
        let out_ns = latency
            .map(|l| l.final_latency_ms)
            .map_or(0, |ms| (ms.max(0.0) as f64 * 1e6) as u64);
        ic.set_downstream_latency_ns(dsp_ns + out_ns);
    }

    let render_time_ms = output
        .render_duty
        .update(timings.render_ms, timings.frame_ms);
    let meter_snapshot = if has_metering_clients {
        telemetry
            .audio_meter
            .as_mut()
            .and_then(|meter| meter_render_output(meter, renderer, &rendered))
    } else {
        None
    };
    let sent_meter_bundle =
        if let (Some(snapshot), Some(osc_sender)) = (meter_snapshot, &telemetry.osc_sender) {
            if let Err(e) = osc_sender.send_meter_bundle(
                &snapshot,
                &rendered.object_gains,
                &rendered.object_band_gains,
                rendered.object_test_position,
                rendered.object_test_level,
                Some(timings.decode_ms),
                Some(rendered.crossover_time_ms),
                Some(render_time_ms),
                None,
                Some(timings.frame_ms),
                latency.map(|l| l.final_latency_ms),
                latency.and_then(|l| l.control_latency_ms),
                latency.and_then(|l| l.smoothed_control_latency_ms),
                latency.and_then(|l| l.target_control_latency_ms),
                latency.and_then(|l| l.downstream_latency_ms),
                latency.and_then(|l| l.avail_input_latency_ms),
                latency.and_then(|l| l.output_fifo_latency_ms),
                latency.and_then(|l| l.resampler_pending_latency_ms),
                figures.resample_ratio,
                figures.adaptive_band,
                figures.adaptive_state,
                Some(timings.drc_gain),
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
        "Writing {} samples ({} channels) to streaming output",
        rendered.samples.len(),
        rendered.n_channels
    );
    // The width of what was rendered (the sink was sized from the same
    // `output_channel_count` before the render).
    let rendered_channels = rendered.n_channels;
    // The metering lists go back to the renderer, which refills them on the
    // next metered frame (Studio connected meters every frame): no per-frame
    // allocation on the render path.
    let samples_audio = AudioSamples::F32(renderer.recycle_frame(rendered));
    let write_started_at = Instant::now();
    output
        .audio_writer
        .as_mut()
        .expect("audio_writer present")
        .write_pcm_samples(&samples_audio, rendered_channels)?;
    let write_time_ms = write_started_at.elapsed().as_secs_f32() * 1000.0;
    if sent_meter_bundle {
        if let Some(osc_sender) = &telemetry.osc_sender {
            if let Err(e) = osc_sender.send_timing_update(None, None, Some(write_time_ms)) {
                log::warn!("Failed to send write timing OSC update: {}", e);
            }
        }
    }
    output.render_buf = match samples_audio {
        AudioSamples::F32(v) => v,
        _ => unreachable!(),
    };
    Ok(())
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
