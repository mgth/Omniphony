//! Signal handling for graceful daemon shutdown and stream reload.
//!
//! Uses the self-pipe trick on Unix: signal handlers write a byte to a
//! non-blocking pipe. The decoder thread polls the read end alongside its
//! data fd for low-latency, race-free signal delivery.
//!
//! On Windows, `SetConsoleCtrlHandler` captures Ctrl+C, Ctrl+Break and
//! console close / shutdown events, and sets the same atomic flag.
//!
//! Signal / event semantics:
//! - **SIGTERM / SIGINT** (Unix), **Ctrl+C / close / shutdown** (Windows)
//!   → stop decoding and exit cleanly (`is_requested`)
//! - **SIGHUP** (Unix only) → interrupt current stream and restart (`is_reload_requested`)

use std::sync::atomic::{AtomicBool, Ordering};

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);
static RELOAD_REQUESTED: AtomicBool = AtomicBool::new(false);
static RESTART_FROM_CONFIG_REQUESTED: AtomicBool = AtomicBool::new(false);

// Windows: HANDLE of the manual-reset event used to wake process_chunks_with_shutdown.
// Stored as isize so it can live in an AtomicIsize (HANDLE is isize under the hood).
// 0 = not yet created.
#[cfg(windows)]
use std::sync::atomic::AtomicIsize;
#[cfg(windows)]
static SHUTDOWN_EVENT_HANDLE: AtomicIsize = AtomicIsize::new(0);

// ─── Unix: self-pipe + POSIX signal handlers ─────────────────────────────────

#[cfg(unix)]
use std::sync::atomic::AtomicI32;

#[cfg(unix)]
static SIGNAL_WRITE_FD: AtomicI32 = AtomicI32::new(-1);

/// Handler for SIGTERM / SIGINT — triggers clean shutdown.
#[cfg(unix)]
extern "C" fn shutdown_handler(_sig: libc::c_int) {
    SHUTDOWN_REQUESTED.store(true, Ordering::Relaxed);
    let fd = SIGNAL_WRITE_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        unsafe { libc::write(fd, b"\x00".as_ptr() as *const libc::c_void, 1) };
    }
}

/// Handler for SIGHUP — triggers stream reload without process exit.
#[cfg(unix)]
extern "C" fn reload_handler(_sig: libc::c_int) {
    RELOAD_REQUESTED.store(true, Ordering::Relaxed);
    let fd = SIGNAL_WRITE_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        unsafe { libc::write(fd, b"\x01".as_ptr() as *const libc::c_void, 1) };
    }
}

// ─── Windows: SetConsoleCtrlHandler ──────────────────────────────────────────

/// Ctrl event handler — Ctrl+C, Ctrl+Break, console close, system shutdown.
#[cfg(windows)]
unsafe extern "system" fn ctrl_handler(ctrl_type: u32) -> windows::Win32::Foundation::BOOL {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    };
    use windows::Win32::System::Threading::SetEvent;
    if ctrl_type == CTRL_C_EVENT
        || ctrl_type == CTRL_BREAK_EVENT
        || ctrl_type == CTRL_CLOSE_EVENT
        || ctrl_type == CTRL_SHUTDOWN_EVENT
        || ctrl_type == CTRL_LOGOFF_EVENT
    {
        SHUTDOWN_REQUESTED.store(true, Ordering::Relaxed);
        // Wake any thread blocked in WaitForMultipleObjects inside
        // process_chunks_with_shutdown.
        let h = SHUTDOWN_EVENT_HANDLE.load(Ordering::Relaxed);
        if h != 0 {
            let _ = SetEvent(HANDLE(h));
        }
        windows::Win32::Foundation::BOOL(1) // handled
    } else {
        windows::Win32::Foundation::BOOL(0) // not handled — OS takes default action
    }
}

// ─── ShutdownHandle ───────────────────────────────────────────────────────────

/// Cross-platform signal / Ctrl-event handling infrastructure.
///
/// Install once at process startup with [`ShutdownHandle::install`].
///
/// On Unix the `read_fd` field must be passed to the decoder thread so it can
/// `poll(2)` both input data and signal notifications simultaneously.
/// On Windows the decoder thread detects shutdown by polling
/// [`ShutdownHandle::is_requested`].
///
/// - SIGTERM / SIGINT (Unix), Ctrl+C / close (Windows) → [`is_requested`]
/// - SIGHUP (Unix only)                                → [`is_reload_requested`]
pub struct ShutdownHandle {
    /// Read end of the self-pipe. Poll this fd to detect a shutdown signal.
    /// Only available on Unix (used with poll(2) in the decoder thread).
    #[cfg(unix)]
    pub read_fd: i32,

    /// Windows manual-reset event signaled by ctrl_handler.
    /// Pass to process_chunks_with_shutdown so it can WaitForMultipleObjects
    /// alongside the I/O completion event.
    #[cfg(windows)]
    pub shutdown_event: isize, // HANDLE stored as isize
}

impl ShutdownHandle {
    /// Register OS signal / Ctrl-event handlers and reset the shared flags.
    pub fn install() -> anyhow::Result<Self> {
        SHUTDOWN_REQUESTED.store(false, Ordering::Relaxed);
        RELOAD_REQUESTED.store(false, Ordering::Relaxed);
        RESTART_FROM_CONFIG_REQUESTED.store(false, Ordering::Relaxed);

        #[cfg(unix)]
        {
            let mut fds = [0i32; 2];
            let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) };
            if rc != 0 {
                return Err(anyhow::anyhow!(
                    "Failed to create shutdown pipe: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let [read_fd, write_fd] = fds;

            SIGNAL_WRITE_FD.store(write_fd, Ordering::Relaxed);

            // SA_RESTART: slow syscalls (read/write) are restarted automatically.
            // poll(2) is NOT restarted on Linux and returns EINTR instead —
            // handled explicitly in the poll loop.
            let sa_shutdown = libc::sigaction {
                sa_sigaction: shutdown_handler as *const () as libc::sighandler_t,
                sa_mask: unsafe { std::mem::zeroed() },
                sa_flags: libc::SA_RESTART,
                sa_restorer: None,
            };
            let sa_reload = libc::sigaction {
                sa_sigaction: reload_handler as *const () as libc::sighandler_t,
                sa_mask: unsafe { std::mem::zeroed() },
                sa_flags: libc::SA_RESTART,
                sa_restorer: None,
            };

            let registrations: &[(libc::c_int, &libc::sigaction)] = &[
                (libc::SIGTERM, &sa_shutdown),
                (libc::SIGINT, &sa_shutdown),
                (libc::SIGHUP, &sa_reload),
            ];
            for &(sig, sa) in registrations {
                let rc = unsafe { libc::sigaction(sig, sa, std::ptr::null_mut()) };
                if rc != 0 {
                    unsafe {
                        libc::close(read_fd);
                        libc::close(write_fd);
                    }
                    return Err(anyhow::anyhow!(
                        "Failed to register signal {} handler: {}",
                        sig,
                        std::io::Error::last_os_error()
                    ));
                }
            }

            log::debug!(
                "Signal handlers registered (SIGTERM/SIGINT → shutdown, SIGHUP → reload), \
                 pipe: read_fd={read_fd} write_fd={write_fd}"
            );

            return Ok(Self { read_fd });
        }

        #[cfg(windows)]
        {
            use windows::Win32::System::Console::SetConsoleCtrlHandler;
            use windows::Win32::System::Threading::CreateEventW;

            // Create a manual-reset, initially non-signaled event.
            // ctrl_handler calls SetEvent on it so that WaitForMultipleObjects
            // in process_chunks_with_shutdown wakes up immediately.
            let event = unsafe {
                CreateEventW(
                    None,                                // default security
                    windows::Win32::Foundation::BOOL(1), // manual-reset
                    windows::Win32::Foundation::BOOL(0), // not signaled initially
                    windows::core::PCWSTR::null(),       // unnamed
                )
                .map_err(|e| anyhow::anyhow!("Failed to create shutdown event: {e}"))?
            };

            SHUTDOWN_EVENT_HANDLE.store(event.0, Ordering::Relaxed);

            unsafe {
                SetConsoleCtrlHandler(Some(ctrl_handler), windows::Win32::Foundation::BOOL(1))
                    .map_err(|e| anyhow::anyhow!("Failed to register Ctrl handler: {e}"))?;
            }

            log::debug!(
                "Windows Ctrl handler registered (Ctrl+C / close / shutdown → shutdown), \
                 shutdown_event handle={:#x}",
                event.0
            );

            return Ok(Self {
                shutdown_event: event.0,
            });
        }

        #[cfg(not(any(unix, windows)))]
        {
            log::warn!("No signal handling available on this platform");
            return Ok(Self {});
        }

        #[allow(unreachable_code)]
        Err(anyhow::anyhow!("unreachable"))
    }

    /// Returns `true` if a shutdown signal / event has been received.
    #[inline]
    pub fn is_requested() -> bool {
        SHUTDOWN_REQUESTED.load(Ordering::Relaxed)
    }

    /// Returns `true` if SIGHUP has been received (Unix only; stream reload).
    #[inline]
    pub fn is_reload_requested() -> bool {
        RELOAD_REQUESTED.load(Ordering::Relaxed)
    }

    /// Returns the Windows shutdown event handle if signal handling has already
    /// been installed, or `0` otherwise.
    #[cfg(windows)]
    #[inline]
    pub(crate) fn current_shutdown_event() -> isize {
        SHUTDOWN_EVENT_HANDLE.load(Ordering::Relaxed)
    }

    /// Clear the reload flag after the reload has been handled.
    #[inline]
    pub fn clear_reload() {
        RELOAD_REQUESTED.store(false, Ordering::Relaxed);
    }

    /// Returns `true` if a full render restart from config has been requested.
    #[inline]
    pub fn is_restart_from_config_requested() -> bool {
        RESTART_FROM_CONFIG_REQUESTED.load(Ordering::Relaxed)
    }

    /// Clear the restart-from-config flag after it has been handled.
    #[inline]
    pub fn clear_restart_from_config() {
        RESTART_FROM_CONFIG_REQUESTED.store(false, Ordering::Relaxed);
    }

    /// Return a platform-agnostic shutdown signal suitable for passing to
    /// interrupt-aware I/O helpers such as `InputReader::process_chunks_with_shutdown`.
    pub fn shutdown_signal(&self) -> crate::ShutdownSignal {
        crate::ShutdownSignal {
            #[cfg(unix)]
            fd: self.read_fd,
            #[cfg(windows)]
            event: self.shutdown_event,
        }
    }
}

impl Drop for ShutdownHandle {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            // Restore default signal dispositions so the process behaves normally
            // if it continues running after this handle is dropped.
            for &sig in &[libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
                let sa_dfl = libc::sigaction {
                    sa_sigaction: libc::SIG_DFL,
                    sa_mask: unsafe { std::mem::zeroed() },
                    sa_flags: 0,
                    sa_restorer: None,
                };
                unsafe { libc::sigaction(sig, &sa_dfl, std::ptr::null_mut()) };
            }

            // Close the write end first — this causes a POLLHUP on the read end,
            // waking up any thread still blocked in poll().
            let wfd = SIGNAL_WRITE_FD.swap(-1, Ordering::Relaxed);
            if wfd >= 0 {
                unsafe { libc::close(wfd) };
            }
            unsafe { libc::close(self.read_fd) };
        }

        #[cfg(windows)]
        {
            use windows::Win32::Foundation::{CloseHandle, HANDLE};
            unsafe {
                let _ = windows::Win32::System::Console::SetConsoleCtrlHandler(
                    Some(ctrl_handler),
                    windows::Win32::Foundation::BOOL(0),
                );
                let h = SHUTDOWN_EVENT_HANDLE.swap(0, Ordering::Relaxed);
                if h != 0 {
                    let _ = CloseHandle(HANDLE(h));
                }
            }
        }
    }
}

// ─── Programmatic shutdown / reload (Windows service control handler) ────────

/// Programmatically trigger a clean shutdown — equivalent of SIGTERM on Unix.
///
/// Sets `SHUTDOWN_REQUESTED` and signals the overlapped-I/O event so that
/// `process_chunks_with_shutdown` wakes up immediately.
/// Called by the Windows Service control handler on `ServiceControl::Stop`.
#[cfg(unix)]
pub fn request_shutdown() {
    SHUTDOWN_REQUESTED.store(true, Ordering::Relaxed);
    let fd = SIGNAL_WRITE_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        unsafe { libc::write(fd, b"\x00".as_ptr() as *const libc::c_void, 1) };
    }
}

/// Programmatically trigger a clean shutdown — equivalent of SIGTERM on Unix.
///
/// Sets `SHUTDOWN_REQUESTED` and signals the overlapped-I/O event so that
/// `process_chunks_with_shutdown` wakes up immediately.
/// Called by the Windows Service control handler on `ServiceControl::Stop`.
#[cfg(windows)]
pub fn request_shutdown() {
    SHUTDOWN_REQUESTED.store(true, Ordering::Relaxed);
    signal_shutdown_event();
}

/// Programmatically trigger a stream reload — equivalent of SIGHUP on Unix.
///
/// Sets `RELOAD_REQUESTED` and signals the overlapped-I/O event.
/// Called by the Windows Service control handler on `ServiceControl::Custom(128)`.
#[cfg(windows)]
pub fn request_reload() {
    RELOAD_REQUESTED.store(true, Ordering::Relaxed);
    signal_shutdown_event();
}

/// Programmatically trigger a full render restart so CLI/config resolution runs
/// again before the stream is reopened.
#[cfg(unix)]
pub fn request_restart_from_config() {
    RESTART_FROM_CONFIG_REQUESTED.store(true, Ordering::Relaxed);
    let fd = SIGNAL_WRITE_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        unsafe { libc::write(fd, b"\x02".as_ptr() as *const libc::c_void, 1) };
    }
}

/// Programmatically trigger a full render restart so CLI/config resolution runs
/// again before the stream is reopened.
#[cfg(windows)]
pub fn request_restart_from_config() {
    RESTART_FROM_CONFIG_REQUESTED.store(true, Ordering::Relaxed);
    signal_shutdown_event();
}

/// Signal `SHUTDOWN_EVENT_HANDLE` so that any thread blocked in
/// `WaitForMultipleObjects` inside `process_chunks_with_shutdown` wakes up.
#[cfg(windows)]
fn signal_shutdown_event() {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Threading::SetEvent;
    let h = SHUTDOWN_EVENT_HANDLE.load(Ordering::Relaxed);
    if h != 0 {
        unsafe {
            let _ = SetEvent(HANDLE(h));
        }
    }
}

// ─── systemd integration (Unix only) ─────────────────────────────────────────

/// Send a notification to systemd via the sd_notify(3) protocol.
///
/// No-op when `NOTIFY_SOCKET` is not set (i.e. not running under systemd).
///
/// Common messages:
/// - `"READY=1\n"` — service fully initialised, ready to serve
/// - `"STOPPING=1\n"` — service is beginning a clean shutdown
/// - `"WATCHDOG=1\n"` — watchdog keepalive heartbeat
/// - `"STATUS=text\n"` — arbitrary status shown by `systemctl status`
#[cfg(unix)]
pub fn sd_notify(msg: &str) {
    let Ok(socket_path) = std::env::var("NOTIFY_SOCKET") else {
        return; // Not running under systemd
    };

    unsafe {
        let sock = libc::socket(libc::AF_UNIX, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0);
        if sock < 0 {
            return;
        }

        let mut addr: libc::sockaddr_un = std::mem::zeroed();
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;

        // NOTIFY_SOCKET uses '@' as prefix for abstract Unix sockets; replace
        // it with the required leading null byte.
        let is_abstract = socket_path.starts_with('@');
        let path_bytes: Vec<u8> = if is_abstract {
            let mut b = socket_path.into_bytes();
            b[0] = 0;
            b
        } else {
            socket_path.into_bytes()
        };

        let copy_len = path_bytes.len().min(addr.sun_path.len());
        for (i, &b) in path_bytes.iter().take(copy_len).enumerate() {
            addr.sun_path[i] = b as libc::c_char;
        }

        // addr_len = offsetof(sun_path) + path_bytes.len()
        // For abstract sockets the leading null is already part of path_bytes.
        // For filesystem sockets add one byte for the null terminator.
        let addr_len = (std::mem::offset_of!(libc::sockaddr_un, sun_path)
            + path_bytes.len()
            + if is_abstract { 0 } else { 1 }) as libc::socklen_t;

        libc::sendto(
            sock,
            msg.as_ptr() as *const libc::c_void,
            msg.len(),
            libc::MSG_NOSIGNAL,
            &addr as *const libc::sockaddr_un as *const libc::sockaddr,
            addr_len,
        );

        libc::close(sock);
    }
}

/// Start a background thread that sends `WATCHDOG=1` to systemd at half the
/// configured watchdog interval.
///
/// No-op when `WATCHDOG_USEC` is not set or does not match the current PID
/// (`WATCHDOG_PID` check, as recommended by the sd_notify(3) man page).
/// The thread stops automatically when a shutdown signal is received.
#[cfg(unix)]
pub fn spawn_watchdog() {
    // WATCHDOG_PID: systemd sets this to tell exactly which process should
    // send the watchdog ping.  Skip if it targets a different process.
    if let Ok(pid_str) = std::env::var("WATCHDOG_PID") {
        if pid_str.trim().parse::<u32>().ok() != Some(std::process::id()) {
            return;
        }
    }

    let Ok(usec_str) = std::env::var("WATCHDOG_USEC") else {
        return; // Not configured
    };
    let Ok(usec) = usec_str.trim().parse::<u64>() else {
        return;
    };
    if usec == 0 {
        return;
    }

    // Ping at half the configured timeout, as recommended.
    let interval = std::time::Duration::from_micros(usec / 2);

    log::debug!(
        "Watchdog enabled (WATCHDOG_USEC={usec}µs), pinging every {}ms",
        interval.as_millis()
    );

    std::thread::Builder::new()
        .name("watchdog".into())
        .spawn(move || {
            while !ShutdownHandle::is_requested() {
                sd_notify("WATCHDOG=1\n");
                std::thread::sleep(interval);
            }
            log::debug!("Watchdog thread stopped");
        })
        .ok(); // Failure to spawn is non-fatal
}
