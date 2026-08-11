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

use crate::crossover::{BiquadState, FreqBand, LR4CrossoverBank, compute_bands};
use crate::live_params::{ObjectLiveParams, RampMode, RendererControl};
use crate::ramp_strategy::{RampContext, RampStrategy};
use crate::render_backend::MultiBandTable;
use crate::spatial_vbap::Gains;
use crate::speaker_layout::SpeakerLayout;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;

use super::ChannelRoute;
use super::components::{BandRenderer, ChannelState, split_bands};
use super::{GAIN_SLEW_SECS, SpatialRenderer};
use crate::ramp_strategy::RampProgress;

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

/// Frame-scoped inputs for [`SpeakerRenderStage::mix_channels`], all borrowed
/// from `render_frame` locals (the live snapshot, the topology guard, the
/// routing snapshot). Grouped so the call survives the `LiveSnapshot` borrow
/// regime with one struct instead of a dozen loose arguments.
pub(super) struct SpeakerStageFrame<'a> {
    pub(super) input_pcm: &'a [f32],
    pub(super) input_channel_count: usize,
    pub(super) sample_length: usize,
    pub(super) channel_routing: &'a [ChannelRoute],
    pub(super) label_to_speaker: &'a HashMap<bridge_api::RChannelLabel, usize>,
    pub(super) layout: &'a SpeakerLayout,
    pub(super) object_params: &'a [ObjectLiveParams],
    pub(super) ramp_mode: RampMode,
    pub(super) ramp_strategy: &'a dyn RampStrategy,
    pub(super) ramp_context: &'a RampContext,
    pub(super) log_object_positions: bool,
    pub(super) is_first: bool,
    pub(super) measure_breakdown: bool,
}

/// Monitoring outputs of one mix pass (the OSC meter bundle feeds).
pub(super) struct SpeakerStageDiagnostics {
    pub(super) object_gains: Vec<(usize, Gains)>,
    pub(super) object_band_gains: Vec<(usize, Vec<Gains>)>,
    pub(super) object_band_sq: Vec<(usize, Vec<f64>)>,
    pub(super) crossover_elapsed: std::time::Duration,
}

impl SpeakerRenderStage {
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

    /// Mix every input channel into `output` (interleaved, pre-zeroed,
    /// `sample_length * num_speakers` of THIS stage's layout): direct beds
    /// one-hot via the label mapping, objects through the per-band engines
    /// (crossover split when active), per-sample gain slew. Advances the
    /// shared `channel_states` (ramps + slew) — exactly one mix pass per
    /// `channel_states` per frame (see the module doc).
    pub(super) fn mix_channels(
        &mut self,
        frame: SpeakerStageFrame<'_>,
        channel_states: &mut Vec<ChannelState>,
        output: &mut [f32],
    ) -> SpeakerStageDiagnostics {
        let SpeakerStageFrame {
            input_pcm,
            input_channel_count,
            sample_length,
            channel_routing,
            label_to_speaker: active_label_to_speaker,
            layout: active_layout,
            object_params,
            ramp_mode,
            ramp_strategy,
            ramp_context,
            log_object_positions,
            is_first,
            measure_breakdown,
        } = frame;
        // Per-object VBAP gains at the final sample — monitoring only (OSC meter
        // bundle). Only collected when `measure_breakdown` is set; left empty (no
        // allocation) on the plain render path (e.g. mpv without Studio open).
        let mut object_gains_out: Vec<(usize, Gains)> = if measure_breakdown {
            Vec::with_capacity(input_channel_count)
        } else {
            Vec::new()
        };
        let mut object_band_gains_out: Vec<(usize, Vec<Gains>)> = Vec::new();
        let mut object_band_sq_out: Vec<(usize, Vec<f64>)> = Vec::new();
        let mut crossover_elapsed = std::time::Duration::ZERO;
        let profile_crossover = measure_breakdown && self.crossover_filter_bank.is_some();

        // Directly-routed channels always come FIRST in PCM data, then objects.
        let num_routed = channel_routing.len();

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

        // Process each channel
        for input_channel_idx in 0..input_channel_count {
            // Per-channel mute (applies to beds and objects), as a 0/1 factor.
            let obj_gain = match object_params.get(input_channel_idx) {
                Some(o) if o.muted => 0.0,
                _ => 1.0,
            };

            // Get gain from cached metadata (common for ALL channels - beds and objects)
            let state = SpatialRenderer::state_mut(channel_states, input_channel_idx);
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
                // Deliberately NOT filtered on `initialized`, to match the
                // HashMap version exactly: `state_mut` above already created
                // this channel's entry earlier in the same iteration, so the
                // lookup was always `Some` and the `None` arm was unreachable.
                // Filtering would make it reachable. No behavioural difference
                // could be demonstrated either way — `null_partial_metadata`
                // passes with and without the filter — so this keeps the
                // original control flow rather than relying on the two being
                // equivalent.
                let state_mut = channel_states.get_mut(input_channel_idx);
                let state = match state_mut {
                    // Skip if no metadata available
                    Some(s) => s,
                    None => {
                        if log_object_positions {
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
                match ramp_mode {
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
                    // Per-band energy for the meters. Under metering every ramp
                    // arm took the block path, so `crossover_band_scratch` still
                    // holds this object's full block of band samples.
                    if profile_crossover {
                        let sums: Vec<f64> = (0..band_gains.len())
                            .map(|b| {
                                self.crossover_band_scratch[b][..sample_length]
                                    .iter()
                                    .map(|&s| (s as f64) * (s as f64))
                                    .sum()
                            })
                            .collect();
                        object_band_sq_out.push((input_channel_idx, sums));
                    }
                }

                // Return the pooled buffer for the next object/frame.
                self.band_gains_scratch = band_gains;
            }
        }

        SpeakerStageDiagnostics {
            object_gains: object_gains_out,
            object_band_gains: object_band_gains_out,
            object_band_sq: object_band_sq_out,
            crossover_elapsed,
        }
    }

    /// Build a stage for `layout`: band engines (+ crossover bank when the
    /// layout defines finite crossover edges), unified table, delay lines and
    /// per-layout scratch.
    pub(super) fn new(
        control: &Arc<RendererControl>,
        layout: &SpeakerLayout,
        topology_identity: usize,
        num_speakers: usize,
        sample_rate: u32,
    ) -> Result<Self> {
        let (render_bands, crossover_filter_bank) =
            Self::build_crossover(control, layout, num_speakers, sample_rate, &[])?;
        let unified_table = Self::build_unified_table(&render_bands, num_speakers);
        Ok(Self {
            num_speakers,
            sample_rate,
            render_bands,
            render_bands_topology_identity: topology_identity,
            unified_table,
            crossover_filter_bank,
            crossover_filter_states: Vec::new(),
            crossover_band_scratch: std::array::from_fn(|_| Vec::new()),
            band_gains_scratch: Vec::new(),
            interp_end_scratch: Vec::new(),
            speaker_gains_buf: vec![0.0f32; num_speakers],
            bed_routing_gains_buf: vec![0.0f32; num_speakers],
            delay_lines: {
                let max_delay = (0.1 * sample_rate as f32) as usize; // 100 ms
                (0..num_speakers)
                    .map(|_| crate::delay_line::DelayLine::new(max_delay))
                    .collect()
            },
        })
    }

    /// Rebuild the band engines when the published topology changed. Passes the
    /// current bands so an evaluation-only recompute (unchanged geometry
    /// generation) reuses each band's triangulated gain model and rebuilds only
    /// the evaluation wrapper, instead of re-triangulating every band.
    ///
    /// Deliberately does NOT clear the delay lines: those keep their memory
    /// across topology refreshes (only the crossover filter states reset, as
    /// their count depends on the new bank).
    pub(super) fn refresh_for_topology(
        &mut self,
        control: &Arc<RendererControl>,
        topology_identity: usize,
        active_layout: &SpeakerLayout,
    ) -> Result<()> {
        if self.render_bands_topology_identity == topology_identity {
            return Ok(());
        }

        let (render_bands, crossover_filter_bank) = Self::build_crossover(
            control,
            active_layout,
            self.num_speakers,
            self.sample_rate,
            &self.render_bands,
        )?;
        self.unified_table = Self::build_unified_table(&render_bands, self.num_speakers);
        self.render_bands = render_bands;
        self.crossover_filter_bank = crossover_filter_bank;
        self.crossover_filter_states.clear();
        self.crossover_band_scratch.iter_mut().for_each(Vec::clear);
        self.render_bands_topology_identity = topology_identity;
        Ok(())
    }

    /// Output stage: per-speaker gains (live gain/mute × `total_gain`), delay
    /// lines, and peak detection over the interleaved buffer. Returns
    /// `(peak_sample, peak_speaker_idx)`; the caller owns clip reporting and
    /// auto-gain (a virtual stage must never fold reductions into the shared
    /// master gain).
    pub(super) fn finalize_output(
        &mut self,
        speaker_params: &[crate::live_params::SpeakerLiveParams],
        total_gain: f32,
        output: &mut [f32],
    ) -> (f32, usize) {
        // Pre-compute per-speaker total gains and update delay-line targets in a
        // single pass over the speaker list — one HashMap lookup per speaker.
        // Mute overrides gain to 0.0 without touching the stored gain value.
        self.speaker_gains_buf
            .iter_mut()
            .enumerate()
            .for_each(|(idx, g)| {
                let sp = speaker_params.get(idx);
                *g = if sp.is_some_and(|s| s.muted) {
                    0.0
                } else {
                    total_gain * sp.map_or(1.0, |s| s.gain)
                };
            });
        for (idx, dl) in self.delay_lines.iter_mut().enumerate() {
            dl.set_target_ms(
                speaker_params.get(idx).map_or(0.0, |s| s.delay_ms),
                self.sample_rate,
            );
        }
        let speaker_total_gains = &self.speaker_gains_buf;

        // Apply per-speaker gains and delay lines, and detect peak (tracking which
        // speaker channel held the peak, for clip reporting).
        let sample_length = output.len() / self.num_speakers.max(1);
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
        (peak_sample, peak_speaker_idx)
    }

    /// Push the live read-time interpolation flag into the precomputed
    /// evaluators and the unified table. This flag only selects nearest-cell
    /// vs trilinear at lookup time; the table content is independent of it, so
    /// toggling it never rebuilds the table. Synced every frame — just a
    /// handful of relaxed atomic stores.
    pub(super) fn sync_position_interpolation(&self, interpolate: bool) {
        for band in &self.render_bands {
            if let Some(engine) = band.engine() {
                engine.set_position_interpolation(interpolate);
            }
        }
        if let Some(table) = self.unified_table.as_ref() {
            table.set_position_interpolation(interpolate);
        }
    }

    /// Build crossover band engines from a speaker layout.
    ///
    /// Returns `(render_bands, Some(filter_bank))` when the layout defines finite crossover
    /// edges on at least one speaker (producing ≥ 2 bands), or `(single_band, None)` when
    /// no crossover is needed. `render_bands` always has at least one entry.
    fn build_crossover(
        control: &Arc<RendererControl>,
        layout: &SpeakerLayout,
        num_speakers: usize,
        sample_rate: u32,
        prev_bands: &[BandRenderer],
    ) -> Result<(Vec<BandRenderer>, Option<LR4CrossoverBank>)> {
        // For each new band, reuse the matching previous band (same speaker subset)
        // so an evaluation-only refresh can keep its triangulated gain model.
        let make_renderer = |b: &FreqBand| {
            let prev = prev_bands
                .iter()
                .find(|p| p.speaker_indices == b.speaker_indices);
            BandRenderer::from_band(b, layout, num_speakers, control, prev)
        };

        let bands = compute_bands(layout);
        if bands.len() <= 1 {
            let render_bands = bands
                .iter()
                .map(make_renderer)
                .collect::<Result<Vec<_>>>()?;
            return Ok((render_bands, None));
        }

        let cutoffs: Vec<f32> = bands
            .windows(2)
            .map(|w| w[0].high_hz)
            .filter(|f| f.is_finite())
            .collect();

        let filter_bank = LR4CrossoverBank::new(&cutoffs, sample_rate);
        let render_bands = bands
            .iter()
            .map(make_renderer)
            .collect::<Result<Vec<_>>>()?;

        log::info!(
            "Crossover enabled: {} bands, cutoffs = {:?} Hz",
            bands.len(),
            cutoffs
        );

        Ok((render_bands, Some(filter_bank)))
    }

    /// Merge the per-band cartesian tables into a single multi-band table so a
    /// lookup localises the cell once for all bands. Returns `None` (→ per-band
    /// path) unless there are several bands all backed by a cartesian evaluator.
    fn build_unified_table(
        render_bands: &[BandRenderer],
        num_speakers: usize,
    ) -> Option<MultiBandTable> {
        if render_bands.len() <= 1 {
            return None;
        }
        // Every band shares the active evaluation mode, so they are all cartesian
        // or all polar. Try cartesian first; if any band has no cartesian view,
        // fall through to the polar path. A band without an engine (< 3 speakers)
        // has no precomputed table → no unified table (per-band path).
        let mut cartesian = Vec::with_capacity(render_bands.len());
        let mut all_cartesian = true;
        for band in render_bands {
            let engine = band.engine()?;
            match engine.cartesian_parts() {
                Some(parts) => cartesian.push((parts, band.speaker_indices.as_slice())),
                None => {
                    all_cartesian = false;
                    break;
                }
            }
        }
        if all_cartesian {
            let table = MultiBandTable::build_cartesian(&cartesian, num_speakers);
            if table.is_some() {
                log::info!(
                    "Crossover: unified cartesian table built for {} bands",
                    render_bands.len()
                );
            }
            return table;
        }
        drop(cartesian);

        let mut polar = Vec::with_capacity(render_bands.len());
        for band in render_bands {
            let engine = band.engine()?;
            polar.push((engine.polar_parts()?, band.speaker_indices.as_slice()));
        }
        let table = MultiBandTable::build_polar(&polar, num_speakers);
        if table.is_some() {
            log::info!(
                "Crossover: unified polar table built for {} bands",
                render_bands.len()
            );
        }
        table
    }
}
