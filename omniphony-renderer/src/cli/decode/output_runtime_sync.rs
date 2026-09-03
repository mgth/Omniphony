use super::state::{OutputState, RuntimeOutputState};
use crate::cli::command::{OutputBackend, OutputFileFormatArg};
use anyhow::Result;
use audio_input::InputControl;
use audio_output::{AdaptiveResamplingConfig, AudioControl};
use std::str::FromStr;

/// Parse a live-requested `output_file_format` string into the CLI enum.
fn parse_output_file_format(s: &str) -> Option<OutputFileFormatArg> {
    match s.trim().to_ascii_lowercase().as_str() {
        "caf" => Some(OutputFileFormatArg::Caf),
        "raw_f32" | "rawf32" | "raw" | "f32" => Some(OutputFileFormatArg::RawF32),
        _ => None,
    }
}

pub struct OutputRuntimeCoordinator<'a> {
    output: &'a mut OutputState,
    runtime: &'a mut RuntimeOutputState,
    audio_control: Option<&'a AudioControl>,
    /// Loses its pacer handle whenever the writer is retired here; see
    /// [`OutputState::invalidate_writer`].
    input_control: Option<&'a InputControl>,
}

impl<'a> OutputRuntimeCoordinator<'a> {
    pub fn new(
        output: &'a mut OutputState,
        runtime: &'a mut RuntimeOutputState,
        audio_control: Option<&'a AudioControl>,
        input_control: Option<&'a InputControl>,
    ) -> Self {
        Self {
            output,
            runtime,
            audio_control,
            input_control,
        }
    }

    pub fn sync_all(&mut self) -> Result<()> {
        // Backend switch first: it may change `runtime.active_output_backend`,
        // which the device/rate/adaptive syncs below must then observe.
        self.sync_requested_output_backend()?;
        let output_backend = self.runtime.active_output_backend;
        self.sync_requested_output_device(output_backend)?;
        self.sync_requested_output_sample_rate(output_backend)?;
        self.sync_requested_adaptive_resampling(output_backend)?;
        self.sync_requested_latency_target(output_backend)?;
        self.sync_requested_adaptive_tuning(output_backend)?;
        self.sync_ratio_reset();
        Ok(())
    }

    /// Apply a live-requested output backend switch (e.g. device ↔ `file`) and
    /// any `file` destination/format change. A backend change always rebuilds
    /// the writer; a destination/format change rebuilds only while `file` is
    /// the active backend.
    fn sync_requested_output_backend(&mut self) -> Result<()> {
        let requested_backend = self
            .audio_control
            .and_then(|control| control.requested_output_backend())
            .and_then(|s| OutputBackend::from_str(&s).ok())
            .unwrap_or(self.runtime.active_output_backend);
        let requested_file = self
            .audio_control
            .and_then(|control| control.requested_output_file())
            .unwrap_or_else(|| self.runtime.output_file.clone());
        let requested_format = self
            .audio_control
            .and_then(|control| control.requested_output_file_format())
            .and_then(|s| parse_output_file_format(&s))
            .unwrap_or(self.runtime.output_file_format);

        let backend_changed = requested_backend != self.runtime.active_output_backend;
        let file_changed = requested_file != self.runtime.output_file
            || requested_format != self.runtime.output_file_format;

        if !backend_changed && !file_changed {
            return Ok(());
        }

        self.runtime.active_output_backend = requested_backend;
        self.runtime.output_file = requested_file;
        self.runtime.output_file_format = requested_format;
        log::info!(
            "Applying requested output backend: {:?} (file='{}', format={:?})",
            self.runtime.active_output_backend,
            self.runtime.output_file,
            self.runtime.output_file_format
        );

        if backend_changed || requested_backend == OutputBackend::File {
            self.flush_and_invalidate_writer();
        }

        Ok(())
    }

    fn flush_and_invalidate_writer(&mut self) {
        match output_backend_supported_for_runtime_reset() {
            true => {
                if let Some(mut writer) = self.output.invalidate_writer(self.input_control) {
                    let _ = writer.flush();
                }
                self.output.reset_realtime_output_tracking();
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
        #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
        {
            let _ = output_backend;
            return Ok(());
        }

        #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
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

        #[cfg(any(target_os = "windows", target_os = "macos"))]
        {
            let requested = self
                .audio_control
                .and_then(|control| control.requested_latency_target_ms())
                .unwrap_or(self.runtime.latency_target_ms);

            if requested != self.runtime.latency_target_ms {
                self.runtime.latency_target_ms = requested.max(1);
                log::info!(
                    "Applying requested output latency target: {} ms",
                    self.runtime.latency_target_ms
                );

                if output_backend.is_local_cpal() {
                    self.flush_and_invalidate_writer();
                }
            }
        }

        #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
        let _ = output_backend;

        Ok(())
    }

    fn sync_requested_adaptive_tuning(&mut self, _output_backend: OutputBackend) -> Result<()> {
        #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
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
                        hard_recover_high_in_far_mode: control
                            .requested_adaptive_resampling_hard_recover_high_in_far_mode(),
                        hard_recover_low_in_far_mode: control
                            .requested_adaptive_resampling_hard_recover_low_in_far_mode(),
                        far_mode_return_fade_in_ms: control
                            .requested_adaptive_resampling_far_mode_return_fade_in_ms(),
                        kp_near: kp,
                        ki: control.requested_adaptive_resampling_ki(),
                        integral_discharge_ratio: control
                            .requested_adaptive_resampling_integral_discharge_ratio(),
                        max_adjust,
                        update_interval_callbacks: control
                            .requested_adaptive_resampling_update_interval_callbacks()
                            .max(1),
                        high_recover_entry_margin_ms: control
                            .requested_adaptive_resampling_high_recover_entry_margin_ms(),
                        low_recover_settle_stable_ms: control
                            .requested_adaptive_resampling_low_recover_settle_stable_ms(),
                        low_recover_entry_margin_ms: control
                            .requested_adaptive_resampling_low_recover_entry_margin_ms(),
                        low_recover_exit_margin_ms: control
                            .requested_adaptive_resampling_low_recover_exit_margin_ms(),
                        low_recover_settle_margin_ms: control
                            .requested_adaptive_resampling_low_recover_settle_margin_ms(),
                        low_recover_refill_delta_alpha: control
                            .requested_adaptive_resampling_low_recover_refill_delta_alpha(),
                        control_smoothing_cutoff_hz: control
                            .requested_adaptive_resampling_control_smoothing_cutoff_hz(),
                        control_smoothing_order: control
                            .requested_adaptive_resampling_control_smoothing_order(),
                        paused: control.requested_adaptive_resampling_paused(),
                        use_pre_bridge_clock: control
                            .requested_adaptive_resampling_use_pre_bridge_clock(),
                        use_output_pacing: control
                            .requested_adaptive_resampling_use_output_pacing(),
                        disable_backpressure: control
                            .requested_adaptive_resampling_disable_backpressure(),
                    }
                })
                .unwrap_or_else(|| self.runtime.adaptive_resampling_config.clone());

            if requested.enable_far_mode == self.runtime.adaptive_resampling_config.enable_far_mode
                && requested.force_silence_in_far_mode
                    == self
                        .runtime
                        .adaptive_resampling_config
                        .force_silence_in_far_mode
                && requested.hard_recover_high_in_far_mode
                    == self
                        .runtime
                        .adaptive_resampling_config
                        .hard_recover_high_in_far_mode
                && requested.hard_recover_low_in_far_mode
                    == self
                        .runtime
                        .adaptive_resampling_config
                        .hard_recover_low_in_far_mode
                && requested.far_mode_return_fade_in_ms
                    == self
                        .runtime
                        .adaptive_resampling_config
                        .far_mode_return_fade_in_ms
                && requested.kp_near == self.runtime.adaptive_resampling_config.kp_near
                && requested.ki == self.runtime.adaptive_resampling_config.ki
                && requested.integral_discharge_ratio
                    == self
                        .runtime
                        .adaptive_resampling_config
                        .integral_discharge_ratio
                && requested.max_adjust == self.runtime.adaptive_resampling_config.max_adjust
                && requested.update_interval_callbacks
                    == self
                        .runtime
                        .adaptive_resampling_config
                        .update_interval_callbacks
                && requested.high_recover_entry_margin_ms
                    == self
                        .runtime
                        .adaptive_resampling_config
                        .high_recover_entry_margin_ms
                && requested.low_recover_settle_stable_ms
                    == self
                        .runtime
                        .adaptive_resampling_config
                        .low_recover_settle_stable_ms
                && requested.low_recover_entry_margin_ms
                    == self
                        .runtime
                        .adaptive_resampling_config
                        .low_recover_entry_margin_ms
                && requested.low_recover_exit_margin_ms
                    == self
                        .runtime
                        .adaptive_resampling_config
                        .low_recover_exit_margin_ms
                && requested.low_recover_settle_margin_ms
                    == self
                        .runtime
                        .adaptive_resampling_config
                        .low_recover_settle_margin_ms
                && requested.low_recover_refill_delta_alpha
                    == self
                        .runtime
                        .adaptive_resampling_config
                        .low_recover_refill_delta_alpha
                && requested.control_smoothing_cutoff_hz
                    == self
                        .runtime
                        .adaptive_resampling_config
                        .control_smoothing_cutoff_hz
                && requested.control_smoothing_order
                    == self
                        .runtime
                        .adaptive_resampling_config
                        .control_smoothing_order
                && requested.paused == self.runtime.adaptive_resampling_config.paused
                && requested.use_pre_bridge_clock
                    == self.runtime.adaptive_resampling_config.use_pre_bridge_clock
                && requested.use_output_pacing
                    == self.runtime.adaptive_resampling_config.use_output_pacing
                && requested.disable_backpressure
                    == self.runtime.adaptive_resampling_config.disable_backpressure
            {
                return Ok(());
            }

            self.runtime.adaptive_resampling_config = requested;
            log::info!(
                "Applying adaptive resampling tuning (live): far_mode={}, far_silence={}, hard_recover_high={}, hard_recover_low={}, far_return_fade_in_ms={}, kp={:.4}, ki={:.4}, integral_discharge_ratio={:.4}, max_adjust={:.6}, update_interval_callbacks={}, far_threshold_ms={}, low_recover_settle_stable_ms={:.2}, low_recover_entry_margin_ms={:.2}, low_recover_exit_margin_ms={:.2}, low_recover_settle_margin_ms={:.2}, low_recover_refill_delta_alpha={:.3}, control_smoothing_cutoff_hz={:.3}, control_smoothing_order={}, use_pre_bridge_clock={}, use_output_pacing={}, disable_backpressure={}",
                self.runtime.adaptive_resampling_config.enable_far_mode,
                self.runtime
                    .adaptive_resampling_config
                    .force_silence_in_far_mode,
                self.runtime
                    .adaptive_resampling_config
                    .hard_recover_high_in_far_mode,
                self.runtime
                    .adaptive_resampling_config
                    .hard_recover_low_in_far_mode,
                self.runtime
                    .adaptive_resampling_config
                    .far_mode_return_fade_in_ms,
                self.runtime.adaptive_resampling_config.kp_near,
                self.runtime.adaptive_resampling_config.ki,
                self.runtime
                    .adaptive_resampling_config
                    .integral_discharge_ratio,
                self.runtime.adaptive_resampling_config.max_adjust,
                self.runtime
                    .adaptive_resampling_config
                    .update_interval_callbacks,
                self.runtime
                    .adaptive_resampling_config
                    .high_recover_entry_margin_ms,
                self.runtime
                    .adaptive_resampling_config
                    .low_recover_settle_stable_ms,
                self.runtime
                    .adaptive_resampling_config
                    .low_recover_entry_margin_ms,
                self.runtime
                    .adaptive_resampling_config
                    .low_recover_exit_margin_ms,
                self.runtime
                    .adaptive_resampling_config
                    .low_recover_settle_margin_ms,
                self.runtime
                    .adaptive_resampling_config
                    .low_recover_refill_delta_alpha,
                self.runtime
                    .adaptive_resampling_config
                    .control_smoothing_cutoff_hz,
                self.runtime
                    .adaptive_resampling_config
                    .control_smoothing_order,
                self.runtime.adaptive_resampling_config.use_pre_bridge_clock,
                self.runtime.adaptive_resampling_config.use_output_pacing,
                self.runtime.adaptive_resampling_config.disable_backpressure,
            );

            // Apply live on the running audio thread — no restart needed.
            self.output
                .update_adaptive_config(self.runtime.adaptive_resampling_config.clone());
        }

        #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
        let _ = output_backend;

        Ok(())
    }

    fn sync_ratio_reset(&mut self) {
        #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
        if let Some(audio) = self.audio_control {
            if audio.take_ratio_reset() {
                self.output.request_ratio_reset();
            }
        }
    }
}

fn should_reset_writer(output_backend: OutputBackend) -> bool {
    match output_backend {
        #[cfg(target_os = "linux")]
        OutputBackend::Pipewire => true,
        #[cfg(target_os = "windows")]
        OutputBackend::Asio => true,
        #[cfg(target_os = "macos")]
        OutputBackend::Coreaudio => true,
        // The file/FIFO/stdout sink is rebuilt live when its destination,
        // format or a relevant parameter changes.
        OutputBackend::File => true,
        _ => false,
    }
}

fn output_backend_supported_for_runtime_reset() -> bool {
    #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
    {
        true
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        false
    }
}
