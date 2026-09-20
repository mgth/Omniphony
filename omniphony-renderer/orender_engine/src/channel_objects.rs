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
//! [`Engine`]: crate::engine::Engine

use std::collections::HashMap;

use renderer::live_params::PhantomExtractMode;
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
        let id = self.generator_id.trim();
        !id.is_empty() && !id.eq_ignore_ascii_case("none")
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

/// The phantom-extraction and height-lift stages, driven as one.
pub struct ChannelObjectStages {
    phantom: PhantomExtractStage,
    object_gen: ObjectGenStage,
}

impl ChannelObjectStages {
    pub fn new() -> Self {
        Self {
            phantom: PhantomExtractStage::new(),
            object_gen: ObjectGenStage::new(),
        }
    }

    /// Register a host-supplied (out-of-tree) generator factory.
    pub fn register_generator(&mut self, factory: Box<dyn ObjectGeneratorFactory>) {
        self.object_gen.register(factory);
    }

    /// The generator catalogue, as published to Studio.
    pub fn generator_listings_json(&self) -> String {
        self.object_gen.registry().listings_json()
    }

    /// The phantom-extraction parameter schema, as published to Studio's
    /// sliders. Alongside the catalogue so a host publishes both from one
    /// place, or neither.
    pub fn phantom_schema_json() -> String {
        crate::phantom_extract::phantom_schema_json()
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

    /// Push each stage's live parameter overrides.
    ///
    /// Sparse: absent keys keep the stage default. Cheap and idempotent, so a
    /// freshly rebuilt stage re-receives them on the next frame.
    pub fn push_params(
        &mut self,
        phantom_params: &HashMap<String, f32>,
        generator_params: &HashMap<String, f32>,
        sample_rate: u32,
    ) {
        for (key, &value) in phantom_params.iter() {
            self.phantom.set_param(key, value, sample_rate);
        }
        for (key, &value) in generator_params.iter() {
            self.object_gen.set_param(key, value, sample_rate);
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
    /// skipped rather than asked for an empty extension.
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
        let (mid_pcm, mid_channels): (&[f32], usize) = if counts.phantom > 0 {
            self.phantom
                .process_and_extend(bed, channel_count, sample_count, sample_rate)
        } else {
            (&*bed, channel_count)
        };
        if counts.synth > 0 {
            self.object_gen
                .fill_and_extend(mid_pcm, mid_channels, sample_count, sample_rate)
        } else {
            (mid_pcm, mid_channels)
        }
    }
}

impl Default for ChannelObjectStages {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
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
