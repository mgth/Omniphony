//! App-level commands: the full UI state snapshot, OSC connection config
//! (load/save + metering toggle), the About box, and the auto-tune snapshot
//! store.
//!
//! These touch the shared [`AppState`], the on-disk OSC config and the
//! auto-tune snapshot mutex rather than only forwarding over OSC.
//!
//! [`AppState`]: crate::model::app_state::AppState

use super::OscControlMsg;
use super::{SharedState, send_control};
use crate::host::config::{OscConfig, load_config, save_config};

/// Return type for [`get_about_info`]. Public so the UI crate can name it; its
/// fields stay private, and the About box reads it serialised, as the web did.
#[derive(serde::Serialize)]
pub struct AboutInfo {
    name: &'static str,
    version: &'static str,
    license: &'static str,
    repository_url: &'static str,
    description: &'static str,
}

/// The metering switch: the model, the listener's registration and the config
/// file, which the next launch reads. The log line is the caller's.
pub fn set_metering_enabled(state: &SharedState, enabled: bool) {
    state.inner.lock().unwrap().app.osc_metering_enabled = Some(u8::from(enabled));
    send_control(&state.osc_tx, OscControlMsg::SetMeteringEnabled { enabled });
    let mut config = load_config(&state.config_dir);
    config.osc_metering_enabled = enabled;
    if let Err(e) = save_config(&state.config_dir, &config) {
        eprintln!("[osc] could not save the configuration: {e}");
    }
}

/// The meter publish rate, applied and sent.
pub fn set_meter_rate_hz(state: &SharedState, hz: f32) {
    state.inner.lock().unwrap().app.meter_rate_hz = Some(hz);
    super::diag::control_metering_rate_hz(state, hz);
}

/// Passive/synthetic startup must not launch a real renderer implicitly.
/// An explicit launch action can re-arm the watchdog later.
pub fn suppress_autostart(state: &SharedState) {
    state.watchdog.lock().unwrap().suppressed = true;
}

/// Lift the watchdog's suppression, which installing the service sets: turning
/// auto-start back on is the user saying the watchdog may try again.
pub fn resume_watchdog(state: &SharedState) {
    state.watchdog.lock().unwrap().suppressed = false;
}

/// Finish operations on the current destination before enqueuing a target
/// change. The transport drains this channel in order, so STOP/restoration
/// are sent to the old renderer even when the next action reconnects elsewhere.
fn finish_session_operations(state: &SharedState) {
    crate::host::services::speaker_test::stop(state);
    crate::host::services::auto_tune::revert(state);
}

/// Reserve an intent before scheduling DNS, so a delayed startup request
/// cannot replace a later manual connection.
pub fn begin_connection(state: &SharedState) -> u64 {
    let mut request = state.connection_request.lock().unwrap();
    *request = request.wrapping_add(1);
    *request
}

/// Point the client at a renderer: resolve the address (a hostname needs a
/// lookup, done here so the failure can be shown), reconnect, and say so in
/// the log. Returns the error message when nothing resolves.
pub fn connect_to(
    state: &SharedState,
    host: &str,
    port: u16,
) -> Result<std::net::SocketAddr, String> {
    let request = begin_connection(state);
    connect_requested(state, host, port, request)
}

pub fn connect_requested(
    state: &SharedState,
    host: &str,
    port: u16,
    request: u64,
) -> Result<std::net::SocketAddr, String> {
    connect_resolved(state, host, port, request, crate::osc::resolve)
}

fn connect_resolved(
    state: &SharedState,
    host: &str,
    port: u16,
    request: u64,
    resolve: impl FnOnce(&str) -> Option<std::net::SocketAddr>,
) -> Result<std::net::SocketAddr, String> {
    let host = host.trim().trim_matches(['[', ']']);
    let target = match host.parse::<std::net::IpAddr>() {
        Ok(ip) => std::net::SocketAddr::new(ip, port).to_string(),
        Err(_) => format!("{host}:{port}"),
    };
    let Some(addr) = resolve(&target).filter(std::net::SocketAddr::is_ipv4) else {
        let message = format!("cannot resolve {target}");
        state
            .inner
            .lock()
            .unwrap()
            .push_log("error", "osc", &message);
        return Err(message);
    };
    let current = state.connection_request.lock().unwrap();
    if *current != request {
        return Err("connection request superseded".into());
    }
    finish_session_operations(state);
    send_control(
        &state.osc_tx,
        OscControlMsg::Reconnect {
            host: addr.ip().to_string(),
            rx_port: addr.port(),
            listen_port: *state.listen_port.lock().unwrap(),
        },
    );
    state
        .inner
        .lock()
        .unwrap()
        .push_log("info", "osc", format!("connecting to {addr}"));
    Ok(addr)
}

/// One line into the log the overlay shows. The view says what happened; the
/// model keeps it.
pub fn push_log(state: &SharedState, level: &str, target: &str, message: String) {
    state.inner.lock().unwrap().push_log(level, target, message);
}

/// Empty the log ring the overlay shows.
pub fn clear_log(state: &SharedState) {
    state.inner.lock().unwrap().log.clear();
}

/// Take the backend file the renderer sent, if it is the one being waited for.
///
/// Taking rather than reading: the editor owns the content once it has it, and
/// leaving the slot full would hand the same file to the next request that
/// happened to name the same backend and key.
pub fn take_backend_file(
    state: &SharedState,
    backend: &str,
    key: &str,
) -> Option<crate::osc::dispatch::BackendFile> {
    let mut live = state.inner.lock().unwrap();
    match &live.backend_file_content {
        Some(f) if f.backend == backend && f.key == key => live.backend_file_content.take(),
        _ => None,
    }
}

/// Consume a failure only for the editor that requested this parameter.
pub fn take_backend_file_error(
    state: &SharedState,
    backend: &str,
    key: &str,
) -> Option<crate::osc::dispatch::BackendFileFailure> {
    let mut live = state.inner.lock().unwrap();
    match &live.backend_file_error {
        Some(error) if error.backend == backend && error.key == key => {
            live.backend_file_error.take().map(|error| error.failure)
        }
        _ => None,
    }
}

pub fn get_state(state: &SharedState) -> serde_json::Value {
    let s = state.inner.lock().unwrap();
    serde_json::to_value(&s.app).unwrap_or(serde_json::Value::Null)
}

pub fn get_osc_config(state: &SharedState) -> OscConfig {
    load_config(&state.config_dir)
}

/// Hostname loopback test used by the auto-start watchdog.
pub fn host_is_local(host: &str) -> bool {
    let host = host.trim().trim_matches(['[', ']']);
    host.is_empty()
        || host.eq_ignore_ascii_case("localhost")
        || host == "::1"
        || host.starts_with("127.")
}

/// Whether the active transport target is loopback, so the renderer shares this
/// machine's filesystem. The native Browse dialog returns a path in this machine's
/// namespace, so the UI only offers it for editable file params when this is true;
/// editing a remote renderer's files still works through the OSC content channel.
pub fn renderer_is_local(state: &SharedState) -> bool {
    state
        .stats
        .target
        .lock()
        .unwrap()
        .is_some_and(|target| target.ip().is_loopback())
}

pub fn get_about_info() -> AboutInfo {
    AboutInfo {
        name: "Omniphony Studio",
        version: env!("CARGO_PKG_VERSION"),
        license: "GPL-3.0-or-later",
        repository_url: "https://github.com/mgth/Omniphony",
        description: "Omniphony is an open spatial-audio project built around realtime rendering, transport, control, and monitoring tools for object-based audio workflows. Omniphony Studio is the visual control surface of that ecosystem.",
    }
}

pub fn save_osc_config(state: &SharedState, mut config: OscConfig) -> Result<(), String> {
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
    connect_to(state, &config.host, config.osc_rx_port)?;
    Ok(())
}

pub fn control_osc_metering(state: &SharedState, enable: i32) -> Result<(), String> {
    let enabled = enable != 0;
    let mut cfg = load_config(&state.config_dir);
    cfg.osc_metering_enabled = enabled;
    save_config(&state.config_dir, &cfg)?;
    state.inner.lock().unwrap().osc_metering_enabled = Some(if enabled { 1 } else { 0 });
    send_control(&state.osc_tx, OscControlMsg::SetMeteringEnabled { enabled });
    Ok(())
}

pub fn auto_tune_snapshot_save(state: &SharedState, snapshot: serde_json::Value) {
    *state.auto_tune_snapshot.lock().unwrap() = Some(snapshot);
}

pub fn auto_tune_snapshot_take(state: &SharedState) -> Option<serde_json::Value> {
    state.auto_tune_snapshot.lock().unwrap().take()
}

pub fn auto_tune_snapshot_peek(state: &SharedState) -> Option<serde_json::Value> {
    state.auto_tune_snapshot.lock().unwrap().clone()
}

#[cfg(test)]
mod reconnect_tests {
    use super::*;

    #[test]
    fn local_file_access_follows_transport_instead_of_persisted_preferences() {
        let mut state = crate::host::commands::tests::state();
        let directory = tempfile::tempdir().unwrap();
        state.config_dir = directory.path().into();
        for saved_host in ["127.0.0.1", "192.0.2.10"] {
            save_config(
                &state.config_dir,
                &OscConfig {
                    host: saved_host.into(),
                    ..Default::default()
                },
            )
            .unwrap();
            for (target, local) in [
                (None, false),
                (Some("127.0.0.1:9000"), true),
                (Some("192.0.2.10:9000"), false),
                (Some("127.0.0.2:9010"), true),
            ] {
                *state.stats.target.lock().unwrap() = target.map(|value| value.parse().unwrap());
                assert_eq!(renderer_is_local(&state), local);
            }
        }
    }

    #[test]
    fn slow_initial_resolution_cannot_replace_a_newer_manual_target() {
        let mut state = crate::host::commands::tests::state();
        let (tx, commands) = std::sync::mpsc::channel();
        state.osc_tx = tx;
        let state = std::sync::Arc::new(state);
        let request = begin_connection(&state);
        let (release, blocked) = std::sync::mpsc::channel();
        let initial = state.clone();
        let worker = std::thread::spawn(move || {
            connect_resolved(&initial, "slow.example", 9000, request, |_| {
                blocked.recv().unwrap();
                Some("127.0.0.1:9000".parse().unwrap())
            })
        });
        connect_to(&state, "127.0.0.2", 9010).unwrap();
        release.send(()).unwrap();
        assert!(worker.join().unwrap().is_err());
        let queued: Vec<_> = commands.try_iter().collect();
        assert_eq!(queued.len(), 1);
        assert!(
            matches!(&queued[0], crate::osc::Control::Reconnect { target } if *target == "127.0.0.2:9010".parse().unwrap())
        );
    }

    #[test]
    fn reconnect_stops_an_active_burst_before_switching_even_to_same_target() {
        for destination in ["127.0.0.1", "127.0.0.2"] {
            let mut state = crate::host::commands::tests::state();
            let (tx, rx) = std::sync::mpsc::channel();
            state.osc_tx = tx;
            *state.stats.target.lock().unwrap() = Some("127.0.0.1:9000".parse().unwrap());
            crate::host::services::speaker_test::start(
                &state,
                2,
                -10.0,
                "test_only".into(),
                "burst",
            );
            rx.try_iter().for_each(drop);
            connect_to(&state, destination, 9000).unwrap();
            let commands: Vec<_> = rx.try_iter().collect();
            assert!(
                matches!(&commands[0], crate::osc::Control::Send { args, .. } if matches!(args.first(), Some(rosc::OscType::Int(-1))))
            );
            assert!(matches!(
                &commands[1],
                crate::osc::Control::Reconnect { .. }
            ));
            assert!(state.read().speaker_test.running.is_none());
        }
    }
}
