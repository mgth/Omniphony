//! Cascaded binaural mode (issue #220): the app's speaker pipeline renders
//! onto **the app's own speaker layout** treated as a virtual room, then each
//! virtual speaker is binauralised as a static source by the existing
//! [`crate::binaural`] renderer.
//!
//! Since #223 the mode ran a *second* [`super::SpeakerRenderStage`] on a
//! dedicated `cascade_layout`; that separate layout is gone. The virtual room
//! IS the configured speaker layout, and the mode reuses the **main** stage —
//! idle in binaural mode anyway — so:
//!
//! - every rendering option applies (room mapping, distance, spread,
//!   crossover, ramp modes, backend), exactly as on the physical room;
//! - the per-speaker live params (gain / mute / delay rows in Studio) drive
//!   the virtual speakers one-for-one;
//! - delay-line and crossover state carry over seamlessly across
//!   speaker↔headphone switches (same stage, same width);
//! - the per-object monitoring outputs index the app layout, so the object
//!   meters stay valid in cascade mode.
//!
//! The cost model holds: the convolution count is bound by the layout size
//! whatever the stream carries, and with a static head the per-source HRIR
//! update no-ops ([`crate::binaural::EarConvolver::set_coeffs_smooth`]
//! compares kernels).
//!
//! Headphone L/R gain/mute deliberately do NOT ride the first two per-speaker
//! slots anymore (they would collide with the virtual FL/FR rows): the ears
//! have dedicated live params ([`crate::live_params::EarLiveParams`]).

use crate::live_params::RenderTopology;

use super::speaker_stage::{SpeakerRenderStage, SpeakerStageDiagnostics, SpeakerStageFrame};
use super::{ChannelState, SpatialRenderer};

/// Binaural input geometry + bus scratch for the cascaded mode, derived from
/// the active topology's speaker layout. Cheap to rebuild (no engines — the
/// mode reuses the main [`SpeakerRenderStage`]).
pub(super) struct CascadeStage {
    /// Main topology identity this geometry was derived from.
    pub(super) topology_identity: usize,
    /// One entry per speaker of the app layout. Non-spatialized entries (the
    /// LFE) are flagged direct: both ears, −3 dB, no HRTF — the binaural
    /// stage's standing policy (issue #156).
    pub(super) bin_pos: Vec<[f64; 3]>,
    pub(super) bin_gain: Vec<f32>,
    pub(super) bin_direct: Vec<bool>,
    /// Interleaved virtual-speaker bus the binaural stage consumes as its PCM
    /// input. Reused every frame; exposed to the host for virtual metering
    /// (see `SpatialRenderer::virtual_bus`).
    pub(super) bus: Vec<f32>,
}

impl CascadeStage {
    /// Total virtual buses (= the app layout's speaker count).
    #[inline]
    pub(super) fn num_buses(&self) -> usize {
        self.bin_pos.len()
    }

    pub(super) fn from_topology(topology: &RenderTopology, topology_identity: usize) -> Self {
        let speakers = &topology.speaker_layout.speakers;
        Self {
            topology_identity,
            bin_pos: speakers
                .iter()
                .map(|s| [s.x as f64, s.y as f64, s.z as f64])
                .collect(),
            // Per-channel level shaping happens in the virtual mix (including
            // the per-speaker rows); the binaural stage sees unity buses.
            bin_gain: vec![1.0f32; speakers.len()],
            bin_direct: speakers.iter().map(|s| !s.spatialize).collect(),
            bus: Vec::new(),
        }
    }
}

impl SpatialRenderer {
    /// Keep the cascade geometry in sync with the active topology. Called from
    /// `render_frame` *before* the live snapshot is taken. Infallible and
    /// cheap — just position/flag vectors off the layout.
    pub(super) fn refresh_cascade_for_topology(
        &mut self,
        topology: &RenderTopology,
        topology_identity: usize,
    ) {
        let up_to_date = self
            .cascade
            .as_ref()
            .is_some_and(|c| c.topology_identity == topology_identity);
        if !up_to_date {
            self.cascade = Some(CascadeStage::from_topology(topology, topology_identity));
        }
    }
}

/// `RampMode::Interp` caches per-band gains sized to the *mixing* layout in
/// the shared `ChannelState`s. Since the cascade reuses the main stage the
/// width only changes on a relayout — clear the caches then so the next block
/// re-seeds, exactly like a channel's first block. Free function: it runs
/// after the `LiveSnapshot` is taken, so no `&mut self` method can exist there.
pub(super) fn reseed_interp_on_width_change(
    channel_states: &mut [ChannelState],
    last_mix_num_speakers: &mut usize,
    mix_width: usize,
) {
    if *last_mix_num_speakers == mix_width {
        return;
    }
    for state in channel_states {
        state.interp_prev_gains.clear();
    }
    *last_mix_num_speakers = mix_width;
}

/// Run the virtual pipeline for one frame: the MAIN speaker stage mixes onto
/// the cascade buses, the output stage applies the per-speaker live params
/// (the same rows that drive physical speakers), then the binaural stage
/// renders the fixed virtual sources into `output` (interleaved stereo).
/// Returns the mix diagnostics — valid for the app layout, so object meters
/// keep working in cascade mode.
///
/// Free function over the exact fields it needs (not a `&mut self` method):
/// `render_frame` holds a `LiveSnapshot` borrowing other `SpatialRenderer`
/// fields for the whole frame.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_cascade_frame(
    cascade: &mut CascadeStage,
    stage: &mut SpeakerRenderStage,
    channel_states: &mut Vec<ChannelState>,
    binaural: &mut crate::binaural::BinauralRenderer,
    frame: SpeakerStageFrame<'_>,
    speaker_params: &[crate::live_params::SpeakerLiveParams],
    binaural_params: &crate::binaural::BinauralFrameParams,
    output: &mut [f32],
) -> SpeakerStageDiagnostics {
    let total = cascade.num_buses();
    debug_assert_eq!(total, stage.num_speakers);
    let sample_length = frame.sample_length;
    cascade.bus.clear();
    cascade.bus.resize(sample_length * total, 0.0);

    let diag = stage.mix_channels(frame, channel_states, &mut cascade.bus);
    // Per-speaker rows apply to the virtual speakers; unity total gain —
    // master gain applies once, on the final stereo. The returned peak is
    // ignored: clip handling watches the stereo output, and auto-gain must
    // never react to virtual buses.
    let _ = stage.finalize_output(speaker_params, 1.0, &mut cascade.bus);

    // The buses are the binaural stage's PCM input; the virtual speakers are
    // its fixed sources. With a static head the per-source HRIR update no-ops
    // and only `total` convolver pairs run, whatever the object count.
    binaural.render_frame(
        &cascade.bus,
        total,
        sample_length,
        binaural_params,
        &cascade.bin_pos,
        &cascade.bin_gain,
        &cascade.bin_direct,
        output,
    );
    diag
}
