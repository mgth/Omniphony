//! The input as a host's controls and its clients share it: what is asked
//! for (staged until applied), and what runs.

use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

/// The channels a carrier sink's linear PCM can have
/// (`docs/iamf-transport-plan.md`): a burst is found by the three words that
/// open it, which a frame of four channels holds, and PipeWire names 64
/// unpositioned (`AUX`) channels.
pub const CARRIER_MIN_CHANNELS: u16 = 4;
pub const CARRIER_MAX_CHANNELS: u16 = 64;

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum InputMode {
    #[serde(rename = "pipe_bridge", alias = "bridge")]
    Bridge,
    /// The PipeWire sink. `pipewire_bridge` was its name while a PCM-only
    /// sink held `pipewire` / `live`; that sink is gone and this one
    /// negotiates linear PCM too, so all three spellings resolve here and a
    /// configuration saved under any of them keeps working.
    #[serde(rename = "pipewire", alias = "pipewire_bridge", alias = "live")]
    Pipewire,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InputBackend {
    /// The only live-input backend.
    Pipewire,
}

/// The input as it is asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestedAudioInputConfig {
    pub mode: InputMode,
    /// The sink's node name and description; `None` is the default one.
    pub node_name: Option<String>,
    pub node_description: Option<String>,
    /// The sink is a carrier sink of this many channels; `None` is the sink
    /// that takes audio.
    pub carrier_channels: Option<u16>,
}

impl Default for RequestedAudioInputConfig {
    fn default() -> Self {
        Self {
            mode: InputMode::Bridge,
            node_name: None,
            node_description: None,
            carrier_channels: None,
        }
    }
}

/// The input that runs.
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
        }
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

    pub fn set_requested_node_name(&self, value: Option<String>) {
        self.update_requested(|requested| requested.node_name = value);
    }

    pub fn set_requested_node_description(&self, value: Option<String>) {
        self.update_requested(|requested| requested.node_description = value);
    }

    pub fn set_requested_carrier_channels(&self, value: Option<u16>) {
        self.update_requested(|requested| requested.carrier_channels = value);
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
