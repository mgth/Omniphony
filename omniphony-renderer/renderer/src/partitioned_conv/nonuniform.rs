//! Non-uniform partitioning: a long kernel at the latency of a small block.
//!
//! Under a uniform partition every block pays one multiply-accumulate per
//! `block` taps of kernel, so the block size the latency calls for also sets
//! the price of the whole tail. Here only the head of the kernel sits on
//! that small block; the rest is cut into *segments* convolved on larger
//! blocks — a ladder of sizes, each a multiple of the one before — which
//! cover the same taps with far fewer partitions. The output is the uniform
//! one up to rounding, at the head block's latency.
//!
//! # Deadlines
//!
//! A segment on blocks of `B` samples starts at tap `2·B − head` of the
//! kernel. `B − head` of that offset is the wait for its input block to
//! fill; the other `B` is slack: an output block is first read one whole
//! segment block after the input it depends on is complete. The slack is
//! what keeps the worst head block flat — the transforms and accumulations
//! of one segment block are spread over the `B / head` head blocks of that
//! period ([`TailStreams::run`]) instead of landing on the head block that
//! completes it.
//!
//! # Pieces
//!
//! [`NonUniformPlan`] holds one [`ConvolutionPlan`] per ladder level and the
//! geometry; [`NonUniformKernel`] is a kernel cut along it. The head level
//! is driven by the caller exactly as a uniform convolver
//! ([`NonUniformPlan::head`], one [`InputHistory`] per input,
//! [`NonUniformKernel::head`]); [`TailStreams`] carries every level past it
//! for a set of inputs summed into a set of outputs, fed the head blocks as
//! they complete.

use std::ops::Range;

use super::{ConvolutionPlan, InputHistory, OutputScratch, PartitionedKernel};

/// A level past the head is used once the taps beyond its offset would take
/// more than this many partitions of the level below: under that, the
/// transforms the level adds cost more than the partitions it saves.
const OPEN_LEVEL_PARTITIONS: usize = 6;

/// What one transform of a segment block costs next to the accumulation of
/// one of its partitions, for the schedule's bookkeeping only: it decides
/// how the work of a period is shared between its head blocks, not what is
/// computed.
const TRANSFORM_COST: u64 = 6;

/// Partitions a tail kernel is accumulated by at a time. The schedule moves
/// in whole tasks, so this bounds what one head block can run past its
/// share when the kernels are long: about one transform's worth.
const RUN_PARTITIONS: usize = 4;

/// One [`ConvolutionPlan`] per level of a ladder of block sizes, and where
/// each level sits in a kernel. Cloning shares the FFT plans.
#[derive(Clone)]
pub struct NonUniformPlan {
    /// Smallest block (the head) first.
    plans: Vec<ConvolutionPlan>,
}

/// A kernel cut along a [`NonUniformPlan`]: the head's partitions, then
/// those of each tail segment in use. A kernel that ends before a level's
/// offset has no partition there.
pub struct NonUniformKernel {
    taps: usize,
    head: PartitionedKernel,
    tails: Vec<TailKernel>,
}

/// The part of a kernel one tail segment convolves.
pub struct TailKernel {
    /// The segment's partitions in runs of [`RUN_PARTITIONS`] (the last one
    /// shorter), each accumulated as one task.
    runs: Vec<PartitionedKernel>,
}

impl NonUniformPlan {
    /// Plans for a ladder of block sizes, the head — which sets the latency —
    /// first, each size a multiple of the one before. A one-level ladder is
    /// the uniform convolver.
    pub fn new(ladder: &[usize]) -> Self {
        assert!(!ladder.is_empty(), "a ladder has at least the head block");
        for pair in ladder.windows(2) {
            assert!(
                pair[1] > pair[0] && pair[1] % pair[0] == 0,
                "each ladder size must be a multiple of the one before: {ladder:?}"
            );
        }
        Self {
            plans: ladder.iter().map(|&b| ConvolutionPlan::new(b)).collect(),
        }
    }

    /// The head level's plan: the block the caller streams on.
    #[inline]
    pub fn head(&self) -> &ConvolutionPlan {
        &self.plans[0]
    }

    /// Block size of `level`.
    #[inline]
    pub fn block(&self, level: usize) -> usize {
        self.plans[level].block()
    }

    /// First kernel tap of `level`: zero for the head, `2·block − head` past
    /// it (see the module doc).
    #[inline]
    pub fn offset(&self, level: usize) -> usize {
        if level == 0 {
            0
        } else {
            2 * self.block(level) - self.block(0)
        }
    }

    /// Levels a set of kernels of up to `taps` samples is cut into. Every
    /// kernel convolved together must be cut for the same count: the last
    /// level in use takes whatever lies beyond its offset.
    pub fn levels_for(&self, taps: usize) -> usize {
        let mut levels = 1;
        while levels < self.plans.len()
            && taps > self.offset(levels) + OPEN_LEVEL_PARTITIONS * self.block(levels - 1)
        {
            levels += 1;
        }
        levels
    }

    /// Taps of a `taps`-long kernel that fall on `level` when `levels` are in
    /// use.
    fn span(&self, level: usize, levels: usize, taps: usize) -> Range<usize> {
        let start = self.offset(level).min(taps);
        let end = if level + 1 < levels {
            self.offset(level + 1).min(taps)
        } else {
            taps
        };
        start..end
    }

    /// Partitions a `taps`-long kernel occupies on `level` when `levels` are
    /// in use.
    pub fn partitions_for(&self, level: usize, levels: usize, taps: usize) -> usize {
        self.plans[level].partitions_for(self.span(level, levels, taps).len())
    }

    /// Cut `kernel` into `levels` segments (allocating). Not for the audio
    /// thread.
    pub fn partition(&self, kernel: &[f32], levels: usize) -> NonUniformKernel {
        debug_assert!((1..=self.plans.len()).contains(&levels));
        let taps = kernel.len();
        let tails = (1..levels)
            .map(|l| {
                let plan = &self.plans[l];
                let runs = kernel[self.span(l, levels, taps)]
                    .chunks(RUN_PARTITIONS * plan.block())
                    .map(|run| plan.partition(run))
                    .collect();
                TailKernel { runs }
            })
            .collect();
        NonUniformKernel {
            taps,
            head: self.plans[0].partition(&kernel[self.span(0, levels, taps)]),
            tails,
        }
    }

    /// Allocate the tail state of `inputs` streams summed into `outputs`,
    /// for kernels of up to `taps` samples cut into `levels`.
    pub fn make_tails(
        &self,
        levels: usize,
        taps: usize,
        inputs: usize,
        outputs: usize,
    ) -> TailStreams {
        let head = self.block(0);
        let segments: Vec<TailSegment> = (1..levels)
            .map(|l| {
                let plan = self.plans[l].clone();
                let block = plan.block();
                let capacity = self.partitions_for(l, levels, taps);
                TailSegment {
                    period: block / head,
                    slot: 0,
                    runs: capacity.div_ceil(RUN_PARTITIONS),
                    inputs: (0..inputs).map(|_| plan.make_input(capacity)).collect(),
                    scratch: plan.make_scratch(),
                    front: vec![vec![0.0; block]; outputs],
                    back: vec![vec![0.0; block]; outputs],
                    fade: vec![0.0; block],
                    source: 0,
                    starting: false,
                    blend: false,
                    task: 0,
                    tasks: 0,
                    spent: 0,
                    budget: 0,
                    plan,
                }
            })
            .collect();
        // Two blocks of the largest segment: the one being analysed stays
        // whole while the next one fills.
        let ring_blocks = segments.last().map_or(0, |s| 2 * s.period);
        TailStreams {
            head_block: head,
            ring: vec![vec![0.0; ring_blocks * head]; if ring_blocks == 0 { 0 } else { inputs }],
            ring_blocks,
            write_block: 0,
            segments,
        }
    }
}

impl NonUniformKernel {
    /// Kernel length in samples (before partitioning).
    #[inline]
    pub fn taps(&self) -> usize {
        self.taps
    }

    /// The head level's partitions.
    #[inline]
    pub fn head(&self) -> &PartitionedKernel {
        &self.head
    }

    /// The part tail segment `segment` (level `segment + 1`) convolves.
    #[inline]
    pub fn tail(&self, segment: usize) -> &TailKernel {
        &self.tails[segment]
    }

    /// Levels the kernel was cut into.
    #[inline]
    pub fn levels(&self) -> usize {
        1 + self.tails.len()
    }

    /// Partitions over every level.
    pub fn partitions(&self) -> usize {
        self.head.partitions() + self.tails.iter().map(TailKernel::partitions).sum::<usize>()
    }

    /// Resident size of the spectra, bytes.
    pub fn bytes(&self) -> usize {
        std::iter::once(&self.head)
            .chain(self.tails.iter().flat_map(|t| &t.runs))
            .map(|k| std::mem::size_of_val(k.spectra.as_slice()))
            .sum()
    }
}

impl TailKernel {
    /// Partitions of the segment the kernel reaches into.
    pub fn partitions(&self) -> usize {
        self.runs.iter().map(PartitionedKernel::partitions).sum()
    }
}

/// Resident size of one input's streaming state, bytes.
pub fn history_bytes(input: &InputHistory) -> usize {
    std::mem::size_of_val(input.fdl.as_slice())
        + std::mem::size_of_val(input.fft_scratch.as_slice())
        + (input.pending.capacity() + input.prev_block.len() + input.fft_in.len())
            * std::mem::size_of::<f32>()
}

/// Streaming state of every level past the head, for a set of inputs summed
/// into a set of outputs. Once per head block: [`Self::feed`] the block of
/// every input, then [`Self::run`] each segment, which does that head
/// block's share of the segment's work and adds the segment's output.
/// Nothing here allocates.
pub struct TailStreams {
    head_block: usize,
    /// Per input: the last `ring_blocks` head blocks, for the deferred
    /// transforms. Empty without a tail segment.
    ring: Vec<Vec<f32>>,
    ring_blocks: usize,
    /// Ring slot the next head block is written to.
    write_block: usize,
    segments: Vec<TailSegment>,
}

/// One level past the head.
struct TailSegment {
    plan: ConvolutionPlan,
    /// Head blocks per segment block.
    period: usize,
    /// Head blocks since the period started (0 on the head block that
    /// completes a segment block).
    slot: usize,
    /// Runs the longest kernel of the segment is accumulated in.
    runs: usize,
    inputs: Vec<InputHistory>,
    scratch: OutputScratch,
    /// Per output: the block being read, a head block per [`TailStreams::run`].
    front: Vec<Vec<f32>>,
    /// Per output: the block being computed, read over the next period.
    back: Vec<Vec<f32>>,
    /// The outgoing kernels' block of the output a blend is computing.
    fade: Vec<f32>,
    /// Ring offset of the input block the period analyses.
    source: usize,
    /// A period started on this head block and its work is not laid out yet.
    starting: bool,
    /// The period in flight computes two kernel sets and ramps between them.
    blend: bool,
    /// Next task of the period in flight, of `tasks`.
    task: usize,
    tasks: usize,
    /// Cost of the tasks done, of the period's `budget`.
    spent: u64,
    budget: u64,
}

impl TailStreams {
    /// The tail of a head-only layout: no segment, nothing to do.
    pub fn empty() -> Self {
        Self {
            head_block: 0,
            ring: Vec::new(),
            ring_blocks: 0,
            write_block: 0,
            segments: Vec::new(),
        }
    }

    /// Tail segments (levels past the head); segment `s` is level `s + 1`.
    #[inline]
    pub fn segments(&self) -> usize {
        self.segments.len()
    }

    /// Resident size of the streaming state, bytes.
    pub fn bytes(&self) -> usize {
        let samples = |v: &Vec<f32>| std::mem::size_of_val(v.as_slice());
        self.ring.iter().map(samples).sum::<usize>()
            + self
                .segments
                .iter()
                .map(|s| {
                    s.inputs.iter().map(history_bytes).sum::<usize>()
                        + s.front.iter().chain(&s.back).map(samples).sum::<usize>()
                        + samples(&s.fade)
                })
                .sum::<usize>()
    }

    /// Take the head block each input just completed (one slice per input,
    /// in order) and move every segment one head block on. A segment whose
    /// block is complete starts a period: the block computed over the last
    /// one becomes the one read.
    pub fn feed<'a>(&mut self, blocks: impl IntoIterator<Item = &'a [f32]>) {
        if self.segments.is_empty() {
            return;
        }
        let head = self.head_block;
        let at = self.write_block * head;
        for (ring, block) in self.ring.iter_mut().zip(blocks) {
            ring[at..at + head].copy_from_slice(block);
        }
        self.write_block = (self.write_block + 1) % self.ring_blocks;
        let ring_len = self.ring_blocks * head;
        let end = self.write_block * head;
        for seg in &mut self.segments {
            seg.slot = (seg.slot + 1) % seg.period;
            if seg.slot == 0 {
                debug_assert_eq!(seg.task, seg.tasks, "a period ended with work left");
                std::mem::swap(&mut seg.front, &mut seg.back);
                seg.source = (end + ring_len - seg.plan.block()) % ring_len;
                seg.starting = true;
            }
        }
    }

    /// Whether `segment` starts a period on this head block: the moment its
    /// kernels may change (see [`Self::run`]).
    #[inline]
    pub fn period_starts(&self, segment: usize) -> bool {
        self.segments[segment].starting
    }

    /// Do this head block's share of `segment`'s work and add the head block
    /// of its output to `out` (one block per output).
    ///
    /// `kernel(pass, input, output)` gives this segment's part of the kernel
    /// applied from `input` to `output`, `None` where the two are not
    /// connected. It must answer the same for the whole period. `blend` is
    /// read on the head block a period starts on: when set, the period
    /// computes pass 0 (the outgoing kernels) and pass 1 (the incoming ones)
    /// and ramps from the first to the second over the first head block of
    /// its output, as [`ConvolutionPlan::finish_blend`] does for the head;
    /// otherwise pass 0 alone.
    pub fn run<'k>(
        &mut self,
        segment: usize,
        blend: bool,
        kernel: impl Fn(usize, usize, usize) -> Option<&'k TailKernel>,
        out: &mut [Vec<f32>],
    ) {
        let head = self.head_block;
        let seg = &mut self.segments[segment];
        if seg.starting {
            seg.starting = false;
            seg.begin(blend, &kernel);
        }
        // Up to this slot's share of the period's cost; whatever is left on
        // the last slot, which is the deadline.
        let due = seg.budget * (seg.slot as u64 + 1);
        let last = seg.slot + 1 == seg.period;
        while seg.task < seg.tasks && (last || seg.spent * (seg.period as u64) < due) {
            seg.run_task(&self.ring, head, &kernel);
        }
        let at = seg.slot * head;
        for (out, front) in out.iter_mut().zip(&seg.front) {
            for (o, &t) in out.iter_mut().zip(&front[at..at + head]) {
                *o += t;
            }
        }
    }
}

impl TailSegment {
    /// Lay out a period: one analysis per input, then per output and per
    /// pass one accumulation per input and run of partitions and one inverse
    /// transform.
    fn begin<'k>(
        &mut self,
        blend: bool,
        kernel: &impl Fn(usize, usize, usize) -> Option<&'k TailKernel>,
    ) {
        let inputs = self.inputs.len();
        let outputs = self.back.len();
        let passes = 1 + blend as usize;
        self.blend = blend;
        self.task = 0;
        self.tasks = inputs + outputs * passes * (inputs * self.runs + 1);
        self.spent = 0;
        self.budget = (inputs + outputs * passes) as u64 * TRANSFORM_COST;
        for output in 0..outputs {
            for pass in 0..passes {
                for input in 0..inputs {
                    if let Some(k) = kernel(pass, input, output) {
                        self.budget += k.partitions() as u64;
                    }
                }
            }
        }
    }

    /// Run the next task of the period in flight.
    fn run_task<'k>(
        &mut self,
        ring: &[Vec<f32>],
        head: usize,
        kernel: &impl Fn(usize, usize, usize) -> Option<&'k TailKernel>,
    ) {
        let inputs = self.inputs.len();
        let task = self.task;
        self.task += 1;
        if task < inputs {
            let input = &mut self.inputs[task];
            let block = &ring[task][self.source..self.source + self.plan.block()];
            input.pending.extend_from_slice(block);
            self.plan.analyze(input);
            self.spent += TRANSFORM_COST;
            return;
        }
        let passes = 1 + self.blend as usize;
        let per_pass = inputs * self.runs + 1;
        let step = task - inputs;
        let output = step / (passes * per_pass);
        let pass = step / per_pass % passes;
        let step = step % per_pass;
        if step == 0 {
            self.scratch.clear();
        }
        if step < inputs * self.runs {
            let (input, run) = (step / self.runs, step % self.runs);
            if let Some(k) = kernel(pass, input, output).and_then(|k| k.runs.get(run)) {
                // The run's first partition applies `run · RUN_PARTITIONS`
                // blocks back: read the history from there.
                let history = &mut self.inputs[input];
                let newest = history.fdl_pos;
                history.fdl_pos =
                    (newest + history.capacity - run * RUN_PARTITIONS) % history.capacity;
                self.plan.accumulate(history, k, &mut self.scratch);
                history.fdl_pos = newest;
                self.spent += k.partitions() as u64;
            }
            return;
        }
        self.spent += TRANSFORM_COST;
        if self.blend && pass == 0 {
            self.plan.finish(&mut self.scratch, &mut self.fade);
            return;
        }
        let out = &mut self.back[output];
        self.plan.finish(&mut self.scratch, out);
        if self.blend {
            let step = 1.0 / head as f32;
            for (i, (o, &old)) in out[..head].iter_mut().zip(&self.fade[..head]).enumerate() {
                let w = (i + 1) as f32 * step;
                *o = old + (*o - old) * w;
            }
        }
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
        for (n, out) in y.iter_mut().enumerate().skip(delay) {
            let m = n - delay;
            *out = h
                .iter()
                .take(m + 1)
                .enumerate()
                .map(|(k, &hk)| hk as f64 * x[m - k] as f64)
                .sum();
        }
        y
    }

    /// One input through one kernel on `plan`, one output sample per input
    /// sample. `swap` replaces the kernel on the head block of that index:
    /// the head blends on that block, each tail segment at its next period.
    fn stream(
        plan: &NonUniformPlan,
        kernel: &NonUniformKernel,
        swap: Option<(usize, &NonUniformKernel)>,
        x: &[f32],
    ) -> Vec<f32> {
        let levels = kernel.levels();
        let head = plan.head();
        let block = head.block();
        let taps = kernel.taps().max(swap.map_or(0, |(_, k)| k.taps()));
        let mut input = head.make_input(plan.partitions_for(0, levels, taps).max(1));
        let mut tails = plan.make_tails(levels, taps, 1, 1);
        let mut scratch = head.make_scratch();
        let mut out = vec![vec![0.0f32; block]];
        let mut read = block;
        let mut blocks = 0;
        let mut current = kernel;
        // Per tail segment: the kernel its period in flight uses, and the
        // one it is leaving.
        let mut seg_kernels: Vec<(&NonUniformKernel, Option<&NonUniformKernel>)> =
            vec![(kernel, None); tails.segments()];
        let mut y = Vec::with_capacity(x.len());
        for &s in x {
            if input.push(s) {
                head.analyze(&mut input);
                tails.feed([input.last_block()]);
                match swap {
                    Some((at, to)) if at == blocks => {
                        head.synthesize_blend(
                            &input,
                            current.head(),
                            to.head(),
                            &mut scratch,
                            &mut out[0],
                        );
                        current = to;
                    }
                    _ => head.synthesize(&input, current.head(), &mut scratch, &mut out[0]),
                }
                for (s, kernels) in seg_kernels.iter_mut().enumerate() {
                    if tails.period_starts(s) {
                        kernels.1 = None;
                        if !std::ptr::eq(kernels.0, current) {
                            *kernels = (current, Some(kernels.0));
                        }
                    }
                    let (to, from) = *kernels;
                    tails.run(
                        s,
                        from.is_some(),
                        |pass, _, _| {
                            Some(match from {
                                Some(from) if pass == 0 => from.tail(s),
                                _ => to.tail(s),
                            })
                        },
                        &mut out,
                    );
                }
                blocks += 1;
                read = 0;
            }
            y.push(if read < block {
                read += 1;
                out[0][read - 1]
            } else {
                0.0
            });
        }
        y
    }

    fn max_error(y: &[f32], want: &[f64]) -> (f64, f64) {
        let peak = want.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        let err = y
            .iter()
            .zip(want)
            .fold(0.0f64, |m, (&a, &b)| m.max((a as f64 - b).abs()));
        (err, peak)
    }

    #[test]
    fn geometry_follows_the_deadline_rule() {
        let plan = NonUniformPlan::new(&[16, 64, 256]);
        assert_eq!(plan.offset(0), 0);
        assert_eq!(plan.offset(1), 2 * 64 - 16);
        assert_eq!(plan.offset(2), 2 * 256 - 16);
        // A level opens once the taps past its offset outgrow a few
        // partitions of the level below.
        let open1 = plan.offset(1) + OPEN_LEVEL_PARTITIONS * 16;
        let open2 = plan.offset(2) + OPEN_LEVEL_PARTITIONS * 64;
        assert_eq!(plan.levels_for(0), 1);
        assert_eq!(plan.levels_for(open1), 1);
        assert_eq!(plan.levels_for(open1 + 1), 2);
        assert_eq!(plan.levels_for(open2), 2);
        assert_eq!(plan.levels_for(open2 + 1), 3);
        // Inner levels are whole numbers of partitions; the last takes the rest.
        let taps = 2000;
        assert_eq!(plan.partitions_for(0, 3, taps), 112 / 16);
        assert_eq!(plan.partitions_for(1, 3, taps), (496 - 112) / 64);
        assert_eq!(plan.partitions_for(2, 3, taps), (taps - 496).div_ceil(256));
        assert_eq!(plan.partitions_for(1, 2, taps), (taps - 112).div_ceil(64));
        let kernel = plan.partition(&noise(taps, 1), 3);
        assert_eq!(kernel.levels(), 3);
        assert_eq!(kernel.taps(), taps);
        assert_eq!(
            kernel.partitions(),
            (0..3)
                .map(|l| plan.partitions_for(l, 3, taps))
                .sum::<usize>()
        );
        // A kernel that ends inside the head has no partition past it.
        let short = plan.partition(&noise(40, 2), 3);
        assert_eq!(short.head().partitions(), 3);
        assert_eq!(short.tail(0).partitions(), 0);
        assert_eq!(short.tail(1).partitions(), 0);
    }

    /// The defining property: whatever the cut, the stream equals the direct
    /// convolution delayed by the head block − 1 — for kernels that end in
    /// the head, exactly on a segment boundary, one tap past it and deep in
    /// the last segment.
    #[test]
    fn matches_direct_convolution_for_every_cut() {
        let plan = NonUniformPlan::new(&[16, 64, 256]);
        let x = noise(6000, 99);
        for &taps in &[1, 40, 112, 113, 300, 496, 497, 1000, 2500] {
            let h = noise(taps, 7 + taps as u32);
            let want = direct(&x, &h, plan.head().latency_samples());
            for levels in 1..=3 {
                let kernel = plan.partition(&h, levels);
                let y = stream(&plan, &kernel, None, &x);
                let (err, peak) = max_error(&y, &want);
                assert!(
                    err < 2e-5 * peak,
                    "{taps} taps on {levels} levels: max err {err}, peak {peak}"
                );
            }
        }
    }

    /// A one-level ladder is the uniform convolver, bit for bit.
    #[test]
    fn a_one_level_ladder_is_the_uniform_convolver() {
        let h = noise(700, 3);
        let x = noise(3000, 5);
        let plan = NonUniformPlan::new(&[32]);
        let y = stream(&plan, &plan.partition(&h, 1), None, &x);
        let uniform = ConvolutionPlan::new(32);
        let kernel = uniform.partition(&h);
        let mut input = uniform.make_input(kernel.partitions());
        let mut scratch = uniform.make_scratch();
        let mut block = vec![0.0f32; 32];
        let mut read = 32;
        let mut want = Vec::new();
        for &s in &x {
            if input.push(s) {
                uniform.analyze(&mut input);
                uniform.synthesize(&input, &kernel, &mut scratch, &mut block);
                read = 0;
            }
            want.push(if read < 32 {
                read += 1;
                block[read - 1]
            } else {
                0.0
            });
        }
        assert_eq!(y, want);
    }

    /// A kernel swap: the head ramps on the block it lands on, each tail
    /// segment over the first head block of the first output block whose
    /// period starts on or after it. Rebuilt here from the per-level streams
    /// of the two kernels.
    #[test]
    fn a_swap_ramps_each_segment_at_its_next_period() {
        let ladder = [16, 64, 256];
        let plan = NonUniformPlan::new(&ladder);
        let taps = 1500;
        let (ha, hb) = (noise(taps, 21), noise(taps, 22));
        let (ka, kb) = (plan.partition(&ha, 3), plan.partition(&hb, 3));
        let x = noise(8000, 23);
        for swap_block in [0, 5, 15, 16, 31, 40] {
            let y = stream(&plan, &ka, Some((swap_block, &kb)), &x);
            let mut want = vec![0.0f64; x.len()];
            for level in 0..3 {
                let span = plan.span(level, 3, taps);
                let only = |h: &[f32]| {
                    let mut m = vec![0.0f32; taps];
                    m[span.clone()].copy_from_slice(&h[span.clone()]);
                    direct(&x, &m, plan.head().latency_samples())
                };
                let (a, b) = (only(&ha), only(&hb));
                // First head block (counted from 0) whose output carries
                // the incoming kernel: the swap block for the head; for a
                // tail segment, one period after the first period start on
                // or after the swap.
                let period = ladder[level] / 16;
                let first = if level == 0 {
                    swap_block
                } else {
                    (swap_block + 1).div_ceil(period) * period - 1 + period
                };
                // Head block `c` is read from output sample `16·c + 15`.
                let start = 16 * first + 15;
                for (n, w) in want.iter_mut().enumerate() {
                    let ramp = if n < start {
                        0.0
                    } else {
                        ((n - start + 1) as f64 / 16.0).min(1.0)
                    };
                    *w += a[n] + (b[n] - a[n]) * ramp;
                }
            }
            let (err, peak) = max_error(&y, &want);
            assert!(
                err < 2e-5 * peak,
                "swap on block {swap_block}: max err {err}, peak {peak}"
            );
        }
    }

    /// The schedule: after each head block of a period a segment has done at
    /// least that block's share of the period's cost and at most one task
    /// more, and all of it by the last — no head block inherits the period.
    #[test]
    fn a_period_is_spread_over_its_head_blocks() {
        let plan = NonUniformPlan::new(&[16, 256]);
        let taps = 4000;
        let kernels: Vec<NonUniformKernel> = (0..6)
            .map(|i| plan.partition(&noise(taps, 50 + i), 2))
            .collect();
        let (inputs, outputs) = (3, 2);
        let mut tails = plan.make_tails(2, taps, inputs, outputs);
        let block = noise(16, 60);
        let mut out = vec![vec![0.0f32; 16]; outputs];
        assert!(kernels[0].tail(0).partitions() > 3 * RUN_PARTITIONS);
        let largest = TRANSFORM_COST.max(RUN_PARTITIONS as u64);
        for head_block in 0..(16 * 5) {
            tails.feed((0..inputs).map(|_| block.as_slice()));
            let blend = head_block >= 16 * 3;
            tails.run(
                0,
                blend,
                |pass, i, o| Some(kernels[(pass + i * outputs + o) % 6].tail(0)),
                &mut out,
            );
            let seg = &tails.segments[0];
            if seg.tasks == 0 {
                continue;
            }
            let share = seg.budget * (seg.slot as u64 + 1);
            let done = seg.spent * seg.period as u64;
            assert!(done >= share, "slot {} is behind", seg.slot);
            assert!(
                done < share + largest * seg.period as u64,
                "slot {} ran more than one task ahead",
                seg.slot
            );
            if seg.slot + 1 == seg.period {
                assert_eq!(seg.task, seg.tasks);
                assert_eq!(seg.spent, seg.budget);
            }
        }
    }
}
