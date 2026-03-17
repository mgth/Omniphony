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
    Arc,
    atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{
    ADAPTIVE_BAND_FAR, ADAPTIVE_BAND_HARD, ADAPTIVE_BAND_NEAR, AdaptiveControllerState,
    AdaptiveResamplingConfig, apply_ema, compute_adaptive_step,
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
/// `latency_ms` is both the prefill threshold (playback starts when the buffer
/// reaches this level) and the PI controller target.  Having them identical
/// means the controller starts in its linear regime from the very first frame.
///
/// `max_latency_ms` should be set to at least `2 × latency_ms` to give the
/// ring buffer enough headroom for mpv burst writes without blocking the writer.
#[derive(Debug, Clone)]
pub struct PipewireBufferConfig {
    /// Target latency: prefill threshold AND PI controller setpoint (ms). Default: 500.
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

    let pending = core.sync(0).map_err(|e| anyhow!("PipeWire sync failed: {e:?}"))?;

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

// Maximum I contribution: 200 ppm.  Keeps the integral from dominating when the buffer
// sits above target for a while (e.g. during initial mpv burst fill).
const MAX_INTEGRAL_TERM: f64 = 0.0002;
// Rubato resampler constants
const RESAMPLER_CHUNK_SIZE: usize = 1024; // Input chunk size for resampler

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
    /// Set by request_flush(); the PipeWire callback drains the ring buffer,
    /// resets its state, and clears this flag on the next callback invocation.
    flush_requested: Arc<AtomicBool>,
    /// Signals the PipeWire worker thread to stop and exit cleanly.
    shutdown_requested: Arc<AtomicBool>,
    /// Timestamp of the last successful write into the local ring buffer.
    last_write_ms: Arc<AtomicU64>,
    /// Downstream graph latency as measured by pw_stream_get_time().delay (f32 ms bits).
    /// Updated every ~100 callbacks once the stream is stable.
    graph_latency_ms_bits: Arc<AtomicU32>,
    /// Smoothed control latency used by the adaptive controller.
    control_latency_ms_bits: Arc<AtomicU32>,
    /// Configured ring-buffer target latency (from PipewireBufferConfig::latency_ms).
    target_latency_ms: u32,
    pw_thread: Option<thread::JoinHandle<()>>,
    bootstrap_started_at: Instant,
    bootstrap_write_calls: u32,
    bootstrap_written_samples: usize,
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
        let current_rate_adjust = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let rate_adjust_clone = current_rate_adjust.clone();
        let current_adaptive_band = Arc::new(AtomicU8::new(0));
        let adaptive_band_clone = current_adaptive_band.clone();
        let flush_requested = Arc::new(AtomicBool::new(false));
        let flush_requested_clone = flush_requested.clone();
        let shutdown_requested = Arc::new(AtomicBool::new(false));
        let shutdown_requested_clone = shutdown_requested.clone();
        let last_write_ms = Arc::new(AtomicU64::new(wallclock_millis()));
        let last_write_ms_clone = last_write_ms.clone();
        let graph_latency_ms_bits = Arc::new(AtomicU32::new(0u32));
        let graph_latency_clone = graph_latency_ms_bits.clone();
        let control_latency_ms_bits = Arc::new(AtomicU32::new(0u32));
        let control_latency_clone = control_latency_ms_bits.clone();
        let adaptive_config_for_thread = adaptive_config.clone();

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
                rate_adjust_clone,
                adaptive_band_clone,
                flush_requested_clone,
                shutdown_requested_clone,
                last_write_ms_clone,
                graph_latency_clone,
                control_latency_clone,
            ) {
                log::error!("PipeWire thread error: {}", e);
            }
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
            flush_requested,
            shutdown_requested,
            last_write_ms,
            graph_latency_ms_bits,
            control_latency_ms_bits,
            target_latency_ms,
            pw_thread: Some(pw_thread),
            bootstrap_started_at: Instant::now(),
            bootstrap_write_calls: 0,
            bootstrap_written_samples: 0,
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

        let mut sample_idx = 0;
        let mut wait_count = 0;
        let mut last_log_time = std::time::Instant::now();

        while sample_idx < samples.len() {
            // Check buffer fill level
            let buffer_level = self.sample_buffer.len();

            // If buffer is too full, wait for it to drain
            if buffer_level >= max_buffer_fill {
                if wait_count == 0 {
                    log::trace!(
                        "Buffer nearly full ({} samples / {} max), waiting for playback to catch up...",
                        buffer_level,
                        max_buffer_fill
                    );
                }
                wait_count += 1;
                thread::sleep(Duration::from_millis(10));

                // Log periodically while waiting
                if last_log_time.elapsed().as_secs() >= 2 {
                    log::warn!(
                        "Still waiting for buffer to drain: {} samples, waited {}ms",
                        buffer_level,
                        wait_count * 10
                    );
                    last_log_time = std::time::Instant::now();
                }

                // Safety timeout to prevent infinite loop and memory accumulation
                if wait_count > 200 {
                    // 2 seconds max wait instead of 5
                    log::warn!(
                        "Buffer drain timeout after 2s - dropping {} remaining samples to prevent OOM",
                        samples.len() - sample_idx
                    );
                    break;
                }
                continue;
            }

            // Push samples one by one until buffer is full or we've pushed everything
            while sample_idx < samples.len() && self.sample_buffer.len() < max_buffer_fill {
                if self.sample_buffer.push(samples[sample_idx]).is_ok() {
                    sample_idx += 1;
                } else {
                    // Buffer full, will wait on next iteration
                    break;
                }
            }
        }

        // Only log if we had to wait (indicates potential issues)
        if wait_count > 0 {
            log::trace!(
                "Buffer drain wait: {} waits ({}ms), pushed {} samples",
                wait_count,
                wait_count * 10,
                sample_idx
            );
        }

        self.bootstrap_write_calls = self.bootstrap_write_calls.saturating_add(1);
        self.bootstrap_written_samples = self.bootstrap_written_samples.saturating_add(sample_idx);
        if sample_idx > 0 {
            self.last_write_ms
                .store(wallclock_millis(), Ordering::Relaxed);
        }
        if self.bootstrap_write_calls <= 5 {
            log::info!(
                "PipeWire bootstrap write #{}: pushed {} / {} samples, ring {} -> {}, elapsed {:.0} ms",
                self.bootstrap_write_calls,
                sample_idx,
                samples.len(),
                buffer_before,
                self.sample_buffer.len(),
                self.bootstrap_started_at.elapsed().as_secs_f64() * 1000.0
            );
        }

        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        let timeout = Duration::from_secs(5);
        let start = std::time::Instant::now();
        let mut last_level = self.sample_buffer.len();
        let mut last_change = start;

        while !self.sample_buffer.is_empty() {
            if start.elapsed() > timeout {
                log::warn!(
                    "Flush timeout - {} samples remaining",
                    self.sample_buffer.len()
                );
                while self.sample_buffer.pop().is_some() {}
                break;
            }
            thread::sleep(Duration::from_millis(10));
            let current = self.sample_buffer.len();
            if current < last_level {
                last_level = current;
                last_change = std::time::Instant::now();
            } else if last_change.elapsed() > Duration::from_millis(500) {
                // Buffer stalled — likely in recovery mode (callback not consuming).
                // Drain immediately rather than spinning until timeout.
                log::debug!("Flush: buffer stalled at {} samples, draining", current);
                while self.sample_buffer.pop().is_some() {}
                break;
            }
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

    /// Ring-buffer-only latency in ms (excludes PipeWire graph delay).
    fn ring_latency_ms(&self) -> f32 {
        let fill_frames = self.sample_buffer.len() / self.channel_count as usize;
        fill_frames as f32 / self.sample_rate as f32 * 1000.0
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

    pub fn adaptive_band(&self) -> Option<&'static str> {
        match self.current_adaptive_band.load(Ordering::Relaxed) {
            1 => Some("near"),
            2 => Some("far"),
            3 => Some("hard"),
            _ => None,
        }
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
        self.ring_latency_ms() + self.graph_latency_ms()
    }

    pub fn control_audio_delay_ms(&self) -> f32 {
        f32::from_bits(self.control_latency_ms_bits.load(Ordering::Relaxed))
    }

    /// Signal the PipeWire callback to discard buffered audio and re-enter prefill.
    ///
    /// Call this after a decoder seek/reset so that stale pre-seek audio is not
    /// played out.  The callback drains the ring buffer and resets its state on
    /// the next invocation, then waits for the buffer to refill before resuming.
    pub fn request_flush(&self) {
        self.flush_requested.store(true, Ordering::Relaxed);
        log::debug!("PipeWire flush requested (seek/decoder reset)");
    }
}

impl Drop for PipewireWriter {
    fn drop(&mut self) {
        log::debug!("Dropping PipeWire writer");
        self.shutdown_requested.store(true, Ordering::Relaxed);
        self.flush_requested.store(true, Ordering::Relaxed);
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
    adaptive_config: PipewireAdaptiveResamplingConfig,
    current_rate_adjust: Arc<AtomicU32>,
    current_adaptive_band: Arc<AtomicU8>,
    flush_requested: Arc<AtomicBool>,
    shutdown_requested: Arc<AtomicBool>,
    _last_write_ms: Arc<AtomicU64>,
    graph_latency_ms_out: Arc<AtomicU32>,
    control_latency_ms_out: Arc<AtomicU32>,
) -> Result<()> {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum PlaybackState {
        Prefill,
        Running,
    }

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
    let _state_listener = stream
        .add_local_listener_with_user_data(())
        .state_changed(move |_, _, old, new| {
            log::info!("PipeWire stream state changed: {:?} -> {:?}", old, new);
            if new == pw::stream::StreamState::Streaming {
                ready_for_state.store(true, Ordering::Relaxed);
                log::info!("PipeWire stream is now STREAMING");
            }
        })
        .register()
        .map_err(|e| anyhow!("Failed to register state listener: {:?}", e))?;

    // Atomic for adaptive rate matching (stores f32::to_bits(rate))
    // 1.0 = normal speed, >1.0 = faster, <1.0 = slower
    let desired_rate = Arc::new(AtomicU32::new(1.0f32.to_bits()));

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
        let max_resample_ratio_relative = 1.0
            + adaptive_config
                .max_adjust
                .max(adaptive_config.max_adjust_far)
                .max(0.000001);
        let max_resample_ratio_abs = resample_ratio * max_resample_ratio_relative;

        log::debug!(
            "Initializing PipeWire resampler: base_ratio={:.4}, max_ratio={:.4}, chunk_size={}",
            resample_ratio,
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
    let mut resampler_input: Vec<Vec<f32>> =
        vec![vec![0.0; RESAMPLER_CHUNK_SIZE]; channel_count as usize];
    let mut input_frames_collected = 0;
    let mut output_fifo: Vec<f32> =
        Vec::with_capacity(RESAMPLER_CHUNK_SIZE * channel_count as usize * 4);

    // Setup process callback
    let buffer_for_callback = buffer.clone();
    let desired_rate_for_callback = desired_rate.clone();
    let rate_adjust_for_callback = current_rate_adjust.clone();
    let flush_requested_for_callback = flush_requested.clone();
    let shutdown_requested_for_callback = shutdown_requested.clone();
    let graph_latency_for_callback = graph_latency_ms_out.clone();
    let control_latency_for_callback = control_latency_ms_out.clone();
    let adaptive_resampling_enabled = enable_adaptive_resampling;
    let latency_servo_enabled = !use_local_resampler;
    let mut callback_count = 0u64;
    let mut playback_state = PlaybackState::Prefill;
    let mut prefill_stable_callbacks = 0u32;
    let mut last_prefill_available = 0usize;
    let mut underrun_warned = false;
    let mut controller_state = AdaptiveControllerState::default();
    // -1 = hard underfill recovery (zero-fill), 1 = hard overfill recovery (drop), 0 = inactive.
    let mut hard_correction_mode = 0i8;

    // Compute channel-aware buffer thresholds for the callback (ms → frames → samples).
    // IMPORTANT: these thresholds are compared against `buffer_for_callback.len()`, which
    // stores INPUT-domain samples (writer pushes at `sample_rate` before local resampling).
    // Therefore the conversion must use the input sample rate, not `actual_output_rate`.
    // Using output rate here underestimates latency when downsampling (e.g. 96k -> 48k),
    // causing too-low target fill, long-term A/V drift, and instability.
    //
    // latency_ms is used for both the prefill threshold and the PI controller setpoint so
    // that the controller starts in its linear regime from the very first frame.
    let latency_frames = (buffer_config.latency_ms as usize * sample_rate as usize) / 1000;
    let max_buffer_frames = (buffer_config.max_latency_ms as usize * sample_rate as usize) / 1000;
    let min_buffer_fill = latency_frames * channel_count as usize;
    let max_buffer_fill = max_buffer_frames * channel_count as usize;
    let target_buffer_fill = min_buffer_fill; // same as prefill threshold
    let mut logged_runtime_target = target_buffer_fill;
    // Treat errors above ~120 ms as transient mismatch and allow faster convergence.
    let samples_per_ms = (sample_rate as usize).saturating_mul(channel_count as usize) / 1000;
    let fast_catchup_threshold =
        (adaptive_config.near_far_threshold_ms as usize).saturating_mul(samples_per_ms);
    let hard_correction_threshold =
        (adaptive_config.hard_correction_threshold_ms as usize).saturating_mul(samples_per_ms);
    let measurement_smoothing_alpha = adaptive_config.measurement_smoothing_alpha.clamp(0.0, 1.0);
    let hard_correction_release_margin = hard_correction_threshold / 2;
    let hard_correction_max_step = hard_correction_threshold / 2;

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
            callback_count += 1;

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

                    // Seek/decoder-reset flush: discard stale pre-seek audio and
                    // re-enter prefill so post-seek audio starts cleanly.
                    if flush_requested_for_callback.load(Ordering::Relaxed) {
                        while buffer_for_callback.pop().is_some() {}
                        playback_state = PlaybackState::Prefill;
                        prefill_stable_callbacks = 0;
                        last_prefill_available = 0;
                        controller_state = AdaptiveControllerState::default();
                        desired_rate_for_callback.store(1.0f32.to_bits(), Ordering::Relaxed);
                        rate_adjust_for_callback.store(1.0f32.to_bits(), Ordering::Relaxed);
                        underrun_warned = false;
                        output_fifo.clear();
                        input_frames_collected = 0;
                        flush_requested_for_callback.store(false, Ordering::Relaxed);
                        control_latency_for_callback.store(0u32, Ordering::Relaxed);
                        log::info!("PipeWire: seek flush complete, waiting for buffer prefill");
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
                    // The callback observes the ring before consuming this PipeWire block.
                    // For latency control, use a midpoint estimate so the controller tracks
                    // roughly the same delay the UI/user perceives during the block.
                    let control_available =
                        available.saturating_sub(frame_aligned_max / 2);
                    let total_available = available.saturating_add(output_fifo.len());
                    let smoothed_control_available = apply_ema(
                        &mut controller_state.smoothed_control_available,
                        control_available as f64,
                        measurement_smoothing_alpha,
                    )
                    .max(0.0)
                    .round() as usize;
                    let smoothed_total_available = apply_ema(
                        &mut controller_state.smoothed_total_available,
                        total_available as f64,
                        measurement_smoothing_alpha,
                    )
                    .max(0.0)
                    .round() as usize;
                    let control_latency_ms = (smoothed_control_available as f32
                        / channel_count as f32
                        / sample_rate as f32)
                        * 1000.0
                        + f32::from_bits(graph_latency_for_callback.load(Ordering::Relaxed));
                    control_latency_for_callback.store(control_latency_ms.to_bits(), Ordering::Relaxed);
                    let start_margin = frame_aligned_max / 2;
                    let start_threshold = runtime_target_buffer_fill
                        .saturating_add(start_margin)
                        .min(max_buffer_fill);
                    let desired_start_fill = start_threshold;
                    let is_prefill_ready = available >= start_threshold;
                    let is_prefill_stable = available >= last_prefill_available.saturating_sub(frame_aligned_max / 2);

                    if playback_state == PlaybackState::Prefill {
                        if is_prefill_ready && is_prefill_stable {
                            prefill_stable_callbacks = prefill_stable_callbacks.saturating_add(1);
                        } else {
                            prefill_stable_callbacks = 0;
                        }
                        last_prefill_available = available;
                    }

                    let waiting_for_prefill = playback_state == PlaybackState::Prefill
                        && prefill_stable_callbacks < 2;

                    // Output silence while prefilling to the target.
                    if waiting_for_prefill {
                        for i in 0..frame_aligned_max {
                            dest[i] = 0.0;
                        }
                        frame_aligned_max
                    } else {
                        if playback_state != PlaybackState::Running {
                            let excess_samples = available.saturating_sub(desired_start_fill);
                            let excess_frames = excess_samples / ch;
                            let trimmed_samples = excess_frames * ch;
                            if trimmed_samples > 0 {
                                for _ in 0..trimmed_samples {
                                    let _ = buffer_for_callback.pop();
                                }
                            }
                            let start_fill = available.saturating_sub(trimmed_samples);
                            playback_state = PlaybackState::Running;
                            prefill_stable_callbacks = 0;
                            if trimmed_samples > 0 {
                                log::info!(
                                    "Starting PipeWire playback ({} samples in buffer, trimmed {} excess samples)",
                                    start_fill,
                                    trimmed_samples
                                );
                            } else {
                                log::info!("Starting PipeWire playback ({} samples in buffer)", start_fill);
                            }
                        }

                        let mut hard_zero_fill = false;
                        if adaptive_resampling_enabled && hard_correction_threshold > 0 {
                            if hard_correction_mode < 0 {
                                if smoothed_control_available.saturating_add(hard_correction_release_margin)
                                    >= runtime_target_buffer_fill
                                {
                                    hard_correction_mode = 0;
                                } else {
                                    controller_state.accumulated_drift = 0.0;
                                    rate_adjust_for_callback
                                        .store(1.0f32.to_bits(), Ordering::Relaxed);
                                    desired_rate_for_callback
                                        .store(1.0f32.to_bits(), Ordering::Relaxed);
                                    current_adaptive_band.store(ADAPTIVE_BAND_HARD, Ordering::Relaxed);
                                    hard_zero_fill = true;
                                }
                            } else if hard_correction_mode > 0 {
                                let desired_keep = runtime_target_buffer_fill
                                    .saturating_add(hard_correction_release_margin);
                                if smoothed_total_available <= desired_keep {
                                    hard_correction_mode = 0;
                                } else {
                                    let mut to_drop =
                                        smoothed_total_available.saturating_sub(desired_keep);
                                    to_drop = to_drop.min(hard_correction_max_step.max(ch));
                                    let fifo_drop = to_drop.min(output_fifo.len());
                                    if fifo_drop > 0 {
                                        output_fifo.drain(0..fifo_drop);
                                        to_drop = to_drop.saturating_sub(fifo_drop);
                                    }
                                    let ring_drop = (to_drop / ch) * ch;
                                    for _ in 0..ring_drop {
                                        let _ = buffer_for_callback.pop();
                                    }
                                    controller_state.accumulated_drift = 0.0;
                                    current_adaptive_band.store(ADAPTIVE_BAND_HARD, Ordering::Relaxed);
                                    log::debug!(
                                        "PipeWire hard correction: overfill total={} target={} threshold={} -> drop fifo={} ring={}",
                                        smoothed_total_available,
                                        runtime_target_buffer_fill,
                                        hard_correction_threshold,
                                        fifo_drop,
                                        ring_drop
                                    );
                                }
                            } else if smoothed_control_available
                                .saturating_add(hard_correction_threshold)
                                < runtime_target_buffer_fill
                            {
                                hard_correction_mode = -1;
                                controller_state.accumulated_drift = 0.0;
                                rate_adjust_for_callback
                                    .store(1.0f32.to_bits(), Ordering::Relaxed);
                                desired_rate_for_callback
                                    .store(1.0f32.to_bits(), Ordering::Relaxed);
                                current_adaptive_band.store(ADAPTIVE_BAND_HARD, Ordering::Relaxed);
                                hard_zero_fill = true;
                                log::debug!(
                                    "PipeWire hard correction: underfill buf={} target={} threshold={} -> zero-fill",
                                    smoothed_control_available,
                                    runtime_target_buffer_fill,
                                    hard_correction_threshold
                                );
                            } else if smoothed_total_available
                                > runtime_target_buffer_fill
                                    .saturating_add(hard_correction_threshold)
                            {
                                hard_correction_mode = 1;
                                let mut to_drop =
                                    smoothed_total_available.saturating_sub(
                                        runtime_target_buffer_fill
                                            .saturating_add(hard_correction_release_margin),
                                    );
                                to_drop = to_drop.min(hard_correction_max_step.max(ch));
                                let fifo_drop = to_drop.min(output_fifo.len());
                                if fifo_drop > 0 {
                                    output_fifo.drain(0..fifo_drop);
                                    to_drop = to_drop.saturating_sub(fifo_drop);
                                }
                                let ring_drop = (to_drop / ch) * ch;
                                for _ in 0..ring_drop {
                                    let _ = buffer_for_callback.pop();
                                }
                                controller_state.accumulated_drift = 0.0;
                                current_adaptive_band.store(ADAPTIVE_BAND_HARD, Ordering::Relaxed);
                                    log::debug!(
                                        "PipeWire hard correction: overfill total={} target={} threshold={} -> drop fifo={} ring={}",
                                        smoothed_total_available,
                                        runtime_target_buffer_fill,
                                        hard_correction_threshold,
                                        fifo_drop,
                                    ring_drop
                                );
                            }
                        }

                        if hard_zero_fill {
                            for i in 0..frame_aligned_max {
                                dest[i] = 0.0;
                            }
                            frame_aligned_max
                        } else if let Some(ref mut resampler) = resampler_opt {
                            // Adaptive rate matching with resampler
                            if adaptive_resampling_enabled && callback_count % 10 == 0 {
                                let step = compute_adaptive_step(
                                    &mut controller_state,
                                    &adaptive_config,
                                    smoothed_control_available,
                                    runtime_target_buffer_fill,
                                    fast_catchup_threshold,
                                    resample_ratio,
                                    480,
                                    MAX_INTEGRAL_TERM,
                                );
                                current_adaptive_band.store(step.band, Ordering::Relaxed);

                                if let Err(e) = resampler.set_resample_ratio(step.current_ratio, true) as Result<(), rubato::ResampleError> {
                                    log::warn!("Failed to set resampler ratio: {}", e);
                                }

                                // Store INPUT consumption adjust factor for OSC monitoring.
                                let effective_adjust = step.consume_adjust as f32;
                                rate_adjust_for_callback.store(effective_adjust.to_bits(), Ordering::Relaxed);

                                if callback_count % 100 == 0 {
                                    log::debug!(
                                        "PipeWire Adaptive: buf={}/{} drift={} ratio={:.6} (base={:.2} P={:.6} I={:.6})",
                                        smoothed_control_available,
                                        runtime_target_buffer_fill,
                                        step.drift,
                                        step.current_ratio,
                                        resample_ratio,
                                        step.p_term,
                                        step.i_term
                                    );
                                }
                            }

                            // Feed resampler until output_fifo has enough data
                            let audio_samples_needed = max_samples;

                            while output_fifo.len() < audio_samples_needed {
                                // Fill input buffer
                                while input_frames_collected < RESAMPLER_CHUNK_SIZE {
                                    let mut frame_complete = true;
                                    if buffer_for_callback.len() >= channel_count as usize {
                                        for ch in 0..channel_count as usize {
                                            if let Some(sample_f32) = buffer_for_callback.pop() {
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
                                        break;
                                    }
                                }

                                // Run resampler if input is full
                                if input_frames_collected == RESAMPLER_CHUNK_SIZE {
                                    match resampler.process(&resampler_input, None) {
                                        Ok(output_planar) => {
                                            let output_frames = output_planar[0].len();
                                            for i in 0..output_frames {
                                                for ch in 0..channel_count as usize {
                                                    output_fifo.push(output_planar[ch][i]);
                                                }
                                            }
                                            input_frames_collected = 0;
                                        },
                                        Err(e) => {
                                            log::error!("Resampler error: {}", e);
                                            break;
                                        }
                                    }
                                } else {
                                    break;
                                }
                            }

                            // Fill output buffer from FIFO
                            if output_fifo.len() >= audio_samples_needed {
                                // Copy f32 samples directly from FIFO to PipeWire
                                for i in 0..audio_samples_needed {
                                    dest[i] = output_fifo[i];
                                }
                                output_fifo.drain(0..audio_samples_needed);
                            } else {
                                // Underrun: not enough data in output_fifo
                                if !underrun_warned {
                                    log::warn!("Resampler underrun: only {} samples in FIFO, needed {}", output_fifo.len(), audio_samples_needed);
                                    underrun_warned = true;
                                }
                                for i in 0..max_samples {
                                    dest[i] = 0.0;
                                }
                            }
                            max_samples
                        } else {
                            // Path 2: No resampling (direct copy with optional rate control via PipeWire)
                            // Adaptive rate matching via PipeWire rate control
                            if latency_servo_enabled && callback_count % 10 == 0 {
                                let drift =
                                    smoothed_control_available as i64 - runtime_target_buffer_fill as i64;

                                if drift.abs() > 480 {
                                    controller_state.accumulated_drift += drift as f64;
                                    let i_gain = if adaptive_resampling_enabled {
                                        adaptive_config.ki
                                    } else {
                                        LATENCY_SERVO_I_GAIN
                                    };
                                    let integral_contribution = controller_state.accumulated_drift * i_gain;
                                    if integral_contribution.abs() > MAX_INTEGRAL_TERM {
                                        controller_state.accumulated_drift =
                                            (MAX_INTEGRAL_TERM / i_gain) * integral_contribution.signum();
                                    }
                                }

                                let is_far = adaptive_resampling_enabled
                                    && (drift.unsigned_abs() as usize) > fast_catchup_threshold;
                                if adaptive_resampling_enabled {
                                    current_adaptive_band.store(if is_far { ADAPTIVE_BAND_FAR } else { ADAPTIVE_BAND_NEAR }, Ordering::Relaxed);
                                } else {
                                    current_adaptive_band.store(0, Ordering::Relaxed);
                                }
                                let p_gain = if adaptive_resampling_enabled {
                                    if is_far {
                                        adaptive_config.kp_far
                                    } else {
                                        adaptive_config.kp_near
                                    }
                                } else {
                                    LATENCY_SERVO_P_GAIN
                                };
                                let i_gain = if adaptive_resampling_enabled {
                                    adaptive_config.ki
                                } else {
                                    LATENCY_SERVO_I_GAIN
                                };
                                let p_term = drift as f64 * p_gain / 100.0;
                                let i_term = controller_state.accumulated_drift * i_gain;
                                // >1.0 means "consume input faster".
                                let max_adjust = if adaptive_resampling_enabled {
                                    if is_far {
                                        adaptive_config.max_adjust_far
                                    } else {
                                        adaptive_config.max_adjust
                                    }
                                    .max(0.000001)
                                } else {
                                    LATENCY_SERVO_MAX_RATE_ADJUST
                                };
                                let consume_adjust =
                                    (1.0 + p_term + i_term).clamp(1.0 - max_adjust, 1.0 + max_adjust);
                                // SPA_PROP_RATE follows output/input semantics too:
                                // to consume input faster, lower the ratio.
                                let pipewire_rate = (1.0 / consume_adjust) as f32;

                                rate_adjust_for_callback
                                    .store((consume_adjust as f32).to_bits(), Ordering::Relaxed);
                                desired_rate_for_callback.store(pipewire_rate.to_bits(), Ordering::Relaxed);

                                if callback_count % 100 == 0 {
                                    if adaptive_resampling_enabled {
                                        log::debug!(
                                            "Adaptive rate: buf={} target={} max={} drift={} (P={:.6} I={:.6}) -> consume={:.6} pw_rate={:.6}",
                                            smoothed_control_available,
                                            runtime_target_buffer_fill,
                                            max_buffer_fill,
                                            drift,
                                            p_term,
                                            i_term,
                                            consume_adjust,
                                            pipewire_rate
                                        );
                                    } else {
                                        log::debug!(
                                            "PipeWire latency servo: buf={} target={} max={} drift={} -> consume={:.6} pw_rate={:.6}",
                                            smoothed_control_available,
                                            runtime_target_buffer_fill,
                                            max_buffer_fill,
                                            drift,
                                            consume_adjust,
                                            pipewire_rate
                                        );
                                    }
                                }
                            }

                            // Read only complete frames to maintain channel alignment
                            let available_frames = available / ch;
                            let frames_to_read = available_frames.min(max_frames);
                            let samples_to_read = frames_to_read * ch;

                            let mut count = 0;
                            while count < samples_to_read {
                                if let Some(sample_f32) = buffer_for_callback.pop() {
                                    dest[count] = sample_f32;
                                    count += 1;
                                } else {
                                    break;
                                }
                            }

                            // Zero-pad the remainder of the buffer
                            while count < max_samples {
                                dest[count] = 0.0;
                                count += 1;
                            }

                            if samples_to_read < frame_aligned_max && !underrun_warned {
                                log::warn!(
                                    "Buffer underrun: only {} samples available, needed {}",
                                    available,
                                    frame_aligned_max
                                );
                                underrun_warned = true;
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

            // Only apply if significantly different from last applied
            if (desired_rate_value - last_applied_rate).abs() > 0.0001 {
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
