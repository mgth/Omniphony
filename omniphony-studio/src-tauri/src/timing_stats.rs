//! Sliding-window statistics for the renderer's timing telemetry.
//!
//! The renderer reports latency and per-stage frame times as they happen —
//! anywhere from 10 Hz to well over 1 kHz, depending on the format's frame size
//! (a 40-sample TrueHD access unit ticks far faster than a 32 ms E-AC-3 sync
//! frame). The Studio only ever draws min/mean/max over a few seconds of that,
//! so forwarding every sample to the frontend just to have JS reduce it is
//! wasted IPC.
//!
//! # Why buckets rather than a list of samples
//!
//! The frontend kept each series as an array of `{t, v}`, pushing per message
//! and shifting expired entries off the front. That is unbounded in the message
//! rate: nothing in the protocol caps how fast the renderer reports, and the
//! cost of the window grows with it.
//!
//! Here each series is a fixed ring of time buckets holding `(count, sum, min,
//! max)`. Recording is O(1) with no allocation whatever the rate; a bucket is
//! reset lazily when its slot is reused, so no sweeping is needed. Memory is
//! fixed at construction.
//!
//! The cost is granularity: the window edge lands on a bucket boundary, so a
//! query covers its span ±one bucket. With [`BUCKETS`] buckets that is well
//! under a percent of the span — invisible on a gauge, and the tests pin it.

/// Buckets per series. The window span is divided evenly across these, so this
/// sets the granularity: 200 buckets over a 5 s span is a 25 ms edge.
const BUCKETS: usize = 200;

#[derive(Clone, Copy)]
struct Bucket {
    /// Which slice of time this bucket holds, as `now_ms / bucket_ms`. A slot
    /// whose epoch is not the one being asked for is stale and reads as empty,
    /// which is what makes expiry free.
    epoch: u64,
    count: u32,
    sum: f64,
    min: f64,
    max: f64,
}

impl Bucket {
    const EMPTY: Bucket = Bucket {
        epoch: u64::MAX,
        count: 0,
        sum: 0.0,
        min: f64::INFINITY,
        max: f64::NEG_INFINITY,
    };

    fn restart(&mut self, epoch: u64, value: f64) {
        self.epoch = epoch;
        self.count = 1;
        self.sum = value;
        self.min = value;
        self.max = value;
    }
}

/// Aggregate of one series over a queried span.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowStats {
    pub min: f64,
    pub max: f64,
    pub mean: f64,
    pub count: u32,
}

/// One series: a ring of time buckets covering `span_ms`.
pub struct TimeWindow {
    buckets: Vec<Bucket>,
    bucket_ms: u64,
}

impl TimeWindow {
    /// Allocate a window covering `span_ms`. This is the only allocation the
    /// window ever performs.
    pub fn new(span_ms: u64) -> Self {
        let bucket_ms = (span_ms / BUCKETS as u64).max(1);
        Self {
            buckets: vec![Bucket::EMPTY; BUCKETS],
            bucket_ms,
        }
    }

    /// Fold one sample in. Non-finite values are ignored rather than poisoning
    /// the series — the renderer reports a NaN write time when the output is
    /// not running yet.
    pub fn record(&mut self, now_ms: u64, value: f64) {
        if !value.is_finite() {
            return;
        }
        let epoch = now_ms / self.bucket_ms;
        let slot = (epoch % BUCKETS as u64) as usize;
        let bucket = &mut self.buckets[slot];
        if bucket.epoch == epoch {
            bucket.count += 1;
            bucket.sum += value;
            if value < bucket.min {
                bucket.min = value;
            }
            if value > bucket.max {
                bucket.max = value;
            }
        } else {
            // Slot belongs to an older lap of the ring: reuse it as this epoch's.
            bucket.restart(epoch, value);
        }
    }

    /// Aggregate the last `span_ms`, or `None` if nothing landed in it.
    ///
    /// `span_ms` may be shorter than the window's own span — that is how one
    /// series serves both a long max and a short mean.
    pub fn stats(&self, now_ms: u64, span_ms: u64) -> Option<WindowStats> {
        let current = now_ms / self.bucket_ms;
        let reach = (span_ms / self.bucket_ms).min(BUCKETS as u64 - 1);
        let oldest = current.saturating_sub(reach);

        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        let mut sum = 0.0;
        let mut count: u32 = 0;

        for bucket in &self.buckets {
            // `> current` guards a clock that went backwards; `< oldest` is the
            // ordinary expiry test, and also rejects never-written slots since
            // their epoch is u64::MAX... which would read as future, so the
            // first test catches those.
            if bucket.epoch > current || bucket.epoch < oldest {
                continue;
            }
            count += bucket.count;
            sum += bucket.sum;
            if bucket.min < min {
                min = bucket.min;
            }
            if bucket.max > max {
                max = bucket.max;
            }
        }

        if count == 0 {
            return None;
        }
        Some(WindowStats {
            min,
            max,
            mean: sum / count as f64,
            count,
        })
    }

    /// Drop every sample. Used when the renderer stops reporting a series, so a
    /// stale max cannot linger on the gauge.
    pub fn clear(&mut self) {
        for bucket in &mut self.buckets {
            *bucket = Bucket::EMPTY;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_window_has_no_stats() {
        let w = TimeWindow::new(5000);
        assert!(w.stats(0, 5000).is_none());
        assert!(w.stats(1_000_000, 5000).is_none());
    }

    #[test]
    fn aggregates_min_mean_max() {
        let mut w = TimeWindow::new(5000);
        for (i, v) in [10.0, 20.0, 30.0].iter().enumerate() {
            w.record(1000 + i as u64 * 100, *v);
        }
        let s = w.stats(1300, 5000).expect("samples are in window");
        assert_eq!(s.count, 3);
        assert_eq!(s.min, 10.0);
        assert_eq!(s.max, 30.0);
        assert!((s.mean - 20.0).abs() < 1e-9);
    }

    #[test]
    fn samples_leave_the_window_as_time_passes() {
        let mut w = TimeWindow::new(5000);
        w.record(0, 42.0);
        assert!(w.stats(1000, 5000).is_some());
        // Far past the span: the sample is gone.
        assert!(w.stats(20_000, 5000).is_none());
    }

    #[test]
    fn a_short_span_sees_only_recent_samples() {
        let mut w = TimeWindow::new(5000);
        w.record(0, 100.0);
        w.record(4000, 1.0);
        // Long span sees both.
        let long = w.stats(4000, 5000).unwrap();
        assert_eq!(long.max, 100.0);
        // Short span sees only the recent one. This is the case that matters:
        // the render-time series is asked for a 5 s max and a 1 s mean.
        let short = w.stats(4000, 1000).unwrap();
        assert_eq!(short.max, 1.0);
        assert_eq!(short.min, 1.0);
    }

    /// The window edge lands on a bucket boundary, so a query reaches at most
    /// one bucket further back than asked. Pin that bound.
    #[test]
    fn the_window_edge_is_accurate_to_one_bucket() {
        let span = 5000u64;
        let bucket_ms = span / BUCKETS as u64;
        let mut w = TimeWindow::new(span);
        w.record(0, 1.0);
        // Just inside the span: definitely present.
        assert!(w.stats(span - bucket_ms, span).is_some());
        // Beyond the span by more than one bucket: definitely gone.
        assert!(w.stats(span + 2 * bucket_ms, span).is_none());
    }

    #[test]
    fn a_high_rate_series_stays_bounded_and_correct() {
        let mut w = TimeWindow::new(5000);
        // 2 kHz for 10 seconds — twice the window, 20k samples through a
        // structure that never grows.
        for i in 0..20_000u64 {
            w.record(i / 2, (i % 100) as f64);
        }
        let s = w.stats(10_000, 5000).unwrap();
        // Only the last 5 s of samples may be counted, never all 20k.
        assert!(s.count <= 10_100, "count was {}", s.count);
        assert!(s.count >= 9_900, "count was {}", s.count);
        assert_eq!(w.buckets.len(), BUCKETS);
    }

    #[test]
    fn non_finite_samples_are_ignored() {
        let mut w = TimeWindow::new(5000);
        w.record(0, f64::NAN);
        w.record(0, f64::INFINITY);
        assert!(w.stats(0, 5000).is_none());
        w.record(0, 5.0);
        let s = w.stats(0, 5000).unwrap();
        assert_eq!(s.count, 1);
        assert_eq!(s.max, 5.0);
    }

    #[test]
    fn clear_drops_everything() {
        let mut w = TimeWindow::new(5000);
        w.record(0, 1.0);
        w.clear();
        assert!(w.stats(0, 5000).is_none());
    }

    /// A stale slot from an earlier lap of the ring must not be read as current.
    #[test]
    fn a_stale_ring_slot_does_not_resurface() {
        let span = 5000u64;
        let bucket_ms = span / BUCKETS as u64;
        let mut w = TimeWindow::new(span);
        w.record(0, 999.0);
        // Advance exactly one full lap so the same slot comes round again.
        let one_lap = bucket_ms * BUCKETS as u64;
        w.record(one_lap, 1.0);
        let s = w.stats(one_lap, span).unwrap();
        assert_eq!(s.max, 1.0, "the old lap's 999 leaked through");
        assert_eq!(s.count, 1);
    }
}
