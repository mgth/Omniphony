//! The capture side's clock, published to the output callback.
//!
//! The capture point (PipeWire capture callback, pipe reader) publishes the
//! latest `(time, frames received)` pair after each delivery, plus the
//! accounting offset (`inserted − dropped`). The output callback reads them
//! once per cycle without waiting.
//!
//! The pair is a seqlock: the single writer bumps the sequence to odd, writes,
//! bumps it to even; a reader retries a bounded number of times while the
//! sequence is odd or moved, then falls back to the last pair it read. The
//! output callback never blocks and never sees a torn pair.
//!
//! `received` is counted in the ring's frame index space: frame `n` received
//! here is the `n`-th frame the renderer will push into the output ring in
//! this epoch (the capture side adds the ring's base when an epoch starts).

use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering, fence};

use audio_sync::SourceObservation;

/// See the [module docs](self).
#[derive(Debug, Default)]
pub struct SourceTap {
    seq: AtomicU32,
    t_bits: AtomicU64,
    received: AtomicU64,
    offset: AtomicI64,
    breaks: AtomicU64,
}

const READ_ATTEMPTS: usize = 8;

impl SourceTap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Writer side: `received` frames had arrived at time `t_s` (reference
    /// clock, s). One writer only.
    pub fn publish(&self, t_s: f64, received: u64) {
        let s = self.seq.load(Ordering::Relaxed);
        self.seq.store(s.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        self.t_bits.store(t_s.to_bits(), Ordering::Relaxed);
        self.received.store(received, Ordering::Relaxed);
        self.seq.store(s.wrapping_add(2), Ordering::Release);
    }

    /// Writer side: add `delta` frames inserted (positive) or dropped
    /// (negative) between the capture point and the ring.
    pub fn account(&self, delta: i64) {
        self.offset.fetch_add(delta, Ordering::Relaxed);
    }

    /// Writer side: the source broke its phase (another stream started on
    /// the same capture, such as a track change), before the reading that
    /// carries the new stream is published.
    pub fn mark_break(&self) {
        self.breaks.fetch_add(1, Ordering::Release);
    }

    /// Reader side: breaks marked so far.
    pub fn breaks(&self) -> u64 {
        self.breaks.load(Ordering::Acquire)
    }

    /// Reader side: the latest pair, if one has been published and could be
    /// read consistently within a few attempts.
    pub fn latest(&self) -> Option<SourceObservation> {
        for _ in 0..READ_ATTEMPTS {
            let s1 = self.seq.load(Ordering::Acquire);
            if s1 == 0 {
                return None;
            }
            if s1 & 1 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let t = f64::from_bits(self.t_bits.load(Ordering::Relaxed));
            let received = self.received.load(Ordering::Relaxed);
            fence(Ordering::Acquire);
            if self.seq.load(Ordering::Relaxed) == s1 {
                return Some(SourceObservation {
                    t,
                    received: received as f64,
                });
            }
        }
        None
    }

    /// Reader side: the accounting offset (frames).
    pub fn offset(&self) -> f64 {
        self.offset.load(Ordering::Relaxed) as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn nothing_before_the_first_publish() {
        let tap = SourceTap::new();
        assert!(tap.latest().is_none());
        tap.publish(1.5, 960);
        let obs = tap.latest().unwrap();
        assert_eq!((obs.t, obs.received), (1.5, 960.0));
        tap.account(-40);
        tap.account(10);
        assert_eq!(tap.offset(), -30.0);
    }

    /// A reader racing the writer never sees a pair that was not published
    /// together.
    #[test]
    fn pairs_are_never_torn() {
        let tap = Arc::new(SourceTap::new());
        let writer = {
            let tap = Arc::clone(&tap);
            std::thread::spawn(move || {
                for n in 1..200_000u64 {
                    // t is always received / 1000.
                    tap.publish(n as f64 / 1000.0, n);
                }
            })
        };
        let mut seen = 0;
        while !writer.is_finished() {
            if let Some(obs) = tap.latest() {
                assert_eq!(obs.t, obs.received / 1000.0);
                seen += 1;
            }
        }
        writer.join().unwrap();
        assert!(seen > 0);
    }
}
