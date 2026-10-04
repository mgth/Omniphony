//! What a render host keeps about the stream it is rendering, and the rules
//! that keep it up to date: one type, shared by both hosts — the embedded
//! [`Engine`] (liborender, inside mpv) and the `orender render` CLI.
//!
//! Each host used to keep its own copy of these fields with its own update
//! code, and a per-stream rule honoured in one host only was a recurring bug
//! (a segment start without the content-generation bump, synthetic objects
//! rendered by one host only). The data and the rules now live here; the
//! hosts keep only their plumbing (decode thread, output buffers, sinks, OSC
//! and overlay emission) and what only one of them needs.
//!
//! Nothing here allocates on a steady stream: the event buffers and the
//! object↔channel declaration are reused, and the planners cache their plans.
//!
//! [`Engine`]: crate::engine::Engine

use std::collections::HashMap;

use bridge_api::{RChannelLabel, RChannelPose, RCoordinateFormat, RDecodedFrame, RMetadataFrame};
use renderer::live_params::RendererControl;
use renderer::placement::SourceFamily;
use renderer::spatial_renderer::{SpatialChannelEvent, SpatialRenderer};
use renderer::speaker_layout::SpeakerLayout;

use crate::channel_objects::{
    ChannelObjectStages, FixedProcessingReport, FixedProcessingState, StageCounts, StageSync,
};
use crate::decode_step::{ChannelTag, Declaration};
use crate::events::Configuration;
use crate::object_gen::{PrepareCtx, layout_has_height};
use crate::osc::{ObjectMeta, OscSender};
use crate::virtual_bed::{
    BedChannelPlanner, BedPlanKind, FixedChannelPlanner, OwnedPlacement, RoomRatios,
    build_fixed_channel_objects, build_virtual_bed_objects,
};

/// What the bridge declares about the current presentation, as the host
/// applies it: the family whose placement policy the fixed channels are
/// planned with, the poses it states for its channels, its name for the
/// format (empty when it states none) and the tags on its channels.
#[derive(Clone, Debug, PartialEq)]
pub struct StreamDeclaration {
    pub family: SourceFamily,
    pub poses: Vec<RChannelPose>,
    pub label: String,
    pub tags: Vec<ChannelTag>,
    /// The channels tagged as dialogue, from `tags`: worked out once here so
    /// the per-frame gain compares no strings.
    pub dialogue_channels: Vec<usize>,
}

impl Default for StreamDeclaration {
    /// A stream whose bridge has declared nothing yet.
    fn default() -> Self {
        Self {
            family: SourceFamily::Generic,
            poses: Vec::new(),
            label: String::new(),
            tags: Vec::new(),
            dialogue_channels: Vec::new(),
        }
    }
}

impl From<Declaration> for StreamDeclaration {
    fn from(declaration: Declaration) -> Self {
        let mut dialogue_channels: Vec<usize> = declaration
            .tags
            .iter()
            .filter(|tag| tag.kind == ChannelTag::DIALOGUE)
            .flat_map(|tag| tag.channels.iter().copied())
            .collect();
        dialogue_channels.sort_unstable();
        dialogue_channels.dedup();
        Self {
            family: SourceFamily::from_declared(&declaration.family),
            poses: declaration.poses,
            label: declaration.label,
            tags: declaration.tags,
            dialogue_channels,
        }
    }
}

/// How long a dialogue level change takes to reach the sound: the renderer's
/// own gain slew, short enough to follow a slider, long enough not to click.
const TAG_GAIN_RAMP_SECS: f32 = 0.02;

/// The gain on the channels tagged as dialogue: the live dialogue level,
/// reached over [`TAG_GAIN_RAMP_SECS`] whenever it changes. Applied to the
/// converted PCM before the upmix stages, so what phantom extraction takes
/// out of a dialogue channel carries the level too.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TagGain {
    /// The gain applied to the last sample.
    pub gain: f32,
    target: f32,
    ramp_remaining: u32,
}

impl Default for TagGain {
    fn default() -> Self {
        Self {
            gain: 1.0,
            target: 1.0,
            ramp_remaining: 0,
        }
    }
}

impl TagGain {
    /// Scale `channels` of the interleaved `pcm` toward `target_db`. Nothing
    /// to do — no tagged channel, or the level at 0 dB and settled — costs a
    /// comparison. A stream without tagged channels takes the level as it
    /// stands, so the next one that has some starts at it rather than
    /// fading in from wherever the last one left off.
    #[inline]
    pub fn apply(
        &mut self,
        pcm: &mut [f32],
        channel_count: usize,
        channels: &[usize],
        target_db: f32,
        sample_rate: u32,
    ) {
        let target = renderer::dsp::db::db_to_linear(target_db);
        if channels.is_empty() || channel_count == 0 {
            *self = Self {
                gain: target,
                target,
                ramp_remaining: 0,
            };
            return;
        }
        if target != self.target {
            self.target = target;
            self.ramp_remaining = ((sample_rate as f32 * TAG_GAIN_RAMP_SECS) as u32).max(1);
        }
        if self.ramp_remaining == 0 && self.gain == 1.0 {
            return;
        }
        // A declaration that does not fit the frame (it is read with the
        // labels, so this is a bridge bug) leaves the frame alone.
        if channels.iter().any(|&c| c >= channel_count) {
            return;
        }
        for frame in pcm.chunks_exact_mut(channel_count) {
            if self.ramp_remaining > 0 {
                self.gain += (self.target - self.gain) / self.ramp_remaining as f32;
                self.ramp_remaining -= 1;
            } else {
                self.gain = self.target;
            }
            for &c in channels {
                frame[c] *= self.gain;
            }
        }
    }
}

/// The DRC gain ramp: the gain applied to the decoded PCM, moving toward the
/// stream's DRC word over the ramp the bridge states. Continues across frames
/// and segments; only a new stream (or a seek) starts it over.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrcRamp {
    /// The gain applied to the last sample converted.
    pub gain: f32,
    pub target_gain: f32,
    pub ramp_samples_remaining: u32,
}

impl Default for DrcRamp {
    fn default() -> Self {
        Self {
            gain: 1.0,
            target_gain: 1.0,
            ramp_samples_remaining: 0,
        }
    }
}

impl DrcRamp {
    /// Convert `frame`'s PCM to `f32` into `out`, applying the DRC gain: the
    /// frame's DRC word weighted by the live DRC weight becomes the target,
    /// reached over the frame's ramp.
    #[inline]
    pub fn fill_pcm_f32(
        &mut self,
        out: &mut Vec<f32>,
        frame: &RDecodedFrame,
        control: &RendererControl,
    ) {
        let weight = control.live.read().drc_weight.clamp(0.0, 1.0);
        self.target_gain = if weight >= 1.0 {
            frame.drc_gain
        } else if weight <= 0.0 {
            1.0
        } else {
            frame.drc_gain.powf(weight)
        };
        self.ramp_samples_remaining = frame.drc_ramp_duration;
        crate::render::fill_pcm_f32_drc(
            out,
            &frame.pcm,
            frame.channel_count as usize,
            &mut self.gain,
            self.target_gain,
            &mut self.ramp_samples_remaining,
        );
    }
}

/// The per-stream state of a render host. See the [module docs](self).
pub struct StreamState {
    /// The bridge's declaration for the current labels: applied from the frame
    /// it came with, kept until the next one (segment starts included — a
    /// segment start or a label change comes with a new one).
    pub declaration: StreamDeclaration,
    /// How the bridge states object positions. A property of the bridge, set
    /// once it is loaded.
    pub coordinate_format: RCoordinateFormat,
    /// The stream carries objects: set by the first metadata frame, cleared by
    /// a segment start.
    pub has_objects: bool,
    /// The object stream's fixed prefix: routing + plan cache.
    pub fixed_planner: FixedChannelPlanner,
    /// Channel content: caches the channel mapping, so a steady stream replans
    /// only when the labels or the placement params actually change.
    pub bed_planner: BedChannelPlanner,
    /// The bridge's sparse object↔channel declaration, sorted by channel.
    /// See `docs/channel-object-contract.md`.
    pub object_channels: Vec<(u32, usize)>,
    /// Where a metadata frame's declaration is sorted before it is compared
    /// with the cached one, so the comparison does not allocate.
    object_channels_scratch: Vec<(u32, usize)>,
    /// Object names (id → name), accumulated from the sparse name updates
    /// whether or not anyone is listening: a Studio attaching mid-stream would
    /// otherwise show unnamed objects until the next declaration, which for
    /// most content never comes.
    pub object_names: HashMap<u32, String>,
    /// An object frame's events: the fixed prefix's plan events, then the
    /// metadata's. Filled by [`plan_object_frame`](Self::plan_object_frame);
    /// the host renders and clears it.
    pub frame_events: Vec<SpatialChannelEvent>,
    /// A channel frame's events: the bed plan's, then the synthesized
    /// objects'. Separate from `frame_events`, which an object frame extends
    /// without clearing: sharing one buffer would leak a channel frame's events
    /// into the next object frame.
    pub bed_events: Vec<SpatialChannelEvent>,
    /// Phantom extraction + bed→height lift, synthesizing objects from
    /// channel content.
    pub channel_objects: ChannelObjectStages,
    /// The fixed-channel processing diagnostic Studio shows.
    pub fixed_processing: FixedProcessingState,
    /// The dialogue normalisation level (dBFS, ≤ 0) applied to the renderer
    /// for this segment, once a frame has declared it: latched, then released
    /// by a segment start so the next segment's level applies.
    pub dialnorm: Option<i8>,
    /// The DRC gain ramp.
    pub drc: DrcRamp,
    /// The dialogue level on the tagged channels.
    pub dialogue: TagGain,
    /// The tags last published to the control, so they are published (and
    /// allocated) only when they change.
    published_tags: Option<Vec<ChannelTag>>,
}

impl Default for StreamState {
    fn default() -> Self {
        Self::new(RCoordinateFormat::Cartesian)
    }
}

impl StreamState {
    pub fn new(coordinate_format: RCoordinateFormat) -> Self {
        Self {
            declaration: StreamDeclaration::default(),
            coordinate_format,
            has_objects: false,
            fixed_planner: FixedChannelPlanner::new(),
            bed_planner: BedChannelPlanner::new(),
            object_channels: Vec::new(),
            object_channels_scratch: Vec::new(),
            object_names: HashMap::new(),
            frame_events: Vec::new(),
            bed_events: Vec::new(),
            channel_objects: ChannelObjectStages::new(),
            fixed_processing: FixedProcessingState::default(),
            dialnorm: None,
            drc: DrcRamp::default(),
            dialogue: TagGain::default(),
            published_tags: None,
        }
    }

    /// Convert `frame`'s PCM to `f32` into `out`: the DRC gain ramp, then the
    /// dialogue level on the channels the declaration tags as dialogue. Also
    /// publishes the declaration's tags to `control` when they changed.
    #[inline]
    pub fn fill_pcm_f32(
        &mut self,
        out: &mut Vec<f32>,
        frame: &RDecodedFrame,
        control: &RendererControl,
    ) {
        let dialogue_db = control.live.read().dialogue_gain_db;
        self.drc.fill_pcm_f32(out, frame, control);
        self.dialogue.apply(
            out,
            frame.channel_count as usize,
            &self.declaration.dialogue_channels,
            dialogue_db,
            frame.sampling_frequency,
        );
        self.publish_tags(control);
    }

    /// Tell `control` (and through it Studio) what the stream tags, when it
    /// changed since the last time.
    fn publish_tags(&mut self, control: &RendererControl) {
        if self.published_tags.as_ref() == Some(&self.declaration.tags) {
            return;
        }
        let tags = self.declaration.tags.clone();
        let json: Vec<serde_json::Value> = tags
            .iter()
            .map(|tag| {
                serde_json::json!({
                    "kind": tag.kind,
                    "language": tag.language,
                    "label": tag.label,
                    "channels": tag.channels,
                })
            })
            .collect();
        control.set_channel_tags(serde_json::Value::Array(json).to_string());
        self.published_tags = Some(tags);
    }

    /// Take on the bridge's declaration for this frame and the ones after it.
    pub fn apply_declaration(&mut self, declaration: Declaration) {
        self.declaration = declaration.into();
    }

    /// A segment starts (the bridge's `is_new_segment`, or it reset itself):
    /// the renderer's per-object and ramp state goes, OSC clients are told the
    /// content changed so they purge the previous layout's objects, and the
    /// per-segment state starts over ([`reset_segment`](Self::reset_segment)).
    pub fn begin_segment(&mut self, renderer: &SpatialRenderer, osc: Option<&mut OscSender>) {
        crate::spatial::begin_segment(renderer, osc);
        self.reset_segment(Some(&renderer.renderer_control()));
    }

    /// Drop what a segment start invalidates: the object flag, the plans, the
    /// object declarations and names, the pending events and the dialogue
    /// normalisation latch; the fixed-channel diagnostic goes back to idle on
    /// `control`, when there is a renderer.
    ///
    /// Not the bridge's declaration: a segment start comes with a fresh one,
    /// applied to the frame right after this. Not the DRC ramp either, which
    /// runs across segments.
    pub fn reset_segment(&mut self, control: Option<&RendererControl>) {
        self.has_objects = false;
        self.fixed_planner.reset();
        self.bed_planner.reset();
        self.object_channels.clear();
        self.object_names.clear();
        self.frame_events.clear();
        self.bed_events.clear();
        self.dialnorm = None;
        if let Some(control) = control {
            self.fixed_processing.reset(control);
        }
    }

    /// Apply the frame's dialogue normalisation level to the renderer, once
    /// per segment. True when it was applied by this call, for the host to
    /// publish the loudness state.
    pub fn latch_dialnorm(&mut self, frame: &RDecodedFrame, renderer: &SpatialRenderer) -> bool {
        if self.dialnorm.is_some() {
            return false;
        }
        let Some(level) = frame.dialogue_level.into_option() else {
            return false;
        };
        renderer.set_loudness(level);
        self.dialnorm = Some(level);
        true
    }

    /// A metadata frame arrived: the stream carries objects, and the frame may
    /// update the object↔channel declaration and the object names.
    pub fn note_object_metadata(&mut self, meta: &RMetadataFrame) {
        self.has_objects = true;
        if !meta.object_channels.is_empty() {
            let declared = &mut self.object_channels_scratch;
            declared.clear();
            declared.extend(
                meta.object_channels
                    .iter()
                    .map(|oc| (oc.id, oc.channel as usize)),
            );
            declared.sort_unstable_by_key(|&(_, channel)| channel);
            if self.object_channels != *declared {
                std::mem::swap(&mut self.object_channels, declared);
            }
        }
        for update in meta.name_updates.iter() {
            if self.object_names.get(&update.id).map(String::as_str) != Some(update.name.as_str()) {
                self.object_names.insert(update.id, update.name.to_string());
            }
        }
    }

    /// Plan one metadata frame of an object stream into
    /// [`frame_events`](Self::frame_events): the fixed prefix through the
    /// shared channel planner (virtualized by default, per-entry direct opt-in,
    /// exactly as channel content; cached until an input changes), then the
    /// objects and the stream's channel gains.
    pub fn plan_object_frame(
        &mut self,
        labels: &[RChannelLabel],
        meta: &RMetadataFrame,
        conf: &Configuration,
        renderer: &SpatialRenderer,
    ) {
        self.fixed_planner.plan_object_stream_fixed(
            labels,
            self.declaration.family,
            &self.declaration.poses,
            renderer,
            &mut self.frame_events,
        );
        crate::spatial::build_spatial_channel_events(
            conf,
            self.coordinate_format,
            &self.object_channels,
            &meta.channel_gains,
            self.fixed_planner.fixed_trims(),
            meta.sample_pos,
            meta.ramp_duration,
            &mut self.frame_events,
        );
    }

    /// The objects one metadata frame displays (OSC, overlay): the fixed
    /// prefix as planned, when there is a renderer, then the dynamic objects,
    /// named.
    pub fn object_frame_metas(
        &self,
        renderer: Option<&SpatialRenderer>,
        conf: &Configuration,
    ) -> Vec<ObjectMeta> {
        let mut objects = renderer
            .and_then(|renderer| {
                build_fixed_channel_objects(
                    renderer,
                    self.fixed_planner.fixed_labels(),
                    self.declaration.family,
                    &self.declaration.poses,
                )
            })
            .unwrap_or_default();
        objects.extend(crate::spatial::build_object_metas(
            conf,
            self.coordinate_format,
            &self.object_names,
        ));
        objects
    }

    /// The coordinate format as the OSC object frame encodes it.
    pub fn osc_coordinate_format(&self) -> i32 {
        match self.coordinate_format {
            RCoordinateFormat::Cartesian => 0,
            RCoordinateFormat::Polar => 1,
        }
    }

    /// Publish the fixed-channel diagnostic for an object stream: its fixed
    /// prefix, which never reaches the channel-object stages.
    pub fn publish_object_stream_processing(
        &mut self,
        control: &RendererControl,
        labels: &[RChannelLabel],
    ) {
        let fixed_end = labels
            .iter()
            .position(|label| *label == RChannelLabel::Object)
            .unwrap_or(labels.len());
        self.fixed_processing.publish(
            control,
            &FixedProcessingReport {
                stream_has_objects: true,
                family: self.declaration.family,
                source_label: &self.declaration.label,
                labels: &labels[..fixed_end],
                output_has_height: layout_has_height(&control.active_topology().speaker_layout),
                stages: ChannelObjectStages::selection_from_control(control),
            },
        );
    }

    /// Plan a channel frame. On [`BedPlanKind::Events`],
    /// [`bed_events`](Self::bed_events) holds the plan's events (direct
    /// channels routed one-hot by label, virtual ones as VBAP objects; the
    /// routing is applied to the renderer on change only), for
    /// [`sync_channel_objects`](Self::sync_channel_objects) to extend.
    pub fn plan_bed(
        &mut self,
        renderer: &SpatialRenderer,
        labels: &[RChannelLabel],
    ) -> BedPlanKind {
        let kind = self.bed_planner.plan(
            renderer,
            labels,
            self.declaration.family,
            &self.declaration.poses,
        );
        if kind == BedPlanKind::Events {
            self.bed_events.clear();
            self.bed_events.extend_from_slice(self.bed_planner.events());
        }
        kind
    }

    /// After [`plan_bed`](Self::plan_bed): plan the objects synthesized from
    /// the bed (phantom extraction, then the bed→height lift), placed from the
    /// poses the bed plan resolved; publish the fixed-channel diagnostic; and
    /// append their events to [`bed_events`](Self::bed_events) — phantom
    /// objects in the channels right after the bed's `channel_count`, the
    /// height objects past them. Their audio comes from
    /// [`ChannelObjectStages::process_and_extend`] once the PCM exists.
    pub fn sync_channel_objects(
        &mut self,
        control: &RendererControl,
        labels: &[RChannelLabel],
        channel_count: usize,
        output_layout: &SpeakerLayout,
        sample_rate: u32,
    ) -> StageCounts {
        let ctx = PrepareCtx {
            input_labels: labels,
            output_layout,
            sample_rate,
            bed_poses: self.bed_planner.poses(),
        };
        let sync: StageSync = self.channel_objects.sync_from_control(control, &ctx);
        let counts = sync.counts;
        self.fixed_processing.publish(
            control,
            &FixedProcessingReport {
                stream_has_objects: false,
                family: self.declaration.family,
                source_label: &self.declaration.label,
                labels,
                output_has_height: layout_has_height(output_layout),
                stages: sync,
            },
        );
        self.bed_events
            .extend(self.channel_objects.events(channel_count));
        counts
    }

    /// The objects a channel frame displays (OSC, overlay): the bed's channels
    /// where the virtual bed places them, then the synthesized objects. Only
    /// the display path needs the bed layout and the room ratios, so they are
    /// read (and the layout copied) here, never on the render path.
    pub fn bed_frame_metas(
        &self,
        control: &RendererControl,
        labels: &[RChannelLabel],
        output_layout: &SpeakerLayout,
    ) -> Vec<ObjectMeta> {
        let (placement, room, surround_placement) = {
            let live = control.live.read();
            (
                OwnedPlacement::from_live(&live, self.declaration.family),
                RoomRatios::from_live(&live),
                live.surround_placement,
            )
        };
        let mut objects = build_virtual_bed_objects(
            labels,
            &placement.policy(&self.declaration.poses),
            Some(output_layout),
            room,
            surround_placement,
        )
        .unwrap_or_default();
        objects.extend(self.channel_objects.object_metas());
        objects
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use abi_stable::std_types::{RString, RVec};
    use bridge_api::{RNameUpdate, RObjectChannel};

    fn meta(channels: &[(u32, u32)], names: &[(u32, &str)]) -> RMetadataFrame {
        RMetadataFrame {
            object_channels: channels
                .iter()
                .map(|&(id, channel)| RObjectChannel { id, channel })
                .collect(),
            name_updates: names
                .iter()
                .map(|&(id, name)| RNameUpdate {
                    id,
                    name: RString::from(name),
                })
                .collect(),
            events: RVec::new(),
            channel_gains: RVec::new(),
            sample_pos: 0,
            ramp_duration: 0,
        }
    }

    /// The declaration is cached sorted by channel, a frame without one keeps
    /// it, and names accumulate.
    #[test]
    fn metadata_updates_the_object_declaration_and_names() {
        let mut stream = StreamState::default();
        stream.note_object_metadata(&meta(&[(7, 3), (5, 1)], &[(5, "Dialog")]));
        assert!(stream.has_objects);
        assert_eq!(stream.object_channels, [(5, 1), (7, 3)]);
        stream.note_object_metadata(&meta(&[], &[(7, "Fx")]));
        assert_eq!(stream.object_channels, [(5, 1), (7, 3)]);
        assert_eq!(stream.object_names.len(), 2);
        stream.note_object_metadata(&meta(&[(9, 2)], &[]));
        assert_eq!(stream.object_channels, [(9, 2)]);
    }

    /// A segment start drops the per-segment state but keeps the declaration
    /// and the DRC ramp, which run across segments.
    #[test]
    fn a_segment_reset_keeps_the_declaration_and_the_drc_ramp() {
        let mut stream = StreamState::default();
        stream.apply_declaration(Declaration {
            poses: Vec::new(),
            family: "dts".to_owned(),
            label: "DTS".to_owned(),
            tags: Vec::new(),
        });
        stream.note_object_metadata(&meta(&[(1, 0)], &[(1, "A")]));
        stream.dialnorm = Some(-27);
        stream.drc.gain = 0.5;
        stream.reset_segment(None);
        assert!(!stream.has_objects);
        assert!(stream.object_channels.is_empty());
        assert!(stream.object_names.is_empty());
        assert_eq!(stream.dialnorm, None);
        assert_eq!(stream.declaration.family, SourceFamily::Dts);
        assert_eq!(stream.declaration.label, "DTS");
        assert_eq!(stream.drc.gain, 0.5);
    }

    fn tagged(tags: Vec<ChannelTag>) -> StreamDeclaration {
        Declaration {
            tags,
            ..Declaration::default()
        }
        .into()
    }

    fn tag(kind: &str, channels: &[usize]) -> ChannelTag {
        ChannelTag {
            kind: kind.to_owned(),
            channels: channels.to_vec(),
            ..ChannelTag::default()
        }
    }

    /// Only the dialogue tags make the channels the level applies to, in
    /// order and once each; a kind this renderer does not know is kept for
    /// Studio but scales nothing.
    #[test]
    fn the_dialogue_channels_come_from_the_dialogue_tags() {
        let declaration = tagged(vec![
            tag("dialogue", &[14, 12]),
            tag("commentary", &[20]),
            tag("dialogue", &[13, 12]),
        ]);
        assert_eq!(declaration.dialogue_channels, [12, 13, 14]);
        assert_eq!(declaration.tags.len(), 3);
    }

    /// The level reaches the tagged channels over the ramp and then holds;
    /// the other channels are left as they are.
    #[test]
    fn the_dialogue_level_ramps_onto_the_tagged_channels_only() {
        let rate = 48_000;
        let ramp = (rate as f32 * TAG_GAIN_RAMP_SECS) as usize;
        let mut gain = TagGain::default();
        let mut pcm = vec![1.0f32; 3 * (ramp + 100)];
        gain.apply(&mut pcm, 3, &[1, 2], -6.0, rate);
        let target = renderer::dsp::db::db_to_linear(-6.0);
        let frames: Vec<&[f32]> = pcm.chunks_exact(3).collect();
        assert!(frames.iter().all(|f| f[0] == 1.0));
        assert!(
            frames[0][1] < 1.0 && frames[0][1] > 0.99,
            "{}",
            frames[0][1]
        );
        assert!(frames[ramp / 2][1] > target && frames[ramp / 2][1] < 1.0);
        assert!((frames[ramp - 1][1] - target).abs() < 1e-6);
        assert!(
            frames[ramp..]
                .iter()
                .all(|f| f[1] == target && f[2] == target)
        );
        // Steady: the next frame is scaled throughout.
        let mut next = vec![1.0f32; 30];
        gain.apply(&mut next, 3, &[1, 2], -6.0, rate);
        assert!(next.chunks_exact(3).all(|f| f == [1.0, target, target]));
    }

    /// No tagged channel, or 0 dB settled: the PCM is not touched, and a
    /// stream that tags its dialogue later starts at the level, not from
    /// 0 dB.
    #[test]
    fn nothing_tagged_or_zero_db_leaves_the_pcm_alone() {
        let mut gain = TagGain::default();
        let mut pcm = vec![0.5f32; 64];
        gain.apply(&mut pcm, 2, &[], -6.0, 48_000);
        assert!(pcm.iter().all(|&s| s == 0.5));
        gain.apply(&mut pcm, 2, &[1], -6.0, 48_000);
        let target = renderer::dsp::db::db_to_linear(-6.0);
        assert!(pcm.chunks_exact(2).all(|f| f == [0.5, 0.5 * target]));

        let mut settled = TagGain::default();
        let mut pcm = vec![0.5f32; 64];
        settled.apply(&mut pcm, 2, &[1], 0.0, 48_000);
        assert!(pcm.iter().all(|&s| s == 0.5));
        // A tag past the frame's channels is a bridge bug: left alone.
        settled.apply(&mut pcm, 2, &[2], -6.0, 48_000);
        assert!(pcm.iter().all(|&s| s == 0.5));
    }

    /// The tags reach the control when they change, and only then.
    #[test]
    fn the_tags_are_published_when_they_change() {
        let renderer = crate::channel_objects::tests::renderer_7_1_4();
        let control = renderer.renderer_control();
        let mut stream = StreamState::default();
        stream.declaration = tagged(vec![ChannelTag {
            kind: "dialogue".to_owned(),
            language: "fr".to_owned(),
            label: "Dialogue".to_owned(),
            channels: vec![12, 13],
        }]);
        stream.publish_tags(&control);
        let published: serde_json::Value = serde_json::from_str(&control.channel_tags()).unwrap();
        assert_eq!(
            published,
            serde_json::json!([{
                "kind": "dialogue", "language": "fr", "label": "Dialogue", "channels": [12, 13]
            }])
        );
        let generation = control.live_state_generation();
        stream.publish_tags(&control);
        assert_eq!(control.live_state_generation(), generation);
        stream.declaration = StreamDeclaration::default();
        stream.publish_tags(&control);
        assert_eq!(control.channel_tags(), "[]");
        assert_ne!(control.live_state_generation(), generation);
    }
}
