//! Uniform-partitioned overlap-save FFT convolution.
//!
//! One streaming **input** keeps a frequency-domain delay line (FDL) of its
//! most recent block spectra; any number of **kernels** can then be applied
//! to that input, each costing one complex multiply-accumulate over its
//! partitions plus one inverse FFT per block. The forward FFT is paid once
//! per input whatever the kernel count, which is the shape both users need:
//!
//! * the linear-phase FIR crossover ([`crate::crossover::fir`]) — one input,
//!   one kernel per lowpass;
//! * the BRIR renderer — one virtual-speaker bus per input, one kernel per
//!   ear, kernels tens of thousands of taps long.
//!
//! Kernels are partitioned into `block`-sample chunks whose spectra are
//! computed once, off the audio thread ([`ConvolutionPlan::partition`] /
//! [`ConvolutionPlan::repartition`]); the streaming side never allocates.
//! Streaming latency is `block − 1` samples ([`ConvolutionPlan::latency_samples`]):
//! the first output sample of a block is available once the block's last input
//! sample has been pushed.
//!
//! A kernel swap is linear in the kernel, so a click-free change is a per-sample
//! linear blend of the outputs of the outgoing and incoming kernels over one
//! block ([`ConvolutionPlan::synthesize_blend`]) — two MAC+IFFT passes for that
//! block only.

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

/// FFT plans and geometry for one partition size. Build once per partition
/// size and share it between every input and kernel of that size (cloning
/// shares the plans).
#[derive(Clone)]
pub struct ConvolutionPlan {
    /// Hop / partition size in samples.
    block: usize,
    /// Spectrum bins per partition: `block + 1` (real FFT of `2 · block`).
    bins: usize,
    fft: Arc<dyn RealToComplex<f32>>,
    ifft: Arc<dyn ComplexToReal<f32>>,
}

/// A kernel split into `block`-sized partitions, each stored as its
/// `2 · block` real-FFT spectrum. Immutable once built; `Send + Sync`, so it
/// can be prepared on a worker thread and handed to the audio thread.
pub struct PartitionedKernel {
    taps: usize,
    block: usize,
    bins: usize,
    /// `[partition][bin]`, flattened with `bins` entries per partition.
    spectra: Vec<Complex<f32>>,
}

/// Streaming state of one input: the pending block being assembled, the
/// previous block (overlap-save history) and the ring of past spectra.
pub struct InputHistory {
    pending: Vec<f32>,
    prev_block: Vec<f32>,
    /// Ring of the last `capacity` input spectra, `bins` each, flattened.
    fdl: Vec<Complex<f32>>,
    capacity: usize,
    /// Ring index of the most recent spectrum.
    fdl_pos: usize,
    fft_in: Vec<f32>,
    fft_scratch: Vec<Complex<f32>>,
}

/// Scratch for producing one output block. One per audio thread is enough;
/// it holds nothing that outlives a [`ConvolutionPlan::synthesize`] call.
pub struct OutputScratch {
    spec_acc: Vec<Complex<f32>>,
    ifft_out: Vec<f32>,
    ifft_scratch: Vec<Complex<f32>>,
}

impl ConvolutionPlan {
    /// Plan for a partition size of `block` samples (`block ≥ 1`).
    pub fn new(block: usize) -> Self {
        assert!(block >= 1, "partition size must be at least one sample");
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(2 * block);
        let ifft = planner.plan_fft_inverse(2 * block);
        Self {
            block,
            bins: block + 1,
            fft,
            ifft,
        }
    }

    /// Partition size in samples.
    #[inline]
    pub fn block(&self) -> usize {
        self.block
    }

    /// Streaming latency in samples: `block − 1`.
    #[inline]
    pub fn latency_samples(&self) -> usize {
        self.block - 1
    }

    /// Partitions a kernel of `taps` samples occupies under this plan.
    #[inline]
    pub fn partitions_for(&self, taps: usize) -> usize {
        taps.div_ceil(self.block)
    }

    /// Partition `kernel` (allocating). Not for the audio thread.
    pub fn partition(&self, kernel: &[f32]) -> PartitionedKernel {
        let mut out = PartitionedKernel {
            taps: 0,
            block: self.block,
            bins: self.bins,
            spectra: Vec::new(),
        };
        self.repartition(kernel, &mut out);
        out
    }

    /// Partition `kernel` into `out`, reusing its allocation when large
    /// enough (a worker thread preparing successive kernels keeps one
    /// `PartitionedKernel` per slot and never grows it in steady state).
    pub fn repartition(&self, kernel: &[f32], out: &mut PartitionedKernel) {
        let partitions = self.partitions_for(kernel.len());
        out.taps = kernel.len();
        out.block = self.block;
        out.bins = self.bins;
        out.spectra.clear();
        out.spectra
            .resize(partitions * self.bins, Complex::default());
        let mut time = vec![0.0f32; 2 * self.block];
        let mut scratch = self.fft.make_scratch_vec();
        for (p, spec) in out.spectra.chunks_exact_mut(self.bins).enumerate() {
            let chunk = &kernel[p * self.block..kernel.len().min((p + 1) * self.block)];
            time.fill(0.0);
            time[..chunk.len()].copy_from_slice(chunk);
            self.fft
                .process_with_scratch(&mut time, spec, &mut scratch)
                .expect("kernel FFT sizes are fixed by construction");
        }
    }

    /// Allocate the streaming state of one input able to serve kernels of up
    /// to `capacity` partitions.
    pub fn make_input(&self, capacity: usize) -> InputHistory {
        let capacity = capacity.max(1);
        InputHistory {
            pending: Vec::with_capacity(self.block),
            prev_block: vec![0.0; self.block],
            fdl: vec![Complex::default(); capacity * self.bins],
            capacity,
            fdl_pos: 0,
            fft_in: vec![0.0; 2 * self.block],
            fft_scratch: self.fft.make_scratch_vec(),
        }
    }

    /// Allocate an output scratch for this plan.
    pub fn make_scratch(&self) -> OutputScratch {
        OutputScratch {
            spec_acc: vec![Complex::default(); self.bins],
            ifft_out: vec![0.0; 2 * self.block],
            ifft_scratch: self.ifft.make_scratch_vec(),
        }
    }

    /// Transform the completed pending block of `input` into its spectrum
    /// ring. `input` must hold exactly one block ([`InputHistory::push`]
    /// returned `true`); the block is consumed (pending emptied) and stays
    /// readable as [`InputHistory::last_block`] until the next call.
    pub fn analyze(&self, input: &mut InputHistory) {
        debug_assert_eq!(
            input.pending.len(),
            self.block,
            "analyze needs a full block"
        );
        let b = self.block;
        input.fft_in[..b].copy_from_slice(&input.prev_block);
        input.fft_in[b..].copy_from_slice(&input.pending);
        input.prev_block.copy_from_slice(&input.pending);
        input.fdl_pos = (input.fdl_pos + 1) % input.capacity;
        let pos = input.fdl_pos;
        let spec = &mut input.fdl[pos * self.bins..(pos + 1) * self.bins];
        self.fft
            .process_with_scratch(&mut input.fft_in, spec, &mut input.fft_scratch)
            .expect("streaming FFT sizes are fixed by construction");
        input.pending.clear();
    }

    /// Add `kernel` applied to the spectrum ring of `input` into `scratch`,
    /// without inverse-transforming: several inputs and kernels can be
    /// summed in the frequency domain and inverse-transformed once by
    /// [`Self::finish`] — one IFFT per output however many sources feed it.
    /// Call [`OutputScratch::clear`] before the first term of a block.
    pub fn accumulate(
        &self,
        input: &InputHistory,
        kernel: &PartitionedKernel,
        scratch: &mut OutputScratch,
    ) {
        debug_assert_eq!(
            kernel.block, self.block,
            "kernel partitioned for another block size"
        );
        let partitions = kernel.partitions();
        debug_assert!(
            partitions <= input.capacity,
            "kernel has {partitions} partitions but the input history holds {}",
            input.capacity
        );
        let bins = self.bins;
        let cap = input.capacity;
        for p in 0..partitions {
            let idx = (input.fdl_pos + cap - p) % cap;
            let src = &input.fdl[idx * bins..(idx + 1) * bins];
            let ker = &kernel.spectra[p * bins..(p + 1) * bins];
            for ((acc, &s), &k) in scratch.spec_acc.iter_mut().zip(src).zip(ker) {
                *acc += s * k;
            }
        }
    }

    /// Inverse-transform the accumulated spectrum into `scratch.ifft_out`.
    fn inverse(&self, scratch: &mut OutputScratch) {
        self.ifft
            .process_with_scratch(
                &mut scratch.spec_acc,
                &mut scratch.ifft_out,
                &mut scratch.ifft_scratch,
            )
            .expect("streaming FFT sizes are fixed by construction");
    }

    /// Inverse-transform normalisation: `realfft` leaves a factor of the FFT
    /// length on the round trip.
    #[inline]
    fn scale(&self) -> f32 {
        1.0 / (2 * self.block) as f32
    }

    /// Inverse-transform what [`Self::accumulate`] summed into `scratch` and
    /// write the output block into `out` (`out.len() == block`).
    pub fn finish(&self, scratch: &mut OutputScratch, out: &mut [f32]) {
        debug_assert_eq!(out.len(), self.block);
        self.inverse(scratch);
        let scale = self.scale();
        // Overlap-save: the first `block` samples are circular garbage.
        for (o, &v) in out.iter_mut().zip(&scratch.ifft_out[self.block..]) {
            *o = v * scale;
        }
    }

    /// Like [`Self::finish`], but ramping `out` — which holds the block of an
    /// outgoing kernel set — linearly toward the accumulated result across
    /// the block (weight `(i + 1) / block` at sample `i`, so the last sample
    /// is the new result exactly). The click-free swap of a kernel set: both
    /// sets are run for that one block only.
    pub fn finish_blend(&self, scratch: &mut OutputScratch, out: &mut [f32]) {
        debug_assert_eq!(out.len(), self.block);
        self.inverse(scratch);
        let scale = self.scale();
        let step = 1.0 / self.block as f32;
        for (i, (o, &v)) in out
            .iter_mut()
            .zip(&scratch.ifft_out[self.block..])
            .enumerate()
        {
            let w = (i + 1) as f32 * step;
            let target = v * scale;
            *o += (target - *o) * w;
        }
    }

    /// Write the current output block of `input` convolved with `kernel` into
    /// `out` (`out.len() == block`). Call once per [`Self::analyze`] and per
    /// kernel; the input must have been analysed at least once.
    pub fn synthesize(
        &self,
        input: &InputHistory,
        kernel: &PartitionedKernel,
        scratch: &mut OutputScratch,
        out: &mut [f32],
    ) {
        scratch.clear();
        self.accumulate(input, kernel, scratch);
        self.finish(scratch, out);
    }

    /// Like [`Self::synthesize`], but blending linearly from the output of
    /// `from` to the output of `to` across the block (see
    /// [`Self::finish_blend`]). Use it for the single block in which a kernel
    /// changes, then continue with [`Self::synthesize`] on `to`.
    pub fn synthesize_blend(
        &self,
        input: &InputHistory,
        from: &PartitionedKernel,
        to: &PartitionedKernel,
        scratch: &mut OutputScratch,
        out: &mut [f32],
    ) {
        self.synthesize(input, from, scratch, out);
        scratch.clear();
        self.accumulate(input, to, scratch);
        self.finish_blend(scratch, out);
    }
}

impl OutputScratch {
    /// Zero the accumulator ahead of a block's first [`ConvolutionPlan::accumulate`].
    #[inline]
    pub fn clear(&mut self) {
        self.spec_acc.fill(Complex::default());
    }
}

impl PartitionedKernel {
    /// Kernel length in samples (before partitioning).
    #[inline]
    pub fn taps(&self) -> usize {
        self.taps
    }

    /// Number of partitions (`ceil(taps / block)`).
    #[inline]
    pub fn partitions(&self) -> usize {
        self.spectra.len() / self.bins
    }

    /// Partition size this kernel was built for.
    #[inline]
    pub fn block(&self) -> usize {
        self.block
    }
}

impl InputHistory {
    /// Append one input sample; returns `true` when the pending block is
    /// complete and must be handed to [`ConvolutionPlan::analyze`] before the
    /// next push.
    #[inline]
    pub fn push(&mut self, x: f32) -> bool {
        debug_assert!(
            self.pending.len() < self.prev_block.len(),
            "analyze the full block first"
        );
        self.pending.push(x);
        self.pending.len() == self.prev_block.len()
    }

    /// Samples pushed since the last [`ConvolutionPlan::analyze`].
    #[inline]
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// The last block handed to [`ConvolutionPlan::analyze`] (zeros before
    /// the first one) — the raw input the current output block corresponds to.
    #[inline]
    pub fn last_block(&self) -> &[f32] {
        &self.prev_block
    }

    /// Partitions this history can serve.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Zero all memory in place (no reallocation), so a new signal never
    /// splices into the previous one's tail.
    pub fn reset(&mut self) {
        self.pending.clear();
        self.prev_block.fill(0.0);
        self.fdl.fill(Complex::default());
        self.fdl_pos = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic full-band test signal in [−1, 1] (LCG; no rand dep).
    fn noise(len: usize, seed: u32) -> Vec<f32> {
        let mut s = seed;
        (0..len)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (s >> 8) as f32 / (1 << 24) as f32 * 2.0 - 1.0
            })
            .collect()
    }

    /// Direct convolution in f64, delayed by `delay` samples.
    fn direct(x: &[f32], h: &[f32], delay: usize) -> Vec<f64> {
        let mut y = vec![0.0f64; x.len()];
        for (n, out) in y.iter_mut().enumerate() {
            if n < delay {
                continue;
            }
            let m = n - delay;
            let mut acc = 0.0f64;
            for (k, &hk) in h.iter().enumerate() {
                if k > m {
                    break;
                }
                acc += hk as f64 * x[m - k] as f64;
            }
            *out = acc;
        }
        y
    }

    /// Stream `x` through `kernel` sample by sample, reading one output
    /// sample per input sample (zeros until the first block completes).
    fn stream(
        plan: &ConvolutionPlan,
        input: &mut InputHistory,
        kernel: &PartitionedKernel,
        x: &[f32],
    ) -> Vec<f32> {
        let mut scratch = plan.make_scratch();
        let mut block_out = vec![0.0f32; plan.block()];
        let mut read = plan.block(); // nothing to read yet
        let mut y = Vec::with_capacity(x.len());
        for &s in x {
            if input.push(s) {
                plan.analyze(input);
                plan.synthesize(input, kernel, &mut scratch, &mut block_out);
                read = 0;
            }
            y.push(if read < plan.block() {
                let v = block_out[read];
                read += 1;
                v
            } else {
                0.0
            });
        }
        y
    }

    /// The defining property: the stream equals the direct convolution
    /// delayed by `block − 1`, for a kernel whose last partition is partial.
    #[test]
    fn matches_direct_convolution() {
        let plan = ConvolutionPlan::new(128);
        let h = noise(2500, 7); // 20 partitions, last one 68 taps
        let kernel = plan.partition(&h);
        assert_eq!(kernel.partitions(), 20);
        assert_eq!(kernel.taps(), 2500);
        let mut input = plan.make_input(kernel.partitions());
        let x = noise(6000, 99);
        let y = stream(&plan, &mut input, &kernel, &x);
        let want = direct(&x, &h, plan.latency_samples());
        let peak = want.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        let max_err = y
            .iter()
            .zip(&want)
            .fold(0.0f64, |m, (&a, &b)| m.max((a as f64 - b).abs()));
        assert!(
            max_err < 2e-5 * peak,
            "partitioned convolution drifts from direct: max err {max_err}, peak {peak}"
        );
    }

    /// A history sized for more partitions than the kernel uses must give
    /// bit-identical output: the surplus ring slots are never read.
    #[test]
    fn oversized_history_is_bit_identical() {
        let plan = ConvolutionPlan::new(64);
        let h = noise(200, 3);
        let kernel = plan.partition(&h);
        let x = noise(2000, 5);
        let mut tight = plan.make_input(kernel.partitions());
        let mut wide = plan.make_input(kernel.partitions() + 5);
        let a = stream(&plan, &mut tight, &kernel, &x);
        let b = stream(&plan, &mut wide, &kernel, &x);
        assert_eq!(a, b);
    }

    /// A unit impulse through a unit kernel comes back at exactly
    /// `latency_samples()`, pinning the latency accounting.
    #[test]
    fn impulse_lands_at_latency() {
        let plan = ConvolutionPlan::new(32);
        let mut h = vec![0.0f32; 40];
        h[0] = 1.0;
        let kernel = plan.partition(&h);
        let mut input = plan.make_input(kernel.partitions());
        let mut x = vec![0.0f32; 200];
        x[0] = 1.0;
        let y = stream(&plan, &mut input, &kernel, &x);
        let hits: Vec<(usize, f32)> = y
            .iter()
            .enumerate()
            .filter(|(_, v)| v.abs() > 1e-6)
            .map(|(i, &v)| (i, v))
            .collect();
        assert_eq!(hits.len(), 1, "one echo expected, got {hits:?}");
        assert_eq!(hits[0].0, plan.latency_samples());
        assert!((hits[0].1 - 1.0).abs() < 1e-6);
    }

    /// Blending from a kernel to itself is the plain output, bit for bit;
    /// blending between two gains ramps linearly across the block and lands
    /// on the incoming kernel at the last sample.
    #[test]
    fn blend_ramps_linearly_between_kernels() {
        let plan = ConvolutionPlan::new(16);
        let h = noise(50, 11);
        let kernel = plan.partition(&h);
        let x = noise(500, 13);
        let mut input = plan.make_input(kernel.partitions());
        let mut scratch = plan.make_scratch();
        let mut plain = vec![0.0f32; 16];
        let mut blended = vec![0.0f32; 16];
        for &s in &x {
            if input.push(s) {
                plan.analyze(&mut input);
                plan.synthesize(&input, &kernel, &mut scratch, &mut plain);
                plan.synthesize_blend(&input, &kernel, &kernel, &mut scratch, &mut blended);
                assert_eq!(plain, blended);
            }
        }

        // DC input, δ → 0.5·δ: the settled output must fall from 1 to 0.5
        // with weight (i + 1) / block.
        let unity = plan.partition(&[1.0]);
        let half = plan.partition(&[0.5]);
        let mut input = plan.make_input(1);
        let mut out = vec![0.0f32; 16];
        for _ in 0..48 {
            if input.push(1.0) {
                plan.analyze(&mut input);
            }
        }
        plan.synthesize_blend(&input, &unity, &half, &mut scratch, &mut out);
        for (i, &v) in out.iter().enumerate() {
            let w = (i + 1) as f32 / 16.0;
            let want = 1.0 + (0.5 - 1.0) * w;
            assert!((v - want).abs() < 1e-5, "sample {i}: {v} vs {want}");
        }
        assert!((out[15] - 0.5).abs() < 1e-5);
    }

    /// `reset` wipes the tail: after an impulse and a reset, silence in gives
    /// silence out even though the kernel spans several blocks.
    #[test]
    fn reset_clears_history() {
        let plan = ConvolutionPlan::new(8);
        let h = vec![0.25f32; 40];
        let kernel = plan.partition(&h);
        let mut input = plan.make_input(kernel.partitions());
        let mut x = vec![0.0f32; 16];
        x[0] = 1.0;
        let _ = stream(&plan, &mut input, &kernel, &x);
        input.reset();
        let y = stream(&plan, &mut input, &kernel, &vec![0.0f32; 80]);
        assert!(y.iter().all(|&v| v == 0.0), "tail survived reset: {y:?}");
    }

    /// Summing two sources in the frequency domain and inverse-transforming
    /// once equals the sum of their separately synthesized blocks.
    #[test]
    fn accumulate_sums_sources_before_one_inverse() {
        let plan = ConvolutionPlan::new(64);
        let ka = plan.partition(&noise(150, 31));
        let kb = plan.partition(&noise(90, 37));
        let xa = noise(640, 41);
        let xb = noise(640, 43);
        let mut ia = plan.make_input(ka.partitions());
        let mut ib = plan.make_input(kb.partitions());
        let mut scratch = plan.make_scratch();
        let (mut oa, mut ob, mut sum) = (vec![0.0f32; 64], vec![0.0f32; 64], vec![0.0f32; 64]);
        for (&a, &b) in xa.iter().zip(&xb) {
            let done = ia.push(a);
            let done_b = ib.push(b);
            assert_eq!(done, done_b);
            if done {
                plan.analyze(&mut ia);
                plan.analyze(&mut ib);
                plan.synthesize(&ia, &ka, &mut scratch, &mut oa);
                plan.synthesize(&ib, &kb, &mut scratch, &mut ob);
                scratch.clear();
                plan.accumulate(&ia, &ka, &mut scratch);
                plan.accumulate(&ib, &kb, &mut scratch);
                plan.finish(&mut scratch, &mut sum);
                for i in 0..64 {
                    assert!(
                        (sum[i] - (oa[i] + ob[i])).abs() < 1e-5,
                        "sample {i}: {} vs {}",
                        sum[i],
                        oa[i] + ob[i]
                    );
                }
            }
        }
    }

    /// `repartition` into a previously larger kernel yields exactly what a
    /// fresh `partition` does — and an empty kernel is a valid silent one.
    #[test]
    fn repartition_matches_partition() {
        let plan = ConvolutionPlan::new(32);
        let big = noise(300, 17);
        let small = noise(70, 19);
        let fresh = plan.partition(&small);
        let mut reused = plan.partition(&big);
        plan.repartition(&small, &mut reused);
        assert_eq!(reused.taps(), fresh.taps());
        assert_eq!(reused.partitions(), fresh.partitions());
        assert_eq!(reused.spectra, fresh.spectra);

        let empty = plan.partition(&[]);
        assert_eq!(empty.partitions(), 0);
        let mut input = plan.make_input(1);
        let y = stream(&plan, &mut input, &empty, &noise(100, 23));
        assert!(y.iter().all(|&v| v == 0.0));
    }
}
