use super::output::AudioWriter;
use crate::cli::command::{OutputBackend, OutputFileFormatArg};
use audio_output::AdaptiveResamplingConfig;
#[cfg(target_os = "linux")]
use audio_output::pipewire::PipewireBufferConfig;
use bridge_api::RCoordinateFormat;
use orender_engine::osc::OscSender;
use renderer::metering::AudioMeter;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// Tracks the diag-publication cadence. The rate is read from a shared
/// atomic (`RendererControl::diag_rate_atomic`) each tick so the user
/// can adjust the cadence live; the interval is cached to avoid recomputing
/// the Duration from f32 each call.
pub struct DiagPublishCadence {
    pub rate_hz_bits: Arc<AtomicU32>,
    pub interval: Duration,
    pub last_rate_seen: f32,
    pub last_send_at: Option<Instant>,
}

impl DiagPublishCadence {
    pub fn new(rate_hz_bits: Arc<AtomicU32>) -> Self {
        let initial = f32::from_bits(rate_hz_bits.load(Ordering::Relaxed)).max(1.0);
        Self {
            rate_hz_bits,
            interval: Duration::from_secs_f32(1.0 / initial),
            last_rate_seen: initial,
            last_send_at: None,
        }
    }

    /// Return true if the interval has elapsed since the last send (or no
    /// send has happened yet); refresh the cached interval from the atomic
    /// when the rate has changed.
    pub fn should_send(&mut self, now: Instant) -> bool {
        let hz = f32::from_bits(self.rate_hz_bits.load(Ordering::Relaxed)).max(1.0);
        if (hz - self.last_rate_seen).abs() > 1e-3 {
            self.last_rate_seen = hz;
            self.interval = Duration::from_secs_f32(1.0 / hz);
        }
        match self.last_send_at {
            None => true,
            Some(last) => now.duration_since(last) >= self.interval,
        }
    }

    pub fn mark_sent(&mut self, now: Instant) {
        self.last_send_at = Some(now);
    }
}

#[derive(Clone)]
pub struct RuntimeOutputState {
    #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
    pub output_device: Option<String>,
    #[cfg(target_os = "linux")]
    pub pw_buffer_config: PipewireBufferConfig,
    pub adaptive_resampling_config: AdaptiveResamplingConfig,
    #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
    pub latency_target_ms: u32,
    pub output_sample_rate: Option<u32>,
    pub enable_adaptive_resampling: bool,
    /// Currently active output backend. Seeded at launch from the resolved
    /// backend and mutated live when Studio requests a switch (e.g. to `file`).
    pub active_output_backend: OutputBackend,
    /// Destination for the `file` backend: `-` (stdout) or a file/FIFO path.
    pub output_file: String,
    /// Encoding for the `file` backend.
    pub output_file_format: OutputFileFormatArg,
}

impl Default for RuntimeOutputState {
    fn default() -> Self {
        Self {
            #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
            output_device: None,
            #[cfg(target_os = "linux")]
            pw_buffer_config: PipewireBufferConfig::default(),
            adaptive_resampling_config: AdaptiveResamplingConfig::default(),
            #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
            latency_target_ms: 220,
            output_sample_rate: None,
            enable_adaptive_resampling: false,
            active_output_backend: OutputBackend::platform_default()
                .unwrap_or(OutputBackend::Unsupported),
            output_file: "-".to_string(),
            output_file_format: OutputFileFormatArg::RawF32,
        }
    }
}

pub struct TelemetryState {
    pub osc_sender: Option<OscSender>,
    pub audio_meter: Option<AudioMeter>,
    pub diag_cadence: Option<DiagPublishCadence>,
}

impl Default for TelemetryState {
    fn default() -> Self {
        Self {
            osc_sender: None,
            audio_meter: None,
            diag_cadence: None,
        }
    }
}

pub struct SpatialState {
    pub has_objects: bool,
    /// Fixed-channel bed ids in the legacy 0-9 EXPORT order (file-output
    /// conformance only — rendering goes through `fixed_planner`).
    pub bed_indices: Option<Vec<usize>>,
    /// Shared fixed-channel planner (routing + plan cache), mirroring the
    /// embedded engine.
    pub fixed_planner: orender_engine::virtual_bed::FixedChannelPlanner,
    /// Cached object↔channel declaration from the bridge (sparse emission),
    /// sorted by channel.
    pub object_channels: Vec<(u32, usize)>,
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
            fixed_planner: orender_engine::virtual_bed::FixedChannelPlanner::new(),
            object_channels: Vec::new(),
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
    pub last_audio_output_device: Option<String>,
    pub drc_gain: f32,
    pub drc_ramp_samples_remaining: u32,
    pub drc_target_gain: f32,
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
            last_audio_output_device: None,
            drc_gain: 1.0,
            drc_ramp_samples_remaining: 0,
            drc_target_gain: 1.0,
        }
    }
}

impl OutputState {
    pub fn invalidate_writer(&mut self) -> Option<AudioWriter> {
        self.output_init_failed = false;
        self.audio_writer.take()
    }

    pub fn reset_realtime_output_tracking(&mut self) {
        self.bootstrap_frames_seen = 0;
        self.bootstrap_started_at = None;
        self.last_audio_delay_written_ms = None;
        self.last_audio_delay_attempted_ms = None;
        self.last_audio_delay_write_error_at = None;
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
    pub last_input_state_generation: Option<u64>,
    /// Set to true once the direct trigger mode has been wired (trigger fn sent to writer).
    pub direct_trigger_wired: bool,
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
            last_input_state_generation: None,
            direct_trigger_wired: false,
        }
    }
}

pub struct FrameHandlerContext {
    pub bed_conform: bool,
    pub use_loudness: bool,
    pub decode_time_ms: f32,
    pub queue_delay_ms: f32,
}
