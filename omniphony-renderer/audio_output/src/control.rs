use crate::AdaptiveResamplingConfig;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering},
};

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InputMode {
    Bridge,
    Live,
    PipewireBridge,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InputBackend {
    Pipewire,
    Asio,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum InputMapMode {
    SevenOneFixed,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InputLfeMode {
    Object,
    Direct,
    Drop,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InputSampleFormat {
    F32,
    S16,
}

#[derive(Debug, Clone)]
pub struct RequestedAudioInputConfig {
    pub mode: InputMode,
    pub backend: Option<InputBackend>,
    pub node_name: Option<String>,
    pub node_description: Option<String>,
    pub layout_path: Option<std::path::PathBuf>,
    pub channels: Option<u16>,
    pub sample_rate_hz: Option<u32>,
    pub sample_format: Option<InputSampleFormat>,
    pub map_mode: InputMapMode,
    pub lfe_mode: InputLfeMode,
}

impl Default for RequestedAudioInputConfig {
    fn default() -> Self {
        Self {
            mode: InputMode::Bridge,
            backend: None,
            node_name: None,
            node_description: None,
            layout_path: None,
            channels: None,
            sample_rate_hz: None,
            sample_format: None,
            map_mode: InputMapMode::SevenOneFixed,
            lfe_mode: InputLfeMode::Direct,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AppliedAudioInputState {
    pub active_mode: InputMode,
    pub backend: Option<InputBackend>,
    pub channels: Option<u16>,
    pub sample_rate_hz: Option<u32>,
    pub node_name: Option<String>,
    pub stream_format: Option<String>,
    pub input_error: Option<String>,
}

impl Default for AppliedAudioInputState {
    fn default() -> Self {
        Self {
            active_mode: InputMode::Bridge,
            backend: None,
            channels: None,
            sample_rate_hz: None,
            node_name: None,
            stream_format: None,
            input_error: None,
        }
    }
}

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

pub struct InputControl {
    requested: Mutex<RequestedAudioInputConfig>,
    applied: Mutex<AppliedAudioInputState>,
    apply_pending: AtomicBool,
    state_generation: AtomicU64,
    /// Rate-adjust feedback from the output resampler, for use by the input DRIVER clock.
    /// Encoded as f32 bits. 1.0 = no correction; <1.0 = output slowing down (DRIVER too fast).
    output_rate_adjust: Arc<AtomicU32>,
    /// Direct trigger mode: pending trigger counter owned by the writer's RT callback.
    /// Set by handler.rs after the writer is wired; capture mainloop clones and drains it.
    /// Mutex<None> until wired.
    pending_input_triggers: Mutex<Option<Arc<AtomicI64>>>,
    /// Sample rate of the capture stream (e.g. 192000), used for Bresenham scheduling on the
    /// output side. Set by register_direct_trigger_target().
    input_trigger_rate_hz: AtomicU32,
    /// When true, the capture mainloop drains pending_input_triggers instead of the timer schedule.
    direct_trigger_active: Arc<AtomicBool>,
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

    pub fn set_requested_adaptive_resampling_hard_recover_high_in_far_mode(&self, enabled: bool) {
        self.update_requested(|requested| requested.adaptive.hard_recover_high_in_far_mode = enabled);
    }

    pub fn requested_adaptive_resampling_hard_recover_high_in_far_mode(&self) -> bool {
        self.requested_snapshot().adaptive.hard_recover_high_in_far_mode
    }

    pub fn set_requested_adaptive_resampling_hard_recover_low_in_far_mode(&self, enabled: bool) {
        self.update_requested(|requested| requested.adaptive.hard_recover_low_in_far_mode = enabled);
    }

    pub fn requested_adaptive_resampling_hard_recover_low_in_far_mode(&self) -> bool {
        self.requested_snapshot().adaptive.hard_recover_low_in_far_mode
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

impl Default for InputControl {
    fn default() -> Self {
        Self::new(RequestedAudioInputConfig::default())
    }
}

impl InputControl {
    pub fn new(requested: RequestedAudioInputConfig) -> Self {
        Self {
            requested: Mutex::new(requested),
            applied: Mutex::new(AppliedAudioInputState::default()),
            apply_pending: AtomicBool::new(false),
            state_generation: AtomicU64::new(1),
            output_rate_adjust: Arc::new(AtomicU32::new(1.0f32.to_bits())),
            pending_input_triggers: Mutex::new(None),
            input_trigger_rate_hz: AtomicU32::new(0),
            direct_trigger_active: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Called by the output side to publish its current rate-adjust to the input DRIVER.
    pub fn set_output_rate_adjust(&self, rate: f32) {
        self.output_rate_adjust.store(rate.to_bits(), Ordering::Relaxed);
    }

    /// Returns a clone of the rate-adjust atomic, suitable for sharing with long-lived threads.
    pub fn output_rate_adjust_atomic(&self) -> Arc<AtomicU32> {
        Arc::clone(&self.output_rate_adjust)
    }

    /// Called by the capture side after stream.connect(): store the capture sample rate for the
    /// output-side Bresenham calculation.  Does NOT set direct_trigger_active — that happens from
    /// handler.rs once the writer is also ready.
    pub fn register_direct_trigger_target(&self, capture_rate_hz: u32) {
        self.input_trigger_rate_hz.store(capture_rate_hz, Ordering::Relaxed);
    }

    /// Returns the capture stream sample rate (set by register_direct_trigger_target).
    /// Returns 0 if not yet registered.
    pub fn input_trigger_rate_hz(&self) -> u32 {
        self.input_trigger_rate_hz.load(Ordering::Relaxed)
    }

    /// Called by handler.rs after the writer is ready: hand the writer's pending-trigger counter
    /// to InputControl so the capture mainloop can drain it.
    pub fn set_pending_input_triggers(&self, counter: Arc<AtomicI64>) {
        *self.pending_input_triggers.lock().unwrap() = Some(counter);
    }

    /// Returns a clone of the pending trigger counter, or None if not yet wired.
    pub fn pending_input_triggers(&self) -> Option<Arc<AtomicI64>> {
        self.pending_input_triggers.lock().unwrap().clone()
    }

    /// Returns a clone of the direct_trigger_active flag for sharing with the capture mainloop.
    pub fn direct_trigger_active_arc(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.direct_trigger_active)
    }

    /// Enable or disable direct trigger mode (output callback drives trigger counts).
    pub fn set_direct_trigger_active(&self, active: bool) {
        self.direct_trigger_active.store(active, Ordering::Relaxed);
    }

    fn bump_state_generation(&self) {
        self.state_generation.fetch_add(1, Ordering::Relaxed);
    }

    pub fn state_generation(&self) -> u64 {
        self.state_generation.load(Ordering::Relaxed)
    }

    pub fn requested_snapshot(&self) -> RequestedAudioInputConfig {
        self.requested.lock().unwrap().clone()
    }

    pub fn update_requested(&self, f: impl FnOnce(&mut RequestedAudioInputConfig)) {
        let mut requested = self.requested.lock().unwrap();
        f(&mut requested);
        drop(requested);
        self.bump_state_generation();
    }

    pub fn applied_snapshot(&self) -> AppliedAudioInputState {
        self.applied.lock().unwrap().clone()
    }

    pub fn update_applied(&self, f: impl FnOnce(&mut AppliedAudioInputState)) {
        let mut applied = self.applied.lock().unwrap();
        f(&mut applied);
        drop(applied);
        self.bump_state_generation();
    }

    pub fn request_apply(&self) {
        self.apply_pending.store(true, Ordering::Relaxed);
        self.bump_state_generation();
    }

    pub fn take_apply_pending(&self) -> bool {
        self.apply_pending
            .compare_exchange(true, false, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    }

    pub fn is_apply_pending(&self) -> bool {
        self.apply_pending.load(Ordering::Relaxed)
    }

    pub fn set_requested_mode(&self, mode: InputMode) {
        self.update_requested(|requested| requested.mode = mode);
    }

    pub fn set_requested_backend(&self, backend: Option<InputBackend>) {
        self.update_requested(|requested| requested.backend = backend);
    }

    pub fn set_requested_node_name(&self, value: Option<String>) {
        self.update_requested(|requested| requested.node_name = value);
    }

    pub fn set_requested_node_description(&self, value: Option<String>) {
        self.update_requested(|requested| requested.node_description = value);
    }

    pub fn set_requested_layout_path(&self, value: Option<std::path::PathBuf>) {
        self.update_requested(|requested| requested.layout_path = value);
    }

    pub fn set_requested_channels(&self, value: Option<u16>) {
        self.update_requested(|requested| requested.channels = value);
    }

    pub fn set_requested_sample_rate_hz(&self, value: Option<u32>) {
        self.update_requested(|requested| requested.sample_rate_hz = value);
    }

    pub fn set_requested_sample_format(&self, value: Option<InputSampleFormat>) {
        self.update_requested(|requested| requested.sample_format = value);
    }

    pub fn set_requested_map_mode(&self, value: InputMapMode) {
        self.update_requested(|requested| requested.map_mode = value);
    }

    pub fn set_requested_lfe_mode(&self, value: InputLfeMode) {
        self.update_requested(|requested| requested.lfe_mode = value);
    }

    pub fn set_input_state(
        &self,
        mode: InputMode,
        backend: Option<InputBackend>,
        channels: Option<u16>,
        sample_rate_hz: Option<u32>,
        node_name: Option<String>,
        stream_format: Option<String>,
    ) {
        self.update_applied(|applied| {
            applied.active_mode = mode;
            applied.backend = backend;
            applied.channels = channels;
            applied.sample_rate_hz = sample_rate_hz;
            applied.node_name = node_name;
            applied.stream_format = stream_format;
        });
    }

    pub fn set_input_error(&self, error: Option<String>) {
        self.update_applied(|applied| applied.input_error = error);
    }
}
