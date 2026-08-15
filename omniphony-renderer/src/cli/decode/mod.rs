mod bootstrap;
mod config_resolution;
pub mod decoder_thread;
pub mod handler;
mod idle_feed;
mod live_input;
pub mod output;
mod output_runtime_sync;
mod sample_write;
mod session_run;
mod spatial_metadata;
pub mod state;
mod writer_lifecycle;

// Re-export the main render function
pub use session_run::cmd_render;
