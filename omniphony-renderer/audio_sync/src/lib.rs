//! Clock synchronisation core for the realtime output.
//!
//! The output stage holds a fixed end-to-end latency `L` between a source
//! frame reaching the renderer and the same frame being heard, while the
//! source runs on its own clock. Everything here is pure arithmetic on
//! timestamps and frame counters, so it can be driven by a real backend or by
//! the closed-loop plant in [`sim`] alike:
//!
//! - [`dll`]: second-order delay-locked loops that turn noisy, bursty
//!   `(time, frame count)` observations into a smooth rate and position;
//! - [`servo`]: the latency servo — feed-forward ratio from the two rates,
//!   a phase PI on the measured end-to-end latency, and the single
//!   start/realign rule;
//! - [`accounting`]: per-epoch counters for frames inserted or dropped between
//!   the capture point and the resampler, so the latency measurement stays
//!   exact when the pipeline loses or adds frames.
//!
//! See `docs/resampling-rework-plan.md` for the design.
//!
//! Times are seconds on one monotonic reference clock (`CLOCK_MONOTONIC` on
//! Linux), positions are frame counts. Nothing here allocates after
//! construction.

pub mod accounting;
pub mod dll;
pub mod envelope;
pub mod servo;
#[cfg(any(test, feature = "sim"))]
pub mod sim;
mod source;

pub use accounting::{Accounting, DiscontinuityEvent, DiscontinuityReason, Stage};
pub use dll::{Dll, DllConfig};
pub use envelope::ArrivalEnvelope;
pub use servo::{
    CallbackInput, CallbackPlan, Phase, Servo, ServoConfig, SourceObservation, Telemetry,
};
