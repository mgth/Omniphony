//! Realtime audio primitives for the output stage.
//!
//! - [`ring`]: a single-producer single-consumer ring of interleaved `f32`
//!   frames. Bulk copies, whole frames only, absolute frame counters on both
//!   sides — the counters are what the latency servo reads as "written" and
//!   "consumed".
//! - [`resampler`]: a variable-ratio polyphase windowed-sinc resampler that
//!   produces exactly the frames a device callback asks for, reads exactly
//!   the source frames it needs, and reports its fractional source position
//!   exactly (Q32.32), so that position *is* the played-frames counter.
//!
//! Both allocate at construction only. The design comes from spike S3
//! (`docs/resampling-rework/spike-s3-resampler.md`).

pub mod resampler;
pub mod ring;

pub use resampler::{Design, DriftResampler, Position};
pub use ring::{Consumer, Producer, frame_ring};
