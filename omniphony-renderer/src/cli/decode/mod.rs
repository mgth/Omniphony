mod bootstrap;
mod config_resolution;
pub mod decoder_thread;
pub mod handler;
mod output_runtime_sync;
pub mod output;
mod sample_write;
mod session_run;
mod spatial_metadata;
pub mod state;
mod virtual_bed;
mod writer_lifecycle;

// Re-export the main render function
pub use session_run::cmd_render;
