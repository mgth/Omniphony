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

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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
/// convolution per output. The measurement protocol's bound on uploads.
pub const MAX_FILTER_SECONDS: f64 = omniphony_osc_contract::measurement::MAX_FILTER_SECONDS;

/// Largest filter file read, bytes.
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Where one output's filter comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilterSource {
    /// A mono WAV file (PCM or float) holding the impulse response.
    File(Arc<str>),
    /// An impulse response handed over as samples (a measurement client's
    /// upload): live state, written to a file only by a Save.
    Samples(Arc<FilterSamples>),
}

impl FilterSource {
    /// How the source is shown to the user.
    pub fn describe(&self) -> Cow<'_, str> {
        match self {
            Self::File(path) => Cow::Borrowed(path),
            Self::Samples(samples) => Cow::Owned(format!(
                "live filter ({} taps at {} Hz)",
                samples.taps.len(),
                samples.sample_rate
            )),
        }
    }
}

/// A filter's impulse response, at the rate it was designed at.
pub struct FilterSamples {
    pub sample_rate: u32,
    pub taps: Box<[f32]>,
    /// [`omniphony_osc_contract::measurement::taps_hash`] of `taps`.
    pub hash: u64,
}

impl FilterSamples {
    /// `taps` at `sample_rate`, checked as a file's would be against an
    /// engine at `engine_rate`: a rate the stage can use, within
    /// [`MAX_FILTER_SECONDS`], finite and not silent.
    pub fn new(sample_rate: u32, taps: Vec<f32>, engine_rate: u32) -> Result<Self, String> {
        check_source_rate(sample_rate, engine_rate)?;
        if taps.len() > max_source_taps(sample_rate) {
            return Err(format!(
                "the filter is {:.2} s long; the limit is {MAX_FILTER_SECONDS} s",
                taps.len() as f64 / sample_rate as f64
            ));
        }
        if taps.iter().any(|t| !t.is_finite()) {
            return Err("the filter holds non-finite samples".to_string());
        }
        if taps.iter().all(|&t| t == 0.0) {
            return Err("the filter is silent".to_string());
        }
        Ok(Self {
            sample_rate,
            hash: omniphony_osc_contract::measurement::taps_hash(&taps),
            taps: taps.into_boxed_slice(),
        })
    }
}

impl PartialEq for FilterSamples {
    fn eq(&self, other: &Self) -> bool {
        self.sample_rate == other.sample_rate
            && self.hash == other.hash
            && self.taps.len() == other.taps.len()
            && self
                .taps
                .iter()
                .zip(other.taps.iter())
                .all(|(a, b)| a.to_bits() == b.to_bits())
    }
}

impl Eq for FilterSamples {}

impl std::fmt::Debug for FilterSamples {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilterSamples")
            .field("sample_rate", &self.sample_rate)
            .field("taps", &self.taps.len())
            .field("hash", &format_args!("{:016x}", self.hash))
            .finish()
    }
}

/// Filters handed over as samples and not saved yet, by output name (the
/// layout's speaker name, the measurement protocol's output ID). Kept apart
/// from the layout, which names files, and laid over it by
/// [`OutputFilters::for_layout`]: a layout edit that moves, adds, removes or
/// replaces speakers then carries each one with its speaker, as it carries a
/// file. A Save turns them into files; a Reload or a profile switch drops
/// them.
pub type InlineFilters = Arc<BTreeMap<String, Arc<FilterSamples>>>;

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
        Self::for_layout(layout, &BTreeMap::new())
    }

    /// The filters a layout names, with the `inline` ones (by speaker name)
    /// taking the place of a speaker's file.
    pub fn for_layout(
        layout: &crate::speaker_layout::SpeakerLayout,
        inline: &BTreeMap<String, Arc<FilterSamples>>,
    ) -> Self {
        Self::new(
            layout
                .speakers
                .iter()
                .map(|s| match inline.get(&s.name) {
                    Some(samples) => Some(FilterSource::Samples(Arc::clone(samples))),
                    None => s
                        .correction_filter
                        .as_deref()
                        .map(|path| FilterSource::File(path.into())),
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
    /// Length at that rate, as read (0 when it could not be used).
    pub source_taps: usize,
    /// [`omniphony_osc_contract::measurement::taps_hash`] of the taps as
    /// read, at that rate (0 when it could not be used).
    pub hash: u64,
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

/// A filter brought to the engine rate.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedFilter {
    /// The impulse response at the engine rate.
    pub taps: Vec<f32>,
    /// Rate the filter was designed at (the file's).
    pub source_rate: u32,
    /// Length at that rate, as read.
    pub source_taps: usize,
    /// Content hash of the taps as read ([`FilterStatus::hash`]).
    pub hash: u64,
    /// Samples the response was moved later by to keep the whole of it
    /// through a rate change ([`resample_filter`]); 0 at the engine rate.
    pub delay: usize,
}

/// Read a filter and bring it to `sample_rate`.
pub fn load_filter(source: &FilterSource, sample_rate: u32) -> Result<LoadedFilter, String> {
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
        FilterSource::Samples(samples) => {
            filter_from_samples(samples.taps.to_vec(), samples.sample_rate, sample_rate)
        }
    }
}

/// Whether a filter at `source_rate` can play at `sample_rate`.
fn check_source_rate(source_rate: u32, sample_rate: u32) -> Result<(), String> {
    if source_rate == 0 {
        return Err("the filter declares a sample rate of 0 Hz".to_string());
    }
    if source_rate != sample_rate && !FILTER_SOURCE_RATES.contains(&source_rate) {
        return Err(format!(
            "a filter at {source_rate} Hz cannot be used at {sample_rate} Hz: a filter at \
             another rate than the engine's must be designed between {} and {} Hz",
            FILTER_SOURCE_RATES.start(),
            FILTER_SOURCE_RATES.end()
        ));
    }
    Ok(())
}

/// Most taps a filter designed at `source_rate` may have: bounded at its own
/// rate, before resampling, so a header claiming a few hertz cannot expand a
/// short file a thousandfold.
pub fn max_source_taps(source_rate: u32) -> usize {
    omniphony_osc_contract::measurement::max_filter_taps(source_rate) + 64
}

/// [`load_filter`] on a WAV file's bytes.
fn filter_from_wav(bytes: &[u8], sample_rate: u32) -> Result<LoadedFilter, String> {
    let wav = crate::object_test::clip::parse_wav(bytes)?;
    if wav.channels != 1 {
        return Err(format!(
            "the file has {} channels; a correction filter is one mono impulse response",
            wav.channels
        ));
    }
    filter_from_samples(wav.samples, wav.sample_rate, sample_rate)
}

/// A filter's taps at `source_rate`, brought to `sample_rate`.
fn filter_from_samples(
    samples: Vec<f32>,
    source_rate: u32,
    sample_rate: u32,
) -> Result<LoadedFilter, String> {
    check_source_rate(source_rate, sample_rate)?;
    let max_taps = (MAX_FILTER_SECONDS * sample_rate as f64) as usize;
    if samples.len() > max_source_taps(source_rate) {
        return Err(format!(
            "the filter is {:.2} s long; the limit is {MAX_FILTER_SECONDS} s",
            samples.len() as f64 / source_rate as f64
        ));
    }
    if samples.iter().any(|t| !t.is_finite()) {
        return Err("the filter holds non-finite samples".to_string());
    }
    let source_taps = samples.len();
    let hash = omniphony_osc_contract::measurement::taps_hash(&samples);
    let (mut taps, delay) = if source_rate == sample_rate {
        (samples, 0)
    } else {
        resample_filter(&samples, source_rate, sample_rate)
    };
    taps.truncate(max_taps + delay);
    // Trailing zeros cost partitions for nothing.
    let end = taps.iter().rposition(|&t| t != 0.0).map_or(0, |i| i + 1);
    taps.truncate(end);
    if taps.is_empty() {
        return Err("the filter is silent".to_string());
    }
    Ok(LoadedFilter {
        taps,
        source_rate,
        source_taps,
        hash,
        delay,
    })
}

/// Half-width of the interpolation kernel that moves a filter between
/// rates, in samples of the lower rate. Long enough for a flat passband to
/// 90 % of the lower Nyquist with the images and aliases ~90 dB down; the
/// binaural resampler's 16 is made for HRIRs, which nobody listens to near
/// Nyquist, and lets a downsampled filter alias by most of a dB at 19 kHz.
const FILTER_HALF_WIDTH: usize = 64;

/// Kaiser window shape for that kernel (≈ 90 dB of stopband).
const FILTER_KAISER_BETA: f64 = 8.6;

/// Cut-off of that kernel as a fraction of the lower Nyquist: the band above
/// it, which only a filter's top 5 % reaches, is given to the transition.
const FILTER_CUTOFF: f64 = 0.95;

/// Points of the tabulated kernel per sample of the source, interpolated
/// linearly between (≈ −95 dB of interpolation error).
const FILTER_KERNEL_OVERSAMPLING: usize = 256;

/// Rates a filter file may be designed at when the engine runs at another
/// one. Below, a correction filter has no treble to correct; above, nothing
/// records or designs at. The bounds also bound the resampling: its delay
/// (the kernel's half-width at the lower rate, at most 2 ms) and its table.
pub const FILTER_SOURCE_RATES: std::ops::RangeInclusive<u32> =
    omniphony_osc_contract::measurement::FILTER_SOURCE_RATES;

/// A filter designed at `from` Hz as the same response at `to` Hz, and the
/// samples it was moved later by.
///
/// Band-limited interpolation spreads each tap over a symmetric kernel: a
/// tap at time zero also lands on the samples before it. A causal filter
/// with energy at its start (a minimum-phase correction, or the identity)
/// keeps its response only with that part, so the response is moved later by
/// the kernel's half-width, rounded up to whole samples at `to`: about
/// 1.3 ms, 2 ms at most within [`FILTER_SOURCE_RATES`]. The stage delays
/// every output by the same amount, so the outputs stay aligned.
///
/// Interpolating an impulse response at another rate also samples it at
/// another density, which scales its gain by `to / from`; the `from / to`
/// factor puts it back.
fn resample_filter(taps: &[f32], from: u32, to: u32) -> (Vec<f32>, usize) {
    let low = from.min(to) as f64;
    // In samples of the source: the kernel's half-width at the lower rate.
    let half = (FILTER_HALF_WIDTH as f64 * from as f64 / low).ceil() as usize;
    let delay = (half as f64 * to as f64 / from as f64).ceil() as usize;
    let cutoff = FILTER_CUTOFF * (to as f64 / from as f64).min(1.0);
    // The kernel at every 1/OVERSAMPLING of a source sample over its width.
    let os = FILTER_KERNEL_OVERSAMPLING;
    let i0_beta = bessel_i0(FILTER_KAISER_BETA);
    let kernel: Vec<f64> = (0..=2 * half * os + 1)
        .map(|i| {
            let d = i as f64 / os as f64 - half as f64;
            let x = d / half as f64;
            if x.abs() >= 1.0 {
                return 0.0;
            }
            let window = bessel_i0(FILTER_KAISER_BETA * (1.0 - x * x).sqrt()) / i0_beta;
            let arg = std::f64::consts::PI * cutoff * d;
            let sinc = if arg.abs() < 1e-12 {
                1.0
            } else {
                arg.sin() / arg
            };
            cutoff * sinc * window
        })
        .collect();
    let at = |d: f64| {
        let pos = (d + half as f64) * os as f64;
        let i = pos.floor();
        let (k, f) = (i as usize, pos - i);
        if i < 0.0 || k + 1 >= kernel.len() {
            return 0.0;
        }
        kernel[k] + (kernel[k + 1] - kernel[k]) * f
    };
    // Output `n` reads the source at `(n − delay) · from / to`, kept as the
    // exact fraction of the reduced rates.
    let g = gcd(from, to) as i64;
    let (from_g, to_g) = (from as i64 / g, to as i64 / g);
    let out_len = delay + ((taps.len() + half) as u64 * to as u64).div_ceil(from as u64) as usize;
    let scale = from as f64 / to as f64;
    let out = (0..out_len)
        .map(|n| {
            let num = (n as i64 - delay as i64) * from_g;
            let (whole, frac) = (
                num.div_euclid(to_g),
                num.rem_euclid(to_g) as f64 / to_g as f64,
            );
            let first = whole - half as i64 + 1;
            let acc: f64 = (0..2 * half)
                .filter_map(|j| {
                    let k = first + j as i64;
                    let tap = *taps.get(usize::try_from(k).ok()?)?;
                    Some(tap as f64 * at(frac + half as f64 - 1.0 - j as f64))
                })
                .sum();
            (acc * scale) as f32
        })
        .collect();
    (out, delay)
}

/// Modified Bessel function of the first kind, order 0 (the Kaiser window's).
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let half = x / 2.0;
    for k in 1..64 {
        term *= half / k as f64;
        let t = term * term;
        sum += t;
        if t < sum * 1e-17 {
            break;
        }
    }
    sum
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
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
    /// Samples every output is delayed by besides the head block: the
    /// longest move of a resampled filter ([`resample_filter`]). An output
    /// without a filter then plays through a unit filter at this delay.
    delay: usize,
    /// Indexed by output; `None` leaves the output unfiltered (only when
    /// `delay` is 0).
    kernels: Vec<Option<NonUniformKernel>>,
}

/// The banks a tail segment convolves: its blocks are computed a period
/// ahead, so it follows the stage's bank at its own period starts (as the
/// BRIR stage's do).
struct TailBanks {
    current: Arc<Bank>,
    outgoing: Option<Arc<Bank>>,
}

/// The streaming state of every output, sized for one [`Geometry`], with the
/// buffers the stage writes its output into: everything the audio thread
/// needs to start on new streams, built on the worker.
struct Streams {
    geometry: Geometry,
    /// One head history per output.
    inputs: Vec<InputHistory>,
    /// One tail per output: output `o` is fed by input `o` alone.
    tails: Vec<TailStreams>,
    /// One entry per tail segment.
    tail_banks: Vec<TailBanks>,
    /// Per output: the block being produced.
    blocks: Vec<Vec<f32>>,
    /// Interleaved output of the last completed block.
    fifo: Vec<f32>,
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
            blocks: Vec::new(),
            fifo: Vec::new(),
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
            blocks: vec![vec![0.0; CORRECTION_BLOCK]; outputs],
            fifo: vec![0.0; outputs * CORRECTION_BLOCK],
        }
    }
}

/// A prepared set: the bank, and the streams when the geometry changed.
/// Shared with the audio thread through the incoming slot; the streams are
/// taken out under a lock nobody else holds by then, so the audio thread
/// moves them in without unwrapping (and freeing) the `Arc`.
struct Prepared {
    key: Key,
    bank: Arc<Bank>,
    streams: parking_lot::Mutex<Option<Streams>>,
}

/// What the audio thread hands to the reclaimer to be freed there. The
/// fields are never read: they are carried only to be dropped.
#[allow(dead_code)]
enum Retired {
    Bank(Arc<Bank>),
    Streams(Streams),
    Prepared(Arc<Prepared>),
    Filters(OutputFilters),
}

/// A filter set for the worker to prepare.
struct Request {
    key: Key,
    filters: OutputFilters,
    /// The geometry of the streams the stage runs: the bank is cut for it
    /// when it fits, so the swap keeps them.
    current: Option<Geometry>,
}

/// Requests queued for the worker. The stage counts them and sends no more
/// than this, so a send never fails (and never frees the request on the
/// audio thread): a change the queue cannot take is asked again on the next
/// frame, by which time the worker, which keeps only the latest, has caught
/// up.
const REQUEST_CAPACITY: usize = 4;

/// Retirements queued for the reclaimer, a thread that does nothing but free
/// them, so the queue drains even while the worker spends seconds preparing a
/// set. Both channels are bounded: their slots are allocated with them, and a
/// send from the audio thread never allocates.
const RETIRE_CAPACITY: usize = 64;

/// Retirements the stage keeps, in storage allocated with it, while the
/// reclaimer's queue is full; sent on as it drains.
const DEFERRED_CAPACITY: usize = 64;

/// Room [`Reclaim::has_room`] keeps free before the stage takes a new set:
/// what adopting one retires (the outgoing streams and bank, a bank on its
/// way out, the prepared set), what follows it (the fade's bank, one outgoing
/// bank per tail level) and what a deactivation then retires (bank, fade,
/// streams, a set in the slot), with margin. Past it, the stage keeps playing
/// the set it has until the reclaimer catches up, so a retirement never has
/// to be freed on the audio thread, however long the reclaimer stalls.
const RETIRE_RESERVE: usize = 16;

/// Where the audio thread hands what it replaces: the reclaimer's queue,
/// overflowing into storage allocated with the stage, with a count of what
/// has not been freed yet.
struct Reclaim {
    tx: mpsc::SyncSender<Retired>,
    /// Retired and not yet freed by the reclaimer (queued or deferred).
    pending: Arc<AtomicUsize>,
    /// Waiting for room in the queue (capacity [`DEFERRED_CAPACITY`], never
    /// grown).
    deferred: Vec<Retired>,
    /// Retirements freed on the audio thread for want of room. Zero by
    /// construction ([`RETIRE_RESERVE`]); kept to be checked.
    freed_here: u64,
}

impl Reclaim {
    /// The reclaimer's end and the receiver it drains.
    fn new() -> (Self, mpsc::Receiver<Retired>, Arc<AtomicUsize>) {
        let (tx, rx) = mpsc::sync_channel::<Retired>(RETIRE_CAPACITY);
        let pending = Arc::new(AtomicUsize::new(0));
        let reclaim = Self {
            tx,
            pending: Arc::clone(&pending),
            deferred: Vec::with_capacity(DEFERRED_CAPACITY),
            freed_here: 0,
        };
        (reclaim, rx, pending)
    }

    /// The reclaimer: frees what it receives and counts it out.
    fn drain(rx: mpsc::Receiver<Retired>, pending: &AtomicUsize) {
        for retired in rx {
            drop(retired);
            pending.fetch_sub(1, Ordering::AcqRel);
        }
    }

    /// Whether the stage may take a new set: its retirements, and those of a
    /// deactivation after it, fit in what is left ([`RETIRE_RESERVE`]).
    fn has_room(&self) -> bool {
        self.pending.load(Ordering::Acquire) + RETIRE_RESERVE <= RETIRE_CAPACITY + DEFERRED_CAPACITY
    }

    /// Hand `retired` to the reclaimer, or keep it while its queue is full.
    fn retire(&mut self, retired: Retired) {
        self.pending.fetch_add(1, Ordering::AcqRel);
        match self.tx.try_send(retired) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(retired)) => {
                if self.deferred.len() < self.deferred.capacity() {
                    self.deferred.push(retired);
                } else {
                    // Unreachable while the stage keeps to `has_room`.
                    self.pending.fetch_sub(1, Ordering::AcqRel);
                    self.freed_here += 1;
                    drop(retired);
                }
            }
            // The reclaimer is gone: the stage is being torn down.
            Err(mpsc::TrySendError::Disconnected(_)) => {
                self.pending.fetch_sub(1, Ordering::AcqRel);
            }
        }
    }

    /// Send on what waited for room in the queue.
    fn flush(&mut self) {
        while let Some(retired) = self.deferred.pop() {
            match self.tx.try_send(retired) {
                Ok(()) => {}
                Err(mpsc::TrySendError::Full(retired)) => {
                    // Back where it was: within the capacity it came from.
                    self.deferred.push(retired);
                    return;
                }
                Err(mpsc::TrySendError::Disconnected(_)) => return,
            }
        }
    }
}

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
    request_tx: mpsc::SyncSender<Request>,
    /// Requests sent and not yet taken by the worker.
    requests_in_flight: Arc<AtomicUsize>,
    reclaim: Reclaim,
    streams: Streams,
    /// The head's partition of a one-tap unit filter: what an output without
    /// a filter ramps from or to.
    identity: PartitionedKernel,
    scratch: OutputScratch,
    read_pos: usize,
    sink: OutputCorrectionStatusSink,
    synchronous_builds: bool,
    /// The key of the bank the stage plays (since [`Self::adopt`]).
    resident: Option<Key>,
    /// Where the stage says which set every output plays through alone
    /// ([`settled_key`]), 0 while a change is still on its way through the
    /// tail segments or the stage is out of the way.
    settled: Arc<AtomicU64>,
    /// `settled` holds the resident key.
    settled_published: bool,
    /// The resident set ran alone on the last block.
    settle_seen: bool,
}

/// What [`OutputCorrectionStage::set_settled_flag`]'s flag holds once every
/// output plays through filter set `generation` alone, with the correction
/// `enabled` or not: no fade and no tail segment still convolving with the
/// set before it. Never 0 (generations start at 1).
pub fn settled_key(generation: u64, enabled: bool) -> u64 {
    (generation << 1) | u64::from(enabled)
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
        let (stage, rx, pending) = Self::unreclaimed(sample_rate, sink, ladder);
        std::thread::Builder::new()
            .name("output-correction-reclaim".into())
            .spawn(move || {
                crate::background_pool::enter_background();
                Reclaim::drain(rx, &pending)
            })
            .expect("spawn output correction reclaimer");
        stage
    }

    /// The stage without its reclaimer: the receiver and the pending count
    /// it would drain, for [`Self::with_ladder`] to hand to one (and the
    /// tests, to stall it).
    fn unreclaimed(
        sample_rate: u32,
        sink: OutputCorrectionStatusSink,
        ladder: &[usize],
    ) -> (Self, mpsc::Receiver<Retired>, Arc<AtomicUsize>) {
        debug_assert_eq!(ladder[0], CORRECTION_BLOCK);
        let plan = NonUniformPlan::new(ladder);
        let incoming: Arc<ArcSwapOption<Prepared>> = Arc::new(ArcSwapOption::empty());
        let (request_tx, request_rx) = mpsc::sync_channel::<Request>(REQUEST_CAPACITY);
        let requests_in_flight = Arc::new(AtomicUsize::new(0));
        {
            let plan = plan.clone();
            let slot = Arc::clone(&incoming);
            let sink = Arc::clone(&sink);
            let in_flight = Arc::clone(&requests_in_flight);
            std::thread::Builder::new()
                .name("output-correction-worker".into())
                .spawn(move || {
                    crate::background_pool::enter_background();
                    Self::worker(request_rx, &in_flight, plan, sample_rate, sink, slot)
                })
                .expect("spawn output correction worker");
        }
        let (reclaim, retire_rx, pending) = Reclaim::new();
        let stage = Self {
            sample_rate,
            identity: plan.head().partition(&[1.0]),
            scratch: plan.head().make_scratch(),
            plan,
            key: None,
            bank: None,
            fade_from: None,
            incoming,
            request_tx,
            requests_in_flight,
            reclaim,
            streams: Streams::empty(),
            read_pos: 0,
            sink,
            synchronous_builds: false,
            resident: None,
            settled: Arc::new(AtomicU64::new(0)),
            settled_published: false,
            settle_seen: false,
        };
        (stage, retire_rx, pending)
    }

    /// Prepare filter sets on the calling thread, inside [`Self::ensure`],
    /// so each lands on the frame that asks for it. For offline renders,
    /// whose output must not depend on how fast the worker is; a live host
    /// leaves it off (a set reads files).
    pub fn set_synchronous_builds(&mut self, on: bool) {
        self.synchronous_builds = on;
    }

    /// Report on `flag` which set the outputs play through alone
    /// ([`settled_key`]): a measurement waits for it after switching the
    /// filters off, which reaches the longest tail segment only after up to
    /// two of its periods.
    pub fn set_settled_flag(&mut self, flag: Arc<AtomicU64>) {
        flag.store(self.settled.load(Ordering::Acquire), Ordering::Release);
        self.settled = flag;
    }

    /// The resident set is no longer the one `settled` names.
    fn unsettle(&mut self) {
        self.settled_published = false;
        self.settle_seen = false;
        self.settled.store(0, Ordering::Release);
    }

    /// Engine rate the stage was built for.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Whether the outputs go through the stage.
    pub fn is_active(&self) -> bool {
        self.bank.is_some() && !self.streams.inputs.is_empty()
    }

    /// Samples the stage delays the outputs by while active: the head block
    /// (`CORRECTION_BLOCK − 1`), plus the move of a resampled filter. 0 while
    /// inactive.
    pub fn latency_samples(&self) -> usize {
        match &self.bank {
            Some(bank) if self.is_active() => self.plan.head().latency_samples() + bank.delay,
            _ => 0,
        }
    }

    /// Hand `retired` to the reclaimer (see [`Reclaim::retire`]).
    fn retire(&mut self, retired: Retired) {
        self.reclaim.retire(retired);
    }

    /// The worker: reads and partitions filter sets. Only the latest request
    /// queued is prepared.
    fn worker(
        rx: mpsc::Receiver<Request>,
        in_flight: &AtomicUsize,
        plan: NonUniformPlan,
        sample_rate: u32,
        sink: OutputCorrectionStatusSink,
        slot: Arc<ArcSwapOption<Prepared>>,
    ) {
        while let Ok(mut latest) = rx.recv() {
            in_flight.fetch_sub(1, Ordering::AcqRel);
            while let Ok(next) = rx.try_recv() {
                in_flight.fetch_sub(1, Ordering::AcqRel);
                latest = next;
            }
            let Request {
                key,
                filters,
                current,
            } = latest;
            let prepared = Self::prepare(&plan, key, &filters, current, sample_rate, &sink);
            slot.store(Some(Arc::new(prepared)));
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
        let mut loaded: Vec<Option<LoadedFilter>> = Vec::with_capacity(key.outputs);
        let mut statuses = Vec::with_capacity(key.outputs);
        for output in 0..key.outputs {
            let Some(source) = filters.source(output) else {
                loaded.push(None);
                statuses.push(None);
                continue;
            };
            let mut status = FilterStatus {
                source: source.describe().into_owned(),
                ..FilterStatus::default()
            };
            match load_filter(source, sample_rate) {
                Ok(filter) => {
                    status.taps = filter.taps.len() - filter.delay.min(filter.taps.len());
                    status.source_rate = filter.source_rate;
                    status.source_taps = filter.source_taps;
                    status.hash = filter.hash;
                    loaded.push(Some(filter));
                }
                Err(e) => {
                    log::warn!(
                        "output correction: output {output} plays unfiltered: {e} ({})",
                        status.source
                    );
                    status.error = Some(e);
                    loaded.push(None);
                }
            }
            statuses.push(Some(status));
        }
        sink(OutputCorrectionStatus {
            generation: filters.generation,
            sample_rate,
            outputs: statuses,
        });
        // One delay for every output, filtered or not, and whether the
        // correction is on or off: switching it must not move the outputs.
        let delay = loaded.iter().flatten().map(|f| f.delay).max().unwrap_or(0);
        let taps: Vec<Option<Vec<f32>>> = loaded
            .into_iter()
            .map(|filter| match filter {
                Some(filter) if key.enabled => {
                    let mut taps = vec![0.0; delay - filter.delay];
                    taps.extend_from_slice(&filter.taps);
                    Some(taps)
                }
                _ if delay > 0 => {
                    let mut unit = vec![0.0; delay + 1];
                    unit[delay] = 1.0;
                    Some(unit)
                }
                _ => None,
            })
            .collect();
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
            delay,
            kernels: taps
                .iter()
                .map(|t| t.as_deref().map(|t| plan.partition(t, levels)))
                .collect(),
        });
        let streams = (current != Some(geometry)).then(|| Streams::new(plan, &bank));
        Prepared {
            key,
            bank,
            streams: parking_lot::Mutex::new(streams),
        }
    }

    /// Follow the live filters; called once per frame from the audio thread
    /// before [`Self::process`]. Steady state is one compare. A change asks
    /// the worker for the new set; the stage keeps the current one until it
    /// lands. Without any filter the stage stops at once. Nothing here
    /// allocates or frees: sets are built on the worker, what is replaced
    /// goes to the reclaimer, and while the reclaimer is behind the stage
    /// keeps the set it has rather than run out of room for what it would
    /// replace.
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
        if !self.reclaim.deferred.is_empty() {
            self.reclaim.flush();
        }
        // While the reclaimer is behind, the stage keeps the set it plays:
        // a new one is asked for and taken once its retirements fit.
        if !self.reclaim.has_room() {
            return;
        }
        // A request the queue could not take is asked again next frame.
        if self.key != Some(key) && self.request(key, filters) {
            self.key = Some(key);
        }
        if let Some(prepared) = self.incoming.swap(None) {
            if self.key == Some(prepared.key) {
                self.adopt(&prepared, filters);
            }
            self.retire(Retired::Prepared(prepared));
        }
    }

    /// Ask for `key`'s set: through the worker, or here with synchronous
    /// builds (published through the worker's slot, so it is adopted the same
    /// way, on this frame). `false` when the worker's queue is full: nothing
    /// was sent, nothing taken.
    fn request(&mut self, key: Key, filters: &OutputFilters) -> bool {
        let current = (!self.streams.inputs.is_empty()).then_some(self.streams.geometry);
        if self.synchronous_builds {
            let prepared = Self::prepare(
                &self.plan,
                key,
                filters,
                current,
                self.sample_rate,
                &self.sink,
            );
            self.incoming.store(Some(Arc::new(prepared)));
            return true;
        }
        // The only sender: below the capacity, the send cannot fail on a
        // full queue.
        if self.requests_in_flight.load(Ordering::Acquire) >= REQUEST_CAPACITY {
            return false;
        }
        self.requests_in_flight.fetch_add(1, Ordering::AcqRel);
        let request = Request {
            key,
            filters: filters.clone(),
            current,
        };
        match self.request_tx.try_send(request) {
            Ok(()) => true,
            Err(mpsc::TrySendError::Full(request)) => {
                self.requests_in_flight.fetch_sub(1, Ordering::AcqRel);
                self.retire(Retired::Filters(request.filters));
                false
            }
            // The worker is gone: nothing will ever come, do not ask again
            // on every frame.
            Err(mpsc::TrySendError::Disconnected(request)) => {
                self.retire(Retired::Filters(request.filters));
                true
            }
        }
    }

    /// Take a prepared set: new streams start from silence, a bank for the
    /// running streams ramps in over the next block.
    fn adopt(&mut self, prepared: &Prepared, filters: &OutputFilters) {
        let bank = Arc::clone(&prepared.bank);
        let landed = (prepared.streams.lock().is_some() || bank.geometry == self.streams.geometry)
            .then_some(prepared.key);
        if landed.is_some() {
            self.resident = landed;
            self.unsettle();
        }
        if let Some(streams) = prepared.streams.lock().take() {
            let old = std::mem::replace(&mut self.streams, streams);
            if !old.inputs.is_empty() {
                self.retire(Retired::Streams(old));
            }
            if let Some(old) = self.bank.replace(bank) {
                self.retire(Retired::Bank(old));
            }
            if let Some(old) = self.fade_from.take() {
                self.retire(Retired::Bank(old));
            }
            self.read_pos = 0;
            return;
        }
        if bank.geometry != self.streams.geometry {
            // Cut for streams that were replaced while it was prepared (a
            // set landing in between): ask again for the streams running now.
            self.retire(Retired::Bank(bank));
            if !self.request(prepared.key, filters) {
                self.key = None;
            }
            return;
        }
        match self.bank.replace(bank) {
            Some(old) => {
                if let Some(prev) = self.fade_from.replace(old) {
                    // Two swaps in one block: the middle bank never played.
                    self.retire(Retired::Bank(prev));
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
        self.resident = None;
        self.unsettle();
        if let Some(old) = self.bank.take() {
            self.retire(Retired::Bank(old));
        }
        if let Some(old) = self.fade_from.take() {
            self.retire(Retired::Bank(old));
        }
        let old = std::mem::replace(&mut self.streams, Streams::empty());
        if !old.inputs.is_empty() {
            self.retire(Retired::Streams(old));
        }
        // A set still on its way is for the filters just dropped.
        if let Some(prepared) = self.incoming.swap(None) {
            self.retire(Retired::Prepared(prepared));
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
            frame.copy_from_slice(&self.streams.fifo[at..at + outputs]);
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
            blocks,
            fifo,
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
        for (o, (input, out)) in inputs.iter().zip(blocks.iter_mut()).enumerate() {
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
                    self.reclaim.retire(Retired::Bank(old));
                }
                if !Arc::ptr_eq(&banks.current, bank) {
                    banks.outgoing = Some(std::mem::replace(&mut banks.current, Arc::clone(bank)));
                }
            }
            let current = &*banks.current;
            let outgoing = banks.outgoing.as_deref();
            for (o, (tail, out)) in tails.iter_mut().zip(blocks.iter_mut()).enumerate() {
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
            self.reclaim.retire(Retired::Bank(old));
        }
        // The block just computed still carries tail output the previous
        // period computed ahead: the outputs are the new set's alone from
        // the block after the one every segment first runs on it.
        if !self.settled_published
            && let Some(key) = self.resident
            && tail_banks
                .iter()
                .all(|b| b.outgoing.is_none() && Arc::ptr_eq(&b.current, bank))
        {
            if self.settle_seen {
                self.settled
                    .store(settled_key(key.generation, key.enabled), Ordering::Release);
                self.settled_published = true;
            }
            self.settle_seen = true;
        }
        let outputs = blocks.len();
        for (o, block) in blocks.iter().enumerate() {
            for (i, &v) in block.iter().enumerate() {
                fifo[i * outputs + o] = v;
            }
        }
        self.read_pos = 0;
    }
}

/// A canonical mono 32-bit float WAV file of `samples`: how a Save writes an
/// inline filter, which [`load_filter`] reads back sample for sample.
pub fn filter_wav(rate: u32, samples: &[f32]) -> Vec<u8> {
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

#[cfg(test)]
pub(crate) fn test_wav(rate: u32, samples: &[f32]) -> Vec<u8> {
    filter_wav(rate, samples)
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

    use super::test_wav as wav_f32;

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

    /// Taps handed over as samples (a measurement client's upload) filter
    /// exactly like the same taps read from a file, and report the same
    /// length, rate and content hash.
    #[test]
    fn inline_samples_filter_like_the_same_file() {
        let h = filter(3000, 21);
        let (_d, file) = filter_file("inline-twin", RATE, &h);
        let samples = Arc::new(FilterSamples::new(RATE, h.clone(), RATE).unwrap());
        let x = noise(12_000, 4);
        let from_file = run(
            &mut stage(&SMALL_LADDER),
            &OutputFilters::new(vec![Some(file.clone())]),
            true,
            &x,
            1,
            256,
        );
        let inline = OutputFilters::new(vec![Some(FilterSource::Samples(Arc::clone(&samples)))]);
        let from_samples = run(&mut stage(&SMALL_LADDER), &inline, true, &x, 1, 256);
        assert_eq!(from_file, from_samples);
        let a = load_filter(&file, RATE).unwrap();
        let b = load_filter(&FilterSource::Samples(Arc::clone(&samples)), RATE).unwrap();
        assert_eq!((a.source_taps, a.hash), (b.source_taps, b.hash));
        assert_eq!(b.hash, samples.hash);
        // Checked as a file is.
        assert!(
            FilterSamples::new(RATE, vec![0.0; 10], RATE).is_err(),
            "silent"
        );
        assert!(FilterSamples::new(RATE, vec![f32::NAN], RATE).is_err());
        assert!(FilterSamples::new(8_000, vec![1.0], RATE).is_err(), "rate");
        assert!(FilterSamples::new(RATE, vec![1.0; max_source_taps(RATE) + 1], RATE).is_err());
    }

    /// The settled flag names the set the outputs play through alone: not
    /// before the switch has reached the longest tail segment (up to two of
    /// its periods), and from then on the output is that set's alone.
    #[test]
    fn the_settled_flag_waits_for_the_longest_tail_after_a_switch() {
        for len in [600, 3_000, 40_000] {
            settles_after_a_switch(len);
        }
    }

    fn settles_after_a_switch(len: usize) {
        let h = filter(len, 13);
        let (_d, src) = filter_file(&format!("settle-{len}"), RATE, &h);
        let filters = OutputFilters::new(vec![Some(src)]);
        let flag = Arc::new(AtomicU64::new(0));
        let mut s = stage(&CORRECTION_LADDER);
        s.set_settled_flag(Arc::clone(&flag));
        let x = noise(120_000, 3);
        let mut out = x.clone();
        let chunk = 256;
        let switch = 60_000;
        let (mut on_at, mut off_at) = (None, None);
        for (i, block) in out.chunks_mut(chunk).enumerate() {
            let at = i * chunk;
            let enabled = at < switch;
            s.ensure(&filters, enabled, 1);
            s.process(block, 1);
            let value = flag.load(Ordering::Acquire);
            if on_at.is_none() && value == settled_key(filters.generation, true) {
                on_at = Some(at);
            }
            if enabled {
                continue;
            }
            assert_ne!(value, settled_key(filters.generation, true), "{at}");
            if off_at.is_none() && value == settled_key(filters.generation, false) {
                off_at = Some(at + chunk);
            }
        }
        assert!(on_at.is_some(), "the first set settles too");
        let off_at = off_at.expect("the switch settles");
        assert!(
            off_at > switch + CORRECTION_BLOCK
                && off_at <= switch + 2 * 8192 + 2 * CORRECTION_BLOCK,
            "{off_at}"
        );
        let lat = CORRECTION_BLOCK - 1;
        for n in off_at..x.len() {
            assert!((out[n] - x[n - lat]).abs() < 1e-5, "sample {n}");
        }
        // A change of set clears it at once; taking the outputs out too.
        let other = OutputFilters::new(vec![Some(FilterSource::Samples(Arc::new(
            FilterSamples::new(RATE, vec![0.5], RATE).unwrap(),
        )))]);
        s.ensure(&other, true, 1);
        assert_eq!(flag.load(Ordering::Acquire), 0);
        s.ensure(&OutputFilters::default(), true, 1);
        assert_eq!(flag.load(Ordering::Acquire), 0);
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

    /// The reclaimer stalled (starved, or held up in the allocator) while the
    /// user keeps switching the correction: the stage stops taking new sets
    /// before its retirements could run out of room, keeps playing the one it
    /// has, never frees anything itself, still leaves the speaker path
    /// cleanly, and catches up with the last switch once the reclaimer runs
    /// again.
    #[test]
    fn a_stalled_reclaimer_holds_the_resident_set_instead_of_freeing_here() {
        let (_d, src) = filter_file("stalled-reclaim", RATE, &[0.5, 0.25, 0.125]);
        let filters = OutputFilters::new(vec![Some(src)]);
        let (mut s, rx, pending) =
            OutputCorrectionStage::unreclaimed(RATE, Arc::new(|_| {}), &CORRECTION_LADDER);
        s.set_synchronous_builds(true);
        let mut buf = vec![0.5f32; 256];
        s.ensure(&filters, true, 1);
        s.process(&mut buf, 1);
        assert!(s.is_active());
        for i in 0..300 {
            let mut block = vec![0.5f32; 256];
            s.ensure(&filters, i % 2 == 1, 1);
            s.process(&mut block, 1);
            assert!(
                block.iter().any(|&v| v != 0.0),
                "the resident set keeps playing"
            );
        }
        assert!(
            pending.load(Ordering::Acquire) + RETIRE_RESERVE > RETIRE_CAPACITY + DEFERRED_CAPACITY,
            "the stage stopped at the reserve"
        );
        assert!(
            pending.load(Ordering::Acquire) <= RETIRE_CAPACITY + DEFERRED_CAPACITY,
            "retirements fit"
        );
        assert_eq!(s.reclaim.freed_here, 0, "nothing freed on the audio thread");
        // A switch now waits for the reclaimer.
        let held = s.key.map(|k| k.enabled).unwrap();
        let last = !held;
        for _ in 0..10 {
            s.ensure(&filters, last, 1);
            s.process(&mut buf, 1);
        }
        assert_eq!(s.key.map(|k| k.enabled), Some(held), "the switch waits");
        // The reclaimer runs again: everything retired reaches it, and the
        // stage takes the last switch.
        let mut freed = 0;
        for _ in 0..50 {
            while let Ok(retired) = rx.try_recv() {
                drop(retired);
                pending.fetch_sub(1, Ordering::AcqRel);
                freed += 1;
            }
            s.ensure(&filters, last, 1);
            s.process(&mut buf, 1);
        }
        assert!(freed > 0);
        assert_eq!(s.key.map(|k| k.enabled), Some(last));
        // Stalled again, leaving the speaker path still frees nothing here.
        for i in 0..300 {
            s.ensure(&filters, i % 2 == 0, 1);
            s.process(&mut buf, 1);
        }
        s.deactivate();
        assert!(!s.is_active());
        assert_eq!(s.reclaim.freed_here, 0);
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

    /// Response of `h` at `f` Hz when played at `fs`: (gain, phase with the
    /// delay `delay` samples removed).
    fn response(h: &[f32], fs: f64, f: f64, delay: usize) -> (f64, f64) {
        let (re, im) = h
            .iter()
            .enumerate()
            .fold((0.0f64, 0.0f64), |(re, im), (n, &v)| {
                let w = -2.0 * std::f64::consts::PI * f * (n as f64 - delay as f64) / fs;
                (re + v as f64 * w.cos(), im + v as f64 * w.sin())
            });
        ((re * re + im * im).sqrt(), im.atan2(re))
    }

    /// A filter moved to another rate keeps its response, magnitude and
    /// phase, once its common delay is taken out: the identity and a
    /// minimum-phase lowpass (both with their energy at time zero, where an
    /// interpolation that drops the kernel's first half loses most), over
    /// the rate pairs a filter file and an engine come in, up to 85 % of
    /// the lower Nyquist (18.7 kHz between 44.1 and 48 kHz), where the
    /// interpolation kernel's own roll-off starts.
    #[test]
    fn a_filter_at_another_rate_keeps_its_response() {
        let mut identity = vec![0.0f32; 256];
        identity[0] = 1.0;
        let a = 0.9f32;
        let lowpass: Vec<f32> = (0..2000).map(|n| (1.0 - a) * a.powi(n)).collect();
        let rates = [32_000u32, 44_100, 48_000, 88_200, 96_000, 192_000];
        for (name, taps) in [("identity", &identity), ("lowpass", &lowpass)] {
            for &from in &rates {
                for &to in &rates {
                    if from == to {
                        continue;
                    }
                    let loaded = filter_from_wav(&wav_f32(from, taps), to).unwrap();
                    assert_eq!(loaded.source_rate, from);
                    let nyquist = 0.5 * from.min(to) as f64;
                    for f in [0.0, 50.0, 1000.0, 5000.0, 10_000.0, 0.85 * nyquist] {
                        let (g_src, p_src) = response(taps, from as f64, f, 0);
                        let (g, p) = response(&loaded.taps, to as f64, f, loaded.delay);
                        let db = 20.0 * (g / g_src).log10();
                        assert!(
                            db.abs() < 0.05,
                            "{name} {from}->{to} at {f} Hz: {db:+.3} dB"
                        );
                        let dp = (p - p_src + std::f64::consts::PI)
                            .rem_euclid(2.0 * std::f64::consts::PI)
                            - std::f64::consts::PI;
                        assert!(
                            dp.abs() < 0.02,
                            "{name} {from}->{to} at {f} Hz: {dp:+.4} rad"
                        );
                    }
                    // The move is the kernel's half-width: 2 ms at most.
                    assert!(loaded.delay as f64 / (to as f64) <= 0.0021, "{from}->{to}");
                }
            }
        }
    }

    /// Every output, filtered or not, takes the move of a resampled filter,
    /// and keeps it with the correction off: the outputs stay aligned and the
    /// latency does not change under an A/B switch.
    #[test]
    fn a_resampled_filter_delays_every_output_alike() {
        let mut identity = vec![0.0f32; 64];
        identity[0] = 1.0;
        let (_d, src) = filter_file("resampled", 44_100, &identity);
        let filters = OutputFilters::new(vec![Some(src), None]);
        // Tones well inside the band both rates share.
        let x: Vec<f32> = (0..2 * 8000)
            .map(|i| {
                let t = (i / 2) as f32 / RATE as f32;
                let f = if i % 2 == 0 { 500.0 } else { 3000.0 };
                0.5 * (2.0 * std::f32::consts::PI * f * t).sin()
            })
            .collect();
        for enabled in [true, false] {
            let mut s = stage(&CORRECTION_LADDER);
            let out = run(&mut s, &filters, enabled, &x, 2, 512);
            let lat = s.latency_samples();
            assert!(lat > CORRECTION_BLOCK - 1, "the move is in the latency");
            for o in 0..2 {
                let (got, dry) = (channel(&out, 2, o), channel(&x, 2, o));
                let err: f64 = (lat + 64..got.len() - 64)
                    .map(|n| (got[n] as f64 - dry[n - lat] as f64).powi(2))
                    .sum();
                let sig: f64 = dry.iter().map(|&v| (v as f64).powi(2)).sum();
                assert!(
                    10.0 * (err / sig).log10() < -60.0,
                    "output {o}, correction {enabled}"
                );
            }
        }
    }

    /// A rate pair with no small common factor (a WAV at 44 101 Hz) moves
    /// the response by the kernel's half-width like any other, and a rate
    /// outside the supported range is refused before anything is sized by it.
    #[test]
    fn odd_rates_resample_with_a_bounded_delay_and_absurd_ones_are_refused() {
        let mut identity = vec![0.0f32; 256];
        identity[0] = 1.0;
        let odd = filter_from_wav(&wav_f32(44_101, &identity), RATE).unwrap();
        assert!(odd.delay <= 70, "delay {}", odd.delay);
        let (g, _) = response(&odd.taps, RATE as f64, 1000.0, odd.delay);
        assert!((20.0 * g.log10()).abs() < 0.05);
        for rate in [8_000, 1_000_003] {
            let err = filter_from_wav(&wav_f32(rate, &[1.0]), RATE).unwrap_err();
            assert!(err.contains("cannot be used"), "{rate}: {err}");
        }
        // At the engine's own rate any rate is the engine's: no resampling.
        assert_eq!(
            filter_from_wav(&wav_f32(RATE, &[1.0]), RATE).unwrap().delay,
            0
        );
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
