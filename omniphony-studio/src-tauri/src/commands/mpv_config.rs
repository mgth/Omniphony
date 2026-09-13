//! Toggle `ad=orender` in the user's mpv config: the Tauri command surface.
//!
//! The rules — everything Studio writes lives between two marker comments, the
//! block goes in the global section rather than inside whatever profile happens
//! to be last, and a pre-existing `ad=` written by the user is never touched —
//! live in `omniphony_studio_core::host::commands::mpv_config`, with the tests
//! that hold them. This is the two wrappers that put them on the bridge.

use omniphony_studio_core::host::commands::mpv_config as core;

pub use core::MpvOrenderStatus;

/// Current state of `ad=orender` in the user's mpv config.
#[tauri::command]
pub fn mpv_orender_status() -> Result<MpvOrenderStatus, String> {
    core::mpv_orender_status()
}

/// Add, enable or comment out `ad=orender`, and report the resulting state.
#[tauri::command]
pub fn mpv_orender_set(enabled: bool) -> Result<MpvOrenderStatus, String> {
    core::mpv_orender_set(enabled)
}
