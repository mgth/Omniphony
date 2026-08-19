//! What the output callback carries from one invocation to the next.
//!
//! The PipeWire process callback is a closure that captures around fifty
//! things, but only these ten are *state*: everything else is context — an
//! `Arc` it publishes through, or a scalar describing the negotiated format,
//! fixed for the life of the stream.
//!
//! Separating the two is what makes the callback divisible. A step lifted out
//! of it needs the state and little else, so it can take `&mut CallbackState`
//! instead of a dozen parameters — which is what the far-mode step and the
//! latency publication had to do the hard way before this existed.
//!
//! Nothing here is shared across threads: the callback owns it outright, and
//! that is why none of it is atomic. The atomics live in
//! [`crate::output_telemetry::OutputTelemetry`], which is the other half of the
//! picture — what the callback *publishes*, as opposed to what it *remembers*.

use std::time::Instant;

use crate::AdaptiveResamplingConfig;
use crate::adaptive_runtime::AdaptiveRuntimeState;
use crate::resampler_fifo::ResamplerFifoEngine;

/// The local resampler and the ratio it is currently running at.
///
/// `configured` is what the stream was built for; `effective` is what the servo
/// has moved it to. They are equal at rest and diverge while the PI is
/// correcting, so both are needed: the servo steers `effective`, and a reset
/// goes back to `configured`.
pub struct ResamplerState<R> {
    /// `None` when input and output rates match and no resampling is needed.
    pub engine: Option<R>,
    pub fifo: ResamplerFifoEngine,
    pub effective_ratio: f64,
    pub configured_ratio: f64,
}

/// Everything the callback remembers between invocations.
pub struct CallbackState<R> {
    pub resampler: ResamplerState<R>,
    /// The adaptive latency state machine — bands, phases, integrator.
    pub runtime: AdaptiveRuntimeState,
    /// Entry time of the previous callback, for the inter-callback interval.
    /// `None` before the first one.
    pub last_callback_at: Option<Instant>,
    /// Monotonic count of ring samples dropped by any recovery path. Published
    /// as a counter, so it only ever grows.
    pub recovery_discard_total: u64,
    /// Fractional accumulator for the Bresenham input-trigger schedule: the
    /// output callback drives the input stream at a rate that is not a whole
    /// multiple of its own, so the remainder is carried here.
    pub bresenham_acc: i64,
    /// Last target the loop logged, so a steady target is not re-logged every
    /// callback.
    pub logged_runtime_target: usize,
    /// How many callbacks between servo runs, refreshed from the live config.
    pub adaptive_update_interval: u64,
    /// The live config as this callback sees it.
    ///
    /// Refreshed once per callback with a non-blocking `try_lock`; on
    /// contention the previous copy stands until the next callback. Keeping a
    /// copy is what removed three blocking locks per callback from the realtime
    /// path, and it also means the whole callback sees one consistent config
    /// rather than re-reading a value that can change under it.
    pub adaptive_config: AdaptiveResamplingConfig,
}

impl<R> CallbackState<R> {
    pub fn new(
        engine: Option<R>,
        channel_count: usize,
        configured_ratio: f64,
        target_buffer_fill: usize,
        adaptive_config: AdaptiveResamplingConfig,
    ) -> Self {
        let mut runtime = AdaptiveRuntimeState::new(configured_ratio);
        runtime.activate_startup_low_recover();
        Self {
            resampler: ResamplerState {
                engine,
                fifo: ResamplerFifoEngine::new(channel_count),
                effective_ratio: configured_ratio,
                configured_ratio,
            },
            runtime,
            last_callback_at: None,
            recovery_discard_total: 0,
            bresenham_acc: 0,
            logged_runtime_target: target_buffer_fill,
            adaptive_update_interval: adaptive_config.update_interval_callbacks.max(1) as u64,
            adaptive_config,
        }
    }
}
