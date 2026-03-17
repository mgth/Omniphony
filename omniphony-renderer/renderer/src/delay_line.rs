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

        // Fractional read (linear interpolation).
        let read_f = (self.write_pos as f32 - self.current).rem_euclid(cap as f32);
        let i0 = read_f as usize; // floor — always < cap because rem_euclid
        let i1 = (i0 + 1) % cap;
        let frac = read_f - i0 as f32;
        let output = self.buf[i0] + frac * (self.buf[i1] - self.buf[i0]);

        // Advance write pointer.
        self.write_pos = (self.write_pos + 1) % cap;

        output
    }
}
