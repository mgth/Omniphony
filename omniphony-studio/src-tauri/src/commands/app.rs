//! App-level commands: the full UI state snapshot, OSC connection config
//! (load/save + metering toggle), the About box, and the auto-tune snapshot
//! store.
//!
//! These touch the shared [`AppState`], the on-disk OSC config and the
//! auto-tune snapshot mutex rather than only forwarding over OSC.
//!
//! [`AppState`]: crate::app_state::AppState

use crate::config::{load_config, save_config, OscConfig};
use crate::osc_listener::OscControlMsg;
use crate::{send_control, SharedState};
use tauri::State;

/// Return type for [`get_about_info`]. Kept `pub(crate)` so the
/// `generate_handler!` expansion in `main.rs` can name the command's output.
#[derive(serde::Serialize)]
pub(crate) struct AboutInfo {
    name: &'static str,
    version: &'static str,
    license: &'static str,
    repository_url: &'static str,
    description: &'static str,
}

#[tauri::command]
pub fn get_state(state: State<SharedState>) -> serde_json::Value {
    let s = state.inner.lock().unwrap();
    serde_json::to_value(&*s).unwrap_or(serde_json::Value::Null)
}

#[tauri::command]
pub fn get_osc_config(state: State<SharedState>) -> OscConfig {
    load_config(&state.config_dir)
}

/// Loopback test shared by [`renderer_is_local`] and the auto-start watchdog.
pub(crate) fn host_is_local(host: &str) -> bool {
    let host = host.trim();
    host.is_empty()
        || host.eq_ignore_ascii_case("localhost")
        || host == "::1"
        || host.starts_with("127.")
}

/// Whether the configured renderer host is loopback, so the renderer shares this
/// machine's filesystem. The native Browse dialog returns a path in this machine's
/// namespace, so the UI only offers it for editable file params when this is true;
/// editing a remote renderer's files still works through the OSC content channel.
#[tauri::command]
pub fn renderer_is_local(state: State<SharedState>) -> bool {
    host_is_local(&load_config(&state.config_dir).host)
}

#[tauri::command]
pub fn get_about_info() -> AboutInfo {
    AboutInfo {
        name: "Omniphony Studio",
        version: env!("CARGO_PKG_VERSION"),
        license: "GPL-3.0-or-later",
        repository_url: "https://github.com/mgth/Omniphony",
        description: "Omniphony is an open spatial-audio project built around realtime rendering, transport, control, and monitoring tools for object-based audio workflows. Omniphony Studio is the visual control surface of that ecosystem.",
    }
}

#[tauri::command]
pub fn save_osc_config(state: State<SharedState>, mut config: OscConfig) -> Result<(), String> {
    // The JS config form doesn't carry server-only fields; preserve them from
    // the persisted copy so a save doesn't wipe the remembered import dir.
    let persisted = load_config(&state.config_dir);
    config.last_layout_import_dir = persisted.last_layout_import_dir;
    config.rust_auto_tune = persisted.rust_auto_tune;
    save_config(&state.config_dir, &config)?;
    // A config change re-arms the auto-start watchdog after a failure streak.
    state.watchdog.lock().unwrap().rearm();
    state.inner.lock().unwrap().osc_metering_enabled =
        Some(if config.osc_metering_enabled { 1 } else { 0 });
    send_control(
        &state.osc_tx,
        OscControlMsg::SetMeteringEnabled {
            enabled: config.osc_metering_enabled,
        },
    );
    let listen_port = *state.listen_port.lock().unwrap();
    send_control(
        &state.osc_tx,
        OscControlMsg::Reconnect {
            host: config.host,
            rx_port: config.osc_rx_port,
            listen_port,
        },
    );
    Ok(())
}

#[tauri::command]
pub fn control_osc_metering(state: State<SharedState>, enable: i32) -> Result<(), String> {
    let enabled = enable != 0;
    let mut cfg = load_config(&state.config_dir);
    cfg.osc_metering_enabled = enabled;
    save_config(&state.config_dir, &cfg)?;
    state.inner.lock().unwrap().osc_metering_enabled = Some(if enabled { 1 } else { 0 });
    send_control(&state.osc_tx, OscControlMsg::SetMeteringEnabled { enabled });
    Ok(())
}

#[tauri::command]
pub fn auto_tune_snapshot_save(state: State<SharedState>, snapshot: serde_json::Value) {
    *state.auto_tune_snapshot.lock().unwrap() = Some(snapshot);
}

#[tauri::command]
pub fn auto_tune_snapshot_take(state: State<SharedState>) -> Option<serde_json::Value> {
    state.auto_tune_snapshot.lock().unwrap().take()
}

#[tauri::command]
pub fn auto_tune_snapshot_peek(state: State<SharedState>) -> Option<serde_json::Value> {
    state.auto_tune_snapshot.lock().unwrap().clone()
}
