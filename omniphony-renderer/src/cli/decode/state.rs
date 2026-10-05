use super::decoder_thread::{Declaration, DecodedSource};
use super::output::AudioWriter;
use crate::cli::command::{OutputBackend, OutputFileFormatArg};
use audio_input::InputControl;
use audio_output::AdaptiveResamplingConfig;
#[cfg(target_os = "linux")]
use audio_output::pipewire::PipewireBufferConfig;
use orender_engine::osc::OscSender;
use orender_engine::stream_state::{StreamDeclaration, StreamState};
use renderer::metering::AudioMeter;
use renderer::placement::{PlacementState, SourceFamily};
use renderer::spatial_renderer::SpatialRenderer;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// What the handler calls the PipeWire sink's plain PCM, which has no bridge
/// to name it (`StreamDeclaration::label`).
const LIVE_PCM_LABEL: &str = "PCM";

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

/// The CLI's per-stream state: the state both hosts keep
/// ([`StreamState`], with the rules that update it), plus what only this host
/// needs — the bed-conformed export's bed ids, and which of the PipeWire
/// sink's two producers the declaration and the dialogue level are for.
pub struct SpatialState {
    pub stream: StreamState,
    /// Fixed-channel bed ids in the legacy 0-9 EXPORT order (file-output
    /// conformance only — rendering goes through the stream's planners).
    pub bed_indices: Option<Vec<usize>>,
    /// The input the stream's declaration is for (see
    /// [`SpatialState::take_declaration`]); `None` before the first frame.
    declared_for: Option<DecodedSource>,
    /// The bridge's declaration, set aside while the PipeWire sink plays
    /// plain PCM.
    bridge_declaration_aside: Option<StreamDeclaration>,
    /// The bridge's dialogue normalisation level, set aside likewise (see
    /// [`SpatialState::take_input`]).
    bridge_dialnorm_aside: Option<i8>,
    pub au_index: u64,
}

impl Default for SpatialState {
    fn default() -> Self {
        Self {
            stream: StreamState::default(),
            bed_indices: None,
            declared_for: None,
            bridge_declaration_aside: None,
            bridge_dialnorm_aside: None,
            au_index: 0,
        }
    }
}

impl SpatialState {
    /// Whether this frame carries objects, and so takes the object render path.
    ///
    /// Derived from the frame's source, not from `has_objects` alone: that
    /// flag latches on the first frame carrying metadata and is only cleared
    /// at a segment reset, so once an object stream has played, plain channel
    /// content arriving afterwards would keep taking the object path. The sink
    /// switches between encoded and linear PCM at will, so that happens in one
    /// session. A live PCM frame is channel content by construction — fixed
    /// labels, no metadata — whatever played before it.
    pub fn frame_has_objects(&self, source: DecodedSource) -> bool {
        self.stream.has_objects && !matches!(source, DecodedSource::Live)
    }

    /// Take on the input a frame came from: its declaration
    /// ([`take_declaration`](Self::take_declaration)) and, with a renderer,
    /// its dialogue normalisation. True when the renderer's loudness changed,
    /// for the host to publish it.
    ///
    /// The level a bridge sent is its stream's, like its declaration. Plain
    /// PCM carries none: while it plays the renderer applies no correction,
    /// and the bridge's level waits here. The bitstream gets it back when it
    /// returns, as its bridge only sends it at a major sync. A level left in
    /// the renderer by a stream that has ended is dropped for PCM all the
    /// same.
    pub fn take_input(
        &mut self,
        source: DecodedSource,
        declaration: Option<StreamDeclaration>,
        renderer: Option<&SpatialRenderer>,
    ) -> bool {
        let loudness_changed = match renderer {
            Some(renderer) if self.declared_for != Some(source) => match source {
                DecodedSource::Live => {
                    self.bridge_dialnorm_aside = self.stream.dialnorm.take();
                    renderer.clear_loudness()
                }
                DecodedSource::Bridge => match self.bridge_dialnorm_aside.take() {
                    Some(level) => {
                        renderer.set_loudness(level);
                        self.stream.dialnorm = Some(level);
                        true
                    }
                    None => false,
                },
            },
            _ => false,
        };
        self.take_declaration(source, declaration);
        loudness_changed
    }

    /// The bridge's segment starts over: the level set aside for it goes with
    /// the latch ([`StreamState::reset_segment`]), the new segment brings its
    /// own.
    pub fn drop_dialnorm_aside(&mut self) {
        self.bridge_dialnorm_aside = None;
    }

    /// Take on the declaration a frame from `source` came with.
    ///
    /// The PipeWire sink feeds the handler from two producers: its bridge
    /// (bitstreams, declared by the bridge on the frames that need it) and
    /// plain PCM, which declares nothing. So the input changing is itself a
    /// declaration, applied here once per change rather than on every frame:
    /// PCM after a bitstream is PCM — its family, no declared poses — and not
    /// the bitstream's layout; a bitstream after PCM gets back the declaration
    /// its bridge made, which that bridge will not repeat, never having seen
    /// the PCM. Every frame of the pipe comes from the bridge, so there this
    /// only ever applies what the bridge declared.
    ///
    /// The declaration comes resolved ([`resolve_declaration`]): the family
    /// table is read when a bridge declares, not on every frame.
    pub fn take_declaration(
        &mut self,
        source: DecodedSource,
        declaration: Option<StreamDeclaration>,
    ) {
        let previous = self.declared_for.replace(source);
        if let Some(declaration) = declaration {
            self.bridge_declaration_aside = None;
            self.stream.set_declaration(declaration);
            return;
        }
        if previous == Some(source) {
            return;
        }
        match source {
            DecodedSource::Live => {
                // Moved, not cloned: the bridge's declaration waits here
                // until its input comes back.
                let pcm = StreamDeclaration {
                    family: SourceFamily::PCM,
                    label: LIVE_PCM_LABEL.to_owned(),
                    ..StreamDeclaration::default()
                };
                self.bridge_declaration_aside =
                    Some(std::mem::replace(&mut self.stream.declaration, pcm));
            }
            DecodedSource::Bridge => {
                // Nothing set aside: the bridge never declared, as for a
                // fresh stream.
                self.stream.declaration = self.bridge_declaration_aside.take().unwrap_or_default();
            }
        }
    }
}

/// What the sink carries: the renderer's output, or the decoded channels as
/// they are (host passthrough, bed-conformed export, no renderer at all).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputSource {
    Rendered,
    Decoded,
}

pub struct OutputState {
    pub audio_writer: Option<AudioWriter>,
    /// Channel count [`Self::audio_writer`] was built for.
    ///
    /// The sink is created once, from the width the renderer emitted at the
    /// time; the output mode can change afterwards (a headphone toggle swaps a
    /// speaker array for a stereo pair). Remembering what was built is what lets
    /// the caller notice the two have parted company and rebuild.
    pub audio_writer_channels: Option<usize>,
    pub bootstrap_frames_seen: u32,
    pub bootstrap_started_at: Option<Instant>,
    pub render_buf: Vec<f32>,
    pub pcm_f32_buf: Vec<f32>,
    /// Reused copy of the decoded PCM for the unrendered writes.
    pub pcm_i32_buf: Vec<i32>,
    pub output_init_failed: bool,
    pub last_audio_delay_written_ms: Option<f32>,
    pub last_audio_delay_attempted_ms: Option<f32>,
    pub last_audio_delay_write_error_at: Option<Instant>,
    pub last_audio_sample_rate_hz: Option<u32>,
    pub last_audio_sample_format: Option<String>,
    pub last_audio_output_device: Option<String>,
    /// Duty-cycle EMA of the render cost for the meter bundle, as in the
    /// embedded engine: raw per-frame timings alias with 40-sample access
    /// units, so the published figure is a smoothed per-frame equivalent.
    pub render_duty: renderer::metering::DutyEma,
}

impl Default for OutputState {
    fn default() -> Self {
        Self {
            audio_writer: None,
            audio_writer_channels: None,
            bootstrap_frames_seen: 0,
            bootstrap_started_at: None,
            render_buf: Vec::new(),
            pcm_f32_buf: Vec::new(),
            pcm_i32_buf: Vec::new(),
            output_init_failed: false,
            last_audio_delay_written_ms: None,
            last_audio_delay_attempted_ms: None,
            last_audio_delay_write_error_at: None,
            last_audio_sample_rate_hz: None,
            last_audio_sample_format: None,
            last_audio_output_device: None,
            render_duty: Default::default(),
        }
    }
}

impl OutputState {
    /// Retire the audio writer so the next frame builds a fresh one.
    ///
    /// The writer's cross-crate wiring goes with it: a PipeWire writer leaves
    /// its pacer handle on the `InputControl`, and the input thread keeps
    /// draining that handle on every chunk until something replaces it. The
    /// control is taken here so the handle is cleared in the same step; the
    /// next writer installs its own when it is built.
    pub fn invalidate_writer(
        &mut self,
        input_control: Option<&InputControl>,
    ) -> Option<AudioWriter> {
        self.output_init_failed = false;
        self.audio_writer_channels = None;
        if let Some(control) = input_control {
            control.clear_output_pacer();
        }
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

/// A bridge's declaration as the stream applies it, its family resolved
/// against the renderer's table; against the renderer's own families alone
/// when nothing renders (the family then places nothing).
pub fn resolve_declaration(
    renderer: Option<&renderer::spatial_renderer::SpatialRenderer>,
    declaration: Option<Declaration>,
) -> Option<StreamDeclaration> {
    let declaration = declaration?;
    Some(match renderer {
        Some(renderer) => {
            let control = renderer.renderer_control();
            let live = control.live.read();
            StreamDeclaration::new(declaration, &live.placement)
        }
        None => StreamDeclaration::new(declaration, &PlacementState::default()),
    })
}

pub struct FrameHandlerContext {
    pub bed_conform: bool,
    pub decode_time_ms: f32,
    pub queue_delay_ms: f32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_api::{RChannelLabel, RChannelPose};

    /// The table a bridge declaring three families leaves.
    fn table() -> PlacementState {
        let mut table = PlacementState::default();
        for name in ["dolby", "dts", "auro"] {
            table.declare(name, name, renderer::placement::PlacementMode::Room);
        }
        table
    }

    fn family(name: &str) -> SourceFamily {
        table().find(name).expect("declared")
    }

    fn declared(family: &str, poses: usize, label: &str) -> Option<StreamDeclaration> {
        let declaration = Declaration {
            poses: (0..poses)
                .map(|_| RChannelPose {
                    label: RChannelLabel::Ls,
                    azimuth_deg: -90.0,
                    elevation_deg: 0.0,
                })
                .collect(),
            family: family.to_owned(),
            label: label.to_owned(),
            tags: Vec::new(),
        };
        Some(StreamDeclaration::new(declaration, &table()))
    }

    fn state(s: &SpatialState) -> (SourceFamily, usize, &str) {
        let d = &s.stream.declaration;
        (d.family, d.poses.len(), &d.label)
    }

    /// The input switching between the sink's bridge and its plain PCM is a
    /// declaration of its own, applied once per switch.
    #[test]
    fn the_declaration_follows_the_sinks_input() {
        use DecodedSource::{Bridge, Live};
        let pcm = (SourceFamily::PCM, 0, "PCM");
        let mut s = SpatialState::default();

        // The first frame of a session is PCM.
        s.take_declaration(Live, None);
        assert_eq!(state(&s), pcm);
        // Declared once: a later PCM frame leaves the state alone.
        s.stream.declaration.label.push('!');
        s.take_declaration(Live, None);
        assert_eq!(s.stream.declaration.label, "PCM!");
        s.stream.declaration.label.pop();

        // A bitstream declares for itself.
        s.take_declaration(Bridge, declared("dts", 2, "DTS"));
        assert_eq!(state(&s), (family("dts"), 2, "DTS"));
        s.take_declaration(Bridge, None);
        assert_eq!(state(&s), (family("dts"), 2, "DTS"));
        // PCM after it does not keep its layout.
        s.take_declaration(Live, None);
        assert_eq!(state(&s), pcm);
        // Back to the bitstream, which does not declare again: its own
        // declaration comes back.
        s.take_declaration(Bridge, None);
        assert_eq!(state(&s), (family("dts"), 2, "DTS"));
        // A late bitstream frame between PCM ones: each input gets its own.
        s.take_declaration(Live, None);
        s.take_declaration(Bridge, None);
        assert_eq!(state(&s), (family("dts"), 2, "DTS"));
        s.take_declaration(Live, None);
        assert_eq!(state(&s), pcm);
        // A new bitstream declaring: the one set aside is gone for good.
        s.take_declaration(Bridge, declared("dolby", 0, "TrueHD"));
        s.take_declaration(Live, None);
        s.take_declaration(Bridge, None);
        assert_eq!(state(&s), (family("dolby"), 0, "TrueHD"));

        // A bridge that never declared, after PCM: as a fresh stream.
        let mut s = SpatialState::default();
        s.take_declaration(Live, None);
        s.take_declaration(Bridge, None);
        assert_eq!(state(&s), (SourceFamily::GENERIC, 0, ""));
    }

    /// A frame carrying `level`, as a bridge sends it at a major sync.
    fn frame_with_dialogue_level(level: i8) -> bridge_api::RDecodedFrame {
        bridge_api::RDecodedFrame {
            sampling_frequency: 48_000,
            sample_count: 0,
            channel_count: 0,
            pcm: Vec::new().into(),
            channel_labels: Vec::new().into(),
            metadata: Vec::new().into(),
            drc_gain: 1.0,
            drc_ramp_duration: 0,
            dialogue_level: Some(level).into(),
            is_new_segment: false,
        }
    }

    /// The dialogue level a bridge sent is its stream's: the sink's plain PCM
    /// plays without it, and the bitstream gets it back, without its bridge
    /// sending it again.
    #[test]
    fn the_dialogue_level_follows_the_sinks_input() {
        use DecodedSource::{Bridge, Live};
        let renderer = super::super::handler::tests::test_renderer();
        let applied = || renderer.renderer_control().live.read().dialogue_level;
        let mut s = SpatialState::default();

        // A bitstream, and the level its bridge sends.
        assert!(!s.take_input(Bridge, None, Some(&renderer)));
        assert!(
            s.stream
                .latch_dialnorm(&frame_with_dialogue_level(-11), &renderer)
        );
        assert_eq!(applied(), Some(-11));

        // PCM after it: no correction, published once.
        assert!(s.take_input(Live, None, Some(&renderer)));
        assert_eq!(applied(), None);
        assert!(!s.take_input(Live, None, Some(&renderer)));

        // Back to the bitstream: its level, and the latch closed on it, so a
        // repeat at the next major sync changes nothing.
        assert!(s.take_input(Bridge, None, Some(&renderer)));
        assert_eq!(applied(), Some(-11));
        assert!(
            !s.stream
                .latch_dialnorm(&frame_with_dialogue_level(-11), &renderer)
        );
        assert!(!s.take_input(Bridge, None, Some(&renderer)));

        // The bridge resets while PCM plays: its next segment brings its own
        // level, the one set aside is not given back.
        assert!(s.take_input(Live, None, Some(&renderer)));
        super::super::spatial_metadata::SpatialMetadataCoordinator::new(
            &mut s,
            Some(&renderer),
            None,
        )
        .reset_for_segment();
        assert!(!s.take_input(Bridge, None, Some(&renderer)));
        assert_eq!(applied(), None);
        assert!(
            s.stream
                .latch_dialnorm(&frame_with_dialogue_level(-21), &renderer)
        );
        assert_eq!(applied(), Some(-21));

        // The stream ended (the handler starts its state over) and left its
        // level in the renderer: PCM does not play with it either.
        let mut s = SpatialState::default();
        assert!(s.take_input(Live, None, Some(&renderer)));
        assert_eq!(applied(), None);
        assert!(!s.take_input(Bridge, None, Some(&renderer)));
        assert_eq!(applied(), None);
    }

    /// With the bridge as the only input nothing is handed over: the level
    /// stays the stream's business (`StreamState::latch_dialnorm`), as in the
    /// embedded engine.
    #[test]
    fn a_bridge_only_input_keeps_its_dialogue_level() {
        let renderer = super::super::handler::tests::test_renderer();
        renderer.set_loudness(-27);
        let mut s = SpatialState::default();
        assert!(!s.take_input(DecodedSource::Bridge, None, Some(&renderer)));
        assert!(!s.take_input(DecodedSource::Bridge, None, Some(&renderer)));
        assert_eq!(
            renderer.renderer_control().live.read().dialogue_level,
            Some(-27)
        );
    }

    /// The pipe only ever has the bridge: its declarations apply as they come,
    /// and a frame without one changes nothing.
    #[test]
    fn a_bridge_only_input_applies_its_declarations_as_they_come() {
        let mut s = SpatialState::default();
        s.take_declaration(DecodedSource::Bridge, None);
        assert_eq!(state(&s), (SourceFamily::GENERIC, 0, ""));
        s.take_declaration(DecodedSource::Bridge, declared("auro", 3, "Auro-3D"));
        s.take_declaration(DecodedSource::Bridge, None);
        assert_eq!(state(&s), (family("auro"), 3, "Auro-3D"));
    }
}
