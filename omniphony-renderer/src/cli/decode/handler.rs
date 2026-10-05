use super::decoder_thread::DecodedSource;
use super::output_runtime_sync::OutputRuntimeCoordinator;
use super::sample_write::SampleWriteCoordinator;
use super::spatial_metadata::SpatialMetadataCoordinator;
use super::state::{
    DecodeSessionState, FrameHandlerContext, OutputSource, OutputState, RuntimeOutputState,
    SpatialState, TelemetryState,
};
use super::writer_lifecycle::WriterLifecycleCoordinator;
use crate::cli::command::OutputBackend;
use audio_input::{InputClockMode, InputControl, InputMode};
use audio_output::AudioControl;
use bridge_api::RDecodedFrame;
use renderer::live_params::ChannelRenderMode;

use anyhow::Result;
use orender_engine::decode_step::DrcModeSync;
use std::sync::{Arc, RwLock};
use std::time::Instant;

/// Where the handler sends the DRC mode picked in the live params: the value
/// the pipe decoder thread and the PipeWire sink's bridge decoder both read
/// before decoding (seeded with the configured mode before they start).
/// Outlives a stream reset, like the decoders.
#[derive(Default)]
pub struct DrcForwarding {
    pub shared: Option<Arc<RwLock<String>>>,
    pub sync: DrcModeSync,
}

impl DrcForwarding {
    /// Forward `requested` to the decoders when it changed.
    fn forward(&mut self, requested: &str) {
        if !self.sync.update(requested) {
            return;
        }
        let mode = self.sync.mode();
        if let Some(shared) = self.shared.as_ref() {
            let mut shared = shared.write().unwrap_or_else(|e| e.into_inner());
            shared.clear();
            shared.push_str(mode);
        }
    }
}

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
    pub input_control: Option<Arc<InputControl>>,
    pub drc: DrcForwarding,
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
            input_control: None,
            drc: DrcForwarding::default(),
        }
    }
}

impl DecodeHandler {
    fn reset_direct_trigger_wiring(&mut self) {
        if !self.session.direct_trigger_wired {
            return;
        }
        if let Some(input_control) = self.input_control.as_ref() {
            input_control.set_direct_trigger_active(false);
            input_control.clear_pending_input_triggers();
        }
        self.session.direct_trigger_wired = false;
        log::info!("Direct trigger wiring reset after output writer restart");
    }

    fn wants_output_driven_bridge_clock(&self) -> bool {
        self.input_control
            .as_ref()
            .map(|control| {
                let requested = control.requested_snapshot();
                requested.mode == InputMode::Pipewire && requested.clock_mode == InputClockMode::Dac
            })
            .unwrap_or(false)
    }

    /// Record the decoded stream in the applied input state, in pure pipe mode.
    ///
    /// In PipeWire mode the applied state is the capture's — the sink's node
    /// and carrier, as the live-input manager published them — and a decoded
    /// frame changes none of it, whichever producer it came from (the sink's
    /// own bridge decoder, the input pipe, the speaker-test idle feed).
    /// Recording the frame there turned the applied mode into `Bridge` with
    /// the capture still up, and [`should_accept_source`](Self::should_accept_source)
    /// then refused the sink's linear PCM until the next input apply.
    fn sync_input_runtime_state(
        &mut self,
        source: DecodedSource,
        frame: &RDecodedFrame,
    ) -> Result<()> {
        let Some(input_control) = self.input_control.as_ref() else {
            return Ok(());
        };
        let applied_before = input_control.applied_snapshot();
        if matches!(source, DecodedSource::Bridge)
            && applied_before.active_mode == InputMode::Bridge
        {
            let channels = Some(frame.channel_count as u16);
            let sample_rate_hz = Some(frame.sampling_frequency);
            let stream_format = Some("bridge-decoded".to_string());
            let changed = applied_before.channels != channels
                || applied_before.sample_rate_hz != sample_rate_hz
                || applied_before.stream_format != stream_format
                || applied_before.backend.is_some()
                || applied_before.node_name.is_some();
            if changed {
                input_control.set_input_state(
                    InputMode::Bridge,
                    None,
                    channels,
                    sample_rate_hz,
                    None,
                    None,
                    stream_format,
                );
            }
        }

        self.poll_runtime_state()
    }

    pub fn should_accept_source(&self, source: DecodedSource) -> bool {
        let active_mode = self
            .input_control
            .as_ref()
            .map(|control| control.applied_snapshot().active_mode)
            .unwrap_or(InputMode::Bridge);
        matches!(
            (active_mode, source),
            (InputMode::Bridge, DecodedSource::Bridge)
                // The PipeWire sink advertises PCM alongside IEC 61937, so this
                // mode legitimately produces either source depending on what the
                // client negotiated.
                | (InputMode::Pipewire, DecodedSource::Bridge | DecodedSource::Live)
        )
    }

    pub fn poll_runtime_state(&mut self) -> Result<()> {
        // Live output changes (device, backend, rate, latency) were applied
        // from `handle_decoded_frame` alone, so they only landed when the
        // decoder happened to be producing. Idle — between two programmes, or
        // in front of a test signal — the request sat in `AudioControl` until
        // something else made frames flow again, which is why switching the
        // output device looked like it needed an unrelated apply on the *input*
        // to take effect. Apply it on the idle tick too. Only the request is
        // consumed here: an invalidated writer is rebuilt by the next frame,
        // real or fabricated by the speaker-test idle feed.
        OutputRuntimeCoordinator::new(
            &mut self.output,
            &mut self.runtime,
            self.audio_control.as_deref(),
            self.input_control.as_deref(),
        )
        .sync_all()?;
        if self.output.audio_writer.is_none() {
            self.reset_direct_trigger_wiring();
        }

        if let Some(renderer) = self.spatial_renderer.as_ref() {
            let control = renderer.renderer_control();
            self.drc.forward(&control.live.read().options.drc_mode);
        }

        let Some(input_control) = self.input_control.as_ref() else {
            return Ok(());
        };
        let generation = input_control.state_generation();
        let last = self.session.last_input_state_generation;
        if last == Some(generation) {
            return Ok(());
        }
        self.session.last_input_state_generation = Some(generation);
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
            osc_sender.send_live_state_bundle()?;
        }
        Ok(())
    }

    pub fn handle_decoded_frame(
        &mut self,
        source: DecodedSource,
        frame: RDecodedFrame,
        ctx: &FrameHandlerContext,
    ) -> Result<()> {
        let now = Instant::now();
        if self.session.started_at.is_none() {
            self.session.started_at = Some(now);
        }
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
            let pathological_gap_ms = (frame_duration_ms * 200.0).max(500.0);
            let suspicious_metadata_change = metadata_count > 0 && sample_count_changed;
            if wall_gap_ms > pathological_gap_ms || suspicious_metadata_change {
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
            }
        }
        // DIAG decoder→ring: publish per-frame cadence metrics so the diag
        // plot can see whether the 1 Hz oscillation on avail_input is
        // introduced between the bridge worker thread and the main decoder
        // thread (mpsc queueing) or further downstream.
        if let Some(ic) = self.input_control.as_ref() {
            let diag = ic.diag_registry();
            let dt_us = self
                .session
                .last_frame_received_at
                .map(|prev| now.saturating_duration_since(prev).as_micros() as u64)
                .unwrap_or(0);
            let h_dt = diag.register("decoder_frame_dt_us", "Decoder frame dt", "decoder", "us");
            h_dt.store(
                (dt_us as f64).to_bits(),
                std::sync::atomic::Ordering::Relaxed,
            );
            let h_lag = diag.register("decoder_queue_lag_us", "MPSC queue lag", "decoder", "us");
            // queue_delay_ms was already computed by handle_audio_message as
            // `decoded.sent_at.elapsed()` — convert to us for consistency.
            let queue_lag_us = (ctx.queue_delay_ms * 1000.0).max(0.0) as u64;
            h_lag.store(
                (queue_lag_us as f64).to_bits(),
                std::sync::atomic::Ordering::Relaxed,
            );
        }
        self.session.last_frame_received_at = Some(now);
        self.session.last_frame_sample_count = Some(sample_count_u32);

        self.session.decoded_frames += 1u64;
        self.session.final_sample_rate = sample_rate;
        self.spatial.au_index += 1;

        self.sync_input_runtime_state(source, &frame)?;
        if let Some(renderer) = self.spatial_renderer.as_mut() {
            orender_engine::render::follow_stream_rate(renderer, sample_rate)?;
        }

        // Apply dialogue normalisation from bridge (updated on major sync frames).
        // The level is always stored so OSC clients receive loudness/source
        // and loudness/gain regardless of whether --use-loudness is set.
        if let Some(renderer) = self.spatial_renderer.as_ref() {
            if self.spatial.stream.latch_dialnorm(&frame, renderer) {
                if let Some(osc_sender) = self
                    .telemetry
                    .osc_sender
                    .as_ref()
                    .filter(|sender| sender.has_osc_clients())
                {
                    osc_sender.send_loudness_state();
                }
            }
        }

        // Everything sent about this frame describes the block starting here.
        let block_start = self.session.decoded_samples;
        if let Some(osc_sender) = self.telemetry.osc_sender.as_ref() {
            osc_sender.render_at(block_start);
        }

        SpatialMetadataCoordinator::new(
            &mut self.spatial,
            self.spatial_renderer.as_ref(),
            self.telemetry.osc_sender.as_mut(),
        )
        .handle_spatial_metadata(&frame, frame.sampling_frequency)?;

        self.session.decoded_samples += sample_count as u64;

        let latency_snapshot = self
            .output
            .audio_writer
            .as_ref()
            .and_then(|w| w.latency_snapshot());
        // Where the listener is: everything written before this frame, less
        // what is still in flight behind the render — the output chain the
        // writer measures, and the render's own delay (a linear-phase
        // crossover). Lets a client show each block when it is heard.
        if let (Some(latency), Some(osc_sender)) =
            (latency_snapshot, self.telemetry.osc_sender.as_ref())
        {
            let rate = sample_rate.max(1);
            let in_flight = (f64::from(latency.final_latency_ms.max(0.0)) * f64::from(rate)
                / 1000.0) as u64
                + self
                    .spatial_renderer
                    .as_ref()
                    .map_or(0, |r| r.output_latency_samples() as u64);
            osc_sender.send_heard(block_start.saturating_sub(in_flight), rate);
        }
        let current_latency_instant_ms = latency_snapshot.map(|snapshot| snapshot.final_latency_ms);
        let current_latency_control_ms =
            latency_snapshot.and_then(|snapshot| snapshot.control_latency_ms);
        let current_latency_target_ms =
            latency_snapshot.and_then(|snapshot| snapshot.target_control_latency_ms);
        let current_resample_ratio = self
            .output
            .audio_writer
            .as_ref()
            .and_then(|w| w.resample_ratio());
        // Propagate output rate-adjust to the input DRIVER feedback loop.
        if let (Some(rate), Some(ic)) = (current_resample_ratio, self.input_control.as_ref()) {
            ic.set_output_rate_adjust(rate);
        }
        let wants_output_driven_bridge_clock = self.wants_output_driven_bridge_clock();
        if !wants_output_driven_bridge_clock {
            self.reset_direct_trigger_wiring();
        }
        // Wire direct trigger mode once both the writer and the capture rate are ready.
        if wants_output_driven_bridge_clock && !self.session.direct_trigger_wired {
            if let Some(ic) = self.input_control.as_ref() {
                let rate_hz = ic.input_trigger_rate_hz();
                let quantum_frames = ic.input_trigger_quantum_frames();
                // Logged until wiring succeeds: this mode stays silent when a
                // precondition is missing, and the quantum in particular is
                // worth seeing — it sets the trigger rate, so a wrong one makes
                // the sink pull at a multiple of real time.
                log::debug!(
                    "Direct trigger wiring attempt: rate={rate_hz}Hz quantum={quantum_frames}fr writer={}",
                    self.output.audio_writer.is_some()
                );
                if rate_hz > 0 && quantum_frames > 0 {
                    if let Some(writer) = self.output.audio_writer.as_ref() {
                        writer.set_input_trigger_rate_hz(rate_hz);
                        writer.set_input_trigger_quantum_frames(quantum_frames);
                        #[cfg(target_os = "linux")]
                        if let Some(pending) = writer.pending_input_triggers() {
                            ic.set_pending_input_triggers(pending);
                            ic.set_direct_trigger_active(true);
                            self.session.direct_trigger_wired = true;
                            log::info!(
                                "Direct trigger mode active: capture={}Hz quantum={}fr; capture mainloop paces trigger_process() on the output-derived schedule",
                                rate_hz,
                                quantum_frames
                            );
                        }
                    }
                }
            }
        }
        if wants_output_driven_bridge_clock {
            if let Some(ic) = self.input_control.as_ref() {
                if let Some(writer) = self.output.audio_writer.as_ref() {
                    let rate_hz = ic.input_trigger_rate_hz();
                    let quantum_frames = ic.input_trigger_quantum_frames();
                    if rate_hz > 0 {
                        writer.set_input_trigger_rate_hz(rate_hz);
                    }
                    if quantum_frames > 0 {
                        writer.set_input_trigger_quantum_frames(quantum_frames);
                    }
                }
            }
        }
        let current_adaptive_band = self
            .output
            .audio_writer
            .as_ref()
            .and_then(|w| w.adaptive_band());

        if let Some(measured_ms) = current_latency_instant_ms {
            let baseline_ms = *self
                .session
                .first_measured_output_delay_ms
                .get_or_insert(measured_ms);
            let should_log = self
                .session
                .last_output_delay_log_at
                .map(|prev| now.saturating_duration_since(prev).as_secs_f64() >= 1.0)
                .unwrap_or(true);
            if should_log {
                let session_elapsed_s = self
                    .session
                    .started_at
                    .map(|started| now.saturating_duration_since(started).as_secs_f64())
                    .unwrap_or(0.0);
                let decoded_audio_ms =
                    self.session.decoded_samples as f64 / sample_rate.max(1) as f64 * 1000.0;
                let slope_ms_per_s = if session_elapsed_s > 0.0 {
                    (measured_ms - baseline_ms) as f64 / session_elapsed_s
                } else {
                    0.0
                };
                log::trace!(
                    "Output delay trend: measured_ms={:.3} control_ms={:?} target_ms={:?} delta_from_start_ms={:+.3} slope_ms_per_s={:+.3} session_s={:.3} decoded_audio_ms={:.0} ratio={:?} band={:?}",
                    measured_ms,
                    current_latency_control_ms,
                    current_latency_target_ms,
                    measured_ms - baseline_ms,
                    slope_ms_per_s,
                    session_elapsed_s,
                    decoded_audio_ms,
                    current_resample_ratio,
                    current_adaptive_band,
                );
                self.session.last_output_delay_log_at = Some(now);
            }
        }

        let (effective_channel_count, output_source) =
            self.output_shape(&frame, source, ctx.bed_conform);

        // A live headphone toggle changes that width under a writer that has
        // already been built for the old one. Retire it here so the block below
        // builds a new one; the alternative is the same mismatch, arrived at
        // from the other direction.
        if self
            .output
            .audio_writer_channels
            .is_some_and(|built| built != effective_channel_count)
        {
            log::info!(
                "Output width changed ({} → {} channels): rebuilding the audio writer",
                self.output.audio_writer_channels.unwrap_or(0),
                effective_channel_count,
            );
            if let Some(mut writer) = self.output.invalidate_writer(self.input_control.as_deref()) {
                let _ = writer.flush();
            }
            self.output.reset_realtime_output_tracking();
        }

        // Nor can a file capture carried over a stream end take a stream at
        // another rate: the sink writes samples as they come, so they would
        // sit under a CAF header describing the streams before them and play
        // at the wrong speed. That stream starts the file over, as every
        // stream did before captures were carried over.
        if let Some(capture_rate) = self
            .output
            .carried_capture_rate
            .take()
            .filter(|&rate| rate != sample_rate)
        {
            log::warn!(
                "Stream at {} Hz cannot continue the {} Hz capture in '{}': starting the file over",
                sample_rate,
                capture_rate,
                self.runtime.output_file,
            );
            if let Some(mut writer) = self.output.invalidate_writer(self.input_control.as_deref()) {
                let _ = writer.flush();
            }
        }

        OutputRuntimeCoordinator::new(
            &mut self.output,
            &mut self.runtime,
            self.audio_control.as_deref(),
            self.input_control.as_deref(),
        )
        .sync_all()?;
        // `sync_all` may have switched the active backend live (e.g. Studio
        // requested `file`); read the post-sync value for the writer build.
        let active_output_backend = self.runtime.active_output_backend;
        if self.output.audio_writer.is_none() {
            self.reset_direct_trigger_wiring();
        }
        WriterLifecycleCoordinator::new(
            &mut self.output,
            &self.runtime,
            &mut self.telemetry,
            &self.spatial,
            &self.session,
            self.spatial_renderer.as_ref(),
            self.audio_control.as_ref(),
            self.input_control.as_ref(),
        )
        .create_audio_writer_if_needed(
            active_output_backend,
            sample_rate,
            effective_channel_count,
            output_source,
        )?;
        WriterLifecycleCoordinator::new(
            &mut self.output,
            &self.runtime,
            &mut self.telemetry,
            &self.spatial,
            &self.session,
            self.spatial_renderer.as_ref(),
            self.audio_control.as_ref(),
            self.input_control.as_ref(),
        )
        .publish_audio_state_if_changed(active_output_backend, sample_rate);

        let mut sample_write = SampleWriteCoordinator::new(
            &mut self.output,
            &mut self.telemetry,
            &mut self.spatial,
            &self.session,
            self.input_control.as_deref(),
            self.spatial_renderer.as_mut(),
        );
        if ctx.bed_conform && sample_write.frame_has_objects(source) {
            sample_write.write_audio_samples_bed_conform(&frame, ctx.decode_time_ms)?;
        } else {
            sample_write.write_audio_samples(&frame, ctx.decode_time_ms, source)?;
        }

        Ok(())
    }

    pub fn finalize(&mut self) -> Result<()> {
        if let Some(ref mut writer) = self.output.audio_writer {
            writer.finish()?;
        }

        Ok(())
    }

    /// The sink's width for this frame, and whether it carries the renderer's
    /// output or the decoded channels as they are. The one place this host
    /// decides it: the writer is built, labelled, rebuilt on a change and fed
    /// from it — a second guess anywhere else is how a binaural stereo pair
    /// once ended up in a speaker-wide sink.
    ///
    /// - bed-conformed export of an object frame: the conformed decoded PCM;
    /// - host passthrough (channel content, channel render mode `host`): the
    ///   decoded channels, unrendered;
    /// - otherwise, with a renderer: what the renderer emits
    ///   ([`SpatialRenderer::output_channel_count`] — the speakers, or the
    ///   binaural pair);
    /// - no renderer: the decoded channels.
    ///
    /// [`SpatialRenderer::output_channel_count`]: renderer::spatial_renderer::SpatialRenderer::output_channel_count
    pub(crate) fn output_shape(
        &self,
        frame: &RDecodedFrame,
        source: DecodedSource,
        bed_conform: bool,
    ) -> (usize, OutputSource) {
        let channel_count = frame.channel_count as usize;
        let frame_has_objects = self.spatial.frame_has_objects(source);
        if bed_conform && frame_has_objects {
            let bed_indices = self.spatial.bed_indices.as_deref().unwrap_or(&[]);
            return (
                ChannelCountCalculator::calculate_conformed_channel_count(
                    channel_count,
                    bed_indices,
                ),
                OutputSource::Decoded,
            );
        }
        match self.spatial_renderer.as_ref() {
            Some(renderer)
                if frame_has_objects
                    || renderer.renderer_control().live.read().channel_render_mode
                        != ChannelRenderMode::Host =>
            {
                (renderer.output_channel_count(), OutputSource::Rendered)
            }
            _ => (channel_count, OutputSource::Decoded),
        }
    }

    /// A new segment (`is_new_segment`): the realtime output restarts its
    /// tracking and the spatial state starts over. The writer is retired, not
    /// rebuilt here: the frame that follows builds it through
    /// [`output_shape`](Self::output_shape) like any other, at the right width
    /// and with the right channel names. (It used to be rebuilt here at the
    /// decoded width, unlabelled, and then again by that frame whenever the
    /// renderer's width differed — every segment.)
    ///
    /// A file sink is kept: rebuilding it reopens — truncates — the
    /// destination, which threw away everything written before the segment.
    pub fn handle_stream_restart(&mut self, output_backend: OutputBackend) -> Result<()> {
        log::info!(
            "Stream restart detected at AU {}, resetting realtime output state",
            self.spatial.au_index
        );

        if output_backend != OutputBackend::File {
            if let Some(mut writer) = self.output.invalidate_writer(self.input_control.as_deref()) {
                writer.flush()?;
            }
        }
        self.reset_direct_trigger_wiring();
        self.output.reset_realtime_output_tracking();
        self.session.first_measured_output_delay_ms = None;
        self.session.last_output_delay_log_at = None;
        self.reset_spatial_state_for_segment();
        Ok(())
    }

    /// The stream ended (continuous mode) and its output is flushed: the
    /// handler starts over for the next one, keeping what outlives a stream.
    ///
    /// That includes a sink writing to a regular file. The next frame would
    /// build a new one, and opening the path again truncates it: a capture
    /// then only ever held what followed the last stream end, where the same
    /// run to stdout holds everything. Any other writer is dropped, as it
    /// always was — which is what tells a FIFO's reader the stream is over.
    ///
    /// The capture is one format. Only a stream of the same width and rate
    /// continues it; the next frame checks both
    /// ([`handle_decoded_frame`](Self::handle_decoded_frame)), and another
    /// format starts the file over.
    pub fn reset_for_next_stream(&mut self) {
        let spatial_renderer = self.spatial_renderer.take();
        let audio_control = self.audio_control.take();
        let input_control = self.input_control.take();
        // The decoders outlive the stream: without their DRC links, a mode picked
        // after the first stream end never reached them.
        let drc = std::mem::take(&mut self.drc);
        let osc_sender = self.telemetry.osc_sender.take();
        let audio_meter = self.telemetry.audio_meter.take();
        let runtime = self.runtime.clone();
        // A property of the bridge, which outlives the stream too.
        let coordinate_format = self.spatial.stream.coordinate_format;
        let file_capture = self
            .output
            .audio_writer
            .take_if(|writer| writer.is_regular_file_sink());
        let file_capture_channels = self.output.audio_writer_channels;
        // What the capture holds is at the rate of the stream that ended; one
        // that brought no frame leaves the rate carried over before it.
        let file_capture_rate = self
            .output
            .carried_capture_rate
            .unwrap_or(self.session.final_sample_rate);

        *self = DecodeHandler::default();

        self.spatial.stream.coordinate_format = coordinate_format;

        self.spatial_renderer = spatial_renderer;
        self.audio_control = audio_control;
        self.input_control = input_control;
        self.drc = drc;
        self.telemetry.osc_sender = osc_sender;
        self.telemetry.audio_meter = audio_meter;
        self.runtime = runtime;
        if file_capture.is_some() {
            self.output.audio_writer = file_capture;
            self.output.audio_writer_channels = file_capture_channels;
            self.output.carried_capture_rate = Some(file_capture_rate);
        }
        if let Some(ref mut osc_sender) = self.telemetry.osc_sender {
            osc_sender.bump_content_generation();
        }
    }

    fn reset_spatial_state_for_segment(&mut self) {
        SpatialMetadataCoordinator::new(
            &mut self.spatial,
            self.spatial_renderer.as_ref(),
            self.telemetry.osc_sender.as_mut(),
        )
        .reset_for_segment();
    }

    /// The bridge reset itself (sync loss, seek): the spatial state starts
    /// over, as in the embedded engine. Audio keeps flowing — the writer and
    /// its buffers are left alone, a transient decoder reset must not turn
    /// into a dropout.
    pub fn handle_bridge_reset(&mut self) {
        log::debug!("Bridge reset: starting the spatial state over");
        self.reset_spatial_state_for_segment();
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use bridge_api::RChannelLabel;

    pub(in crate::cli::decode) fn test_renderer() -> renderer::spatial_renderer::SpatialRenderer {
        orender_engine::renderer_build::build_spatial_renderer(
            &orender_engine::renderer_build::SpatialRendererParams::from_render_config(None),
            renderer::speaker_layout::SpeakerLayout::preset("7.1.4").expect("preset"),
            48_000,
            bridge_api::RVbapCartesianDefaults {
                x_size: 9,
                y_size: 9,
                z_size: 5,
                allow_negative_z: false,
            },
            bridge_api::RVbapTableMode::Cartesian,
            None,
        )
        .expect("renderer")
    }

    /// A 5.1 bed frame, as the decoder thread hands it over.
    fn bed_frame() -> RDecodedFrame {
        bed_frame_of(480, 48_000)
    }

    /// A 5.1 bed frame of `sample_count` samples at `sampling_frequency`.
    fn bed_frame_of(sample_count: u32, sampling_frequency: u32) -> RDecodedFrame {
        use RChannelLabel::*;
        let labels = vec![L, R, C, LFE, Ls, Rs];
        RDecodedFrame {
            sampling_frequency,
            sample_count,
            channel_count: labels.len() as u32,
            pcm: vec![0i32; sample_count as usize * labels.len()].into(),
            channel_labels: labels.into(),
            metadata: abi_stable::std_types::RVec::new(),
            drc_gain: 1.0,
            drc_ramp_duration: 0,
            dialogue_level: abi_stable::std_types::ROption::RNone,
            is_new_segment: false,
        }
    }

    /// Switch the renderer to headphones and render until the cross-fade has
    /// landed on the binaural pair.
    fn switch_to_binaural(renderer: &mut renderer::spatial_renderer::SpatialRenderer) {
        renderer
            .renderer_control()
            .live
            .write()
            .binaural
            .output_mode = renderer::live_params::OutputMode::Binaural;
        let silence = vec![0.0f32; 480 * 6];
        for _ in 0..16 {
            renderer
                .render_frame(&silence, 6, &[], Vec::new(), false)
                .expect("render");
            if !renderer.output_is_speaker_array() {
                return;
            }
        }
        panic!("the output mode never switched to binaural");
    }

    /// In headphone mode the sink is the binaural stereo pair, labelled as
    /// such — not the speaker count, and not the layout's speaker names (the
    /// class of bug that once fed a stereo pair to a twelve-channel sink).
    #[test]
    fn a_binaural_render_gets_a_stereo_sink_named_fl_fr() {
        let mut renderer = test_renderer();
        switch_to_binaural(&mut renderer);
        assert_eq!(renderer.output_channel_names(), ["FL", "FR"]);

        let handler = DecodeHandler {
            spatial_renderer: Some(renderer),
            ..DecodeHandler::default()
        };
        assert_eq!(
            handler.output_shape(&bed_frame(), DecodedSource::Bridge, false),
            (2, OutputSource::Rendered)
        );
    }

    /// Host passthrough writes the decoded channels unrendered, so the sink is
    /// sized for them; the renderer's width only applies to what it renders.
    #[test]
    fn host_passthrough_sizes_the_sink_for_the_decoded_channels() {
        let renderer = test_renderer();
        let speakers = renderer.output_channel_count();
        renderer.renderer_control().live.write().channel_render_mode = ChannelRenderMode::Host;
        let mut handler = DecodeHandler {
            spatial_renderer: Some(renderer),
            ..DecodeHandler::default()
        };
        assert_eq!(
            handler.output_shape(&bed_frame(), DecodedSource::Bridge, false),
            (6, OutputSource::Decoded)
        );
        // An object stream is always rendered, whatever the channel mode.
        handler.spatial.stream.has_objects = true;
        assert_eq!(
            handler.output_shape(&bed_frame(), DecodedSource::Bridge, false),
            (speakers, OutputSource::Rendered)
        );
    }

    /// A segment start resets the spatial state the way the embedded engine
    /// does: OSC clients are told the content changed (so the previous
    /// layout's objects are purged) and the new segment's dialogue level is
    /// applied instead of the previous one's.
    #[test]
    fn a_segment_restart_resets_like_the_engine() {
        let osc = orender_engine::osc::OscSender::new("127.0.0.1:9".parse().unwrap())
            .expect("osc sender");
        let generation = osc.content_generation();
        let mut handler = DecodeHandler {
            spatial_renderer: Some(test_renderer()),
            ..DecodeHandler::default()
        };
        handler.telemetry.osc_sender = Some(osc);
        handler.spatial.stream.dialnorm = Some(-27);
        handler.spatial.stream.has_objects = true;
        handler
            .spatial
            .stream
            .object_names
            .insert(3, "Dialog".to_string());

        handler
            .handle_stream_restart(OutputBackend::Unsupported)
            .expect("restart");

        let osc = handler.telemetry.osc_sender.as_ref().unwrap();
        assert_eq!(osc.content_generation(), generation + 1);
        assert_eq!(handler.spatial.stream.dialnorm, None);
        assert!(!handler.spatial.stream.has_objects);
        assert!(handler.spatial.stream.object_names.is_empty());
    }

    /// A file sink survives a segment start: rebuilding it reopens — and
    /// truncates — the destination, losing everything written before.
    #[test]
    fn a_segment_restart_keeps_the_file_sink() {
        let path = std::env::temp_dir().join(format!(
            "orender-segment-restart-{}.f32",
            std::process::id()
        ));
        let mut handler = DecodeHandler::default();
        handler.output.audio_writer = Some(
            super::super::output::AudioWriter::create_file(
                path.to_str().unwrap(),
                audio_output::FileSinkFormat::RawF32,
                48_000,
                2,
                None,
            )
            .expect("file sink"),
        );
        handler.output.audio_writer_channels = Some(2);
        let block = super::super::output::AudioSamples::F32(vec![0.25; 480 * 2]);
        let write = |handler: &mut DecodeHandler| {
            handler
                .output
                .audio_writer
                .as_mut()
                .expect("writer")
                .write_pcm_samples(&block, 2)
                .expect("write");
        };

        write(&mut handler);
        handler
            .handle_stream_restart(OutputBackend::File)
            .expect("restart");
        write(&mut handler);
        handler.finalize().expect("finalize");
        drop(handler);

        let written = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            written,
            2 * 480 * 2 * 4,
            "the block before the restart was lost"
        );
    }

    /// What one [`bed_frame`] takes in a raw f32 sink, written unrendered.
    const BED_FRAME_BYTES: usize = 480 * 6 * 4;

    /// A handler as `--output-backend file --output-file <destination>` leaves
    /// it, with no renderer: frames reach the sink as decoded.
    fn file_output_handler(destination: &std::path::Path) -> DecodeHandler {
        let mut handler = DecodeHandler::default();
        handler.runtime.active_output_backend = OutputBackend::File;
        handler.runtime.output_file = destination.to_str().expect("utf-8 path").to_owned();
        handler
    }

    /// One frame through the handler, as the decoder thread hands it over.
    fn feed(handler: &mut DecodeHandler, frame: RDecodedFrame) {
        let ctx = FrameHandlerContext {
            bed_conform: false,
            decode_time_ms: 0.0,
            queue_delay_ms: 0.0,
        };
        handler
            .handle_decoded_frame(DecodedSource::Bridge, frame, &ctx)
            .expect("frame");
    }

    /// A capture to a regular file holds the streams of a continuous run one
    /// after the other. The stream end used to drop the sink with the rest of
    /// the handler, and the next frame built a new one — truncating the file,
    /// so it only ever held what followed the last stream end.
    #[test]
    fn a_stream_end_keeps_a_regular_file_sink() {
        let path =
            std::env::temp_dir().join(format!("orender-stream-end-{}.f32", std::process::id()));
        let mut handler = file_output_handler(&path);

        feed(&mut handler, bed_frame());
        handler.finalize().expect("finalize");
        handler.reset_for_next_stream();
        feed(&mut handler, bed_frame());
        handler.finalize().expect("finalize");
        drop(handler);

        let written = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            written,
            2 * BED_FRAME_BYTES as u64,
            "the stream before the stream end was lost"
        );
    }

    /// The sample rate a CAF header declares, and the bytes of audio after it.
    /// The header has no `chan` chunk: no renderer, no speakers to describe.
    fn caf_rate_and_payload(caf: &[u8]) -> (f64, usize) {
        const HEADER_BYTES: usize = 68;
        assert_eq!(&caf[..4], b"caff");
        assert_eq!(&caf[8..12], b"desc");
        assert_eq!(&caf[52..56], b"data");
        let rate = f64::from_be_bytes(caf[20..28].try_into().unwrap());
        (rate, caf.len() - HEADER_BYTES)
    }

    /// A capture is at one rate: the sink writes samples as they come, under
    /// a CAF header that describes them once. Streams at that rate follow one
    /// another in it; a stream at another rate starts the file over with a
    /// header of its own, where it would otherwise play at the wrong speed.
    #[test]
    fn a_stream_at_another_rate_starts_the_capture_over() {
        let path = std::env::temp_dir().join(format!(
            "orender-stream-end-rate-{}.caf",
            std::process::id()
        ));
        let mut handler = file_output_handler(&path);
        handler.runtime.output_file_format = crate::cli::command::OutputFileFormatArg::Caf;
        let capture = || caf_rate_and_payload(&std::fs::read(&path).expect("capture"));
        let stream = |handler: &mut DecodeHandler, rate: u32| {
            feed(handler, bed_frame_of(480, rate));
            handler.finalize().expect("finalize");
            handler.reset_for_next_stream();
        };

        stream(&mut handler, 48_000);
        stream(&mut handler, 48_000);
        let same_rate = capture();
        stream(&mut handler, 96_000);
        let other_rate = capture();
        // A stream that brings no frame does not make the capture forget its
        // rate.
        handler.reset_for_next_stream();
        stream(&mut handler, 48_000);
        let after_an_empty_stream = capture();
        drop(handler);
        let _ = std::fs::remove_file(&path);

        assert_eq!(same_rate, (48_000.0, 2 * BED_FRAME_BYTES));
        assert_eq!(other_rate, (96_000.0, BED_FRAME_BYTES));
        assert_eq!(after_an_empty_stream, (48_000.0, BED_FRAME_BYTES));
    }

    /// A FIFO sink is still closed by a stream end, which is how its reader
    /// learns the stream is over, and opened again by the next stream.
    #[cfg(unix)]
    #[test]
    fn a_stream_end_still_closes_a_fifo_sink() {
        use std::io::Read;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::OpenOptionsExt;

        // This thread writes a stream into the FIFO and only then reads it
        // back, so a stream must fit whatever the pipe holds or its flush
        // never returns. That is as little as a page or two for a user over
        // the pipe budget (`pipe-user-pages-soft`); `_POSIX_PIPE_BUF`, 512
        // bytes, always fits.
        const SAMPLES: u32 = 8;
        const STREAM_BYTES: usize = SAMPLES as usize * 6 * 4;
        const { assert!(STREAM_BYTES <= 512) };

        let path =
            std::env::temp_dir().join(format!("orender-stream-end-{}.fifo", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        // Non-blocking, so an empty pipe reads as `WouldBlock` while a writer
        // holds it and as end of file once none does.
        let mut reader = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
            .expect("fifo reader");
        let mut stream = [0u8; STREAM_BYTES];
        let mut handler = file_output_handler(&path);

        feed(&mut handler, bed_frame_of(SAMPLES, 48_000));
        handler.finalize().expect("finalize");
        reader.read_exact(&mut stream).expect("first stream");
        let pending = reader
            .read(&mut stream)
            .expect_err("the sink is still open");
        assert_eq!(pending.kind(), std::io::ErrorKind::WouldBlock);

        handler.reset_for_next_stream();
        assert!(handler.output.audio_writer.is_none());
        assert_eq!(reader.read(&mut stream).expect("end of stream"), 0);

        feed(&mut handler, bed_frame_of(SAMPLES, 48_000));
        handler.finalize().expect("finalize");
        reader.read_exact(&mut stream).expect("second stream");
        drop(handler);
        let _ = std::fs::remove_file(&path);
    }
}
