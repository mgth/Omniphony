//! The Studio's panels, one module per group of sections. Each is an
//! `impl StudioSpike` block, so a panel reads top-down like the markup it
//! replaces while the state stays in one place.

pub mod about;
pub mod audio;
pub mod audio_input;
pub mod audio_output;
pub mod auto_tune;
pub mod binaural;
pub mod channel_editor;
pub mod connection;
pub mod diag_plot;
pub mod display;
pub mod drc;
pub mod footer;
pub mod gradient;
pub mod host_services;
pub mod hybrid;
pub mod info_modal;
pub mod latency;
pub mod lists;
pub mod log;
pub mod meters;
pub mod mpv_overlay;
pub mod object_test;
pub mod object_test_sheet;
pub mod profiles;
pub mod renderer;
pub mod renderer_perf;
pub mod resample_plot;
pub mod room;
pub mod row_glyphs;
pub mod scene_fx;
pub mod script_editor;
pub mod sofa_browser;
pub mod sources_2d;
pub mod speaker_editor;
pub mod speaker_layouts;
pub mod tools;
pub mod updates;
