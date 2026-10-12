//! Line fit for a source whose readings arrive late by a one-sided jitter.
//!
//! A `follow` source (mpv writing a pipe) delivers bytes no earlier than its
//! clock says, but often later: scheduling, pipe buffering, the player's own
//! batching. Measured on real mpv: ±20 ms. Each reading `(t, received)` lies on
//! or *below* the source's true line `received = rate·t + c`. Averaging such
//! readings (a DLL) both biases the phase by the mean lateness and keeps it
//! noisy, and the servo drags the true latency along with the estimate.
//!
//! For one-sided delay the classic estimator (Moon, Skelly & Towsley, 1999,
//! "Estimation and removal of clock skew from network delay measurements") is
//! a linear program: the line lying on or above every reading with the least
//! total distance to them. Its solution is an edge of the readings' **upper
//! convex hull** — the one spanning the readings' mean time. It converges as
//! `1/N` rather than `1/√N`, needs no rate prior, and involves no feedback.
//!
//! Here the readings are first reduced to the earliest one per
//! [`BUCKET_S`] (which keeps the hull small), over a sliding window (up to
//! [`MAX_WINDOW_S`]) so a changing rate (display-resample) is followed. The
//! hull is rebuilt when a bucket closes, in `O(n)` (monotone chain, the
//! buckets being in time order). Fixed capacity, no allocation after
//! construction.
//!
//! Its **slope** is what the `follow` estimator uses (see `source.rs`): a
//! position extrapolated from the hull edge at the window's mean time to the
//! present carries that edge's slope error over half the window.

/// One reduced reading per this long (s).
pub const BUCKET_S: f64 = 0.25;
/// Longest window of buckets the fit can use (s).
pub const MAX_WINDOW_S: f64 = 128.0;
const BUCKETS: usize = 512; // MAX_WINDOW_S / BUCKET_S

/// See the [module docs](self).
#[derive(Debug, Clone)]
pub struct ArrivalEnvelope {
    nominal_rate: f64,
    window_s: f64,
    origin: Option<f64>,
    /// Closed buckets `(x, y)` in time order, `x = t − origin`,
    /// `y = received − nominal·x` (residual to the nominal line).
    buckets: [(f64, f64); BUCKETS],
    head: usize,
    len: usize,
    /// The bucket being filled: its index and best reading so far.
    open: Option<(i64, (f64, f64))>,
    /// Hull scratch: the points in time order, and hull indices into them.
    pts: [(f64, f64); BUCKETS + 1],
    hull: [usize; BUCKETS + 1],
    /// Fitted line on residuals: `y = slope·(x − x0) + y0`.
    fit: Option<(f64, f64, f64)>,
}

impl ArrivalEnvelope {
    /// A fit over `window_s` (at most [`MAX_WINDOW_S`]) for a source
    /// nominally at `nominal_rate` frames per second.
    pub fn new(nominal_rate: f64, window_s: f64) -> Self {
        Self {
            nominal_rate,
            window_s: window_s.min(MAX_WINDOW_S),
            origin: None,
            buckets: [(0.0, 0.0); BUCKETS],
            head: 0,
            len: 0,
            open: None,
            pts: [(0.0, 0.0); BUCKETS + 1],
            hull: [0; BUCKETS + 1],
            fit: None,
        }
    }

    /// Forget every reading (the source restarted).
    pub fn reset(&mut self) {
        self.origin = None;
        self.head = 0;
        self.len = 0;
        self.open = None;
        self.fit = None;
    }

    /// Take a reading: `received` frames had arrived at `t`.
    pub fn observe(&mut self, t: f64, received: f64) {
        let origin = *self.origin.get_or_insert(t);
        let x = t - origin;
        let y = received - self.nominal_rate * x;
        let index = (x / BUCKET_S).floor() as i64;
        match self.open {
            Some((i, best)) if i == index => {
                if y > best.1 {
                    self.open = Some((i, (x, y)));
                    if self.len == 0 {
                        self.refit();
                    }
                }
            }
            Some((_, best)) => {
                self.close_bucket(best);
                self.open = Some((index, (x, y)));
                self.refit();
            }
            None => {
                self.open = Some((index, (x, y)));
                self.refit();
            }
        }
    }

    fn close_bucket(&mut self, point: (f64, f64)) {
        if self.len == BUCKETS {
            self.head = (self.head + 1) % BUCKETS;
            self.len -= 1;
        }
        self.buckets[(self.head + self.len) % BUCKETS] = point;
        self.len += 1;
        while self.len > 1 && point.0 - self.buckets[self.head].0 > self.window_s {
            self.head = (self.head + 1) % BUCKETS;
            self.len -= 1;
        }
    }

    /// Rebuild the upper hull of the closed buckets plus the open one and
    /// pick the edge spanning their mean time.
    fn refit(&mut self) {
        let mut n = 0;
        for i in 0..self.len {
            self.pts[n] = self.buckets[(self.head + i) % BUCKETS];
            n += 1;
        }
        if let Some((_, p)) = self.open {
            self.pts[n] = p;
            n += 1;
        }
        if n == 0 {
            self.fit = None;
            return;
        }
        if n == 1 {
            self.fit = Some((0.0, self.pts[0].0, self.pts[0].1));
            return;
        }
        // Upper hull, monotone chain (points are in increasing x).
        let mut h = 0;
        for i in 0..n {
            while h >= 2
                && cross(
                    self.pts[self.hull[h - 2]],
                    self.pts[self.hull[h - 1]],
                    self.pts[i],
                ) >= 0.0
            {
                h -= 1;
            }
            self.hull[h] = i;
            h += 1;
        }
        let mean_x = self.pts[..n].iter().map(|p| p.0).sum::<f64>() / n as f64;
        let mut edge = (self.hull[0], self.hull[h - 1]);
        for w in 0..h - 1 {
            let (a, b) = (self.hull[w], self.hull[w + 1]);
            if self.pts[a].0 <= mean_x && mean_x <= self.pts[b].0 {
                edge = (a, b);
                break;
            }
        }
        let (a, b) = (self.pts[edge.0], self.pts[edge.1]);
        let slope = if b.0 > a.0 {
            (b.1 - a.1) / (b.0 - a.0)
        } else {
            0.0
        };
        self.fit = Some((slope, a.0, a.1));
    }

    /// Time spanned by the closed buckets (s): how much evidence the slope
    /// rests on.
    pub fn span_s(&self) -> f64 {
        if self.len < 2 {
            return 0.0;
        }
        let last = self.buckets[(self.head + self.len - 1) % BUCKETS].0;
        last - self.buckets[self.head].0
    }

    /// Whether a line has been fitted.
    pub fn is_tracking(&self) -> bool {
        self.fit.is_some()
    }

    /// Fitted rate (frames per second), once tracking.
    pub fn rate(&self) -> Option<f64> {
        self.fit.map(|(slope, _, _)| self.nominal_rate + slope)
    }

    /// Fitted position at `t`, once tracking.
    pub fn position_at(&self, t: f64) -> Option<f64> {
        let origin = self.origin?;
        let (slope, x0, y0) = self.fit?;
        let x = t - origin;
        Some(self.nominal_rate * x + y0 + slope * (x - x0))
    }
}

/// `> 0` when `o → a → b` turns left (counter-clockwise).
fn cross(o: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lateness(k: u64) -> f64 {
        let x = (k.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 11) as f64 / (1u64 << 53) as f64;
        0.040 * x
    }

    /// Readings on a line, each arriving late by a pseudo-random 0–40 ms: the
    /// fit recovers the line's position and rate, whatever the rate.
    #[test]
    fn recovers_the_line_from_late_readings() {
        for ppm in [0.0, 80.0, 1000.0, -1000.0] {
            let nominal = 48_000.0;
            let rate = nominal * (1.0 + ppm * 1e-6);
            let mut fit = ArrivalEnvelope::new(nominal, 30.0);
            let mut worst = 0.0f64;
            let mut worst_ppm = 0.0f64;
            for k in 0..(24 * 300u64) {
                let t_true = k as f64 / 24.0;
                let late = lateness(k);
                fit.observe(t_true + late, t_true * rate);
                if t_true > 40.0 {
                    let now = t_true + late;
                    worst = worst.max((fit.position_at(now).unwrap() - now * rate).abs());
                    let r = (fit.rate().unwrap() / nominal - 1.0) * 1e6;
                    worst_ppm = worst_ppm.max((r - ppm).abs());
                }
            }
            // Once the 30 s window is full: within 1 ms of frames, and the
            // rate within 50 ppm (it steps when the hull edge changes; the
            // DLL behind it smooths that).
            eprintln!("{ppm} ppm: worst {worst:.1} frames, rate off by {worst_ppm:.1} ppm");
            assert!(worst < 48.0, "{ppm} ppm: worst {worst} frames");
            assert!(worst_ppm < 50.0, "{ppm} ppm: rate off by {worst_ppm} ppm");
        }
    }

    #[test]
    fn reset_forgets_everything() {
        let mut fit = ArrivalEnvelope::new(48_000.0, 30.0);
        fit.observe(0.0, 10_000.0);
        assert!(fit.is_tracking());
        fit.reset();
        assert!(!fit.is_tracking());
        assert_eq!(fit.position_at(1.0), None);
    }
}
