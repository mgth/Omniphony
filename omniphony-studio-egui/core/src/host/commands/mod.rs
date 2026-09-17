//! The Tauri host's command handlers (`src-tauri/src/commands/*.rs`), ported
//! mechanically: `#[tauri::command]` gone, `State<SharedState>` became
//! `&SharedState`, the `AppHandle` path lookups became [`HostPaths`]. The
//! bodies are otherwise the host's, so both hosts send the same OSC. Keep in
//! sync with `src-tauri` until the two share a crate.
#![allow(dead_code)]

pub mod adaptive;
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
        request: u64,
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
    /// The paths of a shipped Studio: the resources found next to the
    /// executable (see [`crate::host::bundle`]), nothing else resolved. A
    /// checkout build gets no resource directory and finds its renderer
    /// through the checkout instead.
    pub fn bundled() -> Self {
        Self {
            resource_dir: crate::host::bundle::resource_dir(),
            ..Self::default()
        }
    }

    pub fn resource_dir(&self) -> Result<PathBuf, String> {
        self.resource_dir
            .clone()
            .ok_or_else(|| "no resource directory".to_owned())
    }
}

/// Everything the host owns: the model, the way out to the renderer, and the
/// handful of things that outlive a single call.
///
/// Every field is `pub(crate)`. The UI holds one of these and calls functions
/// on it; what it must not have is the ability to write the model behind the
/// commands' back, or to send whatever it likes down `osc_tx`. That used to be
/// a rule in `tests/architecture.rs`; making the fields unreachable is the
/// compiler saying the same thing.
pub struct SharedState {
    pub(crate) jobs: crate::host::services::jobs::Jobs,
    pub(crate) inner: SharedLive,
    pub(crate) osc_tx: ControlTx,
    /// Where this host keeps its configuration. A path, so reading it is
    /// harmless; the UI names it when asking the core to load a file.
    pub config_dir: PathBuf,
    pub(crate) listen_port: Arc<Mutex<u16>>,
    pub(crate) realtime_seq: AtomicI32,
    pub(crate) connection_request: Mutex<u64>,
    pub(crate) renderer_child: Arc<Mutex<Option<std::process::Child>>>,
    pub(crate) watchdog: Arc<Mutex<WatchdogControl>>,
    pub(crate) auto_tune_snapshot: Arc<Mutex<Option<serde_json::Value>>>,
    pub(crate) paths: HostPaths,
    /// What the listener has seen, for the services that judge the link.
    pub(crate) stats: Arc<crate::osc::OscStats>,
    /// How core work says it has something to show.
    pub(crate) waker: crate::osc::Waker,
}

/// The model, readable and nothing more.
///
/// A `MutexGuard<Live>` derefs mutably, so whoever holds one can write the
/// model — which is the whole of what the old `model-write` rule watched for.
/// This hands out the same lock without that half, and there is no way from
/// here to the other one.
pub struct ModelRead<'a>(std::sync::MutexGuard<'a, crate::osc::dispatch::Live>);

impl std::ops::Deref for ModelRead<'_> {
    type Target = crate::osc::dispatch::Live;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl SharedState {
    /// Build the host's state. The composition root calls this; the mutable
    /// odds and ends start empty.
    pub fn new(
        inner: SharedLive,
        osc_tx: ControlTx,
        config_dir: PathBuf,
        listen_port: u16,
        stats: Arc<crate::osc::OscStats>,
        waker: crate::osc::Waker,
    ) -> Self {
        Self {
            jobs: Default::default(),
            inner,
            osc_tx,
            config_dir,
            listen_port: Arc::new(Mutex::new(listen_port)),
            realtime_seq: AtomicI32::new(0),
            connection_request: Mutex::new(0),
            renderer_child: Default::default(),
            watchdog: Default::default(),
            auto_tune_snapshot: Default::default(),
            paths: HostPaths::bundled(),
            stats,
            waker,
        }
    }

    /// Refuse new jobs and finish every accepted operation before tearing down the host.
    pub fn shutdown_jobs(&self) {
        self.jobs.shutdown();
    }

    /// Read the model. Writing it is a command's job.
    pub fn read(&self) -> ModelRead<'_> {
        ModelRead(self.inner.lock().unwrap())
    }
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
        OscControlMsg::Reconnect {
            host,
            rx_port,
            request,
            ..
        } => {
            match host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .map(|ip| std::net::SocketAddr::new(ip, rx_port))
            {
                Ok(target) => Control::Reconnect { target, request },
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

/// Returns what went out, so the caller can show it: the two distance metrics
/// are the same grammar under two addresses.
pub fn send_distance_metric(state: &SharedState, address: &str, value: String) -> Option<String> {
    let normalized = value.trim().to_ascii_lowercase();
    if !matches!(normalized.as_str(), "spherical" | "chebyshev") {
        return None;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: address.to_string(),
            value: normalized.clone(),
        },
    );
    Some(normalized)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A host state a test can call commands on: the model is real, and what
    /// the commands send goes into a channel nothing reads.
    pub(crate) fn state() -> SharedState {
        state_with_waker(Arc::new(|| {}))
    }

    /// The same, with a waker a test can watch: what a command announces to the
    /// clock is part of its contract, since the clock is asleep otherwise.
    pub(crate) fn state_with_waker(waker: crate::osc::Waker) -> SharedState {
        let (tx, rx) = std::sync::mpsc::channel();
        // Kept alive, so a send does not fail and change what is under test.
        std::mem::forget(rx);
        SharedState {
            jobs: Default::default(),
            inner: Arc::new(Mutex::new(crate::osc::dispatch::Live::new(
                crate::model::app_state::AppState::new(Vec::new()),
            ))),
            osc_tx: tx,
            config_dir: std::path::PathBuf::from("/nonexistent"),
            listen_port: Arc::new(Mutex::new(0)),
            realtime_seq: AtomicI32::new(0),
            connection_request: Mutex::new(0),
            renderer_child: Default::default(),
            watchdog: Default::default(),
            auto_tune_snapshot: Default::default(),
            paths: HostPaths::default(),
            stats: crate::osc::OscStats::new(),
            waker,
        }
    }
}
