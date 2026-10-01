//! Variable-ratio resampler: polyphase windowed-sinc fractional-delay FIR,
//! interleaved multichannel, exactly N output frames per call, exact
//! fixed-point source position.
//!
//! # Model
//!
//! The input is a sequence of frames `x[j]` (`j` is the absolute ring frame
//! index). Each output frame is the band-limited value `x(p)` at a fractional
//! source position `p`, from a `T`-tap kernel centred on `p`:
//!
//! ```text
//! i = floor(p), mu = p - i
//! y = sum_{k=0}^{T-1} c_k(mu) * x[i - T/2 + 1 + k]
//! c_k(mu) = h(mu + T/2 - 1 - k),  h(t) = fc * sinc(fc t) * kaiser(t / (T/2))
//! ```
//!
//! So `p` *is* the played position, with no delay on top: the servo's
//! `N_play` is [`DriftResampler::position`]. To produce frame `p` the
//! resampler must have read up to `i + T/2`: it runs `T/2` frames ahead of
//! the played position in the ring ([`DriftResampler::lookahead_frames`]).
//!
//! `p` advances by the step `s` (source frames per output frame) after each
//! output frame. `p` and `s` are Q32.32 fixed point, so the accumulated
//! position is exact and reproducible. A new ratio is reached by a linear ramp
//! of the step across one call, so there is no step discontinuity.
//!
//! # Coefficient table
//!
//! `c_k(mu)` is tabulated once on `L` segments of `mu`, each a cubic Hermite
//! fitted to the values and the exact derivatives at both ends (4 rows of `T`,
//! Horner form). At 64 taps and `L = 32` that is 32 KiB and reaches the `f32`
//! floor (spike S3: −139 dB THD+N at 1 kHz, −135 dB at 20 kHz). Every row is
//! normalised to unit DC gain; with the cutoff at the input Nyquist the
//! `mu = 0` row is an exact impulse, so ratio 1 at phase 0 is bit-transparent.
//!
//! # Realtime use
//!
//! ```text
//! let need = rs.prepare(n_out, ratio);        // frames to read from the ring
//! if !ring.read_exact(rs.input_slot()) { ... } // underrun: handle first
//! rs.render(out);                              // exactly n_out frames
//! ```
//!
//! Nothing allocates or panics after construction: out-of-range requests are
//! clamped (`n_out` to the construction maximum, the ratio to the range the
//! history was sized for).

use std::f64::consts::PI;

const FRAC_ONE: f64 = 4_294_967_296.0; // 2^32

/// Kernel design. See [`Design::for_rates`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Design {
    /// Kernel length `T` (even, ≥ 4).
    pub taps: usize,
    /// Number of `mu` segments `L` (power of two).
    pub segments: usize,
    /// Kaiser window β.
    pub beta: f64,
    /// Cutoff relative to the input Nyquist (1.0 = `fs_in / 2`).
    pub cutoff: f64,
}

impl Design {
    /// Drift-only at ≤ 48 kHz: 64 taps, Kaiser β 14, cutoff at the input
    /// Nyquist (transition 20 → 28 kHz at 48 kHz), 32 Hermite segments.
    pub const DRIFT_48K: Design = Design {
        taps: 64,
        segments: 32,
        beta: 14.0,
        cutoff: 1.0,
    };

    /// The design table of spike S3 (§6).
    ///
    /// - Equal rates up to 48 kHz: [`DRIFT_48K`](Self::DRIFT_48K).
    /// - Equal rates at 88.2 kHz and up: 32 taps (the transition band is
    ///   ultrasonic, so a shorter kernel loses nothing audible).
    /// - Any conversion: 96 taps, cutoff `min(1, fs_out / fs_in)`.
    pub fn for_rates(fs_in: u32, fs_out: u32) -> Self {
        if fs_in == fs_out {
            if fs_in >= 88_200 {
                Design {
                    taps: 32,
                    ..Self::DRIFT_48K
                }
            } else {
                Self::DRIFT_48K
            }
        } else {
            Design {
                taps: 96,
                cutoff: (fs_out as f64 / fs_in.max(1) as f64).min(1.0),
                ..Self::DRIFT_48K
            }
        }
    }

    /// The cheaper option of S3 for constrained CPUs: 48 taps, β 12.
    pub const CONSTRAINED: Design = Design {
        taps: 48,
        segments: 32,
        beta: 12.0,
        cutoff: 1.0,
    };

    fn sanitised(self) -> Self {
        Design {
            taps: (self.taps.max(4) + 1) & !1,
            segments: self.segments.max(1).next_power_of_two(),
            beta: self.beta,
            cutoff: self.cutoff.clamp(0.01, 1.0),
        }
    }
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    let mut k = 1.0;
    while term >= sum * 1e-17 {
        term *= q / (k * k);
        sum += term;
        k += 1.0;
    }
    sum
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else if x == x.round() {
        // Exact zero crossings, so the `mu = 0` row is an exact impulse.
        0.0
    } else {
        let px = PI * x;
        px.sin() / px
    }
}

/// One DC-normalised kernel row for fractional position `mu`.
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

/// `L` segments × 4 Hermite rows × `T` taps.
struct CoefTable {
    taps: usize,
    log2_segments: u32,
    data: Vec<f32>,
}

impl CoefTable {
    fn new(d: &Design) -> Self {
        let t = d.taps;
        let l = d.segments;
        let mut data = Vec::with_capacity(l * 4 * t);
        let (mut a, mut b) = (vec![0.0; t], vec![0.0; t]);
        let (mut da, mut db) = (vec![0.0; t], vec![0.0; t]);
        let (mut lo, mut hi) = (vec![0.0; t], vec![0.0; t]);
        let delta = 1e-6;
        let mut deriv = |mu: f64, out: &mut [f64]| {
            row(d, mu - delta, &mut lo);
            row(d, mu + delta, &mut hi);
            for k in 0..t {
                // d/dmu scaled to one segment.
                out[k] = (hi[k] - lo[k]) / (2.0 * delta) / l as f64;
            }
        };
        for m in 0..l {
            row(d, m as f64 / l as f64, &mut a);
            row(d, (m + 1) as f64 / l as f64, &mut b);
            deriv(m as f64 / l as f64, &mut da);
            deriv((m + 1) as f64 / l as f64, &mut db);
            data.extend(a.iter().map(|&v| v as f32));
            data.extend(da.iter().map(|&v| v as f32));
            data.extend((0..t).map(|k| (3.0 * (b[k] - a[k]) - 2.0 * da[k] - db[k]) as f32));
            data.extend((0..t).map(|k| (2.0 * (a[k] - b[k]) + da[k] + db[k]) as f32));
        }
        CoefTable {
            taps: t,
            log2_segments: l.trailing_zeros(),
            data,
        }
    }

    /// The kernel for the Q0.32 fraction `frac`, into `kern` (`T` long).
    #[inline(always)]
    fn kernel(&self, frac: u32, kern: &mut [f32]) {
        let t = self.taps;
        let wide = (frac as u64) << self.log2_segments;
        let m = (wide >> 32) as usize;
        let f = (wide as u32) as f32 * (1.0 / FRAC_ONE as f32);
        let seg = &self.data[m * 4 * t..(m + 1) * 4 * t];
        let (c0, rest) = seg.split_at(t);
        let (c1, rest) = rest.split_at(t);
        let (c2, c3) = rest.split_at(t);
        for ((((o, &a0), &a1), &a2), &a3) in kern[..t].iter_mut().zip(c0).zip(c1).zip(c2).zip(c3) {
            let v = fmadd(f, a3, a2);
            let v = fmadd(f, v, a1);
            *o = fmadd(f, v, a0);
        }
    }
}

/// `a * b + c`, fused when the target has FMA (aarch64 always does).
/// Without it, `mul_add` would call libm, so the ops stay separate.
#[inline(always)]
fn fmadd(a: f32, b: f32, c: f32) -> f32 {
    #[cfg(any(target_feature = "fma", target_arch = "aarch64"))]
    {
        a.mul_add(b, c)
    }
    #[cfg(not(any(target_feature = "fma", target_arch = "aarch64")))]
    {
        a * b + c
    }
}

/// Four lanes, shaped so LLVM's SLP vectoriser maps them onto one 128-bit
/// register (SSE2 / NEON) without `unsafe` intrinsics.
#[derive(Clone, Copy)]
#[repr(align(16))]
struct F4([f32; 4]);

impl F4 {
    const ZERO: F4 = F4([0.0; 4]);

    #[inline(always)]
    fn fma(self, h: f32, x: &[f32]) -> F4 {
        F4([
            fmadd(h, x[0], self.0[0]),
            fmadd(h, x[1], self.0[1]),
            fmadd(h, x[2], self.0[2]),
            fmadd(h, x[3], self.0[3]),
        ])
    }

    #[inline(always)]
    fn add(self, o: F4) -> F4 {
        F4([
            self.0[0] + o.0[0],
            self.0[1] + o.0[1],
            self.0[2] + o.0[2],
            self.0[3] + o.0[3],
        ])
    }
}

/// `G` groups of 4 channels from channel `cb`, with even/odd tap
/// accumulators to halve the FMA dependency chain.
#[inline(always)]
fn block4<const G: usize>(kern: &[f32], win: &[f32], c: usize, cb: usize, out: &mut [f32]) {
    let mut acc0 = [F4::ZERO; G];
    let mut acc1 = [F4::ZERO; G];
    let mut rows = win.chunks_exact(c);
    for h in kern.chunks_exact(2) {
        let (Some(r0), Some(r1)) = (rows.next(), rows.next()) else {
            break;
        };
        let r0 = &r0[cb..cb + 4 * G];
        let r1 = &r1[cb..cb + 4 * G];
        for g in 0..G {
            acc0[g] = acc0[g].fma(h[0], &r0[4 * g..4 * g + 4]);
            acc1[g] = acc1[g].fma(h[1], &r1[4 * g..4 * g + 4]);
        }
    }
    for g in 0..G {
        out[cb + 4 * g..cb + 4 * g + 4].copy_from_slice(&acc0[g].add(acc1[g]).0);
    }
}

#[inline(always)]
fn block_scalar(kern: &[f32], win: &[f32], c: usize, ch: usize, out: &mut [f32]) {
    let (mut a0, mut a1) = (0.0f32, 0.0f32);
    let mut rows = win.chunks_exact(c);
    for h in kern.chunks_exact(2) {
        let (Some(r0), Some(r1)) = (rows.next(), rows.next()) else {
            break;
        };
        a0 = fmadd(h[0], r0[ch], a0);
        a1 = fmadd(h[1], r1[ch], a1);
    }
    out[ch] = a0 + a1;
}

/// One output frame: channel groups of 16/8/4 lanes, then scalar.
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

/// Exact source position: absolute frame index plus a Q0.32 fraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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

/// See the [module docs](self).
pub struct DriftResampler {
    channels: usize,
    taps: usize,
    half: usize,
    table: CoefTable,
    kern: Vec<f32>,
    /// Interleaved history: absolute frame `j` lives at
    /// `(j - hist_start) * channels`.
    hist: Vec<f32>,
    hist_start: i64,
    /// One past the last frame taken from the ring.
    consumed: i64,
    pos: Position,
    /// Q32.32 source frames per output frame.
    step: u64,
    plan_n: usize,
    plan_dstep: i64,
    plan_target: u64,
    pending: usize,
    max_out: usize,
    max_step_q: u64,
}

impl DriftResampler {
    /// `max_out`: the largest callback ever requested; `max_step`: the largest
    /// ratio (source frames per output frame) ever requested, e.g. 1.01, or
    /// `fs_in / fs_out` plus margin for a conversion. Everything is allocated
    /// here.
    pub fn new(channels: usize, design: &Design, max_out: usize, max_step: f64) -> Self {
        let design = design.sanitised();
        let channels = channels.max(1);
        let max_out = max_out.max(1);
        let max_step = max_step.max(1.0);
        let taps = design.taps;
        let cap_frames = taps + (max_out as f64 * max_step).ceil() as usize + 4;
        let mut rs = DriftResampler {
            channels,
            taps,
            half: taps / 2,
            table: CoefTable::new(&design),
            kern: vec![0.0; taps],
            hist: vec![0.0; cap_frames * channels],
            hist_start: 0,
            consumed: 0,
            pos: Position { frame: 0, frac: 0 },
            step: 1 << 32,
            plan_n: 0,
            plan_dstep: 0,
            plan_target: 1 << 32,
            pending: 0,
            max_out,
            max_step_q: (max_step * FRAC_ONE) as u64,
        };
        rs.reset(0);
        rs
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Restart at absolute ring frame `at` with a silent history: the next
    /// output frame is source frame `at`, and the next frame read from the
    /// ring must be frame `at`.
    pub fn reset(&mut self, at: i64) {
        self.hist.fill(0.0);
        self.hist_start = at - (self.half as i64 - 1);
        self.consumed = at;
        self.pos = Position { frame: at, frac: 0 };
        self.plan_n = 0;
        self.pending = 0;
    }

    /// Set the ratio at once (no ramp), e.g. before the first call.
    pub fn set_ratio(&mut self, ratio: f64) {
        self.step = self.ratio_q(ratio);
    }

    /// Current ratio (source frames per output frame).
    pub fn ratio(&self) -> f64 {
        self.step as f64 / FRAC_ONE
    }

    /// Source position of the next output frame: every source frame before it
    /// has been played. This is `N_play`.
    pub fn position(&self) -> Position {
        self.pos
    }

    /// One past the last frame taken from the ring.
    pub fn consumed(&self) -> i64 {
        self.consumed
    }

    /// Frames the resampler reads ahead of [`position`](Self::position).
    pub fn lookahead_frames(&self) -> usize {
        self.half
    }

    /// Size of the coefficient table.
    pub fn table_bytes(&self) -> usize {
        self.table.data.len() * std::mem::size_of::<f32>()
    }

    fn ratio_q(&self, ratio: f64) -> u64 {
        let q = (ratio.max(1.0 / 1024.0) * FRAC_ONE).round() as u64;
        q.min(self.max_step_q)
    }

    /// Jump the position forward by `frames` source frames (fractional
    /// allowed), as the servo's start/realign rule asks. Returns how many
    /// frames the caller must [`discard`](crate::Consumer::discard) from the
    /// ring before the next [`prepare`](Self::prepare): those the jump passes
    /// over entirely, which never need to enter the history.
    pub fn skip(&mut self, frames: f64) -> usize {
        if frames.is_nan() || frames <= 0.0 {
            return 0;
        }
        let q = self.pos.q() + (frames * FRAC_ONE).round() as i128;
        self.pos = Position::from_q(q);
        let keep_from = self.pos.frame - self.half as i64 + 1;
        if keep_from > self.consumed {
            let discard = (keep_from - self.consumed) as usize;
            self.consumed = keep_from;
            self.hist_start = keep_from;
            discard
        } else {
            self.drop_history_before(keep_from);
            0
        }
    }

    /// Plan a call producing `n_out` frames while the ratio ramps linearly to
    /// `ratio`. Returns how many ring frames must be written into
    /// [`input_slot`](Self::input_slot) before [`render`](Self::render).
    pub fn prepare(&mut self, n_out: usize, ratio: f64) -> usize {
        let n_out = n_out.clamp(1, self.max_out);
        let target = self.ratio_q(ratio) as i64;
        let dstep = (target - self.step as i64) / n_out as i64;
        // Position of the last output frame:
        // p0 + sum_{n=1}^{N-1} (s + n·d) = p0 + (N-1)·s + d·(N-1)·N/2.
        let m = (n_out - 1) as i128;
        let last = self.pos.q() + m * self.step as i128 + dstep as i128 * m * (m + 1) / 2;
        let need_end = (last >> 32) as i64 + self.half as i64 + 1;
        self.pending = (need_end - self.consumed).max(0) as usize;
        self.plan_n = n_out;
        self.plan_dstep = dstep;
        self.plan_target = target as u64;
        self.pending
    }

    /// Where the ring reader copies the frames [`prepare`](Self::prepare)
    /// asked for (interleaved).
    pub fn input_slot(&mut self) -> &mut [f32] {
        let off = (self.consumed - self.hist_start) as usize * self.channels;
        let end = (off + self.pending * self.channels).min(self.hist.len());
        &mut self.hist[off.min(end)..end]
    }

    /// Produce the planned frames into `out` (interleaved; extra room is left
    /// untouched). Returns the number of frames written.
    pub fn render(&mut self, out: &mut [f32]) -> usize {
        let c = self.channels;
        let t = self.taps;
        let n_out = self.plan_n.min(out.len() / c);
        self.consumed += self.pending as i64;
        self.pending = 0;
        let mut step = self.step as i64;
        let mut q = self.pos.q();
        let half = self.half as i64;
        for frame in out.chunks_exact_mut(c).take(n_out) {
            let i = (q >> 32) as i64;
            let frac = (q & 0xffff_ffff) as u32;
            let h0 = (i - half + 1 - self.hist_start) as usize * c;
            self.table.kernel(frac, &mut self.kern);
            dot_frame(&self.kern, &self.hist[h0..h0 + t * c], c, frame);
            step += self.plan_dstep;
            q += step as i128;
        }
        // The truncated per-frame increment can leave the ramp a few units of
        // 2^-32 short of the target; land on it so the next call starts
        // there. The position stays exact: it integrated the steps used.
        self.step = self.plan_target;
        self.pos = Position::from_q(q);
        self.plan_n = 0;
        let keep_from = self.pos.frame - half + 1;
        self.drop_history_before(keep_from);
        n_out
    }

    /// Drop history frames the next window can no longer reach.
    fn drop_history_before(&mut self, keep_from: i64) {
        let drop = (keep_from - self.hist_start).max(0) as usize;
        if drop == 0 {
            return;
        }
        let c = self.channels;
        let live = ((self.consumed - self.hist_start).max(0) as usize).min(self.hist.len() / c);
        let drop = drop.min(live);
        self.hist.copy_within(drop * c..live * c, 0);
        self.hist_start += drop as i64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(
        rs: &mut DriftResampler,
        input: &[f32],
        cursor: &mut usize,
        n: usize,
        ratio: f64,
        out: &mut [f32],
    ) {
        let need = rs.prepare(n, ratio);
        let c = rs.channels();
        let slot = rs.input_slot();
        slot.copy_from_slice(&input[*cursor..*cursor + need * c]);
        *cursor += need * c;
        rs.render(out);
    }

    #[test]
    fn unity_ratio_at_phase_zero_is_bit_exact() {
        let mut rs = DriftResampler::new(3, &Design::DRIFT_48K, 512, 1.01);
        let input: Vec<f32> = (0..3 * 10_000)
            .map(|i| ((i * 7919) % 1000) as f32 / 1000.0 - 0.5)
            .collect();
        let mut cur = 0;
        let mut out = vec![0.0; 3 * 256];
        let mut all = Vec::new();
        for _ in 0..30 {
            feed(&mut rs, &input, &mut cur, 256, 1.0, &mut out);
            all.extend_from_slice(&out);
        }
        assert_eq!(&all[..], &input[..all.len()]);
    }

    #[test]
    fn position_matches_the_closed_form() {
        let mut rs = DriftResampler::new(2, &Design::DRIFT_48K, 4096, 1.01);
        let mut expect: i128 = 0;
        let mut step: i64 = 1 << 32;
        let ns = [64usize, 4096, 1000, 257, 1];
        let ratios = [1.0005, 0.998, 1.000_01, 1.002, 1.0];
        let mut out = vec![0.0; 2 * 4096];
        for it in 0..200 {
            let (n, ratio) = (ns[it % ns.len()], ratios[it % ratios.len()]);
            rs.prepare(n, ratio);
            rs.input_slot().fill(0.25);
            assert_eq!(rs.render(&mut out), n);
            let target = (ratio * FRAC_ONE).round() as i64;
            let d = (target - step) / n as i64;
            for _ in 0..n {
                step += d;
                expect += step as i128;
            }
            step = target;
            assert_eq!(rs.position().q(), expect);
            let ahead = rs.consumed() as f64 - rs.position().as_f64();
            assert!(ahead > 0.0 && ahead <= 32.0 + 2.0, "ahead {ahead}");
        }
    }

    /// A skip lands the position exactly, discards what it passes over, and
    /// playback continues from the right source frame.
    #[test]
    fn skip_moves_the_position_and_discards_the_gap() {
        let mut rs = DriftResampler::new(1, &Design::DRIFT_48K, 256, 1.01);
        // A ramp: output value = source position, so the output tells us
        // which source frame plays.
        let input: Vec<f32> = (0..20_000).map(|i| i as f32).collect();
        let mut cur = 0usize;
        let mut out = vec![0.0; 256];
        feed(&mut rs, &input, &mut cur, 256, 1.0, &mut out);
        assert_eq!(rs.position().frame, 256);
        let discard = rs.skip(1000.0);
        assert_eq!(rs.position().frame, 1256);
        // The ring read front was 256 + 32 = 288; the window of 1256 starts at
        // 1256 − 31 = 1225.
        assert_eq!(discard, 1225 - 288);
        cur += discard;
        feed(&mut rs, &input, &mut cur, 256, 1.0, &mut out);
        assert_eq!(out[0], 1256.0);
        assert_eq!(out[255], 1511.0);
    }

    #[test]
    fn a_small_skip_stays_in_the_history() {
        let mut rs = DriftResampler::new(1, &Design::DRIFT_48K, 256, 1.01);
        let input: Vec<f32> = (0..20_000).map(|i| i as f32).collect();
        let mut cur = 0usize;
        let mut out = vec![0.0; 256];
        feed(&mut rs, &input, &mut cur, 256, 1.0, &mut out);
        assert_eq!(rs.skip(10.0), 0);
        feed(&mut rs, &input, &mut cur, 256, 1.0, &mut out);
        assert_eq!(out[0], 266.0);
    }

    #[test]
    fn out_of_range_requests_are_clamped_not_fatal() {
        let mut rs = DriftResampler::new(2, &Design::DRIFT_48K, 128, 1.01);
        // Too many frames, absurd ratio: clamped to 128 frames at 1.01.
        let need = rs.prepare(10_000, 50.0);
        assert!(need <= 128 * 2);
        rs.input_slot().fill(0.0);
        let mut out = vec![0.0; 2 * 10_000];
        assert_eq!(rs.render(&mut out), 128);
        assert!((rs.ratio() - 1.01).abs() < 1e-9);
        // A destination too small for the plan is not overrun.
        rs.prepare(128, 1.0);
        rs.input_slot().fill(0.0);
        let mut small = vec![0.0; 2 * 10];
        assert_eq!(rs.render(&mut small), 10);
    }

    #[test]
    fn designs_follow_the_rates() {
        assert_eq!(Design::for_rates(48_000, 48_000), Design::DRIFT_48K);
        assert_eq!(Design::for_rates(96_000, 96_000).taps, 32);
        let conv = Design::for_rates(48_000, 44_100);
        assert_eq!(conv.taps, 96);
        assert!((conv.cutoff - 44_100.0 / 48_000.0).abs() < 1e-12);
        assert_eq!(Design::for_rates(44_100, 48_000).cutoff, 1.0);
    }
}
