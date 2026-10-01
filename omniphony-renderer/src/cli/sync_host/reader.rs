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
//! The FIFO is opened read-write and kept open for the life of the reader.
//! The pipe therefore always has a writer (us), so reads block instead of
//! returning end-of-file between streams, and the player's `open` for
//! writing never meets a pipe without a reader — the race that otherwise
//! kills it with `SIGPIPE`. A stream ends when the pipe has been silent for
//! [`END_OF_STREAM_S`]; it starts with the next byte.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread;

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
    },
    /// The pipe went silent.
    End,
}

/// Where a chunk ends on an IEC 958 carrier's timeline.
#[derive(Debug, Clone, Copy)]
pub struct Transport {
    /// Carrier frames per second and channels (2-byte samples).
    pub rate: u32,
    pub channels: u32,
    /// Bytes captured in this epoch up to the end of the chunk, which ends
    /// at the chunk's `t`.
    pub bytes_end: u64,
}

/// Chunks the hand-off queue holds: at 64 KiB each, well over a second of
/// even the 3 MB/s HBR carrier.
const QUEUE_CHUNKS: usize = 1024;
const CHUNK_BYTES: usize = 64 * 1024;
/// Silence after which the stream is over (s).
pub const END_OF_STREAM_S: f64 = 0.5;
const POLL_MS: i32 = 50;

/// Counters the reader publishes.
#[derive(Default)]
pub struct ReaderStats {
    pub bytes_read: AtomicU64,
    pub chunks_dropped: AtomicU64,
}

/// Spawn the reader on the FIFO at `path` (created if missing) and return
/// the receiving end and its counters.
pub fn spawn_reader(
    path: PathBuf,
    drain_on_open: bool,
) -> std::io::Result<(Receiver<ReaderMsg>, Arc<ReaderStats>)> {
    let fd = open_fifo(&path)?;
    if drain_on_open {
        let drained = drain(fd);
        if drained > 0 {
            log::info!(
                "sync host: drained {drained} stale bytes from {}",
                path.display()
            );
        }
    }
    let (tx, rx) = sync_channel(QUEUE_CHUNKS);
    let stats = Arc::new(ReaderStats::default());
    let thread_stats = Arc::clone(&stats);
    thread::Builder::new()
        .name("sync-pipe-reader".into())
        .spawn(move || read_loop(fd, tx, &thread_stats))?;
    Ok((rx, stats))
}

fn open_fifo(path: &Path) -> std::io::Result<i32> {
    let c = CString::new(path.as_os_str().as_bytes())?;
    if !path.exists() {
        // SAFETY: valid NUL-terminated path.
        if unsafe { libc::mkfifo(c.as_ptr(), 0o666) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    // SAFETY: valid path; O_RDWR on a FIFO is defined on Linux and keeps a
    // writer attached so reads never see end-of-file.
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(fd)
}

/// Read whatever is buffered without blocking; returns the bytes discarded.
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

fn read_loop(fd: i32, tx: SyncSender<ReaderMsg>, stats: &ReaderStats) {
    let mut buf = vec![0u8; CHUNK_BYTES];
    let mut in_stream = false;
    let mut last_data = 0.0f64;
    loop {
        if !poll_readable(fd, POLL_MS) {
            if in_stream && reference_now_s() - last_data >= END_OF_STREAM_S {
                in_stream = false;
                if tx.send(ReaderMsg::End).is_err() {
                    break;
                }
            }
            continue;
        }
        // SAFETY: `buf` is valid for its length.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            continue;
        }
        let t = reference_now_s();
        last_data = t;
        if !in_stream {
            in_stream = true;
            if tx.send(ReaderMsg::Start).is_err() {
                break;
            }
        }
        let n = n as usize;
        stats.bytes_read.fetch_add(n as u64, Ordering::Relaxed);
        match tx.try_send(ReaderMsg::Chunk {
            t,
            bytes: buf[..n].to_vec(),
            transport: None,
        }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                let dropped = stats.chunks_dropped.fetch_add(1, Ordering::Relaxed) + 1;
                if dropped.is_power_of_two() {
                    log::warn!(
                        "sync host: decoder behind real time, {dropped} input chunks dropped"
                    );
                }
            }
            Err(TrySendError::Disconnected(_)) => break,
        }
    }
    // SAFETY: we own `fd`.
    unsafe {
        libc::close(fd);
    }
}
