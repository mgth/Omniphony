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

use crate::crossover::{
    CrossoverBank, CrossoverStates, FirCrossoverBank, FreqBand, LR4CrossoverBank, compute_bands,
};
use crate::delay_line::IntegerDelay;
use crate::live_params::{
    CrossoverInfo, CrossoverType, MAX_SAMPLE_RAMP_STRIDE, ObjectLiveParams, RampMode,
    RenderTopology, RendererControl,
};
use crate::ramp_strategy::{RampContext, RampStrategy};
use crate::render_backend::{CornerCache, MultiBandTable};
use crate::spatial_vbap::Gains;
use crate::speaker_layout::SpeakerLayout;
use anyhow::Result;
use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use super::ChannelRoute;
use super::components::{BandRenderer, ChannelState};
use super::{GAIN_SLEW_SECS, SpatialRenderer};
use crate::ramp_strategy::RampProgress;

mod band_worker;
use band_worker::{BandWorker, FailedBuild, Finished};

/// What a band set is built for: the published topology and the crossover
/// options that are live rather than part of it. Two sets with the same key
/// are the same set.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct BandSetKey {
    /// `Arc::as_ptr` of the topology. The set keeps that `Arc`, so the
    /// address cannot be reused while the set is installed.
    topology: usize,
    crossover_type: CrossoverType,
    /// The FIR transition ratio; 0 when the engine is not FIR, so a tuning
    /// change of an inactive engine rebuilds nothing.
    fir_ratio: f32,
}

impl BandSetKey {
    fn wanted(control: &RendererControl, topology: &Arc<RenderTopology>) -> Self {
        let live = control.live.read();
        Self {
            topology: Arc::as_ptr(topology) as usize,
            crossover_type: live.options.crossover_type,
            fir_ratio: if live.options.crossover_type == CrossoverType::Fir {
                live.options.crossover_fir_transition_ratio
            } else {
                0.0
            },
        }
    }
}

/// Everything the speaker stage builds for one [`BandSetKey`]: what
/// [`SpeakerRenderStage::refresh_for_topology`] installs at once.
pub(super) struct BandSet {
    key: BandSetKey,
    topology: Arc<RenderTopology>,
    render_bands: Vec<BandRenderer>,
    crossover_filter_bank: Option<CrossoverBank>,
    /// The facts about that bank, published on the control when the set is
    /// installed: a set that is dropped instead must not be advertised.
    crossover_info: CrossoverInfo,
    /// Fresh filter memory for that bank, for the channels that were being
    /// filtered when the set was asked for: allocated with the set, so the
    /// first block mixed on it does not (the FIR engine's is large).
    crossover_filter_states: Vec<Option<CrossoverStates>>,
    unified_table: Option<MultiBandTable>,
    speaker_freq_ranges: Vec<(Option<f32>, Option<f32>)>,
}

/// Bands a build can start from. For the topology they were built for, they
/// are taken over as they are: a band's gain table depends on the topology,
/// not on the crossover engine or the sample rate, so a change of those
/// samples nothing. For another topology, their gain models are reused where
/// the geometry did not change (see [`BandRenderer::from_band`]).
pub(super) struct PreviousBands<'a> {
    pub(super) topology: Option<&'a Arc<RenderTopology>>,
    pub(super) bands: &'a [BandRenderer],
}

pub(super) struct SpeakerRenderStage {
    /// Output width of THIS stage's layout (total speakers, incl. LFE).
    pub(super) num_speakers: usize,
    /// Sample rate for slew and delay-target conversion.
    pub(super) sample_rate: u32,
    /// Per-band VBAP engines. Once built, always ≥1 entry (the "all speakers"
    /// band when no crossover is configured). Before the first build (see
    /// [`Self::unbuilt`]) it is empty, or holds the bands of the stage this
    /// one replaces ([`Self::unbuilt_replacing`]) for that build to take
    /// over; nothing is mixed before it. Each returns full-size `Gains`.
    pub(super) render_bands: Vec<BandRenderer>,
    /// What the installed band engines were built for; `None` until the
    /// first build, which [`Self::refresh_for_topology`] then always runs.
    built: Option<BandSetKey>,
    /// The topology `render_bands` were built for, kept so its address — the
    /// key — stays unique while they are installed, and to tell a build
    /// whether it can take them over.
    built_topology: Option<Arc<RenderTopology>>,
    /// The set last asked of the worker and not answered yet.
    requested: Option<BandSetKey>,
    /// The key the worker last failed to build a set for, while it is still
    /// the one wanted: not asked again until the key has moved off it.
    failed: Option<FailedBuild>,
    /// Builds band sets off the render thread, and frees the retired ones.
    worker: BandWorker,
    /// Build band sets on the render thread, on the frame that needs them —
    /// see [`SpatialRenderer::set_synchronous_stage_builds`].
    pub(super) synchronous_builds: bool,
    /// Merged multi-band cartesian table when all bands use cartesian
    /// evaluators (`None` → per-band path). `pub(super)`: tests force the
    /// per-band path by clearing it.
    pub(super) unified_table: Option<MultiBandTable>,
    /// Per-channel copy of the unified-table cell each object sits in, keyed
    /// by channel index like `crossover_filter_states`. A cache names the table
    /// it was filled from, so one filled before a rebuild can never be read
    /// against the table that replaced it.
    pub(super) table_caches: Vec<CornerCache>,
    /// `None` when `render_bands` has exactly 1 entry (no crossover active).
    /// The engine inside (LR4 IIR vs linear-phase FIR) follows the
    /// `crossover_type` live option, compared every frame through
    /// [`BandSetKey`] so a flip rebuilds the bank without a topology change.
    pub(super) crossover_filter_bank: Option<CrossoverBank>,
    /// Per-object filter states for the crossover bank, keyed by channel index.
    pub(super) crossover_filter_states: Vec<Option<CrossoverStates>>,
    /// Per-channel compensation delays for directly-routed (bed) channels,
    /// keyed by channel index like `crossover_filter_states`. Only populated
    /// when the active crossover engine has latency (FIR): a direct route
    /// bypasses the filter bank, so without this delay beds would land
    /// `latency_samples()` early relative to every filtered object.
    pub(super) bed_delays: Vec<Option<IntegerDelay>>,
    /// Generator for the speaker test signal. Kept on the stage (not created per
    /// test) so starting one allocates nothing and the render path stays clean.
    pub(super) test_noise: crate::speaker_test::PinkNoise,
    /// Crossover states for the test signal, sized on first use. Separate from
    /// `crossover_filter_states`: the test is its own source and must not share
    /// filter memory with an input channel.
    pub(super) test_filter_states: Option<CrossoverStates>,
    /// Usable frequency range per speaker of THIS stage's layout
    /// (`(freq_low, freq_high)`, `None` = open end), captured at build time so
    /// the test can band-limit a direct (non-spatialized) speaker to its own
    /// range — such a speaker appears in no crossover band.
    pub(super) speaker_freq_ranges: Vec<(Option<f32>, Option<f32>)>,
    /// Dedicated LR4 bank splitting at the tested direct speaker's own edges.
    /// Built lazily when a test targets a direct speaker that declares a
    /// frequency range; dropped whenever the test identity changes (its edges
    /// are the tested speaker's own). Always LR4 regardless of the live
    /// `crossover_type`: a single-speaker test has nothing to phase-align with,
    /// so the FIR engine's latency would buy nothing.
    pub(super) test_direct_bank: Option<CrossoverBank>,
    /// Samples the current test has been running, against the safety cap. Reset
    /// whenever the test target or level changes.
    pub(super) test_elapsed_samples: u64,
    /// The test the elapsed count belongs to, to spot a restart.
    pub(super) test_identity: Option<(usize, u32)>,
    /// Crossover states for the object test's own signal. Separate from both
    /// `crossover_filter_states` and `test_filter_states` for the same reason
    /// those are separate from each other: it is a third independent source, and
    /// sharing filter memory would splice one signal's tail into another.
    pub(super) object_test_filter_states: Option<CrossoverStates>,
    /// Previous block's destination gains for the object test, per band — the
    /// start point of this block's interpolation. Exactly the role
    /// `ChannelState::interp_prev_gains` plays for a real object; the test has
    /// no `ChannelState` of its own, so it keeps its own copy. Empty when no
    /// test is running, which is also how a fresh start is detected.
    pub(super) object_test_prev_gains: Vec<Gains>,
    /// Destination gains for this block, per band.
    pub(super) object_test_end_gains: Vec<Gains>,
    /// Band samples of the source being mixed: one block per band, refilled
    /// for every object (and for the object test). The band meters read the
    /// last object's block back from here.
    pub(super) crossover_band_scratch: [Vec<f32>; 8],
    /// The speaker-major bus the block is mixed on before it is interleaved
    /// into the output: `num_speakers` blocks back to back. Grown when the
    /// block length rises, never per block.
    pub(super) mix_bus: Vec<f32>,
    /// Interpolation fractions of a block, `(i + 1) / len`: shared by every
    /// source whose gains are interpolated across the block.
    pub(super) block_fractions: Vec<f32>,
    /// `RampMode::Sample`: per-channel gains a block that ended mid-movement
    /// hands to the next one, keyed by channel index.
    pub(super) gain_carries: Vec<GainCarry>,
    /// Count of mix passes, which is what dates a [`GainCarry`].
    pub(super) mix_pass: u64,
    /// `RampMode::Sample`: pooled gains at the end of the interpolation
    /// segment being mixed.
    pub(super) segment_end_scratch: Vec<Gains>,
    /// Reusable per-object band-gain buffer (taken via `mem::take` per object).
    pub(super) band_gains_scratch: Vec<Gains>,
    /// `RampMode::Interp` only: pooled destination band gains for the object
    /// currently being rendered.
    pub(super) interp_end_scratch: Vec<Gains>,
    /// Per-speaker gain scratch — pre-allocated once, reused every frame.
    pub(super) speaker_gains_buf: Vec<f32>,
    /// Per-speaker delay lines — fixed 100 ms capacity, render-thread owned.
    pub(super) delay_lines: Vec<crate::delay_line::DelayLine>,
    /// The metering lists a metered frame fills: lent to its
    /// [`SpeakerStageDiagnostics`] (then its `RenderedFrame`) and handed back
    /// by [`SpatialRenderer::recycle_frame`], so a metered frame refills the
    /// previous one's allocations instead of making its own. Untouched by an
    /// unmetered frame.
    pub(super) meter_buffers: MeterBuffers,
}

/// The per-channel metering lists of one mix pass (see
/// [`SpeakerStageDiagnostics`] for what each holds).
#[derive(Default)]
pub(super) struct MeterBuffers {
    pub(super) object_gains: Vec<(usize, Gains)>,
    pub(super) object_band_gains: Vec<(usize, Vec<Gains>)>,
    pub(super) object_band_sq: Vec<(usize, Vec<f64>)>,
}

impl MeterBuffers {
    /// Take back the lists a frame was lent, keeping for each the one with
    /// the larger allocation: an unmetered frame carries empty, unallocated
    /// lists, and must not displace the pooled ones.
    pub(super) fn reclaim(&mut self, returned: MeterBuffers) {
        fn keep_larger<T>(pooled: &mut Vec<T>, returned: Vec<T>) {
            if returned.capacity() > pooled.capacity() {
                *pooled = returned;
            }
        }
        keep_larger(&mut self.object_gains, returned.object_gains);
        keep_larger(&mut self.object_band_gains, returned.object_band_gains);
        keep_larger(&mut self.object_band_sq, returned.object_band_sq);
    }
}

/// The next entry of a pooled per-channel metering list, keyed to `channel`
/// and emptied: the inner buffer an earlier frame left in that slot is reused,
/// so a list refilled with as many channels as before allocates nothing.
/// `filled` counts the entries written this frame; the caller truncates the
/// list to it once the frame is mixed.
fn pooled_entry<'a, T>(
    list: &'a mut Vec<(usize, Vec<T>)>,
    filled: &mut usize,
    channel: usize,
) -> &'a mut Vec<T> {
    if *filled == list.len() {
        list.push((channel, Vec::new()));
    }
    let entry = &mut list[*filled];
    *filled += 1;
    entry.0 = channel;
    entry.1.clear();
    &mut entry.1
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
    /// `RampMode::Sample`: samples between two gain lookups of a moving
    /// object (`LiveParams::sample_ramp_stride`).
    pub(super) sample_ramp_stride: usize,
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

/// The bus a block is mixed on: speaker-major (`planar`: one block of `block`
/// samples per speaker, back to back), over the stage's sample-major output
/// buffer (`interleaved`: one frame of `num_speakers` per sample), which is
/// where the sums start from when there is audio to add to, and where they are
/// left.
///
/// Speaker-major so that mixing a source is, per band and per speaker it
/// actually feeds, one pass over a contiguous run of samples — the shape a
/// compiler widens — instead of a walk over every speaker for every sample,
/// most of it multiplying by zero. For a given speaker and sample the additions
/// come in source order, then band order, so the sums are those of a
/// sample-major mix. A speaker whose gain is zero is skipped: `x · 0` added to
/// a sum that started at `+0.0` leaves its bits alone for any finite `x`.
///
/// Every method takes the source's band samples as one block per band
/// (`bands[b][sample]`) and its gains as one `Gains` per band; a band present
/// on one side only contributes nothing.
struct MixBus<'a> {
    planar: &'a mut [f32],
    interleaved: &'a mut [f32],
    block: usize,
    num_speakers: usize,
    /// Which buffer holds the sums so far: `planar` when true.
    in_planar: bool,
}

impl<'a> MixBus<'a> {
    /// A silent bus over a zeroed `planar`, to be left in `output`.
    fn silent(
        planar: &'a mut [f32],
        output: &'a mut [f32],
        block: usize,
        num_speakers: usize,
    ) -> Self {
        Self {
            planar,
            interleaved: output,
            block,
            num_speakers,
            in_planar: true,
        }
    }

    /// A bus that starts from what `output` holds and is left there.
    fn over(
        planar: &'a mut [f32],
        output: &'a mut [f32],
        block: usize,
        num_speakers: usize,
    ) -> Self {
        Self {
            planar,
            interleaved: output,
            block,
            num_speakers,
            in_planar: false,
        }
    }

    /// Move the sums to the speaker-major buffer if they are not there.
    /// `max(1)`: an empty bus has nothing to hand out, and a zero chunk size is
    /// not one `chunks_exact` accepts.
    #[inline(always)]
    fn make_planar(&mut self) {
        if self.in_planar {
            return;
        }
        let frames = self.interleaved.chunks_exact(self.num_speakers.max(1));
        let rows = self.planar.chunks_exact_mut(self.block.max(1));
        for (speaker_idx, row) in rows.enumerate() {
            for (sum, frame) in row.iter_mut().zip(frames.clone()) {
                *sum = frame[speaker_idx];
            }
        }
        self.in_planar = true;
    }

    /// Move the sums to the output buffer if they are not there.
    #[inline(always)]
    fn make_interleaved(&mut self) {
        if !self.in_planar {
            return;
        }
        let rows = self.planar.chunks_exact(self.block.max(1));
        for (speaker_idx, row) in rows.enumerate() {
            let frames = self.interleaved.chunks_exact_mut(self.num_speakers.max(1));
            for (&sum, frame) in row.iter().zip(frames) {
                frame[speaker_idx] = sum;
            }
        }
        self.in_planar = false;
    }

    /// The sums as one block per speaker.
    #[inline(always)]
    fn rows(&mut self) -> std::slice::ChunksExactMut<'_, f32> {
        self.make_planar();
        self.planar.chunks_exact_mut(self.block.max(1))
    }

    /// Leave the sums in the output buffer.
    fn finish(mut self) {
        self.make_interleaved();
    }

    /// Add `range` of the source with gains that hold across it.
    #[inline(always)]
    fn add_constant(&mut self, bands: &[Vec<f32>], gains: &[Gains], range: Range<usize>) {
        if range.is_empty() {
            return;
        }
        for (band, gains) in bands.iter().zip(gains) {
            let Some(band) = band.get(range.clone()) else {
                continue;
            };
            for (row, &gain) in self.rows().zip(gains.iter()) {
                if gain == 0.0 {
                    continue;
                }
                let Some(row) = row.get_mut(range.clone()) else {
                    continue;
                };
                for (sum, &sample) in row.iter_mut().zip(band) {
                    *sum += sample * gain;
                }
            }
        }
    }

    /// Add `range` of the source with gains that go linearly from `start` to
    /// `end`: sample `range.start + i` gets `start·(1 − f) + end·f` with
    /// `f = fractions[i]`.
    ///
    /// No shortcut when `start == end`: `g·(1 − f) + g·f` does not round back
    /// to `g` for every `f`. Only a speaker silent at both ends is skipped.
    #[inline(always)]
    fn add_lerp(
        &mut self,
        bands: &[Vec<f32>],
        start: &[Gains],
        end: &[Gains],
        fractions: &[f32],
        range: Range<usize>,
    ) {
        for ((band, start), end) in bands.iter().zip(start).zip(end) {
            let Some(band) = band.get(range.clone()) else {
                continue;
            };
            for ((row, &g0), &g1) in self.rows().zip(start.iter()).zip(end.iter()) {
                if g0 == 0.0 && g1 == 0.0 {
                    continue;
                }
                let Some(row) = row.get_mut(range.clone()) else {
                    continue;
                };
                for ((sum, &sample), &f) in row.iter_mut().zip(band).zip(fractions) {
                    *sum += sample * (g0 * (1.0 - f) + g1 * f);
                }
            }
        }
    }
}

/// The interpolation fractions of a block of `len` samples that reaches its
/// destination gains on the last one: `(i + 1) / len`. They depend on the
/// block length only, so the buffer is refilled when that changes and not
/// otherwise.
fn block_fractions(fractions: &mut Vec<f32>, len: usize) {
    if fractions.len() == len {
        return;
    }
    let inv_len = 1.0 / len.max(1) as f32;
    fractions.clear();
    fractions.extend((0..len).map(|i| (i as f32 + 1.0) * inv_len));
}

/// A channel's gains at the last sample of a block that ended mid-movement:
/// where the next block's first interpolation segment starts from, so the
/// gains do not step at the block boundary.
#[derive(Default)]
pub(super) struct GainCarry {
    gains: Vec<Gains>,
    /// The mix pass that left the gains. They are good for the pass right
    /// after it and for no other: a block mixed in between — settled, in
    /// another ramp mode, or not through the stage at all — ends on other gains.
    pass: u64,
}

/// `RampMode::Sample`: advance the position ramp sample by sample and mix the
/// block with gains that follow the ramped position.
///
/// While the position holds, the samples share one set of gains: `lookup` is
/// called once and the run is a pass over the block per band and per speaker.
/// An object that is not ramping — the common case, metadata is sparse — is
/// one such run: one lookup per block, at its first sample.
///
/// While the position moves, the gains are looked up at the last sample of
/// every segment of `stride` samples and interpolated linearly inside it, from
/// the gains of the sample before the segment. A segment also ends where the
/// block ends and where the movement stops, so the last sample of a ramp, and
/// every sample that follows it, has the gains of its own position. Across a
/// block boundary the starting gains come from `carry`; a movement that starts
/// in this block starts from the gains of the run it interrupts.
///
/// `stride` is the live `sample_ramp_stride`, clamped to
/// `[1, MAX_SAMPLE_RAMP_STRIDE]`; 1 is a lookup per sample. Its default, 8, is
/// 0.17 ms at 48 kHz, a span over which a ramping object moves by a small
/// fraction of a table cell: the gains along it are close to linear, and a
/// lookup per sample was eight times the work for a difference far below
/// audibility.
///
/// `band_gains` is left holding the gains of the block's last sample, which
/// are those of its position; `segment_end` is scratch.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn mix_sample_ramp<S: RampStrategy + ?Sized>(
    bus: &mut MixBus<'_>,
    bands: &[Vec<f32>],
    sample_length: usize,
    ramp: &mut crate::ramp_strategy::ChannelRampState,
    ramp_strategy: &S,
    ramp_context: &RampContext,
    stride: usize,
    carry: &mut GainCarry,
    pass: u64,
    band_gains: &mut Vec<Gains>,
    segment_end: &mut Vec<Gains>,
    mut lookup: impl FnMut([f64; 3], [f32; 3], &mut Vec<Gains>),
) {
    let stride = stride.clamp(1, MAX_SAMPLE_RAMP_STRIDE);

    // `band_gains` holds the gains of the sample before `run_start` once
    // `have_gains` is set: the previous block's last sample to begin with, if
    // that block left them.
    let mut have_gains = carry.pass.wrapping_add(1) == pass
        && carry.gains.len() == band_gains.len()
        && carry
            .gains
            .iter()
            .zip(band_gains.iter())
            .all(|(kept, slot)| kept.len() == slot.len());
    if have_gains {
        band_gains.clone_from_slice(&carry.gains);
    }
    let mut last_pos = [f64::NAN; 3];
    let mut last_size = [f32::NAN; 3];
    // First sample not mixed yet, and whether the samples since then move.
    let mut run_start = 0;
    let mut moving = false;
    // Last sample of the segment under way, while moving.
    let mut segment_last = 0;
    // Last sample on which a segment ran its full course while still moving.
    let mut moving_until = usize::MAX;

    // Mix the moving segment `range`, interpolating from the gains before it
    // (`band_gains`) to those of its last sample's position (`end`): sample
    // `i` of `len` gets the fraction `(i + 1) / len`, exactly one on the last.
    // A segment of one sample is mixed with `end`, where the interpolation
    // ends.
    let mix_segment =
        |bus: &mut MixBus<'_>, band_gains: &[Gains], end: &[Gains], range: Range<usize>| {
            let len = range.len();
            if len <= 1 {
                bus.add_constant(bands, end, range);
                return;
            }
            let mut fractions = [0.0f32; MAX_SAMPLE_RAMP_STRIDE];
            for (i, f) in fractions.iter_mut().enumerate().take(len) {
                *f = (i + 1) as f32 / len as f32;
            }
            bus.add_lerp(bands, band_gains, end, &fractions[..len], range);
        };

    for sample_idx in 0..sample_length {
        let progress = ramp.current_progress().unwrap_or(RampProgress {
            completed_units: 0,
            total_units: 0,
        });
        ramp_strategy.evaluate(ramp, progress, ramp_context);
        let position = ramp.output_position;
        let size = ramp.current_size;

        if position != last_pos || size != last_size {
            if !moving {
                if have_gains {
                    // The run so far keeps its gains; a segment starts here.
                    bus.add_constant(bands, band_gains, run_start..sample_idx);
                    moving = true;
                    run_start = sample_idx;
                    segment_last = (sample_idx + stride).min(sample_length) - 1;
                } else {
                    // Nothing to interpolate from: the block starts on the
                    // gains of its first sample's position.
                    lookup(position, size, band_gains);
                    have_gains = true;
                }
            }
            last_pos = position;
            last_size = size;
        } else if moving {
            // The movement stopped on the previous sample: the segment ends
            // there, and this sample starts a run on the gains it reached.
            lookup(last_pos, last_size, segment_end);
            mix_segment(bus, band_gains, segment_end, run_start..sample_idx);
            std::mem::swap(band_gains, segment_end);
            moving = false;
            run_start = sample_idx;
        }
        if moving && sample_idx == segment_last {
            lookup(position, size, segment_end);
            mix_segment(bus, band_gains, segment_end, run_start..sample_idx + 1);
            std::mem::swap(band_gains, segment_end);
            moving = false;
            run_start = sample_idx + 1;
            moving_until = sample_idx;
        }

        ramp.commit_output_position();
        ramp.advance_ramp(1);
    }
    bus.add_constant(bands, band_gains, run_start..sample_length);

    // The block ended mid-movement: its last gains are where the next block's
    // first segment starts from.
    if moving_until.wrapping_add(1) == sample_length {
        carry.gains.clear();
        carry.gains.extend_from_slice(band_gains);
        carry.pass = pass;
    }
}

impl SpeakerRenderStage {
    /// Fill `out` with one full-size `Gains` per render band at `position`. Uses
    /// the unified multi-band table (one cell localisation for all bands) when
    /// available, else falls back to a per-band lookup. Free-standing (borrows
    /// only the fields it needs) so it composes with the other per-channel
    /// mutable borrows held across the render arms.
    ///
    /// `cache` is the calling channel's cell cache for the unified table; a
    /// source with no channel of its own passes `None` and reads the table
    /// directly. The gains are the same either way.
    fn fill_band_gains(
        unified: &Option<MultiBandTable>,
        cache: Option<&mut CornerCache>,
        render_bands: &[BandRenderer],
        render_params: crate::ramp_strategy::RampRenderParams,
        position: [f64; 3],
        size: [f32; 3],
        out: &mut Vec<Gains>,
    ) {
        if let Some(table) = unified {
            let position = position.map(|v| v as f32);
            match cache {
                Some(cache) => table.sample_cached(cache, position, out),
                None => table.sample_into(position, out),
            }
        } else {
            out.clear();
            out.extend(
                render_bands
                    .iter()
                    .map(|b| b.compute_gains(render_params, position, size)),
            );
        }
    }

    /// Mix every input channel into `output` (interleaved,
    /// `sample_length * num_speakers` of THIS stage's layout, overwritten):
    /// direct beds to the speaker their label maps to, objects through the
    /// per-band engines (crossover split when active), per-sample gain slew.
    /// The channels are summed on the speaker-major bus and interleaved once at
    /// the end. Advances the shared `channel_states` (ramps + slew) — exactly
    /// one mix pass per `channel_states` per frame (see the module doc).
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
            sample_ramp_stride,
            ramp_strategy,
            ramp_context,
            log_object_positions,
            is_first,
            measure_breakdown,
        } = frame;
        // Per-channel gains at the final sample and per-band energies —
        // monitoring only (OSC meter bundle). Only collected when
        // `measure_breakdown` is set, into the pooled lists (refilled in place,
        // see `MeterBuffers`); the plain render path (e.g. mpv without Studio
        // open) hands out empty, unallocated lists and leaves the pool alone.
        let mut meters = if measure_breakdown {
            std::mem::take(&mut self.meter_buffers)
        } else {
            MeterBuffers::default()
        };
        meters.object_gains.clear();
        let mut band_gains_filled = 0;
        let mut band_sq_filled = 0;
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

        self.mix_pass = self.mix_pass.wrapping_add(1);

        // The bus every channel is summed on. Its speaker-major buffer is sized
        // on the first block of a length and zeroed for each.
        self.mix_bus.clear();
        self.mix_bus.resize(self.num_speakers * sample_length, 0.0);
        let mut bus = MixBus::silent(&mut self.mix_bus, output, sample_length, self.num_speakers);

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

            // Convert gain from dB to linear (−inf floor honoured).
            let gain_linear = super::components::gain_db_to_linear(gain_db);
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
                // DIRECT CHANNEL: routed to the one speaker its label resolves
                // to in the active topology.
                let speaker_idx = match active_label_to_speaker.get(&label) {
                    Some(&idx) if idx < self.num_speakers => idx,
                    _ => {
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

                // Time alignment: the FIR crossover delays every filtered
                // (object) channel by a constant latency, and a direct route
                // bypasses the filter bank — so it must be delayed by the same
                // amount or beds land early relative to objects. Zero-latency
                // engines (LR4) skip this entirely.
                let crossover_latency = self
                    .crossover_filter_bank
                    .as_ref()
                    .map_or(0, |b| b.latency_samples());
                let mut bed_delay = if crossover_latency > 0 {
                    if self.bed_delays.len() <= input_channel_idx {
                        self.bed_delays.resize_with(input_channel_idx + 1, || None);
                    }
                    let slot = &mut self.bed_delays[input_channel_idx];
                    if slot.as_ref().is_none_or(|d| d.delay() != crossover_latency) {
                        *slot = Some(IntegerDelay::new(crossover_latency));
                    }
                    slot.as_mut()
                } else {
                    None
                };

                // A bed feeds its speaker at unity and no other: one pass over
                // that speaker's block.
                if let Some(row) = bus.rows().nth(speaker_idx) {
                    for (sample_idx, out) in row.iter_mut().enumerate() {
                        let mut sample = input_pcm
                            [sample_idx * input_channel_count + input_channel_idx]
                            * (gain_start + gain_step * sample_idx as f32);
                        if let Some(delay) = bed_delay.as_mut() {
                            sample = delay.push(sample);
                        }
                        *out += sample;
                    }
                }

                if measure_breakdown {
                    let mut gains = Gains::zeroed(self.num_speakers);
                    gains.set(speaker_idx, 1.0);
                    meters.object_gains.push((input_channel_idx, gains));
                }

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
                // Always iterate over `render_bands` (1 band = no crossover, N bands =
                // the active crossover engine). Each band returns full-size Gains
                // (zeroed for out-of-band speakers), which is what tells the mix
                // which speakers a band feeds.

                // Lazily allocate per-object filter state only when crossover is active.
                let obj_filter_states: Option<&mut CrossoverStates> =
                    if let Some(fb) = self.crossover_filter_bank.as_ref() {
                        if self.crossover_filter_states.len() <= input_channel_idx {
                            self.crossover_filter_states
                                .resize_with(input_channel_idx + 1, || None);
                        }
                        Some(fb.ensure_channel_states(
                            &mut self.crossover_filter_states[input_channel_idx],
                            input_channel_idx,
                        ))
                    } else {
                        None
                    };

                let render_params = ramp_context.render_params();

                // Grown when the channel count rises, never per block.
                if self.table_caches.len() <= input_channel_idx {
                    self.table_caches
                        .resize_with(input_channel_idx + 1, CornerCache::default);
                }

                // This object's gain-slewed input sample at `sample_idx`.
                let input_at = |sample_idx: usize| {
                    input_pcm[sample_idx * input_channel_count + input_channel_idx]
                        * (gain_start + gain_step * sample_idx as f32)
                };

                // Band samples for the whole block, one buffer per band: the
                // crossover split when a bank is active (timed as one block
                // under metering), else the input as the single full band.
                let n_bands = match (self.crossover_filter_bank.as_ref(), obj_filter_states) {
                    (Some(fb), Some(states)) => {
                        let started_at = profile_crossover.then(std::time::Instant::now);
                        fb.process_block(
                            sample_length,
                            states,
                            &mut self.crossover_band_scratch,
                            input_at,
                        );
                        if let Some(started_at) = started_at {
                            crossover_elapsed += started_at.elapsed();
                        }
                        fb.num_bands().min(self.crossover_band_scratch.len())
                    }
                    _ => {
                        let band = &mut self.crossover_band_scratch[0];
                        band.clear();
                        band.extend((0..sample_length).map(input_at));
                        1
                    }
                };
                let bands = &self.crossover_band_scratch[..n_bands];

                // Reuse the per-object band-gain buffer (pooled in the renderer) so
                // the hot render path does not allocate a fresh Vec per object per
                // frame. Each arm leaves the gains of the block's last sample in
                // `band_gains` when the meters ask for them; it is put back at the
                // end.
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
                            Some(&mut self.table_caches[input_channel_idx]),
                            &self.render_bands,
                            render_params,
                            position,
                            size,
                            &mut band_gains,
                        );
                        bus.add_constant(bands, &band_gains, 0..sample_length);
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
                            Some(&mut self.table_caches[input_channel_idx]),
                            &self.render_bands,
                            render_params,
                            position,
                            size,
                            &mut band_gains,
                        );
                        bus.add_constant(bands, &band_gains, 0..sample_length);
                        state.ramp.commit_output_position();
                        state.ramp.advance_ramp(sample_length as u64);
                    }
                    RampMode::Sample => {
                        // One Gains slot per band, reused across the block (and
                        // across objects/frames via the pooled buffer).
                        band_gains
                            .resize(self.render_bands.len(), Gains::zeroed(self.num_speakers));
                        if self.gain_carries.len() <= input_channel_idx {
                            self.gain_carries
                                .resize_with(input_channel_idx + 1, GainCarry::default);
                        }
                        let unified_table = &self.unified_table;
                        let table_cache = &mut self.table_caches[input_channel_idx];
                        let render_bands = &self.render_bands;
                        let lookup = |position, size, out: &mut Vec<Gains>| {
                            Self::fill_band_gains(
                                unified_table,
                                Some(&mut *table_cache),
                                render_bands,
                                render_params,
                                position,
                                size,
                                out,
                            )
                        };
                        let carry = &mut self.gain_carries[input_channel_idx];
                        let segment_end = &mut self.segment_end_scratch;
                        // The built-in ramp is called directly, so its
                        // per-sample evaluation inlines into the loop.
                        match ramp_strategy.as_position() {
                            Some(position) => mix_sample_ramp(
                                &mut bus,
                                bands,
                                sample_length,
                                &mut state.ramp,
                                position,
                                ramp_context,
                                sample_ramp_stride,
                                carry,
                                self.mix_pass,
                                &mut band_gains,
                                segment_end,
                                lookup,
                            ),
                            None => mix_sample_ramp(
                                &mut bus,
                                bands,
                                sample_length,
                                &mut state.ramp,
                                ramp_strategy,
                                ramp_context,
                                sample_ramp_stride,
                                carry,
                                self.mix_pass,
                                &mut band_gains,
                                segment_end,
                                lookup,
                            ),
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

                        Self::fill_band_gains(
                            &self.unified_table,
                            Some(&mut self.table_caches[input_channel_idx]),
                            &self.render_bands,
                            render_params,
                            position,
                            size,
                            &mut self.interp_end_scratch,
                        );
                        let n_bands = self.interp_end_scratch.len();

                        // First block for this channel → start == end (no jump in).
                        if state.interp_prev_gains.len() != n_bands {
                            state.interp_prev_gains.clear();
                            state
                                .interp_prev_gains
                                .extend_from_slice(&self.interp_end_scratch);
                        }

                        block_fractions(&mut self.block_fractions, sample_length);
                        bus.add_lerp(
                            bands,
                            &state.interp_prev_gains,
                            &self.interp_end_scratch,
                            &self.block_fractions,
                            0..sample_length,
                        );

                        // The meters read the gains of the last sample: the
                        // interpolation evaluated there, as the mix did.
                        if measure_breakdown {
                            band_gains.resize(n_bands, Gains::zeroed(self.num_speakers));
                            if let Some(&f) = self.block_fractions.last() {
                                for ((slot, start), end) in band_gains
                                    .iter_mut()
                                    .zip(&state.interp_prev_gains)
                                    .zip(&self.interp_end_scratch)
                                {
                                    for ((gain, &g0), &g1) in
                                        slot.iter_mut().zip(start.iter()).zip(end.iter())
                                    {
                                        *gain = g0 * (1.0 - f) + g1 * f;
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
                // per-object gains, and copy the band gains out.
                if measure_breakdown {
                    let mut summed = Gains::zeroed(self.num_speakers);
                    for gains in &band_gains {
                        for (i, &g) in gains.iter().enumerate() {
                            summed[i] += g;
                        }
                    }
                    pooled_entry(
                        &mut meters.object_band_gains,
                        &mut band_gains_filled,
                        input_channel_idx,
                    )
                    .extend_from_slice(&band_gains);
                    meters.object_gains.push((input_channel_idx, summed));
                    // Per-band energy for the meters, from this object's block
                    // of band samples.
                    if profile_crossover {
                        let crossover_band_scratch = &self.crossover_band_scratch;
                        pooled_entry(
                            &mut meters.object_band_sq,
                            &mut band_sq_filled,
                            input_channel_idx,
                        )
                        .extend((0..band_gains.len()).map(|b| {
                            crossover_band_scratch
                                .get(b)
                                .and_then(|band| band.get(..sample_length))
                                .unwrap_or(&[])
                                .iter()
                                .map(|&s| (s as f64) * (s as f64))
                                .sum::<f64>()
                        }));
                    }
                }

                // Return the pooled buffer for the next object/frame.
                self.band_gains_scratch = band_gains;
            }
        }

        bus.finish();

        meters.object_band_gains.truncate(band_gains_filled);
        meters.object_band_sq.truncate(band_sq_filled);
        SpeakerStageDiagnostics {
            object_gains: meters.object_gains,
            object_band_gains: meters.object_band_gains,
            object_band_sq: meters.object_band_sq,
            crossover_elapsed,
        }
    }

    /// A stage for `layout` whose band engines, crossover bank and unified
    /// table are NOT built yet: only the delay lines and per-layout scratch
    /// are. The first [`Self::refresh_for_topology`] builds the rest.
    ///
    /// Deferred on purpose. Every host seeds the control from its config
    /// (backend, crossover engine, …) after the renderer is constructed, so
    /// bands built here would be built from the defaults and thrown away by
    /// the first frame — on a hybrid layout with crossover, the band gain
    /// tables were sampled twice at every start-up.
    pub(super) fn unbuilt(
        control: &Arc<RendererControl>,
        layout: &SpeakerLayout,
        num_speakers: usize,
        sample_rate: u32,
    ) -> Self {
        Self {
            num_speakers,
            sample_rate,
            render_bands: Vec::new(),
            built: None,
            built_topology: None,
            requested: None,
            failed: None,
            worker: BandWorker::spawn(Arc::clone(control), num_speakers, sample_rate),
            synchronous_builds: false,
            unified_table: None,
            table_caches: Vec::new(),
            crossover_filter_bank: None,
            crossover_filter_states: Vec::new(),
            bed_delays: Vec::new(),
            test_noise: crate::speaker_test::PinkNoise::default(),
            test_filter_states: None,
            speaker_freq_ranges: layout
                .speakers
                .iter()
                .map(|s| (s.freq_low, s.freq_high))
                .collect(),
            test_direct_bank: None,
            test_elapsed_samples: 0,
            test_identity: None,
            object_test_filter_states: None,
            object_test_prev_gains: Vec::new(),
            object_test_end_gains: Vec::new(),
            crossover_band_scratch: std::array::from_fn(|_| Vec::new()),
            mix_bus: Vec::new(),
            block_fractions: Vec::new(),
            gain_carries: Vec::new(),
            mix_pass: 0,
            segment_end_scratch: Vec::new(),
            band_gains_scratch: Vec::new(),
            interp_end_scratch: Vec::new(),
            speaker_gains_buf: vec![0.0f32; num_speakers],
            meter_buffers: MeterBuffers::default(),
            delay_lines: {
                let max_delay = (0.1 * sample_rate as f32) as usize; // 100 ms
                (0..num_speakers)
                    .map(|_| crate::delay_line::DelayLine::new(max_delay))
                    .collect()
            },
        }
    }

    /// [`Self::unbuilt`] in place of `previous`, at another sample rate.
    /// Everything timed in samples starts over (crossover bank, filter
    /// memory, delay lines), but the first build takes the band engines of
    /// `previous` over if the topology is still theirs: a gain table does not
    /// depend on the rate, and sampling them again is what a start-up at a
    /// rate other than the stream's would otherwise pay twice.
    pub(super) fn unbuilt_replacing(
        previous: &mut Self,
        control: &Arc<RendererControl>,
        layout: &SpeakerLayout,
        sample_rate: u32,
    ) -> Self {
        let mut stage = Self::unbuilt(control, layout, previous.num_speakers, sample_rate);
        stage.built_topology = previous.built_topology.take();
        stage.render_bands = std::mem::take(&mut previous.render_bands);
        stage
    }

    /// Keep the band engines in step with the published topology and the
    /// live crossover options. Returns whether it installed a new set.
    ///
    /// The render thread builds a set itself only when it has none yet (the
    /// first frame of a host that did not
    /// [`prepare`](SpatialRenderer::prepare_speaker_stage) the stage) or in
    /// synchronous mode (offline renders). Otherwise it asks the worker and
    /// keeps rendering the installed set until the new one lands: building a
    /// set samples a gain table per band, which must not stall the audio. A
    /// set the worker could not build is not asked again while the key stays
    /// on it; the installed one renders on.
    ///
    /// Deliberately does NOT clear the delay lines: those keep their memory
    /// across topology refreshes (only the crossover filter states reset, as
    /// their count depends on the new bank).
    pub(super) fn refresh_for_topology(
        &mut self,
        control: &Arc<RendererControl>,
        topology: &Arc<RenderTopology>,
    ) -> Result<bool> {
        let wanted = BandSetKey::wanted(control, topology);
        let mut installed = false;
        match self.worker.take_finished() {
            Some(Finished::Set(set)) => {
                // A set the topology or the options moved on from is dropped;
                // so is a duplicate of the installed one.
                if self.requested == Some(set.key) {
                    // That request is answered, whether the set is still
                    // wanted or not.
                    self.requested = None;
                }
                if set.key == wanted && self.built != Some(wanted) {
                    self.install(control, set);
                    installed = true;
                } else {
                    self.worker.retire(Box::new(set));
                }
            }
            Some(Finished::Failed(failed)) => {
                // Answered too: there will be no set for that key.
                if self.requested == Some(failed.key) {
                    self.requested = None;
                }
                self.forget_failure();
                self.failed = Some(failed);
            }
            None => {}
        }
        // A failure only holds while the key stays on it, and its set is
        // still missing: the build is asked again if the key comes back.
        if self
            .failed
            .as_ref()
            .is_some_and(|f| f.key != wanted || self.built == Some(wanted))
        {
            self.forget_failure();
        }
        if self.built == Some(wanted) {
            // Back on the installed set: a pending request for another is
            // stale, and must be asked again if the key returns to it.
            self.requested = None;
            return Ok(installed);
        }
        if self.built.is_none() || self.synchronous_builds {
            let set = Self::build_band_set(
                control,
                Arc::clone(topology),
                wanted,
                self.num_speakers,
                self.sample_rate,
                PreviousBands {
                    topology: self.built_topology.as_ref(),
                    bands: &self.render_bands,
                },
                self.filtered_channels(),
            )?;
            self.worker
                .seed(Arc::clone(topology), set.render_bands.clone());
            self.install(control, set);
            self.requested = None;
            return Ok(true);
        }
        if self.requested != Some(wanted) && self.failed.is_none() {
            if self
                .worker
                .request(Arc::clone(topology), wanted, self.filtered_channels())
            {
                self.requested = Some(wanted);
            } else {
                // The worker is gone, which only a panic outside a build can
                // do: nothing will answer, so this is a failed build too.
                self.failed = Some(FailedBuild {
                    key: wanted,
                    topology: Arc::clone(topology),
                });
                control.report_band_build_error(
                    "Speaker stage: the band worker is gone, band engines not rebuilt; \
                     the previous ones keep rendering"
                        .to_string(),
                );
            }
        }
        Ok(installed)
    }

    /// The topology the installed bands were built for: the published one,
    /// or the one before it while the worker builds the bands of a change, or
    /// for good if it could not. `None` before the first build. What derives
    /// from the layout and must agree with the gains follows this one.
    pub(super) fn installed_topology(&self) -> Option<&Arc<RenderTopology>> {
        self.built.and(self.built_topology.as_ref())
    }

    /// Whether a band set has been asked of the worker and not answered yet:
    /// neither installed nor failed.
    pub(super) fn rebuild_pending(&self) -> bool {
        self.requested.is_some()
    }

    /// Whether the worker failed to build the set the stage last wanted: the
    /// previous bands keep rendering, and the reason is on the control
    /// ([`RendererControl::take_band_build_error`]) and in the log.
    pub(super) fn rebuild_failed(&self) -> bool {
        self.failed.is_some()
    }

    /// The channels holding crossover filter memory right now: from the first
    /// that does to the last slot. A set built now allocates theirs for its
    /// own bank. Directly-routed channels come first and hold none, so this
    /// is the objects; a channel that starts being filtered later gets its
    /// memory on its first block, as before.
    fn filtered_channels(&self) -> Range<usize> {
        match self
            .crossover_filter_states
            .iter()
            .position(Option::is_some)
        {
            Some(first) => first..self.crossover_filter_states.len(),
            None => 0..0,
        }
    }

    /// Drop the remembered failure, its topology freed by the worker.
    fn forget_failure(&mut self) {
        if let Some(failed) = self.failed.take() {
            self.worker.retire(Box::new(failed));
        }
    }

    /// Swap `set` in, publish its crossover facts, reset the state tied to
    /// the bands it replaces, and hand the replaced bands to the worker to
    /// free.
    fn install(&mut self, control: &RendererControl, set: BandSet) {
        let BandSet {
            key,
            topology,
            render_bands,
            crossover_filter_bank,
            crossover_info,
            crossover_filter_states,
            unified_table,
            speaker_freq_ranges,
        } = set;
        // Here, not where the set is built: the control must name the bank
        // that renders, and a set built for a key since left is never that.
        control.set_crossover_info(crossover_info);
        self.forget_failure();
        // The filter memory goes with the bank it was made for: the replaced
        // one is freed by the worker with the rest, the new one came built.
        let replaced = (
            self.built_topology.replace(topology),
            std::mem::replace(&mut self.render_bands, render_bands),
            std::mem::replace(&mut self.crossover_filter_bank, crossover_filter_bank),
            std::mem::replace(&mut self.crossover_filter_states, crossover_filter_states),
            std::mem::replace(&mut self.unified_table, unified_table),
            std::mem::replace(&mut self.speaker_freq_ranges, speaker_freq_ranges),
        );
        self.worker.retire(Box::new(replaced));
        self.built = Some(key);
        // Gains kept for the next block are those of the bands being replaced.
        self.drop_gain_carries();
        // The table identity already rules a stale cell out; emptying the
        // caches here keeps that from being the only thing that does.
        self.table_caches
            .iter_mut()
            .for_each(CornerCache::invalidate);
        self.bed_delays.clear();
        self.test_filter_states = None;
        self.test_direct_bank = None;
        self.object_test_filter_states = None;
        self.crossover_band_scratch.iter_mut().for_each(Vec::clear);
    }

    /// Build the band set `key` describes, on whichever thread calls it.
    /// `previous` are the bands it can start from (see [`PreviousBands`]);
    /// `filtered_channels` the channels to allocate crossover filter memory
    /// for (see [`Self::filtered_channels`]).
    fn build_band_set(
        control: &Arc<RendererControl>,
        topology: Arc<RenderTopology>,
        key: BandSetKey,
        num_speakers: usize,
        sample_rate: u32,
        previous: PreviousBands<'_>,
        filtered_channels: Range<usize>,
    ) -> Result<BandSet> {
        let layout = &topology.speaker_layout;
        let same_topology = previous.topology.is_some_and(|p| Arc::ptr_eq(p, &topology));
        let (render_bands, crossover_filter_bank, crossover_info) = Self::build_crossover(
            control,
            layout,
            topology.geometry_generation,
            num_speakers,
            sample_rate,
            previous.bands,
            same_topology,
            key.crossover_type,
            key.fir_ratio,
        )?;
        let crossover_filter_states = match &crossover_filter_bank {
            Some(bank) => (0..filtered_channels.end)
                .map(|channel| {
                    filtered_channels
                        .contains(&channel)
                        .then(|| bank.make_channel_states(channel))
                })
                .collect(),
            None => Vec::new(),
        };
        let unified_table = Self::build_unified_table(&render_bands, num_speakers);
        let speaker_freq_ranges = layout
            .speakers
            .iter()
            .map(|s| (s.freq_low, s.freq_high))
            .collect();
        Ok(BandSet {
            key,
            topology,
            render_bands,
            crossover_filter_bank,
            crossover_info,
            crossover_filter_states,
            unified_table,
            speaker_freq_ranges,
        })
    }

    /// Longest a test may run without the client refreshing it. A client that
    /// dies mid-test must not leave a speaker making noise indefinitely.
    const TEST_MAX_SECONDS: u64 = 120;

    /// Write the speaker test signal into `output`, band-limited to the bands the
    /// target speaker actually reproduces.
    ///
    /// Called between the mix and `finalize_output`, so the test then goes
    /// through the same per-speaker gain, delay and mute as programme audio —
    /// the point being to hear what that speaker will actually do.
    ///
    /// "Bands the speaker reproduces" is literal rather than approximate: the
    /// crossover already computes, per band, which speakers cover it
    /// (`BandRenderer::speaker_indices`), so the noise goes through the very
    /// same LR4 bank as programme audio and only the bands listing this speaker
    /// are summed. A full-range speaker sums all of them and hears the whole
    /// signal; a sub hears only its own. A direct (non-spatialized) speaker
    /// appears in no band at all — programme audio bypasses the filter bank on
    /// its way there — but its declared `freq_low`/`freq_high` still say what
    /// it can reproduce, so the test band-limits to that range with a
    /// dedicated LR4 split, and plays unfiltered only when no range is
    /// declared.
    ///
    /// `test.level` is a **peak** target, not an RMS one: the contribution
    /// written here never exceeds `±level`, so a level ≤ 1.0 cannot clip. That
    /// takes two parts, because pink noise has no peak ceiling of its own —
    /// scaling by `1 / PinkNoise::CREST` puts the typical peak on target, and
    /// the clamp makes the ceiling exact for the rare sample beyond it (about
    /// one every 16 s; see [`crate::speaker_test::PinkNoise::CREST`]).
    ///
    /// The clamp lands *after* the band sum rather than on the raw generator
    /// output because the LR4 bank reconstructs to an allpass: it preserves RMS
    /// but shifts phase, so the summed peak can exceed the peak of the sample
    /// that fed it. Only a bound on what is actually written is a real bound.
    ///
    /// Two limits of that guarantee, both deliberate: the bound is on the test's
    /// own contribution, so `WithProgramme` can still clip against a loud mix
    /// (it is a `+=` onto programme audio); and the level is referenced at the
    /// injection point, so the per-speaker and master gain applied afterwards by
    /// `finalize_output` scale it like any other signal.
    ///
    /// Returns true while a test is running, so the caller can skip peak
    /// tracking — a loud test must not drive the auto-gain and quietly rescale
    /// the very thing being judged.
    pub(super) fn inject_speaker_test(
        &mut self,
        test: Option<crate::live_params::SpeakerTest>,
        sample_rate: u32,
        output: &mut [f32],
    ) -> bool {
        let Some(test) = test else {
            // Idle: forget the elapsed count so the next test starts fresh, and
            // drop the generator state so two tests never sound spliced.
            if self.test_identity.is_some() {
                self.test_identity = None;
                self.test_elapsed_samples = 0;
                self.test_noise.reset();
                self.test_direct_bank = None;
            }
            return false;
        };
        if test.speaker_idx >= self.num_speakers || self.num_speakers == 0 {
            return false;
        }

        // Level is part of the identity: nudging the slider restarts the clock,
        // which is what a listener adjusting by ear expects.
        let identity = (test.speaker_idx, test.level.to_bits());
        if self.test_identity != Some(identity) {
            self.test_identity = Some(identity);
            self.test_elapsed_samples = 0;
            self.test_noise.reset();
            if let Some(states) = self.test_filter_states.as_mut() {
                states.reset();
            }
            // The dedicated direct-speaker bank splits at the tested speaker's
            // own edges, so a new target invalidates it.
            self.test_direct_bank = None;
        }

        let frames = output.len() / self.num_speakers;
        let cap = Self::TEST_MAX_SECONDS * sample_rate.max(1) as u64;
        if self.test_elapsed_samples >= cap {
            return false;
        }
        let frames = frames.min((cap - self.test_elapsed_samples) as usize);
        if frames == 0 {
            return false;
        }

        match test.isolation {
            crate::live_params::TestIsolation::WithProgramme => {}
            crate::live_params::TestIsolation::TestOnly => {
                // Silence the programme on the speaker under test only: the
                // other speakers keep playing, which is what you want when
                // checking one speaker against a running mix.
                for f in 0..frames {
                    output[f * self.num_speakers + test.speaker_idx] = 0.0;
                }
            }
            crate::live_params::TestIsolation::TestOnlySoloSpeaker => {
                for f in 0..frames {
                    let base = f * self.num_speakers;
                    output[base..base + self.num_speakers].fill(0.0);
                }
            }
        }

        // Peak-referenced level: scale the unit-RMS generator so its typical
        // peak lands on `test.level`, then clamp to make that a hard ceiling.
        // Hoisted out of the loops — one divide per block, not per sample.
        let gain = test.level / crate::speaker_test::PinkNoise::CREST;
        let ceiling = test.level.abs();

        // A speaker listed in no band is a direct (non-spatialized) route —
        // band membership is computed over spatialized speakers only, so the
        // band-summing path below would sum nothing and the test would be
        // silent. Such a speaker gets the dedicated fallback further down.
        let covered = self
            .render_bands
            .iter()
            .any(|band| band.speaker_indices.contains(&test.speaker_idx));

        // Sum only the bands this speaker covers. Without a crossover there is
        // one band covering everything, so this degenerates to unfiltered noise.
        match self.crossover_filter_bank.as_ref() {
            Some(bank) if covered => {
                let states = bank.ensure_states(&mut self.test_filter_states);
                for f in 0..frames {
                    let raw = self.test_noise.next_sample();
                    let bands = bank.process_sample(raw, states);
                    let mut acc = 0.0f32;
                    for (b, band) in self.render_bands.iter().enumerate() {
                        if b < bands.len() && band.speaker_indices.contains(&test.speaker_idx) {
                            acc += bands.get(b);
                        }
                    }
                    output[f * self.num_speakers + test.speaker_idx] +=
                        (acc * gain).clamp(-ceiling, ceiling);
                }
            }
            _ => {
                // Direct (non-spatialized) speaker, or no crossover at all.
                // Programme audio reaches a direct speaker by bypassing the
                // filter bank (see the direct-channel path in `mix_channels`),
                // but the speaker's declared frequency range still says what
                // it can reproduce — so the test honours it with a dedicated
                // LR4 split at the speaker's own edges, and only falls back to
                // unfiltered noise when no range is declared. `lo < hi` guards
                // a degenerate range, whose middle band would be near-silence.
                let (lo, hi) = self
                    .speaker_freq_ranges
                    .get(test.speaker_idx)
                    .copied()
                    .unwrap_or((None, None));
                let range_valid = match (lo, hi) {
                    (Some(lo), Some(hi)) => lo < hi,
                    (None, None) => false,
                    _ => true,
                };
                if range_valid {
                    if self.test_direct_bank.is_none() {
                        // Built once per test start (identity change drops it),
                        // not in the steady-state path.
                        let edges: Vec<f32> = [lo, hi].into_iter().flatten().collect();
                        self.test_direct_bank = Some(CrossoverBank::Lr4(LR4CrossoverBank::new(
                            &edges,
                            self.sample_rate,
                        )));
                    }
                    let bank = self.test_direct_bank.as_ref().expect("just built");
                    let states = bank.ensure_states(&mut self.test_filter_states);
                    // Bands are [0, lo), [lo, hi), [hi, ∞) minus the absent
                    // edges — the speaker's own band is right after the low
                    // split when there is one.
                    let keep = usize::from(lo.is_some());
                    for f in 0..frames {
                        let raw = self.test_noise.next_sample();
                        let bands = bank.process_sample(raw, states);
                        output[f * self.num_speakers + test.speaker_idx] +=
                            (bands.get(keep) * gain).clamp(-ceiling, ceiling);
                    }
                } else {
                    for f in 0..frames {
                        let raw = self.test_noise.next_sample();
                        output[f * self.num_speakers + test.speaker_idx] +=
                            (raw * gain).clamp(-ceiling, ceiling);
                    }
                }
            }
        }

        self.test_elapsed_samples += frames as u64;
        true
    }

    /// Pan the object test's signal into `output` using the active render
    /// backend, so what is heard is what the renderer would do with a real
    /// object at that position.
    ///
    /// `noise` is the already level-scaled, peak-bounded mono block from
    /// [`crate::object_test::ObjectTestSource`]; `None` means no test is running
    /// and the interpolation state is dropped so the next start does not ramp in
    /// from a stale position. Returns true while a test is running, so the
    /// caller can suppress peak tracking exactly as it does for a speaker test.
    ///
    /// Nothing here is panning logic. The gains come from the same
    /// [`Self::fill_band_gains`] the object mix loop calls, which reaches the
    /// live backend through the band renderers — so the out-of-hull mode, the
    /// distance model and the spread settings currently configured all apply,
    /// and a contributed backend works without knowing this feature exists. The
    /// crossover split is the same too: each band is panned with that band's own
    /// gains, because a real object is.
    ///
    /// Movement is smoothed by mirroring `RampMode::Interp`: one gain evaluation
    /// per block at the requested position, then a per-sample linear blend from
    /// the previous block's destination. That is deliberately the *existing*
    /// ramp rather than a new one — it is the mechanism the renderer already
    /// trusts to move an object without clicking, and it costs one evaluation
    /// per block no matter how fast the user drags. The generator itself never
    /// restarts on a move (see `ObjectTestSource::identity_of`), so the two
    /// together give a continuous signal whose position slides.
    pub(super) fn inject_object_test(
        &mut self,
        test: Option<crate::live_params::ObjectTest>,
        block: Option<&crate::object_test::ObjectTestBlock<'_>>,
        render_params: crate::ramp_strategy::RampRenderParams,
        output: &mut [f32],
    ) -> bool {
        let (Some(test), Some(block)) = (test, block) else {
            // Drop the ramp start point: a test that starts later must begin at
            // its own position, not slide there from wherever the last one
            // ended.
            self.object_test_prev_gains.clear();
            return false;
        };
        let noise = block.pcm;
        if self.num_speakers == 0 || noise.is_empty() {
            return false;
        }
        let frames = (output.len() / self.num_speakers).min(noise.len());
        if frames == 0 {
            return false;
        }

        // Isolation. An object has no single speaker to solo, so the two
        // "test only" variants mean the same thing here: silence the programme
        // everywhere, leaving only the panned test.
        match test.isolation {
            crate::live_params::TestIsolation::WithProgramme => {}
            crate::live_params::TestIsolation::TestOnly
            | crate::live_params::TestIsolation::TestOnlySoloSpeaker => {
                for f in 0..frames {
                    let base = f * self.num_speakers;
                    output[base..base + self.num_speakers].fill(0.0);
                }
            }
        }

        // Destination gains for this block: one evaluation per band at the
        // requested position, taken/put back so `fill_band_gains` can borrow the
        // fields it needs alongside the scratch.
        let mut end = std::mem::take(&mut self.object_test_end_gains);
        Self::fill_band_gains(
            &self.unified_table,
            // The test has no input channel, hence no cell cache: one lookup
            // per block reads the table directly.
            None,
            &self.render_bands,
            render_params,
            // The orbit position, not the placed one: the source is wherever
            // this block puts it.
            block.position.map(|v| v as f64),
            test.size,
            &mut end,
        );
        self.object_test_end_gains = end;
        let n_bands = self.object_test_end_gains.len();
        if n_bands == 0 {
            return false;
        }

        // First block of this test (or a layout width change) → start == end, so
        // it begins at its position instead of sweeping in from silence.
        if self.object_test_prev_gains.len() != n_bands
            || self
                .object_test_prev_gains
                .first()
                .is_some_and(|g| g.len() != self.num_speakers)
        {
            self.object_test_prev_gains.clear();
            self.object_test_prev_gains
                .extend_from_slice(&self.object_test_end_gains);
        }

        // The test's band samples for the block, as for a real object.
        let n_bands = match self.crossover_filter_bank.as_ref() {
            Some(bank) => {
                let states = bank.ensure_states(&mut self.object_test_filter_states);
                bank.process_block(frames, states, &mut self.crossover_band_scratch, |i| {
                    noise[i]
                });
                bank.num_bands().min(self.crossover_band_scratch.len())
            }
            None => {
                let band = &mut self.crossover_band_scratch[0];
                band.clear();
                band.extend_from_slice(&noise[..frames]);
                1
            }
        };

        // The test goes onto what `output` already holds, through the same bus
        // as every other source.
        self.mix_bus.clear();
        self.mix_bus.resize(self.num_speakers * frames, 0.0);
        let mut bus = MixBus::over(&mut self.mix_bus, output, frames, self.num_speakers);
        block_fractions(&mut self.block_fractions, frames);
        bus.add_lerp(
            &self.crossover_band_scratch[..n_bands],
            &self.object_test_prev_gains,
            &self.object_test_end_gains,
            &self.block_fractions,
            0..frames,
        );
        bus.finish();

        // This block's destination is the next block's start.
        self.object_test_prev_gains.clear();
        self.object_test_prev_gains
            .extend_from_slice(&self.object_test_end_gains);
        true
    }

    /// Forget the gains the last mix pass left for the next one. For a frame
    /// that does not continue it: the stream restarted, or the frame is
    /// rendered without this stage.
    pub(super) fn drop_gain_carries(&mut self) {
        self.mix_pass = self.mix_pass.wrapping_add(1);
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
        //
        // Speaker-major, not sample-major: a sample-major walk touches all N
        // delay rings once per sample, so N × 100 ms of ring buffer is cycled
        // through the cache every block. One speaker at a time keeps a single
        // ring hot, and lifts both the bypass test and the peak comparison out
        // of the per-sample loop.
        let num_speakers = self.num_speakers;
        let sample_length = output.len() / num_speakers.max(1);
        let mut peak_sample: f32 = 0.0;
        let mut peak_speaker_idx: usize = 0;
        for (speaker_idx, delay_line) in self.delay_lines.iter_mut().enumerate().take(num_speakers)
        {
            let gain = speaker_total_gains[speaker_idx];
            let mut peak: f32 = 0.0;
            // Zero delay is the common case (no per-speaker delay configured),
            // and there `process` is the identity — but it still pays for a
            // float remainder and an interpolated read per sample.
            if delay_line.is_bypass() {
                for sample_idx in 0..sample_length {
                    let s = &mut output[sample_idx * num_speakers + speaker_idx];
                    *s *= gain;
                    delay_line.push_history(*s);
                    peak = peak.max(s.abs());
                }
            } else {
                for sample_idx in 0..sample_length {
                    let s = &mut output[sample_idx * num_speakers + speaker_idx];
                    *s = delay_line.process(*s * gain);
                    peak = peak.max(s.abs());
                }
            }
            if peak > peak_sample {
                peak_sample = peak;
                peak_speaker_idx = speaker_idx;
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
    /// Returns `(render_bands, Some(filter_bank), info)` when the layout
    /// defines finite crossover edges on at least one speaker (producing ≥ 2
    /// bands), or `(single_band, None, info)` when no crossover is needed.
    /// `render_bands` always has at least one entry. The filter engine and
    /// FIR transition ratio are the ones the set is built for
    /// ([`BandSetKey`]); `info` describes the result, for the control once
    /// the set is installed. `geometry_generation` is the one of the topology
    /// `layout` comes from, which the band gain models are built for.
    /// `prev_bands_same_topology` says `prev_bands` were built for that very
    /// topology: they are then taken over, table and all.
    #[allow(clippy::too_many_arguments)]
    fn build_crossover(
        control: &Arc<RendererControl>,
        layout: &SpeakerLayout,
        geometry_generation: u64,
        num_speakers: usize,
        sample_rate: u32,
        prev_bands: &[BandRenderer],
        prev_bands_same_topology: bool,
        crossover_type: CrossoverType,
        fir_transition_ratio: f32,
    ) -> Result<(Vec<BandRenderer>, Option<CrossoverBank>, CrossoverInfo)> {
        // For each new band, the matching previous band (same speaker subset):
        // taken over as it is when the topology is the same, else reused for
        // its triangulated gain model, which an evaluation-only refresh keeps.
        let make_renderer = |b: &FreqBand| {
            let prev = prev_bands
                .iter()
                .find(|p| p.speaker_indices == b.speaker_indices);
            match prev {
                Some(prev) if prev_bands_same_topology => Ok(prev.clone()),
                _ => BandRenderer::from_band(
                    b,
                    layout,
                    geometry_generation,
                    num_speakers,
                    control,
                    prev,
                ),
            }
        };

        let bands = compute_bands(layout);
        if bands.len() <= 1 {
            let render_bands = bands
                .iter()
                .map(make_renderer)
                .collect::<Result<Vec<_>>>()?;
            let info = CrossoverInfo {
                engine: crossover_type,
                bands: 1,
                cutoffs_hz: Vec::new(),
                taps: None,
                latency_samples: 0,
                sample_rate,
            };
            return Ok((render_bands, None, info));
        }

        let cutoffs: Vec<f32> = bands
            .windows(2)
            .map(|w| w[0].high_hz)
            .filter(|f| f.is_finite())
            .collect();

        let filter_bank = match crossover_type {
            CrossoverType::Lr4 => CrossoverBank::Lr4(LR4CrossoverBank::new(&cutoffs, sample_rate)),
            CrossoverType::Fir => CrossoverBank::Fir(FirCrossoverBank::with_spec(
                &cutoffs,
                sample_rate,
                crate::crossover::FirCrossoverSpec {
                    transition_ratio: fir_transition_ratio,
                    ..Default::default()
                },
            )),
        };
        let render_bands = bands
            .iter()
            .map(make_renderer)
            .collect::<Result<Vec<_>>>()?;

        log::info!(
            "Crossover enabled ({}): {} bands, cutoffs = {:?} Hz, latency {} samples",
            crossover_type.as_str(),
            bands.len(),
            cutoffs,
            filter_bank.latency_samples(),
        );

        let info = CrossoverInfo {
            engine: crossover_type,
            bands: bands.len(),
            cutoffs_hz: cutoffs,
            taps: match &filter_bank {
                CrossoverBank::Fir(bank) => Some(bank.taps()),
                CrossoverBank::Lr4(_) => None,
            },
            latency_samples: filter_bank.latency_samples(),
            sample_rate,
        };

        Ok((render_bands, Some(filter_bank), info))
    }

    /// Merge the per-band tables into a single multi-band table so a lookup
    /// localises the cell once for all bands, and reads the per-object corner
    /// cache. A layout without crossover gets one too, for its single band:
    /// the same bits as its evaluator
    /// (`a_single_band_renders_the_same_bits_through_the_unified_table`) at a
    /// cheaper read. Returns `None` (→ per-band path) unless every band is
    /// backed by a precomputed cartesian, or every one by a polar, table.
    fn build_unified_table(
        render_bands: &[BandRenderer],
        num_speakers: usize,
    ) -> Option<MultiBandTable> {
        if render_bands.is_empty() {
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
                    "Speaker stage: unified cartesian table built for {} band(s)",
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
                "Speaker stage: unified polar table built for {} band(s)",
                render_bands.len()
            );
        }
        table
    }
}

#[cfg(test)]
mod tests;
