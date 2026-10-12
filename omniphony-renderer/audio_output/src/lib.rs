//! The renderer's audio output: the sync stage that plays what the engine
//! renders at a held latency ([`sync_output`]), the file sink, and the state
//! a host's controls and its clients share ([`control`]).

pub mod control;
pub mod file_sink;
pub mod host_choice;
#[cfg(target_os = "linux")]
pub mod pipewire;
#[cfg(target_os = "linux")]
pub mod pipewire_registry;
pub mod sync_output;

pub use control::{
    AppliedAudioOutputState, AudioControl, OutputDeviceOption, RequestedAudioOutputConfig,
};
pub use file_sink::{CafChannelDesc, FileAudioWriter, FileSinkFormat};
