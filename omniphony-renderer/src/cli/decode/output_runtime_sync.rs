use super::state::{OutputState, RuntimeOutputState};
use crate::cli::command::OutputBackend;
use anyhow::Result;
use audio_output::{AdaptiveResamplingConfig, AudioControl};

pub struct OutputRuntimeCoordinator<'a> {
    output: &'a mut OutputState,
    runtime: &'a mut RuntimeOutputState,
    audio_control: Option<&'a AudioControl>,
}

impl<'a> OutputRuntimeCoordinator<'a> {
    pub fn new(
        output: &'a mut OutputState,
        runtime: &'a mut RuntimeOutputState,
        audio_control: Option<&'a AudioControl>,
    ) -> Self {
        Self {
            output,
            runtime,
            audio_control,
        }
    }

    pub fn sync_all(&mut self, output_backend: OutputBackend) -> Result<()> {
        self.sync_requested_output_device(output_backend)?;
        self.sync_requested_output_sample_rate(output_backend)?;
        self.sync_requested_adaptive_resampling(output_backend)?;
        self.sync_requested_latency_target(output_backend)?;
        self.sync_requested_adaptive_tuning(output_backend)?;
        Ok(())
    }

    fn flush_and_invalidate_writer(&mut self) {
        match output_backend_supported_for_runtime_reset() {
            true => {
                if let Some(mut writer) = self.output.invalidate_writer() {
                    let _ = writer.flush();
                }
            }
            false => {}
        }
    }

    fn sync_requested_output_sample_rate(&mut self, output_backend: OutputBackend) -> Result<()> {
        let requested = self
            .audio_control
            .and_then(|control| control.requested_output_sample_rate())
            .or(self.runtime.output_sample_rate);

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

        if should_reset_writer(output_backend) {
            self.flush_and_invalidate_writer();
        }

        Ok(())
    }

    fn sync_requested_output_device(&mut self, output_backend: OutputBackend) -> Result<()> {
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        {
            let _ = output_backend;
            return Ok(());
        }

        #[cfg(any(target_os = "linux", target_os = "windows"))]
        {
            let requested = self
                .audio_control
                .and_then(|control| control.requested_output_device())
                .or_else(|| self.runtime.output_device.clone());

            if requested == self.runtime.output_device {
                return Ok(());
            }

            self.runtime.output_device = requested.clone();
            log::info!(
                "Applying requested output device: {}",
                requested.unwrap_or_else(|| "default".to_string())
            );

            if should_reset_writer(output_backend) {
                self.flush_and_invalidate_writer();
            }

            Ok(())
        }
    }

    fn sync_requested_adaptive_resampling(&mut self, output_backend: OutputBackend) -> Result<()> {
        let requested = self
            .audio_control
            .map(|control| control.requested_adaptive_resampling())
            .unwrap_or(self.runtime.enable_adaptive_resampling);

        if requested == self.runtime.enable_adaptive_resampling {
            return Ok(());
        }

        self.runtime.enable_adaptive_resampling = requested;
        log::info!(
            "Applying requested adaptive resampling: {}",
            if requested { "enabled" } else { "disabled" }
        );

        if should_reset_writer(output_backend) {
            self.flush_and_invalidate_writer();
        }

        Ok(())
    }

    fn sync_requested_latency_target(&mut self, output_backend: OutputBackend) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            let requested = self
                .audio_control
                .and_then(|control| control.requested_latency_target_ms())
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

            if matches!(output_backend, OutputBackend::Pipewire) {
                self.flush_and_invalidate_writer();
            }
        }

        #[cfg(target_os = "windows")]
        {
            let requested = self
                .audio_control
                .and_then(|control| control.requested_latency_target_ms())
                .unwrap_or(self.runtime.asio_target_latency_ms);

            if requested != self.runtime.asio_target_latency_ms {
                self.runtime.asio_target_latency_ms = requested.max(1);
                log::info!(
                    "Applying requested ASIO latency target: {} ms",
                    self.runtime.asio_target_latency_ms
                );

                if matches!(output_backend, OutputBackend::Asio) {
                    self.flush_and_invalidate_writer();
                }
            }
        }

        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        let _ = output_backend;

        Ok(())
    }

    fn sync_requested_adaptive_tuning(&mut self, output_backend: OutputBackend) -> Result<()> {
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        {
            let requested = self
                .audio_control
                .map(|control| {
                    let kp = control.requested_adaptive_resampling_kp_near();
                    let max_adjust = control.requested_adaptive_resampling_max_adjust();
                    AdaptiveResamplingConfig {
                        enable_far_mode: control.requested_adaptive_resampling_enable_far_mode(),
                        force_silence_in_far_mode: control
                            .requested_adaptive_resampling_force_silence_in_far_mode(),
                        hard_recover_in_far_mode: true,
                        far_mode_return_fade_in_ms: control
                            .requested_adaptive_resampling_far_mode_return_fade_in_ms(),
                        kp_near: kp,
                        kp_far: kp,
                        ki: control.requested_adaptive_resampling_ki(),
                        max_adjust,
                        max_adjust_far: max_adjust,
                        update_interval_callbacks: control
                            .requested_adaptive_resampling_update_interval_callbacks()
                            .max(1),
                        near_far_threshold_ms: control
                            .requested_adaptive_resampling_near_far_threshold_ms(),
                        measurement_smoothing_alpha: control
                            .requested_adaptive_resampling_measurement_smoothing_alpha(),
                    }
                })
                .unwrap_or_else(|| self.runtime.adaptive_resampling_config.clone());

            if requested.enable_far_mode == self.runtime.adaptive_resampling_config.enable_far_mode
                && requested.force_silence_in_far_mode
                    == self
                        .runtime
                        .adaptive_resampling_config
                        .force_silence_in_far_mode
                && requested.far_mode_return_fade_in_ms
                    == self
                        .runtime
                        .adaptive_resampling_config
                        .far_mode_return_fade_in_ms
                && requested.kp_near == self.runtime.adaptive_resampling_config.kp_near
                && requested.ki == self.runtime.adaptive_resampling_config.ki
                && requested.max_adjust == self.runtime.adaptive_resampling_config.max_adjust
                && requested.update_interval_callbacks
                    == self
                        .runtime
                        .adaptive_resampling_config
                        .update_interval_callbacks
                && requested.near_far_threshold_ms
                    == self
                        .runtime
                        .adaptive_resampling_config
                        .near_far_threshold_ms
                && requested.measurement_smoothing_alpha
                    == self
                        .runtime
                        .adaptive_resampling_config
                        .measurement_smoothing_alpha
            {
                return Ok(());
            }

            self.runtime.adaptive_resampling_config = requested;
            log::info!(
                "Applying adaptive resampling tuning: far_mode={}, far_silence={}, hard_recover=forced, far_return_fade_in_ms={}, kp={:.8}, ki={:.8}, max_adjust={:.6}, update_interval_callbacks={}, far_threshold_ms={}, measurement_smoothing_alpha={:.3}",
                self.runtime.adaptive_resampling_config.enable_far_mode,
                self.runtime
                    .adaptive_resampling_config
                    .force_silence_in_far_mode,
                self.runtime
                    .adaptive_resampling_config
                    .far_mode_return_fade_in_ms,
                self.runtime.adaptive_resampling_config.kp_near,
                self.runtime.adaptive_resampling_config.ki,
                self.runtime.adaptive_resampling_config.max_adjust,
                self.runtime
                    .adaptive_resampling_config
                    .update_interval_callbacks,
                self.runtime
                    .adaptive_resampling_config
                    .near_far_threshold_ms,
                self.runtime
                    .adaptive_resampling_config
                    .measurement_smoothing_alpha
            );

            if should_reset_writer(output_backend) {
                self.flush_and_invalidate_writer();
            }
        }

        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        let _ = output_backend;

        Ok(())
    }
}

fn should_reset_writer(output_backend: OutputBackend) -> bool {
    match output_backend {
        #[cfg(target_os = "linux")]
        OutputBackend::Pipewire => true,
        #[cfg(target_os = "windows")]
        OutputBackend::Asio => true,
        _ => false,
    }
}

fn output_backend_supported_for_runtime_reset() -> bool {
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    {
        true
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        false
    }
}
