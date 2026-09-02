use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering},
};
use sys::diag::DiagRegistry;

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum InputMode {
    #[serde(rename = "pipe_bridge", alias = "bridge")]
    Bridge,
    /// The PipeWire sink. `pipewire` and `live` named the removed PCM-only
    /// sink; they resolve here because this sink negotiates linear PCM too,
    /// so a configuration saved for that mode keeps working.
    #[serde(rename = "pipewire_bridge", alias = "pipewire", alias = "live")]
    PipewireBridge,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InputBackend {
    Pipewire,
    Asio,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum InputMapMode {
    #[serde(rename = "7.1-fixed", alias = "seven-one-fixed")]
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
pub enum InputClockMode {
    Dac,
    Pipewire,
    Upstream,
}

#[derive(Debug, Clone)]
pub struct RequestedAudioInputConfig {
    pub mode: InputMode,
    pub backend: Option<InputBackend>,
    pub node_name: Option<String>,
    pub node_description: Option<String>,
    pub layout_path: Option<PathBuf>,
    pub current_layout: Option<renderer::speaker_layout::SpeakerLayout>,
    pub clock_mode: InputClockMode,
    pub channels: Option<u16>,
    pub sample_rate_hz: Option<u32>,
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
            current_layout: None,
            clock_mode: InputClockMode::Dac,
            channels: None,
            sample_rate_hz: None,
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
    pub node_description: Option<String>,
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
            node_description: None,
            stream_format: None,
            input_error: None,
        }
    }
}

pub struct InputControl {
    requested: Mutex<RequestedAudioInputConfig>,
    applied: Mutex<AppliedAudioInputState>,
    apply_pending: AtomicBool,
    state_generation: AtomicU64,
    output_rate_adjust: Arc<AtomicU32>,
    pending_input_triggers: Mutex<Option<Arc<AtomicI64>>>,
    input_trigger_rate_hz: AtomicU32,
    input_trigger_quantum_frames: AtomicU32,
    direct_trigger_active: Arc<AtomicBool>,
    /// Generic diagnostic-metric registry. Any producer can register a metric
    /// with a name/label/group/unit; the OSC publisher emits the full schema
    /// (when changed) and the current values (each meter-bundle tick) so the
    /// Studio plot can render an arbitrary subset chosen by the user.
    diag_registry: Arc<DiagRegistry>,
    /// Cumulative source-clock time in microseconds, f64-encoded as u64 bits.
    /// Written by the input PwStream callback from per-IEC958-chunk subframe
    /// counts. Read by the audio_output PI when `use_pre_bridge_clock` is on
    /// so the servo sees a decoder-batching-free clock reference instead of
    /// the post-decode ring buffer level. Also published to the diag plot.
    input_clock_us: Arc<AtomicU64>,
    /// Cross-crate handle to the audio_output post-rendering pacer. The
    /// PipeWire input thread uses this to drain rendered samples from the
    /// pacer FIFO into the ring buffer in lockstep with IEC958 chunk
    /// arrival, smoothing the decoder's burst pattern out of the ring.
    /// Installed by the writer lifecycle once both sides exist.
    output_pacer: Mutex<Option<audio_output::PacerHandle>>,
    /// Total latency of everything BEHIND the bridge sink's input, in
    /// nanoseconds: render DSP latency (e.g. the linear-phase FIR crossover)
    /// plus the measured output-chain latency (post-render ring + pacer FIFO
    /// + graph delay to the DAC — the graph delay already folds in the
    /// terminal sink's own declared latency, per PipeWire's latency
    /// accounting). Written by the render/write path each rendered frame;
    /// read by the client-node backend, which republishes the sink's
    /// Latency/ProcessLatency params when it moves so upstream players
    /// (pipewire-pulse → Firefox, …) delay video accordingly. 0 until the
    /// first rendered frame.
    downstream_latency_ns: AtomicU64,
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
            input_trigger_quantum_frames: AtomicU32::new(0),
            direct_trigger_active: Arc::new(AtomicBool::new(false)),
            diag_registry: {
                let r = Arc::new(DiagRegistry::new());
                // Sentinel metric: present as soon as an InputControl exists,
                // so the Studio diag plot can always confirm the schema/values
                // OSC chain is alive. If the chips row is non-empty but the
                // IEC958/bridge metrics are missing, the bridge register sites
                // are not being reached.
                let alive = r.register("_diag_alive", "diag alive (sentinel)", "_diag", "");
                alive.store((1.0_f64).to_bits(), Ordering::Relaxed);
                r
            },
            input_clock_us: Arc::new(AtomicU64::new(0)),
            output_pacer: Mutex::new(None),
            downstream_latency_ns: AtomicU64::new(0),
        }
    }

    /// Publish the current downstream latency (see the field doc). Called by
    /// the render/write path each rendered frame; a plain atomic store.
    pub fn set_downstream_latency_ns(&self, ns: u64) {
        self.downstream_latency_ns.store(ns, Ordering::Relaxed);
    }

    /// The last published downstream latency in nanoseconds (0 before the
    /// first rendered frame).
    pub fn downstream_latency_ns(&self) -> u64 {
        self.downstream_latency_ns.load(Ordering::Relaxed)
    }

    pub fn diag_registry(&self) -> Arc<DiagRegistry> {
        Arc::clone(&self.diag_registry)
    }

    /// Cumulative source-clock time atomic (f64-bits microseconds). Shared
    /// owner-side: written by the input PwStream callback, read by the
    /// audio_output PI loop when running on the pre-bridge clock signal.
    pub fn input_clock_us_atomic(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.input_clock_us)
    }

    /// Install a cross-crate handle to the audio_output post-rendering
    /// pacer. Called from the decode lifecycle once the PipewireWriter is
    /// created. The PipeWire input thread reads the handle via
    /// [`output_pacer`] on each chunk and drains the FIFO into the ring.
    pub fn install_output_pacer(&self, handle: audio_output::PacerHandle) {
        if let Ok(mut guard) = self.output_pacer.lock() {
            *guard = Some(handle);
        }
    }

    /// Clone the currently-installed pacer handle, if any.
    pub fn output_pacer(&self) -> Option<audio_output::PacerHandle> {
        self.output_pacer
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
    }

    pub fn set_output_rate_adjust(&self, rate: f32) {
        self.output_rate_adjust
            .store(rate.to_bits(), Ordering::Relaxed);
    }

    pub fn output_rate_adjust_atomic(&self) -> Arc<AtomicU32> {
        Arc::clone(&self.output_rate_adjust)
    }

    pub fn register_direct_trigger_target(&self, capture_rate_hz: u32) {
        self.input_trigger_rate_hz
            .store(capture_rate_hz, Ordering::Relaxed);
    }

    pub fn input_trigger_rate_hz(&self) -> u32 {
        self.input_trigger_rate_hz.load(Ordering::Relaxed)
    }

    pub fn register_direct_trigger_quantum_frames(&self, quantum_frames: u32) {
        self.input_trigger_quantum_frames
            .store(quantum_frames, Ordering::Relaxed);
    }

    pub fn input_trigger_quantum_frames(&self) -> u32 {
        self.input_trigger_quantum_frames.load(Ordering::Relaxed)
    }

    pub fn set_pending_input_triggers(&self, counter: Arc<AtomicI64>) {
        *self.pending_input_triggers.lock().unwrap() = Some(counter);
    }

    pub fn pending_input_triggers(&self) -> Option<Arc<AtomicI64>> {
        self.pending_input_triggers.lock().unwrap().clone()
    }

    pub fn clear_pending_input_triggers(&self) {
        *self.pending_input_triggers.lock().unwrap() = None;
    }

    pub fn direct_trigger_active_arc(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.direct_trigger_active)
    }

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
        let taken = self
            .apply_pending
            .compare_exchange(true, false, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok();
        if taken {
            self.bump_state_generation();
        }
        taken
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

    pub fn set_requested_layout_path(&self, value: Option<PathBuf>) {
        self.update_requested(|requested| requested.layout_path = value);
    }

    pub fn set_requested_current_layout(
        &self,
        value: Option<renderer::speaker_layout::SpeakerLayout>,
    ) {
        self.update_requested(|requested| requested.current_layout = value);
    }

    pub fn set_requested_clock_mode(&self, value: InputClockMode) {
        self.update_requested(|requested| requested.clock_mode = value);
    }

    pub fn set_requested_channels(&self, value: Option<u16>) {
        self.update_requested(|requested| requested.channels = value);
    }

    pub fn set_requested_sample_rate_hz(&self, value: Option<u32>) {
        self.update_requested(|requested| requested.sample_rate_hz = value);
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
        node_description: Option<String>,
        stream_format: Option<String>,
    ) {
        self.update_applied(|applied| {
            applied.active_mode = mode;
            applied.backend = backend;
            applied.channels = channels;
            applied.sample_rate_hz = sample_rate_hz;
            applied.node_name = node_name;
            applied.node_description = node_description;
            applied.stream_format = stream_format;
        });
    }

    pub fn set_input_error(&self, error: Option<String>) {
        self.update_applied(|applied| applied.input_error = error);
    }
}
