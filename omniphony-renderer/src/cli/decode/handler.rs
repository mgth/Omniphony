use super::output::AudioWriter;
use super::output_runtime_sync::OutputRuntimeCoordinator;
use super::sample_write::SampleWriteCoordinator;
use super::spatial_metadata::SpatialMetadataCoordinator;
use super::writer_lifecycle::WriterLifecycleCoordinator;
use crate::cli::command::OutputBackend;
use crate::runtime_osc::OscSender;
#[cfg(target_os = "linux")]
use audio_output::pipewire::PipewireBufferConfig;
use audio_output::{AdaptiveResamplingConfig, AudioControl};
use bridge_api::{RChannelLabel, RCoordinateFormat, RDecodedFrame};

use anyhow::Result;
use log::Level;
use renderer::metering::AudioMeter;
use renderer::speaker_layout::SpeakerLayout;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Instant;

pub(crate) struct BedChannelMapper;

pub(crate) struct ChannelCountCalculator;

impl ChannelCountCalculator {
    const TARGET_BED_CHANNELS: usize = 10; // 7.1.2 layout

    /// Calculate the effective channel count for bed conformance
    /// Returns (num_bed_channels, num_object_channels, conformed_channel_count)
    pub(crate) fn calculate_bed_conform_counts(
        original_channel_count: usize,
        bed_indices: &[usize],
    ) -> (usize, usize, usize) {
        let num_bed_channels = bed_indices.len();
        let num_object_channels = original_channel_count.saturating_sub(num_bed_channels);
        let conformed_channel_count = Self::TARGET_BED_CHANNELS + num_object_channels;
        (
            num_bed_channels,
            num_object_channels,
            conformed_channel_count,
        )
    }

    /// Calculate conformed channel count only (shorthand for common case)
    pub(crate) fn calculate_conformed_channel_count(
        original_channel_count: usize,
        bed_indices: &[usize],
    ) -> usize {
        let (_, _, conformed_count) =
            Self::calculate_bed_conform_counts(original_channel_count, bed_indices);
        conformed_count
    }
}

impl BedChannelMapper {
    pub(crate) fn apply_bed_conformance(
        original_samples: Vec<i32>,
        original_channel_count: usize,
        bed_indices: &[usize],
    ) -> Vec<i32> {
        let (num_bed_channels, num_object_channels, conformed_channel_count) =
            ChannelCountCalculator::calculate_bed_conform_counts(
                original_channel_count,
                bed_indices,
            );
        let samples_per_frame = original_samples.len() / original_channel_count;

        let mut conformed_samples = Vec::with_capacity(samples_per_frame * conformed_channel_count);

        for sample_idx in 0..samples_per_frame {
            // Handle bed channels (0-9)
            for target_bed_ch in 0..ChannelCountCalculator::TARGET_BED_CHANNELS {
                if let Some(source_ch_pos) =
                    bed_indices.iter().position(|&idx| idx == target_bed_ch)
                {
                    let sample =
                        original_samples[sample_idx * original_channel_count + source_ch_pos];
                    conformed_samples.push(sample);
                } else {
                    conformed_samples.push(0i32);
                }
            }

            // Handle object channels
            for obj_ch in 0..num_object_channels {
                let source_ch = num_bed_channels + obj_ch;
                let sample = original_samples[sample_idx * original_channel_count + source_ch];
                conformed_samples.push(sample);
            }
        }

        conformed_samples
    }

    pub(crate) fn apply_bed_conformance_to_frame(
        pcm: &[i32],
        sample_count: usize,
        channel_count: usize,
        bed_indices: &[usize],
    ) -> Vec<i32> {
        let (num_bed_channels, num_object_channels, conformed_channel_count) =
            ChannelCountCalculator::calculate_bed_conform_counts(channel_count, bed_indices);

        let mut samples = Vec::with_capacity(sample_count * conformed_channel_count);

        for sample_idx in 0..sample_count {
            // Handle bed channels (0-9)
            for target_bed_ch in 0..ChannelCountCalculator::TARGET_BED_CHANNELS {
                if let Some(source_ch_pos) =
                    bed_indices.iter().position(|&idx| idx == target_bed_ch)
                {
                    let sample = pcm[sample_idx * channel_count + source_ch_pos];
                    samples.push(sample);
                } else {
                    samples.push(0i32);
                }
            }

            // Handle object channels
            for obj_ch in 0..num_object_channels {
                let source_ch = num_bed_channels + obj_ch;
                let sample = pcm[sample_idx * channel_count + source_ch];
                samples.push(sample);
            }
        }

        samples
    }
}

#[inline]
pub(crate) fn map_depth_with_room_ratios(
    depth: f32,
    front_ratio: f32,
    rear_ratio: f32,
    center_blend: f32,
) -> f32 {
    let d = depth.clamp(-1.0, 1.0);
    let blend = center_blend.clamp(0.0, 1.0);
    let center_ratio = rear_ratio + (front_ratio - rear_ratio) * blend;
    if d >= 0.0 {
        let t = d;
        let a = center_ratio - front_ratio;
        let b = 2.0 * (front_ratio - center_ratio);
        a * t * t * t + b * t * t + center_ratio * t
    } else {
        let t = -d;
        let a = center_ratio - rear_ratio;
        let b = 2.0 * (rear_ratio - center_ratio);
        -(a * t * t * t + b * t * t + center_ratio * t)
    }
}

pub(crate) fn inverse_map_depth_with_room_ratios(
    mapped_depth: f32,
    front_ratio: f32,
    rear_ratio: f32,
    center_blend: f32,
) -> f32 {
    let y = mapped_depth;
    if y >= 0.0 {
        let target = y.clamp(0.0, front_ratio.max(0.0));
        let mut lo = 0.0f32;
        let mut hi = 1.0f32;
        for _ in 0..28 {
            let mid = (lo + hi) * 0.5;
            let val = map_depth_with_room_ratios(mid, front_ratio, rear_ratio, center_blend);
            if val < target {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        (lo + hi) * 0.5
    } else {
        let target = y.clamp(-rear_ratio.max(0.0), 0.0);
        let mut lo = -1.0f32;
        let mut hi = 0.0f32;
        for _ in 0..28 {
            let mid = (lo + hi) * 0.5;
            let val = map_depth_with_room_ratios(mid, front_ratio, rear_ratio, center_blend);
            if val < target {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        (lo + hi) * 0.5
    }
}

pub(crate) fn inverse_room_ratio_map_for_virtual_object(
    target_x: f32,
    target_y: f32,
    target_z: f32,
    room_ratio: [f32; 3],
    room_ratio_rear: f32,
    room_ratio_lower: f32,
    room_ratio_center_blend: f32,
) -> (f32, f32, f32) {
    let width = room_ratio[0].max(0.01);
    let front = room_ratio[1].max(0.01);
    let height = room_ratio[2].max(0.01);
    let rear = room_ratio_rear.max(0.01);
    let lower = room_ratio_lower.max(0.01);

    let x = (target_x / width).clamp(-1.0, 1.0);
    let y = inverse_map_depth_with_room_ratios(target_y, front, rear, room_ratio_center_blend)
        .clamp(-1.0, 1.0);
    let z = if target_z >= 0.0 {
        (target_z / height).clamp(-1.0, 1.0)
    } else {
        (target_z / lower).clamp(-1.0, 1.0)
    };
    (x, y, z)
}

#[derive(Clone)]
struct VirtualBedLayouts {
    layout_5_1: Option<SpeakerLayout>,
    layout_7_1: Option<SpeakerLayout>,
}

static VIRTUAL_BED_LAYOUTS: OnceLock<VirtualBedLayouts> = OnceLock::new();

fn virtual_bed_layouts() -> &'static VirtualBedLayouts {
    VIRTUAL_BED_LAYOUTS.get_or_init(|| VirtualBedLayouts {
        layout_5_1: load_virtual_bed_layout("5.1.yaml"),
        layout_7_1: load_virtual_bed_layout("7.1.yaml"),
    })
}

fn load_virtual_bed_layout(file_name: &str) -> Option<SpeakerLayout> {
    let mut candidates: Vec<PathBuf> = vec![
        PathBuf::from("layouts").join(file_name),
        PathBuf::from("omniphony").join("layouts").join(file_name),
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("layouts")
            .join(file_name),
    ];
    candidates.dedup();

    for path in candidates {
        if !path.exists() {
            continue;
        }
        match SpeakerLayout::from_file(&path) {
            Ok(layout) => {
                log::info!("Loaded virtual bed layout from {}", path.display());
                return Some(layout);
            }
            Err(e) => {
                log::warn!(
                    "Failed to load virtual bed layout '{}' ({}): {}",
                    file_name,
                    path.display(),
                    e
                );
            }
        }
    }

    log::warn!(
        "Virtual bed layout '{}' not found on disk, using built-in fallback positions",
        file_name
    );
    None
}

pub(crate) fn find_speaker_in_layout(
    layout: &SpeakerLayout,
    aliases: &[&str],
) -> Option<(String, f32, f32, f32)> {
    for speaker in &layout.speakers {
        if aliases
            .iter()
            .any(|alias| speaker.name.eq_ignore_ascii_case(alias))
        {
            return Some((
                speaker.name.clone(),
                speaker.azimuth,
                speaker.elevation,
                speaker.distance,
            ));
        }
    }
    None
}

pub(crate) fn label_aliases(label: RChannelLabel, use_7_1: bool) -> Option<&'static [&'static str]> {
    match label {
        RChannelLabel::L => Some(&["FL", "L", "FrontLeft", "LeftFront"]),
        RChannelLabel::R => Some(&["FR", "R", "FrontRight", "RightFront"]),
        RChannelLabel::C => Some(&["C", "FC", "Center", "Centre"]),
        RChannelLabel::LFE | RChannelLabel::LFE2 => {
            Some(&["LFE", "LFE1", "Sub", "Subwoofer", "SW"])
        }
        RChannelLabel::Ls => {
            if use_7_1 {
                Some(&["SL", "Ls", "LeftSurround", "SurroundLeft"])
            } else {
                Some(&[
                    "SL",
                    "Ls",
                    "BL",
                    "Lb",
                    "LeftSurround",
                    "SurroundLeft",
                    "BackLeft",
                    "LeftBack",
                ])
            }
        }
        RChannelLabel::Rs => {
            if use_7_1 {
                Some(&["SR", "Rs", "RightSurround", "SurroundRight"])
            } else {
                Some(&[
                    "SR",
                    "Rs",
                    "BR",
                    "Rb",
                    "RightSurround",
                    "SurroundRight",
                    "BackRight",
                    "RightBack",
                ])
            }
        }
        RChannelLabel::Lb => Some(&[
            "BL", "Lb", "Lrs", "BackLeft", "LeftBack", "RearLeft", "LeftRear",
        ]),
        RChannelLabel::Rb => Some(&[
            "BR",
            "Rb",
            "Rrs",
            "BackRight",
            "RightBack",
            "RearRight",
            "RightRear",
        ]),
        RChannelLabel::Cb => Some(&["BC", "Cb", "BackCenter", "RearCenter"]),
        _ => None,
    }
}

pub(crate) fn fallback_virtual_bed_pose(
    label: RChannelLabel,
    use_7_1: bool,
) -> Option<(String, f32, f32, f32)> {
    let (name, az, el, dist) = match label {
        RChannelLabel::L => ("FL", if use_7_1 { -26.0 } else { -30.0 }, 0.0, 2.0),
        RChannelLabel::R => ("FR", if use_7_1 { 26.0 } else { 30.0 }, 0.0, 2.0),
        RChannelLabel::C => ("C", 0.0, 0.0, 2.0),
        RChannelLabel::LFE | RChannelLabel::LFE2 => ("LFE", 0.0, 0.0, 1.0),
        RChannelLabel::Ls => ("SL", if use_7_1 { -100.0 } else { -110.0 }, 0.0, 1.0),
        RChannelLabel::Rs => ("SR", if use_7_1 { 100.0 } else { 110.0 }, 0.0, 1.0),
        RChannelLabel::Lb => ("BL", -142.5, 0.0, 1.0),
        RChannelLabel::Rb => ("BR", 142.5, 0.0, 1.0),
        RChannelLabel::Cb => ("BC", 180.0, 0.0, 1.0),
        _ => return None,
    };
    Some((name.to_string(), az, el, dist))
}

pub(crate) fn resolve_virtual_bed_pose(
    label: RChannelLabel,
    use_7_1: bool,
) -> Option<(String, f32, f32, f32)> {
    let layouts = virtual_bed_layouts();
    let layout_opt = if use_7_1 {
        layouts.layout_7_1.as_ref()
    } else {
        layouts.layout_5_1.as_ref()
    };

    if let (Some(layout), Some(aliases)) = (layout_opt, label_aliases(label, use_7_1)) {
        if let Some(found) = find_speaker_in_layout(layout, aliases) {
            return Some(found);
        }
    }

    fallback_virtual_bed_pose(label, use_7_1)
}

pub struct WriterState {
    pub fail_level: Level,
}

#[derive(Clone)]
pub struct RuntimeOutputState {
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    pub output_device: Option<String>,
    #[cfg(target_os = "linux")]
    pub pw_buffer_config: PipewireBufferConfig,
    pub adaptive_resampling_config: AdaptiveResamplingConfig,
    #[cfg(target_os = "windows")]
    pub asio_target_latency_ms: u32,
    /// Works for both ASIO (Windows) and PipeWire (Linux)
    pub output_sample_rate: Option<u32>,
    pub enable_adaptive_resampling: bool,
}

impl Default for RuntimeOutputState {
    fn default() -> Self {
        Self {
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            output_device: None,
            #[cfg(target_os = "linux")]
            pw_buffer_config: PipewireBufferConfig::default(),
            adaptive_resampling_config: AdaptiveResamplingConfig::default(),
            #[cfg(target_os = "windows")]
            asio_target_latency_ms: 220,
            output_sample_rate: None,
            enable_adaptive_resampling: false,
        }
    }
}

pub struct TelemetryState {
    pub osc_sender: Option<OscSender>,
    pub audio_meter: Option<AudioMeter>,
}

impl Default for TelemetryState {
    fn default() -> Self {
        Self {
            osc_sender: None,
            audio_meter: None,
        }
    }
}

pub struct SpatialState {
    pub has_objects: bool,
    pub bed_indices: Option<Vec<usize>>,
    pub object_names: std::collections::HashMap<u32, String>,
    pub au_index: u64,
    pub segment_index: u32,
    /// Track if we're in segmented mode.
    pub is_segmented: bool,
    /// Sample position when current segment started.
    pub segment_start_samples: u64,
    /// Pending spatial events for next render_frame call.
    pub frame_events: Vec<renderer::spatial_renderer::SpatialChannelEvent>,
    /// Track if loudness metadata correction has been applied.
    pub loudness_applied: bool,
    /// Coordinate representation used by bridge metadata events.
    pub coordinate_format: RCoordinateFormat,
}

impl Default for SpatialState {
    fn default() -> Self {
        Self {
            has_objects: false,
            bed_indices: None,
            object_names: std::collections::HashMap::new(),
            au_index: 0,
            segment_index: 0,
            is_segmented: false,
            segment_start_samples: 0,
            frame_events: Vec::new(),
            loudness_applied: false,
            coordinate_format: RCoordinateFormat::Cartesian,
        }
    }
}

pub struct OutputState {
    pub audio_writer: Option<AudioWriter>,
    /// Number of decoded frames seen since the current writer bootstrap started.
    pub bootstrap_frames_seen: u32,
    /// Timestamp of the current writer bootstrap attempt.
    pub bootstrap_started_at: Option<Instant>,
    /// Reusable output buffer donated to render_frame and returned via RenderedFrame::samples.
    /// Eliminates per-frame heap allocation after the first rendered frame.
    pub render_buf: Vec<f32>,
    /// Reusable scratch buffer for i32 -> f32 PCM conversion.
    pub pcm_f32_buf: Vec<f32>,
    /// Set when output backend initialization failed; cleared when a config
    /// change is applied via invalidate_writer(). While true, init is skipped
    /// so the renderer waits silently instead of spamming errors.
    pub output_init_failed: bool,
    /// Last audio delay written to /tmp/omniphony_delay, in ms.
    pub last_audio_delay_written_ms: Option<f32>,
    /// Last audio delay we attempted to publish to /tmp/omniphony_delay, in ms.
    pub last_audio_delay_attempted_ms: Option<f32>,
    /// Throttle repeated delay-write warnings to avoid log floods.
    pub last_audio_delay_write_error_at: Option<Instant>,
    /// Last announced output sample rate (for OSC state de-duplication).
    pub last_audio_sample_rate_hz: Option<u32>,
    /// Last announced output sample format label.
    pub last_audio_sample_format: Option<String>,
}

impl Default for OutputState {
    fn default() -> Self {
        Self {
            audio_writer: None,
            bootstrap_frames_seen: 0,
            bootstrap_started_at: None,
            render_buf: Vec::new(),
            pcm_f32_buf: Vec::new(),
            output_init_failed: false,
            last_audio_delay_written_ms: None,
            last_audio_delay_attempted_ms: None,
            last_audio_delay_write_error_at: None,
            last_audio_sample_rate_hz: None,
            last_audio_sample_format: None,
        }
    }
}

impl OutputState {
    /// Drop the current audio writer and clear the init-failed flag so the
    /// next decoded frame will attempt to (re-)initialize the output backend.
    /// Call this whenever a runtime config change is applied.
    pub fn invalidate_writer(&mut self) -> Option<AudioWriter> {
        self.output_init_failed = false;
        self.audio_writer.take()
    }
}

pub struct DecodeSessionState {
    pub decoded_frames: u64,
    pub decoded_samples: u64,
    pub final_sample_rate: u32,
    pub last_frame_received_at: Option<Instant>,
    pub last_frame_sample_count: Option<u32>,
}

impl Default for DecodeSessionState {
    fn default() -> Self {
        Self {
            decoded_frames: 0,
            decoded_samples: 0,
            final_sample_rate: 48000,
            last_frame_received_at: None,
            last_frame_sample_count: None,
        }
    }
}

pub struct DecodeHandler {
    pub output: OutputState,
    pub telemetry: TelemetryState,
    pub runtime: RuntimeOutputState,
    pub spatial: SpatialState,
    pub session: DecodeSessionState,
    pub spatial_renderer: Option<renderer::spatial_renderer::SpatialRenderer>,
    pub audio_control: Option<Arc<AudioControl>>,
}

impl Default for DecodeHandler {
    fn default() -> Self {
        Self {
            output: OutputState::default(),
            telemetry: TelemetryState::default(),
            runtime: RuntimeOutputState::default(),
            spatial: SpatialState::default(),
            session: DecodeSessionState::default(),
            spatial_renderer: None,
            audio_control: None,
        }
    }
}

pub struct FrameHandlerContext<'a> {
    pub output_backend: OutputBackend,
    pub state: &'a WriterState,
    pub bed_conform: bool,
    pub use_loudness: bool,
    pub decode_time_ms: f32,
    pub queue_delay_ms: f32,
}

impl DecodeHandler {
    pub fn handle_decoded_frame(
        &mut self,
        frame: RDecodedFrame,
        ctx: &FrameHandlerContext,
    ) -> Result<()> {
        let now = Instant::now();
        let sample_rate = frame.sampling_frequency;
        let channel_count = frame.channel_count as usize;
        let sample_count = frame.sample_count as usize;
        let sample_count_u32 = frame.sample_count;
        let metadata_count = frame.metadata.len();

        if let Some(prev_at) = self.session.last_frame_received_at {
            let wall_gap_ms = now.saturating_duration_since(prev_at).as_secs_f64() * 1000.0;
            let frame_duration_ms = sample_count as f64 / sample_rate.max(1) as f64 * 1000.0;
            let sample_count_changed = self
                .session
                .last_frame_sample_count
                .is_some_and(|prev| prev != sample_count_u32);
            let severe_gap_ms = (frame_duration_ms * 20.0).max(100.0);
            let suspicious_metadata_change = metadata_count > 0 && sample_count_changed;
            if wall_gap_ms > severe_gap_ms || suspicious_metadata_change {
                log::warn!(
                    "Decoded frame cadence anomaly: samples={} ch={} sr={} metadata={} decode_ms={:.3} queue_ms={:.3} wall_gap_ms={:.3} frame_ms={:.3} prev_samples={:?}",
                    sample_count,
                    channel_count,
                    sample_rate,
                    metadata_count,
                    ctx.decode_time_ms,
                    ctx.queue_delay_ms,
                    wall_gap_ms,
                    frame_duration_ms,
                    self.session.last_frame_sample_count
                );
            } else if wall_gap_ms > (frame_duration_ms * 1.5).max(15.0) {
                log::debug!(
                    "Decoded frame burst cadence: samples={} ch={} sr={} metadata={} decode_ms={:.3} queue_ms={:.3} wall_gap_ms={:.3} frame_ms={:.3} prev_samples={:?}",
                    sample_count,
                    channel_count,
                    sample_rate,
                    metadata_count,
                    ctx.decode_time_ms,
                    ctx.queue_delay_ms,
                    wall_gap_ms,
                    frame_duration_ms,
                    self.session.last_frame_sample_count
                );
            }
        }
        self.session.last_frame_received_at = Some(now);
        self.session.last_frame_sample_count = Some(sample_count_u32);

        self.session.decoded_frames += 1u64;
        self.session.final_sample_rate = sample_rate;
        self.spatial.au_index += 1;

        // Apply dialogue normalisation from bridge (updated on major sync frames).
        // The level is always stored so OSC clients receive loudness/source
        // and loudness/gain regardless of whether --use-loudness is set.
        if !self.spatial.loudness_applied {
            if let Some(dialogue_level) = frame.dialogue_level.into_option() {
                if let Some(ref renderer) = self.spatial_renderer {
                    renderer.set_loudness(dialogue_level);
                    self.spatial.loudness_applied = true;
                    if self
                        .telemetry
                        .osc_sender
                        .as_ref()
                        .is_some_and(|sender| sender.has_osc_clients())
                    {
                        let osc_sender = self
                            .telemetry
                            .osc_sender
                            .as_ref()
                            .expect("osc_sender present");
                        osc_sender.send_loudness_state();
                    }
                }
            }
        }

        SpatialMetadataCoordinator::new(
            &mut self.spatial,
            self.spatial_renderer.as_ref(),
            self.telemetry.osc_sender.as_mut(),
        )
        .handle_spatial_metadata(&frame, frame.sampling_frequency)?;

        self.session.decoded_samples += sample_count as u64;

        let effective_channel_count = if ctx.bed_conform && self.spatial.has_objects {
            let empty_vec = Vec::new();
            let bed_indices = self.spatial.bed_indices.as_ref().unwrap_or(&empty_vec);
            ChannelCountCalculator::calculate_conformed_channel_count(channel_count, bed_indices)
        } else if let Some(ref renderer) = self.spatial_renderer {
            // When VBAP is active, output channel count is the number of speakers
            renderer.num_speakers()
        } else {
            channel_count
        };

        OutputRuntimeCoordinator::new(
            &mut self.output,
            &mut self.runtime,
            self.audio_control.as_deref(),
        )
        .sync_all(ctx.output_backend)?;
        WriterLifecycleCoordinator::new(
            &mut self.output,
            &self.runtime,
            &mut self.telemetry,
            &self.spatial,
            &self.session,
            self.spatial_renderer.as_ref(),
            self.audio_control.as_ref(),
        )
        .create_audio_writer_if_needed(ctx.output_backend, sample_rate, effective_channel_count)?;
        WriterLifecycleCoordinator::new(
            &mut self.output,
            &self.runtime,
            &mut self.telemetry,
            &self.spatial,
            &self.session,
            self.spatial_renderer.as_ref(),
            self.audio_control.as_ref(),
        )
        .publish_audio_state_if_changed(ctx.output_backend, sample_rate);

        let mut sample_write = SampleWriteCoordinator::new(
            &mut self.output,
            &mut self.telemetry,
            &mut self.spatial,
            &self.session,
            self.spatial_renderer.as_mut(),
        );
        if ctx.bed_conform && sample_write.spatial_has_objects() {
            sample_write.write_audio_samples_bed_conform(&frame, ctx.decode_time_ms)?;
        } else {
            sample_write.write_audio_samples(&frame, ctx.decode_time_ms)?;
        }

        Ok(())
    }

    pub fn finalize(&mut self) -> Result<()> {
        if let Some(ref mut writer) = self.output.audio_writer {
            writer.finish()?;
        }

        Ok(())
    }

    pub fn handle_stream_restart(
        &mut self,
        output_backend: OutputBackend,
        sample_rate: u32,
        channel_count: usize,
        bed_conform: bool,
    ) -> Result<()> {
        log::info!(
            "Stream restart detected at AU {}, resetting realtime output state",
            self.spatial.au_index
        );

        if let Some(mut writer) = self.output.invalidate_writer() {
            writer.flush()?;
        }
        self.output.bootstrap_frames_seen = 0;
        self.output.bootstrap_started_at = None;

        let effective_channel_count = if bed_conform && self.spatial.has_objects {
            let empty_vec = Vec::new();
            let bed_indices = self.spatial.bed_indices.as_ref().unwrap_or(&empty_vec);
            ChannelCountCalculator::calculate_conformed_channel_count(channel_count, bed_indices)
        } else {
            channel_count
        };
        self.output.audio_writer = Some(
            WriterLifecycleCoordinator::new(
                &mut self.output,
                &self.runtime,
                &mut self.telemetry,
                &self.spatial,
                &self.session,
                self.spatial_renderer.as_ref(),
                self.audio_control.as_ref(),
            )
            .build_audio_writer(output_backend, sample_rate, effective_channel_count, None)?,
        );
        self.reset_spatial_state_for_segment();
        Ok(())
    }

    fn reset_spatial_state_for_segment(&mut self) {
        SpatialMetadataCoordinator::new(
            &mut self.spatial,
            self.spatial_renderer.as_ref(),
            None,
        )
        .reset_for_segment();
    }

    pub fn handle_decoder_flush_request(&mut self) {
        log::info!("Received flush request after decoder reset");
        self.reset_spatial_state_for_segment();
    }
}
