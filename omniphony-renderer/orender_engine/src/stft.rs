//! Small STFT/WOLA building blocks shared by the frequency-domain stages
//! (currently the spectral phantom extractor; the DirAC object generator keeps
//! its own equivalent machinery for now).
//!
//! The window pair is sine (√Hann) on BOTH analysis and synthesis at 50% hop:
//! their product is periodic Hann, whose 50%-overlap sum is exactly 1
//! (sin² + cos²), so analysis→synthesis is an *exact* WOLA identity — required
//! for the residual-bed path, which must be bit-transparent when nothing is
//! extracted. (The DirAC generator's Hann·Hann pair at 50% hop is NOT
//! constant-overlap-add — it carries a hop-rate amplitude ripple that only its
//! decorrelated diffuse stream can absorb.)
//!
//! The output FIFO is primed with `fft_size − hop` zeros; together with the
//! `hop`-sample analysis fill and the `fft_size − hop` synthesis overlap this
//! gives every stream through an [`OlaFifo`] the same fixed end-to-end latency
//! of exactly `fft_size` samples.

use crate::object_gen::flush_denorm;

/// Sine window (√ of the periodic Hann) of length `n` — the analysis *and*
/// synthesis window; their product overlap-adds to exactly 1 at 50% hop.
pub(crate) fn sine_window(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| (std::f32::consts::PI * i as f32 / n as f32).sin())
        .collect()
}

/// Synthesis overlap-add accumulator + primed output FIFO for one stream.
///
/// `add_frame` folds one inverse-FFT frame in (synthesis window + WOLA scale)
/// and releases `hop` finished samples into the FIFO; `pop` consumes them one
/// sample at a time. The FIFO priming makes the stream come out delayed by
/// exactly one FFT frame regardless of the host block size.
pub(crate) struct OlaFifo {
    ola: Vec<f32>,
    fifo: Vec<f32>,
    read: usize,
    write: usize,
    len: usize,
    hop: usize,
}

impl OlaFifo {
    /// `max_block` bounds the host block size the FIFO must absorb between pops.
    pub(crate) fn new(fft_size: usize, hop: usize, max_block: usize) -> Self {
        let mut s = Self {
            ola: vec![0.0; fft_size],
            fifo: vec![0.0; max_block + fft_size],
            read: 0,
            write: 0,
            len: 0,
            hop,
        };
        // Prime so the total latency lands on exactly one FFT frame: the first
        // release happens after `hop` input samples and its first
        // `fft_size − hop` samples predate the input, so `fft_size − hop`
        // zeros up front put input sample 0 at output sample `fft_size`.
        for _ in 0..fft_size - hop {
            s.push(0.0);
        }
        s
    }

    #[inline]
    fn push(&mut self, v: f32) {
        if self.len < self.fifo.len() {
            self.fifo[self.write] = v;
            self.write = (self.write + 1) % self.fifo.len();
            self.len += 1;
        }
    }

    #[inline]
    pub(crate) fn pop(&mut self) -> f32 {
        if self.len == 0 {
            return 0.0;
        }
        let v = self.fifo[self.read];
        self.read = (self.read + 1) % self.fifo.len();
        self.len -= 1;
        v
    }

    /// Fold one inverse-FFT output frame into the accumulator (`scale` carries
    /// the iFFT normalisation and the window COLA factor) and release `hop`
    /// samples.
    pub(crate) fn add_frame(&mut self, ifft_out: &[f32], win: &[f32], scale: f32) {
        let n = self.ola.len();
        for i in 0..n {
            self.ola[i] += scale * win[i] * ifft_out[i];
        }
        self.release();
    }

    /// Advance one hop without new content (the pending tail still drains) —
    /// the cheap path for a stream whose current frame is silent.
    pub(crate) fn add_silent_frame(&mut self) {
        self.release();
    }

    fn release(&mut self) {
        let n = self.ola.len();
        let hop = self.hop;
        for i in 0..hop {
            self.push(flush_denorm(self.ola[i]));
        }
        self.ola.copy_within(hop..n, 0);
        for v in self.ola[n - hop..n].iter_mut() {
            *v = 0.0;
        }
    }
}

/// Plain fixed-length delay ring, to keep the channels that bypass the STFT
/// (LFE, unpositioned) time-aligned with the transformed ones.
pub(crate) struct DelayLine {
    buf: Vec<f32>,
    idx: usize,
}

impl DelayLine {
    pub(crate) fn new(delay: usize) -> Self {
        Self {
            buf: vec![0.0; delay.max(1)],
            idx: 0,
        }
    }

    #[inline]
    pub(crate) fn push_pop(&mut self, x: f32) -> f32 {
        let y = self.buf[self.idx];
        self.buf[self.idx] = x;
        self.idx += 1;
        if self.idx >= self.buf.len() {
            self.idx = 0;
        }
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_line_delays_exactly() {
        let mut dl = DelayLine::new(4);
        let mut out = Vec::new();
        for i in 0..10 {
            out.push(dl.push_pop(i as f32));
        }
        assert_eq!(out[..4], [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(out[4..], [0.0, 1.0, 2.0, 3.0, 4.0, 5.0]);
    }

    #[test]
    fn ola_fifo_priming_and_underflow() {
        let mut f = OlaFifo::new(8, 4, 16);
        for _ in 0..4 {
            assert_eq!(f.pop(), 0.0); // fft_size − hop primed zeros
        }
        // Nothing folded in yet → underflow reads back zeros.
        assert_eq!(f.pop(), 0.0);
    }
}
