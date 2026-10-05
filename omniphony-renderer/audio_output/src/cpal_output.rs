#![cfg(any(target_os = "windows", target_os = "macos"))]
//! cpal-backed realtime output writer shared by the Windows (ASIO) and macOS
//! (CoreAudio) backends. The two platforms differ only in which cpal host they
//! open; the ring-buffer, local resampler and adaptive-rate servo are identical.

use anyhow::{Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use rubato::{Resampler, SincFixedIn};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering},
};
use std::time::Duration;

use crate::callback_log::{CallbackLog, CallbackLogDrain, callback_event};
use crate::output_telemetry::{interleaved_samples_to_ms, samples_to_ms};
use crate::{
    AdaptiveResamplingConfig, LOCAL_RESAMPLER_MAX_RELATIVE_RATIO, adaptive_band_name,
    adaptive_runtime::{
        AdaptiveRuntimeState, FarModeDecision, LatencyMetricTargets, LowRecoverPhase,
        adaptive_runtime_state_name, compute_hard_recover_high_plan, far_mode_band_from_latency,
        note_refill_or_underrun, output_to_input_domain_samples, paused_rate_adjust,
        postprocess_interleaved_output, reset_adaptive_runtime, run_adaptive_servo,
        should_run_adaptive_servo, store_latency_metrics_from_control_available,
        update_far_mode_state, update_latency_metrics, zero_pad_tail,
    },
    adaptive_runtime_state_code, adaptive_runtime_state_name_from_code,
    clamp_ratio_for_local_resampler, local_resampler_ratio_bounds,
    resampler_fifo::{RESAMPLER_CHUNK_SIZE, ResamplerFifoEngine, output_resampler_params},
    ring_buffer_io::{
        OUTPUT_RING_CAPACITY, RingMonitor, RingWriter, flush_ring_buffer,
        push_samples_drop_overflow, push_samples_with_backpressure, sample_ring,
    },
};

// Adaptive rate matching constants (time-domain targets).
const MIN_BUFFER_MS: u32 = 25;
const DEFAULT_TARGET_BUFFER_MS: u32 = 220;
const MAX_BUFFER_MS: u32 = 250;

/// Human-readable label for the active cpal output backend, used in logs and
/// error messages (referenced via inline `{BACKEND}` format args throughout).
#[cfg(target_os = "windows")]
const BACKEND: &str = "ASIO";
#[cfg(target_os = "macos")]
const BACKEND: &str = "CoreAudio";

/// Largest device callback, in frames, the converting stream wrapper sizes
/// its scratch for up front. A larger callback still works: the scratch
/// grows once.
const MAX_CALLBACK_FRAMES: usize = 8192;

/// Build the output stream in the device's native sample format `T`.
///
/// `render` always produces f32. For an f32 device it writes straight into
/// the device buffer; for any other format it renders into a scratch buffer
/// sized once here and each sample is converted on the way out, so the
/// callback allocates nothing in steady state.
fn build_stream<T, R>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut render: R,
    scratch_capacity: usize,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample + cpal::FromSample<f32> + 'static,
    R: FnMut(&mut [f32]) + Send + 'static,
{
    let err_fn = |err| log::error!("an error occurred on stream: {}", err);
    let stream = if T::FORMAT == cpal::SampleFormat::F32 {
        device.build_output_stream(
            config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| render(data),
            err_fn,
            None,
        )?
    } else {
        let mut scratch = vec![0.0f32; scratch_capacity];
        device.build_output_stream(
            config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                if scratch.len() < data.len() {
                    scratch.resize(data.len(), 0.0);
                }
                let buf = &mut scratch[..data.len()];
                render(buf);
                for (out, &sample) in data.iter_mut().zip(buf.iter()) {
                    *out = T::from_sample(sample);
                }
            },
            err_fn,
            None,
        )?
    };
    Ok(stream)
}

/// The post-rendering output pacer (`use_output_pacing`) exists only on the
/// PipeWire backend, where the bridge input thread drains it. Say so instead
/// of silently ignoring the request.
fn warn_if_output_pacing_requested(config: &AdaptiveResamplingConfig) {
    if config.use_output_pacing {
        log::warn!("{BACKEND}: output pacing is only implemented on PipeWire; ignored here");
    }
}

/// Open the cpal host that backs realtime output on this platform: the ASIO
/// host on Windows, the default (CoreAudio) host on macOS.
fn output_host() -> Result<cpal::Host> {
    #[cfg(target_os = "windows")]
    {
        cpal::host_from_id(cpal::HostId::Asio)
            .map_err(|e| anyhow!("{BACKEND} host not available: {:?}", e))
    }
    #[cfg(target_os = "macos")]
    {
        Ok(cpal::default_host())
    }
}

pub struct CpalWriter {
    /// The writing end of the ring the device callback reads. The renderer is
    /// its one producer.
    sample_buffer: RingWriter,
    /// The same ring's level, for the flush, and the request to drop what a
    /// flush gave up on.
    ring: RingMonitor,
    input_sample_rate: u32,
    _output_sample_rate: u32,
    channel_count: u32,         // Number of audio channels we're producing
    _device_channel_count: u32, // Number of channels the output device expects
    _stream_ready: Arc<AtomicBool>,
    enable_adaptive_resampling: bool, // Enable PI controller for buffer stability
    max_buffer_fill: usize,
    target_buffer_fill: usize,
    current_rate_adjust: Arc<AtomicU32>,
    current_adaptive_band: Arc<AtomicU8>,
    current_runtime_state: Arc<AtomicU8>,
    measured_latency_ms_bits: Arc<AtomicU32>,
    control_latency_ms_bits: Arc<AtomicU32>,
    pipeline_latency_ms_bits: Arc<AtomicU32>,
    avail_input_latency_ms_bits: Arc<AtomicU32>,
    output_fifo_latency_ms_bits: Arc<AtomicU32>,
    resampler_pending_latency_ms_bits: Arc<AtomicU32>,
    live_adaptive_config: Arc<Mutex<AdaptiveResamplingConfig>>,
    /// `AdaptiveResamplingConfig::disable_backpressure`, mirrored so the
    /// writer reads it without taking the config lock.
    backpressure_disabled: Arc<AtomicBool>,
    reset_ratio_requested: Arc<AtomicBool>,
    // We keep the stream alive by holding it here, though cpal streams run in background threads
    _stream: Option<cpal::Stream>,
    /// Logs what the device callback queues. Declared after the stream, so it
    /// is dropped after it (fields drop in order) and logs the last events.
    _callback_log_drain: CallbackLogDrain,
}

/// Get a list of available output device names for this platform's backend.
pub fn list_output_devices() -> Result<Vec<String>> {
    let host = output_host()?;

    let devices: Vec<String> = host
        .output_devices()?
        .filter_map(|d| d.name().ok())
        .collect();

    Ok(devices)
}

impl CpalWriter {
    pub fn list_output_devices() -> Result<()> {
        println!("Available {BACKEND} Devices:");
        let devices = list_output_devices()?;

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

        // The reading end moves into the callback, its one consumer.
        let (sample_buffer, mut ring_reader) = sample_ring(OUTPUT_RING_CAPACITY);
        let ring = sample_buffer.monitor();
        let stream_ready = Arc::new(AtomicBool::new(false));
        let ready_clone = stream_ready.clone();
        let current_rate_adjust = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let current_rate_adjust_clone = current_rate_adjust.clone();
        let current_adaptive_band = Arc::new(AtomicU8::new(0));
        let current_adaptive_band_clone = current_adaptive_band.clone();
        let current_runtime_state = Arc::new(AtomicU8::new(0));
        let current_runtime_state_clone = current_runtime_state.clone();
        let measured_latency_ms_bits = Arc::new(AtomicU32::new(0u32));
        let measured_latency_ms_bits_clone = measured_latency_ms_bits.clone();
        let control_latency_ms_bits = Arc::new(AtomicU32::new(0u32));
        let control_latency_ms_bits_clone = control_latency_ms_bits.clone();
        let pipeline_latency_ms_bits = Arc::new(AtomicU32::new(0u32));
        let pipeline_latency_ms_bits_clone = pipeline_latency_ms_bits.clone();
        let avail_input_latency_ms_bits = Arc::new(AtomicU32::new(0u32));
        let avail_input_latency_ms_bits_clone = avail_input_latency_ms_bits.clone();
        let output_fifo_latency_ms_bits = Arc::new(AtomicU32::new(0u32));
        let output_fifo_latency_ms_bits_clone = output_fifo_latency_ms_bits.clone();
        let resampler_pending_latency_ms_bits = Arc::new(AtomicU32::new(0u32));
        let resampler_pending_latency_ms_bits_clone = resampler_pending_latency_ms_bits.clone();
        // Ring-buffer thresholds in INPUT-domain samples (same domain as ring_reader.available()).
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

        // Open the platform's cpal output host (ASIO on Windows, CoreAudio on macOS).
        let host = output_host()?;

        log::info!("{BACKEND} host initialized");

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
                    "{BACKEND} output device '{target_name}' not found; run the platform list-output-devices command to see available devices.",
                )
            })?
        } else {
            host.default_output_device()
                .ok_or_else(|| anyhow!("No default {BACKEND} output device found"))?
        };

        log::info!(
            "Using {BACKEND} device: {}",
            device.name().unwrap_or_default()
        );

        // Find a supported configuration that has at least the required channels
        let supported_configs: Vec<_> = device.supported_output_configs()?.collect();

        log::info!(
            "Looking for {BACKEND} config supporting {} channels at {} Hz",
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
            // Fewest channels first; among those, f32 (no conversion).
            .min_by_key(|c| (c.channels(), c.sample_format() != cpal::SampleFormat::F32))
            .ok_or_else(|| {
                anyhow!(
                    "{BACKEND} device does not support {} channels at {} Hz. Available configs: {:?}",
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
        let sample_format = best_config.sample_format();

        // Configure stream with device's channel count and output sample rate
        let config = cpal::StreamConfig {
            channels: device_channel_count,
            sample_rate: cpal::SampleRate(output_sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        if resample_ratio != 1.0 {
            log::info!(
                "{BACKEND} Config: {} Hz (resampling from {} Hz, ratio {:.3}x), {} device channels (using {} for output)",
                output_sample_rate,
                input_sample_rate,
                resample_ratio,
                device_channel_count,
                channel_count
            );
        } else {
            log::info!(
                "{BACKEND} Config: {} Hz, {} device channels (using {} for output)",
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
            "{BACKEND} buffer thresholds ({}ch @ {}Hz input): min={} target={} max={} samples",
            channel_count,
            input_sample_rate,
            min_buffer_fill,
            target_buffer_fill,
            max_buffer_fill
        );

        // Initialize Resampler (High quality Sinc)
        // Base ratio for upsampling (e.g., 2.0 for 48kHz -> 96kHz)
        // Adaptive rate matching will make small adjustments around this base ratio
        let params = output_resampler_params();

        let backpressure_disabled = Arc::new(AtomicBool::new(adaptive_config.disable_backpressure));
        warn_if_output_pacing_requested(&adaptive_config);
        let live_config = Arc::new(Mutex::new(adaptive_config));
        let live_config_for_callback = Arc::clone(&live_config);
        let initial_cfg = live_config.lock().clone();

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
        runtime_state.activate_startup_low_recover();
        let mut effective_resample_ratio = resample_ratio;
        let reset_ratio_requested = Arc::new(AtomicBool::new(false));
        let reset_ratio_for_callback = Arc::clone(&reset_ratio_requested);
        let _high_recover_entry_margin_samples =
            (initial_cfg.high_recover_entry_margin_ms as usize).saturating_mul(samples_per_ms);
        let device_channel_count_for_callback = device_channel_count;
        let adaptive_resampling_enabled = enable_adaptive_resampling;
        // The live config as the callback sees it: its own copy, refreshed
        // once per callback without blocking (on contention the previous copy
        // stands until the next one), as the PipeWire callback does.
        let mut callback_cfg = initial_cfg.clone();
        // The callback reports through this queue; the drain thread logs.
        let (mut callback_log, callback_log_reader) = CallbackLog::new(module_path!());
        let callback_log_drain = CallbackLogDrain::spawn(callback_log_reader);

        // The whole output callback, on an f32 device buffer. Devices whose
        // native format is not f32 (ASIO drivers commonly expose only I32)
        // get it through a converting wrapper, see `build_stream`.
        let render = move |data: &mut [f32]| {
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
            if let Some(cfg) = live_config_for_callback.try_lock() {
                callback_cfg.clone_from(&cfg);
            }
            let is_pi_paused = callback_cfg.paused;

            // A flush that gave up leaves the rest for this end to drop.
            ring_reader.apply_requested_discard();

            // 1. Check buffer fill & Calculate Rate
            // Input-domain samples (frames * channels)
            let available_samples = ring_reader.available();
            // Raw FIFO level for any future diagnostic plot; not used in the
            // PI input below (see chunk-cancellation rationale).
            let _output_fifo_input_domain_samples_raw = output_to_input_domain_samples(
                resampler_fifo.output_len(),
                effective_resample_ratio,
            );
            // Chunk-cycle-cancelled FIFO contribution: the FIFO is a
            // deterministic sawtooth between 0 and one chunk's worth of
            // samples. Substituting its expected steady-state mean (half
            // a chunk in input domain) for the instantaneous value
            // removes the chunk-induced low-frequency oscillation from
            // `control_available` without phase lag.
            let output_fifo_input_domain_samples =
                (RESAMPLER_CHUNK_SIZE / 2).saturating_mul(channel_count as usize);
            let pending_resampler_input_samples = resampler_fifo.pending_input_samples();
            // data.len() is in device-channel domain; convert it to rendered-audio samples
            // before comparing against the renderer/ring buffer fill level.
            let callback_frames = data.len() / device_channel_count_for_callback as usize;
            let callback_audio_samples = callback_frames * channel_count as usize;
            // Ring-buffer occupancy is tracked in input-domain samples, while the
            // The device callback consumes output-domain samples after local resampling.
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
            pipeline_latency_ms_bits_clone.store(callback_midpoint_ms.to_bits(), Ordering::Relaxed);
            // The device callback dt comes from the nominal frame size of
            // the active buffer. We don't have an atomic-published dt
            // here as on the PipeWire path; the configured value is
            // accurate enough for the cutoff math.
            let callback_dt_s = if input_sample_rate > 0 {
                callback_frames as f64 / input_sample_rate as f64
            } else {
                0.021
            };
            let metrics = update_latency_metrics(
                &mut runtime_state,
                available_samples,
                output_fifo_input_domain_samples,
                pending_resampler_input_samples,
                0, // cpal backends have no output pacer stage
                callback_input_domain_samples,
                channel_count as usize,
                input_sample_rate,
                callback_midpoint_ms,
                callback_cfg.control_smoothing_cutoff_hz,
                callback_cfg.control_smoothing_order,
                callback_dt_s,
                LatencyMetricTargets {
                    measured_latency_ms_bits: &measured_latency_ms_bits_clone,
                    control_latency_ms_bits: &control_latency_ms_bits_clone,
                },
            );
            // Publish the three components of `control_available` as ms so they
            // can be plotted independently in the Studio control plot.
            {
                let to_ms = |samples: usize| -> f32 {
                    samples_to_ms(samples, channel_count as usize, input_sample_rate)
                };
                avail_input_latency_ms_bits_clone
                    .store(to_ms(available_samples).to_bits(), Ordering::Relaxed);
                output_fifo_latency_ms_bits_clone.store(
                    to_ms(output_fifo_input_domain_samples).to_bits(),
                    Ordering::Relaxed,
                );
                resampler_pending_latency_ms_bits_clone.store(
                    to_ms(pending_resampler_input_samples).to_bits(),
                    Ordering::Relaxed,
                );
            }
            let fallback_band = far_mode_band_from_latency(
                &callback_cfg,
                metrics.control_available,
                target_buffer_fill,
                samples_per_ms,
            );
            current_adaptive_band_clone.store(fallback_band, Ordering::Relaxed);
            let mut recovery_band = fallback_band;

            // Adaptive rate logic (PI Controller)
            // Adjusts the resampling ratio around the base ratio to maintain buffer level
            // Only active if adaptive resampling is enabled
            if adaptive_resampling_enabled
                && !is_pi_paused
                && runtime_state.low_recover_phase == LowRecoverPhase::Inactive
            {
                // Only adjust rate if we have started playback and have enough data
                if should_run_adaptive_servo(
                    callback_count,
                    callback_cfg.update_interval_callbacks,
                    metrics.total_available_input_domain,
                    channel_count as usize,
                ) {
                    let mut decision = run_adaptive_servo(
                        &mut runtime_state,
                        &callback_cfg,
                        metrics,
                        target_buffer_fill,
                        resample_ratio,
                        100,
                        callback_cfg.max_adjust.max(0.000_001),
                        samples_per_ms,
                        samples_per_ms_f64,
                    );

                    // Update resampler ratio
                    let clamped_ratio = clamp_ratio_for_local_resampler(
                        resample_ratio,
                        decision.step.current_ratio,
                    );
                    decision.step.current_ratio = clamped_ratio;
                    decision.step.consume_adjust = resample_ratio / clamped_ratio;
                    decision.effective_resample_ratio = clamped_ratio;
                    decision.displayed_rate_adjust =
                        paused_rate_adjust(resample_ratio, clamped_ratio);

                    if let Err(e) = resampler.set_resample_ratio(clamped_ratio, true) {
                        callback_event!(callback_log, Warn, "failed to set the resampler ratio"; e);
                    } else {
                        effective_resample_ratio = clamped_ratio;
                    }
                    current_rate_adjust_clone
                        .store(decision.displayed_rate_adjust.to_bits(), Ordering::Relaxed);
                    current_adaptive_band_clone.store(decision.adaptive_band, Ordering::Relaxed);
                    recovery_band = decision.adaptive_band;

                    if callback_count % 100 == 0 {
                        callback_event!(
                            callback_log,
                            Trace,
                            "adaptive",
                            buf = metrics.control_available,
                            target = target_buffer_fill,
                            drift = decision.step.drift,
                            ratio = decision.step.current_ratio,
                            base = resample_ratio,
                            p = decision.step.p_term,
                            i = decision.step.i_term,
                            kp = callback_cfg.kp_near,
                            ki = callback_cfg.ki,
                            max_adjust = callback_cfg.max_adjust
                        );
                    }
                }
            } else if adaptive_resampling_enabled && is_pi_paused {
                let held_consume_adjust =
                    paused_rate_adjust(resample_ratio, effective_resample_ratio);
                current_rate_adjust_clone.store(held_consume_adjust.to_bits(), Ordering::Relaxed);
                recovery_band = current_adaptive_band_clone.load(Ordering::Relaxed);
            } else {
                current_rate_adjust_clone.store(1.0f32.to_bits(), Ordering::Relaxed);
            }

            // 2. Decide far-mode recovery before consuming more input for this callback.
            // data.len() is frames * device_channel_count
            // output_fifo contains frames * channel_count
            let output_frames_needed = data.len() / device_channel_count_for_callback as usize;
            let audio_samples_needed = output_frames_needed * channel_count as usize;
            let startup_low_recover_was_active = runtime_state.startup_low_recover_active;
            let low_recover_was_active =
                runtime_state.low_recover_phase != LowRecoverPhase::Inactive;
            let far_decision: FarModeDecision = update_far_mode_state(
                &mut runtime_state,
                &callback_cfg,
                recovery_band == crate::ADAPTIVE_BAND_FAR,
                metrics.control_available,
                metrics.smoothed_control_available,
                target_buffer_fill,
                callback_input_domain_samples,
                effective_resample_ratio,
                channel_count as usize,
                input_sample_rate,
                output_sample_rate,
            );
            current_runtime_state_clone.store(
                adaptive_runtime_state_code(adaptive_runtime_state_name(
                    runtime_state.low_recover_phase,
                    far_decision.hard_recover_high,
                )),
                Ordering::Relaxed,
            );
            let mut projected_control_available = metrics.control_available;
            if far_decision.recovery_reacquire_pending && far_decision.mute_far_output {
                projected_control_available =
                    projected_control_available.saturating_sub(callback_input_domain_samples);
            } else if far_decision.hard_recover_high {
                let plan = compute_hard_recover_high_plan(
                    callback_input_domain_samples,
                    metrics.control_available,
                    target_buffer_fill,
                    effective_resample_ratio,
                    channel_count as usize,
                );
                projected_control_available =
                    projected_control_available.saturating_sub(plan.desired_consume_input_samples);
            } else if far_decision.hold_low_recover {
                let trim_input_samples = output_to_input_domain_samples(
                    far_decision.low_recover_trim_output_samples,
                    effective_resample_ratio,
                );
                let muted_consume_input_samples =
                    if far_decision.mute_far_output && far_decision.consume_while_muted {
                        callback_input_domain_samples
                    } else {
                        0
                    };
                projected_control_available = projected_control_available
                    .saturating_sub(trim_input_samples.saturating_add(muted_consume_input_samples));
            }
            store_latency_metrics_from_control_available(
                projected_control_available,
                channel_count as usize,
                input_sample_rate,
                callback_midpoint_ms,
                LatencyMetricTargets {
                    measured_latency_ms_bits: &measured_latency_ms_bits_clone,
                    control_latency_ms_bits: &control_latency_ms_bits_clone,
                },
            );
            if far_decision.hold_low_recover {
                current_rate_adjust_clone.store(1.0f32.to_bits(), Ordering::Relaxed);
                if !low_recover_was_active {
                    resampler.reset();
                    let _ = resampler.set_resample_ratio(resample_ratio, false);
                    resampler_fifo.reset();
                } else if effective_resample_ratio.to_bits() != resample_ratio.to_bits() {
                    let _ = resampler.set_resample_ratio(resample_ratio, false);
                }
                effective_resample_ratio = resample_ratio;
            }
            let startup_low_recover_finished =
                startup_low_recover_was_active && !runtime_state.startup_low_recover_active;
            if startup_low_recover_finished {
                // Drop any filter/FIFO history accumulated while muted so the first
                // audible callback starts from a clean state.
                resampler.reset();
                let _ = resampler.set_resample_ratio(effective_resample_ratio, false);
                resampler_fifo.reset();
                if far_decision.mute_far_output {
                    data.fill(0.0);
                    return;
                }
            }
            if far_decision.hold_low_recover {
                let muted_samples_to_consume =
                    if far_decision.mute_far_output && far_decision.consume_while_muted {
                        audio_samples_needed
                    } else {
                        0
                    };
                let prepared_samples = if far_decision.mute_far_output {
                    muted_samples_to_consume
                        .saturating_add(far_decision.low_recover_trim_output_samples)
                } else {
                    audio_samples_needed
                        .saturating_add(far_decision.low_recover_trim_output_samples)
                };
                if prepared_samples > 0 {
                    if let Err(e) = resampler_fifo.ensure_output_samples(
                        &mut ring_reader,
                        &mut resampler,
                        prepared_samples,
                    ) {
                        callback_event!(callback_log, Error, "resampler error"; e);
                    }
                    if far_decision.low_recover_trim_output_samples > 0 {
                        resampler_fifo
                            .discard_samples(far_decision.low_recover_trim_output_samples);
                    }
                    if muted_samples_to_consume > 0 {
                        resampler_fifo.discard_samples(muted_samples_to_consume);
                    }
                }
                if far_decision.mute_far_output {
                    data.fill(0.0);
                }
            } else {
                if let Err(e) = resampler_fifo.ensure_output_samples(
                    &mut ring_reader,
                    &mut resampler,
                    audio_samples_needed,
                ) {
                    callback_event!(callback_log, Error, "resampler error"; e);
                }
            }

            // 3. Fill the device callback buffer from FIFO
            if far_decision.hard_recover_high {
                let plan = compute_hard_recover_high_plan(
                    callback_input_domain_samples,
                    metrics.control_available,
                    target_buffer_fill,
                    effective_resample_ratio,
                    channel_count as usize,
                );
                if let Err(e) = resampler_fifo.ensure_output_samples(
                    &mut ring_reader,
                    &mut resampler,
                    plan.desired_consume_output_samples,
                ) {
                    callback_event!(callback_log, Error, "resampler error"; e);
                }
                resampler_fifo.discard_samples(plan.desired_consume_output_samples);
                data.fill(0.0);
            } else if far_decision.hold_low_recover && far_decision.mute_far_output {
                data.fill(0.0);
            } else if resampler_fifo.output_len() >= audio_samples_needed {
                // We have enough data
                // Map audio channels to device channels
                // If device has more channels than audio, extra channels are zeroed
                // Straight from the FIFO into the device frames, extra
                // device channels zeroed — no per-callback allocation.
                let moved = resampler_fifo.drain_frames_into(
                    &mut data[..output_frames_needed * device_channel_count_for_callback as usize],
                    channel_count as usize,
                    device_channel_count_for_callback as usize,
                );
                zero_pad_tail(data, moved * device_channel_count_for_callback as usize);
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
                    &mut callback_log,
                    "output underrun: zero-padding the remainder",
                    resampler_fifo.output_len(),
                    audio_samples_needed,
                );
                // Fill what we have, silence the rest. Whole frames, mapped
                // onto the device's channel layout like the branch above:
                // a flat copy would shift every channel when the device
                // is wider than the audio.
                let moved = resampler_fifo.drain_frames_into(
                    data,
                    channel_count as usize,
                    device_channel_count_for_callback as usize,
                );
                zero_pad_tail(data, moved * device_channel_count_for_callback as usize);
                postprocess_interleaved_output(
                    data,
                    device_channel_count_for_callback as usize,
                    far_decision.mute_far_output,
                    &mut runtime_state,
                );
            }
        };
        // Room for the largest callback a driver is expected to hand over, so
        // the converting wrapper never grows its scratch in steady state.
        let scratch_capacity = MAX_CALLBACK_FRAMES * device_channel_count as usize;
        let stream = match sample_format {
            cpal::SampleFormat::F32 => {
                build_stream::<f32, _>(&device, &config, render, scratch_capacity)?
            }
            cpal::SampleFormat::I32 => {
                build_stream::<i32, _>(&device, &config, render, scratch_capacity)?
            }
            cpal::SampleFormat::I16 => {
                build_stream::<i16, _>(&device, &config, render, scratch_capacity)?
            }
            other => {
                return Err(anyhow!(
                    "{BACKEND} device sample format {other:?} is not supported (f32, i32, i16)"
                ));
            }
        };

        stream.play()?;
        ready_clone.store(true, Ordering::Relaxed);

        Ok(Self {
            sample_buffer,
            ring,
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
            current_runtime_state,
            measured_latency_ms_bits,
            control_latency_ms_bits,
            pipeline_latency_ms_bits,
            avail_input_latency_ms_bits,
            output_fifo_latency_ms_bits,
            resampler_pending_latency_ms_bits,
            live_adaptive_config: live_config,
            backpressure_disabled,
            reset_ratio_requested,
            _callback_log_drain: callback_log_drain,
            _stream: Some(stream),
        })
    }

    pub fn write_samples(&mut self, samples: &[f32]) -> Result<()> {
        // Back-pressure disabled (diagnostic): never block the renderer; push
        // what fits below the threshold and drop the overflow — the same
        // policy as the PipeWire writer.
        let report = if self.backpressure_disabled.load(Ordering::Relaxed) {
            push_samples_drop_overflow(&mut self.sample_buffer, samples, self.max_buffer_fill)
        } else {
            push_samples_with_backpressure(
                &mut self.sample_buffer,
                samples,
                self.max_buffer_fill,
                10,
                200,
            )
        };
        if report.timed_out {
            log::warn!("Buffer drain timeout");
        }
        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        let _ = flush_ring_buffer(
            &self.ring,
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

    pub fn adaptive_runtime_state(&self) -> Option<&'static str> {
        adaptive_runtime_state_name_from_code(self.current_runtime_state.load(Ordering::Relaxed))
    }

    pub fn total_audio_delay_ms(&self) -> f32 {
        self.target_control_latency_ms()
            + f32::from_bits(self.pipeline_latency_ms_bits.load(Ordering::Relaxed))
    }

    pub fn target_control_latency_ms(&self) -> f32 {
        interleaved_samples_to_ms(
            self.target_buffer_fill,
            self.channel_count as usize,
            self.input_sample_rate,
        )
    }

    pub fn measured_audio_delay_ms(&self) -> f32 {
        f32::from_bits(self.measured_latency_ms_bits.load(Ordering::Relaxed))
    }

    pub fn control_audio_delay_ms(&self) -> f32 {
        f32::from_bits(self.control_latency_ms_bits.load(Ordering::Relaxed))
    }

    /// EMA-smoothed control latency. The cpal backends (ASIO, CoreAudio) do not
    /// yet maintain a separate smoothed metric, so this falls back to the raw
    /// control latency.
    pub fn smoothed_control_audio_delay_ms(&self) -> f32 {
        self.control_audio_delay_ms()
    }

    /// Ring-buffer occupancy converted to ms (first component of `control_available`).
    pub fn avail_input_audio_delay_ms(&self) -> f32 {
        f32::from_bits(self.avail_input_latency_ms_bits.load(Ordering::Relaxed))
    }

    /// Resampler output FIFO content converted back to input-domain ms
    /// (second component of `control_available`).
    pub fn output_fifo_audio_delay_ms(&self) -> f32 {
        f32::from_bits(self.output_fifo_latency_ms_bits.load(Ordering::Relaxed))
    }

    /// Local resampler pending input samples expressed as ms
    /// (third component of `control_available`).
    pub fn resampler_pending_audio_delay_ms(&self) -> f32 {
        f32::from_bits(
            self.resampler_pending_latency_ms_bits
                .load(Ordering::Relaxed),
        )
    }

    /// Signal the audio thread to snap the resampling ratio back to base and reset the integrator.
    pub fn request_ratio_reset(&self) {
        self.reset_ratio_requested.store(true, Ordering::Relaxed);
    }

    /// Update adaptive resampling tuning parameters without restarting the audio stream.
    pub fn update_adaptive_config(&self, config: AdaptiveResamplingConfig) {
        self.backpressure_disabled
            .store(config.disable_backpressure, Ordering::Relaxed);
        warn_if_output_pacing_requested(&config);
        *self.live_adaptive_config.lock() = config;
    }
}
