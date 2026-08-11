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

use crate::live_params::{RampMode, RendererControl};
use crate::ramp_strategy::{
    PositionRampStrategy, RampContext, RampProgress, RampRenderParams, RampStrategy, RampTarget,
};

use crate::spatial_vbap::DistanceModel;
use anyhow::Result;
use std::sync::Arc;

mod cascade;
mod components;
mod construction;
mod speaker_stage;
use components::{ChannelState, evaluation_build_config};
pub use components::{RenderedFrame, SpatialChannelEvent};
use speaker_stage::SpeakerRenderStage;

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
    diffuse_mirror_axes: crate::spatial_vbap::MirrorAxes,
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
    /// Per-channel state, indexed by channel. A plain `Vec` owned by `&mut
    /// self`: `render_frame` takes `&mut self`, so the audio path needs no
    /// lock and no hashing. Grown only when the channel count rises, never
    /// per block.
    channel_states: Vec<ChannelState>,
    /// Set by [`Self::reset_runtime_state`] from other threads and consumed by
    /// `render_frame`. An atomic flag replaces the mutex that used to guard
    /// `channel_states`: the reset is the only cross-thread access, and making
    /// it a flag keeps the render path lock-free.
    reset_requested: std::sync::atomic::AtomicBool,

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

    /// Per-layout speaker rendering state (band engines, crossover, delay
    /// lines, per-layout scratch). Extracted so the cascaded binaural mode can
    /// later run a second stage against a virtual layout.
    speaker_stage: SpeakerRenderStage,

    /// Scratch snapshot of live per-object params, indexed by input channel.
    object_params_buf: Vec<crate::live_params::ObjectLiveParams>,

    /// Scratch snapshot of live per-speaker params, indexed by output speaker.
    speaker_params_buf: Vec<crate::live_params::SpeakerLiveParams>,

    /// Last integrated generation for per-object live params.
    object_params_generation_seen: u64,

    /// Last integrated generation for per-speaker live params.
    speaker_params_generation_seen: u64,

    /// Optional contributor-provided ramp strategy override.
    ramp_strategy_override: Option<Arc<dyn RampStrategy>>,

    /// Independent binaural (headphone) output stage. Used only when
    /// `LiveParams::binaural.output_mode == OutputMode::Binaural`; otherwise the
    /// classic VBAP path runs and this holds no live state.
    binaural: crate::binaural::BinauralRenderer,

    /// Cascaded binaural geometry (`binaural.mode == Cascaded`): binaural
    /// input positions/flags derived from the app layout + the virtual bus
    /// scratch. Derived lazily the first frame the mode is active, re-derived
    /// when the topology identity changes. `None` while unused.
    cascade: Option<cascade::CascadeStage>,

    /// Speaker width of the stage that ran the previous frame's mix pass.
    /// `RampMode::Interp` caches layout-sized gains in the shared
    /// `ChannelState`s; a width change (speaker↔cascade switch, cascade
    /// layout change, main relayout) must clear them or stale entries would
    /// index out of the new width. 0 until the first mix pass.
    last_mix_num_speakers: usize,

    /// Scratch per-channel world positions for the binaural path (reused).
    binaural_pos_buf: Vec<[f64; 3]>,

    /// Scratch per-channel gains for the binaural path (reused).
    binaural_gain_buf: Vec<f32>,

    /// Scratch per-channel "direct" flags for the binaural path (reused):
    /// beds mapped to a `spatialize: false` speaker (the LFE) feed both ears
    /// equally instead of being HRTF-spatialized.
    binaural_direct_buf: Vec<bool>,
}

impl SpatialRenderer {
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
            diffuse_mirror_axes: live.diffuse_mirror_axes,
            distance_model: self.distance_model,
        })
    }

    /// Clear cached per-channel spatial/ramp state after a decoder reset or
    /// stream restart so stale object positions cannot leak into subsequent
    /// rendering.
    /// Borrow a channel's state, growing the backing `Vec` if the stream just
    /// widened. Growth happens only when the channel count rises — never per
    /// block — so the render path stays allocation-free in steady state.
    fn state_mut(states: &mut Vec<ChannelState>, channel_idx: usize) -> &mut ChannelState {
        if channel_idx >= states.len() {
            states.resize_with(channel_idx + 1, ChannelState::default);
        }
        &mut states[channel_idx]
    }

    pub fn reset_runtime_state(&self) {
        self.reset_requested
            .store(true, std::sync::atomic::Ordering::Release);
        self.first_render
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Update channel states from format-agnostic spatial events.
    ///
    /// Called internally from `render_frame` when pending events are present.
    /// The `channel_idx` and `is_bed` fields of each event must already be
    /// resolved by the caller (see `SpatialChannelEvent`).
    /// Takes `&mut Vec<ChannelState>` rather than `&mut self` so the caller can
    /// split the borrow: `render_frame` holds an immutable snapshot of other
    /// fields while this mutates channel state.
    fn update_metadata(
        states: &mut Vec<ChannelState>,
        log_object_positions: bool,
        sample_rate: u32,
        events: &[SpatialChannelEvent],
        strategy: &dyn RampStrategy,
        ctx: &RampContext,
    ) -> Result<()> {
        for event in events {
            let state = Self::state_mut(states, event.channel_idx);
            state.initialized = true;

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
                    if log_object_positions {
                        let remaining_units = state.ramp.remaining_ramp_units.unwrap_or(0);
                        let sample_pos = event.sample_pos.unwrap_or(0);
                        if state.ramp.target_position != target_position {
                            log::info!(
                                "  Obj ch{:2}: sample_pos {} remaining {} - Starting ramp over {} samples (~{}ms)",
                                event.channel_idx,
                                sample_pos,
                                remaining_units,
                                state.ramp.ramp_length,
                                state.ramp.ramp_length as f32 / sample_rate as f32 * 1000.0
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

        // Consume a reset requested from another thread (decoder reset, stream
        // restart). Clearing here rather than under a lock in
        // `reset_runtime_state` is what keeps the render path lock-free; the
        // capacity is retained so the regrowth costs no allocation.
        if self
            .reset_requested
            .swap(false, std::sync::atomic::Ordering::Acquire)
        {
            self.channel_states.clear();
        }

        // ── 0. Independent binaural (headphone) path ─────────────────────────
        // When headphone output is selected, bypass the entire VBAP / crossover /
        // speaker chain and emit a 2-channel frame. The branch is taken below,
        // after `update_metadata` has applied the pending events (new ramp
        // targets); the branch itself advances each object's position ramp for
        // the block. Flag it here.
        let (binaural_active, cascade_active) = {
            let g = self.control.live.read();
            (
                matches!(
                    g.binaural.output_mode,
                    crate::live_params::OutputMode::Binaural
                ),
                matches!(g.binaural.mode, crate::live_params::BinauralMode::Cascaded),
            )
        };

        // ── 1. Load the current immutable render topology and keep band engines in sync ──
        let topology_guard = self.control.active_topology();
        let topology = &*topology_guard;
        let topology_identity = std::sync::Arc::as_ptr(&topology_guard) as usize;
        self.speaker_stage.refresh_for_topology(
            &self.control,
            topology_identity,
            &topology.speaker_layout,
        )?;
        // Cascaded binaural geometry: derived from the active topology, kept
        // in sync only while the mode is active. Must run before the live
        // snapshot below, which borrows `self` fields for the rest of the frame.
        if binaural_active && cascade_active {
            self.refresh_cascade_for_topology(topology, topology_identity);
        }

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
                diffuse_mirror_axes: g.distance_diffuse_mirror_axes,
            }
        };
        self.speaker_stage
            .sync_position_interpolation(live_position_interpolation);

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
            Self::update_metadata(
                &mut self.channel_states,
                self.log_object_positions,
                self.sample_rate,
                pending_events,
                ramp_strategy,
                &ramp_context,
            )?;
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
            let (binaural_params, ears) = {
                let g = self.control.live.read();
                // Compare against the live source in place: no per-frame clone
                // (the `Sofa` variant carries a heap path), and any rebuild is
                // pushed to the worker inside `ensure_source`.
                self.binaural.ensure_source(&g.binaural.hrir_source);
                (
                    crate::binaural::BinauralFrameParams {
                        head_pose: g.binaural.head_pose,
                        unit_scale_m: g.binaural.unit_scale_m,
                        head_radius_m: g.binaural.head_radius_m,
                        reflections: g.binaural.reflections.clone(),
                        reverb: g.binaural.reverb.clone(),
                        air_absorption: g.binaural.air_absorption,
                    },
                    g.binaural.ears,
                )
            };
            let mut output = samples_buf;
            output.clear();
            output.resize(sample_length * 2, 0.0);
            let mut cascade_diag = None;
            if cascade_active && self.cascade.is_some() {
                // Cascaded mode: the MAIN speaker stage renders the app layout
                // as a virtual room, then the fixed virtual speakers are
                // binauralised. Taken/put back so the free function can borrow
                // the other renderer fields it needs.
                let mut geometry = self.cascade.take().expect("checked is_some above");
                cascade::reseed_interp_on_width_change(
                    &mut self.channel_states,
                    &mut self.last_mix_num_speakers,
                    self.speaker_stage.num_speakers,
                );
                let is_first = self
                    .first_render
                    .swap(false, std::sync::atomic::Ordering::Relaxed);
                let diag = cascade::render_cascade_frame(
                    &mut geometry,
                    &mut self.speaker_stage,
                    &mut self.channel_states,
                    &mut self.binaural,
                    speaker_stage::SpeakerStageFrame {
                        input_pcm,
                        input_channel_count,
                        sample_length,
                        channel_routing: &channel_routing,
                        label_to_speaker: active_label_to_speaker,
                        layout: active_layout,
                        object_params: live.object_params,
                        ramp_mode: live.ramp_mode,
                        ramp_strategy,
                        ramp_context: &ramp_context,
                        log_object_positions: self.log_object_positions,
                        is_first,
                        measure_breakdown,
                    },
                    live.speaker_params,
                    &binaural_params,
                    &mut output,
                );
                cascade_diag = Some(diag);
                self.cascade = Some(geometry);
            } else {
                self.binaural_pos_buf.clear();
                self.binaural_pos_buf
                    .resize(input_channel_count, [0.0, 1.0, 0.0]);
                self.binaural_gain_buf.clear();
                self.binaural_gain_buf.resize(input_channel_count, 0.0);
                self.binaural_direct_buf.clear();
                self.binaural_direct_buf.resize(input_channel_count, false);
                let num_routed = channel_routing.len();
                {
                    let states = &mut self.channel_states;
                    for c in 0..input_channel_count {
                        // Object-level mute as a 0/1 factor (per-object output gain was
                        // removed; only mute remains live-tunable).
                        let obj_gain = match self.object_params_buf.get(c) {
                            Some(o) if o.muted => 0.0,
                            _ => 1.0,
                        };
                        // Stream metadata gain, same semantics as the VBAP path:
                        // silent (-128 = -inf dB) until the first metadata arrives.
                        let gain_db = states
                            .get(c)
                            .filter(|s| s.initialized)
                            .map(|s| s.gain_db)
                            .unwrap_or(-128);
                        let gain_linear = if gain_db == -128 {
                            0.0
                        } else {
                            10.0_f32.powf(gain_db as f32 / 20.0)
                        };
                        // Slewed like the VBAP path (block-end value: the binaural
                        // stage updates per block anyway).
                        let ramp_samples = self.sample_rate as f32 * GAIN_SLEW_SECS;
                        if let Some(state) = states.get_mut(c) {
                            let (start, step) = state.slew_gain(
                                obj_gain * gain_linear,
                                sample_length,
                                ramp_samples,
                            );
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
                        } else if let Some(st) = states.get_mut(c) {
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
            }
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
            // Ear-channel mute/gain: dedicated live params (the ears used to
            // ride the first two per-speaker slots, which now belong to the
            // virtual FL/FR rows in cascaded mode).
            let ear = |idx: usize| -> f32 {
                let e = ears[idx.min(1)];
                if e.muted { 0.0 } else { e.gain }
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
            // shared master gain, targeting the configured ceiling.
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
            // Cascaded mode returns the virtual mix diagnostics: they index
            // the app layout, so the object meters stay valid on headphones.
            return Ok(match cascade_diag {
                Some(mut diag) => {
                    diag.object_gains.sort_by_key(|(idx, _)| *idx);
                    diag.object_band_gains.sort_by_key(|(idx, _)| *idx);
                    diag.object_band_sq.sort_by_key(|(idx, _)| *idx);
                    RenderedFrame {
                        samples: output,
                        object_gains: diag.object_gains,
                        object_band_gains: diag.object_band_gains,
                        object_band_sq: diag.object_band_sq,
                        crossover_time_ms: diag.crossover_elapsed.as_secs_f32() * 1000.0,
                    }
                }
                None => RenderedFrame {
                    samples: output,
                    object_gains: Vec::new(),
                    object_band_gains: Vec::new(),
                    object_band_sq: Vec::new(),
                    crossover_time_ms: 0.0,
                },
            });
        }

        // Reuse the donated buffer — resize (no alloc if capacity suffices) and zero it.
        let mut output = samples_buf;
        let required = sample_length * self.num_speakers;
        output.clear();
        output.resize(required, 0.0);

        // Check if this is the first render for detailed logging
        let is_first = self
            .first_render
            .swap(false, std::sync::atomic::Ordering::Relaxed);
        cascade::reseed_interp_on_width_change(
            &mut self.channel_states,
            &mut self.last_mix_num_speakers,
            self.speaker_stage.num_speakers,
        );
        let frame = speaker_stage::SpeakerStageFrame {
            input_pcm,
            input_channel_count,
            sample_length,
            channel_routing: &channel_routing,
            label_to_speaker: active_label_to_speaker,
            layout: active_layout,
            object_params: live.object_params,
            ramp_mode: live.ramp_mode,
            ramp_strategy,
            ramp_context: &ramp_context,
            log_object_positions: self.log_object_positions,
            is_first,
            measure_breakdown,
        };
        let mut diag =
            self.speaker_stage
                .mix_channels(frame, &mut self.channel_states, &mut output);

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

        let (peak_sample, peak_speaker_idx) =
            self.speaker_stage
                .finalize_output(live.speaker_params, total_gain, &mut output);

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

        diag.object_gains.sort_by_key(|(idx, _)| *idx);
        diag.object_band_gains.sort_by_key(|(idx, _)| *idx);
        diag.object_band_sq.sort_by_key(|(idx, _)| *idx);
        Ok(RenderedFrame {
            samples: output,
            object_gains: diag.object_gains,
            object_band_gains: diag.object_band_gains,
            object_band_sq: diag.object_band_sq,
            crossover_time_ms: diag.crossover_elapsed.as_secs_f32() * 1000.0,
        })
    }

    /// Get the number of output speakers
    /// Whether the binaural (headphone) output path is active — hosts use this
    /// to route their metering (ears vs speakers) without guessing from the
    /// channel count (a 2.0 speaker layout is also 2-channel).
    pub fn output_is_binaural(&self) -> bool {
        matches!(
            self.control.live.read().binaural.output_mode,
            crate::live_params::OutputMode::Binaural
        )
    }

    /// The virtual-speaker bus of the last cascaded frame, when the cascaded
    /// binaural mode rendered it: `(interleaved_samples, channel_count)` in
    /// app-layout speaker order, post per-speaker params. The host meters
    /// this so Studio's speaker gauges show the virtual room while the
    /// stereo output feeds the ear meters. `None` outside cascaded mode.
    pub fn virtual_bus(&self) -> Option<(&[f32], usize)> {
        let active = {
            let g = self.control.live.read();
            matches!(
                g.binaural.output_mode,
                crate::live_params::OutputMode::Binaural
            ) && matches!(g.binaural.mode, crate::live_params::BinauralMode::Cascaded)
        };
        if !active {
            return None;
        }
        self.cascade
            .as_ref()
            .filter(|c| !c.bus.is_empty())
            .map(|c| (c.bus.as_slice(), c.num_buses()))
    }

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

#[cfg(test)]
mod golden_tests;

#[cfg(all(test, feature = "perf-gate"))]
mod perf_gate;
