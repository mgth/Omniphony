//! The two stages that synthesize objects from channel content, and the policy
//! that drives them.
//!
//! Both hosts render channel content — the mpv-embedded [`Engine`] and the
//! `orender render` CLI that serves the PipeWire sink — so both need these
//! stages. They lived inline in the engine, which is why the CLI silently had
//! neither: selecting a generator in Studio did nothing for anything played
//! through the sink, with no error to say so.
//!
//! Order matters and is fixed here rather than at each call site. The phantom
//! pre-stage subtracts correlated content from the bed *in place* and appends
//! its planar objects; the height lift then runs on the reduced bed and appends
//! its own. The renderer sees one extended interleaved buffer,
//! `bed | phantom objects | height objects`, and the object channels ride the
//! existing object/VBAP path. Both stages are zero-cost when inactive.
//!
//! The owner holds the one planar pool both stages write into and builds the
//! extended buffer once, whichever stages ran; the hosts drive it through
//! [`ChannelObjectStages::sync_from_control`], one read of the live params
//! per frame. Both stages are plugins ([`renderer::plugin`]): their parameter
//! values live in `RendererControl`'s plugin store, and reach the stages only
//! when they change or a stage is rebuilt.
//!
//! [`Engine`]: crate::engine::Engine

use bridge_api::RChannelLabel;
use renderer::backend_params::ParamValue;
use renderer::live_params::{PhantomExtractMode, RendererControl};
use renderer::placement::SourceFamily;
use renderer::plugin::{PHANTOM_EXTRACT_ID, ParamMap, PluginKind, PluginListing, PluginParams};
use renderer::spatial_renderer::SpatialChannelEvent;

use crate::object_gen::{ObjectGenStage, ObjectGeneratorFactory, PrepareCtx, SynthObjectSpec};
use crate::osc::ObjectMeta;
use crate::phantom_extract::PhantomExtractStage;

/// What the live options ask of the two stages this frame.
///
/// `synthetic_objects_enabled` is the master: with it off both stages stay
/// inactive whatever else is selected, so a host can bypass the processing
/// without losing the user's setup.
#[derive(Clone, Debug)]
pub struct StageSelection<'a> {
    pub synthetic_objects_enabled: bool,
    pub phantom_mode: PhantomExtractMode,
    pub generator_id: &'a str,
}

impl StageSelection<'_> {
    /// Whether a generator is selected at all, independent of the master —
    /// what the diagnostic state reports as the generator being "on".
    pub fn generator_selected(&self) -> bool {
        crate::object_gen::generator_selected(self.generator_id)
    }
}

/// How many objects each stage planned. Zero on both means neither runs, and
/// the bed is handed through untouched.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StageCounts {
    pub phantom: usize,
    pub synth: usize,
}

impl StageCounts {
    pub fn total(&self) -> usize {
        self.phantom + self.synth
    }

    pub fn any(&self) -> bool {
        self.total() > 0
    }
}

/// What [`ChannelObjectStages::sync_from_control`] read and planned for a
/// frame — the stage counts plus the selection behind them, for the host's
/// diagnostic state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StageSync {
    pub counts: StageCounts,
    pub synthetic_objects_enabled: bool,
    pub phantom_mode: PhantomExtractMode,
    /// A generator is selected, whatever the master says
    /// ([`StageSelection::generator_selected`]).
    pub generator_selected: bool,
    pub options_epoch: u64,
}

/// One synthesizing stage, as the owner runs it once planned: its live
/// parameters, and its per-frame DSP into the owner's planar pool.
trait ChannelObjectStage {
    fn set_param(&mut self, key: &str, value: &ParamValue, sample_rate: u32);

    /// Write this frame's object audio into `out` (one zeroed buffer of
    /// `sample_count` samples per spec). A stage may modify `bed` in place:
    /// the phantom pre-stage subtracts what it extracts.
    fn process(
        &mut self,
        bed: &mut [f32],
        channel_count: usize,
        sample_count: usize,
        sample_rate: u32,
        out: &mut [Vec<f32>],
    );
}

impl ChannelObjectStage for PhantomExtractStage {
    fn set_param(&mut self, key: &str, value: &ParamValue, sample_rate: u32) {
        PhantomExtractStage::set_param(self, key, value, sample_rate);
    }

    fn process(
        &mut self,
        bed: &mut [f32],
        channel_count: usize,
        sample_count: usize,
        _sample_rate: u32,
        out: &mut [Vec<f32>],
    ) {
        PhantomExtractStage::process(self, bed, channel_count, sample_count, out);
    }
}

impl ChannelObjectStage for ObjectGenStage {
    fn set_param(&mut self, key: &str, value: &ParamValue, sample_rate: u32) {
        ObjectGenStage::set_param(self, key, value, sample_rate);
    }

    fn process(
        &mut self,
        bed: &mut [f32],
        channel_count: usize,
        sample_count: usize,
        sample_rate: u32,
        out: &mut [Vec<f32>],
    ) {
        ObjectGenStage::process(self, bed, channel_count, sample_count, sample_rate, out);
    }
}

/// Interleave `planar` after the `channel_count` channels of `bed` into
/// `out` (resized; no allocation once warm) and return the extended width.
pub(crate) fn extend_interleaved(
    bed: &[f32],
    channel_count: usize,
    sample_count: usize,
    planar: &[Vec<f32>],
    out: &mut Vec<f32>,
) -> usize {
    let out_ch = channel_count + planar.len();
    out.clear();
    out.resize(sample_count * out_ch, 0.0);
    for s in 0..sample_count {
        let src = &bed[s * channel_count..s * channel_count + channel_count];
        let dst = &mut out[s * out_ch..s * out_ch + out_ch];
        dst[..channel_count].copy_from_slice(src);
        for (k, buf) in planar.iter().enumerate() {
            dst[channel_count + k] = buf[s];
        }
    }
    out_ch
}

/// The phantom-extraction and height-lift stages, driven as one.
pub struct ChannelObjectStages {
    phantom: PhantomExtractStage,
    object_gen: ObjectGenStage,
    /// Planar object audio for both stages, phantom objects first — one
    /// buffer per planned object, kept across frames.
    planar: Vec<Vec<f32>>,
    /// The extended interleaved buffer: `bed | phantom objects | height
    /// objects`.
    pcm_ext: Vec<f32>,
    /// What the stages were last handed their parameters for: the plugin
    /// store's generation and the generator instance. `None` until the first
    /// frame that runs a stage.
    params_applied: Option<ParamsApplied>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ParamsApplied {
    generation: u64,
    generator_builds: u64,
}

impl ChannelObjectStages {
    pub fn new() -> Self {
        Self {
            phantom: PhantomExtractStage::new(),
            object_gen: ObjectGenStage::new(),
            planar: Vec::new(),
            pcm_ext: Vec::new(),
            params_applied: None,
        }
    }

    /// Register a host-supplied (out-of-tree) generator factory.
    pub fn register_generator(&mut self, factory: Box<dyn ObjectGeneratorFactory>) {
        self.object_gen.register(factory);
    }

    /// The generator catalogue, as published to Studio.
    pub fn generator_listings(&self) -> Vec<PluginListing> {
        self.object_gen.registry().listings()
    }

    /// (Re)plan both stages for this frame and return what each will synthesize.
    ///
    /// Cheap when nothing changed: each stage compares a plan signature and
    /// only rebuilds on a real change.
    pub fn sync(
        &mut self,
        ctx: &PrepareCtx,
        selection: &StageSelection,
        options_epoch: u64,
    ) -> StageCounts {
        self.phantom.set_mode(selection.phantom_mode);
        let phantom_enabled = selection.synthetic_objects_enabled
            && selection.phantom_mode != PhantomExtractMode::Off;
        let phantom = self.phantom.sync(phantom_enabled, ctx, options_epoch);

        // The master gates the generator by selecting "none" rather than by
        // skipping the sync, so the stage tears its plan down instead of
        // leaving stale objects behind.
        let effective_id = if selection.synthetic_objects_enabled {
            selection.generator_id
        } else {
            "none"
        };
        let synth = self.object_gen.sync(effective_id, ctx, options_epoch);

        StageCounts { phantom, synth }
    }

    /// Publish the Studio state that describes this machinery rather than the
    /// stream: the generator catalogue with each generator's parameter
    /// schema, the phantom-extraction parameter schema, and the fixed-channel
    /// catalogue (every channel label with its default poses). Both hosts call
    /// it once their stages exist, and again after registering a generator.
    pub fn publish_static_state(&self, control: &RendererControl) {
        control.set_object_generator_listings(self.generator_listings());
        control.set_phantom_listing(crate::phantom_extract::phantom_listing());
        control.set_fixed_channel_catalog(crate::virtual_bed::fixed_channel_catalog_json());
    }

    /// The stage selection in the live params, with nothing planned: what an
    /// object stream reports, whose channels never reach the stages.
    pub fn selection_from_control(control: &RendererControl) -> StageSync {
        let options_epoch = control.options_epoch();
        let live = control.live.read();
        let selection = StageSelection {
            synthetic_objects_enabled: live.synthetic_objects_enabled,
            phantom_mode: live.phantom_extract_mode,
            generator_id: &live.object_generator_id,
        };
        StageSync {
            counts: StageCounts::default(),
            synthetic_objects_enabled: selection.synthetic_objects_enabled,
            phantom_mode: selection.phantom_mode,
            generator_selected: selection.generator_selected(),
            options_epoch,
        }
    }

    /// Read the stage selection off the live params (one lock-free read, nothing
    /// cloned), (re)plan both stages and hand them their parameters when those
    /// changed or the generator was rebuilt — what both hosts do on every
    /// channel frame. In steady state that is one atomic load: the plugin
    /// store is only locked when something moved.
    pub fn sync_from_control(&mut self, control: &RendererControl, ctx: &PrepareCtx) -> StageSync {
        // The epoch before the params, so a bump seen here comes with its
        // write (see `renderer::live_cell`).
        let options_epoch = control.options_epoch();
        let live = control.live.read();
        let selection = StageSelection {
            synthetic_objects_enabled: live.synthetic_objects_enabled,
            phantom_mode: live.phantom_extract_mode,
            generator_id: &live.object_generator_id,
        };
        let counts = self.sync(ctx, &selection, options_epoch);
        if counts.any() {
            let applied = ParamsApplied {
                generation: control.plugin_params_generation(),
                generator_builds: self.object_gen.builds(),
            };
            if self.params_applied != Some(applied) {
                control.with_plugin_params(|params| self.push_params(params, ctx.sample_rate));
                self.params_applied = Some(applied);
            }
        }
        StageSync {
            counts,
            synthetic_objects_enabled: selection.synthetic_objects_enabled,
            phantom_mode: selection.phantom_mode,
            generator_selected: selection.generator_selected(),
            options_epoch,
        }
    }

    /// Hand each stage its stored parameter values: the phantom stage's, and
    /// those of the generator the current plan was built for.
    ///
    /// Sparse: absent keys keep the stage default. Idempotent, so applying
    /// the same values again changes nothing.
    pub fn push_params(&mut self, params: &PluginParams, sample_rate: u32) {
        let phantom = params.plugin(PluginKind::PhantomExtract, PHANTOM_EXTRACT_ID);
        let generator = params.plugin(PluginKind::ObjectGenerator, self.object_gen.active_id());
        let stages: [(&mut dyn ChannelObjectStage, Option<&ParamMap>); 2] = [
            (&mut self.phantom, phantom),
            (&mut self.object_gen, generator),
        ];
        for (stage, values) in stages {
            for (key, value) in values.into_iter().flatten() {
                stage.set_param(key, value, sample_rate);
            }
        }
    }

    pub fn phantom_specs(&self) -> &[SynthObjectSpec] {
        self.phantom.specs()
    }

    pub fn object_specs(&self) -> &[SynthObjectSpec] {
        self.object_gen.specs()
    }

    /// Every synthesized object, in the channel order they occupy past the bed:
    /// phantom objects first, then the height objects.
    pub fn specs(&self) -> impl Iterator<Item = &SynthObjectSpec> {
        self.phantom_specs().iter().chain(self.object_specs())
    }

    /// The channel events these objects need, in the slots they occupy past the
    /// bed — `channel_count` being the bed width before extension.
    ///
    /// The slot arithmetic lives here so the two hosts cannot disagree about
    /// it: the order matches [`specs`](Self::specs), which matches the order
    /// [`process_and_extend`](Self::process_and_extend) appends the audio in.
    pub fn events(&self, channel_count: usize) -> impl Iterator<Item = SpatialChannelEvent> + '_ {
        self.specs()
            .enumerate()
            .map(move |(index, spec)| SpatialChannelEvent {
                channel_idx: channel_count + index,
                is_bed: false,
                gain_db: Some(f32::from(spec.gain_db)),
                ramp_length: Some(0),
                size: Some(spec.size),
                position: Some(spec.position),
                sample_pos: Some(0),
            })
    }

    /// The synthesized objects as OSC object metadata, for Studio's 3D view and
    /// object list.
    ///
    /// Alongside [`events`](Self::events) because the two go together: a host
    /// that emits the events renders the objects, and a host that skips these
    /// renders them without ever showing them — audible but invisible, which
    /// reads as the feature being broken.
    pub fn object_metas(&self) -> impl Iterator<Item = ObjectMeta> + '_ {
        self.specs().map(|spec| ObjectMeta {
            name: spec.name.clone(),
            x: spec.position[0] as f32,
            y: spec.position[1] as f32,
            z: spec.position[2] as f32,
            coord_mode: "cartesian".to_string(),
            direct_speaker_index: None,
            gain: f32::from(spec.gain_db),
            priority: 0.0,
            size: spec.size,
            fixed: false,
            label: String::new(),
            kind: spec.kind,
        })
    }

    /// Run both stages over the bed and return the extended interleaved buffer
    /// with its new channel count.
    ///
    /// `bed` is modified in place by the phantom pre-stage. Pass the counts from
    /// [`sync`](Self::sync) for this frame: a stage that planned nothing is
    /// skipped. The height lift reads the bed the phantom stage reduced, and
    /// both write into the one planar pool, interleaved once.
    ///
    /// The result borrows from the stages when either ran and from `bed` when
    /// neither did, so both are held for as long as it lives — the caller gets
    /// one buffer to render from and cannot disturb the bed underneath it.
    pub fn process_and_extend<'a>(
        &'a mut self,
        bed: &'a mut [f32],
        channel_count: usize,
        sample_count: usize,
        sample_rate: u32,
        counts: StageCounts,
    ) -> (&'a [f32], usize) {
        let total = counts.total();
        if total == 0 {
            return (&*bed, channel_count);
        }
        if self.planar.len() < total {
            self.planar.resize_with(total, Vec::new);
        }
        let planar = &mut self.planar[..total];
        for buf in planar.iter_mut() {
            buf.clear();
            buf.resize(sample_count, 0.0);
        }
        let (phantom_out, synth_out) = planar.split_at_mut(counts.phantom);
        let stages: [(&mut dyn ChannelObjectStage, &mut [Vec<f32>]); 2] = [
            (&mut self.phantom, phantom_out),
            (&mut self.object_gen, synth_out),
        ];
        for (stage, out) in stages {
            if !out.is_empty() {
                stage.process(bed, channel_count, sample_count, sample_rate, out);
            }
        }
        let out_ch = extend_interleaved(
            bed,
            channel_count,
            sample_count,
            &self.planar[..total],
            &mut self.pcm_ext,
        );
        (&self.pcm_ext, out_ch)
    }
}

impl Default for ChannelObjectStages {
    fn default() -> Self {
        Self::new()
    }
}

/// The stream's side of the fixed-channel processing diagnostic.
pub struct FixedProcessingReport<'a> {
    /// An object stream: its fixed channels never reach the stages.
    pub stream_has_objects: bool,
    pub family: SourceFamily,
    /// The bridge's name for the format (`FormatBridge::source_label`).
    pub source_label: &'a str,
    /// The fixed channels: the whole bed, or an object stream's prefix.
    pub labels: &'a [RChannelLabel],
    pub output_has_height: bool,
    pub stages: StageSync,
}

#[derive(Clone, PartialEq)]
struct FixedProcessingSig {
    stream_has_objects: bool,
    family: SourceFamily,
    source_label: String,
    labels: Vec<RChannelLabel>,
    output_has_height: bool,
    stages: StageSync,
}

/// The fixed-channel processing diagnostic Studio shows
/// (`RendererControl::set_fixed_channel_processing`): whether phantom
/// extraction and the height lift run on the current stream, and if not,
/// why. Published by both hosts, and rebuilt only when what it reports
/// changes — never on every frame.
#[derive(Default)]
pub struct FixedProcessingState {
    sig: Option<FixedProcessingSig>,
}

impl FixedProcessingState {
    /// The state with no stream (and forget the last one published).
    pub fn reset(&mut self, control: &RendererControl) {
        self.sig = None;
        control.set_fixed_channel_processing(
            r#"{"stream":"idle","labels":[],"phantom":"no_stream","height":"no_stream"}"#
                .to_string(),
        );
    }

    pub fn publish(&mut self, control: &RendererControl, report: &FixedProcessingReport) {
        let FixedProcessingReport {
            stream_has_objects,
            family,
            source_label,
            labels,
            output_has_height,
            stages,
        } = *report;
        let unchanged = self.sig.as_ref().is_some_and(|sig| {
            sig.stream_has_objects == stream_has_objects
                && sig.family == family
                && sig.source_label == source_label
                && sig.labels.as_slice() == labels
                && sig.output_has_height == output_has_height
                && sig.stages == stages
        });
        if unchanged {
            return;
        }
        self.sig = Some(FixedProcessingSig {
            stream_has_objects,
            family,
            source_label: source_label.to_string(),
            labels: labels.to_vec(),
            output_has_height,
            stages,
        });

        let phantom = if stages.phantom_mode == PhantomExtractMode::Off {
            "off"
        } else if !stages.synthetic_objects_enabled {
            "master_off"
        } else if stream_has_objects {
            "object_stream"
        } else if stages.counts.phantom > 0 {
            "active"
        } else {
            "insufficient_channels"
        };
        let input_has_height = crate::object_gen::input_has_height(labels);
        let height = if !stages.generator_selected {
            "off"
        } else if !stages.synthetic_objects_enabled {
            "master_off"
        } else if stream_has_objects {
            "object_stream"
        } else if input_has_height {
            "input_has_height"
        } else if !output_has_height {
            "output_has_no_height"
        } else if stages.counts.synth > 0 {
            "active"
        } else {
            "insufficient_channels"
        };
        let names: Vec<&str> = labels
            .iter()
            .map(|&label| bridge_api::labels::canonical_name(label))
            .collect();
        let family_name = control.live.read().placement.info(family).name.clone();
        let state = serde_json::json!({
            "stream": if stream_has_objects { "objects" } else { "fixed" },
            "family": family_name,
            "label": source_label,
            "labels": names,
            "inputHasHeight": input_has_height,
            "outputHasHeight": output_has_height,
            "phantom": phantom,
            "height": height,
        });
        control.set_fixed_channel_processing(state.to_string());
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn selection(master: bool, id: &str) -> StageSelection<'_> {
        StageSelection {
            synthetic_objects_enabled: master,
            phantom_mode: PhantomExtractMode::Off,
            generator_id: id,
        }
    }

    #[test]
    fn generator_selected_treats_none_and_blank_as_unselected() {
        assert!(!selection(true, "").generator_selected());
        assert!(!selection(true, "   ").generator_selected());
        assert!(!selection(true, "none").generator_selected());
        assert!(!selection(true, "NONE").generator_selected());
        assert!(selection(true, "copy_up").generator_selected());
    }

    /// The master gates the stages, not the selection: a host turning it off
    /// must not lose which generator the user picked.
    #[test]
    fn master_off_leaves_the_selection_readable() {
        let sel = selection(false, "copy_up");
        assert!(sel.generator_selected());
        assert!(!sel.synthetic_objects_enabled);
    }

    #[test]
    fn counts_report_emptiness() {
        assert!(!StageCounts::default().any());
        assert_eq!(StageCounts::default().total(), 0);
        let counts = StageCounts {
            phantom: 2,
            synth: 3,
        };
        assert!(counts.any());
        assert_eq!(counts.total(), 5);
    }

    pub(crate) fn renderer_7_1_4() -> renderer::spatial_renderer::SpatialRenderer {
        crate::renderer_build::build_spatial_renderer(
            &crate::renderer_build::SpatialRendererParams::from_render_config(None),
            renderer::speaker_layout::SpeakerLayout::preset("7.1.4").expect("preset layout"),
            48_000,
            bridge_api::RVbapCartesianDefaults {
                x_size: 9,
                y_size: 9,
                z_size: 5,
                allow_negative_z: true,
            },
            bridge_api::RVbapTableMode::Cartesian,
            None,
        )
        .expect("renderer")
    }

    /// Both stages from the live params in one call: the selection facts the
    /// hosts publish come back with the counts, and the extended buffer is
    /// `bed | phantom objects | height objects`, the lift reading the bed
    /// the phantom stage reduced.
    #[test]
    fn both_stages_share_one_extension() {
        use bridge_api::RChannelLabel::*;
        let renderer = renderer_7_1_4();
        let control = renderer.renderer_control();
        {
            let mut live = control.live.write();
            live.synthetic_objects_enabled = true;
            live.phantom_extract_mode = PhantomExtractMode::Broadband;
            live.object_generator_id = "copy_up".to_string();
        }
        let labels = [L, R, C, LFE, Ls, Rs];
        let poses = crate::virtual_bed::room_bed_poses(
            &labels,
            renderer::live_params::SurroundPlacement::Side,
        );
        let topology = control.active_topology();
        let ctx = PrepareCtx {
            input_labels: &labels,
            output_layout: &topology.speaker_layout,
            sample_rate: 48_000,
            bed_poses: &poses,
        };
        let mut stages = ChannelObjectStages::new();
        let sync = stages.sync_from_control(&control, &ctx);
        assert!(sync.synthetic_objects_enabled && sync.generator_selected);
        assert_eq!(sync.phantom_mode, PhantomExtractMode::Broadband);
        assert_eq!(sync.options_epoch, control.options_epoch());
        let counts = sync.counts;
        assert!(counts.phantom > 0 && counts.synth == 5, "{counts:?}");

        // Correlated L/C content: the phantom stage pulls part of it out of
        // the bed, so the lift must see the reduced channels.
        let (c, n) = (labels.len(), 256);
        let mut bed = vec![0.0f32; c * n];
        for s in 0..n {
            let v = (s as f32 * 0.05).sin();
            bed[s * c] = v;
            bed[s * c + 2] = v;
            bed[s * c + 4] = 0.3 * (s as f32 * 0.11).cos();
        }
        let (ext, width) = stages.process_and_extend(&mut bed, c, n, 48_000, counts);
        assert_eq!(width, c + counts.total());
        let ext = ext.to_vec();
        let lifted_sources: Vec<usize> = (0..c).filter(|&ch| labels[ch] != LFE).collect();
        for s in 0..n {
            let row = &ext[s * width..(s + 1) * width];
            assert_eq!(&row[..c], &bed[s * c..(s + 1) * c], "reduced bed first");
            for (k, &src) in lifted_sources.iter().enumerate() {
                assert_eq!(row[c + counts.phantom + k], row[src], "lift of ch {src}");
            }
        }
    }

    /// What a host publishes once its stages exist: the three catalogues
    /// Studio builds its controls from, none of them left at the empty default.
    #[test]
    fn static_state_publishes_every_catalogue() {
        let renderer = renderer_7_1_4();
        let control = renderer.renderer_control();
        ChannelObjectStages::new().publish_static_state(&control);
        for (what, json) in [
            ("generators", control.object_generators_json()),
            ("phantom", control.phantom_json()),
            ("catalogue", control.fixed_channel_catalog()),
        ] {
            let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
            assert!(
                value.as_array().is_some_and(|a| !a.is_empty())
                    || value.as_object().is_some_and(|o| !o.is_empty()),
                "{what} published empty: {json}"
            );
        }
    }

    /// The diagnostic reports why each stage does (not) run, and is rebuilt
    /// only when what it reports changes.
    #[test]
    fn fixed_processing_state_reports_and_resets() {
        use bridge_api::RChannelLabel::*;
        let renderer = renderer_7_1_4();
        let control = renderer.renderer_control();
        let dts = crate::virtual_bed::tests::test_family(&control, "dts");
        let mut state = FixedProcessingState::default();
        let stages = StageSync {
            counts: StageCounts {
                phantom: 5,
                synth: 0,
            },
            synthetic_objects_enabled: true,
            phantom_mode: PhantomExtractMode::Broadband,
            generator_selected: true,
            options_epoch: 0,
        };
        let labels = [L, R, C, LFE, Ls, Rs];
        let report = FixedProcessingReport {
            stream_has_objects: false,
            family: dts,
            source_label: "DTS-HD MA",
            labels: &labels,
            output_has_height: false,
            stages,
        };
        state.publish(&control, &report);
        let json: serde_json::Value =
            serde_json::from_str(&control.fixed_channel_processing()).expect("valid JSON");
        assert_eq!(json["stream"], "fixed");
        assert_eq!(json["family"], "dts");
        assert_eq!(json["label"], "DTS-HD MA");
        assert_eq!(json["labels"][4], "Ls");
        assert_eq!(json["phantom"], "active");
        assert_eq!(json["height"], "output_has_no_height");

        let generation = control.live_state_generation();
        state.publish(&control, &report);
        assert_eq!(
            control.live_state_generation(),
            generation,
            "unchanged → no republish"
        );

        state.reset(&control);
        let json: serde_json::Value =
            serde_json::from_str(&control.fixed_channel_processing()).expect("valid JSON");
        assert_eq!(json["stream"], "idle");
    }

    /// With nothing planned the bed must come back untouched, not copied into
    /// an extension buffer.
    #[test]
    fn inactive_stages_hand_the_bed_through() {
        let mut stages = ChannelObjectStages::new();
        let mut bed = vec![0.25f32, -0.25, 0.5, -0.5];
        let (out, channels) =
            stages.process_and_extend(&mut bed, 2, 2, 48_000, StageCounts::default());
        assert_eq!(channels, 2);
        assert_eq!(out, &[0.25f32, -0.25, 0.5, -0.5]);
    }
}
