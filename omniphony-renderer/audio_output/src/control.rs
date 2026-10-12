use parking_lot::Mutex;

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
    /// Live-requested output backend id ("pipewire" / "asio" / "file"), or
    /// `None` to keep the active one. Parsed by the CLI's runtime sync layer
    /// (this crate stays decoupled from the CLI's `OutputBackend` enum).
    pub output_backend: Option<String>,
    /// Live-requested destination for the `file` backend (`-` = stdout, or a
    /// file/FIFO path).
    pub output_file: Option<String>,
    /// Live-requested `file` backend encoding ("raw_f32" / "caf").
    pub output_file_format: Option<String>,
    /// Sync engine: bandwidth of the latency loop (Hz); 0 is the source's
    /// own default (`audio_sync::ServoConfig`).
    pub loop_bandwidth_hz: f32,
    /// Sync engine: latency error beyond which the output realigns instead
    /// of steering (ms).
    pub realign_threshold_ms: f32,
    /// Sync engine: the clock the source is taken to run on.
    pub source_clock: SourceClock,
}

/// The clock a source runs on, as asked for (`docs/resampling-rework-plan.md`
/// §2.1). The input decides what is possible: a setting it cannot have falls
/// back to its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SourceClock {
    /// The input's own: `own` for the renderer's sink, `follow` for a pipe,
    /// `none` for a file.
    #[default]
    Auto,
    /// The renderer's clock (its sink, which it drives).
    Own,
    /// The writer's clock, read from the arrival of its bytes (a player
    /// writing a pipe in real time).
    Follow,
    /// No clock (a file, a writer faster than real time): the device is the
    /// master and the source is held back.
    None,
}

impl SourceClock {
    pub const NAMES: [&'static str; 4] = ["auto", "own", "follow", "none"];

    pub fn as_str(self) -> &'static str {
        match self {
            SourceClock::Auto => "auto",
            SourceClock::Own => "own",
            SourceClock::Follow => "follow",
            SourceClock::None => "none",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(SourceClock::Auto),
            "own" => Some(SourceClock::Own),
            "follow" => Some(SourceClock::Follow),
            "none" => Some(SourceClock::None),
            _ => None,
        }
    }
}

/// [`RequestedAudioOutputConfig::realign_threshold_ms`] when nothing sets it.
pub const DEFAULT_REALIGN_THRESHOLD_MS: f32 = 20.0;

impl Default for RequestedAudioOutputConfig {
    fn default() -> Self {
        Self {
            output_device: None,
            output_sample_rate_hz: None,
            latency_target_ms: None,
            output_backend: None,
            output_file: None,
            output_file_format: None,
            loop_bandwidth_hz: 0.0,
            realign_threshold_ms: DEFAULT_REALIGN_THRESHOLD_MS,
            source_clock: SourceClock::Auto,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AppliedAudioOutputState {
    pub output_device: Option<String>,
    pub output_sample_rate_hz: Option<u32>,
    pub sample_format: String,
    pub audio_error: Option<String>,
    /// The host the open output stream plays through, as Studio shows it
    /// (`ASIO`, `WASAPI (fallback: no ASIO driver)`, `CoreAudio`); `None`
    /// when no stream is open or the backend has no host to name (PipeWire,
    /// file).
    pub output_host: Option<&'static str>,
    /// Sync engine: the latency the output holds when it had to raise the
    /// requested target (ms); `None` while it holds the target itself.
    pub latency_held_ms: Option<f64>,
}

pub struct AudioControl {
    requested: Mutex<RequestedAudioOutputConfig>,
    applied: Mutex<AppliedAudioOutputState>,
    available_output_devices: Mutex<Vec<OutputDeviceOption>>,
    device_list_fetcher: Mutex<Option<Box<dyn Fn() -> Vec<OutputDeviceOption> + Send + Sync>>>,
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

    pub fn set_requested_loop_bandwidth_hz(&self, value: f32) {
        self.update_requested(|requested| requested.loop_bandwidth_hz = value);
    }

    pub fn set_requested_source_clock(&self, value: SourceClock) {
        self.update_requested(|requested| requested.source_clock = value);
    }

    pub fn set_requested_realign_threshold_ms(&self, value: f32) {
        self.update_requested(|requested| requested.realign_threshold_ms = value);
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

    pub fn set_effective_output_host(&self, output_host: Option<&'static str>) {
        self.update_applied(|applied| applied.output_host = output_host);
    }

    pub fn effective_output_host(&self) -> Option<&'static str> {
        self.applied.lock().output_host
    }

    pub fn set_latency_held_ms(&self, held_ms: Option<f64>) {
        self.update_applied(|applied| applied.latency_held_ms = held_ms);
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
