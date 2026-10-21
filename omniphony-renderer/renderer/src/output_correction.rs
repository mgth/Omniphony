//! Per-output FIR correction: one convolution per physical output.
//!
//! Room correction filters (designed by Omniphony Companion, or by any tool
//! that exports a FIR as a mono WAV: REW, rePhase, DRC-FIR, …) run on the
//! rendered speaker feeds, after the mix and before each output's gain and
//! delay. Every output goes through the stage once one has a filter, so all
//! of them share the same latency: an output without a filter is delayed by
//! the stage's block, never filtered.
//!
//! # Streaming
//!
//! Filters are cut along a non-uniform ladder ([`CORRECTION_LADDER`]): the
//! head runs on [`CORRECTION_BLOCK`]-sample blocks, which sets the latency
//! (`CORRECTION_BLOCK − 1` samples, ~2.6 ms at 48 kHz), and the rest of a
//! filter — several thousand taps for a bass correction — on larger blocks
//! whose work is spread over the head blocks ([`TailStreams`]). Nothing on
//! the audio thread allocates or reads a file: filters are read, resampled
//! to the engine rate and partitioned on the stage's worker thread, handed
//! over through an [`ArcSwapOption`], and what the audio thread retires is
//! sent back to the worker to be freed there.
//!
//! # Changes
//!
//! A change of filters (a new file, the correction switched off) keeps the
//! streams and ramps from the outgoing filters to the incoming ones over one
//! block, the way the BRIR stage swaps head orientations; switching the
//! correction off keeps the stage running with no filter (the latency does
//! not move under the listener, which is what an A/B comparison needs). Only
//! the first filter, the last one going away, a change of output count or a
//! filter longer than the streams were sized for rebuild the streams, which
//! restarts the outputs from silence for one block.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};

use arc_swap::ArcSwapOption;

use crate::partitioned_conv::nonuniform::{NonUniformKernel, NonUniformPlan, TailStreams};
use crate::partitioned_conv::{InputHistory, OutputScratch, PartitionedKernel};

/// Head block of the stage: its latency is one block less one sample.
pub const CORRECTION_BLOCK: usize = 128;

/// Block sizes a filter is cut into, the head first (see
/// [`NonUniformPlan::levels_for`]): a filter of a few hundred taps stays on
/// the head, a bass correction of tens of thousands reaches the 8192 level.
pub const CORRECTION_LADDER: [usize; 4] = [CORRECTION_BLOCK, 512, 2048, 8192];

/// Longest filter accepted, in seconds at the engine rate. A bass
/// correction rarely needs half of it; the bound keeps a wrong file (a
/// recording picked instead of a filter) from costing seconds of
/// convolution per output.
pub const MAX_FILTER_SECONDS: f64 = 1.5;

/// Largest filter file read, bytes.
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Where one output's filter comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilterSource {
    /// A mono WAV file (PCM or float) holding the impulse response.
    File(Arc<str>),
}

impl FilterSource {
    /// How the source is shown to the user.
    pub fn describe(&self) -> &str {
        match self {
            Self::File(path) => path,
        }
    }
}

/// Source of fresh [`OutputFilters::generation`] values: one counter for the
/// whole process, so two different filter sets never share a generation.
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

/// What each output is filtered with: the live state the stage follows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OutputFilters {
    /// Changes with every edit, so the stage compares one number per frame.
    /// Zero for the empty set.
    pub generation: u64,
    /// Indexed by output (layout order); a missing entry or `None` leaves
    /// that output unfiltered.
    pub sources: Arc<[Option<FilterSource>]>,
}

impl OutputFilters {
    /// The filters `sources` name, with a fresh generation; the empty set
    /// when none does.
    pub fn new(sources: Vec<Option<FilterSource>>) -> Self {
        if sources.iter().all(Option::is_none) {
            return Self::default();
        }
        Self {
            generation: NEXT_GENERATION.fetch_add(1, Ordering::Relaxed),
            sources: sources.into(),
        }
    }

    /// The filters a layout names (`Speaker::correction_filter`). Shared by
    /// renderer construction, a layout replacement and the live profile
    /// switch, like `live_params::speaker_live_from_layout`.
    pub fn from_layout(layout: &crate::speaker_layout::SpeakerLayout) -> Self {
        Self::new(
            layout
                .speakers
                .iter()
                .map(|s| {
                    s.correction_filter
                        .as_deref()
                        .map(|path| FilterSource::File(path.into()))
                })
                .collect(),
        )
    }

    /// This set with output `index` filtered by `source` (`None`: no
    /// filter), or `None` when that is what it already does.
    pub fn with(&self, index: usize, source: Option<FilterSource>) -> Option<Self> {
        if self.source(index) == source.as_ref() {
            return None;
        }
        let mut sources: Vec<_> = self.sources.to_vec();
        if sources.len() <= index {
            sources.resize(index + 1, None);
        }
        sources[index] = source;
        Some(Self::new(sources))
    }

    /// The filter of output `index`.
    pub fn source(&self, index: usize) -> Option<&FilterSource> {
        self.sources.get(index).and_then(Option::as_ref)
    }

    /// Whether any output has a filter.
    pub fn any(&self) -> bool {
        self.sources.iter().any(Option::is_some)
    }
}

/// One output's filter as the worker found it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FilterStatus {
    /// The source, as [`FilterSource::describe`] gives it.
    pub source: String,
    /// Length at the engine rate, samples (0 when it could not be used).
    pub taps: usize,
    /// Rate the filter was designed at (the file's).
    pub source_rate: u32,
    /// Why the output plays unfiltered, when the filter could not be used.
    pub error: Option<String>,
}

/// The outcome of the last filter set the stage prepared.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OutputCorrectionStatus {
    /// [`OutputFilters::generation`] of the set.
    pub generation: u64,
    /// Engine rate the filters were prepared for.
    pub sample_rate: u32,
    /// Indexed by output; `None` where the output has no filter.
    pub outputs: Vec<Option<FilterStatus>>,
}

/// Where the worker reports each prepared set.
pub type OutputCorrectionStatusSink = Arc<dyn Fn(OutputCorrectionStatus) + Send + Sync>;

/// Read a filter and bring it to `sample_rate`: the samples and the rate the
/// file was at.
pub fn load_filter(source: &FilterSource, sample_rate: u32) -> Result<(Vec<f32>, u32), String> {
    match source {
        FilterSource::File(path) => {
            let path: &str = path;
            if path.trim().is_empty() {
                return Err("no filter file selected".to_string());
            }
            let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
            if meta.len() > MAX_FILE_BYTES {
                return Err(format!(
                    "{path}: {} MB is too large for a filter",
                    meta.len() / (1024 * 1024)
                ));
            }
            let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
            filter_from_wav(&bytes, sample_rate)
        }
    }
}

/// [`load_filter`] on a WAV file's bytes.
fn filter_from_wav(bytes: &[u8], sample_rate: u32) -> Result<(Vec<f32>, u32), String> {
    let wav = crate::object_test::clip::parse_wav(bytes)?;
    if wav.channels != 1 {
        return Err(format!(
            "the file has {} channels; a correction filter is one mono impulse response",
            wav.channels
        ));
    }
    let source_rate = wav.sample_rate;
    if source_rate == 0 {
        return Err("the file declares a sample rate of 0 Hz".to_string());
    }
    let max_taps = (MAX_FILTER_SECONDS * sample_rate as f64) as usize;
    // Bounded before resampling too: a header claiming a few hertz would
    // otherwise expand a short file a thousandfold.
    let max_source_taps = (MAX_FILTER_SECONDS * source_rate as f64) as usize + 64;
    if wav.samples.len() > max_source_taps {
        return Err(format!(
            "the filter is {:.2} s long; the limit is {MAX_FILTER_SECONDS} s",
            wav.samples.len() as f64 / source_rate as f64
        ));
    }
    let mut taps = if source_rate == sample_rate {
        wav.samples
    } else {
        resample_filter(&wav.samples, source_rate, sample_rate)
    };
    taps.truncate(max_taps);
    // Trailing zeros cost partitions for nothing.
    let end = taps.iter().rposition(|&t| t != 0.0).map_or(0, |i| i + 1);
    taps.truncate(end);
    if taps.is_empty() {
        return Err("the filter is silent".to_string());
    }
    if taps.iter().any(|t| !t.is_finite()) {
        return Err("the filter holds non-finite samples".to_string());
    }
    Ok((taps, source_rate))
}

/// A filter designed at `from` Hz as the same frequency response at `to` Hz.
/// Interpolating an impulse response at a different rate samples it at a
/// different density, which scales its gain by `to / from`; the
/// `from / to` factor puts it back.
fn resample_filter(taps: &[f32], from: u32, to: u32) -> Vec<f32> {
    let kernel = crate::binaural::measured::ResampleKernel::new(from, to);
    let mut out = Vec::new();
    kernel.resample_into(taps, &mut out);
    let scale = from as f32 / to as f32;
    for t in &mut out {
        *t *= scale;
    }
    out
}

/// What the stage is asked for: the filter set, whether the correction is
/// on, and the output count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Key {
    generation: u64,
    enabled: bool,
    outputs: usize,
}

/// How streams are sized: the output count and the longest filter, in
/// taps, they can serve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Geometry {
    outputs: usize,
    taps: usize,
}

/// The partitioned filters of every output, cut for one [`Geometry`].
struct Bank {
    geometry: Geometry,
    /// Indexed by output; `None` leaves the output unfiltered.
    kernels: Vec<Option<NonUniformKernel>>,
}

/// The banks a tail segment convolves: its blocks are computed a period
/// ahead, so it follows the stage's bank at its own period starts (as the
/// BRIR stage's do).
struct TailBanks {
    current: Arc<Bank>,
    outgoing: Option<Arc<Bank>>,
}

/// The streaming state of every output, sized for one [`Geometry`].
struct Streams {
    geometry: Geometry,
    /// One head history per output.
    inputs: Vec<InputHistory>,
    /// One tail per output: output `o` is fed by input `o` alone.
    tails: Vec<TailStreams>,
    /// One entry per tail segment.
    tail_banks: Vec<TailBanks>,
}

impl Streams {
    fn empty() -> Self {
        Self {
            geometry: Geometry {
                outputs: 0,
                taps: 0,
            },
            inputs: Vec::new(),
            tails: Vec::new(),
            tail_banks: Vec::new(),
        }
    }

    /// Silent state for `bank`'s geometry (allocating).
    fn new(plan: &NonUniformPlan, bank: &Arc<Bank>) -> Self {
        let Geometry { outputs, taps } = bank.geometry;
        let levels = plan.levels_for(taps);
        let capacity = plan.partitions_for(0, levels, taps);
        Self {
            geometry: bank.geometry,
            inputs: (0..outputs)
                .map(|_| plan.head().make_input(capacity))
                .collect(),
            tails: (0..outputs)
                .map(|_| plan.make_tails(levels, taps, 1, 1))
                .collect(),
            tail_banks: (1..levels)
                .map(|_| TailBanks {
                    current: Arc::clone(bank),
                    outgoing: None,
                })
                .collect(),
        }
    }
}

/// A prepared set: the bank, and the streams when the geometry changed.
struct Prepared {
    key: Key,
    bank: Arc<Bank>,
    streams: Option<Streams>,
}

enum Request {
    Prepare {
        key: Key,
        filters: OutputFilters,
        /// The geometry of the streams the stage runs: the bank is cut for
        /// it when it fits, so the swap keeps them.
        current: Option<Geometry>,
    },
    /// Something the audio thread retired, carried here only to be freed.
    Drop(Retired),
}

type Retired = Box<dyn std::any::Any + Send>;

/// See the module doc.
pub struct OutputCorrectionStage {
    sample_rate: u32,
    plan: NonUniformPlan,
    /// What was last asked for.
    key: Option<Key>,
    bank: Option<Arc<Bank>>,
    /// The bank on its way out during the block a swap lands in.
    fade_from: Option<Arc<Bank>>,
    incoming: Arc<ArcSwapOption<Prepared>>,
    request_tx: mpsc::Sender<Request>,
    streams: Streams,
    /// The head's partition of a one-tap unit filter: what an output without
    /// a filter ramps from or to.
    identity: PartitionedKernel,
    scratch: OutputScratch,
    /// Per output: the block being produced.
    blocks: Vec<Vec<f32>>,
    /// Interleaved output of the last completed block.
    fifo: Vec<f32>,
    read_pos: usize,
    sink: OutputCorrectionStatusSink,
    synchronous_builds: bool,
}

impl OutputCorrectionStage {
    /// A stage whose status goes nowhere.
    pub fn new(sample_rate: u32) -> Self {
        Self::with_status_sink(sample_rate, Arc::new(|_| {}))
    }

    /// A stage reporting every prepared set to `sink`.
    pub fn with_status_sink(sample_rate: u32, sink: OutputCorrectionStatusSink) -> Self {
        Self::with_ladder(sample_rate, sink, &CORRECTION_LADDER)
    }

    fn with_ladder(sample_rate: u32, sink: OutputCorrectionStatusSink, ladder: &[usize]) -> Self {
        debug_assert_eq!(ladder[0], CORRECTION_BLOCK);
        let plan = NonUniformPlan::new(ladder);
        let incoming: Arc<ArcSwapOption<Prepared>> = Arc::new(ArcSwapOption::empty());
        let (request_tx, request_rx) = mpsc::channel::<Request>();
        {
            let plan = plan.clone();
            let slot = Arc::clone(&incoming);
            let sink = Arc::clone(&sink);
            std::thread::Builder::new()
                .name("output-correction-worker".into())
                .spawn(move || {
                    crate::background_pool::enter_background();
                    Self::worker(request_rx, plan, sample_rate, sink, slot)
                })
                .expect("spawn output correction worker");
        }
        Self {
            sample_rate,
            identity: plan.head().partition(&[1.0]),
            scratch: plan.head().make_scratch(),
            plan,
            key: None,
            bank: None,
            fade_from: None,
            incoming,
            request_tx,
            streams: Streams::empty(),
            blocks: Vec::new(),
            fifo: Vec::new(),
            read_pos: 0,
            sink,
            synchronous_builds: false,
        }
    }

    /// Prepare filter sets on the calling thread, inside [`Self::ensure`],
    /// so each lands on the frame that asks for it. For offline renders,
    /// whose output must not depend on how fast the worker is; a live host
    /// leaves it off (a set reads files).
    pub fn set_synchronous_builds(&mut self, on: bool) {
        self.synchronous_builds = on;
    }

    /// Engine rate the stage was built for.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Whether the outputs go through the stage.
    pub fn is_active(&self) -> bool {
        self.bank.is_some() && !self.streams.inputs.is_empty()
    }

    /// Samples the stage delays the outputs by: `CORRECTION_BLOCK − 1` while
    /// active, else 0.
    pub fn latency_samples(&self) -> usize {
        if self.is_active() {
            self.plan.head().latency_samples()
        } else {
            0
        }
    }

    fn retire(&self, r: Retired) {
        // A closed channel means the worker is gone (the stage is being torn
        // down); dropping here is then the only option.
        let _ = self.request_tx.send(Request::Drop(r));
    }

    /// The worker: reads and partitions filter sets, frees what the audio
    /// thread retires. Only the latest request queued is prepared.
    fn worker(
        rx: mpsc::Receiver<Request>,
        plan: NonUniformPlan,
        sample_rate: u32,
        sink: OutputCorrectionStatusSink,
        slot: Arc<ArcSwapOption<Prepared>>,
    ) {
        while let Ok(first) = rx.recv() {
            let mut latest = None;
            let mut handle = |req: Request| match req {
                Request::Prepare {
                    key,
                    filters,
                    current,
                } => latest = Some((key, filters, current)),
                Request::Drop(retired) => drop(retired),
            };
            handle(first);
            while let Ok(next) = rx.try_recv() {
                handle(next);
            }
            if let Some((key, filters, current)) = latest {
                let prepared = Self::prepare(&plan, key, &filters, current, sample_rate, &sink);
                slot.store(Some(Arc::new(prepared)));
            }
        }
    }

    /// Read, resample and partition `filters` for `key` (allocating), and
    /// report the outcome to `sink`. The bank is cut for the `current`
    /// streams when every filter fits them; otherwise new streams come with
    /// it.
    fn prepare(
        plan: &NonUniformPlan,
        key: Key,
        filters: &OutputFilters,
        current: Option<Geometry>,
        sample_rate: u32,
        sink: &OutputCorrectionStatusSink,
    ) -> Prepared {
        let mut taps: Vec<Option<Vec<f32>>> = Vec::with_capacity(key.outputs);
        let mut statuses = Vec::with_capacity(key.outputs);
        for output in 0..key.outputs {
            let Some(source) = filters.source(output) else {
                taps.push(None);
                statuses.push(None);
                continue;
            };
            let mut status = FilterStatus {
                source: source.describe().to_string(),
                ..FilterStatus::default()
            };
            match load_filter(source, sample_rate) {
                Ok((samples, source_rate)) => {
                    status.taps = samples.len();
                    status.source_rate = source_rate;
                    taps.push(key.enabled.then_some(samples));
                }
                Err(e) => {
                    log::warn!(
                        "output correction: output {output} plays unfiltered: {e} ({})",
                        status.source
                    );
                    status.error = Some(e);
                    taps.push(None);
                }
            }
            statuses.push(Some(status));
        }
        sink(OutputCorrectionStatus {
            generation: filters.generation,
            sample_rate,
            outputs: statuses,
        });
        let longest = taps.iter().flatten().map(Vec::len).max().unwrap_or(1);
        let geometry = match current {
            Some(g) if g.outputs == key.outputs && longest <= g.taps => g,
            _ => Geometry {
                outputs: key.outputs,
                taps: longest,
            },
        };
        let levels = plan.levels_for(geometry.taps);
        let bank = Arc::new(Bank {
            geometry,
            kernels: taps
                .iter()
                .map(|t| t.as_deref().map(|t| plan.partition(t, levels)))
                .collect(),
        });
        let streams = (current != Some(geometry)).then(|| Streams::new(plan, &bank));
        Prepared { key, bank, streams }
    }

    /// Follow the live filters; called once per frame from the audio thread
    /// before [`Self::process`]. Steady state is one compare. A change asks
    /// the worker for the new set; the stage keeps the current one until it
    /// lands. Without any filter the stage stops at once.
    pub fn ensure(&mut self, filters: &OutputFilters, enabled: bool, outputs: usize) {
        if !filters.any() || outputs == 0 {
            if self.key.is_some() {
                self.stop();
            }
            return;
        }
        let key = Key {
            generation: filters.generation,
            enabled,
            outputs,
        };
        if self.key != Some(key) {
            self.key = Some(key);
            self.request(key, filters.clone());
        }
        if let Some(prepared) = self.incoming.swap(None) {
            match Arc::try_unwrap(prepared) {
                Ok(prepared) if self.key == Some(prepared.key) => self.adopt(prepared, filters),
                Ok(prepared) => self.retire(Box::new(prepared)),
                Err(_) => unreachable!("the incoming slot held the only reference"),
            }
        }
    }

    /// Ask for `key`'s set: through the worker, or here with synchronous
    /// builds (published through the worker's slot, so it is adopted the same
    /// way, on this frame).
    fn request(&mut self, key: Key, filters: OutputFilters) {
        let current = (!self.streams.inputs.is_empty()).then_some(self.streams.geometry);
        if self.synchronous_builds {
            let prepared = Self::prepare(
                &self.plan,
                key,
                &filters,
                current,
                self.sample_rate,
                &self.sink,
            );
            self.incoming.store(Some(Arc::new(prepared)));
        } else {
            let _ = self.request_tx.send(Request::Prepare {
                key,
                filters,
                current,
            });
        }
    }

    /// Take a prepared set: new streams start from silence, a bank for the
    /// running streams ramps in over the next block.
    fn adopt(&mut self, prepared: Prepared, filters: &OutputFilters) {
        let Prepared { bank, streams, key } = prepared;
        if let Some(streams) = streams {
            let old = std::mem::replace(&mut self.streams, streams);
            if !old.inputs.is_empty() {
                self.retire(Box::new(old));
            }
            if let Some(old) = self.bank.replace(bank) {
                self.retire(Box::new(old));
            }
            if let Some(old) = self.fade_from.take() {
                self.retire(Box::new(old));
            }
            let outputs = self.streams.geometry.outputs;
            if self.blocks.len() != outputs {
                self.blocks = vec![vec![0.0; CORRECTION_BLOCK]; outputs];
            }
            self.fifo.clear();
            self.fifo.resize(outputs * CORRECTION_BLOCK, 0.0);
            self.read_pos = 0;
            return;
        }
        if bank.geometry != self.streams.geometry {
            // Cut for streams that were replaced while it was prepared (a
            // set landing in between): ask again for the streams running now.
            self.retire(Box::new(bank));
            self.request(key, filters.clone());
            return;
        }
        match self.bank.replace(bank) {
            Some(old) => {
                if let Some(prev) = self.fade_from.replace(old) {
                    // Two swaps in one block: the middle bank never played.
                    self.retire(Box::new(prev));
                }
            }
            None => unreachable!("a bank without streams arrives only for running streams"),
        }
    }

    /// Take the outputs out of the stage (a path that does not use it):
    /// one compare when already out. The next [`Self::ensure`] starts over.
    pub fn deactivate(&mut self) {
        if self.key.is_some() {
            self.stop();
        }
    }

    /// Drop the filters and the streams: the outputs leave the stage.
    fn stop(&mut self) {
        self.key = None;
        if let Some(old) = self.bank.take() {
            self.retire(Box::new(old));
        }
        if let Some(old) = self.fade_from.take() {
            self.retire(Box::new(old));
        }
        let old = std::mem::replace(&mut self.streams, Streams::empty());
        if !old.inputs.is_empty() {
            self.retire(Box::new(old));
        }
        // A set still on its way is for the filters just dropped.
        if let Some(prepared) = self.incoming.swap(None) {
            self.retire(Box::new(prepared));
        }
    }

    /// Filter the interleaved speaker feeds in `out` (`outputs` per sample)
    /// in place. Does nothing while the stage is inactive or prepared for
    /// another output count.
    pub fn process(&mut self, out: &mut [f32], outputs: usize) {
        if !self.is_active() || self.streams.geometry.outputs != outputs || outputs == 0 {
            return;
        }
        for frame in out.chunks_exact_mut(outputs) {
            let mut complete = false;
            for (input, &x) in self.streams.inputs.iter_mut().zip(frame.iter()) {
                complete = input.push(x);
            }
            if complete {
                self.process_block();
            }
            let at = self.read_pos * outputs;
            frame.copy_from_slice(&self.fifo[at..at + outputs]);
            self.read_pos += 1;
        }
    }

    /// One completed block: analyse every output, run the head filters
    /// (ramping a landing swap), then let every tail segment do this block's
    /// share of its work and add its output.
    fn process_block(&mut self) {
        let plan = self.plan.head();
        let Streams {
            inputs,
            tails,
            tail_banks,
            ..
        } = &mut self.streams;
        for input in inputs.iter_mut() {
            plan.analyze(input);
        }
        for (tail, input) in tails.iter_mut().zip(inputs.iter()) {
            tail.feed(std::iter::once(input.last_block()));
        }
        let bank = self.bank.as_ref().expect("active");
        let fade_from = self.fade_from.as_deref();
        for (o, (input, out)) in inputs.iter().zip(&mut self.blocks).enumerate() {
            let incoming = bank.kernels[o].as_ref();
            match fade_from {
                None => match incoming {
                    None => out.copy_from_slice(input.last_block()),
                    Some(k) => plan.synthesize(input, k.head(), &mut self.scratch, out),
                },
                Some(old) => match (old.kernels[o].as_ref(), incoming) {
                    (None, None) => out.copy_from_slice(input.last_block()),
                    (from, to) => plan.synthesize_blend(
                        input,
                        from.map_or(&self.identity, NonUniformKernel::head),
                        to.map_or(&self.identity, NonUniformKernel::head),
                        &mut self.scratch,
                        out,
                    ),
                },
            }
        }
        for (segment, banks) in tail_banks.iter_mut().enumerate() {
            // Every output's tail was built and fed together: their periods
            // start on the same head block.
            if tails[0].period_starts(segment) {
                if let Some(old) = banks.outgoing.take() {
                    // What `retire` does, without borrowing the whole stage.
                    let _ = self.request_tx.send(Request::Drop(Box::new(old)));
                }
                if !Arc::ptr_eq(&banks.current, bank) {
                    banks.outgoing = Some(std::mem::replace(&mut banks.current, Arc::clone(bank)));
                }
            }
            let current = &*banks.current;
            let outgoing = banks.outgoing.as_deref();
            for (o, (tail, out)) in tails.iter_mut().zip(&mut self.blocks).enumerate() {
                tail.run(
                    segment,
                    outgoing.is_some(),
                    |pass, _, _| {
                        let bank = match outgoing {
                            Some(outgoing) if pass == 0 => outgoing,
                            _ => current,
                        };
                        bank.kernels[o].as_ref().map(|k| k.tail(segment))
                    },
                    std::slice::from_mut(out),
                );
            }
        }
        if let Some(old) = self.fade_from.take() {
            self.retire(Box::new(old));
        }
        let outputs = self.blocks.len();
        for (o, block) in self.blocks.iter().enumerate() {
            for (i, &v) in block.iter().enumerate() {
                self.fifo[i * outputs + o] = v;
            }
        }
        self.read_pos = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    const SMALL_LADDER: [usize; 3] = [CORRECTION_BLOCK, 256, 1024];

    /// Deterministic full-band test signal in [−1, 1].
    fn noise(len: usize, seed: u32) -> Vec<f32> {
        let mut s = seed;
        (0..len)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (s >> 8) as f32 / (1 << 24) as f32 * 2.0 - 1.0
            })
            .collect()
    }

    /// A decaying filter of `len` taps.
    fn filter(len: usize, seed: u32) -> Vec<f32> {
        noise(len, seed)
            .into_iter()
            .enumerate()
            .map(|(i, v)| v * (-(i as f32) / (len as f32 / 4.0)).exp())
            .collect()
    }

    /// A canonical mono 32-bit float WAV.
    fn wav_f32(rate: u32, samples: &[f32]) -> Vec<u8> {
        let data = samples.len() * 4;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&((36 + data) as u32).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&3u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * 4).to_le_bytes());
        b.extend_from_slice(&4u16.to_le_bytes());
        b.extend_from_slice(&32u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(data as u32).to_le_bytes());
        for s in samples {
            b.extend_from_slice(&s.to_le_bytes());
        }
        b
    }

    /// `taps` written as a WAV file in a fresh temporary directory.
    fn filter_file(name: &str, rate: u32, taps: &[f32]) -> (tempdir::Dir, FilterSource) {
        let dir = tempdir::Dir::new(name);
        let path = dir.path().join("filter.wav");
        std::fs::write(&path, wav_f32(rate, taps)).unwrap();
        let source = FilterSource::File(path.to_string_lossy().into());
        (dir, source)
    }

    mod tempdir {
        /// A directory removed on drop.
        pub struct Dir(std::path::PathBuf);
        impl Dir {
            pub fn new(name: &str) -> Self {
                let path = std::env::temp_dir().join(format!(
                    "orender-output-correction-{name}-{}",
                    std::process::id()
                ));
                let _ = std::fs::remove_dir_all(&path);
                std::fs::create_dir_all(&path).unwrap();
                Self(path)
            }
            pub fn path(&self) -> &std::path::Path {
                &self.0
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    fn stage(ladder: &[usize]) -> OutputCorrectionStage {
        let mut s = OutputCorrectionStage::with_ladder(RATE, Arc::new(|_| {}), ladder);
        s.set_synchronous_builds(true);
        s
    }

    /// Interleave per-output signals.
    fn interleave(signals: &[Vec<f32>]) -> Vec<f32> {
        let len = signals[0].len();
        (0..len)
            .flat_map(|i| signals.iter().map(move |s| s[i]))
            .collect()
    }

    fn channel(interleaved: &[f32], outputs: usize, o: usize) -> Vec<f32> {
        interleaved
            .iter()
            .skip(o)
            .step_by(outputs)
            .copied()
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

    fn residual_db(got: &[f32], want: &[f64]) -> f64 {
        let err: f64 = got
            .iter()
            .zip(want)
            .map(|(&g, &w)| (g as f64 - w).powi(2))
            .sum();
        let sig: f64 = want.iter().map(|w| w * w).sum();
        10.0 * (err / sig.max(1e-30)).log10()
    }

    /// Run `stage` over `input` in host chunks of `chunk` frames.
    fn run(
        stage: &mut OutputCorrectionStage,
        filters: &OutputFilters,
        enabled: bool,
        input: &[f32],
        outputs: usize,
        chunk: usize,
    ) -> Vec<f32> {
        let mut out = input.to_vec();
        for block in out.chunks_mut(chunk * outputs) {
            stage.ensure(filters, enabled, outputs);
            stage.process(block, outputs);
        }
        out
    }

    #[test]
    fn without_a_filter_the_stage_stays_out_of_the_way() {
        let mut s = stage(&CORRECTION_LADDER);
        let input = noise(3 * 1000, 1);
        let out = run(&mut s, &OutputFilters::default(), true, &input, 3, 256);
        assert_eq!(out, input);
        assert_eq!(s.latency_samples(), 0);
    }

    #[test]
    fn filtered_outputs_match_direct_convolution_and_others_are_only_delayed() {
        let outputs = 3;
        let h_long = filter(5000, 7);
        let h_short = filter(90, 8);
        let (_d1, long) = filter_file("match-long", RATE, &h_long);
        let (_d2, short) = filter_file("match-short", RATE, &h_short);
        let filters = OutputFilters::new(vec![Some(long), None, Some(short)]);
        let signals: Vec<Vec<f32>> = (0..outputs).map(|o| noise(12_000, 10 + o as u32)).collect();
        let latency = CORRECTION_BLOCK - 1;
        for ladder in [
            &CORRECTION_LADDER[..],
            &SMALL_LADDER[..],
            &[CORRECTION_BLOCK][..],
        ] {
            let mut s = stage(ladder);
            let out = run(&mut s, &filters, true, &interleave(&signals), outputs, 480);
            assert_eq!(s.latency_samples(), latency);
            let o0 = channel(&out, outputs, 0);
            let o2 = channel(&out, outputs, 2);
            assert!(
                residual_db(&o0, &direct(&signals[0], &h_long, latency)) < -100.0,
                "{ladder:?}"
            );
            assert!(
                residual_db(&o2, &direct(&signals[2], &h_short, latency)) < -100.0,
                "{ladder:?}"
            );
            let o1 = channel(&out, outputs, 1);
            assert_eq!(&o1[latency..], &signals[1][..signals[1].len() - latency]);
            assert!(o1[..latency].iter().all(|&v| v == 0.0));
        }
    }

    #[test]
    fn host_chunking_does_not_change_the_output() {
        let outputs = 2;
        let (_d, src) = filter_file("chunking", RATE, &filter(3000, 3));
        let filters = OutputFilters::new(vec![Some(src), None]);
        let input = interleave(&[noise(9000, 1), noise(9000, 2)]);
        let a = run(
            &mut stage(&CORRECTION_LADDER),
            &filters,
            true,
            &input,
            outputs,
            1024,
        );
        let b = run(
            &mut stage(&CORRECTION_LADDER),
            &filters,
            true,
            &input,
            outputs,
            37,
        );
        assert_eq!(a, b);
    }

    #[test]
    fn switching_the_correction_off_keeps_the_latency_and_ramps_to_the_dry_signal() {
        let outputs = 1;
        let h = filter(4000, 5);
        let (_d, src) = filter_file("toggle", RATE, &h);
        let filters = OutputFilters::new(vec![Some(src)]);
        let x = noise(40_000, 9);
        let mut s = stage(&CORRECTION_LADDER);
        let mut out = x.clone();
        let (first, second) = out.split_at_mut(20_000);
        s.ensure(&filters, true, outputs);
        s.process(first, outputs);
        s.ensure(&filters, false, outputs);
        s.process(second, outputs);
        assert_eq!(s.latency_samples(), CORRECTION_BLOCK - 1);
        // Long after the switch (the longest tail period, plus the filter
        // ringing out), the output is the dry input delayed by the latency.
        let lat = CORRECTION_BLOCK - 1;
        let settled = 20_000 + 2 * 8192 + 4000;
        for n in settled..x.len() {
            assert!((out[n] - x[n - lat]).abs() < 1e-5, "sample {n}");
        }
        // No jump at the switch larger than the signal's own steps.
        let max_step = out
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        let wet = direct(&x, &h, lat);
        let wet_step = wet
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f64::max);
        assert!(
            (max_step as f64) < 2.0 * wet_step.max(2.0),
            "{max_step} vs {wet_step}"
        );
    }

    #[test]
    fn a_new_filter_for_the_running_streams_ends_on_the_new_convolution() {
        let outputs = 2;
        let (h1, h2) = (filter(3000, 21), filter(2500, 22));
        let (_d1, a) = filter_file("swap-a", RATE, &h1);
        let (_d2, b) = filter_file("swap-b", RATE, &h2);
        let first = OutputFilters::new(vec![Some(a), None]);
        let second = first.with(0, Some(b)).unwrap();
        let x = noise(40_000, 4);
        let input = interleave(&[x.clone(), x.clone()]);
        let mut s = stage(&CORRECTION_LADDER);
        let mut out = input.clone();
        let (p1, p2) = out.split_at_mut(20_000 * outputs);
        s.ensure(&first, true, outputs);
        s.process(p1, outputs);
        s.ensure(&second, true, outputs);
        s.process(p2, outputs);
        let o0 = channel(&out, outputs, 0);
        let want = direct(&x, &h2, CORRECTION_BLOCK - 1);
        let settled = 20_000 + 2 * 8192 + 3000;
        assert!(residual_db(&o0[settled..], &want[settled..]) < -100.0);
    }

    #[test]
    fn an_unreadable_filter_reports_why_and_leaves_its_output_unfiltered() {
        let statuses = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let sink_statuses = Arc::clone(&statuses);
        let mut s = OutputCorrectionStage::with_status_sink(
            RATE,
            Arc::new(move |st| sink_statuses.lock().push(st)),
        );
        s.set_synchronous_builds(true);
        let (_d, good) = filter_file("unreadable", RATE, &[0.5]);
        let filters = OutputFilters::new(vec![
            Some(FilterSource::File("/nonexistent/filter.wav".into())),
            Some(good),
        ]);
        let x = noise(2 * 2000, 6);
        let out = run(&mut s, &filters, true, &x, 2, 512);
        let lat = CORRECTION_BLOCK - 1;
        let o0 = channel(&out, 2, 0);
        let o1 = channel(&out, 2, 1);
        let x0 = channel(&x, 2, 0);
        let x1 = channel(&x, 2, 1);
        assert_eq!(&o0[lat..], &x0[..x0.len() - lat]);
        for n in lat..o1.len() {
            assert!((o1[n] - 0.5 * x1[n - lat]).abs() < 1e-6);
        }
        let statuses = statuses.lock();
        let last = statuses.last().unwrap();
        assert_eq!(last.generation, filters.generation);
        let bad = last.outputs[0].as_ref().unwrap();
        assert!(
            bad.error
                .as_deref()
                .unwrap()
                .contains("/nonexistent/filter.wav")
        );
        let ok = last.outputs[1].as_ref().unwrap();
        assert_eq!(
            (ok.taps, ok.source_rate, ok.error.as_deref()),
            (1, RATE, None)
        );
    }

    #[test]
    fn removing_the_last_filter_takes_the_outputs_out_of_the_stage() {
        let (_d, src) = filter_file("remove", RATE, &[1.0]);
        let filters = OutputFilters::new(vec![Some(src)]);
        let mut s = stage(&CORRECTION_LADDER);
        let x = noise(1000, 2);
        run(&mut s, &filters, true, &x, 1, 256);
        assert!(s.is_active());
        let none = filters.with(0, None).unwrap();
        assert!(!none.any());
        let out = run(&mut s, &none, true, &x, 1, 256);
        assert!(!s.is_active());
        assert_eq!(out, x);
    }

    #[test]
    fn a_filter_at_another_rate_keeps_its_frequency_response() {
        // A one-pole lowpass at 44.1 kHz: its DC gain (the sum of the taps)
        // and its gain at 1 kHz must survive the move to 48 kHz.
        let from = 44_100;
        let a = 0.9f32;
        let taps: Vec<f32> = (0..2000).map(|n| (1.0 - a) * a.powi(n)).collect();
        let (out, rate) = filter_from_wav(&wav_f32(from, &taps), RATE).unwrap();
        assert_eq!(rate, from);
        // The filter starts on a step, which the interpolation smooths a
        // little: a few thousandths of a dB at DC.
        let dc: f32 = out.iter().sum();
        assert!((20.0 * dc.log10()).abs() < 0.05, "DC gain {dc}");
        let gain_at = |h: &[f32], fs: f32, f: f32| {
            let (re, im) = h
                .iter()
                .enumerate()
                .fold((0.0f64, 0.0f64), |(re, im), (n, &v)| {
                    let w = -2.0 * std::f64::consts::PI * f as f64 * n as f64 / fs as f64;
                    (re + v as f64 * w.cos(), im + v as f64 * w.sin())
                });
            (re * re + im * im).sqrt()
        };
        let g_src = gain_at(&taps, from as f32, 1000.0);
        let g_dst = gain_at(&out, RATE as f32, 1000.0);
        assert!((20.0 * (g_dst / g_src).log10()).abs() < 0.05);
    }

    #[test]
    fn a_stereo_or_overlong_file_is_refused() {
        let mut stereo = wav_f32(RATE, &[0.5, 0.5]);
        stereo[22] = 2; // channel count
        assert!(
            filter_from_wav(&stereo, RATE)
                .unwrap_err()
                .contains("2 channels")
        );
        let long = vec![0.1f32; (2.0 * RATE as f64) as usize];
        assert!(
            filter_from_wav(&wav_f32(RATE, &long), RATE)
                .unwrap_err()
                .contains("limit")
        );
        assert!(
            filter_from_wav(&wav_f32(RATE, &[0.0; 16]), RATE)
                .unwrap_err()
                .contains("silent")
        );
    }

    #[test]
    fn layout_filters_follow_the_speakers() {
        let mut layout = crate::speaker_layout::SpeakerLayout::preset("stereo").unwrap();
        assert!(!OutputFilters::from_layout(&layout).any());
        layout.speakers[1].correction_filter = Some("/x/right.wav".into());
        let f = OutputFilters::from_layout(&layout);
        assert_eq!(f.source(0), None);
        assert_eq!(
            f.source(1),
            Some(&FilterSource::File("/x/right.wav".into()))
        );
        assert_ne!(f.generation, 0);
        assert!(
            f.with(1, Some(FilterSource::File("/x/right.wav".into())))
                .is_none()
        );
        let g = f
            .with(0, Some(FilterSource::File("/x/left.wav".into())))
            .unwrap();
        assert_ne!(g.generation, f.generation);
    }
}
