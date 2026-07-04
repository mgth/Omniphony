//! Headless decode→render engine for the `orender` spatial audio renderer.
//!
//! This crate hosts the host-agnostic decode→render session: load a format
//! decoder bridge plugin, push raw packets through it, turn the decoded
//! PCM + spatial metadata into VBAP-rendered multichannel PCM, and (optionally)
//! expose OSC live control. It performs **no audio I/O** — no stdin input, no
//! PipeWire/ASIO output, no adaptive resampling. Those stay in the host:
//!
//! - the `orender` CLI binary wraps this with stdin input + PipeWire/ASIO output,
//! - `orender_ffi` (liborender.so) wraps it for the mpv integration, where mpv
//!   owns audio output and A/V sync.

pub mod bridge_loader;
pub mod channel_layout;
pub mod degraded;
pub mod engine;
pub mod events;
pub mod object_gen;
pub mod osc;
pub mod overlay;
pub mod phantom_extract;
mod phantom_spectral;
pub mod render;
pub mod renderer_build;
pub mod spatial;
mod stft;
pub mod virtual_bed;

pub use channel_layout::label_for_speaker_name;
pub use degraded::{DegradedReporter, start_degraded_reporter};
pub use engine::{Engine, OscOptions, RenderedAudio};
pub use osc::{ObjectMeta, OscSender};
/// The shared omniphony config (`~/.config/omniphony/config.yaml`) + its path,
/// re-exported so hosts default to the SAME config as the `orender` CLI + studio
/// (bridge path, layout, OSC settings, render params).
pub use renderer::config::{
    Config, RenderConfig, default_config_path, migrate_legacy_windows_config,
};
pub use virtual_bed::{build_virtual_bed_events, build_virtual_bed_objects};

/// Install the shared live-log logger used by the engine.
///
/// This is the SAME logger the `orender` CLI installs: it writes to stderr **and**
/// buffers records that the OSC listener streams to connected clients (Studio's
/// log panel). Embedding hosts that drive the engine over OSC — notably
/// `orender_ffi` (liborender.so for mpv) — must call this instead of a plain
/// `env_logger`, otherwise `log::*` diagnostics never reach OSC clients.
///
/// `level` is the initial runtime verbosity (OSC-adjustable later via
/// `/omniphony/control/log_level`). Returns `Err` if a global logger is already
/// installed; callers should guard with their own `Once` and ignore that error.
pub fn init_live_logging(level: log::LevelFilter, json: bool) -> Result<(), log::SetLoggerError> {
    sys::live_log::init_logger(level, json)
}
