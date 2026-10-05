#![cfg(target_os = "linux")]

use anyhow::{Result, anyhow};
use parking_lot::Mutex;
use pipewire as pw;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::callback_core::{CallbackContext, CallbackShared, OutputCallbackCore, PacerLink};
use crate::callback_log::{CallbackLogDrain, callback_event};
use crate::pipewire_registry::{
    ClientEntry, client_from_props, connect_main_loop, non_empty, owner_pid, registry_snapshot,
};
use crate::{
    AdaptiveResamplingConfig, local_resampler_ratio_bounds,
    pacer::{PacerDrainEnds, PacerHandle},
    resampler_fifo::{RESAMPLER_CHUNK_SIZE, new_output_resampler},
    ring_buffer_io::{
        OUTPUT_RING_CAPACITY, RingMonitor, RingReader, RingWriter, flush_ring_buffer,
        push_samples_drop_overflow, push_samples_with_backpressure, sample_ring,
    },
};

/// `SPA_PROP_rate` from <spa/param/props.h>: the stream adapter's resample
/// rate scaler, the control `pw_stream_set_control` takes to speed up or slow
/// down how fast the graph drains this stream.
const SPA_PROP_RATE: u32 = pw::spa::sys::SPA_PROP_rate;

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

/// Properties that pin the output stream to `target`.
///
/// Four properties, because stating the device once is not enough:
///
/// - `target.object` is what WirePlumber 0.5 resolves first; `node.target` is
///   the deprecated spelling it only falls back to, so both are stated.
/// - `node.dont-move` makes the session manager ignore a `target.node` entry
///   in the default metadata. Any mixer offering "play on the default device"
///   writes `target.node = -1` there, and that entry outranks both properties
///   above — the stream then leaves the requested device without a word.
/// - `node.dont-fallback` forbids the consolation prize: when the requested
///   device cannot be linked, the stream must stay unlinked rather than land
///   on the default sink.
///
/// The last two matter because of what the default sink can be. Omniphony
/// publishes its own bridge input sink, and a machine that plays through
/// Omniphony has it set as the default: falling back there loops the rendered
/// output straight back into the decoder input, and — since that node drives
/// the graph it is triggered by — hands the output stream a clock that only
/// this stream keeps alive. The ring then stops draining altogether, which
/// reads as an endless "Buffer drain timeout" and total silence.
fn output_target_properties(target: &str) -> [(&'static str, &str); 4] {
    [
        ("target.object", target),
        ("node.target", target),
        ("node.dont-move", "true"),
        ("node.dont-fallback", "true"),
    ]
}

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

/// Bound on the registry round trip behind the device list: a wedged daemon
/// must not hang the caller (the control surface asks for this list).
const DEVICE_LIST_TIMEOUT: Duration = Duration::from_secs(2);

/// An `Audio/Sink` node offered as an output device, with its owning client.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SinkCandidate {
    /// `node.name`, what `target.object` is set to.
    value: String,
    /// Human-readable label: description, nick, or device name.
    label: String,
    client_id: Option<u32>,
}

/// Reads a registry `Node` global: `Some` only for a named `Audio/Sink`.
fn sink_candidate_from_props<'a>(get: impl Fn(&str) -> Option<&'a str>) -> Option<SinkCandidate> {
    if get(*pw::keys::MEDIA_CLASS)? != "Audio/Sink" {
        return None;
    }
    let value = non_empty(get(*pw::keys::NODE_NAME))?;
    let label = non_empty(get(*pw::keys::NODE_DESCRIPTION))
        .or_else(|| non_empty(get(*pw::keys::NODE_NICK)))
        .or_else(|| non_empty(get(*pw::keys::DEVICE_DESCRIPTION)))
        .or_else(|| non_empty(get(*pw::keys::DEVICE_NAME)))
        .unwrap_or(value);
    Some(SinkCandidate {
        value: value.to_string(),
        label: label.to_string(),
        client_id: non_empty(get(*pw::keys::CLIENT_ID)).and_then(|v| v.parse().ok()),
    })
}

/// The `(node.name, label)` list offered for output, sorted by label.
///
/// Sinks this very process publishes are left out: that is Omniphony's own
/// bridge input sink, and rendering into it loops the output straight back
/// into the decoder input, with a clock only this output stream keeps alive
/// (see [`output_target_properties`]). Offering it would only let the user
/// pick the one device that cannot work.
fn output_device_list(
    sinks: &[SinkCandidate],
    clients: &[ClientEntry],
    own_pid: u32,
) -> Vec<(String, String)> {
    let mut devices: Vec<(String, String)> = sinks
        .iter()
        .filter(|sink| owner_pid(sink.client_id, clients) != Some(own_pid))
        .map(|sink| (sink.value.clone(), sink.label.clone()))
        .collect();
    devices.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    devices.dedup_by(|a, b| a.0 == b.0);
    devices
}

pub fn list_pipewire_output_devices() -> Result<Vec<(String, String)>> {
    let conn = connect_main_loop()?;
    let sinks = Rc::new(RefCell::new(Vec::<SinkCandidate>::new()));
    let clients = Rc::new(RefCell::new(Vec::<ClientEntry>::new()));
    let sinks_for_registry = Rc::clone(&sinks);
    let clients_for_registry = Rc::clone(&clients);
    let answered = registry_snapshot(
        &conn.mainloop,
        &conn.core,
        DEVICE_LIST_TIMEOUT,
        move |global| {
            let Some(props) = global.props.as_ref() else {
                return;
            };
            match global.type_ {
                pw::types::ObjectType::Node => {
                    if let Some(sink) = sink_candidate_from_props(|key| props.get(key)) {
                        sinks_for_registry.borrow_mut().push(sink);
                    }
                }
                pw::types::ObjectType::Client => {
                    clients_for_registry
                        .borrow_mut()
                        .push(client_from_props(global.id, |key| props.get(key)));
                }
                _ => {}
            }
        },
    )?;
    if !answered {
        return Err(anyhow!(
            "PipeWire registry did not answer within {DEVICE_LIST_TIMEOUT:?}"
        ));
    }
    Ok(output_device_list(
        &sinks.borrow(),
        &clients.borrow(),
        std::process::id(),
    ))
}

/// Drift, in samples, the servo leaves alone on this backend.
const SERVO_DEADBAND_SAMPLES: usize = 480;

/// The `SPA_PROP_rate` value that makes the graph drain the ring
/// `consume_adjust` times as fast as nominal: what the callback publishes as
/// its native rate.
///
/// The stream adapter resamples with `in_rate * rate / out_rate` input frames
/// per output frame (spa audioconvert divides its resampler rate by
/// `props.rate`), so a value above 1.0 consumes more of this stream per graph
/// cycle. That is the same direction as `consume_adjust`: no inversion, unlike
/// the local resampler's output/input ratio.
fn pipewire_rate_for_consume_adjust(consume_adjust: f64) -> f32 {
    consume_adjust as f32
}

fn wallclock_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub struct PipewireWriter {
    /// The ring the DAC callback reads, seen from here: its level, and the
    /// request to drop what a flush gave up on. Neither of its ends — the
    /// callback holds the reading one, and the writing one is `write_target`
    /// without the pacer, the pacer drain's with it.
    ring: RingMonitor,
    /// Where `write_samples` pushes: the ring itself, or, when the
    /// post-rendering pacer is on, the pacer FIFO. The PipeWire INPUT thread
    /// then drains that FIFO into the ring at a cadence that mirrors the
    /// IEC958 chunk arrival rate, so the ring buffer the DAC consumes sees a
    /// smooth flow regardless of the decoder's burst pattern.
    ///
    /// Which of the two is settled when the writer is built: each ring has
    /// one producer, so the renderer cannot take turns with the drain.
    write_target: RingWriter,
    /// The drain's handle on the pacer; `None` when pacing is off.
    pacer: Option<PacerHandle>,
    /// When true, `write_samples` pushes without ever blocking and drops the
    /// overflow above the back-pressure threshold instead of waiting for the
    /// DAC to drain. Decouples the producer from the consumer clock.
    backpressure_disabled: Arc<AtomicBool>,
    pacer_pre_roll_threshold_samples: usize,
    sample_rate: u32,
    channel_count: u32,
    /// Pre-computed back-pressure threshold in samples (max_latency_ms → samples).
    max_buffer_samples: usize,
    /// PipeWire quantum latency in ms (pre-computed for use in latency_ms()).
    quantum_ms: f32,
    stream_ready: Arc<AtomicBool>,
    enable_adaptive_resampling: bool,
    /// What the process callback reads and publishes: the live config, the
    /// servo's rate adjust, band and state, and every telemetry atomic.
    shared: CallbackShared,
    /// Signals the PipeWire worker thread to stop and exit cleanly.
    shutdown_requested: Arc<AtomicBool>,
    /// Configured ring-buffer target latency (from PipewireBufferConfig::latency_ms).
    target_latency_ms: u32,
    pw_thread: Option<thread::JoinHandle<()>>,
    bootstrap_started_at: Instant,
    bootstrap_write_calls: u32,
    bootstrap_written_samples: usize,
    input_trigger: InputTrigger,
}

/// Direct trigger mode: the output callback schedules the capture stream's
/// cycles, Bresenham-style, in proportion to the audio it plays.
#[derive(Clone)]
struct InputTrigger {
    /// Cycles owed to the capture stream. The output callback increments it;
    /// the capture mainloop drains it and calls `pw_stream_trigger_process()`
    /// from its own thread (required for correct operation).
    pending: Arc<AtomicI64>,
    /// Sample rate of the capture stream, for the trigger ratio.
    rate_hz: Arc<AtomicU32>,
    /// Observed capture quantum in transport frames. Combined with `rate_hz`
    /// so the schedule follows real audio duration, not callback count alone.
    quantum_frames: Arc<AtomicU32>,
}

impl InputTrigger {
    fn new() -> Self {
        Self {
            pending: Arc::new(AtomicI64::new(0)),
            rate_hz: Arc::new(AtomicU32::new(0)),
            quantum_frames: Arc::new(AtomicU32::new(0)),
        }
    }

    /// Owe the capture stream the cycles `output_frames` at `output_rate`
    /// amount to. `acc` carries the remainder from one callback to the next.
    fn schedule(&self, acc: &mut i64, output_frames: usize, output_rate: u32) {
        let in_rate = self.rate_hz.load(Ordering::Relaxed) as i64;
        let in_quantum = self.quantum_frames.load(Ordering::Relaxed) as i64;
        if in_rate <= 0 || in_quantum <= 0 || output_frames == 0 {
            return;
        }
        *acc += (output_frames as i64).saturating_mul(in_rate);
        let trigger_den = (output_rate as i64).saturating_mul(in_quantum);
        while trigger_den > 0 && *acc >= trigger_den {
            self.pending.fetch_add(1, Ordering::Relaxed);
            *acc -= trigger_den;
        }
    }
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

        let (ring_writer, ring_reader) = sample_ring(OUTPUT_RING_CAPACITY);
        let ring = ring_writer.monitor();
        let pacer_enabled = adaptive_config.use_output_pacing;
        let backpressure_disabled = Arc::new(AtomicBool::new(adaptive_config.disable_backpressure));
        let shared = CallbackShared::new(adaptive_config);
        let telemetry = &shared.telemetry;
        // 64 ms of audio at the output rate × channel count. Covers >1 AU
        // for both supported input codecs (~32 ms per AU) with margin.
        let pacer_pre_roll_threshold_samples =
            ((sample_rate as usize) * (channel_count as usize) * 64 / 1000).max(1);
        // The ring's writing end goes to the renderer, or to the pacer drain
        // with the renderer writing to the pacer FIFO instead. The callback
        // gets the pacer's flags, to make it re-prime after a recovery.
        let (write_target, pacer, pacer_link) = if pacer_enabled {
            // Pacer FIFO: capacity matches the ring so worst-case can buffer
            // the same amount of audio. It only fills meaningfully when the
            // input-thread drain lags or is paused (eg. during pre-roll).
            let (fifo_writer, fifo_reader) = sample_ring(OUTPUT_RING_CAPACITY);
            let pre_roll_complete = Arc::new(AtomicBool::new(false));
            let flush_requested = Arc::new(AtomicBool::new(false));
            let handle = PacerHandle {
                ends: Arc::new(Mutex::new(PacerDrainEnds {
                    fifo: fifo_reader,
                    ring: ring_writer,
                })),
                pre_roll_complete: Arc::clone(&pre_roll_complete),
                flush_requested: Arc::clone(&flush_requested),
                pre_roll_threshold_samples: pacer_pre_roll_threshold_samples,
                out_sample_rate: sample_rate,
                out_channels: channel_count,
                diag_drain_total: Arc::clone(&telemetry.pacer_drain_total),
                diag_underrun_total: Arc::clone(&telemetry.pacer_underrun_total),
                diag_fifo_level: Arc::clone(&telemetry.pacer_fifo_level),
            };
            let link = PacerLink {
                pre_roll_complete,
                flush_requested,
                buffer_samples: pacer_pre_roll_threshold_samples,
            };
            (fifo_writer, Some(handle), Some(link))
        } else {
            (ring_writer, None, None)
        };
        let stream_ready = Arc::new(AtomicBool::new(false));
        let ready_clone = stream_ready.clone();
        let ready_for_thread_cleanup = stream_ready.clone();
        let shutdown_requested = Arc::new(AtomicBool::new(false));
        let shutdown_requested_clone = shutdown_requested.clone();
        let input_trigger = InputTrigger::new();
        let input_trigger_for_thread = input_trigger.clone();
        let shared_for_thread = shared.clone();

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
                ring_reader,
                sample_rate,
                channel_count,
                ready_clone,
                output_device,
                channel_names,
                enable_adaptive_resampling,
                output_sample_rate,
                buffer_config,
                shared_for_thread,
                shutdown_requested_clone,
                input_trigger_for_thread,
                input_clock_us,
                pacer_link,
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
            ring,
            write_target,
            pacer,
            backpressure_disabled,
            pacer_pre_roll_threshold_samples,
            sample_rate,
            channel_count,
            max_buffer_samples,
            quantum_ms,
            stream_ready,
            enable_adaptive_resampling,
            shared,
            shutdown_requested,
            target_latency_ms,
            pw_thread: Some(pw_thread),
            bootstrap_started_at: Instant::now(),
            bootstrap_write_calls: 0,
            bootstrap_written_samples: 0,
            input_trigger,
        })
    }

    pub fn write_samples(&mut self, samples: &[f32]) -> Result<()> {
        // Cumulative-flow counter: increment by what we're ABOUT to push,
        // not by what the back-pressure-aware push will actually accept.
        // This matches the PI's mental model of "samples committed to the
        // pipeline" — back-pressure drops are exceptional and would show
        // up as a separate divergence anyway.
        self.shared
            .telemetry
            .cumulative_written_input_samples
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
        let pacer_active = self.pacer.is_some();
        let target_buffer = &mut self.write_target;
        let max_buffer_fill = if pacer_active {
            self.pacer_pre_roll_threshold_samples
        } else {
            self.max_buffer_samples
        };
        // The ring's level frames the first few writes in the log; it is not
        // read after that, so that the renderer does not pull the callback's
        // counters into its cache on every write.
        let bootstrap = self.bootstrap_write_calls < 5;
        let buffer_before = if bootstrap { self.ring.fill() } else { 0 };
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
            self.shared
                .telemetry
                .last_write_ms
                .store(wallclock_millis(), Ordering::Relaxed);
        }
        if bootstrap {
            log::debug!(
                "PipeWire bootstrap write #{}: pushed {} / {} samples, ring {} -> {}, elapsed {:.0} ms",
                self.bootstrap_write_calls,
                report.pushed_samples,
                samples.len(),
                buffer_before,
                self.ring.fill(),
                self.bootstrap_started_at.elapsed().as_secs_f64() * 1000.0
            );
        }

        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        let report = flush_ring_buffer(
            &self.ring,
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
                "Flush: buffer stalled at {} samples, left for the callback to drop",
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
        self.ring.fill()
    }

    /// Estimated current end-to-end audio latency in milliseconds.
    ///
    /// Composed of:
    /// - Ring buffer latency: current fill (in frames) / sample_rate
    /// - PipeWire quantum latency: quantum_frames / output_sample_rate
    pub fn latency_ms(&self) -> f32 {
        let fill_frames = self.ring.fill() / self.channel_count as usize;
        let ring_ms = fill_frames as f32 / self.sample_rate as f32 * 1000.0;
        ring_ms + self.quantum_ms
    }

    /// Current rate-adjust factor applied by the PI controller.
    /// Returns `None` when adaptive resampling is disabled.
    /// Value is near 1.0; deviation from 1.0 represents clock drift correction
    /// (e.g. 1.0015 = consuming 0.15 % faster than nominal to drain the buffer).
    pub fn rate_adjust(&self) -> Option<f32> {
        if self.enable_adaptive_resampling {
            Some(self.shared.rate_adjust())
        } else {
            None
        }
    }

    /// Set the capture sample rate for the Bresenham trigger ratio.
    /// Once set, the output RT callback increments pending_input_triggers() per output callback.
    pub fn set_input_trigger_rate_hz(&self, rate_hz: u32) {
        self.input_trigger.rate_hz.store(rate_hz, Ordering::Relaxed);
    }

    /// Set the observed capture quantum in transport frames for direct-trigger scheduling.
    pub fn set_input_trigger_quantum_frames(&self, quantum_frames: u32) {
        self.input_trigger
            .quantum_frames
            .store(quantum_frames, Ordering::Relaxed);
    }

    /// Returns the Arc that the output RT callback increments (Bresenham).
    /// Pass this to InputControl.set_pending_input_triggers() so the capture mainloop can drain it.
    pub fn pending_input_triggers(&self) -> Arc<AtomicI64> {
        Arc::clone(&self.input_trigger.pending)
    }

    /// Hand a cross-crate handle to the post-rendering pacer to the audio
    /// input layer (via `InputControl::install_output_pacer`). The input
    /// PwStream callback uses this to drain the FIFO into the ring buffer
    /// in lockstep with IEC958 chunk arrival.
    ///
    /// `None` when this output was built with pacing off: there is then no
    /// FIFO to drain, and the ring's writing end is the renderer's.
    pub fn pacer_handle(&self) -> Option<PacerHandle> {
        self.pacer.clone()
    }

    /// Hot-swap the back-pressure disable flag. Called when
    /// `AdaptiveResamplingConfig::disable_backpressure` flips via OSC.
    pub fn set_backpressure_disabled(&self, disabled: bool) {
        self.backpressure_disabled
            .store(disabled, Ordering::Relaxed);
    }

    pub fn adaptive_band(&self) -> Option<&'static str> {
        self.shared.adaptive_band()
    }

    pub fn adaptive_runtime_state(&self) -> Option<&'static str> {
        self.shared.adaptive_runtime_state()
    }

    /// Downstream graph latency in ms as reported by pw_stream_get_time_n().delay.
    /// Includes PipeWire graph scheduling and the netjack2 driver quantum.
    /// Returns 0.0 until the stream has been active for ~2 seconds.
    pub fn graph_latency_ms(&self) -> f32 {
        f32::from_bits(
            self.shared
                .telemetry
                .graph_latency_ms_bits
                .load(Ordering::Relaxed),
        )
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
        f32::from_bits(
            self.shared
                .telemetry
                .measured_latency_ms_bits
                .load(Ordering::Relaxed),
        )
    }

    pub fn control_audio_delay_ms(&self) -> f32 {
        f32::from_bits(
            self.shared
                .telemetry
                .control_latency_ms_bits
                .load(Ordering::Relaxed),
        )
    }

    /// EMA-smoothed control latency in ms (the value the servo actually tracks).
    pub fn smoothed_control_audio_delay_ms(&self) -> f32 {
        f32::from_bits(
            self.shared
                .telemetry
                .smoothed_control_latency_ms_bits
                .load(Ordering::Relaxed),
        )
    }

    /// Ring-buffer occupancy converted to ms (first component of `control_available`).
    pub fn avail_input_audio_delay_ms(&self) -> f32 {
        f32::from_bits(
            self.shared
                .telemetry
                .avail_input_latency_ms_bits
                .load(Ordering::Relaxed),
        )
    }

    /// Resampler output FIFO content converted back to input-domain ms
    /// (second component of `control_available`).
    pub fn output_fifo_audio_delay_ms(&self) -> f32 {
        f32::from_bits(
            self.shared
                .telemetry
                .output_fifo_latency_ms_bits
                .load(Ordering::Relaxed),
        )
    }

    /// Local resampler pending input samples expressed as ms
    /// (third component of `control_available`).
    pub fn resampler_pending_audio_delay_ms(&self) -> f32 {
        f32::from_bits(
            self.shared
                .telemetry
                .resampler_pending_latency_ms_bits
                .load(Ordering::Relaxed),
        )
    }

    /// Signal the audio thread to snap the resampling ratio back to base and reset the integrator.
    pub fn request_ratio_reset(&self) {
        self.shared.request_ratio_reset();
    }

    /// Update adaptive resampling tuning parameters without restarting the audio thread.
    pub fn update_adaptive_config(&self, config: AdaptiveResamplingConfig) {
        // Output pacing is NOT hot-swappable: it decides which thread produces
        // into the ring, and swapping producers under a running stream is what
        // the single-producer invariant forbids. Say so rather than silently
        // ignoring the request.
        let pacer_enabled = self.pacer.is_some();
        if config.use_output_pacing != pacer_enabled {
            log::warn!(
                "Output pacing is fixed for the lifetime of the audio output                  (running with {}, requested {}); the change takes effect at the                  next output start.",
                pacer_enabled,
                config.use_output_pacing
            );
        }
        self.set_backpressure_disabled(config.disable_backpressure);
        *self.shared.live_config.lock() = config;
    }

    /// Diagnostic metric handles published by the PipeWire output backend.
    /// Registered in the global registry by the caller; adding a metric to
    /// [`OutputTelemetry`] surfaces it in the diag plot with no change here.
    pub fn diag_atomic_handles(&self) -> Vec<diag::DiagAtomicHandle> {
        self.shared.telemetry.diag_handles()
    }
}

impl Drop for PipewireWriter {
    fn drop(&mut self) {
        log::debug!("Dropping PipeWire writer");
        // The callback returns at its top from here on, so what is left in the
        // ring is never played and goes with the ring. It is not popped from
        // this thread: the ring has one consumer, the callback. And it is not
        // flushed again either — flush() was already called by finalize(), and
        // a second one would block for another 500ms–5s if the callback is in
        // recovery mode.
        self.shutdown_requested.store(true, Ordering::Relaxed);
        if let Some(handle) = self.pw_thread.take() {
            let _ = handle.join();
        }
    }
}

fn run_pipewire_loop(
    ring: RingReader,
    sample_rate: u32, // Native sample rate (48000 Hz)
    channel_count: u32,
    stream_ready: Arc<AtomicBool>,
    output_device: Option<String>,
    channel_names: Option<Vec<String>>,
    enable_adaptive_resampling: bool,
    output_sample_rate: Option<u32>, // Target output rate for upsampling
    buffer_config: PipewireBufferConfig,
    shared: CallbackShared,
    shutdown_requested: Arc<AtomicBool>,
    input_trigger: InputTrigger,
    input_clock_us: Arc<AtomicU64>,
    pacer: Option<PacerLink>,
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

    let props = output_stream_properties(
        channel_count,
        output_device.as_deref(),
        channel_names.as_deref(),
        buffer_config.quantum_frames,
        actual_output_rate,
    );
    let stream = pw::stream::StreamBox::new(&core, "omniphony-audio", props)
        .map_err(|e| anyhow!("Failed to create stream: {:?}", e))?;

    // Setup state changed listener
    let ready_for_state = stream_ready.clone();
    let graph_latency_for_state = shared.telemetry.graph_latency_ms_bits.clone();
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

    // Initialize resampler for true rate conversion and for adaptive 1:1 operation.
    let resampler = if use_local_resampler {
        let (min_ratio, max_ratio) = local_resampler_ratio_bounds(resample_ratio);
        log::debug!(
            "Initializing PipeWire resampler: base_ratio={:.4}, min_ratio={:.4}, max_ratio={:.4}, chunk_size={}",
            resample_ratio,
            min_ratio,
            max_ratio,
            RESAMPLER_CHUNK_SIZE
        );
        Some(
            new_output_resampler(resample_ratio, channel_count as usize)
                .map_err(|e| anyhow!("Failed to create resampler: {:?}", e))?,
        )
    } else {
        None
    };

    // The servo's setpoint, from the latency target. It is compared against
    // ring levels, which count INPUT-domain samples (the writer pushes at
    // `sample_rate`, before local resampling), so it is converted at the input
    // rate: the output rate underestimates the latency when downsampling (e.g.
    // 96k -> 48k), giving too low a fill, long-term A/V drift and instability.
    let target_buffer_fill =
        (buffer_config.latency_ms as usize * sample_rate as usize) / 1000 * channel_count as usize;
    let max_buffer_fill = (buffer_config.max_latency_ms as usize * sample_rate as usize) / 1000
        * channel_count as usize;
    log::info!(
        "PipeWire buffer thresholds ({}ch): latency={}ms max={}ms quantum={}fr | \
         target={} max={} samples",
        channel_count,
        buffer_config.latency_ms,
        buffer_config.max_latency_ms,
        buffer_config.quantum_frames,
        target_buffer_fill,
        max_buffer_fill
    );
    if let Some(pacer) = &pacer {
        // The pacer adds its fixed capacity to the end-to-end latency, so the
        // ring aims for the rest: ring + pacer lands on `latency_ms`.
        log::debug!(
            "PipeWire ring target: {} samples ({} for the output pacer)",
            target_buffer_fill.saturating_sub(pacer.buffer_samples),
            pacer.buffer_samples
        );
    }

    let initial_config = shared.live_config.lock().clone();
    let (mut callback_core, callback_log_reader) = OutputCallbackCore::new(
        CallbackContext {
            channel_count: channel_count as usize,
            dest_channels: channel_count as usize,
            input_sample_rate: sample_rate,
            output_sample_rate: actual_output_rate,
            target_buffer_fill,
            servo_deadband_samples: SERVO_DEADBAND_SAMPLES,
            adaptive_resampling: enable_adaptive_resampling,
            pacer,
            input_clock_us: Some(input_clock_us),
        },
        shared.clone(),
        ring,
        resampler,
        initial_config,
        module_path!(),
    );
    // The callback reports through this queue; the drain thread logs. Declared
    // before the listener, so it outlives the callback and logs its last events.
    let _callback_log_drain = CallbackLogDrain::spawn(callback_log_reader);

    let shutdown_requested_for_callback = shutdown_requested.clone();
    let graph_latency = shared.telemetry.graph_latency_ms_bits.clone();
    // Remainder of the input-trigger schedule, carried between callbacks.
    let mut trigger_acc = 0i64;
    let _listener = stream
        .add_local_listener_with_user_data(())
        .process(move |stream, _| {
            if shutdown_requested_for_callback.load(Ordering::Relaxed) {
                return;
            }
            let mut output_frames = 0;
            if let Some(mut buffer) = stream.dequeue_buffer() {
                // Per-cycle frame count requested by PipeWire for THIS callback.
                // `data.data()` below only exposes the mapped capacity (maxsize),
                // which can be many quanta large; `requested()` is the real
                // processing quantum.
                let requested_frames = buffer.requested();
                let datas = buffer.datas_mut();
                if datas.is_empty() {
                    return;
                }
                let data = &mut datas[0];
                let chunk_size_bytes = data.chunk().size();

                let written = if let Some(slice) = data.data() {
                    let capacity_samples = slice.len() / 4; // 4 bytes per f32
                    let dest = unsafe {
                        std::slice::from_raw_parts_mut(slice.as_ptr() as *mut f32, capacity_samples)
                    };
                    let ch = channel_count as usize;
                    // Produce the per-cycle request, not the whole capacity:
                    // that handed PipeWire ~256 ms per callback and collapsed
                    // the control loop to a 256 ms granularity (the DAC
                    // latency sawtooth + a fixed ~128 ms `callback/2` offset).
                    // Capacity only when the request is unavailable.
                    let capacity_frames = capacity_samples / ch;
                    let frames = if requested_frames > 0 {
                        (requested_frames as usize).min(capacity_frames)
                    } else {
                        capacity_frames
                    };
                    let samples = frames * ch;
                    let callback_number = callback_core.callback_count() + 1;
                    if callback_number == 1 {
                        callback_event!(
                            callback_core.log(),
                            Info,
                            "PipeWire callback #1",
                            buffer_bytes = slice.len(),
                            samples = samples,
                            channels = ch,
                            frames = frames,
                            requested_frames = requested_frames,
                            chunk_size_bytes = chunk_size_bytes
                        );
                    }
                    // Downstream graph latency, every ~100 callbacks to
                    // amortise the cost (`pw_stream_get_time_n` is RT-safe
                    // inside the process callback).
                    if callback_number % 100 == 50 {
                        if let Some(delay_ms) = graph_delay_ms(stream) {
                            graph_latency.store(delay_ms.to_bits(), Ordering::Relaxed);
                        }
                    }
                    callback_core.process(
                        &mut dest[..samples],
                        f32::from_bits(graph_latency.load(Ordering::Relaxed)),
                    );
                    output_frames = frames;
                    samples
                } else {
                    0
                };

                let chunk = data.chunk_mut();
                *chunk.offset_mut() = 0;
                *chunk.size_mut() = (written * 4) as u32;
                *chunk.stride_mut() = 4;
            }
            input_trigger.schedule(&mut trigger_acc, output_frames, actual_output_rate);
        })
        .register()
        .map_err(|e| anyhow!("Failed to register process listener: {:?}", e))?;

    let format = output_format_pod(actual_output_rate, channel_count)?;
    let param =
        pw::spa::pod::Pod::from_bytes(&format).ok_or_else(|| anyhow!("Failed to create param"))?;

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

    if use_local_resampler {
        while !shutdown_requested.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_secs(1));
        }
    } else {
        // The direct-copy path's servo steers the stream's own rate; apply it
        // from here, off the callback.
        apply_native_rate_until_shutdown(
            &stream,
            &main_loop,
            &shared.native_rate,
            &shutdown_requested,
        );
    }

    // Stop the thread loop before returning so a dropped writer cannot leave
    // background PipeWire control threads alive. The stream/core teardown then
    // happens naturally when these objects are dropped on the owning thread.
    main_loop.stop();
    Ok(())
}

/// The output stream's properties: its name, the device it is pinned to, its
/// channel positions and its processing quantum.
fn output_stream_properties(
    channel_count: u32,
    output_device: Option<&str>,
    channel_names: Option<&[String]>,
    quantum_frames: u32,
    output_rate: u32,
) -> pw::properties::PropertiesBox {
    let mut props = pw::properties::PropertiesBox::new();
    props.insert("node.name", "omniphony-vbap-renderer");
    props.insert("media.name", "VBAP Spatial Audio");

    if let Some(target) = output_device {
        for (key, value) in output_target_properties(target) {
            props.insert(key, value);
        }
        log::info!(
            "PipeWire output target: {} (pinned: no session move, no default-sink fallback)",
            target
        );
    }

    // audio.position tells PipeWire the spatial positions of the channels
    // (e.g. "FL,FR,C,LFE,BL,BR"), in its standard names (C→FC, BL→RL, BR→RR).
    if let Some(names) = channel_names {
        let positions = names
            .iter()
            .map(|n| to_pipewire_position(n))
            .collect::<Vec<_>>()
            .join(",");
        props.insert("audio.position", positions.as_str());
        props.insert("audio.channels", channel_count.to_string().as_str());
        log::info!("PipeWire channel positions: {}", positions);
    }

    // `node.latency` controls the PipeWire processing quantum (callback size),
    // not an abstract graph latency. Requesting the ring target (e.g. 500 ms)
    // here forced PipeWire into ~256 ms callbacks: the control loop then ran
    // once per 256 ms and the `callback/2` midpoint correction became a fixed
    // ~128 ms offset. The target latency must live in the sample ring
    // (target_buffer_fill), so request only the processing quantum here.
    props.insert(
        "node.latency",
        format!("{}/{}", quantum_frames, output_rate).as_str(),
    );
    log::debug!(
        "PipeWire stream properties configured: latency={}/{} (~{:.0}ms)",
        quantum_frames,
        output_rate,
        quantum_frames as f64 / output_rate as f64 * 1000.0
    );
    props
}

/// The `EnumFormat` the stream offers: interleaved f32 at `rate`.
fn output_format_pod(rate: u32, channels: u32) -> Result<Vec<u8>> {
    let mut audio_info = pw::spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(pw::spa::param::audio::AudioFormat::F32LE);
    audio_info.set_rate(rate);
    audio_info.set_channels(channels);
    Ok(pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(pw::spa::pod::Object {
            type_: pw::spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
            id: pw::spa::param::ParamType::EnumFormat.as_raw(),
            properties: audio_info.into(),
        }),
    )
    .map_err(|e| anyhow!("Failed to serialize format: {:?}", e))?
    .0
    .into_inner())
}

/// The graph's delay downstream of `stream`, in ms, when it reports one.
fn graph_delay_ms(stream: &pw::stream::Stream) -> Option<f32> {
    // `pw_stream_get_time_n` with our struct size: the library writes only as
    // much of `pw_time` as it knows, whatever version it is.
    let mut time = std::mem::MaybeUninit::<pw::sys::pw_time>::zeroed();
    let ok = unsafe {
        pw::sys::pw_stream_get_time_n(
            stream.as_raw_ptr(),
            time.as_mut_ptr(),
            std::mem::size_of::<pw::sys::pw_time>(),
        )
    };
    // Zero-initialised, so fully initialised whatever the call wrote.
    let time = unsafe { time.assume_init() };
    (ok == 0 && time.rate.denom > 0 && time.delay > 0)
        .then(|| time.delay as f32 / time.rate.denom as f32 * 1000.0)
}

/// Apply the callback's native rate to the stream as `SPA_PROP_rate` until
/// shutdown, every change of it and nothing else.
fn apply_native_rate_until_shutdown(
    stream: &pw::stream::StreamBox,
    main_loop: &pw::thread_loop::ThreadLoopRc,
    native_rate: &AtomicU32,
    shutdown_requested: &AtomicBool,
) {
    let stream_ptr = stream.as_raw_ptr();
    let loop_ptr = main_loop.as_raw_ptr();
    let mut last_applied_rate = 1.0f32;

    while !shutdown_requested.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(50));
        let mut rate = pipewire_rate_for_consume_adjust(f32::from_bits(
            native_rate.load(Ordering::Relaxed),
        ) as f64);
        // No deadband here: the observed behaviour follows the requested
        // ratio exactly.
        if rate.to_bits() == last_applied_rate.to_bits() {
            continue;
        }
        let result = unsafe {
            pw::sys::pw_thread_loop_lock(loop_ptr);
            // `pw_stream_set_control` is variadic in C: after the first (id,
            // n_values, values) triple it reads further triples until an id
            // of 0, hence the trailing terminator.
            let result = pw::sys::pw_stream_set_control(
                stream_ptr,
                SPA_PROP_RATE,
                1,
                &mut rate as *mut f32,
                0u32,
            );
            pw::sys::pw_thread_loop_unlock(loop_ptr);
            result
        };
        if result == 0 {
            last_applied_rate = rate;
            log::trace!("Applied rate adjustment: {:.6}", rate);
        } else {
            log::warn!("Failed to apply rate control: {}", result);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sink(value: &str, label: &str, client_id: Option<u32>) -> SinkCandidate {
        SinkCandidate {
            value: value.into(),
            label: label.into(),
            client_id,
        }
    }

    fn client(id: u32, pid: Option<u32>) -> ClientEntry {
        ClientEntry {
            id,
            pid,
            binary: None,
        }
    }

    /// Omniphony's own bridge sink (published by this process) is not an
    /// output device: rendering into it would feed the output back into
    /// the decoder input. Every other sink stays, including one whose owner
    /// cannot be resolved.
    #[test]
    fn device_list_leaves_out_this_processes_own_sinks() {
        let own_pid = 4242;
        let sinks = [
            sink("omniphony", "Omniphony", Some(7)),
            sink("alsa_output.dac", "USB DAC", Some(8)),
            sink("other_renderer", "Omniphony (other)", Some(9)),
            sink("loaded_by_daemon", "Null sink", None),
        ];
        let clients = [
            client(7, Some(own_pid)),
            client(8, Some(100)),
            client(9, Some(200)),
        ];
        let list = output_device_list(&sinks, &clients, own_pid);
        assert_eq!(
            list,
            vec![
                ("loaded_by_daemon".to_string(), "Null sink".to_string()),
                (
                    "other_renderer".to_string(),
                    "Omniphony (other)".to_string()
                ),
                ("alsa_output.dac".to_string(), "USB DAC".to_string()),
            ]
        );
    }

    /// Talks to the session's PipeWire daemon: run with `--ignored` to see
    /// the list a renderer would offer. Read-only (one registry snapshot).
    #[test]
    #[ignore = "needs a running PipeWire session"]
    fn live_device_list_answers_within_the_timeout() {
        let devices = list_pipewire_output_devices().expect("device list");
        for (value, label) in &devices {
            eprintln!("{value}\t{label}");
        }
    }

    #[test]
    fn device_list_sorts_by_label_and_drops_duplicate_names() {
        let sinks = [
            sink("b", "Beta", None),
            sink("a", "Alpha", None),
            sink("b", "Beta", None),
        ];
        let list = output_device_list(&sinks, &[], 1);
        assert_eq!(
            list,
            vec![
                ("a".to_string(), "Alpha".to_string()),
                ("b".to_string(), "Beta".to_string()),
            ]
        );
    }

    #[test]
    fn sink_candidates_are_named_audio_sinks_with_a_label_fallback() {
        let props = |pairs: &'static [(&'static str, &'static str)]| {
            move |key: &str| pairs.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
        };
        let s = sink_candidate_from_props(props(&[
            ("media.class", "Audio/Sink"),
            ("node.name", " dac "),
            ("node.nick", "DAC nick"),
            ("client.id", "12"),
        ]))
        .expect("a named sink");
        assert_eq!(s, sink("dac", "DAC nick", Some(12)));
        assert!(
            sink_candidate_from_props(props(&[
                ("media.class", "Audio/Source"),
                ("node.name", "mic"),
            ]))
            .is_none()
        );
        assert!(
            sink_candidate_from_props(props(&[("media.class", "Audio/Sink"), ("node.name", " ")]))
                .is_none()
        );
    }

    #[test]
    fn rate_control_is_the_spa_rate_property() {
        // 0x10c in <spa/param/props.h>; the old literal 3 named no rate control.
        assert_eq!(SPA_PROP_RATE, 0x10c);
    }

    #[test]
    fn rate_control_drains_faster_when_the_ring_is_too_full() {
        // Ring above target -> consume_adjust > 1 -> the adapter must consume
        // more input per cycle, which SPA_PROP_rate does for values above 1.
        assert!(pipewire_rate_for_consume_adjust(1.001) > 1.0);
        assert!(pipewire_rate_for_consume_adjust(0.999) < 1.0);
        assert_eq!(pipewire_rate_for_consume_adjust(1.0), 1.0);
    }

    #[test]
    fn output_target_is_stated_in_both_spellings() {
        let props = output_target_properties("bluez_output.AA_BB_CC.1");
        assert_eq!(
            props[0],
            ("target.object", "bluez_output.AA_BB_CC.1"),
            "WirePlumber 0.5 resolves target.object first"
        );
        assert_eq!(props[1], ("node.target", "bluez_output.AA_BB_CC.1"));
    }

    #[test]
    fn output_target_refuses_moves_and_default_fallback() {
        let props = output_target_properties("alsa_output.pci-0000_00_1f.3.analog-stereo");
        assert!(props.contains(&("node.dont-move", "true")));
        assert!(props.contains(&("node.dont-fallback", "true")));
    }
}
