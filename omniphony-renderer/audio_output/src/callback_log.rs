//! Logging from the device callbacks without logging in them.
//!
//! The output callbacks are the engine's only hard-realtime code. A `log::`
//! call there takes the process logger's mutex and formats a string — the
//! underrun path, the moment the callback is already late, being the one that
//! logged most. Instead the callback pushes a [`CallbackEvent`] — a static
//! message, a few numbers, and the resampler's error when there is one — onto
//! a preallocated lock-free queue, and a normal thread ([`CallbackLogDrain`])
//! logs it a few milliseconds later. A full queue drops the event and counts
//! it; nothing in the push allocates, locks or formats, and an event owns
//! nothing, so dropping one frees nothing either.
//!
//! `tests/realtime_callbacks.rs` holds the callbacks to this.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam::queue::ArrayQueue;

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

    /// Add a named number. Beyond [`MAX_FIELDS`] it is ignored.
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

/// The queue between a callback and its drain.
pub struct CallbackLog {
    target: &'static str,
    queue: ArrayQueue<CallbackEvent>,
    dropped: AtomicU64,
    /// The most verbose level the process logger accepts for `target`, as
    /// `log::Level as usize` (0: none). `log::max_level()` cannot stand in for
    /// it: the engine's logger (`live_log`) pins that to `Trace` and filters
    /// at runtime. The drain thread keeps this current; the callback reads it.
    level: AtomicUsize,
}

impl CallbackLog {
    /// A queue whose events are logged under `target` (the backend's module).
    pub fn new(target: &'static str) -> Arc<Self> {
        Arc::new(Self {
            target,
            queue: ArrayQueue::new(CAPACITY),
            dropped: AtomicU64::new(0),
            level: AtomicUsize::new(accepted_level(target)),
        })
    }

    /// Whether an event at `level` would be logged. One atomic load: the
    /// callback tests it before building an event it would only drop.
    #[inline]
    pub fn enabled(&self, level: log::Level) -> bool {
        level as usize <= self.level.load(Ordering::Relaxed)
    }

    /// Queue `event` for the drain. Realtime-safe: no lock, no allocation.
    #[inline]
    pub fn push(&self, event: CallbackEvent) {
        if !self.enabled(event.level) {
            return;
        }
        if self.queue.push(event).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Take the logger's current level for `target`, so a level changed at
    /// runtime reaches the callback. On a normal thread only.
    fn follow_logger_level(&self) {
        self.level
            .store(accepted_level(self.target), Ordering::Relaxed);
    }

    /// Log everything queued. On a normal thread only.
    pub fn drain(&self) {
        while let Some(event) = self.queue.pop() {
            log::log!(target: self.target, event.level, "{event}");
        }
        let dropped = self.dropped.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            log::warn!(
                target: self.target,
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

/// The thread that drains a [`CallbackLog`]. Stopping it (on drop) drains
/// what is left, so nothing queued before is lost.
pub struct CallbackLogDrain {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl CallbackLogDrain {
    pub fn spawn(log: Arc<CallbackLog>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = Arc::clone(&stop);
            let log = Arc::clone(&log);
            std::thread::Builder::new()
                // Linux keeps 15 bytes of a thread name, and what is left must
                // not read as the callback's own thread in a profiler.
                .name("cb-log-drain".into())
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        log.follow_logger_level();
                        log.drain();
                        // Parked rather than asleep: dropping the drain wakes
                        // it at once instead of waiting out the interval.
                        std::thread::park_timeout(DRAIN_INTERVAL);
                    }
                    log.drain();
                })
                .ok()
        };
        if thread.is_none() {
            // Nothing would ever log the events: have the callback build none.
            log.level.store(0, Ordering::Relaxed);
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

    /// A queue that accepts events up to `level`, whatever logger the test
    /// process has (none: `tests/callback_log_level.rs` covers a real one).
    fn log_at(level: log::Level) -> Arc<CallbackLog> {
        let log = CallbackLog::new("test");
        log.level.store(level as usize, Ordering::Relaxed);
        log
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
        let log = log_at(log::Level::Info);
        assert!(log.enabled(log::Level::Warn));
        assert!(log.enabled(log::Level::Info));
        assert!(!log.enabled(log::Level::Debug));
        log.push(CallbackEvent::new(log::Level::Debug, "filtered"));
        assert!(log.queue.is_empty());
        log.push(CallbackEvent::new(log::Level::Info, "kept"));
        assert_eq!(log.queue.len(), 1);
        assert_eq!(log.dropped.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn a_full_queue_counts_what_it_drops() {
        let log = log_at(log::Level::Error);
        for _ in 0..CAPACITY + 5 {
            log.push(CallbackEvent::new(log::Level::Error, "e"));
        }
        assert_eq!(log.queue.len(), CAPACITY);
        assert_eq!(log.dropped.load(Ordering::Relaxed), 5);
        log.drain();
        assert!(log.queue.is_empty());
        assert_eq!(log.dropped.load(Ordering::Relaxed), 0);
    }
}
