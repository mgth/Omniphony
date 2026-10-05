//! Logging from the device callbacks without logging in them.
//!
//! The output callbacks are the engine's only hard-realtime code. A `log::`
//! call there takes the process logger's mutex and formats a string — the
//! underrun path, the moment the callback is already late, being the one that
//! logged most. Instead the callback pushes a [`CallbackEvent`] — a static
//! message, a few numbers, and an error moved in when there is one — onto a
//! preallocated lock-free queue, and a normal thread ([`CallbackLogDrain`])
//! logs it a few milliseconds later. A full queue drops the event and counts
//! it; nothing in the push allocates, locks or formats.
//!
//! `tests/realtime_callbacks.rs` holds the callbacks to this.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

/// An error a callback reports, moved in as it is: formatting it is the
/// drain's job.
pub enum CallbackError {
    Any(anyhow::Error),
    Resample(rubato::ResampleError),
}

impl From<anyhow::Error> for CallbackError {
    fn from(error: anyhow::Error) -> Self {
        Self::Any(error)
    }
}

impl From<rubato::ResampleError> for CallbackError {
    fn from(error: rubato::ResampleError) -> Self {
        Self::Resample(error)
    }
}

impl std::fmt::Display for CallbackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Any(error) => write!(f, "{error:#}"),
            Self::Resample(error) => write!(f, "{error}"),
        }
    }
}

/// One record of what happened in a callback.
pub struct CallbackEvent {
    level: log::Level,
    message: &'static str,
    fields: [(&'static str, f64); MAX_FIELDS],
    len: usize,
    error: Option<CallbackError>,
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

    /// Carry `error`, to be formatted by the drain.
    pub fn with_error(mut self, error: impl Into<CallbackError>) -> Self {
        self.error = Some(error.into());
        self
    }

    fn text(&self) -> String {
        let mut text = self.message.to_string();
        for (i, (name, value)) in self.fields[..self.len].iter().enumerate() {
            text.push_str(if i == 0 { " (" } else { ", " });
            text.push_str(&format!("{name}={value}"));
        }
        if self.len > 0 {
            text.push(')');
        }
        if let Some(error) = &self.error {
            text.push_str(&format!(": {error}"));
        }
        text
    }
}

/// Queue an event on a [`CallbackLog`] from a device callback:
/// `callback_event!(log, Warn, "message", name = value, …; error)`. Builds
/// nothing when the level is filtered out. Values are converted with `as f64`.
macro_rules! callback_event {
    ($log:expr, $level:ident, $message:expr $(, $name:ident = $value:expr)* $(; $error:expr)?) => {{
        if $crate::callback_log::CallbackLog::enabled(::log::Level::$level) {
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
}

impl CallbackLog {
    /// A queue whose events are logged under `target` (the backend's name).
    pub fn new(target: &'static str) -> Arc<Self> {
        Arc::new(Self {
            target,
            queue: ArrayQueue::new(CAPACITY),
            dropped: AtomicU64::new(0),
        })
    }

    /// Whether an event at `level` would be logged. One atomic load: the
    /// callback tests it before building an event it would only drop.
    #[inline]
    pub fn enabled(level: log::Level) -> bool {
        level <= log::max_level()
    }

    /// Queue `event` for the drain. Realtime-safe: no lock, no allocation.
    #[inline]
    pub fn push(&self, event: CallbackEvent) {
        if !Self::enabled(event.level) {
            return;
        }
        if self.queue.push(event).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Log everything queued. On a normal thread only.
    pub fn drain(&self) {
        while let Some(event) = self.queue.pop() {
            log::log!(target: self.target, event.level, "{}", event.text());
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
            std::thread::Builder::new()
                .name("audio-callback-log".into())
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        log.drain();
                        std::thread::sleep(DRAIN_INTERVAL);
                    }
                    log.drain();
                })
                .ok()
        };
        if thread.is_none() {
            log::warn!("audio output: no thread to log the output callback's events");
        }
        Self { stop, thread }
    }
}

impl Drop for CallbackLogDrain {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_formats_its_fields_and_error() {
        let event = CallbackEvent::new(log::Level::Warn, "underrun")
            .with("available", 12u32)
            .with("needed", 64u32)
            .with_error(anyhow::anyhow!("boom"));
        assert_eq!(event.text(), "underrun (available=12, needed=64): boom");
        assert_eq!(
            CallbackEvent::new(log::Level::Info, "started").text(),
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
    fn a_full_queue_counts_what_it_drops() {
        log::set_max_level(log::LevelFilter::Trace);
        let log = CallbackLog::new("test");
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
