use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(unix)]
use std::os::unix::io::AsRawFd;

use anyhow::Result;

/// Unified input reader that handles both file and pipe input with buffered reading
pub struct InputReader {
    reader: Box<dyn Read>,
    is_pipe: bool,
    /// Raw file descriptor used by [`InputReader::process_chunks_with_shutdown`]
    /// for poll(2)-based I/O. Kept separate so the boxed reader (which may
    /// buffer internally) does not interfere with the poll-based read path.
    #[cfg(unix)]
    pub data_fd: Option<i32>,
    /// Raw Windows HANDLE for named-pipe overlapped I/O.
    /// When set, `reader` is a dummy (io::empty()) and all real I/O goes
    /// through `process_chunks_with_shutdown` which uses ReadFile + OVERLAPPED.
    #[cfg(windows)]
    pub raw_handle: Option<isize>,
}

/// Check if a file is a FIFO (named pipe) on Unix systems
#[cfg(unix)]
fn is_fifo<P: AsRef<Path>>(path: P) -> Result<bool> {
    let metadata = std::fs::metadata(path)?;
    // S_IFIFO = 0o010000 (FIFO mask in st_mode)
    Ok((metadata.mode() & 0o170000) == 0o010000)
}

/// Create a FIFO (named pipe) on Unix systems
#[cfg(unix)]
fn create_fifo<P: AsRef<Path>>(path: P) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let path_cstr = CString::new(path.as_ref().as_os_str().as_bytes())?;

    // The decoder may run as a system service while mpv runs as the desktop
    // user, so the FIFO must stay writable across users.
    let result = unsafe { libc::mkfifo(path_cstr.as_ptr(), 0o666) };

    if result != 0 {
        return Err(anyhow::anyhow!(
            "Failed to create FIFO: {}",
            io::Error::last_os_error()
        ));
    }

    Ok(())
}

#[cfg(unix)]
fn ensure_fifo_permissions<P: AsRef<Path>>(path: P) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let path_ref = path.as_ref();
    if !is_fifo(path_ref)? {
        return Ok(());
    }

    let metadata = std::fs::metadata(path_ref)?;
    if metadata.mode() & 0o222 == 0o222 {
        return Ok(());
    }

    let path_cstr = CString::new(path_ref.as_os_str().as_bytes())?;
    let result = unsafe { libc::chmod(path_cstr.as_ptr(), 0o666) };

    if result != 0 {
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::PermissionDenied {
            log::warn!(
                "Could not chmod FIFO {} to 0666 ({}); keeping existing permissions",
                path_ref.display(),
                err
            );
            return Ok(());
        }
        return Err(anyhow::anyhow!(
            "Failed to chmod FIFO {}: {}",
            path_ref.display(),
            err
        ));
    }

    Ok(())
}

/// Create a Windows named pipe server opened in overlapped mode.
///
/// Returns the raw HANDLE as `isize`. The caller owns the handle and must
/// close it (via the `InputReader::drop` impl).
#[cfg(windows)]
fn create_named_pipe_server(pipe_name: &str) -> Result<isize> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_OVERLAPPED, FILE_GENERIC_READ, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING,
    };
    use windows::core::PCWSTR;

    let wide_name: Vec<u16> = OsStr::new(pipe_name)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    log::info!(
        "Creating Windows named pipe server (overlapped): {}",
        pipe_name
    );

    // Try to open an already-existing server pipe first.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide_name.as_ptr()),
            FILE_GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED, // overlapped mode for interruptible reads
            HANDLE::default(),
        )
    };

    let handle = match handle {
        Ok(h) if h != INVALID_HANDLE_VALUE => {
            log::info!("Named pipe already exists, opened in overlapped mode");
            h
        }
        _ => {
            // Pipe doesn't exist (or client hasn't connected yet) — create a
            // new server-side pipe instance and block until a client connects.
            create_new_pipe_server_overlapped(&wide_name)?
        }
    };

    Ok(handle.0)
}

/// One inbound, overlapped instance of the named pipe `wide_name` (UTF-16,
/// NUL-terminated), not connected yet.
#[cfg(windows)]
fn create_pipe_instance(wide_name: &[u16]) -> Result<windows::Win32::Foundation::HANDLE> {
    use windows::Win32::Foundation::{BOOL, INVALID_HANDLE_VALUE};
    use windows::Win32::Security::{
        InitializeSecurityDescriptor, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
        SECURITY_DESCRIPTOR, SetSecurityDescriptorDacl,
    };
    use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
    use windows::Win32::System::Pipes::{CreateNamedPipeW, NAMED_PIPE_MODE};
    use windows::core::PCWSTR;

    // dwOpenMode = PIPE_ACCESS_INBOUND (0x1) | FILE_FLAG_OVERLAPPED (0x40000000)
    const PIPE_ACCESS_INBOUND_U32: u32 = 0x00000001;
    const FILE_FLAG_OVERLAPPED_U32: u32 = 0x40000000;
    const PIPE_TYPE_BYTE_U32: u32 = 0x00000000;
    const PIPE_WAIT_U32: u32 = 0x00000000;
    const PIPE_UNLIMITED_INSTANCES: u32 = 255;

    // Build a null-DACL security descriptor so that any user (including
    // interactive users in Session 1) can connect and write to the pipe,
    // even when orender runs as a Windows service in Session 0 under
    // LocalSystem.  A null DACL grants full access to everyone; without
    // this the default DACL inherited from LocalSystem's token blocks
    // unprivileged callers such as mpv running on the desktop.
    let mut sd: SECURITY_DESCRIPTOR = unsafe { std::mem::zeroed() };
    let psd = PSECURITY_DESCRIPTOR(&mut sd as *mut SECURITY_DESCRIPTOR as *mut _);
    unsafe {
        InitializeSecurityDescriptor(psd, 1) // 1 = SECURITY_DESCRIPTOR_REVISION
            .map_err(|e| anyhow::anyhow!("InitializeSecurityDescriptor: {e}"))?;
        SetSecurityDescriptorDacl(
            psd,
            BOOL(1), // bDaclPresent = TRUE
            None,    // pDacl = NULL → null DACL, grants access to everyone
            BOOL(0), // bDaclDefaulted = FALSE
        )
        .map_err(|e| anyhow::anyhow!("SetSecurityDescriptorDacl: {e}"))?;
    }
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: psd.0,
        bInheritHandle: BOOL(0),
    };

    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(wide_name.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(PIPE_ACCESS_INBOUND_U32 | FILE_FLAG_OVERLAPPED_U32),
            NAMED_PIPE_MODE(PIPE_TYPE_BYTE_U32 | PIPE_WAIT_U32),
            PIPE_UNLIMITED_INSTANCES,
            65536,
            65536,
            0,
            Some(&sa),
        )
    };

    if handle == INVALID_HANDLE_VALUE {
        return Err(anyhow::anyhow!(
            "Failed to create named pipe: {}",
            io::Error::last_os_error()
        ));
    }

    Ok(handle)
}

/// A named pipe served for one writer at a time, read with a timeout: what a
/// reader that must stay stoppable reads (the sync host's capture). The
/// instance outlives its writers: when one disconnects, the next is waited
/// for on the same handle.
///
/// One overlapped operation is pending at a time (the connect, or a read
/// into the buffer this owns), and [`read`](Self::read) only waits for it.
#[cfg(windows)]
pub struct NamedPipeSource {
    handle: windows::Win32::Foundation::HANDLE,
    event: windows::Win32::Foundation::HANDLE,
    /// Boxed: the kernel keeps its address while an operation is pending.
    overlapped: Box<windows::Win32::System::IO::OVERLAPPED>,
    buffer: Box<[u8]>,
    state: PipeState,
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PipeState {
    /// No writer, and no connect pending.
    Idle,
    /// A connect is pending.
    Connecting,
    /// A writer is connected, and no read is pending.
    Connected,
    /// A read into `buffer` is pending.
    Reading,
}

// SAFETY: the handles are owned by this value alone, and Win32 handles may
// be used from any thread.
#[cfg(windows)]
unsafe impl Send for NamedPipeSource {}

#[cfg(windows)]
impl NamedPipeSource {
    const ERROR_IO_PENDING: u32 = 997;
    const ERROR_PIPE_CONNECTED: u32 = 535;
    const ERROR_NO_DATA: u32 = 232;
    const WAIT_OBJECT_0: u32 = 0;
    const WAIT_TIMEOUT: u32 = 258;

    /// Create the pipe `name` (`\\.\pipe\…`), with `chunk` bytes of read
    /// buffer. Writers can connect from then on.
    pub fn create(name: &str, chunk: usize) -> Result<Self> {
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::Foundation::BOOL;
        use windows::Win32::System::Threading::CreateEventW;

        let wide: Vec<u16> = std::ffi::OsStr::new(name)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let handle = create_pipe_instance(&wide)?;
        // Manual-reset: it stays signaled until the next operation starts.
        let event = unsafe { CreateEventW(None, BOOL(1), BOOL(0), windows::core::PCWSTR::null()) }
            .map_err(|e| {
                unsafe {
                    let _ = windows::Win32::Foundation::CloseHandle(handle);
                }
                anyhow::anyhow!("CreateEventW: {e}")
            })?;
        Ok(Self {
            handle,
            event,
            // SAFETY: an all-zero OVERLAPPED is its initial state.
            overlapped: Box::new(unsafe { std::mem::zeroed() }),
            buffer: vec![0u8; chunk.max(1)].into_boxed_slice(),
            state: PipeState::Idle,
        })
    }

    /// Start an operation: the event is reset and set in a fresh OVERLAPPED.
    fn arm(&mut self) {
        use windows::Win32::System::Threading::ResetEvent;
        // SAFETY: no operation is pending on `overlapped` (the caller's
        // state says so), and the event is ours.
        unsafe {
            let _ = ResetEvent(self.event);
            *self.overlapped = std::mem::zeroed();
        }
        self.overlapped.hEvent = self.event;
    }

    /// The writer is gone: free the instance for the next one.
    fn disconnect(&mut self) {
        // SAFETY: our handle; nothing is pending.
        unsafe {
            let _ = windows::Win32::System::Pipes::DisconnectNamedPipe(self.handle);
        }
        self.state = PipeState::Idle;
    }

    /// Wait up to `timeout_ms` for bytes, and copy them into `out`: `Some(n)`
    /// with `n > 0`, or `None` when nothing came (no writer yet, a writer
    /// that left, a writer that is silent).
    pub fn read(&mut self, out: &mut [u8], timeout_ms: u32) -> Option<usize> {
        use windows::Win32::Foundation::{BOOL, GetLastError};
        use windows::Win32::Storage::FileSystem::ReadFile;
        use windows::Win32::System::IO::GetOverlappedResult;
        use windows::Win32::System::Pipes::ConnectNamedPipe;
        use windows::Win32::System::Threading::WaitForSingleObject;

        loop {
            match self.state {
                PipeState::Idle => {
                    self.arm();
                    // SAFETY: our handle and our boxed OVERLAPPED.
                    let connected =
                        unsafe { ConnectNamedPipe(self.handle, Some(&mut *self.overlapped)) };
                    let error = unsafe { GetLastError().0 };
                    self.state = match connected {
                        Ok(()) => PipeState::Connected,
                        Err(_) if error == Self::ERROR_PIPE_CONNECTED => PipeState::Connected,
                        Err(_) if error == Self::ERROR_IO_PENDING => PipeState::Connecting,
                        // A writer that came and went before the connect.
                        Err(_) if error == Self::ERROR_NO_DATA => {
                            self.disconnect();
                            return None;
                        }
                        Err(e) => {
                            log::debug!("ConnectNamedPipe: {e}");
                            std::thread::sleep(std::time::Duration::from_millis(u64::from(
                                timeout_ms,
                            )));
                            return None;
                        }
                    };
                }
                PipeState::Connecting | PipeState::Reading => {
                    // SAFETY: our event.
                    let wait = unsafe { WaitForSingleObject(self.event, timeout_ms) }.0;
                    if wait == Self::WAIT_TIMEOUT {
                        return None;
                    }
                    if wait != Self::WAIT_OBJECT_0 {
                        log::debug!("WaitForSingleObject returned {wait:#x}");
                        return None;
                    }
                    let mut transferred = 0u32;
                    // SAFETY: the operation on `overlapped` has completed.
                    let done = unsafe {
                        GetOverlappedResult(
                            self.handle,
                            &*self.overlapped,
                            &mut transferred,
                            BOOL(0),
                        )
                    };
                    let was_reading = self.state == PipeState::Reading;
                    match done {
                        Ok(()) if was_reading && transferred > 0 => {
                            self.state = PipeState::Connected;
                            return Some(self.take(out, transferred as usize));
                        }
                        Ok(()) if was_reading => self.disconnect(),
                        Ok(()) => self.state = PipeState::Connected,
                        // The writer left (broken pipe), or the connect failed.
                        Err(_) => self.disconnect(),
                    }
                    if self.state == PipeState::Idle {
                        return None;
                    }
                }
                PipeState::Connected => {
                    self.arm();
                    let want = self.buffer.len().min(out.len());
                    // SAFETY: `buffer` and `overlapped` are boxed and outlive
                    // the operation (it is cancelled and waited for on drop).
                    let read = unsafe {
                        ReadFile(
                            self.handle,
                            Some(&mut self.buffer[..want]),
                            None,
                            Some(&mut *self.overlapped),
                        )
                    };
                    let error = unsafe { GetLastError().0 };
                    match read {
                        // Completed at once: the result is read as for one
                        // that was pending (the event is signaled).
                        Ok(()) => self.state = PipeState::Reading,
                        Err(_) if error == Self::ERROR_IO_PENDING => {
                            self.state = PipeState::Reading
                        }
                        Err(_) => {
                            self.disconnect();
                            return None;
                        }
                    }
                }
            }
        }
    }

    /// Hand over the `n` bytes the read left in the buffer.
    fn take(&mut self, out: &mut [u8], n: usize) -> usize {
        let n = n.min(out.len());
        out[..n].copy_from_slice(&self.buffer[..n]);
        n
    }
}

#[cfg(windows)]
impl Drop for NamedPipeSource {
    fn drop(&mut self) {
        use windows::Win32::Foundation::{BOOL, CloseHandle};
        use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult};
        // SAFETY: our handles; a pending operation is cancelled and waited
        // for before the OVERLAPPED and the buffer it writes to are freed.
        unsafe {
            if matches!(self.state, PipeState::Connecting | PipeState::Reading) {
                let _ = CancelIoEx(self.handle, Some(&*self.overlapped));
                let mut transferred = 0u32;
                let _ =
                    GetOverlappedResult(self.handle, &*self.overlapped, &mut transferred, BOOL(1));
            }
            let _ = CloseHandle(self.handle);
            let _ = CloseHandle(self.event);
        }
    }
}

#[cfg(all(test, windows))]
mod pipe_tests {
    use super::NamedPipeSource;
    use std::io::Write;

    fn name(tag: &str) -> String {
        format!(r"\\.\pipe\orender-test-{}-{tag}", std::process::id())
    }

    /// Read until `want` bytes came or `tries` timeouts passed.
    fn read_all(pipe: &mut NamedPipeSource, want: usize, tries: usize) -> Vec<u8> {
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        for _ in 0..tries {
            if let Some(n) = pipe.read(&mut buf, 50) {
                got.extend_from_slice(&buf[..n]);
            }
            if got.len() >= want {
                break;
            }
        }
        got
    }

    /// With no writer a read times out, again and again; what a writer
    /// sends comes out in order; and after it leaves, the next writer is
    /// served on the same pipe.
    #[test]
    fn writers_come_and_go_and_reads_time_out_between_them() {
        let name = name("come-and-go");
        let mut pipe = NamedPipeSource::create(&name, 4096).unwrap();
        let mut buf = [0u8; 16];
        let started = std::time::Instant::now();
        assert_eq!(pipe.read(&mut buf, 30), None);
        assert_eq!(pipe.read(&mut buf, 30), None);
        assert!(started.elapsed() >= std::time::Duration::from_millis(50));

        for round in 0..2u8 {
            let data: Vec<u8> = (0..10_000u32).map(|i| (i as u8) ^ round).collect();
            let (path, sent) = (name.clone(), data.clone());
            let writer = std::thread::spawn(move || {
                let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
                file.write_all(&sent).unwrap();
            });
            let got = read_all(&mut pipe, data.len(), 200);
            writer.join().unwrap();
            assert_eq!(got, data, "round {round}");
            // The writer is gone: reads time out until the next one.
            assert_eq!(read_all(&mut pipe, 1, 3), Vec::<u8>::new());
        }
    }

    /// Dropping the source with a connect or a read pending does not hang.
    #[test]
    fn a_pending_operation_is_cancelled_on_drop() {
        let mut pipe = NamedPipeSource::create(&name("drop"), 4096).unwrap();
        let mut buf = [0u8; 16];
        assert_eq!(pipe.read(&mut buf, 10), None);
        drop(pipe);
    }
}

/// Create a new named-pipe server instance with FILE_FLAG_OVERLAPPED and wait
/// for the first client connection.
#[cfg(windows)]
fn create_new_pipe_server_overlapped(
    wide_name: &[u16],
) -> Result<windows::Win32::Foundation::HANDLE> {
    use windows::Win32::Foundation::{BOOL, GetLastError, HANDLE};
    use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
    use windows::Win32::System::Pipes::ConnectNamedPipe;
    use windows::Win32::System::Threading::{
        CreateEventW, INFINITE, ResetEvent, WaitForMultipleObjects,
    };

    let handle = create_pipe_instance(wide_name)?;

    log::info!("Named pipe server created, waiting for client connection...");

    const ERROR_IO_PENDING: u32 = 997;
    const ERROR_PIPE_CONNECTED: i32 = 535;
    const ERROR_OPERATION_ABORTED: u32 = 995;
    const WAIT_OBJECT_0: u32 = 0x0000_0000;

    let shutdown_event = HANDLE(crate::ShutdownHandle::current_shutdown_event());
    let io_event = unsafe {
        CreateEventW(
            None,
            BOOL(1), // manual-reset
            BOOL(0), // not signaled
            windows::core::PCWSTR::null(),
        )
        .map_err(|e| anyhow::anyhow!("CreateEventW failed during pipe connect: {e}"))?
    };

    let connect_result: Result<()> = (|| {
        let _ = unsafe { ResetEvent(io_event) };

        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlapped.hEvent = io_event;

        let connect_result = unsafe { ConnectNamedPipe(handle, Some(&mut overlapped)) };
        let last_err = unsafe { GetLastError().0 };

        if connect_result.is_ok() {
            return Ok(());
        }

        if (connect_result.err().unwrap().code().0 as i32) == ERROR_PIPE_CONNECTED {
            return Ok(());
        }

        if last_err != ERROR_IO_PENDING {
            return Err(anyhow::anyhow!(
                "Failed to connect named pipe: {}",
                io::Error::last_os_error()
            ));
        }

        let handles = [io_event, shutdown_event];
        let wait = unsafe { WaitForMultipleObjects(&handles, BOOL(0), INFINITE).0 };
        match wait {
            n if n == WAIT_OBJECT_0 => {}
            n if n == WAIT_OBJECT_0 + 1 => {
                unsafe {
                    let _ = CancelIoEx(handle, Some(&overlapped));
                    let mut dummy = 0u32;
                    let _ = GetOverlappedResult(handle, &overlapped, &mut dummy, BOOL(1));
                }
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "Shutdown requested while waiting for named pipe client",
                )
                .into());
            }
            _ => {
                return Err(anyhow::anyhow!(
                    "WaitForMultipleObjects failed during pipe connect: {wait:#x}"
                ));
            }
        }

        let mut transferred = 0u32;
        match unsafe { GetOverlappedResult(handle, &overlapped, &mut transferred, BOOL(0)) } {
            Ok(()) => Ok(()),
            Err(_) => {
                let err = unsafe { GetLastError().0 };
                if err == ERROR_OPERATION_ABORTED {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "Named pipe connect cancelled during shutdown",
                    )
                    .into())
                } else {
                    Err(anyhow::anyhow!(
                        "ConnectNamedPipe completion failed: WIN32={err}"
                    ))
                }
            }
        }
    })();

    unsafe {
        let _ = windows::Win32::Foundation::CloseHandle(io_event);
    }

    connect_result?;

    log::info!("Client connected to named pipe");
    Ok(handle)
}

/// Drain all pending data from a file descriptor in non-blocking mode
/// Returns the number of bytes drained
#[cfg(unix)]
fn drain_fd(file: &File) -> Result<usize> {
    use std::os::unix::io::AsRawFd;

    let fd = file.as_raw_fd();
    let mut total_drained = 0usize;
    let mut drain_buf = vec![0u8; 64 * 1024];

    // Set non-blocking mode
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 {
            return Err(anyhow::anyhow!("Failed to get file flags"));
        }
        if libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(anyhow::anyhow!("Failed to set non-blocking mode"));
        }

        // Drain all available data
        loop {
            let result = libc::read(
                fd,
                drain_buf.as_mut_ptr() as *mut libc::c_void,
                drain_buf.len(),
            );

            if result < 0 {
                let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
                if errno == libc::EAGAIN || errno == libc::EWOULDBLOCK {
                    // No more data available
                    break;
                }
                return Err(anyhow::anyhow!(
                    "Read error while draining: errno {}",
                    errno
                ));
            } else if result == 0 {
                // EOF (shouldn't happen for FIFO, but handle it)
                break;
            } else {
                total_drained += result as usize;
            }
        }

        // Restore blocking mode
        if libc::fcntl(fd, libc::F_SETFL, flags) < 0 {
            return Err(anyhow::anyhow!("Failed to restore blocking mode"));
        }
    }

    Ok(total_drained)
}

impl InputReader {
    /// Create a new InputReader from a path.
    /// Use "-" for stdin pipe input.
    ///
    /// # Arguments
    /// * `input_path` - Path to input file or "-" for stdin
    /// * `drain_pipe` - If true, drain buffered data from named pipes before reading
    pub fn new<P: AsRef<Path>>(input_path: P, drain_pipe: bool) -> Result<Self> {
        let path_str = input_path.as_ref().to_string_lossy();
        let is_stdin_pipe = path_str == "-";

        if is_stdin_pipe {
            return Ok(Self {
                reader: Box::new(io::stdin().lock()),
                is_pipe: true,
                #[cfg(unix)]
                data_fd: Some(libc::STDIN_FILENO),
                #[cfg(windows)]
                raw_handle: None, // stdin not supported with overlapped I/O for now
            });
        }

        #[cfg(windows)]
        {
            if path_str.starts_with(r"\\.\pipe\") {
                // Named pipe in overlapped mode: bypass BufReader and use the
                // raw HANDLE directly in process_chunks_with_shutdown.
                if drain_pipe {
                    log::warn!("Draining named pipe buffer is not supported on Windows.");
                }
                let raw_handle = create_named_pipe_server(&path_str)?;
                return Ok(Self {
                    reader: Box::new(io::empty()), // unused; overlapped path bypasses this
                    is_pipe: true,
                    raw_handle: Some(raw_handle),
                });
            } else {
                let file = File::open(&input_path)?;
                return Ok(Self {
                    reader: Box::new(BufReader::new(file)),
                    is_pipe: false,
                    raw_handle: None,
                });
            }
        }

        #[cfg(unix)]
        {
            use std::ffi::CString;
            use std::os::unix::ffi::OsStrExt;
            use std::os::unix::io::FromRawFd;

            let path_cstr = CString::new(input_path.as_ref().as_os_str().as_bytes())?;

            // Open with O_NONBLOCK so that opening a FIFO with no writer connected
            // does not block the decoder thread.  With SA_RESTART (used by the signal
            // handler), a plain blocking open(2) on a FIFO would restart after SIGINT,
            // keeping the thread stuck and preventing clean Ctrl-C shutdown.
            // We clear O_NONBLOCK right after open() so that subsequent reads behave
            // normally; process_chunks_with_shutdown uses poll(2) before every read,
            // so it will never block — the signal pipe wakes it up within 200 ms.
            let open_nonblock = |path: &CString| -> io::Result<i32> {
                let fd = unsafe {
                    libc::open(
                        path.as_ptr(),
                        libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
                    )
                };
                if fd >= 0 {
                    Ok(fd)
                } else {
                    Err(io::Error::last_os_error())
                }
            };

            let raw_fd = match open_nonblock(&path_cstr) {
                Ok(fd) => fd,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    // Path doesn't exist — create it as a FIFO and retry once.
                    log::info!("Creating named pipe: {}", path_str);
                    create_fifo(&input_path)?;
                    ensure_fifo_permissions(&input_path)?;
                    log::info!("Named pipe created successfully: {}", path_str);
                    open_nonblock(&path_cstr).map_err(anyhow::Error::from)?
                }
                Err(e) => return Err(e.into()),
            };

            ensure_fifo_permissions(&input_path)?;

            // Clear O_NONBLOCK now that open() has returned.
            let flags = unsafe { libc::fcntl(raw_fd, libc::F_GETFL) };
            if flags >= 0 {
                unsafe { libc::fcntl(raw_fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) };
            }

            let file = unsafe { File::from_raw_fd(raw_fd) };

            let is_named_pipe = is_fifo(&input_path)?;

            // Detect if this is a FIFO and drain if requested
            if drain_pipe && is_named_pipe {
                let drained = drain_fd(&file)?;
                if drained > 0 {
                    log::info!(
                        "Drained {} bytes ({:.2} KB) from named pipe to minimize latency",
                        drained,
                        drained as f64 / 1024.0
                    );
                } else {
                    log::debug!("Named pipe detected but no buffered data to drain");
                }
            }

            // Save the fd *before* moving `file` into BufReader so we can use
            // it for poll(2) in process_chunks_with_shutdown.
            let data_fd = file.as_raw_fd();

            return Ok(Self {
                reader: Box::new(BufReader::new(file)),
                is_pipe: is_named_pipe,
                data_fd: Some(data_fd),
            });
        }

        // Fallback for other OSes: treat as a regular file
        #[cfg(not(any(unix, windows)))]
        {
            let file = File::open(&input_path)?;
            Ok(Self {
                reader: Box::new(BufReader::new(file)),
                is_pipe: false,
            })
        }

        // Needed on unix/windows because of the early returns above — the
        // compiler needs a reachable expression here on those targets.
        #[allow(unreachable_code)]
        Err(anyhow::anyhow!("unreachable"))
    }

    /// Read a chunk of data into the provided buffer.
    /// Returns the number of bytes read, 0 indicates EOF.
    pub fn read_chunk(&mut self, buffer: &mut [u8]) -> Result<usize> {
        let bytes_read = self.reader.read(buffer)?;
        Ok(bytes_read)
    }

    /// Check if this is pipe input.
    pub fn is_pipe(&self) -> bool {
        self.is_pipe
    }

    /// Read all remaining data for non-streaming use cases.
    pub fn read_all(&mut self) -> Result<Vec<u8>> {
        let mut data = Vec::new();
        self.reader.read_to_end(&mut data)?;
        Ok(data)
    }

    /// Process data in chunks using a callback function.
    /// The callback receives each chunk and should return Ok(true) to continue or Ok(false) to stop.
    pub fn process_chunks<F>(&mut self, chunk_size: usize, mut callback: F) -> Result<()>
    where
        F: FnMut(&[u8]) -> Result<bool>,
    {
        let mut buffer = vec![0u8; chunk_size];

        loop {
            let bytes_read = self.read_chunk(&mut buffer)?;
            if bytes_read == 0 {
                break; // EOF
            }

            if !callback(&buffer[..bytes_read])? {
                break; // Callback requested stop
            }
        }

        Ok(())
    }

    /// Process data chunks using `poll(2)` so that a shutdown signal fd can be
    /// monitored alongside the data fd.
    ///
    /// Polls both `self.data_fd` and `signal_fd` with a 200 ms timeout.  When
    /// `signal` becomes readable (or `ShutdownHandle::is_requested()` is set)
    /// the loop exits cleanly without returning an error.
    ///
    /// Falls back to [`Self::process_chunks`] when no `data_fd` is available.
    pub fn process_chunks_with_shutdown<F>(
        &mut self,
        chunk_size: usize,
        signal: &crate::ShutdownSignal,
        mut callback: F,
    ) -> Result<()>
    where
        F: FnMut(&[u8]) -> Result<bool>,
    {
        #[cfg(unix)]
        let signal_fd = signal.fd;
        #[cfg(windows)]
        return self.process_chunks_with_shutdown_windows(chunk_size, signal.event, &mut callback);
        #[cfg(not(any(unix, windows)))]
        return self.process_chunks(chunk_size, &mut callback);

        #[cfg(unix)]
        {
            let Some(data_fd) = self.data_fd else {
                // No raw fd available — fall back to the regular buffered path.
                return self.process_chunks(chunk_size, callback);
            };

            let mut buffer = vec![0u8; chunk_size];

            loop {
                // Check flags first — avoids one extra poll() when already signalled.
                if crate::ShutdownHandle::is_requested()
                    || crate::ShutdownHandle::is_reload_requested()
                    || crate::ShutdownHandle::is_restart_from_config_requested()
                {
                    break;
                }

                let mut pollfds = [
                    libc::pollfd {
                        fd: data_fd,
                        events: libc::POLLIN | libc::POLLHUP,
                        revents: 0,
                    },
                    libc::pollfd {
                        fd: signal_fd,
                        events: libc::POLLIN | libc::POLLHUP,
                        revents: 0,
                    },
                ];

                // 200 ms timeout: worst-case latency for detecting a shutdown signal
                // when poll() was just entered before the signal handler ran.
                let nready = unsafe { libc::poll(pollfds.as_mut_ptr(), 2, 200) };

                if nready < 0 {
                    let err = std::io::Error::last_os_error();
                    if err.kind() == std::io::ErrorKind::Interrupted {
                        // EINTR from signal handler — loop; flag is now set
                        continue;
                    }
                    return Err(err.into());
                }

                // Signal fd became readable (shutdown, reload, or restart).
                // Drain all pending bytes so the next poll() call doesn't wake
                // up immediately on leftover data.  The caller checks the flags.
                if pollfds[1].revents != 0 {
                    let mut discard = [0u8; 64];
                    loop {
                        let n = unsafe {
                            libc::read(
                                signal_fd,
                                discard.as_mut_ptr() as *mut libc::c_void,
                                discard.len(),
                            )
                        };
                        if n <= 0 {
                            break;
                        }
                    }
                    break;
                }

                if nready == 0 {
                    // Timeout — no data yet, check flag again at top of loop
                    continue;
                }

                // EOF on data fd: writer closed the pipe with no pending data
                if pollfds[0].revents & libc::POLLIN == 0 && pollfds[0].revents & libc::POLLHUP != 0
                {
                    log::debug!("Input EOF (POLLHUP without POLLIN)");
                    break;
                }

                if pollfds[0].revents & (libc::POLLIN | libc::POLLHUP) == 0 {
                    continue;
                }

                // Read directly from the raw fd, bypassing the BufReader wrapper.
                // This keeps the poll() fd state consistent with what we actually read.
                let n = loop {
                    let result = unsafe {
                        libc::read(
                            data_fd,
                            buffer.as_mut_ptr() as *mut libc::c_void,
                            buffer.len(),
                        )
                    };
                    if result < 0 {
                        let err = std::io::Error::last_os_error();
                        if err.kind() == std::io::ErrorKind::Interrupted {
                            continue; // EINTR — retry the read
                        }
                        return Err(err.into());
                    }
                    break result as usize;
                };

                if n == 0 {
                    break; // EOF
                }

                if !callback(&buffer[..n])? {
                    break; // Callback requested stop
                }
            }

            Ok(())
        } // end #[cfg(unix)]
    }

    /// Windows implementation: overlapped ReadFile + WaitForMultipleObjects.
    #[cfg(windows)]
    fn process_chunks_with_shutdown_windows<F>(
        &mut self,
        chunk_size: usize,
        shutdown_event: isize,
        mut callback: F,
    ) -> Result<()>
    where
        F: FnMut(&[u8]) -> Result<bool>,
    {
        let Some(raw_handle) = self.raw_handle else {
            return self.process_chunks(chunk_size, callback);
        };

        use windows::Win32::Foundation::{BOOL, CloseHandle, GetLastError, HANDLE};
        use windows::Win32::Storage::FileSystem::ReadFile;
        use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
        use windows::Win32::System::Threading::{
            CreateEventW, INFINITE, ResetEvent, WaitForMultipleObjects,
        };

        // Win32 error codes used below
        const ERROR_IO_PENDING: u32 = 997;
        const ERROR_BROKEN_PIPE: u32 = 109;
        const ERROR_HANDLE_EOF: u32 = 38;
        const ERROR_OPERATION_ABORTED: u32 = 995;
        const ERROR_PIPE_NOT_CONNECTED: u32 = 233;

        // WaitForMultipleObjects return values
        const WAIT_OBJECT_0: u32 = 0x0000_0000;
        const WAIT_FAILED: u32 = 0xFFFF_FFFF;

        let pipe_handle = HANDLE(raw_handle);
        let sd_handle = HANDLE(shutdown_event);

        // Manual-reset event for I/O completion notification.
        // We reset it explicitly before each ReadFile call.
        let io_event = unsafe {
            CreateEventW(
                None,
                BOOL(1), // manual-reset
                BOOL(0), // not signaled
                windows::core::PCWSTR::null(),
            )
            .map_err(|e| anyhow::anyhow!("CreateEventW failed: {e}"))?
        };

        let result: Result<()> = (|| {
            let mut buffer = vec![0u8; chunk_size];

            loop {
                // Fast path: check flag before blocking.
                if crate::ShutdownHandle::is_requested() {
                    break;
                }

                // Reset the I/O event before starting a new read.
                let _ = unsafe { ResetEvent(io_event) };

                let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
                overlapped.hEvent = io_event;

                // Start an overlapped read.  On an overlapped-mode pipe handle,
                // ReadFile returns immediately with ERROR_IO_PENDING when no data
                // is available yet, rather than blocking.
                let read_result = unsafe {
                    ReadFile(
                        pipe_handle,
                        Some(buffer.as_mut_slice()),
                        None, // byte count comes from GetOverlappedResult
                        Some(&mut overlapped),
                    )
                };

                let last_err = unsafe { GetLastError().0 };

                if read_result.is_err() && last_err != ERROR_IO_PENDING {
                    // Real error — treat as end-of-stream.
                    if last_err != ERROR_BROKEN_PIPE
                        && last_err != ERROR_HANDLE_EOF
                        && last_err != ERROR_PIPE_NOT_CONNECTED
                    {
                        log::debug!("ReadFile error WIN32={last_err}, treating as EOF");
                    }
                    break;
                }

                if last_err == ERROR_IO_PENDING {
                    // Async: wait for I/O completion OR shutdown signal.
                    let handles = [io_event, sd_handle];
                    let wait = unsafe { WaitForMultipleObjects(&handles, BOOL(0), INFINITE).0 };

                    match wait {
                        // io_event signaled — I/O completed.
                        n if n == WAIT_OBJECT_0 => { /* fall through to GetOverlappedResult */ }
                        // shutdown_event signaled — cancel the pending read.
                        n if n == WAIT_OBJECT_0 + 1 => {
                            unsafe {
                                let _ = CancelIoEx(pipe_handle, Some(&overlapped));
                                // Wait for cancellation to complete (avoids use-after-free
                                // of the OVERLAPPED on the stack).
                                let mut dummy = 0u32;
                                let _ = GetOverlappedResult(
                                    pipe_handle,
                                    &overlapped,
                                    &mut dummy,
                                    BOOL(1), // bWait = TRUE
                                );
                            }
                            break;
                        }
                        WAIT_FAILED | _ => {
                            log::debug!("WaitForMultipleObjects returned {wait:#x}");
                            break;
                        }
                    }
                }
                // else: read completed synchronously (last_err == 0).

                // Collect the transferred byte count.
                let mut transferred = 0u32;
                match unsafe {
                    GetOverlappedResult(pipe_handle, &overlapped, &mut transferred, BOOL(0))
                } {
                    Ok(()) => {}
                    Err(_) => {
                        let e = unsafe { GetLastError().0 };
                        if e != ERROR_OPERATION_ABORTED
                            && e != ERROR_BROKEN_PIPE
                            && e != ERROR_HANDLE_EOF
                        {
                            log::debug!("GetOverlappedResult error WIN32={e}");
                        }
                        break;
                    }
                }

                if transferred == 0 {
                    break; // EOF
                }

                log::trace!(
                    "pipe read: transferred={} chunk_size={}",
                    transferred,
                    chunk_size,
                );

                if !callback(&buffer[..transferred as usize])? {
                    break;
                }
            }
            Ok(())
        })();

        unsafe {
            let _ = CloseHandle(io_event);
        }

        result
    }
}

/// Close the overlapped pipe HANDLE on drop (Windows only).
/// The `BufReader<File>` path closes its handle through `File::drop`.
/// For the overlapped path we own the HANDLE directly, so we close it here.
#[cfg(windows)]
impl Drop for InputReader {
    fn drop(&mut self) {
        if let Some(h) = self.raw_handle.take() {
            if h != 0 {
                unsafe {
                    let _ = windows::Win32::Foundation::CloseHandle(
                        windows::Win32::Foundation::HANDLE(h),
                    );
                }
            }
        }
    }
}
