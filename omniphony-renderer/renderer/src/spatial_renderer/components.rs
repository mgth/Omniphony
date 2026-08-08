//! Supporting value-types and free functions for the spatial render path,
//! split out of `mod.rs` to keep the core renderer file focused. These are
//! `pub(super)` (or `pub` for the renderer's public API types) so the
//! `SpatialRenderer` impl in the parent module can use them.

use crate::crossover::{BiquadState, FreqBand, LR4CrossoverBank};
use crate::live_params::{RenderTopology, RendererControl};
use crate::ramp_strategy::ChannelRampState;
use crate::render_backend::{
    CartesianEvaluationConfig, EvaluationBuildConfig, PolarEvaluationConfig, PreparedRenderEngine,
    RenderRequest,
};
use crate::spatial_vbap::{Gains, VbapTableMode};
use anyhow::Result;
use std::sync::Arc;

/// Output of a single rendered frame.
pub struct RenderedFrame {
    /// Interleaved speaker audio: `[sample0_spk0, sample0_spk1, ..., sample1_spk0, ...]`.
    pub samples: Vec<f32>,
    /// VBAP gains at the final sample for each rendered object channel.
    /// `(channel_idx, gains)` — `gains[speaker_idx]` is the gain applied to that speaker.
    /// Ordered by `channel_idx`. Empty if no objects were spatialized this frame.
    pub object_gains: Vec<(usize, Gains)>,
    /// Per-band VBAP gains for crossover objects.
    /// `(channel_idx, [band0_gains, band1_gains, ...])` — each `Gains` is full-size
    /// (`num_speakers`), indexed by global speaker index.
    /// Empty for non-crossover objects or when no crossover is active.
    pub object_band_gains: Vec<(usize, Vec<Gains>)>,
    /// Per-band sum of squared band samples over this frame for crossover
    /// objects: `(channel_idx, [band0_sum_sq, ...])`, band order matching
    /// [`Self::object_band_gains`]. Measured post object-gain — the energy the
    /// object actually contributes to the render, unlike the full-band input
    /// meter which is pre-gain. Feeds the per-band object meters; empty when
    /// metering is off or no crossover is active.
    pub object_band_sq: Vec<(usize, Vec<f64>)>,
    /// Time spent in the crossover filter-bank stage during `render_frame`.
    ///
    /// This is a subset of the total render time.
    pub crossover_time_ms: f32,
}

pub(super) fn evaluation_build_config(
    request_template: RenderRequest,
    position_interpolation: bool,
    table_mode: VbapTableMode,
    azimuth_resolution: i32,
    elevation_resolution: i32,
    distance_step: f32,
    distance_max: f32,
    allow_negative_z: bool,
) -> EvaluationBuildConfig {
    let cartesian = match table_mode {
        VbapTableMode::Cartesian {
            x_size,
            y_size,
            z_size,
            z_neg_size,
        } => CartesianEvaluationConfig {
            x_size,
            y_size,
            z_size,
            z_neg_size,
        },
        VbapTableMode::Polar => CartesianEvaluationConfig {
            x_size: 2,
            y_size: 2,
            z_size: 2,
            z_neg_size: usize::from(allow_negative_z),
        },
    };
    let azimuth_values = (360 / azimuth_resolution.max(1)).max(2) as usize;
    let elevation_span = if allow_negative_z { 180 } else { 90 };
    let elevation_values = (elevation_span / elevation_resolution.max(1)).max(2) as usize;
    let distance_values =
        ((distance_max.max(0.01) / distance_step.max(0.01)).round() as usize).max(1) + 1;
    EvaluationBuildConfig {
        request_template,
        position_interpolation,
        cartesian,
        polar: PolarEvaluationConfig {
            azimuth_values,
            elevation_values,
            distance_values,
            distance_max,
            allow_negative_z,
        },
        // Initial build uses default metrics; a configured non-default metric is
        // applied to live params post-construction, which triggers a rebuild via
        // evaluation_build_config_from_live.
        distance_model_metric: crate::spatial_vbap::DistanceMetric::default(),
        distance_diffuse_metric: crate::spatial_vbap::DistanceMetric::default(),
        // Single table on the initial build; a configured interval count is
        // applied via live params (evaluation_build_config_from_live) on rebuild.
        object_size_intervals: 0,
        object_size_mode: crate::render_backend::SizeToSpreadMode::default(),
    }
}

/// Format-agnostic spatial metadata for one audio channel (bed or object).
///
/// The renderer accepts this type instead of any format-specific event type
/// (e.g. `damf::Event`), so that any spatial audio source (ADM, …)
/// can feed the renderer without a format dependency.
///
/// All fields except `channel_idx` and `is_bed` are `Option` — `None` means
/// "keep the previously cached value for this channel".
#[derive(Debug, Clone)]
pub struct SpatialChannelEvent {
    /// PCM channel index in the interleaved input buffer.
    pub channel_idx: usize,
    /// `true` → direct speaker routing (bed); `false` → VBAP spatialization (object).
    pub is_bed: bool,
    /// Channel gain in dB (`None` = unchanged).
    pub gain_db: Option<i8>,
    /// Ramp duration in audio frames (`None` = unchanged).
    pub ramp_length: Option<u32>,
    /// Object spatial extent per axis (w, d, h), each in [0.0, 1.0]
    /// (`None` = unchanged). `[0.0, 0.0, 0.0]` denotes a point source.
    pub size: Option<[f32; 3]>,
    /// Target position in ADM Cartesian coordinates [x, y, z] (`None` = unchanged).
    /// x ∈ [-1, 1] left/right · y ∈ [-1, 1] back/front · z ∈ [-1, 1] floor/ceiling.
    pub position: Option<[f64; 3]>,
    /// Sample index within the access unit where this event takes effect.
    pub sample_pos: Option<u64>,
}

/// Per-channel state for movement detection and gain ramping
#[derive(Clone)]
pub(super) struct ChannelState {
    /// Has this channel ever received metadata?
    ///
    /// The state used to live in a `HashMap`, where *absence* carried this
    /// meaning — a missing entry read as −128 dB (silent) while a
    /// default-constructed one reads as 0 dB (unity). Moving to a
    /// preallocated `Vec` makes every channel present, so the distinction
    /// has to be explicit or every stream would start at unity instead of
    /// silent.
    pub(super) initialized: bool,

    /// Gain in dB
    pub(super) gain_db: i8,

    /// Linear gain actually applied at the end of the last rendered block.
    /// Every gain step (metadata jump, mute toggle, plan transition, stream
    /// start) is slewed toward its target at a constant rate — full scale in
    /// [`GAIN_SLEW_SECS`](super::GAIN_SLEW_SECS) — so routing/plan changes
    /// never click. Starts silent: streams fade in over the slew time.
    pub(super) slewed_gain: f32,

    pub(super) ramp: ChannelRampState,

    /// `RampMode::Interp` only: this channel's destination band gains from the
    /// previous block, reused as the start of the next block's interpolation.
    /// Empty until the first block for this channel.
    pub(super) interp_prev_gains: Vec<Gains>,
}

impl ChannelState {
    /// Advance the gain slew by one block toward `target`; returns the
    /// block's `(start, per_sample_step)` so mix loops can interpolate
    /// `start + step * sample_idx`.
    pub(super) fn slew_gain(
        &mut self,
        target: f32,
        sample_length: usize,
        ramp_samples: f32,
    ) -> (f32, f32) {
        let start = self.slewed_gain;
        let max_step = if ramp_samples > 0.0 {
            sample_length as f32 / ramp_samples
        } else {
            f32::INFINITY
        };
        let delta = (target - start).clamp(-max_step, max_step);
        self.slewed_gain = start + delta;
        let step = if sample_length > 0 {
            delta / sample_length as f32
        } else {
            0.0
        };
        (start, step)
    }
}

impl Default for ChannelState {
    fn default() -> Self {
        Self {
            initialized: false,
            gain_db: -128, // -inf dB (muted)
            slewed_gain: 0.0,
            ramp: ChannelRampState::default(),
            interp_prev_gains: Vec::new(),
        }
    }
}

/// VBAP engine for one frequency band.
///
/// Built with the same parameters as the main renderer (`table_mode`, `allow_negative_z`,
/// `position_interpolation`).  `compute_gains` always returns full-size `Gains`
/// (`num_speakers` entries) with zeros for speakers outside this band, enabling
/// uniform SIMD-friendly accumulation in the render loop.
pub(super) struct BandRenderer {
    /// Global speaker indices for the speakers in this band.
    pub(super) speaker_indices: Vec<usize>,
    /// Full speaker count — size of the returned `Gains`.
    num_speakers: usize,
    /// Band-restricted render topology (its `backend` is the prepared engine).
    /// Stored (rather than just the engine) so a geometry-unchanged refresh can
    /// reuse its gain model via `build_topology_reusing`. Always `Some` for a band
    /// with ≥1 speaker — 1–2 speakers use the degenerate-VBAP `DegenerateVbapBackend`,
    /// ≥3 the full panner, falling back to `DegenerateVbapBackend` when that set
    /// still can't be triangulated. `None` only for an empty (coverage-gap) band,
    /// which renders silence.
    topology: Option<Arc<RenderTopology>>,
    /// `true` when this band covers every speaker in identity order
    /// (`speaker_indices == 0..num_speakers`) — the no-crossover case. Then the
    /// band-local gains are already full-size and the scatter is skipped.
    is_identity: bool,
}

impl BandRenderer {
    pub(super) fn from_band(
        band: &FreqBand,
        layout: &crate::speaker_layout::SpeakerLayout,
        num_speakers: usize,
        control: &Arc<RendererControl>,
        prev: Option<&BandRenderer>,
    ) -> Result<Self> {
        let speaker_indices = band.speaker_indices.clone();
        // Log which speakers fall into this crossover band and its frequency range.
        // A speaker is in band [lo, hi) when its usable range overlaps it, so the
        // sub band [0, lo) holds every *full-range* spatializable speaker (those
        // without a `freq_low` cutoff); band-limited speakers join higher bands.
        // (LFE / non-spatializable speakers are excluded from every band.)
        let band_high = if band.high_hz.is_finite() {
            format!("{:.0}", band.high_hz)
        } else {
            "inf".to_string()
        };
        let band_speaker_names: Vec<&str> = speaker_indices
            .iter()
            .map(|&idx| layout.speakers[idx].name.as_str())
            .collect();
        if speaker_indices.is_empty() {
            log::warn!(
                "Crossover band [{:.0}, {}) Hz has no speakers — a coverage gap, content in \
                 this range is dropped",
                band.low_hz,
                band_high,
            );
        } else {
            log::info!(
                "Crossover band [{:.0}, {}) Hz: {} speakers {:?}",
                band.low_hz,
                band_high,
                speaker_indices.len(),
                band_speaker_names,
            );
        }
        let topology = if !speaker_indices.is_empty() {
            let band_layout = crate::speaker_layout::SpeakerLayout {
                radius_m: layout.radius_m,
                speakers: speaker_indices
                    .iter()
                    .map(|&idx| layout.speakers[idx].clone())
                    .collect(),
            };
            let plan = control
                .prepare_topology_rebuild_for_layout(band_layout)
                .ok_or_else(|| anyhow::anyhow!("failed to prepare band topology rebuild"))?;
            // Reuse the previous band's geometry when it covers the same speakers
            // and the geometry generation is unchanged: `build_topology_reusing`
            // then re-wraps the model (no re-triangulation).
            let prev_topology = prev
                .filter(|p| p.speaker_indices == speaker_indices)
                .and_then(|p| p.topology.as_deref());
            Some(Arc::new(plan.build_topology_reusing(prev_topology)?))
        } else {
            None
        };

        let is_identity = speaker_indices.len() == num_speakers
            && speaker_indices.iter().enumerate().all(|(i, &idx)| i == idx);

        Ok(Self {
            speaker_indices,
            num_speakers,
            topology,
            is_identity,
        })
    }

    /// The prepared engine for this band, when it has one (≥3 speakers).
    pub(super) fn engine(&self) -> Option<&Arc<PreparedRenderEngine>> {
        self.topology.as_ref().map(|t| &t.backend)
    }

    /// Compute VBAP gains for this band at `position`.
    ///
    /// Returns full-size `Gains` (length = `num_speakers`): band speakers get their
    /// VBAP gain, all other speakers get 0.
    pub(super) fn compute_gains(
        &self,
        render_params: crate::ramp_strategy::RampRenderParams,
        position: [f64; 3],
        event_size: [f32; 3],
    ) -> crate::spatial_vbap::Gains {
        let req = render_params.render_request_for_event(position, event_size);
        let n = self.speaker_indices.len();
        let band_gains = match self.engine() {
            Some(engine) => engine.compute_gains(&req).gains,
            // Only an empty (coverage-gap) band has no engine; it renders silence.
            // Bands with 1–2 (or degenerate ≥3) speakers carry a
            // `DegenerateVbapBackend` engine.
            None => crate::spatial_vbap::Gains::zeroed(n),
        };
        // No crossover: band gains are already full-size and in speaker order, so
        // return them directly instead of zeroing + scattering into a fresh Gains.
        if self.is_identity {
            return band_gains;
        }
        // Scatter band-local gains into a full-size vector.
        let mut full = crate::spatial_vbap::Gains::zeroed(self.num_speakers);
        for (gi, &g) in band_gains.iter().enumerate() {
            full.set(self.speaker_indices[gi], g);
        }
        full
    }
}

/// Split one sample into frequency bands.
///
/// When a crossover filter bank is active, runs the sample through the LR4 chain.
/// Otherwise returns a 1-band passthrough so the caller's band loop is identical.
#[inline]
pub(super) fn split_bands(
    raw: f32,
    filter_bank: &Option<LR4CrossoverBank>,
    states: Option<&mut [BiquadState]>,
) -> crate::crossover::SmallBands {
    match (filter_bank.as_ref(), states) {
        (Some(fb), Some(s)) => fb.process_sample(raw, s),
        _ => crate::crossover::SmallBands::single(raw),
    }
}
