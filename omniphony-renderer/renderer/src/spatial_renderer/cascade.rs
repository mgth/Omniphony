//! Cascaded binaural stage (issue #220): objects are VBAP-panned onto a fixed
//! virtual speaker layout, then each virtual speaker is binauralised as a
//! static source by the existing [`crate::binaural`] renderer.
//!
//! The point is the cost model: the direct binaural path convolves one HRIR
//! pair *per object* (128-tap FIR × 2 ears each), so its cost grows with the
//! object count. Here the convolution count is bound by the virtual layout
//! size, whatever the stream carries — the mix onto the buses costs a few
//! multiplies per object per sample (VBAP gains are sparse), and the virtual
//! speakers never move, so the per-source HRIR/ITD update in the binaural
//! stage is a no-op every frame the head does not turn
//! ([`crate::binaural::EarConvolver::set_coeffs_smooth`] compares kernels).
//!
//! Layout convention: the virtual layout carries only spatialized speakers.
//! Input channels routed to a *non-spatialized* speaker (the LFE) bypass the
//! cascade onto a dedicated **direct bus** — the last binaural input channel,
//! flagged `chan_direct` so the LFE policy (both ears, −3 dB, no HRTF) applies
//! unchanged. Non-spatialized entries in a user-supplied layout are dropped
//! with a log.

use crate::live_params::{ObjectLiveParams, RenderTopology, RendererControl};
use crate::ramp_strategy::{RampContext, RampProgress, RampStrategy};
use crate::speaker_layout::SpeakerLayout;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;

use super::{ChannelRoute, ChannelState, GAIN_SLEW_SECS, SpatialRenderer};

pub(super) struct CascadeStage {
    /// Virtual-layout topology: the prepared render engine (VBAP or whichever
    /// backend is live) built for the virtual layout.
    pub(super) topology: Arc<RenderTopology>,
    /// Request parameters for the virtual pan. Deliberately **neutral** —
    /// no `room_ratio` warp, no distance diffuse: those map ADM space onto a
    /// physical room, and the binaural convention (see `BinauralLiveParams`)
    /// is that anisotropic room mapping must not distort HRTF directions.
    /// A source sitting on a virtual speaker direction then collapses to a
    /// one-hot pan, i.e. exactly the direct per-object render.
    pub(super) request_params: crate::ramp_strategy::RampRenderParams,
    /// The `binaural.cascade_layout` string this stage was built from.
    pub(super) layout_key: String,
    /// Main topology identity seen at build time. A change means live
    /// backend/evaluation params were republished, so the virtual engine is
    /// rebuilt too (the same trigger the crossover bands use).
    pub(super) main_topology_identity: usize,
    /// Fixed binaural input geometry: one entry per virtual speaker plus the
    /// trailing direct bus. Never changes between rebuilds.
    pub(super) bin_pos: Vec<[f64; 3]>,
    pub(super) bin_gain: Vec<f32>,
    pub(super) bin_direct: Vec<bool>,
    /// Interleaved (K+1)-channel scratch: the virtual speaker buses the
    /// binaural stage consumes as its PCM input. Reused every frame.
    pub(super) bus: Vec<f32>,
    /// Per input channel: the per-virtual-speaker gains (panning × channel
    /// gain) applied at the end of the previous block. The mix interpolates
    /// from these to the current block's target so neither object motion nor
    /// gain slew steps at a block boundary.
    pub(super) prev_gains: Vec<Vec<f32>>,
    /// `prev_gains[c]` is only meaningful when the channel rendered in the
    /// immediately preceding block; otherwise the mix fades in from zero.
    pub(super) prev_valid: Vec<bool>,
}

impl CascadeStage {
    /// Number of virtual speakers (excluding the direct bus).
    #[inline]
    pub(super) fn num_virtual(&self) -> usize {
        self.bin_pos.len() - 1
    }

    /// Total binaural input channels: virtual speakers + direct bus.
    #[inline]
    pub(super) fn num_buses(&self) -> usize {
        self.bin_pos.len()
    }

    pub(super) fn build(
        control: &Arc<RendererControl>,
        layout_key: &str,
        main_topology_identity: usize,
        prev: Option<&RenderTopology>,
    ) -> Result<Self> {
        let layout = resolve_layout(layout_key)?;
        let mut plan = control
            .prepare_topology_rebuild_for_layout(layout)
            .ok_or_else(|| anyhow::anyhow!("failed to prepare the virtual topology rebuild"))?;
        // Neutralize the room mapping in the evaluation template so precomputed
        // tables bake the same neutral pan the realtime path computes (see
        // `request_params`).
        let template = &mut plan.evaluation_build_config.request_template;
        template.room_ratio = [1.0; 3];
        template.room_ratio_rear = 1.0;
        template.room_ratio_lower = 1.0;
        template.room_ratio_center_blend = 0.0;
        template.use_distance_diffuse = false;
        // No distance attenuation either: like the direct binaural path,
        // object/bed levels are authored upstream (the direct path applies no
        // inverse-distance gain — distance drives *cues* only).
        template.distance_model = crate::spatial_vbap::DistanceModel::None;
        let request_params = crate::ramp_strategy::RampRenderParams {
            room_ratio: template.room_ratio,
            room_ratio_rear: template.room_ratio_rear,
            room_ratio_lower: template.room_ratio_lower,
            room_ratio_center_blend: template.room_ratio_center_blend,
            use_distance_diffuse: template.use_distance_diffuse,
            distance_diffuse_threshold: template.distance_diffuse_threshold,
            distance_diffuse_curve: template.distance_diffuse_curve,
            diffuse_mirror_axes: template.diffuse_mirror_axes,
            distance_model: template.distance_model,
        };
        // Reuse the previous stage's triangulated gain model when only the
        // evaluation mode/grid changed (same fast path as the crossover bands).
        let topology = Arc::new(plan.build_topology_reusing(prev)?);
        let k = topology.num_speakers;

        let mut bin_pos: Vec<[f64; 3]> = topology
            .speaker_layout
            .speakers
            .iter()
            .map(|s| [s.x as f64, s.y as f64, s.z as f64])
            .collect();
        // Direct (LFE) bus: fed to both ears equally, position never read.
        bin_pos.push([0.0, 1.0, 0.0]);
        let mut bin_direct = vec![false; k];
        bin_direct.push(true);
        // Per-channel gains are folded into the bus mix; the binaural stage
        // sees unity.
        let bin_gain = vec![1.0f32; k + 1];

        log::info!(
            "Cascaded binaural: virtual layout '{}' with {} speakers (+1 direct bus), backend {}",
            layout_key,
            k,
            topology.backend.backend_id(),
        );

        Ok(Self {
            topology,
            request_params,
            layout_key: layout_key.to_string(),
            main_topology_identity,
            bin_pos,
            bin_gain,
            bin_direct,
            bus: Vec::new(),
            prev_gains: Vec::new(),
            prev_valid: Vec::new(),
        })
    }

    /// Size the per-frame scratch for this block. The bus is zeroed; per-channel
    /// history grows (never shrinks) with the input channel count.
    pub(super) fn begin_frame(&mut self, input_channel_count: usize, sample_length: usize) {
        let total = self.num_buses();
        self.bus.clear();
        self.bus.resize(sample_length * total, 0.0);
        if self.prev_gains.len() < input_channel_count {
            self.prev_gains.resize_with(input_channel_count, Vec::new);
            self.prev_valid.resize(input_channel_count, false);
        }
    }
}

impl SpatialRenderer {
    /// Keep the cascade stage in sync with the live `cascade_layout` and the
    /// main topology identity. Called from `render_frame` *before* the live
    /// snapshot is taken (the snapshot borrows `self` fields for the rest of
    /// the frame). Builds run on the render thread — the same trade the
    /// crossover band refresh already makes; they only happen on a mode/layout/
    /// topology change, never in steady state.
    pub(super) fn refresh_cascade_for_topology(&mut self, main_topology_identity: usize) {
        let layout_key = {
            let g = self.control.live.read();
            let up_to_date = self.cascade.as_ref().is_some_and(|stage| {
                stage.main_topology_identity == main_topology_identity
                    && stage.layout_key == g.binaural.cascade_layout
            });
            if up_to_date {
                return;
            }
            g.binaural.cascade_layout.clone()
        };
        if self.cascade_failed_identity == main_topology_identity
            && self.cascade_failed_key.as_deref() == Some(layout_key.as_str())
        {
            return;
        }
        let prev = self.cascade.take();
        match CascadeStage::build(
            &self.control,
            &layout_key,
            main_topology_identity,
            prev.as_ref().map(|s| s.topology.as_ref()),
        ) {
            Ok(mut stage) => {
                // Same layout rebuilt for an evaluation/topology refresh: keep
                // the per-channel interpolation history so audio stays smooth
                // across the swap.
                if let Some(prev) = prev {
                    if prev.layout_key == stage.layout_key
                        && prev.num_virtual() == stage.num_virtual()
                    {
                        stage.prev_gains = prev.prev_gains;
                        stage.prev_valid = prev.prev_valid;
                    }
                }
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

/// Mix every input channel onto the virtual speaker buses, then binauralise
/// the buses as fixed sources.
///
/// Free function over the exact fields it needs (not a `&mut self` method):
/// `render_frame` holds a `LiveSnapshot` borrowing other `SpatialRenderer`
/// fields for the whole frame, so a full-`self` borrow cannot exist there.
///
/// Position policy mirrors the direct binaural branch: beds at their
/// main-layout speaker direction, objects at their block-ramped position,
/// LFE-routed channels onto the trailing direct bus. Panning gains and the
/// channel gain slew are folded together and interpolated per sample from the
/// previous block's applied gains, so neither object motion nor gain steps
/// click at block boundaries.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_cascade_frame(
    stage: &mut CascadeStage,
    channel_states: &mut Vec<ChannelState>,
    object_params: &[ObjectLiveParams],
    binaural: &mut crate::binaural::BinauralRenderer,
    sample_rate: u32,
    input_pcm: &[f32],
    input_channel_count: usize,
    sample_length: usize,
    channel_routing: &[ChannelRoute],
    active_layout: &SpeakerLayout,
    active_label_to_speaker: &HashMap<bridge_api::RChannelLabel, usize>,
    ramp_strategy: &dyn RampStrategy,
    ramp_context: &RampContext,
    binaural_params: &crate::binaural::BinauralFrameParams,
    output: &mut [f32],
) {
    stage.begin_frame(input_channel_count, sample_length);
    let k = stage.num_virtual();
    let total = stage.num_buses();
    let ramp_samples = sample_rate as f32 * GAIN_SLEW_SECS;
    // Neutral pan parameters (no room warp) — see `CascadeStage::request_params`.
    let render_params = stage.request_params;
    let inv_len = if sample_length > 0 {
        1.0 / sample_length as f32
    } else {
        0.0
    };

    for c in 0..input_channel_count {
        // Object-level mute as a 0/1 factor (same as the direct branch).
        let obj_gain = match object_params.get(c) {
            Some(o) if o.muted => 0.0,
            _ => 1.0,
        };
        let state = SpatialRenderer::state_mut(channel_states, c);
        // Stream metadata gain: silent (-128 = -inf dB) until metadata arrives.
        let gain_db = state.gain_db;
        let gain_linear = if gain_db == -128 {
            0.0
        } else {
            10.0_f32.powf(gain_db as f32 / 20.0)
        };
        let (gain_start, gain_step) =
            state.slew_gain(gain_linear * obj_gain, sample_length, ramp_samples);
        let gain_end = gain_start + gain_step * sample_length as f32;

        let direct_label = match channel_routing.get(c) {
            Some(ChannelRoute::Direct(label)) => Some(*label),
            _ => None,
        };

        // Source position + extent for the virtual pan.
        let pos: [f64; 3];
        let size: [f32; 3];
        if let Some(label) = direct_label {
            let speaker = active_label_to_speaker
                .get(&label)
                .and_then(|&idx| active_layout.speakers.get(idx));
            match speaker {
                Some(s) if !s.spatialize => {
                    // LFE policy (issue #156): no usable direction — dedicated
                    // direct bus, unity routing, gain slewed per sample.
                    if gain_start != 0.0 || gain_step != 0.0 {
                        for s_idx in 0..sample_length {
                            let f = gain_start + gain_step * s_idx as f32;
                            stage.bus[s_idx * total + k] +=
                                input_pcm[s_idx * input_channel_count + c] * f;
                        }
                    }
                    stage.prev_valid[c] = false;
                    continue;
                }
                // Bed: a point source at its main-layout speaker direction.
                Some(s) => {
                    pos = [s.x as f64, s.y as f64, s.z as f64];
                    size = [0.0; 3];
                }
                // Label absent from the layout — skipped, like the speaker path.
                None => {
                    stage.prev_valid[c] = false;
                    continue;
                }
            }
        } else {
            // Object: advance the position ramp at block granularity, exactly
            // like the direct binaural branch (nothing else advances ramps in
            // binaural mode).
            let progress = state.ramp.current_progress().unwrap_or(RampProgress {
                completed_units: 0,
                total_units: 0,
            });
            ramp_strategy.evaluate(&mut state.ramp, progress, ramp_context);
            pos = state.ramp.output_position;
            state.ramp.commit_output_position();
            state.ramp.advance_ramp(sample_length as u64);
            size = state.ramp.current_size;
        }

        let silent = gain_start == 0.0 && gain_step == 0.0;
        let prev = &mut stage.prev_gains[c];
        if prev.len() != k {
            prev.clear();
            prev.resize(k, 0.0);
            stage.prev_valid[c] = false;
        }
        if silent && !stage.prev_valid[c] {
            continue;
        }
        // Target gains: pure panning × block-end channel gain. A silent block
        // skips the panning lookup — it only fades the previous tail out.
        let target = (!silent).then(|| {
            let req = render_params.render_request_for_event(pos, size);
            stage.topology.backend.compute_gains(&req).gains
        });
        debug_assert!(target.as_ref().is_none_or(|g| g.len() == k));
        if !stage.prev_valid[c] {
            prev.iter_mut().for_each(|v| *v = 0.0);
        }

        let mut any_target = false;
        for ki in 0..k {
            let a = prev[ki];
            let b = match target.as_ref() {
                Some(g) => g[ki] * gain_end,
                None => 0.0,
            };
            if b != 0.0 {
                any_target = true;
            }
            // VBAP gains are sparse (≤3 speakers for a point source): skip
            // buses this channel neither feeds now nor fed last block.
            if a == 0.0 && b == 0.0 {
                continue;
            }
            let step = (b - a) * inv_len;
            let mut g = a;
            let mut in_idx = c;
            let mut bus_idx = ki;
            for _ in 0..sample_length {
                g += step;
                stage.bus[bus_idx] += input_pcm[in_idx] * g;
                in_idx += input_channel_count;
                bus_idx += total;
            }
            prev[ki] = b;
        }
        stage.prev_valid[c] = any_target;
    }

    // The buses are the binaural stage's PCM input; the virtual speakers are
    // its fixed sources (per-channel gain is already folded into the buses, so
    // `bin_gain` is unity). With a static head the per-source HRIR update
    // no-ops and only K+1 convolver pairs run, whatever the object count.
    binaural.render_frame(
        &stage.bus,
        total,
        sample_length,
        binaural_params,
        &stage.bin_pos,
        &stage.bin_gain,
        &stage.bin_direct,
        output,
    );
}

/// Resolve the `cascade_layout` key: preset name first, then a YAML file path.
/// Non-spatialized entries (LFE) are dropped — the cascade routes those input
/// channels onto its dedicated direct bus instead.
fn resolve_layout(key: &str) -> Result<SpeakerLayout> {
    let layout = match SpeakerLayout::preset(key) {
        Ok(layout) => layout,
        Err(preset_err) => SpeakerLayout::from_file(key).with_context(|| {
            format!("'{key}' is neither a preset ({preset_err}) nor a readable layout file")
        })?,
    };
    let total = layout.speakers.len();
    let speakers: Vec<_> = layout
        .speakers
        .into_iter()
        .filter(|s| s.spatialize)
        .collect();
    if speakers.len() < total {
        log::info!(
            "Cascaded binaural: dropped {} non-spatialized speaker(s) from '{key}' \
             (LFE-routed channels use the direct bus instead)",
            total - speakers.len(),
        );
    }
    anyhow::ensure!(
        !speakers.is_empty(),
        "virtual layout '{key}' has no spatialized speakers"
    );
    Ok(SpeakerLayout {
        radius_m: layout.radius_m,
        speakers,
    })
}
