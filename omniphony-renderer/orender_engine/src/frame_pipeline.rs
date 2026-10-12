//! The per-frame sequence of a render host, written once for both: the
//! embedded [`Engine`] (liborender, inside mpv) and the `orender render` CLI.
//!
//! Each host used to run its own copy of this sequence, each described as
//! mirroring the other, and every change to the frame path had to be made
//! twice: one made on one side only was silent, the player and the CLI simply
//! rendered differently. The sequence now lives here, in two calls:
//!
//! - [`FramePipeline::prepare`], what a frame says about the stream: the
//!   renderer follows its rate, its dialogue normalisation is latched, and
//!   its metadata is planned into the object frame's events (and shown);
//! - [`FramePipeline::render`], its audio: the channel-content plan and the
//!   objects synthesized from it, the PCM with the DRC ramp and the dialogue
//!   level, metering, the render, and the meter bundle.
//!
//! What stays with a host is its plumbing — the decode thread, segment starts
//! and resets (each host has its own around the shared
//! [`StreamState::begin_segment`]), where the bridge's declaration comes from,
//! the output buffers and sinks. The in-process overlay is fed from here; it
//! self-gates ([`overlay::is_active`]), so the CLI, which never pulls it, pays
//! one atomic load per frame.
//!
//! `tests/host_parity.rs` in the CLI crate holds the two hosts to the same
//! output, bit for bit.
//!
//! [`Engine`]: crate::engine::Engine

use anyhow::Result;
use bridge_api::{RChannelLabel, RCoordinateFormat, RDecodedFrame};
use renderer::metering::{AudioMeter, DutyEma, MeterSnapshot};
use renderer::spatial_renderer::SpatialRenderer;

use crate::channel_objects::StageCounts;
use crate::events::Configuration;
use crate::osc::{MeterTimings, ObjectMeta, OscSender};
use crate::render_metering::{meter_render_input, meter_render_output};
use crate::stream_state::StreamState;
use crate::virtual_bed::BedPlanKind;
use crate::{overlay, render};

/// What a frame's render produced.
pub enum FrameOutput {
    /// The renderer's output, interleaved, `channels` wide: the speakers, or
    /// the binaural pair.
    Rendered { samples: Vec<f32>, channels: usize },
    /// Channel content whose labels map to nothing: silence as wide as the
    /// renderer's output, so the host still advances by the frame.
    Silence { samples: Vec<f32>, channels: usize },
    /// Channel content in the `host` channel render mode: nothing was
    /// rendered, the decoded channels are the host's to play as they are.
    /// The donated buffer comes back unused.
    Passthrough { unused: Vec<f32> },
}

/// A frame's render: its output, and what the host reports about it.
pub struct FrameRender {
    pub output: FrameOutput,
    /// The render call alone, raw (the meter bundle carries it smoothed);
    /// 0 when nothing was rendered.
    pub render_ms: f32,
    /// A meter bundle went out for this frame.
    pub meter_bundle_sent: bool,
}

/// The per-frame sequence and what it keeps across frames. See the
/// [module docs](self).
pub struct FramePipeline {
    /// The per-stream state and its rules.
    pub stream: StreamState,
    /// The frame's PCM as `f32`, the synthesized objects' channels appended:
    /// kept across frames so a steady stream allocates nothing.
    pcm_f32: Vec<f32>,
    /// Duty-cycle EMA of the render cost, for the meter bundle: raw per-frame
    /// timings alias with 40-sample TrueHD access units (the FIR crossover's
    /// burst lands on one frame in ~26), so the emitted figure is smoothed to
    /// a per-frame equivalent.
    render_duty: DutyEma,
    /// The labels of a channel frame are being rendered as silence (no
    /// mapping): said once when it starts, not on every frame.
    unmapped_labels: bool,
    /// The meter snapshot and the overlay's object levels, refilled on each
    /// metered frame: a send lends the snapshot to the OSC telemetry thread
    /// and takes a spare back, so metering allocates nothing once warm.
    meter_snapshot: MeterSnapshot,
    overlay_levels: Vec<(u32, f64)>,
    /// The evaluation grid last offered to the renderer's control: a
    /// stream's bridge hint is offered once, when it changes.
    offered_grid: Option<renderer::evaluation_grid::BridgeHint>,
}

impl FramePipeline {
    pub fn new(coordinate_format: RCoordinateFormat) -> Self {
        Self {
            stream: StreamState::new(coordinate_format),
            pcm_f32: Vec::new(),
            render_duty: DutyEma::default(),
            unmapped_labels: false,
            meter_snapshot: MeterSnapshot::default(),
            overlay_levels: Vec::new(),
            offered_grid: None,
        }
    }

    /// What `frame`, the block starting at `sample_pos`, says about the
    /// stream. Run once per frame, after the host has started a segment and
    /// taken on the bridge's declaration if the frame brings either, and
    /// before [`render`](Self::render).
    ///
    /// Everything sent to OSC from here on describes this block. The renderer
    /// follows the frame's rate; the frame's dialogue normalisation is latched
    /// (once per segment); each metadata payload marks the stream as carrying
    /// objects, updates the object↔channel declaration and the names, is
    /// planned into [`StreamState::frame_events`] and shown (OSC clients, the
    /// overlay).
    ///
    /// Without a renderer (a CLI run without `--enable-vbap`) nothing is
    /// planned; the objects are still declared and shown.
    pub fn prepare(
        &mut self,
        frame: &RDecodedFrame,
        sample_pos: u64,
        mut renderer: Option<&mut SpatialRenderer>,
        mut osc: Option<&mut OscSender>,
    ) -> Result<()> {
        if let Some(osc) = osc.as_deref_mut() {
            osc.render_at(sample_pos);
        }
        if let Some(renderer) = renderer.as_deref_mut() {
            render::follow_stream_rate(renderer, frame.sampling_frequency)?;
            self.offer_bridge_grid(renderer);
        }
        let renderer = renderer.map(|renderer| &*renderer);
        let want_osc = osc.as_deref().is_some_and(OscSender::has_osc_clients);

        if let Some(renderer) = renderer
            && self.stream.latch_dialnorm(frame, renderer)
            && want_osc
            && let Some(osc) = osc.as_deref_mut()
        {
            osc.send_loudness_state();
        }

        if frame.metadata.is_empty() {
            return Ok(());
        }
        let overlay_active = overlay::is_active();
        let sample_rate = frame.sampling_frequency.max(1);
        for meta in frame.metadata.iter() {
            // Cached whether or not anyone is listening: names are declared
            // sparsely, typically once at the start of a stream, so a Studio
            // attaching mid-playback would otherwise show an object list that
            // stays unnamed until the next declaration — which for most
            // content never comes.
            self.stream.note_object_metadata(meta);
            let conf = Configuration::from(meta);
            if let Some(renderer) = renderer {
                self.stream
                    .plan_object_frame(&frame.channel_labels, meta, &conf, renderer);
            }
            if !(want_osc || overlay_active) {
                continue;
            }
            let objects = self.stream.object_frame_metas(renderer, &conf);
            if want_osc && let Some(osc) = osc.as_deref_mut() {
                // Object frames and timestamps carry the bridge's own sample
                // position, unchanged.
                osc.send_object_frame(
                    meta.sample_pos,
                    meta.ramp_duration,
                    self.stream.osc_coordinate_format(),
                    &objects,
                );
                let seconds = meta.sample_pos as f64 / sample_rate as f64;
                osc.send_timestamp(meta.sample_pos, seconds);
            }
            if overlay_active {
                overlay::update_positions(overlay_positions(&objects));
            }
        }
        Ok(())
    }

    /// Offer the grid the stream's bridge hints to the renderer's control,
    /// once per change; a host off the audio thread takes it and rebuilds
    /// when the grid follows the bridge (`renderer::evaluation_grid`). A
    /// compare per frame; the offer itself never blocks and is made again
    /// on a later frame when the control was busy.
    fn offer_bridge_grid(&mut self, renderer: &SpatialRenderer) {
        if let Some(grid) = self.stream.declaration.grid
            && self.offered_grid != Some(grid)
            && renderer.renderer_control().offer_bridge_hint(grid)
        {
            self.offered_grid = Some(grid);
        }
    }

    /// Render `frame`, the block starting at `sample_pos`, after
    /// [`prepare`](Self::prepare).
    ///
    /// `objects` picks the path: the object frame's events, or channel
    /// content — planned by the channel render mode, with the objects the
    /// enabled stages synthesize from the bed. Usually whether the stream
    /// carries objects ([`StreamState::has_objects`]); a host that knows the
    /// frame is plain PCM whatever played before says so.
    ///
    /// The output is written into `donated` (a buffer of the host's, handed
    /// back in the [`FrameOutput`]). `meter` is metered when an OSC client
    /// or the overlay wants levels, and created if it does not exist yet.
    /// The meter bundle carries `output_stage` — the host's output-stage
    /// figures, all `None` in the embedded host — with this frame's timings
    /// (`decode_ms`, the render's) filled in here.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        frame: &RDecodedFrame,
        sample_pos: u64,
        objects: bool,
        renderer: &mut SpatialRenderer,
        mut osc: Option<&mut OscSender>,
        meter: &mut Option<AudioMeter>,
        mut donated: Vec<f32>,
        decode_ms: f32,
        output_stage: MeterTimings,
    ) -> Result<FrameRender> {
        let channel_count = frame.channel_count as usize;
        let sample_count = frame.sample_count as usize;
        let sample_rate = frame.sampling_frequency.max(1);
        let overlay_active = overlay::is_active();
        let control = renderer.renderer_control();

        let mut stage_counts = StageCounts::default();
        if objects {
            self.stream
                .publish_object_stream_processing(&control, &frame.channel_labels);
        } else {
            let labels: &[RChannelLabel] = &frame.channel_labels;
            // The plan depends only on the labels and a few live params, so the
            // planner reuses it until one of them actually changes — a steady
            // stream plans once instead of ~1200 times a second.
            let kind = self.stream.plan_bed(renderer, labels);
            if kind != BedPlanKind::Silence {
                self.unmapped_labels = false;
            }
            match kind {
                BedPlanKind::Events => {}
                BedPlanKind::HostPassthrough => {
                    return Ok(FrameRender {
                        output: FrameOutput::Passthrough { unused: donated },
                        render_ms: 0.0,
                        meter_bundle_sent: false,
                    });
                }
                BedPlanKind::Silence => {
                    if !std::mem::replace(&mut self.unmapped_labels, true) {
                        log::warn!(
                            "No channel render mapping for labels {labels:?} - outputting silence"
                        );
                    }
                    // As wide as the sink: what the renderer would have
                    // emitted (the binaural pair in headphone mode, not the
                    // speaker count).
                    let channels = renderer.output_channel_count();
                    donated.clear();
                    donated.resize(sample_count * channels, 0.0);
                    return Ok(FrameRender {
                        output: FrameOutput::Silence {
                            samples: donated,
                            channels,
                        },
                        render_ms: 0.0,
                        meter_bundle_sent: false,
                    });
                }
            }

            // Borrowed from the live topology rather than `speaker_layout()`,
            // which hands back a deep copy of the whole layout.
            let topology = control.active_topology();
            let output_layout = &topology.speaker_layout;
            // Synthesize objects from the bed (phantom extraction, then the
            // bed→height lift): planned here, so each gets its channel event;
            // their audio is appended to the bed PCM below.
            stage_counts = self.stream.sync_channel_objects(
                &control,
                labels,
                channel_count,
                output_layout,
                sample_rate,
            );

            // The virtual bed and the synthesized objects: shown, they appear
            // in Studio's 3D view and object list; omitted, they are rendered
            // but never seen.
            let want_osc = osc.as_deref().is_some_and(OscSender::has_osc_clients);
            if want_osc || overlay_active {
                let shown = self.stream.bed_frame_metas(&control, labels, &topology);
                if !shown.is_empty() {
                    if want_osc && let Some(osc) = osc.as_deref_mut() {
                        osc.send_object_frame(sample_pos, 0, 0, &shown);
                    }
                    if overlay_active {
                        overlay::update_positions(overlay_positions(&shown));
                    }
                }
            }
        }

        // The decoded PCM as f32, with the DRC gain ramp and the dialogue
        // level applied; then the two upmix stages now that the bed PCM
        // exists. The phantom pre-stage subtracts correlated content from the
        // bed *in place* and appends its planar objects; the height lift runs
        // on the reduced bed and appends its own. The renderer sees one
        // extended interleaved buffer (bed | phantom objects | height
        // objects). Both cost nothing when inactive, and on the object path.
        self.stream.fill_pcm_f32(&mut self.pcm_f32, frame, &control);
        let (render_pcm, render_channels) = self.stream.channel_objects.process_and_extend(
            &mut self.pcm_f32,
            channel_count,
            sample_count,
            sample_rate,
            stage_counts,
        );

        // VU metering: the render input (fixed channels and objects, the
        // extended width) before the render, the speakers after. For OSC
        // metering clients and the overlay, whose object circles follow the
        // levels — which it needs with no OSC client connected, hence a meter
        // created here when the host has none.
        let want_meter_osc = osc.as_deref().is_some_and(OscSender::has_metering_clients);
        let want_metering = want_meter_osc || overlay_active;
        if want_metering && meter.is_none() {
            *meter = Some(AudioMeter::new_with_rate_atomic(
                renderer.num_speakers(),
                control.meter_rate_atomic(),
            ));
        }
        if want_metering && let Some(meter) = meter.as_mut() {
            meter_render_input(meter, render_pcm, render_channels);
        }

        // An object frame's events, or a channel frame's (the bed plan's and
        // the synthesized objects').
        let events = if objects {
            &self.stream.frame_events
        } else {
            &self.stream.bed_events
        };
        let render_started = std::time::Instant::now();
        let mut rendered =
            renderer.render_frame(render_pcm, render_channels, events, donated, want_metering)?;
        let render_ms = render_started.elapsed().as_secs_f32() * 1000.0;
        // Emptied, so the next object frame's events land in the same
        // allocation.
        self.stream.frame_events.clear();
        let frame_ms = sample_count as f32 / sample_rate as f32 * 1000.0;
        let render_smoothed_ms = self.render_duty.update(render_ms, frame_ms);

        // The width of what was rendered, never a fresh
        // `output_channel_count()`: that re-reads the live output mode, which
        // the OSC listener flips on its own thread. A switch to speakers
        // landing between the render and that read used to publish 12
        // channels for 2-channel binaural samples, and the embedded host
        // copied `n_frames * 12` floats out of a buffer holding a sixth of
        // that — heap read past the end, played as PCM.
        let channels = rendered.n_channels;

        let mut meter_bundle_sent = false;
        if want_metering
            && let Some(meter) = meter.as_mut()
            && meter_render_output(meter, renderer, &rendered, &mut self.meter_snapshot)
        {
            if overlay_active {
                self.overlay_levels.clear();
                self.overlay_levels.extend(
                    self.meter_snapshot
                        .object_levels
                        .iter()
                        .map(|&(id, _peak, rms)| (id, rms as f64)),
                );
                overlay::update_levels(&self.overlay_levels);
            }
            if want_meter_osc && let Some(osc) = osc {
                let timings = MeterTimings {
                    decode_time_ms: Some(decode_ms),
                    crossover_time_ms: Some(rendered.crossover_time_ms),
                    render_time_ms: Some(render_smoothed_ms),
                    frame_duration_ms: Some(frame_ms),
                    drc_gain: Some(self.stream.drc.gain),
                    ..output_stage
                };
                osc.send_meter_bundle(&mut self.meter_snapshot, &mut rendered, timings);
                meter_bundle_sent = true;
            }
        }

        // The embedded host hands (n_frames, n_channels) over FFI, and its
        // host sizes its copy from them — so a violation here is not a wrong
        // number, it is a read past the end of this buffer. Free in release.
        debug_assert_eq!(
            rendered.samples.len(),
            sample_count * channels,
            "rendered samples must be n_frames * n_channels"
        );
        // The metering lists go back to the renderer, which refills them on
        // the next metered frame: no per-frame allocation on the render path.
        let samples = renderer.recycle_frame(rendered);
        Ok(FrameRender {
            output: FrameOutput::Rendered { samples, channels },
            render_ms,
            meter_bundle_sent,
        })
    }
}

/// Map the per-frame object metas to overlay positions `(id, x, y, z)`. The id
/// is the object's frame index, matching the `/omniphony/object/{id}` OSC id, so
/// the overlay keys colours and motion trails exactly as Studio did. Polar
/// objects carry no front-view cartesian position, so they sit at the origin —
/// identical to the previous Studio→Lua path, which zeroed non-cartesian
/// positions before sending them to the overlay.
fn overlay_positions(objects: &[ObjectMeta]) -> Vec<(u32, f64, f64, f64, String)> {
    objects
        .iter()
        .enumerate()
        .map(|(idx, o)| {
            let (x, y, z) = if o.coord_mode.eq_ignore_ascii_case("cartesian") {
                (o.x as f64, o.y as f64, o.z as f64)
            } else {
                (0.0, 0.0, 0.0)
            };
            (idx as u32, x, y, z, o.name.clone())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer_build::{SpatialRendererParams, build_spatial_renderer};
    use abi_stable::std_types::{ROption, RVec};
    use bridge_api::{REvent, RMetadataFrame, RObjectChannel};
    use renderer::live_params::ChannelRenderMode;
    use renderer::speaker_layout::SpeakerLayout;

    const SAMPLES: usize = 256;

    /// The pipeline writes the process-global overlay (object positions,
    /// levels) whenever an overlay session is active, which one of the
    /// overlay's own tests may have just armed: hold the overlay's test lock,
    /// or those tests read positions this one wrote.
    fn overlay_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::overlay::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    fn renderer() -> SpatialRenderer {
        build_spatial_renderer(
            &SpatialRendererParams::from_render_config(None),
            SpeakerLayout::preset("7.1.4").expect("preset layout"),
            48_000,
            bridge_api::RVbapCartesianDefaults {
                x_size: 9,
                y_size: 9,
                z_size: 5,
                z_neg_size: 0,
                allow_negative_z: true,
            },
            bridge_api::RVbapTableMode::Cartesian,
            None,
        )
        .expect("renderer")
    }

    /// A frame of `labels`, each channel at the level `levels` gives it.
    fn frame(labels: &[RChannelLabel], levels: &[i32]) -> RDecodedFrame {
        RDecodedFrame {
            sampling_frequency: 48_000,
            sample_count: SAMPLES as u32,
            channel_count: labels.len() as u32,
            pcm: (0..SAMPLES).flat_map(|_| levels.iter().copied()).collect(),
            channel_labels: labels.iter().copied().collect(),
            metadata: RVec::new(),
            drc_gain: 1.0,
            drc_ramp_duration: 0,
            dialogue_level: ROption::RNone,
            is_new_segment: false,
        }
    }

    /// The grid a stream's bridge hints is offered to the renderer once per
    /// change, with the frame its declaration came with; content no bridge
    /// declared keeps the one in force.
    #[test]
    fn a_streams_grid_hint_is_offered_once_per_change() {
        use renderer::evaluation_grid::EvaluationGrid;
        let mut renderer = renderer();
        let control = renderer.renderer_control();
        let mut pipeline = FramePipeline::new(RCoordinateFormat::Cartesian);
        let frame = frame(&[RChannelLabel::L, RChannelLabel::R], &[1000, 1000]);
        let other = EvaluationGrid::from_hint(
            bridge_api::RVbapCartesianDefaults {
                x_size: 20,
                y_size: 20,
                z_size: 8,
                z_neg_size: 0,
                allow_negative_z: false,
            },
            bridge_api::RVbapTableMode::Polar,
        );
        let offers = control.bridge_grids_offered();

        // No declaration yet, then the stream's: offered once.
        pipeline
            .prepare(&frame, 0, Some(&mut renderer), None)
            .unwrap();
        assert_eq!(control.bridge_grids_offered(), offers);
        pipeline.stream.declaration.grid = Some(renderer::evaluation_grid::BridgeHint {
            grid: other,
            bridge: 1,
        });
        for _ in 0..3 {
            pipeline
                .prepare(&frame, 0, Some(&mut renderer), None)
                .unwrap();
        }
        assert_eq!(control.bridge_grids_offered(), offers + 1);
        assert!(control.bridge_grid_pending());

        // Undeclared content (live PCM) keeps it.
        pipeline.stream.declaration.grid = None;
        pipeline
            .prepare(&frame, 0, Some(&mut renderer), None)
            .unwrap();
        assert_eq!(control.bridge_grids_offered(), offers + 1);
    }

    /// Energy of output channel `channel` in an interleaved block.
    fn energy(samples: &[f32], channels: usize, channel: usize) -> f32 {
        samples
            .iter()
            .skip(channel)
            .step_by(channels)
            .map(|s| s * s)
            .sum()
    }

    fn render(
        pipeline: &mut FramePipeline,
        renderer: &mut SpatialRenderer,
        frame: &RDecodedFrame,
        objects: bool,
    ) -> FrameRender {
        pipeline
            .render(
                frame,
                0,
                objects,
                renderer,
                None,
                &mut None,
                Vec::new(),
                0.0,
                MeterTimings::default(),
            )
            .expect("render")
    }

    /// An object frame: `prepare` plans its metadata, and the object path
    /// renders it where it says — then leaves no event behind for the next
    /// frame. The same stream's plain PCM (a live input after a bitstream)
    /// takes the channel path whatever `has_objects` says.
    #[test]
    fn an_object_frame_is_planned_then_rendered_where_it_says() {
        let _overlay = overlay_lock();
        use RChannelLabel::{L, Object, R};
        let mut renderer = renderer();
        let mut pipeline = FramePipeline::new(RCoordinateFormat::Cartesian);
        let mut object_frame = frame(&[L, R, Object], &[0, 0, 1 << 22]);
        object_frame.metadata = RVec::from(vec![RMetadataFrame {
            events: RVec::from(vec![REvent {
                id: 0,
                sample_pos: 0,
                has_pos: true,
                // Front right, in the ADM's cartesian convention.
                pos: [1.0, 1.0, 0.0],
                gain_db: 0,
                size: [0.0; 3],
                ramp_duration: 0,
            }]),
            object_channels: RVec::from(vec![RObjectChannel { id: 0, channel: 2 }]),
            channel_gains: RVec::new(),
            name_updates: RVec::new(),
            sample_pos: 0,
            ramp_duration: 0,
        }]);

        pipeline
            .prepare(&object_frame, 0, Some(&mut renderer), None)
            .expect("prepare");
        assert!(pipeline.stream.has_objects);
        assert!(!pipeline.stream.frame_events.is_empty(), "nothing planned");

        let out = render(&mut pipeline, &mut renderer, &object_frame, true);
        let FrameOutput::Rendered { samples, channels } = out.output else {
            panic!("an object frame renders");
        };
        assert_eq!(channels, 12);
        assert_eq!(samples.len(), SAMPLES * channels);
        // 7.1.4 in render order: L, R, …
        let (left, right) = (energy(&samples, 12, 0), energy(&samples, 12, 1));
        assert!(
            right > 100.0 * left.max(1e-12),
            "the object is front right: L {left} R {right}"
        );
        assert!(pipeline.stream.frame_events.is_empty(), "events left over");

        // Plain PCM of the same stream: the bed plan, not the object path.
        let pcm = frame(&[L, R], &[1 << 22, 0]);
        pipeline
            .prepare(&pcm, SAMPLES as u64, Some(&mut renderer), None)
            .expect("prepare");
        let out = render(&mut pipeline, &mut renderer, &pcm, false);
        let FrameOutput::Rendered { samples, channels } = out.output else {
            panic!("channel content renders");
        };
        let (left, right) = (energy(&samples, channels, 0), energy(&samples, channels, 1));
        assert!(left > 100.0 * right.max(1e-12), "L {left} R {right}");
    }

    /// The `host` channel render mode renders nothing: the decoded channels
    /// are the host's, and its buffer comes back untouched.
    #[test]
    fn the_host_channel_mode_hands_the_channels_back() {
        let _overlay = overlay_lock();
        use RChannelLabel::{L, R};
        let mut renderer = renderer();
        renderer.renderer_control().live.write().channel_render_mode = ChannelRenderMode::Host;
        let mut pipeline = FramePipeline::new(RCoordinateFormat::Cartesian);
        let pcm = frame(&[L, R], &[1 << 22, 0]);
        let donated = Vec::with_capacity(4096);
        let out = pipeline
            .render(
                &pcm,
                0,
                false,
                &mut renderer,
                None,
                &mut None,
                donated,
                0.0,
                MeterTimings::default(),
            )
            .expect("render");
        let FrameOutput::Passthrough { unused } = out.output else {
            panic!("the host mode renders nothing");
        };
        assert_eq!(unused.capacity(), 4096, "the host's buffer comes back");
        assert_eq!(out.render_ms, 0.0);
    }
}
