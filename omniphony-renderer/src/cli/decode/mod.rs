mod decode_impl;
pub mod decoder_thread;
pub mod handler;
mod output_runtime_sync;
pub mod output;
mod sample_write;
mod spatial_metadata;
mod writer_lifecycle;

// Re-export the main render function
pub use decode_impl::cmd_render;
