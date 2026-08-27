//! PI auto-tune.
//!
//! The detectors are ported and pinned against the frontend; the state machine
//! that consumes them is the next step. Until it lands nothing outside the
//! tests calls into here, which is deliberate — the port is landing with its
//! safety net first, before anything is deleted or wired to a live audio path.
#![allow(dead_code)]

pub mod detectors;
pub mod replay;
pub mod state_machine;
