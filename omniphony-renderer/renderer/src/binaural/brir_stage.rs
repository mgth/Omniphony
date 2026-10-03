//! The BRIR render stage of the cascaded binaural path.
//!
//! The cascade ([`crate::spatial_renderer::cascade`]) mixes the programme
//! onto the app's speaker layout as a virtual room; with a room impulse
//! response set selected ([`super::HrirSource::Brir`]) this stage takes over
//! from the HRTF binaural stage and convolves each virtual speaker bus with
//! the pair measured from the nearest emitter of the set, at the head
//! orientation nearest the tracked one. Nothing else is applied: the
//! propagation delay, the interaural delay, the reflections and the tail are
//! the measurement. A non-spatialized bus (the LFE) is fed to both ears at
//! constant power, as on the HRTF path.
//!
//! # Streaming
//!
//! The responses are partitioned non-uniformly
//! ([`crate::partitioned_conv::nonuniform`]): their head on blocks of
//! [`BRIR_BLOCK`] samples, their tail on the larger blocks of
//! [`BRIR_LADDER`]. One [`InputHistory`] per bus carries the head. When the
//! buses complete a block, every bus is analysed once, and for each ear the
//! kernels of every bus are accumulated in the frequency domain before a
//! single inverse transform — one IFFT per ear per block however many
//! buses. The tail segments work the same way on their own blocks, each
//! spreading the work of one of its blocks over the head blocks of that
//! period, and add their share of the output to every head block. The
//! host's frames are decoupled from the block: samples are pushed one at a
//! time and the output is read from a block-sized FIFO, so the stage adds
//! exactly `BRIR_BLOCK − 1` samples of latency, reported through
//! [`BrirStage::latency_samples`].
//!
//! # Kernels and the worker
//!
//! The set stays in the time domain; only the *active orientation* is
//! partitioned, as a [`KernelBank`] (one pair per emitter). Banks and sets
//! are built on a worker thread and handed over through `ArcSwapOption`
//! slots: the audio thread compares, sends a request, and keeps convolving
//! the current bank until the new one lands. A bank swap is blended over
//! one block ([`crate::partitioned_conv::ConvolutionPlan::finish_blend`])
//! by the head, on the block it lands on. A tail segment's blocks are
//! computed a period ahead, so each segment takes the new bank at the first
//! of its periods to start from there on, and ramps to it over the first
//! [`BRIR_BLOCK`] samples of that period's output ([`TailStreams::run`]): a
//! part of the response follows the head no later than it lies into the
//! response. Retired banks and sets go back to the worker to be freed. The set loaded for a head-tracked listener holds
//! every orientation; without tracking only the one nearest straight ahead
//! is resident (see [`crate::live_params::BrirLiveParams`]).

use std::sync::Arc;
use std::sync::mpsc;

use arc_swap::ArcSwapOption;

use super::DIRECT_EAR_GAIN;
use super::brir::{BrirLoadOptions, BrirSet};
use super::head_pose::HeadPose;
use crate::partitioned_conv::nonuniform::{NonUniformKernel, NonUniformPlan, TailStreams};
use crate::partitioned_conv::{InputHistory, OutputScratch};

/// Partition size of the head of the BRIR convolution, in samples. Sets the
/// stage's own latency (`BRIR_BLOCK − 1`) and the grain the work is done
/// in: the stage computes once per `BRIR_BLOCK` samples.
pub const BRIR_BLOCK: usize = 128;

/// Block sizes the responses are partitioned on: the head, then the tail
/// segments. A segment of block `B` starts `2·B − BRIR_BLOCK` taps into the
/// response and is used once the responses reach far enough past that
/// (see [`NonUniformPlan::levels_for`]).
///
/// A ratio of four between sizes is where the two costs balance. Per head
/// block a level's transforms cost about as much as three or four
/// partitions, and this ratio leaves seven partitions on the head and six
/// on each inner segment; a ratio of two takes two levels where this takes
/// one to save two partitions, a ratio of eight saves one level in three
/// for ten partitions more. The last size bounds the largest single
/// transform, the one piece of a period's work that cannot be split across
/// head blocks; past it a longer response only adds partitions of that
/// size.
pub const BRIR_LADDER: [usize; 4] = [BRIR_BLOCK, 512, 2048, 8192];

/// Angle between a virtual speaker and the emitter it is rendered from
/// above which the mapping is logged as a mismatch, degrees.
const MISMATCH_WARN_DEG: f32 = 10.0;

/// What the last BRIR load produced, for the control surface.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BrirStatus {
    /// The file asked for (empty when no BRIR source is selected).
    pub path: String,
    /// The resident set, when the load succeeded.
    pub loaded: Option<BrirSummary>,
    /// Why the file is not in use (the cascade then runs on the HRTF stage).
    pub error: Option<String>,
}

/// Shape of a resident BRIR set.
#[derive(Debug, Clone, PartialEq)]
pub struct BrirSummary {
    pub conventions: String,
    pub emitters: usize,
    pub orientations: usize,
    pub max_taps: usize,
    pub sample_rate: u32,
    pub bytes: usize,
}

impl BrirSummary {
    fn of(set: &BrirSet) -> Self {
        Self {
            conventions: set.conventions().to_string(),
            emitters: set.emitters().len(),
            orientations: set.orientations().len(),
            max_taps: set.max_taps(),
            sample_rate: set.sample_rate(),
            bytes: set.bytes(),
        }
    }
}

/// Where the stage reports each load's [`BrirStatus`].
pub type BrirStatusSink = Arc<dyn Fn(BrirStatus) + Send + Sync>;

/// What a load is identified by: the file and the load options. A change
/// reloads.
#[derive(Debug, Clone, PartialEq)]
struct LoadKey {
    path: String,
    opts: BrirLoadOptions,
}

/// The partitioned pairs of every emitter at one head orientation.
pub(crate) struct KernelBank {
    /// Identity of the set the bank was built from (`Arc` pointer).
    set_id: usize,
    orientation: usize,
    /// `[emitter] → [left, right]`, every kernel cut into the levels the
    /// set's longest response calls for.
    kernels: Vec<[NonUniformKernel; 2]>,
}

fn set_id(set: &Arc<BrirSet>) -> usize {
    Arc::as_ptr(set) as usize
}

fn build_bank(plan: &NonUniformPlan, set: &Arc<BrirSet>, orientation: usize) -> KernelBank {
    let levels = plan.levels_for(set.max_taps());
    let kernels = (0..set.emitters().len())
        .map(|e| {
            let pair = set.pair(e, orientation);
            [
                plan.partition(&pair.left, levels),
                plan.partition(&pair.right, levels),
            ]
        })
        .collect();
    KernelBank {
        set_id: set_id(set),
        orientation,
        kernels,
    }
}

/// The banks a tail segment convolves: its blocks are computed a period
/// ahead, so it follows the stage's bank at its own period starts.
struct TailBanks {
    /// The bank the period in flight is computed with.
    current: Arc<KernelBank>,
    /// The bank that period ramps from, on the period a swap reaches the
    /// segment.
    outgoing: Option<Arc<KernelBank>>,
}

/// The streaming state of every bus, sized for one set's longest kernel.
struct Streams {
    /// Longest kernel the state is sized for, in samples.
    taps: usize,
    /// One head history per bus.
    inputs: Vec<InputHistory>,
    tails: TailStreams,
    /// One entry per tail segment.
    tail_banks: Vec<TailBanks>,
}

impl Streams {
    /// What an unready stage holds.
    fn empty() -> Self {
        Self {
            taps: 0,
            inputs: Vec::new(),
            tails: TailStreams::empty(),
            tail_banks: Vec::new(),
        }
    }

    /// Silent state for `buses` buses convolved with `bank`, a bank of a
    /// set whose longest kernel is `taps` samples (allocating).
    fn new(plan: &NonUniformPlan, taps: usize, bank: &Arc<KernelBank>, buses: usize) -> Self {
        let levels = plan.levels_for(taps);
        let capacity = plan.partitions_for(0, levels, taps);
        Self {
            taps,
            inputs: (0..buses)
                .map(|_| plan.head().make_input(capacity))
                .collect(),
            tails: plan.make_tails(levels, taps, buses, 2),
            tail_banks: (1..levels)
                .map(|_| TailBanks {
                    current: Arc::clone(bank),
                    outgoing: None,
                })
                .collect(),
        }
    }
}

/// A loaded set with everything the audio thread needs to start on it.
struct Loaded {
    key: LoadKey,
    set: Arc<BrirSet>,
    bank: Arc<KernelBank>,
    streams: Streams,
}

enum Request {
    Load {
        key: LoadKey,
        buses: usize,
    },
    Bank {
        set: Arc<BrirSet>,
        orientation: usize,
    },
    /// Something the audio thread retired (a set, a bank, streams):
    /// carried here only to be dropped on the worker, not there.
    Drop(Retired),
}

type Retired = Box<dyn std::any::Any + Send>;

/// See the module doc.
pub struct BrirStage {
    sample_rate: u32,
    plan: NonUniformPlan,
    /// The load last asked for.
    key: Option<LoadKey>,
    set: Option<Arc<BrirSet>>,
    bank: Option<Arc<KernelBank>>,
    /// The bank on its way out during the block a swap lands in.
    fade_from: Option<Arc<KernelBank>>,
    /// Orientation requested from the worker and not delivered yet.
    pending_orientation: Option<usize>,
    incoming_set: Arc<ArcSwapOption<Loaded>>,
    incoming_bank: Arc<ArcSwapOption<KernelBank>>,
    request_tx: mpsc::Sender<Request>,
    /// Per bus: the emitter it is rendered from, `None` for a direct bus.
    bus_emitter: Vec<Option<usize>>,
    /// Identity of the bus geometry `bus_emitter` was built for.
    geometry_id: usize,
    /// Identity of the set `bus_emitter` was built against.
    mapped_set_id: usize,
    streams: Streams,
    scratch: [OutputScratch; 2],
    ear_block: [Vec<f32>; 2],
    /// Interleaved stereo output of the last completed block.
    fifo: Vec<f32>,
    read_pos: usize,
    /// Bumped on every set swap.
    set_generation: u64,
    /// Where load statuses go; the synchronous path reports through it too.
    sink: BrirStatusSink,
    /// Load sets and build banks on the calling thread instead of the
    /// worker — see [`BrirStage::set_synchronous_builds`].
    synchronous_builds: bool,
}

impl BrirStage {
    /// A stage whose load status goes nowhere.
    pub fn new(sample_rate: u32) -> Self {
        Self::with_status_sink(sample_rate, Arc::new(|_| {}))
    }

    /// A stage reporting every load's [`BrirStatus`] to `sink`.
    pub fn with_status_sink(sample_rate: u32, sink: BrirStatusSink) -> Self {
        Self::with_ladder(sample_rate, sink, &BRIR_LADDER)
    }

    /// A stage partitioning its responses on `ladder`, whose first size is
    /// [`BRIR_BLOCK`]. The output does not depend on the ladder beyond
    /// rounding (and the moment a tail segment follows a bank swap); the
    /// cost does.
    fn with_ladder(sample_rate: u32, sink: BrirStatusSink, ladder: &[usize]) -> Self {
        debug_assert_eq!(ladder[0], BRIR_BLOCK);
        let plan = NonUniformPlan::new(ladder);
        let incoming_set: Arc<ArcSwapOption<Loaded>> = Arc::new(ArcSwapOption::empty());
        let incoming_bank: Arc<ArcSwapOption<KernelBank>> = Arc::new(ArcSwapOption::empty());
        let (request_tx, request_rx) = mpsc::channel::<Request>();
        {
            let plan = plan.clone();
            let set_slot = Arc::clone(&incoming_set);
            let bank_slot = Arc::clone(&incoming_bank);
            let sink = Arc::clone(&sink);
            std::thread::Builder::new()
                .name("binaural-brir-worker".into())
                .spawn(move || {
                    Self::worker(request_rx, plan, sample_rate, sink, set_slot, bank_slot)
                })
                .expect("spawn BRIR worker");
        }
        Self {
            sample_rate,
            scratch: [plan.head().make_scratch(), plan.head().make_scratch()],
            plan,
            key: None,
            set: None,
            bank: None,
            fade_from: None,
            pending_orientation: None,
            incoming_set,
            incoming_bank,
            request_tx,
            bus_emitter: Vec::new(),
            geometry_id: usize::MAX,
            mapped_set_id: 0,
            streams: Streams::empty(),
            ear_block: [vec![0.0; BRIR_BLOCK], vec![0.0; BRIR_BLOCK]],
            fifo: vec![0.0; 2 * BRIR_BLOCK],
            read_pos: 0,
            set_generation: 0,
            sink,
            synchronous_builds: false,
        }
    }

    /// Load sets and build orientation banks on the calling thread, inside
    /// [`Self::ensure_loaded`] and the block that asks for a new
    /// orientation, so each lands on the very frame that requests it.
    ///
    /// For offline renders: the asynchronous swap lands at whichever block
    /// the worker happens to finish by, so two renders of the same input
    /// would differ. A live host must leave this off — a load reads a SOFA
    /// file and partitions every kernel, which the audio thread must never
    /// wait for. The steady-state per-frame cost is the same either way. Set
    /// it before the first frame: a load already handed to the worker still
    /// lands late.
    pub fn set_synchronous_builds(&mut self, on: bool) {
        self.synchronous_builds = on;
    }

    /// The worker: loads sets, partitions banks, frees what the audio
    /// thread retires. Requests are coalesced: a load supersedes everything
    /// queued before it, and only the latest bank request is built.
    fn worker(
        rx: mpsc::Receiver<Request>,
        plan: NonUniformPlan,
        sample_rate: u32,
        sink: BrirStatusSink,
        set_slot: Arc<ArcSwapOption<Loaded>>,
        bank_slot: Arc<ArcSwapOption<KernelBank>>,
    ) {
        while let Ok(first) = rx.recv() {
            let mut load: Option<(LoadKey, usize)> = None;
            let mut bank: Option<(Arc<BrirSet>, usize)> = None;
            let mut handle = |req: Request| match req {
                Request::Load { key, buses } => {
                    load = Some((key, buses));
                    bank = None;
                }
                Request::Bank { set, orientation } => bank = Some((set, orientation)),
                Request::Drop(retired) => drop(retired),
            };
            handle(first);
            while let Ok(next) = rx.try_recv() {
                handle(next);
            }
            if let Some((key, buses)) = load {
                if let Some(loaded) = Self::load_ready(&plan, key, buses, sample_rate, &sink) {
                    set_slot.store(Some(Arc::new(loaded)));
                }
            } else if let Some((set, orientation)) = bank {
                bank_slot.store(Some(Arc::new(build_bank(&plan, &set, orientation))));
            }
        }
    }

    /// Load `key` with everything the audio thread needs to start on it (the
    /// front bank, the streams of `buses` buses), reporting the outcome to
    /// `sink`. `None` when the file cannot be used.
    fn load_ready(
        plan: &NonUniformPlan,
        key: LoadKey,
        buses: usize,
        sample_rate: u32,
        sink: &BrirStatusSink,
    ) -> Option<Loaded> {
        match Self::load(&key, sample_rate) {
            Ok(set) => {
                let set = Arc::new(set);
                let (yaw, pitch) = (0.0, 0.0);
                let front = set.nearest_orientation(yaw, pitch);
                let bank = Arc::new(build_bank(plan, &set, front));
                let streams = Streams::new(plan, set.max_taps(), &bank, buses);
                sink(BrirStatus {
                    path: key.path.clone(),
                    loaded: Some(BrirSummary::of(&set)),
                    error: None,
                });
                Some(Loaded {
                    key,
                    set,
                    bank,
                    streams,
                })
            }
            Err(e) => {
                log::warn!(
                    "binaural: BRIR '{}' unavailable ({e}); the cascade runs on the HRTF stage",
                    key.path
                );
                sink(BrirStatus {
                    path: key.path.clone(),
                    loaded: None,
                    error: Some(e),
                });
                None
            }
        }
    }

    #[cfg(feature = "sofa")]
    fn load(key: &LoadKey, sample_rate: u32) -> Result<BrirSet, String> {
        if key.path.trim().is_empty() {
            return Err("no BRIR file selected".to_string());
        }
        BrirSet::from_sofa(&key.path, sample_rate, &key.opts).map_err(|e| e.to_string())
    }

    #[cfg(not(feature = "sofa"))]
    fn load(_key: &LoadKey, _sample_rate: u32) -> Result<BrirSet, String> {
        Err("SOFA support not built into this renderer (enable the 'sofa' feature)".to_string())
    }

    /// Engine rate the stage was built for.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Samples the stage delays the buses by: `BRIR_BLOCK − 1`.
    pub fn latency_samples(&self) -> usize {
        self.plan.head().latency_samples()
    }

    /// Whether a set and its bank are resident: the stage can render.
    pub fn is_ready(&self) -> bool {
        self.set.is_some() && self.bank.is_some()
    }

    /// Bumped on every set swap (observable by tests and diagnostics).
    pub fn set_generation(&self) -> u64 {
        self.set_generation
    }

    /// The resident set, if any.
    pub fn set(&self) -> Option<&Arc<BrirSet>> {
        self.set.as_ref()
    }

    /// Orientation index of the bank being convolved.
    pub fn bank_orientation(&self) -> Option<usize> {
        self.bank.as_ref().map(|b| b.orientation)
    }

    fn retire(&self, r: Retired) {
        // A closed channel means the worker is gone (the stage is being torn
        // down); dropping here is then the only option.
        let _ = self.request_tx.send(Request::Drop(r));
    }

    /// Track the requested file and options; called once per frame from the
    /// audio thread. Steady state is one compare. A change sends a load
    /// request to the worker; the stage keeps its current set until the new
    /// one lands, then swaps (and the streams of `buses` buses come pre-built
    /// with it).
    /// With [`Self::set_synchronous_builds`] on, the load runs here instead
    /// and the new set is live on this frame.
    pub fn ensure_loaded(&mut self, path: &str, opts: &BrirLoadOptions, buses: usize) {
        let changed = match &self.key {
            Some(k) => k.path != path || k.opts != *opts,
            None => true,
        };
        if changed {
            let key = LoadKey {
                path: path.to_string(),
                opts: *opts,
            };
            self.key = Some(key.clone());
            if self.synchronous_builds {
                // Published through the slot the worker uses, so the swap
                // below takes it exactly as it takes the worker's.
                if let Some(loaded) =
                    Self::load_ready(&self.plan, key, buses, self.sample_rate, &self.sink)
                {
                    self.incoming_set.store(Some(Arc::new(loaded)));
                }
            } else {
                let _ = self.request_tx.send(Request::Load { key, buses });
            }
        }
        if let Some(loaded) = self.incoming_set.swap(None) {
            let stale = self.key.as_ref() != Some(&loaded.key);
            // `Arc<Loaded>` is uniquely ours now: take its parts.
            match Arc::try_unwrap(loaded) {
                Ok(loaded) if !stale => self.adopt(loaded),
                Ok(loaded) => {
                    self.retire(Box::new(loaded.set));
                    self.retire(Box::new(loaded.bank));
                    self.retire(Box::new(loaded.streams));
                }
                Err(_) => unreachable!("the incoming slot held the only reference"),
            }
        }
    }

    /// Swap a freshly loaded set in, retiring the previous one.
    fn adopt(&mut self, loaded: Loaded) {
        if let Some(old) = self.set.replace(loaded.set) {
            self.retire(Box::new(old));
        }
        if let Some(old) = self.bank.replace(loaded.bank) {
            self.retire(Box::new(old));
        }
        if let Some(old) = self.fade_from.take() {
            self.retire(Box::new(old));
        }
        let old_streams = std::mem::replace(&mut self.streams, loaded.streams);
        if !old_streams.inputs.is_empty() {
            self.retire(Box::new(old_streams));
        }
        self.pending_orientation = None;
        self.fifo.fill(0.0);
        self.read_pos = 0;
        self.set_generation += 1;
    }

    /// Install a set directly (tests): the front bank is built on the
    /// calling thread and the stage is ready on return.
    #[cfg(test)]
    pub(crate) fn install_set(&mut self, set: Arc<BrirSet>, buses: usize) {
        let front = set.nearest_orientation(0.0, 0.0);
        let bank = Arc::new(build_bank(&self.plan, &set, front));
        let streams = Streams::new(&self.plan, set.max_taps(), &bank, buses);
        self.adopt(Loaded {
            key: LoadKey {
                path: String::new(),
                opts: BrirLoadOptions::default(),
            },
            set,
            bank,
            streams,
        });
    }

    /// Map the virtual speakers onto the set's emitters and size the
    /// streams. `geometry_id` identifies the bus geometry (the cascade's
    /// topology identity); steady state is two compares. A bus is rendered
    /// from the emitter nearest to it in direction; mismatches beyond
    /// [`MISMATCH_WARN_DEG`] and emitters shared by several buses are
    /// logged once per mapping.
    pub fn configure_buses(&mut self, positions: &[[f64; 3]], direct: &[bool], geometry_id: usize) {
        let Some(set) = self.set.as_ref() else {
            return;
        };
        let sid = set_id(set);
        let total = positions.len();
        if self.geometry_id != geometry_id
            || self.mapped_set_id != sid
            || self.bus_emitter.len() != total
        {
            self.bus_emitter.clear();
            let emitters = set.emitters();
            let unit = |p: [f32; 3]| {
                let n = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
                if n > 1e-9 {
                    [p[0] / n, p[1] / n, p[2] / n]
                } else {
                    [0.0, 1.0, 0.0]
                }
            };
            let emitter_dirs: Vec<[f32; 3]> = emitters.iter().map(|&e| unit(e)).collect();
            let mut uses = vec![0usize; emitters.len()];
            for (b, p) in positions.iter().enumerate() {
                if direct.get(b).copied().unwrap_or(false) || emitters.is_empty() {
                    self.bus_emitter.push(None);
                    continue;
                }
                let d = unit([p[0] as f32, p[1] as f32, p[2] as f32]);
                let (best, dot) = emitter_dirs
                    .iter()
                    .enumerate()
                    .map(|(i, e)| (i, d[0] * e[0] + d[1] * e[1] + d[2] * e[2]))
                    .fold(
                        (0, f32::NEG_INFINITY),
                        |acc, x| if x.1 > acc.1 { x } else { acc },
                    );
                let angle = dot.clamp(-1.0, 1.0).acos().to_degrees();
                if angle > MISMATCH_WARN_DEG {
                    log::warn!(
                        "BRIR: virtual speaker {b} is {angle:.0}° from its nearest emitter {best}; the set has no loudspeaker there"
                    );
                }
                uses[best] += 1;
                self.bus_emitter.push(Some(best));
            }
            for (e, &n) in uses.iter().enumerate() {
                if n > 1 {
                    log::warn!("BRIR: emitter {e} serves {n} virtual speakers");
                }
            }
            log::info!(
                "BRIR: {} virtual speakers → {} emitters used of {} ({} direct)",
                total,
                uses.iter().filter(|&&n| n > 0).count(),
                emitters.len(),
                self.bus_emitter.iter().filter(|e| e.is_none()).count()
            );
            self.geometry_id = geometry_id;
            self.mapped_set_id = sid;
        }
        // Streams: pre-built by the load for the bus count then; a relayout
        // since is the rare case that allocates here.
        if self.streams.inputs.len() != total || self.streams.taps != set.max_taps() {
            let Some(bank) = self.bank.as_ref() else {
                return;
            };
            let streams = Streams::new(&self.plan, set.max_taps(), bank, total);
            let old = std::mem::replace(&mut self.streams, streams);
            if !old.inputs.is_empty() {
                self.retire(Box::new(old));
            }
            self.fifo.fill(0.0);
            self.read_pos = 0;
        }
    }

    /// Per bus: the emitter it is rendered from (`None` = direct).
    pub fn bus_emitters(&self) -> &[Option<usize>] {
        &self.bus_emitter
    }

    /// Convolve one frame of interleaved buses (`total` per sample) into
    /// `out` (interleaved stereo, `2 · sample_length`, added to). The stage
    /// must be ready and configured for `total` buses.
    pub fn render_frame(
        &mut self,
        bus: &[f32],
        total: usize,
        sample_length: usize,
        head_pose: HeadPose,
        out: &mut [f32],
    ) {
        debug_assert_eq!(out.len(), 2 * sample_length);
        debug_assert_eq!(bus.len(), total * sample_length);
        if !self.is_ready() || self.streams.inputs.len() != total || total == 0 {
            return;
        }
        for i in 0..sample_length {
            let frame = &bus[i * total..(i + 1) * total];
            let mut complete = false;
            for (input, &x) in self.streams.inputs.iter_mut().zip(frame) {
                complete = input.push(x);
            }
            if complete {
                self.process_block(head_pose);
            }
            out[2 * i] += self.fifo[2 * self.read_pos];
            out[2 * i + 1] += self.fifo[2 * self.read_pos + 1];
            self.read_pos += 1;
        }
    }

    /// One completed block: analyse every bus, follow the head, sum the
    /// head kernels per ear, blend a landing bank swap, add the direct
    /// buses, then let every tail segment do this block's share of its work
    /// and add its output.
    fn process_block(&mut self, head_pose: HeadPose) {
        for input in &mut self.streams.inputs {
            self.plan.head().analyze(input);
        }
        self.streams
            .tails
            .feed(self.streams.inputs.iter().map(InputHistory::last_block));
        let set = self.set.as_ref().expect("ready");
        let sid = set_id(set);

        // Follow the head: ask for the nearest measured orientation when it
        // differs from the bank's and is not already on its way.
        if set.orientations().len() > 1 {
            let (yaw, pitch) = head_pose.yaw_pitch_deg();
            let wanted = set.nearest_orientation(yaw, pitch);
            let current = self.bank.as_ref().map(|b| b.orientation);
            if current != Some(wanted) && self.pending_orientation != Some(wanted) {
                if self.synchronous_builds {
                    // Through the worker's slot: the swap below blends it in
                    // on this very block.
                    self.incoming_bank
                        .store(Some(Arc::new(build_bank(&self.plan, set, wanted))));
                } else {
                    let _ = self.request_tx.send(Request::Bank {
                        set: Arc::clone(set),
                        orientation: wanted,
                    });
                }
                self.pending_orientation = Some(wanted);
            }
        }
        if let Some(bank) = self.incoming_bank.swap(None) {
            if bank.set_id == sid {
                if self.pending_orientation == Some(bank.orientation) {
                    self.pending_orientation = None;
                }
                let old = self.bank.replace(bank);
                if let Some(prev) = self.fade_from.replace(old.expect("ready")) {
                    // Two swaps in one block: the middle bank never played.
                    self.retire(Box::new(prev));
                }
            } else {
                self.retire(Box::new(bank));
            }
        }

        let bank = self.bank.as_ref().expect("ready");
        let plan = self.plan.head();
        let Streams {
            inputs,
            tails,
            tail_banks,
            ..
        } = &mut self.streams;
        let inputs = &*inputs;
        let bus_emitter = &self.bus_emitter;
        for ear in 0..2 {
            let scratch = &mut self.scratch[ear];
            let out = &mut self.ear_block[ear];
            // The outgoing bank first when a swap lands, then the new one
            // ramps in over the block.
            let first = self.fade_from.as_deref().unwrap_or(bank);
            scratch.clear();
            for (input, e) in inputs.iter().zip(bus_emitter) {
                if let Some(e) = e {
                    plan.accumulate(input, first.kernels[*e][ear].head(), scratch);
                }
            }
            plan.finish(scratch, out);
            if self.fade_from.is_some() {
                scratch.clear();
                for (input, e) in inputs.iter().zip(bus_emitter) {
                    if let Some(e) = e {
                        plan.accumulate(input, bank.kernels[*e][ear].head(), scratch);
                    }
                }
                plan.finish_blend(scratch, out);
            }
            // Direct buses: the block just analysed is the one this output
            // block corresponds to, so no extra delay is needed.
            for (input, e) in inputs.iter().zip(bus_emitter) {
                if e.is_none() {
                    for (o, &x) in out.iter_mut().zip(input.last_block()) {
                        *o += DIRECT_EAR_GAIN * x;
                    }
                }
            }
        }
        for (segment, banks) in tail_banks.iter_mut().enumerate() {
            if tails.period_starts(segment) {
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
            tails.run(
                segment,
                outgoing.is_some(),
                |pass, bus, ear| {
                    let bank = match outgoing {
                        Some(outgoing) if pass == 0 => outgoing,
                        _ => current,
                    };
                    let emitter = bus_emitter.get(bus).copied().flatten();
                    emitter.map(|e| bank.kernels[e][ear].tail(segment))
                },
                &mut self.ear_block,
            );
        }
        if let Some(old) = self.fade_from.take() {
            self.retire(Box::new(old));
        }
        for i in 0..BRIR_BLOCK {
            self.fifo[2 * i] = self.ear_block[0][i];
            self.fifo[2 * i + 1] = self.ear_block[1][i];
        }
        self.read_pos = 0;
    }
}

/// Synthetic sets for the stage's tests and the renderer's.
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Arc;

    use crate::binaural::brir::{BrirLoadOptions, BrirSet, RawRoomIr};

    /// A synthetic `MultiSpeakerBRIR`-shaped set: emitters at SOFA azimuths
    /// `spk_az` (2 m, SOFA sign: left positive), views at SOFA azimuths
    /// `yaws`. Each pair is an impulse at `40 + 5·e` samples of amplitude
    /// `0.5 + 0.1·e + 0.01·o`, negated on the right ear and given a crude
    /// interaural level difference (the ear on the emitter's side is 1.5×,
    /// the other 0.5×), plus a decaying tail so the kernels span several
    /// partitions. Tails are kept whole (no floor, no length bound).
    pub(crate) fn synth_set(spk_az: &[f32], yaws: &[f32], n: usize) -> Arc<BrirSet> {
        synth_set_decay(spk_az, yaws, n, 60.0)
    }

    /// [`synth_set`] with the tail's decay constant in samples (`60` there;
    /// a long one keeps a long response from vanishing under the floor).
    pub(crate) fn synth_set_decay(
        spk_az: &[f32],
        yaws: &[f32],
        n: usize,
        decay_samples: f32,
    ) -> Arc<BrirSet> {
        let sph = |az_deg: f32, r: f32| {
            let az = az_deg.to_radians();
            [r * az.cos(), r * az.sin(), 0.0]
        };
        let (m, r, e) = (yaws.len(), 2, spk_az.len());
        let mut ir = vec![0.0f32; m * r * e * n];
        for mi in 0..m {
            for ri in 0..r {
                for (k, &az) in spk_az.iter().enumerate() {
                    let base = ((mi * r + ri) * e + k) * n;
                    // SOFA azimuth is left-positive: `side` is +1 on the right.
                    let side = -az.to_radians().sin();
                    let ild = if ri == 0 {
                        1.0 - 0.5 * side
                    } else {
                        1.0 + 0.5 * side
                    };
                    let amp = (0.5 + 0.1 * k as f32 + 0.01 * mi as f32)
                        * ild
                        * if ri == 0 { 1.0 } else { -1.0 };
                    let d = 40 + 5 * k;
                    ir[base + d] = amp;
                    for t in 1..(n - d) {
                        let sign = if t % 2 == 0 { 1.0 } else { -0.7 };
                        ir[base + d + t] = amp * 0.3 * (-(t as f32) / decay_samples).exp() * sign;
                    }
                }
            }
        }
        let emitter: Vec<f32> = spk_az.iter().flat_map(|&a| sph(a, 2.0)).collect();
        let view: Vec<f32> = yaws.iter().flat_map(|&y| sph(y, 1.0)).collect();
        let raw = RawRoomIr {
            conventions: "MultiSpeakerBRIR",
            sample_rate: 48000.0,
            m,
            r,
            e,
            n,
            source_position: &[0.0, 0.0, 0.0],
            emitter_position: &emitter,
            listener_position: &[0.0, 0.0, 0.0],
            listener_view: &view,
            data_ir: &ir,
            data_delay: &[],
        };
        let opts = BrirLoadOptions {
            max_length_s: 0.0,
            tail_floor_db: 120.0,
            ..Default::default()
        };
        Arc::new(BrirSet::from_raw(&raw, 48000, &opts).unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{synth_set, synth_set_decay};
    use super::*;
    use crate::binaural::brir::BrirPair;

    /// The reference the ladder is held to: every tap on [`BRIR_BLOCK`]
    /// partitions.
    const UNIFORM: [usize; 1] = [BRIR_BLOCK];

    /// A short ladder with the shape of [`BRIR_LADDER`], for the tests that
    /// run the reference many times.
    const SMALL_LADDER: [usize; 3] = [BRIR_BLOCK, 256, 1024];

    /// Deterministic full-band noise in [−0.5, 0.5) (LCG; no rand dep).
    fn noise(len: usize, seed: u32) -> Vec<f32> {
        let mut lcg = seed;
        (0..len)
            .map(|_| {
                lcg = lcg.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (lcg >> 8) as f32 / (1 << 24) as f32 - 0.5
            })
            .collect()
    }

    /// `total` virtual speakers spread around the listener, those listed in
    /// `direct` not spatialized.
    fn ring_buses(total: usize, direct: &[usize]) -> (Vec<[f64; 3]>, Vec<bool>) {
        let positions = (0..total)
            .map(|i| {
                let a = (i as f64 + 0.5) * std::f64::consts::TAU / total as f64;
                [a.sin(), a.cos(), 0.0]
            })
            .collect();
        (positions, (0..total).map(|i| direct.contains(&i)).collect())
    }

    /// A set with one emitter at each of `positions` and one orientation
    /// per yaw (sorted), emitter `e`'s pairs being noise of `taps[e]`
    /// samples at unit energy: as loud in the last partition as in the
    /// first, so a misplaced tail segment cannot hide under the head.
    fn noise_set(positions: &[[f64; 3]], taps: &[usize], yaws: &[f32], seed: u32) -> Arc<BrirSet> {
        let emitters = positions
            .iter()
            .map(|p| [p[0] as f32, p[1] as f32, p[2] as f32])
            .collect();
        let mut pairs = Vec::new();
        for (e, &n) in taps.iter().enumerate() {
            for o in 0..yaws.len() {
                let k = 2 * (e * yaws.len() + o) as u32;
                let ear = |k: u32| -> Vec<f32> {
                    let gain = 2.0 / (n as f32).sqrt();
                    noise(n, seed.wrapping_add(k))
                        .into_iter()
                        .map(|v| v * gain)
                        .collect()
                };
                pairs.push(BrirPair {
                    left: ear(k),
                    right: ear(k + 1),
                });
            }
        }
        let orientations = yaws.iter().map(|&y| (y, 0.0)).collect();
        Arc::new(BrirSet::from_pairs(emitters, orientations, pairs))
    }

    /// One orientation of `set` with every tap outside `span` zeroed: the
    /// part of the responses one level of a ladder convolves.
    fn span_of(set: &BrirSet, orientation: usize, span: std::ops::Range<usize>) -> Arc<BrirSet> {
        let pairs = (0..set.emitters().len())
            .map(|e| {
                let pair = set.pair(e, orientation);
                let keep = |ir: &[f32]| -> Vec<f32> {
                    ir.iter()
                        .enumerate()
                        .map(|(i, &v)| if span.contains(&i) { v } else { 0.0 })
                        .collect()
                };
                BrirPair {
                    left: keep(&pair.left),
                    right: keep(&pair.right),
                }
            })
            .collect();
        Arc::new(BrirSet::from_pairs(
            set.emitters().to_vec(),
            vec![(0.0, 0.0)],
            pairs,
        ))
    }

    /// A ready stage partitioning `set` on `ladder`.
    fn stage_on(
        ladder: &[usize],
        set: &Arc<BrirSet>,
        positions: &[[f64; 3]],
        direct: &[bool],
    ) -> BrirStage {
        let mut stage = BrirStage::with_ladder(48000, Arc::new(|_| {}), ladder);
        stage.install_set(Arc::clone(set), positions.len());
        stage.configure_buses(positions, direct, 1);
        assert!(stage.is_ready());
        stage
    }

    /// Render `bus` (interleaved, `total` wide) in frames of `sample_length`
    /// and return (left, right).
    fn render(
        stage: &mut BrirStage,
        bus: &[f32],
        total: usize,
        sample_length: usize,
        head: HeadPose,
    ) -> (Vec<f32>, Vec<f32>) {
        let mut l = Vec::new();
        let mut r = Vec::new();
        for chunk in bus.chunks(total * sample_length) {
            let n = chunk.len() / total;
            let mut out = vec![0.0f32; 2 * n];
            stage.render_frame(chunk, total, n, head, &mut out);
            for f in out.chunks_exact(2) {
                l.push(f[0]);
                r.push(f[1]);
            }
        }
        (l, r)
    }

    /// Largest sample difference between two renders, over both ears, in
    /// decibels relative to the reference's RMS level.
    fn residual_db(got: &(Vec<f32>, Vec<f32>), want: &(Vec<f32>, Vec<f32>)) -> f64 {
        assert_eq!(got.0.len(), want.0.len());
        let pairs = || {
            let left = got.0.iter().zip(&want.0);
            left.chain(got.1.iter().zip(&want.1))
        };
        let worst = pairs().fold(0.0f64, |m, (&g, &w)| m.max((g as f64 - w as f64).abs()));
        let energy: f64 = pairs().map(|(_, &w)| (w as f64) * (w as f64)).sum();
        let rms = (energy / (2 * want.0.len()) as f64).sqrt();
        assert!(rms > 1e-3, "the reference is not silent: {rms}");
        20.0 * (worst / rms).max(1e-12).log10()
    }

    /// What the ladder's output may differ from the uniform one by: rounding.
    const RESIDUAL_DB: f64 = -110.0;

    /// Virtual speakers: left (−30°), right (+30°), and a direct LFE.
    fn buses() -> (Vec<[f64; 3]>, Vec<bool>) {
        let a = 30f64.to_radians();
        (
            vec![
                [-a.sin(), a.cos(), 0.0],
                [a.sin(), a.cos(), 0.0],
                [0.0, 1.0, 0.0],
            ],
            vec![false, false, true],
        )
    }

    fn ready_stage(set: &Arc<BrirSet>) -> BrirStage {
        let mut stage = BrirStage::new(48000);
        stage.install_set(Arc::clone(set), 3);
        let (pos, direct) = buses();
        stage.configure_buses(&pos, &direct, 1);
        stage
    }

    /// Render `frames` of `sample_length` from `bus` (interleaved, 3 wide)
    /// and return (left, right).
    fn run(
        stage: &mut BrirStage,
        bus: &[f32],
        sample_length: usize,
        head: HeadPose,
    ) -> (Vec<f32>, Vec<f32>) {
        render(stage, bus, 3, sample_length, head)
    }

    fn impulse_on(bus: usize, len: usize) -> Vec<f32> {
        let mut v = vec![0.0f32; 3 * len];
        v[bus] = 1.0;
        v
    }

    #[test]
    fn impulse_on_a_bus_returns_its_pair_delayed_by_the_block() {
        let set = synth_set(&[30.0, -30.0], &[0.0], 700);
        let mut stage = ready_stage(&set);
        assert!(stage.is_ready());
        assert_eq!(stage.bus_emitters(), &[Some(0), Some(1), None]);
        let len = 10 * BRIR_BLOCK;
        let (l, r) = run(&mut stage, &impulse_on(1, len), 40, HeadPose::identity());
        let pair = set.pair(1, 0);
        let lat = stage.latency_samples();
        let taps = pair.taps().min(len - lat);
        assert!(taps >= 600, "the pair spans several partitions: {taps}");
        for k in 0..taps {
            assert!(
                (l[k + lat] - pair.left[k]).abs() < 1e-4,
                "left tap {k}: {} vs {}",
                l[k + lat],
                pair.left[k]
            );
            assert!((r[k + lat] - pair.right[k]).abs() < 1e-4, "right tap {k}");
        }
        assert!(
            l[..lat].iter().all(|&v| v == 0.0),
            "nothing before the latency"
        );
    }

    #[test]
    fn direct_bus_feeds_both_ears_at_constant_power() {
        let set = synth_set(&[30.0, -30.0], &[0.0], 300);
        let mut stage = ready_stage(&set);
        let len = 3 * BRIR_BLOCK;
        let (l, r) = run(&mut stage, &impulse_on(2, len), 64, HeadPose::identity());
        let lat = stage.latency_samples();
        assert!((l[lat] - DIRECT_EAR_GAIN).abs() < 1e-6);
        assert!((r[lat] - DIRECT_EAR_GAIN).abs() < 1e-6);
        let rest: f32 = l.iter().chain(&r).map(|v| v.abs()).sum::<f32>() - 2.0 * DIRECT_EAR_GAIN;
        assert!(
            rest.abs() < 1e-6,
            "a direct bus is not convolved: residual {rest}"
        );
    }

    /// The pair of a bus comes back whole and in place across every level of
    /// the ladder, `BRIR_BLOCK − 1` samples late and not one more.
    #[test]
    fn a_long_pair_comes_back_whole_at_the_head_latency() {
        let (positions, direct) = ring_buses(3, &[2]);
        let taps = 30_000;
        let set = noise_set(&positions, &[taps; 3], &[0.0], 7);
        let mut stage = stage_on(&BRIR_LADDER, &set, &positions, &direct);
        assert_eq!(stage.plan.levels_for(taps), BRIR_LADDER.len());
        assert_eq!(stage.latency_samples(), BRIR_BLOCK - 1);
        let len = taps + 3 * BRIR_BLOCK;
        let (l, r) = render(&mut stage, &impulse_on(1, len), 3, 40, HeadPose::identity());
        let pair = set.pair(1, 0);
        let lat = stage.latency_samples();
        let peak = pair.left.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(l[..lat].iter().chain(&r[..lat]).all(|&v| v == 0.0));
        for k in 0..taps {
            assert!(
                (l[k + lat] - pair.left[k]).abs() < 1e-4 * peak,
                "left tap {k}: {} vs {}",
                l[k + lat],
                pair.left[k]
            );
            assert!(
                (r[k + lat] - pair.right[k]).abs() < 1e-4 * peak,
                "right tap {k}"
            );
        }
        assert!(l[taps + lat..].iter().all(|v| v.abs() < 1e-4 * peak));
    }

    /// The ladder gives the uniform partition's output for responses that
    /// end inside the head, exactly on a level's offset, one tap past it,
    /// at the lengths a level comes into use, and deep in the last level.
    #[test]
    fn the_ladder_output_is_the_uniform_one_whatever_the_response_length() {
        let (positions, direct) = ring_buses(3, &[1]);
        let plan = NonUniformPlan::new(&BRIR_LADDER);
        let mut lengths = vec![90, 30_000];
        for level in 1..BRIR_LADDER.len() {
            let opens = (0..).find(|&t| plan.levels_for(t) > level).unwrap();
            lengths.extend([plan.offset(level), plan.offset(level) + 1, opens - 1, opens]);
        }
        let mut layouts = std::collections::BTreeSet::new();
        for taps in lengths {
            layouts.insert(plan.levels_for(taps));
            let set = noise_set(&positions, &[taps; 3], &[0.0], taps as u32);
            let len = (taps + 3 * BRIR_LADDER[plan.levels_for(taps) - 1]).max(4096);
            let signal = noise(3 * len, 11);
            let mut uniform = stage_on(&UNIFORM, &set, &positions, &direct);
            let mut ladder = stage_on(&BRIR_LADDER, &set, &positions, &direct);
            let want = render(&mut uniform, &signal, 3, 128, HeadPose::identity());
            let got = render(&mut ladder, &signal, 3, 128, HeadPose::identity());
            let residual = residual_db(&got, &want);
            assert!(
                residual < RESIDUAL_DB,
                "{taps} taps: residual {residual:.1} dB"
            );
        }
        assert_eq!(layouts.len(), BRIR_LADDER.len(), "every layout was run");
    }

    /// Responses of different lengths in one set — one ending in each level,
    /// on its offset or past it — with a direct bus between them.
    #[test]
    fn responses_of_mixed_lengths_match_the_uniform_output() {
        let (positions, direct) = ring_buses(6, &[3]);
        let plan = NonUniformPlan::new(&BRIR_LADDER);
        let taps = [
            90,
            plan.offset(1),
            plan.offset(2) + 1,
            1,
            plan.offset(3),
            30_000,
        ];
        let set = noise_set(&positions, &taps, &[0.0], 5);
        let signal = noise(6 * 50_000, 13);
        let mut uniform = stage_on(&UNIFORM, &set, &positions, &direct);
        let mut ladder = stage_on(&BRIR_LADDER, &set, &positions, &direct);
        let want = render(&mut uniform, &signal, 6, 128, HeadPose::identity());
        let got = render(&mut ladder, &signal, 6, 128, HeadPose::identity());
        let residual = residual_db(&got, &want);
        assert!(residual < RESIDUAL_DB, "residual {residual:.1} dB");
    }

    /// Every bus count from one to eight, every third bus direct, the
    /// responses ending in each level of the ladder.
    #[test]
    fn every_bus_count_matches_the_uniform_output() {
        let plan = NonUniformPlan::new(&SMALL_LADDER);
        let lengths = [
            4000,
            50,
            plan.offset(1),
            plan.offset(1) + 1,
            plan.offset(2),
            plan.offset(2) + 1,
        ];
        assert_eq!(plan.levels_for(4000), SMALL_LADDER.len());
        for total in 1..=8 {
            let direct: Vec<usize> = (0..total).filter(|b| b % 3 == 2).collect();
            let (positions, direct) = ring_buses(total, &direct);
            let taps: Vec<usize> = (0..total).map(|b| lengths[b % lengths.len()]).collect();
            let set = noise_set(&positions, &taps, &[0.0], total as u32);
            let signal = noise(total * 9000, 17);
            let mut uniform = stage_on(&UNIFORM, &set, &positions, &direct);
            let mut ladder = stage_on(&SMALL_LADDER, &set, &positions, &direct);
            let want = render(&mut uniform, &signal, total, 128, HeadPose::identity());
            let got = render(&mut ladder, &signal, total, 128, HeadPose::identity());
            let residual = residual_db(&got, &want);
            assert!(
                residual < RESIDUAL_DB,
                "{total} buses: residual {residual:.1} dB"
            );
        }
    }

    /// The tail segments' work is laid out on the stage's own blocks, never
    /// on the host's frames: a set that uses every level of the ladder
    /// renders the same bits whatever the frame size.
    #[test]
    fn host_chunking_does_not_change_the_output() {
        let (positions, direct) = ring_buses(3, &[2]);
        let taps = 30_000;
        let set = noise_set(&positions, &[taps; 3], &[0.0], 3);
        let signal = noise(3 * (taps + 2 * 8192), 0x1234_5678);
        let mut reference = stage_on(&BRIR_LADDER, &set, &positions, &direct);
        assert_eq!(reference.plan.levels_for(taps), BRIR_LADDER.len());
        let want = render(&mut reference, &signal, 3, 40, HeadPose::identity());
        assert!(want.0.iter().any(|&v| v != 0.0));
        for frame in [1, 127, 128, 1000, 20_000] {
            let mut stage = stage_on(&BRIR_LADDER, &set, &positions, &direct);
            let got = render(&mut stage, &signal, 3, frame, HeadPose::identity());
            assert!(got == want, "frames of {frame} samples changed the output");
        }
    }

    /// Render frames of [`BRIR_BLOCK`] samples, the head at `yaw(frame)`
    /// degrees: with synchronous builds a turn lands on the block of the
    /// frame that makes it.
    fn render_turning(
        stage: &mut BrirStage,
        bus: &[f32],
        total: usize,
        yaw: impl Fn(usize) -> f32,
    ) -> (Vec<f32>, Vec<f32>) {
        stage.set_synchronous_builds(true);
        let mut l = Vec::new();
        let mut r = Vec::new();
        for (frame, chunk) in bus.chunks(total * BRIR_BLOCK).enumerate() {
            let head = HeadPose::from_euler_deg(yaw(frame), 0.0, 0.0);
            let (fl, fr) = render(stage, chunk, total, BRIR_BLOCK, head);
            l.extend(fl);
            r.extend(fr);
        }
        (l, r)
    }

    /// What a head turn means level by level: the head ramps to the new
    /// orientation over the block the bank lands on, each tail segment over
    /// the first [`BRIR_BLOCK`] samples of the first of its output blocks
    /// whose period starts on that block or later. Rebuilt here from the
    /// uniform reference run on each level's taps alone, for each
    /// orientation, and held to the same residual as a head at rest.
    #[test]
    fn a_head_turn_ramps_each_level_at_its_next_period() {
        let (positions, direct) = ring_buses(2, &[]);
        let plan = NonUniformPlan::new(&SMALL_LADDER);
        let taps = 4000;
        let levels = plan.levels_for(taps);
        assert_eq!(levels, SMALL_LADDER.len());
        let set = noise_set(&positions, &[taps; 2], &[0.0, 20.0], 23);
        let blocks = 96;
        let signal = noise(2 * blocks * BRIR_BLOCK, 29);
        // Per level and orientation: that level's share of the output.
        let shares: Vec<[(Vec<f32>, Vec<f32>); 2]> = (0..levels)
            .map(|level| {
                let end = if level + 1 < levels {
                    plan.offset(level + 1)
                } else {
                    taps
                };
                [0, 1].map(|orientation| {
                    let only = span_of(&set, orientation, plan.offset(level)..end);
                    let mut uniform = stage_on(&UNIFORM, &only, &positions, &direct);
                    render(&mut uniform, &signal, 2, BRIR_BLOCK, HeadPose::identity())
                })
            })
            .collect();
        for turn in [0, 1, 6, 7, 8, 13] {
            let mut stage = stage_on(&SMALL_LADDER, &set, &positions, &direct);
            let got = render_turning(&mut stage, &signal, 2, |frame| {
                if frame >= turn { 18.0 } else { 0.0 }
            });
            assert_eq!(stage.bank_orientation(), Some(1));
            let mut want = (vec![0.0f32; got.0.len()], vec![0.0f32; got.0.len()]);
            for (level, [from, to]) in shares.iter().enumerate() {
                // First block (counted from 0) whose output ramps to the new
                // bank; block `c` is read from output sample `128·c + 127`.
                let period = SMALL_LADDER[level] / BRIR_BLOCK;
                let first = if level == 0 {
                    turn
                } else {
                    (turn + 1).div_ceil(period) * period - 1 + period
                };
                let start = BRIR_BLOCK * first + BRIR_BLOCK - 1;
                let ramp = |n: usize| {
                    if n < start {
                        0.0
                    } else {
                        ((n - start + 1) as f32 / BRIR_BLOCK as f32).min(1.0)
                    }
                };
                for n in 0..want.0.len() {
                    want.0[n] += from.0[n] + (to.0[n] - from.0[n]) * ramp(n);
                    want.1[n] += from.1[n] + (to.1[n] - from.1[n]) * ramp(n);
                }
            }
            let residual = residual_db(&got, &want);
            assert!(
                residual < RESIDUAL_DB,
                "turn on block {turn}: residual {residual:.1} dB"
            );
        }
    }

    /// Against the uniform reference making the same turns: the same output
    /// until the first bank lands, and again once the last tail segment has
    /// taken the last bank — through two turns on consecutive blocks (the
    /// tail segments never play the bank in between) and one more while the
    /// largest segment is still on its way.
    #[test]
    fn head_turns_settle_on_the_uniform_output() {
        let (positions, direct) = ring_buses(3, &[1]);
        let taps = 30_000;
        let set = noise_set(&positions, &[taps; 3], &[0.0, 20.0, 40.0], 31);
        let yaw = |frame: usize| match frame {
            0..5 => 0.0,
            5 => 18.0,
            6..70 => 38.0,
            _ => 0.0,
        };
        let last_turn: usize = 70;
        let period = BRIR_LADDER[BRIR_LADDER.len() - 1] / BRIR_BLOCK;
        // The first block after the largest segment's ramp.
        let settled = (last_turn + 1).div_ceil(period) * period + period;
        let blocks = settled + taps / BRIR_BLOCK + 8;
        let signal = noise(3 * blocks * BRIR_BLOCK, 37);
        let mut uniform = stage_on(&UNIFORM, &set, &positions, &direct);
        let mut ladder = stage_on(&BRIR_LADDER, &set, &positions, &direct);
        let want = render_turning(&mut uniform, &signal, 3, yaw);
        let got = render_turning(&mut ladder, &signal, 3, yaw);
        assert_eq!(uniform.bank_orientation(), Some(0));
        assert_eq!(ladder.bank_orientation(), Some(0));
        assert!(got.0.iter().chain(&got.1).all(|v| v.is_finite()));
        let part = |x: &(Vec<f32>, Vec<f32>), range: std::ops::Range<usize>| {
            (x.0[range.clone()].to_vec(), x.1[range].to_vec())
        };
        // Block `c` is read from output sample `128·c + 127`.
        let before = 0..BRIR_BLOCK * 5 + BRIR_BLOCK - 1;
        let after = BRIR_BLOCK * settled + BRIR_BLOCK - 1..want.0.len();
        for range in [before, after.clone()] {
            let residual = residual_db(&part(&got, range.clone()), &part(&want, range.clone()));
            assert!(
                residual < RESIDUAL_DB,
                "samples {range:?}: residual {residual:.1} dB"
            );
        }
        // One block earlier the largest segment is still ramping.
        let early = after.start - BRIR_BLOCK..after.end;
        assert!(residual_db(&part(&got, early.clone()), &part(&want, early)) > RESIDUAL_DB);
    }

    /// A new set, then a relayout, in the middle of a stream: both restart
    /// the ladder's streams as they restart the uniform ones.
    #[test]
    fn a_set_swap_and_a_relayout_match_the_uniform_output() {
        let (positions, direct) = ring_buses(3, &[2]);
        let (wide_positions, wide_direct) = ring_buses(4, &[0]);
        let first = noise_set(&positions, &[30_000; 3], &[0.0], 41);
        let second = noise_set(&wide_positions, &[9000, 500, 12_000, 20], &[0.0], 43);
        let run = |ladder: &[usize]| {
            let mut stage = stage_on(ladder, &first, &positions, &direct);
            let head = HeadPose::identity();
            let mut out = render(&mut stage, &noise(3 * 40_000, 47), 3, 100, head);
            let generation = stage.set_generation();
            stage.install_set(Arc::clone(&second), 3);
            stage.configure_buses(&positions, &direct, 1);
            assert_eq!(stage.set_generation(), generation + 1);
            let next = render(&mut stage, &noise(3 * 20_000, 53), 3, 100, head);
            out.0.extend(next.0);
            out.1.extend(next.1);
            stage.configure_buses(&wide_positions, &wide_direct, 2);
            assert_eq!(stage.bus_emitters().len(), 4);
            let next = render(&mut stage, &noise(4 * 20_000, 59), 4, 100, head);
            out.0.extend(next.0);
            out.1.extend(next.1);
            out
        };
        let want = run(&UNIFORM);
        let got = run(&BRIR_LADDER);
        let residual = residual_db(&got, &want);
        assert!(residual < RESIDUAL_DB, "residual {residual:.1} dB");
    }

    #[test]
    fn head_turn_switches_to_the_nearest_orientation() {
        // Views at SOFA −20, 0, +20 → renderer yaws +20, 0, −20 → sorted
        // indices 0 (−20), 1 (0), 2 (+20).
        let set = synth_set(&[30.0, -30.0], &[-20.0, 0.0, 20.0], 400);
        let mut stage = ready_stage(&set);
        assert_eq!(stage.bank_orientation(), Some(1));
        // Turn right by 18°: nearest is +20 (index 2). The bank comes from
        // the worker; keep rendering silence until it lands.
        let head = HeadPose::from_euler_deg(18.0, 0.0, 0.0);
        let silence = vec![0.0f32; 3 * BRIR_BLOCK];
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while stage.bank_orientation() != Some(2) {
            assert!(
                std::time::Instant::now() < deadline,
                "bank for orientation 2 never landed"
            );
            let _ = run(&mut stage, &silence, BRIR_BLOCK, head);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // Once the swap has played out, an impulse yields the new pair.
        let _ = run(&mut stage, &silence, BRIR_BLOCK, head);
        let len = 8 * BRIR_BLOCK;
        let (l, _) = run(&mut stage, &impulse_on(0, len), 40, head);
        let pair = set.pair(0, 2);
        let lat = stage.latency_samples();
        let taps = pair.taps().min(len - lat);
        assert!(taps >= 300, "{taps}");
        for k in 0..taps {
            assert!((l[k + lat] - pair.left[k]).abs() < 1e-4, "tap {k}");
        }
        assert!(l.iter().all(|v| v.is_finite()));
    }

    /// With synchronous builds (offline renders) the bank for a new head
    /// orientation is built in the block that asks for it and starts
    /// blending in there, instead of whenever the worker delivers it.
    #[test]
    fn synchronous_builds_turn_the_head_on_the_requesting_block() {
        let set = synth_set(&[30.0, -30.0], &[-20.0, 0.0, 20.0], 400);
        let mut stage = ready_stage(&set);
        stage.set_synchronous_builds(true);
        assert_eq!(stage.bank_orientation(), Some(1));
        let head = HeadPose::from_euler_deg(18.0, 0.0, 0.0);
        let silence = vec![0.0f32; 3 * BRIR_BLOCK];
        let _ = run(&mut stage, &silence, BRIR_BLOCK, head);
        assert_eq!(stage.bank_orientation(), Some(2));
    }

    /// A synchronous load has reported its outcome by the time
    /// `ensure_loaded` returns; a failed one leaves the stage unready (the
    /// cascade then runs on the HRTF stage, as it does live).
    #[test]
    fn a_synchronous_load_reports_before_returning() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::<BrirStatus>::new()));
        let sink_seen = Arc::clone(&seen);
        let mut stage = BrirStage::with_status_sink(
            48000,
            Arc::new(move |s| sink_seen.lock().unwrap().push(s)),
        );
        stage.set_synchronous_builds(true);
        stage.ensure_loaded("/nonexistent.sofa", &BrirLoadOptions::default(), 3);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert_eq!(seen[0].path, "/nonexistent.sofa");
        assert!(seen[0].loaded.is_none());
        assert!(seen[0].error.is_some());
        assert!(!stage.is_ready());
    }

    #[test]
    fn unready_stage_is_silent_and_reports_its_latency() {
        let mut stage = BrirStage::new(48000);
        assert!(!stage.is_ready());
        assert_eq!(stage.latency_samples(), BRIR_BLOCK - 1);
        let mut out = vec![0.0f32; 80];
        stage.render_frame(&[1.0; 120], 3, 40, HeadPose::identity(), &mut out);
        assert!(out.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn a_new_set_is_adopted_and_the_old_one_retired() {
        let a = synth_set(&[30.0, -30.0], &[0.0], 300);
        let b = synth_set(&[0.0], &[0.0], 300);
        let mut stage = ready_stage(&a);
        let g = stage.set_generation();
        stage.install_set(Arc::clone(&b), 3);
        let (pos, direct) = buses();
        stage.configure_buses(&pos, &direct, 1);
        assert_eq!(stage.set_generation(), g + 1);
        // One emitter now: both spatialized buses render from it.
        assert_eq!(stage.bus_emitters(), &[Some(0), Some(0), None]);
        assert_eq!(Arc::strong_count(&b), 2, "the stage holds the new set");
    }

    /// Cost per block for a realistic bus count — 12 virtual speakers, 11
    /// convolved and the LFE — with responses of a quarter of a second to two
    /// seconds, on the uniform partition and on the ladder, the head at rest
    /// and turning (a bank swap every seven blocks, some fifty a second,
    /// which keeps every tail segment ramping). Prints per case the
    /// partitions per level, the resident size of one orientation's bank
    /// and of the streams, and the mean and worst block time. The cases are
    /// run in interleaved rounds and the best round of each is printed: on a
    /// shared machine a block reads long, never short. Manual, on a quiet
    /// core:
    /// `cargo test --release -p renderer --lib -- --ignored --nocapture brir_stage::tests::block_burst_timing`.
    #[test]
    #[ignore = "timing printout for a release build; not a gate"]
    fn block_burst_timing() {
        const BUSES: usize = 12;
        const ROUNDS: usize = 5;
        const WARM_UP: usize = 256;
        const BLOCKS: usize = 2048;
        const SWAP_EVERY: usize = 7;
        let az: Vec<f32> = (0..BUSES).map(|i| -180.0 + 30.0 * i as f32).collect();
        let positions: Vec<[f64; 3]> = az
            .iter()
            .map(|a| {
                let r = (-a).to_radians() as f64;
                [r.sin(), r.cos(), 0.0]
            })
            .collect();
        let mut direct = vec![false; BUSES];
        direct[BUSES - 1] = true;
        let bus = noise(16 * BUSES * BRIR_BLOCK, 1);

        struct Case {
            label: String,
            stage: BrirStage,
            /// Two banks of the same orientation to swap between.
            banks: [Arc<KernelBank>; 2],
            /// `[at rest, turning]` → per round (mean, worst), microseconds.
            rounds: [Vec<(f64, f64)>; 2],
        }
        let mib = |bytes: usize| bytes as f64 / (1024.0 * 1024.0);
        let mut cases = Vec::new();
        for taps in [12_000usize, 24_000, 48_000, 96_000] {
            let set = synth_set_decay(&az, &[0.0], taps, taps as f32 / 4.0);
            for (name, ladder) in [("uniform", &UNIFORM[..]), ("ladder", &BRIR_LADDER[..])] {
                let stage = stage_on(ladder, &set, &positions, &direct);
                let banks = [0, 1].map(|_| Arc::new(build_bank(&stage.plan, &set, 0)));
                let levels = stage.plan.levels_for(set.max_taps());
                let partitions: Vec<usize> = (0..levels)
                    .map(|l| stage.plan.partitions_for(l, levels, set.max_taps()))
                    .collect();
                let bank_bytes: usize = banks[0]
                    .kernels
                    .iter()
                    .flatten()
                    .map(NonUniformKernel::bytes)
                    .sum();
                let stream_bytes = stage.streams.tails.bytes()
                    + stage
                        .streams
                        .inputs
                        .iter()
                        .map(crate::partitioned_conv::nonuniform::history_bytes)
                        .sum::<usize>();
                cases.push(Case {
                    label: format!(
                        "{:.2} s {name:7} {:16} bank {:4.1} MiB, streams {:4.1} MiB",
                        set.max_taps() as f64 / 48_000.0,
                        format!("{partitions:?}"),
                        mib(bank_bytes),
                        mib(stream_bytes)
                    ),
                    stage,
                    banks,
                    rounds: [Vec::new(), Vec::new()],
                });
            }
        }

        let mut out = vec![0.0f32; 2 * BRIR_BLOCK];
        for _ in 0..ROUNDS {
            for case in &mut cases {
                for (turning, rounds) in case.rounds.iter_mut().enumerate() {
                    let (mut total, mut worst) = (0.0f64, 0.0f64);
                    // The blocks before `WARM_UP` bring the case back into
                    // the cache the others pushed it out of.
                    for block in 0..WARM_UP + BLOCKS {
                        if turning == 1 && block % SWAP_EVERY == 0 {
                            let bank = &case.banks[block / SWAP_EVERY % 2];
                            case.stage.incoming_bank.store(Some(Arc::clone(bank)));
                        }
                        let frame = &bus[block % 16 * BUSES * BRIR_BLOCK..][..BUSES * BRIR_BLOCK];
                        let start = std::time::Instant::now();
                        case.stage.render_frame(
                            frame,
                            BUSES,
                            BRIR_BLOCK,
                            HeadPose::identity(),
                            &mut out,
                        );
                        let us = start.elapsed().as_secs_f64() * 1e6;
                        if block >= WARM_UP {
                            total += us;
                            worst = worst.max(us);
                        }
                    }
                    rounds.push((total / BLOCKS as f64, worst));
                }
            }
        }
        println!(
            "BRIR block times, µs per {BRIR_BLOCK}-sample block ({:.0} µs of audio), best of {ROUNDS} rounds",
            BRIR_BLOCK as f64 * 1e6 / 48_000.0
        );
        for case in &cases {
            let best = |rounds: &[(f64, f64)]| {
                rounds.iter().fold((f64::INFINITY, f64::INFINITY), |b, r| {
                    (b.0.min(r.0), b.1.min(r.1))
                })
            };
            let (rest, turning) = (best(&case.rounds[0]), best(&case.rounds[1]));
            println!(
                "{} | at rest: mean {:6.1} worst {:6.1} | turning: mean {:6.1} worst {:6.1}",
                case.label, rest.0, rest.1, turning.0, turning.1
            );
        }
    }

    #[cfg(not(feature = "sofa"))]
    #[test]
    fn a_load_without_sofa_support_reports_the_error() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::<BrirStatus>::new()));
        let sink_seen = Arc::clone(&seen);
        let mut stage = BrirStage::with_status_sink(
            48000,
            Arc::new(move |s| sink_seen.lock().unwrap().push(s)),
        );
        stage.ensure_loaded("/nonexistent.sofa", &BrirLoadOptions::default(), 3);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while seen.lock().unwrap().is_empty() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let s = seen.lock().unwrap()[0].clone();
        assert_eq!(s.path, "/nonexistent.sofa");
        assert!(s.loaded.is_none());
        assert!(s.error.as_deref().unwrap_or("").contains("sofa"), "{s:?}");
        assert!(!stage.is_ready());
    }
}
