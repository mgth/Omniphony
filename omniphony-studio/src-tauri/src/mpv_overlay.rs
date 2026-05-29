//! Bridge from Studio to a running mpv instance over its JSON IPC socket.
//!
//! Studio decimates audio object positions/levels at ~30 Hz and pushes them
//! to the `omniphony-overlay` lua script in mpv via `script-message`. The
//! lua script then draws the live X/Z + Y-color overlay on top of the
//! video.
//!
//! The socket path is whatever mpv was launched with
//! (`--input-ipc-server=…`):
//!  - Unix: a filesystem path to a Unix domain socket (e.g.
//!    `/tmp/omniphony-mpv.sock`).
//!  - Windows: a named pipe path (e.g. `\\.\pipe\omniphony-mpv`).
//! No normalisation — the user types the literal mpv was launched with,
//! same convention as the audio input pipe.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::net::UnixStream;

const PREFS_FILENAME: &str = "mpv_overlay.json";

#[cfg(unix)]
pub const DEFAULT_SOCKET_PATH: &str = "/tmp/omniphony-mpv.sock";
#[cfg(windows)]
pub const DEFAULT_SOCKET_PATH: &str = r"\\.\pipe\omniphony-mpv";

/// Open the mpv IPC endpoint at `path` and return a (reader, writer) pair
/// of owned byte streams. On Unix the path is a Unix domain socket; on
/// Windows it's a named pipe (`\\.\pipe\<name>`) opened in overlapped
/// mode (see [`windows_pipe`]) so the writer can time out instead of
/// parking the whole thread when mpv's 4 KB IPC buffer fills.
fn open_ipc(
    path: &str,
) -> std::io::Result<(Box<dyn Read + Send>, Box<dyn Write + Send>)> {
    #[cfg(unix)]
    {
        let stream = UnixStream::connect(path)?;
        let read = stream.try_clone()?;
        Ok((Box::new(read), Box::new(stream)))
    }
    #[cfg(windows)]
    {
        let (reader, writer) = windows_pipe::open(path)?;
        Ok((Box::new(reader), Box::new(writer)))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "mpv overlay IPC requires Unix sockets or Windows named pipes",
        ))
    }
}

/// Windows-specific named-pipe I/O with overlapped writes.
///
/// mpv's IPC server creates the pipe with a 4 KB buffer in each
/// direction (`input/ipc-win.c`: `bufsiz = 4096`). Under heavy decode
/// load mpv's per-client IPC thread is slow to drain, the buffer fills,
/// and a blocking `WriteFile` from our side parks the writer thread —
/// frames stop flowing until something (typically a seek) unblocks mpv.
///
/// The fix: open the pipe with `FILE_FLAG_OVERLAPPED` and bound each
/// write with a small timeout via `GetOverlappedResult`. A pending
/// write that doesn't complete in time is cancelled with `CancelIoEx`
/// and the frame is silently dropped — the next snapshot will overwrite
/// the mailbox slot anyway. The reader stays blocking (infinite wait)
/// so we keep draining mpv's replies the same way as before.
///
/// Both reader and writer must use overlapped I/O because a single
/// `FILE_FLAG_OVERLAPPED` handle rejects synchronous calls.
#[cfg(windows)]
mod windows_pipe {
    use std::io::{self, Read, Write};
    use std::os::windows::ffi::OsStrExt;
    use std::sync::Arc;
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE, HANDLE,
        INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, ReadFile, WriteFile,
    };
    use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
    use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

    /// Drop the frame if the underlying WriteFile is still pending after
    /// this many ms — picked well above a typical 4 KB-buffer drain at
    /// 30 Hz but short enough to keep the writer responsive.
    const WRITE_TIMEOUT_MS: u32 = 200;

    /// Owned overlapped HANDLE shared between the reader and writer
    /// halves; `CloseHandle` runs on Drop of the last `Arc`.
    struct OwnedHandle(HANDLE);
    unsafe impl Send for OwnedHandle {}
    unsafe impl Sync for OwnedHandle {}
    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
    }

    pub struct Reader {
        h: Arc<OwnedHandle>,
    }
    pub struct Writer {
        h: Arc<OwnedHandle>,
    }

    pub fn open(path: &str) -> io::Result<(Reader, Writer)> {
        let wide: Vec<u16> = std::ffi::OsStr::new(path)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let raw = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                std::ptr::null_mut(),
            )
        };
        if raw.is_null() || raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let h = Arc::new(OwnedHandle(raw));
        Ok((Reader { h: h.clone() }, Writer { h }))
    }

    /// RAII wrapper around a manual-reset event used by a single
    /// overlapped op. Closed on Drop.
    struct Event(HANDLE);
    impl Event {
        fn new() -> io::Result<Self> {
            let h = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
            if h.is_null() {
                Err(io::Error::last_os_error())
            } else {
                Ok(Self(h))
            }
        }
    }
    impl Drop for Event {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
    }

    impl Read for Reader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let event = Event::new()?;
            let mut ol: OVERLAPPED = unsafe { std::mem::zeroed() };
            ol.hEvent = event.0;
            let ok = unsafe {
                ReadFile(
                    self.h.0,
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                    std::ptr::null_mut(),
                    &mut ol,
                )
            };
            if ok == 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                    return Err(err);
                }
            }
            // Block until completion — we want the reader to behave just
            // like a normal blocking read so mpv's replies keep draining.
            let mut transferred: u32 = 0;
            let ok = unsafe { GetOverlappedResult(self.h.0, &ol, &mut transferred, 1) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(transferred as usize)
        }
    }

    impl Write for Writer {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let event = Event::new()?;
            let mut ol: OVERLAPPED = unsafe { std::mem::zeroed() };
            ol.hEvent = event.0;
            let ok = unsafe {
                WriteFile(
                    self.h.0,
                    buf.as_ptr(),
                    buf.len() as u32,
                    std::ptr::null_mut(),
                    &mut ol,
                )
            };
            if ok == 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                    return Err(err);
                }
            }
            // Bounded wait — if mpv's IPC buffer is full, give up rather
            // than block the writer thread indefinitely.
            let waited = unsafe { WaitForSingleObject(event.0, WRITE_TIMEOUT_MS) };
            if waited == WAIT_TIMEOUT {
                // The kernel hasn't drained anything yet; abort the
                // queued I/O so the handle is back to a clean state and
                // pretend the write succeeded. The mailbox semantics
                // make this safe: the next overlay snapshot will replace
                // whatever we just dropped.
                unsafe {
                    CancelIoEx(self.h.0, &ol);
                    GetOverlappedResult(self.h.0, &ol, &mut 0u32, 1);
                }
                return Ok(buf.len());
            } else if waited != WAIT_OBJECT_0 {
                return Err(io::Error::last_os_error());
            }
            let mut transferred: u32 = 0;
            let ok = unsafe { GetOverlappedResult(self.h.0, &ol, &mut transferred, 0) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            if (transferred as usize) != buf.len() {
                // Partial write would corrupt mpv's byte-mode parser —
                // surface the error so the caller drops the connection
                // and reconnects fresh.
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "partial overlapped write to mpv pipe",
                ));
            }
            Ok(transferred as usize)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct OverlayPrefs {
    pub enabled: bool,
    pub socket_path: String,
}

impl Default for OverlayPrefs {
    fn default() -> Self {
        Self {
            enabled: false,
            socket_path: DEFAULT_SOCKET_PATH.to_string(),
        }
    }
}

fn prefs_path(config_dir: &Path) -> PathBuf {
    config_dir.join(PREFS_FILENAME)
}

pub fn load_prefs(config_dir: &Path) -> OverlayPrefs {
    let Ok(data) = std::fs::read_to_string(prefs_path(config_dir)) else {
        return OverlayPrefs::default();
    };
    serde_json::from_str(&data).unwrap_or_default()
}

pub fn save_prefs(config_dir: &Path, prefs: &OverlayPrefs) -> Result<(), String> {
    std::fs::create_dir_all(config_dir).map_err(|e| e.to_string())?;
    let data = serde_json::to_string_pretty(prefs).map_err(|e| e.to_string())?;
    std::fs::write(prefs_path(config_dir), data).map_err(|e| e.to_string())
}

enum WriterMsg {
    /// Overlay snapshot — droppable. A new Frame *overwrites* any
    /// pending Frame already in the inbox so the writer thread always
    /// pulls the most recent state, never a backlog.
    ///
    /// Dead since the overlay moved in-process to orender (pulled over FFI);
    /// retained with the rest of the socket frame path pending its removal.
    #[allow(dead_code)]
    Frame(String),
    /// Trail prefs, reconnect re-push, etc. Must reach mpv; queued in
    /// FIFO order alongside frames.
    Control(String),
    Shutdown,
}

/// Single-producer-many-consumers-style mailbox used between the OSC
/// tick thread (frames) / Tauri command handlers (control) and the
/// writer thread that owns the pipe handle.
///
/// `Frame` messages are *latest-value*: a fresh push replaces any
/// already-pending Frame, so even if the writer is briefly blocked on a
/// slow mpv IPC the queue can't accumulate stale snapshots.
/// `Control` messages keep FIFO order — trail prefs in particular must
/// not be silently dropped or coalesced.
struct Inbox {
    queue: Mutex<Option<VecDeque<WriterMsg>>>, // `None` ≡ inbox closed
    cv: Condvar,
}

impl Inbox {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(Some(VecDeque::new())),
            cv: Condvar::new(),
        })
    }

    /// Replace any pending Frame with this one (latest-value),
    /// otherwise append. Returns `false` if the inbox is closed.
    #[allow(dead_code)] // dead since the overlay moved in-process; see WriterMsg::Frame
    fn push_frame(&self, line: String) -> bool {
        let mut g = self.queue.lock().unwrap();
        let Some(q) = g.as_mut() else { return false };
        for slot in q.iter_mut() {
            if let WriterMsg::Frame(s) = slot {
                *s = line;
                self.cv.notify_one();
                return true;
            }
        }
        q.push_back(WriterMsg::Frame(line));
        self.cv.notify_one();
        true
    }

    fn push_back(&self, msg: WriterMsg) -> bool {
        let mut g = self.queue.lock().unwrap();
        let Some(q) = g.as_mut() else { return false };
        q.push_back(msg);
        self.cv.notify_one();
        true
    }

    /// Block until a message is available; returns `None` when the
    /// inbox is closed and drained.
    fn pop(&self) -> Option<WriterMsg> {
        let mut g = self.queue.lock().unwrap();
        loop {
            if g.is_none() {
                return None;
            }
            if let Some(msg) = g.as_mut().unwrap().pop_front() {
                return Some(msg);
            }
            g = self.cv.wait(g).unwrap();
        }
    }

    /// Close the inbox so any sleeping writer wakes up and exits.
    fn close(&self) {
        let mut g = self.queue.lock().unwrap();
        *g = None;
        self.cv.notify_all();
    }
}

/// Holds the writer-thread mailbox. A `None` value means "not connected".
///
/// `reconnect_path` is the path of the last user-initiated connect. It is
/// kept across a transient connection loss (mpv restart) so the tick thread
/// can re-establish the link without bothering the user, and cleared only
/// on a user-initiated disconnect.
#[derive(Default)]
pub struct MpvOverlayState {
    inner: Mutex<Option<Arc<Inbox>>>,
    reconnect_path: Mutex<Option<String>>,
    #[allow(dead_code)] // read only by the dormant frame-push self-heal path
    last_reconnect_at: Mutex<Option<Instant>>,
    /// Last trail prefs pushed by JS. Re-sent on every successful
    /// (re)connect so a fresh mpv picks them up without needing JS to
    /// re-push.
    trail_prefs: Mutex<Option<TrailPrefs>>,
}

#[derive(Clone, Debug)]
pub struct TrailPrefs {
    pub enabled: bool,
    pub ttl_ms: u32,
    /// "diffuse" or "line"; anything else falls back to "line" lua-side.
    pub mode: String,
    /// Max XYZ displacement (normalised units) between two consecutive
    /// trail points before the connecting segment is treated as a
    /// teleport and skipped lua-side.
    pub teleport_threshold: f32,
}

impl MpvOverlayState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open the mpv IPC socket at `path`. Disconnects any prior session
    /// and stores the path so the tick thread can auto-reconnect if mpv
    /// later restarts.
    pub fn connect(&self, path: &str) -> Result<(), String> {
        *self.reconnect_path.lock().unwrap() = Some(path.to_string());
        self.connect_inner(path)
    }

    pub fn set_trail_prefs(&self, prefs: TrailPrefs) -> Result<(), String> {
        // Best-effort push to mpv. Ignore the error — if we're not
        // connected, the next successful (re)connect will resend the
        // stashed copy.
        let _ = self.send_trail_prefs_now(&prefs);
        *self.trail_prefs.lock().unwrap() = Some(prefs);
        Ok(())
    }

    fn send_trail_prefs_now(&self, prefs: &TrailPrefs) -> Result<(), String> {
        // Sanitise the mode to a known token to keep the lua parser strict.
        let mode = match prefs.mode.as_str() {
            "diffuse" => "diffuse",
            _ => "line",
        };
        // Clamp the threshold to the same range Studio uses, then format
        // with a few decimals — the lua parser is strict about the wire
        // format and won't accept scientific notation.
        let threshold = prefs.teleport_threshold.clamp(0.0, 4.0);
        let line = format!(
            r#"{{"command":["set_property","user-data/omniphony/overlay/trail-config","{}|{}|{}|{:.3}"]}}"#,
            if prefs.enabled { 1 } else { 0 },
            prefs.ttl_ms,
            mode,
            threshold
        );
        self.send_line(line)
    }

    fn connect_inner(&self, path: &str) -> Result<(), String> {
        self.drop_connection();

        let (mut read_handle, mut write_handle) =
            open_ipc(path).map_err(|e| format!("connect {path}: {e}"))?;
        // mpv writes a JSON response for every command we send. If we
        // don't read them, mpv's reply buffer fills (~25 s at 20 Hz) and
        // its main thread blocks trying to write the next reply — which
        // silently freezes the whole IPC. The dedicated reader thread
        // just drains.
        std::thread::Builder::new()
            .name("mpv-overlay-reader".into())
            .spawn(move || {
                let mut buf = [0u8; 4096];
                loop {
                    match read_handle.read(&mut buf) {
                        Ok(0) => break, // EOF — mpv closed the endpoint
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
            })
            .map_err(|e| format!("spawn reader thread: {e}"))?;
        let inbox = Inbox::new();
        let writer_inbox = inbox.clone();
        std::thread::Builder::new()
            .name("mpv-overlay-writer".into())
            .spawn(move || {
                while let Some(msg) = writer_inbox.pop() {
                    let line = match msg {
                        WriterMsg::Frame(s) | WriterMsg::Control(s) => s,
                        WriterMsg::Shutdown => break,
                    };
                    if write_handle.write_all(line.as_bytes()).is_err()
                        || write_handle.write_all(b"\n").is_err()
                    {
                        break;
                    }
                }
                // Make sure no producer keeps blocking on a closed pipe.
                writer_inbox.close();
            })
            .map_err(|e| format!("spawn writer thread: {e}"))?;
        *self.inner.lock().unwrap() = Some(inbox);
        // Re-push the last trail prefs so a fresh mpv (or one whose
        // user-data was wiped on restart) lines up with what Studio
        // thinks they are.
        let prefs_snapshot = self.trail_prefs.lock().unwrap().clone();
        if let Some(prefs) = prefs_snapshot {
            let _ = self.send_trail_prefs_now(&prefs);
        }
        Ok(())
    }

    /// User-initiated disconnect: close the writer thread AND wipe the
    /// stored path so the auto-reconnect loop stops trying.
    pub fn disconnect(&self) {
        *self.reconnect_path.lock().unwrap() = None;
        self.drop_connection();
    }

    /// Close the writer thread but keep `reconnect_path` so the tick
    /// thread can re-establish the link transparently when the other end
    /// comes back.
    fn drop_connection(&self) {
        if let Some(inbox) = self.inner.lock().unwrap().take() {
            // Queue an in-band shutdown so anything still pending gets
            // a chance to flush in FIFO order, then close so the writer
            // wakes immediately even if the queue was empty.
            let _ = inbox.push_back(WriterMsg::Shutdown);
            inbox.close();
        }
    }

    /// Attempt to reconnect to the stored path. Called by the tick thread
    /// when the connection has been lost but the user hasn't disabled the
    /// overlay. Rate-limited internally to avoid hammering the socket.
    #[allow(dead_code)] // dead since the overlay moved in-process; pending socket-path removal
    pub fn try_reconnect(&self) -> bool {
        if self.is_connected() {
            return false;
        }
        let path = match self.reconnect_path.lock().unwrap().clone() {
            Some(p) => p,
            None => return false,
        };
        let now = Instant::now();
        {
            let mut last = self.last_reconnect_at.lock().unwrap();
            if let Some(prev) = *last {
                if now.duration_since(prev) < Duration::from_secs(2) {
                    return false;
                }
            }
            *last = Some(now);
        }
        // `connect` would clobber `reconnect_path`; call the inner path
        // directly so a transient failure here does not erase it.
        self.connect_inner(&path).is_ok()
    }

    /// Push a control-class message (trail prefs, etc.) — queued FIFO,
    /// must-deliver. Drops silently if not connected. If the writer
    /// thread is gone (mpv closed the socket), we tear down the
    /// connection state so the tick thread stops queuing into a dead
    /// inbox.
    pub fn send_line(&self, line: String) -> Result<(), String> {
        let pushed = {
            let guard = self.inner.lock().unwrap();
            let Some(inbox) = guard.as_ref() else {
                return Err("not connected".into());
            };
            inbox.push_back(WriterMsg::Control(line))
        };
        if !pushed {
            self.drop_connection();
            return Err("writer thread gone".into());
        }
        Ok(())
    }

    pub fn is_connected(&self) -> bool {
        self.inner.lock().unwrap().is_some()
    }

    /// Send used by the OSC tick thread to push the latest overlay
    /// snapshot. Frames are *latest-value*: if a frame is already
    /// pending in the inbox we overwrite it, so a temporarily slow
    /// mpv-side reader can't bury the writer under a backlog. Pacing is
    /// up to the caller (driven by the renderer's metering rate).
    #[allow(dead_code)] // dead since the overlay moved in-process; pending socket-path removal
    pub fn try_send_throttled(&self, line: String) -> bool {
        let pushed = {
            let guard = self.inner.lock().unwrap();
            let Some(inbox) = guard.as_ref() else {
                return false;
            };
            inbox.push_frame(line)
        };
        if !pushed {
            self.drop_connection();
        }
        pushed
    }
}

pub type SharedOverlay = Arc<MpvOverlayState>;
