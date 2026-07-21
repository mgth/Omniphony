//! Spatial audio renderer using VBAP
//!
//! This module handles rendering spatial object audio to speaker channels
//! using Vector-Based Amplitude Panning (VBAP).
//!
//! # Architecture
//!
//! 1. **Initialization**: Create `SpatialRenderer` with speaker layout
//! 2. **Per-Frame Rendering**: For each decoded audio frame with spatial metadata:
//!    - Extract object positions from metadata
//!    - Convert ADM coordinates to spherical (az/el)
//!    - Get VBAP gains for each object
//!    - Mix object audio into speaker channels
//! 3. **Output**: Return speaker-rendered audio samples
//!
//! # Example
//!
//! ```ignore
//! use omniphony_renderer::spatial_renderer::SpatialRenderer;
//! use omniphony_renderer::speaker_layout::SpeakerLayout;
//! use omniphony_renderer::spatial_vbap::{DistanceModel, VbapTableMode};
//!
//! // Load speaker layout
//! let layout = SpeakerLayout::preset("7.1.4")?;
//!
//! // Create renderer with VBAP configuration
//! let renderer = SpatialRenderer::new(
//!     layout,
//!     48000,                 // sample rate (Hz)
//!     1,                     // azimuth resolution
//!     1,                     // elevation resolution
//!     0.25,                  // spread resolution (0.0 = single table, >0 = dynamic spread)
//!     2.0,                   // polar distance max
//!     VbapTableMode::Polar,  // precomputed table mode
//!     true,                  // allow_negative_z
//!     DistanceModel::Linear, // distance attenuation model
//!     false,                 // spread_from_distance (false = use spread_min/spread_max)
//!     1.0,                   // spread_distance_range (distance where spread reaches 0)
//!     1.0,                   // spread_distance_curve (1.0 = linear, 2.0 = quadratic)
//!     0.0,                   // spread_min
//!     1.0,                   // spread_max
//!     false,                 // log_object_positions
//!     [1.0, 2.0, 0.5],       // room_ratio [width, length, height]
//!     2.0,                   // room_ratio_rear
//!     0.5,                   // room_ratio_center_blend
//!     0.0,                   // master_gain_db
//!     false,                 // auto_gain
//!     false,                 // use_loudness
//!     false,                 // distance_diffuse
//!     1.0,                   // distance_diffuse_threshold
//!     1.0,                   // distance_diffuse_curve
//!     omniphony_renderer::live_params::PreferredEvaluationMode::PrecomputedPolar, // bridge preferred mode
//!     omniphony_renderer::live_params::LiveEvaluationMode::PrecomputedPolar,      // initial live selection
//!     31,                    // cartesian default x size
//!     31,                    // cartesian default y size
//!     15,                    // cartesian default z size
//! )?;
//!
//! // Render objects for a frame (in decode loop)
//! let speaker_samples = renderer.render_frame(
//!     &decoded_access_unit,
//!     &spatial_metadata,
//!     bed_channel_count,
//! )?;
//! ```

use crate::crossover::{BiquadState, LR4CrossoverBank};
use crate::live_params::{RampMode, RendererControl};
use crate::ramp_strategy::{
    PositionRampStrategy, RampContext, RampProgress, RampRenderParams, RampStrategy, RampTarget,
};
use crate::render_backend::MultiBandTable;
use crate::spatial_vbap::{DistanceModel, Gains};
use anyhow::Result;
use std::sync::Arc;

mod components;
mod construction;
use components::{BandRenderer, ChannelState, evaluation_build_config, split_bands};
pub use components::{RenderedFrame, SpatialChannelEvent};

/// Snapshot of `LiveParams` taken at the start of each render frame.
///
/// Holding this snapshot (rather than keeping the `RwLock` locked) allows the
/// OSC listener to write new values at any time without blocking the render
/// thread between samples.
struct LiveSnapshot<'a> {
    master_gain: f32,
    object_params: &'a [crate::live_params::ObjectLiveParams],
    ramp_mode: RampMode,
    use_loudness: bool,
    auto_gain: bool,
    auto_gain_ceiling_db: f32,
    speaker_params: &'a [crate::live_params::SpeakerLiveParams],
    room_ratio: [f32; 3],
    room_ratio_rear: f32,
    room_ratio_lower: f32,
    room_ratio_center_blend: f32,
    use_distance_diffuse: bool,
    distance_diffuse_threshold: f32,
    distance_diffuse_curve: f32,
}

/// Put the calling thread's FPU in flush-to-zero / denormals-are-zero mode,
/// once per thread (issue #154).
///
/// Every recursive DSP path in the renderer (FDN delay lines and damping,
/// reflection-tap smoothing, air-absorption one-poles, biquad states) decays
/// exponentially toward zero after input stops; without FTZ those tails enter
/// denormal range, where each multiply can cost 10–100× on x86 — a CPU spike
/// exactly when the stream goes silent. Flushing to zero is the standard
/// audio-DSP trade: values below ~1e-38 are ~−760 dBFS, far beyond audibility.
///
/// This claims the FP environment of the host's thread (mpv's decode thread,
/// the CLI engine), which is deliberate: that thread runs our DSP, and FTZ is
/// the conventional processing mode for realtime audio. On unknown
/// architectures this is a no-op (correct, just without the protection).
#[inline]
fn ensure_denormals_flushed() {
    use std::cell::Cell;
    thread_local! {
        static CLAIMED: Cell<bool> = const { Cell::new(false) };
    }
    CLAIMED.with(|claimed| {
        if claimed.get() {
            return;
        }
        claimed.set(true);
        #[cfg(target_arch = "x86_64")]
        unsafe {
            // MXCSR bits: FTZ = 15, DAZ = 6 (DAZ exists on every x86-64 CPU
            // this crate targets). Inline asm instead of the deprecated
            // `_mm_setcsr` intrinsics: the write is opaque to LLVM, which is
            // the point — the changed FP mode must not be reasoned away.
            let mut mxcsr: u32 = 0;
            std::arch::asm!("stmxcsr [{}]", in(reg) &mut mxcsr, options(nostack));
            mxcsr |= (1 << 15) | (1 << 6);
            std::arch::asm!("ldmxcsr [{}]", in(reg) &mxcsr, options(nostack));
        }
        #[cfg(target_arch = "aarch64")]
        unsafe {
            // FPCR.FZ (bit 24): flush-to-zero for f32/f64 (Apple Silicon
            // builds). Read-modify-write keeps the rounding mode intact.
            let mut fpcr: u64;
            std::arch::asm!("mrs {}, fpcr", out(reg) fpcr);
            fpcr |= 1 << 24;
            std::arch::asm!("msr fpcr, {}", in(reg) fpcr);
        }
    });
}

/// Spatial audio renderer using VBAP
/// Time for a full-scale (0 → unity) gain change to complete, in seconds.
/// Every per-channel gain step is slewed at this constant rate so metadata
/// jumps, mute toggles and channel-plan transitions never click
/// (`docs/channel-object-contract.md`, phase 2b).
pub const GAIN_SLEW_SECS: f32 = 0.02;

/// Per-input-channel routing decision, in the layout-independent label
/// language of `docs/channel-object-contract.md`: a `Direct` channel is
/// one-hot routed to the speaker its label resolves to in the active topology
/// (skipped when the layout has none); a `Virtual` channel renders through
/// the VBAP/object path from its metadata events.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ChannelRoute {
    Direct(bridge_api::RChannelLabel),
    Virtual,
}

pub struct SpatialRenderer {
    /// Number of output speakers (total, including non-spatialized like LFE)
    num_speakers: usize,

    /// Spread resolution for multi-table VBAP (0.0 = single table)
    spread_resolution: f32,

    /// Bed channel IDs in PCM order (e.g. [3, 0, 1, 2, ...]).
    /// Updated when format metadata changes and read lock-free in the audio thread.
    channel_routing: arc_swap::ArcSwap<Vec<ChannelRoute>>,

    /// Flag for first render (for detailed logging)
    first_render: std::sync::atomic::AtomicBool,

    /// Frame counter for periodic logging
    frame_counter: std::sync::atomic::AtomicU64,

    /// Per-channel state (movement detection + gain ramping)
    channel_states: parking_lot::Mutex<std::collections::HashMap<usize, ChannelState>>,

    /// Sample rate for ramp time calculations
    sample_rate: u32,

    /// Distance attenuation model
    distance_model: DistanceModel,

    /// Enable detailed logging of object positions (ramping and movement)
    log_object_positions: bool,

    /// Dialog normalization gain in linear (1.0 = no normalization)
    /// Set dynamically when dialogue_level is received from the stream
    loudness_gain: std::sync::atomic::AtomicU32,

    /// `true` once auto-gain has lowered the master gain at least once this
    /// session. The reduction itself lives in `LiveParams::master_gain`; this
    /// flag only gates the end-of-stream summary.
    auto_gain_triggered: std::sync::atomic::AtomicBool,

    /// Shared live parameters + speaker layout + pending VBAP swap.
    control: Arc<RendererControl>,

    /// Per-speaker gain scratch buffer — pre-allocated once, reused every frame.
    speaker_gains_buf: Vec<f32>,

    /// Scratch snapshot of live per-object params, indexed by input channel.
    object_params_buf: Vec<crate::live_params::ObjectLiveParams>,

    /// Scratch snapshot of live per-speaker params, indexed by output speaker.
    speaker_params_buf: Vec<crate::live_params::SpeakerLiveParams>,

    /// Last integrated generation for per-object live params.
    object_params_generation_seen: u64,

    /// Last integrated generation for per-speaker live params.
    speaker_params_generation_seen: u64,

    /// Scratch routing gains for bed channels.
    ///
    /// Keep this as a reusable full speaker-domain buffer instead of collapsing beds
    /// back to a hardcoded one-speaker fast path. Bed routing is expected to evolve
    /// beyond strict 1:1 mapping so we can simulate missing or non-standard speakers
    /// without changing the downstream mix model again.
    bed_routing_gains_buf: Vec<f32>,

    /// Per-speaker delay lines — one per speaker, fixed 100 ms capacity.
    /// Owned exclusively by the render thread; no locking required.
    delay_lines: Vec<crate::delay_line::DelayLine>,

    /// Optional contributor-provided ramp strategy override.
    ramp_strategy_override: Option<Arc<dyn RampStrategy>>,

    /// Independent binaural (headphone) output stage. Used only when
    /// `LiveParams::binaural.output_mode == OutputMode::Binaural`; otherwise the
    /// classic VBAP path runs and this holds no live state.
    binaural: crate::binaural::BinauralRenderer,

    /// Scratch per-channel world positions for the binaural path (reused).
    binaural_pos_buf: Vec<[f64; 3]>,

    /// Scratch per-channel gains for the binaural path (reused).
    binaural_gain_buf: Vec<f32>,

    /// Scratch per-channel "direct" flags for the binaural path (reused):
    /// beds mapped to a `spatialize: false` speaker (the LFE) feed both ears
    /// equally instead of being HRTF-spatialized.
    binaural_direct_buf: Vec<bool>,

    /// Per-band VBAP engines.  Always has at least one entry (the "all speakers" band when
    /// no crossover is configured).  Each engine returns full-size `Gains` (`num_speakers`).
    render_bands: Vec<BandRenderer>,
    /// Topology identity used to build the current crossover band engines.
    render_bands_topology_identity: usize,

    /// Crossover filter bank for splitting objects into frequency bands.
    /// Unified multi-band cartesian table: when crossover is active and all
    /// bands use a cartesian evaluator, the per-band tables are merged so a
    /// lookup localises the cell once for every band. `None` → per-band path.
    unified_table: Option<MultiBandTable>,
    /// `None` when `render_bands` has exactly 1 entry (no crossover active).
    crossover_filter_bank: Option<LR4CrossoverBank>,

    /// Per-object filter states for the crossover bank, keyed by channel index.
    crossover_filter_states: Vec<Option<Vec<BiquadState>>>,

    /// Reusable per-band scratch used only when collecting crossover timing.
    crossover_band_scratch: [Vec<f32>; 8],

    /// Reusable per-object band-gain buffer. Taken via `mem::take` at the start
    /// of each object's render and put back afterwards, so the per-object VBAP
    /// gain vector is allocated once and reused across objects and frames
    /// instead of a fresh `Vec` per object per frame.
    band_gains_scratch: Vec<Gains>,

    /// `RampMode::Interp` only: pooled destination band gains for the object
    /// currently being rendered (one entry per render band). Reused each object.
    interp_end_scratch: Vec<Gains>,
}

impl SpatialRenderer {
    /// Fill `out` with one full-size `Gains` per render band at `position`. Uses
    /// the unified multi-band table (one cell localisation for all bands) when
    /// available, else falls back to a per-band lookup. Free-standing (borrows
    /// only the two fields it needs) so it composes with the other per-channel
    /// mutable borrows held across the render arms.
    fn fill_band_gains(
        unified: &Option<MultiBandTable>,
        render_bands: &[BandRenderer],
        render_params: crate::ramp_strategy::RampRenderParams,
        position: [f64; 3],
        size: [f32; 3],
        out: &mut Vec<Gains>,
    ) {
        out.clear();
        if let Some(table) = unified {
            table.sample_into(position.map(|v| v as f32), out);
        } else {
            out.extend(
                render_bands
                    .iter()
                    .map(|b| b.compute_gains(render_params, position, size)),
            );
        }
    }

    /// Whether auto-gain has lowered the master gain at least once this session.
    pub fn auto_gain_triggered(&self) -> bool {
        self.auto_gain_triggered
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Set loudness metadata correction gain based on `dialogue_level` from the stream.
    ///
    /// The reference level is -31 dBFS. The gain is calculated as:
    /// gain_db = -31 - dialogue_level
    ///
    /// For example:
    /// - dialogue_level = -27 dBFS → gain = -4 dB
    /// - dialogue_level = -31 dBFS → gain = 0 dB (reference)
    /// - dialogue_level = -24 dBFS → gain = -7 dB
    pub fn set_loudness(&self, dialogue_level: i8) {
        const REFERENCE_LEVEL: i32 = -31;
        let gain_db = REFERENCE_LEVEL - (dialogue_level as i32);
        let gain_linear = 10.0_f32.powf(gain_db as f32 / 20.0);
        self.loudness_gain
            .store(gain_linear.to_bits(), std::sync::atomic::Ordering::Relaxed);
        self.control.live.write().dialogue_level = Some(dialogue_level);
        log::info!(
            "Dialog normalization: dialogue_level={} dBFS → gain={} dB (linear: {:.4})",
            dialogue_level,
            gain_db,
            gain_linear
        );
    }

    /// Set the bed channel IDs in PCM channel order.
    ///
    /// Must be called once when the first metadata arrives, before any call to `render_frame`.
    /// The mapping is stable for the lifetime of the stream.
    pub fn configure_channel_routing(&self, routes: &[ChannelRoute]) {
        self.channel_routing
            .store(std::sync::Arc::new(routes.to_vec()));
        log::debug!("Renderer channel routing configured: {:?}", routes);
    }

    /// Return the shared `RendererControl` Arc so that `OscSender` can hold it.
    pub fn renderer_control(&self) -> Arc<RendererControl> {
        Arc::clone(&self.control)
    }

    pub fn set_ramp_strategy(&mut self, strategy: Arc<dyn RampStrategy>) {
        self.ramp_strategy_override = Some(strategy);
        self.reset_runtime_state();
    }

    pub fn clear_ramp_strategy(&mut self) {
        self.ramp_strategy_override = None;
        self.reset_runtime_state();
    }

    fn ramp_context(&self, live: &LiveSnapshot<'_>) -> RampContext {
        RampContext::new(RampRenderParams {
            room_ratio: live.room_ratio,
            room_ratio_rear: live.room_ratio_rear,
            room_ratio_lower: live.room_ratio_lower,
            room_ratio_center_blend: live.room_ratio_center_blend,
            use_distance_diffuse: live.use_distance_diffuse,
            distance_diffuse_threshold: live.distance_diffuse_threshold,
            distance_diffuse_curve: live.distance_diffuse_curve,
            distance_model: self.distance_model,
        })
    }

    /// Clear cached per-channel spatial/ramp state after a decoder reset or
    /// stream restart so stale object positions cannot leak into subsequent
    /// rendering.
    pub fn reset_runtime_state(&self) {
        self.channel_states.lock().clear();
        self.first_render
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Update channel states from format-agnostic spatial events.
    ///
    /// Called internally from `render_frame` when pending events are present.
    /// The `channel_idx` and `is_bed` fields of each event must already be
    /// resolved by the caller (see `SpatialChannelEvent`).
    fn update_metadata(
        &self,
        events: &[SpatialChannelEvent],
        strategy: &dyn RampStrategy,
        ctx: &RampContext,
    ) -> Result<()> {
        let mut channel_states = self.channel_states.lock();

        for event in events {
            let state = channel_states
                .entry(event.channel_idx)
                .or_insert_with(ChannelState::default);

            if let Some(gain) = event.gain_db {
                state.gain_db = gain;
            }
            if let Some(ramp_length) = event.ramp_length {
                state.ramp.ramp_length = ramp_length as u64;
            }

            // Beds are routed directly to speakers — no position state needed.
            if event.is_bed {
                continue;
            }

            // Per-event size becomes the new ramp target. `None` = unchanged.
            let new_target_size = event.size.unwrap_or(state.ramp.target_size);
            let size_changed = state.ramp.target_size != new_target_size;

            if let Some(target_position) = event.position {
                if state.ramp.target_position != target_position || size_changed {
                    let current_ramp_length = state.ramp.ramp_length;
                    if self.log_object_positions {
                        let remaining_units = state.ramp.remaining_ramp_units.unwrap_or(0);
                        let sample_pos = event.sample_pos.unwrap_or(0);
                        if state.ramp.target_position != target_position {
                            log::info!(
                                "  Obj ch{:2}: sample_pos {} remaining {} - Starting ramp over {} samples (~{}ms)",
                                event.channel_idx,
                                sample_pos,
                                remaining_units,
                                state.ramp.ramp_length,
                                state.ramp.ramp_length as f32 / self.sample_rate as f32 * 1000.0
                            );
                        }
                    }
                    strategy.update_target(
                        &mut state.ramp,
                        RampTarget {
                            position: target_position,
                            size: new_target_size,
                            ramp_length: current_ramp_length,
                        },
                        event.sample_pos,
                        ctx,
                    );
                }
            } else if size_changed {
                state.ramp.target_size = new_target_size;
                if state.ramp.remaining_ramp_units.is_none() {
                    state.ramp.current_size = new_target_size;
                }
            }
        }

        Ok(())
    }

    /// Render audio objects to speaker channels for a single frame
    ///
    /// This function takes the raw PCM data from the decoder (bed + objects),
    /// separates the object channels based on bed_indices, applies VBAP panning
    /// to object channels, and routes bed channels directly to speakers.
    ///
    /// # Arguments
    ///
    /// * `pcm_data` - Decoded PCM samples [sample_idx][channel_idx]
    /// * `metadata` - Spatial object metadata (positions, gains, etc.)
    /// * `total_channels` - Total number of channels in pcm_data (bed + objects)
    /// * `bed_indices` - Indices of channels that are bed channels (e.g., [3] for LFE only)
    ///
    /// # Returns
    ///
    /// Interleaved speaker samples: [sample_idx][speaker_idx]
    ///
    /// # Notes
    ///
    /// - Channels in `bed_indices` are copied directly to corresponding speakers
    /// - All other channels are treated as objects and spatialized with VBAP
    /// - Output has self.num_speakers channels
    /// - MAX_CHANNELS is 16 (decoder maximum)
    /// Render a frame of spatial audio into a pre-allocated output buffer.
    ///
    /// The caller provides `samples_buf` — a `Vec<f32>` that will be cleared,
    /// resized to `sample_length × num_speakers`, and filled with interleaved
    /// speaker audio.  Passing back the `RenderedFrame::samples` from the
    /// *previous* call eliminates the per-frame heap allocation after warm-up:
    ///
    /// ```ignore
    /// let mut buf = Vec::new();
    /// loop {
    ///     let frame = renderer.render_frame(pcm, channels, events, buf)?;
    ///     // … consume frame.samples …
    ///     buf = frame.samples; // donate back for next iteration
    /// }
    /// ```
    pub fn render_frame(
        &mut self,
        input_pcm: &[f32],
        input_channel_count: usize,
        pending_events: &[SpatialChannelEvent],
        samples_buf: Vec<f32>,
        measure_breakdown: bool,
    ) -> Result<RenderedFrame> {
        // The render thread belongs to the host (mpv's decode thread, the CLI
        // engine, …), so the FP environment is claimed here, at the DSP entry
        // point, rather than at thread creation.
        ensure_denormals_flushed();

        // ── 0. Independent binaural (headphone) path ─────────────────────────
        // When headphone output is selected, bypass the entire VBAP / crossover /
        // speaker chain and emit a 2-channel frame. The branch is taken below,
        // after `update_metadata` has applied the pending events (new ramp
        // targets); the branch itself advances each object's position ramp for
        // the block. Flag it here.
        let binaural_active = matches!(
            self.control.live.read().binaural.output_mode,
            crate::live_params::OutputMode::Binaural
        );

        // ── 1. Load the current immutable render topology and keep band engines in sync ──
        let topology_guard = self.control.active_topology();
        let topology = &*topology_guard;
        let topology_identity = std::sync::Arc::as_ptr(&topology_guard) as usize;
        self.refresh_crossover_for_topology(topology_identity, &topology.speaker_layout)?;

        // ── 1. Snapshot live params so we hold the read lock for as short a time as possible ──
        let live_position_interpolation;
        let live = {
            let g = self.control.live.read();
            live_position_interpolation = g.evaluation.position_interpolation;
            let object_params_generation = self
                .control
                .object_params_generation
                .load(std::sync::atomic::Ordering::Relaxed);
            let speaker_params_generation = self
                .control
                .speaker_params_generation
                .load(std::sync::atomic::Ordering::Relaxed);

            if self.object_params_generation_seen != object_params_generation {
                if self.object_params_buf.len() < input_channel_count {
                    self.object_params_buf.resize(
                        input_channel_count,
                        crate::live_params::ObjectLiveParams::default(),
                    );
                }
                for params in self.object_params_buf.iter_mut().take(input_channel_count) {
                    *params = crate::live_params::ObjectLiveParams::default();
                }
                for (&idx, params) in &g.objects {
                    if idx >= self.object_params_buf.len() {
                        self.object_params_buf
                            .resize(idx + 1, crate::live_params::ObjectLiveParams::default());
                    }
                    self.object_params_buf[idx] = params.clone();
                }
                self.object_params_generation_seen = object_params_generation;
            } else if self.object_params_buf.len() < input_channel_count {
                self.object_params_buf.resize(
                    input_channel_count,
                    crate::live_params::ObjectLiveParams::default(),
                );
            }

            if self.speaker_params_generation_seen != speaker_params_generation {
                if self.speaker_params_buf.len() < self.num_speakers {
                    self.speaker_params_buf.resize(
                        self.num_speakers,
                        crate::live_params::SpeakerLiveParams::default(),
                    );
                }
                for params in self.speaker_params_buf.iter_mut().take(self.num_speakers) {
                    *params = crate::live_params::SpeakerLiveParams::default();
                }
                for (&idx, params) in &g.speakers {
                    if idx < self.speaker_params_buf.len() {
                        self.speaker_params_buf[idx] = params.clone();
                    }
                }
                self.speaker_params_generation_seen = speaker_params_generation;
            }
            LiveSnapshot {
                master_gain: g.master_gain,
                object_params: &self.object_params_buf[..input_channel_count],
                ramp_mode: g.ramp_mode,
                use_loudness: g.use_loudness,
                auto_gain: g.auto_gain,
                auto_gain_ceiling_db: g.auto_gain_ceiling_db,
                speaker_params: &self.speaker_params_buf[..self.num_speakers],
                room_ratio: g.room_ratio,
                room_ratio_rear: g.room_ratio_rear,
                room_ratio_lower: g.room_ratio_lower,
                room_ratio_center_blend: g.room_ratio_center_blend,
                use_distance_diffuse: g.use_distance_diffuse,
                distance_diffuse_threshold: g.distance_diffuse_threshold,
                distance_diffuse_curve: g.distance_diffuse_curve,
            }
        };
        // Push the live read-time interpolation flag into the precomputed
        // evaluators and the unified table. This flag only selects nearest-cell
        // vs trilinear at lookup time; the table content is independent of it, so
        // toggling it no longer rebuilds the table (the OSC handler dropped its
        // `trigger_layout_recompute`). We sync the current value every frame —
        // just a handful of relaxed atomic stores.
        for band in &self.render_bands {
            if let Some(engine) = band.engine() {
                engine.set_position_interpolation(live_position_interpolation);
            }
        }
        if let Some(table) = self.unified_table.as_ref() {
            table.set_position_interpolation(live_position_interpolation);
        }

        let ramp_context = self.ramp_context(&live);
        let ramp_strategy_override = self.ramp_strategy_override.clone();
        // The ramp always interpolates the object POSITION across the block; the
        // `position_interpolation` flag now only selects how the table is read at
        // that position — nearest cell (1 lookup) vs trilinear (8 lookups) — via
        // the evaluator's `interpolate` flag, which tracks the live boolean
        // (toggling it triggers a layout recompute). The old GainTable strategy
        // (frozen position + a per-sample gain lerp the render path never read)
        // is gone.
        static POSITION_STRATEGY: PositionRampStrategy = PositionRampStrategy;
        let ramp_strategy: &dyn RampStrategy = if let Some(ref strategy) = ramp_strategy_override {
            strategy.as_ref()
        } else {
            &POSITION_STRATEGY
        };

        if !pending_events.is_empty() {
            self.update_metadata(pending_events, ramp_strategy, &ramp_context)?;
        }

        // Derive sample count from slice length and channel count.
        let sample_length = if input_channel_count > 0 {
            input_pcm.len() / input_channel_count
        } else {
            0
        };

        // Snapshot the routing once for this frame via ArcSwap: no mutex and no
        // Vec clone.
        let channel_routing = self.channel_routing.load_full();
        let active_layout = &topology.speaker_layout;
        let active_label_to_speaker = &topology.label_to_speaker;

        // ── Binaural branch ──────────────────────────────────────────────────
        // Build per-channel world positions (beds → speaker direction, objects →
        // ramp position) and gains, then render to interleaved stereo. Bypasses
        // the entire speaker/VBAP path below.
        if binaural_active {
            self.binaural_pos_buf.clear();
            self.binaural_pos_buf
                .resize(input_channel_count, [0.0, 1.0, 0.0]);
            self.binaural_gain_buf.clear();
            self.binaural_gain_buf.resize(input_channel_count, 0.0);
            self.binaural_direct_buf.clear();
            self.binaural_direct_buf.resize(input_channel_count, false);
            let num_routed = channel_routing.len();
            {
                let mut states = self.channel_states.lock();
                for c in 0..input_channel_count {
                    // Object-level mute as a 0/1 factor (per-object output gain was
                    // removed; only mute remains live-tunable).
                    let obj_gain = match self.object_params_buf.get(c) {
                        Some(o) if o.muted => 0.0,
                        _ => 1.0,
                    };
                    // Stream metadata gain, same semantics as the VBAP path:
                    // silent (-128 = -inf dB) until the first metadata arrives.
                    let gain_db = states.get(&c).map(|s| s.gain_db).unwrap_or(-128);
                    let gain_linear = if gain_db == -128 {
                        0.0
                    } else {
                        10.0_f32.powf(gain_db as f32 / 20.0)
                    };
                    // Slewed like the VBAP path (block-end value: the binaural
                    // stage updates per block anyway).
                    let ramp_samples = self.sample_rate as f32 * GAIN_SLEW_SECS;
                    if let Some(state) = states.get_mut(&c) {
                        let (start, step) =
                            state.slew_gain(obj_gain * gain_linear, sample_length, ramp_samples);
                        self.binaural_gain_buf[c] = start + step * sample_length as f32;
                    } else {
                        self.binaural_gain_buf[c] = 0.0;
                    }
                    // Same direct/virtual split as the VBAP path.
                    let direct_label = match channel_routing.get(c) {
                        Some(ChannelRoute::Direct(label)) if c < num_routed => Some(*label),
                        _ => None,
                    };
                    if let Some(label) = direct_label {
                        // Direct channel: place it at its resolved speaker's
                        // direction. A channel routed to a non-spatialized
                        // speaker (the LFE) keeps the direct-routing intent in
                        // headphone mode too: fed to both ears equally, no
                        // HRTF (issue #156).
                        if let Some(&spk) = active_label_to_speaker.get(&label) {
                            if let Some(s) = active_layout.speakers.get(spk) {
                                self.binaural_pos_buf[c] = [s.x as f64, s.y as f64, s.z as f64];
                                self.binaural_direct_buf[c] = !s.spatialize;
                            }
                        }
                    } else if let Some(st) = states.get_mut(&c) {
                        // Advance the position ramp for this block (Frame-mode
                        // granularity: the binaural stage updates HRIR/ITD once
                        // per block anyway). Nothing else advances ramps in
                        // binaural mode — the VBAP mix loop that normally does
                        // is bypassed — so without this every object stays at
                        // the ramp default [0,0,0]: dead centre, and rotation-
                        // invariant (the zero vector ignores the head pose).
                        let progress = st.ramp.current_progress().unwrap_or(RampProgress {
                            completed_units: 0,
                            total_units: 0,
                        });
                        ramp_strategy.evaluate(&mut st.ramp, progress, &ramp_context);
                        self.binaural_pos_buf[c] = st.ramp.output_position;
                        st.ramp.commit_output_position();
                        st.ramp.advance_ramp(sample_length as u64);
                    }
                }
            }
            let binaural_params = {
                let g = self.control.live.read();
                // Compare against the live source in place: no per-frame clone
                // (the `Sofa` variant carries a heap path), and any rebuild is
                // pushed to the worker inside `ensure_source`.
                self.binaural.ensure_source(&g.binaural.hrir_source);
                crate::binaural::BinauralFrameParams {
                    head_pose: g.binaural.head_pose,
                    unit_scale_m: g.binaural.unit_scale_m,
                    head_radius_m: g.binaural.head_radius_m,
                    reflections: g.binaural.reflections.clone(),
                    reverb: g.binaural.reverb.clone(),
                    air_absorption: g.binaural.air_absorption,
                }
            };
            let mut output = samples_buf;
            output.clear();
            output.resize(sample_length * 2, 0.0);
            self.binaural.render_frame(
                input_pcm,
                input_channel_count,
                sample_length,
                &binaural_params,
                &self.binaural_pos_buf,
                &self.binaural_gain_buf,
                &self.binaural_direct_buf,
                &mut output,
            );
            // Output gain parity with the speaker path: master gain × dialnorm
            // (auto-gain reductions are already folded into master_gain).
            let loudness = if live.use_loudness {
                f32::from_bits(
                    self.loudness_gain
                        .load(std::sync::atomic::Ordering::Relaxed),
                )
            } else {
                1.0
            };
            let total_gain = live.master_gain * loudness;
            // Ear-channel mute/gain: Studio's headphone L/R rows reuse the
            // first two speaker param slots (the same slots the L/R meters
            // ride), so M/S on them works in binaural mode too.
            let ear = |idx: usize| -> f32 {
                live.speaker_params
                    .get(idx)
                    .map_or(1.0, |p| if p.muted { 0.0 } else { p.gain })
            };
            let gain_l = total_gain * ear(0);
            let gain_r = total_gain * ear(1);
            // Apply the ear gains and track the output peak in the same pass
            // (a whole immersive stream summed onto two channels exceeds full
            // scale easily, so the stereo bus needs the same overload
            // handling as the speaker path).
            let mut peak_sample: f32 = 0.0;
            let mut peak_ear: usize = 0;
            for frame in output.chunks_exact_mut(2) {
                frame[0] *= gain_l;
                frame[1] *= gain_r;
                let a_l = frame[0].abs();
                if a_l > peak_sample {
                    peak_sample = a_l;
                    peak_ear = 0;
                }
                let a_r = frame[1].abs();
                if a_r > peak_sample {
                    peak_sample = a_r;
                    peak_ear = 1;
                }
            }

            // Clipping handling — same policy as the speaker path below:
            // detection always at 0 dBFS so the UI indicators work with
            // auto-gain off; the correction (when enabled) folds into the
            // shared master gain, targeting the configured ceiling. The ear
            // index reuses the first two speaker param slots, the same slots
            // Studio's headphone L/R rows already ride for mute/gain.
            if peak_sample > 1.0 {
                self.control.note_clip(peak_ear);
                if live.auto_gain {
                    let ceiling = 10.0_f32.powf(live.auto_gain_ceiling_db / 20.0);
                    let required_gain = ceiling / peak_sample;
                    // Re-reading under the write lock preserves any
                    // concurrent OSC master change.
                    let new_master_gain = {
                        let mut params = self.control.live.write();
                        params.master_gain *= required_gain;
                        params.master_gain
                    };
                    self.control.mark_dirty();
                    self.control.bump_live_state();
                    self.auto_gain_triggered
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    log::warn!(
                        "Clipping detected on headphone {} (peak={:.3})! Master gain reduced to {:.4} ({:.1} dB), ceiling {:.1} dBFS",
                        if peak_ear == 0 { "L" } else { "R" },
                        peak_sample,
                        new_master_gain,
                        20.0 * new_master_gain.log10(),
                        live.auto_gain_ceiling_db
                    );
                }
            }
            return Ok(RenderedFrame {
                samples: output,
                object_gains: Vec::new(),
                object_band_gains: Vec::new(),
                crossover_time_ms: 0.0,
            });
        }

        // Reuse the donated buffer — resize (no alloc if capacity suffices) and zero it.
        let mut output = samples_buf;
        let required = sample_length * self.num_speakers;
        output.clear();
        output.resize(required, 0.0);

        // Per-object VBAP gains at the final sample — monitoring only (OSC meter
        // bundle). Only collected when `measure_breakdown` is set; left empty (no
        // allocation) on the plain render path (e.g. mpv without Studio open).
        let mut object_gains_out: Vec<(usize, Gains)> = if measure_breakdown {
            Vec::with_capacity(input_channel_count)
        } else {
            Vec::new()
        };
        let mut object_band_gains_out: Vec<(usize, Vec<Gains>)> = Vec::new();
        let mut crossover_elapsed = std::time::Duration::ZERO;
        let profile_crossover = measure_breakdown && self.crossover_filter_bank.is_some();

        // Directly-routed channels always come FIRST in PCM data, then objects.
        let num_routed = channel_routing.len();

        // Check if this is the first render for detailed logging
        let is_first = self
            .first_render
            .swap(false, std::sync::atomic::Ordering::Relaxed);
        if is_first {
            log::info!(
                "VBAP render: {} total PCM channels, {} routed entries, {} trailing object channels",
                input_channel_count,
                num_routed,
                input_channel_count.saturating_sub(num_routed),
            );
            log::info!("  Channel routing: {:?}", channel_routing);
        }

        // Hold channel metadata state lock once for the whole render pass.
        // This avoids lock/unlock churn in the channel loop.
        let mut channel_states = self.channel_states.lock();

        // Process each channel
        for input_channel_idx in 0..input_channel_count {
            // Per-channel mute (applies to beds and objects), as a 0/1 factor.
            let obj_gain = match live.object_params.get(input_channel_idx) {
                Some(o) if o.muted => 0.0,
                _ => 1.0,
            };

            // Get gain from cached metadata (common for ALL channels - beds and objects)
            let state = channel_states.entry(input_channel_idx).or_default();
            let gain_db = state.gain_db;

            // Convert gain from dB to linear
            let gain_linear = if gain_db == -128 {
                0.0 // -inf dB
            } else {
                10.0_f32.powf(gain_db as f32 / 20.0)
            };
            // Slewed per-sample gain factor (includes the mute 0/1 factor):
            // factor(s) = gain_start + gain_step * s.
            let ramp_samples = self.sample_rate as f32 * GAIN_SLEW_SECS;
            let (gain_start, gain_step) =
                state.slew_gain(gain_linear * obj_gain, sample_length, ramp_samples);

            // A channel is directly routed when its routing entry is
            // `Direct` (channels beyond the routing table are trailing object
            // channels; `Virtual` entries render through the object path from
            // their metadata events).
            let direct_label = match channel_routing.get(input_channel_idx) {
                Some(ChannelRoute::Direct(label)) => Some(*label),
                _ => None,
            };
            if let Some(label) = direct_label {
                // DIRECT CHANNEL: one-hot route to the speaker its label
                // resolves to in the active topology.
                let speaker_idx = match active_label_to_speaker.get(&label) {
                    Some(&idx) => idx,
                    None => {
                        // No matching speaker in this layout — skip the channel.
                        if is_first {
                            log::warn!(
                                "  Direct ch{} ({label:?}) has no matching speaker in layout, skipping",
                                input_channel_idx,
                            );
                        }
                        continue;
                    }
                };

                self.bed_routing_gains_buf.fill(0.0);
                self.bed_routing_gains_buf[speaker_idx] = 1.0;

                // Mix bed samples through the same per-speaker gain accumulation model
                // used for objects, but with a one-hot routing table.
                for sample_idx in 0..sample_length {
                    let sample = input_pcm[sample_idx * input_channel_count + input_channel_idx]
                        * (gain_start + gain_step * sample_idx as f32);
                    let out_base = sample_idx * self.num_speakers;
                    for (speaker_idx, &gain) in self.bed_routing_gains_buf.iter().enumerate() {
                        output[out_base + speaker_idx] += sample * gain;
                    }
                }

                let mut gains = Gains::zeroed(self.num_speakers);
                for (speaker_idx, &gain) in self.bed_routing_gains_buf.iter().enumerate() {
                    gains.set(speaker_idx, gain);
                }
                object_gains_out.push((input_channel_idx, gains));

                if is_first {
                    let speaker_name = active_layout.speakers[speaker_idx].name.as_str();
                    log::info!(
                        "  Direct ch{} ({label:?}) → Speaker {} ({}) gain={}dB",
                        input_channel_idx,
                        speaker_idx,
                        speaker_name,
                        gain_db
                    );
                }
            } else {
                let state_mut = channel_states.get_mut(&input_channel_idx);
                let state = match state_mut {
                    // Skip if no metadata available
                    Some(s) => s,
                    None => {
                        if self.log_object_positions {
                            log::warn!(
                                "Channel {} missing cached metadata, skipping",
                                input_channel_idx
                            );
                        }
                        continue;
                    }
                };

                // ── Unified band rendering path ─────────────────────────────────────────
                // Always iterate over `render_bands` (1 band = no crossover, N bands = LR4).
                // Each band returns full-size Gains (zeroed for out-of-band speakers) so the
                // inner mix loop is contiguous and SIMD-friendly.

                // Lazily allocate per-object filter state only when crossover is active.
                let obj_filter_states: Option<&mut Vec<BiquadState>> =
                    if let Some(fb) = self.crossover_filter_bank.as_ref() {
                        let state_count = fb.state_count();
                        if self.crossover_filter_states.len() <= input_channel_idx {
                            self.crossover_filter_states
                                .resize_with(input_channel_idx + 1, || None);
                        }
                        let slot = &mut self.crossover_filter_states[input_channel_idx];
                        if slot.is_none() {
                            *slot = Some(vec![BiquadState::default(); state_count]);
                        }
                        slot.as_mut()
                    } else {
                        None
                    };

                let render_params = ramp_context.render_params();

                // Reuse the per-object band-gain buffer (pooled in the renderer) so
                // the hot render path does not allocate a fresh Vec per object per
                // frame. Each arm fills `band_gains`; it is put back at the end.
                let mut band_gains = std::mem::take(&mut self.band_gains_scratch);
                band_gains.clear();
                match live.ramp_mode {
                    RampMode::Off => {
                        state.ramp.remaining_ramp_units = None;
                        state.ramp.start_position = state.ramp.target_position;
                        state.ramp.current_position = state.ramp.target_position;
                        state.ramp.start_size = state.ramp.target_size;
                        state.ramp.current_size = state.ramp.target_size;
                        state.ramp.output_position = state.ramp.target_position;

                        let position = state.ramp.output_position;
                        let size = state.ramp.current_size;
                        Self::fill_band_gains(
                            &self.unified_table,
                            &self.render_bands,
                            render_params,
                            position,
                            size,
                            &mut band_gains,
                        );

                        let mut fst = obj_filter_states;
                        if profile_crossover {
                            let fb = self.crossover_filter_bank.as_ref().expect("crossover bank");
                            let fst_slice = fst.as_mut().expect("filter states").as_mut_slice();
                            let started_at = std::time::Instant::now();
                            fb.process_block(
                                sample_length,
                                fst_slice,
                                &mut self.crossover_band_scratch,
                                |sample_idx| {
                                    input_pcm[sample_idx * input_channel_count + input_channel_idx]
                                        * (gain_start + gain_step * sample_idx as f32)
                                },
                            );
                            crossover_elapsed += started_at.elapsed();
                            for sample_idx in 0..sample_length {
                                let out_base = sample_idx * self.num_speakers;
                                for (b, gains) in band_gains.iter().enumerate() {
                                    let s = self.crossover_band_scratch[b][sample_idx];
                                    for (spk, &g) in gains.iter().enumerate() {
                                        output[out_base + spk] += s * g;
                                    }
                                }
                            }
                        } else {
                            for sample_idx in 0..sample_length {
                                let raw = input_pcm
                                    [sample_idx * input_channel_count + input_channel_idx]
                                    * (gain_start + gain_step * sample_idx as f32);
                                let split = split_bands(
                                    raw,
                                    &self.crossover_filter_bank,
                                    fst.as_mut().map(|v| v.as_mut_slice()),
                                );
                                let out_base = sample_idx * self.num_speakers;
                                for (b, gains) in band_gains.iter().enumerate() {
                                    let s = split.get(b);
                                    for (spk, &g) in gains.iter().enumerate() {
                                        output[out_base + spk] += s * g;
                                    }
                                }
                            }
                        }
                    }
                    RampMode::Frame => {
                        let progress = state.ramp.current_progress().unwrap_or(RampProgress {
                            completed_units: 0,
                            total_units: 0,
                        });
                        ramp_strategy.evaluate(&mut state.ramp, progress, &ramp_context);
                        let position = state.ramp.output_position;
                        let size = state.ramp.current_size;
                        Self::fill_band_gains(
                            &self.unified_table,
                            &self.render_bands,
                            render_params,
                            position,
                            size,
                            &mut band_gains,
                        );

                        let mut fst = obj_filter_states;
                        if profile_crossover {
                            let fb = self.crossover_filter_bank.as_ref().expect("crossover bank");
                            let fst_slice = fst.as_mut().expect("filter states").as_mut_slice();
                            let started_at = std::time::Instant::now();
                            fb.process_block(
                                sample_length,
                                fst_slice,
                                &mut self.crossover_band_scratch,
                                |sample_idx| {
                                    input_pcm[sample_idx * input_channel_count + input_channel_idx]
                                        * (gain_start + gain_step * sample_idx as f32)
                                },
                            );
                            crossover_elapsed += started_at.elapsed();
                            for sample_idx in 0..sample_length {
                                let out_base = sample_idx * self.num_speakers;
                                for (b, gains) in band_gains.iter().enumerate() {
                                    let s = self.crossover_band_scratch[b][sample_idx];
                                    for (spk, &g) in gains.iter().enumerate() {
                                        output[out_base + spk] += s * g;
                                    }
                                }
                            }
                        } else {
                            for sample_idx in 0..sample_length {
                                let raw = input_pcm
                                    [sample_idx * input_channel_count + input_channel_idx]
                                    * (gain_start + gain_step * sample_idx as f32);
                                let split = split_bands(
                                    raw,
                                    &self.crossover_filter_bank,
                                    fst.as_mut().map(|v| v.as_mut_slice()),
                                );
                                let out_base = sample_idx * self.num_speakers;
                                for (b, gains) in band_gains.iter().enumerate() {
                                    let s = split.get(b);
                                    for (spk, &g) in gains.iter().enumerate() {
                                        output[out_base + spk] += s * g;
                                    }
                                }
                            }
                        }
                        state.ramp.commit_output_position();
                        state.ramp.advance_ramp(sample_length as u64);
                    }
                    RampMode::Sample => {
                        let mut fst = obj_filter_states;
                        // One Gains slot per band, reused each sample (and across
                        // objects/frames via the pooled buffer).
                        band_gains
                            .resize(self.render_bands.len(), Gains::zeroed(self.num_speakers));
                        if profile_crossover {
                            let fb = self.crossover_filter_bank.as_ref().expect("crossover bank");
                            let fst_slice = fst.as_mut().expect("filter states").as_mut_slice();
                            let started_at = std::time::Instant::now();
                            fb.process_block(
                                sample_length,
                                fst_slice,
                                &mut self.crossover_band_scratch,
                                |sample_idx| {
                                    input_pcm[sample_idx * input_channel_count + input_channel_idx]
                                        * (gain_start + gain_step * sample_idx as f32)
                                },
                            );
                            crossover_elapsed += started_at.elapsed();
                            // See the non-crossover branch: only recompute the VBAP
                            // gains when the position/size changes (skips redundant
                            // per-sample work while the object is static).
                            let mut last_pos = [f64::NAN; 3];
                            let mut last_size = [f32::NAN; 3];
                            for sample_idx in 0..sample_length {
                                let progress =
                                    state.ramp.current_progress().unwrap_or(RampProgress {
                                        completed_units: 0,
                                        total_units: 0,
                                    });
                                ramp_strategy.evaluate(&mut state.ramp, progress, &ramp_context);
                                let position = state.ramp.output_position;
                                let size = state.ramp.current_size;
                                if position != last_pos || size != last_size {
                                    Self::fill_band_gains(
                                        &self.unified_table,
                                        &self.render_bands,
                                        render_params,
                                        position,
                                        size,
                                        &mut band_gains,
                                    );
                                    last_pos = position;
                                    last_size = size;
                                }
                                let out_base = sample_idx * self.num_speakers;
                                for (b, gains) in band_gains.iter().enumerate() {
                                    let s = self.crossover_band_scratch[b][sample_idx];
                                    for (spk, &g) in gains.iter().enumerate() {
                                        output[out_base + spk] += s * g;
                                    }
                                }
                                state.ramp.commit_output_position();
                                state.ramp.advance_ramp(1);
                            }
                        } else {
                            // Recompute the per-band VBAP gains only when the
                            // interpolated position/size actually changes. While the
                            // object is not ramping (the common case — metadata is
                            // sparse) `output_position` is constant across the block,
                            // so this collapses 1 `compute_gains` call per band per
                            // sample down to one per block while staying bit-identical.
                            let mut last_pos = [f64::NAN; 3];
                            let mut last_size = [f32::NAN; 3];
                            for sample_idx in 0..sample_length {
                                let progress =
                                    state.ramp.current_progress().unwrap_or(RampProgress {
                                        completed_units: 0,
                                        total_units: 0,
                                    });
                                ramp_strategy.evaluate(&mut state.ramp, progress, &ramp_context);
                                let position = state.ramp.output_position;
                                let size = state.ramp.current_size;
                                if position != last_pos || size != last_size {
                                    Self::fill_band_gains(
                                        &self.unified_table,
                                        &self.render_bands,
                                        render_params,
                                        position,
                                        size,
                                        &mut band_gains,
                                    );
                                    last_pos = position;
                                    last_size = size;
                                }
                                let raw = input_pcm
                                    [sample_idx * input_channel_count + input_channel_idx]
                                    * (gain_start + gain_step * sample_idx as f32);
                                let split = split_bands(
                                    raw,
                                    &self.crossover_filter_bank,
                                    fst.as_mut().map(|v| v.as_mut_slice()),
                                );
                                let out_base = sample_idx * self.num_speakers;
                                for (b, gains) in band_gains.iter().enumerate() {
                                    let s = split.get(b);
                                    for (spk, &g) in gains.iter().enumerate() {
                                        output[out_base + spk] += s * g;
                                    }
                                }
                                state.ramp.commit_output_position();
                                state.ramp.advance_ramp(1);
                            }
                        }
                    }
                    RampMode::Interp => {
                        // Destination gains for this block: one VBAP evaluation per
                        // band at the target position. The object's audible path is
                        // then a per-sample linear interpolation from the previous
                        // block's end gains to these — no per-sample VBAP.
                        state.ramp.remaining_ramp_units = None;
                        state.ramp.current_position = state.ramp.target_position;
                        state.ramp.current_size = state.ramp.target_size;
                        state.ramp.output_position = state.ramp.target_position;
                        let position = state.ramp.target_position;
                        let size = state.ramp.target_size;

                        let mut end = std::mem::take(&mut self.interp_end_scratch);
                        Self::fill_band_gains(
                            &self.unified_table,
                            &self.render_bands,
                            render_params,
                            position,
                            size,
                            &mut end,
                        );
                        self.interp_end_scratch = end;
                        let n_bands = self.interp_end_scratch.len();

                        // First block for this channel → start == end (no jump in).
                        if state.interp_prev_gains.len() != n_bands {
                            state.interp_prev_gains.clear();
                            state
                                .interp_prev_gains
                                .extend_from_slice(&self.interp_end_scratch);
                        }
                        band_gains.resize(n_bands, Gains::zeroed(self.num_speakers));

                        let mut fst = obj_filter_states;
                        let inv_n = 1.0 / sample_length.max(1) as f32;
                        if profile_crossover {
                            let fb = self.crossover_filter_bank.as_ref().expect("crossover bank");
                            let fst_slice = fst.as_mut().expect("filter states").as_mut_slice();
                            let started_at = std::time::Instant::now();
                            fb.process_block(
                                sample_length,
                                fst_slice,
                                &mut self.crossover_band_scratch,
                                |sample_idx| {
                                    input_pcm[sample_idx * input_channel_count + input_channel_idx]
                                        * (gain_start + gain_step * sample_idx as f32)
                                },
                            );
                            crossover_elapsed += started_at.elapsed();
                            for sample_idx in 0..sample_length {
                                let f = (sample_idx as f32 + 1.0) * inv_n;
                                for b in 0..n_bands {
                                    let (s0, s1) =
                                        (&state.interp_prev_gains[b], &self.interp_end_scratch[b]);
                                    let slot = &mut band_gains[b];
                                    for spk in 0..self.num_speakers {
                                        slot[spk] = s0[spk] * (1.0 - f) + s1[spk] * f;
                                    }
                                }
                                let out_base = sample_idx * self.num_speakers;
                                for (b, gains) in band_gains.iter().enumerate() {
                                    let s = self.crossover_band_scratch[b][sample_idx];
                                    for (spk, &g) in gains.iter().enumerate() {
                                        output[out_base + spk] += s * g;
                                    }
                                }
                            }
                        } else {
                            for sample_idx in 0..sample_length {
                                let f = (sample_idx as f32 + 1.0) * inv_n;
                                for b in 0..n_bands {
                                    let (s0, s1) =
                                        (&state.interp_prev_gains[b], &self.interp_end_scratch[b]);
                                    let slot = &mut band_gains[b];
                                    for spk in 0..self.num_speakers {
                                        slot[spk] = s0[spk] * (1.0 - f) + s1[spk] * f;
                                    }
                                }
                                let raw = input_pcm
                                    [sample_idx * input_channel_count + input_channel_idx]
                                    * (gain_start + gain_step * sample_idx as f32);
                                let split = split_bands(
                                    raw,
                                    &self.crossover_filter_bank,
                                    fst.as_mut().map(|v| v.as_mut_slice()),
                                );
                                let out_base = sample_idx * self.num_speakers;
                                for (b, gains) in band_gains.iter().enumerate() {
                                    let s = split.get(b);
                                    for (spk, &g) in gains.iter().enumerate() {
                                        output[out_base + spk] += s * g;
                                    }
                                }
                            }
                        }

                        // Cache this block's destination as the next block's start.
                        state.interp_prev_gains.clear();
                        state
                            .interp_prev_gains
                            .extend_from_slice(&self.interp_end_scratch);
                    }
                };

                // Monitoring outputs (OSC meter bundle): only built when requested.
                // `band_gains` is already full-size — sum across bands for the
                // per-object gains, and hand a copy of the band gains out.
                if measure_breakdown {
                    let mut summed = Gains::zeroed(self.num_speakers);
                    for gains in &band_gains {
                        for (i, &g) in gains.iter().enumerate() {
                            summed[i] += g;
                        }
                    }
                    object_band_gains_out.push((input_channel_idx, band_gains.clone()));
                    object_gains_out.push((input_channel_idx, summed));
                }

                // Return the pooled buffer for the next object/frame.
                self.band_gains_scratch = band_gains;
            }
        }
        drop(channel_states);

        // topology_guard is an ArcSwap Guard (no lock held); drop it here to make the
        // intent explicit before the gain/auto-gain section.
        drop(topology_guard);

        // Increment frame counter
        let _frame_num = self
            .frame_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // Dialog norm: only apply if the live flag is set.
        let loudness = if live.use_loudness {
            f32::from_bits(
                self.loudness_gain
                    .load(std::sync::atomic::Ordering::Relaxed),
            )
        } else {
            1.0
        };

        // Auto-gain reduction is folded directly into `master_gain` (see the
        // clipping branch below), so it needs no separate factor here.
        let total_gain = live.master_gain * loudness;

        // Pre-compute per-speaker total gains and update delay-line targets in a
        // single pass over the speaker list — one HashMap lookup per speaker.
        // Mute overrides gain to 0.0 without touching the stored gain value.
        self.speaker_gains_buf
            .iter_mut()
            .enumerate()
            .for_each(|(idx, g)| {
                let sp = live.speaker_params.get(idx);
                *g = if sp.is_some_and(|s| s.muted) {
                    0.0
                } else {
                    total_gain * sp.map_or(1.0, |s| s.gain)
                };
            });
        for (idx, dl) in self.delay_lines.iter_mut().enumerate() {
            dl.set_target_ms(
                live.speaker_params.get(idx).map_or(0.0, |s| s.delay_ms),
                self.sample_rate,
            );
        }
        let speaker_total_gains = &self.speaker_gains_buf;

        // Apply per-speaker gains and delay lines, and detect peak (tracking which
        // speaker channel held the peak, for clip reporting).
        let mut peak_sample: f32 = 0.0;
        let mut peak_speaker_idx: usize = 0;
        for sample_idx in 0..sample_length {
            for speaker_idx in 0..self.num_speakers {
                let s = &mut output[sample_idx * self.num_speakers + speaker_idx];
                *s *= speaker_total_gains[speaker_idx];
                *s = self.delay_lines[speaker_idx].process(*s);
                let a = s.abs();
                if a > peak_sample {
                    peak_sample = a;
                    peak_speaker_idx = speaker_idx;
                }
            }
        }

        // Clipping handling. Detection is always at 0 dBFS (peak > 1.0) and the
        // clip flag is raised (with the offending speaker) regardless of auto-gain
        // so the UI clip indicators work even when auto-gain is disabled.
        if peak_sample > 1.0 {
            self.control.note_clip(peak_speaker_idx);

            // Auto-gain: fold the required attenuation directly into the live
            // master gain (peak-hold, no recovery) so the reduction is visible on
            // the master control and persisted with it. Detection stays at 0 dBFS
            // but the correction targets the configured ceiling (default −1 dBFS),
            // leaving headroom so it fires less often. The write lock is taken only
            // on clipping frames (transient), never in steady state.
            //
            // The log + name resolution live here (not in the always-run flag path)
            // so a sustained clip with auto-gain *off* only flips the atomic flag for
            // the UI indicators — it does not spam the log or load the topology each
            // frame. With auto-gain on, the correction makes clips transient anyway.
            if live.auto_gain {
                let speaker_name = self
                    .control
                    .active_topology()
                    .speaker_layout
                    .speakers
                    .get(peak_speaker_idx)
                    .map(|s| s.name.clone())
                    .unwrap_or_else(|| format!("#{peak_speaker_idx}"));
                let ceiling = 10.0_f32.powf(live.auto_gain_ceiling_db / 20.0);
                // Bring this peak down to the ceiling rather than exactly 0 dBFS.
                let required_gain = ceiling / peak_sample;
                // Apply it to the shared master gain. Re-reading under the write
                // lock preserves any concurrent OSC master change.
                let new_master_gain = {
                    let mut params = self.control.live.write();
                    params.master_gain *= required_gain;
                    params.master_gain
                };
                self.control.mark_dirty();
                self.control.bump_live_state();
                self.auto_gain_triggered
                    .store(true, std::sync::atomic::Ordering::Relaxed);

                log::warn!(
                    "Clipping detected on speaker '{}' (peak={:.3})! Master gain reduced to {:.4} ({:.1} dB), ceiling {:.1} dBFS",
                    speaker_name,
                    peak_sample,
                    new_master_gain,
                    20.0 * new_master_gain.log10(),
                    live.auto_gain_ceiling_db
                );
            }
        }

        object_gains_out.sort_by_key(|(idx, _)| *idx);
        object_band_gains_out.sort_by_key(|(idx, _)| *idx);
        Ok(RenderedFrame {
            samples: output,
            object_gains: object_gains_out,
            object_band_gains: object_band_gains_out,
            crossover_time_ms: crossover_elapsed.as_secs_f32() * 1000.0,
        })
    }

    /// Get the number of output speakers
    pub fn num_speakers(&self) -> usize {
        self.num_speakers
    }

    /// Number of channels the renderer actually emits this frame: 2 in binaural
    /// (headphone) mode, otherwise the speaker count. Hosts must size their sink
    /// and `RenderedAudio` from this, not from [`num_speakers`](Self::num_speakers).
    pub fn output_channel_count(&self) -> usize {
        match self.control.live.read().binaural.output_mode {
            crate::live_params::OutputMode::Binaural => 2,
            crate::live_params::OutputMode::SpeakerArray => self.num_speakers,
        }
    }

    pub fn speaker_layout(&self) -> crate::speaker_layout::SpeakerLayout {
        self.control.active_layout()
    }

    /// Get speaker names
    pub fn speaker_names(&self) -> Vec<String> {
        self.control
            .topology
            .load()
            .speaker_layout
            .speaker_names()
            .into_iter()
            .map(|s| s.to_string())
            .collect()
    }

    /// Get spread resolution
    pub fn spread_resolution(&self) -> f32 {
        self.spread_resolution
    }
}

#[cfg(test)]
mod tests;
