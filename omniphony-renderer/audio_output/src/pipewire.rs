#![cfg(target_os = "linux")]

use anyhow::{Result, anyhow};
use crossbeam::queue::ArrayQueue;
use parking_lot::Mutex;
use pipewire as pw;
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicI64, AtomicU8, AtomicU32, AtomicU64, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{
    ADAPTIVE_BAND_FAR, AdaptiveResamplingConfig, LOCAL_RESAMPLER_MAX_RELATIVE_RATIO,
    adaptive_runtime::{
        AdaptiveRuntimeState, FarModeStepCtx, FarModeStepInputs, LatencyMetricTargets,
        LowRecoverPhase, MAX_INTEGRAL_TERM, PRE_BRIDGE_CALIBRATION_CALLBACKS,
        compute_hard_recover_high_plan, discard_ring_samples, far_mode_band_from_latency,
        far_mode_step, note_refill_or_underrun, output_to_input_domain_samples, paused_rate_adjust,
        postprocess_interleaved_output, reset_adaptive_runtime, run_adaptive_servo,
        should_run_adaptive_servo, update_latency_metrics, zero_pad_tail,
    },
    adaptive_runtime_state_name_from_code, clamp_ratio_for_local_resampler,
    local_resampler_ratio_bounds,
    resampler_fifo::{RESAMPLER_CHUNK_SIZE, ResamplerFifoEngine},
    ring_buffer_io::{
        flush_ring_buffer, push_samples_drop_overflow, push_samples_with_backpressure,
    },
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
    /// Post-rendering pacer FIFO. When `pacer_enabled` is true,
    /// `write_samples` pushes here instead of directly to `sample_buffer`;
    /// the PipeWire INPUT thread drains this FIFO into `sample_buffer` at
    /// a cadence that mirrors the IEC958 chunk arrival rate, so the ring
    /// buffer the DAC consumes sees a smooth flow regardless of the
    /// decoder's burst pattern.
    pacer_fifo: Arc<ArrayQueue<f32>>,
    pacer_enabled: bool,
    /// When true, `write_samples` pushes without ever blocking and drops the
    /// overflow above the back-pressure threshold instead of waiting for the
    /// DAC to drain. Decouples the producer from the consumer clock.
    backpressure_disabled: Arc<AtomicBool>,
    pacer_pre_roll_complete: Arc<AtomicBool>,
    pacer_flush_requested: Arc<AtomicBool>,
    pacer_pre_roll_threshold_samples: usize,
    pacer_drain_total: Arc<AtomicU64>,
    pacer_underrun_total: Arc<AtomicU64>,
    pacer_fifo_level: Arc<AtomicU64>,
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
    /// EMA-smoothed control latency in ms bits — the value the servo actually tracks.
    smoothed_control_latency_ms_bits: Arc<AtomicU32>,
    /// Ring-buffer level converted to ms — first of the three control-available components.
    avail_input_latency_ms_bits: Arc<AtomicU32>,
    /// Output FIFO (resampler output) converted back to input-domain ms — second component.
    output_fifo_latency_ms_bits: Arc<AtomicU32>,
    /// Pending samples inside the local resampler input — third component.
    resampler_pending_latency_ms_bits: Arc<AtomicU32>,
    /// Cumulative input-domain samples written via `write_samples`. Paired
    /// with `cumulative_drained_input_samples` below to give the PI a
    /// chunk-noise-free `control_available`: the difference (written -
    /// drained) is the true "samples in flight" between writer and the
    /// resampler's consumption point, with no chunk granularity artefacts.
    cumulative_written_input_samples: Arc<std::sync::atomic::AtomicU64>,
    /// Cumulative samples drained from the input ring per output callback
    /// (in input domain: callback_output_frames × channels / ratio). The
    /// counter is incremented continuously (not at chunk boundaries) so
    /// the running difference with `cumulative_written_input_samples` is
    /// smooth. Owned here to keep the Arc alive; the callback thread has
    /// its own clone for the fetch_add.
    #[allow(dead_code)]
    cumulative_drained_input_samples: Arc<std::sync::atomic::AtomicU64>,
    /// Diagnostic: the cumulative-flow control_available value (written -
    /// drained) actually fed to the PI. Exposed here for direct comparison
    /// with `output_ring_input_samples` (raw ring level) — if both show the
    /// same sawtooth, the flow-counter approach hasn't suppressed it.
    cumulative_flow_control_available_bits: Arc<std::sync::atomic::AtomicU64>,
    /// Diagnostic atomics published by the PipeWire output process callback.
    /// `f64` bits stored each callback; lets the Studio diag plot trace the
    /// drain cadence (interval between callbacks) to localise ring-level
    /// oscillations whose origin is downstream of the input/decoder/write
    /// chain.
    output_callback_dt_us_bits: Arc<std::sync::atomic::AtomicU64>,
    /// FIFO level between local resampler and PipeWire (input-domain
    /// samples). Post-d43ddab the callback drains only ~21 ms per cycle so
    /// this FIFO is no longer fully drained per call and can itself
    /// oscillate — strong candidate for the residual DAC sawtooth.
    output_fifo_input_domain_samples_bits: Arc<std::sync::atomic::AtomicU64>,
    /// Pending input samples held inside the resampler (input-domain).
    /// Complements the FIFO metric: any oscillation here can manifest as
    /// latency wobble downstream.
    output_resampler_pending_input_samples_bits: Arc<std::sync::atomic::AtomicU64>,
    /// Latency signals duplicated as f64-encoded u64 atomics so they can
    /// be selected in the generic diag plot alongside other metrics. The
    /// `*_ms_bits: AtomicU32` set above is kept untouched — it feeds the
    /// dedicated latency snapshot / OSC pipeline.
    diag_latency_smoothed_ms_bits: Arc<std::sync::atomic::AtomicU64>,
    diag_latency_control_ms_bits: Arc<std::sync::atomic::AtomicU64>,
    diag_rate_adjust_ppm_bits: Arc<std::sync::atomic::AtomicU64>,
    /// f64-encoded mirrors of the three `control_available` components in ms
    /// (ring / output-FIFO / resampler-pending). Same values fed to the
    /// latency OSC path, exposed here so they are selectable in the generic
    /// diag plot instead of needing a separate components plot.
    diag_latency_avail_input_ms_bits: Arc<std::sync::atomic::AtomicU64>,
    diag_latency_output_fifo_ms_bits: Arc<std::sync::atomic::AtomicU64>,
    diag_latency_resampler_pending_ms_bits: Arc<std::sync::atomic::AtomicU64>,
    /// Effective resample ratio expressed as ppm deviation from 1.0
    /// (i.e. (ratio - 1.0) × 1e6). Stays constant when the PI is paused;
    /// any modulation here under pause indicates a bug in the ratio
    /// freeze path. f64 bits.
    output_effective_ratio_ppm_bits: Arc<std::sync::atomic::AtomicU64>,
    /// Raw input ring-buffer length at callback entry, in samples. Same
    /// quantity as `avail_input_latency_ms_bits` but published from the
    /// callback at native cadence (not converted to ms, not sampled at
    /// send_meter_bundle). Lets us cross-check whether the 1 Hz on the
    /// components plot is real or a sampling/aliasing artefact.
    output_ring_input_samples_bits: Arc<std::sync::atomic::AtomicU64>,
    /// Current adaptive runtime state code (0=stable, 1=low-recover,
    /// 2=settling, 3=high-recover). Surfaces here as f64 bits so the diag
    /// plot can chart state transitions over time — if the state changes
    /// periodically while PI is paused, recovery is the source of any
    /// ring-level oscillation.
    runtime_state_code_bits: Arc<std::sync::atomic::AtomicU64>,
    /// Monotonic counter of ring samples discarded by ANY recovery path
    /// (low_recover_trim, hard_recover_high, recovery_reacquire_pending).
    /// Plot as a counter: a non-flat slope means recovery is firing.
    recovery_discard_count_bits: Arc<std::sync::atomic::AtomicU64>,
    /// Configured ring-buffer target latency (from PipewireBufferConfig::latency_ms).
    target_latency_ms: u32,
    live_adaptive_config: Arc<Mutex<AdaptiveResamplingConfig>>,
    reset_ratio_requested: Arc<AtomicBool>,
    pw_thread: Option<thread::JoinHandle<()>>,
    bootstrap_started_at: Instant,
    bootstrap_write_calls: u32,
    bootstrap_written_samples: usize,
    /// Direct trigger mode: pending trigger counter shared with the capture mainloop.
    /// The output process callback increments this (Bresenham); the capture mainloop drains it
    /// and calls pw_stream_trigger_process() from its own thread (required for correct operation).
    pending_input_triggers: Arc<AtomicI64>,
    /// Sample rate of the capture stream, used to compute the Bresenham trigger ratio.
    input_trigger_rate_hz: Arc<AtomicU32>,
    /// Observed capture quantum in transport frames. Combined with input_trigger_rate_hz so
    /// direct trigger mode follows real audio duration instead of callback count alone.
    input_trigger_quantum_frames: Arc<AtomicU32>,
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
        input_clock_us: Arc<std::sync::atomic::AtomicU64>,
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
            input_clock_us,
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
        input_clock_us: Arc<std::sync::atomic::AtomicU64>,
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
        // Pacer FIFO: capacity matches the ring so worst-case can buffer
        // the same amount of audio. It only fills meaningfully when the
        // input-thread drain lags or is paused (eg. during pre-roll).
        let pacer_fifo = Arc::new(ArrayQueue::new(BUFFER_SIZE));
        let pacer_enabled = adaptive_config.use_output_pacing;
        let backpressure_disabled = Arc::new(AtomicBool::new(adaptive_config.disable_backpressure));
        let pacer_pre_roll_complete = Arc::new(AtomicBool::new(false));
        let pacer_flush_requested = Arc::new(AtomicBool::new(false));
        // 64 ms of audio at the output rate × channel count. Covers >1 AU
        // for both supported input codecs (~32 ms per AU) with margin.
        let pacer_pre_roll_threshold_samples =
            ((sample_rate as usize) * (channel_count as usize) * 64 / 1000).max(1);
        let pacer_drain_total = Arc::new(AtomicU64::new(0));
        let pacer_underrun_total = Arc::new(AtomicU64::new(0));
        let pacer_fifo_level = Arc::new(AtomicU64::new(0));
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
        let smoothed_control_latency_ms_bits = Arc::new(AtomicU32::new(0u32));
        let smoothed_control_latency_clone = smoothed_control_latency_ms_bits.clone();
        let avail_input_latency_ms_bits = Arc::new(AtomicU32::new(0u32));
        let avail_input_latency_clone = avail_input_latency_ms_bits.clone();
        let output_fifo_latency_ms_bits = Arc::new(AtomicU32::new(0u32));
        let output_fifo_latency_clone = output_fifo_latency_ms_bits.clone();
        let resampler_pending_latency_ms_bits = Arc::new(AtomicU32::new(0u32));
        let resampler_pending_latency_clone = resampler_pending_latency_ms_bits.clone();
        let cumulative_written_input_samples = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let cumulative_drained_input_samples = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let cumulative_drained_input_samples_clone = cumulative_drained_input_samples.clone();
        let cumulative_written_input_samples_clone = cumulative_written_input_samples.clone();
        let cumulative_flow_control_available_bits = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let cumulative_flow_control_available_clone =
            cumulative_flow_control_available_bits.clone();
        let output_callback_dt_us_bits = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let output_callback_dt_us_clone = output_callback_dt_us_bits.clone();
        let output_fifo_input_domain_samples_bits = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let output_fifo_input_domain_samples_clone = output_fifo_input_domain_samples_bits.clone();
        let output_resampler_pending_input_samples_bits =
            Arc::new(std::sync::atomic::AtomicU64::new(0));
        let output_resampler_pending_input_samples_clone =
            output_resampler_pending_input_samples_bits.clone();
        let diag_latency_smoothed_ms_bits = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let diag_latency_smoothed_ms_clone = diag_latency_smoothed_ms_bits.clone();
        let diag_latency_control_ms_bits = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let diag_latency_control_ms_clone = diag_latency_control_ms_bits.clone();
        let diag_rate_adjust_ppm_bits = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let diag_rate_adjust_ppm_clone = diag_rate_adjust_ppm_bits.clone();
        let diag_latency_avail_input_ms_bits = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let diag_latency_avail_input_ms_clone = diag_latency_avail_input_ms_bits.clone();
        let diag_latency_output_fifo_ms_bits = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let diag_latency_output_fifo_ms_clone = diag_latency_output_fifo_ms_bits.clone();
        let diag_latency_resampler_pending_ms_bits = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let diag_latency_resampler_pending_ms_clone =
            diag_latency_resampler_pending_ms_bits.clone();
        let output_effective_ratio_ppm_bits = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let output_effective_ratio_ppm_clone = output_effective_ratio_ppm_bits.clone();
        let output_ring_input_samples_bits = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let output_ring_input_samples_clone = output_ring_input_samples_bits.clone();
        let runtime_state_code_bits = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let runtime_state_code_clone = runtime_state_code_bits.clone();
        let recovery_discard_count_bits = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let recovery_discard_count_clone = recovery_discard_count_bits.clone();
        let live_config = Arc::new(Mutex::new(adaptive_config));
        let adaptive_config_for_thread = Arc::clone(&live_config);
        let reset_ratio_requested = Arc::new(AtomicBool::new(false));
        let reset_ratio_for_thread = Arc::clone(&reset_ratio_requested);
        let pending_input_triggers = Arc::new(AtomicI64::new(0));
        let pending_input_triggers_for_thread = Arc::clone(&pending_input_triggers);
        let input_trigger_rate_hz = Arc::new(AtomicU32::new(0));
        let input_trigger_rate_for_thread = Arc::clone(&input_trigger_rate_hz);
        let input_trigger_quantum_frames = Arc::new(AtomicU32::new(0));
        let input_trigger_quantum_for_thread = Arc::clone(&input_trigger_quantum_frames);
        // Pacer atomics cloned into the PipeWire thread so the DAC callback
        // can flush the FIFO and re-arm pre-roll on recovery_reacquire.
        let pacer_pre_roll_complete_for_thread = Arc::clone(&pacer_pre_roll_complete);
        let pacer_flush_requested_for_thread = Arc::clone(&pacer_flush_requested);
        let pacer_enabled_for_thread = pacer_enabled;
        let pacer_pre_roll_threshold_for_thread = pacer_pre_roll_threshold_samples;

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
                smoothed_control_latency_clone,
                avail_input_latency_clone,
                output_fifo_latency_clone,
                resampler_pending_latency_clone,
                cumulative_written_input_samples_clone,
                cumulative_drained_input_samples_clone,
                cumulative_flow_control_available_clone,
                output_callback_dt_us_clone,
                output_fifo_input_domain_samples_clone,
                output_resampler_pending_input_samples_clone,
                diag_latency_smoothed_ms_clone,
                diag_latency_control_ms_clone,
                diag_rate_adjust_ppm_clone,
                diag_latency_avail_input_ms_clone,
                diag_latency_output_fifo_ms_clone,
                diag_latency_resampler_pending_ms_clone,
                output_effective_ratio_ppm_clone,
                output_ring_input_samples_clone,
                runtime_state_code_clone,
                recovery_discard_count_clone,
                pending_input_triggers_for_thread,
                input_trigger_rate_for_thread,
                input_trigger_quantum_for_thread,
                input_clock_us,
                pacer_pre_roll_complete_for_thread,
                pacer_flush_requested_for_thread,
                pacer_enabled_for_thread,
                pacer_pre_roll_threshold_for_thread,
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
            pacer_fifo,
            pacer_enabled,
            backpressure_disabled,
            pacer_pre_roll_complete,
            pacer_flush_requested,
            pacer_pre_roll_threshold_samples,
            pacer_drain_total,
            pacer_underrun_total,
            pacer_fifo_level,
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
            smoothed_control_latency_ms_bits,
            avail_input_latency_ms_bits,
            output_fifo_latency_ms_bits,
            resampler_pending_latency_ms_bits,
            cumulative_written_input_samples,
            cumulative_drained_input_samples,
            cumulative_flow_control_available_bits,
            output_callback_dt_us_bits,
            output_fifo_input_domain_samples_bits,
            output_resampler_pending_input_samples_bits,
            diag_latency_smoothed_ms_bits,
            diag_latency_control_ms_bits,
            diag_rate_adjust_ppm_bits,
            diag_latency_avail_input_ms_bits,
            diag_latency_output_fifo_ms_bits,
            diag_latency_resampler_pending_ms_bits,
            output_effective_ratio_ppm_bits,
            output_ring_input_samples_bits,
            runtime_state_code_bits,
            recovery_discard_count_bits,
            target_latency_ms,
            live_adaptive_config: live_config,
            reset_ratio_requested,
            pw_thread: Some(pw_thread),
            bootstrap_started_at: Instant::now(),
            bootstrap_write_calls: 0,
            bootstrap_written_samples: 0,
            pending_input_triggers,
            input_trigger_rate_hz,
            input_trigger_quantum_frames,
        })
    }

    pub fn write_samples(&mut self, samples: &[f32]) -> Result<()> {
        // Cumulative-flow counter: increment by what we're ABOUT to push,
        // not by what the back-pressure-aware push will actually accept.
        // This matches the PI's mental model of "samples committed to the
        // pipeline" — back-pressure drops are exceptional and would show
        // up as a separate divergence anyway.
        self.cumulative_written_input_samples
            .fetch_add(samples.len() as u64, Ordering::Relaxed);
        // Check if stream is ready
        if !self.stream_ready.load(Ordering::Relaxed) {
            log::trace!("Stream not ready yet, dropping {} samples", samples.len());
            return Ok(());
        }

        // Route to pacer when enabled. The pacer FIFO is drained into the
        // ring by the PipeWire input thread (in lockstep with IEC958 chunk
        // arrival), so the ring sees a smooth flow that's decoupled from
        // the decoder's burst pattern. When routing through the pacer we
        // also clamp the pacer's depth to its pre-roll capacity via
        // backpressure on the renderer push: this guarantees the pacer
        // contributes a *fixed* latency (= pre_roll_threshold) instead of
        // drifting up to seconds of buffered audio.
        let pacer_active = self.pacer_enabled;
        let target_buffer: &Arc<ArrayQueue<f32>> = if pacer_active {
            &self.pacer_fifo
        } else {
            &self.sample_buffer
        };
        let max_buffer_fill = if pacer_active {
            self.pacer_pre_roll_threshold_samples
        } else {
            self.max_buffer_samples
        };
        let buffer_before = self.sample_buffer.len();
        // Back-pressure disabled (diagnostic): never block the renderer; push
        // what fits below the threshold and drop the overflow. This unhooks the
        // producer from the DAC drain clock so the source (mpv) free-runs.
        let report = if self.backpressure_disabled.load(Ordering::Relaxed) {
            push_samples_drop_overflow(target_buffer, samples, max_buffer_fill)
        } else {
            push_samples_with_backpressure(target_buffer, samples, max_buffer_fill, 10, 200)
        };
        if report.timed_out {
            log::warn!(
                "Buffer drain timeout after 2s - dropping {} remaining samples to prevent OOM (target={})",
                samples.len().saturating_sub(report.pushed_samples),
                if pacer_active { "pacer_fifo" } else { "ring" }
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
            log::debug!(
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
            log::warn!(
                "Flush timeout - {} samples remaining",
                report.remaining_samples
            );
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

    /// Set the capture sample rate for the Bresenham trigger ratio.
    /// Once set, the output RT callback increments pending_input_triggers() per output callback.
    pub fn set_input_trigger_rate_hz(&self, rate_hz: u32) {
        self.input_trigger_rate_hz.store(rate_hz, Ordering::Relaxed);
    }

    /// Set the observed capture quantum in transport frames for direct-trigger scheduling.
    pub fn set_input_trigger_quantum_frames(&self, quantum_frames: u32) {
        self.input_trigger_quantum_frames
            .store(quantum_frames, Ordering::Relaxed);
    }

    /// Returns the Arc that the output RT callback increments (Bresenham).
    /// Pass this to InputControl.set_pending_input_triggers() so the capture mainloop can drain it.
    pub fn pending_input_triggers(&self) -> Arc<AtomicI64> {
        Arc::clone(&self.pending_input_triggers)
    }

    /// Hand a cross-crate handle to the post-rendering pacer to the audio
    /// input layer (via `InputControl::install_output_pacer`). The input
    /// PwStream callback uses this to drain the FIFO into the ring buffer
    /// in lockstep with IEC958 chunk arrival.
    pub fn pacer_handle(&self) -> crate::pacer::PacerHandle {
        crate::pacer::PacerHandle {
            pacer_fifo: Arc::clone(&self.pacer_fifo),
            ring: Arc::clone(&self.sample_buffer),
            pre_roll_complete: Arc::clone(&self.pacer_pre_roll_complete),
            flush_requested: Arc::clone(&self.pacer_flush_requested),
            pre_roll_threshold_samples: self.pacer_pre_roll_threshold_samples,
            out_sample_rate: self.sample_rate,
            out_channels: self.channel_count,
            enabled: self.pacer_enabled,
            diag_drain_total: Arc::clone(&self.pacer_drain_total),
            diag_underrun_total: Arc::clone(&self.pacer_underrun_total),
            diag_fifo_level: Arc::clone(&self.pacer_fifo_level),
        }
    }

    /// Hot-swap the back-pressure disable flag. Called when
    /// `AdaptiveResamplingConfig::disable_backpressure` flips via OSC.
    pub fn set_backpressure_disabled(&self, disabled: bool) {
        self.backpressure_disabled
            .store(disabled, Ordering::Relaxed);
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

    pub fn target_control_latency_ms(&self) -> f32 {
        self.target_latency_ms as f32
    }

    /// Measured total audio delay seen by the listener:
    /// current ring-buffer latency + PipeWire graph latency.
    pub fn measured_audio_delay_ms(&self) -> f32 {
        f32::from_bits(self.measured_latency_ms_bits.load(Ordering::Relaxed))
    }

    pub fn control_audio_delay_ms(&self) -> f32 {
        f32::from_bits(self.control_latency_ms_bits.load(Ordering::Relaxed))
    }

    /// EMA-smoothed control latency in ms (the value the servo actually tracks).
    pub fn smoothed_control_audio_delay_ms(&self) -> f32 {
        f32::from_bits(
            self.smoothed_control_latency_ms_bits
                .load(Ordering::Relaxed),
        )
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

    /// Update adaptive resampling tuning parameters without restarting the audio thread.
    pub fn update_adaptive_config(&self, config: AdaptiveResamplingConfig) {
        // Output pacing is NOT hot-swappable: it decides which thread produces
        // into the ring, and swapping producers under a running stream is what
        // the single-producer invariant forbids. Say so rather than silently
        // ignoring the request.
        if config.use_output_pacing != self.pacer_enabled {
            log::warn!(
                "Output pacing is fixed for the lifetime of the audio output                  (running with {}, requested {}); the change takes effect at the                  next output start.",
                self.pacer_enabled,
                config.use_output_pacing
            );
        }
        self.set_backpressure_disabled(config.disable_backpressure);
        *self.live_adaptive_config.lock() = config;
    }

    /// Diagnostic metric handles published by the PipeWire output backend.
    /// Each entry is registered in the global registry by the caller (one
    /// call to `DiagRegistry::register_external`); the backend updates the
    /// underlying atomics from the process callback. Adding a new metric in
    /// this list is the only change needed to surface it in the Studio diag
    /// plot — no other plumbing required.
    pub fn diag_atomic_handles(&self) -> Vec<sys::diag::DiagAtomicHandle> {
        vec![
            sys::diag::DiagAtomicHandle {
                name: "output_callback_dt_us",
                label: "Output callback dt",
                group: "output",
                unit: "us",
                atomic: Arc::clone(&self.output_callback_dt_us_bits),
            },
            sys::diag::DiagAtomicHandle {
                name: "output_fifo_input_domain_samples",
                label: "Output FIFO level (input-domain)",
                group: "output",
                unit: "samples",
                atomic: Arc::clone(&self.output_fifo_input_domain_samples_bits),
            },
            sys::diag::DiagAtomicHandle {
                name: "output_resampler_pending_input_samples",
                label: "Resampler pending input samples",
                group: "output",
                unit: "samples",
                atomic: Arc::clone(&self.output_resampler_pending_input_samples_bits),
            },
            sys::diag::DiagAtomicHandle {
                name: "latency_smoothed_ms",
                label: "Smoothed control latency",
                group: "latency",
                unit: "ms",
                atomic: Arc::clone(&self.diag_latency_smoothed_ms_bits),
            },
            sys::diag::DiagAtomicHandle {
                name: "latency_control_ms",
                label: "Control latency (raw)",
                group: "latency",
                unit: "ms",
                atomic: Arc::clone(&self.diag_latency_control_ms_bits),
            },
            sys::diag::DiagAtomicHandle {
                name: "rate_adjust_ppm",
                label: "Rate adjust",
                group: "latency",
                unit: "ppm",
                atomic: Arc::clone(&self.diag_rate_adjust_ppm_bits),
            },
            sys::diag::DiagAtomicHandle {
                name: "latency_avail_input_ms",
                label: "Avail input latency",
                group: "latency",
                unit: "ms",
                atomic: Arc::clone(&self.diag_latency_avail_input_ms_bits),
            },
            sys::diag::DiagAtomicHandle {
                name: "latency_output_fifo_ms",
                label: "Output FIFO latency",
                group: "latency",
                unit: "ms",
                atomic: Arc::clone(&self.diag_latency_output_fifo_ms_bits),
            },
            sys::diag::DiagAtomicHandle {
                name: "latency_resampler_pending_ms",
                label: "Resampler pending latency",
                group: "latency",
                unit: "ms",
                atomic: Arc::clone(&self.diag_latency_resampler_pending_ms_bits),
            },
            sys::diag::DiagAtomicHandle {
                name: "output_effective_ratio_ppm",
                label: "Effective ratio (ppm dev)",
                group: "output",
                unit: "ppm",
                atomic: Arc::clone(&self.output_effective_ratio_ppm_bits),
            },
            sys::diag::DiagAtomicHandle {
                name: "output_ring_input_samples",
                label: "Ring input level (native sampling)",
                group: "output",
                unit: "samples",
                atomic: Arc::clone(&self.output_ring_input_samples_bits),
            },
            sys::diag::DiagAtomicHandle {
                name: "cumulative_flow_control_available",
                label: "Cumulative-flow control_available (PI input)",
                group: "output",
                unit: "samples",
                atomic: Arc::clone(&self.cumulative_flow_control_available_bits),
            },
            sys::diag::DiagAtomicHandle {
                name: "pacer_fifo_level",
                label: "Pacer FIFO level",
                group: "output",
                unit: "samples",
                atomic: Arc::clone(&self.pacer_fifo_level),
            },
            sys::diag::DiagAtomicHandle {
                name: "pacer_drain_total",
                label: "Pacer drain cumulative",
                group: "output",
                unit: "samples",
                atomic: Arc::clone(&self.pacer_drain_total),
            },
            sys::diag::DiagAtomicHandle {
                name: "pacer_underrun_total",
                label: "Pacer underrun cumulative",
                group: "output",
                unit: "samples",
                atomic: Arc::clone(&self.pacer_underrun_total),
            },
            sys::diag::DiagAtomicHandle {
                name: "runtime_state_code",
                label: "Runtime state (0=stable,1=low,2=settle,3=high)",
                group: "output",
                unit: "",
                atomic: Arc::clone(&self.runtime_state_code_bits),
            },
            sys::diag::DiagAtomicHandle {
                name: "recovery_discard_count",
                label: "Cumulative samples discarded by recovery",
                group: "output",
                unit: "samples",
                atomic: Arc::clone(&self.recovery_discard_count_bits),
            },
        ]
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
    smoothed_control_latency_ms_out: Arc<AtomicU32>,
    avail_input_latency_ms_out: Arc<AtomicU32>,
    output_fifo_latency_ms_out: Arc<AtomicU32>,
    resampler_pending_latency_ms_out: Arc<AtomicU32>,
    cumulative_written_input_samples: Arc<std::sync::atomic::AtomicU64>,
    cumulative_drained_input_samples: Arc<std::sync::atomic::AtomicU64>,
    cumulative_flow_control_available_out: Arc<std::sync::atomic::AtomicU64>,
    output_callback_dt_us_out: Arc<std::sync::atomic::AtomicU64>,
    output_fifo_input_domain_samples_out: Arc<std::sync::atomic::AtomicU64>,
    output_resampler_pending_input_samples_out: Arc<std::sync::atomic::AtomicU64>,
    diag_latency_smoothed_ms_out: Arc<std::sync::atomic::AtomicU64>,
    diag_latency_control_ms_out: Arc<std::sync::atomic::AtomicU64>,
    diag_rate_adjust_ppm_out: Arc<std::sync::atomic::AtomicU64>,
    diag_latency_avail_input_ms_out: Arc<std::sync::atomic::AtomicU64>,
    diag_latency_output_fifo_ms_out: Arc<std::sync::atomic::AtomicU64>,
    diag_latency_resampler_pending_ms_out: Arc<std::sync::atomic::AtomicU64>,
    output_effective_ratio_ppm_out: Arc<std::sync::atomic::AtomicU64>,
    output_ring_input_samples_out: Arc<std::sync::atomic::AtomicU64>,
    runtime_state_code_out: Arc<std::sync::atomic::AtomicU64>,
    recovery_discard_count_out: Arc<std::sync::atomic::AtomicU64>,
    pending_input_triggers: Arc<AtomicI64>,
    input_trigger_rate_hz: Arc<AtomicU32>,
    input_trigger_quantum_frames: Arc<AtomicU32>,
    input_clock_us: Arc<std::sync::atomic::AtomicU64>,
    pacer_pre_roll_complete: Arc<AtomicBool>,
    pacer_flush_requested: Arc<AtomicBool>,
    pacer_enabled: bool,
    pacer_pre_roll_threshold_samples: usize,
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

    // `node.latency` controls the PipeWire processing quantum (callback size),
    // not an abstract graph latency. Requesting the ring target (e.g. 500 ms)
    // here forced PipeWire into ~256 ms callbacks: the control loop then ran
    // once per 256 ms and the `callback/2` midpoint correction became a fixed
    // ~128 ms offset. The target latency must live in the `sample_buffer` ring
    // (target_buffer_fill), so request only the processing quantum here.
    let requested_latency_frames = buffer_config.quantum_frames;
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
    let adaptive_config_snapshot = adaptive_config.lock().clone();

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
    let output_callback_dt_us_for_callback = output_callback_dt_us_out.clone();
    let output_fifo_input_domain_samples_for_callback =
        output_fifo_input_domain_samples_out.clone();
    let output_resampler_pending_input_samples_for_callback =
        output_resampler_pending_input_samples_out.clone();
    let diag_latency_smoothed_ms_for_callback = diag_latency_smoothed_ms_out.clone();
    let diag_latency_control_ms_for_callback = diag_latency_control_ms_out.clone();
    let diag_rate_adjust_ppm_for_callback = diag_rate_adjust_ppm_out.clone();
    let diag_latency_avail_input_ms_for_callback = diag_latency_avail_input_ms_out.clone();
    let diag_latency_output_fifo_ms_for_callback = diag_latency_output_fifo_ms_out.clone();
    let diag_latency_resampler_pending_ms_for_callback =
        diag_latency_resampler_pending_ms_out.clone();
    let input_clock_us_for_callback = input_clock_us.clone();
    let cumulative_written_for_callback = cumulative_written_input_samples.clone();
    let cumulative_drained_for_callback = cumulative_drained_input_samples.clone();
    let cumulative_flow_control_available_for_callback =
        cumulative_flow_control_available_out.clone();
    let output_effective_ratio_ppm_for_callback = output_effective_ratio_ppm_out.clone();
    let output_ring_input_samples_for_callback = output_ring_input_samples_out.clone();
    let runtime_state_code_for_callback = runtime_state_code_out.clone();
    let recovery_discard_count_for_callback = recovery_discard_count_out.clone();
    let mut last_output_callback_at: Option<Instant> = None;
    let mut recovery_discard_total: u64 = 0;
    let live_adaptive_config_for_callback = Arc::clone(&adaptive_config);
    let reset_ratio_for_callback = Arc::clone(&reset_ratio_requested);
    let adaptive_resampling_enabled = enable_adaptive_resampling;
    let latency_servo_enabled = !use_local_resampler;
    // Direct trigger mode: Bresenham accumulator and shared counter for the capture mainloop.
    let pending_input_triggers_for_callback = Arc::clone(&pending_input_triggers);
    let input_trigger_rate_for_callback = Arc::clone(&input_trigger_rate_hz);
    let input_trigger_quantum_for_callback = Arc::clone(&input_trigger_quantum_frames);
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
    let mut adaptive_update_interval =
        adaptive_config_snapshot.update_interval_callbacks.max(1) as u64;
    // The live config as this callback sees it.
    //
    // The config is pushed from the control thread and read from the realtime
    // callback, which used to take the mutex — blocking — up to three times per
    // callback, and could see a different value at each. It is a handful of
    // scalars, so the callback keeps its own copy and refreshes it once, at
    // entry, without blocking: on contention the previous copy stands until the
    // next callback, a few milliseconds later.
    let mut adaptive_cfg = adaptive_config_snapshot.clone();

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
            // DIAG output-callback: publish inter-callback dt + snapshot of
            // ring level + resampler ratio (in ppm dev from 1.0). The
            // resampler in/out per-callback deltas are computed below using
            // ring/FIFO snapshots taken before any drain.
            let now_cb = Instant::now();
            let dt_cb_us = last_output_callback_at
                .map(|prev| now_cb.saturating_duration_since(prev).as_micros() as u64)
                .unwrap_or(0);
            last_output_callback_at = Some(now_cb);
            output_callback_dt_us_for_callback
                .store((dt_cb_us as f64).to_bits(), Ordering::Relaxed);
            let ring_input_samples_at_entry = buffer_for_callback.len();
            output_ring_input_samples_for_callback
                .store((ring_input_samples_at_entry as f64).to_bits(), Ordering::Relaxed);
            let callback_count = runtime_state.advance_callback();
            let mut callback_output_frames: usize = 0;

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
            // The one config read of this callback. Everything downstream uses
            // `adaptive_cfg`, so the whole callback also sees one consistent
            // config rather than re-reading a value that can change under it.
            if let Some(cfg) = live_adaptive_config_for_callback.try_lock() {
                adaptive_cfg.clone_from(&cfg);
            }
            let is_pi_paused = adaptive_cfg.paused;

            if let Some(mut buffer) = stream.dequeue_buffer() {
                // Per-cycle frame count requested by PipeWire for THIS callback.
                // `data.data()` below only exposes the mapped capacity (maxsize),
                // which can be many quanta large; `requested()` is the real
                // processing quantum.
                let requested_frames_this_cycle = buffer.requested();
                let datas = buffer.datas_mut();
                if datas.is_empty() {
                    return;
                }

                let data = &mut datas[0];
                let chunk_size_bytes = data.chunk().size();

                // Get the data slice and fill it
                let written = if let Some(slice) = data.data() {
                    let capacity_samples = slice.len() / 4; // 4 bytes per f32
                    let dest = unsafe {
                        std::slice::from_raw_parts_mut(
                            slice.as_ptr() as *mut f32,
                            capacity_samples,
                        )
                    };

                    let ch = channel_count as usize;
                    // `data.data()` exposes the full mapped buffer capacity, which
                    // can span many quanta. `buffer.requested()` is the number of
                    // frames PipeWire wants for THIS cycle. Producing the whole
                    // capacity instead handed PipeWire ~256 ms per callback, which
                    // collapsed the control loop to a 256 ms granularity (the DAC
                    // latency sawtooth + a fixed ~128 ms `callback/2` offset).
                    // Clamp to the per-cycle request, falling back to capacity if
                    // it is unavailable.
                    let capacity_frames = capacity_samples / ch;
                    let max_frames = if requested_frames_this_cycle > 0 {
                        (requested_frames_this_cycle as usize).min(capacity_frames)
                    } else {
                        capacity_frames
                    };
                    let max_samples = max_frames * ch;
                    let frame_aligned_max = max_samples;
                    callback_output_frames = max_frames;
                    // When the pacer is active, it adds its (fixed) capacity
                    // to the total end-to-end latency. To keep the user's
                    // configured `latency_ms` as the TOTAL latency target
                    // (rather than just the ring level), subtract the pacer
                    // capacity from the PI's setpoint. The PI then aims for
                    // a ring level of `user_target − pacer_capacity` and the
                    // sum (ring + pacer) lands at user_target. Without this,
                    // setting latency to 500 ms would actually give
                    // 500 + pacer_capacity audible delay.
                    let runtime_target_buffer_fill = if pacer_enabled {
                        target_buffer_fill.saturating_sub(pacer_pre_roll_threshold_samples)
                    } else {
                        target_buffer_fill
                    };

                    if callback_count == 1 {
                        log::info!(
                            "PipeWire callback #1: buffer={} bytes, {} samples, {} channels → {} frames (remainder: {}) | requested_frames={} chunk_size_bytes={}",
                            slice.len(), max_samples, ch, max_frames, max_samples % ch,
                            requested_frames_this_cycle, chunk_size_bytes
                        );
                        if max_samples != frame_aligned_max {
                            log::warn!(
                                "PipeWire buffer NOT frame-aligned! {} samples / {} channels = {} remainder. Sink may have different channel count.",
                                max_samples, ch, max_samples % ch
                            );
                        }
                    }
                    if runtime_target_buffer_fill != logged_runtime_target {
                        log::debug!(
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
                    // Raw FIFO level (oscillates with the chunk cycle) — kept
                    // for the components plot so the chunk dynamics remain
                    // observable.
                    let output_fifo_input_domain_samples_raw = output_to_input_domain_samples(
                        resampler_fifo.output_len(),
                        effective_resample_ratio,
                    );
                    let pending_resampler_input_samples = resampler_fifo.pending_input_samples();
                    // DIAG: publish the FIFO and resampler-pending raw input-
                    // domain levels so the diag plot can see whether either is
                    // the source of the post-d43ddab residual sawtooth.
                    output_fifo_input_domain_samples_for_callback.store(
                        (output_fifo_input_domain_samples_raw as f64).to_bits(),
                        Ordering::Relaxed,
                    );
                    output_resampler_pending_input_samples_for_callback.store(
                        (pending_resampler_input_samples as f64).to_bits(),
                        Ordering::Relaxed,
                    );
                    // Callback consumption (input-domain samples).
                    let callback_input_domain_samples = if effective_resample_ratio > 0.0 {
                        ((frame_aligned_max as f64) / effective_resample_ratio).round() as usize
                    } else {
                        frame_aligned_max
                    };
                    // Increment cumulative-drained BEFORE we read the running
                    // difference for control_available. callback_input_domain
                    // _samples is what's about to be consumed this cycle.
                    cumulative_drained_for_callback
                        .fetch_add(callback_input_domain_samples as u64, Ordering::Relaxed);
                    // Cumulative-flow control_available: difference between
                    // total written-to-pipeline and total drained-from-ring,
                    // both in input domain. Smooth by construction — chunk
                    // granularity is just discretisation of the same flow.
                    // No phase lag, no EMA, no constant approximation.
                    let written = cumulative_written_for_callback.load(Ordering::Relaxed);
                    let drained = cumulative_drained_for_callback.load(Ordering::Relaxed);
                    let control_available_override =
                        written.saturating_sub(drained).min(usize::MAX as u64) as usize;
                    // Publish for direct diag-plot inspection (raw counters
                    // converted to f64 bits so the registry can read them).
                    cumulative_flow_control_available_for_callback.store(
                        (control_available_override as f64).to_bits(),
                        Ordering::Relaxed,
                    );
                    // Classical control_available calculation (ring + FIFO +
                    // pending - callback/2). The cumulative-flow override was
                    // tried during the 0.4 Hz investigation but caused
                    // bootstrap deadlock: at startup the callback increments
                    // `drained` before any write has happened, so the override
                    // saturates to 0, the state machine enters perpetual
                    // low-recover, the ring fills, and write_samples hits
                    // the back-pressure timeout (`Buffer drain timeout after
                    // 2s`). See LATENCY_DAC_SAWTOOTH_REPORT.md, session
                    // 2026-05-17, "Cancellation attempts". The cumulative
                    // counters and `cumulative_flow_control_available` are
                    // still published as observation metrics below.
                    // Read the measured callback period (us) for the IIR
                    // cutoff math. Fallback to the configured quantum if
                    // the atomic hasn't been populated yet on the first
                    // callback.
                    let callback_dt_us =
                        f64::from_bits(output_callback_dt_us_for_callback.load(Ordering::Relaxed));
                    let callback_dt_s = if callback_dt_us > 0.0 {
                        callback_dt_us / 1_000_000.0
                    } else {
                        // Same nominal as `quantum_ms` (computed at
                        // construction); kept as a constant fallback.
                        (1024_f64) / (sample_rate as f64)
                    };
                    // Pacer's contribution to the user-visible latency:
                    // the configured pre-roll capacity, used as a FIXED
                    // amount. Backpressure in `write_samples` clamps the
                    // pacer to this capacity, so the live `pacer_fifo.len()`
                    // never significantly exceeds it. Combined with the
                    // `runtime_target_buffer_fill` adjustment above, this
                    // keeps the total end-to-end latency at the user's
                    // configured `latency_ms` regardless of whether the
                    // pacer is active.
                    let pacer_buffer_samples = if pacer_enabled {
                        pacer_pre_roll_threshold_samples
                    } else {
                        0
                    };
                    let mut metrics = update_latency_metrics(
                        &mut runtime_state,
                        available,
                        output_fifo_input_domain_samples_raw,
                        pending_resampler_input_samples,
                        pacer_buffer_samples,
                        callback_input_domain_samples,
                        channel_count as usize,
                        sample_rate,
                        f32::from_bits(graph_latency_for_callback.load(Ordering::Relaxed)),
                        adaptive_cfg.control_smoothing_cutoff_hz,
                        adaptive_cfg.control_smoothing_order,
                        callback_dt_s,
                        LatencyMetricTargets {
                            measured_latency_ms_bits: &measured_latency_ms_out,
                            control_latency_ms_bits: &control_latency_ms_out,
                        },
                    );
                    let _ = control_available_override; // kept for future revival of the flow override
                    // Pre-bridge clock substitution. When `use_pre_bridge_clock`
                    // is on and the input PwStream has produced at least one
                    // chunk, replace `metrics.smoothed_control_available` with
                    // a drift signal computed from the IEC958 source clock and
                    // the DAC drain counter — both monotone and bursting-free,
                    // so the PI sees genuine clock drift without the decoder's
                    // 3.1 Hz batching ripple. The first reading captures a
                    // calibration offset so the substituted value lands on
                    // `target_buffer_fill` and only deviates as the two clocks
                    // actually diverge. The diag plot and the low-recover
                    // state machine keep using the unmodified ring-based
                    // metrics (control_available, control_latency_ms).
                    let input_clock_us_now =
                        f64::from_bits(input_clock_us_for_callback.load(Ordering::Relaxed));
                    let pre_bridge_requested = adaptive_cfg.use_pre_bridge_clock;
                    let pre_bridge_ready = pre_bridge_requested && input_clock_us_now > 0.0;
                    if pre_bridge_ready {
                        let input_clock_samples_eq = (input_clock_us_now / 1_000_000.0
                            * sample_rate as f64
                            * channel_count as f64)
                            as i64;
                        let drained_now =
                            cumulative_drained_for_callback.load(Ordering::Relaxed) as i64;
                        if !runtime_state.pre_bridge_offset_initialized {
                            // Accumulate the raw (input_clock − drained) over
                            // PRE_BRIDGE_CALIBRATION_CALLBACKS callbacks (≥ 1
                            // full decoder batching cycle) and finalize the
                            // offset as the average. A single-sample capture
                            // would lock in the decoder's instantaneous
                            // internal latency (0..~320 ms for the lossless
                            // multichannel codec) and
                            // bias the ring's steady-state position by up to
                            // half that range. Averaging removes the bias.
                            runtime_state.pre_bridge_offset_accum +=
                                (input_clock_samples_eq - drained_now) as i128;
                            runtime_state.pre_bridge_offset_count += 1;
                            if runtime_state.pre_bridge_offset_count
                                >= PRE_BRIDGE_CALIBRATION_CALLBACKS
                            {
                                let count = runtime_state.pre_bridge_offset_count as i128;
                                runtime_state.pre_bridge_offset_samples =
                                    (runtime_state.pre_bridge_offset_accum / count) as i64;
                                runtime_state.pre_bridge_offset_initialized = true;
                            }
                        } else {
                            let drift_samples = input_clock_samples_eq
                                - drained_now
                                - runtime_state.pre_bridge_offset_samples;
                            let target = runtime_target_buffer_fill as i64;
                            let override_value = (target + drift_samples).max(0) as usize;
                            metrics.smoothed_control_available = override_value;
                        }
                    }
                    // Freeze the PI only while we are still waiting for the
                    // first IEC958 chunk to arrive (no source-clock data
                    // yet). Once chunks are flowing, leave the ring-mode PI
                    // running during the calibration window — it keeps the
                    // ring stable around `target_buffer_fill`, so the offset
                    // we accumulate is centred on the steady-state value
                    // instead of a drift trajectory. Without this, the
                    // resampler ratio would stay frozen during calibration
                    // and the clock skew (~−55000 ppm here) would shift the
                    // ring by ~82 ms over the 1.5 s window — the very bias
                    // the averaging is supposed to remove.
                    let pre_bridge_bootstrap_freeze =
                        pre_bridge_requested && !pre_bridge_ready;
                    let is_pi_paused = is_pi_paused || pre_bridge_bootstrap_freeze;
                    // Publish the smoothed control latency for display. Fold in
                    // the pacer's fixed contribution (`pacer_buffer_samples`,
                    // the same amount already added to the raw/control latency
                    // via `display_control_available`) so the smoothed value is
                    // the true end-to-end latency and stays comparable to the
                    // target. The servo keeps tracking the pacer-excluded
                    // `metrics.smoothed_control_available`; only the displayed
                    // value changes here.
                    let smoothed_display_available =
                        metrics.smoothed_control_available.saturating_add(pacer_buffer_samples);
                    let smoothed_control_latency_ms = if channel_count > 0 && sample_rate > 0 {
                        smoothed_display_available as f32
                            / channel_count as f32
                            / sample_rate as f32
                            * 1000.0
                    } else {
                        0.0
                    };
                    smoothed_control_latency_ms_out
                        .store(smoothed_control_latency_ms.to_bits(), Ordering::Relaxed);
                    // DIAG: f64-encoded mirrors of the latency / rate signals
                    // so the generic diag plot can graph them alongside any
                    // other registered metric on the same time scale.
                    diag_latency_smoothed_ms_for_callback.store(
                        (smoothed_control_latency_ms as f64).to_bits(),
                        Ordering::Relaxed,
                    );
                    let control_latency_ms_now = metrics.control_latency_ms;
                    diag_latency_control_ms_for_callback.store(
                        (control_latency_ms_now as f64).to_bits(),
                        Ordering::Relaxed,
                    );
                    let rate_adjust_now =
                        f32::from_bits(rate_adjust_for_callback.load(Ordering::Relaxed));
                    diag_rate_adjust_ppm_for_callback.store(
                        (((rate_adjust_now as f64) - 1.0) * 1e6).to_bits(),
                        Ordering::Relaxed,
                    );
                    // Publish the three components of `control_available` as ms so
                    // they can be plotted independently in the Studio control plot.
                    let samples_to_ms = |samples: usize| -> f32 {
                        if channel_count > 0 && sample_rate > 0 {
                            samples as f32 / channel_count as f32 / sample_rate as f32 * 1000.0
                        } else {
                            0.0
                        }
                    };
                    let avail_input_ms = samples_to_ms(available);
                    let output_fifo_ms = samples_to_ms(output_fifo_input_domain_samples_raw);
                    let resampler_pending_ms = samples_to_ms(pending_resampler_input_samples);
                    avail_input_latency_ms_out.store(avail_input_ms.to_bits(), Ordering::Relaxed);
                    output_fifo_latency_ms_out.store(output_fifo_ms.to_bits(), Ordering::Relaxed);
                    resampler_pending_latency_ms_out
                        .store(resampler_pending_ms.to_bits(), Ordering::Relaxed);
                    // f64 mirrors for the generic diag plot (replaces the
                    // standalone components plot).
                    diag_latency_avail_input_ms_for_callback
                        .store((avail_input_ms as f64).to_bits(), Ordering::Relaxed);
                    diag_latency_output_fifo_ms_for_callback
                        .store((output_fifo_ms as f64).to_bits(), Ordering::Relaxed);
                    diag_latency_resampler_pending_ms_for_callback
                        .store((resampler_pending_ms as f64).to_bits(), Ordering::Relaxed);
                    // Band classification feeds the low-recover state machine: use
                    // raw control_available so the hysteresis bands act on the real
                    // buffer level. (The servo gets the smoothed value separately.)
                    let fallback_band = far_mode_band_from_latency(
                        &adaptive_cfg,
                        metrics.control_available,
                        runtime_target_buffer_fill,
                        samples_per_ms,
                    );
                    current_adaptive_band.store(fallback_band, Ordering::Relaxed);
                    if let Some(ref mut resampler) = resampler_opt {
                        if adaptive_resampling_enabled
                            && !is_pi_paused
                            && runtime_state.low_recover_phase == LowRecoverPhase::Inactive
                        {
                            adaptive_update_interval =
                                adaptive_cfg.update_interval_callbacks.max(1) as u64;
                            if should_run_adaptive_servo(
                                callback_count,
                                adaptive_update_interval as u32,
                                metrics.total_available_input_domain,
                                channel_count as usize,
                            ) {
                                let mut decision = run_adaptive_servo(
                                    &mut runtime_state,
                                    &adaptive_cfg,
                                    metrics,
                                    runtime_target_buffer_fill,
                                    resample_ratio,
                                    480,
                                    adaptive_cfg.max_adjust.max(0.000_001),
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
                                        log::debug!(
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
                        let low_recover_was_active =
                            runtime_state.low_recover_phase != LowRecoverPhase::Inactive;
                        // State machine on raw: its entry/exit/settle bands are
                        // already the hysteresis; smoothing on top added phase lag
                        // that drove a slow oscillation.
                        let far_decision = far_mode_step(
                            &mut runtime_state,
                            &FarModeStepCtx {
                                adaptive_config: &adaptive_cfg,
                                channel_count: channel_count as usize,
                                input_sample_rate: sample_rate,
                                output_sample_rate: actual_output_rate,
                                runtime_state_code: &current_runtime_state,
                                latency: LatencyMetricTargets {
                                    measured_latency_ms_bits: &measured_latency_ms_out,
                                    control_latency_ms_bits: &control_latency_ms_out,
                                },
                            },
                            FarModeStepInputs {
                                is_far_band: current_adaptive_band.load(Ordering::Relaxed)
                                    == ADAPTIVE_BAND_FAR,
                                control_available: metrics.control_available,
                                smoothed_control_available: metrics.smoothed_control_available,
                                target_buffer_fill: runtime_target_buffer_fill,
                                callback_input_domain_samples,
                                resample_ratio: effective_resample_ratio,
                                pacer_buffer_samples,
                                graph_latency_ms: f32::from_bits(
                                    graph_latency_for_callback.load(Ordering::Relaxed),
                                ),
                            },
                        );
                        if far_decision.hold_low_recover {
                            desired_rate_for_callback.store(1.0f32.to_bits(), Ordering::Relaxed);
                            if !low_recover_was_active {
                                resampler.reset();
                                let _ = resampler.set_resample_ratio(resample_ratio, false);
                                resampler_fifo.reset();
                            } else if effective_resample_ratio.to_bits() != resample_ratio.to_bits() {
                                let _ = resampler.set_resample_ratio(resample_ratio, false);
                            }
                            effective_resample_ratio = resample_ratio;
                        }
                        if far_decision.recovery_reacquire_pending {
                            resampler.reset();
                            let _ = resampler.set_resample_ratio(resample_ratio, false);
                            resampler_fifo.reset();
                            effective_resample_ratio = resample_ratio;
                            rate_adjust_for_callback.store(1.0f32.to_bits(), Ordering::Relaxed);
                            desired_rate_for_callback.store(1.0f32.to_bits(), Ordering::Relaxed);
                            runtime_state.recovery_reacquire_pending = false;
                            // The drained counter we use as the pre-bridge
                            // reference may have advanced during recovery
                            // while input_clock_us did not (paused source).
                            // Drop the cached offset and the in-flight
                            // averaging window so the next steady-state run
                            // captures a fresh, valid mean.
                            runtime_state.pre_bridge_offset_initialized = false;
                            runtime_state.pre_bridge_offset_accum = 0;
                            runtime_state.pre_bridge_offset_count = 0;
                            // Flush the post-rendering pacer too: rendered
                            // samples queued before the reacquire are stale.
                            // Re-arm pre-roll so the input thread re-primes
                            // before starting real drains.
                            pacer_flush_requested.store(true, Ordering::Release);
                            pacer_pre_roll_complete.store(false, Ordering::Relaxed);
                            if far_decision.mute_far_output {
                                if let Err(e) = resampler_fifo.ensure_output_samples(
                                    &buffer_for_callback,
                                    resampler,
                                    audio_samples_needed,
                                ) {
                                    log::error!("Resampler error during recovery reacquire: {}", e);
                                } else if resampler_fifo.output_len() > 0 {
                                    resampler_fifo.discard_samples(audio_samples_needed);
                                }
                                dest[..max_samples].fill(0.0);
                                max_samples
                            } else if let Err(e) = resampler_fifo.ensure_output_samples(
                                &buffer_for_callback,
                                resampler,
                                audio_samples_needed,
                            ) {
                                log::error!("Resampler error during recovery reacquire: {}", e);
                                zero_pad_tail(&mut dest[..max_samples], 0);
                                max_samples
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
                                max_samples
                            } else {
                                let copy_count =
                                    resampler_fifo.drain_into_slice(&mut dest[..audio_samples_needed]);
                                zero_pad_tail(&mut dest[..max_samples], copy_count);
                                note_refill_or_underrun(
                                    &mut runtime_state,
                                    "Resampler output underrun",
                                    "Resampler output underrun",
                                    copy_count,
                                    audio_samples_needed,
                                );
                                postprocess_interleaved_output(
                                    &mut dest[..audio_samples_needed],
                                    ch,
                                    far_decision.mute_far_output,
                                    &mut runtime_state,
                                );
                                max_samples
                            }
                        } else {
                            if far_decision.hold_low_recover {
                                let muted_samples_to_consume = if far_decision.mute_far_output
                                    && far_decision.consume_while_muted
                                {
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
                                        &buffer_for_callback,
                                        resampler,
                                        prepared_samples,
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
                                if far_decision.mute_far_output {
                                    dest[..max_samples].fill(0.0);
                                }
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
                            } else if far_decision.hold_low_recover && far_decision.mute_far_output {
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
                        }
                    } else {
                        if latency_servo_enabled
                            && !is_pi_paused
                            && callback_count % adaptive_update_interval == 0
                            && runtime_state.low_recover_phase == LowRecoverPhase::Inactive
                        {
                            if adaptive_resampling_enabled {
                                adaptive_update_interval =
                                    adaptive_cfg.update_interval_callbacks.max(1) as u64;
                            }
                            let drift = metrics.smoothed_control_available as i64
                                - runtime_target_buffer_fill as i64;

                            if adaptive_resampling_enabled {
                                let decision = run_adaptive_servo(
                                    &mut runtime_state,
                                    &adaptive_cfg,
                                    metrics,
                                    runtime_target_buffer_fill,
                                    1.0,
                                    480,
                                    adaptive_cfg.max_adjust.max(0.000_001),
                                    samples_per_ms,
                                    samples_per_ms_f64,
                                );
                                current_adaptive_band.store(decision.adaptive_band, Ordering::Relaxed);
                                rate_adjust_for_callback.store(
                                    decision.displayed_rate_adjust.to_bits(),
                                    Ordering::Relaxed,
                                );
                                desired_rate_for_callback
                                    .store((decision.step.current_ratio as f32).to_bits(), Ordering::Relaxed);

                                if callback_count % 100 == 0 {
                                    log::trace!(
                                        "PipeWire native adaptive: buf={}/{} drift={} rate={:.6} consume={:.6} (P={:.6} I={:.6})",
                                        metrics.control_available,
                                        runtime_target_buffer_fill,
                                        decision.step.drift,
                                        decision.step.current_ratio,
                                        decision.step.consume_adjust,
                                        decision.step.p_term,
                                        decision.step.i_term
                                    );
                                }
                            } else {
                                if drift.abs() > 480 {
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
                                // Band classification on raw — see resampler path.
                                let far_band = far_mode_band_from_latency(
                                    &adaptive_cfg,
                                    metrics.control_available,
                                    runtime_target_buffer_fill,
                                    samples_per_ms,
                                );
                                current_adaptive_band.store(far_band, Ordering::Relaxed);
                                let p_term = {
                                drift as f64 * LATENCY_SERVO_P_GAIN / 100.0
                                };
                                let i_term = {
                                runtime_state.controller_state.accumulated_drift * LATENCY_SERVO_I_GAIN
                                };
                                let max_adjust = {
                                LATENCY_SERVO_MAX_RATE_ADJUST
                                };
                                let consume_adjust =
                                    (1.0 + p_term + i_term).clamp(1.0 - max_adjust, 1.0 + max_adjust);
                                let pipewire_rate = (1.0 / consume_adjust) as f32;

                                rate_adjust_for_callback
                                    .store((consume_adjust as f32).to_bits(), Ordering::Relaxed);
                                desired_rate_for_callback.store(pipewire_rate.to_bits(), Ordering::Relaxed);

                                if callback_count % 100 == 0 {
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

                        // State machine on raw — see resampler path.
                        // Straight copy: the ratio is 1.0, so input and output
                        // domains coincide. Same step as the resampler path.
                        let far_decision = far_mode_step(
                            &mut runtime_state,
                            &FarModeStepCtx {
                                adaptive_config: &adaptive_cfg,
                                channel_count: channel_count as usize,
                                input_sample_rate: sample_rate,
                                output_sample_rate: actual_output_rate,
                                runtime_state_code: &current_runtime_state,
                                latency: LatencyMetricTargets {
                                    measured_latency_ms_bits: &measured_latency_ms_out,
                                    control_latency_ms_bits: &control_latency_ms_out,
                                },
                            },
                            FarModeStepInputs {
                                is_far_band: current_adaptive_band.load(Ordering::Relaxed)
                                    == ADAPTIVE_BAND_FAR,
                                control_available: metrics.control_available,
                                smoothed_control_available: metrics.smoothed_control_available,
                                target_buffer_fill: runtime_target_buffer_fill,
                                callback_input_domain_samples,
                                resample_ratio: 1.0,
                                pacer_buffer_samples,
                                graph_latency_ms: f32::from_bits(
                                    graph_latency_for_callback.load(Ordering::Relaxed),
                                ),
                            },
                        );
                        if far_decision.hold_low_recover {
                            desired_rate_for_callback.store(1.0f32.to_bits(), Ordering::Relaxed);
                        }
                        if far_decision.recovery_reacquire_pending {
                            desired_rate_for_callback.store(1.0f32.to_bits(), Ordering::Relaxed);
                            rate_adjust_for_callback.store(1.0f32.to_bits(), Ordering::Relaxed);
                            runtime_state.recovery_reacquire_pending = false;
                            runtime_state.pre_bridge_offset_initialized = false;
                            runtime_state.pre_bridge_offset_accum = 0;
                            runtime_state.pre_bridge_offset_count = 0;
                            pacer_flush_requested.store(true, Ordering::Release);
                            pacer_pre_roll_complete.store(false, Ordering::Relaxed);
                            if far_decision.mute_far_output {
                                let dropped =
                                    discard_ring_samples(&buffer_for_callback, samples_to_read);
                                recovery_discard_total =
                                    recovery_discard_total.saturating_add(dropped as u64);
                                recovery_discard_count_for_callback.store(
                                    (recovery_discard_total as f64).to_bits(),
                                    Ordering::Relaxed,
                                );
                                if dropped < samples_to_read {
                                    log::debug!(
                                        "Recovery reacquire underfed: consumed {} / {} samples while re-priming output",
                                        dropped,
                                        samples_to_read
                                    );
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
                                max_samples
                            }
                        } else if far_decision.hard_recover_high {
                            let plan = compute_hard_recover_high_plan(
                                callback_input_domain_samples,
                                metrics.control_available,
                                runtime_target_buffer_fill,
                                1.0,
                                channel_count as usize,
                            );
                            let dropped =
                                discard_ring_samples(&buffer_for_callback, plan.desired_consume_input_samples);
                            recovery_discard_total =
                                recovery_discard_total.saturating_add(dropped as u64);
                            recovery_discard_count_for_callback.store(
                                (recovery_discard_total as f64).to_bits(),
                                Ordering::Relaxed,
                            );
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
                            let muted_samples_to_consume = if far_decision.mute_far_output
                                && far_decision.consume_while_muted
                            {
                                samples_to_read
                            } else {
                                0
                            };
                            let samples_to_discard = muted_samples_to_consume
                                .saturating_add(far_decision.low_recover_trim_input_samples);
                            if samples_to_discard > 0 {
                                let dropped =
                                    discard_ring_samples(&buffer_for_callback, samples_to_discard);
                                recovery_discard_total =
                                    recovery_discard_total.saturating_add(dropped as u64);
                                recovery_discard_count_for_callback.store(
                                    (recovery_discard_total as f64).to_bits(),
                                    Ordering::Relaxed,
                                );
                                if dropped < samples_to_discard {
                                    log::debug!(
                                        "Low-recover muted consume underfed: consumed {} / {} samples while stabilizing resume latency",
                                        dropped,
                                        samples_to_discard
                                    );
                                }
                            }
                            if far_decision.mute_far_output {
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
                                max_samples
                            }
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

            // Direct trigger mode: increment the shared pending-trigger counter (Bresenham).
            // The capture mainloop drains this counter and fires pw_stream_trigger_process()
            // from its own thread — the only reliable caller for the capture DRIVER stream.
            let in_rate = input_trigger_rate_for_callback.load(Ordering::Relaxed) as i64;
            let in_quantum = input_trigger_quantum_for_callback.load(Ordering::Relaxed) as i64;
            if in_rate > 0 && in_quantum > 0 && callback_output_frames > 0 {
                bresenham_acc += (callback_output_frames as i64).saturating_mul(in_rate);
                let trigger_den = (actual_output_rate as i64).saturating_mul(in_quantum);
                while trigger_den > 0 && bresenham_acc >= trigger_den {
                    pending_input_triggers_for_callback.fetch_add(1, Ordering::Relaxed);
                    bresenham_acc -= trigger_den;
                }
            }
            // DIAG output-callback: publish current adaptive runtime state
            // (so the diag plot reveals any periodic state transitions even
            // when PI servo is paused).
            runtime_state_code_for_callback.store(
                (current_runtime_state.load(Ordering::Relaxed) as f64).to_bits(),
                Ordering::Relaxed,
            );
            // DIAG output: effective ratio (ppm deviation from 1.0).
            let ratio_ppm_dev = (effective_resample_ratio - 1.0) * 1_000_000.0;
            output_effective_ratio_ppm_for_callback
                .store((ratio_ppm_dev).to_bits(), Ordering::Relaxed);
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
