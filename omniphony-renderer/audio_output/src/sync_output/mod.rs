//! The rewritten output stage (resampling rework, `docs/resampling-rework-plan.md`).
//!
//! It lives beside the legacy regulation (`adaptive_runtime`, `pipewire`,
//! `cpal_output`) until the cutover, which deletes that code.
//!
//! - [`core`]: the callback body shared by every backend;
//! - [`clock`]: the reference clock all timestamps are taken on;
//! - `pipewire`: the PipeWire adapter (Linux);
//! - `cpal_adapter`: the cpal adapter (ASIO, CoreAudio; `cpal-check` builds it
//!   on Linux for type-checking);
//! - [`source_tap`]: the capture side's clock, published to the callback;
//! - [`telemetry`]: what the callback publishes.

pub mod clock;
pub mod core;
#[cfg(any(target_os = "windows", target_os = "macos", feature = "cpal-check"))]
pub mod cpal_adapter;
#[cfg(target_os = "linux")]
pub mod pipewire;
pub mod source_tap;
pub mod telemetry;

pub use self::core::{DeviceTiming, OutputCore, OutputCoreConfig};
pub use clock::reference_now_s;
#[cfg(target_os = "linux")]
pub use pipewire::{PipewireSyncOutput, PipewireSyncOutputConfig};
pub use source_tap::SourceTap;
pub use telemetry::{SyncSnapshot, SyncTelemetry};
