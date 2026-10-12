//! The output stage: a device held at a latency target
//! (`docs/latency-regulation.md`, `docs/resampling-rework-plan.md`).
//!
//! - [`core`]: the callback body shared by every backend;
//! - [`clock`]: the reference clock all timestamps are taken on;
//! - [`device`]: one output device, whatever the platform (what a host
//!   opens);
//! - `pipewire`: the PipeWire adapter (Linux);
//! - `cpal_adapter`: the cpal adapter (ASIO, CoreAudio; `cpal-check` builds it
//!   on Linux for type-checking);
//! - [`source_tap`]: the capture side's clock, published to the callback;
//! - [`telemetry`]: what the callback publishes.

pub mod clock;
pub mod core;
#[cfg(any(target_os = "windows", target_os = "macos", feature = "cpal-check"))]
pub mod cpal_adapter;
pub mod device;
#[cfg(target_os = "linux")]
pub mod pipewire;
pub mod source_tap;
pub mod telemetry;

pub use self::core::{DeviceTiming, OutputCore, OutputCoreConfig};
pub use clock::reference_now_s;
pub use device::{
    Negotiated, SyncOutput, SyncOutputConfig, list_output_devices, negotiate, output_host_label,
};
#[cfg(target_os = "linux")]
pub use pipewire::{PipewireSyncOutput, PipewireSyncOutputConfig};
pub use source_tap::SourceTap;
pub use telemetry::{SyncSnapshot, SyncTelemetry};
