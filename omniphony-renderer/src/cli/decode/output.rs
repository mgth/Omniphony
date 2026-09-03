use anyhow::{Result, anyhow};
#[cfg(any(target_os = "windows", target_os = "macos"))]
use audio_output::AdaptiveResamplingConfig;
#[cfg(any(target_os = "windows", target_os = "macos"))]
use audio_output::cpal_output::CpalWriter;
#[cfg(target_os = "linux")]
use audio_output::pipewire::{
    PipewireAdaptiveResamplingConfig, PipewireBufferConfig, PipewireWriter,
};
#[cfg(target_os = "linux")]
use std::sync::{Arc, atomic::AtomicI64};

/// The downstream consumer of the `file` sink closed its end of the pipe or
/// FIFO. Not a fault of the render: the output simply has nowhere to go any
/// more, and the run ends the way it does at the end of its input. Raised by
/// [`AudioWriter::write_pcm_samples`] in place of the `BrokenPipe` it maps
/// from, so the session loop can tell it apart from a real write error.
#[derive(Debug)]
pub struct OutputClosed;

impl std::fmt::Display for OutputClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("output consumer closed the pipe")
    }
}

impl std::error::Error for OutputClosed {}

/// Classify a `file` sink write failure: a reader that went away (`EPIPE`)
/// is the end of the output, anything else is a genuine write error.
fn file_sink_error(err: std::io::Error) -> anyhow::Error {
    if err.kind() == std::io::ErrorKind::BrokenPipe {
        anyhow::Error::new(OutputClosed)
    } else {
        err.into()
    }
}

/// Flush the `file` sink, treating a consumer that already went away as
/// nothing left to deliver: the final flush of a run that ended on
/// [`OutputClosed`] must not turn back into an error.
fn flush_file_sink(writer: &mut audio_output::FileAudioWriter) -> Result<()> {
    match writer.flush() {
        Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        other => other.map_err(anyhow::Error::from),
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AudioLatencySnapshot {
    /// Current end-to-end latency observed by the listener.
    pub final_latency_ms: f32,
    /// Internal buffer-control latency used by the servo/recovery logic.
    pub control_latency_ms: Option<f32>,
    /// EMA-smoothed control latency — the value the servo actually tracks.
    pub smoothed_control_latency_ms: Option<f32>,
    /// Target buffer-control latency used by the servo/recovery logic.
    pub target_control_latency_ms: Option<f32>,
    /// Downstream latency contribution outside the internal control buffer.
    pub downstream_latency_ms: Option<f32>,
    /// Ring-buffer level (input-domain) converted to ms — first of the three
    /// components that sum into `control_available`. Useful to localise the
    /// origin of oscillations in the control buffer.
    pub avail_input_latency_ms: Option<f32>,
    /// Output-FIFO content of the local resampler converted back to input-domain
    /// ms — second component of `control_available`.
    pub output_fifo_latency_ms: Option<f32>,
    /// Resampler-pending input samples expressed as ms — third component of
    /// `control_available`.
    pub resampler_pending_latency_ms: Option<f32>,
}

/// Full scale of the integer sample domain: decoders hand out 24-bit samples
/// sign-extended into an `i32`, so unity is 2^23 and not `i32::MAX`. Anything
/// producing frames for the renderer has to scale to this, or it arrives 256x
/// too loud.
pub const I32_PCM_FULL_SCALE: i32 = 1 << 23;

/// Audio sample data in different formats
pub enum AudioSamples {
    /// 24-bit signed integer samples (stored in i32 LSB)
    I32(Vec<i32>),
    /// 32-bit floating point samples (range -1.0 to 1.0)
    F32(Vec<f32>),
}

impl AudioSamples {
    /// Get length in samples
    pub fn len(&self) -> usize {
        match self {
            AudioSamples::I32(v) => v.len(),
            AudioSamples::F32(v) => v.len(),
        }
    }

    /// Convert to f32 format (range -1.0 to 1.0), converting from i32 if necessary
    pub fn to_f32(&self) -> Vec<f32> {
        match self {
            AudioSamples::I32(v) => v
                .iter()
                .map(|&s| (s as f64 / I32_PCM_FULL_SCALE as f64) as f32)
                .collect(),
            AudioSamples::F32(v) => v.clone(),
        }
    }

    /// Borrow as f32 slice if already in f32 format, otherwise None
    pub fn as_f32(&self) -> Option<&[f32]> {
        match self {
            AudioSamples::I32(_) => None,
            AudioSamples::F32(v) => Some(v),
        }
    }
}

pub enum AudioWriter {
    #[cfg(target_os = "linux")]
    Pipewire(PipewireWriter),
    /// cpal-backed local output: ASIO on Windows, CoreAudio on macOS. Both
    /// share `CpalWriter`, so a single variant serves both platforms.
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    Cpal(CpalWriter),
    /// File / FIFO / stdout sink (raw f32 or CAF). Cross-platform,
    /// non-realtime: all latency/adaptive accessors below report `None`.
    File(audio_output::FileAudioWriter),
    Unsupported,
}

impl AudioWriter {
    #[cfg(target_os = "linux")]
    pub fn create_pipewire(
        sample_rate: u32,
        channel_count: u32,
        sink_target: Option<String>,
        enable_adaptive_resampling: bool,
        output_sample_rate: Option<u32>,
        buffer_config: PipewireBufferConfig,
        adaptive_config: PipewireAdaptiveResamplingConfig,
        input_clock_us: std::sync::Arc<std::sync::atomic::AtomicU64>,
    ) -> Result<Self> {
        let pipewire_writer = PipewireWriter::new(
            sample_rate,
            channel_count,
            sink_target,
            enable_adaptive_resampling,
            output_sample_rate,
            buffer_config,
            adaptive_config,
            input_clock_us,
        )?;
        Ok(AudioWriter::Pipewire(pipewire_writer))
    }

    #[cfg(target_os = "linux")]
    pub fn create_pipewire_with_channel_names(
        sample_rate: u32,
        channel_count: u32,
        sink_target: Option<String>,
        channel_names: Vec<String>,
        enable_adaptive_resampling: bool,
        output_sample_rate: Option<u32>,
        buffer_config: PipewireBufferConfig,
        adaptive_config: PipewireAdaptiveResamplingConfig,
        input_clock_us: std::sync::Arc<std::sync::atomic::AtomicU64>,
    ) -> Result<Self> {
        let pipewire_writer = PipewireWriter::new_with_channel_names(
            sample_rate,
            channel_count,
            sink_target,
            Some(channel_names),
            enable_adaptive_resampling,
            output_sample_rate,
            buffer_config,
            adaptive_config,
            input_clock_us,
        )?;
        Ok(AudioWriter::Pipewire(pipewire_writer))
    }

    /// Create the platform's cpal-backed realtime output writer: ASIO on
    /// Windows, CoreAudio on macOS. Both go through `CpalWriter`.
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub fn create_cpal(
        input_sample_rate: u32,
        sample_rate: u32,
        channel_count: u32,
        device_name: Option<String>,
        target_latency_ms: u32,
        enable_adaptive_resampling: bool,
        adaptive_config: AdaptiveResamplingConfig,
    ) -> Result<Self> {
        let cpal_writer = CpalWriter::new(
            input_sample_rate,
            sample_rate,
            channel_count,
            device_name,
            target_latency_ms,
            enable_adaptive_resampling,
            adaptive_config,
        )?;
        Ok(AudioWriter::Cpal(cpal_writer))
    }

    /// Create a file/FIFO/stdout sink. `destination` is `"-"` for stdout or a
    /// file/FIFO path. `channel_descs` carries the speaker geometry embedded in
    /// the CAF `chan` chunk; ignored for the raw-f32 format.
    pub fn create_file(
        destination: &str,
        format: audio_output::FileSinkFormat,
        sample_rate: u32,
        channel_count: u32,
        channel_descs: Option<Vec<audio_output::CafChannelDesc>>,
    ) -> Result<Self> {
        let writer = audio_output::FileAudioWriter::new(
            destination,
            format,
            sample_rate,
            channel_count,
            channel_descs,
        )
        .map_err(|e| anyhow!("failed to open audio output destination '{destination}': {e}"))?;
        Ok(AudioWriter::File(writer))
    }

    pub fn write_pcm_samples(
        &mut self,
        samples: &AudioSamples,
        _channel_count: usize,
    ) -> Result<()> {
        #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
        let _ = samples;

        match self {
            #[cfg(target_os = "linux")]
            AudioWriter::Pipewire(pipewire_writer) => {
                if let Some(f32_slice) = samples.as_f32() {
                    pipewire_writer.write_samples(f32_slice)?;
                } else {
                    let samples_f32 = samples.to_f32();
                    pipewire_writer.write_samples(&samples_f32)?;
                }
                Ok(())
            }
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            AudioWriter::Cpal(w) => {
                let samples_f32 = samples.to_f32();
                w.write_samples(&samples_f32)?;
                Ok(())
            }
            AudioWriter::File(file_writer) => {
                let written = if let Some(f32_slice) = samples.as_f32() {
                    file_writer.write_samples(f32_slice)
                } else {
                    let samples_f32 = samples.to_f32();
                    file_writer.write_samples(&samples_f32)
                };
                written.map_err(file_sink_error)
            }
            AudioWriter::Unsupported => Err(anyhow!("No supported realtime output backend")),
        }
    }

    /// Cross-crate handle to the post-rendering pacer, if this backend has
    /// one. Used by the decode lifecycle to install the handle on the
    /// audio_input `InputControl` so the PipeWire input thread can drain
    /// the FIFO into the ring.
    #[cfg(target_os = "linux")]
    pub fn pacer_handle(&self) -> Option<audio_output::PacerHandle> {
        match self {
            AudioWriter::Pipewire(w) => Some(w.pacer_handle()),
            _ => None,
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub fn pacer_handle(&self) -> Option<audio_output::PacerHandle> {
        None
    }

    pub fn close_and_drop(self) -> Result<()> {
        match self {
            #[cfg(target_os = "linux")]
            AudioWriter::Pipewire(mut w) => {
                w.flush()?;
                drop(w);
                Ok(())
            }
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            AudioWriter::Cpal(mut w) => {
                w.flush()?;
                drop(w);
                Ok(())
            }
            AudioWriter::File(mut w) => {
                flush_file_sink(&mut w)?;
                drop(w);
                Ok(())
            }
            AudioWriter::Unsupported => Err(anyhow!("No supported realtime output backend")),
        }
    }

    pub fn finish(&mut self) -> Result<()> {
        match self {
            #[cfg(target_os = "linux")]
            AudioWriter::Pipewire(pipewire_writer) => {
                pipewire_writer.flush()?;
                Ok(())
            }
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            AudioWriter::Cpal(w) => {
                w.flush()?;
                Ok(())
            }
            AudioWriter::File(file_writer) => flush_file_sink(file_writer),
            AudioWriter::Unsupported => Err(anyhow!("No supported realtime output backend")),
        }
    }

    pub fn flush(&mut self) -> Result<()> {
        self.finish()
    }

    /// Returns the current estimated audio latency in milliseconds, if supported by the backend.
    pub fn latency_ms(&self) -> Option<f32> {
        match self {
            #[cfg(target_os = "linux")]
            AudioWriter::Pipewire(pw) => Some(pw.latency_ms()),
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            AudioWriter::Cpal(w) => Some(w.latency_ms()),
            AudioWriter::File(_) => None,
            AudioWriter::Unsupported => None,
        }
    }

    /// Returns the current PI controller rate-adjust factor, or `None` if adaptive
    /// resampling is disabled or the backend does not support it.
    pub fn resample_ratio(&self) -> Option<f32> {
        match self {
            #[cfg(target_os = "linux")]
            AudioWriter::Pipewire(pw) => pw.rate_adjust(),
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            AudioWriter::Cpal(w) => w.rate_adjust(),
            AudioWriter::File(_) => None,
            AudioWriter::Unsupported => None,
        }
    }

    pub fn adaptive_band(&self) -> Option<&'static str> {
        match self {
            #[cfg(target_os = "linux")]
            AudioWriter::Pipewire(pw) => pw.adaptive_band(),
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            AudioWriter::Cpal(w) => w.adaptive_band(),
            AudioWriter::File(_) => None,
            AudioWriter::Unsupported => None,
        }
    }

    pub fn adaptive_runtime_state(&self) -> Option<&'static str> {
        match self {
            #[cfg(target_os = "linux")]
            AudioWriter::Pipewire(pw) => pw.adaptive_runtime_state(),
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            AudioWriter::Cpal(w) => w.adaptive_runtime_state(),
            AudioWriter::File(_) => None,
            AudioWriter::Unsupported => None,
        }
    }

    pub fn latency_snapshot(&self) -> Option<AudioLatencySnapshot> {
        let final_latency_ms = self.measured_audio_delay_ms()?;
        let control_latency_ms = self.control_audio_delay_ms();
        let smoothed_control_latency_ms = self.smoothed_control_audio_delay_ms();
        let target_control_latency_ms = self.target_control_latency_ms();
        let downstream_latency_ms = control_latency_ms.and_then(|control_ms| {
            let downstream_ms = final_latency_ms - control_ms;
            (downstream_ms >= 0.0).then_some(downstream_ms)
        });
        let avail_input_latency_ms = self.avail_input_audio_delay_ms();
        let output_fifo_latency_ms = self.output_fifo_audio_delay_ms();
        let resampler_pending_latency_ms = self.resampler_pending_audio_delay_ms();
        Some(AudioLatencySnapshot {
            final_latency_ms,
            control_latency_ms,
            smoothed_control_latency_ms,
            target_control_latency_ms,
            downstream_latency_ms,
            avail_input_latency_ms,
            output_fifo_latency_ms,
            resampler_pending_latency_ms,
        })
    }

    pub fn target_control_latency_ms(&self) -> Option<f32> {
        match self {
            #[cfg(target_os = "linux")]
            AudioWriter::Pipewire(pw) => {
                let v = pw.target_control_latency_ms();
                if v > 0.0 { Some(v) } else { None }
            }
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            AudioWriter::Cpal(w) => {
                let v = w.target_control_latency_ms();
                if v > 0.0 { Some(v) } else { None }
            }
            AudioWriter::File(_) => None,
            AudioWriter::Unsupported => None,
        }
    }

    /// Total audio delay in ms (ring-buffer target + backend graph latency).
    pub fn target_audio_delay_ms(&self) -> Option<f32> {
        match self {
            #[cfg(target_os = "linux")]
            AudioWriter::Pipewire(pw) => {
                let v = pw.total_audio_delay_ms();
                if v > 0.0 { Some(v) } else { None }
            }
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            AudioWriter::Cpal(w) => {
                let v = w.total_audio_delay_ms();
                if v > 0.0 { Some(v) } else { None }
            }
            AudioWriter::File(_) => None,
            AudioWriter::Unsupported => None,
        }
    }

    /// Backward-compatible alias for the configured final latency target.
    pub fn total_audio_delay_ms(&self) -> Option<f32> {
        self.target_audio_delay_ms()
    }

    /// Measured total audio delay in ms (current ring-buffer + backend graph latency).
    pub fn measured_audio_delay_ms(&self) -> Option<f32> {
        match self {
            #[cfg(target_os = "linux")]
            AudioWriter::Pipewire(pw) => {
                let v = pw.measured_audio_delay_ms();
                if v > 0.0 { Some(v) } else { None }
            }
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            AudioWriter::Cpal(w) => {
                let v = w.measured_audio_delay_ms();
                if v > 0.0 { Some(v) } else { None }
            }
            AudioWriter::File(_) => None,
            AudioWriter::Unsupported => None,
        }
    }

    pub fn control_audio_delay_ms(&self) -> Option<f32> {
        match self {
            #[cfg(target_os = "linux")]
            AudioWriter::Pipewire(pw) => {
                let v = pw.control_audio_delay_ms();
                if v > 0.0 { Some(v) } else { None }
            }
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            AudioWriter::Cpal(w) => {
                let v = w.control_audio_delay_ms();
                if v > 0.0 { Some(v) } else { None }
            }
            AudioWriter::File(_) => None,
            AudioWriter::Unsupported => None,
        }
    }

    pub fn smoothed_control_audio_delay_ms(&self) -> Option<f32> {
        match self {
            #[cfg(target_os = "linux")]
            AudioWriter::Pipewire(pw) => {
                let v = pw.smoothed_control_audio_delay_ms();
                if v > 0.0 { Some(v) } else { None }
            }
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            AudioWriter::Cpal(w) => {
                let v = w.smoothed_control_audio_delay_ms();
                if v > 0.0 { Some(v) } else { None }
            }
            AudioWriter::File(_) => None,
            AudioWriter::Unsupported => None,
        }
    }

    /// Component accessors keep zero values: they represent a snapshot of the
    /// underlying buffer occupancy, where 0 ms is a valid runtime state
    /// (e.g. the resampler holds no pending input at this callback).
    /// Unlike the aggregate latency accessors, we don't use `> 0.0` as a
    /// readiness sentinel here.
    pub fn avail_input_audio_delay_ms(&self) -> Option<f32> {
        match self {
            #[cfg(target_os = "linux")]
            AudioWriter::Pipewire(pw) => Some(pw.avail_input_audio_delay_ms().max(0.0)),
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            AudioWriter::Cpal(w) => Some(w.avail_input_audio_delay_ms().max(0.0)),
            AudioWriter::File(_) => None,
            AudioWriter::Unsupported => None,
        }
    }

    pub fn output_fifo_audio_delay_ms(&self) -> Option<f32> {
        match self {
            #[cfg(target_os = "linux")]
            AudioWriter::Pipewire(pw) => Some(pw.output_fifo_audio_delay_ms().max(0.0)),
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            AudioWriter::Cpal(w) => Some(w.output_fifo_audio_delay_ms().max(0.0)),
            AudioWriter::File(_) => None,
            AudioWriter::Unsupported => None,
        }
    }

    pub fn resampler_pending_audio_delay_ms(&self) -> Option<f32> {
        match self {
            #[cfg(target_os = "linux")]
            AudioWriter::Pipewire(pw) => Some(pw.resampler_pending_audio_delay_ms().max(0.0)),
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            AudioWriter::Cpal(w) => Some(w.resampler_pending_audio_delay_ms().max(0.0)),
            AudioWriter::File(_) => None,
            AudioWriter::Unsupported => None,
        }
    }

    /// Diagnostic metric handles published by the active output backend.
    /// Each entry should be passed to `DiagRegistry::register_external`.
    /// Returns an empty Vec on backends that do not yet publish any diag.
    pub fn diag_atomic_handles(&self) -> Vec<sys::diag::DiagAtomicHandle> {
        match self {
            #[cfg(target_os = "linux")]
            AudioWriter::Pipewire(pw) => pw.diag_atomic_handles(),
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            AudioWriter::Cpal(_) => Vec::new(),
            AudioWriter::File(_) => Vec::new(),
            AudioWriter::Unsupported => Vec::new(),
        }
    }

    /// Signal the audio thread to snap the resampling ratio back to base and reset the integrator.
    pub fn request_ratio_reset(&self) {
        match self {
            #[cfg(target_os = "linux")]
            AudioWriter::Pipewire(pw) => pw.request_ratio_reset(),
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            AudioWriter::Cpal(w) => w.request_ratio_reset(),
            AudioWriter::File(_) => {}
            AudioWriter::Unsupported => {}
        }
    }

    /// Update adaptive resampling tuning parameters on the live audio thread without a restart.
    pub fn update_adaptive_config(&self, config: audio_output::AdaptiveResamplingConfig) {
        match self {
            #[cfg(target_os = "linux")]
            AudioWriter::Pipewire(pw) => pw.update_adaptive_config(config),
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            AudioWriter::Cpal(w) => w.update_adaptive_config(config),
            AudioWriter::File(_) => {}
            AudioWriter::Unsupported => {}
        }
    }

    /// Set the capture sample rate for the Bresenham trigger ratio (direct trigger mode).
    pub fn set_input_trigger_rate_hz(&self, _rate_hz: u32) {
        match self {
            #[cfg(target_os = "linux")]
            AudioWriter::Pipewire(pw) => pw.set_input_trigger_rate_hz(_rate_hz),
            _ => {}
        }
    }

    /// Set the observed capture quantum in transport frames for direct-trigger mode.
    pub fn set_input_trigger_quantum_frames(&self, _quantum_frames: u32) {
        match self {
            #[cfg(target_os = "linux")]
            AudioWriter::Pipewire(pw) => pw.set_input_trigger_quantum_frames(_quantum_frames),
            _ => {}
        }
    }

    /// Returns the pending-trigger counter incremented by the output RT callback (Bresenham).
    /// Pass this to InputControl.set_pending_input_triggers() for the capture mainloop to drain.
    #[cfg(target_os = "linux")]
    pub fn pending_input_triggers(&self) -> Option<Arc<AtomicI64>> {
        match self {
            AudioWriter::Pipewire(pw) => Some(pw.pending_input_triggers()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use audio_output::{FileAudioWriter, FileSinkFormat};
    use std::io::{self, Write};

    /// A destination whose reader has gone: every write fails with `EPIPE`.
    struct GoneReader;

    impl Write for GoneReader {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// A destination that fails for a reason unrelated to its reader.
    struct FullDisk;

    impl Write for FullDisk {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("no space left on device"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn file_writer_over(sink: impl Write + Send + 'static) -> AudioWriter {
        AudioWriter::File(
            FileAudioWriter::with_writer(Box::new(sink), FileSinkFormat::RawF32, 48_000, 2, None)
                .expect("a raw sink has no header to write"),
        )
    }

    /// Larger than the sink's own buffer, so the write reaches the destination
    /// at once instead of sitting in the buffer until a later flush.
    fn unbuffered_block() -> AudioSamples {
        AudioSamples::F32(vec![0.0; 32 * 1024])
    }

    #[test]
    fn a_reader_that_went_away_is_reported_as_output_closed() {
        let mut writer = file_writer_over(GoneReader);
        let err = writer
            .write_pcm_samples(&unbuffered_block(), 2)
            .expect_err("the closed pipe must surface");
        assert!(err.is::<OutputClosed>(), "unexpected error: {err:#}");
    }

    #[test]
    fn other_write_failures_stay_errors() {
        let mut writer = file_writer_over(FullDisk);
        let err = writer
            .write_pcm_samples(&unbuffered_block(), 2)
            .expect_err("a failed write must surface");
        assert!(
            !err.is::<OutputClosed>(),
            "misread as a closed pipe: {err:#}"
        );
        assert!(err.downcast_ref::<io::Error>().is_some());
    }

    #[test]
    fn flushing_towards_a_gone_reader_is_not_an_error() {
        let mut writer = file_writer_over(GoneReader);
        writer
            .write_pcm_samples(&AudioSamples::F32(vec![0.0; 16]), 2)
            .expect("a small block is buffered, not yet written");
        writer.finish().expect("nothing left to deliver");
        writer.close_and_drop().expect("nothing left to deliver");
    }
}
