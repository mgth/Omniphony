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
use crate::live_params::RendererControl;
use crate::render_backend::MultiBandTable;
use crate::spatial_vbap::Gains;
use crate::speaker_layout::SpeakerLayout;
use anyhow::Result;
use std::sync::Arc;

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

impl SpeakerRenderStage {
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
