use super::output::{AudioSamples, AudioWriter};
use crate::cli::command::OutputBackend;
use crate::events::{Configuration, Event};
#[cfg(all(target_os = "linux", feature = "pipewire"))]
use audio_output::pipewire::{PipewireAdaptiveResamplingConfig, PipewireBufferConfig};
#[cfg(all(target_os = "windows", feature = "asio"))]
use audio_output::AdaptiveResamplingConfig;
use bridge_api::{RChannelLabel, RCoordinateFormat, RDecodedFrame, RMetadataFrame};

use anyhow::{Result, anyhow};
use log::Level;
use renderer::metering::AudioMeter;
use renderer::osc_output::{ObjectMeta, OscSender};
use renderer::speaker_layout::SpeakerLayout;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Instant;

struct BedChannelMapper;

struct ChannelCountCalculator;

impl ChannelCountCalculator {
    const TARGET_BED_CHANNELS: usize = 10; // 7.1.2 layout

    /// Calculate the effective channel count for bed conformance
    /// Returns (num_bed_channels, num_object_channels, conformed_channel_count)
    fn calculate_bed_conform_counts(
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
    fn calculate_conformed_channel_count(
        original_channel_count: usize,
        bed_indices: &[usize],
    ) -> usize {
        let (_, _, conformed_count) =
            Self::calculate_bed_conform_counts(original_channel_count, bed_indices);
        conformed_count
    }
}

impl BedChannelMapper {
    fn apply_bed_conformance(
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

    fn apply_bed_conformance_to_frame(
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

fn build_virtual_bed_events(
    channel_labels: &[RChannelLabel],
    room_ratio: [f32; 3],
    room_ratio_rear: f32,
    room_ratio_lower: f32,
    room_ratio_center_blend: f32,
) -> Option<Vec<renderer::spatial_renderer::SpatialChannelEvent>> {
    let has_back = channel_labels
        .iter()
        .any(|l| matches!(l, RChannelLabel::Lb | RChannelLabel::Rb | RChannelLabel::Cb));
    let use_7_1 = has_back;

    let mut events: Vec<renderer::spatial_renderer::SpatialChannelEvent> =
        Vec::with_capacity(channel_labels.len());

    for (channel_idx, label) in channel_labels.iter().enumerate() {
        let (_name, az_deg, el_deg, dist_m) = match resolve_virtual_bed_pose(*label, use_7_1) {
            Some(v) => v,
            None => continue,
        };

        let (sx, sy, sz) = renderer::spatial_vbap::spherical_to_adm(az_deg, el_deg, dist_m);
        let (x, y, z) = inverse_room_ratio_map_for_virtual_object(
            sx,
            sy,
            sz,
            room_ratio,
            room_ratio_rear,
            room_ratio_lower,
            room_ratio_center_blend,
        );
        events.push(renderer::spatial_renderer::SpatialChannelEvent {
            channel_idx,
            is_bed: false,
            gain_db: Some(0),
            ramp_length: Some(0),
            spread: None,
            position: Some([x as f64, y as f64, z as f64]),
            sample_pos: Some(0),
        });
    }

    if events.is_empty() {
        None
    } else {
        Some(events)
    }
}

fn build_virtual_bed_objects(
    channel_labels: &[RChannelLabel],
    room_ratio: [f32; 3],
    room_ratio_rear: f32,
    room_ratio_lower: f32,
    room_ratio_center_blend: f32,
) -> Option<Vec<ObjectMeta>> {
    let has_back = channel_labels
        .iter()
        .any(|l| matches!(l, RChannelLabel::Lb | RChannelLabel::Rb | RChannelLabel::Cb));
    let use_7_1 = has_back;

    let mut objects: Vec<ObjectMeta> = Vec::with_capacity(channel_labels.len());
    for label in channel_labels {
        let (name, az_deg, el_deg, dist_m) = match resolve_virtual_bed_pose(*label, use_7_1) {
            Some(v) => v,
            None => continue,
        };
        let (sx, sy, sz) = renderer::spatial_vbap::spherical_to_adm(az_deg, el_deg, dist_m);
        let (x, y, z) = inverse_room_ratio_map_for_virtual_object(
            sx,
            sy,
            sz,
            room_ratio,
            room_ratio_rear,
            room_ratio_lower,
            room_ratio_center_blend,
        );
        objects.push(ObjectMeta {
            name,
            x,
            y,
            z,
            direct_speaker_index: None,
            gain: 0,
            priority: 0.0,
            divergence: 0.0,
        });
    }
    if objects.is_empty() {
        None
    } else {
        Some(objects)
    }
}

#[inline]
fn map_depth_with_room_ratios(
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

fn inverse_map_depth_with_room_ratios(
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

fn inverse_room_ratio_map_for_virtual_object(
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

fn find_speaker_in_layout(
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

fn label_aliases(label: RChannelLabel, use_7_1: bool) -> Option<&'static [&'static str]> {
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

fn fallback_virtual_bed_pose(
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

fn resolve_virtual_bed_pose(
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
    #[cfg(any(
        all(target_os = "linux", feature = "pipewire"),
        all(target_os = "windows", feature = "asio")
    ))]
    pub output_device: Option<String>,
    #[cfg(all(target_os = "linux", feature = "pipewire"))]
    pub pw_buffer_config: PipewireBufferConfig,
    #[cfg(all(target_os = "linux", feature = "pipewire"))]
    pub pw_adaptive_config: PipewireAdaptiveResamplingConfig,
    #[cfg(all(target_os = "windows", feature = "asio"))]
    pub asio_adaptive_config: AdaptiveResamplingConfig,
    /// Works for both ASIO (Windows) and PipeWire (Linux)
    pub output_sample_rate: Option<u32>,
    pub enable_adaptive_resampling: bool,
}

impl Default for RuntimeOutputState {
    fn default() -> Self {
        Self {
            #[cfg(any(
                all(target_os = "linux", feature = "pipewire"),
                all(target_os = "windows", feature = "asio")
            ))]
            output_device: None,
            #[cfg(all(target_os = "linux", feature = "pipewire"))]
            pw_buffer_config: PipewireBufferConfig::default(),
            #[cfg(all(target_os = "linux", feature = "pipewire"))]
            pw_adaptive_config: PipewireAdaptiveResamplingConfig::default(),
            #[cfg(all(target_os = "windows", feature = "asio"))]
            asio_adaptive_config: AdaptiveResamplingConfig::default(),
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
}

impl Default for DecodeSessionState {
    fn default() -> Self {
        Self {
            decoded_frames: 0,
            decoded_samples: 0,
            final_sample_rate: 48000,
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
        }
    }
}

pub struct FrameHandlerContext<'a> {
    pub output_backend: OutputBackend,
    pub state: &'a WriterState,
    pub bed_conform: bool,
    pub use_loudness: bool,
}

impl DecodeHandler {
    #[inline]
    fn normalize_azimuth_deg(mut azimuth_deg: f32) -> f32 {
        while azimuth_deg < -180.0 {
            azimuth_deg += 360.0;
        }
        while azimuth_deg > 180.0 {
            azimuth_deg -= 360.0;
        }
        azimuth_deg
    }

    fn event_pos_raw(_coordinate_format: RCoordinateFormat, event: &Event) -> Option<[f64; 3]> {
        let p = event.pos()?;
        if p.len() < 3 {
            return None;
        }
        Some([p[0], p[1], p[2]])
    }

    fn event_pos_as_adm_cartesian(
        coordinate_format: RCoordinateFormat,
        event: &Event,
    ) -> Option<[f64; 3]> {
        let p = event.pos()?;
        if p.len() < 3 {
            return None;
        }

        match coordinate_format {
            RCoordinateFormat::Cartesian => Some([p[0], p[1], p[2]]),
            RCoordinateFormat::Polar => {
                // Normalize/clamp bridge polar input to keep renderer behavior stable.
                let az = Self::normalize_azimuth_deg(p[0] as f32);
                let el = (p[1] as f32).clamp(-90.0, 90.0);
                let dist = (p[2] as f32).max(0.0);
                let (x, y, z) = renderer::spatial_vbap::spherical_to_adm(az, el, dist);
                Some([x as f64, y as f64, z as f64])
            }
        }
    }

    fn effective_audio_state(
        output_backend: OutputBackend,
        input_sample_rate: u32,
        _output_rate: Option<u32>,
    ) -> (u32, &'static str) {
        match output_backend {
            #[cfg(all(target_os = "linux", feature = "pipewire"))]
            OutputBackend::Pipewire => (_output_rate.unwrap_or(input_sample_rate), "f32le"),
            #[cfg(all(target_os = "windows", feature = "asio"))]
            OutputBackend::Asio => (_output_rate.unwrap_or(input_sample_rate), "f32le"),
            _ => (input_sample_rate, "s24le"),
        }
    }

    fn sync_requested_output_sample_rate(&mut self, output_backend: OutputBackend) -> Result<()> {
        let requested = self
            .spatial_renderer
            .as_ref()
            .map(|r| r.renderer_control().requested_output_sample_rate())
            .unwrap_or(self.runtime.output_sample_rate);

        if requested == self.runtime.output_sample_rate {
            return Ok(());
        }

        self.runtime.output_sample_rate = requested;
        log::info!(
            "Applying requested output sample rate: {}",
            requested
                .map(|v| v.to_string())
                .unwrap_or_else(|| "native".to_string())
        );

        // Recreate streaming backends to apply the new output rate.
        match output_backend {
            #[cfg(all(target_os = "linux", feature = "pipewire"))]
            OutputBackend::Pipewire => {
                if let Some(mut writer) = self.output.invalidate_writer() {
                    let _ = writer.flush();
                }
            }
            #[cfg(all(target_os = "windows", feature = "asio"))]
            OutputBackend::Asio => {
                if let Some(mut writer) = self.output.invalidate_writer() {
                    let _ = writer.flush();
                }
            }
            _ => {}
        }

        Ok(())
    }

    fn sync_requested_output_device(&mut self, output_backend: OutputBackend) -> Result<()> {
        let requested = self
            .spatial_renderer
            .as_ref()
            .map(|r| r.renderer_control().requested_output_device())
            .unwrap_or_else(|| self.runtime.output_device.clone());

        if requested == self.runtime.output_device {
            return Ok(());
        }

        self.runtime.output_device = requested.clone();
        log::info!(
            "Applying requested output device: {}",
            requested.unwrap_or_else(|| "default".to_string())
        );

        match output_backend {
            #[cfg(all(target_os = "linux", feature = "pipewire"))]
            OutputBackend::Pipewire => {
                if let Some(mut writer) = self.output.invalidate_writer() {
                    let _ = writer.flush();
                }
            }
            #[cfg(all(target_os = "windows", feature = "asio"))]
            OutputBackend::Asio => {
                if let Some(mut writer) = self.output.invalidate_writer() {
                    let _ = writer.flush();
                }
            }
            _ => {}
        }

        Ok(())
    }

    fn sync_requested_adaptive_resampling(&mut self, output_backend: OutputBackend) -> Result<()> {
        let requested = self
            .spatial_renderer
            .as_ref()
            .map(|r| r.renderer_control().requested_adaptive_resampling())
            .unwrap_or(self.runtime.enable_adaptive_resampling);

        if requested == self.runtime.enable_adaptive_resampling {
            return Ok(());
        }

        self.runtime.enable_adaptive_resampling = requested;
        log::info!(
            "Applying requested adaptive resampling: {}",
            if requested { "enabled" } else { "disabled" }
        );

        // Recreate streaming backends to apply the new adaptive mode.
        match output_backend {
            #[cfg(all(target_os = "linux", feature = "pipewire"))]
            OutputBackend::Pipewire => {
                if let Some(mut writer) = self.output.invalidate_writer() {
                    let _ = writer.flush();
                }
            }
            #[cfg(all(target_os = "windows", feature = "asio"))]
            OutputBackend::Asio => {
                if let Some(mut writer) = self.output.invalidate_writer() {
                    let _ = writer.flush();
                }
            }
            _ => {}
        }

        Ok(())
    }

    fn sync_requested_latency_target(&mut self, output_backend: OutputBackend) -> Result<()> {
        #[cfg(all(target_os = "linux", feature = "pipewire"))]
        {
            let requested = self
                .spatial_renderer
                .as_ref()
                .and_then(|r| r.renderer_control().requested_latency_target_ms())
                .unwrap_or(self.runtime.pw_buffer_config.latency_ms);

            if requested == self.runtime.pw_buffer_config.latency_ms {
                return Ok(());
            }

            self.runtime.pw_buffer_config.latency_ms = requested.max(1);
            if self.runtime.pw_buffer_config.max_latency_ms
                <= self.runtime.pw_buffer_config.latency_ms
            {
                self.runtime.pw_buffer_config.max_latency_ms =
                    self.runtime.pw_buffer_config.latency_ms.saturating_mul(2);
            }
            log::info!(
                "Applying requested latency target: {} ms (max={} ms)",
                self.runtime.pw_buffer_config.latency_ms,
                self.runtime.pw_buffer_config.max_latency_ms
            );

            if let OutputBackend::Pipewire = output_backend {
                if let Some(mut writer) = self.output.invalidate_writer() {
                    let _ = writer.flush();
                }
            }
        }

        #[cfg(not(all(target_os = "linux", feature = "pipewire")))]
        let _ = output_backend;

        Ok(())
    }

    fn sync_requested_adaptive_tuning(&mut self, output_backend: OutputBackend) -> Result<()> {
        #[cfg(all(target_os = "linux", feature = "pipewire"))]
        {
            let requested = self
                .spatial_renderer
                .as_ref()
                .map(|r| r.renderer_control())
                .map(|control| PipewireAdaptiveResamplingConfig {
                    kp_near: control.requested_adaptive_resampling_kp_near(),
                    kp_far: control.requested_adaptive_resampling_kp_far(),
                    ki: control.requested_adaptive_resampling_ki(),
                    max_adjust: control.requested_adaptive_resampling_max_adjust(),
                    max_adjust_far: control.requested_adaptive_resampling_max_adjust_far(),
                    near_far_threshold_ms: control
                        .requested_adaptive_resampling_near_far_threshold_ms(),
                    hard_correction_threshold_ms: control
                        .requested_adaptive_resampling_hard_correction_threshold_ms(),
                    measurement_smoothing_alpha: control
                        .requested_adaptive_resampling_measurement_smoothing_alpha(),
                })
                .unwrap_or_else(|| self.runtime.pw_adaptive_config.clone());

            if requested.kp_near == self.runtime.pw_adaptive_config.kp_near
                && requested.kp_far == self.runtime.pw_adaptive_config.kp_far
                && requested.ki == self.runtime.pw_adaptive_config.ki
                && requested.max_adjust == self.runtime.pw_adaptive_config.max_adjust
                && requested.max_adjust_far == self.runtime.pw_adaptive_config.max_adjust_far
                && requested.near_far_threshold_ms
                    == self.runtime.pw_adaptive_config.near_far_threshold_ms
                && requested.hard_correction_threshold_ms
                    == self.runtime.pw_adaptive_config.hard_correction_threshold_ms
                && requested.measurement_smoothing_alpha
                    == self.runtime.pw_adaptive_config.measurement_smoothing_alpha
            {
                return Ok(());
            }

            self.runtime.pw_adaptive_config = requested;
            log::info!(
                "Applying adaptive resampling tuning: kp_near={:.8}, kp_far={:.8}, ki={:.8}, max_adjust={:.6}, max_adjust_far={:.6}, near_far_threshold_ms={}, hard_correction_threshold_ms={}, measurement_smoothing_alpha={:.3}",
                self.runtime.pw_adaptive_config.kp_near,
                self.runtime.pw_adaptive_config.kp_far,
                self.runtime.pw_adaptive_config.ki,
                self.runtime.pw_adaptive_config.max_adjust,
                self.runtime.pw_adaptive_config.max_adjust_far,
                self.runtime.pw_adaptive_config.near_far_threshold_ms,
                self.runtime.pw_adaptive_config.hard_correction_threshold_ms,
                self.runtime.pw_adaptive_config.measurement_smoothing_alpha
            );

            if let OutputBackend::Pipewire = output_backend {
                if let Some(mut writer) = self.output.invalidate_writer() {
                    let _ = writer.flush();
                }
            }
        }

        #[cfg(not(all(target_os = "linux", feature = "pipewire")))]
        let _ = output_backend;

        Ok(())
    }

    fn publish_audio_state_if_changed(
        &mut self,
        output_backend: OutputBackend,
        input_sample_rate: u32,
    ) {
        let (effective_rate, sample_format) = Self::effective_audio_state(
            output_backend,
            input_sample_rate,
            self.runtime.output_sample_rate,
        );

        if self.output.last_audio_sample_rate_hz == Some(effective_rate)
            && self.output.last_audio_sample_format.as_deref() == Some(sample_format)
        {
            return;
        }

        self.output.last_audio_sample_rate_hz = Some(effective_rate);
        self.output.last_audio_sample_format = Some(sample_format.to_string());

        if let Some(ref renderer) = self.spatial_renderer {
            let control = renderer.renderer_control();
            control.set_audio_state(effective_rate, sample_format);
        }
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
            if let Err(e) = osc_sender.send_audio_state(effective_rate, sample_format) {
                log::warn!("Failed to send OSC audio state: {}", e);
            }
        }
    }

    pub fn handle_decoded_frame(
        &mut self,
        frame: RDecodedFrame,
        ctx: &FrameHandlerContext,
    ) -> Result<()> {
        let sample_rate = frame.sampling_frequency;
        let channel_count = frame.channel_count as usize;
        let sample_count = frame.sample_count as usize;

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

        self.handle_spatial_metadata(&frame, ctx.output_backend, ctx.state, ctx.bed_conform)?;

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

        self.sync_requested_output_device(ctx.output_backend)?;
        self.sync_requested_output_sample_rate(ctx.output_backend)?;
        self.sync_requested_adaptive_resampling(ctx.output_backend)?;
        self.sync_requested_latency_target(ctx.output_backend)?;
        self.sync_requested_adaptive_tuning(ctx.output_backend)?;
        self.create_audio_writer_if_needed(
            ctx.output_backend,
            sample_rate,
            effective_channel_count,
        )?;
        self.publish_audio_state_if_changed(ctx.output_backend, sample_rate);

        if ctx.bed_conform && self.spatial.has_objects {
            self.write_audio_samples_bed_conform(&frame)?;
        } else {
            self.write_audio_samples(&frame)?;
        }

        Ok(())
    }

    fn handle_spatial_metadata(
        &mut self,
        frame: &RDecodedFrame,
        _output_backend: OutputBackend,
        _state: &WriterState,
        _bed_conform: bool,
    ) -> Result<()> {
        if frame.metadata.is_empty() {
            return Ok(());
        }

        for meta in frame.metadata.iter() {
            let conf = Configuration::from(meta);
            self.spatial.has_objects = true;

            // Bed indices are provided explicitly by the bridge and may appear
            // only after object mode is already active. Apply them whenever a
            // metadata frame carries a non-empty set so the renderer does not
            // stay stuck in the temporary "all objects" fallback path.
            if !meta.bed_indices.is_empty() {
                let new_bed_indices: Vec<usize> = meta.bed_indices.iter().copied().collect();
                let changed = self.spatial.bed_indices.as_ref() != Some(&new_bed_indices);
                if changed {
                    self.spatial.bed_indices = Some(new_bed_indices);
                    log::debug!(
                        "Extracted bed indices from bridge metadata: {:?}",
                        self.spatial.bed_indices
                    );

                    if let (Some(renderer), Some(bi)) =
                        (&self.spatial_renderer, &self.spatial.bed_indices)
                    {
                        renderer.configure_beds(bi);
                    }
                }
            }

            self.handle_metadata_writing(meta, conf, frame.sampling_frequency)?;
        }
        Ok(())
    }

    fn convert_samples_to_bed_conform(
        &self,
        original_samples: Vec<i32>,
        original_channel_count: usize,
        _conformed_channel_count: usize,
    ) -> Vec<i32> {
        let empty_vec = Vec::new();
        let bed_indices = self.spatial.bed_indices.as_ref().unwrap_or(&empty_vec);
        BedChannelMapper::apply_bed_conformance(
            original_samples,
            original_channel_count,
            bed_indices,
        )
    }

    fn handle_metadata_writing(
        &mut self,
        meta: &RMetadataFrame,
        conf: Configuration,
        sample_rate: u32,
    ) -> Result<()> {
        let sample_pos = meta.sample_pos;

        // Calculate the relative sample position within the current segment.
        let segment_relative_sample_pos = if self.spatial.is_segmented {
            let relative_pos = sample_pos.saturating_sub(self.spatial.segment_start_samples);
            log::trace!(
                "Adjusting metadata sample position: absolute={}, segment_start={}, relative={}",
                sample_pos,
                self.spatial.segment_start_samples,
                relative_pos
            );
            relative_pos
        } else {
            sample_pos
        };
        let coordinate_format = self.spatial.coordinate_format;

        // Send via OSC only when an active OSC client exists.
        if self
            .telemetry
            .osc_sender
            .as_ref()
            .is_some_and(|sender| sender.has_osc_clients())
        {
            let osc_sender = self
                .telemetry
                .osc_sender
                .as_mut()
                .expect("osc_sender present");
            for upd in meta.name_updates.iter() {
                self.spatial
                    .object_names
                    .insert(upd.id, upd.name.to_string());
            }
            let active_layout = self
                .spatial_renderer
                .as_ref()
                .map(|renderer| renderer.speaker_layout());
            let bed_to_speaker = active_layout
                .as_ref()
                .map(|layout| layout.bed_to_speaker_mapping())
                .unwrap_or_default();
            let objects: Vec<ObjectMeta> = conf
                .events
                .iter()
                .enumerate()
                .map(|(idx, event)| {
                    let logical_id = event.id().unwrap_or(idx as u32);
                    let direct_speaker_index = if logical_id < 10 {
                        bed_to_speaker
                            .get(&(logical_id as usize))
                            .copied()
                            .map(|idx| idx as u32)
                    } else {
                        None
                    };
                    let [ox, oy, oz] = direct_speaker_index
                        .and_then(|speaker_idx| {
                            active_layout.as_ref().and_then(|layout| {
                                layout.speakers.get(speaker_idx as usize).map(|speaker| {
                                    let (x, y, z) = renderer::spatial_vbap::spherical_to_adm(
                                        speaker.azimuth,
                                        speaker.elevation,
                                        speaker.distance,
                                    );
                                    [x as f64, y as f64, z as f64]
                                })
                            })
                        })
                        .unwrap_or_else(|| {
                            Self::event_pos_raw(coordinate_format, event).unwrap_or([0.0, 0.0, 0.0])
                        });
                    ObjectMeta {
                        name: self
                            .spatial
                            .object_names
                            .get(&logical_id)
                            .cloned()
                            .unwrap_or_else(|| format!("Obj_{logical_id}")),
                        x: ox as f32,
                        y: oy as f32,
                        z: oz as f32,
                        direct_speaker_index,
                        gain: event.gain_db().map_or(-128, |g| g as i32),
                        // Event stream does not currently carry these fields.
                        priority: 0.0,
                        divergence: 0.0,
                    }
                })
                .collect();
            let ramp_duration = meta.ramp_duration;
            let osc_coord_format = match coordinate_format {
                RCoordinateFormat::Cartesian => 0,
                RCoordinateFormat::Polar => 1,
            };
            if let Err(e) = osc_sender.send_object_frame(
                segment_relative_sample_pos,
                ramp_duration,
                osc_coord_format,
                &objects,
            ) {
                log::warn!("Failed to send OSC metadata: {}", e);
            }
            let seconds = segment_relative_sample_pos as f64 / sample_rate as f64;
            if let Err(e) = osc_sender.send_timestamp(segment_relative_sample_pos, seconds) {
                log::warn!("Failed to send OSC timestamp: {}", e);
            }
        }

        // Convert DAMF events to format-agnostic SpatialChannelEvent and accumulate
        // for atomic application at the next render_frame() call.
        // The ID→channel mapping lives here (format-specific layer), not in the renderer.
        if self.spatial_renderer.is_some() {
            let bed_indices = self.spatial.bed_indices.as_deref().unwrap_or(&[]);
            let bed_id_to_channel: std::collections::HashMap<usize, usize> = bed_indices
                .iter()
                .enumerate()
                .map(|(idx, &bid)| (bid, idx))
                .collect();
            let num_beds = bed_indices.len();

            for event in &conf.events {
                let object_id = match event.id() {
                    Some(id) => id as usize,
                    None => continue,
                };
                let (channel_idx, is_bed) = if object_id < 10 {
                    match bed_id_to_channel.get(&object_id) {
                        Some(&ch) => (ch, true),
                        None => continue,
                    }
                } else {
                    (num_beds + (object_id - 10), false)
                };
                self.spatial
                    .frame_events
                    .push(renderer::spatial_renderer::SpatialChannelEvent {
                        channel_idx,
                        is_bed,
                        gain_db: event.gain_db(),
                        ramp_length: event.ramp_length(),
                        // Ignore per-event bridge spread. Runtime spread comes from live spread settings.
                        spread: None,
                        position: Self::event_pos_as_adm_cartesian(coordinate_format, event),
                        sample_pos: event.sample_pos,
                    });
            }
        }

        Ok(())
    }

    fn create_audio_writer_if_needed(
        &mut self,
        output_backend: OutputBackend,
        sample_rate: u32,
        channel_count: usize,
    ) -> Result<()> {
        #[cfg(not(any(
            all(target_os = "linux", feature = "pipewire"),
            all(target_os = "windows", feature = "asio")
        )))]
        let _ = (output_backend, sample_rate, channel_count);

        if self.output.audio_writer.is_none() && !self.output.output_init_failed {
            #[cfg(all(target_os = "linux", feature = "pipewire"))]
            if output_backend == OutputBackend::Pipewire {
                // With VBAP active, do not start PipeWire on the first decoded
                // frames before we know whether the stream carries objects.
                // For object streams we wait until explicit bed/object metadata
                // has arrived and stabilized a little. For non-object content we
                // allow a short fallback bootstrap after a handful of frames.
                if self.spatial_renderer.is_some() {
                    self.output
                        .bootstrap_started_at
                        .get_or_insert_with(Instant::now);
                    if !self.spatial.has_objects {
                        self.output.bootstrap_frames_seen =
                            self.output.bootstrap_frames_seen.saturating_add(1);
                        if self.output.bootstrap_frames_seen < 8 {
                            return Ok(());
                        }
                    } else {
                        if self.spatial.bed_indices.is_none() {
                            self.output.bootstrap_frames_seen = 0;
                            return Ok(());
                        }
                        self.output.bootstrap_frames_seen =
                            self.output.bootstrap_frames_seen.saturating_add(1);
                        if self.output.bootstrap_frames_seen < 3 {
                            return Ok(());
                        }
                    }
                }

                log::info!(
                    "Creating PipeWire audio stream: {} Hz, {} channels (bootstrap_frames={}, has_objects={}, bed_indices={:?}, decoded_frames={}, decoded_samples={}, bootstrap_elapsed_ms={:.0})",
                    sample_rate,
                    channel_count,
                    self.output.bootstrap_frames_seen,
                    self.spatial.has_objects,
                    self.spatial.bed_indices,
                    self.session.decoded_frames,
                    self.session.decoded_samples,
                    self.output
                        .bootstrap_started_at
                        .map(|t| t.elapsed().as_secs_f64() * 1000.0)
                        .unwrap_or(0.0)
                );

                // If VBAP is active, use speaker names for channel labels
                let speaker_names = if let Some(ref renderer) = self.spatial_renderer {
                    Some(
                        renderer
                            .speaker_names()
                            .iter()
                            .map(|s| s.to_string())
                            .collect(),
                    )
                } else {
                    None
                };
                match self.build_audio_writer(output_backend, sample_rate, channel_count, speaker_names) {
                    Ok(writer) => {
                        self.output.audio_writer = Some(writer);
                        self.output.bootstrap_frames_seen = 0;
                        self.output.bootstrap_started_at = None;
                    }
                    Err(e) => {
                        log::warn!("Output backend initialization failed, waiting for a valid config: {}", e);
                        self.output.output_init_failed = true;
                    }
                }
                return Ok(());
            }

            #[cfg(all(target_os = "windows", feature = "asio"))]
            if output_backend == OutputBackend::Asio {
                // Use output_sample_rate if specified, otherwise use stream's native sample_rate
                let effective_sample_rate = self.runtime.output_sample_rate.unwrap_or(sample_rate);

                if let Some(output_rate) = self.runtime.output_sample_rate {
                    log::info!(
                        "Creating ASIO audio stream with upsampling: {} Hz -> {} Hz, {} channels",
                        sample_rate,
                        output_rate,
                        channel_count
                    );
                } else {
                    log::info!(
                        "Creating ASIO audio stream: {} Hz, {} channels",
                        sample_rate,
                        channel_count
                    );
                }

                match self.build_audio_writer(output_backend, effective_sample_rate, channel_count, None) {
                    Ok(writer) => {
                        self.output.audio_writer = Some(writer);
                    }
                    Err(e) => {
                        log::warn!("Output backend initialization failed, waiting for a valid config: {}", e);
                        self.output.output_init_failed = true;
                    }
                }
                return Ok(());
            }
        }
        Ok(())
    }

    fn build_audio_writer(
        &self,
        output_backend: OutputBackend,
        sample_rate: u32,
        channel_count: usize,
        #[cfg(all(target_os = "linux", feature = "pipewire"))] pipewire_channel_names: Option<
            Vec<String>,
        >,
        #[cfg(not(all(target_os = "linux", feature = "pipewire")))] _pipewire_channel_names: Option<
            Vec<String>,
        >,
    ) -> Result<AudioWriter> {
        #[cfg(not(any(
            all(target_os = "linux", feature = "pipewire"),
            all(target_os = "windows", feature = "asio")
        )))]
        let _ = (output_backend, sample_rate, channel_count);

        match output_backend {
            #[cfg(all(target_os = "linux", feature = "pipewire"))]
            OutputBackend::Pipewire => {
                if let Some(names) = pipewire_channel_names {
                    Ok(AudioWriter::create_pipewire_with_channel_names(
                        sample_rate,
                        channel_count as u32,
                        self.runtime.output_device.clone(),
                        names,
                        self.runtime.enable_adaptive_resampling,
                        self.runtime.output_sample_rate,
                        self.runtime.pw_buffer_config.clone(),
                        self.runtime.pw_adaptive_config.clone(),
                    )?)
                } else {
                    Ok(AudioWriter::create_pipewire(
                        sample_rate,
                        channel_count as u32,
                        self.runtime.output_device.clone(),
                        self.runtime.enable_adaptive_resampling,
                        self.runtime.output_sample_rate,
                        self.runtime.pw_buffer_config.clone(),
                        self.runtime.pw_adaptive_config.clone(),
                    )?)
                }
            }
            #[cfg(all(target_os = "windows", feature = "asio"))]
            OutputBackend::Asio => {
                let effective_sample_rate = self.runtime.output_sample_rate.unwrap_or(sample_rate);
                Ok(AudioWriter::create_asio(
                    sample_rate,
                    effective_sample_rate,
                    channel_count as u32,
                    self.runtime.output_device.clone(),
                    self.runtime.enable_adaptive_resampling,
                    self.runtime.asio_adaptive_config.clone(),
                )?)
            }
            OutputBackend::Unsupported => Err(anyhow!("No supported realtime output backend")),
        }
    }

    fn write_audio_samples(&mut self, frame: &RDecodedFrame) -> Result<()> {
        let channel_count = frame.channel_count as usize;
        let sample_count = frame.sample_count as usize;

        // Read latency and resample ratio before acquiring mutable borrow on audio_writer.
        // Expose comparable total delays for telemetry:
        // - measured total latency: current ring latency + graph delay
        // - target total latency: configured ring target + graph delay
        let current_latency_instant_ms: Option<f32> = self
            .output
            .audio_writer
            .as_ref()
            .and_then(|w| w.measured_audio_delay_ms());
        let current_latency_control_ms: Option<f32> = self
            .output
            .audio_writer
            .as_ref()
            .and_then(|w| w.control_audio_delay_ms());
        let current_latency_target_ms: Option<f32> = self
            .output
            .audio_writer
            .as_ref()
            .and_then(|w| w.total_audio_delay_ms());
        let current_resample_ratio: Option<f32> = self
            .output
            .audio_writer
            .as_ref()
            .and_then(|w| w.resample_ratio());
        let current_adaptive_band: Option<&'static str> = self
            .output
            .audio_writer
            .as_ref()
            .and_then(|w| w.adaptive_band());

        // Keep omniphony_delay updated when total delay drifts enough.
        // Prefer the configured target delay (stable sync reference for mpv),
        // fall back to measured delay when target is unavailable.
        // TODO: find a cleaner IPC mechanism to communicate latency to the player
        // instead of writing to a temp file (e.g. OSC, named pipe, shared memory).
        if let Some(total_ms) = self.output.audio_writer.as_ref().and_then(|w| {
            w.total_audio_delay_ms()
                .or_else(|| w.measured_audio_delay_ms())
        }) {
            let should_write = self
                .output
                .last_audio_delay_written_ms
                .map(|prev| (total_ms - prev).abs() >= 20.0)
                .unwrap_or(true);
            if should_write {
                let delay_s = -(total_ms / 1000.0);
                let delay_path = std::env::temp_dir().join("omniphony_delay");
                if let Err(e) = std::fs::write(&delay_path, format!("{:.4}\n", delay_s)) {
                    log::warn!("Could not write {}: {}", delay_path.display(), e);
                } else {
                    self.output.last_audio_delay_written_ms = Some(total_ms);
                }
            }
        }

        if self.output.audio_writer.is_some() {
            let mut pcm_f32_scratch = std::mem::take(&mut self.output.pcm_f32_buf);
            // Check if we should use VBAP spatial rendering.
            if let Some(ref mut renderer) = self.spatial_renderer {
                log::trace!(
                    "VBAP check: has_objects={}, metadata.len()={}, channel_count={}",
                    self.spatial.has_objects,
                    frame.metadata.len(),
                    channel_count
                );

                if !frame.metadata.is_empty() {
                    log::trace!(
                        "Processed {} metadata payload(s) via bridge",
                        frame.metadata.len()
                    );
                }

                if self.spatial.has_objects {
                    log::trace!(
                        "Using VBAP spatial rendering (metadata source: {})",
                        if frame.metadata.is_empty() {
                            "cached"
                        } else {
                            "current frame"
                        }
                    );

                    // Convert 24-bit LSB-aligned i32 samples to f32 [-1.0, 1.0]
                    // using a reusable scratch buffer.
                    fill_pcm_f32_reuse(&mut pcm_f32_scratch, &frame.pcm);
                    let pcm_data_f32 = &pcm_f32_scratch;

                    // Meter object levels — iterate by channel_count-sized chunks.
                    if self
                        .telemetry
                        .osc_sender
                        .as_ref()
                        .is_some_and(|sender| sender.has_metering_clients())
                    {
                        if let Some(ref mut meter) = self.telemetry.audio_meter {
                        meter.update_channel_count(channel_count);
                        for chunk in pcm_data_f32.chunks_exact(channel_count) {
                            meter.process_objects(chunk, channel_count);
                        }
                    }
                    }

                    // Render using VBAP. Pending metadata events (if any) are applied
                    // atomically at the start of render_frame, guaranteeing correct ordering.
                    let pending_events = std::mem::take(&mut self.spatial.frame_events);
                    // Donate the previous frame's buffer so render_frame can reuse its
                    // allocation without a new heap alloc (buffer donation pattern).
                    let donated_buf = std::mem::take(&mut self.output.render_buf);
                    let rendered = renderer.render_frame(
                        &pcm_data_f32,
                        channel_count,
                        &pending_events,
                        donated_buf,
                    )?;

                    let num_speakers = renderer.num_speakers();

                    // Meter speaker levels and send OSC bundle if interval elapsed.
                    let meter_snapshot = if self
                        .telemetry
                        .osc_sender
                        .as_ref()
                        .is_some_and(|sender| sender.has_metering_clients())
                    {
                        self.telemetry.audio_meter.as_mut().and_then(|m| {
                            m.process_speakers(&rendered.samples, num_speakers);
                            m.poll()
                        })
                    } else {
                        None
                    };
                    if let (Some(snapshot), Some(osc_sender)) =
                        (meter_snapshot, &self.telemetry.osc_sender)
                    {
                        if let Err(e) = osc_sender.send_meter_bundle(
                            &snapshot,
                            &rendered.object_gains,
                            current_latency_instant_ms,
                            current_latency_control_ms,
                            current_latency_target_ms,
                            current_resample_ratio,
                            current_adaptive_band,
                        ) {
                            log::warn!("Failed to send meter OSC bundle: {}", e);
                        }
                    }

                    log::trace!(
                        "Writing {} samples ({} sample_count × {} speakers) to streaming output",
                        rendered.samples.len(),
                        sample_count,
                        num_speakers
                    );

                    // Wrap in AudioSamples without moving ownership so we can reclaim
                    // the Vec after the write (write_pcm_samples takes &AudioSamples).
                    let samples_audio = AudioSamples::F32(rendered.samples);
                    self.output
                        .audio_writer
                        .as_mut()
                        .expect("audio_writer present")
                        .write_pcm_samples(&samples_audio, num_speakers)?;
                    // Reclaim the Vec for the next frame (no allocation needed).
                    self.output.render_buf = match samples_audio {
                        AudioSamples::F32(v) => v,
                        _ => unreachable!(),
                    };
                    self.output.pcm_f32_buf = pcm_f32_scratch;
                    return Ok(());
                } else {
                    // No object metadata: synthesize fixed-position virtual objects
                    // from the canonical bed layouts (5.1 / 7.1) and render via VBAP.
                    let labels: Vec<RChannelLabel> = frame.channel_labels.iter().copied().collect();
                    let (room_ratio, room_ratio_rear, room_ratio_lower, room_ratio_center_blend) = {
                        let control = renderer.renderer_control();
                        let live = control.live.read().unwrap();
                        (
                            live.room_ratio,
                            live.room_ratio_rear,
                            live.room_ratio_lower,
                            live.room_ratio_center_blend,
                        )
                    };
                    let virtual_events = match build_virtual_bed_events(
                        &labels,
                        room_ratio,
                        room_ratio_rear,
                        room_ratio_lower,
                        room_ratio_center_blend,
                    ) {
                        Some(v) => v,
                        None => {
                            log::warn!(
                                "No virtual bed VBAP map for channel labels {:?} - outputting silence",
                                labels
                            );
                            let num_speakers = renderer.num_speakers();
                            self.output
                                .audio_writer
                                .as_mut()
                                .expect("audio_writer present")
                                .write_pcm_samples(
                                    &AudioSamples::I32(vec![0i32; sample_count * num_speakers]),
                                    num_speakers,
                                )?;
                            self.output.pcm_f32_buf = pcm_f32_scratch;
                            return Ok(());
                        }
                    };

                    fill_pcm_f32_reuse(&mut pcm_f32_scratch, &frame.pcm);
                    let pcm_data_f32 = &pcm_f32_scratch;

                    // Meter virtual object levels from the decoded bed channels.
                    if self
                        .telemetry
                        .osc_sender
                        .as_ref()
                        .is_some_and(|sender| sender.has_metering_clients())
                    {
                        if let Some(ref mut meter) = self.telemetry.audio_meter {
                        meter.update_channel_count(channel_count);
                        for chunk in pcm_data_f32.chunks_exact(channel_count) {
                            meter.process_objects(chunk, channel_count);
                        }
                    }
                    }

                    let donated_buf = std::mem::take(&mut self.output.render_buf);
                    let rendered = renderer.render_frame(
                        &pcm_data_f32,
                        channel_count,
                        &virtual_events,
                        donated_buf,
                    )?;
                    let num_speakers = renderer.num_speakers();

                    // Meter speaker levels and emit the OSC meter bundle.
                    let meter_snapshot = if self
                        .telemetry
                        .osc_sender
                        .as_ref()
                        .is_some_and(|sender| sender.has_metering_clients())
                    {
                        self.telemetry.audio_meter.as_mut().and_then(|m| {
                            m.process_speakers(&rendered.samples, num_speakers);
                            m.poll()
                        })
                    } else {
                        None
                    };
                    if let (Some(snapshot), Some(osc_sender)) =
                        (meter_snapshot, &self.telemetry.osc_sender)
                    {
                        if let Err(e) = osc_sender.send_meter_bundle(
                            &snapshot,
                            &rendered.object_gains,
                            current_latency_instant_ms,
                            current_latency_control_ms,
                            current_latency_target_ms,
                            current_resample_ratio,
                            current_adaptive_band,
                        ) {
                            log::warn!("Failed to send meter OSC bundle: {}", e);
                        }
                    }

                    let samples_audio = AudioSamples::F32(rendered.samples);
                    self.output
                        .audio_writer
                        .as_mut()
                        .expect("audio_writer present")
                        .write_pcm_samples(&samples_audio, num_speakers)?;
                    self.output.render_buf = match samples_audio {
                        AudioSamples::F32(v) => v,
                        _ => unreachable!(),
                    };
                    self.output.pcm_f32_buf = pcm_f32_scratch;

                    if self
                        .telemetry
                        .osc_sender
                        .as_ref()
                        .is_some_and(|sender| sender.has_osc_clients())
                    {
                        if let (Some(ref mut osc_sender), Some(objects)) = (
                            self.telemetry.osc_sender.as_mut(),
                            build_virtual_bed_objects(
                                &labels,
                                room_ratio,
                                room_ratio_rear,
                                room_ratio_lower,
                                room_ratio_center_blend,
                            ),
                        ) {
                            let sample_pos = self
                                .session
                                .decoded_samples
                                .saturating_sub(sample_count as u64);
                            if let Err(e) =
                                osc_sender.send_object_frame(sample_pos, 0, 0, &objects)
                            {
                                log::warn!("Failed to send OSC virtual bed frame: {}", e);
                            }
                        }
                    }
                    return Ok(());
                }
            } else {
                log::trace!("Skipping VBAP: spatial_renderer is None");
            }

            // Fallback: standard pass-through (no VBAP).
            // frame.pcm is already interleaved — use it directly.
            log::trace!(
                "Writing {} samples (NO VBAP: {} sample_count × {} channels) to streaming output",
                frame.pcm.len(),
                sample_count,
                channel_count
            );

            self.output
                .audio_writer
                .as_mut()
                .expect("audio_writer present")
                .write_pcm_samples(&AudioSamples::I32(frame.pcm.to_vec()), channel_count)?;
            self.output.pcm_f32_buf = pcm_f32_scratch;
        }
        Ok(())
    }

    fn write_audio_samples_bed_conform(&mut self, frame: &RDecodedFrame) -> Result<()> {
        let channel_count = frame.channel_count as usize;
        let sample_count = frame.sample_count as usize;

        if let Some(ref mut writer) = self.output.audio_writer {
            let empty_vec = Vec::new();
            let bed_indices = self.spatial.bed_indices.as_ref().unwrap_or(&empty_vec);
            let conformed_channel_count = ChannelCountCalculator::calculate_conformed_channel_count(
                channel_count,
                bed_indices,
            );

            let samples = BedChannelMapper::apply_bed_conformance_to_frame(
                &frame.pcm,
                sample_count,
                channel_count,
                bed_indices,
            );

            writer.write_pcm_samples(&AudioSamples::I32(samples), conformed_channel_count)?;
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
        self.output.audio_writer = Some(self.build_audio_writer(
            output_backend,
            sample_rate,
            effective_channel_count,
            None,
        )?);
        self.reset_spatial_state_for_segment();
        Ok(())
    }

    fn reset_spatial_state_for_segment(&mut self) {
        self.spatial.has_objects = false;
        self.spatial.bed_indices = None; // Will be recalculated from new metadata
        self.spatial.object_names.clear();
        self.spatial.frame_events.clear();
        if let Some(renderer) = &self.spatial_renderer {
            renderer.reset_runtime_state();
        }
    }

    pub fn handle_decoder_flush_request(&mut self) {
        log::info!("Received flush request after decoder reset");
        if let Some(ref writer) = self.output.audio_writer {
            writer.request_flush();
        }
        self.reset_spatial_state_for_segment();
    }
}

#[inline]
fn fill_pcm_f32_reuse(out: &mut Vec<f32>, pcm: &[i32]) {
    const SCALE: f32 = 8_388_608.0;
    out.clear();
    out.reserve(pcm.len().saturating_sub(out.capacity()));
    for &s in pcm {
        out.push(s as f32 / SCALE);
    }
}
