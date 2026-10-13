//! The renderer's audio input, as a host's controls and its clients see it
//! ([`control`]), and the PipeWire formats and parameters the sink input is
//! published with ([`pipewire_pods`]).

pub mod control;
#[cfg(target_os = "linux")]
pub mod pipewire_pods;

pub use control::{
    AppliedAudioInputState, CARRIER_MAX_CHANNELS, CARRIER_MIN_CHANNELS, InputBackend, InputControl,
    InputMode,
};
