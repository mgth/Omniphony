//! The Studio's panels, one module per group of sections. Each is an
//! `impl StudioSpike` block, so a panel reads top-down like the markup it
//! replaces while the state stays in one place.

pub mod audio;
pub mod connection;
pub mod display;
pub mod lists;
pub mod log;
pub mod tools;
