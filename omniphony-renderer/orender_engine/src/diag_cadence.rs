//! The cadence diag bundles go out at, shared by every host that publishes
//! them.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// Tracks the diag-publication cadence. The rate is read from a shared
/// atomic (`RendererControl::diag_rate_atomic`) each tick so the user
/// can adjust the cadence live; the interval is cached to avoid recomputing
/// the Duration from f32 each call.
pub struct DiagPublishCadence {
    pub rate_hz_bits: Arc<AtomicU32>,
    pub interval: Duration,
    pub last_rate_seen: f32,
    pub last_send_at: Option<Instant>,
}

impl DiagPublishCadence {
    pub fn new(rate_hz_bits: Arc<AtomicU32>) -> Self {
        let initial = f32::from_bits(rate_hz_bits.load(Ordering::Relaxed)).max(1.0);
        Self {
            rate_hz_bits,
            interval: Duration::from_secs_f32(1.0 / initial),
            last_rate_seen: initial,
            last_send_at: None,
        }
    }

    /// Return true if the interval has elapsed since the last send (or no
    /// send has happened yet); refresh the cached interval from the atomic
    /// when the rate has changed.
    pub fn should_send(&mut self, now: Instant) -> bool {
        let hz = f32::from_bits(self.rate_hz_bits.load(Ordering::Relaxed)).max(1.0);
        if (hz - self.last_rate_seen).abs() > 1e-3 {
            self.last_rate_seen = hz;
            self.interval = Duration::from_secs_f32(1.0 / hz);
        }
        match self.last_send_at {
            None => true,
            Some(last) => now.duration_since(last) >= self.interval,
        }
    }

    pub fn mark_sent(&mut self, now: Instant) {
        self.last_send_at = Some(now);
    }
}
