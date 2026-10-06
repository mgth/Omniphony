//! Logging from the device callbacks without logging in them.
//!
//! The output callbacks are the engine's only hard-realtime code. A `log::`
//! call there takes the process logger's mutex and formats a string — the
//! underrun path, the moment the callback is already late, being the one that
//! logged most. Instead the callback pushes a [`CallbackEvent`] — a static
//! message, a few numbers, and the resampler's error when there is one — onto
//! a preallocated ring, and a normal thread ([`CallbackLogDrain`]) logs it a
//! few milliseconds later. A full ring drops the event and counts it; nothing
//! in the push allocates, locks, formats or waits, and an event owns nothing,
//! so dropping one frees nothing either.
//!
//! The ring is `rtrb`'s, single-producer single-consumer: its push reads the
//! consumer's position at most once and gives up when there is no free slot.
//! A general-purpose queue does not promise that. `crossbeam`'s
//! `ArrayQueue::push` spins while a `pop` is in progress on the slot it
//! wants, so with the queue full the callback waited for the drain thread, a
//! normal-priority one, to be scheduled again.
//!
//! `tests/realtime_callbacks.rs` holds the callbacks to this.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// Numbers an event carries at most.
const MAX_FIELDS: usize = 10;

/// Events held between two drains. A callback at 48 kHz / 64 frames runs
/// 750 times a second; a drain every [`DRAIN_INTERVAL`] leaves room for an
/// event per callback and then some.
const CAPACITY: usize = 256;

const DRAIN_INTERVAL: Duration = Duration::from_millis(50);

/// One record of what happened in a callback.
pub struct CallbackEvent {
    level: log::Level,
    message: &'static str,
    fields: [(&'static str, f64); MAX_FIELDS],
    len: usize,
    error: Option<rubato::ResampleError>,
}

impl CallbackEvent {
    pub fn new(level: log::Level, message: &'static str) -> Self {
        Self {
            level,
            message,
            fields: [("", 0.0); MAX_FIELDS],
            len: 0,
            error: None,
        }
    }

    /// Add a named number. Beyond `MAX_FIELDS` it is ignored.
    pub fn with(mut self, name: &'static str, value: impl Into<f64>) -> Self {
        if self.len < MAX_FIELDS {
            self.fields[self.len] = (name, value.into());
            self.len += 1;
        }
        self
    }

    /// Carry the resampler's `error`, to be formatted by the drain. It is
    /// plain data: the callback neither allocates for it nor frees it.
    pub fn with_error(mut self, error: rubato::ResampleError) -> Self {
        self.error = Some(error);
        self
    }
}

/// `message (name=value, …): error`. Written straight into the logger's
/// formatter, so an event the logger filters out is never formatted.
impl std::fmt::Display for CallbackEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message)?;
        for (i, (name, value)) in self.fields[..self.len].iter().enumerate() {
            f.write_str(if i == 0 { " (" } else { ", " })?;
            write!(f, "{name}={value}")?;
        }
        if self.len > 0 {
            f.write_str(")")?;
        }
        if let Some(error) = &self.error {
            write!(f, ": {error}")?;
        }
        Ok(())
    }
}

/// Queue an event on a [`CallbackLog`] from a device callback:
/// `callback_event!(log, Warn, "message", name = value, …; error)`. Builds
/// nothing when the level is filtered out. Values are converted with `as f64`.
macro_rules! callback_event {
    ($log:expr, $level:ident, $message:expr $(, $name:ident = $value:expr)* $(; $error:expr)?) => {{
        if $log.enabled(::log::Level::$level) {
            $log.push(
                $crate::callback_log::CallbackEvent::new(::log::Level::$level, $message)
                    $(.with(stringify!($name), $value as f64))*
                    $(.with_error($error))?,
            );
        }
    }};
}
pub(crate) use callback_event;

/// What the two ends of a callback log share besides the ring.
struct Shared {
    target: &'static str,
    dropped: AtomicU64,
    /// The most verbose level the process logger accepts for `target`, as
    /// `log::Level as usize` (0: none). `log::max_level()` cannot stand in for
    /// it: the engine's logger (`live_log`) pins that to `Trace` and filters
    /// at runtime. The drain thread keeps this current; the callback reads it.
    level: AtomicUsize,
}

/// The callback's end of the log. There is one per log and it cannot be
/// shared or cloned: the ring has a single producer.
pub struct CallbackLog {
    shared: Arc<Shared>,
    events: rtrb::Producer<CallbackEvent>,
}

impl CallbackLog {
    /// A log whose events are logged under `target` (the backend's module):
    /// this end for the callback, the reader for [`CallbackLogDrain::spawn`].
    pub fn new(target: &'static str) -> (Self, CallbackLogReader) {
        let shared = Arc::new(Shared {
            target,
            dropped: AtomicU64::new(0),
            level: AtomicUsize::new(accepted_level(target)),
        });
        let (producer, consumer) = rtrb::RingBuffer::new(CAPACITY);
        let log = Self {
            shared: Arc::clone(&shared),
            events: producer,
        };
        let reader = CallbackLogReader {
            shared,
            events: consumer,
        };
        (log, reader)
    }

    /// Whether an event at `level` would be logged. One atomic load: the
    /// callback tests it before building an event it would only drop.
    #[inline]
    pub fn enabled(&self, level: log::Level) -> bool {
        level as usize <= self.shared.level.load(Ordering::Relaxed)
    }

    /// Queue `event` for the drain. Realtime-safe: no lock, no allocation and
    /// no waiting. Without a free slot the event is dropped and counted, and
    /// that includes the drain being stopped in the middle of taking one.
    #[inline]
    pub fn push(&mut self, event: CallbackEvent) {
        if !self.enabled(event.level) {
            return;
        }
        if self.events.push(event).is_err() {
            self.shared.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// The drain's end of the log.
pub struct CallbackLogReader {
    shared: Arc<Shared>,
    events: rtrb::Consumer<CallbackEvent>,
}

impl CallbackLogReader {
    /// Take the logger's current level for the target, so a level changed at
    /// runtime reaches the callback. On a normal thread only.
    fn follow_logger_level(&self) {
        self.shared
            .level
            .store(accepted_level(self.shared.target), Ordering::Relaxed);
    }

    /// Log everything queued. On a normal thread only.
    pub fn drain(&mut self) {
        let target = self.shared.target;
        while let Ok(event) = self.events.pop() {
            log::log!(target: target, event.level, "{event}");
        }
        let dropped = self.shared.dropped.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            log::warn!(
                target: target,
                "{dropped} output callback log events dropped (queue full)"
            );
        }
    }
}

/// The most verbose level the process logger accepts for `target`, as
/// `log::Level as usize`; 0 when it accepts none.
fn accepted_level(target: &str) -> usize {
    use log::Level::{Debug, Error, Info, Trace, Warn};
    [Trace, Debug, Info, Warn, Error]
        .into_iter()
        .find(|&level| log::log_enabled!(target: target, level))
        .map_or(0, |level| level as usize)
}

/// The thread that drains a [`CallbackLogReader`]. Stopping it (on drop)
/// drains what is left, so nothing queued before is lost.
pub struct CallbackLogDrain {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl CallbackLogDrain {
    pub fn spawn(mut reader: CallbackLogReader) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::clone(&reader.shared);
        let thread = {
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                // Linux keeps 15 bytes of a thread name, and what is left must
                // not read as the callback's own thread in a profiler.
                .name("cb-log-drain".into())
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        reader.follow_logger_level();
                        reader.drain();
                        // Parked rather than asleep: dropping the drain wakes
                        // it at once instead of waiting out the interval.
                        std::thread::park_timeout(DRAIN_INTERVAL);
                    }
                    reader.drain();
                })
                .ok()
        };
        if thread.is_none() {
            // Nothing would ever log the events: have the callback build none.
            shared.level.store(0, Ordering::Relaxed);
            log::warn!("audio output: no thread to log the output callback's events");
        }
        Self { stop, thread }
    }
}

impl Drop for CallbackLogDrain {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A log that accepts events up to `level`, whatever logger the test
    /// process has (none: `tests/callback_log_level.rs` covers a real one).
    fn log_at(level: log::Level) -> (CallbackLog, CallbackLogReader) {
        let (log, reader) = CallbackLog::new("test");
        log.shared.level.store(level as usize, Ordering::Relaxed);
        (log, reader)
    }

    fn dropped(log: &CallbackLog) -> u64 {
        log.shared.dropped.load(Ordering::Relaxed)
    }

    #[test]
    fn an_event_formats_its_fields_and_error() {
        // Not `Clone`: one to carry, one to say what it should read as.
        let error = || rubato::ResampleError::SyncNotAdjustable;
        let event = CallbackEvent::new(log::Level::Warn, "underrun")
            .with("available", 12u32)
            .with("needed", 64u32)
            .with_error(error());
        assert_eq!(
            event.to_string(),
            format!("underrun (available=12, needed=64): {}", error())
        );
        assert_eq!(
            CallbackEvent::new(log::Level::Info, "started").to_string(),
            "started"
        );
    }

    #[test]
    fn fields_beyond_the_maximum_are_ignored() {
        let mut event = CallbackEvent::new(log::Level::Info, "many");
        for _ in 0..MAX_FIELDS + 3 {
            event = event.with("x", 1u8);
        }
        assert_eq!(event.len, MAX_FIELDS);
    }

    #[test]
    fn an_event_below_the_level_is_not_queued() {
        let (mut log, reader) = log_at(log::Level::Info);
        assert!(log.enabled(log::Level::Warn));
        assert!(log.enabled(log::Level::Info));
        assert!(!log.enabled(log::Level::Debug));
        log.push(CallbackEvent::new(log::Level::Debug, "filtered"));
        assert_eq!(reader.events.slots(), 0);
        log.push(CallbackEvent::new(log::Level::Info, "kept"));
        assert_eq!(reader.events.slots(), 1);
        assert_eq!(dropped(&log), 0);
    }

    #[test]
    fn a_full_queue_counts_what_it_drops() {
        let (mut log, mut reader) = log_at(log::Level::Error);
        for _ in 0..CAPACITY + 5 {
            log.push(CallbackEvent::new(log::Level::Error, "e"));
        }
        assert_eq!(reader.events.slots(), CAPACITY);
        assert_eq!(dropped(&log), 5);
        reader.drain();
        assert_eq!(reader.events.slots(), 0);
        assert_eq!(dropped(&log), 0);
    }

    /// The case a general-purpose queue gets wrong: the queue is full and the
    /// drain has been preempted in the middle of taking an event, read but its
    /// slot not released yet. The push must come back at once with the event
    /// dropped, not wait for the drain to be scheduled again.
    #[test]
    fn a_push_does_not_wait_for_a_drain_stopped_mid_event() {
        let (mut log, mut reader) = log_at(log::Level::Error);
        for _ in 0..CAPACITY {
            log.push(CallbackEvent::new(log::Level::Error, "e"));
        }
        assert_eq!(dropped(&log), 0);

        // The drain, held where a preempted thread can be.
        let taking = reader.events.read_chunk(1).unwrap();
        log.push(CallbackEvent::new(log::Level::Error, "no slot yet"));
        assert_eq!(dropped(&log), 1);

        // It resumes and releases the slot: the next event fits.
        taking.commit_all();
        log.push(CallbackEvent::new(log::Level::Error, "fits"));
        assert_eq!(dropped(&log), 1);
        assert_eq!(reader.events.slots(), CAPACITY);
    }
}
