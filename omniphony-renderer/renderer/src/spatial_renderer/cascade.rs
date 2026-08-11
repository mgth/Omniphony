//! Cascaded binaural stage (issue #220): the **full speaker pipeline** runs
//! against a fixed virtual layout, then each virtual speaker is binauralised
//! as a static source by the existing [`crate::binaural`] renderer.
//!
//! Since the speaker path became an instantiable [`SpeakerRenderStage`], the
//! cascade is literally a second stage on the virtual layout: every rendering
//! option applies — room mapping, distance model/diffuse, spread, crossover
//! bands, ramp modes, backend selection — so the cascade renders *the same
//! scene a physical room with this layout would play*, binauralised. That is
//! the mode's philosophy: headphone virtualization of the speaker render (the
//! `Direct` binaural mode remains the per-object HRTF path).
//!
//! The cost model still holds: the convolution count is bound by the layout
//! size whatever the stream carries, and with a static head the per-source
//! HRIR update no-ops ([`crate::binaural::EarConvolver::set_coeffs_smooth`]
//! compares kernels).
//!
//! Shared-state discipline (see `speaker_stage.rs` module doc): the virtual
//! stage advances the shared `channel_states` — in cascade mode it is the
//! frame's *only* mix pass, so slew advances exactly once. The caller guards
//! the layout-width change on mode/layout switches by clearing
//! `interp_prev_gains` (see `SpatialRenderer::reseed_interp_on_width_change`).
//!
//! Monitoring outputs of the virtual mix are currently dropped: the meter
//! bundle consumers assume main-layout speaker indexing. Surfacing virtual
//! buses/objects in Studio is the planned observability follow-up.

use crate::live_params::RendererControl;
use crate::speaker_layout::SpeakerLayout;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;

use super::speaker_stage::{SpeakerRenderStage, SpeakerStageFrame};
use super::{ChannelRoute, ChannelState, SpatialRenderer};

pub(super) struct CascadeStage {
    /// The `binaural.cascade_layout` string this stage was built from.
    pub(super) layout_key: String,
    /// Main topology identity seen at build time. A change means live
    /// backend/evaluation params were republished, so the virtual engines are
    /// rebuilt too (the same trigger the crossover bands use).
    pub(super) main_topology_identity: usize,
    /// The resolved virtual layout (including non-spatialized entries).
    pub(super) layout: SpeakerLayout,
    /// Per-label routing for direct (bed) channels, resolved on the virtual
    /// layout — beds route one-hot exactly like on a physical room.
    pub(super) label_to_speaker: HashMap<bridge_api::RChannelLabel, usize>,
    /// The full speaker pipeline on the virtual layout.
    pub(super) stage: SpeakerRenderStage,
    /// Fixed binaural input geometry, one entry per virtual speaker.
    /// Non-spatialized entries (LFE) are flagged direct: both ears, −3 dB,
    /// no HRTF — the binaural stage's standing policy (issue #156).
    pub(super) bin_pos: Vec<[f64; 3]>,
    pub(super) bin_gain: Vec<f32>,
    pub(super) bin_direct: Vec<bool>,
    /// Interleaved virtual-speaker bus scratch the binaural stage consumes as
    /// its PCM input. Reused every frame.
    pub(super) bus: Vec<f32>,
}

impl CascadeStage {
    /// Total virtual buses (= the virtual layout's speaker count).
    #[inline]
    pub(super) fn num_buses(&self) -> usize {
        self.bin_pos.len()
    }

    pub(super) fn build(
        control: &Arc<RendererControl>,
        layout_key: &str,
        main_topology_identity: usize,
        sample_rate: u32,
    ) -> Result<Self> {
        let layout = resolve_layout(layout_key)?;
        let num_speakers = layout.num_speakers();
        let stage = SpeakerRenderStage::new(
            control,
            &layout,
            main_topology_identity,
            num_speakers,
            sample_rate,
        )?;

        let bin_pos: Vec<[f64; 3]> = layout
            .speakers
            .iter()
            .map(|s| [s.x as f64, s.y as f64, s.z as f64])
            .collect();
        let bin_direct: Vec<bool> = layout.speakers.iter().map(|s| !s.spatialize).collect();
        // Per-channel level shaping happens in the virtual mix; the binaural
        // stage sees unity buses.
        let bin_gain = vec![1.0f32; num_speakers];

        log::info!(
            "Cascaded binaural: virtual layout '{}' with {} speakers ({} spatialized)",
            layout_key,
            num_speakers,
            layout.speakers.iter().filter(|s| s.spatialize).count(),
        );

        Ok(Self {
            layout_key: layout_key.to_string(),
            main_topology_identity,
            label_to_speaker: layout.label_to_speaker_mapping(),
            layout,
            stage,
            bin_pos,
            bin_gain,
            bin_direct,
            bus: Vec::new(),
        })
    }
}

impl SpatialRenderer {
    /// Keep the cascade stage in sync with the live `cascade_layout` and the
    /// main topology identity. Called from `render_frame` *before* the live
    /// snapshot is taken (the snapshot borrows `self` fields for the rest of
    /// the frame). Builds run on the render thread — the same trade the
    /// crossover band refresh already makes; they only happen on a mode/
    /// layout/topology change, never in steady state.
    pub(super) fn refresh_cascade_for_topology(&mut self, main_topology_identity: usize) {
        enum Action {
            UpToDate,
            /// Same layout, new topology identity: live backend/evaluation
            /// params were republished — refresh the inner stage in place
            /// (reusing its triangulated gain models when only the
            /// evaluation changed).
            RefreshInner,
            Rebuild(String),
        }
        let action = {
            let g = self.control.live.read();
            match self.cascade.as_ref() {
                Some(s)
                    if s.main_topology_identity == main_topology_identity
                        && s.layout_key == g.binaural.cascade_layout =>
                {
                    Action::UpToDate
                }
                Some(s) if s.layout_key == g.binaural.cascade_layout => Action::RefreshInner,
                _ => Action::Rebuild(g.binaural.cascade_layout.clone()),
            }
        };
        let layout_key = match action {
            Action::UpToDate => return,
            Action::RefreshInner => {
                let stage = self.cascade.as_mut().expect("RefreshInner needs a stage");
                match stage.stage.refresh_for_topology(
                    &self.control,
                    main_topology_identity,
                    &stage.layout,
                ) {
                    Ok(()) => stage.main_topology_identity = main_topology_identity,
                    Err(e) => {
                        log::warn!(
                            "Cascaded binaural: virtual stage refresh failed ({e:#}) — \
                             rendering the direct per-object path instead"
                        );
                        self.cascade = None;
                    }
                }
                return;
            }
            Action::Rebuild(key) => key,
        };
        if self.cascade_failed_identity == main_topology_identity
            && self.cascade_failed_key.as_deref() == Some(layout_key.as_str())
        {
            return;
        }
        match CascadeStage::build(
            &self.control,
            &layout_key,
            main_topology_identity,
            self.sample_rate,
        ) {
            Ok(stage) => {
                self.cascade = Some(stage);
                self.cascade_failed_key = None;
            }
            Err(e) => {
                log::warn!(
                    "Cascaded binaural: virtual stage build failed ({e:#}) — \
                     rendering the direct per-object path instead"
                );
                self.cascade = None;
                self.cascade_failed_key = Some(layout_key);
                self.cascade_failed_identity = main_topology_identity;
            }
        }
    }
}

/// `RampMode::Interp` caches per-band gains sized to the *mixing* layout in
/// the shared `ChannelState`s. When the active mix stage changes width
/// (speaker↔cascade switch, cascade layout change, or a main relayout), stale
/// entries would index out of the new width — clear them so the next block
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

/// Run the full virtual pipeline for one frame: speaker mix onto the buses,
/// output stage (neutral per-speaker params — virtual speakers have no live
/// gain/delay rows yet; profiles will provide them), then binaural rendering
/// of the fixed virtual sources into `output` (interleaved stereo).
///
/// Free function over the exact fields it needs (not a `&mut self` method):
/// `render_frame` holds a `LiveSnapshot` borrowing other `SpatialRenderer`
/// fields for the whole frame.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_cascade_frame(
    cascade: &mut CascadeStage,
    channel_states: &mut Vec<ChannelState>,
    binaural: &mut crate::binaural::BinauralRenderer,
    frame: CascadeFrameInputs<'_>,
    binaural_params: &crate::binaural::BinauralFrameParams,
    output: &mut [f32],
) {
    let total = cascade.num_buses();
    cascade.bus.clear();
    cascade.bus.resize(frame.sample_length * total, 0.0);

    let stage_frame = SpeakerStageFrame {
        input_pcm: frame.input_pcm,
        input_channel_count: frame.input_channel_count,
        sample_length: frame.sample_length,
        channel_routing: frame.channel_routing,
        label_to_speaker: &cascade.label_to_speaker,
        layout: &cascade.layout,
        object_params: frame.object_params,
        ramp_mode: frame.ramp_mode,
        ramp_strategy: frame.ramp_strategy,
        ramp_context: frame.ramp_context,
        log_object_positions: frame.log_object_positions,
        is_first: frame.is_first,
        measure_breakdown: false,
    };
    // Monitoring outputs dropped for now — see the module doc.
    let _ = cascade
        .stage
        .mix_channels(stage_frame, channel_states, &mut cascade.bus);
    // Neutral output stage: unity gains, no mutes, no delays (defaults for
    // missing entries), unity total gain — master gain applies once, on the
    // final stereo. The returned peak is ignored: clip handling watches the
    // stereo output, and auto-gain must never react to virtual buses.
    let _ = cascade.stage.finalize_output(&[], 1.0, &mut cascade.bus);

    // The buses are the binaural stage's PCM input; the virtual speakers are
    // its fixed sources. With a static head the per-source HRIR update no-ops
    // and only `total` convolver pairs run, whatever the object count.
    binaural.render_frame(
        &cascade.bus,
        total,
        frame.sample_length,
        binaural_params,
        &cascade.bin_pos,
        &cascade.bin_gain,
        &cascade.bin_direct,
        output,
    );
}

/// Frame-scoped inputs for [`render_cascade_frame`] — the subset of
/// [`SpeakerStageFrame`] the caller owns (the cascade substitutes its own
/// layout and label mapping).
pub(super) struct CascadeFrameInputs<'a> {
    pub(super) input_pcm: &'a [f32],
    pub(super) input_channel_count: usize,
    pub(super) sample_length: usize,
    pub(super) channel_routing: &'a [ChannelRoute],
    pub(super) object_params: &'a [crate::live_params::ObjectLiveParams],
    pub(super) ramp_mode: crate::live_params::RampMode,
    pub(super) ramp_strategy: &'a dyn crate::ramp_strategy::RampStrategy,
    pub(super) ramp_context: &'a crate::ramp_strategy::RampContext,
    pub(super) log_object_positions: bool,
    pub(super) is_first: bool,
}

/// Resolve the `cascade_layout` key: preset name first, then a YAML file path.
/// Non-spatialized entries (LFE) are kept — they route one-hot like on a
/// physical room and reach both ears dry through the binaural direct policy.
fn resolve_layout(key: &str) -> Result<SpeakerLayout> {
    let layout = match SpeakerLayout::preset(key) {
        Ok(layout) => layout,
        Err(preset_err) => SpeakerLayout::from_file(key).with_context(|| {
            format!("'{key}' is neither a preset ({preset_err}) nor a readable layout file")
        })?,
    };
    anyhow::ensure!(
        layout.speakers.iter().any(|s| s.spatialize),
        "virtual layout '{key}' has no spatialized speakers"
    );
    Ok(layout)
}
