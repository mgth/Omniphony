//! Tauri command handlers, grouped into themed modules.
//!
//! Each command is a thin glue function: it takes the shared [`SharedState`] and
//! forwards a value to the renderer over OSC. They live here, split by feature
//! area, instead of in one large `main.rs`. `main.rs` glob-imports each module so
//! `tauri::generate_handler!` can keep listing commands by bare name.
//!
//! [`SharedState`]: crate::SharedState

pub mod app;
pub mod audio;
pub mod auto_tune;
pub mod binaural;
pub mod diag;
pub mod engine;
pub mod gain;
pub mod input;
pub mod layout_io;
pub mod mpv_config;
pub mod mpv_overlay;
pub mod orender;
pub mod profiles;
pub mod render;
pub mod resampling;
pub mod sofa_browser;
pub mod speakers;
