//! Direct-form FIR convolver for one ear of one object.
//!
//! Capacity is [`HRIR_LEN`]; the number of taps actually run is the length
//! of the kernel handed to [`set_coeffs`](EarConvolver::set_coeffs) /
//! [`set_coeffs_smooth`](EarConvolver::set_coeffs_smooth) — the set's
//! [`hrir_len`](super::hrir::hrir_len) for the engine rate — so the cost
//! scales with the rate rather than the span shrinking. The input history persists across
//! coefficient swaps, so updating the HRIR between frames (as the object or
//! head moves) keeps the *state* continuous — and a kernel change is
//! additionally crossfaded over a caller-chosen ramp
//! ([`set_coeffs_smooth`](EarConvolver::set_coeffs_smooth)): the old and new
//! kernels run over the same history and blend linearly, which is exactly
//! equivalent to interpolating the coefficients per sample, so the transfer
//! function moves without a block-boundary discontinuity (issue #155). The
//! doubled dot product is paid only during fade samples of blocks whose kernel
//! actually changed. Direct-form FIR is the simplest steady-state cost for
//! short kernels; a partitioned-FFT path can replace this if profiling on the
//! object count demands it.
//!
//! # Tap-loop layout
//!
//! The tap loop is the renderer's hottest inner loop — `len` multiply-adds
//! per sample *per ear per object* — so its shape is deliberate on four counts:
//!
//! * **The history is linear and double-written**, not a ring walked backwards.
//!   Each input lands at both `pos` and `pos + len`, which makes
//!   `hist[pos + 1 ..= pos + len]` the last `len` inputs in
//!   oldest→newest order, contiguous and ascending whatever `pos` is. The
//!   kernel is stored reversed to match, so the loop is a plain forward dot
//!   product over two contiguous slices — no wrap test and no descending index
//!   in the way of the load/store units.
//! * **The accumulation is split across [`ACC_LANES`] independent partial
//!   sums** (see that constant).
//! * **A crossfade is two plain dot products over the same window**, not one
//!   loop feeding both kernels' partial sums. The single-kernel loop is the
//!   shape the vectoriser turns into one multiply-add per vector; the
//!   two-kernel loop stays scalar and costs five dot products instead of two.
//!   Each lane of each kernel sees the same additions in the same order either
//!   way, so the split changes no bit.
//! * **Samples go through a block at a time**
//!   ([`process_block`](EarConvolver::process_block)) and the history stores
//!   stay out of the tap loop: the `pos + len` copies of a whole run are
//!   written before its first window is read, the `pos` copies after its last.
//!   A sample stored right ahead of its own window leaves the window's last
//!   vector load overlapping scalar stores that have not retired yet; such a
//!   load cannot be forwarded and waits for them, which chains each sample
//!   behind the previous one's horizontal sum — with 8-lane vectors, enough
//!   to hold a 128-tap kernel at the cost of a 256-tap one. With the stores
//!   hoisted, the windows of a block overlap freely in the out-of-order core.
//!
//! The block is also where the instruction set is chosen. On x86-64 the same
//! source is compiled a second time with AVX2 enabled — one 8-lane
//! multiply-add per chunk instead of two 4-lane ones — and picked per block
//! when the CPU has it; a `#[target_feature]` function cannot inline into the
//! caller, so a per-sample choice would pay a call on every sample. FMA stays
//! off: fusing the multiply-add rounds once where the baseline build rounds
//! twice, whereas plain AVX2 only widens the same operations. Both builds of
//! the loop therefore return the same bits, on every machine.
//!
//! None of this is a micro-optimisation: together they take the loop from
//! latency-bound scalar to throughput-bound. Splitting the accumulation is a
//! *reassociation* of `f32` additions, so its outputs sit at the ULP level of
//! a single running sum — which is exactly the cross-host noise the golden
//! gate is dimensioned for (`dsp_fixtures::golden::BINAURAL_RESIDUAL_GATE_DBFS`).

use super::hrir::HRIR_LEN;

/// Independent partial sums accumulated by the tap loop.
///
/// A single `acc += c * h` chain is bound by floating-point add/FMA *latency*
/// (~4-7 cycles per tap on both x86 and Cortex-A), not by throughput: the taps
/// serialise on one register. Splitting the accumulation into this many
/// independent chains keeps several multiply-adds in flight at once.
///
/// It has to be written in the source rather than left to the optimiser:
/// `f32` addition is not associative, so re-associating a plain reduction would
/// change results, and no compiler will do it without fast-math — on *any*
/// target. That matters for the embedded target in particular (issue #220): the
/// CoreELEC images for its Amlogic SoC run a 32-bit ARM userspace, where Rust
/// exposes no stable NEON intrinsics and AArch32's non-IEEE flush-to-zero
/// makes LLVM more conservative still, so this is the only vectorisation-shaped
/// win available there.
///
/// 8 covers the FMA latency on the in-order Cortex-A53 as well as on wide x86,
/// maps onto 2 NEON or SSE2 vectors or 1 AVX2 vector where those *are*
/// reachable, and divides `HRIR_LEN`.
const ACC_LANES: usize = 8;

const _: () = assert!(
    HRIR_LEN.is_multiple_of(ACC_LANES),
    "the tap loop consumes the kernel in whole ACC_LANES chunks"
);

/// Forward dot product of a reversed kernel with the ascending history window.
#[inline(always)]
fn dot(coeffs: &[f32], win: &[f32]) -> f32 {
    let mut acc = [0.0f32; ACC_LANES];
    for (c, h) in coeffs
        .chunks_exact(ACC_LANES)
        .zip(win.chunks_exact(ACC_LANES))
    {
        for l in 0..ACC_LANES {
            acc[l] += c[l] * h[l];
        }
    }
    acc.iter().sum()
}

pub struct EarConvolver {
    /// Past inputs, each written twice (`pos` and `pos + len`) so the
    /// live window is always a contiguous ascending slice. `pos` marks the
    /// primary slot of the most recent sample.
    hist: [f32; 2 * HRIR_LEN],
    pos: usize,
    /// Taps in use: the length of the last kernel set, `HRIR_LEN` until
    /// then. Fixed for the life of a stream — it decides where the history
    /// wraps, so it is adopted from the first kernel and expected to hold.
    len: usize,
    /// Whether any sample has been processed since the length was adopted.
    started: bool,
    /// Current kernel, stored **reversed** (`rcoeffs[j] == coeffs[len - 1 - j]`)
    /// to match the oldest→newest history window.
    rcoeffs: [f32; HRIR_LEN],
    /// Fade-out kernel of a running crossfade (valid while `fade_pos <
    /// fade_len`), reversed the same way.
    prev_rcoeffs: [f32; HRIR_LEN],
    fade_pos: u32,
    fade_len: u32,
}

impl Default for EarConvolver {
    fn default() -> Self {
        Self::new()
    }
}

impl EarConvolver {
    pub fn new() -> Self {
        Self {
            hist: [0.0; 2 * HRIR_LEN],
            pos: 0,
            len: HRIR_LEN,
            started: false,
            rcoeffs: [0.0; HRIR_LEN],
            prev_rcoeffs: [0.0; HRIR_LEN],
            fade_pos: 0,
            fade_len: 0,
        }
    }

    /// Taps currently run per sample.
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Never empty: a fresh convolver runs the full capacity of zeros.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Adopt the kernel length. The history wraps at `len`, so a change
    /// after samples have flowed would misalign the window; the length is
    /// a property of the engine rate and is expected to be set once, on the
    /// first kernel, before the first sample.
    #[inline]
    fn adopt_len(&mut self, len: usize) {
        if len == self.len {
            return;
        }
        debug_assert!(len > 0 && len <= HRIR_LEN && len.is_multiple_of(ACC_LANES));
        debug_assert!(
            !self.started,
            "kernel length changed from {} to {len} on a running convolver",
            self.len
        );
        self.len = len;
        self.pos = 0;
        self.hist.fill(0.0);
        self.fade_pos = 0;
        self.fade_len = 0;
    }

    /// Replace the FIR kernel immediately (no crossfade). The history is
    /// untouched, so the *state* remains continuous through the swap, but the
    /// transfer function jumps — prefer [`set_coeffs_smooth`](Self::set_coeffs_smooth)
    /// on live update paths. `coeffs.len()` is the number of taps to run.
    #[inline]
    pub fn set_coeffs(&mut self, coeffs: &[f32]) {
        self.adopt_len(coeffs.len());
        for (dst, &c) in self.rcoeffs.iter_mut().zip(coeffs.iter().rev()) {
            *dst = c;
        }
        self.fade_pos = 0;
        self.fade_len = 0;
    }

    /// Whether `coeffs` is the kernel already loaded (compared in natural
    /// order against the reversed store — no temporary).
    #[inline]
    fn kernel_is(&self, coeffs: &[f32]) -> bool {
        coeffs.len() == self.len && self.rcoeffs[..self.len].iter().eq(coeffs.iter().rev())
    }

    /// Replace the FIR kernel, crossfading from the current one over the next
    /// `fade_len` processed samples. A no-op when the kernel is unchanged (a
    /// static object under a static head — the common case — costs one array
    /// compare and keeps the single dot product). Restarting mid-fade departs
    /// from the currently *effective* (blended) kernel, so back-to-back
    /// changes stay click-free too.
    pub fn set_coeffs_smooth(&mut self, coeffs: &[f32], fade_len: usize) {
        if self.kernel_is(coeffs) {
            return;
        }
        if fade_len == 0 {
            self.set_coeffs(coeffs);
            return;
        }
        self.adopt_len(coeffs.len());
        let len = self.len;
        if self.fade_pos < self.fade_len {
            // Freeze the running blend as the new fade-out kernel.
            let w = self.fade_pos as f32 / self.fade_len as f32;
            for i in 0..len {
                self.prev_rcoeffs[i] += (self.rcoeffs[i] - self.prev_rcoeffs[i]) * w;
            }
        } else {
            self.prev_rcoeffs[..len].copy_from_slice(&self.rcoeffs[..len]);
        }
        for (dst, &c) in self.rcoeffs.iter_mut().zip(coeffs.iter().rev()) {
            *dst = c;
        }
        self.fade_pos = 0;
        self.fade_len = fade_len as u32;
    }

    /// Push one input sample and return the filtered output. A block of one:
    /// callers with a whole block in hand use
    /// [`process_block`](Self::process_block).
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let mut y = x;
        self.process_block(std::slice::from_mut(&mut y));
        y
    }

    /// Filter a block in place: `buf` holds the input samples on entry and
    /// the matching outputs on return. Any length; the result does not depend
    /// on how a signal is cut into blocks.
    pub fn process_block(&mut self, buf: &mut [f32]) {
        #[cfg(target_arch = "x86_64")]
        if std::arch::is_x86_feature_detected!("avx2") {
            // SAFETY: AVX2, the only feature `process_block_avx2` is compiled
            // with beyond the baseline, was detected on this CPU just above.
            unsafe { self.process_block_avx2(buf) };
            return;
        }
        self.process_block_portable(buf);
    }

    /// [`run_block`](Self::run_block) built for the crate's baseline target.
    fn process_block_portable(&mut self, buf: &mut [f32]) {
        self.run_block(buf);
    }

    /// [`run_block`](Self::run_block) built with AVX2: the same operations in
    /// the same order, eight lanes to a vector (see the module docs).
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    fn process_block_avx2(&mut self, buf: &mut [f32]) {
        self.run_block(buf);
    }

    /// The block loop, inlined into each of its builds.
    #[inline(always)]
    fn run_block(&mut self, mut buf: &mut [f32]) {
        let len = self.len;
        self.started |= !buf.is_empty();
        while !buf.is_empty() {
            // One run of samples per pass, cut where the history wraps.
            let start = if self.pos + 1 == len { 0 } else { self.pos + 1 };
            let run = buf.len().min(len - start);
            let (samples, rest) = buf.split_at_mut(run);
            // Upper copies of the whole run first: the window of sample `i`
            // ends on its own copy and reads none of the later ones.
            self.hist[start + len..start + len + run].copy_from_slice(samples);
            // `windows` hands out `hist[start + i + 1..][..len]` for each
            // sample `i` of the run: its last `len` inputs, oldest→newest.
            let windows = self.hist[start + 1..start + run + len].windows(len);
            let coeffs = &self.rcoeffs[..len];
            let prev_coeffs = &self.prev_rcoeffs[..len];
            for (y, win) in samples.iter_mut().zip(windows) {
                *y = if self.fade_pos < self.fade_len {
                    // Crossfade: both kernels over the shared history, linear
                    // blend.
                    self.fade_pos += 1;
                    let w = self.fade_pos as f32 / self.fade_len as f32;
                    let acc_new = dot(coeffs, win);
                    let acc_old = dot(prev_coeffs, win);
                    acc_old + (acc_new - acc_old) * w
                } else {
                    dot(coeffs, win)
                };
            }
            // Lower copies last: until here those slots held the inputs of one
            // lap back, which the windows above start on.
            self.hist.copy_within(start + len..start + len + run, start);
            self.pos = start + run - 1;
            buf = rest;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_impulse_kernel_is_passthrough() {
        let mut c = EarConvolver::new();
        let mut k = [0.0; HRIR_LEN];
        k[0] = 1.0;
        c.set_coeffs(&k);
        assert_eq!(c.process(0.5), 0.5);
        assert_eq!(c.process(-0.25), -0.25);
    }

    #[test]
    fn delayed_kernel_delays_signal() {
        let mut c = EarConvolver::new();
        let mut k = [0.0; HRIR_LEN];
        k[3] = 1.0; // 3-sample delay
        c.set_coeffs(&k);
        let xs = [1.0, 0.0, 0.0, 0.0, 0.0];
        let ys: Vec<f32> = xs.iter().map(|&x| c.process(x)).collect();
        assert_eq!(ys, vec![0.0, 0.0, 0.0, 1.0, 0.0]);
    }

    /// The last tap is the one the doubled history has to reach across the
    /// wrap, and `pos` visits every slot — so drive the impulse through a full
    /// lap and a bit more. A window that mirrors the wrong copy shows up here
    /// and nowhere else.
    #[test]
    fn last_tap_delays_by_full_kernel_length() {
        let mut c = EarConvolver::new();
        let mut k = [0.0; HRIR_LEN];
        k[HRIR_LEN - 1] = 1.0;
        c.set_coeffs(&k);
        // Impulse at t=0, then silence: the echo must come back exactly once,
        // at t = HRIR_LEN - 1, whatever lap of the buffer that lands on.
        let mut hits = Vec::new();
        for t in 0..(3 * HRIR_LEN) {
            let y = c.process(if t == 0 { 1.0 } else { 0.0 });
            if y != 0.0 {
                hits.push((t, y));
            }
        }
        assert_eq!(hits, vec![(HRIR_LEN - 1, 1.0)]);
    }

    /// Every tap index must map to its own delay — a full sweep catches an
    /// off-by-one in the reversed kernel that a single probe tap would miss.
    #[test]
    fn every_tap_maps_to_its_own_delay() {
        for tap in [0, 1, 2, 7, 8, 63, HRIR_LEN - 2, HRIR_LEN - 1] {
            let mut c = EarConvolver::new();
            let mut k = [0.0; HRIR_LEN];
            k[tap] = 1.0;
            c.set_coeffs(&k);
            let mut hits = Vec::new();
            for t in 0..(2 * HRIR_LEN) {
                let y = c.process(if t == 0 { 1.0 } else { 0.0 });
                if y != 0.0 {
                    hits.push(t);
                }
            }
            assert_eq!(hits, vec![tap], "tap {tap} landed at the wrong delay");
        }
    }

    /// A shorter kernel runs, wraps and delays on its own length: the last
    /// tap of a 256-tap kernel lands at 255, through several laps of the
    /// 256-slot history.
    #[test]
    fn a_shorter_kernel_wraps_on_its_own_length() {
        let mut c = EarConvolver::new();
        let mut k = [0.0; 256];
        k[255] = 1.0;
        c.set_coeffs(&k);
        assert_eq!(c.len(), 256);
        let mut hits = Vec::new();
        for t in 0..(3 * 256) {
            let y = c.process(if t == 0 { 1.0 } else { 0.0 });
            if y != 0.0 {
                hits.push((t, y));
            }
        }
        assert_eq!(hits, vec![(255, 1.0)]);
    }

    #[test]
    fn dc_gain_equals_coefficient_sum() {
        let mut c = EarConvolver::new();
        let mut k = [0.0; HRIR_LEN];
        k[0] = 0.5;
        k[1] = 0.25;
        c.set_coeffs(&k);
        // Drive DC; after the kernel fills, output settles at sum of coeffs.
        let mut y = 0.0;
        for _ in 0..HRIR_LEN {
            y = c.process(1.0);
        }
        assert!((y - 0.75).abs() < 1e-6);
    }

    /// A kernel change must ramp the output linearly over the fade instead of
    /// jumping at the swap (issue #155): DC through gain 1.0 → gain 0.5 with a
    /// 16-sample fade steps down by exactly 1/32 per sample.
    #[test]
    fn kernel_swap_crossfades_linearly() {
        let mut c = EarConvolver::new();
        let mut a = [0.0; HRIR_LEN];
        a[0] = 1.0;
        c.set_coeffs(&a);
        for _ in 0..HRIR_LEN {
            c.process(1.0); // settle at 1.0
        }
        let mut b = [0.0; HRIR_LEN];
        b[0] = 0.5;
        const FADE: usize = 16;
        c.set_coeffs_smooth(&b, FADE);
        let mut prev = 1.0f32;
        for i in 0..FADE {
            let y = c.process(1.0);
            let expected = 1.0 - 0.5 * (i + 1) as f32 / FADE as f32;
            assert!(
                (y - expected).abs() < 1e-6,
                "sample {i}: got {y}, expected {expected}"
            );
            assert!(y < prev, "fade must be monotonic");
            prev = y;
        }
        // Steady state on the new kernel afterwards.
        assert!((c.process(1.0) - 0.5).abs() < 1e-6);
    }

    /// Re-setting the same kernel must not restart a fade or change output.
    #[test]
    fn unchanged_kernel_is_a_no_op() {
        let mut c = EarConvolver::new();
        let mut k = [0.0; HRIR_LEN];
        k[0] = 1.0;
        c.set_coeffs(&k);
        for _ in 0..4 {
            c.process(1.0);
        }
        c.set_coeffs_smooth(&k, 16);
        assert_eq!(c.process(1.0), 1.0, "same kernel must stay transparent");
    }

    /// A second change mid-fade must depart from the blended kernel — the
    /// output stays inside the envelope of the kernels involved, no jump back.
    #[test]
    fn midfade_restart_stays_continuous() {
        let mut c = EarConvolver::new();
        let mut a = [0.0; HRIR_LEN];
        a[0] = 1.0;
        c.set_coeffs(&a);
        for _ in 0..HRIR_LEN {
            c.process(1.0);
        }
        let mut b = [0.0; HRIR_LEN];
        b[0] = 0.0; // fade toward silence
        c.set_coeffs_smooth(&b, 16);
        let mut y = 1.0;
        for _ in 0..8 {
            y = c.process(1.0); // half-way: ~0.5
        }
        assert!((y - 0.5).abs() < 1e-6);
        // Change again mid-fade, back to gain 1.0: must ramp 0.5 → 1.0.
        c.set_coeffs_smooth(&a, 16);
        let first = c.process(1.0);
        assert!(
            (first - 0.5).abs() < 0.1,
            "restart must depart from the blended kernel, got {first}"
        );
        for _ in 0..16 {
            y = c.process(1.0);
        }
        assert!((y - 1.0).abs() < 1e-6, "must settle on the new kernel");
    }

    /// Both kernels of a crossfade in one pass over the window, each feeding
    /// its own set of partial sums: the arithmetic a fade sample is defined
    /// by. The convolver computes it as two [`dot`] calls and has to return
    /// these bits.
    fn dot2_reference(new_c: &[f32], old_c: &[f32], win: &[f32]) -> (f32, f32) {
        let mut acc_new = [0.0f32; ACC_LANES];
        let mut acc_old = [0.0f32; ACC_LANES];
        for ((cn, co), h) in new_c
            .chunks_exact(ACC_LANES)
            .zip(old_c.chunks_exact(ACC_LANES))
            .zip(win.chunks_exact(ACC_LANES))
        {
            for l in 0..ACC_LANES {
                let hv = h[l];
                acc_new[l] += cn[l] * hv;
                acc_old[l] += co[l] * hv;
            }
        }
        (acc_new.iter().sum(), acc_old.iter().sum())
    }

    /// The single-kernel sum, tap by tap: tap `j` goes to partial sum
    /// `j % ACC_LANES`, in tap order.
    fn dot_reference(coeffs: &[f32], win: &[f32]) -> f32 {
        let mut acc = [0.0f32; ACC_LANES];
        for (j, (c, h)) in coeffs.iter().zip(win).enumerate() {
            acc[j % ACC_LANES] += c * h;
        }
        acc.iter().sum()
    }

    /// One sample through `c` the plain way: both history copies stored, then
    /// the window that ends on them read back, a fade sample going through
    /// [`dot2_reference`]. What [`EarConvolver::process_block`] has to match
    /// bit for bit, in its output and in the state it leaves.
    fn reference_sample(c: &mut EarConvolver, x: f32) -> f32 {
        let len = c.len;
        c.started = true;
        c.pos = if c.pos + 1 == len { 0 } else { c.pos + 1 };
        c.hist[c.pos] = x;
        c.hist[c.pos + len] = x;
        let win = &c.hist[c.pos + 1..c.pos + 1 + len];
        if c.fade_pos < c.fade_len {
            c.fade_pos += 1;
            let w = c.fade_pos as f32 / c.fade_len as f32;
            let (acc_new, acc_old) = dot2_reference(&c.rcoeffs[..len], &c.prev_rcoeffs[..len], win);
            acc_old + (acc_new - acc_old) * w
        } else {
            dot_reference(&c.rcoeffs[..len], win)
        }
    }

    /// Deterministic noise in [-1, 1).
    fn noise(seed: u64) -> f32 {
        let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        x ^= x >> 30;
        x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x ^= x >> 27;
        ((x >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }

    /// Kernel lengths the engine runs ([`hrir_len`](super::super::hrir::hrir_len)
    /// at 48, 88.2, 96, 176.4 and 192 kHz), and two short ones whose history
    /// wraps every few samples.
    const LENGTHS: [usize; 7] = [8, 24, 128, 240, 256, 472, 512];

    /// Runs noise through a `len`-tap convolver with `filter` processing each
    /// block, and returns the outputs with the final state.
    ///
    /// The blocks are uneven — a single sample, the live 40, one short of the
    /// kernel, the kernel, more than two laps of the history — and the kernel
    /// changes between them every way the engine can change it: over the
    /// block as `render_frame` does, over a fade that ends inside the block,
    /// over one that outlasts it and is then restarted mid-way, and at once.
    /// Returns the number of fade samples too, so a caller can tell the
    /// crossfade was exercised.
    fn exercise(
        len: usize,
        filter: impl Fn(&mut EarConvolver, &mut [f32]),
    ) -> (Vec<u32>, EarConvolver, usize) {
        let kernel = |n: u64| -> Vec<f32> {
            (0..len as u64)
                .map(|i| noise(n * 10_000 + i) * 0.25)
                .collect()
        };
        let sizes = [1, 2, 40, 7, len - 1, len, len + 1, 64, 2 * len + 3, 13];
        let mut c = EarConvolver::new();
        c.set_coeffs(&kernel(0));
        let mut out = Vec::new();
        let mut fade_samples = 0;
        let mut t = 0u64;
        for k in 0..80usize {
            let n = sizes[k % sizes.len()];
            let next = kernel(k as u64 + 1);
            match k % 7 {
                1 => c.set_coeffs_smooth(&next, n.min(len)),
                3 => c.set_coeffs_smooth(&next, 3 * n + 5),
                4 => c.set_coeffs_smooth(&next, 9),
                6 => c.set_coeffs_smooth(&next, (n / 2).max(1)),
                _ if k % 23 == 22 => c.set_coeffs(&next),
                _ => {}
            }
            fade_samples += ((c.fade_len - c.fade_pos) as usize).min(n);
            let mut block: Vec<f32> = (0..n as u64)
                .map(|i| noise(1_000_000 + t + i) * 0.5)
                .collect();
            t += n as u64;
            filter(&mut c, &mut block);
            out.extend(block.iter().map(|y| y.to_bits()));
        }
        (out, c, fade_samples)
    }

    /// Asserts two [`exercise`] runs agree bit for bit, outputs and state.
    fn assert_same_run(
        what: &str,
        len: usize,
        got: &(Vec<u32>, EarConvolver, usize),
        want: &(Vec<u32>, EarConvolver, usize),
    ) {
        let (out, c, _) = got;
        let (ref_out, ref_c, _) = want;
        assert_eq!(out.len(), ref_out.len());
        if let Some(i) = (0..out.len()).find(|&i| out[i] != ref_out[i]) {
            panic!(
                "{what}, {len} taps: sample {i} is {} ({:#010x}), expected {} ({:#010x})",
                f32::from_bits(out[i]),
                out[i],
                f32::from_bits(ref_out[i]),
                ref_out[i],
            );
        }
        let bits = |h: &[f32]| h.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
        assert_eq!(
            bits(&c.hist),
            bits(&ref_c.hist),
            "{what}, {len} taps: history"
        );
        assert_eq!(
            (c.pos, c.fade_pos, c.fade_len, c.started),
            (ref_c.pos, ref_c.fade_pos, ref_c.fade_len, ref_c.started),
            "{what}, {len} taps: position and fade state"
        );
    }

    fn reference_run(len: usize) -> (Vec<u32>, EarConvolver, usize) {
        exercise(len, |c, block| {
            for v in block {
                *v = reference_sample(c, *v);
            }
        })
    }

    /// A block — stores hoisted around the tap loop, a fade sample as two
    /// plain dot products — returns the bits of the sample-by-sample
    /// reference with its single-pass crossfade, whatever the kernel length,
    /// the block size and the place the history wraps, and leaves the same
    /// state behind. Checked for the dispatching entry, for the portable
    /// build of the loop on its own, and for blocks of one.
    #[test]
    fn blocks_match_the_per_sample_reference_bit_for_bit() {
        for len in LENGTHS {
            let want = reference_run(len);
            assert!(
                want.2 > want.0.len() / 4 && want.2 < want.0.len(),
                "{len} taps: {} fade samples out of {} — the run must mix both",
                want.2,
                want.0.len()
            );
            let got = exercise(len, |c, block| c.process_block(block));
            assert_same_run("process_block", len, &got, &want);
            let got = exercise(len, |c, block| c.process_block_portable(block));
            assert_same_run("portable build", len, &got, &want);
            let got = exercise(len, |c, block| {
                for v in block {
                    *v = c.process(*v);
                }
            });
            assert_same_run("one sample at a time", len, &got, &want);
        }
    }

    /// The AVX2 build of the loop returns the bits of the portable one and of
    /// the reference. Nothing to compare on a CPU without AVX2, where the
    /// dispatch never takes that build.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn avx2_build_matches_the_portable_one_bit_for_bit() {
        if !std::arch::is_x86_feature_detected!("avx2") {
            eprintln!("no AVX2 on this CPU: comparison skipped");
            return;
        }
        for len in LENGTHS {
            let got = exercise(len, |c, block| {
                // SAFETY: AVX2 was detected at the top of the test.
                unsafe { c.process_block_avx2(block) }
            });
            let portable = exercise(len, |c, block| c.process_block_portable(block));
            assert_same_run("AVX2 build against the portable one", len, &got, &portable);
            assert_same_run("AVX2 build", len, &got, &reference_run(len));
        }
    }

    /// An empty block is a no-op: no sample has flowed, so the kernel length
    /// can still be adopted afterwards.
    #[test]
    fn empty_block_leaves_the_convolver_untouched() {
        let mut c = EarConvolver::new();
        c.process_block(&mut []);
        assert!(!c.started);
        assert_eq!(c.pos, 0);
        c.set_coeffs(&[1.0; 256]);
        assert_eq!(c.len(), 256);
    }
}
