//! Windows Service Control Manager (SCM) integration.
//!
//! Allows omniphony-renderer to run as a Windows service. The full CLI command line is
//! embedded in the service's `binPath` at registration time so that the
//! existing Clap argument parsing works unchanged inside the service.
//!
//! # Service registration
//!
//! ```bat
//! sc create omniphony-renderer binPath= "C:\orender.exe --output-backend asio \\.\pipe\input.audio"
//! sc description omniphony-renderer "Spatial audio decoder"
//! sc start omniphony-renderer
//! sc stop  omniphony-renderer
//! ```
//!
//! # Stream reload (equivalent of SIGHUP on Linux)
//!
//! ```bat
//! sc control omniphony-renderer 128
//! ```
//!
//! # Service control flow
//!
//! ```
//! SCM starts process (with args from binPath)
//!   │
//!   └─► main() calls try_start_service()
//!         │
//!         └─► service_dispatcher::start() → SCM calls ffi_service_main
//!               │
//!               ├── register control handler
//!               ├── report SERVICE_START_PENDING
//!               ├── Cli::parse() + cmd_render() [blocks until stream ends]
//!               │     └─► decode_impl calls notify_running() → SERVICE_RUNNING
//!               │     └─► on stop: notify_stop_pending() → SERVICE_STOP_PENDING
//!               └── report SERVICE_STOPPED
//! ```

use std::ffi::OsString;
use std::sync::OnceLock;
use std::time::Duration;

static APP_MAIN: OnceLock<fn() -> anyhow::Result<()>> = OnceLock::new();

use windows_service::{
    define_windows_service,
    service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult, ServiceStatusHandle},
    service_dispatcher,
};

// ─── shared state ─────────────────────────────────────────────────────────────

/// SCM status handle — set once when the service control handler is registered.
/// `service_set_running` / `service_set_stopping` read from here.
static STATUS_HANDLE: OnceLock<ServiceStatusHandle> = OnceLock::new();

const SERVICE_NAME: &str = "omniphony-renderer";

// ─── SCM entry point ──────────────────────────────────────────────────────────

define_windows_service!(ffi_service_main, service_entry);

fn service_entry(_arguments: Vec<OsString>) {
    if let Err(e) = run_service() {
        log::error!("Windows service error: {e:#}");
    }
}

fn run_service() -> anyhow::Result<()> {
    // ── Register the control handler ────────────────────────────────────────
    let status_handle = service_control_handler::register(
        SERVICE_NAME,
        |control| -> ServiceControlHandlerResult {
            match control {
                // Stop / Shutdown → clean shutdown (flush audio, exit).
                ServiceControl::Stop | ServiceControl::Shutdown => {
                    crate::shutdown::request_shutdown();
                    // Report STOP_PENDING immediately so SCM does not time out
                    // waiting for the service to acknowledge the stop request.
                    if let Some(h) = STATUS_HANDLE.get() {
                        let _ = h.set_service_status(ServiceStatus {
                            service_type: ServiceType::OWN_PROCESS,
                            current_state: ServiceState::StopPending,
                            controls_accepted: ServiceControlAccept::empty(),
                            exit_code: ServiceExitCode::Win32(0),
                            checkpoint: 0,
                            wait_hint: Duration::from_secs(30),
                            process_id: None,
                        });
                    }
                    ServiceControlHandlerResult::NoError
                }
                // Custom command 128 → reload stream (equivalent of SIGHUP).
                ServiceControl::UserEvent(code) if code.to_raw() == 128 => {
                    crate::shutdown::request_reload();
                    ServiceControlHandlerResult::NoError
                }
                // Interrogate → report current state unchanged (SCM protocol).
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                _ => ServiceControlHandlerResult::NotImplemented,
            }
        },
    )?;

    // Store so notify_running / notify_stop_pending can update status later.
    STATUS_HANDLE
        .set(status_handle)
        .map_err(|_| anyhow::anyhow!("STATUS_HANDLE already set (service_entry called twice?)"))?;

    // ── Report START_PENDING ─────────────────────────────────────────────────
    // VBAP table loading can take tens of seconds; use a generous wait_hint.
    update_status(
        ServiceState::StartPending,
        ServiceControlAccept::empty(),
        ServiceExitCode::Win32(0),
        0,
        Duration::from_secs(120),
    );

    // ── Run the application (blocks until the stream ends or shutdown is requested) ──
    let result = APP_MAIN
        .get()
        .map(|f| f())
        .unwrap_or_else(|| anyhow::bail!("APP_MAIN not set before try_start_service"));

    // ── Report STOPPED ───────────────────────────────────────────────────────
    let code = if result.is_ok() { 0 } else { 1 };
    update_status(
        ServiceState::Stopped,
        ServiceControlAccept::empty(),
        ServiceExitCode::Win32(code),
        0,
        Duration::ZERO,
    );

    result
}

// ─── internal helper ─────────────────────────────────────────────────────────

fn update_status(
    state: ServiceState,
    accepted: ServiceControlAccept,
    exit_code: ServiceExitCode,
    checkpoint: u32,
    wait_hint: Duration,
) {
    if let Some(handle) = STATUS_HANDLE.get() {
        let _ = handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accepted,
            exit_code,
            checkpoint,
            wait_hint,
            process_id: None,
        });
    }
}

// ─── public API called from decode_impl ──────────────────────────────────────

/// Report SERVICE_RUNNING to SCM.
///
/// Call after all initialisation is complete (VBAP tables loaded, signal
/// handlers installed) — the Windows equivalent of `sd_notify("READY=1\n")`.
pub fn notify_running() {
    update_status(
        ServiceState::Running,
        ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        ServiceExitCode::Win32(0),
        0,
        Duration::ZERO,
    );
}

/// Report SERVICE_STOP_PENDING to SCM.
///
/// Call when a stop signal has been received and audio flush is in progress —
/// the Windows equivalent of `sd_notify("STOPPING=1\n")`.
pub fn notify_stop_pending() {
    update_status(
        ServiceState::StopPending,
        ServiceControlAccept::empty(),
        ServiceExitCode::Win32(0),
        1,
        Duration::from_secs(30),
    );
}

// ─── entry point called from main() ──────────────────────────────────────────

/// Try to enter the Windows Service Control Manager dispatch loop.
///
/// Returns `true` if the process was started by SCM (service has now run to
/// completion and `main()` should return).
/// Returns `false` if the process was started from the console — caller should
/// proceed with normal `Cli::parse()` / `cmd_render()` flow.
pub fn try_start_service(app: fn() -> anyhow::Result<()>) -> bool {
    let _ = APP_MAIN.set(app);
    match service_dispatcher::start(SERVICE_NAME, ffi_service_main) {
        Ok(()) => true,
        Err(windows_service::Error::Winapi(ref e)) if e.raw_os_error() == Some(1063) => {
            // ERROR_FAILED_SERVICE_CONTROLLER_CONNECT: not running under SCM.
            false
        }
        Err(e) => {
            // Unexpected error — log and fall through to console mode.
            eprintln!("omniphony-renderer: service_dispatcher::start error: {e}");
            false
        }
    }
}
