use anyhow::{Result, anyhow};
#[cfg(target_os = "windows")]
use audio_output::AdaptiveResamplingConfig;
#[cfg(target_os = "windows")]
use audio_output::asio::AsioWriter;
#[cfg(target_os = "linux")]
use audio_output::pipewire::{
    PipewireAdaptiveResamplingConfig, PipewireBufferConfig, PipewireWriter,
};

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
            AudioSamples::I32(v) => v.iter().map(|&s| (s as f64 / 8388608.0) as f32).collect(),
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
    #[cfg(target_os = "windows")]
    Asio(AsioWriter),
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
    ) -> Result<Self> {
        let pipewire_writer = PipewireWriter::new(
            sample_rate,
            channel_count,
            sink_target,
            enable_adaptive_resampling,
            output_sample_rate,
            buffer_config,
            adaptive_config,
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
        )?;
        Ok(AudioWriter::Pipewire(pipewire_writer))
    }

    #[cfg(target_os = "windows")]
    pub fn create_asio(
        input_sample_rate: u32,
        sample_rate: u32,
        channel_count: u32,
        device_name: Option<String>,
        target_latency_ms: u32,
        enable_adaptive_resampling: bool,
        adaptive_config: AdaptiveResamplingConfig,
    ) -> Result<Self> {
        let asio_writer = AsioWriter::new(
            input_sample_rate,
            sample_rate,
            channel_count,
            device_name,
            target_latency_ms,
            enable_adaptive_resampling,
            adaptive_config,
        )?;
        Ok(AudioWriter::Asio(asio_writer))
    }

    pub fn write_pcm_samples(
        &mut self,
        samples: &AudioSamples,
        _channel_count: usize,
    ) -> Result<()> {
        #[cfg(not(any(
            target_os = "linux",
            target_os = "windows"
        )))]
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
            #[cfg(target_os = "windows")]
            AudioWriter::Asio(asio_writer) => {
                let samples_f32 = samples.to_f32();
                asio_writer.write_samples(&samples_f32)?;
                Ok(())
            }
            AudioWriter::Unsupported => Err(anyhow!("No supported realtime output backend")),
        }
    }

    pub fn close_and_drop(self) -> Result<()> {
        match self {
            #[cfg(target_os = "linux")]
            AudioWriter::Pipewire(mut w) => {
                w.flush()?;
                drop(w);
                Ok(())
            }
            #[cfg(target_os = "windows")]
            AudioWriter::Asio(mut w) => {
                w.flush()?;
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
            #[cfg(target_os = "windows")]
            AudioWriter::Asio(asio_writer) => {
                asio_writer.flush()?;
                Ok(())
            }
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
            #[cfg(target_os = "windows")]
            AudioWriter::Asio(asio) => Some(asio.latency_ms()),
            AudioWriter::Unsupported => None,
        }
    }

    /// Returns the current PI controller rate-adjust factor, or `None` if adaptive
    /// resampling is disabled or the backend does not support it.
    pub fn resample_ratio(&self) -> Option<f32> {
        match self {
            #[cfg(target_os = "linux")]
            AudioWriter::Pipewire(pw) => pw.rate_adjust(),
            #[cfg(target_os = "windows")]
            AudioWriter::Asio(asio) => asio.rate_adjust(),
            AudioWriter::Unsupported => None,
        }
    }

    pub fn adaptive_band(&self) -> Option<&'static str> {
        match self {
            #[cfg(target_os = "linux")]
            AudioWriter::Pipewire(pw) => pw.adaptive_band(),
            #[cfg(target_os = "windows")]
            AudioWriter::Asio(asio) => asio.adaptive_band(),
            AudioWriter::Unsupported => None,
        }
    }

    /// Total audio delay in ms (ring-buffer target + backend graph latency).
    pub fn total_audio_delay_ms(&self) -> Option<f32> {
        match self {
            #[cfg(target_os = "linux")]
            AudioWriter::Pipewire(pw) => {
                let v = pw.total_audio_delay_ms();
                if v > 0.0 { Some(v) } else { None }
            }
            #[cfg(target_os = "windows")]
            AudioWriter::Asio(asio) => {
                let v = asio.total_audio_delay_ms();
                if v > 0.0 { Some(v) } else { None }
            }
            AudioWriter::Unsupported => None,
        }
    }

    /// Measured total audio delay in ms (current ring-buffer + backend graph latency).
    pub fn measured_audio_delay_ms(&self) -> Option<f32> {
        match self {
            #[cfg(target_os = "linux")]
            AudioWriter::Pipewire(pw) => {
                let v = pw.measured_audio_delay_ms();
                if v > 0.0 { Some(v) } else { None }
            }
            #[cfg(target_os = "windows")]
            AudioWriter::Asio(asio) => {
                let v = asio.measured_audio_delay_ms();
                if v > 0.0 { Some(v) } else { None }
            }
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
            #[cfg(target_os = "windows")]
            AudioWriter::Asio(asio) => {
                let v = asio.control_audio_delay_ms();
                if v > 0.0 { Some(v) } else { None }
            }
            AudioWriter::Unsupported => None,
        }
    }
}
