//! Deterministic fixtures and analysis for Omniphony DSP validation.
//!
//! Dev-only: this crate is a path dev-dependency of `renderer`, and nothing in
//! the dependency graph of `orender` or `liborender` references it, so release
//! builds never compile it.
//!
//! It exists so that the null test, the criterion benches, and the future
//! worst-case-block-time gate all measure *the same* scenes. Duplicating scene
//! generation between those consumers is how they silently drift apart.

pub mod analysis;
pub mod dirs;
pub mod golden;
pub mod residual;
pub mod scene;
