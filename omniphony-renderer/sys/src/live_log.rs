use env_logger::Logger;
use log::{Level, LevelFilter, Log, Metadata, Record};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

const LOG_BUFFER_CAPACITY: usize = 512;

#[derive(Debug, Clone)]
pub struct BufferedLogRecord {
    pub seq: u64,
    pub level: String,
    pub target: String,
    pub message: String,
}

struct DynamicLogger {
    inner: Logger,
    level: AtomicUsize,
    next_seq: AtomicU64,
    records: Mutex<VecDeque<BufferedLogRecord>>,
}

static LOGGER: OnceLock<&'static DynamicLogger> = OnceLock::new();

impl DynamicLogger {
    fn new(inner: Logger, level: LevelFilter) -> Self {
        Self {
            inner,
            level: AtomicUsize::new(level_to_usize(level)),
            next_seq: AtomicU64::new(1),
            records: Mutex::new(VecDeque::with_capacity(LOG_BUFFER_CAPACITY)),
        }
    }

    fn current_level(&self) -> LevelFilter {
        usize_to_level(self.level.load(Ordering::Relaxed))
    }

    fn set_level(&self, level: LevelFilter) {
        self.level.store(level_to_usize(level), Ordering::Relaxed);
    }

    fn accepts(&self, level: Level) -> bool {
        level_allowed(self.current_level(), level)
    }

    fn push_record(&self, record: &Record<'_>) {
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        let mut records = self.records.lock().unwrap();
        if records.len() >= LOG_BUFFER_CAPACITY {
            records.pop_front();
        }
        records.push_back(BufferedLogRecord {
            seq,
            level: record.level().as_str().to_ascii_lowercase(),
            target: record.target().to_string(),
            message: record.args().to_string(),
        });
    }

    fn push_external_record(&self, level: Level, target: &str, message: &str) {
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        let mut records = self.records.lock().unwrap();
        if records.len() >= LOG_BUFFER_CAPACITY {
            records.pop_front();
        }
        records.push_back(BufferedLogRecord {
            seq,
            level: level.as_str().to_ascii_lowercase(),
            target: target.to_string(),
            message: message.to_string(),
        });
    }

    fn emit_external_record(&self, level: Level, target: &str, message: &str) {
        if self.accepts(level) {
            let args = format_args!("{message}");
            let record = Record::builder()
                .args(args)
                .level(level)
                .target(target)
                .build();
            self.inner.log(&record);
        }
        self.push_external_record(level, target, message);
    }

    fn records_since(&self, last_seq: u64) -> Vec<BufferedLogRecord> {
        let records = self.records.lock().unwrap();
        records
            .iter()
            .filter(|entry| entry.seq > last_seq)
            .cloned()
            .collect()
    }
}

impl Log for DynamicLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        self.accepts(metadata.level())
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        self.inner.log(record);
        self.push_record(record);
    }

    fn flush(&self) {
        self.inner.flush();
    }
}

fn level_to_usize(level: LevelFilter) -> usize {
    match level {
        LevelFilter::Off => 0,
        LevelFilter::Error => 1,
        LevelFilter::Warn => 2,
        LevelFilter::Info => 3,
        LevelFilter::Debug => 4,
        LevelFilter::Trace => 5,
    }
}

fn usize_to_level(value: usize) -> LevelFilter {
    match value {
        0 => LevelFilter::Off,
        1 => LevelFilter::Error,
        2 => LevelFilter::Warn,
        3 => LevelFilter::Info,
        4 => LevelFilter::Debug,
        _ => LevelFilter::Trace,
    }
}

fn level_allowed(filter: LevelFilter, level: Level) -> bool {
    match filter {
        LevelFilter::Off => false,
        LevelFilter::Error => level <= Level::Error,
        LevelFilter::Warn => level <= Level::Warn,
        LevelFilter::Info => level <= Level::Info,
        LevelFilter::Debug => level <= Level::Debug,
        LevelFilter::Trace => true,
    }
}

pub fn init_logger(level: LevelFilter, json: bool) -> Result<(), log::SetLoggerError> {
    let mut builder = env_logger::Builder::from_default_env();
    builder.filter_level(LevelFilter::Trace);
    if json {
        builder.format(|buf, record| {
            use std::io::Write;
            writeln!(
                buf,
                "{{\"ts\":{},\"lvl\":\"{}\",\"target\":\"{}\",\"msg\":\"{}\"}}",
                buf.timestamp(),
                record.level(),
                record.target(),
                record.args()
            )
        });
    } else {
        builder.format_timestamp_secs();
    }
    let logger = Box::leak(Box::new(DynamicLogger::new(builder.build(), level)));
    log::set_logger(logger)?;
    log::set_max_level(LevelFilter::Trace);
    let _ = LOGGER.set(logger);
    Ok(())
}

pub fn set_runtime_level(level: LevelFilter) {
    if let Some(logger) = LOGGER.get() {
        logger.set_level(level);
    }
}

pub fn current_runtime_level() -> LevelFilter {
    LOGGER
        .get()
        .map(|logger| logger.current_level())
        .unwrap_or(LevelFilter::Info)
}

pub fn current_runtime_level_name() -> &'static str {
    match current_runtime_level() {
        LevelFilter::Off => "off",
        LevelFilter::Error => "error",
        LevelFilter::Warn => "warn",
        LevelFilter::Info => "info",
        LevelFilter::Debug => "debug",
        LevelFilter::Trace => "trace",
    }
}

pub fn records_since(last_seq: u64) -> Vec<BufferedLogRecord> {
    LOGGER
        .get()
        .map(|logger| logger.records_since(last_seq))
        .unwrap_or_default()
}

pub fn emit_external_record(level: Level, target: &str, message: &str) {
    if let Some(logger) = LOGGER.get() {
        logger.emit_external_record(level, target, message);
    }
}
