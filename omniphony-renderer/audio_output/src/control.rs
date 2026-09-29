use crate::AdaptiveResamplingConfig;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct OutputDeviceOption {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct RequestedAudioOutputConfig {
    pub output_device: Option<String>,
    pub output_sample_rate_hz: Option<u32>,
    pub latency_target_ms: Option<u32>,
    pub adaptive_enabled: bool,
    pub adaptive: AdaptiveResamplingConfig,
    /// Live-requested output backend id ("pipewire" / "asio" / "file"), or
    /// `None` to keep the active one. Parsed by the CLI's runtime sync layer
    /// (this crate stays decoupled from the CLI's `OutputBackend` enum).
    pub output_backend: Option<String>,
    /// Live-requested destination for the `file` backend (`-` = stdout, or a
    /// file/FIFO path).
    pub output_file: Option<String>,
    /// Live-requested `file` backend encoding ("raw_f32" / "caf").
    pub output_file_format: Option<String>,
}

impl Default for RequestedAudioOutputConfig {
    fn default() -> Self {
        Self {
            output_device: None,
            output_sample_rate_hz: None,
            latency_target_ms: None,
            adaptive_enabled: false,
            adaptive: AdaptiveResamplingConfig::default(),
            output_backend: None,
            output_file: None,
            output_file_format: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AppliedAudioOutputState {
    pub output_device: Option<String>,
    pub output_sample_rate_hz: Option<u32>,
    pub sample_format: String,
    pub audio_error: Option<String>,
}

pub struct AudioControl {
    requested: Mutex<RequestedAudioOutputConfig>,
    applied: Mutex<AppliedAudioOutputState>,
    available_output_devices: Mutex<Vec<OutputDeviceOption>>,
    device_list_fetcher: Mutex<Option<Box<dyn Fn() -> Vec<OutputDeviceOption> + Send + Sync>>>,
    reset_ratio_pending: AtomicBool,
}

impl Default for AudioControl {
    fn default() -> Self {
        Self::new(RequestedAudioOutputConfig::default())
    }
}

impl AudioControl {
    pub fn new(requested: RequestedAudioOutputConfig) -> Self {
        Self {
            requested: Mutex::new(requested),
            applied: Mutex::new(AppliedAudioOutputState::default()),
            available_output_devices: Mutex::new(Vec::new()),
            device_list_fetcher: Mutex::new(None),
            reset_ratio_pending: AtomicBool::new(false),
        }
    }

    pub fn requested_snapshot(&self) -> RequestedAudioOutputConfig {
        self.requested.lock().clone()
    }

    pub fn update_requested(&self, f: impl FnOnce(&mut RequestedAudioOutputConfig)) {
        let mut requested = self.requested.lock();
        f(&mut requested);
    }

    pub fn applied_snapshot(&self) -> AppliedAudioOutputState {
        self.applied.lock().clone()
    }

    pub fn update_applied(&self, f: impl FnOnce(&mut AppliedAudioOutputState)) {
        let mut applied = self.applied.lock();
        f(&mut applied);
    }

    pub fn set_requested_output_device(&self, output_device: Option<String>) {
        self.update_requested(|requested| requested.output_device = output_device);
    }

    pub fn requested_output_device(&self) -> Option<String> {
        self.requested_snapshot().output_device
    }

    pub fn set_requested_output_backend(&self, backend: Option<String>) {
        self.update_requested(|requested| requested.output_backend = backend);
    }

    pub fn requested_output_backend(&self) -> Option<String> {
        self.requested_snapshot().output_backend
    }

    pub fn set_requested_output_file(&self, output_file: Option<String>) {
        self.update_requested(|requested| requested.output_file = output_file);
    }

    pub fn requested_output_file(&self) -> Option<String> {
        self.requested_snapshot().output_file
    }

    pub fn set_requested_output_file_format(&self, format: Option<String>) {
        self.update_requested(|requested| requested.output_file_format = format);
    }

    pub fn requested_output_file_format(&self) -> Option<String> {
        self.requested_snapshot().output_file_format
    }

    pub fn set_requested_output_sample_rate(&self, rate_hz: Option<u32>) {
        self.update_requested(|requested| requested.output_sample_rate_hz = rate_hz);
    }

    pub fn requested_output_sample_rate(&self) -> Option<u32> {
        self.requested_snapshot().output_sample_rate_hz
    }

    pub fn set_requested_latency_target_ms(&self, value: Option<u32>) {
        self.update_requested(|requested| requested.latency_target_ms = value);
    }

    pub fn requested_latency_target_ms(&self) -> Option<u32> {
        self.requested_snapshot().latency_target_ms
    }

    pub fn set_requested_adaptive_resampling(&self, enabled: bool) {
        self.update_requested(|requested| requested.adaptive_enabled = enabled);
    }

    pub fn requested_adaptive_resampling(&self) -> bool {
        self.requested_snapshot().adaptive_enabled
    }

    /// The requested adaptive-resampling tuning, copied out under one lock
    /// (the rest of the requested config is not cloned).
    pub fn requested_adaptive_config(&self) -> AdaptiveResamplingConfig {
        self.requested.lock().adaptive.clone()
    }

    pub fn set_requested_adaptive_resampling_enable_far_mode(&self, enabled: bool) {
        self.update_requested(|requested| requested.adaptive.enable_far_mode = enabled);
    }

    pub fn set_requested_adaptive_resampling_force_silence_in_far_mode(&self, enabled: bool) {
        self.update_requested(|requested| requested.adaptive.force_silence_in_far_mode = enabled);
    }

    pub fn set_requested_adaptive_resampling_hard_recover_high_in_far_mode(&self, enabled: bool) {
        self.update_requested(|requested| {
            requested.adaptive.hard_recover_high_in_far_mode = enabled
        });
    }

    pub fn set_requested_adaptive_resampling_hard_recover_low_in_far_mode(&self, enabled: bool) {
        self.update_requested(|requested| {
            requested.adaptive.hard_recover_low_in_far_mode = enabled
        });
    }

    pub fn set_requested_adaptive_resampling_far_mode_return_fade_in_ms(&self, value: u32) {
        self.update_requested(|requested| requested.adaptive.far_mode_return_fade_in_ms = value);
    }

    pub fn set_requested_adaptive_resampling_kp_near(&self, value: f32) {
        self.update_requested(|requested| requested.adaptive.kp_near = value as f64);
    }

    pub fn set_requested_adaptive_resampling_ki(&self, value: f32) {
        self.update_requested(|requested| requested.adaptive.ki = value as f64);
    }

    pub fn set_requested_adaptive_resampling_integral_discharge_ratio(&self, value: f32) {
        self.update_requested(|requested| {
            requested.adaptive.integral_discharge_ratio = value as f64;
        });
    }

    pub fn set_requested_adaptive_resampling_max_adjust(&self, value: f32) {
        self.update_requested(|requested| requested.adaptive.max_adjust = value as f64);
    }

    pub fn set_requested_adaptive_resampling_update_interval_callbacks(&self, value: u32) {
        self.update_requested(|requested| requested.adaptive.update_interval_callbacks = value);
    }

    pub fn set_requested_adaptive_resampling_high_recover_entry_margin_ms(&self, value: u32) {
        self.update_requested(|requested| requested.adaptive.high_recover_entry_margin_ms = value);
    }

    pub fn set_requested_adaptive_resampling_low_recover_settle_stable_ms(&self, value: f32) {
        self.update_requested(|requested| {
            requested.adaptive.low_recover_settle_stable_ms = value;
        });
    }

    pub fn set_requested_adaptive_resampling_low_recover_entry_margin_ms(&self, value: f32) {
        self.update_requested(|requested| {
            requested.adaptive.low_recover_entry_margin_ms = value;
        });
    }

    pub fn set_requested_adaptive_resampling_low_recover_exit_margin_ms(&self, value: f32) {
        self.update_requested(|requested| {
            requested.adaptive.low_recover_exit_margin_ms = value;
        });
    }

    pub fn set_requested_adaptive_resampling_low_recover_settle_margin_ms(&self, value: f32) {
        self.update_requested(|requested| {
            requested.adaptive.low_recover_settle_margin_ms = value;
        });
    }

    pub fn set_requested_adaptive_resampling_low_recover_refill_delta_alpha(&self, value: f32) {
        self.update_requested(|requested| {
            requested.adaptive.low_recover_refill_delta_alpha = value;
        });
    }

    pub fn set_requested_adaptive_resampling_control_smoothing_cutoff_hz(&self, value: f32) {
        self.update_requested(|requested| {
            requested.adaptive.control_smoothing_cutoff_hz = value as f64;
        });
    }

    pub fn set_requested_adaptive_resampling_control_smoothing_order(&self, value: u32) {
        self.update_requested(|requested| {
            requested.adaptive.control_smoothing_order = value.clamp(1, 2);
        });
    }

    pub fn set_requested_adaptive_resampling_paused(&self, paused: bool) {
        self.update_requested(|requested| requested.adaptive.paused = paused);
    }

    pub fn set_requested_adaptive_resampling_use_pre_bridge_clock(&self, enabled: bool) {
        self.update_requested(|requested| requested.adaptive.use_pre_bridge_clock = enabled);
    }

    pub fn set_requested_adaptive_resampling_use_output_pacing(&self, enabled: bool) {
        self.update_requested(|requested| requested.adaptive.use_output_pacing = enabled);
    }

    pub fn set_requested_adaptive_resampling_disable_backpressure(&self, disabled: bool) {
        self.update_requested(|requested| requested.adaptive.disable_backpressure = disabled);
    }

    /// Request a one-shot ratio reset. Consumed by the sync loop via `take_ratio_reset`.
    pub fn request_ratio_reset(&self) {
        self.reset_ratio_pending.store(true, Ordering::Relaxed);
    }

    /// Returns true and clears the pending flag if a reset was requested.
    pub fn take_ratio_reset(&self) -> bool {
        self.reset_ratio_pending
            .compare_exchange(true, false, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    }

    pub fn set_available_output_devices(&self, devices: Vec<OutputDeviceOption>) {
        *self.available_output_devices.lock() = devices;
    }

    pub fn available_output_devices(&self) -> Vec<OutputDeviceOption> {
        self.available_output_devices.lock().clone()
    }

    pub fn set_device_list_fetcher(
        &self,
        fetcher: impl Fn() -> Vec<OutputDeviceOption> + Send + Sync + 'static,
    ) {
        *self.device_list_fetcher.lock() = Some(Box::new(fetcher));
    }

    pub fn refresh_available_output_devices(&self) -> Option<Vec<OutputDeviceOption>> {
        let fetcher = self.device_list_fetcher.lock();
        fetcher.as_ref().map(|f| {
            let devices = f();
            *self.available_output_devices.lock() = devices.clone();
            devices
        })
    }

    pub fn set_audio_state(&self, sample_rate_hz: u32, sample_format: impl Into<String>) {
        self.update_applied(|applied| {
            applied.output_sample_rate_hz = Some(sample_rate_hz);
            applied.sample_format = sample_format.into();
        });
    }

    pub fn set_effective_output_device(&self, output_device: Option<String>) {
        self.update_applied(|applied| applied.output_device = output_device);
    }

    pub fn set_audio_error(&self, error: Option<String>) {
        self.update_applied(|applied| applied.audio_error = error);
    }

    pub fn audio_state(&self) -> (Option<u32>, String) {
        let applied = self.applied_snapshot();
        (applied.output_sample_rate_hz, applied.sample_format)
    }

    pub fn audio_error(&self) -> Option<String> {
        self.applied_snapshot().audio_error
    }

    pub fn effective_output_device(&self) -> Option<String> {
        self.applied_snapshot().output_device
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one snapshot the live sync reads carries every setter's value, and
    /// compares unequal to the config it replaces.
    #[test]
    fn requested_adaptive_config_reflects_the_setters() {
        let control = AudioControl::default();
        let before = control.requested_adaptive_config();
        assert_eq!(before, AdaptiveResamplingConfig::default());

        control.set_requested_adaptive_resampling_ki(3.0);
        control.set_requested_adaptive_resampling_use_output_pacing(true);
        let after = control.requested_adaptive_config();
        assert_eq!(after.ki, 3.0);
        assert!(after.use_output_pacing);
        assert_ne!(after, before);
    }
}
