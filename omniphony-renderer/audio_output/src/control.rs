use crate::AdaptiveResamplingConfig;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

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
}

impl Default for RequestedAudioOutputConfig {
    fn default() -> Self {
        Self {
            output_device: None,
            output_sample_rate_hz: None,
            latency_target_ms: None,
            adaptive_enabled: false,
            adaptive: AdaptiveResamplingConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AppliedAudioOutputState {
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
        self.requested.lock().unwrap().clone()
    }

    pub fn update_requested(&self, f: impl FnOnce(&mut RequestedAudioOutputConfig)) {
        let mut requested = self.requested.lock().unwrap();
        f(&mut requested);
    }

    pub fn applied_snapshot(&self) -> AppliedAudioOutputState {
        self.applied.lock().unwrap().clone()
    }

    pub fn update_applied(&self, f: impl FnOnce(&mut AppliedAudioOutputState)) {
        let mut applied = self.applied.lock().unwrap();
        f(&mut applied);
    }

    pub fn set_requested_output_device(&self, output_device: Option<String>) {
        self.update_requested(|requested| requested.output_device = output_device);
    }

    pub fn requested_output_device(&self) -> Option<String> {
        self.requested_snapshot().output_device
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

    pub fn set_requested_adaptive_resampling_enable_far_mode(&self, enabled: bool) {
        self.update_requested(|requested| requested.adaptive.enable_far_mode = enabled);
    }

    pub fn requested_adaptive_resampling_enable_far_mode(&self) -> bool {
        self.requested_snapshot().adaptive.enable_far_mode
    }

    pub fn set_requested_adaptive_resampling_force_silence_in_far_mode(&self, enabled: bool) {
        self.update_requested(|requested| requested.adaptive.force_silence_in_far_mode = enabled);
    }

    pub fn requested_adaptive_resampling_force_silence_in_far_mode(&self) -> bool {
        self.requested_snapshot().adaptive.force_silence_in_far_mode
    }

    pub fn set_requested_adaptive_resampling_hard_recover_in_far_mode(&self, enabled: bool) {
        self.update_requested(|requested| requested.adaptive.hard_recover_in_far_mode = enabled);
    }

    pub fn requested_adaptive_resampling_hard_recover_in_far_mode(&self) -> bool {
        self.requested_snapshot().adaptive.hard_recover_in_far_mode
    }

    pub fn set_requested_adaptive_resampling_far_mode_return_fade_in_ms(&self, value: u32) {
        self.update_requested(|requested| requested.adaptive.far_mode_return_fade_in_ms = value);
    }

    pub fn requested_adaptive_resampling_far_mode_return_fade_in_ms(&self) -> u32 {
        self.requested_snapshot()
            .adaptive
            .far_mode_return_fade_in_ms
    }

    pub fn set_requested_adaptive_resampling_kp_near(&self, value: f32) {
        self.update_requested(|requested| requested.adaptive.kp_near = value as f64);
    }

    pub fn requested_adaptive_resampling_kp_near(&self) -> f64 {
        self.requested_snapshot().adaptive.kp_near
    }

    pub fn set_requested_adaptive_resampling_ki(&self, value: f32) {
        self.update_requested(|requested| requested.adaptive.ki = value as f64);
    }

    pub fn requested_adaptive_resampling_ki(&self) -> f64 {
        self.requested_snapshot().adaptive.ki
    }

    pub fn set_requested_adaptive_resampling_integral_discharge_ratio(&self, value: f32) {
        self.update_requested(|requested| {
            requested.adaptive.integral_discharge_ratio = value as f64;
        });
    }

    pub fn requested_adaptive_resampling_integral_discharge_ratio(&self) -> f64 {
        self.requested_snapshot().adaptive.integral_discharge_ratio
    }

    pub fn set_requested_adaptive_resampling_max_adjust(&self, value: f32) {
        self.update_requested(|requested| requested.adaptive.max_adjust = value as f64);
    }

    pub fn requested_adaptive_resampling_max_adjust(&self) -> f64 {
        self.requested_snapshot().adaptive.max_adjust
    }

    pub fn set_requested_adaptive_resampling_update_interval_callbacks(&self, value: u32) {
        self.update_requested(|requested| requested.adaptive.update_interval_callbacks = value);
    }

    pub fn requested_adaptive_resampling_update_interval_callbacks(&self) -> u32 {
        self.requested_snapshot().adaptive.update_interval_callbacks
    }

    pub fn set_requested_adaptive_resampling_near_far_threshold_ms(&self, value: u32) {
        self.update_requested(|requested| requested.adaptive.near_far_threshold_ms = value);
    }

    pub fn requested_adaptive_resampling_near_far_threshold_ms(&self) -> u32 {
        self.requested_snapshot().adaptive.near_far_threshold_ms
    }

    pub fn set_requested_adaptive_resampling_paused(&self, paused: bool) {
        self.update_requested(|requested| requested.adaptive.paused = paused);
    }

    pub fn requested_adaptive_resampling_paused(&self) -> bool {
        self.requested_snapshot().adaptive.paused
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
        *self.available_output_devices.lock().unwrap() = devices;
    }

    pub fn available_output_devices(&self) -> Vec<OutputDeviceOption> {
        self.available_output_devices.lock().unwrap().clone()
    }

    pub fn set_device_list_fetcher(
        &self,
        fetcher: impl Fn() -> Vec<OutputDeviceOption> + Send + Sync + 'static,
    ) {
        *self.device_list_fetcher.lock().unwrap() = Some(Box::new(fetcher));
    }

    pub fn refresh_available_output_devices(&self) -> Option<Vec<OutputDeviceOption>> {
        let fetcher = self.device_list_fetcher.lock().unwrap();
        fetcher.as_ref().map(|f| {
            let devices = f();
            *self.available_output_devices.lock().unwrap() = devices.clone();
            devices
        })
    }

    pub fn set_audio_state(&self, sample_rate_hz: u32, sample_format: impl Into<String>) {
        self.update_applied(|applied| {
            applied.output_sample_rate_hz = Some(sample_rate_hz);
            applied.sample_format = sample_format.into();
        });
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
}
