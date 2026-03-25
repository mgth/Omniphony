use super::output::AudioWriter;
use crate::cli::command::OutputBackend;
use crate::runtime_osc::OscSender;
#[cfg(target_os = "linux")]
use audio_output::pipewire::PipewireBufferConfig;
use audio_output::AdaptiveResamplingConfig;
use bridge_api::RCoordinateFormat;
use log::Level;
use renderer::metering::AudioMeter;
use std::time::Instant;

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
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    pub latency_target_ms: u32,
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
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            latency_target_ms: 220,
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
    pub is_segmented: bool,
    pub segment_start_samples: u64,
    pub frame_events: Vec<renderer::spatial_renderer::SpatialChannelEvent>,
    pub loudness_applied: bool,
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
    pub bootstrap_frames_seen: u32,
    pub bootstrap_started_at: Option<Instant>,
    pub render_buf: Vec<f32>,
    pub pcm_f32_buf: Vec<f32>,
    pub output_init_failed: bool,
    pub last_audio_delay_written_ms: Option<f32>,
    pub last_audio_delay_attempted_ms: Option<f32>,
    pub last_audio_delay_write_error_at: Option<Instant>,
    pub last_audio_sample_rate_hz: Option<u32>,
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
    pub fn invalidate_writer(&mut self) -> Option<AudioWriter> {
        self.output_init_failed = false;
        self.audio_writer.take()
    }

    pub fn update_adaptive_config(&self, config: audio_output::AdaptiveResamplingConfig) {
        if let Some(writer) = &self.audio_writer {
            writer.update_adaptive_config(config);
        }
    }

    pub fn request_ratio_reset(&self) {
        if let Some(writer) = &self.audio_writer {
            writer.request_ratio_reset();
        }
    }
}

pub struct DecodeSessionState {
    pub decoded_frames: u64,
    pub decoded_samples: u64,
    pub final_sample_rate: u32,
    pub started_at: Option<Instant>,
    pub last_frame_received_at: Option<Instant>,
    pub last_frame_sample_count: Option<u32>,
    pub last_output_delay_log_at: Option<Instant>,
    pub first_measured_output_delay_ms: Option<f32>,
}

impl Default for DecodeSessionState {
    fn default() -> Self {
        Self {
            decoded_frames: 0,
            decoded_samples: 0,
            final_sample_rate: 48000,
            started_at: None,
            last_frame_received_at: None,
            last_frame_sample_count: None,
            last_output_delay_log_at: None,
            first_measured_output_delay_ms: None,
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
