//! Every atomic the PipeWire output callback publishes to.
//!
//! These are all the same kind of thing — a number written by the audio
//! callback and read by the diagnostics registry, the latency snapshot, or the
//! OSC meter bundle — and they were carried individually: twenty-four fields on
//! the writer, twenty-four parameters through `run_pipewire_loop` (of its
//! forty-seven), and a clone apiece before the callback could capture them.
//!
//! Grouping them is not only about the argument count. Each one carries a
//! comment explaining what it measures and, for several, the investigation that
//! put it there; scattered through a struct that is otherwise about streams and
//! resamplers, that knowledge reads as noise. Here it is the subject.
//!
//! Everything is `f32`/`f64` bits in an integer atomic, because that is what
//! the lock-free diag registry stores.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64};

#[derive(Clone)]
pub struct OutputTelemetry {
    /// Timestamp of the last successful write into the local ring buffer.
    pub last_write_ms: Arc<AtomicU64>,
    /// Smoothed measured total latency (ring + output FIFO + graph) in ms bits.
    pub measured_latency_ms_bits: Arc<AtomicU32>,
    /// Internal controller latency (ring + output FIFO midpoint) in ms bits.
    pub control_latency_ms_bits: Arc<AtomicU32>,
    /// Downstream graph latency as measured by pw_stream_get_time().delay (f32 ms bits).
    /// Updated every ~100 callbacks once the stream is stable.
    pub graph_latency_ms_bits: Arc<AtomicU32>,
    /// EMA-smoothed control latency in ms bits — the value the servo actually tracks.
    pub smoothed_control_latency_ms_bits: Arc<AtomicU32>,
    /// Ring-buffer level converted to ms — first of the three control-available components.
    pub avail_input_latency_ms_bits: Arc<AtomicU32>,
    /// Output FIFO (resampler output) converted back to input-domain ms — second component.
    pub output_fifo_latency_ms_bits: Arc<AtomicU32>,
    /// Pending samples inside the local resampler input — third component.
    pub resampler_pending_latency_ms_bits: Arc<AtomicU32>,
    /// Cumulative input-domain samples written via `write_samples`. Paired
    /// with `cumulative_drained_input_samples` below to give the PI a
    /// chunk-noise-free `control_available`: the difference (written -
    /// drained) is the true "samples in flight" between writer and the
    /// resampler's consumption point, with no chunk granularity artefacts.
    pub cumulative_written_input_samples: Arc<std::sync::atomic::AtomicU64>,
    /// Cumulative samples drained from the input ring per output callback
    /// (in input domain: callback_output_frames × channels / ratio). The
    /// counter is incremented continuously (not at chunk boundaries) so
    /// the running difference with `cumulative_written_input_samples` is
    /// smooth. Owned here to keep the Arc alive; the callback thread has
    /// its own clone for the fetch_add.
    #[allow(dead_code)]
    pub cumulative_drained_input_samples: Arc<std::sync::atomic::AtomicU64>,
    /// Diagnostic: the cumulative-flow control_available value (written -
    /// drained) actually fed to the PI. Exposed here for direct comparison
    /// with `output_ring_input_samples` (raw ring level) — if both show the
    /// same sawtooth, the flow-counter approach hasn't suppressed it.
    pub cumulative_flow_control_available_bits: Arc<std::sync::atomic::AtomicU64>,
    /// Diagnostic atomics published by the PipeWire output process callback.
    /// `f64` bits stored each callback; lets the Studio diag plot trace the
    /// drain cadence (interval between callbacks) to localise ring-level
    /// oscillations whose origin is downstream of the input/decoder/write
    /// chain.
    pub output_callback_dt_us_bits: Arc<std::sync::atomic::AtomicU64>,
    /// FIFO level between local resampler and PipeWire (input-domain
    /// samples). Post-d43ddab the callback drains only ~21 ms per cycle so
    /// this FIFO is no longer fully drained per call and can itself
    /// oscillate — strong candidate for the residual DAC sawtooth.
    pub output_fifo_input_domain_samples_bits: Arc<std::sync::atomic::AtomicU64>,
    /// Pending input samples held inside the resampler (input-domain).
    /// Complements the FIFO metric: any oscillation here can manifest as
    /// latency wobble downstream.
    pub output_resampler_pending_input_samples_bits: Arc<std::sync::atomic::AtomicU64>,
    /// Latency signals duplicated as f64-encoded u64 atomics so they can
    /// be selected in the generic diag plot alongside other metrics. The
    /// `*_ms_bits: AtomicU32` set above is kept untouched — it feeds the
    /// dedicated latency snapshot / OSC pipeline.
    pub diag_latency_smoothed_ms_bits: Arc<std::sync::atomic::AtomicU64>,
    pub diag_latency_control_ms_bits: Arc<std::sync::atomic::AtomicU64>,
    pub diag_rate_adjust_ppm_bits: Arc<std::sync::atomic::AtomicU64>,
    /// f64-encoded mirrors of the three `control_available` components in ms
    /// (ring / output-FIFO / resampler-pending). Same values fed to the
    /// latency OSC path, exposed here so they are selectable in the generic
    /// diag plot instead of needing a separate components plot.
    pub diag_latency_avail_input_ms_bits: Arc<std::sync::atomic::AtomicU64>,
    pub diag_latency_output_fifo_ms_bits: Arc<std::sync::atomic::AtomicU64>,
    pub diag_latency_resampler_pending_ms_bits: Arc<std::sync::atomic::AtomicU64>,
    /// Effective resample ratio expressed as ppm deviation from 1.0
    /// (i.e. (ratio - 1.0) × 1e6). Stays constant when the PI is paused;
    /// any modulation here under pause indicates a bug in the ratio
    /// freeze path. f64 bits.
    pub output_effective_ratio_ppm_bits: Arc<std::sync::atomic::AtomicU64>,
    /// Raw input ring-buffer length at callback entry, in samples. Same
    /// quantity as `avail_input_latency_ms_bits` but published from the
    /// callback at native cadence (not converted to ms, not sampled at
    /// send_meter_bundle). Lets us cross-check whether the 1 Hz on the
    /// components plot is real or a sampling/aliasing artefact.
    pub output_ring_input_samples_bits: Arc<std::sync::atomic::AtomicU64>,
    /// Current adaptive runtime state code (0=stable, 1=low-recover,
    /// 2=settling, 3=high-recover). Surfaces here as f64 bits so the diag
    /// plot can chart state transitions over time — if the state changes
    /// periodically while PI is paused, recovery is the source of any
    /// ring-level oscillation.
    pub runtime_state_code_bits: Arc<std::sync::atomic::AtomicU64>,
    /// Monotonic counter of ring samples discarded by ANY recovery path
    /// (low_recover_trim, hard_recover_high, recovery_reacquire_pending).
    /// Plot as a counter: a non-flat slope means recovery is firing.
    pub recovery_discard_count_bits: Arc<std::sync::atomic::AtomicU64>,
    /// Pacer diagnostics: cumulative samples drained from the pacer FIFO,
    /// cumulative zero-fills emitted when it underran, and its instantaneous
    /// level. Published by this backend like the rest, and shared with
    /// `PacerHandle` so the drain writes the same atomics the plot reads.
    pub pacer_drain_total: Arc<AtomicU64>,
    pub pacer_underrun_total: Arc<AtomicU64>,
    pub pacer_fifo_level: Arc<AtomicU64>,
}

impl OutputTelemetry {
    /// Diagnostic metric handles published by the PipeWire output backend.
    /// Each entry is registered in the global registry by the caller (one
    /// call to `DiagRegistry::register_external`); the backend updates the
    /// underlying atomics from the process callback. Adding a new metric in
    /// this list is the only change needed to surface it in the Studio diag
    /// plot — no other plumbing required.
    pub fn diag_handles(&self) -> Vec<sys::diag::DiagAtomicHandle> {
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

    pub fn new() -> Self {
        Self {
            last_write_ms: Arc::new(AtomicU64::new(0)),
            measured_latency_ms_bits: Arc::new(AtomicU32::new(0)),
            control_latency_ms_bits: Arc::new(AtomicU32::new(0)),
            graph_latency_ms_bits: Arc::new(AtomicU32::new(0)),
            smoothed_control_latency_ms_bits: Arc::new(AtomicU32::new(0)),
            avail_input_latency_ms_bits: Arc::new(AtomicU32::new(0)),
            output_fifo_latency_ms_bits: Arc::new(AtomicU32::new(0)),
            resampler_pending_latency_ms_bits: Arc::new(AtomicU32::new(0)),
            cumulative_written_input_samples: Arc::new(AtomicU64::new(0)),
            cumulative_drained_input_samples: Arc::new(AtomicU64::new(0)),
            cumulative_flow_control_available_bits: Arc::new(AtomicU64::new(0)),
            output_callback_dt_us_bits: Arc::new(AtomicU64::new(0)),
            output_fifo_input_domain_samples_bits: Arc::new(AtomicU64::new(0)),
            output_resampler_pending_input_samples_bits: Arc::new(AtomicU64::new(0)),
            diag_latency_smoothed_ms_bits: Arc::new(AtomicU64::new(0)),
            diag_latency_control_ms_bits: Arc::new(AtomicU64::new(0)),
            diag_rate_adjust_ppm_bits: Arc::new(AtomicU64::new(0)),
            diag_latency_avail_input_ms_bits: Arc::new(AtomicU64::new(0)),
            diag_latency_output_fifo_ms_bits: Arc::new(AtomicU64::new(0)),
            diag_latency_resampler_pending_ms_bits: Arc::new(AtomicU64::new(0)),
            output_effective_ratio_ppm_bits: Arc::new(AtomicU64::new(0)),
            output_ring_input_samples_bits: Arc::new(AtomicU64::new(0)),
            runtime_state_code_bits: Arc::new(AtomicU64::new(0)),
            recovery_discard_count_bits: Arc::new(AtomicU64::new(0)),
            pacer_drain_total: Arc::new(AtomicU64::new(0)),
            pacer_underrun_total: Arc::new(AtomicU64::new(0)),
            pacer_fifo_level: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl Default for OutputTelemetry {
    fn default() -> Self {
        Self::new()
    }
}
