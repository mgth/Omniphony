#![cfg(target_os = "windows")]

use anyhow::{Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam::queue::ArrayQueue;
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering},
};
use std::thread;
use std::time::Duration;

use crate::{
    ADAPTIVE_BAND_HARD, AdaptiveControllerState, AdaptiveResamplingConfig, adaptive_band_name,
    apply_ema, compute_adaptive_step,
};

// Buffer size: 4 seconds of audio at 48kHz, 16 channels
const BUFFER_SIZE: usize = 48000 * 16 * 4;

// Adaptive rate matching constants (time-domain targets).
const MIN_BUFFER_MS: u32 = 25;
const DEFAULT_TARGET_BUFFER_MS: u32 = 220;
const MAX_BUFFER_MS: u32 = 250;
const MAX_INTEGRAL_TERM: f64 = 0.0002;

// Rubato constants
const RESAMPLER_CHUNK_SIZE: usize = 1024; // Input chunk size for resampler

pub struct AsioWriter {
    sample_buffer: Arc<ArrayQueue<f32>>,
    input_sample_rate: u32,
    _output_sample_rate: u32,
    channel_count: u32,        // Number of audio channels we're producing
    device_channel_count: u32, // Number of channels the ASIO device expects
    stream_ready: Arc<AtomicBool>,
    enable_adaptive_resampling: bool, // Enable PI controller for buffer stability
    max_buffer_fill: usize,
    target_buffer_fill: usize,
    current_rate_adjust: Arc<AtomicU32>,
    current_adaptive_band: Arc<AtomicU8>,
    measured_latency_ms_bits: Arc<AtomicU32>,
    control_latency_ms_bits: Arc<AtomicU32>,
    flush_requested: Arc<AtomicBool>,
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
        let current_rate_adjust = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let current_rate_adjust_clone = current_rate_adjust.clone();
        let current_adaptive_band = Arc::new(AtomicU8::new(0));
        let current_adaptive_band_clone = current_adaptive_band.clone();
        let measured_latency_ms_bits = Arc::new(AtomicU32::new(0u32));
        let measured_latency_ms_bits_clone = measured_latency_ms_bits.clone();
        let control_latency_ms_bits = Arc::new(AtomicU32::new(0u32));
        let control_latency_ms_bits_clone = control_latency_ms_bits.clone();
        let flush_requested = Arc::new(AtomicBool::new(false));
        let flush_requested_clone = flush_requested.clone();

        // Ring-buffer thresholds in INPUT-domain samples (same domain as buffer_clone.len()).
        let samples_per_ms =
            (input_sample_rate as usize).saturating_mul(channel_count as usize) / 1000;
        let target_buffer_ms = if target_latency_ms == 0 {
            DEFAULT_TARGET_BUFFER_MS
        } else {
            target_latency_ms
        };
        let max_buffer_ms = MAX_BUFFER_MS.max(target_buffer_ms.saturating_mul(2));
        let min_buffer_fill = (samples_per_ms * MIN_BUFFER_MS as usize).max(channel_count as usize);
        let target_buffer_fill =
            (samples_per_ms * target_buffer_ms as usize).max(min_buffer_fill);
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

        // Calculate max ratio for adaptive adjustments
        // Allow small adjustments around the base resample ratio
        // Rubato expects a relative ratio bound (>= 1.0), not an absolute ratio.
        let max_resample_ratio_relative = 1.0
            + adaptive_config
                .max_adjust
                .max(adaptive_config.max_adjust_far)
                .max(0.000001);
        let max_resample_ratio_abs = resample_ratio * max_resample_ratio_relative;

        log::debug!(
            "Initializing resampler: base_ratio={:.4}, max_ratio={:.4}, chunk_size={}",
            resample_ratio,
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

        // Intermediate buffers
        // Stores input for resampler (planar)
        let mut resampler_input: Vec<Vec<f32>> =
            vec![vec![0.0; RESAMPLER_CHUNK_SIZE]; channel_count as usize];
        // Stores accumulated input samples count
        let mut input_frames_collected = 0;

        // FIFO for resampled output waiting to be consumed by ASIO callback
        // Interleaved f32 samples
        let mut output_fifo: Vec<f32> =
            Vec::with_capacity(RESAMPLER_CHUNK_SIZE * channel_count as usize * 4);

        // Control loop state
        let mut controller_state = AdaptiveControllerState::default();
        let mut callback_count = 0u64;
        let mut playback_started = false;
        let mut underrun_warned = false;
        let near_far_threshold_samples =
            (adaptive_config.near_far_threshold_ms as usize).saturating_mul(samples_per_ms);
        let measurement_smoothing_alpha =
            adaptive_config.measurement_smoothing_alpha.clamp(0.0, 1.0);
        let hard_correction_threshold_samples =
            (adaptive_config.hard_correction_threshold_ms as usize).saturating_mul(samples_per_ms);
        let hard_correction_release_margin = hard_correction_threshold_samples / 2;
        let hard_correction_max_step = hard_correction_threshold_samples / 2;
        let mut hard_correction_mode: i8 = 0;

        let device_channel_count_for_callback = device_channel_count;
        let adaptive_resampling_enabled = enable_adaptive_resampling;

        let err_fn = |err| log::error!("an error occurred on stream: {}", err);

        let stream = device.build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                callback_count += 1;

                if flush_requested_clone.swap(false, Ordering::Relaxed) {
                    while buffer_clone.pop().is_some() {}
                    output_fifo.clear();
                    input_frames_collected = 0;
                    controller_state = AdaptiveControllerState::default();
                    current_rate_adjust_clone.store(1.0f32.to_bits(), Ordering::Relaxed);
                    current_adaptive_band_clone.store(0, Ordering::Relaxed);
                    measured_latency_ms_bits_clone.store(0u32, Ordering::Relaxed);
                    control_latency_ms_bits_clone.store(0u32, Ordering::Relaxed);
                    playback_started = false;
                    underrun_warned = false;
                }

                // 1. Check buffer fill & Calculate Rate
                let available_samples = buffer_clone.len(); // Input-domain samples (frames * channels)
                let output_fifo_input_domain_samples = if resample_ratio > 0.0 {
                    ((output_fifo.len() as f64) / resample_ratio).round() as usize
                } else {
                    output_fifo.len()
                };
                let total_available_input_domain =
                    available_samples.saturating_add(output_fifo_input_domain_samples);
                // data.len() is in device-channel domain; convert it to rendered-audio samples
                // before comparing against the renderer/ring buffer fill level.
                let callback_frames = data.len() / device_channel_count_for_callback as usize;
                let callback_audio_samples = callback_frames * channel_count as usize;
                // Ring-buffer occupancy is tracked in input-domain samples, while the
                // ASIO callback consumes output-domain samples after local resampling.
                // Convert the callback midpoint estimate back to input-domain samples
                // before comparing against the input-domain fill level.
                let callback_input_domain_samples = if resample_ratio > 0.0 {
                    ((callback_audio_samples as f64) / resample_ratio).round() as usize
                } else {
                    callback_audio_samples
                };
                let control_available = total_available_input_domain
                    .saturating_sub(callback_input_domain_samples / 2);
                let smoothed_control_available = apply_ema(
                    &mut controller_state.smoothed_control_available,
                    control_available as f64,
                    measurement_smoothing_alpha,
                )
                .max(0.0)
                .round() as usize;
                let smoothed_total_available = apply_ema(
                    &mut controller_state.smoothed_total_available,
                    total_available_input_domain as f64,
                    measurement_smoothing_alpha,
                )
                .max(0.0)
                .round() as usize;
                let measured_latency_ms = (smoothed_total_available as f32
                    / channel_count as f32
                    / input_sample_rate as f32)
                    * 1000.0;
                let control_latency_ms = (smoothed_control_available as f32
                    / channel_count as f32
                    / input_sample_rate as f32)
                    * 1000.0;
                measured_latency_ms_bits_clone
                    .store(measured_latency_ms.to_bits(), Ordering::Relaxed);
                control_latency_ms_bits_clone
                    .store(control_latency_ms.to_bits(), Ordering::Relaxed);

                let mut hard_zero_fill = false;
                if adaptive_resampling_enabled && hard_correction_threshold_samples > 0 {
                    if hard_correction_mode < 0 {
                        if smoothed_control_available.saturating_add(hard_correction_release_margin)
                            >= target_buffer_fill
                        {
                            hard_correction_mode = 0;
                        } else {
                            controller_state.accumulated_drift = 0.0;
                            current_rate_adjust_clone.store(1.0f32.to_bits(), Ordering::Relaxed);
                            current_adaptive_band_clone.store(ADAPTIVE_BAND_HARD, Ordering::Relaxed);
                            hard_zero_fill = true;
                        }
                    } else if hard_correction_mode > 0 {
                        let desired_keep =
                            target_buffer_fill.saturating_add(hard_correction_release_margin);
                        if smoothed_total_available <= desired_keep {
                            hard_correction_mode = 0;
                        } else {
                            let mut to_drop = smoothed_total_available.saturating_sub(desired_keep);
                            to_drop = to_drop.min(hard_correction_max_step.max(channel_count as usize));
                            let fifo_drop = to_drop.min(output_fifo.len());
                            if fifo_drop > 0 {
                                output_fifo.drain(0..fifo_drop);
                                to_drop = to_drop.saturating_sub(fifo_drop);
                            }
                            let ring_drop = (to_drop / channel_count as usize) * channel_count as usize;
                            for _ in 0..ring_drop {
                                let _ = buffer_clone.pop();
                            }
                            controller_state.accumulated_drift = 0.0;
                            current_adaptive_band_clone.store(ADAPTIVE_BAND_HARD, Ordering::Relaxed);
                        }
                    } else if smoothed_control_available
                        .saturating_add(hard_correction_threshold_samples)
                        < target_buffer_fill
                    {
                        hard_correction_mode = -1;
                        controller_state.accumulated_drift = 0.0;
                        current_rate_adjust_clone.store(1.0f32.to_bits(), Ordering::Relaxed);
                        current_adaptive_band_clone.store(ADAPTIVE_BAND_HARD, Ordering::Relaxed);
                        hard_zero_fill = true;
                    } else if smoothed_total_available
                        > target_buffer_fill.saturating_add(hard_correction_threshold_samples)
                    {
                        hard_correction_mode = 1;
                        let mut to_drop = smoothed_total_available.saturating_sub(
                            target_buffer_fill.saturating_add(hard_correction_release_margin),
                        );
                        to_drop = to_drop.min(hard_correction_max_step.max(channel_count as usize));
                        let fifo_drop = to_drop.min(output_fifo.len());
                        if fifo_drop > 0 {
                            output_fifo.drain(0..fifo_drop);
                            to_drop = to_drop.saturating_sub(fifo_drop);
                        }
                        let ring_drop = (to_drop / channel_count as usize) * channel_count as usize;
                        for _ in 0..ring_drop {
                            let _ = buffer_clone.pop();
                        }
                        controller_state.accumulated_drift = 0.0;
                        current_adaptive_band_clone.store(ADAPTIVE_BAND_HARD, Ordering::Relaxed);
                    }
                }

                if hard_zero_fill {
                    data.fill(0.0);
                    return;
                }

                // Adaptive rate logic (PI Controller)
                // Adjusts the resampling ratio around the base ratio to maintain buffer level
                // Only active if adaptive resampling is enabled
                if adaptive_resampling_enabled {
                    // Only adjust rate if we have started playback and have enough data
                    if playback_started && callback_count % 10 == 0 {
                        let step = compute_adaptive_step(
                            &mut controller_state,
                            &adaptive_config,
                            smoothed_control_available,
                            target_buffer_fill,
                            near_far_threshold_samples,
                            resample_ratio,
                            100,
                            MAX_INTEGRAL_TERM,
                        );

                        // Update resampler ratio
                        if let Err(e) = resampler.set_resample_ratio(step.current_ratio, true) {
                            log::warn!("Failed to set resampler ratio: {}", e);
                        }
                        current_rate_adjust_clone
                            .store((step.consume_adjust as f32).to_bits(), Ordering::Relaxed);
                        current_adaptive_band_clone.store(step.band, Ordering::Relaxed);

                        if callback_count % 100 == 0 {
                            log::debug!(
                                "ASIO Adaptive: buf={}/{} drift={} ratio={:.6} (base={:.2} P={:.6} I={:.6})",
                                smoothed_control_available,
                                target_buffer_fill,
                                step.drift,
                                step.current_ratio,
                                resample_ratio,
                                step.p_term,
                                step.i_term
                            );
                        }
                    }
                } else {
                    current_rate_adjust_clone.store(1.0f32.to_bits(), Ordering::Relaxed);
                    current_adaptive_band_clone.store(0, Ordering::Relaxed);
                }

                // 2. Feed Resampler until we have enough data in output_fifo for this callback
                // data.len() is frames * device_channel_count
                // output_fifo contains frames * channel_count
                let output_frames_needed = data.len() / device_channel_count_for_callback as usize;
                let audio_samples_needed = output_frames_needed * channel_count as usize;

                while output_fifo.len() < audio_samples_needed {
                    // We need to run the resampler
                    // Do we have enough input for a chunk?
                    // 2.1 Fill input buffer from ringbuffer
                    while input_frames_collected < RESAMPLER_CHUNK_SIZE {
                        // Try to pop one frame (all channels)
                        let mut frame_complete = true;

                        // Check if we have a full frame available in ringbuffer
                        if buffer_clone.len() >= channel_count as usize {
                            for ch in 0..channel_count as usize {
                                if let Some(sample_f32) = buffer_clone.pop() {
                                    // Already in f32 format [-1.0, 1.0]
                                    resampler_input[ch][input_frames_collected] = sample_f32;
                                } else {
                                    frame_complete = false;
                                    break;
                                }
                            }
                        } else {
                            frame_complete = false;
                        }

                        if frame_complete {
                            input_frames_collected += 1;
                        } else {
                            // Not enough data in input buffer to fill a resampler chunk
                            // If playback hasn't started, wait.
                            // If it has, this is an underrun condition.
                            break;
                        }
                    }

                    // 2.2 Run Resampler if input is full
                    if input_frames_collected == RESAMPLER_CHUNK_SIZE {
                        match resampler.process(&resampler_input, None) {
                            Ok(output_planar) => {
                                // Interleave and push to FIFO
                                let output_frames = output_planar[0].len();
                                for i in 0..output_frames {
                                    for ch in 0..channel_count as usize {
                                        output_fifo.push(output_planar[ch][i]);
                                    }
                                }
                                // Reset input counter
                                input_frames_collected = 0;
                            },
                            Err(e) => {
                                log::error!("Resampler error: {}", e);
                                break;
                            }
                        }
                    } else {
                        // Cannot fill resampler chunk, underrun
                        break;
                    }
                }

                // 3. Fill ASIO callback buffer from FIFO
                if output_fifo.len() >= audio_samples_needed {
                    // We have enough data
                        if !playback_started {
                        if available_samples >= min_buffer_fill {
                            playback_started = true;
                            log::info!("ASIO playback started");
                        } else {
                            // Still filling, silence
                            data.fill(0.0);
                            return;
                        }
                    }

                    // Map audio channels to device channels
                    // If device has more channels than audio, extra channels are zeroed
                    data.fill(0.0); // Zero all channels first

                    let audio_iter = output_fifo.drain(0..audio_samples_needed);
                    let audio_samples: Vec<f32> = audio_iter.collect();

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

                } else {
                    // Underrun
                    if playback_started && !underrun_warned {
                        log::warn!(
                            "ASIO Underrun! output_fifo={}, needed={} (frames={})",
                            output_fifo.len(),
                            audio_samples_needed,
                            output_frames_needed
                        );
                        underrun_warned = true;
                    }
                    // Fill what we have, silence rest
                    let available = output_fifo.len();
                    let drain_iter = output_fifo.drain(..);
                    for (dest, src) in data.iter_mut().zip(drain_iter) {
                        *dest = src;
                    }
                    // Silence remaining
                    for sample in data.iter_mut().skip(available) {
                        *sample = 0.0;
                    }
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
            device_channel_count: device_channel_count as u32,
            stream_ready,
            enable_adaptive_resampling,
            max_buffer_fill,
            target_buffer_fill,
            current_rate_adjust,
            current_adaptive_band,
            measured_latency_ms_bits,
            control_latency_ms_bits,
            flush_requested,
            _stream: Some(stream),
        })
    }

    pub fn write_samples(&mut self, samples: &[f32]) -> Result<()> {
        // Same logic as PipewireWriter
        let mut sample_idx = 0;
        let mut wait_count = 0;
        let _last_log_time = std::time::Instant::now();

        while sample_idx < samples.len() {
            let buffer_level = self.sample_buffer.len();

            if buffer_level >= self.max_buffer_fill {
                if wait_count == 0 {
                    // log::debug!("Buffer full, waiting...");
                }
                wait_count += 1;
                thread::sleep(Duration::from_millis(10));

                if wait_count > 200 {
                    log::warn!("Buffer drain timeout");
                    break;
                }
                continue;
            }

            while sample_idx < samples.len() && self.sample_buffer.len() < self.max_buffer_fill {
                if self.sample_buffer.push(samples[sample_idx]).is_ok() {
                    sample_idx += 1;
                } else {
                    break;
                }
            }
        }
        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        let timeout = Duration::from_secs(5);
        let start = std::time::Instant::now();
        while !self.sample_buffer.is_empty() {
            if start.elapsed() > timeout {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
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
        (self.target_buffer_fill as f32
            / self.channel_count as f32
            / self.input_sample_rate as f32)
            * 1000.0
    }

    pub fn measured_audio_delay_ms(&self) -> f32 {
        f32::from_bits(self.measured_latency_ms_bits.load(Ordering::Relaxed))
    }

    pub fn control_audio_delay_ms(&self) -> f32 {
        f32::from_bits(self.control_latency_ms_bits.load(Ordering::Relaxed))
    }

    pub fn request_flush(&self) {
        self.flush_requested.store(true, Ordering::Relaxed);
    }
}
