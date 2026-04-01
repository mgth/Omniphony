#![cfg(target_os = "linux")]

use anyhow::{Result, anyhow};
use crossbeam::queue::ArrayQueue;
use pipewire as pw;
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{
    ADAPTIVE_BAND_FAR, ADAPTIVE_BAND_NEAR, AdaptiveResamplingConfig,
    LOCAL_RESAMPLER_MAX_RELATIVE_RATIO, adaptive_runtime_state_code,
    adaptive_runtime_state_name_from_code, clamp_ratio_for_local_resampler,
    local_resampler_ratio_bounds,
    adaptive_runtime::{
        AdaptiveRuntimeState, FarModeDecision, LatencyMetricTargets, MAX_INTEGRAL_TERM,
        adaptive_runtime_state_name, compute_hard_recover_high_plan,
        discard_ring_samples, note_refill_or_underrun, far_mode_band_from_latency,
        output_to_input_domain_samples, paused_rate_adjust, postprocess_interleaved_output,
        reset_adaptive_runtime,
        run_adaptive_servo, should_run_adaptive_servo, update_far_mode_state,
        update_latency_metrics, zero_pad_tail,
    },
    ring_buffer_io::{flush_ring_buffer, push_samples_with_backpressure},
    resampler_fifo::{RESAMPLER_CHUNK_SIZE, ResamplerFifoEngine},
};

// FFI bindings for PipeWire thread-safe rate control and stream timing
#[link(name = "pipewire-0.3")]
unsafe extern "C" {
    fn pw_stream_set_control(
        stream: *mut std::ffi::c_void,
        id: u32,
        n_values: u32,
        values: *const f32,
        flags: u32,
    ) -> i32;

    fn pw_thread_loop_lock(loop_: *mut std::ffi::c_void);
    fn pw_thread_loop_unlock(loop_: *mut std::ffi::c_void);

    /// RT-safe.  `time` must point to a zero-initialised PwTime.
    fn pw_stream_get_time(stream: *mut std::ffi::c_void, time: *mut PwTime) -> i32;
}

/// Mirrors `struct spa_fraction` from <spa/utils/defs.h>
#[repr(C)]
struct SpaFraction {
    num: u32,
    denom: u32,
}

/// Mirrors `struct pw_time` from <pipewire/stream.h>.
/// Must match the full C struct exactly to avoid stack corruption when
/// pw_stream_get_time() writes past the end of an undersized struct.
/// Fields up to `queued` exist since 0.3.0; `buffered`/`queued_buffers`/
/// `avail_buffers` were added in 0.3.50; `size` in 1.1.0.
/// Total: 64 bytes (verified against pipewire-sys bindgen output).
#[repr(C)]
#[derive(Default)]
struct PwTime {
    now: i64,
    rate: SpaFraction,
    ticks: u64,
    /// Downstream graph latency in `rate` ticks (frames at `rate.denom` Hz).
    /// Does NOT include queued ring-buffer samples.
    delay: i64,
    queued: u64,
    // Fields added in 0.3.50 — must be present to avoid stack overflow.
    buffered: u64,
    queued_buffers: u32,
    avail_buffers: u32,
    // Field added in 1.1.0.
    size: u64,
}

impl Default for SpaFraction {
    fn default() -> Self {
        SpaFraction { num: 0, denom: 1 }
    }
}

// SPA control IDs from spa/control/control.h
const SPA_PROP_RATE: u32 = 3;

/// Convert speaker name to PipeWire channel position name
/// PipeWire expects lowercase positions like "FL", "FR", "FC", "LFE", "RL", "RR", etc.
fn to_pipewire_position(name: &str) -> String {
    match name {
        "C" => "FC".to_string(),    // Center → Front-Center
        "BL" => "RL".to_string(),   // Back-Left → Rear-Left
        "BR" => "RR".to_string(),   // Back-Right → Rear-Right
        "BC" => "RC".to_string(),   // Back-Center → Rear-Center
        other => other.to_string(), // FL, FR, LFE, SL, SR, etc. stay as-is
    }
}

// Buffer size: 4 seconds of audio at 48kHz, 16 channels
const BUFFER_SIZE: usize = 48000 * 16 * 4;

/// Runtime configuration for PipeWire buffer sizes and quantum.
///
/// All latency values are in milliseconds and converted to frames at runtime
/// using the actual sample rate, so they work correctly with any sample rate.
///
/// `latency_ms` is the PI controller target.
///
/// `max_latency_ms` should be set to at least `2 × latency_ms` to give the
/// ring buffer enough headroom for mpv burst writes without blocking the writer.
#[derive(Debug, Clone)]
pub struct PipewireBufferConfig {
    /// Target latency used by the PI controller (ms). Default: 500.
    pub latency_ms: u32,
    /// Maximum buffer fill before applying back-pressure (ms). Default: latency_ms × 2.
    pub max_latency_ms: u32,
    /// PipeWire processing quantum in frames. Default: 1024 (~21ms at 48kHz).
    pub quantum_frames: u32,
}

impl Default for PipewireBufferConfig {
    fn default() -> Self {
        let latency_ms = 500;
        Self {
            latency_ms,
            max_latency_ms: latency_ms * 2,
            quantum_frames: 1024,
        }
    }
}

pub type PipewireAdaptiveResamplingConfig = AdaptiveResamplingConfig;

pub fn list_pipewire_output_devices() -> Result<Vec<(String, String)>> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|e| anyhow!("Failed to create PipeWire main loop: {e:?}"))?;
    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(|e| anyhow!("Failed to create PipeWire context: {e:?}"))?;
    let core = context
        .connect_rc(None)
        .map_err(|e| anyhow!("Failed to connect to PipeWire core: {e:?}"))?;
    let registry = core
        .get_registry()
        .map_err(|e| anyhow!("Failed to get PipeWire registry: {e:?}"))?;

    let done = Rc::new(Cell::new(false));
    let collected = Rc::new(RefCell::new(Vec::<(String, String)>::new()));

    let pending = core
        .sync(0)
        .map_err(|e| anyhow!("PipeWire sync failed: {e:?}"))?;

    let done_clone = Rc::clone(&done);
    let loop_clone = mainloop.clone();
    let _listener_core = core
        .add_listener_local()
        .done(move |id, seq| {
            if id == pw::core::PW_ID_CORE && seq == pending {
                done_clone.set(true);
                loop_clone.quit();
            }
        })
        .register();

    let collected_clone = Rc::clone(&collected);
    let _listener_registry = registry
        .add_listener_local()
        .global(move |global| {
            if global.type_ != pw::types::ObjectType::Node {
                return;
            }
            let Some(props) = global.props.as_ref() else {
                return;
            };
            let Some(media_class) = props.get(*pw::keys::MEDIA_CLASS) else {
                return;
            };
            if media_class != "Audio/Sink" {
                return;
            }

            let Some(value) = props
                .get(*pw::keys::NODE_NAME)
                .map(str::trim)
                .filter(|v| !v.is_empty())
            else {
                return;
            };

            let label = props
                .get(*pw::keys::NODE_DESCRIPTION)
                .or_else(|| props.get(*pw::keys::NODE_NICK))
                .or_else(|| props.get(*pw::keys::DEVICE_DESCRIPTION))
                .or_else(|| props.get(*pw::keys::DEVICE_NAME))
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .unwrap_or(value);

            collected_clone
                .borrow_mut()
                .push((value.to_string(), label.to_string()));
        })
        .register();

    while !done.get() {
        mainloop.run();
    }

    let mut devices = collected.borrow().clone();
    devices.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    devices.dedup_by(|a, b| a.0 == b.0);
    Ok(devices)
}

// Small always-on PipeWire latency servo used even when user-facing adaptive
// resampling is disabled. The goal is not aggressive correction, only holding
// the ring buffer close to the requested latency target without audible glitches.
const LATENCY_SERVO_P_GAIN: f64 = 0.000004;
const LATENCY_SERVO_I_GAIN: f64 = 0.0000002;
const LATENCY_SERVO_MAX_RATE_ADJUST: f64 = 0.03;
fn wallclock_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub struct PipewireWriter {
    sample_buffer: Arc<ArrayQueue<f32>>,
    sample_rate: u32,
    channel_count: u32,
    /// Pre-computed back-pressure threshold in samples (max_latency_ms → samples).
    max_buffer_samples: usize,
    /// PipeWire quantum latency in ms (pre-computed for use in latency_ms()).
    quantum_ms: f32,
    stream_ready: Arc<AtomicBool>,
    enable_adaptive_resampling: bool,
    /// Current rate-adjust factor applied by the PI controller (f32 bits).
    /// 1.0 = nominal; >1.0 = PipeWire consuming slightly faster; <1.0 = slower.
    /// Only meaningful when adaptive resampling is enabled.
    current_rate_adjust: Arc<AtomicU32>,
    /// 0 = unknown, 1 = near, 2 = far.
    current_adaptive_band: Arc<AtomicU8>,
    /// 0 = idle, 1 = low-recover, 2 = settling, 3 = high-recover.
    current_runtime_state: Arc<AtomicU8>,
    /// Signals the PipeWire worker thread to stop and exit cleanly.
    shutdown_requested: Arc<AtomicBool>,
    /// Timestamp of the last successful write into the local ring buffer.
    last_write_ms: Arc<AtomicU64>,
    /// Smoothed measured total latency (ring + output FIFO + graph) in ms bits.
    measured_latency_ms_bits: Arc<AtomicU32>,
    /// Internal controller latency (ring + output FIFO midpoint) in ms bits.
    control_latency_ms_bits: Arc<AtomicU32>,
    /// Downstream graph latency as measured by pw_stream_get_time().delay (f32 ms bits).
    /// Updated every ~100 callbacks once the stream is stable.
    graph_latency_ms_bits: Arc<AtomicU32>,
    /// Configured ring-buffer target latency (from PipewireBufferConfig::latency_ms).
    target_latency_ms: u32,
    live_adaptive_config: Arc<Mutex<AdaptiveResamplingConfig>>,
    reset_ratio_requested: Arc<AtomicBool>,
    pw_thread: Option<thread::JoinHandle<()>>,
    bootstrap_started_at: Instant,
    bootstrap_write_calls: u32,
    bootstrap_written_samples: usize,
    /// Direct trigger mode: closure calling pw_stream_trigger_process() on the capture DRIVER stream.
    /// When set, the output process callback fires this N times per callback (Bresenham ratio).
    input_trigger_fn: Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync + 'static>>>>,
    /// Sample rate of the capture stream, used to compute the Bresenham trigger ratio.
    input_trigger_rate_hz: Arc<AtomicU32>,
}

impl PipewireWriter {
    pub fn new(
        sample_rate: u32,
        channel_count: u32,
        output_device: Option<String>,
        enable_adaptive_resampling: bool,
        output_sample_rate: Option<u32>,
        buffer_config: PipewireBufferConfig,
        adaptive_config: PipewireAdaptiveResamplingConfig,
    ) -> Result<Self> {
        Self::new_with_channel_names(
            sample_rate,
            channel_count,
            output_device,
            None,
            enable_adaptive_resampling,
            output_sample_rate,
            buffer_config,
            adaptive_config,
        )
    }

    pub fn new_with_channel_names(
        sample_rate: u32,
        channel_count: u32,
        output_device: Option<String>,
        channel_names: Option<Vec<String>>,
        enable_adaptive_resampling: bool,
        output_sample_rate: Option<u32>,
        mut buffer_config: PipewireBufferConfig,
        adaptive_config: PipewireAdaptiveResamplingConfig,
    ) -> Result<Self> {
        // Keep headroom above target, otherwise PI control saturates and the
        // buffer tends to stabilize below setpoint (target at the ceiling).
        if buffer_config.max_latency_ms <= buffer_config.latency_ms {
            let corrected = buffer_config.latency_ms.saturating_mul(2);
            log::warn!(
                "PipeWire max_latency_ms ({}) must be > latency_ms ({}). Auto-correcting to {} ms.",
                buffer_config.max_latency_ms,
                buffer_config.latency_ms,
                corrected
            );
            buffer_config.max_latency_ms = corrected;
        }

        let sample_buffer = Arc::new(ArrayQueue::new(BUFFER_SIZE));
        let buffer_clone = sample_buffer.clone();
        let stream_ready = Arc::new(AtomicBool::new(false));
        let ready_clone = stream_ready.clone();
        let ready_for_thread_cleanup = stream_ready.clone();
        let current_rate_adjust = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let rate_adjust_clone = current_rate_adjust.clone();
        let current_adaptive_band = Arc::new(AtomicU8::new(0));
        let adaptive_band_clone = current_adaptive_band.clone();
        let current_runtime_state = Arc::new(AtomicU8::new(0));
        let runtime_state_clone = current_runtime_state.clone();
        let shutdown_requested = Arc::new(AtomicBool::new(false));
        let shutdown_requested_clone = shutdown_requested.clone();
        let last_write_ms = Arc::new(AtomicU64::new(wallclock_millis()));
        let last_write_ms_clone = last_write_ms.clone();
        let measured_latency_ms_bits = Arc::new(AtomicU32::new(0u32));
        let measured_latency_clone = measured_latency_ms_bits.clone();
        let control_latency_ms_bits = Arc::new(AtomicU32::new(0u32));
        let control_latency_clone = control_latency_ms_bits.clone();
        let graph_latency_ms_bits = Arc::new(AtomicU32::new(0u32));
        let graph_latency_clone = graph_latency_ms_bits.clone();
        let live_config = Arc::new(Mutex::new(adaptive_config));
        let adaptive_config_for_thread = Arc::clone(&live_config);
        let reset_ratio_requested = Arc::new(AtomicBool::new(false));
        let reset_ratio_for_thread = Arc::clone(&reset_ratio_requested);
        let input_trigger_fn: Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync + 'static>>>> =
            Arc::new(Mutex::new(None));
        let input_trigger_fn_for_thread = Arc::clone(&input_trigger_fn);
        let input_trigger_rate_hz = Arc::new(AtomicU32::new(0));
        let input_trigger_rate_for_thread = Arc::clone(&input_trigger_rate_hz);

        // Capture before moving buffer_config into the thread closure.
        let max_latency_ms = buffer_config.max_latency_ms;
        let target_latency_ms = buffer_config.latency_ms;
        let output_rate_for_quantum = output_sample_rate.unwrap_or(sample_rate);
        let quantum_ms =
            buffer_config.quantum_frames as f32 / output_rate_for_quantum as f32 * 1000.0;

        // Spawn PipeWire thread
        let pw_thread = thread::spawn(move || {
            log::debug!("PipeWire thread started");
            if let Err(e) = run_pipewire_loop(
                buffer_clone,
                sample_rate,
                channel_count,
                ready_clone,
                output_device,
                channel_names,
                enable_adaptive_resampling,
                output_sample_rate,
                buffer_config,
                adaptive_config_for_thread,
                reset_ratio_for_thread,
                rate_adjust_clone,
                adaptive_band_clone,
                runtime_state_clone,
                shutdown_requested_clone,
                last_write_ms_clone,
                measured_latency_clone,
                control_latency_clone,
                graph_latency_clone,
                input_trigger_fn_for_thread,
                input_trigger_rate_for_thread,
            ) {
                log::error!("PipeWire thread error: {}", e);
            }
            ready_for_thread_cleanup.store(false, Ordering::Relaxed);
            log::debug!("PipeWire thread exited");
        });

        // Wait for stream to be ready (with timeout)
        let timeout = Duration::from_secs(3);
        let start = std::time::Instant::now();
        while !stream_ready.load(Ordering::Relaxed) {
            if start.elapsed() > timeout {
                log::warn!("PipeWire stream initialization timeout - continuing anyway");
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }

        log::info!(
            "PipeWire stream initialized: {} Hz, {} channels",
            sample_rate,
            channel_count
        );
        log::info!("Audio streaming to PipeWire is now active");

        if enable_adaptive_resampling {
            log::info!("PipeWire adaptive resampling enabled (PI controller for buffer stability)");
        } else {
            log::info!("PipeWire adaptive resampling disabled (fixed playback rate)");
        }

        let max_buffer_samples =
            (max_latency_ms as usize * sample_rate as usize / 1000) * channel_count as usize;

        Ok(Self {
            sample_buffer,
            sample_rate,
            channel_count,
            max_buffer_samples,
            quantum_ms,
            stream_ready,
            enable_adaptive_resampling,
            current_rate_adjust,
            current_adaptive_band,
            current_runtime_state,
            shutdown_requested,
            last_write_ms,
            measured_latency_ms_bits,
            control_latency_ms_bits,
            graph_latency_ms_bits,
            target_latency_ms,
            live_adaptive_config: live_config,
            reset_ratio_requested,
            pw_thread: Some(pw_thread),
            bootstrap_started_at: Instant::now(),
            bootstrap_write_calls: 0,
            bootstrap_written_samples: 0,
            input_trigger_fn,
            input_trigger_rate_hz,
        })
    }

    pub fn write_samples(&mut self, samples: &[f32]) -> Result<()> {
        // Check if stream is ready
        if !self.stream_ready.load(Ordering::Relaxed) {
            log::trace!("Stream not ready yet, dropping {} samples", samples.len());
            return Ok(());
        }

        let max_buffer_fill = self.max_buffer_samples;
        let buffer_before = self.sample_buffer.len();
        let report = push_samples_with_backpressure(
            &self.sample_buffer,
            samples,
            max_buffer_fill,
            10,
            200,
        );
        if report.timed_out {
            log::warn!(
                "Buffer drain timeout after 2s - dropping {} remaining samples to prevent OOM",
                samples.len().saturating_sub(report.pushed_samples)
            );
        }

        // Only log if we had to wait (indicates potential issues)
        if report.wait_count > 0 {
            log::trace!(
                "Buffer drain wait: {} waits ({}ms), pushed {} samples",
                report.wait_count,
                report.wait_count * 10,
                report.pushed_samples
            );
        }

        self.bootstrap_write_calls = self.bootstrap_write_calls.saturating_add(1);
        self.bootstrap_written_samples = self
            .bootstrap_written_samples
            .saturating_add(report.pushed_samples);
        if report.pushed_samples > 0 {
            self.last_write_ms
                .store(wallclock_millis(), Ordering::Relaxed);
        }
        if self.bootstrap_write_calls <= 5 {
            log::info!(
                "PipeWire bootstrap write #{}: pushed {} / {} samples, ring {} -> {}, elapsed {:.0} ms",
                self.bootstrap_write_calls,
                report.pushed_samples,
                samples.len(),
                buffer_before,
                self.sample_buffer.len(),
                self.bootstrap_started_at.elapsed().as_secs_f64() * 1000.0
            );
        }

        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        let report = flush_ring_buffer(
            &self.sample_buffer,
            Duration::from_secs(5),
            Duration::from_millis(10),
            Some(Duration::from_millis(500)),
        );
        if report.timed_out {
            log::warn!("Flush timeout - {} samples remaining", report.remaining_samples);
        } else if report.stalled {
            log::debug!(
                "Flush: buffer stalled at {} samples, draining",
                report.remaining_samples
            );
        }

        log::debug!("PipeWire buffer flushed");
        Ok(())
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channel_count(&self) -> u32 {
        self.channel_count
    }

    pub fn buffer_fill_level(&self) -> usize {
        self.sample_buffer.len()
    }

    /// Estimated current end-to-end audio latency in milliseconds.
    ///
    /// Composed of:
    /// - Ring buffer latency: current fill (in frames) / sample_rate
    /// - PipeWire quantum latency: quantum_frames / output_sample_rate
    pub fn latency_ms(&self) -> f32 {
        let fill_frames = self.sample_buffer.len() / self.channel_count as usize;
        let ring_ms = fill_frames as f32 / self.sample_rate as f32 * 1000.0;
        ring_ms + self.quantum_ms
    }

    /// Current rate-adjust factor applied by the PI controller.
    /// Returns `None` when adaptive resampling is disabled.
    /// Value is near 1.0; deviation from 1.0 represents clock drift correction
    /// (e.g. 1.0015 = consuming 0.15 % faster than nominal to drain the buffer).
    pub fn rate_adjust(&self) -> Option<f32> {
        if self.enable_adaptive_resampling {
            Some(f32::from_bits(
                self.current_rate_adjust.load(Ordering::Relaxed),
            ))
        } else {
            None
        }
    }

    /// Register the closure that fires pw_stream_trigger_process() on the capture DRIVER stream.
    /// Once set, the output process callback fires this N times per callback using Bresenham
    /// scheduling (ratio = input_rate_hz / output_sample_rate).
    pub fn set_input_trigger(&self, f: Arc<dyn Fn() + Send + Sync + 'static>, rate_hz: u32) {
        *self.input_trigger_fn.lock().unwrap() = Some(f);
        self.input_trigger_rate_hz.store(rate_hz, Ordering::Relaxed);
    }

    pub fn adaptive_band(&self) -> Option<&'static str> {
        match self.current_adaptive_band.load(Ordering::Relaxed) {
            1 => Some("near"),
            2 => Some("far"),
            3 => Some("hard"),
            _ => None,
        }
    }

    pub fn adaptive_runtime_state(&self) -> Option<&'static str> {
        adaptive_runtime_state_name_from_code(self.current_runtime_state.load(Ordering::Relaxed))
    }

    /// Downstream graph latency in ms as reported by pw_stream_get_time().delay.
    /// Includes PipeWire graph scheduling and the netjack2 driver quantum.
    /// Returns 0.0 until the stream has been active for ~2 seconds.
    pub fn graph_latency_ms(&self) -> f32 {
        f32::from_bits(self.graph_latency_ms_bits.load(Ordering::Relaxed))
    }

    /// Target audio delay seen by the listener:
    /// configured ring-buffer target + PipeWire graph latency.
    /// Pass the negative of this (in seconds) to mpv's `audio-delay`.
    pub fn total_audio_delay_ms(&self) -> f32 {
        self.target_latency_ms as f32 + self.graph_latency_ms()
    }

    /// Measured total audio delay seen by the listener:
    /// current ring-buffer latency + PipeWire graph latency.
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

    /// Update adaptive resampling tuning parameters without restarting the audio thread.
    pub fn update_adaptive_config(&self, config: AdaptiveResamplingConfig) {
        if let Ok(mut c) = self.live_adaptive_config.lock() {
            *c = config;
        }
    }
}

impl Drop for PipewireWriter {
    fn drop(&mut self) {
        log::debug!("Dropping PipeWire writer");
        self.shutdown_requested.store(true, Ordering::Relaxed);
        // Discard any remaining samples — flush() was already called by finalize().
        // Calling flush() again here would block for another 500ms–5s if the callback
        // is in recovery mode.  A quick drain + brief pause is sufficient for Drop.
        while self.sample_buffer.pop().is_some() {}
        if let Some(handle) = self.pw_thread.take() {
            let _ = handle.join();
        }
    }
}

fn run_pipewire_loop(
    buffer: Arc<ArrayQueue<f32>>,
    sample_rate: u32, // Native sample rate (48000 Hz)
    channel_count: u32,
    stream_ready: Arc<AtomicBool>,
    output_device: Option<String>,
    channel_names: Option<Vec<String>>,
    enable_adaptive_resampling: bool,
    output_sample_rate: Option<u32>, // Target output rate for upsampling
    buffer_config: PipewireBufferConfig,
    adaptive_config: Arc<Mutex<PipewireAdaptiveResamplingConfig>>,
    reset_ratio_requested: Arc<AtomicBool>,
    current_rate_adjust: Arc<AtomicU32>,
    current_adaptive_band: Arc<AtomicU8>,
    current_runtime_state: Arc<AtomicU8>,
    shutdown_requested: Arc<AtomicBool>,
    _last_write_ms: Arc<AtomicU64>,
    measured_latency_ms_out: Arc<AtomicU32>,
    control_latency_ms_out: Arc<AtomicU32>,
    graph_latency_ms_out: Arc<AtomicU32>,
    input_trigger_fn: Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync + 'static>>>>,
    input_trigger_rate_hz: Arc<AtomicU32>,
) -> Result<()> {
    // Determine actual output rate and resampling ratio
    let actual_output_rate = output_sample_rate.unwrap_or(sample_rate);
    let resample_ratio = actual_output_rate as f64 / sample_rate as f64;
    let needs_resampling = resample_ratio != 1.0;
    let use_local_resampler = needs_resampling || enable_adaptive_resampling;

    if use_local_resampler {
        log::info!(
            "PipeWire local resampling: {} Hz -> {} Hz (ratio {:.2}x, adaptive={})",
            sample_rate,
            actual_output_rate,
            resample_ratio,
            enable_adaptive_resampling
        );
    }

    pw::init();

    // Use ThreadLoop for thread-safe rate control
    let main_loop = unsafe {
        pw::thread_loop::ThreadLoopRc::new(None, None)
            .map_err(|e| anyhow!("Failed to create thread loop: {:?}", e))?
    };

    let context = pw::context::ContextRc::new(&main_loop, None)
        .map_err(|e| anyhow!("Failed to create context: {:?}", e))?;

    let core = context
        .connect_rc(None)
        .map_err(|e| anyhow!("Failed to connect: {:?}", e))?;

    let mut props = pw::properties::PropertiesBox::new();

    // Set node name
    props.insert("node.name", "omniphony-vbap-renderer");
    props.insert("media.name", "VBAP Spatial Audio");

    // Set target output node if specified
    if let Some(ref target) = output_device {
        props.insert("node.target", target.as_str());
        log::info!("PipeWire output target: {}", target);
    }

    // Set channel names if provided (e.g., "FL,FR,C,LFE,BL,BR")
    // audio.position tells PipeWire the spatial positions of channels
    // Convert to PipeWire standard names (C→FC, BL→RL, BR→RR)
    let positions_string = channel_names.as_ref().map(|names| {
        names
            .iter()
            .map(|n| to_pipewire_position(n))
            .collect::<Vec<_>>()
            .join(",")
    });
    let channels_string = channel_count.to_string();
    if let Some(ref positions) = positions_string {
        props.insert("audio.position", positions.as_str());
        props.insert("audio.channels", channels_string.as_str());
        log::info!("PipeWire channel positions: {}", positions);
    }

    // Request a graph latency aligned with the configured target instead of
    // pinning the stream to the processing quantum.
    let requested_latency_frames = ((buffer_config.latency_ms as u64 * actual_output_rate as u64)
        / 1000)
        .max(buffer_config.quantum_frames as u64) as u32;
    let requested_latency_str = format!("{}/{}", requested_latency_frames, actual_output_rate);
    props.insert("node.latency", requested_latency_str.as_str());

    log::debug!(
        "PipeWire stream properties configured: latency={}/{} (~{:.0}ms)",
        requested_latency_frames,
        actual_output_rate,
        requested_latency_frames as f64 / actual_output_rate as f64 * 1000.0
    );

    let stream = pw::stream::StreamBox::new(&core, "omniphony-audio", props)
        .map_err(|e| anyhow!("Failed to create stream: {:?}", e))?;

    // Setup state changed listener
    let ready_for_state = stream_ready.clone();
    let graph_latency_for_state = graph_latency_ms_out.clone();
    let _state_listener = stream
        .add_local_listener_with_user_data(())
        .state_changed(move |_, _, old, new| {
            log::info!("PipeWire stream state changed: {:?} -> {:?}", old, new);
            if new == pw::stream::StreamState::Streaming {
                ready_for_state.store(true, Ordering::Relaxed);
                log::info!("PipeWire stream is now STREAMING");
            } else {
                let was_ready = ready_for_state.swap(false, Ordering::Relaxed);
                graph_latency_for_state.store(0u32, Ordering::Relaxed);
                if was_ready {
                    log::warn!(
                        "PipeWire stream left STREAMING ({:?}); pausing writes until recovery",
                        new
                    );
                }
            }
        })
        .register()
        .map_err(|e| anyhow!("Failed to register state listener: {:?}", e))?;

    // Atomic for adaptive rate matching (stores f32::to_bits(rate))
    // 1.0 = normal speed, >1.0 = faster, <1.0 = slower
    let desired_rate = Arc::new(AtomicU32::new(1.0f32.to_bits()));

    // Snapshot the current config for initialisation (resampler max ratio, thresholds).
    // The live Arc is polled in the callback for any subsequent updates.
    let adaptive_config_snapshot = adaptive_config.lock().unwrap().clone();

    // Initialize resampler for true rate conversion and for adaptive 1:1 operation.
    let mut resampler_opt = if use_local_resampler {
        let params = SincInterpolationParameters {
            sinc_len: 256,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 256,
            window: WindowFunction::BlackmanHarris2,
        };

        // Rubato expects a relative ratio bound (>= 1.0), not an absolute ratio.
        let max_resample_ratio_relative = LOCAL_RESAMPLER_MAX_RELATIVE_RATIO;
        let (min_resample_ratio_abs, max_resample_ratio_abs) =
            local_resampler_ratio_bounds(resample_ratio);

        log::debug!(
            "Initializing PipeWire resampler: base_ratio={:.4}, min_ratio={:.4}, max_ratio={:.4}, chunk_size={}",
            resample_ratio,
            min_resample_ratio_abs,
            max_resample_ratio_abs,
            RESAMPLER_CHUNK_SIZE
        );

        let resampler = SincFixedIn::<f32>::new(
            resample_ratio,
            max_resample_ratio_relative,
            params,
            RESAMPLER_CHUNK_SIZE,
            channel_count as usize,
        )
        .map_err(|e| anyhow!("Failed to create resampler: {:?}", e))?;

        Some(resampler)
    } else {
        None
    };

    // Intermediate buffers for resampling
    let mut resampler_fifo = ResamplerFifoEngine::new(channel_count as usize);
    let mut effective_resample_ratio = resample_ratio;
    let mut runtime_state = AdaptiveRuntimeState::new(resample_ratio);
    runtime_state.activate_startup_low_recover();

    // Setup process callback
    let buffer_for_callback = buffer.clone();
    let desired_rate_for_callback = desired_rate.clone();
    let rate_adjust_for_callback = current_rate_adjust.clone();
    let shutdown_requested_for_callback = shutdown_requested.clone();
    let graph_latency_for_callback = graph_latency_ms_out.clone();
    let live_adaptive_config_for_callback = Arc::clone(&adaptive_config);
    let reset_ratio_for_callback = Arc::clone(&reset_ratio_requested);
    let adaptive_resampling_enabled = enable_adaptive_resampling;
    let latency_servo_enabled = !use_local_resampler;
    // Direct trigger mode: snapshot trigger fn lazily on first callback, then run Bresenham.
    let input_trigger_fn_for_callback = Arc::clone(&input_trigger_fn);
    let input_trigger_rate_for_callback = Arc::clone(&input_trigger_rate_hz);
    let mut local_input_trigger: Option<Arc<dyn Fn() + Send + Sync + 'static>> = None;
    let mut bresenham_acc: i64 = 0;
    // Compute channel-aware buffer thresholds for the callback (ms → frames → samples).
    // IMPORTANT: these thresholds are compared against `buffer_for_callback.len()`, which
    // stores INPUT-domain samples (writer pushes at `sample_rate` before local resampling).
    // Therefore the conversion must use the input sample rate, not `actual_output_rate`.
    // Using output rate here underestimates latency when downsampling (e.g. 96k -> 48k),
    // causing too-low target fill, long-term A/V drift, and instability.
    //
    // latency_ms is used as the PI controller setpoint.
    let latency_frames = (buffer_config.latency_ms as usize * sample_rate as usize) / 1000;
    let max_buffer_frames = (buffer_config.max_latency_ms as usize * sample_rate as usize) / 1000;
    let min_buffer_fill = latency_frames * channel_count as usize;
    let max_buffer_fill = max_buffer_frames * channel_count as usize;
    let target_buffer_fill = min_buffer_fill;
    let mut logged_runtime_target = target_buffer_fill;
    // Treat errors above ~120 ms as transient mismatch and allow faster convergence.
    let samples_per_ms = (sample_rate as usize).saturating_mul(channel_count as usize) / 1000;
    let samples_per_ms_f64 = samples_per_ms as f64;
    let mut fast_catchup_threshold =
        (adaptive_config_snapshot.near_far_threshold_ms as usize).saturating_mul(samples_per_ms);
    let mut adaptive_update_interval = adaptive_config_snapshot.update_interval_callbacks.max(1) as u64;

    log::info!(
        "PipeWire buffer thresholds ({}ch): latency={}ms max={}ms quantum={}fr | \
         target={} max={} samples",
        channel_count,
        buffer_config.latency_ms,
        buffer_config.max_latency_ms,
        buffer_config.quantum_frames,
        min_buffer_fill,
        max_buffer_fill
    );

    let _listener = stream
        .add_local_listener_with_user_data(())
        .process(move |stream, _| {
            if shutdown_requested_for_callback.load(Ordering::Relaxed) {
                return;
            }
            let callback_count = runtime_state.advance_callback();

            // --- Test controls: reset ratio / pause PI ---
            if reset_ratio_for_callback.load(Ordering::Relaxed) {
                reset_ratio_for_callback.store(false, Ordering::Relaxed);
                if let Some(ref mut resampler) = resampler_opt {
                    let _ = resampler.set_resample_ratio(resample_ratio, false);
                }
                let reset = reset_adaptive_runtime(&mut runtime_state, resample_ratio);
                effective_resample_ratio = reset.effective_resample_ratio;
                rate_adjust_for_callback
                    .store(reset.displayed_rate_adjust.to_bits(), Ordering::Relaxed);
                desired_rate_for_callback.store(1.0f32.to_bits(), Ordering::Relaxed);
                current_adaptive_band.store(reset.adaptive_band, Ordering::Relaxed);
            }
            let is_pi_paused = live_adaptive_config_for_callback
                .try_lock()
                .map(|cfg| cfg.paused)
                .unwrap_or(false);

            if let Some(mut buffer) = stream.dequeue_buffer() {
                let datas = buffer.datas_mut();
                if datas.is_empty() {
                    return;
                }

                let data = &mut datas[0];

                // Get the data slice and fill it
                let written = if let Some(slice) = data.data() {
                    let max_samples = slice.len() / 4; // 4 bytes per f32
                    let dest = unsafe {
                        std::slice::from_raw_parts_mut(
                            slice.as_ptr() as *mut f32,
                            max_samples,
                        )
                    };

                    // Ensure frame alignment: PipeWire may provide buffers sized
                    // for the sink's channel count rather than our source's channel count.
                    // We must only write complete frames to avoid channel misalignment.
                    let ch = channel_count as usize;
                    let max_frames = max_samples / ch;
                    let frame_aligned_max = max_frames * ch;
                    let runtime_target_buffer_fill = target_buffer_fill;

                    if callback_count == 1 {
                        log::info!(
                            "PipeWire callback #1: buffer={} bytes, {} samples, {} channels → {} frames (remainder: {})",
                            slice.len(), max_samples, ch, max_frames, max_samples % ch
                        );
                        if max_samples != frame_aligned_max {
                            log::warn!(
                                "PipeWire buffer NOT frame-aligned! {} samples / {} channels = {} remainder. Sink may have different channel count.",
                                max_samples, ch, max_samples % ch
                            );
                        }
                    }
                    if runtime_target_buffer_fill != logged_runtime_target {
                        log::info!(
                            "PipeWire runtime target fill adjusted to {} samples for observed callback size {} samples",
                            runtime_target_buffer_fill,
                            frame_aligned_max
                        );
                        logged_runtime_target = runtime_target_buffer_fill;
                    }

                    // Sample downstream graph latency (RT-safe: pw_stream_get_time is RT-safe
                    // inside the process callback). Update every ~100 callbacks to amortise cost.
                    if callback_count % 100 == 50 {
                        let stream_ptr = stream.as_raw_ptr();
                        let mut pw_t = PwTime::default();
                        let ok = unsafe { pw_stream_get_time(stream_ptr as *mut _, &mut pw_t) };
                        if ok == 0 && pw_t.rate.denom > 0 && pw_t.delay > 0 {
                            let delay_ms = pw_t.delay as f32 / pw_t.rate.denom as f32 * 1000.0;
                            graph_latency_for_callback.store(delay_ms.to_bits(), Ordering::Relaxed);
                        }
                    }

                    // Check if we have enough samples to prevent stuttering
                    let available = buffer_for_callback.len();
                    // Convert output FIFO contents back to input-domain samples so
                    // the controller sees the true amount of audio in flight, not
                    // just the ring buffer.  Without this, the controller
                    // systematically under-estimates the fill level and
                    // over-compensates (integral windup → buffer drain).
                    let output_fifo_input_domain_samples = output_to_input_domain_samples(
                        resampler_fifo.output_len(),
                        effective_resample_ratio,
                    );
                    // The callback observes the ring before consuming this PipeWire block.
                    // For latency control, use a midpoint estimate so the controller tracks
                    // roughly the same delay the UI/user perceives during the block.
                    // Convert callback size to input domain to avoid domain mismatch
                    // when upsampling (frame_aligned_max is in output domain).
                    let callback_input_domain_samples = if effective_resample_ratio > 0.0 {
                        ((frame_aligned_max as f64) / effective_resample_ratio).round() as usize
                    } else {
                        frame_aligned_max
                    };
                    let metrics = update_latency_metrics(
                        &mut runtime_state,
                        available,
                        output_fifo_input_domain_samples,
                        callback_input_domain_samples,
                        channel_count as usize,
                        sample_rate,
                        f32::from_bits(graph_latency_for_callback.load(Ordering::Relaxed)),
                        LatencyMetricTargets {
                            measured_latency_ms_bits: &measured_latency_ms_out,
                            control_latency_ms_bits: &control_latency_ms_out,
                        },
                    );
                    let current_adaptive_cfg = live_adaptive_config_for_callback.lock().unwrap().clone();
                    let fallback_band = far_mode_band_from_latency(
                        &current_adaptive_cfg,
                        metrics.control_available,
                        runtime_target_buffer_fill,
                        samples_per_ms,
                    );
                    current_adaptive_band.store(fallback_band, Ordering::Relaxed);
                    if let Some(ref mut resampler) = resampler_opt {
                        if adaptive_resampling_enabled
                            && !is_pi_paused
                        {
                            // Refresh config from the live Arc (non-blocking; keep stale on contention).
                            if let Ok(cfg) = live_adaptive_config_for_callback.try_lock() {
                                fast_catchup_threshold = (cfg.near_far_threshold_ms as usize)
                                    .saturating_mul(samples_per_ms);
                                adaptive_update_interval = cfg.update_interval_callbacks.max(1) as u64;
                            }
                            if should_run_adaptive_servo(
                                callback_count,
                                adaptive_update_interval as u32,
                                metrics.total_available_input_domain,
                                channel_count as usize,
                            ) {
                                let mut decision = run_adaptive_servo(
                                    &mut runtime_state,
                                    &current_adaptive_cfg,
                                    metrics,
                                    runtime_target_buffer_fill,
                                    resample_ratio,
                                    480,
                                    current_adaptive_cfg.max_adjust.max(0.000_001),
                                    samples_per_ms,
                                    samples_per_ms_f64,
                                );
                                current_adaptive_band.store(decision.adaptive_band, Ordering::Relaxed);

                                let clamped_ratio = clamp_ratio_for_local_resampler(
                                    resample_ratio,
                                    decision.step.current_ratio,
                                );
                                decision.step.current_ratio = clamped_ratio;
                                decision.step.consume_adjust = resample_ratio / clamped_ratio;
                                decision.effective_resample_ratio = clamped_ratio;
                                decision.displayed_rate_adjust =
                                    paused_rate_adjust(resample_ratio, clamped_ratio);

                                if let Err(e) =
                                    resampler.set_resample_ratio(clamped_ratio, true)
                                        as Result<(), rubato::ResampleError>
                                {
                                    log::warn!("Failed to set resampler ratio: {}", e);
                                } else {
                                    effective_resample_ratio = clamped_ratio;
                                    if effective_resample_ratio.to_bits()
                                        != runtime_state.last_logged_ratio_bits
                                    {
                                        let rel_ratio = effective_resample_ratio / resample_ratio;
                                        log::info!(
                                            "PipeWire adaptive ratio applied: base={:.6} effective={:.6} relative={:.6} consume={:.6} drift={} buf={}/{}",
                                            resample_ratio,
                                            effective_resample_ratio,
                                            rel_ratio,
                                            decision.step.consume_adjust,
                                            decision.step.drift,
                                            metrics.control_available,
                                            runtime_target_buffer_fill
                                        );
                                    }
                                    runtime_state.last_logged_ratio_bits =
                                        effective_resample_ratio.to_bits();
                                    decision.effective_resample_ratio = effective_resample_ratio;
                                }

                                rate_adjust_for_callback
                                    .store(decision.displayed_rate_adjust.to_bits(), Ordering::Relaxed);

                                if callback_count % 100 == 0 {
                                    log::debug!(
                                        "PipeWire Adaptive: buf={}/{} drift={} ratio={:.6} (base={:.2} P={:.6} I={:.6})",
                                        metrics.control_available,
                                        runtime_target_buffer_fill,
                                        decision.step.drift,
                                        decision.step.current_ratio,
                                        resample_ratio,
                                        decision.step.p_term,
                                        decision.step.i_term
                                    );
                                }
                            }
                        }

                        let audio_samples_needed = max_samples;
                        let far_mode_cfg = live_adaptive_config_for_callback.lock().unwrap().clone();
                        let far_decision = update_far_mode_state(
                            &mut runtime_state,
                            &far_mode_cfg,
                            current_adaptive_band.load(Ordering::Relaxed) == ADAPTIVE_BAND_FAR,
                            metrics.control_available,
                            runtime_target_buffer_fill,
                            callback_input_domain_samples,
                            effective_resample_ratio,
                            channel_count as usize,
                            sample_rate,
                            actual_output_rate,
                        );
                        current_runtime_state.store(
                            adaptive_runtime_state_code(adaptive_runtime_state_name(
                                runtime_state.low_recover_phase,
                                far_decision.hard_recover_high,
                            )),
                            Ordering::Relaxed,
                        );

                        if far_decision.hold_low_recover {
                            let muted_samples_to_consume = if far_decision.consume_while_muted {
                                audio_samples_needed
                            } else {
                                0
                            };
                            let muted_total_samples = muted_samples_to_consume
                                .saturating_add(far_decision.low_recover_trim_output_samples);
                            if muted_total_samples > 0 {
                                if let Err(e) = resampler_fifo.ensure_output_samples(
                                    &buffer_for_callback,
                                    resampler,
                                    muted_total_samples,
                                ) {
                                    log::error!("Resampler error: {}", e);
                                } else {
                                    if far_decision.low_recover_trim_output_samples > 0 {
                                        resampler_fifo.discard_samples(
                                            far_decision.low_recover_trim_output_samples,
                                        );
                                    }
                                    if muted_samples_to_consume > 0 {
                                        resampler_fifo.discard_samples(muted_samples_to_consume);
                                    }
                                }
                            }
                            dest[..max_samples].fill(0.0);
                        } else if let Err(e) = resampler_fifo.ensure_output_samples(
                            &buffer_for_callback,
                            resampler,
                            audio_samples_needed,
                        ) {
                            log::error!("Resampler error: {}", e);
                        }

                        if far_decision.hard_recover_high {
                            let plan = compute_hard_recover_high_plan(
                                callback_input_domain_samples,
                                metrics.control_available,
                                runtime_target_buffer_fill,
                                effective_resample_ratio,
                                channel_count as usize,
                            );
                            if let Err(e) = resampler_fifo.ensure_output_samples(
                                &buffer_for_callback,
                                resampler,
                                plan.desired_consume_output_samples,
                            ) {
                                log::error!("Resampler error: {}", e);
                            }
                            resampler_fifo.discard_samples(plan.desired_consume_output_samples);
                            dest[..max_samples].fill(0.0);
                        } else if far_decision.hold_low_recover {
                            dest[..max_samples].fill(0.0);
                        } else if resampler_fifo.output_len() >= audio_samples_needed {
                            let copied =
                                resampler_fifo.drain_into_slice(&mut dest[..audio_samples_needed]);
                            debug_assert_eq!(copied, audio_samples_needed);
                            postprocess_interleaved_output(
                                &mut dest[..audio_samples_needed],
                                ch,
                                far_decision.mute_far_output,
                                &mut runtime_state,
                            );
                        } else {
                            let fifo_available = resampler_fifo.output_len();
                            let copy_count =
                                resampler_fifo.drain_into_slice(&mut dest[..audio_samples_needed]);
                            zero_pad_tail(&mut dest[..max_samples], copy_count);
                            note_refill_or_underrun(
                                &mut runtime_state,
                                "Resampler underrun",
                                "Resampler underrun",
                                fifo_available,
                                audio_samples_needed,
                            );
                        }
                        max_samples
                    } else {
                        if latency_servo_enabled && !is_pi_paused && callback_count % adaptive_update_interval == 0 {
                            // Refresh config from live Arc (non-blocking; keep stale on contention).
                            let native_servo_cfg = live_adaptive_config_for_callback.lock().unwrap().clone();
                            if adaptive_resampling_enabled {
                                fast_catchup_threshold = (native_servo_cfg.near_far_threshold_ms as usize)
                                    .saturating_mul(samples_per_ms);
                                adaptive_update_interval = native_servo_cfg.update_interval_callbacks.max(1) as u64;
                            }
                            let drift =
                                metrics.control_available as i64 - runtime_target_buffer_fill as i64;
                            let drift_ms = drift as f64 / samples_per_ms_f64.max(1.0);

                            if drift.abs() > 480 {
                                if adaptive_resampling_enabled {
                                    let max_integral_term =
                                        native_servo_cfg.max_adjust.max(0.000_001);
                                    // accumulated_drift in ms for ppm/ms gains
                                    runtime_state.controller_state.accumulated_drift += drift_ms;
                                    let integral_contribution =
                                        runtime_state.controller_state.accumulated_drift
                                            * native_servo_cfg.ki / 1_000_000.0;
                                    if integral_contribution.abs() > max_integral_term
                                        && native_servo_cfg.ki > 0.0
                                    {
                                        runtime_state.controller_state.accumulated_drift =
                                            (max_integral_term * 1_000_000.0 / native_servo_cfg.ki)
                                                * integral_contribution.signum();
                                    }
                                } else {
                                    runtime_state.controller_state.accumulated_drift += drift as f64;
                                    let integral_contribution =
                                        runtime_state.controller_state.accumulated_drift
                                            * LATENCY_SERVO_I_GAIN;
                                    if integral_contribution.abs() > MAX_INTEGRAL_TERM {
                                        runtime_state.controller_state.accumulated_drift =
                                            (MAX_INTEGRAL_TERM / LATENCY_SERVO_I_GAIN)
                                                * integral_contribution.signum();
                                    }
                                }
                            }

                            let is_far = adaptive_resampling_enabled
                                && native_servo_cfg.enable_far_mode
                                && (drift.unsigned_abs() as usize) > fast_catchup_threshold;
                            let far_band = far_mode_band_from_latency(
                                &native_servo_cfg,
                                metrics.control_available,
                                runtime_target_buffer_fill,
                                samples_per_ms,
                            );
                            current_adaptive_band.store(
                                if adaptive_resampling_enabled { if is_far { ADAPTIVE_BAND_FAR } else { ADAPTIVE_BAND_NEAR } } else { far_band },
                                Ordering::Relaxed,
                            );
                            let p_term = if adaptive_resampling_enabled {
                                drift_ms * native_servo_cfg.kp_near / 1_000_000.0
                            } else {
                                drift as f64 * LATENCY_SERVO_P_GAIN / 100.0
                            };
                            let i_term = if adaptive_resampling_enabled {
                                runtime_state.controller_state.accumulated_drift
                                    * native_servo_cfg.ki / 1_000_000.0
                            } else {
                                runtime_state.controller_state.accumulated_drift * LATENCY_SERVO_I_GAIN
                            };
                            let max_adjust = if adaptive_resampling_enabled {
                                native_servo_cfg.max_adjust.max(0.000001)
                            } else {
                                LATENCY_SERVO_MAX_RATE_ADJUST
                            };
                            let consume_adjust =
                                (1.0 + p_term + i_term).clamp(1.0 - max_adjust, 1.0 + max_adjust);
                            let pipewire_rate = (1.0 / consume_adjust) as f32;

                            rate_adjust_for_callback
                                .store((consume_adjust as f32).to_bits(), Ordering::Relaxed);
                            desired_rate_for_callback.store(pipewire_rate.to_bits(), Ordering::Relaxed);

                            if callback_count % 100 == 0 {
                                if adaptive_resampling_enabled {
                                    log::trace!(
                                        "Adaptive rate: buf={} target={} max={} drift={} (P={:.6} I={:.6}) -> consume={:.6} pw_rate={:.6}",
                                        metrics.control_available,
                                        runtime_target_buffer_fill,
                                        max_buffer_fill,
                                        drift,
                                        p_term,
                                        i_term,
                                        consume_adjust,
                                        pipewire_rate
                                    );
                                } else {
                                    log::trace!(
                                        "PipeWire latency servo: buf={} target={} max={} drift={} -> consume={:.6} pw_rate={:.6}",
                                        metrics.control_available,
                                        runtime_target_buffer_fill,
                                        max_buffer_fill,
                                        drift,
                                        consume_adjust,
                                        pipewire_rate
                                    );
                                }
                            }
                        }

                        let available_frames = available / ch;
                        let frames_to_read = available_frames.min(max_frames);
                        let samples_to_read = frames_to_read * ch;

                        let far_mode_cfg2 = live_adaptive_config_for_callback.lock().unwrap().clone();
                        let far_decision: FarModeDecision = update_far_mode_state(
                            &mut runtime_state,
                            &far_mode_cfg2,
                            current_adaptive_band.load(Ordering::Relaxed) == ADAPTIVE_BAND_FAR,
                            metrics.control_available,
                            runtime_target_buffer_fill,
                            callback_input_domain_samples,
                            1.0,
                            channel_count as usize,
                            sample_rate,
                            actual_output_rate,
                        );
                        current_runtime_state.store(
                            adaptive_runtime_state_code(adaptive_runtime_state_name(
                                runtime_state.low_recover_phase,
                                far_decision.hard_recover_high,
                            )),
                            Ordering::Relaxed,
                        );
                        if far_decision.hard_recover_high {
                            let plan = compute_hard_recover_high_plan(
                                callback_input_domain_samples,
                                metrics.control_available,
                                runtime_target_buffer_fill,
                                1.0,
                                channel_count as usize,
                            );
                            let dropped =
                                discard_ring_samples(&buffer_for_callback, plan.desired_consume_input_samples);
                            if dropped < plan.desired_consume_input_samples {
                                log::debug!(
                                    "Far hard recover underfed: consumed {} / {} samples while targeting exact recovery",
                                    dropped,
                                    plan.desired_consume_input_samples
                                );
                            }
                            dest[..max_samples].fill(0.0);
                            max_samples
                        } else if far_decision.hold_low_recover {
                            let muted_samples_to_consume = if far_decision.consume_while_muted {
                                samples_to_read
                            } else {
                                0
                            };
                            let muted_total_samples = muted_samples_to_consume
                                .saturating_add(far_decision.low_recover_trim_input_samples);
                            if muted_total_samples > 0 {
                                let dropped =
                                    discard_ring_samples(&buffer_for_callback, muted_total_samples);
                                if dropped < muted_total_samples {
                                    log::debug!(
                                        "Low-recover muted consume underfed: consumed {} / {} samples while stabilizing resume latency",
                                        dropped,
                                        muted_total_samples
                                    );
                                }
                            }
                            dest[..max_samples].fill(0.0);
                            max_samples
                        } else {
                            let mut count = 0;
                            while count < samples_to_read {
                                if let Some(sample_f32) = buffer_for_callback.pop() {
                                    dest[count] = sample_f32;
                                    count += 1;
                                } else {
                                    break;
                                }
                            }

                            while count < max_samples {
                                dest[count] = 0.0;
                                count += 1;
                            }
                            postprocess_interleaved_output(
                                dest,
                                ch,
                                far_decision.mute_far_output,
                                &mut runtime_state,
                            );

                            if samples_to_read < frame_aligned_max {
                                note_refill_or_underrun(
                                    &mut runtime_state,
                                    "Buffer underrun",
                                    "Buffer underrun",
                                    samples_to_read,
                                    frame_aligned_max,
                                );
                            }

                            max_samples
                        }
                    }
                } else {
                    0
                };

                // Update chunk metadata
                let chunk = data.chunk_mut();
                *chunk.offset_mut() = 0;
                *chunk.size_mut() = (written * 4) as u32;
                *chunk.stride_mut() = 4;
            }

            // Direct trigger mode: fire pw_stream_trigger_process() on the capture DRIVER stream
            // N times per output callback, using Bresenham scheduling for non-integer ratios.
            // Lazily snapshot the closure on first availability (avoids locking on every callback).
            if local_input_trigger.is_none() {
                if let Ok(guard) = input_trigger_fn_for_callback.try_lock() {
                    local_input_trigger = guard.clone();
                }
            }
            if let Some(ref trigger) = local_input_trigger {
                let in_rate = input_trigger_rate_for_callback.load(Ordering::Relaxed) as i64;
                if in_rate > 0 {
                    bresenham_acc += in_rate;
                    while bresenham_acc >= actual_output_rate as i64 {
                        trigger();
                        bresenham_acc -= actual_output_rate as i64;
                    }
                }
            }
        })
        .register()
        .map_err(|e| anyhow!("Failed to register process listener: {:?}", e))?;

    // Configure audio format
    let mut audio_info = pw::spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(pw::spa::param::audio::AudioFormat::F32LE);
    audio_info.set_rate(actual_output_rate); // Use output rate (may be upsampled)
    audio_info.set_channels(channel_count);

    // Serialize format
    let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(pw::spa::pod::Object {
            type_: pw::spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
            id: pw::spa::param::ParamType::EnumFormat.as_raw(),
            properties: audio_info.into(),
        }),
    )
    .map_err(|e| anyhow!("Failed to serialize format: {:?}", e))?
    .0
    .into_inner();

    let param =
        pw::spa::pod::Pod::from_bytes(&values).ok_or_else(|| anyhow!("Failed to create param"))?;

    log::debug!("Connecting PipeWire stream...");

    // Lock for connection (returns RAII guard that auto-unlocks)
    {
        let _lock = main_loop.lock();

        // Connect stream
        stream
            .connect(
                pw::spa::utils::Direction::Output,
                None,
                pw::stream::StreamFlags::AUTOCONNECT
                    | pw::stream::StreamFlags::MAP_BUFFERS
                    | pw::stream::StreamFlags::RT_PROCESS,
                &mut [&param],
            )
            .map_err(|e| anyhow!("Failed to connect stream: {:?}", e))?;
    } // Lock automatically released here

    // Start the thread loop (this spawns the RT thread)
    main_loop.start();

    log::debug!("PipeWire thread loop started");

    // Thread-safe PipeWire rate control loop.
    // Runs whenever we are in the direct-copy path so the gentle latency servo
    // can hold the ring buffer near the requested target even with the public
    // adaptive mode disabled.
    if !use_local_resampler {
        let stream_ptr = stream.as_raw_ptr();
        let loop_ptr = main_loop.as_raw_ptr();
        let mut last_applied_rate = 1.0f32;

        loop {
            if shutdown_requested.load(Ordering::Relaxed) {
                break;
            }
            // Sleep to avoid busy-waiting (check every 50ms)
            thread::sleep(Duration::from_millis(50));

            // Read desired rate from atomic
            let rate_bits = desired_rate.load(Ordering::Relaxed);
            let desired_rate_value = f32::from_bits(rate_bits);

            // Apply every actual control-value change so the observed behavior
            // matches the requested ratio without an extra deadband here.
            if desired_rate_value.to_bits() != last_applied_rate.to_bits() {
                // Lock the thread loop for thread-safe API calls
                unsafe {
                    pw_thread_loop_lock(loop_ptr as *mut _);

                    // Apply rate control
                    let rate = desired_rate_value;
                    let result = pw_stream_set_control(
                        stream_ptr as *mut _,
                        SPA_PROP_RATE,
                        1,
                        &rate as *const f32,
                        0,
                    );

                    pw_thread_loop_unlock(loop_ptr as *mut _);

                    if result == 0 {
                        last_applied_rate = desired_rate_value;
                        log::trace!("Applied rate adjustment: {:.6}", rate);
                    } else {
                        log::warn!("Failed to apply rate control: {}", result);
                    }
                }
            }
        }
    } else {
        loop {
            if shutdown_requested.load(Ordering::Relaxed) {
                break;
            }
            thread::sleep(Duration::from_secs(1));
        }
    }

    // Stop the thread loop before returning so a dropped writer cannot leave
    // background PipeWire control threads alive. The stream/core teardown then
    // happens naturally when these objects are dropped on the owning thread.
    main_loop.stop();
    Ok(())
}
