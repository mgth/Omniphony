//! The callback log follows the process logger's own filter.
//!
//! The engine's logger (`live_log`) pins `log::max_level()` to `Trace` and
//! filters at runtime, so that static says nothing about what gets logged. A
//! queue gated on it built and queued every debug event at the default level,
//! on every callback, for the drain to format and the logger to discard.
//!
//! This installs a logger of the same shape. It is alone in its test binary
//! because a process has one logger.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use audio_output::callback_log::{CallbackEvent, CallbackLog, CallbackLogDrain};

/// Accepts up to `level`, which can change while the process runs.
struct RuntimeFiltered {
    level: AtomicUsize,
    lines: Mutex<Vec<String>>,
}

static LOGGER: RuntimeFiltered = RuntimeFiltered {
    level: AtomicUsize::new(log::Level::Info as usize),
    lines: Mutex::new(Vec::new()),
};

impl log::Log for RuntimeFiltered {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() as usize <= self.level.load(Ordering::Relaxed)
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            self.lines.lock().unwrap().push(format!(
                "{} {} {}",
                record.level(),
                record.target(),
                record.args()
            ));
        }
    }

    fn flush(&self) {}
}

fn wait_until(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn the_queue_follows_the_loggers_runtime_level() {
    log::set_logger(&LOGGER).unwrap();
    log::set_max_level(log::LevelFilter::Trace);

    let (mut queue, reader) = CallbackLog::new("audio_output::test");
    assert!(queue.enabled(log::Level::Info));
    assert!(
        !queue.enabled(log::Level::Debug),
        "log::max_level() is Trace, but the logger accepts Info"
    );

    let drain = CallbackLogDrain::spawn(reader);
    queue.push(CallbackEvent::new(log::Level::Debug, "filtered"));
    queue.push(CallbackEvent::new(log::Level::Warn, "kept").with("n", 3u32));

    // A level raised at runtime reaches the callback at the next drain...
    LOGGER
        .level
        .store(log::Level::Debug as usize, Ordering::Relaxed);
    wait_until("the queue accepts debug", || {
        queue.enabled(log::Level::Debug)
    });
    queue.push(CallbackEvent::new(log::Level::Debug, "now wanted"));

    // ...and so does one lowered.
    LOGGER
        .level
        .store(log::Level::Warn as usize, Ordering::Relaxed);
    wait_until("the queue refuses info", || {
        !queue.enabled(log::Level::Info)
    });
    assert!(queue.enabled(log::Level::Warn));
    queue.push(CallbackEvent::new(log::Level::Error, "last"));

    // Stopping the drain logs what is left.
    drop(drain);
    let lines = LOGGER.lines.lock().unwrap();
    assert!(
        !lines.iter().any(|line| line.contains("filtered")),
        "{lines:#?}"
    );
    assert_eq!(lines.first().unwrap(), "WARN audio_output::test kept (n=3)");
    assert_eq!(lines.last().unwrap(), "ERROR audio_output::test last");
}
