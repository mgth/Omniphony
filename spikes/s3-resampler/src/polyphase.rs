//! In-house variable-ratio resampler: polyphase windowed-sinc fractional-delay
//! FIR, interleaved multichannel, exactly N output frames per call, exact
//! fixed-point input position.
//!
//! # Model
//!
//! The input is a sequence of frames `x[j]` (absolute frame index `j`). Each
//! output frame is the band-limited reconstruction `x(p)` at a fractional input
//! position `p`, computed with a `T`-tap kernel centred on `p`:
//!
//! ```text
//! i = floor(p), mu = p - i
//! y = sum_{k=0}^{T-1} c_k(mu) * x[i - T/2 + 1 + k]
//! c_k(mu) = h(mu + T/2 - 1 - k),  h(t) = fc * sinc(fc t) * kaiser(t / (T/2))
//! ```
//!
//! So `p` *is* the played position: no delay is added on top of it. The
//! resampler must have read up to frame `i + T/2`, i.e. it holds `T/2` frames
//! of look-ahead; that is its constant group delay relative to what it has
//! consumed from the ring.
//!
//! `p` advances by the step `s` (input frames per output frame, = the ratio
//! `rate(S)/rate(D)`) after every output frame. `p` and `s` are Q32.32 fixed
//! point, so the accumulated position is exact and reproducible: the servo
//! reads it back rather than integrating the ratio itself.
//!
//! # Coefficient table
//!
//! `c_k(mu)` is tabulated once at construction on `L` segments of `mu`
//! (`L` a power of two). Two interpolation rules between segment ends:
//!
//! * `Linear`: `L + 1` rows, `c = a + f (b - a)`. Error falls 12 dB per
//!   doubling of `L`; ~L = 1024 is needed for -130 dB at 20 kHz.
//! * `Hermite`: per segment, a cubic in `f` fitted to the values and the
//!   exact `d/dmu` at both ends, stored as 4 rows (`c0..c3`, Horner form).
//!   `L = 32` already reaches the f32 floor, so the table stays ~32 KiB.
//!
//! Every row is normalised to unit DC gain (in f64) before rounding to f32.
//! With the cutoff at the input Nyquist (`fc = 1`), the `mu = 0` row is an
//! exact unit impulse, so ratio 1 at phase 0 is bit-transparent.

use std::f64::consts::PI;

pub const FRAC_ONE: f64 = 4294967296.0; // 2^32

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interp {
    Linear,
    Hermite,
}

#[derive(Clone, Copy, Debug)]
pub struct Design {
    /// Kernel length T (even).
    pub taps: usize,
    /// Number of mu segments L (power of two).
    pub segments: usize,
    /// Kaiser window beta.
    pub beta: f64,
    /// Cutoff relative to the input Nyquist (1.0 = fs_in / 2). For a nominal
    /// ratio near 1 the transition is centred on Nyquist; for a fixed
    /// down-conversion use `min(1, fs_out / fs_in)`.
    pub cutoff: f64,
    pub interp: Interp,
}

impl Design {
    /// Recommended design for the drift resampler at 48 kHz (see the spike
    /// report): 64 taps, Kaiser beta 14, Hermite over 32 segments.
    pub const RECOMMENDED: Design = Design {
        taps: 64,
        segments: 32,
        beta: 14.0,
        cutoff: 1.0,
        interp: Interp::Hermite,
    };
}

fn bessel_i0(x: f64) -> f64 {
    // Power series; converges fast for the beta range used here (< 20).
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    let mut k = 1.0;
    loop {
        term *= q / (k * k);
        sum += term;
        if term < sum * 1e-17 {
            return sum;
        }
        k += 1.0;
    }
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else if x == x.round() {
        // exact zero crossings, so the mu = 0 row is an exact impulse
        0.0
    } else {
        let px = PI * x;
        px.sin() / px
    }
}

/// One normalised kernel row (f64) for fractional position `mu`.
fn row(d: &Design, mu: f64, out: &mut [f64]) {
    let half = d.taps as f64 / 2.0;
    let i0b = bessel_i0(d.beta);
    let mut sum = 0.0;
    for (k, o) in out.iter_mut().enumerate() {
        let t = mu + half - 1.0 - k as f64;
        let x = (t / half).clamp(-1.0, 1.0);
        let w = bessel_i0(d.beta * (1.0 - x * x).max(0.0).sqrt()) / i0b;
        let v = d.cutoff * sinc(d.cutoff * t) * w;
        *o = v;
        sum += v;
    }
    for o in out.iter_mut() {
        *o /= sum;
    }
}

pub struct CoefTable {
    taps: usize,
    log2_segments: u32,
    interp: Interp,
    /// Linear: (L+1) rows x T. Hermite: L segments x 4 rows x T.
    data: Vec<f32>,
}

impl CoefTable {
    pub fn new(d: &Design) -> Self {
        assert!(d.taps >= 4 && d.taps % 2 == 0);
        assert!(d.segments.is_power_of_two());
        let t = d.taps;
        let l = d.segments;
        let mut a = vec![0.0; t];
        let mut b = vec![0.0; t];
        let data = match d.interp {
            Interp::Linear => {
                let mut data = Vec::with_capacity((l + 1) * t);
                for m in 0..=l {
                    row(d, m as f64 / l as f64, &mut a);
                    data.extend(a.iter().map(|&v| v as f32));
                }
                data
            }
            Interp::Hermite => {
                let mut data = Vec::with_capacity(l * 4 * t);
                let mut da = vec![0.0; t];
                let mut db = vec![0.0; t];
                let mut lo = vec![0.0; t];
                let mut hi = vec![0.0; t];
                let delta = 1e-6;
                let deriv = |mu: f64, out: &mut [f64], lo: &mut [f64], hi: &mut [f64]| {
                    row(d, mu - delta, lo);
                    row(d, mu + delta, hi);
                    for k in 0..t {
                        // per segment step: d/dmu * (1/L)
                        out[k] = (hi[k] - lo[k]) / (2.0 * delta) / l as f64;
                    }
                };
                for m in 0..l {
                    let mu0 = m as f64 / l as f64;
                    let mu1 = (m + 1) as f64 / l as f64;
                    row(d, mu0, &mut a);
                    row(d, mu1, &mut b);
                    deriv(mu0, &mut da, &mut lo, &mut hi);
                    deriv(mu1, &mut db, &mut lo, &mut hi);
                    let c0: Vec<f64> = a.clone();
                    let c1: Vec<f64> = da.clone();
                    let c2: Vec<f64> = (0..t).map(|k| 3.0 * (b[k] - a[k]) - 2.0 * da[k] - db[k]).collect();
                    let c3: Vec<f64> = (0..t).map(|k| 2.0 * (a[k] - b[k]) + da[k] + db[k]).collect();
                    for c in [&c0, &c1, &c2, &c3] {
                        data.extend(c.iter().map(|&v| v as f32));
                    }
                }
                data
            }
        };
        CoefTable {
            taps: t,
            log2_segments: l.trailing_zeros(),
            interp: d.interp,
            data,
        }
    }

    pub fn size_bytes(&self) -> usize {
        self.data.len() * 4
    }

    /// Build the kernel for the Q0.32 fraction `frac` into `kern` (len T).
    #[inline(always)]
    fn kernel(&self, frac: u32, kern: &mut [f32]) {
        let t = self.taps;
        let wide = (frac as u64) << self.log2_segments;
        let m = (wide >> 32) as usize;
        let f = (wide as u32) as f32 * (1.0 / FRAC_ONE as f32);
        match self.interp {
            Interp::Linear => {
                let base = m * t;
                let a = &self.data[base..base + t];
                let b = &self.data[base + t..base + 2 * t];
                for ((o, &a), &b) in kern.iter_mut().zip(a).zip(b) {
                    *o = fmadd(f, b - a, a);
                }
            }
            Interp::Hermite => {
                let base = m * 4 * t;
                let seg = &self.data[base..base + 4 * t];
                let (c0, rest) = seg.split_at(t);
                let (c1, rest) = rest.split_at(t);
                let (c2, c3) = rest.split_at(t);
                let kern = &mut kern[..t];
                for ((((o, &a0), &a1), &a2), &a3) in kern.iter_mut().zip(c0).zip(c1).zip(c2).zip(c3) {
                    let v = fmadd(f, a3, a2);
                    let v = fmadd(f, v, a1);
                    *o = fmadd(f, v, a0);
                }
            }
        }
    }
}

/// `a * b + c`, fused when the target has FMA (aarch64 always does). Without
/// FMA, `mul_add` would call libm, so fall back to separate ops.
#[inline(always)]
pub fn fmadd(a: f32, b: f32, c: f32) -> f32 {
    #[cfg(any(target_feature = "fma", target_arch = "aarch64"))]
    {
        a.mul_add(b, c)
    }
    #[cfg(not(any(target_feature = "fma", target_arch = "aarch64")))]
    {
        a * b + c
    }
}

/// Four lanes, written so that LLVM's SLP vectoriser maps it onto one
/// 128-bit register (SSE2 / NEON) or half an AVX register.
#[derive(Clone, Copy)]
#[repr(align(16))]
struct F4([f32; 4]);

impl F4 {
    const ZERO: F4 = F4([0.0; 4]);
    #[inline(always)]
    fn fma(self, h: f32, x: &[f32]) -> F4 {
        let x: &[f32; 4] = x[..4].try_into().unwrap();
        F4([
            fmadd(h, x[0], self.0[0]),
            fmadd(h, x[1], self.0[1]),
            fmadd(h, x[2], self.0[2]),
            fmadd(h, x[3], self.0[3]),
        ])
    }
    #[inline(always)]
    fn add(self, o: F4) -> F4 {
        F4([self.0[0] + o.0[0], self.0[1] + o.0[1], self.0[2] + o.0[2], self.0[3] + o.0[3]])
    }
}

/// `G` groups of 4 channels starting at channel `cb`, two accumulators per
/// group (even/odd taps) to halve the FMA dependency chain.
#[inline(always)]
fn block4<const G: usize>(kern: &[f32], win: &[f32], c: usize, cb: usize, out: &mut [f32]) {
    let mut acc0 = [F4::ZERO; G];
    let mut acc1 = [F4::ZERO; G];
    let mut rows = win.chunks_exact(c);
    for h in kern.chunks_exact(2) {
        let r0 = &rows.next().unwrap()[cb..cb + 4 * G];
        let r1 = &rows.next().unwrap()[cb..cb + 4 * G];
        for g in 0..G {
            acc0[g] = acc0[g].fma(h[0], &r0[4 * g..]);
            acc1[g] = acc1[g].fma(h[1], &r1[4 * g..]);
        }
    }
    for g in 0..G {
        out[cb + 4 * g..cb + 4 * g + 4].copy_from_slice(&acc0[g].add(acc1[g]).0);
    }
}

/// Fewer than 4 channels left: scalar, two accumulators.
#[inline(always)]
fn block_scalar(kern: &[f32], win: &[f32], c: usize, ch: usize, out: &mut [f32]) {
    let (mut a0, mut a1) = (0.0f32, 0.0f32);
    let mut rows = win.chunks_exact(c);
    for h in kern.chunks_exact(2) {
        a0 = fmadd(h[0], rows.next().unwrap()[ch], a0);
        a1 = fmadd(h[1], rows.next().unwrap()[ch], a1);
    }
    out[ch] = a0 + a1;
}

/// Channel groups of 16/8/4 lanes, then scalar. Each group keeps its
/// accumulators in registers across the taps.
#[inline(always)]
fn dot_frame(kern: &[f32], win: &[f32], c: usize, out: &mut [f32]) {
    let mut cb = 0;
    while cb + 16 <= c {
        block4::<4>(kern, win, c, cb, out);
        cb += 16;
    }
    if cb + 8 <= c {
        block4::<2>(kern, win, c, cb, out);
        cb += 8;
    }
    if cb + 4 <= c {
        block4::<1>(kern, win, c, cb, out);
        cb += 4;
    }
    while cb < c {
        block_scalar(kern, win, c, cb, out);
        cb += 1;
    }
}

/// Exact input position: integer frame index plus a Q0.32 fraction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    pub frame: i64,
    pub frac: u32,
}

impl Position {
    pub fn as_f64(self) -> f64 {
        self.frame as f64 + self.frac as f64 / FRAC_ONE
    }
    fn q(self) -> i128 {
        ((self.frame as i128) << 32) | self.frac as i128
    }
    fn from_q(q: i128) -> Self {
        Position {
            frame: (q >> 32) as i64,
            frac: (q & 0xffff_ffff) as u32,
        }
    }
}

pub struct VarResampler {
    ch: usize,
    taps: usize,
    half: usize,
    table: CoefTable,
    kern: Vec<f32>,
    /// Interleaved history; frame `j` (absolute) lives at `(j - hist_start) * ch`.
    hist: Vec<f32>,
    hist_start: i64,
    /// One past the last absolute frame pulled from the ring.
    consumed: i64,
    pos: Position,
    /// Q32.32 input frames per output frame.
    step: u64,
    plan_n: usize,
    plan_dstep: i64,
    plan_target: u64,
    pending: usize,
    max_out: usize,
}

impl VarResampler {
    /// `max_out`: largest N per call; `max_step`: largest ratio (input frames
    /// per output frame) ever requested, e.g. 1.01.
    pub fn new(ch: usize, design: &Design, max_out: usize, max_step: f64) -> Self {
        let table = CoefTable::new(design);
        let taps = design.taps;
        let cap_frames = taps + (max_out as f64 * max_step).ceil() as usize + 4;
        let mut r = VarResampler {
            ch,
            taps,
            half: taps / 2,
            table,
            kern: vec![0.0; taps],
            hist: vec![0.0; cap_frames * ch],
            hist_start: 0,
            consumed: 0,
            pos: Position { frame: 0, frac: 0 },
            step: 1 << 32,
            plan_n: 0,
            plan_dstep: 0,
            plan_target: 1 << 32,
            pending: 0,
            max_out,
        };
        r.reset();
        r
    }

    /// Back to position 0 with `T/2 - 1` frames of silence before frame 0.
    pub fn reset(&mut self) {
        self.hist.fill(0.0);
        self.hist_start = -(self.half as i64 - 1);
        self.consumed = 0;
        self.pos = Position { frame: 0, frac: 0 };
        self.plan_n = 0;
        self.pending = 0;
    }

    pub fn set_step(&mut self, ratio: f64) {
        self.step = (ratio * FRAC_ONE).round() as u64;
    }

    /// Position (in input frames) of the next output frame. This is the
    /// "played" counter: every input frame before it has been fully emitted.
    pub fn position(&self) -> Position {
        self.pos
    }

    /// Frames pulled from the ring so far.
    pub fn consumed(&self) -> i64 {
        self.consumed
    }

    /// Constant look-ahead between consumption and the played position
    /// (`consumed - position` is in `(T/2, T/2 + 1]` right after `prepare`).
    pub fn group_delay_frames(&self) -> usize {
        self.half
    }

    pub fn table_bytes(&self) -> usize {
        self.table.size_bytes()
    }

    pub fn current_step(&self) -> f64 {
        self.step as f64 / FRAC_ONE
    }

    /// Plan a call producing `n_out` frames while the step ramps linearly to
    /// `ratio`. Returns how many input frames must be written into
    /// [`input_slot`](Self::input_slot) before [`render`](Self::render).
    pub fn prepare(&mut self, n_out: usize, ratio: f64) -> usize {
        assert!(n_out >= 1 && n_out <= self.max_out);
        let target = (ratio * FRAC_ONE).round() as i64;
        let dstep = (target - self.step as i64) / n_out as i64;
        // Position of the last output frame: p0 + sum_{n=0}^{N-2} (s + (n+1) d)
        let m = (n_out - 1) as i128;
        let last = self.pos.q() + m * self.step as i128 + dstep as i128 * m * (m + 1) / 2;
        let need_end = (last >> 32) as i64 + self.half as i64 + 1;
        self.pending = (need_end - self.consumed).max(0) as usize;
        self.plan_n = n_out;
        self.plan_dstep = dstep;
        self.plan_target = target as u64;
        self.pending
    }

    /// Positions (input frames) that a `prepare(n_out, ratio)` + `render`
    /// would assign to each output frame. Pure; for tests and harnesses.
    pub fn planned_positions(&self, n_out: usize, ratio: f64, out: &mut Vec<f64>) {
        let target = (ratio * FRAC_ONE).round() as i64;
        let dstep = (target - self.step as i64) / n_out as i64;
        let mut step = self.step as i64;
        let mut q = self.pos.q();
        for _ in 0..n_out {
            out.push(Position::from_q(q).as_f64());
            step += dstep;
            q += step as i128;
        }
    }

    /// Where the ring reader writes the `pending` frames (interleaved).
    pub fn input_slot(&mut self) -> &mut [f32] {
        let off = (self.consumed - self.hist_start) as usize * self.ch;
        &mut self.hist[off..off + self.pending * self.ch]
    }

    /// Produce exactly the planned `n_out` frames into `out` (interleaved).
    pub fn render(&mut self, out: &mut [f32]) {
        let c = self.ch;
        let t = self.taps;
        let n_out = self.plan_n;
        assert!(out.len() >= n_out * c);
        self.consumed += self.pending as i64;
        self.pending = 0;
        let mut step = self.step as i64;
        let dstep = self.plan_dstep;
        let mut q = self.pos.q();
        let half = self.half as i64;
        for (n, frame) in out.chunks_exact_mut(c).take(n_out).enumerate() {
            let _ = n;
            let i = (q >> 32) as i64;
            let frac = (q & 0xffff_ffff) as u32;
            let h0 = (i - half + 1 - self.hist_start) as usize * c;
            self.table.kernel(frac, &mut self.kern);
            dot_frame(&self.kern, &self.hist[h0..h0 + t * c], c, frame);
            step += dstep;
            q += step as i128;
        }
        // The truncated per-frame increment can leave the ramp up to N units
        // of 2^-32 short of the target; snap so the next call starts on it.
        // The position stays exact: it was integrated from the steps used.
        let _ = step;
        self.step = self.plan_target;
        self.pos = Position::from_q(q);
        // Keep only what the next window can still reach.
        let keep_from = self.pos.frame - half + 1;
        let drop = (keep_from - self.hist_start).max(0) as usize;
        if drop > 0 {
            let live = (self.consumed - self.hist_start) as usize;
            self.hist.copy_within(drop * c..live * c, 0);
            self.hist_start += drop as i64;
        }
    }

    /// Convenience for tests: pull from a closure that fills the slot.
    pub fn process(&mut self, n_out: usize, ratio: f64, fill: impl FnOnce(&mut [f32]), out: &mut [f32]) {
        self.prepare(n_out, ratio);
        fill(self.input_slot());
        self.render(out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unity_ratio_phase_zero_is_bit_exact() {
        let mut r = VarResampler::new(3, &Design::RECOMMENDED, 512, 1.01);
        let input: Vec<f32> = (0..3 * 10_000).map(|i| ((i * 7919) % 1000) as f32 / 1000.0 - 0.5).collect();
        let mut cur = 0;
        let mut out = vec![0.0; 3 * 256];
        let mut all = Vec::new();
        for _ in 0..30 {
            r.process(
                256,
                1.0,
                |s| {
                    s.copy_from_slice(&input[cur..cur + s.len()]);
                    cur += s.len();
                },
                &mut out,
            );
            all.extend_from_slice(&out);
        }
        assert_eq!(&all[..], &input[..all.len()]);
    }

    #[test]
    fn position_matches_closed_form() {
        let mut r = VarResampler::new(2, &Design::RECOMMENDED, 4096, 1.01);
        let mut expect: i128 = 0;
        let mut step: i64 = 1 << 32;
        let ns = [64usize, 4096, 1000, 257, 1];
        let ratios = [1.0005, 0.998, 1.000_01, 1.002, 1.0];
        let mut out = vec![0.0; 2 * 4096];
        for it in 0..200 {
            let n = ns[it % ns.len()];
            let ratio = ratios[it % ratios.len()];
            let need = r.prepare(n, ratio);
            r.input_slot().fill(0.25);
            r.render(&mut out);
            let target = (ratio * FRAC_ONE).round() as i64;
            let d = (target - step) / n as i64;
            for _ in 0..n {
                step += d;
                expect += step as i128;
            }
            step = target;
            assert_eq!(r.position().q(), expect);
            // consumption front stays T/2 (+1) ahead of the played position
            let ahead = r.consumed() as f64 - r.position().as_f64();
            assert!(ahead > 0.0 && ahead <= 34.0 + 3.0, "ahead {ahead} need {need}");
        }
    }
}
