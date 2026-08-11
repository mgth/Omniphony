//! Per-layout speaker rendering state, extracted from `SpatialRenderer` so it
//! can be instantiated more than once (the cascaded binaural mode renders the
//! same pipeline onto a *virtual* speaker layout before binauralising it).
//!
//! The stage owns everything whose size or content derives from one speaker
//! layout: the per-band VBAP engines and their unified multi-band table, the
//! crossover filter bank and per-object filter memory, the per-speaker delay
//! lines, and the per-layout-sized scratch buffers. Everything shared across
//! render paths — most importantly `channel_states` (position ramps + gain
//! slew), which the binaural branch advances too — stays on `SpatialRenderer`
//! and is passed in explicitly.
//!
//! NOTE (cascaded mode, phase B): `ChannelState::interp_prev_gains` and the
//! gain slew are stateful *per channel* and layout-sized; two stages sharing
//! one `channel_states` in the same frame would double-slew and thrash the
//! interp re-seed check. Exactly one mix pass per `channel_states` per frame —
//! the cascade must give the virtual stage its own slew/interp storage.

use crate::crossover::{BiquadState, LR4CrossoverBank};
use crate::render_backend::MultiBandTable;
use crate::spatial_vbap::Gains;

use super::components::BandRenderer;

pub(super) struct SpeakerRenderStage {
    /// Output width of THIS stage's layout (total speakers, incl. LFE).
    pub(super) num_speakers: usize,
    /// Sample rate for slew and delay-target conversion.
    pub(super) sample_rate: u32,
    /// Per-band VBAP engines. Always ≥1 entry (the "all speakers" band when no
    /// crossover is configured). Each returns full-size `Gains`.
    pub(super) render_bands: Vec<BandRenderer>,
    /// Topology identity used to build the current band engines.
    pub(super) render_bands_topology_identity: usize,
    /// Merged multi-band cartesian table when all bands use cartesian
    /// evaluators (`None` → per-band path). `pub(super)`: tests force the
    /// per-band path by clearing it.
    pub(super) unified_table: Option<MultiBandTable>,
    /// `None` when `render_bands` has exactly 1 entry (no crossover active).
    pub(super) crossover_filter_bank: Option<LR4CrossoverBank>,
    /// Per-object filter states for the crossover bank, keyed by channel index.
    pub(super) crossover_filter_states: Vec<Option<Vec<BiquadState>>>,
    /// Reusable per-band scratch used only when collecting crossover timing.
    pub(super) crossover_band_scratch: [Vec<f32>; 8],
    /// Reusable per-object band-gain buffer (taken via `mem::take` per object).
    pub(super) band_gains_scratch: Vec<Gains>,
    /// `RampMode::Interp` only: pooled destination band gains for the object
    /// currently being rendered.
    pub(super) interp_end_scratch: Vec<Gains>,
    /// Per-speaker gain scratch — pre-allocated once, reused every frame.
    pub(super) speaker_gains_buf: Vec<f32>,
    /// Scratch routing gains for bed channels (full speaker-domain buffer).
    pub(super) bed_routing_gains_buf: Vec<f32>,
    /// Per-speaker delay lines — fixed 100 ms capacity, render-thread owned.
    pub(super) delay_lines: Vec<crate::delay_line::DelayLine>,
}
