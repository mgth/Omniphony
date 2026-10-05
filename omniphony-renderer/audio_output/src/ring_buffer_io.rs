//! The sample ring between the renderer and the device callback.
//!
//! One thread writes interleaved samples, one reads them, and neither end
//! waits for the other. The ring is `rtrb`'s single-producer single-consumer
//! buffer: each end owns its index and reads the other's with one atomic
//! load, so a writer stopped in the middle of a block looks like a ring that
//! does not hold that block yet, and a reader stopped in the middle of one
//! looks like a ring that still holds it. Samples move in blocks: a copy (two
//! where the block wraps around the end of the ring) and one index store,
//! whatever the block size.
//!
//! A general-purpose queue does not promise that. `crossbeam`'s
//! `ArrayQueue::pop` spins while a `push` is in progress on the slot at its
//! head, so with the ring otherwise empty the device callback waited for the
//! renderer thread, a normal-priority one, to be scheduled again; and every
//! sample cost several atomic operations on each side.
//!
//! The two ends are [`RingWriter`] and [`RingReader`]. Neither can be cloned
//! or shared, so the single producer and the single consumer are properties
//! of the types. Whoever holds neither end and still needs the level (a flush
//! waiting for the device to play the ring out) asks a [`RingMonitor`].
//!
//! `tests/realtime_callbacks.rs` scans this module: nothing here may lock,
//! log or format.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// Capacity of the renderer → device ring, in interleaved samples: 4 s of
/// 16 channels at 48 kHz. Shared by every realtime backend; the back-pressure
/// threshold (`max_buffer_fill`) keeps the working fill far below it.
pub const OUTPUT_RING_CAPACITY: usize = 48000 * 16 * 4;

/// A value on a cache line of its own, so that the end that stores it does
/// not take the line the other end is storing to.
#[repr(align(128))]
struct OwnLine<T>(T);

/// What the two ends publish for a [`RingMonitor`], and what it asks of them.
struct Shared {
    capacity: usize,
    /// Samples pushed since the start, wrapping. Stored by the writer alone,
    /// before the samples become readable.
    written: OwnLine<AtomicUsize>,
    /// Samples taken since the start, wrapping. Stored by the reader alone,
    /// once their slots are free again.
    taken: OwnLine<AtomicUsize>,
    /// How many of the oldest samples the reader is asked to throw away at
    /// its next turn. Read by the reader once per callback.
    discard_requested: AtomicUsize,
}

/// A ring of `capacity` interleaved samples: the end that writes it and the
/// end that reads it.
pub fn sample_ring(capacity: usize) -> (RingWriter, RingReader) {
    let (producer, consumer) = rtrb::RingBuffer::new(capacity);
    let shared = Arc::new(Shared {
        capacity,
        written: OwnLine(AtomicUsize::new(0)),
        taken: OwnLine(AtomicUsize::new(0)),
        discard_requested: AtomicUsize::new(0),
    });
    let writer = RingWriter {
        producer,
        shared: Arc::clone(&shared),
        written: 0,
    };
    let reader = RingReader {
        consumer,
        shared,
        taken: 0,
    };
    (writer, reader)
}

/// The writing end of a sample ring. One per ring, owned by one thread at a
/// time.
pub struct RingWriter {
    producer: rtrb::Producer<f32>,
    shared: Arc<Shared>,
    /// This end's copy of `Shared::written`.
    written: usize,
}

impl RingWriter {
    /// Samples queued, as this end sees them: an upper bound, since the
    /// reader may be taking some right now.
    pub fn fill(&self) -> usize {
        self.shared.capacity - self.producer.slots()
    }

    /// A handle on this ring's level for a thread that holds neither end.
    pub fn monitor(&self) -> RingMonitor {
        RingMonitor {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Push the start of `samples`, as much of it as the ring has room for,
    /// and return how many samples that was. Never waits.
    pub fn push_slice(&mut self, samples: &[f32]) -> usize {
        self.push_slice_below(samples, usize::MAX)
    }

    /// Push the start of `samples` without taking the fill above `max_fill`
    /// (or the capacity), and return how many samples that was. Never waits.
    pub fn push_slice_below(&mut self, samples: &[f32], max_fill: usize) -> usize {
        let free = self.producer.slots();
        let fill = self.shared.capacity - free;
        // `max_fill - fill` cannot exceed `free` once `max_fill` is capped.
        let room = max_fill.min(self.shared.capacity).saturating_sub(fill);
        let count = samples.len().min(room);
        if count == 0 {
            return 0;
        }
        self.announce(count);
        // The free slots were counted above and only the reader changes that
        // number, upwards: all of `count` goes in.
        let (pushed, _) = self.producer.push_partial_slice(&samples[..count]);
        debug_assert_eq!(pushed.len(), count);
        count
    }

    /// Push `count` samples of silence, as many of them as the ring has room
    /// for, and return how many that was. Never waits.
    pub fn push_silence(&mut self, count: usize) -> usize {
        let count = count.min(self.producer.slots());
        if count == 0 {
            return 0;
        }
        self.announce(count);
        // `write_chunk` hands the slots over already holding 0.0.
        if let Ok(chunk) = self.producer.write_chunk(count) {
            chunk.commit_all();
        }
        count
    }

    /// Count `count` samples as written, before they become readable: the
    /// reader must never be seen ahead of the writer by a [`RingMonitor`].
    fn announce(&mut self, count: usize) {
        self.written = self.written.wrapping_add(count);
        self.shared.written.0.store(self.written, Ordering::Release);
    }
}

/// The reading end of a sample ring. One per ring, owned by one thread at a
/// time — the device callback, for the output ring.
pub struct RingReader {
    consumer: rtrb::Consumer<f32>,
    shared: Arc<Shared>,
    /// This end's copy of `Shared::taken`.
    taken: usize,
}

impl RingReader {
    /// Samples ready to be read: a lower bound, since the writer may be
    /// adding some right now.
    pub fn available(&self) -> usize {
        self.consumer.slots()
    }

    /// Move the oldest samples into the start of `dest`, as many as the ring
    /// holds and `dest` takes, and return how many that was. Never waits.
    pub fn pop_slice(&mut self, dest: &mut [f32]) -> usize {
        let (popped, _) = self.consumer.pop_partial_slice(dest);
        let count = popped.len();
        if count > 0 {
            self.account(count);
        }
        count
    }

    /// Take the `count` oldest samples, or as many as the ring holds, and
    /// show them to `visit` in order, in one or two blocks (two when they
    /// wrap around the end of the ring). Returns how many samples that was.
    /// Never waits.
    pub fn pop_with(&mut self, count: usize, mut visit: impl FnMut(&[f32])) -> usize {
        let count = count.min(self.consumer.slots());
        if count == 0 {
            return 0;
        }
        let Ok(chunk) = self.consumer.read_chunk(count) else {
            return 0;
        };
        let (first, second) = chunk.as_slices();
        visit(first);
        if !second.is_empty() {
            visit(second);
        }
        chunk.commit_all();
        self.account(count);
        count
    }

    /// Throw away the `count` oldest samples, or as many as the ring holds,
    /// and return how many that was. Never waits.
    pub fn discard(&mut self, count: usize) -> usize {
        let count = count.min(self.consumer.slots());
        if count == 0 {
            return 0;
        }
        if let Ok(chunk) = self.consumer.read_chunk(count) {
            chunk.commit_all();
        }
        self.account(count);
        count
    }

    /// Throw away what a [`RingMonitor`] asked to have thrown away, and
    /// return how many samples that was. The device callback calls it once,
    /// at its top; without a request it is one atomic load.
    pub fn apply_requested_discard(&mut self) -> usize {
        if self.shared.discard_requested.load(Ordering::Relaxed) == 0 {
            return 0;
        }
        let requested = self.shared.discard_requested.swap(0, Ordering::Relaxed);
        self.discard(requested)
    }

    /// Count `count` samples as taken, once their slots are free again.
    fn account(&mut self, count: usize) {
        self.taken = self.taken.wrapping_add(count);
        self.shared.taken.0.store(self.taken, Ordering::Release);
    }
}

/// A ring's level, for a thread that holds neither of its ends, and the one
/// thing such a thread may ask of the ring. Cloned freely.
#[derive(Clone)]
pub struct RingMonitor {
    shared: Arc<Shared>,
}

impl RingMonitor {
    /// Samples queued. Exact when both ends are at rest; while they move it
    /// errs high, by a block being written or one just read.
    pub fn fill(&self) -> usize {
        // In this order: the writer counts a block before the reader can take
        // it, so what the writer has counted by now covers everything the
        // reader had taken a moment ago.
        let taken = self.shared.taken.0.load(Ordering::Acquire);
        let written = self.shared.written.0.load(Ordering::Acquire);
        written.wrapping_sub(taken).min(self.shared.capacity)
    }

    /// Ask the reader to throw away the `samples` oldest samples at its next
    /// turn. The ring is the reader's to empty: a second thread popping it
    /// would be a second consumer.
    pub fn request_discard(&self, samples: usize) {
        self.shared
            .discard_requested
            .store(samples, Ordering::Relaxed);
    }
}

pub struct WriteSamplesReport {
    pub pushed_samples: usize,
    pub wait_count: u32,
    pub timed_out: bool,
}

/// Push `samples`, keeping the ring's fill at or below `max_buffer_fill`:
/// when there is no room below it, sleep `sleep_ms` and try again, and give
/// up after `timeout_waits` such waits. Blocking, for the renderer thread.
pub fn push_samples_with_backpressure(
    ring: &mut RingWriter,
    samples: &[f32],
    max_buffer_fill: usize,
    sleep_ms: u64,
    timeout_waits: u32,
) -> WriteSamplesReport {
    let mut sample_idx = 0usize;
    let mut wait_count = 0u32;

    while sample_idx < samples.len() {
        let pushed = ring.push_slice_below(&samples[sample_idx..], max_buffer_fill);
        if pushed == 0 {
            wait_count = wait_count.saturating_add(1);
            thread::sleep(Duration::from_millis(sleep_ms));
            if wait_count > timeout_waits {
                return WriteSamplesReport {
                    pushed_samples: sample_idx,
                    wait_count,
                    timed_out: true,
                };
            }
            continue;
        }
        sample_idx += pushed;
    }

    WriteSamplesReport {
        pushed_samples: sample_idx,
        wait_count,
        timed_out: false,
    }
}

/// Non-blocking variant of [`push_samples_with_backpressure`]: pushes as many
/// samples as fit below `max_buffer_fill` (and as the ring physically
/// accepts), then drops the remainder immediately instead of waiting for the
/// consumer to drain. Used when back-pressure is disabled, so the producer is
/// never throttled by the output buffer level.
pub fn push_samples_drop_overflow(
    ring: &mut RingWriter,
    samples: &[f32],
    max_buffer_fill: usize,
) -> WriteSamplesReport {
    WriteSamplesReport {
        pushed_samples: ring.push_slice_below(samples, max_buffer_fill),
        wait_count: 0,
        timed_out: false,
    }
}

pub struct FlushReport {
    pub timed_out: bool,
    pub stalled: bool,
    pub remaining_samples: usize,
}

/// Wait for the reader to play the ring out. After `timeout`, or once the
/// level has not gone down for `stall_timeout`, give up: what is left is no
/// longer wanted, and the reader is asked to throw it away at its next turn.
/// Blocking, for the renderer thread.
pub fn flush_ring_buffer(
    ring: &RingMonitor,
    timeout: Duration,
    poll_interval: Duration,
    stall_timeout: Option<Duration>,
) -> FlushReport {
    let start = Instant::now();
    let mut last_level = ring.fill();
    let mut last_change = start;

    while ring.fill() > 0 {
        if start.elapsed() > timeout {
            let remaining = ring.fill();
            ring.request_discard(remaining);
            return FlushReport {
                timed_out: true,
                stalled: false,
                remaining_samples: remaining,
            };
        }

        thread::sleep(poll_interval);
        let current = ring.fill();
        if current < last_level {
            last_level = current;
            last_change = Instant::now();
        } else if let Some(stall_timeout) = stall_timeout {
            if last_change.elapsed() > stall_timeout {
                ring.request_discard(current);
                return FlushReport {
                    timed_out: false,
                    stalled: true,
                    remaining_samples: current,
                };
            }
        }
    }

    FlushReport {
        timed_out: false,
        stalled: false,
        remaining_samples: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pop_all(reader: &mut RingReader) -> Vec<f32> {
        let mut out = vec![0.0; reader.available()];
        let count = reader.pop_slice(&mut out);
        out.truncate(count);
        out
    }

    #[test]
    fn samples_come_out_in_the_order_they_went_in() {
        let (mut writer, mut reader) = sample_ring(8);
        assert_eq!(writer.push_slice(&[1.0, 2.0, 3.0]), 3);
        assert_eq!(writer.fill(), 3);
        assert_eq!(reader.available(), 3);
        let mut dest = [0.0; 2];
        assert_eq!(reader.pop_slice(&mut dest), 2);
        assert_eq!(dest, [1.0, 2.0]);
        assert_eq!(pop_all(&mut reader), [3.0]);
        assert_eq!(writer.fill(), 0);
    }

    /// A block that wraps around the end of the ring is still one block to
    /// the caller, on both sides.
    #[test]
    fn a_block_wraps_around_the_end_of_the_ring() {
        let (mut writer, mut reader) = sample_ring(4);
        writer.push_slice(&[1.0, 2.0, 3.0]);
        reader.discard(3);
        // Three free slots at the end and start of the storage: 3, 0, 1.
        assert_eq!(writer.push_slice(&[4.0, 5.0, 6.0]), 3);
        let mut seen = Vec::new();
        let mut blocks = 0;
        assert_eq!(
            reader.pop_with(3, |block| {
                seen.extend_from_slice(block);
                blocks += 1;
            }),
            3
        );
        assert_eq!(seen, [4.0, 5.0, 6.0]);
        assert_eq!(blocks, 2, "one block up to the end, one from the start");

        writer.push_slice(&[7.0, 8.0, 9.0]);
        assert_eq!(pop_all(&mut reader), [7.0, 8.0, 9.0]);
    }

    #[test]
    fn a_full_ring_takes_what_fits_and_says_so() {
        let (mut writer, mut reader) = sample_ring(4);
        assert_eq!(writer.push_slice(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]), 4);
        assert_eq!(writer.push_slice(&[7.0]), 0);
        assert_eq!(writer.push_silence(3), 0);
        assert_eq!(pop_all(&mut reader), [1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn an_empty_ring_gives_nothing_and_says_so() {
        let (_writer, mut reader) = sample_ring(4);
        let mut dest = [9.0; 4];
        assert_eq!(reader.pop_slice(&mut dest), 0);
        assert_eq!(dest, [9.0; 4], "nothing written when the ring is empty");
        assert_eq!(reader.discard(2), 0);
        assert_eq!(reader.pop_with(2, |_| panic!("nothing to show")), 0);
    }

    #[test]
    fn silence_is_pushed_as_zeros() {
        let (mut writer, mut reader) = sample_ring(8);
        writer.push_slice(&[1.0]);
        assert_eq!(writer.push_silence(3), 3);
        assert_eq!(pop_all(&mut reader), [1.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn a_push_stays_below_the_requested_fill() {
        let (mut writer, mut reader) = sample_ring(8);
        assert_eq!(writer.push_slice_below(&[1.0; 6], 4), 4);
        assert_eq!(writer.push_slice_below(&[1.0; 6], 4), 0);
        reader.discard(3);
        assert_eq!(writer.push_slice_below(&[1.0; 6], 4), 3);
        // A threshold above the capacity is the capacity.
        assert_eq!(writer.push_slice_below(&[1.0; 6], 100), 4);
        assert_eq!(writer.fill(), 8);
    }

    #[test]
    fn discard_caps_at_what_the_ring_holds() {
        let (mut writer, mut reader) = sample_ring(8);
        writer.push_slice(&[0.0, 1.0, 2.0, 3.0, 4.0]);
        // Requesting more than present drains everything and reports the real count.
        assert_eq!(reader.discard(100), 5);
        assert_eq!(reader.available(), 0);
    }

    #[test]
    fn discard_drops_the_oldest_first() {
        let (mut writer, mut reader) = sample_ring(8);
        writer.push_slice(&[0.0, 1.0, 2.0, 3.0, 4.0]);
        assert_eq!(reader.discard(3), 3);
        // FIFO: the three oldest (0,1,2) are gone, 3.0 is now at the front.
        assert_eq!(pop_all(&mut reader), [3.0, 4.0]);
    }

    /// The case a general-purpose queue gets wrong on the reading side: the
    /// writer has been preempted in the middle of a block, its slots claimed
    /// and not yet handed over. The device callback must come back at once
    /// with what was complete before, not wait for the writer to be scheduled
    /// again.
    #[test]
    fn a_read_does_not_wait_for_a_writer_stopped_mid_block() {
        let (mut writer, mut reader) = sample_ring(8);
        writer.push_slice(&[1.0, 2.0]);

        // The writer, held where a preempted thread can be.
        let mut writing = writer.producer.write_chunk(4).unwrap();
        writing.as_mut_slices().0.fill(7.0);

        assert_eq!(reader.available(), 2);
        let mut dest = [0.0; 8];
        assert_eq!(reader.pop_slice(&mut dest), 2, "what was complete");
        assert_eq!(dest[..2], [1.0, 2.0]);
        // Empty now, with the block still in progress: each of these returns.
        assert_eq!(reader.available(), 0);
        assert_eq!(reader.pop_slice(&mut dest), 0);
        assert_eq!(reader.discard(4), 0);
        assert_eq!(reader.pop_with(4, |_| panic!("nothing to show")), 0);

        // It resumes and hands the block over: the next read has it.
        writing.commit_all();
        assert_eq!(reader.pop_slice(&mut dest), 4);
        assert_eq!(dest[..4], [7.0; 4]);
    }

    /// The mirror image on the writing side: the ring is full and the reader
    /// has been preempted in the middle of a block, read but its slots not
    /// released yet. The push must come back at once with nothing pushed.
    #[test]
    fn a_write_does_not_wait_for_a_reader_stopped_mid_block() {
        let (mut writer, mut reader) = sample_ring(4);
        assert_eq!(writer.push_slice(&[1.0, 2.0, 3.0, 4.0]), 4);

        // The reader, held where a preempted thread can be.
        let reading = reader.consumer.read_chunk(3).unwrap();
        assert_eq!(reading.as_slices().0, [1.0, 2.0, 3.0]);

        assert_eq!(writer.fill(), 4);
        assert_eq!(writer.push_slice(&[5.0, 6.0]), 0, "no free slot yet");
        assert_eq!(writer.push_silence(2), 0);
        let report = push_samples_drop_overflow(&mut writer, &[5.0, 6.0], 4);
        assert_eq!(report.pushed_samples, 0);

        // It resumes and releases the slots: the next push fits.
        reading.commit_all();
        assert_eq!(writer.push_slice(&[5.0, 6.0]), 2);
        assert_eq!(pop_all(&mut reader), [4.0, 5.0, 6.0]);
    }

    /// The level a third thread reads follows both ends, and never shows the
    /// reader ahead of the writer.
    #[test]
    fn the_monitor_follows_both_ends() {
        let (mut writer, mut reader) = sample_ring(8);
        let monitor = writer.monitor();
        assert_eq!(monitor.fill(), 0);
        writer.push_slice(&[1.0; 5]);
        assert_eq!(monitor.fill(), 5);
        reader.discard(2);
        assert_eq!(monitor.fill(), 3);
        writer.push_silence(4);
        assert_eq!(monitor.fill(), 7);
        let mut dest = [0.0; 8];
        reader.pop_slice(&mut dest);
        assert_eq!(monitor.fill(), 0);
    }

    /// The counters behind the monitor wrap; the level does not notice.
    #[test]
    fn the_monitor_survives_its_counters_wrapping() {
        let (mut writer, mut reader) = sample_ring(8);
        let start = usize::MAX - 2;
        writer.written = start;
        writer.shared.written.0.store(start, Ordering::Relaxed);
        reader.taken = start;
        reader.shared.taken.0.store(start, Ordering::Relaxed);
        let monitor = writer.monitor();

        writer.push_slice(&[1.0; 6]);
        assert_eq!(monitor.fill(), 6);
        reader.discard(4);
        assert_eq!(monitor.fill(), 2);
    }

    /// Nothing is thrown away by the thread that asks: the request waits for
    /// the reader, which honours it once.
    #[test]
    fn a_requested_discard_is_done_by_the_reader_once() {
        let (mut writer, mut reader) = sample_ring(8);
        let monitor = writer.monitor();
        writer.push_slice(&[1.0, 2.0, 3.0]);
        assert_eq!(reader.apply_requested_discard(), 0, "nothing asked");

        monitor.request_discard(2);
        assert_eq!(monitor.fill(), 3, "the requester must not consume");
        assert_eq!(reader.apply_requested_discard(), 2);
        assert_eq!(reader.apply_requested_discard(), 0, "request consumed");
        assert_eq!(pop_all(&mut reader), [3.0]);
    }

    #[test]
    fn backpressure_pushes_everything_once_the_reader_makes_room() {
        let (mut writer, mut reader) = sample_ring(64);
        let samples: Vec<f32> = (0..40).map(|i| i as f32).collect();
        let reading = thread::spawn(move || {
            let mut seen = Vec::new();
            let mut dest = [0.0; 8];
            while seen.len() < 40 {
                let count = reader.pop_slice(&mut dest);
                seen.extend_from_slice(&dest[..count]);
                thread::sleep(Duration::from_millis(1));
            }
            seen
        });
        // Never more than 8 queued: the push has to wait for the reader.
        let report = push_samples_with_backpressure(&mut writer, &samples, 8, 1, 2000);
        assert!(!report.timed_out);
        assert_eq!(report.pushed_samples, 40);
        assert_eq!(reading.join().unwrap(), samples);
    }

    #[test]
    fn backpressure_gives_up_when_nothing_reads() {
        let (mut writer, _reader) = sample_ring(64);
        let report = push_samples_with_backpressure(&mut writer, &[1.0; 12], 8, 1, 3);
        assert!(report.timed_out);
        assert_eq!(report.pushed_samples, 8);
        assert_eq!(report.wait_count, 4);
    }

    /// A threshold above the capacity used to spin without sleeping once the
    /// ring was physically full: the level never reached the threshold, and
    /// no push went through.
    #[test]
    fn backpressure_waits_on_a_full_ring_below_its_threshold() {
        let (mut writer, _reader) = sample_ring(8);
        let report = push_samples_with_backpressure(&mut writer, &[1.0; 12], 100, 1, 3);
        assert!(report.timed_out);
        assert_eq!(report.pushed_samples, 8);
    }

    #[test]
    fn drop_overflow_drops_what_is_above_the_threshold() {
        let (mut writer, mut reader) = sample_ring(64);
        let report = push_samples_drop_overflow(&mut writer, &[1.0, 2.0, 3.0, 4.0, 5.0], 3);
        assert_eq!(report.pushed_samples, 3);
        assert_eq!(report.wait_count, 0);
        assert!(!report.timed_out);
        assert_eq!(pop_all(&mut reader), [1.0, 2.0, 3.0]);
    }

    #[test]
    fn flush_returns_once_the_reader_has_played_the_ring_out() {
        let (mut writer, mut reader) = sample_ring(64);
        let monitor = writer.monitor();
        writer.push_slice(&[1.0; 24]);
        let reading = thread::spawn(move || {
            let mut dest = [0.0; 8];
            while reader.available() > 0 {
                reader.pop_slice(&mut dest);
                thread::sleep(Duration::from_millis(2));
            }
            reader
        });
        let report = flush_ring_buffer(
            &monitor,
            Duration::from_secs(5),
            Duration::from_millis(1),
            Some(Duration::from_secs(2)),
        );
        assert!(!report.timed_out && !report.stalled);
        assert_eq!(report.remaining_samples, 0);
        let mut reader = reading.join().unwrap();
        assert_eq!(reader.apply_requested_discard(), 0, "nothing was asked");
    }

    /// A reader that has stopped: the flush gives up, and leaves the rest for
    /// the reader to throw away rather than popping it from this thread.
    #[test]
    fn a_stalled_flush_asks_the_reader_to_drop_the_rest() {
        let (mut writer, mut reader) = sample_ring(64);
        let monitor = writer.monitor();
        writer.push_slice(&[1.0; 10]);
        let report = flush_ring_buffer(
            &monitor,
            Duration::from_secs(5),
            Duration::from_millis(1),
            Some(Duration::from_millis(5)),
        );
        assert!(report.stalled && !report.timed_out);
        assert_eq!(report.remaining_samples, 10);
        assert_eq!(reader.available(), 10, "the flush consumed nothing");

        // Audio written after the flush is not what was asked to go.
        writer.push_slice(&[2.0; 3]);
        assert_eq!(reader.apply_requested_discard(), 10);
        assert_eq!(pop_all(&mut reader), [2.0; 3]);
    }

    #[test]
    fn a_timed_out_flush_asks_the_reader_to_drop_the_rest() {
        let (mut writer, mut reader) = sample_ring(64);
        let monitor = writer.monitor();
        writer.push_slice(&[1.0; 10]);
        let report = flush_ring_buffer(
            &monitor,
            Duration::from_millis(5),
            Duration::from_millis(1),
            None,
        );
        assert!(report.timed_out && !report.stalled);
        assert_eq!(report.remaining_samples, 10);
        assert_eq!(reader.apply_requested_discard(), 10);
    }

    /// Both ends at full speed on two threads, in blocks of changing sizes
    /// over a ring small enough to wrap thousands of times: every sample
    /// arrives once, in order.
    #[test]
    fn two_threads_exchange_every_sample_in_order() {
        const TOTAL: usize = 400_000;
        let (mut writer, mut reader) = sample_ring(1024);
        let monitor = writer.monitor();

        let writing = thread::spawn(move || {
            let block: Vec<f32> = (0..TOTAL).map(|i| i as f32).collect();
            let mut sent = 0;
            let mut size = 1;
            while sent < TOTAL {
                let end = TOTAL.min(sent + size);
                let pushed = writer.push_slice(&block[sent..end]);
                if pushed == 0 {
                    thread::yield_now();
                }
                sent += pushed;
                size = size % 257 + 1;
            }
        });

        let mut next = 0usize;
        let mut dest = [0.0f32; 300];
        let mut size = 1;
        while next < TOTAL {
            assert!(monitor.fill() <= 1024);
            let count = reader.pop_slice(&mut dest[..size]);
            for &sample in &dest[..count] {
                assert_eq!(sample, next as f32);
                next += 1;
            }
            if count == 0 {
                thread::yield_now();
            }
            size = size % 300 + 1;
        }
        writing.join().unwrap();
        assert_eq!(reader.available(), 0);
        assert_eq!(monitor.fill(), 0);
    }
}
