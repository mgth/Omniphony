//! Construction and topology (re)build for [`SpatialRenderer`]: the `new`
//! constructor, the live-params/crossover/unified-table builders, the
//! `finish_construction` assembler, and `refresh_crossover_for_topology` (the
//! per-frame topology-change hook). Split out of `mod.rs` to keep the render
//! hot path and the (one-time / rare) build path in separate files.
//!
//! These are `&mut self`/associated methods that do not hold the live or
//! channel-state guards, so unlike `render_frame` they extract cleanly.

use super::{BandRenderer, SpatialRenderer, evaluation_build_config};
use crate::crossover::{FreqBand, LR4CrossoverBank, compute_bands};
use crate::live_params::{
    CartesianEvaluationParams, EvaluationLiveParams, LiveEvaluationMode, LiveParams,
    PolarEvaluationParams, PreferredEvaluationMode, RampMode, RenderTopology, RendererControl,
};
use crate::render_backend::{
    DegenerateVbapBackend, EffectiveEvaluationMode, GainModel, MultiBandTable, RenderRequest,
    VbapBackend, build_prepared_render_engine,
};
use crate::spatial_vbap::{DistanceModel, VbapPanner, VbapTableMode};
use crate::speaker_layout::SpeakerLayout;
use anyhow::Result;
use std::sync::Arc;

impl SpatialRenderer {
    /// Create a new spatial renderer
    ///
    /// # Arguments
    ///
    /// * `speaker_layout` - Speaker configuration
    /// * `sample_rate` - Sample rate in Hz (for ramp timing)
    /// * `az_res_deg` - Azimuth resolution in degrees (1-10)
    /// * `el_res_deg` - Elevation resolution in degrees (1-10)
    /// * `spread_resolution` - Spread table resolution (0.0 = single table with spread=0, >0 = dynamic spread)
    /// * `distance_model` - Distance attenuation model
    /// * `spread_from_distance` - Calculate spread from distance instead of object spread metadata
    /// * `spread_distance_range` - Distance at which spread reaches 0.0
    /// * `spread_distance_curve` - Curve exponent for distance-based spread
    /// * `spread_min` - Minimum effective spread
    /// * `spread_max` - Maximum effective spread
    /// * `log_object_positions` - Enable detailed logging of object positions
    /// * `room_ratio` - Room proportions [width, length, height] for scaling ADM coordinates
    /// * `master_gain_db` - Master gain in dB (applied to final output)
    /// * `auto_gain` - Enable automatic gain reduction to prevent clipping
    /// * `use_loudness` - Apply loudness metadata correction gain from stream metadata
    /// * `distance_diffuse` - Enable distance-based antipodal diffuse blending
    /// * `distance_diffuse_threshold` - ADM distance at which blend reaches 100% direct
    /// * `distance_diffuse_curve` - Curve exponent for the blend weight
    ///
    /// **Note:** This method requires the `saf_vbap` feature to generate VBAP tables.
    /// Without saf_vbap, use `from_vbap_file()` to load pre-generated tables.
    pub fn new(
        speaker_layout: SpeakerLayout,
        sample_rate: u32,
        az_res_deg: i32,
        el_res_deg: i32,
        spread_resolution: f32,
        distance_max: f32,
        table_mode: VbapTableMode,
        allow_negative_z: bool,
        vbap_position_interpolation: bool,
        distance_model: DistanceModel,
        spread_from_distance: bool,
        spread_distance_range: f32,
        spread_distance_curve: f32,
        spread_min: f32,
        spread_max: f32,
        log_object_positions: bool,
        room_ratio: [f32; 3],
        room_ratio_rear: f32,
        room_ratio_lower: f32,
        room_ratio_center_blend: f32,
        master_gain_db: f32,
        auto_gain: bool,
        use_loudness: bool,
        distance_diffuse: bool,
        distance_diffuse_threshold: f32,
        distance_diffuse_curve: f32,
        preferred_evaluation_mode: PreferredEvaluationMode,
        initial_evaluation_mode: LiveEvaluationMode,
        cartesian_default_x_size: usize,
        cartesian_default_y_size: usize,
        cartesian_default_z_size: usize,
        cartesian_default_z_neg_size: usize,
    ) -> Result<Self> {
        let num_speakers = speaker_layout.num_speakers();
        let spatializable_positions = speaker_layout
            .spatializable_positions_for_room(
                room_ratio,
                room_ratio_rear,
                room_ratio_lower,
                room_ratio_center_blend,
            )
            .0;
        let num_vbap_speakers = spatializable_positions.len();

        let distance_step = if spread_resolution > 0.0 {
            spread_resolution
        } else {
            0.25
        };
        // Build the full triangulating panner; if the geometry is degenerate
        // (collinear/coplanar, or a speaker at the listener) and can't be
        // triangulated, degrade to the triangulation-free directional pan and warn
        // loudly rather than failing the whole engine (which would leave the host
        // with no audio at all). The warning surfaces on stderr and in Studio's log
        // panel.
        let (model, vbap_triangles): (Box<dyn GainModel>, usize) =
            match VbapPanner::new(&spatializable_positions, az_res_deg, el_res_deg, 0.0) {
                Ok(panner) => {
                    let panner = panner.with_negative_z(allow_negative_z);
                    let triangles = panner.num_triangles();
                    (
                        Box::new(VbapBackend::new(
                            panner,
                            crate::render_backend::VbapSpreadParams {
                                spread_min,
                                spread_max,
                                spread_from_distance,
                                spread_distance_range,
                                spread_distance_curve,
                                size_to_spread_mode: Default::default(),
                            },
                        )),
                        triangles,
                    )
                }
                Err(e) => {
                    let names: Vec<&str> = speaker_layout
                        .speakers
                        .iter()
                        .filter(|s| s.spatialize)
                        .map(|s| s.name.as_str())
                        .collect();
                    log::warn!(
                        "VBAP triangulation failed for {} spatializable speaker(s) {:?}: {}. \
                         Falling back to degenerate directional pan (no triangulation) — audio \
                         continues, but this layout cannot use full VBAP. Check the speaker \
                         geometry (collinear/coplanar speakers, or one placed at the listener).",
                        num_vbap_speakers,
                        names,
                        e
                    );
                    (
                        Box::new(DegenerateVbapBackend::with_omni(
                            spatializable_positions.clone(),
                            crate::backend_registry::collect_omni_mask(&speaker_layout),
                        )),
                        0,
                    )
                }
            };
        let topology = RenderTopology::new(
            Arc::new(build_prepared_render_engine(
                model,
                match table_mode {
                    VbapTableMode::Polar => EffectiveEvaluationMode::PrecomputedPolar,
                    VbapTableMode::Cartesian { .. } => {
                        EffectiveEvaluationMode::PrecomputedCartesian
                    }
                },
                &evaluation_build_config(
                    RenderRequest {
                        adm_position: [0.0, 0.0, 0.0],
                        event_size: [0.0, 0.0, 0.0],
                        room_ratio,
                        room_ratio_rear,
                        room_ratio_lower,
                        room_ratio_center_blend,
                        use_distance_diffuse: distance_diffuse,
                        distance_diffuse_threshold,
                        distance_diffuse_curve,
                        distance_model,
                    },
                    vbap_position_interpolation,
                    table_mode,
                    az_res_deg,
                    el_res_deg,
                    distance_step,
                    distance_max,
                    allow_negative_z,
                ),
            )?),
            speaker_layout,
        )?;

        log::info!(
            "Created spatial renderer: {} total speakers, {} spatializable, {} triangles, spread_res={}, table_mode={:?}, distance_model={}",
            num_speakers,
            num_vbap_speakers,
            vbap_triangles,
            spread_resolution,
            table_mode,
            distance_model
        );

        let excluded: Vec<&str> = topology
            .speaker_layout
            .speakers
            .iter()
            .filter(|s| !s.spatialize)
            .map(|s| s.name.as_str())
            .collect();
        let live_params = Self::build_live_params_and_log(
            &topology.speaker_layout,
            initial_evaluation_mode,
            az_res_deg,
            el_res_deg,
            distance_step,
            distance_max,
            allow_negative_z,
            vbap_position_interpolation,
            cartesian_default_x_size,
            cartesian_default_y_size,
            cartesian_default_z_size,
            cartesian_default_z_neg_size,
            master_gain_db,
            spread_min,
            spread_max,
            spread_from_distance,
            spread_distance_range,
            spread_distance_curve,
            RampMode::Sample,
            use_loudness,
            distance_model,
            room_ratio,
            room_ratio_rear,
            room_ratio_lower,
            room_ratio_center_blend,
            distance_diffuse,
            distance_diffuse_threshold,
            distance_diffuse_curve,
            auto_gain,
            &excluded,
            &topology.label_to_speaker,
        );
        let editable_layout = topology.speaker_layout.clone();
        let control = RendererControl::new(
            live_params,
            topology,
            editable_layout,
            Some(crate::live_params::BackendRebuildParams {
                backend_id: "vbap",
                preferred_evaluation_mode,
                allow_negative_z,
                vbap: Some(crate::live_params::VbapModelRebuildParams {
                    az_res_deg,
                    el_res_deg,
                    spread_resolution,
                    distance_max,
                    distance_model,
                    allow_negative_z,
                }),
            }),
        );

        Ok(Self::finish_construction(
            num_speakers,
            spread_resolution,
            sample_rate,
            distance_model,
            log_object_positions,
            control,
        )?)
    }

    /// Create a new spatial renderer from a pre-loaded VBAP evaluation file
    ///
    /// This uses a serialized evaluation table directly, without constructing a VBAP backend.
    /// The loaded file becomes the active evaluator, which preserves the original lookup data
    /// and keeps the file-loading path independent from backend implementations.
    ///
    /// # Arguments
    ///
    /// * `loaded_file` - Pre-loaded VBAP evaluation file
    /// * `speaker_layout` - Speaker configuration (must match the VBAP table)
    /// * `sample_rate` - Sample rate in Hz (for ramp timing)
    /// Build `LiveParams` from common constructor arguments and emit the shared log lines.
    ///
    /// Called by both `new` and `from_vbap` after each constructor has logged its own
    /// format-specific header (VBAP table size, triangle count, …).
    #[allow(clippy::too_many_arguments)]
    fn build_live_params_and_log(
        speaker_layout: &SpeakerLayout,
        initial_evaluation_mode: LiveEvaluationMode,
        az_res_deg: i32,
        el_res_deg: i32,
        distance_res: f32,
        distance_max: f32,
        allow_negative_z: bool,
        vbap_position_interpolation: bool,
        cartesian_default_x_size: usize,
        cartesian_default_y_size: usize,
        cartesian_default_z_size: usize,
        cartesian_default_z_neg_size: usize,
        master_gain_db: f32,
        spread_min: f32,
        spread_max: f32,
        spread_from_distance: bool,
        spread_distance_range: f32,
        spread_distance_curve: f32,
        ramp_mode: RampMode,
        use_loudness: bool,
        distance_model: DistanceModel,
        room_ratio: [f32; 3],
        room_ratio_rear: f32,
        room_ratio_lower: f32,
        room_ratio_center_blend: f32,
        distance_diffuse: bool,
        distance_diffuse_threshold: f32,
        distance_diffuse_curve: f32,
        auto_gain: bool,
        excluded: &[&str],
        label_to_speaker: &std::collections::HashMap<bridge_api::RChannelLabel, usize>,
    ) -> LiveParams {
        if !excluded.is_empty() {
            log::info!("Excluded from VBAP spatialization: {}", excluded.join(", "));
        }
        if spread_from_distance {
            log::warn!(
                "spread-from-distance enabled: object spread metadata will be OVERRIDDEN by \
                 distance-based spread (formula: spread = (1.0 - dist/{})^{}, clamped to [0,1])",
                spread_distance_range,
                spread_distance_curve
            );
        }
        log::info!("Spread range: [{:.2}, {:.2}]", spread_min, spread_max);
        log::info!(
            "Room ratio: width={}, length={}, height+={}",
            room_ratio[0],
            room_ratio[1],
            room_ratio[2]
        );
        log::info!("Room ratio rear (depth<0): {}", room_ratio_rear);
        log::info!("Room ratio lower (z<0): {}", room_ratio_lower);
        log::info!("Room ratio center blend: {}", room_ratio_center_blend);
        log::info!("Ramp mode: {}", ramp_mode.as_str());
        log::info!(
            "VBAP position interpolation: {}",
            if vbap_position_interpolation {
                "enabled"
            } else {
                "disabled (nearest-cell lookup)"
            }
        );
        let master_gain = 10.0_f32.powf(master_gain_db / 20.0);
        log::info!(
            "Master gain: {:.1} dB (linear: {:.4}), auto-gain: {}",
            master_gain_db,
            master_gain,
            auto_gain
        );
        log::info!("Label to speaker mapping (by name): {:?}", label_to_speaker);

        let mut speaker_live = std::collections::HashMap::new();
        for (idx, spk) in speaker_layout.speakers.iter().enumerate() {
            if spk.delay_ms != 0.0 {
                speaker_live.insert(
                    idx,
                    crate::live_params::SpeakerLiveParams {
                        delay_ms: spk.delay_ms.max(0.0),
                        ..Default::default()
                    },
                );
            }
        }

        LiveParams {
            master_gain,
            objects: std::collections::HashMap::new(),
            spread_min,
            spread_max,
            spread_from_distance,
            spread_distance_range,
            spread_distance_curve,
            size_to_spread_mode: Default::default(),
            ramp_mode,
            backend_id: "vbap".to_string(),
            evaluation: EvaluationLiveParams {
                mode: initial_evaluation_mode,
                position_interpolation: vbap_position_interpolation,
                cartesian: CartesianEvaluationParams {
                    x_size: cartesian_default_x_size.max(1),
                    y_size: cartesian_default_y_size.max(1),
                    z_size: cartesian_default_z_size.max(1),
                    z_neg_size: cartesian_default_z_neg_size,
                },
                polar: PolarEvaluationParams {
                    azimuth_values: (360.0 / az_res_deg.max(1) as f32).round() as i32,
                    elevation_values: (((if allow_negative_z { 180.0 } else { 90.0 })
                        / el_res_deg.max(1) as f32)
                        .round() as i32),
                    distance_res: (distance_max / distance_res.max(0.01)).round() as i32,
                    distance_max: distance_max.max(0.01),
                },
                object_size_intervals: 0,
            },
            use_loudness,
            auto_gain,
            auto_gain_ceiling_db: crate::config_fields::auto_gain_ceiling_db::DEFAULT,
            distance_model,
            distance_model_metric: crate::spatial_vbap::DistanceMetric::default(),
            distance_diffuse_metric: crate::spatial_vbap::DistanceMetric::default(),
            speakers: speaker_live,
            room_ratio,
            room_ratio_rear,
            room_ratio_lower,
            room_ratio_center_blend,
            dialogue_level: None,
            use_distance_diffuse: distance_diffuse,
            distance_diffuse_threshold,
            distance_diffuse_curve,
            drc_mode: "Off".to_string(),
            drc_weight: 1.0,
            hybrid: crate::live_params::HybridLiveParams::default(),
            binaural: crate::live_params::BinauralLiveParams::default(),
            // Seeded to the default (Spatial); the CLI bootstrap and the
            // embedded mpv host (`Engine::from_paths`) override it from
            // Internal host/CLI override; persistent user config is normalized
            // to the spatial policy by the option/config migration layer.
            channel_render_mode: crate::live_params::ChannelRenderMode::default(),
            // Seeded to the default (Side); the CLI bootstrap and the embedded
            // mpv host override it from `render.surround_placement`.
            surround_placement: crate::live_params::SurroundPlacement::default(),
            // Seeded to the default (ByIndex); the CLI bootstrap and the embedded
            // mpv host override it from `render.output_channel_mapping`.
            output_channel_mapping: crate::live_params::OutputChannelMapping::default(),
            // Seeded from `render.virtual_bed` by the same bootstrap; `None`
            // uses the built-in canonical poses (LFE direct, rest virtualized).
            virtual_bed: None,
            // Off by default; selects the bed→height object generator (2D upmix)
            // for channel content. Empty / "none" = disabled.
            object_generator_id: String::new(),
            // Empty = each generator uses its declared param defaults.
            object_generator_params: std::collections::HashMap::new(),
            // Renderer-synthesized objects and phantom extraction are both off
            // by default; their selections remain independent so the master can
            // temporarily bypass processing without losing setup.
            synthetic_objects_enabled: false,
            phantom_extract_mode: crate::live_params::PhantomExtractMode::Off,
            phantom_params: std::collections::HashMap::new(),
        }
    }

    /// Build crossover band engines from a speaker layout.
    ///
    /// Returns `(render_bands, Some(filter_bank))` when the layout defines finite crossover
    /// edges on at least one speaker (producing ≥ 2 bands), or `(single_band, None)` when
    /// no crossover is needed. `render_bands` always has at least one entry.
    #[allow(clippy::too_many_arguments)]
    fn build_crossover(
        control: &Arc<RendererControl>,
        layout: &crate::speaker_layout::SpeakerLayout,
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

    /// Assemble the `SpatialRenderer` struct from fully resolved components.
    ///
    /// Called by both `new` and `from_vbap` after each constructor has built its
    /// VBAP panner and `RendererControl`.
    #[allow(clippy::too_many_arguments)]
    fn finish_construction(
        num_speakers: usize,
        spread_resolution: f32,
        sample_rate: u32,
        distance_model: DistanceModel,
        log_object_positions: bool,
        control: Arc<RendererControl>,
    ) -> Result<Self> {
        let active_topology = control.active_topology();
        let topology_identity = std::sync::Arc::as_ptr(&active_topology) as usize;
        let (render_bands, crossover_filter_bank) = Self::build_crossover(
            &control,
            &active_topology.speaker_layout,
            num_speakers,
            sample_rate,
            &[],
        )?;
        let unified_table = Self::build_unified_table(&render_bands, num_speakers);

        Ok(Self {
            num_speakers,
            spread_resolution,
            channel_routing: arc_swap::ArcSwap::new(std::sync::Arc::new(Vec::new())),
            first_render: std::sync::atomic::AtomicBool::new(true),
            frame_counter: std::sync::atomic::AtomicU64::new(0),
            channel_states: parking_lot::Mutex::new(std::collections::HashMap::new()),
            sample_rate,
            distance_model,
            log_object_positions,
            loudness_gain: std::sync::atomic::AtomicU32::new(1.0_f32.to_bits()),
            auto_gain_triggered: std::sync::atomic::AtomicBool::new(false),
            control,
            speaker_gains_buf: vec![0.0f32; num_speakers],
            object_params_buf: Vec::new(),
            speaker_params_buf: vec![
                crate::live_params::SpeakerLiveParams::default();
                num_speakers
            ],
            object_params_generation_seen: 0,
            speaker_params_generation_seen: 0,
            bed_routing_gains_buf: vec![0.0f32; num_speakers],
            delay_lines: {
                let max_delay = (0.1 * sample_rate as f32) as usize; // 100 ms
                (0..num_speakers)
                    .map(|_| crate::delay_line::DelayLine::new(max_delay))
                    .collect()
            },
            ramp_strategy_override: None,
            binaural: crate::binaural::BinauralRenderer::new(sample_rate),
            binaural_pos_buf: Vec::new(),
            binaural_gain_buf: Vec::new(),
            binaural_direct_buf: Vec::new(),
            render_bands,
            unified_table,
            render_bands_topology_identity: topology_identity,
            crossover_filter_bank,
            crossover_filter_states: Vec::new(),
            crossover_band_scratch: std::array::from_fn(|_| Vec::new()),
            band_gains_scratch: Vec::new(),
            interp_end_scratch: Vec::new(),
        })
    }

    pub(super) fn refresh_crossover_for_topology(
        &mut self,
        topology_identity: usize,
        active_layout: &crate::speaker_layout::SpeakerLayout,
    ) -> Result<()> {
        if self.render_bands_topology_identity == topology_identity {
            return Ok(());
        }

        // Pass the current bands so an evaluation-only recompute (unchanged geometry
        // generation) reuses each band's triangulated gain model and rebuilds only
        // the evaluation wrapper, instead of re-triangulating every band.
        let (render_bands, crossover_filter_bank) = Self::build_crossover(
            &self.control,
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
}
