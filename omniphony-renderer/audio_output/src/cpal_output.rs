#![cfg(any(target_os = "windows", target_os = "macos"))]
//! cpal-backed realtime output writer shared by the Windows (ASIO, with a
//! WASAPI fallback) and macOS (CoreAudio) backends. The platforms differ only
//! in which cpal host they open; the ring-buffer, local resampler and
//! adaptive-rate servo are identical.

use anyhow::{Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, Ordering},
};
use std::time::Duration;

use crate::callback_core::{CallbackContext, CallbackShared, OutputCallbackCore};
use crate::callback_log::CallbackLogDrain;
use crate::host_choice::too_few_channels_message;
#[cfg(target_os = "windows")]
use crate::host_choice::{HostChoice, choose_host};
use crate::output_telemetry::interleaved_samples_to_ms;
use crate::{
    AdaptiveResamplingConfig, local_resampler_ratio_bounds,
    resampler_fifo::{RESAMPLER_CHUNK_SIZE, new_output_resampler},
    ring_buffer_io::{
        RingMonitor, RingWriter, flush_ring_buffer, push_samples_drop_overflow,
        push_samples_with_backpressure, sample_ring,
    },
};

// Adaptive rate matching constants (time-domain targets).
const MIN_BUFFER_MS: u32 = 25;
const DEFAULT_TARGET_BUFFER_MS: u32 = 220;
const MAX_BUFFER_MS: u32 = 250;
/// Drift, in samples, the servo leaves alone on this backend.
const SERVO_DEADBAND_SAMPLES: usize = 100;

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
fn warn_if_output_pacing_requested(backend: &str, config: &AdaptiveResamplingConfig) {
    if config.use_output_pacing {
        log::warn!("{backend}: output pacing is only implemented on PipeWire; ignored here");
    }
}

/// The cpal host realtime output opened, and how logs, errors and Studio name
/// it.
struct OutputHost {
    host: cpal::Host,
    /// Short host name for logs and errors: `ASIO`, `WASAPI` or `CoreAudio`.
    name: &'static str,
    /// What Studio shows as the output host: the name, plus on Windows why
    /// output is not on ASIO.
    label: &'static str,
    /// WASAPI shared mode: the Windows mixer fixes the channel count.
    shared_mode: bool,
}

/// The output device names `host` lists, or `None` when it cannot list them.
#[cfg(target_os = "windows")]
fn output_device_names(host: &cpal::Host) -> Option<Vec<String>> {
    match host.output_devices() {
        Ok(devices) => Some(devices.filter_map(|d| d.name().ok()).collect()),
        Err(e) => {
            log::debug!("{:?} host cannot list output devices: {e}", host.id());
            None
        }
    }
}

/// cpal's ASIO host, or `None` (logged) when it cannot be opened.
#[cfg(target_os = "windows")]
fn asio_host() -> Option<cpal::Host> {
    cpal::host_from_id(cpal::HostId::Asio)
        .map_err(|e| log::debug!("ASIO host not available: {e:?}"))
        .ok()
}

#[cfg(target_os = "windows")]
fn wasapi_host() -> Result<cpal::Host> {
    cpal::host_from_id(cpal::HostId::Wasapi)
        .map_err(|e| anyhow!("WASAPI host not available either: {e:?}"))
}

/// The ASIO host when it has an output device, else WASAPI in shared mode;
/// see [`choose_host`]. `requested` is the configured device name: one only
/// WASAPI lists selects WASAPI.
#[cfg(target_os = "windows")]
fn output_host(requested: Option<&str>) -> Result<OutputHost> {
    let asio = asio_host();
    let asio_devices = asio.as_ref().and_then(output_device_names);
    let choice = choose_host(asio_devices.as_deref(), requested, || {
        cpal::host_from_id(cpal::HostId::Wasapi)
            .ok()
            .as_ref()
            .and_then(output_device_names)
            .unwrap_or_default()
    });
    let host = match (choice, asio) {
        (HostChoice::Asio, Some(host)) => host,
        (HostChoice::Asio, None) => return Err(anyhow!("ASIO host not available")),
        (HostChoice::WasapiFallback(reason), _) => {
            log::warn!(
                "ASIO output not used: {}; falling back to WASAPI (shared mode)",
                reason.describe()
            );
            wasapi_host()?
        }
    };
    Ok(OutputHost {
        host,
        name: choice.host_name(),
        label: choice.label(),
        shared_mode: matches!(choice, HostChoice::WasapiFallback(_)),
    })
}

/// The default (CoreAudio) host.
#[cfg(target_os = "macos")]
fn output_host(_requested: Option<&str>) -> Result<OutputHost> {
    Ok(OutputHost {
        host: cpal::default_host(),
        name: "CoreAudio",
        label: "CoreAudio",
        shared_mode: false,
    })
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
    /// What the device callback reads and publishes: the live config, the
    /// servo's rate adjust, band and state, and every telemetry atomic.
    shared: CallbackShared,
    /// Half the device callback, in ms (`f32` bits): the latency between the
    /// buffer this side sees and the device's playback point.
    pipeline_latency_ms_bits: Arc<AtomicU32>,
    /// `AdaptiveResamplingConfig::disable_backpressure`, mirrored so the
    /// writer reads it without taking the config lock.
    backpressure_disabled: Arc<AtomicBool>,
    /// Short name of the host the stream runs on (`ASIO`, `WASAPI`, `CoreAudio`).
    host_name: &'static str,
    /// The host as Studio shows it, with the fallback reason on Windows.
    host_label: &'static str,
    // We keep the stream alive by holding it here, though cpal streams run in background threads
    _stream: Option<cpal::Stream>,
    /// Logs what the device callback queues. Declared after the stream, so it
    /// is dropped after it (fields drop in order) and logs the last events.
    _callback_log_drain: CallbackLogDrain,
}

/// The output devices of the host realtime output opens by default, with
/// that host's label: on Windows the ASIO devices, or the WASAPI ones (and a
/// label naming the fallback) when ASIO has none.
#[cfg(target_os = "windows")]
pub fn list_output_host_devices() -> Result<(&'static str, Vec<String>)> {
    let asio_devices = asio_host().as_ref().and_then(output_device_names);
    match choose_host(asio_devices.as_deref(), None, Vec::new) {
        choice @ HostChoice::Asio => Ok((choice.label(), asio_devices.unwrap_or_default())),
        choice @ HostChoice::WasapiFallback(reason) => {
            log::info!(
                "Listing WASAPI output devices: {}; output falls back to WASAPI (shared mode)",
                reason.describe()
            );
            let devices = output_device_names(&wasapi_host()?).unwrap_or_default();
            Ok((choice.label(), devices))
        }
    }
}

/// The CoreAudio output devices, with the host's label.
#[cfg(target_os = "macos")]
pub fn list_output_host_devices() -> Result<(&'static str, Vec<String>)> {
    let opened = output_host(None)?;
    let devices: Vec<String> = opened
        .host
        .output_devices()?
        .filter_map(|d| d.name().ok())
        .collect();
    Ok((opened.label, devices))
}

/// Get a list of available output device names for the host realtime output
/// opens by default (see [`list_output_host_devices`]).
pub fn list_output_devices() -> Result<Vec<String>> {
    list_output_host_devices().map(|(_, devices)| devices)
}

impl CpalWriter {
    pub fn list_output_devices() -> Result<()> {
        let (host, devices) = list_output_host_devices()?;
        println!("Available {host} Devices:");

        for (i, device_name) in devices.iter().enumerate() {
            println!("  {}: {}", i, device_name);
        }

        Ok(())
    }

    /// The host this writer plays through, as Studio shows it: `ASIO`,
    /// `CoreAudio`, or `WASAPI (fallback: …)` naming why ASIO was not used.
    pub fn output_host_label(&self) -> &'static str {
        self.host_label
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

        let stream_ready = Arc::new(AtomicBool::new(false));
        let ready_clone = stream_ready.clone();
        let pipeline_latency_ms_bits = Arc::new(AtomicU32::new(0u32));
        let pipeline_latency_for_callback = pipeline_latency_ms_bits.clone();
        // Ring-buffer thresholds in INPUT-domain samples (same domain as ring_reader.available()).
        let samples_per_ms =
            (input_sample_rate as usize).saturating_mul(channel_count as usize) / 1000;
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

        // The ring holds what the back-pressure threshold lets in, and no
        // more. A change of latency rebuilds the writer, and the ring with it.
        // The reading end moves into the callback, its one consumer.
        let channels = (channel_count as usize).max(1);
        let (sample_buffer, ring_reader) =
            sample_ring(max_buffer_fill.div_ceil(channels), channels);
        let ring = sample_buffer.monitor();

        // Open the platform's cpal output host: ASIO on Windows, WASAPI when
        // ASIO cannot serve; CoreAudio on macOS. Chosen once, here.
        let OutputHost {
            host,
            name: backend,
            label: host_label,
            shared_mode,
        } = output_host(output_device.as_deref())?;

        log::info!("Output host: {host_label}");

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
                    "{backend} output device '{target_name}' not found; run the platform list-output-devices command to see available devices.",
                )
            })?
        } else {
            host.default_output_device()
                .ok_or_else(|| anyhow!("No default {backend} output device found"))?
        };

        log::info!(
            "Using {backend} device: {}",
            device.name().unwrap_or_default()
        );

        // Find a supported configuration that has at least the required channels
        let supported_configs: Vec<_> = device.supported_output_configs()?.collect();

        log::info!(
            "Looking for {backend} config supporting {} channels at {} Hz",
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

        // A device with fewer channels than the output is refused, never
        // played with channels dropped. WASAPI shared mode in particular
        // offers only the Windows mixer's count (often 2 or 8).
        if let Some(offered) = supported_configs.iter().map(|c| c.channels()).max()
            && u32::from(offered) < channel_count
        {
            return Err(anyhow!(too_few_channels_message(
                backend,
                shared_mode,
                &device.name().unwrap_or_default(),
                offered,
                channel_count,
            )));
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
                    "{backend} device does not support {} channels at {} Hz. Available configs: {:?}",
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
                "{backend} Config: {} Hz (resampling from {} Hz, ratio {:.3}x), {} device channels (using {} for output)",
                output_sample_rate,
                input_sample_rate,
                resample_ratio,
                device_channel_count,
                channel_count
            );
        } else {
            log::info!(
                "{backend} Config: {} Hz, {} device channels (using {} for output)",
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
            "{backend} buffer thresholds ({}ch @ {}Hz input): min={} target={} max={} samples",
            channel_count,
            input_sample_rate,
            min_buffer_fill,
            target_buffer_fill,
            max_buffer_fill
        );

        // The local resampler (high quality sinc) runs at the base ratio
        // (e.g. 2.0 for 48 kHz -> 96 kHz); the servo moves it a little around
        // that to hold the ring on target.
        let (min_resample_ratio_abs, max_resample_ratio_abs) =
            local_resampler_ratio_bounds(resample_ratio);
        log::debug!(
            "Initializing resampler: base_ratio={:.4}, min_ratio={:.4}, max_ratio={:.4}, chunk_size={}",
            resample_ratio,
            min_resample_ratio_abs,
            max_resample_ratio_abs,
            RESAMPLER_CHUNK_SIZE
        );
        let resampler = new_output_resampler(resample_ratio, channel_count as usize)
            .map_err(|e| anyhow!("Failed to create resampler: {:?}", e))?;

        let backpressure_disabled = Arc::new(AtomicBool::new(adaptive_config.disable_backpressure));
        warn_if_output_pacing_requested(backend, &adaptive_config);
        let initial_config = adaptive_config.clone();
        let shared = CallbackShared::new(adaptive_config);
        let (mut callback_core, callback_log_reader) = OutputCallbackCore::new(
            CallbackContext {
                channel_count: channel_count as usize,
                dest_channels: device_channel_count as usize,
                input_sample_rate,
                output_sample_rate,
                target_buffer_fill,
                servo_deadband_samples: SERVO_DEADBAND_SAMPLES,
                adaptive_resampling: enable_adaptive_resampling,
                // The output pacer and the pre-bridge clock exist on PipeWire only.
                pacer: None,
                input_clock_us: None,
            },
            shared.clone(),
            ring_reader,
            Some(resampler),
            initial_config,
            module_path!(),
        );
        // The callback reports through this queue; the drain thread logs.
        let callback_log_drain = CallbackLogDrain::spawn(callback_log_reader);
        let device_frame_width = device_channel_count as usize;

        // The whole output callback, on an f32 device buffer. Devices whose
        // native format is not f32 (ASIO drivers commonly expose only I32)
        // get it through a converting wrapper, see `build_stream`.
        let render = move |data: &mut [f32]| {
            // The device plays this buffer over its length: on average, half
            // of it lies between what the ring holds and the playback point.
            let frames = data.len() / device_frame_width;
            let callback_midpoint_ms = frames as f32 / output_sample_rate as f32 * 500.0;
            pipeline_latency_for_callback.store(callback_midpoint_ms.to_bits(), Ordering::Relaxed);
            callback_core.process(data, callback_midpoint_ms);
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
                    "{backend} device sample format {other:?} is not supported (f32, i32, i16)"
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
            shared,
            pipeline_latency_ms_bits,
            backpressure_disabled,
            host_name: backend,
            host_label,
            _callback_log_drain: callback_log_drain,
            _stream: Some(stream),
        })
    }

    pub fn write_samples(&mut self, samples: &[f32]) -> Result<()> {
        // What was committed to the pipeline, for the cumulative-flow metric.
        self.shared
            .telemetry
            .cumulative_written_input_samples
            .fetch_add(samples.len() as u64, Ordering::Relaxed);
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
        self.enable_adaptive_resampling
            .then(|| self.shared.rate_adjust())
    }

    pub fn adaptive_band(&self) -> Option<&'static str> {
        self.shared.adaptive_band()
    }

    pub fn adaptive_runtime_state(&self) -> Option<&'static str> {
        self.shared.adaptive_runtime_state()
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
        Self::ms(&self.shared.telemetry.measured_latency_ms_bits)
    }

    pub fn control_audio_delay_ms(&self) -> f32 {
        Self::ms(&self.shared.telemetry.control_latency_ms_bits)
    }

    /// Smoothed control latency in ms (the value the servo actually tracks).
    pub fn smoothed_control_audio_delay_ms(&self) -> f32 {
        Self::ms(&self.shared.telemetry.smoothed_control_latency_ms_bits)
    }

    /// Ring-buffer occupancy converted to ms (first component of `control_available`).
    pub fn avail_input_audio_delay_ms(&self) -> f32 {
        Self::ms(&self.shared.telemetry.avail_input_latency_ms_bits)
    }

    /// Resampler output FIFO content converted back to input-domain ms
    /// (second component of `control_available`).
    pub fn output_fifo_audio_delay_ms(&self) -> f32 {
        Self::ms(&self.shared.telemetry.output_fifo_latency_ms_bits)
    }

    /// Local resampler pending input samples expressed as ms
    /// (third component of `control_available`).
    pub fn resampler_pending_audio_delay_ms(&self) -> f32 {
        Self::ms(&self.shared.telemetry.resampler_pending_latency_ms_bits)
    }

    fn ms(bits: &AtomicU32) -> f32 {
        f32::from_bits(bits.load(Ordering::Relaxed))
    }

    /// Diagnostic metric handles published by the device callback, for the
    /// global registry.
    pub fn diag_atomic_handles(&self) -> Vec<diag::DiagAtomicHandle> {
        self.shared.telemetry.diag_handles()
    }

    /// Signal the audio thread to snap the resampling ratio back to base and reset the integrator.
    pub fn request_ratio_reset(&self) {
        self.shared.request_ratio_reset();
    }

    /// Update adaptive resampling tuning parameters without restarting the audio stream.
    pub fn update_adaptive_config(&self, config: AdaptiveResamplingConfig) {
        self.backpressure_disabled
            .store(config.disable_backpressure, Ordering::Relaxed);
        warn_if_output_pacing_requested(self.host_name, &config);
        *self.shared.live_config.lock() = config;
    }
}
