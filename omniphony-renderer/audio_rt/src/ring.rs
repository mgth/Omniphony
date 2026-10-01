//! Single-producer single-consumer ring of interleaved `f32` frames.
//!
//! The renderer thread pushes, the output callback reads. Each side owns one
//! monotonic counter of frames moved (`written`, `read`); the slot of frame
//! `n` is `n % capacity`. A side publishes its counter with `Release` after
//! copying and reads the other's with `Acquire` before, so the bytes a counter
//! covers are always visible to the side that reads it.
//!
//! Only whole frames move: a push that does not fit is cut at a frame
//! boundary and reports how much went in, so the channel interleaving can
//! never rotate (the old per-sample queue could, spike S4). The counters are
//! absolute for the life of the ring, which is what the latency servo wants:
//! `written()` is the end of the readable data and `read()` the resampler's
//! consumption front, both in frames.

use std::cell::UnsafeCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

struct Shared {
    buf: Box<[UnsafeCell<f32>]>,
    channels: usize,
    capacity: usize,
    written: AtomicU64,
    read: AtomicU64,
}

// SAFETY: the producer only writes slots outside `[read, written)` and the
// consumer only reads slots inside it; the counters, published with
// Release/Acquire, are what keeps the two regions apart.
unsafe impl Sync for Shared {}
unsafe impl Send for Shared {}

impl Shared {
    fn base(&self) -> *mut f32 {
        // `UnsafeCell<f32>` has the layout of `f32`.
        self.buf.as_ptr() as *mut f32
    }

    /// Copy `frames` frames between the ring starting at absolute frame `at`
    /// and `ext`, in the direction given, wrapping at the end.
    ///
    /// # Safety
    /// The caller owns `[at, at + frames)` for this direction (see `Sync`).
    unsafe fn copy(&self, at: u64, ext: *mut f32, frames: usize, into_ring: bool) {
        let c = self.channels;
        let start = (at % self.capacity as u64) as usize;
        let first = frames.min(self.capacity - start);
        let second = frames - first;
        // SAFETY: `start + first <= capacity`, `second < capacity`, and `ext`
        // holds `frames * c` samples (checked by the callers).
        unsafe {
            let ring_first = self.base().add(start * c);
            let ring_second = self.base();
            if into_ring {
                std::ptr::copy_nonoverlapping(ext, ring_first, first * c);
                std::ptr::copy_nonoverlapping(ext.add(first * c), ring_second, second * c);
            } else {
                std::ptr::copy_nonoverlapping(ring_first, ext, first * c);
                std::ptr::copy_nonoverlapping(ring_second, ext.add(first * c), second * c);
            }
        }
    }
}

/// Create a ring of `capacity_frames` frames of `channels` samples, and its
/// two ends.
pub fn frame_ring(channels: usize, capacity_frames: usize) -> (Producer, Consumer) {
    let channels = channels.max(1);
    let capacity = capacity_frames.max(1);
    let buf = (0..channels * capacity)
        .map(|_| UnsafeCell::new(0.0))
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let shared = Arc::new(Shared {
        buf,
        channels,
        capacity,
        written: AtomicU64::new(0),
        read: AtomicU64::new(0),
    });
    (
        Producer {
            shared: Arc::clone(&shared),
        },
        Consumer { shared },
    )
}

/// The writing end. Not `Clone`: there is exactly one producer.
pub struct Producer {
    shared: Arc<Shared>,
}

impl Producer {
    pub fn channels(&self) -> usize {
        self.shared.channels
    }

    pub fn capacity(&self) -> usize {
        self.shared.capacity
    }

    /// Frames pushed so far, ever.
    pub fn written(&self) -> u64 {
        self.shared.written.load(Ordering::Relaxed)
    }

    /// Frames that fit right now.
    pub fn free(&self) -> usize {
        let read = self.shared.read.load(Ordering::Acquire);
        self.shared.capacity - (self.written() - read) as usize
    }

    /// Push the whole frames of `samples` (a trailing partial frame is
    /// ignored) that fit, and return how many frames went in. Never blocks.
    pub fn push(&mut self, samples: &[f32]) -> usize {
        let frames = (samples.len() / self.shared.channels).min(self.free());
        if frames == 0 {
            return 0;
        }
        let at = self.written();
        // SAFETY: `[at, at + frames)` is free (`frames <= free()`), and
        // `samples` holds at least `frames * channels` samples.
        unsafe {
            self.shared
                .copy(at, samples.as_ptr() as *mut f32, frames, true);
        }
        self.shared
            .written
            .store(at + frames as u64, Ordering::Release);
        frames
    }
}

/// The reading end. Not `Clone`: there is exactly one consumer.
pub struct Consumer {
    shared: Arc<Shared>,
}

impl Consumer {
    pub fn channels(&self) -> usize {
        self.shared.channels
    }

    pub fn capacity(&self) -> usize {
        self.shared.capacity
    }

    /// Frames consumed (read or discarded) so far, ever.
    pub fn read(&self) -> u64 {
        self.shared.read.load(Ordering::Relaxed)
    }

    /// Frames pushed so far, ever: the end of the readable data.
    pub fn written(&self) -> u64 {
        self.shared.written.load(Ordering::Acquire)
    }

    /// Frames readable right now.
    pub fn available(&self) -> usize {
        (self.written() - self.read()) as usize
    }

    /// Fill `dst` with exactly `dst.len() / channels` frames, or read nothing
    /// and return `false` if fewer are available.
    pub fn read_exact(&mut self, dst: &mut [f32]) -> bool {
        let frames = dst.len() / self.shared.channels;
        if frames > self.available() {
            return false;
        }
        if frames == 0 {
            return true;
        }
        let at = self.read();
        // SAFETY: `[at, at + frames)` is readable (`frames <= available()`),
        // and `dst` holds at least `frames * channels` samples.
        unsafe {
            self.shared.copy(at, dst.as_mut_ptr(), frames, false);
        }
        self.shared
            .read
            .store(at + frames as u64, Ordering::Release);
        true
    }

    /// Drop up to `frames` frames unread; returns how many were dropped.
    pub fn discard(&mut self, frames: usize) -> usize {
        let frames = frames.min(self.available());
        let at = self.read();
        self.shared
            .read
            .store(at + frames as u64, Ordering::Release);
        frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_whole_frames_and_wraps() {
        let (mut tx, mut rx) = frame_ring(2, 4);
        assert_eq!(tx.push(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]), 3);
        let mut out = [0.0; 4];
        assert!(rx.read_exact(&mut out));
        assert_eq!(out, [1.0, 2.0, 3.0, 4.0]);
        // Wraps: slots 3, 0, 1.
        assert_eq!(tx.push(&[7.0, 8.0, 9.0, 10.0, 11.0, 12.0]), 3);
        let mut out = [0.0; 8];
        assert!(rx.read_exact(&mut out));
        assert_eq!(out, [5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0]);
        assert_eq!((rx.read(), rx.written()), (6, 6));
    }

    #[test]
    fn a_push_that_does_not_fit_is_cut_at_a_frame() {
        let (mut tx, rx) = frame_ring(3, 2);
        // 3 frames offered, 2 fit, and a trailing partial frame is ignored.
        assert_eq!(tx.push(&[0.0; 3 * 3 + 2]), 2);
        assert_eq!(tx.push(&[0.0; 3]), 0);
        assert_eq!(rx.available(), 2);
    }

    #[test]
    fn a_short_read_reads_nothing() {
        let (mut tx, mut rx) = frame_ring(2, 8);
        tx.push(&[1.0; 4]);
        let mut out = [9.0; 6];
        assert!(!rx.read_exact(&mut out));
        assert_eq!(out, [9.0; 6]);
        assert_eq!(rx.read(), 0);
    }

    #[test]
    fn discard_is_bounded_by_what_is_there() {
        let (mut tx, mut rx) = frame_ring(1, 8);
        tx.push(&[1.0, 2.0, 3.0]);
        assert_eq!(rx.discard(5), 3);
        assert_eq!(rx.available(), 0);
    }

    /// Two threads, odd chunk sizes, many wraps: every frame arrives once, in
    /// order, with its channels in place.
    #[test]
    fn concurrent_transfer_is_lossless_and_ordered() {
        const CH: usize = 3;
        const FRAMES: u64 = 200_000;
        let (mut tx, mut rx) = frame_ring(CH, 1000);
        let producer = std::thread::spawn(move || {
            let mut next = 0u64;
            let mut chunk = Vec::new();
            while next < FRAMES {
                let n = (1 + (next % 97)) as usize;
                chunk.clear();
                for f in next..(next + n as u64).min(FRAMES) {
                    for c in 0..CH {
                        chunk.push((f * 4 + c as u64) as f32);
                    }
                }
                let pushed = tx.push(&chunk);
                next += pushed as u64;
                if pushed == 0 {
                    std::thread::yield_now();
                }
            }
        });
        let mut expect = 0u64;
        let mut buf = vec![0.0f32; 64 * CH];
        while expect < FRAMES {
            let n = (1 + (expect % 61) as usize).min(64);
            let n = n.min((FRAMES - expect) as usize);
            if rx.read_exact(&mut buf[..n * CH]) {
                for frame in buf[..n * CH].chunks_exact(CH) {
                    for (c, &v) in frame.iter().enumerate() {
                        assert_eq!(v, (expect * 4 + c as u64) as f32);
                    }
                    expect += 1;
                }
            } else {
                std::thread::yield_now();
            }
        }
        producer.join().unwrap();
    }
}
