pub mod input;
pub mod live_log;
pub mod shutdown;

#[cfg(windows)]
pub mod windows;

pub use input::InputReader;
pub use shutdown::ShutdownHandle;

/// Platform-agnostic shutdown signal for interrupt-aware I/O.
///
/// Created via [`ShutdownHandle::shutdown_signal()`]. Pass to
/// `InputReader::process_chunks_with_shutdown` so that blocking reads
/// can be interrupted immediately when a shutdown or reload is requested.
pub struct ShutdownSignal {
    /// Unix: read end of the self-pipe (for poll(2)).
    #[cfg(unix)]
    pub fd: i32,
    /// Windows: manual-reset event handle (as isize, for WaitForMultipleObjects).
    #[cfg(windows)]
    pub event: isize,
}

/// Try to start the process as a Windows service.
///
/// Returns `true` if running under the Windows SCM (the `app` callback is
/// called and blocks until the service stops). Always returns `false` on
/// non-Windows platforms.
pub fn try_start_service(_app: fn() -> anyhow::Result<()>) -> bool {
    #[cfg(windows)]
    {
        return windows::try_start_service(_app);
    }
    false
}

/// Notify the service supervisor that the service is fully initialised and
/// ready to process requests.
///
/// - Unix: sends `READY=1` to systemd and starts the watchdog thread.
/// - Windows: reports `SERVICE_RUNNING` to the SCM.
pub fn notify_ready() {
    #[cfg(unix)]
    {
        shutdown::sd_notify("READY=1\n");
        shutdown::spawn_watchdog();
    }
    #[cfg(windows)]
    windows::notify_running();
}

/// Notify the service supervisor that the service is reloading its
/// configuration (e.g. on SIGHUP). `monotonic_usec` is the current
/// `CLOCK_MONOTONIC` timestamp in microseconds (required by systemd ≥ 253).
///
/// No-op on Windows (SCM has no equivalent reload notification).
pub fn notify_reloading(monotonic_usec: u64) {
    #[cfg(unix)]
    shutdown::sd_notify(&format!("RELOADING=1\nMONOTONIC_USEC={monotonic_usec}\n"));
    let _ = monotonic_usec;
}

/// Notify the service supervisor that the service is beginning a clean
/// shutdown.
///
/// - Unix: sends `STOPPING=1` to systemd.
/// - Windows: reports `SERVICE_STOP_PENDING` to the SCM.
pub fn notify_stopping() {
    #[cfg(unix)]
    shutdown::sd_notify("STOPPING=1\n");
    #[cfg(windows)]
    windows::notify_stop_pending();
}
