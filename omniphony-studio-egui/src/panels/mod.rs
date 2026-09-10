//! The Studio's panels, one module per group of sections. Each is an
//! `impl StudioSpike` block, so a panel reads top-down like the markup it
//! replaces while the state stays in one place.

pub mod audio;
pub mod audio_input;
pub mod audio_output;
pub mod binaural;
pub mod connection;
pub mod display;
pub mod drc;
pub mod latency;
pub mod lists;
pub mod log;
pub mod object_test;
pub mod object_test_sheet;
pub mod profiles;
pub mod renderer;
pub mod room;
pub mod sources_2d;
pub mod speaker_editor;
pub mod tools;
