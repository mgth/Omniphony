//! What the output core publishes for display and diagnostics.
//!
//! One relaxed store per field per callback; readers (OSC, Studio, the diag
//! plot) poll whenever they like. Floats are stored as bits.

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use audio_sync::{Phase, Telemetry};

/// See the [module docs](self).
#[derive(Debug, Default)]
pub struct SyncTelemetry {
    phase: AtomicU8,
    latency_ms: AtomicU64,
    error_ms: AtomicU64,
    floor_ms: AtomicU64,
    source_ppm: AtomicU64,
    device_ppm: AtomicU64,
    feedforward_ppm: AtomicU64,
    correction_ppm: AtomicU64,
    realigns: AtomicU64,
    underruns: AtomicU64,
    clock_mismatch: AtomicU8,
    /// Ring reads that came up short after the servo had planned them: a
    /// planning bug or a ring the producer rewound; zero in normal running.
    short_reads: AtomicU64,
}

/// A consistent-enough copy for display.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SyncSnapshot {
    pub phase: u8,
    pub latency_ms: Option<f64>,
    pub error_ms: f64,
    pub floor_ms: f64,
    pub source_ppm: f64,
    pub device_ppm: f64,
    pub feedforward_ppm: f64,
    pub correction_ppm: f64,
    pub realigns: u64,
    pub underruns: u64,
    pub clock_mismatch: bool,
    pub short_reads: u64,
}

fn put(a: &AtomicU64, v: f64) {
    a.store(v.to_bits(), Ordering::Relaxed);
}

fn get(a: &AtomicU64) -> f64 {
    f64::from_bits(a.load(Ordering::Relaxed))
}

/// Codes for [`SyncSnapshot::phase`].
pub const PHASE_STARTING: u8 = 0;
pub const PHASE_RUNNING: u8 = 1;
pub const PHASE_REALIGNING: u8 = 2;

impl SyncTelemetry {
    pub fn new() -> Self {
        let t = Self::default();
        put(&t.latency_ms, f64::NAN);
        t
    }

    pub(crate) fn publish(&self, phase: Phase, t: &Telemetry) {
        let code = match phase {
            Phase::Starting => PHASE_STARTING,
            Phase::Running => PHASE_RUNNING,
            Phase::Realigning => PHASE_REALIGNING,
        };
        self.phase.store(code, Ordering::Relaxed);
        put(&self.latency_ms, t.latency_s.map_or(f64::NAN, |l| l * 1e3));
        put(&self.error_ms, t.error_s * 1e3);
        put(&self.floor_ms, t.latency_floor_s * 1e3);
        put(&self.source_ppm, t.source_ppm);
        put(&self.device_ppm, t.device_ppm);
        put(&self.feedforward_ppm, t.feedforward_ppm);
        put(&self.correction_ppm, t.correction_ppm);
        self.realigns.store(t.realigns, Ordering::Relaxed);
        self.underruns.store(t.underruns, Ordering::Relaxed);
        self.clock_mismatch
            .store(t.clock_mismatch as u8, Ordering::Relaxed);
    }

    pub(crate) fn note_short_read(&self) {
        self.short_reads.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> SyncSnapshot {
        let latency = get(&self.latency_ms);
        SyncSnapshot {
            phase: self.phase.load(Ordering::Relaxed),
            latency_ms: (!latency.is_nan()).then_some(latency),
            error_ms: get(&self.error_ms),
            floor_ms: get(&self.floor_ms),
            source_ppm: get(&self.source_ppm),
            device_ppm: get(&self.device_ppm),
            feedforward_ppm: get(&self.feedforward_ppm),
            correction_ppm: get(&self.correction_ppm),
            realigns: self.realigns.load(Ordering::Relaxed),
            underruns: self.underruns.load(Ordering::Relaxed),
            clock_mismatch: self.clock_mismatch.load(Ordering::Relaxed) != 0,
            short_reads: self.short_reads.load(Ordering::Relaxed),
        }
    }
}
