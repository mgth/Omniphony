//! The capture side of the pipe: a thread that only reads.
//!
//! In `follow` mode the writer (mpv `--ao=pcm`) runs on its own clock, and
//! the time its bytes arrive *is* the source clock as orender can see it. So
//! the reader does nothing but read, stamp each chunk on the reference clock,
//! and hand it on without ever waiting for downstream: a slow decoder must
//! not hold the pipe back, or the writer would be paced by orender again.
//! A full hand-off queue is a fault (the decoder cannot keep up with real
//! time): the chunk is dropped, counted and logged, never waited on.
//!
//! On Unix the FIFO is opened read-write and kept open for the life of the
//! reader. The pipe therefore always has a writer (us), so reads block
//! instead of returning end-of-file between streams, and the player's `open`
//! for writing never meets a pipe without a reader — the race that otherwise
//! kills it with `SIGPIPE`. On Windows the reader serves a named pipe
//! (`\\.\pipe\…`) for one writer after another, on one instance. Either
//! way a stream ends when the pipe has been silent for [`END_OF_STREAM_S`];
//! it starts with the next byte.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::thread;
use std::time::Duration;

use audio_output::sync_output::reference_now_s;

/// What the reader hands to the engine thread.
pub enum ReaderMsg {
    /// Bytes arrived after silence: a new epoch starts.
    Start,
    /// Bytes read at `t` (reference clock, s). `transport` is set when the
    /// capture knows the bytes' exact place on the carrier's timeline (the
    /// `own` sink); a pipe does not.
    Chunk {
        t: f64,
        bytes: Vec<u8>,
        transport: Option<Transport>,
        /// Bytes read just before these and not handed over (the host was
        /// behind): the time they spanned on the carrier passed all the
        /// same. Always 0 with a `transport`, which counts them itself.
        skipped: u64,
    },
    /// The pipe went silent.
    End,
}

/// Where a chunk ends on its carrier's timeline: an IEC 958 carrier, or the
/// `own` sink's linear PCM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transport {
    /// Carrier frames per second and channels.
    pub rate: u32,
    pub channels: u32,
    /// What the bytes are: an IEC 61937 stream in 2-byte samples, or
    /// linear PCM in 4-byte floats.
    pub format: CarrierFormat,
    /// Bytes captured in this epoch up to the end of the chunk, which ends
    /// at the chunk's `t`.
    pub bytes_end: u64,
}

/// What a carrier's samples are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CarrierFormat {
    /// An IEC 61937 stream on an IEC 958 carrier: 16-bit samples.
    #[default]
    Iec958,
    /// Linear PCM, 32-bit float, interleaved.
    PcmF32,
}

impl CarrierFormat {
    /// Bytes one sample of one channel takes.
    pub fn sample_bytes(self) -> u32 {
        match self {
            CarrierFormat::Iec958 => 2,
            CarrierFormat::PcmF32 => 4,
        }
    }
}

/// Chunks the hand-off queue holds: at 64 KiB each, well over a second of
/// even the 3 MB/s HBR carrier.
const QUEUE_CHUNKS: usize = 1024;
const CHUNK_BYTES: usize = 64 * 1024;
/// Silence after which the stream is over (s).
pub const END_OF_STREAM_S: f64 = 0.5;
const POLL_MS: u32 = 50;

/// Counters the reader publishes.
#[derive(Default)]
pub struct ReaderStats {
    pub bytes_read: AtomicU64,
    pub chunks_dropped: AtomicU64,
}

/// A running capture (the pipe reader or the `own` sink): the messages it
/// hands to the engine thread, and its counters.
///
/// Dropping it stops the capture's threads and waits for them, so the pipe
/// is closed and the sink is gone from the graph before a successor opens
/// its own (a restart from config).
pub struct Capture {
    /// `None` only while dropping: closed first, so a thread blocked on a
    /// full queue wakes up.
    rx: Option<Receiver<ReaderMsg>>,
    pub stats: Arc<ReaderStats>,
    stop: Arc<AtomicBool>,
    threads: Vec<thread::JoinHandle<()>>,
    /// The latency the capture tells its player (ns), for one that does
    /// (the `own` sink): written by the host, read by the capture.
    advertised_latency_ns: Arc<AtomicI64>,
    /// The source has no clock: the host paces it (see [`Capture::set_paced`]).
    paced: Arc<AtomicBool>,
    /// Chunks handed over and not taken yet: what a paced reader waits on.
    queued: Arc<AtomicUsize>,
}

impl Capture {
    /// Wrap a capture's receiving end. `stop` is the flag its threads poll.
    pub fn new(rx: Receiver<ReaderMsg>, stats: Arc<ReaderStats>, stop: Arc<AtomicBool>) -> Self {
        Self {
            rx: Some(rx),
            stats,
            stop,
            threads: Vec::new(),
            advertised_latency_ns: Arc::new(AtomicI64::new(0)),
            paced: Arc::new(AtomicBool::new(false)),
            queued: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Whether the host paces this source (`none`: a file, a writer faster
    /// than real time) instead of following it. A paced reader waits for the
    /// host to take what it read before it reads on, so the writer of a pipe
    /// is held back by the pipe; a followed one never waits, and drops what
    /// the host is too slow for.
    pub fn set_paced(&self, paced: bool) {
        self.paced.store(paced, Ordering::Relaxed);
    }

    /// The cell the capture reads the latency to advertise from.
    pub fn advertised_latency(&self) -> Arc<AtomicI64> {
        Arc::clone(&self.advertised_latency_ns)
    }

    /// Tell the player another latency: the target changed, or the servo
    /// raised it. A pipe has nobody to tell.
    pub fn advertise_latency_ms(&self, ms: f64) {
        self.advertised_latency_ns
            .store((ms * 1e6) as i64, Ordering::Relaxed);
    }

    /// A thread the capture owns, joined when it is dropped.
    pub fn own(&mut self, thread: thread::JoinHandle<()>) {
        self.threads.push(thread);
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<ReaderMsg, RecvTimeoutError> {
        let msg = match &self.rx {
            Some(rx) => rx.recv_timeout(timeout)?,
            None => return Err(RecvTimeoutError::Disconnected),
        };
        if matches!(msg, ReaderMsg::Chunk { .. }) {
            // One less waiting, never below none (a discard may have zeroed
            // the count under a chunk already on its way).
            let mut queued = self.queued.load(Ordering::Relaxed);
            while queued > 0 {
                match self.queued.compare_exchange_weak(
                    queued,
                    queued - 1,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(now) => queued = now,
                }
            }
        }
        Ok(msg)
    }

    /// Discard what is queued (standby: nothing is played, and the capture
    /// must not fill its queue).
    pub fn discard_pending(&self) {
        if let Some(rx) = &self.rx {
            while rx.try_recv().is_ok() {}
        }
        self.queued.store(0, Ordering::Relaxed);
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.rx = None;
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// What a reader reads, and with it how a stream ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadKind {
    /// A FIFO, kept open for writers to come and go: a stream ends when the
    /// pipe has been silent for [`END_OF_STREAM_S`], and another may follow.
    Fifo,
    /// A regular file or standard input: one stream, which ends at end of
    /// file, and the reader with it.
    ToEnd,
}

/// What the read loop reads from: bytes, with a wait that ends.
trait ByteSource: Send + 'static {
    /// Wait up to `timeout_ms` for bytes and read them into `buf`.
    fn read(&mut self, buf: &mut [u8], timeout_ms: u32) -> ReadOutcome;
}

enum ReadOutcome {
    Bytes(usize),
    /// Nothing within the wait.
    Idle,
    /// The end of what there is to read (a file's end).
    End,
}

/// Spawn the reader on the pipe at `path`: a FIFO, created if missing, or on
/// Windows a named pipe, served here.
pub fn spawn_reader(path: PathBuf, drain_on_open: bool) -> std::io::Result<Capture> {
    let source = platform::open_pipe(&path, drain_on_open)?;
    spawn_read_loop(source, ReadKind::Fifo, "sync-pipe-reader", true, false)
}

/// Spawn the reader on the regular file at `path`, or on standard input for
/// `-`: one stream, read to its end. Paced from the start: a file has no
/// clock.
pub fn spawn_file_reader(path: &Path) -> std::io::Result<Capture> {
    let (source, stoppable) = platform::open_file(path)?;
    spawn_read_loop(source, ReadKind::ToEnd, "sync-file-reader", stoppable, true)
}

/// `join`: whether the capture waits for the thread when it is dropped. A
/// read that cannot be woken (standard input, where it cannot be polled)
/// would hold the drop until the next byte: that thread is left to end on
/// its own.
///
/// `paced`: whether the host paces the source from its first byte. Said
/// before the thread starts: set after, the reader has already read ahead by
/// as much as it could in the meantime (megabytes of a file).
fn spawn_read_loop(
    source: Box<dyn ByteSource>,
    kind: ReadKind,
    name: &str,
    join: bool,
    paced: bool,
) -> std::io::Result<Capture> {
    let (tx, rx) = sync_channel(QUEUE_CHUNKS);
    let stats = Arc::new(ReaderStats::default());
    let stop = Arc::new(AtomicBool::new(false));
    let mut capture = Capture::new(rx, Arc::clone(&stats), Arc::clone(&stop));
    capture.set_paced(paced);
    let (paced, queued) = (Arc::clone(&capture.paced), Arc::clone(&capture.queued));
    let thread = thread::Builder::new().name(name.into()).spawn(move || {
        read_loop(
            source,
            kind,
            tx,
            ReadShared {
                stats: &stats,
                stop: &stop,
                paced: &paced,
                queued: &queued,
            },
        )
    })?;
    if join {
        capture.own(thread);
    }
    Ok(capture)
}

/// What the read loop shares with its [`Capture`].
struct ReadShared<'a> {
    stats: &'a ReaderStats,
    stop: &'a AtomicBool,
    paced: &'a AtomicBool,
    queued: &'a AtomicUsize,
}

/// Chunks a paced reader lets wait for the host before it stops reading:
/// enough for the host never to wait for the reader, and no more, so the
/// writer of a pipe is held back within a chunk or two of what plays.
const PACED_QUEUE_CHUNKS: usize = 2;

#[cfg(unix)]
mod platform {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    use super::{ByteSource, ReadOutcome};

    /// A descriptor the loop polls and reads, closed with it.
    struct FdSource(i32);

    impl ByteSource for FdSource {
        fn read(&mut self, buf: &mut [u8], timeout_ms: u32) -> ReadOutcome {
            if !poll_readable(self.0, timeout_ms as i32) {
                return ReadOutcome::Idle;
            }
            // SAFETY: `buf` is valid for its length.
            let n = unsafe { libc::read(self.0, buf.as_mut_ptr().cast(), buf.len()) };
            match n {
                0 => ReadOutcome::End,
                n if n > 0 => ReadOutcome::Bytes(n as usize),
                _ => ReadOutcome::Idle,
            }
        }
    }

    impl Drop for FdSource {
        fn drop(&mut self) {
            // SAFETY: we own the descriptor.
            unsafe {
                libc::close(self.0);
            }
        }
    }

    pub fn open_pipe(path: &Path, drain_on_open: bool) -> std::io::Result<Box<dyn ByteSource>> {
        let c = CString::new(path.as_os_str().as_bytes())?;
        if !path.exists() {
            // SAFETY: valid NUL-terminated path.
            if unsafe { libc::mkfifo(c.as_ptr(), 0o666) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        // SAFETY: valid path; O_RDWR on a FIFO keeps a writer attached so
        // reads never see end-of-file.
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if drain_on_open {
            let drained = drain(fd);
            if drained > 0 {
                log::info!(
                    "sync host: drained {drained} stale bytes from {}",
                    path.display()
                );
            }
        }
        Ok(Box::new(FdSource(fd)))
    }

    /// The source, and whether a read of it can be waited out (always here:
    /// it is polled).
    pub fn open_file(path: &Path) -> std::io::Result<(Box<dyn ByteSource>, bool)> {
        let fd = if path.as_os_str() == "-" {
            // A descriptor of our own: the source closes what it reads.
            // SAFETY: duplicating standard input.
            unsafe { libc::dup(libc::STDIN_FILENO) }
        } else {
            let c = CString::new(path.as_os_str().as_bytes())?;
            // SAFETY: valid NUL-terminated path.
            unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) }
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok((Box::new(FdSource(fd)), true))
    }

    /// Read whatever is buffered without blocking; returns the bytes
    /// discarded.
    fn drain(fd: i32) -> usize {
        let mut buf = [0u8; 16 * 1024];
        let mut total = 0;
        while poll_readable(fd, 0) {
            // SAFETY: `buf` is valid for its length.
            let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                break;
            }
            total += n as usize;
        }
        total
    }

    fn poll_readable(fd: i32, timeout_ms: i32) -> bool {
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        let r = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        r > 0 && pfd.revents & libc::POLLIN != 0
    }
}

#[cfg(windows)]
mod platform {
    use std::io::Read;
    use std::path::Path;

    use super::{ByteSource, CHUNK_BYTES, ReadOutcome};

    impl ByteSource for sys::input::NamedPipeSource {
        fn read(&mut self, buf: &mut [u8], timeout_ms: u32) -> ReadOutcome {
            match sys::input::NamedPipeSource::read(self, buf, timeout_ms) {
                Some(n) => ReadOutcome::Bytes(n),
                None => ReadOutcome::Idle,
            }
        }
    }

    /// A file or standard input, read as it comes: the read blocks.
    struct Blocking(Box<dyn Read + Send>);

    impl ByteSource for Blocking {
        fn read(&mut self, buf: &mut [u8], _timeout_ms: u32) -> ReadOutcome {
            match self.0.read(buf) {
                Ok(0) | Err(_) => ReadOutcome::End,
                Ok(n) => ReadOutcome::Bytes(n),
            }
        }
    }

    /// A named pipe is created here, with nothing in it yet: there is
    /// nothing stale to drain.
    pub fn open_pipe(path: &Path, _drain_on_open: bool) -> std::io::Result<Box<dyn ByteSource>> {
        let name = path.to_string_lossy();
        let source = sys::input::NamedPipeSource::create(&name, CHUNK_BYTES)
            .map_err(|e| std::io::Error::other(format!("{e:#}")))?;
        Ok(Box::new(source))
    }

    /// The source, and whether a read of it can be waited out: a file's
    /// always returns, standard input's only when something is written.
    pub fn open_file(path: &Path) -> std::io::Result<(Box<dyn ByteSource>, bool)> {
        if path.as_os_str() == "-" {
            Ok((Box::new(Blocking(Box::new(std::io::stdin()))), false))
        } else {
            let file = std::fs::File::open(path)?;
            Ok((Box::new(Blocking(Box::new(file))), true))
        }
    }
}

fn read_loop(
    mut source: Box<dyn ByteSource>,
    kind: ReadKind,
    tx: SyncSender<ReaderMsg>,
    shared: ReadShared<'_>,
) {
    let ReadShared {
        stats,
        stop,
        paced,
        queued,
    } = shared;
    let mut buf = vec![0u8; CHUNK_BYTES];
    let mut in_stream = false;
    // Bytes read and not handed over since the last chunk that was.
    let mut skipped = 0u64;
    let mut last_data = 0.0f64;
    while !stop.load(Ordering::Relaxed) {
        let paced_now = paced.load(Ordering::Relaxed);
        if paced_now && queued.load(Ordering::Relaxed) >= PACED_QUEUE_CHUNKS {
            // The host has not taken what was read: read no further. The
            // silence that ends a FIFO's stream is not counted meanwhile.
            last_data = reference_now_s();
            thread::sleep(Duration::from_millis(2));
            continue;
        }
        let n = match source.read(&mut buf, POLL_MS) {
            ReadOutcome::Bytes(n) => n,
            // End of file: the stream is over, and so is this reader. (A
            // file that held nothing was no stream at all.)
            ReadOutcome::End if kind == ReadKind::ToEnd => {
                if in_stream {
                    let _ = tx.send(ReaderMsg::End);
                }
                break;
            }
            ReadOutcome::Idle | ReadOutcome::End => {
                if kind == ReadKind::Fifo
                    && in_stream
                    && reference_now_s() - last_data >= END_OF_STREAM_S
                {
                    in_stream = false;
                    if tx.send(ReaderMsg::End).is_err() {
                        break;
                    }
                }
                continue;
            }
        };
        let t = reference_now_s();
        last_data = t;
        if !in_stream {
            in_stream = true;
            if tx.send(ReaderMsg::Start).is_err() {
                break;
            }
        }
        stats.bytes_read.fetch_add(n as u64, Ordering::Relaxed);
        let chunk = ReaderMsg::Chunk {
            t,
            bytes: buf[..n].to_vec(),
            transport: None,
            skipped: std::mem::take(&mut skipped),
        };
        queued.fetch_add(1, Ordering::Relaxed);
        if paced_now {
            // Nothing is dropped from a source the host paces.
            if tx.send(chunk).is_err() {
                break;
            }
            continue;
        }
        match tx.try_send(chunk) {
            Ok(()) => {}
            Err(TrySendError::Full(ReaderMsg::Chunk {
                bytes,
                skipped: before,
                ..
            })) => {
                // The next chunk that gets through says what went missing.
                skipped = before + bytes.len() as u64;
                queued.fetch_sub(1, Ordering::Relaxed);
                let dropped = stats.chunks_dropped.fetch_add(1, Ordering::Relaxed) + 1;
                if dropped.is_power_of_two() {
                    log::warn!(
                        "sync host: decoder behind real time, {dropped} input chunks dropped"
                    );
                }
            }
            Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A file of the test's own, removed when the guard goes.
    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn scratch_file(name: &str, bytes: &[u8]) -> Scratch {
        let path =
            std::env::temp_dir().join(format!("orender-sync-reader-{}-{name}", std::process::id()));
        std::fs::File::create(&path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
        Scratch(path)
    }

    fn next(capture: &Capture) -> Option<ReaderMsg> {
        capture.recv_timeout(Duration::from_secs(5)).ok()
    }

    /// A file is one stream: it starts, every byte comes once and in order,
    /// in chunks of the size a pipe is read in, it ends, and the reader is
    /// done.
    #[test]
    fn a_file_is_read_to_its_end_as_one_stream() {
        let content: Vec<u8> = (0..(CHUNK_BYTES * 2 + 1234))
            .map(|i| (i % 251) as u8)
            .collect();
        let file = scratch_file("whole.bin", &content);
        let capture = spawn_file_reader(&file.0).unwrap();
        assert!(matches!(next(&capture), Some(ReaderMsg::Start)));
        let mut read = Vec::new();
        let mut sizes = Vec::new();
        loop {
            match next(&capture) {
                Some(ReaderMsg::Chunk {
                    bytes, transport, ..
                }) => {
                    assert!(transport.is_none());
                    sizes.push(bytes.len());
                    read.extend_from_slice(&bytes);
                }
                Some(ReaderMsg::End) => break,
                other => panic!("unexpected {:?}", other.is_some()),
            }
        }
        assert_eq!(read, content);
        assert_eq!(sizes, [CHUNK_BYTES, CHUNK_BYTES, 1234]);
        assert!(
            matches!(
                capture.recv_timeout(Duration::from_secs(5)),
                Err(RecvTimeoutError::Disconnected)
            ),
            "the reader ends with its file"
        );
    }

    /// A paced reader reads no further than a chunk or two ahead of what the
    /// host has taken: the whole file is not read into the queue.
    #[test]
    fn a_paced_reader_waits_for_the_host() {
        let file = scratch_file("paced.bin", &vec![7u8; CHUNK_BYTES * 20]);
        let capture = spawn_file_reader(&file.0).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let read = capture.stats.bytes_read.load(Ordering::Relaxed) as usize;
        assert!(
            read <= CHUNK_BYTES * (PACED_QUEUE_CHUNKS + 1),
            "{read} bytes read ahead of a host that took nothing"
        );
        // Taking them lets it read on, to the end, with nothing dropped.
        let mut total = 0;
        while let Some(msg) = next(&capture) {
            match msg {
                ReaderMsg::Chunk { bytes, .. } => total += bytes.len(),
                ReaderMsg::End => break,
                ReaderMsg::Start => {}
            }
        }
        assert_eq!(total, CHUNK_BYTES * 20);
        assert_eq!(capture.stats.chunks_dropped.load(Ordering::Relaxed), 0);
    }

    /// A file that holds nothing is no stream: the reader just ends.
    #[test]
    fn an_empty_file_is_no_stream() {
        let file = scratch_file("empty.bin", &[]);
        let capture = spawn_file_reader(&file.0).unwrap();
        assert!(matches!(
            capture.recv_timeout(Duration::from_secs(5)),
            Err(RecvTimeoutError::Disconnected)
        ));
    }
}
