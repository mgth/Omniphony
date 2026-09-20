//! PI auto-tune.
//!
//! `state_machine` is a port of the frontend's FSM, pinned against recorded
//! runs of it (`replay`). `runner` drives that machine against the live
//! renderer, and is only reachable when the `rust_auto_tune` config flag is
//! on — the frontend implementation is still the default.
#![allow(dead_code)]

pub mod detectors;
pub mod replay;
pub mod runner;
pub mod state_machine;
pub mod wire;
