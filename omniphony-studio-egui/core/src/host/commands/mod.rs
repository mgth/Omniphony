//! The Tauri host's command handlers (`src-tauri/src/commands/*.rs`), ported
//! mechanically: `#[tauri::command]` gone, `State<SharedState>` became
//! `&SharedState`, the `AppHandle` path lookups became [`HostPaths`]. The
//! bodies are otherwise the host's, so both hosts send the same OSC. Keep in
//! sync with `src-tauri` until the two share a crate.
#![allow(dead_code)]

pub mod app;
pub mod audio;
pub mod binaural;
pub mod diag;
pub mod engine;
pub mod gain;
pub mod input;
pub mod layout_io;
pub mod mpv_config;
pub mod mpv_overlay;
pub mod orender;
pub mod profiles;
pub mod render;
pub mod resampling;
pub mod sofa;
pub mod speakers;

use std::path::PathBuf;
use std::sync::atomic::AtomicI32;
use std::sync::{Arc, Mutex};

use rosc::OscType;

use crate::osc::{Control, ControlTx, SharedLive};

/// The host's `OscControlMsg`, kept so the command bodies read unchanged.
pub enum OscControlMsg {
    SendFloat {
        address: String,
        value: f32,
    },
    SendInt {
        address: String,
        value: i32,
    },
    SendNoArgs {
        address: String,
    },
    SendString {
        address: String,
        value: String,
    },
    SendFloats3 {
        address: String,
        a: f32,
        b: f32,
        c: f32,
    },
    SendArgs {
        address: String,
        args: Vec<OscType>,
    },
    Reconnect {
        host: String,
        rx_port: u16,
        listen_port: u16,
    },
    SetMeteringEnabled {
        enabled: bool,
    },
}

/// Paths the Tauri host resolved through `AppHandle::path()`.
#[derive(Clone, Debug, Default)]
pub struct HostPaths {
    /// Bundled resources (layouts, orender binary, engine library).
    pub resource_dir: Option<PathBuf>,
    /// Where log dumps go.
    pub log_dir: Option<PathBuf>,
    /// The user's downloads directory (memory CSV dumps).
    pub download_dir: Option<PathBuf>,
}

impl HostPaths {
    pub fn resource_dir(&self) -> Result<PathBuf, String> {
        self.resource_dir
            .clone()
            .ok_or_else(|| "no resource directory".to_owned())
    }
}

/// The host's `SharedState`, minus the auto-tune runner and the renderer
/// watchdog (later phases).
pub struct SharedState {
    pub inner: SharedLive,
    pub osc_tx: ControlTx,
    pub config_dir: PathBuf,
    pub listen_port: Arc<Mutex<u16>>,
    pub realtime_seq: AtomicI32,
    pub renderer_child: Arc<Mutex<Option<std::process::Child>>>,
    pub watchdog: Arc<Mutex<WatchdogControl>>,
    pub auto_tune_snapshot: Arc<Mutex<Option<serde_json::Value>>>,
    pub paths: HostPaths,
    /// What the listener has seen, for the services that judge the link.
    pub stats: Arc<crate::osc::OscStats>,
}

/// State of the local-renderer auto-start watchdog (host `main.rs`).
#[derive(Default)]
pub struct WatchdogControl {
    pub attempts: u8,
    pub cooldown_until: Option<std::time::Instant>,
    pub last_spawn_at: Option<std::time::Instant>,
    pub check_requested_at: Option<std::time::Instant>,
    pub suppressed: bool,
}

impl WatchdogControl {
    pub fn rearm(&mut self) {
        self.attempts = 0;
        self.cooldown_until = None;
        self.suppressed = false;
    }
}

pub fn send_control(tx: &ControlTx, msg: OscControlMsg) {
    let control = match msg {
        OscControlMsg::SendFloat { address, value } => Control::Send {
            address,
            args: vec![OscType::Float(value)],
        },
        OscControlMsg::SendInt { address, value } => Control::Send {
            address,
            args: vec![OscType::Int(value)],
        },
        OscControlMsg::SendNoArgs { address } => Control::Send {
            address,
            args: Vec::new(),
        },
        OscControlMsg::SendString { address, value } => Control::Send {
            address,
            args: vec![OscType::String(value)],
        },
        OscControlMsg::SendFloats3 { address, a, b, c } => Control::Send {
            address,
            args: vec![OscType::Float(a), OscType::Float(b), OscType::Float(c)],
        },
        OscControlMsg::SendArgs { address, args } => Control::Send { address, args },
        OscControlMsg::Reconnect { host, rx_port, .. } => {
            match format!("{host}:{rx_port}").parse() {
                Ok(target) => Control::Reconnect { target },
                Err(e) => {
                    log::warn!("[control] reconnect target {host}:{rx_port}: {e}");
                    return;
                }
            }
        }
        OscControlMsg::SetMeteringEnabled { enabled } => Control::SetMetering { enabled },
    };
    let _ = tx.send(control);
}

pub fn send_json_control(tx: &ControlTx, address: &str, payload: serde_json::Value) {
    let Ok(value) = serde_json::to_string(&payload) else {
        return;
    };
    send_control(
        tx,
        OscControlMsg::SendString {
            address: address.to_string(),
            value,
        },
    );
}

pub fn send_distance_metric(state: &SharedState, address: &str, value: String) {
    let normalized = value.trim().to_ascii_lowercase();
    if !matches!(normalized.as_str(), "spherical" | "chebyshev") {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: address.to_string(),
            value: normalized,
        },
    );
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A host state a test can call commands on: the model is real, and what
    /// the commands send goes into a channel nothing reads.
    pub(crate) fn state() -> SharedState {
        let (tx, rx) = std::sync::mpsc::channel();
        // Kept alive, so a send does not fail and change what is under test.
        std::mem::forget(rx);
        SharedState {
            inner: Arc::new(Mutex::new(crate::osc::dispatch::Live::new(
                crate::model::app_state::AppState::new(Vec::new()),
            ))),
            osc_tx: tx,
            config_dir: std::path::PathBuf::from("/nonexistent"),
            listen_port: Arc::new(Mutex::new(0)),
            realtime_seq: AtomicI32::new(0),
            renderer_child: Default::default(),
            watchdog: Default::default(),
            auto_tune_snapshot: Default::default(),
            paths: HostPaths::default(),
            stats: crate::osc::OscStats::new(),
        }
    }
}
