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
pub mod engine;
pub mod events;
pub mod osc;
pub mod overlay;
pub mod render;
pub mod renderer_build;
pub mod spatial;
pub mod virtual_bed;

pub use channel_layout::label_for_speaker_name;
/// The shared omniphony config (`~/.config/omniphony/config.yaml`) + its path,
/// re-exported so hosts default to the SAME config as the `orender` CLI + studio
/// (bridge path, layout, OSC settings, render params).
pub use renderer::config::{Config, default_config_path};
pub use engine::{Engine, OscOptions, RenderedAudio};
pub use osc::{ObjectMeta, OscSender};
pub use virtual_bed::{build_virtual_bed_events, build_virtual_bed_objects};
