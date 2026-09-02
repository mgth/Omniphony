//! Per-speaker fractional delay line with linear-interpolated read pointer.
//!
//! # Design
//!
//! Each `DelayLine` holds a fixed-size circular buffer sized for 100 ms at the
//! renderer's sample rate.  The read pointer is fractional and ramps toward the
//! target at a capped velocity of **1 delay-sample per output sample**, so a
//! 100 ms delay change takes at most 100 ms to complete with no discontinuity.
//!
//! Fractional positions are resolved with linear interpolation between the two
//! neighbouring buffer slots.

/// Maximum ramp speed: delay changes by at most this many samples per output sample.
/// At this rate a 100 ms change at 48 kHz (4 800 samples) completes in 100 ms.
const RAMP_RATE: f32 = 1.0;

pub struct DelayLine {
    /// Circular buffer, zero-initialised.  Size = max_delay_samples + 2.
    /// The +2 gives one extra slot for the linear-interpolation upper neighbour
    /// and one slot of safety margin.
    buf: Vec<f32>,

    /// Next write position (advances by 1 each sample, wraps at buf.len()).
    write_pos: usize,

    /// Current fractional delay in samples — the actual read offset used this
    /// sample.  Ramps toward `target` at ≤ RAMP_RATE per sample.
    current: f32,

    /// Target delay in samples, pre-computed from `delay_ms × sample_rate / 1000`.
    /// Updated by `set_target_ms`; never changes between calls.
    target: f32,
}

impl DelayLine {
    /// Allocate a delay line capable of holding up to `max_delay_samples` of
    /// history.  The buffer is zeroed so early reads produce silence.
    pub fn new(max_delay_samples: usize) -> Self {
        Self {
            buf: vec![0.0f32; max_delay_samples + 2],
            write_pos: 0,
            current: 0.0,
            target: 0.0,
        }
    }

    /// Set the target delay from milliseconds + sample rate.
    ///
    /// The conversion (`ms × sr / 1000`) is done **once here**, so `process`
    /// never performs it in the hot loop.  Clamped to `[0, max_delay_samples]`.
    pub fn set_target_ms(&mut self, delay_ms: f32, sample_rate: u32) {
        let max = (self.buf.len() - 2) as f32;
        self.target = (delay_ms * sample_rate as f32 / 1000.0).clamp(0.0, max);
    }

    /// Returns `true` if this delay line is a no-op (target and current are 0).
    #[inline]
    pub fn is_bypass(&self) -> bool {
        self.target == 0.0 && self.current == 0.0
    }

    /// Keep the ring warm without doing the fractional read.
    ///
    /// While [`is_bypass`](Self::is_bypass) holds, `process` reduces to the
    /// identity: the read pointer sits exactly on the write position with a
    /// zero fractional part, so the interpolation returns `input` unchanged.
    /// A caller that has already checked `is_bypass` can skip to the write —
    /// but it must still *do* the write, or the history is missing when a
    /// non-zero delay is set later and the ramp starts reading behind the
    /// write pointer.
    #[inline]
    pub fn push_history(&mut self, input: f32) {
        debug_assert!(
            self.is_bypass(),
            "push_history is only the identity while bypassed",
        );
        self.buf[self.write_pos] = input;
        self.write_pos += 1;
        if self.write_pos == self.buf.len() {
            self.write_pos = 0;
        }
    }

    /// Process one sample through the delay line.
    ///
    /// Write `input` into the buffer, ramp the read pointer one step toward the
    /// target, then return the linearly-interpolated sample at the current read
    /// position.
    #[inline]
    pub fn process(&mut self, input: f32) -> f32 {
        let cap = self.buf.len();

        // Write.
        self.buf[self.write_pos] = input;

        // Ramp current toward target (capped at RAMP_RATE per sample).
        let delta = self.target - self.current;
        if delta.abs() <= RAMP_RATE {
            self.current = self.target;
        } else {
            self.current += RAMP_RATE * delta.signum();
        }

        // Fractional read (linear interpolation). `current` is clamped to
        // `[0, cap - 2]`, so the read position is at most one lap behind the
        // write position: one conditional add wraps it, with no division in
        // the per-sample path (this runs twice per channel per sample for
        // the ITD alone).
        let mut read_f = self.write_pos as f32 - self.current;
        if read_f < 0.0 {
            read_f += cap as f32;
        }
        // f32 edge: a tiny negative (current a hair above write_pos) rounds
        // to exactly `cap` after the add, which floor()s to an out-of-bounds
        // index. `cap` is the same position as 0 — wrap it.
        if read_f >= cap as f32 {
            read_f = 0.0;
        }
        let i0 = read_f as usize;
        let i1 = if i0 + 1 == cap { 0 } else { i0 + 1 };
        let frac = read_f - i0 as f32;
        let output = self.buf[i0] + frac * (self.buf[i1] - self.buf[i0]);

        // Advance write pointer.
        self.write_pos += 1;
        if self.write_pos == cap {
            self.write_pos = 0;
        }

        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fractional delay reads back the impulse at the right place, on
    /// every lap of the ring — the wrap of both the write pointer and the
    /// read position is exercised many times over.
    #[test]
    fn fractional_delay_is_exact_across_ring_laps() {
        let mut dl = DelayLine::new(30);
        dl.set_target_ms(5.5 / 48.0, 48_000);
        // Settle the ramp on silence.
        for _ in 0..64 {
            dl.process(0.0);
        }
        for lap in 0..7 {
            let mut out = Vec::new();
            for t in 0..40 {
                out.push(dl.process(if t == 0 { 1.0 } else { 0.0 }));
            }
            // 5.5 samples: half the impulse at 5, half at 6, nothing else.
            for (t, &y) in out.iter().enumerate() {
                let expected = if t == 5 || t == 6 { 0.5 } else { 0.0 };
                assert!((y - expected).abs() < 1e-6, "lap {lap} t {t}: {y}");
            }
        }
    }

    /// Regression: a fractional target a hair above an integer write position
    /// makes `(write_pos - current)` a tiny negative; `rem_euclid(cap)` then
    /// rounds to exactly `cap` in f32 and the floor()ed read index lands out
    /// of bounds. Sweep fractional targets right above 5 samples through full
    /// buffer laps — this panicked before the wrap guard.
    #[test]
    fn fractional_delay_just_above_write_pos_does_not_panic() {
        for k in 0..400 {
            let mut dl = DelayLine::new(144);
            // Targets densely covering (5.0, 5.0 + ~1e-5) samples.
            let target_samples = 5.0f32 + k as f32 * 2.5e-8;
            dl.set_target_ms(target_samples / 48.0, 48_000);
            for i in 0..600 {
                let y = dl.process((i % 7) as f32 - 3.0);
                assert!(y.is_finite());
            }
        }
    }
}
