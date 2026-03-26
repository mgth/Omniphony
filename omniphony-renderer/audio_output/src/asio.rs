#![cfg(target_os = "windows")]

use anyhow::{Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam::queue::ArrayQueue;
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering},
};
use std::time::Duration;

use crate::{
    AdaptiveResamplingConfig, LOCAL_RESAMPLER_MAX_RELATIVE_RATIO, adaptive_band_name,
    clamp_ratio_for_local_resampler, local_resampler_ratio_bounds,
    adaptive_runtime::{
        AdaptiveRuntimeState, FarModeDecision, LatencyMetricTargets, MAX_INTEGRAL_TERM,
        compute_hard_recover_plan, note_refill_or_underrun, output_to_input_domain_samples,
        paused_rate_adjust, postprocess_interleaved_output, reset_adaptive_runtime,
        run_adaptive_servo, should_run_adaptive_servo, update_far_mode_state,
        update_latency_metrics, zero_pad_tail,
    },
    ring_buffer_io::{flush_ring_buffer, push_samples_with_backpressure},
    resampler_fifo::{RESAMPLER_CHUNK_SIZE, ResamplerFifoEngine},
};

// Buffer size: 4 seconds of audio at 48kHz, 16 channels
const BUFFER_SIZE: usize = 48000 * 16 * 4;

// Adaptive rate matching constants (time-domain targets).
const MIN_BUFFER_MS: u32 = 25;
const DEFAULT_TARGET_BUFFER_MS: u32 = 220;
const MAX_BUFFER_MS: u32 = 250;
pub struct AsioWriter {
    sample_buffer: Arc<ArrayQueue<f32>>,
    input_sample_rate: u32,
    _output_sample_rate: u32,
    channel_count: u32,        // Number of audio channels we're producing
    _device_channel_count: u32, // Number of channels the ASIO device expects
    _stream_ready: Arc<AtomicBool>,
    enable_adaptive_resampling: bool, // Enable PI controller for buffer stability
    max_buffer_fill: usize,
    target_buffer_fill: usize,
    current_rate_adjust: Arc<AtomicU32>,
    current_adaptive_band: Arc<AtomicU8>,
    measured_latency_ms_bits: Arc<AtomicU32>,
    control_latency_ms_bits: Arc<AtomicU32>,
    pipeline_latency_ms_bits: Arc<AtomicU32>,
    live_adaptive_config: Arc<Mutex<AdaptiveResamplingConfig>>,
    reset_ratio_requested: Arc<AtomicBool>,
    // We keep the stream alive by holding it here, though cpal streams run in background threads
    _stream: Option<cpal::Stream>,
}

/// Get a list of available ASIO device names
pub fn list_asio_devices() -> Result<Vec<String>> {
    let host = cpal::host_from_id(cpal::HostId::Asio)
        .map_err(|e| anyhow!("ASIO Host not available: {:?}", e))?;

    let devices: Vec<String> = host
        .output_devices()?
        .filter_map(|d| d.name().ok())
        .collect();

    Ok(devices)
}

impl AsioWriter {
    pub fn list_asio_devices() -> Result<()> {
        println!("Available ASIO Devices:");
        let devices = list_asio_devices()?;

        for (i, device_name) in devices.iter().enumerate() {
            println!("  {}: {}", i, device_name);
        }

        Ok(())
    }

    pub fn new(
        input_sample_rate: u32,
        sample_rate: u32,
        channel_count: u32,
        output_device: Option<String>,
        target_latency_ms: u32,
        enable_adaptive_resampling: bool,
        adaptive_config: AdaptiveResamplingConfig,
    ) -> Result<Self> {
        Self::new_with_channel_names(
            input_sample_rate,
            sample_rate,
            channel_count,
            output_device,
            None,
            target_latency_ms,
            enable_adaptive_resampling,
            adaptive_config,
        )
    }

    pub fn new_with_channel_names(
        input_sample_rate: u32,  // Decoded stream sample rate
        output_sample_rate: u32, // Target output rate (e.g., 96000 Hz for upsampling)
        channel_count: u32,
        output_device: Option<String>,
        _channel_names: Option<Vec<String>>,
        target_latency_ms: u32,
        enable_adaptive_resampling: bool,
        adaptive_config: AdaptiveResamplingConfig,
    ) -> Result<Self> {
        // Local resampling ratio is output_rate / input_rate.
        let resample_ratio = output_sample_rate as f64 / input_sample_rate as f64;

        let sample_buffer = Arc::new(ArrayQueue::new(BUFFER_SIZE));
        let buffer_clone = sample_buffer.clone();
        let stream_ready = Arc::new(AtomicBool::new(false));
        let ready_clone = stream_ready.clone();
        // Pre-fill gate: the ASIO callback outputs silence until the ring buffer
        // has accumulated at least target_buffer_fill samples.  This prevents
        // underruns caused by bursty pipe delivery (e.g. mpv ao=pcm writing
        // several frames at once then being silent for the inter-burst gap).
        let playback_ready = Arc::new(AtomicBool::new(false));
        let playback_ready_clone = Arc::clone(&playback_ready);
        let current_rate_adjust = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let current_rate_adjust_clone = current_rate_adjust.clone();
        let current_adaptive_band = Arc::new(AtomicU8::new(0));
        let current_adaptive_band_clone = current_adaptive_band.clone();
        let measured_latency_ms_bits = Arc::new(AtomicU32::new(0u32));
        let measured_latency_ms_bits_clone = measured_latency_ms_bits.clone();
        let control_latency_ms_bits = Arc::new(AtomicU32::new(0u32));
        let control_latency_ms_bits_clone = control_latency_ms_bits.clone();
        let pipeline_latency_ms_bits = Arc::new(AtomicU32::new(0u32));
        let pipeline_latency_ms_bits_clone = pipeline_latency_ms_bits.clone();
        // Ring-buffer thresholds in INPUT-domain samples (same domain as buffer_clone.len()).
        let samples_per_ms =
            (input_sample_rate as usize).saturating_mul(channel_count as usize) / 1000;
        let samples_per_ms_f64 = samples_per_ms as f64;
        let target_buffer_ms = if target_latency_ms == 0 {
            DEFAULT_TARGET_BUFFER_MS
        } else {
            target_latency_ms
        };
        let max_buffer_ms = MAX_BUFFER_MS.max(target_buffer_ms.saturating_mul(2));
        let min_buffer_fill = (samples_per_ms * MIN_BUFFER_MS as usize).max(channel_count as usize);
        let target_buffer_fill = (samples_per_ms * target_buffer_ms as usize).max(min_buffer_fill);
        let max_buffer_fill = (samples_per_ms * max_buffer_ms as usize)
            .max(target_buffer_fill + channel_count as usize);

        // Initialize CPAL ASIO host
        let host = cpal::host_from_id(cpal::HostId::Asio)
            .map_err(|e| anyhow!("ASIO Host not available: {:?}", e))?;

        log::info!("ASIO Host initialized");

        // Find device by name if specified, otherwise use default
        let device = if let Some(ref target_name) = output_device {
            // Search for device with matching name
            let mut found_device = None;
            for device in host.output_devices()? {
                if let Ok(name) = device.name() {
                    if name == *target_name {
                        found_device = Some(device);
                        break;
                    }
                }
            }

            found_device.ok_or_else(|| {
                anyhow!(
                    "ASIO device '{}' not found. Use 'orender list-asio-devices' to see available devices.",
                    target_name
                )
            })?
        } else {
            host.default_output_device()
                .ok_or_else(|| anyhow!("No default ASIO output device found"))?
        };

        log::info!("Using ASIO device: {}", device.name().unwrap_or_default());

        // Find a supported configuration that has at least the required channels
        let supported_configs: Vec<_> = device.supported_output_configs()?.collect();

        log::info!(
            "Looking for ASIO config supporting {} channels at {} Hz",
            channel_count,
            output_sample_rate
        );
        log::debug!("Available configurations:");
        for config_range in &supported_configs {
            log::debug!(
                "  Channels: {}, Sample rate: {:?}-{:?}, Sample format: {:?}",
                config_range.channels(),
                config_range.min_sample_rate(),
                config_range.max_sample_rate(),
                config_range.sample_format()
            );
        }

        // Find best matching config (prefer exact match, then next larger channel count)
        let best_config = supported_configs
            .iter()
            .filter(|c| {
                c.channels() >= channel_count as u16
                    && output_sample_rate >= c.min_sample_rate().0
                    && output_sample_rate <= c.max_sample_rate().0
            })
            .min_by_key(|c| c.channels())
            .ok_or_else(|| {
                anyhow!(
                    "ASIO device does not support {} channels at {} Hz. Available configs: {:?}",
                    channel_count,
                    output_sample_rate,
                    supported_configs
                        .iter()
                        .map(|c| format!(
                            "{}ch @ {}-{} Hz",
                            c.channels(),
                            c.min_sample_rate().0,
                            c.max_sample_rate().0
                        ))
                        .collect::<Vec<_>>()
                )
            })?;

        let device_channel_count = best_config.channels();

        // Configure stream with device's channel count and output sample rate
        let config = cpal::StreamConfig {
            channels: device_channel_count,
            sample_rate: cpal::SampleRate(output_sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        if resample_ratio != 1.0 {
            log::info!(
                "ASIO Config: {} Hz (resampling from {} Hz, ratio {:.3}x), {} device channels (using {} for output)",
                output_sample_rate,
                input_sample_rate,
                resample_ratio,
                device_channel_count,
                channel_count
            );
        } else {
            log::info!(
                "ASIO Config: {} Hz, {} device channels (using {} for output)",
                output_sample_rate,
                device_channel_count,
                channel_count
            );
        }

        if device_channel_count > channel_count as u16 {
            log::info!(
                "Device has more channels ({}) than needed ({}), extra channels will be silent",
                device_channel_count,
                channel_count
            );
        }

        if enable_adaptive_resampling {
            log::info!("Adaptive resampling enabled (PI controller for buffer stability)");
        } else {
            log::info!("Adaptive resampling disabled (fixed resampling ratio)");
        }
        log::info!(
            "ASIO buffer thresholds ({}ch @ {}Hz input): min={} target={} max={} samples",
            channel_count,
            input_sample_rate,
            min_buffer_fill,
            target_buffer_fill,
            max_buffer_fill
        );

        // Initialize Resampler (High quality Sinc)
        // Base ratio for upsampling (e.g., 2.0 for 48kHz -> 96kHz)
        // Adaptive rate matching will make small adjustments around this base ratio
        let params = SincInterpolationParameters {
            sinc_len: 256,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 256,
            window: WindowFunction::BlackmanHarris2,
        };

        let live_config = Arc::new(Mutex::new(adaptive_config));
        let live_config_for_callback = Arc::clone(&live_config);
        let initial_cfg = live_config.lock().unwrap().clone();

        // Calculate max ratio for adaptive adjustments
        // Allow small adjustments around the base resample ratio
        // Rubato expects a relative ratio bound (>= 1.0), not an absolute ratio.
        let max_resample_ratio_relative = LOCAL_RESAMPLER_MAX_RELATIVE_RATIO;
        let (min_resample_ratio_abs, max_resample_ratio_abs) =
            local_resampler_ratio_bounds(resample_ratio);

        log::debug!(
            "Initializing resampler: base_ratio={:.4}, min_ratio={:.4}, max_ratio={:.4}, chunk_size={}",
            resample_ratio,
            min_resample_ratio_abs,
            max_resample_ratio_abs,
            RESAMPLER_CHUNK_SIZE
        );

        let mut resampler = SincFixedIn::<f32>::new(
            resample_ratio,
            max_resample_ratio_relative,
            params,
            RESAMPLER_CHUNK_SIZE,
            channel_count as usize,
        )
        .map_err(|e| anyhow!("Failed to create resampler: {:?}", e))?;

        let mut resampler_fifo = ResamplerFifoEngine::new(channel_count as usize);
        let mut runtime_state = AdaptiveRuntimeState::new(resample_ratio);
        let mut effective_resample_ratio = resample_ratio;
        let reset_ratio_requested = Arc::new(AtomicBool::new(false));
        let reset_ratio_for_callback = Arc::clone(&reset_ratio_requested);
        let _near_far_threshold_samples =
            (initial_cfg.near_far_threshold_ms as usize).saturating_mul(samples_per_ms);
        let device_channel_count_for_callback = device_channel_count;
        let adaptive_resampling_enabled = enable_adaptive_resampling;

        let err_fn = |err| log::error!("an error occurred on stream: {}", err);

        let stream = device.build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let callback_count = runtime_state.advance_callback();

                // --- Test controls: reset ratio / pause PI ---
                if reset_ratio_for_callback.load(Ordering::Relaxed) {
                    reset_ratio_for_callback.store(false, Ordering::Relaxed);
                    let _ = resampler.set_resample_ratio(resample_ratio, false);
                    let reset = reset_adaptive_runtime(&mut runtime_state, resample_ratio);
                    effective_resample_ratio = reset.effective_resample_ratio;
                    current_rate_adjust_clone
                        .store(reset.displayed_rate_adjust.to_bits(), Ordering::Relaxed);
                    current_adaptive_band_clone.store(reset.adaptive_band, Ordering::Relaxed);
                }
                let is_pi_paused = live_config_for_callback
                    .try_lock()
                    .map(|cfg| cfg.paused)
                    .unwrap_or(false);

                // Pre-fill gate: hold playback until the ring buffer has enough
                // audio to sustain the first inter-burst gap without underrun.
                // Once the gate opens it stays open for the lifetime of the stream.
                if !playback_ready_clone.load(Ordering::Relaxed) {
                    let fill = buffer_clone.len();
                    if fill >= target_buffer_fill {
                        playback_ready_clone.store(true, Ordering::Relaxed);
                        log::info!(
                            "ASIO pre-fill complete ({} samples = {:.0} ms), starting playback",
                            fill,
                            fill as f32 / samples_per_ms_f64 as f32
                        );
                    } else {
                        data.fill(0.0);
                        return;
                    }
                }

                // 1. Check buffer fill & Calculate Rate
                let available_samples = buffer_clone.len(); // Input-domain samples (frames * channels)
                let output_fifo_input_domain_samples =
                    output_to_input_domain_samples(
                        resampler_fifo.output_len(),
                        effective_resample_ratio,
                    );
                // data.len() is in device-channel domain; convert it to rendered-audio samples
                // before comparing against the renderer/ring buffer fill level.
                let callback_frames = data.len() / device_channel_count_for_callback as usize;
                let callback_audio_samples = callback_frames * channel_count as usize;
                // Ring-buffer occupancy is tracked in input-domain samples, while the
                // ASIO callback consumes output-domain samples after local resampling.
                // Convert the callback midpoint estimate back to input-domain samples
                // before comparing against the input-domain fill level.
                let callback_input_domain_samples = if effective_resample_ratio > 0.0 {
                    ((callback_audio_samples as f64) / effective_resample_ratio).round() as usize
                } else {
                    callback_audio_samples
                };
                let callback_midpoint_ms = if channel_count > 0 && input_sample_rate > 0 {
                    (callback_input_domain_samples as f32
                        / channel_count as f32
                        / input_sample_rate as f32)
                        * 500.0
                } else {
                    0.0
                };
                pipeline_latency_ms_bits_clone
                    .store(callback_midpoint_ms.to_bits(), Ordering::Relaxed);
                let metrics = update_latency_metrics(
                    &mut runtime_state,
                    available_samples,
                    output_fifo_input_domain_samples,
                    callback_input_domain_samples,
                    channel_count as usize,
                    input_sample_rate,
                    callback_midpoint_ms,
                    LatencyMetricTargets {
                        measured_latency_ms_bits: &measured_latency_ms_bits_clone,
                        control_latency_ms_bits: &control_latency_ms_bits_clone,
                    },
                );

                // Adaptive rate logic (PI Controller)
                // Adjusts the resampling ratio around the base ratio to maintain buffer level
                // Only active if adaptive resampling is enabled
                if adaptive_resampling_enabled && !is_pi_paused {
                    // Only adjust rate if we have started playback and have enough data
                    let current_asio_cfg = live_config_for_callback.lock().unwrap().clone();
                    if should_run_adaptive_servo(
                        callback_count,
                        current_asio_cfg.update_interval_callbacks,
                        metrics.total_available_input_domain,
                        channel_count as usize,
                    ) {
                        let mut decision = run_adaptive_servo(
                            &mut runtime_state,
                            &current_asio_cfg,
                            metrics,
                            target_buffer_fill,
                            resample_ratio,
                            100,
                            current_asio_cfg.max_adjust.max(0.000_001),
                            samples_per_ms,
                            samples_per_ms_f64,
                        );

                        // Update resampler ratio
                        let clamped_ratio =
                            clamp_ratio_for_local_resampler(resample_ratio, decision.step.current_ratio);
                        decision.step.current_ratio = clamped_ratio;
                        decision.step.consume_adjust = resample_ratio / clamped_ratio;
                        decision.effective_resample_ratio = clamped_ratio;
                        decision.displayed_rate_adjust =
                            paused_rate_adjust(resample_ratio, clamped_ratio);

                        if let Err(e) = resampler.set_resample_ratio(clamped_ratio, true) {
                            log::warn!("Failed to set resampler ratio: {}", e);
                        } else {
                            effective_resample_ratio = clamped_ratio;
                        }
                        current_rate_adjust_clone
                            .store(decision.displayed_rate_adjust.to_bits(), Ordering::Relaxed);
                        current_adaptive_band_clone.store(decision.adaptive_band, Ordering::Relaxed);

                        if callback_count % 100 == 0 {
                            log::debug!(
                                "ASIO Adaptive: buf={}/{} drift={} ratio={:.6} (base={:.2} P={:.6} I={:.6} kp={:.6} ki={:.6} max_adjust={:.6})",
                                metrics.control_available,
                                target_buffer_fill,
                                decision.step.drift,
                                decision.step.current_ratio,
                                resample_ratio,
                                decision.step.p_term,
                                decision.step.i_term,
                                current_asio_cfg.kp_near,
                                current_asio_cfg.ki,
                                current_asio_cfg.max_adjust,
                            );
                        }
                    }
                } else if adaptive_resampling_enabled && is_pi_paused {
                    let held_consume_adjust =
                        paused_rate_adjust(resample_ratio, effective_resample_ratio);
                    current_rate_adjust_clone
                        .store(held_consume_adjust.to_bits(), Ordering::Relaxed);
                } else {
                    current_rate_adjust_clone.store(1.0f32.to_bits(), Ordering::Relaxed);
                    current_adaptive_band_clone.store(0, Ordering::Relaxed);
                }

                // 2. Feed Resampler until we have enough data in output_fifo for this callback
                // data.len() is frames * device_channel_count
                // output_fifo contains frames * channel_count
                let output_frames_needed = data.len() / device_channel_count_for_callback as usize;
                let audio_samples_needed = output_frames_needed * channel_count as usize;
                if let Err(e) =
                    resampler_fifo.ensure_output_samples(&buffer_clone, &mut resampler, audio_samples_needed)
                {
                    log::error!("Resampler error: {}", e);
                }

                // 3. Fill ASIO callback buffer from FIFO
                let far_mode_cfg = live_config_for_callback.lock().unwrap().clone();
                let far_decision: FarModeDecision = update_far_mode_state(
                    &mut runtime_state,
                    &far_mode_cfg,
                    adaptive_resampling_enabled,
                    current_adaptive_band_clone.load(Ordering::Relaxed) == crate::ADAPTIVE_BAND_FAR,
                    output_sample_rate,
                );
                if far_decision.hard_recover_far {
                    let plan = compute_hard_recover_plan(
                        callback_input_domain_samples,
                        metrics.control_available,
                        target_buffer_fill,
                        resample_ratio,
                        channel_count as usize,
                    );
                    if let Err(e) = resampler_fifo.ensure_output_samples(
                        &buffer_clone,
                        &mut resampler,
                        plan.desired_consume_output_samples,
                    ) {
                        log::error!("Resampler error: {}", e);
                    }
                    resampler_fifo.discard_samples(plan.desired_consume_output_samples);
                    data.fill(0.0);
                } else if resampler_fifo.output_len() >= audio_samples_needed {
                    // We have enough data
                    // Map audio channels to device channels
                    // If device has more channels than audio, extra channels are zeroed
                    data.fill(0.0); // Zero all channels first
                    let audio_samples = resampler_fifo.drain_to_vec(audio_samples_needed);

                    // Interleave into device buffer: frame by frame
                    for frame_idx in 0..output_frames_needed {
                        let device_frame_start = frame_idx * device_channel_count_for_callback as usize;
                        let audio_frame_start = frame_idx * channel_count as usize;

                        // Copy audio channels to device channels
                        for ch in 0..channel_count as usize {
                            data[device_frame_start + ch] = audio_samples[audio_frame_start + ch];
                        }
                        // Remaining device channels (if any) stay at 0.0
                    }
                    postprocess_interleaved_output(
                        data,
                        device_channel_count_for_callback as usize,
                        far_decision.mute_far_output,
                        &mut runtime_state,
                    );

                } else {
                    // Underrun
                    note_refill_or_underrun(
                        &mut runtime_state,
                        "ASIO underrun",
                        "ASIO underrun",
                        resampler_fifo.output_len(),
                        audio_samples_needed,
                    );
                    // Fill what we have, silence rest
                    let copied = resampler_fifo.drain_into_slice(data);
                    zero_pad_tail(data, copied);
                    postprocess_interleaved_output(
                        data,
                        device_channel_count_for_callback as usize,
                        far_decision.mute_far_output,
                        &mut runtime_state,
                    );
                }
            },
            err_fn,
            None, // Timeout
        )?;

        stream.play()?;
        ready_clone.store(true, Ordering::Relaxed);

        Ok(Self {
            sample_buffer,
            input_sample_rate,
            _output_sample_rate: output_sample_rate,
            channel_count,
            _device_channel_count: device_channel_count as u32,
            _stream_ready: stream_ready,
            enable_adaptive_resampling,
            max_buffer_fill,
            target_buffer_fill,
            current_rate_adjust,
            current_adaptive_band,
            measured_latency_ms_bits,
            control_latency_ms_bits,
            pipeline_latency_ms_bits,
            live_adaptive_config: live_config,
            reset_ratio_requested,
            _stream: Some(stream),
        })
    }

    pub fn write_samples(&mut self, samples: &[f32]) -> Result<()> {
        let report = push_samples_with_backpressure(
            &self.sample_buffer,
            samples,
            self.max_buffer_fill,
            10,
            200,
        );
        if report.timed_out {
            log::warn!("Buffer drain timeout");
        }
        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        let _ = flush_ring_buffer(
            &self.sample_buffer,
            Duration::from_secs(5),
            Duration::from_millis(50),
            None,
        );
        Ok(())
    }

    pub fn latency_ms(&self) -> f32 {
        self.measured_audio_delay_ms()
    }

    pub fn rate_adjust(&self) -> Option<f32> {
        if self.enable_adaptive_resampling {
            Some(f32::from_bits(
                self.current_rate_adjust.load(Ordering::Relaxed),
            ))
        } else {
            None
        }
    }

    pub fn adaptive_band(&self) -> Option<&'static str> {
        adaptive_band_name(self.current_adaptive_band.load(Ordering::Relaxed))
    }

    pub fn total_audio_delay_ms(&self) -> f32 {
        (self.target_buffer_fill as f32 / self.channel_count as f32 / self.input_sample_rate as f32)
            * 1000.0
            + f32::from_bits(self.pipeline_latency_ms_bits.load(Ordering::Relaxed))
    }

    pub fn measured_audio_delay_ms(&self) -> f32 {
        f32::from_bits(self.measured_latency_ms_bits.load(Ordering::Relaxed))
    }

    pub fn control_audio_delay_ms(&self) -> f32 {
        f32::from_bits(self.control_latency_ms_bits.load(Ordering::Relaxed))
    }

    /// Signal the audio thread to snap the resampling ratio back to base and reset the integrator.
    pub fn request_ratio_reset(&self) {
        self.reset_ratio_requested.store(true, Ordering::Relaxed);
    }

    /// Update adaptive resampling tuning parameters without restarting the audio stream.
    pub fn update_adaptive_config(&self, config: AdaptiveResamplingConfig) {
        if let Ok(mut c) = self.live_adaptive_config.lock() {
            *c = config;
        }
    }
}
