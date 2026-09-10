//! Host-side services the Tauri host implemented in `src-tauri/src/` outside
//! the OSC listener: configuration files, runtime-environment overrides and
//! the control layer that turns UI actions into OSC messages.

pub mod audio_config;
pub mod commands;
pub mod config;
pub mod control;
pub mod peak_hold;
pub mod prefs;
pub mod runtime_env;
pub mod timing_stats;
